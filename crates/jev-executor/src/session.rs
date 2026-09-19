//! Execution session: registered data sources plus plan execution.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::compute::concat_batches;
use datafusion::arrow::util::pretty::pretty_format_batches;
use datafusion::physical_plan::{ExecutionPlan, collect};
use datafusion::prelude::{CsvReadOptions, ParquetReadOptions, SessionContext};
use jevir::logical::Op;
use jev_provider::{BoxFuture, SemanticBackend};
use jevir::physical::{PhysicalPlan, PhysicalQuery};
use jevir::{Catalog, LogicalPlan, Schema, SortKey};

use crate::error::ExecError;
use crate::metrics::ExecMetrics;
use crate::relational::{Lowered, Lowerer};
use crate::semantic::{SemanticCache, SemanticRuntime};
use crate::types::schema_to_jevir;

/// Owns the DataFusion context and the JevIR view of every registered table.
pub struct Session {
    ctx: SessionContext,
    tables: HashMap<String, Schema>,
    semantic: Option<Arc<dyn SemanticBackend>>,
    cache: SemanticCache,
    max_semantic_rows: usize,
}

/// Default cap on rows sent to the semantic backend by one operator.
pub const DEFAULT_MAX_SEMANTIC_ROWS: usize = 10_000;

/// Rows produced by a plan, with their JevIR schema.
#[derive(Debug)]
pub struct QueryResult {
    pub schema: Schema,
    pub batches: Vec<RecordBatch>,
    pub metrics: ExecMetrics,
}

impl QueryResult {
    pub fn num_rows(&self) -> usize {
        self.batches.iter().map(RecordBatch::num_rows).sum()
    }

    pub fn to_pretty_string(&self) -> Result<String, ExecError> {
        Ok(pretty_format_batches(&self.batches).map_err(datafusion::error::DataFusionError::from)?.to_string())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            ctx: SessionContext::new(),
            tables: HashMap::new(),
            semantic: None,
            cache: SemanticCache::default(),
            max_semantic_rows: DEFAULT_MAX_SEMANTIC_ROWS,
        }
    }

    pub fn with_semantic_backend(mut self, backend: Arc<dyn SemanticBackend>) -> Self {
        self.semantic = Some(backend);
        self
    }

    pub fn semantic_backend(&self) -> Option<&dyn SemanticBackend> {
        self.semantic.as_deref()
    }

    pub(crate) fn context(&self) -> &SessionContext {
        &self.ctx
    }

    /// Caps the rows any single semantic operator may send to the backend.
    pub fn set_max_semantic_rows(&mut self, max: usize) {
        self.max_semantic_rows = max;
    }

    /// Registers a `.csv` or `.parquet` file under its file stem
    /// (lowercased, non-alphanumerics replaced by `_`). Returns the table name.
    pub async fn register_file(&mut self, path: impl AsRef<Path>) -> Result<String, ExecError> {
        let path = path.as_ref();
        let fail = |reason: &str| ExecError::Register { path: path.display().to_string(), reason: reason.into() };
        let stem = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| fail("no file name"))?;
        let name: String =
            stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("csv") => self.register_csv(&name, path).await?,
            Some("parquet") => self.register_parquet(&name, path).await?,
            _ => return Err(fail("expected a .csv or .parquet file")),
        }
        Ok(name)
    }

    pub async fn register_csv(&mut self, name: &str, path: impl AsRef<Path>) -> Result<(), ExecError> {
        self.check_new(name)?;
        self.ctx.register_csv(name, path_str(path.as_ref())?, CsvReadOptions::new()).await?;
        self.record_schema(name).await
    }

    pub async fn register_parquet(&mut self, name: &str, path: impl AsRef<Path>) -> Result<(), ExecError> {
        self.check_new(name)?;
        self.ctx.register_parquet(name, path_str(path.as_ref())?, ParquetReadOptions::default()).await?;
        self.record_schema(name).await
    }

    fn check_new(&self, name: &str) -> Result<(), ExecError> {
        match self.tables.contains_key(name) {
            true => Err(ExecError::Register { path: name.into(), reason: format!("table `{name}` is already registered") }),
            false => Ok(()),
        }
    }

    async fn record_schema(&mut self, name: &str) -> Result<(), ExecError> {
        let df = self.ctx.table(name).await?;
        let schema = schema_to_jevir(df.schema().as_arrow())?;
        self.tables.insert(name.to_string(), schema);
        Ok(())
    }

    /// Executes a physical plan.
    pub async fn execute(&self, query: &PhysicalQuery) -> Result<QueryResult, ExecError> {
        let start = Instant::now();
        let mut metrics = ExecMetrics::default();
        let out = self.run(&query.root, &mut metrics).await?;

        let produced = schema_to_jevir(out.batch.schema().as_ref())?;
        if produced != query.schema {
            return Err(ExecError::Internal(format!(
                "execution produced schema {produced}, but JevIR inferred {}",
                query.schema
            )));
        }
        metrics.total_time = start.elapsed();
        if let Some(backend) = &self.semantic {
            metrics.estimated_cost_usd =
                metrics.input_tokens as f64 * backend.info().usd_per_million_input_tokens / 1e6;
        }
        Ok(QueryResult { schema: query.schema.clone(), batches: vec![out.batch], metrics })
    }

    fn run<'a>(
        &'a self,
        plan: &'a PhysicalPlan,
        metrics: &'a mut ExecMetrics,
    ) -> BoxFuture<'a, Result<Materialized, ExecError>> {
        Box::pin(async move {
            match plan {
                PhysicalPlan::JevBatch(exec) => {
                    let input = self.run(&exec.input, metrics).await?;
                    let backend = self.semantic.as_deref().ok_or(ExecError::NoSemanticBackend)?;
                    let runtime = SemanticRuntime { backend, cache: &self.cache, max_rows: self.max_semantic_rows };
                    metrics.semantic_batches += 1;
                    let batch = runtime.evaluate(exec, input.batch, metrics).await?;
                    // semantic operators preserve row order
                    Ok(Materialized { batch, ordering: input.ordering })
                }
                PhysicalPlan::DataFusion(exec) => {
                    let mut materialized = HashMap::new();
                    let mut materialized_rows = 0;
                    for input in &exec.inputs {
                        let done = self.run(&input.exec, metrics).await?;
                        materialized_rows += done.batch.num_rows();
                        let df = self.ctx.read_batch(done.batch)?;
                        materialized.insert(Arc::as_ptr(&input.node) as usize, Lowered { df, ordering: done.ordering });
                    }
                    let mut tables = HashMap::new();
                    for name in scanned_tables(&exec.plan) {
                        let df = self.ctx.table(name.as_str()).await?;
                        tables.insert(name, df);
                    }
                    let lowered = Lowerer { ctx: &self.ctx, tables: &tables, materialized: &materialized }
                        .lower_root(&exec.plan)?;
                    let schema = Arc::new(lowered.df.schema().as_arrow().clone());
                    let physical = lowered.df.create_physical_plan().await?;
                    let batches = collect(physical.clone(), self.ctx.task_ctx()).await?;
                    // leaf operators read base tables plus the materialized semantic results
                    metrics.rows_scanned += leaf_rows(&physical).saturating_sub(materialized_rows);
                    let batch = match batches.first() {
                        Some(first) => concat_batches(&first.schema(), &batches)?,
                        None => RecordBatch::new_empty(schema),
                    };
                    Ok(Materialized { batch, ordering: lowered.ordering })
                }
            }
        })
    }
}

/// A fully computed intermediate result.
struct Materialized {
    batch: RecordBatch,
    ordering: Option<Vec<SortKey>>,
}

impl Catalog for Session {
    fn table_schema(&self, table: &str) -> Option<Schema> {
        self.tables.get(table).cloned()
    }

    fn table_names(&self) -> Vec<String> {
        self.tables.table_names()
    }
}

/// Rows produced by the leaf (scan) operators of an executed DataFusion plan.
fn leaf_rows(plan: &Arc<dyn ExecutionPlan>) -> usize {
    match plan.children().as_slice() {
        [] => plan.metrics().and_then(|m| m.output_rows()).unwrap_or(0),
        children => children.iter().map(|c| leaf_rows(c)).sum(),
    }
}

fn path_str(path: &Path) -> Result<&str, ExecError> {
    path.to_str().ok_or_else(|| ExecError::Register { path: path.display().to_string(), reason: "path is not UTF-8".into() })
}

fn scanned_tables(plan: &LogicalPlan) -> Vec<String> {
    let mut out = Vec::new();
    if let Op::Scan(scan) = &plan.op {
        out.push(scan.table.clone());
    }
    for input in plan.inputs() {
        out.extend(scanned_tables(input));
    }
    out.sort();
    out.dedup();
    out
}

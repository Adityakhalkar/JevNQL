//! Execution session: registered data sources plus plan execution.

use std::collections::HashMap;
use std::path::Path;

use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::util::pretty::pretty_format_batches;
use datafusion::prelude::{CsvReadOptions, ParquetReadOptions, SessionContext};
use jevir::logical::Op;
use jevir::{Catalog, LogicalPlan, Schema, ValidatedPlan};

use crate::error::ExecError;
use crate::relational::Lowerer;
use crate::types::schema_to_jevir;

/// Owns the DataFusion context and the JevIR view of every registered table.
pub struct Session {
    ctx: SessionContext,
    tables: HashMap<String, Schema>,
}

/// Rows produced by a plan, with their JevIR schema.
#[derive(Debug)]
pub struct QueryResult {
    pub schema: Schema,
    pub batches: Vec<RecordBatch>,
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
        Self { ctx: SessionContext::new(), tables: HashMap::new() }
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

    /// Executes a validated plan.
    pub async fn execute(&self, plan: &ValidatedPlan) -> Result<QueryResult, ExecError> {
        let mut tables = HashMap::new();
        for name in scanned_tables(&plan.plan) {
            let df = self.ctx.table(name.as_str()).await?;
            tables.insert(name, df);
        }
        let df = Lowerer { ctx: &self.ctx, tables: &tables }.lower_root(&plan.plan)?;

        let produced = schema_to_jevir(df.schema().as_arrow())?;
        if produced != plan.schema {
            return Err(ExecError::Internal(format!(
                "DataFusion produced schema {produced}, but JevIR inferred {}",
                plan.schema
            )));
        }
        Ok(QueryResult { schema: plan.schema.clone(), batches: df.collect().await? })
    }
}

impl Catalog for Session {
    fn table_schema(&self, table: &str) -> Option<Schema> {
        self.tables.get(table).cloned()
    }

    fn table_names(&self) -> Vec<String> {
        self.tables.table_names()
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

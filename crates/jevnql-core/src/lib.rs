//! JevNQL engine: the public API over the Rust core.
//!
//! Frontends hand the engine logical JevIR (JSON). The engine validates it
//! against the registered data, optimizes it, plans it physically and runs it
//! on DataFusion and a semantic backend.

use std::path::Path;
use std::sync::Arc;

use jev_executor::Session;
pub use jev_executor::{DEFAULT_MAX_SEMANTIC_ROWS, ExecError, ExecMetrics, Progress, ProgressHook, QueryResult, TableProfile};
use jev_optimizer::{PhysicalConfig, RuleApplication, optimize, physical_plan};
use jev_provider::simulated::SimulatedBackend;
use jev_provider::typesafe::TypeSafeJevBackend;
pub use jev_provider::{ProviderError, SemanticBackend};
use jevir::physical::{PhysicalPlan, PhysicalQuery};
use jevir::{Catalog, DataType, IrError, ValidatedPlan};

/// Which semantic backend to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// TypeSafe Jev if `TYPESAFE_API_KEY` is set, otherwise simulated.
    Auto,
    Jev,
    Simulated,
}

impl BackendChoice {
    pub fn build(self) -> Result<Arc<dyn SemanticBackend>, ProviderError> {
        match self {
            BackendChoice::Jev => Ok(Arc::new(TypeSafeJevBackend::from_env()?)),
            BackendChoice::Simulated => Ok(Arc::new(SimulatedBackend::default())),
            BackendChoice::Auto => {
                BackendChoice::Jev.build().or_else(|_| BackendChoice::Simulated.build())
            }
        }
    }
}

/// A plan taken through validation, optimization and physical planning.
pub struct Prepared {
    pub logical: ValidatedPlan,
    pub optimized: ValidatedPlan,
    pub rules: Vec<RuleApplication>,
    pub physical: PhysicalQuery,
}

impl Prepared {
    /// Engines in execution order, e.g. `["DataFusion", "Jev", "DataFusion"]`.
    pub fn engine_path(&self) -> Vec<&'static str> {
        fn visit(plan: &PhysicalPlan, out: &mut Vec<&'static str>) {
            let engine = match plan {
                PhysicalPlan::DataFusion(d) => {
                    d.inputs.iter().for_each(|i| visit(&i.exec, out));
                    "DataFusion"
                }
                PhysicalPlan::JevBatch(j) => {
                    visit(&j.input, out);
                    "Jev"
                }
            };
            if out.last() != Some(&engine) {
                out.push(engine);
            }
        }
        let mut out = Vec::new();
        visit(&self.physical.root, &mut out);
        out
    }
}

pub struct Engine {
    session: Session,
}

impl Engine {
    /// Opens an engine over `.csv` / `.parquet` files (one table per file).
    pub async fn open(files: &[impl AsRef<Path>], backend: Option<Arc<dyn SemanticBackend>>) -> Result<Self, ExecError> {
        let mut session = Session::new();
        if let Some(backend) = backend {
            session = session.with_semantic_backend(backend);
        }
        for file in files {
            session.register_file(file).await?;
        }
        Ok(Self { session })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn backend_name(&self) -> Option<&str> {
        self.session.semantic_backend().map(|b| b.info().name.as_str())
    }

    /// Profiles of every registered table, sorted by name.
    pub async fn profiles(&self) -> Result<Vec<TableProfile>, ExecError> {
        let mut out = Vec::new();
        for table in self.session.table_names() {
            out.push(self.session.profile(&table).await?);
        }
        Ok(out)
    }

    /// What the natural-language translator needs to know about the data:
    /// columns, low-cardinality values, and parent/child relations (a column
    /// unique in one table and repeated in another) with count percentiles.
    pub async fn vocabulary(&self) -> Result<jev_nl::Vocabulary, ExecError> {
        use jev_nl::{Column, ColumnKind, Relation, Table};
        let names = self.session.table_names();
        let mut tables = Vec::new();
        for name in &names {
            let schema = self.session.table_schema(name).expect("registered");
            let listed = self.session.listed_values(name).await?;
            let columns = schema
                .fields()
                .iter()
                .map(|f| Column {
                    name: f.name.clone(),
                    kind: match &f.data_type {
                        DataType::Utf8 => ColumnKind::Text,
                        t if t.is_numeric() => ColumnKind::Number,
                        t if t.is_temporal() => ColumnKind::Date,
                        _ => ColumnKind::Other,
                    },
                    values: listed.iter().find(|(c, _)| *c == f.name).map(|(_, v)| v.clone()).unwrap_or_default(),
                })
                .collect();
            tables.push(Table { name: name.clone(), columns });
        }
        let mut relations = Vec::new();
        for parent in &names {
            let parent_schema = self.session.table_schema(parent).expect("registered");
            for child in names.iter().filter(|c| *c != parent) {
                let child_schema = self.session.table_schema(child).expect("registered");
                for key in parent_schema.names().into_iter().filter(|k| k.ends_with("_id") && child_schema.contains(k)) {
                    if self.session.is_unique(parent, key).await? && !self.session.is_unique(child, key).await? {
                        relations.push(Relation {
                            parent: parent.clone(),
                            child: child.clone(),
                            key: key.to_string(),
                            count_percentiles: self.session.count_percentiles(child, key).await?,
                        });
                    }
                }
            }
        }
        Ok(jev_nl::Vocabulary { tables, relations })
    }

    /// Decodes and type-checks a logical JevIR plan document.
    pub fn validate(&self, plan_json: &str) -> Result<ValidatedPlan, IrError> {
        jevir::decode(plan_json, &self.session)
    }

    /// Validates, optionally optimizes, and physically plans a plan document.
    pub fn prepare(&self, plan_json: &str, optimize_plan: bool) -> Result<Prepared, IrError> {
        let logical = self.validate(plan_json)?;
        let (optimized, rules) = match optimize_plan {
            true => {
                let out = optimize(&logical, &self.session)?;
                (out.plan, out.trace.applied)
            }
            false => (logical.clone(), Vec::new()),
        };
        let config = PhysicalConfig {
            concurrency: self.session.semantic_backend().map_or(16, |b| b.info().max_concurrency),
            fuse: optimize_plan,
            ..Default::default()
        };
        let physical = physical_plan(&optimized, &config);
        Ok(Prepared { logical, optimized, rules, physical })
    }

    pub async fn execute(&self, prepared: &Prepared) -> Result<QueryResult, ExecError> {
        self.session.execute(&prepared.physical).await
    }
}

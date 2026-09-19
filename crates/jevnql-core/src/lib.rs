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

/// A plain-English question turned into NQL.
pub struct Interpretation {
    pub translation: jev_nl::Translation,
    /// Phrases whose meaning the semantic backend decided (one request).
    pub decided: usize,
    pub input_tokens: u64,
    /// Set when the backend could not be reached and defaults were used.
    pub error: Option<String>,
}

/// Compact description of the data for interpretation requests.
fn schema_summary(vocab: &jev_nl::Vocabulary) -> String {
    let mut lines = Vec::new();
    for t in &vocab.tables {
        let cols: Vec<String> = t
            .columns
            .iter()
            .map(|c| match c.values.len() {
                0 => c.name.clone(),
                _ => format!("{} ({})", c.name, c.values.iter().take(8).cloned().collect::<Vec<_>>().join("|")),
            })
            .collect();
        lines.push(format!("{}: {}", t.name, cols.join(", ")));
    }
    for r in &vocab.relations {
        lines.push(format!("each {} row belongs to one {} via {}", jev_nl_singular(&r.child), jev_nl_singular(&r.parent), r.key));
    }
    lines.join("\n")
}

fn jev_nl_singular(table: &str) -> String {
    table.strip_suffix('s').unwrap_or(table).to_string()
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
                        let child_table = tables.iter().find(|t: &&Table| t.name == *child).expect("built above");
                        let sum_percentiles = match jev_nl::money_column(child_table) {
                            Some(money) => Some((money.name.clone(), self.session.sum_percentiles(child, key, &money.name).await?)),
                            None => None,
                        };
                        relations.push(Relation {
                            parent: parent.clone(),
                            child: child.clone(),
                            key: key.to_string(),
                            count_percentiles: self.session.count_percentiles(child, key).await?,
                            sum_percentiles,
                        });
                    }
                }
            }
        }
        Ok(jev_nl::Vocabulary { tables, relations })
    }

    /// Turns a plain-English question into NQL. Phrases the rules can't settle
    /// are decided by the semantic backend, all in one request (one typed
    /// Choice per phrase, over the question and a schema summary). Without a
    /// backend, or if the request fails, the default readings are used.
    pub async fn interpret(
        &self,
        question: &str,
        vocab: &jev_nl::Vocabulary,
        today: (i32, u32, u32),
    ) -> Result<Interpretation, jev_nl::NlError> {
        let analysis = jev_nl::analyze(question, vocab, today)?;
        let decisions = analysis.decisions.clone();
        let Some(backend) = self.session.semantic_backend().filter(|_| !decisions.is_empty()) else {
            return Ok(Interpretation { translation: analysis.resolve(&[]), decided: 0, input_tokens: 0, error: None });
        };
        let request = jev_provider::SemanticRequest {
            state: serde_json::json!({ "question": question, "data": schema_summary(vocab) }),
            questions: decisions
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let options = d
                        .options
                        .iter()
                        .map(|o| jev_provider::ChoiceOption { label: o.label.clone(), description: Some(o.description.clone()) })
                        .collect();
                    (format!("d{i}"), jev_provider::Question::Choice { instructions: d.question.clone(), options })
                })
                .collect(),
        };
        match backend.evaluate(&request).await {
            Ok(response) => {
                let chosen: Vec<jev_nl::Chosen> = decisions
                    .iter()
                    .enumerate()
                    .map(|(i, d)| match response.answers.get(&format!("d{i}")) {
                        Some(jev_provider::Answer::Choice { label, confidence, probabilities }) => {
                            let index = |l: &str| d.options.iter().position(|o| o.label == l);
                            jev_nl::Chosen {
                                option: index(label).unwrap_or(0),
                                confidence: Some(*confidence),
                                runner_up: probabilities.iter().find(|(l, _)| l != label).and_then(|(l, p)| Some((index(l)?, *p))),
                            }
                        }
                        _ => jev_nl::Chosen { option: 0, confidence: None, runner_up: None },
                    })
                    .collect();
                Ok(Interpretation {
                    translation: analysis.resolve(&chosen),
                    decided: decisions.len(),
                    input_tokens: response.input_tokens,
                    error: None,
                })
            }
            Err(e) => Ok(Interpretation { translation: analysis.resolve(&[]), decided: 0, input_tokens: 0, error: Some(e.to_string()) }),
        }
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

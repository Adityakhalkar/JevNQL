//! Physical JevIR: *how* a query is executed.
//!
//! A physical plan alternates between two engines:
//! - [`DataFusionExec`]: a relational logical subtree executed by DataFusion;
//! - [`JevBatchExec`]: semantic operators evaluated on a semantic backend.
//!
//! A `DataFusionExec` whose subtree contains semantic operators lists them as
//! `inputs`: each is executed first and its rows are substituted for that
//! subtree, so data flows DataFusion -> Jev -> DataFusion.

use std::fmt;

use crate::display::label;
use crate::logical::{ChoiceOption, LogicalPlan, PlanRef};
use crate::types::Schema;

/// A physical plan plus the output schema its logical plan promised.
#[derive(Debug, Clone)]
pub struct PhysicalQuery {
    pub root: PhysicalPlan,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    DataFusion(DataFusionExec),
    JevBatch(JevBatchExec),
}

#[derive(Debug, Clone)]
pub struct DataFusionExec {
    /// Relational subtree; nodes listed in `inputs` are replaced by their results.
    pub plan: PlanRef,
    pub inputs: Vec<SemanticInput>,
}

/// A semantic node inside a relational subtree, and how to compute it.
#[derive(Debug, Clone)]
pub struct SemanticInput {
    /// The logical node (identity matters: matched by pointer).
    pub node: PlanRef,
    pub exec: PhysicalPlan,
}

/// Evaluates `ops` for every input row, sending the model only the `context`
/// columns. All ops share one request per distinct state.
#[derive(Debug, Clone)]
pub struct JevBatchExec {
    pub input: Box<PhysicalPlan>,
    pub context: Vec<String>,
    /// Applied in order; filters remove rows after all judgments are made.
    pub ops: Vec<SemanticOp>,
    /// Maximum requests in flight.
    pub concurrency: usize,
    pub cache: CachePolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SemanticOp {
    Filter { predicate: String, threshold: f64, output: Option<String> },
    Score { question: String, levels: Vec<String>, output: String },
    Choice { question: String, options: Vec<ChoiceOption>, output: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Reuse answers for identical (state, question) pairs and store new ones.
    ReadWrite,
    Disabled,
}

impl SemanticOp {
    /// The column this op adds, if any.
    pub fn output(&self) -> Option<&str> {
        match self {
            SemanticOp::Filter { output, .. } => output.as_deref(),
            SemanticOp::Score { output, .. } | SemanticOp::Choice { output, .. } => Some(output),
        }
    }
}

impl fmt::Display for SemanticOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticOp::Filter { predicate, threshold, .. } => write!(f, "filter p >= {threshold}: {predicate:?}"),
            SemanticOp::Score { question, output, .. } => write!(f, "score {output}: {question:?}"),
            SemanticOp::Choice { question, output, .. } => write!(f, "choice {output}: {question:?}"),
        }
    }
}

impl fmt::Display for PhysicalQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_physical(&self.root, "", f)
    }
}

fn write_physical(plan: &PhysicalPlan, prefix: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match plan {
        PhysicalPlan::JevBatch(j) => {
            let cache = match j.cache {
                CachePolicy::ReadWrite => "cache",
                CachePolicy::Disabled => "no cache",
            };
            writeln!(f, "JevBatchExec[context: {} | concurrency {} | {cache}]", j.context.join(", "), j.concurrency)?;
            for op in &j.ops {
                writeln!(f, "{prefix}│   {op}")?;
            }
            write!(f, "{prefix}└── ")?;
            write_physical(&j.input, &format!("{prefix}    "), f)
        }
        PhysicalPlan::DataFusion(d) => {
            writeln!(f, "DataFusionExec")?;
            write!(f, "{prefix}└── ")?;
            write_relational(&d.plan, d, &format!("{prefix}    "), f)
        }
    }
}

fn write_relational(node: &LogicalPlan, exec: &DataFusionExec, prefix: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if let Some(input) = exec.inputs.iter().find(|i| std::ptr::eq(i.node.as_ref(), node)) {
        return write_physical(&input.exec, prefix, f);
    }
    writeln!(f, "{}", label(&node.op))?;
    let inputs = node.inputs();
    for (i, child) in inputs.iter().enumerate() {
        let last = i + 1 == inputs.len();
        write!(f, "{prefix}{}", if last { "└── " } else { "├── " })?;
        write_relational(child, exec, &format!("{prefix}{}", if last { "    " } else { "│   " }), f)?;
    }
    Ok(())
}

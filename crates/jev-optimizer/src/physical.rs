//! Logical -> physical planning.
//!
//! Relational subtrees become [`DataFusionExec`]s; semantic operators become
//! [`JevBatchExec`]s. With `fuse`, a chain of semantic operators over the same
//! context shares one batch: each state is sent once with every question.
//!
//! Fusion never extends a batch past a SemanticFilter: the appended question
//! would be asked speculatively for rows the filter drops, which only pays
//! off when states are large relative to questions and the filter is
//! permissive. That trade needs a cost model; until then fusion is limited
//! to cases where it is never worse.

use jevir::ValidatedPlan;
use jevir::logical::{Op, PlanRef};
use jevir::physical::{CachePolicy, DataFusionExec, JevBatchExec, PhysicalPlan, PhysicalQuery, SemanticInput, SemanticOp};

#[derive(Debug, Clone)]
pub struct PhysicalConfig {
    pub concurrency: usize,
    pub cache: CachePolicy,
    /// Fuse adjacent semantic operators over the same context.
    pub fuse: bool,
}

impl Default for PhysicalConfig {
    fn default() -> Self {
        Self { concurrency: 16, cache: CachePolicy::ReadWrite, fuse: true }
    }
}

pub fn physical_plan(plan: &ValidatedPlan, config: &PhysicalConfig) -> PhysicalQuery {
    PhysicalQuery { root: plan_node(&plan.plan, config), schema: plan.schema.clone() }
}

fn plan_node(node: &PlanRef, config: &PhysicalConfig) -> PhysicalPlan {
    let batch = |input: &PlanRef, context: &[String], op: SemanticOp| match plan_node(input, config) {
        PhysicalPlan::JevBatch(mut below) if config.fuse && fusable(&below, context) => {
            below.ops.push(op);
            PhysicalPlan::JevBatch(below)
        }
        input => PhysicalPlan::JevBatch(JevBatchExec {
            input: Box::new(input),
            context: context.to_vec(),
            ops: vec![op],
            concurrency: config.concurrency,
            cache: config.cache,
        }),
    };
    match &node.op {
        Op::SemanticFilter(s) => batch(
            &s.input,
            &s.context,
            SemanticOp::Filter { predicate: s.predicate.clone(), threshold: s.threshold, output: s.output.clone() },
        ),
        Op::SemanticScore(s) => batch(
            &s.input,
            &s.context,
            SemanticOp::Score { question: s.question.clone(), levels: s.levels.clone(), output: s.output.clone() },
        ),
        Op::SemanticChoice(s) => batch(
            &s.input,
            &s.context,
            SemanticOp::Choice { question: s.question.clone(), options: s.options.clone(), output: s.output.clone() },
        ),
        _ => {
            let mut inputs = Vec::new();
            collect_semantic_inputs(node, config, &mut inputs);
            PhysicalPlan::DataFusion(DataFusionExec { plan: node.clone(), inputs })
        }
    }
}

/// Same context columns, none produced inside the batch, and no filter in
/// the batch (which would make the appended question speculative).
fn fusable(batch: &JevBatchExec, context: &[String]) -> bool {
    let mut a = batch.context.clone();
    let mut b = context.to_vec();
    a.sort();
    b.sort();
    a == b
        && !batch.ops.iter().any(|op| matches!(op, SemanticOp::Filter { .. }))
        && !batch.ops.iter().any(|op| op.output().is_some_and(|o| context.iter().any(|c| c == o)))
}

/// Finds the topmost semantic nodes below a relational node.
fn collect_semantic_inputs(node: &PlanRef, config: &PhysicalConfig, out: &mut Vec<SemanticInput>) {
    for child in node.inputs() {
        if child.op.is_semantic() {
            out.push(SemanticInput { node: child.clone(), exec: plan_node(child, config) });
        } else {
            collect_semantic_inputs(child, config, out);
        }
    }
}

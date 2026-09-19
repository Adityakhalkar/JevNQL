//! Logical -> physical planning.
//!
//! Relational subtrees become [`DataFusionExec`]s; each semantic operator
//! becomes a [`JevBatchExec`] over its input.

use jevir::ValidatedPlan;
use jevir::logical::{Op, PlanRef};
use jevir::physical::{CachePolicy, DataFusionExec, JevBatchExec, PhysicalPlan, PhysicalQuery, SemanticInput, SemanticOp};

#[derive(Debug, Clone)]
pub struct PhysicalConfig {
    pub concurrency: usize,
    pub cache: CachePolicy,
}

impl Default for PhysicalConfig {
    fn default() -> Self {
        Self { concurrency: 16, cache: CachePolicy::ReadWrite }
    }
}

pub fn physical_plan(plan: &ValidatedPlan, config: &PhysicalConfig) -> PhysicalQuery {
    PhysicalQuery { root: plan_node(&plan.plan, config), schema: plan.schema.clone() }
}

fn plan_node(node: &PlanRef, config: &PhysicalConfig) -> PhysicalPlan {
    let batch = |input: &PlanRef, context: &[String], op: SemanticOp| {
        PhysicalPlan::JevBatch(JevBatchExec {
            input: Box::new(plan_node(input, config)),
            context: context.to_vec(),
            ops: vec![op],
            concurrency: config.concurrency,
            cache: config.cache,
        })
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

//! Semantic late execution.
//!
//! Row-wise operators that only *append* a column (SemanticScore,
//! SemanticChoice, Fetch) commute with a TopK that does not sort by that
//! column: the same rows are selected either way, and the appended column is
//! computed per row. Moving them above the TopK means they run on k rows
//! instead of all candidates.
//!
//! SemanticFilter does *not* commute with TopK (filtering then taking the top
//! k differs from taking the top k then filtering), so it never moves here.
//! Deterministic filters already sink below semantic operators via predicate
//! pushdown, which is the other half of late execution.

use jevir::logical::{Op, PlanRef};

use crate::Trace;

pub(crate) fn late_execution(node: &PlanRef, trace: &mut Trace) -> PlanRef {
    let inputs = node.inputs().into_iter().map(|c| late_execution(c, trace)).collect();
    lift_above_top_k(&node.with_inputs(inputs), trace)
}

fn lift_above_top_k(node: &PlanRef, trace: &mut Trace) -> PlanRef {
    let Op::TopK(top) = &node.op else {
        return node.clone();
    };
    let child = &top.input;
    let liftable = matches!(child.op, Op::SemanticScore(_) | Op::SemanticChoice(_) | Op::Fetch(_));
    let appended = child.op.appended_column().unwrap_or_default();
    if !liftable || top.keys.iter().any(|k| k.expr.columns().contains(appended)) {
        return node.clone();
    }
    trace.fire("semantic late execution", format!("{} `{appended}` after TopK[{}]", child.op.name(), top.k));
    // TopK now reads the child's input; the child's other inputs stay put
    let mut child_inputs: Vec<PlanRef> = child.inputs().into_iter().cloned().collect();
    let lowered_top = lift_above_top_k(&node.with_inputs(vec![child_inputs[0].clone()]), trace);
    child_inputs[0] = lowered_top;
    child.with_inputs(child_inputs)
}

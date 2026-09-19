//! Predicate and projection pushdown.

use std::collections::BTreeSet;

use jevir::logical::{JoinType, LogicalPlan, Op, PlanRef};
use jevir::{Catalog, Expr, infer_schema};

use crate::Trace;

/// Output column names of a (valid) plan; empty if it cannot be typed, which
/// makes every rule below conservatively decline.
fn names(plan: &LogicalPlan, catalog: &dyn Catalog) -> BTreeSet<String> {
    infer_schema(plan, catalog).map(|s| s.names().into_iter().map(String::from).collect()).unwrap_or_default()
}

fn column_set(e: &Expr) -> BTreeSet<String> {
    e.columns().into_iter().map(String::from).collect()
}

/// Rebuilds `node` with a modified operator and new inputs.
fn rebuild(node: &LogicalPlan, edit: impl FnOnce(&mut Op<PlanRef>), inputs: Vec<PlanRef>) -> PlanRef {
    let mut op = node.op.clone();
    edit(&mut op);
    LogicalPlan { op }.with_inputs(inputs)
}

/// Deterministic predicate pushdown: every filter conjunct moves as far
/// toward the scans as is equivalent, in particular below semantic operators
/// that do not produce the columns it reads.
pub(crate) fn push_predicates(node: &PlanRef, catalog: &dyn Catalog, trace: &mut Trace) -> PlanRef {
    let inputs = node.inputs().into_iter().map(|c| push_predicates(c, catalog, trace)).collect();
    let node = node.with_inputs(inputs);
    match &node.op {
        Op::Filter(f) => f.predicate.conjuncts().into_iter().fold(f.input.clone(), |acc, conjunct| {
            push(conjunct.clone(), &acc, catalog, trace)
        }),
        _ => node,
    }
}

/// Returns `node` filtered by `pred`, with the filter placed as deep as possible.
fn push(pred: Expr, node: &PlanRef, catalog: &dyn Catalog, trace: &mut Trace) -> PlanRef {
    let cols = column_set(&pred);
    let reads_appended = node.op.appended_column().is_some_and(|c| cols.contains(c));
    let crossed = |trace: &mut Trace, what: &str| trace.fire("predicate pushdown", format!("`{pred}` below {what}"));
    match &node.op {
        op @ (Op::SemanticFilter(_) | Op::SemanticScore(_) | Op::SemanticChoice(_)) if !reads_appended => {
            crossed(trace, op.name());
            let input = node.inputs()[0].clone();
            node.with_inputs(vec![push(pred, &input, catalog, trace)])
        }
        Op::Fetch(f) if !reads_appended => {
            crossed(trace, "Fetch");
            node.with_inputs(vec![push(pred, &f.input, catalog, trace), f.source.clone()])
        }
        // filters commute
        Op::Filter(f) => node.with_inputs(vec![push(pred, &f.input, catalog, trace)]),
        Op::Sort(s) => node.with_inputs(vec![push(pred, &s.input, catalog, trace)]),
        Op::Aggregate(a) if cols.iter().all(|c| a.group_by.contains(c)) => {
            crossed(trace, "Aggregate");
            node.with_inputs(vec![push(pred, &a.input, catalog, trace)])
        }
        Op::Project(p) if cols.iter().all(|c| p.exprs.iter().any(|e| e.name == *c)) => {
            let inner = pred.substitute(&|c| p.exprs.iter().find(|e| e.name == c).map(|e| e.expr.clone()));
            crossed(trace, "Project");
            node.with_inputs(vec![push(inner, &p.input, catalog, trace)])
        }
        Op::Join(j) => {
            let (left, right) = (&j.left, &j.right);
            if !cols.is_empty() && cols.is_subset(&names(left, catalog)) {
                crossed(trace, "Join (left side)");
                node.with_inputs(vec![push(pred, left, catalog, trace), right.clone()])
            } else if j.join_type == JoinType::Inner && !cols.is_empty() && cols.is_subset(&names(right, catalog)) {
                crossed(trace, "Join (right side)");
                node.with_inputs(vec![left.clone(), push(pred, right, catalog, trace)])
            } else {
                filter(node, pred)
            }
        }
        _ => filter(node, pred),
    }
}

fn filter(input: &PlanRef, predicate: Expr) -> PlanRef {
    LogicalPlan::new(Op::Filter(jevir::logical::Filter { input: input.clone(), predicate }))
}

/// Merges stacked filters into one conjunction (cosmetic, fewer operators).
pub(crate) fn merge_filters(node: &PlanRef) -> PlanRef {
    let inputs = node.inputs().into_iter().map(merge_filters).collect();
    let node = node.with_inputs(inputs);
    if let Op::Filter(outer) = &node.op
        && let Op::Filter(inner) = &outer.input.op
    {
        let predicate = Expr::conjunction([inner.predicate.clone(), outer.predicate.clone()]).expect("two conjuncts");
        return filter(&inner.input, predicate);
    }
    node
}

/// Projection pushdown with dead-operator elimination: each operator keeps
/// only the columns its consumers need, scans read only those columns, and
/// semantic operators / fetches whose output nobody reads are removed.
pub(crate) fn prune(node: &PlanRef, required: &BTreeSet<String>, catalog: &dyn Catalog, trace: &mut Trace) -> PlanRef {
    let with = |extra: &BTreeSet<String>| required.union(extra).cloned().collect::<BTreeSet<_>>();
    let keys_of = |keys: &[jevir::SortKey]| keys.iter().flat_map(|k| column_set(&k.expr)).collect::<BTreeSet<_>>();
    match &node.op {
        Op::Scan(scan) => {
            let available: Vec<String> = match &scan.columns {
                Some(cols) => cols.clone(),
                None => catalog.table_schema(&scan.table).map(|s| s.names().iter().map(|n| n.to_string()).collect()).unwrap_or_default(),
            };
            let mut keep: Vec<String> = available.iter().filter(|c| required.contains(*c)).cloned().collect();
            if keep.is_empty() {
                keep = available.iter().take(1).cloned().collect();
            }
            if keep.len() == available.len() {
                return node.clone();
            }
            trace.fire("projection pushdown", format!("scan {} reads {} of {} columns", scan.table, keep.len(), available.len()));
            rebuild(node, |op| if let Op::Scan(s) = op { s.columns = Some(keep) }, vec![])
        }
        Op::Filter(f) => node.with_inputs(vec![prune(&f.input, &with(&column_set(&f.predicate)), catalog, trace)]),
        Op::Project(p) => {
            let mut kept: Vec<_> = p.exprs.iter().filter(|e| required.contains(&e.name)).cloned().collect();
            if kept.is_empty() {
                kept = p.exprs.iter().take(1).cloned().collect();
            }
            let need = kept.iter().flat_map(|e| column_set(&e.expr)).collect();
            let input = prune(&p.input, &need, catalog, trace);
            rebuild(node, |op| if let Op::Project(p) = op { p.exprs = kept }, vec![input])
        }
        Op::Join(j) => {
            let side = |plan: &PlanRef, keys: BTreeSet<String>| {
                let need: BTreeSet<String> = names(plan, catalog).intersection(required).cloned().chain(keys).collect();
                need
            };
            let left = side(&j.left, j.on.iter().map(|k| k.left.clone()).collect());
            let right = side(&j.right, j.on.iter().map(|k| k.right.clone()).collect());
            node.with_inputs(vec![prune(&j.left, &left, catalog, trace), prune(&j.right, &right, catalog, trace)])
        }
        Op::Aggregate(a) => {
            let need = a.group_by.iter().cloned().chain(a.aggregates.iter().filter_map(|g| g.arg.as_ref()).flat_map(column_set)).collect();
            node.with_inputs(vec![prune(&a.input, &need, catalog, trace)])
        }
        Op::Sort(s) => node.with_inputs(vec![prune(&s.input, &with(&keys_of(&s.keys)), catalog, trace)]),
        Op::TopK(t) => node.with_inputs(vec![prune(&t.input, &with(&keys_of(&t.keys)), catalog, trace)]),
        Op::Fetch(f) if !required.contains(&f.output) => {
            trace.fire("projection pushdown", format!("removed Fetch of unused `{}`", f.output));
            prune(&f.input, required, catalog, trace)
        }
        Op::Fetch(f) => {
            let mut input_need = with(&BTreeSet::from([f.on.left.clone()]));
            input_need.remove(&f.output);
            let source_need = f.fields.iter().cloned().chain([f.on.right.clone()]).chain(keys_of(&f.order_by)).collect();
            node.with_inputs(vec![prune(&f.input, &input_need, catalog, trace), prune(&f.source, &source_need, catalog, trace)])
        }
        Op::SemanticScore(_) | Op::SemanticChoice(_) if !required.contains(node.op.appended_column().expect("semantic output")) => {
            trace.fire("projection pushdown", format!("removed {} with unused output", node.op.name()));
            prune(node.inputs()[0], required, catalog, trace)
        }
        Op::SemanticFilter(_) | Op::SemanticScore(_) | Op::SemanticChoice(_) => {
            let context: BTreeSet<String> = node.op.semantic_context().expect("semantic").iter().cloned().collect();
            let mut need = with(&context);
            let appended = node.op.appended_column().map(String::from);
            if let Some(out) = &appended {
                need.remove(out);
            }
            let input = prune(node.inputs()[0], &need, catalog, trace);
            // an unread SemanticFilter probability column is not materialized
            let drop_prob = matches!(&node.op, Op::SemanticFilter(_)) && appended.is_some_and(|o| !required.contains(&o));
            rebuild(node, |op| if let (Op::SemanticFilter(s), true) = (op, drop_prob) { s.output = None }, vec![input])
        }
    }
}

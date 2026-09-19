//! JevNQL optimizer.
//!
//! Logical rewrites (all semantics-preserving):
//! 1. deterministic predicate pushdown, including below semantic operators;
//! 2. semantic late execution: semantic work moves after TopK when equivalent;
//! 3. projection pushdown with dead semantic-operator elimination.
//!
//! Physical planning then fuses adjacent semantic operators over the same
//! context into one Jev batch (see [`physical`]).

mod pushdown;
mod semantic;
pub mod physical;

use std::collections::BTreeSet;
use std::fmt;

use jevir::{Catalog, IrError, ValidatedPlan, infer_schema};

pub use physical::{PhysicalConfig, physical_plan};

/// One firing of an optimizer rule, for EXPLAIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleApplication {
    pub rule: &'static str,
    pub detail: String,
}

impl fmt::Display for RuleApplication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.rule, self.detail)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Trace {
    pub applied: Vec<RuleApplication>,
}

impl Trace {
    pub(crate) fn fire(&mut self, rule: &'static str, detail: String) {
        self.applied.push(RuleApplication { rule, detail });
    }
}

#[derive(Debug, Clone)]
pub struct Optimized {
    pub plan: ValidatedPlan,
    pub trace: Trace,
}

/// Rewrites a logical plan into a cheaper equivalent one.
pub fn optimize(plan: &ValidatedPlan, catalog: &dyn Catalog) -> Result<Optimized, IrError> {
    let mut trace = Trace::default();
    let rewritten = pushdown::push_predicates(&plan.plan, catalog, &mut trace);
    let rewritten = semantic::late_execution(&rewritten, &mut trace);
    let required: BTreeSet<String> = plan.schema.names().into_iter().map(String::from).collect();
    let rewritten = pushdown::prune(&rewritten, &required, catalog, &mut trace);
    let rewritten = pushdown::merge_filters(&rewritten);

    let schema = infer_schema(&rewritten, catalog)
        .map_err(|e| IrError::invalid(format!("optimizer produced an invalid plan: {e}")))?;
    if schema != plan.schema {
        return Err(IrError::invalid(format!("optimizer changed the output schema from {} to {schema}", plan.schema)));
    }
    Ok(Optimized { plan: ValidatedPlan { plan: rewritten, schema }, trace })
}

//! Logical JevIR: *what* a query computes.
//!
//! Operators are generic over their input reference `I`:
//! - [`LogicalPlan`] uses `Arc<LogicalPlan>` (an in-memory operator tree);
//! - the JSON step list (see [`crate::json`]) uses step-id strings.
//!
//! One definition serves both, so the frontend boundary and the IR cannot drift.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::expr::{AggregateExpr, Expr, NamedExpr, SortKey};

pub type PlanRef = Arc<LogicalPlan>;

#[derive(Debug, Clone, PartialEq)]
pub struct LogicalPlan {
    pub op: Op<PlanRef>,
}

impl LogicalPlan {
    pub fn new(op: Op<PlanRef>) -> PlanRef {
        Arc::new(LogicalPlan { op })
    }

    pub fn inputs(&self) -> Vec<&PlanRef> {
        self.op.inputs()
    }

    /// The same operator over new inputs (in [`Op::inputs`] order).
    pub fn with_inputs(&self, inputs: Vec<PlanRef>) -> PlanRef {
        assert_eq!(inputs.len(), self.inputs().len(), "{} input arity", self.op.name());
        let mut inputs = inputs.into_iter();
        let op = self
            .op
            .clone()
            .try_map_inputs(|_| Ok::<_, std::convert::Infallible>(inputs.next().expect("arity checked")))
            .unwrap_or_else(|e| match e {});
        LogicalPlan::new(op)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op<I> {
    // ---- deterministic ----
    Scan(Scan),
    Filter(Filter<I>),
    Project(Project<I>),
    Join(Join<I>),
    Aggregate(Aggregate<I>),
    Sort(Sort<I>),
    TopK(TopK<I>),
    Fetch(Fetch<I>),
    // ---- semantic ----
    SemanticFilter(SemanticFilter<I>),
    SemanticScore(SemanticScore<I>),
    SemanticChoice(SemanticChoice<I>),
}

/// Reads a base table. `columns` restricts (and orders) the columns read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub table: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter<I> {
    pub input: I,
    pub predicate: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project<I> {
    pub input: I,
    pub exprs: Vec<NamedExpr>,
}

/// Equi-join. Output: all left columns, then right columns except the right
/// join keys. Any other name present on both sides is a validation error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Join<I> {
    pub left: I,
    pub right: I,
    pub on: Vec<JoinKey>,
    #[serde(default)]
    pub join_type: JoinType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinKey {
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinType {
    #[default]
    Inner,
    Left,
}

/// Groups by `group_by` columns. Output: group columns, then aggregates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aggregate<I> {
    pub input: I,
    #[serde(default)]
    pub group_by: Vec<String>,
    pub aggregates: Vec<AggregateExpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sort<I> {
    pub input: I,
    pub keys: Vec<SortKey>,
}

/// The first `k` rows by `keys` (sort + limit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopK<I> {
    pub input: I,
    pub keys: Vec<SortKey>,
    pub k: usize,
}

/// Attaches, to each input row, the list of related `source` rows
/// (`input.on.left = source.on.right`) as a `list<struct<fields>>` column.
///
/// This is how per-entity context such as a customer's review history is
/// gathered for semantic judgment without multiplying rows the way a join does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fetch<I> {
    pub input: I,
    pub source: I,
    pub on: JoinKey,
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_by: Vec<SortKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    pub output: String,
}

/// Keeps rows for which the natural-language `predicate` holds with
/// probability >= `threshold`, judged from the `context` columns only.
/// If `output` is set, the probability is kept as a float64 column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticFilter<I> {
    pub input: I,
    pub context: Vec<String>,
    pub predicate: String,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

fn default_threshold() -> f64 {
    0.5
}

/// Rates each row along ordered `levels` (lowest first), judged from the
/// `context` columns. `output` is a float64 in [0, 1] (0 = first level).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticScore<I> {
    pub input: I,
    pub context: Vec<String>,
    pub question: String,
    pub levels: Vec<String>,
    pub output: String,
}

/// Labels each row with one of `options`, judged from the `context` columns.
/// `output` is a utf8 column holding the chosen label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticChoice<I> {
    pub input: I,
    pub context: Vec<String>,
    pub question: String,
    pub options: Vec<ChoiceOption>,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl<I> Op<I> {
    pub fn name(&self) -> &'static str {
        match self {
            Op::Scan(_) => "Scan",
            Op::Filter(_) => "Filter",
            Op::Project(_) => "Project",
            Op::Join(_) => "Join",
            Op::Aggregate(_) => "Aggregate",
            Op::Sort(_) => "Sort",
            Op::TopK(_) => "TopK",
            Op::Fetch(_) => "Fetch",
            Op::SemanticFilter(_) => "SemanticFilter",
            Op::SemanticScore(_) => "SemanticScore",
            Op::SemanticChoice(_) => "SemanticChoice",
        }
    }

    /// Whether the operator requires semantic (model) evaluation.
    pub fn is_semantic(&self) -> bool {
        matches!(self, Op::SemanticFilter(_) | Op::SemanticScore(_) | Op::SemanticChoice(_))
    }

    /// Columns a semantic operator shows the model.
    pub fn semantic_context(&self) -> Option<&[String]> {
        match self {
            Op::SemanticFilter(s) => Some(&s.context),
            Op::SemanticScore(s) => Some(&s.context),
            Op::SemanticChoice(s) => Some(&s.context),
            _ => None,
        }
    }

    /// The column an operator appends to its (first) input, if any.
    pub fn appended_column(&self) -> Option<&str> {
        match self {
            Op::SemanticFilter(s) => s.output.as_deref(),
            Op::SemanticScore(s) => Some(&s.output),
            Op::SemanticChoice(s) => Some(&s.output),
            Op::Fetch(f) => Some(&f.output),
            _ => None,
        }
    }

    /// Inputs in canonical order (join: left, right; fetch: input, source).
    pub fn inputs(&self) -> Vec<&I> {
        match self {
            Op::Scan(_) => vec![],
            Op::Filter(o) => vec![&o.input],
            Op::Project(o) => vec![&o.input],
            Op::Join(o) => vec![&o.left, &o.right],
            Op::Aggregate(o) => vec![&o.input],
            Op::Sort(o) => vec![&o.input],
            Op::TopK(o) => vec![&o.input],
            Op::Fetch(o) => vec![&o.input, &o.source],
            Op::SemanticFilter(o) => vec![&o.input],
            Op::SemanticScore(o) => vec![&o.input],
            Op::SemanticChoice(o) => vec![&o.input],
        }
    }

    /// Rebuilds the operator with each input mapped through `f`.
    pub fn try_map_inputs<J, E>(self, mut f: impl FnMut(I) -> Result<J, E>) -> Result<Op<J>, E> {
        Ok(match self {
            Op::Scan(o) => Op::Scan(o),
            Op::Filter(o) => Op::Filter(Filter { input: f(o.input)?, predicate: o.predicate }),
            Op::Project(o) => Op::Project(Project { input: f(o.input)?, exprs: o.exprs }),
            Op::Join(o) => Op::Join(Join { left: f(o.left)?, right: f(o.right)?, on: o.on, join_type: o.join_type }),
            Op::Aggregate(o) => {
                Op::Aggregate(Aggregate { input: f(o.input)?, group_by: o.group_by, aggregates: o.aggregates })
            }
            Op::Sort(o) => Op::Sort(Sort { input: f(o.input)?, keys: o.keys }),
            Op::TopK(o) => Op::TopK(TopK { input: f(o.input)?, keys: o.keys, k: o.k }),
            Op::Fetch(o) => Op::Fetch(Fetch {
                input: f(o.input)?,
                source: f(o.source)?,
                on: o.on,
                fields: o.fields,
                order_by: o.order_by,
                limit: o.limit,
                output: o.output,
            }),
            Op::SemanticFilter(o) => Op::SemanticFilter(SemanticFilter {
                input: f(o.input)?,
                context: o.context,
                predicate: o.predicate,
                threshold: o.threshold,
                output: o.output,
            }),
            Op::SemanticScore(o) => Op::SemanticScore(SemanticScore {
                input: f(o.input)?,
                context: o.context,
                question: o.question,
                levels: o.levels,
                output: o.output,
            }),
            Op::SemanticChoice(o) => Op::SemanticChoice(SemanticChoice {
                input: f(o.input)?,
                context: o.context,
                question: o.question,
                options: o.options,
                output: o.output,
            }),
        })
    }
}

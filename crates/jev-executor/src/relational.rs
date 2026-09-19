//! Lowering of relational JevIR operators to DataFusion plans.
//!
//! JevIR stays the source of truth: this module only translates already
//! validated operators. Semantic operators are not relational: the caller
//! executes them first and supplies their results as `materialized` inputs.

use std::collections::HashMap;

use datafusion::arrow::datatypes::{DataType as ArrowType, IntervalMonthDayNano};
use datafusion::common::{Column, ScalarValue};
use datafusion::execution::FunctionRegistry;
use datafusion::functions_aggregate::count::count_all;
use datafusion::functions_aggregate::expr_fn::{array_agg, avg, count, count_distinct, max, min, sum};
use datafusion::logical_expr::{ExprFunctionExt, Operator, SortExpr, binary_expr, cast};
use datafusion::prelude::{DataFrame, Expr as DfExpr, JoinType as DfJoinType, SessionContext, lit};
use jevir::logical::{JoinType, Op};
use jevir::{AggFunc, AggregateExpr, BinaryOp, Expr, Function, LogicalPlan, Scalar, SortKey, UnaryOp};

use crate::error::ExecError;

/// A lowered relation plus the row order it is known to carry.
#[derive(Clone)]
pub(crate) struct Lowered {
    pub df: DataFrame,
    /// Sort keys the rows are ordered by, if an upstream Sort/TopK ordering
    /// still applies. Filter, Fetch and semantic operators preserve order;
    /// Join and Aggregate do not.
    pub ordering: Option<Vec<SortKey>>,
}

pub(crate) struct Lowerer<'a> {
    pub ctx: &'a SessionContext,
    /// Base tables referenced by the plan, keyed by name.
    pub tables: &'a HashMap<String, DataFrame>,
    /// Results of already-executed semantic subtrees, keyed by node address.
    pub materialized: &'a HashMap<usize, Lowered>,
}

impl Lowerer<'_> {
    /// Lowers a plan whose operators are all relational.
    pub fn lower(&self, plan: &LogicalPlan) -> Result<Lowered, ExecError> {
        if let Some(done) = self.materialized.get(&(plan as *const LogicalPlan as usize)) {
            return Ok(done.clone());
        }
        let input = |i: usize| self.lower(plan.inputs()[i]);
        Ok(match &plan.op {
            Op::Scan(scan) => {
                let df = self.tables.get(&scan.table).cloned().ok_or_else(|| {
                    ExecError::Internal(format!("table `{}` was not prepared", scan.table))
                })?;
                let df = match &scan.columns {
                    Some(cols) => df.select(cols.iter().map(|c| column(None, c)).collect::<Vec<_>>())?,
                    None => df,
                };
                Lowered { df, ordering: None }
            }
            Op::Filter(f) => {
                let l = input(0)?;
                Lowered { df: l.df.filter(self.expr(&f.predicate)?)?, ordering: l.ordering }
            }
            Op::Project(p) => {
                let l = input(0)?;
                let exprs = p.exprs.iter().map(|e| Ok(self.expr(&e.expr)?.alias(&e.name))).collect::<Result<Vec<_>, ExecError>>()?;
                // ordering survives only if every key column passes through unchanged
                let passes = |c: &str| p.exprs.iter().any(|e| e.name == c && e.expr == jevir::expr::col(c));
                let ordering = l.ordering.filter(|keys| keys.iter().all(|k| k.expr.columns().into_iter().all(passes)));
                Lowered { df: l.df.select(exprs)?, ordering }
            }
            Op::Join(j) => {
                let (left, right) = (input(0)?.df, input(1)?.df);
                let left_names = field_names(&left);
                let right_names: Vec<String> = field_names(&right)
                    .into_iter()
                    .filter(|n| !j.on.iter().any(|k| k.right == *n))
                    .collect();
                let on = j.on.iter().map(|k| column(Some("l"), &k.left).eq(column(Some("r"), &k.right)));
                let join_type = match j.join_type {
                    JoinType::Inner => DfJoinType::Inner,
                    JoinType::Left => DfJoinType::Left,
                };
                let joined = left.alias("l")?.join_on(right.alias("r")?, join_type, on)?;
                let out = left_names
                    .iter()
                    .map(|n| column(Some("l"), n).alias(n))
                    .chain(right_names.iter().map(|n| column(Some("r"), n).alias(n)))
                    .collect::<Vec<_>>();
                Lowered { df: joined.select(out)?, ordering: None }
            }
            Op::Aggregate(a) => {
                let l = input(0)?;
                let groups = a.group_by.iter().map(|g| column(None, g)).collect();
                let aggs = a.aggregates.iter().map(|agg| self.aggregate(agg)).collect::<Result<Vec<_>, _>>()?;
                Lowered { df: l.df.aggregate(groups, aggs)?, ordering: None }
            }
            Op::Sort(s) => {
                let l = input(0)?;
                Lowered { df: l.df.sort(self.sort_exprs(&s.keys)?)?, ordering: Some(s.keys.clone()) }
            }
            Op::TopK(t) => {
                let l = input(0)?;
                let df = l.df.sort(self.sort_exprs(&t.keys)?)?.limit(0, Some(t.k))?;
                Lowered { df, ordering: Some(t.keys.clone()) }
            }
            Op::Fetch(f) => {
                let (l, source) = (input(0)?, input(1)?.df);
                let input_names = field_names(&l.df);
                let named_struct = self.udf("named_struct")?.call(
                    f.fields.iter().flat_map(|name| [lit(name.as_str()), column(None, name)]).collect(),
                );
                // ties broken by every orderable fetched field, so the list (and
                // what `limit` keeps) is deterministic
                let mut order = self.sort_exprs(&f.order_by)?;
                for name in &f.fields {
                    let nested = source.schema().field_with_unqualified_name(name).map(|x| x.data_type().is_nested());
                    let already = f.order_by.iter().any(|k| k.expr == jevir::expr::col(name.as_str()));
                    if matches!(nested, Ok(false)) && !already {
                        order.push(column(None, name).sort(true, false));
                    }
                }
                let list = array_agg(named_struct).order_by(order).build()?;
                let mut history = source.aggregate(vec![column(None, &f.on.right)], vec![list.alias(&f.output)])?;
                if let Some(limit) = f.limit {
                    let sliced = self
                        .udf("array_slice")?
                        .call(vec![column(None, &f.output), lit(1i64), lit(limit as i64)])
                        .alias(&f.output);
                    history = history.select(vec![column(None, &f.on.right), sliced])?;
                }
                let joined = l.df.alias("l")?.join_on(
                    history.alias("r")?,
                    DfJoinType::Left,
                    [column(Some("l"), &f.on.left).eq(column(Some("r"), &f.on.right))],
                )?;
                let out = input_names
                    .iter()
                    .map(|n| column(Some("l"), n).alias(n))
                    .chain([column(Some("r"), &f.output).alias(&f.output)])
                    .collect::<Vec<_>>();
                let mut df = joined.select(out)?;
                // the hash join does not keep input order; restore it
                if let Some(keys) = &l.ordering {
                    df = df.sort(self.sort_exprs(keys)?)?;
                }
                Lowered { df, ordering: l.ordering }
            }
            op @ (Op::SemanticFilter(_) | Op::SemanticScore(_) | Op::SemanticChoice(_)) => {
                return Err(ExecError::Internal(format!("{} reached relational lowering unexecuted", op.name())));
            }
        })
    }

    /// Lowers the plan root, re-establishing inherited row order.
    pub fn lower_root(&self, plan: &LogicalPlan) -> Result<Lowered, ExecError> {
        let lowered = self.lower(plan)?;
        match (&plan.op, &lowered.ordering) {
            (Op::Sort(_) | Op::TopK(_), _) | (_, None) => Ok(lowered),
            (_, Some(keys)) => Ok(Lowered { df: lowered.df.sort(self.sort_exprs(keys)?)?, ordering: lowered.ordering }),
        }
    }

    fn udf(&self, name: &str) -> Result<std::sync::Arc<datafusion::logical_expr::ScalarUDF>, ExecError> {
        Ok(self.ctx.udf(name)?)
    }

    fn sort_exprs(&self, keys: &[SortKey]) -> Result<Vec<SortExpr>, ExecError> {
        // NULLS LAST in both directions: "top" never means "unknown"
        keys.iter().map(|k| Ok(self.expr(&k.expr)?.sort(!k.descending, false))).collect()
    }

    fn aggregate(&self, agg: &AggregateExpr) -> Result<DfExpr, ExecError> {
        let arg = || -> Result<DfExpr, ExecError> {
            let arg = agg.arg.as_ref().ok_or_else(|| ExecError::Internal(format!("`{agg}` has no argument")))?;
            self.expr(arg)
        };
        let e = match (agg.func, &agg.arg) {
            (AggFunc::Count, None) => count_all(),
            (AggFunc::Count, Some(_)) => count(arg()?),
            (AggFunc::CountDistinct, _) => count_distinct(arg()?),
            (AggFunc::Sum, _) => sum(arg()?),
            (AggFunc::Avg, _) => avg(arg()?),
            (AggFunc::Min, _) => min(arg()?),
            (AggFunc::Max, _) => max(arg()?),
        };
        Ok(e.alias(&agg.output))
    }

    pub fn expr(&self, e: &Expr) -> Result<DfExpr, ExecError> {
        Ok(match e {
            Expr::Column { name } => column(None, name),
            Expr::Literal(s) => lit(scalar(s)),
            Expr::Binary { op, left, right } => {
                let (l, r) = (self.expr(left)?, self.expr(right)?);
                let op = match op {
                    BinaryOp::Eq => Operator::Eq,
                    BinaryOp::NotEq => Operator::NotEq,
                    BinaryOp::Lt => Operator::Lt,
                    BinaryOp::LtEq => Operator::LtEq,
                    BinaryOp::Gt => Operator::Gt,
                    BinaryOp::GtEq => Operator::GtEq,
                    BinaryOp::And => Operator::And,
                    BinaryOp::Or => Operator::Or,
                    BinaryOp::Add => Operator::Plus,
                    BinaryOp::Sub => Operator::Minus,
                    BinaryOp::Mul => Operator::Multiply,
                    // JevIR division is always real division
                    BinaryOp::Div => {
                        return Ok(binary_expr(
                            cast(l, ArrowType::Float64),
                            Operator::Divide,
                            cast(r, ArrowType::Float64),
                        ));
                    }
                };
                binary_expr(l, op, r)
            }
            Expr::Unary { op: UnaryOp::Not, expr } => DfExpr::Not(Box::new(self.expr(expr)?)),
            Expr::Unary { op: UnaryOp::Negate, expr } => DfExpr::Negative(Box::new(self.expr(expr)?)),
            Expr::Function { name, args } => {
                let args = args.iter().map(|a| self.expr(a)).collect::<Result<Vec<_>, _>>()?;
                let (udf, args) = match name {
                    Function::Lower => ("lower", args),
                    Function::Upper => ("upper", args),
                    Function::Length => ("character_length", args),
                    Function::Contains => ("contains", args),
                    Function::StartsWith => ("starts_with", args),
                    Function::Abs => ("abs", args),
                    Function::Round => ("round", args.into_iter().map(|a| cast(a, ArrowType::Float64)).collect()),
                    Function::Coalesce => ("coalesce", args),
                    Function::Year => ("date_part", [vec![lit("year")], args].concat()),
                    Function::Month => ("date_part", [vec![lit("month")], args].concat()),
                    Function::CurrentDate => ("current_date", args),
                };
                self.udf(udf)?.call(args)
            }
            Expr::InList { expr, list, negated } => {
                let list = list.iter().map(|a| self.expr(a)).collect::<Result<Vec<_>, _>>()?;
                self.expr(expr)?.in_list(list, *negated)
            }
            Expr::IsNull { expr, negated: false } => self.expr(expr)?.is_null(),
            Expr::IsNull { expr, negated: true } => self.expr(expr)?.is_not_null(),
        })
    }
}

/// Column reference built without identifier parsing (names may contain dots
/// or capitals).
fn column(qualifier: Option<&str>, name: &str) -> DfExpr {
    DfExpr::Column(Column::new(qualifier, name))
}

fn field_names(df: &DataFrame) -> Vec<String> {
    df.schema().fields().iter().map(|f| f.name().clone()).collect()
}

fn scalar(s: &Scalar) -> ScalarValue {
    match s {
        Scalar::Null => ScalarValue::Null,
        Scalar::Boolean(b) => ScalarValue::Boolean(Some(*b)),
        Scalar::Int64(v) => ScalarValue::Int64(Some(*v)),
        Scalar::Float64(v) => ScalarValue::Float64(Some(*v)),
        Scalar::Utf8(v) => ScalarValue::Utf8(Some(v.clone())),
        Scalar::Date(d) => ScalarValue::Date32(Some(*d)),
        Scalar::Interval { months, days } => {
            ScalarValue::IntervalMonthDayNano(Some(IntervalMonthDayNano::new(*months, *days, 0)))
        }
    }
}

//! Deterministic scalar expressions.
//!
//! Expressions are a typed AST, never SQL text. Backends lower them to their own
//! representation (e.g. DataFusion `Expr`). JSON form is internally tagged by
//! `kind`, e.g. `{"kind": "column", "name": "amount"}`.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::IrError;
use crate::types::{DataType, Schema, comparable, unify};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expr {
    Column {
        name: String,
    },
    Literal(Scalar),
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Function {
        name: Function,
        #[serde(default)]
        args: Vec<Expr>,
    },
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        #[serde(default)]
        negated: bool,
    },
    IsNull {
        expr: Box<Expr>,
        #[serde(default)]
        negated: bool,
    },
}

pub fn col(name: impl Into<String>) -> Expr {
    Expr::Column { name: name.into() }
}

pub fn lit(value: Scalar) -> Expr {
    Expr::Literal(value)
}

impl Expr {
    pub fn binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
        Expr::Binary { op, left: Box::new(left), right: Box::new(right) }
    }

    /// Splits `a AND b AND c` into its conjuncts.
    pub fn conjuncts(&self) -> Vec<&Expr> {
        match self {
            Expr::Binary { op: BinaryOp::And, left, right } => {
                let mut out = left.conjuncts();
                out.extend(right.conjuncts());
                out
            }
            e => vec![e],
        }
    }

    /// Joins expressions with AND; `None` if empty.
    pub fn conjunction(exprs: impl IntoIterator<Item = Expr>) -> Option<Expr> {
        exprs.into_iter().reduce(|acc, e| Expr::binary(acc, BinaryOp::And, e))
    }

    /// Replaces column references for which `f` returns an expression.
    pub fn substitute(&self, f: &dyn Fn(&str) -> Option<Expr>) -> Expr {
        let sub = |e: &Expr| Box::new(e.substitute(f));
        match self {
            Expr::Column { name } => f(name).unwrap_or_else(|| self.clone()),
            Expr::Literal(_) => self.clone(),
            Expr::Binary { op, left, right } => Expr::Binary { op: *op, left: sub(left), right: sub(right) },
            Expr::Unary { op, expr } => Expr::Unary { op: *op, expr: sub(expr) },
            Expr::Function { name, args } => {
                Expr::Function { name: *name, args: args.iter().map(|a| a.substitute(f)).collect() }
            }
            Expr::InList { expr, list, negated } => Expr::InList {
                expr: sub(expr),
                list: list.iter().map(|a| a.substitute(f)).collect(),
                negated: *negated,
            },
            Expr::IsNull { expr, negated } => Expr::IsNull { expr: sub(expr), negated: *negated },
        }
    }

    /// Names of all columns the expression reads.
    pub fn columns(&self) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        self.collect_columns(&mut out);
        out
    }

    fn collect_columns<'a>(&'a self, out: &mut BTreeSet<&'a str>) {
        match self {
            Expr::Column { name } => {
                out.insert(name);
            }
            Expr::Literal(_) => {}
            Expr::Binary { left, right, .. } => {
                left.collect_columns(out);
                right.collect_columns(out);
            }
            Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } => expr.collect_columns(out),
            Expr::Function { args, .. } => args.iter().for_each(|a| a.collect_columns(out)),
            Expr::InList { expr, list, .. } => {
                expr.collect_columns(out);
                list.iter().for_each(|a| a.collect_columns(out));
            }
        }
    }

    /// Infers the expression's type against an input schema.
    pub fn data_type(&self, schema: &Schema) -> Result<DataType, IrError> {
        use DataType::*;
        match self {
            Expr::Column { name } => {
                let t = &schema.resolve(name)?.data_type;
                if *t == Other {
                    return Err(IrError::invalid(format!("column `{name}` has an unsupported type")));
                }
                Ok(t.clone())
            }
            Expr::Literal(s) => Ok(s.data_type()),
            Expr::Binary { op, left, right } => {
                let (l, r) = (left.data_type(schema)?, right.data_type(schema)?);
                let bad = || IrError::invalid(format!("cannot apply `{op}` to {l} and {r} in `{self}`"));
                match op {
                    BinaryOp::And | BinaryOp::Or => match (&l, &r) {
                        (Boolean | Null, Boolean | Null) => Ok(Boolean),
                        _ => Err(bad()),
                    },
                    op if op.is_comparison() => comparable(&l, &r).then_some(Boolean).ok_or_else(bad),
                    BinaryOp::Div if l.is_numeric() && r.is_numeric() => Ok(Float64),
                    _ if l.is_numeric() && r.is_numeric() => {
                        Ok(if l == Int64 && r == Int64 { Int64 } else { Float64 })
                    }
                    // date +/- interval keeps its temporal type
                    BinaryOp::Add | BinaryOp::Sub if l.is_temporal() && r == Interval => Ok(l),
                    BinaryOp::Add if l == Interval && r.is_temporal() => Ok(r),
                    _ => Err(bad()),
                }
            }
            Expr::Unary { op, expr } => {
                let t = expr.data_type(schema)?;
                match (op, &t) {
                    (UnaryOp::Not, Boolean | Null) => Ok(Boolean),
                    (UnaryOp::Negate, t) if t.is_numeric() || *t == Interval => Ok(t.clone()),
                    _ => Err(IrError::invalid(format!("cannot apply `{op}` to {t} in `{self}`"))),
                }
            }
            Expr::Function { name, args } => {
                let types = args.iter().map(|a| a.data_type(schema)).collect::<Result<Vec<_>, _>>()?;
                name.return_type(&types).map_err(|m| IrError::invalid(format!("{m} in `{self}`")))
            }
            Expr::InList { expr, list, .. } => {
                let t = expr.data_type(schema)?;
                if list.is_empty() {
                    return Err(IrError::invalid(format!("empty IN list in `{self}`")));
                }
                for item in list {
                    let it = item.data_type(schema)?;
                    if !comparable(&t, &it) {
                        return Err(IrError::invalid(format!("IN list item of type {it} is not comparable with {t} in `{self}`")));
                    }
                }
                Ok(Boolean)
            }
            Expr::IsNull { expr, .. } => {
                expr.data_type(schema)?;
                Ok(Boolean)
            }
        }
    }

    /// Binding strength used to print minimal parentheses.
    fn precedence(&self) -> u8 {
        match self {
            Expr::Binary { op, .. } => op.precedence(),
            Expr::Unary { op: UnaryOp::Not, .. } => 3,
            Expr::InList { .. } | Expr::IsNull { .. } => 4,
            Expr::Unary { op: UnaryOp::Negate, .. } => 7,
            _ => 8,
        }
    }

    fn fmt_prec(&self, f: &mut fmt::Formatter<'_>, min: u8) -> fmt::Result {
        if self.precedence() < min {
            f.write_str("(")?;
            fmt::Display::fmt(self, f)?;
            return f.write_str(")");
        }
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Column { name } => f.write_str(name),
            Expr::Literal(s) => write!(f, "{s}"),
            Expr::Binary { op, left, right } => {
                let p = op.precedence();
                left.fmt_prec(f, p)?;
                write!(f, " {op} ")?;
                // left-associative: an equal-precedence right operand needs parens
                right.fmt_prec(f, p + 1)
            }
            Expr::Unary { op: UnaryOp::Not, expr } => {
                f.write_str("NOT ")?;
                expr.fmt_prec(f, 3)
            }
            Expr::Unary { op: UnaryOp::Negate, expr } => {
                f.write_str("-")?;
                expr.fmt_prec(f, 7)
            }
            Expr::Function { name, args } => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(")")
            }
            Expr::InList { expr, list, negated } => {
                expr.fmt_prec(f, 5)?;
                f.write_str(if *negated { " NOT IN (" } else { " IN (" })?;
                for (i, a) in list.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(")")
            }
            Expr::IsNull { expr, negated } => {
                expr.fmt_prec(f, 5)?;
                f.write_str(if *negated { " IS NOT NULL" } else { " IS NULL" })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "!=")]
    NotEq,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    LtEq,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    GtEq,
    #[serde(rename = "and")]
    And,
    #[serde(rename = "or")]
    Or,
    #[serde(rename = "+")]
    Add,
    #[serde(rename = "-")]
    Sub,
    #[serde(rename = "*")]
    Mul,
    #[serde(rename = "/")]
    Div,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        use BinaryOp::*;
        matches!(self, Eq | NotEq | Lt | LtEq | Gt | GtEq)
    }

    fn precedence(self) -> u8 {
        use BinaryOp::*;
        match self {
            Or => 1,
            And => 2,
            Eq | NotEq | Lt | LtEq | Gt | GtEq => 4,
            Add | Sub => 5,
            Mul | Div => 6,
        }
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use BinaryOp::*;
        f.write_str(match self {
            Eq => "=",
            NotEq => "!=",
            Lt => "<",
            LtEq => "<=",
            Gt => ">",
            GtEq => ">=",
            And => "AND",
            Or => "OR",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOp {
    Not,
    Negate,
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UnaryOp::Not => "NOT",
            UnaryOp::Negate => "-",
        })
    }
}

/// Whitelisted scalar functions. Each has a checked signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Function {
    Lower,
    Upper,
    Length,
    Contains,
    StartsWith,
    Abs,
    Round,
    Coalesce,
    Year,
    Month,
    CurrentDate,
}

impl Function {
    fn name(self) -> &'static str {
        use Function::*;
        match self {
            Lower => "lower",
            Upper => "upper",
            Length => "length",
            Contains => "contains",
            StartsWith => "starts_with",
            Abs => "abs",
            Round => "round",
            Coalesce => "coalesce",
            Year => "year",
            Month => "month",
            CurrentDate => "current_date",
        }
    }

    pub fn return_type(self, args: &[DataType]) -> Result<DataType, String> {
        use DataType::*;
        use Function::*;
        let sig = |expected: &str| format!("`{}` expects ({expected}), got ({})", self.name(), join_types(args));
        let is_str = |t: &DataType| matches!(t, Utf8 | Null);
        match (self, args) {
            (Lower | Upper, [t]) if is_str(t) => Ok(Utf8),
            (Length, [t]) if is_str(t) => Ok(Int64),
            (Contains | StartsWith, [a, b]) if is_str(a) && is_str(b) => Ok(Boolean),
            (Abs, [t]) if t.is_numeric() => Ok(t.clone()),
            (Round, [t]) | (Round, [t, Int64]) if t.is_numeric() => Ok(Float64),
            (Year | Month, [t]) if t.is_temporal() => Ok(Int64),
            (CurrentDate, []) => Ok(Date),
            (Coalesce, [first, rest @ ..]) if !rest.is_empty() => rest
                .iter()
                .try_fold(first.clone(), |acc, t| unify(&acc, t))
                .ok_or_else(|| sig("arguments of a common type")),
            (Lower | Upper | Length, _) => Err(sig("utf8")),
            (Contains | StartsWith, _) => Err(sig("utf8, utf8")),
            (Abs, _) => Err(sig("numeric")),
            (Round, _) => Err(sig("numeric[, int64]")),
            (Year | Month, _) => Err(sig("date or timestamp")),
            (CurrentDate, _) => Err(sig("")),
            (Coalesce, _) => Err(sig("at least two arguments")),
        }
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn join_types(types: &[DataType]) -> String {
    types.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

/// A typed literal value.
///
/// JSON form: `{"value": 42}` infers the type from the JSON value;
/// `{"value": "2026-01-01", "type": "date"}` and
/// `{"value": "30 days", "type": "interval"}` give it explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawScalar", into = "RawScalar")]
pub enum Scalar {
    Null,
    Boolean(bool),
    Int64(i64),
    Float64(f64),
    Utf8(String),
    /// Days since 1970-01-01 (Arrow Date32).
    Date(i32),
    Interval { months: i32, days: i32 },
}

impl Scalar {
    pub fn data_type(&self) -> DataType {
        match self {
            Scalar::Null => DataType::Null,
            Scalar::Boolean(_) => DataType::Boolean,
            Scalar::Int64(_) => DataType::Int64,
            Scalar::Float64(_) => DataType::Float64,
            Scalar::Utf8(_) => DataType::Utf8,
            Scalar::Date(_) => DataType::Date,
            Scalar::Interval { .. } => DataType::Interval,
        }
    }

    pub fn date(iso: &str) -> Result<Scalar, IrError> {
        parse_date(iso).map(Scalar::Date).ok_or_else(|| IrError::invalid(format!("invalid date `{iso}`, expected YYYY-MM-DD")))
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::Null => f.write_str("NULL"),
            Scalar::Boolean(b) => f.write_str(if *b { "TRUE" } else { "FALSE" }),
            Scalar::Int64(v) => write!(f, "{v}"),
            Scalar::Float64(v) => write!(f, "{v:?}"),
            Scalar::Utf8(s) => write!(f, "'{}'", s.replace('\'', "''")),
            Scalar::Date(d) => write!(f, "DATE '{}'", format_date(*d)),
            Scalar::Interval { months, days } => write!(f, "INTERVAL '{}'", format_interval(*months, *days)),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScalar {
    value: serde_json::Value,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    ty: Option<String>,
}

impl TryFrom<RawScalar> for Scalar {
    type Error = String;

    fn try_from(raw: RawScalar) -> Result<Self, String> {
        use serde_json::Value as V;
        let mismatch = |ty: &str| format!("literal {} is not a valid {ty}", raw.value);
        match (raw.ty.as_deref(), &raw.value) {
            (_, V::Null) => Ok(Scalar::Null),
            (None | Some("boolean"), V::Bool(b)) => Ok(Scalar::Boolean(*b)),
            (None | Some("int64"), V::Number(n)) if n.is_i64() => Ok(Scalar::Int64(n.as_i64().unwrap())),
            (None | Some("float64"), V::Number(n)) => n.as_f64().map(Scalar::Float64).ok_or_else(|| mismatch("float64")),
            (None | Some("utf8"), V::String(s)) => Ok(Scalar::Utf8(s.clone())),
            (Some("date"), V::String(s)) => parse_date(s).map(Scalar::Date).ok_or_else(|| mismatch("date (YYYY-MM-DD)")),
            (Some("interval"), V::String(s)) => {
                parse_interval(s).map(|(months, days)| Scalar::Interval { months, days }).ok_or_else(|| {
                    mismatch("interval (e.g. \"30 days\", \"2 weeks\", \"3 months\", \"1 year\")")
                })
            }
            (Some(t @ ("boolean" | "int64" | "float64" | "utf8" | "date" | "interval")), _) => Err(mismatch(t)),
            (Some(t), _) => Err(format!(
                "unknown literal type `{t}`; expected boolean, int64, float64, utf8, date or interval"
            )),
            (None, v) => Err(format!("unsupported literal {v}")),
        }
    }
}

impl From<Scalar> for RawScalar {
    fn from(s: Scalar) -> Self {
        use serde_json::Value as V;
        let (value, ty) = match s {
            Scalar::Null => (V::Null, None),
            Scalar::Boolean(b) => (V::Bool(b), None),
            Scalar::Int64(v) => (V::from(v), None),
            Scalar::Float64(v) => (V::from(v), Some("float64")),
            Scalar::Utf8(s) => (V::String(s), None),
            Scalar::Date(d) => (V::String(format_date(d)), Some("date")),
            Scalar::Interval { months, days } => (V::String(format_interval(months, days)), Some("interval")),
        };
        RawScalar { value, ty: ty.map(str::to_string) }
    }
}

// Civil-date conversion (Howard Hinnant's days_from_civil / civil_from_days).

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parse_date(s: &str) -> Option<i32> {
    let mut parts = s.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    let days = days_from_civil(y, m, d);
    // round-trip rejects out-of-range months/days such as 2026-02-30
    (civil_from_days(days) == (y, m, d)).then(|| i32::try_from(days).ok()).flatten()
}

fn format_date(days: i32) -> String {
    let (y, m, d) = civil_from_days(i64::from(days));
    format!("{y:04}-{m:02}-{d:02}")
}

fn parse_interval(s: &str) -> Option<(i32, i32)> {
    let (n, unit) = s.trim().split_once(char::is_whitespace)?;
    let n: i32 = n.parse().ok()?;
    match unit.trim().trim_end_matches('s') {
        "day" => Some((0, n)),
        "week" => Some((0, n.checked_mul(7)?)),
        "month" => Some((n, 0)),
        "year" => Some((n.checked_mul(12)?, 0)),
        _ => None,
    }
}

fn format_interval(months: i32, days: i32) -> String {
    let plural = |n: i32, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match (months, days) {
        (m, 0) if m != 0 && m % 12 == 0 => plural(m / 12, "year"),
        (m, 0) if m != 0 => plural(m, "month"),
        (0, d) => plural(d, "day"),
        (m, d) => format!("{} {}", plural(m, "month"), plural(d, "day")),
    }
}

/// An aggregate call with its output column name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateExpr {
    pub func: AggFunc,
    /// Omitted only for `count` (i.e. `COUNT(*)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<Expr>,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggFunc {
    Count,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggregateExpr {
    pub fn data_type(&self, schema: &Schema) -> Result<DataType, IrError> {
        use AggFunc::*;
        let arg = self.arg.as_ref().map(|a| a.data_type(schema)).transpose()?;
        let bad = |expected: &str| IrError::invalid(format!("`{self}`: {expected}"));
        match (self.func, arg) {
            (Count, _) => Ok(DataType::Int64),
            (_, None) => Err(bad("requires an argument")),
            (CountDistinct, Some(_)) => Ok(DataType::Int64),
            (Sum, Some(DataType::Int64)) => Ok(DataType::Int64),
            (Sum, Some(t)) if t.is_numeric() => Ok(DataType::Float64),
            (Avg, Some(t)) if t.is_numeric() => Ok(DataType::Float64),
            (Sum | Avg, Some(t)) => Err(bad(&format!("expects a numeric argument, got {t}"))),
            (Min | Max, Some(t)) if t.is_orderable() => Ok(t),
            (Min | Max, Some(t)) => Err(bad(&format!("expects an orderable argument, got {t}"))),
        }
    }
}

impl fmt::Display for AggregateExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.func {
            AggFunc::Count => "COUNT",
            AggFunc::CountDistinct => "COUNT_DISTINCT",
            AggFunc::Sum => "SUM",
            AggFunc::Avg => "AVG",
            AggFunc::Min => "MIN",
            AggFunc::Max => "MAX",
        };
        match &self.arg {
            Some(a) => write!(f, "{name}({a}) AS {}", self.output),
            None => write!(f, "{name}(*) AS {}", self.output),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortKey {
    pub expr: Expr,
    #[serde(default)]
    pub descending: bool,
}

impl fmt::Display for SortKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.expr, if self.descending { " DESC" } else { "" })
    }
}

/// A projected expression and its output name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedExpr {
    pub name: String,
    pub expr: Expr,
}

impl fmt::Display for NamedExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.expr {
            Expr::Column { name } if *name == self.name => f.write_str(name),
            e => write!(f, "{e} AS {}", self.name),
        }
    }
}

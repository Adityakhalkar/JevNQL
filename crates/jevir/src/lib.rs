//! JevIR: the typed intermediate representation of JevNQL.
//!
//! Every frontend (natural language, later JevSQL) compiles to logical JevIR;
//! every execution backend consumes plans derived from it. Natural language
//! never compiles to SQL directly.

pub mod display;
pub mod error;
pub mod expr;
pub mod json;
pub mod logical;
pub mod types;
pub mod validate;

pub use error::IrError;
pub use expr::{AggFunc, AggregateExpr, BinaryOp, Expr, Function, NamedExpr, Scalar, SortKey, UnaryOp};
pub use json::{ValidatedPlan, decode, encode};
pub use logical::{LogicalPlan, Op, PlanRef};
pub use types::{DataType, Field, Schema};
pub use validate::{Catalog, derive_schema, infer_schema};

//! JevNQL executor.
//!
//! Relational JevIR operators are lowered to Apache DataFusion; semantic
//! operators run on a semantic backend (added in a later step).

pub mod error;
mod relational;
pub mod session;
pub mod types;

pub use error::ExecError;
pub use session::{QueryResult, Session};

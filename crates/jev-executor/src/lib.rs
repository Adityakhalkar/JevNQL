//! JevNQL executor.
//!
//! Executes physical JevIR: relational segments on Apache DataFusion,
//! semantic batches on a [`jev_provider::SemanticBackend`].

pub mod error;
pub mod metrics;
mod relational;
mod semantic;
pub mod session;
pub mod types;

pub use error::ExecError;
pub use metrics::ExecMetrics;
pub use session::{DEFAULT_MAX_SEMANTIC_ROWS, QueryResult, Session};

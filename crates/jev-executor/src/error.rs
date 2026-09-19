use datafusion::error::DataFusionError;
use jevir::IrError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecError {
    #[error(transparent)]
    Ir(#[from] IrError),
    #[error("datafusion: {0}")]
    DataFusion(#[from] DataFusionError),
    #[error("cannot register `{path}`: {reason}")]
    Register { path: String, reason: String },
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// JevIR and the execution engine disagree; always an engine bug.
    #[error("internal error: {0}")]
    Internal(String),
}

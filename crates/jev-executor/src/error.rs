use datafusion::arrow::error::ArrowError;
use datafusion::error::DataFusionError;
use jev_provider::ProviderError;
use jevir::IrError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecError {
    #[error(transparent)]
    Ir(#[from] IrError),
    #[error("datafusion: {0}")]
    DataFusion(#[from] DataFusionError),
    #[error("arrow: {0}")]
    Arrow(#[from] ArrowError),
    #[error("semantic backend: {0}")]
    Provider(#[from] ProviderError),
    #[error("cannot register `{path}`: {reason}")]
    Register { path: String, reason: String },
    #[error("plan has semantic operators but no semantic backend is configured")]
    NoSemanticBackend,
    #[error(
        "{rows} rows would be sent to the semantic backend (limit {max}); reduce candidates deterministically or raise the limit"
    )]
    SemanticBudget { rows: usize, max: usize },
    #[error("a semantic state is ~{tokens} tokens (limit {limit}); pass fewer context columns or limit fetched rows")]
    StateTooLarge { tokens: usize, limit: usize },
    #[error("cancelled before sending anything to the semantic backend")]
    Cancelled,
    /// JevIR and the execution engine disagree; always an engine bug.
    #[error("internal error: {0}")]
    Internal(String),
}

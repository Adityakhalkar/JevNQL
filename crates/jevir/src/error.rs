use thiserror::Error;

/// Errors from decoding or validating JevIR.
///
/// Messages are written to be actionable by the frontend that produced the plan
/// (including an LLM-based compiler repairing its own output).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IrError {
    #[error("malformed plan: {0}")]
    Malformed(String),
    #[error("step `{step}`: {message}")]
    Step { step: String, message: String },
    #[error("{0}")]
    Invalid(String),
}

impl IrError {
    pub fn invalid(message: impl Into<String>) -> Self {
        IrError::Invalid(message.into())
    }

    /// Attributes an error to a plan step, keeping existing attribution.
    pub fn at_step(self, step: &str) -> Self {
        match self {
            IrError::Invalid(message) | IrError::Malformed(message) => {
                IrError::Step { step: step.to_string(), message }
            }
            e @ IrError::Step { .. } => e,
        }
    }
}

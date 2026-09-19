//! Semantic backend interface for JevNQL.
//!
//! The engine asks typed questions about a JSON `state` and gets typed answers
//! back. It knows nothing about any provider's HTTP API: provider specifics
//! live in implementations such as [`typesafe::TypeSafeJevBackend`].
//!
//! One request carries several questions about the same state (a noul, score
//! or choice each), because System One models read the state once and answer
//! every question in parallel; this is what makes semantic batching cheap.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use thiserror::Error;

pub mod mock;
pub mod simulated;
pub mod typesafe;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A typed semantic judgment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Question {
    /// Probability that the statement holds for the state.
    Noul { instructions: String },
    /// Position along ordered levels (lowest first).
    Score { instructions: String, levels: Vec<String> },
    /// One label out of a fixed set.
    Choice { instructions: String, options: Vec<ChoiceOption> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChoiceOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Noul { probability: f64 },
    /// `value` is normalized to [0, 1]: 0 = first level, 1 = last level.
    Score { value: f64, confidence: f64 },
    /// `probabilities` lists every option, most likely first.
    Choice { label: String, confidence: f64, probabilities: Vec<(String, f64)> },
}

#[derive(Debug, Clone)]
pub struct SemanticRequest {
    pub state: Value,
    /// Question ids are for the caller only; they carry no meaning to the model.
    pub questions: Vec<(String, Question)>,
}

#[derive(Debug, Clone)]
pub struct SemanticResponse {
    pub answers: HashMap<String, Answer>,
    pub input_tokens: u64,
}

/// Static facts about a backend, used for scheduling, limits and cost.
#[derive(Debug, Clone)]
pub struct BackendInfo {
    /// Stable identifier, also used as the semantic cache namespace.
    pub name: String,
    pub usd_per_million_input_tokens: f64,
    /// Suggested number of requests in flight.
    pub max_concurrency: usize,
    /// Largest state the backend accepts, in (approximate) tokens.
    pub max_state_tokens: usize,
}

pub trait SemanticBackend: Send + Sync {
    fn info(&self) -> &BackendInfo;

    /// Answers every question in `request` against its state.
    fn evaluate<'a>(&'a self, request: &'a SemanticRequest) -> BoxFuture<'a, Result<SemanticResponse, ProviderError>>;
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("missing API key: set {0}")]
    MissingApiKey(&'static str),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

/// Rough token count (~4 characters per token), for limits and estimates.
pub fn approx_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

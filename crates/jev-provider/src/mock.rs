//! Deterministic in-process backend for tests.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use crate::{
    Answer, BackendInfo, BoxFuture, ProviderError, Question, SemanticBackend, SemanticRequest, SemanticResponse,
    approx_tokens,
};

type AnswerFn = dyn Fn(&Value, &Question) -> Answer + Send + Sync;

/// Answers questions with a caller-supplied function and counts requests.
pub struct MockBackend {
    info: BackendInfo,
    answer: Box<AnswerFn>,
    requests: AtomicUsize,
    questions: AtomicUsize,
}

impl MockBackend {
    pub fn new(answer: impl Fn(&Value, &Question) -> Answer + Send + Sync + 'static) -> Self {
        Self {
            info: BackendInfo {
                name: "mock".into(),
                usd_per_million_input_tokens: 0.042,
                max_concurrency: 4,
                max_state_tokens: 30_000,
            },
            answer: Box::new(answer),
            requests: AtomicUsize::new(0),
            questions: AtomicUsize::new(0),
        }
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    pub fn questions(&self) -> usize {
        self.questions.load(Ordering::SeqCst)
    }
}

impl SemanticBackend for MockBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn evaluate<'a>(&'a self, request: &'a SemanticRequest) -> BoxFuture<'a, Result<SemanticResponse, ProviderError>> {
        Box::pin(async move {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.questions.fetch_add(request.questions.len(), Ordering::SeqCst);
            let answers = request.questions.iter().map(|(id, q)| (id.clone(), (self.answer)(&request.state, q))).collect();
            let text: usize = request.questions.iter().map(|(_, q)| format!("{q:?}").len()).sum();
            let input_tokens = (approx_tokens(&request.state.to_string()) + text / 4) as u64;
            Ok(SemanticResponse { answers, input_tokens })
        })
    }
}

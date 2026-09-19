use std::sync::Arc;
use std::time::Duration;

/// Live execution events, for progress displays.
#[derive(Debug, Clone)]
pub enum Progress {
    /// A semantic batch is about to send `requests` backend calls for `rows` rows.
    SemanticStart { label: String, rows: usize, requests: usize },
    /// `done` of the batch's requests have completed.
    SemanticAdvance { done: usize },
    SemanticEnd,
}

/// Returns `false` on `SemanticStart` to cancel the batch before any request.
pub type ProgressHook = Arc<dyn Fn(&Progress) -> bool + Send + Sync>;

/// Execution metrics reported with every result.
#[derive(Debug, Clone, Default)]
pub struct ExecMetrics {
    /// Rows read from base tables by DataFusion.
    pub rows_scanned: usize,
    /// JevBatchExec operators executed.
    pub semantic_batches: usize,
    /// Rows fed into semantic operators.
    pub semantic_rows: usize,
    /// Distinct context states among those rows.
    pub distinct_states: usize,
    /// Semantic backend requests sent.
    pub requests: usize,
    /// Questions sent to the backend (a request can carry several).
    pub questions: usize,
    /// Questions answered from the semantic cache.
    pub cache_hits: usize,
    pub input_tokens: u64,
    pub semantic_time: Duration,
    pub total_time: Duration,
    pub estimated_cost_usd: f64,
}

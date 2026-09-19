mod common;

use std::sync::Arc;

use common::*;
use jev_executor::{ExecError, Session};
use jev_optimizer::{PhysicalConfig, physical_plan};
use jev_provider::mock::MockBackend;
use jev_provider::{Answer, Question};
use serde_json::{Value, json};

/// All non-date string values anywhere in a state.
fn texts(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if !s.starts_with(|c: char| c.is_ascii_digit()) => out.push(s.to_lowercase()),
        Value::Array(a) => a.iter().for_each(|x| texts(x, out)),
        Value::Object(o) => o.values().for_each(|x| texts(x, out)),
        _ => {}
    }
}

fn about_price(s: &str) -> bool {
    s.contains("pric") || s.contains("expens")
}

/// Pricing-complaint "model": score = share of texts about price.
fn mock() -> Arc<MockBackend> {
    Arc::new(MockBackend::new(|state, q| {
        let mut all = Vec::new();
        texts(state, &mut all);
        let priced = all.iter().filter(|t| about_price(t)).count();
        match q {
            Question::Noul { .. } => Answer::Noul { probability: if priced > 0 { 0.9 } else { 0.1 } },
            Question::Score { .. } => {
                Answer::Score { value: priced as f64 / all.len().max(1) as f64, confidence: 0.8 }
            }
            Question::Choice { options, .. } => {
                let label = &options[usize::from(all.iter().any(|t| t == "smb"))].label;
                Answer::Choice { label: label.clone(), confidence: 0.9 }
            }
        }
    }))
}

async fn semantic_session() -> (Session, Arc<MockBackend>, tempfile::TempDir) {
    let backend = mock();
    let (s, dir) = session_with(Session::new().with_semantic_backend(backend.clone())).await;
    (s, backend, dir)
}

#[tokio::test]
async fn example_plan_runs_hybrid() {
    let (s, backend, _dir) = semantic_session().await;
    let result = run_json(&s, EXAMPLE).await.unwrap();

    // DataFusion reduced 7 orders to 3 candidates; only those reach the model
    let rows = rows(&result);
    let ranked: Vec<(&str, &str)> = rows.iter().map(|r| (r[0].as_str(), r[3].as_str())).collect();
    assert_eq!(ranked, [("2", "0.6666666666666666"), ("1", "0.5"), ("3", "0.0")]);
    assert_eq!(backend.requests(), 3);
    let m = &result.metrics;
    assert_eq!((m.semantic_rows, m.distinct_states, m.requests, m.cache_hits), (3, 3, 3, 0));
    assert!(m.input_tokens > 0 && m.estimated_cost_usd > 0.0);
    // 7 orders + 6 reviews read; one semantic batch
    assert_eq!((m.rows_scanned, m.semantic_batches), (13, 1));
}

#[tokio::test]
async fn physical_plan_explains_engine_split() {
    let (s, _backend, _dir) = semantic_session().await;
    let plan = jevir::decode(EXAMPLE, &s).unwrap();
    let text = physical_plan(&plan, &PhysicalConfig::default()).to_string();
    let heads: Vec<&str> = text.lines().map(|l| l.split('[').next().unwrap()).collect();
    assert_eq!(
        heads,
        [
            "DataFusionExec",
            "└── TopK",
            "    └── JevBatchExec",
            "        │   score dissatisfaction: \"Across this customer's reviews in chronological order, how increasingly dissatisfied do they appear with our pricing?\"",
            "        └── DataFusionExec",
            "            └── Fetch",
            "                ├── TopK",
            "                │   └── Aggregate",
            "                │       └── Filter",
            "                │           └── Scan",
            "                └── Scan",
        ],
        "\n{text}"
    );
}

#[tokio::test]
async fn relational_work_resumes_after_semantic_filter() {
    let (s, backend, _dir) = semantic_session().await;
    let result = run(
        &s,
        json!([
            {"id": "r", "op": "scan", "table": "reviews"},
            {"id": "f", "op": "semantic_filter", "input": "r", "context": ["text"],
             "predicate": "The review complains about pricing"},
            {"id": "a", "op": "aggregate", "input": "f", "group_by": ["customer_id"],
             "aggregates": [{"func": "count", "output": "complaints"}]},
            {"id": "s", "op": "sort", "input": "a", "keys": [{"expr": c("customer_id")}]}
        ]),
    )
    .await
    .unwrap();
    assert_eq!(rows(&result), [["1", "1"], ["2", "2"]]);
    assert_eq!(backend.requests(), 6);
}

#[tokio::test]
async fn filter_keeps_probability_and_row_order() {
    let (s, _backend, _dir) = semantic_session().await;
    let result = run(
        &s,
        json!([
            {"id": "r", "op": "scan", "table": "reviews"},
            {"id": "s", "op": "sort", "input": "r", "keys": [{"expr": c("review_id"), "descending": true}]},
            {"id": "f", "op": "semantic_filter", "input": "s", "context": ["text"],
             "predicate": "The review complains about pricing", "threshold": 0.5, "output": "p_pricing"},
            {"id": "p", "op": "project", "input": "f", "exprs": [
                {"name": "review_id", "expr": c("review_id")}, {"name": "p_pricing", "expr": c("p_pricing")}]}
        ]),
    )
    .await
    .unwrap();
    assert_eq!(rows(&result), [["5", "0.9"], ["4", "0.9"], ["2", "0.9"]]);
}

#[tokio::test]
async fn identical_states_are_deduplicated_and_cached() {
    let (s, backend, _dir) = semantic_session().await;
    let steps = json!([
        {"id": "c", "op": "scan", "table": "customers"},
        {"id": "k", "op": "semantic_choice", "input": "c", "context": ["segment"],
         "question": "Is this account large or small?",
         "options": [{"label": "large"}, {"label": "small"}], "output": "size"},
        {"id": "s", "op": "sort", "input": "k", "keys": [{"expr": c("customer_id")}]}
    ]);
    let first = run(&s, steps.clone()).await.unwrap();
    let sizes: Vec<String> = rows(&first).into_iter().map(|r| r[3].clone()).collect();
    assert_eq!(sizes, ["large", "small", "large", "small"]);
    // 4 rows, 2 distinct segments -> 2 requests
    assert_eq!((first.metrics.semantic_rows, first.metrics.requests), (4, 2));

    let second = run(&s, steps).await.unwrap();
    assert_eq!((second.metrics.requests, second.metrics.cache_hits), (0, 2));
    assert_eq!(backend.requests(), 2);
}

#[tokio::test]
async fn semantic_row_budget_is_enforced_before_any_request() {
    let (mut s, backend, _dir) = semantic_session().await;
    s.set_max_semantic_rows(3);
    let err = run(
        &s,
        json!([
            {"id": "r", "op": "scan", "table": "reviews"},
            {"id": "f", "op": "semantic_filter", "input": "r", "context": ["text"], "predicate": "Mentions price"}
        ]),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ExecError::SemanticBudget { rows: 6, max: 3 }), "{err}");
    assert_eq!(backend.requests(), 0);
}

/// Runs `steps` naively and optimized (fresh sessions, so no shared cache) and
/// checks both return identical rows; returns (naive, optimized) metrics.
async fn equivalent(steps: Value) -> (jev_executor::ExecMetrics, jev_executor::ExecMetrics) {
    let doc = json!({"version": 1, "steps": steps}).to_string();
    let (naive_session, _, _d1) = semantic_session().await;
    let (opt_session, _, _d2) = semantic_session().await;
    let naive = run_naive(&naive_session, &doc).await.unwrap();
    let optimized = run_optimized(&opt_session, &doc).await.unwrap();
    assert_eq!(rows(&naive), rows(&optimized), "optimized plan changed the result");
    assert!(!rows(&naive).is_empty());
    (naive.metrics, optimized.metrics)
}

#[tokio::test]
async fn optimizer_preserves_results_and_cuts_semantic_rows() {
    let (naive, opt) = equivalent(json!([
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "spend", "op": "aggregate", "input": "o", "group_by": ["customer_id"],
         "aggregates": [{"func": "sum", "arg": c("amount"), "output": "total_spend"}]},
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "hist", "op": "fetch", "input": "spend", "source": "r",
         "on": {"left": "customer_id", "right": "customer_id"}, "fields": ["text"], "output": "history"},
        {"id": "sc", "op": "semantic_score", "input": "hist", "context": ["history"],
         "question": "How unhappy with pricing?", "levels": ["content", "unhappy"], "output": "unhappy"},
        {"id": "big", "op": "filter", "input": "sc", "predicate": bin(">", c("total_spend"), l(json!(100.0)))},
        {"id": "top", "op": "top_k", "input": "big", "k": 2, "keys": [{"expr": c("total_spend"), "descending": true}]},
        {"id": "out", "op": "project", "input": "top", "exprs": [
            {"name": "customer_id", "expr": c("customer_id")}, {"name": "unhappy", "expr": c("unhappy")}]}
    ]))
    .await;
    // all 4 customers judged naively; only the top 2 after optimization
    assert_eq!((naive.semantic_rows, opt.semantic_rows), (4, 2));
}

#[tokio::test]
async fn deterministic_filter_runs_before_semantic_filter() {
    let (naive, opt) = equivalent(json!([
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "f", "op": "semantic_filter", "input": "r", "context": ["text"], "predicate": "Complains about pricing"},
        {"id": "low", "op": "filter", "input": "f", "predicate": bin("<=", c("rating"), l(json!(2)))}
    ]))
    .await;
    assert_eq!((naive.semantic_rows, opt.semantic_rows), (6, 2));
}

#[tokio::test]
async fn fused_batch_sends_each_state_once() {
    let (naive, opt) = equivalent(json!([
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "sc", "op": "semantic_score", "input": "r", "context": ["text"], "question": "How unhappy with pricing?",
         "levels": ["content", "unhappy"], "output": "unhappy"},
        {"id": "k", "op": "semantic_choice", "input": "sc", "context": ["text"], "question": "Which segment does this sound like?",
         "options": [{"label": "enterprise"}, {"label": "smb"}], "output": "sounds_like"},
        {"id": "s", "op": "sort", "input": "k", "keys": [{"expr": c("review_id")}]}
    ]))
    .await;
    // naive: 6 score + 6 choice requests; fused: 6 requests, 2 questions each
    assert_eq!((naive.requests, opt.requests), (12, 6));
    assert_eq!((naive.questions, opt.questions), (12, 12));
    assert!(opt.input_tokens < naive.input_tokens, "{} vs {}", opt.input_tokens, naive.input_tokens);
}

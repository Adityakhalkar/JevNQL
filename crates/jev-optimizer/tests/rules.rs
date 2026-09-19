use std::collections::HashMap;

use jev_optimizer::{PhysicalConfig, optimize, physical_plan};
use jevir::{DataType, Field, Schema};
use serde_json::{Value, json};

fn catalog() -> HashMap<String, Schema> {
    use DataType::*;
    let table = |fields: &[(&str, DataType)]| {
        Schema::new(fields.iter().map(|(n, t)| Field::new(*n, t.clone())).collect()).unwrap()
    };
    HashMap::from([
        ("customers".into(), table(&[("customer_id", Int64), ("name", Utf8), ("segment", Utf8)])),
        ("orders".into(), table(&[("order_id", Int64), ("customer_id", Int64), ("amount", Float64), ("order_date", Date)])),
        ("reviews".into(), table(&[("review_id", Int64), ("customer_id", Int64), ("rating", Int64), ("text", Utf8)])),
    ])
}

fn c(name: &str) -> Value {
    json!({"kind": "column", "name": name})
}

fn gt(col: &str, v: Value) -> Value {
    json!({"kind": "binary", "op": ">", "left": c(col), "right": {"kind": "literal", "value": v}})
}

/// Optimized plan as indented operator labels.
fn optimized(steps: Value) -> (String, Vec<String>) {
    let cat = catalog();
    let plan = jevir::decode(&json!({"version": 1, "steps": steps}).to_string(), &cat).unwrap();
    let out = optimize(&plan, &cat).unwrap();
    assert_eq!(out.plan.schema, plan.schema);
    (out.plan.plan.to_string(), out.trace.applied.iter().map(ToString::to_string).collect())
}

fn score(input: &str, context: &str, output: &str) -> Value {
    json!({"id": output, "op": "semantic_score", "input": input, "context": [context],
           "question": "How dissatisfied is this customer?", "levels": ["content", "unhappy"], "output": output})
}

/// A plan written in the "obvious" order: judge everyone, then narrow down.
fn naive_top_spenders() -> Value {
    json!([
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "spend", "op": "aggregate", "input": "o", "group_by": ["customer_id"],
         "aggregates": [{"func": "sum", "arg": c("amount"), "output": "total_spend"}]},
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "hist", "op": "fetch", "input": "spend", "source": "r",
         "on": {"left": "customer_id", "right": "customer_id"}, "fields": ["text"], "output": "history"},
        score("hist", "history", "unhappy"),
        {"id": "big", "op": "filter", "input": "unhappy", "predicate": gt("total_spend", json!(100.0))},
        {"id": "top", "op": "top_k", "input": "big", "k": 10, "keys": [{"expr": c("total_spend"), "descending": true}]},
        {"id": "out", "op": "project", "input": "top", "exprs": [
            {"name": "customer_id", "expr": c("customer_id")}, {"name": "unhappy", "expr": c("unhappy")}]}
    ])
}

#[test]
fn deterministic_reduction_runs_before_semantic_work() {
    let (plan, rules) = optimized(naive_top_spenders());
    let shape: Vec<&str> = plan.lines().map(|l| l.split('[').next().unwrap()).collect();
    assert_eq!(
        shape,
        [
            "Project",
            "└── SemanticScore",
            "    └── Fetch",
            "        ├── TopK",
            "        │   └── Filter",
            "        │       └── Aggregate",
            "        │           └── Scan",
            "        └── Scan",
        ],
        "\n{plan}"
    );
    assert!(plan.contains("Scan[orders: customer_id, amount]"), "{plan}");
    assert!(plan.contains("Scan[reviews: customer_id, text]"), "{plan}");
    for expected in [
        "predicate pushdown: `total_spend > 100.0` below SemanticScore",
        "predicate pushdown: `total_spend > 100.0` below Fetch",
        "semantic late execution: SemanticScore `unhappy` after TopK[10]",
        "semantic late execution: Fetch `history` after TopK[10]",
        "projection pushdown: scan orders reads 2 of 4 columns",
    ] {
        assert!(rules.iter().any(|r| r == expected), "missing `{expected}` in {rules:#?}");
    }
}

#[test]
fn semantic_filter_never_moves_past_top_k() {
    let (plan, _) = optimized(json!([
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "f", "op": "semantic_filter", "input": "r", "context": ["text"], "predicate": "Complains about price"},
        {"id": "t", "op": "top_k", "input": "f", "k": 3, "keys": [{"expr": c("rating")}]}
    ]));
    assert!(plan.starts_with("TopK[3: rating]\n└── SemanticFilter"), "{plan}");
}

#[test]
fn top_k_on_semantic_output_keeps_semantic_below() {
    let (plan, _) = optimized(json!([
        {"id": "c", "op": "scan", "table": "customers"},
        score("c", "name", "unhappy"),
        {"id": "t", "op": "top_k", "input": "unhappy", "k": 3, "keys": [{"expr": c("unhappy"), "descending": true}]}
    ]));
    assert!(plan.starts_with("TopK[3: unhappy DESC]\n└── SemanticScore"), "{plan}");
}

#[test]
fn filter_on_semantic_output_stays_above_it() {
    let (plan, _) = optimized(json!([
        {"id": "c", "op": "scan", "table": "customers"},
        score("c", "name", "unhappy"),
        {"id": "f", "op": "filter", "input": "unhappy", "predicate": {"kind": "binary", "op": "and",
            "left": gt("unhappy", json!(0.5)), "right": gt("customer_id", json!(10))}}
    ]));
    let shape: Vec<&str> = plan.lines().collect();
    assert_eq!(shape[0], "Filter[unhappy > 0.5]", "{plan}");
    assert!(shape[1].starts_with("└── SemanticScore"), "{plan}");
    assert_eq!(shape[2], "    └── Filter[customer_id > 10]", "{plan}");
}

#[test]
fn unused_semantic_work_is_removed() {
    let (plan, rules) = optimized(json!([
        {"id": "c", "op": "scan", "table": "customers"},
        score("c", "name", "unhappy"),
        {"id": "p", "op": "project", "input": "unhappy", "exprs": [{"name": "segment", "expr": c("segment")}]}
    ]));
    assert_eq!(plan, "Project[segment]\n└── Scan[customers: segment]\n");
    assert!(rules.contains(&"projection pushdown: removed SemanticScore with unused output".to_string()));
}

#[test]
fn filters_push_through_projections_and_joins() {
    let (plan, _) = optimized(json!([
        {"id": "c", "op": "scan", "table": "customers"},
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "j", "op": "join", "left": "c", "right": "o", "on": [{"left": "customer_id", "right": "customer_id"}]},
        {"id": "p", "op": "project", "input": "j", "exprs": [
            {"name": "who", "expr": c("name")},
            {"name": "cents", "expr": {"kind": "binary", "op": "*", "left": c("amount"), "right": {"kind": "literal", "value": 100}}}]},
        {"id": "f", "op": "filter", "input": "p", "predicate": {"kind": "binary", "op": "and",
            "left": gt("cents", json!(5000)), "right": {"kind": "binary", "op": "=", "left": c("who"), "right": {"kind": "literal", "value": "Asha"}}}}
    ]));
    let expected = "\
Project[name AS who, amount * 100 AS cents]
└── Join[inner: customer_id = customer_id]
    ├── Filter[name = 'Asha']
    │   └── Scan[customers: customer_id, name]
    └── Filter[amount * 100 > 5000]
        └── Scan[orders: customer_id, amount]
";
    assert_eq!(plan, expected);
}

#[test]
fn left_join_keeps_right_side_filters_above() {
    let (plan, _) = optimized(json!([
        {"id": "c", "op": "scan", "table": "customers"},
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "j", "op": "join", "left": "c", "right": "o", "join_type": "left",
         "on": [{"left": "customer_id", "right": "customer_id"}]},
        {"id": "f", "op": "filter", "input": "j", "predicate": gt("amount", json!(10.0))}
    ]));
    assert!(plan.starts_with("Filter[amount > 10.0]\n└── Join[left"), "{plan}");
}

#[test]
fn same_context_semantic_ops_fuse_into_one_batch() {
    let cat = catalog();
    let filter = json!({"op": "semantic_filter", "context": ["text"], "predicate": "Complains about price"});
    let score = |context: &str| {
        json!({"op": "semantic_score", "context": [context], "question": "How angry is the review?",
               "levels": ["calm", "angry"], "output": "anger"})
    };
    // scan -> first -> second
    let chain = |first: &Value, second: &Value| {
        let mut a = first.clone();
        a["id"] = json!("a");
        a["input"] = json!("r");
        let mut b = second.clone();
        b["id"] = json!("b");
        b["input"] = json!("a");
        json!({"version": 1, "steps": [{"id": "r", "op": "scan", "table": "reviews"}, a, b]}).to_string()
    };
    let batches = |doc: String, fuse: bool| {
        let plan = jevir::decode(&doc, &cat).unwrap();
        let text = physical_plan(&plan, &PhysicalConfig { fuse, ..Default::default() }).to_string();
        text.matches("JevBatchExec").count()
    };
    assert_eq!(batches(chain(&score("text"), &filter), true), 1);
    assert_eq!(batches(chain(&score("text"), &filter), false), 2);
    // different context: separate states, nothing to share
    assert_eq!(batches(chain(&score("rating"), &filter), true), 2);
    // never speculate past a filter
    assert_eq!(batches(chain(&filter, &score("text")), true), 2);
}

use std::collections::HashMap;

use jevir::expr::{col, lit};
use jevir::{BinaryOp, DataType, Expr, Field, IrError, Scalar, Schema, decode, encode};
use serde_json::{Value, json};

const EXAMPLE: &str = include_str!("../../../examples/plans/high_value_unhappy.json");

fn catalog() -> HashMap<String, Schema> {
    let table = |fields: &[(&str, DataType)]| {
        Schema::new(fields.iter().map(|(n, t)| Field::new(*n, t.clone())).collect()).unwrap()
    };
    use DataType::*;
    HashMap::from([
        ("customers".into(), table(&[("customer_id", Int64), ("name", Utf8), ("segment", Utf8)])),
        (
            "orders".into(),
            table(&[("order_id", Int64), ("customer_id", Int64), ("amount", Float64), ("order_date", Date)]),
        ),
        (
            "reviews".into(),
            table(&[("review_id", Int64), ("customer_id", Int64), ("rating", Int64), ("text", Utf8), ("created_at", Date)]),
        ),
    ])
}

fn decode_steps(steps: Value) -> Result<jevir::ValidatedPlan, IrError> {
    decode(&json!({ "version": 1, "steps": steps }).to_string(), &catalog())
}

/// Asserts decoding fails at `step` with a message containing `needle`.
fn assert_step_error(steps: Value, step: &str, needle: &str) {
    match decode_steps(steps) {
        Err(IrError::Step { step: s, message }) => {
            assert_eq!(s, step, "wrong step for: {message}");
            assert!(message.contains(needle), "expected `{needle}` in: {message}");
        }
        other => panic!("expected error at step `{step}`, got {other:?}"),
    }
}

fn c(name: &str) -> Value {
    json!({"kind": "column", "name": name})
}

#[test]
fn example_plan_infers_schema() {
    let v = decode(EXAMPLE, &catalog()).unwrap();
    let names: Vec<&str> = v.schema.names();
    assert_eq!(names, ["customer_id", "total_spend", "review_history", "dissatisfaction"]);
    assert_eq!(v.schema.field("total_spend").unwrap().data_type, DataType::Float64);
    assert_eq!(
        v.schema.field("review_history").unwrap().data_type.to_string(),
        "list<struct<created_at: date, rating: int64, text: utf8>>"
    );
}

#[test]
fn example_plan_explains_as_tree() {
    let v = decode(EXAMPLE, &catalog()).unwrap();
    let text = v.plan.to_string();
    let expected = [
        "TopK[20: dissatisfaction DESC]",
        "└── SemanticScore[dissatisfaction (3 levels): ",
        "    └── Fetch[review_history <- customer_id.customer_id: created_at, rating, text | order created_at | limit 50]",
        "        ├── TopK[500: total_spend DESC]",
        "        │   └── Aggregate[by customer_id | SUM(amount) AS total_spend]",
        "        │       └── Filter[order_date >= DATE '2026-01-01']",
        "        │           └── Scan[orders]",
        "        └── Scan[reviews]",
    ];
    for (line, want) in text.lines().zip(expected) {
        assert!(line.starts_with(want), "line `{line}` should start with `{want}`\n{text}");
    }
    assert_eq!(text.lines().count(), expected.len());
}

#[test]
fn encode_decode_round_trip() {
    let v = decode(EXAMPLE, &catalog()).unwrap();
    let again = decode(&encode(&v.plan).to_string(), &catalog()).unwrap();
    assert_eq!(v.plan, again.plan);
    assert_eq!(v.schema, again.schema);
}

#[test]
fn shared_subplan_is_encoded_once() {
    let v = decode_steps(json!([
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "big", "op": "filter", "input": "o",
         "predicate": {"kind": "binary", "op": ">", "left": c("amount"), "right": {"kind": "literal", "value": 100}}},
        {"id": "hist", "op": "fetch", "input": "big", "source": "big",
         "on": {"left": "customer_id", "right": "customer_id"}, "fields": ["amount"], "output": "same_customer"}
    ]))
    .unwrap();
    let steps = encode(&v.plan)["steps"].as_array().unwrap().len();
    assert_eq!(steps, 3);
}

#[test]
fn semantic_ops_type_their_outputs() {
    let v = decode_steps(json!([
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "f", "op": "semantic_filter", "input": "r", "context": ["text"],
         "predicate": "The review complains about price", "threshold": 0.8, "output": "p_price"},
        {"id": "ch", "op": "semantic_choice", "input": "f", "context": ["text"],
         "question": "Which product area does the review mainly discuss?",
         "options": [{"label": "billing"}, {"label": "quality", "description": "defects, durability"}],
         "output": "area"}
    ]))
    .unwrap();
    assert_eq!(v.schema.field("p_price").unwrap().data_type, DataType::Float64);
    assert_eq!(v.schema.field("area").unwrap().data_type, DataType::Utf8);
}

#[test]
fn unknown_column_names_step_and_alternatives() {
    assert_step_error(
        json!([
            {"id": "o", "op": "scan", "table": "orders"},
            {"id": "f", "op": "filter", "input": "o",
             "predicate": {"kind": "binary", "op": ">", "left": c("revenue"), "right": {"kind": "literal", "value": 1}}}
        ]),
        "f",
        "unknown column `revenue`; available: order_id, customer_id, amount, order_date",
    );
}

#[test]
fn rejects_type_errors() {
    // filter on a non-boolean
    assert_step_error(
        json!([{"id": "o", "op": "scan", "table": "orders"}, {"id": "f", "op": "filter", "input": "o", "predicate": c("amount")}]),
        "f",
        "must be boolean",
    );
    // comparing a date with a string
    assert_step_error(
        json!([{"id": "o", "op": "scan", "table": "orders"},
               {"id": "f", "op": "filter", "input": "o",
                "predicate": {"kind": "binary", "op": ">=", "left": c("order_date"), "right": {"kind": "literal", "value": "2026-01-01"}}}]),
        "f",
        "cannot apply `>=` to date and utf8",
    );
    // sum over text
    assert_step_error(
        json!([{"id": "r", "op": "scan", "table": "reviews"},
               {"id": "a", "op": "aggregate", "input": "r", "group_by": ["customer_id"],
                "aggregates": [{"func": "sum", "arg": c("text"), "output": "x"}]}]),
        "a",
        "expects a numeric argument, got utf8",
    );
}

#[test]
fn rejects_malformed_steps() {
    // typo'd field is not silently ignored
    assert_step_error(
        json!([{"id": "r", "op": "scan", "table": "reviews"},
               {"id": "s", "op": "semantic_filter", "input": "r", "context": ["text"], "predicate": "angry", "treshold": 0.9}]),
        "s",
        "unknown field `treshold`",
    );
    assert_step_error(json!([{"id": "x", "op": "explode", "input": "y"}]), "x", "unknown variant `explode`");
    assert_step_error(
        json!([{"id": "f", "op": "filter", "input": "later", "predicate": c("x")}, {"id": "later", "op": "scan", "table": "orders"}]),
        "f",
        "not defined by an earlier step",
    );
    assert_step_error(
        json!([{"id": "o", "op": "scan", "table": "orders"}, {"id": "o", "op": "scan", "table": "orders"}]),
        "o",
        "duplicate step id",
    );
    assert_step_error(
        json!([{"id": "o", "op": "scan", "table": "orders"}, {"id": "c", "op": "scan", "table": "customers"}]),
        "o",
        "never used",
    );
    assert_step_error(json!([{"id": "o", "op": "scan", "table": "invoices"}]), "o", "unknown table `invoices`");
}

#[test]
fn rejects_invalid_semantic_ops() {
    let scan = json!({"id": "r", "op": "scan", "table": "reviews"});
    assert_step_error(
        json!([scan, {"id": "s", "op": "semantic_filter", "input": "r", "context": ["text"], "predicate": "angry", "threshold": 1.5}]),
        "s",
        "threshold must be in [0, 1]",
    );
    assert_step_error(
        json!([scan, {"id": "s", "op": "semantic_score", "input": "r", "context": ["text"], "question": "How angry?",
                      "levels": ["calm"], "output": "anger"}]),
        "s",
        "at least two",
    );
    assert_step_error(
        json!([scan, {"id": "s", "op": "semantic_choice", "input": "r", "context": ["text"], "question": "Which?",
                      "options": [{"label": "a"}, {"label": "a"}], "output": "pick"}]),
        "s",
        "distinct",
    );
    assert_step_error(
        json!([scan, {"id": "s", "op": "semantic_score", "input": "r", "context": ["body"], "question": "How angry?",
                      "levels": ["calm", "angry"], "output": "anger"}]),
        "s",
        "unknown column `body`",
    );
    assert_step_error(
        json!([scan, {"id": "s", "op": "semantic_score", "input": "r", "context": ["text"], "question": "How angry?",
                      "levels": ["calm", "angry"], "output": "rating"}]),
        "s",
        "`rating` already exists",
    );
}

#[test]
fn join_drops_right_keys_and_rejects_collisions() {
    let v = decode_steps(json!([
        {"id": "c", "op": "scan", "table": "customers"},
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "j", "op": "join", "left": "c", "right": "o", "on": [{"left": "customer_id", "right": "customer_id"}]}
    ]))
    .unwrap();
    assert_eq!(v.schema.names(), ["customer_id", "name", "segment", "order_id", "amount", "order_date"]);

    assert_step_error(
        json!([
            {"id": "o", "op": "scan", "table": "orders"},
            {"id": "r", "op": "scan", "table": "reviews"},
            {"id": "j", "op": "join", "left": "o", "right": "r", "on": [{"left": "order_id", "right": "review_id"}]}
        ]),
        "j",
        "`customer_id` exists on both sides",
    );
}

#[test]
fn literals_parse_and_print() {
    let date: Scalar = serde_json::from_value(json!({"value": "2024-02-29", "type": "date"})).unwrap();
    assert_eq!(date.to_string(), "DATE '2024-02-29'");
    assert_eq!(serde_json::to_value(&date).unwrap(), json!({"value": "2024-02-29", "type": "date"}));
    assert!(serde_json::from_value::<Scalar>(json!({"value": "2026-02-30", "type": "date"})).is_err());
    assert_eq!(Scalar::date("1970-01-01").unwrap(), Scalar::Date(0));

    let interval: Scalar = serde_json::from_value(json!({"value": "2 weeks", "type": "interval"})).unwrap();
    assert_eq!(interval, Scalar::Interval { months: 0, days: 14 });
    assert_eq!(interval.to_string(), "INTERVAL '14 days'");

    let quoted = Scalar::Utf8("it's".into());
    assert_eq!(quoted.to_string(), "'it''s'");
    assert!(serde_json::from_value::<Scalar>(json!({"value": 1.5, "type": "int64"})).is_err());
}

#[test]
fn expressions_print_with_minimal_parens() {
    let e = Expr::binary(
        Expr::binary(col("a"), BinaryOp::Add, col("b")),
        BinaryOp::Mul,
        Expr::binary(col("c"), BinaryOp::Sub, lit(Scalar::Int64(1))),
    );
    assert_eq!(e.to_string(), "(a + b) * (c - 1)");
    let e = Expr::binary(
        col("x"),
        BinaryOp::And,
        Expr::binary(col("y"), BinaryOp::Or, col("z")),
    );
    assert_eq!(e.to_string(), "x AND (y OR z)");
    let e = Expr::binary(Expr::binary(col("a"), BinaryOp::Sub, col("b")), BinaryOp::Sub, col("c"));
    assert_eq!(e.to_string(), "a - b - c");
    let e = Expr::binary(col("a"), BinaryOp::Sub, Expr::binary(col("b"), BinaryOp::Sub, col("c")));
    assert_eq!(e.to_string(), "a - (b - c)");
}

#[test]
fn date_arithmetic_and_functions_typecheck() {
    let v = decode_steps(json!([
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "recent", "op": "filter", "input": "o", "predicate": {
            "kind": "binary", "op": ">=", "left": c("order_date"),
            "right": {"kind": "binary", "op": "-",
                      "left": {"kind": "function", "name": "current_date"},
                      "right": {"kind": "literal", "value": "30 days", "type": "interval"}}}},
        {"id": "p", "op": "project", "input": "recent", "exprs": [
            {"name": "customer_id", "expr": c("customer_id")},
            {"name": "month", "expr": {"kind": "function", "name": "month", "args": [c("order_date")]}},
            {"name": "cents", "expr": {"kind": "binary", "op": "*", "left": c("amount"), "right": {"kind": "literal", "value": 100}}}
        ]}
    ]))
    .unwrap();
    assert_eq!(v.schema.to_string(), "(customer_id: int64, month: int64, cents: float64)");

    assert_step_error(
        json!([{"id": "r", "op": "scan", "table": "reviews"},
               {"id": "f", "op": "filter", "input": "r",
                "predicate": {"kind": "function", "name": "contains", "args": [c("text")]}}]),
        "f",
        "`contains` expects (utf8, utf8), got (utf8)",
    );
}

#[test]
fn expressions_reject_unknown_fields() {
    assert_step_error(
        json!([{"id": "o", "op": "scan", "table": "orders"},
               {"id": "f", "op": "filter", "input": "o",
                "predicate": {"kind": "is_null", "expr": c("amount"), "negate": true}}]),
        "f",
        "unknown field `negate`",
    );
    let err = serde_json::from_value::<Expr>(json!({"kind": "literal", "value": 1, "typ": "int64"})).unwrap_err();
    assert!(err.to_string().contains("unknown field `typ`"), "{err}");
}

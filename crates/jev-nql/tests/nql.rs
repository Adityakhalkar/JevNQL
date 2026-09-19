use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use jev_executor::Session;
use jev_nql::compile;
use jev_optimizer::{PhysicalConfig, optimize, physical_plan};
use jev_provider::simulated::SimulatedBackend;
use jevir::{DataType, Field, Schema};

fn catalog() -> HashMap<String, Schema> {
    use DataType::*;
    let table = |fields: &[(&str, DataType)]| {
        Schema::new(fields.iter().map(|(n, t)| Field::new(*n, t.clone())).collect()).unwrap()
    };
    HashMap::from([
        ("customers".into(), table(&[("customer_id", Int64), ("name", Utf8), ("segment", Utf8)])),
        ("orders".into(), table(&[("order_id", Int64), ("customer_id", Int64), ("amount", Float64), ("order_date", Date)])),
        ("reviews".into(), table(&[("review_id", Int64), ("customer_id", Int64), ("rating", Int64), ("text", Utf8), ("created_at", Date)])),
    ])
}

const README_EXAMPLE: &str = r#"
FROM customers
WITH orders  AS spend   (SUM amount WHERE order_date >= DATE '2026-01-01')
WITH reviews AS history (LAST 30 BY created_at)
FIND customers WHO:
    spend > 1000
    AND "seem increasingly unhappy with our pricing"
SCORE leave_risk: "How likely is this customer to cancel?"
    LEVELS ("No sign of leaving", "Frustrated", "Clear intent to cancel")
RANK BY spend DESC LIMIT 20
RETURN customer_id, name, spend, leave_risk
"#;

fn err(src: &str) -> String {
    match compile(src, &catalog()) {
        Ok(_) => panic!("expected an error for:\n{src}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn readme_example_compiles_to_typed_plan() {
    let c = compile(README_EXAMPLE, &catalog()).unwrap();
    assert_eq!(c.plan.schema.to_string(), "(customer_id: int64, name: utf8, spend: float64, leave_risk: float64)");
    let text = c.plan.plan.to_string();
    let ops: Vec<&str> = text.lines().map(|l| l.trim_start_matches(|ch: char| " │├└─".contains(ch)).split('[').next().unwrap()).collect();
    assert_eq!(
        ops,
        ["Project", "TopK", "SemanticScore", "SemanticFilter", "Filter", "Fetch", "Join", "Scan", "Aggregate", "Filter", "Scan", "Scan"],
        "\n{text}"
    );
    // LAST 30 BY created_at keeps the 30 newest; judgments default to the history
    assert!(text.contains("Fetch[history <- customer_id.customer_id: review_id, rating, text, created_at | order created_at DESC | limit 30]"), "{text}");
    assert!(text.contains("SemanticFilter[p >= 0.5: \"seem increasingly unhappy with our pricing\" | context: history]"), "{text}");
    assert!(text.contains("Filter[order_date >= DATE '2026-01-01']"), "{text}");
}

#[test]
fn deterministic_query_has_no_semantic_steps() {
    let c = compile(
        "FROM customers WITH reviews AS review_count (COUNT) RANK BY review_count DESC, customer_id LIMIT 10 RETURN name, review_count",
        &catalog(),
    )
    .unwrap();
    assert!(!c.plan.plan.to_string().contains("Semantic"));
    assert_eq!(c.plan.schema.to_string(), "(name: utf8, review_count: int64)");
}

#[test]
fn expressions_follow_precedence_and_types() {
    let c = compile(
        "FROM orders WHERE (amount * 2 > 100 OR customer_id IN (1, 2)) AND order_date IS NOT NULL AND NOT year(order_date) = 2025 \
         RETURN order_id, amount / 2 AS half",
        &catalog(),
    )
    .unwrap();
    let text = c.plan.plan.to_string();
    assert!(
        text.contains("Filter[(amount * 2 > 100 OR customer_id IN (1, 2)) AND order_date IS NOT NULL AND NOT year(order_date) = 2025]"),
        "{text}"
    );
    assert_eq!(c.plan.schema.to_string(), "(order_id: int64, half: float64)");
}

#[test]
fn classify_and_explicit_context() {
    let c = compile(
        r#"FROM reviews WHERE "complains about price" USING text
           CLASSIFY topic: "What is this review mainly about?" INTO (pricing, quality, "support staff") USING text"#,
        &catalog(),
    )
    .unwrap();
    let text = c.plan.plan.to_string();
    assert!(text.contains("SemanticChoice[topic in {pricing, quality, support staff}"), "{text}");
    assert_eq!(c.plan.schema.field("topic").unwrap().data_type, DataType::Utf8);
}

#[test]
fn syntax_errors_point_at_the_query() {
    assert_eq!(err("FIND customers"), "line 1, column 1: expected FROM, found `FIND`");
    assert!(err("FROM customers WHERE segment = 'smb").contains("line 1, column 32: unterminated 'quoted' text"));
    assert!(err("FROM customers WHERE segment = 'smb' OR \"unhappy\"").contains("inside parentheses"));
    assert!(err("FROM customers LIMIT 5").contains("LIMIT needs an order"));
    assert!(err("FROM customers WHERE name = \"Asha\"").contains("line 1, column 29: double-quoted judgments can only be FIND conditions"));
    assert!(err("FROM customers\nRETURN nme").contains("step `return`: unknown column `nme`"));
    assert!(err("FROM customers WHERE shout(name)").contains("unknown function `shout`"));
    assert!(err("FROM customers RANK BY name RETURN name FROM x").contains("clauses go in the order"));
}

#[test]
fn judgments_need_readable_context() {
    assert!(err("FROM orders WHERE \"looks fraudulent\"").contains("needs text to read"));
}

#[test]
fn join_keys_are_inferred_or_explicit() {
    let mut cat = catalog();
    cat.insert(
        "visits".into(),
        Schema::new(vec![
            Field::new("customer_id", DataType::Int64),
            Field::new("name", DataType::Utf8),
            Field::new("page", DataType::Utf8),
        ])
        .unwrap(),
    );
    // shares customer_id and name: the only `*_id` column wins
    let c = compile("FROM customers WITH visits AS seen (COUNT) RETURN name, seen", &cat).unwrap();
    assert!(c.plan.plan.to_string().contains("Join[left: customer_id = customer_id]"));
    let c = compile("FROM customers WITH visits AS seen ON name (COUNT) RETURN name, seen", &cat).unwrap();
    assert!(c.plan.plan.to_string().contains("Join[left: name = name]"));
}

#[tokio::test]
async fn compiled_query_runs_end_to_end() {
    let mut session = Session::new().with_semantic_backend(Arc::new(SimulatedBackend::default()));
    let mini = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data/mini");
    for t in ["customers", "orders", "reviews"] {
        session.register_file(mini.join(format!("{t}.csv"))).await.unwrap();
    }
    let c = compile(
        r#"FROM customers
           WITH reviews AS history (LAST 10 BY created_at)
           FIND customers WHO: "complain about prices or cost"
           RANK BY customer_id
           RETURN customer_id, name"#,
        &session,
    )
    .unwrap();
    let optimized = optimize(&c.plan, &session).unwrap();
    let result = session.execute(&physical_plan(&optimized.plan, &PhysicalConfig::default())).await.unwrap();
    let names: Vec<String> = (0..result.num_rows())
        .map(|r| datafusion_display(&result, r, 1))
        .collect();
    // Asha ("Price went up again") and Ben ("Too expensive now", "pricey")
    assert_eq!(names, ["Asha", "Ben"]);
}

fn datafusion_display(result: &jev_executor::QueryResult, row: usize, col: usize) -> String {
    use datafusion::arrow::util::display::array_value_to_string;
    array_value_to_string(result.batches[0].column(col), row).unwrap()
}

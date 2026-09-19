use std::collections::HashMap;

use jev_nl::{Column, ColumnKind, Relation, Table, Vocabulary, translate};
use jevir::{DataType, Field, Schema};

const TODAY: (i32, u32, u32) = (2026, 9, 19);

fn col(name: &str, kind: ColumnKind, values: &[&str]) -> Column {
    Column { name: name.into(), kind, values: values.iter().map(|v| v.to_string()).collect() }
}

fn vocabulary() -> Vocabulary {
    use ColumnKind::*;
    let rel = |child: &str, p: [f64; 4]| Relation {
        parent: "customers".into(),
        child: child.into(),
        key: "customer_id".into(),
        count_percentiles: p,
    };
    Vocabulary {
        tables: vec![
            Table {
                name: "customers".into(),
                columns: vec![
                    col("customer_id", Number, &[]),
                    col("name", Text, &[]),
                    col("segment", Text, &["enterprise", "mid-market", "smb"]),
                    col("country", Text, &["India", "United States", "Germany"]),
                    col("signup_date", Date, &[]),
                ],
            },
            Table {
                name: "orders".into(),
                columns: vec![col("order_id", Number, &[]), col("customer_id", Number, &[]), col("amount", Number, &[]), col("order_date", Date, &[])],
            },
            Table {
                name: "reviews".into(),
                columns: vec![
                    col("review_id", Number, &[]),
                    col("customer_id", Number, &[]),
                    col("rating", Number, &[]),
                    col("text", Text, &[]),
                    col("created_at", Date, &[]),
                ],
            },
            Table {
                name: "tickets".into(),
                columns: vec![
                    col("ticket_id", Number, &[]),
                    col("customer_id", Number, &[]),
                    col("created_at", Date, &[]),
                    col("subject", Text, &[]),
                    col("body", Text, &[]),
                    col("status", Text, &["open", "closed"]),
                    col("priority", Text, &["low", "normal", "high"]),
                ],
            },
        ],
        relations: vec![rel("orders", [20.0, 31.0, 57.2, 90.0]), rel("reviews", [2.0, 3.0, 6.0, 10.0]), rel("tickets", [1.0, 1.0, 2.0, 4.0])],
    }
}

/// The same schema as a JevIR catalog, so translations can be compiled.
fn catalog() -> HashMap<String, Schema> {
    vocabulary()
        .tables
        .iter()
        .map(|t| {
            let fields = t
                .columns
                .iter()
                .map(|c| {
                    let ty = match c.kind {
                        ColumnKind::Text => DataType::Utf8,
                        ColumnKind::Date => DataType::Date,
                        ColumnKind::Number if c.name == "amount" => DataType::Float64,
                        _ => DataType::Int64,
                    };
                    Field::new(&c.name, ty)
                })
                .collect();
            (t.name.clone(), Schema::new(fields).unwrap())
        })
        .collect()
}

/// Translates, checks the NQL compiles, and returns it.
fn nql(question: &str) -> String {
    let t = translate(question, &vocabulary(), TODAY).unwrap_or_else(|e| panic!("{question}: {e}"));
    if let Err(e) = jev_nql::compile(&t.nql, &catalog()) {
        panic!("{question}\ncompiled to invalid NQL ({e}):\n{}", t.nql);
    }
    t.nql
}

fn has(nql: &str, parts: &[&str]) {
    for p in parts {
        assert!(nql.contains(p), "missing `{p}` in:\n{nql}");
    }
}

#[test]
fn enterprise_customers_with_many_orders_who_sound_like_leaving() {
    let q = nql("enterprise customers with many orders who sound like they're leaving");
    has(
        &q,
        &[
            "FROM customers",
            "WITH orders AS order_count (COUNT)",
            "WITH reviews AS review_history (LAST 20 BY created_at FIELDS (rating, text, created_at))",
            "WITH tickets AS ticket_history (LAST 20 BY created_at FIELDS (created_at, subject, body, status, priority))",
            "segment = 'enterprise'",
            "order_count >= 58",
            "\"Based on their history, does this customer sound like they're leaving?\"",
        ],
    );
    let t = translate("enterprise customers with many orders who sound like they're leaving", &vocabulary(), TODAY).unwrap();
    assert!(t.notes.iter().any(|n| n.contains("\"many orders\" → at least 58 (top 25% of customers by order count)")), "{:#?}", t.notes);
}

#[test]
fn values_and_explicit_quantities() {
    has(&nql("customers from India with more than 10 reviews"), &["country = 'India'", "review_count > 10"]);
    has(
        &nql("customers in the United States who placed at least 5 orders in 2025"),
        &[
            "country = 'United States'",
            "order_count >= 5",
            "WITH orders AS order_count (COUNT WHERE order_date >= DATE '2025-01-01' AND order_date < DATE '2026-01-01')",
        ],
    );
    has(&nql("non-enterprise customers with no tickets"), &["segment != 'enterprise'", "ticket_count IS NULL"]);
    let q = nql("customers from India with more than 10 reviews");
    assert!(!q.contains('"'), "no judgment expected:\n{q}");
}

#[test]
fn rankings() {
    has(&nql("top 10 customers with the most reviews"), &["RANK BY review_count DESC, customer_id LIMIT 10"]);
    has(
        &nql("biggest spenders this year who seem unhappy about pricing"),
        &[
            "FROM customers",
            "WITH orders AS spend (SUM amount WHERE order_date >= DATE '2026-01-01')",
            "RANK BY spend DESC, customer_id LIMIT 20",
            "\"Based on their history, does this customer seem unhappy about pricing?\"",
        ],
    );
}

#[test]
fn judgments_on_the_rows_themselves() {
    has(&nql("open tickets that sound urgent"), &["FROM tickets", "status = 'open'", "\"does this ticket sound urgent?\""]);
    has(
        &nql("reviews from the last 30 days that complain about price"),
        &["created_at >= current_date() - INTERVAL '30 days'", "\"does this review complain about price?\""],
    );
    has(&nql("which customers are frustrated"), &["\"Based on their history, is this customer frustrated?\""]);
}

#[test]
fn column_names_and_judgment_phrases_stay_whole() {
    let q = nql("high priority open tickets that sound angry");
    has(&q, &["priority = 'high'", "status = 'open'", "\"does this ticket sound angry?\""]);
    assert_eq!(q.matches('"').count(), 2, "one judgment only:\n{q}");
    let q = nql("customers with many tickets who seem happy with the product");
    has(&q, &["ticket_count >= 2", "\"Based on their history, does this customer seem happy with the product?\""]);
    assert_eq!(q.matches('"').count(), 2, "one judgment only:\n{q}");
}

#[test]
fn numeric_facts_never_become_judgments() {
    // the question that went wrong: money and ratings are columns, not judgments
    let q = nql("customers that paid more than $20 and have rated app more than 3 stars");
    has(
        &q,
        &[
            "WITH orders AS spend (SUM amount)",
            "WITH reviews AS avg_rating (AVG rating)",
            "spend > 20",
            "avg_rating > 3",
        ],
    );
    assert!(!q.contains('"'), "no judgment expected:\n{q}");
    assert!(!q.contains("RANK BY spend"), "a comparison is a filter, not a ranking:\n{q}");
    has(&nql("customers who spent at least 1000.50 this year"), &["spend >= 1000.50", "order_date >= DATE '2026-01-01'"]);
    has(&nql("highest rated customers"), &["RANK BY avg_rating DESC, customer_id LIMIT 20"]);
    has(&nql("reviews with rating under 2"), &["FROM reviews", "rating < 2"]);
}

#[test]
fn unplaceable_comparisons_are_errors() {
    let err = translate("customers with loyalty over 5", &vocabulary(), TODAY).unwrap_err();
    assert!(err.0.contains("couldn't tell which column \"over 5\""), "{err}");
}

#[test]
fn unknown_subject_is_reported() {
    let err = translate("hello world", &vocabulary(), TODAY);
    assert!(err.is_err_and(|e| e.0.contains("customers, orders, reviews, tickets")));
}

use std::path::Path;

use datafusion::arrow::util::display::array_value_to_string;
use datafusion::dataframe::DataFrameWriteOptions;
use datafusion::prelude::{CsvReadOptions, SessionContext};
use jev_executor::{ExecError, QueryResult, Session};
use jevir::DataType;
use serde_json::{Value, json};
use tempfile::TempDir;

const CUSTOMERS: &str = "customer_id,name,segment
1,Asha,enterprise
2,Ben,smb
3,Chen,enterprise
4,Dia,smb
";

const ORDERS: &str = "order_id,customer_id,amount,order_date
101,1,500.0,2026-02-01
102,1,300.0,2025-12-20
103,2,900.0,2026-03-05
104,3,50.0,2026-01-10
105,3,75.5,2026-04-01
106,2,100.0,2026-05-01
107,4,20.0,2025-06-01
";

const REVIEWS: &str = "review_id,customer_id,rating,text,created_at
1,1,4,Great product,2026-01-05
2,1,2,\"Price went up again, not happy\",2026-03-01
3,2,5,Love it,2026-02-02
4,2,1,Too expensive now,2026-04-10
5,2,3,Okay but pricey,2026-03-15
6,3,5,Excellent,2026-01-20
";

const EXAMPLE: &str = include_str!("../../../examples/plans/high_value_unhappy.json");

async fn session() -> (Session, TempDir) {
    let dir = TempDir::new().unwrap();
    let mut session = Session::new();
    for (name, data) in [("customers", CUSTOMERS), ("orders", ORDERS), ("reviews", REVIEWS)] {
        let path = dir.path().join(format!("{name}.csv"));
        std::fs::write(&path, data).unwrap();
        assert_eq!(session.register_file(&path).await.unwrap(), name);
    }
    (session, dir)
}

async fn run(session: &Session, steps: Value) -> Result<QueryResult, ExecError> {
    let plan = jevir::decode(&json!({"version": 1, "steps": steps}).to_string(), session)?;
    session.execute(&plan).await
}

fn rows(result: &QueryResult) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for batch in &result.batches {
        for r in 0..batch.num_rows() {
            out.push(batch.columns().iter().map(|c| array_value_to_string(c, r).unwrap()).collect());
        }
    }
    out
}

fn c(name: &str) -> Value {
    json!({"kind": "column", "name": name})
}

fn bin(op: &str, left: Value, right: Value) -> Value {
    json!({"kind": "binary", "op": op, "left": left, "right": right})
}

fn l(value: Value) -> Value {
    json!({"kind": "literal", "value": value})
}

fn date(iso: &str) -> Value {
    json!({"kind": "literal", "value": iso, "type": "date"})
}

#[tokio::test]
async fn catalog_maps_csv_types() {
    let (s, _dir) = session().await;
    let orders = jevir::Catalog::table_schema(&s, "orders").unwrap();
    assert_eq!(orders.to_string(), "(order_id: int64, customer_id: int64, amount: float64, order_date: date)");
}

#[tokio::test]
async fn top_spenders_with_fetched_history() {
    let (s, _dir) = session().await;
    let result = run(
        &s,
        json!([
            {"id": "o", "op": "scan", "table": "orders"},
            {"id": "y", "op": "filter", "input": "o", "predicate": bin(">=", c("order_date"), date("2026-01-01"))},
            {"id": "a", "op": "aggregate", "input": "y", "group_by": ["customer_id"],
             "aggregates": [{"func": "sum", "arg": c("amount"), "output": "total_spend"}]},
            {"id": "t", "op": "top_k", "input": "a", "k": 2, "keys": [{"expr": c("total_spend"), "descending": true}]},
            {"id": "r", "op": "scan", "table": "reviews"},
            {"id": "h", "op": "fetch", "input": "t", "source": "r",
             "on": {"left": "customer_id", "right": "customer_id"}, "fields": ["created_at", "text"],
             "order_by": [{"expr": c("created_at")}], "limit": 2, "output": "history"}
        ]),
    )
    .await
    .unwrap();

    let rows = rows(&result);
    assert_eq!(rows.len(), 2);
    // TopK order survives the fetch's hash join
    assert_eq!(rows[0][..2], ["2", "1000.0"]);
    assert_eq!(rows[1][..2], ["1", "500.0"]);
    // history is ordered by created_at and truncated to 2 entries
    let history = &rows[0][2];
    assert!(history.find("Love it").unwrap() < history.find("Okay but pricey").unwrap(), "{history}");
    assert!(!history.contains("Too expensive now"), "{history}");
    assert!(matches!(result.schema.field("history").unwrap().data_type, DataType::List { .. }));
}

#[tokio::test]
async fn semantic_operators_are_not_yet_executable() {
    let (s, _dir) = session().await;
    let plan = jevir::decode(EXAMPLE, &s).unwrap();
    let err = s.execute(&plan).await.unwrap_err();
    assert!(matches!(err, ExecError::Unsupported(ref m) if m.contains("SemanticScore")), "{err}");
}

#[tokio::test]
async fn join_project_and_functions() {
    let (s, _dir) = session().await;
    let result = run(
        &s,
        json!([
            {"id": "c", "op": "scan", "table": "customers"},
            {"id": "o", "op": "scan", "table": "orders"},
            {"id": "j", "op": "join", "left": "c", "right": "o", "on": [{"left": "customer_id", "right": "customer_id"}]},
            {"id": "f", "op": "filter", "input": "j", "predicate": bin("and",
                bin("=", c("segment"), l(json!("enterprise"))),
                bin("=", json!({"kind": "function", "name": "year", "args": [c("order_date")]}), l(json!(2026))))},
            {"id": "s", "op": "sort", "input": "f", "keys": [{"expr": c("amount"), "descending": true}]},
            {"id": "p", "op": "project", "input": "s", "exprs": [
                {"name": "who", "expr": {"kind": "function", "name": "upper", "args": [c("name")]}},
                {"name": "amount", "expr": c("amount")},
                {"name": "half", "expr": bin("/", c("order_id"), l(json!(2)))}
            ]}
        ]),
    )
    .await
    .unwrap();
    assert_eq!(result.schema.to_string(), "(who: utf8, amount: float64, half: float64)");
    assert_eq!(
        rows(&result),
        [["ASHA", "500.0", "50.5"], ["CHEN", "75.5", "52.5"], ["CHEN", "50.0", "52.0"]]
    );
}

#[tokio::test]
async fn aggregates_match_inferred_types() {
    let (s, _dir) = session().await;
    let result = run(
        &s,
        json!([
            {"id": "c", "op": "scan", "table": "customers"},
            {"id": "o", "op": "scan", "table": "orders"},
            {"id": "j", "op": "join", "left": "c", "right": "o", "on": [{"left": "customer_id", "right": "customer_id"}]},
            {"id": "a", "op": "aggregate", "input": "j", "group_by": ["segment"], "aggregates": [
                {"func": "count", "output": "orders"},
                {"func": "count_distinct", "arg": c("customer_id"), "output": "customers"},
                {"func": "sum", "arg": c("order_id"), "output": "id_sum"},
                {"func": "avg", "arg": c("amount"), "output": "avg_amount"},
                {"func": "min", "arg": c("order_date"), "output": "first"},
                {"func": "max", "arg": c("name"), "output": "last_name"}
            ]},
            {"id": "s", "op": "sort", "input": "a", "keys": [{"expr": c("segment")}]}
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        rows(&result),
        [
            ["enterprise", "4", "2", "412", "231.375", "2025-12-20", "Chen"],
            ["smb", "3", "2", "316", "340.0", "2025-06-01", "Dia"],
        ]
    );
}

#[tokio::test]
async fn filter_expression_features() {
    let (s, _dir) = session().await;
    let lower_text = json!({"kind": "function", "name": "lower", "args": [c("text")]});
    let result = run(
        &s,
        json!([
            {"id": "r", "op": "scan", "table": "reviews"},
            {"id": "f", "op": "filter", "input": "r", "predicate": bin("or",
                json!({"kind": "function", "name": "contains", "args": [lower_text, l(json!("price"))]}),
                json!({"kind": "in_list", "expr": c("rating"), "list": [l(json!(1)), l(json!(2))]}))},
            {"id": "n", "op": "filter", "input": "f", "predicate": {"kind": "unary", "op": "not",
                "expr": {"kind": "is_null", "expr": c("text")}}},
            {"id": "a", "op": "aggregate", "input": "n", "aggregates": [{"func": "count", "output": "n"}]}
        ]),
    )
    .await
    .unwrap();
    // reviews 2 (price + rating 2), 4 (rating 1), 5 (pricey)
    assert_eq!(rows(&result), [["3"]]);
}

#[tokio::test]
async fn reads_parquet() {
    let dir = TempDir::new().unwrap();
    let csv = dir.path().join("orders.csv");
    std::fs::write(&csv, ORDERS).unwrap();
    let parquet = dir.path().join("Orders 2026.parquet");
    write_parquet(&csv, &parquet).await;

    let mut s = Session::new();
    assert_eq!(s.register_file(&parquet).await.unwrap(), "orders_2026");
    let result = run(
        &s,
        json!([
            {"id": "o", "op": "scan", "table": "orders_2026", "columns": ["customer_id", "amount"]},
            {"id": "a", "op": "aggregate", "input": "o", "group_by": ["customer_id"],
             "aggregates": [{"func": "max", "arg": c("amount"), "output": "biggest"}]},
            {"id": "t", "op": "top_k", "input": "a", "k": 1, "keys": [{"expr": c("biggest"), "descending": true}]}
        ]),
    )
    .await
    .unwrap();
    assert_eq!(rows(&result), [["2", "900.0"]]);
}

async fn write_parquet(csv: &Path, parquet: &Path) {
    SessionContext::new()
        .read_csv(csv.to_str().unwrap(), CsvReadOptions::new())
        .await
        .unwrap()
        .write_parquet(parquet.to_str().unwrap(), DataFrameWriteOptions::new().with_single_file_output(true), None)
        .await
        .unwrap();
}

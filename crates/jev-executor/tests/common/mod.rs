#![allow(dead_code)]

use datafusion::arrow::util::display::array_value_to_string;
use jev_executor::{ExecError, QueryResult, Session};
use jev_optimizer::{PhysicalConfig, physical_plan};
use serde_json::{Value, json};
use tempfile::TempDir;

pub const CUSTOMERS: &str = "customer_id,name,segment
1,Asha,enterprise
2,Ben,smb
3,Chen,enterprise
4,Dia,smb
";

pub const ORDERS: &str = "order_id,customer_id,amount,order_date
101,1,500.0,2026-02-01
102,1,300.0,2025-12-20
103,2,900.0,2026-03-05
104,3,50.0,2026-01-10
105,3,75.5,2026-04-01
106,2,100.0,2026-05-01
107,4,20.0,2025-06-01
";

pub const REVIEWS: &str = "review_id,customer_id,rating,text,created_at
1,1,4,Great product,2026-01-05
2,1,2,\"Price went up again, not happy\",2026-03-01
3,2,5,Love it,2026-02-02
4,2,1,Too expensive now,2026-04-10
5,2,3,Okay but pricey,2026-03-15
6,3,5,Excellent,2026-01-20
";

pub const EXAMPLE: &str = include_str!("../../../../examples/plans/high_value_unhappy.json");

pub async fn session() -> (Session, TempDir) {
    session_with(Session::new()).await
}

pub async fn session_with(mut session: Session) -> (Session, TempDir) {
    let dir = TempDir::new().unwrap();
    for (name, data) in [("customers", CUSTOMERS), ("orders", ORDERS), ("reviews", REVIEWS)] {
        let path = dir.path().join(format!("{name}.csv"));
        std::fs::write(&path, data).unwrap();
        assert_eq!(session.register_file(&path).await.unwrap(), name);
    }
    (session, dir)
}

pub async fn run(session: &Session, steps: Value) -> Result<QueryResult, ExecError> {
    run_json(session, &json!({"version": 1, "steps": steps}).to_string()).await
}

pub async fn run_json(session: &Session, doc: &str) -> Result<QueryResult, ExecError> {
    let plan = jevir::decode(doc, session)?;
    session.execute(&physical_plan(&plan, &PhysicalConfig::default())).await
}

pub fn rows(result: &QueryResult) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for batch in &result.batches {
        for r in 0..batch.num_rows() {
            out.push(batch.columns().iter().map(|c| array_value_to_string(c, r).unwrap()).collect());
        }
    }
    out
}

pub fn c(name: &str) -> Value {
    json!({"kind": "column", "name": name})
}

pub fn bin(op: &str, left: Value, right: Value) -> Value {
    json!({"kind": "binary", "op": op, "left": left, "right": right})
}

pub fn l(value: Value) -> Value {
    json!({"kind": "literal", "value": value})
}

pub fn date(iso: &str) -> Value {
    json!({"kind": "literal", "value": iso, "type": "date"})
}


//! Table profiles: what a frontend compiler needs to know about the data.

use datafusion::arrow::array::{Array, RecordBatch};
use datafusion::arrow::util::display::array_value_to_string;
use datafusion::common::Column;
use datafusion::functions_aggregate::expr_fn::{max, min};
use datafusion::prelude::Expr;
use jevir::DataType;
use serde::Serialize;

use crate::error::ExecError;
use crate::session::Session;

const EXAMPLES: usize = 5;
const EXAMPLE_CHARS: usize = 80;

#[derive(Debug, Clone, Serialize)]
pub struct TableProfile {
    pub name: String,
    pub rows: usize,
    pub columns: Vec<ColumnProfile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnProfile {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    /// A few distinct values (text columns), truncated.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

fn column(name: &str) -> Expr {
    Expr::Column(Column::new_unqualified(name))
}

fn values(batches: &[RecordBatch], col: usize) -> Result<Vec<String>, ExecError> {
    let mut out = Vec::new();
    for batch in batches {
        let array = batch.column(col);
        for row in 0..batch.num_rows() {
            if !array.is_null(row) {
                out.push(array_value_to_string(array, row)?);
            }
        }
    }
    Ok(out)
}

impl Session {
    /// Row count, value ranges and example values for one registered table.
    pub async fn profile(&self, table: &str) -> Result<TableProfile, ExecError> {
        let schema = jevir::Catalog::table_schema(self, table)
            .ok_or_else(|| ExecError::Register { path: table.into(), reason: "no such table".into() })?;
        let df = self.context().table(table).await?;
        let rows = df.clone().count().await?;

        let mut columns: Vec<ColumnProfile> = schema
            .fields()
            .iter()
            .map(|f| ColumnProfile {
                name: f.name.clone(),
                data_type: f.data_type.to_string(),
                min: None,
                max: None,
                examples: Vec::new(),
            })
            .collect();

        // one aggregate pass for the ranges of numeric and temporal columns
        let ranged: Vec<usize> = (0..columns.len())
            .filter(|&i| {
                let t = &schema.fields()[i].data_type;
                t.is_numeric() || t.is_temporal()
            })
            .collect();
        if !ranged.is_empty() {
            let aggs = ranged
                .iter()
                .flat_map(|&i| {
                    let name = &columns[i].name;
                    [min(column(name)).alias(format!("min{i}")), max(column(name)).alias(format!("max{i}"))]
                })
                .collect();
            let batches = df.clone().aggregate(vec![], aggs)?.collect().await?;
            for (k, &i) in ranged.iter().enumerate() {
                columns[i].min = values(&batches, 2 * k)?.pop();
                columns[i].max = values(&batches, 2 * k + 1)?.pop();
            }
        }

        for (i, field) in schema.fields().iter().enumerate() {
            if field.data_type != DataType::Utf8 {
                continue;
            }
            let batches = df.clone().select(vec![column(&field.name)])?.distinct()?.limit(0, Some(EXAMPLES))?.collect().await?;
            columns[i].examples = values(&batches, 0)?
                .into_iter()
                .map(|v| match v.char_indices().nth(EXAMPLE_CHARS) {
                    Some((cut, _)) => format!("{}…", &v[..cut]),
                    None => v,
                })
                .collect();
        }
        Ok(TableProfile { name: table.to_string(), rows, columns })
    }
}

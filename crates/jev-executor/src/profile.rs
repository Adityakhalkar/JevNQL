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

/// Distinct values of text columns with at most this many values are listed
/// (used to recognize values such as `enterprise` in questions).
pub const MAX_LISTED_VALUES: usize = 50;

impl Session {
    /// For each low-cardinality text column: its distinct values.
    pub async fn listed_values(&self, table: &str) -> Result<Vec<(String, Vec<String>)>, ExecError> {
        let schema = jevir::Catalog::table_schema(self, table)
            .ok_or_else(|| ExecError::Register { path: table.into(), reason: "no such table".into() })?;
        let df = self.context().table(table).await?;
        let mut out = Vec::new();
        for field in schema.fields().iter().filter(|f| f.data_type == DataType::Utf8) {
            let batches = df
                .clone()
                .select(vec![column(&field.name)])?
                .distinct()?
                .limit(0, Some(MAX_LISTED_VALUES + 1))?
                .collect()
                .await?;
            let values = values(&batches, 0)?;
            if values.len() <= MAX_LISTED_VALUES {
                out.push((field.name.clone(), values));
            }
        }
        Ok(out)
    }

    /// Whether `column` has a distinct value in every row of `table`.
    pub async fn is_unique(&self, table: &str, column_name: &str) -> Result<bool, ExecError> {
        use datafusion::functions_aggregate::expr_fn::count_distinct;
        let df = self.context().table(table).await?;
        let rows = df.clone().count().await?;
        let batches = df.aggregate(vec![], vec![count_distinct(column(column_name)).alias("n")])?.collect().await?;
        Ok(values(&batches, 0)?.pop().and_then(|v| v.parse::<usize>().ok()) == Some(rows))
    }

    /// Percentiles (p25, p50, p75, p90) of the number of `table` rows per
    /// `key` value, over keys that have at least one row.
    pub async fn count_percentiles(&self, table: &str, key: &str) -> Result<[f64; 4], ExecError> {
        use datafusion::functions_aggregate::count::count_all;
        use datafusion::functions_aggregate::expr_fn::approx_percentile_cont;
        use datafusion::prelude::lit;
        let counts = self.context().table(table).await?.aggregate(vec![column(key)], vec![count_all().alias("n")])?;
        let pct = |p: f64| approx_percentile_cont(column("n").sort(true, false), lit(p), None).alias(format!("p{p}"));
        let batches = counts.aggregate(vec![], vec![pct(0.25), pct(0.5), pct(0.75), pct(0.9)])?.collect().await?;
        let mut out = [0.0; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = values(&batches, i)?.pop().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        }
        Ok(out)
    }
}

impl Session {
    /// Percentiles (p25, p50, p75, p90) of the per-`key` total of `value`.
    pub async fn sum_percentiles(&self, table: &str, key: &str, value: &str) -> Result<[f64; 4], ExecError> {
        use datafusion::functions_aggregate::expr_fn::{approx_percentile_cont, sum};
        use datafusion::prelude::lit;
        let totals = self.context().table(table).await?.aggregate(vec![column(key)], vec![sum(column(value)).alias("t")])?;
        let pct = |p: f64| approx_percentile_cont(column("t").sort(true, false), lit(p), None).alias(format!("p{p}"));
        let batches = totals.aggregate(vec![], vec![pct(0.25), pct(0.5), pct(0.75), pct(0.9)])?.collect().await?;
        let mut out = [0.0; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = values(&batches, i)?.pop().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        }
        Ok(out)
    }
}

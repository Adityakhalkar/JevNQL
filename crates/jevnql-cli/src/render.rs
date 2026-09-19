//! Human-readable EXPLAIN, result tables and metrics.

use std::fmt::Write;

use jevir::physical::PhysicalPlan;
use jevnql_core::{ExecError, ExecMetrics, Prepared, QueryResult};

const MAX_CELL: usize = 60;

pub fn explain(p: &Prepared) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "LOGICAL PLAN\n\n{}", p.logical.plan);
    let _ = writeln!(s, "OPTIMIZER\n");
    if p.rules.is_empty() {
        let _ = writeln!(s, "  (no rewrites)");
    }
    for rule in &p.rules {
        let _ = writeln!(s, "  - {rule}");
    }
    let _ = writeln!(s, "\nPHYSICAL PLAN\n\n{}", p.physical);
    s
}

/// Every cell as display text.
pub fn cells(result: &QueryResult) -> Result<Vec<Vec<String>>, ExecError> {
    use datafusion_display::array_value_to_string;
    let mut rows = Vec::new();
    for batch in &result.batches {
        for r in 0..batch.num_rows() {
            let row = batch.columns().iter().map(|c| array_value_to_string(c, r)).collect::<Result<_, _>>()?;
            rows.push(row);
        }
    }
    Ok(rows)
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_CELL) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_string(),
    }
}

pub fn table(result: &QueryResult, max_rows: usize) -> Result<String, ExecError> {
    let header: Vec<String> = result.schema.names().iter().map(|n| n.to_string()).collect();
    let rows: Vec<Vec<String>> = cells(result)?.into_iter().map(|r| r.iter().map(|c| clip(c)).collect()).collect();
    let shown = &rows[..rows.len().min(max_rows)];
    let widths: Vec<usize> = (0..header.len())
        .map(|i| shown.iter().map(|r| r[i].chars().count()).chain([header[i].chars().count()]).max().unwrap_or(0))
        .collect();
    let line = |cols: &[String]| {
        let padded: Vec<String> = cols.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        format!("| {} |\n", padded.join(" | "))
    };
    let rule = format!("+{}+\n", widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+"));
    let mut s = rule.clone() + &line(&header) + &rule;
    for row in shown {
        s += &line(row);
    }
    s += &rule;
    let _ = match rows.len() > shown.len() {
        true => writeln!(s, "{} rows ({} shown)", rows.len(), shown.len()),
        false => writeln!(s, "{} row{}", rows.len(), if rows.len() == 1 { "" } else { "s" }),
    };
    Ok(s)
}

pub fn metrics(m: &ExecMetrics, prepared: &Prepared, backend: Option<&str>) -> String {
    let simulated = backend == Some("simulated");
    let path: Vec<&str> = prepared
        .engine_path()
        .into_iter()
        .map(|e| if e == "Jev" && simulated { "Jev(simulated)" } else { e })
        .collect();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    let mut s = String::new();
    let mut row = |label: &str, value: String| {
        let _ = writeln!(s, "  {label:<28}{value}");
    };
    row("Engine", path.join(" → "));
    let uses_semantic = matches!(prepared.physical.root, PhysicalPlan::JevBatch(_)) || path.contains(&"Jev") || path.contains(&"Jev(simulated)");
    if uses_semantic {
        row(
            "Semantic backend",
            match backend {
                Some("simulated") => "simulated (offline stand-in, not Jev; set TYPESAFE_API_KEY)".into(),
                Some(name) => name.into(),
                None => "none".into(),
            },
        );
    }
    row("Rows scanned", thousands(m.rows_scanned as u64));
    row("Rows to semantic operators", thousands(m.semantic_rows as u64));
    row("Distinct states judged", thousands(m.distinct_states as u64));
    row("Jev batches / requests", format!("{} / {}", m.semantic_batches, thousands(m.requests as u64)));
    row("Questions asked", thousands(m.questions as u64));
    row("Input tokens", thousands(m.input_tokens));
    row("Cache hits", thousands(m.cache_hits as u64));
    row("Execution time", format!("{:.1} ms (semantic {:.1} ms)", ms(m.total_time), ms(m.semantic_time)));
    row("Estimated semantic cost", format!("${:.6}", m.estimated_cost_usd));
    s
}

/// `1234567` -> `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Arrow cell formatting, re-exported through the executor's DataFusion.
mod datafusion_display {
    use datafusion::arrow::array::ArrayRef;
    use datafusion::arrow::util::display;
    use jevnql_core::ExecError;

    pub fn array_value_to_string(array: &ArrayRef, row: usize) -> Result<String, ExecError> {
        Ok(display::array_value_to_string(array, row)?)
    }
}

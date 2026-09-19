//! Terminal UI for interactive use: interpretation, plan summary, live Jev
//! progress, result tables and a one-line metrics footer. Used only when
//! writing to a terminal; piped output stays plain (see `render`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use console::{Style, Term, measure_text_width, style, truncate_str};
use indicatif::{ProgressBar, ProgressStyle};
use jevnql_core::{ExecMetrics, Prepared, Progress, ProgressHook, QueryResult};

use crate::ask::{AskError, Asked};
use crate::render;

const MAX_CELL: usize = 36;

pub struct Ui {
    width: usize,
    pub verbose: bool,
    activity: Arc<Mutex<Option<ProgressBar>>>,
}

fn dim(s: impl std::fmt::Display) -> String {
    style(s).dim().to_string()
}

fn bullet(label: &str) -> String {
    format!("{} {}", style("●").cyan(), style(label).bold())
}

impl Ui {
    pub fn new() -> Self {
        let (_, cols) = Term::stdout().size();
        Self { width: (cols as usize).clamp(60, 160), verbose: false, activity: Arc::new(Mutex::new(None)) }
    }

    pub fn banner(&self, tables: &[(String, usize)], backend: &str) {
        let inner = self.width.min(84) - 4;
        let line = |text: String| {
            let pad = inner.saturating_sub(measure_text_width(&text));
            println!("{} {text}{} {}", dim("│"), " ".repeat(pad), dim("│"));
        };
        println!("{}{}{}", dim("╭─ "), style("JevNQL").cyan().bold(), dim(format!(" {}╮", "─".repeat(inner - 6))));
        line("Ask about your data in plain English, or write NQL.".to_string());
        let names: Vec<String> = tables.iter().map(|(n, rows)| format!("{} {}", style(n).bold(), dim(format!("{}", compact(*rows))))).collect();
        line(names.join(dim("  ·  ").as_str()));
        let jev = match backend {
            "simulated" => style("simulated (offline)").yellow().to_string(),
            name => style(format!("live · {name}")).green().to_string(),
        };
        line(format!("{} {jev}", dim("judgments:")));
        line(dim("EXPLAIN <question> · \\verbose · \\tables · \\clear · \\help · \\q"));
        println!("{}", dim(format!("╰{}╯", "─".repeat(inner + 2))));
    }

    pub fn tables(&self, tables: &[(String, Vec<(String, String)>)]) {
        for (name, cols) in tables {
            let cols: Vec<String> = cols.iter().map(|(c, t)| format!("{c} {}", dim(t))).collect();
            println!("  {}  {}", style(name).bold(), cols.join(dim(", ").as_str()));
        }
    }

    pub fn understood(&self, asked: &Asked) {
        let Some(notes) = &asked.notes else { return };
        println!("{}", bullet("Understood"));
        let rows: Vec<(String, String)> = notes
            .iter()
            .filter_map(|n| n.split_once(" → ").map(|(l, r)| (l.trim().trim_matches('"').to_string(), r.to_string())))
            .collect();
        let left = rows.iter().map(|(l, _)| measure_text_width(l)).max().unwrap_or(0).min(34);
        for note in notes {
            match note.split_once(" → ") {
                Some((l, r)) => {
                    let l = truncate_str(l.trim().trim_matches('"'), left, "…");
                    let pad = left.saturating_sub(measure_text_width(&l));
                    let (r, tag) = match r.split_once("  [Jev ") {
                        Some((body, tag)) => (body, format!("  {}", style(format!("Jev chose · {}", tag.trim_end_matches(']'))).magenta().dim())),
                        None => (r, String::new()),
                    };
                    let r = match r.strip_prefix("judgment for Jev: ") {
                        Some(q) => format!("{} {}", style("Jev judges").magenta().bold(), style(q).italic()),
                        None => r.to_string(),
                    };
                    let r = format!("{r}{tag}");
                    println!("  {}{}  {r}", style(l).yellow(), " ".repeat(pad));
                }
                None => println!("  {}", dim(note.trim())),
            }
        }
        if self.verbose {
            println!();
            for line in asked.nql.lines() {
                println!("  {}", dim(line));
            }
        }
        println!();
    }

    pub fn plan(&self, prepared: &Prepared, backend: Option<&str>) {
        let path: Vec<String> = prepared
            .engine_path()
            .into_iter()
            .map(|e| match (e, backend) {
                ("Jev", Some("simulated")) => style("Jev(sim)").yellow().to_string(),
                ("Jev", _) => style("Jev").magenta().to_string(),
                (e, _) => style(e).blue().to_string(),
            })
            .collect();
        let rewrites = prepared.rules.len();
        println!(
            "{}  {}  {}",
            bullet("Plan"),
            path.join(dim(" → ").as_str()),
            dim(format!("· {rewrites} optimizer rewrite{}", if rewrites == 1 { "" } else { "s" }))
        );
        if self.verbose {
            println!();
            for line in render::explain(prepared).lines() {
                println!("  {line}");
            }
        }
    }

    /// Drives the live activity line. For live backends, a semantic batch of
    /// more than `CONFIRM_REQUESTS` requests waits for confirmation first.
    pub fn progress_hook(&self, live: bool, usd_per_million_tokens: f64) -> ProgressHook {
        let slot = self.activity.clone();
        let can_ask = live && std::io::IsTerminal::is_terminal(&std::io::stdin());
        Arc::new(move |event: &Progress| {
            let guard = slot.lock().expect("activity lock");
            let Some(bar) = guard.as_ref() else { return true };
            match event {
                Progress::SemanticStart { rows, requests, .. } if can_ask && *requests > CONFIRM_REQUESTS => {
                    let tokens = *requests as f64 * TOKENS_PER_REQUEST;
                    let question = format!(
                        "{} Jev will judge {} rows: {} requests, ~{:.1}M tokens, ~${:.3}, ~{}. Continue? [Y/n] ",
                        style("?").yellow().bold(),
                        compact(*rows),
                        compact(*requests),
                        tokens / 1e6,
                        tokens * usd_per_million_tokens / 1e6,
                        duration(*requests as f64 / REQUESTS_PER_SECOND),
                    );
                    let yes = bar.suspend(|| {
                        print!("{question}");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                        let mut answer = String::new();
                        // no answer (end of input, read error) never spends money
                        match std::io::stdin().read_line(&mut answer) {
                            Ok(0) | Err(_) => false,
                            Ok(_) => !answer.trim().to_lowercase().starts_with('n'),
                        }
                    });
                    if !yes {
                        return false;
                    }
                    if let Progress::SemanticStart { label, rows, requests } = event {
                        start_bar(bar, label, *rows, *requests);
                    }
                }
                Progress::SemanticStart { label, rows, requests } => start_bar(bar, label, *rows, *requests),
                Progress::SemanticAdvance { done } => bar.set_position(*done as u64),
                Progress::SemanticEnd => {
                    bar.set_style(spinner());
                    bar.set_message(style("DataFusion").blue().to_string());
                }
            }
            true
        })
    }

    pub fn start_activity(&self) {
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner());
        bar.set_message(format!("{} scanning and filtering", style("DataFusion").blue()));
        bar.enable_steady_tick(Duration::from_millis(80));
        *self.activity.lock().expect("activity lock") = Some(bar);
    }

    pub fn stop_activity(&self) {
        if let Some(bar) = self.activity.lock().expect("activity lock").take() {
            bar.finish_and_clear();
        }
    }

    pub fn result(&self, result: &QueryResult, prepared: &Prepared, backend: Option<&str>) {
        let rows = render::cells(result).unwrap_or_default();
        let header: Vec<String> = result.schema.names().iter().map(|n| n.to_string()).collect();
        println!("\n{}", bullet(&format!("{} result{}", compact(rows.len()), if rows.len() == 1 { "" } else { "s" })));
        if !rows.is_empty() {
            self.table(&header, &rows);
        }
        self.footer(&result.metrics, prepared, backend);
    }

    fn table(&self, header: &[String], rows: &[Vec<String>]) {
        let limit = if self.verbose { 50 } else { 15 };
        let shown = &rows[..rows.len().min(limit)];
        let numeric: Vec<bool> = (0..header.len())
            .map(|i| shown.iter().all(|r| r[i].is_empty() || r[i].parse::<f64>().is_ok()))
            .collect();
        let cells: Vec<Vec<String>> = shown
            .iter()
            .map(|r| r.iter().enumerate().map(|(i, c)| if numeric[i] { number(c, &header[i]) } else { c.clone() }).collect())
            .collect();
        let mut widths: Vec<usize> = (0..header.len())
            .map(|i| cells.iter().map(|r| measure_text_width(&r[i])).chain([measure_text_width(&header[i])]).max().unwrap_or(1).min(MAX_CELL))
            .collect();
        // shrink the widest text columns until the table fits; numbers never shrink
        let budget = self.width.saturating_sub(3 * header.len() + 1);
        let floor: Vec<usize> = (0..header.len()).map(|i| if numeric[i] { widths[i] } else { widths[i].min(8) }).collect();
        while widths.iter().sum::<usize>() > budget {
            let Some(i) = (0..widths.len()).filter(|&i| widths[i] > floor[i]).max_by_key(|&i| widths[i]) else { break };
            widths[i] -= 1;
        }
        let rule = |l: &str, m: &str, r: &str| dim(format!("{l}{}{r}", widths.iter().map(|w| "─".repeat(w + 2)).collect::<Vec<_>>().join(m)));
        let line = |row: &[String], head: bool| {
            let cols: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let c = truncate_str(c, widths[i], "…");
                    let pad = " ".repeat(widths[i] - measure_text_width(&c));
                    match (head, numeric[i]) {
                        (true, _) => format!("{}{pad}", style(c).bold()),
                        (false, true) => format!("{pad}{c}"),
                        (false, false) => format!("{c}{pad}"),
                    }
                })
                .collect();
            format!("{} {} {}", dim("│"), cols.join(&format!(" {} ", dim("│"))), dim("│"))
        };
        println!("{}", rule("╭", "┬", "╮"));
        println!("{}", line(header, true));
        println!("{}", rule("├", "┼", "┤"));
        for row in &cells {
            println!("{}", line(row, false));
        }
        println!("{}", rule("╰", "┴", "╯"));
        if rows.len() > shown.len() {
            println!("  {}", dim(format!("… {} more (\\verbose shows up to 50; RANK BY … LIMIT n to narrow)", rows.len() - shown.len())));
        }
    }

    fn footer(&self, m: &ExecMetrics, prepared: &Prepared, backend: Option<&str>) {
        let secs = m.total_time.as_secs_f64();
        let time = if secs < 1.0 { format!("{:.0} ms", secs * 1000.0) } else { format!("{secs:.1} s") };
        let mut parts = vec![format!("{} rows scanned", compact(m.rows_scanned))];
        if prepared.engine_path().contains(&"Jev") {
            parts.push(format!("{} → Jev", compact(m.semantic_rows)));
            parts.push(format!("{} request{}", compact(m.requests), if m.requests == 1 { "" } else { "s" }));
            if m.cache_hits > 0 {
                parts.push(format!("{} cached", compact(m.cache_hits)));
            }
        } else {
            parts.push("no Jev calls".into());
        }
        parts.push(time);
        if m.estimated_cost_usd > 0.0 {
            parts.push(format!("${:.4}", m.estimated_cost_usd));
        }
        if backend == Some("simulated") && prepared.engine_path().contains(&"Jev") {
            parts.push("simulated judgments".into());
        }
        println!("  {}", dim(parts.join(" · ")));
    }

    pub fn ask_error(&self, e: &AskError) {
        println!("{} {}", style("✗").red().bold(), e.message);
        if let Some((nql, pos)) = &e.nql {
            match pos {
                Some((line, col)) => {
                    if let Some(text) = nql.lines().nth(line - 1) {
                        println!("  {}", dim(text));
                        println!("  {}{}", " ".repeat(col.saturating_sub(1)), style("^").red().bold());
                    }
                }
                None if self.verbose => nql.lines().for_each(|l| println!("  {}", dim(l))),
                None => {}
            }
        }
    }

    pub fn error(&self, message: &str) {
        println!("{} {message}", style("✗").red().bold());
    }

    pub fn prompt(&self) -> String {
        format!("{} ", Style::new().cyan().bold().apply_to("❯"))
    }
}

/// Ask before sending a live batch larger than this.
const CONFIRM_REQUESTS: usize = 1_000;
/// Rough per-request figures for the estimate (observed on Jev).
const TOKENS_PER_REQUEST: f64 = 450.0;
const REQUESTS_PER_SECOND: f64 = 35.0;

fn start_bar(bar: &ProgressBar, label: &str, rows: usize, requests: usize) {
    bar.set_style(
        ProgressStyle::with_template("{spinner:.magenta} {msg}  {bar:28.magenta/dim} {pos}/{len}").expect("template").progress_chars("█▓░"),
    );
    bar.set_length(requests as u64);
    bar.set_position(0);
    bar.set_message(format!("{} {label} · {} rows", style("Jev").magenta().bold(), compact(rows)));
}

fn duration(secs: f64) -> String {
    match secs {
        s if s < 60.0 => format!("{s:.0} s"),
        s => format!("{:.0} min", (s / 60.0).ceil()),
    }
}

fn spinner() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {msg}").expect("template").tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

/// `230035` -> `230,035`; `1250000` -> `1.25M` (for counts in prose).
fn compact(n: usize) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.2}M", n as f64 / 1e6),
        n => group(n),
    }
}

/// `1370000` -> `1,370,000` (for values in tables).
fn group(n: usize) -> String {
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

/// Readable numbers: ids untouched, floats rounded, big values grouped.
fn number(cell: &str, column: &str) -> String {
    let Ok(v) = cell.parse::<f64>() else { return cell.to_string() };
    if column.ends_with("_id") || !cell.contains('.') {
        return cell.to_string();
    }
    if v.abs() >= 1000.0 {
        let whole = group(v.trunc().abs() as usize);
        format!("{}{whole}.{:02}", if v < 0.0 { "-" } else { "" }, ((v.abs().fract() * 100.0).round() as u64).min(99))
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

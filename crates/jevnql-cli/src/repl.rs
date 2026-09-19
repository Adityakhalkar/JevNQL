//! The interactive shell. In a terminal: line editing, history, live progress
//! and formatted results (see `ui`). With piped input: plain text.

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use jevir::Catalog;
use jevnql_core::Engine;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::{Value, json};

use crate::ask;
use crate::ui::Ui;
use crate::{Request, backend_price, handle, live_backend};

type Error = Box<dyn std::error::Error>;

const HELP: &str = "\
  Ask in plain English:
    enterprise customers with many orders who sound like they're leaving
    top 10 customers with the most reviews
    open tickets that sound urgent

  Or write NQL (starts with FROM; end with `;` or an empty line):
    FROM customers
    WITH reviews AS history (LAST 20 BY created_at)
    FIND customers WHO: \"seem unhappy with our pricing\";

  EXPLAIN <question>   show the interpretation and plans without running
  \\verbose             toggle full plans, generated NQL and up to 50 rows
  \\tables              tables and columns
  \\ir                  the last compiled JevIR plan (JSON)
  \\clear               clear the screen (or Ctrl+L)
  \\q                   quit";

pub async fn run(engine: &mut Engine, optimize: bool) -> Result<(), Error> {
    match std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        true => interactive(engine, optimize).await,
        false => plain(engine, optimize).await,
    }
}

/// Splits a leading EXPLAIN off a question.
fn explain_prefix(text: &str) -> (bool, String) {
    let t = text.trim();
    match t.get(..8) {
        Some(head) if head.eq_ignore_ascii_case("EXPLAIN ") => (true, t[8..].trim().trim_matches('"').to_string()),
        _ => (false, t.to_string()),
    }
}

async fn interactive(engine: &mut Engine, optimize: bool) -> Result<(), Error> {
    let mut ui = Ui::new();
    let loading = ProgressBar::new_spinner();
    loading.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}")?.tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "));
    loading.set_message("Reading your data…");
    loading.enable_steady_tick(Duration::from_millis(80));
    let vocab = engine.vocabulary().await?;
    let profiles = engine.profiles().await?;
    loading.finish_and_clear();

    let backend = engine.backend_name().unwrap_or("none").to_string();
    let (live, price) = (live_backend(engine), backend_price(engine));
    engine.session_mut().set_progress(Some(ui.progress_hook(live, price)));
    let table_sizes: Vec<(String, usize)> = profiles.iter().map(|p| (p.name.clone(), p.rows)).collect();
    ui.banner(&table_sizes, &backend);

    let history = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".jevnql_history"));
    let mut editor = DefaultEditor::new()?;
    if let Some(path) = &history {
        let _ = editor.load_history(path);
    }
    let mut last_ir: Option<Value> = None;
    loop {
        println!();
        let first = match editor.readline(&ui.prompt()) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(e.into()),
        };
        let trimmed = first.trim();
        match trimmed {
            "" => continue,
            "\\q" | "\\quit" | "exit" | "quit" => break,
            "\\help" => {
                println!("{HELP}");
                continue;
            }
            "\\clear" | "clear" => {
                console::Term::stdout().clear_screen()?;
                ui.banner(&table_sizes, &backend);
                continue;
            }
            "\\verbose" => {
                ui.verbose = !ui.verbose;
                println!("  verbose {}", if ui.verbose { "on" } else { "off" });
                continue;
            }
            "\\tables" => {
                let tables: Vec<(String, Vec<(String, String)>)> = engine
                    .session()
                    .table_names()
                    .into_iter()
                    .map(|n| {
                        let schema = engine.session().table_schema(&n).expect("registered");
                        let cols = schema.fields().iter().map(|f| (f.name.clone(), f.data_type.to_string())).collect();
                        (n, cols)
                    })
                    .collect();
                ui.tables(&tables);
                continue;
            }
            "\\ir" => {
                match &last_ir {
                    Some(ir) => println!("{}", serde_json::to_string_pretty(ir)?),
                    None => println!("  (no query yet)"),
                }
                continue;
            }
            _ => {}
        }

        // NQL can span lines: read until `;` or an empty line
        let mut text = first.clone();
        let (_, body) = explain_prefix(&text);
        if ask::is_nql(&body) && !trimmed.ends_with(';') {
            loop {
                match editor.readline(&format!("{} ", console::style("…").dim())) {
                    Ok(line) if line.trim().is_empty() => break,
                    Ok(line) => {
                        let done = line.trim_end().ends_with(';');
                        text.push('\n');
                        text.push_str(&line);
                        if done {
                            break;
                        }
                    }
                    Err(ReadlineError::Interrupted) => {
                        text.clear();
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            if text.is_empty() {
                continue;
            }
        }
        let _ = editor.add_history_entry(text.replace('\n', " "));
        let (explain_only, question) = explain_prefix(&text);
        println!();
        if let Some(ir) = answer(&ui, engine, &vocab, question.trim_end_matches(';'), explain_only, optimize).await {
            last_ir = Some(ir);
        }
    }
    if let Some(path) = &history {
        let _ = editor.save_history(path);
    }
    Ok(())
}

/// Compiles, plans and (unless explaining) runs one question. Returns the JevIR.
pub async fn answer(
    ui: &Ui,
    engine: &Engine,
    vocab: &jev_nl::Vocabulary,
    question: &str,
    explain_only: bool,
    optimize: bool,
) -> Option<Value> {
    let asked = match ask::compile(engine, vocab, question).await {
        Ok(a) => a,
        Err(e) => {
            ui.ask_error(&e);
            return None;
        }
    };
    ui.understood(&asked);
    let prepared = match engine.prepare(&asked.document.to_string(), optimize) {
        Ok(p) => p,
        Err(e) => {
            ui.error(&e.to_string());
            return None;
        }
    };
    let backend = engine.backend_name().map(str::to_string);
    ui.plan(&prepared, backend.as_deref());
    if explain_only {
        if !ui.verbose {
            println!();
            for line in crate::render::explain(&prepared).lines() {
                println!("  {line}");
            }
        }
        return Some(asked.document);
    }
    ui.start_activity();
    let result = engine.execute(&prepared).await;
    ui.stop_activity();
    match result {
        Ok(r) => ui.result(&r, &prepared, backend.as_deref()),
        Err(e) => ui.error(&e.to_string()),
    }
    Some(asked.document)
}

/// Piped input: one question per line (NQL until `;` or an empty line).
async fn plain(engine: &Engine, optimize: bool) -> Result<(), Error> {
    let vocab = engine.vocabulary().await?;
    println!("JevNQL — semantic backend: {}. Type \\help for help.", engine.backend_name().unwrap_or("none"));
    let mut buffer = String::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if buffer.is_empty() {
            match trimmed {
                "" => continue,
                "\\q" | "\\quit" | "exit" | "quit" => return Ok(()),
                "\\help" => {
                    println!("{HELP}");
                    continue;
                }
                _ => {}
            }
        }
        buffer.push_str(&line);
        buffer.push('\n');
        let (_, body) = explain_prefix(&buffer);
        let complete = !ask::is_nql(&body) || trimmed.is_empty() || trimmed.ends_with(';');
        if !complete {
            continue;
        }
        let (explain_only, question) = explain_prefix(&std::mem::take(&mut buffer));
        println!("\nJevNQL > {}", question.trim());
        match ask::compile(engine, &vocab, question.trim_end_matches(';')).await {
            Err(e) => eprintln!("error: {}", e.message),
            Ok(asked) => {
                if let Some(notes) = &asked.notes {
                    println!("Interpreted as:");
                    notes.iter().for_each(|n| println!("  - {n}"));
                    println!("\n{}\n", asked.nql);
                }
                let out = handle(engine, Request::Run { plan: asked.document, explain_only, optimize }).await;
                match out["ok"] == json!(true) {
                    true => println!("{}", out["text"].as_str().unwrap_or_default()),
                    false => eprintln!("error: {}", out["error"].as_str().unwrap_or_default()),
                }
            }
        }
        std::io::stdout().flush()?;
    }
    Ok(())
}

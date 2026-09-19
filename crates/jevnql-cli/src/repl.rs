//! Interactive NQL shell.

use std::io::{BufRead, Write};

use jevir::Catalog;
use jevnql_core::Engine;
use serde_json::{Value, json};

use crate::{Request, handle};

const HELP: &str = "\
  Queries end with `;` or a blank line. Prefix with EXPLAIN to see plans without running.

    FROM customers
    WITH reviews AS history (LAST 30 BY created_at)
    FIND customers WHO: \"seem unhappy with our pricing\"
    RANK BY customer_id LIMIT 20;

  \\tables   list tables and columns
  \\ir       print the last compiled JevIR plan
  \\q        quit";

pub async fn run(engine: &Engine, optimize: bool) -> Result<(), Box<dyn std::error::Error>> {
    let backend = engine.backend_name().unwrap_or("none");
    println!("JevNQL — semantic backend: {backend}. Type \\help for help.\n");
    tables(engine);
    let mut last_ir: Option<Value> = None;
    let mut buffer = String::new();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("{}", if buffer.is_empty() { "\nJevNQL > " } else { "       … " });
        std::io::stdout().flush()?;
        let Some(line) = lines.next().transpose()? else {
            println!();
            return Ok(());
        };
        let trimmed = line.trim();
        if buffer.is_empty() {
            match trimmed {
                "" => continue,
                "\\q" | "\\quit" | "exit" | "quit" => return Ok(()),
                "\\help" => {
                    println!("{HELP}");
                    continue;
                }
                "\\tables" => {
                    tables(engine);
                    continue;
                }
                "\\ir" => {
                    match &last_ir {
                        Some(ir) => println!("{}", serde_json::to_string_pretty(ir)?),
                        None => println!("(no query yet)"),
                    }
                    continue;
                }
                _ => {}
            }
        }
        buffer.push_str(&line);
        buffer.push('\n');
        if !(trimmed.is_empty() || trimmed.ends_with(';')) {
            continue;
        }
        let source = std::mem::take(&mut buffer);
        let (explain_only, source) = match source.trim_start().get(..8) {
            Some(head) if head.eq_ignore_ascii_case("EXPLAIN ") => (true, source.trim_start()[8..].to_string()),
            _ => (false, source),
        };
        let compiled = match jev_nql::compile(&source, engine.session()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                continue;
            }
        };
        last_ir = Some(compiled.document.clone());
        let out = handle(engine, Request::Run { plan: compiled.document, explain_only, optimize }).await;
        match out["ok"] == json!(true) {
            true => println!("{}", out["text"].as_str().unwrap_or_default()),
            false => eprintln!("error: {}", out["error"].as_str().unwrap_or_default()),
        }
    }
}

fn tables(engine: &Engine) {
    let session = engine.session();
    for name in session.table_names() {
        let schema = session.table_schema(&name).expect("registered");
        let cols: Vec<String> = schema.fields().iter().map(|f| format!("{}: {}", f.name, f.data_type)).collect();
        println!("  {name}: {}", cols.join(", "));
    }
}

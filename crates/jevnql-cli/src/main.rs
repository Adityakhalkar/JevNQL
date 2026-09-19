//! `jevnql-engine`: the JevNQL engine's command-line interface.
//!
//! Plan-level commands (`explain`, `run`) are for people; `catalog`,
//! `validate` and `serve` speak JSON and are the boundary the Python NL
//! compiler and shell use until native bindings exist.

mod render;

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use jevnql_core::{BackendChoice, DEFAULT_MAX_SEMANTIC_ROWS, Engine, ExecMetrics, Prepared};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "jevnql-engine", version, about = "JevNQL engine: validate, optimize and run JevIR plans")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Semantic backend: TypeSafe Jev (needs TYPESAFE_API_KEY), the offline simulator, or auto.
    #[arg(long, global = true, value_enum, default_value_t = Backend::Auto)]
    backend: Backend,
    /// Refuse to send more rows than this to one semantic operator.
    #[arg(long, global = true, default_value_t = DEFAULT_MAX_SEMANTIC_ROWS)]
    max_semantic_rows: usize,
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Auto,
    Jev,
    Simulated,
}

#[derive(Subcommand)]
enum Command {
    /// Print table profiles (columns, types, ranges, examples) as JSON.
    Catalog { #[arg(required = true)] files: Vec<PathBuf> },
    /// Validate a logical JevIR plan; prints {"ok": ..., ...} as JSON.
    Validate {
        /// Plan document path, or `-` for stdin.
        #[arg(long)]
        plan: String,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Show the logical plan, optimizer rewrites and physical plan.
    Explain {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        no_optimize: bool,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Execute a plan: EXPLAIN, results and metrics.
    Run {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        no_optimize: bool,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Answer JSON requests, one per line on stdin, keeping data and the
    /// semantic cache loaded between requests.
    Serve { #[arg(required = true)] files: Vec<PathBuf> },
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Catalog,
    Validate { plan: Value },
    Run {
        plan: Value,
        #[serde(default)]
        explain_only: bool,
        #[serde(default = "yes")]
        optimize: bool,
    },
}

fn yes() -> bool {
    true
}

type Error = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

async fn open(cli: &Cli, files: &[PathBuf], semantic: bool) -> Result<Engine, Error> {
    let backend = match (semantic, cli.backend) {
        (false, _) => None,
        (true, Backend::Auto) => Some(BackendChoice::Auto.build()?),
        (true, Backend::Jev) => Some(BackendChoice::Jev.build()?),
        (true, Backend::Simulated) => Some(BackendChoice::Simulated.build()?),
    };
    let mut engine = Engine::open(files, backend).await?;
    engine.session_mut().set_max_semantic_rows(cli.max_semantic_rows);
    Ok(engine)
}

fn read_plan(plan: &str) -> Result<String, Error> {
    Ok(match plan {
        "-" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        path => std::fs::read_to_string(path)?,
    })
}

async fn run(cli: Cli) -> Result<ExitCode, Error> {
    match &cli.command {
        Command::Catalog { files } => {
            let engine = open(&cli, files, false).await?;
            println!("{}", handle(&engine, Request::Catalog).await);
        }
        Command::Validate { plan, files } => {
            let engine = open(&cli, files, false).await?;
            let plan: Value = serde_json::from_str(&read_plan(plan)?)?;
            let out = handle(&engine, Request::Validate { plan }).await;
            println!("{out}");
            if out["ok"] != json!(true) {
                return Ok(ExitCode::FAILURE);
            }
        }
        Command::Explain { plan, no_optimize, files } | Command::Run { plan, no_optimize, files } => {
            let explain_only = matches!(cli.command, Command::Explain { .. });
            let engine = open(&cli, files, !explain_only).await?;
            let plan: Value = serde_json::from_str(&read_plan(plan)?)?;
            let out = handle(&engine, Request::Run { plan, explain_only, optimize: !no_optimize }).await;
            match out["ok"] == json!(true) {
                true => print!("{}", out["text"].as_str().unwrap_or_default()),
                false => {
                    eprintln!("error: {}", out["error"].as_str().unwrap_or_default());
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        Command::Serve { files } => {
            let engine = open(&cli, files, true).await?;
            let mut stdout = std::io::stdout();
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let out = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => handle(&engine, request).await,
                    Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
                };
                writeln!(stdout, "{out}")?;
                stdout.flush()?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Answers one request as JSON; errors become `{"ok": false, "error": ...}`.
async fn handle(engine: &Engine, request: Request) -> Value {
    let result: Result<Value, Error> = async {
        Ok(match request {
            Request::Catalog => json!({"ok": true, "tables": engine.profiles().await?}),
            Request::Validate { plan } => match engine.validate(&plan.to_string()) {
                Ok(valid) => {
                    let schema: Vec<_> = valid
                        .schema
                        .fields()
                        .iter()
                        .map(|f| json!({"name": f.name, "type": f.data_type.to_string()}))
                        .collect();
                    json!({"ok": true, "schema": schema, "logical_plan": valid.plan.to_string()})
                }
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            },
            Request::Run { plan, explain_only, optimize } => {
                let prepared = match engine.prepare(&plan.to_string(), optimize) {
                    Ok(p) => p,
                    Err(e) => return Ok(json!({"ok": false, "error": e.to_string()})),
                };
                let explain = render::explain(&prepared);
                if explain_only {
                    return Ok(json!({"ok": true, "text": explain}));
                }
                let result = engine.execute(&prepared).await?;
                let text = format!(
                    "{explain}RESULT\n\n{}\nMETRICS\n\n{}",
                    render::table(&result, 20)?,
                    render::metrics(&result.metrics, &prepared, engine.backend_name())
                );
                json!({
                    "ok": true,
                    "text": text,
                    "columns": result.schema.names(),
                    "rows": render::cells(&result)?,
                    "metrics": metrics_json(&result.metrics, &prepared),
                })
            }
        })
    }
    .await;
    result.unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()}))
}

fn metrics_json(m: &ExecMetrics, prepared: &Prepared) -> Value {
    json!({
        "engine_path": prepared.engine_path(),
        "rows_scanned": m.rows_scanned,
        "semantic_rows": m.semantic_rows,
        "distinct_states": m.distinct_states,
        "semantic_batches": m.semantic_batches,
        "requests": m.requests,
        "questions": m.questions,
        "cache_hits": m.cache_hits,
        "input_tokens": m.input_tokens,
        "total_ms": m.total_time.as_secs_f64() * 1000.0,
        "semantic_ms": m.semantic_time.as_secs_f64() * 1000.0,
        "estimated_cost_usd": m.estimated_cost_usd,
    })
}

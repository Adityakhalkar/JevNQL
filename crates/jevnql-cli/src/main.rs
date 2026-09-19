//! `jevnql` command-line interface.
//!
//! Machine-facing subcommands speak JSON on stdout; they are the boundary the
//! Python NL compiler uses until native bindings exist.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jevnql_core::Engine;
use serde_json::json;

#[derive(Parser)]
#[command(name = "jevnql", version, about = "Compiler and engine for natural-language data queries")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print table profiles (columns, types, ranges, examples) as JSON.
    Catalog {
        /// Data files (.csv / .parquet); each becomes a table named after the file.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Validate a logical JevIR plan; prints {"ok": ..., ...} as JSON.
    Validate {
        /// Plan document path, or `-` for stdin.
        #[arg(long)]
        plan: String,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
}

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

async fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cli.command {
        Command::Catalog { files } => {
            let engine = Engine::open(&files).await?;
            println!("{}", json!({ "tables": engine.profiles().await? }));
            Ok(ExitCode::SUCCESS)
        }
        Command::Validate { plan, files } => {
            let text = match plan.as_str() {
                "-" => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
                path => std::fs::read_to_string(path)?,
            };
            let engine = Engine::open(&files).await?;
            match engine.validate(&text) {
                Ok(valid) => {
                    let schema: Vec<_> = valid
                        .schema
                        .fields()
                        .iter()
                        .map(|f| json!({"name": f.name, "type": f.data_type.to_string()}))
                        .collect();
                    println!("{}", json!({"ok": true, "schema": schema, "logical_plan": valid.plan.to_string()}));
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    println!("{}", json!({"ok": false, "error": e.to_string()}));
                    Ok(ExitCode::FAILURE)
                }
            }
        }
    }
}

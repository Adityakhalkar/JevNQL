//! NQL: a structured natural query language that compiles to logical JevIR.
//!
//! ```text
//! FROM customers
//! WITH orders  AS spend   (SUM amount WHERE order_date >= DATE '2026-01-01')
//! WITH reviews AS history (LAST 30 BY created_at)
//! FIND customers WHO:
//!     spend > 1000
//!     AND "seem increasingly unhappy with our pricing"
//! SCORE leave_risk: "How likely is this customer to cancel?"
//!     LEVELS ("No sign of leaving", "Frustrated", "Clear intent to cancel")
//! RANK BY spend DESC LIMIT 20
//! RETURN customer_id, name, spend, leave_risk
//! ```
//!
//! `'single quotes'` are text values; `"double quotes"` are semantic
//! judgments evaluated by the semantic backend. Everything else is
//! deterministic and must parse as an expression. Compilation is
//! deterministic: no language model is involved.

mod compile;
mod lexer;
mod parser;

use std::fmt;

pub use compile::{Compiled, compile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NqlError {
    pub message: String,
    /// 1-based position of the offending token, for syntax errors.
    pub position: Option<(usize, usize)>,
}

impl NqlError {
    pub(crate) fn at(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self { message: message.into(), position: Some((line, col)) }
    }

    pub(crate) fn plan(message: impl Into<String>) -> Self {
        Self { message: message.into(), position: None }
    }
}

impl fmt::Display for NqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some((line, col)) => write!(f, "line {line}, column {col}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for NqlError {}

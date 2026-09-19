//! One entry point for questions: NQL (starts with FROM) or plain English.

use serde_json::Value;

use jevnql_core::Engine;

/// A question compiled to a JevIR plan document.
pub struct Asked {
    pub document: Value,
    /// The NQL that was compiled.
    pub nql: String,
    /// How a plain-English question was interpreted (None for NQL input).
    pub notes: Option<Vec<String>>,
}

pub struct AskError {
    pub message: String,
    /// The NQL the error refers to, with the 1-based (line, column) if known.
    pub nql: Option<(String, Option<(usize, usize)>)>,
}

/// NQL starts with FROM, after any blank or `--` comment lines.
pub fn is_nql(question: &str) -> bool {
    let first = question.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("--")).unwrap_or("");
    first.get(..4).is_some_and(|w| w.eq_ignore_ascii_case("FROM"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn nql_is_recognized_after_comments() {
        assert!(super::is_nql("-- top customers\n\nFROM customers"));
        assert!(super::is_nql("  from customers"));
        assert!(!super::is_nql("customers from India"));
        assert!(!super::is_nql("-- FROM in a comment\nshow me customers"));
    }
}

/// Plain English is interpreted (Jev decides what the rules can't) and
/// translated to NQL; NQL is compiled as written.
pub async fn compile(engine: &Engine, vocab: &jev_nl::Vocabulary, question: &str) -> Result<Asked, AskError> {
    let (nql, notes) = match is_nql(question) {
        true => (question.to_string(), None),
        false => {
            let i = engine.interpret(question, vocab, today()).await.map_err(|e| AskError { message: e.to_string(), nql: None })?;
            let mut notes = i.translation.notes;
            if i.decided > 0 {
                notes.push(format!(
                    "Jev decided {} phrase{} · 1 request · {} tokens",
                    i.decided,
                    if i.decided == 1 { "" } else { "s" },
                    i.input_tokens
                ));
            }
            if let Some(e) = i.error {
                notes.push(format!("Jev unavailable, used default readings ({e})"));
            }
            (i.translation.nql, Some(notes))
        }
    };
    match jev_nql::compile(&nql, engine.session()) {
        Ok(c) => Ok(Asked { document: c.document, nql, notes }),
        Err(e) => Err(AskError { message: e.message.clone(), nql: Some((nql, e.position)) }),
    }
}

/// Today's date (UTC) as (year, month, day).
fn today() -> (i32, u32, u32) {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

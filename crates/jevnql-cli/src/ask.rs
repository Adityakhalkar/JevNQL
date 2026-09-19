//! One entry point for questions: NQL (starts with FROM) or plain English.

use jevnql_core::Engine;
use serde_json::Value;

/// Compiles a question to a JevIR plan document. Plain English is first
/// translated to NQL; the interpretation is printed so it can be checked.
pub fn compile(engine: &Engine, vocab: &jev_nl::Vocabulary, question: &str) -> Result<Value, String> {
    let is_nql = question.trim_start().get(..4).is_some_and(|w| w.eq_ignore_ascii_case("FROM"));
    let nql = match is_nql {
        true => question.to_string(),
        false => {
            let t = jev_nl::translate(question, vocab, today()).map_err(|e| e.to_string())?;
            println!("Interpreted as:");
            for note in &t.notes {
                println!("  - {note}");
            }
            println!("\n{}\n", indent(&t.nql));
            t.nql
        }
    };
    jev_nql::compile(&nql, engine.session()).map(|c| c.document).map_err(|e| e.to_string())
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
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

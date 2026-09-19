//! Offline stand-in for Jev, so demos and pipelines run without an API key.
//!
//! It is NOT a language model. It scores texts by keyword overlap with the
//! question, a small sentiment lexicon, and (for "increasingly"-style
//! questions) recency weighting. Results are deterministic and roughly
//! plausible on plainly worded data; anything subtler needs the real Jev.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::{
    Answer, BackendInfo, BoxFuture, ProviderError, Question, SemanticBackend, SemanticRequest, SemanticResponse,
    approx_tokens,
};

const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "any", "appear", "appears", "are", "as", "at", "be", "by", "customer", "customers",
    "does", "do", "for", "from", "has", "have", "how", "in", "is", "it", "its", "of", "on", "or", "our", "review",
    "reviews", "seem", "seems", "show", "that", "the", "their", "them", "they", "this", "to", "was", "what", "which",
    "who", "with", "whose", "history", "across", "order", "chronological", "specifically", "we", "us", "very",
];

/// Concept -> words that express it in customer text.
const CONCEPTS: &[(&[&str], &[&str])] = &[
    (
        &["price", "prices", "pricing", "priced", "cost", "costs", "expensive", "value", "money", "billing"],
        &["price", "prices", "pricing", "pricey", "priced", "expensive", "cost", "costs", "costly", "overpriced", "charge", "charged",
          "fee", "fees", "bill", "billing", "money", "afford", "subscription"],
    ),
    (
        &["leave", "leaving", "churn", "cancel", "switch", "quit", "stay"],
        &["cancel", "cancelling", "canceling", "leave", "leaving", "switch", "switching", "competitor", "done",
          "unsubscribe", "refund", "alternative", "moving"],
    ),
    (
        &["urgent", "urgency", "asap", "outage", "broken"],
        &["urgent", "asap", "immediately", "outage", "down", "broken", "critical", "now"],
    ),
];

const NEGATIVE: &[&str] = &[
    "unhappy", "disappointed", "disappointing", "frustrated", "frustrating", "angry", "annoyed", "bad", "terrible",
    "awful", "worst", "poor", "hate", "not", "never", "complain", "complaint", "worse", "broken", "slow", "useless",
    "ridiculous", "too", "again", "cancel", "refund", "expensive", "overpriced", "pricey",
];

const NEGATIVE_INTENT: &[&str] = &[
    "unhappy", "dissatisfied", "dissatisfaction", "frustrated", "frustration", "angry", "complain", "complains",
    "complaining", "negative", "upset", "disappointed", "unhappiness", "leave", "churn", "cancel",
];

const TREND: &[&str] = &["increasingly", "increasing", "growing", "worsening", "more", "trend", "over"];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Free-text strings in a state, in document order (dates and numbers skipped).
fn texts(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.chars().any(char::is_alphabetic) => out.push(s.to_lowercase()),
        Value::Array(a) => a.iter().for_each(|x| texts(x, out)),
        Value::Object(o) => o.values().for_each(|x| texts(x, out)),
        _ => {}
    }
}

/// Words that signal the question's topic in the data.
fn topic_terms(instructions: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for w in words(instructions) {
        if STOPWORDS.contains(&w.as_str()) || NEGATIVE_INTENT.contains(&w.as_str()) || TREND.contains(&w.as_str()) {
            continue;
        }
        match CONCEPTS.iter().find(|(triggers, _)| triggers.contains(&w.as_str())) {
            Some((_, expansion)) => terms.extend(expansion.iter().map(|s| s.to_string())),
            None if w.len() > 3 => {
                terms.insert(w);
            }
            None => {}
        }
    }
    terms
}

/// Probability-like strength of `instructions` in `state`, in [0, 1].
fn strength(instructions: &str, state: &Value) -> f64 {
    let mut docs = Vec::new();
    texts(state, &mut docs);
    if docs.is_empty() {
        return 0.0;
    }
    let q = words(instructions);
    let negative = q.iter().any(|w| NEGATIVE_INTENT.contains(&w.as_str()));
    let trend = q.iter().any(|w| TREND.contains(&w.as_str()));
    let terms = topic_terms(instructions);

    let hit = |doc: &str| {
        let ws = words(doc);
        let topical = terms.is_empty() || ws.iter().any(|w| terms.contains(w));
        let sour = ws.iter().any(|w| NEGATIVE.contains(&w.as_str()));
        f64::from(u8::from(topical && (!negative || sour)))
    };
    // later documents weigh more for trend questions (histories are chronological)
    let weight = |i: usize| if trend { (i + 1) as f64 } else { 1.0 };
    let total: f64 = (0..docs.len()).map(weight).sum();
    let score: f64 = docs.iter().enumerate().map(|(i, d)| weight(i) * hit(d)).sum();
    score / total
}

pub struct SimulatedBackend {
    info: BackendInfo,
}

impl Default for SimulatedBackend {
    fn default() -> Self {
        Self {
            info: BackendInfo {
                name: "simulated".into(),
                // costs are estimated at Jev's price, for comparison
                usd_per_million_input_tokens: 0.042,
                max_concurrency: 16,
                max_state_tokens: 30_000,
            },
        }
    }
}

impl SemanticBackend for SimulatedBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn evaluate<'a>(&'a self, request: &'a SemanticRequest) -> BoxFuture<'a, Result<SemanticResponse, ProviderError>> {
        Box::pin(async move {
            let mut question_chars = 0;
            let answers = request
                .questions
                .iter()
                .map(|(id, q)| {
                    let answer = match q {
                        Question::Noul { instructions } => {
                            question_chars += instructions.len();
                            Answer::Noul { probability: 0.05 + 0.9 * strength(instructions, &request.state) }
                        }
                        Question::Score { instructions, levels } => {
                            question_chars += instructions.len() + levels.iter().map(String::len).sum::<usize>();
                            Answer::Score { value: strength(instructions, &request.state), confidence: 0.5 }
                        }
                        Question::Choice { instructions, options } => {
                            question_chars += instructions.len();
                            let best = options
                                .iter()
                                .map(|o| {
                                    let text = format!("{} {}", o.label, o.description.as_deref().unwrap_or_default());
                                    (strength(&text, &request.state), o)
                                })
                                .fold(None::<(f64, &crate::ChoiceOption)>, |best, (s, o)| match best {
                                    Some((b, _)) if b >= s => best,
                                    _ => Some((s, o)),
                                })
                                .map(|(_, o)| o.label.clone())
                                .unwrap_or_default();
                            Answer::Choice { label: best, confidence: 0.5 }
                        }
                    };
                    (id.clone(), answer)
                })
                .collect();
            let input_tokens = (approx_tokens(&request.state.to_string()) + question_chars / 4) as u64;
            Ok(SemanticResponse { answers, input_tokens })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pricing_complaints_outscore_praise() {
        let q = "Does this customer appear increasingly dissatisfied with our pricing?";
        let unhappy = json!({"history": [{"text": "Great product"}, {"text": "Way too expensive now, cancelling"}]});
        let happy = json!({"history": [{"text": "Price is fair"}, {"text": "Love it, great value"}]});
        assert!(strength(q, &unhappy) > 0.5);
        assert!(strength(q, &happy) < 0.2);
    }

    #[test]
    fn trend_questions_weight_recent_texts() {
        let q = "Is this customer increasingly unhappy about pricing?";
        let worsening = json!([{"text": "ok"}, {"text": "prices are too high, not happy"}]);
        let improving = json!([{"text": "prices are too high, not happy"}, {"text": "ok"}]);
        assert!(strength(q, &worsening) > strength(q, &improving));
    }
}

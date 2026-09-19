//! TypeSafe hosted Jev backend (`POST /v1/systemone`).
//!
//! All TypeSafe-specific HTTP and wire-format logic lives in this module.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::{Answer, BackendInfo, BoxFuture, ProviderError, Question, SemanticBackend, SemanticRequest, SemanticResponse};

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
pub const DEFAULT_MODEL: &str = "jev-latest";
const API_KEY_VAR: &str = "TYPESAFE_API_KEY";
const MAX_RETRIES: u32 = 5;

pub struct TypeSafeJevBackend {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    info: BackendInfo,
}

impl TypeSafeJevBackend {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            client: reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("static client config"),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.into(),
            info: BackendInfo {
                name: format!("typesafe:{model}"),
                // Jev 1.13 pricing: input tokens only
                usd_per_million_input_tokens: 0.042,
                // 1,200 requests/minute = 20/s
                max_concurrency: 16,
                // 32k tokens for state + longest question
                max_state_tokens: 30_000,
            },
            model,
        }
    }

    /// Reads `TYPESAFE_API_KEY`; `TYPESAFE_MODEL` and `TYPESAFE_BASE_URL` are optional.
    pub fn from_env() -> Result<Self, ProviderError> {
        let key = std::env::var(API_KEY_VAR).map_err(|_| ProviderError::MissingApiKey(API_KEY_VAR))?;
        let model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        let mut backend = Self::new(key, model);
        if let Ok(url) = std::env::var("TYPESAFE_BASE_URL") {
            backend.base_url = url;
        }
        Ok(backend)
    }

    async fn post(&self, body: &Value) -> Result<Value, ProviderError> {
        let url = format!("{}/v1/systemone", self.base_url.trim_end_matches('/'));
        let mut attempt = 0;
        loop {
            let sent = self.client.post(&url).bearer_auth(&self.api_key).json(body).send().await;
            let retry_after = match sent {
                Ok(resp) if resp.status().is_success() => {
                    return resp.json().await.map_err(|e| ProviderError::InvalidResponse(e.to_string()));
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let wait = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()?.parse::<f64>().ok())
                        .map(Duration::from_secs_f64);
                    let retryable = matches!(status, 429 | 500 | 502 | 503 | 504 | 529);
                    if !retryable || attempt >= MAX_RETRIES {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::Http { status, body });
                    }
                    wait
                }
                Err(e) if attempt >= MAX_RETRIES || !(e.is_timeout() || e.is_connect()) => {
                    return Err(ProviderError::Transport(e.to_string()));
                }
                Err(_) => None,
            };
            let backoff = Duration::from_millis(500 * 2u64.pow(attempt));
            tokio::time::sleep(retry_after.unwrap_or(backoff).min(Duration::from_secs(30))).await;
            attempt += 1;
        }
    }
}

impl SemanticBackend for TypeSafeJevBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn evaluate<'a>(&'a self, request: &'a SemanticRequest) -> BoxFuture<'a, Result<SemanticResponse, ProviderError>> {
        Box::pin(async move {
            let body = self.post(&request_body(&self.model, request)).await?;
            parse_response(request, &body)
        })
    }
}

/// Encodes a request in the TypeSafe wire format.
pub fn request_body(model: &str, request: &SemanticRequest) -> Value {
    let questions: Map<String, Value> = request
        .questions
        .iter()
        .map(|(id, q)| {
            let q = match q {
                Question::Noul { instructions } => json!({"type": "noul", "instructions": instructions}),
                Question::Score { instructions, levels } => {
                    json!({"type": "score", "instructions": instructions, "criteria": levels})
                }
                Question::Choice { instructions, options } => {
                    let criteria: Map<String, Value> =
                        options.iter().map(|o| (o.label.clone(), json!(o.description))).collect();
                    json!({"type": "choice", "instructions": instructions, "criteria": criteria})
                }
            };
            (id.clone(), q)
        })
        .collect();
    json!({"state": request.state, "model": model, "questions": questions})
}

/// Decodes a TypeSafe response, checking it answers exactly what was asked.
pub fn parse_response(request: &SemanticRequest, body: &Value) -> Result<SemanticResponse, ProviderError> {
    let bad = |m: String| ProviderError::InvalidResponse(m);
    let answers_json = body.get("answers").and_then(Value::as_object).ok_or_else(|| bad("missing `answers`".into()))?;
    let mut answers = HashMap::new();
    for (id, question) in &request.questions {
        let a = answers_json.get(id).ok_or_else(|| bad(format!("no answer for question `{id}`")))?;
        let num = |field: &str| {
            a.get(field).and_then(Value::as_f64).ok_or_else(|| bad(format!("answer `{id}` lacks numeric `{field}`")))
        };
        let answer = match question {
            Question::Noul { .. } => Answer::Noul { probability: num("noul")? },
            Question::Score { levels, .. } => {
                let top = (levels.len() - 1) as f64;
                Answer::Score { value: (num("score")? / top).clamp(0.0, 1.0), confidence: num("confidence")? }
            }
            Question::Choice { options, .. } => {
                let label = a.get("choice").and_then(Value::as_str).unwrap_or_default();
                if !options.iter().any(|o| o.label == label) {
                    return Err(bad(format!("answer `{id}` chose unknown option `{label}`")));
                }
                let mut probabilities: Vec<(String, f64)> = options
                    .iter()
                    .map(|o| (o.label.clone(), a.pointer(&format!("/probabilities/{}", o.label.replace('~', "~0").replace('/', "~1"))).and_then(Value::as_f64).unwrap_or(0.0)))
                    .collect();
                probabilities.sort_by(|x, y| y.1.total_cmp(&x.1));
                Answer::Choice { label: label.to_string(), confidence: num("confidence")?, probabilities }
            }
        };
        answers.insert(id.clone(), answer);
    }
    let input_tokens = body.pointer("/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0);
    Ok(SemanticResponse { answers, input_tokens })
}

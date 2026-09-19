use jev_provider::typesafe::{TypeSafeJevBackend, parse_response, request_body};
use jev_provider::{Answer, ChoiceOption, Question, SemanticBackend, SemanticRequest};
use serde_json::json;

fn request() -> SemanticRequest {
    SemanticRequest {
        state: json!({"document": "Help! My payouts have been failing for 3 days."}),
        questions: vec![
            ("is_urgent".into(), Question::Noul { instructions: "Does this convey urgency?".into() }),
            (
                "department".into(),
                Question::Choice {
                    instructions: "Which team should handle this?".into(),
                    options: vec![
                        ChoiceOption { label: "billing".into(), description: Some("Payments, invoicing, refunds".into()) },
                        ChoiceOption { label: "technical".into(), description: None },
                    ],
                },
            ),
            (
                "frustration".into(),
                Question::Score {
                    instructions: "How frustrated is the customer?".into(),
                    levels: vec!["Calm".into(), "Frustrated".into(), "Very angry".into()],
                },
            ),
        ],
    }
}

#[test]
fn encodes_documented_wire_format() {
    let body = request_body("jev-latest", &request());
    assert_eq!(
        body,
        json!({
            "state": {"document": "Help! My payouts have been failing for 3 days."},
            "model": "jev-latest",
            "questions": {
                "is_urgent": {"type": "noul", "instructions": "Does this convey urgency?"},
                "department": {"type": "choice", "instructions": "Which team should handle this?",
                               "criteria": {"billing": "Payments, invoicing, refunds", "technical": null}},
                "frustration": {"type": "score", "instructions": "How frustrated is the customer?",
                                "criteria": ["Calm", "Frustrated", "Very angry"]}
            }
        })
    );
}

#[test]
fn decodes_documented_answers_and_normalizes_scores() {
    let body = json!({
        "model": "jev-latest",
        "answers": {
            "is_urgent": {"type": "noul", "noul": 0.92},
            "department": {"type": "choice", "choice": "technical",
                           "probabilities": {"billing": 0.15, "technical": 0.85}, "confidence": 0.82},
            "frustration": {"type": "score", "score": 1.6, "legend": {"0": "Calm", "1": "Frustrated", "2": "Very angry"},
                            "probabilities": {"0": 0.05, "1": 0.3, "2": 0.65}, "confidence": 0.78}
        },
        "usage": {"input_tokens": 312, "output_tokens": 48}
    });
    let r = parse_response(&request(), &body).unwrap();
    assert_eq!(r.input_tokens, 312);
    assert_eq!(r.answers["is_urgent"], Answer::Noul { probability: 0.92 });
    assert_eq!(
        r.answers["department"],
        Answer::Choice {
            label: "technical".into(),
            confidence: 0.82,
            probabilities: vec![("technical".into(), 0.85), ("billing".into(), 0.15)],
        }
    );
    assert_eq!(r.answers["frustration"], Answer::Score { value: 0.8, confidence: 0.78 });
}

#[test]
fn rejects_incomplete_or_unknown_answers() {
    let missing = json!({"answers": {"is_urgent": {"type": "noul", "noul": 0.5}}});
    assert!(parse_response(&request(), &missing).unwrap_err().to_string().contains("no answer for question"));

    let bad_label = json!({"answers": {
        "is_urgent": {"noul": 0.5},
        "department": {"choice": "sales", "confidence": 0.9},
        "frustration": {"score": 1.0, "confidence": 0.5}
    }});
    assert!(parse_response(&request(), &bad_label).unwrap_err().to_string().contains("unknown option `sales`"));
}

/// Live call; runs only when TYPESAFE_API_KEY is set.
#[tokio::test]
async fn live_jev_call() {
    let Ok(backend) = TypeSafeJevBackend::from_env() else {
        eprintln!("skipping: TYPESAFE_API_KEY not set");
        return;
    };
    let r = backend.evaluate(&request()).await.unwrap();
    assert!(matches!(r.answers["is_urgent"], Answer::Noul { probability } if probability > 0.5));
    assert!(r.input_tokens > 0);
}

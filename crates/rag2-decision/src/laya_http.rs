//! Laya decision models over HTTP: the decision wire (`POST /v1/decide {state, questions}` ->
//! `{answers: [...]}`, `GET /v1/models`). RAG2's typed questions go out as-is; answers come back
//! validated against what was asked.

use rag2_core::interfaces::{Answer, DecisionProvider, Question, check_answers};
use rag2_core::{Error, Result};
use std::time::Duration;

/// The decision wire's request body.
pub fn request_json(state: &str, questions: &[Question]) -> serde_json::Value {
    serde_json::json!({ "state": state, "questions": questions })
}

/// Parse and validate a wire response against the questions asked. The model is untrusted: a
/// label that was not offered, a confidence outside [0, 1] or a count mismatch is refused.
pub fn parse_answers(v: &serde_json::Value, questions: &[Question]) -> Result<Vec<Answer>> {
    let answers: Vec<Answer> = serde_json::from_value(v["answers"].clone())
        .map_err(|e| Error::Unavailable(format!("decision model answered off-wire: {e}")))?;
    check_answers(questions, &answers)?;
    Ok(answers)
}

/// A Laya sidecar on the decision wire (`POST /v1/decide`, `GET /v1/models`).
pub struct LayaHttp {
    label: String,
    endpoint: String,
    serve_id: String,
    http: reqwest::blocking::Client,
}

impl LayaHttp {
    pub fn new(name: &str, endpoint: &str, serve_id: &str, timeout: Duration) -> Result<Self> {
        Ok(LayaHttp {
            label: format!("laya-http:{name}"),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            serve_id: serve_id.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(Error::internal)?,
        })
    }
}

impl DecisionProvider for LayaHttp {
    fn name(&self) -> &str {
        &self.label
    }

    /// Healthy means something answers AND says it serves the configured model: a port is not a
    /// model.
    fn healthy(&self) -> bool {
        let Ok(r) = self.http.get(format!("{}/v1/models", self.endpoint)).send() else {
            return false;
        };
        let Ok(v) = r.json::<serde_json::Value>() else {
            return false;
        };
        v["data"]
            .as_array()
            .is_some_and(|d| d.iter().any(|m| m["id"] == self.serve_id.as_str()))
    }

    fn decide(&self, state: &str, questions: &[Question]) -> Result<Vec<Answer>> {
        let resp = self
            .http
            .post(format!("{}/v1/decide", self.endpoint))
            .json(&request_json(state, questions))
            .send()
            .map_err(|e| Error::Unavailable(format!("{}: {e}", self.label)))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| Error::Unavailable(format!("{}: {e}", self.label)))?;
        if !status.is_success() {
            return Err(Error::Unavailable(format!(
                "{} refused: {}",
                self.label,
                v["error"].as_str().unwrap_or("unknown")
            )));
        }
        parse_answers(&v, questions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::routing::{get, post};
    use rag2_core::interfaces::ChoiceOption;

    fn questions() -> Vec<Question> {
        vec![
            Question::Choice {
                instructions: "Which retrieval suits this query?".into(),
                options: vec![
                    ChoiceOption {
                        label: "keyword".into(),
                        description: "exact terms".into(),
                    },
                    ChoiceOption {
                        label: "semantic".into(),
                        description: "meaning".into(),
                    },
                ],
            },
            Question::YesNo {
                instructions: "Is the query about safety?".into(),
            },
        ]
    }

    #[test]
    fn off_wire_or_dishonest_answers_are_refused() {
        let q = questions();
        let ok = serde_json::json!({"answers": [{"label": "keyword", "confidence": 0.9}, {"yes": false, "confidence": 0.7}]});
        assert_eq!(parse_answers(&ok, &q).expect("valid").len(), 2);
        let bad_label = serde_json::json!({"answers": [{"label": "graph", "confidence": 0.9}, {"yes": false, "confidence": 0.7}]});
        assert!(parse_answers(&bad_label, &q).is_err());
        let short = serde_json::json!({"answers": [{"label": "keyword", "confidence": 0.9}]});
        assert!(parse_answers(&short, &q).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_http_adapter_talks_to_a_sidecar_and_checks_its_identity() {
        let app = axum::Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(serde_json::json!({"data": [{"id": "laya"}]})) }),
            )
            .route(
                "/v1/decide",
                post(|Json(v): Json<serde_json::Value>| async move {
                    let n = v["questions"].as_array().map_or(0, Vec::len);
                    let mut a = vec![serde_json::json!({"label": "semantic", "confidence": 0.95})];
                    if n > 1 {
                        a.push(serde_json::json!({"yes": true, "confidence": 0.8}));
                    }
                    Json(serde_json::json!({ "answers": a }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let endpoint = format!("http://{addr}");
        let (ok, wrong) = tokio::task::spawn_blocking(move || {
            let good =
                LayaHttp::new("t", &endpoint, "laya", Duration::from_secs(5)).expect("client");
            let wrong = LayaHttp::new("t", &endpoint, "other-model", Duration::from_secs(5))
                .expect("client");
            assert!(good.healthy());
            (good.decide("query text", &questions()), wrong.healthy())
        })
        .await
        .expect("join");
        let answers = ok.expect("answers");
        assert_eq!(
            answers[0],
            Answer::Choice {
                label: "semantic".into(),
                confidence: 0.95
            }
        );
        assert!(!wrong, "a port serving another model is not healthy");
    }
}

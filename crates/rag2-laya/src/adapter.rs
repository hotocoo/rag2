//! Laya as a RAG2 [`DecisionProvider`]. The adapter only translates: RAG2's typed questions become
//! the decision wire (`{state, questions}`), Laya's answers come back validated against what was
//! asked. Laya keeps its own responsibilities (typed questions in, calibrated answers out, one
//! forward pass) and gains none.

use crate::model::Laya;
use rag2_config::schema::{DecisionModelConfig, Device as CfgDevice, EmbedderConfig};
use rag2_core::interfaces::{Answer, DecisionProvider, Embedder, Question, check_answers};
use rag2_core::{Error, Result};
use rag2_engine::components::ComponentFactory;
use std::sync::Arc;
use std::time::Duration;

/// The decision wire's request body (Aletheia `request_json`).
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

fn device(d: CfgDevice) -> Result<candle_core::Device> {
    match d {
        CfgDevice::Cpu => Ok(candle_core::Device::Cpu),
        CfgDevice::Metal => candle_core::Device::new_metal(0).map_err(|e| {
            Error::InvalidInput(format!(
                "metal device unavailable (build with --features metal): {e}"
            ))
        }),
        CfgDevice::Cuda => candle_core::Device::new_cuda(0).map_err(|e| {
            Error::InvalidInput(format!(
                "cuda device unavailable (build with --features cuda): {e}"
            ))
        }),
    }
}

/// A checkpoint loaded into this process.
pub struct LayaLocal {
    label: String,
    model: Laya,
}

impl LayaLocal {
    pub fn load(name: &str, checkpoint_dir: &str, dev: CfgDevice) -> Result<Self> {
        let started = std::time::Instant::now();
        let model = Laya::load(std::path::Path::new(checkpoint_dir), device(dev)?)
            .map_err(|e| Error::InvalidInput(format!("Laya checkpoint {checkpoint_dir}: {e}")))?;
        tracing::info!(
            model = name,
            dir = checkpoint_dir,
            secs = started.elapsed().as_secs_f64(),
            "Laya checkpoint loaded"
        );
        Ok(LayaLocal {
            label: format!("laya-local:{name}"),
            model,
        })
    }
}

impl DecisionProvider for LayaLocal {
    fn name(&self) -> &str {
        &self.label
    }
    fn healthy(&self) -> bool {
        true
    }
    fn decide(&self, state: &str, questions: &[Question]) -> Result<Vec<Answer>> {
        let out = self
            .model
            .decide(&request_json(state, questions))
            .map_err(Error::InvalidInput)?;
        parse_answers(&out, questions)
    }
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
    /// model (Aletheia ADR-052).
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

/// Builds Laya decision models from configuration.
pub struct LayaFactory;

impl ComponentFactory for LayaFactory {
    fn embedder(&self, _: &str, _: &EmbedderConfig) -> Result<Option<Arc<dyn Embedder>>> {
        Ok(None)
    }

    fn decision_model(
        &self,
        name: &str,
        cfg: &DecisionModelConfig,
    ) -> Result<Option<Arc<dyn DecisionProvider>>> {
        Ok(Some(match cfg {
            DecisionModelConfig::LayaLocal {
                checkpoint_dir,
                device,
            } => Arc::new(LayaLocal::load(name, checkpoint_dir, *device)?),
            DecisionModelConfig::LayaHttp {
                endpoint,
                serve_id,
                timeout_ms,
            } => Arc::new(LayaHttp::new(
                name,
                endpoint,
                serve_id,
                Duration::from_millis(*timeout_ms),
            )?),
        }))
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
    fn requests_use_the_aletheia_decision_wire() {
        let v = request_json("s", &questions());
        assert_eq!(v["state"], "s");
        assert_eq!(v["questions"][0]["type"], "choice");
        assert_eq!(v["questions"][0]["options"][1]["label"], "semantic");
        assert_eq!(v["questions"][1]["type"], "yesno");
        // The vendored parser accepts what the adapter sends.
        for q in v["questions"].as_array().expect("questions") {
            crate::parse_question(q).expect("Laya parses the adapter's questions");
        }
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

    /// Runs only with a real checkpoint: `RAG2_LAYA_CHECKPOINT=<dir> cargo test -p rag2-laya -- --ignored`.
    #[test]
    #[ignore = "needs RAG2_LAYA_CHECKPOINT"]
    fn a_real_checkpoint_answers_typed_questions() {
        let Ok(dir) = std::env::var("RAG2_LAYA_CHECKPOINT") else {
            return;
        };
        let m = LayaLocal::load("laya", &dir, CfgDevice::Cpu).expect("load");
        let a = m
            .decide("list all files in the current directory", &questions())
            .expect("decide");
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|x| (0.0..=1.0).contains(&x.confidence())));
    }
}

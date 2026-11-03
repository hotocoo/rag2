//! A System One decision server as a RAG2 [`DecisionProvider`]: Liquid AI's d1 models served by
//! llama.cpp (`llama-server -m d1-2B-Q8_0.gguf`, b11960 / v0.8.0 or newer) on `POST /v1/systemone`.
//!
//! Wire (captured from llama.cpp 0.8.0, fixture in `tests/fixtures/systemone_response.json`):
//! request `{state, questions: {key: {type: noul|choice, instructions, criteria?}}}`, response
//! `{answers: {key: {type, noul} | {type, choice, confidence, probabilities}}}`. One state per
//! request. At most `concurrency` requests are in flight per provider, across all queries; more wait
//! in a FIFO queue ([`crate::admission`]) and the wait counts against the request timeout.
//!
//! Mapping: `noul` is P(yes), so `yes = P >= NOUL_YES_THRESHOLD` and `confidence = max(P, 1-P)`
//! (the probability of the answer given). `choice` keeps the server's label and confidence.

use crate::admission::FairLimiter;
use rag2_core::interfaces::{Answer, DecisionProvider, Question, check_answers};
use rag2_core::{Error, Result};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// P(yes) at or above which a `noul` answer reads as yes.
pub const NOUL_YES_THRESHOLD: f64 = 0.5;

fn key(i: usize) -> String {
    format!("q{i}")
}

/// The System One request body for one state.
pub fn request_json(state: &str, questions: &[Question]) -> Value {
    let qs: Map<String, Value> = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let v = match q {
                Question::YesNo { instructions } => {
                    json!({"type": "noul", "instructions": instructions})
                }
                Question::Choice {
                    instructions,
                    options,
                } => {
                    // d1 reads each option through its description; a bare label describes itself.
                    let criteria: Map<String, Value> = options
                        .iter()
                        .map(|o| {
                            let d = if o.description.trim().is_empty() {
                                &o.label
                            } else {
                                &o.description
                            };
                            (o.label.clone(), Value::String(d.clone()))
                        })
                        .collect();
                    json!({"type": "choice", "instructions": instructions, "criteria": criteria})
                }
            };
            (key(i), v)
        })
        .collect();
    json!({ "state": state, "questions": qs })
}

/// Parse and validate a System One response. The server is untrusted: a missing answer, a wrong
/// type, a probability outside [0, 1] or a label that was not offered fails the whole call.
pub fn parse_answers(v: &Value, questions: &[Question]) -> Result<Vec<Answer>> {
    let off = |why: String| Error::Unavailable(format!("System One answered off-wire: {why}"));
    let answers = v["answers"]
        .as_object()
        .ok_or_else(|| off("no answers object".into()))?;
    let out = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let a = answers
                .get(&key(i))
                .ok_or_else(|| off(format!("missing {}", key(i))))?;
            match q {
                Question::YesNo { .. } => {
                    let p = a["noul"]
                        .as_f64()
                        .filter(|p| (0.0..=1.0).contains(p))
                        .ok_or_else(|| off(format!("{} has no noul in [0, 1]", key(i))))?;
                    Ok(Answer::YesNo {
                        yes: p >= NOUL_YES_THRESHOLD,
                        confidence: p.max(1.0 - p) as f32,
                    })
                }
                Question::Choice { .. } => {
                    let label = a["choice"]
                        .as_str()
                        .ok_or_else(|| off(format!("{} has no choice", key(i))))?;
                    let confidence = a["confidence"]
                        .as_f64()
                        .ok_or_else(|| off(format!("{} has no confidence", key(i))))?;
                    Ok(Answer::Choice {
                        label: label.to_string(),
                        confidence: confidence as f32,
                    })
                }
            }
        })
        .collect::<Result<Vec<_>>>()?;
    check_answers(questions, &out)?;
    Ok(out)
}

pub struct SystemOneHttp {
    label: String,
    endpoint: String,
    serve_id: String,
    concurrency: usize,
    timeout: Duration,
    limiter: FairLimiter,
    http: reqwest::blocking::Client,
}

impl SystemOneHttp {
    pub fn new(
        name: &str,
        endpoint: &str,
        serve_id: &str,
        timeout: Duration,
        concurrency: usize,
    ) -> Result<Self> {
        if concurrency == 0 {
            return Err(Error::InvalidInput(
                "System One concurrency must be > 0".into(),
            ));
        }
        Ok(SystemOneHttp {
            label: format!("systemone:{name}"),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            serve_id: serve_id.to_string(),
            concurrency,
            timeout,
            limiter: FairLimiter::new(concurrency),
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .pool_max_idle_per_host(concurrency)
                .build()
                .map_err(Error::internal)?,
        })
    }
}

impl DecisionProvider for SystemOneHttp {
    fn name(&self) -> &str {
        &self.label
    }

    /// Healthy means the server lists the configured model by id or alias: a port is not a model.
    fn healthy(&self) -> bool {
        let Ok(r) = self.http.get(format!("{}/v1/models", self.endpoint)).send() else {
            return false;
        };
        let Ok(v) = r.json::<Value>() else {
            return false;
        };
        let id = self.serve_id.as_str();
        v["data"].as_array().is_some_and(|d| {
            d.iter().any(|m| {
                m["id"] == id
                    || m["aliases"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| x == id))
            })
        })
    }

    fn decide(&self, state: &str, questions: &[Question]) -> Result<Vec<Answer>> {
        let unavailable = |e: String| Error::Unavailable(format!("{}: {e}", self.label));
        // One deadline for queueing and the request together.
        let deadline = Instant::now() + self.timeout;
        let _permit = self.limiter.acquire(deadline).ok_or_else(|| {
            unavailable(format!(
                "saturated: no free slot within {} ms",
                self.timeout.as_millis()
            ))
        })?;
        let resp = self
            .http
            .post(format!("{}/v1/systemone", self.endpoint))
            .timeout(deadline.saturating_duration_since(Instant::now()))
            .json(&request_json(state, questions))
            .send()
            .map_err(|e| unavailable(e.to_string()))?;
        let status = resp.status();
        let v: Value = resp.json().map_err(|e| unavailable(e.to_string()))?;
        if !status.is_success() {
            return Err(unavailable(format!(
                "refused ({status}): {}",
                v["error"]["message"].as_str().unwrap_or("unknown")
            )));
        }
        parse_answers(&v, questions)
    }

    /// Up to `concurrency` requests in flight (shared with every other caller); results keep the
    /// order of `states`.
    fn decide_each(&self, states: &[String], questions: &[Question]) -> Vec<Result<Vec<Answer>>> {
        let workers = self.concurrency.min(states.len());
        if workers <= 1 {
            return states.iter().map(|s| self.decide(s, questions)).collect();
        }
        let next = AtomicUsize::new(0);
        let mut done: Vec<(usize, Result<Vec<Answer>>)> = std::thread::scope(|sc| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    sc.spawn(|| {
                        let mut mine = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(s) = states.get(i) else { break };
                            mine.push((i, self.decide(s, questions)));
                        }
                        mine
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| {
                    h.join().unwrap_or_else(|_| vec![]) // a panicking worker loses its items; filled below
                })
                .collect()
        });
        done.sort_by_key(|(i, _)| *i);
        let mut it = done.into_iter().peekable();
        (0..states.len())
            .map(|i| match it.next_if(|(j, _)| *j == i) {
                Some((_, r)) => r,
                None => Err(Error::Unavailable(format!(
                    "{}: worker failed before answering",
                    self.label
                ))),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_core::interfaces::ChoiceOption;

    fn questions() -> Vec<Question> {
        vec![
            Question::YesNo {
                instructions: "Is the customer asking for a refund?".into(),
            },
            Question::Choice {
                instructions: "Which team should handle this?".into(),
                options: ["billing", "technical", "fraud"]
                    .iter()
                    .map(|l| ChoiceOption {
                        label: (*l).into(),
                        description: String::new(),
                    })
                    .collect(),
            },
        ]
    }

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/systemone_response.json"))
            .expect("fixture")
    }

    #[test]
    fn requests_key_questions_and_describe_bare_labels() {
        let v = request_json("s", &questions());
        assert_eq!(v["state"], "s");
        assert_eq!(v["questions"]["q0"]["type"], "noul");
        assert_eq!(v["questions"]["q1"]["type"], "choice");
        assert_eq!(v["questions"]["q1"]["criteria"]["fraud"], "fraud");
    }

    #[test]
    fn parses_the_captured_llama_cpp_response() {
        let a = parse_answers(&fixture(), &questions()).expect("parse");
        match &a[0] {
            Answer::YesNo { yes, confidence } => {
                assert!(*yes);
                assert!(*confidence > 0.99);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&a[1], Answer::Choice { label, .. } if label == "billing"));
    }

    #[test]
    fn low_p_yes_is_a_confident_no() {
        let v = json!({"answers": {"q0": {"type": "noul", "noul": 0.1}}});
        let a = parse_answers(&v, &questions()[..1]).expect("parse");
        assert!(
            matches!(a[0], Answer::YesNo { yes: false, confidence } if (confidence - 0.9).abs() < 1e-6)
        );
    }

    #[test]
    fn dishonest_answers_are_refused() {
        let q = questions();
        let mut bad = fixture();
        bad["answers"]["q1"]["choice"] = json!("legal");
        assert!(parse_answers(&bad, &q).is_err(), "label not offered");
        let mut bad = fixture();
        bad["answers"]["q0"]["noul"] = json!(1.5);
        assert!(parse_answers(&bad, &q).is_err(), "probability out of range");
        let mut bad = fixture();
        bad["answers"].as_object_mut().expect("obj").remove("q1");
        assert!(parse_answers(&bad, &q).is_err(), "missing answer");
        let mut bad = fixture();
        bad["answers"]["q1"]["confidence"] = json!(f64::NAN);
        assert!(parse_answers(&bad, &q).is_err(), "nan confidence");
    }

    /// Live d1 server, e.g. `llama-server -m d1-2B-Q8_0.gguf --alias d1-2B -np 1`, then
    /// `RAG2_SYSTEMONE=http://127.0.0.1:8081 cargo test -p rag2-decision systemone -- --ignored`.
    fn live() -> Option<SystemOneHttp> {
        let ep = std::env::var("RAG2_SYSTEMONE").ok()?;
        Some(SystemOneHttp::new("d1", &ep, "d1-2B", Duration::from_secs(60), 4).expect("client"))
    }

    #[test]
    #[ignore = "needs RAG2_SYSTEMONE (a live d1 server)"]
    fn live_d1_answers_typed_questions() {
        let Some(m) = live() else { return };
        assert!(m.healthy(), "server must list the d1-2B alias");
        let a = m
            .decide(
                "I was charged twice this month, please refund one of them.",
                &questions(),
            )
            .expect("decide");
        assert!(matches!(a[0], Answer::YesNo { yes: true, .. }));
        assert!(matches!(&a[1], Answer::Choice { label, .. } if label == "billing"));
    }

    #[test]
    #[ignore = "needs RAG2_SYSTEMONE (a live d1 server)"]
    fn live_d1_fan_out_keeps_state_order() {
        let Some(m) = live() else { return };
        let q = [Question::YesNo {
            instructions: "Does the message ask for a refund?".into(),
        }];
        let refund = "I was charged twice, please refund me.";
        let other = "The app crashes when I open the settings page.";
        let states: Vec<String> = (0..16)
            .map(|i| if i % 2 == 0 { refund } else { other }.to_string())
            .collect();
        let yes: Vec<bool> = m
            .decide_each(&states, &q)
            .into_iter()
            .map(|r| matches!(r.expect("answer")[0], Answer::YesNo { yes: true, .. }))
            .collect();
        let want: Vec<bool> = (0..16).map(|i| i % 2 == 0).collect();
        assert_eq!(yes, want);
    }

    #[test]
    #[ignore = "needs RAG2_SYSTEMONE (a live d1 server)"]
    fn live_d1_server_refusal_is_unavailable() {
        let Some(m) = live() else { return };
        let empty = [Question::Choice {
            instructions: "Pick one".into(),
            options: vec![],
        }];
        assert!(matches!(m.decide("x", &empty), Err(Error::Unavailable(_))));
    }
}

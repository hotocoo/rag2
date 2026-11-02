//! Versioned component interfaces (see [`crate::INTERFACE_VERSION`]). Implementations are chosen by
//! configuration; the engine depends only on these traits.

use crate::Result;
use crate::model::Hit;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An approximate-nearest-neighbour index over one tenant's chunk vectors.
///
/// Thread-safe: searches run concurrently with each other and with writes.
pub trait VectorIndex: Send + Sync {
    /// Implementation name, as configured (`"hnsw"`, `"flat"`, ...).
    fn kind(&self) -> &'static str;
    fn dim(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Insert or replace the vector for `id`. Dimension mismatches are `InvalidInput`.
    fn upsert(&self, id: u64, vector: &[f32]) -> Result<()>;
    fn remove(&self, id: u64) -> Result<bool>;
    /// The `k` nearest live ids by cosine similarity, best first. `ef` overrides the index's
    /// search breadth when it has one (a recall/latency knob set per pipeline).
    fn search(&self, query: &[f32], k: usize, ef: Option<usize>) -> Result<Vec<Hit>>;
    /// True when the index has degraded enough (e.g. tombstones) that a rebuild is due.
    fn needs_rebuild(&self) -> bool {
        false
    }
}

/// Turns text into vectors.
pub trait Embedder: Send + Sync {
    /// Identity recorded with stored vectors; a model change invalidates them.
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
    /// One unit-normalized vector per document text, in order.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// The vector for a query. Models trained with query/document prefixes override this.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(&[text])?
            .pop()
            .ok_or_else(|| crate::Error::Internal("embedder returned no vector".into()))
    }
}

/// Version of the plugin call envelope (`{hook, tenant, config, input}` -> output JSON).
pub const PLUGIN_ABI_VERSION: u32 = 1;

/// Extension hooks a plugin can implement.
pub mod hooks {
    /// Input: a document; output: the document (possibly changed) or `null` to drop it.
    pub const DOCUMENT_TRANSFORM: &str = "document_transform";
    /// Input: `{"query": "..."}`; output: `{"query": "..."}`.
    pub const QUERY_REWRITE: &str = "query_rewrite";
    /// Input: `{"query", "passages": [...]}`; output: `{"scores": [f32; passages]}`.
    pub const RERANK: &str = "rerank";
    pub const ALL: [&str; 3] = [DOCUMENT_TRANSFORM, QUERY_REWRITE, RERANK];
}

/// A sandboxed extension (Wasm). Calls are isolated from each other and from the host.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    /// Hooks the plugin declares in its manifest.
    fn hooks(&self) -> &[String];
    /// Invoke `hook` with `input`; `tenant` is passed through so plugins can vary by tenant.
    fn call(
        &self,
        hook: &str,
        tenant: &str,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value>;
}

/// One typed question for a decision model (Laya's wire, Aletheia ADR-186).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Pick exactly one option; the label comes back.
    Choice {
        instructions: String,
        options: Vec<ChoiceOption>,
    },
    /// Is the statement true?
    #[serde(rename = "yesno")]
    YesNo { instructions: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChoiceOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// One answer, with the model's calibrated confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Answer {
    Choice { label: String, confidence: f32 },
    YesNo { yes: bool, confidence: f32 },
}

impl Answer {
    pub fn confidence(&self) -> f32 {
        match self {
            Answer::Choice { confidence, .. } | Answer::YesNo { confidence, .. } => *confidence,
        }
    }
}

/// A non-generative decision model: a state and typed questions in, calibrated answers out, in
/// one pass. Mirrors Aletheia's `DecisionProvider`. An answer the provider could not give fails the
/// whole call; a partial answer set would let the caller act on half a decision.
pub trait DecisionProvider: Send + Sync {
    fn name(&self) -> &str;
    fn healthy(&self) -> bool;
    fn decide(&self, state: &str, questions: &[Question]) -> Result<Vec<Answer>>;
}

/// Validate a provider's answers against the questions asked: same count, offered labels only,
/// confidences in [0, 1]. Providers are untrusted.
pub fn check_answers(questions: &[Question], answers: &[Answer]) -> Result<()> {
    use crate::Error::Unavailable;
    if questions.len() != answers.len() {
        return Err(Unavailable(
            "decision model answered a different number of questions".into(),
        ));
    }
    for (q, a) in questions.iter().zip(answers) {
        if !(0.0..=1.0).contains(&a.confidence()) {
            return Err(Unavailable(
                "decision model confidence outside [0, 1]".into(),
            ));
        }
        match (q, a) {
            (Question::Choice { options, .. }, Answer::Choice { label, .. }) => {
                if !options.iter().any(|o| &o.label == label) {
                    return Err(Unavailable(
                        "decision model chose a label not offered".into(),
                    ));
                }
            }
            (Question::YesNo { .. }, Answer::YesNo { .. }) => {}
            _ => return Err(Unavailable("decision model answer type mismatch".into())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice() -> Question {
        Question::Choice {
            instructions: "route".into(),
            options: vec![
                ChoiceOption {
                    label: "a".into(),
                    description: String::new(),
                },
                ChoiceOption {
                    label: "b".into(),
                    description: String::new(),
                },
            ],
        }
    }

    #[test]
    fn questions_serialize_on_the_laya_wire() {
        let v = serde_json::to_value(choice()).expect("serialize");
        assert_eq!(v["type"], "choice");
        let yn = serde_json::to_value(Question::YesNo {
            instructions: "x".into(),
        })
        .expect("ser");
        assert_eq!(yn["type"], "yesno");
    }

    #[test]
    fn answers_are_checked_against_what_was_asked() {
        let q = [choice()];
        let ok = [Answer::Choice {
            label: "a".into(),
            confidence: 0.7,
        }];
        assert!(check_answers(&q, &ok).is_ok());
        let unoffered = [Answer::Choice {
            label: "z".into(),
            confidence: 0.7,
        }];
        assert!(check_answers(&q, &unoffered).is_err());
        let bad_conf = [Answer::Choice {
            label: "a".into(),
            confidence: 1.5,
        }];
        assert!(check_answers(&q, &bad_conf).is_err());
        let wrong_type = [Answer::YesNo {
            yes: true,
            confidence: 0.9,
        }];
        assert!(check_answers(&q, &wrong_type).is_err());
        assert!(check_answers(&q, &[]).is_err());
    }

    #[test]
    fn wire_answers_parse_untagged() {
        let a: Answer = serde_json::from_str(r#"{"yes":true,"confidence":0.8}"#).expect("parse");
        assert_eq!(
            a,
            Answer::YesNo {
                yes: true,
                confidence: 0.8
            }
        );
        let c: Answer = serde_json::from_str(r#"{"label":"b","confidence":0.5}"#).expect("parse");
        assert_eq!(c.confidence(), 0.5);
    }
}

//! Laya checkpoints run in this process as a RAG2 [`DecisionProvider`]. Questions go through the
//! decision wire's JSON (shared with the HTTP adapter in `rag2-decision`) and answers come back
//! validated against what was asked.

use crate::model::Laya;
use rag2_config::schema::{DecisionModelConfig, Device as CfgDevice, EmbedderConfig};
use rag2_core::interfaces::{Answer, DecisionProvider, Embedder, Question};
use rag2_core::{Error, Result};
use rag2_decision::laya_http::{parse_answers, request_json};
use rag2_engine::components::ComponentFactory;
use std::sync::Arc;

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

/// Builds in-process Laya decision models (`kind: laya_local`); other kinds are left to the
/// factories that handle them (`rag2_decision::DecisionFactory` for HTTP models).
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
        Ok(match cfg {
            DecisionModelConfig::LayaLocal {
                checkpoint_dir,
                device,
            } => Some(Arc::new(LayaLocal::load(name, checkpoint_dir, *device)?)),
            DecisionModelConfig::LayaHttp { .. } | DecisionModelConfig::SystemOne { .. } => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn requests_use_the_laya_decision_wire() {
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

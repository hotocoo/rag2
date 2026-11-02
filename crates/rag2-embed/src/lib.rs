//! Embedders selected by configuration:
//! - [`http::HttpEmbedder`]: any OpenAI-compatible embeddings API.
//! - [`candle::CandleEmbedder`]: a local ModernBERT-family sentence encoder (safetensors +
//!   tokenizer.json), run with candle on the encoder vendored with Laya.

pub mod candle;
pub mod http;

use rag2_config::schema::{DecisionModelConfig, EmbedderConfig};
use rag2_core::Result;
use rag2_core::interfaces::{DecisionProvider, Embedder};
use rag2_engine::components::ComponentFactory;
use std::sync::Arc;
use std::time::Duration;

/// Scale `v` to unit length in place (zero vectors stay zero).
pub(crate) fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 && n.is_finite() {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub struct EmbedFactory;

impl ComponentFactory for EmbedFactory {
    fn embedder(&self, name: &str, cfg: &EmbedderConfig) -> Result<Option<Arc<dyn Embedder>>> {
        Ok(match cfg {
            EmbedderConfig::Hash { .. } => None,
            EmbedderConfig::Http {
                endpoint,
                model,
                dim,
                api_key,
                timeout_ms,
                batch,
            } => {
                let key = api_key
                    .as_ref()
                    .map(|k| k.resolve().map_err(rag2_core::Error::InvalidInput))
                    .transpose()?;
                Some(Arc::new(http::HttpEmbedder::new(
                    name,
                    endpoint,
                    model,
                    *dim,
                    key,
                    Duration::from_millis(*timeout_ms),
                    *batch,
                )?))
            }
            EmbedderConfig::Candle {
                model_dir,
                device,
                pooling,
                batch,
                query_prefix,
                document_prefix,
                max_tokens,
            } => Some(Arc::new(candle::CandleEmbedder::load(
                name,
                candle::CandleOptions {
                    model_dir: model_dir.into(),
                    device: *device,
                    pooling: *pooling,
                    batch: *batch,
                    query_prefix: query_prefix.clone(),
                    document_prefix: document_prefix.clone(),
                    max_tokens: *max_tokens,
                },
            )?)),
        })
    }

    fn decision_model(
        &self,
        _: &str,
        _: &DecisionModelConfig,
    ) -> Result<Option<Arc<dyn DecisionProvider>>> {
        Ok(None)
    }
}

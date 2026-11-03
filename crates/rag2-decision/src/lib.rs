//! Decision models RAG2 reaches over HTTP: System One servers (Liquid AI d1 on llama.cpp,
//! [`systemone`]) and Laya sidecars ([`laya_http`]). No model runs in this crate, so a deployment
//! that only uses d1 builds without candle.

pub mod admission;
pub mod laya_http;
pub mod systemone;

use rag2_config::schema::{DecisionModelConfig, EmbedderConfig};
use rag2_core::Result;
use rag2_core::interfaces::{DecisionProvider, Embedder};
use rag2_engine::components::ComponentFactory;
use std::sync::Arc;
use std::time::Duration;

/// Builds `system_one` and `laya_http` decision models; `laya_local` is left to `rag2-laya`.
pub struct DecisionFactory;

impl ComponentFactory for DecisionFactory {
    fn embedder(&self, _: &str, _: &EmbedderConfig) -> Result<Option<Arc<dyn Embedder>>> {
        Ok(None)
    }

    fn decision_model(
        &self,
        name: &str,
        cfg: &DecisionModelConfig,
    ) -> Result<Option<Arc<dyn DecisionProvider>>> {
        Ok(match cfg {
            DecisionModelConfig::SystemOne {
                endpoint,
                serve_id,
                timeout_ms,
                concurrency,
            } => Some(Arc::new(systemone::SystemOneHttp::new(
                name,
                endpoint,
                serve_id,
                Duration::from_millis(*timeout_ms),
                *concurrency,
            )?)),
            DecisionModelConfig::LayaHttp {
                endpoint,
                serve_id,
                timeout_ms,
            } => Some(Arc::new(laya_http::LayaHttp::new(
                name,
                endpoint,
                serve_id,
                Duration::from_millis(*timeout_ms),
            )?)),
            DecisionModelConfig::LayaLocal { .. } => None,
        })
    }
}

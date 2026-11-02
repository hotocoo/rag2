//! Local sentence embeddings with a ModernBERT-family checkpoint (`config.json`,
//! `model.safetensors`, `tokenizer.json`) on candle, using the ModernBERT encoder vendored with
//! Laya. Pooling (`cls` or `mean`) and query/document prefixes follow the model card and are set
//! in configuration.

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use rag2_config::schema::{Device as CfgDevice, Pooling};
use rag2_core::interfaces::Embedder;
use rag2_core::{Error, Result};
use rag2_laya::encoder::{Config, ModernBert};
use std::path::PathBuf;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

pub struct CandleOptions {
    pub model_dir: PathBuf,
    pub device: CfgDevice,
    pub pooling: Pooling,
    pub batch: usize,
    pub query_prefix: String,
    pub document_prefix: String,
    pub max_tokens: usize,
}

pub struct CandleEmbedder {
    id: String,
    dim: usize,
    model: ModernBert,
    tok: Tokenizer,
    dev: Device,
    opts: CandleOptions,
}

fn ce(e: impl std::fmt::Display) -> Error {
    Error::InvalidInput(e.to_string())
}

/// Read a ModernBERT config, accepting the newer nested `rope_parameters` form (as Laya does).
pub fn read_config(dir: &std::path::Path) -> Result<Config> {
    let text = std::fs::read_to_string(dir.join("config.json"))
        .map_err(|e| ce(format!("config.json: {e}")))?;
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(ce)?;
    if v.get("global_rope_theta").is_none() {
        let rp = v["rope_parameters"].clone();
        v["global_rope_theta"] = rp["full_attention"]["rope_theta"].clone();
        v["local_rope_theta"] = rp["sliding_attention"]["rope_theta"].clone();
    }
    serde_json::from_value(v)
        .map_err(|e| ce(format!("config.json is not a ModernBERT config: {e}")))
}

impl CandleEmbedder {
    // Memory-mapping the safetensors file is the only unsafe operation (see SAFETY below).
    #[allow(unsafe_code)]
    pub fn load(name: &str, opts: CandleOptions) -> Result<Self> {
        if opts.batch == 0 || opts.max_tokens == 0 {
            return Err(ce("candle embedder needs batch > 0 and max_tokens > 0"));
        }
        let dir = &opts.model_dir;
        let cfg = read_config(dir)?;
        let dev = match opts.device {
            CfgDevice::Cpu => Device::Cpu,
            CfgDevice::Metal => Device::new_metal(0)
                .map_err(|e| ce(format!("metal: {e} (build with --features metal)")))?,
            CfgDevice::Cuda => Device::new_cuda(0)
                .map_err(|e| ce(format!("cuda: {e} (build with --features cuda)")))?,
        };
        let mut tok = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| ce(format!("tokenizer.json: {e}")))?;
        tok.with_truncation(Some(TruncationParams {
            max_length: opts.max_tokens,
            ..Default::default()
        }))
        .map_err(ce)?;
        tok.with_padding(Some(PaddingParams {
            pad_id: cfg.pad_token_id,
            ..Default::default()
        }));
        // SAFETY: the weights file is mapped read-only and must not be modified while the process
        // runs; models are replaced by pointing configuration at a new directory, never in place.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DType::F32, &dev)
        }
        .map_err(|e| ce(format!("model.safetensors: {e}")))?;
        // Bare `ModernBertModel` exports have no `model.` prefix; the encoder expects one.
        let vb = if vb.contains_tensor("model.embeddings.tok_embeddings.weight") {
            vb
        } else {
            vb.rename_f(|s: &str| s.strip_prefix("model.").unwrap_or(s).to_string())
        };
        let model = ModernBert::load(vb, &cfg).map_err(|e| ce(format!("loading encoder: {e}")))?;
        tracing::info!(embedder = name, dir = %dir.display(), dim = cfg.hidden_size, "candle embedder loaded");
        Ok(CandleEmbedder {
            id: format!("candle:{name}:{}:{}", dir.display(), cfg.hidden_size),
            dim: cfg.hidden_size,
            model,
            tok,
            dev,
            opts,
        })
    }

    fn run(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let enc = self.tok.encode_batch(texts.to_vec(), true).map_err(ce)?;
        let n = enc.len();
        let l = enc.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        if l == 0 {
            return Ok(vec![vec![0.0; self.dim]; n]);
        }
        let ids: Vec<u32> = enc.iter().flat_map(|e| e.get_ids().to_vec()).collect();
        let mask: Vec<u32> = enc
            .iter()
            .flat_map(|e| e.get_attention_mask().to_vec())
            .collect();
        let ids = Tensor::from_vec(ids, (n, l), &self.dev).map_err(ce)?;
        let mask_t = Tensor::from_vec(mask.clone(), (n, l), &self.dev).map_err(ce)?;
        let h = self.model.forward(&ids, &mask_t).map_err(ce)?; // (n, l, d)
        let pooled = match self.opts.pooling {
            Pooling::Cls => h.narrow(1, 0, 1).and_then(|t| t.squeeze(1)).map_err(ce)?,
            Pooling::Mean => {
                let m = mask_t
                    .to_dtype(DType::F32)
                    .and_then(|m| m.unsqueeze(2))
                    .map_err(ce)?; // (n, l, 1)
                let sum = h.broadcast_mul(&m).and_then(|x| x.sum(1)).map_err(ce)?;
                let count = m.sum(1).and_then(|c| c.clamp(1f32, f32::MAX)).map_err(ce)?;
                sum.broadcast_div(&count).map_err(ce)?
            }
        };
        let mut out: Vec<Vec<f32>> = pooled.to_vec2().map_err(ce)?;
        for v in out.iter_mut() {
            crate::normalize(v);
        }
        Ok(out)
    }

    fn with_prefix(&self, prefix: &str, texts: &[&str]) -> Vec<String> {
        texts.iter().map(|t| format!("{prefix}{t}")).collect()
    }
}

impl Embedder for CandleEmbedder {
    fn id(&self) -> &str {
        &self.id
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for group in texts.chunks(self.opts.batch) {
            out.extend(self.run(&self.with_prefix(&self.opts.document_prefix, group))?);
        }
        Ok(out)
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.run(&self.with_prefix(&self.opts.query_prefix, &[text]))?
            .pop()
            .ok_or_else(|| Error::Internal("no vector".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ModernBERT-base config cached on this machine parses (weights are not cached, so a
    /// full forward pass runs only with `RAG2_EMBED_MODEL_DIR`).
    #[test]
    fn modernbert_configs_parse_including_nested_rope() {
        let dir = std::env::temp_dir().join(format!("rag2-embed-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let cfg = serde_json::json!({
            "vocab_size": 50368, "hidden_size": 768, "num_hidden_layers": 22, "num_attention_heads": 12,
            "intermediate_size": 1152, "max_position_embeddings": 8192, "layer_norm_eps": 1e-5,
            "pad_token_id": 50283, "global_attn_every_n_layers": 3, "local_attention": 128,
            "rope_parameters": {"full_attention": {"rope_theta": 160000.0}, "sliding_attention": {"rope_theta": 10000.0}}
        });
        std::fs::write(dir.join("config.json"), cfg.to_string()).expect("write");
        let c = read_config(&dir).expect("config");
        assert_eq!(c.global_rope_theta, 160000.0);
        assert_eq!(c.hidden_size, 768);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[ignore = "needs RAG2_EMBED_MODEL_DIR with a ModernBERT sentence-embedding checkpoint"]
    fn a_real_checkpoint_embeds_similar_texts_closer() {
        let Ok(dir) = std::env::var("RAG2_EMBED_MODEL_DIR") else {
            return;
        };
        let e = CandleEmbedder::load(
            "t",
            CandleOptions {
                model_dir: dir.into(),
                device: CfgDevice::Cpu,
                pooling: Pooling::Mean,
                batch: 8,
                query_prefix: "search_query: ".into(),
                document_prefix: "search_document: ".into(),
                max_tokens: 256,
            },
        )
        .expect("load");
        let v = e
            .embed(&[
                "The pump failed.",
                "The pump broke down.",
                "Quarterly revenue rose.",
            ])
            .expect("embed");
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]));
    }
}

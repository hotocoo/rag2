//! Building embedders and decision models from configuration. Implementations register through
//! [`ComponentFactory`]; the engine never names a provider. The built-in factory handles the
//! `hash` embedder; the server binary adds factories for candle, HTTP and Laya.

use crate::text::words;
use parking_lot::Mutex;
use rag2_config::schema::{DecisionModelConfig, EmbedderConfig, PluginConfig, Profile};
use rag2_core::interfaces::{DecisionProvider, Embedder, Plugin};
use rag2_core::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

pub trait ComponentFactory: Send + Sync {
    /// Build the embedder, or `Ok(None)` when this factory does not handle its kind.
    fn embedder(&self, name: &str, cfg: &EmbedderConfig) -> Result<Option<Arc<dyn Embedder>>>;
    /// Build the decision model, or `Ok(None)` when this factory does not handle its kind.
    fn decision_model(
        &self,
        name: &str,
        cfg: &DecisionModelConfig,
    ) -> Result<Option<Arc<dyn DecisionProvider>>>;
    /// Build a plugin, or `Ok(None)` when this factory does not host plugins.
    fn plugin(&self, _name: &str, _cfg: &PluginConfig) -> Result<Option<Arc<dyn Plugin>>> {
        Ok(None)
    }
}

/// Signed feature hashing of word unigrams and bigrams, log-scaled term frequency, unit length.
/// No model and no training: a lexical-semantic baseline that keeps the dense path working with
/// zero setup, and a fallback for tests and air-gapped installs.
pub struct HashEmbedder {
    id: String,
    dim: usize,
}

/// Seed separating unigram from bigram hashes.
const BIGRAM_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

impl HashEmbedder {
    pub fn new(dim: usize) -> Result<Self> {
        if dim == 0 {
            return Err(Error::InvalidInput("hash embedder dim must be > 0".into()));
        }
        Ok(HashEmbedder {
            id: format!("hash-v1-{dim}"),
            dim,
        })
    }

    fn one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        let ws: Vec<String> = words(text).collect();
        let mut add = |h: u64| {
            let i = (h % self.dim as u64) as usize;
            v[i] += if h >> 63 == 0 { 1.0 } else { -1.0 };
        };
        for w in &ws {
            add(xxhash_rust::xxh3::xxh3_64(w.as_bytes()));
        }
        for pair in ws.windows(2) {
            let joined = format!("{} {}", pair[0], pair[1]);
            add(xxhash_rust::xxh3::xxh3_64_with_seed(
                joined.as_bytes(),
                BIGRAM_SEED,
            ));
        }
        for x in v.iter_mut() {
            *x = x.signum() * x.abs().ln_1p();
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }
}

impl Embedder for HashEmbedder {
    fn id(&self) -> &str {
        &self.id
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.one(t)).collect())
    }
}

pub struct BuiltinFactory;

impl ComponentFactory for BuiltinFactory {
    fn embedder(&self, _name: &str, cfg: &EmbedderConfig) -> Result<Option<Arc<dyn Embedder>>> {
        match cfg {
            EmbedderConfig::Hash { dim } => Ok(Some(Arc::new(HashEmbedder::new(*dim)?))),
            _ => Ok(None),
        }
    }
    fn decision_model(
        &self,
        _: &str,
        _: &DecisionModelConfig,
    ) -> Result<Option<Arc<dyn DecisionProvider>>> {
        Ok(None)
    }
}

/// The components an active profile needs, built once per activation.
#[derive(Clone, Default)]
pub struct Components {
    pub embedders: HashMap<String, Arc<dyn Embedder>>,
    pub decision_models: HashMap<String, Arc<dyn DecisionProvider>>,
    pub plugins: HashMap<String, Arc<dyn Plugin>>,
}

/// Builds [`Components`], reusing an instance when its configuration did not change (reloading a
/// profile must not reload unchanged models).
pub struct Registry {
    factories: Vec<Arc<dyn ComponentFactory>>,
    embedders: Mutex<HashMap<(String, String), Arc<dyn Embedder>>>,
    decisions: Mutex<HashMap<(String, String), Arc<dyn DecisionProvider>>>,
    plugins: Mutex<HashMap<(String, String), Arc<dyn Plugin>>>,
}

fn fingerprint<T: serde::Serialize>(name: &str, cfg: &T) -> (String, String) {
    (
        name.to_string(),
        serde_json::to_string(cfg).unwrap_or_default(),
    )
}

impl Registry {
    pub fn new(factories: Vec<Arc<dyn ComponentFactory>>) -> Self {
        Registry {
            factories,
            embedders: Mutex::new(HashMap::new()),
            decisions: Mutex::new(HashMap::new()),
            plugins: Mutex::new(HashMap::new()),
        }
    }

    pub fn build(&self, p: &Profile) -> Result<Components> {
        let mut out = Components::default();
        let mut emb_cache = self.embedders.lock();
        let mut next_emb = HashMap::new();
        for (name, cfg) in &p.embedders {
            let key = fingerprint(name, cfg);
            let e = match emb_cache.get(&key) {
                Some(e) => e.clone(),
                None => self
                    .make(|f| f.embedder(name, cfg))
                    .map_err(|e| ctx(e, "embedder", name))?,
            };
            out.embedders.insert(name.clone(), e.clone());
            next_emb.insert(key, e);
        }
        *emb_cache = next_emb;
        drop(emb_cache);
        let mut dec_cache = self.decisions.lock();
        let mut next_dec = HashMap::new();
        for (name, cfg) in &p.decision_models {
            let key = fingerprint(name, cfg);
            let d = match dec_cache.get(&key) {
                Some(d) => d.clone(),
                None => self
                    .make(|f| f.decision_model(name, cfg))
                    .map_err(|e| ctx(e, "decision model", name))?,
            };
            out.decision_models.insert(name.clone(), d.clone());
            next_dec.insert(key, d);
        }
        *dec_cache = next_dec;
        drop(dec_cache);
        let mut plug_cache = self.plugins.lock();
        let mut next_plug = HashMap::new();
        for (name, cfg) in &p.plugins {
            let key = fingerprint(name, cfg);
            let pl = match plug_cache.get(&key) {
                Some(x) => x.clone(),
                None => self
                    .make(|f| f.plugin(name, cfg))
                    .map_err(|e| ctx(e, "plugin", name))?,
            };
            out.plugins.insert(name.clone(), pl.clone());
            next_plug.insert(key, pl);
        }
        *plug_cache = next_plug;
        Ok(out)
    }

    fn make<T: ?Sized>(
        &self,
        f: impl Fn(&dyn ComponentFactory) -> Result<Option<Arc<T>>>,
    ) -> Result<Arc<T>> {
        for factory in &self.factories {
            if let Some(c) = f(factory.as_ref())? {
                return Ok(c);
            }
        }
        Err(Error::InvalidInput(
            "no installed component factory supports this kind (is the feature compiled in?)"
                .into(),
        ))
    }
}

fn ctx(e: Error, what: &str, name: &str) -> Error {
    Error::InvalidInput(format!("{what} {name:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_vector::simd::dot;

    #[test]
    fn hash_embeddings_are_unit_length_deterministic_and_lexically_similar() {
        let e = HashEmbedder::new(256).expect("embedder");
        let v = e
            .embed(&[
                "reactor coolant pump",
                "coolant pump reactor failure",
                "quarterly revenue",
            ])
            .expect("embed");
        assert!((dot(&v[0], &v[0]) - 1.0).abs() < 1e-5);
        assert_eq!(v[0], e.embed(&["reactor coolant pump"]).expect("embed")[0]);
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]));
        assert_eq!(e.embed(&[""]).expect("embed")[0], vec![0.0; 256]);
    }

    #[test]
    fn unchanged_components_are_reused_across_builds() {
        let r = Registry::new(vec![Arc::new(BuiltinFactory)]);
        let p = Profile::minimal("x");
        let a = r.build(&p).expect("build");
        let b = r.build(&p).expect("build");
        assert!(Arc::ptr_eq(
            &a.embedders["default"],
            &b.embedders["default"]
        ));
    }

    #[test]
    fn unsupported_kinds_name_the_component() {
        let r = Registry::new(vec![Arc::new(BuiltinFactory)]);
        let mut p = Profile::minimal("x");
        p.decision_models.insert(
            "laya".into(),
            DecisionModelConfig::LayaHttp {
                endpoint: "http://127.0.0.1:1".into(),
                serve_id: "laya".into(),
                timeout_ms: 1,
            },
        );
        let err = r.build(&p).err().expect("error");
        assert!(err.to_string().contains("decision model \"laya\""));
    }
}

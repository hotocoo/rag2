//! RAG2's engine with a live d1 server as relevance gate, in both gate modes. Ignored unless
//! `RAG2_SYSTEMONE` points at llama-server with d1 (`--alias d1-2B`):
//! `RAG2_SYSTEMONE=http://127.0.0.1:8081 cargo test -p rag2-laya --test live_d1_engine -- --ignored`

use rag2_config::schema::{DecisionGate, DecisionModelConfig, GateMode, Profile};
use rag2_config::{ConfigManager, FsConfigStore};
use rag2_core::model::{Document, Metadata};
use rag2_core::{DocumentId, TenantId};
use rag2_engine::{Engine, QueryRequest};
use std::sync::Arc;

const SERVE_ID: &str = "d1-2B";
const TIMEOUT_MS: u64 = 60_000;
const CONCURRENCY: usize = 4;
const MIN_CONFIDENCE: f32 = 0.7;

const DOCS: [(&str, &str); 3] = [
    (
        "seals",
        "Mechanical pump seals are inspected every 500 operating hours. Inspectors check the seal \
         faces for scoring and replace any seal that leaks more than ten drops per minute.",
    ),
    (
        "bearings",
        "Pump bearings are lubricated every 2,000 operating hours with lithium grease; \
         overheating bearings are replaced during the next planned shutdown.",
    ),
    (
        "cafeteria",
        "The staff cafeteria serves breakfast from seven to nine and lunch from twelve to two. \
         Vegetarian options are available every day.",
    ),
];

fn engine(endpoint: &str, mode: GateMode, dir: &std::path::Path) -> Arc<Engine> {
    let mut p = Profile::minimal("live");
    p.storage.data_dir = dir.join("data").to_string_lossy().into_owned();
    p.indexes.lexical.commit_interval_ms = 0;
    p.cache.query_entries = 0;
    p.decision_models.insert(
        "d1".into(),
        DecisionModelConfig::SystemOne {
            endpoint: endpoint.into(),
            serve_id: SERVE_ID.into(),
            timeout_ms: TIMEOUT_MS,
            concurrency: CONCURRENCY,
        },
    );
    let pl = p.pipelines.get_mut("default").expect("default pipeline");
    pl.top_k = DOCS.len();
    pl.abstention.enabled = false;
    pl.relevance_gate = Some(DecisionGate {
        model: "d1".into(),
        instructions: "Does the passage contain information that answers the query?".into(),
        min_confidence: MIN_CONFIDENCE,
        mode,
    });
    let store = Arc::new(FsConfigStore::open(dir.join("config")).expect("store"));
    let cfg = Arc::new(ConfigManager::open(store, "live", None, move || p, 2).expect("config"));
    Engine::open(cfg, vec![Arc::new(rag2_laya::adapter::LayaFactory)]).expect("engine")
}

fn gated_ids(mode: GateMode) -> Option<Vec<String>> {
    let endpoint = std::env::var("RAG2_SYSTEMONE").ok()?;
    let dir = tempfile::tempdir().expect("tempdir");
    let e = engine(&endpoint, mode, dir.path());
    let t = TenantId::new("plant").expect("tenant");
    let docs = DOCS
        .iter()
        .map(|(id, text)| Document {
            id: DocumentId::new(*id).expect("id"),
            title: None,
            text: (*text).into(),
            source: None,
            metadata: Metadata::new(),
        })
        .collect();
    e.ingest(&t, docs).expect("ingest");
    e.flush().expect("flush");
    let mut q = QueryRequest::new("How often are pump seals inspected?");
    q.no_cache = true;
    let r = e.query(&t, q).expect("query");
    assert!(
        r.warnings.iter().all(|w| !w.contains("d1")),
        "{:?}",
        r.warnings
    );
    Some(
        r.evidence
            .iter()
            .map(|x| x.provenance.doc_id.to_string())
            .collect(),
    )
}

fn check(mode: GateMode) {
    let Some(ids) = gated_ids(mode) else { return };
    assert!(ids.contains(&"seals".to_string()), "{mode:?} kept {ids:?}");
    assert!(
        !ids.contains(&"cafeteria".to_string()),
        "{mode:?} kept {ids:?}"
    );
}

#[test]
#[ignore = "needs RAG2_SYSTEMONE (a live d1 server)"]
fn per_item_gate_drops_the_unrelated_passage() {
    check(GateMode::PerItem);
}

#[test]
#[ignore = "needs RAG2_SYSTEMONE (a live d1 server)"]
fn joint_gate_drops_the_unrelated_passage() {
    check(GateMode::Joint);
}

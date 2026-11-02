//! Plugins wired through the engine: ingest transforms, query rewrites, reranking.

use rag2_config::schema::{PluginConfig, Profile};
use rag2_config::{ConfigManager, FsConfigStore};
use rag2_core::interfaces::hooks;
use rag2_core::model::{Document, Metadata};
use rag2_core::{DocumentId, TenantId};
use rag2_engine::{Engine, IngestStatus, QueryRequest};
use std::sync::Arc;

const MANIFEST_AT: u64 = 1024;
const OUTPUT_AT: u64 = 4096;

fn packed(at: u64, s: &str) -> i64 {
    ((at << 32) | s.len() as u64) as i64
}

fn wat_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A WAT plugin declaring `hooks`, whose `call` runs `body` (default: return `output`).
fn plugin_wat(hooks: &[&str], output: &str, body: Option<&str>, imports: &str) -> String {
    let manifest =
        serde_json::json!({ "name": "t", "version": "1.0.0", "hooks": hooks }).to_string();
    let body = body
        .map(String::from)
        .unwrap_or_else(|| format!("(i64.const {})", packed(OUTPUT_AT, output)));
    format!(
        r#"(module
  {imports}
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 16384))
  (data (i32.const {MANIFEST_AT}) "{m}")
  (data (i32.const {OUTPUT_AT}) "{o}")
  (func (export "alloc") (param $n i32) (result i32) (local $p i32)
    (local.set $p (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $n)))
    (if (i32.gt_u (global.get $next) (i32.mul (memory.size) (i32.const 65536)))
      (then (drop (memory.grow (i32.add (i32.div_u (local.get $n) (i32.const 65536)) (i32.const 1))))))
    (local.get $p))
  (func (export "rag2_abi_version") (result i32) (i32.const 1))
  (func (export "rag2_manifest") (result i64) (i64.const {mp}))
  (func (export "call") (param i32 i32) (result i64) {body}))"#,
        m = wat_str(&manifest),
        o = wat_str(output),
        mp = packed(MANIFEST_AT, &manifest),
    )
}

fn write(dir: &std::path::Path, name: &str, wat: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, wat).expect("write");
    p.to_string_lossy().into_owned()
}

fn plugin(path: String, hook: &str) -> PluginConfig {
    PluginConfig {
        path,
        sha256: None,
        hooks: vec![hook.into()],
        config: serde_json::Value::Null,
        fuel: 10_000_000,
        memory_mb: 4,
        timeout_ms: 200,
        allow_log: false,
    }
}

fn doc(id: &str, text: &str) -> Document {
    Document {
        id: DocumentId::new(id).expect("id"),
        title: None,
        text: text.into(),
        source: None,
        metadata: Metadata::new(),
    }
}

#[test]
fn plugins_transform_ingest_rewrite_queries_and_degrade_gracefully() {
    let d = tempfile::tempdir().expect("dir");
    let rewrite = write(
        d.path(),
        "rw.wat",
        &plugin_wat(
            &[hooks::QUERY_REWRITE],
            r#"{"query":"diesel generator"}"#,
            None,
            "",
        ),
    );
    let rerank = write(
        d.path(),
        "rr.wat",
        &plugin_wat(&[hooks::RERANK], r#"{"scores":[1.0]}"#, None, ""),
    );
    let mut p = Profile::minimal("t");
    p.storage.data_dir = d.path().join("data").to_string_lossy().into_owned();
    p.indexes.lexical.commit_interval_ms = 0;
    p.plugins
        .insert("rw".into(), plugin(rewrite, hooks::QUERY_REWRITE));
    p.plugins.insert("rr".into(), plugin(rerank, hooks::RERANK));
    let pl = p.pipelines.get_mut("default").expect("default");
    pl.plugins.query_rewrite = vec!["rw".into()];
    pl.plugins.rerank = Some("rr".into());
    let store = Arc::new(FsConfigStore::open(d.path().join("cfg")).expect("store"));
    let cfg = Arc::new(ConfigManager::open(store, "t", None, || p, 10).expect("config"));
    let e = Engine::open(cfg.clone(), vec![Arc::new(rag2_plugins::PluginFactory)]).expect("engine");
    let t = TenantId::new("acme").expect("t");
    e.ingest(
        &t,
        vec![
            doc("pump", "The coolant pump seals are inspected monthly."),
            doc(
                "backup",
                "The backup diesel generator starts in ten seconds.",
            ),
        ],
    )
    .expect("ingest");
    let r = e.query(&t, QueryRequest::new("pump seals")).expect("query");
    assert_eq!(r.query_rewritten.as_deref(), Some("diesel generator"));
    assert_eq!(r.evidence[0].provenance.doc_id.as_str(), "backup");
    // The rerank plugin returns one score for two passages: rejected, fused order kept, warned.
    assert!(
        r.warnings.iter().any(|w| w.contains("rerank skipped")),
        "{:?}",
        r.warnings
    );

    // An ingest transform that drops every document.
    let drop_all = write(
        d.path(),
        "drop.wat",
        &plugin_wat(&[hooks::DOCUMENT_TRANSFORM], "null", None, ""),
    );
    let mut p2 = (*cfg.current().profile).clone();
    p2.plugins
        .insert("drop".into(), plugin(drop_all, hooks::DOCUMENT_TRANSFORM));
    p2.ingest_plugins = vec!["drop".into()];
    cfg.apply(
        &serde_json::to_value(p2).expect("value"),
        "test",
        "add drop plugin",
    )
    .expect("apply");
    let rep = e
        .ingest(&t, vec![doc("secret", "do not store me")])
        .expect("ingest");
    assert_eq!(rep.documents[0].status, IngestStatus::Dropped);
    assert!(
        e.get_document(&t, &DocumentId::new("secret").expect("id"))
            .expect("get")
            .is_none()
    );
}

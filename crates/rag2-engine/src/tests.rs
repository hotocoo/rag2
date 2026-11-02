use super::*;
use rag2_config::FsConfigStore;
use rag2_config::schema::{Profile, RetrieverConfig};
use rag2_core::model::{AbstainReason, Document, Metadata};

struct Fixture {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
}

fn profile(dir: &std::path::Path) -> Profile {
    let mut p = Profile::minimal("test");
    p.storage.data_dir = dir.join("data").to_string_lossy().into_owned();
    p.indexes.lexical.commit_interval_ms = 0;
    p.indexes.lexical.writer_heap_mb = 16;
    p
}

fn fixture_with(edit: impl FnOnce(&mut Profile)) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut p = profile(dir.path());
    edit(&mut p);
    let store = Arc::new(FsConfigStore::open(dir.path().join("config")).expect("store"));
    let cfg = Arc::new(ConfigManager::open(store, "test", None, || p, 10).expect("config"));
    let engine = Engine::open(cfg, Vec::new()).expect("engine");
    Fixture { _dir: dir, engine }
}

fn fixture() -> Fixture {
    fixture_with(|_| {})
}

fn t(s: &str) -> TenantId {
    TenantId::new(s).expect("tenant")
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

fn corpus() -> Vec<Document> {
    vec![
        doc(
            "pump",
            "The reactor coolant pump circulates water through the core. Pump seals are inspected monthly.",
        ),
        doc(
            "sales",
            "Quarterly sales grew twelve percent, driven by the new subscription plan in Europe.",
        ),
        doc(
            "backup",
            "The backup diesel generator starts within ten seconds of a grid failure.",
        ),
        doc(
            "hr",
            "Employees accrue twenty days of paid leave per year, prorated for part-time staff.",
        ),
    ]
}

#[test]
fn hybrid_query_finds_the_relevant_document_with_provenance_and_context() {
    let f = fixture();
    let a = t("acme");
    let r = f.engine.ingest(&a, corpus()).expect("ingest");
    assert!(
        r.documents
            .iter()
            .all(|d| d.status == IngestStatus::Created)
    );
    let resp = f
        .engine
        .query(&a, QueryRequest::new("how often are pump seals inspected"))
        .expect("query");
    assert_eq!(resp.evidence[0].provenance.doc_id.as_str(), "pump");
    assert!(
        resp.evidence[0]
            .signals
            .iter()
            .any(|s| s.retriever == RetrieverKind::Lexical)
    );
    assert!(
        resp.evidence[0]
            .signals
            .iter()
            .any(|s| s.retriever == RetrieverKind::Dense)
    );
    assert!(
        resp.context
            .as_deref()
            .is_some_and(|c| c.starts_with("[1] (pump)"))
    );
    assert!(resp.confidence > 0.0 && resp.confidence <= 1.0);
    assert!(resp.timings.total_us > 0);
}

#[test]
fn tenants_never_see_each_others_documents() {
    let f = fixture();
    f.engine.ingest(&t("a"), corpus()).expect("ingest");
    f.engine
        .ingest(
            &t("b"),
            vec![doc(
                "other",
                "Completely unrelated text about gardening tomatoes.",
            )],
        )
        .expect("ingest");
    let resp = f
        .engine
        .query(&t("b"), QueryRequest::new("reactor coolant pump"))
        .expect("query");
    assert!(
        resp.evidence
            .iter()
            .all(|e| e.provenance.doc_id.as_str() == "other")
    );
    assert!(
        f.engine
            .get_document(&t("b"), &DocumentId::new("pump").expect("id"))
            .expect("get")
            .is_none()
    );
}

#[test]
fn unchanged_documents_are_skipped_and_updates_replace_old_chunks() {
    let f = fixture();
    let a = t("inc");
    f.engine.ingest(&a, corpus()).expect("ingest");
    let again = f.engine.ingest(&a, corpus()).expect("again");
    assert!(
        again
            .documents
            .iter()
            .all(|d| d.status == IngestStatus::Unchanged)
    );
    let upd = f
        .engine
        .ingest(
            &a,
            vec![doc(
                "pump",
                "Pump seals are now replaced every week by the night crew.",
            )],
        )
        .expect("update");
    assert_eq!(upd.documents[0].status, IngestStatus::Updated);
    assert_eq!(upd.documents[0].version, 2);
    let resp = f
        .engine
        .query(&a, QueryRequest::new("circulates water through the core"))
        .expect("query");
    assert!(
        resp.evidence.iter().all(|e| !e.text.contains("circulates")),
        "old chunk text must be gone from every index"
    );
    assert_eq!(f.engine.stats(&a).expect("stats").documents, 4);
}

#[test]
fn deleting_removes_from_every_index() {
    let f = fixture();
    let a = t("del");
    f.engine.ingest(&a, corpus()).expect("ingest");
    assert_eq!(
        f.engine
            .delete_documents(&a, &[DocumentId::new("backup").expect("id")])
            .expect("delete"),
        1
    );
    let resp = f
        .engine
        .query(&a, QueryRequest::new("diesel generator grid failure"))
        .expect("query");
    assert!(
        resp.evidence
            .iter()
            .all(|e| e.provenance.doc_id.as_str() != "backup")
    );
    let s = f.engine.stats(&a).expect("stats");
    assert_eq!(s.documents, 3);
    assert_eq!(s.vector_indexes[0].vectors as u64, s.lexical_docs);
}

#[test]
fn queries_with_no_support_abstain() {
    let f = fixture_with(|p| {
        p.pipelines
            .get_mut("default")
            .expect("default")
            .abstention
            .min_confidence = 0.6
    });
    let a = t("abs");
    f.engine.ingest(&a, corpus()).expect("ingest");
    let resp = f
        .engine
        .query(&a, QueryRequest::new("zebra migration patterns savanna"))
        .expect("query");
    let ab = resp.abstained.expect("should abstain");
    assert!(matches!(
        ab.reason,
        AbstainReason::LowConfidence | AbstainReason::NoEvidence
    ));
    assert!(resp.context.is_none());
    let empty = f
        .engine
        .query(&t("nobody"), QueryRequest::new("anything"))
        .expect("query");
    assert_eq!(
        empty.abstained.expect("abstain").reason,
        AbstainReason::NoEvidence
    );
}

#[test]
fn metadata_filters_restrict_results_and_drive_structured_retrieval() {
    let f = fixture_with(|p| {
        let pl = p.pipelines.get_mut("default").expect("default");
        pl.retrievers.push(RetrieverConfig {
            kind: RetrieverKind::Structured,
            embedder: None,
            top_k: 10,
            weight: 1.0,
            ef_search: None,
            enabled: true,
        });
    });
    let a = t("meta");
    let mut docs = corpus();
    docs[0]
        .metadata
        .insert("dept".into(), serde_json::json!("ops"));
    docs[2]
        .metadata
        .insert("dept".into(), serde_json::json!("ops"));
    docs[1]
        .metadata
        .insert("dept".into(), serde_json::json!("finance"));
    f.engine.ingest(&a, docs).expect("ingest");
    let mut req = QueryRequest::new("growth");
    req.filters.insert("dept".into(), serde_json::json!("ops"));
    let resp = f.engine.query(&a, req).expect("query");
    assert!(!resp.evidence.is_empty());
    assert!(
        resp.evidence
            .iter()
            .all(|e| ["pump", "backup"].contains(&e.provenance.doc_id.as_str()))
    );
    assert!(resp.evidence.iter().any(|e| {
        e.signals
            .iter()
            .any(|s| s.retriever == RetrieverKind::Structured)
    }));
}

#[test]
fn graph_expansion_reaches_adjacent_and_linked_chunks() {
    let f = fixture_with(|p| {
        p.chunking.max_chars = 60;
        p.chunking.overlap_chars = 0;
        p.chunking.min_chars = 5;
        let pl = p.pipelines.get_mut("default").expect("default");
        pl.retrievers.push(RetrieverConfig {
            kind: RetrieverKind::Graph,
            embedder: None,
            top_k: 10,
            weight: 1.0,
            ef_search: None,
            enabled: true,
        });
        pl.top_k = 10;
        pl.graph.seeds = 1;
    });
    let a = t("graph");
    let mut manual = doc(
        "manual",
        "Section one covers turbine startup procedures. Section two lists torque values for flange bolts.",
    );
    manual
        .metadata
        .insert("links".into(), serde_json::json!(["appendix"]));
    let appendix = doc("appendix", "Appendix with calibration certificates.");
    f.engine.ingest(&a, vec![manual, appendix]).expect("ingest");
    let resp = f
        .engine
        .query(&a, QueryRequest::new("turbine startup procedures"))
        .expect("query");
    let graph_found: Vec<&str> = resp
        .evidence
        .iter()
        .filter(|e| {
            e.signals
                .iter()
                .any(|s| s.retriever == RetrieverKind::Graph)
        })
        .map(|e| e.provenance.doc_id.as_str())
        .collect();
    assert!(
        graph_found.contains(&"manual"),
        "adjacent chunk via graph: {graph_found:?}"
    );
    assert!(
        graph_found.contains(&"appendix"),
        "linked document via graph: {graph_found:?}"
    );
}

#[test]
fn multi_hop_follows_terms_from_first_hop_evidence() {
    let f = fixture_with(|p| {
        let pl = p.pipelines.get_mut("default").expect("default");
        pl.multi_hop.max_hops = 1;
        pl.retrievers.retain(|r| r.kind == RetrieverKind::Lexical);
    });
    let a = t("hop");
    f.engine
        .ingest(
            &a,
            vec![
                doc("who", "Project Falcon is led by engineer Okonkwo."),
                doc(
                    "where",
                    "Okonkwo works from the Lagos office near the harbour.",
                ),
                doc("noise", "Cafeteria menu lists soup and bread."),
            ],
        )
        .expect("ingest");
    let mut req = QueryRequest::new("project falcon");
    req.hops = Some(1);
    let resp = f.engine.query(&a, req).expect("query");
    assert_eq!(resp.hops, 1);
    let ids: Vec<&str> = resp
        .evidence
        .iter()
        .map(|e| e.provenance.doc_id.as_str())
        .collect();
    assert!(
        ids.contains(&"where"),
        "second hop should reach the linked fact: {ids:?}"
    );
    assert!(
        resp.evidence
            .iter()
            .any(|e| e.signals.iter().any(|s| s.hop == 1))
    );
}

#[test]
fn responses_are_cached_until_the_tenant_changes() {
    let f = fixture();
    let a = t("cache");
    f.engine.ingest(&a, corpus()).expect("ingest");
    let q = QueryRequest::new("paid leave per year");
    assert!(!f.engine.query(&a, q.clone()).expect("first").cached);
    assert!(f.engine.query(&a, q.clone()).expect("second").cached);
    f.engine
        .ingest(&a, vec![doc("new", "Leave policy updated.")])
        .expect("ingest");
    assert!(!f.engine.query(&a, q).expect("after write").cached);
}

#[test]
fn cached_responses_do_not_outlive_a_pending_lexical_commit() {
    let f = fixture_with(|p| p.indexes.lexical.commit_interval_ms = 60_000);
    let a = t("nrt");
    f.engine.ingest(&a, corpus()).expect("ingest");
    let q = QueryRequest::new("paid leave per year");
    assert!(!f.engine.query(&a, q.clone()).expect("before commit").cached);
    f.engine.commit_lexical().expect("commit");
    let after = f.engine.query(&a, q).expect("after commit");
    assert!(
        !after.cached,
        "commit must invalidate responses computed before it"
    );
    assert!(
        after.evidence[0]
            .signals
            .iter()
            .any(|s| s.retriever == RetrieverKind::Lexical)
    );
}

#[test]
fn a_stale_lexical_index_is_rebuilt_from_storage_on_recovery() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = profile(dir.path());
    let store = Arc::new(FsConfigStore::open(dir.path().join("config")).expect("store"));
    let a = t("crash");
    {
        let cfg = Arc::new(
            ConfigManager::open(store.clone(), "test", None, || p.clone(), 10).expect("config"),
        );
        let e = Engine::open(cfg, Vec::new()).expect("engine");
        e.ingest(&a, corpus()).expect("ingest");
        e.flush().expect("flush");
    }
    // Simulate a crash that lost the lexical index.
    std::fs::remove_dir_all(dir.path().join("data/lexical/crash")).expect("remove lexical");
    let cfg = Arc::new(ConfigManager::open(store, "test", None, || p, 10).expect("config"));
    let e = Engine::open(cfg, Vec::new()).expect("engine");
    assert_eq!(e.recover_all().expect("recover"), 1);
    let resp = e
        .query(&a, QueryRequest::new("diesel generator"))
        .expect("query");
    assert_eq!(resp.evidence[0].provenance.doc_id.as_str(), "backup");
    assert_eq!(e.stats(&a).expect("stats").lexical_docs, 4);
}

#[test]
fn configuration_activation_changes_behaviour_without_restart() {
    let f = fixture();
    let a = t("live");
    f.engine.ingest(&a, corpus()).expect("ingest");
    assert!(
        f.engine
            .query(&a, QueryRequest::new("pump"))
            .expect("query")
            .evidence
            .len()
            > 1
    );
    let mut p = (*f.engine.config().current().profile).clone();
    p.pipelines.get_mut("default").expect("default").top_k = 1;
    f.engine
        .config()
        .apply(&serde_json::to_value(p).expect("value"), "test", "narrow")
        .expect("apply");
    assert_eq!(
        f.engine
            .query(&a, QueryRequest::new("pump"))
            .expect("query")
            .evidence
            .len(),
        1
    );
    // Storage settings are refused at runtime.
    let mut bad = (*f.engine.config().current().profile).clone();
    bad.storage.cache_mb += 1;
    assert!(
        f.engine
            .config()
            .apply(&serde_json::to_value(bad).expect("value"), "test", "")
            .is_err()
    );
}

#[test]
fn limits_are_enforced() {
    let f = fixture_with(|p| {
        p.limits.max_query_chars = 10;
        p.limits.max_document_bytes = 20;
    });
    let a = t("lim");
    assert!(
        f.engine
            .query(&a, QueryRequest::new("this query is too long"))
            .is_err()
    );
    assert!(
        f.engine
            .ingest(
                &a,
                vec![doc("big", "this document is longer than twenty bytes")]
            )
            .is_err()
    );
    let mut r = QueryRequest::new("ok");
    r.top_k = Some(10_000);
    assert!(f.engine.query(&a, r).is_err());
}

fn node(role: rag2_config::schema::ClusterRole) -> Fixture {
    fixture_with(|p| {
        p.cluster.role = role;
        p.cluster.replication_key = Some(rag2_config::schema::SecretRef::Env("UNUSED".into()));
        p.cluster.primary_url = Some("http://127.0.0.1:1".into());
    })
}

fn replicate(primary: &Engine, replica: &Engine, t: &TenantId) -> usize {
    let head = primary.log_head(t).expect("head");
    let after = match replica.replication_position(t).expect("pos") {
        p if p.epoch == head.epoch => p.seq,
        _ => 0,
    };
    let entries = primary.read_log(t, after, 1_000).expect("log");
    replica
        .apply_replicated(t, head.epoch, &entries)
        .expect("apply")
}

#[test]
fn a_replica_serves_the_same_answers_as_its_primary() {
    use rag2_config::schema::ClusterRole;
    let (p, r) = (node(ClusterRole::Primary), node(ClusterRole::Replica));
    let a = t("repl");
    p.engine.ingest(&a, corpus()).expect("ingest");
    assert!(replicate(&p.engine, &r.engine, &a) > 0);
    let q = QueryRequest::new("how often are pump seals inspected");
    let (pr, rr) = (
        p.engine.query(&a, q.clone()).expect("primary"),
        r.engine.query(&a, q.clone()).expect("replica"),
    );
    assert_eq!(
        pr.evidence[0].provenance.doc_id,
        rr.evidence[0].provenance.doc_id
    );
    assert_eq!(
        pr.evidence[0].chunk_id, rr.evidence[0].chunk_id,
        "chunk ids are preserved"
    );
    // Updates and deletes follow; a second pass applies nothing new.
    p.engine
        .delete_documents(&a, &[DocumentId::new("pump").expect("id")])
        .expect("delete");
    replicate(&p.engine, &r.engine, &a);
    assert_eq!(replicate(&p.engine, &r.engine, &a), 0);
    let after = r.engine.query(&a, q).expect("replica after delete");
    assert!(
        after
            .evidence
            .iter()
            .all(|e| e.provenance.doc_id.as_str() != "pump")
    );
    assert_eq!(r.engine.stats(&a).expect("stats").documents, 3);
}

#[test]
fn a_recreated_tenant_on_the_primary_resets_the_replica_copy() {
    use rag2_config::schema::ClusterRole;
    let (p, r) = (node(ClusterRole::Primary), node(ClusterRole::Replica));
    let a = t("epoch");
    p.engine.ingest(&a, corpus()).expect("ingest");
    replicate(&p.engine, &r.engine, &a);
    p.engine.delete_tenant(&a).expect("delete tenant");
    p.engine
        .ingest(
            &a,
            vec![doc("only", "A single fresh document about turbines.")],
        )
        .expect("ingest");
    replicate(&p.engine, &r.engine, &a);
    assert_eq!(r.engine.stats(&a).expect("stats").documents, 1);
    assert!(
        r.engine
            .get_document(&a, &DocumentId::new("pump").expect("id"))
            .expect("get")
            .is_none()
    );
}

#[test]
fn source_sync_adds_updates_and_deletes_only_its_own_documents() {
    let f = fixture();
    let a = t("sync");
    f.engine
        .ingest(
            &a,
            vec![doc("manual", "Uploaded by hand, not by the connector.")],
        )
        .expect("ingest");
    let r1 = f
        .engine
        .sync_source(
            &a,
            "wiki",
            vec![doc("w1", "first page"), doc("w2", "second page")],
            true,
        )
        .expect("sync");
    assert_eq!((r1.created, r1.deleted), (2, 0));
    let r2 = f
        .engine
        .sync_source(&a, "wiki", vec![doc("w1", "first page, edited")], true)
        .expect("sync");
    assert_eq!((r2.updated, r2.deleted), (1, 1));
    assert!(
        f.engine
            .get_document(&a, &DocumentId::new("w2").expect("id"))
            .expect("get")
            .is_none()
    );
    assert!(
        f.engine
            .get_document(&a, &DocumentId::new("manual").expect("id"))
            .expect("get")
            .is_some()
    );
}

use super::*;
use rag2_core::model::Metadata;

fn open(dir: &std::path::Path, backend: BackendKind, key: Option<Vec<u8>>) -> Storage {
    Storage::open(StorageOptions {
        backend,
        path: dir.to_path_buf(),
        cache_bytes: 8 << 20,
        sync_writes: false,
        encryption_key: key,
        replication_log: false,
    })
    .expect("open storage")
}

fn t(s: &str) -> TenantId {
    TenantId::new(s).expect("tenant")
}

fn doc(id: &str, chunks: &[u64]) -> DocumentRecord {
    DocumentRecord {
        id: DocumentId::new(id).expect("doc id"),
        title: None,
        source: None,
        metadata: Metadata::new(),
        version: 1,
        content_hash: 9,
        chunks: chunks.iter().map(|c| ChunkId(*c)).collect(),
        embedders: vec!["e".into()],
        ingested_at_ms: 0,
    }
}

fn chunk(doc: &str, id: u64, text: &str) -> Chunk {
    Chunk {
        id: ChunkId(id),
        doc_id: DocumentId::new(doc).expect("doc id"),
        ordinal: 0,
        text: text.into(),
        start: 0,
        end: text.len() as u32,
        hash: 0,
    }
}

fn each_backend(f: impl Fn(&dyn Fn(Option<Vec<u8>>) -> (tempfile::TempDir, Storage))) {
    for b in [BackendKind::Fjall, BackendKind::Redb] {
        f(&move |key| {
            let dir = tempfile::tempdir().expect("tempdir");
            let s = open(dir.path(), b, key);
            (dir, s)
        });
    }
}

#[test]
fn documents_chunks_and_vectors_round_trip_atomically() {
    each_backend(|mk| {
        let (_d, s) = mk(None);
        let a = t("acme");
        s.put_document(
            &a,
            &doc("d1", &[1, 2]),
            &[chunk("d1", 1, "x"), chunk("d1", 2, "y")],
            &[("e", ChunkId(1), vec![1.0, 2.0])],
        )
        .expect("put");
        assert_eq!(
            s.get_document(&a, &DocumentId::new("d1").expect("id"))
                .expect("get")
                .expect("some")
                .chunks
                .len(),
            2
        );
        assert_eq!(
            s.get_chunk(&a, ChunkId(2))
                .expect("chunk")
                .expect("some")
                .text,
            "y"
        );
        assert_eq!(
            s.get_vector(&a, "e", ChunkId(1)).expect("vec"),
            Some(vec![1.0, 2.0])
        );
        assert_eq!(s.document_count(&a).expect("count"), 1);

        // Replacing drops the old chunks in the same batch.
        let (old, g) = s
            .put_document(&a, &doc("d1", &[3]), &[chunk("d1", 3, "z")], &[])
            .expect("replace");
        assert!(old.is_some());
        assert_eq!(g, 2);
        assert_eq!(s.generation(&a).expect("gen"), 2);
        assert!(s.get_chunk(&a, ChunkId(1)).expect("chunk").is_none());
        assert!(s.get_vector(&a, "e", ChunkId(1)).expect("vec").is_none());
        assert_eq!(s.document_count(&a).expect("count"), 1);

        s.delete_document(&a, &DocumentId::new("d1").expect("id"))
            .expect("delete");
        assert!(s.get_chunk(&a, ChunkId(3)).expect("chunk").is_none());
        assert_eq!(s.document_count(&a).expect("count"), 0);
    });
}

#[test]
fn one_tenant_can_never_read_or_list_another_tenants_data() {
    each_backend(|mk| {
        let (_d, s) = mk(None);
        let (a, b, ab) = (t("a"), t("b"), t("ab"));
        s.put_document(
            &a,
            &doc("same", &[1]),
            &[chunk("same", 1, "tenant a secret")],
            &[("e", ChunkId(1), vec![1.0])],
        )
        .expect("put a");
        s.put_document(&ab, &doc("x", &[1]), &[chunk("x", 1, "tenant ab")], &[])
            .expect("put ab");
        let id = DocumentId::new("same").expect("id");
        assert!(s.get_document(&b, &id).expect("get").is_none());
        assert!(s.get_chunk(&b, ChunkId(1)).expect("chunk").is_none());
        assert!(s.list_documents(&b, None, 100).expect("list").is_empty());
        assert!(
            s.scan_vectors(&b, "e", None, 100)
                .expect("vectors")
                .is_empty()
        );
        // "a" must not see "ab" even though "a" is a string prefix of "ab".
        let listed = s.list_documents(&a, None, 100).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id.as_str(), "same");
        assert_eq!(s.scan_chunks(&a, None, 100).expect("chunks").len(), 1);
        // Deleting a tenant leaves the other intact.
        s.delete_tenant(&a).expect("delete tenant");
        assert!(s.get_chunk(&a, ChunkId(1)).expect("chunk").is_none());
        assert_eq!(
            s.get_chunk(&ab, ChunkId(1))
                .expect("chunk")
                .expect("ab kept")
                .text,
            "tenant ab"
        );
        let tenants: Vec<String> = s
            .tenants()
            .expect("tenants")
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(tenants, vec!["ab".to_string()]);
    });
}

#[test]
fn listing_pages_in_id_order() {
    each_backend(|mk| {
        let (_d, s) = mk(None);
        let a = t("p");
        for i in 0..5 {
            s.put_document(&a, &doc(&format!("d{i}"), &[]), &[], &[])
                .expect("put");
        }
        let first = s.list_documents(&a, None, 2).expect("page 1");
        let second = s.list_documents(&a, Some(&first[1].id), 2).expect("page 2");
        let ids: Vec<&str> = first.iter().chain(&second).map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["d0", "d1", "d2", "d3"]);
    });
}

#[test]
fn chunk_ids_are_never_reused_across_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = t("c");
    let r1 = open(dir.path(), BackendKind::Fjall, None)
        .allocate_chunk_ids(&a, 10)
        .expect("alloc");
    let r2 = open(dir.path(), BackendKind::Fjall, None)
        .allocate_chunk_ids(&a, 5)
        .expect("alloc");
    assert_eq!((r1, r2), (0..10, 10..15));
}

#[test]
fn encrypted_values_are_unreadable_without_the_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = t("enc");
    {
        let s = open(dir.path(), BackendKind::Fjall, Some(vec![1u8; 32]));
        s.put_document(
            &a,
            &doc("d", &[1]),
            &[chunk("d", 1, "classified text")],
            &[("e", ChunkId(1), vec![0.5])],
        )
        .expect("put");
        assert_eq!(
            s.get_chunk(&a, ChunkId(1))
                .expect("chunk")
                .expect("some")
                .text,
            "classified text"
        );
        s.persist().expect("persist");
    }
    {
        let wrong = open(dir.path(), BackendKind::Fjall, Some(vec![2u8; 32]));
        assert!(wrong.get_chunk(&a, ChunkId(1)).is_err());
    }
    let plain = open(dir.path(), BackendKind::Fjall, None);
    assert!(
        plain.get_chunk(&a, ChunkId(1)).is_err(),
        "ciphertext must not parse as plaintext"
    );
}

#[test]
fn audit_events_read_back_in_order_with_a_cursor() {
    each_backend(|mk| {
        let (_d, s) = mk(Some(vec![3u8; 32]));
        for i in 0..3u64 {
            s.append_audit(1_000 + i, &serde_json::json!({ "i": i }))
                .expect("append");
        }
        let page = s.scan_audit(None, 2).expect("scan");
        assert_eq!(page.len(), 2);
        let rest = s.scan_audit(Some(&page[1].0), 10).expect("scan");
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].1["i"], 2);
    });
}

#[test]
fn metadata_index_finds_documents_and_follows_replacement() {
    each_backend(|mk| {
        let (_d, s) = mk(None);
        let a = t("m");
        let mut r = doc("d1", &[]);
        r.metadata.insert("team".into(), serde_json::json!("infra"));
        r.metadata
            .insert("tags".into(), serde_json::json!(["x", "y"]));
        s.put_document(&a, &r, &[], &[]).expect("put");
        let find =
            |f: &str, v: serde_json::Value| s.documents_with(&a, f, &v, 10).expect("find").len();
        assert_eq!(find("team", serde_json::json!("infra")), 1);
        assert_eq!(find("tags", serde_json::json!("y")), 1);
        r.metadata.insert("team".into(), serde_json::json!("data"));
        s.put_document(&a, &r, &[], &[]).expect("replace");
        assert_eq!(find("team", serde_json::json!("infra")), 0);
        assert_eq!(find("team", serde_json::json!("data")), 1);
        s.delete_document(&a, &r.id).expect("delete");
        assert_eq!(find("team", serde_json::json!("data")), 0);
        assert_eq!(find("tags", serde_json::json!("x")), 0);
    });
}

//! The shipped example plugin (examples/plugins/redact-emails) against the real host. Skips when
//! the example has not been built (`cargo build --release --target wasm32-unknown-unknown`).

use rag2_config::schema::PluginConfig;
use rag2_core::interfaces::{Plugin, hooks};
use rag2_plugins::WasmPlugin;

#[test]
fn the_redaction_example_redacts_and_drops() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins/redact-emails/target/wasm32-unknown-unknown/release/redact_emails.wasm");
    if !path.exists() {
        eprintln!(
            "skipping: build the example plugin first ({})",
            path.display()
        );
        return;
    }
    let cfg = PluginConfig {
        path: path.to_string_lossy().into_owned(),
        sha256: None,
        hooks: vec![hooks::DOCUMENT_TRANSFORM.into()],
        config: serde_json::json!({ "drop_if_contains": "CONFIDENTIAL" }),
        fuel: 50_000_000,
        memory_mb: 16,
        timeout_ms: 500,
        allow_log: false,
    };
    let p = WasmPlugin::load("redact", &cfg).expect("load example");
    let doc = serde_json::json!({ "id": "d", "text": "Contact ana@corp.example.com for access." });
    let out = p
        .call(hooks::DOCUMENT_TRANSFORM, "acme", &doc)
        .expect("call");
    assert_eq!(out["text"], "Contact [redacted email] for access.");
    assert_eq!(out["id"], "d");
    let secret = serde_json::json!({ "id": "s", "text": "CONFIDENTIAL board minutes" });
    assert!(
        p.call(hooks::DOCUMENT_TRANSFORM, "acme", &secret)
            .expect("call")
            .is_null()
    );
}

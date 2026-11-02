use super::*;
use rag2_core::interfaces::hooks;

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

fn cfg(path: String, hooks: &[&str]) -> PluginConfig {
    PluginConfig {
        path,
        sha256: None,
        hooks: hooks.iter().map(|h| h.to_string()).collect(),
        config: serde_json::json!({ "k": 1 }),
        fuel: 10_000_000,
        memory_mb: 4,
        timeout_ms: 200,
        allow_log: false,
    }
}

#[test]
fn a_plugin_answers_its_hook_and_is_refused_others() {
    let d = tempfile::tempdir().expect("dir");
    let out = r#"{"query":"rewritten query"}"#;
    let path = write(
        d.path(),
        "rw.wat",
        &plugin_wat(&[hooks::QUERY_REWRITE], out, None, ""),
    );
    let p = WasmPlugin::load("rw", &cfg(path, &[hooks::QUERY_REWRITE])).expect("load");
    let r = p
        .call(
            hooks::QUERY_REWRITE,
            "acme",
            &serde_json::json!({ "query": "q" }),
        )
        .expect("call");
    assert_eq!(r["query"], "rewritten query");
    assert!(
        p.call(hooks::RERANK, "acme", &serde_json::json!({}))
            .is_err()
    );
}

#[test]
fn hooks_the_manifest_does_not_declare_cannot_be_granted() {
    let d = tempfile::tempdir().expect("dir");
    let path = write(
        d.path(),
        "p.wat",
        &plugin_wat(&[hooks::QUERY_REWRITE], "{}", None, ""),
    );
    assert!(WasmPlugin::load("p", &cfg(path, &[hooks::RERANK])).is_err());
}

#[test]
fn runaway_loops_hit_the_instruction_budget() {
    let d = tempfile::tempdir().expect("dir");
    let path = write(
        d.path(),
        "spin.wat",
        &plugin_wat(
            &[hooks::RERANK],
            "{}",
            Some("(loop $l (br $l)) (i64.const 0)"),
            "",
        ),
    );
    let p = WasmPlugin::load("spin", &cfg(path, &[hooks::RERANK])).expect("load");
    let err = p
        .call(hooks::RERANK, "t", &serde_json::json!({}))
        .expect_err("must stop");
    assert!(err.to_string().contains("instruction budget"), "{err}");
}

#[test]
fn slow_plugins_hit_the_wall_clock_deadline() {
    let d = tempfile::tempdir().expect("dir");
    let path = write(
        d.path(),
        "slow.wat",
        &plugin_wat(
            &[hooks::RERANK],
            "{}",
            Some("(loop $l (br $l)) (i64.const 0)"),
            "",
        ),
    );
    let mut c = cfg(path, &[hooks::RERANK]);
    c.fuel = u64::MAX / 2;
    c.timeout_ms = 30;
    let p = WasmPlugin::load("slow", &c).expect("load");
    let started = std::time::Instant::now();
    let err = p
        .call(hooks::RERANK, "t", &serde_json::json!({}))
        .expect_err("must stop");
    assert!(err.to_string().contains("time limit"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn memory_beyond_the_quota_is_refused() {
    let d = tempfile::tempdir().expect("dir");
    let hog = "(if (i32.eq (memory.grow (i32.const 1024)) (i32.const -1)) (then unreachable)) (i64.const 0)";
    let path = write(
        d.path(),
        "hog.wat",
        &plugin_wat(&[hooks::RERANK], "{}", Some(hog), ""),
    );
    let p = WasmPlugin::load("hog", &cfg(path, &[hooks::RERANK])).expect("load");
    assert!(p.call(hooks::RERANK, "t", &serde_json::json!({})).is_err());
}

#[test]
fn imports_are_permissions() {
    let d = tempfile::tempdir().expect("dir");
    let wat = plugin_wat(
        &[hooks::RERANK],
        "{}",
        None,
        r#"(import "env" "log" (func $log (param i32 i32)))"#,
    );
    let path = write(d.path(), "log.wat", &wat);
    assert!(
        WasmPlugin::load("log", &cfg(path.clone(), &[hooks::RERANK])).is_err(),
        "log not granted"
    );
    let mut c = cfg(path, &[hooks::RERANK]);
    c.allow_log = true;
    assert!(WasmPlugin::load("log", &c).is_ok());
    let wasi = plugin_wat(
        &[hooks::RERANK],
        "{}",
        None,
        r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#,
    );
    let path = write(d.path(), "wasi.wat", &wasi);
    assert!(
        WasmPlugin::load("wasi", &cfg(path, &[hooks::RERANK])).is_err(),
        "no WASI"
    );
}

#[test]
fn integrity_pins_are_checked() {
    let d = tempfile::tempdir().expect("dir");
    let wat = plugin_wat(&[hooks::RERANK], "{}", None, "");
    let path = write(d.path(), "pin.wat", &wat);
    let good: String = sha2::Sha256::digest(wat.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut c = cfg(path, &[hooks::RERANK]);
    c.sha256 = Some("0".repeat(64));
    assert!(WasmPlugin::load("pin", &c).is_err());
    c.sha256 = Some(good);
    assert!(WasmPlugin::load("pin", &c).is_ok());
}

#[test]
fn plugin_errors_surface_as_errors() {
    let d = tempfile::tempdir().expect("dir");
    let path = write(
        d.path(),
        "e.wat",
        &plugin_wat(&[hooks::RERANK], r#"{"error":"bad input"}"#, None, ""),
    );
    let p = WasmPlugin::load("e", &cfg(path, &[hooks::RERANK])).expect("load");
    assert!(
        p.call(hooks::RERANK, "t", &serde_json::json!({}))
            .expect_err("error")
            .to_string()
            .contains("bad input")
    );
}

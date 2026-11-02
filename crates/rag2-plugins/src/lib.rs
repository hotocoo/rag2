//! Sandboxed Wasm plugins on Wasmtime.
//!
//! # ABI (version [`rag2_core::interfaces::PLUGIN_ABI_VERSION`])
//!
//! A plugin is a core Wasm module exporting:
//! - `memory`
//! - `alloc(len: i32) -> i32`: a buffer the host writes the call envelope into
//! - `rag2_abi_version() -> i32`
//! - `rag2_manifest() -> i64`: `(ptr << 32) | len` of JSON `{"name", "version", "hooks": [...]}`
//! - `call(ptr: i32, len: i32) -> i64`: input is JSON `{"abi", "hook", "tenant", "config",
//!   "input"}`; output `(ptr << 32) | len` of the hook's JSON result, or `{"error": "..."}`
//!
//! The only import a plugin may use is `env.log(ptr: i32, len: i32)`, and only when the profile
//! sets `allow_log`. There is no WASI: no files, network, clock or environment.
//!
//! # Isolation and quotas
//!
//! Every call gets a fresh instance (no state survives between calls or tenants), a fuel budget
//! (instructions), a memory ceiling, and a wall-clock deadline enforced by epoch interruption.

use rag2_config::schema::PluginConfig;
use rag2_core::interfaces::{PLUGIN_ABI_VERSION, Plugin};
use rag2_core::{Error, Result};
use sha2::Digest;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use wasmtime::{
    Caller, Config, Engine, InstancePre, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};

/// Epoch tick; deadlines are rounded up to whole ticks.
const EPOCH_TICK: Duration = Duration::from_millis(5);
/// Longest log line a plugin may emit.
const MAX_LOG_BYTES: usize = 4 * 1024;
/// Largest JSON a plugin may return.
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Fuel for reading the manifest at load time.
const MANIFEST_FUEL: u64 = 10_000_000;
const BYTES_PER_MB: u64 = 1024 * 1024;

struct State {
    limits: StoreLimits,
    plugin: String,
}

/// One engine per process; a background thread advances its epoch for deadlines.
fn engine() -> Result<&'static Engine> {
    static ENGINE: OnceLock<std::result::Result<Engine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut c = Config::new();
            c.consume_fuel(true).epoch_interruption(true);
            let e = Engine::new(&c).map_err(|e| e.to_string())?;
            let ticker = e.clone();
            std::thread::Builder::new()
                .name("rag2-plugin-epoch".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(EPOCH_TICK);
                        ticker.increment_epoch();
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(e)
        })
        .as_ref()
        .map_err(|e| Error::Internal(format!("wasm engine: {e}")))
}

fn split(packed: i64) -> (usize, usize) {
    let p = packed as u64;
    ((p >> 32) as usize, (p & 0xffff_ffff) as usize)
}

fn read(mem: &Memory, store: &Store<State>, ptr: usize, len: usize) -> Result<Vec<u8>> {
    if len > MAX_OUTPUT_BYTES {
        return Err(Error::Unavailable(format!(
            "plugin output of {len} bytes exceeds the limit"
        )));
    }
    let data = mem.data(store);
    let end = ptr
        .checked_add(len)
        .filter(|e| *e <= data.len())
        .ok_or_else(|| Error::Unavailable("plugin returned an out-of-bounds buffer".into()))?;
    Ok(data[ptr..end].to_vec())
}

pub struct WasmPlugin {
    name: String,
    hooks: Vec<String>,
    cfg: PluginConfig,
    pre: InstancePre<State>,
}

impl WasmPlugin {
    pub fn load(name: &str, cfg: &PluginConfig) -> Result<Self> {
        let bytes = std::fs::read(&cfg.path)
            .map_err(|e| Error::InvalidInput(format!("plugin {name}: {}: {e}", cfg.path)))?;
        if let Some(want) = &cfg.sha256 {
            let got: String = sha2::Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if !got.eq_ignore_ascii_case(want) {
                return Err(Error::InvalidInput(format!(
                    "plugin {name}: sha256 mismatch (file has {got})"
                )));
            }
        }
        let engine = engine()?;
        let module = Module::new(engine, &bytes)
            .map_err(|e| Error::InvalidInput(format!("plugin {name}: invalid module: {e}")))?;
        let mut linker: Linker<State> = Linker::new(engine);
        if cfg.allow_log {
            linker
                .func_wrap("env", "log", |mut caller: Caller<'_, State>, ptr: i32, len: i32| {
                    let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return };
                    let data = mem.data(&caller);
                    let (p, l) = (ptr as usize, (len as usize).min(MAX_LOG_BYTES));
                    if let Some(bytes) = data.get(p..p.saturating_add(l)) {
                        let msg = String::from_utf8_lossy(bytes).into_owned();
                        tracing::info!(target: "rag2::plugin", plugin = %caller.data().plugin, "{msg}");
                    }
                })
                .map_err(|e| Error::Internal(e.to_string()))?;
        }
        // Linking fails here if the module imports anything it was not granted.
        let pre = linker.instantiate_pre(&module).map_err(|e| {
            Error::InvalidInput(format!("plugin {name}: imports not permitted: {e}"))
        })?;
        let mut p = WasmPlugin {
            name: name.into(),
            hooks: Vec::new(),
            cfg: cfg.clone(),
            pre,
        };
        let manifest = p.manifest()?;
        let declared: Vec<String> = manifest["hooks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        for h in &cfg.hooks {
            if !declared.contains(h) {
                return Err(Error::InvalidInput(format!(
                    "plugin {name} does not implement hook {h} (declares {declared:?})"
                )));
            }
        }
        p.hooks = cfg.hooks.clone();
        tracing::info!(plugin = name, version = %manifest["version"], hooks = ?p.hooks, "plugin loaded");
        Ok(p)
    }

    fn store(&self, fuel: u64) -> Result<Store<State>> {
        let engine = engine()?;
        let limits = StoreLimitsBuilder::new()
            .memory_size((self.cfg.memory_mb * BYTES_PER_MB) as usize)
            .instances(1)
            .tables(4)
            .memories(1)
            .build();
        let mut store = Store::new(
            engine,
            State {
                limits,
                plugin: self.name.clone(),
            },
        );
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(fuel)
            .map_err(|e| Error::Internal(e.to_string()))?;
        let ticks = self
            .cfg
            .timeout_ms
            .div_ceil(EPOCH_TICK.as_millis() as u64)
            .max(1);
        store.set_epoch_deadline(ticks);
        Ok(store)
    }

    fn trap(&self, e: wasmtime::Error) -> Error {
        let text = format!("{e:?}");
        if text.contains("fuel") {
            Error::Unavailable(format!(
                "plugin {} exceeded its instruction budget",
                self.name
            ))
        } else if text.contains("interrupt") || text.contains("epoch") {
            Error::Unavailable(format!("plugin {} exceeded its time limit", self.name))
        } else {
            Error::Unavailable(format!("plugin {} failed: {e}", self.name))
        }
    }

    fn manifest(&self) -> Result<serde_json::Value> {
        let mut store = self.store(MANIFEST_FUEL)?;
        let inst = self.pre.instantiate(&mut store).map_err(|e| self.trap(e))?;
        let abi = inst
            .get_typed_func::<(), i32>(&mut store, "rag2_abi_version")
            .and_then(|f| f.call(&mut store, ()))
            .map_err(|e| {
                Error::InvalidInput(format!("plugin {}: no rag2_abi_version: {e}", self.name))
            })?;
        if abi as u32 != PLUGIN_ABI_VERSION {
            return Err(Error::InvalidInput(format!(
                "plugin {} uses ABI {abi}; this host supports {PLUGIN_ABI_VERSION}",
                self.name
            )));
        }
        let packed = inst
            .get_typed_func::<(), i64>(&mut store, "rag2_manifest")
            .and_then(|f| f.call(&mut store, ()))
            .map_err(|e| {
                Error::InvalidInput(format!("plugin {}: no rag2_manifest: {e}", self.name))
            })?;
        let mem = inst
            .get_memory(&mut store, "memory")
            .ok_or_else(|| Error::InvalidInput("plugin exports no memory".into()))?;
        let (p, l) = split(packed);
        serde_json::from_slice(&read(&mem, &store, p, l)?)
            .map_err(|e| Error::InvalidInput(format!("plugin {} manifest: {e}", self.name)))
    }
}

impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn hooks(&self) -> &[String] {
        &self.hooks
    }

    fn call(
        &self,
        hook: &str,
        tenant: &str,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        if !self.hooks.iter().any(|h| h == hook) {
            return Err(Error::Forbidden(format!(
                "plugin {} is not granted hook {hook}",
                self.name
            )));
        }
        let envelope = serde_json::to_vec(&serde_json::json!({
            "abi": PLUGIN_ABI_VERSION,
            "hook": hook,
            "tenant": tenant,
            "config": self.cfg.config,
            "input": input,
        }))
        .map_err(Error::internal)?;
        let mut store = self.store(self.cfg.fuel)?;
        let inst = self.pre.instantiate(&mut store).map_err(|e| self.trap(e))?;
        let mem = inst
            .get_memory(&mut store, "memory")
            .ok_or_else(|| Error::Unavailable("plugin exports no memory".into()))?;
        let alloc = inst
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|e| self.trap(e))?;
        let call = inst
            .get_typed_func::<(i32, i32), i64>(&mut store, "call")
            .map_err(|e| self.trap(e))?;
        let len = i32::try_from(envelope.len())
            .map_err(|_| Error::InvalidInput("plugin input too large".into()))?;
        let ptr = alloc.call(&mut store, len).map_err(|e| self.trap(e))?;
        mem.write(&mut store, ptr as usize, &envelope)
            .map_err(|_| {
                Error::Unavailable(format!(
                    "plugin {} allocated an out-of-bounds buffer",
                    self.name
                ))
            })?;
        let packed = call
            .call(&mut store, (ptr, len))
            .map_err(|e| self.trap(e))?;
        let (p, l) = split(packed);
        let out: serde_json::Value =
            serde_json::from_slice(&read(&mem, &store, p, l)?).map_err(|e| {
                Error::Unavailable(format!("plugin {} returned invalid JSON: {e}", self.name))
            })?;
        if let Some(msg) = out.get("error").and_then(|m| m.as_str()) {
            return Err(Error::InvalidInput(format!("plugin {}: {msg}", self.name)));
        }
        Ok(out)
    }
}

/// Hosts `plugins.*` from the profile.
pub struct PluginFactory;

impl rag2_engine::components::ComponentFactory for PluginFactory {
    fn embedder(
        &self,
        _: &str,
        _: &rag2_config::schema::EmbedderConfig,
    ) -> Result<Option<Arc<dyn rag2_core::interfaces::Embedder>>> {
        Ok(None)
    }
    fn decision_model(
        &self,
        _: &str,
        _: &rag2_config::schema::DecisionModelConfig,
    ) -> Result<Option<Arc<dyn rag2_core::interfaces::DecisionProvider>>> {
        Ok(None)
    }
    fn plugin(&self, name: &str, cfg: &PluginConfig) -> Result<Option<Arc<dyn Plugin>>> {
        Ok(Some(Arc::new(WasmPlugin::load(name, cfg)?)))
    }
}

#[cfg(test)]
mod tests;

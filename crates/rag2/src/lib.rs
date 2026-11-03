//! RAG2 in one dependency.
//!
//! Embed the engine in any Rust program:
//!
//! ```no_run
//! # fn main() -> rag2::Result<()> {
//! let rag = rag2::open("./rag2-data")?; // versioned config + storage live in this directory
//! let tenant = rag2::TenantId::new("docs")?;
//! rag.ingest(&tenant, vec![rag2::Document::new("seals", "Pump seals are inspected every 500 hours.")?])?;
//! let answer = rag.query(&tenant, rag2::QueryRequest::new("How often are pump seals inspected?"))?;
//! println!("{} passages, confidence {}", answer.evidence.len(), answer.confidence);
//! # Ok(()) }
//! ```
//!
//! Or mount the HTTP API (REST, OpenAPI, MCP, admin, metrics) into an existing axum server with
//! [`router`] (feature `server`). Every behavior is configured by the profile, not by code: see
//! `docs/CONFIGURATION.md` and `rag2 config schema`.

use std::path::Path;
use std::sync::Arc;

pub use rag2_config::schema::Profile;
pub use rag2_config::{self as config, ConfigManager, FsConfigStore};
pub use rag2_core::model::{Document, Metadata};
pub use rag2_core::{self as core, DocumentId, Error, Result, TenantId};
pub use rag2_engine::components::ComponentFactory;
pub use rag2_engine::{self as engine, Engine, QueryRequest, QueryResponse};

#[cfg(feature = "client")]
pub use rag2_client as client;
#[cfg(feature = "server")]
pub use rag2_server as server;

/// Profile name [`open`] manages in its directory.
pub const PROFILE: &str = "main";
/// Versioned profiles, under the directory given to [`open`].
pub const CONFIG_DIR: &str = "config";
/// Storage and indexes, under the directory given to [`open`], unless the profile names another.
pub const DATA_DIR: &str = "data";

/// Every component factory compiled in: the built-in hash embedder always; d1 and Laya HTTP
/// decision models with `decision`; candle/HTTP embedders and in-process Laya with `models`; Wasm
/// plugins with `plugins`.
pub fn factories() -> Vec<Arc<dyn ComponentFactory>> {
    #[allow(unused_mut)]
    let mut f: Vec<Arc<dyn ComponentFactory>> = Vec::new();
    #[cfg(feature = "decision")]
    f.push(Arc::new(rag2_decision::DecisionFactory));
    #[cfg(feature = "models")]
    {
        f.push(Arc::new(rag2_embed::EmbedFactory));
        f.push(Arc::new(rag2_laya::adapter::LayaFactory));
    }
    #[cfg(feature = "plugins")]
    f.push(Arc::new(rag2_plugins::PluginFactory));
    f
}

fn data_dir(dir: &Path) -> String {
    dir.join(DATA_DIR).to_string_lossy().into_owned()
}

fn manager(dir: &Path) -> Result<Arc<ConfigManager>> {
    let data = data_dir(dir);
    let store = Arc::new(FsConfigStore::open(dir.join(CONFIG_DIR))?);
    Ok(Arc::new(ConfigManager::open(
        store,
        PROFILE,
        None,
        move || {
            let mut p = Profile::minimal(PROFILE);
            p.storage.data_dir = data;
            p
        },
        rag2_config::defaults::CONFIG_HISTORY,
    )?))
}

/// Open RAG2 in `dir` (created if missing). A new directory starts from the built-in default
/// profile with storage in `dir/data`; an existing one resumes its active profile version.
pub fn open(dir: impl AsRef<Path>) -> Result<Arc<Engine>> {
    Engine::open(manager(dir.as_ref())?, factories())
}

/// Open RAG2 in `dir` and activate `profile` as a new version when it differs from the active
/// one (validated first; the previous version stays available for rollback). A profile that
/// leaves `storage.data_dir` at its default stores data in `dir/data`.
pub fn open_with_profile(dir: impl AsRef<Path>, profile: &Profile) -> Result<Arc<Engine>> {
    let m = manager(dir.as_ref())?;
    let mut profile = profile.clone();
    if profile.storage.data_dir == rag2_config::defaults::DATA_DIR {
        profile.storage.data_dir = data_dir(dir.as_ref());
    }
    let value = serde_json::to_value(&profile).map_err(Error::internal)?;
    m.apply(&value, "rag2::open_with_profile", "embedded profile")?;
    Engine::open(m, factories())
}

/// Open RAG2 in `dir` with a profile from YAML or JSON text (a file's contents, an env var).
pub fn open_with_profile_text(dir: impl AsRef<Path>, text: &str) -> Result<Arc<Engine>> {
    let value = rag2_config::parse_profile_value(text)?;
    let profile: Profile =
        serde_json::from_value(value).map_err(|e| Error::InvalidInput(format!("profile: {e}")))?;
    open_with_profile(dir, &profile)
}

/// The full HTTP API for `engine` as an axum `Router`, with its background work started on the
/// current tokio runtime. Mount it with `Router::nest("/rag", rag2::router(engine)?)` or merge it.
#[cfg(feature = "server")]
pub fn router(engine: Arc<Engine>) -> Result<axum::Router> {
    Ok(rag2_server::app(engine, None)?.0)
}

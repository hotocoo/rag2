//! The live configuration: one atomically swapped snapshot that every request reads, plus the
//! operations that change it (submit, preview, activate, rollback).
//!
//! Readers call [`ConfigManager::current`] (an `Arc` clone, no lock). Activation validates the
//! candidate, asks every registered veto hook (e.g. "can the engine build these embedders?"), and
//! only then swaps the pointer and persists it. A failed activation changes nothing.

use crate::patch::{Change, diff, merge_patch};
use crate::schema::Profile;
use crate::store::{Activation, ConfigStore, VersionMeta, now_ms};
use crate::validate::{Issue, has_errors, validate};
use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use rag2_core::{Error, Result};
use serde::Serialize;
use std::sync::Arc;

/// The active configuration snapshot.
#[derive(Debug)]
pub struct Active {
    pub version: u64,
    pub environment: Option<String>,
    /// Profile with the environment overlay applied.
    pub profile: Arc<Profile>,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Preview {
    pub valid: bool,
    pub issues: Vec<Issue>,
    /// Changes against the active version (after overlays on both sides).
    pub changes: Vec<Change>,
}

type Veto = Box<dyn Fn(&Profile) -> std::result::Result<(), String> + Send + Sync>;
type Listener = Box<dyn Fn(&Arc<Active>) + Send + Sync>;

pub struct ConfigManager {
    store: Arc<dyn ConfigStore>,
    name: String,
    environment: Option<String>,
    active: ArcSwap<Active>,
    vetoes: RwLock<Vec<Veto>>,
    listeners: RwLock<Vec<Listener>>,
    /// Serializes activations so two operators cannot interleave validate and swap.
    activation: Mutex<()>,
    history: usize,
}

/// Parse YAML or JSON (JSON when the first non-blank character is `{`).
pub fn parse_profile_value(text: &str) -> Result<serde_json::Value> {
    let parsed = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(|e| e.to_string())
    } else {
        serde_norway::from_str(text).map_err(|e| e.to_string())
    };
    parsed.map_err(|e| Error::InvalidInput(format!("profile is not valid YAML/JSON: {e}")))
}

/// Deserialize a profile, then apply the named environment overlay.
pub fn resolve(value: &serde_json::Value, environment: Option<&str>) -> Result<Profile> {
    let base: Profile = serde_json::from_value(value.clone())
        .map_err(|e| Error::InvalidInput(format!("profile: {e}")))?;
    let Some(env) = environment else {
        return Ok(base);
    };
    let Some(overlay) = base.environments.get(env) else {
        return Err(Error::InvalidInput(format!(
            "profile has no environment {env:?}"
        )));
    };
    let merged = merge_patch(value, overlay);
    serde_json::from_value(merged)
        .map_err(|e| Error::InvalidInput(format!("environment {env}: {e}")))
}

impl ConfigManager {
    /// Load the active version of `name`; when none exists yet, store and activate `bootstrap`.
    pub fn open(
        store: Arc<dyn ConfigStore>,
        name: &str,
        environment: Option<String>,
        bootstrap: impl FnOnce() -> Profile,
        history: usize,
    ) -> Result<Self> {
        let version = match store.active(name)? {
            Some(v) => v,
            None => {
                let value = serde_json::to_value(bootstrap()).map_err(Error::internal)?;
                let meta = store.put(name, &value, "system", "bootstrap")?;
                store.record_activation(
                    name,
                    &Activation {
                        version: meta.version,
                        at_ms: now_ms(),
                        author: "system".into(),
                        action: "activate".into(),
                        environment: environment.clone(),
                    },
                )?;
                meta.version
            }
        };
        let stored = store.get(name, version)?;
        let profile = resolve(&stored.profile, environment.as_deref())?;
        let issues = validate(&profile);
        if has_errors(&issues) {
            return Err(Error::InvalidInput(format!(
                "active profile {name} v{version} is invalid: {issues:?}"
            )));
        }
        let active = Active {
            version,
            environment: environment.clone(),
            profile: Arc::new(profile),
            hash: stored.meta.hash,
        };
        Ok(ConfigManager {
            store,
            name: name.into(),
            environment,
            active: ArcSwap::from_pointee(active),
            vetoes: RwLock::new(Vec::new()),
            listeners: RwLock::new(Vec::new()),
            activation: Mutex::new(()),
            history,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The active snapshot. Lock-free; hold the `Arc` for the duration of one request so a
    /// concurrent activation never changes configuration mid-request.
    pub fn current(&self) -> Arc<Active> {
        self.active.load_full()
    }

    /// Register a hook that may refuse a candidate profile before it becomes active.
    pub fn add_veto(
        &self,
        f: impl Fn(&Profile) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) {
        self.vetoes.write().push(Box::new(f));
    }

    /// Register a hook called after every successful activation.
    pub fn on_change(&self, f: impl Fn(&Arc<Active>) + Send + Sync + 'static) {
        self.listeners.write().push(Box::new(f));
    }

    pub fn store(&self) -> &Arc<dyn ConfigStore> {
        &self.store
    }

    /// Validate `value` and compare it with the active version without changing anything.
    pub fn preview(&self, value: &serde_json::Value) -> Preview {
        let cur = self.current();
        let profile = match resolve(value, self.environment.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                return Preview {
                    valid: false,
                    issues: vec![Issue {
                        path: String::new(),
                        message: e.to_string(),
                        severity: crate::validate::Severity::Error,
                    }],
                    changes: Vec::new(),
                };
            }
        };
        let mut issues = validate(&profile);
        if !has_errors(&issues) {
            for veto in self.vetoes.read().iter() {
                if let Err(m) = veto(&profile) {
                    issues.push(Issue {
                        path: String::new(),
                        message: m,
                        severity: crate::validate::Severity::Error,
                    });
                }
            }
        }
        let before = serde_json::to_value(cur.profile.as_ref()).unwrap_or_default();
        let after = serde_json::to_value(&profile).unwrap_or_default();
        Preview {
            valid: !has_errors(&issues),
            issues,
            changes: diff(&before, &after),
        }
    }

    /// Store a new version (not activated). Refused when it does not validate.
    pub fn submit(
        &self,
        value: &serde_json::Value,
        author: &str,
        comment: &str,
    ) -> Result<VersionMeta> {
        let p = self.preview(value);
        if !p.valid {
            return Err(Error::InvalidInput(format!(
                "profile rejected: {}",
                summarize(&p.issues)
            )));
        }
        let meta = self.store.put(&self.name, value, author, comment)?;
        if let Err(e) = self.store.prune(&self.name, self.history) {
            tracing::warn!(error = %e, "pruning old profile versions failed");
        }
        Ok(meta)
    }

    /// Make `version` active. Validation, vetoes, persist, swap, notify; any failure leaves the
    /// active version unchanged.
    pub fn activate(&self, version: u64, author: &str) -> Result<Arc<Active>> {
        self.activate_as(version, author, "activate")
    }

    fn activate_as(&self, version: u64, author: &str, action: &str) -> Result<Arc<Active>> {
        let _g = self.activation.lock();
        let stored = self.store.get(&self.name, version)?;
        let p = self.preview(&stored.profile);
        if !p.valid {
            return Err(Error::InvalidInput(format!(
                "version {version} rejected: {}",
                summarize(&p.issues)
            )));
        }
        let profile = resolve(&stored.profile, self.environment.as_deref())?;
        self.store.record_activation(
            &self.name,
            &Activation {
                version,
                at_ms: now_ms(),
                author: author.into(),
                action: action.into(),
                environment: self.environment.clone(),
            },
        )?;
        let next = Arc::new(Active {
            version,
            environment: self.environment.clone(),
            profile: Arc::new(profile),
            hash: stored.meta.hash,
        });
        self.active.store(next.clone());
        tracing::info!(profile = %self.name, version, action, author, "configuration activated");
        for l in self.listeners.read().iter() {
            l(&next);
        }
        Ok(next)
    }

    /// Re-activate the version that was active before the current one.
    pub fn rollback(&self, author: &str) -> Result<Arc<Active>> {
        let cur = self.current().version;
        let log = self.store.activations(&self.name)?;
        // Walk back past every entry for the current version to the one before it.
        let prev = log
            .iter()
            .rev()
            .skip_while(|a| a.version == cur)
            .map(|a| a.version)
            .next()
            .ok_or_else(|| Error::Conflict("no earlier activation to roll back to".into()))?;
        self.activate_as(prev, author, "rollback")
    }

    /// Submit and activate in one step (hot reload, `rag2 apply`).
    pub fn apply(
        &self,
        value: &serde_json::Value,
        author: &str,
        comment: &str,
    ) -> Result<Arc<Active>> {
        if serde_json::to_value(self.current().profile.as_ref())
            .ok()
            .as_ref()
            == Some(value)
            && self.environment.is_none()
        {
            return Ok(self.current());
        }
        let meta = self.submit(value, author, comment)?;
        self.activate(meta.version, author)
    }
}

fn summarize(issues: &[Issue]) -> String {
    issues
        .iter()
        .filter(|i| i.severity == crate::validate::Severity::Error)
        .map(|i| {
            if i.path.is_empty() {
                i.message.clone()
            } else {
                format!("{}: {}", i.path, i.message)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FsConfigStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn manager(dir: &std::path::Path, env: Option<&str>) -> ConfigManager {
        let store = Arc::new(FsConfigStore::open(dir).expect("store"));
        ConfigManager::open(
            store,
            "main",
            env.map(String::from),
            || Profile::minimal("main"),
            10,
        )
        .expect("open")
    }

    fn with_top_k(m: &ConfigManager, k: usize) -> serde_json::Value {
        let mut p = (*m.current().profile).clone();
        p.pipelines.get_mut("default").expect("default").top_k = k;
        serde_json::to_value(p).expect("value")
    }

    #[test]
    fn bootstrap_then_activate_then_rollback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = manager(dir.path(), None);
        assert_eq!(m.current().version, 1);
        let seen = Arc::new(AtomicU64::new(0));
        let s = seen.clone();
        m.on_change(move |a| s.store(a.version, Ordering::SeqCst));

        let v2 = m
            .submit(&with_top_k(&m, 20), "alice", "wider")
            .expect("submit");
        assert_eq!(m.current().version, 1, "submit must not activate");
        m.activate(v2.version, "alice").expect("activate");
        assert_eq!(m.current().profile.pipelines["default"].top_k, 20);
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        m.rollback("bob").expect("rollback");
        assert_eq!(m.current().version, 1);
        // Survives restart.
        drop(m);
        assert_eq!(manager(dir.path(), None).current().version, 1);
    }

    #[test]
    fn invalid_or_vetoed_profiles_never_become_active() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = manager(dir.path(), None);
        assert!(m.submit(&with_top_k(&m, 0), "a", "").is_err());
        m.add_veto(|p| {
            if p.pipelines["default"].top_k == 7 {
                Err("seven is vetoed".into())
            } else {
                Ok(())
            }
        });
        let err = m.apply(&with_top_k(&m, 7), "a", "").expect_err("vetoed");
        assert!(err.to_string().contains("seven is vetoed"));
        assert_eq!(m.current().version, 1);
        assert_eq!(m.store().list("main").expect("list").len(), 1);
    }

    #[test]
    fn preview_lists_changes_without_side_effects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = manager(dir.path(), None);
        let p = m.preview(&with_top_k(&m, 15));
        assert!(p.valid);
        assert_eq!(p.changes.len(), 1);
        assert_eq!(m.store().list("main").expect("list").len(), 1);
    }

    #[test]
    fn environment_overlays_apply_at_activation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FsConfigStore::open(dir.path()).expect("store"));
        let mut p = Profile::minimal("main");
        p.environments.insert(
            "prod".into(),
            serde_json::json!({"limits": {"max_top_k": 50}}),
        );
        let m = ConfigManager::open(store, "main", Some("prod".into()), || p, 10).expect("open");
        assert_eq!(m.current().profile.limits.max_top_k, 50);
    }

    #[test]
    fn yaml_and_json_both_parse_and_unknown_fields_are_refused() {
        let y = parse_profile_value("api_version: rag2/v1\nmetadata:\n  name: x\n").expect("yaml");
        assert!(resolve(&y, None).is_ok());
        let j =
            parse_profile_value(r#"{"api_version":"rag2/v1","metadata":{"name":"x"},"typo":1}"#)
                .expect("json");
        assert!(resolve(&j, None).is_err());
    }
}

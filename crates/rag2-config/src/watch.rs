//! Hot reload: watch an operator-edited profile file and apply it when it changes. An invalid
//! edit is logged and ignored; the running configuration stays as it was.

use crate::manager::{ConfigManager, parse_profile_value};
use notify::{EventKind, RecursiveMode, Watcher};
use rag2_core::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Keeps the watch alive; dropping it stops hot reload.
pub struct ReloadGuard {
    _watcher: notify::RecommendedWatcher,
}

/// Read, parse and apply `path` once. Returns the activated version.
pub fn reload_file(manager: &ConfigManager, path: &Path) -> Result<u64> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;
    let value = parse_profile_value(&text)?;
    Ok(manager
        .apply(
            &value,
            "file-watch",
            &format!("hot reload of {}", path.display()),
        )?
        .version)
}

/// Watch `path`; after changes settle for `debounce`, apply the file.
pub fn watch_file(
    manager: Arc<ConfigManager>,
    path: PathBuf,
    debounce: Duration,
) -> Result<ReloadGuard> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let target = path.clone();
    let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
        if let Ok(ev) = ev
            && matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_))
            && ev.paths.iter().any(|p| p.file_name() == target.file_name())
        {
            let _ = tx.send(());
        }
    })
    .map_err(|e| Error::Internal(format!("file watcher: {e}")))?;
    // Watch the directory: editors replace files by rename, which a file watch would miss.
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(|e| Error::Internal(format!("watch {}: {e}", dir.display())))?;
    std::thread::Builder::new()
        .name("rag2-config-reload".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Coalesce a burst of events (editor write + rename + chmod) into one reload.
                while rx.recv_timeout(debounce).is_ok() {}
                match reload_file(&manager, &path) {
                    Ok(v) => tracing::info!(file = %path.display(), version = v, "profile reloaded"),
                    Err(e) => tracing::error!(file = %path.display(), error = %e, "profile reload refused; keeping the active version"),
                }
            }
        })
        .map_err(Error::internal)?;
    Ok(ReloadGuard { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Profile;
    use crate::store::FsConfigStore;

    #[test]
    fn a_valid_file_applies_and_an_invalid_one_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FsConfigStore::open(dir.path().join("store")).expect("store"));
        let m = ConfigManager::open(store, "main", None, || Profile::minimal("main"), 10)
            .expect("open");
        let f = dir.path().join("main.yaml");
        let mut p = Profile::minimal("main");
        p.limits.max_top_k = 42;
        std::fs::write(&f, serde_norway::to_string(&p).expect("yaml")).expect("write");
        assert_eq!(reload_file(&m, &f).expect("reload"), 2);
        assert_eq!(m.current().profile.limits.max_top_k, 42);
        std::fs::write(
            &f,
            "api_version: rag2/v1\nmetadata: {name: main}\nlimits: {max_top_k: 0}\n",
        )
        .expect("write");
        assert!(reload_file(&m, &f).is_err());
        assert_eq!(m.current().profile.limits.max_top_k, 42);
    }
}

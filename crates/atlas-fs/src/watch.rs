//! Change events for a folder tree, debounced so a burst of writes (a sync
//! client dropping a folder in) arrives as one batch of paths. Watching can
//! miss events (network filesystems, overflowing queues), so callers also
//! run a periodic full sync; this only makes changes show up quickly.

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Keeps the watch alive; dropping it stops the events.
pub struct Watcher {
    _inner: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
}

/// Calls `on_change` with the paths that changed under `root`, at most about
/// once a second. Runs on notify's own thread; keep `on_change` quick (send
/// to a channel).
pub fn watch(root: &Path, on_change: impl Fn(Vec<PathBuf>) + Send + 'static) -> notify::Result<Watcher> {
    let mut debouncer = new_debouncer(Duration::from_secs(1), None, move |result: DebounceEventResult| match result {
        Ok(events) => {
            let mut paths: Vec<PathBuf> = events.into_iter().flat_map(|e| e.event.paths).collect();
            paths.sort();
            paths.dedup();
            if !paths.is_empty() {
                on_change(paths);
            }
        }
        Err(errors) => {
            for e in errors {
                tracing::warn!(error = %e, "file watch error");
            }
        }
    })?;
    debouncer.watch(root, RecursiveMode::Recursive)?;
    Ok(Watcher { _inner: debouncer })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn reports_a_new_file() {
        let d = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = watch(d.path(), move |paths| {
            let _ = tx.send(paths);
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(d.path().join("new.txt"), "hi").unwrap();
        let paths = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        assert!(paths.iter().any(|p| p.ends_with("new.txt")), "{paths:?}");
    }
}

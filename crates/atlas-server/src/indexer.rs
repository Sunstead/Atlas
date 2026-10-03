//! Keeps the search index current, for every user, in the background.
//!
//! - **Jobs** are per connection: a full sync, or a list of changed items
//!   from a file watcher. Queued jobs for one connection merge (a full sync
//!   covers any items), and a connection never syncs twice at once.
//! - **Concurrency**: at most [`MAX_SYNCS`] connections sync at a time, each
//!   on a blocking thread (walking and reading files is blocking work).
//! - **Triggers**: startup, a connection created or changed, "Sync now", file
//!   watchers, and a full reconcile every [`RECONCILE_EVERY`], because
//!   watchers can miss events.
//!
//! Sources are built from their connection rows and cached until the row
//! changes.

use crate::config::SourcesConfig;
use crate::sources;
use atlas_common::SyncInfo;
use atlas_core::{Source, SourceError, SyncMode};
use atlas_index::Index;
use atlas_state::{ConnectionRow, Db};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

pub const MAX_SYNCS: usize = 2;
pub const RECONCILE_EVERY: Duration = Duration::from_secs(15 * 60);

pub struct Indexer {
    pub index: Arc<Index>,
    db: Db,
    sources: Arc<SourcesConfig>,
    tx: mpsc::UnboundedSender<i64>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<i64>>>,
    inner: Mutex<Inner>,
    me: Weak<Indexer>,
}

#[derive(Default)]
struct Inner {
    queued: HashMap<i64, SyncMode>,
    running: HashMap<i64, CancellationToken>,
    watchers: HashMap<i64, Vec<atlas_fs::Watcher>>,
    built: HashMap<i64, (i64, Arc<dyn Source>)>,
}

/// Merges a new job into one already waiting.
fn merge(current: Option<SyncMode>, new: SyncMode) -> SyncMode {
    match (current, new) {
        (Some(SyncMode::Full), _) | (_, SyncMode::Full) => SyncMode::Full,
        (Some(SyncMode::Items(mut a)), SyncMode::Items(b)) => {
            a.extend(b);
            a.sort();
            a.dedup();
            SyncMode::Items(a)
        }
        (None, new) => new,
    }
}

fn message(e: &SourceError) -> String {
    e.to_string()
}

impl Indexer {
    pub fn new(index: Arc<Index>, db: Db, sources: Arc<SourcesConfig>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new_cyclic(|me| Self {
            index,
            db,
            sources,
            tx,
            rx: Mutex::new(Some(rx)),
            inner: Mutex::new(Inner::default()),
            me: me.clone(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts the worker, queues a full sync of every enabled connection,
    /// and schedules the periodic reconcile.
    pub fn start(self: &Arc<Self>) {
        let Some(mut rx) = self.rx.lock().unwrap_or_else(|p| p.into_inner()).take() else { return };
        let this = self.clone();
        tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(MAX_SYNCS));
            while let Some(conn) = rx.recv().await {
                let Ok(permit) = permits.clone().acquire_owned().await else { break };
                let this = this.clone();
                tokio::spawn(async move {
                    this.run(conn).await;
                    drop(permit);
                });
            }
        });

        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(RECONCILE_EVERY);
            loop {
                tick.tick().await;
                this.sync_all().await;
            }
        });
    }

    /// A full sync of every enabled connection that Atlas indexes.
    pub async fn sync_all(&self) {
        match self.db.all_connections().await {
            Ok(rows) => {
                for row in rows.iter().filter(|r| r.enabled && sources::is_indexed(&r.kind)) {
                    self.enqueue(row.id, SyncMode::Full);
                }
            }
            Err(e) => tracing::warn!(error = %e, "can't list connections to sync"),
        }
    }

    pub fn enqueue(&self, conn: i64, mode: SyncMode) {
        let mut inner = self.lock();
        let first = !inner.queued.contains_key(&conn);
        let current = inner.queued.remove(&conn);
        inner.queued.insert(conn, merge(current, mode));
        // Queued once; a running sync re-sends it when it finishes.
        if first && !inner.running.contains_key(&conn) {
            let _ = self.tx.send(conn);
        }
    }

    /// A connection was created or edited: rebuild its source, and sync it
    /// if it's enabled, or stop if it's paused.
    pub fn changed(&self, row: &ConnectionRow) {
        {
            let mut inner = self.lock();
            inner.built.remove(&row.id);
            if !row.enabled {
                if let Some(c) = inner.running.get(&row.id) {
                    c.cancel();
                }
                inner.watchers.remove(&row.id);
                inner.queued.remove(&row.id);
            }
        }
        if row.enabled && sources::is_indexed(&row.kind) {
            self.enqueue(row.id, SyncMode::Full);
        }
    }

    /// A connection was deleted: stop, unwatch, and drop what it indexed.
    pub async fn forget(&self, conn: i64) {
        {
            let mut inner = self.lock();
            if let Some(c) = inner.running.get(&conn) {
                c.cancel();
            }
            inner.watchers.remove(&conn);
            inner.queued.remove(&conn);
            inner.built.remove(&conn);
        }
        let index = self.index.clone();
        match tokio::task::spawn_blocking(move || index.remove_connection(conn)).await {
            Ok(Ok(n)) => tracing::info!(connection = conn, items = n, "removed a connection's items"),
            Ok(Err(e)) => tracing::warn!(connection = conn, error = %e, "can't remove a connection's items"),
            Err(e) => tracing::warn!(error = %e, "index task failed"),
        }
    }

    /// The source for a connection, built once per version of its row.
    pub fn source(&self, row: &ConnectionRow) -> Result<Arc<dyn Source>, SourceError> {
        if let Some((version, s)) = self.lock().built.get(&row.id) {
            if *version == row.updated_at {
                return Ok(s.clone());
            }
        }
        let built = sources::build(&self.sources, row)?;
        self.lock().built.insert(row.id, (row.updated_at, built.clone()));
        Ok(built)
    }

    pub fn status(&self, conn: i64) -> Option<SyncInfo> {
        let running = self.lock().running.contains_key(&conn);
        let items = self.index.count(conn).ok()?;
        let state = self.index.sync_state(conn).ok()?;
        Some(SyncInfo { running, items, last_synced_at: state.last_full_at, error: state.last_error })
    }

    async fn run(self: Arc<Self>, conn: i64) {
        let (mode, cancel) = {
            let mut inner = self.lock();
            if inner.running.contains_key(&conn) {
                return;
            }
            let Some(mode) = inner.queued.remove(&conn) else { return };
            let cancel = CancellationToken::new();
            inner.running.insert(conn, cancel.clone());
            (mode, cancel)
        };

        let this = self.clone();
        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || handle.block_on(this.sync_one(conn, mode, cancel))).await;
        if let Err(e) = result {
            tracing::error!(connection = conn, error = %e, "sync task panicked");
        }

        let mut inner = self.lock();
        inner.running.remove(&conn);
        if inner.queued.contains_key(&conn) {
            let _ = self.tx.send(conn);
        }
    }

    async fn sync_one(&self, conn: i64, mode: SyncMode, cancel: CancellationToken) {
        let row = match self.db.connection_by_id(conn).await {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(connection = conn, error = %e, "can't load a connection to sync");
                return;
            }
        };
        if !row.enabled {
            return;
        }
        let full = mode == SyncMode::Full;
        let source = match self.source(&row) {
            Ok(s) => s,
            Err(e) => {
                tracing::info!(connection = conn, error = %e, "can't sync");
                let _ = self.index.record_sync(conn, Err(message(&e)));
                return;
            }
        };
        let Some(indexed) = source.as_indexed() else { return };

        let started = std::time::Instant::now();
        let outcome: Result<atlas_core::SyncReport, SourceError> = async {
            let known = self.index.known(conn)?;
            let mut batch = self.index.batch(row.user.get(), conn);
            let report = indexed.sync(mode, &known, &mut batch, &cancel).await?;
            batch.commit()?;
            Ok(report)
        }
        .await;

        match outcome {
            Ok(report) => {
                if report.upserted + report.deleted > 0 || full {
                    tracing::info!(
                        connection = conn,
                        full,
                        seen = report.seen,
                        upserted = report.upserted,
                        deleted = report.deleted,
                        failed = report.failed,
                        ms = started.elapsed().as_millis() as u64,
                        "synced"
                    );
                }
                if full {
                    let _ = self.index.record_sync(conn, Ok(()));
                }
                self.ensure_watch(conn, &source);
            }
            Err(SourceError::Cancelled) => tracing::info!(connection = conn, "sync cancelled"),
            Err(e) => {
                tracing::warn!(connection = conn, error = %e, "sync failed");
                let _ = self.index.record_sync(conn, Err(message(&e)));
            }
        }
    }

    /// Watches a local source's folders, turning changes into item syncs.
    /// A changed folder, or a path that's gone (a file or a whole folder),
    /// gets a full sync instead: renames move everything under them.
    fn ensure_watch(&self, conn: i64, source: &Arc<dyn Source>) {
        let Some(indexed) = source.as_indexed() else { return };
        let roots = indexed.watch_roots();
        if roots.is_empty() || self.lock().watchers.contains_key(&conn) {
            return;
        }
        let mut watchers = Vec::new();
        for root in roots {
            let me = self.me.clone();
            let source = source.clone();
            let w = atlas_fs::watch(&root, move |paths| {
                let Some(me) = me.upgrade() else { return };
                let Some(indexed) = source.as_indexed() else { return };
                let mut ids = HashSet::new();
                let mut full = false;
                for p in &paths {
                    if indexed.id_for_path(p).is_none() {
                        continue;
                    }
                    match std::fs::symlink_metadata(p) {
                        Ok(m) if m.is_file() => {
                            if let Some(id) = indexed.id_for_path(p) {
                                ids.insert(id);
                            }
                        }
                        _ => full = true,
                    }
                }
                if full {
                    me.enqueue(conn, SyncMode::Full);
                } else if !ids.is_empty() {
                    me.enqueue(conn, SyncMode::Items(ids.into_iter().collect()));
                }
            });
            match w {
                Ok(w) => watchers.push(w),
                Err(e) => tracing::warn!(connection = conn, root = %root.display(), error = %e, "can't watch; relying on periodic syncs"),
            }
        }
        self.lock().watchers.insert(conn, watchers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_wins_and_items_merge() {
        let items = |v: &[&str]| SyncMode::Items(v.iter().map(|s| s.to_string()).collect());
        assert_eq!(merge(None, items(&["a"])), items(&["a"]));
        assert_eq!(merge(Some(items(&["b", "a"])), items(&["a", "c"])), items(&["a", "b", "c"]));
        assert_eq!(merge(Some(SyncMode::Full), items(&["a"])), SyncMode::Full);
        assert_eq!(merge(Some(items(&["a"])), SyncMode::Full), SyncMode::Full);
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Content index lifecycle: searcher access, published status, backfill marker and commands.

use super::{
    BackfillMarker, BatchOutcome, ContentCommand, CONTENT_BACKFILL_MARKER,
    CONTENT_BACKFILL_MARKER_TMP, CONTENT_BACKFILL_SCHEMA,
};
use cairn_content::{ContentIndex, ContentIndexError, ContentSearcher};
use cairn_protocol::status::{ContentIndexState, ContentIndexStatus};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::ServiceConfig;
use crate::sync_engine::MultiVolumeEngine;

impl MultiVolumeEngine {
    fn set_content_searcher(&self, searcher: Arc<ContentSearcher>) {
        *self.content_searcher.write().unwrap() = Some(searcher);
    }

    pub(super) fn content_searcher(&self) -> Option<Arc<ContentSearcher>> {
        self.content_searcher.read().unwrap().clone()
    }

    /// Opens the existing index read-only. The service answers queries and must never own a
    /// Tantivy writer: a writer locks the directory, starts merge threads and rewrites meta.json.
    pub(crate) fn get_or_create_content_searcher(&self) -> Option<Arc<ContentSearcher>> {
        if let Some(searcher) = self.content_searcher() {
            return Some(searcher);
        }

        let content_dir = self.content_dir.clone();
        match ContentSearcher::open(&content_dir) {
            Ok(searcher) => {
                let arc = Arc::new(searcher);
                *self.content_searcher.write().unwrap() = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::warn!("Failed to open ContentSearcher at {:?}: {}", content_dir, e);
                None
            }
        }
    }

    /// Clears the entire full-text content index (CNT-07). This is the in-process fallback used
    /// by tests and by a runtime that has no controller thread; incremental and query paths only
    /// ever read the index.
    pub fn clear_content_index(&self) -> Result<(), ContentIndexError> {
        self.drop_content_searcher();
        self.remove_backfill_marker();
        let index = ContentIndex::open_or_create(&self.content_dir)?;
        let cleared = index.clear();
        let finished = index.finish();
        cleared?;
        finished
    }

    /// Current content-index lifecycle. The GUI and the query path read this before touching the
    /// index; a disabled, backfilling or failed index must not be treated as an empty result.
    pub fn content_index_status(&self) -> ContentIndexStatus {
        self.content_status.read().unwrap().clone()
    }

    pub(in crate::sync_engine) fn set_content_state(&self, state: ContentIndexState) {
        let mut status = self.content_status.write().unwrap();
        status.state = state;
    }

    pub(in crate::sync_engine) fn set_content_progress(&self, processed: u64, total: u64) {
        let mut status = self.content_status.write().unwrap();
        status.state = ContentIndexState::Backfilling { processed, total };
    }

    fn set_content_ready(&self, documents: u64, bytes: u64) {
        let mut status = self.content_status.write().unwrap();
        status.state = ContentIndexState::Ready;
        status.indexed_documents = documents;
        status.index_bytes = bytes;
        status.last_updated_ts = unix_now();
    }

    pub(super) fn set_content_error(&self, message: String) {
        // A failed batch may have drained changes that no later USN replay will return. Keep the
        // failure from being resumed as if the index were complete; the next start rebuilds.
        self.remove_backfill_marker();
        let mut status = self.content_status.write().unwrap();
        status.state = ContentIndexState::Error(message);
    }

    pub(super) fn backfill_marker_path(&self) -> PathBuf {
        self.content_dir.join(CONTENT_BACKFILL_MARKER)
    }

    /// Removes the resume marker and any half-written replacement. Used whenever the index may no
    /// longer describe a completed full backfill (new backfill, cancel, clear).
    pub(super) fn remove_backfill_marker(&self) {
        for name in [CONTENT_BACKFILL_MARKER, CONTENT_BACKFILL_MARKER_TMP] {
            let path = self.content_dir.join(name);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => tracing::warn!("Could not remove {}: {}", path.display(), err),
            }
        }
    }

    /// Records a completed full backfill so the next process can reopen the index read-only
    /// instead of clearing and re-extracting every eligible file. `cfg` is the configuration the
    /// backfill ran with: a change that arrived while it was running cannot cover files already
    /// skipped, so the marker must keep the scope the index actually describes and let the next
    /// start reject it. The marker is replaced atomically; a write failure is logged but never
    /// turns the already published index into an error, because the running index is still fully
    /// usable.
    pub(super) fn write_backfill_marker(&self, cfg: &ServiceConfig) {
        let status = self.content_index_status();
        if !status.state.is_ready() {
            return;
        }
        let marker = BackfillMarker {
            schema: CONTENT_BACKFILL_SCHEMA,
            extensions: cfg.content_index_extensions.clone(),
            max_bytes: cfg.content_index_max_bytes,
            excluded_paths: cfg.excluded_paths.clone(),
            content_excluded_paths: cfg.content_excluded_paths.clone(),
            documents: status.indexed_documents,
            completed_ts: unix_now(),
        };
        let payload = match serde_json::to_vec_pretty(&marker) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!("Could not encode the content backfill marker: {err}");
                return;
            }
        };
        let path = self.backfill_marker_path();
        let tmp = self.content_dir.join(CONTENT_BACKFILL_MARKER_TMP);
        let persisted = std::fs::create_dir_all(&self.content_dir)
            .and_then(|()| {
                let mut file = std::fs::File::create(&tmp)?;
                file.write_all(&payload)?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(err) = persisted {
            tracing::warn!(
                "Could not persist the content backfill marker at {}: {}",
                path.display(),
                err
            );
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Reuses the index of a completed backfill when the configuration it was built with is still
    /// current. The marker is only a claim: the index itself is opened read-only and must contain
    /// documents, so a marker without a usable index never produces a fake Ready. Any doubt removes
    /// the marker and returns false, which sends the caller down the full-backfill path.
    pub(super) fn try_resume_content_index(&self) -> bool {
        // A half-written marker is never authoritative: remove it before trusting the real one.
        let _ = std::fs::remove_file(self.content_dir.join(CONTENT_BACKFILL_MARKER_TMP));

        let path = self.backfill_marker_path();
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        let marker = match serde_json::from_slice::<BackfillMarker>(&bytes) {
            Ok(marker) => marker,
            Err(err) => {
                tracing::warn!(
                    "Ignoring unreadable content backfill marker {}: {}",
                    path.display(),
                    err
                );
                self.remove_backfill_marker();
                return false;
            }
        };
        let cfg = self.get_config();
        if marker.schema != CONTENT_BACKFILL_SCHEMA {
            tracing::info!(
                "Content backfill marker schema {} does not match {}; rebuilding",
                marker.schema,
                CONTENT_BACKFILL_SCHEMA
            );
            self.remove_backfill_marker();
            return false;
        }
        if marker.extensions != cfg.content_index_extensions
            || marker.max_bytes != cfg.content_index_max_bytes
            || marker.excluded_paths != cfg.excluded_paths
            || marker.content_excluded_paths != cfg.content_excluded_paths
        {
            tracing::info!(
                "Content index configuration changed since the last full backfill; rebuilding"
            );
            self.remove_backfill_marker();
            return false;
        }

        match ContentSearcher::open(&self.content_dir) {
            Ok(searcher) => {
                let documents = match searcher.document_count() {
                    Ok(documents) => documents,
                    Err(err) => {
                        tracing::warn!(
                            "Could not read the content index at {}: {}; rebuilding",
                            self.content_dir.display(),
                            err
                        );
                        self.remove_backfill_marker();
                        return false;
                    }
                };
                if documents == 0 {
                    tracing::warn!(
                        "Content index at {} is empty despite a completed backfill; rebuilding",
                        self.content_dir.display()
                    );
                    self.remove_backfill_marker();
                    return false;
                }
                let bytes = searcher.index_size_bytes();
                self.set_content_searcher(Arc::new(searcher));
                self.set_content_ready(documents, bytes);
                tracing::info!(
                    "Resumed the content index from the completed backfill ({documents} documents)"
                );
                true
            }
            Err(err) => {
                tracing::warn!(
                    "Could not resume the content index at {}: {}; rebuilding",
                    self.content_dir.display(),
                    err
                );
                self.remove_backfill_marker();
                false
            }
        }
    }

    /// Starts the one content thread if it is not running. The service calls this once at startup;
    /// the thread receives every later start/cancel/clear command through the channel.
    pub fn ensure_content_runtime(self: &Arc<Self>) {
        let mut guard = self.content_commands.lock().unwrap();
        if guard.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        *guard = Some(tx.clone());
        drop(guard);

        let engine = self.clone();
        let spawned = std::thread::Builder::new()
            .name("cairn-content".to_string())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    engine.handle_content_command(command);
                }
            });
        if let Err(err) = spawned {
            self.set_content_error(format!("could not start content worker thread: {err}"));
            return;
        }

        if !self.config.read().unwrap().content_index_enabled {
            return;
        }
        // A resumed index never runs the full-backfill path, so stale worker files from a crashed
        // process must be cleaned here as well. No worker is active during startup.
        self.cleanup_stale_content_files();
        // A completed backfill stays valid across restarts. Reopening it read-only skips the
        // destructive `Start`, which would clear the index and re-extract every eligible file.
        // Changes made while the service was stopped are replayed from the USN journal and arrive
        // through the incremental path.
        if self.try_resume_content_index() {
            return;
        }
        self.send_content_command(ContentCommand::Start);
    }

    fn handle_content_command(&self, command: ContentCommand) {
        match command {
            ContentCommand::Start => self.run_full_backfill(),
            ContentCommand::IndexChanges { tasks, deletions } => {
                self.run_incremental(tasks, deletions)
            }
            ContentCommand::Cancel => {
                self.content_cancel.store(true, Ordering::SeqCst);
                // A disabled or cancelled index is not a completed backfill; the next start must
                // rebuild instead of resuming it.
                self.remove_backfill_marker();
                // Release the searcher and its mapped segments; a later start reopens the index.
                self.drop_content_searcher();
                self.set_content_state(ContentIndexState::Disabled);
            }
            ContentCommand::Clear => self.run_clear_content(),
        }
    }

    /// Disables the master switch and persists it; the controller stops the active batch via the
    /// shared cancellation flag. The index files are left in place until the user clears them.
    pub fn disable_content_index(&self) -> Result<(), String> {
        if !self.get_config().content_index_enabled {
            return Ok(());
        }
        // The switch is flipped on the live configuration under the apply lock, so a concurrent
        // submission is never overwritten by the snapshot this call read before it.
        self.apply_config_change(true, |current| {
            let mut next = current.clone();
            next.content_index_enabled = false;
            Ok(next)
        })
    }

    /// Signals any active `CairnWorker` subprocess to terminate on service shutdown without
    /// mutating the persisted configuration or removing the completed backfill marker.
    pub fn stop_content_worker(&self) {
        self.content_cancel.store(true, Ordering::SeqCst);
        let _ = self.content_commands.lock().unwrap().take();
    }

    /// Accepts a clear request. The controller processes it after any running batch so the worker
    /// never competes with another writer for the index lock.
    pub fn request_clear_content_index(&self) -> bool {
        let sender = self.content_commands.lock().unwrap();
        if let Some(tx) = sender.as_ref() {
            return tx.send(ContentCommand::Clear).is_ok();
        }
        drop(sender);
        if self.clear_content_index().is_ok() {
            self.set_content_state(ContentIndexState::Disabled);
            return true;
        }
        false
    }
}

impl MultiVolumeEngine {
    pub(super) fn drop_content_searcher(&self) {
        let _ = self.content_searcher.write().unwrap().take();
    }

    /// Removes leftovers from worker runs that were killed before cleaning up. Only transient
    /// batch files and old temp files are eligible; index segments, meta and the writer lock are
    /// never touched. The content thread is the only writer, so no batch is active here.
    pub(super) fn cleanup_stale_content_files(&self) {
        let Ok(entries) = std::fs::read_dir(&self.content_dir) else {
            return;
        };
        let now = SystemTime::now();
        let stale_after = Duration::from_secs(60 * 60);
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // The resume marker describes the index beside it; its age says nothing about whether
            // the backfill completed, so cleanup never removes it.
            if name == CONTENT_BACKFILL_MARKER {
                continue;
            }
            let is_batch = (name.starts_with("batch-") || name.starts_with("deletes-"))
                && name.ends_with(".jsonl");
            let is_stale_tmp = (name.starts_with(".tmp") || name == CONTENT_BACKFILL_MARKER_TMP)
                && entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age >= stale_after);
            if !is_batch && !is_stale_tmp {
                continue;
            }
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(
                    "Could not remove stale content file {}: {}",
                    path.display(),
                    err
                );
            }
        }
    }
}

impl MultiVolumeEngine {
    /// A failed incremental batch must not disable full-text search. The previously published
    /// searcher and its Ready state keep answering from the last committed snapshot, while the
    /// resume marker is removed because the drained USN changes will not be replayed on the next
    /// start; that start performs a full backfill and repairs the gap.
    pub(super) fn keep_last_content_snapshot(&self, error: String) {
        self.remove_backfill_marker();
        tracing::warn!(
            "content incremental batch failed; keeping the last committed snapshot: {error}"
        );
    }

    fn run_clear_content(&self) {
        self.drop_content_searcher();
        // A cleared index is not a completed backfill; a restart must rebuild instead of resume.
        self.remove_backfill_marker();
        self.set_content_progress(0, 0);
        match self.run_worker_batch(&[], &[], true) {
            BatchOutcome::Completed => {
                let mut status = self.content_status.write().unwrap();
                status.state = ContentIndexState::Disabled;
                status.indexed_documents = 0;
                status.index_bytes = 0;
                status.last_updated_ts = 0;
            }
            BatchOutcome::Cancelled => self.set_content_state(ContentIndexState::Disabled),
            BatchOutcome::Failed(failure) => self.set_content_error(failure.message),
        }
    }

    pub(super) fn reopen_content_index_ready(&self) {
        // Closing or clearing can land after the last batch finished. Re-check before publishing
        // Ready so a late batch cannot resurrect a switch the user already turned off.
        if self.content_cancel.load(Ordering::SeqCst)
            || !self.config.read().unwrap().content_index_enabled
        {
            self.drop_content_searcher();
            self.set_content_state(ContentIndexState::Disabled);
            return;
        }
        match ContentSearcher::open(&self.content_dir) {
            Ok(searcher) => {
                if self.content_cancel.load(Ordering::SeqCst)
                    || !self.config.read().unwrap().content_index_enabled
                {
                    self.drop_content_searcher();
                    self.set_content_state(ContentIndexState::Disabled);
                    return;
                }
                let documents = match searcher.document_count() {
                    Ok(documents) => documents,
                    Err(err) => {
                        // A published Ready with no readable count answers `content:` queries as
                        // empty. Report the failure instead: the index exists but cannot be read.
                        self.set_content_error(format!(
                            "content index could not be read at {}: {err}",
                            self.content_dir.display()
                        ));
                        return;
                    }
                };
                let bytes = searcher.index_size_bytes();
                self.set_content_searcher(Arc::new(searcher));
                self.set_content_ready(documents, bytes);
            }
            Err(err) => self.set_content_error(format!(
                "content index could not be reopened at {}: {err}",
                self.content_dir.display()
            )),
        }
    }
}

impl MultiVolumeEngine {
    /// Test seam: tests build a writable content index directly, so they mark the state ready.
    #[cfg(test)]
    pub fn mark_content_index_ready_for_test(&self) {
        let documents = self
            .content_searcher()
            .map(|searcher| searcher.document_count().unwrap_or(0))
            .unwrap_or(0);
        self.set_content_ready(documents, 0);
    }
}

pub(super) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

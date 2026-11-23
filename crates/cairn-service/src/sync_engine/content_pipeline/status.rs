// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Content index lifecycle: searcher access, published status, backfill marker and commands.

use super::{
    BackfillMarker, BatchOutcome, CONTENT_BACKFILL_MARKER, CONTENT_BACKFILL_MARKER_TMP,
    CONTENT_BACKFILL_SCHEMA, ContentCommand, ContentPipeline,
};
use cairn_content::{ContentIndexError, ContentSearcher, ContentWriter};
use cairn_protocol::status::{ContentIndexState, ContentIndexStatus};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::ServiceConfig;
use crate::sync_engine::MultiVolumeEngine;

/// Sums the content index files currently on disk, without following links. Used only on
/// lifecycle transitions, never by the status poll.
pub(super) fn content_dir_bytes(path: &Path) -> u64 {
    fn walk(dir: &Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                if file_type.is_file() {
                    entry.metadata().ok().map(|meta| meta.len())
                } else if file_type.is_dir() {
                    Some(walk(&entry.path()))
                } else {
                    None
                }
            })
            .sum()
    }
    walk(path)
}

impl ContentPipeline {
    pub(super) fn send_command(&self, command: ContentCommand) -> bool {
        let sender = self.commands.lock().unwrap();
        let Some(tx) = sender.as_ref() else {
            return false;
        };
        let failure_is_visible = !matches!(command, ContentCommand::Cancel);
        let delivered = tx.send(command).is_ok();
        drop(sender);
        if !delivered {
            tracing::error!(
                "content command could not be delivered: the content thread has stopped"
            );
            if failure_is_visible {
                self.set_error("content indexing stopped unexpectedly".to_string());
            }
        }
        delivered
    }

    pub(in crate::sync_engine) fn apply_switch(&self, previous_enabled: bool, next_enabled: bool) {
        if previous_enabled != next_enabled {
            if next_enabled {
                self.cancel.store(false, Ordering::SeqCst);
                self.set_progress(0, 0);
                self.send_command(ContentCommand::Start);
            } else {
                self.cancel.store(true, Ordering::SeqCst);
                self.set_disabled();
                self.send_command(ContentCommand::Cancel);
            }
        } else if next_enabled && matches!(self.status().state, ContentIndexState::Error(_)) {
            self.cancel.store(false, Ordering::SeqCst);
            self.set_progress(0, 0);
            self.send_command(ContentCommand::Start);
        }
    }

    pub(super) fn reset_cancel(&self) {
        self.cancel.store(false, Ordering::SeqCst);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn set_searcher(&self, searcher: Arc<ContentSearcher>) {
        *self.searcher.write().unwrap() = Some(searcher);
    }

    pub(super) fn searcher(&self) -> Option<Arc<ContentSearcher>> {
        self.searcher.read().unwrap().clone()
    }

    pub(super) fn drop_searcher(&self) {
        let _ = self.searcher.write().unwrap().take();
    }

    /// Opens the existing index read-only. The service must never hold a Tantivy writer.
    pub(super) fn get_or_create_searcher(&self) -> Option<Arc<ContentSearcher>> {
        if let Some(searcher) = self.searcher() {
            return Some(searcher);
        }

        match ContentSearcher::open(&self.dir) {
            Ok(searcher) => {
                let arc = Arc::new(searcher);
                *self.searcher.write().unwrap() = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::warn!("Failed to open ContentSearcher at {:?}: {}", self.dir, e);
                None
            }
        }
    }

    pub(super) fn clear_index_inline(&self) -> Result<(), ContentIndexError> {
        self.drop_searcher();
        self.remove_backfill_marker();
        let index = ContentWriter::open_or_create(&self.dir)?;
        let cleared = index.clear();
        let finished = index.finish();
        cleared?;
        finished
    }

    pub(super) fn set_state(&self, state: ContentIndexState) {
        self.status.write().unwrap().state = state;
    }

    pub(super) fn set_disabled(&self) {
        let bytes = content_dir_bytes(&self.dir);
        let mut status = self.status.write().unwrap();
        status.state = ContentIndexState::Disabled;
        status.indexed_documents = 0;
        status.index_bytes = bytes;
        status.last_updated_ts = 0;
    }

    pub(super) fn clear_disabled(&self) {
        let mut status = self.status.write().unwrap();
        status.state = ContentIndexState::Disabled;
        status.indexed_documents = 0;
        status.index_bytes = 0;
        status.last_updated_ts = 0;
    }

    pub(super) fn set_progress(&self, processed: u64, total: u64) {
        self.status.write().unwrap().state = ContentIndexState::Backfilling { processed, total };
    }

    pub(super) fn set_ready(&self, documents: u64, bytes: u64) {
        let mut status = self.status.write().unwrap();
        status.state = ContentIndexState::Ready;
        status.indexed_documents = documents;
        status.index_bytes = bytes;
        status.last_updated_ts = unix_now();
    }

    pub(super) fn set_error(&self, message: String) {
        self.remove_backfill_marker();
        self.status.write().unwrap().state = ContentIndexState::Error(message);
    }

    pub(super) fn backfill_marker_path(&self) -> PathBuf {
        self.dir.join(CONTENT_BACKFILL_MARKER)
    }

    pub(super) fn remove_backfill_marker(&self) {
        for name in [CONTENT_BACKFILL_MARKER, CONTENT_BACKFILL_MARKER_TMP] {
            let path = self.dir.join(name);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => tracing::warn!("Could not remove {}: {}", path.display(), err),
            }
        }
    }

    pub(super) fn write_backfill_marker(&self, cfg: &ServiceConfig) {
        let status = self.status();
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
        let tmp = self.dir.join(CONTENT_BACKFILL_MARKER_TMP);
        let persisted = std::fs::create_dir_all(&self.dir)
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

    pub(super) fn try_resume_index(&self, cfg: &ServiceConfig) -> bool {
        let _ = std::fs::remove_file(self.dir.join(CONTENT_BACKFILL_MARKER_TMP));

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

        match ContentSearcher::open(&self.dir) {
            Ok(searcher) => {
                let documents = match searcher.document_count() {
                    Ok(documents) => documents,
                    Err(err) => {
                        tracing::warn!(
                            "Could not read the content index at {}: {}; rebuilding",
                            self.dir.display(),
                            err
                        );
                        self.remove_backfill_marker();
                        return false;
                    }
                };
                if documents == 0 {
                    tracing::warn!(
                        "Content index at {} is empty despite a completed backfill; rebuilding",
                        self.dir.display()
                    );
                    self.remove_backfill_marker();
                    return false;
                }
                let bytes = searcher.index_size_bytes();
                self.set_searcher(Arc::new(searcher));
                self.set_ready(documents, bytes);
                tracing::info!(
                    "Resumed the content index from the completed backfill ({documents} documents)"
                );
                true
            }
            Err(err) => {
                tracing::warn!(
                    "Could not resume the content index at {}: {}; rebuilding",
                    self.dir.display(),
                    err
                );
                self.remove_backfill_marker();
                false
            }
        }
    }

    pub(super) fn cleanup_stale_files(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        let stale_after = Duration::from_secs(60 * 60);
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
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

    pub(super) fn keep_last_snapshot(&self, error: &str) {
        self.remove_backfill_marker();
        tracing::warn!(
            "content incremental batch failed; keeping the last committed snapshot: {error}"
        );
    }

    pub(super) fn reopen_ready(&self, enabled_in_config: impl Fn() -> bool) {
        if self.is_cancelled() || !enabled_in_config() {
            self.drop_searcher();
            self.set_disabled();
            return;
        }
        match ContentSearcher::open(&self.dir) {
            Ok(searcher) => {
                if self.is_cancelled() || !enabled_in_config() {
                    self.drop_searcher();
                    self.set_disabled();
                    return;
                }
                let documents = match searcher.document_count() {
                    Ok(documents) => documents,
                    Err(err) => {
                        self.set_error(format!(
                            "content index could not be read at {}: {err}",
                            self.dir.display()
                        ));
                        return;
                    }
                };
                let bytes = searcher.index_size_bytes();
                self.set_searcher(Arc::new(searcher));
                self.set_ready(documents, bytes);
            }
            Err(err) => self.set_error(format!(
                "content index could not be reopened at {}: {err}",
                self.dir.display()
            )),
        }
    }
}

impl MultiVolumeEngine {
    #[cfg(test)]
    pub(super) fn content_searcher(&self) -> Option<Arc<ContentSearcher>> {
        self.content_pipeline.searcher()
    }

    /// Opens the existing index read-only. The service answers queries and must never own a
    /// Tantivy writer: a writer locks the directory, starts merge threads and rewrites meta.json.
    pub(crate) fn get_or_create_content_searcher(&self) -> Option<Arc<ContentSearcher>> {
        self.content_pipeline.get_or_create_searcher()
    }

    /// Clears the entire full-text content index (CNT-07).
    pub fn clear_content_index(&self) -> Result<(), ContentIndexError> {
        self.content_pipeline.clear_index_inline()
    }

    pub fn content_index_status(&self) -> ContentIndexStatus {
        self.content_pipeline.status()
    }

    fn set_content_state(&self, state: ContentIndexState) {
        self.content_pipeline.set_state(state);
    }

    pub(super) fn set_content_disabled(&self) {
        self.content_pipeline.set_disabled();
    }

    pub(super) fn set_content_progress(&self, processed: u64, total: u64) {
        self.content_pipeline.set_progress(processed, total);
    }

    pub(super) fn set_content_error(&self, message: String) {
        self.content_pipeline.set_error(message);
    }

    #[cfg(test)]
    pub(super) fn backfill_marker_path(&self) -> PathBuf {
        self.content_pipeline.backfill_marker_path()
    }

    pub(super) fn remove_backfill_marker(&self) {
        self.content_pipeline.remove_backfill_marker();
    }

    pub(super) fn write_backfill_marker(&self, cfg: &ServiceConfig) {
        self.content_pipeline.write_backfill_marker(cfg);
    }

    pub(super) fn try_resume_content_index(&self) -> bool {
        let cfg = self.get_config();
        self.content_pipeline.try_resume_index(&cfg)
    }

    pub fn ensure_content_runtime(self: &Arc<Self>) {
        let mut guard = self.content_pipeline.commands.lock().unwrap();
        if guard.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let engine = self.clone();
        let spawned = std::thread::Builder::new()
            .name("cairn-content".to_string())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    engine.handle_content_command(command);
                }
            });
        match spawned {
            Ok(_) => {
                *guard = Some(tx);
                drop(guard);
            }
            Err(err) => {
                drop(guard);
                self.set_content_error(format!("could not start content worker thread: {err}"));
                return;
            }
        }

        if !self.config.read().unwrap().content_index_enabled {
            return;
        }
        self.cleanup_stale_content_files();
        if self.try_resume_content_index() {
            return;
        }
        self.send_content_command(ContentCommand::Start);
    }

    fn handle_content_command(&self, command: ContentCommand) {
        match command {
            ContentCommand::Start => self.run_full_backfill(),
            ContentCommand::IndexChanges { tasks, deletions } => {
                self.run_incremental(&tasks, &deletions);
            }
            ContentCommand::Cancel => {
                self.content_pipeline.cancel.store(true, Ordering::SeqCst);
                self.remove_backfill_marker();
                self.drop_content_searcher();
                self.set_content_disabled();
            }
            ContentCommand::Clear => self.run_clear_content(),
        }
    }

    pub fn disable_content_index(&self) -> Result<(), String> {
        if !self.get_config().content_index_enabled {
            return Ok(());
        }
        self.apply_config_change(true, |current| {
            let mut next = current.clone();
            next.content_index_enabled = false;
            Ok(next)
        })
    }

    /// Signals any active `CairnWorker` subprocess to terminate on service shutdown.
    pub fn stop_content_worker(&self) {
        self.content_pipeline.shutdown.store(true, Ordering::SeqCst);
        self.content_pipeline.cancel.store(true, Ordering::SeqCst);
        let _ = self.content_pipeline.commands.lock().unwrap().take();
    }

    pub fn request_clear_content_index(&self) -> bool {
        let sender = self.content_pipeline.commands.lock().unwrap();
        if let Some(tx) = sender.as_ref() {
            return tx.send(ContentCommand::Clear).is_ok();
        }
        drop(sender);
        if self.clear_content_index().is_ok() {
            self.content_pipeline.clear_disabled();
            return true;
        }
        false
    }

    pub(super) fn drop_content_searcher(&self) {
        self.content_pipeline.drop_searcher();
    }

    pub(super) fn cleanup_stale_content_files(&self) {
        self.content_pipeline.cleanup_stale_files();
    }

    pub(super) fn keep_last_content_snapshot(&self, error: &str) {
        self.content_pipeline.keep_last_snapshot(error);
    }

    pub(super) fn run_clear_content(&self) {
        self.run_clear_content_with(&super::worker_process::SubprocessWorkerPort::for_clear(
            self,
        ));
    }

    pub(super) fn run_clear_content_with(
        &self,
        port: &dyn super::worker_process::WorkerProcessPort,
    ) {
        self.drop_content_searcher();
        self.remove_backfill_marker();
        self.set_content_progress(0, 0);
        match self.run_worker_batch_with(port, &[], &[], true) {
            BatchOutcome::Completed => self.content_pipeline.clear_disabled(),
            BatchOutcome::Cancelled => self.set_content_state(ContentIndexState::Disabled),
            BatchOutcome::Failed(failure) => self.set_content_error(failure.message),
        }
    }

    pub(in crate::sync_engine) fn reopen_content_index_ready(&self) {
        self.content_pipeline
            .reopen_ready(|| self.config.read().unwrap().content_index_enabled);
    }
}

pub(super) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

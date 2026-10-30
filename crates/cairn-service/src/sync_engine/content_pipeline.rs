// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Full-text content index pipeline (CNT-01, CNT-04, CNT-05, CNT-06, CNT-07).
//!
//! The service owns MFT discovery and decides which files are eligible. Extraction and Tantivy
//! writes happen in `CairnWorker.exe`; this module only builds batches, drives the worker and
//! publishes a state machine the GUI can show. The worker is never handed the LocalSystem
//! service's address space, so a malformed document cannot take the service down.

use super::MultiVolumeEngine;
use cairn_content::{ContentIndex, ContentIndexError, ContentSearcher};
use cairn_core::{FLAG_DIRECTORY, FLAG_REPARSE_POINT};
use cairn_protocol::status::{ContentIndexState, ContentIndexStatus, ContentIndexTask};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::ServiceConfig;

/// Files handed to one worker process. A worker restart is cheap; a batch small enough to finish
/// within the timeout keeps a single malformed file from stalling the whole backfill.
const CONTENT_BATCH_SIZE: usize = 500;
/// Records inspected per lock acquisition while collecting a batch.
const CONTENT_SCAN_CHUNK: usize = 2_000;
/// Upper bound for one worker batch. Cancellation is checked every 100 ms while waiting.
const CONTENT_WORKER_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(windows)]
fn lower_worker_priority(child: &std::process::Child) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_SET_INFORMATION,
    };

    // SAFETY: the handle is opened for the child this process just spawned and is closed below.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SET_INFORMATION, false, child.id()) }) else {
        return;
    };
    // SAFETY: the handle is live; this only changes the child's scheduling/I/O priority.
    let _ = unsafe { SetPriorityClass(handle, PROCESS_MODE_BACKGROUND_BEGIN) };
    // SAFETY: paired with the successful OpenProcess above.
    let _ = unsafe { CloseHandle(handle) };
}

/// Format version of the completed-backfill marker. Bump it when the content index layout
/// changes; an old marker then forces a full rebuild instead of trusting an incompatible index.
const CONTENT_BACKFILL_SCHEMA: u32 = 2;
/// Proof that a full backfill with the current configuration finished and published Ready.
const CONTENT_BACKFILL_MARKER: &str = "backfill.complete.json";
/// Half-written replacement, renamed over the marker only after its payload is on disk.
const CONTENT_BACKFILL_MARKER_TMP: &str = "backfill.complete.json.tmp";

/// Commands handled by the single content thread. The thread owns the full-backfill state machine
/// so the pipe server never blocks on a disk-sized job.
pub enum ContentCommand {
    Start,
    IndexChanges {
        tasks: Vec<ContentIndexTask>,
        deletions: Vec<(u64, u64)>,
    },
    Cancel,
    Clear,
}

enum BatchOutcome {
    Completed,
    Cancelled,
    Failed(String),
}

/// Marker written after a full backfill reaches Ready. It records the configuration the index was
/// built with, not a live checksum: incremental updates deliberately do not rewrite it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackfillMarker {
    schema: u32,
    extensions: Vec<String>,
    max_bytes: u64,
    excluded_paths: Vec<String>,
    content_excluded_paths: Vec<String>,
    documents: u64,
    completed_ts: i64,
}

impl MultiVolumeEngine {
    fn set_content_searcher(&self, searcher: Arc<ContentSearcher>) {
        *self.content_searcher.write().unwrap() = Some(searcher);
    }

    fn content_searcher(&self) -> Option<Arc<ContentSearcher>> {
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

    pub(super) fn set_content_state(&self, state: ContentIndexState) {
        let mut status = self.content_status.write().unwrap();
        status.state = state;
    }

    pub(super) fn set_content_progress(&self, processed: u64, total: u64) {
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

    fn set_content_error(&self, message: String) {
        // A failed batch may have drained changes that no later USN replay will return. Keep the
        // failure from being resumed as if the index were complete; the next start rebuilds.
        self.remove_backfill_marker();
        let mut status = self.content_status.write().unwrap();
        status.state = ContentIndexState::Error(message);
    }

    fn backfill_marker_path(&self) -> PathBuf {
        self.content_dir.join(CONTENT_BACKFILL_MARKER)
    }

    /// Removes the resume marker and any half-written replacement. Used whenever the index may no
    /// longer describe a completed full backfill (new backfill, cancel, clear).
    fn remove_backfill_marker(&self) {
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
    /// instead of clearing and re-extracting every eligible file. The marker is replaced
    /// atomically; a write failure is logged but never turns the already published index into an
    /// error, because the running index is still fully usable.
    fn write_backfill_marker(&self) {
        let status = self.content_index_status();
        if !status.state.is_ready() {
            return;
        }
        let cfg = self.get_config();
        let marker = BackfillMarker {
            schema: CONTENT_BACKFILL_SCHEMA,
            extensions: cfg.content_index_extensions,
            max_bytes: cfg.content_index_max_bytes,
            excluded_paths: cfg.excluded_paths,
            content_excluded_paths: cfg.content_excluded_paths,
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
    fn try_resume_content_index(&self) -> bool {
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
                let documents = searcher.document_count().unwrap_or(0);
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
        let _ = tx.send(ContentCommand::Start);
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
        let mut cfg = self.get_config();
        if !cfg.content_index_enabled {
            return Ok(());
        }
        cfg.content_index_enabled = false;
        let config_toml = toml::to_string_pretty(&cfg)
            .map_err(|err| format!("could not serialize service configuration: {err}"))?;
        self.apply_config_toml(&config_toml)
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

    /// Number of eligible records, used for the backfill denominator. Uses the same task builder
    /// as the scan so path exclusions cannot make the progress total disagree with the work.
    fn count_eligible(&self, cfg: &ServiceConfig) -> u64 {
        let volumes = self.volumes.read().unwrap();
        volumes
            .values()
            .map(|volume| {
                (0..volume.records.len())
                    .filter(|&index| self.content_task(volume, index, cfg).is_some())
                    .count() as u64
            })
            .sum()
    }

    fn content_eligible(volume: &super::VolumeIndex, index: usize, cfg: &ServiceConfig) -> bool {
        let Some(rec) = volume.records.get(index) else {
            return false;
        };
        if rec.is_tombstone()
            || rec.is_dir()
            // A reparse point's target can lie outside the volume, outside the size limit or in
            // an excluded directory, and the worker follows the path it is handed.
            || rec.is_reparse_point()
            || rec.size > cfg.content_index_max_bytes
        {
            return false;
        }
        let ext = volume
            .arena
            .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        cfg.content_index_extensions
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&ext))
    }

    fn record_path(volume: &super::VolumeIndex, index: usize) -> String {
        volume.path_cache.get_full_path(
            &volume.records,
            &volume.arena,
            volume.drive_letter,
            index as u32,
        )
    }

    fn is_self_data_path(&self, path: &str) -> bool {
        self.self_data_dirs
            .iter()
            .any(|root| path_is_under(path, root))
    }

    fn content_task(
        &self,
        volume: &super::VolumeIndex,
        index: usize,
        cfg: &ServiceConfig,
    ) -> Option<ContentIndexTask> {
        if !Self::content_eligible(volume, index, cfg) {
            return None;
        }
        let rec = &volume.records[index];
        let path = Self::record_path(volume, index);
        if self.is_self_data_path(&path)
            || crate::exclusion::is_path_excluded(&path, &cfg.excluded_paths)
            || crate::exclusion::is_path_excluded(&path, &cfg.content_excluded_paths)
        {
            return None;
        }
        Some(ContentIndexTask {
            volume_id: rec.volume_id as u64,
            frn: rec.frn,
            size: rec.size,
            modified_ts: rec.modified_ts,
            path,
        })
    }

    /// One bounded scan of a volume. Returns the next record index so a volume is never copied
    /// into a second full-sized task list.
    fn next_content_batch(
        &self,
        drive: char,
        start: usize,
        cfg: &ServiceConfig,
    ) -> (Vec<ContentIndexTask>, usize) {
        let volumes = self.volumes.read().unwrap();
        let Some(volume) = volumes.get(&drive) else {
            return (Vec::new(), start);
        };
        let end = (start + CONTENT_SCAN_CHUNK).min(volume.records.len());
        let mut tasks = Vec::new();
        let mut cursor = start;
        while cursor < end && tasks.len() < CONTENT_BATCH_SIZE {
            if let Some(task) = self.content_task(volume, cursor, cfg) {
                tasks.push(task);
            }
            cursor += 1;
        }
        (tasks, cursor)
    }

    /// Returns the replacement tasks and the documents whose record no longer qualifies (renamed
    /// out of the whitelist, grew past the limit, became excluded, or was tombstoned).
    fn changed_content_tasks(
        &self,
        changes: &HashMap<char, HashSet<u64>>,
        cfg: &ServiceConfig,
    ) -> (Vec<ContentIndexTask>, Vec<(u64, u64)>) {
        let volumes = self.volumes.read().unwrap();
        let mut tasks = Vec::new();
        let mut deletions = HashSet::new();
        for (drive, frns) in changes {
            let Some(volume) = volumes.get(drive) else {
                continue;
            };
            for frn in frns {
                let Some(indices) = volume.frn_to_idx.get(frn) else {
                    continue;
                };
                for &index in indices {
                    let index = index as usize;
                    // The service's own writes are not user content. They must not create tasks
                    // and must not create deletion commits either: a commit rewrites `meta.json`,
                    // whose change event would otherwise start another incremental cycle.
                    if self.is_self_data_path(&Self::record_path(volume, index)) {
                        continue;
                    }
                    match self.content_task(volume, index, cfg) {
                        Some(task) => tasks.push(task),
                        None => {
                            deletions.insert((volume.content_volume_id(), *frn));
                        }
                    }
                }
            }
        }
        (tasks, deletions.into_iter().collect())
    }

    fn drain_pending_content_deletions(&self) -> HashSet<(u64, u64)> {
        let mut lock = self.volumes.write().unwrap();
        lock.values_mut()
            .flat_map(|volume| volume.flush_pending_content_deletions())
            .collect()
    }

    /// A deleted FRN can be reused before the debounce fires. Keep the deletion only when no live
    /// eligible record now owns that FRN; otherwise the replacement task is about to overwrite it.
    fn resolve_content_deletions(
        &self,
        pending: HashSet<(u64, u64)>,
        cfg: &ServiceConfig,
    ) -> Vec<(u64, u64)> {
        let volumes = self.volumes.read().unwrap();
        let mut out: Vec<(u64, u64)> = pending
            .into_iter()
            .filter(|(volume_id, frn)| {
                let Some(volume) = volumes
                    .values()
                    .find(|volume| volume.content_volume_id() == *volume_id)
                else {
                    return true;
                };
                // A deleted batch file, log or index segment never had a document worth committing
                // a deletion for; replaying that churn would rewrite the index it belongs to.
                if volume.frn_to_idx.get(frn).is_some_and(|indices| {
                    indices.iter().all(|&index| {
                        self.is_self_data_path(&Self::record_path(volume, index as usize))
                    })
                }) {
                    return false;
                }
                !volume.frn_to_idx.get(frn).is_some_and(|indices| {
                    indices
                        .iter()
                        .any(|&index| self.content_task(volume, index as usize, cfg).is_some())
                })
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Batches and drains pending data changes across all volumes (CNT-05).
    pub fn drain_pending_data_changes(&self) -> HashMap<char, HashSet<u64>> {
        let mut result = HashMap::new();
        let mut lock = self.volumes.write().unwrap();
        for (&drive, vol) in lock.iter_mut() {
            let changes = vol.flush_pending_data_changes();
            if !changes.is_empty() {
                result.insert(drive, changes);
            }
        }
        result
    }

    /// Reads size, timestamps and the directory/reparse bits for records whose journal events do
    /// not carry them.
    ///
    /// A USN `Created` record has no file size and only the event time, so without this pass every
    /// file created while the service was away would keep `0 B` until something touched it again.
    /// The same pass re-reads the attribute bits, so a create whose journal record lost them still
    /// stops a symlink from looking like a regular file. Paths are resolved under the read lock,
    /// the filesystem is queried without any lock, and the records are updated under the write
    /// lock by `(drive, index, frn)`.
    fn refresh_pending_metadata(&self, changes: &HashMap<char, HashSet<u64>>) {
        let mut pending: Vec<(char, u32, u64, String)> = Vec::new();
        {
            let lock = self.volumes.read().unwrap();
            for (drive, frns) in changes {
                let Some(volume) = lock.get(drive) else {
                    continue;
                };
                for frn in frns {
                    let Some(indices) = volume.frn_to_idx.get(frn) else {
                        continue;
                    };
                    for &index in indices {
                        let record = &volume.records[index as usize];
                        if record.is_tombstone() {
                            continue;
                        }
                        let path = volume.path_cache.get_full_path(
                            &volume.records,
                            &volume.arena,
                            *drive,
                            index,
                        );
                        pending.push((*drive, index, *frn, path));
                    }
                }
            }
        }
        if pending.is_empty() {
            return;
        }

        struct RefreshedMetadata {
            drive: char,
            index: u32,
            frn: u64,
            size: u64,
            created_ts: i64,
            modified_ts: i64,
            is_dir: bool,
            is_reparse: bool,
        }

        let updates: Vec<RefreshedMetadata> = pending
            .into_iter()
            .filter_map(|(drive, index, frn, path)| {
                let metadata = std::fs::symlink_metadata(path).ok()?;
                let (is_dir, is_reparse) = directory_and_reparse(&metadata);
                Some(RefreshedMetadata {
                    drive,
                    index,
                    frn,
                    size: if is_dir { 0 } else { metadata.len() },
                    created_ts: crate::filetime::system_time_to_filetime(metadata.created()),
                    modified_ts: crate::filetime::system_time_to_filetime(metadata.modified()),
                    is_dir,
                    is_reparse,
                })
            })
            .collect();

        let mut lock = self.volumes.write().unwrap();
        for update in updates {
            let Some(volume) = lock.get_mut(&update.drive) else {
                continue;
            };
            let Some(record) = volume.records.get_mut(update.index as usize) else {
                continue;
            };
            if record.frn != update.frn || record.is_tombstone() {
                continue;
            }
            let was_dir = record.is_dir();
            // Only the bits the filesystem owns are replaced; tombstone and the other record flags
            // keep their current value.
            let mut flags = record.flags & !(FLAG_DIRECTORY | FLAG_REPARSE_POINT);
            if update.is_dir {
                flags |= FLAG_DIRECTORY;
            }
            if update.is_reparse {
                flags |= FLAG_REPARSE_POINT;
            }
            let metadata_changed = record.flags != flags
                || record.created_ts != update.created_ts
                || record.modified_ts != update.modified_ts;
            record.flags = flags;
            record.created_ts = update.created_ts;
            record.modified_ts = update.modified_ts;
            let kind_changed = was_dir != update.is_dir;
            if kind_changed {
                // The size now means the other thing. Recompute after this record is updated; the
                // metadata pass cannot describe a directory's aggregate.
                record.size = if update.is_dir { 0 } else { update.size };
            }
            if metadata_changed {
                volume.presorted_dirty = true;
            }
            if kind_changed {
                // The whole batch shares one write lock; marking here lets the loop finish and the
                // aggregation run once below instead of once per converted record.
                volume.mark_directory_sizes_dirty();
            } else if !update.is_dir {
                volume.update_file_size(update.index, update.size);
            }
        }
        for volume in lock.values_mut() {
            volume.ensure_directory_sizes();
        }
    }

    /// Processes debounced data changes. Metadata is display data and is refreshed even when the
    /// content switch is off; eligible changed files are handed to the content thread instead of
    /// being extracted inline in the service.
    pub fn process_pending_data_changes(&self) {
        let changes_by_drive = self.drain_pending_data_changes();
        let pending_deletions = self.drain_pending_content_deletions();
        if changes_by_drive.is_empty() && pending_deletions.is_empty() {
            return;
        }

        if !changes_by_drive.is_empty() {
            self.refresh_pending_metadata(&changes_by_drive);
        }

        let cfg = self.config.read().unwrap().clone();
        if !cfg.content_index_enabled {
            return;
        }
        if matches!(
            self.content_index_status().state,
            ContentIndexState::Disabled | ContentIndexState::Error(_)
        ) {
            return;
        }

        let (tasks, ineligible) = self.changed_content_tasks(&changes_by_drive, &cfg);
        let mut pending = pending_deletions;
        pending.extend(ineligible);
        let deletions = self.resolve_content_deletions(pending, &cfg);
        if tasks.is_empty() && deletions.is_empty() {
            return;
        }
        let sender = self.content_commands.lock().unwrap();
        if let Some(tx) = sender.as_ref() {
            let _ = tx.send(ContentCommand::IndexChanges { tasks, deletions });
        }
    }

    fn drop_content_searcher(&self) {
        let _ = self.content_searcher.write().unwrap().take();
    }

    /// Removes leftovers from worker runs that were killed before cleaning up. Only transient
    /// batch files and old temp files are eligible; index segments, meta and the writer lock are
    /// never touched. The content thread is the only writer, so no batch is active here.
    fn cleanup_stale_content_files(&self) {
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

    fn run_full_backfill(&self) {
        self.content_cancel.store(false, Ordering::SeqCst);
        self.drop_content_searcher();
        // A new backfill invalidates any previous completion until it reaches Ready again; a
        // cancelled or failed run must not leave a marker that a later restart would trust.
        self.remove_backfill_marker();
        self.cleanup_stale_content_files();

        let cfg = self.get_config();
        if !cfg.content_index_enabled {
            self.set_content_state(ContentIndexState::Disabled);
            return;
        }

        let total = self.count_eligible(&cfg);
        self.set_content_progress(0, total);

        let drives: Vec<char> = self.volumes.read().unwrap().keys().copied().collect();
        let mut processed = 0u64;
        let mut cleared = false;

        for drive in drives {
            let mut start = 0usize;
            loop {
                if self.content_cancel.load(Ordering::SeqCst) {
                    self.set_content_state(ContentIndexState::Disabled);
                    return;
                }
                let (tasks, next) = self.next_content_batch(drive, start, &cfg);
                if tasks.is_empty() {
                    if next <= start {
                        break;
                    }
                    start = next;
                    continue;
                }
                let clear = !cleared;
                match self.run_worker_batch(&tasks, &[], clear) {
                    BatchOutcome::Completed => {
                        cleared = true;
                        processed += tasks.len() as u64;
                        self.set_content_progress(processed, total);
                    }
                    BatchOutcome::Cancelled => {
                        self.set_content_state(ContentIndexState::Disabled);
                        return;
                    }
                    BatchOutcome::Failed(err) => {
                        self.set_content_error(err);
                        return;
                    }
                }
                start = next;
            }
        }

        // An empty eligible set still has to clear a previous index through the worker.
        if !cleared {
            match self.run_worker_batch(&[], &[], true) {
                BatchOutcome::Completed => {}
                BatchOutcome::Cancelled => {
                    self.set_content_state(ContentIndexState::Disabled);
                    return;
                }
                BatchOutcome::Failed(err) => {
                    self.set_content_error(err);
                    return;
                }
            }
        }

        if self.content_cancel.load(Ordering::SeqCst) {
            self.set_content_state(ContentIndexState::Disabled);
            return;
        }
        self.reopen_content_index_ready();
        // Only a published Ready index is a reusable backfill. `write_backfill_marker` re-checks
        // the state, so a reopen failure leaves no marker behind.
        self.write_backfill_marker();
    }

    fn run_incremental(&self, tasks: Vec<ContentIndexTask>, deletions: Vec<(u64, u64)>) {
        self.run_incremental_with(tasks, deletions, |tasks, deletions, clear| {
            self.run_worker_batch(tasks, deletions, clear)
        });
    }

    /// The batch runner is a parameter so a test can exercise the failure policy without spawning
    /// `CairnWorker`; production always passes [`Self::run_worker_batch`].
    fn run_incremental_with<F>(
        &self,
        tasks: Vec<ContentIndexTask>,
        deletions: Vec<(u64, u64)>,
        run_batch: F,
    ) where
        F: Fn(&[ContentIndexTask], &[(u64, u64)], bool) -> BatchOutcome,
    {
        if tasks.is_empty() && deletions.is_empty() {
            return;
        }
        let cfg = self.get_config();
        if !cfg.content_index_enabled {
            self.set_content_state(ContentIndexState::Disabled);
            return;
        }
        if self.content_cancel.load(Ordering::SeqCst) {
            self.drop_content_searcher();
            self.set_content_state(ContentIndexState::Disabled);
            return;
        }

        // Incremental batches never leave Ready: the worker commits new segments while the
        // existing read-only searcher keeps answering from the last committed snapshot. Dropping
        // it here made `content:` queries return empty for every small USN burst.
        if tasks.is_empty() {
            // A deletion-only cycle still has to run the worker once; it never writes from the
            // service process.
            match run_batch(&[], &deletions, false) {
                BatchOutcome::Completed => {}
                BatchOutcome::Cancelled => {
                    self.drop_content_searcher();
                    self.set_content_state(ContentIndexState::Disabled);
                    return;
                }
                BatchOutcome::Failed(err) => {
                    self.keep_last_content_snapshot(err);
                    return;
                }
            }
        } else {
            let chunk_count = tasks.len().div_ceil(CONTENT_BATCH_SIZE);
            for (chunk_index, chunk) in tasks.chunks(CONTENT_BATCH_SIZE).enumerate() {
                if self.content_cancel.load(Ordering::SeqCst) {
                    self.drop_content_searcher();
                    self.set_content_state(ContentIndexState::Disabled);
                    return;
                }
                // Deletions travel with the last chunk so one worker process applies the whole
                // incremental cycle when possible.
                let chunk_deletions = if chunk_index + 1 == chunk_count {
                    deletions.as_slice()
                } else {
                    &[]
                };
                match run_batch(chunk, chunk_deletions, false) {
                    BatchOutcome::Completed => {}
                    BatchOutcome::Cancelled => {
                        self.drop_content_searcher();
                        self.set_content_state(ContentIndexState::Disabled);
                        return;
                    }
                    BatchOutcome::Failed(err) => {
                        self.keep_last_content_snapshot(err);
                        return;
                    }
                }
            }
        }

        // Replace the reader once the batch is fully committed and refresh the published
        // document count and size. A failure to reopen is reported as Error; the old searcher
        // stays unused behind a non-Ready state.
        self.reopen_content_index_ready();
    }

    /// A failed incremental batch must not disable full-text search. The previously published
    /// searcher and its Ready state keep answering from the last committed snapshot, while the
    /// resume marker is removed because the drained USN changes will not be replayed on the next
    /// start; that start performs a full backfill and repairs the gap.
    fn keep_last_content_snapshot(&self, error: String) {
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
            BatchOutcome::Failed(err) => self.set_content_error(err),
        }
    }

    fn reopen_content_index_ready(&self) {
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
                let documents = searcher.document_count().unwrap_or(0);
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

    fn worker_path() -> Result<PathBuf, String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("could not locate the service executable: {err}"))?;
        let dir = exe
            .parent()
            .ok_or_else(|| "service executable has no parent directory".to_string())?;
        // Production keeps both binaries side by side. Unit tests run from target/debug/deps, so
        // the parent directory is the cargo target directory that holds CairnWorker.exe.
        let mut search_dirs = vec![dir.to_path_buf()];
        if let Some(parent) = dir.parent() {
            search_dirs.push(parent.to_path_buf());
        }
        for base in search_dirs {
            for name in ["CairnWorker.exe", "CairnWorker"] {
                let candidate = base.join(name);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
        Err(format!(
            "CairnWorker executable not found beside {}",
            exe.display()
        ))
    }

    /// Runs one worker batch, retrying once when the failure looks like a transient sharing
    /// conflict. The previous searcher is dropped first so its mapped index segments cannot keep
    /// the files locked while the retry runs; re-running a batch (including `--clear`) is
    /// idempotent, so no request kind is special-cased.
    fn run_worker_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        let first = self.run_worker_process(tasks, deletions, clear);
        let err = match &first {
            BatchOutcome::Failed(err) if worker_error_is_transient(err) => err.clone(),
            _ => return first,
        };
        tracing::warn!(
            "CairnWorker hit a transient sharing error; releasing the searcher and retrying once: {err}"
        );
        self.drop_content_searcher();
        std::thread::sleep(Duration::from_millis(250));
        if self.content_cancel.load(Ordering::SeqCst) {
            return BatchOutcome::Cancelled;
        }
        self.run_worker_process(tasks, deletions, clear)
    }

    fn run_worker_process(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        let worker = match Self::worker_path() {
            Ok(path) => path,
            Err(err) => return BatchOutcome::Failed(err),
        };
        if let Err(err) = std::fs::create_dir_all(&self.content_dir) {
            return BatchOutcome::Failed(format!(
                "could not create content directory {}: {err}",
                self.content_dir.display()
            ));
        }

        let batch_path = self
            .content_dir
            .join(format!("batch-{}.jsonl", std::process::id()));
        let delete_path = (!deletions.is_empty()).then(|| {
            self.content_dir
                .join(format!("deletes-{}.jsonl", std::process::id()))
        });
        let cleanup_paths: Vec<PathBuf> = std::iter::once(batch_path.clone())
            .chain(delete_path.clone())
            .collect();

        if let Err(err) = write_json_lines(&batch_path, tasks) {
            remove_worker_files(&cleanup_paths);
            return BatchOutcome::Failed(err);
        }
        if let Some(path) = &delete_path {
            if let Err(err) = write_json_lines(path, deletions) {
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::Failed(err);
            }
        }

        let cfg = self.get_config();
        let mut command = Command::new(worker);
        command
            .arg("--index-dir")
            .arg(&self.content_dir)
            .arg("--batch")
            .arg(&batch_path)
            .arg("--max-bytes")
            .arg(cfg.content_index_max_bytes.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;
            command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS.0);
        }
        if let Some(path) = &delete_path {
            command.arg("--delete-batch").arg(path);
        }
        if clear {
            command.arg("--clear");
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::Failed(format!("could not start CairnWorker: {err}"));
            }
        };
        #[cfg(windows)]
        lower_worker_priority(&child);
        // Drain both stdout and stderr while the worker runs, capping kept bytes to 64 KiB and
        // discarding any excess so neither pipe can fill its OS buffer and deadlock the child or
        // exhaust service memory.
        const MAX_WORKER_PIPE_BYTES: u64 = 64 * 1024;
        let stdout_reader = child.stdout.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut capped = pipe.take(MAX_WORKER_PIPE_BYTES);
                let mut buf = Vec::new();
                let _ = capped.read_to_end(&mut buf);
                let mut rest = capped.into_inner();
                let _ = std::io::copy(&mut rest, &mut std::io::sink());
                String::from_utf8_lossy(&buf).into_owned()
            })
        });
        let stderr_reader = child.stderr.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut capped = pipe.take(MAX_WORKER_PIPE_BYTES);
                let mut buf = Vec::new();
                let _ = capped.read_to_end(&mut buf);
                let mut rest = capped.into_inner();
                let _ = std::io::copy(&mut rest, &mut std::io::sink());
                String::from_utf8_lossy(&buf).into_owned()
            })
        });
        let deadline = Instant::now() + CONTENT_WORKER_TIMEOUT;

        loop {
            if self.content_cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::Cancelled;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let output = stdout_reader
                        .and_then(|reader| reader.join().ok())
                        .unwrap_or_default();
                    let errors = stderr_reader
                        .and_then(|reader| reader.join().ok())
                        .unwrap_or_default();
                    remove_worker_files(&cleanup_paths);
                    if !status.success() {
                        return BatchOutcome::Failed(format!(
                            "CairnWorker exited with {status}: {output} {errors}"
                        ));
                    }
                    let summary: serde_json::Value =
                        serde_json::from_str(output.trim()).unwrap_or_default();
                    let processed = summary["processed"].as_u64().unwrap_or(0);
                    let failed = summary["failed"].as_u64().unwrap_or(0);
                    if failed > 0 {
                        // A file can vanish between the USN event and extraction, so a batch of
                        // temporarily created files may legitimately skip everything. The worker
                        // still exited cleanly and committed its deletions; only a non-zero exit
                        // is a batch failure. Treating all-skipped as fatal disabled content
                        // search on ordinary temporary-file churn.
                        tracing::warn!(
                            processed,
                            failed,
                            "CairnWorker skipped content files: {}",
                            errors.trim()
                        );
                    }
                    return BatchOutcome::Completed;
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    remove_worker_files(&cleanup_paths);
                    return BatchOutcome::Failed(format!("could not wait for CairnWorker: {err}"));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::Failed("CairnWorker timed out".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

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

/// Whether a worker failure looks like a file-handle conflict that can clear on its own: Tantivy
/// or Windows reporting access denied / a sharing violation. Anything else (bad arguments, a
/// crashed worker, a timeout) is not retried at this level.
fn worker_error_is_transient(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "permissiondenied",
        "os error 5",
        "拒绝访问",
        "sharing violation",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn write_json_lines<T: serde::Serialize>(path: &Path, items: &[T]) -> Result<(), String> {
    let mut file = std::fs::File::create(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    for item in items {
        let line = serde_json::to_string(item)
            .map_err(|err| format!("could not encode worker task: {err}"))?;
        writeln!(file, "{line}")
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    Ok(())
}

fn remove_worker_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Directory and reparse-point bits of an entry, read without following the final link.
fn directory_and_reparse(metadata: &std::fs::Metadata) -> (bool, bool) {
    #[cfg(windows)]
    {
        use cairn_ntfs::usn::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT};
        use std::os::windows::fs::MetadataExt;
        let attributes = metadata.file_attributes();
        (
            attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        )
    }
    #[cfg(not(windows))]
    {
        (metadata.is_dir(), metadata.file_type().is_symlink())
    }
}

/// Case-insensitive containment on directory boundaries, so `C:\ProgramData\Cairn2` is not
/// considered to be inside `C:\ProgramData\Cairn`.
fn path_is_under(path: &str, root: &Path) -> bool {
    let path = path.replace('/', "\\");
    let path = path.trim_end_matches('\\');
    let root = root.to_string_lossy().replace('/', "\\");
    let root = root.trim_end_matches('\\');
    if path.is_empty() || root.is_empty() || path.len() < root.len() {
        return false;
    }
    let Some(prefix) = path.get(..root.len()) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(root)
        && (path.len() == root.len() || path.as_bytes()[root.len()] == b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_engine::VolumeIndex;
    use cairn_core::engine::CancellationToken;
    use cairn_core::sort::SortColumn;
    use cairn_core::syntax::QueryDefaults;
    use cairn_ntfs::mft::ExtractedItem;
    use cairn_ntfs::usn::{UsnChange, FILE_ATTRIBUTE_REPARSE_POINT};

    /// Builds volume `C` from relative paths (directories first, file last) and returns the file
    /// FRNs in input order. Reparse points are expressed through `flags`, as the scanner does.
    fn volume_with_files(files: &[(&[&str], u16)]) -> (VolumeIndex, Vec<u64>) {
        let mut items = Vec::new();
        let mut directories: HashMap<String, u64> = HashMap::new();
        let mut next_frn = 1u64;
        let mut file_frns = Vec::new();
        for (components, flags) in files {
            let mut parent_frn = 0u64;
            for depth in 1..components.len() {
                let key = components[..depth].join("\\").to_ascii_lowercase();
                if let Some(&frn) = directories.get(&key) {
                    parent_frn = frn;
                    continue;
                }
                let frn = next_frn;
                next_frn += 1;
                items.push(ExtractedItem {
                    frn,
                    parent_frn,
                    name: components[depth - 1].to_string(),
                    is_dir: true,
                    size: 0,
                    created_ts: 1,
                    modified_ts: 1,
                    flags: cairn_core::FLAG_DIRECTORY,
                    is_reparse_point: false,
                });
                directories.insert(key, frn);
                parent_frn = frn;
            }
            let frn = next_frn;
            next_frn += 1;
            items.push(ExtractedItem {
                frn,
                parent_frn,
                name: components.last().expect("file name").to_string(),
                is_dir: false,
                size: 32,
                created_ts: 1,
                modified_ts: 1,
                flags: *flags,
                is_reparse_point: *flags & cairn_core::FLAG_REPARSE_POINT != 0,
            });
            file_frns.push(frn);
        }
        let mut volume = VolumeIndex::new('C');
        volume.ingest_items(items);
        (volume, file_frns)
    }

    fn content_config() -> ServiceConfig {
        ServiceConfig {
            content_index_extensions: ["json", "toml", "log", "txt"]
                .into_iter()
                .map(String::from)
                .collect(),
            content_index_max_bytes: 1024,
            content_excluded_paths: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn service_data_files_are_never_content_tasks() {
        let (volume, frns) = volume_with_files(&[
            (&["ProgramData", "Cairn", "content", "meta.json"], 0),
            (&["PROGRAMDATA", "CAIRN", "CONTENT", "META.JSON"], 0),
            (&["ProgramData", "Cairn", "content", ".managed.json"], 0),
            (&["ProgramData", "Cairn", "service.toml"], 0),
            (&["ProgramData", "Cairn", "logs", "cairn.log"], 0),
            (&["Users", "Public", "notes.json"], 0),
        ]);
        let index = tempfile::tempdir().expect("index");
        let root = std::path::Path::new(r"C:\ProgramData\Cairn");
        let engine = MultiVolumeEngine::with_paths(
            index.path(),
            &root.join("content"),
            &root.join("service.toml"),
            vec![],
            content_config(),
        )
        .with_service_data_root(root);
        let cfg = engine.get_config();
        let index_of = |volume: &VolumeIndex, frn: u64| {
            *volume
                .frn_to_idx
                .get(&frn)
                .and_then(|indices| indices.first())
                .expect("record") as usize
        };

        for (i, frn) in frns.iter().take(5).enumerate() {
            assert!(
                engine
                    .content_task(&volume, index_of(&volume, *frn), &cfg)
                    .is_none(),
                "the service's own data (case {i}) must never reach the content worker"
            );
        }
        let user_task = engine
            .content_task(&volume, index_of(&volume, frns[5]), &cfg)
            .expect("a user file outside the data root stays eligible");
        assert_eq!(user_task.path, r"C:\Users\Public\notes.json");

        engine.add_volume(volume);
        let changes = HashMap::from([('C', frns.iter().copied().collect::<HashSet<_>>())]);
        let (tasks, deletions) = engine.changed_content_tasks(&changes, &cfg);
        assert_eq!(tasks.len(), 1, "only the user file is an incremental task");
        assert_eq!(tasks[0].path, r"C:\Users\Public\notes.json");
        assert!(
            deletions.is_empty(),
            "own writes must not become deletion commits, which would rewrite the index"
        );
    }

    #[test]
    fn a_sibling_of_the_service_data_root_is_not_excluded() {
        let (volume, frns) = volume_with_files(&[(&["ProgramData", "Cairn2", "notes.json"], 0)]);
        let index = tempfile::tempdir().expect("index");
        let root = std::path::Path::new(r"C:\ProgramData\Cairn");
        let engine = MultiVolumeEngine::with_paths(
            index.path(),
            &root.join("content"),
            &root.join("service.toml"),
            vec![],
            content_config(),
        )
        .with_service_data_root(root);
        let index_of = |volume: &VolumeIndex, frn: u64| {
            *volume
                .frn_to_idx
                .get(&frn)
                .and_then(|indices| indices.first())
                .expect("record") as usize
        };

        let task = engine
            .content_task(&volume, index_of(&volume, frns[0]), &engine.get_config())
            .expect("a directory whose name merely starts with Cairn is user content");
        assert_eq!(task.path, r"C:\ProgramData\Cairn2\notes.json");
    }

    #[test]
    fn reparse_point_records_are_never_content_tasks() {
        let (volume, frns) = volume_with_files(&[
            (
                &["Users", "Public", "link.txt"],
                cairn_core::FLAG_REPARSE_POINT,
            ),
            (&["Users", "Public", "real.txt"], 0),
        ]);
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
        let cfg = content_config();
        let index_of = |volume: &VolumeIndex, frn: u64| {
            *volume
                .frn_to_idx
                .get(&frn)
                .and_then(|indices| indices.first())
                .expect("record") as usize
        };

        assert!(
            engine
                .content_task(&volume, index_of(&volume, frns[0]), &cfg)
                .is_none(),
            "a reparse point must not be opened through the worker"
        );
        assert!(
            engine
                .content_task(&volume, index_of(&volume, frns[1]), &cfg)
                .is_some(),
            "a regular file beside the reparse point stays eligible"
        );

        engine.add_volume(volume);
        let changes = HashMap::from([('C', frns.iter().copied().collect::<HashSet<_>>())]);
        let (tasks, _) = engine.changed_content_tasks(&changes, &cfg);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].path, r"C:\Users\Public\real.txt");
    }

    #[test]
    fn a_created_reparse_point_never_becomes_a_content_task() {
        let (mut volume, _) = volume_with_files(&[(&["Users", "Public", "seed.txt"], 0)]);
        let public_frn = volume
            .records
            .iter()
            .find(|record| {
                volume
                    .arena
                    .get_name(record.name_offset, record.name_len)
                    .eq_ignore_ascii_case("Public")
            })
            .expect("Public directory")
            .frn;

        volume.apply_usn_change(UsnChange::Created {
            frn: 900,
            parent_frn: public_frn,
            name: "link.txt".to_string(),
            is_dir: false,
            attributes: FILE_ATTRIBUTE_REPARSE_POINT,
            ts: 5,
        });

        let index = |volume: &VolumeIndex, frn: u64| {
            *volume
                .frn_to_idx
                .get(&frn)
                .and_then(|indices| indices.first())
                .expect("record") as usize
        };
        let record = &volume.records[index(&volume, 900)];
        assert!(record.is_reparse_point());
        assert!(!record.is_dir());

        let index_dir = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir.path(), vec![]);
        engine.add_volume(volume);
        let changes = HashMap::from([('C', HashSet::from([900u64]))]);
        let (tasks, deletions) = engine.changed_content_tasks(&changes, &engine.get_config());
        assert!(
            tasks.is_empty(),
            "a created reparse point must not reach the content worker"
        );
        assert_eq!(deletions, vec![(2, 900)]);
    }

    #[test]
    fn content_exclusions_skip_system_dependency_and_cache_trees() {
        let (volume, frns) = volume_with_files(&[
            (&["Users", "ZYN", "Documents", "keep.txt"], 0),
            (&["Users", "ZYN", "AppData", "Local", "cache.txt"], 0),
            (&["Program Files", "App", "readme.txt"], 0),
            (
                &["Workspace", "proj", "node_modules", "pkg", "index.txt"],
                0,
            ),
            (&["Windows", "Logs", "system.txt"], 0),
            (&["dev", ".cargo", "registry", "lib.txt"], 0),
            (&["dev", "proj", "target", "debug", "out.txt"], 0),
        ]);
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
        let cfg = backfill_config();
        let record_index = |frn: u64| {
            *volume
                .frn_to_idx
                .get(&frn)
                .and_then(|indices| indices.first())
                .expect("record") as usize
        };

        let keep = engine
            .content_task(&volume, record_index(frns[0]), &cfg)
            .expect("a user document must stay eligible");
        assert!(keep.path.contains("Documents"));

        for (position, frn) in frns.iter().enumerate().skip(1) {
            assert!(
                engine
                    .content_task(&volume, record_index(*frn), &cfg)
                    .is_none(),
                "content task {position} is under an excluded tree"
            );
        }
    }

    #[test]
    fn a_basic_info_change_to_a_reparse_point_queues_a_deletion() {
        let (mut volume, frns) = volume_with_files(&[(&["Users", "Public", "notes.txt"], 0)]);
        let frn = frns[0];

        volume.apply_usn_change(UsnChange::BasicInfoChanged {
            frn,
            ts: 2,
            attributes: FILE_ATTRIBUTE_REPARSE_POINT,
        });

        let record = volume
            .records
            .iter()
            .find(|record| record.frn == frn)
            .expect("record");
        assert!(
            record.is_reparse_point(),
            "the attributes must reach the record"
        );
        assert!(
            !record.is_dir(),
            "a file attribute set without DIRECTORY must not make it a directory"
        );
        assert!(
            volume.pending_data_changes.contains(&frn),
            "the content pipeline must re-evaluate the changed record"
        );

        let index_dir = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir.path(), vec![]);
        engine.add_volume(volume);
        let changes = HashMap::from([('C', HashSet::from([frn]))]);
        let (tasks, deletions) = engine.changed_content_tasks(&changes, &engine.get_config());
        assert!(
            tasks.is_empty(),
            "a record that became a reparse point must not be re-extracted"
        );
        assert_eq!(deletions, vec![(2, frn)]);
    }

    /// Startup replays the change journal into the pending sets and only then compacts the cached
    /// table. Compaction must not swallow that replay: a create/modify would keep its stale
    /// metadata and missing document, and a deletion would leave its body document behind.
    #[test]
    fn compaction_keeps_pending_work_replayed_before_startup() {
        let (mut volume, frns) = volume_with_files(&[
            (&["Users", "Public", "keep.txt"], 0),
            (&["Users", "Public", "gone.txt"], 0),
            (&["ProgramData", "Cairn", "content", "meta.json"], 0),
        ]);
        let keep = frns[0];
        let gone = frns[1];

        volume.apply_usn_change(UsnChange::DataChanged { frn: keep });
        volume.apply_usn_change(UsnChange::Renamed {
            frn: keep,
            parent_frn: 0,
            name: "old-name.txt".to_string(),
            is_dir: false,
            is_new_name: false,
        });
        volume.apply_usn_change(UsnChange::Deleted { frn: gone });

        // A cache written before the service data root was excluded still holds its files.
        let cairn = volume
            .records
            .iter()
            .find(|record| volume.record_name(record) == "Cairn")
            .expect("Cairn directory")
            .frn;
        volume.set_excluded_roots([cairn]);

        assert!(
            volume.compact() > 0,
            "the tombstone and the service-data subtree must be dropped"
        );
        assert!(
            volume.records.iter().all(|record| record.frn != gone),
            "a compacted tombstone must not stay in the table"
        );
        assert!(volume.pending_data_changes.contains(&keep));
        assert!(volume.pending_renames.contains_key(&keep));
        assert!(volume.pending_content_deletions.contains(&(2, gone)));
        assert!(
            volume.excluded_frns.contains(&cairn),
            "the exclusion itself is configuration and must survive"
        );

        // Both downstream consumers still find their work through the existing paths.
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
        let cfg = content_config();
        engine.add_volume(volume);

        let changes = engine.drain_pending_data_changes();
        let (tasks, ineligible) = engine.changed_content_tasks(&changes, &cfg);
        assert_eq!(
            tasks.len(),
            1,
            "the change replayed before compaction still applies"
        );
        assert_eq!(tasks[0].frn, keep);

        let mut pending = engine.drain_pending_content_deletions();
        pending.extend(ineligible);
        let deletions = engine.resolve_content_deletions(pending, &cfg);
        assert_eq!(
            deletions,
            vec![(2, gone)],
            "the deletion replayed before compaction must still remove its document"
        );
    }

    /// A USN `Created` record can arrive without any attribute bits. The debounce pass reads the
    /// entry itself, so the record still learns that the path is a reparse point.
    #[cfg(windows)]
    #[test]
    fn the_debounce_refresh_marks_a_created_symlink_as_reparse() {
        use std::os::windows::fs::symlink_file;
        use std::path::Component;

        let tree = tempfile::tempdir().expect("tree");
        let target = tree.path().join("target.txt");
        std::fs::write(&target, "payload").expect("target file");
        let link = tree.path().join("link.txt");
        if let Err(err) = symlink_file(&target, &link) {
            eprintln!("skipping symlink refresh test: {err}");
            return;
        }

        let mut items = Vec::new();
        let mut parent_frn = 0u64;
        let mut next_frn = 1u64;
        for component in tree.path().components() {
            let Component::Normal(name) = component else {
                continue;
            };
            items.push(ExtractedItem {
                frn: next_frn,
                parent_frn,
                name: name.to_string_lossy().into_owned(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            });
            parent_frn = next_frn;
            next_frn += 1;
        }

        let mut volume = VolumeIndex::new('C');
        volume.ingest_items(items);

        let index_dir = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir.path(), vec![]);
        engine.add_volume(volume);
        engine.update_volume('C', |volume| {
            volume.apply_usn_change(UsnChange::Created {
                frn: next_frn,
                parent_frn,
                name: "link.txt".to_string(),
                is_dir: false,
                attributes: 0,
                ts: 1,
            });
            let index = *volume
                .frn_to_idx
                .get(&next_frn)
                .and_then(|indices| indices.first())
                .expect("created record");
            volume.records[index as usize].flags |= cairn_core::FLAG_HIDDEN;
        });

        engine.process_pending_data_changes();

        let record = engine
            .with_volumes(|volumes| {
                volumes
                    .get(&'C')
                    .expect("volume C")
                    .records
                    .iter()
                    .find(|record| record.frn == next_frn)
                    .copied()
            })
            .expect("created record");
        assert!(
            record.is_reparse_point(),
            "metadata refresh must add the reparse flag a missing USN attribute hid"
        );
        assert!(!record.is_dir());
        assert!(
            record.is_hidden(),
            "refresh must keep flags it does not own"
        );
    }

    #[test]
    fn test_pending_data_changes_tracking_and_drain() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 42,
            parent_frn: 5,
            name: "data.txt".to_string(),
            is_dir: false,
            size: 512,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);

        // A DataChanged event is debounced until the next drain.
        vol.apply_usn_change(UsnChange::DataChanged { frn: 42 });
        assert!(vol.pending_data_changes.contains(&42));

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        engine.add_volume(vol);

        let drained = engine.drain_pending_data_changes();
        assert_eq!(drained.get(&'C').map(|s| s.contains(&42)), Some(true));

        // After drain, pending set is empty.
        let drained_again = engine.drain_pending_data_changes();
        assert!(drained_again.is_empty());
    }

    #[test]
    fn excluded_paths_are_not_sent_to_the_content_worker() {
        use std::path::Component;

        let tree = tempfile::tempdir().expect("tree");
        std::fs::create_dir_all(tree.path().join("skip")).expect("skip dir");
        std::fs::write(tree.path().join("keep.txt"), "keep").expect("keep file");
        std::fs::write(tree.path().join("skip").join("secret.txt"), "secret").expect("secret");

        let mut items = Vec::new();
        let mut parent_frn = 0u64;
        let mut next_frn = 1u64;
        for component in tree.path().components() {
            let Component::Normal(name) = component else {
                continue;
            };
            items.push(ExtractedItem {
                frn: next_frn,
                parent_frn,
                name: name.to_string_lossy().into_owned(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            });
            parent_frn = next_frn;
            next_frn += 1;
        }

        let keep_frn = next_frn;
        items.push(ExtractedItem {
            frn: keep_frn,
            parent_frn,
            name: "keep.txt".to_string(),
            is_dir: false,
            size: 4,
            created_ts: 1,
            modified_ts: 1,
            flags: 0,
            is_reparse_point: false,
        });
        next_frn += 1;
        let skip_dir_frn = next_frn;
        items.push(ExtractedItem {
            frn: skip_dir_frn,
            parent_frn,
            name: "skip".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 1,
            modified_ts: 1,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        });
        next_frn += 1;
        let secret_frn = next_frn;
        items.push(ExtractedItem {
            frn: secret_frn,
            parent_frn: skip_dir_frn,
            name: "secret.txt".to_string(),
            is_dir: false,
            size: 6,
            created_ts: 1,
            modified_ts: 1,
            flags: 0,
            is_reparse_point: false,
        });

        let mut volume = VolumeIndex::new('C');
        volume.ingest_items(items);
        let cfg = ServiceConfig {
            content_index_extensions: vec!["txt".to_string()],
            content_index_max_bytes: 1024,
            excluded_paths: vec![tree.path().join("skip").to_string_lossy().into_owned()],
            content_excluded_paths: Vec::new(),
            ..Default::default()
        };
        let index_of = |frn: u64| {
            *volume
                .frn_to_idx
                .get(&frn)
                .and_then(|indices| indices.first())
                .expect("record") as usize
        };
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);

        assert!(
            engine
                .content_task(&volume, index_of(keep_frn), &cfg)
                .is_some(),
            "an eligible file outside the rules must be indexed"
        );
        assert!(
            engine
                .content_task(&volume, index_of(secret_frn), &cfg)
                .is_none(),
            "a file under an excluded path must never reach the content worker"
        );
    }

    /// A create event only says a name appeared. The row must still report the file's real size and
    /// timestamps, and that has to happen with content indexing off, because the table shows it
    /// either way.
    #[test]
    fn a_created_event_reads_the_real_size_and_time_from_disk() {
        use std::path::Component;

        let tree = tempfile::tempdir().expect("tree");
        let file = tree.path().join("created.bin");
        std::fs::write(&file, vec![7u8; 1234]).expect("fixture file");

        let mut items = Vec::new();
        let mut parent_frn = 0u64;
        let mut next_frn = 1u64;
        for component in tree.path().components() {
            let Component::Normal(name) = component else {
                continue;
            };
            items.push(ExtractedItem {
                frn: next_frn,
                parent_frn,
                name: name.to_string_lossy().into_owned(),
                is_dir: true,
                size: 0,
                created_ts: 0,
                modified_ts: 0,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            });
            parent_frn = next_frn;
            next_frn += 1;
        }

        let mut volume = VolumeIndex::new('C');
        volume.ingest_items(items);

        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
        engine.add_volume(volume);
        engine.update_volume('C', |volume| {
            volume.apply_usn_change(UsnChange::Created {
                frn: next_frn,
                parent_frn,
                name: "created.bin".to_string(),
                is_dir: false,
                attributes: 0,
                ts: 1,
            })
        });

        let created_size = |engine: &MultiVolumeEngine| {
            engine.with_volumes(|volumes| {
                volumes
                    .get(&'C')
                    .expect("volume C")
                    .records
                    .iter()
                    .find(|record| record.frn == next_frn)
                    .expect("created record")
                    .size
            })
        };
        assert_eq!(
            created_size(&engine),
            0,
            "the journal record carries no size, so the row starts at zero"
        );

        engine.process_pending_data_changes();

        let (size, created_ts, modified_ts, parent_size) = engine.with_volumes(|volumes| {
            let records = &volumes.get(&'C').expect("volume C").records;
            let record = records
                .iter()
                .find(|record| record.frn == next_frn)
                .expect("created record");
            let parent_size = records
                .iter()
                .find(|record| record.frn == parent_frn)
                .expect("parent record")
                .size;
            (
                record.size,
                record.created_ts,
                record.modified_ts,
                parent_size,
            )
        });
        assert_eq!(size, 1234);
        assert_eq!(
            parent_size, 1234,
            "a discovered file size must reach its directory aggregate"
        );
        assert!(created_ts > 0 && modified_ts > 0, "timestamps are FILETIME");
    }

    #[test]
    fn test_content_query_intersection_with_mft() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        // 1. Ingest MFT files on drive C:
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            ExtractedItem {
                frn: 101,
                parent_frn: 5,
                name: "architecture.md".to_string(),
                is_dir: false,
                size: 2048,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 102,
                parent_frn: 5,
                name: "guide.txt".to_string(),
                is_dir: false,
                size: 1024,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
        ]);
        engine.add_volume(vol);

        // 2. Index documents with a writer (the worker's role), then publish a read-only searcher.
        let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
        writer
            .index_document(
                2,
                101,
                2048,
                100,
                r"C:\docs\architecture.md",
                "Cairn 架构设计与并发求交链路实现",
            )
            .expect("index doc 101");
        writer
            .index_document(
                2,
                102,
                1024,
                100,
                r"C:\docs\guide.txt",
                "使用指南与操作说明",
            )
            .expect("index doc 102");
        writer.finish().expect("finish content index");
        assert!(engine.get_or_create_content_searcher().is_some());
        engine.mark_content_index_ready_for_test();

        let cancel = CancellationToken::new();

        // Query 1: Pure content query "content:架构" -> should match architecture.md (frn 101 on C:)
        let (results, total, _, snippets, _) = engine.execute_search_multi(
            "content:架构",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].drive_letter, 'C');
        assert_eq!(results[0].record_idx, 0); // 101
        assert!(snippets.contains_key(&('C', 101)));

        // Multi-volume isolation: Add volume D with an unrelated file having identical FRN 101
        let mut vol_d = VolumeIndex::new('D');
        vol_d.ingest_items(vec![ExtractedItem {
            frn: 101, // Identical FRN on Drive D
            parent_frn: 5,
            name: "d_unrelated.txt".to_string(),
            is_dir: false,
            size: 500,
            modified_ts: 100,
            created_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);
        engine.add_volume(vol_d);

        // Content search should STILL only return C:\docs\architecture.md and NOT D:\d_unrelated.txt
        let (results_mv, total_mv, ..) = engine.execute_search_multi(
            "content:架构",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total_mv, 1);
        assert_eq!(results_mv[0].drive_letter, 'C');

        // Query 2: Combined MFT filter and content "ext:md content:架构" -> matches 101
        let (results_combined, total_c, ..) = engine.execute_search_multi(
            "ext:md content:架构",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total_c, 1);
        assert_eq!(results_combined.len(), 1);

        // Query 3: Combined MFT filter mismatch "ext:txt content:架构" -> 0 matches
        let (results_mismatch, total_m, ..) = engine.execute_search_multi(
            "ext:txt content:架构",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total_m, 0);
        assert!(results_mismatch.is_empty());

        // Query 4: Content query for non-existent word -> 0 matches
        let (results_none, total_n, ..) = engine.execute_search_multi(
            "content:不存在的词",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total_n, 0);
        assert!(results_none.is_empty());
    }

    #[test]
    fn test_clear_content_index_roundtrip() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 101,
            parent_frn: 5,
            name: "test.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);
        engine.add_volume(vol);

        let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
        writer
            .index_document(2, 101, 100, 100, r"C:\test.txt", "全文搜索关键词")
            .unwrap();
        writer.finish().expect("finish content index");
        assert!(engine.get_or_create_content_searcher().is_some());
        engine.mark_content_index_ready_for_test();

        let cancel = CancellationToken::new();
        let (_, total, ..) = engine.execute_search_multi(
            "content:关键词",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1);

        // Clear content index (CNT-07)
        engine.clear_content_index().unwrap();

        let (_, total_after, ..) = engine.execute_search_multi(
            "content:关键词",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total_after, 0);
    }

    #[test]
    fn the_service_never_creates_a_content_index() {
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);

        assert!(engine.get_or_create_content_searcher().is_none());
        assert!(
            !engine.content_dir.exists(),
            "the query path must not create an index, a directory or a writer"
        );
    }

    #[test]
    fn stale_batches_and_old_temps_are_removed_without_touching_index_files() {
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
        let dir = engine.content_dir.clone();
        std::fs::create_dir_all(&dir).expect("content dir");

        std::fs::write(dir.join("batch-1.jsonl"), "{}\n").expect("batch file");
        std::fs::write(dir.join("deletes-1.jsonl"), "[]\n").expect("delete batch file");
        std::fs::write(dir.join(".tmp-old"), "stale").expect("old temp file");
        let old = std::fs::File::options()
            .write(true)
            .open(dir.join(".tmp-old"))
            .expect("old temp file");
        old.set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .expect("set old mtime");
        drop(old);
        std::fs::write(dir.join(".tmp-fresh"), "fresh").expect("fresh temp file");
        let index_files = [
            "meta.json",
            ".managed.json",
            "seg.idx",
            ".tantivy-writer.lock",
        ];
        for name in index_files {
            std::fs::write(dir.join(name), "keep").expect("index file");
        }

        engine.cleanup_stale_content_files();

        assert!(!dir.join("batch-1.jsonl").exists());
        assert!(!dir.join("deletes-1.jsonl").exists());
        assert!(!dir.join(".tmp-old").exists());
        assert!(
            dir.join(".tmp-fresh").exists(),
            "a recently written temp file may still be in use"
        );
        for name in index_files {
            assert!(
                dir.join(name).exists(),
                "cleanup must not delete index file {name}"
            );
        }
    }

    fn backfill_config() -> ServiceConfig {
        ServiceConfig {
            content_index_enabled: true,
            content_index_extensions: vec!["txt".to_string()],
            content_index_max_bytes: 1024,
            ..Default::default()
        }
    }

    fn backfill_marker_for(config: &ServiceConfig) -> BackfillMarker {
        BackfillMarker {
            schema: CONTENT_BACKFILL_SCHEMA,
            extensions: config.content_index_extensions.clone(),
            max_bytes: config.content_index_max_bytes,
            excluded_paths: config.excluded_paths.clone(),
            content_excluded_paths: config.content_excluded_paths.clone(),
            documents: 1,
            completed_ts: unix_now(),
        }
    }

    /// Builds a real on-disk index with one searchable document and records the completion marker
    /// through the production writer, exactly as a finished backfill does.
    fn engine_with_completed_backfill(
        index_dir: &std::path::Path,
        config: &ServiceConfig,
    ) -> MultiVolumeEngine {
        let engine =
            MultiVolumeEngine::with_index_dir_drives_and_config(index_dir, vec![], config.clone());
        let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
        writer
            .index_document(2, 101, 7, 1, r"C:\docs\note.txt", "resumable needle")
            .expect("index document");
        writer.finish().expect("finish content index");
        assert!(engine.get_or_create_content_searcher().is_some());
        engine.mark_content_index_ready_for_test();
        engine.write_backfill_marker();
        assert!(engine.backfill_marker_path().exists(), "marker was written");
        engine
    }

    #[test]
    fn only_file_handle_conflicts_count_as_transient_worker_failures() {
        for error in [
            "CairnWorker exited with exit code: 1: PermissionDenied (os error 5)",
            "Access is denied. (os error 5)",
            "拒绝访问。 (os error 5)",
            "The process cannot access the file because it is being used by another process. \
             (os error 32): sharing violation",
        ] {
            assert!(
                worker_error_is_transient(error),
                "{error:?} must be retried once"
            );
        }
        for error in [
            "CairnWorker timed out",
            "invalid content batch payload",
            "os error 2: the system cannot find the file specified",
            "CairnWorker exited with 0xc0000409",
        ] {
            assert!(
                !worker_error_is_transient(error),
                "{error:?} is not a sharing conflict"
            );
        }
    }

    /// A single bad file (including a worker crash) must not turn the whole content index into
    /// Error. The old searcher keeps answering from its last committed snapshot, and removing the
    /// resume marker makes the next start rebuild the gaps that this batch could not apply.
    #[test]
    fn a_failed_incremental_batch_keeps_the_ready_snapshot() {
        let index = tempfile::tempdir().expect("index");
        let config = backfill_config();
        let engine = engine_with_completed_backfill(index.path(), &config);
        let before = engine.content_index_status();
        assert_eq!(before.state, ContentIndexState::Ready);
        assert!(engine.backfill_marker_path().exists());
        let searcher = engine
            .content_searcher()
            .expect("a completed backfill publishes a searcher");

        let tasks = vec![ContentIndexTask {
            volume_id: 2,
            frn: 101,
            size: 8,
            modified_ts: 1,
            path: r"C:\docs\new.txt".to_string(),
        }];
        engine.run_incremental_with(tasks, Vec::new(), |_, _, _| {
            // A crashed worker (WER 0xc0000409) is a permanent failure for this batch, but still
            // not a reason to disable content search for every other file.
            BatchOutcome::Failed("CairnWorker exited with 0xc0000409".to_string())
        });

        let after = engine.content_index_status();
        assert_eq!(after.state, ContentIndexState::Ready);
        assert_eq!(after.indexed_documents, before.indexed_documents);
        let retained = engine
            .content_searcher()
            .expect("the last committed snapshot must keep serving queries");
        assert!(
            Arc::ptr_eq(&searcher, &retained),
            "the failure must not replace the published searcher"
        );
        assert!(
            !engine.backfill_marker_path().exists(),
            "the next start must rebuild instead of resuming an incomplete index"
        );
    }

    #[test]
    fn a_completed_backfill_is_reused_on_restart_without_a_writer() {
        let index = tempfile::tempdir().expect("index");
        let config = backfill_config();
        let engine = engine_with_completed_backfill(index.path(), &config);
        // A writer creates this lock file. The resume path must not recreate it.
        let writer_lock = engine.content_dir.join(".tantivy-writer.lock");
        let _ = std::fs::remove_file(&writer_lock);
        drop(engine);

        let engine =
            MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config);
        assert!(
            engine.try_resume_content_index(),
            "a completed backfill must be reusable after a restart"
        );
        let status = engine.content_index_status();
        assert_eq!(status.state, ContentIndexState::Ready);
        assert_eq!(status.indexed_documents, 1);
        let searcher = engine
            .get_or_create_content_searcher()
            .expect("the resumed index must answer queries");
        let hits = searcher.search("needle", 10).expect("query");
        assert_eq!(hits.len(), 1, "the indexed document must stay searchable");
        assert!(
            !writer_lock.exists(),
            "resuming the index must never open a writer"
        );
    }

    #[test]
    fn a_marker_that_disagrees_with_the_configuration_is_discarded() {
        let index = tempfile::tempdir().expect("index");
        let config = backfill_config();
        let prepared = engine_with_completed_backfill(index.path(), &config);
        let valid = backfill_marker_for(&config);
        drop(prepared);

        let engine =
            MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config);
        let write_marker = |marker: &BackfillMarker| {
            std::fs::write(
                engine.backfill_marker_path(),
                serde_json::to_vec(marker).expect("encode marker"),
            )
            .expect("write marker");
        };

        let mut wrong_schema = valid.clone();
        wrong_schema.schema += 1;
        write_marker(&wrong_schema);
        assert!(
            !engine.try_resume_content_index(),
            "an unknown marker schema must force a rebuild"
        );
        assert!(
            !engine.backfill_marker_path().exists(),
            "a rejected marker must be deleted"
        );

        let mut wrong_extensions = valid.clone();
        wrong_extensions.extensions = vec!["md".to_string()];
        write_marker(&wrong_extensions);
        assert!(
            !engine.try_resume_content_index(),
            "changed extensions must force a rebuild"
        );
        assert!(!engine.backfill_marker_path().exists());

        let mut wrong_max_bytes = valid.clone();
        wrong_max_bytes.max_bytes += 1;
        write_marker(&wrong_max_bytes);
        assert!(
            !engine.try_resume_content_index(),
            "a changed size limit must force a rebuild"
        );
        assert!(!engine.backfill_marker_path().exists());

        let mut wrong_exclusions = valid.clone();
        wrong_exclusions
            .excluded_paths
            .push(r"C:\extra".to_string());
        write_marker(&wrong_exclusions);
        assert!(
            !engine.try_resume_content_index(),
            "changed exclusions must force a rebuild"
        );
        assert!(!engine.backfill_marker_path().exists());
    }

    #[test]
    fn a_marker_without_a_usable_index_is_discarded() {
        let config = backfill_config();

        // The marker exists but no index was ever written.
        let missing = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_drives_and_config(
            missing.path(),
            vec![],
            config.clone(),
        );
        std::fs::create_dir_all(&engine.content_dir).expect("content dir");
        std::fs::write(
            engine.backfill_marker_path(),
            serde_json::to_vec(&backfill_marker_for(&config)).expect("encode marker"),
        )
        .expect("write marker");
        assert!(
            !engine.try_resume_content_index(),
            "a marker without an index must not produce Ready"
        );
        assert!(
            !engine.backfill_marker_path().exists(),
            "the unusable marker must be deleted"
        );
        assert!(!engine.content_index_status().state.is_ready());

        // The index opens but holds no documents.
        let empty = tempfile::tempdir().expect("index");
        let engine =
            MultiVolumeEngine::with_index_dir_drives_and_config(empty.path(), vec![], config);
        let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
        writer.finish().expect("finish empty index");
        assert!(engine.get_or_create_content_searcher().is_some());
        engine.mark_content_index_ready_for_test();
        engine.write_backfill_marker();
        assert!(engine.backfill_marker_path().exists());
        assert!(
            !engine.try_resume_content_index(),
            "an empty index is not a completed backfill"
        );
        assert!(!engine.backfill_marker_path().exists());
    }

    /// A USN burst writes through the worker while the service keeps answering `content:` queries.
    /// The published searcher must not be dropped when the incremental batch starts, and the
    /// end-of-batch reopen must publish the new count without a Backfilling gap.
    #[test]
    fn an_incremental_commit_keeps_the_index_searchable_until_reopen() {
        let index = tempfile::tempdir().expect("index");
        let engine = MultiVolumeEngine::with_index_dir_drives_and_config(
            index.path(),
            vec![],
            backfill_config(),
        );

        // Finished backfill: one committed document and a published read-only searcher.
        let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
        writer
            .index_document(2, 101, 7, 1, r"C:\docs\first.txt", "first needle")
            .expect("index first document");
        writer.finish().expect("finish content index");
        assert!(engine.get_or_create_content_searcher().is_some());
        engine.mark_content_index_ready_for_test();
        assert!(engine.content_index_status().state.is_ready());

        // The worker commits its batch while the service still holds the existing reader.
        let worker =
            ContentIndex::open_or_create(&engine.content_dir).expect("incremental content index");
        worker
            .index_document(2, 102, 7, 1, r"C:\docs\second.txt", "second needle")
            .expect("index second document");
        worker.finish().expect("finish incremental batch");

        assert!(
            engine.content_index_status().state.is_ready(),
            "an incremental commit must not take the published index out of Ready"
        );
        let searcher = engine
            .get_or_create_content_searcher()
            .expect("the published searcher must survive the batch");
        assert_eq!(
            searcher.search("needle", 10).expect("search").len(),
            2,
            "the reader must keep serving after the worker commit"
        );

        // Reopening after the batch publishes the new document count and stays Ready.
        engine.reopen_content_index_ready();
        let status = engine.content_index_status();
        assert_eq!(status.state, ContentIndexState::Ready);
        assert_eq!(status.indexed_documents, 2);
        let searcher = engine
            .get_or_create_content_searcher()
            .expect("reopened searcher");
        assert_eq!(searcher.search("needle", 10).expect("search").len(), 2);
    }

    #[test]
    fn cleanup_keeps_the_backfill_marker_and_remove_deletes_it() {
        let index = tempfile::tempdir().expect("index");
        let config = backfill_config();
        let engine = MultiVolumeEngine::with_index_dir_drives_and_config(
            index.path(),
            vec![],
            config.clone(),
        );
        std::fs::create_dir_all(&engine.content_dir).expect("content dir");
        std::fs::write(
            engine.backfill_marker_path(),
            serde_json::to_vec(&backfill_marker_for(&config)).expect("encode marker"),
        )
        .expect("write marker");
        std::fs::write(engine.content_dir.join(CONTENT_BACKFILL_MARKER_TMP), "{}")
            .expect("write tmp marker");

        // Age both files beyond the stale threshold: the completion marker must survive any age,
        // while a leftover half-written replacement is cleaned up.
        for name in [CONTENT_BACKFILL_MARKER, CONTENT_BACKFILL_MARKER_TMP] {
            let file = std::fs::File::options()
                .write(true)
                .open(engine.content_dir.join(name))
                .expect("marker file");
            file.set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
                .expect("set old mtime");
        }
        engine.cleanup_stale_content_files();
        assert!(
            engine.backfill_marker_path().exists(),
            "cleanup must keep the completed-backfill marker"
        );
        assert!(
            !engine
                .content_dir
                .join(CONTENT_BACKFILL_MARKER_TMP)
                .exists(),
            "a stale half-written marker is removed"
        );

        engine.remove_backfill_marker();
        assert!(!engine.backfill_marker_path().exists());
    }

    /// End-to-end backfill: enables the switch, waits for the controller, and verifies that a file
    /// which existed before enabling becomes searchable. Ignored by default because it needs the
    /// real `CairnWorker` binary beside the test executable.
    #[test]
    #[ignore = "requires target/debug/CairnWorker.exe"]
    fn a_full_backfill_reaches_ready_and_indexes_existing_files() {
        use std::path::Component;

        let tree = tempfile::tempdir().expect("tree");
        let file = tree.path().join("hello.txt");
        std::fs::write(&file, "hello full backfill").expect("fixture file");

        let mut items = Vec::new();
        let mut parent_frn = 0u64;
        let mut next_frn = 1u64;
        for component in tree.path().components() {
            let Component::Normal(name) = component else {
                continue;
            };
            items.push(ExtractedItem {
                frn: next_frn,
                parent_frn,
                name: name.to_string_lossy().into_owned(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            });
            parent_frn = next_frn;
            next_frn += 1;
        }
        let hello_frn = next_frn;
        items.push(ExtractedItem {
            frn: hello_frn,
            parent_frn,
            name: "hello.txt".to_string(),
            is_dir: false,
            size: 19,
            created_ts: 1,
            modified_ts: 1,
            flags: 0,
            is_reparse_point: false,
        });

        let mut volume = VolumeIndex::new('C');
        volume.ingest_items(items);

        let index = tempfile::tempdir().expect("index");
        let config = ServiceConfig {
            content_index_enabled: true,
            content_index_extensions: vec!["txt".to_string()],
            content_index_max_bytes: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(MultiVolumeEngine::with_index_dir_drives_and_config(
            index.path(),
            vec![],
            config,
        ));
        engine.add_volume(volume);
        engine.ensure_content_runtime();

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = engine.content_index_status();
            match &status.state {
                ContentIndexState::Ready => {
                    assert!(
                        status.indexed_documents >= 1,
                        "ready index must contain the existing file: {status:?}"
                    );
                    break;
                }
                ContentIndexState::Error(err) => panic!("backfill failed: {err}"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "backfill did not finish: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        let cancel = CancellationToken::new();
        let (_, total, ..) = engine.execute_search_multi(
            "content:hello",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1, "the pre-existing file must be searchable");

        // A file created after the ready state is queued through the USN path and sent to a fresh
        // worker batch; it must become searchable without a second manual backfill.
        let later = tree.path().join("later.txt");
        std::fs::write(&later, "later needle content").expect("later fixture");
        let later_frn = next_frn + 1;
        engine.update_volume('C', |volume| {
            volume.append_items(vec![ExtractedItem {
                frn: later_frn,
                parent_frn,
                name: "later.txt".to_string(),
                is_dir: false,
                size: 19,
                created_ts: 1,
                modified_ts: 1,
                flags: 0,
                is_reparse_point: false,
            }]);
            volume.apply_usn_change(UsnChange::DataChanged { frn: later_frn });
        });
        engine.process_pending_data_changes();

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = engine.content_index_status();
            if status.state.is_ready() && status.indexed_documents >= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "incremental indexing did not finish: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let (_, total, ..) = engine.execute_search_multi(
            "content:needle",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1, "the new file must be searchable");

        // A deleted file must lose its body document; keeping it would return stale content and
        // could expose a previous file after NTFS reuses the FRN.
        std::fs::remove_file(&later).expect("remove later fixture");
        engine.update_volume('C', |volume| {
            volume.apply_usn_change(UsnChange::Deleted { frn: later_frn });
        });
        engine.process_pending_data_changes();

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = engine.content_index_status();
            if status.state.is_ready() && status.indexed_documents == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "content deletion did not finish: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let (_, total, ..) = engine.execute_search_multi(
            "content:needle",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0, "a deleted file must not stay searchable");

        // Renaming out of the whitelist is a deletion, not a stale document under the old name.
        engine.update_volume('C', |volume| {
            volume.apply_usn_change(UsnChange::Renamed {
                frn: hello_frn,
                parent_frn,
                name: "hello.bin".to_string(),
                is_dir: false,
                is_new_name: true,
            });
        });
        engine.process_pending_data_changes();

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = engine.content_index_status();
            if status.state.is_ready() && status.indexed_documents == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "whitelist rename deletion did not finish: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let (_, total, ..) = engine.execute_search_multi(
            "content:hello",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0, "a renamed-out file must not stay searchable");
    }
}

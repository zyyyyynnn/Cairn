// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Content task construction, eligibility rules and backfill/incremental scheduling.

use super::{BatchOutcome, ContentCommand};
use cairn_core::{FLAG_DIRECTORY, FLAG_REPARSE_POINT};
use cairn_protocol::status::{ContentIndexState, ContentIndexTask};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::config::ServiceConfig;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};

/// Files handed to one worker process. A worker restart is cheap; a batch small enough to finish
/// within the timeout keeps a single malformed file from stalling the whole backfill.
const CONTENT_BATCH_SIZE: usize = 500;
/// Records inspected per lock acquisition while collecting a batch.
const CONTENT_SCAN_CHUNK: usize = 2_000;

impl MultiVolumeEngine {
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

    fn content_eligible(volume: &VolumeIndex, index: usize, cfg: &ServiceConfig) -> bool {
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

    fn record_path(volume: &VolumeIndex, index: usize) -> String {
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

    pub(super) fn content_task(
        &self,
        volume: &VolumeIndex,
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
    pub(super) fn changed_content_tasks(
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

    pub(super) fn drain_pending_content_deletions(&self) -> HashSet<(u64, u64)> {
        let mut lock = self.volumes.write().unwrap();
        lock.values_mut()
            .flat_map(|volume| volume.flush_pending_content_deletions())
            .collect()
    }

    /// A deleted FRN can be reused before the debounce fires. Keep the deletion only when no live
    /// eligible record now owns that FRN; otherwise the replacement task is about to overwrite it.
    pub(super) fn resolve_content_deletions(
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
        self.send_content_command(ContentCommand::IndexChanges { tasks, deletions });
    }
}

impl MultiVolumeEngine {
    pub(super) fn run_full_backfill(&self) {
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
                    BatchOutcome::Failed(failure) => {
                        self.set_content_error(failure.message);
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
                BatchOutcome::Failed(failure) => {
                    self.set_content_error(failure.message);
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
        // the state, so a reopen failure leaves no marker behind. The marker records `cfg`, the
        // snapshot this run was built from, not a configuration that changed mid-run.
        self.write_backfill_marker(&cfg);
    }

    pub(super) fn run_incremental(&self, tasks: Vec<ContentIndexTask>, deletions: Vec<(u64, u64)>) {
        self.run_incremental_with(tasks, deletions, |tasks, deletions, clear| {
            self.run_worker_batch(tasks, deletions, clear)
        });
    }

    /// The batch runner is a parameter so a test can exercise the failure policy without spawning
    /// `CairnWorker`; production always passes [`Self::run_worker_batch`].
    pub(super) fn run_incremental_with<F>(
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
                BatchOutcome::Failed(failure) => {
                    self.keep_last_content_snapshot(failure.message);
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
                    BatchOutcome::Failed(failure) => {
                        self.keep_last_content_snapshot(failure.message);
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

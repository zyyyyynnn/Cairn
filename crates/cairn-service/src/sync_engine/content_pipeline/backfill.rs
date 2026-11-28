// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::{BatchOutcome, ContentCommand};
use cairn_protocol::status::{ContentIndexState, ContentIndexTask};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::ServiceConfig;
use crate::exclusion::{ExclusionPath, ExclusionRules};
use crate::sync_engine::volume_index::RefreshedRecordMetadata;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};

const CONTENT_BATCH_SIZE: usize = 500;
const CONTENT_SCAN_CHUNK: usize = 2_000;

pub(super) struct ContentRules<'a> {
    cfg: &'a ServiceConfig,
    excluded: ExclusionRules,
    content_excluded: ExclusionRules,
}

impl<'a> ContentRules<'a> {
    pub(super) fn new(cfg: &'a ServiceConfig) -> Self {
        Self {
            cfg,
            excluded: ExclusionRules::parse(&cfg.excluded_paths),
            content_excluded: ExclusionRules::parse(&cfg.content_excluded_paths),
        }
    }

    fn excludes(&self, path: &str) -> bool {
        if self.excluded.is_empty() && self.content_excluded.is_empty() {
            return false;
        }
        let path = ExclusionPath::new(path);
        self.excluded.excludes(&path) || self.content_excluded.excludes(&path)
    }
}

impl MultiVolumeEngine {
    fn count_eligible(&self, rules: &ContentRules<'_>) -> u64 {
        let volumes = self.volumes.read().unwrap();
        volumes
            .values()
            .map(|volume| {
                (0..volume.records().len())
                    .filter(|&index| self.content_task(volume, index, rules).is_some())
                    .count() as u64
            })
            .sum()
    }

    fn content_eligible(volume: &VolumeIndex, index: usize, cfg: &ServiceConfig) -> bool {
        let Some(rec) = volume.records().get(index) else {
            return false;
        };
        if rec.is_tombstone()
            || rec.is_dir()
            || rec.is_reparse_point()
            || rec.size > cfg.content_index_max_bytes
        {
            return false;
        }
        let ext = volume
            .arena()
            .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        cfg.content_index_extensions
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&ext))
    }

    fn record_path(volume: &VolumeIndex, index: usize) -> String {
        volume.path_cache().get_full_path(
            volume.records(),
            volume.arena(),
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
        rules: &ContentRules<'_>,
    ) -> Option<ContentIndexTask> {
        if !Self::content_eligible(volume, index, rules.cfg) {
            return None;
        }
        let rec = &volume.records()[index];
        let path = Self::record_path(volume, index);
        if self.is_self_data_path(&path) || rules.excludes(&path) {
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

    fn next_content_batch(
        &self,
        drive: char,
        start: usize,
        rules: &ContentRules<'_>,
    ) -> (Vec<ContentIndexTask>, usize) {
        let volumes = self.volumes.read().unwrap();
        let Some(volume) = volumes.get(&drive) else {
            return (Vec::new(), start);
        };
        let end = (start + CONTENT_SCAN_CHUNK).min(volume.records().len());
        let mut tasks = Vec::new();
        let mut cursor = start;
        while cursor < end && tasks.len() < CONTENT_BATCH_SIZE {
            if let Some(task) = self.content_task(volume, cursor, rules) {
                tasks.push(task);
            }
            cursor += 1;
        }
        (tasks, cursor)
    }

    pub(super) fn changed_content_tasks(
        &self,
        changes: &HashMap<char, HashSet<u64>>,
        rules: &ContentRules<'_>,
    ) -> (Vec<ContentIndexTask>, Vec<(u64, u64)>) {
        let volumes = self.volumes.read().unwrap();
        let mut tasks = Vec::new();
        let mut deletions = HashSet::new();
        for (drive, frns) in changes {
            let Some(volume) = volumes.get(drive) else {
                continue;
            };
            for frn in frns {
                let Some(indices) = volume.record_indices_for_frn(*frn) else {
                    continue;
                };
                for &index in indices {
                    let index = index as usize;
                    if self.is_self_data_path(&Self::record_path(volume, index)) {
                        continue;
                    }
                    match self.content_task(volume, index, rules) {
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
            .flat_map(VolumeIndex::flush_pending_content_deletions)
            .collect()
    }

    pub(super) fn resolve_content_deletions(
        &self,
        pending: HashSet<(u64, u64)>,
        rules: &ContentRules<'_>,
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
                if volume.record_indices_for_frn(*frn).is_some_and(|indices| {
                    indices.iter().all(|&index| {
                        self.is_self_data_path(&Self::record_path(volume, index as usize))
                    })
                }) {
                    return false;
                }
                !volume.record_indices_for_frn(*frn).is_some_and(|indices| {
                    indices
                        .iter()
                        .any(|&index| self.content_task(volume, index as usize, rules).is_some())
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

    fn refresh_pending_metadata(&self, changes: &HashMap<char, HashSet<u64>>) {
        let mut pending: Vec<(char, u32, u64, String)> = Vec::new();
        {
            let lock = self.volumes.read().unwrap();
            for (drive, frns) in changes {
                let Some(volume) = lock.get(drive) else {
                    continue;
                };
                for frn in frns {
                    let Some(indices) = volume.record_indices_for_frn(*frn) else {
                        continue;
                    };
                    for &index in indices {
                        let record = &volume.records()[index as usize];
                        if record.is_tombstone() {
                            continue;
                        }
                        let path = volume.path_cache().get_full_path(
                            volume.records(),
                            volume.arena(),
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
            metadata: RefreshedRecordMetadata,
        }

        let updates: Vec<RefreshedMetadata> = pending
            .into_iter()
            .filter_map(|(drive, index, frn, path)| {
                let metadata = std::fs::symlink_metadata(path).ok()?;
                let (is_dir, _) = directory_and_reparse(&metadata);
                Some(RefreshedMetadata {
                    drive,
                    metadata: RefreshedRecordMetadata {
                        index,
                        frn,
                        size: if is_dir { 0 } else { metadata.len() },
                        created_ts: crate::filetime::system_time_to_filetime(metadata.created()),
                        modified_ts: crate::filetime::system_time_to_filetime(metadata.modified()),
                        win32_attributes:
                            crate::sync_engine::folder_source::metadata_win32_attributes(&metadata),
                    },
                })
            })
            .collect();

        let mut lock = self.volumes.write().unwrap();
        for update in updates {
            let Some(volume) = lock.get_mut(&update.drive) else {
                continue;
            };
            volume.apply_refreshed_metadata(update.metadata);
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

        let rules = ContentRules::new(&cfg);
        let (tasks, ineligible) = self.changed_content_tasks(&changes_by_drive, &rules);
        let mut pending = pending_deletions;
        pending.extend(ineligible);
        let deletions = self.resolve_content_deletions(pending, &rules);
        if tasks.is_empty() && deletions.is_empty() {
            return;
        }
        self.send_content_command(ContentCommand::IndexChanges { tasks, deletions });
    }
}

impl MultiVolumeEngine {
    pub(super) fn run_full_backfill(&self) {
        self.run_full_backfill_with(&super::worker_process::SubprocessWorkerPort::new(self));
    }

    pub(super) fn run_full_backfill_with(
        &self,
        port: &dyn super::worker_process::WorkerProcessPort,
    ) {
        if !self.content_pipeline.reset_cancel() {
            self.set_content_disabled();
            return;
        }
        self.drop_content_searcher();
        self.remove_backfill_marker();
        self.cleanup_stale_content_files();

        let cfg = self.get_config();
        if !cfg.content_index_enabled {
            self.set_content_disabled();
            return;
        }

        let rules = ContentRules::new(&cfg);
        let total = self.count_eligible(&rules);
        self.set_content_progress(0, total);

        let drives: Vec<char> = self.volumes.read().unwrap().keys().copied().collect();
        let mut processed = 0u64;
        let mut cleared = false;

        for drive in drives {
            let mut start = 0usize;
            loop {
                if self.content_pipeline.is_cancelled() {
                    self.set_content_disabled();
                    return;
                }
                let (tasks, next) = self.next_content_batch(drive, start, &rules);
                if tasks.is_empty() {
                    if next <= start {
                        break;
                    }
                    start = next;
                    continue;
                }
                let clear = !cleared;
                match self.run_worker_batch_with(port, &tasks, &[], clear) {
                    BatchOutcome::Completed => {
                        cleared = true;
                        processed += tasks.len() as u64;
                        self.set_content_progress(processed, total);
                    }
                    BatchOutcome::Cancelled => {
                        self.set_content_disabled();
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

        if !cleared {
            match self.run_worker_batch_with(port, &[], &[], true) {
                BatchOutcome::Completed => {}
                BatchOutcome::Cancelled => {
                    self.set_content_disabled();
                    return;
                }
                BatchOutcome::Failed(failure) => {
                    self.set_content_error(failure.message);
                    return;
                }
            }
        }

        if self.content_pipeline.is_cancelled() {
            self.set_content_disabled();
            return;
        }
        self.reopen_content_index_ready();
        self.write_backfill_marker(&cfg);
    }

    pub(super) fn run_incremental(&self, tasks: &[ContentIndexTask], deletions: &[(u64, u64)]) {
        self.run_incremental_with(tasks, deletions, |tasks, deletions, clear| {
            self.run_worker_batch(tasks, deletions, clear)
        });
    }

    pub(super) fn run_incremental_with<F>(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        run_batch: F,
    ) where
        F: Fn(&[ContentIndexTask], &[(u64, u64)], bool) -> BatchOutcome,
    {
        if tasks.is_empty() && deletions.is_empty() {
            return;
        }
        let cfg = self.get_config();
        if !cfg.content_index_enabled {
            self.set_content_disabled();
            return;
        }
        if self.content_pipeline.is_cancelled() {
            self.drop_content_searcher();
            self.set_content_disabled();
            return;
        }

        if tasks.is_empty() {
            // A deletion-only cycle still has to run the worker once so its deletions are committed.
            match run_batch(&[], deletions, false) {
                BatchOutcome::Completed => {}
                BatchOutcome::Cancelled => {
                    self.drop_content_searcher();
                    self.set_content_disabled();
                    return;
                }
                BatchOutcome::Failed(failure) => {
                    self.keep_last_content_snapshot(&failure.message);
                    return;
                }
            }
        } else {
            let chunk_count = tasks.len().div_ceil(CONTENT_BATCH_SIZE);
            for (chunk_index, chunk) in tasks.chunks(CONTENT_BATCH_SIZE).enumerate() {
                if self.content_pipeline.is_cancelled() {
                    self.drop_content_searcher();
                    self.set_content_disabled();
                    return;
                }
                let chunk_deletions = if chunk_index + 1 == chunk_count {
                    deletions
                } else {
                    &[]
                };
                match run_batch(chunk, chunk_deletions, false) {
                    BatchOutcome::Completed => {}
                    BatchOutcome::Cancelled => {
                        self.drop_content_searcher();
                        self.set_content_disabled();
                        return;
                    }
                    BatchOutcome::Failed(failure) => {
                        self.keep_last_content_snapshot(&failure.message);
                        return;
                    }
                }
            }
        }

        self.reopen_content_index_ready();
    }
}

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

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-volume synchronization engine and index lifecycle manager.

use cairn_content::ContentSearcher;
use cairn_core::sort::PresortedIndices;
use cairn_ntfs::volume::enumerate_fixed_drives;
use cairn_protocol::status::{
    ContentIndexState, ContentIndexStatus, VolumeState, VolumeStatusSummary,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::config::ServiceConfig;
use crate::persistence;

mod config_apply;
pub mod content_pipeline;
pub mod journal_reconciliation;
mod path_exclusions;
pub mod post_scan;
pub mod presorted;
mod search;
mod startup_scan;
mod treemap_build;
mod volume_index;
mod volume_status;

pub use journal_reconciliation::*;
pub use post_scan::*;
pub use search::{MultiSearchResult, SearchResultItem};
pub use treemap_build::TreeUnavailable;
pub use volume_index::VolumeIndex;

#[cfg(test)]
mod tests;

pub struct MultiVolumeEngine {
    volumes: RwLock<HashMap<char, VolumeIndex>>,
    index_dir: std::path::PathBuf,
    content_dir: std::path::PathBuf,
    /// Directories written by this process (MFT cache, Tantivy index, configuration and logs).
    /// Content extraction must never read them back: a Tantivy commit rewrites `meta.json`, and
    /// indexing that file would feed the index's own writes back in as new user content.
    self_data_dirs: Vec<std::path::PathBuf>,
    config_path: std::path::PathBuf,
    config: RwLock<ServiceConfig>,
    content_searcher: RwLock<Option<Arc<ContentSearcher>>>,
    content_status: RwLock<ContentIndexStatus>,
    content_cancel: Arc<AtomicBool>,
    content_commands: Mutex<Option<std::sync::mpsc::Sender<content_pipeline::ContentCommand>>>,
    /// Last complete set of volume summaries, outside the volume lock. A query holds the volume
    /// read lock for seconds; serving the response envelope from this cache keeps new connections
    /// (handshake, status probes) from waiting behind it. Refreshed on every uncontended read.
    summary_cache: Mutex<Vec<VolumeStatusSummary>>,
    /// Service data is owned by SYSTEM, so its configuration is written with the restricted
    /// descriptor.
    /// Whether the configuration on disk was authenticated at startup. A rejected file must not
    /// become trusted merely because a later reload reads it again.
    config_trusted: AtomicBool,
}

impl Default for MultiVolumeEngine {
    fn default() -> Self {
        Self::with_config(ServiceConfig::default())
    }
}

impl MultiVolumeEngine {
    pub fn new() -> Self {
        Self::with_config(ServiceConfig::default())
    }

    pub fn with_config(config: ServiceConfig) -> Self {
        let root = crate::security::default_service_data_root();
        Self::with_paths(
            &root.join("index"),
            &root.join("content"),
            &crate::config::service_config_path(),
            enumerate_fixed_drives(),
            config,
        )
        .with_service_data_root(&root)
    }

    /// Builds the engine around the SPEC SET-02 data layout, whose access control has already been
    /// enforced by [`crate::security::prepare_data_layout`].
    pub fn for_data_layout(
        layout: &crate::security::ServiceDataLayout,
        config: ServiceConfig,
    ) -> Self {
        let engine = Self::with_paths(
            &layout.index_dir,
            &layout.content_dir,
            &layout.config_path,
            enumerate_fixed_drives(),
            config,
        )
        .with_service_data_root(&layout.root);
        engine
            .config_trusted
            .store(!layout.config_untrusted, Ordering::SeqCst);
        engine
    }

    pub fn with_index_dir_and_drives(index_dir: &Path, drives: Vec<char>) -> Self {
        Self::with_index_dir_drives_and_config(index_dir, drives, ServiceConfig::default())
    }

    /// Test and development layout: the content index lives below `index_dir`, while the service
    /// configuration keeps its real location.
    pub fn with_index_dir_drives_and_config(
        index_dir: &Path,
        drives: Vec<char>,
        config: ServiceConfig,
    ) -> Self {
        Self::with_paths(
            index_dir,
            &index_dir.join("content"),
            &crate::config::service_config_path(),
            drives,
            config,
        )
    }

    pub fn with_paths(
        index_dir: &Path,
        content_dir: &Path,
        config_path: &Path,
        drives: Vec<char>,
        config: ServiceConfig,
    ) -> Self {
        let mut self_data_dirs = Vec::new();
        Self::push_self_data_dir(&mut self_data_dirs, content_dir);
        Self::push_self_data_dir(&mut self_data_dirs, index_dir);
        // Configuration and logs sit beside these two directories. Protecting their common parent
        // covers the whole service data tree without having to enumerate every child.
        if let Some(parent) = content_dir.parent() {
            Self::push_self_data_dir(&mut self_data_dirs, parent);
        }
        // Resolve the roots to file ids once, before any volume exists, so an initial $MFT scan, a
        // cached-index replay and every later USN batch all filter the same subtree by identity.
        let excluded_roots = Self::excluded_roots_by_drive(&self_data_dirs);
        let mut map = HashMap::new();

        // Filter excluded_drives during volume initialization and scanning (IDX-01)
        let active_drives: Vec<char> = drives
            .into_iter()
            .filter(|&d| {
                !config
                    .excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&d))
            })
            .collect();

        for d in active_drives {
            let mut vol = VolumeIndex::new(d);
            if let Some(roots) = excluded_roots.get(&d) {
                vol.set_excluded_roots(roots.iter().copied());
            }
            let cache_file = index_dir.join(format!("vol_{}.idx", d));
            let mut loaded_ok = false;
            if cache_file.exists() {
                match persistence::load_volume_index(&cache_file) {
                    Ok(loaded) => {
                        let directory_sizes_valid = loaded.directory_sizes_valid;
                        vol.records = loaded.records;
                        vol.live_count = vol
                            .records
                            .iter()
                            .filter(|record| !record.is_tombstone())
                            .count() as u64;
                        vol.arena = loaded.arena;
                        vol.last_usn = loaded.last_usn;
                        vol.journal_id = loaded.journal_id;
                        vol.volume_serial = loaded.volume_serial;
                        vol.directory_sizes_valid = directory_sizes_valid;

                        for (idx, r) in vol.records.iter().enumerate() {
                            if !r.is_tombstone() {
                                vol.frn_to_idx.entry(r.frn).or_default().push(idx as u32);
                            }
                        }

                        // Caches written before folder sizes were persisted still store zeroes for
                        // every directory. Aggregate once and immediately rewrite the flag, so only
                        // the first launch after the upgrade pays for it.
                        if !vol.directory_sizes_valid {
                            vol.recompute_directory_sizes();
                            match vol.save_to_dir(index_dir) {
                                Ok(_) => tracing::info!(
                                    "Migrated cached index for {}: computed directory sizes",
                                    d
                                ),
                                Err(error) => tracing::warn!(
                                    "Cached index for {} has computed directory sizes that could not be persisted: {}",
                                    d,
                                    error
                                ),
                            }
                        }
                        // The presorted size array is built from whatever the records now hold, so
                        // the first page and size sort agree with the table.
                        vol.presorted = PresortedIndices::build(
                            &vol.records,
                            &vol.arena,
                            &vol.path_cache,
                            vol.drive_letter,
                        );
                        vol.presorted_dirty = false;

                        // Validate against live volume if accessible and catch up USN (IDX-04, IDX-05, IDX-07)
                        #[cfg(windows)]
                        if enumerate_fixed_drives().contains(&d) {
                            match cairn_ntfs::RawVolumeHandle::open(d) {
                                Ok(handle) => match handle.query_usn_journal() {
                                    Ok(usn_data) => {
                                        // A saved position must lie inside the journal's current
                                        // [first, next] window. Checking only the lower bound left a
                                        // cache whose USN was ahead of the journal (a reset/replaced
                                        // journal or a stale cache) permanently CatchingUp: every
                                        // read started past the tail and the index could never catch
                                        // up. Treat any out-of-window position as a rebuild.
                                        if !Self::cached_position_is_usable(
                                            vol.journal_id,
                                            vol.last_usn,
                                            &usn_data,
                                        ) {
                                            tracing::warn!(
                                                "USN journal mismatch or out of range for {} (cached journal {}, last_usn {}; current journal {}, first {}, next {}): will rebuild",
                                                d,
                                                vol.journal_id,
                                                vol.last_usn,
                                                usn_data.journal_id,
                                                usn_data.first_usn,
                                                usn_data.next_usn
                                            );
                                            vol.clear();
                                            vol.status.set(VolumeState::Rebuilding);
                                            loaded_ok = false;
                                        } else if Self::backlog_is_too_large(
                                            vol.last_usn,
                                            usn_data.next_usn,
                                        ) {
                                            tracing::warn!(
                                                "USN backlog for {} is {} bytes; rebuilding instead of replaying",
                                                d,
                                                usn_data.next_usn.saturating_sub(vol.last_usn)
                                            );
                                            vol.clear();
                                            vol.status.set(VolumeState::Rebuilding);
                                            loaded_ok = false;
                                        } else {
                                            // Replay the change journal from the saved position up to
                                            // the journal tail (IDX-04). An index whose backlog is not
                                            // exhausted is reported as CatchingUp, never as Synced,
                                            // because stale results must be annotated (IDX-07).
                                            let mut buf = vec![0u8; 512 * 1024];
                                            match replay_usn_backlog(
                                                &handle,
                                                &mut vol,
                                                &mut buf,
                                                usn_data.journal_id,
                                                usn_data.next_usn,
                                            ) {
                                                Ok(()) => {
                                                    loaded_ok = true;
                                                    tracing::info!(
                                                        "Loaded cached index for {}: {} records, state {:?}",
                                                        d,
                                                        vol.records.len(),
                                                        vol.status
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "USN catchup read for {}: failed ({:?}), will rebuild",
                                                        d,
                                                        e
                                                    );
                                                    vol.clear();
                                                    vol.status.set(VolumeState::Rebuilding);
                                                    loaded_ok = false;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "USN query failed for {}: ({:?}), falling back to SnapshotMode (IDX-08)",
                                            d,
                                            e
                                        );
                                        vol.status.set(VolumeState::SnapshotMode);
                                        vol.usn_enabled = false;
                                        loaded_ok = true;
                                    }
                                },
                                Err(e) => {
                                    // The cached index is retained but its freshness cannot be
                                    // established, so it is Offline rather than Synced (IDX-07).
                                    tracing::warn!(
                                        "Cannot open live handle for {} ({:?}), retaining cached index as Offline",
                                        d,
                                        e
                                    );
                                    vol.status.set(VolumeState::Offline);
                                    loaded_ok = true;
                                }
                            }
                        } else {
                            // The drive is not present anymore: the cached index can only be stale.
                            tracing::info!("Volume {} is no longer enumerated: marked Offline", d);
                            vol.status.set(VolumeState::Offline);
                            loaded_ok = true;
                        }
                        #[cfg(not(windows))]
                        {
                            // No change journal exists here, so the cached index is served as a
                            // snapshot whose freshness nobody has proven (IDX-07).
                            vol.status.set(VolumeState::SnapshotMode);
                            vol.usn_enabled = false;
                            loaded_ok = true;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load cached index for {}: {}, will rebuild",
                            d,
                            e
                        );
                        vol.clear();
                        vol.status.set(VolumeState::Rebuilding);
                        loaded_ok = false;
                    }
                }
            }

            if !loaded_ok && vol.status.state() != VolumeState::Offline {
                let mut scan_succeeded = false;
                #[cfg(windows)]
                {
                    tracing::info!(
                        "No cache or invalid cache for {}: attempting live MFT scan...",
                        d
                    );
                    // T0 is captured before the scan runs, so every record produced while `$MFT` is
                    // being read is replayed afterwards instead of being skipped forever (IDX-07).
                    let scan_start = open_journal_at_scan_start(d);
                    match cairn_ntfs::scan_live_volume_mft(d, 4096) {
                        Ok(items) if !items.is_empty() => {
                            tracing::info!(
                                "Scanned {} items from live MFT for {}:",
                                items.len(),
                                d
                            );
                            vol.ingest_items(items);
                            scan_succeeded =
                                finalize_scanned_volume(&mut vol, index_dir, scan_start).is_ok();
                        }
                        Ok(_) => {
                            tracing::debug!("Live MFT scan for {}: returned 0 items", d);
                        }
                        Err(err) => {
                            tracing::debug!("Live MFT scan for {}: not available ({})", d, err);
                        }
                    }
                }

                // Without a usable scan the volume holds no records at all, so it must not be
                // reported as fresh data (IDX-05, IDX-07).
                if !scan_succeeded {
                    vol.clear();
                    vol.status.set(VolumeState::Rebuilding);
                }
            }

            // Caches written before the service data root was excluded still hold its files and an
            // unbounded tombstone history. Reclaim both before the volume serves queries.
            let before_compact = vol.records.len();
            if vol.compact_if_needed() {
                tracing::info!(
                    "Volume {}: compacted index from {} to {} records ({} dropped)",
                    d,
                    before_compact,
                    vol.records.len(),
                    before_compact.saturating_sub(vol.records.len())
                );
            }

            map.insert(d, vol);
        }

        let content_enabled = config.content_index_enabled;
        Self {
            volumes: RwLock::new(map),
            index_dir: index_dir.to_path_buf(),
            content_dir: content_dir.to_path_buf(),
            self_data_dirs,
            config_path: config_path.to_path_buf(),
            config: RwLock::new(config),
            content_searcher: RwLock::new(None),
            content_status: RwLock::new(if content_enabled {
                ContentIndexStatus {
                    state: ContentIndexState::Backfilling {
                        processed: 0,
                        total: 0,
                    },
                    ..Default::default()
                }
            } else {
                ContentIndexStatus::default()
            }),
            content_cancel: Arc::new(AtomicBool::new(false)),
            content_commands: Mutex::new(None),
            summary_cache: Mutex::new(Vec::new()),
            config_trusted: AtomicBool::new(true),
        }
    }

    /// Adds the data root that holds this indexer's content index, MFT cache, configuration and
    /// logs. The root itself is protected, not just its known children, so `service.toml` and the
    /// log directory are covered without enumerating them here. A bare volume root is ignored:
    /// protecting it would disable content indexing for the whole disk.
    pub(crate) fn with_service_data_root(mut self, root: &Path) -> Self {
        if Self::push_self_data_dir(&mut self.self_data_dirs, root) {
            // Volumes already loaded (the service layout) need the root for the USN events that
            // follow; a fresh scan is not repeated here.
            self.add_self_data_exclusion(root);
        }
        self
    }

    fn push_self_data_dir(dirs: &mut Vec<std::path::PathBuf>, dir: &Path) -> bool {
        let normalized = dir.to_string_lossy().replace('/', "\\");
        let trimmed = normalized.trim_end_matches('\\');
        if trimmed.is_empty() || (trimmed.len() == 2 && trimmed.ends_with(':')) {
            return false;
        }
        if dirs.iter().any(|existing| existing == dir) {
            return false;
        }
        dirs.push(dir.to_path_buf());
        true
    }

    /// Resolves every service data directory to the FRN of the volume that owns it. A path that no
    /// longer exists (an old layout, an unmounted drive) is skipped: it cannot be indexed either.
    fn excluded_roots_by_drive(dirs: &[std::path::PathBuf]) -> HashMap<char, HashSet<u64>> {
        let mut roots: HashMap<char, HashSet<u64>> = HashMap::new();
        for dir in dirs {
            let Some(drive) = Self::drive_letter_of_path(dir) else {
                continue;
            };
            match cairn_ntfs::directory_file_id(dir) {
                Some(frn) => {
                    roots.entry(drive).or_default().insert(frn);
                }
                None => tracing::debug!(
                    "Cannot resolve the file id of service data directory {}; it stays indexed",
                    dir.display()
                ),
            }
        }
        roots
    }

    /// Seeds an already loaded volume with one excluded root. Used when the data root is attached
    /// after the volumes were built, so subsequent USN events are filtered immediately.
    fn add_self_data_exclusion(&self, dir: &Path) {
        let Some(drive) = Self::drive_letter_of_path(dir) else {
            return;
        };
        let Some(frn) = cairn_ntfs::directory_file_id(dir) else {
            tracing::debug!(
                "Cannot resolve the file id of service data directory {}; it stays indexed",
                dir.display()
            );
            return;
        };
        let mut volumes = self.volumes.write().unwrap();
        if let Some(volume) = volumes.get_mut(&drive) {
            volume.set_excluded_roots(std::iter::once(frn));
        }
    }

    fn drive_letter_of_path(path: &Path) -> Option<char> {
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            match path.components().next()? {
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                        Some((letter as char).to_ascii_uppercase())
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            None
        }
    }
}

impl MultiVolumeEngine {
    fn collect_volume_summaries(volumes: &HashMap<char, VolumeIndex>) -> Vec<VolumeStatusSummary> {
        let mut summaries: Vec<_> = volumes.values().map(|v| v.to_summary()).collect();
        summaries.sort_by_key(|s| s.drive_letter);
        summaries
    }

    fn filter_excluded_drives(
        &self,
        summaries: Vec<VolumeStatusSummary>,
    ) -> Vec<VolumeStatusSummary> {
        let cfg = self.config.read().unwrap();
        summaries
            .into_iter()
            .filter(|s| {
                !cfg.excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&s.drive_letter))
            })
            .collect()
    }

    /// Cached summaries for ordinary envelopes. A query holds the volume read lock for seconds;
    /// a handshake must still complete, so a summary one query old is served instead of waiting.
    pub fn get_volume_summaries(&self) -> Vec<VolumeStatusSummary> {
        let summaries = match self.volumes.try_read() {
            Ok(volumes) => {
                let summaries = Self::collect_volume_summaries(&volumes);
                *self.summary_cache.lock().unwrap() = summaries.clone();
                summaries
            }
            Err(_) => {
                let cached = self.summary_cache.lock().unwrap().clone();
                if cached.is_empty() {
                    let volumes = self.volumes.read().unwrap();
                    let summaries = Self::collect_volume_summaries(&volumes);
                    *self.summary_cache.lock().unwrap() = summaries.clone();
                    summaries
                } else {
                    cached
                }
            }
        };
        self.filter_excluded_drives(summaries)
    }

    /// Fresh summaries for the answer to a query: its final volume state decides whether the
    /// client shows an animating scan or a settled result. The query has already released the
    /// index, so this read only waits for an unrelated short writer critical section.
    pub fn get_volume_summaries_fresh(&self) -> Vec<VolumeStatusSummary> {
        let summaries = {
            let volumes = self.volumes.read().unwrap();
            let summaries = Self::collect_volume_summaries(&volumes);
            *self.summary_cache.lock().unwrap() = summaries.clone();
            summaries
        };
        self.filter_excluded_drives(summaries)
    }

    pub fn with_volumes<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&HashMap<char, VolumeIndex>) -> R,
    {
        let lock = self.volumes.read().unwrap();
        f(&lock)
    }

    #[cfg(test)]
    pub fn with_primary_volume<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&VolumeIndex) -> R,
    {
        let lock = self.volumes.read().unwrap();
        let vol = lock.get(&'C').or_else(|| lock.values().next())?;
        Some(f(vol))
    }

    #[cfg(test)]
    pub fn with_primary_volume_mut<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut VolumeIndex) -> R,
    {
        let mut lock = self.volumes.write().unwrap();
        let vol = if lock.contains_key(&'C') {
            lock.get_mut(&'C')?
        } else {
            lock.values_mut().next()?
        };
        Some(f(vol))
    }
}

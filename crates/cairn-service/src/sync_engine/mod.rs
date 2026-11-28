// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-volume synchronization engine and index lifecycle manager.

use cairn_core::IndexMemoryFootprint;
use cairn_core::property_store::{FOLDER_SOURCE_ID_BASE, SourceId};
use cairn_ntfs::volume::enumerate_fixed_drives;
use cairn_ntfs::volume_identity::{VolumeIdentity, enumerate_volumes};
use cairn_protocol::status::{
    IndexJournalEntry, IndexJournalStatus, SourceStatusSummary, VolumeState, VolumeStatusSummary,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::ServiceConfig;
use crate::persistence;

const MAX_FOLDER_SOURCES: usize = 64;

mod config_apply;
pub mod content_pipeline;
pub mod folder_source;
pub mod index_journal;
pub mod journal_reconciliation;
pub(crate) mod path_exclusions;
pub mod post_scan;
pub mod property_pipeline;
mod property_refresh;
mod search;
mod startup_scan;
mod volume_index;
mod volume_status;
mod worker_runner;

pub use journal_reconciliation::*;
pub use post_scan::*;
pub use search::{AllFilesView, MatchSet, SearchOutcome, SearchResultItem};
pub(crate) use volume_index::build_file_row;
pub use volume_index::{CompactPolicy, CompactionReport, RefreshedRecordMetadata, VolumeIndex};

#[cfg(test)]
mod tests;

pub(crate) fn unix_now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn log_safe(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

pub trait WorkerBinaryLocator: Send + Sync {
    fn locate(&self) -> Result<PathBuf, String>;
}

pub struct AdjacentWorkerBinaryLocator;

impl WorkerBinaryLocator for AdjacentWorkerBinaryLocator {
    fn locate(&self) -> Result<PathBuf, String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("could not locate the service executable: {err}"))?;
        let dir = exe
            .parent()
            .ok_or_else(|| "service executable has no parent directory".to_string())?;
        for name in ["CairnWorker.exe", "CairnWorker"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        Err(format!(
            "CairnWorker executable not found beside {}",
            exe.display()
        ))
    }
}

pub struct FixedWorkerBinaryLocator(pub PathBuf);

impl WorkerBinaryLocator for FixedWorkerBinaryLocator {
    fn locate(&self) -> Result<PathBuf, String> {
        Ok(self.0.clone())
    }
}

pub struct MultiVolumeEngine {
    // See docs/engineering/indexing.md#3-并发内存与流式查询 for the lock order.
    persist_lock: Mutex<()>,
    volumes: RwLock<HashMap<char, VolumeIndex>>,
    folder_sources: Arc<RwLock<HashMap<SourceId, folder_source::FolderSource>>>,
    folder_watchers: Mutex<HashMap<SourceId, folder_source::FolderWatcherHandle>>,
    folder_scanning: Arc<Mutex<HashSet<SourceId>>>,
    next_folder_source_id: AtomicU32,
    property_pipeline: property_pipeline::PropertyPipeline,
    index_journal: Arc<Mutex<index_journal::IndexJournal>>,
    index_dir: PathBuf,
    self_data_dirs: Vec<PathBuf>,
    config_path: PathBuf,
    config: RwLock<ServiceConfig>,
    config_apply_lock: Mutex<()>,
    content_pipeline: content_pipeline::ContentPipeline,
    summary_cache: Mutex<Vec<VolumeStatusSummary>>,
    usn_read_buf: Mutex<Vec<u8>>,
    worker_locator: Arc<dyn WorkerBinaryLocator>,
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

    pub fn with_worker_locator(mut self, locator: Arc<dyn WorkerBinaryLocator>) -> Self {
        self.worker_locator = locator;
        self
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
        worker_runner::cleanup_stale_property_work_files(&index_dir.join("property-work"));
        let mut self_data_dirs = Vec::new();
        Self::push_self_data_dir(&mut self_data_dirs, content_dir);
        Self::push_self_data_dir(&mut self_data_dirs, index_dir);
        if let Some(parent) = content_dir.parent() {
            Self::push_self_data_dir(&mut self_data_dirs, parent);
        }
        let excluded_roots = Self::excluded_roots_by_drive(&self_data_dirs);
        let mut map = HashMap::new();

        let active_drives: Vec<char> = drives
            .into_iter()
            .filter(|&d| {
                !config
                    .excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&d))
            })
            .collect();
        let identities_by_drive: HashMap<char, VolumeIdentity> = if active_drives.is_empty() {
            HashMap::new()
        } else {
            enumerate_volumes()
                .into_iter()
                .filter_map(|id| id.drive_letter.map(|ch| (ch.to_ascii_uppercase(), id)))
                .collect()
        };

        let full_path_exclusions =
            crate::exclusion::ExclusionRules::parse(&config.excluded_paths).uses_full_path_rules();
        for d in active_drives {
            let mut vol = VolumeIndex::new(d);
            vol.set_full_path_exclusion_rules(full_path_exclusions);
            let live_identity = identities_by_drive.get(&d.to_ascii_uppercase());
            if let Some(roots) = excluded_roots.get(&d) {
                vol.set_excluded_roots(roots.iter().copied());
            }
            let cache_file = index_dir.join(format!("vol_{d}.idx"));
            let mut loaded_ok = false;
            if cache_file.exists() {
                match persistence::load_volume_index(&cache_file) {
                    Ok(loaded) => {
                        vol.restore_from_loaded(loaded);
                        let saved_guid = persistence::load_volume_guid_sidecar(index_dir, d);
                        if let Some(ref guid) = saved_guid {
                            vol.volume_guid = guid.clone();
                        }

                        let identity_mismatch = if let Some(identity) = live_identity {
                            if saved_guid.is_some() {
                                !vol.matches_volume_identity(identity)
                            } else if vol.volume_serial != 0 && identity.volume_serial != 0 {
                                vol.volume_serial != identity.volume_serial
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if identity_mismatch {
                            tracing::warn!(
                                "Volume {} identity changed since cached index was written: rebuilding",
                                d
                            );
                            vol.clear();
                            vol.status.set(VolumeState::Rebuilding);
                            loaded_ok = false;
                        } else {
                            if let Some(identity) = live_identity {
                                vol.set_identity(identity.clone());
                                if saved_guid.is_none()
                                    && let Err(err) = persistence::save_volume_guid_sidecar(
                                        index_dir,
                                        d,
                                        &vol.volume_guid,
                                    )
                                {
                                    tracing::warn!(
                                        "Failed to persist GUID sidecar for volume {}: {}",
                                        d,
                                        err
                                    );
                                }
                            }

                            if !vol.directory_sizes_valid() {
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

                            #[cfg(windows)]
                            if enumerate_fixed_drives().contains(&d) {
                                match cairn_ntfs::RawVolumeHandle::open(d) {
                                    Ok(handle) => match handle.query_usn_journal() {
                                        Ok(usn_data) => {
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
                                                            vol.records().len(),
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
                                tracing::info!(
                                    "Volume {} is no longer enumerated: marked Offline",
                                    d
                                );
                                vol.status.set(VolumeState::Offline);
                                loaded_ok = true;
                            }
                            #[cfg(not(windows))]
                            {
                                vol.status.set(VolumeState::SnapshotMode);
                                vol.usn_enabled = false;
                                loaded_ok = true;
                            }
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
                if let Some(identity) = live_identity {
                    vol.set_identity(identity.clone());
                }
                let mut scan_succeeded = false;
                #[cfg(windows)]
                {
                    tracing::info!(
                        "No cache or invalid cache for {}: attempting live MFT scan...",
                        d
                    );
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

                if !scan_succeeded {
                    vol.clear();
                    vol.status.set(VolumeState::Rebuilding);
                }
            }

            let before_compact = vol.records().len();
            if vol.compact_if_needed() {
                tracing::info!(
                    "Volume {}: compacted index from {} to {} records ({} dropped)",
                    d,
                    before_compact,
                    vol.records().len(),
                    before_compact.saturating_sub(vol.records().len())
                );
            }

            map.insert(d, vol);
        }

        let content_enabled = config.content_index_enabled;
        let property_enabled = config.property_index_enabled;
        let journal_enabled = config.index_journal_enabled;
        let engine = Self {
            volumes: RwLock::new(map),
            folder_sources: Arc::new(RwLock::new(HashMap::new())),
            folder_watchers: Mutex::new(HashMap::new()),
            folder_scanning: Arc::new(Mutex::new(HashSet::new())),
            next_folder_source_id: AtomicU32::new(FOLDER_SOURCE_ID_BASE),
            property_pipeline: property_pipeline::PropertyPipeline::new(property_enabled),
            index_journal: Arc::new(Mutex::new(index_journal::IndexJournal::new(
                journal_enabled,
            ))),
            index_dir: index_dir.to_path_buf(),
            self_data_dirs,
            config_path: config_path.to_path_buf(),
            config: RwLock::new(config),
            persist_lock: Mutex::new(()),
            config_apply_lock: Mutex::new(()),
            content_pipeline: content_pipeline::ContentPipeline::new(content_dir, content_enabled),
            summary_cache: Mutex::new(Vec::new()),
            usn_read_buf: Mutex::new(Vec::new()),
            worker_locator: Arc::new(AdjacentWorkerBinaryLocator),
            config_trusted: AtomicBool::new(true),
        };
        engine.ensure_folder_sources_loaded();
        engine
    }

    #[cfg(test)]
    pub(crate) fn content_dir(&self) -> &Path {
        self.content_pipeline.dir()
    }

    pub(crate) fn with_service_data_root(mut self, root: &Path) -> Self {
        if Self::push_self_data_dir(&mut self.self_data_dirs, root) {
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
        let mut summaries: Vec<_> = volumes
            .values()
            .map(volume_index::VolumeIndex::to_summary)
            .collect();
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

    pub(crate) fn with_sources_for_rows<F, R>(&self, f: F) -> R
    where
        F: FnOnce(crate::session::SourceTables<'_>) -> R,
    {
        let volumes = self.volumes.read().unwrap();
        let folders = self.folder_sources.read().unwrap();
        let properties = self.property_pipeline.read_tables();
        f(crate::session::SourceTables {
            volumes: &volumes,
            folders: &folders,
            properties: &properties,
        })
    }

    /// Ensures configured directory sources are loaded and watched in memory.
    pub fn ensure_folder_sources_loaded(&self) {
        let cfg = self.config.read().unwrap().clone();
        self.sync_folder_sources_from_config(&cfg.folder_sources, &cfg.excluded_paths);
    }

    pub(super) fn sync_folder_sources_from_config(
        &self,
        configured_roots: &[String],
        excluded_paths: &[String],
    ) {
        let mut desired: Vec<(PathBuf, String)> = Vec::new();
        let mut desired_keys: HashSet<String> = HashSet::new();
        for raw in configured_roots {
            let normalized = folder_source::normalize_folder_root(Path::new(raw));
            let (path, display_root) = match normalized {
                Ok(normalized) => normalized,
                Err(err) => {
                    if let Ok(unreachable) =
                        folder_source::normalize_folder_root_syntax(Path::new(raw))
                    {
                        tracing::warn!(
                            root = %unreachable,
                            error = %err,
                            "folder source is currently unreachable; keeping its index"
                        );
                        desired_keys.insert(unreachable.to_ascii_lowercase());
                    } else {
                        tracing::warn!(root = ?raw, "folder source ignored: invalid path");
                    }
                    continue;
                }
            };
            if let Some(drive) = self.folder_source_volume_conflict(&path) {
                tracing::warn!(
                    root = %display_root,
                    drive = %drive,
                    "folder source ignored: path is inside an indexed volume"
                );
                continue;
            }
            let key = display_root.to_ascii_lowercase();
            if desired_keys.insert(key) {
                desired.push((path, display_root));
            }
        }
        if desired.len() > MAX_FOLDER_SOURCES {
            tracing::warn!(
                configured = desired.len(),
                limit = MAX_FOLDER_SOURCES,
                "folder source list truncated"
            );
            desired.truncate(MAX_FOLDER_SOURCES);
        }

        let (removed_ids, dropped_handles, present_keys) = {
            let mut folders = self.folder_sources.write().unwrap();
            let mut removed = Vec::new();
            folders.retain(|&sid, src| {
                let keep = desired_keys.contains(&src.display_root().to_ascii_lowercase());
                if !keep {
                    removed.push(sid);
                }
                keep
            });
            let mut present = HashSet::new();
            for src in folders.values_mut() {
                src.set_excluded_paths(excluded_paths);
                present.insert(src.display_root().to_ascii_lowercase());
            }
            let dropped_handles: Vec<folder_source::FolderWatcherHandle> = if removed.is_empty() {
                Vec::new()
            } else {
                let mut watchers = self.folder_watchers.lock().unwrap();
                removed
                    .iter()
                    .filter_map(|sid| watchers.remove(sid))
                    .collect()
            };
            (removed, dropped_handles, present)
        };

        drop(dropped_handles);
        if !removed_ids.is_empty() {
            self.property_pipeline.clear_sources(&removed_ids);
        }

        for (root_path, display) in desired {
            let key = display.to_ascii_lowercase();
            if present_keys.contains(&key) {
                continue;
            }
            let sid = SourceId(self.next_folder_source_id.fetch_add(1, Ordering::Relaxed));
            let Ok(mut source) = folder_source::FolderSource::new(sid, &root_path) else {
                continue;
            };
            if !folder_source::reserve_scan(&self.folder_scanning, sid) {
                continue;
            }
            source.set_excluded_paths(excluded_paths);
            source.begin_scan();
            let inserted = {
                let mut folders = self.folder_sources.write().unwrap();
                if folders
                    .values()
                    .any(|s| s.display_root().eq_ignore_ascii_case(&display))
                {
                    false
                } else {
                    folders.insert(sid, source);
                    true
                }
            };
            if !inserted {
                folder_source::release_scan(&self.folder_scanning, sid);
                continue;
            }
            self.start_folder_watcher_for(sid, root_path);
            self.folder_scan_context()
                .spawn_reserved_scan(sid, unix_now_secs());
        }
    }

    fn folder_scan_context(&self) -> folder_source::FolderScanContext {
        folder_source::FolderScanContext {
            sources: Arc::clone(&self.folder_sources),
            property_handles: self.property_refresh_handles(),
            scanning: Arc::clone(&self.folder_scanning),
            journal: Arc::clone(&self.index_journal),
        }
    }

    fn folder_source_volume_conflict(&self, root: &Path) -> Option<char> {
        let display = root.to_string_lossy();
        let drive = cairn_core::path_cache::drive_letter_prefix(&display)?.to_ascii_uppercase();
        let excluded = self
            .config
            .read()
            .unwrap()
            .excluded_drives
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(&drive));
        if excluded {
            return None;
        }
        self.volumes
            .read()
            .unwrap()
            .contains_key(&drive)
            .then_some(drive)
    }

    fn start_folder_scan(&self, source_id: SourceId) -> Result<bool, String> {
        if !folder_source::reserve_scan(&self.folder_scanning, source_id) {
            return Ok(false);
        }
        let (root, live) = {
            let mut folders = self.folder_sources.write().unwrap();
            let Some(source) = folders.get_mut(&source_id) else {
                folder_source::release_scan(&self.folder_scanning, source_id);
                return Err(format!("source {} is not indexed", source_id.0));
            };
            source.begin_scan();
            (source.root_path.clone(), source.live_watch_enabled())
        };
        let has_watcher = {
            let mut watchers = self.folder_watchers.lock().unwrap();
            watchers.retain(|_, handle| !handle.is_finished());
            watchers.contains_key(&source_id)
        };
        if !live || !has_watcher {
            let previous = self.folder_watchers.lock().unwrap().remove(&source_id);
            drop(previous);
            self.start_folder_watcher_for(source_id, root);
        }
        if self
            .folder_scan_context()
            .spawn_reserved_scan(source_id, unix_now_secs())
        {
            Ok(true)
        } else {
            Err(format!("could not start a scan for source {}", source_id.0))
        }
    }

    fn start_folder_watcher_for(&self, source_id: SourceId, root_path: PathBuf) {
        if !root_path.is_dir() {
            let mut folders = self.folder_sources.write().unwrap();
            if let Some(src) = folders.get_mut(&source_id) {
                src.set_live_watch_enabled(false);
            }
            return;
        }
        let handle_opt = {
            #[cfg(windows)]
            {
                let scan_ctx = self.folder_scan_context();
                folder_source::spawn_folder_watcher(root_path, move |events| {
                    if events
                        .iter()
                        .any(|event| matches!(event, folder_source::FolderWatchEvent::WatchFailed))
                    {
                        let mut folders = scan_ctx.sources.write().unwrap();
                        if let Some(source) = folders.get_mut(&source_id) {
                            source.set_live_watch_enabled(false);
                        }
                        tracing::warn!(
                            source = source_id.0,
                            "folder watcher stopped; source is now snapshot-only"
                        );
                        return;
                    }
                    let outcome = {
                        let mut folders = scan_ctx.sources.write().unwrap();
                        let Some(source) = folders.get_mut(&source_id) else {
                            return;
                        };
                        let mut journal_lock = scan_ctx.journal.lock().unwrap();
                        source.apply_folder_events(
                            &events,
                            Some(&mut *journal_lock),
                            unix_now_secs(),
                        )
                    };
                    if !outcome.changed_records.is_empty() {
                        scan_ctx
                            .property_handles
                            .enqueue_records_and_wake(source_id, outcome.changed_records);
                    }
                    if outcome.needs_rescan {
                        scan_ctx.reserve_and_spawn_scan(source_id, unix_now_secs());
                    }
                })
            }
            #[cfg(not(windows))]
            {
                let _ = &root_path;
                None::<folder_source::FolderWatcherHandle>
            }
        };
        let active = handle_opt.is_some();
        if !active {
            tracing::warn!(
                source = source_id.0,
                "folder watcher could not start; source stays snapshot-only"
            );
        }
        let (previous, orphan) = {
            let mut folders = self.folder_sources.write().unwrap();
            let mut watchers = self.folder_watchers.lock().unwrap();
            if let Some(src) = folders.get_mut(&source_id) {
                src.set_live_watch_enabled(active);
                let prev = if let Some(handle) = handle_opt {
                    watchers.insert(source_id, handle)
                } else {
                    watchers.remove(&source_id)
                };
                (prev, None)
            } else {
                (watchers.remove(&source_id), handle_opt)
            }
        };
        drop((previous, orphan));
    }

    pub fn add_folder_source(
        &self,
        path: &Path,
        persist: bool,
    ) -> Result<SourceStatusSummary, String> {
        let (root_path, display) = folder_source::normalize_folder_root(path)?;
        let meta = std::fs::symlink_metadata(&root_path)
            .map_err(|err| format!("folder source {} is invalid: {err}", root_path.display()))?;
        if !meta.is_dir() {
            return Err(format!(
                "folder source {} is not a directory",
                root_path.display()
            ));
        }
        if let Some(drive) = self.folder_source_volume_conflict(&root_path) {
            return Err(format!(
                "folder source {display} is inside indexed volume {drive}:"
            ));
        }

        self.apply_config_change(persist, |current| {
            let mut next = current.clone();
            let already_listed = next.folder_sources.iter().any(|existing| {
                folder_source::normalize_folder_root(Path::new(existing))
                    .is_ok_and(|(_, norm)| norm.eq_ignore_ascii_case(&display))
            });
            if !already_listed {
                if next.folder_sources.len() >= MAX_FOLDER_SOURCES {
                    return Err(format!(
                        "folder source limit is {MAX_FOLDER_SOURCES}; remove one before adding {display}"
                    ));
                }
                next.folder_sources.push(display.clone());
            }
            Ok(next)
        })?;

        let folders = self.folder_sources.read().unwrap();
        folders
            .values()
            .find(|src| src.display_root().eq_ignore_ascii_case(&display))
            .map(folder_source::FolderSource::to_summary)
            .ok_or_else(|| format!("folder source {display} was not registered"))
    }

    pub fn remove_folder_source(&self, path: &Path, persist: bool) -> Result<bool, String> {
        let display = folder_source::normalize_folder_root(path)
            .map(|(_, display)| display)
            .or_else(|_| folder_source::normalize_folder_root_syntax(path))?;
        let mut removed = false;
        self.apply_config_change(persist, |current| {
            let mut next = current.clone();
            let before = next.folder_sources.len();
            next.folder_sources.retain(|existing| {
                folder_source::normalize_folder_root(Path::new(existing))
                    .map_or(!existing.eq_ignore_ascii_case(&display), |(_, norm)| {
                        !norm.eq_ignore_ascii_case(&display)
                    })
            });
            removed = next.folder_sources.len() != before;
            Ok(next)
        })?;
        Ok(removed)
    }

    pub fn active_drives(&self) -> Vec<char> {
        self.volumes.read().unwrap().keys().copied().collect()
    }

    pub fn rescan_source(&self, source_id: SourceId) -> Result<usize, String> {
        if let Some(drive) = source_id.drive_letter() {
            return self.rescan_volume(drive);
        }
        if !self.folder_sources.read().unwrap().contains_key(&source_id) {
            return Err(format!("source {} is not indexed", source_id.0));
        }
        self.start_folder_scan(source_id).map(|_| 0)
    }

    pub fn rescan_folder_sources(&self) -> usize {
        let ids: Vec<SourceId> = self
            .folder_sources
            .read()
            .unwrap()
            .keys()
            .copied()
            .collect();
        ids.into_iter()
            .filter(|&id| self.start_folder_scan(id).is_ok())
            .count()
    }

    pub fn get_sources(&self) -> Vec<SourceStatusSummary> {
        let excluded_drives = self.config.read().unwrap().excluded_drives.clone();
        let volumes = self.volumes.read().unwrap();
        let folders = self.folder_sources.read().unwrap();
        let mut summaries: Vec<SourceStatusSummary> = volumes
            .values()
            .filter(|vol| {
                !excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&vol.drive_letter))
            })
            .map(volume_index::VolumeIndex::to_source_summary)
            .collect();
        summaries.extend(
            folders
                .values()
                .map(folder_source::FolderSource::to_summary),
        );
        summaries.sort_by_key(|s| s.source_id.0);
        summaries
    }

    /// Whether `source_id` names an indexed source a query may address. A folder source that is
    /// still scanning is known but has no rows yet; the query answers empty and the shell refreshes
    /// when the scan completes. Only an unknown or excluded source is refused.
    pub fn is_known_source(&self, source_id: SourceId) -> bool {
        if let Some(drive) = source_id.drive_letter() {
            let excluded_drives = self.config.read().unwrap().excluded_drives.clone();
            return self.volumes.read().unwrap().get(&drive).is_some_and(|_| {
                !excluded_drives
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(&drive))
            });
        }
        self.folder_sources.read().unwrap().contains_key(&source_id)
    }

    pub fn get_index_journal(&self, after_seq: u64, limit: usize) -> Vec<IndexJournalEntry> {
        self.index_journal.lock().unwrap().query(after_seq, limit)
    }

    pub fn index_journal_status(&self) -> IndexJournalStatus {
        self.index_journal.lock().unwrap().status()
    }

    pub fn set_index_journal_enabled_persisted(
        &self,
        enabled: bool,
        persist: bool,
    ) -> Result<IndexJournalStatus, String> {
        self.apply_config_change(persist, |current| {
            let mut next = current.clone();
            next.index_journal_enabled = enabled;
            Ok(next)
        })?;
        Ok(self.index_journal_status())
    }

    /// Aggregates resident heap allocations across all indexed sources and side tables.
    pub fn memory_footprint(&self) -> IndexMemoryFootprint {
        let mut total = IndexMemoryFootprint::default();
        {
            let lock = self.volumes.read().unwrap();
            for vol in lock.values() {
                total.accumulate(&vol.memory_footprint());
            }
        }
        {
            let folders = self.folder_sources.read().unwrap();
            for folder in folders.values() {
                total.accumulate(&folder.memory_footprint());
            }
        }
        total.property_store_bytes += self.property_pipeline.allocated_bytes();
        total.scratch_bytes += self.usn_read_buf.lock().unwrap().capacity();
        total.scratch_bytes += self.index_journal.lock().unwrap().status().resident_bytes;
        total
    }
}

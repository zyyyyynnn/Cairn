// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory-backed index source (`IDX-11`) for non-NTFS paths and arbitrary watched folders.
//!
//! Uses an iterative directory tree scan with symlink/junction loop protection and a bounded
//! Win32 `ReadDirectoryChangesW` watcher that falls back cleanly to a full rescan on overflow.

use super::index_journal::IndexJournal;
use super::property_refresh::PropertyRefreshHandles;
use super::volume_status::VolumeStatus;
use crate::exclusion::{ExclusionPath, ExclusionRules};
use crate::filetime::system_time_to_filetime;
use cairn_core::arena::NamePool;
use cairn_core::frn_index::{hashmap_table_bytes, FrnLookup, IndexMemoryFootprint};
use cairn_core::model::{flags_from_win32_attributes, FileRecord, FLAG_REPARSE_POINT};
use cairn_core::path_cache::{normalize_root, DirectoryPathCache};
use cairn_core::property_store::{PropertySideTables, SourceId};
use cairn_core::query::SourceIndex;
use cairn_core::sort::PresortedIndices;
use cairn_protocol::status::{JournalChangeKind, SourceKind, SourceStatusSummary, VolumeState};
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

/// Maximum change events absorbed incrementally before falling back to a full folder rescan.
pub const FOLDER_CHANGE_BUFFER_CAP: usize = 4_096;

/// Change events produced by a filesystem directory watcher or supplied directly in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderWatchEvent {
    Added(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
    Renamed {
        old_path: PathBuf,
        new_path: PathBuf,
    },
    Overflow,
    /// The OS watcher could not continue. The source falls back to snapshot semantics.
    WatchFailed,
}

/// Result of applying one watcher batch: whether the source needs a full rescan, and which record
/// keys changed so the property pipeline can refresh exactly those records.
pub struct FolderApplyOutcome {
    pub needs_rescan: bool,
    pub changed_records: Vec<u64>,
}

/// Normalizes the path syntax without touching the filesystem. Used to keep an offline source in
/// the configuration when its root is temporarily unreachable.
pub(super) fn normalize_folder_root_syntax(path: &Path) -> Result<String, String> {
    let raw = path.to_string_lossy().replace('/', "\\");
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("folder source path cannot be empty".to_string());
    }
    let stripped = trimmed.strip_prefix(r"\\?\").unwrap_or(trimmed);
    if stripped.starts_with(r"\\") {
        return Err("folder source must be a local drive path".to_string());
    }
    let bytes = stripped.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err("folder source must be an absolute drive path".to_string());
    }
    if bytes.len() > 2 && bytes[2] != b'\\' {
        return Err("folder source must be an absolute drive path".to_string());
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let mut candidate = format!("{drive}:{}", &stripped[2..]);
    while candidate.ends_with('\\') && candidate.len() > 2 {
        candidate.pop();
    }
    if candidate.len() == 2 {
        candidate.push('\\');
    }
    let rest = if candidate.len() > 3 {
        &candidate[3..]
    } else {
        ""
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in rest.split('\\').filter(|segment| !segment.is_empty()) {
        match segment {
            "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut folded = format!("{drive}:\\");
    if !segments.is_empty() {
        folded.push_str(&segments.join("\\"));
    }
    Ok(folded)
}

/// Normalizes and validates a folder source root.
///
/// Only local drive paths are accepted: UNC shares, device paths, relative paths and reparse-point
/// roots are refused. The returned path is canonical, so `..`, short names and mixed separators
/// resolve to one identity.
pub fn normalize_folder_root(path: &Path) -> Result<(PathBuf, String), String> {
    let candidate = normalize_folder_root_syntax(path)?;

    let probe = Path::new(&candidate);
    let drive = candidate
        .chars()
        .next()
        .map(|ch| ch.to_ascii_uppercase())
        .unwrap_or('C');
    let root_meta = fs::symlink_metadata(probe)
        .map_err(|err| format!("folder source {} is unreachable: {err}", probe.display()))?;
    if !root_meta.is_dir() {
        return Err(format!(
            "folder source {} is not a directory",
            probe.display()
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if root_meta.file_attributes() & 0x400 != 0 {
            return Err(format!(
                "folder source {} is a reparse point",
                probe.display()
            ));
        }
        ensure_local_drive(drive)?;
    }

    let canonical = fs::canonicalize(probe)
        .map_err(|err| format!("folder source {} is unreachable: {err}", probe.display()))?;
    let canonical = canonical.to_string_lossy();
    let canonical = canonical.strip_prefix(r"\\?\").unwrap_or(&canonical);
    let mut display = canonical.trim_end_matches('\\').to_string();
    if display.len() == 2 {
        display.push('\\');
    }
    Ok((PathBuf::from(&display), display))
}

#[cfg(windows)]
fn ensure_local_drive(drive: char) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;

    // GetDriveTypeW returns these documented values; they live outside the windows-rs
    // FileSystem module surface used here.
    const DRIVE_UNKNOWN: u32 = 0;
    const DRIVE_NO_ROOT_DIR: u32 = 1;
    const DRIVE_REMOTE: u32 = 4;

    let root = [drive as u16, b':' as u16, b'\\' as u16, 0];
    // SAFETY: `root` is a null-terminated "X:\" drive root.
    let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if kind == DRIVE_REMOTE || kind == DRIVE_UNKNOWN || kind == DRIVE_NO_ROOT_DIR {
        return Err(format!("folder source drive {drive}: is not a local disk"));
    }
    Ok(())
}

/// Stable per-source record key derived from the lower-case relative path.
///
/// Folder sources have no NTFS file reference number. A path-derived key keeps a file's identity
/// stable across rescans so side tables never attach an old file's properties to a new one.
fn stable_record_key(rel_key: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in rel_key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if hash == 0 {
        1
    } else {
        hash
    }
}

fn next_unique_key(rel_key: &str, lookup: &FrnLookup) -> u64 {
    let mut key = stable_record_key(rel_key);
    while lookup.get(key).is_some() {
        key = key.wrapping_add(0x9E37_79B9_7F4A_7C15);
        if key == 0 {
            key = 1;
        }
    }
    key
}

#[cfg(windows)]
pub(crate) fn metadata_win32_attributes(meta: &Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
pub(crate) fn metadata_win32_attributes(meta: &Metadata) -> u32 {
    let mut attrs = 0u32;
    if meta.is_dir() {
        attrs |= cairn_core::WIN32_ATTR_DIRECTORY;
    }
    if meta.file_type().is_symlink() {
        attrs |= cairn_core::WIN32_ATTR_REPARSE_POINT;
    }
    if meta.permissions().readonly() {
        attrs |= cairn_core::WIN32_ATTR_READONLY;
    }
    attrs
}

fn is_reparse_or_symlink(meta: &Metadata, flags: u16) -> bool {
    meta.file_type().is_symlink() || (flags & FLAG_REPARSE_POINT) != 0
}

fn metadata_record_fields(meta: &Metadata) -> (u16, bool, bool, u64, i64, i64) {
    let attrs = metadata_win32_attributes(meta);
    let mut flags = flags_from_win32_attributes(attrs);
    let is_dir = meta.is_dir();
    if is_dir {
        flags |= cairn_core::FLAG_DIRECTORY;
    }
    let is_reparse = is_reparse_or_symlink(meta, flags);
    if is_reparse {
        flags |= FLAG_REPARSE_POINT;
    }
    let size = if is_dir { 0 } else { meta.len() };
    let created_ts = system_time_to_filetime(meta.created());
    let modified_ts = system_time_to_filetime(meta.modified());
    (flags, is_dir, is_reparse, size, created_ts, modified_ts)
}

/// Directory-backed file index source matching NTFS `VolumeIndex` query/sort/row interfaces.
pub struct FolderSource {
    pub source_id: SourceId,
    pub root_path: PathBuf,
    pub root_prefix: Arc<str>,
    pub(super) status: VolumeStatus,
    records: Vec<FileRecord>,
    arena: NamePool,
    path_cache: DirectoryPathCache,
    presorted: PresortedIndices,
    frn_to_idx: FrnLookup,
    path_to_idx: HashMap<String, u32>,
    excluded_paths: Vec<String>,
    exclusion_rules: ExclusionRules,
    live_watch_enabled: bool,
    scan_pending: bool,
    pending_events: Vec<FolderWatchEvent>,
    pending_overflow: bool,
    overflow_retry_used: bool,
    last_skipped: u64,
    generation: u64,
    live_count: u64,
    has_tombstones: bool,
    volume_id_tag: u16,
}

impl FolderSource {
    pub fn new(source_id: SourceId, root: &Path) -> Result<Self, String> {
        let (normalized_path, display) = normalize_folder_root(root)?;
        let root_prefix = normalize_root(&display);
        let volume_id_tag = cairn_core::path_cache::drive_letter_prefix(&display)
            .and_then(FileRecord::volume_id_from_drive)
            .unwrap_or(0);
        Ok(Self {
            source_id,
            root_path: normalized_path,
            root_prefix,
            status: VolumeStatus::rebuilding(),
            records: Vec::new(),
            arena: NamePool::default(),
            path_cache: DirectoryPathCache::new(),
            presorted: PresortedIndices::default(),
            frn_to_idx: FrnLookup::new(),
            path_to_idx: HashMap::new(),
            excluded_paths: Vec::new(),
            exclusion_rules: ExclusionRules::default(),
            live_watch_enabled: false,
            scan_pending: false,
            pending_events: Vec::new(),
            pending_overflow: false,
            overflow_retry_used: false,
            last_skipped: 0,
            generation: 1,
            live_count: 0,
            has_tombstones: false,
            volume_id_tag,
        })
    }

    pub fn display_root(&self) -> &str {
        let s = self.root_prefix.as_ref();
        if s.len() > 3 {
            s.trim_end_matches('\\')
        } else {
            s
        }
    }

    pub fn records(&self) -> &[FileRecord] {
        &self.records
    }

    pub fn arena(&self) -> &NamePool {
        &self.arena
    }

    pub fn path_cache(&self) -> &DirectoryPathCache {
        &self.path_cache
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn live_count(&self) -> u64 {
        self.live_count
    }

    pub(crate) fn live_watch_enabled(&self) -> bool {
        self.live_watch_enabled
    }

    pub fn is_active(&self) -> bool {
        self.status.state().is_synced() || !self.records.is_empty()
    }

    #[inline]
    pub fn record_indices_for_frn(&self, frn: u64) -> Option<&[u32]> {
        self.frn_to_idx.get(frn)
    }

    pub(crate) fn calculate_matched_bytes(&self, matched_indices: &[u32]) -> u64 {
        super::volume_index::calculate_matched_bytes_for_records(&self.records, matched_indices)
    }

    pub fn as_source_index<'a>(&'a self, properties: &'a PropertySideTables) -> SourceIndex<'a> {
        SourceIndex {
            source_id: self.source_id,
            root_prefix: &self.root_prefix,
            records: &self.records,
            arena: &self.arena,
            path_cache: &self.path_cache,
            presorted: None,
            properties,
            has_tombstones: self.has_tombstones,
        }
    }

    pub fn to_summary(&self) -> SourceStatusSummary {
        SourceStatusSummary {
            source_id: self.source_id,
            kind: SourceKind::Folder,
            root_path: self.display_root().to_string(),
            volume_guid: String::new(),
            status: self.status.state(),
            indexed_count: self.live_count,
            live_watch_enabled: self.live_watch_enabled,
        }
    }

    pub(crate) fn set_live_watch_enabled(&mut self, enabled: bool) {
        self.live_watch_enabled = enabled;
        if enabled {
            if self.status.state() == VolumeState::SnapshotMode {
                self.status.mark_folder_live();
            }
        } else if self.status.state() == VolumeState::Synced {
            self.status.set(VolumeState::SnapshotMode);
        }
    }

    pub(super) fn begin_scan(&mut self) {
        self.scan_pending = true;
        self.pending_events.clear();
        self.pending_overflow = false;
        self.status.set(VolumeState::Rebuilding);
    }

    pub(super) fn take_pending_events(&mut self) -> (Vec<FolderWatchEvent>, bool) {
        self.scan_pending = false;
        (
            std::mem::take(&mut self.pending_events),
            std::mem::replace(&mut self.pending_overflow, false),
        )
    }

    pub(super) fn last_skipped(&self) -> u64 {
        self.last_skipped
    }

    pub fn set_excluded_paths(&mut self, excluded_paths: &[String]) {
        self.excluded_paths = excluded_paths.to_vec();
        self.exclusion_rules = ExclusionRules::parse(excluded_paths);
    }

    pub fn excluded_paths(&self) -> &[String] {
        &self.excluded_paths
    }

    pub fn memory_footprint(&self) -> IndexMemoryFootprint {
        let (arena_bytes, folded_arena_bytes) = self.arena.allocated_bytes();
        let path_map_bytes = hashmap_table_bytes::<String, u32>(self.path_to_idx.capacity())
            + self.path_to_idx.keys().map(String::capacity).sum::<usize>();
        IndexMemoryFootprint {
            record_count: self.records.len(),
            live_count: self.live_count as usize,
            records_bytes: self.records.capacity() * size_of::<FileRecord>(),
            arena_bytes,
            folded_arena_bytes,
            frn_lookup_bytes: self.frn_to_idx.allocated_bytes(),
            presorted_bytes: self.presorted.allocated_bytes(),
            property_store_bytes: 0,
            directory_sizes_bytes: 0,
            path_cache_bytes: self.path_cache.allocated_bytes() + path_map_bytes,
            scratch_bytes: 0,
        }
    }

    /// Rebuilds the folder tree index using an iterative filesystem walk, skipping junctions/symlinks
    /// to avoid cycles and marking the source `Offline` when the root is unreachable.
    pub fn rescan(&mut self, excluded_paths: &[String]) -> Result<usize, String> {
        self.set_excluded_paths(excluded_paths);
        let root_meta = match fs::symlink_metadata(&self.root_path) {
            Ok(meta) => meta,
            Err(err) => {
                self.status.set(VolumeState::Offline);
                return Err(format!(
                    "folder source {} is unreachable: {err}",
                    self.root_path.display()
                ));
            }
        };
        if !root_meta.is_dir() {
            self.status.set(VolumeState::Offline);
            return Err(format!(
                "folder source {} is not a directory",
                self.root_path.display()
            ));
        }
        #[cfg(windows)]
        if metadata_win32_attributes(&root_meta) & 0x400 != 0 {
            self.status.set(VolumeState::Offline);
            return Err(format!(
                "folder source {} is a reparse point",
                self.root_path.display()
            ));
        }

        self.status.set(VolumeState::Rebuilding);
        let mut records = Vec::new();
        let mut arena = NamePool::default();
        let mut frn_to_idx = FrnLookup::new();
        let mut path_to_idx = HashMap::new();
        let mut live_count = 0u64;
        let mut skipped = 0u64;

        let mut stack: Vec<(PathBuf, u32, String)> = vec![(
            self.root_path.clone(),
            FileRecord::ROOT_PARENT,
            String::new(),
        )];
        let mut visited_dirs: HashSet<PathBuf> = HashSet::new();

        while let Some((dir_path, parent_idx, rel_dir)) = stack.pop() {
            if !visited_dirs.insert(dir_path.clone()) {
                continue;
            }
            let Ok(read_dir) = fs::read_dir(&dir_path) else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            for entry in read_dir {
                let Ok(entry) = entry else {
                    skipped = skipped.saturating_add(1);
                    continue;
                };
                let entry_path = entry.path();
                if self
                    .exclusion_rules
                    .excludes(&ExclusionPath::new(&entry_path.to_string_lossy()))
                {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(meta) = fs::symlink_metadata(&entry_path) else {
                    skipped = skipped.saturating_add(1);
                    continue;
                };
                let (flags, is_dir, is_reparse, size, created_ts, modified_ts) =
                    metadata_record_fields(&meta);
                let rel_key = if rel_dir.is_empty() {
                    name.to_ascii_lowercase()
                } else {
                    format!("{rel_dir}\\{}", name.to_ascii_lowercase())
                };

                let idx = records.len() as u32;
                let key = next_unique_key(&rel_key, &frn_to_idx);
                let (name_offset, name_len, ext_offset) = arena.insert(&name);
                records.push(FileRecord {
                    frn: key,
                    parent_idx,
                    name_offset,
                    name_len,
                    ext_offset,
                    volume_id: self.volume_id_tag,
                    flags,
                    size,
                    modified_ts,
                    created_ts,
                });
                frn_to_idx.push(key, idx);
                path_to_idx.insert(rel_key.clone(), idx);
                live_count = live_count.saturating_add(1);
                if is_dir && !is_reparse {
                    stack.push((entry_path, idx, rel_key));
                }
            }
        }

        // Because every parent directory is pushed before scanning its children, parent_idx < idx holds.
        for idx in (0..records.len()).rev() {
            let parent_idx = records[idx].parent_idx;
            if parent_idx != FileRecord::ROOT_PARENT && (parent_idx as usize) < idx {
                records[parent_idx as usize].size = records[parent_idx as usize]
                    .size
                    .saturating_add(records[idx].size);
            }
        }

        self.records = records;
        self.arena = arena;
        self.frn_to_idx = frn_to_idx;
        self.path_to_idx = path_to_idx;
        self.path_cache.clear();
        self.presorted.clear_and_shrink();
        self.live_count = live_count;
        self.has_tombstones = false;
        self.generation = self.generation.wrapping_add(1);
        self.last_skipped = skipped;
        self.status.mark_folder_scanned(self.live_watch_enabled);
        if skipped > 0 {
            self.status.set(VolumeState::SnapshotMode);
        }
        Ok(self.records.len())
    }

    fn relative_key(&self, path: &Path) -> Option<(String, String)> {
        let rel = path.strip_prefix(&self.root_path).ok()?;
        let rel_str = rel.to_string_lossy().replace('/', "\\");
        let trimmed = rel_str.trim_matches('\\');
        if trimmed.is_empty() {
            return None;
        }
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())?;
        Some((trimmed.to_ascii_lowercase(), name))
    }

    fn parent_idx_for_path(&self, path: &Path) -> Option<u32> {
        let parent = path.parent()?;
        if parent == self.root_path {
            return Some(FileRecord::ROOT_PARENT);
        }
        let (rel_parent, _) = self.relative_key(parent)?;
        self.path_to_idx.get(&rel_parent).copied()
    }

    fn adjust_ancestors(&mut self, mut parent_idx: u32, delta: i128) {
        if delta == 0 {
            return;
        }
        let mut steps = 0usize;
        while parent_idx != FileRecord::ROOT_PARENT && (parent_idx as usize) < self.records.len() {
            steps += 1;
            if steps > self.records.len() {
                break;
            }
            let rec = &mut self.records[parent_idx as usize];
            if rec.is_tombstone() {
                break;
            }
            rec.size = if delta >= 0 {
                rec.size.saturating_add(delta as u64)
            } else {
                rec.size.saturating_sub((-delta) as u64)
            };
            parent_idx = rec.parent_idx;
        }
    }

    fn tombstone_subtree(&mut self, rel_key: &str, removed: &mut Vec<u64>) -> Option<u64> {
        let prefix = format!("{rel_key}\\");
        let mut targets: Vec<(String, u32)> = self
            .path_to_idx
            .iter()
            .filter(|(key, _)| key.as_str() == rel_key || key.starts_with(&prefix))
            .map(|(k, &idx)| (k.clone(), idx))
            .collect();
        targets.sort_by_key(|entry| std::cmp::Reverse(entry.1));

        let mut primary_frn = None;
        for (key, idx) in targets {
            self.path_to_idx.remove(&key);
            let Some(rec) = self.records.get(idx as usize).copied() else {
                continue;
            };
            if rec.is_tombstone() {
                continue;
            }
            if primary_frn.is_none() || key == rel_key {
                primary_frn = Some(rec.frn);
            }
            removed.push(rec.frn);
            if !rec.is_dir() {
                self.adjust_ancestors(rec.parent_idx, -(rec.size as i128));
            }
            self.records[idx as usize].mark_tombstone();
            self.live_count = self.live_count.saturating_sub(1);
            self.has_tombstones = true;
        }
        primary_frn
    }

    fn upsert_live_path(
        &mut self,
        path: &Path,
        removed: &mut Vec<u64>,
    ) -> Option<(u64, JournalChangeKind)> {
        let (rel_key, name) = self.relative_key(path)?;
        if self
            .exclusion_rules
            .excludes(&ExclusionPath::new(&path.to_string_lossy()))
        {
            return None;
        }
        let meta = fs::symlink_metadata(path).ok()?;
        let (flags, is_dir, _is_reparse, size, created_ts, modified_ts) =
            metadata_record_fields(&meta);

        if let Some(&idx) = self.path_to_idx.get(&rel_key) {
            let rec = &self.records[idx as usize];
            if !rec.is_tombstone() {
                let was_dir = rec.is_dir();
                let old_size = rec.size;
                let parent_idx = rec.parent_idx;
                let frn = rec.frn;
                if was_dir != is_dir {
                    self.tombstone_subtree(&rel_key, removed);
                } else {
                    let rec = &mut self.records[idx as usize];
                    rec.flags = flags;
                    rec.created_ts = created_ts;
                    rec.modified_ts = modified_ts;
                    if !was_dir {
                        rec.size = size;
                        let delta = size as i128 - old_size as i128;
                        self.adjust_ancestors(parent_idx, delta);
                    }
                    return Some((frn, JournalChangeKind::Modified));
                }
            }
        }

        let parent_idx = self.parent_idx_for_path(path)?;
        let (name_offset, name_len, ext_offset) = self.arena.insert(&name);
        let idx = self.records.len() as u32;
        let key = next_unique_key(&rel_key, &self.frn_to_idx);
        self.records.push(FileRecord {
            frn: key,
            parent_idx,
            name_offset,
            name_len,
            ext_offset,
            volume_id: self.volume_id_tag,
            flags,
            size,
            modified_ts,
            created_ts,
        });
        self.frn_to_idx.push(key, idx);
        self.path_to_idx.insert(rel_key, idx);
        self.live_count = self.live_count.saturating_add(1);
        if !is_dir {
            self.adjust_ancestors(parent_idx, size as i128);
        }
        Some((key, JournalChangeKind::Created))
    }

    /// Applies a bounded batch of change events. Returns the rescan requirement and the record keys
    /// touched by the batch so callers can refresh derived indexes for exactly those records.
    pub fn apply_folder_events(
        &mut self,
        events: &[FolderWatchEvent],
        mut journal: Option<&mut IndexJournal>,
        now_unix: i64,
    ) -> FolderApplyOutcome {
        if self.scan_pending {
            if events.len() + self.pending_events.len() > FOLDER_CHANGE_BUFFER_CAP
                || events.iter().any(|event| {
                    matches!(
                        event,
                        FolderWatchEvent::Overflow | FolderWatchEvent::WatchFailed
                    )
                })
            {
                self.pending_overflow = true;
                self.pending_events.clear();
            } else {
                self.pending_events.extend_from_slice(events);
            }
            return FolderApplyOutcome {
                needs_rescan: false,
                changed_records: Vec::new(),
            };
        }
        if events.len() > FOLDER_CHANGE_BUFFER_CAP
            || events.iter().any(|e| {
                matches!(
                    e,
                    FolderWatchEvent::Overflow | FolderWatchEvent::WatchFailed
                )
            })
        {
            if let Some(ref mut j) = journal {
                j.push(
                    self.source_id,
                    0,
                    JournalChangeKind::OverflowRescan,
                    self.display_root().to_string(),
                    None,
                    now_unix,
                );
            }
            return FolderApplyOutcome {
                needs_rescan: true,
                changed_records: Vec::new(),
            };
        }
        let mut needs_rescan = false;
        let mut table_changed = false;
        let mut changed_records = Vec::new();

        for event in events {
            match event {
                FolderWatchEvent::Overflow | FolderWatchEvent::WatchFailed => {
                    return FolderApplyOutcome {
                        needs_rescan: true,
                        changed_records,
                    };
                }
                FolderWatchEvent::Added(path) | FolderWatchEvent::Modified(path) => {
                    if let Some((frn, kind)) = self.upsert_live_path(path, &mut changed_records) {
                        table_changed = true;
                        changed_records.push(frn);
                        if let Some(ref mut j) = journal {
                            j.push(
                                self.source_id,
                                frn,
                                kind,
                                path.to_string_lossy().into_owned(),
                                None,
                                now_unix,
                            );
                        }
                    } else if self.relative_key(path).is_some()
                        && path.exists()
                        && !self
                            .exclusion_rules
                            .excludes(&ExclusionPath::new(&path.to_string_lossy()))
                    {
                        // A nested directory was created before our table saw it; trigger a quick rescan.
                        needs_rescan = true;
                    }
                }
                FolderWatchEvent::Removed(path) => {
                    if let Some((rel_key, _)) = self.relative_key(path) {
                        let frn = self
                            .tombstone_subtree(&rel_key, &mut changed_records)
                            .unwrap_or(0);
                        table_changed = true;
                        if let Some(ref mut j) = journal {
                            j.push(
                                self.source_id,
                                frn,
                                JournalChangeKind::Deleted,
                                path.to_string_lossy().into_owned(),
                                None,
                                now_unix,
                            );
                        }
                    }
                }
                FolderWatchEvent::Renamed { old_path, new_path } => {
                    let mut moved_frn = 0;
                    if let Some((old_key, _)) = self.relative_key(old_path) {
                        if let Some(&idx) = self.path_to_idx.get(&old_key) {
                            if self.records[idx as usize].is_dir() {
                                needs_rescan = true;
                            }
                        }
                        moved_frn = self
                            .tombstone_subtree(&old_key, &mut changed_records)
                            .unwrap_or(0);
                    }
                    if let Some((frn, _)) = self.upsert_live_path(new_path, &mut changed_records) {
                        moved_frn = frn;
                        changed_records.push(frn);
                        if self
                            .frn_to_idx
                            .first(frn)
                            .is_some_and(|idx| self.records[idx as usize].is_dir())
                        {
                            needs_rescan = true;
                        }
                    }
                    table_changed = true;
                    if let Some(ref mut j) = journal {
                        j.push(
                            self.source_id,
                            moved_frn,
                            JournalChangeKind::Renamed,
                            new_path.to_string_lossy().into_owned(),
                            Some(old_path.to_string_lossy().into_owned()),
                            now_unix,
                        );
                    }
                }
            }
        }

        if table_changed {
            self.path_cache.clear();
        }
        FolderApplyOutcome {
            needs_rescan,
            changed_records,
        }
    }

    pub(crate) fn table_view(&self) -> super::SourceTableView<'_> {
        super::SourceTableView {
            root_prefix: self.root_prefix.clone(),
            records: &self.records,
            arena: &self.arena,
            path_cache: &self.path_cache,
            generation: self.generation,
        }
    }
}

struct ScanGuard {
    scanning: Arc<Mutex<HashSet<SourceId>>>,
    source_id: SourceId,
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.scanning.lock().unwrap().remove(&self.source_id);
    }
}

/// Reserves the single in-flight scan slot for `source_id`; the caller owns releasing it through a
/// guard or [`release_scan`].
pub(super) fn reserve_scan(scanning: &Arc<Mutex<HashSet<SourceId>>>, source_id: SourceId) -> bool {
    scanning.lock().unwrap().insert(source_id)
}

pub(super) fn release_scan(scanning: &Arc<Mutex<HashSet<SourceId>>>, source_id: SourceId) {
    scanning.lock().unwrap().remove(&source_id);
}

/// Shared engine handles needed to scan, reinstall, and notify property refresh for a folder source.
#[derive(Clone)]
pub(super) struct FolderScanContext {
    pub sources: Arc<RwLock<HashMap<SourceId, FolderSource>>>,
    pub property_handles: PropertyRefreshHandles,
    pub scanning: Arc<Mutex<HashSet<SourceId>>>,
    pub journal: Arc<Mutex<IndexJournal>>,
}

impl FolderScanContext {
    pub(super) fn reserve_and_spawn_scan(&self, source_id: SourceId, now_unix: i64) -> bool {
        if !reserve_scan(&self.scanning, source_id) {
            return false;
        }
        {
            let mut folders = self.sources.write().unwrap();
            if let Some(source) = folders.get_mut(&source_id) {
                source.begin_scan();
            }
        }
        self.spawn_reserved_scan(source_id, now_unix)
    }

    /// Scans one folder source on a background thread and installs the result under a short write lock.
    ///
    /// The caller reserves the scan slot and begins the scan (buffering watcher events) before arming
    /// the watcher, so events that happen during the traversal are replayed onto the installed snapshot.
    pub(super) fn spawn_reserved_scan(&self, source_id: SourceId, now_unix: i64) -> bool {
        let prepared = {
            let folders = self.sources.read().unwrap();
            let Some(source) = folders.get(&source_id) else {
                release_scan(&self.scanning, source_id);
                return false;
            };
            (source.root_path.clone(), source.excluded_paths().to_vec())
        };
        let (root_path, excluded) = prepared;
        let thread_ctx = self.clone();
        let spawned = std::thread::Builder::new()
            .name("cairn-folder-scan".to_string())
            .spawn(move || {
                let scan_guard = ScanGuard {
                    scanning: Arc::clone(&thread_ctx.scanning),
                    source_id,
                };
                let scanned = FolderSource::new(source_id, &root_path).and_then(|mut source| {
                    source.rescan(&excluded)?;
                    Ok(source)
                });
                let needs_rescan = match scanned {
                    Ok(scanned) => install_scanned_source(
                        &thread_ctx.sources,
                        &thread_ctx.property_handles,
                        &thread_ctx.journal,
                        source_id,
                        scanned,
                        now_unix,
                    ),
                    Err(err) => {
                        tracing::warn!(
                            source = source_id.0,
                            error = %super::log_safe(&err),
                            "folder scan failed"
                        );
                        let mut folders = thread_ctx.sources.write().unwrap();
                        if let Some(source) = folders.get_mut(&source_id) {
                            source.scan_pending = false;
                            source.set_live_watch_enabled(false);
                            source.status.set(VolumeState::Offline);
                        }
                        false
                    }
                };
                drop(scan_guard);
                if needs_rescan {
                    thread_ctx.reserve_and_spawn_scan(source_id, now_unix);
                }
            });
        if spawned.is_err() {
            release_scan(&self.scanning, source_id);
            let mut folders = self.sources.write().unwrap();
            if let Some(source) = folders.get_mut(&source_id) {
                source.scan_pending = false;
                source.status.set(VolumeState::SnapshotMode);
            }
            return false;
        }
        true
    }
}

fn install_scanned_source(
    sources: &Arc<RwLock<HashMap<SourceId, FolderSource>>>,
    property_handles: &PropertyRefreshHandles,
    journal: &Arc<Mutex<IndexJournal>>,
    source_id: SourceId,
    mut scanned: FolderSource,
    now_unix: i64,
) -> bool {
    let (present_keys, needs_follow_up): (HashSet<u64>, bool) = {
        let mut folders = sources.write().unwrap();
        let Some(old) = folders.get_mut(&source_id) else {
            return false;
        };
        let (pending, overflow) = old.take_pending_events();
        let live_excluded = old.excluded_paths.clone();
        let exclusions_changed = scanned.excluded_paths != live_excluded;
        if exclusions_changed {
            scanned.set_excluded_paths(&live_excluded);
        }
        scanned.generation = old.generation.wrapping_add(1);
        scanned.live_watch_enabled = old.live_watch_enabled;
        scanned.overflow_retry_used = old.overflow_retry_used;
        scanned.scan_pending = false;
        let skipped = scanned.last_skipped();
        scanned
            .status
            .mark_folder_scanned(scanned.live_watch_enabled);
        if skipped > 0 {
            scanned.status.set(VolumeState::SnapshotMode);
        }
        folders.insert(source_id, scanned);
        let Some(source) = folders.get_mut(&source_id) else {
            return false;
        };
        let mut needs_rescan = exclusions_changed;
        if !pending.is_empty() {
            let mut journal_lock = journal.lock().unwrap();
            needs_rescan |= source
                .apply_folder_events(&pending, Some(&mut journal_lock), now_unix)
                .needs_rescan;
        }
        let mut retry_overflow = false;
        if overflow {
            if source.overflow_retry_used {
                source.overflow_retry_used = false;
                tracing::warn!(
                    source = source_id.0,
                    "folder scan overflowed again; waiting for the next scheduled rescan"
                );
            } else {
                source.overflow_retry_used = true;
                retry_overflow = true;
                let mut journal_lock = journal.lock().unwrap();
                journal_lock.push(
                    source_id,
                    0,
                    JournalChangeKind::OverflowRescan,
                    source.display_root().to_string(),
                    None,
                    now_unix,
                );
            }
        } else {
            source.overflow_retry_used = false;
        }
        if overflow || needs_rescan || skipped > 0 {
            source.status.set(VolumeState::SnapshotMode);
        }
        if skipped > 0 {
            tracing::warn!(
                source = source_id.0,
                skipped,
                "folder scan skipped unreadable entries; source is snapshot-only"
            );
        }
        let present_keys = source
            .records()
            .iter()
            .filter(|record| !record.is_tombstone())
            .map(|record| record.frn)
            .collect();
        (present_keys, needs_rescan || retry_overflow)
    };
    property_handles.retain_and_enqueue_source(source_id, present_keys);
    needs_follow_up
}

/// Active OS directory watcher handle whose drop cleanly cancels and joins its worker thread.
pub(crate) struct FolderWatcherHandle {
    #[cfg(windows)]
    stop_event: usize,
    join: Option<JoinHandle<()>>,
}

impl FolderWatcherHandle {
    pub(crate) fn is_finished(&self) -> bool {
        self.join
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
    }
}

impl Drop for FolderWatcherHandle {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.stop_event != 0 {
            use windows::Win32::Foundation::{CloseHandle, HANDLE};
            use windows::Win32::System::Threading::SetEvent;
            // SAFETY: stop_event is an owned Win32 event handle created by CreateEventW.
            unsafe {
                let handle = HANDLE(self.stop_event as *mut core::ffi::c_void);
                let _ = SetEvent(handle);
                if let Some(join) = self.join.take() {
                    let _ = join.join();
                }
                let _ = CloseHandle(handle);
            }
            self.stop_event = 0;
            return;
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(windows)]
pub(crate) fn spawn_folder_watcher(
    root_path: PathBuf,
    on_events: impl Fn(Vec<FolderWatchEvent>) + Send + 'static,
) -> Option<FolderWatcherHandle> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_EVENT, WAIT_OBJECT_0,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
        FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    let wide_dir: Vec<u16> = root_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: wide_dir is null-terminated; flags request async ReadDirectoryChangesW handle.
    let dir_handle = unsafe {
        CreateFileW(
            PCWSTR(wide_dir.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .ok()?;
    if dir_handle == INVALID_HANDLE_VALUE || dir_handle.is_invalid() {
        return None;
    }

    // SAFETY: manual-reset Win32 event closed on FolderWatcherHandle::drop.
    let stop_event = match unsafe { CreateEventW(None, true, false, None) } {
        Ok(event) => event,
        Err(_) => {
            // SAFETY: dir_handle was opened above and is no longer used.
            unsafe {
                let _ = CloseHandle(dir_handle);
            }
            return None;
        }
    };
    // SAFETY: auto-reset overlapped IO completion event owned by watcher thread.
    let io_event = match unsafe { CreateEventW(None, false, false, None) } {
        Ok(event) => event,
        Err(_) => {
            // SAFETY: both handles were opened above and are no longer used.
            unsafe {
                let _ = CloseHandle(stop_event);
                let _ = CloseHandle(dir_handle);
            }
            return None;
        }
    };

    let dir_raw = dir_handle.0 as usize;
    let stop_raw = stop_event.0 as usize;
    let io_raw = io_event.0 as usize;
    // Wait until the first ReadDirectoryChangesW call attaches a notify buffer to dir_handle;
    // changes written before that call are never buffered by Windows.
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<bool>(1);

    let join = std::thread::Builder::new()
        .name("cairn-folder-watch".to_string())
        .spawn(move || {
            let dir_h = HANDLE(dir_raw as *mut core::ffi::c_void);
            let stop_h = HANDLE(stop_raw as *mut core::ffi::c_void);
            let io_h = HANDLE(io_raw as *mut core::ffi::c_void);
            let mut ready_tx = Some(ready_tx);
            let mut buffer = vec![0u8; 64 * 1024];
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_ATTRIBUTES
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE
                | FILE_NOTIFY_CHANGE_CREATION;

            loop {
                let mut overlapped = OVERLAPPED {
                    hEvent: io_h,
                    ..Default::default()
                };
                // SAFETY: dir_h and buffer remain valid until CancelIoEx/GetOverlappedResult completes.
                let issued = unsafe {
                    ReadDirectoryChangesW(
                        dir_h,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as u32,
                        true,
                        filter,
                        None,
                        Some(&mut overlapped),
                        None,
                    )
                };
                if issued.is_err() {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(false);
                    }
                    break;
                }
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(true);
                }

                let waits = [stop_h, io_h];
                // SAFETY: Both event handles stay valid until the loop exits before FolderWatcherHandle closes stop_h.
                let wait = unsafe { WaitForMultipleObjects(&waits, false, INFINITE) };
                if wait == WAIT_OBJECT_0 {
                    // SAFETY: `stop_h` signaled stop; cancel pending overlapped I/O on `dir_h` before closing.
                    unsafe {
                        let _ = CancelIoEx(dir_h, Some(&overlapped));
                        let mut cancelled_bytes = 0u32;
                        let _ = GetOverlappedResult(dir_h, &overlapped, &mut cancelled_bytes, true);
                    }
                    break;
                }
                if wait != WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
                    on_events(vec![FolderWatchEvent::WatchFailed]);
                    break;
                }

                let mut bytes_returned = 0u32;
                // SAFETY: overlapped signaled completion on io_h.
                let completed =
                    unsafe { GetOverlappedResult(dir_h, &overlapped, &mut bytes_returned, true) };
                if completed.is_err() {
                    on_events(vec![FolderWatchEvent::WatchFailed]);
                    break;
                }
                if bytes_returned == 0 {
                    on_events(vec![FolderWatchEvent::Overflow]);
                    continue;
                }

                let events = parse_notify_buffer(&root_path, &buffer[..bytes_returned as usize]);
                if !events.is_empty() {
                    on_events(events);
                }
            }

            // SAFETY: Close directory handle and IO completion event owned by this thread.
            unsafe {
                let _ = CloseHandle(io_h);
                let _ = CloseHandle(dir_h);
            }
        })
        .ok();

    let Some(join_handle) = join else {
        // SAFETY: Clean up created handles when thread spawn fails.
        unsafe {
            let _ = CloseHandle(io_event);
            let _ = CloseHandle(stop_event);
            let _ = CloseHandle(dir_handle);
        }
        return None;
    };

    if !ready_rx.recv().unwrap_or(false) {
        // SAFETY: First ReadDirectoryChangesW failed before entering WaitForMultipleObjects.
        unsafe {
            let _ = SetEvent(stop_event);
        }
        let _ = join_handle.join();
        // SAFETY: watcher thread has exited; close `stop_event` owned by startup cleanup.
        unsafe {
            let _ = CloseHandle(stop_event);
        }
        return None;
    }

    Some(FolderWatcherHandle {
        stop_event: stop_raw,
        join: Some(join_handle),
    })
}

#[cfg(windows)]
fn parse_notify_buffer(root: &Path, buf: &[u8]) -> Vec<FolderWatchEvent> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ACTION, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
        FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
    };

    // NextEntryOffset (u32) + Action (u32) + FileNameLength (u32); struct size_of includes
    // trailing alignment padding that exceeds a 1-character (14-byte) notification record.
    const FILE_NOTIFY_HEADER_BYTES: usize = 12;

    let mut events = Vec::new();
    let mut offset = 0usize;
    let mut pending_old: Option<PathBuf> = None;

    while offset + FILE_NOTIFY_HEADER_BYTES <= buf.len() {
        let next_entry_offset =
            u32::from_le_bytes(buf[offset..offset + 4].try_into().expect("4-byte header")) as usize;
        let action = FILE_ACTION(u32::from_le_bytes(
            buf[offset + 4..offset + 8]
                .try_into()
                .expect("4-byte header"),
        ));
        let name_bytes = u32::from_le_bytes(
            buf[offset + 8..offset + 12]
                .try_into()
                .expect("4-byte header"),
        ) as usize;
        let name_offset = offset + FILE_NOTIFY_HEADER_BYTES;
        if name_offset + name_bytes > buf.len() || !name_bytes.is_multiple_of(2) {
            events.push(FolderWatchEvent::Overflow);
            break;
        }
        let u16s: Vec<u16> = buf[name_offset..name_offset + name_bytes]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        let rel = String::from_utf16_lossy(&u16s);
        let full_path = root.join(rel);

        match action {
            FILE_ACTION_ADDED => events.push(FolderWatchEvent::Added(full_path)),
            FILE_ACTION_REMOVED => events.push(FolderWatchEvent::Removed(full_path)),
            FILE_ACTION_MODIFIED => events.push(FolderWatchEvent::Modified(full_path)),
            FILE_ACTION_RENAMED_OLD_NAME => pending_old = Some(full_path),
            FILE_ACTION_RENAMED_NEW_NAME => {
                if let Some(old_path) = pending_old.take() {
                    events.push(FolderWatchEvent::Renamed {
                        old_path,
                        new_path: full_path,
                    });
                } else {
                    events.push(FolderWatchEvent::Added(full_path));
                }
            }
            _ => {}
        }

        if events.len() > FOLDER_CHANGE_BUFFER_CAP {
            return vec![FolderWatchEvent::Overflow];
        }
        if next_entry_offset == 0 {
            break;
        }
        let next = offset + next_entry_offset;
        if next <= offset {
            break;
        }
        offset = next;
    }

    // A rename split across two buffers leaves the old name without its new name here; report the
    // removal now so the next buffer's Added event cannot leave a ghost row behind.
    if let Some(old_path) = pending_old.take() {
        events.push(FolderWatchEvent::Removed(old_path));
    }

    events
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use windows::Win32::Storage::FileSystem::FILE_ACTION_ADDED;

    #[test]
    fn snapshot_until_live_watcher_is_enabled() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"x").unwrap();
        let mut source = FolderSource::new(SourceId(2000), dir.path()).unwrap();
        source.rescan(&[]).unwrap();
        assert_eq!(source.to_summary().status, VolumeState::SnapshotMode);
        source.set_live_watch_enabled(true);
        assert_eq!(source.to_summary().status, VolumeState::Synced);
        source.set_live_watch_enabled(false);
        assert_eq!(source.to_summary().status, VolumeState::SnapshotMode);
    }

    #[test]
    fn syntax_normalization_folds_dot_segments_and_requires_absolute_drive_paths() {
        assert_eq!(
            normalize_folder_root_syntax(Path::new(r"c:\Data\..\Data\.")).unwrap(),
            r"C:\Data"
        );
        assert_eq!(
            normalize_folder_root_syntax(Path::new(r"C:\\a//b/")).unwrap(),
            r"C:\a\b"
        );
        assert_eq!(
            normalize_folder_root_syntax(Path::new(r"\\?\C:\Data")).unwrap(),
            r"C:\Data"
        );
        assert_eq!(
            normalize_folder_root_syntax(Path::new(r"C:\")).unwrap(),
            r"C:\"
        );
        assert!(normalize_folder_root_syntax(Path::new(r"C:foo")).is_err());
        assert!(normalize_folder_root_syntax(Path::new(r"\\server\share")).is_err());
    }

    fn test_property_handles() -> PropertyRefreshHandles {
        PropertyRefreshHandles {
            tables: Arc::new(RwLock::new(PropertySideTables::new(true))),
            refresh: Arc::new(Mutex::new(
                super::super::property_refresh::PropertyRefresh::default(),
            )),
            spawner: Arc::new(Mutex::new(None)),
        }
    }

    /// A scan that overflowed retries once. The retry budget lives on the source across the
    /// reinstall that every completed scan performs; without inheriting it, a repeatedly
    /// overflowing folder would rescan forever.
    #[test]
    fn scan_overflow_retries_once_across_reinstalls() {
        let dir = tempdir().unwrap();
        let id = SourceId(2001);
        let mut old = FolderSource::new(id, dir.path()).unwrap();
        old.scan_pending = true;
        old.apply_folder_events(&[FolderWatchEvent::Overflow], None, 0);
        let sources = Arc::new(RwLock::new(HashMap::from([(id, old)])));
        let property_handles = test_property_handles();
        let journal = Arc::new(Mutex::new(IndexJournal::new(true)));

        let scanned = FolderSource::new(id, dir.path()).unwrap();
        assert!(install_scanned_source(
            &sources,
            &property_handles,
            &journal,
            id,
            scanned,
            1
        ));
        assert!(
            sources
                .read()
                .unwrap()
                .get(&id)
                .unwrap()
                .overflow_retry_used
        );

        {
            let mut folders = sources.write().unwrap();
            let source = folders.get_mut(&id).unwrap();
            source.scan_pending = true;
            source.apply_folder_events(&[FolderWatchEvent::Overflow], None, 0);
        }
        let scanned = FolderSource::new(id, dir.path()).unwrap();
        assert!(!install_scanned_source(
            &sources,
            &property_handles,
            &journal,
            id,
            scanned,
            2
        ));
        assert!(
            !sources
                .read()
                .unwrap()
                .get(&id)
                .unwrap()
                .overflow_retry_used
        );
    }

    /// A scan captures its exclusions before it starts. If the live source gained or lost a rule
    /// while the scan ran, the installed snapshot must adopt the live rules and ask for a follow-up
    /// scan so the records match them.
    #[test]
    fn installing_a_scan_adopts_exclusions_changed_while_it_ran() {
        let dir = tempdir().unwrap();
        let skipped = dir.path().join("skip.txt");
        std::fs::write(&skipped, b"x").unwrap();
        let id = SourceId(2100);
        let mut old = FolderSource::new(id, dir.path()).unwrap();
        old.set_excluded_paths(&[skipped.to_string_lossy().into_owned()]);
        let scanned = FolderSource::new(id, dir.path()).unwrap();
        let sources = Arc::new(RwLock::new(HashMap::from([(id, old)])));
        let property_handles = test_property_handles();
        let journal = Arc::new(Mutex::new(IndexJournal::new(true)));

        assert!(install_scanned_source(
            &sources,
            &property_handles,
            &journal,
            id,
            scanned,
            1
        ));
        let installed = sources.read().unwrap();
        let source = installed.get(&id).unwrap();
        assert!(source
            .exclusion_rules
            .excludes(&ExclusionPath::new(&skipped.to_string_lossy())));
    }

    #[test]
    fn parse_notify_buffer_accepts_single_character_14_byte_record() {
        let mut buf = Vec::with_capacity(14);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&FILE_ACTION_ADDED.0.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&u16::from(b'a').to_le_bytes());

        let events = parse_notify_buffer(Path::new(r"C:\Watched"), &buf);
        assert_eq!(
            events,
            vec![FolderWatchEvent::Added(PathBuf::from(r"C:\Watched\a"))]
        );
    }

    #[test]
    fn directory_rename_into_scope_and_type_replacement_report_property_work() {
        let dir = tempdir().unwrap();
        let mut source = FolderSource::new(SourceId(2200), dir.path()).unwrap();
        let canonical_dir = PathBuf::from(source.display_root());
        let sub = canonical_dir.join("bundle");
        let child = sub.join("track.png");
        fs::create_dir_all(&sub).unwrap();
        fs::write(&child, b"png").unwrap();

        source.rescan(&[]).unwrap();
        let child_frn = source
            .records()
            .iter()
            .find(|r| source.arena().get_name(r.name_offset, r.name_len) == "track.png")
            .unwrap()
            .frn;

        // Replacing directory `bundle` with a regular file must report the tombstoned child FRN.
        fs::remove_dir_all(&sub).unwrap();
        fs::write(&sub, b"now a file").unwrap();
        let replaced = source.apply_folder_events(&[FolderWatchEvent::Added(sub.clone())], None, 1);
        assert!(
            replaced.changed_records.contains(&child_frn),
            "descendant FRN must be emitted for property slot cleanup: {:?}",
            replaced.changed_records
        );

        // Moving an excluded directory into an unexcluded name must ask for a rescan.
        let hidden = canonical_dir.join("ignored_dir");
        let shown = canonical_dir.join("shown_dir");
        fs::create_dir_all(&shown).unwrap();
        fs::write(shown.join("cover.png"), b"png").unwrap();
        source.set_excluded_paths(&["ignored_dir".to_string()]);
        let renamed = source.apply_folder_events(
            &[FolderWatchEvent::Renamed {
                old_path: hidden,
                new_path: shown,
            }],
            None,
            2,
        );
        assert!(renamed.needs_rescan);
    }
}

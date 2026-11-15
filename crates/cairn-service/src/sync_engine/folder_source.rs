// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory-backed index source (`IDX-11`) for non-NTFS paths and arbitrary watched folders.
//!
//! Uses an iterative directory tree scan with symlink/junction loop protection and a bounded
//! Win32 `ReadDirectoryChangesW` watcher that falls back cleanly to a full rescan on overflow.

use super::index_journal::IndexJournal;
use super::volume_status::VolumeStatus;
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
use std::sync::Arc;
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
}

/// Normalizes a folder source root for configuration deduplication and display.
pub fn normalize_folder_root(path: &Path) -> Result<(PathBuf, String), String> {
    let raw = path.to_string_lossy().replace('/', "\\");
    let stripped = raw
        .strip_prefix(r"\\?\")
        .unwrap_or(&raw)
        .trim()
        .trim_end_matches('\\');
    if stripped.is_empty() {
        return Err("folder source path cannot be empty".to_string());
    }
    let normalized_display = if stripped.len() == 2 && stripped.ends_with(':') {
        format!("{stripped}\\")
    } else {
        stripped.to_string()
    };
    let canonical = if normalized_display.len() == 3 {
        let mut chars = normalized_display.chars();
        if let (Some(drive), Some(':'), Some('\\')) = (chars.next(), chars.next(), chars.next()) {
            format!("{}:\\", drive.to_ascii_uppercase())
        } else {
            normalized_display
        }
    } else if normalized_display.as_bytes().get(1) == Some(&b':') {
        let mut out = normalized_display;
        if let Some(first) = out.as_bytes().first().copied() {
            if first.is_ascii_alphabetic() {
                let upper = (first as char).to_ascii_uppercase();
                out.replace_range(0..1, &upper.to_string());
            }
        }
        out
    } else {
        normalized_display
    };
    Ok((PathBuf::from(&canonical), canonical))
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

fn should_exclude_segment(name: &str, excluded_segments: &[String]) -> bool {
    excluded_segments
        .iter()
        .any(|rule| !rule.contains(['\\', '/']) && name.eq_ignore_ascii_case(rule))
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
    live_watch_enabled: bool,
    generation: u64,
    next_key: u64,
    live_count: u64,
    live_file_bytes: u64,
    has_tombstones: bool,
    volume_id_tag: u16,
}

impl FolderSource {
    pub fn new(source_id: SourceId, root: &Path) -> Result<Self, String> {
        let (normalized_path, display) = normalize_folder_root(root)?;
        let root_prefix = normalize_root(&display);
        let volume_id_tag = normalized_path
            .to_string_lossy()
            .chars()
            .next()
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
            live_watch_enabled: false,
            generation: 1,
            next_key: 1,
            live_count: 0,
            live_file_bytes: 0,
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

    pub fn live_file_bytes(&self) -> u64 {
        self.live_file_bytes
    }

    pub fn has_tombstones(&self) -> bool {
        self.has_tombstones
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
        SourceIndex::new(
            self.source_id,
            &self.root_prefix,
            &self.records,
            &self.arena,
            &self.path_cache,
            None,
            properties,
        )
        .with_tombstones(self.has_tombstones)
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
    }

    pub fn set_excluded_paths(&mut self, excluded_paths: &[String]) {
        self.excluded_paths = excluded_paths.to_vec();
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
        self.excluded_paths = excluded_paths.to_vec();
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

        self.status.set(VolumeState::Rebuilding);
        let mut records = Vec::new();
        let mut arena = NamePool::default();
        let mut frn_to_idx = FrnLookup::new();
        let mut path_to_idx = HashMap::new();
        let mut next_key = 1u64;
        let mut live_count = 0u64;
        let mut live_file_bytes = 0u64;

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
                continue;
            };
            for entry in read_dir.flatten() {
                let entry_path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if should_exclude_segment(&name, excluded_paths) {
                    continue;
                }
                let Ok(meta) = fs::symlink_metadata(&entry_path) else {
                    continue;
                };
                let (flags, is_dir, is_reparse, size, created_ts, modified_ts) =
                    metadata_record_fields(&meta);
                let rel_key = if rel_dir.is_empty() {
                    name.to_ascii_lowercase()
                } else {
                    format!("{rel_dir}\\{}", name.to_ascii_lowercase())
                };
                if !excluded_paths.is_empty() && path_is_excluded(&rel_key, excluded_paths) {
                    continue;
                }

                let idx = records.len() as u32;
                let key = next_key;
                next_key = next_key.saturating_add(1);
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
                if !is_dir {
                    live_file_bytes = live_file_bytes.saturating_add(size);
                } else if !is_reparse {
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
        self.next_key = next_key;
        self.live_count = live_count;
        self.live_file_bytes = live_file_bytes;
        self.has_tombstones = false;
        self.generation = self.generation.wrapping_add(1);
        self.status.mark_folder_synced();
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

    fn tombstone_subtree(&mut self, rel_key: &str) -> Option<u64> {
        let prefix = format!("{rel_key}\\");
        let targets: Vec<(String, u32)> = self
            .path_to_idx
            .iter()
            .filter(|(key, _)| key.as_str() == rel_key || key.starts_with(&prefix))
            .map(|(k, &idx)| (k.clone(), idx))
            .collect();

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
            if !rec.is_dir() {
                self.live_file_bytes = self.live_file_bytes.saturating_sub(rec.size);
                self.adjust_ancestors(rec.parent_idx, -(rec.size as i128));
            }
            self.records[idx as usize].mark_tombstone();
            self.live_count = self.live_count.saturating_sub(1);
            self.has_tombstones = true;
        }
        primary_frn
    }

    fn upsert_live_path(&mut self, path: &Path) -> Option<(u64, JournalChangeKind)> {
        let (rel_key, name) = self.relative_key(path)?;
        if path_is_excluded(&rel_key, &self.excluded_paths) {
            return None;
        }
        let meta = fs::symlink_metadata(path).ok()?;
        let (flags, is_dir, _is_reparse, size, created_ts, modified_ts) =
            metadata_record_fields(&meta);

        if let Some(&idx) = self.path_to_idx.get(&rel_key) {
            let rec = &mut self.records[idx as usize];
            if !rec.is_tombstone() {
                let was_dir = rec.is_dir();
                let old_size = rec.size;
                let parent_idx = rec.parent_idx;
                let frn = rec.frn;
                rec.flags = flags;
                rec.created_ts = created_ts;
                rec.modified_ts = modified_ts;
                if !was_dir && !is_dir {
                    rec.size = size;
                    let delta = size as i128 - old_size as i128;
                    if delta >= 0 {
                        self.live_file_bytes = self.live_file_bytes.saturating_add(delta as u64);
                    } else {
                        self.live_file_bytes = self.live_file_bytes.saturating_sub((-delta) as u64);
                    }
                    self.adjust_ancestors(parent_idx, delta);
                }
                return Some((frn, JournalChangeKind::Modified));
            }
        }

        let parent_idx = self.parent_idx_for_path(path)?;
        let (name_offset, name_len, ext_offset) = self.arena.insert(&name);
        let idx = self.records.len() as u32;
        let key = self.next_key;
        self.next_key = self.next_key.saturating_add(1);
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
            self.live_file_bytes = self.live_file_bytes.saturating_add(size);
            self.adjust_ancestors(parent_idx, size as i128);
        }
        Some((key, JournalChangeKind::Created))
    }

    /// Applies a bounded batch of change events. Returns `true` when buffer overflow or structural
    /// ambiguity requires a full subtree rescan.
    pub fn apply_folder_events(
        &mut self,
        events: &[FolderWatchEvent],
        mut journal: Option<&mut IndexJournal>,
        now_unix: i64,
    ) -> bool {
        if events.len() > FOLDER_CHANGE_BUFFER_CAP
            || events
                .iter()
                .any(|e| matches!(e, FolderWatchEvent::Overflow))
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
            return true;
        }
        let mut needs_rescan = false;
        let mut table_changed = false;

        for event in events {
            match event {
                FolderWatchEvent::Overflow => return true,
                FolderWatchEvent::Added(path) | FolderWatchEvent::Modified(path) => {
                    if let Some((frn, kind)) = self.upsert_live_path(path) {
                        table_changed = true;
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
                    } else if path.exists() && self.relative_key(path).is_some() {
                        // A nested directory was created before our table saw it; trigger a quick rescan.
                        needs_rescan = true;
                    }
                }
                FolderWatchEvent::Removed(path) => {
                    if let Some((rel_key, _)) = self.relative_key(path) {
                        let frn = self.tombstone_subtree(&rel_key).unwrap_or(0);
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
                        moved_frn = self.tombstone_subtree(&old_key).unwrap_or(0);
                    }
                    if let Some((frn, _)) = self.upsert_live_path(new_path) {
                        moved_frn = frn;
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
            self.generation = self.generation.wrapping_add(1);
        }
        needs_rescan
    }

    pub fn build_file_row<F, P>(
        &self,
        record_idx: u32,
        folded_terms: &[Vec<u8>],
        snippet_lookup: F,
        property_lookup: P,
    ) -> Option<cairn_protocol::message::FileRowData>
    where
        F: FnOnce(u64) -> Vec<String>,
        P: FnOnce(u64) -> Option<cairn_core::property_store::RecordPropertyData>,
    {
        super::volume_index::build_file_row_from_parts(
            &self.records,
            &self.arena,
            &self.path_cache,
            &self.root_prefix,
            record_idx,
            folded_terms,
            snippet_lookup,
            property_lookup,
        )
    }
}

fn path_is_excluded(rel_lower: &str, excluded_paths: &[String]) -> bool {
    if excluded_paths.is_empty() {
        return false;
    }
    rel_lower.split('\\').any(|segment| {
        excluded_paths
            .iter()
            .any(|rule| !rule.contains(['\\', '/']) && segment.eq_ignore_ascii_case(rule))
    }) || excluded_paths.iter().any(|rule| {
        rule.contains(['\\', '/'])
            && rel_lower.contains(&rule.replace('/', "\\").to_ascii_lowercase())
    })
}

/// Active OS directory watcher handle whose drop cleanly cancels and joins its worker thread.
pub(crate) struct FolderWatcherHandle {
    #[cfg(windows)]
    stop_event: usize,
    join: Option<JoinHandle<()>>,
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
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
        FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects, INFINITE};
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
    let stop_event = unsafe { CreateEventW(None, true, false, None) }.ok()?;
    // SAFETY: auto-reset overlapped IO completion event owned by watcher thread.
    let io_event = unsafe { CreateEventW(None, false, false, None) }.ok()?;

    let dir_raw = dir_handle.0 as usize;
    let stop_raw = stop_event.0 as usize;
    let io_raw = io_event.0 as usize;

    let join = std::thread::Builder::new()
        .name("cairn-folder-watch".to_string())
        .spawn(move || {
            let dir_h = HANDLE(dir_raw as *mut core::ffi::c_void);
            let stop_h = HANDLE(stop_raw as *mut core::ffi::c_void);
            let io_h = HANDLE(io_raw as *mut core::ffi::c_void);
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
                let mut bytes_returned = 0u32;
                // SAFETY: dir_h and buffer remain valid until CancelIoEx/GetOverlappedResult completes.
                let issued = unsafe {
                    ReadDirectoryChangesW(
                        dir_h,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as u32,
                        true,
                        filter,
                        Some(&mut bytes_returned),
                        Some(&mut overlapped),
                        None,
                    )
                };
                if issued.is_err() {
                    break;
                }

                let waits = [stop_h, io_h];
                // SAFETY: Both event handles stay valid until the loop exits before FolderWatcherHandle closes stop_h.
                let wait = unsafe { WaitForMultipleObjects(&waits, false, INFINITE) };
                if wait == WAIT_OBJECT_0 {
                    unsafe {
                        let _ = CancelIoEx(dir_h, Some(&overlapped));
                        let _ = GetOverlappedResult(dir_h, &overlapped, &mut bytes_returned, true);
                    }
                    break;
                }

                // SAFETY: overlapped signaled completion on io_h.
                let completed =
                    unsafe { GetOverlappedResult(dir_h, &overlapped, &mut bytes_returned, false) };
                if completed.is_err() || bytes_returned == 0 {
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

    if join.is_none() {
        // SAFETY: Clean up created handles when thread spawn fails.
        unsafe {
            let _ = CloseHandle(io_event);
            let _ = CloseHandle(stop_event);
            let _ = CloseHandle(dir_handle);
        }
        return None;
    }

    Some(FolderWatcherHandle {
        stop_event: stop_raw,
        join,
    })
}

#[cfg(windows)]
fn parse_notify_buffer(root: &Path, buf: &[u8]) -> Vec<FolderWatchEvent> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
        FILE_ACTION_RENAMED_OLD_NAME, FILE_NOTIFY_INFORMATION,
    };

    let mut events = Vec::new();
    let mut offset = 0usize;
    let mut pending_old: Option<PathBuf> = None;

    while offset + size_of::<FILE_NOTIFY_INFORMATION>() <= buf.len() {
        // SAFETY: Read fixed header fields cleanly via unaligned pointer read.
        let info = unsafe {
            std::ptr::read_unaligned(buf[offset..].as_ptr().cast::<FILE_NOTIFY_INFORMATION>())
        };
        let name_bytes = info.FileNameLength as usize;
        let name_offset = offset + 12;
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

        match info.Action {
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
        if info.NextEntryOffset == 0 {
            break;
        }
        let next = offset + info.NextEntryOffset as usize;
        if next <= offset {
            break;
        }
        offset = next;
    }

    events
}

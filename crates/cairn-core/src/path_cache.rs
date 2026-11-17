// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Path reconstruction and directory path memoization cache.

use crate::arena::NamePool;
use crate::model::FileRecord;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Maximum number of directory paths cached in memory before eviction.
pub const MAX_DIRECTORY_CACHE_ENTRIES: usize = 16_384;

static ROOT_PATHS: std::sync::LazyLock<[Arc<str>; 26]> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|i| {
        let letter = (b'A' + i as u8) as char;
        Arc::from(format!("{letter}:\\"))
    })
});

/// Returns a pre-allocated static `Arc<str>` for standard root drive paths (e.g. `C:\`), avoiding heap allocation.
#[inline]
pub fn root_path_for_drive(drive_letter: char) -> Arc<str> {
    let upper = drive_letter.to_ascii_uppercase();
    if upper.is_ascii_uppercase() {
        let idx = (upper as u8 - b'A') as usize;
        ROOT_PATHS[idx].clone()
    } else {
        Arc::from(format!("{upper}:\\"))
    }
}

/// Extracts the uppercase ASCII drive letter from a path beginning with `X:` (e.g. `C:\...` or `C:`).
#[inline]
pub fn drive_letter_prefix(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some((bytes[0] as char).to_ascii_uppercase())
    } else {
        None
    }
}

/// Extracts the uppercase ASCII drive letter from an exact mount root (`X:\` or `X:/`).
#[inline]
pub fn drive_letter_of_mount(path: &str) -> Option<char> {
    if path.len() == 3 && matches!(path.as_bytes()[2], b'\\' | b'/') {
        drive_letter_prefix(path)
    } else {
        None
    }
}

/// Normalizes an index source root (drive letter `C:\`, mount point `C:\Mounts\Data`, or folder
/// `D:\Projects`) into an `Arc<str>` ending with `\` while reusing static drive roots when matched.
pub fn normalize_root(root_prefix: &str) -> Arc<str> {
    let trimmed = root_prefix.trim();
    if let Some(drive) = drive_letter_prefix(trimmed) {
        if trimmed.as_bytes()[2..]
            .iter()
            .all(|ch| *ch == b'\\' || *ch == b'/')
        {
            return root_path_for_drive(drive);
        }
    }
    if trimmed.is_empty() {
        return Arc::from("");
    }
    let mut normalized = trimmed.replace('/', "\\");
    if !normalized.ends_with('\\') {
        normalized.push('\\');
    }
    Arc::from(normalized.as_str())
}

/// Thread-safe directory path cache mapping directory indices to cached directory paths.
#[derive(Default)]
pub struct DirectoryPathCache {
    cache: RwLock<HashMap<u32, Arc<str>>>,
}

impl DirectoryPathCache {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Clears the path cache (e.g. after USN rename or directory moves) and frees memory.
    pub fn clear(&self) {
        if let Ok(mut lock) = self.cache.write() {
            lock.clear();
            lock.shrink_to_fit();
        }
    }

    /// Returns estimated heap bytes held by the map table and cached `Arc<str>` payloads.
    pub fn allocated_bytes(&self) -> usize {
        let Ok(lock) = self.cache.read() else {
            return 0;
        };
        let table = crate::frn_index::hashmap_table_bytes::<u32, Arc<str>>(lock.capacity());
        let strings: usize = lock
            .values()
            .map(|s| 2 * std::mem::size_of::<usize>() + s.len())
            .sum();
        table + strings
    }

    /// Reconstructs the full parent directory path for `record_idx` using a drive letter.
    pub fn get_parent_path(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        drive_letter: char,
        record_idx: u32,
    ) -> Arc<str> {
        let root = root_path_for_drive(drive_letter);
        self.get_parent_path_with_root(records, arena, &root, record_idx)
    }

    /// Reconstructs the full parent directory path for `record_idx` under an arbitrary root path.
    pub fn get_parent_path_for_root(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        root_prefix: &str,
        record_idx: u32,
    ) -> Arc<str> {
        let root = normalize_root(root_prefix);
        self.get_parent_path_with_root(records, arena, &root, record_idx)
    }

    /// Reconstructs the parent directory path using a pre-normalized `Arc<str>` root ending in `\`.
    pub fn get_parent_path_with_root(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        root: &Arc<str>,
        record_idx: u32,
    ) -> Arc<str> {
        let parent_idx = match records.get(record_idx as usize) {
            Some(rec) => rec.parent_idx,
            None => return root.clone(),
        };

        if parent_idx == FileRecord::ROOT_PARENT || parent_idx == record_idx {
            return root.clone();
        }

        if let Ok(read_guard) = self.cache.read() {
            if let Some(cached) = read_guard.get(&parent_idx) {
                return cached.clone();
            }
        }

        let (computed_path, intermediates) =
            reconstruct_dir_path(records, arena, root, parent_idx, &self.cache);

        if let Ok(mut write_guard) = self.cache.write() {
            if write_guard.len() >= MAX_DIRECTORY_CACHE_ENTRIES {
                let to_remove = MAX_DIRECTORY_CACHE_ENTRIES / 4;
                let keys: Vec<u32> = write_guard.keys().take(to_remove).copied().collect();
                for k in keys {
                    write_guard.remove(&k);
                }
            }
            for (idx, path) in intermediates {
                write_guard.insert(idx, path);
            }
            write_guard.insert(parent_idx, computed_path.clone());
        }

        computed_path
    }

    /// Reconstructs the complete path (including file name) for `record_idx` using a drive letter.
    pub fn get_full_path(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        drive_letter: char,
        record_idx: u32,
    ) -> String {
        let root = root_path_for_drive(drive_letter);
        self.get_full_path_with_root(records, arena, &root, record_idx)
    }

    /// Reconstructs the complete path for `record_idx` under an arbitrary root prefix.
    pub fn get_full_path_for_root(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        root_prefix: &str,
        record_idx: u32,
    ) -> String {
        let root = normalize_root(root_prefix);
        self.get_full_path_with_root(records, arena, &root, record_idx)
    }

    /// Reconstructs the complete path using a pre-normalized `Arc<str>` root.
    pub fn get_full_path_with_root(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        root: &Arc<str>,
        record_idx: u32,
    ) -> String {
        let Some(rec) = records.get(record_idx as usize) else {
            return String::new();
        };

        let parent_path = self.get_parent_path_with_root(records, arena, root, record_idx);
        let name = arena.get_name(rec.name_offset, rec.name_len);

        if parent_path.ends_with('\\') {
            format!("{parent_path}{name}")
        } else {
            format!("{parent_path}\\{name}")
        }
    }
}

/// Reconstructs the directory path starting from `dir_idx` upwards to `root`,
/// reusing any already-cached ancestor prefix and recording intermediate ancestor paths.
fn reconstruct_dir_path(
    records: &[FileRecord],
    arena: &NamePool,
    root: &str,
    dir_idx: u32,
    cache: &RwLock<HashMap<u32, Arc<str>>>,
) -> (Arc<str>, Vec<(u32, Arc<str>)>) {
    let mut chain = Vec::new();
    let mut curr_idx = dir_idx;
    let mut visited = 0;
    const MAX_DEPTH: usize = 256;
    let mut cached_prefix = None;

    {
        let read_guard = cache.read().ok();
        while curr_idx != FileRecord::ROOT_PARENT && visited < MAX_DEPTH {
            visited += 1;
            let Some(rec) = records.get(curr_idx as usize) else {
                break;
            };

            let name = arena.get_name(rec.name_offset, rec.name_len);
            if !name.is_empty() {
                chain.push((curr_idx, name));
            }

            if rec.parent_idx == curr_idx || rec.parent_idx == FileRecord::ROOT_PARENT {
                break;
            }

            if let Some(prefix) = read_guard.as_ref().and_then(|g| g.get(&rec.parent_idx)) {
                cached_prefix = Some(prefix.clone());
                break;
            }

            curr_idx = rec.parent_idx;
        }
    }

    let mut current_path = if let Some(prefix) = cached_prefix {
        let mut s = String::with_capacity(prefix.len() + chain.len() * 16);
        s.push_str(&prefix);
        s
    } else {
        let mut s = String::with_capacity(root.len() + chain.len() * 16);
        s.push_str(root);
        if !root.is_empty() && !root.ends_with('\\') {
            s.push('\\');
        }
        s
    };

    let mut intermediates = Vec::new();
    let chain_len = chain.len();

    for (i, &(node_idx, seg)) in chain.iter().rev().enumerate() {
        current_path.push_str(seg);
        current_path.push('\\');
        if i + 1 < chain_len {
            intermediates.push((node_idx, Arc::from(current_path.as_str())));
        }
    }

    (Arc::from(current_path.as_str()), intermediates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_cache_reconstruction() {
        let mut arena = NamePool::default();
        let (root_name_off, root_name_len, _) = arena.insert("");
        let (win_off, win_len, _) = arena.insert("Windows");
        let (sys_off, sys_len, _) = arena.insert("System32");
        let (cmd_off, cmd_len, cmd_ext) = arena.insert("cmd.exe");

        let records = vec![
            // 0: root
            FileRecord {
                frn: 5,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: root_name_off,
                name_len: root_name_len,
                ext_offset: u16::MAX,
                volume_id: 0,
                flags: crate::model::FLAG_DIRECTORY,
                size: 0,
                modified_ts: 0,
                created_ts: 0,
            },
            // 1: Windows
            FileRecord {
                frn: 100,
                parent_idx: 0,
                name_offset: win_off,
                name_len: win_len,
                ext_offset: u16::MAX,
                volume_id: 0,
                flags: crate::model::FLAG_DIRECTORY,
                size: 0,
                modified_ts: 0,
                created_ts: 0,
            },
            // 2: System32
            FileRecord {
                frn: 101,
                parent_idx: 1,
                name_offset: sys_off,
                name_len: sys_len,
                ext_offset: u16::MAX,
                volume_id: 0,
                flags: crate::model::FLAG_DIRECTORY,
                size: 0,
                modified_ts: 0,
                created_ts: 0,
            },
            // 3: cmd.exe
            FileRecord {
                frn: 102,
                parent_idx: 2,
                name_offset: cmd_off,
                name_len: cmd_len,
                ext_offset: cmd_ext,
                volume_id: 0,
                flags: 0,
                size: 300000,
                modified_ts: 0,
                created_ts: 0,
            },
        ];

        let cache = DirectoryPathCache::new();
        let full = cache.get_full_path(&records, &arena, 'C', 3);
        assert_eq!(full, "C:\\Windows\\System32\\cmd.exe");

        let parent = cache.get_parent_path(&records, &arena, 'C', 3);
        assert_eq!(&*parent, "C:\\Windows\\System32\\");
    }

    #[test]
    fn test_cache_eviction_preserves_most_entries() {
        let cache = DirectoryPathCache::new();
        let records = vec![FileRecord {
            frn: 1,
            parent_idx: 999_999,
            name_offset: 0,
            name_len: 0,
            ext_offset: u16::MAX,
            volume_id: 0,
            flags: 0,
            size: 0,
            modified_ts: 0,
            created_ts: 0,
        }];
        let arena = NamePool::default();

        // Populate with mock entries up to capacity
        if let Ok(mut write) = cache.cache.write() {
            for i in 0..MAX_DIRECTORY_CACHE_ENTRIES as u32 {
                write.insert(i, format!("C:\\dir_{i}\\").into());
            }
        }

        // Accessing one new directory should trigger 25% eviction instead of 100% wipe
        let _ = cache.get_parent_path(&records, &arena, 'C', 0);
        let len = cache.cache.read().unwrap().len();
        // Started with 16384, evicted 4096, inserted 1 -> 12289
        assert!(len > (MAX_DIRECTORY_CACHE_ENTRIES * 7 / 10));
        assert!(len <= MAX_DIRECTORY_CACHE_ENTRIES);
    }
}

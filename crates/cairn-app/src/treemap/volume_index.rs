// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Pre-built volume index (`vol_<drive>.idx`) loader for instant in-memory
//! Treemap hierarchy and drill-down.

use cairn_core::model::FileRecord;
use cairn_core::treemap::DirectorySizeIndex;
use cairn_core::NamePool;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Cached in-memory volume index for fast drill-down without disk re-scans.
pub(super) struct VolumeTreemapIndex {
    pub(super) records: Vec<FileRecord>,
    pub(super) arena: NamePool,
    pub(super) dir_index: DirectorySizeIndex,
}

/// Checks standard locations for a pre-built volume index file (`vol_<drive>.idx`).
pub(super) fn try_load_index_for_drive(drive: char) -> Option<VolumeTreemapIndex> {
    let drive_upper = drive.to_ascii_uppercase();
    let file_name = format!("vol_{}.idx", drive_upper);

    let candidates = [
        dirs_local_appdata().map(|p| p.join("Cairn").join("User").join("index").join(&file_name)),
        dirs_local_appdata().map(|p| p.join("Cairn").join("index").join(&file_name)),
        dirs_programdata().map(|p| p.join("Cairn").join("index").join(&file_name)),
        Some(PathBuf::from(r"C:\ProgramData\Cairn\index").join(&file_name)),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.is_file() {
            if let Some(idx) = load_index_file(&candidate) {
                return Some(idx);
            }
        }
    }
    None
}

fn dirs_local_appdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

fn dirs_programdata() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(PathBuf::from)
}

fn load_index_file(path: &Path) -> Option<VolumeTreemapIndex> {
    let mut f = File::open(path).ok()?;
    let metadata = f.metadata().ok()?;
    let file_len = metadata.len();

    let mut header = [0u8; 64];
    f.read_exact(&mut header).ok()?;

    if &header[0..8] != b"CAIRNIDX" {
        return None;
    }

    let version = u32::from_le_bytes(header[8..12].try_into().ok()?);
    if version != 1 {
        return None;
    }

    let record_count = u32::from_le_bytes(header[40..44].try_into().ok()?) as usize;
    let arena_len = u32::from_le_bytes(header[44..48].try_into().ok()?) as usize;
    let folded_len = u32::from_le_bytes(header[48..52].try_into().ok()?) as usize;

    let record_size = std::mem::size_of::<FileRecord>();
    let records_byte_len = record_count.checked_mul(record_size)?;

    let expected_file_len = 64u64
        .checked_add(records_byte_len as u64)?
        .checked_add(arena_len as u64)?
        .checked_add(folded_len as u64)?;
    if file_len < expected_file_len {
        return None;
    }

    let zero_record = FileRecord {
        frn: 0,
        parent_idx: 0,
        name_offset: 0,
        name_len: 0,
        ext_offset: 0,
        volume_id: 0,
        flags: 0,
        size: 0,
        modified_ts: 0,
        created_ts: 0,
    };
    let mut records = vec![zero_record; record_count];
    if records_byte_len > 0 {
        // SAFETY: `FileRecord` is a `#[repr(C)]` plain-old-data struct of primitive
        // integer fields with no padding bytes (`size_of::<FileRecord>() == 48`),
        // and `records` is initialized with `record_count` elements.
        let records_bytes = unsafe {
            std::slice::from_raw_parts_mut(records.as_mut_ptr().cast::<u8>(), records_byte_len)
        };
        f.read_exact(records_bytes).ok()?;
    }

    let mut arena_buf = vec![0u8; arena_len];
    f.read_exact(&mut arena_buf).ok()?;

    let mut folded_buf = vec![0u8; folded_len];
    f.read_exact(&mut folded_buf).ok()?;

    let arena = NamePool::from_raw_parts(arena_buf, folded_buf);
    let dir_index = DirectorySizeIndex::build(&records, &arena);

    Some(VolumeTreemapIndex {
        records,
        arena,
        dir_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::model::FLAG_DIRECTORY;
    use std::io::Write;

    #[test]
    fn load_index_file_reads_records_directly_and_rejects_corrupt_headers() {
        let mut pool = NamePool::default();
        let (dir_off, dir_len, dir_ext) = pool.insert("Users");
        let (file_off, file_len, file_ext) = pool.insert("report.pdf");
        let records = vec![
            FileRecord {
                frn: 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: dir_off,
                name_len: dir_len,
                ext_offset: dir_ext,
                volume_id: 2,
                flags: FLAG_DIRECTORY,
                size: 0,
                modified_ts: 100,
                created_ts: 90,
            },
            FileRecord {
                frn: 2,
                parent_idx: 0,
                name_offset: file_off,
                name_len: file_len,
                ext_offset: file_ext,
                volume_id: 2,
                flags: 0,
                size: 8192,
                modified_ts: 200,
                created_ts: 150,
            },
        ];

        let (arena_bytes, folded_bytes) = pool.raw_parts();
        let record_bytes = unsafe {
            std::slice::from_raw_parts(
                records.as_ptr().cast::<u8>(),
                records.len() * std::mem::size_of::<FileRecord>(),
            )
        };

        let mut header = [0u8; 64];
        header[0..8].copy_from_slice(b"CAIRNIDX");
        header[8..12].copy_from_slice(&1u32.to_le_bytes());
        header[40..44].copy_from_slice(&(records.len() as u32).to_le_bytes());
        header[44..48].copy_from_slice(&(arena_bytes.len() as u32).to_le_bytes());
        header[48..52].copy_from_slice(&(folded_bytes.len() as u32).to_le_bytes());

        let temp_dir =
            std::env::temp_dir().join(format!("cairn_treemap_idx_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let idx_path = temp_dir.join("vol_C.idx");

        {
            let mut file = File::create(&idx_path).unwrap();
            file.write_all(&header).unwrap();
            file.write_all(record_bytes).unwrap();
            file.write_all(arena_bytes).unwrap();
            file.write_all(folded_bytes).unwrap();
        }

        let loaded = load_index_file(&idx_path).expect("valid index file loads");
        assert_eq!(loaded.records, records);
        let tree =
            loaded
                .dir_index
                .build_tree_node(&loaded.records, &loaded.arena, Some(0), "Users", 4);
        assert_eq!(tree.size, 8192);
        assert_eq!(loaded.dir_index.descendant_count(None), 2);

        // Truncated file is rejected.
        let truncated_path = temp_dir.join("truncated.idx");
        std::fs::write(&truncated_path, header).unwrap();
        assert!(load_index_file(&truncated_path).is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

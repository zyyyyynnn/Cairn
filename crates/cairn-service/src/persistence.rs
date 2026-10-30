// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fast atomic index persistence and verification for Cairn (IDX-04, NFR-09).

use cairn_core::arena::NamePool;
use cairn_core::model::FileRecord;
use crc32fast::Hasher;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const INDEX_MAGIC: &[u8; 8] = b"CAIRNIDX";
pub const INDEX_VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;

/// Header byte 13 says whether the saved records already carry correct recursive directory sizes.
/// Caches written before folder sizes existed leave it zero, so a loader can tell that it must
/// aggregate once instead of trusting the stored zeroes. The byte was reserved (always zero) in the
/// original header, which keeps every older cache loadable without a version bump.
const HEADER_DIRECTORY_SIZES_VALID: usize = 13;

#[derive(Error, Debug)]
pub enum PersistenceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid index file magic signature: expected CAIRNIDX")]
    InvalidMagic,
    #[error("Unsupported index format version: {0}")]
    UnsupportedVersion(u32),
    #[error("Index checksum mismatch (corrupted index file)")]
    ChecksumMismatch,
    #[error("Invalid data payload length")]
    InvalidPayloadLength,
}

#[derive(Clone, Debug)]
pub struct LoadedIndexData {
    pub drive_letter: char,
    pub volume_serial: u64,
    pub journal_id: u64,
    pub last_usn: i64,
    pub records: Vec<FileRecord>,
    pub arena: NamePool,
    /// `false` for caches written before directory sizes were persisted (or by a build that could
    /// not prove the totals); the caller must aggregate once before serving the records.
    pub directory_sizes_valid: bool,
}

/// Atomically saves volume index records and arena to `vol_<drive>.idx`.
#[allow(clippy::too_many_arguments)]
pub fn save_volume_index_atomic(
    dir: &Path,
    drive_letter: char,
    volume_serial: u64,
    journal_id: u64,
    last_usn: i64,
    records: &[FileRecord],
    arena: &NamePool,
    directory_sizes_valid: bool,
) -> Result<PathBuf, PersistenceError> {
    fs::create_dir_all(dir)?;

    let file_name = format!("vol_{}.idx", drive_letter.to_ascii_uppercase());
    let target_path = dir.join(&file_name);
    let tmp_path = dir.join(format!("{}.tmp", file_name));

    let records_byte_len = std::mem::size_of_val(records);
    let (arena_bytes, folded_bytes) = arena.raw_parts();

    // Compute CRC32 checksum across all payload bytes
    let mut hasher = Hasher::new();
    // SAFETY: FileRecord is a #[repr(C)] POD struct with primitive integer fields.
    let records_slice =
        unsafe { std::slice::from_raw_parts(records.as_ptr() as *const u8, records_byte_len) };
    hasher.update(records_slice);
    hasher.update(arena_bytes);
    hasher.update(folded_bytes);
    let checksum = hasher.finalize();

    // Construct 64-byte header
    let mut header = [0u8; HEADER_SIZE];
    header[0..8].copy_from_slice(INDEX_MAGIC);
    header[8..12].copy_from_slice(&INDEX_VERSION.to_le_bytes());
    header[12] = drive_letter as u8;
    header[HEADER_DIRECTORY_SIZES_VALID] = u8::from(directory_sizes_valid);
    header[16..24].copy_from_slice(&volume_serial.to_le_bytes());
    header[24..32].copy_from_slice(&journal_id.to_le_bytes());
    header[32..40].copy_from_slice(&last_usn.to_le_bytes());
    header[40..44].copy_from_slice(&(records.len() as u32).to_le_bytes());
    header[44..48].copy_from_slice(&(arena_bytes.len() as u32).to_le_bytes());
    header[48..52].copy_from_slice(&(folded_bytes.len() as u32).to_le_bytes());
    header[52..56].copy_from_slice(&checksum.to_le_bytes());

    // Write to temporary file
    {
        let mut f = File::create(&tmp_path)?;
        f.write_all(&header)?;
        f.write_all(records_slice)?;
        f.write_all(arena_bytes)?;
        f.write_all(folded_bytes)?;
        f.flush()?;
    }

    // Atomic replace
    atomic_replace_file(&tmp_path, &target_path)?;

    Ok(target_path)
}

/// Loads and validates an index file from disk.
pub fn load_volume_index(path: &Path) -> Result<LoadedIndexData, PersistenceError> {
    let mut f = File::open(path)?;
    let mut header = [0u8; HEADER_SIZE];
    f.read_exact(&mut header)?;

    if &header[0..8] != INDEX_MAGIC {
        return Err(PersistenceError::InvalidMagic);
    }

    let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if version != INDEX_VERSION {
        return Err(PersistenceError::UnsupportedVersion(version));
    }

    let drive_letter = header[12] as char;
    let directory_sizes_valid = header[HEADER_DIRECTORY_SIZES_VALID] != 0;
    let volume_serial = u64::from_le_bytes(header[16..24].try_into().unwrap());
    let journal_id = u64::from_le_bytes(header[24..32].try_into().unwrap());
    let last_usn = i64::from_le_bytes(header[32..40].try_into().unwrap());
    let record_count = u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize;
    let arena_len = u32::from_le_bytes(header[44..48].try_into().unwrap()) as usize;
    let folded_len = u32::from_le_bytes(header[48..52].try_into().unwrap()) as usize;
    let expected_checksum = u32::from_le_bytes(header[52..56].try_into().unwrap());

    let record_size = std::mem::size_of::<FileRecord>();
    let records_byte_len = record_count
        .checked_mul(record_size)
        .ok_or(PersistenceError::InvalidPayloadLength)?;
    let expected_file_len = HEADER_SIZE
        .checked_add(records_byte_len)
        .and_then(|n| n.checked_add(arena_len))
        .and_then(|n| n.checked_add(folded_len))
        .ok_or(PersistenceError::InvalidPayloadLength)?;
    let actual_file_len =
        usize::try_from(f.metadata()?.len()).map_err(|_| PersistenceError::InvalidPayloadLength)?;
    if actual_file_len != expected_file_len {
        return Err(PersistenceError::InvalidPayloadLength);
    }
    if arena_len != folded_len {
        return Err(PersistenceError::InvalidPayloadLength);
    }

    let mut records_buf = vec![0u8; records_byte_len];
    f.read_exact(&mut records_buf)?;

    let mut arena_buf = vec![0u8; arena_len];
    f.read_exact(&mut arena_buf)?;

    let mut folded_buf = vec![0u8; folded_len];
    f.read_exact(&mut folded_buf)?;

    // Verify CRC32
    let mut hasher = Hasher::new();
    hasher.update(&records_buf);
    hasher.update(&arena_buf);
    hasher.update(&folded_buf);
    let computed_checksum = hasher.finalize();

    if computed_checksum != expected_checksum {
        return Err(PersistenceError::ChecksumMismatch);
    }

    // Convert records_buf to Vec<FileRecord>
    let mut records = Vec::with_capacity(record_count);
    for chunk in records_buf.chunks_exact(record_size) {
        // SAFETY: FileRecord is #[repr(C)] POD, length matches exact struct size.
        let rec = unsafe { std::ptr::read_unaligned(chunk.as_ptr() as *const FileRecord) };
        records.push(rec);
    }

    let arena = NamePool::from_raw_parts(arena_buf, folded_buf);

    Ok(LoadedIndexData {
        drive_letter,
        volume_serial,
        journal_id,
        last_usn,
        records,
        arena,
        directory_sizes_valid,
    })
}

fn atomic_replace_file(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let wide_src: Vec<u16> = src
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let wide_dst: Vec<u16> = dst
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: MoveFileExW replaces destination file atomically with write through.
        let res = unsafe {
            MoveFileExW(
                PCWSTR(wide_src.as_ptr()),
                PCWSTR(wide_dst.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if res.is_err() {
            // Fall back to remove + rename if needed
            let _ = fs::remove_file(dst);
            fs::rename(src, dst)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(src, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_index_atomic_save_and_load_roundtrip() {
        let temp = tempdir().unwrap();
        let mut pool = NamePool::default();
        let (off1, len1, ext1) = pool.insert("main.rs");
        let (off2, len2, ext2) = pool.insert("test_document.docx");

        let records = vec![
            FileRecord {
                frn: 100,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: off1,
                name_len: len1,
                ext_offset: ext1,
                volume_id: 0,
                flags: 0,
                size: 2048,
                modified_ts: 1700000000,
                created_ts: 1700000000,
            },
            FileRecord {
                frn: 101,
                parent_idx: 0,
                name_offset: off2,
                name_len: len2,
                ext_offset: ext2,
                volume_id: 0,
                flags: 0,
                size: 65536,
                modified_ts: 1700000500,
                created_ts: 1700000500,
            },
        ];

        let saved_path = save_volume_index_atomic(
            temp.path(),
            'C',
            0x12345678,
            999,
            50000,
            &records,
            &pool,
            true,
        )
        .expect("save should succeed");

        assert!(saved_path.exists());

        let loaded = load_volume_index(&saved_path).expect("load should succeed");
        assert_eq!(loaded.drive_letter, 'C');
        assert_eq!(loaded.volume_serial, 0x12345678);
        assert_eq!(loaded.journal_id, 999);
        assert_eq!(loaded.last_usn, 50000);
        assert_eq!(loaded.records.len(), 2);
        assert_eq!(loaded.records[0].frn, 100);
        assert_eq!(loaded.records[1].frn, 101);
        assert_eq!(loaded.arena.get_name(off1, len1), "main.rs");
        assert_eq!(loaded.arena.get_name(off2, len2), "test_document.docx");
        assert!(
            loaded.directory_sizes_valid,
            "the saved validity flag must round-trip"
        );
    }

    #[test]
    fn an_index_saved_without_directory_sizes_is_marked_for_one_recompute() {
        let temp = tempdir().unwrap();
        let pool = NamePool::default();

        let saved_path = save_volume_index_atomic(temp.path(), 'D', 7, 8, 9, &[], &pool, false)
            .expect("save should succeed");
        let loaded = load_volume_index(&saved_path).expect("load should succeed");

        assert!(
            !loaded.directory_sizes_valid,
            "a cache that never aggregated folder sizes must say so"
        );
    }

    #[test]
    fn a_cache_with_a_zeroed_validity_byte_still_loads_as_a_legacy_index() {
        let temp = tempdir().unwrap();
        let mut pool = NamePool::default();
        let (offset, len, ext) = pool.insert("old.txt");
        let records = vec![FileRecord {
            frn: 1,
            parent_idx: FileRecord::ROOT_PARENT,
            name_offset: offset,
            name_len: len,
            ext_offset: ext,
            volume_id: 0,
            flags: 0,
            size: 12,
            modified_ts: 1,
            created_ts: 1,
        }];

        let saved_path = save_volume_index_atomic(temp.path(), 'E', 1, 2, 3, &records, &pool, true)
            .expect("save should succeed");
        // Older builds reserved header byte 13 and always wrote zero there; the rest of the header
        // and payload are unchanged, so clearing it reproduces exactly such a cache.
        let mut bytes = fs::read(&saved_path).expect("read cache");
        bytes[HEADER_DIRECTORY_SIZES_VALID] = 0;
        fs::write(&saved_path, &bytes).expect("write legacy cache");

        let loaded = load_volume_index(&saved_path).expect("legacy cache must load");
        assert_eq!(loaded.records.len(), 1);
        assert_eq!(loaded.last_usn, 3);
        assert!(!loaded.directory_sizes_valid);
    }

    #[test]
    fn corrupted_header_lengths_fail_before_allocating_unbounded_buffers() {
        let temp = tempdir().unwrap();
        let pool = NamePool::default();
        let saved_path = save_volume_index_atomic(temp.path(), 'C', 1, 2, 3, &[], &pool, true)
            .expect("save should succeed");

        let mut bytes = fs::read(&saved_path).expect("read cache");
        // Set record_count = u32::MAX (~192 GiB if allocated unchecked) on a 64-byte file.
        bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&saved_path, &bytes).expect("write corrupted header");

        let err = load_volume_index(&saved_path)
            .expect_err("crafted header length must fail before allocation");
        assert!(
            matches!(err, PersistenceError::InvalidPayloadLength),
            "expected InvalidPayloadLength, got {err:?}"
        );
    }
}

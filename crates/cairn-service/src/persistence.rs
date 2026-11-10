// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fast atomic index persistence and verification for Cairn (IDX-04, NFR-09). The byte layout
//! belongs to `cairn_core::index_file`; this module owns the file IO and the atomic replace.

use cairn_core::arena::NamePool;
use cairn_core::index_file::{
    decode_record, encode_record, IndexFileError, IndexHeader, HEADER_SIZE, RECORD_SIZE,
};
use cairn_core::model::FileRecord;
use crc32fast::Hasher;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

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

    let (arena_bytes, folded_bytes) = arena.raw_parts();
    let record_count =
        u32::try_from(records.len()).map_err(|_| PersistenceError::InvalidPayloadLength)?;
    let arena_len =
        u32::try_from(arena_bytes.len()).map_err(|_| PersistenceError::InvalidPayloadLength)?;
    let folded_len =
        u32::try_from(folded_bytes.len()).map_err(|_| PersistenceError::InvalidPayloadLength)?;

    // The checksum covers the encoded payload exactly as it will be written.
    let mut hasher = Hasher::new();
    let mut record_buf = [0u8; RECORD_SIZE];
    for record in records {
        encode_record(record, &mut record_buf);
        hasher.update(&record_buf);
    }
    hasher.update(arena_bytes);
    hasher.update(folded_bytes);
    let checksum = hasher.finalize();

    let header = IndexHeader {
        drive_letter,
        directory_sizes_valid,
        volume_serial,
        journal_id,
        last_usn,
        record_count,
        arena_len,
        folded_len,
        checksum,
    }
    .encode();

    {
        // Records are encoded one at a time to avoid a second full-size allocation; the buffer
        // keeps the write pattern batched instead of one syscall per record.
        let mut file = BufWriter::with_capacity(1 << 20, File::create(&tmp_path)?);
        file.write_all(&header)?;
        for record in records {
            encode_record(record, &mut record_buf);
            file.write_all(&record_buf)?;
        }
        file.write_all(arena_bytes)?;
        file.write_all(folded_bytes)?;
        file.flush()?;
    }

    atomic_replace_file(&tmp_path, &target_path)?;

    Ok(target_path)
}

/// Loads and validates an index file from disk.
pub fn load_volume_index(path: &Path) -> Result<LoadedIndexData, PersistenceError> {
    let mut file = File::open(path)?;
    let mut header_bytes = [0u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes)?;
    let header = IndexHeader::decode(&header_bytes).map_err(|error| match error {
        IndexFileError::InvalidMagic => PersistenceError::InvalidMagic,
        IndexFileError::UnsupportedVersion(version) => {
            PersistenceError::UnsupportedVersion(version)
        }
    })?;

    let records_byte_len = (header.record_count as usize)
        .checked_mul(RECORD_SIZE)
        .ok_or(PersistenceError::InvalidPayloadLength)?;
    let expected_file_len = HEADER_SIZE
        .checked_add(records_byte_len)
        .and_then(|n| n.checked_add(header.arena_len as usize))
        .and_then(|n| n.checked_add(header.folded_len as usize))
        .ok_or(PersistenceError::InvalidPayloadLength)?;
    let actual_file_len = usize::try_from(file.metadata()?.len())
        .map_err(|_| PersistenceError::InvalidPayloadLength)?;
    if actual_file_len != expected_file_len {
        return Err(PersistenceError::InvalidPayloadLength);
    }
    if header.arena_len != header.folded_len {
        return Err(PersistenceError::InvalidPayloadLength);
    }

    let mut records_buf = vec![0u8; records_byte_len];
    file.read_exact(&mut records_buf)?;

    let mut arena_buf = vec![0u8; header.arena_len as usize];
    file.read_exact(&mut arena_buf)?;

    let mut folded_buf = vec![0u8; header.folded_len as usize];
    file.read_exact(&mut folded_buf)?;

    let mut hasher = Hasher::new();
    hasher.update(&records_buf);
    hasher.update(&arena_buf);
    hasher.update(&folded_buf);
    if hasher.finalize() != header.checksum {
        return Err(PersistenceError::ChecksumMismatch);
    }

    let (chunks, remainder) = records_buf.as_chunks::<RECORD_SIZE>();
    debug_assert!(
        remainder.is_empty(),
        "record buffer is a whole number of records"
    );
    let mut records = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        records.push(decode_record(chunk));
    }

    Ok(LoadedIndexData {
        drive_letter: header.drive_letter,
        volume_serial: header.volume_serial,
        journal_id: header.journal_id,
        last_usn: header.last_usn,
        records,
        arena: NamePool::from_raw_parts(arena_buf, folded_buf),
        directory_sizes_valid: header.directory_sizes_valid,
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
    use cairn_core::index_file::HEADER_DIRECTORY_SIZES_VALID;
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
        // A zero validity byte is the on-disk form of a cache whose folder sizes were never
        // proven; the rest of the file is unchanged, so only that byte needs rewriting.
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
        // Set record_count = u32::MAX (~192 GiB if allocated unchecked) on a 64-byte file. The
        // count lives at header bytes 40..44.
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

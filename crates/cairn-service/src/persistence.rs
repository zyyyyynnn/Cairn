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

/// Streaming chunk size for decoding records without holding a full parallel byte buffer (384 KiB).
const DECODE_CHUNK_RECORDS: usize = 8_192;
const STREAM_CHUNK_BYTES: usize = DECODE_CHUNK_RECORDS * RECORD_SIZE;

/// Controlled growth headroom (~3.1%) reserved on loaded/compacted record tables so small USN
/// bursts do not trigger immediate 2x `Vec` reallocations.
pub(crate) fn headroom_for_records(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (len / 32).max(64)
    }
}

/// Controlled growth headroom (~3.1%) reserved on loaded/compacted string arenas.
pub(crate) fn headroom_for_arena(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (len / 32).max(2_048)
    }
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

/// Loads and validates a volume index from an arbitrary stream using bounded chunked decoding and
/// incremental CRC32 verification.
pub fn load_volume_index_from<R: Read>(reader: R) -> Result<LoadedIndexData, PersistenceError> {
    load_volume_index_with_len(reader, None)
}

/// Loads and validates an index file from disk.
pub fn load_volume_index(path: &Path) -> Result<LoadedIndexData, PersistenceError> {
    let file = File::open(path)?;
    let actual_file_len = usize::try_from(file.metadata()?.len())
        .map_err(|_| PersistenceError::InvalidPayloadLength)?;
    load_volume_index_with_len(file, Some(actual_file_len))
}

fn read_exact_or_payload_err<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<(), PersistenceError> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            Err(PersistenceError::InvalidPayloadLength)
        }
        Err(err) => Err(PersistenceError::Io(err)),
    }
}

fn read_stream_vec<R: Read>(
    reader: &mut R,
    hasher: &mut Hasher,
    total_len: usize,
    headroom: usize,
    known_file_len: bool,
) -> Result<Vec<u8>, PersistenceError> {
    if known_file_len {
        let mut buf = Vec::with_capacity(total_len.saturating_add(headroom));
        buf.resize(total_len, 0);
        read_exact_or_payload_err(reader, &mut buf)?;
        hasher.update(&buf);
        return Ok(buf);
    }

    let mut out = Vec::with_capacity(total_len.min(STREAM_CHUNK_BYTES));
    let mut chunk = vec![0u8; total_len.min(STREAM_CHUNK_BYTES)];
    let mut remaining = total_len;
    while remaining > 0 {
        let to_read = remaining.min(chunk.len());
        read_exact_or_payload_err(reader, &mut chunk[..to_read])?;
        hasher.update(&chunk[..to_read]);
        out.extend_from_slice(&chunk[..to_read]);
        remaining -= to_read;
    }
    if headroom > 0 && out.capacity().saturating_sub(out.len()) < headroom {
        out.reserve_exact(headroom);
    }
    let target_cap = out.len().saturating_add(headroom);
    if out.capacity() > target_cap {
        out.shrink_to(target_cap);
    }
    Ok(out)
}

fn load_volume_index_with_len<R: Read>(
    mut reader: R,
    actual_file_len: Option<usize>,
) -> Result<LoadedIndexData, PersistenceError> {
    let mut header_bytes = [0u8; HEADER_SIZE];
    read_exact_or_payload_err(&mut reader, &mut header_bytes)?;
    let header = IndexHeader::decode(&header_bytes).map_err(|error| match error {
        IndexFileError::InvalidMagic => PersistenceError::InvalidMagic,
        IndexFileError::UnsupportedVersion(version) => {
            PersistenceError::UnsupportedVersion(version)
        }
    })?;

    let record_count = header.record_count as usize;
    let records_byte_len = record_count
        .checked_mul(RECORD_SIZE)
        .ok_or(PersistenceError::InvalidPayloadLength)?;
    let arena_len = header.arena_len as usize;
    let folded_len = header.folded_len as usize;
    let expected_file_len = HEADER_SIZE
        .checked_add(records_byte_len)
        .and_then(|n| n.checked_add(arena_len))
        .and_then(|n| n.checked_add(folded_len))
        .ok_or(PersistenceError::InvalidPayloadLength)?;
    if let Some(actual_len) = actual_file_len {
        if actual_len != expected_file_len {
            return Err(PersistenceError::InvalidPayloadLength);
        }
    }
    if arena_len != folded_len {
        return Err(PersistenceError::InvalidPayloadLength);
    }

    let mut hasher = Hasher::new();
    let rec_headroom = headroom_for_records(record_count);
    let known_file_len = actual_file_len.is_some();
    let initial_rec_cap = if known_file_len {
        record_count.saturating_add(rec_headroom)
    } else {
        record_count.min(DECODE_CHUNK_RECORDS)
    };
    let mut records = Vec::with_capacity(initial_rec_cap);

    let chunk_cap = record_count.min(DECODE_CHUNK_RECORDS);
    let mut chunk_buf = vec![0u8; chunk_cap * RECORD_SIZE];
    let mut remaining_records = record_count;
    while remaining_records > 0 {
        let batch = remaining_records.min(DECODE_CHUNK_RECORDS);
        let batch_bytes = batch * RECORD_SIZE;
        read_exact_or_payload_err(&mut reader, &mut chunk_buf[..batch_bytes])?;
        hasher.update(&chunk_buf[..batch_bytes]);
        records.reserve(batch);
        let (chunks, remainder) = chunk_buf[..batch_bytes].as_chunks::<RECORD_SIZE>();
        debug_assert!(
            remainder.is_empty(),
            "record chunk buffer is a whole number of records"
        );
        for chunk in chunks {
            records.push(decode_record(chunk));
        }
        remaining_records -= batch;
    }
    drop(chunk_buf);

    if !known_file_len {
        if rec_headroom > 0 && records.capacity().saturating_sub(records.len()) < rec_headroom {
            records.reserve_exact(rec_headroom);
        }
        let target_cap = records.len().saturating_add(rec_headroom);
        if records.capacity() > target_cap {
            records.shrink_to(target_cap);
        }
    }

    let arena_headroom = headroom_for_arena(arena_len);
    let arena_buf = read_stream_vec(
        &mut reader,
        &mut hasher,
        arena_len,
        arena_headroom,
        known_file_len,
    )?;
    let folded_buf = read_stream_vec(
        &mut reader,
        &mut hasher,
        folded_len,
        arena_headroom,
        known_file_len,
    )?;

    if !known_file_len {
        let mut trailing = [0u8; 1];
        if reader.read(&mut trailing)? != 0 {
            return Err(PersistenceError::InvalidPayloadLength);
        }
    }

    if hasher.finalize() != header.checksum {
        return Err(PersistenceError::ChecksumMismatch);
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

    #[test]
    fn streaming_reader_roundtrips_and_guards_checksum_truncation_and_headroom() {
        let temp = tempdir().unwrap();
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        for i in 0..1_000u64 {
            let (off, len, ext) = pool.insert(&format!("entry_{i:04}.dat"));
            records.push(FileRecord {
                frn: 100 + i,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: off,
                name_len: len,
                ext_offset: ext,
                volume_id: 2,
                flags: 0,
                size: 512,
                modified_ts: 10,
                created_ts: 10,
            });
        }

        let saved_path =
            save_volume_index_atomic(temp.path(), 'C', 42, 88, 1234, &records, &pool, true)
                .expect("save should succeed");
        let raw = fs::read(&saved_path).expect("read saved index");

        let loaded = load_volume_index_from(std::io::Cursor::new(&raw)).expect("stream load");
        assert_eq!(loaded.records.len(), 1_000);
        assert!(
            loaded.records.capacity() > 1_000 && loaded.records.capacity() <= 1_100,
            "loaded records must retain controlled ~3% headroom, got {}",
            loaded.records.capacity()
        );
        let (arena_cap, folded_cap) = loaded.arena.allocated_bytes();
        assert!(
            arena_cap > loaded.arena.len() && folded_cap > loaded.arena.len(),
            "loaded NamePool must retain controlled headroom"
        );

        let mut bad_crc = raw.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 0xff;
        assert!(matches!(
            load_volume_index_from(std::io::Cursor::new(&bad_crc)),
            Err(PersistenceError::ChecksumMismatch)
        ));

        let mut with_trailing = raw.clone();
        with_trailing.push(0);
        assert!(matches!(
            load_volume_index_from(std::io::Cursor::new(&with_trailing)),
            Err(PersistenceError::InvalidPayloadLength)
        ));

        let truncated = &raw[..raw.len() - 5];
        assert!(load_volume_index_from(std::io::Cursor::new(truncated)).is_err());
    }
}

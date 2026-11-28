// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fast atomic index persistence and verification for Cairn (IDX-04, NFR-09). The byte layout
//! belongs to `cairn_core::index_file`; this module owns the file IO and the atomic replace.

use cairn_core::arena::NamePool;
use cairn_core::index_file::{
    HEADER_SIZE, IndexFileError, IndexHeader, RECORD_SIZE, decode_record, encode_record,
};
use cairn_core::model::FileRecord;
use crc32fast::Hasher;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) struct TempFileGuard {
    path: Option<PathBuf>,
}

impl TempFileGuard {
    pub(crate) fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn create_unique_temp_file(
    dir: &Path,
    stem: &str,
) -> Result<(PathBuf, File, TempFileGuard), std::io::Error> {
    let pid = std::process::id();
    for _ in 0..64 {
        let seq = TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = dir.join(format!("{stem}.{pid}.{seq}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => {
                let guard = TempFileGuard {
                    path: Some(tmp_path.clone()),
                };
                return Ok((tmp_path, file, guard));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "exhausted attempts to allocate unique temporary file",
    ))
}

pub(crate) fn atomic_write_bytes(target: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    let stem = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cairn");
    let (tmp_path, mut file, mut guard) = create_unique_temp_file(parent, stem)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    atomic_replace_file(&tmp_path, target)?;
    guard.disarm();
    Ok(())
}

#[derive(Error, Debug)]
pub enum PersistenceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid index file magic signature")]
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

#[derive(Clone, Copy)]
pub struct VolumeSnapshot<'a> {
    pub drive_letter: char,
    pub volume_serial: u64,
    pub journal_id: u64,
    pub last_usn: i64,
    pub records: &'a [FileRecord],
    pub arena: &'a NamePool,
    pub directory_sizes_valid: bool,
}

const DECODE_CHUNK_RECORDS: usize = 8_192;
const STREAM_CHUNK_BYTES: usize = DECODE_CHUNK_RECORDS * RECORD_SIZE;

pub(crate) fn headroom_for_records(len: usize) -> usize {
    if len == 0 { 0 } else { (len / 32).max(64) }
}

pub(crate) fn headroom_for_arena(len: usize) -> usize {
    if len == 0 { 0 } else { (len / 32).max(2_048) }
}

/// Atomically saves volume index records and arena to `vol_<drive>.idx`.
pub fn save_volume_index_atomic(
    dir: &Path,
    snapshot: &VolumeSnapshot<'_>,
) -> Result<PathBuf, PersistenceError> {
    fs::create_dir_all(dir)?;

    let file_name = format!("vol_{}.idx", snapshot.drive_letter.to_ascii_uppercase());
    let target_path = dir.join(&file_name);
    let (tmp_path, raw_file, mut guard) = create_unique_temp_file(dir, &file_name)?;

    let (arena_bytes, folded_bytes) = snapshot.arena.raw_parts();
    let record_count = u32::try_from(snapshot.records.len())
        .map_err(|_| PersistenceError::InvalidPayloadLength)?;
    let arena_len =
        u32::try_from(arena_bytes.len()).map_err(|_| PersistenceError::InvalidPayloadLength)?;
    let folded_len =
        u32::try_from(folded_bytes.len()).map_err(|_| PersistenceError::InvalidPayloadLength)?;

    let mut hasher = Hasher::new();
    let mut record_buf = [0u8; RECORD_SIZE];
    for record in snapshot.records {
        encode_record(record, &mut record_buf);
        hasher.update(&record_buf);
    }
    hasher.update(arena_bytes);
    hasher.update(folded_bytes);
    let checksum = hasher.finalize();

    let header = IndexHeader {
        drive_letter: snapshot.drive_letter,
        directory_sizes_valid: snapshot.directory_sizes_valid,
        volume_serial: snapshot.volume_serial,
        journal_id: snapshot.journal_id,
        last_usn: snapshot.last_usn,
        record_count,
        arena_len,
        folded_len,
        checksum,
    }
    .encode();

    {
        let mut writer = BufWriter::with_capacity(1 << 20, raw_file);
        writer.write_all(&header)?;
        for record in snapshot.records {
            encode_record(record, &mut record_buf);
            writer.write_all(&record_buf)?;
        }
        writer.write_all(arena_bytes)?;
        writer.write_all(folded_bytes)?;
        let file = writer
            .into_inner()
            .map_err(|err| PersistenceError::Io(err.into_error()))?;
        file.sync_all()?;
    }

    atomic_replace_file(&tmp_path, &target_path)?;
    guard.disarm();

    Ok(target_path)
}

fn volume_guid_sidecar_path(dir: &Path, drive_letter: char) -> PathBuf {
    dir.join(format!("vol_{}.guid", drive_letter.to_ascii_uppercase()))
}

pub(crate) fn save_volume_guid_sidecar(
    dir: &Path,
    drive_letter: char,
    volume_guid: &str,
) -> Result<(), std::io::Error> {
    let trimmed = volume_guid.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let target_path = volume_guid_sidecar_path(dir, drive_letter);
    atomic_write_bytes(&target_path, trimmed.as_bytes())
}

pub(crate) fn load_volume_guid_sidecar(dir: &Path, drive_letter: char) -> Option<String> {
    let raw = fs::read_to_string(volume_guid_sidecar_path(dir, drive_letter)).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
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
    if let Some(actual_len) = actual_file_len
        && actual_len != expected_file_len
    {
        return Err(PersistenceError::InvalidPayloadLength);
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

pub(crate) fn atomic_replace_file(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        use windows::core::PCWSTR;

        const MAX_REPLACE_ATTEMPTS: usize = 20;

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

        for attempt in 0..MAX_REPLACE_ATTEMPTS {
            // SAFETY: `wide_src` and `wide_dst` are NUL-terminated UTF-16 paths valid for the call.
            let res = unsafe {
                MoveFileExW(
                    PCWSTR(wide_src.as_ptr()),
                    PCWSTR(wide_dst.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            match res {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let last = std::io::Error::last_os_error();
                    let os_err = if last.raw_os_error().is_some_and(|code| code != 0) {
                        last
                    } else {
                        std::io::Error::other(err.to_string())
                    };
                    let transient = matches!(os_err.raw_os_error(), Some(5 | 32))
                        && !fs::symlink_metadata(dst).is_ok_and(|meta| meta.is_dir());
                    if transient && attempt + 1 < MAX_REPLACE_ATTEMPTS {
                        if attempt < 4 {
                            std::thread::yield_now();
                        } else {
                            let delay_ms = (((attempt - 3) as u64) * 5).min(50);
                            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        }
                        continue;
                    }
                    return Err(os_err);
                }
            }
        }
        unreachable!("loop returns on final attempt")
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
            &VolumeSnapshot {
                drive_letter: 'C',
                volume_serial: 0x12345678,
                journal_id: 999,
                last_usn: 50000,
                records: &records,
                arena: &pool,
                directory_sizes_valid: true,
            },
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

        let saved_path = save_volume_index_atomic(
            temp.path(),
            &VolumeSnapshot {
                drive_letter: 'D',
                volume_serial: 7,
                journal_id: 8,
                last_usn: 9,
                records: &[],
                arena: &pool,
                directory_sizes_valid: false,
            },
        )
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

        let saved_path = save_volume_index_atomic(
            temp.path(),
            &VolumeSnapshot {
                drive_letter: 'E',
                volume_serial: 1,
                journal_id: 2,
                last_usn: 3,
                records: &records,
                arena: &pool,
                directory_sizes_valid: true,
            },
        )
        .expect("save should succeed");
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
        let saved_path = save_volume_index_atomic(
            temp.path(),
            &VolumeSnapshot {
                drive_letter: 'C',
                volume_serial: 1,
                journal_id: 2,
                last_usn: 3,
                records: &[],
                arena: &pool,
                directory_sizes_valid: true,
            },
        )
        .expect("save should succeed");

        let mut bytes = fs::read(&saved_path).expect("read cache");
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

        let saved_path = save_volume_index_atomic(
            temp.path(),
            &VolumeSnapshot {
                drive_letter: 'C',
                volume_serial: 42,
                journal_id: 88,
                last_usn: 1234,
                records: &records,
                arena: &pool,
                directory_sizes_valid: true,
            },
        )
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

    #[test]
    fn concurrent_atomic_writes_leave_no_temp_files_and_preserve_target_on_error() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("vol_C.guid");

        std::thread::scope(|scope| {
            for idx in 0..8 {
                let target_ref = &target;
                scope.spawn(move || {
                    for iter in 0..8 {
                        let payload = format!("guid-{idx}-{iter}");
                        atomic_write_bytes(target_ref, payload.as_bytes()).unwrap();
                    }
                });
            }
        });

        let final_content = fs::read_to_string(&target).unwrap();
        assert!(final_content.starts_with("guid-"));

        let dir_target = temp.path().join("blocked");
        fs::create_dir_all(&dir_target).unwrap();
        let sentinel = dir_target.join("keep.txt");
        fs::write(&sentinel, b"intact").unwrap();
        assert!(atomic_write_bytes(&dir_target, b"overwrite").is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"intact");

        let leftover_temps: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftover_temps.is_empty(),
            "unexpected leftover temp files: {leftover_temps:?}"
        );
    }
}

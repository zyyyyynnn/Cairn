// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use cairn_core::{
    FLAG_ARCHIVE, FLAG_DIRECTORY, FLAG_HIDDEN, FLAG_READONLY, FLAG_SYSTEM, WIN32_ATTR_ARCHIVE,
    WIN32_ATTR_HIDDEN, WIN32_ATTR_READONLY, WIN32_ATTR_SYSTEM,
};
use cairn_ntfs::attribute::{ATTR_BITMAP, ATTR_DATA, ATTR_FILE_NAME, ATTR_STANDARD_INFORMATION};
use cairn_ntfs::mft_reader::{
    BlockReader, MftScanOptions, SliceBlockReader, scan_mft_from_file, scan_mft_from_reader,
};
use cairn_ntfs::volume_identity::{enumerate_volumes, parse_volume_path_names};
use std::sync::Mutex;

const RECORD_SIZE: usize = 1024;
const SECTOR_SIZE: usize = 512;

fn seal_record_usa(rec: &mut [u8], usn: u16) {
    rec[4..6].copy_from_slice(&48u16.to_le_bytes());
    rec[6..8].copy_from_slice(&3u16.to_le_bytes());
    rec[48..50].copy_from_slice(&usn.to_le_bytes());
    rec.copy_within(510..512, 50);
    rec.copy_within(1022..1024, 52);
    rec[510..512].copy_from_slice(&usn.to_le_bytes());
    rec[1022..1024].copy_from_slice(&usn.to_le_bytes());
}

fn push_resident_attr(rec: &mut [u8], cursor: &mut usize, type_id: u32, payload: &[u8]) {
    let total_len = ((24 + payload.len() + 7) & !7).max(24);
    let pos = *cursor;
    rec[pos..pos + 4].copy_from_slice(&type_id.to_le_bytes());
    rec[pos + 4..pos + 8].copy_from_slice(&(total_len as u32).to_le_bytes());
    rec[pos + 8] = 0; // resident
    rec[pos + 16..pos + 20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    rec[pos + 20..pos + 22].copy_from_slice(&24u16.to_le_bytes());
    rec[pos + 24..pos + 24 + payload.len()].copy_from_slice(payload);
    *cursor += total_len;
}

fn push_non_resident_runlist_attr(
    rec: &mut [u8],
    cursor: &mut usize,
    type_id: u32,
    real_size: u64,
    runlist: &[u8],
) {
    let total_len = ((64 + runlist.len() + 7) & !7).max(72);
    let pos = *cursor;
    rec[pos..pos + 4].copy_from_slice(&type_id.to_le_bytes());
    rec[pos + 4..pos + 8].copy_from_slice(&(total_len as u32).to_le_bytes());
    rec[pos + 8] = 1; // non-resident
    rec[pos + 32..pos + 34].copy_from_slice(&64u16.to_le_bytes());
    rec[pos + 40..pos + 48].copy_from_slice(&real_size.to_le_bytes());
    rec[pos + 48..pos + 56].copy_from_slice(&real_size.to_le_bytes());
    rec[pos + 64..pos + 64 + runlist.len()].copy_from_slice(runlist);
    *cursor += total_len;
}

struct FileRecordSpec<'a> {
    rec_num: u32,
    seq: u16,
    is_dir: bool,
    si_dos_flags: u32,
    fn_flags: u32,
    parent_frn: u64,
    name: &'a str,
    size: u64,
}

fn build_file_record(spec: FileRecordSpec<'_>) -> Vec<u8> {
    let mut rec = vec![0u8; RECORD_SIZE];
    rec[0..4].copy_from_slice(b"FILE");
    rec[0x10..0x12].copy_from_slice(&spec.seq.to_le_bytes());
    rec[0x14..0x16].copy_from_slice(&56u16.to_le_bytes());
    let mut flags = 0x0001u16; // in use
    if spec.is_dir {
        flags |= 0x0002;
    }
    rec[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
    rec[0x1C..0x20].copy_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
    rec[0x2C..0x30].copy_from_slice(&spec.rec_num.to_le_bytes());

    let mut cursor = 56usize;

    let mut si = [0u8; 48];
    si[0..8].copy_from_slice(&1000i64.to_le_bytes());
    si[8..16].copy_from_slice(&2000i64.to_le_bytes());
    si[32..36].copy_from_slice(&spec.si_dos_flags.to_le_bytes());
    push_resident_attr(&mut rec, &mut cursor, ATTR_STANDARD_INFORMATION, &si);

    let utf16: Vec<u16> = spec.name.encode_utf16().collect();
    let mut fn_buf = vec![0u8; 66 + utf16.len() * 2];
    fn_buf[0..8].copy_from_slice(&spec.parent_frn.to_le_bytes());
    fn_buf[8..16].copy_from_slice(&900i64.to_le_bytes());
    fn_buf[16..24].copy_from_slice(&1900i64.to_le_bytes());
    fn_buf[56..60].copy_from_slice(&spec.fn_flags.to_le_bytes());
    fn_buf[64] = utf16.len() as u8;
    fn_buf[65] = 0x01; // Win32 namespace
    for (i, ch) in utf16.iter().enumerate() {
        fn_buf[66 + i * 2..66 + i * 2 + 2].copy_from_slice(&ch.to_le_bytes());
    }
    push_resident_attr(&mut rec, &mut cursor, ATTR_FILE_NAME, &fn_buf);

    if !spec.is_dir {
        let data_payload = vec![b'x'; spec.size.min(64) as usize];
        push_resident_attr(&mut rec, &mut cursor, ATTR_DATA, &data_payload);
    }

    rec[cursor..cursor + 4].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    seal_record_usa(&mut rec, 0x1337);
    rec
}

struct TrackingReader<'a> {
    data: &'a [u8],
    reads: Mutex<Vec<(u64, usize)>>,
}

impl<'a> TrackingReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            reads: Mutex::new(Vec::new()),
        }
    }

    fn read_offsets(&self) -> Vec<(u64, usize)> {
        self.reads.lock().unwrap().clone()
    }
}

impl BlockReader for TrackingReader<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        self.reads.lock().unwrap().push((offset, buf.len()));
        if offset as usize >= self.data.len() {
            return Ok(0);
        }
        let avail = self.data.len() - offset as usize;
        let n = buf.len().min(avail);
        buf[..n].copy_from_slice(&self.data[offset as usize..offset as usize + n]);
        Ok(n)
    }
}

#[test]
fn volume_image_skips_bitmap_zero_chunks_and_extracts_hidden_system_flags() {
    // Cluster size 4096 B (4 records per cluster). Reserve 64 records = 16 clusters starting at LCN 4.
    let mft_lcn: usize = 4;
    let mft_offset = mft_lcn * 4096;
    let total_clusters = 64;
    let mut disk = vec![0u8; total_clusters * 4096];

    // Boot sector
    disk[3..11].copy_from_slice(b"NTFS    ");
    disk[0x0B..0x0D].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    disk[0x0D] = 8;
    disk[0x40] = 0xF6; // -10 => 1024 B/record
    disk[0x30..0x38].copy_from_slice(&(mft_lcn as i64).to_le_bytes());

    // Record 0 ($MFT): $DATA at LCN 4 for 16 clusters (64 records), plus non-resident $BITMAP at LCN 2.
    let mut rec0 = vec![0u8; RECORD_SIZE];
    rec0[0..4].copy_from_slice(b"FILE");
    rec0[0x14..0x16].copy_from_slice(&56u16.to_le_bytes());
    rec0[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
    let mut cursor = 56usize;
    // Non-resident $DATA: length 16 clusters (0x10) at LCN 4 => [0x11, 0x10, 0x04, 0x00]
    push_non_resident_runlist_attr(
        &mut rec0,
        &mut cursor,
        ATTR_DATA,
        (64 * RECORD_SIZE) as u64,
        &[0x11, 0x10, 0x04, 0x00],
    );
    // Non-resident $BITMAP: length 1 cluster at LCN 2 => [0x11, 0x01, 0x02, 0x00]
    push_non_resident_runlist_attr(
        &mut rec0,
        &mut cursor,
        ATTR_BITMAP,
        8,
        &[0x11, 0x01, 0x02, 0x00],
    );
    rec0[cursor..cursor + 4].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    seal_record_usa(&mut rec0, 0xAA55);
    disk[mft_offset..mft_offset + RECORD_SIZE].copy_from_slice(&rec0);

    // $BITMAP at LCN 2: mark bits 0 (rec 0), 5 (rec 5 root), 24, 25, and 60 allocated. Leave 26..=59 as 0!
    let bitmap_offset = 2 * 4096;
    disk[bitmap_offset] = 0b0010_0001; // bits 0, 5
    disk[bitmap_offset + 3] = 0b0000_0011; // bits 24, 25 (bit 26 = 0!)
    disk[bitmap_offset + 7] = 0b0001_0000; // bit 60

    // Write records:
    // Record 5 (root `.`): must stay skipped when skip_system_records=true
    let rec5 = build_file_record(FileRecordSpec {
        rec_num: 5,
        seq: 5,
        is_dir: true,
        si_dos_flags: 0,
        fn_flags: 0,
        parent_frn: 5,
        name: ".",
        size: 0,
    });
    disk[mft_offset + 5 * RECORD_SIZE..mft_offset + 6 * RECORD_SIZE].copy_from_slice(&rec5);

    // Record 24: HIDDEN + READONLY in $STANDARD_INFORMATION (even though $FILE_NAME has 0)
    let rec24 = build_file_record(FileRecordSpec {
        rec_num: 24,
        seq: 1,
        is_dir: false,
        si_dos_flags: WIN32_ATTR_HIDDEN | WIN32_ATTR_READONLY,
        fn_flags: 0,
        parent_frn: 5,
        name: "secret.txt",
        size: 42,
    });
    disk[mft_offset + 24 * RECORD_SIZE..mft_offset + 25 * RECORD_SIZE].copy_from_slice(&rec24);

    // Record 25: SYSTEM + ARCHIVE directory
    let rec25 = build_file_record(FileRecordSpec {
        rec_num: 25,
        seq: 1,
        is_dir: true,
        si_dos_flags: WIN32_ATTR_SYSTEM | WIN32_ATTR_ARCHIVE,
        fn_flags: 0,
        parent_frn: 5,
        name: "System Volume Information",
        size: 0,
    });
    disk[mft_offset + 25 * RECORD_SIZE..mft_offset + 26 * RECORD_SIZE].copy_from_slice(&rec25);

    // Record 26: stale valid FILE signature on disk, but bitmap bit 26 is 0 -> MUST BE SKIPPED
    let rec26_stale = build_file_record(FileRecordSpec {
        rec_num: 26,
        seq: 9,
        is_dir: false,
        si_dos_flags: 0,
        fn_flags: 0,
        parent_frn: 5,
        name: "deleted-ghost.txt",
        size: 12,
    });
    disk[mft_offset + 26 * RECORD_SIZE..mft_offset + 27 * RECORD_SIZE]
        .copy_from_slice(&rec26_stale);

    // Record 60: allocated far after a chunk of all-zero bitmap bits (records 32..=55)
    let rec60 = build_file_record(FileRecordSpec {
        rec_num: 60,
        seq: 1,
        is_dir: false,
        si_dos_flags: WIN32_ATTR_ARCHIVE,
        fn_flags: 0,
        parent_frn: 5,
        name: "tail.log",
        size: 7,
    });
    disk[mft_offset + 60 * RECORD_SIZE..mft_offset + 61 * RECORD_SIZE].copy_from_slice(&rec60);

    // Use tiny 8-record chunks (8192 B) so records 32..=55 form entire zero-bitmap chunks skipped without read_at!
    let mut reader = TrackingReader::new(&disk);
    let result = scan_mft_from_reader(
        &mut reader,
        MftScanOptions {
            chunk_records: 8,
            skip_system_records: true,
        },
    )
    .expect("scan synthetic volume");

    let names: Vec<&str> = result.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["secret.txt", "System Volume Information", "tail.log"]
    );

    let secret = &result.items[0];
    assert_eq!(
        secret.flags & (FLAG_HIDDEN | FLAG_READONLY),
        FLAG_HIDDEN | FLAG_READONLY
    );
    assert_eq!(secret.size, 42);

    let sysdir = &result.items[1];
    assert_eq!(
        sysdir.flags & (FLAG_DIRECTORY | FLAG_SYSTEM | FLAG_ARCHIVE),
        FLAG_DIRECTORY | FLAG_SYSTEM | FLAG_ARCHIVE
    );

    assert!(result.stats.bitmap_skipped_records >= 30);

    // Chunk 4 (records 32..40), chunk 5 (40..48), chunk 6 (48..56) have zero bitmap bits -> zero I/O!
    let offsets = reader.read_offsets();
    let chunk4_off = (mft_offset + 32 * RECORD_SIZE) as u64;
    let chunk5_off = (mft_offset + 40 * RECORD_SIZE) as u64;
    let chunk6_off = (mft_offset + 48 * RECORD_SIZE) as u64;
    assert!(!offsets.iter().any(|(o, _)| *o == chunk4_off));
    assert!(!offsets.iter().any(|(o, _)| *o == chunk5_off));
    assert!(!offsets.iter().any(|(o, _)| *o == chunk6_off));
}

#[test]
fn standalone_mft_image_and_file_build_without_boot_sector() {
    // 26 sequential records packed directly as an offline $MFT dump (IDX-10)
    let mut mft_bytes = vec![0u8; 26 * RECORD_SIZE];
    let mut rec0 = vec![0u8; RECORD_SIZE];
    rec0[0..4].copy_from_slice(b"FILE");
    rec0[0x14..0x16].copy_from_slice(&56u16.to_le_bytes());
    rec0[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
    rec0[0x1C..0x20].copy_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
    let mut cursor = 56usize;
    let bitmap = [0b0000_0001u8, 0, 0, 0b0000_0010u8]; // bit 0 & bit 25 only
    push_resident_attr(&mut rec0, &mut cursor, ATTR_BITMAP, &bitmap);
    rec0[cursor..cursor + 4].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    seal_record_usa(&mut rec0, 0x0042);
    mft_bytes[0..RECORD_SIZE].copy_from_slice(&rec0);

    let rec24_ignored = build_file_record(FileRecordSpec {
        rec_num: 24,
        seq: 1,
        is_dir: false,
        si_dos_flags: 0,
        fn_flags: 0,
        parent_frn: 5,
        name: "ignored.txt",
        size: 1,
    });
    mft_bytes[24 * RECORD_SIZE..25 * RECORD_SIZE].copy_from_slice(&rec24_ignored);
    let rec25 = build_file_record(FileRecordSpec {
        rec_num: 25,
        seq: 2,
        is_dir: false,
        si_dos_flags: WIN32_ATTR_HIDDEN | WIN32_ATTR_SYSTEM,
        fn_flags: 0,
        parent_frn: 5,
        name: "kept.sys",
        size: 64,
    });
    mft_bytes[25 * RECORD_SIZE..26 * RECORD_SIZE].copy_from_slice(&rec25);

    let mut slice_reader = SliceBlockReader::new(&mft_bytes);
    let from_slice = scan_mft_from_reader(&mut slice_reader, MftScanOptions::default())
        .expect("standalone slice image");
    assert_eq!(from_slice.items.len(), 1);
    assert_eq!(from_slice.items[0].name, "kept.sys");
    assert_eq!(
        from_slice.items[0].flags & (FLAG_HIDDEN | FLAG_SYSTEM),
        FLAG_HIDDEN | FLAG_SYSTEM
    );

    let tmp = tempfile::NamedTempFile::new().expect("temp mft image");
    std::fs::write(tmp.path(), &mft_bytes).expect("write image");
    let from_file = scan_mft_from_file(tmp.path(), MftScanOptions::default()).expect("scan file");
    assert_eq!(from_file.items.len(), 1);
    assert_eq!(from_file.items[0].frn, from_slice.items[0].frn);
}

#[test]
fn volume_identity_parses_multi_sz_and_enumerates_real_guids() {
    let raw: Vec<u16> = "C:\\\0C:\\Mounts\\Data\\\0\0".encode_utf16().collect();
    assert_eq!(
        parse_volume_path_names(&raw),
        vec!["C:\\".to_string(), "C:\\Mounts\\Data\\".to_string()]
    );

    let vols = enumerate_volumes();
    assert!(!vols.is_empty());
    #[cfg(windows)]
    {
        for vol in &vols {
            assert!(
                vol.volume_guid.starts_with(r"\\?\Volume{"),
                "expected real Win32 Volume GUID, got {:?}",
                vol.volume_guid
            );
            assert!(!vol.mount_point.is_empty());
        }
        assert!(vols.iter().any(|v| v.drive_letter == Some('C')));
    }
}

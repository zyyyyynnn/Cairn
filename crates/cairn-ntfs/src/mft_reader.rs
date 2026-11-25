// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Block-reader MFT scanner supporting full NTFS volumes, `$BITMAP` zero-window skipping, and
//! standalone offline `$MFT` images.

use crate::attribute::{AttributeParseError, NonResidentAttrSpan, parse_attributes};
use crate::boot_sector::{BootSectorError, NtfsBootSector};
use crate::fixup::apply_fixup;
use crate::mft::{ExtractedItem, MftError, parse_mft_record};
use crate::runlist::{DataRun, RunlistError, parse_runlist};
use crate::volume::VolumeError;
use rayon::prelude::*;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_BITMAP_BYTES: usize = 256 * 1024 * 1024;
const STANDALONE_DEFAULT_RECORD_SIZE: usize = 1024;
const STANDALONE_DEFAULT_SECTOR_SIZE: usize = 512;

/// Random-access block reader abstraction used across live volumes, offline images, and tests.
pub trait BlockReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error>;

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// In-memory [`BlockReader`] backed by a byte slice.
pub struct SliceBlockReader<'a> {
    data: &'a [u8],
}

impl<'a> SliceBlockReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }
}

impl BlockReader for SliceBlockReader<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        if start >= self.data.len() {
            return Ok(0);
        }
        let avail = self.data.len() - start;
        let n = buf.len().min(avail);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        Ok(n)
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

/// File-backed [`BlockReader`] for offline NTFS disk images and standalone `$MFT` extracts.
pub struct FileBlockReader {
    file: File,
    len: u64,
}

impl FileBlockReader {
    pub fn open(path: &Path) -> Result<Self, VolumeError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl BlockReader for FileBlockReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buf)
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Controls chunk sizing and system-record filtering during `$MFT` scanning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MftScanOptions {
    pub chunk_records: usize,
    pub skip_system_records: bool,
}

impl Default for MftScanOptions {
    fn default() -> Self {
        Self {
            chunk_records: 4096,
            skip_system_records: true,
        }
    }
}

impl MftScanOptions {
    pub fn with_chunk_records(mut self, chunk_records: usize) -> Self {
        self.chunk_records = chunk_records.max(1);
        self
    }
}

/// Accounting metrics emitted by [`scan_mft_from_reader`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MftScanStats {
    pub bitmap_skipped_records: u64,
}

/// Extracted `$MFT` records together with scan statistics.
#[derive(Clone, Debug, Default)]
pub struct MftScanResult {
    pub items: Vec<ExtractedItem>,
    pub stats: MftScanStats,
}

struct MftLayout {
    record_size: usize,
    sector_size: usize,
    bitmap: Option<Vec<u8>>,
    source: MftSourceLayout,
}

enum MftSourceLayout {
    VolumeRuns {
        bytes_per_cluster: u64,
        runs: Vec<DataRun>,
        total_records_cap: Option<u64>,
    },
    StandaloneLinear {
        known_records: Option<u64>,
    },
}

fn read_at_exact_or_eof<R: BlockReader + ?Sized>(
    reader: &mut R,
    mut offset: u64,
    buf: &mut [u8],
) -> Result<usize, std::io::Error> {
    let mut total = 0usize;
    while total < buf.len() {
        let n = reader.read_at(offset, &mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
        offset = offset.saturating_add(n as u64);
    }
    Ok(total)
}

#[inline]
fn bitmap_has_bit(bits: &[u8], rec_num: u64) -> bool {
    let byte_idx = (rec_num / 8) as usize;
    byte_idx < bits.len() && (bits[byte_idx] & (1u8 << (rec_num % 8))) != 0
}

#[inline]
fn should_parse_record(rec_num: u64, bitmap: Option<&[u8]>, skip_system_records: bool) -> bool {
    if skip_system_records && rec_num < 24 {
        return false;
    }
    bitmap.is_none_or(|bits| bitmap_has_bit(bits, rec_num))
}

fn read_non_resident_bitmap<R: BlockReader + ?Sized>(
    reader: &mut R,
    bytes_per_cluster: u64,
    record_buf: &[u8],
    span: &NonResidentAttrSpan,
) -> Result<Vec<u8>, VolumeError> {
    let Some(runlist_start) = span.attr_offset.checked_add(span.runlist_offset) else {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    };
    let Some(attr_end) = span.attr_offset.checked_add(span.attr_len) else {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    };
    if span.runlist_offset > span.attr_len || attr_end > record_buf.len() {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    }
    let runlist_bytes = &record_buf[runlist_start..attr_end];
    let runs = parse_runlist(runlist_bytes, 0)?;
    let target_len = usize::try_from(span.real_size)
        .unwrap_or(MAX_BITMAP_BYTES)
        .min(MAX_BITMAP_BYTES);
    let mut out = vec![0u8; target_len];

    // Stage reads at cluster granularity so raw Win32 volume handles satisfy sector alignment
    // even when `$BITMAP` real_size is smaller than a sector (for example 8 bytes).
    let cluster_len = usize::try_from(bytes_per_cluster).unwrap_or(4096).max(512);
    let chunk_cap = ((64 * 1024) / cluster_len).max(1) * cluster_len;
    let mut staging = vec![0u8; chunk_cap];
    let mut written = 0usize;

    for run in runs {
        if written >= target_len {
            break;
        }
        let run_bytes_total = run.cluster_count.saturating_mul(bytes_per_cluster);
        let run_needed =
            (target_len - written).min(usize::try_from(run_bytes_total).unwrap_or(usize::MAX));
        match run.lcn {
            None => {
                // Sparse bitmap region stays zeroed.
                written += run_needed;
            }
            Some(lcn) => {
                if lcn < 0 {
                    return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
                }
                let mut offset = (lcn as u64).saturating_mul(bytes_per_cluster);
                let mut remaining_in_run = run_needed;
                while remaining_in_run > 0 {
                    let clusters_for_rem =
                        (remaining_in_run.div_ceil(cluster_len) * cluster_len).min(chunk_cap);
                    let read_bytes =
                        read_at_exact_or_eof(reader, offset, &mut staging[..clusters_for_rem])?;
                    if read_bytes == 0 {
                        break;
                    }
                    let take = remaining_in_run.min(read_bytes);
                    out[written..written + take].copy_from_slice(&staging[..take]);
                    written += take;
                    remaining_in_run -= take;
                    offset = offset.saturating_add(read_bytes as u64);
                }
            }
        }
    }

    Ok(out)
}

fn parse_record0_data_runs(
    record0_buf: &[u8],
    data_offset: usize,
    rec_size: usize,
) -> Result<Vec<DataRun>, VolumeError> {
    if data_offset
        .checked_add(34)
        .is_none_or(|end| end > record0_buf.len())
    {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    }
    let runlist_offset =
        u16::from_le_bytes([record0_buf[data_offset + 32], record0_buf[data_offset + 33]]) as usize;
    let attr_len = u32::from_le_bytes(
        record0_buf[data_offset + 4..data_offset + 8]
            .try_into()
            .unwrap(),
    ) as usize;

    let Some(runlist_start) = data_offset.checked_add(runlist_offset) else {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    };
    let Some(attr_end) = data_offset.checked_add(attr_len) else {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    };
    if runlist_start >= rec_size
        || attr_end > rec_size
        || attr_end > record0_buf.len()
        || runlist_offset >= attr_len
    {
        return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
    }

    let runlist_bytes = &record0_buf[runlist_start..attr_end];
    Ok(parse_runlist(runlist_bytes, 0)?)
}

struct VolumeRuns {
    boot_sector: NtfsBootSector,
    runs: Vec<DataRun>,
    bitmap: Option<Vec<u8>>,
    total_records_cap: Option<u64>,
}

fn locate_volume_runs<R: BlockReader + ?Sized>(
    reader: &mut R,
    boot_sector_bytes: &[u8],
) -> Result<VolumeRuns, VolumeError> {
    let boot_sector = NtfsBootSector::parse(boot_sector_bytes)?;
    let bytes_per_cluster = boot_sector.bytes_per_cluster();
    let rec_size = boot_sector.mft_record_size as usize;
    let sector_size = boot_sector.bytes_per_sector as usize;
    if bytes_per_cluster == 0
        || sector_size == 0
        || rec_size < 256
        || !rec_size.is_power_of_two()
        || boot_sector.mft_start_lcn < 0
    {
        return Err(VolumeError::BootSector(BootSectorError::SectorTooSmall(
            boot_sector_bytes.len(),
        )));
    }

    let mft_start_offset = (boot_sector.mft_start_lcn as u64).saturating_mul(bytes_per_cluster);
    let read_size = rec_size.max(sector_size);
    let mut record0_buf = vec![0u8; read_size];
    let read_n = read_at_exact_or_eof(reader, mft_start_offset, &mut record0_buf)?;
    if read_n < rec_size {
        return Err(VolumeError::Mft(MftError::Attribute(
            AttributeParseError::BufferTooSmall(read_n),
        )));
    }

    apply_fixup(&mut record0_buf[..rec_size], sector_size)?;
    let first_attr_offset = u16::from_le_bytes([record0_buf[0x14], record0_buf[0x15]]) as usize;
    let attrs = parse_attributes(&record0_buf[..rec_size], first_attr_offset)?;

    let data_offset = attrs
        .data_runlist_offset
        .ok_or(VolumeError::Mft(MftError::Attribute(
            AttributeParseError::BufferTooSmall(0),
        )))?;
    let runs = parse_record0_data_runs(&record0_buf, data_offset, rec_size)?;
    let total_records_cap = attrs
        .default_data_size
        .map(|sz| sz / rec_size as u64)
        .filter(|&n| n > 0);

    let bitmap = if let Some(res) = attrs.bitmap_resident {
        Some(res)
    } else if let Some(span) = attrs.bitmap_non_resident.as_ref() {
        Some(read_non_resident_bitmap(
            reader,
            bytes_per_cluster,
            &record0_buf[..rec_size],
            span,
        )?)
    } else {
        None
    };

    Ok(VolumeRuns {
        boot_sector,
        runs,
        bitmap,
        total_records_cap,
    })
}

/// Locates the NTFS `$MFT` boot geometry and data runs from a generic [`BlockReader`].
#[cfg(test)]
pub(crate) fn locate_mft_clusters_from_reader<R: BlockReader + ?Sized>(
    reader: &mut R,
) -> Result<(NtfsBootSector, Vec<DataRun>), VolumeError> {
    let mut boot_sector_buf = [0u8; 4096];
    let head_len = read_at_exact_or_eof(reader, 0, &mut boot_sector_buf)?;
    if head_len < 512 {
        return Err(VolumeError::BootSector(BootSectorError::SectorTooSmall(
            head_len,
        )));
    }
    let found = locate_volume_runs(reader, &boot_sector_buf[..512])?;
    Ok((found.boot_sector, found.runs))
}

fn infer_standalone_geometry(header: &[u8]) -> (usize, usize) {
    let raw_rec = if header.len() >= 0x20 {
        u32::from_le_bytes(header[0x1C..0x20].try_into().unwrap()) as usize
    } else {
        STANDALONE_DEFAULT_RECORD_SIZE
    };
    let record_size = if (512..=16384).contains(&raw_rec) && raw_rec.is_power_of_two() {
        raw_rec
    } else {
        STANDALONE_DEFAULT_RECORD_SIZE
    };

    let usa_count = if header.len() >= 8 {
        u16::from_le_bytes([header[6], header[7]]) as usize
    } else {
        0
    };
    let sector_size = if usa_count > 1 {
        let candidate = record_size / (usa_count - 1);
        if candidate >= 512 && candidate.is_power_of_two() && record_size.is_multiple_of(candidate)
        {
            candidate
        } else {
            STANDALONE_DEFAULT_SECTOR_SIZE
        }
    } else {
        STANDALONE_DEFAULT_SECTOR_SIZE
    };
    (record_size, sector_size)
}

fn detect_mft_layout<R: BlockReader + ?Sized>(reader: &mut R) -> Result<MftLayout, VolumeError> {
    let mut header = [0u8; 4096];
    let head_len = read_at_exact_or_eof(reader, 0, &mut header)?;

    if head_len >= 512 && &header[3..11] == b"NTFS    " {
        let found = locate_volume_runs(reader, &header[..512])?;
        let bytes_per_cluster = found.boot_sector.bytes_per_cluster();
        return Ok(MftLayout {
            record_size: found.boot_sector.mft_record_size as usize,
            sector_size: found.boot_sector.bytes_per_sector as usize,
            bitmap: found.bitmap,
            source: MftSourceLayout::VolumeRuns {
                bytes_per_cluster,
                runs: found.runs,
                total_records_cap: found.total_records_cap,
            },
        });
    }

    if head_len >= 48 && &header[0..4] == b"FILE" {
        let (record_size, sector_size) = infer_standalone_geometry(&header[..head_len]);
        let mut rec0 = vec![0u8; record_size];
        if head_len >= record_size {
            rec0.copy_from_slice(&header[..record_size]);
        } else {
            let n = read_at_exact_or_eof(reader, 0, &mut rec0)?;
            if n < record_size {
                return Err(VolumeError::Mft(MftError::Attribute(
                    AttributeParseError::BufferTooSmall(n),
                )));
            }
        }
        apply_fixup(&mut rec0, sector_size)?;
        let first_attr_offset = u16::from_le_bytes([rec0[0x14], rec0[0x15]]) as usize;
        let attrs = parse_attributes(&rec0, first_attr_offset)?;
        let known_records = reader.byte_len().map(|len| len / record_size as u64);

        return Ok(MftLayout {
            record_size,
            sector_size,
            bitmap: attrs.bitmap_resident,
            source: MftSourceLayout::StandaloneLinear { known_records },
        });
    }

    if head_len < 512 {
        Err(VolumeError::BootSector(BootSectorError::SectorTooSmall(
            head_len,
        )))
    } else {
        // Delegates invalid OEM signature error formatting to NtfsBootSector::parse.
        Err(VolumeError::BootSector(
            NtfsBootSector::parse(&header[..512]).unwrap_err(),
        ))
    }
}

struct ContiguousRange<'a> {
    start_offset: u64,
    start_rec_num: u64,
    num_records: u64,
    record_size: usize,
    sector_size: usize,
    options: MftScanOptions,
    bitmap: Option<&'a [u8]>,
}

fn scan_contiguous_range<R: BlockReader + ?Sized>(
    reader: &mut R,
    range: ContiguousRange<'_>,
    buffer: &mut Vec<u8>,
    result: &mut MftScanResult,
) -> Result<bool, VolumeError> {
    let record_size = range.record_size;
    if record_size == 0 {
        return Err(VolumeError::BootSector(BootSectorError::InvalidGeometry));
    }
    let sector_size = range.sector_size;
    let options = range.options;
    let bitmap = range.bitmap;
    let chunk_records = options.chunk_records.max(1) as u64;
    let chunk_bytes = usize::try_from(chunk_records)
        .ok()
        .and_then(|n| n.checked_mul(record_size))
        .ok_or(VolumeError::BootSector(BootSectorError::InvalidGeometry))?;
    if buffer.len() < chunk_bytes {
        buffer.resize(chunk_bytes, 0);
    }

    let mut remaining = range.num_records;
    let mut rec_cursor = range.start_rec_num;
    let mut offset_cursor = range.start_offset;

    while remaining > 0 {
        let win_recs = remaining.min(chunk_records) as usize;
        let win_end = rec_cursor + win_recs as u64;

        let any_needed = (rec_cursor..win_end)
            .any(|r| should_parse_record(r, bitmap, options.skip_system_records));

        if !any_needed {
            if let Some(bits) = bitmap {
                let unallocated = (rec_cursor..win_end)
                    .filter(|&r| !bitmap_has_bit(bits, r))
                    .count() as u64;
                result.stats.bitmap_skipped_records += unallocated;
            }
            rec_cursor = win_end;
            remaining -= win_recs as u64;
            offset_cursor = offset_cursor.saturating_add((win_recs * record_size) as u64);
            continue;
        }

        let to_read = win_recs * record_size;
        let read_bytes = read_at_exact_or_eof(reader, offset_cursor, &mut buffer[..to_read])?;
        let records_in_buf = read_bytes / record_size;

        if records_in_buf == 0 {
            return Ok(true);
        }

        let actual_end = rec_cursor + records_in_buf as u64;
        if let Some(bits) = bitmap {
            let unallocated = (rec_cursor..actual_end)
                .filter(|&r| !bitmap_has_bit(bits, r))
                .count() as u64;
            result.stats.bitmap_skipped_records += unallocated;
        }

        let parsed_items: Vec<ExtractedItem> = buffer[..records_in_buf * record_size]
            .par_chunks_exact_mut(record_size)
            .enumerate()
            .filter_map(|(i, rec_buf)| {
                let rec_num = rec_cursor + i as u64;
                if !should_parse_record(rec_num, bitmap, options.skip_system_records) {
                    return None;
                }
                parse_mft_record(rec_buf, sector_size, rec_num).ok()
            })
            .flatten()
            .collect();

        result.items.extend(parsed_items);

        rec_cursor = actual_end;
        remaining -= records_in_buf as u64;
        offset_cursor = offset_cursor.saturating_add((records_in_buf * record_size) as u64);

        if records_in_buf < win_recs {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Scans an NTFS `$MFT` stream (full volume image, live handle, or standalone `$MFT` extract)
/// from `reader`, skipping zero-bitmap windows before issuing I/O.
pub fn scan_mft_from_reader<R: BlockReader + ?Sized>(
    reader: &mut R,
    options: MftScanOptions,
) -> Result<MftScanResult, VolumeError> {
    let layout = detect_mft_layout(reader)?;
    let chunk_records = options.chunk_records.max(1);
    let mut buffer = vec![0u8; chunk_records * layout.record_size];
    let mut result = MftScanResult::default();
    let bitmap = layout.bitmap.as_deref();

    match layout.source {
        MftSourceLayout::VolumeRuns {
            bytes_per_cluster,
            runs,
            total_records_cap,
        } => {
            let mut current_record_num = 0u64;
            for run in runs {
                if let Some(cap) = total_records_cap
                    && current_record_num >= cap
                {
                    break;
                }
                let run_bytes = run.cluster_count.saturating_mul(bytes_per_cluster);
                let mut run_records = run_bytes / layout.record_size as u64;
                if let Some(cap) = total_records_cap {
                    run_records = run_records.min(cap.saturating_sub(current_record_num));
                }
                if run_records == 0 {
                    continue;
                }

                let Some(lcn) = run.lcn else {
                    if let Some(bits) = bitmap {
                        result.stats.bitmap_skipped_records +=
                            (current_record_num..current_record_num + run_records)
                                .filter(|&r| !bitmap_has_bit(bits, r))
                                .count() as u64;
                    }
                    current_record_num += run_records;
                    continue;
                };
                if lcn < 0 {
                    return Err(VolumeError::Runlist(RunlistError::UnexpectedEnd(0)));
                }

                let run_offset = (lcn as u64).saturating_mul(bytes_per_cluster);
                let _hit_eof = scan_contiguous_range(
                    reader,
                    ContiguousRange {
                        start_offset: run_offset,
                        start_rec_num: current_record_num,
                        num_records: run_records,
                        record_size: layout.record_size,
                        sector_size: layout.sector_size,
                        options,
                        bitmap,
                    },
                    &mut buffer,
                    &mut result,
                )?;
                current_record_num += run_records;
            }
        }
        MftSourceLayout::StandaloneLinear { known_records } => {
            let max_records =
                known_records.or_else(|| bitmap.map(|bits| (bits.len() as u64).saturating_mul(8)));
            if let Some(total) = max_records {
                let _ = scan_contiguous_range(
                    reader,
                    ContiguousRange {
                        start_offset: 0,
                        start_rec_num: 0,
                        num_records: total,
                        record_size: layout.record_size,
                        sector_size: layout.sector_size,
                        options,
                        bitmap,
                    },
                    &mut buffer,
                    &mut result,
                )?;
            } else {
                let mut rec_cursor = 0u64;
                let mut offset_cursor = 0u64;
                let step = chunk_records as u64;
                loop {
                    let hit_eof = scan_contiguous_range(
                        reader,
                        ContiguousRange {
                            start_offset: offset_cursor,
                            start_rec_num: rec_cursor,
                            num_records: step,
                            record_size: layout.record_size,
                            sector_size: layout.sector_size,
                            options,
                            bitmap,
                        },
                        &mut buffer,
                        &mut result,
                    )?;
                    if hit_eof {
                        break;
                    }
                    rec_cursor += step;
                    offset_cursor =
                        offset_cursor.saturating_add((chunk_records * layout.record_size) as u64);
                }
            }
        }
    }

    Ok(result)
}

/// Scans an offline NTFS volume image or standalone `$MFT` file at `path`.
pub fn scan_mft_from_file(
    path: &Path,
    options: MftScanOptions,
) -> Result<MftScanResult, VolumeError> {
    let mut reader = FileBlockReader::open(path)?;
    scan_mft_from_reader(&mut reader, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_record0_runlist_and_non_resident_bitmap_fail_without_panicking() {
        let bad_span = NonResidentAttrSpan {
            attr_offset: 1000,
            runlist_offset: 16,
            attr_len: 100,
            real_size: 64,
        };
        let mut reader = SliceBlockReader::new(&[]);
        assert!(read_non_resident_bitmap(&mut reader, 4096, &[0u8; 512], &bad_span).is_err());

        assert!(parse_record0_data_runs(&[0u8; 64], 40, 64).is_err());

        let mut malformed_file = vec![0u8; 1024];
        malformed_file[0..4].copy_from_slice(b"FILE");
        malformed_file[4..6].copy_from_slice(&48u16.to_le_bytes());
        malformed_file[6..8].copy_from_slice(&99u16.to_le_bytes()); // out of bounds USA count
        let mut reader = SliceBlockReader::new(&malformed_file);
        assert!(scan_mft_from_reader(&mut reader, MftScanOptions::default()).is_err());
    }
}

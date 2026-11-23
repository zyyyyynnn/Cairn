// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Pure-Rust extended property parsers (`IDX-12`).
//!
//! Properties come only from the in-repo parsers. The Windows Shell property system is not
//! queried: its handlers are third-party code and would run inside the elevated worker.

#![forbid(unsafe_code)]

use crate::extractor::open_no_follow;
use cairn_core::property_store::RecordPropertyData;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_CONTAINER_READ_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_READ_BYTES: usize = 256 * 1024;
const MAX_XML_ENTRY_BYTES: u64 = 512 * 1024;

/// Extracts extended properties (`length_ms`, `author`, `tags`, `dimensions`, `framerate_mfps`)
/// from a file on disk using the in-repo parsers.
pub fn extract_file_properties(path: &Path) -> Option<RecordPropertyData> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    if !cairn_core::property_store::PROPERTY_INDEX_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }

    let data = extract_pure_rust_from_path(&ext, path).unwrap_or_default();

    (!data.is_empty()).then_some(data)
}

/// Parses extended properties directly from an in-memory buffer for format `ext`.
pub fn parse_properties_from_bytes(ext: &str, bytes: &[u8]) -> Option<RecordPropertyData> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if !cairn_core::property_store::PROPERTY_INDEX_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let data = match ext.as_str() {
        "png" => parse_png(bytes),
        "jpg" | "jpeg" => parse_jpeg(bytes),
        "gif" => parse_gif(bytes),
        "bmp" => parse_bmp(bytes),
        "webp" => parse_webp(bytes),
        "wav" => parse_wav(bytes),
        "avi" => parse_avi(bytes),
        "flac" => parse_flac(bytes),
        "mp3" => parse_mp3(bytes, None),
        "mp4" | "m4a" | "mov" | "m4v" | "3gp" => parse_mp4_moov(bytes),
        "pdf" => parse_pdf(bytes),
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" => {
            parse_zip_office(std::io::Cursor::new(bytes))
        }
        _ => None,
    }?;
    (!data.is_empty()).then_some(data)
}

fn extract_pure_rust_from_path(ext: &str, path: &Path) -> Option<RecordPropertyData> {
    match ext {
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" => {
            let file = open_no_follow(path).ok()?;
            parse_zip_office(file)
        }
        "mp4" | "m4a" | "mov" | "m4v" | "3gp" => parse_mp4_file(path),
        "mp3" => {
            let mut file = open_no_follow(path).ok()?;
            let file_len = file.metadata().ok()?.len();
            let header = read_capped(&mut file, MAX_HEADER_READ_BYTES)?;
            parse_mp3(&header, Some(file_len))
        }
        "pdf" => {
            let mut file = open_no_follow(path).ok()?;
            let file_len = file.metadata().ok()?.len();
            if file_len as usize > MAX_CONTAINER_READ_BYTES {
                return None;
            }
            let bytes = read_capped(&mut file, MAX_CONTAINER_READ_BYTES)?;
            parse_pdf(&bytes)
        }
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "wav" | "avi" | "flac" => {
            let mut file = open_no_follow(path).ok()?;
            let bytes = read_capped(&mut file, MAX_HEADER_READ_BYTES)?;
            parse_properties_from_bytes(ext, &bytes)
        }
        _ => None,
    }
}

fn read_capped<R: Read>(reader: &mut R, max_bytes: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn clean_opt_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn read_u16_be(bytes: &[u8], off: usize) -> Option<u16> {
    let slice = bytes.get(off..off + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u16_le(bytes: &[u8], off: usize) -> Option<u16> {
    let slice = bytes.get(off..off + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_be(bytes: &[u8], off: usize) -> Option<u32> {
    let slice = bytes.get(off..off + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u32_le(bytes: &[u8], off: usize) -> Option<u32> {
    let slice = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn dims_data(w: u32, h: u32) -> Option<RecordPropertyData> {
    (w > 0 && h > 0).then(|| RecordPropertyData {
        dimensions: Some((w, h)),
        ..Default::default()
    })
}

fn parse_png(bytes: &[u8]) -> Option<RecordPropertyData> {
    const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if bytes.get(..8)? != SIG || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = read_u32_be(bytes, 16)?;
    let h = read_u32_be(bytes, 20)?;
    dims_data(w, h)
}

fn parse_gif(bytes: &[u8]) -> Option<RecordPropertyData> {
    let sig = bytes.get(..6)?;
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return None;
    }
    let w = u32::from(read_u16_le(bytes, 6)?);
    let h = u32::from(read_u16_le(bytes, 8)?);
    dims_data(w, h)
}

fn parse_bmp(bytes: &[u8]) -> Option<RecordPropertyData> {
    if bytes.get(..2)? != b"BM" {
        return None;
    }
    let dib_size = read_u32_le(bytes, 14)?;
    if dib_size < 16 {
        return None;
    }
    let w = read_u32_le(bytes, 18)? as i32;
    let h = (read_u32_le(bytes, 22)? as i32).unsigned_abs();
    (w > 0 && h > 0).then(|| RecordPropertyData {
        dimensions: Some((w as u32, h)),
        ..Default::default()
    })
}

fn parse_jpeg(bytes: &[u8]) -> Option<RecordPropertyData> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut pos = 2usize;
    while pos + 3 < bytes.len() {
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if matches!(marker, 0x00 | 0x01 | 0xD0..=0xD8) {
            continue;
        }
        let len = usize::from(read_u16_be(bytes, pos)?);
        if len < 2 || pos + len > bytes.len() {
            break;
        }
        if matches!(marker, 0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u32::from(read_u16_be(bytes, pos + 3)?);
            let w = u32::from(read_u16_be(bytes, pos + 5)?);
            return dims_data(w, h);
        }
        pos += len;
    }
    None
}

fn parse_webp(bytes: &[u8]) -> Option<RecordPropertyData> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let chunk = bytes.get(12..16)?;
    match chunk {
        b"VP8X" if bytes.len() >= 30 => {
            let w = 1
                + (u32::from(bytes[24])
                    | (u32::from(bytes[25]) << 8)
                    | (u32::from(bytes[26]) << 16));
            let h = 1
                + (u32::from(bytes[27])
                    | (u32::from(bytes[28]) << 8)
                    | (u32::from(bytes[29]) << 16));
            dims_data(w, h)
        }
        b"VP8L" if bytes.len() >= 25 && bytes[20] == 0x2F => {
            let bits = read_u32_le(bytes, 21)?;
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            dims_data(w, h)
        }
        b"VP8 "
            if bytes.len() >= 30 && bytes.get(23..26) == Some([0x9D, 0x01, 0x2A].as_slice()) =>
        {
            let w = u32::from(read_u16_le(bytes, 26)? & 0x3FFF);
            let h = u32::from(read_u16_le(bytes, 28)? & 0x3FFF);
            dims_data(w, h)
        }
        _ => None,
    }
}

fn parse_wav(bytes: &[u8]) -> Option<RecordPropertyData> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut byte_rate = None;
    let mut data_bytes = None;
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = bytes.get(pos..pos + 4)?;
        let size = usize::try_from(read_u32_le(bytes, pos + 4)?).ok()?;
        let body = bytes.get(pos + 8..);
        if id == b"fmt " {
            if let Some(chunk) = body.and_then(|b| b.get(..size.min(b.len()))) {
                byte_rate = read_u32_le(chunk, 8);
            }
        } else if id == b"data" {
            data_bytes = Some(size as u64);
            break;
        }
        let step = 8usize.checked_add(size)?.checked_add(size & 1)?;
        pos = pos.checked_add(step)?;
    }
    let rate = u64::from(byte_rate?).max(1);
    if byte_rate? == 0 {
        return None;
    }
    let ms = data_bytes?.saturating_mul(1000) / rate;
    (ms > 0).then(|| RecordPropertyData {
        length_ms: Some(ms),
        ..Default::default()
    })
}

fn parse_avi(bytes: &[u8]) -> Option<RecordPropertyData> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"AVI " {
        return None;
    }
    let avih_off = bytes.windows(4).position(|w| w == b"avih")?;
    let body = bytes.get(avih_off + 8..)?;
    if body.len() < 40 {
        return None;
    }
    let us_per_frame = read_u32_le(body, 0)?;
    let total_frames = read_u32_le(body, 16)?;
    let width = read_u32_le(body, 32)?;
    let height = read_u32_le(body, 36)?;
    let length_ms = (us_per_frame > 0 && total_frames > 0)
        .then(|| u64::from(us_per_frame).saturating_mul(u64::from(total_frames)) / 1000);
    let framerate_mfps =
        (us_per_frame > 0).then(|| (1_000_000_000u64 / u64::from(us_per_frame)) as u32);
    let dimensions = (width > 0 && height > 0).then_some((width, height));
    let out = RecordPropertyData {
        length_ms,
        dimensions,
        framerate_mfps,
        ..Default::default()
    };
    (!out.is_empty()).then_some(out)
}

fn parse_flac(bytes: &[u8]) -> Option<RecordPropertyData> {
    if bytes.get(..4)? != b"fLaC" {
        return None;
    }
    let mut out = RecordPropertyData::default();
    let mut pos = 4usize;
    while pos + 4 <= bytes.len() {
        let header = bytes[pos];
        let is_last = (header & 0x80) != 0;
        let kind = header & 0x7F;
        let len = (usize::from(bytes[pos + 1]) << 16)
            | (usize::from(bytes[pos + 2]) << 8)
            | usize::from(bytes[pos + 3]);
        pos += 4;
        let body = bytes.get(pos..pos.saturating_add(len).min(bytes.len()))?;
        if kind == 0 && body.len() >= 18 {
            let sample_rate = (u32::from(body[10]) << 12)
                | (u32::from(body[11]) << 4)
                | (u32::from(body[12]) >> 4);
            let total_samples = ((u64::from(body[13] & 0x0F)) << 32)
                | (u64::from(body[14]) << 24)
                | (u64::from(body[15]) << 16)
                | (u64::from(body[16]) << 8)
                | u64::from(body[17]);
            if sample_rate > 0 && total_samples > 0 {
                out.length_ms = Some(total_samples.saturating_mul(1000) / u64::from(sample_rate));
            }
        } else if kind == 4 {
            parse_vorbis_comments(body, &mut out);
        }
        pos = pos.saturating_add(len);
        if is_last {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn parse_vorbis_comments(body: &[u8], out: &mut RecordPropertyData) {
    let Some(vendor_len) = read_u32_le(body, 0).and_then(|n| usize::try_from(n).ok()) else {
        return;
    };
    let mut pos = 4usize.saturating_add(vendor_len);
    let Some(count) = read_u32_le(body, pos) else {
        return;
    };
    pos += 4;
    let mut tags = Vec::new();
    for _ in 0..count.min(256) {
        let Some(len) = read_u32_le(body, pos).and_then(|n| usize::try_from(n).ok()) else {
            break;
        };
        pos += 4;
        let Some(slice) = body.get(pos..pos.saturating_add(len)) else {
            break;
        };
        pos += len;
        let entry = String::from_utf8_lossy(slice);
        if let Some((k, v)) = entry.split_once('=') {
            let key = k.trim().to_ascii_uppercase();
            if matches!(key.as_str(), "ARTIST" | "AUTHOR" | "ALBUMARTIST") && out.author.is_none() {
                out.author = clean_opt_text(v);
            } else if matches!(key.as_str(), "GENRE" | "TAGS" | "KEYWORDS")
                && let Some(clean) = clean_opt_text(v)
            {
                tags.push(clean);
            }
        }
    }
    if out.tags.is_none() && !tags.is_empty() {
        out.tags = Some(tags.join(", "));
    }
}

fn parse_mp3(bytes: &[u8], file_len: Option<u64>) -> Option<RecordPropertyData> {
    let mut out = RecordPropertyData::default();
    let mut audio_off = 0usize;

    if bytes.get(..3) == Some(b"ID3".as_slice()) && bytes.len() >= 10 {
        let synch_size = ((usize::from(bytes[6] & 0x7F)) << 21)
            | ((usize::from(bytes[7] & 0x7F)) << 14)
            | ((usize::from(bytes[8] & 0x7F)) << 7)
            | usize::from(bytes[9] & 0x7F);
        audio_off = 10 + synch_size;
        let tag_end = audio_off.min(bytes.len());
        let major = bytes[3];
        if major >= 3 {
            parse_id3v2_frames(&bytes[10..tag_end], major, &mut out);
        }
    }

    if let Some(frame_off) = find_mp3_frame(&bytes[audio_off.min(bytes.len())..]) {
        let abs = audio_off.min(bytes.len()) + frame_off;
        if let Some((bitrate_bps, sample_rate, spf)) = parse_mp3_frame_header(&bytes[abs..]) {
            if let Some(xing_frames) = parse_xing_frames(&bytes[abs..]) {
                let ms = u64::from(xing_frames) * u64::from(spf) * 1000 / u64::from(sample_rate);
                if ms > 0 {
                    out.length_ms = Some(ms);
                }
            } else if let Some(total) = file_len {
                let audio_bytes = total.saturating_sub(abs as u64);
                let ms = audio_bytes.saturating_mul(8_000) / u64::from(bitrate_bps.max(1));
                if ms > 0 {
                    out.length_ms = Some(ms);
                }
            }
        }
    }

    (!out.is_empty()).then_some(out)
}

fn parse_id3v2_frames(tag: &[u8], major: u8, out: &mut RecordPropertyData) {
    let mut pos = 0usize;
    let mut tags = Vec::new();
    while pos + 10 <= tag.len() {
        let id = &tag[pos..pos + 4];
        if id == [0, 0, 0, 0] {
            break;
        }
        let size = if major == 4 {
            ((usize::from(tag[pos + 4] & 0x7F)) << 21)
                | ((usize::from(tag[pos + 5] & 0x7F)) << 14)
                | ((usize::from(tag[pos + 6] & 0x7F)) << 7)
                | usize::from(tag[pos + 7] & 0x7F)
        } else {
            read_u32_be(tag, pos + 4).unwrap_or(0) as usize
        };
        pos += 10;
        if size == 0 || pos.saturating_add(size) > tag.len() {
            break;
        }
        let payload = &tag[pos..pos + size];
        pos += size;
        if matches!(id, b"TPE1" | b"TPE2" | b"TCOM") && out.author.is_none() {
            out.author = decode_id3_text(payload);
        } else if matches!(id, b"TCON" | b"TIT1")
            && let Some(text) = decode_id3_text(payload)
        {
            tags.push(text);
        }
    }
    if out.tags.is_none() && !tags.is_empty() {
        out.tags = Some(tags.join(", "));
    }
}

fn decode_utf16_bytes(data: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| {
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

fn decode_id3_text(payload: &[u8]) -> Option<String> {
    let (&enc, raw) = payload.split_first()?;
    let decoded = match enc {
        0 => String::from_utf8_lossy(raw).into_owned(),
        3 => String::from_utf8_lossy(raw).into_owned(),
        1 | 2 => {
            let (le, data) = if raw.starts_with(&[0xFF, 0xFE]) {
                (true, &raw[2..])
            } else if raw.starts_with(&[0xFE, 0xFF]) {
                (false, &raw[2..])
            } else {
                (enc == 2, raw)
            };
            decode_utf16_bytes(data, le)
        }
        _ => return None,
    };
    clean_opt_text(decoded.trim_matches('\0'))
}

fn find_mp3_frame(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(2)
        .position(|w| w[0] == 0xFF && (w[1] & 0xE0) == 0xE0)
}

fn parse_mp3_frame_header(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    if bytes.len() < 4 {
        return None;
    }
    let version = (bytes[1] >> 3) & 0x03;
    let layer = (bytes[1] >> 1) & 0x03;
    let bitrate_idx = usize::from((bytes[2] >> 4) & 0x0F);
    let sr_idx = usize::from((bytes[2] >> 2) & 0x03);
    if version == 1 || layer != 1 || bitrate_idx == 0 || bitrate_idx == 15 || sr_idx == 3 {
        return None;
    }
    const SR_V1: [u32; 3] = [44_100, 48_000, 32_000];
    const BR_V1_L3: [u32; 15] = [
        0, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000,
        192_000, 224_000, 256_000, 320_000,
    ];
    const BR_V2_L3: [u32; 15] = [
        0, 8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000,
        128_000, 144_000, 160_000,
    ];
    let (sample_rate, spf, bitrate) = if version == 3 {
        (SR_V1[sr_idx], 1152, BR_V1_L3[bitrate_idx])
    } else if version == 2 {
        (SR_V1[sr_idx] / 2, 576, BR_V2_L3[bitrate_idx])
    } else {
        (SR_V1[sr_idx] / 4, 576, BR_V2_L3[bitrate_idx])
    };
    Some((bitrate, sample_rate, spf))
}

fn parse_xing_frames(frame: &[u8]) -> Option<u32> {
    let pos = frame
        .windows(4)
        .position(|w| w == b"Xing" || w == b"Info")?;
    let flags = read_u32_be(frame, pos + 4)?;
    (flags & 0x01 != 0)
        .then(|| read_u32_be(frame, pos + 8))
        .flatten()
}

fn parse_mp4_file(path: &Path) -> Option<RecordPropertyData> {
    let mut file = open_no_follow(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut pos = 0u64;
    let mut header = [0u8; 16];

    while pos + 8 <= file_len {
        file.seek(SeekFrom::Start(pos)).ok()?;
        file.read_exact(&mut header[..8]).ok()?;
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind: [u8; 4] = [header[4], header[5], header[6], header[7]];
        let (box_size, header_len) = if size32 == 1 {
            if pos + 16 > file_len {
                break;
            }
            file.read_exact(&mut header[8..16]).ok()?;
            let ext = u64::from_be_bytes(header[8..16].try_into().ok()?);
            (ext, 16u64)
        } else if size32 == 0 {
            (file_len - pos, 8u64)
        } else {
            (u64::from(size32), 8u64)
        };
        if box_size < header_len || pos + box_size > file_len {
            break;
        }
        if &kind == b"moov" {
            let capped = box_size.min(MAX_CONTAINER_READ_BYTES as u64) as usize;
            file.seek(SeekFrom::Start(pos)).ok()?;
            let mut buf = vec![0u8; capped];
            file.read_exact(&mut buf).ok()?;
            return parse_mp4_moov(&buf);
        }
        pos += box_size;
    }
    None
}

fn parse_mp4_moov(bytes: &[u8]) -> Option<RecordPropertyData> {
    let mut out = RecordPropertyData::default();
    walk_mp4_atoms(bytes, &mut out);
    (!out.is_empty()).then_some(out)
}

fn walk_mp4_atoms(bytes: &[u8], out: &mut RecordPropertyData) {
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let size = match read_u32_be(bytes, pos) {
            Some(0) => bytes.len() - pos,
            Some(1) => {
                if pos + 16 > bytes.len() {
                    break;
                }
                usize::try_from(u64::from_be_bytes(
                    bytes[pos + 8..pos + 16].try_into().unwrap_or([0; 8]),
                ))
                .unwrap_or(0)
            }
            Some(n) => n as usize,
            None => break,
        };
        let hdr = if read_u32_be(bytes, pos) == Some(1) {
            16
        } else {
            8
        };
        if size < hdr || pos.saturating_add(size) > bytes.len() {
            break;
        }
        let kind = &bytes[pos + 4..pos + 8];
        let body = &bytes[pos + hdr..pos + size];
        match kind {
            b"moov" | b"udta" | b"ilst" => walk_mp4_atoms(body, out),
            b"meta" if body.len() > 4 => walk_mp4_atoms(&body[4..], out),
            b"trak" => parse_mp4_trak(body, out),
            b"mvhd" => parse_mp4_mvhd(body, out),
            b"\xa9ART" | b"aART" if out.author.is_none() => {
                out.author = extract_mp4_ilst_text(body);
            }
            b"\xa9gen" | b"keyw" | b"\xa9cmt" if out.tags.is_none() => {
                out.tags = extract_mp4_ilst_text(body);
            }
            _ => {}
        }
        pos += size;
    }
}

fn parse_mp4_mvhd(body: &[u8], out: &mut RecordPropertyData) {
    let Some(&version) = body.first() else {
        return;
    };
    let (timescale, duration) = if version == 0 && body.len() >= 20 {
        (
            read_u32_be(body, 12).unwrap_or(0),
            u64::from(read_u32_be(body, 16).unwrap_or(0)),
        )
    } else if version == 1 && body.len() >= 32 {
        (
            read_u32_be(body, 20).unwrap_or(0),
            u64::from_be_bytes(body[24..32].try_into().unwrap_or([0; 8])),
        )
    } else {
        return;
    };
    if timescale > 0 && duration > 0 && out.length_ms.is_none() {
        out.length_ms = Some(duration.saturating_mul(1000) / u64::from(timescale));
    }
}

fn parse_mp4_trak(trak: &[u8], out: &mut RecordPropertyData) {
    let mut dims = None;
    let mut timescale = None;
    let mut sample_delta = None;
    let mut is_video = false;

    let mut stack = vec![trak];
    while let Some(slice) = stack.pop() {
        let mut pos = 0usize;
        while pos + 8 <= slice.len() {
            let size = read_u32_be(slice, pos).unwrap_or(0) as usize;
            if size < 8 || pos.saturating_add(size) > slice.len() {
                break;
            }
            let kind = &slice[pos + 4..pos + 8];
            let body = &slice[pos + 8..pos + size];
            match kind {
                b"mdia" | b"minf" | b"stbl" => stack.push(body),
                b"tkhd" => {
                    if let Some(&ver) = body.first() {
                        let off = if ver == 1 { 88 } else { 76 };
                        if let (Some(w), Some(h)) =
                            (read_u32_be(body, off), read_u32_be(body, off + 4))
                        {
                            let px_w = w >> 16;
                            let px_h = h >> 16;
                            if px_w > 0 && px_h > 0 {
                                dims = Some((px_w, px_h));
                            }
                        }
                    }
                }
                b"hdlr" if body.len() >= 12 && &body[8..12] == b"vide" => {
                    is_video = true;
                }
                b"mdhd" => {
                    if let Some(&ver) = body.first() {
                        let off = if ver == 1 { 20 } else { 12 };
                        timescale = read_u32_be(body, off).filter(|t| *t > 0);
                    }
                }
                b"stts" if body.len() >= 16 => {
                    let entries = read_u32_be(body, 4).unwrap_or(0);
                    if entries >= 1 {
                        sample_delta = read_u32_be(body, 12).filter(|d| *d > 0);
                    }
                }
                _ => {}
            }
            pos += size;
        }
    }

    if (is_video || sample_delta.is_some()) && out.dimensions.is_none() {
        out.dimensions = dims;
    }
    if (is_video || out.dimensions.is_some())
        && let (Some(ts), Some(delta)) = (timescale, sample_delta)
    {
        let mfps = (u64::from(ts) * 1000 + u64::from(delta) / 2) / u64::from(delta);
        if mfps > 0 && out.framerate_mfps.is_none() {
            out.framerate_mfps = u32::try_from(mfps).ok();
        }
    }
}

fn extract_mp4_ilst_text(atom: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    while pos + 8 <= atom.len() {
        let size = read_u32_be(atom, pos)? as usize;
        if size < 8 || pos.saturating_add(size) > atom.len() {
            break;
        }
        if &atom[pos + 4..pos + 8] == b"data" && size > 16 {
            let text = String::from_utf8_lossy(&atom[pos + 16..pos + size]);
            return clean_opt_text(&text);
        }
        pos += size;
    }
    None
}

fn parse_zip_office<R: Read + Seek>(reader: R) -> Option<RecordPropertyData> {
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    let read_options = zip::read::ZipReadOptions::new().enforce_header_consistency(true);
    let mut xml_bytes = Vec::new();
    for entry_name in ["docProps/core.xml", "meta.xml"] {
        let Ok(entry) = archive.by_name_with_options(entry_name, read_options.clone()) else {
            continue;
        };
        if entry.size() > MAX_XML_ENTRY_BYTES {
            continue;
        }
        xml_bytes.clear();
        if entry
            .take(MAX_XML_ENTRY_BYTES + 1)
            .read_to_end(&mut xml_bytes)
            .is_err()
        {
            continue;
        }
        if xml_bytes.len() > MAX_XML_ENTRY_BYTES as usize {
            continue;
        }
        let Ok(xml) = std::str::from_utf8(&xml_bytes) else {
            continue;
        };
        let data = parse_core_properties_xml(xml);
        if !data.is_empty() {
            return Some(data);
        }
    }
    None
}

fn parse_core_properties_xml(xml: &str) -> RecordPropertyData {
    let mut reader = Reader::from_str(xml);
    let mut current: Option<&'static str> = None;
    let mut pending: Option<std::borrow::Cow<'_, str>> = None;
    let mut creator = None;
    let mut keywords = None;
    let mut subject = None;

    let mut commit_field = |field: &str, raw: &str| {
        if let Some(clean) = clean_opt_text(raw) {
            match field {
                "creator" if creator.is_none() => creator = Some(clean),
                "keywords" => keywords = Some(clean),
                "subject" if subject.is_none() => subject = Some(clean),
                _ => {}
            }
        }
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => {
                pending = None;
                current = match tag.name().as_ref() {
                    "dc:creator" | "meta:initial-creator" => Some("creator"),
                    "cp:keywords" | "meta:keyword" => Some("keywords"),
                    "dc:subject" => Some("subject"),
                    _ => None,
                };
            }
            Ok(Event::End(_)) => {
                if let (Some(field), Some(raw)) = (current.take(), pending.take()) {
                    commit_field(field, &raw);
                }
            }
            Ok(Event::Text(text)) if current.is_some() => {
                let piece = text.into_inner();
                match &mut pending {
                    None => pending = Some(piece),
                    Some(acc) => acc.to_mut().push_str(&piece),
                }
            }
            Ok(Event::CData(cdata)) if current.is_some() => {
                let piece = cdata.into_inner();
                match &mut pending {
                    None => pending = Some(piece),
                    Some(acc) => acc.to_mut().push_str(&piece),
                }
            }
            Ok(Event::GeneralRef(entity)) if current.is_some() => {
                if let Ok(Some(ch)) = entity.resolve_char_ref() {
                    match &mut pending {
                        None => pending = Some(std::borrow::Cow::Owned(ch.to_string())),
                        Some(acc) => acc.to_mut().push(ch),
                    }
                } else if let Some(resolved) =
                    quick_xml::escape::resolve_xml_entity(entity.as_ref())
                {
                    match &mut pending {
                        None => pending = Some(std::borrow::Cow::Borrowed(resolved)),
                        Some(acc) => acc.to_mut().push_str(resolved),
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    RecordPropertyData {
        author: creator,
        tags: keywords.or(subject),
        ..Default::default()
    }
}

fn parse_pdf(bytes: &[u8]) -> Option<RecordPropertyData> {
    let doc = lopdf::Document::load_mem(bytes).ok()?;
    let info_obj = doc.trailer.get(b"Info").ok()?;
    let dict = match info_obj {
        lopdf::Object::Reference(id) => doc.get_dictionary(*id).ok()?,
        lopdf::Object::Dictionary(d) => d,
        _ => return None,
    };

    let author = read_pdf_text(dict, b"Author");
    let tags = read_pdf_text(dict, b"Keywords").or_else(|| read_pdf_text(dict, b"Subject"));
    let data = RecordPropertyData {
        author,
        tags,
        ..Default::default()
    };
    (!data.is_empty()).then_some(data)
}

fn read_pdf_text(dict: &lopdf::Dictionary, key: &[u8]) -> Option<String> {
    let Ok(lopdf::Object::String(bytes, _)) = dict.get(key) else {
        return None;
    };
    let text = if bytes.starts_with(&[0xFE, 0xFF]) {
        std::borrow::Cow::Owned(decode_utf16_bytes(&bytes[2..], false))
    } else {
        String::from_utf8_lossy(bytes)
    };
    clean_opt_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_office_zip(
        entry_name: &str,
        payload: &[u8],
        method: zip::CompressionMethod,
    ) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default().compression_method(method);
            writer.start_file(entry_name, options).expect("start entry");
            writer.write_all(payload).expect("write entry");
            writer.finish().expect("finish zip");
        }
        cursor.into_inner()
    }

    #[test]
    fn rejects_oversized_office_xml_entry_without_truncating() {
        let padding = "x".repeat(MAX_XML_ENTRY_BYTES as usize + 1024);
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>Alice</dc:creator>
  <dc:description>{padding}</dc:description>
</cp:coreProperties>"#
        );
        let mut zip_bytes = build_office_zip(
            "docProps/core.xml",
            xml.as_bytes(),
            zip::CompressionMethod::Deflated,
        );
        assert_eq!(parse_properties_from_bytes("docx", &zip_bytes), None);

        let expected_size = (xml.len() as u32).to_le_bytes();
        let spoofed_size = 128u32.to_le_bytes();
        let lfh = zip_bytes
            .windows(4)
            .position(|w| w == [0x50, 0x4B, 0x03, 0x04])
            .expect("local file header");
        let cdh = zip_bytes
            .windows(4)
            .position(|w| w == [0x50, 0x4B, 0x01, 0x02])
            .expect("central directory header");
        assert_eq!(&zip_bytes[lfh + 22..lfh + 26], &expected_size);
        assert_eq!(&zip_bytes[cdh + 24..cdh + 28], &expected_size);
        zip_bytes[lfh + 22..lfh + 26].copy_from_slice(&spoofed_size);
        zip_bytes[cdh + 24..cdh + 28].copy_from_slice(&spoofed_size);

        assert_eq!(parse_properties_from_bytes("docx", &zip_bytes), None);
    }

    #[test]
    fn rejects_office_zip_with_inconsistent_local_header() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>Alice</dc:creator>
</cp:coreProperties>"#;
        let mut zip_bytes = build_office_zip(
            "docProps/core.xml",
            xml.as_bytes(),
            zip::CompressionMethod::Stored,
        );
        assert_eq!(
            parse_properties_from_bytes("docx", &zip_bytes)
                .and_then(|p| p.author)
                .as_deref(),
            Some("Alice")
        );

        // Mutate the local file header's entry name while leaving the central directory entry
        // unchanged so header consistency validation rejects the mismatch.
        let local_name_offset = zip_bytes
            .windows(b"docProps/core.xml".len())
            .position(|window| window == b"docProps/core.xml")
            .expect("local header filename");
        zip_bytes[local_name_offset..local_name_offset + b"docProps/core.xml".len()]
            .copy_from_slice(b"docProps/fake.xml");

        assert_eq!(parse_properties_from_bytes("docx", &zip_bytes), None);
    }

    #[test]
    fn pdf_text_and_xml_unescape_preserve_decoded_values() {
        let mut dict = lopdf::Dictionary::new();
        dict.set("Author", lopdf::Object::string_literal("Alice"));
        let mut utf16 = vec![0xFE, 0xFF];
        for unit in "Bob".encode_utf16() {
            utf16.extend_from_slice(&unit.to_be_bytes());
        }
        dict.set("Subject", lopdf::Object::string_literal(utf16));
        assert_eq!(read_pdf_text(&dict, b"Author").as_deref(), Some("Alice"));
        assert_eq!(read_pdf_text(&dict, b"Subject").as_deref(), Some("Bob"));

        let parsed = parse_core_properties_xml(
            r#"<cp:coreProperties><dc:creator>A &amp; B</dc:creator><cp:keywords>one, two</cp:keywords></cp:coreProperties>"#,
        );
        assert_eq!(parsed.author.as_deref(), Some("A & B"));
        assert_eq!(parsed.tags.as_deref(), Some("one, two"));

        let cdata = parse_core_properties_xml(
            r#"<cp:coreProperties><dc:creator><![CDATA[Carol & Dave]]></dc:creator><cp:keywords><![CDATA[rust, xml]]></cp:keywords></cp:coreProperties>"#,
        );
        assert_eq!(cdata.author.as_deref(), Some("Carol & Dave"));
        assert_eq!(cdata.tags.as_deref(), Some("rust, xml"));

        let unclosed = parse_core_properties_xml(
            r#"<cp:coreProperties><dc:creator>Alice</dc:creator><cp:keywords>truncated"#,
        );
        assert_eq!(unclosed.author.as_deref(), Some("Alice"));
        assert_eq!(unclosed.tags, None);
    }
}

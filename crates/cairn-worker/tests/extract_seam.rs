// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration seam tests for isolated extended property extraction (`IDX-12`).

use cairn_core::property_store::{RecordPropertyData, SourceId};
use cairn_protocol::{PropertyExtractResult, PropertyExtractTask};
use cairn_worker::property_extract::{extract_file_properties, parse_properties_from_bytes};
use std::io::Write;
use std::path::Path;

fn build_minimal_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes
}

fn build_minimal_wav(duration_ms: u32, sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 2;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
    let data_bytes = (u64::from(byte_rate) * u64::from(duration_ms) / 1000) as u32;
    let riff_size = 36 + data_bytes;

    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&(channels * (bits_per_sample / 8)).to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    out
}

fn build_minimal_docx(path: &Path, author: &str, tags: &str) {
    let file = std::fs::File::create(path).expect("create docx");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("docProps/core.xml", options).expect("entry");
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>{author}</dc:creator>
  <cp:keywords>{tags}</cp:keywords>
</cp:coreProperties>"#
    );
    zip.write_all(xml.as_bytes()).expect("write core.xml");
    zip.finish().expect("finish zip");
}

fn build_minimal_mp4_with_metadata(
    duration_ms: u32,
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    author: &str,
) -> Vec<u8> {
    fn atom(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let len = (8 + body.len()) as u32;
        let mut out = Vec::with_capacity(len as usize);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(body);
        out
    }

    // mvhd (version 0): timescale=1000, duration=duration_ms
    let mut mvhd_body = vec![0u8; 100];
    mvhd_body[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd_body[16..20].copy_from_slice(&duration_ms.to_be_bytes());
    let mvhd = atom(*b"mvhd", &mvhd_body);

    // tkhd (version 0): width & height in 16.16 fixed point at offsets 76..84
    let mut tkhd_body = vec![0u8; 84];
    tkhd_body[76..80].copy_from_slice(&(width << 16).to_be_bytes());
    tkhd_body[80..84].copy_from_slice(&(height << 16).to_be_bytes());
    let tkhd = atom(*b"tkhd", &tkhd_body);

    // mdhd: timescale=fps_num, duration=fps_num * 2
    let mut mdhd_body = vec![0u8; 24];
    mdhd_body[12..16].copy_from_slice(&fps_num.to_be_bytes());
    mdhd_body[16..20].copy_from_slice(&(fps_num * 2).to_be_bytes());
    let mdhd = atom(*b"mdhd", &mdhd_body);

    // stts: entry_count=1, sample_count=60, sample_delta=fps_den
    let mut stts_body = vec![0u8; 16];
    stts_body[4..8].copy_from_slice(&1u32.to_be_bytes());
    stts_body[8..12].copy_from_slice(&60u32.to_be_bytes());
    stts_body[12..16].copy_from_slice(&fps_den.to_be_bytes());
    let stts = atom(*b"stts", &stts_body);

    // hdlr: vide
    let mut hdlr_body = vec![0u8; 24];
    hdlr_body[8..12].copy_from_slice(b"vide");
    let hdlr = atom(*b"hdlr", &hdlr_body);

    let stbl = atom(*b"stbl", &stts);
    let minf = atom(*b"minf", &stbl);
    let mdia = atom(*b"mdia", &[mdhd, hdlr, minf].concat());
    let trak = atom(*b"trak", &[tkhd, mdia].concat());

    // meta -> ilst -> ©ART -> data
    let mut data_body = vec![0u8; 8];
    data_body[3] = 1; // UTF-8
    data_body.extend_from_slice(author.as_bytes());
    let data = atom(*b"data", &data_body);
    let art = atom(*b"\xa9ART", &data);
    let ilst = atom(*b"ilst", &art);
    let mut meta_body = vec![0u8; 4]; // version+flags
    meta_body.extend_from_slice(&ilst);
    let meta = atom(*b"meta", &meta_body);
    let udta = atom(*b"udta", &meta);

    atom(*b"moov", &[mvhd, trak, udta].concat())
}

#[test]
fn pure_rust_parsers_extract_media_image_and_document_properties() {
    let png =
        parse_properties_from_bytes("png", &build_minimal_png(1920, 1080)).expect("png properties");
    assert_eq!(png.dimensions, Some((1920, 1080)));

    let wav = parse_properties_from_bytes("wav", &build_minimal_wav(2500, 48_000))
        .expect("wav properties");
    assert_eq!(wav.length_ms, Some(2500));

    let mp4 = parse_properties_from_bytes(
        "mp4",
        &build_minimal_mp4_with_metadata(9500, 3840, 2160, 30_000, 1001, "Alice"),
    )
    .expect("mp4 properties");
    assert_eq!(mp4.length_ms, Some(9500));
    assert_eq!(mp4.dimensions, Some((3840, 2160)));
    assert_eq!(mp4.framerate_mfps, Some(29_970));
    assert_eq!(mp4.author.as_deref(), Some("Alice"));

    let temp = tempfile::tempdir().expect("tempdir");
    let docx_path = temp.path().join("report.docx");
    build_minimal_docx(&docx_path, "Bob", "spec,finance");
    let docx = extract_file_properties(&docx_path).expect("docx properties");
    assert_eq!(
        docx,
        RecordPropertyData {
            length_ms: None,
            author: Some("Bob".to_string()),
            tags: Some("spec,finance".to_string()),
            dimensions: None,
            framerate_mfps: None,
        }
    );

    assert!(
        parse_properties_from_bytes("png", b"not a valid png").is_none(),
        "corrupt headers must degrade cleanly to None"
    );

    // JPEG SOF0 + GIF89a + BMP
    let jpeg = vec![
        0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x04, 0x38, 0x07, 0x80, 0x01, 0x01, 0x11, 0x00,
    ];
    assert_eq!(
        parse_properties_from_bytes("jpg", &jpeg).and_then(|p| p.dimensions),
        Some((1920, 1080))
    );
    let mut gif = Vec::from(b"GIF89a".as_slice());
    gif.extend_from_slice(&640u16.to_le_bytes());
    gif.extend_from_slice(&480u16.to_le_bytes());
    assert_eq!(
        parse_properties_from_bytes("gif", &gif).and_then(|p| p.dimensions),
        Some((640, 480))
    );
    let mut bmp = vec![0u8; 26];
    bmp[..2].copy_from_slice(b"BM");
    bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&1280u32.to_le_bytes());
    bmp[22..26].copy_from_slice(&(-720i32).to_le_bytes());
    assert_eq!(
        parse_properties_from_bytes("bmp", &bmp).and_then(|p| p.dimensions),
        Some((1280, 720))
    );

    // FLAC STREAMINFO + VORBIS_COMMENT
    let mut flac = Vec::from(b"fLaC".as_slice());
    let mut streaminfo = [0u8; 34];
    // sample_rate = 44100 (0xAC44), total_samples = 88200 (2000ms)
    streaminfo[10] = (44_100u32 >> 12) as u8;
    streaminfo[11] = (44_100u32 >> 4) as u8;
    streaminfo[12] = ((44_100u32 & 0x0F) << 4) as u8;
    streaminfo[14..18].copy_from_slice(&88_200u32.to_be_bytes());
    flac.push(0x00);
    flac.extend_from_slice(&(34u32.to_be_bytes()[1..]));
    flac.extend_from_slice(&streaminfo);
    let mut comment = Vec::new();
    comment.extend_from_slice(&0u32.to_le_bytes()); // empty vendor
    comment.extend_from_slice(&2u32.to_le_bytes());
    for entry in [b"ARTIST=Elena".as_slice(), b"GENRE=Jazz".as_slice()] {
        comment.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        comment.extend_from_slice(entry);
    }
    flac.push(0x84); // is_last | VORBIS_COMMENT
    flac.extend_from_slice(&(comment.len() as u32).to_be_bytes()[1..]);
    flac.extend_from_slice(&comment);
    let flac_props = parse_properties_from_bytes("flac", &flac).expect("flac props");
    assert_eq!(flac_props.length_ms, Some(2000));
    assert_eq!(flac_props.author.as_deref(), Some("Elena"));
    assert_eq!(flac_props.tags.as_deref(), Some("Jazz"));

    // PDF Info dictionary
    let mut doc = lopdf::Document::with_version("1.5");
    let mut info_dict = lopdf::Dictionary::new();
    info_dict.set("Author", lopdf::Object::string_literal("Cairn Team"));
    info_dict.set("Keywords", lopdf::Object::string_literal("rust,ntfs"));
    let info_id = doc.add_object(info_dict);
    doc.trailer.set("Info", info_id);
    let mut pdf_bytes = Vec::new();
    doc.save_to(&mut pdf_bytes).expect("save pdf");
    let pdf_props = parse_properties_from_bytes("pdf", &pdf_bytes).expect("pdf props");
    assert_eq!(pdf_props.author.as_deref(), Some("Cairn Team"));
    assert_eq!(pdf_props.tags.as_deref(), Some("rust,ntfs"));

    // Corrupt media candidate returns cleanly without panic.
    let unknown_bin = temp.path().join("unknown.avi");
    std::fs::write(&unknown_bin, b"RIFF\x00\x00\x00\x00AVI bad").expect("write dummy avi");
    let _ = extract_file_properties(&unknown_bin);
}

#[test]
fn real_worker_binary_extracts_properties_batch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let png_path = temp.path().join("shot.png");
    let wav_path = temp.path().join("clip.wav");
    let docx_path = temp.path().join("notes.docx");
    std::fs::write(&png_path, build_minimal_png(800, 600)).expect("write png");
    std::fs::write(&wav_path, build_minimal_wav(1500, 44_100)).expect("write wav");
    build_minimal_docx(&docx_path, "Carol", "arch");

    let tasks = [
        PropertyExtractTask {
            source_id: SourceId(1),
            record_key: 10,
            modified_ts: 100,
            path: png_path.to_string_lossy().into_owned(),
        },
        PropertyExtractTask {
            source_id: SourceId(1),
            record_key: 20,
            modified_ts: 200,
            path: wav_path.to_string_lossy().into_owned(),
        },
        PropertyExtractTask {
            source_id: SourceId(2),
            record_key: 30,
            modified_ts: 300,
            path: docx_path.to_string_lossy().into_owned(),
        },
    ];

    let batch_path = temp.path().join("batch.jsonl");
    let out_path = temp.path().join("out.jsonl");
    {
        let mut batch = std::fs::File::create(&batch_path).expect("create batch");
        for task in &tasks {
            writeln!(batch, "{}", serde_json::to_string(task).expect("json")).expect("write");
        }
    }

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_CairnWorker"))
        .arg("--extract-properties")
        .arg("--batch")
        .arg(&batch_path)
        .arg("--out")
        .arg(&out_path)
        .status()
        .expect("run CairnWorker");
    assert!(status.success(), "CairnWorker exited with {status}");

    let raw = std::fs::read_to_string(&out_path).expect("read out.jsonl");
    let results: Vec<PropertyExtractResult> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse result"))
        .collect();

    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0].properties.as_ref().and_then(|p| p.dimensions),
        Some((800, 600))
    );
    assert_eq!(
        results[1].properties.as_ref().and_then(|p| p.length_ms),
        Some(1500)
    );
    let doc_props = results[2].properties.as_ref().expect("docx props");
    assert_eq!(doc_props.author.as_deref(), Some("Carol"));
    assert_eq!(doc_props.tags.as_deref(), Some("arch"));
}

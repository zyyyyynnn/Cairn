// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Text extraction trait and implementations for plain text, Office, and PDF documents.

use crate::detector::decode_bytes_to_string;
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use zip::{ZipArchive, ZipReadOptions};

const EXTRACTOR_HEADER_OVERHEAD_BYTES: usize = 4 * 1024;
const MAX_EXTRACTOR_INPUT_BYTES: usize = 24 * 1024 * 1024;

fn extractor_input_budget(max_bytes: usize) -> usize {
    if max_bytes == 0 {
        return 0;
    }
    max_bytes
        .saturating_mul(4)
        .saturating_add(EXTRACTOR_HEADER_OVERHEAD_BYTES)
        .min(MAX_EXTRACTOR_INPUT_BYTES)
}

fn truncate_to_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

#[derive(Error, Debug)]
pub enum ExtractError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zip extraction error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("XML parsing error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("PDF parsing error: {0}")]
    Pdf(String),
    #[error("Unsupported file extension: {0}")]
    Unsupported(String),
    #[error("Expansion limit exceeded (potential zip bomb)")]
    ExpansionLimitExceeded,
}

/// Unified trait for extracting plain text from different document types.
pub trait TextExtractor: Send + Sync {
    fn supports(&self, ext: &str) -> bool;
    fn extract(&self, path: &Path, max_bytes: usize) -> Result<String, ExtractError>;
}

/// Opens a file for reading without following a reparse point at the final path component.
///
/// The worker only receives paths the service believes are regular files, but that belief can be
/// stale (a file replaced by a symlink between indexing and extraction). Opening the reparse
/// point itself and then checking the opened handle's attributes closes that gap: the handle that
/// is read is the one whose attributes were checked.
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to read a reparse point: {}", path.display()),
            ));
        }
        Ok(file)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// Extractor for plain text, source code, and structured text formats.
#[derive(Default)]
pub struct PlainTextExtractor;

impl TextExtractor for PlainTextExtractor {
    fn supports(&self, ext: &str) -> bool {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "txt"
                | "md"
                | "markdown"
                | "rs"
                | "py"
                | "js"
                | "ts"
                | "jsx"
                | "tsx"
                | "c"
                | "cpp"
                | "h"
                | "hpp"
                | "cs"
                | "go"
                | "java"
                | "json"
                | "toml"
                | "yaml"
                | "yml"
                | "xml"
                | "html"
                | "css"
                | "csv"
                | "log"
                | "ini"
                | "conf"
                | "bat"
                | "cmd"
                | "ps1"
                | "sh"
        )
    }

    fn extract(&self, path: &Path, max_bytes: usize) -> Result<String, ExtractError> {
        let file = open_no_follow(path)?;
        let cap = file
            .metadata()
            .map(|m| (m.len() as usize).min(max_bytes))
            .unwrap_or(0);
        let mut buffer = Vec::with_capacity(cap);
        file.take(max_bytes as u64).read_to_end(&mut buffer)?;
        let mut text = decode_bytes_to_string(&buffer);
        truncate_to_char_boundary(&mut text, max_bytes);
        Ok(text)
    }
}

/// Extractor for Office Open XML files (docx, xlsx, pptx) using streaming XML parsing.
#[derive(Default)]
pub struct OfficeExtractor;

impl TextExtractor for OfficeExtractor {
    fn supports(&self, ext: &str) -> bool {
        matches!(ext.to_ascii_lowercase().as_str(), "docx" | "xlsx" | "pptx")
    }

    fn extract(&self, path: &Path, max_bytes: usize) -> Result<String, ExtractError> {
        let file = open_no_follow(path)?;
        let mut archive = ZipArchive::new(BufReader::new(file))?;
        let mut extracted_text = String::new();
        let mut total_uncompressed = 0usize;
        let max_uncompressed_limit = extractor_input_budget(max_bytes);
        let mut xml_bytes = Vec::new();
        let mut buf = Vec::new();

        for i in 0..archive.len() {
            // A local header that disagrees with the central directory can smuggle a different
            // entry past the name-based selection below, so every entry is read with consistency
            // enforced.
            let mut zip_file = archive
                .by_index_with_options(i, ZipReadOptions::new().enforce_header_consistency(true))?;
            let file_name = match zip_file.name() {
                Ok(n) => n.to_string(),
                Err(_) => continue,
            };

            // Only extract text from document content XML files
            let is_content_xml = file_name.ends_with(".xml")
                && (file_name.starts_with("word/")
                    || file_name.starts_with("xl/worksheets/")
                    || file_name.starts_with("xl/sharedStrings")
                    || file_name.starts_with("ppt/slides/"));

            if !is_content_xml {
                continue;
            }

            // The central directory declares the uncompressed size: refuse an entry that cannot
            // fit before it is decompressed, then cap the read so a lying header cannot exceed
            // the budget either.
            let remaining = max_uncompressed_limit.saturating_sub(total_uncompressed);
            if zip_file.size() > remaining as u64 {
                return Err(ExtractError::ExpansionLimitExceeded);
            }
            xml_bytes.clear();
            (&mut zip_file)
                .take(remaining as u64 + 1)
                .read_to_end(&mut xml_bytes)?;
            total_uncompressed += xml_bytes.len();
            if xml_bytes.len() > remaining {
                return Err(ExtractError::ExpansionLimitExceeded);
            }

            let mut reader = XmlReader::from_reader(xml_bytes.as_slice());
            reader.config_mut().trim_text(true);

            loop {
                buf.clear();
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Text(e)) => {
                        let text = e.as_ref();
                        extracted_text.push_str(text);
                        extracted_text.push(' ');
                        if extracted_text.len() >= max_bytes {
                            truncate_to_char_boundary(&mut extracted_text, max_bytes);
                            return Ok(extracted_text);
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break,
                    _ => {}
                }
            }
        }

        truncate_to_char_boundary(&mut extracted_text, max_bytes);
        Ok(extracted_text)
    }
}

/// Extractor for PDF files using `lopdf`.
#[derive(Default)]
pub struct PdfExtractor;

impl TextExtractor for PdfExtractor {
    fn supports(&self, ext: &str) -> bool {
        ext.eq_ignore_ascii_case("pdf")
    }

    fn extract(&self, path: &Path, max_bytes: usize) -> Result<String, ExtractError> {
        let file = open_no_follow(path)?;
        let budget = extractor_input_budget(max_bytes);
        let file_len = file.metadata().map(|m| m.len()).ok();
        if let Some(len) = file_len {
            if len > budget as u64 {
                return Err(ExtractError::Pdf(format!(
                    "PDF size {len} exceeds read budget {budget}"
                )));
            }
        }

        let cap = file_len.map(|len| (len as usize).min(budget)).unwrap_or(0);
        let mut buffer = Vec::with_capacity(cap);
        file.take(budget as u64 + 1).read_to_end(&mut buffer)?;
        if buffer.len() > budget {
            return Err(ExtractError::Pdf(format!(
                "PDF size {} exceeds read budget {budget}",
                buffer.len()
            )));
        }

        let doc =
            lopdf::Document::load_mem(&buffer).map_err(|e| ExtractError::Pdf(e.to_string()))?;

        let mut text = String::new();
        for page_id in doc.get_pages().keys() {
            if let Ok(content) = doc.extract_text(&[*page_id]) {
                text.push_str(&content);
                text.push('\n');
                if text.len() >= max_bytes {
                    break;
                }
            }
        }

        truncate_to_char_boundary(&mut text, max_bytes);
        Ok(text)
    }
}

/// Multi-format composite text extractor.
#[derive(Default)]
pub struct CompositeExtractor {
    extractors: Vec<Arc<dyn TextExtractor>>,
}

impl CompositeExtractor {
    pub fn new() -> Self {
        Self {
            extractors: vec![
                Arc::new(PlainTextExtractor),
                Arc::new(OfficeExtractor),
                Arc::new(PdfExtractor),
            ],
        }
    }

    pub fn extract_file(&self, path: &Path, max_bytes: usize) -> Result<String, ExtractError> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        for extractor in &self.extractors {
            if extractor.supports(ext) {
                return extractor.extract(path, max_bytes);
            }
        }

        Err(ExtractError::Unsupported(ext.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn open_no_follow_rejects_a_symlink_and_reads_a_regular_file() {
        use std::os::windows::fs::symlink_file;

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "payload").expect("target file");
        let link = dir.path().join("link.txt");
        if let Err(err) = symlink_file(&target, &link) {
            // Creating a symlink needs SeCreateSymbolicLinkPrivilege or Developer Mode.
            eprintln!("skipping open_no_follow symlink test: {err}");
            return;
        }

        let err = open_no_follow(&link).expect_err("a symlink must not be opened");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        let mut file = open_no_follow(&target).expect("regular file opens");
        let mut contents = String::new();
        file.read_to_string(&mut contents).expect("read target");
        assert_eq!(contents, "payload");
    }

    #[cfg(windows)]
    #[test]
    fn plain_text_extraction_refuses_a_symlink() {
        use std::os::windows::fs::symlink_file;

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "payload").expect("target file");
        let link = dir.path().join("link.txt");
        if let Err(err) = symlink_file(&target, &link) {
            eprintln!("skipping extraction symlink test: {err}");
            return;
        }

        let err = PlainTextExtractor
            .extract(&link, 1024)
            .expect_err("extraction must not follow the link");
        assert!(matches!(err, ExtractError::Io(_)));

        let text = PlainTextExtractor
            .extract(&target, 1024)
            .expect("regular file extracts");
        assert_eq!(text, "payload");
    }

    fn write_zip(path: &std::path::Path, entries: &[(&str, &str)]) {
        use std::io::Write as _;

        let file = std::fs::File::create(path).expect("zip file");
        let mut writer = zip::ZipWriter::new(file);
        for (name, body) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start entry");
            writer.write_all(body.as_bytes()).expect("write entry");
        }
        writer.finish().expect("finish zip");
    }

    #[test]
    fn office_extraction_reads_text_from_content_xml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("report.docx");
        write_zip(
            &path,
            &[
                (
                    "word/document.xml",
                    "<w:document><w:body><w:p><w:t>quarterly report</w:t></w:p></w:body></w:document>",
                ),
                ("word/media/image1.png", "not xml content"),
            ],
        );

        let text = OfficeExtractor.extract(&path, 4096).expect("extract");
        assert!(text.contains("quarterly report"), "{text}");
    }

    #[test]
    fn office_extraction_refuses_an_entry_beyond_the_expansion_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bomb.docx");
        let payload = "a".repeat(256 * 1024);
        write_zip(&path, &[("word/document.xml", &payload)]);

        let err = OfficeExtractor
            .extract(&path, 1024)
            .expect_err("an entry larger than the budget must be refused");
        assert!(matches!(err, ExtractError::ExpansionLimitExceeded));
    }

    fn build_minimal_pdf(text: &str) -> Vec<u8> {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let stream_body = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET");
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
            format!(
                "4 0 obj\n<< /Length {} >>\nstream\n{stream_body}\nendstream\nendobj\n",
                stream_body.len()
            ),
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
        ];

        let mut pdf = Vec::from(b"%PDF-1.4\n".as_slice());
        let mut offsets = vec![0usize];
        for obj in &objects {
            offsets.push(pdf.len());
            pdf.extend_from_slice(obj.as_bytes());
        }
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn office_extraction_tightens_budget_and_truncates_utf8_safely() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Tightened expansion limit rejects 8x payload (old 10x allowed this through).
        let medium_bomb = dir.path().join("medium_bomb.docx");
        let medium_payload = "x".repeat(16 * 1024);
        write_zip(&medium_bomb, &[("word/document.xml", &medium_payload)]);
        let err = OfficeExtractor
            .extract(&medium_bomb, 2048)
            .expect_err("8x entry must exceed tightened expansion budget");
        assert!(matches!(err, ExtractError::ExpansionLimitExceeded));

        // Large single XML text run is truncated strictly within max_bytes on a UTF-8 boundary.
        let cjk_doc = dir.path().join("cjk.docx");
        let xml = format!(
            "<w:document><w:body><w:p><w:t>{}</w:t></w:p></w:body></w:document>",
            "中".repeat(400)
        );
        write_zip(&cjk_doc, &[("word/document.xml", &xml)]);
        let text = OfficeExtractor.extract(&cjk_doc, 100).expect("extract cjk");
        assert!(text.len() <= 100, "len was {}", text.len());
        assert!(!text.is_empty());
    }

    #[test]
    fn pdf_extraction_enforces_pre_parse_budget_and_truncates_output() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Oversized file must fail on read budget before lopdf tries parsing the trailer/objects.
        let huge_pdf = dir.path().join("oversized.pdf");
        let mut oversized = Vec::from(b"%PDF-1.4\n".as_slice());
        oversized.resize(32 * 1024, b' ');
        std::fs::write(&huge_pdf, &oversized).expect("write huge pdf");
        let err = PdfExtractor
            .extract(&huge_pdf, 1024)
            .expect_err("oversized PDF must abort before full parse");
        assert!(
            matches!(&err, ExtractError::Pdf(msg) if msg.contains("budget")),
            "unexpected error: {err:?}"
        );

        // Valid PDF within budget extracts text and truncates strictly within max_bytes.
        let valid_pdf = dir.path().join("sample.pdf");
        let body = format!("Cairn PDF test {}", "alpha ".repeat(80));
        std::fs::write(&valid_pdf, build_minimal_pdf(&body)).expect("write valid pdf");
        let extracted = PdfExtractor
            .extract(&valid_pdf, 64)
            .expect("extract valid pdf");
        assert!(extracted.len() <= 64, "len was {}", extracted.len());
        assert!(extracted.contains("Cairn PDF test"), "{extracted:?}");
    }
}

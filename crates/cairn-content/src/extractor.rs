// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Text extraction trait and implementations for plain text, Office, and PDF documents.

use crate::detector::decode_bytes_to_string;
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use zip::ZipArchive;

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
        let mut buffer = Vec::new();
        file.take(max_bytes as u64).read_to_end(&mut buffer)?;
        Ok(decode_bytes_to_string(&buffer))
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
        let mut archive = ZipArchive::new(file)?;
        let mut extracted_text = String::new();
        let mut total_uncompressed = 0usize;
        let max_uncompressed_limit = max_bytes * 10; // Expansion ratio defense

        for i in 0..archive.len() {
            let mut zip_file = archive.by_index(i)?;
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

            let mut xml_bytes = Vec::new();
            zip_file.read_to_end(&mut xml_bytes)?;
            total_uncompressed += xml_bytes.len();
            if total_uncompressed > max_uncompressed_limit {
                return Err(ExtractError::ExpansionLimitExceeded);
            }

            let mut reader = XmlReader::from_reader(xml_bytes.as_slice());
            reader.config_mut().trim_text(true);

            let mut buf = Vec::new();
            while let Ok(event) = reader.read_event_into(&mut buf) {
                match event {
                    Event::Text(e) => {
                        let text = e.as_ref();
                        extracted_text.push_str(text);
                        extracted_text.push(' ');
                        if extracted_text.len() >= max_bytes {
                            return Ok(extracted_text);
                        }
                    }
                    Event::Eof => break,
                    _ => {}
                }
                buf.clear();
            }
        }

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
        let doc = lopdf::Document::load_from(file).map_err(|e| ExtractError::Pdf(e.to_string()))?;

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
}

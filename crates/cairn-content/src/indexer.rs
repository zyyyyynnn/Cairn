// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Tantivy full-text index management and content querying.

use crate::tokenizer::{CjkBigramLatinTokenizer, CJK_BIGRAM_LATIN_TOKENIZER};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContentIndexError {
    #[error("Tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("Query parse error: {0}")]
    QueryParse(#[from] tantivy::query::QueryParserError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("content index writer is still in use")]
    WriterInUse,
    #[error("content index writer lock is poisoned")]
    WriterPoisoned,
}

#[derive(Clone, Debug)]
pub struct ContentHit {
    pub volume_id: u64,
    pub frn: u64,
    pub path: String,
    pub snippet: String,
}

/// Read-only handle onto an existing Tantivy index. It never opens a writer, so a process that
/// only answers queries cannot hold merge threads or rewrite the index.
#[derive(Clone)]
pub struct ContentSearcher {
    index: Index,
    index_dir: std::path::PathBuf,
    reader: IndexReader,
    f_volume_id: Field,
    f_frn: Field,
    f_file_size: Field,
    f_modified_ts: Field,
    f_path: Field,
    f_body: Field,
}

impl ContentSearcher {
    /// Opens an existing index for searching. A missing directory or an index without the
    /// expected schema is an error; this constructor never creates or writes anything.
    pub fn open<P: AsRef<Path>>(index_dir: P) -> Result<Self, ContentIndexError> {
        let dir_path = index_dir.as_ref();
        let index = Index::open_in_dir(dir_path)?;
        Self::from_index(index, dir_path)
    }

    fn from_index(index: Index, index_dir: &Path) -> Result<Self, ContentIndexError> {
        // Register custom CJK bigram + Latin mixed tokenizer
        index
            .tokenizers()
            .register(CJK_BIGRAM_LATIN_TOKENIZER, CjkBigramLatinTokenizer);

        // Resolve the fields from the on-disk schema: a searcher must never invent a schema that
        // disagrees with the segments it is about to read.
        let schema = index.schema();
        let f_volume_id = schema.get_field("volume_id")?;
        let f_frn = schema.get_field("frn")?;
        let f_file_size = schema.get_field("file_size")?;
        let f_modified_ts = schema.get_field("modified_ts")?;
        let f_path = schema.get_field("path")?;
        let f_body = schema.get_field("body")?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;

        Ok(Self {
            index,
            index_dir: index_dir.to_path_buf(),
            reader,
            f_volume_id,
            f_frn,
            f_file_size,
            f_modified_ts,
            f_path,
            f_body,
        })
    }

    /// Number of committed documents in the index.
    pub fn document_count(&self) -> Result<u64, ContentIndexError> {
        self.reader.reload()?;
        Ok(self.reader.searcher().num_docs())
    }

    /// Bytes occupied by the index directory on disk.
    pub fn index_size_bytes(&self) -> u64 {
        fn is_reparse_or_symlink(meta: &fs::Metadata) -> bool {
            if meta.file_type().is_symlink() {
                return true;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    return true;
                }
            }
            false
        }

        fn dir_size(path: &Path) -> u64 {
            let Ok(entries) = fs::read_dir(path) else {
                return 0;
            };
            entries
                .flatten()
                .map(|entry| {
                    let entry_path = entry.path();
                    let Ok(meta) = fs::symlink_metadata(&entry_path) else {
                        return 0;
                    };
                    if is_reparse_or_symlink(&meta) {
                        return 0;
                    }
                    if meta.is_dir() {
                        dir_size(&entry_path)
                    } else {
                        meta.len()
                    }
                })
                .sum()
        }
        dir_size(&self.index_dir)
    }

    /// Queries the full-text content index.
    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
    ) -> Result<Vec<ContentHit>, ContentIndexError> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let mut query_parser = QueryParser::for_index(&self.index, vec![self.f_body, self.f_path]);
        query_parser.set_conjunction_by_default();
        let query = query_parser.parse_query(query_str)?;

        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut hits = Vec::with_capacity(top_docs.len());

        for (_score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

            let volume_id = retrieved_doc
                .get_first(self.f_volume_id)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let frn = retrieved_doc
                .get_first(self.f_frn)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let path = retrieved_doc
                .get_first(self.f_path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let body = retrieved_doc
                .get_first(self.f_body)
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Compute preview snippet
            let snippet = create_snippet(body, query_str, 120);

            hits.push(ContentHit {
                volume_id,
                frn,
                path,
                snippet,
            });
        }

        Ok(hits)
    }

    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> Result<usize, ContentIndexError> {
        Ok(self.index.searchable_segment_ids()?.len())
    }
}

/// Full-text search index backed by Tantivy.
pub struct ContentIndex {
    searcher: ContentSearcher,
    writer: Arc<Mutex<IndexWriter>>,
}

impl ContentIndex {
    /// Opens or creates a Tantivy index in `index_dir`.
    pub fn open_or_create<P: AsRef<Path>>(index_dir: P) -> Result<Self, ContentIndexError> {
        let dir_path = index_dir.as_ref();
        fs::create_dir_all(dir_path)?;

        let mut schema_builder = Schema::builder();
        schema_builder.add_u64_field("volume_id", INDEXED | FAST | STORED);
        schema_builder.add_u64_field("frn", INDEXED | FAST | STORED);
        schema_builder.add_u64_field("file_size", FAST);
        schema_builder.add_i64_field("modified_ts", FAST);
        schema_builder.add_text_field("path", STRING | STORED);

        let body_indexing = TextFieldIndexing::default()
            .set_tokenizer(CJK_BIGRAM_LATIN_TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let body_options = TextOptions::default()
            .set_indexing_options(body_indexing)
            .set_stored();
        schema_builder.add_text_field("body", body_options);

        let schema = schema_builder.build();

        let index = match Index::open_in_dir(dir_path) {
            Ok(idx) => idx,
            Err(_) => Index::create_in_dir(dir_path, schema)?,
        };

        let searcher = ContentSearcher::from_index(index.clone(), dir_path)?;
        let writer = Arc::new(Mutex::new(index.writer(50_000_000)?));

        Ok(Self { searcher, writer })
    }

    /// Indexes or updates a document.
    pub fn index_document(
        &self,
        volume_id: u64,
        frn: u64,
        size: u64,
        modified_ts: i64,
        path: &str,
        body: &str,
    ) -> Result<(), ContentIndexError> {
        self.index_document_deferred(volume_id, frn, size, modified_ts, path, body)?;
        self.commit()
    }

    /// Adds or replaces one document without committing. A backfill batch uses this for every
    /// file and commits once at the end; committing per document turns a large batch into an
    /// fsync storm.
    pub fn index_document_deferred(
        &self,
        volume_id: u64,
        frn: u64,
        size: u64,
        modified_ts: i64,
        path: &str,
        body: &str,
    ) -> Result<(), ContentIndexError> {
        let mut writer = self.writer.lock().unwrap();

        // Remove the previous document for this (volume_id, frn). Deleting by FRN alone would
        // remove a same-numbered record on another volume, whose IDs are only unique per volume.
        Self::delete_from(
            &mut writer,
            self.searcher.f_volume_id,
            self.searcher.f_frn,
            volume_id,
            frn,
        )?;

        writer.add_document(doc!(
            self.searcher.f_volume_id => volume_id,
            self.searcher.f_frn => frn,
            self.searcher.f_file_size => size,
            self.searcher.f_modified_ts => modified_ts,
            self.searcher.f_path => path,
            self.searcher.f_body => body,
        ))?;
        Ok(())
    }

    /// Removes documents for deleted or no-longer-eligible `(volume_id, frn)` pairs without
    /// committing. A debounced batch deletes all of them and commits once.
    pub fn delete_documents_deferred(
        &self,
        documents: &[(u64, u64)],
    ) -> Result<(), ContentIndexError> {
        if documents.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer.lock().unwrap();
        for &(volume_id, frn) in documents {
            Self::delete_from(
                &mut writer,
                self.searcher.f_volume_id,
                self.searcher.f_frn,
                volume_id,
                frn,
            )?;
        }
        Ok(())
    }

    /// Removes documents and commits immediately. An empty batch is a no-op: committing it would
    /// rewrite `meta.json` and feed the index's own write back to the change journal.
    pub fn delete_documents(&self, documents: &[(u64, u64)]) -> Result<(), ContentIndexError> {
        if documents.is_empty() {
            return Ok(());
        }
        self.delete_documents_deferred(documents)?;
        self.commit()
    }

    fn delete_from(
        writer: &mut IndexWriter,
        volume_field: Field,
        frn_field: Field,
        volume_id: u64,
        frn: u64,
    ) -> Result<(), ContentIndexError> {
        let volume_query = Box::new(TermQuery::new(
            Term::from_field_u64(volume_field, volume_id),
            IndexRecordOption::Basic,
        )) as Box<dyn tantivy::query::Query>;
        let frn_query = Box::new(TermQuery::new(
            Term::from_field_u64(frn_field, frn),
            IndexRecordOption::Basic,
        )) as Box<dyn tantivy::query::Query>;
        writer.delete_query(Box::new(BooleanQuery::new(vec![
            (Occur::Must, volume_query),
            (Occur::Must, frn_query),
        ])))?;
        Ok(())
    }

    /// Commits everything added since the previous commit.
    pub fn commit(&self) -> Result<(), ContentIndexError> {
        let mut writer = self.writer.lock().unwrap();
        writer.commit()?;
        Ok(())
    }

    /// Number of committed documents in the index.
    pub fn document_count(&self) -> Result<u64, ContentIndexError> {
        self.searcher.document_count()
    }

    /// Bytes occupied by the index directory on disk.
    pub fn index_size_bytes(&self) -> u64 {
        self.searcher.index_size_bytes()
    }

    /// Queries the full-text content index.
    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
    ) -> Result<Vec<ContentHit>, ContentIndexError> {
        self.searcher.search(query_str, limit)
    }

    /// Removes every document without committing. The worker issues one clear, one delete batch
    /// and one document batch per process and commits them together.
    pub fn clear_deferred(&self) -> Result<(), ContentIndexError> {
        let writer = self.writer.lock().unwrap();
        writer.delete_all_documents()?;
        Ok(())
    }

    /// Clears the entire content index (CNT-07).
    pub fn clear(&self) -> Result<(), ContentIndexError> {
        self.clear_deferred()?;
        self.commit()
    }

    /// Consumes the writer and waits until Tantivy has finished every pending merge. A process
    /// that exits right after `commit` aborts its merge threads and leaves one segment per commit.
    pub fn finish(self) -> Result<(), ContentIndexError> {
        let ContentIndex { searcher, writer } = self;
        // The reader's segment mappings are not needed once the writer is about to finish.
        drop(searcher);
        let writer = Arc::try_unwrap(writer).map_err(|_| ContentIndexError::WriterInUse)?;
        let writer = writer
            .into_inner()
            .map_err(|_| ContentIndexError::WriterPoisoned)?;
        writer.wait_merging_threads()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> Result<usize, ContentIndexError> {
        self.searcher.segment_count()
    }
}

pub fn create_snippet(body: &str, query: &str, max_len: usize) -> String {
    if body.is_empty() {
        return String::new();
    }
    let lower_body = body.to_ascii_lowercase();
    let lower_query = query.to_ascii_lowercase();

    let pos = lower_body.find(&lower_query).unwrap_or(0);
    let target_start = pos.saturating_sub(max_len / 4);
    let start = floor_char_boundary(body, target_start);

    let target_end = (start + max_len).min(body.len());
    let end = floor_char_boundary(body, target_end);

    let mut snippet = String::new();
    if start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(&body[start..end]);
    if end < body.len() {
        snippet.push_str("...");
    }
    snippet
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_content_index_roundtrip() {
        let temp = tempdir().unwrap();
        let index = ContentIndex::open_or_create(temp.path()).expect("open failed");

        index
            .index_document(
                0,
                1001,
                1024,
                1600000000,
                "C:\\docs\\arch.md",
                "Cairn 架构设计与高性能全文检索系统",
            )
            .expect("index doc failed");

        let hits = index.search("架构设计", 10).expect("search failed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].frn, 1001);
        assert!(hits[0].snippet.contains("架构设计"));
    }

    #[test]
    fn the_same_frn_on_two_volumes_is_two_independent_documents() {
        let temp = tempdir().unwrap();
        let index = ContentIndex::open_or_create(temp.path()).expect("open failed");

        index
            .index_document(2, 101, 10, 1, r"C:\a.md", "alpha unique")
            .unwrap();
        index
            .index_document(3, 101, 10, 1, r"D:\b.md", "beta unique")
            .unwrap();

        let alpha = index.search("alpha", 10).unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].volume_id, 2);
        let beta = index.search("beta", 10).unwrap();
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].volume_id, 3);

        index
            .index_document(2, 101, 10, 2, r"C:\a.md", "replaced alpha")
            .unwrap();
        assert!(index.search("unique", 10).unwrap().len() == 1);
        let replaced = index.search("replaced", 10).unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].volume_id, 2);
        assert_eq!(
            index.search("beta", 10).unwrap()[0].volume_id,
            3,
            "updating one volume must not delete the other volume's record"
        );
    }

    #[test]
    fn test_delete_by_volume_and_frn_keeps_other_documents() {
        let temp = tempdir().unwrap();
        let index = ContentIndex::open_or_create(temp.path()).expect("open failed");
        index
            .index_document(0, 1001, 1, 1, "C:\\gone.txt", "needle in the gone file")
            .unwrap();
        index
            .index_document(1, 1001, 1, 1, "D:\\kept.txt", "needle on another volume")
            .unwrap();
        index
            .index_document(0, 1002, 1, 1, "C:\\kept.txt", "needle in another file")
            .unwrap();

        index.delete_documents(&[(0, 1001)]).expect("delete failed");

        let hits = index.search("needle", 10).expect("search failed");
        assert_eq!(hits.len(), 2);
        assert!(
            !hits.iter().any(|hit| hit.volume_id == 0 && hit.frn == 1001),
            "the deleted document must not survive"
        );
        assert!(index.document_count().unwrap() == 2);
    }

    #[test]
    fn read_only_searcher_reads_an_existing_index() {
        let temp = tempdir().unwrap();
        {
            let index = ContentIndex::open_or_create(temp.path()).expect("open failed");
            index
                .index_document(1, 7, 10, 1, r"C:\readme.txt", "only searchable text")
                .expect("index doc failed");
            index.finish().expect("finish failed");
        }

        let searcher = ContentSearcher::open(temp.path()).expect("read-only open failed");
        assert_eq!(searcher.document_count().unwrap(), 1);
        let hits = searcher.search("searchable", 10).expect("search failed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].volume_id, 1);
        assert!(searcher.index_size_bytes() > 0);
    }

    #[test]
    fn read_only_searcher_does_not_create_a_missing_index() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("missing-content");

        assert!(ContentSearcher::open(&missing).is_err());
        assert!(
            !missing.exists(),
            "opening a searcher must not create the index directory"
        );

        let empty = temp.path().join("empty-content");
        std::fs::create_dir(&empty).unwrap();
        assert!(
            ContentSearcher::open(&empty).is_err(),
            "a directory without a schema is not a searchable index"
        );
    }

    #[test]
    fn finishing_the_writer_merges_segments_from_many_commits() {
        let temp = tempdir().unwrap();
        let index = ContentIndex::open_or_create(temp.path()).expect("open failed");
        let commits = 12u64;
        let docs_per_commit = 20u64;

        for commit in 0..commits {
            for doc in 0..docs_per_commit {
                let frn = commit * docs_per_commit + doc + 1;
                index
                    .index_document_deferred(
                        0,
                        frn,
                        1,
                        1,
                        &format!(r"C:\merge-{frn}.txt"),
                        "mergeable body",
                    )
                    .unwrap();
            }
            index.commit().unwrap();
        }

        assert_eq!(index.document_count().unwrap(), commits * docs_per_commit);
        assert!(index.segment_count().unwrap() > 0);
        index.finish().expect("finish failed");

        let searcher = ContentSearcher::open(temp.path()).expect("read-only open failed");
        let segments = searcher.segment_count().unwrap();
        assert!(segments > 0);
        assert!(
            segments < commits as usize,
            "waiting for merges must leave fewer segments than commits, got {segments}"
        );
        assert_eq!(
            searcher.search("mergeable", 1000).unwrap().len(),
            (commits * docs_per_commit) as usize
        );
    }

    #[test]
    fn a_long_lived_searcher_does_not_block_a_writer_merge() {
        let temp = tempdir().unwrap();
        let index = ContentIndex::open_or_create(temp.path()).expect("seed index");
        for commit in 0..12u64 {
            for doc in 0..20u64 {
                let frn = commit * 20 + doc + 1;
                index
                    .index_document_deferred(
                        0,
                        frn,
                        1,
                        1,
                        &format!(r"C:\seed-{frn}.txt"),
                        "seed body",
                    )
                    .unwrap();
            }
            index.commit().unwrap();
        }

        // The service keeps this reader open while a separate worker process writes and merges.
        index.finish().expect("seed writer");
        let searcher = ContentSearcher::open(temp.path()).expect("long-lived searcher");
        let writer = ContentIndex::open_or_create(temp.path()).expect("worker index");
        for doc in 0..20u64 {
            writer
                .index_document_deferred(
                    0,
                    10_000 + doc,
                    1,
                    1,
                    &format!(r"C:\worker-{doc}.txt"),
                    "worker body",
                )
                .unwrap();
        }
        writer.commit().unwrap();
        writer
            .finish()
            .expect("worker merge must not be blocked by the reader");
        assert!(searcher.search("worker", 100).is_ok());
    }

    #[test]
    fn test_create_snippet_utf8_boundary() {
        let long_cjk =
            "这是一段很长很长很长很长很长的文本，包含关键字搜索，后面还有更多的内容。".repeat(10);
        let snippet = create_snippet(&long_cjk, "关键字搜索", 40);
        assert!(!snippet.is_empty());
        assert!(snippet.contains("关键字搜索"));
    }

    #[test]
    #[cfg(windows)]
    fn index_size_bytes_ignores_directory_junctions() {
        let temp = tempdir().unwrap();
        let index_dir = temp.path().join("index");
        let external_dir = temp.path().join("external");
        fs::create_dir_all(&external_dir).unwrap();
        fs::write(external_dir.join("large.bin"), vec![b'x'; 64 * 1024]).unwrap();

        let index = ContentIndex::open_or_create(&index_dir).expect("open index");
        index.commit().unwrap();
        let searcher = ContentSearcher::open(&index_dir).expect("open searcher");
        let base_size = searcher.index_size_bytes();

        // Create an NTFS junction inside index_dir pointing to external_dir.
        let junction_path = index_dir.join("loop_junction");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction_path)
            .arg(&external_dir)
            .output()
            .expect("run mklink /J");
        assert!(status.status.success(), "mklink /J failed");

        assert_eq!(
            searcher.index_size_bytes(),
            base_size,
            "index_size_bytes must not traverse reparse points / directory junctions"
        );
    }
}

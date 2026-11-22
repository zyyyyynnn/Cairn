// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-content`: Full-text content search engine with Tantivy,
//! CJK Bigram + Latin tokenizer, auto encoding detection, and document text extractors.

pub mod detector;
pub mod errors;
pub mod extractor;
pub mod indexer;
pub mod tokenizer;

pub use detector::decode_bytes_to_string;
pub use errors::raw_os_error;
pub use extractor::{
    CompositeExtractor, ExtractError, OfficeExtractor, PdfExtractor, PlainTextExtractor,
    TextExtractor,
};
pub use indexer::{
    CONTENT_RESULT_TOP_K, ContentHit, ContentIndex, ContentIndexError, ContentSearcher,
    ContentWriter, MAX_CONTENT_SNIPPETS, create_snippets,
};
pub use tokenizer::{CJK_BIGRAM_LATIN_TOKENIZER, CjkBigramLatinTokenizer};

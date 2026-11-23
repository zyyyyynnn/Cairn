// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-content`: Full-text content search engine with Tantivy,
//! CJK Bigram + Latin tokenizer, auto encoding detection, and document text extractors.

#[cfg(feature = "extractors")]
pub mod detector;
pub mod errors;
#[cfg(feature = "extractors")]
pub mod extractor;
pub mod indexer;
pub mod tokenizer;

#[cfg(feature = "extractors")]
pub use detector::decode_bytes_to_string;
pub use errors::raw_os_error;
#[cfg(feature = "extractors")]
pub use extractor::{
    CompositeExtractor, ExtractError, OfficeExtractor, PdfExtractor, PlainTextExtractor,
    TextExtractor, open_no_follow,
};
pub use indexer::{
    CONTENT_RESULT_TOP_K, ContentHit, ContentIndex, ContentIndexError, ContentSearcher,
    ContentWriter, MAX_CONTENT_SNIPPETS, create_snippets,
};
pub use tokenizer::{CJK_BIGRAM_LATIN_TOKENIZER, CjkBigramLatinTokenizer};

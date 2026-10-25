// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]
#![allow(clippy::chunks_exact_to_as_chunks)]

//! `cairn-content`: Full-text content search engine with Tantivy,
//! CJK Bigram + Latin tokenizer, auto encoding detection, and document text extractors.

pub mod detector;
pub mod extractor;
pub mod indexer;
pub mod tokenizer;

pub use detector::decode_bytes_to_string;
pub use extractor::{
    CompositeExtractor, ExtractError, OfficeExtractor, PdfExtractor, PlainTextExtractor,
    TextExtractor,
};
pub use indexer::{ContentHit, ContentIndex, ContentIndexError, ContentSearcher};
pub use tokenizer::{CjkBigramLatinTokenizer, CJK_BIGRAM_LATIN_TOKENIZER};

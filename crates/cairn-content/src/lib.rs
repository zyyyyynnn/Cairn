// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-content`: Full-text content index and query library powered by Tantivy
//! with a CJK Bigram + Latin tokenizer. Document parsing and encoding detection
//! live in `cairn-worker`.

pub mod errors;
pub mod indexer;
pub mod tokenizer;

pub use errors::raw_os_error;
pub use indexer::{
    CONTENT_RESULT_TOP_K, ContentHit, ContentIndex, ContentIndexError, ContentSearcher,
    ContentWriter, MAX_CONTENT_SNIPPETS, create_snippets,
};
pub use tokenizer::{CJK_BIGRAM_LATIN_TOKENIZER, CjkBigramLatinTokenizer};

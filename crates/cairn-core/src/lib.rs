// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-core`: High-performance compact file indexing, Everything query syntax parsing,
//! and parallel SIMD search execution.
//!
//! Strictly forbids unsafe code and avoids any platform-specific Windows API dependencies.

pub mod arena;
pub mod engine;
pub mod filter;
pub mod format;
pub mod model;
pub mod path_cache;
pub mod sort;
pub mod syntax;
pub mod time;
pub mod treemap;

pub use arena::NamePool;
pub use engine::{
    execute_query, execute_query_ordered, find_highlights, glob_match, CancellationToken,
};
pub use filter::{DateFilter, DateOp, SizeFilter, SizeOp};
pub use format::format_size_bytes;
pub use model::{
    FileRecord, FLAG_ARCHIVE, FLAG_COMPRESSED, FLAG_DIRECTORY, FLAG_HIDDEN, FLAG_READONLY,
    FLAG_REPARSE_POINT, FLAG_SPARSE, FLAG_SYSTEM, FLAG_TOMBSTONE,
};
pub use path_cache::DirectoryPathCache;
pub use sort::{sort_indices, PresortedIndices, SortColumn, SortDirection};
pub use syntax::{
    parse_query, parse_query_with, parse_search_term, QueryDefaults, QueryExpr, SearchTerm,
};

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_random_query_parsing_does_not_panic(s in "\\PC*") {
            let _ = parse_query(&s);
        }

        #[test]
        fn test_random_namepool_insert(s in "[a-zA-Z0-9_.-]{1,100}") {
            let mut pool = NamePool::default();
            let (off, len, ext) = pool.insert(&s);
            let retrieved = pool.get_name(off, len);
            prop_assert_eq!(retrieved, &s);
            if let Some(dot_pos) = s.rfind('.') {
                if dot_pos + 1 < s.len() {
                    prop_assert_eq!(pool.get_ext(off, len, ext), &s[dot_pos + 1..]);
                }
            }
        }
    }
}

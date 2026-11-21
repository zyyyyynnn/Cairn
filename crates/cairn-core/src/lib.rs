// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-core`: High-performance compact file indexing, Everything query syntax parsing,
//! and parallel SIMD search execution.
//!
//! Strictly forbids unsafe code and avoids any platform-specific Windows API dependencies.

pub mod arena;
pub mod dir_size;
pub mod engine;
pub mod file_kind;
pub mod filter;
pub mod format;
pub mod frn_index;
pub mod index_file;
pub mod model;
pub mod path_cache;
pub mod pinyin;
mod pinyin_data;
pub mod property_store;
pub mod query;
pub mod sort;
pub mod syntax;
pub mod text_match;
pub mod time;

pub use arena::NamePool;
pub use dir_size::DirectorySizeIndex;
pub use engine::{CancellationToken, execute_query, execute_query_ordered, glob_match};
pub use file_kind::FileKind;
pub use filter::{
    AttribFilter, DateFilter, DateOp, DimensionsFilter, DupeMode, SizeFilter, SizeOp,
};
pub use format::format_size_bytes;
pub use frn_index::{FrnLookup, IndexMemoryFootprint};
pub use model::{
    FLAG_ARCHIVE, FLAG_COMPRESSED, FLAG_DIRECTORY, FLAG_HIDDEN, FLAG_INDEXED_ATTRIBUTES_MASK,
    FLAG_READONLY, FLAG_REPARSE_POINT, FLAG_SPARSE, FLAG_SYSTEM, FLAG_TOMBSTONE, FileRecord,
    WIN32_ATTR_ARCHIVE, WIN32_ATTR_COMPRESSED, WIN32_ATTR_DIRECTORY, WIN32_ATTR_HIDDEN,
    WIN32_ATTR_READONLY, WIN32_ATTR_REPARSE_POINT, WIN32_ATTR_SPARSE_FILE, WIN32_ATTR_SYSTEM,
    flags_from_win32_attributes,
};
pub use path_cache::{DirectoryPathCache, normalize_root};
pub use property_store::{
    FOLDER_SOURCE_ID_BASE, PropertyKind, PropertySideTables, RecordPropertyData,
    RecordPropertyView, SourceId,
};
pub use sort::{
    PresortedIndices, SortColumn, SortContext, SortDirection, SortKey, SortSpec,
    compare_cross_source_records, sort_indices_with_spec,
};
pub use syntax::{
    QueryDefaults, QueryExpr, SearchTerm, parse_query, parse_query_with, parse_search_term,
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
            if let Some(dot_pos) = s.rfind('.')
                && dot_pos + 1 < s.len() {
                    prop_assert_eq!(pool.get_ext(off, len, ext), &s[dot_pos + 1..]);
                }
        }
    }
}

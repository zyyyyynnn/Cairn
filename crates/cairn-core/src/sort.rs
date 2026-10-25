// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-column sorting and presorted index management.

use crate::arena::NamePool;
use crate::engine::CancellationToken;
use crate::model::FileRecord;
use crate::path_cache::DirectoryPathCache;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
    Extension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

use std::cmp::Ordering;

/// Compares two strings using Natural Sort Order (numbers compared numerically).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut ai = 0;
    let mut bi = 0;

    while ai < a_bytes.len() && bi < b_bytes.len() {
        let ca = a_bytes[ai];
        let cb = b_bytes[bi];

        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let a_start = ai;
            while ai < a_bytes.len() && a_bytes[ai].is_ascii_digit() {
                ai += 1;
            }
            let b_start = bi;
            while bi < b_bytes.len() && b_bytes[bi].is_ascii_digit() {
                bi += 1;
            }

            let a_digits = &a[a_start..ai];
            let b_digits = &b[b_start..bi];

            let a_trimmed = a_digits.trim_start_matches('0');
            let b_trimmed = b_digits.trim_start_matches('0');

            if a_trimmed.len() != b_trimmed.len() {
                return a_trimmed.len().cmp(&b_trimmed.len());
            }
            if a_trimmed != b_trimmed {
                return a_trimmed.cmp(b_trimmed);
            }
            if a_digits.len() != b_digits.len() {
                return a_digits.len().cmp(&b_digits.len());
            }
        } else if ca.is_ascii() && cb.is_ascii() {
            let la = ca.to_ascii_lowercase();
            let lb = cb.to_ascii_lowercase();
            if la != lb {
                return la.cmp(&lb);
            }
            ai += 1;
            bi += 1;
        } else {
            let char_a = a[ai..].chars().next().unwrap();
            let char_b = b[bi..].chars().next().unwrap();
            let low_a = char_a.to_lowercase().next().unwrap_or(char_a);
            let low_b = char_b.to_lowercase().next().unwrap_or(char_b);

            if low_a != low_b {
                return low_a.cmp(&low_b);
            }

            ai += char_a.len_utf8();
            bi += char_b.len_utf8();
        }
    }

    a.len().cmp(&b.len())
}

/// Compares two strings ASCII case-insensitively without heap allocations.
#[inline]
pub fn cmp_ascii_ignore_case(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|b| b.to_ascii_lowercase())
        .cmp(b.bytes().map(|b| b.to_ascii_lowercase()))
}

/// Presorted arrays of record indices for zero-latency sorting on large datasets.
/// Each presorted index consumes exactly 4 Bytes / record (16 MB for 4 million items).
#[derive(Default, Clone)]
pub struct PresortedIndices {
    pub by_name: Vec<u32>,
    pub by_size: Vec<u32>,
    pub by_date_modified: Vec<u32>,
    pub by_path: Vec<u32>,
}

impl PresortedIndices {
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
            && self.by_size.is_empty()
            && self.by_date_modified.is_empty()
            && self.by_path.is_empty()
    }

    pub fn clear(&mut self) {
        self.by_name.clear();
        self.by_size.clear();
        self.by_date_modified.clear();
        self.by_path.clear();
    }

    /// Ensures a single requested column's presorted index is built on-demand.
    pub fn ensure_column(
        &mut self,
        col: SortColumn,
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
    ) {
        let count = records.len() as u32;
        if count == 0 {
            return;
        }

        match col {
            SortColumn::Name if self.by_name.is_empty() => {
                let mut by_name: Vec<u32> = (0..count).collect();
                by_name.par_sort_unstable_by(|&a, &b| {
                    let ra = &records[a as usize];
                    let rb = &records[b as usize];
                    let na = arena.get_name(ra.name_offset, ra.name_len);
                    let nb = arena.get_name(rb.name_offset, rb.name_len);
                    natural_cmp(na, nb).then_with(|| a.cmp(&b))
                });
                self.by_name = by_name;
            }
            SortColumn::Size if self.by_size.is_empty() => {
                let mut by_size: Vec<u32> = (0..count).collect();
                by_size.par_sort_unstable_by(|&a, &b| {
                    let ra = &records[a as usize];
                    let rb = &records[b as usize];
                    ra.size
                        .cmp(&rb.size)
                        .then_with(|| {
                            let na = arena.get_name(ra.name_offset, ra.name_len);
                            let nb = arena.get_name(rb.name_offset, rb.name_len);
                            natural_cmp(na, nb)
                        })
                        .then_with(|| a.cmp(&b))
                });
                self.by_size = by_size;
            }
            SortColumn::DateModified if self.by_date_modified.is_empty() => {
                let mut by_date_modified: Vec<u32> = (0..count).collect();
                by_date_modified.par_sort_unstable_by(|&a, &b| {
                    let ra = &records[a as usize];
                    let rb = &records[b as usize];
                    ra.modified_ts
                        .cmp(&rb.modified_ts)
                        .then_with(|| {
                            let na = arena.get_name(ra.name_offset, ra.name_len);
                            let nb = arena.get_name(rb.name_offset, rb.name_len);
                            natural_cmp(na, nb)
                        })
                        .then_with(|| a.cmp(&b))
                });
                self.by_date_modified = by_date_modified;
            }
            SortColumn::Path if self.by_path.is_empty() => {
                let mut by_path: Vec<u32> = (0..count).collect();
                by_path.par_sort_unstable_by(|&a, &b| {
                    let pa = path_cache.get_parent_path(records, arena, drive_letter, a);
                    let pb = path_cache.get_parent_path(records, arena, drive_letter, b);
                    pa.cmp(&pb)
                        .then_with(|| {
                            let ra = &records[a as usize];
                            let rb = &records[b as usize];
                            let na = arena.get_name(ra.name_offset, ra.name_len);
                            let nb = arena.get_name(rb.name_offset, rb.name_len);
                            natural_cmp(na, nb)
                        })
                        .then_with(|| a.cmp(&b))
                });
                self.by_path = by_path;
            }
            _ => {}
        }
    }

    /// Builds all presorted indices in parallel.
    pub fn build(
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
    ) -> Self {
        let mut indices = Self::default();
        indices.ensure_column(SortColumn::Name, records, arena, path_cache, drive_letter);
        indices.ensure_column(SortColumn::Size, records, arena, path_cache, drive_letter);
        indices.ensure_column(
            SortColumn::DateModified,
            records,
            arena,
            path_cache,
            drive_letter,
        );
        indices.ensure_column(SortColumn::Path, records, arena, path_cache, drive_letter);
        indices
    }
}

/// Sorts query result indices according to requested column and direction.
#[allow(clippy::too_many_arguments)]
pub fn sort_indices(
    indices: &mut [u32],
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    col: SortColumn,
    dir: SortDirection,
    cancel: &CancellationToken,
) {
    if indices.len() <= 1 || cancel.is_cancelled() {
        return;
    }

    indices.par_sort_unstable_by(|&a, &b| {
        let ra = &records[a as usize];
        let rb = &records[b as usize];

        let ord = match col {
            SortColumn::Name => {
                let na = arena.get_name(ra.name_offset, ra.name_len);
                let nb = arena.get_name(rb.name_offset, rb.name_len);
                natural_cmp(na, nb)
            }
            SortColumn::Path => {
                let pa = path_cache.get_parent_path(records, arena, drive_letter, a);
                let pb = path_cache.get_parent_path(records, arena, drive_letter, b);
                pa.cmp(&pb).then_with(|| {
                    let na = arena.get_name(ra.name_offset, ra.name_len);
                    let nb = arena.get_name(rb.name_offset, rb.name_len);
                    natural_cmp(na, nb)
                })
            }
            SortColumn::Size => ra.size.cmp(&rb.size).then_with(|| {
                let na = arena.get_name(ra.name_offset, ra.name_len);
                let nb = arena.get_name(rb.name_offset, rb.name_len);
                natural_cmp(na, nb)
            }),
            SortColumn::DateModified => ra.modified_ts.cmp(&rb.modified_ts).then_with(|| {
                let na = arena.get_name(ra.name_offset, ra.name_len);
                let nb = arena.get_name(rb.name_offset, rb.name_len);
                natural_cmp(na, nb)
            }),
            SortColumn::DateCreated => ra.created_ts.cmp(&rb.created_ts).then_with(|| {
                let na = arena.get_name(ra.name_offset, ra.name_len);
                let nb = arena.get_name(rb.name_offset, rb.name_len);
                natural_cmp(na, nb)
            }),
            SortColumn::Extension => {
                let ea = arena.get_ext(ra.name_offset, ra.name_len, ra.ext_offset);
                let eb = arena.get_ext(rb.name_offset, rb.name_len, rb.ext_offset);
                cmp_ascii_ignore_case(ea, eb).then_with(|| {
                    let na = arena.get_name(ra.name_offset, ra.name_len);
                    let nb = arena.get_name(rb.name_offset, rb.name_len);
                    natural_cmp(na, nb)
                })
            }
        };

        let primary = if dir == SortDirection::Descending {
            ord.reverse()
        } else {
            ord
        };

        primary.then_with(|| a.cmp(&b))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_natural_sort_numeric() {
        assert_eq!(natural_cmp("file2.txt", "file10.txt"), Ordering::Less);
        assert_eq!(natural_cmp("file10.txt", "file2.txt"), Ordering::Greater);
        assert_eq!(natural_cmp("item1", "item1"), Ordering::Equal);
        assert_eq!(natural_cmp("文档2.docx", "文档10.docx"), Ordering::Less);
        assert_eq!(natural_cmp("a01.txt", "a1.txt"), Ordering::Greater);
    }

    #[test]
    fn test_secondary_sort_key_and_tertiary_stability() {
        let mut arena = NamePool::default();
        let (n0, l0, e0) = arena.insert("b.txt");
        let (n1, l1, e1) = arena.insert("a.txt");
        let (n2, l2, e2) = arena.insert("a.txt");

        let records = vec![
            FileRecord {
                frn: 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: n0,
                name_len: l0,
                ext_offset: e0,
                volume_id: 0,
                flags: 0,
                size: 100,
                modified_ts: 50,
                created_ts: 10,
            },
            FileRecord {
                frn: 2,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: n1,
                name_len: l1,
                ext_offset: e1,
                volume_id: 0,
                flags: 0,
                size: 100,
                modified_ts: 50,
                created_ts: 20,
            },
            FileRecord {
                frn: 3,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: n2,
                name_len: l2,
                ext_offset: e2,
                volume_id: 0,
                flags: 0,
                size: 100,
                modified_ts: 50,
                created_ts: 30,
            },
        ];

        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        // 1. Sort by Size ascending: sizes are equal (100), secondary key (Name) puts "a.txt" before "b.txt".
        // Between the two "a.txt" records (indices 1 and 2), tertiary key index breaks tie (1 before 2).
        let mut indices = vec![0, 1, 2];
        sort_indices(
            &mut indices,
            &records,
            &arena,
            &path_cache,
            'C',
            SortColumn::Size,
            SortDirection::Ascending,
            &cancel,
        );
        assert_eq!(indices, vec![1, 2, 0]);

        // 2. Sort by DateModified ascending: all modified_ts are equal (50).
        indices = vec![0, 2, 1];
        sort_indices(
            &mut indices,
            &records,
            &arena,
            &path_cache,
            'C',
            SortColumn::DateModified,
            SortDirection::Ascending,
            &cancel,
        );
        assert_eq!(indices, vec![1, 2, 0]);

        // 3. Presorted index by size:
        let mut presorted = PresortedIndices::default();
        presorted.ensure_column(SortColumn::Size, &records, &arena, &path_cache, 'C');
        assert_eq!(presorted.by_size, vec![1, 2, 0]);
    }

    #[test]
    fn cmp_ascii_ignore_case_matches_lowercase_ordering() {
        assert_eq!(cmp_ascii_ignore_case("TXT", "txt"), Ordering::Equal);
        assert_eq!(cmp_ascii_ignore_case("doc", "DOCX"), Ordering::Less);
        assert_eq!(cmp_ascii_ignore_case("ZIP", "tar"), Ordering::Greater);
        assert_eq!(cmp_ascii_ignore_case("", "a"), Ordering::Less);
        assert_eq!(cmp_ascii_ignore_case("", ""), Ordering::Equal);
        assert_eq!(cmp_ascii_ignore_case("7Z", "7z"), Ordering::Equal);
        assert_eq!(cmp_ascii_ignore_case("c++", "C++"), Ordering::Equal);
        assert_eq!(cmp_ascii_ignore_case("7z", "zip"), Ordering::Less);
        assert_eq!(cmp_ascii_ignore_case("file", "FILE"), Ordering::Equal);
    }
}

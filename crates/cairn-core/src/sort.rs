// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-column sorting and presorted index management.

use crate::arena::NamePool;
use crate::engine::CancellationToken;
use crate::model::FileRecord;
use crate::path_cache::DirectoryPathCache;
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
    Extension,
}

impl SortColumn {
    /// The stable spelling shared by the CLI `--sort` value and the stored settings key.
    pub const fn key(self) -> &'static str {
        match self {
            SortColumn::Name => "name",
            SortColumn::Path => "path",
            SortColumn::Size => "size",
            SortColumn::DateModified => "modified",
            SortColumn::DateCreated => "created",
            SortColumn::Extension => "ext",
        }
    }
}

/// A sort key that names no [`SortColumn`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownSortColumn(String);

impl fmt::Display for UnknownSortColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown sort column `{}`; expected name, path, size, modified, created, or ext",
            self.0
        )
    }
}

impl std::error::Error for UnknownSortColumn {}

impl FromStr for SortColumn {
    type Err = UnknownSortColumn;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "name" => Ok(SortColumn::Name),
            "path" => Ok(SortColumn::Path),
            "size" => Ok(SortColumn::Size),
            "modified" | "datemodified" => Ok(SortColumn::DateModified),
            "created" | "datecreated" => Ok(SortColumn::DateCreated),
            "ext" | "extension" => Ok(SortColumn::Extension),
            _ => Err(UnknownSortColumn(value.to_string())),
        }
    }
}

/// The wire spelling is the released one: a positional postcard field must keep the same shape
/// when the Rust type is narrowed, so the enum travels as its key string and an unknown value
/// fails the frame instead of silently becoming the default.
impl Serialize for SortColumn {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for SortColumn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

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

#[derive(Clone, Copy)]
struct NameSortEntry {
    idx: u32,
    name_offset: u32,
    name_len: u16,
    safe_len: u8,
    prefix: [u8; 7],
}

#[inline]
fn make_name_entry(rec: &FileRecord, idx: u32, arena: &NamePool) -> NameSortEntry {
    let raw = arena.get_name(rec.name_offset, rec.name_len).as_bytes();
    let mut prefix = [0u8; 7];
    let mut safe_len = 0usize;
    let limit = raw.len().min(7);
    while safe_len < limit {
        let b = raw[safe_len];
        if b >= 0x80 || b.is_ascii_digit() {
            break;
        }
        prefix[safe_len] = b.to_ascii_lowercase();
        safe_len += 1;
    }
    NameSortEntry {
        idx,
        name_offset: rec.name_offset,
        name_len: rec.name_len,
        safe_len: safe_len as u8,
        prefix,
    }
}

#[inline]
fn cmp_name_entries(a: &NameSortEntry, b: &NameSortEntry, arena: &NamePool) -> Ordering {
    let k = a.safe_len.min(b.safe_len) as usize;
    if k > 0 {
        let ord = a.prefix[..k].cmp(&b.prefix[..k]);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    let na = arena.get_name(a.name_offset, a.name_len);
    let nb = arena.get_name(b.name_offset, b.name_len);
    natural_cmp(na, nb)
}

/// The column's ascending order, including the name tiebreak the column builders use.
fn cmp_indices(
    col: SortColumn,
    a: u32,
    b: u32,
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
) -> Ordering {
    let ra = &records[a as usize];
    let rb = &records[b as usize];
    match col {
        SortColumn::Name => cmp_name_entries(
            &make_name_entry(ra, a, arena),
            &make_name_entry(rb, b, arena),
            arena,
        ),
        SortColumn::Size => ra.size.cmp(&rb.size).then_with(|| {
            cmp_name_entries(
                &make_name_entry(ra, a, arena),
                &make_name_entry(rb, b, arena),
                arena,
            )
        }),
        SortColumn::DateModified => ra.modified_ts.cmp(&rb.modified_ts).then_with(|| {
            cmp_name_entries(
                &make_name_entry(ra, a, arena),
                &make_name_entry(rb, b, arena),
                arena,
            )
        }),
        SortColumn::Path => {
            let pa = path_cache.get_parent_path(records, arena, drive_letter, a);
            let pb = path_cache.get_parent_path(records, arena, drive_letter, b);
            pa.cmp(&pb).then_with(|| {
                natural_cmp(
                    arena.get_name(ra.name_offset, ra.name_len),
                    arena.get_name(rb.name_offset, rb.name_len),
                )
            })
        }
        SortColumn::DateCreated | SortColumn::Extension => Ordering::Equal,
    }
}

#[inline]
fn apply_sort_dir(ord: Ordering, dir: SortDirection) -> Ordering {
    if dir == SortDirection::Descending {
        ord.reverse()
    } else {
        ord
    }
}

const PACKED_SORT_THRESHOLD: usize = 1024;

impl PresortedIndices {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Clears all columns and releases backing buffers immediately.
    pub fn clear_and_shrink(&mut self) {
        self.clear();
    }

    /// Returns allocated backing bytes across all built presorted columns.
    pub fn allocated_bytes(&self) -> usize {
        (self.by_name.capacity()
            + self.by_size.capacity()
            + self.by_date_modified.capacity()
            + self.by_path.capacity())
            * std::mem::size_of::<u32>()
    }

    /// Ensures a single requested column's presorted index is built on-demand. Only the name,
    /// size, modified and path columns have presorted arrays; `DateCreated` and `Extension`
    /// leave the indices untouched, so callers sort those columns directly.
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
                let mut entries: Vec<NameSortEntry> = (0..count)
                    .into_par_iter()
                    .map(|idx| make_name_entry(&records[idx as usize], idx, arena))
                    .collect();
                entries.par_sort_unstable_by(|a, b| {
                    cmp_name_entries(a, b, arena).then_with(|| a.idx.cmp(&b.idx))
                });
                let mut by_name: Vec<u32> = Vec::with_capacity(count as usize);
                by_name.extend(entries.into_iter().map(|e| e.idx));
                self.by_name = by_name;
            }
            SortColumn::Size if self.by_size.is_empty() => {
                let mut entries: Vec<(u64, NameSortEntry)> = (0..count)
                    .into_par_iter()
                    .map(|idx| {
                        let rec = &records[idx as usize];
                        (rec.size, make_name_entry(rec, idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(sa, na), (sb, nb)| {
                    sa.cmp(sb)
                        .then_with(|| cmp_name_entries(na, nb, arena))
                        .then_with(|| na.idx.cmp(&nb.idx))
                });
                let mut by_size: Vec<u32> = Vec::with_capacity(count as usize);
                by_size.extend(entries.into_iter().map(|(_, n)| n.idx));
                self.by_size = by_size;
            }
            SortColumn::DateModified if self.by_date_modified.is_empty() => {
                let mut entries: Vec<(i64, NameSortEntry)> = (0..count)
                    .into_par_iter()
                    .map(|idx| {
                        let rec = &records[idx as usize];
                        (rec.modified_ts, make_name_entry(rec, idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(ma, na), (mb, nb)| {
                    ma.cmp(mb)
                        .then_with(|| cmp_name_entries(na, nb, arena))
                        .then_with(|| na.idx.cmp(&nb.idx))
                });
                let mut by_date: Vec<u32> = Vec::with_capacity(count as usize);
                by_date.extend(entries.into_iter().map(|(_, n)| n.idx));
                self.by_date_modified = by_date;
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

    /// Merges records whose key changed since the last build into the cached column. `changed`
    /// holds both records new to the table and records whose old position must go: `removed`
    /// answers whether a base index is one of the latter. The base stays sorted, so one pass
    /// combines it with the sorted change list.
    #[allow(clippy::too_many_arguments)]
    pub fn merge_changed(
        &mut self,
        col: SortColumn,
        changed: &mut Vec<u32>,
        removed: impl Fn(u32) -> bool,
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
    ) {
        let base = match col {
            SortColumn::Name => &mut self.by_name,
            SortColumn::Size => &mut self.by_size,
            SortColumn::DateModified => &mut self.by_date_modified,
            SortColumn::Path => &mut self.by_path,
            SortColumn::DateCreated | SortColumn::Extension => return,
        };
        changed.sort_unstable_by(|&a, &b| {
            cmp_indices(col, a, b, records, arena, path_cache, drive_letter).then_with(|| a.cmp(&b))
        });
        let base_len = base.len();
        let mut merged = Vec::with_capacity(base_len + changed.len());
        let (mut left, mut right) = (0usize, 0usize);
        loop {
            while left < base_len && removed(base[left]) {
                left += 1;
            }
            if left >= base_len || right >= changed.len() {
                break;
            }
            let ord = cmp_indices(
                col,
                base[left],
                changed[right],
                records,
                arena,
                path_cache,
                drive_letter,
            )
            .then_with(|| base[left].cmp(&changed[right]));
            if ord == Ordering::Greater {
                merged.push(changed[right]);
                right += 1;
            } else {
                merged.push(base[left]);
                left += 1;
            }
        }
        while left < base_len {
            if !removed(base[left]) {
                merged.push(base[left]);
            }
            left += 1;
        }
        merged.extend_from_slice(&changed[right..]);
        *base = merged;
        changed.clear();
    }

    /// Builds the four presorted columns (name, size, modified, path). Each column sorts in
    /// parallel; the columns themselves are built one after another.
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

    if indices.len() >= PACKED_SORT_THRESHOLD {
        match col {
            SortColumn::Name => {
                let mut entries: Vec<NameSortEntry> = indices
                    .par_iter()
                    .map(|&idx| make_name_entry(&records[idx as usize], idx, arena))
                    .collect();
                entries.par_sort_unstable_by(|a, b| {
                    apply_sort_dir(cmp_name_entries(a, b, arena), dir)
                        .then_with(|| a.idx.cmp(&b.idx))
                });
                for (dst, entry) in indices.iter_mut().zip(entries) {
                    *dst = entry.idx;
                }
                return;
            }
            SortColumn::Path => {
                let mut entries: Vec<(std::sync::Arc<str>, NameSortEntry)> = indices
                    .par_iter()
                    .map(|&idx| {
                        let parent = path_cache.get_parent_path(records, arena, drive_letter, idx);
                        (parent, make_name_entry(&records[idx as usize], idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(pa, na), (pb, nb)| {
                    apply_sort_dir(
                        pa.cmp(pb).then_with(|| cmp_name_entries(na, nb, arena)),
                        dir,
                    )
                    .then_with(|| na.idx.cmp(&nb.idx))
                });
                for (dst, (_, n)) in indices.iter_mut().zip(entries) {
                    *dst = n.idx;
                }
                return;
            }
            SortColumn::Size => {
                let mut entries: Vec<(u64, NameSortEntry)> = indices
                    .par_iter()
                    .map(|&idx| {
                        let rec = &records[idx as usize];
                        (rec.size, make_name_entry(rec, idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(sa, na), (sb, nb)| {
                    apply_sort_dir(
                        sa.cmp(sb).then_with(|| cmp_name_entries(na, nb, arena)),
                        dir,
                    )
                    .then_with(|| na.idx.cmp(&nb.idx))
                });
                for (dst, (_, n)) in indices.iter_mut().zip(entries) {
                    *dst = n.idx;
                }
                return;
            }
            SortColumn::DateModified => {
                let mut entries: Vec<(i64, NameSortEntry)> = indices
                    .par_iter()
                    .map(|&idx| {
                        let rec = &records[idx as usize];
                        (rec.modified_ts, make_name_entry(rec, idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(ma, na), (mb, nb)| {
                    apply_sort_dir(
                        ma.cmp(mb).then_with(|| cmp_name_entries(na, nb, arena)),
                        dir,
                    )
                    .then_with(|| na.idx.cmp(&nb.idx))
                });
                for (dst, (_, n)) in indices.iter_mut().zip(entries) {
                    *dst = n.idx;
                }
                return;
            }
            SortColumn::DateCreated => {
                let mut entries: Vec<(i64, NameSortEntry)> = indices
                    .par_iter()
                    .map(|&idx| {
                        let rec = &records[idx as usize];
                        (rec.created_ts, make_name_entry(rec, idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(ca, na), (cb, nb)| {
                    apply_sort_dir(
                        ca.cmp(cb).then_with(|| cmp_name_entries(na, nb, arena)),
                        dir,
                    )
                    .then_with(|| na.idx.cmp(&nb.idx))
                });
                for (dst, (_, n)) in indices.iter_mut().zip(entries) {
                    *dst = n.idx;
                }
                return;
            }
            SortColumn::Extension => {
                let mut entries: Vec<(u16, NameSortEntry)> = indices
                    .par_iter()
                    .map(|&idx| {
                        let rec = &records[idx as usize];
                        (rec.ext_offset, make_name_entry(rec, idx, arena))
                    })
                    .collect();
                entries.par_sort_unstable_by(|(ea_off, na), (eb_off, nb)| {
                    let ea = arena.get_ext(na.name_offset, na.name_len, *ea_off);
                    let eb = arena.get_ext(nb.name_offset, nb.name_len, *eb_off);
                    apply_sort_dir(
                        cmp_ascii_ignore_case(ea, eb).then_with(|| cmp_name_entries(na, nb, arena)),
                        dir,
                    )
                    .then_with(|| na.idx.cmp(&nb.idx))
                });
                for (dst, (_, n)) in indices.iter_mut().zip(entries) {
                    *dst = n.idx;
                }
                return;
            }
        }
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
    fn every_sort_column_round_trips_through_its_key() {
        for column in [
            SortColumn::Name,
            SortColumn::Path,
            SortColumn::Size,
            SortColumn::DateModified,
            SortColumn::DateCreated,
            SortColumn::Extension,
        ] {
            assert_eq!(column.key().parse::<SortColumn>(), Ok(column));
        }
        assert_eq!("Name".parse::<SortColumn>(), Ok(SortColumn::Name));
        assert_eq!(
            "DATEMODIFIED".parse::<SortColumn>(),
            Ok(SortColumn::DateModified)
        );
        assert_eq!("extension".parse::<SortColumn>(), Ok(SortColumn::Extension));
    }

    #[test]
    fn an_unknown_sort_key_is_refused() {
        let error = "bogus".parse::<SortColumn>().expect_err("unknown key");
        assert!(error.to_string().contains("bogus"));
    }

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

    #[test]
    fn presorted_clear_releases_backing_buffers() {
        let mut arena = NamePool::default();
        let mut records = Vec::new();
        for i in 0..2_000u64 {
            let (name_offset, name_len, ext_offset) = arena.insert(&format!("item_{i:04}.txt"));
            records.push(FileRecord {
                frn: i + 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset,
                name_len,
                ext_offset,
                volume_id: 0,
                flags: 0,
                size: i,
                modified_ts: i as i64,
                created_ts: i as i64,
            });
        }
        let cache = DirectoryPathCache::new();
        let mut presorted = PresortedIndices::default();
        presorted.ensure_column(SortColumn::Name, &records, &arena, &cache, 'C');
        assert!(presorted.allocated_bytes() >= 2_000 * std::mem::size_of::<u32>());

        presorted.clear();
        assert_eq!(
            presorted.allocated_bytes(),
            0,
            "PresortedIndices::clear must drop backing allocations instead of keeping empty Vec shells"
        );
    }

    #[test]
    fn sort_indices_large_slice_matches_reference_order() {
        let samples = [
            "alpha.txt",
            "Alpha.TXT",
            "alpha1.txt",
            "alpha2.txt",
            "alpha10.txt",
            "alpha01.txt",
            "beta_core.rs",
            "Beta_Core2.rs",
            "beta_core10.rs",
            "00start.log",
            "1start.log",
            "10start.log",
            "文档2.docx",
            "文档10.docx",
            "a",
            "a0",
            "a1",
            "a\x01",
            "",
        ];
        let mut arena = NamePool::default();
        let mut records = Vec::with_capacity(1_600);
        for i in 0..1_600usize {
            let base = samples[i % samples.len()];
            let name = if base.is_empty() {
                String::new()
            } else {
                format!("{base}_{}", i / samples.len())
            };
            let (name_offset, name_len, ext_offset) = arena.insert(&name);
            records.push(FileRecord {
                frn: i as u64 + 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset,
                name_len,
                ext_offset,
                volume_id: 0,
                flags: 0,
                size: ((i * 7919) % 500) as u64,
                modified_ts: ((i * 101) % 300) as i64,
                created_ts: ((i * 37) % 200) as i64,
            });
        }
        let cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        for col in [
            SortColumn::Name,
            SortColumn::Size,
            SortColumn::DateModified,
            SortColumn::DateCreated,
            SortColumn::Extension,
            SortColumn::Path,
        ] {
            for dir in [SortDirection::Ascending, SortDirection::Descending] {
                let mut got: Vec<u32> = (0..records.len() as u32).collect();
                sort_indices(&mut got, &records, &arena, &cache, 'C', col, dir, &cancel);

                let mut expected: Vec<u32> = (0..records.len() as u32).collect();
                expected.sort_by(|&a, &b| {
                    let ra = &records[a as usize];
                    let rb = &records[b as usize];
                    let na = arena.get_name(ra.name_offset, ra.name_len);
                    let nb = arena.get_name(rb.name_offset, rb.name_len);
                    let ord = match col {
                        SortColumn::Name => natural_cmp(na, nb),
                        SortColumn::Size => ra.size.cmp(&rb.size).then_with(|| natural_cmp(na, nb)),
                        SortColumn::DateModified => ra
                            .modified_ts
                            .cmp(&rb.modified_ts)
                            .then_with(|| natural_cmp(na, nb)),
                        SortColumn::DateCreated => ra
                            .created_ts
                            .cmp(&rb.created_ts)
                            .then_with(|| natural_cmp(na, nb)),
                        SortColumn::Extension => cmp_ascii_ignore_case(
                            arena.get_ext(ra.name_offset, ra.name_len, ra.ext_offset),
                            arena.get_ext(rb.name_offset, rb.name_len, rb.ext_offset),
                        )
                        .then_with(|| natural_cmp(na, nb)),
                        SortColumn::Path => cache
                            .get_parent_path(&records, &arena, 'C', a)
                            .cmp(&cache.get_parent_path(&records, &arena, 'C', b))
                            .then_with(|| natural_cmp(na, nb)),
                    };
                    let primary = if dir == SortDirection::Descending {
                        ord.reverse()
                    } else {
                        ord
                    };
                    primary.then_with(|| a.cmp(&b))
                });
                assert_eq!(got, expected, "mismatch for col={col:?} dir={dir:?}");
            }
        }
    }
}

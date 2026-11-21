// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-column sorting and presorted index management.

use crate::arena::NamePool;
use crate::engine::CancellationToken;
use crate::model::FileRecord;
use crate::path_cache::{DirectoryPathCache, root_path_for_drive};
use crate::property_store::{PropertyKind, PropertySideTables, SourceId};
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
    Extension,
    Length,
    Author,
    Tags,
    Dimensions,
    Framerate,
}

impl SortColumn {
    /// Stable spelling shared by CLI `--sort`, wire serialization, and settings persistence.
    pub const fn key(self) -> &'static str {
        match self {
            SortColumn::Name => "name",
            SortColumn::Path => "path",
            SortColumn::Size => "size",
            SortColumn::DateModified => "modified",
            SortColumn::DateCreated => "created",
            SortColumn::Extension => "ext",
            SortColumn::Length => "length",
            SortColumn::Author => "author",
            SortColumn::Tags => "tags",
            SortColumn::Dimensions => "dimensions",
            SortColumn::Framerate => "framerate",
        }
    }

    /// Returns the required [`PropertyKind`] when this column reads from [`PropertySideTables`].
    pub const fn property_kind(self) -> Option<PropertyKind> {
        match self {
            SortColumn::Length => Some(PropertyKind::Length),
            SortColumn::Author => Some(PropertyKind::Author),
            SortColumn::Tags => Some(PropertyKind::Tags),
            SortColumn::Dimensions => Some(PropertyKind::Dimensions),
            SortColumn::Framerate => Some(PropertyKind::Framerate),
            _ => None,
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
            "unknown sort column `{}`; expected name, path, size, modified, created, ext, length, author, tags, dimensions, or framerate",
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
            "modified" | "datemodified" | "date-modified" | "date_modified" => {
                Ok(SortColumn::DateModified)
            }
            "created" | "datecreated" | "date-created" | "date_created" => {
                Ok(SortColumn::DateCreated)
            }
            "ext" | "extension" => Ok(SortColumn::Extension),
            "length" | "duration" => Ok(SortColumn::Length),
            "author" | "artist" => Ok(SortColumn::Author),
            "tags" | "tag" => Ok(SortColumn::Tags),
            "dimensions" | "resolution" => Ok(SortColumn::Dimensions),
            "framerate" | "fps" => Ok(SortColumn::Framerate),
            _ => Err(UnknownSortColumn(value.to_string())),
        }
    }
}

/// Wire serialization uses each column's stable lowercase key string and rejects unknown values.
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

/// A single column and direction within a [`SortSpec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub column: SortColumn,
    pub direction: SortDirection,
}

impl SortKey {
    pub const fn new(column: SortColumn, direction: SortDirection) -> Self {
        Self { column, direction }
    }
}

/// Ordered list of sort keys applied from highest to lowest precedence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSpec {
    pub keys: Vec<SortKey>,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self::single(SortColumn::Name, SortDirection::Ascending)
    }
}

impl SortSpec {
    pub fn single(column: SortColumn, direction: SortDirection) -> Self {
        Self {
            keys: vec![SortKey::new(column, direction)],
        }
    }

    pub fn primary(&self) -> SortKey {
        self.keys
            .first()
            .copied()
            .unwrap_or_else(|| SortKey::new(SortColumn::Name, SortDirection::Ascending))
    }

    /// Toggles direction when already sorted by `column` alone, or replaces with a single key.
    pub fn set_or_toggle_primary(&mut self, column: SortColumn, initial: SortDirection) {
        if self.keys.len() == 1 && self.keys[0].column == column {
            self.keys[0].direction = self.keys[0].direction.flipped();
        } else {
            self.keys.clear();
            self.keys.push(SortKey::new(column, initial));
        }
    }

    /// Appends `column` as a tiebreaker key or flips its direction when already present.
    pub fn shift_toggle_column(&mut self, column: SortColumn, initial: SortDirection) {
        if let Some(existing) = self.keys.iter_mut().find(|k| k.column == column) {
            existing.direction = existing.direction.flipped();
        } else {
            self.keys.push(SortKey::new(column, initial));
        }
    }

    /// Parses comma/semicolon-separated sort clauses such as `-size,+name` or `size-desc,name-asc`.
    pub fn parse_clause(raw: &str) -> Result<Self, UnknownSortColumn> {
        let mut keys = Vec::new();
        for part in raw.split([',', ';']) {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            let key = parse_sort_key_token(trimmed)?;
            if !keys.iter().any(|k: &SortKey| k.column == key.column) {
                keys.push(key);
            }
        }
        if keys.is_empty() {
            return Err(UnknownSortColumn(raw.to_string()));
        }
        Ok(Self { keys })
    }

    /// Formats the specification in canonical `column-dir,...` notation.
    pub fn to_clause(&self) -> String {
        self.keys
            .iter()
            .map(|k| {
                let d = match k.direction {
                    SortDirection::Ascending => "asc",
                    SortDirection::Descending => "desc",
                };
                format!("{}-{d}", k.column.key())
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn parse_sort_key_token(token: &str) -> Result<SortKey, UnknownSortColumn> {
    if let Some(rest) = token.strip_prefix('-') {
        return Ok(SortKey::new(
            rest.trim().parse()?,
            SortDirection::Descending,
        ));
    }
    if let Some(rest) = token.strip_prefix('+') {
        return Ok(SortKey::new(rest.trim().parse()?, SortDirection::Ascending));
    }
    let lower = token.to_ascii_lowercase();
    for (suffix, dir) in [
        (":desc", SortDirection::Descending),
        (":descending", SortDirection::Descending),
        ("-desc", SortDirection::Descending),
        ("-descending", SortDirection::Descending),
        (" desc", SortDirection::Descending),
        (" descending", SortDirection::Descending),
        (":asc", SortDirection::Ascending),
        (":ascending", SortDirection::Ascending),
        ("-asc", SortDirection::Ascending),
        ("-ascending", SortDirection::Ascending),
        (" asc", SortDirection::Ascending),
        (" ascending", SortDirection::Ascending),
    ] {
        if let Some(prefix_len) = lower.len().checked_sub(suffix.len())
            && &lower[prefix_len..] == suffix
        {
            return Ok(SortKey::new(token[..prefix_len].trim().parse()?, dir));
        }
    }
    Ok(SortKey::new(token.parse()?, SortDirection::Ascending))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

impl SortDirection {
    #[inline]
    pub const fn flipped(self) -> Self {
        match self {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        }
    }
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

/// Immutable inputs required for sorting record indices across columns and properties.
#[derive(Clone, Copy)]
pub struct SortContext<'a> {
    pub records: &'a [FileRecord],
    pub arena: &'a NamePool,
    pub path_cache: &'a DirectoryPathCache,
    pub root: &'a Arc<str>,
    pub source_id: SourceId,
    pub properties: Option<&'a PropertySideTables>,
}

impl<'a> SortContext<'a> {
    pub fn for_root(
        records: &'a [FileRecord],
        arena: &'a NamePool,
        path_cache: &'a DirectoryPathCache,
        root: &'a Arc<str>,
    ) -> Self {
        Self {
            records,
            arena,
            path_cache,
            root,
            source_id: SourceId::default(),
            properties: None,
        }
    }
}

#[inline]
fn cmp_name_only(records: &[FileRecord], arena: &NamePool, a: u32, b: u32) -> Ordering {
    let ra = &records[a as usize];
    let rb = &records[b as usize];
    natural_cmp(
        arena.get_name(ra.name_offset, ra.name_len),
        arena.get_name(rb.name_offset, rb.name_len),
    )
}

#[inline]
fn cmp_opt_property<T>(
    oa: Option<T>,
    ob: Option<T>,
    dir: SortDirection,
    cmp: impl FnOnce(T, T) -> Ordering,
) -> Ordering {
    match (oa, ob) {
        (Some(va), Some(vb)) => apply_sort_dir(cmp(va, vb), dir),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Compares record `a` from `ctx_a` against record `b` from `ctx_b` solely by `(col, dir)`.
fn cmp_column_between(
    ctx_a: &SortContext<'_>,
    a: u32,
    ctx_b: &SortContext<'_>,
    b: u32,
    col: SortColumn,
    dir: SortDirection,
) -> Ordering {
    let ra = &ctx_a.records[a as usize];
    let rb = &ctx_b.records[b as usize];
    match col {
        SortColumn::Name => {
            let na = ctx_a.arena.get_name(ra.name_offset, ra.name_len);
            let nb = ctx_b.arena.get_name(rb.name_offset, rb.name_len);
            apply_sort_dir(natural_cmp(na, nb), dir)
        }
        SortColumn::Path => {
            let pa = ctx_a.path_cache.get_parent_path_with_root(
                ctx_a.records,
                ctx_a.arena,
                ctx_a.root,
                a,
            );
            let pb = ctx_b.path_cache.get_parent_path_with_root(
                ctx_b.records,
                ctx_b.arena,
                ctx_b.root,
                b,
            );
            apply_sort_dir(pa.cmp(&pb), dir)
        }
        SortColumn::Size => apply_sort_dir(ra.size.cmp(&rb.size), dir),
        SortColumn::DateModified => apply_sort_dir(ra.modified_ts.cmp(&rb.modified_ts), dir),
        SortColumn::DateCreated => apply_sort_dir(ra.created_ts.cmp(&rb.created_ts), dir),
        SortColumn::Extension => {
            let ea = ctx_a
                .arena
                .get_ext(ra.name_offset, ra.name_len, ra.ext_offset);
            let eb = ctx_b
                .arena
                .get_ext(rb.name_offset, rb.name_len, rb.ext_offset);
            apply_sort_dir(cmp_ascii_ignore_case(ea, eb), dir)
        }
        SortColumn::Length
        | SortColumn::Author
        | SortColumn::Tags
        | SortColumn::Dimensions
        | SortColumn::Framerate => {
            let pa = ctx_a
                .properties
                .and_then(|p| p.get(ctx_a.source_id, ra.frn, ra.modified_ts));
            let pb = ctx_b
                .properties
                .and_then(|p| p.get(ctx_b.source_id, rb.frn, rb.modified_ts));
            match col {
                SortColumn::Length => cmp_opt_property(
                    pa.and_then(|p| p.length_ms),
                    pb.and_then(|p| p.length_ms),
                    dir,
                    |va, vb| va.cmp(&vb),
                ),
                SortColumn::Author => cmp_opt_property(
                    pa.and_then(|p| p.author),
                    pb.and_then(|p| p.author),
                    dir,
                    natural_cmp,
                ),
                SortColumn::Tags => cmp_opt_property(
                    pa.and_then(|p| p.tags),
                    pb.and_then(|p| p.tags),
                    dir,
                    natural_cmp,
                ),
                SortColumn::Dimensions => cmp_opt_property(
                    pa.and_then(|p| p.dimensions),
                    pb.and_then(|p| p.dimensions),
                    dir,
                    |(wa, ha), (wb, hb)| {
                        (wa as u64 * ha as u64)
                            .cmp(&(wb as u64 * hb as u64))
                            .then_with(|| wa.cmp(&wb))
                    },
                ),
                SortColumn::Framerate => cmp_opt_property(
                    pa.and_then(|p| p.framerate_mfps),
                    pb.and_then(|p| p.framerate_mfps),
                    dir,
                    |va, vb| va.cmp(&vb),
                ),
                SortColumn::Name
                | SortColumn::Path
                | SortColumn::Size
                | SortColumn::DateModified
                | SortColumn::DateCreated
                | SortColumn::Extension => unreachable!(),
            }
        }
    }
}

/// Compares record `a` from `ctx_a` against record `b` from `ctx_b` using an ordered multi-key [`SortSpec`].
pub fn compare_cross_source_records(
    ctx_a: &SortContext<'_>,
    a: u32,
    ctx_b: &SortContext<'_>,
    b: u32,
    spec: &SortSpec,
) -> Ordering {
    for key in &spec.keys {
        let ord = cmp_column_between(ctx_a, a, ctx_b, b, key.column, key.direction);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    if spec.keys.iter().any(|k| k.column == SortColumn::Name) {
        Ordering::Equal
    } else {
        cmp_column_between(
            ctx_a,
            a,
            ctx_b,
            b,
            SortColumn::Name,
            SortDirection::Ascending,
        )
    }
}

/// Compares `a` and `b` solely by `col` in `dir`, without tiebreak fallback.
fn cmp_column_only(
    ctx: &SortContext<'_>,
    col: SortColumn,
    dir: SortDirection,
    a: u32,
    b: u32,
) -> Ordering {
    cmp_column_between(ctx, a, ctx, b, col, dir)
}

/// The column's ascending order, including the name tiebreak the presorted column builders use.
fn cmp_indices(col: SortColumn, a: u32, b: u32, ctx: &SortContext<'_>) -> Ordering {
    if col == SortColumn::Name {
        return cmp_name_entries(
            &make_name_entry(&ctx.records[a as usize], a, ctx.arena),
            &make_name_entry(&ctx.records[b as usize], b, ctx.arena),
            ctx.arena,
        );
    }
    cmp_column_only(ctx, col, SortDirection::Ascending, a, b).then_with(|| {
        cmp_name_entries(
            &make_name_entry(&ctx.records[a as usize], a, ctx.arena),
            &make_name_entry(&ctx.records[b as usize], b, ctx.arena),
            ctx.arena,
        )
    })
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

fn build_presorted_by_key<K: Send + Sync>(
    count: u32,
    records: &[FileRecord],
    arena: &NamePool,
    make_key: impl Fn(u32, &FileRecord) -> K + Sync,
    cmp_key: impl Fn(&K, &K) -> Ordering + Sync,
) -> Vec<u32> {
    let mut entries: Vec<(K, NameSortEntry)> = (0..count)
        .into_par_iter()
        .map(|idx| {
            let rec = &records[idx as usize];
            (make_key(idx, rec), make_name_entry(rec, idx, arena))
        })
        .collect();
    entries.par_sort_unstable_by(|(ka, na), (kb, nb)| {
        cmp_key(ka, kb)
            .then_with(|| cmp_name_entries(na, nb, arena))
            .then_with(|| na.idx.cmp(&nb.idx))
    });
    let mut out = Vec::with_capacity(count as usize);
    out.extend(entries.into_iter().map(|(_, n)| n.idx));
    out
}

fn packed_sort_by_key<K: Send + Sync>(
    indices: &mut [u32],
    records: &[FileRecord],
    arena: &NamePool,
    dir: SortDirection,
    make_key: impl Fn(u32, &FileRecord) -> K + Sync,
    cmp_key: impl Fn(&K, &K) -> Ordering + Sync,
) {
    let mut entries: Vec<(K, NameSortEntry)> = indices
        .par_iter()
        .map(|&idx| {
            let rec = &records[idx as usize];
            (make_key(idx, rec), make_name_entry(rec, idx, arena))
        })
        .collect();
    entries.par_sort_unstable_by(|(ka, na), (kb, nb)| {
        apply_sort_dir(
            cmp_key(ka, kb).then_with(|| cmp_name_entries(na, nb, arena)),
            dir,
        )
        .then_with(|| na.idx.cmp(&nb.idx))
    });
    for (dst, (_, n)) in indices.iter_mut().zip(entries) {
        *dst = n.idx;
    }
}

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

    /// Ensures a single resident presorted column (`Name`, `Size`, `DateModified`, or `Path`) is built.
    pub fn ensure_column(
        &mut self,
        col: SortColumn,
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
    ) {
        let root = root_path_for_drive(drive_letter);
        self.ensure_column_with_root(col, records, arena, path_cache, &root);
    }

    /// Variant of [`Self::ensure_column`] accepting an arbitrary pre-normalized `root`.
    pub fn ensure_column_with_root(
        &mut self,
        col: SortColumn,
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        root: &Arc<str>,
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
                let mut by_name = Vec::with_capacity(count as usize);
                by_name.extend(entries.into_iter().map(|e| e.idx));
                self.by_name = by_name;
            }
            SortColumn::Size if self.by_size.is_empty() => {
                self.by_size =
                    build_presorted_by_key(count, records, arena, |_, r| r.size, Ord::cmp);
            }
            SortColumn::DateModified if self.by_date_modified.is_empty() => {
                self.by_date_modified =
                    build_presorted_by_key(count, records, arena, |_, r| r.modified_ts, Ord::cmp);
            }
            SortColumn::Path if self.by_path.is_empty() => {
                let mut by_path: Vec<u32> = (0..count).collect();
                by_path.par_sort_unstable_by(|&a, &b| {
                    let pa = path_cache.get_parent_path_with_root(records, arena, root, a);
                    let pb = path_cache.get_parent_path_with_root(records, arena, root, b);
                    pa.cmp(&pb)
                        .then_with(|| cmp_name_only(records, arena, a, b))
                        .then_with(|| a.cmp(&b))
                });
                self.by_path = by_path;
            }
            _ => {}
        }
    }

    /// Merges changed/inserted records into the cached column, filtering out `removed` indices.
    pub fn merge_changed(
        &mut self,
        col: SortColumn,
        changed: &mut Vec<u32>,
        removed: impl Fn(u32) -> bool,
        ctx: &SortContext<'_>,
    ) {
        let base = match col {
            SortColumn::Name => &mut self.by_name,
            SortColumn::Size => &mut self.by_size,
            SortColumn::DateModified => &mut self.by_date_modified,
            SortColumn::Path => &mut self.by_path,
            _ => return,
        };
        changed.sort_unstable_by(|&a, &b| cmp_indices(col, a, b, ctx).then_with(|| a.cmp(&b)));
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
            let ord = cmp_indices(col, base[left], changed[right], ctx)
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

    /// Builds the four resident presorted columns (`Name`, `Size`, `DateModified`, `Path`).
    pub fn build(
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
    ) -> Self {
        let root = root_path_for_drive(drive_letter);
        Self::build_with_root(records, arena, path_cache, &root)
    }

    /// Variant of [`Self::build`] accepting an arbitrary pre-normalized `root`.
    pub fn build_with_root(
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        root: &Arc<str>,
    ) -> Self {
        let mut indices = Self::default();
        indices.ensure_column_with_root(SortColumn::Name, records, arena, path_cache, root);
        indices.ensure_column_with_root(SortColumn::Size, records, arena, path_cache, root);
        indices.ensure_column_with_root(SortColumn::DateModified, records, arena, path_cache, root);
        indices.ensure_column_with_root(SortColumn::Path, records, arena, path_cache, root);
        indices
    }
}

fn sort_single_column(
    indices: &mut [u32],
    ctx: &SortContext<'_>,
    col: SortColumn,
    dir: SortDirection,
) {
    if col.property_kind().is_some() {
        indices.par_sort_unstable_by(|&a, &b| {
            cmp_column_only(ctx, col, dir, a, b)
                .then_with(|| cmp_name_only(ctx.records, ctx.arena, a, b))
                .then_with(|| a.cmp(&b))
        });
        return;
    }

    if indices.len() >= PACKED_SORT_THRESHOLD {
        match col {
            SortColumn::Name => {
                let mut entries: Vec<NameSortEntry> = indices
                    .par_iter()
                    .map(|&idx| make_name_entry(&ctx.records[idx as usize], idx, ctx.arena))
                    .collect();
                entries.par_sort_unstable_by(|a, b| {
                    apply_sort_dir(cmp_name_entries(a, b, ctx.arena), dir)
                        .then_with(|| a.idx.cmp(&b.idx))
                });
                for (dst, entry) in indices.iter_mut().zip(entries) {
                    *dst = entry.idx;
                }
            }
            SortColumn::Path => packed_sort_by_key(
                indices,
                ctx.records,
                ctx.arena,
                dir,
                |idx, _| {
                    ctx.path_cache
                        .get_parent_path_with_root(ctx.records, ctx.arena, ctx.root, idx)
                },
                Ord::cmp,
            ),
            SortColumn::Size => packed_sort_by_key(
                indices,
                ctx.records,
                ctx.arena,
                dir,
                |_, r| r.size,
                Ord::cmp,
            ),
            SortColumn::DateModified => packed_sort_by_key(
                indices,
                ctx.records,
                ctx.arena,
                dir,
                |_, r| r.modified_ts,
                Ord::cmp,
            ),
            SortColumn::DateCreated => packed_sort_by_key(
                indices,
                ctx.records,
                ctx.arena,
                dir,
                |_, r| r.created_ts,
                Ord::cmp,
            ),
            SortColumn::Extension => packed_sort_by_key(
                indices,
                ctx.records,
                ctx.arena,
                dir,
                |idx, r| (r.ext_offset, idx),
                |&(oa, ia), &(ob, ib)| {
                    let ra = &ctx.records[ia as usize];
                    let rb = &ctx.records[ib as usize];
                    let ea = ctx.arena.get_ext(ra.name_offset, ra.name_len, oa);
                    let eb = ctx.arena.get_ext(rb.name_offset, rb.name_len, ob);
                    cmp_ascii_ignore_case(ea, eb)
                },
            ),
            SortColumn::Length
            | SortColumn::Author
            | SortColumn::Tags
            | SortColumn::Dimensions
            | SortColumn::Framerate => unreachable!(),
        }
        return;
    }

    indices.par_sort_unstable_by(|&a, &b| {
        let ord = cmp_column_only(ctx, col, SortDirection::Ascending, a, b).then_with(|| {
            if col == SortColumn::Name {
                Ordering::Equal
            } else {
                cmp_name_only(ctx.records, ctx.arena, a, b)
            }
        });
        apply_sort_dir(ord, dir).then_with(|| a.cmp(&b))
    });
}

/// Sorts result indices by an ordered multi-key [`SortSpec`].
pub fn sort_indices_with_spec(
    indices: &mut [u32],
    ctx: &SortContext<'_>,
    spec: &SortSpec,
    cancel: &CancellationToken,
) {
    if indices.len() <= 1 || cancel.is_cancelled() {
        return;
    }
    if spec.keys.len() <= 1 {
        let key = spec.primary();
        sort_single_column(indices, ctx, key.column, key.direction);
        return;
    }

    let has_name = spec.keys.iter().any(|k| k.column == SortColumn::Name);
    indices.par_sort_unstable_by(|&a, &b| {
        for key in &spec.keys {
            let ord = cmp_column_only(ctx, key.column, key.direction, a, b);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        let tie = if has_name {
            Ordering::Equal
        } else {
            cmp_name_only(ctx.records, ctx.arena, a, b)
        };
        tie.then_with(|| a.cmp(&b))
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
            SortColumn::Length,
            SortColumn::Author,
            SortColumn::Tags,
            SortColumn::Dimensions,
            SortColumn::Framerate,
        ] {
            assert_eq!(column.key().parse::<SortColumn>(), Ok(column));
        }
        assert_eq!("Name".parse::<SortColumn>(), Ok(SortColumn::Name));
        assert_eq!(
            "DATEMODIFIED".parse::<SortColumn>(),
            Ok(SortColumn::DateModified)
        );
        assert_eq!("extension".parse::<SortColumn>(), Ok(SortColumn::Extension));
        assert_eq!("duration".parse::<SortColumn>(), Ok(SortColumn::Length));
        assert_eq!("fps".parse::<SortColumn>(), Ok(SortColumn::Framerate));
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
        let root = root_path_for_drive('C');
        let ctx = SortContext::for_root(&records, &arena, &path_cache, &root);
        let cancel = CancellationToken::new();

        // Size tiebreaks by Name ascending ("a.txt" before "b.txt") and then record index (1 before 2).
        let mut indices = vec![0, 1, 2];
        sort_indices_with_spec(
            &mut indices,
            &ctx,
            &SortSpec::single(SortColumn::Size, SortDirection::Ascending),
            &cancel,
        );
        assert_eq!(indices, vec![1, 2, 0]);

        indices = vec![0, 2, 1];
        sort_indices_with_spec(
            &mut indices,
            &ctx,
            &SortSpec::single(SortColumn::DateModified, SortDirection::Ascending),
            &cancel,
        );
        assert_eq!(indices, vec![1, 2, 0]);

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

        let root = root_path_for_drive('C');
        let ctx = SortContext::for_root(&records, &arena, &cache, &root);
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
                sort_indices_with_spec(&mut got, &ctx, &SortSpec::single(col, dir), &cancel);

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
                        SortColumn::Path => {
                            let root = crate::path_cache::root_path_for_drive('C');
                            cache
                                .get_parent_path_with_root(&records, &arena, &root, a)
                                .cmp(&cache.get_parent_path_with_root(&records, &arena, &root, b))
                                .then_with(|| natural_cmp(na, nb))
                        }
                        _ => unreachable!("loop covers non-property columns"),
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

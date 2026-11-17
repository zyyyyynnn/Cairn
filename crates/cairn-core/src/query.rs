// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! High-level query execution across indexed sources, extended properties, and multi-key sorting.

use crate::arena::NamePool;
use crate::engine::{
    execute_query_with_context, glob_match, CancellationToken, ContentHitResolver, EvalContext,
};
use crate::filter::DupeMode;
use crate::model::FileRecord;
use crate::path_cache::{normalize_root, DirectoryPathCache};
use crate::property_store::{PropertyKind, PropertySideTables, SourceId};
use crate::sort::{
    sort_indices_with_spec, PresortedIndices, SortColumn, SortContext, SortDirection, SortSpec,
};
use crate::syntax::{QueryExpr, SearchTerm};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Borrowed view of one indexed source (NTFS volume or folder source) supplied to [`execute_query`].
#[derive(Clone, Copy)]
pub struct SourceIndex<'a> {
    pub source_id: SourceId,
    pub root_prefix: &'a str,
    pub records: &'a [FileRecord],
    pub arena: &'a NamePool,
    pub path_cache: &'a DirectoryPathCache,
    pub presorted: Option<&'a PresortedIndices>,
    pub properties: &'a PropertySideTables,
    pub has_tombstones: bool,
}

/// Execution policy passed to [`execute_query`].
#[derive(Clone, Default)]
pub struct QueryPolicy<'a> {
    pub sort: SortSpec,
    pub content_hit: Option<ContentHitResolver<'a>>,
}

/// Explicit degradation reason emitted when an extended capability was requested while disabled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryDegradedReason {
    PropertyIndexDisabled { requested: Vec<PropertyKind> },
}

/// Resulting row sequence produced by [`execute_query`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryRows {
    /// Zero-materialization view over an intact presorted index column.
    PresortedAll {
        column: SortColumn,
        descending: bool,
        len: usize,
    },
    /// Materialized matching record indices in final sort order.
    Materialized(Vec<u32>),
}

/// Complete output of [`execute_query`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryResult {
    pub rows: QueryRows,
    pub applied_sort: SortSpec,
    pub highlight_terms: Vec<String>,
    pub degraded: Option<QueryDegradedReason>,
}

/// Evaluates `expr` against `source` honoring `policy` and `cancel`.
pub fn execute_query(
    source: &SourceIndex<'_>,
    expr: &QueryExpr,
    policy: &QueryPolicy<'_>,
    cancel: &CancellationToken,
) -> QueryResult {
    let mut applied_sort = expr
        .extract_sort_spec()
        .unwrap_or_else(|| policy.sort.clone());
    if applied_sort.keys.is_empty() {
        applied_sort = SortSpec::default();
    }

    let highlight_terms = expr.collect_highlight_terms();
    let degraded = detect_degraded_properties(source.properties, expr, &applied_sort);

    if cancel.is_cancelled() {
        return QueryResult {
            rows: QueryRows::Materialized(Vec::new()),
            applied_sort,
            highlight_terms,
            degraded,
        };
    }

    if !source.properties.is_enabled() && definitely_empty_without_properties(expr) {
        return QueryResult {
            rows: QueryRows::Materialized(Vec::new()),
            applied_sort,
            highlight_terms,
            degraded,
        };
    }

    let root = normalize_root(source.root_prefix);
    if expr.is_unfiltered() && !source.has_tombstones && applied_sort.keys.len() == 1 {
        let key = applied_sort.primary();
        if let Some(ps) = source.presorted {
            if let Some(len) = presorted_column_len(ps, key.column) {
                return QueryResult {
                    rows: QueryRows::PresortedAll {
                        column: key.column,
                        descending: key.direction == SortDirection::Descending,
                        len,
                    },
                    applied_sort,
                    highlight_terms,
                    degraded,
                };
            }
        }
    }

    let hierarchy = expr
        .uses_hierarchy()
        .then(|| HierarchyOracle::build(source.records, source.arena, &root, expr));
    let dupes = expr
        .uses_dupes()
        .then(|| DupeOracle::build(source.records, source.arena, expr));

    let extra_eval = |term: &SearchTerm, rec: &FileRecord, idx: u32| -> bool {
        eval_extra_term(term, rec, idx, source, hierarchy.as_ref(), dupes.as_ref())
    };

    let drive_letter = drive_letter_from_root(&root);
    let eval_ctx = EvalContext {
        records: source.records,
        arena: source.arena,
        path_cache: source.path_cache,
        root: &root,
        drive_letter,
        content_hit: policy.content_hit,
        extra_term_eval: Some(&extra_eval),
    };

    let eval_expr = if expr.is_unfiltered() {
        QueryExpr::And(Vec::new())
    } else {
        expr.clone()
    };
    let mut matched = execute_query_with_context(&eval_ctx, &eval_expr, None, cancel);

    if !cancel.is_cancelled() && matched.len() > 1 {
        let sort_ctx = SortContext {
            records: source.records,
            arena: source.arena,
            path_cache: source.path_cache,
            root: &root,
            source_id: source.source_id,
            properties: Some(source.properties),
        };
        sort_indices_with_spec(&mut matched, &sort_ctx, &applied_sort, cancel);
    }

    QueryResult {
        rows: QueryRows::Materialized(matched),
        applied_sort,
        highlight_terms,
        degraded,
    }
}

fn presorted_column_len(ps: &PresortedIndices, col: SortColumn) -> Option<usize> {
    let slice = match col {
        SortColumn::Name => &ps.by_name,
        SortColumn::Size => &ps.by_size,
        SortColumn::DateModified => &ps.by_date_modified,
        SortColumn::Path => &ps.by_path,
        _ => return None,
    };
    (!slice.is_empty()).then_some(slice.len())
}

fn drive_letter_from_root(root: &str) -> Option<char> {
    let bytes = root.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some((bytes[0] as char).to_ascii_uppercase())
    } else {
        None
    }
}

pub fn detect_degraded_properties(
    properties: &PropertySideTables,
    expr: &QueryExpr,
    sort: &SortSpec,
) -> Option<QueryDegradedReason> {
    if properties.is_enabled() {
        return None;
    }
    let mut requested = expr.required_property_kinds();
    for key in &sort.keys {
        if let Some(kind) = key.column.property_kind() {
            requested.push(kind);
        }
    }
    if requested.is_empty() {
        return None;
    }
    requested.sort();
    requested.dedup();
    Some(QueryDegradedReason::PropertyIndexDisabled { requested })
}

fn definitely_empty_without_properties(expr: &QueryExpr) -> bool {
    match expr {
        QueryExpr::Term(t) => {
            t.length_filter.is_some()
                || t.author_filter.is_some()
                || t.tags_filter.is_some()
                || t.dimensions_filter.is_some()
                || t.framerate_filter.is_some()
        }
        QueryExpr::And(subs) => subs.iter().any(definitely_empty_without_properties),
        QueryExpr::Or(subs) => {
            !subs.is_empty() && subs.iter().all(definitely_empty_without_properties)
        }
        QueryExpr::Not(_) => false,
    }
}

fn eval_extra_term(
    term: &SearchTerm,
    rec: &FileRecord,
    idx: u32,
    source: &SourceIndex<'_>,
    hierarchy: Option<&HierarchyOracle>,
    dupes: Option<&DupeOracle>,
) -> bool {
    if let Some(ref df) = term.depth_filter {
        let Some(h) = hierarchy else {
            return false;
        };
        let depth = h.depths.get(idx as usize).copied().unwrap_or(0) as u64;
        if !df.matches(depth) {
            return false;
        }
    }

    if term.empty_folder_only
        || term.child_count_filter.is_some()
        || term.child_file_count_filter.is_some()
        || term.child_folder_count_filter.is_some()
    {
        if !rec.is_dir() {
            return false;
        }
        let Some(h) = hierarchy else {
            return false;
        };
        let (files, folders) = h.child_counts.get(idx as usize).copied().unwrap_or((0, 0));
        let total = (files + folders) as u64;
        if term.empty_folder_only && total != 0 {
            return false;
        }
        if let Some(ref f) = term.child_count_filter {
            if !f.matches(total) {
                return false;
            }
        }
        if let Some(ref f) = term.child_file_count_filter {
            if !f.matches(files as u64) {
                return false;
            }
        }
        if let Some(ref f) = term.child_folder_count_filter {
            if !f.matches(folders as u64) {
                return false;
            }
        }
    }

    if let Some(ref child_pat) = term.child_filter {
        if !rec.is_dir() {
            return false;
        }
        let Some(h) = hierarchy else {
            return false;
        };
        if !h
            .child_matches
            .get(child_pat)
            .and_then(|bits| bits.get(idx as usize))
            .copied()
            .unwrap_or(false)
        {
            return false;
        }
    }

    if let Some(mode) = term.dupe_mode {
        let Some(d) = dupes else {
            return false;
        };
        if !d.matches(mode, idx) {
            return false;
        }
    }

    if term.length_filter.is_some()
        || term.author_filter.is_some()
        || term.tags_filter.is_some()
        || term.dimensions_filter.is_some()
        || term.framerate_filter.is_some()
    {
        let Some(props) = source
            .properties
            .get(source.source_id, rec.frn, rec.modified_ts)
        else {
            return false;
        };
        if let Some(ref lf) = term.length_filter {
            if !props.length_ms.is_some_and(|ms| lf.matches(ms)) {
                return false;
            }
        }
        if let Some(ref pat) = term.author_filter {
            if !props.author.is_some_and(|a| matches_property_text(a, pat)) {
                return false;
            }
        }
        if let Some(ref pat) = term.tags_filter {
            if !props.tags.is_some_and(|t| matches_property_text(t, pat)) {
                return false;
            }
        }
        if let Some(ref dim_f) = term.dimensions_filter {
            if !props.dimensions.is_some_and(|(w, h)| dim_f.matches(w, h)) {
                return false;
            }
        }
        if let Some(ref fps_f) = term.framerate_filter {
            if !props
                .framerate_mfps
                .is_some_and(|mfps| fps_f.matches(mfps as u64))
            {
                return false;
            }
        }
    }

    true
}

#[inline]
fn matches_property_text(value: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    if pattern.contains('*') || pattern.contains('?') {
        glob_match(pattern, value, false)
    } else if pattern.is_ascii() && value.is_ascii() {
        value
            .as_bytes()
            .windows(pattern.len())
            .any(|w| w.eq_ignore_ascii_case(pattern.as_bytes()))
    } else {
        value.to_lowercase().contains(&pattern.to_lowercase())
    }
}

struct HierarchyOracle {
    depths: Vec<u16>,
    /// Per-record `(child_files, child_folders)`.
    child_counts: Vec<(u32, u32)>,
    child_matches: HashMap<String, Vec<bool>>,
}

impl HierarchyOracle {
    fn build(records: &[FileRecord], arena: &NamePool, root: &str, expr: &QueryExpr) -> Self {
        let mut need_depth = false;
        let mut need_counts = false;
        let mut child_patterns = Vec::new();
        collect_hierarchy_needs(expr, &mut need_depth, &mut need_counts, &mut child_patterns);
        child_patterns.sort();
        child_patterns.dedup();

        let len = records.len();
        let child_counts = if need_counts {
            let mut counts = vec![(0u32, 0u32); len];
            for rec in records {
                if rec.is_tombstone() || rec.parent_idx == FileRecord::ROOT_PARENT {
                    continue;
                }
                if let Some(entry) = counts.get_mut(rec.parent_idx as usize) {
                    if rec.is_dir() {
                        entry.1 = entry.1.saturating_add(1);
                    } else {
                        entry.0 = entry.0.saturating_add(1);
                    }
                }
            }
            counts
        } else {
            Vec::new()
        };

        let mut child_matches = HashMap::with_capacity(child_patterns.len());
        if !child_patterns.is_empty() {
            for pat in &child_patterns {
                child_matches.insert(pat.clone(), vec![false; len]);
            }
            for rec in records {
                if rec.is_tombstone() || rec.parent_idx == FileRecord::ROOT_PARENT {
                    continue;
                }
                let p_idx = rec.parent_idx as usize;
                if p_idx >= len {
                    continue;
                }
                let name = arena.get_name(rec.name_offset, rec.name_len);
                for pat in &child_patterns {
                    let hits = child_matches.get_mut(pat).expect("inserted pattern");
                    if !hits[p_idx] && matches_property_text(name, pat) {
                        hits[p_idx] = true;
                    }
                }
            }
        }

        let depths = if need_depth {
            compute_depths(records, root)
        } else {
            Vec::new()
        };

        Self {
            depths,
            child_counts,
            child_matches,
        }
    }
}

fn collect_hierarchy_needs(
    expr: &QueryExpr,
    need_depth: &mut bool,
    need_counts: &mut bool,
    child_patterns: &mut Vec<String>,
) {
    match expr {
        QueryExpr::Term(t) => {
            if t.depth_filter.is_some() {
                *need_depth = true;
            }
            if t.empty_folder_only
                || t.child_count_filter.is_some()
                || t.child_file_count_filter.is_some()
                || t.child_folder_count_filter.is_some()
            {
                *need_counts = true;
            }
            if let Some(ref pat) = t.child_filter {
                child_patterns.push(pat.clone());
            }
        }
        QueryExpr::And(v) | QueryExpr::Or(v) => {
            for s in v {
                collect_hierarchy_needs(s, need_depth, need_counts, child_patterns);
            }
        }
        QueryExpr::Not(inner) => {
            collect_hierarchy_needs(inner, need_depth, need_counts, child_patterns)
        }
    }
}

/// Bounds a corrupt or cyclic parent chain; real directory depth stays far below this.
const MAX_PARENT_CHAIN: usize = 256;

fn compute_depths(records: &[FileRecord], root: &str) -> Vec<u16> {
    let base_depth = root
        .trim_start_matches('\\')
        .split('\\')
        .filter(|seg| !seg.is_empty())
        .count() as u16;
    let mut depths = vec![0u16; records.len()];
    let mut stack = Vec::with_capacity(32);

    for idx in 0..records.len() {
        if depths[idx] != 0 || records[idx].is_tombstone() {
            continue;
        }
        stack.clear();
        let mut curr = idx as u32;
        let mut resolved = base_depth;

        while (curr as usize) < records.len() && stack.len() < MAX_PARENT_CHAIN {
            let cur_usize = curr as usize;
            if depths[cur_usize] != 0 {
                resolved = depths[cur_usize].saturating_add(1);
                break;
            }
            stack.push(cur_usize);
            let parent = records[cur_usize].parent_idx;
            if parent == FileRecord::ROOT_PARENT
                || parent == curr
                || stack.contains(&(parent as usize))
            {
                resolved = base_depth;
                break;
            }
            curr = parent;
        }

        for (step, &node) in stack.iter().rev().enumerate() {
            depths[node] = resolved.saturating_add(step as u16);
        }
    }
    depths
}

struct DupeOracle {
    modes: HashMap<DupeMode, Vec<bool>>,
}

impl DupeOracle {
    fn build(records: &[FileRecord], arena: &NamePool, expr: &QueryExpr) -> Self {
        let mut requested = HashSet::new();
        collect_dupe_modes(expr, &mut requested);

        let mut modes = HashMap::with_capacity(requested.len());
        for mode in requested {
            modes.insert(mode, build_dupe_bitmap(records, arena, mode));
        }
        Self { modes }
    }

    fn matches(&self, mode: DupeMode, idx: u32) -> bool {
        self.modes
            .get(&mode)
            .and_then(|bits| bits.get(idx as usize))
            .copied()
            .unwrap_or(false)
    }
}

fn collect_dupe_modes(expr: &QueryExpr, out: &mut HashSet<DupeMode>) {
    match expr {
        QueryExpr::Term(t) => {
            if let Some(m) = t.dupe_mode {
                out.insert(m);
            }
        }
        QueryExpr::And(v) | QueryExpr::Or(v) => {
            for s in v {
                collect_dupe_modes(s, out);
            }
        }
        QueryExpr::Not(inner) => collect_dupe_modes(inner, out),
    }
}

fn build_dupe_bitmap(records: &[FileRecord], arena: &NamePool, mode: DupeMode) -> Vec<bool> {
    let mut out = vec![false; records.len()];
    match mode {
        DupeMode::Name | DupeMode::UniqueName => {
            let mut counts: HashMap<&[u8], u32> = HashMap::new();
            for rec in records {
                if rec.is_tombstone() {
                    continue;
                }
                let key = arena.get_folded(rec.name_offset, rec.name_len);
                *counts.entry(key).or_insert(0) += 1;
            }
            for (idx, rec) in records.iter().enumerate() {
                if rec.is_tombstone() {
                    continue;
                }
                let key = arena.get_folded(rec.name_offset, rec.name_len);
                let count = counts.get(key).copied().unwrap_or(0);
                out[idx] = match mode {
                    DupeMode::Name => count >= 2,
                    DupeMode::UniqueName => count == 1,
                    _ => unreachable!(),
                };
            }
        }
        DupeMode::Size => {
            let mut counts: HashMap<u64, u32> = HashMap::new();
            for rec in records {
                if rec.is_tombstone() || rec.is_dir() {
                    continue;
                }
                *counts.entry(rec.size).or_insert(0) += 1;
            }
            for (idx, rec) in records.iter().enumerate() {
                if rec.is_tombstone() || rec.is_dir() {
                    continue;
                }
                out[idx] = counts.get(&rec.size).copied().unwrap_or(0) >= 2;
            }
        }
        DupeMode::NamePart => {
            let mut counts: HashMap<&[u8], u32> = HashMap::new();
            for rec in records {
                if rec.is_tombstone() {
                    continue;
                }
                let key = folded_stem(arena, rec);
                *counts.entry(key).or_insert(0) += 1;
            }
            for (idx, rec) in records.iter().enumerate() {
                if rec.is_tombstone() {
                    continue;
                }
                let key = folded_stem(arena, rec);
                out[idx] = counts.get(key).copied().unwrap_or(0) >= 2;
            }
        }
        DupeMode::NameAndSize => {
            let mut counts: HashMap<(&[u8], u64), u32> = HashMap::new();
            for rec in records {
                if rec.is_tombstone() || rec.is_dir() {
                    continue;
                }
                let key = (arena.get_folded(rec.name_offset, rec.name_len), rec.size);
                *counts.entry(key).or_insert(0) += 1;
            }
            for (idx, rec) in records.iter().enumerate() {
                if rec.is_tombstone() || rec.is_dir() {
                    continue;
                }
                let key = (arena.get_folded(rec.name_offset, rec.name_len), rec.size);
                out[idx] = counts.get(&key).copied().unwrap_or(0) >= 2;
            }
        }
    }
    out
}

#[inline]
fn folded_stem<'a>(arena: &'a NamePool, rec: &FileRecord) -> &'a [u8] {
    let folded = arena.get_folded(rec.name_offset, rec.name_len);
    if rec.has_ext() {
        let dot_pos = (rec.ext_offset as usize).saturating_sub(1);
        if dot_pos <= folded.len() {
            return &folded[..dot_pos];
        }
    }
    folded
}

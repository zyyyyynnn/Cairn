// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Metadata and content search: unified multi-source evaluation, exclusion filtering,
//! multi-column sorting and batched multi-snippet extraction.

use super::folder_source::FolderSource;
use super::path_exclusions::PathExclusions;
use super::volume_index::BitSet;
use super::{MultiVolumeEngine, VolumeIndex};
use cairn_content::{ContentSearcher, CONTENT_RESULT_TOP_K};
use cairn_core::arena::NamePool;
use cairn_core::engine::CancellationToken;
use cairn_core::path_cache::{root_path_for_drive, DirectoryPathCache};
use cairn_core::property_store::{PropertySideTables, SourceId};
use cairn_core::query::{
    detect_degraded_properties, execute_query, QueryDegradedReason, QueryPolicy, QueryRows,
    SourceIndex,
};
use cairn_core::sort::{
    compare_cross_source_records, sort_indices_with_spec, SortColumn, SortContext, SortDirection,
    SortSpec,
};
use cairn_core::syntax::{parse_query_with, QueryDefaults, QueryExpr};
use cairn_core::FileRecord;
use rayon::prelude::*;
use rayon::slice::ParallelSliceMut;
use std::collections::HashMap;
use std::sync::Arc;

/// One match the engine found: a record of one indexed source, observed at that source's table
/// generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchResultItem {
    pub source_id: SourceId,
    pub record_idx: u32,
    pub generation: u64,
}

/// The rows a query produced. A whole-volume textless query is answered by the volume's
/// presorted order itself; every other query carries its materialized match list.
pub enum MatchSet {
    Items(Vec<SearchResultItem>),
    AllFiles(AllFilesView),
}

/// One volume's live records in presorted order, plus the exclusions the query applied.
pub struct AllFilesView {
    pub(crate) source_id: SourceId,
    pub(crate) drive_letter: char,
    pub(crate) sort_by: SortColumn,
    pub(crate) sort_asc: bool,
    pub(crate) generation: u64,
    pub(crate) presorted_version: u64,
    pub(crate) exclusion_epoch: u64,
    pub(crate) exclusions: PathExclusions,
}

/// Complete output of one multi-source search.
pub struct SearchOutcome {
    pub rows: MatchSet,
    pub total: usize,
    pub highlight_terms: Vec<String>,
    pub snippets: HashMap<(u32, u64), Vec<String>>,
    pub total_bytes: u64,
    pub applied_sort: SortSpec,
    pub degraded: Option<QueryDegradedReason>,
}

impl SearchOutcome {
    fn empty(
        highlight_terms: Vec<String>,
        applied_sort: SortSpec,
        degraded: Option<QueryDegradedReason>,
    ) -> Self {
        Self {
            rows: MatchSet::Items(Vec::new()),
            total: 0,
            highlight_terms,
            snippets: HashMap::new(),
            total_bytes: 0,
            applied_sort,
            degraded,
        }
    }
}

impl AllFilesView {
    pub(crate) fn visit_visible(
        &self,
        volume: &VolumeIndex,
        offset: usize,
        limit: usize,
        visit: impl FnMut(u32),
    ) -> bool {
        self.visit_visible_until(volume, offset, limit, || false, visit)
    }

    pub(crate) fn visit_visible_until(
        &self,
        volume: &VolumeIndex,
        offset: usize,
        limit: usize,
        stop: impl Fn() -> bool,
        mut visit: impl FnMut(u32),
    ) -> bool {
        if volume.generation() != self.generation
            || volume.presorted_version() != self.presorted_version
            || volume.exclusion_epoch() != self.exclusion_epoch
        {
            return false;
        }
        let Some(column) = volume.presorted_order_snapshot(self.sort_by) else {
            return false;
        };
        let end = offset.saturating_add(limit);
        let mut visible = 0usize;
        let mut stopped = false;
        let mut take = |idx: u32, visible: &mut usize| -> bool {
            if *visible & 0xFFF == 0 && stop() {
                stopped = true;
                return true;
            }
            if *visible >= end {
                return true;
            }
            let record = &volume.records()[idx as usize];
            if record.is_tombstone() || self.exclusions.excludes(volume, idx) {
                return false;
            }
            if *visible >= offset {
                visit(idx);
            }
            *visible += 1;
            *visible >= end
        };
        if self.sort_asc {
            for &idx in column {
                if take(idx, &mut visible) {
                    break;
                }
            }
        } else {
            for &idx in column.iter().rev() {
                if take(idx, &mut visible) {
                    break;
                }
            }
        }
        !stopped
    }
}

#[cfg(test)]
impl MultiVolumeEngine {
    pub(crate) fn test_items(&self, rows: MatchSet) -> Vec<SearchResultItem> {
        match rows {
            MatchSet::Items(items) => items,
            MatchSet::AllFiles(view) => {
                let volumes = self.volumes.read().unwrap();
                let volume = volumes.get(&view.drive_letter).expect("viewed volume");
                let mut items = Vec::new();
                let visible = view.visit_visible(volume, 0, usize::MAX, |idx| {
                    items.push(SearchResultItem {
                        source_id: view.source_id,
                        record_idx: idx,
                        generation: view.generation,
                    });
                });
                if visible {
                    items
                } else {
                    Default::default()
                }
            }
        }
    }
}

enum IndexedSource<'a> {
    Volume(&'a VolumeIndex, Arc<str>),
    Folder(&'a FolderSource),
}

impl<'a> IndexedSource<'a> {
    fn source_id(&self) -> SourceId {
        match self {
            Self::Volume(vol, _) => {
                SourceId::for_drive(vol.drive_letter).expect("NTFS drive letter maps to a SourceId")
            }
            Self::Folder(folder) => folder.source_id,
        }
    }

    fn generation(&self) -> u64 {
        match self {
            Self::Volume(vol, _) => vol.generation(),
            Self::Folder(folder) => folder.generation(),
        }
    }

    fn root_prefix(&self) -> &Arc<str> {
        match self {
            Self::Volume(_, root) => root,
            Self::Folder(folder) => &folder.root_prefix,
        }
    }

    fn records(&self) -> &[FileRecord] {
        match self {
            Self::Volume(vol, _) => vol.records(),
            Self::Folder(folder) => folder.records(),
        }
    }

    fn arena(&self) -> &NamePool {
        match self {
            Self::Volume(vol, _) => vol.arena(),
            Self::Folder(folder) => folder.arena(),
        }
    }

    fn path_cache(&self) -> &DirectoryPathCache {
        match self {
            Self::Volume(vol, _) => vol.path_cache(),
            Self::Folder(folder) => folder.path_cache(),
        }
    }

    fn record_indices_for_frn(&self, frn: u64) -> Option<&[u32]> {
        match self {
            Self::Volume(vol, _) => vol.record_indices_for_frn(frn),
            Self::Folder(folder) => folder.record_indices_for_frn(frn),
        }
    }

    fn excludes(&self, exclusions: &PathExclusions, idx: u32) -> bool {
        match self {
            Self::Volume(vol, _) => exclusions.excludes(vol, idx),
            Self::Folder(folder) => exclusions.excludes_folder(folder, idx),
        }
    }

    fn calculate_matched_bytes(&self, matched: &[u32]) -> u64 {
        match self {
            Self::Volume(vol, _) => vol.calculate_matched_bytes(matched),
            Self::Folder(folder) => folder.calculate_matched_bytes(matched),
        }
    }

    fn as_source_index(&'a self, properties: &'a PropertySideTables) -> SourceIndex<'a> {
        match self {
            Self::Volume(vol, root) => vol.as_source_index(root, properties),
            Self::Folder(folder) => folder.as_source_index(properties),
        }
    }

    fn sort_context(&'a self, properties: Option<&'a PropertySideTables>) -> SortContext<'a> {
        SortContext {
            records: self.records(),
            arena: self.arena(),
            path_cache: self.path_cache(),
            root: self.root_prefix(),
            source_id: self.source_id(),
            properties,
        }
    }
}

enum ContentHits {
    Disjunction(HashMap<SourceId, Vec<u64>>),
    Conjunction(HashMap<SourceId, Vec<u64>>),
    General {
        term_index: HashMap<String, usize>,
        by_source: HashMap<SourceId, Vec<Vec<u64>>>,
    },
}

fn folder_is_on_drive(folder: &FolderSource, drive: char) -> bool {
    let bytes = folder.display_root().as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).eq_ignore_ascii_case(&drive)
}

fn supports_presorted_column(col: SortColumn) -> bool {
    matches!(
        col,
        SortColumn::Name | SortColumn::Size | SortColumn::DateModified | SortColumn::Path
    )
}

fn group_frn_pairs_by_source(pairs: Vec<(u64, u64)>) -> HashMap<SourceId, Vec<u64>> {
    let mut grouped: HashMap<SourceId, Vec<u64>> = HashMap::new();
    for (source_raw, frn) in pairs {
        if let Ok(sid) = u32::try_from(source_raw) {
            grouped.entry(SourceId(sid)).or_default().push(frn);
        }
    }
    grouped
}

fn collect_content_hits(
    searcher: &ContentSearcher,
    query_expr: &QueryExpr,
    content_queries: &[String],
    cancel: &CancellationToken,
) -> Option<(ContentHits, Vec<String>)> {
    if cancel.is_cancelled() {
        return None;
    }
    if let Some(terms) = query_expr.pure_content_disjunction() {
        let pairs = match searcher.search_all_frns_union(&terms) {
            Ok(hits) => hits,
            Err(err) => {
                tracing::warn!("content union search for {terms:?} failed: {err}");
                return None;
            }
        };
        if cancel.is_cancelled() || pairs.is_empty() {
            return None;
        }
        return Some((
            ContentHits::Disjunction(group_frn_pairs_by_source(pairs)),
            terms,
        ));
    }

    if is_positive_conjunction(query_expr) {
        let pairs = match searcher.search_terms_all_frns(content_queries) {
            Ok(hits) => hits,
            Err(err) => {
                tracing::warn!("content conjunction search for {content_queries:?} failed: {err}");
                return None;
            }
        };
        if cancel.is_cancelled() || pairs.is_empty() {
            return None;
        }
        return Some((
            ContentHits::Conjunction(group_frn_pairs_by_source(pairs)),
            content_queries.to_vec(),
        ));
    }

    let term_count = content_queries.len();
    let term_index: HashMap<String, usize> = content_queries
        .iter()
        .enumerate()
        .map(|(slot, term)| (term.clone(), slot))
        .collect();
    let mut by_source: HashMap<SourceId, Vec<Vec<u64>>> = HashMap::new();
    for (slot, term) in content_queries.iter().enumerate() {
        if cancel.is_cancelled() {
            return None;
        }
        let pairs = match searcher.search_all_frns(term) {
            Ok(pairs) => pairs,
            Err(err) => {
                tracing::warn!("content search for {term:?} failed: {err}");
                return None;
            }
        };
        for (source_raw, frn) in pairs {
            if let Ok(sid) = u32::try_from(source_raw) {
                by_source
                    .entry(SourceId(sid))
                    .or_insert_with(|| vec![Vec::new(); term_count])[slot]
                    .push(frn);
            }
        }
    }
    if cancel.is_cancelled() {
        return None;
    }
    Some((
        ContentHits::General {
            term_index,
            by_source,
        },
        query_expr.extract_positive_content_queries(),
    ))
}

impl MultiVolumeEngine {
    /// Returns the sole active NTFS volume eligible for the zero-materialization presorted view.
    fn all_files_view_drive(
        &self,
        drive_scope: Option<char>,
        applied_sort: &SortSpec,
    ) -> Option<char> {
        if applied_sort.keys.len() != 1 || !supports_presorted_column(applied_sort.primary().column)
        {
            return None;
        }
        let excluded_drives = self.config.read().unwrap().excluded_drives.clone();
        let volumes = self.volumes.read().unwrap();
        let folders = self.folder_sources.read().unwrap();
        let active_folders: Vec<&FolderSource> =
            folders.values().filter(|f| f.is_active()).collect();
        match drive_scope {
            Some(drive) => volumes
                .values()
                .find(|vol| {
                    vol.drive_letter == drive
                        && vol.is_active(&excluded_drives)
                        && !active_folders
                            .iter()
                            .any(|folder| folder_is_on_drive(folder, drive))
                })
                .map(|vol| vol.drive_letter),
            None => {
                if !active_folders.is_empty() {
                    return None;
                }
                let mut active = volumes
                    .values()
                    .filter(|vol| vol.is_active(&excluded_drives));
                let first = active.next()?;
                active.next().is_none().then_some(first.drive_letter)
            }
        }
    }

    fn ensure_presorted_column_for(&self, drive: char, col: SortColumn) {
        let needs_work = self
            .volumes
            .read()
            .unwrap()
            .get(&drive)
            .is_some_and(|volume| volume.needs_presorted_column(col));
        if !needs_work {
            return;
        }
        let mut volumes = self.volumes.write().unwrap();
        if let Some(volume) = volumes.get_mut(&drive) {
            volume.ensure_presorted_column(col);
        }
    }

    fn all_files_outcome(
        volume: &VolumeIndex,
        applied_sort: SortSpec,
        exclusions: PathExclusions,
        cancel: &CancellationToken,
        highlight_terms: Vec<String>,
        degraded: Option<QueryDegradedReason>,
    ) -> SearchOutcome {
        let primary = applied_sort.primary();
        let source_id =
            SourceId::for_drive(volume.drive_letter).expect("NTFS drive letter maps to a SourceId");
        let view = AllFilesView {
            source_id,
            drive_letter: volume.drive_letter,
            sort_by: primary.column,
            sort_asc: primary.direction == SortDirection::Ascending,
            generation: volume.generation(),
            presorted_version: volume.presorted_version(),
            exclusion_epoch: volume.exclusion_epoch(),
            exclusions,
        };
        let (total, view_bytes) = if view.exclusions.is_empty() {
            (volume.live_count() as usize, volume.live_file_bytes())
        } else if view.exclusions.uses_segment_table() {
            let mut total = 0usize;
            let mut root_size = None;
            let mut top_level_bytes = 0u64;
            if !view.visit_visible_until(
                volume,
                0,
                usize::MAX,
                || cancel.is_cancelled(),
                |idx| {
                    total += 1;
                    let record = &volume.records()[idx as usize];
                    if record.parent_idx == idx {
                        root_size = Some(record.size);
                    } else if record.parent_idx == FileRecord::ROOT_PARENT {
                        top_level_bytes = top_level_bytes.saturating_add(record.size);
                    }
                },
            ) {
                return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
            }
            (total, root_size.unwrap_or(top_level_bytes))
        } else {
            let mut visible = Vec::with_capacity(volume.live_count() as usize);
            if !view.visit_visible_until(
                volume,
                0,
                usize::MAX,
                || cancel.is_cancelled(),
                |idx| visible.push(idx),
            ) {
                return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
            }
            (visible.len(), volume.calculate_matched_bytes(&visible))
        };
        SearchOutcome {
            rows: MatchSet::AllFiles(view),
            total,
            highlight_terms,
            snippets: HashMap::new(),
            total_bytes: view_bytes,
            applied_sort,
            degraded,
        }
    }

    /// Evaluates metadata and optional content search uniformly across active NTFS volumes and
    /// folder sources.
    pub fn execute_search_multi(
        &self,
        raw_query: &str,
        defaults: &QueryDefaults,
        sort_spec: &SortSpec,
        cancel: &CancellationToken,
    ) -> SearchOutcome {
        let query_expr = parse_query_with(raw_query, defaults);
        let highlight_terms = query_expr.collect_highlight_terms();
        let mut applied_sort = query_expr
            .extract_sort_spec()
            .unwrap_or_else(|| sort_spec.clone());
        if applied_sort.keys.is_empty() {
            applied_sort = SortSpec::default();
        }
        let content_queries = query_expr.extract_content_queries();

        let drive_scope = query_expr.drive_scope();
        let all_files_shape = query_expr.is_empty() || drive_scope.is_some();
        let view_drive = if all_files_shape && content_queries.is_empty() {
            self.all_files_view_drive(drive_scope, &applied_sort)
        } else {
            None
        };
        if let Some(drive) = view_drive {
            self.ensure_presorted_column_for(drive, applied_sort.primary().column);
        }

        let (excluded_drives, excluded_paths) = {
            let cfg = self.config.read().unwrap();
            (cfg.excluded_drives.clone(), cfg.excluded_paths.clone())
        };

        // Run Tantivy hit collection outside index locks so concurrent writes are not stalled.
        let mut content_plan: Option<(Arc<ContentSearcher>, ContentHits, Vec<String>)> = None;
        if !content_queries.is_empty() {
            let searcher = if self.content_index_status().state.is_ready() {
                self.get_or_create_content_searcher()
            } else {
                None
            };
            let Some(searcher) = searcher else {
                let props = self.property_tables.read().unwrap();
                let degraded = detect_degraded_properties(&props, &query_expr, &applied_sort);
                return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
            };
            let Some((hits, snippet_terms)) =
                collect_content_hits(&searcher, &query_expr, &content_queries, cancel)
            else {
                let props = self.property_tables.read().unwrap();
                let degraded = detect_degraded_properties(&props, &query_expr, &applied_sort);
                return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
            };
            content_plan = Some((searcher, hits, snippet_terms));
        }

        type SnippetRequest = (Arc<ContentSearcher>, Vec<(u64, u64)>, Vec<String>);
        let mut snippet_request: Option<SnippetRequest> = None;
        let mut all_results = Vec::new();
        let mut total_bytes = 0u64;
        let degraded;

        {
            let volumes = self.volumes.read().unwrap();
            let folders = self.folder_sources.read().unwrap();
            let properties = self.property_tables.read().unwrap();
            degraded = detect_degraded_properties(&properties, &query_expr, &applied_sort);
            let exclusions = PathExclusions::new(&volumes, &excluded_paths);

            let active_vols: Vec<&VolumeIndex> = volumes
                .values()
                .filter(|vol| vol.is_active(&excluded_drives))
                .collect();
            let active_folders: Vec<&FolderSource> =
                folders.values().filter(|f| f.is_active()).collect();

            if let Some(drive) = view_drive {
                let folder_conflict = active_folders.iter().any(|f| folder_is_on_drive(f, drive));
                let view_allowed = !folder_conflict
                    && match drive_scope {
                        Some(_) => active_vols.iter().any(|vol| vol.drive_letter == drive),
                        None => {
                            active_folders.is_empty()
                                && active_vols.len() == 1
                                && active_vols[0].drive_letter == drive
                        }
                    };
                if view_allowed && degraded.is_none() {
                    if let Some(volume) = volumes.get(&drive) {
                        if volume
                            .presorted_order(applied_sort.primary().column)
                            .is_some()
                        {
                            return Self::all_files_outcome(
                                volume,
                                applied_sort,
                                exclusions,
                                cancel,
                                highlight_terms,
                                degraded,
                            );
                        }
                    }
                }
            }

            let mut sources: Vec<IndexedSource<'_>> =
                Vec::with_capacity(active_vols.len() + active_folders.len());
            for vol in active_vols {
                let root = root_path_for_drive(vol.drive_letter);
                sources.push(IndexedSource::Volume(vol, root));
            }
            for folder in active_folders {
                sources.push(IndexedSource::Folder(folder));
            }

            let single_source = sources.len() == 1;
            let content_only_plan = content_plan.as_ref().map(|(_, hits, _)| hits);

            for src in &sources {
                if cancel.is_cancelled() {
                    return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
                }
                let sid = src.source_id();
                let generation = src.generation();

                let mut matched = match content_only_plan {
                    Some(ContentHits::Disjunction(by_source))
                    | Some(ContentHits::Conjunction(by_source)) => {
                        let Some(frns) = by_source.get(&sid) else {
                            continue;
                        };
                        let bits = hit_bitset_for_source(src, frns, cancel);
                        if cancel.is_cancelled() {
                            return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
                        }
                        let source_idx = src.as_source_index(&properties);
                        let resolver = |_term: &str, idx: u32| bits.contains(idx as usize);
                        let policy = QueryPolicy {
                            sort: applied_sort.clone(),
                            content_hit: Some(&resolver),
                        };
                        match execute_query(&source_idx, &query_expr, &policy, cancel).rows {
                            QueryRows::Materialized(indices) => indices,
                            QueryRows::PresortedAll { .. } => Vec::new(),
                        }
                    }
                    Some(ContentHits::General {
                        term_index,
                        by_source,
                    }) => {
                        let sets: Vec<BitSet> = by_source
                            .get(&sid)
                            .map(|frns_by_term| {
                                frns_by_term
                                    .iter()
                                    .map(|frns| hit_bitset_for_source(src, frns, cancel))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if cancel.is_cancelled() {
                            return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
                        }
                        let source_idx = src.as_source_index(&properties);
                        let resolver = |term: &str, idx: u32| {
                            term_index.get(term).is_some_and(|&slot| {
                                sets.get(slot)
                                    .is_some_and(|bits| bits.contains(idx as usize))
                            })
                        };
                        let policy = QueryPolicy {
                            sort: applied_sort.clone(),
                            content_hit: Some(&resolver),
                        };
                        match execute_query(&source_idx, &query_expr, &policy, cancel).rows {
                            QueryRows::Materialized(indices) => indices,
                            QueryRows::PresortedAll { .. } => Vec::new(),
                        }
                    }
                    None => {
                        let source_idx = src.as_source_index(&properties);
                        let policy = QueryPolicy {
                            sort: applied_sort.clone(),
                            content_hit: None,
                        };
                        match execute_query(&source_idx, &query_expr, &policy, cancel).rows {
                            QueryRows::Materialized(indices) => indices,
                            QueryRows::PresortedAll { .. } => {
                                // Fallback when PresortedAll applies on a non-view path (e.g.
                                // multiple sources or explicit non-view configuration).
                                let mut indices: Vec<u32> = src
                                    .records()
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(idx, rec)| {
                                        (!rec.is_tombstone()).then_some(idx as u32)
                                    })
                                    .collect();
                                if single_source && indices.len() > 1 {
                                    let ctx = src.sort_context(Some(&properties));
                                    sort_indices_with_spec(
                                        &mut indices,
                                        &ctx,
                                        &applied_sort,
                                        cancel,
                                    );
                                }
                                indices
                            }
                        }
                    }
                };

                let Some(src_bytes) =
                    filter_unique_source_matches(src, &mut matched, &exclusions, cancel)
                else {
                    return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
                };
                total_bytes = total_bytes.saturating_add(src_bytes);
                all_results.reserve(matched.len());
                for record_idx in matched {
                    all_results.push(SearchResultItem {
                        source_id: sid,
                        record_idx,
                        generation,
                    });
                }
            }

            if !single_source && all_results.len() > 1 && !cancel.is_cancelled() {
                let ctx_by_id: HashMap<SourceId, SortContext<'_>> = sources
                    .iter()
                    .map(|s| (s.source_id(), s.sort_context(Some(&properties))))
                    .collect();
                all_results.par_sort_unstable_by(|a, b| {
                    let ctx_a = &ctx_by_id[&a.source_id];
                    let ctx_b = &ctx_by_id[&b.source_id];
                    compare_cross_source_records(
                        ctx_a,
                        a.record_idx,
                        ctx_b,
                        b.record_idx,
                        &applied_sort,
                    )
                    .then_with(|| a.source_id.0.cmp(&b.source_id.0))
                    .then_with(|| a.record_idx.cmp(&b.record_idx))
                });
            }

            if cancel.is_cancelled() {
                return SearchOutcome::empty(highlight_terms, applied_sort, degraded);
            }

            if let Some((searcher, _, snippet_terms)) = content_plan {
                if !all_results.is_empty() && !snippet_terms.is_empty() {
                    let src_by_id: HashMap<SourceId, &IndexedSource<'_>> =
                        sources.iter().map(|s| (s.source_id(), s)).collect();
                    let limit = all_results.len().min(CONTENT_RESULT_TOP_K);
                    let mut keys = Vec::with_capacity(limit);
                    for item in &all_results[..limit] {
                        if let Some(src) = src_by_id.get(&item.source_id) {
                            if let Some(rec) = src.records().get(item.record_idx as usize) {
                                keys.push((item.source_id.0 as u64, rec.frn));
                            }
                        }
                    }
                    if !keys.is_empty() {
                        snippet_request = Some((searcher, keys, snippet_terms));
                    }
                }
            }
        }

        let mut snippets = HashMap::new();
        if let Some((searcher, keys, snippet_terms)) = snippet_request {
            if !cancel.is_cancelled() {
                match searcher.snippets_for_keys(&keys, &snippet_terms) {
                    Ok(by_key) => {
                        for ((vol_id, frn), list) in by_key {
                            if let Ok(sid) = u32::try_from(vol_id) {
                                snippets.insert((sid, frn), list);
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!("snippet extraction failed: {err}");
                    }
                }
            }
        }

        let total = all_results.len();
        SearchOutcome {
            rows: MatchSet::Items(all_results),
            total,
            highlight_terms,
            snippets,
            total_bytes,
            applied_sort,
            degraded,
        }
    }
}

fn filter_unique_source_matches(
    source: &IndexedSource<'_>,
    matches: &mut Vec<u32>,
    exclusions: &PathExclusions,
    cancel: &CancellationToken,
) -> Option<u64> {
    let mut write = 0usize;
    for read in 0..matches.len() {
        if (read & 0xFFF) == 0 && cancel.is_cancelled() {
            return None;
        }
        let idx = matches[read];
        if source.excludes(exclusions, idx) {
            continue;
        }
        matches[write] = idx;
        write += 1;
    }
    matches.truncate(write);
    Some(source.calculate_matched_bytes(matches))
}

fn is_positive_conjunction(expr: &QueryExpr) -> bool {
    match expr {
        QueryExpr::Term(_) => true,
        QueryExpr::And(subs) => !subs.is_empty() && subs.iter().all(is_positive_conjunction),
        _ => false,
    }
}

const HIT_RESOLVE_CHUNK: usize = 16_384;

fn hit_bitset_for_source(
    source: &IndexedSource<'_>,
    frns: &[u64],
    cancel: &CancellationToken,
) -> BitSet {
    if frns.is_empty() {
        return BitSet::with_capacity(0);
    }
    let total_records = source.records().len();
    frns.par_chunks(HIT_RESOLVE_CHUNK)
        .map(|chunk| {
            if cancel.is_cancelled() {
                return BitSet::with_capacity(total_records);
            }
            let mut bits = BitSet::with_capacity(total_records);
            for &frn in chunk {
                if let Some(indices) = source.record_indices_for_frn(frn) {
                    for &idx in indices {
                        bits.insert(idx as usize);
                    }
                }
            }
            bits
        })
        .reduce(
            || BitSet::with_capacity(total_records),
            |mut left, right| {
                left.merge(&right);
                left
            },
        )
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Metadata and content search: query evaluation, exclusion filtering, result
//! ordering and snippets.

use super::path_exclusions::PathExclusions;
use super::volume_index::BitSet;
use super::{MultiVolumeEngine, VolumeIndex};
use cairn_content::CONTENT_RESULT_TOP_K;
use cairn_core::engine::CancellationToken;
use cairn_core::sort::{SortColumn, SortDirection};
use cairn_core::syntax::{parse_query_with, QueryDefaults, QueryExpr};
use cairn_core::FileRecord;
use rayon::prelude::*;
use rayon::slice::ParallelSliceMut;
use std::collections::{HashMap, HashSet};

/// One match the engine found: a record of one volume, observed at that volume's table
/// generation.
///
/// The type belongs to the engine because the engine produces it; the query session only caches
/// it. Keeping it here also keeps the dependency one-way: the session pages through a volume,
/// so it knows the volume, and the volume never has to know the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchResultItem {
    pub drive_letter: char,
    pub record_idx: u32,
    /// [`VolumeIndex`] generation observed when the match was produced. A later rebuild,
    /// compaction or reload replaces the record table, so these indices no longer describe the
    /// same rows.
    pub generation: u64,
}

/// The rows a query produced. A whole-volume textless query is answered by the volume's
/// presorted order itself, so its session pages that order instead of retaining one item per
/// record; every other query carries its materialized match list.
pub enum MatchSet {
    Items(Vec<SearchResultItem>),
    AllFiles(AllFilesView),
}

/// One volume's live records in presorted order, plus the exclusions the query applied.
/// Sessions read pages from this descriptor and refuse them once the order or the table
/// changes underneath.
pub struct AllFilesView {
    pub(crate) drive_letter: char,
    pub(crate) sort_by: SortColumn,
    pub(crate) sort_asc: bool,
    pub(crate) generation: u64,
    pub(crate) presorted_version: u64,
    pub(crate) exclusion_epoch: u64,
    pub(crate) exclusions: PathExclusions,
}

/// Everything one query produced: the rows (materialized or a live view), the exact counts,
/// and the display extras. One model for the pipe server, the session cache and the benches.
pub struct SearchOutcome {
    pub rows: MatchSet,
    pub total: usize,
    pub highlight_terms: Vec<String>,
    pub snippets: HashMap<(char, u64), String>,
    pub total_bytes: u64,
}

impl AllFilesView {
    /// Visits the view's visible records in order, starting at `offset` visible rows and
    /// stopping after `limit` visits. Returns `false` when the volume's table or order no
    /// longer matches this view. Totals, paging and tests share this one walk.
    pub(crate) fn visit_visible(
        &self,
        volume: &VolumeIndex,
        offset: usize,
        limit: usize,
        visit: impl FnMut(u32),
    ) -> bool {
        self.visit_visible_until(volume, offset, limit, || false, visit)
    }

    /// The same walk with a stop probe, so a superseded totals scan releases the order.
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
    /// Test seam: the rows a consumer that needs the full list would see. A live all-files
    /// view is materialized from the same order and exclusions its pages use.
    pub(crate) fn test_items(&self, rows: MatchSet) -> Vec<SearchResultItem> {
        match rows {
            MatchSet::Items(items) => items,
            MatchSet::AllFiles(view) => {
                let volumes = self.volumes.read().unwrap();
                let volume = volumes.get(&view.drive_letter).expect("viewed volume");
                let mut items = Vec::new();
                let visible = view.visit_visible(volume, 0, usize::MAX, |idx| {
                    items.push(SearchResultItem {
                        drive_letter: view.drive_letter,
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

impl SearchOutcome {
    fn empty(highlight_terms: Vec<String>, snippets: HashMap<(char, u64), String>) -> Self {
        Self {
            rows: MatchSet::Items(Vec::new()),
            total: 0,
            highlight_terms,
            snippets,
            total_bytes: 0,
        }
    }
}

/// Whether a volume can answer a query: not excluded by preference, and either fully indexed
/// or holding records from a previous session.
fn volume_is_active(volume: &VolumeIndex, excluded_drives: &[char]) -> bool {
    !excluded_drives
        .iter()
        .any(|drive| drive.eq_ignore_ascii_case(&volume.drive_letter))
        && (volume.status.state().is_synced() || !volume.records().is_empty())
}

impl MultiVolumeEngine {
    /// The one active volume a textless query would view. A drive-scoped query names it; an
    /// unscoped one needs exactly one active volume. The presorted build and the view read
    /// this one verdict.
    fn view_drive(&self, drive_scope: Option<char>) -> Option<char> {
        let excluded_drives = {
            let cfg = self.config.read().unwrap();
            cfg.excluded_drives.clone()
        };
        let volumes = self.volumes.read().unwrap();
        match drive_scope {
            Some(drive) => volumes
                .values()
                .find(|volume| {
                    volume_is_active(volume, &excluded_drives)
                        && volume.drive_letter.eq_ignore_ascii_case(&drive)
                })
                .map(|volume| volume.drive_letter),
            None => {
                let mut active = volumes
                    .values()
                    .filter(|volume| volume_is_active(volume, &excluded_drives));
                let volume = active.next()?;
                if active.next().is_some() {
                    return None;
                }
                Some(volume.drive_letter)
            }
        }
    }

    /// Builds or merges the scoped volume's order for a view query.
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

    /// The outcome of a textless whole-volume query: the view descriptor, its exact totals
    /// and the display extras.
    fn all_files_outcome(
        volume: &VolumeIndex,
        sort_by: SortColumn,
        sort_asc: bool,
        exclusions: PathExclusions,
        cancel: &CancellationToken,
        highlight_terms: Vec<String>,
        snippets: HashMap<(char, u64), String>,
    ) -> SearchOutcome {
        let view = AllFilesView {
            drive_letter: volume.drive_letter,
            sort_by,
            sort_asc,
            generation: volume.generation(),
            presorted_version: volume.presorted_version(),
            exclusion_epoch: volume.exclusion_epoch(),
            exclusions,
        };
        // Without path rules the volume's own counters are the exact totals, so the view
        // costs no scan. With rules the visible set is the matched set, so its space total
        // follows the same highest-hit rule as a filtered query.
        let (total, view_bytes) = if view.exclusions.is_empty() {
            (volume.live_count() as usize, volume.live_file_bytes())
        } else if view.exclusions.uses_segment_table() {
            // Segment rules exclude whole subtrees, so a visible record always has visible
            // ancestors. The visible root, or the visible records whose parent is not in the
            // table, are then the only highest hit points; their recursive sizes cover every
            // visible row without materializing the set.
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
                return SearchOutcome::empty(highlight_terms, snippets);
            }
            (total, root_size.unwrap_or(top_level_bytes))
        } else {
            // Full-path rules can exclude a directory while leaving its children visible, so
            // the per-row highest-hit calculation decides.
            let mut visible = Vec::with_capacity(volume.live_count() as usize);
            if !view.visit_visible_until(
                volume,
                0,
                usize::MAX,
                || cancel.is_cancelled(),
                |idx| visible.push(idx),
            ) {
                return SearchOutcome::empty(highlight_terms, snippets);
            }
            (visible.len(), volume.calculate_matched_bytes(&visible))
        };
        SearchOutcome {
            rows: MatchSet::AllFiles(view),
            total,
            highlight_terms,
            snippets,
            total_bytes: view_bytes,
        }
    }

    /// Evaluates search across all volumes, filtering excluded paths, sorting by column,
    /// returning highlight terms and content snippets. `defaults` carries the query-wide match
    /// toggles and attribute filters the client sent with the text.
    pub fn execute_search_multi(
        &self,
        raw_query: &str,
        defaults: &QueryDefaults,
        sort_by: SortColumn,
        sort_asc: bool,
        cancel: &CancellationToken,
    ) -> SearchOutcome {
        let query_expr = parse_query_with(raw_query, defaults);
        let highlight_terms = query_expr.collect_highlight_terms();
        let content_queries = query_expr.extract_content_queries();
        let mft_query_expr = query_expr
            .strip_content_queries()
            .unwrap_or_else(|| QueryExpr::Term(cairn_core::syntax::SearchTerm::default()));

        // A textless query that names one volume is that volume's all-files view; the sidebar
        // default sends `C: `. Its order is built only when the view will consume it. A
        // filtered query scans and sorts just its matches, so rebuilding a whole column first
        // would be pure latency; the multi-volume path merges per-volume matches itself.
        let drive_scope = query_expr.drive_scope();
        let all_files_shape = query_expr.is_empty() || drive_scope.is_some();
        let view_drive = if all_files_shape && content_queries.is_empty() {
            self.view_drive(drive_scope)
        } else {
            None
        };
        if let Some(drive) = view_drive {
            self.ensure_presorted_column_for(drive, sort_by);
        }

        let (excluded_drives, excluded_paths) = {
            let cfg = self.config.read().unwrap();
            (cfg.excluded_drives.clone(), cfg.excluded_paths.clone())
        };

        let mut all_results = Vec::new();
        let mut snippets = HashMap::new();
        // Space of the matched set at its highest hit point: a matched directory brings its
        // recursive size, and anything already covered by a matched ancestor adds nothing.
        let mut total_bytes = 0u64;

        let lock;

        if !content_queries.is_empty() {
            // Tantivy searches run before the volume lock is taken; evaluating content terms
            // against records then holds the read lock, as the metadata query paths do.
            let searcher = if self.content_index_status().state.is_ready() {
                self.get_or_create_content_searcher()
            } else {
                None
            };
            let Some(searcher) = searcher else {
                return SearchOutcome::empty(highlight_terms, snippets);
            };

            // A disjunction of content-only terms is one merged Tantivy search: the union hit
            // set answers every branch at once.
            if let Some(terms) = query_expr.pure_content_disjunction() {
                if cancel.is_cancelled() {
                    return SearchOutcome::empty(highlight_terms, snippets);
                }
                let union_hits = match searcher.search_all_frns_union(&terms) {
                    Ok(hits) => hits,
                    Err(err) => {
                        tracing::warn!("content union search for {terms:?} failed: {err}");
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                };
                if cancel.is_cancelled() || union_hits.is_empty() {
                    return SearchOutcome::empty(highlight_terms, snippets);
                }

                // The collector emits one pair per matching document, so the hit set's length is
                // the union's document count.
                let union_count = union_hits.len();
                let mut frns_by_drive: HashMap<char, Vec<u64>> = HashMap::new();
                for (i, (volume_id, frn)) in union_hits.into_iter().enumerate() {
                    if (i & 0x3FFF) == 0 && cancel.is_cancelled() {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                    let Some(drive_letter) = u16::try_from(volume_id)
                        .ok()
                        .and_then(FileRecord::drive_from_volume_id)
                    else {
                        continue;
                    };
                    frns_by_drive.entry(drive_letter).or_default().push(frn);
                }

                // Snippets are display-only: a scored union search supplies them while it is
                // complete; a very common disjunction keeps just its hit set.
                let mut snippets_by_key: HashMap<(char, u64), String> = HashMap::new();
                if union_count < CONTENT_RESULT_TOP_K {
                    if cancel.is_cancelled() {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                    let hits = match searcher.search_union(&terms, CONTENT_RESULT_TOP_K) {
                        Ok(hits) => hits,
                        Err(err) => {
                            tracing::warn!("content union search for {terms:?} failed: {err}");
                            return SearchOutcome::empty(highlight_terms, snippets);
                        }
                    };
                    for hit in &hits {
                        let Some(drive_letter) = u16::try_from(hit.volume_id)
                            .ok()
                            .and_then(FileRecord::drive_from_volume_id)
                        else {
                            continue;
                        };
                        snippets_by_key.insert((drive_letter, hit.frn), hit.snippet.clone());
                    }
                }

                lock = self.volumes.read().unwrap();
                let exclusions = PathExclusions::new(&lock, &excluded_paths);
                for vol in lock.values() {
                    if cancel.is_cancelled() {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                    if excluded_drives
                        .iter()
                        .any(|&ex| ex.eq_ignore_ascii_case(&vol.drive_letter))
                    {
                        continue;
                    }
                    if !vol.status.state().is_synced() && vol.records().is_empty() {
                        continue;
                    }
                    // A pure content disjunction can only match where some term hit, so a
                    // volume without hits has nothing to scan.
                    let Some(frns) = frns_by_drive.remove(&vol.drive_letter) else {
                        continue;
                    };
                    // One union bitmap per volume; a hit FRN marks every hardlink row it maps
                    // to. The FRN list is consumed as the bitmap is built.
                    let union_bits = hit_bitset(vol, &frns, cancel);
                    let mut matches = Vec::new();
                    for (idx, rec) in vol.records().iter().enumerate() {
                        if (idx & 0x3FFF) == 0 && cancel.is_cancelled() {
                            return SearchOutcome::empty(highlight_terms, snippets);
                        }
                        if !rec.is_tombstone() && union_bits.contains(idx) {
                            matches.push(idx as u32);
                        }
                    }
                    let Some(vol_matches) =
                        collect_volume_matches(vol, matches, &exclusions, cancel, &mut all_results)
                    else {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    };
                    for &idx in &vol_matches {
                        let frn = vol.records()[idx as usize].frn;
                        if let Some(snippet) = snippets_by_key.get(&(vol.drive_letter, frn)) {
                            snippets.insert((vol.drive_letter, frn), snippet.clone());
                        }
                    }
                    total_bytes =
                        total_bytes.saturating_add(vol.calculate_matched_bytes(&vol_matches));
                }
            } else if is_positive_conjunction(&query_expr) {
                // A conjunction of positive terms — content and metadata alike — is one Tantivy
                // query with one clause per content term plus the compiled metadata filter. Its
                // scored top-k bound is historical: truncation shortens the reply before the
                // metadata filter runs, so a conjunction over a very common word can show fewer
                // rows than its true match set.
                let content_hits =
                    match searcher.search_terms(&content_queries, CONTENT_RESULT_TOP_K) {
                        Ok(hits) => hits,
                        Err(err) => {
                            tracing::warn!("content search for {content_queries:?} failed: {err}");
                            return SearchOutcome::empty(highlight_terms, snippets);
                        }
                    };

                if cancel.is_cancelled() || content_hits.is_empty() {
                    return SearchOutcome::empty(highlight_terms, snippets);
                }

                // Precompile MFT filter outside the lock to keep lock duration minimal
                let compiled_mft = if !mft_query_expr.is_empty() {
                    Some(cairn_core::engine::CompiledQuery::compile(&mft_query_expr))
                } else {
                    None
                };

                // Acquire read lock with minimal critical section to convert and merge results
                lock = self.volumes.read().unwrap();
                let exclusions = PathExclusions::new(&lock, &excluded_paths);
                let mut tantivy_vol_matches: HashMap<char, Vec<u32>> = HashMap::new();

                for hit in &content_hits {
                    if cancel.is_cancelled() {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                    let Some(drive_letter) = u16::try_from(hit.volume_id)
                        .ok()
                        .and_then(FileRecord::drive_from_volume_id)
                    else {
                        continue;
                    };
                    if excluded_drives
                        .iter()
                        .any(|&ex| ex.eq_ignore_ascii_case(&drive_letter))
                    {
                        continue;
                    }
                    let Some(vol) = lock.get(&drive_letter) else {
                        continue;
                    };
                    if !vol.status.state().is_synced() && vol.records().is_empty() {
                        continue;
                    }
                    let Some(indices) = vol.record_indices_for_frn(hit.frn) else {
                        continue;
                    };

                    for &idx in indices {
                        let rec = &vol.records()[idx as usize];
                        if rec.is_tombstone() {
                            continue;
                        }
                        if let Some(ref compiled) = compiled_mft {
                            if !compiled.evaluate_record(
                                rec,
                                idx,
                                vol.records(),
                                vol.arena(),
                                vol.path_cache(),
                                vol.drive_letter,
                            ) {
                                continue;
                            }
                        }
                        if exclusions.excludes(vol, idx) {
                            continue;
                        }
                        all_results.push(SearchResultItem {
                            drive_letter: vol.drive_letter,
                            record_idx: idx,
                            generation: vol.generation(),
                        });
                        tantivy_vol_matches
                            .entry(vol.drive_letter)
                            .or_default()
                            .push(idx);
                        snippets.insert((vol.drive_letter, rec.frn), hit.snippet.clone());
                    }
                }

                for (drive, matches) in tantivy_vol_matches {
                    if let Some(vol) = lock.get(&drive) {
                        total_bytes =
                            total_bytes.saturating_add(vol.calculate_matched_bytes(&matches));
                    }
                }
            } else {
                // One complete hit set per content term, then the core evaluator applies
                // Not/Or/And uniformly over content and metadata terms. Records without any hit
                // stay in the game: negation and disjunction are decided per record, so no top-k
                // bound may hide a hit. Snippets are display-only: a term whose scored search is
                // complete contributes them; a very common term keeps just its hit set, so a
                // query cannot retain a snippet for every matching document.
                let term_index: HashMap<&str, usize> = content_queries
                    .iter()
                    .enumerate()
                    .map(|(index, term)| (term.as_str(), index))
                    .collect();
                let positive_terms: HashSet<String> = query_expr
                    .extract_positive_content_queries()
                    .into_iter()
                    .collect();
                let mut hits_by_drive: HashMap<char, Vec<Vec<u64>>> = HashMap::new();
                let mut snippets_by_key: HashMap<(char, u64), String> = HashMap::new();
                for (term_slot, term) in content_queries.iter().enumerate() {
                    if cancel.is_cancelled() {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                    // Snippets exist only for terms whose polarity can make a row match. A term
                    // below a Not never explains the result, and its snippet would mislabel rows
                    // that another branch matched.
                    let wants_snippet = positive_terms.contains(term);
                    let complete_scored = if wants_snippet {
                        match searcher.count_hits(term) {
                            Ok(0) => continue,
                            Ok(count) => count < CONTENT_RESULT_TOP_K,
                            Err(err) => {
                                tracing::warn!("content count for {term:?} failed: {err}");
                                return SearchOutcome::empty(highlight_terms, snippets);
                            }
                        }
                    } else {
                        false
                    };
                    if complete_scored {
                        let hits = match searcher.search(term, CONTENT_RESULT_TOP_K) {
                            Ok(hits) => hits,
                            Err(err) => {
                                tracing::warn!("content search for {term:?} failed: {err}");
                                return SearchOutcome::empty(highlight_terms, snippets);
                            }
                        };
                        for hit in &hits {
                            let Some(drive_letter) = u16::try_from(hit.volume_id)
                                .ok()
                                .and_then(FileRecord::drive_from_volume_id)
                            else {
                                continue;
                            };
                            hits_by_drive
                                .entry(drive_letter)
                                .or_insert_with(|| vec![Vec::new(); content_queries.len()])
                                [term_slot]
                                .push(hit.frn);
                            snippets_by_key.insert((drive_letter, hit.frn), hit.snippet.clone());
                        }
                    } else {
                        let pairs = match searcher.search_all_frns(term) {
                            Ok(pairs) => pairs,
                            Err(err) => {
                                tracing::warn!("content search for {term:?} failed: {err}");
                                return SearchOutcome::empty(highlight_terms, snippets);
                            }
                        };
                        for (volume_id, frn) in pairs {
                            let Some(drive_letter) = u16::try_from(volume_id)
                                .ok()
                                .and_then(FileRecord::drive_from_volume_id)
                            else {
                                continue;
                            };
                            hits_by_drive
                                .entry(drive_letter)
                                .or_insert_with(|| vec![Vec::new(); content_queries.len()])
                                [term_slot]
                                .push(frn);
                        }
                    }
                }

                lock = self.volumes.read().unwrap();
                let exclusions = PathExclusions::new(&lock, &excluded_paths);
                for vol in lock.values() {
                    if cancel.is_cancelled() {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    }
                    if excluded_drives
                        .iter()
                        .any(|&ex| ex.eq_ignore_ascii_case(&vol.drive_letter))
                    {
                        continue;
                    }
                    if !vol.status.state().is_synced() && vol.records().is_empty() {
                        continue;
                    }
                    // One record-index bitmap per content term. A hit FRN sets every record it
                    // maps to, so each hardlink row of a hit file carries the hit. Each FRN list
                    // is released as soon as its term's bitmap is built. A volume without hits
                    // keeps an empty list: negation still evaluates every record, and the
                    // resolver answers false for all terms.
                    let sets: Vec<BitSet> = hits_by_drive
                        .remove(&vol.drive_letter)
                        .map(|frns_by_term| {
                            frns_by_term
                                .into_iter()
                                .map(|frns| hit_bitset(vol, &frns, cancel))
                                .collect()
                        })
                        .unwrap_or_default();
                    let matcher = |term: &str, idx: u32| {
                        term_index.get(term).is_some_and(|&index| {
                            sets.get(index)
                                .is_some_and(|bits| bits.contains(idx as usize))
                        })
                    };
                    let matches = cairn_core::engine::execute_query_with_content(
                        vol.records(),
                        vol.arena(),
                        vol.path_cache(),
                        vol.drive_letter,
                        &query_expr,
                        &matcher,
                        cancel,
                    );
                    let Some(vol_matches) =
                        collect_volume_matches(vol, matches, &exclusions, cancel, &mut all_results)
                    else {
                        return SearchOutcome::empty(highlight_terms, snippets);
                    };
                    for &idx in &vol_matches {
                        let frn = vol.records()[idx as usize].frn;
                        if let Some(snippet) = snippets_by_key.get(&(vol.drive_letter, frn)) {
                            snippets.insert((vol.drive_letter, frn), snippet.clone());
                        }
                    }
                    total_bytes =
                        total_bytes.saturating_add(vol.calculate_matched_bytes(&vol_matches));
                }
            }
        } else {
            lock = self.volumes.read().unwrap();
            // Every match would otherwise pay a full-path reconstruction just to apply a few
            // segment rules (a broad query could match most of the table). Compute the same
            // verdict once per volume and use it as an O(1) lookup; unsupported rule forms keep
            // the reference behavior.
            let exclusions = PathExclusions::new(&lock, &excluded_paths);
            let active_vols: Vec<_> = lock
                .values()
                .filter(|volume| volume_is_active(volume, &excluded_drives))
                .collect();
            // The view is the requested volume's whole order. A scoped query names it; an
            // unscoped one may only view the sole active volume. Both are re-checked against
            // the active set under this lock, so a configuration change or a rebuild between
            // the order build and this read cannot serve a volume the query no longer covers.
            if let Some(drive) = view_drive {
                let view_allowed = match drive_scope {
                    Some(_) => active_vols.iter().any(|vol| vol.drive_letter == drive),
                    None => active_vols.len() == 1 && active_vols[0].drive_letter == drive,
                };
                if view_allowed {
                    if let Some(volume) = lock.get(&drive) {
                        if volume.presorted_order(sort_by).is_some() {
                            return Self::all_files_outcome(
                                volume,
                                sort_by,
                                sort_asc,
                                exclusions,
                                cancel,
                                highlight_terms,
                                snippets,
                            );
                        }
                    }
                }
            }

            if active_vols.len() == 1 {
                let vol = active_vols[0];
                if cancel.is_cancelled() {
                    return SearchOutcome::empty(highlight_terms, snippets);
                }
                let presorted_col = vol.presorted_order(sort_by);
                let mut matches = cairn_core::engine::execute_query_ordered(
                    vol.records(),
                    vol.arena(),
                    vol.path_cache(),
                    vol.drive_letter,
                    &query_expr,
                    presorted_col,
                    cancel,
                );
                let Some(vol_bytes) =
                    filter_unique_volume_matches(vol, &mut matches, &exclusions, cancel)
                else {
                    return SearchOutcome::empty(highlight_terms, snippets);
                };
                if presorted_col.is_some() {
                    if !sort_asc {
                        matches.reverse();
                    }
                } else if matches.len() > 1 && !cancel.is_cancelled() {
                    let dir = if sort_asc {
                        SortDirection::Ascending
                    } else {
                        SortDirection::Descending
                    };
                    cairn_core::sort::sort_indices(
                        &mut matches,
                        vol.records(),
                        vol.arena(),
                        vol.path_cache(),
                        vol.drive_letter,
                        sort_by,
                        dir,
                        cancel,
                    );
                }
                if cancel.is_cancelled() {
                    return SearchOutcome::empty(highlight_terms, snippets);
                }
                let gen = vol.generation();
                let drive_letter = vol.drive_letter;
                all_results = matches
                    .into_iter()
                    .map(|record_idx| SearchResultItem {
                        drive_letter,
                        record_idx,
                        generation: gen,
                    })
                    .collect();
                let total = all_results.len();
                return SearchOutcome {
                    rows: MatchSet::Items(all_results),
                    total,
                    highlight_terms,
                    snippets,
                    total_bytes: vol_bytes,
                };
            }

            for vol in active_vols {
                if cancel.is_cancelled() {
                    return SearchOutcome::empty(highlight_terms, snippets);
                }
                let matches = cairn_core::engine::execute_query_ordered(
                    vol.records(),
                    vol.arena(),
                    vol.path_cache(),
                    vol.drive_letter,
                    &query_expr,
                    None,
                    cancel,
                );
                let Some(vol_matches) =
                    collect_volume_matches(vol, matches, &exclusions, cancel, &mut all_results)
                else {
                    return SearchOutcome::empty(highlight_terms, snippets);
                };
                total_bytes = total_bytes.saturating_add(vol.calculate_matched_bytes(&vol_matches));
            }
        }

        // Multi-volume parallel sort
        let dir = if sort_asc {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };

        if all_results.len() > 1 && !cancel.is_cancelled() {
            let mut vol_by_letter: [Option<&VolumeIndex>; 26] = [None; 26];
            for (ch, vol) in lock.iter() {
                if let Some(idx) = FileRecord::volume_id_from_drive(*ch) {
                    vol_by_letter[idx as usize] = Some(vol);
                }
            }

            let get_vol = |letter: char| -> &VolumeIndex {
                if let Some(idx) = FileRecord::volume_id_from_drive(letter) {
                    if let Some(vol) = vol_by_letter[idx as usize] {
                        return vol;
                    }
                }
                lock.get(&letter).expect("volume index exists for drive")
            };

            enum SortKey<'a> {
                Name(&'a str),
                Path(std::sync::Arc<str>, &'a str),
                Size(u64, &'a str),
                DateModified(i64, &'a str),
                DateCreated(i64, &'a str),
                Extension(&'a str, &'a str),
            }

            let mut decorated: Vec<(SortKey, SearchResultItem)> = all_results
                .into_par_iter()
                .map(|item| {
                    let vol = get_vol(item.drive_letter);
                    let rec = &vol.records()[item.record_idx as usize];
                    let name = vol.arena().get_name(rec.name_offset, rec.name_len);
                    let key = match sort_by {
                        SortColumn::Name => SortKey::Name(name),
                        SortColumn::Path => {
                            let parent = vol.path_cache().get_parent_path(
                                vol.records(),
                                vol.arena(),
                                vol.drive_letter,
                                item.record_idx,
                            );
                            SortKey::Path(parent, name)
                        }
                        SortColumn::Size => SortKey::Size(rec.size, name),
                        SortColumn::DateModified => SortKey::DateModified(rec.modified_ts, name),
                        SortColumn::DateCreated => SortKey::DateCreated(rec.created_ts, name),
                        SortColumn::Extension => {
                            let ext =
                                vol.arena()
                                    .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
                            SortKey::Extension(ext, name)
                        }
                    };
                    (key, item)
                })
                .collect();

            if cancel.is_cancelled() {
                return SearchOutcome::empty(highlight_terms, snippets);
            }

            decorated.par_sort_unstable_by(|(ka, a), (kb, b)| {
                let ord = match (ka, kb) {
                    (SortKey::Name(na), SortKey::Name(nb)) => cairn_core::sort::natural_cmp(na, nb),
                    (SortKey::Path(pa, na), SortKey::Path(pb, nb)) => pa
                        .cmp(pb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::Size(sa, na), SortKey::Size(sb, nb)) => sa
                        .cmp(sb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::DateModified(ma, na), SortKey::DateModified(mb, nb)) => ma
                        .cmp(mb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::DateCreated(ca, na), SortKey::DateCreated(cb, nb)) => ca
                        .cmp(cb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::Extension(ea, na), SortKey::Extension(eb, nb)) => {
                        cairn_core::sort::cmp_ascii_ignore_case(ea, eb)
                            .then_with(|| cairn_core::sort::natural_cmp(na, nb))
                    }
                    _ => std::cmp::Ordering::Equal,
                };

                let primary = if dir == SortDirection::Descending {
                    ord.reverse()
                } else {
                    ord
                };

                primary
                    .then_with(|| a.drive_letter.cmp(&b.drive_letter))
                    .then_with(|| a.record_idx.cmp(&b.record_idx))
            });

            all_results = decorated.into_iter().map(|(_, item)| item).collect();
        }

        if cancel.is_cancelled() {
            return SearchOutcome::empty(highlight_terms, snippets);
        }

        let total = all_results.len();
        SearchOutcome {
            rows: MatchSet::Items(all_results),
            total,
            highlight_terms,
            snippets,
            total_bytes,
        }
    }
}

/// Retains non-excluded unique record indices in place and returns their space at the
/// highest hit point. Returns `None` if `cancel` triggers mid-loop.
fn filter_unique_volume_matches(
    vol: &VolumeIndex,
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
        if exclusions.excludes(vol, idx) {
            continue;
        }
        matches[write] = idx;
        write += 1;
    }
    matches.truncate(write);
    Some(vol.calculate_matched_bytes(matches))
}

/// Filters `matches` against `exclusions` and appends non-excluded [`SearchResultItem`]s into
/// `all_results`. Returns `None` if `cancel` triggers mid-loop.
fn collect_volume_matches(
    vol: &VolumeIndex,
    matches: Vec<u32>,
    exclusions: &PathExclusions,
    cancel: &CancellationToken,
    all_results: &mut Vec<SearchResultItem>,
) -> Option<Vec<u32>> {
    all_results.reserve(matches.len());
    let mut vol_matches = Vec::with_capacity(matches.len());
    for (i, idx) in matches.into_iter().enumerate() {
        if (i & 0xFFF) == 0 && cancel.is_cancelled() {
            return None;
        }
        if exclusions.excludes(vol, idx) {
            continue;
        }
        vol_matches.push(idx);
        all_results.push(SearchResultItem {
            drive_letter: vol.drive_letter,
            record_idx: idx,
            generation: vol.generation(),
        });
    }
    Some(vol_matches)
}

/// A query whose whole shape is a conjunction of positive terms can be answered by one joined
/// Tantivy search filtered by the compiled metadata expression; any Not or Or requires the
/// record-by-record evaluator.
fn is_positive_conjunction(expr: &QueryExpr) -> bool {
    match expr {
        QueryExpr::Term(_) => true,
        QueryExpr::And(subs) => !subs.is_empty() && subs.iter().all(is_positive_conjunction),
        _ => false,
    }
}

/// How many hit FRNs one resolution chunk covers.
const HIT_RESOLVE_CHUNK: usize = 16_384;

/// Record-index bitmap for one term's hit FRNs; a hit FRN marks every hardlink row it maps to.
/// Resolving the list costs the same order of work as a full scan and runs inside the volumes
/// read lock the USN poller waits on, so it is split into parallel chunks and OR-merged. A
/// cancelled query stops at the next chunk.
fn hit_bitset(vol: &VolumeIndex, frns: &[u64], cancel: &CancellationToken) -> BitSet {
    if frns.is_empty() {
        return BitSet::with_capacity(0);
    }
    let total_records = vol.records().len();
    frns.par_chunks(HIT_RESOLVE_CHUNK)
        .map(|chunk| {
            if cancel.is_cancelled() {
                return BitSet::with_capacity(total_records);
            }
            let mut bits = BitSet::with_capacity(total_records);
            for &frn in chunk {
                if let Some(indices) = vol.record_indices_for_frn(frn) {
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

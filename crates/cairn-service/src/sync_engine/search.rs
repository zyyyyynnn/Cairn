// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Metadata and content search: query evaluation, exclusion filtering, result
//! ordering and snippets.

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

pub type MultiSearchResult = (
    Vec<SearchResultItem>,
    usize,
    Vec<String>,
    HashMap<(char, u64), String>,
    u64,
);

impl MultiVolumeEngine {
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
    ) -> MultiSearchResult {
        let query_expr = parse_query_with(raw_query, defaults);
        let highlight_terms = query_expr.collect_highlight_terms();
        let content_queries = query_expr.extract_content_queries();
        let mft_query_expr = query_expr
            .strip_content_queries()
            .unwrap_or_else(|| QueryExpr::Term(cairn_core::syntax::SearchTerm::default()));

        // Only the all-files query can consume a presorted order. A filtered query scans the
        // records and sorts just its matches, so rebuilding the volume's whole name/path/size
        // array first is pure latency: measured 0.65-0.9 s per filtered query on the live index.
        if content_queries.is_empty() && query_expr.is_empty() {
            self.ensure_presorted_indices(sort_by);
        }

        let (excluded_drives, excluded_paths) = {
            let cfg = self.config.read().unwrap();
            (cfg.excluded_drives.clone(), cfg.excluded_paths.clone())
        };

        let mut all_results = Vec::new();
        let mut snippets = HashMap::new();
        // Sum of matched files' own sizes. Directories are containers; adding their recursive
        // size would double count files that are also matched (the all-files view contains both).
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
                return (all_results, 0, highlight_terms, snippets, 0);
            };

            // A conjunction of positive terms — content and metadata alike — is one Tantivy
            // query with one clause per content term plus the compiled metadata filter; any Not
            // or Or needs record-by-record boolean evaluation instead of flattening the
            // expression.
            if is_positive_conjunction(&query_expr) {
                // The scored top-k bound is historical: truncation shortens the reply before the
                // metadata filter runs, so a conjunction over a very common word can show fewer
                // rows than its true match set.
                let content_hits =
                    match searcher.search_terms(&content_queries, CONTENT_RESULT_TOP_K) {
                        Ok(hits) => hits,
                        Err(err) => {
                            tracing::warn!("content search for {content_queries:?} failed: {err}");
                            return (Vec::new(), 0, highlight_terms, snippets, 0);
                        }
                    };

                if cancel.is_cancelled() || content_hits.is_empty() {
                    return (all_results, 0, highlight_terms, snippets, 0);
                }

                // Precompile MFT filter outside the lock to keep lock duration minimal
                let compiled_mft = if !mft_query_expr.is_empty() {
                    Some(cairn_core::engine::CompiledQuery::compile(&mft_query_expr))
                } else {
                    None
                };

                // Acquire read lock with minimal critical section to convert and merge results
                lock = self.volumes.read().unwrap();
                let exclusion_masks = path_exclusion_masks(&lock, &excluded_paths);
                let mut tantivy_vol_matches: HashMap<char, Vec<u32>> = HashMap::new();

                for hit in &content_hits {
                    if cancel.is_cancelled() {
                        return (Vec::new(), 0, highlight_terms, snippets, 0);
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
                    if !vol.status.state().is_synced() && vol.records.is_empty() {
                        continue;
                    }
                    let Some(indices) = vol.frn_to_idx.get(&hit.frn) else {
                        continue;
                    };

                    for &idx in indices {
                        let rec = &vol.records[idx as usize];
                        if rec.is_tombstone() {
                            continue;
                        }
                        if let Some(ref compiled) = compiled_mft {
                            if !compiled.evaluate_record(
                                rec,
                                idx,
                                &vol.records,
                                &vol.arena,
                                &vol.path_cache,
                                vol.drive_letter,
                            ) {
                                continue;
                            }
                        }
                        if is_record_excluded(vol, idx, &exclusion_masks, &excluded_paths) {
                            continue;
                        }
                        all_results.push(SearchResultItem {
                            drive_letter: vol.drive_letter,
                            record_idx: idx,
                            generation: vol.generation,
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
                let mut hits_by_drive: HashMap<char, Vec<HashSet<u64>>> = HashMap::new();
                let mut snippets_by_key: HashMap<(char, u64), String> = HashMap::new();
                for (term_slot, term) in content_queries.iter().enumerate() {
                    if cancel.is_cancelled() {
                        return (Vec::new(), 0, highlight_terms, snippets, 0);
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
                                return (Vec::new(), 0, highlight_terms, snippets, 0);
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
                                return (Vec::new(), 0, highlight_terms, snippets, 0);
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
                                .or_insert_with(|| vec![HashSet::new(); content_queries.len()])
                                [term_slot]
                                .insert(hit.frn);
                            snippets_by_key.insert((drive_letter, hit.frn), hit.snippet.clone());
                        }
                    } else {
                        let pairs = match searcher.search_all_frns(term) {
                            Ok(pairs) => pairs,
                            Err(err) => {
                                tracing::warn!("content search for {term:?} failed: {err}");
                                return (Vec::new(), 0, highlight_terms, snippets, 0);
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
                                .or_insert_with(|| vec![HashSet::new(); content_queries.len()])
                                [term_slot]
                                .insert(frn);
                        }
                    }
                }

                lock = self.volumes.read().unwrap();
                let exclusion_masks = path_exclusion_masks(&lock, &excluded_paths);
                for vol in lock.values() {
                    if cancel.is_cancelled() {
                        return (Vec::new(), 0, highlight_terms, snippets, 0);
                    }
                    if excluded_drives
                        .iter()
                        .any(|&ex| ex.eq_ignore_ascii_case(&vol.drive_letter))
                    {
                        continue;
                    }
                    if !vol.status.state().is_synced() && vol.records.is_empty() {
                        continue;
                    }
                    let matcher = |term: &str, frn: u64| {
                        term_index.get(term).is_some_and(|&index| {
                            hits_by_drive
                                .get(&vol.drive_letter)
                                .is_some_and(|sets| sets[index].contains(&frn))
                        })
                    };
                    let matches = cairn_core::engine::execute_query_with_content(
                        &vol.records,
                        &vol.arena,
                        &vol.path_cache,
                        vol.drive_letter,
                        &query_expr,
                        &matcher,
                        cancel,
                    );
                    let Some(vol_matches) = collect_volume_matches(
                        vol,
                        matches,
                        &exclusion_masks,
                        &excluded_paths,
                        cancel,
                        &mut all_results,
                    ) else {
                        return (Vec::new(), 0, highlight_terms, snippets, 0);
                    };
                    for &idx in &vol_matches {
                        let frn = vol.records[idx as usize].frn;
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
            let mut single_vol_presorted = false;
            // Every match would otherwise pay a full-path reconstruction just to apply a few
            // segment rules (a broad query could match most of the table). Compute the same
            // verdict once per volume and use it as an O(1) lookup; unsupported rule forms keep
            // the reference behavior.
            let exclusion_masks = path_exclusion_masks(&lock, &excluded_paths);
            let active_vols: Vec<_> = lock
                .values()
                .filter(|v| {
                    if excluded_drives
                        .iter()
                        .any(|&ex| ex.eq_ignore_ascii_case(&v.drive_letter))
                    {
                        return false;
                    }
                    v.status.state().is_synced() || !v.records.is_empty()
                })
                .collect();

            let is_single_vol = active_vols.len() == 1;
            for vol in active_vols {
                if cancel.is_cancelled() {
                    return (Vec::new(), 0, highlight_terms, snippets, 0);
                }
                let presorted_col = presorted_order(vol, sort_by);
                if is_single_vol {
                    single_vol_presorted = presorted_col.is_some();
                }
                let matches = cairn_core::engine::execute_query_ordered(
                    &vol.records,
                    &vol.arena,
                    &vol.path_cache,
                    vol.drive_letter,
                    &query_expr,
                    presorted_col,
                    cancel,
                );
                let Some(vol_matches) = collect_volume_matches(
                    vol,
                    matches,
                    &exclusion_masks,
                    &excluded_paths,
                    cancel,
                    &mut all_results,
                ) else {
                    return (Vec::new(), 0, highlight_terms, snippets, 0);
                };
                total_bytes = total_bytes.saturating_add(vol.calculate_matched_bytes(&vol_matches));
            }

            if single_vol_presorted {
                if !sort_asc {
                    all_results.reverse();
                }
                if cancel.is_cancelled() {
                    return (Vec::new(), 0, highlight_terms, snippets, 0);
                }
                let total = all_results.len();
                return (all_results, total, highlight_terms, snippets, total_bytes);
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
                    let rec = &vol.records[item.record_idx as usize];
                    let name = vol.arena.get_name(rec.name_offset, rec.name_len);
                    let key = match sort_by {
                        SortColumn::Name => SortKey::Name(name),
                        SortColumn::Path => {
                            let parent = vol.path_cache.get_parent_path(
                                &vol.records,
                                &vol.arena,
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
                                vol.arena
                                    .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
                            SortKey::Extension(ext, name)
                        }
                    };
                    (key, item)
                })
                .collect();

            if cancel.is_cancelled() {
                return (Vec::new(), 0, highlight_terms, snippets, 0);
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
            return (Vec::new(), 0, highlight_terms, snippets, 0);
        }

        let total = all_results.len();
        (all_results, total, highlight_terms, snippets, total_bytes)
    }
}

/// Filters `matches` against `exclusion_masks` / `excluded_paths` and appends non-excluded
/// [`SearchResultItem`]s into `all_results`. Returns `None` if `cancel` triggers mid-loop.
fn collect_volume_matches(
    vol: &VolumeIndex,
    matches: Vec<u32>,
    exclusion_masks: &Option<HashMap<char, Vec<bool>>>,
    excluded_paths: &[String],
    cancel: &CancellationToken,
    all_results: &mut Vec<SearchResultItem>,
) -> Option<Vec<u32>> {
    all_results.reserve(matches.len());
    let mut vol_matches = Vec::with_capacity(matches.len());
    for (i, idx) in matches.into_iter().enumerate() {
        if (i & 0xFFF) == 0 && cancel.is_cancelled() {
            return None;
        }
        if is_record_excluded(vol, idx, exclusion_masks, excluded_paths) {
            continue;
        }
        vol_matches.push(idx);
        all_results.push(SearchResultItem {
            drive_letter: vol.drive_letter,
            record_idx: idx,
            generation: vol.generation,
        });
    }
    Some(vol_matches)
}

fn is_record_excluded(
    vol: &VolumeIndex,
    idx: u32,
    exclusion_masks: &Option<HashMap<char, Vec<bool>>>,
    excluded_paths: &[String],
) -> bool {
    if exclusion_masks.as_ref().is_some_and(|masks| {
        masks
            .get(&vol.drive_letter)
            .is_some_and(|mask| mask.get(idx as usize).copied().unwrap_or(false))
    }) {
        return true;
    }
    if exclusion_masks.is_none() && !excluded_paths.is_empty() {
        let full_path =
            vol.path_cache
                .get_full_path(&vol.records, &vol.arena, vol.drive_letter, idx);
        if crate::exclusion::is_path_excluded(&full_path, excluded_paths) {
            return true;
        }
    }
    false
}

/// One exclusion verdict per record, shared by every query path. `None` means the rules use a
/// form the mask cannot express, so callers must check each path.
fn path_exclusion_masks(
    volumes: &HashMap<char, VolumeIndex>,
    excluded_paths: &[String],
) -> Option<HashMap<char, Vec<bool>>> {
    if excluded_paths.is_empty() {
        return None;
    }
    let mut masks = HashMap::new();
    for (drive, volume) in volumes {
        masks.insert(*drive, volume.excluded_record_mask(excluded_paths)?);
    }
    Some(masks)
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

/// Presorted index to use as the order for `sort_by`, or `None` when it cannot be trusted: a
/// dirty index no longer matches the records, and DateCreated/Extension have no presorted array.
pub(super) fn presorted_order(vol: &VolumeIndex, sort_by: SortColumn) -> Option<&[u32]> {
    if vol.presorted_dirty {
        return None;
    }
    let order = match sort_by {
        SortColumn::Name => &vol.presorted.by_name,
        SortColumn::Size => &vol.presorted.by_size,
        SortColumn::DateModified => &vol.presorted.by_date_modified,
        SortColumn::Path => &vol.presorted.by_path,
        SortColumn::DateCreated | SortColumn::Extension => return None,
    };
    (!order.is_empty()).then_some(order.as_slice())
}

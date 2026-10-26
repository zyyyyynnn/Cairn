// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fast SIMD query execution engine with Rayon parallel scanning and cooperative cancellation.

use crate::arena::NamePool;
use crate::model::FileRecord;
use crate::path_cache::DirectoryPathCache;
use crate::syntax::{QueryExpr, SearchTerm};
use memchr::memmem::Finder;
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::ops::Range;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative cancellation token checked during long scans and sorts.
#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[inline(always)]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Evaluates a `QueryExpr` against indexed file records in parallel, preserving the specified record order if given.
pub fn execute_query_ordered(
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    query: &QueryExpr,
    order: Option<&[u32]>,
    cancel: &CancellationToken,
) -> Vec<u32> {
    if query.is_empty() {
        if cancel.is_cancelled() {
            return Vec::new();
        }
        // Return all non-tombstone records
        if let Some(order_indices) = order {
            let mut matched = Vec::with_capacity(order_indices.len());
            for (i, &idx) in order_indices.iter().enumerate() {
                if (i & 0x3FFF) == 0 && cancel.is_cancelled() {
                    return Vec::new();
                }
                if !records[idx as usize].is_tombstone() {
                    matched.push(idx);
                }
            }
            return matched;
        } else {
            let mut matched = Vec::with_capacity(records.len());
            for (idx, rec) in records.iter().enumerate() {
                if (idx & 0x3FFF) == 0 && cancel.is_cancelled() {
                    return Vec::new();
                }
                if !rec.is_tombstone() {
                    matched.push(idx as u32);
                }
            }
            return matched;
        }
    }

    let regexes = RegexPrograms::compile(query);
    let chunk_size = 16384;

    let scan_chunk = |chunk: &[u32]| -> Vec<u32> {
        if cancel.is_cancelled() {
            return Vec::new();
        }

        let mut matched = Vec::new();
        for &idx in chunk {
            let rec = &records[idx as usize];
            if rec.is_tombstone() {
                continue;
            }

            if evaluate_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                query,
                &regexes,
            ) {
                matched.push(idx);
            }
        }
        matched
    };

    let scan_range = |range: Range<u32>| -> Vec<u32> {
        if cancel.is_cancelled() {
            return Vec::new();
        }

        let mut matched = Vec::new();
        for idx in range {
            let rec = &records[idx as usize];
            if rec.is_tombstone() {
                continue;
            }

            if evaluate_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                query,
                &regexes,
            ) {
                matched.push(idx);
            }
        }
        matched
    };

    if let Some(order_slice) = order {
        order_slice
            .par_chunks(chunk_size)
            .flat_map(scan_chunk)
            .collect()
    } else {
        let total = records.len() as u32;
        let num_chunks = total.div_ceil(chunk_size as u32);
        (0..num_chunks)
            .into_par_iter()
            .flat_map(|c| {
                let start = c * chunk_size as u32;
                let end = (start + chunk_size as u32).min(total);
                scan_range(start..end)
            })
            .collect()
    }
}

/// Evaluates a `QueryExpr` against indexed file records in parallel.
pub fn execute_query(
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    query: &QueryExpr,
    cancel: &CancellationToken,
) -> Vec<u32> {
    execute_query_ordered(
        records,
        arena,
        path_cache,
        drive_letter,
        query,
        None,
        cancel,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobToken {
    Star,       // * matches anything except \ and /
    DoubleStar, // ** matches anything including \ and /
    Question,   // ? matches single char except \ and /
    Char(char), // exact or folded char
}

/// Precomputed query text representations compiled once per query execution to eliminate
/// inner-loop heap allocations in `match_text` and `evaluate_term`.
#[derive(Clone, Debug)]
pub(crate) struct CompiledTerm {
    pub(crate) ascii_lower: String,
    pub(crate) lower: String,
    pub(crate) folded: Vec<u8>,
    pub(crate) finder: Finder<'static>,
    pub(crate) glob_tokens: Option<Vec<GlobToken>>,
    pub(crate) glob_tokens_cs: Option<Vec<GlobToken>>,
}

/// Precompiled query execution state containing regex programs and precomputed query term representations.
pub struct CompiledQuery {
    programs: RegexPrograms,
}

impl CompiledQuery {
    pub fn compile(query: &QueryExpr) -> Self {
        Self {
            programs: RegexPrograms::compile(query),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_record(
        &self,
        rec: &FileRecord,
        idx: u32,
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
        query: &QueryExpr,
    ) -> bool {
        evaluate_expr(
            rec,
            idx,
            records,
            arena,
            path_cache,
            drive_letter,
            query,
            &self.programs,
        )
    }
}

/// The regex programs and precomputed query representations one query execution needs,
/// compiled before the first record is read.
///
/// Deliberately scoped to a single [`execute_query`] call and immutable once built: it is
/// shared by the parallel chunks of that query only, never across queries.
struct RegexPrograms {
    programs: HashMap<String, Regex>,
    terms: HashMap<String, CompiledTerm>,
    /// Test-only witnesses of the per-record recompilation defect: `compiles` must stay at
    /// the number of distinct patterns however many records are matched, and `lookups` must
    /// reach one per match attempt, i.e. the record loop really reads this cache.
    #[cfg(test)]
    compiles: AtomicUsize,
    #[cfg(test)]
    lookups: AtomicUsize,
    #[cfg(test)]
    term_compiles: AtomicUsize,
    #[cfg(test)]
    term_lookups: AtomicUsize,
}

impl RegexPrograms {
    /// Compiles each distinct regex pattern and search term text of `query` exactly once.
    fn compile(query: &QueryExpr) -> Self {
        let mut built = Self {
            programs: HashMap::new(),
            terms: HashMap::new(),
            #[cfg(test)]
            compiles: AtomicUsize::new(0),
            #[cfg(test)]
            lookups: AtomicUsize::new(0),
            #[cfg(test)]
            term_compiles: AtomicUsize::new(0),
            #[cfg(test)]
            term_lookups: AtomicUsize::new(0),
        };
        for pattern in query.collect_regex_patterns() {
            #[cfg(test)]
            built.compiles.fetch_add(1, Ordering::Relaxed);
            if let Ok(re) = Regex::new(pattern) {
                built.programs.insert(pattern.to_string(), re);
            }
        }
        for text in query.collect_search_texts() {
            #[cfg(test)]
            built.term_compiles.fetch_add(1, Ordering::Relaxed);
            let folded = NamePool::fold_query_bytes(text);
            let finder = Finder::new(&folded).into_owned();

            let (glob_tokens, glob_tokens_cs) = if text.contains('*') || text.contains('?') {
                (
                    Some(parse_glob_tokens(text, false)),
                    Some(parse_glob_tokens(text, true)),
                )
            } else {
                (None, None)
            };

            built.terms.insert(
                text.to_string(),
                CompiledTerm {
                    ascii_lower: text.to_ascii_lowercase(),
                    lower: text.to_lowercase(),
                    folded,
                    finder,
                    glob_tokens,
                    glob_tokens_cs,
                },
            );
        }
        built
    }

    /// A program set with nothing in it, for the tests that exercise non-regex matching.
    #[cfg(test)]
    fn empty() -> Self {
        Self {
            programs: HashMap::new(),
            terms: HashMap::new(),
            compiles: AtomicUsize::new(0),
            lookups: AtomicUsize::new(0),
            term_compiles: AtomicUsize::new(0),
            term_lookups: AtomicUsize::new(0),
        }
    }

    fn get(&self, pattern: &str) -> Option<&Regex> {
        #[cfg(test)]
        self.lookups.fetch_add(1, Ordering::Relaxed);
        self.programs.get(pattern)
    }

    fn get_term(&self, text: &str) -> Option<&CompiledTerm> {
        #[cfg(test)]
        self.term_lookups.fetch_add(1, Ordering::Relaxed);
        self.terms.get(text)
    }

    fn get_ascii_lower<'b>(&'b self, text: &'b str) -> std::borrow::Cow<'b, str> {
        if let Some(t) = self.get_term(text) {
            std::borrow::Cow::Borrowed(&t.ascii_lower)
        } else {
            std::borrow::Cow::Owned(text.to_ascii_lowercase())
        }
    }

    fn get_lower<'b>(&'b self, text: &'b str) -> std::borrow::Cow<'b, str> {
        if let Some(t) = self.get_term(text) {
            std::borrow::Cow::Borrowed(&t.lower)
        } else {
            std::borrow::Cow::Owned(text.to_lowercase())
        }
    }

    fn get_folded<'b>(&'b self, text: &'b str) -> std::borrow::Cow<'b, [u8]> {
        if let Some(t) = self.get_term(text) {
            std::borrow::Cow::Borrowed(&t.folded)
        } else {
            std::borrow::Cow::Owned(NamePool::fold_query_bytes(text))
        }
    }

    #[cfg(test)]
    fn compiles(&self) -> usize {
        self.compiles.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn lookups(&self) -> usize {
        self.lookups.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn term_compiles(&self) -> usize {
        self.term_compiles.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn term_lookups(&self) -> usize {
        self.term_lookups.load(Ordering::Relaxed)
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_expr(
    rec: &FileRecord,
    idx: u32,
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    expr: &QueryExpr,
    regexes: &RegexPrograms,
) -> bool {
    match expr {
        QueryExpr::Term(term) => evaluate_term(
            rec,
            idx,
            records,
            arena,
            path_cache,
            drive_letter,
            term,
            regexes,
        ),
        QueryExpr::And(terms) => terms.iter().all(|sub| {
            evaluate_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                sub,
                regexes,
            )
        }),
        QueryExpr::Or(terms) => terms.iter().any(|sub| {
            evaluate_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                sub,
                regexes,
            )
        }),
        QueryExpr::Not(inner) => !evaluate_expr(
            rec,
            idx,
            records,
            arena,
            path_cache,
            drive_letter,
            inner,
            regexes,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_term(
    rec: &FileRecord,
    idx: u32,
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    term: &SearchTerm,
    regexes: &RegexPrograms,
) -> bool {
    // 0. Unparsable search item (e.g. a lone `size:`): constrain to nothing rather than
    //    silently matching every record.
    if term.match_nothing {
        return false;
    }

    // 1. Fast filters: file_only / folder_only, then the query-wide attribute constraints
    //    a `QueryDefaults` put on this item (hidden and system records are opt-out).
    if term.exclude_hidden && rec.is_hidden() {
        return false;
    }
    if term.exclude_system && rec.is_system() {
        return false;
    }
    if term.file_only && rec.is_dir() {
        return false;
    }
    if term.folder_only && !rec.is_dir() {
        return false;
    }

    // 2. Drive filter
    if let Some(req_drive) = term.drive {
        if !drive_letter.eq_ignore_ascii_case(&req_drive) {
            return false;
        }
    }

    // 3. Extension filter
    if !term.ext_filter.is_empty() {
        if !rec.has_ext() {
            return false;
        }
        let ext = arena.get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
        // SPEC QRY-04: `ext:` entries support `*` and `?`. Wildcard-free entries keep the exact
        // comparison so the common `ext:md` case does not pay for glob character allocation.
        let matched = term.ext_filter.iter().any(|pattern| {
            if pattern.contains('*') || pattern.contains('?') {
                glob_match(pattern, ext, false)
            } else {
                ext.eq_ignore_ascii_case(pattern)
            }
        });
        if !matched {
            return false;
        }
    }

    // 4. Size filter
    if let Some(ref size_filter) = term.size_filter {
        if !size_filter.matches(rec.size) {
            return false;
        }
    }

    // 5. Date filters
    if let Some(ref dm_filter) = term.dm_filter {
        if !dm_filter.matches(rec.modified_ts) {
            return false;
        }
    }
    if let Some(ref dc_filter) = term.dc_filter {
        if !dc_filter.matches(rec.created_ts) {
            return false;
        }
    }

    // 6. Parent path filter
    if let Some(ref parent_req) = term.parent_filter {
        let actual_parent = path_cache.get_parent_path(records, arena, drive_letter, idx);
        let trimmed_parent = actual_parent.trim_end_matches('\\');
        let trimmed_req = parent_req.trim_end_matches('\\');
        if !trimmed_parent.eq_ignore_ascii_case(trimmed_req) {
            return false;
        }
    }

    // 7. Content search filter (CNT-06)
    // The MFT in-memory engine only matches file metadata and paths.
    // When a term requires content matching, it must never falsely match all files.
    if term.content_query.is_some() {
        return false;
    }

    // 8. Text match
    if term.text.is_empty() {
        return true;
    }

    if term.match_path {
        let full_path = path_cache.get_full_path(records, arena, drive_letter, idx);
        match_text(&full_path, &term.text, term, regexes)
    } else {
        let name = arena.get_name(rec.name_offset, rec.name_len);
        if term.is_regex
            || term.whole_filename
            || term.case_sensitive
            || term.whole_words
            || term.prefix
            || term.suffix
            || term.start_with
            || term.end_with
        {
            match_text(name, &term.text, term, regexes)
        } else {
            // SIMD fast path on folded_arena
            let folded_name = arena.get_folded(rec.name_offset, rec.name_len);
            if let Some(pre) = regexes.get_term(&term.text) {
                pre.finder.find(folded_name).is_some()
            } else {
                let folded_query = regexes.get_folded(&term.text);
                let finder = Finder::new(&folded_query);
                finder.find(folded_name).is_some()
            }
        }
    }
}

fn word_starts_with(word: &str, query: &str, query_lower: &str, case_sensitive: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    if word.is_empty() {
        return false;
    }
    if case_sensitive {
        word.starts_with(query)
    } else if word.len() >= query.len()
        && word.is_char_boundary(query.len())
        && word[..query.len()].eq_ignore_ascii_case(query)
    {
        true
    } else if word.is_ascii() && query.is_ascii() {
        false
    } else {
        word.to_lowercase().starts_with(query_lower)
    }
}

fn word_ends_with(word: &str, query: &str, query_lower: &str, case_sensitive: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    if word.is_empty() {
        return false;
    }
    if case_sensitive {
        word.ends_with(query)
    } else {
        let offset = word.len().saturating_sub(query.len());
        if word.len() >= query.len()
            && word.is_char_boundary(offset)
            && word[offset..].eq_ignore_ascii_case(query)
        {
            true
        } else if word.is_ascii() && query.is_ascii() {
            false
        } else {
            word.to_lowercase().ends_with(query_lower)
        }
    }
}

fn match_text(target: &str, query: &str, term: &SearchTerm, regexes: &RegexPrograms) -> bool {
    if term.is_regex {
        return regexes.get(query).is_some_and(|re| re.is_match(target));
    }

    if term.whole_filename {
        // Glob wildcard pattern
        if let Some(pre) = regexes.get_term(query) {
            let tokens = if term.case_sensitive {
                pre.glob_tokens_cs.as_deref()
            } else {
                pre.glob_tokens.as_deref()
            };
            if let Some(tokens) = tokens {
                return match_glob_tokens(tokens, target, term.case_sensitive);
            }
        }
        return glob_match(query, target, term.case_sensitive);
    }

    if term.start_with {
        if term.case_sensitive {
            return target.starts_with(query);
        } else if target.len() >= query.len()
            && target.is_char_boundary(query.len())
            && target[..query.len()].eq_ignore_ascii_case(query)
        {
            return true;
        } else if target.is_ascii() && query.is_ascii() {
            return false;
        } else {
            let q_lower = regexes.get_lower(query);
            return target.to_lowercase().starts_with(q_lower.as_ref());
        }
    }

    if term.end_with {
        if term.case_sensitive {
            return target.ends_with(query);
        } else {
            let offset = target.len().saturating_sub(query.len());
            if target.len() >= query.len()
                && target.is_char_boundary(offset)
                && target[offset..].eq_ignore_ascii_case(query)
            {
                return true;
            } else if target.is_ascii() && query.is_ascii() {
                return false;
            } else {
                let q_lower = regexes.get_lower(query);
                return target.to_lowercase().ends_with(q_lower.as_ref());
            }
        }
    }

    if term.whole_words {
        let words = target.split(|c: char| !c.is_alphanumeric());
        if term.case_sensitive {
            return words.into_iter().any(|w| w == query);
        } else if query.is_ascii() {
            return words.into_iter().any(|w| w.eq_ignore_ascii_case(query));
        } else {
            let q_lower = regexes.get_lower(query);
            return words.into_iter().any(|w| {
                if w.eq_ignore_ascii_case(query) {
                    true
                } else if w.is_ascii() {
                    false
                } else {
                    w.to_lowercase() == q_lower.as_ref()
                }
            });
        }
    }

    if term.prefix && term.suffix {
        let q_lower = regexes.get_lower(query);
        let words = target.split(|c: char| !c.is_alphanumeric());
        return words.into_iter().any(|w| {
            word_starts_with(w, query, q_lower.as_ref(), term.case_sensitive)
                && word_ends_with(w, query, q_lower.as_ref(), term.case_sensitive)
        });
    }

    if term.prefix {
        let q_lower = regexes.get_lower(query);
        let words = target.split(|c: char| !c.is_alphanumeric());
        return words
            .into_iter()
            .any(|w| word_starts_with(w, query, q_lower.as_ref(), term.case_sensitive));
    }

    if term.suffix {
        let q_lower = regexes.get_lower(query);
        let words = target.split(|c: char| !c.is_alphanumeric());
        return words
            .into_iter()
            .any(|w| word_ends_with(w, query, q_lower.as_ref(), term.case_sensitive));
    }

    if term.case_sensitive {
        target.contains(query)
    } else if target.is_ascii() && query.is_ascii() {
        contains_ignore_ascii_case(target, query)
    } else if query.is_ascii() {
        let q_ascii_lower = regexes.get_ascii_lower(query);
        target.to_ascii_lowercase().contains(q_ascii_lower.as_ref())
    } else {
        let q_lower = regexes.get_lower(query);
        target.to_lowercase().contains(q_lower.as_ref())
    }
}

#[inline]
fn contains_ignore_ascii_case(target: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if target.len() < query.len() {
        return false;
    }
    let q_bytes = query.as_bytes();
    let t_bytes = target.as_bytes();
    let first = q_bytes[0].to_ascii_lowercase();
    let query_len = q_bytes.len();
    for i in 0..=(t_bytes.len() - query_len) {
        if t_bytes[i].to_ascii_lowercase() == first
            && t_bytes[i..i + query_len].eq_ignore_ascii_case(q_bytes)
        {
            return true;
        }
    }
    false
}

/// Unicode-aware glob token parsing supporting `*` (no separator), `**` (with separators), and `?` (single char).
pub fn parse_glob_tokens(pattern: &str, case_sensitive: bool) -> Vec<GlobToken> {
    let p_chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < p_chars.len() {
        if p_chars[i] == '*' {
            if i + 1 < p_chars.len() && p_chars[i + 1] == '*' {
                while i < p_chars.len() && p_chars[i] == '*' {
                    i += 1;
                }
                tokens.push(GlobToken::DoubleStar);
            } else {
                tokens.push(GlobToken::Star);
                i += 1;
            }
        } else if p_chars[i] == '?' {
            tokens.push(GlobToken::Question);
            i += 1;
        } else {
            let ch = if case_sensitive {
                p_chars[i]
            } else {
                p_chars[i].to_lowercase().next().unwrap_or(p_chars[i])
            };
            tokens.push(GlobToken::Char(ch));
            i += 1;
        }
    }
    tokens
}

/// Matches precompiled glob tokens against text without reparsing the pattern.
pub fn match_glob_tokens(tokens: &[GlobToken], text: &str, case_sensitive: bool) -> bool {
    let char_count = text.chars().count();
    if char_count <= 256 {
        let mut buf = ['\0'; 256];
        for (i, c) in text.chars().enumerate() {
            buf[i] = c;
        }
        match_glob_recursive(tokens, &buf[..char_count], case_sensitive)
    } else {
        let t_chars: Vec<char> = text.chars().collect();
        match_glob_recursive(tokens, &t_chars, case_sensitive)
    }
}

/// Unicode-aware glob matching supporting `*` (no separator), `**` (with separators), and `?` (single char).
pub fn glob_match(pattern: &str, text: &str, case_sensitive: bool) -> bool {
    let tokens = parse_glob_tokens(pattern, case_sensitive);
    match_glob_tokens(&tokens, text, case_sensitive)
}

fn match_glob_recursive(tokens: &[GlobToken], t_chars: &[char], case_sensitive: bool) -> bool {
    if tokens.is_empty() {
        return t_chars.is_empty();
    }

    match tokens[0] {
        GlobToken::DoubleStar => {
            if match_glob_recursive(&tokens[1..], t_chars, case_sensitive) {
                return true;
            }
            if !t_chars.is_empty() && match_glob_recursive(tokens, &t_chars[1..], case_sensitive) {
                return true;
            }
            false
        }
        GlobToken::Star => {
            if match_glob_recursive(&tokens[1..], t_chars, case_sensitive) {
                return true;
            }
            if !t_chars.is_empty()
                && t_chars[0] != '\\'
                && t_chars[0] != '/'
                && match_glob_recursive(tokens, &t_chars[1..], case_sensitive)
            {
                return true;
            }
            false
        }
        GlobToken::Question => {
            if !t_chars.is_empty() && t_chars[0] != '\\' && t_chars[0] != '/' {
                match_glob_recursive(&tokens[1..], &t_chars[1..], case_sensitive)
            } else {
                false
            }
        }
        GlobToken::Char(expected) => {
            if !t_chars.is_empty() {
                let actual = if case_sensitive {
                    t_chars[0]
                } else {
                    t_chars[0].to_lowercase().next().unwrap_or(t_chars[0])
                };
                if actual == expected {
                    return match_glob_recursive(&tokens[1..], &t_chars[1..], case_sensitive);
                }
            }
            false
        }
    }
}

/// Finds matching highlight byte ranges within `text` for a pattern.
pub fn find_highlights(text: &str, pattern: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    if text.is_empty() || pattern.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let p_len = pattern.len();

    if case_sensitive {
        let finder = Finder::new(pattern.as_bytes());
        let mut curr = 0;
        while let Some(pos) = finder.find(&text.as_bytes()[curr..]) {
            let start = curr + pos;
            let end = start + p_len;
            ranges.push(start..end);
            curr = end;
        }
    } else {
        let text_lower = text.to_ascii_lowercase();
        let pat_lower = pattern.to_ascii_lowercase();
        let finder = Finder::new(pat_lower.as_bytes());
        let mut curr = 0;
        while let Some(pos) = finder.find(&text_lower.as_bytes()[curr..]) {
            let start = curr + pos;
            let end = start + p_len;
            ranges.push(start..end);
            curr = end;
        }
    }

    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matching() {
        assert!(glob_match("*.rs", "main.rs", false));
        assert!(glob_match("cairn_*", "cairn_core", false));
        assert!(!glob_match("*.txt", "main.rs", false));

        // Test Unicode character ? matching
        assert!(glob_match("?", "中", false));
        assert!(glob_match("??", "中国", false));
        assert!(!glob_match("?", "中国", false));
        assert!(glob_match("文?.*", "文件.docx", false));

        // Test * does not cross backslash, ** crosses backslash
        assert!(!glob_match("*.rs", r"C:\dir\main.rs", false));
        assert!(glob_match(r"**\*.rs", r"C:\dir\main.rs", false));
        assert!(glob_match(r"C:\*\main.rs", r"C:\dir\main.rs", false));
    }

    #[test]
    fn test_highlight_ranges() {
        let ranges = find_highlights("Hello World, hello!", "hello", false);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], 0..5);
        assert_eq!(ranges[1], 13..18);
    }

    #[test]
    fn test_start_with_and_end_with_unicode_safety() {
        use crate::syntax::SearchTerm;

        // Query length 1 vs multi-byte characters in target (must not panic)
        let term_start = SearchTerm {
            text: "a".to_string(),
            start_with: true,
            case_sensitive: false,
            ..Default::default()
        };
        assert!(!match_text(
            "中文测试.txt",
            &term_start.text,
            &term_start,
            &RegexPrograms::empty(),
        ));

        let term_start_match = SearchTerm {
            text: "中".to_string(),
            start_with: true,
            case_sensitive: false,
            ..Default::default()
        };
        assert!(match_text(
            "中文测试.txt",
            &term_start_match.text,
            &term_start_match,
            &RegexPrograms::empty()
        ));

        let term_end = SearchTerm {
            text: "a".to_string(),
            end_with: true,
            case_sensitive: false,
            ..Default::default()
        };
        assert!(!match_text(
            "文件.测试",
            &term_end.text,
            &term_end,
            &RegexPrograms::empty(),
        ));

        let term_end_match = SearchTerm {
            text: "试".to_string(),
            end_with: true,
            case_sensitive: false,
            ..Default::default()
        };
        assert!(match_text(
            "文件.测试",
            &term_end_match.text,
            &term_end_match,
            &RegexPrograms::empty()
        ));

        // Unicode case-insensitive matching where characters have different uppercase/lowercase bytes
        let term_cyrillic_start = SearchTerm {
            text: "тест".to_string(),
            start_with: true,
            case_sensitive: false,
            ..Default::default()
        };
        let cyr_prog =
            RegexPrograms::compile(&crate::syntax::QueryExpr::Term(term_cyrillic_start.clone()));
        assert!(match_text(
            "Тест_123.txt",
            &term_cyrillic_start.text,
            &term_cyrillic_start,
            &cyr_prog
        ));

        let term_cyrillic_end = SearchTerm {
            text: "тест".to_string(),
            end_with: true,
            case_sensitive: false,
            ..Default::default()
        };
        let cyr_end_prog =
            RegexPrograms::compile(&crate::syntax::QueryExpr::Term(term_cyrillic_end.clone()));
        assert!(match_text(
            "123_Тест",
            &term_cyrillic_end.text,
            &term_cyrillic_end,
            &cyr_end_prog
        ));

        let term_cyrillic_ww = SearchTerm {
            text: "тест".to_string(),
            whole_words: true,
            case_sensitive: false,
            ..Default::default()
        };
        let cyr_ww_prog =
            RegexPrograms::compile(&crate::syntax::QueryExpr::Term(term_cyrillic_ww.clone()));
        assert!(match_text(
            "Документ Тест 2026",
            &term_cyrillic_ww.text,
            &term_cyrillic_ww,
            &cyr_ww_prog
        ));

        let term_unicode_path = SearchTerm {
            text: "école".to_string(),
            case_sensitive: false,
            ..Default::default()
        };
        let ecole_prog =
            RegexPrograms::compile(&crate::syntax::QueryExpr::Term(term_unicode_path.clone()));
        assert!(match_text(
            r"C:\École\notes.txt",
            &term_unicode_path.text,
            &term_unicode_path,
            &ecole_prog
        ));
    }

    #[test]
    fn test_content_query_does_not_falsely_match_all_files() {
        use crate::syntax::parse_query;

        let mut pool = NamePool::default();
        let (name_offset, name_len, ext_offset) = pool.insert("readme.md");

        let records = vec![FileRecord {
            frn: 100,
            parent_idx: FileRecord::ROOT_PARENT,
            name_offset,
            name_len,
            ext_offset,
            volume_id: 2, // 'C'
            flags: 0,
            size: 1024,
            modified_ts: 1000,
            created_ts: 1000,
        }];

        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        // 1. Pure content query (content:架构) - MUST NOT match all files (CNT-06)
        let q_content = parse_query("content:架构");
        let matches = execute_query(&records, &pool, &path_cache, 'C', &q_content, &cancel);
        assert_eq!(
            matches.len(),
            0,
            "content: query must not match files when content search is not evaluated"
        );

        // 2. Combined extension and content query (ext:md content:架构)
        let q_combined = parse_query("ext:md content:架构");
        let matches_combined =
            execute_query(&records, &pool, &path_cache, 'C', &q_combined, &cancel);
        assert_eq!(
            matches_combined.len(),
            0,
            "ext:md content:架构 must not falsely match readme.md without content hit"
        );

        // 3. Normal metadata queries should still match correctly
        let q_ext = parse_query("ext:md");
        let matches_ext = execute_query(&records, &pool, &path_cache, 'C', &q_ext, &cancel);
        assert_eq!(matches_ext.len(), 1);

        let q_name = parse_query("readme");
        let matches_name = execute_query(&records, &pool, &path_cache, 'C', &q_name, &cancel);
        assert_eq!(matches_name.len(), 1);
    }

    #[test]
    fn test_prefix_and_suffix_word_boundary_matching() {
        use crate::syntax::parse_query;

        let mut pool = NamePool::default();
        let (n1, l1, e1) = pool.insert("cairn-core.rs");
        let (n2, l2, e2) = pool.insert("中文_测试.txt");

        let records = vec![
            FileRecord {
                frn: 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: n1,
                name_len: l1,
                ext_offset: e1,
                volume_id: 2,
                flags: 0,
                size: 100,
                modified_ts: 1000,
                created_ts: 1000,
            },
            FileRecord {
                frn: 2,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: n2,
                name_len: l2,
                ext_offset: e2,
                volume_id: 2,
                flags: 0,
                size: 100,
                modified_ts: 1000,
                created_ts: 1000,
            },
        ];

        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        // 1. prefix: matches start of any word bounded by non-alphanumeric chars
        let q_pref1 = parse_query("prefix:cair");
        let m1 = execute_query(&records, &pool, &path_cache, 'C', &q_pref1, &cancel);
        assert_eq!(m1, vec![0]);

        let q_pref2 = parse_query("prefix:cor");
        let m2 = execute_query(&records, &pool, &path_cache, 'C', &q_pref2, &cancel);
        assert_eq!(m2, vec![0]);

        // Middle of word should NOT match prefix:
        let q_pref_mid = parse_query("prefix:air");
        let m_mid = execute_query(&records, &pool, &path_cache, 'C', &q_pref_mid, &cancel);
        assert_eq!(m_mid.len(), 0);

        // 2. suffix: matches end of any word
        let q_suff1 = parse_query("suffix:rn");
        let s1 = execute_query(&records, &pool, &path_cache, 'C', &q_suff1, &cancel);
        assert_eq!(s1, vec![0]);

        let q_suff2 = parse_query("suffix:ore");
        let s2 = execute_query(&records, &pool, &path_cache, 'C', &q_suff2, &cancel);
        assert_eq!(s2, vec![0]);

        // Middle of word should NOT match suffix:
        let q_suff_mid = parse_query("suffix:ai");
        let s_mid = execute_query(&records, &pool, &path_cache, 'C', &q_suff_mid, &cancel);
        assert_eq!(s_mid.len(), 0);

        // 3. Unicode Chinese word boundaries
        let q_zh_pref = parse_query("prefix:测");
        let m_zh = execute_query(&records, &pool, &path_cache, 'C', &q_zh_pref, &cancel);
        assert_eq!(m_zh, vec![1]);

        let q_zh_suff = parse_query("suffix:试");
        let s_zh = execute_query(&records, &pool, &path_cache, 'C', &q_zh_suff, &cancel);
        assert_eq!(s_zh, vec![1]);
    }

    /// A record set of `count` names built the way the index stores them.
    fn records_of(names: &[&'static str]) -> (NamePool, Vec<FileRecord>) {
        let mut pool = NamePool::with_capacity(4096);
        let mut records = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            let (name_offset, name_len, ext_offset) = pool.insert(name);
            records.push(FileRecord {
                frn: i as u64 + 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset,
                name_len,
                ext_offset,
                volume_id: 2,
                flags: 0,
                size: 10,
                modified_ts: 1000,
                created_ts: 1000,
            });
        }
        (pool, records)
    }

    /// A regex query compiles each distinct pattern once for the whole execution, and every
    /// record reads that compiled program: neither count may grow with the records.
    #[test]
    fn test_regex_programs_compile_once_per_query() {
        use crate::syntax::parse_query;

        const MATCHES: usize = 20_000;

        // One pattern: one program for the whole execution.
        let single = parse_query("regex:^report");
        let programs = RegexPrograms::compile(&single);
        assert_eq!(programs.compiles(), 1);

        // Identical patterns share a program; distinct ones each get their own.
        let repeated = RegexPrograms::compile(&parse_query("regex:log|regex:log regex:log"));
        assert_eq!(repeated.compiles(), 1, "identical patterns compile once");
        let distinct = RegexPrograms::compile(&parse_query("regex:rep.* regex:vi.*"));
        assert_eq!(
            distinct.compiles(),
            2,
            "distinct patterns each compile once"
        );

        // Every match attempt reads that one program: `match_text` has no other way to get a
        // regex, which is what keeps the old per-record `Regex::new` from coming back.
        let term = SearchTerm {
            text: "^report".to_string(),
            is_regex: true,
            ..Default::default()
        };
        for _ in 0..MATCHES {
            assert!(match_text("report.log", &term.text, &term, &programs));
        }
        assert_eq!(
            programs.lookups(),
            MATCHES,
            "every record must be matched against the cached program"
        );
        assert_eq!(
            programs.compiles(),
            1,
            "matching {MATCHES} records must not compile anything more"
        );

        // The same program serves a real execution end to end.
        let names: Vec<&'static str> = vec!["report.log"; MATCHES];
        let (pool, records) = records_of(&names);
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();
        let hits = execute_query(&records, &pool, &path_cache, 'C', &single, &cancel);
        assert_eq!(
            hits.len(),
            MATCHES,
            "the pattern must still match every record"
        );
    }

    /// An unparsable pattern matches nothing, exactly as the per-record compilation did.
    #[test]
    fn test_invalid_regex_matches_nothing() {
        use crate::syntax::parse_query;

        let (pool, records) = records_of(&["report.log", "notes.txt"]);
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        let query = parse_query("regex:[");
        let hits = execute_query(&records, &pool, &path_cache, 'C', &query, &cancel);
        assert!(
            hits.is_empty(),
            "an invalid pattern must not match: {hits:?}"
        );
    }

    /// The query-wide defaults are executed, not just parsed: hidden and system records drop
    /// out of the result set, and with the defaults off the result set is unchanged.
    #[test]
    fn test_attribute_defaults_filter_records() {
        use crate::syntax::{parse_query_with, QueryDefaults};

        let names = ["plain.txt", "secret.txt", "boot.ini", "windows.ini"];
        let (pool, mut records) = records_of(&names);
        records[1].flags |= crate::model::FLAG_HIDDEN;
        records[2].flags |= crate::model::FLAG_SYSTEM;
        records[3].flags |= crate::model::FLAG_HIDDEN | crate::model::FLAG_SYSTEM;
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        let run = |query: &str, defaults: &QueryDefaults| {
            let expr = parse_query_with(query, defaults);
            execute_query(&records, &pool, &path_cache, 'C', &expr, &cancel)
        };

        // Off by default: nothing is filtered, so the existing result set cannot shrink.
        let off = QueryDefaults::default();
        assert_eq!(
            run("", &off),
            vec![0, 1, 2, 3],
            "defaults off must not narrow"
        );
        assert_eq!(run("txt", &off), vec![0, 1], "defaults off must not narrow");

        let hidden = QueryDefaults {
            exclude_hidden: true,
            ..Default::default()
        };
        assert_eq!(run("txt", &hidden), vec![0], "hidden records must drop out");
        // A textless query still narrows, which is what makes the toggle usable at all.
        assert_eq!(
            run("", &hidden),
            vec![0, 2],
            "hidden records must drop out of an empty query"
        );

        let system = QueryDefaults {
            exclude_system: true,
            ..Default::default()
        };
        assert_eq!(run("", &system), vec![0, 1], "system records must drop out");

        let both = QueryDefaults {
            exclude_hidden: true,
            exclude_system: true,
            ..Default::default()
        };
        assert_eq!(run("", &both), vec![0], "only the plain record survives");

        // The constraint is ANDed with the items, never negated with them: `!plain` keeps
        // its complement semantics but still refuses hidden records.
        assert_eq!(
            run("!plain", &hidden),
            vec![2],
            "negation must not flip the attribute filter open"
        );

        // A record whose attributes were never recorded (flags 0) is never treated as hidden.
        assert!(!records[0].is_hidden() && !records[0].is_system());
    }

    /// Search term text representations (lowercase, folded) are precomputed once per query
    /// and reused across every record in match_text / evaluate_term without per-record heap allocations.
    #[test]
    fn test_query_texts_precompute_once_per_query() {
        use crate::syntax::parse_query;

        const MATCHES: usize = 20_000;

        let query = parse_query("path:report prefix:sub");
        let programs = RegexPrograms::compile(&query);
        assert_eq!(programs.term_compiles(), 2);

        let repeated = parse_query("report report report");
        let rep_programs = RegexPrograms::compile(&repeated);
        assert_eq!(
            rep_programs.term_compiles(),
            1,
            "identical search term texts compile once"
        );

        let term = SearchTerm {
            text: "report".to_string(),
            prefix: true,
            ..Default::default()
        };
        for _ in 0..MATCHES {
            assert!(match_text("report_file.txt", &term.text, &term, &programs));
        }
        assert_eq!(
            programs.term_lookups(),
            MATCHES,
            "every record must read the precomputed text from the program context"
        );
        assert_eq!(
            programs.term_compiles(),
            2,
            "matching {MATCHES} records must not compile any new terms"
        );
    }
}

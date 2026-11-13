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
use std::ops::Range;
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

/// Resolves whether a content term's full-text hits include one record, keyed by the record's
/// index in the evaluated table. The in-memory engine has no content index of its own, so the
/// caller that owns it supplies the membership test; the term string is the content query text.
pub type ContentHitResolver<'a> = &'a (dyn Fn(&str, u32) -> bool + Sync);

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
    execute_query_inner(
        records,
        arena,
        path_cache,
        drive_letter,
        query,
        order,
        None,
        cancel,
    )
}

/// Evaluates `query` with `content:` terms resolved through `content_hit`. The boolean structure
/// (And/Or/Not) applies uniformly to metadata and content terms.
pub fn execute_query_with_content(
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    query: &QueryExpr,
    content_hit: ContentHitResolver<'_>,
    cancel: &CancellationToken,
) -> Vec<u32> {
    execute_query_inner(
        records,
        arena,
        path_cache,
        drive_letter,
        query,
        None,
        Some(content_hit),
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_query_inner(
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    query: &QueryExpr,
    order: Option<&[u32]>,
    content_hit: Option<ContentHitResolver<'_>>,
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

    let compiled = CompiledExpr::compile(query);
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

            if evaluate_compiled_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                &compiled,
                content_hit,
            ) {
                if matched.is_empty() {
                    matched.reserve(64);
                }
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

            if evaluate_compiled_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                &compiled,
                content_hit,
            ) {
                if matched.is_empty() {
                    matched.reserve(64);
                }
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

#[derive(Clone, Debug)]
enum AsciiGlobFast {
    Suffix {
        bytes: Vec<u8>,
        forbid_sep: bool,
    },
    Prefix {
        bytes: Vec<u8>,
        forbid_sep: bool,
    },
    Affix {
        prefix: Vec<u8>,
        suffix: Vec<u8>,
    },
    Contains {
        finder: Box<Finder<'static>>,
        forbid_sep: bool,
    },
    Exact {
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
struct CompiledGlob {
    tokens: Vec<GlobToken>,
    fast: Option<AsciiGlobFast>,
}

impl CompiledGlob {
    fn compile(pattern: &str, case_sensitive: bool) -> Self {
        let tokens = parse_glob_tokens(pattern, case_sensitive);
        let fast = if pattern.is_ascii() {
            Self::build_fast_kind(pattern, &tokens, case_sensitive)
        } else {
            None
        };
        Self { tokens, fast }
    }

    fn tokens_to_ascii_bytes(
        pattern_bytes: &[u8],
        tokens: &[GlobToken],
        case_sensitive: bool,
    ) -> Option<Vec<u8>> {
        if case_sensitive {
            let mut out = Vec::with_capacity(tokens.len());
            for tok in tokens {
                match *tok {
                    GlobToken::Char(c) if c.is_ascii() => out.push(c as u8),
                    _ => return None,
                }
            }
            let _ = pattern_bytes;
            Some(out)
        } else {
            let mut out = Vec::with_capacity(tokens.len());
            for tok in tokens {
                match *tok {
                    GlobToken::Char(c) if c.is_ascii() => out.push((c as u8).to_ascii_lowercase()),
                    _ => return None,
                }
            }
            Some(out)
        }
    }

    fn build_fast_kind(
        pattern: &str,
        tokens: &[GlobToken],
        case_sensitive: bool,
    ) -> Option<AsciiGlobFast> {
        let p_bytes = pattern.as_bytes();
        if tokens.iter().any(|t| matches!(t, GlobToken::Question)) {
            return None;
        }
        if tokens.iter().all(|t| matches!(t, GlobToken::Char(_))) {
            let bytes = Self::tokens_to_ascii_bytes(p_bytes, tokens, case_sensitive)?;
            return Some(AsciiGlobFast::Exact { bytes });
        }
        if tokens.len() >= 2 && matches!(tokens[0], GlobToken::Star | GlobToken::DoubleStar) {
            let rest = &tokens[1..];
            if rest.iter().all(|t| matches!(t, GlobToken::Char(_))) {
                let bytes = Self::tokens_to_ascii_bytes(p_bytes, rest, case_sensitive)?;
                return Some(AsciiGlobFast::Suffix {
                    bytes,
                    forbid_sep: tokens[0] == GlobToken::Star,
                });
            }
        }
        if tokens.len() >= 2
            && matches!(
                tokens[tokens.len() - 1],
                GlobToken::Star | GlobToken::DoubleStar
            )
        {
            let head = &tokens[..tokens.len() - 1];
            if head.iter().all(|t| matches!(t, GlobToken::Char(_))) {
                let bytes = Self::tokens_to_ascii_bytes(p_bytes, head, case_sensitive)?;
                return Some(AsciiGlobFast::Prefix {
                    bytes,
                    forbid_sep: tokens[tokens.len() - 1] == GlobToken::Star,
                });
            }
        }
        if tokens.len() >= 3 {
            let first = tokens[0];
            let last = tokens[tokens.len() - 1];
            if (first == GlobToken::Star && last == GlobToken::Star)
                || (first == GlobToken::DoubleStar && last == GlobToken::DoubleStar)
            {
                let mid = &tokens[1..tokens.len() - 1];
                if mid.iter().all(|t| matches!(t, GlobToken::Char(_))) {
                    let bytes = Self::tokens_to_ascii_bytes(p_bytes, mid, case_sensitive)?;
                    if memchr::memchr2(b'\\', b'/', &bytes).is_none() {
                        return Some(AsciiGlobFast::Contains {
                            finder: Box::new(Finder::new(&bytes).into_owned()),
                            forbid_sep: first == GlobToken::Star,
                        });
                    }
                }
            }
            if let Some(star_pos) = tokens.iter().position(|&t| t == GlobToken::Star) {
                if star_pos > 0
                    && star_pos + 1 < tokens.len()
                    && tokens[..star_pos]
                        .iter()
                        .all(|t| matches!(t, GlobToken::Char(_)))
                    && tokens[star_pos + 1..]
                        .iter()
                        .all(|t| matches!(t, GlobToken::Char(_)))
                {
                    let prefix =
                        Self::tokens_to_ascii_bytes(p_bytes, &tokens[..star_pos], case_sensitive)?;
                    let suffix = Self::tokens_to_ascii_bytes(
                        p_bytes,
                        &tokens[star_pos + 1..],
                        case_sensitive,
                    )?;
                    return Some(AsciiGlobFast::Affix { prefix, suffix });
                }
            }
        }
        None
    }

    #[inline]
    fn matches(&self, target: &str, folded_bytes: Option<&[u8]>, case_sensitive: bool) -> bool {
        if let Some(fast) = &self.fast {
            let t_bytes = target.as_bytes();
            match fast {
                AsciiGlobFast::Exact { bytes } => {
                    if target.len() == bytes.len() && t_bytes.is_ascii() {
                        return match folded_bytes {
                            Some(folded) if !case_sensitive => folded == bytes.as_slice(),
                            _ if case_sensitive => t_bytes == bytes.as_slice(),
                            _ => t_bytes.eq_ignore_ascii_case(bytes),
                        };
                    }
                }
                AsciiGlobFast::Suffix { bytes, forbid_sep } => {
                    let lit_len = bytes.len();
                    if target.len() < lit_len {
                        return false;
                    }
                    let split = target.len() - lit_len;
                    let tail = &t_bytes[split..];
                    if tail.is_ascii() {
                        let suffix_ok = match folded_bytes {
                            Some(folded) if !case_sensitive => &folded[split..] == bytes.as_slice(),
                            _ if case_sensitive => tail == bytes.as_slice(),
                            _ => tail.eq_ignore_ascii_case(bytes),
                        };
                        if !suffix_ok {
                            return false;
                        }
                        return !*forbid_sep
                            || memchr::memchr2(b'\\', b'/', &t_bytes[..split]).is_none();
                    }
                }
                AsciiGlobFast::Prefix { bytes, forbid_sep } => {
                    let lit_len = bytes.len();
                    if target.len() < lit_len {
                        return false;
                    }
                    let head = &t_bytes[..lit_len];
                    if head.is_ascii() {
                        let prefix_ok = match folded_bytes {
                            Some(folded) if !case_sensitive => {
                                &folded[..lit_len] == bytes.as_slice()
                            }
                            _ if case_sensitive => head == bytes.as_slice(),
                            _ => head.eq_ignore_ascii_case(bytes),
                        };
                        if !prefix_ok {
                            return false;
                        }
                        return !*forbid_sep
                            || memchr::memchr2(b'\\', b'/', &t_bytes[lit_len..]).is_none();
                    }
                }
                AsciiGlobFast::Affix { prefix, suffix } => {
                    let p_len = prefix.len();
                    let s_len = suffix.len();
                    if target.len() < p_len + s_len {
                        return false;
                    }
                    let head = &t_bytes[..p_len];
                    let split = target.len() - s_len;
                    let tail = &t_bytes[split..];
                    if head.is_ascii() && tail.is_ascii() {
                        let ends_ok = match folded_bytes {
                            Some(folded) if !case_sensitive => {
                                &folded[..p_len] == prefix.as_slice()
                                    && &folded[split..] == suffix.as_slice()
                            }
                            _ if case_sensitive => {
                                head == prefix.as_slice() && tail == suffix.as_slice()
                            }
                            _ => {
                                head.eq_ignore_ascii_case(prefix)
                                    && tail.eq_ignore_ascii_case(suffix)
                            }
                        };
                        if !ends_ok {
                            return false;
                        }
                        return memchr::memchr2(b'\\', b'/', &t_bytes[p_len..split]).is_none();
                    }
                }
                AsciiGlobFast::Contains { finder, forbid_sep } => {
                    if *forbid_sep && memchr::memchr2(b'\\', b'/', t_bytes).is_some() {
                        return false;
                    }
                    if t_bytes.is_ascii() {
                        return match folded_bytes {
                            Some(folded) if !case_sensitive => finder.find(folded).is_some(),
                            _ if case_sensitive => finder.find(t_bytes).is_some(),
                            _ => {
                                if t_bytes.len() <= 256 {
                                    let mut lower_buf = [0u8; 256];
                                    for (dst, &src) in lower_buf.iter_mut().zip(t_bytes.iter()) {
                                        *dst = src.to_ascii_lowercase();
                                    }
                                    finder.find(&lower_buf[..t_bytes.len()]).is_some()
                                } else {
                                    let lower: Vec<u8> =
                                        t_bytes.iter().map(u8::to_ascii_lowercase).collect();
                                    finder.find(&lower).is_some()
                                }
                            }
                        };
                    }
                }
            }
        }
        match_glob_tokens_nfa(&self.tokens, target, case_sensitive)
    }
}

#[derive(Clone, Debug)]
struct CompiledTermPlan {
    term: SearchTerm,
    ext_has_glob: bool,
    ascii_lower_bytes: Option<Vec<u8>>,
    lower: String,
    finder: Finder<'static>,
    regex: Option<Regex>,
    glob: Option<CompiledGlob>,
}

impl CompiledTermPlan {
    fn compile(term: &SearchTerm) -> Self {
        let ext_has_glob = term
            .ext_filter
            .iter()
            .any(|pattern| pattern.contains('*') || pattern.contains('?'));
        let regex = if term.is_regex {
            Regex::new(&term.text).ok()
        } else {
            None
        };
        let glob = if term.whole_filename && !term.is_regex {
            Some(CompiledGlob::compile(&term.text, term.case_sensitive))
        } else {
            None
        };
        let ascii_lower_bytes = if term.text.is_ascii() {
            Some(
                term.text
                    .as_bytes()
                    .iter()
                    .map(u8::to_ascii_lowercase)
                    .collect(),
            )
        } else {
            None
        };
        let lower = term.text.to_lowercase();
        let folded = NamePool::fold_query_bytes(&term.text);
        let finder = Finder::new(&folded).into_owned();
        Self {
            term: term.clone(),
            ext_has_glob,
            ascii_lower_bytes,
            lower,
            finder,
            regex,
            glob,
        }
    }
}

#[derive(Clone, Debug)]
enum CompiledExpr {
    Term(Box<CompiledTermPlan>),
    And(Vec<CompiledExpr>),
    Or(Vec<CompiledExpr>),
    Not(Box<CompiledExpr>),
}

impl CompiledExpr {
    fn compile(query: &QueryExpr) -> Self {
        match query {
            QueryExpr::Term(term) => Self::Term(Box::new(CompiledTermPlan::compile(term))),
            QueryExpr::And(terms) => Self::And(terms.iter().map(Self::compile).collect()),
            QueryExpr::Or(terms) => Self::Or(terms.iter().map(Self::compile).collect()),
            QueryExpr::Not(inner) => Self::Not(Box::new(Self::compile(inner))),
        }
    }
}

/// Precompiled query execution state ready to evaluate individual records without reallocations.
pub struct CompiledQuery {
    plan: CompiledExpr,
}

impl CompiledQuery {
    pub fn compile(query: &QueryExpr) -> Self {
        Self {
            plan: CompiledExpr::compile(query),
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
    ) -> bool {
        evaluate_compiled_expr(
            rec,
            idx,
            records,
            arena,
            path_cache,
            drive_letter,
            &self.plan,
            None,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_compiled_expr(
    rec: &FileRecord,
    idx: u32,
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    expr: &CompiledExpr,
    content_hit: Option<ContentHitResolver<'_>>,
) -> bool {
    match expr {
        CompiledExpr::Term(plan) => evaluate_compiled_term(
            rec,
            idx,
            records,
            arena,
            path_cache,
            drive_letter,
            plan,
            content_hit,
        ),
        CompiledExpr::And(terms) => terms.iter().all(|sub| {
            evaluate_compiled_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                sub,
                content_hit,
            )
        }),
        CompiledExpr::Or(terms) => terms.iter().any(|sub| {
            evaluate_compiled_expr(
                rec,
                idx,
                records,
                arena,
                path_cache,
                drive_letter,
                sub,
                content_hit,
            )
        }),
        CompiledExpr::Not(inner) => !evaluate_compiled_expr(
            rec,
            idx,
            records,
            arena,
            path_cache,
            drive_letter,
            inner,
            content_hit,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_compiled_term(
    rec: &FileRecord,
    idx: u32,
    records: &[FileRecord],
    arena: &NamePool,
    path_cache: &DirectoryPathCache,
    drive_letter: char,
    plan: &CompiledTermPlan,
    content_hit: Option<ContentHitResolver<'_>>,
) -> bool {
    let term = &plan.term;

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
        let matched = if !plan.ext_has_glob {
            term.ext_filter
                .iter()
                .any(|pattern| ext.eq_ignore_ascii_case(pattern))
        } else {
            term.ext_filter.iter().any(|pattern| {
                if pattern.contains('*') || pattern.contains('?') {
                    glob_match(pattern, ext, false)
                } else {
                    ext.eq_ignore_ascii_case(pattern)
                }
            })
        };
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
    // The MFT in-memory engine only matches file metadata and paths. When a term requires
    // content matching, only the caller's hit resolver can answer it; without one the term must
    // never falsely match all files.
    if let Some(ref content_query) = term.content_query {
        return content_hit.is_some_and(|hit| hit(content_query, idx));
    }

    // 8. Text match
    if term.text.is_empty() {
        return true;
    }

    if term.match_path {
        let full_path = path_cache.get_full_path(records, arena, drive_letter, idx);
        match_compiled_text(&full_path, None, plan)
    } else {
        let name = arena.get_name(rec.name_offset, rec.name_len);
        let folded_name = arena.get_folded(rec.name_offset, rec.name_len);
        if term.is_regex
            || term.whole_filename
            || term.case_sensitive
            || term.whole_words
            || term.prefix
            || term.suffix
            || term.start_with
            || term.end_with
        {
            match_compiled_text(name, Some(folded_name), plan)
        } else {
            plan.finder.find(folded_name).is_some()
        }
    }
}

#[inline]
fn starts_with_ignore_case(target: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    // Greek final sigma (U+03A3) is the only context-sensitive lowercase mapping in Rust stdlib.
    if target.contains('\u{03A3}') {
        return target.to_lowercase().starts_with(query_lower);
    }
    let mut target_chars = target.chars().flat_map(char::to_lowercase);
    for q in query_lower.chars() {
        if target_chars.next() != Some(q) {
            return false;
        }
    }
    true
}

#[inline]
fn ends_with_ignore_case(target: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    if target.contains('\u{03A3}') {
        return target.to_lowercase().ends_with(query_lower);
    }
    let mut q_rev = query_lower.chars().rev();
    for ch in target.chars().rev() {
        let mut folded = ['\0'; 3];
        let mut len = 0usize;
        for lc in ch.to_lowercase() {
            folded[len] = lc;
            len += 1;
        }
        for i in (0..len).rev() {
            match q_rev.next() {
                Some(q) if q == folded[i] => {}
                Some(_) => return false,
                None => return true,
            }
        }
    }
    q_rev.next().is_none()
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
    } else if query.is_ascii()
        && word.len() >= query.len()
        && word.as_bytes()[..query.len()].is_ascii()
    {
        word.as_bytes()[..query.len()].eq_ignore_ascii_case(query.as_bytes())
    } else {
        starts_with_ignore_case(word, query_lower)
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
    } else if query.is_ascii() && word.len() >= query.len() {
        let offset = word.len() - query.len();
        if word.as_bytes()[offset..].is_ascii() {
            word.as_bytes()[offset..].eq_ignore_ascii_case(query.as_bytes())
        } else {
            ends_with_ignore_case(word, query_lower)
        }
    } else {
        ends_with_ignore_case(word, query_lower)
    }
}

fn match_compiled_text(target: &str, folded_bytes: Option<&[u8]>, plan: &CompiledTermPlan) -> bool {
    let term = &plan.term;
    let query = term.text.as_str();

    if term.is_regex {
        return plan.regex.as_ref().is_some_and(|re| re.is_match(target));
    }

    if let Some(glob) = &plan.glob {
        return glob.matches(target, folded_bytes, term.case_sensitive);
    }

    if term.start_with {
        if term.case_sensitive {
            return target.starts_with(query);
        }
        if let Some(q_ascii) = &plan.ascii_lower_bytes {
            if target.len() < q_ascii.len() {
                return false;
            }
            let head = &target.as_bytes()[..q_ascii.len()];
            if head.is_ascii() {
                return match folded_bytes {
                    Some(folded) => &folded[..q_ascii.len()] == q_ascii.as_slice(),
                    None => head.eq_ignore_ascii_case(q_ascii),
                };
            }
        }
        return starts_with_ignore_case(target, &plan.lower);
    }

    if term.end_with {
        if term.case_sensitive {
            return target.ends_with(query);
        }
        if let Some(q_ascii) = &plan.ascii_lower_bytes {
            if target.len() < q_ascii.len() {
                return false;
            }
            let offset = target.len() - q_ascii.len();
            let tail = &target.as_bytes()[offset..];
            if tail.is_ascii() {
                return match folded_bytes {
                    Some(folded) => &folded[offset..] == q_ascii.as_slice(),
                    None => tail.eq_ignore_ascii_case(q_ascii),
                };
            }
        }
        return ends_with_ignore_case(target, &plan.lower);
    }

    if term.whole_words {
        let words = target.split(|c: char| !c.is_alphanumeric());
        if term.case_sensitive {
            return words.into_iter().any(|w| w == query);
        } else if query.is_ascii() {
            return words.into_iter().any(|w| w.eq_ignore_ascii_case(query));
        } else {
            return words.into_iter().any(|w| {
                if w.eq_ignore_ascii_case(query) {
                    true
                } else if w.is_ascii() {
                    false
                } else {
                    w.to_lowercase() == plan.lower
                }
            });
        }
    }

    if term.prefix && term.suffix {
        let words = target.split(|c: char| !c.is_alphanumeric());
        return words.into_iter().any(|w| {
            word_starts_with(w, query, &plan.lower, term.case_sensitive)
                && word_ends_with(w, query, &plan.lower, term.case_sensitive)
        });
    }

    if term.prefix {
        let words = target.split(|c: char| !c.is_alphanumeric());
        return words
            .into_iter()
            .any(|w| word_starts_with(w, query, &plan.lower, term.case_sensitive));
    }

    if term.suffix {
        let words = target.split(|c: char| !c.is_alphanumeric());
        return words
            .into_iter()
            .any(|w| word_ends_with(w, query, &plan.lower, term.case_sensitive));
    }

    if term.case_sensitive {
        target.contains(query)
    } else if plan.ascii_lower_bytes.is_some() {
        let t_bytes = target.as_bytes();
        if t_bytes.len() < query.len() {
            false
        } else if t_bytes.len() <= 256 {
            let mut lower_buf = [0u8; 256];
            for (dst, &src) in lower_buf.iter_mut().zip(t_bytes.iter()) {
                *dst = src.to_ascii_lowercase();
            }
            plan.finder.find(&lower_buf[..t_bytes.len()]).is_some()
        } else {
            let lower: Vec<u8> = t_bytes.iter().map(u8::to_ascii_lowercase).collect();
            plan.finder.find(&lower).is_some()
        }
    } else {
        target.to_lowercase().contains(&plan.lower)
    }
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
    match_glob_tokens_nfa(tokens, text, case_sensitive)
}

/// Unicode-aware glob matching supporting `*` (no separator), `**` (with separators), and `?` (single char).
pub fn glob_match(pattern: &str, text: &str, case_sensitive: bool) -> bool {
    let tokens = parse_glob_tokens(pattern, case_sensitive);
    match_glob_tokens(&tokens, text, case_sensitive)
}

fn match_glob_tokens_nfa(tokens: &[GlobToken], text: &str, case_sensitive: bool) -> bool {
    let state_count = tokens.len() + 1;
    if state_count <= 65 {
        let mut curr = [false; 65];
        let mut next = [false; 65];
        run_glob_nfa(
            tokens,
            text,
            case_sensitive,
            &mut curr[..state_count],
            &mut next[..state_count],
        )
    } else {
        let mut curr = vec![false; state_count];
        let mut next = vec![false; state_count];
        run_glob_nfa(tokens, text, case_sensitive, &mut curr, &mut next)
    }
}

fn run_glob_nfa(
    tokens: &[GlobToken],
    text: &str,
    case_sensitive: bool,
    curr: &mut [bool],
    next: &mut [bool],
) -> bool {
    let end = tokens.len();
    curr[0] = true;
    for pos in 0..end {
        if curr[pos] && matches!(tokens[pos], GlobToken::Star | GlobToken::DoubleStar) {
            curr[pos + 1] = true;
        }
    }

    for ch in text.chars() {
        let is_sep = ch == '\\' || ch == '/';
        let norm = if case_sensitive {
            ch
        } else {
            ch.to_lowercase().next().unwrap_or(ch)
        };
        let mut any_active = false;
        for pos in 0..end {
            if !curr[pos] {
                continue;
            }
            match tokens[pos] {
                GlobToken::DoubleStar => {
                    next[pos] = true;
                    any_active = true;
                }
                GlobToken::Star => {
                    if !is_sep {
                        next[pos] = true;
                        any_active = true;
                    }
                }
                GlobToken::Question => {
                    if !is_sep {
                        next[pos + 1] = true;
                        any_active = true;
                    }
                }
                GlobToken::Char(expected) => {
                    if norm == expected {
                        next[pos + 1] = true;
                        any_active = true;
                    }
                }
            }
        }
        if !any_active {
            return false;
        }
        for pos in 0..end {
            if next[pos] && matches!(tokens[pos], GlobToken::Star | GlobToken::DoubleStar) {
                next[pos + 1] = true;
            }
        }
        curr.copy_from_slice(next);
        next.fill(false);
    }

    curr[end]
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
    fn glob_matching_stays_linear_for_star_chains() {
        // Alternating star/literal segments backtrack exponentially without the visited-state
        // memo; both the failing and the matching case must stay fast and correct.
        let pattern = "*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b";
        let mut text = "a".repeat(400);
        assert!(!glob_match(pattern, &text, false));
        text.push('b');
        assert!(glob_match(pattern, &text, false));
    }

    #[test]
    fn test_start_with_and_end_with_unicode_safety() {
        use crate::syntax::SearchTerm;

        let check = |target: &str, term: SearchTerm| {
            let plan = CompiledTermPlan::compile(&term);
            match_compiled_text(target, None, &plan)
        };

        // Query length 1 vs multi-byte characters in target (must not panic)
        assert!(!check(
            "中文测试.txt",
            SearchTerm {
                text: "a".to_string(),
                start_with: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        assert!(check(
            "中文测试.txt",
            SearchTerm {
                text: "中".to_string(),
                start_with: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        assert!(!check(
            "文件.测试",
            SearchTerm {
                text: "a".to_string(),
                end_with: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        assert!(check(
            "文件.测试",
            SearchTerm {
                text: "试".to_string(),
                end_with: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        // Unicode case-insensitive matching where characters have different uppercase/lowercase bytes
        assert!(check(
            "Тест_123.txt",
            SearchTerm {
                text: "тест".to_string(),
                start_with: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        assert!(check(
            "123_Тест",
            SearchTerm {
                text: "тест".to_string(),
                end_with: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        assert!(check(
            "Документ Тест 2026",
            SearchTerm {
                text: "тест".to_string(),
                whole_words: true,
                case_sensitive: false,
                ..Default::default()
            },
        ));

        assert!(check(
            r"C:\École\notes.txt",
            SearchTerm {
                text: "école".to_string(),
                case_sensitive: false,
                ..Default::default()
            },
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
    fn content_terms_follow_boolean_negation_through_a_hit_resolver() {
        use crate::syntax::parse_query;

        let mut pool = NamePool::default();
        let (note_off, note_len, note_ext) = pool.insert("note.txt");
        let (report_off, report_len, report_ext) = pool.insert("report.txt");

        let records = vec![
            FileRecord {
                frn: 101,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: note_off,
                name_len: note_len,
                ext_offset: note_ext,
                volume_id: 2,
                flags: 0,
                size: 1,
                modified_ts: 1,
                created_ts: 1,
            },
            FileRecord {
                frn: 102,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset: report_off,
                name_len: report_len,
                ext_offset: report_ext,
                volume_id: 2,
                flags: 0,
                size: 1,
                modified_ts: 1,
                created_ts: 1,
            },
        ];
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        // Only record 0 carries the content hit for `needle`; the resolver stands in for the
        // service's per-record hit membership.
        let content_hits = |term: &str, idx| term == "needle" && idx == 0;

        let query = parse_query("!content:needle");
        let matched = execute_query_with_content(
            &records,
            &pool,
            &path_cache,
            'C',
            &query,
            &content_hits,
            &cancel,
        );
        assert_eq!(
            matched,
            vec![1],
            "a negated content term must exclude exactly its hits"
        );
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

    #[test]
    fn compiled_query_matches_execute_query_for_regex_and_repeated_terms() {
        use crate::syntax::parse_query;

        let mut names = vec!["report.log"; 2_000];
        names.push("vision_report.txt");
        names.push("notes.md");
        let (pool, records) = records_of(&names);
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        for q in [
            "regex:^report",
            "regex:log|regex:log regex:log",
            "regex:rep.* regex:vi.*",
        ] {
            let expr = parse_query(q);
            let compiled = CompiledQuery::compile(&expr);
            let full = execute_query(&records, &pool, &path_cache, 'C', &expr, &cancel);
            let via_compiled: Vec<u32> = records
                .iter()
                .enumerate()
                .filter_map(|(i, rec)| {
                    compiled
                        .evaluate_record(rec, i as u32, &records, &pool, &path_cache, 'C')
                        .then_some(i as u32)
                })
                .collect();
            assert_eq!(via_compiled, full, "mismatch for compiled query {q}");
        }
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

    #[test]
    fn compiled_query_handles_path_and_prefix_terms_without_divergence() {
        use crate::syntax::parse_query;

        let names = ["sub_report_file.txt", "report.log", "other.txt"];
        let (pool, records) = records_of(&names);
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        for q in ["path:report prefix:sub", "report report report"] {
            let expr = parse_query(q);
            let compiled = CompiledQuery::compile(&expr);
            let full = execute_query(&records, &pool, &path_cache, 'C', &expr, &cancel);
            let via_compiled: Vec<u32> = records
                .iter()
                .enumerate()
                .filter_map(|(i, rec)| {
                    compiled
                        .evaluate_record(rec, i as u32, &records, &pool, &path_cache, 'C')
                        .then_some(i as u32)
                })
                .collect();
            assert_eq!(via_compiled, full, "mismatch for compiled query {q}");
        }
    }

    #[test]
    fn glob_and_affix_queries_match_unicode_separators_and_case_edges() {
        use crate::syntax::parse_query;

        let names = [
            "main.rs",
            "MAIN.RS",
            "中文模块.rs",
            r"sub\fake.rs",
            "cairn_core.rs",
            "my_cairn_lib.txt",
            "build_step.log",
            "报告.LOG",
            "\u{212A}mid\u{212A}",
            "main.rİ",
            "main.rK",
            "Exact.TXT",
            "dir",
            "nested.rs",
        ];
        let (pool, mut records) = records_of(&names);
        records[0].size = 2 * 1024 * 1024;
        records[4].size = 3 * 1024 * 1024;
        records[5].flags |= crate::model::FLAG_TOMBSTONE;
        records[12].flags |= crate::model::FLAG_DIRECTORY;
        records[13].parent_idx = 12;
        let path_cache = DirectoryPathCache::new();
        let cancel = CancellationToken::new();

        let run = |q: &str| {
            let expr = parse_query(q);
            execute_query(&records, &pool, &path_cache, 'C', &expr, &cancel)
        };

        assert_eq!(run("*.rs"), vec![0, 1, 2, 4, 13]);
        assert_eq!(run("**.rs"), vec![0, 1, 2, 3, 4, 13]);
        assert_eq!(run(r"**\dir\*.rs"), vec![13]);
        assert_eq!(run(r"**\*.rs"), vec![0, 1, 2, 3, 4, 13]);
        assert_eq!(run("case:*.RS"), vec![1]);
        assert_eq!(run("cairn_*"), vec![4]);
        assert_eq!(run("cairn_*.rs"), vec![4]);
        assert_eq!(run("*cairn*"), vec![4]);
        assert_eq!(run("$:log"), vec![6, 7]);
        assert_eq!(run("^:build"), vec![6]);
        assert_eq!(run("$:k"), vec![8, 10]);
        assert_eq!(run("^:k"), vec![8]);
        assert_eq!(run("*.ri"), vec![9]);
        assert_eq!(run("*.rk"), vec![10]);
        assert_eq!(run("wfn:exact.txt"), vec![11]);
        assert_eq!(run("size:>1mb"), vec![0, 4]);
    }
}

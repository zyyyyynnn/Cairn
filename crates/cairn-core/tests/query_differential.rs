// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Differential test of the query engine against a naive reference implementation.
//!
//! The reference derives its expectation from the **raw query string** with plain string
//! operations, and deliberately does not use `parse_search_term`, `glob_match` or
//! `SizeFilter`. `parse_query` appears only on the production side, because `execute_query`
//! takes an AST - the reference never reads its output. If both sides shared the lexer,
//! any lexing defect would cancel out on both sides (for example, `size:>1mb` truncating to
//! `And([])` on both sides and reporting zero differences across all 10,000 rounds).
//! See `differential_shares_no_lexer_with_the_engine` below, which pins that invariant.
//! (independently of `parse_query`).
//!
//! Reference grammar modelled (SPEC QRY-02): literal text items matched as case-insensitive
//! substrings of the name, AND by whitespace, OR by `|` with spaces around it ignored, `!`
//! negation, `<...>` grouping, `"..."` literal items, and the precedence rule "OR binds
//! tighter than AND" (`a b|c` == `a <b|c>`).
//!
//! Deliberately NOT generated, because the reference would have to reimplement production
//! value parsers to judge them - they are covered by exact-count end-to-end tests in
//! `tests/query_filters.rs` instead:
//! - functions (`ext:` `size:` `dm:` `dc:` `date-modified:` `date-created:` `parent:`
//!   `content:`) and the built-in macros (`audio:` `doc:` `exe:` `image:` `video:` `zip:`);
//! - modifiers (`case:` `ww:` `wfn:` `path:` `file:` `folder:` `prefix:` `regex:` ...);
//! - wildcards `*` `?` `**`, drive letters, and `\` path matching (SPEC QRY-02, QRY-03);
//! - regex (SPEC QRY-03, Rust `regex` engine);
//! - empty operands such as `""` or a dangling `!`, whose chosen semantics (unsatisfiable,
//!   not "match all") is an explicit product decision pinned in `tests/query_filters.rs`.

use cairn_core::arena::NamePool;
use cairn_core::engine::{execute_query, CancellationToken};
use cairn_core::model::{FileRecord, FLAG_DIRECTORY};
use cairn_core::path_cache::DirectoryPathCache;
use cairn_core::syntax::parse_query;

/// `(name, is_directory)`; names overlap on purpose (`apple` inside `pineapple.txt`,
/// `orange` inside `orangeade.rs`) so substring mistakes are visible.
const FILES: &[(&str, bool)] = &[
    ("apple.txt", false),
    ("apple_pie.txt", false),
    ("pineapple.txt", false),
    ("banana.txt", false),
    ("banana_split.md", false),
    ("orange.txt", false),
    ("orangeade.rs", false),
    ("apple banana.txt", false),
    ("Grape Juice.log", false),
    ("Docs", true),
];

struct Index {
    pool: NamePool,
    records: Vec<FileRecord>,
    path_cache: DirectoryPathCache,
    cancel: CancellationToken,
}

impl Index {
    fn new() -> Self {
        let mut pool = NamePool::with_capacity(4096);
        let mut records = Vec::with_capacity(FILES.len());
        for (i, (name, is_dir)) in FILES.iter().enumerate() {
            let (name_offset, name_len, ext_offset) = pool.insert(name);
            records.push(FileRecord {
                frn: i as u64 + 1,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset,
                name_len,
                ext_offset,
                volume_id: 0,
                flags: if *is_dir { FLAG_DIRECTORY } else { 0 },
                size: (i as u64 + 1) * 1024,
                modified_ts: 1_700_000_000,
                created_ts: 1_700_000_000,
            });
        }
        Self {
            pool,
            records,
            path_cache: DirectoryPathCache::new(),
            cancel: CancellationToken::new(),
        }
    }

    /// Production answer for a raw query: names of the matching records.
    fn matched_names(&self, query: &str) -> Vec<String> {
        let expr = parse_query(query);
        execute_query(
            &self.records,
            &self.pool,
            &self.path_cache,
            'C',
            &expr,
            &self.cancel,
        )
        .into_iter()
        .map(|idx| FILES[idx as usize].0.to_string())
        .collect()
    }
}

// ---------------------------------------------------------------------------
// Naive reference: raw string in, boolean out. No production parser involved.
// ---------------------------------------------------------------------------

/// SPEC QRY-02: whitespace separates AND operands, so the query matches when *every* item
/// matches. An empty query has no items, which the product resolves to "every record".
fn reference_matches(name: &str, query: &str) -> bool {
    let items = split_items(query);
    items.iter().all(|item| reference_item(name, item))
}

/// One item is a disjunction of its top-level `|` separated operands.
fn reference_item(name: &str, item: &str) -> bool {
    split_or(item)
        .iter()
        .any(|operand| reference_operand(name, operand.trim()))
}

/// A single operand: optional `!`, optional `<group>` (whose content is a whole expression),
/// otherwise a quoted or bare literal matched as a case-insensitive substring.
fn reference_operand(name: &str, operand: &str) -> bool {
    if operand.is_empty() {
        // Never generated; an absent operand constrains nothing and matches nothing.
        return false;
    }
    if let Some(rest) = operand.strip_prefix('!') {
        return !reference_operand(name, rest.trim());
    }
    if let Some(rest) = operand.strip_prefix('<') {
        return match angle_close_byte_index(rest) {
            Some(idx) => {
                let inner = &rest[..idx];
                let trailing = rest[idx + '>'.len_utf8()..].trim();
                let grouped = reference_matches(name, inner);
                if trailing.is_empty() {
                    grouped
                } else {
                    // A group glued to more text behaves as an additional AND operand.
                    grouped && reference_item(name, trailing)
                }
            }
            // Unterminated group: treat everything after `<` as the group content.
            None => reference_matches(name, rest),
        };
    }
    let literal = strip_quotes(operand);
    name.to_lowercase().contains(&literal.to_lowercase())
}

/// Splits on whitespace that is not inside quotes or an angle group. Whitespace next to a `|`
/// is not a separator, because SPEC QRY-02 ignores spaces around the OR operator.
fn split_items(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' && !quoted {
            quoted = true;
            current.push(ch);
        } else if ch == '"' && quoted {
            quoted = false;
            current.push(ch);
        } else if !quoted && ch == '<' {
            depth += 1;
            current.push(ch);
        } else if !quoted && ch == '>' && depth > 0 {
            depth -= 1;
            current.push(ch);
        } else if !quoted && depth == 0 && ch.is_whitespace() {
            let before_is_pipe = current.ends_with('|') || current.ends_with("| ");
            let after_is_pipe = chars[i + 1..]
                .iter()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| *c == '|');
            if before_is_pipe || after_is_pipe {
                current.push(' ');
            } else if !current.trim().is_empty() {
                items.push(current.trim().to_string());
                current.clear();
            }
        } else {
            current.push(ch);
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_string());
    }
    items
}

/// Splits one item on its top-level `|` characters.
fn split_or(item: &str) -> Vec<String> {
    let chars: Vec<char> = item.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut quoted = false;

    for &ch in &chars {
        if ch == '"' {
            quoted = !quoted;
            current.push(ch);
        } else if !quoted && ch == '<' {
            depth += 1;
            current.push(ch);
        } else if !quoted && ch == '>' && depth > 0 {
            depth -= 1;
            current.push(ch);
        } else if !quoted && depth == 0 && ch == '|' {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    parts
}

/// Byte index of the `>` that closes a group whose `<` was already consumed, or `None` when
/// the group is never closed.
fn angle_close_byte_index(rest: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut quoted = false;
    for (i, ch) in rest.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        } else if !quoted && ch == '<' {
            depth += 1;
        } else if !quoted && ch == '>' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Query generator
// ---------------------------------------------------------------------------

/// xorshift64: deterministic, dependency-free, and independent of any production RNG use.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<'a, T>(&mut self, options: &'a [T]) -> &'a T {
        &options[(self.next() % options.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "apple", "banana", "orange", "pie", "juice", "grape", "docs", "split",
];

/// Literal words only: no `:`, `\`, `*`, `?` or quotes inside, which keeps every generated
/// item inside the grammar the naive reference models (see the module comment).
fn gen_word(rng: &mut Rng) -> String {
    rng.pick(WORDS).to_string()
}

fn gen_atom(rng: &mut Rng, depth: usize) -> String {
    match rng.next() % if depth == 0 { 4 } else { 6 } {
        0 => gen_word(rng),
        1 => format!("!{}", gen_word(rng)),
        2 => format!("\"{} {}\"", gen_word(rng), gen_word(rng)),
        3 => format!("\"{}\"", gen_word(rng)),
        4 => format!("<{}>", gen_expr(rng, depth - 1)),
        _ => format!("!<{}>", gen_expr(rng, depth - 1)),
    }
}

/// One item: one to three operands joined by `|`, glued or spaced.
fn gen_item(rng: &mut Rng, depth: usize) -> String {
    let mut item = gen_atom(rng, depth);
    let extra = (rng.next() % 3) as usize; // 0..2 further operands
    for _ in 0..extra {
        if rng.next().is_multiple_of(2) {
            item.push('|');
        } else {
            item.push_str(" | ");
        }
        item.push_str(&gen_atom(rng, depth));
    }
    item
}

/// An expression: one to three items joined by a single space (AND).
fn gen_expr(rng: &mut Rng, depth: usize) -> String {
    let count = 1 + (rng.next() % 3) as usize;
    let mut parts = Vec::with_capacity(count);
    for _ in 0..count {
        parts.push(gen_item(rng, depth));
    }
    parts.join(" ")
}

#[test]
fn precedence_rule_has_expected_hit_counts() {
    let index = Index::new();
    let total = FILES.len();

    // SPEC QRY-02: "默认或先于与" - OR binds tighter than AND, the opposite of the usual
    // convention. `apple banana|orange` therefore means `apple <banana|orange>`.
    for query in ["apple banana|orange", "apple <banana|orange>"] {
        assert_eq!(
            index.matched_names(query),
            vec!["apple banana.txt".to_string()],
            "{query}: only a name containing apple AND (banana or orange)"
        );
    }

    // The explicitly grouped alternative must differ, otherwise the precedence assertion
    // above would pass for a parser that ignored operator order entirely: here the AND of
    // apple and banana becomes one operand of the OR, so every orange name matches too.
    assert_eq!(
        index.matched_names("<apple banana>|orange"),
        vec![
            "orange.txt".to_string(),
            "orangeade.rs".to_string(),
            "apple banana.txt".to_string(),
        ],
        "<apple banana>|orange matches (apple AND banana) OR orange"
    );

    // Negation and grouping counts, hand-derived from the fixture.
    assert_eq!(index.matched_names("apple|orange").len(), 6);
    assert_eq!(index.matched_names("!apple orange").len(), 2);
    assert_eq!(index.matched_names("<apple|pie> !banana").len(), 3);
    assert_eq!(index.matched_names("\"grape juice\"").len(), 1);
    assert_eq!(index.matched_names("pie").len(), 1);
    assert_eq!(index.matched_names("docs").len(), 1);
    assert_eq!(index.matched_names("apple|orange docs").len(), 0);
    assert_eq!(index.matched_names("").len(), total);
    assert_eq!(index.matched_names("  ").len(), total);
}

#[test]
fn differential_against_independent_reference() {
    let index = Index::new();
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);

    let mut true_outcomes = 0usize;
    let mut false_outcomes = 0usize;
    let mut distinct_queries = 0usize;
    let mut seen = std::collections::HashSet::new();

    for round in 0..10_000usize {
        let query = gen_expr(&mut rng, 2);
        let production = index.matched_names(&query);
        let reference = reference_names(&query);

        if seen.insert(query.clone()) {
            distinct_queries += 1;
        }

        assert_eq!(
            production, reference,
            "differential mismatch on round {round}: query {query:?}\n  \
             production: {production:?}\n  reference:    {reference:?}"
        );

        true_outcomes += reference.len();
        false_outcomes += FILES.len() - reference.len();
    }

    // Anti-vacuity: a reference that answered "true" (or "false") everywhere would agree with
    // a broken engine, so the mix of outcomes is asserted explicitly.
    let pairs = true_outcomes + false_outcomes;
    assert_eq!(pairs, 10_000 * FILES.len());
    assert!(
        true_outcomes * 5 >= pairs && false_outcomes * 5 >= pairs,
        "generated grammar is not discriminating: {true_outcomes} matches vs {false_outcomes} \
         non-matches out of {pairs} (query, file) pairs"
    );
    assert!(
        distinct_queries > 5_000,
        "generator produced only {distinct_queries} distinct queries, so the differential \
         coverage is far smaller than the round count suggests"
    );
}

/// Ensures the reference implementation shares no lexer with the production engine.
///
/// A bare `>` was a token terminator outside any `<...>` group, so `size:>1mb` lexed to the
/// item `size:`, which parsed to a term without constraints and dropped out of the AND chain,
/// leaving `And([])` - "no query" - which the engine resolved to every record. A raw-string
/// reference that does not share the AST exposes two separate invariants (literal items with `>`
/// and numeric-function items):
/// - literal items containing a bare `>`: with the truncating lexer the engine matched every
///   name containing `apple` while the reference matched nothing, so a lexer-only regression
///   is caught here even though the unsatisfiable-item rule hides it from the numeric cases;
/// - numeric-function items: neither side may answer "the whole library".
#[test]
fn differential_shares_no_lexer_with_the_engine() {
    let index = Index::new();
    let total = FILES.len();

    for query in ["apple>x", "banana>|orange", "!grape>z", "<pie>x docs"] {
        assert_eq!(
            index.matched_names(query),
            reference_names(query),
            "{query:?}: a bare `>` must stay inside the search item"
        );
    }

    for query in ["size:>1mb", "size:>=1mb", "dm:>2026", "dc:>2026"] {
        let production = index.matched_names(query);
        // Neither side may read these as "no search item at all".
        assert!(
            production.len() < total,
            "{query:?} degraded to the whole library ({total} records): {production:?}"
        );
        // On this fixture the numeric filters legitimately match nothing, which is exactly
        // what the independent reference also concludes.
        assert_eq!(
            production,
            reference_names(query),
            "{query:?} disagrees with the reference"
        );
    }
}

/// The reference answer for a raw query, as file names.
fn reference_names(query: &str) -> Vec<String> {
    FILES
        .iter()
        .filter(|(name, _)| reference_matches(name, query))
        .map(|(name, _)| (*name).to_string())
        .collect()
}

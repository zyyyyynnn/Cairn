// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Everything search syntax AST and recursive-descent parser.
//!
//! Enforces:
//! - Default OR (`|`) has higher precedence than AND (spaces).
//! - Explicit grouping `<...>`
//! - Quoted strings `"..."`
//! - Negation `!`
//! - Modifiers and functions

use crate::filter::{DateFilter, SizeFilter};
use serde::{Deserialize, Serialize};

/// Query-wide defaults: what a search item means when the item itself does not say.
///
/// The UI match toggles (case, whole word, path, regex, content) and the hidden/system
/// filters are properties of the whole query, so they travel as this structure instead of
/// being spliced into the query text. A modifier written in the text (`case:`, `no-case:`,
/// `ww:`, `path:`, `no-path:`, `regex:`, `content:`) states its own value for that item and
/// wins over the default; a default only ever widens or narrows items that state nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryDefaults {
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub match_path: bool,
    pub is_regex: bool,
    /// Route each item's text through content search instead of name/path matching.
    pub content: bool,
    pub exclude_hidden: bool,
    pub exclude_system: bool,
}

impl QueryDefaults {
    fn attribute_filter(&self) -> Option<SearchTerm> {
        if self.exclude_hidden || self.exclude_system {
            Some(SearchTerm {
                exclude_hidden: self.exclude_hidden,
                exclude_system: self.exclude_system,
                ..Default::default()
            })
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueryExpr {
    Term(SearchTerm),
    And(Vec<QueryExpr>),
    Or(Vec<QueryExpr>),
    Not(Box<QueryExpr>),
}

impl QueryExpr {
    pub fn is_empty(&self) -> bool {
        match self {
            QueryExpr::Term(t) => t.is_empty(),
            QueryExpr::And(v) | QueryExpr::Or(v) => v.is_empty(),
            QueryExpr::Not(n) => n.is_empty(),
        }
    }

    /// Collects search terms that should be highlighted in search results (QRY-08).
    pub fn collect_highlight_terms(&self) -> Vec<String> {
        let mut terms = Vec::new();
        self.collect_terms_inner(&mut terms);
        terms.sort();
        terms.dedup();
        terms
    }

    fn collect_terms_inner(&self, out: &mut Vec<String>) {
        match self {
            QueryExpr::Term(t) => {
                let clean = t.text.trim().trim_matches('*').trim_matches('?');
                if !clean.is_empty() {
                    out.push(clean.to_string());
                }
            }
            QueryExpr::And(subs) | QueryExpr::Or(subs) => {
                for s in subs {
                    s.collect_terms_inner(out);
                }
            }
            QueryExpr::Not(_) => {
                // Do not highlight terms from negated expressions
            }
        }
    }

    /// Collects the patterns of every regex item, so the executor compiles each distinct
    /// pattern once per query instead of once per record (SPEC QRY-03).
    pub fn collect_regex_patterns(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_regex_patterns_inner(&mut out);
        out.sort();
        out.dedup();
        out
    }

    fn collect_regex_patterns_inner<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            QueryExpr::Term(t) => {
                if t.is_regex && !t.text.is_empty() {
                    out.push(&t.text);
                }
            }
            QueryExpr::And(subs) | QueryExpr::Or(subs) => {
                for s in subs {
                    s.collect_regex_patterns_inner(out);
                }
            }
            QueryExpr::Not(inner) => inner.collect_regex_patterns_inner(out),
        }
    }

    /// Collects the texts of every search term, so the executor compiles each distinct
    /// text pattern (lowercase, folded) once per query instead of per record.
    pub fn collect_search_texts(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_search_texts_inner(&mut out);
        out.sort();
        out.dedup();
        out
    }

    fn collect_search_texts_inner<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            QueryExpr::Term(t) => {
                if !t.text.is_empty() {
                    out.push(&t.text);
                }
            }
            QueryExpr::And(subs) | QueryExpr::Or(subs) => {
                for s in subs {
                    s.collect_search_texts_inner(out);
                }
            }
            QueryExpr::Not(inner) => inner.collect_search_texts_inner(out),
        }
    }

    /// Extracts all full-text content search queries defined by `content:` terms (CNT-06).
    pub fn extract_content_queries(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.extract_content_queries_inner(&mut out);
        out.sort();
        out.dedup();
        out
    }

    fn extract_content_queries_inner(&self, out: &mut Vec<String>) {
        match self {
            QueryExpr::Term(t) => {
                if let Some(ref cq) = t.content_query {
                    let trimmed = cq.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_string());
                    }
                }
            }
            QueryExpr::And(subs) | QueryExpr::Or(subs) => {
                for s in subs {
                    s.extract_content_queries_inner(out);
                }
            }
            QueryExpr::Not(inner) => {
                inner.extract_content_queries_inner(out);
            }
        }
    }

    /// Strips out `content:` constraints from this expression to evaluate MFT metadata/path filters.
    /// Returns `None` if the expression only contained content search constraints.
    pub fn strip_content_queries(&self) -> Option<QueryExpr> {
        match self {
            QueryExpr::Term(t) => {
                if t.content_query.is_some() {
                    let mut stripped = t.clone();
                    stripped.content_query = None;
                    if stripped.is_empty() {
                        None
                    } else {
                        Some(QueryExpr::Term(stripped))
                    }
                } else {
                    Some(QueryExpr::Term(t.clone()))
                }
            }
            QueryExpr::And(subs) => {
                let filtered: Vec<QueryExpr> = subs
                    .iter()
                    .filter_map(|s| s.strip_content_queries())
                    .collect();
                if filtered.is_empty() {
                    None
                } else if filtered.len() == 1 {
                    Some(filtered.into_iter().next().unwrap())
                } else {
                    Some(QueryExpr::And(filtered))
                }
            }
            QueryExpr::Or(subs) => {
                let filtered: Vec<QueryExpr> = subs
                    .iter()
                    .filter_map(|s| s.strip_content_queries())
                    .collect();
                if filtered.is_empty() {
                    None
                } else if filtered.len() == 1 {
                    Some(filtered.into_iter().next().unwrap())
                } else {
                    Some(QueryExpr::Or(filtered))
                }
            }
            QueryExpr::Not(inner) => inner
                .strip_content_queries()
                .map(|stripped| QueryExpr::Not(Box::new(stripped))),
        }
    }
}

/// One search item of a [`QueryExpr`]: its text plus the modifiers that apply to that text.
///
/// The modifier booleans may come from the item's own token (`case:foo`) or from the query's
/// [`QueryDefaults`]; see [`parse_query_with`].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SearchTerm {
    pub raw: String,
    pub text: String,
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub whole_filename: bool,
    pub match_path: bool,
    pub is_regex: bool,
    pub file_only: bool,
    pub folder_only: bool,
    pub prefix: bool,
    pub suffix: bool,
    pub start_with: bool,
    pub end_with: bool,
    pub drive: Option<char>,
    pub ext_filter: Vec<String>,
    pub size_filter: Option<SizeFilter>,
    pub dm_filter: Option<DateFilter>,
    pub dc_filter: Option<DateFilter>,
    pub parent_filter: Option<String>,
    pub content_query: Option<String>,
    /// Rejects records carrying `FLAG_HIDDEN` (query-wide default, never a text modifier).
    pub exclude_hidden: bool,
    /// Rejects records carrying `FLAG_SYSTEM` (query-wide default, never a text modifier).
    pub exclude_system: bool,
    /// Marks a search item whose function value was empty or unparsable: `size:` with no
    /// value, `size:notanumber`, `ext:` without any usable extension, or a dangling operand.
    ///
    /// Such an item matches nothing instead of degrading to "no constraint", which would
    /// silently widen the whole query to every record. Option-only items (`case:`, `file:`)
    /// are deliberately not covered: they are not parse failures.
    pub match_nothing: bool,
}

impl SearchTerm {
    /// Builds an item that matches no record. Used for search items that cannot be parsed
    /// into any constraint, so a malformed item can never match the whole index.
    pub fn match_nothing() -> Self {
        Self {
            match_nothing: true,
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.match_nothing
            && self.text.is_empty()
            && self.drive.is_none()
            && self.ext_filter.is_empty()
            && self.size_filter.is_none()
            && self.dm_filter.is_none()
            && self.dc_filter.is_none()
            && self.parent_filter.is_none()
            && self.content_query.is_none()
            && !self.file_only
            && !self.folder_only
            && !self.exclude_hidden
            && !self.exclude_system
    }
}

/// Parses an Everything search query into an AST `QueryExpr`, with no query-wide defaults.
///
/// Equivalent to [`parse_query_with`] and an all-false [`QueryDefaults`]: every item means
/// exactly what its own token says.
pub fn parse_query(input: &str) -> QueryExpr {
    parse_query_with(input, &QueryDefaults::default())
}

/// Parses an Everything search query into an AST `QueryExpr`, applying `defaults` to every
/// search item that does not state the corresponding modifier itself.
///
/// A completely empty input yields an empty expression, which the engine resolves to
/// "every record". A non-empty input never degrades to that: an item that carries no
/// parsable constraint becomes [`SearchTerm::match_nothing`], so the query matches nothing.
/// An `exclude_hidden` / `exclude_system` default adds a constraint item to the top-level
/// AND, so even a textless query becomes a narrowing one.
pub fn parse_query_with(input: &str, defaults: &QueryDefaults) -> QueryExpr {
    let trimmed = input.trim();
    let expr = if trimmed.is_empty() {
        QueryExpr::And(Vec::new())
    } else {
        let mut parser = QueryParser::new(trimmed, defaults);
        parser.parse_expression()
    };

    match defaults.attribute_filter() {
        None => expr,
        Some(filter) => match expr {
            QueryExpr::And(subs) if subs.is_empty() => {
                QueryExpr::And(vec![QueryExpr::Term(filter)])
            }
            QueryExpr::And(mut subs) => {
                subs.insert(0, QueryExpr::Term(filter));
                QueryExpr::And(subs)
            }
            other => QueryExpr::And(vec![QueryExpr::Term(filter), other]),
        },
    }
}

struct QueryParser<'a> {
    chars: Vec<char>,
    pos: usize,
    /// Number of `<...>` groups the parser is currently inside. A bare `>` is only a
    /// terminator while a group is open; outside one it belongs to the search item
    /// (SPEC QRY-04 numeric operators, e.g. `size:>10mb`).
    group_depth: usize,
    defaults: &'a QueryDefaults,
    _marker: std::marker::PhantomData<&'a str>,
}

impl<'a> QueryParser<'a> {
    fn new(input: &str, defaults: &'a QueryDefaults) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            group_depth: 0,
            defaults,
            _marker: std::marker::PhantomData,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Whether the parser sits on the `>` that closes the innermost open `<...>` group.
    ///
    /// Both the expression loop and the lexer ask this, so the two can never disagree:
    /// if one treated a bare `>` as a terminator while the other did not, the expression
    /// loop would stop consuming and spin forever on that character.
    fn at_group_end(&self) -> bool {
        self.group_depth > 0 && self.peek() == Some('>')
    }

    /// Expression is a list of space-separated OR sequences (AND has lower precedence than OR).
    fn parse_expression(&mut self) -> QueryExpr {
        let mut and_terms = Vec::new();

        self.skip_whitespace();
        while self.pos < self.chars.len() {
            let or_seq = self.parse_or_sequence();
            if !or_seq.is_empty() {
                and_terms.push(or_seq);
            }

            self.skip_whitespace();
            // A closing group bracket ends the expression, but only while a group is open.
            if self.at_group_end() {
                break;
            }
        }

        if and_terms.len() == 1 {
            and_terms.pop().unwrap()
        } else {
            QueryExpr::And(and_terms)
        }
    }

    /// OR sequence has higher precedence: `a|b|c`
    fn parse_or_sequence(&mut self) -> QueryExpr {
        let mut or_terms = Vec::new();

        let first = self.parse_unary();
        or_terms.push(first);

        loop {
            // Check if next non-space character is '|'
            let mut temp_pos = self.pos;
            while temp_pos < self.chars.len() && self.chars[temp_pos].is_whitespace() {
                temp_pos += 1;
            }

            if temp_pos < self.chars.len() && self.chars[temp_pos] == '|' {
                self.pos = temp_pos + 1; // consume '|'
                                         // skip spaces after '|'
                self.skip_whitespace();
                let next_term = self.parse_unary();
                or_terms.push(next_term);
            } else {
                break;
            }
        }

        if or_terms.len() == 1 {
            or_terms.pop().unwrap()
        } else {
            QueryExpr::Or(or_terms)
        }
    }

    /// Unary: `!term` or `<grouped>` or normal term
    fn parse_unary(&mut self) -> QueryExpr {
        self.skip_whitespace();
        if self.peek() == Some('!') {
            self.next(); // consume '!'
            self.skip_whitespace();
            let inner = self.parse_primary();
            QueryExpr::Not(Box::new(inner))
        } else {
            self.parse_primary()
        }
    }

    /// Primary: `<group>` or single token (possibly quoted)
    fn parse_primary(&mut self) -> QueryExpr {
        self.skip_whitespace();
        if self.peek() == Some('<') {
            self.next(); // consume '<'
            self.group_depth += 1;
            let expr = self.parse_expression();
            if self.peek() == Some('>') {
                self.next(); // consume '>'
            }
            self.group_depth -= 1;
            expr
        } else {
            let token = self.read_token();
            if token.is_empty() {
                // Dangling operator (`a|`, `a||b`, `!<>`): the item carries no constraint,
                // so it must not act as "match everything" inside an OR/AND chain.
                QueryExpr::Term(SearchTerm::match_nothing())
            } else {
                QueryExpr::Term(parse_search_term(&token, self.defaults))
            }
        }
    }

    /// Reads a single token until whitespace, `|`, or - only while a `<...>` group is open - `>`.
    ///
    /// Double quotes and in-token angle bracket groups (e.g. `content:<abc 123>`) shield their
    /// terminators. A bare `>` outside any group is ordinary token text, because it is a numeric
    /// comparison operator in functions such as `size:>10mb` (SPEC QRY-04).
    fn read_token(&mut self) -> String {
        let mut token = String::new();
        let mut in_quotes = false;
        let mut angle_depth = 0;

        while let Some(ch) = self.peek() {
            if ch == '"' {
                in_quotes = !in_quotes;
                token.push(ch);
                self.pos += 1;
            } else if !in_quotes && ch == '<' {
                angle_depth += 1;
                token.push(ch);
                self.pos += 1;
            } else if !in_quotes && ch == '>' && angle_depth > 0 {
                angle_depth -= 1;
                token.push(ch);
                self.pos += 1;
            } else if !in_quotes
                && angle_depth == 0
                && (ch.is_whitespace() || ch == '|' || self.at_group_end())
            {
                break;
            } else {
                token.push(ch);
                self.pos += 1;
            }
        }

        token
    }
}

/// Which modifiers a token stated out loud. Those keep whatever value the token gave them;
/// [`QueryDefaults`] only reach the items that stated nothing about a modifier.
#[derive(Default)]
struct StatedModifiers {
    case: bool,
    whole_words: bool,
    path: bool,
    regex: bool,
}

/// Parses modifiers, functions, and text out of a single token.
///
/// Function tokens (`ext:`, `size:`, `dm:`, `dc:`, `parent:`, `content:`) and drive tokens
/// consume the rest of the token and return early. They carry no search text, so no textual
/// default of `defaults` can reach them.
pub fn parse_search_term(raw_token: &str, defaults: &QueryDefaults) -> SearchTerm {
    let mut term = SearchTerm {
        raw: raw_token.to_string(),
        ..Default::default()
    };
    let mut stated = StatedModifiers::default();

    let mut remaining = raw_token;

    // Check for drive specification, e.g. "c:" or "D:"
    if remaining.len() == 2
        && remaining
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && remaining.ends_with(':')
    {
        term.drive = Some(remaining.chars().next().unwrap().to_ascii_uppercase());
        return term;
    }

    // Process modifiers / functions in a loop
    loop {
        let mut matched = false;

        if let Some(rest) = remaining.strip_prefix("case:") {
            term.case_sensitive = true;
            stated.case = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("no-case:") {
            term.case_sensitive = false;
            stated.case = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("ww:") {
            term.whole_words = true;
            stated.whole_words = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("whole-words:") {
            term.whole_words = true;
            stated.whole_words = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("no-whole-words:") {
            term.whole_words = false;
            stated.whole_words = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("wfn:") {
            term.whole_filename = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("whole:") {
            term.whole_filename = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("entire:") {
            term.whole_filename = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("\\:") {
            term.match_path = true;
            stated.path = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("path:") {
            term.match_path = true;
            stated.path = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("no-path:") {
            term.match_path = false;
            stated.path = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("regex:") {
            term.is_regex = true;
            stated.regex = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("file:") {
            term.file_only = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("folder:") {
            term.folder_only = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("prefix:") {
            term.prefix = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("suffix:") {
            term.suffix = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("^:") {
            term.start_with = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("start-with:") {
            term.start_with = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("$:") {
            term.end_with = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("end-with:") {
            term.end_with = true;
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("ext:") {
            let val = unquote(rest);
            term.ext_filter = val
                .split(';')
                .map(|s| {
                    s.trim_start_matches("*.")
                        .trim_start_matches('.')
                        .to_ascii_lowercase()
                })
                .filter(|s| !s.is_empty())
                .collect();
            // An `ext:` without any usable extension is a broken filter, not "no filter".
            term.match_nothing = term.ext_filter.is_empty();
            return term;
        } else if let Some(rest) = remaining.strip_prefix("size:") {
            let val = unquote(rest);
            term.size_filter = SizeFilter::parse(val);
            term.match_nothing = term.size_filter.is_none();
            return term;
        } else if let Some(rest) = remaining.strip_prefix("dm:") {
            let val = unquote(rest);
            term.dm_filter = DateFilter::parse(val);
            term.match_nothing = term.dm_filter.is_none();
            return term;
        } else if let Some(rest) = remaining.strip_prefix("date-modified:") {
            let val = unquote(rest);
            term.dm_filter = DateFilter::parse(val);
            term.match_nothing = term.dm_filter.is_none();
            return term;
        } else if let Some(rest) = remaining.strip_prefix("dc:") {
            let val = unquote(rest);
            term.dc_filter = DateFilter::parse(val);
            term.match_nothing = term.dc_filter.is_none();
            return term;
        } else if let Some(rest) = remaining.strip_prefix("date-created:") {
            let val = unquote(rest);
            term.dc_filter = DateFilter::parse(val);
            term.match_nothing = term.dc_filter.is_none();
            return term;
        } else if let Some(rest) = remaining.strip_prefix("parent:") {
            term.parent_filter = Some(unquote(rest).trim_end_matches('\\').to_string());
            return term;
        } else if let Some(rest) = remaining.strip_prefix("content:") {
            term.content_query = Some(unquote(rest).to_string());
            return term;
        } else if let Some(rest) = remaining.strip_prefix("audio:") {
            term.ext_filter = "mp3;wav;flac;aac;ogg;wma;m4a;opus;ape"
                .split(';')
                .map(String::from)
                .collect();
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("doc:") {
            term.ext_filter = "doc;docx;xls;xlsx;ppt;pptx;pdf;txt;md;rtf;csv;odt;ods;odp"
                .split(';')
                .map(String::from)
                .collect();
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("exe:") {
            term.ext_filter = "exe;bat;cmd;msi;ps1;vbs;com;scr"
                .split(';')
                .map(String::from)
                .collect();
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("image:") {
            term.ext_filter = "jpg;jpeg;png;gif;bmp;webp;svg;ico;tiff;tif;psd;raw"
                .split(';')
                .map(String::from)
                .collect();
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("video:") {
            term.ext_filter = "mp4;mkv;avi;mov;wmv;flv;webm;m4v;rmvb"
                .split(';')
                .map(String::from)
                .collect();
            remaining = rest;
            matched = true;
        } else if let Some(rest) = remaining.strip_prefix("zip:") {
            term.ext_filter = "zip;rar;7z;tar;gz;bz2;xz;iso;cab"
                .split(';')
                .map(String::from)
                .collect();
            remaining = rest;
            matched = true;
        }

        if !matched {
            break;
        }
    }

    // Strip surrounding quotes from search text
    let text = unquote(remaining).to_string();

    // Check for drive prefix in text, e.g. "C:\Windows"
    if text.len() >= 3 && text.as_bytes()[1] == b':' && text.as_bytes()[2] == b'\\' {
        term.drive = Some(text.chars().next().unwrap().to_ascii_uppercase());
        term.match_path = true;
    }

    // Wildcards: if contains * or ?, match whole filename
    if text.contains('*') || text.contains('?') {
        term.whole_filename = true;
    }

    // Path rules: contains '\' matches path; ends with '\' matches folder
    if text.contains('\\') {
        term.match_path = true;
    }
    if text.ends_with('\\') && !text.ends_with(":\\") {
        term.folder_only = true;
    }

    term.text = text;

    // Query-wide defaults reach only the modifiers this token did not state itself.
    if defaults.case_sensitive && !stated.case {
        term.case_sensitive = true;
    }
    if defaults.whole_words && !stated.whole_words {
        term.whole_words = true;
    }
    if defaults.match_path && !stated.path {
        term.match_path = true;
    }
    if defaults.is_regex && !stated.regex {
        term.is_regex = true;
    }
    if defaults.content && !term.text.is_empty() {
        term.content_query = Some(std::mem::take(&mut term.text));
    }

    term
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('<') && s.ends_with('>')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{DateOp, SizeOp};

    #[test]
    fn test_or_precedence_higher_than_and() {
        let q = parse_query("apple banana|orange");
        // Must be parsed as: And([apple, Or([banana, orange])])
        match q {
            QueryExpr::And(terms) => {
                assert_eq!(terms.len(), 2);
                match &terms[0] {
                    QueryExpr::Term(t) => assert_eq!(t.text, "apple"),
                    _ => panic!("Expected term apple"),
                }
                match &terms[1] {
                    QueryExpr::Or(or_terms) => {
                        assert_eq!(or_terms.len(), 2);
                    }
                    _ => panic!("Expected Or sequence"),
                }
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_grouping_and_not() {
        let q = parse_query("!<foo bar>");
        match q {
            QueryExpr::Not(inner) => match *inner {
                QueryExpr::And(terms) => {
                    assert_eq!(terms.len(), 2);
                }
                _ => panic!("Expected grouped And"),
            },
            _ => panic!("Expected Not expression"),
        }
    }

    #[test]
    fn test_modifiers_and_functions() {
        let term = bare("case:ext:rs;toml");
        assert!(term.case_sensitive);
        assert_eq!(term.ext_filter, vec!["rs", "toml"]);

        let term2 = bare("path:\"System32\\\"");
        assert!(term2.match_path);
        assert!(term2.folder_only);
    }

    #[test]
    fn test_content_query_extraction_and_stripping() {
        let q = parse_query("ext:md content:架构");
        assert_eq!(q.extract_content_queries(), vec!["架构"]);
        let stripped = q.strip_content_queries().unwrap();
        match stripped {
            QueryExpr::Term(t) => {
                assert_eq!(t.ext_filter, vec!["md"]);
                assert!(t.content_query.is_none());
            }
            _ => panic!("Expected single stripped Term"),
        }

        let q_only = parse_query("content:架构");
        assert_eq!(q_only.extract_content_queries(), vec!["架构"]);
        assert!(q_only.strip_content_queries().is_none());

        let q_angle = parse_query("content:<abc 123>");
        assert_eq!(q_angle.extract_content_queries(), vec!["abc 123"]);
        assert!(q_angle.strip_content_queries().is_none());
    }

    /// SPEC QRY-04: `<` `<=` `=` `>` `>=` `!=` are comparison operators of function values.
    /// A bare `>` must therefore stay inside the search item; only an open `<...>` group
    /// owns it as a terminator.
    #[test]
    fn test_bare_gt_belongs_to_the_search_item() {
        assert_eq!(
            size_op(&parse_query("size:>10mb")),
            Some(SizeOp::Gt(10 << 20))
        );
        assert_eq!(
            size_op(&parse_query("size:>=1mb")),
            Some(SizeOp::Gte(1 << 20))
        );
        assert_eq!(size_op(&parse_query("size:!=1kb")), Some(SizeOp::Neq(1024)));
        assert_eq!(
            size_op(&parse_query("size:<1mb")),
            Some(SizeOp::Lt(1 << 20))
        );
        assert_eq!(size_op(&parse_query("size:<=1kb")), Some(SizeOp::Lte(1024)));
        assert!(
            !parse_query("size:>10mb").is_empty(),
            "a parsed function item is not an empty query"
        );

        let dm = term_of(&parse_query("dm:>2026")).dm_filter.clone();
        assert!(
            matches!(dm, Some(DateFilter { op: DateOp::Gt(_) })),
            "dm:>2026 lost its operator: {dm:?}"
        );
        let dc = term_of(&parse_query("dc:>2026")).dc_filter.clone();
        assert!(
            matches!(dc, Some(DateFilter { op: DateOp::Gt(_) })),
            "dc:>2026 lost its operator: {dc:?}"
        );

        // A comparison item inside an AND chain keeps every sibling.
        match parse_query("size:>1mb ext:md") {
            QueryExpr::And(subs) => {
                assert_eq!(subs.len(), 2);
                assert_eq!(size_op(&subs[0]), Some(SizeOp::Gt(1 << 20)));
                assert_eq!(term_of(&subs[1]).ext_filter, vec!["md".to_string()]);
            }
            other => panic!("Expected And of two terms, got {other:?}"),
        }

        // `>` is ordinary text outside of a group when it is not part of a function value.
        assert_eq!(term_of(&parse_query("a>b")).text, "a>b");
    }

    #[test]
    fn test_gt_still_closes_an_open_group() {
        match parse_query("<foo bar> size:>1mb") {
            QueryExpr::And(ref subs) => {
                assert_eq!(subs.len(), 2);
                match &subs[0] {
                    QueryExpr::And(group) => {
                        assert_eq!(group.len(), 2);
                        assert_eq!(term_of(&group[0]).text, "foo");
                        assert_eq!(term_of(&group[1]).text, "bar");
                    }
                    other => panic!("Expected grouped And, got {other:?}"),
                }
                assert_eq!(size_op(&subs[1]), Some(SizeOp::Gt(1 << 20)));
            }
            other => panic!("Expected And expression, got {other:?}"),
        }

        // Negated group and nested group still terminate on `>`.
        assert!(matches!(
            parse_query("!<foo bar>"),
            QueryExpr::Not(inner) if matches!(&*inner, QueryExpr::And(g) if g.len() == 2)
        ));
        match parse_query("<a <b c> d>") {
            QueryExpr::And(subs) => assert_eq!(subs.len(), 3),
            other => panic!("Expected three top-level items, got {other:?}"),
        }

        // Inside an open group `>` still closes it, so `size:>1mb` cannot survive there.
        // What must hold is that the truncated item is unsatisfiable rather than vacuous:
        // the query then matches nothing instead of silently returning the whole library.
        match parse_query("<size:>1mb foo>") {
            QueryExpr::And(subs) => {
                assert!(!subs.is_empty());
                assert!(
                    matches!(&subs[0], QueryExpr::Term(t) if t.match_nothing),
                    "truncated function item must be unsatisfiable, got {:?}",
                    subs[0]
                );
            }
            other => panic!("Expected And expression, got {other:?}"),
        }
    }

    /// An item that carries no parsable constraint is unsatisfiable: it must never let a
    /// query that does contain a search term fall back to "return everything".
    #[test]
    fn test_items_without_constraints_match_nothing() {
        for broken in [
            "size:",
            "size:notanumber",
            "ext:",
            "ext:.;.",
            "dm:",
            "dc:not-a-date",
        ] {
            let q = parse_query(broken);
            let term = term_of(&q);
            assert!(term.match_nothing, "{broken} must be flagged unsatisfiable");
            assert!(!q.is_empty(), "{broken} must not look like an empty query");
        }

        // A broken sibling cannot erase the rest of the query either.
        match parse_query("foo size:") {
            QueryExpr::And(subs) => {
                assert_eq!(subs.len(), 2);
                assert_eq!(term_of(&subs[0]).text, "foo");
                assert!(term_of(&subs[1]).match_nothing);
            }
            other => panic!("Expected And of two terms, got {other:?}"),
        }

        // Dangling operator: the missing operand is unsatisfiable, not "match all".
        match parse_query("foo|") {
            QueryExpr::Or(subs) => {
                assert_eq!(subs.len(), 2);
                assert!(term_of(&subs[1]).match_nothing);
            }
            other => panic!("Expected Or sequence, got {other:?}"),
        }

        // Completely empty input keeps the documented "no query" representation.
        assert!(parse_query("").is_empty());
        assert!(parse_query("   ").is_empty());

        // Boundary of the chosen semantics: an option-only item is not a parse failure,
        // so `case:` stays a vacuous constraint (Everything-consistent) instead of
        // becoming unsatisfiable. `file:` / `folder:` are non-empty by construction.
        assert!(!bare("case:").match_nothing);
        assert!(!bare("file:").match_nothing);
        assert!(!bare("size:>1mb").match_nothing);
    }

    /// Parses one token with no query-wide defaults, i.e. the way its own text reads.
    fn bare(raw_token: &str) -> SearchTerm {
        parse_search_term(raw_token, &QueryDefaults::default())
    }

    fn term_of(expr: &QueryExpr) -> &SearchTerm {
        match expr {
            QueryExpr::Term(t) => t,
            other => panic!("Expected a term, got {other:?}"),
        }
    }

    /// The items of a flat `And` chain. Panics on any other shape, which is what the tests
    /// below want: a default that leaked into the wrong place shows up as a panic.
    fn and_terms(expr: &QueryExpr) -> Vec<&SearchTerm> {
        match expr {
            QueryExpr::And(subs) => subs.iter().map(term_of).collect(),
            other => panic!("Expected And chain, got {other:?}"),
        }
    }

    /// A written modifier is item-scoped and nothing else (`case:` applies to its own item
    /// only), while query-wide toggles are passed via [`QueryDefaults`].
    /// Verifies that `case:` on the first word does not bleed into the second word.
    #[test]
    fn written_modifiers_stay_item_scoped() {
        let expr = parse_query("case:报告 季度");
        let terms = and_terms(&expr);
        assert_eq!(terms.len(), 2);
        assert!(terms[0].case_sensitive);
        assert_eq!(terms[0].text, "报告");
        assert!(!terms[1].case_sensitive);
        assert_eq!(terms[1].text, "季度");

        let expr = parse_query("path:报告 季度");
        let terms = and_terms(&expr);
        assert!(terms[0].match_path);
        assert!(!terms[1].match_path);
    }

    /// A `QueryDefaults` flag reaches every item of the query, not just the first one.
    #[test]
    fn query_defaults_apply_to_every_item() {
        let case = QueryDefaults {
            case_sensitive: true,
            ..Default::default()
        };
        let expr = parse_query_with("报告 季度", &case);
        let terms = and_terms(&expr);
        assert_eq!(terms.len(), 2);
        assert!(
            terms.iter().all(|t| t.case_sensitive),
            "case default must apply to all items: {terms:?}"
        );
        assert_eq!(terms[0].text, "报告");
        assert_eq!(terms[1].text, "季度");

        let ww = QueryDefaults {
            whole_words: true,
            ..Default::default()
        };
        let expr = parse_query_with("报告 季度", &ww);
        let terms = and_terms(&expr);
        assert!(terms.iter().all(|t| t.whole_words), "{terms:?}");

        let path = QueryDefaults {
            match_path: true,
            ..Default::default()
        };
        let expr = parse_query_with("报告 季度", &path);
        let terms = and_terms(&expr);
        assert!(terms.iter().all(|t| t.match_path), "{terms:?}");

        let regex = QueryDefaults {
            is_regex: true,
            ..Default::default()
        };
        let expr = parse_query_with("报告 季度", &regex);
        let terms = and_terms(&expr);
        assert!(terms.iter().all(|t| t.is_regex), "{terms:?}");

        // Items inside a group or an OR sequence are items of the query too.
        let expr = parse_query_with("<报告 季度> size:>1mb", &case);
        match &expr {
            QueryExpr::And(subs) => {
                assert_eq!(subs.len(), 2);
                match &subs[0] {
                    QueryExpr::And(group) => {
                        assert!(group.iter().all(|s| term_of(s).case_sensitive), "{group:?}")
                    }
                    other => panic!("Expected grouped And, got {other:?}"),
                }
            }
            other => panic!("Expected And expression, got {other:?}"),
        }
        let expr = parse_query_with("报告|季度", &case);
        match &expr {
            QueryExpr::Or(subs) => assert!(subs.iter().all(|s| term_of(s).case_sensitive)),
            other => panic!("Expected Or sequence, got {other:?}"),
        }
        match &parse_query_with("!报告", &case) {
            QueryExpr::Not(inner) => assert!(term_of(inner).case_sensitive),
            other => panic!("Expected Not, got {other:?}"),
        }
    }

    /// A modifier the query text states itself wins over the query-wide default, including
    /// the negative forms (`no-case:`, `no-whole-words:`, `no-path:`).
    #[test]
    fn stated_modifiers_override_query_defaults() {
        let all_on = QueryDefaults {
            case_sensitive: true,
            whole_words: true,
            match_path: true,
            is_regex: true,
            content: true,
            ..Default::default()
        };
        let expr = parse_query_with("no-case:a no-whole-words:b no-path:c d", &all_on);
        let terms = and_terms(&expr);
        assert_eq!(terms.len(), 4);
        assert!(!terms[0].case_sensitive);
        assert!(!terms[1].whole_words);
        assert!(!terms[2].match_path);
        // The unstated item keeps every default.
        assert!(terms[3].case_sensitive && terms[3].whole_words && terms[3].match_path);
        assert!(terms[3].is_regex);

        // A written `content:` states content for its own item only; the sibling still
        // takes the default.
        let expr = parse_query_with("content:架构 notes", &all_on);
        match &expr {
            QueryExpr::And(subs) => {
                assert_eq!(term_of(&subs[0]).content_query.as_deref(), Some("架构"));
                assert_eq!(term_of(&subs[1]).content_query.as_deref(), Some("notes"));
            }
            other => panic!("Expected And chain, got {other:?}"),
        }

        // `case:` turning the default off is not possible: a default only ever states the
        // absence of a modifier, so an item that wrote `case:` keeps it.
        let off = QueryDefaults::default();
        let expr = parse_query_with("case:a b", &off);
        let terms = and_terms(&expr);
        assert!(terms[0].case_sensitive);
        assert!(!terms[1].case_sensitive);
    }

    /// With the content default on, each item's text becomes a content query and stops being
    /// a name/path term - the semantics the `content:{整段文本}` splice only reached for the
    /// first word of, because `read_token` stops at whitespace outside quotes.
    #[test]
    fn content_default_moves_each_item_text_into_a_content_query() {
        let content = QueryDefaults {
            content: true,
            ..Default::default()
        };
        let expr = parse_query_with("报告 季度", &content);
        let terms = and_terms(&expr);
        assert_eq!(terms.len(), 2);
        assert_eq!(terms[0].content_query.as_deref(), Some("报告"));
        assert_eq!(terms[1].content_query.as_deref(), Some("季度"));
        for t in &terms {
            assert!(t.text.is_empty(), "{t:?} must not also constrain the name");
        }

        // Function items carry no text, so the content default leaves them alone.
        let expr = parse_query_with("ext:md 报告", &content);
        let terms = and_terms(&expr);
        assert_eq!(terms[0].ext_filter, vec!["md".to_string()]);
        assert!(terms[0].content_query.is_none());
        assert_eq!(terms[1].content_query.as_deref(), Some("报告"));
    }

    /// An all-false default is the identity: nothing a hand-written query meant before
    /// changes because the defaults exist.
    #[test]
    fn empty_defaults_are_the_identity() {
        for query in [
            "apple banana|orange",
            "case:报告 季度",
            "!<foo bar>",
            "size:>1mb ext:md",
            "content:<abc 123>",
            "parent:C:\\Users file:",
            "no-path:a ww:b regex:c",
            "\"apple banana\"",
            "",
            "   ",
        ] {
            assert_eq!(
                parse_query(query),
                parse_query_with(query, &QueryDefaults::default()),
                "all-false defaults changed {query:?}"
            );
        }
    }

    /// The hidden/system defaults are one more query-wide constraint, ANDed alongside the
    /// items and never folded into a negation.
    #[test]
    fn attribute_defaults_are_a_top_level_constraint() {
        let hidden = QueryDefaults {
            exclude_hidden: true,
            ..Default::default()
        };

        // Off by default: an unchanged query keeps its old width, and a plain term is not
        // itself an attribute constraint.
        assert!(!parse_query("报告").is_empty());
        assert!(QueryExpr::Term(SearchTerm::default()).is_empty());

        // A textless query with the filter on is no longer "every record".
        let expr = parse_query_with("", &hidden);
        assert!(
            !expr.is_empty(),
            "hidden filter must narrow even an empty query"
        );
        let terms = and_terms(&expr);
        assert_eq!(terms.len(), 1);
        assert!(terms[0].exclude_hidden && !terms[0].exclude_system);

        // Appended to the front of an existing AND chain, leaving the items untouched.
        let expr = parse_query_with("foo bar", &hidden);
        let terms = and_terms(&expr);
        assert_eq!(terms.len(), 3);
        assert!(terms[0].exclude_hidden);
        assert_eq!(
            (terms[1].text.as_str(), terms[2].text.as_str()),
            ("foo", "bar")
        );

        // A negated item does not inherit the constraint: the complement of `foo` is not
        // "everything that is not hidden and not foo".
        match &parse_query_with("!foo", &hidden) {
            QueryExpr::And(subs) => {
                assert_eq!(subs.len(), 2);
                assert!(term_of(&subs[0]).exclude_hidden);
                assert!(
                    matches!(&subs[1], QueryExpr::Not(inner) if !term_of(inner).exclude_hidden),
                    "negated item must not carry the attribute filter"
                );
            }
            other => panic!("Expected And expression, got {other:?}"),
        }

        let both = QueryDefaults {
            exclude_hidden: true,
            exclude_system: true,
            ..Default::default()
        };
        let expr = parse_query_with("foo", &both);
        let terms = and_terms(&expr);
        assert!(terms[0].exclude_hidden && terms[0].exclude_system);

        // Content/strip helpers keep the constraint item.
        let stripped = parse_query_with("content:架构", &both).strip_content_queries();
        assert!(
            matches!(stripped, Some(QueryExpr::Term(ref t)) if t.exclude_hidden && t.exclude_system),
            "attribute constraint must survive content stripping: {stripped:?}"
        );
    }

    /// Size operator carried by a single-term expression, so the assertions above stay short.
    fn size_op(expr: &QueryExpr) -> Option<SizeOp> {
        term_of(expr).size_filter.as_ref().map(|f| f.op.clone())
    }
}

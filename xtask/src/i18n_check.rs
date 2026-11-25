// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Static i18n validator.
//!
//! Three failure modes are checked, because each of them reaches the screen as a raw message
//! id: the interface falls back to the key itself when a locale has no message for it.
//!
//!  - a key one locale defines and the other does not;
//!  - a key the code asks for that no locale defines; and
//!  - a key a locale defines that no code asks for.
//!
//! The last two need the crate's own source tree: a message id is a string literal in the
//! crate, and the crate's locale files say which ids exist. Every crate that owns a `locales/`
//! directory is checked, so the installer and the application cannot drift apart either.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The locales every crate that owns a `locales/` directory must provide.
const LOCALE_NAMES: [&str; 2] = ["zh-CN", "en-US"];

/// Checks every crate in the workspace that owns a locale pair.
pub fn check_i18n(workspace: &Path) -> Result<(), String> {
    let mut report = String::new();
    let mut checked = 0usize;

    for crate_dir in crate_dirs(workspace) {
        let locales = crate_dir.join("locales");
        if !locales.is_dir() {
            continue;
        }
        checked += 1;
        if let Err(problems) = check_crate(&crate_dir, &locales) {
            let _ = writeln!(report, "{problems}");
        }
    }

    if checked == 0 {
        return Err(format!(
            "No crate under {:?} owns a locales directory with {}.",
            workspace.join("crates"),
            LOCALE_NAMES.join(" and ")
        ));
    }
    if !report.is_empty() {
        return Err(report.trim_end().to_string());
    }

    println!(
        "i18n check passed: {checked} crate(s) aligned between {} and {}.",
        LOCALE_NAMES[0], LOCALE_NAMES[1]
    );
    Ok(())
}

/// The crate directories under the workspace, in a stable order.
fn crate_dirs(workspace: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(workspace.join("crates")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Checks one crate's locale pair against its own source tree.
fn check_crate(crate_dir: &Path, locales_dir: &Path) -> Result<(), String> {
    let crate_name = crate_dir.file_name().map_or_else(
        || crate_dir.to_string_lossy().to_string(),
        |name| name.to_string_lossy().to_string(),
    );

    let mut messages_by_locale: Vec<BTreeMap<String, BTreeSet<String>>> = Vec::new();
    for locale in LOCALE_NAMES {
        let path = locales_dir.join(format!("{locale}.ftl"));
        let source =
            fs::read_to_string(&path).map_err(|e| format!("{crate_name}: {path:?}: {e}"))?;
        messages_by_locale.push(extract_messages(&source));
    }

    let zh_messages = &messages_by_locale[0];
    let en_messages = &messages_by_locale[1];
    let zh: BTreeSet<String> = zh_messages.keys().cloned().collect();
    let en: BTreeSet<String> = en_messages.keys().cloned().collect();
    let mut problems = String::new();

    let missing_in_en: Vec<&String> = zh.difference(&en).collect();
    let missing_in_zh: Vec<&String> = en.difference(&zh).collect();
    if !missing_in_en.is_empty() || !missing_in_zh.is_empty() {
        let _ = writeln!(
            problems,
            "{crate_name}: the two locales are not aligned.\n  in {} but not {}: {missing_in_en:?}\n  in {} but not {}: {missing_in_zh:?}",
            LOCALE_NAMES[0], LOCALE_NAMES[1], LOCALE_NAMES[1], LOCALE_NAMES[0]
        );
        return Err(problems);
    }

    let mut var_mismatches = Vec::new();
    for (key, zh_vars) in zh_messages {
        if let Some(en_vars) = en_messages.get(key)
            && zh_vars != en_vars
        {
            var_mismatches.push(format!(
                "  {key}: {} has {:?}, {} has {:?}",
                LOCALE_NAMES[0], zh_vars, LOCALE_NAMES[1], en_vars
            ));
        }
    }
    if !var_mismatches.is_empty() {
        let _ = writeln!(
            problems,
            "{crate_name}: placeholder variables differ between {} and {}:\n{}",
            LOCALE_NAMES[0],
            LOCALE_NAMES[1],
            var_mismatches.join("\n")
        );
    }

    let literals = collect_literals(&crate_dir.join("src"));
    let namespaces = namespaces(&zh);

    // A message id the code asks for, that no locale defines, renders as the raw id.
    let undefined: Vec<&String> = literals
        .keys()
        .filter(|literal| is_message_shaped(literal, &namespaces) && !zh.contains(*literal))
        .collect();
    if !undefined.is_empty() {
        let _ = writeln!(
            problems,
            "{crate_name}: the code asks for message ids that no locale defines."
        );
        for literal in undefined {
            let _ = writeln!(
                problems,
                "  {literal} ({})",
                literals[literal].first().map_or("", String::as_str)
            );
        }
    }

    // A message id a locale defines, that no code asks for, is a translation nobody sees.
    let composed = composition_prefixes(&literals);
    let orphans: Vec<&String> = zh
        .iter()
        .filter(|key| !literals.contains_key(*key))
        .filter(|key| !composed.iter().any(|prefix| key.starts_with(prefix)))
        .collect();
    if !orphans.is_empty() {
        let _ = writeln!(
            problems,
            "{crate_name}: the locale defines message ids that no code asks for."
        );
        for key in orphans {
            let _ = writeln!(problems, "  {key}");
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// Extracts each Fluent message id and its `{ $var }` placeholder set (excluding select
/// expression discriminators `{ $var -> ... }`, which plural-inflected locales may add).
fn extract_messages(content: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut messages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current_key: Option<String> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.starts_with('#') {
            continue;
        }
        if !line.is_empty()
            && !line.starts_with(char::is_whitespace)
            && let Some((raw_key, rhs)) = line.split_once('=')
        {
            let key = raw_key.trim();
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && key.starts_with(|c: char| c.is_ascii_alphabetic())
            {
                let vars = messages.entry(key.to_string()).or_default();
                collect_line_placeholders(rhs, vars);
                current_key = Some(key.to_string());
                continue;
            }
        }
        if let Some(key) = &current_key
            && let Some(vars) = messages.get_mut(key)
        {
            collect_line_placeholders(line, vars);
        }
    }
    messages
}

fn collect_line_placeholders(line: &str, vars: &mut BTreeSet<String>) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'$' {
            j += 1;
            let start = j;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-')
            {
                j += 1;
            }
            if j > start {
                let mut k = j;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if !bytes[k..].starts_with(b"->") {
                    vars.insert(line[start..j].to_string());
                }
            }
        }
        i += 1;
    }
}

/// The namespaces the crate's messages live in: the first segment of every message id. A literal
/// is a message id only when it starts with one of them and continues with well-formed
/// segments, so a serde field name or a config key is not mistaken for one.
fn namespaces(keys: &BTreeSet<String>) -> BTreeSet<String> {
    keys.iter()
        .filter_map(|key| key.split('_').next())
        .filter(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// `true` when the literal has the shape of a message id in this crate.
fn is_message_shaped(literal: &str, namespaces: &BTreeSet<String>) -> bool {
    let Some((namespace, rest)) = literal.split_once('_') else {
        return false;
    };
    if !namespaces.contains(namespace) || rest.is_empty() {
        return false;
    }
    rest.split('_').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}

/// The prefixes the code builds message ids from at runtime. A key that starts with one of them
/// is composed rather than written out, so it counts as used.
fn composition_prefixes(literals: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut prefixes: Vec<String> = literals
        .keys()
        .filter_map(|literal| {
            let head = literal.split('{').next()?;
            head.ends_with('_').then(|| head.to_string())
        })
        .collect();
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

/// Every string literal in the crate's non-test Rust sources, with the file it came from.
fn collect_literals(src_dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut scanned_files = Vec::new();
    let mut excluded_files = BTreeSet::new();
    let mut excluded_dirs = BTreeSet::new();

    for path in rust_sources(src_dir) {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let scan = scan_source(&path, &text);
        if scan.whole_file_is_test {
            excluded_files.insert(normalize_path(&path));
            continue;
        }
        for mod_path in scan.excluded_modules {
            if mod_path.extension().is_some_and(|ext| ext == "rs") {
                excluded_files.insert(mod_path);
            } else {
                excluded_dirs.insert(mod_path);
            }
        }
        scanned_files.push((path, scan.literals));
    }

    let mut literals: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, file_literals) in scanned_files {
        let norm = normalize_path(&path);
        if excluded_files.contains(&norm) || excluded_dirs.iter().any(|dir| norm.starts_with(dir)) {
            continue;
        }
        let location = path.to_string_lossy().to_string();
        for literal in file_literals {
            let entry = literals.entry(literal).or_default();
            if !entry.contains(&location) {
                entry.push(location.clone());
            }
        }
    }
    literals
}

/// The Rust source files under a directory.
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

struct ScannedLiterals {
    literals: Vec<String>,
    excluded_modules: Vec<PathBuf>,
    whole_file_is_test: bool,
}

fn scan_source(file_path: &Path, text: &str) -> ScannedLiterals {
    let chars: Vec<char> = text.chars().collect();
    let mut literals = Vec::new();
    let mut excluded_modules = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        if let Some(next) = try_skip_comment(&chars, i) {
            i = next;
            continue;
        }
        if chars[i] == '#'
            && chars.get(i + 1) == Some(&'!')
            && chars.get(i + 2) == Some(&'[')
            && let Some((attr, after)) = parse_attribute_body(&chars, i + 2)
        {
            if is_cfg_test_attr(&attr) {
                return ScannedLiterals {
                    literals: Vec::new(),
                    excluded_modules: Vec::new(),
                    whole_file_is_test: true,
                };
            }
            i = after;
            continue;
        }
        if chars[i] == '#'
            && chars.get(i + 1) == Some(&'[')
            && let Some((attr, after)) = parse_attribute_body(&chars, i + 1)
            && is_cfg_test_attr(&attr)
        {
            i = skip_cfg_test_item(&chars, after, file_path, &mut excluded_modules);
            continue;
        }
        if let Some((after_open, hashes)) = match_raw_string_start(&chars, i) {
            let (raw, next) = scan_raw_string(&chars, after_open, hashes);
            literals.push(raw);
            i = next;
            continue;
        }
        if let Some(after_prefix) = match_quoted_string_prefix(&chars, i) {
            let (lit, next) = scan_quoted_string(&chars, after_prefix);
            literals.push(lit);
            i = next;
            continue;
        }
        if let Some(next) = try_skip_char_literal(&chars, i) {
            i = next;
            continue;
        }
        i += 1;
    }

    ScannedLiterals {
        literals,
        excluded_modules,
        whole_file_is_test: false,
    }
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn prev_is_boundary(chars: &[char], idx: usize) -> bool {
    idx == 0 || !is_ident_char(chars[idx - 1])
}

fn try_skip_comment(chars: &[char], idx: usize) -> Option<usize> {
    if chars[idx] != '/' {
        return None;
    }
    if chars.get(idx + 1) == Some(&'/') {
        let mut i = idx + 2;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        return Some(i);
    }
    if chars.get(idx + 1) == Some(&'*') {
        let mut i = idx + 2;
        let mut depth = 1usize;
        while i < chars.len() && depth > 0 {
            if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                depth += 1;
                i += 2;
            } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        return Some(i);
    }
    None
}

fn skip_ws_and_comments(chars: &[char], i: &mut usize) {
    while *i < chars.len() {
        if chars[*i].is_whitespace() {
            *i += 1;
        } else if let Some(next) = try_skip_comment(chars, *i) {
            *i = next;
        } else {
            break;
        }
    }
}

fn match_raw_string_start(chars: &[char], idx: usize) -> Option<(usize, usize)> {
    if !prev_is_boundary(chars, idx) {
        return None;
    }
    let mut pos = idx;
    if matches!(chars[pos], 'b' | 'c') {
        pos += 1;
    }
    if pos >= chars.len() || chars[pos] != 'r' {
        return None;
    }
    pos += 1;
    let mut hashes = 0usize;
    while pos < chars.len() && chars[pos] == '#' {
        hashes += 1;
        pos += 1;
    }
    (pos < chars.len() && chars[pos] == '"').then_some((pos + 1, hashes))
}

fn scan_raw_string(chars: &[char], mut i: usize, hashes: usize) -> (String, usize) {
    let mut literal = String::new();
    while i < chars.len() {
        if chars[i] == '"' {
            let mut matched = 0usize;
            while matched < hashes && i + 1 + matched < chars.len() && chars[i + 1 + matched] == '#'
            {
                matched += 1;
            }
            if matched == hashes {
                return (literal, i + 1 + hashes);
            }
        }
        literal.push(chars[i]);
        i += 1;
    }
    (literal, i)
}

fn match_quoted_string_prefix(chars: &[char], idx: usize) -> Option<usize> {
    if chars[idx] == '"' {
        return Some(idx + 1);
    }
    if matches!(chars[idx], 'b' | 'c')
        && chars.get(idx + 1) == Some(&'"')
        && prev_is_boundary(chars, idx)
    {
        return Some(idx + 2);
    }
    None
}

fn scan_quoted_string(chars: &[char], mut i: usize) -> (String, usize) {
    let mut literal = String::new();
    while i < chars.len() && chars[i] != '"' {
        if chars[i] == '\\' && i + 1 < chars.len() {
            literal.push(chars[i + 1]);
            i += 2;
            continue;
        }
        literal.push(chars[i]);
        i += 1;
    }
    if i < chars.len() && chars[i] == '"' {
        i += 1;
    }
    (literal, i)
}

fn try_skip_char_literal(chars: &[char], idx: usize) -> Option<usize> {
    let apos = if chars[idx] == '\'' {
        idx
    } else if chars[idx] == 'b' && chars.get(idx + 1) == Some(&'\'') && prev_is_boundary(chars, idx)
    {
        idx + 1
    } else {
        return None;
    };

    let &next = chars.get(apos + 1)?;
    if next == '\\' {
        let mut i = apos + 2;
        if i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        while i < chars.len() && chars[i] != '\'' && chars[i] != '\n' {
            i += 1;
        }
        if i < chars.len() && chars[i] == '\'' {
            return Some(i + 1);
        }
        return Some(i);
    }
    if next != '\'' && next != '\n' && chars.get(apos + 2) == Some(&'\'') {
        return Some(apos + 3);
    }
    None
}

/// Parses an attribute starting at `[` and returns `(inner_text, index_after_closing_bracket)`.
fn parse_attribute_body(chars: &[char], open_bracket: usize) -> Option<(String, usize)> {
    if chars.get(open_bracket) != Some(&'[') {
        return None;
    }
    let mut i = open_bracket + 1;
    let mut depth = 1usize;
    let mut body = String::new();

    while i < chars.len() && depth > 0 {
        if let Some(next) = try_skip_comment(chars, i) {
            i = next;
            continue;
        }
        if let Some((after_open, hashes)) = match_raw_string_start(chars, i) {
            let (raw, next) = scan_raw_string(chars, after_open, hashes);
            body.push('"');
            body.push_str(&raw);
            body.push('"');
            i = next;
            continue;
        }
        if let Some(after_prefix) = match_quoted_string_prefix(chars, i) {
            let (lit, next) = scan_quoted_string(chars, after_prefix);
            body.push('"');
            body.push_str(&lit);
            body.push('"');
            i = next;
            continue;
        }
        if let Some(next) = try_skip_char_literal(chars, i) {
            i = next;
            continue;
        }
        match chars[i] {
            '[' => {
                depth += 1;
                body.push('[');
            }
            ']' => {
                depth -= 1;
                if depth > 0 {
                    body.push(']');
                }
            }
            ch => body.push(ch),
        }
        i += 1;
    }

    (depth == 0).then_some((body, i))
}

fn is_cfg_test_attr(attr_body: &str) -> bool {
    let compact: String = attr_body.chars().filter(|ch| !ch.is_whitespace()).collect();
    compact == "cfg(test)"
}

fn extract_path_attr(attr_body: &str) -> Option<String> {
    let trimmed = attr_body.trim();
    let rest = trimmed.strip_prefix("path")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

fn skip_cfg_test_item(
    chars: &[char],
    mut i: usize,
    file_path: &Path,
    excluded_modules: &mut Vec<PathBuf>,
) -> usize {
    let mut path_attr: Option<String> = None;
    loop {
        skip_ws_and_comments(chars, &mut i);
        if i < chars.len()
            && chars[i] == '#'
            && chars.get(i + 1) == Some(&'[')
            && let Some((attr, after)) = parse_attribute_body(chars, i + 1)
        {
            if let Some(p) = extract_path_attr(&attr) {
                path_attr = Some(p);
            }
            i = after;
            continue;
        }
        break;
    }

    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut saw_brace = false;
    let mut needs_semicolon_after_brace = false;
    let mut header_idents: Vec<String> = Vec::new();
    let mut cur_ident = String::new();

    while i < chars.len() {
        if let Some(next) = try_skip_comment(chars, i) {
            i = next;
            continue;
        }
        if let Some((after_open, hashes)) = match_raw_string_start(chars, i) {
            let (_, next) = scan_raw_string(chars, after_open, hashes);
            i = next;
            continue;
        }
        if let Some(after_prefix) = match_quoted_string_prefix(chars, i) {
            let (_, next) = scan_quoted_string(chars, after_prefix);
            i = next;
            continue;
        }
        if let Some(next) = try_skip_char_literal(chars, i) {
            i = next;
            continue;
        }

        let ch = chars[i];
        if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 && !saw_brace {
            if is_ident_char(ch) {
                cur_ident.push(ch);
                i += 1;
                continue;
            }
            if !cur_ident.is_empty() {
                header_idents.push(std::mem::take(&mut cur_ident));
            }
        }

        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '{' => {
                if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    saw_brace = true;
                    needs_semicolon_after_brace = starts_value_or_type_alias(&header_idents);
                }
                brace_depth += 1;
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if saw_brace && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    i += 1;
                    if !needs_semicolon_after_brace {
                        return i;
                    }
                    continue;
                }
            }
            ';' if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                if !saw_brace
                    && header_idents.len() >= 2
                    && header_idents[header_idents.len() - 2] == "mod"
                {
                    let mod_name = &header_idents[header_idents.len() - 1];
                    record_excluded_module(
                        file_path,
                        mod_name,
                        path_attr.as_deref(),
                        excluded_modules,
                    );
                }
                return i + 1;
            }
            _ => {}
        }
        i += 1;
    }

    i
}

fn starts_value_or_type_alias(idents: &[String]) -> bool {
    let mut idx = 0usize;
    if idents.get(idx).is_some_and(|w| w == "pub") {
        idx += 1;
        if idents
            .get(idx)
            .is_some_and(|w| matches!(w.as_str(), "crate" | "super" | "self"))
        {
            idx += 1;
        }
    }
    match idents.get(idx).map(String::as_str) {
        Some("static" | "type") => true,
        Some("const") => !matches!(
            idents.get(idx + 1).map(String::as_str),
            Some("fn" | "unsafe" | "extern")
        ),
        _ => false,
    }
}

fn record_excluded_module(
    parent_file: &Path,
    mod_name: &str,
    path_attr: Option<&str>,
    excluded: &mut Vec<PathBuf>,
) {
    let Some(parent_dir) = parent_file.parent() else {
        return;
    };
    if let Some(rel) = path_attr {
        excluded.push(normalize_path(&parent_dir.join(rel)));
        return;
    }
    let stem = parent_file.file_stem().and_then(|s| s.to_str());
    let base_dir = match stem {
        Some("lib" | "main" | "mod") => parent_dir.to_path_buf(),
        Some(other) => parent_dir.join(other),
        None => parent_dir.to_path_buf(),
    };
    excluded.push(normalize_path(&base_dir.join(format!("{mod_name}.rs"))));
    excluded.push(normalize_path(&base_dir.join(mod_name)));
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp.as_os_str());
                }
            }
            _ => out.push(comp.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::check_i18n;

    /// A workspace with one crate that owns a locale pair and a source file.
    fn workspace(zh: &str, en: &str, source: &str) -> TempDir {
        let dir = TempDir::new().expect("a temporary workspace");
        let crate_dir = dir.path().join("crates").join("demo");
        fs::create_dir_all(crate_dir.join("locales")).expect("the locale directory");
        fs::create_dir_all(crate_dir.join("src")).expect("the source directory");
        fs::write(crate_dir.join("locales").join("zh-CN.ftl"), zh).expect("the zh locale");
        fs::write(crate_dir.join("locales").join("en-US.ftl"), en).expect("the en locale");
        fs::write(crate_dir.join("src").join("main.rs"), source).expect("the source file");
        dir
    }

    const ZH: &str = "menu_scope = 搜索范围\nmenu_about = 关于\n";
    const EN: &str = "menu_scope = Search scope\nmenu_about = About\n";
    /// A source file that asks for both messages of the pair above.
    const ASKS_FOR_BOTH: &str = "fn main() { let _ = \"menu_scope\"; let _ = \"menu_about\"; }";

    #[test]
    fn a_workspace_whose_code_and_locales_agree_passes() {
        let dir = workspace(ZH, EN, ASKS_FOR_BOTH);
        check_i18n(dir.path()).expect("the workspace is aligned");
    }

    #[test]
    fn a_message_id_the_code_asks_for_but_no_locale_defines_is_reported() {
        let dir = workspace(
            ZH,
            EN,
            "fn main() { let _ = \"menu_scope\"; let _ = \"menu_scope_typo\"; }",
        );
        let report = check_i18n(dir.path()).expect_err("the unknown id must be reported");
        assert!(report.contains("menu_scope_typo"), "{report}");
    }

    #[test]
    fn a_message_id_a_locale_defines_but_no_code_asks_for_is_reported() {
        let dir = workspace(ZH, EN, "fn main() { let _ = \"menu_scope\"; }");
        let report = check_i18n(dir.path()).expect_err("the unused id must be reported");
        assert!(report.contains("menu_about"), "{report}");
    }

    /// A key that only a comment mentions is not a use of it: the comment does not reach the
    /// screen.
    #[test]
    fn a_message_id_mentioned_only_in_a_comment_is_not_a_use() {
        let dir = workspace(
            ZH,
            EN,
            "// \"menu_about\" is mentioned here but never asked for\nfn main() { let _ = \"menu_scope\"; }",
        );
        let report = check_i18n(dir.path()).expect_err("the unused id must be reported");
        assert!(report.contains("menu_about"), "{report}");
    }

    /// A key the code composes at runtime is used even though no literal spells it out.
    #[test]
    fn a_key_the_code_composes_at_runtime_counts_as_used() {
        let zh = "menu_scope = 搜索范围\nvolume_state_ready = 就绪\n";
        let en = "menu_scope = Search scope\nvolume_state_ready = Ready\n";
        let dir = workspace(
            zh,
            en,
            "fn main() { let key = format!(\"volume_state_{}\", \"ready\"); let _ = (key, \"menu_scope\"); }",
        );
        check_i18n(dir.path()).expect("the composed key is used");
    }

    #[test]
    fn a_locale_pair_that_drifted_apart_is_reported() {
        let dir = workspace(ZH, "menu_scope = Search scope\n", ASKS_FOR_BOTH);
        let report = check_i18n(dir.path()).expect_err("the drift must be reported");
        assert!(report.contains("menu_about"), "{report}");
    }

    /// A serde field name is not a message id, and the checker must not ask the locale for one.
    #[test]
    fn a_snake_case_literal_outside_the_message_namespaces_is_left_alone() {
        let dir = workspace(
            ZH,
            EN,
            "fn main() { let _ = \"default_language\"; let _ = \"content_index_enabled\"; }",
        );
        let report = check_i18n(dir.path()).expect_err("the unused ids must still be reported");
        assert!(report.contains("menu_about"), "{report}");
        assert!(!report.contains("default_language"), "{report}");
    }

    #[test]
    fn locales_dir_missing_zh_cn_is_reported_not_skipped() {
        let dir = TempDir::new().expect("tempdir");
        let crate_dir = dir.path().join("crates").join("demo");
        fs::create_dir_all(crate_dir.join("locales")).expect("locales");
        fs::create_dir_all(crate_dir.join("src")).expect("src");
        fs::write(crate_dir.join("locales/en-US.ftl"), EN).expect("en");
        fs::write(crate_dir.join("src/main.rs"), ASKS_FOR_BOTH).expect("src");

        let report = check_i18n(dir.path()).expect_err("missing zh-CN.ftl must fail");
        assert!(report.contains("zh-CN.ftl"), "{report}");
    }

    #[test]
    fn cfg_test_and_external_test_modules_do_not_mask_orphan_keys() {
        let dir = workspace(
            ZH,
            EN,
            "fn main() { let _ = \"menu_scope\"; }\n#[cfg(test)]\nmod tests {\n    fn f() { let _ = \"menu_about\"; }\n}\n#[cfg(test)]\n#[path = \"extra_tests.rs\"]\nmod extra;\n",
        );
        fs::write(
            dir.path().join("crates/demo/src/extra_tests.rs"),
            "fn g() { let _ = \"menu_about\"; }\n",
        )
        .expect("extra_tests");

        let report = check_i18n(dir.path()).expect_err("test-only usage must not keep orphan key");
        assert!(report.contains("menu_about"), "{report}");
    }

    #[test]
    fn raw_strings_nested_comments_and_escaped_char_literals_scan_accurately() {
        let dir = workspace(
            ZH,
            EN,
            "fn main() {\n    let _ = '\\'';\n    let _ = '\\u{27}';\n    /* outer /* inner \"menu_about\" */ */\n    let _ = r#\"\"menu_scope\"\"#;\n    let _ = r\"menu_scope\";\n    let _ = \"menu_about\";\n}\n",
        );
        check_i18n(dir.path()).expect("raw string and nested comment lexing must succeed");
    }

    #[test]
    fn fluent_variable_placeholder_mismatch_is_reported() {
        let dir = workspace(
            "menu_scope = 范围 { $count } ({ $unit })\n",
            "menu_scope = Scope { $count }\n",
            "fn main() { let _ = \"menu_scope\"; }\n",
        );
        let report = check_i18n(dir.path()).expect_err("placeholder mismatch must fail");
        assert!(report.contains("placeholder variables differ"), "{report}");
        assert!(report.contains("menu_scope"), "{report}");
    }
}

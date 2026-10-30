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
use std::path::{Path, PathBuf};

/// The locales every crate that owns a `locales/` directory must provide.
const LOCALE_NAMES: [&str; 2] = ["zh-CN", "en-US"];

/// Checks every crate in the workspace that owns a locale pair.
pub fn check_i18n(workspace: &Path) -> Result<(), String> {
    let mut report = String::new();
    let mut checked = 0usize;

    for crate_dir in crate_dirs(workspace) {
        let locales = crate_dir.join("locales");
        if !locales.join(format!("{}.ftl", LOCALE_NAMES[0])).exists() {
            continue;
        }
        checked += 1;
        if let Err(problems) = check_crate(&crate_dir, &locales) {
            let _ = writeln!(report, "{}", problems);
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
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Checks one crate's locale pair against its own source tree.
fn check_crate(crate_dir: &Path, locales_dir: &Path) -> Result<(), String> {
    let crate_name = crate_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| crate_dir.to_string_lossy().to_string());

    let mut keys_by_locale: Vec<BTreeSet<String>> = Vec::new();
    for locale in LOCALE_NAMES {
        let path = locales_dir.join(format!("{locale}.ftl"));
        let source =
            fs::read_to_string(&path).map_err(|e| format!("{crate_name}: {path:?}: {e}"))?;
        keys_by_locale.push(extract_keys(&source));
    }

    let zh = &keys_by_locale[0];
    let en = &keys_by_locale[1];
    let mut problems = String::new();

    let missing_in_en: Vec<&String> = zh.difference(en).collect();
    let missing_in_zh: Vec<&String> = en.difference(zh).collect();
    if !missing_in_en.is_empty() || !missing_in_zh.is_empty() {
        let _ = writeln!(
            problems,
            "{crate_name}: the two locales are not aligned.\n  in {} but not {}: {missing_in_en:?}\n  in {} but not {}: {missing_in_zh:?}",
            LOCALE_NAMES[0], LOCALE_NAMES[1], LOCALE_NAMES[1], LOCALE_NAMES[0]
        );
        return Err(problems);
    }

    let literals = collect_literals(&crate_dir.join("src"));
    let namespaces = namespaces(zh);

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
                literals[literal].first().map(String::as_str).unwrap_or("")
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

/// The message ids a locale file defines.
fn extract_keys(content: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for line in content.lines() {
        // A Fluent message id starts the line; an indented line continues a value.
        if line.is_empty() || line.starts_with('#') || line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && key.starts_with(|c: char| c.is_ascii_alphabetic())
        {
            keys.insert(key.to_string());
        }
    }
    keys
}

/// The namespaces the crate's messages live in: the first segment of every message id. A literal
/// is a message id only when it starts with one of them and continues with well-formed
/// segments, so a serde field name or a config key is not mistaken for one.
fn namespaces(keys: &BTreeSet<String>) -> BTreeSet<String> {
    keys.iter()
        .filter_map(|key| key.split('_').next())
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
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

/// Every string literal in the crate's Rust sources, with the file it came from.
fn collect_literals(src_dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut literals: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in rust_sources(src_dir) {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for literal in scan_literals(&text) {
            let entry = literals.entry(literal).or_default();
            let location = path.to_string_lossy().to_string();
            if !entry.contains(&location) {
                entry.push(location);
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

/// The string literals in a Rust source file. Comments are skipped, so a message id mentioned in
/// prose does not count as a use of it.
fn scan_literals(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut literals = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
            }
            '"' => {
                i += 1;
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
                i += 1;
                literals.push(literal);
            }
            // A character literal or a lifetime; neither is a message id.
            '\'' => {
                if chars.get(i + 1) == Some(&'\\') || chars.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    literals
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
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Repository hygiene gate: enforces workspace lint inheritance, formatting settings,
//! `#[ignore]` registry parity with `docs/engineering/testing.md`, clean source comments,
//! and UI/product copy symbol restrictions across `.ftl` and product specifications.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const TESTING_DOC: &str = "docs/engineering/testing.md";
const IGNORED_SECTION_HEADER: &str = "## 被忽略的测试";

const FORBID_UNSAFE_ENTRYPOINTS: &[&str] = &[
    "crates/cairn-core/src/lib.rs",
    "crates/cairn-protocol/src/lib.rs",
    "crates/cairn-content/src/lib.rs",
    "crates/cairn-client/src/lib.rs",
    "crates/cairn-cli/src/main.rs",
    "crates/cairn-worker/src/main.rs",
    "crates/cairn-worker/src/detector.rs",
    "crates/cairn-worker/src/extractor.rs",
    "crates/cairn-worker/src/property_extract.rs",
    "crates/cairn-service/src/main.rs",
    "crates/cairn-setup-bin/src/bin/cairn-setup.rs",
    "crates/cairn-setup-bin/src/bin/cairn-uninstall.rs",
    "crates/cairn-setup-bin/src/bin/cairn-install-guard.rs",
    "xtask/src/lib.rs",
    "xtask/src/main.rs",
];

struct ScannedSource {
    comments: Vec<(usize, String)>,
    code_lines: Vec<(usize, String)>,
}

pub fn check_hygiene(workspace: &Path) -> Result<(), String> {
    let mut violations = Vec::new();

    let rust_files = collect_rust_files(workspace)?;
    let member_count = check_workspace_lints(workspace, &mut violations)?;
    let ignored_count = check_sources(workspace, &rust_files, &mut violations)?;
    let copy_count = check_copy_symbols(workspace, &mut violations)?;

    if !violations.is_empty() {
        return Err(format!(
            "{} hygiene violation(s):\n  {}",
            violations.len(),
            violations.join("\n  ")
        ));
    }

    println!(
        "hygiene check passed: {} crates opt into workspace lints, {} ignored tests registered, {} Rust files & {} copy/spec files clean",
        member_count,
        ignored_count,
        rust_files.len(),
        copy_count
    );
    Ok(())
}

const WORKSPACE_EDITION: &str = "2024";
const WORKSPACE_RESOLVER: &str = "3";

const REQUIRED_RUST_LINTS: &[(&str, &str)] = &[
    ("unsafe_op_in_unsafe_fn", "deny"),
    ("elided_lifetimes_in_paths", "warn"),
];
const REQUIRED_RUSTDOC_LINTS: &[(&str, &str)] = &[
    ("broken_intra_doc_links", "deny"),
    ("private_intra_doc_links", "deny"),
];
const REQUIRED_CLIPPY_LINTS: &[(&str, &str)] = &[
    ("undocumented_unsafe_blocks", "deny"),
    ("missing_safety_doc", "deny"),
    ("await_holding_lock", "deny"),
    ("too_many_arguments", "deny"),
    ("large_enum_variant", "deny"),
    ("dbg_macro", "deny"),
    ("todo", "deny"),
    ("unimplemented", "deny"),
    ("uninlined_format_args", "warn"),
    ("redundant_closure_for_method_calls", "warn"),
    ("unnested_or_patterns", "warn"),
    ("manual_let_else", "warn"),
];

fn check_workspace_lints(workspace: &Path, violations: &mut Vec<String>) -> Result<usize, String> {
    let root_toml = read_rel(workspace, Path::new("Cargo.toml"))?;
    let root_doc = match root_toml.parse::<toml::Table>() {
        Ok(table) => Some(table),
        Err(err) => {
            violations.push(format!("Cargo.toml: invalid TOML: {err}"));
            None
        }
    };

    let workspace_table = root_doc
        .as_ref()
        .and_then(|doc| doc.get("workspace"))
        .and_then(toml::Value::as_table);

    let root_resolver = workspace_table
        .and_then(|ws| ws.get("resolver"))
        .and_then(toml::Value::as_str);

    match root_resolver {
        Some(resolver) if resolver == WORKSPACE_RESOLVER => {}
        Some(resolver) => violations.push(format!(
            "Cargo.toml: [workspace].resolver is `{resolver}`, expected `{WORKSPACE_RESOLVER}`"
        )),
        None => {
            violations.push("Cargo.toml: missing string `[workspace].resolver`".to_string());
        }
    }

    let root_edition = workspace_table
        .and_then(|ws| ws.get("package"))
        .and_then(toml::Value::as_table)
        .and_then(|pkg| pkg.get("edition"))
        .and_then(toml::Value::as_str);

    match root_edition {
        Some(edition) if edition == WORKSPACE_EDITION => {}
        Some(edition) => violations.push(format!(
            "Cargo.toml: [workspace.package].edition is `{edition}`, expected `{WORKSPACE_EDITION}`"
        )),
        None => {
            violations.push("Cargo.toml: missing string `[workspace.package].edition`".to_string());
        }
    }

    let lints_table = workspace_table
        .and_then(|ws| ws.get("lints"))
        .and_then(toml::Value::as_table);
    for (section, required) in [
        ("rust", REQUIRED_RUST_LINTS),
        ("rustdoc", REQUIRED_RUSTDOC_LINTS),
        ("clippy", REQUIRED_CLIPPY_LINTS),
    ] {
        let Some(section_table) = lints_table
            .and_then(|lints| lints.get(section))
            .and_then(toml::Value::as_table)
        else {
            violations.push(format!(
                "Cargo.toml: missing `[workspace.lints.{section}]` table"
            ));
            continue;
        };
        for &(key, expected_level) in required {
            let level = Some(section_table)
                .and_then(|t| t.get(key))
                .and_then(|v| v.as_str().or_else(|| v.as_table()?.get("level")?.as_str()));
            match level {
                Some(level) if level == expected_level => {}
                Some(level) => violations.push(format!(
                    "Cargo.toml: [workspace.lints.{section}].{key} is `{level}`, expected `{expected_level}`"
                )),
                None => violations.push(format!(
                    "Cargo.toml: missing `[workspace.lints.{section}].{key} = \"{expected_level}\"`"
                )),
            }
        }
    }

    for entry_rel in FORBID_UNSAFE_ENTRYPOINTS {
        let path = workspace.join(entry_rel);
        if !path.is_file() {
            violations.push(format!(
                "{entry_rel}: required safe-by-contract entrypoint is missing"
            ));
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let scanned = scan_rust_source(&text);
        if !scanned
            .code_lines
            .iter()
            .any(|(_, line)| line.contains("#![forbid(unsafe_code)]"))
        {
            violations.push(format!(
                "{entry_rel}: safe-by-contract entrypoint must declare `#![forbid(unsafe_code)]`"
            ));
        }
    }

    let expected_edition = root_edition.unwrap_or(WORKSPACE_EDITION);
    let rustfmt_path = workspace.join("rustfmt.toml");
    if !rustfmt_path.is_file() {
        violations.push("rustfmt.toml is missing at workspace root".to_string());
    } else {
        let rustfmt = fs::read_to_string(&rustfmt_path)
            .map_err(|err| format!("{}: {err}", rustfmt_path.display()))?;
        match rustfmt.parse::<toml::Table>() {
            Ok(table) => {
                match table.get("edition").and_then(toml::Value::as_str) {
                    Some(edition) if edition == expected_edition => {}
                    Some(edition) => violations.push(format!(
                        "rustfmt.toml: `edition = \"{edition}\"` does not match `[workspace.package].edition` (`{expected_edition}`)"
                    )),
                    None => violations.push(format!(
                        "rustfmt.toml: missing `edition = \"{expected_edition}\"`"
                    )),
                }
            }
            Err(err) => {
                violations.push(format!("rustfmt.toml: invalid TOML: {err}"));
            }
        }
    }

    let mut manifests = Vec::new();
    let crates_dir = workspace.join("crates");
    if crates_dir.is_dir() {
        for entry in
            fs::read_dir(&crates_dir).map_err(|err| format!("{}: {err}", crates_dir.display()))?
        {
            let entry = entry.map_err(|err| format!("{}: {err}", crates_dir.display()))?;
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    let xtask_manifest = workspace.join("xtask").join("Cargo.toml");
    if xtask_manifest.is_file() {
        manifests.push(xtask_manifest);
    }
    manifests.sort();

    let workspace_dep_packages = extract_workspace_dep_packages(workspace_table);

    for manifest in &manifests {
        let content =
            fs::read_to_string(manifest).map_err(|err| format!("{}: {err}", manifest.display()))?;
        let rel = rel_display(workspace, manifest);
        let parsed = match content.parse::<toml::Table>() {
            Ok(table) => table,
            Err(err) => {
                violations.push(format!("{rel}: invalid TOML: {err}"));
                continue;
            }
        };
        if !inherits_workspace_key(&parsed, &["package", "edition"]) {
            violations.push(format!(
                "{rel}: missing `[package] edition.workspace = true`"
            ));
        }
        if !inherits_workspace_key(&parsed, &["lints"]) {
            violations.push(format!("{rel}: missing `[lints] workspace = true`"));
        }
        if ISOLATED_SERVICE_CONTENT_MANIFESTS.contains(&rel.as_str()) {
            check_isolated_manifest_deps(&rel, &parsed, &workspace_dep_packages, violations);
        }
    }

    check_isolated_lockfile_deps(workspace, violations)?;

    Ok(manifests.len())
}

const ISOLATED_SERVICE_CONTENT_MANIFESTS: &[&str] = &[
    "crates/cairn-service/Cargo.toml",
    "crates/cairn-content/Cargo.toml",
];

const ISOLATED_LOCKFILE_ROOTS: &[&str] = &["cairn-service", "cairn-content"];

const FORBIDDEN_PARSER_AND_WORKER_DEPS: &[&str] = &[
    "cairn-worker",
    "lopdf",
    "zip",
    "quick-xml",
    "chardetng",
    "encoding_rs",
];

fn normalize_crate_name(name: &str) -> String {
    name.replace('_', "-").to_ascii_lowercase()
}

fn is_forbidden_isolated_crate(normalized: &str) -> bool {
    FORBIDDEN_PARSER_AND_WORKER_DEPS
        .iter()
        .any(|forbidden| normalize_crate_name(forbidden) == normalized)
}

fn extract_workspace_dep_packages(
    workspace_table: Option<&toml::Table>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(deps) = workspace_table
        .and_then(|ws| ws.get("dependencies"))
        .and_then(toml::Value::as_table)
    else {
        return map;
    };
    for (alias, spec) in deps {
        let pkg = spec
            .as_table()
            .and_then(|t| t.get("package"))
            .and_then(toml::Value::as_str)
            .unwrap_or(alias.as_str());
        map.insert(normalize_crate_name(alias), normalize_crate_name(pkg));
    }
    map
}

fn check_isolated_manifest_deps(
    rel: &str,
    parsed: &toml::Table,
    workspace_dep_packages: &BTreeMap<String, String>,
    violations: &mut Vec<String>,
) {
    let mut check_dep_table = |section: &str, table: &toml::Table| {
        for (alias, spec) in table {
            let explicit_pkg = spec
                .as_table()
                .and_then(|t| t.get("package"))
                .and_then(toml::Value::as_str);
            let actual_pkg = if let Some(pkg) = explicit_pkg {
                normalize_crate_name(pkg)
            } else if spec
                .as_table()
                .and_then(|t| t.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true)
            {
                let key = normalize_crate_name(alias);
                workspace_dep_packages.get(&key).cloned().unwrap_or(key)
            } else {
                normalize_crate_name(alias)
            };
            if is_forbidden_isolated_crate(&actual_pkg) {
                violations.push(format!(
                    "{rel}: forbidden isolated dependency `{actual_pkg}` in [{section}]"
                ));
            }
        }
    };

    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = parsed.get(section).and_then(toml::Value::as_table) {
            check_dep_table(section, deps);
        }
    }
    if let Some(targets) = parsed.get("target").and_then(toml::Value::as_table) {
        for (cfg, target_val) in targets {
            let Some(target_table) = target_val.as_table() else {
                continue;
            };
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(deps) = target_table.get(section).and_then(toml::Value::as_table) {
                    check_dep_table(&format!("target.{cfg}.{section}"), deps);
                }
            }
        }
    }
}

fn check_isolated_lockfile_deps(
    workspace: &Path,
    violations: &mut Vec<String>,
) -> Result<(), String> {
    let lock_path = workspace.join("Cargo.lock");
    if !lock_path.is_file() {
        return Ok(());
    }
    let content =
        fs::read_to_string(&lock_path).map_err(|err| format!("{}: {err}", lock_path.display()))?;
    let parsed = match content.parse::<toml::Table>() {
        Ok(table) => table,
        Err(err) => {
            violations.push(format!("Cargo.lock: invalid TOML: {err}"));
            return Ok(());
        }
    };
    let Some(packages) = parsed.get("package").and_then(toml::Value::as_array) else {
        return Ok(());
    };

    let mut deps_by_pkg: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in packages {
        let Some(pkg_table) = package.as_table() else {
            continue;
        };
        let Some(name) = pkg_table.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let entry = deps_by_pkg.entry(normalize_crate_name(name)).or_default();
        if let Some(deps) = pkg_table
            .get("dependencies")
            .and_then(toml::Value::as_array)
        {
            for dep in deps {
                if let Some(dep_name) = dep.as_str().and_then(|s| s.split_whitespace().next()) {
                    entry.insert(normalize_crate_name(dep_name));
                }
            }
        }
    }

    for &root in ISOLATED_LOCKFILE_ROOTS {
        if !deps_by_pkg.contains_key(root) {
            continue;
        }
        let mut queue = VecDeque::from([root.to_string()]);
        let mut visited = BTreeSet::from([root.to_string()]);
        let mut parent: BTreeMap<String, String> = BTreeMap::new();
        let mut reported = BTreeSet::new();

        while let Some(curr) = queue.pop_front() {
            let Some(next_deps) = deps_by_pkg.get(&curr) else {
                continue;
            };
            for next in next_deps {
                if is_forbidden_isolated_crate(next) && reported.insert(next.clone()) {
                    let mut chain = vec![next.as_str()];
                    let mut cursor = curr.as_str();
                    chain.push(cursor);
                    while let Some(prev) = parent.get(cursor) {
                        chain.push(prev.as_str());
                        cursor = prev.as_str();
                    }
                    chain.reverse();
                    violations.push(format!(
                        "Cargo.lock: forbidden isolated dependency `{next}` reachable from `{root}` via `{}`",
                        chain.join(" -> ")
                    ));
                    continue;
                }
                if visited.insert(next.clone()) {
                    parent.insert(next.clone(), curr.clone());
                    queue.push_back(next.clone());
                }
            }
        }
    }

    Ok(())
}

fn inherits_workspace_key(manifest: &toml::Table, path: &[&str]) -> bool {
    let mut current: Option<&toml::Value> = None;
    for (idx, segment) in path.iter().enumerate() {
        current = if idx == 0 {
            manifest.get(*segment)
        } else {
            current
                .and_then(toml::Value::as_table)
                .and_then(|t| t.get(*segment))
        };
    }
    current
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true)
}

fn check_sources(
    workspace: &Path,
    rust_files: &[PathBuf],
    violations: &mut Vec<String>,
) -> Result<usize, String> {
    let mut actual: BTreeSet<(String, String)> = BTreeSet::new();

    for file in rust_files {
        let text = fs::read_to_string(file).map_err(|err| format!("{}: {err}", file.display()))?;
        let rel = rel_display(workspace, file);
        let crate_name = crate_name_for(&rel);
        let scanned = scan_rust_source(&text);

        for (line_no, comment) in &scanned.comments {
            if let Some(marker) = find_banned_comment_marker(comment) {
                violations.push(format!(
                    "{rel}:{line_no}: forbidden marker `{marker}` in comment"
                ));
            }
        }

        for (idx, (line_no, code)) in scanned.code_lines.iter().enumerate() {
            let trimmed = code.trim();
            if let Some(forbidden_lint) = REQUIRED_CLIPPY_LINTS
                .iter()
                .filter(|&&(_, level)| level == "deny")
                .map(|&(lint, _)| lint)
                .find(|lint| trimmed.contains(&format!("clippy::{lint}")))
            {
                violations.push(format!(
                    "{rel}:{line_no}: forbidden clippy suppression `{trimmed}` (`clippy::{forbidden_lint}`)"
                ));
            }
            if trimmed.contains(concat!("core::prelude::", "rust_"))
                || trimmed.contains(concat!("std::prelude::", "rust_"))
            {
                violations.push(format!(
                    "{rel}:{line_no}: forbidden edition-tied prelude import `{trimmed}`"
                ));
            }
            if !(trimmed.starts_with("#[") && trimmed.contains("ignore")) {
                continue;
            }
            let raw_line = text.lines().nth(line_no.saturating_sub(1)).unwrap_or("");
            if !has_ignore_reason(raw_line) {
                violations.push(format!(
                    "{rel}:{line_no}: #[ignore] must include a non-empty reason (e.g. #[ignore = \"...\"])"
                ));
            }
            match find_next_fn_name(&scanned.code_lines[idx + 1..]) {
                Some(fn_name) => {
                    let inline_modules = inline_module_prefix(&scanned.code_lines, idx);
                    let fn_path = if inline_modules.is_empty() {
                        fn_name
                    } else {
                        format!("{inline_modules}::{fn_name}")
                    };
                    let qualified = qualified_test_name(&rel, &fn_path);
                    if !actual.insert((crate_name.clone(), qualified.clone())) {
                        violations.push(format!(
                            "{rel}:{line_no}: duplicate #[ignore] test `{crate_name}` `{qualified}`"
                        ));
                    }
                }
                None => {
                    violations.push(format!(
                        "{rel}:{line_no}: #[ignore] attribute has no following `fn`"
                    ));
                }
            }
        }
    }

    let testing_path = workspace.join(TESTING_DOC);
    if !testing_path.is_file() {
        violations.push(format!("{TESTING_DOC} is missing"));
        return Ok(actual.len());
    }
    let testing_doc = fs::read_to_string(&testing_path)
        .map_err(|err| format!("{}: {err}", testing_path.display()))?;
    let registered = parse_ignored_table(&testing_doc, violations)?;

    for (crate_name, fn_name) in actual.difference(&registered) {
        violations.push(format!(
            "{TESTING_DOC}: unregistered #[ignore] test `{crate_name}` `{fn_name}`"
        ));
    }
    for (crate_name, fn_name) in registered.difference(&actual) {
        violations.push(format!(
            "{TESTING_DOC}: stale ignored-test entry `{crate_name}` `{fn_name}` (no matching #[ignore] test found)"
        ));
    }

    Ok(actual.len())
}

fn has_ignore_reason(attr_line: &str) -> bool {
    let Some(pos) = attr_line.find("ignore") else {
        return false;
    };
    let rest = &attr_line[pos + "ignore".len()..];
    let Some((_, rhs)) = rest.split_once('=') else {
        return false;
    };
    let value = rhs
        .trim()
        .trim_end_matches([')', ']'])
        .trim()
        .trim_matches('"')
        .trim();
    !value.is_empty()
}

fn find_next_fn_name(lines: &[(usize, String)]) -> Option<String> {
    for (_, raw) in lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("#[") {
            continue;
        }
        if let Some(after_fn) = find_fn_keyword(line) {
            let ident: String = after_fn
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            if !ident.is_empty() {
                return Some(ident);
            }
        }
    }
    None
}

fn find_fn_keyword(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    for (idx, _) in line.match_indices("fn") {
        let prev_ok = idx == 0 || bytes[idx - 1].is_ascii_whitespace();
        let after = idx + 2;
        let next_ok = bytes.get(after).is_some_and(u8::is_ascii_whitespace);
        if prev_ok && next_ok {
            return Some(line[after..].trim_start());
        }
    }
    None
}

fn parse_ignored_table(
    doc: &str,
    violations: &mut Vec<String>,
) -> Result<BTreeSet<(String, String)>, String> {
    let mut entries = BTreeSet::new();
    let mut in_section = false;
    let mut saw_table = false;

    for (line_idx, raw) in doc.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed == IGNORED_SECTION_HEADER {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if trimmed.starts_with("## ") {
            break;
        }
        if !trimmed.starts_with('|') {
            continue;
        }
        saw_table = true;
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 3 {
            continue;
        }
        if cells[0] == "测试" || cells[0].chars().all(|ch| ch == '-' || ch == ':') {
            continue;
        }
        let ticks = backtick_tokens(cells[0]);
        if ticks.len() < 2 {
            violations.push(format!(
                "{TESTING_DOC}:{}: ignored-test row column 1 must contain `<crate> <fn_name>` in backticks",
                line_idx + 1
            ));
            continue;
        }
        if cells[1].is_empty() || cells[2].is_empty() {
            violations.push(format!(
                "{TESTING_DOC}:{}: ignored-test row `{}` `{}` must document both reason and run prerequisites",
                line_idx + 1,
                ticks[0],
                ticks[1]
            ));
        }
        let key = (ticks[0].to_string(), ticks[1].to_string());
        if !entries.insert(key.clone()) {
            violations.push(format!(
                "{TESTING_DOC}:{}: duplicate ignored-test entry `{}` `{}`",
                line_idx + 1,
                key.0,
                key.1
            ));
        }
    }

    if !in_section || !saw_table {
        violations.push(format!(
            "{TESTING_DOC}: missing section `{IGNORED_SECTION_HEADER}` with registration table"
        ));
    }

    Ok(entries)
}

fn backtick_tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        out.push(&after[..end]);
        rest = &after[end + 1..];
    }
    out
}

fn find_banned_comment_marker(comment: &str) -> Option<&'static str> {
    let is_inline = !comment.starts_with('/') && !comment.starts_with('!');
    if is_inline && starts_with_numbered_step(comment) {
        return Some("numbered step narration");
    }
    ["TODO", "FIXME", "XXX", "HACK"]
        .into_iter()
        .find(|&marker| contains_word(comment, marker))
        .or_else(|| {
            ["next milestone", "the old code"]
                .into_iter()
                .find(|&phrase| contains_phrase_ci(comment, phrase))
        })
        .or_else(|| {
            ["Issue #", "PR #"]
                .into_iter()
                .find(|&prefix| contains_numbered_tag(comment, prefix, false))
        })
        .or_else(|| {
            ["Seam", "Phase", "Round", "Step"]
                .into_iter()
                .find(|&stage| contains_numbered_tag(comment, stage, true))
        })
}

fn starts_with_numbered_step(comment: &str) -> bool {
    let trimmed = comment.trim_start();
    let digit_len = trimmed.chars().take_while(char::is_ascii_digit).count();
    digit_len > 0 && trimmed[digit_len..].starts_with(". ")
}

fn contains_phrase_ci(haystack: &str, phrase: &str) -> bool {
    let lower = haystack.to_ascii_lowercase();
    let needle = phrase.to_ascii_lowercase();
    let mut start = 0;
    while let Some(offset) = lower[start..].find(&needle) {
        let abs = start + offset;
        let end = abs + needle.len();
        let prev_ok = lower[..abs]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_ident_char(ch));
        let next_ok = lower[end..]
            .chars()
            .next()
            .is_none_or(|ch| !is_ident_char(ch));
        if prev_ok && next_ok {
            return true;
        }
        let step = lower[abs..].chars().next().map_or(1, char::len_utf8);
        start = abs + step;
    }
    false
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let abs = start + offset;
        let end = abs + needle.len();
        let prev_ok = haystack[..abs]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_ident_char(ch));
        let next_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|ch| !is_ident_char(ch));
        if prev_ok && next_ok {
            return true;
        }
        let step = haystack[abs..].chars().next().map_or(1, char::len_utf8);
        start = abs + step;
    }
    false
}

fn contains_numbered_tag(haystack: &str, tag: &str, require_space: bool) -> bool {
    let lower = haystack.to_ascii_lowercase();
    let needle = tag.to_ascii_lowercase();
    let mut start = 0;
    while let Some(offset) = lower[start..].find(&needle) {
        let abs = start + offset;
        let prev_ok = lower[..abs]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_ident_char(ch));
        if prev_ok {
            let rest = &lower[abs + needle.len()..];
            let has_sep = !require_space || rest.starts_with(char::is_whitespace);
            let after_ws = rest.trim_start();
            if has_sep && after_ws.starts_with(|ch: char| ch.is_ascii_digit()) {
                return true;
            }
        }
        let step = lower[abs..].chars().next().map_or(1, char::len_utf8);
        start = abs + step;
    }
    false
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn check_copy_symbols(workspace: &Path, violations: &mut Vec<String>) -> Result<usize, String> {
    let mut files = Vec::new();

    let crates_dir = workspace.join("crates");
    if crates_dir.is_dir() {
        for entry in
            fs::read_dir(&crates_dir).map_err(|err| format!("{}: {err}", crates_dir.display()))?
        {
            let entry = entry.map_err(|err| format!("{}: {err}", crates_dir.display()))?;
            let locales_dir = entry.path().join("locales");
            if locales_dir.is_dir() {
                for ftl in fs::read_dir(&locales_dir)
                    .map_err(|err| format!("{}: {err}", locales_dir.display()))?
                {
                    let ftl = ftl.map_err(|err| format!("{}: {err}", locales_dir.display()))?;
                    let path = ftl.path();
                    if path.extension().is_some_and(|ext| ext == "ftl") {
                        files.push(path);
                    }
                }
            }
        }
    }

    let mut doc_files = Vec::new();
    for root_doc in ["README.md", "CONTRIBUTING.md"] {
        let p = workspace.join(root_doc);
        if p.is_file() {
            doc_files.push((p, false));
        }
    }
    let spec = workspace.join("docs").join("SPEC.md");
    if spec.is_file() {
        doc_files.push((spec, true));
    }
    for sub in ["product", "engineering"] {
        let dir = workspace.join("docs").join(sub);
        if dir.is_dir() {
            for entry in fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))? {
                let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "md") {
                    doc_files.push((path, true));
                }
            }
        }
    }
    files.sort();
    doc_files.sort();

    for file in &files {
        let text = fs::read_to_string(file).map_err(|err| format!("{}: {err}", file.display()))?;
        let rel = rel_display(workspace, file);
        for (line_idx, line) in text.lines().enumerate() {
            if let Some(ch) = line.chars().find(|&c| is_forbidden_copy_char(c)) {
                violations.push(format!(
                    "{rel}:{}: forbidden symbol U+{:04X} (`{ch}`) in locale copy",
                    line_idx + 1,
                    ch as u32
                ));
            }
        }
    }

    for (file, check_stage_words) in &doc_files {
        let text = fs::read_to_string(file).map_err(|err| format!("{}: {err}", file.display()))?;
        let rel = rel_display(workspace, file);
        let check_stage_words = *check_stage_words || rel == "README.md";
        for (line_idx, line) in text.lines().enumerate() {
            if let Some(ch) = line.chars().find(|&c| is_forbidden_copy_char(c)) {
                violations.push(format!(
                    "{rel}:{}: forbidden symbol U+{:04X} (`{ch}`) in documentation",
                    line_idx + 1,
                    ch as u32
                ));
            }
            if check_stage_words {
                for banned in [
                    "Milestone",
                    "milestone",
                    "首期",
                    "不在本期",
                    "首版",
                    "设计验证阶段",
                    "验证阶段",
                ] {
                    if line.contains(banned) {
                        violations.push(format!(
                            "{rel}:{}: forbidden process/stage wording `{banned}`",
                            line_idx + 1
                        ));
                    }
                }
            }
        }
    }

    Ok(files.len() + doc_files.len())
}

fn is_forbidden_copy_char(ch: char) -> bool {
    matches!(ch, '·' | '▼' | '▾' | '\u{FE0F}')
        || matches!(ch as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF)
}

/// Scans Rust source once, splitting comment lines from non-string code lines so neither
/// `#[ignore]` detection nor comment checks false-trigger on string literals.
fn scan_rust_source(source: &str) -> ScannedSource {
    let chars: Vec<char> = source.chars().collect();
    let mut comments = Vec::new();
    let mut code_lines = Vec::new();
    let mut cur_code = String::new();
    let mut line = 1usize;
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '\n' {
            code_lines.push((line, std::mem::take(&mut cur_code)));
            line += 1;
            i += 1;
            continue;
        }

        if let Some((after_open, hashes)) = match_raw_string_start(&chars, i) {
            i = skip_raw_string(
                &chars,
                after_open,
                hashes,
                &mut line,
                &mut code_lines,
                &mut cur_code,
            );
            continue;
        }

        if ch == '"'
            || (ch == 'b'
                && i + 1 < chars.len()
                && chars[i + 1] == '"'
                && prev_is_boundary(&chars, i))
        {
            if ch == 'b' {
                i += 2;
            } else {
                i += 1;
            }
            while i < chars.len() {
                if chars[i] == '\n' {
                    code_lines.push((line, std::mem::take(&mut cur_code)));
                    line += 1;
                    i += 1;
                } else if chars[i] == '\\' && i + 1 < chars.len() {
                    if chars[i + 1] == '\n' {
                        code_lines.push((line, std::mem::take(&mut cur_code)));
                        line += 1;
                    }
                    i += 2;
                } else if chars[i] == '"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            // Preserve the attribute value presence on lines like `#[ignore = "reason"]`.
            cur_code.push('"');
            cur_code.push('"');
            continue;
        }

        if ch == '\'' && is_char_literal(&chars, i) {
            i = skip_char_literal(&chars, i);
            continue;
        }

        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let start_line = line;
            let mut buf = String::new();
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                buf.push(chars[i]);
                i += 1;
            }
            comments.push((start_line, buf));
            continue;
        }

        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let mut depth = 1usize;
            let mut cur_line = line;
            let mut buf = String::new();
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '\n' {
                    comments.push((cur_line, std::mem::take(&mut buf)));
                    code_lines.push((line, std::mem::take(&mut cur_code)));
                    line += 1;
                    cur_line = line;
                    i += 1;
                } else if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    buf.push(chars[i]);
                    i += 1;
                }
            }
            if !buf.is_empty() {
                comments.push((cur_line, buf));
            }
            continue;
        }

        cur_code.push(ch);
        i += 1;
    }

    if !cur_code.is_empty() {
        code_lines.push((line, cur_code));
    }

    ScannedSource {
        comments,
        code_lines,
    }
}

fn prev_is_boundary(chars: &[char], idx: usize) -> bool {
    idx == 0 || !is_ident_char(chars[idx - 1])
}

fn match_raw_string_start(chars: &[char], idx: usize) -> Option<(usize, usize)> {
    if !prev_is_boundary(chars, idx) {
        return None;
    }
    let mut pos = idx;
    if chars[pos] == 'b' {
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
    (pos < chars.len() && chars[pos] == '"').then(|| (pos + 1, hashes))
}

fn skip_raw_string(
    chars: &[char],
    mut i: usize,
    hashes: usize,
    line: &mut usize,
    code_lines: &mut Vec<(usize, String)>,
    cur_code: &mut String,
) -> usize {
    while i < chars.len() {
        if chars[i] == '\n' {
            code_lines.push((*line, std::mem::take(cur_code)));
            *line += 1;
            i += 1;
            continue;
        }
        if chars[i] == '"' {
            let mut matched = 0usize;
            while matched < hashes && i + 1 + matched < chars.len() && chars[i + 1 + matched] == '#'
            {
                matched += 1;
            }
            if matched == hashes {
                cur_code.push('"');
                cur_code.push('"');
                return i + 1 + hashes;
            }
        }
        i += 1;
    }
    i
}

fn is_char_literal(chars: &[char], idx: usize) -> bool {
    let Some(&next) = chars.get(idx + 1) else {
        return false;
    };
    if next == '\\' {
        return true;
    }
    next != '\n' && chars.get(idx + 2) == Some(&'\'')
}

fn skip_char_literal(chars: &[char], idx: usize) -> usize {
    let mut i = idx + 1;
    if i < chars.len() && chars[i] == '\\' {
        i += 1;
        if i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        while i < chars.len() && chars[i] != '\'' && chars[i] != '\n' {
            i += 1;
        }
        if i < chars.len() && chars[i] == '\'' {
            return i + 1;
        }
        return i;
    }
    if i + 1 < chars.len() && chars[i + 1] == '\'' {
        return i + 2;
    }
    idx + 1
}

fn collect_rust_files(workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for root in ["crates", "xtask"] {
        let dir = workspace.join(root);
        if dir.is_dir() {
            collect_rs_recursive(&dir, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect_rs_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect_rs_recursive(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn inline_module_prefix(lines: &[(usize, String)], end: usize) -> String {
    let mut depth = 0i32;
    let mut modules: Vec<(String, i32)> = Vec::new();
    for (_, line) in &lines[..end] {
        let trimmed = line.trim();
        if let Some(name) = inline_module_name(trimmed) {
            modules.push((name, depth));
        }
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        while modules
            .last()
            .is_some_and(|(_, module_depth)| depth <= *module_depth)
        {
            modules.pop();
        }
    }
    modules
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>()
        .join("::")
}

fn inline_module_name(line: &str) -> Option<String> {
    let rest = if let Some(rest) = line.strip_prefix("pub(") {
        let close = rest.find(')')?;
        rest[close + 1..].trim_start()
    } else {
        ["pub "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .unwrap_or(line)
    };
    let rest = rest.strip_prefix("mod ")?;
    if !rest.contains('{') {
        return None;
    }
    let name: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn qualified_test_name(rel_path: &str, fn_name: &str) -> String {
    let rest = rel_path
        .strip_prefix("crates/")
        .and_then(|tail| tail.split_once('/').map(|(_, tail)| tail))
        .or_else(|| rel_path.split_once('/').map(|(_, tail)| tail))
        .unwrap_or(rel_path);
    if matches!(rest, "src/lib.rs" | "src/main.rs") {
        return fn_name.to_string();
    }
    let without_src = rest.strip_prefix("src/").unwrap_or(rest);
    let stem = without_src.strip_suffix(".rs").unwrap_or(without_src);
    let module_path = stem.strip_suffix("/mod").unwrap_or(stem).replace('/', "::");
    if module_path.is_empty() {
        fn_name.to_string()
    } else {
        format!("{module_path}::{fn_name}")
    }
}

fn crate_name_for(rel_path: &str) -> String {
    let mut parts = rel_path.split('/');
    match (parts.next(), parts.next()) {
        (Some("crates"), Some(crate_dir)) => crate_dir.to_string(),
        (Some(first), _) => first.to_string(),
        _ => String::new(),
    }
}

fn read_rel(workspace: &Path, rel: &Path) -> Result<String, String> {
    let full = workspace.join(rel);
    fs::read_to_string(&full).map_err(|err| format!("{}: {err}", full.display()))
}

fn rel_display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_valid_workspace(root: &Path) {
        fs::create_dir_all(root.join("crates/demo/src")).expect("crate src");
        fs::create_dir_all(root.join("crates/demo/locales")).expect("locales");
        fs::create_dir_all(root.join("xtask/src")).expect("xtask src");
        fs::create_dir_all(root.join("docs/engineering")).expect("docs eng");
        fs::create_dir_all(root.join("docs/product")).expect("docs prod");

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\n[workspace.package]\nedition = \"2024\"\n[workspace.lints.rust]\nunsafe_op_in_unsafe_fn = \"deny\"\nelided_lifetimes_in_paths = \"warn\"\n[workspace.lints.rustdoc]\nbroken_intra_doc_links = \"deny\"\nprivate_intra_doc_links = \"deny\"\n[workspace.lints.clippy]\nundocumented_unsafe_blocks = \"deny\"\nmissing_safety_doc = \"deny\"\nawait_holding_lock = \"deny\"\ntoo_many_arguments = \"deny\"\nlarge_enum_variant = \"deny\"\ndbg_macro = \"deny\"\ntodo = \"deny\"\nunimplemented = \"deny\"\nuninlined_format_args = \"warn\"\nredundant_closure_for_method_calls = \"warn\"\nunnested_or_patterns = \"warn\"\nmanual_let_else = \"warn\"\n",
        )
        .expect("root cargo");
        fs::write(root.join("rustfmt.toml"), "edition = \"2024\"\n").expect("rustfmt");
        for entry_rel in FORBID_UNSAFE_ENTRYPOINTS {
            let path = root.join(entry_rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("entrypoint parent");
            }
            fs::write(path, "#![forbid(unsafe_code)]\n").expect("forbid unsafe entrypoint");
        }
        fs::write(
            root.join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nedition.workspace = true\n[lints]\nworkspace = true\n",
        )
        .expect("demo cargo");
        fs::write(
            root.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nedition.workspace = true\n[lints]\nworkspace = true\n",
        )
        .expect("xtask cargo");
        fs::write(
            root.join("crates/demo/src/lib.rs"),
            "/// 1. Markdown list item in doc comment.\n// Around 2 entries share one path.\n#[test]\n#[ignore = \"requires hardware device\"]\nfn hardware_round_trip() {}\n",
        )
        .expect("demo lib");
        fs::write(root.join("crates/demo/locales/en-US.ftl"), "title = Demo\n").expect("ftl");
        fs::write(root.join("docs/SPEC.md"), "# Spec\n").expect("spec");
        fs::write(
            root.join("docs/engineering/testing.md"),
            "# Testing\n\n## 被忽略的测试\n\n| 测试 | 忽略理由 | 运行方式与前条件 |\n| --- | --- | --- |\n| `demo` `hardware_round_trip` | Requires hardware device | Run with `-- --ignored` |\n",
        )
        .expect("testing doc");
    }

    #[test]
    fn valid_workspace_passes_and_false_positives_are_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("crates/demo/src/strings.rs"),
            "const S: &str = \"TODO Issue #123 Seam 6 · ▼\";\nconst R: &str = r#\"\n#[ignore]\n  // PR #9 Step 1\n\"#;\n",
        )
        .expect("strings");

        check_hygiene(temp.path()).expect("valid workspace passes");
    }

    #[test]
    fn empty_ignore_reason_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("crates/demo/src/lib.rs"),
            "#[test]\n#[ignore = \"\"]\nfn hardware_round_trip() {}\n",
        )
        .expect("demo lib");

        let err = check_hygiene(temp.path()).expect_err("empty reason must fail");
        assert!(err.contains("non-empty reason"), "{err}");
    }

    #[test]
    fn cfg_attr_ignore_without_reason_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("crates/demo/src/lib.rs"),
            "#[test]\n#[cfg_attr(test, ignore)]\nfn hardware_round_trip() {}\n",
        )
        .expect("demo lib");

        let err = check_hygiene(temp.path()).expect_err("unattributed ignore must fail");
        assert!(err.contains("non-empty reason"), "{err}");
    }

    #[test]
    fn expect_and_multiline_clippy_suppressions_are_rejected() {
        for source in [
            "#[expect(clippy::too_many_arguments)]\nfn helper(a: u8) {}\n",
            "#[allow(\n    clippy::too_many_arguments\n)]\nfn helper(a: u8) {}\n",
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            seed_valid_workspace(temp.path());
            fs::write(temp.path().join("crates/demo/src/lib.rs"), source).expect("demo lib");
            let err = check_hygiene(temp.path()).expect_err("suppression must fail");
            assert!(err.contains("forbidden clippy suppression"), "{err}");
        }
    }

    #[test]
    fn missing_workspace_lint_inheritance_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("overwrite cargo");

        let err = check_hygiene(temp.path()).expect_err("missing [lints] must fail");
        assert!(err.contains("crates/demo/Cargo.toml"), "{err}");
    }

    #[test]
    fn bare_or_unregistered_ignore_and_stale_doc_entry_fail() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());

        fs::write(
            temp.path().join("crates/demo/src/lib.rs"),
            "#[test]\n#[ignore]\nfn undocumented_skip() {}\n",
        )
        .expect("overwrite lib");

        let err = check_hygiene(temp.path()).expect_err("ignore drift must fail");
        assert!(err.contains("must include a non-empty reason"), "{err}");
        assert!(err.contains("unregistered #[ignore] test"), "{err}");
        assert!(err.contains("stale ignored-test entry"), "{err}");
    }

    #[test]
    fn comment_markers_and_forbidden_copy_symbols_fail() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());

        fs::write(
            temp.path().join("crates/demo/src/bad.rs"),
            "// TODO clean up Issue #42 after Seam 6 and Step 2\nfn f() {}\n",
        )
        .expect("bad rs");
        fs::write(
            temp.path().join("crates/demo/locales/en-US.ftl"),
            "bad_label = Item · Detail\n",
        )
        .expect("bad ftl");

        let err = check_hygiene(temp.path()).expect_err("markers and symbols must fail");
        assert!(err.contains("TODO"), "{err}");
        assert!(err.contains("bad_label") || err.contains("U+00B7"), "{err}");
    }

    #[test]
    fn missing_forbid_unsafe_numbered_step_and_eng_stage_fail() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());

        fs::write(temp.path().join("xtask/src/lib.rs"), "pub fn helper() {}\n").expect("xtask");
        fs::write(
            temp.path().join("crates/demo/src/steps.rs"),
            concat!(
                "// 1. Check header\n// the old code did X; next milestone fixes Y\n",
                "#[allow(clippy::",
                "too_many_arguments)]\nfn f() {}\n",
            ),
        )
        .expect("steps");
        fs::write(
            temp.path().join("docs/engineering/design.md"),
            "## 架构\n\n首期暂不支持 · 细节\n",
        )
        .expect("eng doc");

        let err = check_hygiene(temp.path()).expect_err("extended hygiene violations must fail");
        assert!(err.contains("forbid(unsafe_code)"), "{err}");
        assert!(err.contains("forbidden clippy suppression"), "{err}");
        assert!(err.contains("numbered step narration"), "{err}");
        assert!(
            err.contains("next milestone") || err.contains("the old code"),
            "{err}"
        );
        assert!(err.contains("首期"), "{err}");
        assert!(err.contains("U+00B7"), "{err}");
    }

    #[test]
    fn missing_elided_lifetimes_lint_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\n[workspace.package]\nedition = \"2024\"\n[workspace.lints.rust]\nunsafe_op_in_unsafe_fn = \"deny\"\n[workspace.lints.rustdoc]\nbroken_intra_doc_links = \"deny\"\nprivate_intra_doc_links = \"deny\"\n[workspace.lints.clippy]\nundocumented_unsafe_blocks = \"deny\"\nmissing_safety_doc = \"deny\"\nawait_holding_lock = \"deny\"\ntoo_many_arguments = \"deny\"\nlarge_enum_variant = \"deny\"\ndbg_macro = \"deny\"\ntodo = \"deny\"\nunimplemented = \"deny\"\nuninlined_format_args = \"warn\"\nredundant_closure_for_method_calls = \"warn\"\nunnested_or_patterns = \"warn\"\n",
        )
        .expect("root cargo");

        let err =
            check_hygiene(temp.path()).expect_err("missing elided_lifetimes_in_paths must fail");
        assert!(err.contains("elided_lifetimes_in_paths"), "{err}");
        assert!(err.contains("manual_let_else"), "{err}");
    }

    #[test]
    fn edition_tied_prelude_import_and_resolver_drift_fail() {
        for import in [
            concat!("use core::prelude::", "rust_2024::test;\n"),
            concat!("use std::prelude::", "rust_2021::test;\n"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            seed_valid_workspace(temp.path());
            fs::write(temp.path().join("crates/demo/src/prelude_leak.rs"), import)
                .expect("write prelude leak");
            let err = check_hygiene(temp.path()).expect_err("edition prelude import must fail");
            assert!(
                err.contains("forbidden edition-tied prelude import"),
                "{err}"
            );
        }

        let temp_drift = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp_drift.path());
        let drifted = fs::read_to_string(temp_drift.path().join("Cargo.toml"))
            .expect("read root cargo")
            .replace("resolver = \"3\"", "resolver = \"2\"");
        fs::write(temp_drift.path().join("Cargo.toml"), drifted).expect("write drifted cargo");
        let err = check_hygiene(temp_drift.path()).expect_err("resolver drift must fail");
        assert!(err.contains("[workspace].resolver"), "{err}");

        let temp_missing = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp_missing.path());
        let missing = fs::read_to_string(temp_missing.path().join("Cargo.toml"))
            .expect("read root cargo")
            .replace("resolver = \"3\"\n", "");
        fs::write(temp_missing.path().join("Cargo.toml"), &missing).expect("write missing cargo");
        let err = check_hygiene(temp_missing.path()).expect_err("missing resolver must fail");
        assert!(
            err.contains("missing string `[workspace].resolver`"),
            "{err}"
        );

        let temp_inline = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp_inline.path());
        let inline_lint = fs::read_to_string(temp_inline.path().join("Cargo.toml"))
            .expect("read root cargo")
            .replace(
                "elided_lifetimes_in_paths = \"warn\"",
                "elided_lifetimes_in_paths = { level = \"warn\", priority = -1 }",
            );
        fs::write(temp_inline.path().join("Cargo.toml"), inline_lint).expect("write inline lint");
        check_hygiene(temp_inline.path()).expect("inline lint table with level must pass");
    }

    #[test]
    fn root_and_rustfmt_edition_drift_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(temp.path().join("rustfmt.toml"), "edition = \"2021\"\n").expect("rustfmt");

        let err = check_hygiene(temp.path()).expect_err("rustfmt edition drift must fail");
        assert!(err.contains("rustfmt.toml"), "{err}");

        let temp_root = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp_root.path());
        let root_cargo = fs::read_to_string(temp_root.path().join("Cargo.toml"))
            .expect("read root cargo")
            .replace("edition = \"2024\"", "edition = \"2021\"");
        fs::write(temp_root.path().join("Cargo.toml"), root_cargo).expect("write root cargo");

        let err = check_hygiene(temp_root.path()).expect_err("root edition drift must fail");
        assert!(err.contains("[workspace.package].edition"), "{err}");
    }

    #[test]
    fn member_manifest_without_workspace_edition_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nedition = \"2024\"\n[lints]\nworkspace = true\n",
        )
        .expect("overwrite demo cargo");

        let err = check_hygiene(temp.path()).expect_err("non-inherited member edition must fail");
        assert!(err.contains("crates/demo/Cargo.toml"), "{err}");
        assert!(err.contains("edition.workspace = true"), "{err}");
    }

    #[test]
    fn isolated_manifests_reject_worker_and_parser_dependencies() {
        for (crate_dir, bad_section) in [
            (
                "cairn-service",
                "[dependencies]\ncairn-worker.workspace = true\n",
            ),
            ("cairn-content", "[dependencies]\nlopdf.workspace = true\n"),
            (
                "cairn-service",
                "[dependencies]\npdf_alias = { package = \"lopdf\", workspace = true }\n",
            ),
            (
                "cairn-content",
                "[dev-dependencies]\nquick-xml.workspace = true\n",
            ),
            (
                "cairn-service",
                "[build-dependencies]\nquick_xml.workspace = true\n",
            ),
            (
                "cairn-content",
                "[dependencies]\nencoding-rs.workspace = true\n",
            ),
            (
                "cairn-service",
                "[target.'cfg(windows)'.dependencies]\nzip.workspace = true\n",
            ),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            seed_valid_workspace(temp.path());
            fs::create_dir_all(temp.path().join("crates").join(crate_dir)).expect("crate dir");
            fs::write(
                temp.path().join("crates").join(crate_dir).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{crate_dir}\"\nedition.workspace = true\n[lints]\nworkspace = true\n{bad_section}"
                ),
            )
            .expect("write isolated manifest");

            let err = check_hygiene(temp.path())
                .expect_err("parser or worker dependency in isolated crate must fail");
            assert!(err.contains("forbidden isolated dependency"), "{err}");
        }

        let temp_alias = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp_alias.path());
        fs::create_dir_all(temp_alias.path().join("crates/cairn-service")).expect("crate dir");
        fs::write(
            temp_alias.path().join("crates/cairn-service/Cargo.toml"),
            "[package]\nname = \"cairn-service\"\nedition.workspace = true\n[lints]\nworkspace = true\n[dependencies]\nzip = { package = \"benign-crate\", version = \"1.0\" }\n",
        )
        .expect("write benign alias");
        check_hygiene(temp_alias.path()).expect("benign crate aliased as zip must pass");
    }

    #[test]
    fn missing_or_commented_forbid_unsafe_entrypoint_fails() {
        let missing = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(missing.path());
        fs::remove_file(missing.path().join("crates/cairn-core/src/lib.rs"))
            .expect("remove entrypoint");
        let err = check_hygiene(missing.path()).expect_err("missing entrypoint must fail");
        assert!(
            err.contains(
                "crates/cairn-core/src/lib.rs: required safe-by-contract entrypoint is missing"
            ),
            "{err}"
        );

        let commented = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(commented.path());
        fs::write(
            commented.path().join("crates/cairn-core/src/lib.rs"),
            "// #![forbid(unsafe_code)]\npub fn f() {}\n",
        )
        .expect("commented forbid");
        let err = check_hygiene(commented.path()).expect_err("commented forbid must fail");
        assert!(
            err.contains("crates/cairn-core/src/lib.rs: safe-by-contract entrypoint must declare"),
            "{err}"
        );
    }

    #[test]
    fn workspace_renamed_and_lockfile_transitive_forbidden_deps_fail() {
        let renamed = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(renamed.path());
        let root_cargo = fs::read_to_string(renamed.path().join("Cargo.toml"))
            .expect("read root cargo")
            + "\n[workspace.dependencies]\npdf_alias = { package = \"lopdf\", version = \"0.47\" }\n";
        fs::write(renamed.path().join("Cargo.toml"), root_cargo).expect("write root cargo");
        fs::write(
            renamed.path().join("crates/cairn-service/Cargo.toml"),
            "[package]\nname = \"cairn-service\"\nedition.workspace = true\n[lints]\nworkspace = true\n[dependencies]\npdf_alias.workspace = true\n",
        )
        .expect("write cairn-service");
        let err = check_hygiene(renamed.path()).expect_err("workspace alias to lopdf must fail");
        assert!(err.contains("forbidden isolated dependency"), "{err}");

        let lock = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(lock.path());
        fs::write(
            lock.path().join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"cairn-content\"\nversion = \"0.1.0\"\ndependencies = [\"helper\"]\n\n[[package]]\nname = \"helper\"\nversion = \"0.1.0\"\ndependencies = [\"quick_xml 0.42.0\"]\n\n[[package]]\nname = \"quick_xml\"\nversion = \"0.42.0\"\n",
        )
        .expect("write lockfile");
        let err = check_hygiene(lock.path()).expect_err("transitive lockfile dep must fail");
        assert!(
            err.contains("Cargo.lock: forbidden isolated dependency"),
            "{err}"
        );
        assert!(
            err.contains("cairn-content -> helper -> quick-xml"),
            "{err}"
        );
    }

    #[test]
    fn inline_modules_keep_distinct_ignored_test_keys() {
        let temp = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(temp.path());
        fs::write(
            temp.path().join("crates/demo/src/lib.rs"),
            "mod alpha {\n    #[test]\n    #[ignore = \"alpha\"]\n    fn same_name() {}\n}\n\
             pub(in crate::foo) mod beta {\n    #[test]\n    #[ignore = \"beta\"]\n    fn same_name() {}\n}\n",
        )
        .expect("inline modules");
        fs::write(
            temp.path().join("docs/engineering/testing.md"),
            "# Testing\n\n## 被忽略的测试\n\n| 测试 | 忽略理由 | 运行方式与前条件 |\n| --- | --- | --- |\n| `demo` `alpha::same_name` | Alpha | Local |\n",
        )
        .expect("testing doc");

        let err = check_hygiene(temp.path()).expect_err("beta test must not be hidden");
        assert!(err.contains("beta::same_name"), "{err}");
    }

    #[test]
    fn all_deny_clippy_suppressions_and_duplicate_ignores_fail() {
        for lint in [
            "undocumented_unsafe_blocks",
            "missing_safety_doc",
            "await_holding_lock",
            "too_many_arguments",
            "large_enum_variant",
            "dbg_macro",
            "todo",
            "unimplemented",
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            seed_valid_workspace(temp.path());
            let source = format!("#[allow(clippy::{lint})]\nfn helper() {{}}\n");
            fs::write(temp.path().join("crates/demo/src/lib.rs"), source).expect("write lint");
            let err = check_hygiene(temp.path()).expect_err("deny clippy suppression must fail");
            assert!(err.contains("forbidden clippy suppression"), "{err}");
        }

        let distinct = tempfile::tempdir().expect("tempdir");
        seed_valid_workspace(distinct.path());
        fs::create_dir_all(distinct.path().join("crates/demo/src/alpha")).expect("alpha");
        fs::create_dir_all(distinct.path().join("crates/demo/src/beta")).expect("beta");
        fs::write(
            distinct.path().join("crates/demo/src/lib.rs"),
            "#[cfg(test)]\nmod alpha;\n#[cfg(test)]\nmod beta;\n",
        )
        .expect("demo lib");
        fs::write(
            distinct.path().join("crates/demo/src/alpha/mod.rs"),
            "#[test]\n#[ignore = \"alpha\"]\nfn same_name() {}\n",
        )
        .expect("alpha mod");
        fs::write(
            distinct.path().join("crates/demo/src/beta/mod.rs"),
            "#[test]\n#[ignore = \"beta\"]\nfn same_name() {}\n",
        )
        .expect("beta mod");
        fs::write(
            distinct.path().join("docs/engineering/testing.md"),
            "# Testing\n\n## 被忽略的测试\n\n| 测试 | 忽略理由 | 运行方式与前条件 |\n| --- | --- | --- |\n| `demo` `alpha::same_name` | Alpha | Local |\n| `demo` `beta::same_name` | Beta | Local |\n",
        )
        .expect("testing doc");
        check_hygiene(distinct.path()).expect("distinct module paths must pass");

        fs::write(
            distinct.path().join("docs/engineering/testing.md"),
            "# Testing\n\n## 被忽略的测试\n\n| 测试 | 忽略理由 | 运行方式与前条件 |\n| --- | --- | --- |\n| `demo` `alpha::same_name` | Alpha | Local |\n| `demo` `alpha::same_name` | Dup | Local |\n| `demo` `beta::same_name` | Beta | Local |\n",
        )
        .expect("dup testing doc");
        let err = check_hygiene(distinct.path()).expect_err("duplicate doc row must fail");
        assert!(err.contains("duplicate ignored-test entry"), "{err}");
    }
}

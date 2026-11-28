// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Comment-budget ratchet: enforces per-file and per-item budgets for non-exempt `//`
//! comment blocks, validates live SAFETY directives and decision links, and rejects Rust
//! lint suppressions and block comments.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hygiene_check::scan_rust_source;

pub(crate) const BASELINE_FILE: &str = ".comment-baseline";
pub(crate) const DEBT_FILE: &str = ".comment-debt";
const BASELINE_VERSION: u32 = 2;
const BASELINE_UNIT: &str = "comment-block";
const GENERATED_BY: &str = "xtask/comment_check";
const DEBT_OWNER: &str = "@zyyyyynnn";
const DEBT_TRACKING: &str = "docs/engineering/quality.md#12-静态检查与回归门禁";

#[derive(Debug, Serialize, Deserialize)]
struct Baseline {
    version: u32,
    unit: String,
    generated_by: String,
    files: BTreeMap<String, usize>,
    items: Vec<Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Item {
    id: String,
    file: String,
    start: usize,
    end: usize,
    kind: String,
    text_hash: String,
    visibility: String,
    test_scope: String,
    outcome: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DebtItem {
    id: String,
    file: String,
    start: usize,
    end: usize,
    kind: String,
    text_hash: String,
    visibility: String,
    test_scope: String,
}

struct FileScan {
    blocks: Vec<Item>,
    suppressions: Vec<usize>,
    block_comments: Vec<usize>,
    violations: Vec<(usize, String)>,
}

pub(crate) fn check_comments(
    workspace: &Path,
    rust_files: &[PathBuf],
    update_baseline: bool,
    allow_increase: bool,
) -> Result<usize, String> {
    if !update_baseline
        && !workspace.join(BASELINE_FILE).is_file()
        && !workspace.join(".git").exists()
    {
        return Ok(0);
    }

    let mut counts = BTreeMap::new();
    let mut items = Vec::new();
    let mut violations = Vec::new();

    for file in rust_files {
        let text = fs::read_to_string(file).map_err(|err| format!("{}: {err}", file.display()))?;
        let rel = relative(workspace, file);
        let scan = scan_file(workspace, &rel, &text);
        for line in scan.suppressions {
            violations.push(format!(
                "{rel}:{line}: Rust lint suppression is not allowed"
            ));
        }
        for line in scan.block_comments {
            violations.push(format!("{rel}:{line}: block comments are not allowed"));
        }
        for (line, reason) in scan.violations {
            violations.push(format!("{rel}:{line}: {reason}"));
        }
        if !scan.blocks.is_empty() {
            counts.insert(rel, scan.blocks.len());
        }
        items.extend(scan.blocks);
    }

    if update_baseline {
        if !violations.is_empty() {
            return Err(format!(
                "cannot update the comment baseline:\n  {}",
                violations.join("\n  ")
            ));
        }
        let existing = read_baseline(workspace)?;
        if let Some(old) = &existing
            && !allow_increase
        {
            let old_ids: BTreeSet<&str> = old.items.iter().map(|item| item.id.as_str()).collect();
            let mut increases: Vec<String> = Vec::new();
            for (rel, count) in &counts {
                let allowed = old.files.get(rel).copied().unwrap_or(0);
                if *count > allowed {
                    increases.push(format!("{rel}: {count} block(s), baseline {allowed}"));
                }
            }
            for item in &items {
                if !old_ids.contains(item.id.as_str()) {
                    increases.push(format!(
                        "{}:{}: new or changed comment block",
                        item.file, item.start
                    ));
                }
            }
            if !increases.is_empty() {
                return Err(format!(
                    "refusing to raise the comment baseline; pass --allow-increase only after review:\n  {}",
                    increases.join("\n  ")
                ));
            }
        }
        write_baseline(workspace, &counts, &items)?;
        write_debt(workspace, &items)?;
        let total = counts.values().sum();
        println!(
            "comment baseline updated: {} file(s), {total} non-exempt comment block(s), {} open debt item(s)",
            counts.len(),
            items.len()
        );
        return Ok(total);
    }

    let Some(old) = read_baseline(workspace)? else {
        if workspace.join(".git").exists() {
            return Err(format!(
                "{BASELINE_FILE} is missing; run `check-comments --update` only after the comments were cleaned"
            ));
        }
        if !violations.is_empty() {
            return Err(format!(
                "{} comment violation(s):\n  {}",
                violations.len(),
                violations.join("\n  ")
            ));
        }
        return Ok(counts.values().sum());
    };

    for rel in counts.keys().chain(old.files.keys()) {
        let current = counts.get(rel).copied().unwrap_or(0);
        let allowed = old.files.get(rel).copied().unwrap_or(0);
        if current > allowed {
            violations.push(format!(
                "{rel}: {current} non-exempt comment block(s), baseline {allowed}"
            ));
        } else if current < allowed {
            violations.push(format!(
                "{rel}: stale baseline: {current} non-exempt comment block(s), baseline {allowed}; run `check-comments --update` after the cleanup"
            ));
        }
    }

    let old_ids: BTreeSet<&str> = old.items.iter().map(|item| item.id.as_str()).collect();
    for item in &items {
        if !old_ids.contains(item.id.as_str()) {
            violations.push(format!(
                "{}:{}: new or changed comment block (id {})",
                item.file, item.start, item.id
            ));
        }
    }

    if !violations.is_empty() {
        return Err(format!(
            "{} comment violation(s):\n  {}",
            violations.len(),
            violations.join("\n  ")
        ));
    }
    Ok(counts.values().sum())
}

fn scan_file(workspace: &Path, rel: &str, text: &str) -> FileScan {
    let scanned = scan_rust_source(text);
    let mut violations = safety_violations(&scanned.comments, &scanned.code_lines);
    for (line, comment) in &scanned.comments {
        let body = comment_body(comment);
        if let Some(link) = link_token(body)
            && let Err(reason) = validate_link(workspace, link)
        {
            violations.push((*line, reason));
        }
    }
    let blocks = non_exempt_blocks(workspace, rel, &scanned.comments, &scanned.code_lines);
    FileScan {
        blocks,
        suppressions: lint_suppressions(&scanned.code_lines),
        block_comments: scanned.block_comment_lines,
        violations,
    }
}

fn safety_violations(
    comments: &[(usize, String)],
    code_lines: &[(usize, String)],
) -> Vec<(usize, String)> {
    let mut violations = Vec::new();
    let mut index = 0;
    while index < comments.len() {
        let (start, text) = &comments[index];
        let kind = comment_kind(text);
        let mut end = index;
        while end + 1 < comments.len()
            && comments[end + 1].0 == comments[end].0 + 1
            && comment_kind(&comments[end + 1].1) == kind
        {
            end += 1;
        }
        if kind == 0
            && text.trim_start().starts_with("SAFETY:")
            && !is_live_safety(code_lines, comments[end].0)
        {
            violations.push((*start, "orphan SAFETY directive on safe code".to_string()));
        }
        index = end + 1;
    }
    violations
}

fn non_exempt_blocks(
    workspace: &Path,
    rel: &str,
    comments: &[(usize, String)],
    code_lines: &[(usize, String)],
) -> Vec<Item> {
    let mut blocks = Vec::new();
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    let mut index = 0;
    while index < comments.len() {
        let (start, text) = &comments[index];
        let kind = comment_kind(text);
        let mut end = index;
        while end + 1 < comments.len()
            && comments[end + 1].0 == comments[end].0 + 1
            && comment_kind(&comments[end + 1].1) == kind
        {
            end += 1;
        }
        let end_line = comments[end].0;
        if kind == 0 {
            let first = text.trim_start();
            let exempt = (*start <= 3
                && (first.starts_with("Cairn -")
                    || first.starts_with("Copyright")
                    || first.starts_with("SPDX-")))
                || (first.starts_with("SAFETY:") && is_live_safety(code_lines, end_line))
                || link_token(first).is_some_and(|link| validate_link(workspace, link).is_ok());
            if !exempt {
                let text = comments[index..=end]
                    .iter()
                    .map(|(_, line)| line.trim())
                    .collect::<Vec<_>>()
                    .join("\n");
                let hash = format!("{:016x}", fnv1a64(&text));
                let count = occurrences.entry(hash.clone()).or_insert(0);
                *count += 1;
                let id = format!(
                    "C-{:016x}",
                    fnv1a64(&format!("{rel}\u{0}{hash}\u{0}{count}"))
                );
                blocks.push(Item {
                    id,
                    file: rel.to_string(),
                    start: *start,
                    end: end_line,
                    kind: "line".to_string(),
                    text_hash: hash,
                    visibility: "n/a".to_string(),
                    test_scope: if is_test_path(rel) {
                        "true".to_string()
                    } else {
                        "unknown".to_string()
                    },
                    outcome: "open".to_string(),
                });
            }
        }
        index = end + 1;
    }
    blocks
}

fn comment_kind(text: &str) -> u8 {
    if text.starts_with('/') {
        1
    } else if text.starts_with('!') {
        2
    } else {
        0
    }
}

fn comment_body(text: &str) -> &str {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix('!') {
        rest.trim_start()
    } else if let Some(rest) = trimmed.strip_prefix('/') {
        rest.trim_start()
    } else {
        trimmed
    }
}

fn link_token(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("See docs/engineering/")?;
    let token = rest.split_whitespace().next()?;
    Some(token.trim_end_matches(['.', ',', ';']))
}

fn validate_link(workspace: &Path, link: &str) -> Result<(), String> {
    if link.is_empty() {
        return Err("decision link is empty".to_string());
    }
    let (path, anchor) = link.split_once('#').unwrap_or((link, ""));
    if path.is_empty() {
        return Err("decision link has no target path".to_string());
    }
    let target = workspace.join("docs/engineering").join(path);
    if !target.is_file() {
        return Err(format!(
            "decision link target docs/engineering/{path} does not exist"
        ));
    }
    if anchor.is_empty() {
        return Ok(());
    }
    let text = fs::read_to_string(&target)
        .map_err(|err| format!("cannot read docs/engineering/{path}: {err}"))?;
    if heading_slugs(&text).contains(anchor) {
        Ok(())
    } else {
        Err(format!(
            "decision link anchor #{anchor} does not exist in docs/engineering/{path}"
        ))
    }
}

fn heading_slugs(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let hashes = line.chars().take_while(|ch| *ch == '#').count();
            (1..=6).contains(&hashes).then(|| {
                let title = line[hashes..].trim();
                slugify(title)
            })
        })
        .collect()
}

fn slugify(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            out.extend(ch.to_lowercase());
        } else if ch.is_whitespace() && !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn is_live_safety(code_lines: &[(usize, String)], end: usize) -> bool {
    code_lines
        .iter()
        .filter(|(line, _)| *line > end)
        .map(|(_, code)| code.trim())
        .filter(|code| !code.is_empty() && !code.starts_with('#'))
        .take(4)
        .any(|code| code.contains("unsafe"))
}

fn lint_suppressions(code_lines: &[(usize, String)]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut in_attribute = false;
    for (line, code) in code_lines {
        let compact: String = code.chars().filter(|ch| !ch.is_whitespace()).collect();
        if compact.starts_with("#[") || compact.starts_with("#![") {
            in_attribute = true;
        }
        if in_attribute && (compact.contains("allow(") || compact.contains("expect(")) {
            found.push(*line);
        }
        if in_attribute && compact.ends_with(']') {
            in_attribute = false;
        }
    }
    found
}

fn is_test_path(rel: &str) -> bool {
    rel.contains("/tests/") || rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs")
}

fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn relative(workspace: &Path, file: &Path) -> String {
    file.strip_prefix(workspace)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_baseline(workspace: &Path) -> Result<Option<Baseline>, String> {
    let path = workspace.join(BASELINE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    if !text.trim_start().starts_with('{') {
        return Err(format!(
            "{}: legacy baseline; remove it and run `check-comments --update`",
            path.display()
        ));
    }
    let baseline: Baseline = serde_json::from_str(&text)
        .map_err(|err| format!("{}: invalid baseline JSON: {err}", path.display()))?;
    if baseline.version != BASELINE_VERSION || baseline.unit != BASELINE_UNIT {
        return Err(format!(
            "{}: expected version {BASELINE_VERSION} unit `{BASELINE_UNIT}`",
            path.display()
        ));
    }
    Ok(Some(baseline))
}

fn write_baseline(
    workspace: &Path,
    counts: &BTreeMap<String, usize>,
    items: &[Item],
) -> Result<(), String> {
    let files = serde_json::to_string(counts)
        .map_err(|err| format!("cannot serialize baseline files: {err}"))?;
    let items = items
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("cannot serialize baseline items: {err}"))?
        .join(",\n    ");
    let text = format!(
        "{{\n  \"version\": {BASELINE_VERSION},\n  \"unit\": \"{BASELINE_UNIT}\",\n  \"generated_by\": \"{GENERATED_BY}\",\n  \"files\": {files},\n  \"items\": [\n    {items}\n  ]\n}}\n"
    );
    let path = workspace.join(BASELINE_FILE);
    fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))
}

fn write_debt(workspace: &Path, items: &[Item]) -> Result<(), String> {
    let count = items.len();
    let items = items
        .iter()
        .map(|item| {
            serde_json::to_string(&DebtItem {
                id: item.id.clone(),
                file: item.file.clone(),
                start: item.start,
                end: item.end,
                kind: item.kind.clone(),
                text_hash: item.text_hash.clone(),
                visibility: item.visibility.clone(),
                test_scope: item.test_scope.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("cannot serialize debt items: {err}"))?
        .join(",\n    ");
    let text = format!(
        "{{\n  \"version\": 1,\n  \"unit\": \"{BASELINE_UNIT}\",\n  \"count\": {count},\n  \"owner\": \"{DEBT_OWNER}\",\n  \"tracking\": \"{DEBT_TRACKING}\",\n  \"items\": [\n    {items}\n  ]\n}}\n"
    );
    let path = workspace.join(DEBT_FILE);
    fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let docs = temp.path().join("docs/engineering");
        fs::create_dir_all(&docs).expect("docs");
        fs::write(
            docs.join("quality.md"),
            "# Quality\n\n### 1.2 static checks\n\n### 2.1 window\n",
        )
        .expect("quality");
        temp
    }

    fn source() -> String {
        "// Cairn - High-performance Windows file search engine\n\
         // Copyright (c) 2026 Cairn Contributors\n\
         // SPDX-License-Identifier: Apache-2.0\n\
         \n\
         // A non-exempt block that restates the constant below.\n\
         const A: u8 = 1;\n\
         // SAFETY: the caller guarantees a live pointer.\n\
         unsafe { consume() };\n\
         // See docs/engineering/quality.md#12-static-checks.\n\
         const B: u8 = 2;\n"
            .to_string()
    }

    fn write_source(temp: &tempfile::TempDir, text: &str) -> PathBuf {
        let dir = temp.path().join("crates/demo/src");
        fs::create_dir_all(&dir).expect("src");
        let path = dir.join("lib.rs");
        fs::write(&path, text).expect("write");
        path
    }

    fn baseline(temp: &tempfile::TempDir) -> Baseline {
        read_baseline(temp.path())
            .expect("read baseline")
            .expect("baseline exists")
    }

    #[test]
    fn baseline_round_trip_and_new_comment_fails() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        check_comments(temp.path(), std::slice::from_ref(&path), false, false)
            .expect("baseline passes");

        let mut text = source();
        text.push_str("\n// New narration.\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("new block fails");
        assert!(
            err.contains("new or changed") || err.contains("baseline"),
            "{err}"
        );
    }

    #[test]
    fn equal_count_swap_fails() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        write_source(
            &temp,
            &source().replace(
                "// A non-exempt block that restates the constant below.",
                "// A different non-exempt block.",
            ),
        );
        let err = check_comments(temp.path(), &[path], false, false).expect_err("swap fails");
        assert!(err.contains("new or changed"), "{err}");
    }

    #[test]
    fn stale_baseline_fails() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        write_source(
            &temp,
            &source().replace(
                "// A non-exempt block that restates the constant below.\n",
                "",
            ),
        );
        let err = check_comments(temp.path(), &[path], false, false).expect_err("stale fails");
        assert!(err.contains("stale baseline"), "{err}");
    }

    #[test]
    fn orphan_safety_and_dead_link_fail() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str("\n// SAFETY: not attached to unsafe code.\nconst C: u8 = 3;\n");
        text.push_str("\n// See docs/engineering/missing.md#nope.\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("directives fail");
        assert!(
            err.contains("SAFETY") || err.contains("does not exist"),
            "{err}"
        );
    }

    #[test]
    fn strings_do_not_count_but_whitespace_suppression_does() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let text = format!(
            "{}\nconst STRING: &str = \"// not a comment #[allow(dead_code)]\";\n#[allow (dead_code)]\nfn helper() {{}}\n",
            source()
        );
        write_source(&temp, &text);
        let err =
            check_comments(temp.path(), &[path], false, false).expect_err("suppression fails");
        assert!(err.contains("lint suppression"), "{err}");
    }

    #[test]
    fn update_refuses_increase_without_allow_flag() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str("\n// New approved block.\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect_err("increase refused");
        assert!(err.contains("refusing to raise"), "{err}");
        check_comments(temp.path(), &[path], true, true).expect("explicit increase");
        assert_eq!(baseline(&temp).files.len(), 1);
    }
}

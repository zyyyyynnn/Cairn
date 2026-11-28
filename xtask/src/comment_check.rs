// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Comment-budget ratchet: enforces per-file counts and per-item identities for non-exempt
//! comment blocks, validates live SAFETY directives and decision links, verifies open-debt
//! parity, and rejects Rust lint suppressions and block comments.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hygiene_check::scan_rust_source;

pub(crate) const BASELINE_FILE: &str = ".comment-baseline";
pub(crate) const DEBT_FILE: &str = ".comment-debt";
const COMMENT_SCHEMA_VERSION: u32 = 1;
const BASELINE_UNIT: &str = "comment-block";
const GENERATED_BY: &str = "xtask/comment_check";
const DEBT_OWNER: &str = "@zyyyyynnn";
const DEBT_TRACKING: &str = "docs/engineering/quality.md#12-静态检查与回归门禁";

#[derive(Debug, Serialize, Deserialize)]
struct Baseline {
    schema_version: u32,
    unit: String,
    generated_by: String,
    files: BTreeMap<String, usize>,
    items: Vec<Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Item {
    id: String,
    file: String,
    kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Debt {
    schema_version: u32,
    unit: String,
    count: usize,
    owner: String,
    tracking: String,
    items: Vec<Item>,
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
                        "{}: new or changed {} comment",
                        item.file, item.kind
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
        if existing.is_none() && workspace.join(".git").exists() && !allow_increase {
            return Err(
                "the comment baseline is missing in a Git workspace; pass --allow-increase to recreate it"
                    .to_string(),
            );
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

    let old = match read_baseline(workspace) {
        Ok(Some(old)) => old,
        Ok(None) => {
            if workspace.join(".git").exists() {
                violations.push(format!(
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
        }
        Err(err) => {
            violations.push(err);
            return Err(format!(
                "{} comment violation(s):\n  {}",
                violations.len(),
                violations.join("\n  ")
            ));
        }
    };

    let keys: BTreeSet<&String> = counts.keys().chain(old.files.keys()).collect();
    for rel in keys {
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
                "{}: new or changed {} comment (id {})",
                item.file, item.kind, item.id
            ));
        }
    }

    match read_debt(workspace) {
        Ok(Some(debt)) => {
            let old_items: BTreeSet<&Item> = old.items.iter().collect();
            let debt_items: BTreeSet<&Item> = debt.items.iter().collect();
            if debt_items != old_items {
                violations.push(format!(
                    "{DEBT_FILE}: open debt items do not match {BASELINE_FILE}"
                ));
            }
        }
        Ok(None) => violations.push(format!(
            "{DEBT_FILE} is missing; run `check-comments --update` after the comments were cleaned"
        )),
        Err(err) => violations.push(err),
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
        for link in link_tokens(body) {
            if let Err(reason) = validate_link(workspace, link) {
                violations.push((*line, reason));
            }
        }
    }
    let blocks = comment_blocks(workspace, rel, &scanned.comments, &scanned.code_lines);
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
    let start_index = spdx_header_len(comments);
    for group in comment_groups(&comments[start_index..]) {
        if group.kind == 0
            && comments[start_index + group.start]
                .1
                .trim_start()
                .starts_with("SAFETY:")
            && !is_live_safety(code_lines, comments[start_index + group.end].0)
        {
            violations.push((
                comments[start_index + group.start].0,
                "orphan SAFETY directive on safe code".to_string(),
            ));
        }
    }
    violations
}

fn comment_blocks(
    workspace: &Path,
    rel: &str,
    comments: &[(usize, String)],
    code_lines: &[(usize, String)],
) -> Vec<Item> {
    let mut blocks = Vec::new();
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    let start_index = spdx_header_len(comments);
    for group in comment_groups(&comments[start_index..]) {
        let start = start_index + group.start;
        let end = start_index + group.end;
        let kind = group.kind;
        let first = &comments[start].1;
        let last_line = comments[end].0;
        let body = comment_body(first);
        let exempt = match kind {
            0 => {
                (body.trim_start().starts_with("SAFETY:") && is_live_safety(code_lines, last_line))
                    || (end == start && link_only(workspace, body))
            }
            _ => true,
        };
        if exempt {
            continue;
        }
        let text = comments[start..=end]
            .iter()
            .map(|(_, line)| line.trim())
            .collect::<Vec<_>>()
            .join("\n");
        let kind_name = match kind {
            1 => "doc",
            2 => "inner",
            _ => "line",
        };
        let key = format!("{rel}\u{0}{kind_name}\u{0}{text}");
        let count = occurrences.entry(key.clone()).or_insert(0);
        *count += 1;
        let id = format!("C-{:016x}", fnv1a64(&format!("{key}\u{0}{count}")));
        blocks.push(Item {
            id,
            file: rel.to_string(),
            kind: kind_name.to_string(),
        });
    }
    blocks
}

struct CommentGroup {
    start: usize,
    end: usize,
    kind: u8,
}

fn comment_groups(comments: &[(usize, String)]) -> Vec<CommentGroup> {
    let mut groups = Vec::new();
    let mut index = 0;
    while index < comments.len() {
        let kind = comment_kind(&comments[index].1);
        let mut end = index;
        while end + 1 < comments.len()
            && comments[end + 1].0 == comments[end].0 + 1
            && comment_kind(&comments[end + 1].1) == kind
        {
            end += 1;
        }
        groups.push(CommentGroup {
            start: index,
            end,
            kind,
        });
        index = end + 1;
    }
    groups
}

fn spdx_header_len(comments: &[(usize, String)]) -> usize {
    let prefixes = [" Cairn -", " Copyright", " SPDX-"];
    if comments.len() >= 3
        && comments[0].0 == 1
        && comments[1].0 == 2
        && comments[2].0 == 3
        && prefixes
            .iter()
            .zip(comments.iter().take(3))
            .all(|(prefix, (_, text))| text.starts_with(*prefix))
    {
        3
    } else {
        0
    }
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
    Some(token.trim_end_matches(['.', ',', ';', ':', '。', '，', '；', '：', '、', ')', '）']))
}

fn link_tokens(body: &str) -> impl Iterator<Item = &str> {
    body.match_indices("See docs/engineering/")
        .map(|(offset, _)| {
            let rest = &body[offset + "See docs/engineering/".len()..];
            let token = rest.split_whitespace().next().unwrap_or_default();
            token.trim_end_matches(['.', ',', ';', ':', '。', '，', '；', '：', '、', ')', '）'])
        })
}

fn link_only(workspace: &Path, body: &str) -> bool {
    let Some(token) = link_token(body) else {
        return false;
    };
    let expected = format!("See docs/engineering/{token}");
    let trimmed =
        body.trim_end_matches(['.', ',', ';', ':', '。', '，', '；', '：', '、', ')', '）']);
    trimmed == expected && validate_link(workspace, token).is_ok()
}

fn validate_link(workspace: &Path, link: &str) -> Result<(), String> {
    if link.is_empty() {
        return Err("decision link is empty".to_string());
    }
    let (path, anchor) = link.split_once('#').unwrap_or((link, ""));
    if path.is_empty() || path.contains('\\') || !path.ends_with(".md") {
        return Err(format!(
            "decision link `{link}` is not a docs/engineering `.md` path"
        ));
    }
    if Path::new(path)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "decision link `{link}` must stay below docs/engineering"
        ));
    }
    let docs = workspace.join("docs/engineering");
    let target = docs.join(path);
    if !target.is_file() {
        return Err(format!(
            "decision link target docs/engineering/{path} does not exist"
        ));
    }
    let docs = fs::canonicalize(&docs)
        .map_err(|err| format!("cannot canonicalize docs/engineering: {err}"))?;
    let target = fs::canonicalize(&target)
        .map_err(|err| format!("cannot canonicalize docs/engineering/{path}: {err}"))?;
    if !target.starts_with(&docs) {
        return Err(format!("decision link `{link}` escapes docs/engineering"));
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
    let mut slugs = BTreeSet::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut fence: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(marker) = &fence {
            if line.starts_with(marker.as_str()) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = Some(line[..3].to_string());
            continue;
        }
        if line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        let hashes = line.chars().take_while(|ch| *ch == '#').count();
        if !(1..=6).contains(&hashes) {
            continue;
        }
        let title = line[hashes..].trim();
        let base = slugify(title);
        if base.is_empty() {
            continue;
        }
        let count = seen.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        slugs.insert(slug);
    }
    slugs
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
    let mut code = code_lines
        .iter()
        .filter(|(line, _)| *line > end)
        .map(|(_, code)| code.trim())
        .filter(|code| !code.is_empty() && !code.starts_with('#'))
        .take(2);
    let Some(first) = code.next() else {
        return false;
    };
    if contains_word(first, "unsafe") {
        return true;
    }
    if first.starts_with("let ") && !first.ends_with(';') {
        return code
            .next()
            .is_some_and(|next| next.trim_start().starts_with("unsafe"));
    }
    false
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let begin = start + offset;
        let end = begin + needle.len();
        let before = haystack[..begin].chars().next_back();
        let after = haystack[end..].chars().next();
        let boundary = |ch: Option<char>| ch.is_none_or(|ch| !(ch.is_alphanumeric() || ch == '_'));
        if boundary(before) && boundary(after) {
            return true;
        }
        start = end;
    }
    false
}

fn lint_suppressions(code_lines: &[(usize, String)]) -> Vec<usize> {
    let mut chars = Vec::new();
    let mut lines = Vec::new();
    for (line, code) in code_lines {
        for ch in code.chars() {
            chars.push(ch);
            lines.push(*line);
        }
        chars.push('\n');
        lines.push(*line);
    }
    let mut found = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '#' {
            index += 1;
            continue;
        }
        let start = index;
        let mut look = index + 1;
        while look < chars.len() && chars[look].is_whitespace() {
            look += 1;
        }
        if look < chars.len() && chars[look] == '!' {
            look += 1;
            while look < chars.len() && chars[look].is_whitespace() {
                look += 1;
            }
        }
        if look >= chars.len() || chars[look] != '[' {
            index += 1;
            continue;
        }
        let mut depth = 1usize;
        let mut cursor = look + 1;
        let mut buffer = String::new();
        while cursor < chars.len() {
            match chars[cursor] {
                '[' => {
                    depth += 1;
                    buffer.push('[');
                }
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        if buffer.contains("allow(") || buffer.contains("expect(") {
                            found.push(lines[start]);
                        }
                        break;
                    }
                    buffer.push(']');
                }
                ch if !ch.is_whitespace() => buffer.push(ch),
                _ => {}
            }
            cursor += 1;
        }
        index = cursor.saturating_add(1).max(index + 1);
    }
    found
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
    let baseline: Baseline = serde_json::from_str(&text)
        .map_err(|err| format!("{}: invalid baseline JSON: {err}", path.display()))?;
    if baseline.schema_version != COMMENT_SCHEMA_VERSION || baseline.unit != BASELINE_UNIT {
        return Err(format!(
            "{}: expected schema_version {COMMENT_SCHEMA_VERSION} unit `{BASELINE_UNIT}`",
            path.display()
        ));
    }
    let ids: BTreeSet<&str> = baseline.items.iter().map(|item| item.id.as_str()).collect();
    if ids.len() != baseline.items.len() {
        return Err(format!("{}: duplicate item ids", path.display()));
    }
    let total: usize = baseline.files.values().sum();
    if total != baseline.items.len() {
        return Err(format!(
            "{}: file counts sum to {total}, items are {}",
            path.display(),
            baseline.items.len()
        ));
    }
    Ok(Some(baseline))
}

fn read_debt(workspace: &Path) -> Result<Option<Debt>, String> {
    let path = workspace.join(DEBT_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let debt: Debt = serde_json::from_str(&text)
        .map_err(|err| format!("{}: invalid debt JSON: {err}", path.display()))?;
    if debt.schema_version != COMMENT_SCHEMA_VERSION || debt.unit != BASELINE_UNIT {
        return Err(format!(
            "{}: expected schema_version {COMMENT_SCHEMA_VERSION} unit `{BASELINE_UNIT}`",
            path.display()
        ));
    }
    if debt.count != debt.items.len() {
        return Err(format!(
            "{}: count {} does not match {} items",
            path.display(),
            debt.count,
            debt.items.len()
        ));
    }
    let ids: BTreeSet<&str> = debt.items.iter().map(|item| item.id.as_str()).collect();
    if ids.len() != debt.items.len() {
        return Err(format!("{}: duplicate item ids", path.display()));
    }
    if let Some(link) = debt.tracking.strip_prefix("docs/engineering/") {
        validate_link(workspace, link)?;
    } else {
        return Err(format!(
            "{}: tracking must stay below docs/engineering",
            path.display()
        ));
    }
    Ok(Some(debt))
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
        "{{\n  \"schema_version\": {COMMENT_SCHEMA_VERSION},\n  \"unit\": \"{BASELINE_UNIT}\",\n  \"generated_by\": \"{GENERATED_BY}\",\n  \"files\": {files},\n  \"items\": [\n    {items}\n  ]\n}}\n"
    );
    write_atomic(&workspace.join(BASELINE_FILE), &text)
}

fn write_debt(workspace: &Path, items: &[Item]) -> Result<(), String> {
    let count = items.len();
    let items = items
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("cannot serialize debt items: {err}"))?
        .join(",\n    ");
    let text = format!(
        "{{\n  \"schema_version\": {COMMENT_SCHEMA_VERSION},\n  \"unit\": \"{BASELINE_UNIT}\",\n  \"count\": {count},\n  \"owner\": \"{DEBT_OWNER}\",\n  \"tracking\": \"{DEBT_TRACKING}\",\n  \"items\": [\n    {items}\n  ]\n}}\n"
    );
    write_atomic(&workspace.join(DEBT_FILE), &text)
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, text).map_err(|err| format!("{}: {err}", temporary.display()))?;
    if fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(path);
        fs::rename(&temporary, path).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
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
            "# Quality\n\n### 1.2 静态检查与回归门禁\n\n### 2.1 流式与虚拟化\n",
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
         // See docs/engineering/quality.md#12-静态检查与回归门禁.\n\
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
    fn orphan_safety_fails() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str("\n// SAFETY: not attached to unsafe code.\nconst C: u8 = 3;\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("orphan fails");
        assert!(err.contains("orphan SAFETY"), "{err}");
    }

    #[test]
    fn dead_link_fails() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str("\n// See docs/engineering/missing.md#nope.\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("link fails");
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn link_traversal_and_multiline_narration_fail() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str("\n// See docs/engineering/../quality.md#12-静态检查与回归门禁.\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), std::slice::from_ref(&path), false, false)
            .expect_err("traversal fails");
        assert!(err.contains("below docs/engineering"), "{err}");

        let mut text = source();
        text.push_str("\n// See docs/engineering/quality.md#12-静态检查与回归门禁.\n// narration after a link\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("narration fails");
        assert!(err.contains("new or changed"), "{err}");
    }

    #[test]
    fn link_with_trailing_prose_is_not_exempt() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str("\n// See docs/engineering/quality.md#12-静态检查与回归门禁 plus prose.\n");
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("prose fails");
        assert!(err.contains("new or changed"), "{err}");
    }

    #[test]
    fn links_after_prose_are_still_validated() {
        let temp = workspace();
        let mut text = source();
        text.push_str("\n// Related: See docs/engineering/quality.md#12-静态检查与回归门禁.\n");
        let path = write_source(&temp, &text);
        check_comments(temp.path(), std::slice::from_ref(&path), true, true)
            .expect("update baseline");

        let broken = text.replace("#12-静态检查与回归门禁", "#missing-anchor");
        write_source(&temp, &broken);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("dead link fails");
        assert!(err.contains("anchor"), "{err}");
    }

    #[test]
    fn crlf_comments_keep_the_same_baseline() {
        let temp = workspace();
        let path = write_source(&temp, &source().replace('\n', "\r\n"));
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        check_comments(temp.path(), std::slice::from_ref(&path), false, false)
            .expect("CRLF comments keep their identity");
    }

    #[test]
    fn safety_must_attach_to_the_next_code_line() {
        let temp = workspace();
        let text = "// SAFETY: this is not attached to an unsafe operation.\n\
             fn first() {}\n\
             fn second() {}\n\
             fn third() {}\n\
             fn fourth() {}\n\
             fn fifth() {}\n\
             unsafe fn sixth() {}\n";
        let path = write_source(&temp, text);
        let err = check_comments(temp.path(), &[path], true, true).expect_err("orphan fails");
        assert!(err.contains("orphan SAFETY"), "{err}");
    }

    #[test]
    fn strings_do_not_count_and_whitespace_suppression_fails() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let text = format!(
            "{}\nconst STRING: &str = \"// not a comment #[allow(dead_code)]\";\n",
            source()
        );
        write_source(&temp, &text);
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("string-only update");
        let text = format!("{text}#[allow (dead_code)]\nfn helper() {{}}\n");
        write_source(&temp, &text);
        let err =
            check_comments(temp.path(), &[path], false, false).expect_err("suppression fails");
        assert!(err.contains("lint suppression"), "{err}");
    }

    #[test]
    fn inline_attributes_are_rejected_but_method_expect_is_not() {
        let temp = workspace();
        let method = format!(
            "{}\nfn checked(value: Option<u8>) {{ let _ = value.expect(\"value\"); }}\n",
            source()
        );
        let path = write_source(&temp, &method);
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("a method named expect is not a lint suppression");

        let text = format!("{method}fn helper(#[allow(unused)] x: u8) {{}}\n");
        let path = write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("attribute fails");
        assert!(err.contains("lint suppression"), "{err}");
    }

    #[test]
    fn split_attribute_suppression_fails() {
        let temp = workspace();
        let text = format!("{}\n#[allow\n(dead_code)]\nfn helper() {{}}\n", source());
        let path = write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("split fails");
        assert!(err.contains("lint suppression"), "{err}");

        let text = format!("{}\n#\n[allow(dead_code)]\nfn helper() {{}}\n", source());
        let path = write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("hash split fails");
        assert!(err.contains("lint suppression"), "{err}");
    }

    #[test]
    fn baseline_and_debt_schema_are_validated() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");

        let baseline_path = temp.path().join(BASELINE_FILE);
        let original_baseline = fs::read_to_string(&baseline_path).expect("baseline text");
        let baseline: serde_json::Value =
            serde_json::from_str(&original_baseline).expect("baseline json");

        let mut wrong_schema = baseline.clone();
        wrong_schema["schema_version"] = serde_json::json!(2);
        fs::write(
            &baseline_path,
            serde_json::to_string(&wrong_schema).expect("serialize"),
        )
        .expect("write");
        let err = read_baseline(temp.path()).expect_err("schema mismatch fails");
        assert!(err.contains("expected schema_version"), "{err}");

        let mut duplicate = baseline.clone();
        let item = duplicate["items"][0].clone();
        duplicate["items"].as_array_mut().expect("items").push(item);
        duplicate["files"] = serde_json::json!({"crates/demo/src/lib.rs": 2});
        fs::write(
            &baseline_path,
            serde_json::to_string(&duplicate).expect("serialize"),
        )
        .expect("write");
        let err = read_baseline(temp.path()).expect_err("duplicate ids fail");
        assert!(err.contains("duplicate item ids"), "{err}");

        let mut mismatched_sum = baseline.clone();
        mismatched_sum["files"] = serde_json::json!({"crates/demo/src/lib.rs": 2});
        fs::write(
            &baseline_path,
            serde_json::to_string(&mismatched_sum).expect("serialize"),
        )
        .expect("write");
        let err = read_baseline(temp.path()).expect_err("sum mismatch fails");
        assert!(err.contains("file counts sum"), "{err}");

        fs::write(&baseline_path, original_baseline).expect("restore baseline");

        let debt_path = temp.path().join(DEBT_FILE);
        let original_debt = fs::read_to_string(&debt_path).expect("debt text");
        let debt: serde_json::Value = serde_json::from_str(&original_debt).expect("debt json");

        let mut wrong_schema = debt.clone();
        wrong_schema["schema_version"] = serde_json::json!(2);
        fs::write(
            &debt_path,
            serde_json::to_string(&wrong_schema).expect("serialize"),
        )
        .expect("write");
        let err = read_debt(temp.path()).expect_err("debt schema fails");
        assert!(err.contains("expected schema_version"), "{err}");

        let mut wrong_count = debt.clone();
        wrong_count["count"] = serde_json::json!(0);
        fs::write(
            &debt_path,
            serde_json::to_string(&wrong_count).expect("serialize"),
        )
        .expect("write");
        let err = read_debt(temp.path()).expect_err("debt count fails");
        assert!(err.contains("count 0"), "{err}");

        let mut wrong_tracking = debt;
        wrong_tracking["tracking"] = serde_json::json!("README.md");
        fs::write(
            &debt_path,
            serde_json::to_string(&wrong_tracking).expect("serialize"),
        )
        .expect("write");
        let err = read_debt(temp.path()).expect_err("debt tracking fails");
        assert!(err.contains("tracking must stay"), "{err}");
    }

    #[test]
    fn baseline_recreation_requires_allow_increase() {
        let temp = workspace();
        fs::create_dir_all(temp.path().join(".git")).expect("git dir");
        let path = write_source(&temp, &source());
        let err = check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect_err("missing baseline in git fails");
        assert!(err.contains("--allow-increase"), "{err}");
        check_comments(temp.path(), std::slice::from_ref(&path), true, true)
            .expect("explicit recreation");
    }

    #[test]
    fn debt_is_read_and_must_match_the_baseline() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        fs::remove_file(temp.path().join(DEBT_FILE)).expect("remove debt");
        let err = check_comments(temp.path(), std::slice::from_ref(&path), false, false)
            .expect_err("missing debt fails");
        assert!(err.contains(DEBT_FILE), "{err}");
        assert_eq!(baseline(&temp).files.len(), 1);

        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("restore debt");
        let mut debt = read_debt(temp.path()).expect("read debt").expect("debt");
        debt.items.pop();
        debt.count = debt.items.len();
        fs::write(
            temp.path().join(DEBT_FILE),
            serde_json::to_string(&debt).expect("serialize debt"),
        )
        .expect("write debt");
        let err =
            check_comments(temp.path(), &[path], false, false).expect_err("mismatched debt fails");
        assert!(err.contains("do not match"), "{err}");
    }

    #[test]
    fn spdx_headers_do_not_absorb_following_narration() {
        let temp = workspace();
        let text = "// Cairn - High-performance Windows file search engine\n\
             // Copyright (c) 2026 Cairn Contributors\n\
             // SPDX-License-Identifier: Apache-2.0\n\
             // narration after the header\n\
             const A: u8 = 1;\n";
        let path = write_source(&temp, text);
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        assert_eq!(baseline(&temp).files.values().sum::<usize>(), 1);
    }

    #[test]
    fn orphan_safety_next_to_the_spdx_header_fails() {
        let temp = workspace();
        let text = "// Cairn - High-performance Windows file search engine\n\
             // Copyright (c) 2026 Cairn Contributors\n\
             // SPDX-License-Identifier: Apache-2.0\n\
             // SAFETY: not attached to unsafe code\n\
             const A: u8 = 1;\n";
        let path = write_source(&temp, text);
        let err = check_comments(temp.path(), &[path], true, true).expect_err("orphan fails");
        assert!(err.contains("orphan SAFETY"), "{err}");
    }

    #[test]
    fn safety_uses_tokens_not_substrings() {
        let temp = workspace();
        let path = write_source(&temp, &source());
        check_comments(temp.path(), std::slice::from_ref(&path), true, false)
            .expect("update baseline");
        let mut text = source();
        text.push_str(
            "\n// SAFETY: unsafe_flag is not an unsafe operation.\nconst FLAG: bool = true;\n",
        );
        write_source(&temp, &text);
        let err = check_comments(temp.path(), &[path], false, false).expect_err("orphan fails");
        assert!(err.contains("orphan SAFETY"), "{err}");
    }

    #[test]
    fn link_tokens_trim_punctuation_and_slugs_ignore_fences() {
        assert_eq!(
            link_token("See docs/engineering/quality.md#12-静态检查与回归门禁。"),
            Some("quality.md#12-静态检查与回归门禁")
        );
        let slugs = heading_slugs("# A\n## A\n```\n## Fenced\n```\n## 3. 中文、标点\n");
        assert!(slugs.contains("a"), "{slugs:?}");
        assert!(slugs.contains("a-1"), "{slugs:?}");
        assert!(!slugs.contains("fenced"), "{slugs:?}");
        assert!(slugs.contains("3-中文标点"), "{slugs:?}");
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
        let baseline = baseline(&temp);
        assert_eq!(baseline.items.len(), 2);
        let debt = read_debt(temp.path()).expect("read debt").expect("debt");
        assert_eq!(debt.count, 2);
    }
}

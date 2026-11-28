// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Comment-budget ratchet: enforces a per-file baseline of non-exempt `//` comment blocks and
//! rejects Rust lint suppressions and block comments across the workspace.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hygiene_check::scan_rust_source;

pub(crate) const BASELINE_FILE: &str = ".comment-baseline";

struct FileScan {
    comment_blocks: usize,
    suppressions: Vec<usize>,
    block_comments: Vec<usize>,
}

pub(crate) fn check_comments(
    workspace: &Path,
    rust_files: &[PathBuf],
    update_baseline: bool,
) -> Result<usize, String> {
    let mut counts = BTreeMap::new();
    let mut violations = Vec::new();

    for file in rust_files {
        let text = fs::read_to_string(file).map_err(|err| format!("{}: {err}", file.display()))?;
        let rel = relative(workspace, file);
        let scan = scan_file(&text);
        for line in &scan.suppressions {
            violations.push(format!(
                "{rel}:{line}: Rust lint suppression is not allowed"
            ));
        }
        for line in &scan.block_comments {
            violations.push(format!("{rel}:{line}: block comments are not allowed"));
        }
        if scan.comment_blocks > 0 {
            counts.insert(rel, scan.comment_blocks);
        }
    }

    if update_baseline {
        if !violations.is_empty() {
            return Err(format!(
                "cannot update the comment baseline:\n  {}",
                violations.join("\n  ")
            ));
        }
        write_baseline(workspace, &counts)?;
        let total = counts.values().sum();
        println!(
            "comment baseline updated: {} file(s), {total} non-exempt comment block(s)",
            counts.len()
        );
        return Ok(total);
    }

    let Some(baseline) = read_baseline(workspace)? else {
        if workspace.join(".git").exists() {
            return Err(format!(
                "{BASELINE_FILE} is missing; run `check-comments --update` only after the comments were cleaned"
            ));
        }
        return Ok(counts.values().sum());
    };

    for (rel, count) in &counts {
        let allowed = baseline.get(rel).copied().unwrap_or(0);
        if *count > allowed {
            violations.push(format!(
                "{rel}: {count} non-exempt comment block(s), baseline {allowed}"
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

fn scan_file(text: &str) -> FileScan {
    let scanned = scan_rust_source(text);
    let suppressions = lint_suppressions(&scanned.code_lines);
    FileScan {
        comment_blocks: non_exempt_blocks(&scanned.comments),
        suppressions,
        block_comments: scanned.block_comment_lines,
    }
}

fn lint_suppressions(code_lines: &[(usize, String)]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut in_attribute = false;
    for (line, code) in code_lines {
        let trimmed = code.trim();
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            in_attribute = true;
        }
        if in_attribute && (trimmed.contains("allow(") || trimmed.contains("expect(")) {
            found.push(*line);
        }
        if in_attribute && trimmed.ends_with(']') {
            in_attribute = false;
        }
    }
    found
}

fn non_exempt_blocks(comments: &[(usize, String)]) -> usize {
    let mut blocks = 0;
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
        if kind == 0 && !exempt_block(*start, text) {
            blocks += 1;
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

fn exempt_block(start: usize, text: &str) -> bool {
    let first = text.trim_start();
    (start <= 3
        && (first.starts_with("Cairn -")
            || first.starts_with("Copyright")
            || first.starts_with("SPDX-")))
        || first.starts_with("SAFETY:")
        || first.starts_with("See docs/engineering/")
}

fn relative(workspace: &Path, file: &Path) -> String {
    file.strip_prefix(workspace)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_baseline(workspace: &Path) -> Result<Option<BTreeMap<String, usize>>, String> {
    let path = workspace.join(BASELINE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut baseline = BTreeMap::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((rel, count)) = line.split_once('\t') else {
            return Err(format!(
                "{}:{}: expected `path<TAB>count`",
                path.display(),
                idx + 1
            ));
        };
        let count: usize = count
            .trim()
            .parse()
            .map_err(|err| format!("{}:{}: invalid count: {err}", path.display(), idx + 1))?;
        baseline.insert(rel.replace('\\', "/"), count);
    }
    Ok(Some(baseline))
}

fn write_baseline(workspace: &Path, counts: &BTreeMap<String, usize>) -> Result<(), String> {
    let path = workspace.join(BASELINE_FILE);
    let mut text = String::from(
        "# Per-file budget of non-exempt `//` comment blocks; the gate fails when a file exceeds it.\n",
    );
    for (rel, count) in counts {
        text.push_str(rel);
        text.push('\t');
        text.push_str(&count.to_string());
        text.push('\n');
    }
    fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, rel: &str, text: &str) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, text).expect("write");
        path
    }

    fn sample() -> String {
        "// Cairn - High-performance Windows file search engine\n\
         // Copyright (c) 2026 Cairn Contributors\n\
         // SPDX-License-Identifier: Apache-2.0\n\
         \n\
         // A non-exempt block that restates the constant below.\n\
         const A: u8 = 1;\n\
         // SAFETY: the caller guarantees a live pointer.\n\
         unsafe { consume() };\n\
         // See docs/engineering/quality.md#1-自动化测试与质量门禁体系.\n\
         const B: u8 = 2;\n"
            .to_string()
    }

    #[test]
    fn baseline_ratchet_allows_existing_and_rejects_new_blocks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = write_file(temp.path(), "crates/demo/src/lib.rs", &sample());
        let files = vec![path.clone()];

        check_comments(temp.path(), &files, true).expect("update baseline");
        check_comments(temp.path(), &files, false).expect("baseline passes");

        fs::write(&path, format!("{}\n// New narration.\n", sample())).expect("append");
        let err = check_comments(temp.path(), &files, false).expect_err("new block must fail");
        assert!(err.contains("baseline"), "{err}");
    }

    #[test]
    fn lint_suppressions_and_block_comments_are_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = write_file(
            temp.path(),
            "crates/demo/src/lib.rs",
            &format!(
                "{}#[allow(dead_code)]\nfn f() {{}}\n/* block */\n",
                sample()
            ),
        );
        check_comments(temp.path(), &[path], true).expect_err("suppression and block must fail");
    }
}

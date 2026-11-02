// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Lightweight documentation gate: required pages exist and every Markdown relative link
//! resolves inside the workspace. External URLs and fragments are intentionally out of scope.

use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED: &[&str] = &[
    "README.md",
    "docs/SPEC.md",
    "docs/product/ui.md",
    "docs/product/brand.md",
    "docs/engineering/architecture.md",
    "docs/engineering/quality.md",
    "docs/engineering/release.md",
];

pub fn check_docs(workspace: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_markdown(workspace, &mut files);
    files.sort();

    let mut broken = Vec::new();
    let mut relative_links = 0usize;
    for file in &files {
        let text =
            fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
        for (line, target) in links_in(&text) {
            let Some(path) = resolve_link(workspace, file, &target) else {
                continue;
            };
            relative_links += 1;
            if !path.exists() {
                broken.push(format!(
                    "{}:{line} -> {target}",
                    file.strip_prefix(workspace).unwrap_or(file).display()
                ));
            }
        }
    }

    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|path| !workspace.join(path).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "required documentation is missing:\n  {}",
            missing.join("\n  ")
        ));
    }
    if !broken.is_empty() {
        return Err(format!(
            "{} broken relative link(s):\n  {}",
            broken.len(),
            broken.join("\n  ")
        ));
    }
    println!(
        "documentation check passed: {} Markdown files, {} relative links",
        files.len(),
        relative_links
    );
    Ok(())
}

fn collect_markdown(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.') || matches!(name.as_ref(), "target" | "node_modules" | "dist")
            {
                continue;
            }
            collect_markdown(&path, files);
        } else if name.ends_with(".md") {
            files.push(path);
        }
    }
}

fn links_in(text: &str) -> Vec<(usize, String)> {
    let mut links = Vec::new();
    let mut in_fence = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("](") {
            let after = &rest[start + 2..];
            let Some(end) = after.find(')') else {
                break;
            };
            if let Some(target) = link_target(&after[..end]) {
                links.push((index + 1, target));
            }
            rest = &after[end + 1..];
        }
        if let Some(rest) = line.trim_start().strip_prefix('[') {
            if let Some((_, target)) = rest.split_once("]: ") {
                if let Some(target) = link_target(target) {
                    links.push((index + 1, target));
                }
            }
        }
    }
    links
}

fn link_target(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix('<')
        .and_then(|value| value.split_once('>'))
        .map_or(raw, |(value, _)| value);
    let target = raw.split_whitespace().next()?.trim();
    if target.is_empty() || target.starts_with('#') {
        return None;
    }
    let lower = target.to_ascii_lowercase();
    if [
        "http://", "https://", "mailto:", "tel:", "data:", "file:", "ftp://",
    ]
    .iter()
    .any(|scheme| lower.starts_with(scheme))
    {
        return None;
    }
    let target = target.split(['#', '?']).next().unwrap_or(target);
    if target.is_empty() {
        return None;
    }
    Some(target.replace("%20", " "))
}

fn resolve_link(workspace: &Path, source: &Path, target: &str) -> Option<PathBuf> {
    let target = target.trim_start_matches(['/', '\\']);
    if target.is_empty() {
        return None;
    }
    let base = source.parent().unwrap_or(workspace);
    Some(base.join(target))
}

#[cfg(test)]
mod tests {
    use super::{check_docs, links_in, REQUIRED};

    #[test]
    fn fenced_code_and_external_links_are_not_targets() {
        let text = "\
[ok](docs/SPEC.md)
```text
[ignored](missing.md)
```
[web](https://example.com)
[anchor](#section)
";
        assert_eq!(links_in(text), vec![(1, "docs/SPEC.md".to_string())]);
    }

    #[test]
    fn a_temp_workspace_reports_only_broken_relative_links() {
        let temp = tempfile::tempdir().expect("temp dir");
        for required in REQUIRED {
            let path = temp.path().join(required);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(&path, "# page\n").expect("required page");
        }
        std::fs::write(temp.path().join("README.md"), "[spec](docs/SPEC.md)\n").expect("readme");
        check_docs(temp.path()).expect("a valid relative link");

        std::fs::write(temp.path().join("README.md"), "[gone](docs/missing.md)\n").expect("readme");
        let error = check_docs(temp.path()).expect_err("the missing link must fail");
        assert!(error.contains("docs/missing.md"), "{error}");
    }

    #[test]
    fn a_missing_required_page_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("README.md"), "# readme\n").expect("readme");
        let error = check_docs(temp.path()).expect_err("required pages must exist");
        assert!(error.contains("docs/SPEC.md"), "{error}");
    }
}

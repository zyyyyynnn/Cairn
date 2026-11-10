// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Source-size gate: a Rust file above 2000 lines is a monolith waiting to happen. The check
//! covers the production and test sources of every crate so a split cannot silently regress.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 2000;

pub fn check_size(workspace: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    for root in ["crates", "xtask"] {
        collect_rust_files(&workspace.join(root), &mut files);
    }
    files.sort();

    let mut oversized = Vec::new();
    let mut largest: (usize, PathBuf) = (0, PathBuf::new());
    for file in &files {
        let text =
            fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
        let count = text.lines().count();
        if count > MAX_LINES {
            oversized.push(format!(
                "{}: {count} lines (max {MAX_LINES})",
                file.strip_prefix(workspace).unwrap_or(file).display()
            ));
        }
        if count > largest.0 {
            largest = (
                count,
                file.strip_prefix(workspace).unwrap_or(file).to_path_buf(),
            );
        }
    }

    if !oversized.is_empty() {
        return Err(format!(
            "{} file(s) above the {MAX_LINES}-line limit:\n  {}",
            oversized.len(),
            oversized.join("\n  ")
        ));
    }
    println!(
        "size check passed: {} source files, max {} lines ({})",
        files.len(),
        largest.0,
        largest.1.display()
    );
    Ok(())
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_at_the_limit_passes_and_one_above_it_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("crates").join("demo").join("src");
        fs::create_dir_all(&src).expect("create src");
        fs::write(src.join("ok.rs"), "x\n".repeat(MAX_LINES)).expect("write at limit");
        check_size(temp.path()).expect("a file at the limit passes");

        fs::write(src.join("big.rs"), "x\n".repeat(MAX_LINES + 1)).expect("write over limit");
        let err = check_size(temp.path()).expect_err("an oversized file must fail");
        assert!(
            err.contains("big.rs"),
            "the error must name the file: {err}"
        );
    }

    #[test]
    fn non_rust_files_do_not_count() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("crates").join("demo").join("src");
        fs::create_dir_all(&src).expect("create src");
        fs::write(src.join("notes.md"), "x\n".repeat(MAX_LINES + 1)).expect("write markdown");
        check_size(temp.path()).expect("only Rust sources are gated");
    }
}

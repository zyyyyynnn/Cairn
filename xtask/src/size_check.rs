// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Source-size gate: a Rust file above 2000 lines is a monolith waiting to happen. The check
//! covers the production and test sources of every crate so a split cannot silently regress.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 2000;

pub fn check_size(workspace: &Path) -> Result<(), String> {
    let mut files = crate::hygiene_check::collect_rust_files(workspace)?;
    files.sort();
    files.dedup();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/demo\", \"xtask\"]\n",
        )
        .expect("root manifest");
        fs::create_dir_all(temp.path().join("crates/demo/src")).expect("create src");
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("demo manifest");
        fs::create_dir_all(temp.path().join("xtask")).expect("create xtask root");
        fs::write(
            temp.path().join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\n",
        )
        .expect("xtask manifest");
        temp
    }

    #[test]
    fn a_file_at_the_limit_passes_and_one_above_it_fails() {
        let temp = workspace();
        let src = temp.path().join("crates").join("demo").join("src");
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
        let temp = workspace();
        let src = temp.path().join("crates").join("demo").join("src");
        fs::write(src.join("notes.md"), "x\n".repeat(MAX_LINES + 1)).expect("write markdown");
        check_size(temp.path()).expect("only Rust sources are gated");
    }

    #[test]
    fn a_missing_member_source_root_is_an_error_not_a_silent_pass() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/demo\", \"xtask\"]\n",
        )
        .expect("root manifest");
        let err = check_size(temp.path()).expect_err("a missing member source root must fail");
        assert!(
            err.contains("crates") && err.contains("demo"),
            "the error must name the missing member: {err}"
        );
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Version governance gate: keeps the product version in one place, requires every member
//! manifest to inherit it, and rejects the current product version outside the manifests,
//! lockfile, or release notes.

use std::fs;
use std::path::Path;

use toml::{Table, Value};

const PRODUCT_VERSION_ALLOWED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "CHANGELOG.md",
    "docs/engineering/release.md",
];

pub(crate) fn check_versions(workspace: &Path) -> Result<usize, String> {
    let root = read_toml(&workspace.join("Cargo.toml"))?;
    let version = workspace_version(&root)?;
    let mut violations = Vec::new();
    let members = check_member_manifests(workspace, &mut violations)?;
    let scanned = scan_version_literals(workspace, &version, &mut violations)?;

    if !violations.is_empty() {
        return Err(format!(
            "{} version violation(s):\n  {}",
            violations.len(),
            violations.join("\n  ")
        ));
    }
    println!(
        "version check passed: product `{version}`, {members} member manifest(s), {scanned} file(s) scanned"
    );
    Ok(scanned)
}

fn workspace_version(root: &Table) -> Result<String, String> {
    root.get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(Value::as_str)
        .filter(|version| !version.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Cargo.toml: missing `[workspace.package].version`".to_string())
}

fn check_member_manifests(workspace: &Path, violations: &mut Vec<String>) -> Result<usize, String> {
    let mut manifests = Vec::new();
    let crates = workspace.join("crates");
    if crates.is_dir() {
        let entries =
            fs::read_dir(&crates).map_err(|err| format!("{}: {err}", crates.display()))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("{}: {err}", crates.display()))?;
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    let xtask = workspace.join("xtask/Cargo.toml");
    if xtask.is_file() {
        manifests.push(xtask);
    }
    manifests.sort();

    for manifest in &manifests {
        let rel = relative(workspace, manifest);
        let doc = read_toml(manifest)?;
        let inherited = doc
            .get("package")
            .and_then(|package| package.get("version"))
            .and_then(Value::as_table)
            .and_then(|version| version.get("workspace"))
            .and_then(Value::as_bool)
            == Some(true);
        if !inherited {
            violations.push(format!(
                "{rel}: [package].version must be `version.workspace = true`"
            ));
        }
    }
    Ok(manifests.len())
}

fn scan_version_literals(
    workspace: &Path,
    version: &str,
    violations: &mut Vec<String>,
) -> Result<usize, String> {
    let mut scanned = 0;
    for root in [".github", "crates", "docs", "xtask"] {
        let path = workspace.join(root);
        if path.exists() {
            scan_path(workspace, &path, version, violations, &mut scanned)?;
        }
    }
    let readme = workspace.join("README.md");
    if readme.is_file() {
        scan_file(workspace, &readme, version, violations, &mut scanned)?;
    }
    Ok(scanned)
}

fn scan_path(
    workspace: &Path,
    path: &Path,
    version: &str,
    violations: &mut Vec<String>,
    scanned: &mut usize,
) -> Result<(), String> {
    let rel = relative(workspace, path);
    if path.is_dir() {
        if rel == "docs/research" || path.file_name().is_some_and(|name| name == "target") {
            return Ok(());
        }
        let entries = fs::read_dir(path).map_err(|err| format!("{}: {err}", path.display()))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("{}: {err}", path.display()))?;
            scan_path(workspace, &entry.path(), version, violations, scanned)?;
        }
    } else if path.is_file() {
        scan_file(workspace, path, version, violations, scanned)?;
    }
    Ok(())
}

fn scan_file(
    workspace: &Path,
    path: &Path,
    version: &str,
    violations: &mut Vec<String>,
    scanned: &mut usize,
) -> Result<(), String> {
    let rel = relative(workspace, path);
    if PRODUCT_VERSION_ALLOWED.contains(&rel.as_str()) {
        return Ok(());
    }
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    *scanned += 1;
    if contains_version_literal(&text, version) {
        violations.push(format!(
            "{rel}: product version `{version}` must live only in Cargo.toml, Cargo.lock, or release notes"
        ));
    }
    Ok(())
}

fn contains_version_literal(text: &str, version: &str) -> bool {
    let mut start = 0;
    while let Some(offset) = text[start..].find(version) {
        let begin = start + offset;
        let end = begin + version.len();
        let before = text[..begin].chars().next_back();
        let after = text[end..].chars().next();
        let boundary = |ch: Option<char>| ch.is_none_or(|ch| !ch.is_ascii_digit() && ch != '.');
        if boundary(before) && boundary(after) {
            return true;
        }
        start = end;
    }
    false
}

fn read_toml(path: &Path) -> Result<Table, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    text.parse::<Table>()
        .map_err(|err| format!("{}: invalid TOML: {err}", path.display()))
}

fn relative(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(version: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            format!(
                "[workspace]\nresolver = \"3\"\n[workspace.package]\nversion = \"{version}\"\n"
            ),
        )
        .expect("root manifest");
        fs::create_dir_all(temp.path().join("crates/demo/src")).expect("crate dir");
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion.workspace = true\n",
        )
        .expect("member manifest");
        temp
    }

    #[test]
    fn inherited_versions_and_allowed_literals_pass() {
        let temp = workspace("1.2.3");
        fs::create_dir_all(temp.path().join("docs/engineering")).expect("docs");
        fs::write(
            temp.path().join("docs/engineering/release.md"),
            "current 1.2.3\n",
        )
        .expect("release notes");
        check_versions(temp.path()).expect("passes");
    }

    #[test]
    fn product_literal_outside_allowed_files_fails() {
        let temp = workspace("1.2.3");
        fs::write(temp.path().join("README.md"), "Cairn v1.2.3\n").expect("readme");
        let err = check_versions(temp.path()).expect_err("literal fails");
        assert!(err.contains("README.md"), "{err}");
        assert!(err.contains("product version"), "{err}");
    }

    #[test]
    fn version_boundaries_do_not_match_longer_numbers() {
        let temp = workspace("1.2.3");
        fs::write(temp.path().join("README.md"), "10.2.3 and 1.2.30\n").expect("readme");
        check_versions(temp.path()).expect("boundaries pass");
    }

    #[test]
    fn member_without_workspace_version_fails() {
        let temp = workspace("1.2.3");
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n",
        )
        .expect("member manifest");
        let err = check_versions(temp.path()).expect_err("inheritance fails");
        assert!(err.contains("version.workspace = true"), "{err}");
    }
}

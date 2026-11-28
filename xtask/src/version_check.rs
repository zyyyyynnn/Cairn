// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Version governance gate: keeps the product version in one place, requires every workspace
//! member to inherit it, rejects the current product version outside the manifests/lockfile/
//! changelog, validates MSI numeric fields, and checks the release tag when one is present.

use std::fs;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use xtask::versioning::{msi_version, release_tag_from_env, validate_release_tag};

const PRODUCT_VERSION_ALLOWED_FILES: &[&str] = &["Cargo.toml", "Cargo.lock", "CHANGELOG.md"];
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "target",
    "dist",
    ".codegraph",
    ".agents",
    "node_modules",
    "vendor",
    "docs/research",
];
const TEXT_EXTENSIONS: &[&str] = &[
    "rs",
    "toml",
    "md",
    "yml",
    "yaml",
    "json",
    "ftl",
    "txt",
    "manifest",
    "svg",
    "gitignore",
    "gitattributes",
];

pub(crate) fn check_versions(workspace: &Path) -> Result<usize, String> {
    let root = read_toml(&workspace.join("Cargo.toml"))?;
    let version = workspace_version(&root)?;
    msi_version(&version)?;
    let mut violations = Vec::new();
    let members = check_member_manifests(workspace, &root, &mut violations)?;
    let scanned = scan_version_literals(workspace, &version, &mut violations)?;
    if let Some(tag) = release_tag_from_env()? {
        validate_release_tag(Some(&tag), &version)?;
    }

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

fn check_member_manifests(
    workspace: &Path,
    root: &Table,
    violations: &mut Vec<String>,
) -> Result<usize, String> {
    let manifests = workspace_members(workspace, root)?;
    for manifest in &manifests {
        let rel = relative(workspace, manifest);
        if !manifest.is_file() {
            violations.push(format!("{rel}: workspace member manifest is missing"));
            continue;
        }
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

fn workspace_members(workspace: &Path, root: &Table) -> Result<Vec<PathBuf>, String> {
    if let Some(members) = root
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
    {
        return Ok(members
            .iter()
            .filter_map(Value::as_str)
            .map(|member| workspace.join(member).join("Cargo.toml"))
            .collect());
    }
    let mut manifests = Vec::new();
    let crates = workspace.join("crates");
    if crates.is_dir() {
        for entry in fs::read_dir(&crates).map_err(|err| format!("{}: {err}", crates.display()))? {
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
    Ok(manifests)
}

fn scan_version_literals(
    workspace: &Path,
    version: &str,
    violations: &mut Vec<String>,
) -> Result<usize, String> {
    scan_path(workspace, workspace, version, violations)
}

fn scan_path(
    workspace: &Path,
    path: &Path,
    version: &str,
    violations: &mut Vec<String>,
) -> Result<usize, String> {
    let rel = relative(workspace, path);
    let metadata =
        fs::symlink_metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_dir() {
        if SKIPPED_DIRS
            .iter()
            .any(|skipped| rel == *skipped || rel.starts_with(&format!("{skipped}/")))
            || path.file_name().is_some_and(|name| name == "target")
        {
            return Ok(0);
        }
        let mut scanned = 0;
        for entry in fs::read_dir(path).map_err(|err| format!("{}: {err}", path.display()))? {
            let entry = entry.map_err(|err| format!("{}: {err}", path.display()))?;
            scanned += scan_path(workspace, &entry.path(), version, violations)?;
        }
        return Ok(scanned);
    }
    if !metadata.is_file() {
        return Ok(0);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if PRODUCT_VERSION_ALLOWED_FILES.contains(&name) {
        return Ok(0);
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if !extension.is_some_and(|extension| TEXT_EXTENSIONS.contains(&extension.as_str())) {
        return Ok(0);
    }
    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let text = decode_text(&bytes)
        .map_err(|err| format!("{}: unreadable text file: {err}", path.display()))?;
    if text
        .lines()
        .any(|line| product_context(line) && contains_version_literal(line, version))
    {
        violations.push(format!(
            "{rel}: product version `{version}` must live only in Cargo.toml, Cargo.lock, or CHANGELOG.md"
        ));
    }
    Ok(1usize)
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if rest.len() % 2 != 0 {
            return Err("odd-length UTF-16LE input".to_string());
        }
        let units: Vec<u16> = (0..rest.len() / 2)
            .map(|index| u16::from_le_bytes([rest[index * 2], rest[index * 2 + 1]]))
            .collect();
        return String::from_utf16(&units).map_err(|err| err.to_string());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if rest.len() % 2 != 0 {
            return Err("odd-length UTF-16BE input".to_string());
        }
        let units: Vec<u16> = (0..rest.len() / 2)
            .map(|index| u16::from_be_bytes([rest[index * 2], rest[index * 2 + 1]]))
            .collect();
        return String::from_utf16(&units).map_err(|err| err.to_string());
    }
    String::from_utf8(bytes.to_vec()).map_err(|err| err.to_string())
}

fn product_context(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("cairn")
        || lower.contains("product version")
        || lower.contains("release")
        || lower.contains("tag ")
        || lower.contains("current version")
}

fn contains_version_literal(text: &str, version: &str) -> bool {
    let mut start = 0;
    while let Some(offset) = text[start..].find(version) {
        let begin = start + offset;
        let end = begin + version.len();
        let before = text[..begin].chars().next_back();
        let after = text[end..].chars().next();
        let before_ok = before.is_none_or(|ch| !(ch.is_ascii_digit() || ch == '.'));
        let after_ok = match after {
            None => true,
            Some(ch) if ch.is_ascii_digit() => false,
            Some('.') => text[end + 1..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_ascii_digit()),
            Some(_) => true,
        };
        if before_ok && after_ok {
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

    fn v() -> String {
        [1u64, 2, 3]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    fn workspace(version: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = [\"crates/demo\"]\n[workspace.package]\nversion = \"{version}\"\n"
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
        fs::write(temp.path().join("CHANGELOG.md"), "1.2.3\n").expect("changelog");
        check_versions(temp.path()).expect("passes");
    }

    #[test]
    fn product_literal_outside_allowed_files_fails() {
        let temp = workspace("1.2.3");
        fs::write(temp.path().join("README.md"), format!("Cairn v{}\n", v())).expect("readme");
        let err = check_versions(temp.path()).expect_err("literal fails");
        assert!(err.contains("README.md"), "{err}");
        assert!(err.contains("product version"), "{err}");
    }

    #[test]
    fn external_dependency_versions_do_not_match_product_context() {
        let temp = workspace("1.2.3");
        fs::write(
            temp.path().join("README.md"),
            "gpui-kit 1.2.3\n10.2.3 and 1.2.30\n",
        )
        .expect("readme");
        check_versions(temp.path()).expect("external versions pass");
    }

    #[test]
    fn sentence_period_and_longer_versions_are_handled() {
        let temp = workspace("1.2.3");
        fs::write(temp.path().join("README.md"), format!("Cairn v{}.\n", v())).expect("readme");
        let err = check_versions(temp.path()).expect_err("sentence period fails");
        assert!(err.contains("README.md"), "{err}");

        let temp = workspace("1.2.3");
        fs::write(temp.path().join("README.md"), format!("Cairn v{}0\n", v())).expect("readme");
        check_versions(temp.path()).expect("longer version passes");
    }

    #[test]
    fn github_templates_and_other_root_docs_are_scanned() {
        let temp = workspace("1.2.3");
        fs::create_dir_all(temp.path().join(".github/ISSUE_TEMPLATE")).expect("github");
        fs::write(
            temp.path().join(".github/ISSUE_TEMPLATE/bug_report.yml"),
            format!("placeholder: Cairn v{}\n", v()),
        )
        .expect("template");
        let err = check_versions(temp.path()).expect_err("template fails");
        assert!(err.contains("bug_report.yml"), "{err}");

        let temp = workspace("1.2.3");
        fs::write(
            temp.path().join("CONTRIBUTING.md"),
            format!("Current version: {}\n", v()),
        )
        .expect("contributing");
        let err = check_versions(temp.path()).expect_err("root doc fails");
        assert!(err.contains("CONTRIBUTING.md"), "{err}");
    }

    #[test]
    fn docs_research_is_external_evidence() {
        let temp = workspace("1.2.3");
        fs::create_dir_all(temp.path().join("docs/research")).expect("research");
        fs::write(
            temp.path().join("docs/research/everything.md"),
            format!("Cairn v{}\n", v()),
        )
        .expect("research");
        check_versions(temp.path()).expect("research excluded");
    }

    #[test]
    fn utf16_text_is_scanned() {
        let temp = workspace("1.2.3");
        let units: Vec<u16> = format!("Cairn v{}\n", v()).encode_utf16().collect();
        let mut bytes = vec![0xFF, 0xFE];
        for unit in units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(temp.path().join("NOTES.md"), bytes).expect("utf16");
        let err = check_versions(temp.path()).expect_err("utf16 scanned");
        assert!(err.contains("NOTES.md"), "{err}");
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

    #[test]
    fn members_outside_crates_are_checked() {
        let temp = workspace("1.2.3");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/demo\", \"tools/tool\"]\n[workspace.package]\nversion = \"1.2.3\"\n",
        )
        .expect("root manifest");
        fs::create_dir_all(temp.path().join("tools/tool")).expect("tools");
        fs::write(
            temp.path().join("tools/tool/Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"1.2.3\"\n",
        )
        .expect("member");
        let err = check_versions(temp.path()).expect_err("outside member fails");
        assert!(err.contains("tools/tool/Cargo.toml"), "{err}");
    }
}

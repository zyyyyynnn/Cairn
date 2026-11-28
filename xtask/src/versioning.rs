// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared version identities and MSI/release-tag validation for the build tooling.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use toml::Value;

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Discovers workspace member manifests, including path-dependency auto-members.
pub fn workspace_members(workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let root_path = workspace.join("Cargo.toml");
    let root = read_toml(&root_path)?;
    let root_canonical = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let excluded = excluded_paths(workspace, &root, &root_canonical);
    let mut queue = Vec::new();
    if let Some(members) = root
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
    {
        for member in members.iter().filter_map(Value::as_str) {
            if member.contains('*') {
                return Err(format!(
                    "Cargo.toml: workspace member glob `{member}` is not supported by the gates"
                ));
            }
            queue.push(normalize_path(&workspace.join(member).join("Cargo.toml")));
        }
    }
    if let Some(dependencies) = root
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Value::as_table)
    {
        queue_dependencies(
            &root_path,
            [dependencies],
            &root_canonical,
            &excluded,
            &mut queue,
        );
    }
    if queue.is_empty() {
        let crates = workspace.join("crates");
        if crates.is_dir() {
            for entry in
                fs::read_dir(&crates).map_err(|err| format!("{}: {err}", crates.display()))?
            {
                let entry = entry.map_err(|err| format!("{}: {err}", crates.display()))?;
                let manifest = normalize_path(&entry.path().join("Cargo.toml"));
                if manifest.is_file() {
                    queue.push(manifest);
                }
            }
        }
        let xtask = normalize_path(&workspace.join("xtask/Cargo.toml"));
        if xtask.is_file() {
            queue.push(xtask);
        }
    }

    let mut seen = BTreeSet::new();
    let mut manifests = Vec::new();
    while let Some(manifest) = queue.pop() {
        let canonical = fs::canonicalize(&manifest).unwrap_or_else(|_| manifest.clone());
        if !seen.insert(canonical) {
            continue;
        }
        if !manifest.is_file() {
            manifests.push(manifest);
            continue;
        }
        manifests.push(manifest.clone());
        let doc = read_toml(&manifest)?;
        queue_dependencies(
            &manifest,
            dependency_tables(&doc),
            &root_canonical,
            &excluded,
            &mut queue,
        );
    }
    manifests.sort();
    manifests.dedup();
    Ok(manifests)
}

fn dependency_tables(doc: &toml::Table) -> Vec<&toml::Table> {
    let mut tables = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(Value::as_table) {
            tables.push(table);
        }
    }
    if let Some(targets) = doc.get("target").and_then(Value::as_table) {
        for target in targets.values().filter_map(Value::as_table) {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = target.get(section).and_then(Value::as_table) {
                    tables.push(table);
                }
            }
        }
    }
    tables
}

fn queue_dependencies<'a>(
    manifest: &Path,
    tables: impl IntoIterator<Item = &'a toml::Table>,
    root_canonical: &Path,
    excluded: &[PathBuf],
    queue: &mut Vec<PathBuf>,
) {
    let parent = manifest.parent().expect("manifest has a parent");
    for table in tables {
        for dependency in table.values() {
            let Some(path) = dependency
                .as_table()
                .and_then(|dependency| dependency.get("path"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let dependency_manifest = normalize_path(&parent.join(path).join("Cargo.toml"));
            if !dependency_manifest.is_file() {
                continue;
            }
            let dependency_canonical = fs::canonicalize(&dependency_manifest)
                .unwrap_or_else(|_| dependency_manifest.clone());
            if !dependency_canonical.starts_with(root_canonical)
                || excluded
                    .iter()
                    .any(|excluded| dependency_canonical.starts_with(excluded))
            {
                continue;
            }
            queue.push(dependency_manifest);
        }
    }
}

fn excluded_paths(workspace: &Path, root: &toml::Table, root_canonical: &Path) -> Vec<PathBuf> {
    root.get("workspace")
        .and_then(|workspace| workspace.get("exclude"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|path| {
            let candidate = workspace.join(path);
            fs::canonicalize(&candidate).unwrap_or(candidate)
        })
        .filter(|path| path.starts_with(root_canonical))
        .collect()
}

fn read_toml(path: &Path) -> Result<toml::Table, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    text.parse()
        .map_err(|err| format!("{}: invalid TOML: {err}", path.display()))
}

/// Validates the workspace version accepted by MSI `ProductVersion`.
pub fn msi_version(version: &str) -> Result<&str, String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "workspace version `{version}` must be MAJOR.MINOR.PATCH numeric for MSI ProductVersion"
        ));
    }
    let mut values = [0u32; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || !part.chars().all(|ch| ch.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return Err(format!(
                "workspace version `{version}` must be MAJOR.MINOR.PATCH numeric without leading zeros"
            ));
        }
        values[index] = part
            .parse()
            .map_err(|_| format!("workspace version `{version}` has an invalid component"))?;
    }
    if values[0] > 255 || values[1] > 255 || values[2] > 65535 {
        return Err(format!(
            "workspace version `{version}` exceeds MSI limits (major/minor <= 255, build <= 65535)"
        ));
    }
    Ok(version)
}

/// Returns the GitHub tag name when the build runs for a tag ref.
pub fn release_tag_from_env() -> Result<Option<String>, String> {
    if std::env::var("GITHUB_REF_TYPE").ok().as_deref() != Some("tag") {
        return Ok(None);
    }
    std::env::var("GITHUB_REF_NAME")
        .map(Some)
        .map_err(|_| "GITHUB_REF_TYPE=tag without GITHUB_REF_NAME".to_string())
}

/// Requires a present tag to be exactly `v{version}`.
pub fn validate_release_tag(tag: Option<&str>, version: &str) -> Result<(), String> {
    if let Some(tag) = tag {
        let expected = format!("v{version}");
        if tag != expected {
            return Err(format!(
                "release tag `{tag}` does not match workspace version `{version}` (expected `{expected}`)"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_version(parts: &[u64]) -> String {
        parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    #[test]
    fn msi_version_limits_and_forms() {
        for valid in [text_version(&[0, 2, 1]), text_version(&[255, 255, 65535])] {
            assert_eq!(msi_version(&valid).expect("valid"), valid);
        }
        for bad in [
            text_version(&[0, 2]),
            format!("{}-beta", text_version(&[0, 2, 1])),
            text_version(&[256, 0, 0]),
            text_version(&[0, 2, 70000]),
            format!("0{}.2.1", "1"),
        ] {
            assert!(msi_version(&bad).is_err(), "{bad} must fail");
        }
    }

    #[test]
    fn release_tags_match_the_workspace_version() {
        let version = text_version(&[0, 2, 1]);
        validate_release_tag(None, &version).expect("no tag");
        validate_release_tag(Some(&format!("v{version}")), &version).expect("matching tag");
        assert!(validate_release_tag(Some("v9.9.9"), &version).is_err());
    }

    #[test]
    fn workspace_members_follow_path_dependencies() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/demo\"]\nexclude = [\"tools/excluded\"]\n[workspace.dependencies]\nroot_tool = { path = \"tools/root_tool\" }\n",
        )
        .expect("root");
        fs::create_dir_all(temp.path().join("crates/demo/src")).expect("demo");
        for tool in ["tool", "target_tool", "root_tool", "excluded"] {
            fs::create_dir_all(temp.path().join(format!("tools/{tool}/src"))).expect(tool);
            fs::write(
                temp.path().join(format!("tools/{tool}/Cargo.toml")),
                format!("[package]\nname = \"{tool}\"\n"),
            )
            .expect("tool manifest");
        }
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"demo\"\n\n[dependencies]\ntool = { path = \"../../tools/tool\" }\nexcluded = { path = \"../../tools/excluded\" }\n\n[target.'cfg(windows)'.dependencies]\ntarget_tool = { path = \"../../tools/target_tool\" }\n",
        )
        .expect("demo manifest");

        let members = workspace_members(temp.path()).expect("members");
        let names: Vec<String> = members
            .iter()
            .map(|manifest| {
                manifest
                    .strip_prefix(temp.path())
                    .expect("member")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(names.len(), 4, "{names:?}");
        for expected in [
            "crates/demo/Cargo.toml",
            "tools/tool/Cargo.toml",
            "tools/target_tool/Cargo.toml",
            "tools/root_tool/Cargo.toml",
        ] {
            assert!(names.iter().any(|name| name == expected), "{names:?}");
        }
        assert!(
            !names.iter().any(|name| name.contains("excluded")),
            "{names:?}"
        );
    }
}

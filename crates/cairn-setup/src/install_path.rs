// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The install-folder decision shared by the setup UI and the MSI guard. The setup validates the
//! path a person typed; the guard revalidates the property the MSI actually received, because the
//! published MSI can be run without this UI.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathProblem {
    Empty,
    NotAbsolute,
    Network,
    Root,
    System,
    OutsideProgramFiles,
    File,
    Invalid,
}

impl PathProblem {
    pub fn message_key(self) -> &'static str {
        match self {
            PathProblem::Empty => "path_empty",
            PathProblem::NotAbsolute => "path_not_absolute",
            PathProblem::Network => "path_network",
            PathProblem::Root => "path_root",
            PathProblem::System => "path_system",
            PathProblem::OutsideProgramFiles => "path_outside_program_files",
            PathProblem::File => "path_file",
            PathProblem::Invalid => "path_invalid",
        }
    }
}

/// Validates a folder against the administrator-owned root it must live under. It rejects only
/// paths that cannot be a sensible machine-wide product folder; an existing non-empty folder is
/// allowed because upgrades reuse it.
pub fn validate_install_path_under(
    raw: &str,
    program_files: &Path,
) -> Result<PathBuf, PathProblem> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(PathProblem::Empty);
    }
    if raw.starts_with(r"\\") || raw.starts_with("//") {
        return Err(PathProblem::Network);
    }
    if !Path::new(raw).is_absolute() {
        return Err(PathProblem::NotAbsolute);
    }
    if raw
        .chars()
        .any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(PathProblem::Invalid);
    }
    // A colon is legal only as the drive prefix of an absolute path.
    if raw[2..].contains(':') {
        return Err(PathProblem::Invalid);
    }

    let trimmed = raw.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return Err(PathProblem::Root);
    }
    let path = PathBuf::from(trimmed);
    if path.parent().is_none() {
        return Err(PathProblem::Root);
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(PathProblem::Invalid),
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                if text.ends_with('.') || text.ends_with(' ') || text.contains([';', '%']) {
                    return Err(PathProblem::Invalid);
                }
                normalized.push(part);
            }
        }
    }
    if normalized.parent().is_none() {
        return Err(PathProblem::Root);
    }

    let lower = normalized.to_string_lossy().to_ascii_lowercase();
    if lower == r"c:\windows"
        || lower.starts_with(r"c:\windows\")
        || lower == r"c:\program files"
        || lower == r"c:\program files (x86)"
        || lower == r"c:\programdata"
    {
        return Err(PathProblem::System);
    }

    // A machine-wide service and PATH entry must live under an administrator-owned root. Allowing
    // a user-writable folder would let a standard user replace the service binary after elevation.
    if !is_under(&normalized, program_files) {
        return Err(PathProblem::OutsideProgramFiles);
    }

    if normalized.is_file() {
        return Err(PathProblem::File);
    }
    Ok(normalized)
}

fn is_under(path: &Path, root: &Path) -> bool {
    let root = root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase();
    let path = path
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase();
    path.len() > root.len()
        && path.starts_with(&root)
        && path.as_bytes().get(root.len()) == Some(&b'\\')
}

#[cfg(test)]
mod tests {
    use super::{validate_install_path_under, PathProblem};
    use std::path::PathBuf;

    #[test]
    fn a_path_under_a_custom_root_is_accepted_case_insensitively() {
        let root = PathBuf::from(r"D:\Program Files");
        assert_eq!(
            validate_install_path_under(r"D:\PROGRAM FILES\Cairn\", &root),
            Ok(PathBuf::from(r"D:\PROGRAM FILES\Cairn"))
        );
    }

    #[test]
    fn a_path_outside_the_supplied_root_is_rejected() {
        let root = PathBuf::from(r"D:\Program Files");
        assert_eq!(
            validate_install_path_under(r"C:\Program Files\Cairn", &root),
            Err(PathProblem::OutsideProgramFiles)
        );
        assert_eq!(
            validate_install_path_under(r"D:\Program Files", &root),
            Err(PathProblem::OutsideProgramFiles)
        );
    }

    #[test]
    fn path_components_cannot_escape_or_carry_shell_metacharacters() {
        let root = PathBuf::from(r"C:\Program Files");
        for raw in [
            r"C:\Program Files\..\Windows",
            r"C:\Program Files\Cairn;C:\Tools",
            r"C:\Program Files\Cairn.",
            r"C:\Program Files\bad?name",
        ] {
            assert_eq!(
                validate_install_path_under(raw, &root),
                Err(PathProblem::Invalid),
                "{raw}"
            );
        }
    }
}

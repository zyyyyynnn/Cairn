// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Manual path entry for the Treemap address field: turns what a person typed
//! into the absolute, drive-rooted directory the canvas can open.

use std::path::{Component, Path, PathBuf};

/// Resolves `input` against `current` and normalizes it to `D:\dir\dir` form.
///
/// The result is syntactic only: the service decides whether the path is
/// indexed, and an uncovered path lands on the canvas's unavailable state.
/// Returned `None` means the text cannot name a drive path at all (empty, a
/// UNC share, a drive-relative `C:dir`, or a bad quote pair).
pub(crate) fn normalize_treemap_path(input: &str, current: &str) -> Option<String> {
    let trimmed = input.trim();
    if let Some(quote) = trimmed
        .chars()
        .next()
        .filter(|quote| *quote == '"' || *quote == '\'')
    {
        if trimmed.len() < 2 || !trimmed.ends_with(quote) {
            return None;
        }
    }
    let input = strip_matching_quotes(trimmed);
    if input.is_empty() {
        return None;
    }
    if input.len() == 2 {
        let bytes = input.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()));
        }
    }

    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        Path::new(current).join(input)
    };

    let mut drive = None;
    let mut rooted = false;
    let mut segments: Vec<String> = Vec::new();
    for component in candidate.components() {
        match component {
            Component::Prefix(prefix) => {
                let text = prefix.as_os_str().to_string_lossy();
                let mut characters = text.chars();
                let letter = characters.next()?;
                if !letter.is_ascii_alphabetic()
                    || characters.next() != Some(':')
                    || characters.next().is_some()
                {
                    return None;
                }
                drive = Some(letter.to_ascii_uppercase());
            }
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                segments.pop();
            }
            Component::Normal(name) => {
                // Win32 drops trailing dots and spaces from every segment, so
                // "C:\Windows." opens the same directory as "C:\Windows". The
                // index lookup compares names exactly, so the field must store
                // the canonical spelling or every drill would miss the cache.
                let name = name.to_string_lossy();
                let name = name.trim_end_matches([' ', '.']);
                if name.is_empty() {
                    return None;
                }
                segments.push(name.to_string());
            }
        }
    }

    let drive = drive?;
    if !rooted {
        return None;
    }
    let mut normalized = format!("{drive}:\\");
    for (ix, segment) in segments.iter().enumerate() {
        if ix > 0 {
            normalized.push('\\');
        }
        normalized.push_str(segment);
    }
    Some(normalized)
}

fn strip_matching_quotes(input: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = input
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.trim();
        }
    }
    input
}

#[cfg(test)]
mod tests {
    use super::normalize_treemap_path;

    #[test]
    fn absolute_paths_normalize_drive_case_separators_and_trailing_slash() {
        assert_eq!(
            normalize_treemap_path(r"c:\users\", r"C:\"),
            Some(r"C:\users".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"C:/Program Files/", r"C:\"),
            Some(r"C:\Program Files".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"C:", r"C:\"),
            Some(r"C:\".to_string())
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_current_directory() {
        assert_eq!(
            normalize_treemap_path(r"Users", r"C:\"),
            Some(r"C:\Users".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"..\Windows", r"C:\Users\Alice"),
            Some(r"C:\Users\Windows".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"\Windows", r"C:\Users"),
            Some(r"C:\Windows".to_string())
        );
        assert_eq!(
            normalize_treemap_path(" . ", r"C:\Users"),
            Some(r"C:\Users".to_string())
        );
    }

    #[test]
    fn quoted_paths_and_dot_segments_are_cleaned() {
        assert_eq!(
            normalize_treemap_path(r#""C:\Program Files\..\Windows""#, r"C:\"),
            Some(r"C:\Windows".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"C:\a\.\b\..\c", r"C:\"),
            Some(r"C:\a\c".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"..\..", r"C:\"),
            Some(r"C:\".to_string())
        );
    }

    #[test]
    fn win32_trailing_dots_and_spaces_collapse_to_the_directory_name() {
        assert_eq!(
            normalize_treemap_path(r"C:\Windows. ", r"C:\"),
            Some(r"C:\Windows".to_string())
        );
        assert_eq!(
            normalize_treemap_path(r"C:\Users\Alice.\Folder", r"C:\"),
            Some(r"C:\Users\Alice\Folder".to_string())
        );
        assert_eq!(normalize_treemap_path(r"C:\...", r"C:\"), None);
    }

    #[test]
    fn an_unmatched_quote_is_not_a_directory_name() {
        assert_eq!(normalize_treemap_path("\"C:\\Windows", "C:\\"), None);
        assert_eq!(normalize_treemap_path("'C:\\Windows", "C:\\"), None);
    }

    #[test]
    fn non_drive_paths_are_rejected() {
        assert_eq!(normalize_treemap_path("", r"C:\"), None);
        assert_eq!(normalize_treemap_path("   ", r"C:\"), None);
        assert_eq!(normalize_treemap_path(r"\\server\share", r"C:\"), None);
        assert_eq!(normalize_treemap_path(r"C:Users", r"C:\"), None);
    }
}

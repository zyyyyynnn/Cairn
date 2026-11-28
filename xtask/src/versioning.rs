// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared version identities and MSI/release-tag validation for the build tooling.

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
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Explorer reveal targets for a multi-row selection.

use std::collections::HashSet;
use std::path::Path;

/// One Explorer target per unique parent directory, in selection order. Keeping the first file in
/// each directory lets `/select` highlight a real row while a multi-folder selection opens every
/// folder instead of silently showing only the focused one.
pub(crate) fn reveal_targets(paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for path in paths {
        let Some(parent) = Path::new(path).parent() else {
            continue;
        };
        let key = parent.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            targets.push(path.clone());
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_opens_each_selected_directory_once() {
        let paths = vec![
            r"C:\Alpha\one.txt".to_string(),
            r"C:\Alpha\two.txt".to_string(),
            r"C:\Beta\three.txt".to_string(),
            r"c:\alpha\four.txt".to_string(),
        ];
        assert_eq!(
            reveal_targets(&paths),
            vec![
                r"C:\Alpha\one.txt".to_string(),
                r"C:\Beta\three.txt".to_string()
            ],
            "the first file per case-insensitive parent is the Explorer target"
        );
    }
}

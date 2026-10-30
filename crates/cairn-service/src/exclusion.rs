// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What one exclusion rule means, and how a path answers it (IDX-09).
//!
//! A rule is read once and then answers two questions: does this whole path fall under it, and
//! does this single path segment fall under it. Both answers come from the same rule, so the
//! fast path that decides a whole volume and the full-path check that decides one match cannot
//! disagree about what a rule excludes.

use cairn_core::engine::glob_match;

/// One exclusion rule, read from the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExclusionRule {
    /// A sequence of path segments, e.g. `node_modules` or `target\debug`. A path falls under
    /// the rule when it contains that sequence of segments at a segment boundary: `C:\proj\
    /// node_modules` is excluded by `node_modules`, and `C:\proj\node_modules_cache` is not.
    Segments(Vec<String>),
    /// A pattern, e.g. `*.tmp`. Matched against the path as written, so the pattern's case
    /// counts the way the user's rule does.
    Glob(String),
    /// A drive-qualified prefix, e.g. `C:\Users`. A path falls under it when it starts with the
    /// prefix at a segment boundary.
    AbsolutePrefix(String),
}

impl ExclusionRule {
    /// Reads one rule. An empty rule excludes nothing and has no rule to ask.
    pub fn parse(rule: &str) -> Option<Self> {
        let trimmed = rule.trim();
        if trimmed.is_empty() {
            return None;
        }
        let normalized = trimmed.replace('/', "\\");
        if normalized.contains('*') || normalized.contains('?') {
            return Some(ExclusionRule::Glob(normalized));
        }
        let cleaned = normalized.trim_end_matches('\\');
        if cleaned.is_empty() {
            return None;
        }
        if cleaned.len() >= 2 && cleaned.as_bytes()[1] == b':' {
            return Some(ExclusionRule::AbsolutePrefix(cleaned.to_ascii_lowercase()));
        }
        let segments: Vec<String> = cleaned
            .split('\\')
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.to_ascii_lowercase())
            .collect();
        if segments.is_empty() {
            return None;
        }
        Some(ExclusionRule::Segments(segments))
    }

    /// The one segment this rule matches, when it matches exactly one.
    ///
    /// A rule that matches several segments, a pattern or an absolute prefix cannot be decided
    /// from a name alone, so the fast path only accepts the short ones.
    pub fn single_segment(&self) -> Option<&str> {
        match self {
            ExclusionRule::Segments(segments) if segments.len() == 1 => Some(&segments[0]),
            _ => None,
        }
    }

    /// Whether a whole path falls under this rule.
    pub fn excludes_path(&self, path: &ExclusionPath) -> bool {
        match self {
            ExclusionRule::Glob(pattern) => {
                glob_match(pattern, &path.normalized, false)
                    || glob_match(
                        &format!(r"**\{}", pattern.trim_start_matches(r"**\")),
                        &path.normalized,
                        false,
                    )
            }
            ExclusionRule::AbsolutePrefix(prefix) => {
                path.lower == *prefix
                    || (path.lower.starts_with(prefix.as_str())
                        && (prefix.ends_with('\\')
                            || path.lower.as_bytes().get(prefix.len()) == Some(&b'\\')))
            }
            ExclusionRule::Segments(segments) => path.contains_segments(segments),
        }
    }
}

/// A path, prepared once so every rule reads it the same way.
#[derive(Clone, Debug)]
pub struct ExclusionPath {
    /// The path as written, with forward slashes folded to backslashes.
    normalized: String,
    /// The same path in lower case, for the comparisons that ignore case.
    lower: String,
}

impl ExclusionPath {
    /// Prepares a path for the rules to read.
    pub fn new(path: &str) -> Self {
        let normalized = path.replace('/', "\\");
        let lower = normalized.to_ascii_lowercase();
        Self { normalized, lower }
    }

    /// Whether the path contains the given sequence of segments at a segment boundary.
    fn contains_segments(&self, segments: &[String]) -> bool {
        let path_segments: Vec<&str> = self.lower.split('\\').filter(|s| !s.is_empty()).collect();
        if segments.len() > path_segments.len() {
            return false;
        }
        path_segments.windows(segments.len()).any(|window| {
            window
                .iter()
                .zip(segments)
                .all(|(part, want)| *part == want.as_str())
        })
    }
}

/// The whole rule set, read once and then asked about many paths.
#[derive(Clone, Debug, Default)]
pub struct ExclusionRules {
    rules: Vec<ExclusionRule>,
}

impl ExclusionRules {
    /// Reads every rule of a configuration. An empty rule is skipped: it excludes nothing.
    pub fn parse(rules: &[String]) -> Self {
        Self {
            rules: rules
                .iter()
                .filter_map(|rule| ExclusionRule::parse(rule))
                .collect(),
        }
    }

    /// Whether the rule set excludes nothing at all.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether a path falls under any rule.
    pub fn excludes(&self, path: &ExclusionPath) -> bool {
        self.rules.iter().any(|rule| rule.excludes_path(path))
    }

    /// The one segment each rule matches, when every rule matches exactly one segment.
    ///
    /// The fast path that decides a whole volume can only be built from such rules; anything
    /// else falls back to the full-path check, so both routes answer the same question.
    pub fn single_segments(&self) -> Option<Vec<String>> {
        self.rules
            .iter()
            .map(|rule| rule.single_segment().map(str::to_string))
            .collect()
    }
}

/// Whether a path falls under any of the configured rules (IDX-09).
pub fn is_path_excluded(full_path: &str, rules: &[String]) -> bool {
    if rules.is_empty() || full_path.is_empty() {
        return false;
    }
    let parsed = ExclusionRules::parse(rules);
    if parsed.is_empty() {
        return false;
    }
    parsed.excludes(&ExclusionPath::new(full_path))
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{is_path_excluded, ExclusionPath, ExclusionRule, ExclusionRules};

    fn excluded(path: &str, rule: &str) -> bool {
        is_path_excluded(path, &[rule.to_string()])
    }

    /// A bare segment names a directory, so it excludes that directory and everything under it —
    /// and nothing that merely starts with the same letters.
    #[test]
    fn a_bare_segment_rule_matches_whole_segments_only() {
        assert!(excluded(r"C:\proj\node_modules\x.txt", "node_modules"));
        assert!(excluded(r"C:\proj\node_modules\x.txt", "NODE_MODULES"));
        assert!(
            !excluded(r"C:\proj\node_modules_cache\x.txt", "node_modules"),
            "a longer directory name is not the ruled directory"
        );
        assert!(!excluded(r"C:\proj\my_node_modules\x.txt", "node_modules"));
    }

    /// A rule with separators names a path fragment, and it has to land on a segment boundary on
    /// both sides.
    #[test]
    fn a_relative_path_rule_matches_whole_segment_sequences() {
        assert!(excluded(r"C:\proj\target\debug\x.txt", r"target\debug"));
        assert!(excluded(r"C:\proj\target\debug", r"target\debug"));
        assert!(
            !excluded(r"C:\proj\target\debugger\x.txt", r"target\debug"),
            "a longer directory name is not the ruled path"
        );
    }

    /// A drive-qualified rule is a prefix, and it stops at a segment boundary.
    #[test]
    fn an_absolute_rule_matches_whole_path_prefixes() {
        assert!(excluded(r"C:\Users\alice\a.txt", r"C:\Users"));
        assert!(excluded(r"C:\Users", r"C:\Users"));
        assert!(!excluded(r"C:\UsersData\alice\a.txt", r"C:\Users"));
    }

    /// A pattern is matched against the path as written.
    #[test]
    fn a_pattern_rule_matches_the_path_as_written() {
        assert!(excluded(r"C:\proj\a.tmp", "*.tmp"));
        assert!(!excluded(r"C:\proj\a.txt", "*.tmp"));
    }

    /// An empty rule excludes nothing, so it never turns a result set into an empty one.
    #[test]
    fn an_empty_rule_excludes_nothing() {
        assert!(!excluded(r"C:\proj\a.txt", ""));
        assert!(!excluded(r"C:\proj\a.txt", "   "));
        assert!(ExclusionRules::parse(&[String::new()]).is_empty());
    }

    /// The fast path and the full-path check read the same rule, so they must answer the same
    /// question.
    #[test]
    fn the_fast_path_and_the_full_path_check_agree_on_every_rule() {
        let rules = vec![
            "node_modules".to_string(),
            r"target\debug".to_string(),
            "*.tmp".to_string(),
            r"C:\Users".to_string(),
        ];
        let parsed = ExclusionRules::parse(&rules);

        // The fast path only builds from rules that match one segment, so a mixed set falls
        // back to the full-path check.
        assert!(
            parsed.single_segments().is_none(),
            "a rule set that mixes segment, path, pattern and prefix rules cannot use the fast path"
        );

        for path in [
            r"C:\proj\node_modules\a.txt",
            r"C:\proj\node_modules_cache\a.txt",
            r"C:\proj\target\debug\a.txt",
            r"C:\proj\a.tmp",
            r"C:\Users\alice\a.txt",
        ] {
            let full_path = ExclusionPath::new(path);
            assert_eq!(
                parsed.excludes(&full_path),
                is_path_excluded(path, &rules),
                "the two routes disagree about {path}"
            );
        }

        let bare = ExclusionRules::parse(&["node_modules".to_string()]);
        assert_eq!(
            bare.single_segments(),
            Some(vec!["node_modules".to_string()])
        );
    }

    /// A rule that matches one segment can be decided from a name alone.
    #[test]
    fn a_single_segment_rule_answers_from_the_name_alone() {
        fn single_segment_of(rule: &str) -> Option<String> {
            ExclusionRule::parse(rule).and_then(|rule| rule.single_segment().map(str::to_string))
        }
        assert_eq!(
            single_segment_of("node_modules"),
            Some("node_modules".to_string())
        );
        assert_eq!(single_segment_of(r"target\debug"), None);
        assert_eq!(single_segment_of("*.tmp"), None);
        assert_eq!(single_segment_of(r"C:\Users"), None);
    }
}

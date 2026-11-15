//! Find-in-results matching shared by the service scan and the client highlight, so both
//! sides decide "is this row a match" with one implementation.

use crate::engine::glob_match;
use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FindMode {
    Substring,
    Wildcard,
    Regex,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FindError {
    EmptyPattern,
    Regex(String),
}

pub struct FindPattern {
    mode: FindMode,
    case_sensitive: bool,
    text: String,
    regex: Option<Regex>,
}

impl FindPattern {
    pub fn compile(
        pattern: &str,
        mode: FindMode,
        case_sensitive: bool,
    ) -> Result<Self, FindError> {
        if pattern.is_empty() {
            return Err(FindError::EmptyPattern);
        }
        let regex = match mode {
            FindMode::Regex => Some(
                RegexBuilder::new(pattern)
                    .case_insensitive(!case_sensitive)
                    .build()
                    .map_err(|e| FindError::Regex(e.to_string()))?,
            ),
            _ => None,
        };
        Ok(Self {
            mode,
            case_sensitive,
            text: pattern.to_string(),
            regex,
        })
    }

    pub fn is_match(&self, text: &str) -> bool {
        match self.mode {
            FindMode::Substring => {
                if self.case_sensitive {
                    text.contains(&self.text)
                } else {
                    text.to_lowercase().contains(&self.text.to_lowercase())
                }
            }
            FindMode::Wildcard => glob_match(&self.text, text, self.case_sensitive),
            FindMode::Regex => self.regex.as_ref().is_some_and(|r| r.is_match(text)),
        }
    }

    pub fn matches_row(&self, name: &str, path: &str) -> bool {
        self.is_match(name) || self.is_match(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_matches_name_or_path_case_insensitively_by_default() {
        let p = FindPattern::compile("report", FindMode::Substring, false).unwrap();
        assert!(p.matches_row("Annual REPORT.pdf", "C:\\docs"));
        assert!(p.matches_row("notes.txt", "C:\\Reports\\2026"));
        assert!(!p.matches_row("notes.txt", "C:\\docs"));
    }

    #[test]
    fn substring_case_sensitive_respects_case() {
        let p = FindPattern::compile("Report", FindMode::Substring, true).unwrap();
        assert!(p.is_match("Report.pdf"));
        assert!(!p.is_match("report.pdf"));
    }

    #[test]
    fn wildcard_uses_star_and_question() {
        let p = FindPattern::compile("rep?rt*.pdf", FindMode::Wildcard, false).unwrap();
        assert!(p.is_match("REPORT-2026.pdf"));
        assert!(!p.is_match("report.docx"));
    }

    #[test]
    fn regex_anchors_and_case_flag() {
        let p = FindPattern::compile("^report\\d+$", FindMode::Regex, false).unwrap();
        assert!(p.is_match("Report7"));
        assert!(!p.is_match("xreport7"));
        let sensitive = FindPattern::compile("^report$", FindMode::Regex, true).unwrap();
        assert!(!sensitive.is_match("Report"));
    }

    #[test]
    fn invalid_regex_is_an_error_not_a_panic() {
        assert!(matches!(
            FindPattern::compile("([", FindMode::Regex, false),
            Err(FindError::Regex(_))
        ));
    }

    #[test]
    fn empty_pattern_is_refused() {
        assert!(matches!(
            FindPattern::compile("", FindMode::Substring, false),
            Err(FindError::EmptyPattern)
        ));
    }
}
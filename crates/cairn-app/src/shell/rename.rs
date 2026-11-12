// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-place rename rules and the editor's retained draft (UI-05).
//!
//! The write itself runs through the shell file operation ([`crate::shell::os::rename_path`]).
//! Everything here decides what may be submitted, and keeps a rejected or failed attempt open so
//! the person can correct it without reopening the editor.

use std::ops::Range;

/// Windows limits one name component to 255 UTF-16 code units.
pub const MAX_NAME_UTF16_UNITS: usize = 255;

/// Characters a Windows file name cannot contain, as Explorer reports them.
const INVALID_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// DOS device names are reserved in every directory, with or without an extension.
const RESERVED_STEMS: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why a proposed name cannot be written, or why the shell refused the write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameError {
    /// Empty, or only whitespace.
    Empty,
    /// The text still names the item exactly as it already is.
    Unchanged,
    /// A character Windows forbids in a name component.
    InvalidChar(char),
    /// The name ends in a dot or a space, which Windows strips.
    TrailingDotOrSpace,
    /// A DOS device name such as `CON` or `LPT1`, at any case and with any extension.
    ReservedName,
    /// More than [`MAX_NAME_UTF16_UNITS`] UTF-16 units.
    TooLong,
    /// Another item in the same directory already carries that name.
    TargetExists,
    /// The native operation was dismissed before it wrote anything.
    Cancelled,
    /// The shell refused the write. The diagnostic is kept for the toast and the log.
    Shell { message: String },
}

impl RenameError {
    /// The message that explains a rejected name. `None` for a dismissed operation (nothing to
    /// explain) and for a shell diagnosis, which is shown as it was reported.
    pub fn message_key(&self) -> Option<&'static str> {
        match self {
            Self::Empty => Some("rename_error_empty"),
            Self::Unchanged => Some("rename_error_unchanged"),
            Self::InvalidChar(_) => Some("rename_error_invalid_char"),
            Self::TrailingDotOrSpace => Some("rename_error_trailing_dot_or_space"),
            Self::ReservedName => Some("rename_error_reserved"),
            Self::TooLong => Some("rename_error_too_long"),
            Self::TargetExists => Some("rename_error_target_exists"),
            Self::Cancelled | Self::Shell { .. } => None,
        }
    }
}

/// The editor's retained state: the item it renames, the text being edited, and the last rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameDraft {
    /// Full path of the item before the write.
    pub path: String,
    /// The name the item carries now, so submitting it unchanged is caught before any write.
    pub current: String,
    /// The text the editor holds.
    pub text: String,
    /// The last rejection; cleared as soon as the text changes.
    pub error: Option<RenameError>,
}

impl RenameDraft {
    pub fn begin(path: impl Into<String>, current: impl Into<String>) -> Self {
        let current = current.into();
        Self {
            path: path.into(),
            text: current.clone(),
            current,
            error: None,
        }
    }

    /// Byte range of the name stem: everything before the last dot that has something after it.
    /// A leading dot and a trailing dot are part of the whole name, so `.gitignore` and `name.`
    /// select their entire text instead of an empty stem.
    pub fn stem_range(&self) -> Range<usize> {
        match self.current.rfind('.') {
            Some(dot) if dot > 0 && dot + 1 < self.current.len() => 0..dot,
            _ => 0..self.current.len(),
        }
    }

    /// Replaces the edited text and forgets the previous rejection.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.error = None;
    }

    /// Validates the current text. A rejection is kept on the draft so the editor can remain open
    /// and show it; the returned name is exactly the text to write.
    pub fn commit_candidate(&mut self) -> Result<String, RenameError> {
        let result = validate_name(&self.current, &self.text);
        self.error = result.as_ref().err().cloned();
        result
    }

    /// Records a failed write without discarding the text the person typed.
    pub fn fail(&mut self, error: RenameError) {
        self.error = Some(error);
    }
}

/// Checks one proposed name component against the rules Windows enforces. A legal name is returned
/// exactly as typed; the editor's text is never trimmed here.
pub fn validate_name(current: &str, proposed: &str) -> Result<String, RenameError> {
    if proposed.chars().all(char::is_whitespace) {
        return Err(RenameError::Empty);
    }
    if proposed == current {
        return Err(RenameError::Unchanged);
    }
    if let Some(invalid) = proposed
        .chars()
        .find(|ch| INVALID_CHARS.contains(ch) || ch.is_control())
    {
        return Err(RenameError::InvalidChar(invalid));
    }
    if proposed.ends_with('.') || proposed.ends_with(' ') {
        return Err(RenameError::TrailingDotOrSpace);
    }
    let stem = proposed.split('.').next().unwrap_or(proposed);
    if RESERVED_STEMS
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(RenameError::ReservedName);
    }
    if proposed.encode_utf16().count() > MAX_NAME_UTF16_UNITS {
        return Err(RenameError::TooLong);
    }
    Ok(proposed.to_string())
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{validate_name, RenameDraft, RenameError, MAX_NAME_UTF16_UNITS};

    #[test]
    fn a_name_must_differ_from_the_one_it_replaces() {
        assert_eq!(
            validate_name("notes.txt", "notes.txt"),
            Err(RenameError::Unchanged)
        );
        assert_eq!(
            validate_name("notes.txt", "NOTES.txt"),
            Ok("NOTES.txt".to_string()),
            "a case-only rename changes the displayed name"
        );
    }

    #[test]
    fn empty_and_whitespace_only_names_are_rejected() {
        for current in ["notes.txt", ""] {
            assert_eq!(validate_name(current, ""), Err(RenameError::Empty));
            assert_eq!(validate_name(current, "   "), Err(RenameError::Empty));
            assert_eq!(
                validate_name(current, "\t \u{3000}"),
                Err(RenameError::Empty)
            );
        }
    }

    #[test]
    fn windows_forbidden_characters_are_named() {
        for (proposed, invalid) in [
            ("a<b.txt", '<'),
            ("a>b.txt", '>'),
            ("a:b.txt", ':'),
            ("a\"b.txt", '"'),
            ("a/b.txt", '/'),
            ("a\\b.txt", '\\'),
            ("a|b.txt", '|'),
            ("a?b.txt", '?'),
            ("a*b.txt", '*'),
            ("a\u{7}b.txt", '\u{7}'),
        ] {
            assert_eq!(
                validate_name("notes.txt", proposed),
                Err(RenameError::InvalidChar(invalid)),
                "{proposed}"
            );
        }
    }

    #[test]
    fn a_trailing_dot_or_space_is_rejected() {
        assert_eq!(
            validate_name("notes.txt", "report."),
            Err(RenameError::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_name("notes.txt", "report "),
            Err(RenameError::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_name("notes.txt", "report. "),
            Err(RenameError::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_name("notes.txt", "report.v2"),
            Ok("report.v2".to_string())
        );
    }

    #[test]
    fn windows_device_names_are_reserved_at_any_case_and_extension() {
        for proposed in [
            "CON",
            "con",
            "Con.TXT",
            "PRN.log",
            "aux",
            "Nul.tar.gz",
            "COM1",
            "com9.txt",
            "LPT1",
            "lpt9.log",
        ] {
            assert_eq!(
                validate_name("notes.txt", proposed),
                Err(RenameError::ReservedName),
                "{proposed}"
            );
        }
        for legal in ["CONSOLE.txt", "COM0.txt", "LPT10.txt", "my.CON.txt"] {
            assert_eq!(
                validate_name("notes.txt", legal),
                Ok(legal.to_string()),
                "{legal}"
            );
        }
    }

    #[test]
    fn the_limit_counts_utf16_units() {
        let at_limit = "a".repeat(MAX_NAME_UTF16_UNITS);
        assert_eq!(validate_name("notes.txt", &at_limit), Ok(at_limit.clone()));
        let over_limit = "a".repeat(MAX_NAME_UTF16_UNITS + 1);
        assert_eq!(
            validate_name("notes.txt", &over_limit),
            Err(RenameError::TooLong)
        );
        let wide = "文".repeat(MAX_NAME_UTF16_UNITS + 1);
        assert_eq!(
            validate_name("notes.txt", &wide),
            Err(RenameError::TooLong),
            "one CJK character occupies one UTF-16 unit, but the byte length would lie"
        );
    }

    #[test]
    fn a_legal_name_is_returned_exactly_as_typed() {
        assert_eq!(
            validate_name("notes.txt", " report "),
            Err(RenameError::TrailingDotOrSpace),
            "the editor's own text is not trimmed into legality"
        );
        assert_eq!(
            validate_name("notes.txt", " leading space.txt"),
            Ok(" leading space.txt".to_string())
        );
        assert_eq!(
            validate_name("notes.txt", "文档 (1).md"),
            Ok("文档 (1).md".to_string())
        );
    }

    #[test]
    fn the_editor_starts_on_the_name_stem() {
        let draft = RenameDraft::begin(r"C:\work\report.final.docx", "report.final.docx");
        assert_eq!(draft.text, "report.final.docx");
        assert_eq!(draft.stem_range(), 0.."report.final".len());

        let no_extension = RenameDraft::begin(r"C:\work\notes", "notes");
        assert_eq!(no_extension.stem_range(), 0.."notes".len());

        let dotfile = RenameDraft::begin(r"C:\work\.gitignore", ".gitignore");
        assert_eq!(dotfile.stem_range(), 0..".gitignore".len());

        let trailing_dot = RenameDraft::begin(r"C:\work\name.", "name.");
        assert_eq!(trailing_dot.stem_range(), 0.."name.".len());
    }

    #[test]
    fn a_rejected_submission_keeps_its_error_until_the_text_changes() {
        let mut draft = RenameDraft::begin(r"C:\work\notes.txt", "notes.txt");
        draft.set_text("report.");
        assert_eq!(
            draft.commit_candidate(),
            Err(RenameError::TrailingDotOrSpace)
        );
        assert_eq!(draft.error, Some(RenameError::TrailingDotOrSpace));

        draft.set_text("report.md");
        assert_eq!(draft.error, None, "editing clears the previous rejection");
        assert_eq!(draft.commit_candidate(), Ok("report.md".to_string()));
        assert_eq!(draft.error, None);
    }

    #[test]
    fn every_rejection_names_the_message_that_explains_it() {
        for error in [
            RenameError::Empty,
            RenameError::Unchanged,
            RenameError::InvalidChar('*'),
            RenameError::TrailingDotOrSpace,
            RenameError::ReservedName,
            RenameError::TooLong,
            RenameError::TargetExists,
        ] {
            assert!(
                error.message_key().is_some(),
                "{error:?} has no message to show beside the editor"
            );
        }
        assert_eq!(
            RenameError::Cancelled.message_key(),
            None,
            "a dismissed operation is not a failure to explain"
        );
        assert_eq!(
            RenameError::Shell {
                message: "access denied".to_string()
            }
            .message_key(),
            None,
            "the shell diagnostic itself is the message"
        );
    }

    #[test]
    fn a_failed_write_keeps_the_typed_text_and_its_error() {
        let mut draft = RenameDraft::begin(r"C:\work\notes.txt", "notes.txt");
        draft.set_text("renamed.txt");
        let error = RenameError::Shell {
            message: "PerformOperations: access denied".to_string(),
        };
        draft.fail(error.clone());
        assert_eq!(draft.text, "renamed.txt");
        assert_eq!(draft.error, Some(error));
    }
}

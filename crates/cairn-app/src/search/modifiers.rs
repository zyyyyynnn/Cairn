// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Explicit query match modes and plain-text modifier flags.

/// Text matching modifiers valid only when regex mode is disabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TextModifiers {
    pub(crate) pinyin: bool,
    pub(crate) ignore_punctuation: bool,
    pub(crate) ignore_whitespace: bool,
    pub(crate) ignore_diacritics: bool,
}

impl TextModifiers {
    pub(crate) const fn get(self, modifier: TextModifier) -> bool {
        match modifier {
            TextModifier::Pinyin => self.pinyin,
            TextModifier::IgnorePunctuation => self.ignore_punctuation,
            TextModifier::IgnoreWhitespace => self.ignore_whitespace,
            TextModifier::IgnoreDiacritics => self.ignore_diacritics,
        }
    }

    pub(crate) fn toggle(&mut self, modifier: TextModifier) {
        let slot = match modifier {
            TextModifier::Pinyin => &mut self.pinyin,
            TextModifier::IgnorePunctuation => &mut self.ignore_punctuation,
            TextModifier::IgnoreWhitespace => &mut self.ignore_whitespace,
            TextModifier::IgnoreDiacritics => &mut self.ignore_diacritics,
        };
        *slot = !*slot;
    }

    pub(crate) const fn with_only(modifier: TextModifier) -> Self {
        let mut out = Self {
            pinyin: false,
            ignore_punctuation: false,
            ignore_whitespace: false,
            ignore_diacritics: false,
        };
        match modifier {
            TextModifier::Pinyin => out.pinyin = true,
            TextModifier::IgnorePunctuation => out.ignore_punctuation = true,
            TextModifier::IgnoreWhitespace => out.ignore_whitespace = true,
            TextModifier::IgnoreDiacritics => out.ignore_diacritics = true,
        }
        out
    }
}

/// Individual plain-text matching modifier flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextModifier {
    Pinyin,
    IgnorePunctuation,
    IgnoreWhitespace,
    IgnoreDiacritics,
}

/// Mutual-exclusion state machine between plain-text modifiers and regular expressions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueryMatchMode {
    Plain(TextModifiers),
    Regex,
}

impl Default for QueryMatchMode {
    fn default() -> Self {
        Self::Plain(TextModifiers::default())
    }
}

impl QueryMatchMode {
    pub(crate) const fn is_regex(self) -> bool {
        matches!(self, Self::Regex)
    }

    pub(crate) const fn modifier(self, modifier: TextModifier) -> bool {
        match self {
            Self::Plain(modifiers) => modifiers.get(modifier),
            Self::Regex => false,
        }
    }

    pub(crate) const fn text_modifiers(self) -> TextModifiers {
        match self {
            Self::Plain(modifiers) => modifiers,
            Self::Regex => TextModifiers {
                pinyin: false,
                ignore_punctuation: false,
                ignore_whitespace: false,
                ignore_diacritics: false,
            },
        }
    }

    pub(crate) fn toggle_regex(&mut self) {
        *self = match *self {
            Self::Plain(_) => Self::Regex,
            Self::Regex => Self::Plain(TextModifiers::default()),
        };
    }

    pub(crate) fn toggle_modifier(&mut self, modifier: TextModifier) {
        match self {
            Self::Plain(modifiers) => modifiers.toggle(modifier),
            Self::Regex => *self = Self::Plain(TextModifiers::with_only(modifier)),
        }
    }
}

/// Unified toggle action across case/word/path flags, regex mode, and plain-text modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueryToggle {
    Case,
    WholeWord,
    MatchPath,
    Regex,
    Modifier(TextModifier),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_and_text_modifiers_are_mutually_exclusive() {
        let mut mode = QueryMatchMode::default();
        assert!(!mode.is_regex());

        mode.toggle_modifier(TextModifier::Pinyin);
        mode.toggle_modifier(TextModifier::IgnoreWhitespace);
        assert!(mode.modifier(TextModifier::Pinyin));
        assert!(mode.modifier(TextModifier::IgnoreWhitespace));

        mode.toggle_modifier(TextModifier::Pinyin);
        assert!(!mode.modifier(TextModifier::Pinyin));
        assert!(mode.modifier(TextModifier::IgnoreWhitespace));

        mode.toggle_regex();
        assert!(mode.is_regex());
        assert_eq!(mode.text_modifiers(), TextModifiers::default());
        assert!(!mode.modifier(TextModifier::IgnoreWhitespace));

        mode.toggle_modifier(TextModifier::IgnoreDiacritics);
        assert!(!mode.is_regex());
        assert_eq!(
            mode.text_modifiers(),
            TextModifiers::with_only(TextModifier::IgnoreDiacritics)
        );

        let mut reset = QueryMatchMode::Regex;
        reset.toggle_regex();
        assert_eq!(reset, QueryMatchMode::Plain(TextModifiers::default()));
    }
}

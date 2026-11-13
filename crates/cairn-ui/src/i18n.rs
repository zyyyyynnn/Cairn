// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The Fluent bundle pair shared by the search window and the installer.
//!
//! Both binaries ship the same zh-CN and en-US locales but embed them separately; this module owns
//! the bundle construction, the language choice and the formatting fallback so those policies
//! cannot drift between them.

use std::borrow::Cow;
use std::sync::OnceLock;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use unic_langid::LanguageIdentifier;

pub struct Messages {
    zh_source: Cow<'static, str>,
    en_source: Cow<'static, str>,
    zh: OnceLock<FluentBundle<FluentResource>>,
    en: OnceLock<FluentBundle<FluentResource>>,
}

impl Messages {
    /// Builds a lazily parsed bundle pair from arbitrary locale sources.
    pub fn new(zh_source: &str, en_source: &str) -> Self {
        Self {
            zh_source: Cow::Owned(zh_source.to_string()),
            en_source: Cow::Owned(en_source.to_string()),
            zh: OnceLock::new(),
            en: OnceLock::new(),
        }
    }

    /// Builds a lazily parsed bundle pair from embedded static locale sources without copying.
    pub fn from_static(zh_source: &'static str, en_source: &'static str) -> Self {
        Self {
            zh_source: Cow::Borrowed(zh_source),
            en_source: Cow::Borrowed(en_source),
            zh: OnceLock::new(),
            en: OnceLock::new(),
        }
    }

    /// Returns whether the target locale bundle has been parsed into memory.
    pub fn is_loaded(&self, language: &str) -> bool {
        if language.starts_with("zh") {
            self.zh.get().is_some()
        } else {
            self.en.get().is_some()
        }
    }

    fn bundle(&self, language: &str) -> &FluentBundle<FluentResource> {
        if language.starts_with("zh") {
            self.zh.get_or_init(|| bundle("zh-CN", &self.zh_source))
        } else {
            self.en.get_or_init(|| bundle("en-US", &self.en_source))
        }
    }

    /// Whether the language's bundle carries `key` at all.
    pub fn has_message(&self, language: &str, key: &str) -> bool {
        self.bundle(language).get_message(key).is_some()
    }

    /// The `key` message in `language`, or `None` when the message or its value is absent. An
    /// empty formatted value stays `Some("")`; blankness is the caller's policy.
    pub fn value(&self, language: &str, key: &str) -> Option<String> {
        self.format(language, key, None)
    }

    /// Formats `key` with `args`; `None` when the message or its value is absent.
    pub fn format(&self, language: &str, key: &str, args: Option<&FluentArgs>) -> Option<String> {
        let bundle = self.bundle(language);
        let pattern = bundle.get_message(key)?.value()?;
        let mut errors = Vec::new();
        Some(
            bundle
                .format_pattern(pattern, args, &mut errors)
                .to_string(),
        )
    }
}

fn bundle(language: &str, source: &str) -> FluentBundle<FluentResource> {
    let identifier: LanguageIdentifier = language.parse().expect("supported locale identifier");
    let resource = FluentResource::try_new(source.to_string()).expect("valid Fluent resource");
    let mut bundle = FluentBundle::new(vec![identifier]);
    bundle
        .add_resource(resource)
        .expect("one resource per bundle");
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZH: &str = "hello = 你好\nname = 你好，{ $name }";
    const EN: &str = "hello = Hello\nname = Hello, { $name }";

    #[test]
    fn the_bundle_follows_the_language_prefix_and_reports_missing_keys() {
        let messages = Messages::new(ZH, EN);
        assert_eq!(messages.value("zh-CN", "hello").as_deref(), Some("你好"));
        assert_eq!(messages.value("en-US", "hello").as_deref(), Some("Hello"));
        assert_eq!(
            messages.value("de-DE", "hello").as_deref(),
            Some("Hello"),
            "an unsupported language falls back to the English bundle"
        );
        assert!(messages.value("en-US", "absent").is_none());

        let mut args = FluentArgs::new();
        args.set("name", "Cairn");
        let formatted = messages
            .format("en-US", "name", Some(&args))
            .expect("formatted message");
        assert_eq!(
            formatted.replace(['\u{2068}', '\u{2069}'], ""),
            "Hello, Cairn"
        );
    }

    #[test]
    fn bundles_parse_lazily_on_first_locale_access() {
        let messages = Messages::from_static(ZH, EN);
        assert!(!messages.is_loaded("zh-CN"));
        assert!(!messages.is_loaded("en-US"));

        assert_eq!(messages.value("zh-CN", "hello").as_deref(), Some("你好"));
        assert!(messages.is_loaded("zh-CN"));
        assert!(!messages.is_loaded("en-US"));

        assert_eq!(messages.value("de-DE", "hello").as_deref(), Some("Hello"));
        assert!(messages.is_loaded("en-US"));
    }
}

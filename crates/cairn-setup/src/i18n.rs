// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer's two Fluent bundles. The setup window follows the Windows UI language; it has no
//! language selector of its own.

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use unic_langid::LanguageIdentifier;

const ZH_CN_SOURCE: &str = include_str!("../locales/zh-CN.ftl");
const EN_US_SOURCE: &str = include_str!("../locales/en-US.ftl");

pub struct I18n {
    lang: String,
    zh: FluentBundle<FluentResource>,
    en: FluentBundle<FluentResource>,
}

impl I18n {
    pub fn new(lang: &str) -> Self {
        Self {
            lang: if lang.starts_with("zh") {
                "zh-CN".to_string()
            } else {
                "en-US".to_string()
            },
            zh: bundle("zh-CN", ZH_CN_SOURCE),
            en: bundle("en-US", EN_US_SOURCE),
        }
    }

    fn active(&self) -> &FluentBundle<FluentResource> {
        if self.lang.starts_with("zh") {
            &self.zh
        } else {
            &self.en
        }
    }

    pub fn get(&self, key: &str) -> String {
        self.format(key, &[])
    }

    pub fn format(&self, key: &str, args: &[(&str, &str)]) -> String {
        let bundle = self.active();
        let Some(message) = bundle.get_message(key) else {
            return key.to_string();
        };
        let Some(pattern) = message.value() else {
            return key.to_string();
        };
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            fluent_args.set(*name, *value);
        }
        let mut errors = Vec::new();
        bundle
            .format_pattern(pattern, Some(&fluent_args), &mut errors)
            .to_string()
    }
}

fn bundle(lang: &str, source: &str) -> FluentBundle<FluentResource> {
    let identifier: LanguageIdentifier = lang.parse().expect("supported locale identifier");
    let resource = FluentResource::try_new(source.to_string()).expect("valid Fluent resource");
    let mut bundle = FluentBundle::new(vec![identifier]);
    bundle
        .add_resource(resource)
        .expect("one resource per bundle");
    bundle
}

#[cfg(test)]
mod tests {
    use super::{I18n, EN_US_SOURCE, ZH_CN_SOURCE};
    use std::collections::BTreeSet;

    #[test]
    fn both_languages_resolve_every_visible_control_name() {
        for lang in ["zh-CN", "en-US"] {
            let i18n = I18n::new(lang);
            for key in [
                "license_title",
                "location_title",
                "action_install",
                "action_cancel",
                "uninstall_title",
                "error_title",
            ] {
                assert_ne!(i18n.get(key), key, "{lang} is missing {key}");
            }
        }
    }

    #[test]
    fn placeholders_are_formatted_not_exposed() {
        let en = I18n::new("en-US");
        let text = en
            .format("progress_percent", &[("percent", "42")])
            .replace(['\u{2068}', '\u{2069}'], "");
        assert_eq!(text, "42%");

        let zh = I18n::new("zh-CN");
        let version = zh.format("maintenance_installed", &[("version", "0.1.0")]);
        assert!(version.contains("0.1.0"));
        assert!(!version.contains("$version"));
    }

    /// The failure page names what happened and points at the log. The installer's raw code is
    /// an internal token: it belongs in the MSI log, not on the page.
    #[test]
    fn a_failure_page_never_shows_the_raw_installer_code() {
        for lang in ["zh-CN", "en-US"] {
            let i18n = I18n::new(lang);
            let text = i18n.format("error_msi_unknown", &[("code", "1603")]);
            assert!(!text.contains("1603"), "{lang} leaks the installer code");
            assert!(!text.contains("$code"), "{lang} leaks a raw placeholder");
        }
    }

    #[test]
    fn an_unsupported_language_uses_the_english_bundle() {
        let i18n = I18n::new("de-DE");
        assert_eq!(i18n.get("action_install"), "Install");
    }

    /// The two bundles must carry the same keys. A key that exists in one language and not in
    /// the other puts the raw key on the page in that language, so the sets are compared here
    /// rather than discovered by a person reading the installer.
    #[test]
    fn both_bundles_carry_the_same_keys() {
        fn keys(source: &str) -> BTreeSet<&str> {
            source
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        return None;
                    }
                    let (key, _) = line.split_once('=')?;
                    Some(key.trim())
                })
                .collect()
        }

        let zh = keys(ZH_CN_SOURCE);
        let en = keys(EN_US_SOURCE);
        assert_eq!(zh, en, "the two bundles must carry the same keys");
    }

    /// The uninstall flow names the same failures in uninstall terms. Both bundles must carry
    /// every one of them, or the uninstall page falls back to the raw key.
    #[test]
    fn the_uninstall_failures_have_their_own_words_in_both_languages() {
        for lang in ["zh-CN", "en-US"] {
            let i18n = I18n::new(lang);
            for key in [
                "error_uninstall_msi_cancelled",
                "error_uninstall_msi_busy",
                "error_uninstall_msi_platform",
                "error_uninstall_msi_fatal",
                "error_uninstall_not_installed",
            ] {
                assert_ne!(i18n.get(key), key, "{lang} is missing {key}");
            }
        }
    }

    /// The uninstall sentences must not borrow the installer's wording: the install page says
    /// the installer was damaged, the uninstall page says the installation record is.
    #[test]
    fn the_uninstall_sentences_do_not_repeat_the_install_sentences() {
        for lang in ["zh-CN", "en-US"] {
            let i18n = I18n::new(lang);
            for (install_key, uninstall_key) in [
                ("error_msi_cancelled", "error_uninstall_msi_cancelled"),
                ("error_msi_busy", "error_uninstall_msi_busy"),
                ("error_msi_platform", "error_uninstall_msi_platform"),
                ("error_msi_fatal", "error_uninstall_msi_fatal"),
            ] {
                assert_ne!(
                    i18n.get(install_key),
                    i18n.get(uninstall_key),
                    "{lang} reuses the install wording for {uninstall_key}"
                );
            }
        }
    }
}

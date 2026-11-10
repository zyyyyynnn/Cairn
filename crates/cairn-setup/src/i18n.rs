// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer's two Fluent bundles. The setup window follows the Windows UI language; it has no
//! language selector of its own.

use cairn_ui::i18n::Messages;
use fluent_bundle::FluentArgs;

const ZH_CN_SOURCE: &str = include_str!("../locales/zh-CN.ftl");
const EN_US_SOURCE: &str = include_str!("../locales/en-US.ftl");

pub struct I18n {
    lang: String,
    messages: Messages,
}

impl I18n {
    pub fn new(lang: &str) -> Self {
        Self {
            lang: if lang.starts_with("zh") {
                "zh-CN".to_string()
            } else {
                "en-US".to_string()
            },
            messages: Messages::new(ZH_CN_SOURCE, EN_US_SOURCE),
        }
    }

    pub fn get(&self, key: &str) -> String {
        self.format(key, &[])
    }

    pub fn format(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            fluent_args.set(*name, *value);
        }
        self.messages
            .format(&self.lang, key, Some(&fluent_args))
            .unwrap_or_else(|| key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::I18n;

    #[test]
    fn both_languages_resolve_every_visible_control_name() {
        for lang in ["zh-CN", "en-US"] {
            let i18n = I18n::new(lang);
            for key in [
                "license_title",
                "location_title",
                "action_install",
                "uninstall_title",
                "uninstall_files_removed_title",
                "uninstall_record_remaining",
                "action_remove_remaining",
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
                "error_uninstall_msi_invalid",
                "error_uninstall_cleanup_failed",
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
                ("error_msi_invalid", "error_uninstall_msi_invalid"),
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

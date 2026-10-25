// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fluent i18n bundle manager with embedded zh-CN and en-US locales.

use fluent_bundle::{FluentBundle, FluentResource};
use std::sync::RwLock;
use unic_langid::LanguageIdentifier;

pub static ZH_CN_SOURCE: &str = include_str!("../../locales/zh-CN.ftl");
pub static EN_US_SOURCE: &str = include_str!("../../locales/en-US.ftl");

pub struct I18nManager {
    current_lang: RwLock<String>,
    bundle_zh: FluentBundle<FluentResource>,
    bundle_en: FluentBundle<FluentResource>,
}

impl Default for I18nManager {
    fn default() -> Self {
        Self::new("zh-CN")
    }
}

impl I18nManager {
    pub fn new(default_lang: &str) -> Self {
        let lang_zh: LanguageIdentifier = "zh-CN".parse().unwrap();
        let res_zh = FluentResource::try_new(ZH_CN_SOURCE.to_string()).unwrap();
        let mut bundle_zh = FluentBundle::new(vec![lang_zh]);
        bundle_zh.add_resource(res_zh).unwrap();

        let lang_en: LanguageIdentifier = "en-US".parse().unwrap();
        let res_en = FluentResource::try_new(EN_US_SOURCE.to_string()).unwrap();
        let mut bundle_en = FluentBundle::new(vec![lang_en]);
        bundle_en.add_resource(res_en).unwrap();

        Self {
            current_lang: RwLock::new(default_lang.to_string()),
            bundle_zh,
            bundle_en,
        }
    }

    pub fn set_language(&self, lang: &str) {
        if let Ok(mut lock) = self.current_lang.write() {
            *lock = lang.to_string();
        }
    }

    /// `true` when every active locale resolves `key` to a message. Used by the UI-07 regression
    /// tests so a state or command that gained a message id in code cannot ship untranslated.
    #[cfg(test)]
    pub fn has_key(&self, key: &str) -> bool {
        self.bundle_zh.get_message(key).is_some() && self.bundle_en.get_message(key).is_some()
    }

    /// The `key` message as text in the language this manager currently speaks, or `None` when the
    /// message is absent or formats to nothing. [`Self::get_text`] cannot answer that: it falls back
    /// to the key id itself, so a missing or blank message still reads as non-empty. The NFR-11
    /// registry test needs the difference — an accessible name that resolves to a raw id, or to
    /// whitespace, is no name at all.
    #[cfg(test)]
    pub fn try_text(&self, key: &str) -> Option<String> {
        let lang = self.current_lang.read().unwrap().clone();
        let bundle = if lang.starts_with("zh") {
            &self.bundle_zh
        } else {
            &self.bundle_en
        };

        let pattern = bundle.get_message(key)?.value()?;
        let mut errors = vec![];
        let text = bundle
            .format_pattern(pattern, None, &mut errors)
            .to_string();
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn get_text(&self, key: &str) -> String {
        let lang = self.current_lang.read().unwrap().clone();
        let bundle = if lang.starts_with("zh") {
            &self.bundle_zh
        } else {
            &self.bundle_en
        };

        if let Some(msg) = bundle.get_message(key) {
            if let Some(pattern) = msg.value() {
                let mut errors = vec![];
                let value = bundle.format_pattern(pattern, None, &mut errors);
                return value.to_string();
            }
        }

        key.to_string()
    }

    pub fn get_text_with_args(&self, key: &str, args: &fluent_bundle::FluentArgs) -> String {
        let lang = self.current_lang.read().unwrap().clone();
        let bundle = if lang.starts_with("zh") {
            &self.bundle_zh
        } else {
            &self.bundle_en
        };

        if let Some(msg) = bundle.get_message(key) {
            if let Some(pattern) = msg.value() {
                let mut errors = vec![];
                let value = bundle.format_pattern(pattern, Some(args), &mut errors);
                return value.to_string();
            }
        }

        key.to_string()
    }
}

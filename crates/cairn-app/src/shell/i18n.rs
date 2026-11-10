// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fluent i18n bundle manager with embedded zh-CN and en-US locales.

use cairn_ui::i18n::Messages;
use fluent_bundle::FluentArgs;
use std::sync::RwLock;

const ZH_CN_SOURCE: &str = include_str!("../../locales/zh-CN.ftl");
const EN_US_SOURCE: &str = include_str!("../../locales/en-US.ftl");

pub struct I18nManager {
    current_lang: RwLock<String>,
    messages: Messages,
}

impl Default for I18nManager {
    fn default() -> Self {
        Self::new("zh-CN")
    }
}

impl I18nManager {
    pub fn new(default_lang: &str) -> Self {
        Self {
            current_lang: RwLock::new(default_lang.to_string()),
            messages: Messages::new(ZH_CN_SOURCE, EN_US_SOURCE),
        }
    }

    pub fn set_language(&self, lang: &str) {
        if let Ok(mut lock) = self.current_lang.write() {
            *lock = lang.to_string();
        }
    }

    fn language(&self) -> String {
        self.current_lang.read().unwrap().clone()
    }

    /// `true` when every active locale resolves `key` to a message. Used by the UI-07 regression
    /// tests so a state or command that gained a message id in code cannot ship untranslated.
    #[cfg(test)]
    pub fn has_key(&self, key: &str) -> bool {
        self.messages.has_message("zh-CN", key) && self.messages.has_message("en-US", key)
    }

    /// The `key` message as text in the language this manager currently speaks, or `None` when the
    /// message is absent or formats to nothing. [`Self::get_text`] cannot answer that: it falls back
    /// to the key id itself, so a missing or blank message still reads as non-empty. The NFR-11
    /// registry test needs the difference — an accessible name that resolves to a raw id, or to
    /// whitespace, is no name at all.
    #[cfg(test)]
    pub fn try_text(&self, key: &str) -> Option<String> {
        let text = self.messages.value(&self.language(), key)?;
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn get_text(&self, key: &str) -> String {
        self.messages
            .value(&self.language(), key)
            .unwrap_or_else(|| key.to_string())
    }

    pub fn get_text_with_args(&self, key: &str, args: &FluentArgs) -> String {
        self.messages
            .format(&self.language(), key, Some(args))
            .unwrap_or_else(|| key.to_string())
    }
}

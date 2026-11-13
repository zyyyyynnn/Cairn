// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Fluent i18n bundle manager with embedded zh-CN and en-US locales.

use cairn_ui::i18n::Messages;
use fluent_bundle::FluentArgs;
use gpui_kit::gpui::SharedString;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const ZH_CN_SOURCE: &str = include_str!("../../locales/zh-CN.ftl");
const EN_US_SOURCE: &str = include_str!("../../locales/en-US.ftl");

pub struct I18nManager {
    is_zh: AtomicBool,
    revision: AtomicU64,
    pub(crate) messages: Messages,
}

impl Default for I18nManager {
    fn default() -> Self {
        Self::new("zh-CN")
    }
}

impl I18nManager {
    pub fn new(default_lang: &str) -> Self {
        Self {
            is_zh: AtomicBool::new(default_lang.starts_with("zh")),
            revision: AtomicU64::new(1),
            messages: Messages::from_static(ZH_CN_SOURCE, EN_US_SOURCE),
        }
    }

    pub fn set_language(&self, lang: &str) {
        let next_zh = lang.starts_with("zh");
        let prev_zh = self.is_zh.swap(next_zh, Ordering::AcqRel);
        if prev_zh != next_zh {
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn language(&self) -> &'static str {
        if self.is_zh.load(Ordering::Acquire) {
            "zh-CN"
        } else {
            "en-US"
        }
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
        let text = self.messages.value(self.language(), key)?;
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn get_text(&self, key: &str) -> String {
        self.messages
            .value(self.language(), key)
            .unwrap_or_else(|| key.to_string())
    }

    pub fn get_text_with_args(&self, key: &str, args: &FluentArgs) -> String {
        self.messages
            .format(self.language(), key, Some(args))
            .unwrap_or_else(|| key.to_string())
    }
}

#[derive(Default)]
struct StaticLabelCacheState {
    revision: u64,
    labels: HashMap<&'static str, SharedString>,
}

/// UI-thread cache for zero-argument localized labels, invalidated automatically when
/// `I18nManager::revision()` advances on language changes.
#[derive(Default)]
pub(crate) struct StaticLabelCache {
    state: RefCell<StaticLabelCacheState>,
}

impl StaticLabelCache {
    pub fn get(&self, i18n: &I18nManager, key: &'static str) -> SharedString {
        self.get_or_resolve(i18n.revision(), key, |k| i18n.get_text(k))
    }

    pub fn get_or_resolve(
        &self,
        revision: u64,
        key: &'static str,
        resolve: impl FnOnce(&str) -> String,
    ) -> SharedString {
        let mut state = self.state.borrow_mut();
        if state.revision != revision {
            state.labels.clear();
            state.revision = revision;
        }
        if let Some(cached) = state.labels.get(key) {
            return cached.clone();
        }
        let resolved = SharedString::from(resolve(key));
        state.labels.insert(key, resolved.clone());
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::{I18nManager, StaticLabelCache};
    use std::cell::Cell;

    #[test]
    fn switching_language_parses_secondary_locale_lazily_and_bumps_revision_once() {
        let i18n = I18nManager::new("zh-CN");
        assert!(!i18n.messages.is_loaded("zh-CN"));
        assert!(!i18n.messages.is_loaded("en-US"));

        let _ = i18n.get_text("app_title");
        assert!(i18n.messages.is_loaded("zh-CN"));
        assert!(!i18n.messages.is_loaded("en-US"));
        let rev0 = i18n.revision();

        i18n.set_language("zh-TW");
        assert_eq!(i18n.revision(), rev0);

        i18n.set_language("en-US");
        assert_eq!(i18n.revision(), rev0 + 1);
        assert!(!i18n.messages.is_loaded("en-US"));
        let _ = i18n.get_text("app_title");
        assert!(i18n.messages.is_loaded("en-US"));
    }

    #[test]
    fn static_label_cache_reuses_entries_until_language_revision_changes() {
        let cache = StaticLabelCache::default();
        let i18n = I18nManager::new("zh-CN");
        let calls = Cell::new(0usize);

        let rev1 = i18n.revision();
        let a = cache.get_or_resolve(rev1, "app_title", |k| {
            calls.set(calls.get() + 1);
            format!("zh:{k}")
        });
        let b = cache.get_or_resolve(rev1, "app_title", |k| {
            calls.set(calls.get() + 1);
            format!("zh:{k}")
        });
        assert_eq!(a, b);
        assert_eq!(calls.get(), 1);

        i18n.set_language("en-US");
        let rev2 = i18n.revision();
        let c = cache.get_or_resolve(rev2, "app_title", |k| {
            calls.set(calls.get() + 1);
            format!("en:{k}")
        });
        assert_eq!(c.as_ref(), "en:app_title");
        assert_eq!(calls.get(), 2);
    }
}

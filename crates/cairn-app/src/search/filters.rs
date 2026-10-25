// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! File-type filter model for the search bar capsule (UI-03, QRY-06 macros).
//!
//! The category is a stable machine-readable enum: the UI looks up its label through
//! Fluent, the query builder looks up its macro prefix, and settings persist [`FilterCategory::key`].
//! No translated text is ever compared, so switching locale cannot strand the filter state.

/// Every category the capsule can hold. `All` means "add no macro to the query".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterCategory {
    All,
    Document,
    Picture,
    Audio,
    Video,
    Compressed,
    Executable,
    Folder,
}

impl FilterCategory {
    /// Stable machine-readable key: used for settings persistence and as an element id.
    pub fn key(self) -> &'static str {
        match self {
            FilterCategory::All => "all",
            FilterCategory::Document => "doc",
            FilterCategory::Picture => "image",
            FilterCategory::Audio => "audio",
            FilterCategory::Video => "video",
            FilterCategory::Compressed => "zip",
            FilterCategory::Executable => "exe",
            FilterCategory::Folder => "folder",
        }
    }

    /// Fluent message id carrying the capsule label for the current locale (UI-07).
    pub fn label_key(self) -> &'static str {
        match self {
            FilterCategory::All => "filter_all",
            FilterCategory::Document => "filter_doc",
            FilterCategory::Picture => "filter_image",
            FilterCategory::Audio => "filter_audio",
            FilterCategory::Video => "filter_video",
            FilterCategory::Compressed => "filter_zip",
            FilterCategory::Executable => "filter_exe",
            FilterCategory::Folder => "filter_folder",
        }
    }

    /// Everything macro prefix injected into the query (QRY-06). `All` contributes nothing.
    pub fn query_macro(self) -> Option<&'static str> {
        match self {
            FilterCategory::All => None,
            FilterCategory::Document => Some("doc:"),
            FilterCategory::Picture => Some("image:"),
            FilterCategory::Audio => Some("audio:"),
            FilterCategory::Video => Some("video:"),
            FilterCategory::Compressed => Some("zip:"),
            FilterCategory::Executable => Some("exe:"),
            FilterCategory::Folder => Some("folder:"),
        }
    }

    /// Click-cycle order, preserving the order the capsule has always offered.
    pub fn next(self) -> Self {
        match self {
            FilterCategory::All => FilterCategory::Document,
            FilterCategory::Document => FilterCategory::Picture,
            FilterCategory::Picture => FilterCategory::Compressed,
            FilterCategory::Compressed => FilterCategory::Executable,
            FilterCategory::Executable => FilterCategory::Folder,
            FilterCategory::Folder => FilterCategory::All,
            FilterCategory::Audio | FilterCategory::Video => FilterCategory::All,
        }
    }

    /// The order the sidebar lists its destination rows: the whole set first, then the categories a
    /// person actually narrows a search to. `Alt+P` cycles a shorter path through the same values;
    /// this list is the one the navigation column shows in full.
    pub const fn menu_order() -> &'static [FilterCategory] {
        &[
            FilterCategory::All,
            FilterCategory::Document,
            FilterCategory::Picture,
            FilterCategory::Audio,
            FilterCategory::Video,
            FilterCategory::Compressed,
            FilterCategory::Executable,
            FilterCategory::Folder,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::FilterCategory;

    const ALL: [FilterCategory; 8] = [
        FilterCategory::All,
        FilterCategory::Document,
        FilterCategory::Picture,
        FilterCategory::Audio,
        FilterCategory::Video,
        FilterCategory::Compressed,
        FilterCategory::Executable,
        FilterCategory::Folder,
    ];

    #[test]
    fn machine_keys_are_unique() {
        let mut keys: Vec<&str> = Vec::new();
        for category in ALL {
            let key = category.key();
            assert!(key.is_ascii(), "{key} must be locale independent");
            keys.push(key);
        }
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "each category needs a distinct key");
    }

    #[test]
    fn every_category_has_a_locale_key_and_only_all_has_no_macro() {
        for category in ALL {
            assert!(
                category.label_key().starts_with("filter_"),
                "label must be a Fluent key, never translated text"
            );
            assert_eq!(
                category.query_macro().is_some(),
                category != FilterCategory::All
            );
        }
    }

    #[test]
    fn cycling_returns_to_start_and_never_repeats_a_category() {
        let start = FilterCategory::All;
        let mut visited = vec![start];
        let mut current = start.next();
        while current != start {
            assert!(!visited.contains(&current), "cycle revisits {current:?}");
            visited.push(current);
            current = current.next();
        }
        assert!(
            visited.len() >= 3,
            "the capsule must expose more than two filters"
        );
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Navigation history: back, forward, and state preservation.

use crate::app::CairnApp;
use crate::search::ResultSetChange;
use crate::search::filters::FilterCategory;
use crate::shell::location::ActiveLocation;
use cairn_protocol::status::SourceKind;
use gpui_kit::gpui::{Context, Window};

/// Maximum number of navigation history states preserved in memory.
pub const MAX_NAV_HISTORY: usize = 50;

/// One step of navigation history, preserved for Back and Forward commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavState {
    pub text: String,
    pub filter: FilterCategory,
    pub source_scope: Option<cairn_protocol::message::SourceId>,
}

/// Non-empty bounded navigation history cursor upholding `!entries.is_empty() && index < entries.len()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavHistory {
    entries: Vec<NavState>,
    index: usize,
}

impl NavHistory {
    pub fn new(initial: NavState) -> Self {
        Self {
            entries: vec![initial],
            index: 0,
        }
    }

    pub fn current(&self) -> &NavState {
        &self.entries[self.index]
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[NavState] {
        &self.entries
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn record(&mut self, state: NavState) {
        if self.entries.get(self.index) == Some(&state) {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(state);

        if self.entries.len() > MAX_NAV_HISTORY {
            // Bounded capacity guard prevents unbounded history growth over long sessions.
            let excess = self.entries.len() - MAX_NAV_HISTORY;
            self.entries.drain(0..excess);
        }

        self.index = self.entries.len() - 1;
    }

    pub(crate) fn prune_missing_sources<F>(&mut self, is_valid_folder: F)
    where
        F: Fn(cairn_protocol::message::SourceId) -> bool,
    {
        for entry in &mut self.entries {
            if let Some(scope) = entry.source_scope
                && !is_valid_folder(scope)
            {
                entry.source_scope = None;
            }
        }
        let mut compacted: Vec<NavState> = Vec::with_capacity(self.entries.len());
        let mut new_index = 0;
        for (i, entry) in self.entries.drain(..).enumerate() {
            let duplicate_of_prev = compacted.last() == Some(&entry);
            if !duplicate_of_prev {
                compacted.push(entry);
            }
            if i == self.index {
                new_index = compacted.len().saturating_sub(1);
            }
        }
        self.entries = compacted;
        self.index = new_index.min(self.entries.len().saturating_sub(1));
    }

    pub fn update_current_text(&mut self, text: &str) {
        if let Some(state) = self.entries.get_mut(self.index) {
            state.text = text.trim().to_string();
        }
    }

    pub fn go_back(&mut self) -> Option<NavState> {
        if !self.can_go_back() {
            return None;
        }
        self.index -= 1;
        Some(self.entries[self.index].clone())
    }

    pub fn go_forward(&mut self) -> Option<NavState> {
        if !self.can_go_forward() {
            return None;
        }
        self.index += 1;
        Some(self.entries[self.index].clone())
    }
}

impl CairnApp {
    pub fn can_go_back(&self) -> bool {
        match self.active_location() {
            // The bin is one location step; Back leaves it for the query that was
            // active before it, even when that query is the first history entry.
            ActiveLocation::RecycleBin => true,
            ActiveLocation::Index => self.nav_history.can_go_back(),
        }
    }

    pub fn can_go_forward(&self) -> bool {
        match self.active_location() {
            ActiveLocation::RecycleBin => false,
            ActiveLocation::Index => self.nav_history.can_go_forward(),
        }
    }

    pub fn record_nav_state(&mut self) {
        if !self.location_profile().records_nav_history {
            return;
        }
        self.nav_history.record(NavState {
            text: self.query.search_input.text.trim().to_string(),
            filter: self.query.active_filter,
            source_scope: self.query.source_scope,
        });
    }

    pub fn update_current_nav_state_text(&mut self) {
        // While a local listing is open the search box filters that location in place,
        // not the query this entry describes; writing it back would erase the
        // text the person left before entering.
        if !self.location_profile().records_nav_history {
            return;
        }
        self.nav_history
            .update_current_text(&self.query.search_input.text);
    }

    /// Transitions from a local listing (clearing local filter text) or an existing index query
    /// (preserving typed text into current history) into an index location.
    pub(crate) fn prepare_for_index_location(&mut self) {
        self.close_group();
        if self.location_profile().is_local_listing {
            self.query.search_input.clear();
        } else {
            self.update_current_nav_state_text();
        }
        self.recycle_bin.leave();
    }

    /// Leaves the bin for the query state that was current before it without rewriting history,
    /// so repeated entry/exit never accumulates duplicate steps.
    pub(crate) fn return_from_recycle_bin(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.close_group();
        self.recycle_bin.leave();
        let current = self.nav_history.current().clone();
        self.apply_nav_state(&current, cx);
        self.query.search_focus.focus(window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.active_location() {
            ActiveLocation::RecycleBin => self.return_from_recycle_bin(window, cx),
            ActiveLocation::Index => {
                if !self.nav_history.can_go_back() {
                    return;
                }
                self.close_group();
                self.update_current_nav_state_text();
                if let Some(state) = self.nav_history.go_back() {
                    self.apply_nav_state(&state, cx);
                }
            }
        }
    }

    pub fn go_forward(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.active_location().is_recycle_bin() || !self.nav_history.can_go_forward() {
            return;
        }
        self.close_group();
        self.update_current_nav_state_text();
        if let Some(state) = self.nav_history.go_forward() {
            self.apply_nav_state(&state, cx);
        }
    }

    fn apply_nav_state(&mut self, state: &NavState, cx: &mut Context<'_, Self>) {
        self.query.search_input.set_text(&state.text);
        self.query.active_filter = state.filter;
        self.query.source_scope = state.source_scope.filter(|scope| {
            self.service
                .sources
                .iter()
                .any(|source| source.source_id == *scope && source.kind == SourceKind::Folder)
        });
        self.request_result_set(ResultSetChange::Replace, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::gpui::AppContext as _;

    fn state(text: &str, filter: FilterCategory) -> NavState {
        NavState {
            text: text.to_string(),
            filter,
            source_scope: None,
        }
    }

    #[test]
    fn nav_history_value_handles_dedup_branching_boundaries_and_cap() {
        let mut history = NavHistory::new(state("", FilterCategory::All));
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());
        assert!(history.go_back().is_none());
        assert!(history.go_forward().is_none());

        history.record(state("", FilterCategory::All));
        assert_eq!(history.len(), 1);

        history.record(state("one", FilterCategory::Document));
        history.record(state("two", FilterCategory::Picture));
        assert_eq!(history.len(), 3);
        assert_eq!(history.index(), 2);

        let back = history.go_back().expect("step back");
        assert_eq!(back.filter, FilterCategory::Document);
        assert!(history.can_go_back());
        assert!(history.can_go_forward());

        let forward = history.go_forward().expect("step forward");
        assert_eq!(forward.filter, FilterCategory::Picture);
        assert!(!history.can_go_forward());

        let back = history.go_back().expect("step back again");
        assert_eq!(back.filter, FilterCategory::Document);

        history.update_current_text("  edited  ");
        assert_eq!(history.current().text, "edited");

        history.record(state("branch", FilterCategory::Executable));
        assert_eq!(history.len(), 3);
        assert_eq!(history.index(), 2);
        assert!(!history.can_go_forward());

        for idx in 0..MAX_NAV_HISTORY + 10 {
            history.record(state(&format!("q{idx}"), FilterCategory::All));
        }
        assert_eq!(history.len(), MAX_NAV_HISTORY);
        assert_eq!(history.index(), MAX_NAV_HISTORY - 1);
        assert_eq!(history.entries()[0].text, "q10");
        assert_eq!(history.current().text, format!("q{}", MAX_NAV_HISTORY + 9));
    }

    #[test]
    fn prune_missing_sources_clears_removed_scopes_and_folds_adjacent_duplicates() {
        use cairn_protocol::message::SourceId;
        let removed = SourceId(1000);
        let mut history = NavHistory::new(NavState {
            text: String::new(),
            filter: FilterCategory::All,
            source_scope: None,
        });
        history.record(NavState {
            text: String::new(),
            filter: FilterCategory::All,
            source_scope: Some(removed),
        });
        history.record(NavState {
            text: String::new(),
            filter: FilterCategory::Document,
            source_scope: None,
        });
        let _ = history.go_back();
        assert_eq!(history.index(), 1);

        history.prune_missing_sources(|scope| scope != removed);
        assert_eq!(
            history.entries(),
            &[
                state("", FilterCategory::All),
                state("", FilterCategory::Document),
            ]
        );
        assert_eq!(history.index(), 0);

        history.prune_missing_sources(|_| true);
        assert_eq!(history.index(), 0);

        let mut all_removed = NavHistory::new(NavState {
            text: String::new(),
            filter: FilterCategory::All,
            source_scope: Some(removed),
        });
        all_removed.record(NavState {
            text: String::new(),
            filter: FilterCategory::All,
            source_scope: Some(SourceId(2000)),
        });
        all_removed.prune_missing_sources(|_| false);
        assert_eq!(all_removed.entries(), &[state("", FilterCategory::All)]);
        assert_eq!(all_removed.index(), 0);
    }

    #[test]
    fn recycle_bin_round_trip_and_prepare_for_index_location() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));

                    app.update(cx, |this, cx| {
                        this.query.search_input.set_text("hello");
                        this.open_recycle_bin(window, cx);
                        assert!(this.active_location().is_recycle_bin());
                        let hist_len = this.nav_history.len();

                        this.query.search_input.set_text("bin filter");
                        this.go_back(window, cx);
                        assert!(this.active_location().is_index());
                        assert_eq!(this.query.search_input.text, "hello");
                        assert_eq!(this.nav_history.len(), hist_len);

                        this.open_recycle_bin(window, cx);
                        this.query.search_input.set_text("local only");
                        this.prepare_for_index_location();
                        assert!(this.active_location().is_index());
                        assert!(this.query.search_input.text.is_empty());
                    });

                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn nav_history_steps_and_truncates_on_branch() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));

                    app.update(cx, |this, cx| {
                        assert!(!this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 1);
                        assert_eq!(this.nav_history.index(), 0);
                        assert_eq!(this.nav_history.entries()[0].filter, FilterCategory::All);

                        this.query.active_filter = FilterCategory::Document;
                        this.record_nav_state();

                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 2);
                        assert_eq!(this.nav_history.index(), 1);

                        this.query.active_filter = FilterCategory::Picture;
                        this.record_nav_state();

                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 3);
                        assert_eq!(this.nav_history.index(), 2);

                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history.index(), 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        this.query.active_filter = FilterCategory::Executable;
                        this.record_nav_state();

                        assert_eq!(this.nav_history.len(), 3);
                        assert_eq!(this.nav_history.index(), 2);
                        assert_eq!(this.nav_history.entries()[0].filter, FilterCategory::All);
                        assert_eq!(
                            this.nav_history.entries()[1].filter,
                            FilterCategory::Document
                        );
                        assert_eq!(
                            this.nav_history.entries()[2].filter,
                            FilterCategory::Executable
                        );
                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());

                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history.index(), 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::All);
                        assert_eq!(this.nav_history.index(), 0);
                        assert!(!this.can_go_back());
                        assert!(this.can_go_forward());

                        this.go_forward(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history.index(), 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        this.go_forward(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Executable);
                        assert_eq!(this.nav_history.index(), 2);
                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

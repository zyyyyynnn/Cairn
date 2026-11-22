// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Navigation history: back, forward, and state preservation.

use crate::app::CairnApp;
use crate::search::ResultSetChange;
use crate::search::filters::FilterCategory;
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

impl CairnApp {
    pub fn can_go_back(&self) -> bool {
        // The bin is one location step; Back leaves it for the query that was
        // active before it, even when that query is the first history entry.
        self.recycle_bin.is_open() || self.nav_history_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        // A location step has no forward meaning until it is left. Keeping the
        // navigation Forward button disabled prevents an action that would advance the
        // query history while the bin stays on screen.
        if self.recycle_bin.is_open() {
            return false;
        }
        self.nav_history_index + 1 < self.nav_history.len()
    }

    pub fn record_nav_state(&mut self) {
        let current = NavState {
            text: self.query.search_input.text.trim().to_string(),
            filter: self.query.active_filter,
            source_scope: self.query.source_scope,
        };
        if let Some(last) = self.nav_history.get(self.nav_history_index)
            && last == &current
        {
            return;
        }
        self.nav_history.truncate(self.nav_history_index + 1);
        self.nav_history.push(current);

        // Cap history to bounded size and release excess capacity (SM-2 memory guard)
        if self.nav_history.len() > MAX_NAV_HISTORY {
            let excess = self.nav_history.len() - MAX_NAV_HISTORY;
            self.nav_history.drain(0..excess);
            self.nav_history.shrink_to_fit();
        }

        self.nav_history_index = self.nav_history.len() - 1;
    }

    pub fn update_current_nav_state_text(&mut self) {
        // While the bin is open the search box filters that location in place,
        // not the query this entry describes; writing it back would erase the
        // text the person left before entering.
        if self.recycle_bin.is_open() {
            return;
        }
        if let Some(state) = self.nav_history.get_mut(self.nav_history_index) {
            state.text = self.query.search_input.text.trim().to_string();
        }
    }

    /// Leaves the bin for the query state that was current before it. The nav
    /// entry itself is untouched, so repeated Back/Forward does not create a
    /// second history step for the bin.
    pub(crate) fn return_from_recycle_bin(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.recycle_bin.leave();
        let index = self.nav_history_index;
        self.restore_nav_state(index, cx);
        // The bin owned the results focus; after leaving it the query box is
        // the next logical target, so the person can type immediately.
        self.query.search_focus.focus(window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.recycle_bin.is_open() {
            self.return_from_recycle_bin(window, cx);
            return;
        }
        if self.can_go_back() {
            self.update_current_nav_state_text();
            self.nav_history_index -= 1;
            self.restore_nav_state(self.nav_history_index, cx);
        }
    }

    pub fn go_forward(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) {
        // Forward is disabled in the bin; the guard keeps a stale key or
        // programmatic dispatch from advancing history behind the location.
        if self.recycle_bin.is_open() {
            return;
        }
        if self.can_go_forward() {
            self.update_current_nav_state_text();
            self.nav_history_index += 1;
            self.restore_nav_state(self.nav_history_index, cx);
        }
    }

    /// Restores one entry's query text and filter and asks the service for its
    /// result set.
    fn restore_nav_state(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(state) = self.nav_history.get(index).cloned() else {
            return;
        };
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
                        // Initially seeded with All at index 0
                        assert!(!this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 1);
                        assert_eq!(this.nav_history_index, 0);
                        assert_eq!(this.nav_history[0].filter, FilterCategory::All);

                        this.query.active_filter = FilterCategory::Document;
                        this.record_nav_state();

                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 2);
                        assert_eq!(this.nav_history_index, 1);

                        this.query.active_filter = FilterCategory::Picture;
                        this.record_nav_state();

                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 3);
                        assert_eq!(this.nav_history_index, 2);

                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history_index, 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        // Branching from an earlier entry truncates forward history.
                        this.query.active_filter = FilterCategory::Executable;
                        this.record_nav_state();

                        assert_eq!(this.nav_history.len(), 3);
                        assert_eq!(this.nav_history_index, 2);
                        assert_eq!(this.nav_history[0].filter, FilterCategory::All);
                        assert_eq!(this.nav_history[1].filter, FilterCategory::Document);
                        assert_eq!(this.nav_history[2].filter, FilterCategory::Executable);
                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());

                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history_index, 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::All);
                        assert_eq!(this.nav_history_index, 0);
                        assert!(!this.can_go_back());
                        assert!(this.can_go_forward());

                        this.go_forward(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history_index, 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        this.go_forward(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Executable);
                        assert_eq!(this.nav_history_index, 2);
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

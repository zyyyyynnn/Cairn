// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Navigation history: back, forward, and state preservation.

use crate::app::CairnApp;
use crate::search::filters::FilterCategory;
use gpui_kit::gpui::{Context, Window};

/// Maximum number of navigation history states preserved in memory.
pub const MAX_NAV_HISTORY: usize = 50;

/// One step of sidebar and query navigation history, preserved for Back and Forward commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavState {
    pub text: String,
    pub drive: Option<char>,
    pub filter: FilterCategory,
}

impl CairnApp {
    pub fn can_go_back(&self) -> bool {
        // The bin is one location step; Back leaves it for the query that was
        // active before it, even when that query is the first history entry.
        if self.recycle_bin.is_open() {
            return true;
        }
        if self.drive_view.is_some() {
            return self
                .treemap_state
                .as_ref()
                .is_some_and(|state| state.can_go_back());
        }
        self.nav_history_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        // A location step has no forward meaning until it is left. Keeping the
        // toolbar Forward disabled prevents an action that would advance the
        // query history while the bin stays on screen.
        if self.recycle_bin.is_open() {
            return false;
        }
        if self.drive_view.is_some() {
            return self
                .treemap_state
                .as_ref()
                .is_some_and(|state| state.can_go_forward());
        }
        self.nav_history_index + 1 < self.nav_history.len()
    }

    pub fn record_nav_state(&mut self) {
        let current = NavState {
            text: self.query.search_input.text.trim().to_string(),
            drive: self.query.scope_drive,
            filter: self.query.active_filter,
        };
        if let Some(last) = self.nav_history.get(self.nav_history_index) {
            if last == &current {
                return;
            }
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
        // While the bin is open the search box is a location label, not the
        // query that entry describes; writing it back would erase the text the
        // person left before opening the bin.
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
    pub(crate) fn return_from_recycle_bin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recycle_bin.leave();
        let index = self.nav_history_index;
        self.restore_nav_state(index, cx);
        // The bin owned the results focus; after leaving it the query box is
        // the next logical target, so the person can type immediately.
        self.query.search_focus.focus(window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            self.return_from_recycle_bin(window, cx);
            return;
        }
        if self.drive_view.is_some() {
            // Inside the Treemap, Back belongs to the drill path only. Falling
            // through at the drive root would silently restore hidden table
            // state while the Treemap stays on screen.
            self.treemap_drill_up(cx);
            return;
        }
        if self.can_go_back() {
            self.update_current_nav_state_text();
            self.nav_history_index -= 1;
            self.restore_nav_state(self.nav_history_index, cx);
        }
    }

    pub fn go_forward(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            // Forward is disabled in the bin; the guard keeps a stale key or
            // programmatic dispatch from advancing history behind the location.
            return;
        }
        if self.drive_view.is_some() {
            self.treemap_drill_forward(cx);
            return;
        }
        if self.can_go_forward() {
            self.update_current_nav_state_text();
            self.nav_history_index += 1;
            self.restore_nav_state(self.nav_history_index, cx);
        }
    }

    fn restore_nav_state(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.nav_history.get(index).cloned() {
            self.query.search_input.set_text(&state.text);
            self.query.scope_drive = state.drive;
            self.query.active_filter = state.filter;
            self.trigger_search(cx);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_history_steps_and_truncates_on_branch() {
        let mut history = vec![NavState {
            text: String::new(),
            drive: None,
            filter: FilterCategory::All,
        }];
        let mut index = 0usize;

        let can_back = |idx: usize| idx > 0;
        let can_forward = |idx: usize, len: usize| idx + 1 < len;

        assert!(!can_back(index));
        assert!(!can_forward(index, history.len()));

        // Step 1: User navigates to Document
        history.push(NavState {
            text: String::new(),
            drive: None,
            filter: FilterCategory::Document,
        });
        index = history.len() - 1;
        assert!(can_back(index));
        assert!(!can_forward(index, history.len()));
        assert_eq!(index, 1);

        // Step 2: User navigates to Picture
        history.push(NavState {
            text: String::new(),
            drive: None,
            filter: FilterCategory::Picture,
        });
        index = history.len() - 1;
        assert_eq!(index, 2);

        // Step 3: Go back
        index -= 1;
        assert_eq!(history[index].filter, FilterCategory::Document);
        assert!(can_back(index));
        assert!(can_forward(index, history.len()));

        // Step 4: Branch by navigating to Executable while at Document
        history.truncate(index + 1);
        history.push(NavState {
            text: String::new(),
            drive: None,
            filter: FilterCategory::Executable,
        });
        index = history.len() - 1;
        assert_eq!(history.len(), 3); // All, Document, Executable (Picture dropped)
        assert_eq!(index, 2);
        assert!(!can_forward(index, history.len()));
    }
}

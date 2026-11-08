// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Navigation history: back, forward, and state preservation.

use crate::app::CairnApp;
use crate::search::filters::FilterCategory;
use gpui_kit::gpui::{Context, Window};

/// Maximum number of navigation history states preserved in memory.
pub const MAX_NAV_HISTORY: usize = 50;

/// One step of navigation history, preserved for Back and Forward commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavState {
    pub text: String,
    pub drive: Option<char>,
    pub filter: FilterCategory,
    /// The disk visualization on screen, with the directory its canvas showed. `None` is the
    /// results table, so one history walks across both views.
    pub visualization: Option<(char, String)>,
}

impl CairnApp {
    pub fn can_go_back(&self) -> bool {
        // The bin is one location step; Back leaves it for the query that was
        // active before it, even when that query is the first history entry.
        if self.recycle_bin.is_open() {
            return true;
        }
        // Inside the visualization, Back walks the drill path first and only then leaves the view:
        // the view is one history step, not a dead end at its own root.
        if self.drive_view.is_some() {
            return self.drill_back_is_available() || self.nav_history_index > 0;
        }
        self.nav_history_index > 0
    }

    /// The Treemap can rise to its parent directory; the table and bin have no
    /// path hierarchy for this command.
    pub fn can_go_up(&self) -> bool {
        self.drive_view.is_some()
            && self
                .treemap_state
                .as_ref()
                .is_some_and(|state| state.can_go_up())
    }

    pub fn can_go_forward(&self) -> bool {
        // A location step has no forward meaning until it is left. Keeping the
        // toolbar Forward disabled prevents an action that would advance the
        // query history while the bin stays on screen.
        if self.recycle_bin.is_open() {
            return false;
        }
        if self.drive_view.is_some() {
            return self.drill_forward_is_available()
                || self.nav_history_index + 1 < self.nav_history.len();
        }
        self.nav_history_index + 1 < self.nav_history.len()
    }

    fn drill_back_is_available(&self) -> bool {
        self.treemap_state
            .as_ref()
            .is_some_and(|state| state.can_go_back())
    }

    fn drill_forward_is_available(&self) -> bool {
        self.treemap_state
            .as_ref()
            .is_some_and(|state| state.can_go_forward())
    }

    /// The disk visualization a history entry should reopen: the drive and the directory the
    /// canvas holds right now.
    fn current_visualization(&self) -> Option<(char, String)> {
        let drive = self.drive_view?;
        let dir = self
            .treemap_state
            .as_ref()
            .map(|state| state.current_dir().to_string())?;
        Some((drive, dir))
    }

    pub fn record_nav_state(&mut self) {
        let current = NavState {
            text: self.query.search_input.text.trim().to_string(),
            drive: self.query.scope_drive,
            filter: self.query.active_filter,
            visualization: self.current_visualization(),
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

    /// Records where the canvas stands in the entry that describes it, so Back returns to the
    /// directory the person was looking at rather than the one the view opened on.
    pub(crate) fn sync_visualization_dir(&mut self) {
        let Some(dir) = self
            .treemap_state
            .as_ref()
            .map(|state| state.current_dir().to_string())
        else {
            return;
        };
        if let Some(state) = self.nav_history.get_mut(self.nav_history_index) {
            if let Some((_, recorded)) = &mut state.visualization {
                *recorded = dir;
            }
        }
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
        self.restore_nav_state(index, window, cx);
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
            if self.drill_back_is_available() {
                self.treemap_drill_back(window, cx);
                return;
            }
            // The drill path is spent, so Back leaves the visualization for the step before it
            // rather than reporting a dead end at the view's own root.
            if self.nav_history_index > 0 {
                self.sync_visualization_dir();
                self.update_current_nav_state_text();
                self.nav_history_index -= 1;
                self.restore_nav_state(self.nav_history_index, window, cx);
            }
            return;
        }
        if self.can_go_back() {
            self.update_current_nav_state_text();
            self.nav_history_index -= 1;
            self.restore_nav_state(self.nav_history_index, window, cx);
        }
    }

    /// Rises to the parent directory of the Treemap. A table or bin view has
    /// no parent path, so the command does nothing there.
    pub fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drive_view.is_some() {
            self.treemap_drill_up(window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            // Forward is disabled in the bin; the guard keeps a stale key or
            // programmatic dispatch from advancing history behind the location.
            return;
        }
        if self.drive_view.is_some() {
            if self.drill_forward_is_available() {
                self.treemap_drill_forward(window, cx);
                return;
            }
            if self.nav_history_index + 1 < self.nav_history.len() {
                self.sync_visualization_dir();
                self.update_current_nav_state_text();
                self.nav_history_index += 1;
                self.restore_nav_state(self.nav_history_index, window, cx);
            }
            return;
        }
        if self.can_go_forward() {
            self.update_current_nav_state_text();
            self.nav_history_index += 1;
            self.restore_nav_state(self.nav_history_index, window, cx);
        }
    }

    fn restore_nav_state(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.nav_history.get(index).cloned() else {
            return;
        };
        let is_visualization = state.visualization.is_some();
        match state.visualization {
            Some((drive, dir)) => self.open_drive_at(drive, &dir, window, cx),
            None => self.leave_drive_view(),
        }
        self.query.search_input.set_text(&state.text);
        self.query.scope_drive = state.drive;
        self.query.active_filter = state.filter;
        // A visualization entry owns the surface, so its table query is restored without running a
        // search whose rows nobody can see.
        if !is_visualization {
            self.trigger_search(cx);
        }
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
                gpui_kit::gpui::WindowOptions::default(),
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

                        // Step 1: User navigates to Document filter
                        this.query.active_filter = FilterCategory::Document;
                        this.record_nav_state();

                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 2);
                        assert_eq!(this.nav_history_index, 1);

                        // Step 2: User navigates to Picture filter
                        this.query.active_filter = FilterCategory::Picture;
                        this.record_nav_state();

                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());
                        assert_eq!(this.nav_history.len(), 3);
                        assert_eq!(this.nav_history_index, 2);

                        // Step 3: Go back to Document
                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history_index, 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        // Step 4: Branch by navigating to Executable while at Document
                        this.query.active_filter = FilterCategory::Executable;
                        this.record_nav_state();

                        // History should now be: All, Document, Executable (Picture dropped)
                        assert_eq!(this.nav_history.len(), 3);
                        assert_eq!(this.nav_history_index, 2);
                        assert_eq!(this.nav_history[0].filter, FilterCategory::All);
                        assert_eq!(this.nav_history[1].filter, FilterCategory::Document);
                        assert_eq!(this.nav_history[2].filter, FilterCategory::Executable);
                        assert!(this.can_go_back());
                        assert!(!this.can_go_forward());

                        // Step 5: Go back to Document
                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::Document);
                        assert_eq!(this.nav_history_index, 1);
                        assert!(this.can_go_back());
                        assert!(this.can_go_forward());

                        // Step 6: Go back to All
                        this.go_back(window, cx);
                        assert_eq!(this.query.active_filter, FilterCategory::All);
                        assert_eq!(this.nav_history_index, 0);
                        assert!(!this.can_go_back());
                        assert!(this.can_go_forward());

                        // Step 7: Advance forward back to Executable
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

    #[test]
    fn nav_history_preserves_browsing_dir_when_leaving_drive_view() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions::default(),
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));

                    app.update(cx, |this, cx| {
                        // 1. Initial state at table index 0
                        assert_eq!(this.nav_history_index, 0);
                        assert!(this.drive_view.is_none());

                        // 2. Open drive visualization for 'C'
                        this.open_drive_visualization('C', window, cx);
                        assert_eq!(this.nav_history_index, 1);
                        assert_eq!(this.drive_view, Some('C'));

                        // 3. Navigate directly to a browsing path C:\Windows without drill-back history
                        this.open_drive_at('C', r"C:\Windows", window, cx);

                        // 4. Leave drive view by going back to table
                        this.go_back(window, cx);
                        assert_eq!(this.nav_history_index, 0);
                        assert!(this.drive_view.is_none());

                        // Verify that the drive history entry at index 1 recorded C:\Windows
                        let recorded_dir = this.nav_history[1]
                            .visualization
                            .as_ref()
                            .map(|(_, dir)| dir.as_str());
                        assert_eq!(
                            recorded_dir,
                            Some(r"C:\Windows"),
                            "drive history entry must preserve browsing path when leaving drive view"
                        );

                        // 5. Go forward back into drive view
                        this.go_forward(window, cx);
                        assert_eq!(this.nav_history_index, 1);
                        assert_eq!(this.drive_view, Some('C'));
                        assert_eq!(
                            this.treemap_state.as_ref().map(|s| s.current_dir()),
                            Some(r"C:\Windows")
                        );

                        // 6. Switch directly to drive D and navigate to D:\Games
                        this.open_drive_visualization('D', window, cx);
                        assert_eq!(this.drive_view, Some('D'));
                        this.open_drive_at('D', r"D:\Games", window, cx);

                        // 7. Go back: must restore Drive C at C:\Windows
                        this.go_back(window, cx);
                        assert_eq!(this.drive_view, Some('C'));
                        assert_eq!(
                            this.treemap_state.as_ref().map(|s| s.current_dir()),
                            Some(r"C:\Windows")
                        );

                        // 8. Go forward: must restore Drive D at D:\Games
                        this.go_forward(window, cx);
                        assert_eq!(this.drive_view, Some('D'));
                        assert_eq!(
                            this.treemap_state.as_ref().map(|s| s.current_dir()),
                            Some(r"D:\Games")
                        );
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

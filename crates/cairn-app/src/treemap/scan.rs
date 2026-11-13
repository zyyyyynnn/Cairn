// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Treemap navigation and scan lifecycle: opening a drive view, requesting
//! directory trees from the service and stepping through directory history.

use crate::app::CairnApp;
use crate::search::filters::FilterCategory;
use crate::service::ipc_tasks::get_tokio_runtime;
use crate::shell::os::open_file;
use crate::treemap::state::{tree_unavailable_from_code, TreemapUnavailable, TreemapViewState};
use cairn_core::treemap::same_directory;
use cairn_protocol::message::{ResponsePayload, MAX_TREE_DEPTH};
use gpui_kit::gpui::*;
use std::path::PathBuf;

impl CairnApp {
    /// Switches to the drive disk space Treemap visualization and records it as a history step, so
    /// Back leaves the canvas again instead of stopping at its root.
    pub fn open_drive_visualization(
        &mut self,
        drive: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The query and directory being left behind stay in the entry they belong to.
        self.sync_visualization_dir();
        self.update_current_nav_state_text();
        self.open_drive_at(
            drive,
            &format!("{}:\\", drive.to_ascii_uppercase()),
            window,
            cx,
        );
        self.record_nav_state();
    }

    /// Opens the visualization on `dir` without touching navigation history: Back and Forward
    /// restore a recorded directory through here, and pushing an entry would move the cursor.
    pub(crate) fn open_drive_at(
        &mut self,
        drive: char,
        dir: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drive_upper = drive.to_ascii_uppercase();
        self.discard_rename(cx);
        // A bin or block-listing filter is a location-local search, not a query
        // to carry into the drive view. Every drive entry lands here, so the
        // location is left here rather than at each caller.
        let leaving_bin = self.recycle_bin.leave();
        let leaving_listing = self.block_listing.take().is_some();
        if leaving_bin || leaving_listing {
            self.query.search_input.clear();
        }
        self.clear_treemap_path_menu();
        self.drive_view = Some(drive_upper);
        self.query.scope_drive = Some(drive_upper);
        let (state, scan_target) = TreemapViewState::new(drive_upper, dir, cx.focus_handle());
        state.request_focus(window, cx);
        self.treemap_state = Some(state);
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        cx.notify();
    }

    /// Returns keyboard ownership to the canvas after a navigation, commit or
    /// retry. One method so every path that changes the tree ends the same way.
    pub(super) fn focus_treemap_canvas(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.treemap_state.as_ref() {
            state.request_focus(window, cx);
        }
    }

    /// Returns the Treemap to 100% without touching the selected node. The
    /// canvas keeps keyboard focus so a following wheel, arrow or Escape still
    /// drives the visualization.
    pub fn reset_treemap_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref mut st) = self.treemap_state {
            st.zoom.reset_view();
            st.request_focus(window, cx);
            cx.notify();
        }
    }

    /// Loads the Treemap for `scan_path` from the service's live index.
    ///
    /// The stored drive tree and the cached directory trees are consulted before
    /// this call. A path they do not hold is answered by one pipe round-trip;
    /// when the service cannot answer, the canvas reports the directory as
    /// unavailable rather than scanning the disk itself.
    pub fn start_treemap_scan(&mut self, scan_path: PathBuf, cx: &mut Context<Self>) {
        self.clear_block_listing();
        self.clear_treemap_path_menu();
        let Some(ref mut state) = self.treemap_state else {
            return;
        };
        state.reset_scan_state();
        state.scan_fade_epoch = 0;
        state.is_scanning = true;
        let epoch = state.request_epoch;
        cx.notify();

        let path_for_service = scan_path.to_string_lossy().into_owned();
        let session = self.service.session.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let result = session.get_tree(&path_for_service, MAX_TREE_DEPTH).await;
                let _ = tx.send(result);
            });

            let outcome = match rx.await {
                Ok(Ok(envelope)) => match envelope.payload {
                    ResponsePayload::TreeResult { entries, .. } if !entries.is_empty() => {
                        Ok(entries)
                    }
                    ResponsePayload::TreeResult { .. } => Err(TreemapUnavailable::NotIndexed),
                    ResponsePayload::Error { code, .. } => Err(tree_unavailable_from_code(code)),
                    _ => Err(TreemapUnavailable::ServiceUnavailable),
                },
                _ => Err(TreemapUnavailable::ServiceUnavailable),
            };
            let _ = this.update(cx, |this, cx| {
                let Some(ref mut state) = this.treemap_state else {
                    return;
                };
                if state.request_epoch != epoch {
                    return;
                }
                match outcome {
                    Ok(entries) => {
                        crate::shell::perf::mark(&format!(
                            "treemap tree service entries={}",
                            entries.len()
                        ));
                        if !state.adopt_service_tree(entries) {
                            state.mark_unavailable(TreemapUnavailable::ServiceUnavailable);
                        }
                    }
                    Err(reason) => state.mark_unavailable(reason),
                }
                this.arm_treemap_scan_fade_cleanup(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Arms the Treemap scan fade cleanup timer if a new scan fade epoch is active.
    ///
    /// The timer runs on the background executor and cleans up the fade epoch
    /// after `MASK_DISSOLVE_CLEANUP`, ensuring `render()` remains pure and never
    /// spawns redundant tasks across animation frames.
    pub(crate) fn arm_treemap_scan_fade_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(ref mut state) = self.treemap_state else {
            return;
        };
        let epoch = state.scan_fade_epoch;
        if epoch == 0 || state.scan_fade_timer_armed >= epoch {
            return;
        }
        state.scan_fade_timer_armed = epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::shell::motion::MASK_DISSOLVE_CLEANUP)
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ref mut state) = this.treemap_state {
                    if state.scan_fade_epoch == epoch {
                        state.scan_fade_epoch = 0;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn treemap_drill_down(
        &mut self,
        target_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_treemap_path_menu();
        // A navigation step ends address editing: the field describes the
        // directory the canvas is leaving, not the one it just opened.
        self.cancel_treemap_path_edit(window, cx);
        // A summary block owned by the current root cannot drill anywhere; the
        // old code reloaded the same directory and flashed the empty state.
        if self
            .treemap_state
            .as_ref()
            .is_some_and(|st| same_directory(st.current_dir(), target_path))
        {
            return;
        }
        self.navigate_treemap(|st| st.drill_down(target_path), window, cx);
    }

    /// Moves to the parent directory as a history step. At the drive root the
    /// control is disabled and this does nothing.
    pub fn treemap_drill_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_treemap(TreemapViewState::drill_up, window, cx);
    }

    /// Returns to the directory most recently left by any drill step.
    pub fn treemap_drill_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_treemap(TreemapViewState::drill_back, window, cx);
    }

    pub fn treemap_drill_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_treemap(TreemapViewState::drill_forward, window, cx);
    }

    fn navigate_treemap(
        &mut self,
        step: impl FnOnce(&mut TreemapViewState) -> Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_treemap_path_edit(window, cx);
        let scan_target = self.treemap_state.as_mut().and_then(step);
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        self.focus_treemap_canvas(window, cx);
        cx.notify();
    }

    /// Opens the canvas selection the way a double click does: a summary block lists
    /// its members in the table, a directory drills down, a file opens in its
    /// associated application.
    pub fn treemap_open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self
            .treemap_state
            .as_ref()
            .and_then(|st| st.selected_node().cloned())
        else {
            return;
        };
        if node.is_aggregate() {
            self.open_block_listing(&node.path, window, cx);
        } else if node.item.is_dir {
            self.treemap_drill_down(&node.path, window, cx);
        } else {
            let _ = open_file(&node.path);
        }
    }

    /// Switches back to classic "All files" table filtered by `path`.
    pub fn switch_to_table_with_path(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Leaving the visualization releases the partial tree instead of keeping
        // it alive behind the table, and starts a fresh table entrance.
        self.leave_drive_view();
        self.clear_block_listing();
        self.query.active_filter = FilterCategory::All;
        self.query.search_input.set_text(&Self::parent_query(path));
        self.record_nav_state();
        self.trigger_search(cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// The search that lists a directory's own children. A category narrowing is not written here:
    /// it travels as query state and is spliced when the request is sent, so the box stays a path a
    /// person can read and edit.
    fn parent_query(path: &str) -> String {
        let trimmed = path.trim_end_matches(['\\', '/']);
        // A drive root must keep its separator: `parent:"C"` matches nothing, `parent:"C:\"`
        // lists the root's children. Both `C:` and `C:\` name the same root.
        let is_drive_root = (trimmed.len() == 2 && trimmed.ends_with(':')) || trimmed.len() == 1;
        if is_drive_root {
            format!("parent:\"{}:\\\"", trimmed.trim_end_matches(':'))
        } else {
            format!("parent:\"{trimmed}\"")
        }
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    /// The table hand-off must search the directory's own children. A drive root keeps the
    /// separator the query syntax needs; a nested path is used as written.
    #[test]
    fn the_table_hand_off_queries_the_directory_children() {
        assert_eq!(
            CairnApp::parent_query("C:\\Windows\\WinSxS"),
            "parent:\"C:\\Windows\\WinSxS\""
        );
        assert_eq!(
            CairnApp::parent_query("C:\\Windows\\"),
            "parent:\"C:\\Windows\""
        );
        assert_eq!(CairnApp::parent_query("C:"), "parent:\"C:\\\"");
        assert_eq!(CairnApp::parent_query("C:\\"), "parent:\"C:\\\"");
    }

    #[test]
    fn revealing_a_finished_scan_keys_the_dissolve_to_that_generation() {
        let mut state = crate::treemap::state::TreemapViewState::new_headless('C');
        state.is_scanning = true;
        state.reveal_content();
        assert!(
            state.scan_fade_epoch > 0,
            "a revealed scan starts the dissolve"
        );
        assert_eq!(
            state.scan_fade_epoch, state.reveal_generation,
            "the dissolve belongs to the generation it reveals"
        );
    }

    #[test]
    fn entering_a_drive_leaves_the_block_listing_and_its_filter() {
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
                        this.block_listing = Some(crate::shell::block_listing::BlockListing::new(
                            r"C:\Windows".to_string(),
                            2,
                            Vec::new(),
                        ));
                        this.query.search_input.set_text("amd64");
                        this.open_drive_at('D', r"D:\Games", window, cx);
                        assert!(this.block_listing.is_none());
                        assert!(this.query.search_input.text.is_empty());
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

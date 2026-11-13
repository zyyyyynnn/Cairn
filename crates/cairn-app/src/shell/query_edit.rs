// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Query text editing and clipboard: copy, cut, paste, undo and redo in the search box, plus the
//! keyboard edit keys the focused field forwards here.

use crate::app::CairnApp;
use crate::shell::commands::{
    CopyFileNames, CopyTextOrPaths, CutQueryText, PasteQueryText, RedoQueryText, UndoQueryText,
};
use crate::shell::menu_actions::copy_lines;
use gpui_kit::gpui::*;

impl CairnApp {
    pub fn copy_file_names(&mut self, _: &CopyFileNames, _w: &mut Window, cx: &mut Context<Self>) {
        let names = self.collect_selected_names();
        copy_lines(&names);
        cx.notify();
    }

    pub fn copy_text_or_paths(
        &mut self,
        _: &CopyTextOrPaths,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_copy_text_or_paths(cx);
    }

    pub fn paste_query_text(
        &mut self,
        _: &PasteQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.apply_query_paste(cx) {
            self.query.search_focus.focus(window, cx);
        }
    }

    pub fn undo_query_text(
        &mut self,
        _: &UndoQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.apply_query_undo(cx) {
            self.query.search_focus.focus(window, cx);
        }
    }

    pub fn redo_query_text(
        &mut self,
        _: &RedoQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.apply_query_redo(cx) {
            self.query.search_focus.focus(window, cx);
        }
    }

    pub fn cut_query_text(
        &mut self,
        _: &CutQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.apply_query_cut(cx) {
            self.query.search_focus.focus(window, cx);
        }
    }

    fn apply_copy_text_or_paths(&mut self, cx: &mut Context<Self>) {
        match self.selected_query_text() {
            Some(text) => copy_lines(&[text]),
            None => copy_lines(&self.collect_selected_paths()),
        }
        cx.notify();
    }

    fn apply_query_paste(&mut self, cx: &mut Context<Self>) -> bool {
        if self.drive_view.is_some() {
            return false;
        }
        let Some(text) = crate::shell::os::get_clipboard_text() else {
            return false;
        };
        let text = text.replace("\r\n", " ").replace('\n', " ");
        if text.is_empty() {
            return false;
        }
        self.query
            .search_input
            .handle_replace_text_in_range(None, &text);
        self.schedule_text_search(cx);
        cx.notify();
        true
    }

    fn apply_query_undo(&mut self, cx: &mut Context<Self>) -> bool {
        if self.drive_view.is_some() || !self.query.search_input.undo() {
            return false;
        }
        self.schedule_text_search(cx);
        cx.notify();
        true
    }

    fn apply_query_redo(&mut self, cx: &mut Context<Self>) -> bool {
        if self.drive_view.is_some() || !self.query.search_input.redo() {
            return false;
        }
        self.schedule_text_search(cx);
        cx.notify();
        true
    }

    fn apply_query_cut(&mut self, cx: &mut Context<Self>) -> bool {
        if self.drive_view.is_some() {
            return false;
        }
        let Some(text) = self.selected_query_text() else {
            return false;
        };
        let _ = crate::shell::os::set_clipboard_text(&text);
        self.query.search_input.delete_backward();
        self.schedule_text_search(cx);
        cx.notify();
        true
    }

    pub fn selected_query_text(&self) -> Option<String> {
        let (start, end) = self.query.search_input.normalized_selection();
        if start >= end {
            return None;
        }
        Some(self.query.search_input.text[start..end].to_string())
    }

    pub fn handle_query_edit_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let shift = event.keystroke.modifiers.shift;
        let ctrl = event.keystroke.modifiers.control;

        if ctrl && !shift && key == "z" {
            self.apply_query_undo(cx);
            return;
        }
        if (ctrl && key == "y") || (ctrl && shift && key == "z") {
            self.apply_query_redo(cx);
            return;
        }
        if (ctrl && key == "x") || (shift && key == "delete") {
            self.apply_query_cut(cx);
            return;
        }
        if (ctrl && key == "c") || (ctrl && key == "insert") {
            self.apply_copy_text_or_paths(cx);
            return;
        }
        if (ctrl && key == "v") || (shift && key == "insert") {
            self.apply_query_paste(cx);
            return;
        }
        if ctrl && key == "backspace" {
            self.query.search_input.delete_word_backward();
            self.schedule_text_search(cx);
            cx.notify();
            return;
        }
        if ctrl && key == "delete" {
            self.query.search_input.delete_word_forward();
            self.schedule_text_search(cx);
            cx.notify();
            return;
        }
        if ctrl && key == "left" {
            self.query.search_input.move_cursor_word_left(shift);
            cx.notify();
            return;
        }
        if ctrl && key == "right" {
            self.query.search_input.move_cursor_word_right(shift);
            cx.notify();
            return;
        }

        // Cursor navigation and selection updates adjust cursor/highlight only.
        // Redrawing requires `cx.notify()`, but must NEVER trigger a background query refresh.
        match key {
            "left" => {
                self.query.search_input.move_cursor_left(shift);
                cx.notify();
                return;
            }
            "right" => {
                self.query.search_input.move_cursor_right(shift);
                cx.notify();
                return;
            }
            "home" => {
                self.query.search_input.move_cursor_home(shift);
                cx.notify();
                return;
            }
            "end" => {
                self.query.search_input.move_cursor_end(shift);
                cx.notify();
                return;
            }
            "backspace" => self.query.search_input.delete_backward(),
            "delete" => self.query.search_input.delete_forward(),
            _ => return,
        }

        self.schedule_text_search(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod query_edit_tests {
    use crate::app::CairnApp;
    use gpui_kit::gpui::AppContext as _;

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn selected_query_text_supports_both_forward_and_backward_selection() {
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

                    app.update(cx, |this, _| {
                        this.query.search_input.text = "hello world".to_string();

                        // Forward selection "world" (6..11)
                        this.query.search_input.selection = 6..11;
                        assert_eq!(this.selected_query_text().as_deref(), Some("world"));

                        // Backward selection "world" (11..6)
                        this.query.search_input.selection = 11..6;
                        assert_eq!(
                            this.selected_query_text().as_deref(),
                            Some("world"),
                            "backward selection must return normalized selected text"
                        );

                        // Empty selection (6..6)
                        this.query.search_input.selection = 6..6;
                        assert_eq!(this.selected_query_text(), None);

                        // Mouse click anchor & selection behavior
                        // 1. Single click at index 6 sets anchor and cursor
                        this.handle_search_bar_mouse_down(6, 1, false);
                        assert_eq!(this.query.text_selection_anchor, Some(6));
                        assert_eq!(this.query.search_input.selection, 6..6);

                        // 2. Shift+Click to index 11 expands selection, keeps anchor
                        this.handle_search_bar_mouse_down(11, 1, true);
                        assert_eq!(this.query.text_selection_anchor, Some(6));
                        assert_eq!(this.query.search_input.selection, 6..11);

                        // 3. Shift+Click backwards to index 0 creates backward selection
                        this.handle_search_bar_mouse_down(0, 1, true);
                        assert_eq!(this.query.text_selection_anchor, Some(6));
                        assert_eq!(this.query.search_input.selection, 6..0);

                        // 4. Double-click selects word and CLEARS anchor so jitter cannot wipe it
                        this.handle_search_bar_mouse_down(7, 2, false);
                        assert_eq!(
                            this.query.text_selection_anchor, None,
                            "double click must clear text_selection_anchor"
                        );
                        assert_eq!(this.query.search_input.selection, 6..11);

                        // 5. Triple-click selects all and CLEARS anchor
                        this.handle_search_bar_mouse_down(2, 3, false);
                        assert_eq!(
                            this.query.text_selection_anchor, None,
                            "triple click must clear text_selection_anchor"
                        );
                        assert_eq!(
                            this.query.search_input.selection,
                            0..this.query.search_input.text.len()
                        );
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

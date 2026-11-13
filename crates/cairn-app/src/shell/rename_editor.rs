// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-place rename editor: opening on the single selected result, submitting the typed name to
//! the shell, and settling the row and focus when the write answers (UI-05).

use crate::app::{join_row_path, CairnApp};
use crate::shell::commands::RenameSelected;
use crate::shell::os::{rename_path, BinErrorKind};
use crate::shell::rename::{RenameDraft, RenameError};
use fluent_bundle::FluentArgs;
use gpui_kit::gpui::*;

impl CairnApp {
    /// Opens the editor on the selected row with its name stem selected. A second request while
    /// the editor is open returns focus to it instead of discarding what was typed.
    pub fn rename_selected(
        &mut self,
        _: &RenameSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rename.in_flight {
            return;
        }
        let Some(ix) = self.rename_target_ix() else {
            return;
        };
        let Some(row) = self.results.table.row(ix) else {
            return;
        };
        let path = join_row_path(&row.path, &row.name);
        let current = row.name.clone();
        let input = self.rename.input.clone();
        if self
            .rename
            .draft
            .as_ref()
            .is_some_and(|draft| draft.path == path)
        {
            // A second request on the same row returns to what was typed.
            input.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        // A draft belongs to the row it was opened on; a request on another row
        // replaces it instead of silently editing the old target.
        if self.rename.draft.take().is_some() {
            self.dismiss_rename_error_toast(cx);
        }
        let draft = RenameDraft::begin(path, current.clone());
        let stem = draft.stem_range();
        input.update(cx, |input, cx| {
            input.set_value(current, window, cx);
            input.set_selected_range(stem, cx);
        });
        self.rename.draft = Some(draft);
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Drops the draft; the editor is a transient surface, not a location.
    pub(crate) fn discard_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.draft.take().is_some() {
            self.dismiss_rename_error_toast(cx);
        }
    }

    /// Drops the draft when the selection no longer sits on its row, so Enter,
    /// Escape and the row commands act on the visible selection.
    pub(crate) fn discard_rename_off_row(&mut self, cx: &mut Context<Self>) {
        let Some(draft_path) = self.rename.draft.as_ref().map(|draft| draft.path.clone()) else {
            return;
        };
        let selected = self
            .rename_target_ix()
            .and_then(|ix| self.results.table.row(ix))
            .map(|row| join_row_path(&row.path, &row.name));
        if selected.as_deref() != Some(draft_path.as_str()) {
            self.discard_rename(cx);
        }
    }

    /// Drops the draft when a reloaded window no longer contains its row; the
    /// editor cannot stay open on a row the table no longer shows.
    pub(crate) fn discard_rename_if_row_unloaded(&mut self, cx: &mut Context<Self>) {
        let Some(draft_path) = self.rename.draft.as_ref().map(|draft| draft.path.clone()) else {
            return;
        };
        let listed = self
            .results
            .table
            .loaded_range()
            .filter_map(|ix| self.results.table.row(ix))
            .any(|row| join_row_path(&row.path, &row.name) == draft_path);
        if !listed {
            self.discard_rename(cx);
        }
    }

    /// Keeps the draft's text in step with the input and retires the previous rejection as soon as
    /// the person edits.
    pub(crate) fn rename_text_changed(&mut self, cx: &mut Context<Self>) {
        if self.rename.draft.is_none() {
            return;
        }
        let text = self.rename.input.read(cx).value().to_string();
        if let Some(draft) = self.rename.draft.as_mut() {
            draft.set_text(text);
        }
        self.dismiss_rename_error_toast(cx);
        cx.notify();
    }

    /// Closes the editor without writing and hands the keyboard back to the
    /// table. Escape is the only cancel; a blur keeps the draft.
    pub(crate) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.draft.take().is_none() {
            return;
        }
        self.dismiss_rename_error_toast(cx);
        self.results.results_focus.focus(window, cx);
        cx.notify();
    }

    /// Validates the edited name and starts the shell write on its own COM apartment. The draft
    /// stays open with a visible error when the name is rejected or the shell refuses the write.
    pub(crate) fn commit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.in_flight {
            return;
        }
        let Some(draft) = self.rename.draft.as_mut() else {
            return;
        };
        let text = self.rename.input.read(cx).value().to_string();
        draft.set_text(text);
        let new_name = match draft.commit_candidate() {
            Ok(name) => name,
            Err(error) => {
                self.show_rename_error(error, cx);
                cx.notify();
                return;
            }
        };
        let old_path = draft.path.clone();
        self.rename.in_flight = true;
        cx.notify();

        let thread_path = old_path.clone();
        let thread_name = new_name.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(rename_path(
                std::path::Path::new(&thread_path),
                &thread_name,
            ));
        });
        cx.spawn(async move |this, cx| {
            let outcome = rx.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.rename.in_flight = false;
                match outcome {
                    Ok(Ok(())) => {
                        let editor_open = this.rename.draft.take().is_some();
                        if !this.recycle_bin.is_open() {
                            this.results.table.rename_row(&old_path, &new_name);
                        }
                        if let Some(listing) = this.block_listing.as_mut() {
                            listing.rename_path(&old_path, &new_name);
                        }
                        this.dismiss_rename_error_toast(cx);
                        // Escape during the write may have closed the editor; only a still-open
                        // one returns the keyboard to the table.
                        if editor_open {
                            this.results.results_focus.focus(window, cx);
                        }
                    }
                    Ok(Err(RenameError::Cancelled)) => {
                        if let Some(draft) = this.rename.draft.as_mut() {
                            draft.fail(RenameError::Cancelled);
                        }
                    }
                    Ok(Err(error)) => {
                        if let Some(draft) = this.rename.draft.as_mut() {
                            draft.fail(error.clone());
                        }
                        this.show_rename_error(error, cx);
                    }
                    Err(_) => {
                        let error = RenameError::Shell {
                            message: "the rename task ended before reporting".to_string(),
                        };
                        if let Some(draft) = this.rename.draft.as_mut() {
                            draft.fail(error.clone());
                        }
                        this.show_rename_error(error, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_rename_error(&mut self, error: RenameError, cx: &mut Context<Self>) {
        if matches!(error, RenameError::Cancelled) {
            return;
        }
        let title = self.i18n.get_text("rename_failed_title");
        let message = self.rename_error_message(&error);
        self.show_error_toast(title, message, cx);
    }

    fn dismiss_rename_error_toast(&mut self, cx: &mut Context<Self>) {
        let title = self.i18n.get_text("rename_failed_title");
        self.dismiss_toast_titled(&title, cx);
    }

    fn rename_error_message(&self, error: &RenameError) -> String {
        match error {
            RenameError::InvalidChar(invalid) => {
                if invalid.is_control() {
                    return self.i18n.get_text("rename_error_control_char");
                }
                let mut args = FluentArgs::new();
                args.set("char", invalid.to_string());
                self.i18n
                    .get_text_with_args("rename_error_invalid_char", &args)
            }
            RenameError::Shell { message } => {
                // The raw diagnostic is for the log; the toast names a remedy.
                tracing::warn!("rename failed: {message}");
                self.i18n.get_text(shell_failure_key(message))
            }
            RenameError::Cancelled => String::new(),
            rule => rule
                .message_key()
                .map(|key| self.i18n.get_text(key))
                .unwrap_or_default(),
        }
    }
}

/// The localized remedy for a shell refusal; the raw reason stays in the log.
fn shell_failure_key(reason: &str) -> &'static str {
    match BinErrorKind::from_reason(reason) {
        BinErrorKind::AccessDenied => "rename_error_access_denied",
        BinErrorKind::ReadOnly => "rename_error_read_only",
        BinErrorKind::PathTooLong | BinErrorKind::PathTooDeep => "rename_error_path_too_long",
        BinErrorKind::Conflict => "rename_error_target_exists",
        _ => "rename_error_shell",
    }
}

#[cfg(test)]
mod tests {
    use super::shell_failure_key;
    use crate::app::CairnApp;
    use crate::shell::commands::{EscapeQuery, OpenSelected, RenameSelected};
    use core::prelude::rust_2021::test;
    use gpui_kit::gpui::AppContext as _;

    #[test]
    fn a_shell_refusal_names_a_localized_remedy() {
        for (reason, key) in [
            (
                "PerformOperations: 0x80270021",
                "rename_error_access_denied",
            ),
            (
                "CoCreateInstance(FileOperation): Access is denied. (0x80070005)",
                "rename_error_access_denied",
            ),
            ("PerformOperations: 0x8027003f", "rename_error_read_only"),
            (
                "PerformOperations: 0x80270038",
                "rename_error_path_too_long",
            ),
            (
                "PerformOperations: 0x80270029",
                "rename_error_target_exists",
            ),
            ("something unexpected", "rename_error_shell"),
        ] {
            assert_eq!(shell_failure_key(reason), key, "{reason}");
        }
    }

    fn row(name: &str) -> cairn_protocol::message::FileRowData {
        cairn_protocol::message::FileRowData {
            frn: 0,
            name: name.to_string(),
            path: r"C:\work".to_string(),
            size: 1,
            modified_ts: 0,
            created_ts: 0,
            is_dir: false,
            flags: 0,
            name_highlights: Vec::new(),
            path_highlights: Vec::new(),
            snippet: None,
        }
    }

    #[test]
    fn a_rename_request_owns_one_row_and_replaces_another_rows_draft() {
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
                        this.results
                            .table
                            .set_rows(vec![row("a.txt"), row("b.txt")], 2);
                        this.results.table.select_row(0, false);
                        this.rename_selected(&RenameSelected, window, cx);
                        assert_eq!(
                            this.rename.draft.as_ref().map(|draft| draft.path.clone()),
                            Some(r"C:\work\a.txt".to_string())
                        );

                        // A request on another row replaces the draft; the old
                        // target is never renamed by a later Enter.
                        this.results.table.select_row(1, false);
                        this.rename_selected(&RenameSelected, window, cx);
                        assert_eq!(
                            this.rename.draft.as_ref().map(|draft| draft.path.clone()),
                            Some(r"C:\work\b.txt".to_string())
                        );

                        this.escape_query(&EscapeQuery, window, cx);
                        assert!(this.rename.draft.is_none());

                        // Arrow selection moves to another row and closes the
                        // editor instead of leaving an orphan that eats Escape.
                        this.results.table.select_row(1, false);
                        this.rename_selected(&RenameSelected, window, cx);
                        assert!(this.rename.draft.is_some());
                        this.move_selection(
                            false,
                            false,
                            crate::app::layout::row_height(window),
                            cx,
                        );
                        assert!(this.rename.draft.is_none());

                        // Open is a read command: an editor left without focus
                        // is dismissed, never submitted by it. The row list is
                        // emptied so Open has no real file to launch.
                        this.results.table.select_row(0, false);
                        this.rename_selected(&RenameSelected, window, cx);
                        this.results.table.set_rows(Vec::new(), 0);
                        this.query.search_focus.focus(window, cx);
                        this.open_selected(&OpenSelected, window, cx);
                        assert!(this.rename.draft.is_none());
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

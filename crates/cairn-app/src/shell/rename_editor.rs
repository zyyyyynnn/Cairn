// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-place rename editor: opening on the single selected result, submitting the typed name to
//! the shell, and settling the row and focus when the write answers (UI-05).

use crate::app::{join_row_path, CairnApp};
use crate::shell::commands::RenameSelected;
use crate::shell::os::rename_path;
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
        let input = self.rename.input.clone();
        if self.rename.draft.is_some() {
            input.read(cx).focus_handle(cx).focus(window, cx);
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

    /// Closes the editor without writing. `restore_focus` hands the keyboard back to the table when
    /// the person asked to cancel; a blur leaves focus wherever it moved.
    pub(crate) fn cancel_rename(
        &mut self,
        restore_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rename.draft.take().is_none() {
            return;
        }
        self.dismiss_rename_error_toast(cx);
        if restore_focus {
            self.results.results_focus.focus(window, cx);
        }
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
                        this.results.table.rename_row(&old_path, &new_name);
                        this.dismiss_rename_error_toast(cx);
                        // A blur already handed the keyboard somewhere else; only a submission
                        // that was still being read takes the focus back to the table.
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
                let mut args = FluentArgs::new();
                args.set("char", invalid.to_string());
                self.i18n
                    .get_text_with_args("rename_error_invalid_char", &args)
            }
            RenameError::Shell { message } => message.clone(),
            RenameError::Cancelled => String::new(),
            rule => rule
                .message_key()
                .map(|key| self.i18n.get_text(key))
                .unwrap_or_default(),
        }
    }
}

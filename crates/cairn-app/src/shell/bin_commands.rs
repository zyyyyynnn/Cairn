// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Recycle Bin write orchestration: putting items back, deleting them permanently, emptying the
//! bin, and moving paths to the bin. Every write runs on its own COM apartment and installs the
//! listing that follows it, reconciled against the shell's fresh read.

use crate::app::CairnApp;
use crate::search::ResultSetChange;
use crate::search::filters::FilterCategory;
use crate::shell::commands::{DeleteSelected, RestoreAll, RestoreSelected};
use crate::shell::dialogs::{self, confirmation_alert, confirmation_alert_with_variant};
use crate::shell::location::ActiveLocation;
use crate::shell::os::{
    BinOperationError, RecycleBinItem, delete_permanently_from_recycle_bin, delete_to_recycle_bin,
    empty_recycle_bin, restore_from_recycle_bin,
};
use fluent_bundle::FluentArgs;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::gpui::*;

/// The shell can report a batch as successful while a progress dialog skipped one item. The
/// fresh listing is the authority: an item still present after the operation is a failure.
fn reconcile_bin_items(
    outcome: Result<(), BinOperationError>,
    listing: &Result<Vec<RecycleBinItem>, String>,
    requested: &[(String, String)],
) -> Result<(), BinOperationError> {
    match (outcome, listing) {
        (Ok(()), Ok(items)) => {
            let live: std::collections::HashSet<String> =
                items.iter().map(RecycleBinItem::identity_key).collect();
            let failed: Vec<String> = requested
                .iter()
                .filter(|(key, _)| live.contains(key))
                .map(|(_, name)| name.clone())
                .collect();
            if failed.is_empty() {
                Ok(())
            } else {
                Err(BinOperationError::failed(
                    "some items were not processed",
                    failed,
                ))
            }
        }
        (outcome, _) => outcome,
    }
}

fn reconcile_empty_bin(
    outcome: Result<(), BinOperationError>,
    listing: &Result<Vec<RecycleBinItem>, String>,
) -> Result<(), BinOperationError> {
    match (outcome, listing) {
        (Ok(()), Ok(items)) if !items.is_empty() => Err(BinOperationError::failed(
            "some items still remain",
            items.iter().map(|item| item.name.clone()).collect(),
        )),
        (outcome, _) => outcome,
    }
}

impl CairnApp {
    /// Opens the Recycle Bin view: the shell reports the bin's contents and
    /// the table renders them. Any query leaves the view.
    pub fn open_recycle_bin(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) {
        // The bin is a location, so its first listing fades in instead of
        // replacing the table text in place.
        self.discard_rename(cx);
        self.close_group();
        // Keep the query the person left behind in the current history entry
        // before the bin replaces the search box with a location.
        self.update_current_nav_state_text();
        // Invalidate any in-flight query: its answer belongs to the table and
        // must not replace the listing that is about to be shown.
        self.take_table_for_local_rows();
        self.normalize_local_listing_sort();
        // The bin is a location, not a search scope. The search box starts
        // empty and filters this listing in place; it must not run an index
        // query that would replace the location.
        if !self.query.search_input.text.is_empty() {
            self.query.search_input.clear();
        }
        self.query.active_filter = FilterCategory::All;
        // The bin is a location of its own, so no index source scopes its listing.
        self.query.source_scope = None;
        self.results.rows_scroll.set_offset(Point::default());
        self.results.table.scroll_top = 0.0;
        // `begin_recycle_bin_load` opens the view and resets every part of the bin's state, so
        // the location starts from one clean listing instead of a half-cleared one.
        self.begin_recycle_bin_load(cx);
        cx.notify();
    }

    /// The Delete command: moves a table selection to the Recycle Bin through
    /// the native file pipeline. Inside the bin the same key means a permanent
    /// delete, confirmed by the same project dialog before the shell runs
    /// without its own prompt.
    pub fn delete_selected(
        &mut self,
        _: &DeleteSelected,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(items) = self.selected_bin_items() {
            self.open_bin_delete_confirmation(items, window, cx);
            return;
        }
        self.open_delete_confirmation(self.collect_selected_paths(), window, cx);
    }

    /// Asks before permanently deleting Recycle Bin items. The copy names the
    /// irreversible result, then the shell runs with its own confirmation
    /// suppressed; progress and error UI still belong to Windows.
    fn open_bin_delete_confirmation(
        &mut self,
        items: Vec<std::sync::Arc<RecycleBinItem>>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if items.is_empty() {
            return;
        }
        self.close_group();
        let title = if items.len() == 1 {
            let mut args = FluentArgs::new();
            args.set("name", items[0].name.as_str());
            self.i18n
                .get_text_with_args("recycle_bin_delete_confirm_title", &args)
        } else {
            let mut args = FluentArgs::new();
            args.set("count", items.len() as i64);
            self.i18n
                .get_text_with_args("recycle_bin_delete_confirm_title_many", &args)
        };
        let description_key = if items.len() == 1 {
            "recycle_bin_delete_confirm_description"
        } else {
            "recycle_bin_delete_confirm_description_many"
        };
        let description = self.i18n.get_text(description_key);
        let ok = self.i18n.get_text("recycle_bin_delete_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, app| {
            confirmation_alert(alert, window, app, ok.clone(), cancel.clone())
                .child(dialogs::body(title.clone(), description.clone(), window))
                .confirm()
                .on_ok({
                    let view = view.clone();
                    let items = items.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.delete_bin_items_permanently(items.clone(), cx);
                        });
                        true
                    }
                })
        });
    }

    /// Runs the confirmed permanent delete on the dedicated COM apartment and
    /// installs the listing that follows it.
    pub(crate) fn delete_bin_items_permanently(
        &mut self,
        items: Vec<std::sync::Arc<RecycleBinItem>>,
        cx: &mut Context<'_, Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let names: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        let requested: Vec<(String, String)> = items
            .iter()
            .map(|item| (item.identity_key(), item.name.clone()))
            .collect();
        self.spawn_bin_write(
            "recycle_bin_delete_failed",
            names,
            false,
            move |owner| {
                let outcome = delete_permanently_from_recycle_bin(&items, owner);
                let listing = crate::shell::os::list_recycle_bin();
                let outcome = reconcile_bin_items(outcome, &listing, &requested);
                (outcome, listing)
            },
            cx,
        );
    }

    /// Asks before deleting paths. The shell confirmation is suppressed for
    /// this routine action so the question uses Cairn's own dialog; the shell
    /// still owns progress and error UI.
    pub(crate) fn open_delete_confirmation(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.close_group();
        let title = if paths.len() == 1 {
            let name = std::path::Path::new(&paths[0])
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(paths[0].as_str());
            let mut args = FluentArgs::new();
            args.set("name", name);
            self.i18n.get_text_with_args("delete_confirm_title", &args)
        } else {
            let mut args = FluentArgs::new();
            args.set("count", paths.len() as i64);
            self.i18n
                .get_text_with_args("delete_confirm_title_many", &args)
        };
        let description_key = if paths.len() == 1 {
            "delete_confirm_description"
        } else {
            "delete_confirm_description_many"
        };
        let description = self.i18n.get_text(description_key);
        let ok = self.i18n.get_text("delete_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, app| {
            confirmation_alert(alert, window, app, ok.clone(), cancel.clone())
                .child(dialogs::body(title.clone(), description.clone(), window))
                .confirm()
                .on_ok({
                    let view = view.clone();
                    let paths = paths.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| this.delete_paths(paths.clone(), cx));
                        true
                    }
                })
        });
    }

    /// Runs one bin write on a dedicated COM apartment, then installs the listing that follows it.
    /// The UI thread is only a coordinator: the native progress/confirmation UI stays owned by the
    /// app window, and the generation gate drops a result after the bin has been left.
    fn spawn_bin_write<F>(
        &mut self,
        title_key: &'static str,
        fallback_names: Vec<String>,
        reset_scroll: bool,
        task: F,
        cx: &mut Context<'_, Self>,
    ) where
        F: FnOnce(
                isize,
            ) -> (
                Result<(), BinOperationError>,
                Result<Vec<RecycleBinItem>, String>,
            ) + Send
            + 'static,
    {
        let Some(generation) = self.recycle_bin.begin_mutation() else {
            return;
        };
        let owner = crate::shell::os::active_window_handle();
        self.sync_bin_selection_keys();
        let anchor = self.explicit_bin_selection_indices().first().copied();
        cx.notify();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(task(owner));
        });
        cx.spawn(async move |this, cx| {
            let Ok((outcome, listing)) = rx.await else {
                let _ = this.update(cx, |this, cx| {
                    if this.recycle_bin.generation() == generation {
                        this.recycle_bin.set_mutating(false);
                        cx.notify();
                    }
                });
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_recycle_bin_load(generation, listing, reset_scroll, anchor, cx);
                if let Err(failure) = outcome
                    && !failure.is_cancelled()
                {
                    this.notify_bin_operation_error(title_key, &fallback_names, &failure, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts the selected bin items back through the shell's native restore
    /// pipeline; the list is re-read so a conflict or a missing original
    /// directory cannot leave a row claiming a state the shell does not hold.
    pub fn restore_selected(
        &mut self,
        _: &RestoreSelected,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(items) = self.selected_bin_items() else {
            return;
        };
        self.restore_bin_items(items, cx);
    }

    /// Restores an explicit set of bin items.
    pub(crate) fn restore_bin_items(
        &mut self,
        items: Vec<std::sync::Arc<RecycleBinItem>>,
        cx: &mut Context<'_, Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let names: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        let requested: Vec<(String, String)> = items
            .iter()
            .map(|item| (item.identity_key(), item.name.clone()))
            .collect();
        self.spawn_bin_write(
            "recycle_bin_restore_failed",
            names,
            false,
            move |owner| {
                let outcome = restore_from_recycle_bin(&items, owner);
                let listing = crate::shell::os::list_recycle_bin();
                let outcome = reconcile_bin_items(outcome, &listing, &requested);
                (outcome, listing)
            },
            cx,
        );
    }

    /// Action handler for the `RestoreAll` command.
    pub fn restore_all_action(
        &mut self,
        _: &RestoreAll,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.restore_all(window, cx);
    }

    /// Confirms restoring all items from the Recycle Bin through Cairn's unified dialog style
    /// before delegating to the shell's restore pipeline.
    pub(super) fn restore_all(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.active_location().is_recycle_bin()
            || self.recycle_bin.is_loading()
            || self.recycle_bin.all().is_empty()
        {
            return;
        }
        self.close_group();
        let title = self.i18n.get_text("recycle_bin_restore_all_confirm_title");
        let description = self
            .i18n
            .get_text("recycle_bin_restore_all_confirm_description");
        let ok = self.i18n.get_text("recycle_bin_restore_all_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, app| {
            confirmation_alert_with_variant(
                alert,
                window,
                app,
                ok.clone(),
                cancel.clone(),
                ButtonVariant::Primary,
            )
            .child(dialogs::body(title.clone(), description.clone(), window))
            .confirm()
            .on_ok({
                let view = view.clone();
                move |_, _, cx| {
                    view.update(cx, Self::execute_restore_all);
                    true
                }
            })
        });
    }

    /// Dispatches restore of all items in the bin.
    pub fn execute_restore_all(&mut self, cx: &mut Context<'_, Self>) {
        if !self.active_location().is_recycle_bin() || self.recycle_bin.is_loading() {
            return;
        }
        let items = self.recycle_bin.all().to_vec();
        self.restore_bin_items(items, cx);
    }

    /// Confirms emptying the Recycle Bin through Cairn's unified dialog style before
    /// dispatching the operation to the shell.
    pub(super) fn empty_bin(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.active_location().is_recycle_bin()
            || self.recycle_bin.is_loading()
            || self.recycle_bin.all().is_empty()
        {
            return;
        }
        self.close_group();
        let title = self.i18n.get_text("recycle_bin_empty_confirm_title");
        let description = self.i18n.get_text("recycle_bin_empty_confirm_description");
        let ok = self.i18n.get_text("recycle_bin_empty_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, app| {
            confirmation_alert(alert, window, app, ok.clone(), cancel.clone())
                .child(dialogs::body(title.clone(), description.clone(), window))
                .confirm()
                .on_ok({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, Self::execute_empty_bin);
                        true
                    }
                })
        });
    }

    /// Empties the bin through the shell's own command, then re-reads it. The loading flag is
    /// set before the background operation starts, so it is also the in-flight guard: a second
    /// invocation before the first listing lands must not start a second shell empty.
    pub fn execute_empty_bin(&mut self, cx: &mut Context<'_, Self>) {
        if !self.active_location().is_recycle_bin() || self.recycle_bin.is_loading() {
            return;
        }
        self.spawn_bin_write(
            "recycle_bin_empty_failed",
            Vec::new(),
            true,
            |owner| {
                let outcome = empty_recycle_bin(owner);
                let listing = crate::shell::os::list_recycle_bin();
                let outcome = reconcile_empty_bin(outcome, &listing);
                (outcome, listing)
            },
            cx,
        );
    }

    /// Deletes the given paths to the Recycle Bin and reflects the result in
    /// the result table. Shared by the Delete command and the row actions.
    pub fn delete_paths(&mut self, paths: Vec<String>, cx: &mut Context<'_, Self>) {
        if paths.is_empty() || self.delete_in_flight {
            return;
        }
        let names: Vec<String> = paths
            .iter()
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string()
            })
            .collect();
        let task_paths = paths.clone();
        let task_names = names.clone();
        let owner = crate::shell::os::active_window_handle();
        self.delete_in_flight = true;
        cx.notify();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let delete_result = delete_to_recycle_bin(&task_paths, owner);
            // Single pass over the filesystem to determine which paths still exist, so a shell
            // batch that reports success while an item was skipped is reported as a failure.
            let remaining_names: Vec<String> = task_paths
                .into_iter()
                .zip(task_names)
                .filter(|(path, _)| std::path::Path::new(path).exists())
                .map(|(_, name)| name)
                .collect();

            let outcome = match delete_result {
                Ok(()) => {
                    if remaining_names.is_empty() {
                        Ok(())
                    } else {
                        Err(BinOperationError::failed(
                            "some items could not be deleted",
                            remaining_names,
                        ))
                    }
                }
                Err(failure) => Err(failure),
            };
            let _ = tx.send(outcome);
        });
        cx.spawn(async move |this, cx| {
            let received = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.delete_in_flight = false;
                if let Ok(outcome) = received {
                    if let Err(failure) = &outcome {
                        if failure.is_cancelled() {
                            cx.notify();
                            return;
                        }
                        this.notify_bin_operation_error("delete_failed_title", &names, failure, cx);
                    }
                    match this.active_location() {
                        ActiveLocation::Index => {
                            this.request_result_set(ResultSetChange::Replace, cx);
                        }
                        ActiveLocation::RecycleBin => {
                            if !this.recycle_bin.is_loading() {
                                this.begin_recycle_bin_load(cx);
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod bin_command_tests {
    use super::{reconcile_bin_items, reconcile_empty_bin};
    use crate::app::CairnApp;
    use crate::shell::os::{BinOperationError, RecycleBinItem};
    use gpui_kit::gpui::AppContext as _;

    fn item(name: &str) -> RecycleBinItem {
        RecycleBinItem {
            name: name.to_string(),
            name_lower: name.to_lowercase(),
            extension_lower: crate::shell::os::extract_extension_lower(name),
            original_dir: r"C:\work".to_string(),
            original_dir_lower: r"c:\work".to_string(),
            fs_path: format!(r"C:\$Recycle.Bin\SID\$R{name}"),
            deleted_ts: 1,
            size: 1,
            is_dir: false,
        }
    }

    #[test]
    fn reconciliation_blames_only_items_still_in_the_listing() {
        let requested = vec![
            (item("a.txt").identity_key(), "a.txt".to_string()),
            (item("b.txt").identity_key(), "b.txt".to_string()),
        ];
        let listing = Ok(vec![item("a.txt")]);
        let outcome = reconcile_bin_items(Ok(()), &listing, &requested);
        match outcome {
            Err(BinOperationError::Failed { items, .. }) => {
                assert_eq!(items, vec!["a.txt".to_string()]);
            }
            other => panic!("expected the live item to be blamed, got {other:?}"),
        }
        assert_eq!(
            reconcile_bin_items(Ok(()), &Ok(Vec::new()), &requested),
            Ok(())
        );
        assert_eq!(
            reconcile_bin_items(Err(BinOperationError::Cancelled), &listing, &requested),
            Err(BinOperationError::Cancelled)
        );
        assert!(reconcile_empty_bin(Ok(()), &listing).is_err());
        assert_eq!(reconcile_empty_bin(Ok(()), &Ok(Vec::new())), Ok(()));
    }

    #[test]
    fn entering_the_bin_keeps_the_query_it_replaces() {
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
                        this.query.search_input.set_text("kept query");
                        this.query.coordinator.set_cancelled(true);
                        this.open_recycle_bin(window, cx);
                        assert_eq!(this.nav_history.current().text, "kept query");
                        assert!(this.query.search_input.text.is_empty());
                        assert!(
                            !this.query.is_cancelled(),
                            "the bin is a new location, not the cancelled query"
                        );
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

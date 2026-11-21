// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Folder sources: the sidebar section that scopes a query to one indexed folder, and the
//! manager dialog that adds, rescans and removes folder sources.

use crate::app::{CairnApp, element_id, layout};
use crate::shell::dialogs;
use crate::shell::i18n::I18nManager;
use crate::shell::theme::{self, ShellTokens};
use cairn_protocol::status::{SourceKind, SourceStatusSummary, VolumeState};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::cell::Cell;
use std::rc::Rc;

/// The folder sources in service order. NTFS volumes never enter the sidebar or the manager:
/// their rows are gone and a folder source is the only source a query can be scoped to.
pub(crate) fn folder_source_summaries(
    sources: &[SourceStatusSummary],
) -> impl Iterator<Item = &SourceStatusSummary> {
    sources
        .iter()
        .filter(|source| source.kind == SourceKind::Folder)
}

/// Fluent message naming one source state. The compact words keep a 208px sidebar row readable
/// while still naming every state the service can report.
pub(crate) fn source_state_key(state: VolumeState) -> &'static str {
    match state {
        VolumeState::Synced => "source_state_synced",
        VolumeState::CatchingUp(_) => "source_state_catching_up",
        VolumeState::Rebuilding => "source_state_rebuilding",
        VolumeState::Offline => "source_state_offline",
        VolumeState::SnapshotMode => "source_state_snapshot",
    }
}

pub(crate) fn source_state_label(i18n: &I18nManager, state: VolumeState) -> String {
    i18n.get_text(source_state_key(state))
}

/// The manager dialog's content. It observes [`CairnApp`] so a sources update from the service
/// rebuilds the list while the modal stays open.
pub(crate) struct FolderSourcesDialogView {
    app: Entity<CairnApp>,
    input: Entity<InputState>,
    _input_events: Subscription,
    _app_changes: Subscription,
}

impl FolderSourcesDialogView {
    pub(crate) fn new(
        app: Entity<CairnApp>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let placeholder = app
            .read(cx)
            .i18n
            .get_text("folder_sources_path_placeholder");
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let input_events = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_add(window, cx),
                InputEvent::Change => cx.notify(),
                InputEvent::Blur | InputEvent::Focus => {}
            },
        );
        let app_changes = cx.observe(&app, |_, _, cx| cx.notify());
        Self {
            app,
            input,
            _input_events: input_events,
            _app_changes: app_changes,
        }
    }

    fn input_value(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn submit_add(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let path = self.input_value(cx);
        if path.is_empty() || self.app.read(cx).service.folder_source_action_pending {
            return;
        }
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.app
            .update(cx, |app, cx| app.add_folder_source(path, cx));
    }

    fn remove_source(
        &mut self,
        source_id: cairn_protocol::message::SourceId,
        path: String,
        cx: &mut Context<'_, Self>,
    ) {
        if self.app.read(cx).service.folder_source_action_pending {
            return;
        }
        self.app
            .update(cx, |app, cx| app.remove_folder_source(source_id, path, cx));
    }

    fn rescan_source(
        &mut self,
        source_id: cairn_protocol::message::SourceId,
        label: String,
        cx: &mut Context<'_, Self>,
    ) {
        let app = self.app.read(cx);
        if app.service.connection.is_control_pending() || app.service.folder_source_action_pending {
            return;
        }
        self.app
            .update(cx, |app, cx| app.rescan_source(source_id, label, cx));
    }

    fn render_source_row(
        &self,
        source: &SourceStatusSummary,
        i18n: &I18nManager,
        tokens: &ShellTokens,
        busy: bool,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let status = source_state_label(i18n, source.status);
        let rescan_label = i18n.get_text("folder_sources_rescan");
        let remove_label = i18n.get_text("folder_sources_remove");
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("path", source.root_path.as_str());
        args.set("state", status.as_str());
        let row_label = i18n.get_text_with_args("folder_source_row_label", &args);
        let source_id = source.source_id;
        let view = cx.entity();
        let view_rescan = view.clone();
        let view_remove = view.clone();
        let rescan_path = source.root_path.clone();
        let remove_path = source.root_path.clone();
        let status_color = if source.status.is_synced() {
            tokens.muted_foreground
        } else {
            tokens.warning_ink
        };

        h_flex()
            .id(element_id(
                "folder-source-row",
                &source.source_id.0.to_string(),
            ))
            .w_full()
            .min_h(rems(layout::CONTROL_HEIGHT))
            .gap_x(rems(layout::INNER_SPACING))
            .items_center()
            .rounded(tokens.radius)
            .hover(|this| this.bg(tokens.row_hover))
            .child(
                Icon::new(IconName::Folder)
                    .size(window.rem_size() * theme::UI_ICON_SIZE)
                    .text_color(tokens.muted_foreground),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_y(rems(layout::SPACE_4))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_sm()
                            .text_color(tokens.foreground)
                            .child(source.root_path.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(rems(theme::SMALL_TEXT_SIZE))
                            .text_color(status_color)
                            .child(status),
                    ),
            )
            .child(
                Button::new(element_id(
                    "folder-source-rescan",
                    &source.source_id.0.to_string(),
                ))
                .ghost()
                .small()
                .icon(Icon::new(IconName::RotateCw).size(window.rem_size() * theme::UI_ICON_SIZE))
                .accessibility_label(rescan_label.clone())
                .tooltip(rescan_label)
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    view_rescan.update(cx, |this, cx| {
                        this.rescan_source(source_id, rescan_path.clone(), cx);
                    });
                }),
            )
            .child(
                Button::new(element_id(
                    "folder-source-remove",
                    &source.source_id.0.to_string(),
                ))
                .ghost()
                .small()
                .icon(Icon::new(IconName::FolderX).size(window.rem_size() * theme::UI_ICON_SIZE))
                .accessibility_label(remove_label.clone())
                .tooltip(remove_label)
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    view_remove.update(cx, |this, cx| {
                        this.remove_source(source_id, remove_path.clone(), cx);
                    });
                }),
            )
            .aria_label(row_label)
            .into_any_element()
    }
}

impl Render for FolderSourcesDialogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let tokens = ShellTokens::current(cx);
        let app = self.app.read(cx);
        let i18n = app.i18n.clone();
        let connected = app.service.connection.is_connected();
        let busy = app.service.folder_source_action_pending
            || app.service.connection.is_control_pending()
            || !connected;
        let sources: Vec<SourceStatusSummary> = folder_source_summaries(&app.service.sources)
            .cloned()
            .collect();

        let add_label = i18n.get_text("folder_sources_add");
        let placeholder = i18n.get_text("folder_sources_path_placeholder");
        let empty_label = i18n.get_text("folder_sources_empty");
        let unavailable_label = i18n.get_text("folder_sources_unavailable");
        let add_disabled = busy || self.input_value(cx).is_empty();
        let view = cx.entity();
        let input_row = h_flex()
            .w_full()
            .items_center()
            .gap_x(rems(layout::INNER_SPACING))
            .child(
                Input::new(&self.input)
                    .small()
                    .flex_1()
                    .min_w_0()
                    .cleanable(true)
                    .aria_label(placeholder)
                    .disabled(busy),
            )
            .child(
                Button::new("folder-source-add")
                    .label(add_label.clone())
                    .with_variant(ButtonVariant::Primary)
                    .small()
                    .accessibility_label(add_label)
                    .disabled(add_disabled)
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| this.submit_add(window, cx));
                    }),
            );

        let list: Vec<AnyElement> = if sources.is_empty() {
            vec![
                div()
                    .py(rems(layout::SPACE_8))
                    .text_sm()
                    .text_color(tokens.muted_foreground)
                    .child(empty_label)
                    .into_any_element(),
            ]
        } else {
            sources
                .iter()
                .map(|source| self.render_source_row(source, &i18n, &tokens, busy, window, cx))
                .collect()
        };

        v_flex()
            .w_full()
            .gap_y(rems(layout::SPACE_8))
            .when(!connected, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(tokens.danger)
                        .child(unavailable_label),
                )
            })
            .child(input_row)
            .child(
                v_flex()
                    .w_full()
                    .gap_y(rems(layout::SPACE_4))
                    .children(list),
            )
    }
}

impl CairnApp {
    /// Applies a click on a folder-source row: the first click limits the current query to that
    /// source, a second click on the same row releases it. The typed text and match flags stay;
    /// the result set is replaced because the place itself changed.
    pub(crate) fn select_folder_source(
        &mut self,
        source_id: cairn_protocol::message::SourceId,
        cx: &mut Context<'_, Self>,
    ) {
        if !self
            .service
            .sources
            .iter()
            .any(|source| source.source_id == source_id && source.kind == SourceKind::Folder)
        {
            return;
        }
        if self.recycle_bin.is_open() {
            self.query.search_input.clear();
        } else {
            self.update_current_nav_state_text();
        }
        self.recycle_bin.leave();
        self.close_group();
        self.query.source_scope = if self.query.source_scope == Some(source_id) {
            None
        } else {
            Some(source_id)
        };
        self.record_nav_state();
        self.request_result_set(crate::search::ResultSetChange::Replace, cx);
        cx.notify();
    }

    /// Opens the folder source manager. The dialog content is its own view so the service's
    /// sources snapshot can rebuild the list without reopening the modal.
    pub(crate) fn open_folder_sources_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.close_group();
        self.refresh_sources(cx);
        let app = cx.entity();
        let view = cx.new(|cx| FolderSourcesDialogView::new(app.clone(), window, cx));
        let focus_input = Rc::new(Cell::new(true));
        let content = view.clone();
        let title = self.i18n.get_text("folder_sources_title");
        let close = self.i18n.get_text("folder_sources_close");
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus_input.replace(false) {
                let input = content.read(cx).input.clone();
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            let surface = cairn_ui::theme::dialog_surface(cx.theme());
            dialog
                .width(window.rem_size() * layout::DIALOG_WIDTH)
                .centered()
                .bg(surface)
                .entrance(DialogEntrance::FadeSlide)
                .title(dialogs::title(title.clone(), window))
                .child(content.clone())
                .footer(
                    DialogFooter::new().child(
                        Button::new("folder-sources-close")
                            .label(close.clone())
                            .with_variant(ButtonVariant::Secondary)
                            .small()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{FolderSourcesDialogView, folder_source_summaries, source_state_key};
    use crate::app::CairnApp;
    use crate::shell::i18n::I18nManager;
    use cairn_protocol::message::SourceId;
    use cairn_protocol::status::{SourceKind, SourceStatusSummary, VolumeState};
    use gpui_kit::gpui::AppContext as _;

    fn source(id: u32, kind: SourceKind, status: VolumeState, root: &str) -> SourceStatusSummary {
        SourceStatusSummary {
            source_id: SourceId(id),
            kind,
            root_path: root.to_string(),
            volume_guid: String::new(),
            status,
            indexed_count: 3,
            live_watch_enabled: true,
        }
    }

    #[test]
    fn only_folder_sources_enter_the_sidebar_and_manager() {
        let sources = vec![
            source(2, SourceKind::NtfsVolume, VolumeState::Synced, r"C:\"),
            source(1000, SourceKind::Folder, VolumeState::Synced, r"D:\Work"),
        ];
        let folders: Vec<u32> = folder_source_summaries(&sources)
            .map(|source| source.source_id.0)
            .collect();
        assert_eq!(folders, vec![1000]);
    }

    #[test]
    fn every_source_state_speaks_in_both_locales() {
        for locale in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(locale);
            for state in [
                VolumeState::Synced,
                VolumeState::CatchingUp(20),
                VolumeState::Rebuilding,
                VolumeState::Offline,
                VolumeState::SnapshotMode,
            ] {
                let key = source_state_key(state);
                assert!(i18n.has_key(key), "{locale} is missing {key}");
            }
            assert!(i18n.has_key("folder_sources_title"));
            assert!(i18n.has_key("folder_sources_path_placeholder"));
            assert!(i18n.has_key("folder_sources_add"));
            assert!(i18n.has_key("folder_sources_remove"));
            assert!(i18n.has_key("folder_sources_rescan"));
            assert!(i18n.has_key("folder_sources_empty"));
            assert!(i18n.has_key("folder_sources_close"));
            assert!(i18n.has_key("folder_source_row_label"));
        }
    }

    fn with_dialog(
        test: impl FnOnce(
            &gpui_kit::gpui::Entity<CairnApp>,
            &gpui_kit::gpui::Entity<FolderSourcesDialogView>,
            &mut gpui_kit::gpui::Window,
            &mut gpui_kit::gpui::App,
        ) + 'static,
    ) {
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
                    let dialog = cx.new(|cx| FolderSourcesDialogView::new(app.clone(), window, cx));
                    test(&app, &dialog, window, cx);
                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn submitting_a_path_starts_one_add_request_and_clears_the_box() {
        with_dialog(|app, dialog, window, cx| {
            dialog.update(cx, |dialog, cx| {
                dialog
                    .input
                    .update(cx, |input, cx| input.set_value(r"D:\Work", window, cx));
                dialog.submit_add(window, cx);
            });

            assert!(
                app.read(cx).service.folder_source_action_pending,
                "the add request owns the manager while the service answers"
            );
            assert_eq!(
                dialog.read(cx).input_value(cx),
                "",
                "a submitted path leaves the box for the next one"
            );

            dialog.update(cx, |dialog, cx| {
                dialog
                    .input
                    .update(cx, |input, cx| input.set_value(r"E:\Other", window, cx));
                dialog.submit_add(window, cx);
            });
            assert_eq!(
                dialog.read(cx).input_value(cx),
                r"E:\Other",
                "a second request waits for the first to answer"
            );

            app.update(cx, |app, _| {
                app.service.folder_source_action_pending = false;
            });
            dialog.update(cx, |dialog, cx| dialog.submit_add(window, cx));
            assert!(app.read(cx).service.folder_source_action_pending);
            assert_eq!(dialog.read(cx).input_value(cx), "");
        });
    }

    #[test]
    fn removing_a_row_starts_a_remove_request() {
        with_dialog(|app, dialog, _window, cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.remove_source(SourceId(1000), r"D:\Work".to_string(), cx);
            });
            assert!(app.read(cx).service.folder_source_action_pending);
        });
    }
}

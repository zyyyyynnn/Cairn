// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Result area empty, content-status, and service-offline state bands.

use crate::app::CairnApp;
use crate::service::service_issue::{self, ServiceBandAction, ServiceIssue};
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::menu_rules::IndexMenuState;
use crate::shell::state_view::{icon_action, render_state_band};
use crate::shell::theme::ShellTokens;
use crate::table::surface::{LocalListingStage, SurfaceLocation};
use cairn_protocol::status::ContentIndexState;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::Disableable as _;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::gpui::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContentQueryNotice {
    Loading,
    Unreadable,
    Disabled,
    Backfilling,
    Error,
}

pub(super) fn content_query_notice(state: &IndexMenuState<'_>) -> Option<ContentQueryNotice> {
    if !state.service_connected {
        return None;
    }
    if state.content.loading {
        return Some(ContentQueryNotice::Loading);
    }
    if state.content_action_failed {
        return Some(ContentQueryNotice::Error);
    }
    if state.content.failed {
        return Some(ContentQueryNotice::Unreadable);
    }
    match state.content_state {
        ContentIndexState::Ready => None,
        ContentIndexState::Disabled => Some(ContentQueryNotice::Disabled),
        ContentIndexState::Backfilling { .. } => Some(ContentQueryNotice::Backfilling),
        ContentIndexState::Error(_) => Some(ContentQueryNotice::Error),
    }
}

impl CairnApp {
    pub(super) fn render_empty_state(
        &self,
        tokens: &ShellTokens,
        cx: &mut Context<'_, Self>,
    ) -> ControlElement {
        match self.recycle_bin.surface_location() {
            SurfaceLocation::LocalListing(LocalListingStage::Error) => {
                let control = accessibility::control("recycle-bin-error");
                let message = self.i18n.get_text("recycle_bin_unavailable_title");
                let retry = accessibility::control("recycle-bin-retry");
                let retry_label = accessibility::name(retry, &self.i18n);
                let action = icon_action(retry, AssetIconName::RotateCcw, retry_label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_recycle_bin(window, cx);
                    }))
                    .into_any_element();
                return render_state_band(
                    tokens,
                    control,
                    message,
                    Icon::new(IconName::CircleAlert)
                        .size_8()
                        .text_color(tokens.danger)
                        .into_any_element(),
                    Some(action),
                );
            }
            SurfaceLocation::LocalListing(_) => {
                let control = accessibility::control("result-state");
                let bin_empty = self.recycle_bin.all().is_empty();
                let (message_key, icon) = if bin_empty {
                    (
                        "recycle_bin_empty_title",
                        Icon::new(AssetIconName::Recycle)
                            .size_8()
                            .text_color(tokens.muted_foreground)
                            .into_any_element(),
                    )
                } else {
                    (
                        "recycle_bin_no_matches_title",
                        Icon::new(IconName::Search)
                            .size_8()
                            .text_color(tokens.muted_foreground)
                            .into_any_element(),
                    )
                };
                let message = self.i18n.get_text(message_key);
                return render_state_band(tokens, control, message, icon, None);
            }
            SurfaceLocation::Index => {}
        }

        let control = accessibility::control("result-state");

        if let Some(rejection) = self.query.rejection {
            let mut args = FluentArgs::new();
            args.set("limit", cairn_core::syntax::MAX_CONTENT_TERMS as i64);
            let message = self.i18n.get_text_with_args(rejection.message_key(), &args);
            return render_state_band(
                tokens,
                control,
                message,
                Icon::new(IconName::CircleAlert)
                    .size_8()
                    .text_color(tokens.danger)
                    .into_any_element(),
                None,
            );
        }

        if self.query.is_cancelled() {
            let message = self.i18n.get_text("search_cancelled_title");
            return render_state_band(
                tokens,
                control,
                message,
                Icon::new(AssetIconName::Search)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
                None,
            );
        }

        let has_query = self.has_query();
        let notice = if self.has_content_query() {
            content_query_notice(&self.index_menu_state())
        } else {
            None
        };
        let (message_key, icon) = match notice {
            Some(ContentQueryNotice::Loading) => (
                "empty_content_loading_title",
                Icon::new(AssetIconName::Timer)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
            ),
            Some(ContentQueryNotice::Unreadable) => (
                "empty_content_unreadable_title",
                Icon::new(AssetIconName::TriangleAlert)
                    .size_8()
                    .text_color(tokens.warning_ink)
                    .into_any_element(),
            ),
            Some(ContentQueryNotice::Disabled) => (
                "empty_content_disabled_title",
                Icon::new(AssetIconName::Info)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
            ),
            Some(ContentQueryNotice::Backfilling) => (
                "empty_content_backfilling_title",
                Icon::new(AssetIconName::Timer)
                    .size_8()
                    .text_color(tokens.warning_ink)
                    .into_any_element(),
            ),
            Some(ContentQueryNotice::Error) => (
                "empty_content_error_title",
                Icon::new(AssetIconName::CircleAlert)
                    .size_8()
                    .text_color(tokens.danger)
                    .into_any_element(),
            ),
            None if has_query && self.has_content_query() => (
                "empty_content_scope_line",
                Icon::new(AssetIconName::Search)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
            ),
            None if has_query => (
                "empty_no_matches",
                Icon::new(AssetIconName::Search)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
            ),
            None => (
                "empty_index_empty_title",
                Icon::new(AssetIconName::Search)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
            ),
        };
        let message = self.i18n.get_text(message_key);
        let action = match notice {
            Some(
                ContentQueryNotice::Disabled
                | ContentQueryNotice::Error
                | ContentQueryNotice::Unreadable,
            ) => {
                let label_key = match notice {
                    Some(ContentQueryNotice::Disabled) => "index_enable_action",
                    Some(ContentQueryNotice::Error) => "menu_content_search_retry",
                    _ => "index_status_retry",
                };
                let pending = !matches!(notice, Some(ContentQueryNotice::Unreadable))
                    && self.service.content.action_pending;
                let control = accessibility::control("content-search-action");
                let label = self.i18n.get_text(label_key);
                let icon = match notice {
                    Some(ContentQueryNotice::Disabled) => AssetIconName::Play,
                    _ => AssetIconName::RotateCcw,
                };
                Some(
                    icon_action(control, icon, label)
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            match notice {
                                Some(ContentQueryNotice::Unreadable) => {
                                    this.refresh_content_status(cx);
                                }
                                _ => this.set_content_index_enabled(true, cx),
                            }
                            this.query.search_focus.focus(window, cx);
                        }))
                        .into_any_element(),
                )
            }
            Some(ContentQueryNotice::Loading | ContentQueryNotice::Backfilling) | None => None,
        };
        render_state_band(tokens, control, message, icon, action)
    }

    pub(super) fn render_service_offline_state(
        &self,
        tokens: &ShellTokens,
        cx: &mut Context<'_, Self>,
    ) -> ControlElement {
        let issue = self
            .service
            .connection
            .issue()
            .unwrap_or(ServiceIssue::NotRunning);
        let control = accessibility::control(issue.accessibility_key());
        let message = self.i18n.get_text(issue.message_key());
        let retry = accessibility::control("service-retry-button");
        let retry_label = accessibility::name(retry, &self.i18n);
        let start = accessibility::control("service-start-button");
        let start_label = accessibility::name(start, &self.i18n);
        let probe_label = self.i18n.get_text("service_probe_label");
        let probing = self.service.connection.is_probing();
        let start_face = if probing {
            probe_label.clone()
        } else {
            start_label
        };
        let retry_face = if probing { probe_label } else { retry_label };
        let action = match service_issue::band_action(issue, self.service.connection.is_installed())
        {
            ServiceBandAction::Start => Some(
                icon_action(start, AssetIconName::Play, start_face)
                    .loading(probing)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.start_service(cx);
                    }))
                    .into_any_element(),
            ),
            ServiceBandAction::Retry => Some(
                icon_action(retry, AssetIconName::RotateCcw, retry_face)
                    .loading(probing)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.retry_service_connection(cx);
                    }))
                    .into_any_element(),
            ),
            ServiceBandAction::None => None,
        };
        render_state_band(
            tokens,
            control,
            message,
            Icon::new(IconName::CircleAlert)
                .size_8()
                .text_color(tokens.danger)
                .into_any_element(),
            action,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentQueryNotice, content_query_notice};
    use crate::shell::menu_rules::SubsystemMenuAccess;
    use cairn_protocol::status::{ContentIndexState, IndexJournalStatus, PropertyIndexStatus};

    #[test]
    fn a_content_query_explains_an_index_that_cannot_answer() {
        let property = PropertyIndexStatus::default();
        let journal = IndexJournalStatus::default();
        let mut state = crate::shell::menu_rules::IndexMenuState {
            service_connected: true,
            service_control_pending: false,
            content: SubsystemMenuAccess::default(),
            content_action_failed: false,
            content_state: &ContentIndexState::Ready,
            property: SubsystemMenuAccess::default(),
            property_state: &property,
            journal: SubsystemMenuAccess::default(),
            journal_status: &journal,
        };

        state.service_connected = false;
        assert_eq!(content_query_notice(&state), None);

        state.service_connected = true;
        state.content.loading = true;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Loading)
        );

        state.content.loading = false;
        state.content.failed = true;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Unreadable)
        );

        state.content.failed = false;
        state.content_action_failed = true;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Error)
        );

        state.content_action_failed = false;
        state.content_state = &ContentIndexState::Disabled;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Disabled)
        );

        let backfilling = ContentIndexState::Backfilling {
            processed: 3,
            total: 10,
        };
        state.content_state = &backfilling;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Backfilling)
        );

        let failed = ContentIndexState::Error("worker failed".into());
        state.content_state = &failed;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Error)
        );

        state.content_state = &ContentIndexState::Ready;
        assert_eq!(content_query_notice(&state), None);
    }
}

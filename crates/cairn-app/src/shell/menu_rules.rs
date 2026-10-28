// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What the index rows of the Tools menu may do, stated without the window.
//!
//! The menu answers three questions about a state it does not own: may this row be taken, does
//! this row have anything to say, and does it state a fact the reader can read. The answers live
//! here, over a plain state value, so they can be read and checked on their own instead of only
//! through a running window.

use crate::service::search_scope::{SearchScope, SEARCH_SCOPE_ALL_FILES};
use cairn_protocol::status::ContentIndexState;

/// What the menu knows about the service and the content index right now.
pub struct IndexMenuState<'a> {
    /// The scope in force.
    pub scope: SearchScope,
    /// Whether the background service is registered on this machine.
    pub service_installed: bool,
    /// Whether the window is talking to the service.
    pub service_connected: bool,
    /// Whether a scope switch is running.
    pub switching_scope: bool,
    /// Whether the content index status is still being read.
    pub content_loading: bool,
    /// Whether the last content index status read failed.
    pub content_failed: bool,
    /// Whether a content index action is in flight.
    pub content_action_pending: bool,
    /// Whether the window is served by its own user-privilege indexer instead of the installed
    /// service. The user indexer carries file names only, so a window in this mode has no
    /// content index behind it at all — whatever scope it was showing.
    pub local_mode: bool,
    /// The content index's own state.
    pub content_state: &'a ContentIndexState,
}

impl IndexMenuState<'_> {
    /// Whether a scope choice may be taken. A scope that is already in force is not a choice the
    /// user can make again, a switch that is running must not be started a second time, and
    /// "所有文件" needs the service that is not installed.
    pub fn scope_choice_is_disabled(&self, scope: &str) -> bool {
        self.switching_scope
            || self.scope.as_str() == scope
            || (scope == SEARCH_SCOPE_ALL_FILES && !self.service_installed)
    }

    /// Whether a scope choice is out of reach because the machine cannot provide it. The menu has
    /// no tooltip channel, so the row carries the reason in its label instead.
    pub fn scope_choice_is_unavailable(&self, scope: &str) -> bool {
        scope == SEARCH_SCOPE_ALL_FILES && !self.service_installed
    }

    /// Whether the content rows have anything to say. They only mean something where a content
    /// index exists: the user-privilege indexer carries file names only, so in that mode the rows
    /// and the popup that holds them are absent rather than promises the window cannot keep
    /// (UI-10). This holds for the readable-files scope and for the local fallback alike.
    pub fn content_rows_are_hidden(&self) -> bool {
        self.local_mode
    }

    /// Whether the content switch may be taken. A scope switch moves the window between two
    /// indexers, and the content switch belongs to the one the window is leaving, so it waits
    /// until the switch has settled. A window served by the user indexer has no content index to
    /// turn on at all.
    pub fn content_switch_is_disabled(&self) -> bool {
        self.local_mode
            || !self.service_connected
            || self.content_failed
            || self.content_action_pending
            || self.content_loading
            || self.switching_scope
    }

    /// Whether the rows that state a fact about the index may be read.
    pub fn content_facts_are_disabled(&self) -> bool {
        self.local_mode
            || !self.service_connected
            || self.content_action_pending
            || self.switching_scope
    }

    /// Whether the content index is on. A backfill in progress counts as on: the switch that
    /// turns it off has to be offered while the index is still filling.
    ///
    /// The row's label, the action it runs and the checkmark beside it all read the same fact,
    /// so the label can never promise one action while the click performs another.
    pub fn content_index_is_on(&self) -> bool {
        matches!(
            self.content_state,
            ContentIndexState::Backfilling { .. } | ContentIndexState::Ready
        )
    }

    /// Whether the content switch carries a checkmark. A backfill in progress has its own
    /// progress line, so the mark appears only once the index is ready: it never claims a
    /// finished index (UI-10).
    pub fn content_switch_is_checked(&self) -> bool {
        matches!(self.content_state, ContentIndexState::Ready)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::IndexMenuState;
    use crate::service::search_scope::{
        SearchScope, SEARCH_SCOPE_ALL_FILES, SEARCH_SCOPE_READABLE_FILES,
    };
    use cairn_protocol::status::ContentIndexState;

    /// A window talking to a running service, with a ready content index: the state every rule
    /// is measured against. The readable-files scope runs on the user indexer, so the helper
    /// reports that mode the way the window would.
    fn ready(scope: SearchScope) -> IndexMenuState<'static> {
        IndexMenuState {
            scope,
            service_installed: true,
            service_connected: true,
            switching_scope: false,
            content_loading: false,
            content_failed: false,
            content_action_pending: false,
            local_mode: scope.needs_local_only(),
            content_state: &ContentIndexState::Ready,
        }
    }

    /// "所有文件" needs the service. Where the service is missing the row is out of reach and
    /// says why, while "可读文件" stays reachable — it is only disabled here because it is
    /// already the scope in force, which is a different reason.
    #[test]
    fn the_all_files_scope_is_out_of_reach_without_the_service() {
        let mut state = ready(SearchScope::ReadableFiles);
        state.service_installed = false;

        assert!(state.scope_choice_is_disabled(SEARCH_SCOPE_ALL_FILES));
        assert!(state.scope_choice_is_unavailable(SEARCH_SCOPE_ALL_FILES));

        assert!(state.scope_choice_is_disabled(SEARCH_SCOPE_READABLE_FILES));
        assert!(
            !state.scope_choice_is_unavailable(SEARCH_SCOPE_READABLE_FILES),
            "the readable scope is not unavailable; it is simply the scope in force"
        );
    }

    /// With the service in place both scopes are reachable, except the one already in force.
    #[test]
    fn a_reachable_scope_that_is_already_in_force_is_not_a_choice() {
        let state = ready(SearchScope::ReadableFiles);
        assert!(state.scope_choice_is_disabled(SEARCH_SCOPE_READABLE_FILES));
        assert!(!state.scope_choice_is_disabled(SEARCH_SCOPE_ALL_FILES));
        assert!(!state.scope_choice_is_unavailable(SEARCH_SCOPE_ALL_FILES));
    }

    /// A scope switch is one action: while it runs, no second scope may be started.
    #[test]
    fn a_running_scope_switch_blocks_every_scope_choice() {
        let mut state = ready(SearchScope::ReadableFiles);
        state.switching_scope = true;
        assert!(state.scope_choice_is_disabled(SEARCH_SCOPE_ALL_FILES));
        assert!(state.scope_choice_is_disabled(SEARCH_SCOPE_READABLE_FILES));
    }

    /// The user-privilege indexer carries file names only, so wherever the window is served by it
    /// the content rows and the popup that holds them are absent, not greyed out. The
    /// readable-files scope is the ordinary way in, but the local fallback after a failed service
    /// start reaches the same rule.
    #[test]
    fn the_content_rows_are_absent_wherever_the_user_indexer_answers() {
        assert!(
            ready(SearchScope::ReadableFiles).content_rows_are_hidden(),
            "the readable-files scope runs on the user indexer"
        );
        assert!(!ready(SearchScope::AllFiles).content_rows_are_hidden());

        let mut fallback = ready(SearchScope::AllFiles);
        fallback.local_mode = true;
        assert!(
            fallback.content_rows_are_hidden(),
            "the local fallback has no content index either"
        );
        assert!(
            fallback.content_switch_is_disabled(),
            "a window without a content index cannot offer to turn one on"
        );
    }

    /// A scope switch moves the window to another indexer, so the content switch waits for it.
    #[test]
    fn the_content_switch_waits_for_a_running_scope_switch() {
        let mut state = ready(SearchScope::AllFiles);
        state.switching_scope = true;
        assert!(state.content_switch_is_disabled());
        assert!(state.content_facts_are_disabled());
    }

    /// A backfill in progress shows its own progress line. The checkmark appears only once the
    /// index is ready, so the mark never claims a finished index.
    #[test]
    fn the_content_switch_is_checked_only_when_the_index_is_ready() {
        let mut state = ready(SearchScope::AllFiles);
        assert!(state.content_switch_is_checked());

        state.content_state = &ContentIndexState::Backfilling {
            processed: 10,
            total: 100,
        };
        assert!(!state.content_switch_is_checked());

        state.content_state = &ContentIndexState::Disabled;
        assert!(!state.content_switch_is_checked());
    }

    /// A backfill in progress counts as on: the switch that turns the content index off has to
    /// be offered while the index is still filling.
    #[test]
    fn a_backfill_in_progress_still_counts_as_the_index_being_on() {
        let mut state = ready(SearchScope::AllFiles);
        assert!(state.content_index_is_on());

        state.content_state = &ContentIndexState::Backfilling {
            processed: 10,
            total: 100,
        };
        assert!(
            state.content_index_is_on(),
            "the index is filling, so the switch must offer to turn it off"
        );

        state.content_state = &ContentIndexState::Disabled;
        assert!(!state.content_index_is_on());

        let failed = ContentIndexState::Error("boom".to_string());
        state.content_state = &failed;
        assert!(!state.content_index_is_on());
    }

    /// A switch that cannot reach the service, or an action already in flight, holds the rows
    /// that would act on it.
    #[test]
    fn an_unreachable_service_or_a_pending_action_holds_the_content_rows() {
        let mut state = ready(SearchScope::AllFiles);
        state.service_connected = false;
        assert!(state.content_switch_is_disabled());
        assert!(state.content_facts_are_disabled());

        let mut state = ready(SearchScope::AllFiles);
        state.content_action_pending = true;
        assert!(state.content_switch_is_disabled());
        assert!(state.content_facts_are_disabled());
    }
}

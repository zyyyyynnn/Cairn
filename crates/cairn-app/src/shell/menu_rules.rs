// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What the index rows of the Tools menu may do, stated without the window.
//!
//! The menu answers three questions about a state it does not own: may this row be taken, does
//! this row have anything to say, and does it state a fact the reader can read. The answers live
//! here, over a plain state value, so they can be read and checked on their own instead of only
//! through a running window.

use cairn_protocol::status::ContentIndexState;

/// What the menu knows about the service and the content index right now.
pub struct IndexMenuState<'a> {
    /// Whether the window is talking to the service.
    pub service_connected: bool,
    /// Whether the content index status is still being read.
    pub content_loading: bool,
    /// Whether the last content index status read failed.
    pub content_failed: bool,
    /// Whether the last content index action itself failed.
    pub content_action_failed: bool,
    /// Whether a content index action is in flight.
    pub content_action_pending: bool,
    /// The content index's own state.
    pub content_state: &'a ContentIndexState,
}

impl IndexMenuState<'_> {
    /// Whether the content switch may be taken.
    pub fn content_switch_is_disabled(&self) -> bool {
        !self.service_connected
            || self.content_failed
            || self.content_action_pending
            || self.content_loading
    }

    /// Whether the rows that state a fact about the index may be read.
    pub fn content_facts_are_disabled(&self) -> bool {
        !self.service_connected || self.content_action_pending
    }

    /// Whether the content index is on. A backfill in progress counts as on: the checkmark has
    /// to say the feature is enabled while the progress row says the index is not ready yet.
    ///
    /// The action the row runs and the checkmark beside it read the same fact, so the mark can
    /// never promise one action while the click performs another.
    pub fn content_index_is_on(&self) -> bool {
        matches!(
            self.content_state,
            ContentIndexState::Backfilling { .. } | ContentIndexState::Ready
        )
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::IndexMenuState;
    use cairn_protocol::status::ContentIndexState;

    /// A window talking to a running service, with a ready content index: the state every rule
    /// is measured against.
    fn ready() -> IndexMenuState<'static> {
        IndexMenuState {
            service_connected: true,
            content_loading: false,
            content_failed: false,
            content_action_failed: false,
            content_action_pending: false,
            content_state: &ContentIndexState::Ready,
        }
    }

    /// The checkmark tracks the enabled state, including while the index fills; readiness and
    /// progress belong to the details row, not to the switch mark.
    #[test]
    fn the_content_switch_is_checked_whenever_content_search_is_on() {
        let mut state = ready();
        assert!(state.content_index_is_on());

        state.content_state = &ContentIndexState::Backfilling {
            processed: 10,
            total: 100,
        };
        assert!(
            state.content_index_is_on(),
            "the feature is on while the index is still filling"
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
        let mut state = ready();
        state.service_connected = false;
        assert!(state.content_switch_is_disabled());
        assert!(state.content_facts_are_disabled());

        let mut state = ready();
        state.content_action_pending = true;
        assert!(state.content_switch_is_disabled());
        assert!(state.content_facts_are_disabled());
    }
}

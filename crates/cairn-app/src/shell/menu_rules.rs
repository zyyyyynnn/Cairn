// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What the index rows of the Tools menu may do, stated without the window.
//!
//! The menu answers three questions about a state it does not own: may this row be taken, does
//! this row have anything to say, and does it state a fact the reader can read. The answers live
//! here, over a plain state value, so they can be read and checked on their own instead of only
//! through a running window.

use cairn_protocol::status::{ContentIndexState, IndexJournalStatus, PropertyIndexStatus};

/// What the menu knows about the service and index subsystems right now.
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
    pub property_loading: bool,
    pub property_failed: bool,
    pub property_action_pending: bool,
    pub property_state: &'a PropertyIndexStatus,
    pub journal_loading: bool,
    pub journal_failed: bool,
    pub journal_action_pending: bool,
    pub journal_status: &'a IndexJournalStatus,
    pub service_control_pending: bool,
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

    pub fn property_switch_is_disabled(&self) -> bool {
        !self.service_connected
            || self.property_failed
            || self.property_action_pending
            || self.property_loading
    }

    pub fn property_facts_are_disabled(&self) -> bool {
        !self.service_connected || self.property_action_pending
    }

    pub fn property_index_is_on(&self) -> bool {
        self.property_state.enabled
            || matches!(
                self.property_state.state,
                ContentIndexState::Backfilling { .. } | ContentIndexState::Ready
            )
    }

    pub fn journal_switch_is_disabled(&self) -> bool {
        !self.service_connected
            || self.journal_failed
            || self.journal_action_pending
            || self.journal_loading
    }

    pub fn journal_is_on(&self) -> bool {
        self.journal_status.enabled
    }

    pub fn service_action_is_disabled(&self) -> bool {
        !self.service_connected || self.service_control_pending
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::IndexMenuState;
    use cairn_protocol::status::{ContentIndexState, IndexJournalStatus, PropertyIndexStatus};

    /// A window talking to a running service, with a ready content index: the state every rule
    /// is measured against.
    fn ready() -> IndexMenuState<'static> {
        static PROPERTY_READY: std::sync::LazyLock<PropertyIndexStatus> =
            std::sync::LazyLock::new(|| PropertyIndexStatus {
                state: ContentIndexState::Ready,
                enabled: true,
                indexed_records: 120,
                side_table_bytes: 4096,
                last_updated_ts: 1,
            });
        static JOURNAL_ON: std::sync::LazyLock<IndexJournalStatus> =
            std::sync::LazyLock::new(|| IndexJournalStatus {
                enabled: true,
                entry_count: 15,
                resident_bytes: 2048,
                max_entries: 1000,
                max_bytes: 1_048_576,
                oldest_seq: 1,
                newest_seq: 15,
            });
        IndexMenuState {
            service_connected: true,
            content_loading: false,
            content_failed: false,
            content_action_failed: false,
            content_action_pending: false,
            content_state: &ContentIndexState::Ready,
            property_loading: false,
            property_failed: false,
            property_action_pending: false,
            property_state: &PROPERTY_READY,
            journal_loading: false,
            journal_failed: false,
            journal_action_pending: false,
            journal_status: &JOURNAL_ON,
            service_control_pending: false,
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

    #[test]
    fn property_journal_and_service_controls_follow_connection_and_pending_state() {
        let state = ready();
        assert!(state.property_index_is_on());
        assert!(!state.property_switch_is_disabled());
        assert!(!state.property_facts_are_disabled());
        assert!(state.journal_is_on());
        assert!(!state.journal_switch_is_disabled());
        assert!(!state.service_action_is_disabled());

        let mut disconnected = ready();
        disconnected.service_connected = false;
        assert!(disconnected.property_switch_is_disabled());
        assert!(disconnected.property_facts_are_disabled());
        assert!(disconnected.journal_switch_is_disabled());
        assert!(disconnected.service_action_is_disabled());

        let mut pending = ready();
        pending.property_action_pending = true;
        pending.journal_action_pending = true;
        pending.service_control_pending = true;
        assert!(pending.property_switch_is_disabled());
        assert!(pending.property_facts_are_disabled());
        assert!(pending.journal_switch_is_disabled());
        assert!(pending.service_action_is_disabled());

        let off_journal = IndexJournalStatus {
            enabled: false,
            ..Default::default()
        };
        let mut journal_state = ready();
        journal_state.journal_status = &off_journal;
        assert!(!journal_state.journal_is_on());
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What the index rows of the Tools menu may do, stated without the window.

use crate::service::SubsystemState;
use cairn_protocol::status::{ContentIndexState, IndexJournalStatus, PropertyIndexStatus};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubsystemMenuAccess {
    pub loading: bool,
    pub failed: bool,
    pub action_pending: bool,
}

impl SubsystemMenuAccess {
    pub fn from_subsystem<S>(state: &SubsystemState<S>) -> Self {
        Self {
            loading: state.is_loading(),
            failed: state.has_failure(),
            action_pending: state.action_pending,
        }
    }

    pub fn switch_disabled(self, service_connected: bool) -> bool {
        !service_connected || self.failed || self.action_pending || self.loading
    }

    pub fn facts_disabled(self, service_connected: bool) -> bool {
        !service_connected || self.action_pending
    }
}

/// What the menu knows about the service and index subsystems right now.
pub struct IndexMenuState<'a> {
    pub service_connected: bool,
    pub service_control_pending: bool,
    pub content: SubsystemMenuAccess,
    pub content_action_failed: bool,
    pub content_state: &'a ContentIndexState,
    pub property: SubsystemMenuAccess,
    pub property_state: &'a PropertyIndexStatus,
    pub journal: SubsystemMenuAccess,
    pub journal_status: &'a IndexJournalStatus,
}

impl IndexMenuState<'_> {
    pub fn content_switch_is_disabled(&self) -> bool {
        self.content.switch_disabled(self.service_connected)
    }

    pub fn content_facts_are_disabled(&self) -> bool {
        self.content.facts_disabled(self.service_connected)
    }

    /// Whether the content index is on (including while filling).
    pub fn content_index_is_on(&self) -> bool {
        matches!(
            self.content_state,
            ContentIndexState::Backfilling { .. } | ContentIndexState::Ready
        )
    }

    pub fn property_switch_is_disabled(&self) -> bool {
        self.property.switch_disabled(self.service_connected)
    }

    pub fn property_facts_are_disabled(&self) -> bool {
        self.property.facts_disabled(self.service_connected)
    }

    pub fn property_index_is_on(&self) -> bool {
        self.property_state.enabled
            || matches!(
                self.property_state.state,
                ContentIndexState::Backfilling { .. } | ContentIndexState::Ready
            )
    }

    pub fn journal_switch_is_disabled(&self) -> bool {
        self.journal.switch_disabled(self.service_connected)
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
    use super::{IndexMenuState, SubsystemMenuAccess};
    use cairn_protocol::status::{ContentIndexState, IndexJournalStatus, PropertyIndexStatus};

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
            service_control_pending: false,
            content: SubsystemMenuAccess::default(),
            content_action_failed: false,
            content_state: &ContentIndexState::Ready,
            property: SubsystemMenuAccess::default(),
            property_state: &PROPERTY_READY,
            journal: SubsystemMenuAccess::default(),
            journal_status: &JOURNAL_ON,
        }
    }

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

    #[test]
    fn an_unreachable_service_or_a_pending_action_holds_the_content_rows() {
        let mut state = ready();
        state.service_connected = false;
        assert!(state.content_switch_is_disabled());
        assert!(state.content_facts_are_disabled());

        let mut state = ready();
        state.content.action_pending = true;
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
        pending.property.action_pending = true;
        pending.journal.action_pending = true;
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

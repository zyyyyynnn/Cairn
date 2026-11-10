// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Volume status transitions. The journal ledger's catch-up is the only path that can produce
//! `Synced`; every newly chosen state goes through [`VolumeStatus::set`], which rejects it, so no
//! scan, poll or recovery path can hand-pick freshness without a journal position. [`VolumeStatus::restore`]
//! only rolls back a state the volume already held.

use super::journal_reconciliation::catch_up_state;
use cairn_protocol::status::VolumeState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VolumeStatus(VolumeState);

impl VolumeStatus {
    pub(super) fn rebuilding() -> Self {
        Self(VolumeState::Rebuilding)
    }

    pub(super) fn state(self) -> VolumeState {
        self.0
    }

    /// Applies a non-synced state. `Synced` is produced only by [`Self::reconcile`] and
    /// [`Self::reconcile_poll`].
    pub(super) fn set(&mut self, state: VolumeState) {
        assert!(
            !matches!(state, VolumeState::Synced),
            "Synced is produced only by journal reconciliation"
        );
        self.0 = state;
    }

    /// Restores a state captured earlier, such as the pre-scan state of a volume whose rescan
    /// failed. The caller rolls back to a value the volume already held; this is not a new claim
    /// of freshness.
    pub(super) fn restore(&mut self, state: VolumeState) {
        self.0 = state;
    }

    /// Applies the journal ledger's catch-up result between `start_usn` and the journal tail.
    pub(super) fn reconcile(&mut self, start_usn: i64, applied_usn: i64, tail_usn: i64) {
        self.0 = catch_up_state(start_usn, applied_usn, tail_usn);
    }

    /// Applies a live poll's catch-up result. Without a tail sample the poll has no new proof of
    /// freshness and the volume keeps its state.
    pub(super) fn reconcile_poll(
        &mut self,
        start_usn: i64,
        applied_usn: i64,
        tail_usn: Option<i64>,
    ) {
        if let Some(tail_usn) = tail_usn {
            self.0 = catch_up_state(start_usn, applied_usn, tail_usn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_is_the_only_synced_producer() {
        let mut status = VolumeStatus::rebuilding();
        assert_eq!(status.state(), VolumeState::Rebuilding);

        status.set(VolumeState::CatchingUp(40));
        assert_eq!(status.state(), VolumeState::CatchingUp(40));

        status.reconcile(100, 150, 200);
        assert_eq!(status.state(), VolumeState::CatchingUp(50));

        status.reconcile(100, 200, 200);
        assert_eq!(status.state(), VolumeState::Synced);
    }

    fn poll_state(start_usn: i64, applied_usn: i64, tail_usn: Option<i64>) -> VolumeState {
        let mut status = VolumeStatus(VolumeState::CatchingUp(0));
        status.reconcile_poll(start_usn, applied_usn, tail_usn);
        status.state()
    }

    #[test]
    fn live_poll_reports_catch_up_while_the_tail_is_ahead() {
        assert_eq!(
            poll_state(1000, 2000, Some(2001)),
            VolumeState::CatchingUp(99),
            "one tick of backlog is still a backlog, never Synced"
        );
    }

    #[test]
    fn live_poll_at_the_journal_tail_is_synced() {
        assert_eq!(poll_state(1000, 2000, Some(2000)), VolumeState::Synced);
    }

    #[test]
    fn an_empty_batch_at_the_real_tail_is_synced() {
        assert_eq!(
            poll_state(2000, 2000, Some(2000)),
            VolumeState::Synced,
            "a caught-up volume whose journal read returned no records must leave CatchingUp"
        );
    }

    #[test]
    fn an_empty_batch_with_a_real_backlog_stays_catching_up() {
        assert_eq!(
            poll_state(2000, 2000, Some(3000)),
            VolumeState::CatchingUp(0),
            "an empty read is not proof of freshness when the real tail is ahead"
        );
    }

    #[test]
    fn live_poll_tolerates_positions_moving_backwards() {
        assert_eq!(
            poll_state(2000, 1000, Some(1500)),
            VolumeState::CatchingUp(0)
        );
    }

    #[test]
    fn a_poll_without_a_tail_sample_keeps_the_state() {
        let mut status = VolumeStatus(VolumeState::CatchingUp(10));
        status.reconcile_poll(100, 130, None);
        assert_eq!(status.state(), VolumeState::CatchingUp(10));
        status.reconcile_poll(100, 200, Some(200));
        assert_eq!(status.state(), VolumeState::Synced);
    }

    #[test]
    #[should_panic(expected = "Synced is produced only by journal reconciliation")]
    fn a_direct_synced_state_is_rejected() {
        let mut status = VolumeStatus::rebuilding();
        status.set(VolumeState::Synced);
    }
}

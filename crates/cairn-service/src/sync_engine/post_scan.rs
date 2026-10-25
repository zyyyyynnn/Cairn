// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Post-scan decision machine: determines the terminal state of a volume that was rebuilt
//! from a full MFT scan (IDX-05, IDX-07).

use super::journal_reconciliation::catch_up_state;
use super::VolumeIndex;
use cairn_protocol::status::VolumeState;

/// Outcome of reconciling a full `$MFT` snapshot with the change journal around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostScanDecision {
    /// The snapshot cannot be placed on a trustworthy journal timeline, so it is dropped and the
    /// volume is rebuilt rather than being served or persisted (IDX-05).
    Rebuild,
    /// The snapshot may be served and its freshness is exactly the carried state (IDX-07).
    Fresh(VolumeState),
}

/// Decides the terminal state of a volume that was rebuilt from a full MFT scan.
///
/// The scan is not atomic, so it is bracketed by two journal reads: `scan_start_usn` is the tail at
/// T0 (before the scan) and `tail_usn` the tail at T1 (after it). Every record in `[T0, T1)` may be
/// missing from the snapshot and therefore has to be replayed; `replayed_to` is how far that replay
/// actually got.
pub fn post_scan_decision(
    journal_replaced: bool,
    scan_start_usn: i64,
    replayed_to: Option<i64>,
    tail_usn: Option<i64>,
) -> PostScanDecision {
    if journal_replaced {
        return PostScanDecision::Rebuild;
    }
    match (replayed_to, tail_usn) {
        (Some(applied_usn), Some(tail_usn)) => {
            PostScanDecision::Fresh(catch_up_state(scan_start_usn, applied_usn, tail_usn))
        }
        (None, _) => PostScanDecision::Rebuild,
        (Some(_), None) => PostScanDecision::Fresh(VolumeState::CatchingUp(0)),
    }
}

/// Writes a [`PostScanDecision`] back into the volume, returning `true` when the rebuilt index may be
/// kept. This is the only place a full-scan path assigns a terminal state, so a scan can never
/// hand-pick `Synced` for itself (IDX-07).
pub fn apply_post_scan_decision(vol: &mut VolumeIndex, decision: PostScanDecision) -> bool {
    match decision {
        PostScanDecision::Rebuild => {
            vol.clear();
            vol.usn_enabled = false;
            vol.status = VolumeState::Rebuilding;
            false
        }
        PostScanDecision::Fresh(state) => {
            vol.status = state;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_ntfs::mft::ExtractedItem;

    #[test]
    fn test_post_scan_decision_only_syncs_at_the_journal_tail() {
        assert_eq!(
            post_scan_decision(false, 1000, Some(2000), Some(2000)),
            PostScanDecision::Fresh(VolumeState::Synced)
        );
        assert_eq!(
            post_scan_decision(false, 1000, Some(2500), Some(2000)),
            PostScanDecision::Fresh(VolumeState::Synced)
        );
        assert_eq!(
            post_scan_decision(false, 1000, Some(1500), Some(2000)),
            PostScanDecision::Fresh(VolumeState::CatchingUp(50))
        );
        assert_eq!(
            post_scan_decision(false, 1000, Some(1999), Some(2000)),
            PostScanDecision::Fresh(VolumeState::CatchingUp(99))
        );
        assert_eq!(
            post_scan_decision(false, 1000, Some(1000), Some(2000)),
            PostScanDecision::Fresh(VolumeState::CatchingUp(0))
        );
    }

    #[test]
    fn test_post_scan_decision_refuses_to_claim_freshness_it_cannot_prove() {
        assert_eq!(
            post_scan_decision(true, 1000, Some(2000), Some(2000)),
            PostScanDecision::Rebuild,
            "recreated journal invalidates the scan window"
        );
        assert_eq!(
            post_scan_decision(false, 1000, None, Some(2000)),
            PostScanDecision::Rebuild,
            "failed replay leaves an unknown hole"
        );
        assert_eq!(
            post_scan_decision(true, 1000, None, Some(2000)),
            PostScanDecision::Rebuild
        );
        assert_eq!(
            post_scan_decision(false, 1000, Some(1500), None),
            PostScanDecision::Fresh(VolumeState::CatchingUp(0)),
            "unknown post-scan tail leaves catch-up to the polling loop"
        );
    }

    #[test]
    fn test_apply_post_scan_decision_keeps_an_incomplete_scan_unsynced() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "scanned_file.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 10,
            modified_ts: 10,
            flags: 0,
            is_reparse_point: false,
        }]);

        // Exactly what a full scan whose replay stopped short of T1 produces: the journal position
        // is behind the tail, and the state is whatever the reconciliation decided.
        vol.journal_id = 7;
        vol.usn_enabled = true;
        vol.last_usn = 1500;
        let decision = post_scan_decision(false, 1000, Some(vol.last_usn), Some(2000));
        assert!(apply_post_scan_decision(&mut vol, decision));

        assert_ne!(
            vol.status,
            VolumeState::Synced,
            "a snapshot with an open backlog is the exact IDX-07 violation this state machine forbids"
        );
        assert!(matches!(vol.status, VolumeState::CatchingUp(_)));
        assert_eq!(vol.records.len(), 1, "the partial index is still served");
        assert_eq!(
            vol.last_usn, 1500,
            "the saved position must stay the real replay offset"
        );
    }
}

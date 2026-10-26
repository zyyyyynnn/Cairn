// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Journal reconciliation: aligns live NTFS change journal with MFT scan snapshots
//! to prove freshness before declaring an index Synced (SPEC IDX-05, IDX-07).

use super::post_scan::{apply_post_scan_decision, post_scan_decision};
use super::VolumeIndex;
use cairn_protocol::status::VolumeState;
use std::path::Path;

/// Upper bound on journal reads performed by one catch-up pass, so loading a cache cannot stall
/// startup beyond NFR-02 while a very busy volume keeps producing records.
#[cfg(windows)]
pub const USN_CATCH_UP_MAX_ROUNDS: usize = 64;

/// State of a volume after replaying the change journal up to `applied_usn`.
///
/// Only a volume that reached the journal tail counts as synchronized; everything else is
/// `CatchingUp` with a percentage derived from journal positions, so a partially replayed index is
/// never reported as fresh (SPEC IDX-07).
pub fn catch_up_state(start_usn: i64, applied_usn: i64, tail_usn: i64) -> VolumeState {
    if applied_usn >= tail_usn {
        return VolumeState::Synced;
    }
    let total = tail_usn.saturating_sub(start_usn).max(1);
    let done = applied_usn.saturating_sub(start_usn).max(0);
    VolumeState::CatchingUp(((done * 100 / total).min(99)) as u8)
}

/// State a live poll should report after applying a batch of changes that moved the volume from
/// `previous_usn` to `next_usn`, given the journal tail read after that batch.
///
/// `None` means the poll has no new proof of freshness, so the volume keeps the state it had.
pub fn live_state_after_poll(
    previous_usn: i64,
    next_usn: i64,
    tail_usn: Option<i64>,
) -> Option<VolumeState> {
    tail_usn.map(|tail| catch_up_state(previous_usn, next_usn, tail))
}

/// Whether a volume in `state` should update its live synchronization state when a USN poll
/// succeeds. Including [`VolumeState::Offline`] allows an ejected or temporarily disconnected
/// volume to recover automatically to a live state once it reconnects and its USN journal
/// responds again.
pub const fn is_live_poll_trackable(state: VolumeState) -> bool {
    matches!(
        state,
        VolumeState::Synced | VolumeState::CatchingUp(_) | VolumeState::Offline
    )
}

/// Whether a Win32 error code from opening a volume or reading its USN journal indicates that the
/// underlying device was disconnected, ejected, or is not ready (`VolumeState::Offline`).
pub const fn is_device_offline_win32_error(code: u32) -> bool {
    matches!(
        code,
        2    // ERROR_FILE_NOT_FOUND
        | 3    // ERROR_PATH_NOT_FOUND
        | 15   // ERROR_INVALID_DRIVE
        | 21   // ERROR_NOT_READY
        | 22   // ERROR_BAD_COMMAND
        | 31   // ERROR_GEN_FAILURE
        | 433  // ERROR_NO_SUCH_DEVICE
        | 1117 // ERROR_IO_DEVICE
        | 1167 // ERROR_DEVICE_NOT_CONNECTED
    )
}

#[cfg(windows)]
pub fn replay_usn_backlog(
    handle: &cairn_ntfs::RawVolumeHandle,
    vol: &mut VolumeIndex,
    buf: &mut [u8],
    journal_id: u64,
    tail_usn: i64,
) -> Result<(), cairn_ntfs::usn::UsnError> {
    let start_usn = vol.last_usn;
    vol.journal_id = journal_id;
    vol.usn_enabled = true;
    let mut resume = start_usn;

    for _ in 0..USN_CATCH_UP_MAX_ROUNDS {
        let (next_usn, changes) = handle.read_usn_journal(journal_id, resume, buf)?;
        for change in changes {
            vol.apply_usn_change(change);
        }
        vol.last_usn = next_usn;
        if next_usn >= tail_usn || next_usn <= resume {
            break;
        }
        resume = next_usn;
    }

    if vol.last_usn != start_usn {
        vol.path_cache.clear();
        vol.presorted_dirty = true;
    }
    vol.status = catch_up_state(start_usn, vol.last_usn, tail_usn);
    Ok(())
}

/// Opens the volume and reads the journal tail *before* a full `$MFT` scan starts, giving the T0 end
/// of the replay window.
#[cfg(windows)]
pub fn open_journal_at_scan_start(
    drive: char,
) -> Option<(cairn_ntfs::RawVolumeHandle, cairn_ntfs::usn::UsnJournalData)> {
    let handle = match cairn_ntfs::RawVolumeHandle::open(drive) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::info!(
                "Cannot open raw handle for {} ({:?}): nothing to reconcile a scan with (IDX-08)",
                drive,
                e
            );
            return None;
        }
    };
    match handle.query_usn_journal() {
        Ok(data) => Some((handle, data)),
        Err(e) => {
            tracing::info!(
                "USN journal unavailable for {} ({:?}): nothing to reconcile a scan with (IDX-08)",
                drive,
                e
            );
            None
        }
    }
}

#[cfg(windows)]
pub fn save_scanned_index(vol: &VolumeIndex, index_dir: &Path) {
    let _ = vol.save_to_dir(index_dir);
}

/// Reconciles a volume whose records were just ingested from a full scan with the journal window
/// around that scan, then persists it when the result may be served.
#[cfg(windows)]
pub fn finalize_scanned_volume(
    vol: &mut VolumeIndex,
    index_dir: &Path,
    scan_start: Option<(cairn_ntfs::RawVolumeHandle, cairn_ntfs::usn::UsnJournalData)>,
) -> Result<(), String> {
    let drive = vol.drive_letter;
    let Some((handle, t0)) = scan_start else {
        vol.usn_enabled = false;
        vol.status = VolumeState::SnapshotMode;
        save_scanned_index(vol, index_dir);
        return Ok(());
    };

    vol.journal_id = t0.journal_id;
    vol.last_usn = t0.next_usn;

    let t1 = handle.query_usn_journal().ok();
    let journal_replaced = t1.is_some_and(|data| data.journal_id != t0.journal_id);
    let mut replayed_to = Some(t0.next_usn);

    if journal_replaced {
        tracing::warn!(
            "USN journal for {} was recreated during the scan: the snapshot has no stable timeline",
            drive
        );
    } else if let Some(t1) = t1 {
        let mut buf = vec![0u8; 65536];
        if let Err(e) = replay_usn_backlog(&handle, vol, &mut buf, t0.journal_id, t1.next_usn) {
            tracing::warn!(
                "USN backlog replay for {} failed ({:?}): the snapshot is incomplete",
                drive,
                e
            );
            replayed_to = None;
        }
    } else {
        tracing::warn!(
            "USN journal tail for {} could not be read after the scan: catch-up left to polling",
            drive
        );
    }

    let decision = post_scan_decision(
        journal_replaced,
        t0.next_usn,
        replayed_to,
        t1.map(|data| data.next_usn),
    );
    if apply_post_scan_decision(vol, decision) {
        save_scanned_index(vol, index_dir);
        Ok(())
    } else {
        Err(format!(
            "volume {drive}: full scan could not be reconciled with its USN journal"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_poll_reports_catch_up_while_the_tail_is_ahead() {
        assert_eq!(
            live_state_after_poll(1000, 2000, Some(2001)),
            Some(VolumeState::CatchingUp(99)),
            "one byte of backlog is still a backlog, never Synced (IDX-07)"
        );
    }

    #[test]
    fn live_poll_keeps_the_previous_state_when_the_tail_is_unknown() {
        assert_eq!(
            live_state_after_poll(1000, 2000, None),
            None,
            "without a journal tail the poll cannot prove freshness"
        );
    }

    #[test]
    fn live_poll_at_the_journal_tail_is_synced() {
        assert_eq!(
            live_state_after_poll(1000, 2000, Some(2000)),
            Some(VolumeState::Synced)
        );
    }

    #[test]
    fn an_empty_batch_at_the_real_tail_is_synced() {
        assert_eq!(
            live_state_after_poll(2000, 2000, Some(2000)),
            Some(VolumeState::Synced),
            "a caught-up volume whose journal read returned no records must still leave CatchingUp"
        );
    }

    #[test]
    fn an_empty_batch_with_a_real_backlog_stays_catching_up() {
        assert_eq!(
            live_state_after_poll(2000, 2000, Some(3000)),
            Some(VolumeState::CatchingUp(0)),
            "an empty read is not proof of freshness when the real tail is ahead"
        );
    }

    #[test]
    fn live_poll_tolerates_positions_moving_backwards() {
        assert_eq!(
            live_state_after_poll(2000, 1000, Some(1500)),
            Some(VolumeState::CatchingUp(0))
        );
        assert_eq!(live_state_after_poll(2000, 1000, None), None);
    }

    #[test]
    fn offline_volume_is_trackable_for_live_recovery_and_device_removal_codes_are_recognized() {
        assert!(is_live_poll_trackable(VolumeState::Offline));
        assert!(is_live_poll_trackable(VolumeState::Synced));
        assert!(is_live_poll_trackable(VolumeState::CatchingUp(50)));
        assert!(!is_live_poll_trackable(VolumeState::SnapshotMode));
        assert!(!is_live_poll_trackable(VolumeState::Rebuilding));

        // ERROR_DEVICE_NOT_CONNECTED (1167) and ERROR_NOT_READY (21) mark the volume Offline.
        assert!(is_device_offline_win32_error(1167));
        assert!(is_device_offline_win32_error(21));
        assert!(!is_device_offline_win32_error(0));
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires a live NTFS volume with an enabled USN journal and an elevated process"]
    fn test_live_scan_reconciles_with_the_real_change_journal() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let scan_start = open_journal_at_scan_start('C').expect("C: must expose a USN journal");
        let t0 = scan_start.1.next_usn;

        let mut vol = VolumeIndex::new('C');
        let items = cairn_ntfs::scan_live_volume_mft('C', 4096).expect("live $MFT scan");
        vol.ingest_items(items);

        finalize_scanned_volume(&mut vol, temp_dir.path(), Some(scan_start)).expect("reconciled");

        assert!(vol.last_usn >= t0);
        assert!(!vol.records.is_empty());
        assert!(matches!(
            vol.status,
            VolumeState::Synced | VolumeState::CatchingUp(_)
        ));
        assert_eq!(
            crate::persistence::load_volume_index(&temp_dir.path().join("vol_C.idx"))
                .expect("saved index")
                .last_usn,
            vol.last_usn,
            "the persisted journal position must be the real replay offset"
        );
    }
}

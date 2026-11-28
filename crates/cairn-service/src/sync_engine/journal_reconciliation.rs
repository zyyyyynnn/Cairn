// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Journal reconciliation: aligns live NTFS change journal with MFT scan snapshots
//! to prove freshness before declaring an index Synced (SPEC IDX-05, IDX-07).

use super::VolumeIndex;
use super::post_scan::{apply_post_scan_decision, post_scan_decision};
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
pub(super) trait JournalPort {
    fn query_usn_journal(
        &self,
    ) -> Result<cairn_ntfs::usn::UsnJournalData, cairn_ntfs::usn::UsnError>;

    fn read_usn_journal(
        &self,
        journal_id: u64,
        start_usn: i64,
        out_buf: &mut [u8],
    ) -> Result<(i64, Vec<cairn_ntfs::usn::UsnChange>), cairn_ntfs::usn::UsnError>;
}

#[cfg(windows)]
impl JournalPort for cairn_ntfs::RawVolumeHandle {
    fn query_usn_journal(
        &self,
    ) -> Result<cairn_ntfs::usn::UsnJournalData, cairn_ntfs::usn::UsnError> {
        Self::query_usn_journal(self)
    }

    fn read_usn_journal(
        &self,
        journal_id: u64,
        start_usn: i64,
        out_buf: &mut [u8],
    ) -> Result<(i64, Vec<cairn_ntfs::usn::UsnChange>), cairn_ntfs::usn::UsnError> {
        Self::read_usn_journal(self, journal_id, start_usn, out_buf)
    }
}

#[cfg(windows)]
pub fn replay_usn_backlog(
    handle: &cairn_ntfs::RawVolumeHandle,
    vol: &mut VolumeIndex,
    buf: &mut [u8],
    journal_id: u64,
    tail_usn: i64,
) -> Result<(), cairn_ntfs::usn::UsnError> {
    replay_usn_backlog_with(handle, vol, buf, journal_id, tail_usn)
}

#[cfg(windows)]
fn replay_usn_backlog_with(
    handle: &dyn JournalPort,
    vol: &mut VolumeIndex,
    buf: &mut [u8],
    journal_id: u64,
    tail_usn: i64,
) -> Result<(), cairn_ntfs::usn::UsnError> {
    let start_usn = vol.last_usn;
    vol.journal_id = journal_id;
    vol.usn_enabled = true;
    vol.usn_parse_failed = false;
    let mut resume = start_usn;

    for _ in 0..USN_CATCH_UP_MAX_ROUNDS {
        let (next_usn, changes) = handle.read_usn_journal(journal_id, resume, buf)?;
        vol.apply_usn_changes(changes);
        vol.last_usn = next_usn;
        if next_usn >= tail_usn || next_usn <= resume {
            break;
        }
        resume = next_usn;
    }

    if vol.last_usn != start_usn {
        vol.clear_path_cache();
        vol.mark_presorted_dirty();
    }
    vol.status.reconcile(start_usn, vol.last_usn, tail_usn);
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
    if let Err(err) = vol.save_to_dir(index_dir) {
        tracing::warn!(
            "volume {}: scanned index could not be persisted to {} ({err}): the previous checkpoint stays on disk",
            vol.drive_letter,
            index_dir.display()
        );
    }
}

/// Reconciles a volume whose records were just ingested from a full scan with the journal window
/// around that scan, then persists it when the result may be served.
#[cfg(windows)]
pub fn finalize_scanned_volume(
    vol: &mut VolumeIndex,
    index_dir: &Path,
    scan_start: Option<(cairn_ntfs::RawVolumeHandle, cairn_ntfs::usn::UsnJournalData)>,
) -> Result<(), String> {
    let port = scan_start.map(|(handle, t0)| (Box::new(handle) as Box<dyn JournalPort>, t0));
    finalize_scanned_volume_with(vol, index_dir, port)
}

#[cfg(windows)]
pub(super) fn finalize_scanned_volume_with(
    vol: &mut VolumeIndex,
    index_dir: &Path,
    scan_start: Option<(Box<dyn JournalPort>, cairn_ntfs::usn::UsnJournalData)>,
) -> Result<(), String> {
    let drive = vol.drive_letter;
    let Some((handle, t0)) = scan_start else {
        vol.usn_enabled = false;
        vol.status.set(VolumeState::SnapshotMode);
        save_scanned_index(vol, index_dir);
        return Ok(());
    };

    vol.journal_id = t0.journal_id;
    vol.last_usn = t0.next_usn;
    vol.usn_parse_failed = false;

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
        if let Err(e) = replay_usn_backlog_with(&*handle, vol, &mut buf, t0.journal_id, t1.next_usn)
        {
            tracing::warn!(
                "USN backlog replay for {} failed ({:?}): the snapshot is incomplete",
                drive,
                e
            );
            if matches!(
                e,
                cairn_ntfs::usn::UsnError::UnsupportedRecordVersion(_)
                    | cairn_ntfs::usn::UsnError::MalformedRecord(_)
            ) {
                vol.status.set(VolumeState::Offline);
                vol.usn_parse_failed = true;
                save_scanned_index(vol, index_dir);
                return Ok(());
            }
            replayed_to = None;
        } else {
            replayed_to = Some(vol.last_usn);
        }
    } else {
        tracing::warn!(
            "USN journal tail for {} could not be read after the scan",
            drive
        );
        if !vol.usn_enabled {
            vol.status.set(VolumeState::SnapshotMode);
            save_scanned_index(vol, index_dir);
            return Ok(());
        }
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
    fn offline_volume_is_trackable_for_live_recovery_and_device_removal_codes_are_recognized() {
        assert!(is_live_poll_trackable(VolumeState::Offline));
        assert!(is_live_poll_trackable(VolumeState::Synced));
        assert!(is_live_poll_trackable(VolumeState::CatchingUp(50)));
        assert!(!is_live_poll_trackable(VolumeState::SnapshotMode));
        assert!(!is_live_poll_trackable(VolumeState::Rebuilding));

        assert!(is_device_offline_win32_error(1167));
        assert!(is_device_offline_win32_error(21));
        assert!(!is_device_offline_win32_error(0));
    }

    #[cfg(windows)]
    struct StubJournalPort {
        query_error: Option<u32>,
        query_ok: Option<cairn_ntfs::usn::UsnJournalData>,
        next_usn_step: i64,
    }

    #[cfg(windows)]
    impl JournalPort for StubJournalPort {
        fn query_usn_journal(
            &self,
        ) -> Result<cairn_ntfs::usn::UsnJournalData, cairn_ntfs::usn::UsnError> {
            if let Some(code) = self.query_error {
                return Err(cairn_ntfs::usn::UsnError::QueryFailed(code));
            }
            Ok(self.query_ok.expect("query_ok set"))
        }

        fn read_usn_journal(
            &self,
            _journal_id: u64,
            start_usn: i64,
            _out_buf: &mut [u8],
        ) -> Result<(i64, Vec<cairn_ntfs::usn::UsnChange>), cairn_ntfs::usn::UsnError> {
            Ok((start_usn + self.next_usn_step, Vec::new()))
        }
    }

    #[cfg(windows)]
    #[test]
    fn finalize_scanned_volume_uses_replayed_usn_and_handles_missing_t1_on_non_usn_volume() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = cairn_ntfs::usn::UsnJournalData {
            journal_id: 7,
            first_usn: 0,
            next_usn: 100,
            lowest_valid_usn: 0,
        };

        let mut non_usn_vol = VolumeIndex::new('C');
        non_usn_vol.usn_enabled = false;
        let port = Box::new(StubJournalPort {
            query_error: Some(1),
            query_ok: None,
            next_usn_step: 0,
        });
        finalize_scanned_volume_with(&mut non_usn_vol, dir.path(), Some((port, t0)))
            .expect("missing T1 on non-USN volume falls back to SnapshotMode");
        assert_eq!(non_usn_vol.status.state(), VolumeState::SnapshotMode);

        let mut usn_vol = VolumeIndex::new('E');
        usn_vol.usn_enabled = true;
        let port_usn = Box::new(StubJournalPort {
            query_error: Some(1),
            query_ok: None,
            next_usn_step: 0,
        });
        finalize_scanned_volume_with(&mut usn_vol, dir.path(), Some((port_usn, t0)))
            .expect("missing T1 on USN-enabled volume stays CatchingUp(0)");
        assert!(usn_vol.usn_enabled);
        assert_eq!(usn_vol.status.state(), VolumeState::CatchingUp(0));

        let mut replay_vol = VolumeIndex::new('D');
        let t1 = cairn_ntfs::usn::UsnJournalData {
            journal_id: 7,
            first_usn: 0,
            next_usn: 250,
            lowest_valid_usn: 0,
        };
        let replay_port = Box::new(StubJournalPort {
            query_error: None,
            query_ok: Some(t1),
            next_usn_step: 150,
        });
        finalize_scanned_volume_with(&mut replay_vol, dir.path(), Some((replay_port, t0)))
            .expect("full replay reconciles to Synced");
        assert_eq!(replay_vol.last_usn, 250);
        assert_eq!(replay_vol.status.state(), VolumeState::Synced);
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
        assert!(!vol.records().is_empty());
        assert!(matches!(
            vol.status.state(),
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

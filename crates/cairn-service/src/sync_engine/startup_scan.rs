// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Volume acquisition, rescans, journal polling and index persistence.

use super::{MultiVolumeEngine, VolumeIndex};
use cairn_ntfs::volume::enumerate_fixed_drives;
use cairn_protocol::status::VolumeState;
use std::collections::HashSet;

#[cfg(windows)]
use super::journal_reconciliation::{
    finalize_scanned_volume, is_device_offline_win32_error, is_live_poll_trackable,
    open_journal_at_scan_start,
};
use crate::persistence;

/// A journal backlog beyond this size is cheaper to recover with a fresh `$MFT` scan than by
/// replaying hundreds of thousands of USN records, and it bounds how long a cold start may spend
/// catching up.
const MAX_USN_REPLAY_BACKLOG_BYTES: i64 = 64 * 1024 * 1024;

impl MultiVolumeEngine {
    /// Rescans every volume running in SnapshotMode (IDX-08) and reports how many were rebuilt.
    pub fn rescan_snapshot_volumes(&self) -> usize {
        let snapshot_drives: Vec<char> = {
            let vols = self.volumes.read().unwrap();
            let cfg = self.config.read().unwrap();
            vols.values()
                .filter(|v| v.status.state() == VolumeState::SnapshotMode)
                .map(|v| v.drive_letter)
                .filter(|d| {
                    !cfg.excluded_drives
                        .iter()
                        .any(|ex| ex.eq_ignore_ascii_case(d))
                })
                .collect()
        };

        snapshot_drives
            .into_iter()
            .filter(|drive| self.rescan_volume(*drive).is_ok())
            .count()
    }

    /// Marks a volume as rebuilding before its long scan starts. The summary cache is updated
    /// under its own lock so a handshake during the scan cannot report the previous state.
    #[cfg(windows)]
    pub(super) fn mark_volume_rebuilding(&self, drive: char) -> Option<VolumeState> {
        let previous = {
            let mut volumes = self.volumes.write().unwrap();
            let volume = volumes.get_mut(&drive)?;
            let previous = volume.status.state();
            volume.status.set(VolumeState::Rebuilding);
            previous
        };
        self.set_cached_summary_status(drive, VolumeState::Rebuilding);
        Some(previous)
    }

    #[cfg(windows)]
    pub(super) fn restore_volume_status(&self, drive: char, status: VolumeState) {
        if let Some(volume) = self.volumes.write().unwrap().get_mut(&drive) {
            volume.status.restore(status);
        }
        self.set_cached_summary_status(drive, status);
    }

    #[cfg(windows)]
    fn set_cached_summary_status(&self, drive: char, status: VolumeState) {
        if let Some(summary) = self
            .summary_cache
            .lock()
            .unwrap()
            .iter_mut()
            .find(|summary| summary.drive_letter == drive)
        {
            summary.status = status;
        }
    }

    /// Rebuilds one volume from a full `$MFT` scan (IDX-08). Errors whenever the volume is not
    /// tracked, the scan yields nothing, or its result cannot be reconciled with the change journal,
    /// so a rescan is never reported as a silent success.
    pub fn rescan_volume(&self, drive: char) -> Result<usize, String> {
        #[cfg(windows)]
        {
            if !self.volumes.read().unwrap().contains_key(&drive) {
                return Err(format!("volume {drive}: is not indexed"));
            }

            let previous_status = self
                .mark_volume_rebuilding(drive)
                .ok_or_else(|| format!("volume {drive}: is not indexed"))?;
            // T0 must be read before the scan and the scan must stay outside the volume lock, so the
            // journal is bracketed here and only reconciled once the records are in hand (IDX-07).
            let scan_start = open_journal_at_scan_start(drive);
            match cairn_ntfs::scan_live_volume_mft(drive, 4096) {
                Ok(items) if !items.is_empty() => {
                    let count = items.len();
                    let mut lock = self.volumes.write().unwrap();
                    let vol = match lock.get_mut(&drive) {
                        Some(vol) => vol,
                        None => return Err(format!("volume {drive}: is not indexed")),
                    };
                    vol.ingest_items(items);
                    finalize_scanned_volume(&mut *vol, &self.index_dir, scan_start)?;
                    tracing::info!(
                        "Volume {drive}: rescanned {} records, state {:?}",
                        vol.records.len(),
                        vol.status
                    );
                    Ok(count)
                }
                Ok(_) => {
                    self.restore_volume_status(drive, previous_status);
                    Err(format!("live MFT scan for {drive}: returned no records"))
                }
                Err(e) => {
                    self.restore_volume_status(drive, previous_status);
                    Err(format!("live MFT scan for {drive}: failed: {e}"))
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = drive;
            Err("MFT scanning is only available on Windows".to_string())
        }
    }

    /// Mutates one tracked volume; `false` when it is not indexed (or is excluded by configuration).
    pub fn update_volume<F: FnOnce(&mut VolumeIndex)>(&self, drive: char, update: F) -> bool {
        let mut lock = self.volumes.write().unwrap();
        match lock.get_mut(&drive) {
            Some(volume) => {
                update(volume);
                true
            }
            None => false,
        }
    }

    /// Adds or replaces a volume in the multi-volume engine. A replacement starts one generation
    /// past the table it hides, so a query session created against the old table cannot page into
    /// records that now belong to the new one.
    pub fn add_volume(&self, vol: VolumeIndex) {
        let cfg = self.config.read().unwrap();
        if cfg
            .excluded_drives
            .iter()
            .any(|&ex| ex.eq_ignore_ascii_case(&vol.drive_letter))
        {
            return;
        }
        drop(cfg);
        let mut lock = self.volumes.write().unwrap();
        let generation = lock
            .get(&vol.drive_letter)
            .map(|previous| previous.generation.wrapping_add(1));
        match generation {
            Some(generation) => {
                let mut vol = vol;
                vol.generation = generation;
                lock.insert(vol.drive_letter, vol);
            }
            None => {
                lock.insert(vol.drive_letter, vol);
            }
        }
    }

    /// Atomically saves all non-empty volume indexes to disk.
    pub fn save_all_indexes(&self) -> Result<(), persistence::PersistenceError> {
        let vols = self.volumes.read().unwrap();
        for vol in vols.values() {
            if !vol.records.is_empty() {
                vol.save_to_dir(&self.index_dir)?;
                tracing::info!(
                    "Atomically saved volume index for {}: ({} records, last_usn: {})",
                    vol.drive_letter,
                    vol.records.len(),
                    vol.last_usn
                );
            }
        }
        Ok(())
    }

    /// A cached journal position is usable only when it belongs to the current journal and lies in
    /// the current `[first_usn, next_usn]` window. A position ahead of `next_usn` cannot be caught
    /// up by reading records (there are none), so it must rebuild instead of polling forever.
    pub(super) fn cached_position_is_usable(
        cached_journal_id: u64,
        last_usn: i64,
        current: &cairn_ntfs::usn::UsnJournalData,
    ) -> bool {
        (cached_journal_id == 0 || cached_journal_id == current.journal_id)
            && last_usn >= current.first_usn
            && last_usn <= current.next_usn
    }

    pub(super) fn backlog_is_too_large(last_usn: i64, next_usn: i64) -> bool {
        next_usn.saturating_sub(last_usn) > MAX_USN_REPLAY_BACKLOG_BYTES
    }

    /// `true` while at least one volume is replaying changes, so the poll loop can shorten its
    /// interval and drain the backlog quickly.
    pub fn is_catching_up(&self) -> bool {
        self.volumes.read().unwrap().values().any(|volume| {
            matches!(
                volume.status.state(),
                VolumeState::CatchingUp(_) | VolumeState::Rebuilding
            )
        })
    }

    /// Polls real-time USN change journal on all supported fixed NTFS volumes.
    pub fn poll_usn_changes(&self) {
        #[cfg(windows)]
        {
            // Larger reads mean fewer 50ms rounds during a cold catch-up; 512 KiB is still a
            // transient allocation, not part of the steady-state memory budget.
            let mut buf = vec![0u8; 512 * 1024];
            let present: HashSet<char> = enumerate_fixed_drives().into_iter().collect();
            let drives: Vec<char> = {
                let lock = self.volumes.read().unwrap();
                let cfg = self.config.read().unwrap();
                lock.keys()
                    .copied()
                    .filter(|&d| {
                        !cfg.excluded_drives
                            .iter()
                            .any(|&ex| ex.eq_ignore_ascii_case(&d))
                    })
                    .collect()
            };

            for drive in drives {
                if !present.contains(&drive) {
                    // The volume is gone (ejected, unmounted): the retained index is stale by
                    // definition, so it must not stay reported as Synced (IDX-07).
                    let mut lock = self.volumes.write().unwrap();
                    if let Some(vol) = lock.get_mut(&drive) {
                        if vol.status.state() != VolumeState::Offline {
                            tracing::info!(
                                "Volume {} disappeared from the system: marked Offline",
                                drive
                            );
                            vol.status.set(VolumeState::Offline);
                        }
                    }
                    continue;
                }

                let (journal_id, last_usn, usn_enabled) = {
                    let lock = self.volumes.read().unwrap();
                    if let Some(vol) = lock.get(&drive) {
                        (vol.journal_id, vol.last_usn, vol.usn_enabled)
                    } else {
                        continue;
                    }
                };

                if !usn_enabled {
                    continue;
                }

                let handle = match cairn_ntfs::RawVolumeHandle::open(drive) {
                    Ok(handle) => handle,
                    Err(cairn_ntfs::VolumeError::OpenFailed { code, .. })
                        if is_device_offline_win32_error(code) =>
                    {
                        let mut lock = self.volumes.write().unwrap();
                        if let Some(vol) = lock.get_mut(&drive) {
                            if vol.status.state() != VolumeState::Offline {
                                tracing::info!(
                                    "Volume {} open failed with Win32 error {:#x}: marked Offline",
                                    drive,
                                    code
                                );
                                vol.status.set(VolumeState::Offline);
                            }
                        }
                        continue;
                    }
                    Err(_) => continue,
                };

                let (active_jid, query_usn) = if journal_id == 0 {
                    match handle.query_usn_journal() {
                        Ok(data) => (data.journal_id, data.next_usn),
                        Err(cairn_ntfs::usn::UsnError::QueryFailed(code))
                            if is_device_offline_win32_error(code) =>
                        {
                            let mut lock = self.volumes.write().unwrap();
                            if let Some(vol) = lock.get_mut(&drive) {
                                vol.status.set(VolumeState::Offline);
                            }
                            continue;
                        }
                        Err(_) => continue,
                    }
                } else {
                    (journal_id, last_usn)
                };

                match handle.read_usn_journal(active_jid, query_usn, &mut buf) {
                    Ok((next_usn, changes)) => {
                        // Query the tail for *every* batch, including an empty one. An empty read
                        // means no records were available from `query_usn`, not that the caller is
                        // at the tail; only a real tail can prove Synced. Omitting this query on an
                        // empty batch left a fully caught-up volume stuck in CatchingUp forever.
                        let tail_usn = handle.query_usn_journal().ok().map(|data| data.next_usn);
                        let mut lock = self.volumes.write().unwrap();
                        if let Some(vol) = lock.get_mut(&drive) {
                            let previous_usn = vol.last_usn;
                            vol.journal_id = active_jid;
                            vol.last_usn = next_usn;
                            if !changes.is_empty() {
                                vol.apply_usn_changes(changes);
                                vol.path_cache.clear();
                                // Mark dirty; do NOT synchronously rebuild PresortedIndices under write lock (SPEC SM-4, QRY-05)
                                vol.presorted_dirty = true;
                            }
                            if is_live_poll_trackable(vol.status.state()) {
                                vol.status.reconcile_poll(previous_usn, next_usn, tail_usn);
                            }
                        }
                    }
                    Err(cairn_ntfs::usn::UsnError::ReadFailed(code))
                        if is_device_offline_win32_error(code) =>
                    {
                        let mut lock = self.volumes.write().unwrap();
                        if let Some(vol) = lock.get_mut(&drive) {
                            if vol.status.state() != VolumeState::Offline {
                                tracing::info!(
                                    "USN read on drive {}: failed with device error {:#x}: marked Offline",
                                    drive,
                                    code
                                );
                                vol.status.set(VolumeState::Offline);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::trace!("USN read on drive {}: returned {:?}", drive, e);
                    }
                }
            }
        }
        self.process_pending_data_changes();
    }
}

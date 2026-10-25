// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Volume state and worker status definitions.
//!
//! These are transport-neutral, machine-readable state models only. Rendering a state into
//! user-facing text (and choosing its language, SPEC UI-07) is the presentation layer's job,
//! never the protocol's.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeStatusSummary {
    pub drive_letter: char,
    pub volume_guid: String,
    pub status: VolumeState,
    pub indexed_count: u64,
    pub usn_enabled: bool,
}

/// Per-volume state machine (SPEC IDX-07).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum VolumeState {
    /// Index is consistent with the change journal.
    Synced,
    /// Cached index loaded, change journal backlog still being applied.
    /// Carries a best-effort completion percentage (0..=100).
    CatchingUp(u8),
    /// Index is being rebuilt from the master file table.
    Rebuilding,
    /// Volume is not available for reading; the retained data is of unknown freshness.
    Offline,
    /// Change journal unavailable; index served from periodic snapshots (IDX-08).
    SnapshotMode,
}

impl VolumeState {
    /// Stable machine-readable key, safe to use as a localization lookup suffix and on the wire.
    pub fn state_key(&self) -> &'static str {
        match self {
            VolumeState::Synced => "synced",
            VolumeState::CatchingUp(_) => "catching_up",
            VolumeState::Rebuilding => "rebuilding",
            VolumeState::Offline => "offline",
            VolumeState::SnapshotMode => "snapshot_mode",
        }
    }

    pub fn is_synced(&self) -> bool {
        matches!(self, VolumeState::Synced)
    }

    /// `true` when results from this volume must be annotated as possibly out of date (IDX-07).
    /// Only `Synced` is exempt.
    pub fn is_stale(&self) -> bool {
        !self.is_synced()
    }

    /// Completion percentage while catching up; `None` for every other state.
    pub fn progress_percent(&self) -> Option<u8> {
        match self {
            VolumeState::CatchingUp(percent) => Some(*percent),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkerStatus {
    Idle,
    Indexing { count: usize },
    Disabled,
}

/// Lifecycle of the optional full-text content index. The phase is authoritative for whether a
/// `content:` query may run; a disabled, backfilling or failed index must never look like a query
/// that simply found nothing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentIndexState {
    Disabled,
    Backfilling { processed: u64, total: u64 },
    Ready,
    Error(String),
}

impl ContentIndexState {
    pub fn state_key(&self) -> &'static str {
        match self {
            ContentIndexState::Disabled => "disabled",
            ContentIndexState::Backfilling { .. } => "backfilling",
            ContentIndexState::Ready => "ready",
            ContentIndexState::Error(_) => "error",
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, ContentIndexState::Ready)
    }
}

/// What the index page and the query path read. `indexed_documents` and `index_bytes` describe the
/// last committed index, not the in-flight batch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentIndexStatus {
    pub state: ContentIndexState,
    pub indexed_documents: u64,
    pub index_bytes: u64,
    /// Unix seconds of the last successful commit; zero before the first one.
    pub last_updated_ts: i64,
}

impl Default for ContentIndexStatus {
    fn default() -> Self {
        Self {
            state: ContentIndexState::Disabled,
            indexed_documents: 0,
            index_bytes: 0,
            last_updated_ts: 0,
        }
    }
}

/// One file handed to `cairn-worker.exe`. The service owns discovery and MFT metadata; the worker
/// owns extraction and Tantivy writes, so a malformed document cannot take down the LocalSystem
/// process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentIndexTask {
    pub volume_id: u64,
    pub frn: u64,
    pub size: u64,
    pub modified_ts: i64,
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::VolumeState;

    #[test]
    fn volume_state_keys_are_unique_and_stable() {
        let states = [
            (VolumeState::Synced, "synced"),
            (VolumeState::CatchingUp(42), "catching_up"),
            (VolumeState::Rebuilding, "rebuilding"),
            (VolumeState::Offline, "offline"),
            (VolumeState::SnapshotMode, "snapshot_mode"),
        ];

        let mut keys: Vec<&str> = Vec::new();
        for (state, expected) in states {
            assert_eq!(state.state_key(), expected);
            assert_eq!(
                state.progress_percent(),
                if expected == "catching_up" {
                    Some(42)
                } else {
                    None
                }
            );
            keys.push(state.state_key());
        }
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "each state must map to a distinct key");
    }

    #[test]
    fn only_synced_state_carries_no_staleness_annotation() {
        assert!(!VolumeState::Synced.is_stale());
        assert!(VolumeState::is_synced(&VolumeState::Synced));
        for state in [
            VolumeState::CatchingUp(0),
            VolumeState::Rebuilding,
            VolumeState::Offline,
            VolumeState::SnapshotMode,
        ] {
            assert!(
                state.is_stale(),
                "{state:?} must be annotated as possibly stale (IDX-07)"
            );
        }
    }
}

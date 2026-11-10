// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Full-text content index pipeline (CNT-01, CNT-04, CNT-05, CNT-06, CNT-07).
//!
//! The service owns MFT discovery and decides which files are eligible. Extraction and Tantivy
//! writes happen in `CairnWorker.exe`; this module only builds batches, drives the worker and
//! publishes a state machine the GUI can show. The worker is never handed the LocalSystem
//! service's address space, so a malformed document cannot take the service down.

use cairn_protocol::status::ContentIndexTask;
use serde::{Deserialize, Serialize};

mod backfill;
mod status;
mod worker_process;

use self::worker_process::worker_exit_is_transient;

/// Format version of the completed-backfill marker. Bump it when the content index layout
/// changes; an old marker then forces a full rebuild instead of trusting an incompatible index.
const CONTENT_BACKFILL_SCHEMA: u32 = 2;
/// Proof that a full backfill with the current configuration finished and published Ready.
const CONTENT_BACKFILL_MARKER: &str = "backfill.complete.json";
/// Half-written replacement, renamed over the marker only after its payload is on disk.
const CONTENT_BACKFILL_MARKER_TMP: &str = "backfill.complete.json.tmp";

/// Commands handled by the single content thread. The thread owns the full-backfill state machine
/// so the pipe server never blocks on a disk-sized job.
pub enum ContentCommand {
    Start,
    IndexChanges {
        tasks: Vec<ContentIndexTask>,
        deletions: Vec<(u64, u64)>,
    },
    Cancel,
    Clear,
}

enum BatchOutcome {
    Completed,
    Cancelled,
    Failed(WorkerFailure),
}

/// A failed worker batch. `transient` records whether the worker's exit code names a handle
/// conflict that a retry can clear; the message keeps the process output for the log.
struct WorkerFailure {
    message: String,
    transient: bool,
}

impl BatchOutcome {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed(WorkerFailure {
            message: message.into(),
            transient: false,
        })
    }

    fn worker_failed(message: impl Into<String>, exit_code: Option<i32>) -> Self {
        Self::Failed(WorkerFailure {
            message: message.into(),
            transient: worker_exit_is_transient(exit_code),
        })
    }
}

/// Marker written after a full backfill reaches Ready. It records the configuration the index was
/// built with, not a live checksum: incremental updates deliberately do not rewrite it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackfillMarker {
    schema: u32,
    extensions: Vec<String>,
    max_bytes: u64,
    excluded_paths: Vec<String>,
    content_excluded_paths: Vec<String>,
    documents: u64,
    completed_ts: i64,
}

#[cfg(test)]
mod tests;

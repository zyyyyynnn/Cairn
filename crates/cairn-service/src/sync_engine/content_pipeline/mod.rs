// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Full-text content index pipeline (CNT-01, CNT-04, CNT-05, CNT-06, CNT-07).
//!
//! The service owns MFT discovery and decides which files are eligible. Extraction and Tantivy
//! writes happen in `CairnWorker.exe`; this module builds batches, drives the worker and
//! publishes a state machine the GUI can show. The one in-process write is the explicit clear
//! (`clear_index_inline`), used when no content thread exists. The worker is never handed the
//! `LocalSystem` service's address space, so a malformed document cannot take the service down.

use cairn_content::ContentSearcher;
use cairn_protocol::status::{ContentIndexState, ContentIndexStatus, ContentIndexTask};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

mod backfill;
mod status;
mod worker_process;

use self::status::content_dir_bytes;
use self::worker_process::worker_exit_is_transient;

pub(in crate::sync_engine) struct ContentPipeline {
    dir: PathBuf,
    searcher: RwLock<Option<Arc<ContentSearcher>>>,
    status: RwLock<ContentIndexStatus>,
    cancel: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    cancel_lock: Mutex<()>,
    inline_clear_lock: Mutex<()>,
    commands: Mutex<Option<std::sync::mpsc::Sender<ContentCommand>>>,
}

impl ContentPipeline {
    pub(in crate::sync_engine) fn new(dir: &Path, enabled: bool) -> Self {
        let status = if enabled {
            ContentIndexStatus {
                state: ContentIndexState::Backfilling {
                    processed: 0,
                    total: 0,
                },
                ..Default::default()
            }
        } else {
            ContentIndexStatus {
                state: ContentIndexState::Disabled,
                index_bytes: content_dir_bytes(dir),
                ..Default::default()
            }
        };
        Self {
            dir: dir.to_path_buf(),
            searcher: RwLock::new(None),
            status: RwLock::new(status),
            cancel: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            cancel_lock: Mutex::new(()),
            inline_clear_lock: Mutex::new(()),
            commands: Mutex::new(None),
        }
    }

    pub(in crate::sync_engine) fn dir(&self) -> &Path {
        &self.dir
    }

    fn cancel_flag(&self) -> &Arc<AtomicBool> {
        &self.cancel
    }

    fn shutdown_flag(&self) -> &Arc<AtomicBool> {
        &self.shutdown
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub(in crate::sync_engine) fn status(&self) -> ContentIndexStatus {
        self.status.read().unwrap().clone()
    }
}

const CONTENT_BACKFILL_SCHEMA: u32 = 2;
const CONTENT_BACKFILL_MARKER: &str = "backfill.complete.json";
const CONTENT_BACKFILL_MARKER_TMP: &str = "backfill.complete.json.tmp";

/// Commands handled by the single content thread. The thread owns the full-backfill state machine
/// so the pipe server never blocks on a disk-sized job.
pub enum ContentCommand {
    EnsureStarted,
    Start,
    IndexChanges {
        tasks: Vec<ContentIndexTask>,
        deletions: Vec<(u64, u64)>,
    },
    Cancel,
    Clear,
}

impl crate::sync_engine::MultiVolumeEngine {
    fn send_content_command(&self, command: ContentCommand) -> bool {
        self.content_pipeline.send_command(command)
    }
}

enum BatchOutcome {
    Completed,
    Cancelled,
    Failed(WorkerFailure),
}

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

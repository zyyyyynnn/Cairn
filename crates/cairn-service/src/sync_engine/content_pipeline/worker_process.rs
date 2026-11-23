// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Worker process surface: locating, spawning, bounding and collecting `CairnWorker` batches.

use super::BatchOutcome;
use crate::sync_engine::MultiVolumeEngine;
use crate::sync_engine::worker_runner::{
    TempFileGuard, WorkerProcessOutcome, next_batch_seq, run_bounded_worker, write_jsonl,
};
use cairn_protocol::status::ContentIndexTask;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// Upper bound for one worker batch. Cancellation is checked every 100 ms while waiting.
const CONTENT_WORKER_TIMEOUT: Duration = Duration::from_secs(300);

/// Port abstracting child-process execution and retry backoff for one `CairnWorker` batch.
pub(super) trait WorkerProcessPort {
    fn run_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome;

    /// Waits before retrying a transient sharing conflict; returns `false` if cancelled.
    fn wait_before_retry(&self) -> bool;
}

pub(super) struct SubprocessWorkerPort<'a> {
    engine: &'a MultiVolumeEngine,
}

impl<'a> SubprocessWorkerPort<'a> {
    pub(super) fn new(engine: &'a MultiVolumeEngine) -> Self {
        Self { engine }
    }
}

impl WorkerProcessPort for SubprocessWorkerPort<'_> {
    fn run_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        self.engine.run_worker_process(tasks, deletions, clear)
    }

    fn wait_before_retry(&self) -> bool {
        std::thread::sleep(Duration::from_millis(250));
        !self.engine.content_pipeline.is_cancelled()
    }
}

impl MultiVolumeEngine {
    pub(crate) fn worker_path(&self) -> Result<PathBuf, String> {
        #[cfg(test)]
        if let Some(worker) = &self.worker_binary {
            return Ok(worker.clone());
        }
        let exe = std::env::current_exe()
            .map_err(|err| format!("could not locate the service executable: {err}"))?;
        let dir = exe
            .parent()
            .ok_or_else(|| "service executable has no parent directory".to_string())?;
        // Production ships both binaries side by side; that is the only place the
        // service looks, so a copy in a parent directory can never shadow the
        // packaged worker.
        for name in ["CairnWorker.exe", "CairnWorker"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        Err(format!(
            "CairnWorker executable not found beside {}",
            exe.display()
        ))
    }

    /// Runs one worker batch, retrying once when the failure looks like a transient sharing
    /// conflict. The previous searcher is dropped first so its mapped index segments cannot keep
    /// the files locked while the retry runs.
    pub(super) fn run_worker_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        self.run_worker_batch_with(&SubprocessWorkerPort::new(self), tasks, deletions, clear)
    }

    pub(super) fn run_worker_batch_with(
        &self,
        port: &dyn WorkerProcessPort,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        let first = port.run_batch(tasks, deletions, clear);
        let err = match &first {
            BatchOutcome::Failed(failure) if failure.transient => failure.message.clone(),
            _ => return first,
        };
        tracing::warn!(
            "CairnWorker hit a transient sharing error; releasing the searcher and retrying once: {err}"
        );
        self.drop_content_searcher();
        if !port.wait_before_retry() || self.content_pipeline.is_cancelled() {
            return BatchOutcome::Cancelled;
        }
        port.run_batch(tasks, deletions, clear)
    }

    fn run_worker_process(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        let worker = match self.worker_path() {
            Ok(path) => path,
            Err(err) => return BatchOutcome::failed(err),
        };
        let content_dir = self.content_pipeline.dir();
        if let Err(err) = std::fs::create_dir_all(content_dir) {
            return BatchOutcome::failed(format!(
                "could not create content directory {}: {err}",
                content_dir.display()
            ));
        }

        let seq = next_batch_seq();
        let batch_path = content_dir.join(format!("batch-{}-{seq}.jsonl", std::process::id()));
        let delete_path = (!deletions.is_empty())
            .then(|| content_dir.join(format!("deletes-{}-{seq}.jsonl", std::process::id())));
        let cleanup_paths: Vec<PathBuf> = std::iter::once(batch_path.clone())
            .chain(delete_path.clone())
            .collect();
        let _cleanup = TempFileGuard::new(cleanup_paths);

        if let Err(err) = write_jsonl(&batch_path, tasks) {
            return BatchOutcome::failed(err);
        }
        if let Some(path) = &delete_path
            && let Err(err) = write_jsonl(path, deletions)
        {
            return BatchOutcome::failed(err);
        }

        let cfg = self.get_config();
        let mut args = vec![
            OsString::from("--index-dir"),
            content_dir.as_os_str().to_os_string(),
            OsString::from("--batch"),
            batch_path.into_os_string(),
            OsString::from("--max-bytes"),
            OsString::from(cfg.content_index_max_bytes.to_string()),
        ];
        if let Some(path) = delete_path {
            args.push(OsString::from("--delete-batch"));
            args.push(path.into_os_string());
        }
        if clear {
            args.push(OsString::from("--clear"));
        }

        match run_bounded_worker(
            &worker,
            &args,
            CONTENT_WORKER_TIMEOUT,
            Duration::from_millis(100),
            self.content_pipeline.cancel_flag(),
        ) {
            WorkerProcessOutcome::Completed { stdout, stderr } => {
                let summary: serde_json::Value =
                    serde_json::from_str(stdout.trim()).unwrap_or_default();
                let processed = summary["processed"].as_u64().unwrap_or(0);
                let failed = summary["failed"].as_u64().unwrap_or(0);
                if failed > 0 {
                    // A file can vanish between the USN event and extraction; the worker still
                    // committed its deletions, so only a non-zero exit is a batch failure.
                    tracing::warn!(
                        processed,
                        failed,
                        "CairnWorker skipped content files: {}",
                        stderr.trim()
                    );
                }
                BatchOutcome::Completed
            }
            WorkerProcessOutcome::Cancelled => BatchOutcome::Cancelled,
            WorkerProcessOutcome::TimedOut { .. } => BatchOutcome::failed("CairnWorker timed out"),
            WorkerProcessOutcome::Failed { exit_code, reason } => match exit_code {
                Some(_) => BatchOutcome::worker_failed(reason, exit_code),
                None => BatchOutcome::failed(reason),
            },
        }
    }
}

/// Whether the worker's exit code names a handle conflict a retry can clear.
pub(super) fn worker_exit_is_transient(exit_code: Option<i32>) -> bool {
    matches!(
        exit_code,
        Some(code)
            if code == i32::from(cairn_protocol::worker_exit::ACCESS_DENIED)
                || code == i32::from(cairn_protocol::worker_exit::SHARING_VIOLATION)
    )
}

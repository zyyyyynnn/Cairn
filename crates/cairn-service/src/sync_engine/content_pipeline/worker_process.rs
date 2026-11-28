// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::BatchOutcome;
use crate::sync_engine::MultiVolumeEngine;
use crate::sync_engine::worker_runner::{
    TempFileGuard, WorkerProcessOutcome, next_batch_seq, run_bounded_worker, write_jsonl,
};
use cairn_protocol::status::ContentIndexTask;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CONTENT_WORKER_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) trait WorkerProcessPort {
    fn run_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome;

    fn wait_before_retry(&self) -> bool;
}

pub(super) struct SubprocessWorkerPort<'a> {
    engine: &'a MultiVolumeEngine,
    cancel: &'a AtomicBool,
    shutdown: &'a AtomicBool,
}

impl<'a> SubprocessWorkerPort<'a> {
    pub(super) fn new(engine: &'a MultiVolumeEngine) -> Self {
        Self {
            engine,
            cancel: engine.content_pipeline.cancel_flag(),
            shutdown: engine.content_pipeline.shutdown_flag(),
        }
    }

    pub(super) fn for_clear(engine: &'a MultiVolumeEngine) -> Self {
        Self {
            engine,
            cancel: engine.content_pipeline.shutdown_flag(),
            shutdown: engine.content_pipeline.shutdown_flag(),
        }
    }
}

impl WorkerProcessPort for SubprocessWorkerPort<'_> {
    fn run_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        self.engine
            .run_worker_process(tasks, deletions, clear, self.cancel, self.shutdown)
    }

    fn wait_before_retry(&self) -> bool {
        std::thread::sleep(Duration::from_millis(250));
        !(self.cancel.load(Ordering::SeqCst) || self.shutdown.load(Ordering::SeqCst))
    }
}

impl MultiVolumeEngine {
    pub(crate) fn worker_path(&self) -> Result<PathBuf, String> {
        self.worker_locator.locate()
    }

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
        if !port.wait_before_retry() {
            return BatchOutcome::Cancelled;
        }
        port.run_batch(tasks, deletions, clear)
    }

    fn run_worker_process(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
        cancel: &AtomicBool,
        shutdown: &AtomicBool,
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
            cancel,
            Some(shutdown),
        ) {
            WorkerProcessOutcome::Completed { stdout, stderr } => {
                let summary: serde_json::Value =
                    serde_json::from_str(stdout.trim()).unwrap_or_default();
                let processed = summary["processed"].as_u64().unwrap_or(0);
                let failed = summary["failed"].as_u64().unwrap_or(0);
                if failed > 0 {
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
            WorkerProcessOutcome::IsolationFailed { reason } => BatchOutcome::failed(reason),
            WorkerProcessOutcome::Failed { exit_code, reason } => match exit_code {
                Some(_) => BatchOutcome::worker_failed(reason, exit_code),
                None => BatchOutcome::failed(reason),
            },
        }
    }
}

pub(super) fn worker_exit_is_transient(exit_code: Option<i32>) -> bool {
    matches!(
        exit_code,
        Some(code)
            if code == i32::from(cairn_protocol::worker_exit::ACCESS_DENIED)
                || code == i32::from(cairn_protocol::worker_exit::SHARING_VIOLATION)
    )
}

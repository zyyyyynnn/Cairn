// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Optional extended property extraction pipeline (`IDX-12`).
//!
//! Coordinates `PropertySideTables` lifecycle (strictly zero heap allocation when disabled) and
//! isolates all parsing / Win32 Shell Property fallback inside `CairnWorker.exe` subprocesses with
//! per-batch timeout & automatic single-item fault isolation.

use super::MultiVolumeEngine;
use cairn_core::model::FileRecord;
use cairn_core::property_store::SourceId;
use cairn_protocol::status::{
    ContentIndexState, PropertyExtractResult, PropertyExtractTask, PropertyIndexStatus,
};
use cairn_worker::isolation::{extract_with_isolation, IsolationOutcome};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROPERTY_BATCH_SIZE: usize = 64;
const DEFAULT_PROPERTY_WORKER_TIMEOUT: Duration = Duration::from_secs(15);

const PROPERTY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "mp3", "flac", "wav", "ogg", "m4a",
    "aac", "wma", "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "docx", "xlsx", "pptx", "odt",
    "ods", "odp", "pdf",
];

fn current_unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub fn is_property_eligible_extension(ext: &str) -> bool {
    !ext.is_empty()
        && PROPERTY_EXTENSIONS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

impl MultiVolumeEngine {
    pub fn spawn_property_extraction(self: &Arc<Self>) {
        if !self.get_config().property_index_enabled {
            return;
        }
        let engine = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("cairn-property-extract".to_string())
            .spawn(move || {
                engine.run_property_extraction();
            });
    }

    pub fn stop_property_extraction(&self) {
        self.property_cancel.store(true, Ordering::SeqCst);
    }

    pub fn property_index_status(&self) -> PropertyIndexStatus {
        let tables = self.property_tables.read().unwrap();
        let mut status = self.property_status.read().unwrap().clone();
        status.enabled = tables.is_enabled();
        status.indexed_records = tables.record_count() as u64;
        status.side_table_bytes = tables.allocated_bytes();
        status
    }

    pub fn set_property_index_enabled(&self, enabled: bool) -> PropertyIndexStatus {
        let _guard = self.config_apply_lock.lock().unwrap();
        {
            let mut cfg = self.config.write().unwrap();
            cfg.property_index_enabled = enabled;
        }
        self.apply_property_index_switch(enabled);
        self.property_index_status()
    }

    pub fn set_property_index_enabled_persisted(
        &self,
        enabled: bool,
        persist: bool,
    ) -> Result<PropertyIndexStatus, String> {
        self.apply_config_change(persist, |current| {
            let mut next = current.clone();
            next.property_index_enabled = enabled;
            Ok(next)
        })?;
        Ok(self.property_index_status())
    }

    pub(super) fn apply_property_index_switch(&self, enabled: bool) {
        self.property_cancel.store(!enabled, Ordering::SeqCst);
        {
            let mut tables = self.property_tables.write().unwrap();
            tables.set_enabled(enabled);
        }
        let mut status = self.property_status.write().unwrap();
        status.enabled = enabled;
        if !enabled {
            status.state = ContentIndexState::Disabled;
            status.indexed_records = 0;
            status.side_table_bytes = 0;
            status.last_updated_ts = 0;
        } else if matches!(status.state, ContentIndexState::Disabled) {
            status.state = ContentIndexState::Backfilling {
                processed: 0,
                total: 0,
            };
        }
    }

    /// Collects eligible non-dir, non-reparse live records across all active volumes and folder sources.
    pub fn collect_property_tasks(&self) -> Vec<PropertyExtractTask> {
        let cfg = self.config.read().unwrap().clone();
        let mut tasks = Vec::new();

        {
            let vols = self.volumes.read().unwrap();
            for vol in vols.values() {
                if cfg
                    .excluded_drives
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(&vol.drive_letter))
                {
                    continue;
                }
                let Some(source_id) = SourceId::for_drive(vol.drive_letter) else {
                    continue;
                };
                for (idx, rec) in vol.records().iter().enumerate() {
                    if !is_property_candidate(rec) {
                        continue;
                    }
                    let ext = vol
                        .arena()
                        .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
                    if !is_property_eligible_extension(ext) {
                        continue;
                    }
                    let path = vol.path_cache().get_full_path(
                        vol.records(),
                        vol.arena(),
                        vol.drive_letter,
                        idx as u32,
                    );
                    tasks.push(PropertyExtractTask {
                        source_id,
                        record_key: rec.frn,
                        modified_ts: rec.modified_ts,
                        path,
                    });
                }
            }
        }

        {
            let folders = self.folder_sources.read().unwrap();
            for folder in folders.values() {
                for (idx, rec) in folder.records().iter().enumerate() {
                    if !is_property_candidate(rec) {
                        continue;
                    }
                    let ext = folder
                        .arena()
                        .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
                    if !is_property_eligible_extension(ext) {
                        continue;
                    }
                    let path = folder.path_cache().get_full_path_for_root(
                        folder.records(),
                        folder.arena(),
                        &folder.root_prefix,
                        idx as u32,
                    );
                    tasks.push(PropertyExtractTask {
                        source_id: folder.source_id,
                        record_key: rec.frn,
                        modified_ts: rec.modified_ts,
                        path,
                    });
                }
            }
        }

        tasks
    }

    /// Extracts extended properties for all eligible indexed files using `worker_exe` subprocess
    /// isolation, bisecting crashing/timed-out batches so faulty files are skipped without halting.
    pub fn run_property_extraction_with_worker(
        &self,
        worker_exe: &Path,
        timeout: Duration,
    ) -> Result<PropertyIndexStatus, String> {
        let _extract_guard = self.property_extract_lock.lock().unwrap();
        if !self.property_tables.read().unwrap().is_enabled() {
            return Ok(self.property_index_status());
        }
        let tasks = self.collect_property_tasks();
        let total = tasks.len() as u64;
        {
            let mut status = self.property_status.write().unwrap();
            status.enabled = true;
            status.state = ContentIndexState::Backfilling {
                processed: 0,
                total,
            };
        }

        let work_dir = self.index_dir.join("property-work");
        for chunk in tasks.chunks(PROPERTY_BATCH_SIZE) {
            if self.property_cancel.load(Ordering::SeqCst) {
                break;
            }
            self.process_property_chunk(worker_exe, chunk, &work_dir, timeout, total);
        }

        {
            let enabled = self.property_tables.read().unwrap().is_enabled();
            let mut status = self.property_status.write().unwrap();
            status.enabled = enabled;
            status.state = if enabled {
                ContentIndexState::Ready
            } else {
                ContentIndexState::Disabled
            };
            status.last_updated_ts = current_unix_ts();
        }
        Ok(self.property_index_status())
    }

    pub fn run_property_extraction(&self) {
        if !self.property_tables.read().unwrap().is_enabled() {
            return;
        }
        let Ok(worker_exe) = self.worker_path() else {
            let mut status = self.property_status.write().unwrap();
            status.state = ContentIndexState::Error("CairnWorker executable not found".to_string());
            return;
        };
        let _ =
            self.run_property_extraction_with_worker(&worker_exe, DEFAULT_PROPERTY_WORKER_TIMEOUT);
    }

    fn process_property_chunk(
        &self,
        worker_exe: &Path,
        chunk: &[PropertyExtractTask],
        work_dir: &Path,
        timeout: Duration,
        total: u64,
    ) {
        if chunk.is_empty() || self.property_cancel.load(Ordering::SeqCst) {
            return;
        }
        match extract_with_isolation(worker_exe, chunk, work_dir, timeout, &self.property_cancel) {
            IsolationOutcome::Completed(results) => {
                self.commit_property_results(&results, chunk.len() as u64, total);
            }
            IsolationOutcome::Cancelled => {}
            IsolationOutcome::TimedOut { .. } | IsolationOutcome::WorkerFailed { .. } => {
                if chunk.len() > 1 {
                    let mid = chunk.len() / 2;
                    self.process_property_chunk(
                        worker_exe,
                        &chunk[..mid],
                        work_dir,
                        timeout,
                        total,
                    );
                    self.process_property_chunk(
                        worker_exe,
                        &chunk[mid..],
                        work_dir,
                        timeout,
                        total,
                    );
                } else {
                    self.advance_property_progress(1, total);
                }
            }
        }
    }

    fn advance_property_progress(&self, delta: u64, total: u64) {
        let mut status = self.property_status.write().unwrap();
        if let ContentIndexState::Backfilling { processed, .. } = &mut status.state {
            *processed = processed.saturating_add(delta).min(total);
        }
    }

    fn commit_property_results(
        &self,
        results: &[PropertyExtractResult],
        chunk_len: u64,
        total: u64,
    ) {
        {
            let mut tables = self.property_tables.write().unwrap();
            if !tables.is_enabled() {
                return;
            }
            for item in results {
                if let Some(ref props) = item.properties {
                    tables.upsert(item.source_id, item.record_key, props);
                }
            }
        }
        self.advance_property_progress(chunk_len, total);
    }
}

fn is_property_candidate(rec: &FileRecord) -> bool {
    !rec.is_tombstone() && !rec.is_dir() && !rec.is_reparse_point() && rec.size > 0
}

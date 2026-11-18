// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Optional extended property extraction pipeline (`IDX-12`).
//!
//! Coordinates `PropertySideTables` lifecycle (strictly zero heap allocation when disabled) and
//! isolates all parsing inside `CairnWorker.exe` subprocesses with per-batch timeout and automatic
//! single-item fault isolation. Extraction uses only the in-repo parsers; the Windows Shell
//! property system is never queried from the elevated worker.

use super::folder_source::FolderSource;
use super::volume_index::VolumeIndex;
use super::{unix_now_secs, MultiVolumeEngine, SourceTableView};
use crate::config::ServiceConfig;
use crate::exclusion::{ExclusionPath, ExclusionRules};
use cairn_core::model::FileRecord;
use cairn_core::property_store::{PropertySideTables, SourceId, PROPERTY_INDEX_EXTENSIONS};
use cairn_protocol::status::{
    ContentIndexState, PropertyExtractResult, PropertyExtractTask, PropertyIndexStatus,
};
use cairn_worker::isolation::{extract_with_isolation, IsolationOutcome};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

const PROPERTY_BATCH_SIZE: usize = 64;
const DEFAULT_PROPERTY_WORKER_TIMEOUT: Duration = Duration::from_secs(15);
/// Above this many individually tracked records a full revalidation pass replaces the delta set.
const PROPERTY_DELTA_RECORD_LIMIT: usize = 50_000;

pub(super) type PropertySpawner = Arc<Mutex<Option<Weak<MultiVolumeEngine>>>>;

pub(super) fn wake_property_spawner(spawner: &PropertySpawner) {
    let target = spawner.lock().unwrap().as_ref().and_then(Weak::upgrade);
    if let Some(engine) = target {
        engine.start_queued_property_extraction();
    }
}

#[derive(Clone)]
pub(super) struct PropertyRefreshHandles {
    pub tables: Arc<RwLock<PropertySideTables>>,
    pub refresh: Arc<Mutex<PropertyRefresh>>,
    pub spawner: PropertySpawner,
}

impl PropertyRefreshHandles {
    pub(super) fn wake(&self) {
        wake_property_spawner(&self.spawner);
    }

    fn enqueue_and_wake(&self, mark: impl FnOnce(&mut PropertyRefresh)) {
        let queued = {
            let tables = self.tables.read().unwrap();
            if tables.is_enabled() {
                mark(&mut self.refresh.lock().unwrap());
                true
            } else {
                false
            }
        };
        if queued {
            self.wake();
        }
    }

    pub(super) fn enqueue_source_and_wake(&self, source: SourceId) {
        self.enqueue_and_wake(|refresh| refresh.mark_source(source));
    }

    pub(super) fn enqueue_records_and_wake(
        &self,
        source: SourceId,
        records: impl IntoIterator<Item = u64>,
    ) {
        self.enqueue_and_wake(|refresh| refresh.mark_records(source, records));
    }

    pub(super) fn enqueue_batch_and_wake(
        &self,
        sources: &[SourceId],
        records: &[(SourceId, Vec<u64>)],
    ) {
        if sources.is_empty() && records.is_empty() {
            return;
        }
        self.enqueue_and_wake(|refresh| {
            for &source_id in sources {
                refresh.mark_source(source_id);
            }
            for (source_id, recs) in records {
                refresh.mark_records(*source_id, recs.iter().copied());
            }
        });
    }

    pub(super) fn retain_and_enqueue_source_with(
        &self,
        source: SourceId,
        collect_present_keys: impl FnOnce() -> HashSet<u64>,
    ) -> bool {
        let mut tables = self.tables.write().unwrap();
        if tables.is_enabled() {
            let present_keys = collect_present_keys();
            tables.retain_source_keys(source, &present_keys);
            self.refresh.lock().unwrap().mark_source(source);
            true
        } else {
            false
        }
    }
}

/// Coalesced work for the property pipeline. Sources request a complete recheck; USN and watcher
/// batches request individual records. Requests accumulate while extraction runs and are drained in
/// the worker loop until idle, so a busy volume never blocks callers or drops concurrent wakeups.
#[derive(Default)]
pub(super) struct PropertyRefresh {
    full: bool,
    sources: HashSet<SourceId>,
    records: HashSet<(u32, u64)>,
}

impl PropertyRefresh {
    pub(super) fn mark_full(&mut self) {
        self.full = true;
        self.sources.clear();
        self.records.clear();
    }

    pub(super) fn mark_source(&mut self, source: SourceId) {
        if !self.full {
            self.sources.insert(source);
        }
    }

    pub(super) fn mark_records(
        &mut self,
        source: SourceId,
        records: impl IntoIterator<Item = u64>,
    ) {
        if self.full {
            return;
        }
        self.records
            .extend(records.into_iter().map(|record| (source.0, record)));
        if self.records.len() > PROPERTY_DELTA_RECORD_LIMIT {
            self.mark_full();
        }
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    fn has_work(&self) -> bool {
        self.full || !self.sources.is_empty() || !self.records.is_empty()
    }

    fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

struct PropertyRunGuard<'a>(&'a AtomicBool);

impl Drop for PropertyRunGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub fn is_property_eligible_extension(ext: &str) -> bool {
    !ext.is_empty()
        && PROPERTY_INDEX_EXTENSIONS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

struct PlannedPropertyRefresh {
    tasks: Vec<PropertyExtractTask>,
    source_live_keys: Vec<(SourceId, HashSet<u64>)>,
    stale_records: Vec<(SourceId, u64)>,
    present_sources: Option<HashSet<u32>>,
}

struct PropertyExtraction<'a> {
    worker_exe: &'a Path,
    work_dir: &'a Path,
    timeout: Duration,
    total: u64,
    generation: u64,
    cancel: &'a AtomicBool,
    failed: u64,
    launch_failures: u64,
}

impl MultiVolumeEngine {
    pub(super) fn property_refresh_handles(&self) -> PropertyRefreshHandles {
        PropertyRefreshHandles {
            tables: Arc::clone(&self.property_tables),
            refresh: Arc::clone(&self.property_refresh),
            spawner: Arc::clone(&self.property_spawner),
        }
    }

    /// Registers this engine instance so background index updates and config changes can spawn
    /// queued property extraction passes.
    pub fn ensure_property_runtime(self: &Arc<Self>) {
        *self.property_spawner.lock().unwrap() = Some(Arc::downgrade(self));
    }

    pub fn spawn_property_extraction(self: &Arc<Self>) {
        self.ensure_property_runtime();
        self.start_queued_property_extraction();
    }

    fn start_queued_property_extraction(self: &Arc<Self>) {
        if !self.can_run_queued_property_extraction() {
            return;
        }
        if self.property_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("cairn-property-extract".to_string())
            .spawn(move || loop {
                {
                    let _guard = PropertyRunGuard(&engine.property_running);
                    while engine.can_run_queued_property_extraction() {
                        engine.run_property_extraction();
                    }
                }
                if !engine.can_run_queued_property_extraction()
                    || engine.property_running.swap(true, Ordering::SeqCst)
                {
                    break;
                }
            });
        if spawned.is_err() {
            self.property_running.store(false, Ordering::SeqCst);
        }
    }

    pub fn stop_property_extraction(&self) {
        self.property_stopped.store(true, Ordering::SeqCst);
        self.property_generation.fetch_add(1, Ordering::SeqCst);
        self.property_run_cancel
            .lock()
            .unwrap()
            .store(true, Ordering::SeqCst);
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
        let _ = self.apply_property_index_config(enabled, false);
        self.property_index_status()
    }

    pub fn set_property_index_enabled_persisted(
        &self,
        enabled: bool,
        persist: bool,
    ) -> Result<PropertyIndexStatus, String> {
        self.apply_property_index_config(enabled, persist)
    }

    fn apply_property_index_config(
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

    pub(super) fn apply_property_config_change(&self, enabled: bool, scope_changed: bool) {
        let state_changed = self.property_tables.read().unwrap().is_enabled() != enabled;
        if state_changed {
            self.property_generation.fetch_add(1, Ordering::SeqCst);
            self.property_run_cancel
                .lock()
                .unwrap()
                .store(true, Ordering::SeqCst);
            let mut tables = self.property_tables.write().unwrap();
            tables.set_enabled(enabled);
            let mut refresh = self.property_refresh.lock().unwrap();
            if enabled {
                refresh.mark_full();
            } else {
                refresh.clear();
            }
            let mut status = self.property_status.write().unwrap();
            status.enabled = enabled;
            if !enabled {
                status.state = ContentIndexState::Disabled;
                status.indexed_records = 0;
                status.side_table_bytes = 0;
                status.last_updated_ts = 0;
            } else {
                status.state = ContentIndexState::Backfilling {
                    processed: 0,
                    total: 0,
                };
            }
            return;
        }
        if !enabled {
            return;
        }
        let tables = self.property_tables.read().unwrap();
        if !tables.is_enabled() {
            return;
        }
        let mut refresh = self.property_refresh.lock().unwrap();
        let mut status = self.property_status.write().unwrap();
        if matches!(status.state, ContentIndexState::Error(_)) {
            self.property_generation.fetch_add(1, Ordering::SeqCst);
            status.state = ContentIndexState::Backfilling {
                processed: 0,
                total: 0,
            };
            refresh.mark_full();
        } else if scope_changed {
            refresh.mark_full();
        }
    }

    fn update_property_status_if_current(
        &self,
        generation: u64,
        update: impl FnOnce(&PropertySideTables, &mut PropertyIndexStatus),
    ) -> bool {
        let tables = self.property_tables.read().unwrap();
        let mut status = self.property_status.write().unwrap();
        if self.property_stopped.load(Ordering::SeqCst)
            || self.property_generation.load(Ordering::SeqCst) != generation
            || !tables.is_enabled()
        {
            return false;
        }
        update(&tables, &mut status);
        true
    }

    fn take_current_property_refresh(&self, generation: u64) -> Option<PropertyRefresh> {
        let mut refresh = self.property_refresh.lock().unwrap();
        if self.property_generation.load(Ordering::SeqCst) != generation
            || self.property_stopped.load(Ordering::SeqCst)
        {
            return None;
        }
        Some(refresh.take())
    }

    fn prepare_extraction_tasks(
        &self,
        generation: u64,
        planned: PlannedPropertyRefresh,
    ) -> Option<Vec<PropertyExtractTask>> {
        let mut tasks = planned.tasks;
        let mut tables = self.property_tables.write().unwrap();
        if !tables.is_enabled()
            || self.property_stopped.load(Ordering::SeqCst)
            || self.property_generation.load(Ordering::SeqCst) != generation
        {
            return None;
        }
        if let Some(present_sources) = planned.present_sources {
            tables.retain_sources(&present_sources);
        }
        for (source_id, live_keys) in planned.source_live_keys {
            tables.retain_source_keys(source_id, &live_keys);
        }
        let mut removed_any = false;
        for (source_id, record_key) in planned.stale_records {
            if tables.remove(source_id, record_key) {
                removed_any = true;
            }
        }
        if removed_any {
            tables.compact_strings();
        }
        tasks.retain(|task| {
            tables
                .get(task.source_id, task.record_key, task.modified_ts)
                .is_none()
        });
        Some(tasks)
    }

    fn finish_property_status(
        &self,
        generation: u64,
        total: u64,
        failed: u64,
        launch_failures: u64,
        full_pass: bool,
    ) -> bool {
        self.update_property_status_if_current(generation, |tables, status| {
            let indexed_count = tables.record_count();
            status.enabled = true;
            if total == 0 {
                if !matches!(status.state, ContentIndexState::Error(_)) {
                    status.state = ContentIndexState::Ready;
                    status.last_updated_ts = unix_now_secs();
                }
            } else if failed == total
                && (launch_failures == total || (full_pass && indexed_count == 0))
            {
                status.state = ContentIndexState::Error(format!(
                    "property extraction failed for {failed} files"
                ));
                status.last_updated_ts = unix_now_secs();
            } else {
                status.state = ContentIndexState::Ready;
                status.last_updated_ts = unix_now_secs();
            }
        })
    }

    fn can_run_queued_property_extraction(&self) -> bool {
        if self.property_stopped.load(Ordering::SeqCst) {
            return false;
        }
        let tables = self.property_tables.read().unwrap();
        if !tables.is_enabled() {
            return false;
        }
        if !self.property_refresh.lock().unwrap().has_work() {
            return false;
        }
        !matches!(
            self.property_status.read().unwrap().state,
            ContentIndexState::Error(_)
        )
    }

    /// Refreshes queued property work using `worker_exe` subprocess isolation, bisecting crashing or
    /// timed-out batches so faulty files are skipped without halting.
    pub fn run_property_extraction_with_worker(
        &self,
        worker_exe: &Path,
        timeout: Duration,
    ) -> Result<PropertyIndexStatus, String> {
        let _extract_guard = self.property_extract_lock.lock().unwrap();
        if !self.property_tables.read().unwrap().is_enabled()
            || self.property_stopped.load(Ordering::SeqCst)
        {
            return Ok(self.property_index_status());
        }
        let generation = self.property_generation.load(Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        *self.property_run_cancel.lock().unwrap() = Arc::clone(&cancel);
        let Some(requests) = self.take_current_property_refresh(generation) else {
            return Ok(self.property_index_status());
        };
        let full_pass = requests.full;

        let planned = self.plan_property_refresh_with_scope(&requests, true);
        let Some(tasks) = self.prepare_extraction_tasks(generation, planned) else {
            return Ok(self.property_index_status());
        };

        let total = tasks.len() as u64;
        if total > 0 {
            self.update_property_status_if_current(generation, |_, status| {
                status.state = ContentIndexState::Backfilling {
                    processed: 0,
                    total,
                };
            });
        }

        let work_dir = self.index_dir.join("property-work");
        let mut run = PropertyExtraction {
            worker_exe,
            work_dir: &work_dir,
            timeout,
            total,
            generation,
            cancel: &cancel,
            failed: 0,
            launch_failures: 0,
        };
        for chunk in tasks.chunks(PROPERTY_BATCH_SIZE) {
            if cancel.load(Ordering::SeqCst) || !self.property_run_is_current(generation) {
                break;
            }
            self.process_property_chunk(chunk, &mut run);
        }

        let updated = self.finish_property_status(
            generation,
            total,
            run.failed,
            run.launch_failures,
            full_pass,
        );
        if updated && run.failed > 0 {
            tracing::warn!(
                failed = run.failed,
                total,
                "property extraction skipped files that could not be parsed"
            );
        }
        Ok(self.property_index_status())
    }

    pub fn run_property_extraction(&self) {
        if self.property_stopped.load(Ordering::SeqCst)
            || !self.property_tables.read().unwrap().is_enabled()
        {
            return;
        }
        let generation = self.property_generation.load(Ordering::SeqCst);
        let Ok(worker_exe) = self.worker_path() else {
            let tables = self.property_tables.read().unwrap();
            let mut refresh = self.property_refresh.lock().unwrap();
            let mut status = self.property_status.write().unwrap();
            if !self.property_stopped.load(Ordering::SeqCst)
                && self.property_generation.load(Ordering::SeqCst) == generation
                && tables.is_enabled()
            {
                refresh.clear();
                status.state =
                    ContentIndexState::Error("CairnWorker executable not found".to_string());
            }
            return;
        };
        let _ =
            self.run_property_extraction_with_worker(&worker_exe, DEFAULT_PROPERTY_WORKER_TIMEOUT);
    }

    fn property_run_is_current(&self, generation: u64) -> bool {
        !self.property_stopped.load(Ordering::SeqCst)
            && self.property_generation.load(Ordering::SeqCst) == generation
            && self.property_tables.read().unwrap().is_enabled()
    }

    fn plan_property_refresh_with_scope(
        &self,
        requests: &PropertyRefresh,
        track_sources: bool,
    ) -> PlannedPropertyRefresh {
        let cfg = self.config.read().unwrap().clone();
        let collector = PropertyCollector::new(&cfg, &self.self_data_dirs);
        let mut planned = PlannedPropertyRefresh {
            tasks: Vec::new(),
            source_live_keys: Vec::new(),
            stale_records: Vec::new(),
            present_sources: (track_sources && requests.full).then(HashSet::new),
        };
        let mut seen = HashSet::new();
        let mut selected_sources = HashSet::new();
        let volumes = self.volumes.read().unwrap();
        let folders = self.folder_sources.read().unwrap();
        let volumes_by_source: HashMap<SourceId, &VolumeIndex> = volumes
            .values()
            .filter(|volume| drive_is_indexed(&cfg, volume.drive_letter))
            .filter_map(|volume| {
                SourceId::for_drive(volume.drive_letter).map(|source_id| (source_id, volume))
            })
            .collect();

        for (&source_id, volume) in &volumes_by_source {
            if !requests.full && !requests.sources.contains(&source_id) {
                continue;
            }
            collector.collect_source(
                source_id,
                &volume.table_view(),
                &mut seen,
                &mut selected_sources,
                track_sources,
                &mut planned,
            );
        }
        for folder in folders.values() {
            if !requests.full && !requests.sources.contains(&folder.source_id) {
                continue;
            }
            collector.collect_source(
                folder.source_id,
                &folder.table_view(),
                &mut seen,
                &mut selected_sources,
                track_sources,
                &mut planned,
            );
        }
        if requests.full {
            return planned;
        }

        if track_sources {
            for &source in &requests.sources {
                if !selected_sources.contains(&source.0) {
                    planned.source_live_keys.push((source, HashSet::new()));
                }
            }
        }
        for &(source, record_key) in &requests.records {
            let source_id = SourceId(source);
            if requests.sources.contains(&source_id) || !seen.insert((source, record_key)) {
                continue;
            }
            let task = if let Some(volume) = volumes_by_source.get(&source_id) {
                collector.task_for_frn(
                    source_id,
                    &volume.table_view(),
                    volume.record_indices_for_frn(record_key),
                )
            } else if let Some(folder) = folders.get(&source_id) {
                collector.task_for_frn(
                    source_id,
                    &folder.table_view(),
                    folder.record_indices_for_frn(record_key),
                )
            } else {
                None
            };
            match task {
                Some(task) => planned.tasks.push(task),
                None => planned.stale_records.push((source_id, record_key)),
            }
        }
        planned
    }

    fn process_property_chunk(&self, chunk: &[PropertyExtractTask], run: &mut PropertyExtraction) {
        if chunk.is_empty() || run.cancel.load(Ordering::SeqCst) {
            return;
        }
        match extract_with_isolation(run.worker_exe, chunk, run.work_dir, run.timeout, run.cancel) {
            IsolationOutcome::Completed(results) => {
                self.commit_property_results(
                    &results,
                    chunk.len() as u64,
                    run.total,
                    run.generation,
                );
            }
            IsolationOutcome::Cancelled => {}
            outcome @ (IsolationOutcome::TimedOut { .. }
            | IsolationOutcome::WorkerFailed { .. }) => {
                if chunk.len() > 1 {
                    let mid = chunk.len() / 2;
                    self.process_property_chunk(&chunk[..mid], run);
                    self.process_property_chunk(&chunk[mid..], run);
                } else {
                    run.failed += 1;
                    let reason = match outcome {
                        IsolationOutcome::WorkerFailed {
                            exit_code: None,
                            reason,
                        } => {
                            run.launch_failures += 1;
                            reason
                        }
                        IsolationOutcome::WorkerFailed { reason, .. }
                        | IsolationOutcome::TimedOut { reason } => reason,
                        IsolationOutcome::Completed(_) | IsolationOutcome::Cancelled => {
                            unreachable!()
                        }
                    };
                    tracing::warn!(
                        path = %super::log_safe(&chunk[0].path),
                        reason = %super::log_safe(&reason),
                        "property extraction failed for file"
                    );
                    self.advance_property_progress(1, run.total, run.generation);
                }
            }
        }
    }

    fn advance_property_progress(&self, delta: u64, total: u64, generation: u64) {
        self.update_property_status_if_current(generation, |_, status| {
            if let ContentIndexState::Backfilling { processed, .. } = &mut status.state {
                *processed = processed.saturating_add(delta).min(total);
            }
        });
    }

    fn commit_property_results(
        &self,
        results: &[PropertyExtractResult],
        chunk_len: u64,
        total: u64,
        generation: u64,
    ) {
        if !self.property_run_is_current(generation) {
            return;
        }
        let volumes = self.volumes.read().unwrap();
        let folders = self.folder_sources.read().unwrap();
        {
            let mut tables = self.property_tables.write().unwrap();
            if !tables.is_enabled()
                || self.property_stopped.load(Ordering::SeqCst)
                || self.property_generation.load(Ordering::SeqCst) != generation
            {
                return;
            }
            let mut stale = 0u64;
            let mut removed_any = false;
            for item in results {
                if !result_record_is_current(&volumes, &folders, item) {
                    stale += 1;
                    continue;
                }
                match item.properties {
                    Some(ref props) => {
                        if !tables.upsert(item.source_id, item.record_key, props, item.modified_ts)
                        {
                            removed_any = true;
                        }
                    }
                    None => {
                        if tables.remove(item.source_id, item.record_key) {
                            removed_any = true;
                        }
                    }
                }
            }
            if removed_any {
                tables.compact_strings();
            }
            if stale > 0 {
                tracing::debug!(
                    stale,
                    "property results dropped because their records changed during extraction"
                );
            }
        }
        self.advance_property_progress(chunk_len, total, generation);
    }
}

fn drive_is_indexed(cfg: &ServiceConfig, drive: char) -> bool {
    !cfg.excluded_drives
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(&drive))
}

struct PropertyCollector {
    path_rules: ExclusionRules,
    content_rules: ExclusionRules,
    self_dirs: Vec<String>,
}

impl PropertyCollector {
    fn new(cfg: &ServiceConfig, self_data_dirs: &[PathBuf]) -> Self {
        Self {
            path_rules: ExclusionRules::parse(&cfg.excluded_paths),
            content_rules: ExclusionRules::parse(&cfg.content_excluded_paths),
            self_dirs: self_data_dirs
                .iter()
                .map(|dir| {
                    let lower = dir.to_string_lossy().to_ascii_lowercase();
                    if lower.ends_with('\\') {
                        lower
                    } else {
                        format!("{lower}\\")
                    }
                })
                .collect(),
        }
    }

    fn collect_source(
        &self,
        source_id: SourceId,
        view: &SourceTableView<'_>,
        seen: &mut HashSet<(u32, u64)>,
        selected_sources: &mut HashSet<u32>,
        track_sources: bool,
        planned: &mut PlannedPropertyRefresh,
    ) {
        selected_sources.insert(source_id.0);
        if let Some(present_sources) = &mut planned.present_sources {
            present_sources.insert(source_id.0);
        }
        let mut live_keys = track_sources.then(HashSet::new);
        for (index, record) in view.records.iter().enumerate() {
            if let Some(task) = self.task_for_source(source_id, view, index as u32, record) {
                if seen.insert((source_id.0, record.frn)) {
                    if let Some(keys) = &mut live_keys {
                        keys.insert(record.frn);
                    }
                    planned.tasks.push(task);
                }
            }
        }
        if let Some(keys) = live_keys {
            planned.source_live_keys.push((source_id, keys));
        }
    }

    fn task_for_frn(
        &self,
        source_id: SourceId,
        view: &SourceTableView<'_>,
        indices: Option<&[u32]>,
    ) -> Option<PropertyExtractTask> {
        indices?.iter().find_map(|&index| {
            view.records
                .get(index as usize)
                .filter(|record| !record.is_tombstone())
                .and_then(|record| self.task_for_source(source_id, view, index, record))
        })
    }

    fn task_for_source(
        &self,
        source_id: SourceId,
        view: &SourceTableView<'_>,
        index: u32,
        record: &FileRecord,
    ) -> Option<PropertyExtractTask> {
        if !is_property_candidate(record) {
            return None;
        }
        let ext = view
            .arena
            .get_ext(record.name_offset, record.name_len, record.ext_offset);
        if !is_property_eligible_extension(ext) {
            return None;
        }
        let path = view.path_cache.get_full_path_with_root(
            view.records,
            view.arena,
            &view.root_prefix,
            index,
        );
        if self.is_excluded(&path) {
            return None;
        }
        Some(PropertyExtractTask {
            source_id,
            record_key: record.frn,
            modified_ts: record.modified_ts,
            path,
        })
    }

    fn is_excluded(&self, path: &str) -> bool {
        let prepared = ExclusionPath::new(path);
        let lower = path.to_ascii_lowercase();
        self.path_rules.excludes(&prepared)
            || self.content_rules.excludes(&prepared)
            || self.self_dirs.iter().any(|dir| {
                let root = dir.trim_end_matches('\\');
                lower == root || lower.starts_with(dir)
            })
    }
}

fn result_record_is_current(
    volumes: &HashMap<char, VolumeIndex>,
    folders: &HashMap<SourceId, FolderSource>,
    item: &PropertyExtractResult,
) -> bool {
    if let Some(drive) = item.source_id.drive_letter() {
        return volumes.get(&drive).is_some_and(|volume| {
            record_list_matches(
                volume.records(),
                volume.record_indices_for_frn(item.record_key),
                item.modified_ts,
            )
        });
    }
    folders.get(&item.source_id).is_some_and(|folder| {
        record_list_matches(
            folder.records(),
            folder.record_indices_for_frn(item.record_key),
            item.modified_ts,
        )
    })
}

fn record_list_matches(
    records: &[FileRecord],
    indices: Option<&[u32]>,
    expected_modified_ts: i64,
) -> bool {
    indices.is_some_and(|indices| {
        indices.iter().any(|&index| {
            records.get(index as usize).is_some_and(|record| {
                !record.is_tombstone() && record.modified_ts == expected_modified_ts
            })
        })
    })
}

fn is_property_candidate(rec: &FileRecord) -> bool {
    !rec.is_tombstone() && !rec.is_dir() && !rec.is_reparse_point() && rec.size > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_requests_coalesce_and_collapse_large_deltas() {
        let mut refresh = PropertyRefresh::default();
        refresh.mark_source(SourceId(1));
        refresh.mark_records(SourceId(1), [7, 8, 8]);
        let taken = refresh.take();
        assert!(taken.sources.contains(&SourceId(1)));
        assert_eq!(taken.records.len(), 2);
        assert!(!refresh.has_work());

        let mut large = PropertyRefresh::default();
        large.mark_records(SourceId(2), 0..=PROPERTY_DELTA_RECORD_LIMIT as u64);
        assert!(large.full);
        assert!(large.sources.is_empty());
        assert!(large.records.is_empty());
    }

    #[test]
    fn incremental_parse_failure_preserves_ready_while_launch_failure_sets_error() {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("media");
        let service = temp.path().join("service");
        std::fs::create_dir_all(&media).unwrap();
        let img = media.join("photo.png");
        std::fs::write(&img, b"bad").unwrap();

        let engine = MultiVolumeEngine::with_paths(
            &service.join("index"),
            &service.join("content"),
            &service.join("service.toml"),
            vec![],
            ServiceConfig {
                excluded_paths: Vec::new(),
                content_excluded_paths: Vec::new(),
                property_index_enabled: true,
                ..Default::default()
            },
        );
        let source_id = SourceId(1000);
        let mut folder = FolderSource::new(source_id, &media).unwrap();
        folder.rescan(&[]).unwrap();
        let folder_root = folder.display_root().to_string();
        let img_rec = folder
            .records()
            .iter()
            .find(|r| folder.arena().get_name(r.name_offset, r.name_len) == "photo.png")
            .copied()
            .unwrap();
        engine
            .config
            .write()
            .unwrap()
            .folder_sources
            .push(folder_root.clone());
        engine
            .folder_sources
            .write()
            .unwrap()
            .insert(source_id, folder);

        let missing = temp.path().join("missing-worker.exe");
        let status = engine
            .run_property_extraction_with_worker(&missing, Duration::from_secs(5))
            .unwrap();
        assert!(matches!(status.state, ContentIndexState::Error(_)));

        // An empty delta (no tasks) must keep Error instead of clearing it to Ready.
        engine
            .property_refresh
            .lock()
            .unwrap()
            .mark_records(source_id, [999_999]);
        let empty_delta_status = engine
            .run_property_extraction_with_worker(&missing, Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            empty_delta_status.state,
            ContentIndexState::Error(_)
        ));

        // Re-submitting enabled via apply_config_change retries from Error -> Backfilling.
        engine.update_config(ServiceConfig {
            folder_sources: vec![folder_root],
            excluded_paths: Vec::new(),
            content_excluded_paths: Vec::new(),
            property_index_enabled: true,
            ..Default::default()
        });
        assert!(matches!(
            engine.property_index_status().state,
            ContentIndexState::Backfilling { .. }
        ));

        // Seed one valid slot and confirm a single-item parse failure keeps Ready, whereas a
        // launch failure (`exit_code: None`) on every queued task still transitions to Error.
        {
            let mut tables = engine.property_tables.write().unwrap();
            assert!(tables.upsert(
                source_id,
                img_rec.frn.wrapping_add(1),
                &cairn_core::property_store::RecordPropertyData {
                    dimensions: Some((10, 10)),
                    ..Default::default()
                },
                img_rec.modified_ts,
            ));
        }
        {
            let mut st = engine.property_status.write().unwrap();
            st.state = ContentIndexState::Ready;
        }
        #[cfg(windows)]
        {
            let fail_cmd = temp.path().join("fail-worker.cmd");
            std::fs::write(&fail_cmd, b"@exit /b 1\r\n").unwrap();
            engine.property_refresh.lock().unwrap().take();
            engine
                .property_refresh
                .lock()
                .unwrap()
                .mark_records(source_id, [img_rec.frn]);
            let planned = engine.plan_property_refresh_with_scope(
                &PropertyRefresh {
                    full: true,
                    ..Default::default()
                },
                true,
            );
            assert_eq!(planned.tasks.len(), 1);
            let after_inc = engine
                .run_property_extraction_with_worker(&fail_cmd, Duration::from_secs(5))
                .unwrap();
            assert_eq!(after_inc.state, ContentIndexState::Ready);
            assert_eq!(after_inc.indexed_records, 1);

            engine
                .property_refresh
                .lock()
                .unwrap()
                .mark_records(source_id, [img_rec.frn]);
            let after_launch_err = engine
                .run_property_extraction_with_worker(&missing, Duration::from_secs(5))
                .unwrap();
            assert!(matches!(
                after_launch_err.state,
                ContentIndexState::Error(_)
            ));
            assert_eq!(after_launch_err.indexed_records, 1);
        }
    }

    #[test]
    fn stop_blocks_spawn_and_stale_generation_cannot_consume_newer_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let service = temp.path().join("service");
        let engine = Arc::new(MultiVolumeEngine::with_paths(
            &service.join("index"),
            &service.join("content"),
            &service.join("service.toml"),
            vec![],
            ServiceConfig {
                property_index_enabled: true,
                ..Default::default()
            },
        ));

        // Simulate disable -> enable advancing generation while a stale refresh could be checked.
        let gen_before = engine.property_generation.load(Ordering::SeqCst);
        engine.apply_property_config_change(false, false);
        engine.apply_property_config_change(true, false);
        assert_ne!(
            engine.property_generation.load(Ordering::SeqCst),
            gen_before
        );
        assert!(engine.property_refresh.lock().unwrap().has_work());
        assert!(!engine.property_run_is_current(gen_before));

        // stop_property_extraction permanently blocks background spawns even if work is queued.
        engine.stop_property_extraction();
        engine.property_refresh.lock().unwrap().mark_full();
        engine.spawn_property_extraction();
        assert!(!engine.property_running.load(Ordering::SeqCst));
    }

    #[cfg(windows)]
    #[test]
    fn spawn_loop_drains_reenable_race_after_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("media");
        let service = temp.path().join("service");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("clip.png"), b"bad").unwrap();

        let slow_cmd = temp.path().join("slow-worker.cmd");
        std::fs::write(&slow_cmd, b"@ping -n 3 127.0.0.1 >nul\r\n@exit /b 1\r\n").unwrap();

        let mut engine_val = MultiVolumeEngine::with_paths(
            &service.join("index"),
            &service.join("content"),
            &service.join("service.toml"),
            vec![],
            ServiceConfig {
                excluded_paths: Vec::new(),
                content_excluded_paths: Vec::new(),
                property_index_enabled: true,
                ..Default::default()
            },
        );
        engine_val.set_worker_binary(slow_cmd);
        let engine = Arc::new(engine_val);

        let sid = SourceId(1001);
        let mut folder = FolderSource::new(sid, &media).unwrap();
        folder.rescan(&[]).unwrap();
        engine
            .config
            .write()
            .unwrap()
            .folder_sources
            .push(folder.display_root().to_string());
        engine.folder_sources.write().unwrap().insert(sid, folder);

        engine.spawn_property_extraction();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(
            engine.property_index_status().state,
            ContentIndexState::Backfilling { total, .. } if total > 0
        ) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        // Disable then immediately re-enable while the generation-0 worker is still winding down;
        // the configuration apply wakes the registered runtime without a second manual spawn.
        engine.set_property_index_enabled(false);
        engine.set_property_index_enabled(true);

        let drain_deadline = std::time::Instant::now() + Duration::from_secs(10);
        while engine.property_running.load(Ordering::SeqCst)
            || engine.property_refresh.lock().unwrap().has_work()
        {
            assert!(
                std::time::Instant::now() < drain_deadline,
                "re-enabled refresh stayed stuck in queue"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(
            engine.property_index_status().state,
            ContentIndexState::Error(_)
        ));
    }

    #[test]
    fn runtime_wakes_spawner_on_error_retry_and_scope_update() {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("media");
        let service = temp.path().join("service");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("clip.png"), b"bad").unwrap();

        let sid = SourceId(1002);
        let mut folder = FolderSource::new(sid, &media).unwrap();
        folder.rescan(&[]).unwrap();
        let folder_root = folder.display_root().to_string();

        let mut engine_val = MultiVolumeEngine::with_paths(
            &service.join("index"),
            &service.join("content"),
            &service.join("service.toml"),
            vec![],
            ServiceConfig {
                excluded_paths: vec![folder_root.clone()],
                content_excluded_paths: Vec::new(),
                property_index_enabled: true,
                ..Default::default()
            },
        );
        engine_val.set_worker_binary(temp.path().join("missing-worker.exe"));
        let engine = Arc::new(engine_val);
        engine
            .config
            .write()
            .unwrap()
            .folder_sources
            .push(folder_root.clone());
        engine.folder_sources.write().unwrap().insert(sid, folder);

        // Drain the initial excluded scope into settled Ready with an empty refresh queue.
        engine.spawn_property_extraction();
        let settled_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.property_running.load(Ordering::SeqCst)
            || engine.property_index_status().state != ContentIndexState::Ready
            || engine.property_refresh.lock().unwrap().has_work()
        {
            assert!(std::time::Instant::now() < settled_deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        // Clearing excluded_paths triggers property_scope_changed and wakes registered runtime.
        engine.update_config(ServiceConfig {
            folder_sources: vec![folder_root],
            excluded_paths: Vec::new(),
            content_excluded_paths: Vec::new(),
            property_index_enabled: true,
            ..Default::default()
        });
        let scope_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.property_running.load(Ordering::SeqCst)
            || !matches!(
                engine.property_index_status().state,
                ContentIndexState::Error(_)
            )
        {
            assert!(std::time::Instant::now() < scope_deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        // Retrying enabled after Error must drain cleanly through the registered spawner.
        engine.set_property_index_enabled(true);
        let retry_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.property_running.load(Ordering::SeqCst)
            || engine.property_refresh.lock().unwrap().has_work()
        {
            assert!(std::time::Instant::now() < retry_deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            engine.property_index_status().state,
            ContentIndexState::Error(_)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn incremental_all_failures_keep_ready_with_an_empty_store() {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("media");
        let service = temp.path().join("service");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("photo.png"), b"bad").unwrap();

        let engine = MultiVolumeEngine::with_paths(
            &service.join("index"),
            &service.join("content"),
            &service.join("service.toml"),
            vec![],
            ServiceConfig {
                excluded_paths: Vec::new(),
                content_excluded_paths: Vec::new(),
                property_index_enabled: true,
                ..Default::default()
            },
        );
        let source_id = SourceId(1003);
        let mut folder = FolderSource::new(source_id, &media).unwrap();
        folder.rescan(&[]).unwrap();
        let frn = folder
            .records()
            .iter()
            .find(|r| folder.arena().get_name(r.name_offset, r.name_len) == "photo.png")
            .copied()
            .unwrap()
            .frn;
        engine
            .config
            .write()
            .unwrap()
            .folder_sources
            .push(folder.display_root().to_string());
        engine
            .folder_sources
            .write()
            .unwrap()
            .insert(source_id, folder);

        // Discard the initial full pass so this run is an incremental delta with an empty store.
        engine.property_refresh.lock().unwrap().take();
        engine
            .property_refresh
            .lock()
            .unwrap()
            .mark_records(source_id, [frn]);

        let fail_cmd = temp.path().join("fail-worker.cmd");
        std::fs::write(&fail_cmd, b"@exit /b 1\r\n").unwrap();
        let status = engine
            .run_property_extraction_with_worker(&fail_cmd, Duration::from_secs(5))
            .unwrap();
        assert_eq!(status.indexed_records, 0);
        assert_eq!(
            status.state,
            ContentIndexState::Ready,
            "an incremental all-failure delta must not block later refreshes"
        );
    }

    fn full_plan_tasks(engine: &MultiVolumeEngine) -> Vec<PropertyExtractTask> {
        engine
            .plan_property_refresh_with_scope(
                &PropertyRefresh {
                    full: true,
                    ..Default::default()
                },
                true,
            )
            .tasks
    }

    #[test]
    fn full_plan_selects_a_visible_hardlink_and_drops_a_moved_subtree() {
        let temp = tempfile::tempdir().unwrap();
        let service_dir = temp.path().join("service");
        let engine = MultiVolumeEngine::with_paths(
            &service_dir.join("index"),
            &service_dir.join("content"),
            &service_dir.join("service.toml"),
            vec![],
            ServiceConfig {
                excluded_paths: vec![r"C:\Hidden".to_string()],
                content_excluded_paths: Vec::new(),
                property_index_enabled: true,
                ..Default::default()
            },
        );

        let mut vol = crate::sync_engine::VolumeIndex::new('C');
        vol.ingest_items(vec![
            cairn_ntfs::mft::ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: "Hidden".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: 0x10,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 11,
                parent_frn: 5,
                name: "Docs".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: 0x10,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 12,
                parent_frn: 11,
                name: "album".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: 0x10,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 20,
                parent_frn: 10,
                name: "link.png".to_string(),
                is_dir: false,
                size: 128,
                created_ts: 2,
                modified_ts: 2,
                flags: 0x20,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 20,
                parent_frn: 11,
                name: "visible.png".to_string(),
                is_dir: false,
                size: 128,
                created_ts: 2,
                modified_ts: 2,
                flags: 0x20,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 30,
                parent_frn: 12,
                name: "nested.png".to_string(),
                is_dir: false,
                size: 256,
                created_ts: 2,
                modified_ts: 2,
                flags: 0x20,
                is_reparse_point: false,
            },
        ]);
        engine.add_volume(vol);

        let initial_tasks = full_plan_tasks(&engine);
        assert_eq!(initial_tasks.len(), 2);
        assert!(initial_tasks
            .iter()
            .any(|t| t.record_key == 20 && t.path.eq_ignore_ascii_case(r"C:\Docs\visible.png")));
        assert!(initial_tasks.iter().any(
            |t| t.record_key == 30 && t.path.eq_ignore_ascii_case(r"C:\Docs\album\nested.png")
        ));

        // Renaming directory `album` into excluded `Hidden` must drop its child's task eligibility.
        assert!(engine.update_volume('C', |v| {
            v.apply_usn_changes([
                cairn_ntfs::usn::UsnChange::Renamed {
                    frn: 12,
                    parent_frn: 11,
                    name: "album".to_string(),
                    is_dir: true,
                    is_new_name: false,
                },
                cairn_ntfs::usn::UsnChange::Renamed {
                    frn: 12,
                    parent_frn: 10,
                    name: "album".to_string(),
                    is_dir: true,
                    is_new_name: true,
                },
            ]);
        }));
        let after_move = full_plan_tasks(&engine);
        assert_eq!(after_move.len(), 1);
        assert_eq!(after_move[0].record_key, 20);
    }
}

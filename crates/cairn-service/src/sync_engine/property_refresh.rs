// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Coalesced property refresh requests and the handles shared with index sources.

use super::MultiVolumeEngine;
use cairn_core::property_store::{PropertySideTables, SourceId};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock, Weak};

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

    pub(super) fn retain_and_enqueue_source(&self, source: SourceId, present_keys: HashSet<u64>) {
        let queued = {
            let mut tables = self.tables.write().unwrap();
            if tables.is_enabled() {
                tables.retain_source_keys(source, &present_keys);
                self.refresh.lock().unwrap().mark_source(source);
                true
            } else {
                false
            }
        };
        if queued {
            self.wake();
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
    #[cfg(test)]
    pub(super) fn full() -> Self {
        Self {
            full: true,
            ..Self::default()
        }
    }

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

    pub(super) fn is_full(&self) -> bool {
        self.full
    }

    pub(super) fn sources(&self) -> &HashSet<SourceId> {
        &self.sources
    }

    pub(super) fn records(&self) -> &HashSet<(u32, u64)> {
        &self.records
    }

    pub(super) fn has_work(&self) -> bool {
        self.full || !self.sources.is_empty() || !self.records.is_empty()
    }

    pub(super) fn take(&mut self) -> Self {
        std::mem::take(self)
    }
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
}

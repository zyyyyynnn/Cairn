// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Bounded in-memory change journal (`IDX-13`).
//!
//! Records index changes across NTFS volumes and folder sources with strict FIFO eviction
//! bounded by both entry count (`200_000`) and heap memory (`32 MiB`). The IPC surface exposes
//! read-only queries and an enable/disable toggle without destructive clear or edit operations.

use cairn_core::property_store::SourceId;
use cairn_protocol::status::{IndexJournalEntry, IndexJournalStatus, JournalChangeKind};
use std::collections::VecDeque;
use std::mem::size_of;

pub const INDEX_JOURNAL_MAX_ENTRIES: usize = 200_000;
pub const INDEX_JOURNAL_MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_JOURNAL_QUERY_LIMIT: usize = 10_000;

fn entry_string_bytes(entry: &IndexJournalEntry) -> usize {
    entry.path.capacity() + entry.old_path.as_ref().map_or(0, String::capacity)
}

#[derive(Debug)]
pub struct IndexJournal {
    enabled: bool,
    next_seq: u64,
    entries: VecDeque<IndexJournalEntry>,
    string_bytes: usize,
}

impl Default for IndexJournal {
    fn default() -> Self {
        Self::new(true)
    }
}

impl IndexJournal {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            next_seq: 1,
            entries: VecDeque::new(),
            string_bytes: 0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Toggles journal recording. Disabling clears buffered entries to release heap memory.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.entries = VecDeque::new();
            self.string_bytes = 0;
        }
    }

    fn total_resident_bytes(&self) -> usize {
        if self.entries.is_empty() && self.entries.capacity() == 0 {
            0
        } else {
            self.entries.capacity() * size_of::<IndexJournalEntry>() + self.string_bytes
        }
    }

    /// Appends one normalized change entry when enabled, evicting oldest entries FIFO to honor
    /// [`INDEX_JOURNAL_MAX_ENTRIES`] and [`INDEX_JOURNAL_MAX_BYTES`].
    pub fn push(
        &mut self,
        source_id: SourceId,
        frn: u64,
        kind: JournalChangeKind,
        path: String,
        old_path: Option<String>,
        timestamp_ts: i64,
    ) {
        if !self.enabled {
            return;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let entry = IndexJournalEntry {
            seq,
            timestamp_ts,
            source_id,
            frn,
            kind,
            path,
            old_path,
        };
        let added_strings = entry_string_bytes(&entry);
        let min_entry_cost = size_of::<IndexJournalEntry>() + added_strings;
        if min_entry_cost > INDEX_JOURNAL_MAX_BYTES {
            return;
        }

        while self.entries.len() >= INDEX_JOURNAL_MAX_ENTRIES
            || self
                .string_bytes
                .saturating_add(added_strings)
                .saturating_add((self.entries.len() + 1) * size_of::<IndexJournalEntry>())
                > INDEX_JOURNAL_MAX_BYTES
        {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.string_bytes = self
                .string_bytes
                .saturating_sub(entry_string_bytes(&evicted));
        }
        self.string_bytes = self.string_bytes.saturating_add(added_strings);
        self.entries.push_back(entry);

        if self.total_resident_bytes() > INDEX_JOURNAL_MAX_BYTES {
            self.entries.shrink_to_fit();
            while self.total_resident_bytes() > INDEX_JOURNAL_MAX_BYTES {
                let Some(evicted) = self.entries.pop_front() else {
                    break;
                };
                self.string_bytes = self
                    .string_bytes
                    .saturating_sub(entry_string_bytes(&evicted));
                self.entries.shrink_to_fit();
            }
        }
    }

    /// Read-only cursor query returning entries strictly after `after_seq`, up to `limit`.
    pub fn query(&self, after_seq: u64, limit: usize) -> Vec<IndexJournalEntry> {
        let cap = limit.min(MAX_JOURNAL_QUERY_LIMIT);
        if cap == 0 || self.entries.is_empty() {
            return Vec::new();
        }
        let start = self.entries.partition_point(|entry| entry.seq <= after_seq);
        self.entries.iter().skip(start).take(cap).cloned().collect()
    }

    pub fn status(&self) -> IndexJournalStatus {
        IndexJournalStatus {
            enabled: self.enabled,
            entry_count: self.entries.len(),
            resident_bytes: self.total_resident_bytes(),
            max_entries: INDEX_JOURNAL_MAX_ENTRIES,
            max_bytes: INDEX_JOURNAL_MAX_BYTES,
            oldest_seq: self.entries.front().map_or(0, |e| e.seq),
            newest_seq: self.entries.back().map_or(0, |e| e.seq),
        }
    }
}

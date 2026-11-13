// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Compact FRN-to-record-index lookup and full-ledger index memory accounting.

use std::collections::HashMap;
use std::mem::size_of;

const OVERFLOW_FLAG: u32 = 1 << 31;
const OVERFLOW_MASK: u32 = !OVERFLOW_FLAG;

/// Estimates the heap allocation of a `std::collections::HashMap<K, V>` backing table.
pub(crate) fn hashmap_table_bytes<K, V>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = (capacity * 8).div_ceil(7).next_power_of_two().max(4);
    buckets * (size_of::<(K, V)>() + 1)
}

/// Compact map from 64-bit NTFS FRNs to one or more `FileRecord` indices.
///
/// Single-link files (the overwhelming majority on NTFS) store their `u32` record index inline
/// without allocating a heap `Vec`. Only multi-hardlink FRNs (or indices `>= 2^31`) spill into a
/// compact overflow table.
#[derive(Clone, Debug, Default)]
pub struct FrnLookup {
    entries: HashMap<u64, u32>,
    overflow: Vec<Vec<u32>>,
    free_slots: Vec<u32>,
}

impl FrnLookup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            overflow: Vec::new(),
            free_slots: Vec::new(),
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, frn: u64) -> bool {
        self.entries.contains_key(&frn)
    }

    /// Appends `record_idx` under `frn`, promoting single-link entries to overflow storage only
    /// when a second link (or an index reaching `2^31`) arrives.
    pub fn push(&mut self, frn: u64, record_idx: u32) {
        match self.entries.get_mut(&frn) {
            None => {
                if record_idx < OVERFLOW_FLAG {
                    self.entries.insert(frn, record_idx);
                } else {
                    let slot = Self::alloc_overflow(
                        &mut self.overflow,
                        &mut self.free_slots,
                        vec![record_idx],
                    );
                    self.entries.insert(frn, OVERFLOW_FLAG | slot);
                }
            }
            Some(existing) => {
                if *existing & OVERFLOW_FLAG == 0 {
                    let first = *existing;
                    let slot = Self::alloc_overflow(
                        &mut self.overflow,
                        &mut self.free_slots,
                        vec![first, record_idx],
                    );
                    *existing = OVERFLOW_FLAG | slot;
                } else {
                    let slot = (*existing & OVERFLOW_MASK) as usize;
                    self.overflow[slot].push(record_idx);
                }
            }
        }
    }

    /// Returns all record indices currently mapped to `frn` without allocating.
    #[inline]
    pub fn get(&self, frn: u64) -> Option<&[u32]> {
        let entry = self.entries.get(&frn)?;
        if *entry & OVERFLOW_FLAG == 0 {
            Some(std::slice::from_ref(entry))
        } else {
            let slot = (*entry & OVERFLOW_MASK) as usize;
            Some(self.overflow[slot].as_slice())
        }
    }

    /// Returns the first record index associated with `frn`, if any.
    #[inline]
    pub fn first(&self, frn: u64) -> Option<u32> {
        self.get(frn).and_then(|indices| indices.first().copied())
    }

    /// Removes `frn` and recycles its overflow slot if it used one.
    pub fn remove(&mut self, frn: u64) -> bool {
        let Some(removed) = self.entries.remove(&frn) else {
            return false;
        };
        if removed & OVERFLOW_FLAG != 0 {
            let slot = (removed & OVERFLOW_MASK) as usize;
            self.overflow[slot].clear();
            self.overflow[slot].shrink_to_fit();
            self.free_slots.push(slot as u32);
        }
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.overflow.clear();
        self.free_slots.clear();
    }

    pub fn clear_and_shrink(&mut self) {
        self.entries = HashMap::new();
        self.overflow = Vec::new();
        self.free_slots = Vec::new();
    }

    /// Number of FRNs backed by overflow storage (multi-hardlink entries).
    pub fn multi_link_count(&self) -> usize {
        self.entries
            .values()
            .filter(|&&v| v & OVERFLOW_FLAG != 0)
            .count()
    }

    /// Estimated heap bytes held by the hash table and multi-hardlink overflow buffers.
    pub fn allocated_bytes(&self) -> usize {
        let map_bytes = hashmap_table_bytes::<u64, u32>(self.entries.capacity());
        let overflow_table = self.overflow.capacity() * size_of::<Vec<u32>>();
        let overflow_items: usize = self
            .overflow
            .iter()
            .map(|v| v.capacity() * size_of::<u32>())
            .sum();
        let free_bytes = self.free_slots.capacity() * size_of::<u32>();
        map_bytes + overflow_table + overflow_items + free_bytes
    }

    fn alloc_overflow(
        overflow: &mut Vec<Vec<u32>>,
        free_slots: &mut Vec<u32>,
        indices: Vec<u32>,
    ) -> u32 {
        if let Some(slot) = free_slots.pop() {
            overflow[slot as usize] = indices;
            slot
        } else {
            let slot = overflow.len() as u32;
            debug_assert!(
                slot < OVERFLOW_FLAG,
                "overflow slot index must fit within 31 bits"
            );
            overflow.push(indices);
            slot
        }
    }
}

/// Resident heap-allocation ledger of a volume or multi-volume index. Transient build
/// buffers (sort entries, per-request directory indexes) are deliberately excluded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexMemoryFootprint {
    pub record_count: usize,
    pub live_count: usize,
    pub records_bytes: usize,
    pub arena_bytes: usize,
    pub folded_arena_bytes: usize,
    pub frn_lookup_bytes: usize,
    pub presorted_bytes: usize,
    pub directory_sizes_bytes: usize,
    pub path_cache_bytes: usize,
    pub scratch_bytes: usize,
}

impl IndexMemoryFootprint {
    /// Total tracked heap bytes across all components.
    pub fn total_bytes(&self) -> usize {
        self.records_bytes
            + self.arena_bytes
            + self.folded_arena_bytes
            + self.frn_lookup_bytes
            + self.presorted_bytes
            + self.directory_sizes_bytes
            + self.path_cache_bytes
            + self.scratch_bytes
    }

    /// Average tracked bytes per indexed record (returns `0.0` when empty).
    pub fn bytes_per_record(&self) -> f64 {
        if self.record_count == 0 {
            0.0
        } else {
            self.total_bytes() as f64 / self.record_count as f64
        }
    }

    /// Accumulates `other` into `self` for multi-volume rollups.
    pub fn accumulate(&mut self, other: &Self) {
        self.record_count += other.record_count;
        self.live_count += other.live_count;
        self.records_bytes += other.records_bytes;
        self.arena_bytes += other.arena_bytes;
        self.folded_arena_bytes += other.folded_arena_bytes;
        self.frn_lookup_bytes += other.frn_lookup_bytes;
        self.presorted_bytes += other.presorted_bytes;
        self.directory_sizes_bytes += other.directory_sizes_bytes;
        self.path_cache_bytes += other.path_cache_bytes;
        self.scratch_bytes += other.scratch_bytes;
    }
}

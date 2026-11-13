// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use cairn_core::frn_index::{FrnLookup, IndexMemoryFootprint};
use cairn_core::{DirectoryPathCache, FileRecord, NamePool, PresortedIndices};

#[test]
fn single_link_files_stay_inline_and_promote_only_multi_hardlinks() {
    let mut lookup = FrnLookup::with_capacity(10_000);
    for i in 0..10_000u32 {
        lookup.push(100 + u64::from(i), i);
    }
    assert_eq!(lookup.len(), 10_000);
    assert_eq!(lookup.multi_link_count(), 0);
    assert_eq!(lookup.get(142), Some(&[42][..]));
    assert_eq!(lookup.first(142), Some(42));
    assert_eq!(lookup.get(999_999), None);

    // Promote one FRN to a second hardlink and verify overflow isolation.
    lookup.push(142, 10_000);
    assert_eq!(lookup.multi_link_count(), 1);
    assert_eq!(lookup.get(142), Some(&[42, 10_000][..]));
    assert_eq!(lookup.get(143), Some(&[43][..]));

    // Large >= 2^31 index sentinel must also stay unambiguous if reached.
    lookup.push(555_555, (1u32 << 31) + 7);
    assert_eq!(lookup.get(555_555), Some(&[(1u32 << 31) + 7][..]));
}

#[test]
fn memory_footprint_tracks_real_allocations_within_compact_budget() {
    let count = 10_000usize;
    let mut records = Vec::with_capacity(count);
    let mut pool = NamePool::with_capacity(count * 16);
    let mut lookup = FrnLookup::with_capacity(count);

    for i in 0..count {
        let name = format!("document_{i:05}.txt");
        let (off, len, ext) = pool.insert(&name);
        records.push(FileRecord {
            frn: 1000 + i as u64,
            parent_idx: FileRecord::ROOT_PARENT,
            name_offset: off,
            name_len: len,
            ext_offset: ext,
            volume_id: 2,
            flags: 0,
            size: 1024,
            modified_ts: 1,
            created_ts: 1,
        });
        lookup.push(1000 + i as u64, i as u32);
    }

    let presorted = PresortedIndices::default();
    let path_cache = DirectoryPathCache::new();
    let (arena_bytes, folded_arena_bytes) = pool.allocated_bytes();
    let footprint = IndexMemoryFootprint {
        record_count: records.len(),
        live_count: records.len(),
        records_bytes: records.capacity() * std::mem::size_of::<FileRecord>(),
        arena_bytes,
        folded_arena_bytes,
        frn_lookup_bytes: lookup.allocated_bytes(),
        presorted_bytes: presorted.allocated_bytes(),
        directory_sizes_bytes: 0,
        path_cache_bytes: path_cache.allocated_bytes(),
        scratch_bytes: 0,
    };

    // Compact FrnLookup (~17B/bucket, 0 per-entry Vec allocs) plus FileRecord (48B) + dual NamePool (~32B)
    // stays well below 125 B/record (previous HashMap<u64, Vec<u32>> + eager 4x presorted was >170 B/rec).
    assert!(
        footprint.bytes_per_record() < 125.0,
        "expected compact resident footprint < 125 B/record, got {:.2} B/record ({:?})",
        footprint.bytes_per_record(),
        footprint
    );
    assert_eq!(footprint.presorted_bytes, 0);
}

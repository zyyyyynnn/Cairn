// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use cairn_core::frn_index::{FrnLookup, IndexMemoryFootprint};

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
fn the_footprint_arithmetic_keeps_its_components_independent() {
    let footprint = IndexMemoryFootprint {
        record_count: 10_000,
        live_count: 9_996,
        records_bytes: 480_000,
        arena_bytes: 100_000,
        folded_arena_bytes: 100_000,
        frn_lookup_bytes: 170_000,
        presorted_bytes: 40_000,
        directory_sizes_bytes: 0,
        path_cache_bytes: 1_024,
        scratch_bytes: 8_192,
    };
    assert_eq!(footprint.total_bytes(), 899_216);
    assert!((footprint.bytes_per_record() - 89.9216).abs() < 1e-9);
    assert_eq!(IndexMemoryFootprint::default().bytes_per_record(), 0.0);
}

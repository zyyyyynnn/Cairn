// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::VolumeIndex;
use cairn_ntfs::mft::ExtractedItem;

mod content_query;
mod engine;
mod search;
mod volume_index;

fn index_item(frn: u64, parent_frn: u64, name: &str, is_dir: bool) -> ExtractedItem {
    ExtractedItem {
        frn,
        parent_frn,
        name: name.to_string(),
        is_dir,
        size: if is_dir { 0 } else { 16 },
        created_ts: 1,
        modified_ts: 1,
        flags: if is_dir {
            cairn_core::FLAG_DIRECTORY
        } else {
            0
        },
        is_reparse_point: false,
    }
}

fn sized_item(frn: u64, parent_frn: u64, name: &str, is_dir: bool, size: u64) -> ExtractedItem {
    let mut item = index_item(frn, parent_frn, name, is_dir);
    item.size = size;
    item
}

fn record_index(vol: &VolumeIndex, frn: u64) -> u32 {
    vol.first_record_index_for_frn(frn)
        .expect("fixture record must exist")
}

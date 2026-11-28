// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::backfill::ContentRules;
use super::status::unix_now;
use super::worker_process::worker_exit_is_transient;
use super::*;
use crate::config::ServiceConfig;
use crate::sync_engine::search::MatchSet;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};
use cairn_content::{CONTENT_RESULT_TOP_K, ContentIndex, ContentWriter};
use cairn_core::engine::CancellationToken;
use cairn_core::property_store::SourceId;
use cairn_core::sort::{SortColumn, SortDirection, SortSpec};
use cairn_core::syntax::QueryDefaults;
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::{FILE_ATTRIBUTE_REPARSE_POINT, UsnChange};
use cairn_protocol::status::{ContentIndexState, ContentIndexTask};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub(super) fn sort(column: SortColumn, asc: bool) -> SortSpec {
    let dir = if asc {
        SortDirection::Ascending
    } else {
        SortDirection::Descending
    };
    SortSpec::single(column, dir)
}

pub(super) fn volume_with_files(files: &[(&[&str], u16)]) -> (VolumeIndex, Vec<u64>) {
    let mut items = Vec::new();
    let mut directories: HashMap<String, u64> = HashMap::new();
    let mut next_frn = 1u64;
    let mut file_frns = Vec::new();
    for (components, flags) in files {
        let mut parent_frn = 0u64;
        for depth in 1..components.len() {
            let key = components[..depth].join("\\").to_ascii_lowercase();
            if let Some(&frn) = directories.get(&key) {
                parent_frn = frn;
                continue;
            }
            let frn = next_frn;
            next_frn += 1;
            items.push(ExtractedItem {
                frn,
                parent_frn,
                name: components[depth - 1].to_string(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            });
            directories.insert(key, frn);
            parent_frn = frn;
        }
        let frn = next_frn;
        next_frn += 1;
        items.push(ExtractedItem {
            frn,
            parent_frn,
            name: components.last().expect("file name").to_string(),
            is_dir: false,
            size: 32,
            created_ts: 1,
            modified_ts: 1,
            flags: *flags,
            is_reparse_point: *flags & cairn_core::FLAG_REPARSE_POINT != 0,
        });
        file_frns.push(frn);
    }
    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(items);
    (volume, file_frns)
}

pub(super) fn content_config() -> ServiceConfig {
    ServiceConfig {
        content_index_extensions: ["json", "toml", "log", "txt"]
            .into_iter()
            .map(String::from)
            .collect(),
        content_index_max_bytes: 1024,
        content_excluded_paths: Vec::new(),
        ..Default::default()
    }
}

mod backfill;
mod lifecycle;
mod query;

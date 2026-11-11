// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One exclusion view for a query or a content batch.

use super::VolumeIndex;
use crate::exclusion::{ExclusionPath, ExclusionRules};
use std::collections::HashMap;

/// The configured path exclusions of one operation, read once. Segment rules are answered from a
/// per-volume record mask; rule forms the mask cannot express read the record's full path through
/// the same parsed rule set. Neither route re-parses the configuration per record.
pub(super) struct PathExclusions {
    masks: Option<HashMap<char, Vec<bool>>>,
    rules: ExclusionRules,
}

impl PathExclusions {
    pub(super) fn new(volumes: &HashMap<char, VolumeIndex>, excluded_paths: &[String]) -> Self {
        let rules = ExclusionRules::parse(excluded_paths);
        let masks = if rules.is_empty() {
            None
        } else {
            rules.single_segments().map(|segments| {
                volumes
                    .iter()
                    .map(|(drive, volume)| (*drive, volume.excluded_record_mask(&segments)))
                    .collect()
            })
        };
        Self { masks, rules }
    }

    /// Whether one record of `volume` falls under the rules.
    pub(super) fn excludes(&self, volume: &VolumeIndex, index: u32) -> bool {
        if let Some(masks) = &self.masks {
            return masks
                .get(&volume.drive_letter)
                .is_some_and(|mask| mask.get(index as usize).copied().unwrap_or(false));
        }
        if self.rules.is_empty() {
            return false;
        }
        let path = volume.path_cache.get_full_path(
            &volume.records,
            &volume.arena,
            volume.drive_letter,
            index,
        );
        self.rules.excludes(&ExclusionPath::new(&path))
    }
}

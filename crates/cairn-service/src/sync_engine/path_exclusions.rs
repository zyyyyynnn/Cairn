// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One exclusion view for a query or a content batch.

use super::volume_index::SUBTREE_EXCLUDED;
use super::VolumeIndex;
use crate::exclusion::{ExclusionPath, ExclusionRules};
use std::collections::HashMap;
use std::sync::Arc;

/// The configured path exclusions of one operation, read once. Segment rules are answered from a
/// per-volume subtree state table; rule forms the table cannot express read the record's full path
/// through the same parsed rule set. Neither route re-parses the configuration per record.
pub(crate) struct PathExclusions {
    states: Option<HashMap<char, Arc<Vec<u8>>>>,
    rules: ExclusionRules,
}

impl PathExclusions {
    pub(crate) fn new(volumes: &HashMap<char, VolumeIndex>, excluded_paths: &[String]) -> Self {
        let rules = ExclusionRules::parse(excluded_paths);
        let states = if rules.is_empty() {
            None
        } else {
            rules.single_segments().map(|segments| {
                volumes
                    .iter()
                    .map(|(drive, volume)| {
                        (*drive, volume.segment_excluded_states_cached(&segments))
                    })
                    .collect()
            })
        };
        let full_path_rules = !rules.is_empty() && states.is_none();
        for volume in volumes.values() {
            volume.set_full_path_exclusion_rules(full_path_rules);
        }
        Self { states, rules }
    }

    /// Whether the configuration excludes anything at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether every rule is answered by the per-volume segment table. Such rules exclude
    /// whole subtrees, so a visible record always has visible ancestors.
    pub(crate) fn uses_segment_table(&self) -> bool {
        self.states.is_some()
    }

    /// Whether one record of `volume` falls under the rules.
    pub(crate) fn excludes(&self, volume: &VolumeIndex, index: u32) -> bool {
        if let Some(states) = &self.states {
            return states
                .get(&volume.drive_letter)
                .is_some_and(|table| table.get(index as usize).copied() == Some(SUBTREE_EXCLUDED));
        }
        if self.rules.is_empty() {
            return false;
        }
        let path = volume.path_cache().get_full_path(
            volume.records(),
            volume.arena(),
            volume.drive_letter,
            index,
        );
        self.rules.excludes(&ExclusionPath::new(&path))
    }
}

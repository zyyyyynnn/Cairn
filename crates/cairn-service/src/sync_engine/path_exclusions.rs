// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One exclusion view for a query or a content batch.

use super::VolumeIndex;
use super::volume_index::SUBTREE_EXCLUDED;
use crate::exclusion::{ExclusionPath, ExclusionRules};
use cairn_core::SourceView;
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
                        (
                            drive.to_ascii_uppercase(),
                            volume.segment_excluded_states_cached(&segments),
                        )
                    })
                    .collect()
            })
        };
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

    /// Segment-exclusion lookup table for `drive_letter` when segment-table filtering is active.
    pub(crate) fn drive_states(&self, drive_letter: char) -> Option<&[u8]> {
        self.states
            .as_ref()
            .and_then(|states| states.get(&drive_letter.to_ascii_uppercase()))
            .map(|table| table.as_slice())
    }

    /// Whether one record of `view` falls under the configured path exclusion rules.
    pub(crate) fn excludes(&self, view: &SourceView<'_>, index: u32) -> bool {
        if let Some(states) = &self.states
            && let Some(drive) = view.source_id.drive_letter()
        {
            return states
                .get(&drive)
                .is_some_and(|table| table.get(index as usize).copied() == Some(SUBTREE_EXCLUDED));
        }
        if self.rules.is_empty() {
            return false;
        }
        let path = view.path_cache.get_full_path_with_root(
            view.records,
            view.arena,
            view.root_prefix,
            index,
        );
        self.rules.excludes(&ExclusionPath::new(&path))
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Status model: the result count the bottom band renders once its query has settled, plus the
//! volume state the sidebar renders beside each drive (UI-01, IDX-07).
//!
//! Volume state arrives as a machine-readable [`VolumeState`] from the protocol. This module turns it
//! into user-facing text through Fluent only; no surface renders a protocol key as a label (NFR-11).

use crate::shell::i18n::I18nManager;
use cairn_core::format_size_bytes;
use cairn_protocol::status::{VolumeState, VolumeStatusSummary};
use fluent_bundle::FluentArgs;

#[derive(Default)]
pub struct StatusBarModel {
    /// The last completed result-set total. It is not a running/partial figure: the status band
    /// renders it only after the query and any index build have settled.
    objects_count: usize,
    /// Sum of matched files' own sizes, from the same completed query.
    objects_bytes: u64,
    summary_known: bool,
    pub volumes: Vec<VolumeStatusSummary>,
}

/// Fluent message id for a volume state, derived from the protocol's stable `state_key()` so a new
/// state cannot silently render as untranslated text.
pub fn volume_state_message_key(state: &VolumeState) -> String {
    format!("volume_state_{}", state.state_key())
}

impl StatusBarModel {
    /// Ingests updated volume status envelopes from the service and returns the live indexed count.
    ///
    /// Volume status never seeds the result count: the table's count belongs to its query result
    /// set, not to the raw volume total. The exact query total is the only value the band shows.
    pub fn update_volumes(&mut self, volumes: Vec<VolumeStatusSummary>) -> usize {
        self.volumes = volumes;
        self.total_indexed()
    }

    /// Sum of indexed objects across all currently tracked volumes, saturated on overflow.
    pub fn total_indexed(&self) -> usize {
        self.volumes.iter().fold(0usize, |acc, v| {
            acc.saturating_add(v.indexed_count as usize)
        })
    }

    /// Records a completed query's exact count and matched-file byte total. While a new result
    /// set is loading, the caller clears this first so the band never presents stale values.
    pub fn set_objects_summary(&mut self, count: usize, bytes: u64) {
        self.objects_count = count;
        self.objects_bytes = bytes;
        self.summary_known = true;
    }

    /// Marks the current result summary unknown, for a new result set or a failed query.
    pub fn clear_objects_count(&mut self) {
        self.objects_count = 0;
        self.objects_bytes = 0;
        self.summary_known = false;
    }

    pub fn has_objects_summary(&self) -> bool {
        self.summary_known
    }

    pub fn objects_bytes(&self) -> u64 {
        self.objects_bytes
    }

    #[cfg(test)]
    pub fn objects_count(&self) -> usize {
        self.objects_count
    }

    /// `true` while at least one volume is still building or catching up. This is a loading state,
    /// not a warning: the table renders its skeleton until the index has data to show.
    pub fn is_any_loading(&self) -> bool {
        self.volumes.iter().any(|volume| {
            matches!(
                volume.status,
                VolumeState::CatchingUp(_) | VolumeState::Rebuilding
            )
        })
    }

    /// Localized summary for a completed result set: item count plus matched-file bytes.
    pub fn objects_summary_text(&self, i18n: &I18nManager) -> String {
        let mut args = FluentArgs::new();
        args.set("count", self.objects_count);
        if self.objects_count == 0 {
            i18n.get_text_with_args("status_objects_count", &args)
        } else {
            args.set("size", format_size_bytes(self.objects_bytes));
            i18n.get_text_with_args("status_objects_summary", &args)
        }
    }

    /// Single index-state capsule: the drive letter is data, the state is localized copy.
    pub fn volume_capsule_text(&self, i18n: &I18nManager, volume: &VolumeStatusSummary) -> String {
        let state_text = match volume.status.progress_percent() {
            // The service's per-batch percentage is not a reliable cumulative progress figure, so
            // the capsule names the state instead of publishing a number that can look frozen.
            Some(_) => i18n.get_text("volume_state_catching_up_pending"),
            None => i18n.get_text(&volume_state_message_key(&volume.status)),
        };

        let mut args = FluentArgs::new();
        args.set("drive", volume.drive_letter.to_string());
        args.set("state", state_text);
        i18n.get_text_with_args("volume_capsule", &args)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;
    use crate::shell::i18n::I18nManager;

    fn volume(status: VolumeState) -> VolumeStatusSummary {
        volume_with_count(status, 1)
    }

    fn volume_with_count(status: VolumeState, indexed_count: u64) -> VolumeStatusSummary {
        VolumeStatusSummary {
            drive_letter: 'C',
            volume_guid: "guid".to_string(),
            status,
            indexed_count,
            usn_enabled: true,
        }
    }

    #[test]
    fn every_protocol_state_resolves_to_text_in_both_locales() {
        let i18n = I18nManager::default();
        let states = [
            VolumeState::Synced,
            VolumeState::CatchingUp(42),
            VolumeState::Rebuilding,
            VolumeState::Offline,
            VolumeState::SnapshotMode,
        ];

        for state in states {
            let key = volume_state_message_key(&state);
            assert!(
                i18n.has_key(&key),
                "{key} missing from the locale bundles for {state:?}"
            );
            let text = StatusBarModel::default().volume_capsule_text(&i18n, &volume(state));
            assert!(
                !text.contains(state.state_key()),
                "{state:?} rendered the raw protocol key: {text}"
            );
            assert!(
                text.contains('C'),
                "{state:?} capsule lost the drive letter: {text}"
            );
        }
    }

    #[test]
    fn catching_up_names_the_state_without_a_frozen_percentage() {
        let i18n = I18nManager::default();
        let text = StatusBarModel::default()
            .volume_capsule_text(&i18n, &volume(VolumeState::CatchingUp(42)));
        assert!(
            text.contains("同步中") || text.contains("Synchronizing"),
            "the capsule must name the state: {text}"
        );
        assert!(
            !text.contains("42"),
            "a per-batch percentage is not a cumulative claim and must not be published"
        );
    }

    #[test]
    fn the_capsule_text_differs_between_states() {
        let i18n = I18nManager::default();
        let model = StatusBarModel::default();
        let synced = model.volume_capsule_text(&i18n, &volume(VolumeState::Synced));
        let offline = model.volume_capsule_text(&i18n, &volume(VolumeState::Offline));
        assert_ne!(synced, offline, "states must not read as the same label");
    }

    #[test]
    fn only_building_or_catching_up_volumes_count_as_loading() {
        let mut model = StatusBarModel {
            volumes: vec![volume(VolumeState::Synced)],
            ..Default::default()
        };
        assert!(
            !model.is_any_loading(),
            "a synced index is idle, not loading"
        );

        model.volumes.push(volume(VolumeState::CatchingUp(0)));
        assert!(model.is_any_loading());

        model.volumes = vec![volume(VolumeState::Offline)];
        assert!(
            !model.is_any_loading(),
            "offline is a failure state, not a loading one"
        );
    }

    #[test]
    fn a_completed_count_is_rendered_only_after_it_is_known() {
        let i18n = I18nManager::default();
        let mut model = StatusBarModel::default();
        assert!(!model.has_objects_summary());

        model.set_objects_summary(0, 0);
        assert!(model.has_objects_summary());
        let zero = model.objects_summary_text(&i18n);
        assert!(zero.contains('0'), "{zero}");

        model.set_objects_summary(1_409_944, 1_500_000_000);
        let text = model.objects_summary_text(&i18n);
        assert!(
            text.contains("1409944") || text.contains("1,409,944"),
            "{text}"
        );
        assert!(text.contains("GB"), "{text}");

        model.clear_objects_count();
        assert!(!model.has_objects_summary());
        assert_eq!(model.objects_count(), 0);
    }

    #[test]
    fn update_volumes_never_seeds_the_result_count() {
        let mut model = StatusBarModel::default();
        assert!(!model.has_objects_summary());

        let vol_c = VolumeStatusSummary {
            drive_letter: 'C',
            volume_guid: "guid-c".to_string(),
            status: VolumeState::Synced,
            indexed_count: 50_000,
            usn_enabled: true,
        };
        let vol_d = VolumeStatusSummary {
            drive_letter: 'D',
            volume_guid: "guid-d".to_string(),
            status: VolumeState::CatchingUp(10),
            indexed_count: 25_000,
            usn_enabled: true,
        };

        let total = model.update_volumes(vec![vol_c.clone(), vol_d.clone()]);
        assert_eq!(total, 75_000);
        assert!(!model.has_objects_summary());
        assert_eq!(model.objects_count(), 0);

        model.set_objects_summary(12, 12_000);
        assert!(model.has_objects_summary());

        let mut vol_d_updated = vol_d;
        vol_d_updated.indexed_count = 26_000;
        let total_updated = model.update_volumes(vec![vol_c, vol_d_updated]);
        assert_eq!(total_updated, 76_000);
        assert_eq!(model.objects_count(), 12);
    }

    #[test]
    fn a_zero_match_result_is_known_and_distinct_from_unknown() {
        let i18n = I18nManager::default();
        let mut model = StatusBarModel::default();
        let vol = volume_with_count(VolumeState::Synced, 50_000);
        let total = model.update_volumes(vec![vol]);
        assert_eq!(total, 50_000);
        assert!(!model.has_objects_summary());

        model.set_objects_summary(0, 0);
        assert!(model.has_objects_summary());
        let text = model.objects_summary_text(&i18n);
        assert!(text.contains('0'), "{text}");
    }

    #[test]
    fn update_volumes_handles_empty_or_zero_counts() {
        let mut model = StatusBarModel::default();
        let total = model.update_volumes(Vec::new());
        assert_eq!(total, 0);
        assert_eq!(model.objects_count(), 0);
        assert!(!model.has_objects_summary());

        let total = model.update_volumes(vec![volume_with_count(VolumeState::Synced, 0)]);
        assert_eq!(total, 0);
        assert_eq!(model.objects_count(), 0);
        assert!(!model.has_objects_summary());
    }

    #[test]
    fn update_volumes_handles_hundreds_of_concurrent_loading_volumes() {
        let mut model = StatusBarModel::default();
        let mut volumes = Vec::with_capacity(120);
        for i in 0..120 {
            let letter = (b'A' + (i % 26) as u8) as char;
            let status = if i % 2 == 0 {
                VolumeState::Rebuilding
            } else {
                VolumeState::CatchingUp((i % 100) as u8)
            };
            volumes.push(VolumeStatusSummary {
                drive_letter: letter,
                volume_guid: format!("guid-{i}"),
                status,
                indexed_count: 10_000,
                usn_enabled: true,
            });
        }

        let total = model.update_volumes(volumes);
        assert_eq!(total, 1_200_000);
        assert_eq!(model.objects_count(), 0);
        assert!(!model.has_objects_summary());
        assert!(model.is_any_loading());
    }
}

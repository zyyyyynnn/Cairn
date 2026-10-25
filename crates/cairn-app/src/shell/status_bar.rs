// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Status model: the result count the bottom band renders, plus the volume state the sidebar
//! renders beside each drive (UI-01, IDX-07).
//!
//! Volume state arrives as a machine-readable [`VolumeState`] from the protocol. This module turns it
//! into user-facing text through Fluent only; no surface renders a protocol key as a label (NFR-11).

use crate::shell::i18n::I18nManager;
use cairn_protocol::status::{VolumeState, VolumeStatusSummary};
use fluent_bundle::FluentArgs;

#[derive(Default)]
pub struct StatusBarModel {
    pub total_objects: usize,
    pub volumes: Vec<VolumeStatusSummary>,
}

/// Fluent message id for a volume state, derived from the protocol's stable `state_key()` so a new
/// state cannot silently render as untranslated text.
pub fn volume_state_message_key(state: &VolumeState) -> String {
    format!("volume_state_{}", state.state_key())
}

impl StatusBarModel {
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

    pub fn objects_count_text(&self, i18n: &I18nManager) -> String {
        let mut args = FluentArgs::new();
        args.set("count", self.total_objects);
        i18n.get_text_with_args("status_objects_count", &args)
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
        VolumeStatusSummary {
            drive_letter: 'C',
            volume_guid: "guid".to_string(),
            status,
            indexed_count: 1,
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
    fn the_summary_is_the_number_of_objects() {
        let i18n = I18nManager::default();
        let model = StatusBarModel {
            total_objects: 120,
            ..Default::default()
        };
        let text = model.objects_count_text(&i18n);
        assert!(text.contains("120"), "{text}");
    }
}

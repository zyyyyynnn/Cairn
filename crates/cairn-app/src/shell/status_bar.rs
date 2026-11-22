// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Status model: the result count the bottom band renders once its query has settled, plus the
//! volume states that decide when the index is still loading (UI-01, IDX-07).

use crate::shell::i18n::I18nManager;
use cairn_core::format_size_bytes;
use cairn_protocol::status::{VolumeState, VolumeStatusSummary};
use fluent_bundle::FluentArgs;

#[derive(Default)]
pub struct StatusBarModel {
    /// The last completed result-set total. It is not a running/partial figure: the status band
    /// renders it only after the query and any index build have settled.
    objects_count: usize,
    /// Sum of matched entries counted once at each highest hit point, from the same completed query.
    objects_bytes: u64,
    summary_known: bool,
    pub volumes: Vec<VolumeStatusSummary>,
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

    /// Records a completed query's exact count and highest-hit byte total. While a new result
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
    /// not a warning.
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
        args.set("count_text", format_count(self.objects_count));
        if self.objects_count == 0 || self.objects_bytes == 0 {
            i18n.get_text_with_args("status_objects_count", &args)
        } else {
            args.set("size", format_size_bytes(self.objects_bytes));
            i18n.get_text_with_args("status_objects_summary", &args)
        }
    }
}

/// Groups integer digits so million-scale counts remain scannable in both locales.
pub(crate) fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
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
    fn million_scale_counts_are_grouped() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1_437_236), "1,437,236");
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
    fn folder_results_without_file_bytes_omit_zero_byte_display() {
        let zh = I18nManager::new("zh-CN");
        let en = I18nManager::new("en-US");
        let mut model = StatusBarModel::default();
        model.set_objects_summary(206_211, 0);

        let zh_text = model.objects_summary_text(&zh);
        assert!(zh_text.contains("206,211"), "{zh_text}");
        assert!(zh_text.contains("项目"), "{zh_text}");
        assert!(
            !zh_text.contains("0 B") && !zh_text.contains("0B"),
            "{zh_text}"
        );

        let en_text = model.objects_summary_text(&en);
        assert!(en_text.contains("206,211"), "{en_text}");
        assert!(en_text.contains("items"), "{en_text}");
        assert!(
            !en_text.contains("0 B") && !en_text.contains("0B"),
            "{en_text}"
        );
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

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `cairn-ntfs`: Direct NTFS $MFT parsing, USN Journal monitoring, and volume operations.
#![allow(clippy::chunks_exact_to_as_chunks)]

pub mod attribute;
pub mod boot_sector;
pub mod file_id;
pub mod fixup;
pub mod mft;
pub mod runlist;
pub mod usn;
pub mod volume;

pub use attribute::{parse_attributes, ParsedAttributes, ParsedFileName};
pub use boot_sector::NtfsBootSector;
pub use file_id::directory_file_id;
pub use fixup::{apply_fixup, RECORD_FLAG_IN_USE, RECORD_FLAG_IS_DIR};
pub use mft::{parse_mft_record, ExtractedItem, MftError};
pub use runlist::{parse_runlist, DataRun};
pub use usn::{parse_usn_records, ReadUsnJournalDataV0, UsnChange, UsnJournalData};
pub use volume::{
    enumerate_fixed_drives, read_offline_mft_file, scan_live_volume_mft, VolumeError,
};
#[cfg(windows)]
pub use volume::{locate_mft_clusters, RawVolumeHandle};

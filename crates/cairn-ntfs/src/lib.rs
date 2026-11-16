// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `cairn-ntfs`: Direct NTFS $MFT parsing, USN Journal monitoring, and volume operations.

pub mod attribute;
pub mod boot_sector;
pub mod file_id;
pub mod fixup;
pub mod mft;
pub mod mft_reader;
pub mod runlist;
pub mod usn;
pub mod volume;
pub mod volume_identity;

pub use attribute::{parse_attributes, NonResidentAttrSpan, ParsedAttributes, ParsedFileName};
pub use boot_sector::NtfsBootSector;
pub use file_id::directory_file_id;
pub use fixup::{apply_fixup, RECORD_FLAG_IN_USE, RECORD_FLAG_IS_DIR};
pub use mft::{parse_mft_record, ExtractedItem, MftError};
pub use mft_reader::{
    locate_mft_clusters_from_reader, scan_mft_from_file, scan_mft_from_reader, BlockReader,
    FileBlockReader, MftScanOptions, MftScanResult, MftScanStats, SliceBlockReader,
};
pub use runlist::{parse_runlist, DataRun};
pub use usn::{parse_usn_records, ReadUsnJournalDataV0, UsnChange, UsnJournalData};
pub use volume::{enumerate_fixed_drives, scan_live_volume_mft, VolumeError};
#[cfg(windows)]
pub use volume::{locate_mft_clusters, RawVolumeHandle};
pub use volume_identity::{enumerate_volumes, parse_volume_path_names, VolumeIdentity};

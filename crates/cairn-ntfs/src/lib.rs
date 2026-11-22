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

pub use attribute::{NonResidentAttrSpan, ParsedAttributes, ParsedFileName, parse_attributes};
pub use boot_sector::NtfsBootSector;
pub use file_id::directory_file_id;
pub use fixup::{RECORD_FLAG_IN_USE, RECORD_FLAG_IS_DIR, apply_fixup};
pub use mft::{ExtractedItem, MftError, parse_mft_record};
pub use mft_reader::{
    BlockReader, FileBlockReader, MftScanOptions, MftScanResult, MftScanStats, SliceBlockReader,
    scan_mft_from_file, scan_mft_from_reader,
};
pub use runlist::{DataRun, parse_runlist};
pub use usn::{ReadUsnJournalDataV0, UsnChange, UsnJournalData, parse_usn_records};
#[cfg(windows)]
pub use volume::RawVolumeHandle;
pub use volume::{VolumeError, enumerate_fixed_drives, scan_live_volume_mft};
pub use volume_identity::{VolumeIdentity, enumerate_volumes, parse_volume_path_names};

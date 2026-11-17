// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Sparse optional side tables for high-frequency extended file properties (`IDX-12`).
//!
//! When property indexing is disabled, [`PropertySideTables`] holds zero heap allocations (`0 B`).
//! When enabled, records with extracted properties are indexed by `(SourceId, record_key)` with
//! string values packed into a contiguous UTF-8 byte arena.

use crate::frn_index::hashmap_table_bytes;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::mem::size_of;

const HAS_LENGTH: u8 = 1 << 0;
const HAS_AUTHOR: u8 = 1 << 1;
const HAS_TAGS: u8 = 1 << 2;
const HAS_DIMENSIONS: u8 = 1 << 3;
const HAS_FRAMERATE: u8 = 1 << 4;

/// First numeric [`SourceId`] assigned to indexed folder sources; `0..=25` identify NTFS drives.
pub const FOLDER_SOURCE_ID_BASE: u32 = 1_000;

/// Stable numeric identifier for an indexed source (an NTFS volume or indexed folder source).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SourceId(pub u32);

impl SourceId {
    /// Returns the deterministic identifier for an ASCII NTFS drive letter (`A` -> `0`, `C` -> `2`).
    pub const fn for_drive(drive_letter: char) -> Option<Self> {
        let upper = drive_letter.to_ascii_uppercase();
        if upper.is_ascii_uppercase() {
            Some(Self((upper as u8 - b'A') as u32))
        } else {
            None
        }
    }

    /// Returns the NTFS drive letter if this identifier lies in the `0..=25` drive range.
    pub const fn drive_letter(self) -> Option<char> {
        if self.0 < 26 {
            Some((b'A' + self.0 as u8) as char)
        } else {
            None
        }
    }
}

/// Extended attribute categories covered by [`PropertySideTables`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PropertyKind {
    Length,
    Author,
    Tags,
    Dimensions,
    Framerate,
}

impl PropertyKind {
    pub const ALL: [Self; 5] = [
        Self::Length,
        Self::Author,
        Self::Tags,
        Self::Dimensions,
        Self::Framerate,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Length => "length",
            Self::Author => "author",
            Self::Tags => "tags",
            Self::Dimensions => "dimensions",
            Self::Framerate => "framerate",
        }
    }
}

/// Owned property payload produced by extractors before insertion into [`PropertySideTables`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordPropertyData {
    pub length_ms: Option<u64>,
    pub author: Option<String>,
    pub tags: Option<String>,
    pub dimensions: Option<(u32, u32)>,
    /// Framerate scaled by 1000 (`milli-fps`, e.g. `29.97 fps -> 29_970`).
    pub framerate_mfps: Option<u32>,
}

impl RecordPropertyData {
    pub fn is_empty(&self) -> bool {
        self.length_ms.is_none()
            && self.author.as_deref().is_none_or(str::is_empty)
            && self.tags.as_deref().is_none_or(str::is_empty)
            && self.dimensions.is_none()
            && self.framerate_mfps.is_none()
    }
}

/// Zero-copy borrowed property view returned by [`PropertySideTables::get`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordPropertyView<'a> {
    pub length_ms: Option<u64>,
    pub author: Option<&'a str>,
    pub tags: Option<&'a str>,
    pub dimensions: Option<(u32, u32)>,
    pub framerate_mfps: Option<u32>,
}

impl<'a> RecordPropertyView<'a> {
    pub fn to_owned(self) -> RecordPropertyData {
        RecordPropertyData {
            length_ms: self.length_ms,
            author: self.author.map(str::to_owned),
            tags: self.tags.map(str::to_owned),
            dimensions: self.dimensions,
            framerate_mfps: self.framerate_mfps,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PackedPropertySlot {
    length_ms: u64,
    width: u32,
    height: u32,
    framerate_mfps: u32,
    author_off: u32,
    tags_off: u32,
    author_len: u16,
    tags_len: u16,
    presence: u8,
    /// Record modification time (Unix seconds) captured when the properties were extracted.
    modified_ts: u32,
}

fn stamp_u32(timestamp: i64) -> u32 {
    timestamp.clamp(0, u32::MAX as i64) as u32
}

/// Sparse side-table storage for `length`, `author`, `tags`, `dimensions`, and `framerate`.
#[derive(Clone, Debug, Default)]
pub struct PropertySideTables {
    enabled: bool,
    index: HashMap<(u32, u64), u32>,
    slots: Vec<PackedPropertySlot>,
    free_slots: Vec<u32>,
    strings: Vec<u8>,
}

impl PropertySideTables {
    /// Constructs a disabled store with strictly zero heap capacity.
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Toggles property indexing. Disabling immediately drops and frees all backing storage.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.clear_and_shrink();
        }
    }

    pub fn clear_and_shrink(&mut self) {
        self.index = HashMap::new();
        self.slots = Vec::new();
        self.free_slots = Vec::new();
        self.strings = Vec::new();
    }

    pub fn record_count(&self) -> usize {
        self.index.len()
    }

    /// Inserts or updates extracted properties for `(source, record_key)`.
    ///
    /// Returns `false` if property indexing is disabled (allocating nothing) or if `data` is empty.
    pub fn upsert(
        &mut self,
        source: SourceId,
        record_key: u64,
        data: &RecordPropertyData,
        modified_ts: i64,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        if data.is_empty() {
            self.remove(source, record_key);
            return false;
        }

        let mut slot = PackedPropertySlot {
            modified_ts: stamp_u32(modified_ts),
            ..PackedPropertySlot::default()
        };
        if let Some(ms) = data.length_ms {
            slot.presence |= HAS_LENGTH;
            slot.length_ms = ms;
        }
        if let Some(ref author) = data.author {
            if !author.is_empty() {
                let (off, len) = self.intern_str(author);
                slot.presence |= HAS_AUTHOR;
                slot.author_off = off;
                slot.author_len = len;
            }
        }
        if let Some(ref tags) = data.tags {
            if !tags.is_empty() {
                let (off, len) = self.intern_str(tags);
                slot.presence |= HAS_TAGS;
                slot.tags_off = off;
                slot.tags_len = len;
            }
        }
        if let Some((w, h)) = data.dimensions {
            slot.presence |= HAS_DIMENSIONS;
            slot.width = w;
            slot.height = h;
        }
        if let Some(fps) = data.framerate_mfps {
            slot.presence |= HAS_FRAMERATE;
            slot.framerate_mfps = fps;
        }

        if let Some(&idx) = self.index.get(&(source.0, record_key)) {
            self.slots[idx as usize] = slot;
        } else if let Some(free) = self.free_slots.pop() {
            self.slots[free as usize] = slot;
            self.index.insert((source.0, record_key), free);
        } else {
            let idx = self.slots.len() as u32;
            self.slots.push(slot);
            self.index.insert((source.0, record_key), idx);
        }
        true
    }

    pub fn remove(&mut self, source: SourceId, record_key: u64) -> bool {
        let Some(slot_idx) = self.index.remove(&(source.0, record_key)) else {
            return false;
        };
        if let Some(slot) = self.slots.get_mut(slot_idx as usize) {
            *slot = PackedPropertySlot::default();
            self.free_slots.push(slot_idx);
        }
        true
    }

    pub fn clear_source(&mut self, source: SourceId) {
        let keys: Vec<(u32, u64)> = self
            .index
            .keys()
            .copied()
            .filter(|&(sid, _)| sid == source.0)
            .collect();
        for (sid, rkey) in keys {
            self.remove(SourceId(sid), rkey);
        }
    }

    /// Drops entries for `source` whose record key is no longer present in the source.
    pub fn retain_source_keys(&mut self, source: SourceId, present: &HashSet<u64>) {
        let stale: Vec<u64> = self
            .index
            .keys()
            .copied()
            .filter(|&(sid, key)| sid == source.0 && !present.contains(&key))
            .map(|(_, key)| key)
            .collect();
        for key in stale {
            self.remove(source, key);
        }
    }

    #[inline]
    pub fn get(
        &self,
        source: SourceId,
        record_key: u64,
        modified_ts: i64,
    ) -> Option<RecordPropertyView<'_>> {
        if !self.enabled {
            return None;
        }
        let &slot_idx = self.index.get(&(source.0, record_key))?;
        let slot = self.slots.get(slot_idx as usize)?;
        if slot.modified_ts != stamp_u32(modified_ts) {
            return None;
        }
        Some(RecordPropertyView {
            length_ms: (slot.presence & HAS_LENGTH != 0).then_some(slot.length_ms),
            author: (slot.presence & HAS_AUTHOR != 0)
                .then(|| self.read_str(slot.author_off, slot.author_len)),
            tags: (slot.presence & HAS_TAGS != 0)
                .then(|| self.read_str(slot.tags_off, slot.tags_len)),
            dimensions: (slot.presence & HAS_DIMENSIONS != 0).then_some((slot.width, slot.height)),
            framerate_mfps: (slot.presence & HAS_FRAMERATE != 0).then_some(slot.framerate_mfps),
        })
    }

    /// Exact tracked heap allocation in bytes (`0` when disabled).
    pub fn allocated_bytes(&self) -> usize {
        hashmap_table_bytes::<(u32, u64), u32>(self.index.capacity())
            + self.slots.capacity() * size_of::<PackedPropertySlot>()
            + self.free_slots.capacity() * size_of::<u32>()
            + self.strings.capacity()
    }

    fn intern_str(&mut self, s: &str) -> (u32, u16) {
        let max_len = (u16::MAX as usize).min(s.len());
        let mut valid_len = max_len;
        while valid_len > 0 && !s.is_char_boundary(valid_len) {
            valid_len -= 1;
        }
        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(&s.as_bytes()[..valid_len]);
        (off, valid_len as u16)
    }

    #[inline]
    fn read_str(&self, off: u32, len: u16) -> &str {
        let start = off as usize;
        let end = start.saturating_add(len as usize);
        if end <= self.strings.len() {
            std::str::from_utf8(&self.strings[start..end]).unwrap_or("")
        } else {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PackedPropertySlot;
    use std::mem::size_of;

    #[test]
    fn a_property_slot_stays_within_forty_bytes() {
        assert_eq!(size_of::<PackedPropertySlot>(), 40);
    }
}

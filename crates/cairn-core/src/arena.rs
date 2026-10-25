// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Continuous name storage arena with Unicode case-folding support.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Continuous byte pool storing UTF-8 file names alongside a case-folded lowercase copy.
/// Ensures maximum CPU cache locality and zero individual heap allocations per file.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NamePool {
    /// Original UTF-8 filename bytes.
    arena: Vec<u8>,
    /// Case-folded (lowercase) byte copy for case-insensitive substring searching.
    /// Strictly maintains a 1:1 byte length correspondence with `arena`.
    folded_arena: Vec<u8>,
}

impl NamePool {
    /// Exposes raw internal byte slices for persistence.
    pub fn raw_parts(&self) -> (&[u8], &[u8]) {
        (&self.arena, &self.folded_arena)
    }

    /// Reconstructs a `NamePool` directly from raw byte vectors.
    pub fn from_raw_parts(arena: Vec<u8>, folded_arena: Vec<u8>) -> Self {
        Self {
            arena,
            folded_arena,
        }
    }

    /// Creates a new empty `NamePool` with pre-allocated capacity.
    pub fn with_capacity(capacity_bytes: usize) -> Self {
        Self {
            arena: Vec::with_capacity(capacity_bytes),
            folded_arena: Vec::with_capacity(capacity_bytes),
        }
    }

    /// Reserves capacity for at least `additional_bytes` more bytes in both the original and folded arenas.
    pub fn reserve(&mut self, additional_bytes: usize) {
        self.arena.reserve(additional_bytes);
        self.folded_arena.reserve(additional_bytes);
    }

    /// Inserts a filename into the arena, returning `(offset, length, ext_offset)`.
    ///
    /// - `offset`: byte offset in the arena
    /// - `length`: byte length in UTF-8
    /// - `ext_offset`: relative byte offset within the name pointing after the last '.',
    ///   or `u16::MAX` if no extension is present.
    pub fn insert(&mut self, name: &str) -> (u32, u16, u16) {
        let offset = self.arena.len() as u32;
        let max_len = (u16::MAX as usize).min(name.len());
        let mut valid_len = max_len;
        while valid_len > 0 && !name.is_char_boundary(valid_len) {
            valid_len -= 1;
        }
        let valid_str = &name[..valid_len];
        let len = valid_len as u16;
        let slice = valid_str.as_bytes();

        self.arena.extend_from_slice(slice);

        // Fast path for ASCII filenames (overwhelming majority on Windows)
        if valid_str.is_ascii() {
            let prev_folded_len = self.folded_arena.len();
            self.folded_arena.extend_from_slice(slice);
            self.folded_arena[prev_folded_len..].make_ascii_lowercase();
        } else {
            // Append case-folded characters ensuring exact byte length alignment
            for ch in valid_str.chars() {
                if ch.is_ascii() {
                    self.folded_arena.push(ch.to_ascii_lowercase() as u8);
                } else {
                    let ch_len = ch.len_utf8();
                    let lower_count: usize = ch.to_lowercase().map(|c| c.len_utf8()).sum();
                    if lower_count == ch_len {
                        for lower_ch in ch.to_lowercase() {
                            let mut lower_buf = [0u8; 4];
                            self.folded_arena
                                .extend_from_slice(lower_ch.encode_utf8(&mut lower_buf).as_bytes());
                        }
                    } else {
                        // If Unicode lowercase changes byte length, preserve original character bytes
                        let mut orig_buf = [0u8; 4];
                        self.folded_arena
                            .extend_from_slice(ch.encode_utf8(&mut orig_buf).as_bytes());
                    }
                }
            }
        }
        debug_assert_eq!(self.arena.len(), self.folded_arena.len());

        // Calculate extension offset
        let ext_offset = match slice.iter().rposition(|&b| b == b'.') {
            Some(dot_idx) if dot_idx + 1 < slice.len() => (dot_idx + 1) as u16,
            _ => u16::MAX,
        };

        (offset, len, ext_offset)
    }

    /// Folds a search query using the identical Unicode folding strategy as `insert`.
    pub fn fold_query_bytes(query: &str) -> Vec<u8> {
        if query.is_ascii() {
            let mut out = query.as_bytes().to_vec();
            out.make_ascii_lowercase();
            return out;
        }

        let mut out = Vec::with_capacity(query.len());
        for ch in query.chars() {
            if ch.is_ascii() {
                out.push(ch.to_ascii_lowercase() as u8);
            } else {
                let ch_len = ch.len_utf8();
                let lower_count: usize = ch.to_lowercase().map(|c| c.len_utf8()).sum();
                if lower_count == ch_len {
                    for lower_ch in ch.to_lowercase() {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(lower_ch.encode_utf8(&mut buf).as_bytes());
                    }
                } else {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        out
    }

    /// Retrieves the original filename as a UTF-8 string slice.
    #[inline(always)]
    pub fn get_name(&self, offset: u32, len: u16) -> &str {
        let start = offset as usize;
        let end = start + len as usize;
        if end <= self.arena.len() {
            std::str::from_utf8(&self.arena[start..end]).unwrap_or("")
        } else {
            ""
        }
    }

    /// Retrieves the case-folded (lowercase) byte slice for fast substring matching.
    #[inline(always)]
    pub fn get_folded(&self, offset: u32, len: u16) -> &[u8] {
        let start = offset as usize;
        let end = start + len as usize;
        if end <= self.folded_arena.len() {
            &self.folded_arena[start..end]
        } else {
            &[]
        }
    }

    /// Retrieves the file extension (excluding the leading dot), or empty string if none.
    #[inline(always)]
    pub fn get_ext(&self, offset: u32, len: u16, ext_offset: u16) -> &str {
        if ext_offset == u16::MAX || (ext_offset as usize) >= (len as usize) {
            ""
        } else {
            let start = offset as usize + ext_offset as usize;
            let end = offset as usize + len as usize;
            if end <= self.arena.len() {
                std::str::from_utf8(&self.arena[start..end]).unwrap_or("")
            } else {
                ""
            }
        }
    }

    /// Total number of bytes stored in the arena.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Checks if the arena is empty.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    /// Clears the arena.
    pub fn clear(&mut self) {
        self.arena.clear();
        self.folded_arena.clear();
    }
}

impl fmt::Debug for NamePool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NamePool")
            .field("bytes_len", &self.arena.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_pool_insert_and_retrieve() {
        let mut pool = NamePool::with_capacity(1024);
        let (off1, len1, ext1) = pool.insert("Hello World.TXT");
        assert_eq!(pool.get_name(off1, len1), "Hello World.TXT");
        assert_eq!(
            std::str::from_utf8(pool.get_folded(off1, len1)).unwrap(),
            "hello world.txt"
        );
        assert_eq!(pool.get_ext(off1, len1, ext1), "TXT");

        let (off2, len2, ext2) = pool.insert("文档.docx");
        assert_eq!(pool.get_name(off2, len2), "文档.docx");
        assert_eq!(pool.get_ext(off2, len2, ext2), "docx");

        let (off3, len3, ext3) = pool.insert("noext");
        assert_eq!(ext3, u16::MAX);
        assert_eq!(pool.get_ext(off3, len3, ext3), "");
    }
}

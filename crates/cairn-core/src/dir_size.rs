// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Recursive directory sizes over a compact volume index.

use crate::arena::NamePool;
use crate::model::FileRecord;

/// Precomputed recursive byte totals for compact [`FileRecord`]s.
///
/// Child lists are stored once in compressed form, so the index costs no per-directory
/// allocation. Tombstones, records under a tombstone and records whose parent chain never
/// reaches the root are excluded from the totals.
#[derive(Clone, Debug, Default)]
pub struct DirectorySizeIndex {
    sizes: Vec<u64>,
    child_starts: Vec<u32>,
    child_records: Vec<u32>,
    root_children: Vec<u32>,
}

enum Placement {
    Root,
    Child(u32),
    Excluded,
}

impl DirectorySizeIndex {
    pub fn allocated_bytes(&self) -> usize {
        self.sizes.capacity() * std::mem::size_of::<u64>()
            + (self.child_starts.capacity()
                + self.child_records.capacity()
                + self.root_children.capacity())
                * std::mem::size_of::<u32>()
    }

    pub fn build(records: &[FileRecord], arena: &NamePool) -> Self {
        let n = records.len();
        let root_record = find_root_record(records, arena);
        let placement = |index: usize, record: &FileRecord| -> Placement {
            let idx = index as u32;
            if record.is_tombstone() || Some(idx) == root_record {
                return Placement::Excluded;
            }
            let parent = record.parent_idx;
            if parent == FileRecord::ROOT_PARENT || parent == idx || Some(parent) == root_record {
                Placement::Root
            } else if (parent as usize) < n {
                Placement::Child(parent)
            } else {
                Placement::Excluded
            }
        };

        let mut child_starts = vec![0u32; n + 1];
        let mut root_children = Vec::new();
        for (index, record) in records.iter().enumerate() {
            match placement(index, record) {
                Placement::Root => root_children.push(index as u32),
                Placement::Child(parent) => child_starts[parent as usize + 1] += 1,
                Placement::Excluded => {}
            }
        }
        for index in 0..n {
            child_starts[index + 1] += child_starts[index];
        }
        let mut cursor = child_starts[..n].to_vec();
        let mut child_records = vec![0u32; child_starts[n] as usize];
        for (index, record) in records.iter().enumerate() {
            if let Placement::Child(parent) = placement(index, record) {
                let slot = &mut cursor[parent as usize];
                child_records[*slot as usize] = index as u32;
                *slot += 1;
            }
        }
        drop(cursor);

        let mut index = Self {
            sizes: vec![0; n],
            child_starts,
            child_records,
            root_children,
        };
        index.aggregate(records);

        if let Some(root) = root_record {
            index.sizes[root as usize] = index.root_children.iter().fold(0u64, |total, &child| {
                total.saturating_add(index.sizes[child as usize])
            });
        }
        index
    }

    fn aggregate(&mut self, records: &[FileRecord]) {
        let mut order = Vec::with_capacity(records.len());
        let mut stack = self.root_children.clone();
        while let Some(idx) = stack.pop() {
            order.push(idx);
            stack.extend_from_slice(self.children_of(Some(idx)));
        }
        for &idx in order.iter().rev() {
            let record = &records[idx as usize];
            let own = if record.is_file() { record.size } else { 0 };
            let children = self
                .children_of(Some(idx))
                .iter()
                .fold(0u64, |total, &child| {
                    total.saturating_add(self.sizes[child as usize])
                });
            self.sizes[idx as usize] = own.saturating_add(children);
        }
    }

    fn children_of(&self, dir_idx: Option<u32>) -> &[u32] {
        match dir_idx {
            None => &self.root_children,
            Some(idx) => {
                let idx = idx as usize;
                match (self.child_starts.get(idx), self.child_starts.get(idx + 1)) {
                    (Some(&start), Some(&end)) => &self.child_records[start as usize..end as usize],
                    _ => &[],
                }
            }
        }
    }

    /// Writes every live directory's recursive indexed size back into its record. The service
    /// keeps this value in `FileRecord.size`, so the table can display and sort folders without a
    /// second size structure; files keep their own size.
    pub fn apply_directory_sizes(records: &mut [FileRecord], arena: &NamePool) {
        let index = Self::build(records, arena);
        for (record, size) in records.iter_mut().zip(index.sizes.iter()) {
            if record.is_dir() && !record.is_tombstone() {
                record.size = *size;
            }
        }
    }
}

fn find_root_record(records: &[FileRecord], arena: &NamePool) -> Option<u32> {
    records.iter().enumerate().find_map(|(index, record)| {
        let is_top =
            record.parent_idx == FileRecord::ROOT_PARENT || record.parent_idx == index as u32;
        if record.is_tombstone() || !record.is_dir() || !is_top {
            return None;
        }
        let name = arena.get_name(record.name_offset, record.name_len);
        let is_root_name = name == "."
            || name.is_empty()
            || name == "\\"
            || name == "$Root"
            || name.ends_with(":\\")
            || (name.len() == 2 && name.ends_with(':'));
        is_root_name.then_some(index as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FLAG_DIRECTORY;

    fn push(
        records: &mut Vec<FileRecord>,
        pool: &mut NamePool,
        name: &str,
        parent_idx: u32,
        flags: u16,
        size: u64,
    ) -> u32 {
        let (name_offset, name_len, ext_offset) = pool.insert(name);
        records.push(FileRecord {
            frn: records.len() as u64 + 1,
            parent_idx,
            name_offset,
            name_len,
            ext_offset,
            volume_id: 2,
            flags,
            size,
            modified_ts: 0,
            created_ts: 0,
        });
        records.len() as u32 - 1
    }

    #[test]
    fn directory_sizes_aggregate_indexed_descendants() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let root = push(
            &mut records,
            &mut pool,
            "C:\\",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        let windows = push(&mut records, &mut pool, "Windows", root, FLAG_DIRECTORY, 0);
        push(&mut records, &mut pool, "kernel32.dll", windows, 0, 3000);
        push(&mut records, &mut pool, "pagefile.sys", root, 0, 10_000);
        DirectorySizeIndex::apply_directory_sizes(&mut records, &pool);
        assert_eq!(records[root as usize].size, 13_000);
        assert_eq!(records[windows as usize].size, 3_000);
    }

    #[test]
    fn directory_sizes_saturate_instead_of_overflowing() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let root = push(
            &mut records,
            &mut pool,
            "C:\\",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        push(&mut records, &mut pool, "large.bin", root, 0, u64::MAX);
        push(&mut records, &mut pool, "also-large.bin", root, 0, 1);
        DirectorySizeIndex::apply_directory_sizes(&mut records, &pool);
        assert_eq!(records[root as usize].size, u64::MAX);
    }

    #[test]
    fn without_an_explicit_root_each_top_level_entry_stays_visible() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let a = push(
            &mut records,
            &mut pool,
            "A",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        push(&mut records, &mut pool, "a.bin", a, 0, 100);
        push(
            &mut records,
            &mut pool,
            "b.bin",
            FileRecord::ROOT_PARENT,
            0,
            50,
        );
        DirectorySizeIndex::apply_directory_sizes(&mut records, &pool);
        assert_eq!(records[a as usize].size, 100);
        assert_eq!(records[a as usize + 1].size, 100);
        assert_eq!(records[a as usize + 2].size, 50);
    }

    #[test]
    fn tombstones_and_orphan_parent_chains_are_excluded() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let dead = push(
            &mut records,
            &mut pool,
            "Dead",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        let orphan = push(&mut records, &mut pool, "Orphan", 99, FLAG_DIRECTORY, 0);
        push(&mut records, &mut pool, "a.bin", dead, 0, 100);
        push(&mut records, &mut pool, "b.bin", orphan, 0, 200);
        records[dead as usize].flags |= crate::model::FLAG_TOMBSTONE;
        DirectorySizeIndex::apply_directory_sizes(&mut records, &pool);
        assert_eq!(records[dead as usize].size, 0);
        assert_eq!(records[orphan as usize].size, 0);
    }
}

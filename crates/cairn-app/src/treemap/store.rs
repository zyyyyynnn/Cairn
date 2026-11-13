// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Bounded LRU cache primitives for Treemap directory subtrees and descendant totals.

#[cfg(test)]
use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

pub(crate) const MAX_CACHED_DIR_TREES: usize = 16;
pub(crate) const MAX_SCANNED_ITEM_TOTALS: usize = 64;

/// Insertion/access-ordered bounded LRU map backed by `HashMap` + `VecDeque`.
///
/// Designed for small fixed capacities (`16` trees, `64` directory totals) where recency touch is a
/// short stack scan without extra heap allocations per lookup.
#[derive(Debug)]
pub(crate) struct TreemapLru<K, V> {
    capacity: usize,
    entries: HashMap<K, V>,
    order: VecDeque<K>,
}

impl<K: Eq + Hash + Clone, V> TreemapLru<K, V> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            entries: HashMap::with_capacity(capacity.min(64)),
            order: VecDeque::with_capacity(capacity.min(64)),
        }
    }

    /// Read-only observation for tests in other modules; does not touch recency.
    #[cfg(test)]
    pub(crate) fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.entries.get(key)
    }

    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        if !self.entries.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.entries.get(key)
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        if self.entries.contains_key(&key) {
            self.touch(&key);
            return self.entries.insert(key, value);
        }
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value)
    }

    pub(crate) fn insert_if_absent(&mut self, key: K, value: V) {
        if !self.entries.contains_key(&key) {
            let _ = self.insert(key, value);
        }
    }

    pub(crate) fn remove(&mut self, key: &K) -> Option<V> {
        let removed = self.entries.remove(key)?;
        self.order.retain(|existing| existing != key);
        Some(removed)
    }

    pub(crate) fn retain(&mut self, mut pred: impl FnMut(&K, &mut V) -> bool) {
        self.entries.retain(|k, v| pred(k, v));
        self.order.retain(|k| self.entries.contains_key(k));
    }

    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|existing| existing == key) {
            self.order.remove(pos);
            self.order.push_back(key.clone());
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Read-only observation for tests in other modules.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::TreemapLru;

    #[test]
    fn treemap_lru_bounds_entries_and_promotes_recently_read_keys() {
        let mut lru = TreemapLru::with_capacity(2);
        lru.insert("a".to_string(), 1usize);
        lru.insert("b".to_string(), 2usize);
        assert_eq!(lru.entries.len(), 2);

        // Touch `"a"` so `"b"` becomes least recently used.
        assert_eq!(lru.get(&"a".to_string()).copied(), Some(1));

        lru.insert("c".to_string(), 3usize);
        assert_eq!(lru.entries.len(), 2);
        assert_eq!(lru.entries.get("a").copied(), Some(1));
        assert_eq!(lru.entries.get("b").copied(), None);
        assert_eq!(lru.entries.get("c").copied(), Some(3));
    }
}

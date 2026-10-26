// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Query session cache and pagination manager with multi-volume support.

use crate::sync_engine::VolumeIndex;
use cairn_core::engine::CancellationToken;
use cairn_protocol::message::FileRowData;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Idle sessions older than 5 minutes are evicted when a new session is created.
const SESSION_TTL: Duration = Duration::from_secs(300);

/// Maximum total `SearchResultItem` entries retained across all cached sessions (~256 MiB ceiling).
const MAX_TOTAL_CACHED_ITEMS: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchResultItem {
    pub drive_letter: char,
    pub record_idx: u32,
    /// [`VolumeIndex`] generation observed when the match was produced. A later rebuild,
    /// compaction or reload replaces the record table, so these indices no longer describe the
    /// same rows.
    pub generation: u64,
}

/// Why a row page cannot be served from the cached match list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowsError {
    /// The session was evicted (LRU) or never existed.
    UnknownSession,
    /// The volume's record table was rebuilt since the query, so the cached indices are stale.
    Stale { drive_letter: char },
}

pub struct QuerySession {
    pub pre_folded_terms: Vec<Vec<u8>>,
    pub matched_items: Vec<SearchResultItem>,
    pub snippets: HashMap<(char, u64), String>,
    pub created_at: Instant,
    pub last_accessed_at: Instant,
}

pub struct SessionManager {
    sessions: RwLock<HashMap<u64, QuerySession>>,
    active_queries: RwLock<HashMap<u64, CancellationToken>>,
    next_session_id: AtomicU64,
    max_sessions: usize,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(8)
    }
}

impl SessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            active_queries: RwLock::new(HashMap::new()),
            next_session_id: AtomicU64::new(1),
            max_sessions,
        }
    }

    /// Registers an in-flight query and preemptively cancels any earlier in-flight queries so
    /// rapid typing over short-lived pipe connections immediately aborts superseded searches.
    pub fn begin_query(&self, query_id: u64, conn_cancel: &CancellationToken) -> CancellationToken {
        let token = CancellationToken::new();
        if conn_cancel.is_cancelled() {
            token.cancel();
        }
        let mut active = self.active_queries.write().unwrap();
        for prev in active.values() {
            prev.cancel();
        }
        active.insert(query_id, token.clone());
        token
    }

    /// Removes a completed query from the active-query registry.
    pub fn finish_query(&self, query_id: u64) {
        self.active_queries.write().unwrap().remove(&query_id);
    }

    /// Cancels an in-flight query by `query_id` (or all active queries if `query_id == 0` or not found).
    pub fn cancel_query(&self, query_id: u64) {
        let active = self.active_queries.read().unwrap();
        if let Some(token) = active.get(&query_id) {
            token.cancel();
        } else {
            for token in active.values() {
                token.cancel();
            }
        }
    }

    pub fn create_session(
        &self,
        _query_text: String,
        highlight_terms: Vec<String>,
        matched_items: Vec<SearchResultItem>,
        snippets: HashMap<(char, u64), String>,
    ) -> u64 {
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let mut lock = self.sessions.write().unwrap();
        let now = Instant::now();

        // Evict idle sessions that have exceeded their TTL.
        lock.retain(|_, s| now.saturating_duration_since(s.last_accessed_at) < SESSION_TTL);

        // LRU eviction if session count or total retained item budget is exceeded.
        let new_items = matched_items.len();
        while !lock.is_empty() && {
            let total_items: usize = lock.values().map(|s| s.matched_items.len()).sum();
            lock.len() >= self.max_sessions
                || total_items.saturating_add(new_items) > MAX_TOTAL_CACHED_ITEMS
        } {
            if let Some((&oldest_id, _)) = lock
                .iter()
                .min_by_key(|(&id, s)| (s.last_accessed_at, s.created_at, id))
            {
                lock.remove(&oldest_id);
            } else {
                break;
            }
        }

        let pre_folded_terms: Vec<Vec<u8>> = highlight_terms
            .iter()
            .map(|t| cairn_core::arena::NamePool::fold_query_bytes(t))
            .filter(|t| !t.is_empty())
            .collect();

        lock.insert(
            session_id,
            QuerySession {
                pre_folded_terms,
                matched_items,
                snippets,
                created_at: now,
                last_accessed_at: now,
            },
        );

        session_id
    }

    /// Drops a session whose cached record indices can no longer be served. Later pages of that
    /// session become [`RowsError::UnknownSession`] and return an empty page, which is what the
    /// protocol already does for an evicted session.
    pub fn invalidate(&self, session_id: u64) {
        self.sessions.write().unwrap().remove(&session_id);
    }

    /// Fetches rows across all indexed volumes for an active session.
    ///
    /// A page is only served while every volume it touches still has the generation observed at
    /// query time. Serving indices from a replaced table would show another file's row or an
    /// out-of-range index, so the page is refused instead.
    pub fn get_rows_multi(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
        volumes: &HashMap<char, VolumeIndex>,
    ) -> Result<Vec<FileRowData>, RowsError> {
        let mut lock = self.sessions.write().unwrap();
        let session = lock.get_mut(&session_id).ok_or(RowsError::UnknownSession)?;
        session.last_accessed_at = Instant::now();

        rows_for(session, offset, limit, &session.pre_folded_terms, |item| {
            volumes.get(&item.drive_letter)
        })
    }
}

/// Materializes one page of a session against the current volume tables.
fn rows_for<'a>(
    session: &QuerySession,
    offset: usize,
    limit: usize,
    folded_terms: &[Vec<u8>],
    volume_of: impl Fn(&SearchResultItem) -> Option<&'a VolumeIndex>,
) -> Result<Vec<FileRowData>, RowsError> {
    let end = offset
        .saturating_add(limit)
        .min(session.matched_items.len());
    if offset >= session.matched_items.len() {
        return Ok(Vec::new());
    }

    let slice = &session.matched_items[offset..end];
    let mut rows = Vec::with_capacity(slice.len());

    for item in slice {
        let Some(vol) = volume_of(item) else {
            continue;
        };
        if vol.generation != item.generation {
            return Err(RowsError::Stale {
                drive_letter: item.drive_letter,
            });
        }
        if let Some(row) = vol.build_file_row(item.record_idx, folded_terms, |frn| {
            session.snippets.get(&(item.drive_letter, frn)).cloned()
        }) {
            rows.push(row);
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::model::FileRecord;
    use std::collections::HashMap;

    fn volume_with(name: &str) -> VolumeIndex {
        let mut vol = VolumeIndex::new('C');
        vol.records.push(FileRecord {
            frn: 10,
            parent_idx: FileRecord::ROOT_PARENT,
            name_offset: 0,
            name_len: 0,
            ext_offset: 0,
            volume_id: 2,
            flags: 0,
            size: 1,
            modified_ts: 1,
            created_ts: 1,
        });
        let (name_offset, name_len, ext_offset) = vol.arena.insert(name);
        vol.records[0].name_offset = name_offset;
        vol.records[0].name_len = name_len;
        vol.records[0].ext_offset = ext_offset;
        vol.frn_to_idx.insert(10, vec![0]);
        vol
    }

    fn item(generation: u64) -> SearchResultItem {
        SearchResultItem {
            drive_letter: 'C',
            record_idx: 0,
            generation,
        }
    }

    #[test]
    fn unchanged_volumes_serve_pages() {
        let sessions = SessionManager::default();
        let vol = volume_with("notes.txt");
        let session_id = sessions.create_session(
            "notes".into(),
            Vec::new(),
            vec![item(vol.generation)],
            HashMap::new(),
        );
        let volumes = HashMap::from([('C', vol)]);

        let rows = sessions
            .get_rows_multi(session_id, 0, 10, &volumes)
            .expect("a valid session must page normally");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "notes.txt");
    }

    #[test]
    fn a_rebuilt_volume_refuses_stale_pages_instead_of_serving_other_rows() {
        let sessions = SessionManager::default();
        let vol = volume_with("notes.txt");
        let session_id = sessions.create_session(
            "notes".into(),
            Vec::new(),
            vec![item(vol.generation)],
            HashMap::new(),
        );
        let mut volumes = HashMap::from([('C', vol)]);

        // Replacing the table (rescan, reload, compaction) invalidates every cached record index.
        volumes.get_mut(&'C').unwrap().clear();
        let error = sessions
            .get_rows_multi(session_id, 0, 10, &volumes)
            .expect_err("a stale page must not be served");
        assert_eq!(error, RowsError::Stale { drive_letter: 'C' });
    }

    #[test]
    fn an_evicted_session_is_distinguishable_from_a_stale_one() {
        let sessions = SessionManager::new(1);
        let vol = volume_with("notes.txt");
        let volumes = HashMap::from([('C', vol)]);
        let first =
            sessions.create_session("notes".into(), Vec::new(), vec![item(0)], HashMap::new());
        sessions.create_session("other".into(), Vec::new(), Vec::new(), HashMap::new());
        assert_eq!(
            sessions.get_rows_multi(first, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
    }

    #[test]
    fn lru_eviction_evicts_least_recently_accessed_session() {
        let sessions = SessionManager::new(2);
        let vol = volume_with("notes.txt");
        let volumes = HashMap::from([('C', vol)]);

        let s1 = sessions.create_session("s1".into(), Vec::new(), vec![item(0)], HashMap::new());
        std::thread::sleep(std::time::Duration::from_millis(20));
        let s2 = sessions.create_session("s2".into(), Vec::new(), vec![item(0)], HashMap::new());

        // Access s1, making s2 the least recently accessed
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = sessions.get_rows_multi(s1, 0, 10, &volumes);

        // Create s3: capacity is 2, so s2 should be evicted because s1 was accessed more recently
        let s3 = sessions.create_session("s3".into(), Vec::new(), vec![item(0)], HashMap::new());

        assert!(sessions.get_rows_multi(s1, 0, 10, &volumes).is_ok());
        assert_eq!(
            sessions.get_rows_multi(s2, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
        assert!(sessions.get_rows_multi(s3, 0, 10, &volumes).is_ok());
    }

    #[test]
    fn begin_query_cancels_superseded_queries_and_cancel_query_signals_active_token() {
        let sessions = SessionManager::default();
        let conn = CancellationToken::new();

        let q1 = sessions.begin_query(1, &conn);
        assert!(!q1.is_cancelled());

        // Starting query 2 preemptively cancels query 1.
        let q2 = sessions.begin_query(2, &conn);
        assert!(q1.is_cancelled());
        assert!(!q2.is_cancelled());

        // Cross-connection Cancel(2) cancels query 2.
        sessions.cancel_query(2);
        assert!(q2.is_cancelled());
        sessions.finish_query(2);
    }
}

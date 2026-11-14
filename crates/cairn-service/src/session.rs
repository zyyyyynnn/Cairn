// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Query session cache and pagination manager with multi-volume support.

use crate::sync_engine::{SearchResultItem, VolumeIndex};
use cairn_core::engine::CancellationToken;
use cairn_protocol::message::FileRowData;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Idle sessions older than 5 minutes are evicted when a new session is created.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(300);

/// Maximum total `SearchResultItem` entries retained across all cached sessions (~256 MiB ceiling).
const MAX_TOTAL_CACHED_ITEMS: usize = 16 * 1024 * 1024;

/// Monotonic time source injected into [`SessionManager`] for lock-free reads and deterministic TTL tests.
pub trait Clock: Send + Sync {
    fn now_nanos(&self) -> u64;
}

/// Default process-monotonic clock backed by [`Instant`].
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_nanos(&self) -> u64 {
        self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }
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
    owner: QueryOwner,
    pub pre_folded_terms: Vec<Vec<u8>>,
    pub matched_items: Vec<SearchResultItem>,
    pub snippets: HashMap<(char, u64), String>,
    pub created_nanos: u64,
    pub last_accessed_nanos: AtomicU64,
}

/// The client a query belongs to. Query ids are client-local counters, so the
/// owner is part of the registry key: a stale id from one client cannot reach
/// another client's query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct QueryOwner {
    /// Process id reported by the pipe provider, never by the client.
    pub client_pid: u32,
    /// The token user SID the process runs as.
    pub user_sid: String,
}

pub struct SessionManager {
    sessions: RwLock<HashMap<u64, QuerySession>>,
    active_queries: RwLock<HashMap<(QueryOwner, u64), CancellationToken>>,
    /// Open pipe connections per client. A client's cached results cannot be paged once its
    /// last connection closes, so the last release frees them immediately instead of waiting
    /// for the TTL or LRU pressure.
    connections: Mutex<HashMap<QueryOwner, usize>>,
    next_session_id: AtomicU64,
    max_sessions: usize,
    ttl_nanos: u64,
    clock: Box<dyn Clock>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(8, DEFAULT_SESSION_TTL, SystemClock::new())
    }
}

impl SessionManager {
    pub fn new(max_sessions: usize, ttl: Duration, clock: impl Clock + 'static) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            active_queries: RwLock::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            next_session_id: AtomicU64::new(1),
            max_sessions,
            ttl_nanos: ttl.as_nanos().min(u64::MAX as u128) as u64,
            clock: Box::new(clock),
        }
    }

    /// Records one open pipe connection for `owner`.
    pub fn register_client(&self, owner: &QueryOwner) {
        *self
            .connections
            .lock()
            .unwrap()
            .entry(owner.clone())
            .or_default() += 1;
    }

    /// Drops one open connection. The last one releases the client's cached sessions and
    /// cancels its in-flight queries: nothing can page or join them after the process leaves.
    pub fn release_client(&self, owner: &QueryOwner) {
        let last = {
            let mut connections = self.connections.lock().unwrap();
            match connections.get_mut(owner) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    connections.remove(owner);
                    true
                }
                None => false,
            }
        };
        if !last {
            return;
        }
        self.sessions
            .write()
            .unwrap()
            .retain(|_, session| &session.owner != owner);
        self.active_queries
            .write()
            .unwrap()
            .retain(|(query_owner, _), token| {
                if query_owner == owner {
                    token.cancel();
                    false
                } else {
                    true
                }
            });
    }

    /// Registers an in-flight query and preemptively cancels that client's earlier in-flight queries,
    /// so rapid typing over short-lived pipe connections immediately aborts superseded searches.
    /// Another client's query is never touched.
    pub fn begin_query(&self, owner: &QueryOwner, query_id: u64) -> CancellationToken {
        let token = CancellationToken::new();
        let mut active = self.active_queries.write().unwrap();
        active.retain(|(prev_owner, _), prev| {
            if prev_owner == owner {
                prev.cancel();
                false
            } else {
                true
            }
        });
        active.insert((owner.clone(), query_id), token.clone());
        token
    }

    /// Removes a completed query from the active-query registry.
    pub fn finish_query(&self, owner: &QueryOwner, query_id: u64) {
        self.active_queries
            .write()
            .unwrap()
            .remove(&(owner.clone(), query_id));
    }

    /// Cancels one client's in-flight query by `query_id`. An unknown id is a no-op: ids are
    /// client-local, so a stale id must never cancel another client's query.
    pub fn cancel_query(&self, owner: &QueryOwner, query_id: u64) {
        let active = self.active_queries.read().unwrap();
        if let Some(token) = active.get(&(owner.clone(), query_id)) {
            token.cancel();
        }
    }

    pub fn create_session(
        &self,
        owner: QueryOwner,
        _query_text: String,
        highlight_terms: Vec<String>,
        matched_items: Vec<SearchResultItem>,
        snippets: HashMap<(char, u64), String>,
    ) -> u64 {
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let mut lock = self.sessions.write().unwrap();
        let now = self.clock.now_nanos();

        // Evict idle sessions that have exceeded their TTL.
        lock.retain(|_, s| {
            let last_accessed = s.last_accessed_nanos.load(Ordering::Relaxed);
            now.saturating_sub(last_accessed) < self.ttl_nanos
        });

        // LRU eviction if session count or total retained item budget is exceeded.
        let new_items = matched_items.len();
        while !lock.is_empty() && {
            let total_items: usize = lock.values().map(|s| s.matched_items.len()).sum();
            lock.len() >= self.max_sessions
                || total_items.saturating_add(new_items) > MAX_TOTAL_CACHED_ITEMS
        } {
            if let Some((&oldest_id, _)) = lock.iter().min_by_key(|(&id, s)| {
                (
                    s.last_accessed_nanos.load(Ordering::Relaxed),
                    s.created_nanos,
                    id,
                )
            }) {
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
                owner,
                pre_folded_terms,
                matched_items,
                snippets,
                created_nanos: now,
                last_accessed_nanos: AtomicU64::new(now),
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
        let lock = self.sessions.read().unwrap();
        let session = lock.get(&session_id).ok_or(RowsError::UnknownSession)?;
        session
            .last_accessed_nanos
            .store(self.clock.now_nanos(), Ordering::Relaxed);

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
        if vol.generation() != item.generation {
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
    use cairn_ntfs::mft::ExtractedItem;
    use std::collections::HashMap;

    fn volume_with(name: &str) -> VolumeIndex {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 0,
            name: name.to_string(),
            is_dir: false,
            size: 1,
            created_ts: 1,
            modified_ts: 1,
            flags: 0,
            is_reparse_point: false,
        }]);
        vol
    }

    fn item(generation: u64) -> SearchResultItem {
        SearchResultItem {
            drive_letter: 'C',
            record_idx: 0,
            generation,
        }
    }

    fn owner(pid: u32) -> QueryOwner {
        QueryOwner {
            client_pid: pid,
            user_sid: "S-1-5-21-test".to_string(),
        }
    }

    #[test]
    fn unchanged_volumes_serve_pages() {
        let sessions = SessionManager::default();
        let vol = volume_with("notes.txt");
        let session_id = sessions.create_session(
            owner(7),
            "notes".into(),
            Vec::new(),
            vec![item(vol.generation())],
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
            owner(7),
            "notes".into(),
            Vec::new(),
            vec![item(vol.generation())],
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
        let sessions = SessionManager::new(1, DEFAULT_SESSION_TTL, SystemClock::new());
        let vol = volume_with("notes.txt");
        let volumes = HashMap::from([('C', vol)]);
        let first = sessions.create_session(
            owner(7),
            "notes".into(),
            Vec::new(),
            vec![item(0)],
            HashMap::new(),
        );
        sessions.create_session(
            owner(7),
            "other".into(),
            Vec::new(),
            Vec::new(),
            HashMap::new(),
        );
        assert_eq!(
            sessions.get_rows_multi(first, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
    }

    struct TestClock {
        now_nanos: std::sync::Arc<AtomicU64>,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                now_nanos: std::sync::Arc::new(AtomicU64::new(1_000_000_000)),
            }
        }

        fn advance(&self, delta: Duration) {
            self.now_nanos
                .fetch_add(delta.as_nanos() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now_nanos(&self) -> u64 {
            self.now_nanos.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn lru_eviction_evicts_least_recently_accessed_session() {
        let clock = TestClock::new();
        let sessions = SessionManager::new(
            2,
            DEFAULT_SESSION_TTL,
            TestClock {
                now_nanos: clock.now_nanos.clone(),
            },
        );
        let vol = volume_with("notes.txt");
        let gen = vol.generation();
        let volumes = HashMap::from([('C', vol)]);

        let s1 = sessions.create_session(
            owner(7),
            "s1".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        clock.advance(Duration::from_millis(20));
        let s2 = sessions.create_session(
            owner(7),
            "s2".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );

        // Access s1, making s2 the least recently accessed
        clock.advance(Duration::from_millis(20));
        assert!(sessions.get_rows_multi(s1, 0, 10, &volumes).is_ok());

        // Create s3: capacity is 2, so s2 should be evicted because s1 was accessed more recently
        let s3 = sessions.create_session(
            owner(7),
            "s3".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );

        assert!(sessions.get_rows_multi(s1, 0, 10, &volumes).is_ok());
        assert_eq!(
            sessions.get_rows_multi(s2, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
        assert!(sessions.get_rows_multi(s3, 0, 10, &volumes).is_ok());
    }

    #[test]
    fn ttl_eviction_respects_injected_clock_boundary() {
        let clock = TestClock::new();
        let sessions = SessionManager::new(
            4,
            Duration::from_secs(300),
            TestClock {
                now_nanos: clock.now_nanos.clone(),
            },
        );
        let vol = volume_with("notes.txt");
        let gen = vol.generation();
        let volumes = HashMap::from([('C', vol)]);

        let s1 = sessions.create_session(
            owner(7),
            "s1".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        clock.advance(Duration::from_secs(299));
        let _s2 = sessions.create_session(
            owner(7),
            "s2".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        assert!(
            sessions.get_rows_multi(s1, 0, 10, &volumes).is_ok(),
            "session accessed before TTL must stay live"
        );

        // Accessing s1 reset its idle timer; advance full 300s to cross expiry.
        clock.advance(Duration::from_secs(300));
        let _s3 = sessions.create_session(
            owner(7),
            "s3".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        assert_eq!(
            sessions.get_rows_multi(s1, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
    }

    #[test]
    fn concurrent_get_rows_multi_updates_access_timestamp_without_write_starvation() {
        let clock = TestClock::new();
        let sessions = std::sync::Arc::new(SessionManager::new(
            2,
            DEFAULT_SESSION_TTL,
            TestClock {
                now_nanos: clock.now_nanos.clone(),
            },
        ));
        let vol = volume_with("notes.txt");
        let gen = vol.generation();
        let volumes = std::sync::Arc::new(HashMap::from([('C', vol)]));

        let s1 = sessions.create_session(
            owner(7),
            "s1".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        clock.advance(Duration::from_secs(1));
        let s2 = sessions.create_session(
            owner(7),
            "s2".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        clock.advance(Duration::from_secs(1));

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let s = sessions.clone();
                let v = volumes.clone();
                scope.spawn(move || {
                    for _ in 0..25 {
                        assert!(s.get_rows_multi(s1, 0, 10, &v).is_ok());
                    }
                });
            }
        });

        let _s3 = sessions.create_session(
            owner(7),
            "s3".into(),
            Vec::new(),
            vec![item(gen)],
            HashMap::new(),
        );
        assert!(sessions.get_rows_multi(s1, 0, 10, &volumes).is_ok());
        assert_eq!(
            sessions.get_rows_multi(s2, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
    }

    #[test]
    fn begin_query_cancels_superseded_queries_and_cancel_query_signals_active_token() {
        let sessions = SessionManager::default();
        let owner = QueryOwner::default();

        let q1 = sessions.begin_query(&owner, 1);
        assert!(!q1.is_cancelled());

        // Starting query 2 preemptively cancels query 1.
        let q2 = sessions.begin_query(&owner, 2);
        assert!(q1.is_cancelled());
        assert!(!q2.is_cancelled());

        // Cancel(2) from the same client cancels query 2.
        sessions.cancel_query(&owner, 2);
        assert!(q2.is_cancelled());
        sessions.finish_query(&owner, 2);
    }

    #[test]
    fn a_stale_query_id_cancels_nothing() {
        let sessions = SessionManager::default();
        let owner = QueryOwner::default();
        let q = sessions.begin_query(&owner, 11);

        sessions.cancel_query(&owner, 10);
        assert!(
            !q.is_cancelled(),
            "an id this client never used must not cancel its query"
        );

        sessions.cancel_query(&owner, 11);
        assert!(q.is_cancelled());
    }

    #[test]
    fn another_clients_identical_id_is_a_different_query() {
        let sessions = SessionManager::default();
        let first = QueryOwner {
            client_pid: 100,
            user_sid: "S-1-5-21-1".to_string(),
        };
        let second = QueryOwner {
            client_pid: 200,
            user_sid: "S-1-5-21-2".to_string(),
        };

        let first_query = sessions.begin_query(&first, 1);
        let second_query = sessions.begin_query(&second, 1);
        assert!(
            !first_query.is_cancelled(),
            "another client's query must not supersede this one"
        );
        assert!(!second_query.is_cancelled());

        sessions.cancel_query(&first, 1);
        assert!(first_query.is_cancelled());
        assert!(
            !second_query.is_cancelled(),
            "the same id under another owner is a different query"
        );
        sessions.finish_query(&first, 1);
        assert!(!second_query.is_cancelled());
    }

    /// A client's cached match lists can only be paged while it holds a connection; the last
    /// release frees them instead of waiting for the TTL.
    #[test]
    fn the_last_connection_release_frees_only_that_clients_sessions() {
        let sessions = SessionManager::default();
        let vol = volume_with("notes.txt");
        let generation = vol.generation();
        let volumes = HashMap::from([('C', vol)]);
        let mine = owner(7);
        let other = owner(8);
        sessions.register_client(&mine);
        sessions.register_client(&mine);
        sessions.register_client(&other);
        let my_session = sessions.create_session(
            mine.clone(),
            "mine".into(),
            Vec::new(),
            vec![item(generation)],
            HashMap::new(),
        );
        let other_session = sessions.create_session(
            other.clone(),
            "other".into(),
            Vec::new(),
            vec![item(generation)],
            HashMap::new(),
        );

        sessions.release_client(&mine);
        assert!(
            sessions.get_rows_multi(my_session, 0, 10, &volumes).is_ok(),
            "a remaining connection keeps the session"
        );

        sessions.release_client(&mine);
        assert_eq!(
            sessions.get_rows_multi(my_session, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
        assert!(
            sessions
                .get_rows_multi(other_session, 0, 10, &volumes)
                .is_ok(),
            "another client's session is untouched"
        );
    }

    #[test]
    fn the_last_connection_release_cancels_that_clients_query() {
        let sessions = SessionManager::default();
        let mine = owner(7);
        let other = owner(8);
        sessions.register_client(&mine);
        sessions.register_client(&other);
        let my_query = sessions.begin_query(&mine, 1);
        let other_query = sessions.begin_query(&other, 1);

        sessions.release_client(&mine);
        assert!(my_query.is_cancelled());
        assert!(!other_query.is_cancelled());
    }
}

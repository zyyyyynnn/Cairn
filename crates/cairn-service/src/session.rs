// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Query session cache and pagination manager with multi-volume support.

use crate::sync_engine::folder_source::FolderSource;
use crate::sync_engine::{AllFilesView, MatchSet, SearchResultItem, VolumeIndex};
use cairn_core::engine::CancellationToken;
use cairn_core::property_store::{PropertySideTables, SourceId};
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
    /// The source's record table was rebuilt since the query, so the cached indices are stale.
    Stale { source_id: SourceId },
}

pub struct QuerySession {
    owner: QueryOwner,
    pub pre_folded_terms: Vec<Vec<u8>>,
    rows: MatchSet,
    pub snippets: HashMap<(u32, u64), Vec<String>>,
    pub created_nanos: u64,
    pub last_accessed_nanos: AtomicU64,
}

impl MatchSet {
    /// Rows this result holds in memory. A live all-files view holds no rows: it pages the
    /// volume's order, so it never counts against the cache budget.
    fn retained_items(&self) -> usize {
        match self {
            MatchSet::Items(items) => items.len(),
            MatchSet::AllFiles(_) => 0,
        }
    }
}

impl QuerySession {
    fn retained_items(&self) -> usize {
        self.rows.retained_items()
    }
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
        // The registration lock stays held through the purge: a reconnecting client cannot
        // register and open a session between the last release and the cleanup that removes it.
        let mut connections = self.connections.lock().unwrap();
        let last = match connections.get_mut(owner) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => {
                connections.remove(owner);
                true
            }
            None => false,
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
        rows: MatchSet,
        highlight_terms: &[String],
        snippets: HashMap<(u32, u64), Vec<String>>,
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
        let new_items = rows.retained_items();
        while !lock.is_empty() && {
            let total_items: usize = lock.values().map(QuerySession::retained_items).sum();
            lock.len() >= self.max_sessions
                || total_items.saturating_add(new_items) > MAX_TOTAL_CACHED_ITEMS
        } {
            if let Some((oldest_id, _)) = lock.iter().min_by_key(|(id, s)| {
                (
                    s.last_accessed_nanos.load(Ordering::Relaxed),
                    s.created_nanos,
                    *id,
                )
            }) {
                let oldest_id = *oldest_id;
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
                rows,
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

    /// Fetches rows across all indexed sources for an active session.
    ///
    /// A page is only served while every source it touches still has the generation observed at
    /// query time. Serving indices from a replaced table would show another file's row or an
    /// out-of-range index, so the page is refused instead.
    pub fn get_rows_multi(
        &self,
        owner: &QueryOwner,
        session_id: u64,
        offset: usize,
        limit: usize,
        sources: &SourceTables<'_>,
    ) -> Result<Vec<FileRowData>, RowsError> {
        let lock = self.sessions.read().unwrap();
        let session = lock.get(&session_id).ok_or(RowsError::UnknownSession)?;
        if session.owner != *owner {
            return Err(RowsError::UnknownSession);
        }
        session
            .last_accessed_nanos
            .store(self.clock.now_nanos(), Ordering::Relaxed);
        page_session(session, offset, limit, sources)
    }

    #[cfg(test)]
    pub(crate) fn get_rows_multi_unchecked(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
        sources: &SourceTables<'_>,
    ) -> Result<Vec<FileRowData>, RowsError> {
        let lock = self.sessions.read().unwrap();
        let session = lock.get(&session_id).ok_or(RowsError::UnknownSession)?;
        session
            .last_accessed_nanos
            .store(self.clock.now_nanos(), Ordering::Relaxed);
        page_session(session, offset, limit, sources)
    }
}

const MAX_ROWS_PAGE: usize = 10_000;

fn page_session(
    session: &QuerySession,
    offset: usize,
    limit: usize,
    sources: &SourceTables<'_>,
) -> Result<Vec<FileRowData>, RowsError> {
    let limit = limit.min(MAX_ROWS_PAGE);
    match &session.rows {
        MatchSet::Items(items) => rows_for_items(session, items, offset, limit, sources),
        MatchSet::AllFiles(view) => rows_for_all_files(view, offset, limit, sources),
    }
}

#[derive(Clone, Copy)]
pub struct SourceTables<'a> {
    pub volumes: &'a HashMap<char, VolumeIndex>,
    pub folders: &'a HashMap<SourceId, FolderSource>,
    pub properties: &'a PropertySideTables,
}

/// Pages one live all-files view: the volume's order is the answer, filtered by the
/// exclusions the query applied. The offset counts visible rows, not raw order slots.
fn rows_for_all_files(
    view: &AllFilesView,
    offset: usize,
    limit: usize,
    sources: &SourceTables<'_>,
) -> Result<Vec<FileRowData>, RowsError> {
    let Some(volume) = sources.volumes.get(&view.drive_letter) else {
        return Err(RowsError::Stale {
            source_id: view.source_id,
        });
    };
    let mut rows = Vec::new();
    let current = view.visit_visible(volume, offset, limit, |idx| {
        if let Some(row) = volume.table_view().build_file_row(
            idx,
            &[],
            |_| Vec::new(),
            |frn, modified_ts| {
                sources
                    .properties
                    .get(view.source_id, frn, modified_ts)
                    .map(cairn_core::RecordPropertyView::to_owned)
            },
        ) {
            rows.push(row);
        }
    });
    if current {
        Ok(rows)
    } else {
        Err(RowsError::Stale {
            source_id: view.source_id,
        })
    }
}

/// Materializes one page of a session against the current volume and folder source tables.
fn rows_for_items(
    session: &QuerySession,
    items: &[SearchResultItem],
    offset: usize,
    limit: usize,
    sources: &SourceTables<'_>,
) -> Result<Vec<FileRowData>, RowsError> {
    let end = offset.saturating_add(limit).min(items.len());
    if offset >= items.len() {
        return Ok(Vec::new());
    }

    let slice = &items[offset..end];
    let mut rows = Vec::with_capacity(slice.len());

    for item in slice {
        let Some(source) = item
            .source_id
            .drive_letter()
            .and_then(|drive| sources.volumes.get(&drive).map(VolumeIndex::table_view))
            .or_else(|| {
                sources
                    .folders
                    .get(&item.source_id)
                    .map(FolderSource::table_view)
            })
        else {
            continue;
        };
        if source.generation != item.generation {
            return Err(RowsError::Stale {
                source_id: item.source_id,
            });
        }
        let Some(record) = source.records.get(item.record_idx as usize) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        if let Some(row) = source.build_file_row(
            item.record_idx,
            &session.pre_folded_terms,
            |frn| {
                session
                    .snippets
                    .get(&(item.source_id.0, frn))
                    .cloned()
                    .unwrap_or_default()
            },
            |frn, modified_ts| {
                sources
                    .properties
                    .get(item.source_id, frn, modified_ts)
                    .map(cairn_core::RecordPropertyView::to_owned)
            },
        ) {
            rows.push(row);
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::property_store::RecordPropertyData;
    use cairn_ntfs::mft::ExtractedItem;
    use cairn_ntfs::usn::UsnChange;
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
            source_id: SourceId::for_drive('C').unwrap(),
            record_idx: 0,
            generation,
        }
    }

    fn drive_c() -> SourceId {
        SourceId::for_drive('C').unwrap()
    }

    fn get_rows(
        sessions: &SessionManager,
        session_id: u64,
        offset: usize,
        limit: usize,
        volumes: &HashMap<char, VolumeIndex>,
    ) -> Result<Vec<FileRowData>, RowsError> {
        let folders = HashMap::new();
        let props = PropertySideTables::disabled();
        sessions.get_rows_multi_unchecked(
            session_id,
            offset,
            limit,
            &SourceTables {
                volumes,
                folders: &folders,
                properties: &props,
            },
        )
    }

    fn owner(pid: u32) -> QueryOwner {
        QueryOwner {
            client_pid: pid,
            user_sid: "S-1-5-21-test".to_string(),
        }
    }

    #[test]
    fn a_session_is_invisible_to_another_owner() {
        let sessions = SessionManager::new(1, DEFAULT_SESSION_TTL, SystemClock::new());
        let session_id = create_items(&sessions, owner(1), Vec::new());
        let volumes = HashMap::new();
        let folders = HashMap::new();
        let props = PropertySideTables::disabled();
        let tables = SourceTables {
            volumes: &volumes,
            folders: &folders,
            properties: &props,
        };

        assert_eq!(
            sessions.get_rows_multi(&owner(2), session_id, 0, 10, &tables),
            Err(RowsError::UnknownSession)
        );
        assert!(
            sessions
                .get_rows_multi(&owner(1), session_id, 0, 10, &tables)
                .is_ok()
        );
    }

    fn create_items(
        sessions: &SessionManager,
        owner: QueryOwner,
        items: Vec<SearchResultItem>,
    ) -> u64 {
        sessions.create_session(owner, MatchSet::Items(items), &[], HashMap::new())
    }

    fn three_file_volume() -> VolumeIndex {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(
            ["c.txt", "a.txt", "b.txt"]
                .into_iter()
                .enumerate()
                .map(|(ix, name)| ExtractedItem {
                    frn: 10 + ix as u64,
                    parent_frn: 0,
                    name: name.to_string(),
                    is_dir: false,
                    size: 10,
                    created_ts: 0,
                    modified_ts: 0,
                    flags: 0,
                    is_reparse_point: false,
                })
                .collect(),
        );
        vol
    }

    fn all_files_view(
        mut volume: VolumeIndex,
        sort_by: cairn_core::sort::SortColumn,
        sort_asc: bool,
        rules: &[String],
    ) -> (AllFilesView, HashMap<char, VolumeIndex>) {
        volume.ensure_presorted_column(sort_by);
        let drive_letter = volume.drive_letter;
        let volumes = HashMap::from([(drive_letter, volume)]);
        let exclusions = crate::sync_engine::path_exclusions::PathExclusions::new(&volumes, rules);
        let view = AllFilesView::for_volume(&volumes[&drive_letter], sort_by, sort_asc, exclusions);
        (view, volumes)
    }

    /// The default view retains no row list: it pages the volume's presorted order, and the
    /// page offset counts visible rows.
    #[test]
    fn an_all_files_view_pages_the_presorted_order_without_retaining_items() {
        let sessions = SessionManager::default();
        let vol = three_file_volume();
        let (view, volumes) = all_files_view(vol, cairn_core::sort::SortColumn::Name, true, &[]);
        let id = sessions.create_session(owner(7), MatchSet::AllFiles(view), &[], HashMap::new());

        let first = get_rows(&sessions, id, 0, 2, &volumes).expect("first page");
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].name, "a.txt");
        assert_eq!(first[1].name, "b.txt");

        let second = get_rows(&sessions, id, 2, 2, &volumes).expect("second page");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].name, "c.txt");
    }

    /// A rename that can move a record across an exclusion verdict makes open pages stale;
    /// an ordinary rename keeps them readable.
    #[test]
    fn a_view_refuses_pages_after_a_rule_relevant_rename() {
        let sessions = SessionManager::default();
        let vol = three_file_volume();
        let (view, mut volumes) = all_files_view(
            vol,
            cairn_core::sort::SortColumn::Name,
            true,
            &["$Recycle.Bin".to_string()],
        );
        let id = sessions.create_session(owner(7), MatchSet::AllFiles(view), &[], HashMap::new());

        volumes
            .get_mut(&'C')
            .expect("volume")
            .apply_usn_changes([UsnChange::Renamed {
                frn: 10,
                parent_frn: 0,
                name: "renamed.txt".to_string(),
                is_dir: false,
                is_new_name: true,
            }]);
        assert!(
            get_rows(&sessions, id, 0, 2, &volumes).is_ok(),
            "an ordinary rename keeps the view"
        );

        volumes
            .get_mut(&'C')
            .expect("volume")
            .apply_usn_changes([UsnChange::Renamed {
                frn: 10,
                parent_frn: 0,
                name: "$Recycle.Bin".to_string(),
                is_dir: false,
                is_new_name: true,
            }]);
        assert_eq!(
            get_rows(&sessions, id, 0, 2, &volumes),
            Err(RowsError::Stale {
                source_id: drive_c()
            })
        );
    }

    #[test]
    fn a_descending_all_files_view_pages_the_reversed_order() {
        let sessions = SessionManager::default();
        let vol = three_file_volume();
        let (view, volumes) = all_files_view(vol, cairn_core::sort::SortColumn::Name, false, &[]);
        let id = sessions.create_session(owner(7), MatchSet::AllFiles(view), &[], HashMap::new());
        let page = get_rows(&sessions, id, 0, 2, &volumes).expect("first page");
        assert_eq!(page[0].name, "c.txt");
        assert_eq!(page[1].name, "b.txt");
    }

    /// A rebuilt order makes the session's offsets meaningless; the page is refused instead
    /// of naming a different row.
    #[test]
    fn an_all_files_view_refuses_pages_after_a_rebuild() {
        let sessions = SessionManager::default();
        let vol = three_file_volume();
        let (view, volumes) = all_files_view(vol, cairn_core::sort::SortColumn::Name, true, &[]);
        let id = sessions.create_session(owner(7), MatchSet::AllFiles(view), &[], HashMap::new());

        let mut volumes = volumes;
        volumes
            .get_mut(&'C')
            .expect("volume")
            .ensure_presorted_column(cairn_core::sort::SortColumn::Size);
        assert_eq!(
            get_rows(&sessions, id, 0, 2, &volumes),
            Err(RowsError::Stale {
                source_id: drive_c()
            })
        );
    }

    #[test]
    fn unchanged_volumes_serve_pages() {
        let sessions = SessionManager::default();
        let vol = volume_with("notes.txt");
        let session_id = create_items(&sessions, owner(7), vec![item(vol.generation())]);
        let volumes = HashMap::from([('C', vol)]);

        let rows = get_rows(&sessions, session_id, 0, 10, &volumes)
            .expect("a valid session must page normally");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "notes.txt");
    }

    #[test]
    fn a_rebuilt_volume_refuses_stale_pages_instead_of_serving_other_rows() {
        let sessions = SessionManager::default();
        let vol = volume_with("notes.txt");
        let session_id = create_items(&sessions, owner(7), vec![item(vol.generation())]);
        let mut volumes = HashMap::from([('C', vol)]);

        // Replacing the table (rescan, reload, compaction) invalidates every cached record index.
        volumes.get_mut(&'C').unwrap().clear();
        let error = get_rows(&sessions, session_id, 0, 10, &volumes)
            .expect_err("a stale page must not be served");
        assert_eq!(
            error,
            RowsError::Stale {
                source_id: drive_c()
            }
        );
    }

    #[test]
    fn an_evicted_session_is_distinguishable_from_a_stale_one() {
        let sessions = SessionManager::new(1, DEFAULT_SESSION_TTL, SystemClock::new());
        let vol = volume_with("notes.txt");
        let volumes = HashMap::from([('C', vol)]);
        let first = create_items(&sessions, owner(7), vec![item(0)]);
        create_items(&sessions, owner(7), Vec::new());
        assert_eq!(
            get_rows(&sessions, first, 0, 10, &volumes),
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
        let generation = vol.generation();
        let volumes = HashMap::from([('C', vol)]);

        let s1 = create_items(&sessions, owner(7), vec![item(generation)]);
        clock.advance(Duration::from_millis(20));
        let s2 = create_items(&sessions, owner(7), vec![item(generation)]);

        clock.advance(Duration::from_millis(20));
        assert!(get_rows(&sessions, s1, 0, 10, &volumes).is_ok());

        let s3 = create_items(&sessions, owner(7), vec![item(generation)]);

        assert!(get_rows(&sessions, s1, 0, 10, &volumes).is_ok());
        assert_eq!(
            get_rows(&sessions, s2, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
        assert!(get_rows(&sessions, s3, 0, 10, &volumes).is_ok());
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
        let generation = vol.generation();
        let volumes = HashMap::from([('C', vol)]);

        let s1 = create_items(&sessions, owner(7), vec![item(generation)]);
        clock.advance(Duration::from_secs(299));
        let _s2 = create_items(&sessions, owner(7), vec![item(generation)]);
        assert!(
            get_rows(&sessions, s1, 0, 10, &volumes).is_ok(),
            "session accessed before TTL must stay live"
        );

        // Accessing s1 reset its idle timer; advance full 300s to cross expiry.
        clock.advance(Duration::from_secs(300));
        let _s3 = create_items(&sessions, owner(7), vec![item(generation)]);
        assert_eq!(
            get_rows(&sessions, s1, 0, 10, &volumes),
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
        let generation = vol.generation();
        let volumes = std::sync::Arc::new(HashMap::from([('C', vol)]));

        let s1 = create_items(&sessions, owner(7), vec![item(generation)]);
        clock.advance(Duration::from_secs(1));
        let s2 = create_items(&sessions, owner(7), vec![item(generation)]);
        clock.advance(Duration::from_secs(1));

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let s = sessions.clone();
                let v = volumes.clone();
                scope.spawn(move || {
                    for _ in 0..25 {
                        assert!(get_rows(&s, s1, 0, 10, &v).is_ok());
                    }
                });
            }
        });

        let _s3 = create_items(&sessions, owner(7), vec![item(generation)]);
        assert!(get_rows(&sessions, s1, 0, 10, &volumes).is_ok());
        assert_eq!(
            get_rows(&sessions, s2, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
    }

    #[test]
    fn begin_query_cancels_superseded_queries_and_cancel_query_signals_active_token() {
        let sessions = SessionManager::default();
        let owner = QueryOwner::default();

        let q1 = sessions.begin_query(&owner, 1);
        assert!(!q1.is_cancelled());

        let q2 = sessions.begin_query(&owner, 2);
        assert!(q1.is_cancelled());
        assert!(!q2.is_cancelled());

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
        let my_session = create_items(&sessions, mine.clone(), vec![item(generation)]);
        let other_session = create_items(&sessions, other.clone(), vec![item(generation)]);

        sessions.release_client(&mine);
        assert!(
            get_rows(&sessions, my_session, 0, 10, &volumes).is_ok(),
            "a remaining connection keeps the session"
        );

        sessions.release_client(&mine);
        assert_eq!(
            get_rows(&sessions, my_session, 0, 10, &volumes),
            Err(RowsError::UnknownSession)
        );
        assert!(
            get_rows(&sessions, other_session, 0, 10, &volumes).is_ok(),
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

    #[test]
    fn folder_source_items_preserve_snippets_and_properties() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("docs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("report.mp4"), "clip").unwrap();

        let source_id = SourceId(1001);
        let mut source = FolderSource::new(source_id, &root).expect("folder source");
        source.rescan(&[]).expect("rescan folder source");
        let (idx, frn, modified_ts) = source
            .records()
            .iter()
            .enumerate()
            .find_map(|(i, rec)| {
                let name = source.arena().get_name(rec.name_offset, rec.name_len);
                (name == "report.mp4").then_some((i as u32, rec.frn, rec.modified_ts))
            })
            .expect("indexed report.mp4");
        let generation = source.generation();

        let mut props = PropertySideTables::new(true);
        let expected_props = RecordPropertyData {
            length_ms: Some(12_500),
            author: Some("Cairn".to_string()),
            tags: Some("demo".to_string()),
            dimensions: Some((1920, 1080)),
            framerate_mfps: Some(60_000),
        };
        assert!(props.upsert(source_id, frn, &expected_props, modified_ts));

        let mut snippets = HashMap::new();
        snippets.insert(
            (source_id.0, frn),
            vec!["first snippet".to_string(), "second snippet".to_string()],
        );

        let sessions = SessionManager::default();
        let session_id = sessions.create_session(
            owner(11),
            MatchSet::Items(vec![SearchResultItem {
                source_id,
                record_idx: idx,
                generation,
            }]),
            &["report".to_string()],
            snippets,
        );

        let volumes = HashMap::new();
        let mut folders = HashMap::from([(source_id, source)]);
        let rows = sessions
            .get_rows_multi_unchecked(
                session_id,
                0,
                10,
                &SourceTables {
                    volumes: &volumes,
                    folders: &folders,
                    properties: &props,
                },
            )
            .expect("folder source row");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "report.mp4");
        assert_eq!(rows[0].snippets, vec!["first snippet", "second snippet"]);
        assert_eq!(rows[0].properties.as_ref(), Some(&expected_props));

        // Rescanning advances folder generation -> stale error names the folder source id.
        folders.get_mut(&source_id).unwrap().rescan(&[]).unwrap();
        assert_eq!(
            sessions.get_rows_multi_unchecked(
                session_id,
                0,
                10,
                &SourceTables {
                    volumes: &volumes,
                    folders: &folders,
                    properties: &props,
                },
            ),
            Err(RowsError::Stale { source_id })
        );
    }

    fn populated_volume(
        count: usize,
        excluded_dir_items: usize,
        delete_every: usize,
    ) -> VolumeIndex {
        let mut items = Vec::with_capacity(count + excluded_dir_items + 2);
        items.push(ExtractedItem {
            frn: 1,
            parent_frn: 0,
            name: "$Recycle.Bin".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        });
        items.push(ExtractedItem {
            frn: 2,
            parent_frn: 0,
            name: "skip_full".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        });
        for i in 0..excluded_dir_items {
            items.push(ExtractedItem {
                frn: 100 + i as u64,
                parent_frn: 1,
                name: format!("b_trash_{i:05}.tmp"),
                is_dir: false,
                size: 5,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            });
        }
        let base_frn = 10_000u64;
        for i in 0..count {
            let parent_frn = if i % 19 == 0 { 2 } else { 0 };
            items.push(ExtractedItem {
                frn: base_frn + i as u64,
                parent_frn,
                name: format!("file_{i:06}.txt"),
                is_dir: false,
                size: (i as u64) + 1,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            });
        }

        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(items);
        if delete_every > 0 {
            let deletions: Vec<UsnChange> = (0..count)
                .filter(|i| i % delete_every == 0)
                .map(|i| UsnChange::Deleted {
                    frn: base_frn + i as u64,
                })
                .collect();
            vol.apply_usn_changes(deletions);
        }
        vol
    }

    #[test]
    fn all_files_view_prefix_paging_matches_full_scan_across_block_boundaries() {
        let sessions = SessionManager::default();
        let props = PropertySideTables::disabled();
        let folders = HashMap::new();

        let scenarios = [
            (0usize, vec![]),
            (0usize, vec!["$Recycle.Bin".to_string()]),
            (37usize, vec!["$Recycle.Bin".to_string()]),
            (53usize, vec![r"C:\skip_full".to_string()]),
        ];
        for (delete_every, rules) in &scenarios {
            let delete_every = *delete_every;
            for sort_asc in [true, false] {
                let vol = populated_volume(3_500, 400, delete_every);
                let (view, volumes) =
                    all_files_view(vol, cairn_core::sort::SortColumn::Name, sort_asc, rules);
                let reference: Vec<String> = {
                    let mut names = Vec::new();
                    assert!(view.visit_visible(&volumes[&'C'], 0, usize::MAX, |idx| {
                        let rec = &volumes[&'C'].records()[idx as usize];
                        names.push(
                            volumes[&'C']
                                .arena()
                                .get_name(rec.name_offset, rec.name_len)
                                .to_string(),
                        );
                    }));
                    names
                };

                let session_id = sessions.create_session(
                    owner(12),
                    MatchSet::AllFiles(view),
                    &[],
                    HashMap::new(),
                );
                let tables = SourceTables {
                    volumes: &volumes,
                    folders: &folders,
                    properties: &props,
                };

                let total = reference.len();
                let offsets = [
                    0,
                    1,
                    1023,
                    1024,
                    1025,
                    2047,
                    2048,
                    2049,
                    total.saturating_sub(250),
                    total.saturating_sub(1),
                    total,
                    total + 500,
                ];
                let limits = [0, 1, 17, 200, 1_500];
                for &offset in &offsets {
                    for &limit in &limits {
                        let page = sessions
                            .get_rows_multi_unchecked(session_id, offset, limit, &tables)
                            .expect("page should succeed");
                        let actual: Vec<&str> = page.iter().map(|row| row.name.as_str()).collect();
                        let expected: Vec<&str> = if offset >= total {
                            Vec::new()
                        } else {
                            let end = offset.saturating_add(limit).min(total);
                            reference[offset..end].iter().map(String::as_str).collect()
                        };
                        assert_eq!(
                            actual, expected,
                            "mismatch for delete_every={delete_every} rules={rules:?} sort_asc={sort_asc} offset={offset} limit={limit}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn all_files_view_deep_offset_paging_serves_large_volumes() {
        let sessions = SessionManager::default();
        let props = PropertySideTables::disabled();
        let folders = HashMap::new();
        let rules = vec!["$Recycle.Bin".to_string()];

        for sort_asc in [true, false] {
            let vol = populated_volume(120_000, 2_000, 0);
            let (view, volumes) =
                all_files_view(vol, cairn_core::sort::SortColumn::Name, sort_asc, &rules);
            let session_id =
                sessions.create_session(owner(13), MatchSet::AllFiles(view), &[], HashMap::new());
            let tables = SourceTables {
                volumes: &volumes,
                folders: &folders,
                properties: &props,
            };

            let started = std::time::Instant::now();
            for step in 0..50usize {
                let offset = 50_000 + step * 1_200;
                let page = sessions
                    .get_rows_multi_unchecked(session_id, offset, 200, &tables)
                    .expect("deep page");
                assert_eq!(page.len(), 200);
            }
            let elapsed = started.elapsed();
            assert!(
                elapsed < Duration::from_secs(5),
                "50 deep pages took unexpectedly long: {elapsed:?}"
            );
        }
    }
}

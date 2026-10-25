// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! User-privilege indexer used when the installed LocalSystem service is unavailable.
//!
//! A standard account can only read a subset of the volume, so this indexer walks the directory
//! tree instead of reading `$MFT`, skipping reparse points and whatever the account is refused.
//! The result is a snapshot of the accessible tree: fewer rows than the service indexes, but the
//! window is fully usable without a UAC prompt.
//!
//! The indexer runs as a child of the desktop app on [`cairn_protocol::USER_PIPE_NAME`], so it can
//! never be mistaken for, or compete with, the installed service. It exits when its parent does.

use crate::config::ServiceConfig;
use crate::filetime::system_time_to_filetime;
use crate::pipe_server::PipeServer;
use crate::session::SessionManager;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::volume::enumerate_fixed_drives;
use cairn_protocol::message::USER_PIPE_NAME;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

/// Items collected before a batch is handed to the engine. A batch bounds both the latency of the
/// first rendered rows and how long the index write lock is held.
const SCAN_BATCH_ITEMS: usize = 4096;

/// Top-level directories of a system drive that a standard account gains nothing by walking. The
/// LocalSystem service indexes them; the local scan exists to show the user's own files quickly.
const SYSTEM_ROOT_DIRS: &[&str] = &[
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "$Recycle.Bin",
    "System Volume Information",
    "Recovery",
    "PerfLogs",
    "Windows.old",
    "$WinREAgent",
    "Documents and Settings",
    "Config.Msi",
];

/// Everything the user-mode indexer writes. It owns no service data, so nothing here needs the
/// SYSTEM/Administrators descriptor of SPEC SET-02.
#[derive(Clone, Debug)]
pub struct UserModeLayout {
    pub root: PathBuf,
    pub index_dir: PathBuf,
    pub content_dir: PathBuf,
    pub config_path: PathBuf,
}

impl UserModeLayout {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            index_dir: root.join("index"),
            content_dir: root.join("content"),
            config_path: root.join("settings.toml"),
        }
    }

    pub fn create(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.index_dir)?;
        std::fs::create_dir_all(&self.content_dir)?;
        Ok(())
    }
}

/// `%LOCALAPPDATA%\Cairn\User`, a per-account location the account can always read and write.
pub fn default_user_data_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| std::env::temp_dir().join("Cairn"))
        .join("Cairn")
        .join("User")
}

/// The drive roots a local scan covers: every fixed volume, as `(drive letter, root path)`.
pub fn accessible_scan_roots() -> Vec<(char, PathBuf)> {
    enumerate_fixed_drives()
        .into_iter()
        .map(|drive| (drive, PathBuf::from(format!("{drive}:\\"))))
        .collect()
}

enum ScanEvent {
    Items(Vec<ExtractedItem>),
    Finished,
}

/// A running user-mode indexer.
pub struct UserModeRuntime {
    pub engine: Arc<MultiVolumeEngine>,
    pub sessions: Arc<SessionManager>,
    pub pipe: PipeServer,
    pub layout: UserModeLayout,
}

impl UserModeRuntime {
    /// Builds the indexer for the current account and starts scanning in the background.
    pub fn build() -> io::Result<Self> {
        Self::build_in(&UserModeLayout::new(&default_user_data_root()), None)
    }

    /// [`Self::build`] with an explicit data root and scan roots; tests use a temporary tree.
    pub fn build_in(
        layout: &UserModeLayout,
        scan_roots: Option<Vec<(char, PathBuf)>>,
    ) -> io::Result<Self> {
        layout.create()?;

        let config = if layout.config_path.exists() {
            ServiceConfig::load(&layout.config_path).unwrap_or_else(|error| {
                warn!(
                    "{} could not be parsed ({error}); using defaults",
                    layout.config_path.display()
                );
                ServiceConfig::default()
            })
        } else {
            ServiceConfig::default()
        };

        let engine = Arc::new(
            MultiVolumeEngine::with_paths(
                &layout.index_dir,
                &layout.content_dir,
                &layout.config_path,
                Vec::new(),
                config,
            )
            .with_service_data_root(&layout.root)
            .with_plain_config_persistence(),
        );

        let roots = scan_roots.unwrap_or_else(accessible_scan_roots);
        for (drive, _) in &roots {
            engine.add_volume(VolumeIndex::new(*drive));
        }

        let sessions = Arc::new(SessionManager::default());
        let pipe = PipeServer::with_pipe_name(engine.clone(), sessions.clone(), USER_PIPE_NAME);

        start_scans(engine.clone(), roots);

        Ok(Self {
            engine,
            sessions,
            pipe,
            layout: layout.clone(),
        })
    }

    /// Serves the user pipe until the parent process exits.
    pub async fn run(self, shutdown: impl std::future::Future<Output = ()>) {
        info!(
            "user-mode indexer serving {} on {}",
            self.layout.root.display(),
            USER_PIPE_NAME
        );
        tokio::select! {
            result = self.pipe.run() => {
                if let Err(error) = result {
                    tracing::error!("user-mode pipe server stopped: {error}");
                }
            },
            _ = shutdown => {
                info!("parent process left; user-mode indexer shutting down");
            }
        }
    }
}

/// Walks each root on its own thread and appends batches to the engine on a single apply thread,
/// so rows appear while the walk is still running instead of after it finishes.
fn start_scans(engine: Arc<MultiVolumeEngine>, roots: Vec<(char, PathBuf)>) {
    let (tx, rx) = std::sync::mpsc::channel::<(char, ScanEvent)>();

    for (drive, root) in roots {
        let tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("cairn-user-scan-{drive}"))
            .spawn(move || {
                info!("scanning accessible tree of {drive}: at {}", root.display());
                let visited = scan_tree(&root, |batch| {
                    let _ = tx.send((drive, ScanEvent::Items(batch)));
                });
                info!("finished scanning {drive}: ({visited} entries)");
                let _ = tx.send((drive, ScanEvent::Finished));
            });
        if let Err(error) = spawned {
            warn!("scan thread for {drive}: could not start: {error}");
            // The volume must not stay in a loading state nobody will ever finish.
            engine.update_volume(drive, VolumeIndex::finalize_snapshot_scan);
        }
    }
    drop(tx);

    let applied = std::thread::Builder::new()
        .name("cairn-user-scan-apply".to_string())
        .spawn(move || {
            let mut indexed = std::collections::HashMap::<char, usize>::new();
            for (drive, event) in rx {
                match event {
                    ScanEvent::Items(items) => {
                        let count = items.len();
                        *indexed.entry(drive).or_default() += count;
                        engine.update_volume(drive, |volume| volume.append_items(items));
                    }
                    ScanEvent::Finished => {
                        let total = indexed.remove(&drive).unwrap_or(0);
                        engine.update_volume(drive, VolumeIndex::finalize_snapshot_scan);
                        info!("user index for {drive}: holds {total} entries");
                        if let Err(error) = engine.save_all_indexes() {
                            warn!("could not save user-mode volume index: {error}");
                        }
                    }
                }
            }
        });
    if let Err(error) = applied {
        warn!("user-mode index apply thread could not start: {error}");
    }
}

/// Walks `root` depth-first, emitting batches through `on_batch`; returns the number of entries
/// collected. Directories are emitted before their children, which is the order
/// [`VolumeIndex::append_items`] needs to link parents.
pub fn scan_tree(root: &Path, mut on_batch: impl FnMut(Vec<ExtractedItem>)) -> usize {
    let mut batch = Vec::with_capacity(SCAN_BATCH_ITEMS);
    let mut stack: Vec<(PathBuf, u64)> = vec![(root.to_path_buf(), 0)];
    let mut next_frn: u64 = 1;
    let mut visited = 0usize;

    while let Some((directory, directory_frn)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let at_scan_root = directory == root;

        let mut children = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.is_empty() || (at_scan_root && is_system_root_dir(&name)) {
                continue;
            }
            // `DirEntry::metadata` resolves a junction to its target on some Windows versions, which
            // would hide the reparse attribute and let the walk descend into a cycle.
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let attributes = dos_attributes(&metadata);
            let is_reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            let is_dir = is_directory(attributes, &file_type);

            let frn = next_frn;
            next_frn += 1;
            visited += 1;

            batch.push(ExtractedItem {
                frn,
                parent_frn: directory_frn,
                name,
                is_dir,
                size: if is_dir { 0 } else { metadata.len() },
                created_ts: system_time_to_filetime(metadata.created()),
                modified_ts: system_time_to_filetime(metadata.modified()),
                flags: core_flags(attributes, is_dir),
                is_reparse_point: is_reparse,
            });

            if is_dir && !is_reparse {
                children.push((entry.path(), frn));
            }
            if batch.len() >= SCAN_BATCH_ITEMS {
                on_batch(std::mem::take(&mut batch));
            }
        }

        // Reverse the children so the stack pops them in the order the directory listed them.
        stack.extend(children.into_iter().rev());
    }

    if !batch.is_empty() {
        on_batch(batch);
    }
    visited
}

fn is_system_root_dir(name: &str) -> bool {
    SYSTEM_ROOT_DIRS
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(name))
}

const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x0000_0020;
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;

fn core_flags(attributes: u32, is_dir: bool) -> u16 {
    let mut flags = 0u16;
    if is_dir {
        flags |= cairn_core::FLAG_DIRECTORY;
    }
    if attributes & FILE_ATTRIBUTE_READONLY != 0 {
        flags |= cairn_core::FLAG_READONLY;
    }
    if attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
        flags |= cairn_core::FLAG_HIDDEN;
    }
    if attributes & FILE_ATTRIBUTE_SYSTEM != 0 {
        flags |= cairn_core::FLAG_SYSTEM;
    }
    if attributes & FILE_ATTRIBUTE_ARCHIVE != 0 {
        flags |= cairn_core::FLAG_ARCHIVE;
    }
    if attributes & FILE_ATTRIBUTE_COMPRESSED != 0 {
        flags |= cairn_core::FLAG_COMPRESSED;
    }
    if attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0 {
        flags |= cairn_core::FLAG_SPARSE;
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        flags |= cairn_core::FLAG_REPARSE_POINT;
    }
    flags
}

#[cfg(windows)]
fn dos_attributes(metadata: &std::fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes()
}

#[cfg(not(windows))]
fn dos_attributes(_metadata: &std::fs::Metadata) -> u32 {
    0
}

/// A junction reports as a directory through `FileType`, and its attribute word is the only place
/// that also says it is a reparse point. Callers pair this with [`FILE_ATTRIBUTE_REPARSE_POINT`]
/// before descending.
fn is_directory(attributes: u32, file_type: &std::fs::FileType) -> bool {
    #[cfg(windows)]
    {
        let _ = file_type;
        attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        let _ = attributes;
        file_type.is_dir()
    }
}

/// Waits until `pid` exits, so the indexer's lifetime is the window's. Returns immediately when
/// the process is already gone, including a PID that was reused by a process this call cannot open.
pub async fn wait_for_parent_exit(pid: u32) {
    let _ = tokio::task::spawn_blocking(move || wait_for_process_exit(pid)).await;
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE,
    };

    // SAFETY: the handle is opened only to wait on the process object and is closed exactly once.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return;
        };
        let _ = WaitForSingleObject(handle, INFINITE);
        let _ = CloseHandle(handle);
    }
}

#[cfg(not(windows))]
fn wait_for_process_exit(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::engine::{execute_query, CancellationToken};
    use cairn_core::sort::SortColumn;
    use cairn_core::syntax::parse_query;
    use cairn_protocol::status::VolumeState;
    use cairn_protocol::QueryDefaults;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent directory");
        }
        std::fs::write(path, contents).expect("fixture file");
    }

    #[test]
    fn the_local_scan_emits_directories_before_their_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(&temp.path().join("alpha").join("report.txt"), "report");
        write(&temp.path().join("root.txt"), "root");

        let mut items = Vec::new();
        let visited = scan_tree(temp.path(), |batch| items.extend(batch));

        assert_eq!(visited, items.len());
        assert_eq!(items.len(), 3, "alpha, report.txt and root.txt");

        let alpha = items.iter().find(|item| item.name == "alpha").expect("dir");
        assert!(alpha.is_dir);
        let report = items
            .iter()
            .find(|item| item.name == "report.txt")
            .expect("file");
        assert_eq!(
            report.parent_frn, alpha.frn,
            "a child must point at the directory record that precedes it"
        );
        let root_file = items
            .iter()
            .find(|item| item.name == "root.txt")
            .expect("file");
        assert_eq!(
            root_file.parent_frn, 0,
            "a top-level entry has no record as parent"
        );
        assert!(root_file.size > 0);
        assert!(
            root_file.modified_ts > 100_000_000_000_000_000,
            "timestamps are FILETIME, not unix seconds"
        );
    }

    #[test]
    fn the_scan_skips_a_reparse_point_instead_of_descending_into_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        // The target lives outside the scanned root, so a file found inside it proves the walk
        // followed the link rather than reaching the same directory another way.
        let target = temp.path().join("target");
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).expect("scan root");
        write(&target.join("inside.txt"), "inside");
        let link = root.join("link");

        #[cfg(windows)]
        let created = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(&target, &link).is_ok();

        if !created {
            eprintln!("link could not be created; reparse check not exercised");
            return;
        }

        let mut items = Vec::new();
        scan_tree(&root, |batch| items.extend(batch));

        assert!(
            items.iter().all(|item| item.name != "inside.txt"),
            "the walk descended through a reparse point"
        );
        assert!(
            items
                .iter()
                .any(|item| item.name == "link" && item.is_reparse_point),
            "the link itself is visible, with its reparse attribute"
        );
    }

    #[test]
    fn batches_link_across_ingestion_boundaries_and_resolve_to_the_same_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(&temp.path().join("one").join("two").join("needle.txt"), "x");

        let mut batches = Vec::new();
        scan_tree(temp.path(), |batch| batches.push(batch));

        let mut volume = VolumeIndex::new('C');
        for batch in batches {
            volume.append_items(batch);
        }
        volume.finalize_snapshot_scan();

        assert_eq!(volume.status, VolumeState::SnapshotMode);
        assert!(!volume.usn_enabled, "a directory snapshot has no journal");

        let cancel = CancellationToken::new();
        let matches = execute_query(
            volume.records(),
            &volume.arena,
            &volume.path_cache,
            volume.drive_letter,
            &parse_query("needle"),
            &cancel,
        );
        assert_eq!(matches.len(), 1);

        let full = volume
            .path_cache
            .get_full_path(&volume.records, &volume.arena, 'C', matches[0]);
        assert_eq!(full, r"C:\one\two\needle.txt");
    }

    #[test]
    fn a_user_mode_engine_serves_a_scanned_tree_and_writes_its_own_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = UserModeLayout::new(&temp.path().join("user"));
        let tree = temp.path().join("tree");
        write(&tree.join("docs").join("budget.xlsx"), "xlsx");

        let runtime = UserModeRuntime::build_in(&layout, Some(vec![('C', tree)])).expect("runtime");

        // The scan runs on its own threads; wait for the volume about rather than assuming a timing.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let summaries = runtime.engine.get_volume_summaries();
            if summaries
                .iter()
                .all(|summary| summary.status == VolumeState::SnapshotMode)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "user scan did not finish: {summaries:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let cancel = CancellationToken::new();
        let (results, total, ..) = runtime.engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, results.len());
        assert!(
            total >= 2,
            "the scanned tree must be visible in the all-files view"
        );

        let config = runtime.engine.get_config();
        let toml = toml::to_string_pretty(&config).expect("config");
        runtime
            .engine
            .apply_config_toml(&toml)
            .expect("a user-mode indexer persists settings as the current account");
        assert!(layout.config_path.exists());
    }
}

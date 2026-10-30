// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Progressive filesystem scan that streams a Treemap tree as it grows.
//!
//! The scan reports changes, not snapshots: one [`TreemapScanUpdate::Listing`]
//! of the root, then [`TreemapScanUpdate::Progress`] updates that refine it in
//! place. Every finished subtree crosses the channel exactly once, so a scan
//! never copies the tree it has already built.

use super::tree::TreemapTreeNode;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::Metadata;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

/// How often partial sizes and progressive counters are refreshed while subtrees are scanning.
const PARTIAL_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on concurrent subtree workers.
const MAX_WORKERS: usize = 6;

/// What one scan event changes in the tree being assembled.
#[derive(Clone, Debug, PartialEq)]
pub enum TreemapScanUpdate {
    /// The root listing: direct files with their sizes and every top-level
    /// directory as an empty placeholder. Always the first update; it
    /// replaces any tree assembled before.
    Listing(TreemapTreeNode),
    /// Refines the listing in place.
    Progress {
        /// Top-level subtrees finished since the previous update: aggregated,
        /// sorted and category-resolved. Each root keeps the id of the
        /// placeholder it replaces.
        completed: Vec<TreemapTreeNode>,
        /// Bytes found so far below every unfinished top-level directory, by
        /// placeholder id. Each value is a monotonic lower bound of the
        /// directory's final size.
        partial_sizes: Vec<(u64, u64)>,
    },
}

impl TreemapScanUpdate {
    /// Folds this update into `tree`, the tree assembled from the scan's
    /// earlier updates. Only the top level is touched: placeholders take
    /// their partial size or their finished subtree, then the root is summed
    /// and re-sorted.
    pub fn apply_to(self, tree: &mut TreemapTreeNode) {
        let (completed, partial_sizes) = match self {
            Self::Listing(listing) => {
                *tree = listing;
                return;
            }
            Self::Progress {
                completed,
                partial_sizes,
            } => (completed, partial_sizes),
        };

        let positions: HashMap<u64, usize> = tree
            .children
            .iter()
            .enumerate()
            .filter(|(_, child)| child.is_dir)
            .map(|(position, child)| (child.id, position))
            .collect();
        for subtree in completed {
            if let Some(&position) = positions.get(&subtree.id) {
                tree.children[position] = subtree;
            }
        }
        for (id, bytes) in partial_sizes {
            if let Some(&position) = positions.get(&id) {
                tree.children[position].size = bytes;
            }
        }

        tree.size = tree
            .children
            .iter()
            .fold(0u64, |total, child| total.saturating_add(child.size));
        tree.descendant_items = tree
            .children
            .iter()
            .map(|child| 1 + child.descendant_count())
            .sum();
        tree.children
            .sort_by_key(|child| std::cmp::Reverse(child.size));
    }
}

/// One event of a progressive scan.
#[derive(Clone, Debug, PartialEq)]
pub struct TreemapScanEvent {
    pub update: TreemapScanUpdate,
    /// Files discovered so far, at every depth.
    pub scanned_files: usize,
    /// Directories discovered so far, at every depth.
    pub scanned_dirs: usize,
    /// Set on the last event only. A cancelled scan never sends it.
    pub is_finished: bool,
    /// The root itself could not be read. This is distinct from an empty directory and must not
    /// be rendered as a legitimate "0 items" result.
    pub root_unreadable: bool,
}

fn metadata_modified_ts(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Detects junctions, mount points and symlinks from entry metadata, which
/// does not follow links, so the scan can skip them without another syscall.
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Reads the direct entries of `root_path` without descending: files carry
/// their size, directories are empty placeholders. One metadata lookup per
/// entry (cached by the directory enumeration on Windows); reparse points are
/// skipped and an unreadable root yields an empty listing.
fn scan_listing(root_path: &Path) -> (TreemapTreeNode, bool) {
    let name = root_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root_path.to_string_lossy().into_owned());
    let mut root = TreemapTreeNode::new_dir(1, name, 0);
    let Ok(entries) = std::fs::read_dir(root_path) else {
        return (root, true);
    };

    let mut next_id = 100u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata_is_reparse_point(&metadata) {
            continue;
        }
        next_id += 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        let modified_ts = metadata_modified_ts(&metadata);
        root.children.push(if metadata.is_dir() {
            TreemapTreeNode::new_dir(next_id, name, modified_ts)
        } else {
            TreemapTreeNode::new_file(next_id, name, metadata.len(), modified_ts)
        });
    }
    root.compute_aggregated_sizes();
    root.sort_recursive();
    (root, false)
}

/// One top-level directory tracked by the scan coordinator.
struct TopLevel {
    name: String,
    path: String,
    modified_ts: i64,
    /// Id of the listing placeholder; the finished subtree inherits it so the
    /// tile is refined in place instead of being replaced by a new identity.
    placeholder_id: u64,
    /// Bytes discovered under this directory so far. A monotonic lower bound
    /// of its final recursive size: every discovered file is added once.
    partial_bytes: AtomicU64,
}

/// Shared state handed to one subtree worker.
struct WorkerContext<'a> {
    cancellation: &'a AtomicBool,
    next_id: &'a AtomicU64,
    scanned_files: &'a AtomicUsize,
    scanned_dirs: &'a AtomicUsize,
    partial_bytes: &'a AtomicU64,
}

impl WorkerContext<'_> {
    fn cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Relaxed)
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn count_dir(&self) {
        self.scanned_dirs.fetch_add(1, Ordering::Relaxed);
    }

    fn count_file(&self, size: u64) {
        self.scanned_files.fetch_add(1, Ordering::Relaxed);
        self.partial_bytes.fetch_add(size, Ordering::Relaxed);
    }
}

/// Sums the recursive size and item count below a depth-capped directory while
/// feeding the same files into the top-level partial counter. The count
/// excludes the leaf itself and includes every descendant file and directory.
/// `None` when cancelled.
fn scan_leaf_recursive_size(path: &Path, ctx: &WorkerContext<'_>) -> Option<(u64, usize)> {
    let mut total = 0u64;
    let mut items = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        if ctx.cancelled() {
            return None;
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if ctx.cancelled() {
                return None;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata_is_reparse_point(&metadata) {
                continue;
            }
            items = items.saturating_add(1);
            if metadata.is_dir() {
                ctx.count_dir();
                stack.push(entry.path());
            } else {
                let size = metadata.len();
                ctx.count_file(size);
                total = total.saturating_add(size);
            }
        }
    }
    Some((total, items))
}

/// Scans one subtree down to `remaining_depth`. Subdirectories are walked in
/// parallel through rayon's work-stealing pool, so a top-level directory that
/// is larger than the rest of the drive together no longer serializes the
/// tail of the scan on a single worker. Only the shape is built here; the
/// worker finishes the whole top-level subtree once. Depth-capped leaves
/// already carry their recursive size, which aggregation preserves. `None`
/// when cancelled, so the coordinator discards the partial tree.
fn scan_subtree(
    path: &Path,
    name: &str,
    modified_ts: i64,
    remaining_depth: usize,
    ctx: &WorkerContext<'_>,
) -> Option<TreemapTreeNode> {
    if ctx.cancelled() {
        return None;
    }
    let mut node = TreemapTreeNode::new_dir(ctx.next_id(), name.to_string(), modified_ts);
    let Ok(entries) = std::fs::read_dir(path) else {
        return Some(node);
    };

    let mut directories = Vec::new();
    for entry in entries.flatten() {
        if ctx.cancelled() {
            return None;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata_is_reparse_point(&metadata) {
            continue;
        }
        let child_name = entry.file_name().to_string_lossy().into_owned();
        let child_ts = metadata_modified_ts(&metadata);
        if metadata.is_dir() {
            ctx.count_dir();
            directories.push((entry.path(), child_name, child_ts));
        } else {
            let size = metadata.len();
            ctx.count_file(size);
            node.children.push(TreemapTreeNode::new_file(
                ctx.next_id(),
                child_name,
                size,
                child_ts,
            ));
        }
    }

    // Order is preserved by `collect` on the parallel iterator, so the final
    // tree is byte-identical to a sequential walk.
    let children: Vec<Option<TreemapTreeNode>> = directories
        .into_par_iter()
        .map(|(child_path, child_name, child_ts)| {
            if ctx.cancelled() {
                return None;
            }
            if remaining_depth > 0 {
                scan_subtree(&child_path, &child_name, child_ts, remaining_depth - 1, ctx)
            } else {
                let (size, items) = scan_leaf_recursive_size(&child_path, ctx)?;
                let mut leaf = TreemapTreeNode::new_dir(ctx.next_id(), child_name, child_ts);
                leaf.size = size;
                leaf.descendant_items = items;
                Some(leaf)
            }
        })
        .collect();
    if ctx.cancelled() {
        return None;
    }
    for child in children.into_iter().flatten() {
        node.children.push(child);
    }
    Some(node)
}

/// Collects finished subtrees and turns them into scan events.
struct Coordinator<'a> {
    top_levels: &'a [TopLevel],
    finished: Vec<bool>,
    /// Finished subtrees not yet sent.
    completed: Vec<TreemapTreeNode>,
    scanned_files: &'a AtomicUsize,
    scanned_dirs: &'a AtomicUsize,
    cancellation: &'a AtomicBool,
}

impl Coordinator<'_> {
    fn cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Relaxed)
    }

    fn complete(&mut self, index: usize, tree: TreemapTreeNode) {
        self.finished[index] = true;
        self.completed.push(tree);
    }

    /// Sends everything finished since the last event plus the current
    /// partial size of every unfinished directory. Returns `false` once the
    /// scan is cancelled; a cancelled scan sends nothing more.
    fn emit(&mut self, on_event: &mut impl FnMut(TreemapScanEvent), is_finished: bool) -> bool {
        if self.cancelled() {
            return false;
        }
        let partial_sizes = self
            .top_levels
            .iter()
            .zip(&self.finished)
            .filter(|(_, finished)| !**finished)
            .map(|(top_level, _)| {
                (
                    top_level.placeholder_id,
                    top_level.partial_bytes.load(Ordering::Relaxed),
                )
            })
            .collect();
        let event = TreemapScanEvent {
            update: TreemapScanUpdate::Progress {
                completed: std::mem::take(&mut self.completed),
                partial_sizes,
            },
            scanned_files: self.scanned_files.load(Ordering::Relaxed),
            scanned_dirs: self.scanned_dirs.load(Ordering::Relaxed),
            is_finished,
            root_unreadable: false,
        };
        if self.cancelled() {
            return false;
        }
        on_event(event);
        !self.cancelled()
    }
}

/// Progressive streaming directory scan.
///
/// The root is listed first and sent at once. Each top-level directory then
/// goes to a small bounded worker pool; workers build their subtrees with one
/// metadata lookup per entry and publish a monotonic per-directory byte
/// counter, so the caller can show every top-level directory immediately and
/// refine it while the scan continues.
///
/// Events:
/// 1. The listing; with no top-level directory it is also the last event.
/// 2. Live progress updates every ~100ms (`PARTIAL_INTERVAL`) during timeouts
///    while subtree workers are scanning.
/// 3. A progress update whenever subtrees finish (completions already queued
///    are coalesced), carrying them and the partial sizes of the rest.
/// 4. One final update with `is_finished = true`.
///
/// Reparse points are skipped and unreadable directories are tolerated.
/// Cancellation is checked in every file and directory loop and around each
/// event; a cancelled scan returns without a finished event.
pub fn scan_directory_tree_progressive(
    root_path: &Path,
    max_depth: usize,
    cancellation: Arc<AtomicBool>,
    mut on_event: impl FnMut(TreemapScanEvent),
) {
    let (listing, root_unreadable) = scan_listing(root_path);
    if cancellation.load(Ordering::Relaxed) {
        return;
    }

    let top_levels: Vec<TopLevel> = listing
        .children
        .iter()
        .filter(|child| child.is_dir)
        .map(|child| TopLevel {
            name: child.name.clone(),
            path: root_path.join(&child.name).to_string_lossy().into_owned(),
            modified_ts: child.modified_ts,
            placeholder_id: child.id,
            partial_bytes: AtomicU64::new(0),
        })
        .collect();
    let direct_files = listing.children.len() - top_levels.len();
    // Worker ids start above every listing id so placeholders and freshly
    // scanned nodes cannot collide, even across parallel workers.
    let start_id = listing
        .children
        .iter()
        .map(|child| child.id)
        .max()
        .unwrap_or(listing.id)
        .saturating_add(1);

    // An empty root (or one that cannot be read at all) still terminates.
    let is_finished = top_levels.is_empty();
    on_event(TreemapScanEvent {
        update: TreemapScanUpdate::Listing(listing),
        scanned_files: direct_files,
        scanned_dirs: top_levels.len(),
        is_finished,
        root_unreadable,
    });
    if is_finished || cancellation.load(Ordering::Relaxed) {
        return;
    }

    let scanned_files = AtomicUsize::new(direct_files);
    let scanned_dirs = AtomicUsize::new(top_levels.len());
    let next_id = AtomicU64::new(start_id);
    let next_index = AtomicUsize::new(0);
    let worker_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
        .clamp(1, MAX_WORKERS)
        .min(top_levels.len());
    let (sender, receiver) = mpsc::channel::<(usize, Option<TreemapTreeNode>)>();

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let sender = sender.clone();
            let (cancellation, top_levels, next_index) = (&*cancellation, &top_levels, &next_index);
            let (next_id, scanned_files, scanned_dirs) = (&next_id, &scanned_files, &scanned_dirs);
            scope.spawn(move || loop {
                if cancellation.load(Ordering::Relaxed) {
                    break;
                }
                let index = next_index.fetch_add(1, Ordering::Relaxed);
                let Some(top_level) = top_levels.get(index) else {
                    break;
                };
                let ctx = WorkerContext {
                    cancellation,
                    next_id,
                    scanned_files,
                    scanned_dirs,
                    partial_bytes: &top_level.partial_bytes,
                };
                let subtree = scan_subtree(
                    Path::new(&top_level.path),
                    &top_level.name,
                    top_level.modified_ts,
                    max_depth.saturating_sub(1),
                    &ctx,
                )
                .map(|mut tree| {
                    // One aggregation, sort and category pass for the whole
                    // finished subtree instead of one per level.
                    tree.finish();
                    tree.id = top_level.placeholder_id;
                    tree
                });
                if sender.send((index, subtree)).is_err() {
                    break;
                }
            });
        }
        drop(sender);

        let mut coordinator = Coordinator {
            top_levels: &top_levels,
            finished: vec![false; top_levels.len()],
            completed: Vec::new(),
            scanned_files: &scanned_files,
            scanned_dirs: &scanned_dirs,
            cancellation: &cancellation,
        };
        let mut pending = top_levels.len();

        while pending > 0 {
            if coordinator.cancelled() {
                return;
            }
            match receiver.recv_timeout(PARTIAL_INTERVAL) {
                Ok((index, Some(tree))) => {
                    coordinator.complete(index, tree);
                    pending -= 1;
                    // One event covers every completion already queued.
                    while pending > 0 {
                        match receiver.try_recv() {
                            Ok((queued_index, Some(queued_tree))) => {
                                coordinator.complete(queued_index, queued_tree);
                                pending -= 1;
                            }
                            Ok((_, None)) => return,
                            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                        }
                    }
                    if pending == 0 {
                        break;
                    }
                    if !coordinator.emit(&mut on_event, false) {
                        return;
                    }
                }
                // Workers only produce `None` after observing cancellation:
                // stop without a terminal event.
                Ok((_, None)) => return,
                Err(RecvTimeoutError::Timeout) => {
                    // Emit live progress during timeouts while workers are scanning subtrees.
                    if !coordinator.emit(&mut on_event, false) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // A worker died without reporting its directory. Finish
                    // with what is available rather than hanging the caller.
                    if coordinator.cancelled() {
                        return;
                    }
                    pending = 0;
                }
            }
        }

        coordinator.emit(&mut on_event, true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treemap::category::TreemapCategory;
    use std::collections::HashSet;

    /// The tree a consumer holds after each event, folded with the same
    /// [`TreemapScanUpdate::apply_to`] the application uses.
    struct Snapshot {
        tree: TreemapTreeNode,
        scanned_files: usize,
        scanned_dirs: usize,
        is_finished: bool,
    }

    fn fold(events: Vec<TreemapScanEvent>) -> Vec<Snapshot> {
        let mut tree = TreemapTreeNode::default();
        events
            .into_iter()
            .map(|event| {
                event.update.apply_to(&mut tree);
                Snapshot {
                    tree: tree.clone(),
                    scanned_files: event.scanned_files,
                    scanned_dirs: event.scanned_dirs,
                    is_finished: event.is_finished,
                }
            })
            .collect()
    }

    fn run_progressive_scan(root: &Path, max_depth: usize) -> Vec<Snapshot> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        scan_directory_tree_progressive(root, max_depth, cancel, |event| events.push(event));
        fold(events)
    }

    /// Independent reference: total bytes below `path`, skipping reparse points.
    fn reference_size(path: &Path) -> u64 {
        let mut total = 0u64;
        let mut stack = vec![path.to_path_buf()];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current).unwrap().flatten() {
                let metadata = entry.metadata().unwrap();
                if metadata_is_reparse_point(&metadata) {
                    continue;
                }
                if metadata.is_dir() {
                    stack.push(entry.path());
                } else {
                    total += metadata.len();
                }
            }
        }
        total
    }

    fn collect_tree_ids(node: &TreemapTreeNode, ids: &mut Vec<u64>) {
        ids.push(node.id);
        for child in &node.children {
            collect_tree_ids(child, ids);
        }
    }

    fn top_level<'a>(tree: &'a TreemapTreeNode, name: &str) -> &'a TreemapTreeNode {
        tree.children
            .iter()
            .find(|child| child.name == name)
            .unwrap_or_else(|| panic!("{name} is present"))
    }

    #[test]
    fn the_listing_is_sent_first_and_the_final_tree_is_complete() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("Sub1")).unwrap();
        std::fs::create_dir_all(root.join("Sub2")).unwrap();
        std::fs::write(root.join("Sub1").join("a.txt"), b"12345").unwrap();
        std::fs::write(root.join("Sub2").join("b.txt"), b"1234567890").unwrap();
        std::fs::write(root.join("root.txt"), b"123").unwrap();

        let snapshots = run_progressive_scan(root, 3);
        let listing = &snapshots[0];
        assert!(!listing.is_finished);
        assert_eq!(listing.tree.children.len(), 3);
        assert_eq!(listing.scanned_files, 1);
        assert_eq!(listing.scanned_dirs, 2);
        assert_eq!(top_level(&listing.tree, "root.txt").size, 3);
        assert_eq!(
            top_level(&listing.tree, "Sub1").size,
            0,
            "directories start empty"
        );

        let last = snapshots.last().unwrap();
        assert!(last.is_finished);
        assert_eq!(last.tree.size, 18);
        assert_eq!(last.tree.descendant_count(), 5);
        assert_eq!(top_level(&last.tree, "Sub1").descendant_count(), 1);
        assert_eq!(top_level(&last.tree, "Sub2").descendant_count(), 1);
        assert_eq!(top_level(&last.tree, "root.txt").descendant_count(), 0);
        assert_eq!(last.scanned_files, 3);
        assert!(
            snapshots[..snapshots.len() - 1]
                .iter()
                .all(|s| !s.is_finished),
            "only the last event finishes the scan"
        );
    }

    #[test]
    fn progressive_depth_cap_preserves_recursive_leaf_size() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let leaf_dir = root.join("Top").join("Mid").join("Leaf");
        std::fs::create_dir_all(&leaf_dir).unwrap();
        std::fs::write(leaf_dir.join("deep.bin"), vec![0u8; 4_096]).unwrap();
        std::fs::write(
            root.join("Top").join("Mid").join("mid.bin"),
            vec![0u8; 1_024],
        )
        .unwrap();
        std::fs::write(root.join("Top").join("top.bin"), vec![0u8; 512]).unwrap();

        let snapshots = run_progressive_scan(root, 2);
        let last = snapshots.last().expect("a terminal event is emitted");
        assert!(last.is_finished);
        assert_eq!(last.tree.size, 5_632);

        assert_eq!(last.tree.descendant_count(), 6);
        let top = top_level(&last.tree, "Top");
        assert_eq!(top.size, 5_632);
        assert_eq!(top.descendant_count(), 5);
        let mid = &top.children[0];
        assert_eq!(mid.name, "Mid");
        assert_eq!(mid.size, 5_120);
        assert_eq!(mid.descendant_count(), 3);
        let leaf = top_level(mid, "Leaf");
        assert!(leaf.is_dir);
        assert!(leaf.children.is_empty());
        assert_eq!(leaf.size, 4_096);
        assert_eq!(leaf.descendant_count(), 1);
    }

    #[test]
    fn progressive_final_sizes_match_a_reference_walk() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let alpha = root.join("Alpha");
        let alpha_deep = alpha.join("Deep").join("Deeper");
        std::fs::create_dir_all(&alpha_deep).unwrap();
        std::fs::create_dir_all(root.join("Beta")).unwrap();
        std::fs::create_dir_all(root.join("Gamma")).unwrap();
        std::fs::write(alpha_deep.join("deep.bin"), vec![0u8; 8_000]).unwrap();
        std::fs::write(alpha.join("a.bin"), vec![0u8; 2_000]).unwrap();
        std::fs::write(root.join("Beta").join("b.bin"), vec![0u8; 3_000]).unwrap();
        std::fs::write(root.join("Gamma").join("c.bin"), vec![0u8; 1_000]).unwrap();
        std::fs::write(root.join("root.bin"), vec![0u8; 500]).unwrap();

        let snapshots = run_progressive_scan(root, 5);
        let last = snapshots.last().unwrap();
        assert!(last.is_finished);
        assert_eq!(last.tree.size, reference_size(root));
        for child in last.tree.children.iter().filter(|child| child.is_dir) {
            assert_eq!(
                reference_size(&root.join(&child.name)),
                child.size,
                "{} size must equal its reference walk",
                child.name
            );
        }
    }

    #[test]
    fn progressive_partial_sizes_are_monotonic_lower_bounds() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for name in ["Alpha", "Beta", "Gamma"] {
            let dir = root.join(name);
            std::fs::create_dir_all(dir.join("Nested")).unwrap();
            std::fs::write(dir.join("direct.bin"), vec![0u8; 1_000]).unwrap();
            std::fs::write(dir.join("Nested").join("deep.bin"), vec![0u8; 2_000]).unwrap();
        }

        let snapshots = run_progressive_scan(root, 5);
        let last = snapshots.last().unwrap();
        assert!(last.is_finished);

        let mut previous: HashMap<String, u64> = HashMap::new();
        for snapshot in &snapshots {
            for child in snapshot.tree.children.iter().filter(|child| child.is_dir) {
                let final_size = top_level(&last.tree, &child.name).size;
                assert!(
                    child.size <= final_size,
                    "{} reported {} above final {}",
                    child.name,
                    child.size,
                    final_size
                );
                if let Some(previous_size) = previous.get(&child.name) {
                    assert!(
                        child.size >= *previous_size,
                        "{} size went backwards from {} to {}",
                        child.name,
                        previous_size,
                        child.size
                    );
                }
                previous.insert(child.name.clone(), child.size);
            }
        }
    }

    #[test]
    fn progressive_node_ids_are_unique() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for top in ["One", "Two", "Three", "Four"] {
            let top_dir = root.join(top);
            for nested in ["A", "B"] {
                let nested_dir = top_dir.join(nested);
                std::fs::create_dir_all(&nested_dir).unwrap();
                std::fs::write(nested_dir.join("file.bin"), vec![0u8; 128]).unwrap();
            }
            std::fs::write(top_dir.join("direct.bin"), vec![0u8; 64]).unwrap();
        }
        std::fs::write(root.join("root.bin"), vec![0u8; 32]).unwrap();

        let snapshots = run_progressive_scan(root, 5);
        let last = snapshots.last().unwrap();
        assert!(last.is_finished);

        let mut ids = Vec::new();
        collect_tree_ids(&last.tree, &mut ids);
        let unique: HashSet<u64> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "node ids must be unique");
        assert!(
            ids.len() > 1 + 4 * 3,
            "all nodes from parallel workers are present"
        );
    }

    #[test]
    fn finished_subtrees_keep_their_placeholder_id() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("Alpha").join("Nested")).unwrap();
        std::fs::write(
            root.join("Alpha").join("Nested").join("a.bin"),
            vec![0u8; 10],
        )
        .unwrap();

        let snapshots = run_progressive_scan(root, 5);
        let placeholder = top_level(&snapshots[0].tree, "Alpha").id;
        let finished = top_level(&snapshots.last().unwrap().tree, "Alpha");
        assert_eq!(finished.id, placeholder);
        assert_eq!(finished.children.len(), 1);
    }

    #[test]
    fn a_cancelled_scan_never_emits_a_finished_event() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("Sub")).unwrap();
        std::fs::write(root.join("Sub").join("file.bin"), vec![0u8; 1_024]).unwrap();

        let cancel = Arc::new(AtomicBool::new(true));
        let mut events = Vec::new();
        scan_directory_tree_progressive(root, 3, cancel, |event| events.push(event));

        assert!(
            events.iter().all(|event| !event.is_finished),
            "a cancelled scan must not report completion"
        );
    }

    #[test]
    fn an_empty_root_sends_one_finished_listing() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = run_progressive_scan(temp.path(), 3);

        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].is_finished);
        assert!(snapshots[0].tree.children.is_empty());
        assert_eq!(snapshots[0].scanned_files, 0);
        assert_eq!(snapshots[0].scanned_dirs, 0);
    }

    #[test]
    fn an_unreadable_root_is_distinct_from_an_empty_directory() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let mut events = Vec::new();
        scan_directory_tree_progressive(&missing, 3, Arc::new(AtomicBool::new(false)), |event| {
            events.push(event)
        });

        assert_eq!(events.len(), 1);
        assert!(events[0].is_finished);
        assert!(
            events[0].root_unreadable,
            "a missing root must not look like a legitimate empty directory"
        );
    }

    #[test]
    fn finished_generic_subtrees_resolve_their_content_category() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let generic = root.join("ZYN");
        std::fs::create_dir_all(&generic).unwrap();
        std::fs::write(generic.join("main.rs"), vec![0u8; 128]).unwrap();
        std::fs::write(generic.join("notes.txt"), vec![0u8; 32]).unwrap();

        let snapshots = run_progressive_scan(root, 3);
        let node = top_level(&snapshots.last().unwrap().tree, "ZYN");
        assert_eq!(node.category, TreemapCategory::Code);
    }

    #[test]
    fn progress_updates_refine_placeholders_in_place() {
        let mut tree = TreemapTreeNode::new_dir(1, "C:".to_string(), 0);
        tree.children.push(TreemapTreeNode::new_file(
            100,
            "root.bin".to_string(),
            10,
            0,
        ));
        tree.children
            .push(TreemapTreeNode::new_dir(101, "Alpha".to_string(), 0));
        tree.children
            .push(TreemapTreeNode::new_dir(102, "Beta".to_string(), 0));

        TreemapScanUpdate::Progress {
            completed: Vec::new(),
            partial_sizes: vec![(101, 500), (102, 0)],
        }
        .apply_to(&mut tree);
        assert_eq!(tree.size, 510);
        assert_eq!(tree.children[0].name, "Alpha", "the root re-sorts by size");
        assert_eq!(top_level(&tree, "Alpha").size, 500);
        assert!(top_level(&tree, "Alpha").children.is_empty());
        assert_eq!(top_level(&tree, "Beta").size, 0);

        let mut beta = TreemapTreeNode::new_dir(102, "Beta".to_string(), 0);
        beta.children.push(TreemapTreeNode::new_file(
            9_001,
            "beta.bin".to_string(),
            200,
            0,
        ));
        beta.finish();
        TreemapScanUpdate::Progress {
            completed: vec![beta],
            partial_sizes: vec![(101, 500)],
        }
        .apply_to(&mut tree);
        assert_eq!(tree.size, 710);
        let beta = top_level(&tree, "Beta");
        assert_eq!(beta.size, 200);
        assert_eq!(beta.children.len(), 1);
        assert_eq!(
            top_level(&tree, "Alpha").size,
            500,
            "unfinished directories keep counters"
        );
    }

    #[test]
    fn a_listing_replaces_the_previous_tree() {
        let mut tree = TreemapTreeNode::new_dir(1, "Old".to_string(), 0);
        tree.size = 99;
        let listing = TreemapTreeNode::new_dir(1, "New".to_string(), 0);
        TreemapScanUpdate::Listing(listing.clone()).apply_to(&mut tree);
        assert_eq!(tree, listing);
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Hierarchical nested Treemap layout.
//!
//! Directory containers take a header band and lay their children out
//! recursively in the body below it. Every painted tile stays within a 3:1
//! aspect bound and a label-sized minimum; children that cannot meet it are
//! folded into bounded "N items" summary blocks, and a directory whose body
//! cannot host a legal child stays a compact leaf.

use super::category::TreemapCategory;
use super::geometry::TreemapRect;
use super::path::is_drive_root;
use super::squarify::{squarify, SquarifiedCell};
use super::tree::{TreemapItem, TreemapTreeNode};

/// A nested laid-out rectangular node inside a hierarchical Treemap.
#[derive(Clone, Debug, PartialEq)]
pub struct NestedTreemapLayoutNode {
    pub item: TreemapItem,
    pub path: String,
    /// Overall bounding rectangle (including header and padding).
    pub rect: TreemapRect,
    /// Header rectangle for the directory's name and size band, if present.
    pub header_rect: Option<TreemapRect>,
    /// Inner content rectangle inside which child nodes are laid out.
    pub content_rect: Option<TreemapRect>,
    /// Recursively nested children layout nodes (directories and files).
    pub children: Vec<NestedTreemapLayoutNode>,
    /// Nesting depth from the current view root (0 = root level).
    pub depth: usize,
    /// Number of small children folded into this node, or 0 for a real item.
    /// A non-zero node is a summary block: it has a synthetic parent path for
    /// stable identity and a tooltip, but no children.
    pub aggregate_count: usize,
    /// The folded children as indices into the owning directory's children, so a view can open the
    /// block as a canvas layer.
    pub aggregate_member_indices: Vec<u32>,
}

/// Configuration options for hierarchical nested treemap layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NestedTreemapOptions {
    /// Maximum recursive nesting depth (e.g. 1 to 5).
    pub max_depth: usize,
    /// Minimum width of a node to subdivide further.
    pub min_node_w: f32,
    /// Minimum height of a node to subdivide further.
    pub min_node_h: f32,
    /// Minimum pixel area of a node to subdivide further.
    pub min_node_area: f32,
    /// Height allocated for directory container header strips. The visible
    /// header is painted slightly shorter, so the remaining band becomes the
    /// gap between the header and its children.
    pub header_height: f32,
    /// Layout-side padding around children in directory containers. Sibling
    /// gutters are applied to the painted rect by the view, so this stays 0.
    pub padding: f32,
    /// Upper bound of the per-directory budget for native children; the budget
    /// itself scales with the canvas area. Readable root directories are always
    /// kept and a nested tail of fewer than two members merges back, so this is
    /// a guide rather than a hard cap; the smallest remaining tail folds into
    /// summary blocks.
    pub max_children: usize,
    /// A child below this predicted pixel area joins the aggregate tail.
    pub min_tile_area: f32,
}

impl Default for NestedTreemapOptions {
    fn default() -> Self {
        Self::from_detail_level(3)
    }
}

impl NestedTreemapOptions {
    /// Maps a 1~5 detail level slider into concrete area and depth thresholds.
    pub fn from_detail_level(level: usize) -> Self {
        let (max_depth, min_w, min_h, min_area, max_children, min_tile_area) = match level {
            1 => (1, 80.0, 60.0, 4800.0, 10, 2000.0),
            2 => (2, 64.0, 48.0, 3000.0, 14, 1800.0),
            3 => (3, 50.0, 34.0, 1500.0, 16, 1600.0), // default balanced
            4 => (4, 40.0, 26.0, 850.0, 22, 1200.0),
            _ => (6, 30.0, 20.0, 450.0, 28, 900.0),
        };
        Self {
            max_depth,
            min_node_w: min_w,
            min_node_h: min_h,
            min_node_area: min_area,
            header_height: 20.0,
            padding: 0.0,
            max_children,
            min_tile_area,
        }
    }
}

impl NestedTreemapLayoutNode {
    /// Whether this node is a summary block standing for folded siblings.
    pub fn is_aggregate(&self) -> bool {
        self.aggregate_count > 0
    }

    fn leaf(item: TreemapItem, path: String, rect: TreemapRect, depth: usize) -> Self {
        Self {
            item,
            path,
            rect,
            header_rect: None,
            content_rect: None,
            children: Vec::new(),
            depth,
            aggregate_count: 0,
            aggregate_member_indices: Vec::new(),
        }
    }
}

/// Every painted tile must stay within this width:height ratio. Cells that
/// exceed it are folded into the directory summary and the directory is laid
/// out again, so a long strip can never survive as a tile.
const TREEMAP_MAX_ASPECT_RATIO: f32 = 3.0;
/// Every painted tile must be large enough to carry the standard icon lane and
/// a name. Smaller cells are folded into the summary or hidden, never painted
/// as an unlabeled strip.
const TREEMAP_MIN_TILE_WIDTH: f32 = 46.0;
const TREEMAP_MIN_TILE_HEIGHT: f32 = 30.0;

/// Marker between a summary block's owning directory and its category tag.
const AGGREGATE_MARKER: &str = "::<tail:";

/// Stable identity of a summary block: the directory that owns the folded tail plus the category it
/// stands for. It is never a filesystem path, and the same category keeps the same identity when
/// the canvas is resized.
fn aggregate_path(parent: &str, category: TreemapCategory) -> String {
    format!("{parent}{AGGREGATE_MARKER}{}>", category.machine_key())
}

/// The real directory that owns a summary block, decoded from the block's
/// path. Entering it reveals the folded siblings. `None` for any path that is
/// not a summary path.
pub fn aggregate_parent_path(path: &str) -> Option<&str> {
    let (parent, _) = path.split_once(AGGREGATE_MARKER)?;
    (!parent.is_empty()).then_some(parent)
}

/// One entry of a directory's display plan, in squarify input order.
#[derive(Clone, Debug)]
enum DisplayEntry {
    /// A real child, by index into the directory's children, laid out at its
    /// byte size; a child that cannot fit folds into the tail instead.
    Node { index: usize, weight: u64 },
    /// A summary of folded children, normally one category's tail; a body too
    /// small for one cell per category keeps a single mixed block instead.
    Aggregate {
        path: String,
        members: Vec<usize>,
        size: u64,
        /// The category most of the folded bytes belong to; the block takes that tone.
        category: TreemapCategory,
        /// Area weight the block competes with; raised above `size` when the
        /// block would otherwise be painted as a strip.
        weight: u64,
    },
}

/// What a directory paints and where: the entries and their squarified cells.
#[derive(Debug, Default)]
struct DisplayPlan {
    entries: Vec<DisplayEntry>,
    cells: Vec<SquarifiedCell>,
}

impl DisplayPlan {
    fn squarified(entries: Vec<DisplayEntry>, bounds: TreemapRect) -> Self {
        let weights: Vec<u64> = entries
            .iter()
            .map(|entry| match entry {
                DisplayEntry::Node { weight, .. } => *weight,
                DisplayEntry::Aggregate { weight, .. } => *weight,
            })
            .collect();
        let cells = squarify(&weights, bounds);
        Self { entries, cells }
    }

    /// Splits the cells that break the tile bounds into real children to reflow into the tail and
    /// the folded members an offending summary block stands for.
    fn offenders(&self) -> (Vec<usize>, Vec<usize>) {
        let mut slivers = Vec::new();
        let mut dropped = Vec::new();
        for cell in &self.cells {
            if tile_fits(cell.rect) {
                continue;
            }
            match &self.entries[cell.index] {
                DisplayEntry::Node { index, .. } => slivers.push(*index),
                DisplayEntry::Aggregate { members, .. } => dropped.extend(members.iter().copied()),
            }
        }
        (slivers, dropped)
    }
}

/// Whether a laid-out rect may be painted as a tile.
fn tile_fits(rect: TreemapRect) -> bool {
    let shorter = rect.shorter_side();
    rect.width >= TREEMAP_MIN_TILE_WIDTH
        && rect.height >= TREEMAP_MIN_TILE_HEIGHT
        && !(shorter > 0.0 && rect.longer_side() / shorter > TREEMAP_MAX_ASPECT_RATIO)
}

/// Builds the display entries for the kept children plus one summary block per category present in
/// the tail. A body too small for one cell per category keeps a single mixed summary instead, so a
/// tail never disappears into the directory's total.
fn display_entries(
    nodes: &[TreemapTreeNode],
    parent: &str,
    kept: &[usize],
    tail: &[usize],
    aggregate_floor: u64,
    collapse: bool,
) -> Vec<DisplayEntry> {
    let mut entries = Vec::with_capacity(kept.len() + 1);
    entries.extend(kept.iter().map(|&index| DisplayEntry::Node {
        index,
        weight: nodes[index].size,
    }));
    if tail.is_empty() {
        return entries;
    }
    // One block per category so its tone and label agree; a body too small for
    // several cells keeps a single mixed block instead (see `collapse`).
    let parent_buf;
    let parent = if is_drive_root(parent) {
        if parent.len() == 3 && parent.ends_with('\\') {
            parent
        } else {
            parent_buf = format!("{}\\", parent.trim_end_matches(['\\', '/']));
            &parent_buf
        }
    } else {
        parent.trim_end_matches(['\\', '/'])
    };
    let groups = if collapse {
        // A body too small for one cell per category keeps one summary of the
        // whole tail; its tone follows the bytes it holds. Members are never
        // deleted while the body can still carry a readable cell.
        vec![TailGroup {
            category: tail_category_groups(nodes, tail)
                .into_iter()
                .max_by(|a, b| a.size.cmp(&b.size))
                .map(|group| group.category)
                .unwrap_or(TreemapCategory::Other),
            size: tail.iter().map(|&index| nodes[index].size).sum(),
            members: tail.to_vec(),
        }]
    } else {
        tail_category_groups(nodes, tail)
    };
    for group in groups {
        entries.push(DisplayEntry::Aggregate {
            path: aggregate_path(parent, group.category),
            members: group.members,
            size: group.size,
            category: group.category,
            weight: group.size.max(aggregate_floor),
        });
    }
    entries
}

/// One category's folded tail, largest bytes first.
struct TailGroup {
    category: TreemapCategory,
    members: Vec<usize>,
    size: u64,
}

/// Groups the tail by category so each block's tone and label describe its members.
fn tail_category_groups(nodes: &[TreemapTreeNode], tail: &[usize]) -> Vec<TailGroup> {
    let mut groups: Vec<TailGroup> = Vec::new();
    for &index in tail {
        let category = nodes[index].category;
        match groups.iter_mut().find(|group| group.category == category) {
            Some(group) => {
                group.members.push(index);
                group.size = group.size.saturating_add(nodes[index].size);
            }
            None => groups.push(TailGroup {
                category,
                members: vec![index],
                size: nodes[index].size,
            }),
        }
    }
    groups.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.category.machine_key().cmp(b.category.machine_key()))
    });
    groups
}

/// Plans a directory with more than one sized child. Areas are strictly
/// proportional: a child keeps a native tile at its byte area when that area can
/// carry the smallest readable cell, and folds into one summary block per
/// category when it cannot (or when the arrangement would paint it as a strip).
/// A summary block is the only cell allowed a readable floor.
fn plan_with_tail(
    nodes: &[TreemapTreeNode],
    parent: &str,
    bounds: TreemapRect,
    options: &NestedTreemapOptions,
    depth: usize,
    total_size: u64,
) -> DisplayPlan {
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| {
        nodes[b]
            .size
            .cmp(&nodes[a].size)
            .then_with(|| nodes[a].name.cmp(&nodes[b].name))
    });

    let area = bounds.area();
    let keep_area = options
        .min_tile_area
        .max(TREEMAP_MIN_TILE_WIDTH * TREEMAP_MIN_TILE_HEIGHT);
    let max_children = ((area / 2400.0) as usize).clamp(4, options.max_children.max(4));
    let mut kept = Vec::new();
    let mut tail = Vec::new();
    for (rank, &index) in order.iter().enumerate() {
        let predicted_area = area * (nodes[index].size as f32 / total_size as f32);
        let keep_for_navigation = depth == 0 && nodes[index].is_dir && predicted_area >= keep_area;
        if keep_for_navigation
            || (kept.len() < max_children && (rank == 0 || predicted_area >= keep_area))
        {
            kept.push(index);
        } else {
            tail.push(index);
        }
    }
    if tail.len() < 2 && depth > 0 {
        kept.append(&mut tail);
    }

    let mut aggregate_floor: u64 = 0;
    let mut collapse = false;
    let assemble = |kept: &[usize], tail: &[usize], aggregate_floor: u64, collapse: bool| {
        DisplayPlan::squarified(
            display_entries(nodes, parent, kept, tail, aggregate_floor, collapse),
            bounds,
        )
    };
    let mut plan = assemble(&kept, &tail, aggregate_floor, collapse);
    for _ in 0..64 {
        let (slivers, dropped) = plan.offenders();
        if slivers.is_empty() && dropped.is_empty() {
            break;
        }
        if !slivers.is_empty() {
            // Areas stay proportional: a child that cannot carry a tile at its
            // byte area folds into the summary instead of growing.
            kept.retain(|index| !slivers.contains(index));
            tail.extend(slivers.iter().copied());
            if kept.is_empty() && tail.is_empty() {
                plan = DisplayPlan::default();
                break;
            }
            plan = assemble(&kept, &tail, aggregate_floor, collapse);
            continue;
        }
        if !tail.is_empty() {
            // Only a folded block breaks the bounds: it may grow past its byte
            // share until it can carry a readable cell. The scale stops at the
            // whole body, where one block can fill the directory.
            if aggregate_floor < total_size {
                aggregate_floor = aggregate_floor
                    .saturating_mul(2)
                    .max((total_size / 200).max(1))
                    .min(total_size);
            } else if !collapse {
                // Per-category blocks cannot share a body this small: keep one
                // summary of the whole tail instead of deleting its members.
                collapse = true;
            } else {
                // Only a body that cannot carry any readable cell drops them.
                tail.retain(|index| !dropped.contains(index));
            }
            if kept.is_empty() && tail.is_empty() {
                plan = DisplayPlan::default();
                break;
            }
            plan = assemble(&kept, &tail, aggregate_floor, collapse);
            continue;
        }
        break;
    }

    // The floor escalates by doubling, so the first plan that fits can leave a summary block far
    // larger than the minimum readable cell. Search back down over the same kept/tail set: a block
    // may take the area it needs to be readable, not a column.
    if aggregate_floor > 0 && !tail.is_empty() {
        let mut low = 0u64;
        let mut high = aggregate_floor;
        for _ in 0..12 {
            let mid = low + (high - low) / 2;
            if mid == low {
                break;
            }
            let candidate = assemble(&kept, &tail, mid, collapse);
            let (slivers, dropped) = candidate.offenders();
            if slivers.is_empty() && dropped.is_empty() {
                high = mid;
            } else {
                low = mid;
            }
        }
        if high < aggregate_floor {
            aggregate_floor = high;
            plan = assemble(&kept, &tail, aggregate_floor, collapse);
        }
    }

    // The loop has a hard budget; a pathological canvas may still carry an
    // offending cell. Fold it away rather than painting a strip.
    let (slivers, dropped) = plan.offenders();
    if !slivers.is_empty() || !dropped.is_empty() {
        kept.retain(|index| !slivers.contains(index));
        tail.extend(slivers.iter().copied());
        if !tail.is_empty() {
            collapse = true;
        }
        // Reassemble before the next check: the folded members belong to a
        // plan that must be laid out again, not to the one just measured.
        plan = assemble(&kept, &tail, aggregate_floor, collapse);
        let (slivers, dropped) = plan.offenders();
        if !slivers.is_empty() || !dropped.is_empty() {
            // Only a body that cannot carry any readable cell drops members.
            kept.retain(|index| !slivers.contains(index));
            tail.extend(slivers.iter().copied());
            tail.retain(|index| !dropped.contains(index));
            plan = if kept.is_empty() && tail.is_empty() {
                DisplayPlan::default()
            } else {
                assemble(&kept, &tail, aggregate_floor, collapse)
            };
        }
    }

    // A container must never end up with an empty body. If repeated folds
    // removed every display item, fall back to its largest child so the area
    // is tiled and labeled instead of showing an empty pane.
    if plan.entries.is_empty() {
        if let Some(&largest) = order.first() {
            plan = DisplayPlan::squarified(
                vec![DisplayEntry::Node {
                    index: largest,
                    weight: nodes[largest].size,
                }],
                bounds,
            );
        }
    }
    plan
}

/// True when a laid-out subtree contains no painted rectangle outside the
/// aspect bound. A zero-area rectangle is not painted and cannot be a strip.
fn layout_tree_meets_tile_aspect(node: &NestedTreemapLayoutNode) -> bool {
    let shorter = node.rect.shorter_side();
    let aspect_ok = shorter <= 0.0 || node.rect.longer_side() / shorter <= TREEMAP_MAX_ASPECT_RATIO;
    aspect_ok && node.children.iter().all(layout_tree_meets_tile_aspect)
}

/// Joins a child name onto its containing directory's path. An empty parent
/// (a synthetic root) yields the bare name.
fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else if parent.ends_with('\\') {
        format!("{parent}{name}")
    } else {
        format!("{parent}\\{name}")
    }
}

/// Lays out one real child in its cell: a container with a header band and
/// recursively laid-out body when it can host legal children, otherwise a
/// compact leaf. The child's path is materialized here from `parent`; the
/// tree itself carries names only.
fn layout_child(
    node: &TreemapTreeNode,
    parent: &str,
    rect: TreemapRect,
    options: &NestedTreemapOptions,
    depth: usize,
) -> NestedTreemapLayoutNode {
    let item = TreemapItem::from(node);
    let path = join_path(parent, &node.name);
    // A container must leave a label-sized cell for its child after its own
    // header band; otherwise it renders as a compact leaf instead of a header
    // over a body too short to carry text.
    let min_child_width = options.min_node_w.max(TREEMAP_MIN_TILE_WIDTH);
    let min_child_height = (options.min_node_h + options.header_height)
        .max(options.header_height + TREEMAP_MIN_TILE_HEIGHT);
    let can_subdivide = node.is_dir
        && !node.children.is_empty()
        && (depth + 1) < options.max_depth
        && rect.width >= min_child_width
        && rect.height >= min_child_height
        && rect.area() >= options.min_node_area;
    if !can_subdivide {
        return NestedTreemapLayoutNode::leaf(item, path, rect, depth);
    }

    let header_height = options.header_height.min(rect.height * 0.35);
    let padding = options.padding;
    let content = TreemapRect::new(
        rect.x + padding,
        rect.y + header_height + padding,
        rect.width - 2.0 * padding,
        rect.height - header_height - 2.0 * padding,
    );
    let children = if content.width > 12.0 && content.height > 12.0 {
        let child_parent = format!("{path}\\");
        layout_nested_treemap(&node.children, &child_parent, content, options, depth + 1)
    } else {
        Vec::new()
    };

    // A parent that is itself inside the aspect bound can still leave a child
    // content area too narrow for its own children: the 20px header turns a
    // 3:1 tile into a wider-than-3:1 body. When the resulting tree contains
    // any strip, keep more detail out and paint the directory as one compact
    // leaf instead.
    if !children.iter().all(layout_tree_meets_tile_aspect) {
        return NestedTreemapLayoutNode::leaf(item, path, rect, depth);
    }
    NestedTreemapLayoutNode {
        item,
        path,
        rect,
        header_rect: Some(TreemapRect::new(rect.x, rect.y, rect.width, header_height)),
        content_rect: Some(content),
        children,
        depth,
        aggregate_count: 0,
        aggregate_member_indices: Vec::new(),
    }
}

/// Lays out `nodes` (one directory's children, whose owner is `parent`)
/// inside `bounds`, recursing into directories that can host a readable
/// body. `depth` is the nesting level of `nodes` below the view root.
///
/// The result is in paint order: larger cells first, and every container
/// before its children.
pub fn layout_nested_treemap(
    nodes: &[TreemapTreeNode],
    parent: &str,
    bounds: TreemapRect,
    options: &NestedTreemapOptions,
    depth: usize,
) -> Vec<NestedTreemapLayoutNode> {
    if nodes.is_empty() || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Vec::new();
    }

    let total_size: u64 = nodes.iter().map(|node| node.size).sum();
    let plan = if nodes.len() > 1 && total_size > 0 {
        plan_with_tail(nodes, parent, bounds, options, depth, total_size)
    } else {
        DisplayPlan::squarified(
            (0..nodes.len())
                .map(|index| DisplayEntry::Node {
                    index,
                    weight: nodes[index].size,
                })
                .collect(),
            bounds,
        )
    };

    plan.cells
        .iter()
        .map(|cell| match &plan.entries[cell.index] {
            DisplayEntry::Node { index, .. } => {
                layout_child(&nodes[*index], parent, cell.rect, options, depth)
            }
            DisplayEntry::Aggregate {
                path,
                members,
                size,
                category,
                ..
            } => NestedTreemapLayoutNode {
                aggregate_count: members.len(),
                aggregate_member_indices: members.iter().map(|&index| index as u32).collect(),
                ..NestedTreemapLayoutNode::leaf(
                    TreemapItem {
                        id: 0,
                        name: String::new(),
                        size: *size,
                        is_dir: false,
                        modified_ts: 0,
                        category: *category,
                    },
                    path.clone(),
                    cell.rect,
                    depth,
                )
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_and_layout_nodes_carry_the_category() {
        let mut root = TreemapTreeNode::new_dir(0, "C:".to_string(), 0);
        let mut photos = TreemapTreeNode::new_dir(1, "Photos".to_string(), 0);
        photos.children.push(TreemapTreeNode::new_file(
            2,
            "scan.PNG".to_string(),
            10_000,
            0,
        ));
        root.children.push(photos);
        root.compute_aggregated_sizes();
        root.propagate_content_categories();

        assert_eq!(root.category, TreemapCategory::Image);
        assert_eq!(root.children[0].category, TreemapCategory::Image);
        assert_eq!(
            root.children[0].children[0].category,
            TreemapCategory::Image
        );

        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let layout = layout_nested_treemap(
            &root.children,
            "C:\\",
            bounds,
            &NestedTreemapOptions::default(),
            0,
        );
        let photos = layout
            .iter()
            .find(|n| n.item.name == "Photos")
            .expect("Photos is laid out");
        assert_eq!(photos.item.category, TreemapCategory::Image);
        let scan = photos
            .children
            .iter()
            .find(|n| n.item.name == "scan.PNG")
            .expect("the file leaf is laid out");
        assert_eq!(scan.item.category, TreemapCategory::Image);
    }

    #[test]
    fn test_nested_treemap_layout_hierarchy_and_headers() {
        let mut root = TreemapTreeNode::new_dir(0, "C:".to_string(), 0);

        let mut win = TreemapTreeNode::new_dir(1, "Windows".to_string(), 0);
        let mut sys32 = TreemapTreeNode::new_dir(2, "System32".to_string(), 0);
        sys32.children.push(TreemapTreeNode::new_file(
            3,
            "kernel32.dll".to_string(),
            3_000_000,
            0,
        ));
        win.children.push(sys32);
        win.children.push(TreemapTreeNode::new_file(
            4,
            "explorer.exe".to_string(),
            1_000_000,
            0,
        ));

        let mut users = TreemapTreeNode::new_dir(5, "Users".to_string(), 0);
        let mut user_node = TreemapTreeNode::new_dir(6, "Alice".to_string(), 0);
        user_node.children.push(TreemapTreeNode::new_file(
            7,
            "data.bin".to_string(),
            8_000_000,
            0,
        ));
        users.children.push(user_node);

        root.children.push(win);
        root.children.push(users);
        root.compute_aggregated_sizes();
        root.sort_recursive();

        let bounds = TreemapRect::new(0.0, 0.0, 1000.0, 800.0);
        let options = NestedTreemapOptions::from_detail_level(3);
        let layout = layout_nested_treemap(&root.children, "C:\\", bounds, &options, 0);

        assert_eq!(layout.len(), 2);

        // Users container should have header and children
        let users_layout = layout.iter().find(|n| n.item.name == "Users").unwrap();
        assert!(users_layout.header_rect.is_some());
        assert!(users_layout.content_rect.is_some());
        assert!(!users_layout.children.is_empty());
        assert_eq!(users_layout.children[0].item.name, "Alice");

        // Windows container should have header and children
        let win_layout = layout.iter().find(|n| n.item.name == "Windows").unwrap();
        assert!(win_layout.header_rect.is_some());
        assert!(win_layout.content_rect.is_some());
        assert_eq!(win_layout.children.len(), 2);
    }

    /// A subdivided directory reserves the full 20px header band and leaves no
    /// layout padding: children start at `y + 20` and span the container width.
    /// The view applies a 1px paint inset per tile, which turns shared cell
    /// edges into the visible 2px gutters without compounding them in layout.
    #[test]
    fn nested_treemap_subdivision_uses_a_20px_band_and_no_layout_padding() {
        for level in 1..=5 {
            let options = NestedTreemapOptions::from_detail_level(level);
            assert_eq!(options.header_height, 20.0);
            assert_eq!(options.padding, 0.0);
        }

        let options = NestedTreemapOptions::from_detail_level(3);
        let mut data = TreemapTreeNode::new_dir(1, "Data".to_string(), 0);
        data.children
            .push(TreemapTreeNode::new_file(2, "a.bin".to_string(), 6_000, 0));
        data.children
            .push(TreemapTreeNode::new_file(3, "b.bin".to_string(), 4_000, 0));
        data.compute_aggregated_sizes();

        let rect = TreemapRect::new(30.0, 40.0, 400.0, 300.0);
        let layout = layout_nested_treemap(&[data], "C:\\", rect, &options, 0);
        let container = layout.first().expect("the directory must be laid out");
        let header = container
            .header_rect
            .expect("a subdivided directory has a header");
        let content = container
            .content_rect
            .expect("a subdivided directory has content");

        assert!((header.y - rect.y).abs() < 1e-4);
        assert!((header.height - 20.0).abs() < 1e-4);
        assert!((content.x - rect.x).abs() < 1e-4);
        assert!((content.y - (rect.y + 20.0)).abs() < 1e-4);
        assert!((content.width - rect.width).abs() < 1e-4);
        assert!((content.height - (rect.height - 20.0)).abs() < 1e-4);

        assert!(!container.children.is_empty());
        for child in &container.children {
            assert!(child.rect.x >= content.x - 1e-4);
            assert!(child.rect.y >= content.y - 1e-4);
            assert!(child.rect.x + child.rect.width <= content.x + content.width + 1e-4);
            assert!(child.rect.y + child.rect.height <= content.y + content.height + 1e-4);
        }
    }

    #[test]
    fn test_nested_treemap_detail_level_depth_capping() {
        let mut win = TreemapTreeNode::new_dir(1, "Windows".to_string(), 0);
        let mut sys32 = TreemapTreeNode::new_dir(2, "System32".to_string(), 0);
        sys32.children.push(TreemapTreeNode::new_file(
            3,
            "kernel32.dll".to_string(),
            3_000_000,
            0,
        ));
        win.children.push(sys32);
        win.compute_aggregated_sizes();

        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);

        // LoD level 1: max_depth = 1, so Windows has no subdivided children
        let lod1_opts = NestedTreemapOptions::from_detail_level(1);
        let lod1_layout = layout_nested_treemap(&[win.clone()], "C:\\", bounds, &lod1_opts, 0);
        assert!(lod1_layout[0].children.is_empty());
        assert!(lod1_layout[0].header_rect.is_none());

        // LoD level 3: max_depth = 3, so Windows CAN subdivide
        let lod3_opts = NestedTreemapOptions::from_detail_level(3);
        let lod3_layout = layout_nested_treemap(&[win], "C:\\", bounds, &lod3_opts, 0);
        assert!(!lod3_layout[0].children.is_empty());
        assert!(lod3_layout[0].header_rect.is_some());
    }

    fn layout_test_children(count: usize, size: u64, extension: &str) -> Vec<TreemapTreeNode> {
        (0..count)
            .map(|index| {
                let name = format!("f{index:02}.{extension}");
                TreemapTreeNode::new_file(index as u64 + 1, name, size, 0)
            })
            .collect()
    }

    #[test]
    fn nested_layout_folds_the_small_tail_into_one_block() {
        let mut children = layout_test_children(20, 100, "bin");
        children[0].size = 5_000;
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let options = NestedTreemapOptions::from_detail_level(3);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        assert!(layout.len() < children.len());
        let aggregate = layout
            .iter()
            .find(|node| node.aggregate_count > 0)
            .expect("the long tail must become one summary block");
        assert!(aggregate.aggregate_count >= 2);
        assert!(aggregate.children.is_empty());
        assert!(aggregate.header_rect.is_none());
        assert!(aggregate.path.contains("::<tail:"));
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    /// A tail of one category paints one block carrying every child the canvas did not keep; a tail
    /// of several categories paints one block per category instead.
    #[test]
    fn a_single_category_tail_folds_into_one_block() {
        let children = layout_test_children(60, 100, "bin");
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let options = NestedTreemapOptions::from_detail_level(3);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        let aggregates: Vec<_> = layout
            .iter()
            .filter(|node| node.aggregate_count > 0)
            .collect();
        assert_eq!(
            aggregates.len(),
            1,
            "one category in the tail must be one block, not several size bands"
        );
        assert_eq!(
            aggregates[0].aggregate_count,
            60 - layout
                .iter()
                .filter(|node| node.aggregate_count == 0)
                .count()
        );
        assert!(aggregates[0].path.contains("::<tail:"));
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    /// A tail holding several categories paints one block per category: no block mixes categories
    /// and no block swallows the whole tail, so each block names a set a person can open.
    #[test]
    fn a_mixed_tail_folds_into_one_block_per_category() {
        let mut children = Vec::new();
        for index in 0..40 {
            children.push(TreemapTreeNode::new_file(
                index as u64 + 1,
                format!("clip{index:02}.mp4"),
                10,
                0,
            ));
        }
        for index in 0..40 {
            children.push(TreemapTreeNode::new_file(
                index as u64 + 41,
                format!("note{index:02}.txt"),
                10,
                0,
            ));
        }
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let mut options = NestedTreemapOptions::from_detail_level(3);
        options.max_children = 20;
        options.min_tile_area = 1200.0;

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        let aggregates: Vec<_> = layout
            .iter()
            .filter(|node| node.aggregate_count > 0)
            .collect();
        assert_eq!(
            aggregates.len(),
            2,
            "two categories in the tail, two blocks"
        );
        let mut categories: Vec<_> = aggregates.iter().map(|node| node.item.category).collect();
        categories.sort_by_key(|category| category.machine_key());
        categories.dedup();
        assert_eq!(categories.len(), 2, "each block stands for one category");
        let tiles = layout
            .iter()
            .filter(|node| node.aggregate_count == 0)
            .count();
        assert_eq!(
            aggregates
                .iter()
                .map(|node| node.aggregate_count)
                .sum::<usize>(),
            children.len() - tiles,
            "the blocks together fold exactly the children that are not tiles"
        );
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    #[test]
    fn a_container_must_leave_a_label_sized_child_after_its_header() {
        let mut data = TreemapTreeNode::new_dir(1, "UpdateFramework".to_string(), 0);
        for index in 0..3 {
            data.children.push(TreemapTreeNode::new_file(
                index + 2,
                format!("part{index}.bin"),
                1_000,
                0,
            ));
        }
        data.compute_aggregated_sizes();
        let options = NestedTreemapOptions::from_detail_level(5);

        // 48px total leaves 28px after the header, less than the 30px cell a
        // 28px painted label needs. It must stay a compact leaf, not a header
        // over an unreadable body.
        let short = layout_nested_treemap(
            &[data.clone()],
            "C:\\",
            TreemapRect::new(0.0, 0.0, 140.0, 48.0),
            &options,
            2,
        );
        assert!(short[0].header_rect.is_none());
        assert!(short[0].children.is_empty());

        let tall = layout_nested_treemap(
            &[data],
            "C:\\",
            TreemapRect::new(0.0, 0.0, 140.0, 50.0),
            &options,
            2,
        );
        assert!(tall[0].header_rect.is_some());
        assert!(!tall[0].children.is_empty());
    }

    #[test]
    fn root_layout_keeps_directories_and_summarizes_small_files() {
        let mut children: Vec<TreemapTreeNode> = (0..4)
            .map(|index| {
                let name = format!("dir{index}");
                let mut dir = TreemapTreeNode::new_dir(index as u64 + 1, name, 0);
                dir.size = 50_000 - index as u64 * 10_000;
                dir
            })
            .collect();
        children.extend(layout_test_children(20, 100, "tmp"));
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let options = NestedTreemapOptions::from_detail_level(3);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 0);
        for index in 0..4 {
            let name = format!("dir{index}");
            assert!(
                layout.iter().any(|node| node.item.name == name),
                "root directory {name} must stay a navigation target"
            );
        }
        assert_eq!(
            layout.len(),
            5,
            "tiny root files keep one readable summary block"
        );
        let block = layout
            .iter()
            .find(|node| node.aggregate_count > 0)
            .expect("the tiny files need their summary block");
        assert_eq!(block.aggregate_count, 20);
        assert!(
            tile_fits(block.rect),
            "the summary block must not be a leftover strip: {:.1}x{:.1}",
            block.rect.width,
            block.rect.height
        );
    }

    #[test]
    fn a_tail_of_one_is_kept_instead_of_aggregated() {
        let children = layout_test_children(9, 1_000, "bin");
        let mut options = NestedTreemapOptions::from_detail_level(3);
        options.max_children = 8;
        options.min_tile_area = 0.0;
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        assert_eq!(layout.len(), children.len());
        assert!(layout.iter().all(|node| node.aggregate_count == 0));
    }

    /// The block folds every child that did not become a tile, and it takes the tone of the bytes
    /// it holds, so its colour never implies a category the block does not stand for.
    #[test]
    fn the_block_folds_every_child_that_is_not_a_tile() {
        let mut children = Vec::new();
        for index in 0..30 {
            children.push(TreemapTreeNode::new_file(
                index as u64 + 1,
                format!("code{index:02}.rs"),
                10,
                0,
            ));
        }
        for index in 0..10 {
            children.push(TreemapTreeNode::new_file(
                index as u64 + 31,
                format!("doc{index:02}.txt"),
                100,
                0,
            ));
        }
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let mut options = NestedTreemapOptions::from_detail_level(3);
        options.max_children = 20;
        options.min_tile_area = 1200.0;

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        let aggregates: Vec<_> = layout
            .iter()
            .filter(|node| node.aggregate_count > 0)
            .collect();
        assert_eq!(aggregates.len(), 1);
        let block = aggregates[0];
        let tiles = layout
            .iter()
            .filter(|node| node.aggregate_count == 0)
            .count();
        assert_eq!(block.aggregate_count, children.len() - tiles);
        assert_eq!(
            block.item.category,
            TreemapCategory::Documents,
            "the folded bytes are documents, so the block carries the documents tone"
        );
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    #[test]
    fn sliver_children_are_reflowed_into_the_tail() {
        let mut children = layout_test_children(20, 1_000, "bin");
        children[0].size = 10_000;
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 200.0, 600.0);
        let options = NestedTreemapOptions::from_detail_level(3);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        assert!(layout.iter().any(|node| node.aggregate_count > 0));
        for node in &layout {
            let shorter = node.rect.width.min(node.rect.height);
            let longer = node.rect.width.max(node.rect.height);
            assert!(
                node.rect.width >= TREEMAP_MIN_TILE_WIDTH
                    && node.rect.height >= TREEMAP_MIN_TILE_HEIGHT
                    && (shorter <= 0.0 || longer / shorter <= TREEMAP_MAX_ASPECT_RATIO),
                "a painted tile survived as a {:.1}x{:.1} strip",
                node.rect.width,
                node.rect.height
            );
        }
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    #[test]
    fn zero_size_children_are_not_grouped() {
        let children = layout_test_children(40, 0, "bin");
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let options = NestedTreemapOptions::from_detail_level(3);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        assert_eq!(layout.len(), children.len());
        assert!(layout.iter().all(|node| node.aggregate_count == 0));
    }

    /// A dominant child must not delete its folded siblings: the summary block
    /// stays a readable cell, and every folded byte stays inside the layout.
    #[test]
    fn a_dominant_child_keeps_the_folded_tail_visible() {
        let mut children = Vec::new();
        let mut dominant = TreemapTreeNode::new_dir(1, "dominant".to_string(), 0);
        dominant.size = 1_000_000_000;
        children.push(dominant);
        for index in 0..2u64 {
            let mut node = TreemapTreeNode::new_dir(index + 2, format!("small{index}"), 0);
            node.size = 1_000_000;
            children.push(node);
        }
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 1072.0, 666.0);
        let options = NestedTreemapOptions::from_detail_level(5);

        let layout = layout_nested_treemap(&children, r"C:\Workspace", bounds, &options, 0);
        let block = layout
            .iter()
            .find(|node| node.aggregate_count > 0)
            .unwrap_or_else(|| {
                panic!(
                    "the folded siblings lost their summary block: {:?}",
                    layout
                        .iter()
                        .map(|node| (node.item.name.clone(), node.item.size))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(block.aggregate_count, 2);
        assert!(
            tile_fits(block.rect),
            "the summary block must be a readable cell, got {:.1}x{:.1}",
            block.rect.width,
            block.rect.height
        );
        assert_eq!(
            layout.iter().map(|node| node.item.size).sum::<u64>(),
            total,
            "every folded byte stays inside the layout"
        );
    }

    /// Areas stay proportional: a child that cannot carry a tile at its byte
    /// area folds into the summary block, and every member stays reachable
    /// (as a native tile or as a folded member).
    #[test]
    fn small_members_keep_their_byte_area_as_tiles_or_fold() {
        let members: [(&str, u64); 3] = [
            ("Pickora", 102_263_329),
            ("学术论文写作", 5_661_068),
            ("Resume", 415_461),
        ];
        let nodes: Vec<TreemapTreeNode> = members
            .iter()
            .map(|(name, size)| {
                let mut node = TreemapTreeNode::new_dir(0, (*name).to_string(), 0);
                node.size = *size;
                node
            })
            .collect();
        let mut options = NestedTreemapOptions::from_detail_level(5);
        options.max_children = nodes.len();
        let bounds = TreemapRect::new(0.0, 0.0, 1060.0, 683.0);

        let layout = layout_nested_treemap(&nodes, r"C:\Workspace", bounds, &options, 0);
        let mut visible: Vec<&str> = Vec::new();
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} cell",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
            if node.aggregate_count > 0 {
                visible.extend(
                    node.aggregate_member_indices
                        .iter()
                        .map(|&index| nodes[index as usize].name.as_str()),
                );
            } else {
                visible.push(node.item.name.as_str());
            }
        }
        for (name, _) in members {
            assert!(visible.contains(&name), "{name} is not reachable");
        }
        assert_eq!(
            layout.iter().map(|node| node.item.size).sum::<u64>(),
            nodes.iter().map(|node| node.size).sum::<u64>()
        );
    }

    /// A layer with one dominant member and a long tail of tiny ones folds the
    /// tail instead of tiling thousands of fragments.
    #[test]
    fn a_wide_layer_folds_its_long_tail() {
        let mut nodes = Vec::new();
        let mut dominant = TreemapTreeNode::new_dir(1, "WindowsApps".to_string(), 0);
        dominant.size = 13_890_000_000;
        nodes.push(dominant);
        for index in 0..35u64 {
            nodes.push(TreemapTreeNode::new_file(
                index + 2,
                format!("item{index:02}.bin"),
                1_000 + index * 37,
                0,
            ));
        }
        let mut options = NestedTreemapOptions::from_detail_level(5);
        options.max_children = nodes.len();
        let bounds = TreemapRect::new(0.0, 0.0, 1060.0, 683.0);

        let layout = layout_nested_treemap(&nodes, r"C:\Program Files", bounds, &options, 0);
        let block = layout
            .iter()
            .find(|node| node.aggregate_count > 0)
            .expect("the long tail folds into a block");
        assert_eq!(block.aggregate_count, 35);
        assert_eq!(layout.len(), 2);
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} fragment",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
        }
    }

    /// A tail that holds almost every byte folds into one block at its byte
    /// share; no cell may be painted as a strip.
    #[test]
    fn a_tail_that_dominates_folds_without_painting_strips() {
        let mut nodes = Vec::new();
        let mut largest = TreemapTreeNode::new_file(1, "largest.bin".to_string(), 1_000_000, 0);
        largest.size = 1_000_000;
        nodes.push(largest);
        for index in 0..5_000u64 {
            nodes.push(TreemapTreeNode::new_file(
                index + 2,
                format!("tiny{index:04}.bin"),
                10_000,
                0,
            ));
        }
        let mut options = NestedTreemapOptions::from_detail_level(5);
        options.max_children = nodes.len();
        let bounds = TreemapRect::new(0.0, 0.0, 1060.0, 683.0);

        let layout = layout_nested_treemap(&nodes, r"C:\Program Files", bounds, &options, 0);
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} cell",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
        }
        assert_eq!(
            layout.iter().map(|node| node.item.size).sum::<u64>(),
            nodes.iter().map(|node| node.size).sum::<u64>()
        );
    }

    /// A body too small for one cell per category keeps one summary of the
    /// whole tail: members are not deleted while the body can carry a cell.
    #[test]
    fn a_tiny_body_keeps_its_tail_reachable() {
        let mut nodes = Vec::new();
        let mut largest = TreemapTreeNode::new_file(1, "largest.bin".to_string(), 900_000, 0);
        largest.size = 900_000;
        nodes.push(largest);
        for index in 0..5u64 {
            let name = if index % 2 == 0 {
                format!("doc{index}.txt")
            } else {
                format!("bin{index}.bin")
            };
            nodes.push(TreemapTreeNode::new_file(index + 2, name, 244_000, 0));
        }
        let total: u64 = nodes.iter().map(|node| node.size).sum();
        let options = NestedTreemapOptions::from_detail_level(5);

        let layout = layout_nested_treemap(
            &nodes,
            r"C:\Tiny",
            TreemapRect::new(0.0, 0.0, 90.0, 50.0),
            &options,
            0,
        );
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} cell",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
        }
        let mut shown: Vec<&str> = Vec::new();
        for node in &layout {
            if node.aggregate_count > 0 {
                shown.extend(
                    node.aggregate_member_indices
                        .iter()
                        .map(|&index| nodes[index as usize].name.as_str()),
                );
            } else {
                shown.push(node.item.name.as_str());
            }
        }
        for node in &nodes {
            assert!(
                shown.contains(&node.name.as_str()),
                "{} vanished",
                node.name
            );
        }
        assert_eq!(
            layout.iter().map(|node| node.item.size).sum::<u64>(),
            total,
            "the body keeps every byte"
        );
    }

    /// The live C:\Workspace shape: a workspace that dwarfs its siblings must
    /// still leave every sibling reachable as a tile or a folded member.
    #[test]
    fn a_real_dominant_directory_keeps_every_child() {
        let children: [(&str, u64); 7] = [
            ("Cairn", 9_174_015_100),
            ("Prelude", 865_172_818),
            ("graduation-thesis", 513_088_502),
            ("portfolio", 505_888_511),
            ("Pickora", 102_263_329),
            ("学术论文写作", 5_661_068),
            ("Resume", 415_461),
        ];
        let nodes: Vec<TreemapTreeNode> = children
            .iter()
            .map(|(name, size)| {
                let mut node = TreemapTreeNode::new_dir(0, (*name).to_string(), 0);
                node.size = *size;
                node
            })
            .collect();
        let total: u64 = children.iter().map(|(_, size)| size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 1072.0, 666.0);
        let options = NestedTreemapOptions::from_detail_level(5);

        let layout = layout_nested_treemap(&nodes, r"C:\Workspace", bounds, &options, 0);
        let mut visible: Vec<&str> = Vec::new();
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} cell",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
            if node.aggregate_count > 0 {
                visible.extend(
                    node.aggregate_member_indices
                        .iter()
                        .map(|&index| nodes[index as usize].name.as_str()),
                );
            } else {
                visible.push(node.item.name.as_str());
            }
        }
        let mut visible_sorted = visible.clone();
        visible_sorted.sort_unstable();
        let mut expected: Vec<&str> = children.iter().map(|(name, _)| *name).collect();
        expected.sort_unstable();
        assert_eq!(visible_sorted, expected, "every workspace stays reachable");
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    #[test]
    fn test_nested_treemap_unsorted_nodes_mapping() {
        let node_small = TreemapTreeNode::new_file(1, "small.txt".to_string(), 9_000, 0);
        let node_large = TreemapTreeNode::new_file(2, "large.iso".to_string(), 10_000, 0);
        // Note: input is deliberately [small, large] (unsorted)
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let options = NestedTreemapOptions::default();
        let layout = layout_nested_treemap(&[node_small, node_large], "C:\\", bounds, &options, 0);

        assert_eq!(layout.len(), 2);
        // Each layout item's name MUST match its path exactly (no misaligned zipping)
        for l in &layout {
            if l.path.ends_with("small.txt") {
                assert_eq!(l.item.name, "small.txt");
                assert_eq!(l.item.size, 9_000);
            } else if l.path.ends_with("large.iso") {
                assert_eq!(l.item.name, "large.iso");
                assert_eq!(l.item.size, 10_000);
            } else {
                panic!("unexpected path: {}", l.path);
            }
        }
    }

    /// Progressive snapshots pair one completed subtree with partial placeholder
    /// directories whose byte counters are still tiny. Every painted rectangle at every depth must
    /// satisfy the minimum side and aspect-ratio bounds.
    #[test]
    fn progressive_root_snapshots_never_paint_strips() {
        fn walk<'a>(
            nodes: &'a [NestedTreemapLayoutNode],
            out: &mut Vec<&'a NestedTreemapLayoutNode>,
        ) {
            for node in nodes {
                out.push(node);
                walk(&node.children, out);
            }
        }
        let bounds = TreemapRect::new(0.0, 0.0, 1000.0, 700.0);
        let options = NestedTreemapOptions::from_detail_level(5);
        for dir_count in 3..=20usize {
            for huge in [50u64, 100, 500, 1000] {
                for small in [1u64, 2, 5, 10, 20, 50, 100] {
                    let mut children = Vec::new();
                    for index in 0..dir_count {
                        let name = format!("dir{index:02}");
                        let mut dir = TreemapTreeNode::new_dir(index as u64 + 1, name, 0);
                        let planned = if index == 0 { huge } else { small };
                        // Alternate completed subtrees with still-empty
                        // placeholders, as a live snapshot does.
                        if index % 2 == 0 {
                            let first = planned / 2;
                            let second = planned / 4;
                            let third = planned - first - second;
                            for (child_index, child_size) in
                                [first, second, third].into_iter().enumerate()
                            {
                                dir.children.push(TreemapTreeNode::new_file(
                                    1_000 + index as u64 * 10 + child_index as u64,
                                    format!("part{child_index}.bin"),
                                    child_size,
                                    0,
                                ));
                            }
                            dir.size = planned;
                        } else {
                            dir.size = planned;
                        }
                        children.push(dir);
                    }
                    for file_index in 0..6u64 {
                        children.push(TreemapTreeNode::new_file(
                            100 + file_index,
                            format!("file{file_index}.bin"),
                            1 + file_index * 3,
                            0,
                        ));
                    }
                    let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 0);
                    let mut all = Vec::new();
                    walk(&layout, &mut all);
                    for node in all {
                        let painted = node.rect.width > 0.0 && node.rect.height > 0.0;
                        if !painted {
                            continue;
                        }
                        let shorter = node.rect.width.min(node.rect.height);
                        let longer = node.rect.width.max(node.rect.height);
                        assert!(
                            node.rect.width >= TREEMAP_MIN_TILE_WIDTH
                                && node.rect.height >= TREEMAP_MIN_TILE_HEIGHT,
                            "partial snapshot left {:?} at {:.1}x{:.1} (dir_count={dir_count}, huge={huge}, small={small})",
                            node.item.name,
                            node.rect.width,
                            node.rect.height
                        );
                        assert!(
                            shorter <= 0.0 || longer / shorter <= TREEMAP_MAX_ASPECT_RATIO,
                            "partial snapshot left {:?} as a {:.1}x{:.1} strip (dir_count={dir_count}, huge={huge}, small={small})",
                            node.item.name,
                            node.rect.width,
                            node.rect.height
                        );
                    }
                }
            }
        }
    }

    /// A tile can be within the aspect bound while its header and padding leave
    /// a body that is not. The child would be a strip, so the directory must fall
    /// back to a compact leaf rather than subdividing into that strip.
    #[test]
    fn a_parent_whose_body_would_be_a_strip_renders_as_a_leaf() {
        let mut parent = TreemapTreeNode::new_dir(1, "Parent".to_string(), 0);
        parent.size = 2_048;
        parent.children.push(TreemapTreeNode::new_file(
            2,
            "only.bin".to_string(),
            2_048,
            0,
        ));
        let options = NestedTreemapOptions::from_detail_level(5);
        // A 3:1 tile stays within the parent bound; after the 20px header its
        // 600x180 body would give the only child a 3.33:1 strip.
        let layout = layout_nested_treemap(
            &[parent],
            "C:\\",
            TreemapRect::new(0.0, 0.0, 600.0, 200.0),
            &options,
            0,
        );

        assert_eq!(layout.len(), 1);
        assert!(
            layout[0].header_rect.is_none() && layout[0].children.is_empty(),
            "a directory whose body cannot host a legal child must stay a compact leaf"
        );
    }

    #[test]
    fn an_aggregate_parent_is_the_real_directory_that_owns_the_tail() {
        assert_eq!(
            aggregate_parent_path(&aggregate_path(r"C:\Users\Alice", TreemapCategory::Other)),
            Some(r"C:\Users\Alice")
        );
        assert_eq!(
            aggregate_parent_path(&aggregate_path(r"C:\", TreemapCategory::Other)),
            Some(r"C:\")
        );
        assert_eq!(
            aggregate_parent_path(r"C:\Users\Alice::<tail:folder>"),
            Some(r"C:\Users\Alice")
        );
        assert_eq!(aggregate_parent_path(r"C:\Users\Alice"), None);
        assert_eq!(aggregate_parent_path("::<tail:folder>"), None);
    }

    /// A block's identity is its directory and its category, so a relayout cannot re-key it and two
    /// categories never share one block.
    #[test]
    fn a_block_identity_is_its_directory_and_category() {
        let first = aggregate_path(r"C:\Windows\WinSxS", TreemapCategory::Other);
        assert_eq!(
            first,
            aggregate_path(r"C:\Windows\WinSxS", TreemapCategory::Other)
        );
        assert_ne!(
            first,
            aggregate_path(r"C:\Windows\WinSxS", TreemapCategory::Image)
        );
        assert_ne!(
            first,
            aggregate_path(r"C:\Windows\System32", TreemapCategory::Other)
        );
    }

    #[test]
    fn forward_slash_and_drive_root_produce_clean_aggregate_paths() {
        let nodes = vec![
            TreemapTreeNode::new_file(1, "a.txt".to_string(), 10, 0),
            TreemapTreeNode::new_file(2, "b.txt".to_string(), 10, 0),
        ];
        let entries_forward_drive = display_entries(&nodes, "C:/", &[0], &[1], 0, false);
        let DisplayEntry::Aggregate { path, .. } = &entries_forward_drive[1] else {
            panic!("expected aggregate entry");
        };
        assert_eq!(path, "C:\\::<tail:documents>");

        let entries_forward_dir = display_entries(&nodes, "C:/Users/Alice/", &[0], &[1], 0, false);
        let DisplayEntry::Aggregate { path, .. } = &entries_forward_dir[1] else {
            panic!("expected aggregate entry");
        };
        assert_eq!(path, "C:/Users/Alice::<tail:documents>");
    }

    /// A small body still carries its summary block at a readable size instead
    /// of deleting the folded members.
    #[test]
    fn a_small_body_keeps_its_summary_block() {
        let nodes = vec![
            TreemapTreeNode::new_file(0, "dominant.bin".to_string(), 1777, 0),
            TreemapTreeNode::new_file(1, "small.txt".to_string(), 28, 0),
            TreemapTreeNode::new_file(2, "zero-a.bin".to_string(), 0, 0),
            TreemapTreeNode::new_file(3, "zero-b.bin".to_string(), 0, 0),
            TreemapTreeNode::new_file(4, "zero-c.bin".to_string(), 0, 0),
        ];
        let total: u64 = nodes.iter().map(|node| node.size).sum();
        let options = NestedTreemapOptions::from_detail_level(5);
        let layout = layout_nested_treemap(
            &nodes,
            r"C:\Small",
            TreemapRect::new(0.0, 0.0, 96.0, 60.0),
            &options,
            0,
        );
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} cell",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
        }
        let mut shown: Vec<&str> = Vec::new();
        let mut laid_bytes = 0u64;
        for node in &layout {
            laid_bytes += node.item.size;
            if node.aggregate_count > 0 {
                shown.extend(
                    node.aggregate_member_indices
                        .iter()
                        .map(|&index| nodes[index as usize].name.as_str()),
                );
            } else {
                shown.push(node.item.name.as_str());
            }
        }
        assert!(
            shown.contains(&"small.txt"),
            "the folded member vanished: {shown:?}"
        );
        assert_eq!(laid_bytes, total, "the body keeps every byte");
    }

    /// A single-category tail on a small body collapses into one block that
    /// still lists every member.
    #[test]
    fn a_single_category_small_body_keeps_its_block() {
        let nodes = vec![
            TreemapTreeNode::new_file(0, "dominant.bin".to_string(), 1777, 0),
            TreemapTreeNode::new_file(1, "small.bin".to_string(), 28, 0),
            TreemapTreeNode::new_file(2, "zero-a.bin".to_string(), 0, 0),
            TreemapTreeNode::new_file(3, "zero-b.bin".to_string(), 0, 0),
        ];
        let total: u64 = nodes.iter().map(|node| node.size).sum();
        let options = NestedTreemapOptions::from_detail_level(5);
        let layout = layout_nested_treemap(
            &nodes,
            r"C:\Small",
            TreemapRect::new(0.0, 0.0, 96.0, 60.0),
            &options,
            0,
        );
        let mut shown: Vec<&str> = Vec::new();
        let mut laid_bytes = 0u64;
        for node in &layout {
            laid_bytes += node.item.size;
            if node.aggregate_count > 0 {
                shown.extend(
                    node.aggregate_member_indices
                        .iter()
                        .map(|&index| nodes[index as usize].name.as_str()),
                );
            } else {
                shown.push(node.item.name.as_str());
            }
        }
        for node in &nodes {
            assert!(
                shown.contains(&node.name.as_str()),
                "{} vanished",
                node.name
            );
        }
        assert_eq!(laid_bytes, total, "the body keeps every byte");
    }

    /// A directory body too flat to hold a readable child keeps its compact
    /// leaf instead of painting a strip through its children.
    #[test]
    fn a_flat_directory_body_stays_a_compact_leaf() {
        let mut flat = TreemapTreeNode::new_dir(1, "Flat".to_string(), 0);
        flat.children.push(TreemapTreeNode::new_file(
            2,
            "dominant.jpg".to_string(),
            1_000_000,
            0,
        ));
        for index in 0..40u64 {
            flat.children.push(TreemapTreeNode::new_file(
                index + 3,
                format!("photo{index:02}.jpg"),
                1_000,
                0,
            ));
        }
        flat.compute_aggregated_sizes();
        let options = NestedTreemapOptions::from_detail_level(5);
        let laid = layout_child(
            &flat,
            r"C:\",
            TreemapRect::new(0.0, 0.0, 480.0, 150.0),
            &options,
            0,
        );
        assert!(
            laid.children.is_empty(),
            "a body that cannot tile its children stays a compact leaf"
        );
        assert!(laid.header_rect.is_none());
    }

    /// The live C:\Users shape: two dominant directories beside a handful of tiny
    /// categories. A readable summary block may take the minimum area it needs, not a
    /// full-height column far beyond its byte share.
    #[test]
    fn tiny_summary_blocks_do_not_take_giant_columns() {
        let mut nodes = Vec::new();
        nodes.push(TreemapTreeNode::new_dir(0, "ZYN".to_string(), 0));
        nodes[0].size = 138_170_000_000;
        nodes.push(TreemapTreeNode::new_dir(1, ".gemini".to_string(), 0));
        nodes[1].size = 48_720_000_000;
        let rest: [(&str, u64); 12] = [
            ("rest-doc1.txt", 10_000_000),
            ("rest-doc2.md", 8_000_000),
            ("rest-doc3.pdf", 6_800_000),
            ("rest-doc4.docx", 3_000_000),
            ("rest-exe1.bin", 1_500_000),
            ("rest-exe2.exe", 1_200_000),
            ("rest-exe3.dll", 800_000),
            ("rest-exe4.sys", 500_000),
            ("rest-video.mp4", 1_000_000),
            ("rest-archive.zip", 900_000),
            ("rest-image.png", 3_600),
            ("rest-audio.mp3", 2_000),
        ];
        for (index, (name, size)) in rest.iter().enumerate() {
            nodes.push(TreemapTreeNode::new_file(
                index as u64 + 2,
                (*name).to_string(),
                *size,
                0,
            ));
        }
        let options = NestedTreemapOptions::from_detail_level(5);
        let bounds = TreemapRect::new(0.0, 0.0, 1272.0, 654.0);
        let layout = layout_nested_treemap(&nodes, r"C:\Users", bounds, &options, 0);
        for node in &layout {
            assert!(
                tile_fits(node.rect),
                "{:?} painted a {:.1}x{:.1} cell",
                node.item.name,
                node.rect.width,
                node.rect.height
            );
            if node.aggregate_count > 0 {
                assert!(
                    node.rect.width <= 60.0,
                    "summary block took a {:.1}px-wide column for {} bytes",
                    node.rect.width,
                    node.item.size
                );
                assert!(
                    node.rect.area() <= 6_000.0,
                    "summary block took {:.0}px2 for {} bytes",
                    node.rect.area(),
                    node.item.size
                );
            }
        }
    }

    /// No child is painted or folded twice at any body size.
    #[test]
    fn tail_members_are_listed_once_at_every_body_size() {
        let mut nodes = Vec::new();
        let mut largest = TreemapTreeNode::new_file(0, "largest.bin".to_string(), 1_000_000, 0);
        largest.size = 1_000_000;
        nodes.push(largest);
        for index in 1..400u64 {
            let name = match index % 4 {
                0 => format!("doc{index:02}.txt"),
                1 => format!("pic{index:02}.png"),
                2 => format!("bin{index:02}.exe"),
                _ => format!("clip{index:02}.mp4"),
            };
            nodes.push(TreemapTreeNode::new_file(
                index,
                name,
                40_000 + index * 900,
                0,
            ));
        }
        let options = NestedTreemapOptions::from_detail_level(5);
        for width in (50..=260).step_by(10) {
            for height in (30..=160).step_by(10) {
                let bounds = TreemapRect::new(0.0, 0.0, width as f32, height as f32);
                let layout = layout_nested_treemap(&nodes, r"C:\Sweep", bounds, &options, 0);
                let mut seen = vec![false; nodes.len()];
                for node in &layout {
                    if node.aggregate_count == 0 {
                        let index = nodes
                            .iter()
                            .position(|n| n.name == node.item.name)
                            .expect("a painted tile names a child");
                        assert!(
                            !seen[index],
                            "{} listed twice at {width}x{height}",
                            node.item.name
                        );
                        seen[index] = true;
                    } else {
                        assert_eq!(node.aggregate_count, node.aggregate_member_indices.len());
                        for &member in &node.aggregate_member_indices {
                            assert!(
                                !seen[member as usize],
                                "member {member} listed twice at {width}x{height}"
                            );
                            seen[member as usize] = true;
                        }
                    }
                }
            }
        }
    }
}

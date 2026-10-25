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

use super::category::{CategoryBytes, TreemapCategory};
use super::geometry::TreemapRect;
use super::squarify::{squarify, SquarifiedCell};
use super::tree::{TreemapItem, TreemapTreeNode};
use std::ops::Range;

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
    /// Upper bound on individually displayed children per directory. The
    /// smallest tail is folded into one aggregate block.
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

/// A tail covering more than this share of its directory is split into several
/// summary blocks. One summary swallowing most of a directory reads as an empty
/// pane, regardless of how many items it represents.
const TAIL_AGGREGATE_MAX_SHARE: f32 = 0.18;
const TAIL_AGGREGATE_MAX_GROUPS: usize = 8;
/// A split summary must itself have enough pixels to carry a label; small
/// containers keep one summary instead of a row of unreadable mini blocks.
const MIN_SUMMARY_AREA: f32 = 64.0 * 48.0;
/// Below this share a tail is not painted as a separate strip: its bytes stay
/// in the directory header and the visible tiles fill the area. Without this
/// floor a 1-2% tail is forced into a thin leftover edge.
const MIN_VISIBLE_TAIL_SHARE: f32 = 0.05;

/// Splits the size-ordered tail into roughly equal summary groups. A small tail
/// keeps the previous single-block behavior; a long one yields several bounded
/// "N items" blocks that still cover the whole area.
fn tail_group_ranges(
    nodes: &[TreemapTreeNode],
    tail: &[usize],
    total_size: u64,
    bounds_area: f32,
) -> Vec<std::ops::Range<usize>> {
    if tail.is_empty() {
        return Vec::new();
    }
    let tail_size: u64 = tail.iter().map(|&index| nodes[index].size).sum();
    let share = if total_size > 0 {
        tail_size as f32 / total_size as f32
    } else {
        0.0
    };
    let tail_area = bounds_area * share;
    let wanted_by_share = (share / TAIL_AGGREGATE_MAX_SHARE).ceil() as usize;
    let wanted_by_area = (tail_area / MIN_SUMMARY_AREA).floor().max(1.0) as usize;
    let wanted = wanted_by_share
        .min(wanted_by_area)
        .min(TAIL_AGGREGATE_MAX_GROUPS)
        .min((tail.len() / 2).max(1));
    if wanted == 1 {
        return std::iter::once(0..tail.len()).collect();
    }

    let target = (tail_size / wanted as u64).max(1);
    let mut groups = Vec::with_capacity(wanted);
    let mut start = 0;
    let mut bytes = 0u64;
    for (index, &child) in tail.iter().enumerate() {
        bytes = bytes.saturating_add(nodes[child].size);
        let remaining_items = tail.len() - index - 1;
        let remaining_groups = wanted - groups.len() - 1;
        if groups.len() + 1 < wanted && bytes >= target && remaining_items >= remaining_groups {
            groups.push(start..index + 1);
            start = index + 1;
            bytes = 0;
        }
    }
    if start < tail.len() {
        groups.push(start..tail.len());
    }
    groups
}

/// Marker between a summary block's owning directory and its group tag.
const AGGREGATE_MARKER: &str = "::<tail";

/// Stable identity of a summary block: the directory that owns the folded
/// tail followed by `::<tail>` (first group) or `::<tail#N>`. It is never a
/// filesystem path.
fn aggregate_path(parent: &str, group_index: usize) -> String {
    if group_index == 0 {
        format!("{parent}{AGGREGATE_MARKER}>")
    } else {
        format!("{parent}{AGGREGATE_MARKER}#{group_index}>")
    }
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
    /// A real child, by index into the directory's children.
    Node(usize),
    /// A summary block folding `members` of the tail.
    Aggregate {
        path: String,
        members: Range<usize>,
        size: u64,
        category: TreemapCategory,
    },
}

/// What a directory paints and where: the entries and their squarified cells.
#[derive(Debug, Default)]
struct DisplayPlan {
    entries: Vec<DisplayEntry>,
    cells: Vec<SquarifiedCell>,
}

impl DisplayPlan {
    fn squarified(
        nodes: &[TreemapTreeNode],
        entries: Vec<DisplayEntry>,
        bounds: TreemapRect,
    ) -> Self {
        let sizes: Vec<u64> = entries
            .iter()
            .map(|entry| match entry {
                DisplayEntry::Node(index) => nodes[*index].size,
                DisplayEntry::Aggregate { size, .. } => *size,
            })
            .collect();
        let cells = squarify(&sizes, bounds);
        Self { entries, cells }
    }

    /// Splits the cells that break the tile bounds into real children to
    /// reflow into the tail and summary groups to drop.
    fn offenders(&self) -> (Vec<usize>, Vec<Range<usize>>) {
        let mut slivers = Vec::new();
        let mut dropped = Vec::new();
        for cell in &self.cells {
            if tile_fits(cell.rect) {
                continue;
            }
            match &self.entries[cell.index] {
                DisplayEntry::Node(index) => slivers.push(*index),
                DisplayEntry::Aggregate { members, .. } => dropped.push(members.clone()),
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

/// Builds the display entries for the kept children plus the summary groups
/// of the tail. A tail too small to paint is left out entirely: its bytes
/// stay in the directory header and the visible tiles fill the area.
fn display_entries(
    nodes: &[TreemapTreeNode],
    parent: &str,
    kept: &[usize],
    tail: &[usize],
    total_size: u64,
    bounds_area: f32,
) -> Vec<DisplayEntry> {
    let mut entries = Vec::with_capacity(kept.len() + tail.len().min(TAIL_AGGREGATE_MAX_GROUPS));
    entries.extend(kept.iter().map(|&index| DisplayEntry::Node(index)));

    let tail_size: u64 = tail.iter().map(|&index| nodes[index].size).sum();
    let tail_share = if total_size > 0 {
        tail_size as f32 / total_size as f32
    } else {
        0.0
    };
    if tail_share < MIN_VISIBLE_TAIL_SHARE || bounds_area * tail_share < MIN_SUMMARY_AREA {
        return entries;
    }

    let parent = parent.trim_end_matches('\\');
    for (group_index, members) in tail_group_ranges(nodes, tail, total_size, bounds_area)
        .into_iter()
        .enumerate()
    {
        let mut size = 0u64;
        let mut bytes = CategoryBytes::default();
        for &index in &tail[members.clone()] {
            size = size.saturating_add(nodes[index].size);
            bytes.add(nodes[index].category, nodes[index].size);
        }
        entries.push(DisplayEntry::Aggregate {
            path: aggregate_path(parent, group_index),
            members,
            size,
            category: bytes.dominant().unwrap_or(TreemapCategory::Other),
        });
    }
    entries
}

/// Plans a directory with more than one sized child: keeps the largest
/// children that can carry a readable tile, folds the rest into summary
/// groups, and reflows until no painted cell breaks the tile bounds.
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

    // Root directories are navigation targets while they can carry a readable
    // tile. A partial scan gives some of them a tiny placeholder size, and a
    // 4x350 column is not a navigation target; it is an unclickable strip.
    // Every item, root or not, goes through the same sliver reflow and can
    // reappear once its bytes grow.
    let area = bounds.area();
    let max_children = ((area / 2400.0) as usize).clamp(4, options.max_children.max(4));
    let mut kept = Vec::new();
    let mut tail = Vec::new();
    for (rank, &index) in order.iter().enumerate() {
        let predicted_area = area * (nodes[index].size as f32 / total_size as f32);
        let keep_for_navigation =
            depth == 0 && nodes[index].is_dir && predicted_area >= options.min_tile_area;
        if keep_for_navigation
            || (kept.len() < max_children && (rank == 0 || predicted_area >= options.min_tile_area))
        {
            kept.push(index);
        } else {
            tail.push(index);
        }
    }
    if tail.len() < 2 && depth > 0 {
        kept.append(&mut tail);
    }

    let mut plan = DisplayPlan::squarified(
        nodes,
        display_entries(nodes, parent, &kept, &tail, total_size, area),
        bounds,
    );
    loop {
        let (slivers, dropped) = plan.offenders();
        if slivers.is_empty() && dropped.is_empty() {
            break;
        }

        // Summary groups that cannot meet the aspect bound are removed
        // instead of leaving a thin color strip. Their bytes stay in the
        // directory header, and the surviving tiles fill the area. Remove
        // them even when every painted item was a summary; otherwise the
        // same offending layout is rebuilt forever.
        if !dropped.is_empty() {
            tail = tail
                .iter()
                .enumerate()
                .filter(|(position, _)| !dropped.iter().any(|range| range.contains(position)))
                .map(|(_, &child)| child)
                .collect();
        }
        if !slivers.is_empty() {
            kept.retain(|index| !slivers.contains(index));
            tail.extend(slivers.iter().copied());
        }

        if kept.is_empty() && tail.is_empty() {
            plan = DisplayPlan::default();
            break;
        }
        let display_total = kept
            .iter()
            .chain(tail.iter())
            .map(|&index| nodes[index].size)
            .sum::<u64>()
            .max(1);
        plan = DisplayPlan::squarified(
            nodes,
            display_entries(nodes, parent, &kept, &tail, display_total, area),
            bounds,
        );
    }

    // A container must never end up with an empty body. If repeated drops
    // removed every display item, fall back to its largest child so the area
    // is tiled and labeled instead of showing an empty pane.
    if plan.entries.is_empty() {
        if let Some(&largest) = order.first() {
            plan = DisplayPlan::squarified(nodes, vec![DisplayEntry::Node(largest)], bounds);
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
            nodes,
            (0..nodes.len()).map(DisplayEntry::Node).collect(),
            bounds,
        )
    };

    plan.cells
        .iter()
        .map(|cell| match &plan.entries[cell.index] {
            DisplayEntry::Node(index) => {
                layout_child(&nodes[*index], parent, cell.rect, options, depth)
            }
            DisplayEntry::Aggregate {
                path,
                members,
                size,
                category,
            } => NestedTreemapLayoutNode {
                aggregate_count: members.len(),
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

        assert_eq!(root.category, TreemapCategory::Folder);
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
        let mut user_zyn = TreemapTreeNode::new_dir(6, "ZYN".to_string(), 0);
        user_zyn.children.push(TreemapTreeNode::new_file(
            7,
            "data.bin".to_string(),
            8_000_000,
            0,
        ));
        users.children.push(user_zyn);

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
        assert_eq!(users_layout.children[0].item.name, "ZYN");

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
        assert!(aggregate.path.ends_with("::<tail>"));
        assert_eq!(layout.iter().map(|node| node.item.size).sum::<u64>(), total);
    }

    /// A tail that would swallow most of a directory is split into several
    /// bounded summaries; one giant "N items" pane reads as empty space.
    #[test]
    fn a_large_tail_is_split_into_bounded_summary_blocks() {
        let children = layout_test_children(60, 100, "bin");
        let total: u64 = children.iter().map(|node| node.size).sum();
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let options = NestedTreemapOptions::from_detail_level(3);

        let layout = layout_nested_treemap(&children, "C:\\", bounds, &options, 1);
        let aggregates: Vec<_> = layout
            .iter()
            .filter(|node| node.aggregate_count > 0)
            .collect();
        assert!(
            aggregates.len() > 1,
            "an 80% tail must not stay one giant summary block"
        );
        assert_eq!(
            aggregates
                .iter()
                .map(|node| node.aggregate_count)
                .sum::<usize>(),
            60 - layout
                .iter()
                .filter(|node| node.aggregate_count == 0)
                .count()
        );
        assert!(aggregates.iter().all(|node| node.path.contains("::<tail")));
        assert!(aggregates
            .iter()
            .all(|node| node.rect.area() <= bounds.area() * 0.25));
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
            4,
            "tiny root files must not survive as a leftover strip"
        );
        assert!(layout.iter().all(|node| node.aggregate_count == 0));
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

    #[test]
    fn aggregate_uses_the_dominant_category() {
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
        let aggregate = layout
            .iter()
            .find(|node| node.aggregate_count > 0)
            .expect("the code tail must aggregate");
        assert_eq!(aggregate.item.category, TreemapCategory::Code);
        assert!(aggregate.aggregate_count >= 20);
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
            aggregate_parent_path(&aggregate_path(r"C:\Users\ZYN", 0)),
            Some(r"C:\Users\ZYN")
        );
        assert_eq!(
            aggregate_parent_path(&aggregate_path(r"C:\Users\ZYN", 3)),
            Some(r"C:\Users\ZYN")
        );
        assert_eq!(
            aggregate_parent_path(r"C:\Users\ZYN::<tail>"),
            Some(r"C:\Users\ZYN")
        );
        assert_eq!(aggregate_parent_path(r"C:\Users\ZYN"), None);
        assert_eq!(aggregate_parent_path("::<tail>"), None);
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Disk-space Treemap model: category classification, the hierarchical size
//! tree, directory size aggregation for compact indexes, the nested
//! squarified layout, and progressive filesystem scanning.
//!
//! Data flows one way. A producer — [`DirectorySizeIndex::build_tree_node`] or
//! [`scan_directory_tree_progressive`] — yields a [`TreemapTreeNode`] that is
//! already aggregated, sorted by size and category-resolved, so consumers
//! never re-walk it. [`layout_nested_treemap`] turns its children into
//! [`NestedTreemapLayoutNode`]s in base pixel space; painting, zoom and hit
//! testing belong to the application.

mod category;
mod geometry;
mod index;
mod layout;
mod path;
mod scan;
mod squarify;
mod tree;

pub use category::TreemapCategory;
pub use geometry::TreemapRect;
pub use index::DirectorySizeIndex;
pub use layout::{
    aggregate_parent_path, layout_nested_treemap, NestedTreemapLayoutNode, NestedTreemapOptions,
};
pub use path::{is_drive_root, parent_directory};
pub use scan::{scan_directory_tree_progressive, TreemapScanEvent, TreemapScanUpdate};
pub use tree::{TreemapItem, TreemapTreeNode};

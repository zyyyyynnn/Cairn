// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

/// Lane widths the table grows its four columns from, in rem. They are ratios, not fixed extents:
/// a wider window makes every lane wider in proportion, and a narrower one shrinks them down to
/// their minima before the band scrolls sideways. Dragging a boundary trades width between the two
/// neighbours, so the total — and therefore every other lane — stays where it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnWidths {
    pub name: f32,
    pub path: f32,
    pub size: f32,
    pub date: f32,
}

/// The draggable boundary between two visible neighbouring flex lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnBoundary {
    pub left: usize,
    pub right: usize,
}

#[cfg(test)]
impl ColumnBoundary {
    pub const NAME_PATH: Self = Self { left: 0, right: 1 };
    pub const PATH_SIZE: Self = Self { left: 1, right: 2 };
    pub const SIZE_DATE: Self = Self { left: 2, right: 3 };
}

impl ColumnBoundary {
    /// Stable element-id suffix, one per visible lane pair.
    pub const fn key(self) -> &'static str {
        match (self.left, self.right) {
            (0, 1) => "name-path",
            (0, 2) => "name-size",
            (0, 3) => "name-date",
            (1, 2) => "path-size",
            (1, 3) => "path-date",
            (2, 3) => "size-date",
            _ => "custom",
        }
    }

    pub const fn indices(self) -> (usize, usize) {
        (self.left, self.right)
    }
}

impl ColumnWidths {
    /// Moves `boundary` by `delta` rem, taking width from the lane on its right and giving it to
    /// the lane on its left. Either lane may stop at its own minimum; the other then keeps the
    /// remainder, so a drag can never squeeze a column out of existence.
    pub fn resized(self, boundary: ColumnBoundary, delta: f32, minima: [f32; 4]) -> Self {
        let (left, right) = boundary.indices();
        let mut widths = [self.name, self.path, self.size, self.date];
        debug_assert!(
            widths[left] + widths[right] >= minima[left] + minima[right],
            "column minima exceed their combined width"
        );
        let delta = delta.clamp(
            -(widths[left] - minima[left]),
            widths[right] - minima[right],
        );
        widths[left] += delta;
        widths[right] -= delta;
        Self {
            name: widths[0],
            path: widths[1],
            size: widths[2],
            date: widths[3],
        }
    }

    pub fn total(self) -> f32 {
        self.name + self.path + self.size + self.date
    }
}

pub use cairn_core::sort::{SortColumn, SortDirection, SortSpec};

/// The Fluent message naming a sort column, shared by its table header and its 排序 item so one
/// column never carries two wordings.
pub trait SortColumnLabel {
    fn label_key(self) -> &'static str;

    /// The Recycle Bin caption for the lanes that change meaning there.
    fn recycle_bin_label_key(self) -> &'static str;
}

impl SortColumnLabel for SortColumn {
    fn label_key(self) -> &'static str {
        match self {
            SortColumn::Name => "col_name",
            SortColumn::Path => "col_path",
            SortColumn::Size => "col_size",
            SortColumn::DateModified => "col_modified",
            SortColumn::DateCreated => "col_created",
            SortColumn::Extension => "col_ext",
            SortColumn::Length => "col_length",
            SortColumn::Author => "col_author",
            SortColumn::Tags => "col_tags",
            SortColumn::Dimensions => "col_dimensions",
            SortColumn::Framerate => "col_framerate",
        }
    }

    fn recycle_bin_label_key(self) -> &'static str {
        match self {
            SortColumn::Path => "col_original_location",
            SortColumn::DateModified => "col_deleted",
            _ => self.label_key(),
        }
    }
}

/// Optional columns revealed on demand in index queries (hidden in the Recycle Bin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OptionalColumn {
    DateCreated,
    Length,
    Author,
    Tags,
    Dimensions,
    Framerate,
}

impl OptionalColumn {
    pub const ALL: [OptionalColumn; 6] = [
        OptionalColumn::DateCreated,
        OptionalColumn::Length,
        OptionalColumn::Author,
        OptionalColumn::Tags,
        OptionalColumn::Dimensions,
        OptionalColumn::Framerate,
    ];

    pub const fn sort_column(self) -> SortColumn {
        match self {
            OptionalColumn::DateCreated => SortColumn::DateCreated,
            OptionalColumn::Length => SortColumn::Length,
            OptionalColumn::Author => SortColumn::Author,
            OptionalColumn::Tags => SortColumn::Tags,
            OptionalColumn::Dimensions => SortColumn::Dimensions,
            OptionalColumn::Framerate => SortColumn::Framerate,
        }
    }

    pub fn width_rem(self) -> f32 {
        use crate::app::layout;
        match self {
            OptionalColumn::DateCreated => layout::CREATED_COLUMN,
            OptionalColumn::Length => layout::LENGTH_COLUMN,
            OptionalColumn::Author => layout::AUTHOR_COLUMN,
            OptionalColumn::Tags => layout::TAGS_COLUMN,
            OptionalColumn::Dimensions => layout::DIMENSIONS_COLUMN,
            OptionalColumn::Framerate => layout::FRAMERATE_COLUMN,
        }
    }

    pub const fn uses_tabular_figures(self) -> bool {
        matches!(
            self,
            OptionalColumn::DateCreated
                | OptionalColumn::Length
                | OptionalColumn::Dimensions
                | OptionalColumn::Framerate
        )
    }
}

/// Column visibility items shared by the View menu and the table header context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableColumn {
    Name,
    Path,
    Size,
    DateModified,
    Optional(OptionalColumn),
}

impl TableColumn {
    pub const BASE: [TableColumn; 4] = [
        TableColumn::Name,
        TableColumn::Path,
        TableColumn::Size,
        TableColumn::DateModified,
    ];

    pub const ALL: [TableColumn; 10] = [
        TableColumn::Name,
        TableColumn::Path,
        TableColumn::Size,
        TableColumn::DateModified,
        TableColumn::Optional(OptionalColumn::DateCreated),
        TableColumn::Optional(OptionalColumn::Length),
        TableColumn::Optional(OptionalColumn::Author),
        TableColumn::Optional(OptionalColumn::Tags),
        TableColumn::Optional(OptionalColumn::Dimensions),
        TableColumn::Optional(OptionalColumn::Framerate),
    ];

    pub const fn is_locked(self) -> bool {
        matches!(self, Self::Name)
    }

    pub const fn sort_column(self) -> SortColumn {
        match self {
            Self::Name => SortColumn::Name,
            Self::Path => SortColumn::Path,
            Self::Size => SortColumn::Size,
            Self::DateModified => SortColumn::DateModified,
            Self::Optional(col) => col.sort_column(),
        }
    }

    pub fn label_key(self) -> &'static str {
        self.sort_column().label_key()
    }
}

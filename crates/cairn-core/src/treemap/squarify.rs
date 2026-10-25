// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Squarified Treemap layout (Bruls, Huizing, van Wijk) over plain sizes.

use super::geometry::TreemapRect;

/// One placed cell: the index of the size it represents and its rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SquarifiedCell {
    pub(super) index: usize,
    pub(super) rect: TreemapRect,
}

/// Fills `bounds` with one non-overlapping rectangle per entry of `sizes`,
/// keeping every aspect ratio as close to 1:1 as the row-by-row construction
/// allows.
///
/// Cells come back in placement order: largest size first, ties in input
/// order. A zero size still receives a small visible share (0.05% of the
/// total, at least one unit) so it is never lost; when every size is zero the
/// area is split evenly. Empty input or empty bounds yield no cells.
pub(super) fn squarify(sizes: &[u64], bounds: TreemapRect) -> Vec<SquarifiedCell> {
    if sizes.is_empty() || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Vec::new();
    }

    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));

    let total_size: u64 = sizes.iter().sum();
    let total_area = (bounds.width as f64) * (bounds.height as f64);
    let areas: Vec<f64> = if total_size == 0 {
        vec![total_area / (sizes.len() as f64); sizes.len()]
    } else {
        let min_weight = (total_size as f64 * 0.0005).max(1.0);
        let weights: Vec<f64> = order
            .iter()
            .map(|&index| match sizes[index] {
                0 => min_weight,
                size => size as f64,
            })
            .collect();
        let total_weight: f64 = weights.iter().sum();
        weights
            .iter()
            .map(|&weight| (weight / total_weight) * total_area)
            .collect()
    };

    let mut cells = Vec::with_capacity(sizes.len());
    let mut remaining = bounds;
    let mut row: Vec<(usize, f64)> = Vec::new();
    let mut stats = RowStats::default();

    let mut position = 0;
    while position < order.len() {
        let index = order[position];
        let area = areas[position];

        let side = remaining.shorter_side() as f64;
        let accept = row.is_empty()
            || side <= 1e-4
            || stats.with(area).worst_aspect_ratio(side) <= stats.worst_aspect_ratio(side);
        if accept {
            row.push((index, area));
            stats = stats.with(area);
            position += 1;
        } else {
            place_row(&row, &mut remaining, &mut cells);
            row.clear();
            stats = RowStats::default();
        }
    }
    if !row.is_empty() {
        place_row(&row, &mut remaining, &mut cells);
    }
    cells
}

/// Running totals of the row being built, so testing one more candidate does
/// not copy or rescan the row.
#[derive(Clone, Copy, Debug, Default)]
struct RowStats {
    sum: f64,
    /// Smallest and largest area above the degenerate threshold, if any.
    positive: Option<(f64, f64)>,
}

impl RowStats {
    fn with(self, area: f64) -> Self {
        let positive = if area > 1e-9 {
            Some(match self.positive {
                Some((min, max)) => (min.min(area), max.max(area)),
                None => (area, area),
            })
        } else {
            self.positive
        };
        Self {
            sum: self.sum + area,
            positive,
        }
    }

    /// Worst width:height ratio of any cell in the row laid along `side`.
    /// The ratio of one cell grows with its area on one side and shrinks on
    /// the other, so only the smallest and largest areas can be the worst.
    fn worst_aspect_ratio(&self, side: f64) -> f64 {
        if side <= 1e-4 {
            return f64::MAX;
        }
        if self.sum <= 1e-9 {
            return 1.0;
        }
        let Some((min, max)) = self.positive else {
            return 1.0;
        };
        let sum_sq = self.sum * self.sum;
        let side_sq = side * side;
        ((side_sq * max) / sum_sq).max(sum_sq / (side_sq * min))
    }
}

/// Places one finished row along the shorter side of `bounds` and shrinks
/// `bounds` to the area that remains. The last cell absorbs rounding so the
/// row spans the side exactly.
fn place_row(row: &[(usize, f64)], bounds: &mut TreemapRect, cells: &mut Vec<SquarifiedCell>) {
    if row.is_empty() {
        return;
    }
    let row_area: f64 = row.iter().map(|(_, area)| *area).sum();
    let last = row.len() - 1;

    if bounds.width >= bounds.height {
        let thickness = if bounds.height > 0.0 {
            ((row_area / (bounds.height as f64)) as f32).min(bounds.width)
        } else {
            0.0
        };
        let bottom = bounds.y + bounds.height;
        let mut y = bounds.y;
        for (position, &(index, area)) in row.iter().enumerate() {
            let available = (bottom - y).max(0.0);
            let height = if position == last {
                available
            } else if thickness > 0.0 {
                ((area as f32) / thickness).min(available)
            } else {
                0.0
            };
            cells.push(SquarifiedCell {
                index,
                rect: TreemapRect {
                    x: bounds.x,
                    y,
                    width: thickness,
                    height,
                },
            });
            y += height;
        }
        bounds.x += thickness;
        bounds.width = (bounds.width - thickness).max(0.0);
    } else {
        let thickness = if bounds.width > 0.0 {
            ((row_area / (bounds.width as f64)) as f32).min(bounds.height)
        } else {
            0.0
        };
        let right = bounds.x + bounds.width;
        let mut x = bounds.x;
        for (position, &(index, area)) in row.iter().enumerate() {
            let available = (right - x).max(0.0);
            let width = if position == last {
                available
            } else if thickness > 0.0 {
                ((area as f32) / thickness).min(available)
            } else {
                0.0
            };
            cells.push(SquarifiedCell {
                index,
                rect: TreemapRect {
                    x,
                    y: bounds.y,
                    width,
                    height: thickness,
                },
            });
            x += width;
        }
        bounds.y += thickness;
        bounds.height = (bounds.height - thickness).max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_inside(cells: &[SquarifiedCell], bounds: TreemapRect) {
        for cell in cells {
            assert!(cell.rect.area() > 0.0, "{cell:?} has no area");
            assert!(cell.rect.x >= bounds.x - 0.1);
            assert!(cell.rect.y >= bounds.y - 0.1);
            assert!(cell.rect.x + cell.rect.width <= bounds.x + bounds.width + 0.1);
            assert!(cell.rect.y + cell.rect.height <= bounds.y + bounds.height + 0.1);
        }
    }

    #[test]
    fn empty_input_or_empty_bounds_yield_no_cells() {
        assert!(squarify(&[], TreemapRect::new(0.0, 0.0, 100.0, 100.0)).is_empty());
        assert!(squarify(&[100], TreemapRect::new(0.0, 0.0, 0.0, 100.0)).is_empty());
        assert!(squarify(&[100], TreemapRect::new(0.0, 0.0, 100.0, 0.0)).is_empty());
    }

    #[test]
    fn a_single_size_fills_the_bounds() {
        let bounds = TreemapRect::new(10.0, 20.0, 800.0, 600.0);
        let cells = squarify(&[500], bounds);

        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].index, 0);
        assert!((cells[0].rect.x - 10.0).abs() < 1e-3);
        assert!((cells[0].rect.y - 20.0).abs() < 1e-3);
        assert!((cells[0].rect.width - 800.0).abs() < 1e-3);
        assert!((cells[0].rect.height - 600.0).abs() < 1e-3);
    }

    #[test]
    fn cells_tile_the_bounds_in_size_order() {
        let bounds = TreemapRect::new(0.0, 0.0, 1000.0, 800.0);
        let sizes = [150, 600, 50, 200];
        let cells = squarify(&sizes, bounds);

        assert_eq!(
            cells.iter().map(|cell| cell.index).collect::<Vec<_>>(),
            vec![1, 3, 0, 2],
            "placement runs largest first and reports input indices"
        );
        assert_inside(&cells, bounds);
        let covered: f32 = cells.iter().map(|cell| cell.rect.area()).sum();
        assert!((covered - bounds.area()).abs() / bounds.area() < 0.01);

        // Areas stay proportional to the sizes.
        let total: u64 = sizes.iter().sum();
        for cell in &cells {
            let share = sizes[cell.index] as f32 / total as f32;
            assert!((cell.rect.area() / bounds.area() - share).abs() < 0.01);
        }
    }

    #[test]
    fn equal_sizes_keep_their_input_order() {
        let cells = squarify(&[7, 7, 7], TreemapRect::new(0.0, 0.0, 300.0, 100.0));
        assert_eq!(
            cells.iter().map(|cell| cell.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn all_zero_sizes_split_the_area_evenly() {
        let bounds = TreemapRect::new(0.0, 0.0, 200.0, 100.0);
        let cells = squarify(&[0, 0], bounds);

        assert_eq!(cells.len(), 2);
        assert_inside(&cells, bounds);
        assert!((cells[0].rect.area() - cells[1].rect.area()).abs() < 1e-2);
    }

    #[test]
    fn zero_sizes_beside_real_ones_stay_visible() {
        let bounds = TreemapRect::new(0.0, 0.0, 800.0, 600.0);
        let cells = squarify(&[10_000_000, 0, 0], bounds);

        assert_eq!(cells.len(), 3);
        assert_inside(&cells, bounds);
    }

    #[test]
    fn rows_keep_cells_close_to_square() {
        let sizes: Vec<u64> = (1..=40).map(|value| value * 37).collect();
        let bounds = TreemapRect::new(0.0, 0.0, 1200.0, 700.0);
        let cells = squarify(&sizes, bounds);

        assert_eq!(cells.len(), sizes.len());
        assert_inside(&cells, bounds);
        let worst = cells
            .iter()
            .map(|cell| cell.rect.longer_side() / cell.rect.shorter_side())
            .fold(0.0_f32, f32::max);
        assert!(worst < 4.0, "worst aspect ratio {worst:.2}");
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0
use super::{OVERSCAN_ROWS, TableState, format_timestamp, visible_range};
use crate::table::columns::{ColumnBoundary, ColumnWidths, SortColumn};
use cairn_protocol::message::FileRowData;

const ROW_HEIGHT: f32 = 22.0;
const VIEWPORT: f32 = 440.0;
const PAGE: usize = 200;

fn row(name: &str) -> FileRowData {
    FileRowData {
        name: name.to_string(),
        path: "C:\\".to_string(),
        ..Default::default()
    }
}

fn table(loaded: usize, total: usize) -> TableState {
    let mut table = TableState {
        total_rows: total,
        viewport_height: VIEWPORT,
        ..Default::default()
    };
    table.set_rows(
        (0..loaded).map(|i| row(&format!("file{i}.rs"))).collect(),
        total,
    );
    table
}

#[test]
fn visible_range_size_depends_only_on_the_viewport() {
    let small = visible_range(0.0, VIEWPORT, ROW_HEIGHT, 100);
    let huge = visible_range(0.0, VIEWPORT, ROW_HEIGHT, 100_000);
    assert_eq!(
        small, huge,
        "result-set size must not change the rendered row count"
    );

    let rows_in_viewport = (VIEWPORT / ROW_HEIGHT) as usize;
    assert!(
        huge.len() <= rows_in_viewport + OVERSCAN_ROWS * 2 + 1,
        "rendered {} rows for a {rows_in_viewport} row viewport",
        huge.len()
    );
    assert!(
        huge.len() >= rows_in_viewport,
        "the viewport must be fully covered"
    );
}

#[test]
fn visible_range_is_clamped_to_the_result_set() {
    assert_eq!(visible_range(0.0, VIEWPORT, ROW_HEIGHT, 0), 0..0);
    assert_eq!(visible_range(0.0, VIEWPORT, ROW_HEIGHT, 4), 0..4);
    assert_eq!(
        visible_range(4.0 * ROW_HEIGHT, VIEWPORT, ROW_HEIGHT, 5),
        0..5
    );
}

#[test]
fn visible_range_tracks_the_scroll_offset() {
    let top = visible_range(0.0, VIEWPORT, ROW_HEIGHT, 100_000);
    let deep = visible_range(2_000.0, VIEWPORT, ROW_HEIGHT, 100_000);
    assert!(deep.start > top.start + OVERSCAN_ROWS);
    let bound = (VIEWPORT / ROW_HEIGHT) as usize + OVERSCAN_ROWS * 2 + 1;
    assert!(deep.len() <= bound && top.len() <= bound);
    assert_eq!(
        deep.start + OVERSCAN_ROWS,
        (2_000.0 / ROW_HEIGHT).floor() as usize
    );
}

#[test]
fn visible_range_rejects_degenerate_geometry() {
    assert_eq!(visible_range(0.0, 0.0, ROW_HEIGHT, 10), 0..0);
    assert_eq!(visible_range(0.0, VIEWPORT, 0.0, 10), 0..0);
    assert_eq!(visible_range(1.0e9, VIEWPORT, ROW_HEIGHT, 10), 0..10);
}

#[test]
fn a_million_rows_render_a_constant_number_of_slots() {
    let table = table(200, 1_000_000);
    let range = table.visible_range(ROW_HEIGHT);
    assert!(
        range.len() < 64,
        "{} slots for one million matches",
        range.len()
    );
}

#[test]
fn scroll_top_is_clamped_to_the_band() {
    let mut table = table(100, 100_000);
    table.set_scroll_top(table.scroll_top + 5_000_000.0, ROW_HEIGHT);
    assert_eq!(table.scroll_top, table.max_scroll_top(ROW_HEIGHT));
    table.set_scroll_top(table.scroll_top - 1.0e9, ROW_HEIGHT);
    assert_eq!(table.scroll_top, 0.0);
}

#[test]
fn unfetched_rows_still_extend_the_band() {
    let table = table(1, 1_000_000);
    assert_eq!(table.loaded_range(), 0..1);
    assert_eq!(table.scroll_extent(ROW_HEIGHT), 1_000_000.0 * ROW_HEIGHT);
    assert!(!table.is_loaded(500));
    assert!(table.is_loaded(0));
    assert!(table.row(500).is_none());
    assert_eq!(table.row(0).map(|r| r.name.as_str()), Some("file0.rs"));
}

#[test]
fn ensure_row_visible_moves_the_minimum_amount() {
    let mut table = table(10_000, 10_000);
    table.ensure_row_visible(3, ROW_HEIGHT);
    assert_eq!(table.scroll_top, 0.0, "row 3 is already visible");

    table.ensure_row_visible(500, ROW_HEIGHT);
    assert!(table.scroll_top > 0.0);
    let range = table.visible_range(ROW_HEIGHT);
    assert!(range.contains(&500), "row 500 must be inside {range:?}");
    assert!(
        range.len() <= (VIEWPORT / ROW_HEIGHT) as usize + OVERSCAN_ROWS * 2 + 1,
        "row 500 rendered {} slots",
        range.len()
    );
}

#[test]
fn overlapping_viewport_extends_the_window_without_dropping_visible_rows() {
    let before = table(200, 100_000);
    let visible = 180..220;
    assert_eq!(before.page_request(visible, PAGE), Some((200, PAGE)));

    let mut after = table(200, 100_000);
    after.apply_page(
        200,
        (200..400).map(|i| row(&format!("file{i}.rs"))).collect(),
    );
    assert_eq!(after.loaded_range(), 0..400, "contiguous pages append");
    assert!(after.row(180).is_some(), "already visible rows survive");
}

#[test]
fn a_scrollback_jump_pages_to_the_viewport() {
    let mut table = table(200, 1_000_000);
    table.viewport_height = VIEWPORT;
    table.set_scroll_top(table.scroll_top + 44_000.0, ROW_HEIGHT); // row 2000
    let visible = table.visible_range(ROW_HEIGHT);
    let (offset, limit) = table
        .page_request(visible.clone(), PAGE)
        .expect("the jumped range needs a page");
    assert_eq!(offset % PAGE, 0, "a jump lands on a page boundary");
    assert!(
        offset <= visible.start && offset + limit >= visible.end,
        "the requested page must cover the viewport {visible:?}"
    );

    let mut after = table;
    after.apply_page(offset, (offset..offset + limit).map(|_| row("x")).collect());
    assert_eq!(
        after.loaded_range(),
        offset..offset + limit,
        "a jump replaces the window"
    );
    assert!(visible.clone().all(|ix| after.is_loaded(ix)));
    assert!(
        after.page_request(visible, PAGE).is_none(),
        "no repeat request"
    );
}

#[test]
fn no_page_is_requested_when_everything_is_loaded() {
    let table = table(10, 10);
    assert_eq!(table.page_request(0..10, PAGE), None);
    assert_eq!(table.page_request(0..100_000, PAGE), None);
    assert_eq!(TableState::default().page_request(0..10, PAGE), None);
}

#[test]
fn an_empty_page_is_not_requested_again() {
    let mut table = table(200, 1_000);
    let visible = 180..400;
    assert_eq!(table.page_request(visible.clone(), PAGE), Some((200, PAGE)));

    table.apply_page(200, Vec::new());
    assert_eq!(table.loaded_range(), 0..200, "an empty page adds no rows");
    assert_eq!(
        table.page_request(visible, PAGE),
        None,
        "the same offset must not be fetched again"
    );
    assert_eq!(table.page_request(200..400, PAGE), None);
    assert!(
        table.page_request(600..800, PAGE).is_some(),
        "a different page is still reachable after one empty page"
    );
}

#[test]
fn a_contained_non_empty_page_is_not_marked_empty() {
    let mut table = table(0, 1_000);
    table.apply_page(200, (200..400).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(table.loaded_range(), 200..400);

    table.apply_page(200, (200..250).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(
        table.loaded_range(),
        200..400,
        "a contained page adds nothing"
    );

    table.apply_page(600, (600..800).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(table.loaded_range(), 600..800);
    assert_eq!(
        table.page_request(200..300, PAGE),
        Some((200, PAGE)),
        "valid rows can be fetched again after jumping away"
    );
}

#[test]
fn directional_truncation_prunes_empty_page_offsets() {
    let mut table = table(0, 10_000);

    table.apply_page(1000, (1000..3000).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(table.loaded_range(), 1000..3000);

    table.apply_page(4000, Vec::new());
    assert!(table.empty_page_offsets.contains(&4000));

    table.apply_page(800, (800..1000).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(table.loaded_range(), 800..2800);
    assert!(
        !table.empty_page_offsets.contains(&4000),
        "offsets beyond the truncated tail must be pruned"
    );

    table.apply_page(500, Vec::new());
    assert!(table.empty_page_offsets.contains(&500));

    table.apply_page(2800, (2800..3000).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(table.loaded_range(), 1000..3000);
    assert!(
        !table.empty_page_offsets.contains(&500),
        "offsets before the drained head must be pruned"
    );
}

#[test]
fn non_empty_page_clears_prior_empty_page_offset() {
    let mut table = table(0, 10_000);
    table.apply_page(400, Vec::new());
    assert!(table.empty_page_offsets.contains(&400));
    assert_eq!(table.page_request(400..600, PAGE), None);

    table.apply_page(400, (400..600).map(|i| row(&format!("f{i}"))).collect());
    assert!(
        !table.empty_page_offsets.contains(&400),
        "landing non-empty rows must remove offset from empty_page_offsets"
    );

    table.apply_page(5000, (5000..5200).map(|i| row(&format!("f{i}"))).collect());
    assert_eq!(table.page_request(400..600, PAGE), Some((400, PAGE)));
}

#[test]
fn a_new_query_clears_recorded_empty_pages() {
    let mut table = table(200, 1_000);
    table.apply_page(200, Vec::new());
    assert_eq!(table.page_request(200..400, PAGE), None);

    table.set_rows(
        (0..200).map(|i| row(&format!("file{i}.rs"))).collect(),
        1_000,
    );
    assert_eq!(
        table.page_request(200..400, PAGE),
        Some((200, PAGE)),
        "a fresh result set may retry an offset the old one could not fill"
    );
}

#[test]
fn a_short_page_advances_the_next_request_without_misaligning_rows() {
    let mut table = table(200, 1_000);
    table.apply_page(
        200,
        (200..250).map(|i| row(&format!("file{i}.rs"))).collect(),
    );

    assert_eq!(table.loaded_range(), 0..250);
    assert_eq!(
        table.row(200).map(|r| r.name.as_str()),
        Some("file200.rs"),
        "the first row of a short page keeps its absolute index"
    );
    assert_eq!(
        table.page_request(180..400, PAGE),
        Some((250, PAGE)),
        "the hole between 250 and total starts a new request, not the old one"
    );
}

#[test]
fn selection_uses_absolute_indices_and_survives_a_moving_window() {
    let mut table = table(2, 10_000);
    table.select_row(1, false);
    table.apply_page(2, (2..202).map(|_| row("x")).collect());

    assert!(table.selected_indices.contains(&1));
    table.move_selection_down(false);
    assert_eq!(table.focused_idx, Some(2));
    table.move_selection_down(false);
    assert_eq!(table.focused_idx, Some(3));
    table.select_all_loaded(usize::MAX);
    assert_eq!(table.selected_indices.len(), table.loaded_range().len());
}

#[test]
fn select_all_loaded_stops_at_the_limit() {
    let mut table = table(1_200, 2_000);

    assert_eq!(table.select_all_loaded(1_000), 1_000);
    assert_eq!(table.selected_indices.len(), 1_000);
    assert!(table.selected_indices.contains(&999));
    assert!(!table.selected_indices.contains(&1_000));
    assert_eq!(table.focused_idx, Some(0));
    assert_eq!(table.selection_anchor, Some(0));
}

#[test]
fn selection_cannot_move_past_the_loaded_window() {
    let mut table = table(2, 1_000_000);
    table.select_row(1, false);
    for _ in 0..10 {
        table.move_selection_down(false);
    }
    assert_eq!(table.focused_idx, Some(1));
}

#[test]
fn shift_ranges_from_a_fixed_anchor_and_ctrl_toggles_one_row() {
    let mut table = table(2, 100);
    table.select_row(2, false);
    table.select_row(6, true);

    assert_eq!(table.selection_anchor, Some(2));
    assert_eq!(table.focused_idx, Some(6));
    assert_eq!(
        table.selected_indices,
        std::collections::HashSet::from([2, 3, 4, 5, 6])
    );

    table.toggle_row(4);
    assert!(!table.selected_indices.contains(&4));
    assert_eq!(table.selection_anchor, Some(4));

    table.select_range(1, 3, true);
    assert!(table.selected_indices.contains(&1));
    assert_eq!(table.selection_anchor, Some(1));
    assert_eq!(
        table.selected_indices,
        std::collections::HashSet::from([1, 2, 3, 5, 6])
    );

    table.clear_selection();
    assert_eq!(table.selection_anchor, None);
}

#[test]
fn set_rows_resets_the_viewport_and_selection() {
    let mut table = table(10, 10);
    table.select_row(7, false);
    table.set_scroll_top(900.0, ROW_HEIGHT);

    table.set_rows(vec![row("a.rs")], 1);
    assert_eq!(table.scroll_top, 0.0);
    assert!(table.selected_indices.is_empty());
    assert_eq!(table.focused_idx, None);
    assert_eq!(table.loaded_range(), 0..1);
}

fn widths() -> ColumnWidths {
    ColumnWidths {
        name: 20.0,
        path: 22.5,
        size: 6.875,
        date: 10.0,
    }
}

const MINIMA: [f32; 4] = [12.0, 8.0, 6.0, 9.5];

#[test]
fn dragging_a_boundary_trades_width_between_its_two_lanes() {
    let before = widths();
    let after = before.resized(ColumnBoundary::NAME_PATH, 3.0, MINIMA);
    assert_eq!(after.name, 23.0);
    assert_eq!(after.path, 19.5);
    assert_eq!(after.size, before.size, "a drag touches only two lanes");
    assert_eq!(after.date, before.date);
    assert!((after.total() - before.total()).abs() < f32::EPSILON);
}

#[test]
fn a_drag_stops_when_either_neighbour_reaches_its_minimum() {
    let collapsed_name = widths().resized(ColumnBoundary::NAME_PATH, -100.0, MINIMA);
    assert_eq!(collapsed_name.name, MINIMA[0]);
    assert_eq!(collapsed_name.path, 30.5);

    let collapsed_path = widths().resized(ColumnBoundary::NAME_PATH, 100.0, MINIMA);
    assert_eq!(collapsed_path.path, MINIMA[1]);
    assert_eq!(collapsed_path.name, 34.5);
}

#[test]
fn the_numeric_boundaries_move_their_own_lanes() {
    let after = widths().resized(ColumnBoundary::SIZE_DATE, -0.5, MINIMA);
    assert_eq!(after.size, 6.375);
    assert_eq!(after.date, 10.5);
    assert_eq!(after.name, 20.0);
    assert_eq!(after.path, 22.5);
}

#[test]
fn a_background_refresh_keeps_the_viewport_and_drops_dead_selection() {
    let mut table = table(10, 100);
    table.select_row(7, false);
    table.set_scroll_top(400.0, ROW_HEIGHT);

    table.refresh_rows((0..5).map(|i| row(&format!("r{i}.rs"))).collect(), 5);

    assert_eq!(
        table.scroll_top, 400.0,
        "an index that grew must not pull the reader back to the top"
    );
    assert!(
        table.selected_indices.is_empty(),
        "row 7 no longer names the row it was selected on"
    );
    assert_eq!(table.total_rows(), 5);
    assert_eq!(table.loaded_range(), 0..5);
}

#[test]
fn a_background_refresh_does_not_replace_a_deep_window() {
    let mut table = table(0, 1_000_000);
    table.apply_page(
        70_000,
        (70_000..70_200)
            .map(|i| row(&format!("old{i}.rs")))
            .collect(),
    );

    table.refresh_rows(
        (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
        1_000_100,
    );

    assert_eq!(table.total_rows(), 1_000_100);
    assert_eq!(
        table.loaded_range(),
        70_000..70_200,
        "the page the reader is looking at must survive a first-page refresh"
    );
    assert!(table.row(70_000).is_some());
    assert!(
        table.row(0).is_none(),
        "the first page must not be spliced into a disjoint window"
    );
}

#[test]
fn a_top_anchored_refresh_keeps_rows_below_its_first_page() {
    let mut table = table(400, 1_000);
    table.refresh_rows(
        (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
        1_010,
    );

    assert_eq!(table.loaded_range(), 0..400);
    assert_eq!(table.row(0).map(|r| r.name.as_str()), Some("new0.rs"));
    assert_eq!(
        table.row(200).map(|r| r.name.as_str()),
        Some("file200.rs"),
        "a page shorter than the window updates only its own rows"
    );
}

#[test]
fn an_empty_background_refresh_keeps_the_window() {
    let mut table = table(200, 1_000);
    table.refresh_rows(Vec::new(), 1_200);
    assert_eq!(table.loaded_range(), 0..200);
    assert_eq!(table.total_rows(), 1_200);
}

#[test]
fn a_deep_window_refresh_revalidates_the_visible_rows() {
    let mut table = table(0, 1_000_000);
    table.apply_page(
        70_000,
        (70_000..70_200)
            .map(|i| row(&format!("old{i}.rs")))
            .collect(),
    );
    table.set_scroll_top(70_000.0 * ROW_HEIGHT, ROW_HEIGHT);
    let visible = table.visible_range(ROW_HEIGHT);
    assert!(visible.start < 70_000 && visible.end > 70_000);

    table.refresh_rows(
        (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
        1_000_100,
    );
    assert_eq!(
        table.row(70_000).map(|r| r.name.as_str()),
        Some("old70000.rs"),
        "the rows already on screen survive until their replacement lands"
    );

    let (offset, limit) = table
        .page_request(visible.clone(), PAGE)
        .expect("the refreshed generation must fetch the visible region");
    assert_eq!(offset % PAGE, 0, "a revalidation page is page-aligned");
    assert!(
        offset <= visible.start && offset + limit >= visible.end,
        "the requested page {offset}..{} must cover {visible:?}",
        offset + limit
    );

    table.note_page_request(offset);
    table.apply_page(
        offset,
        (offset..offset + limit)
            .map(|i| row(&format!("fresh{i}.rs")))
            .collect(),
    );

    assert_eq!(table.loaded_range(), offset..offset + limit);
    assert!(
        visible.clone().all(|ix| table
            .row(ix)
            .is_some_and(|row| row.name.starts_with("fresh"))),
        "every visible row must come from the refreshed generation"
    );
    assert_eq!(
        table.page_request(visible, PAGE),
        None,
        "the replaced window already holds the visible rows"
    );
}

#[test]
fn a_page_from_before_the_refresh_cannot_consume_the_revalidation() {
    let mut table = table(0, 1_000_000);
    table.apply_page(
        70_000,
        (70_000..70_200)
            .map(|i| row(&format!("old{i}.rs")))
            .collect(),
    );
    table.set_scroll_top(70_000.0 * ROW_HEIGHT, ROW_HEIGHT);
    let visible = table.visible_range(ROW_HEIGHT);

    table.refresh_rows(
        (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
        1_000_100,
    );

    table.apply_page(0, (0..200).map(|i| row(&format!("stale{i}.rs"))).collect());
    assert_eq!(
        table.loaded_range(),
        70_000..70_200,
        "the stale in-flight page must be ignored"
    );

    let (offset, limit) = table
        .page_request(visible.clone(), PAGE)
        .expect("revalidation request must still be issued");
    table.note_page_request(offset);
    table.apply_page(
        offset,
        (offset..offset + limit)
            .map(|i| row(&format!("fresh{i}.rs")))
            .collect(),
    );

    assert_eq!(table.loaded_range(), offset..offset + limit);
    assert!(
        visible
            .clone()
            .all(|ix| table.row(ix).is_some_and(|r| r.name.starts_with("fresh")))
    );
}

#[test]
fn a_top_window_inside_the_fresh_page_does_not_refetch() {
    let mut table = table(400, 1_000);
    table.select_row(7, false);
    table.refresh_rows(
        (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
        1_010,
    );

    assert!(table.selected_indices.is_empty());
    assert_eq!(table.focused_idx, None);
    assert_eq!(table.selection_anchor, None);
    assert_eq!(table.page_request(0..40, PAGE), None);
    assert_eq!(
        table.page_request(0..200, PAGE),
        None,
        "a viewport inside the fresh page is already current"
    );
}

#[test]
fn an_empty_revalidation_page_drops_the_stale_window() {
    let mut table = table(0, 1_000_000);
    table.apply_page(
        70_000,
        (70_000..70_200)
            .map(|i| row(&format!("old{i}.rs")))
            .collect(),
    );
    table.set_scroll_top(70_000.0 * ROW_HEIGHT, ROW_HEIGHT);
    let visible = table.visible_range(ROW_HEIGHT);
    table.refresh_rows(
        (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
        1_000_100,
    );

    let (offset, _) = table
        .page_request(visible.clone(), PAGE)
        .expect("the visible region needs a page");
    table.note_page_request(offset);
    table.apply_page(offset, Vec::new());

    assert_eq!(table.loaded_range(), 0..0);
    assert!(
        table.row(70_000).is_none(),
        "the stale rows must not outlive their replacement"
    );
    assert_eq!(
        table.page_request(visible, PAGE),
        None,
        "the empty offset is retired instead of requested every frame"
    );
}

#[test]
fn set_rows_empty_clears_ranges_and_total() {
    let mut table = table(10, 100);
    table.set_rows(Vec::new(), 0);
    assert_eq!(table.total_rows(), 0);
    assert_eq!(table.loaded_range(), 0..0);
    assert_eq!(table.scroll_top, 0.0);
    assert!(table.selected_indices.is_empty());
}

#[test]
fn only_exactly_one_loaded_selection_can_be_renamed() {
    let mut table = table(5, 100);
    assert_eq!(table.sole_selected_row(), None);

    table.select_row(2, false);
    assert_eq!(table.sole_selected_row(), Some(2));

    table.select_row(3, true);
    assert_eq!(
        table.sole_selected_row(),
        None,
        "a range selection has no single rename target"
    );

    let mut unloaded = TableState::default();
    unloaded.selected_indices.insert(500);
    assert_eq!(
        unloaded.sole_selected_row(),
        None,
        "a row outside the fetched window has no name to edit"
    );
}

#[test]
fn a_rename_updates_the_loaded_row_in_place() {
    let mut table = TableState::default();
    table.set_rows(
        vec![
            FileRowData {
                name: "first.txt".to_string(),
                path: r"C:\work".to_string(),
                name_highlights: vec![(0, 5)],
                ..Default::default()
            },
            FileRowData {
                name: "second.txt".to_string(),
                path: r"C:\work".to_string(),
                ..Default::default()
            },
        ],
        2,
    );
    table.select_row(1, false);
    let sort = table.primary_sort();

    assert!(table.rename_row(r"C:\work\second.txt", "renamed.md"));

    let row = table.row(1).expect("the row keeps its position");
    assert_eq!(row.name, "renamed.md");
    assert_eq!(row.path, r"C:\work", "a rename never moves the parent");
    assert_eq!(
        crate::app::join_row_path(&row.path, &row.name),
        r"C:\work\renamed.md"
    );
    assert!(
        row.name_highlights.is_empty(),
        "the old highlight ranges no longer describe the new name"
    );
    assert_eq!(table.focused_idx, Some(1));
    assert_eq!(table.selected_indices, std::collections::HashSet::from([1]));
    assert_eq!(table.total_rows(), 2, "a rename keeps the result count");
    assert_eq!(
        table.primary_sort(),
        sort,
        "a rename never reorders the table"
    );
    assert_eq!(
        table.row(0).map(|row| row.name.as_str()),
        Some("first.txt"),
        "the neighbouring row is untouched"
    );

    assert!(!table.rename_row(r"C:\work\missing.txt", "other.md"));
    assert_eq!(
        table.row(1).map(|row| row.name.as_str()),
        Some("renamed.md")
    );
}

#[test]
fn sort_toggles_direction_only_on_the_active_column() {
    let mut table = TableState::default();
    table.toggle_sort(SortColumn::Size);
    assert_eq!(table.primary_sort(), (SortColumn::Size, true));
    table.toggle_sort(SortColumn::Size);
    assert_eq!(table.primary_sort(), (SortColumn::Size, false));
    table.toggle_sort(SortColumn::Name);
    assert_eq!(table.primary_sort(), (SortColumn::Name, true));
}

#[test]
fn an_absent_timestamp_stays_blank() {
    assert_eq!(format_timestamp(0), "");
}

#[test]
fn test_apply_page_bidirectional_and_overlapping() {
    let mut table = TableState::default();
    let page1 = (100..200).map(|i| row(&format!("f{i}"))).collect();
    table.apply_page(100, page1);
    assert_eq!(table.loaded_range(), 100..200);

    let page2 = (180..250).map(|i| row(&format!("f{i}"))).collect();
    table.apply_page(180, page2);
    assert_eq!(table.loaded_range(), 100..250);
    assert_eq!(table.row(100).unwrap().name, "f100");
    assert_eq!(table.row(249).unwrap().name, "f249");

    let page3 = (50..120).map(|i| row(&format!("f{i}"))).collect();
    table.apply_page(50, page3);
    assert_eq!(table.loaded_range(), 50..250);
    assert_eq!(table.row(50).unwrap().name, "f50");
    assert_eq!(table.row(249).unwrap().name, "f249");

    let page4 = (80..150).map(|i| row(&format!("f{i}"))).collect();
    table.apply_page(80, page4);
    assert_eq!(table.loaded_range(), 50..250);

    let page5 = (1000..1050).map(|i| row(&format!("f{i}"))).collect();
    table.apply_page(1000, page5);
    assert_eq!(table.loaded_range(), 1000..1050);
}

#[test]
fn shift_sort_tracks_precedence_and_hiding_column_prunes_keys() {
    use crate::table::columns::OptionalColumn;

    let mut table = TableState::default();
    table.toggle_sort(SortColumn::Size);
    table.shift_toggle_sort(SortColumn::Length);
    assert_eq!(table.sort_rank(SortColumn::Size), Some((1, true)));
    assert_eq!(table.sort_rank(SortColumn::Length), Some((2, true)));

    table.shift_toggle_sort(SortColumn::Length);
    assert_eq!(table.sort_rank(SortColumn::Length), Some((2, false)));

    assert!(table.remove_sort_column(OptionalColumn::Length.sort_column()));
    assert_eq!(table.sort_rank(SortColumn::Length), None);
    assert_eq!(table.sort_rank(SortColumn::Size), Some((1, true)));

    assert!(table.remove_sort_column(SortColumn::Size));
    assert_eq!(table.primary_sort(), (SortColumn::Name, true));
}

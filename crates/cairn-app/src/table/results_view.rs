// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Results table view: headers, rows, placeholders, skeletons, and empty / offline states.

use crate::app::{element_id, join_row_path, layout, split_name_highlights, CairnApp};
use crate::service::service_issue::ServiceIssue;
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::command_groups::GroupCommand;
use crate::shell::commands::RESULTS_CONTEXT;
use crate::shell::menu_rules::IndexMenuState;
use crate::shell::theme::{self, ShellTokens};
use crate::table::model::{format_timestamp, ColumnBoundary, SortColumn};
use crate::view::announce_shortcut;
use cairn_core::format_size_bytes;
use cairn_core::treemap::TreemapCategory;
use cairn_protocol::message::FileRowData;
use cairn_protocol::status::ContentIndexState;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::StyledExt;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    h_flex, v_flex, Icon, IconName, InteractiveElementExt as _, Sizable as _,
};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use gpui_kit::gpui::{ease_out_quint, Animation, AnimationExt};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// Measured on the live 1.2M index: filtered GUI queries answer in 20-80 ms (one 157 ms outlier),
/// while the all-files first page takes ~2.7 s. 180 ms clears every observed fast search and page
/// fetch, so neither the first page nor a scroll-driven placeholder flashes, and still appears
/// promptly when the wait is real.
const SKELETON_DELAY: Duration = Duration::from_millis(180);
/// One full traversal of the row-to-row pulse.
const SKELETON_PULSE: Duration = Duration::from_millis(1400);
/// The pulse is decoration over a static structure; 30 fps is enough and costs half the repaints.
const SKELETON_MAX_FPS: f32 = 30.0;
/// How long the first page takes to fade in where the skeleton used to be.
const ROW_REVEAL: Duration = Duration::from_millis(140);
/// Placeholder-bar geometry is local to the skeleton: no real control shares this height or width.
const SKELETON_BAR_HEIGHT: f32 = 0.75;
const SKELETON_DATE_WIDTH: f32 = 7.0;
/// The resize affordance is a physical hit strip; the painted line is a hairline inside it.
const RESIZE_HIT_WIDTH: Pixels = px(8.0);
const RESIZE_LINE_OFFSET: Pixels = px(3.0);
/// Wide enough for the row commands without letting a long path set the menu's width.
const CONTEXT_MENU_MIN_WIDTH: f32 = 12.0;
/// A soft shade at the bottom edge, not a background-colored veil. A veil over interactive rows
/// washes out their hover and selection fills; this only deepens the surface for a few pixels, so
/// the last row stays legible and its state stays visible.
const ROWS_EDGE_SHADE_HEIGHT: f32 = 3.0 * layout::STEP;
const ROWS_EDGE_SHADE_ALPHA: f32 = 0.06;
/// The shade ramps out over the final row, so reaching the end of the list has no visible step.
const ROWS_EDGE_SHADE_RAMP_ROWS: f32 = 1.0;

/// Why a `content:` query has no rows to show. Silence would read as "no file matches", which is
/// only true once the content index is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentQueryNotice {
    Local,
    Loading,
    /// The status read itself failed; the lifecycle is unknown, not `Disabled`.
    Unreadable,
    Disabled,
    Backfilling,
    Error,
}

impl ContentQueryNotice {
    fn keys(self) -> (&'static str, &'static str) {
        match self {
            Self::Local => ("empty_content_local_title", "empty_content_local_hint"),
            Self::Loading => ("empty_content_loading_title", "empty_content_loading_hint"),
            Self::Unreadable => (
                "empty_content_unreadable_title",
                "empty_content_unreadable_hint",
            ),
            Self::Disabled => (
                "empty_content_disabled_title",
                "empty_content_disabled_hint",
            ),
            Self::Backfilling => (
                "empty_content_backfilling_title",
                "empty_content_backfilling_hint",
            ),
            Self::Error => ("empty_content_error_title", "empty_content_error_hint"),
        }
    }
}

/// A disconnected service shows its own offline band, so this only explains states where a query
/// actually reached an indexer. Local fallback is checked first: the user-privilege indexer
/// carries file names only, even though it is "connected", so a content query can never be
/// answered there.
fn content_query_notice(state: &IndexMenuState<'_>) -> Option<ContentQueryNotice> {
    if state.local_mode {
        return Some(ContentQueryNotice::Local);
    }
    if !state.service_connected {
        return None;
    }
    if state.content_loading {
        return Some(ContentQueryNotice::Loading);
    }
    if state.content_failed {
        return Some(ContentQueryNotice::Unreadable);
    }
    match state.content_state {
        ContentIndexState::Ready => None,
        ContentIndexState::Disabled => Some(ContentQueryNotice::Disabled),
        ContentIndexState::Backfilling { .. } => Some(ContentQueryNotice::Backfilling),
        ContentIndexState::Error(_) => Some(ContentQueryNotice::Error),
    }
}

/// Opacity of one skeleton row at a point in the shared pulse. Neighbouring rows are offset so the
/// brightness travels down the table instead of the whole block blinking at once.
fn pulse_opacity(row: usize, delta: f32) -> f32 {
    let phase = (delta + row as f32 * 0.06).rem_euclid(1.0);
    let triangle = 1.0 - (2.0 * phase - 1.0).abs();
    let smooth = triangle * triangle * (3.0 - 2.0 * triangle);
    0.45 + 0.55 * smooth
}

/// Viewport-local top of the first rendered row.
///
/// The window start is absolute and, for a full index, can name row one million. Multiplying it
/// by the row height before layout loses sub-row precision in f32: every row lands on a coarse
/// rounding grid and the gaps visibly alternate. Subtracting in f64 leaves only the small residual
/// between the window start and the scroll offset for the layout engine to see.
fn rows_band_local_top(first: usize, scroll_top: f32, row_height: f32) -> f32 {
    (first as f64 * row_height as f64 - scroll_top as f64) as f32
}

/// Rows to paint, the viewport-local top they start at, and whether they are pinned.
///
/// A scrollbar drag can move the viewport completely past the fetched window. Painting that band
/// as placeholders leaves nothing on screen while the page request is in flight, which reads as a
/// white flash at drag speed. Keep the closest edge of the window already in memory on screen
/// instead: its tail when the viewport moved below it, its head when it moved above. The page for
/// the new viewport replaces these rows as soon as it lands.
///
/// Such rows are pinned: they do not belong to the rows the scroll position names, so they are
/// painted for visual continuity only. The caller must not wire pointer interaction or a band
/// position onto them.
struct PaintedRows {
    range: Range<usize>,
    top: f32,
    pinned: bool,
}

/// Per-band values every real row shares: the result-set size for its ARIA position, the lane
/// tokens, the row height, and whether this paint shows the rows the scroll position names rather
/// than a pinned preview of another window edge.
struct RowBand<'a> {
    total: usize,
    tokens: &'a ShellTokens,
    row_height: Pixels,
    interactive: bool,
}

fn rows_to_paint(
    visible: Range<usize>,
    loaded: Range<usize>,
    scroll_top: f32,
    viewport_height: f32,
    row_height: f32,
) -> PaintedRows {
    let overlaps = visible.start < loaded.end && visible.end > loaded.start;
    if overlaps || loaded.is_empty() {
        let top = rows_band_local_top(visible.start, scroll_top, row_height);
        return PaintedRows {
            range: visible,
            top,
            pinned: false,
        };
    }

    let count = visible.len().max(1);
    if visible.start >= loaded.end {
        let start = loaded.end.saturating_sub(count).max(loaded.start);
        let top = viewport_height - (loaded.end - start) as f32 * row_height;
        PaintedRows {
            range: start..loaded.end,
            top,
            pinned: true,
        }
    } else {
        let end = loaded.start.saturating_add(count).min(loaded.end);
        PaintedRows {
            range: loaded.start..end,
            top: 0.0,
            pinned: true,
        }
    }
}

/// Strength of the bottom edge shade: full while there is more than one row left to scroll,
/// ramping to nothing at the end so the list never carries a permanent dim band.
fn rows_edge_opacity(remaining: f32, row_height: f32) -> f32 {
    if remaining <= 0.0 || row_height <= 0.0 {
        return 0.0;
    }
    (remaining / (row_height * ROWS_EDGE_SHADE_RAMP_ROWS)).clamp(0.0, 1.0)
}

/// Broad file kind behind a result row's icon, from the taxonomy the Treemap already uses.
///
/// A recovered suffix matters when the display name hides or differs from it. In that case the
/// suffix is carried through the taxonomy's name entry point with a synthetic name instead of a
/// second extension table. When the name already ends in the same suffix — or no suffix was
/// recovered — the taxonomy's own name rules stay in charge, so `CMakeLists.txt` stays code and a
/// bare dotfile stays configuration.
fn broad_file_category(name: &str, resolved_extension: &str) -> TreemapCategory {
    let name_extension = name.rsplit_once('.').map(|(_, extension)| extension);
    let name_names_the_suffix =
        name_extension.is_some_and(|extension| extension.eq_ignore_ascii_case(resolved_extension));
    if resolved_extension.is_empty() || name_names_the_suffix {
        return TreemapCategory::classify(name, false);
    }
    let category = TreemapCategory::classify(&format!("file.{resolved_extension}"), false);
    if category == TreemapCategory::Other {
        // An unknown recovered suffix cannot improve on what the name itself says.
        TreemapCategory::classify(name, false)
    } else {
        category
    }
}

/// One proportional text lane. `flex_basis` at zero makes the grow factors divide the surplus
/// exactly, so the header and every row resolve the same five-to-four split between name and path
/// and the numeric lanes keep their fixed channels (Design Guides: repeat column geometry through
/// the whole region).
pub(crate) trait ColumnLane {
    fn lane_grow(self, grow: f32) -> Self;
}

impl ColumnLane for Div {
    fn lane_grow(self, grow: f32) -> Self {
        self.flex_grow(grow).flex_basis(px(0.0))
    }
}

impl<E> ColumnLane for gpui_kit::gpui::Stateful<E>
where
    gpui_kit::gpui::Stateful<E>: Styled,
{
    fn lane_grow(self, grow: f32) -> Self {
        self.flex_grow(grow).flex_basis(px(0.0))
    }
}

/// The commands the result context menu lists, in the order it lists them. They are the same
/// [`GroupCommand`]s the command bar holds, so a row cannot offer a command the bar has lost.
/// 删除 opens its own section: it is destructive, where 打开/复制 are reads and 属性 is a look-up.
const RESULT_COMMANDS: &[GroupCommand] = &[
    GroupCommand::OpenSelected,
    GroupCommand::RevealLocation,
    GroupCommand::CopyPaths,
    GroupCommand::CopyNames,
    GroupCommand::DeleteSelected,
    GroupCommand::ShowProperties,
];

/// A bin row has no live path to reveal and no useful payload properties sheet. It offers the
/// bin's own restore command instead, then the copy commands, then the permanent delete.
const BIN_COMMANDS: &[GroupCommand] = &[
    GroupCommand::OpenSelected,
    GroupCommand::RestoreSelected,
    GroupCommand::CopyPaths,
    GroupCommand::CopyNames,
    GroupCommand::DeleteSelected,
];

/// The heading a column carries. The bin's path and date lanes hold the original location and
/// the deletion time, so they must not keep the index table's 位置/修改日期 captions.
fn column_label_key(column: SortColumn, in_recycle_bin: bool) -> &'static str {
    if in_recycle_bin {
        column.recycle_bin_label_key()
    } else {
        column.label_key()
    }
}

/// Every empty, no-match and unavailable band shares this geometry. The icon and
/// the optional action are supplied by the state; the rhythm, surface, type and
/// measure are not copied per state.
fn render_state_band(
    tokens: &ShellTokens,
    control: &'static crate::shell::accessibility::KeyControl,
    aria_label: String,
    icon: AnyElement,
    title: String,
    hint: String,
    action: Option<AnyElement>,
) -> ControlElement {
    let band = accessibility::element(control)
        .aria_label(aria_label)
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap(rems(layout::SPACE_8))
        .py(rems(layout::SPACE_48))
        .bg(tokens.background)
        .child(icon)
        .child(
            div()
                .text_sm()
                .font(theme::serif_font(FontWeight::SEMIBOLD))
                .text_color(tokens.foreground)
                .child(title),
        )
        .child(
            div()
                .max_w(rems(layout::MESSAGE_MAX_WIDTH))
                .text_center()
                .text_xs()
                .text_color(tokens.muted_foreground)
                .child(hint),
        );
    match action {
        Some(action) => band.child(action),
        None => band,
    }
}

/// A command's wording can change with the location. The bin's delete is permanent, so it must
/// not share the reversible table command's label; the project confirmation names the
/// irreversible result before the shell runs.
fn result_command_label_key(command: GroupCommand, in_recycle_bin: bool) -> &'static str {
    if in_recycle_bin {
        match command {
            GroupCommand::DeleteSelected => "ctx_delete_permanently",
            // The row's path is the original location, which no longer exists;
            // name the value the command actually copies.
            GroupCommand::CopyPaths => "ctx_copy_original_location",
            _ => command.label_key(),
        }
    } else {
        command.label_key()
    }
}

/// Which lane a heading cell occupies. The names are the row's own parts, so a heading and a cell
/// cannot be given different widths by accident.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderLane {
    Name,
    Path,
    Size,
    Date,
}

impl CairnApp {
    /// The heading row plus whatever the band under it holds: the rows, the empty state, or the
    /// reason there are no rows to show.
    ///
    /// The heading row and the rows share one horizontal extent inside a box that scrolls sideways.
    /// Each lane already refuses to shrink below its own floor, so once the window is narrower than
    /// the four floors plus the sidebar the last column is cut off at the frame edge; scrolling
    /// reaches it instead. The extent is [`layout::TABLE_MIN_WIDTH`] when the window is tight and
    /// stretches to the viewport when it is not, so a wide window shows no trailing gap. The two
    /// state bands sit outside that extent: they have no columns to line up with, and a sentence you
    /// have to scroll sideways to finish reading is not a recovery instruction.
    ///
    /// That overflow needs to be *said*, and one offset has to answer all three of the ways a person
    /// can act on it, so the region owns them together: the scrollbar drawn at the region's own edge,
    /// the four keys of [`RESULTS_CONTEXT`], and the wheel. All three read and write
    /// `crate::app::CairnApp::table_scroll` (SPEC UI-03; Design Guides *Designing data-heavy
    /// interfaces*: "keep headers and primary row identity visually stable").
    pub(crate) fn render_results(
        &mut self,
        tokens: &ShellTokens,
        visible: Range<usize>,
        row_height: Pixels,
        results_hold_focus: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = self.render_table_header(tokens, cx);
        // The rows are the content that overflows sideways, so they scroll. The two state bands are
        // not: a recovery sentence laid out at the columns' full extent runs past the frame and only
        // becomes readable by scrolling sideways, which is the opposite of what the band exists to
        // say. A band fills the width the window actually shows instead.
        // While the first page (or a rebuilt index) is loading, show the progressive skeleton.
        // Once any rows exist the table wins even if the index is still catching up: the cached
        // results are usable, and the single "synchronizing" capsule describes their freshness.
        // The skeleton is the first page only. A background refresh of an index that already
        // shows rows must leave those rows in place: replacing them with the skeleton makes a
        // slow refresh look like a search that never finishes.
        let bin_loading = self.recycle_bin.is_open() && self.recycle_bin.is_loading();
        let first_page_loading = (!self.recycle_bin.is_open()
            && self.results.table.total_rows() == 0
            && (self.query.is_searching
                || (self.service.is_probing_service && self.service.is_service_connected)))
            || (bin_loading && self.results.table.total_rows() == 0);
        let index_loading = !self.recycle_bin.is_open()
            && !self.has_query()
            && self.service.status.is_any_loading()
            && self.results.table.total_rows() == 0;
        // The bin is shell data, not index data: a stopped service must not replace it
        // with the offline band, and an empty bin is its own state, not an offline one.
        let local_location = self.recycle_bin.is_open() || self.service.is_service_connected;
        let rows = if self.recycle_bin.error().is_some() {
            // An unreadable bin is authoritative over any rows still in the table. The state band
            // below owns the retry path; stale rows must never become actionable again.
            None
        } else if first_page_loading || index_loading {
            // A known-long state (rebuild, local scan) shows the skeleton at once; an ordinary
            // first page waits out the measured fast path before adding the skeleton.
            let delay = if index_loading || bin_loading {
                Duration::ZERO
            } else {
                SKELETON_DELAY
            };
            // A long wait renders a still skeleton: a pulse that repaints the window for minutes
            // costs a core without telling the reader anything the skeleton does not already say.
            // The ordinary first page keeps the pulse for its brief wait.
            let still = cx.reduce_motion() || delay.is_zero();
            Some(
                self.render_skeleton_band(tokens, row_height, delay, still)
                    .into_any_element(),
            )
        } else if local_location && self.results.table.total_rows() > 0 {
            Some(
                self.render_rows_band(tokens, visible, row_height, cx)
                    .into_any_element(),
            )
        } else {
            None
        };
        let has_content = rows.is_some();
        // The edge shade belongs to the result body as a whole, not just the row band: it spans
        // the row viewport and the vertical scrollbar gutter so the bottom edge is continuous
        // instead of leaving a bright strip beside the rows. The skeleton and state bands own their
        // own surfaces and must not take it.
        let showing_result_rows = self.recycle_bin.error().is_none()
            && !(first_page_loading || index_loading)
            && local_location
            && self.results.table.total_rows() > 0;
        let row_height_f = f32::from(row_height);
        let scroll_below = (self.results.table.max_scroll_top(row_height_f)
            - self.results.table.scroll_top)
            .max(0.0);
        let shade = if showing_result_rows {
            rows_edge_opacity(scroll_below, row_height_f)
        } else {
            0.0
        };
        let state = if has_content {
            None
        } else if local_location {
            Some(self.render_empty_state(tokens, cx).into_any_element())
        } else {
            Some(
                self.render_service_offline_state(tokens, cx)
                    .into_any_element(),
            )
        };

        // Whether the columns need the sideways route at all, read from the handle that measured it
        // last frame. The component skips an axis whose content fits
        // (`gpui-base-0.7.3/src/scrollbar.rs`: "Hide scrollbar, if the scroll area is smaller than
        // the container"), so this only decides whether to spend the row on it.
        let overflows_sideways = self.results.table_scroll.max_offset().x > px(0.0);

        let viewport = div()
            // `overflow_x_scroll` and its offset live on the interactive element, so the scrolling box
            // needs its own identity. It carries no focus and answers no keys: the region around it
            // does, because the scrollbar that belongs to this box must not travel with its content.
            .id("result-table-viewport")
            // The viewport always fills the width it is given. Without this, a state band (empty,
            // offline, skeleton-free) left the box sized by the header's intrinsic width, so the
            // columns jumped left and right as a query resolved to zero rows. Height still comes
            // from the row: with rows this box shares the leftover body; without them it ends at
            // the heading and the band below takes the space.
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_x_scroll()
            .track_scroll(&self.results.table_scroll)
            // A vertical wheel over this box must stay vertical. Without the lock GPUI feeds a
            // one-axis container the *other* axis' delta, so scrolling down through the rows slid the
            // columns sideways instead (`gpui-pre-0.3.9/src/elements/div.rs`, scroll listener's
            // remap arm; the component layer locks the same axis for the same reason).
            .lock_scroll_axis()
            .child(
                v_flex()
                    .w_full()
                    .min_w(rems(layout::TABLE_MIN_WIDTH))
                    .when(has_content, |this| this.flex_1())
                    .min_h_0()
                    // The heading row owns the separator under it; the scrolling band must not draw
                    // the same edge a second time.
                    .child(header)
                    .when_some(rows, |this, rows| this.child(rows)),
            );

        let control = accessibility::control("result-table");
        announce_shortcut(accessibility::element(control), control)
            .aria_label(accessibility::name(control, &self.i18n))
            .key_context(RESULTS_CONTEXT)
            .track_focus(&self.results.results_focus)
            // Any key in the region means the keyboard is driving, so the focus ring belongs on
            // the table. A pointer press clears the flag before the next frame.
            .on_key_down(cx.listener(|this, _: &KeyDownEvent, _, cx| {
                if !this.results.keyboard_navigation {
                    this.results.keyboard_navigation = true;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(Self::scroll_table_left))
            .on_action(cx.listener(Self::scroll_table_right))
            .on_action(cx.listener(Self::scroll_table_start))
            .on_action(cx.listener(Self::scroll_table_end))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(
                h_flex()
                    // With rows the band takes the leftover height; with a state band the heading row
                    // stays at its own height and the state under it takes the space.
                    .when(has_content, |this| this.flex_1())
                    .min_w_0()
                    .min_h_0()
                    .items_stretch()
                    .relative()
                    .child(viewport)
                    // The gutter is permanent, not conditional on the first page.
                    // Reserving its width while the skeleton is up keeps the
                    // header and every column from jumping when rows land.
                    .child(
                        v_flex()
                            .flex_shrink_0()
                            .h_full()
                            .w(Scrollbar::width())
                            .bg(tokens.background)
                            // The heading owns the top of the region, so its gutter keeps the
                            // header surface and the scrollbar track starts underneath it.
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .h(rems(layout::HEADER_HEIGHT))
                                    .bg(tokens.table_head)
                                    .border_b_1()
                                    .border_color(tokens.border),
                            )
                            .child(div().flex_1().min_h_0().when(
                                self.results.table.total_rows() > 0,
                                |this| {
                                    this.child(
                                        Scrollbar::vertical(&self.results.rows_scroll)
                                            .id("result-table-vscroll")
                                            .mode(ScrollbarMode::Always)
                                            .viewport_from_layout(),
                                    )
                                },
                            )),
                    )
                    // The same shade spans the row viewport and the vertical scrollbar gutter,
                    // so the table's bottom edge stays continuous instead of leaving a bright
                    // strip beside the rows. It is paint-only; the scrollbar and rows keep their
                    // hitboxes.
                    .when(shade > 0.0, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(rems(ROWS_EDGE_SHADE_HEIGHT))
                                .opacity(shade)
                                // 180° puts the transparent stop at the top and the shade at the
                                // edge; 0° in this renderer runs the other way.
                                .bg(linear_gradient(
                                    180.0,
                                    linear_color_stop(tokens.muted_foreground.opacity(0.0), 0.0),
                                    linear_color_stop(
                                        tokens.muted_foreground.opacity(ROWS_EDGE_SHADE_ALPHA),
                                        1.0,
                                    ),
                                )),
                        )
                    }),
            )
            .when(overflows_sideways, |this| {
                this.child(
                    div().flex_shrink_0().w_full().h(Scrollbar::width()).child(
                        Scrollbar::horizontal(&self.results.table_scroll)
                            .id("result-table-hscroll")
                            .mode(ScrollbarMode::Always)
                            .viewport_from_layout(),
                    ),
                )
            })
            .when_some(state, |this, band| this.child(band))
            // Keyboard focus has to be visible, and the ring must not move the columns it surrounds,
            // so it is painted over the region's edge instead of becoming a border on the region. It
            // carries no id and no listener, so drawing it last costs the content nothing it needs to
            // be hit by.
            .when(results_hold_focus, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(tokens.ring),
                )
            })
    }

    fn render_skeleton_band(
        &self,
        tokens: &ShellTokens,
        row_height: Pixels,
        delay: Duration,
        still: bool,
    ) -> impl IntoElement {
        let vh = self.results.table.viewport_height;
        let count = if f32::from(row_height) > 0.0 && vh > 0.0 {
            ((vh / f32::from(row_height)).ceil() as usize + 2).max(8)
        } else {
            14
        };
        let decay_step = 0.65 / (count as f32).max(1.0);
        let skeleton_rows = (0..count).map(move |ix| {
            self.render_skeleton_row(ix, tokens, row_height, decay_step, delay, still)
        });

        v_flex()
            .flex_1()
            .min_h_0()
            .bg(tokens.background)
            .children(skeleton_rows)
    }

    fn render_skeleton_row(
        &self,
        ix: usize,
        tokens: &ShellTokens,
        row_height: Pixels,
        decay_step: f32,
        delay: Duration,
        still: bool,
    ) -> AnyElement {
        const SKELETON_NAME_WIDTHS: [f32; 8] = [8.0, 11.0, 6.5, 9.5, 12.0, 7.5, 10.0, 8.5];
        const SKELETON_PATH_WIDTHS: [f32; 8] = [14.0, 18.0, 12.0, 16.5, 20.0, 13.5, 17.0, 15.0];
        const SKELETON_SIZE_WIDTHS: [f32; 4] = [3.2, 2.5, 4.0, 2.8];

        let name_w = rems(SKELETON_NAME_WIDTHS[ix % SKELETON_NAME_WIDTHS.len()]);
        let path_w = rems(SKELETON_PATH_WIDTHS[ix % SKELETON_PATH_WIDTHS.len()]);
        let size_w = rems(SKELETON_SIZE_WIDTHS[ix % SKELETON_SIZE_WIDTHS.len()]);
        let opacity = (1.0 - (ix as f32 * decay_step)).max(0.35);
        let skel_bg = tokens.skeleton.opacity(opacity);

        let row = h_flex()
            .h(row_height)
            .items_center()
            .w_full()
            .child(
                div()
                    .lane_grow(self.results.column_widths.name)
                    .min_w(rems(layout::NAME_MIN_COLUMN))
                    .pl(rems(layout::CONTENT_INSET))
                    .pr(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .gap(rems(layout::INNER_SPACING))
                    .child(
                        div()
                            .size_4()
                            .flex_shrink_0()
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    )
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(name_w)
                            .max_w_full()
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .child(
                div()
                    .lane_grow(self.results.column_widths.path)
                    .min_w(rems(layout::PATH_MIN_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(path_w)
                            .max_w_full()
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .child(
                div()
                    .lane_grow(self.results.column_widths.size)
                    .min_w(rems(layout::SIZE_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(size_w)
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .child(
                div()
                    .lane_grow(self.results.column_widths.date)
                    .min_w(rems(layout::DATE_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(rems(SKELETON_DATE_WIDTH))
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            );

        // A still skeleton renders as a single static image. Building the animation and then
        // painting a constant opacity would keep requesting frames for a value that can never
        // change, which costs the same as the moving skeleton without showing anything.
        if still {
            return row.opacity(1.0).into_any_element();
        }

        let pulse = Animation::new(SKELETON_PULSE)
            .repeat_synced()
            .with_max_fps(SKELETON_MAX_FPS);
        // The list length must not change between frames: GPUI persists the active animation index
        // in the element state, and a shorter list makes that index out of bounds. The ordinary
        // first page has a non-zero wait phase; a known-long state never reaches this path.
        let animations = vec![Animation::new(delay), pulse];

        row.with_animations(
            element_id("skeleton-row", &ix.to_string()),
            animations,
            move |row, phase, delta| {
                if phase == 0 {
                    // The delay phase: the skeleton exists in the layout but has not become
                    // visible. A fast answer unmounts it before this timer ever elapses.
                    row.opacity(0.0)
                } else {
                    row.opacity(pulse_opacity(ix, delta))
                }
            },
        )
        .into_any_element()
    }

    /// The empty and no-match states. With an empty box the "所有文件" query is still a query, so a
    /// zero-row answer means the index itself has nothing to show, not "start typing". A narrowed
    /// query keeps the ordinary no-match message. Clearing is owned by the search box's own clear
    /// control, so the band never repeats that command as a second button.
    fn render_empty_state(&self, tokens: &ShellTokens, cx: &mut Context<Self>) -> ControlElement {
        if self.recycle_bin.error().is_some() {
            let control = accessibility::control("recycle-bin-error");
            let title = self.i18n.get_text("recycle_bin_unavailable_title");
            let hint = self.i18n.get_text("recycle_bin_unavailable_hint");
            let retry = accessibility::control("recycle-bin-retry");
            let retry_label = accessibility::name(retry, &self.i18n);
            let action = Button::new(retry.element)
                .label(retry_label.clone())
                .accessibility_label(retry_label)
                .mt(rems(layout::SPACE_8))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_recycle_bin(window, cx);
                }))
                .into_any_element();
            return render_state_band(
                tokens,
                control,
                format!("{title}. {hint}"),
                Icon::new(IconName::CircleAlert)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
                title,
                hint,
                Some(action),
            );
        }

        let control = accessibility::control("result-state");
        if self.recycle_bin.is_open() {
            let bin_empty = self.recycle_bin.all().is_empty();
            let (title_index, hint_index) = if bin_empty { (2, 3) } else { (4, 5) };
            let title = accessibility::name_at(control, title_index, &self.i18n);
            let hint = accessibility::name_at(control, hint_index, &self.i18n);
            let icon = if bin_empty {
                Icon::new(AssetIconName::Recycle)
            } else {
                Icon::new(IconName::Search)
            }
            .size_8()
            .text_color(tokens.muted_foreground)
            .into_any_element();
            return render_state_band(
                tokens,
                control,
                format!("{title}. {hint}"),
                icon,
                title,
                hint,
                None,
            );
        }

        let has_query = self.has_query();
        let notice = if self.has_content_query() {
            content_query_notice(&self.index_menu_state())
        } else {
            None
        };
        let notice_keys = notice.map(ContentQueryNotice::keys);
        let (title_key, hint_key) = match notice_keys {
            Some(keys) => keys,
            // A ready content index answering zero rows is a genuine no-match, but it is a
            // no-match inside a scope narrower than "all files". Say that scope once instead of
            // letting a filename-only explanation imply whole-body coverage.
            None if has_query && self.has_content_query() => {
                ("empty_no_matches", "empty_content_scope_hint")
            }
            None if has_query => ("empty_no_matches", "empty_no_matches_hint"),
            None => ("empty_index_empty_title", "empty_index_empty_hint"),
        };
        let title = self.i18n.get_text(title_key);
        let hint = match &self.service.content_status.state {
            ContentIndexState::Error(reason) if notice == Some(ContentQueryNotice::Error) => {
                let mut args = FluentArgs::new();
                args.set("reason", reason.trim());
                self.i18n.get_text_with_args(hint_key, &args)
            }
            _ => self.i18n.get_text(hint_key),
        };
        let message = match notice_keys {
            Some(_) => format!("{title}. {hint}"),
            // The scope sentence is part of the answer, so it belongs in the announced message as
            // well as in the visible hint.
            None if has_query && self.has_content_query() => format!("{title}. {hint}"),
            None if has_query => accessibility::name_at(control, 1, &self.i18n),
            None => accessibility::name_at(control, 0, &self.i18n),
        };
        render_state_band(
            tokens,
            control,
            message,
            Icon::new(IconName::Search)
                .size_8()
                .text_color(tokens.muted_foreground)
                .into_any_element(),
            title,
            hint,
            None,
        )
    }

    fn render_service_offline_state(
        &self,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let issue = self
            .service
            .service_issue
            .unwrap_or(ServiceIssue::NotRunning);
        let control = accessibility::control(issue.accessibility_key());
        let title = accessibility::name_at(control, 0, &self.i18n);
        let recovery = accessibility::name_at(control, 1, &self.i18n);
        let retry = accessibility::control("service-retry-button");
        let retry_label = accessibility::name(retry, &self.i18n);
        let start = accessibility::control("service-start-button");
        let start_label = accessibility::name(start, &self.i18n);
        // Starting the service is a UAC action, so it is offered only when SCM says the service
        // exists. A missing service gets install guidance instead of an elevation prompt.
        let can_start = self.service.service_installed
            && issue == ServiceIssue::NotRunning
            && !self.service.search_scope.needs_local_only();
        // The small line states the outcome or the issue-specific recovery; the button carries
        // the action. Start and retry states keep the same title + hint + action rhythm.
        let hint = if can_start {
            self.i18n.get_text("service_start_hint")
        } else {
            recovery.clone()
        };
        let announcement = format!("{title}. {hint}");

        accessibility::element(control)
            .aria_label(announcement)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(rems(layout::SPACE_12))
            .py(rems(layout::SPACE_48))
            .bg(tokens.background)
            .child(
                Icon::new(IconName::CircleAlert)
                    .size_8()
                    .text_color(tokens.muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(tokens.foreground)
                    .child(title),
            )
            .child(
                div()
                    .max_w(rems(layout::MESSAGE_MAX_WIDTH))
                    .text_center()
                    .text_xs()
                    .text_color(tokens.muted_foreground)
                    .child(hint),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(rems(layout::INNER_SPACING))
                    .mt(rems(layout::SPACE_8))
                    .when(can_start, |this| {
                        this.child(
                            Button::new(start.element)
                                .label(start_label.clone())
                                .accessibility_label(start_label)
                                .small()
                                .loading(self.service.is_probing_service)
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.start_service(cx);
                                })),
                        )
                    })
                    .when(!can_start, |this| {
                        this.child(
                            Button::new(retry.element)
                                .label(retry_label.clone())
                                .accessibility_label(retry_label)
                                .small()
                                .loading(self.service.is_probing_service)
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.retry_service_connection(cx);
                                })),
                        )
                    }),
            )
    }

    /// Scroll owner of the result band. Only `visible` rows are built. The tall spacer underneath
    /// gives the scrollbar the full result extent, while the rows are painted in a fixed overlay
    /// whose top is the viewport-local residual: at a full index the absolute row origin is tens of
    /// millions of pixels, where f32 rounds every row onto a coarser grid and the row rhythm drifts.
    ///
    /// This band stays drawn here instead of using `Table` or `VirtualList` for one reason: the
    /// service reports a total before it has delivered the rows. Both components measure a collection
    /// they already hold, so an index beyond the fetched window cannot exist in them — the placeholder
    /// rows would vanish and the scrollbar would shrink to one page the moment a query returned more.
    /// The row geometry, the cell lanes, the z-free hover and selection fills and every colour below
    /// come from the same tokens and scale helpers the components use.
    fn render_rows_band(
        &mut self,
        tokens: &ShellTokens,
        visible: Range<usize>,
        row_height: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let extent = px(self.results.table.scroll_extent(f32::from(row_height)));
        // The fetched window can start millions of rows into the result set. Laying those rows
        // out inside the scrolled content would place them tens of millions of pixels from the
        // origin, where f32 rounds every row to a coarser grid and the row rhythm visibly drifts
        // (26/28/32 px gaps on a 28 px row). Only the scroll range stays absolute; the rows are
        // drawn over the viewport and their top is the small residual between the window start
        // and the scroll offset.
        let painted = rows_to_paint(
            visible.clone(),
            self.results.table.loaded_range(),
            self.results.table.scroll_top,
            self.results.table.viewport_height,
            f32::from(row_height),
        );
        let rows = self.collect_visible_rows(
            painted.range.clone(),
            tokens,
            row_height,
            !painted.pinned,
            cx,
        );
        let band_top = painted.top;
        let pinned = painted.pinned;
        let first = visible.start;
        let reduce_motion = cx.reduce_motion();

        let control = accessibility::control("result-band");
        let band = accessibility::element(control)
            .aria_label(accessibility::name(control, &self.i18n))
            .aria_orientation(Orientation::Vertical)
            // A pinned band paints rows from the loaded window edge, not the rows this scroll
            // position names; its first visible index would be a false position report. Each
            // painted row still carries its own real absolute index.
            .when(!pinned, |band| band.aria_numeric_value(first as f64))
            .flex_1()
            .min_h_0()
            .relative()
            .bg(tokens.background)
            // The tall spacer is the only scroll content; it gives the scrollbar the full result
            // extent while the rows are painted at viewport-local coordinates above it.
            .child(
                div()
                    .id("result-rows-scroll")
                    .absolute()
                    .inset_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.results.rows_scroll)
                    // The rows overlay owns wheel input; this stays locked so a sideways sweep
                    // that reaches the spacer is never remapped into a vertical scroll.
                    .lock_scroll_axis()
                    .child(div().absolute().left_0().right_0().top_0().h(extent)),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        if delta.y.is_zero() {
                            // A purely sideways gesture belongs to the viewport below.
                            return;
                        }
                        // GPUI adds the wheel delta to the negative scroll offset; scrolling
                        // down reports a negative delta and moves the band towards later rows.
                        let current = this.scrolled_pixels();
                        let max = this
                            .results
                            .table
                            .max_scroll_top(f32::from(layout::row_height(window)));
                        let next = (current - f32::from(delta.y)).clamp(0.0, max);
                        if next != current {
                            this.results.table.scroll_top = next;
                            this.results
                                .rows_scroll
                                .set_offset(point(px(0.0), -px(next)));
                            this.ensure_visible_rows_loaded(layout::row_height(window), cx);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    // Empty space below the last row is still the result band. A press there is
                    // an outside click for the selection; a press on a row stops propagation above.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.dismiss_result_selection(cx)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(band_top))
                            .flex()
                            .flex_col()
                            .children(rows),
                    ),
            );

        // The first page fades in where the skeleton stood. The id is stable while the band stays
        // mounted, so paging and background refreshes do not replay the entrance.
        band.with_animation(
            "result-rows-reveal",
            Animation::new(ROW_REVEAL).with_easing(ease_out_quint()),
            move |band, delta| {
                if reduce_motion {
                    band
                } else {
                    band.opacity(delta)
                }
            },
        )
    }

    fn collect_visible_rows(
        &self,
        visible: Range<usize>,
        tokens: &ShellTokens,
        row_height: Pixels,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let band = RowBand {
            total: self.results.table.total_rows(),
            tokens,
            row_height,
            interactive,
        };
        let is_fetching = self.query.is_fetching_more;
        visible
            .map(|ix| match self.results.table.row(ix) {
                Some(row) => self
                    .render_result_row(ix, row, &band, cx)
                    .into_any_element(),
                None => self.render_placeholder_row(ix, is_fetching, tokens, row_height),
            })
            .collect()
    }

    fn render_result_row(
        &self,
        ix: usize,
        row: &FileRowData,
        band: &RowBand<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let total = band.total;
        let tokens = band.tokens;
        let row_height = band.row_height;
        let is_selected = self.results.table.selected_indices.contains(&ix);
        let is_focused = self.results.table.focused_idx == Some(ix);
        let full_path = join_row_path(&row.path, &row.name);
        let i18n = Rc::clone(&self.i18n);

        let mut row_el = accessibility::element_at(accessibility::control("result-row"), ix)
            .aria_label(full_path.clone())
            // The badge is visible; the snippet behind it is the part assistive technology cannot
            // infer from the file name. Describe the row with it instead of hiding the reason.
            .when_some(
                row.snippet
                    .clone()
                    .filter(|snippet| !snippet.trim().is_empty()),
                |this, snippet| this.aria_description(snippet),
            )
            .aria_selected(is_selected)
            .aria_row_index(ix + 1)
            .aria_position_in_set(ix + 1)
            .aria_size_of_set(total)
            .flex()
            .items_center()
            .h(row_height)
            .bg(tokens.row_background(is_selected))
            .text_color(if is_selected {
                tokens.foreground
            } else {
                tokens.muted_foreground
            })
            .text_sm()
            .child(self.render_name_cell(row, &full_path, tokens, is_selected))
            .child(self.render_path_cell(row, &full_path, tokens, is_selected))
            .child(self.render_size_cell(row, tokens, is_selected))
            .child(self.render_date_cell(row, tokens, is_selected));

        // A pinned row is a preview of the loaded window edge, not the row at this scroll
        // position. Pointer and context-menu handlers would act on an identity the viewport is
        // not showing, so the preview stays readable but inert.
        if band.interactive {
            row_el = row_el
                .hover(|s| s.bg(tokens.row_hover))
                .active(|s| s.bg(tokens.row_hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.select_row_by_pointer(
                            ix,
                            event.modifiers.control,
                            event.modifiers.shift,
                            event.click_count == 2,
                            layout::row_height(window),
                            cx,
                        );
                        // The rows band clears the selection on empty-space presses; a press on a
                        // row selects it and must not bubble back out as an outside click.
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.focus_row_for_context(ix);
                        cx.notify();
                    }),
                );
        }

        if is_focused {
            row_el = row_el.aria_active_descendant();
        }
        if !band.interactive {
            return row_el.into_any_element();
        }
        let in_bin = self.recycle_bin.is_open();
        let commands = if in_bin {
            BIN_COMMANDS
        } else {
            RESULT_COMMANDS
        };
        // The commands the row owns, in the order the context menu lists them. Every one of them is
        // also on the keyboard or in the command bar, so the menu is a shortcut and not the only door.
        row_el
            .context_menu(move |menu, window, _cx| {
                // `PopupMenu::min_w` takes `Pixels`, not a `Length`, so the rem step is resolved against
                // this window's base font — the same one every other region in the shell scales with.
                let mut menu = menu.min_w(window.rem_size() * CONTEXT_MENU_MIN_WIDTH);
                for (ix, command) in commands.iter().enumerate() {
                    // Destructive delete and the properties look-up each open a section;
                    // the bin omits properties, so only delete separates there.
                    if ix > 0
                        && matches!(
                            command,
                            GroupCommand::DeleteSelected | GroupCommand::ShowProperties
                        )
                    {
                        menu = menu.separator();
                    }
                    menu = menu.menu(
                        i18n.get_text(result_command_label_key(*command, in_bin)),
                        command
                            .action()
                            .expect("a result command is an Action the root already handles"),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// File semantics are resolved from the name's final extension. The broad
    /// document/image/archive groups reuse [`TreemapCategory`] — including an extension recovered
    /// from a display name that hides it — while types that need a distinct silhouette in a result
    /// row (shortcuts, spreadsheets, binaries, scripts, fonts) get a dedicated glyph before that
    /// fallback.
    fn result_file_icon(name: &str, extension: &str, is_dir: bool) -> AssetIconName {
        if is_dir {
            return AssetIconName::Folder;
        }
        // The argument is the suffix recovered from the Shell payload; the local copy also falls
        // back to the name's own last segment for the specific glyph groups below.
        let resolved_extension = extension;
        let extension = if extension.is_empty() {
            name.rsplit_once('.')
                .map(|(_, extension)| extension)
                .unwrap_or("")
        } else {
            extension
        };
        let matches = |known: &[&str]| {
            known
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        };
        if matches(&["lnk", "url", "appref-ms", "website", "webloc"]) {
            return AssetIconName::FileSymlink;
        }
        if matches(&["xls", "xlsx", "csv", "tsv"]) {
            return AssetIconName::FileSpreadsheet;
        }
        if matches(&["ttf", "otf", "woff", "woff2", "fon"]) {
            return AssetIconName::FileType;
        }
        if matches(&["exe", "msi", "msix", "com"]) {
            return AssetIconName::AppWindow;
        }
        if matches(&["bat", "cmd", "ps1", "sh"]) {
            return AssetIconName::FileTerminal;
        }
        if matches(&["json", "toml", "yaml", "yml", "xml"]) {
            return AssetIconName::FileBraces;
        }
        if matches(&["dll", "sys", "drv", "efi"]) {
            return AssetIconName::FileCog;
        }
        match broad_file_category(name, resolved_extension) {
            TreemapCategory::Document => AssetIconName::FileText,
            TreemapCategory::Archive => AssetIconName::FileArchive,
            TreemapCategory::Image => AssetIconName::FileImage,
            TreemapCategory::Code => AssetIconName::FileCode,
            TreemapCategory::Audio => AssetIconName::FileMusic,
            TreemapCategory::Video => AssetIconName::FileVideoCamera,
            TreemapCategory::System => AssetIconName::FileCog,
            TreemapCategory::Applications => AssetIconName::AppWindow,
            TreemapCategory::Folder | TreemapCategory::Other => AssetIconName::File,
        }
    }

    /// A name cell is one truncated text run, not a row of independent spans: only a single run can
    /// put the ellipsis where the lane actually ends. The full name is available on hover, so a
    /// clipped value is never the only copy. The element id is the row's full path, which stays the
    /// same while the row is visible and is unique even when names repeat across folders.
    fn render_name_cell(
        &self,
        row: &FileRowData,
        full_path: &str,
        tokens: &ShellTokens,
        is_selected: bool,
    ) -> Stateful<Div> {
        let tooltip = SharedString::from(row.name.as_str());
        let extension = if self.recycle_bin.is_open() {
            self.recycle_bin
                .icon_extension(crate::shell::recycle_bin::bin_icon_key(
                    &row.name,
                    &row.path,
                    row.modified_ts,
                    row.size,
                    row.is_dir,
                ))
                .unwrap_or("")
        } else {
            ""
        };
        let content_match = row
            .snippet
            .clone()
            .filter(|snippet| !snippet.trim().is_empty());

        div()
            .id(element_id("result-name", full_path))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .lane_grow(self.results.column_widths.name)
            .min_w(rems(layout::NAME_MIN_COLUMN))
            .pl(rems(layout::CONTENT_INSET))
            .pr(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .gap(rems(layout::INNER_SPACING))
            .child(
                Icon::new(Self::result_file_icon(&row.name, extension, row.is_dir))
                    .small()
                    // The glyph already distinguishes the file kind. Selection is the only thing
                    // that should raise the icon, together with the row's text.
                    .text_color(if is_selected {
                        tokens.foreground
                    } else {
                        tokens.muted_foreground
                    }),
            )
            .child(result_text(&row.name, &row.name_highlights, tokens))
            // A content hit and a filename hit otherwise look identical. The badge names the
            // reason the row matched; the snippet is available on hover without adding a second
            // line or changing the virtual row height.
            .when_some(content_match, |this, snippet| {
                let tip = SharedString::from(snippet);
                this.child(
                    div()
                        .id(element_id("result-content-match", full_path))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .flex_shrink_0()
                        .px(rems(layout::SPACE_4))
                        .rounded(tokens.radius)
                        .bg(tokens.muted)
                        .text_xs()
                        .text_color(tokens.muted_foreground)
                        .child(self.i18n.get_text("result_content_match")),
                )
            })
    }

    /// The path lane behaves like the name lane: a single ellipsized run plus the complete value on
    /// hover. Its id shares the row's full path, so moving the pointer between the two cells keeps
    /// the hovered row's identity stable.
    fn render_path_cell(
        &self,
        row: &FileRowData,
        full_path: &str,
        tokens: &ShellTokens,
        is_selected: bool,
    ) -> Stateful<Div> {
        let tooltip = SharedString::from(row.path.as_str());

        div()
            .id(element_id("result-path", full_path))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .lane_grow(self.results.column_widths.path)
            .min_w(rems(layout::PATH_MIN_COLUMN))
            .px(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .overflow_hidden()
            .text_color(if is_selected {
                tokens.foreground
            } else {
                tokens.muted_foreground
            })
            .child(result_text(&row.path, &row.path_highlights, tokens))
    }

    /// Sizes share the heading's leading edge; tabular figures let the digits compare down the
    /// column even though the column is left-aligned (SPEC UI-02).
    fn render_size_cell(&self, row: &FileRowData, tokens: &ShellTokens, is_selected: bool) -> Div {
        div()
            .lane_grow(self.results.column_widths.size)
            .min_w(rems(layout::SIZE_COLUMN))
            .px(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .font_features(theme::tabular_figures())
            .text_color(if is_selected {
                tokens.foreground
            } else {
                tokens.muted_foreground
            })
            .child(format_size_bytes(row.size))
    }

    fn render_date_cell(&self, row: &FileRowData, tokens: &ShellTokens, is_selected: bool) -> Div {
        div()
            .lane_grow(self.results.column_widths.date)
            .min_w(rems(layout::DATE_COLUMN))
            .px(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .font_features(theme::tabular_figures())
            .text_color(if is_selected {
                tokens.foreground
            } else {
                tokens.muted_foreground
            })
            .child(format_timestamp(row.modified_ts))
    }

    /// A row the service has not delivered yet: an explicit loading state, so a scrollbar ahead of
    /// the fetched window never reads as an empty result. The words say which of the two states this
    /// is; the bar beside them is the shape of the row that will land there.
    ///
    /// The bars stay invisible for the same measured fast path the first page uses. A scrollbar
    /// drag can cross several unfetched windows in a few frames, and painting a skeleton for a page
    /// that lands before the next frame turns scrolling into a flicker of gray bars.
    fn render_placeholder_row(
        &self,
        ix: usize,
        is_fetching: bool,
        tokens: &ShellTokens,
        row_height: Pixels,
    ) -> AnyElement {
        // The registry lists the two states in order: fetching first, then not loaded yet.
        let control = accessibility::control("result-placeholder");
        let message = if is_fetching {
            accessibility::name_at(control, 0, &self.i18n)
        } else {
            accessibility::name_at(control, 1, &self.i18n)
        };

        const SKELETON_NAME_WIDTHS: [f32; 8] = [8.0, 11.0, 6.5, 9.5, 12.0, 7.5, 10.0, 8.5];
        const SKELETON_PATH_WIDTHS: [f32; 8] = [14.0, 18.0, 12.0, 16.5, 20.0, 13.5, 17.0, 15.0];
        const SKELETON_SIZE_WIDTHS: [f32; 4] = [3.2, 2.5, 4.0, 2.8];

        let name_w = rems(SKELETON_NAME_WIDTHS[ix % SKELETON_NAME_WIDTHS.len()]);
        let path_w = rems(SKELETON_PATH_WIDTHS[ix % SKELETON_PATH_WIDTHS.len()]);
        let size_w = rems(SKELETON_SIZE_WIDTHS[ix % SKELETON_SIZE_WIDTHS.len()]);
        let skel_bg = tokens.skeleton;

        accessibility::element_at(control, ix)
            .aria_label(message)
            .h(row_height)
            .flex()
            .items_center()
            .w_full()
            .child(
                div()
                    .lane_grow(self.results.column_widths.name)
                    .min_w(rems(layout::NAME_MIN_COLUMN))
                    .pl(rems(layout::CONTENT_INSET))
                    .pr(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .gap(rems(layout::INNER_SPACING))
                    .child(
                        div()
                            .size_4()
                            .flex_shrink_0()
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    )
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(name_w)
                            .max_w_full()
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .child(
                div()
                    .lane_grow(self.results.column_widths.path)
                    .min_w(rems(layout::PATH_MIN_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(path_w)
                            .max_w_full()
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .child(
                div()
                    .lane_grow(self.results.column_widths.size)
                    .min_w(rems(layout::SIZE_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(size_w)
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .child(
                div()
                    .lane_grow(self.results.column_widths.date)
                    .min_w(rems(layout::DATE_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(SKELETON_BAR_HEIGHT))
                            .w(rems(SKELETON_DATE_WIDTH))
                            .rounded(tokens.radius)
                            .bg(skel_bg),
                    ),
            )
            .with_animation(
                element_id("result-placeholder", &ix.to_string()),
                Animation::new(SKELETON_DELAY),
                |row, delta| {
                    if delta < 1.0 {
                        row.opacity(0.0)
                    } else {
                        row
                    }
                },
            )
            .into_any_element()
    }

    /// The heading row shares the result row's lanes exactly: same widths, same insets, same
    /// centerline, resolved from the same [`layout`] constants. It is drawn here rather than by
    /// `Table` for the reason recorded on [`CairnApp::render_rows_band`], and it stays clickable
    /// because sorting is a command, not a caption.
    fn render_table_header(&self, tokens: &ShellTokens, cx: &mut Context<Self>) -> ControlElement {
        let control = accessibility::control("table-header");
        accessibility::element(control)
            .aria_label(accessibility::name(control, &self.i18n))
            .h(rems(layout::HEADER_HEIGHT))
            .flex()
            .items_center()
            .flex_shrink_0()
            .bg(tokens.table_head)
            .text_color(tokens.table_head_foreground)
            .border_b_1()
            .border_color(tokens.border)
            .text_sm()
            // A release can land outside the narrow handle, so the row owns the save as well.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_column_resize(cx)),
            )
            .child(self.render_column_header(SortColumn::Name, HeaderLane::Name, tokens, cx))
            .child(self.render_column_header(SortColumn::Path, HeaderLane::Path, tokens, cx))
            .child(self.render_column_header(SortColumn::Size, HeaderLane::Size, tokens, cx))
            .child(self.render_column_header(
                SortColumn::DateModified,
                HeaderLane::Date,
                tokens,
                cx,
            ))
    }

    /// The band on the trailing edge of a heading cell that resizes that lane and its right
    /// neighbour. It is invisible until hovered, so the heading row keeps its single hairline.
    fn render_column_resize_handle(
        &self,
        boundary: ColumnBoundary,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = element_id("column-resize", boundary.key());
        let group = SharedString::from(id.clone());

        div()
            .id(id)
            .group(group.clone())
            .occlude()
            .absolute()
            .right_0()
            .top_0()
            .h_full()
            .w(RESIZE_HIT_WIDTH)
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.results.column_drag = Some(crate::app::ColumnDrag {
                        boundary,
                        widths: this.results.column_widths,
                        start_x: event.position.x,
                    });
                    this.results.keyboard_navigation = false;
                    cx.notify();
                }),
            )
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .absolute()
                    .right(RESIZE_LINE_OFFSET)
                    .top_0()
                    .h_full()
                    .w(theme::HAIRLINE)
                    .bg(tokens.border.opacity(0.0))
                    .group_hover(group, |style| style.bg(tokens.border)),
            )
            .into_any_element()
    }

    fn render_column_header(
        &self,
        column: SortColumn,
        lane: HeaderLane,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> ControlElement {
        let label_key = column_label_key(column, self.recycle_bin.is_open());
        let control = accessibility::control_at(&element_id("column-header", label_key));
        let label = accessibility::name(control, &self.i18n);
        let is_active = self.results.table.sort_column == column;

        let boundary = match lane {
            HeaderLane::Name => Some(ColumnBoundary::NamePath),
            HeaderLane::Path => Some(ColumnBoundary::PathSize),
            HeaderLane::Size => Some(ColumnBoundary::SizeDate),
            HeaderLane::Date => None,
        };

        let cell = accessibility::element(control)
            .aria_label(label.clone())
            .aria_selected(is_active)
            .relative()
            .h(rems(layout::HEADER_HEIGHT))
            .flex()
            .items_center()
            .gap(rems(layout::SPACE_4))
            .text_color(tokens.table_head_foreground)
            // The chevron alone marks the active sort. Bolding that one label made a single
            // header heavier than its peers and visually off the shared type line.
            .hover(|s| s.bg(tokens.row_hover))
            .active(|s| s.bg(tokens.row_hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                // Sorting the index reorders rows under the selection, so the stale selection is
                // dropped. The bin tracks selection by item identity instead, and sorting there
                // must keep that selection.
                if !this.recycle_bin.is_open() {
                    this.dismiss_result_selection(cx);
                }
                this.toggle_table_sort(column, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label),
            )
            .children(is_active.then(|| sort_indicator(self.results.table.sort_asc, tokens)));

        // Every lane is a grow ratio over the widths in `ColumnWidths`, with its old extent as the
        // floor. Heading labels share their values' leading edge; tabular figures keep the size
        // digits comparable.
        let cell = match lane {
            HeaderLane::Name => cell
                .lane_grow(self.results.column_widths.name)
                .min_w(rems(layout::NAME_MIN_COLUMN))
                .pl(rems(layout::CONTENT_INSET))
                .pr(rems(layout::INNER_SPACING)),
            HeaderLane::Path => cell
                .lane_grow(self.results.column_widths.path)
                .min_w(rems(layout::PATH_MIN_COLUMN))
                .px(rems(layout::INNER_SPACING)),
            HeaderLane::Size => cell
                .lane_grow(self.results.column_widths.size)
                .min_w(rems(layout::SIZE_COLUMN))
                .px(rems(layout::INNER_SPACING)),
            HeaderLane::Date => cell
                .lane_grow(self.results.column_widths.date)
                .min_w(rems(layout::DATE_COLUMN))
                .px(rems(layout::INNER_SPACING)),
        };

        match boundary {
            Some(boundary) => cell.child(self.render_column_resize_handle(boundary, tokens, cx)),
            None => cell,
        }
    }
}

/// Sort direction indicator: vector icon aligning with the Lucide icon system.
fn sort_indicator(ascending: bool, tokens: &ShellTokens) -> impl IntoElement {
    Icon::new(if ascending {
        IconName::ChevronUp
    } else {
        IconName::ChevronDown
    })
    .xsmall()
    .text_color(tokens.table_head_foreground)
}

/// One single-line value in the table: the text, its query highlights, and an ellipsis where the
/// lane ends. A highlighted value has to stay one text run so truncation applies to the whole name
/// or path rather than to each span separately; `with_highlights` supplies the per-range weight and
/// color for matched substrings.
fn result_text(text: &str, highlights: &[(usize, usize)], tokens: &ShellTokens) -> Div {
    let value = StyledText::new(SharedString::from(text));
    let value = if highlights.is_empty() {
        value
    } else {
        let mut offset = 0;
        let runs = split_name_highlights(text, highlights)
            .into_iter()
            .filter_map(|span| {
                let start = offset;
                offset += span.text.len();
                span.is_highlight.then_some(start..offset)
            })
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        color: Some(tokens.foreground),
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                )
            });
        value.with_highlights(runs)
    };

    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(value)
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{
        column_label_key, content_query_notice, pulse_opacity, result_command_label_key,
        rows_band_local_top, rows_edge_opacity, rows_to_paint, ContentQueryNotice,
    };
    use crate::app::CairnApp;
    use crate::shell::command_groups::GroupCommand;
    use crate::table::model::SortColumn;
    use cairn_protocol::status::ContentIndexState;
    use gpui_kit::assets::IconName as AssetIconName;

    #[test]
    fn result_rows_use_the_file_semantics_they_can_show() {
        assert_eq!(
            CairnApp::result_file_icon("Deploy.lnk", "", false),
            AssetIconName::FileSymlink
        );
        assert_eq!(
            CairnApp::result_file_icon("BOOKMARKS.URL", "", false),
            AssetIconName::FileSymlink
        );
        assert_eq!(
            CairnApp::result_file_icon("budget.xlsx", "", false),
            AssetIconName::FileSpreadsheet
        );
        assert_eq!(
            CairnApp::result_file_icon("photo.PNG", "", false),
            AssetIconName::FileImage
        );
        assert_eq!(
            CairnApp::result_file_icon("Cairn.exe", "", false),
            AssetIconName::AppWindow
        );
        assert_eq!(
            CairnApp::result_file_icon("build.ps1", "", false),
            AssetIconName::FileTerminal
        );
        assert_eq!(
            CairnApp::result_file_icon("notes.unknown", "", false),
            AssetIconName::File
        );
        assert_eq!(
            CairnApp::result_file_icon("Reports", "", true),
            AssetIconName::Folder
        );

        assert_eq!(
            CairnApp::result_file_icon("Grok Bot", "lnk", false),
            AssetIconName::FileSymlink,
            "a Shell row hides the .lnk suffix from its display name"
        );
        assert_eq!(
            CairnApp::result_file_icon("Screenshot", "png", false),
            AssetIconName::FileImage,
            "a display name that hides its suffix still resolves through the recovered extension"
        );
        assert_eq!(
            CairnApp::result_file_icon(".gitignore", "gitignore", false),
            AssetIconName::FileCode,
            "a bare dotfile keeps the taxonomy's configuration meaning"
        );
        assert_eq!(
            CairnApp::result_file_icon("CMakeLists.txt", "txt", false),
            AssetIconName::FileCode,
            "a name the taxonomy already knows outranks its ordinary suffix"
        );
    }

    #[test]
    fn the_recycle_bin_headers_name_the_values_they_hold() {
        assert_eq!(column_label_key(SortColumn::Name, true), "col_name");
        assert_eq!(
            column_label_key(SortColumn::Path, true),
            "col_original_location"
        );
        assert_eq!(
            column_label_key(SortColumn::DateModified, true),
            "col_deleted"
        );
        assert_eq!(column_label_key(SortColumn::Path, false), "col_path");
        assert_eq!(
            column_label_key(SortColumn::DateModified, false),
            "col_modified"
        );
    }

    #[test]
    fn a_bin_delete_is_words_apart_from_the_reversible_delete() {
        assert_eq!(
            result_command_label_key(GroupCommand::DeleteSelected, true),
            "ctx_delete_permanently"
        );
        assert_eq!(
            result_command_label_key(GroupCommand::DeleteSelected, false),
            "ctx_delete"
        );
        assert_eq!(
            result_command_label_key(GroupCommand::RestoreSelected, true),
            "ctx_restore"
        );
    }

    #[test]
    fn a_content_query_explains_an_index_that_cannot_answer() {
        let mut state = crate::shell::menu_rules::IndexMenuState {
            scope: crate::service::search_scope::SearchScope::AllFiles,
            service_installed: true,
            service_connected: true,
            switching_scope: false,
            content_loading: false,
            content_failed: false,
            content_action_pending: false,
            local_mode: false,
            content_state: &ContentIndexState::Ready,
        };

        state.local_mode = true;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Local),
            "the user indexer carries file names only, even when connected"
        );

        state.local_mode = false;
        state.service_connected = false;
        assert_eq!(
            content_query_notice(&state),
            None,
            "the offline band owns the disconnected state"
        );

        state.service_connected = true;
        state.content_loading = true;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Loading)
        );

        state.content_loading = false;
        state.content_failed = true;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Unreadable),
            "a status read failure must not look like a disabled index"
        );

        state.content_failed = false;
        state.content_state = &ContentIndexState::Disabled;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Disabled)
        );

        let backfilling = ContentIndexState::Backfilling {
            processed: 3,
            total: 10,
        };
        state.content_state = &backfilling;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Backfilling)
        );

        let failed = ContentIndexState::Error("worker failed".into());
        state.content_state = &failed;
        assert_eq!(
            content_query_notice(&state),
            Some(ContentQueryNotice::Error)
        );

        state.content_state = &ContentIndexState::Ready;
        assert_eq!(
            content_query_notice(&state),
            None,
            "a ready index makes zero rows a genuine no-match"
        );
    }

    #[test]
    fn the_skeleton_pulse_never_dims_a_row_out_of_sight() {
        for row in 0..16 {
            for step in 0..40 {
                let opacity = pulse_opacity(row, step as f32 / 40.0);
                assert!(
                    (0.45..=1.0).contains(&opacity),
                    "row {row} at step {step} rendered {opacity}"
                );
            }
        }
    }

    #[test]
    fn adjacent_skeleton_rows_peak_at_different_times() {
        assert_ne!(
            pulse_opacity(0, 0.0),
            pulse_opacity(5, 0.0),
            "a single block-wide blink is not a travelling pulse"
        );
        assert!(
            (pulse_opacity(0, 1.0) - pulse_opacity(0, 0.0)).abs() < f32::EPSILON,
            "the pulse must repeat seamlessly"
        );
    }

    #[test]
    fn rows_are_positioned_from_a_local_residual_at_any_depth() {
        let row_height = 28.0;
        assert_eq!(rows_band_local_top(0, 0.0, row_height), 0.0);
        assert_eq!(rows_band_local_top(100, 2810.0, row_height), -10.0);
        assert_eq!(rows_band_local_top(97, 2810.0, row_height), -94.0);

        // A full index reaches tens of millions of pixels. The scroll offset is already f32 and
        // may round, but the residual handed to layout must stay inside the overscan band and the
        // rows after it must keep the exact row pitch.
        let scroll_top = 1_300_000.0f32 * row_height - 10.0;
        let start_row = (scroll_top / row_height).floor() as usize;
        let first = start_row.saturating_sub(3);
        let local = rows_band_local_top(first, scroll_top, row_height);
        assert!(
            (-4.0 * row_height..=0.0).contains(&local),
            "local top {local} escaped the overscan band"
        );
        let later = rows_band_local_top(first + 3, scroll_top, row_height);
        assert_eq!(
            later - local,
            3.0 * row_height,
            "deep rows must keep the exact row pitch"
        );
    }

    #[test]
    fn a_viewport_inside_the_window_paints_at_its_scroll_residual() {
        let painted = rows_to_paint(100..120, 0..200, 2810.0, 440.0, 28.0);
        assert_eq!(painted.range, 100..120);
        assert_eq!(painted.top, -10.0);
        assert!(!painted.pinned);
    }

    #[test]
    fn an_unloaded_jump_keeps_the_nearest_loaded_rows_on_screen() {
        let row_height = 28.0;
        let viewport = 440.0;

        // Scrolled below the window: its tail stays on screen, bottom-aligned to the band.
        let visible = 1000..1016;
        let loaded = 0..200;
        let painted = rows_to_paint(visible.clone(), loaded, 28_000.0, viewport, row_height);
        assert_eq!(
            painted.range.end, 200,
            "the tail of the window is the closest data"
        );
        assert_eq!(painted.range.len(), visible.len());
        assert!(
            painted.pinned,
            "rows from another scroll position are a preview"
        );
        assert_eq!(
            painted.top + painted.range.len() as f32 * row_height,
            viewport,
            "the pinned tail must end at the band's bottom edge"
        );

        // Scrolled above the window: its head stays on screen, top-aligned.
        let painted = rows_to_paint(0..16, 200..400, 0.0, viewport, row_height);
        assert_eq!(painted.range, 200..216);
        assert_eq!(painted.top, 0.0);
        assert!(painted.pinned);
    }

    #[test]
    fn a_fully_loaded_result_set_is_never_pinned() {
        let painted = rows_to_paint(84..101, 0..101, 2_380.0, 440.0, 28.0);
        assert_eq!(painted.range, 84..101);
        assert_eq!(painted.top, 84.0 * 28.0 - 2_380.0);
        assert!(!painted.pinned);
    }

    #[test]
    fn the_bottom_edge_shade_disappears_before_the_last_row() {
        let row = 28.0;
        assert_eq!(rows_edge_opacity(0.0, row), 0.0);
        assert_eq!(rows_edge_opacity(-5.0, row), 0.0);
        assert_eq!(rows_edge_opacity(row * 0.5, row), 0.5);
        assert_eq!(rows_edge_opacity(row, row), 1.0);
        assert_eq!(rows_edge_opacity(row * 10.0, row), 1.0);
        assert_eq!(rows_edge_opacity(row, 0.0), 0.0);
    }
}

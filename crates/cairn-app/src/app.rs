// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cairn main window: the file-manager shell — title bar, locations sidebar, search band, command
//! bar, virtualized result table and status line.
//!
//! This module owns the window's *state and behaviour*. The layout itself lives in [`crate::view`],
//! which reads these fields and hands interactions back to the owner methods below. Keyboard commands
//! are Actions declared in `crate::commands` and dispatched as Actions against this view's
//! `key_context`, which the window root tracks; they therefore reach the owner methods whichever
//! descendant holds focus.
//!
//! The table renders only the rows the viewport requires ([`crate::table::model::visible_range`]). Indices
//! beyond the fetched window render as loading placeholders and are pulled from the service through
//! the QRY-07 `(offset, limit)` contract, so rendered element count tracks the window height instead
//! of the result count.

use crate::search::filters::FilterCategory;
use crate::search::input::{QueryTextMetrics, SearchInputState};
use crate::service::search_scope::SearchScope;
use crate::shell::command_groups::MenuGroup;
use crate::shell::i18n::I18nManager;
use crate::shell::menu_actions::sort_column_from_key;
use crate::shell::settings::AppSettings;
use crate::shell::status_bar::StatusBarModel;
use crate::table::model::{ColumnBoundary, ColumnWidths, TableState};
use cairn_protocol::status::ContentIndexStatus;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::gpui::*;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// Every repeated distance and size in the shell, as a multiple of the window's `rem` — which is
/// the theme's base font, so zoom moves spacing, type and control frames together. The title-bar
/// section is the exception: its value belongs to the platform geometry, so it stays in `Pixels`.
///
/// The view applies these through `rems(...)`, so the band a region renders at and the arithmetic
/// that measures it come from one definition instead of two numbers that can drift; the scale
/// helpers (`h_8`, `px_3`) are only used where a value is not a shell token. A band's own hairline
/// is not an extra boundary — GPUI lays a box out border-box, so it sits inside the height pinned
/// for that band.
///
/// # The ladder has two rungs
///
/// `CONTROL_HEIGHT` and `ROW_HEIGHT` are the only numbers chosen for themselves. Everything else
/// is an expression over them so every band scales from the same two density tokens. A band that
/// wants its own step gets a new named relationship, not a new literal, and the invariant test
/// below verifies that every band height is derived from `CONTROL_HEIGHT` or `ROW_HEIGHT`:
/// `CairnApp::the_band_measures_rows_in_the_same_rem_step_the_row_renders_at` fails the suite when
/// someone writes a literal where an expression belongs.
pub mod layout {
    use gpui_kit::component::TITLE_BAR_HEIGHT as KIT_TITLE_BAR_HEIGHT;
    use gpui_kit::gpui::{Pixels, Window};

    use crate::shell::theme;

    // ---- the 4px ladder ------------------------------------------------------------
    /// One 4px step in the window's rem. Every layout distance is an integer multiple of this, so
    /// a 100% window renders whole 4px values while the interface zoom still scales the shell.
    /// The type scale stays at the compact 14px base; spacing and type are separate token families.
    pub const STEP: f32 = 4.0 / theme::BASE_FONT_PX;
    pub const SPACE_4: f32 = 1.0 * STEP;
    pub const SPACE_8: f32 = 2.0 * STEP;
    pub const SPACE_12: f32 = 3.0 * STEP;
    pub const SPACE_24: f32 = 6.0 * STEP;
    pub const SPACE_32: f32 = 8.0 * STEP;
    pub const SPACE_48: f32 = 12.0 * STEP;

    // ---- the two base steps --------------------------------------------------------
    /// One interactive control: the query box, the table heading cell, a toolbar trigger. Controls
    /// are what a pointer aims at, so they get the taller of the two steps.
    pub const CONTROL_HEIGHT: f32 = SPACE_32;
    /// One result row: 7 four-pixel steps. Virtualization math requires it to be constant for a
    /// frame. Navigation rows share this step: the sidebar is a list before it is a toolbar.
    pub const ROW_HEIGHT: f32 = 7.0 * STEP;

    // ---- bands, each one an expression over the base steps --------------------------
    /// The 8px inner step: padding inside a row, lane, section or band, and the gap between related
    /// parts such as an icon and its label or two peer controls. One token covers both roles so the
    /// same distance cannot drift between an inset and a gap.
    pub const INNER_SPACING: f32 = SPACE_8;
    /// The search band: the query box plus the tight inset that keeps it off the band's edges.
    pub const SEARCH_BAND_HEIGHT: f32 = CONTROL_HEIGHT + 2.0 * TIGHT_LEAD;
    /// The query box inside the search band. The primary task is a control like any other, so it
    /// takes the control step rather than a step of its own.
    pub const SEARCH_INPUT_HEIGHT: f32 = CONTROL_HEIGHT;
    /// The column-heading row shares the result row's height so it reads as the first row of the
    /// table, not a taller control band above it.
    pub const HEADER_HEIGHT: f32 = ROW_HEIGHT;
    /// The status line: the compact data row, so it takes the result-row step. The shell pins it
    /// so its arithmetic and the rendered band cannot drift apart.
    pub const STATUS_HEIGHT: f32 = ROW_HEIGHT;
    /// The Treemap category legend is a compact status item: one 12px swatch plus a text_xs
    /// label inside a 20px hit row. Its 2px horizontal inset is an optical exception, like
    /// `MENU_LEAD`, and stays off the 4px ladder.
    pub const LEGEND_ROW: f32 = 5.0 * STEP;
    pub const LEGEND_SWATCH: f32 = 3.0 * STEP;
    pub const LEGEND_INSET: f32 = 0.5 * STEP;

    // ---- column lanes, in 4px steps ------------------------------------------------
    // The heading row and every result row read the same three widths, so a column is one definition.
    /// Narrowest name lane: below this a filename stops being the thing the row is for.
    pub const NAME_MIN_COLUMN: f32 = 42.0 * STEP; // 168px
    /// Narrowest path lane, kept wide enough that the trailing folder still reads.
    pub const PATH_MIN_COLUMN: f32 = 28.0 * STEP; // 112px
    pub const SIZE_COLUMN: f32 = 21.0 * STEP; // 84px
    pub const DATE_COLUMN: f32 = 33.0 * STEP; // 132px
    /// The narrowest table that can show all four lanes at their own floors. The heading row and the
    /// rows share this extent, and the band scrolls to it horizontally, so a wide zoom in a short
    /// window moves the columns out of view instead of cutting the last one off at the frame edge.
    /// Derived from the lanes so it cannot disagree with them.
    pub const TABLE_MIN_WIDTH: f32 = NAME_MIN_COLUMN + PATH_MIN_COLUMN + SIZE_COLUMN + DATE_COLUMN;
    /// How far one keyboard step moves the columns, as a fraction of the width the window actually
    /// shows of the table.
    ///
    /// A fixed pixel step would be a different distance at every zoom and in every window: two steps
    /// would clear the table on a laptop and never reach the last column on a wide monitor. A quarter
    /// of the visible table keeps the whole horizontal extent four steps away however the frame is
    /// sized, and leaves the columns the eye just read partly on screen so the move does not lose the
    /// row being compared.
    pub const TABLE_SCROLL_STEP_FACTOR: f32 = 0.25;
    /// Widest an explanatory line runs before it wraps: long enough to read as one sentence, short
    /// enough that the message never decides the width of the region holding it.
    pub const MESSAGE_MAX_WIDTH: f32 = 154.0 * STEP; // 616px
    /// A confirmation asks one short question. 384px at 100% keeps a filename and its explanation
    /// readable without the half-empty surface the framework's 448px default leaves behind.
    pub const CONFIRMATION_WIDTH: f32 = 96.0 * STEP; // 384px
    /// Estimated height used only to seat a short confirmation before GPUI has laid it out. It is a
    /// rem value so the seat scales with interface zoom instead of drifting at 90/110%.
    pub const CONFIRMATION_ESTIMATED_HEIGHT: f32 = 150.0 / theme::BASE_FONT_PX; // ~150px at 100%

    // ---- panes ---------------------------------------------------------------------
    /// The sidebar keeps its previous rendered width on the 4px ladder: 208px and 152px at 100%.
    pub const SIDEBAR_MIN_WIDTH: f32 = 38.0 * STEP;
    pub const SIDEBAR_WIDTH: f32 = 52.0 * STEP;
    /// Sidebar leading glyph. It reads the shared control-glyph token (1rem = 14 px at 100%);
    /// the 0.875rem row label sits inside the 1–1.25em icon band. A graphic size, not a layout
    /// distance, so it is not part of the 4px ladder test.
    pub const SIDEBAR_ICON: f32 = theme::UI_ICON_SIZE;

    // ---- physical title-bar geometry -----------------------------------------------
    /// The platform-owned title-bar step, shared with GPUI Kit's components. It is not a `rem`
    /// multiple: the bar and its caption buttons must agree with the window chrome at every zoom.
    pub const TITLE_BAR_HEIGHT: Pixels = KIT_TITLE_BAR_HEIGHT;
    /// The app mark's own 20 px token (5 × 4 px), separate from the 16 px wordmark.
    pub const TITLE_BAR_ICON: Pixels = cairn_ui::brand::APP_ICON_SIZE;
    /// Caption buttons are square and use the bar's full step on both axes.
    pub const WINDOW_CONTROL_WIDTH: Pixels = TITLE_BAR_HEIGHT;

    /// The shared leading edge of the content column: query band, table heading, row icon and
    /// status summary all inset by this. This is the text/icon baseline the title-bar menu, search
    /// glyph and table heading align to.
    pub const CONTENT_INSET: f32 = SPACE_12;
    /// The tighter outer lead used when an inner control contributes the rest of the leading
    /// inset. A small Ghost Button and a sidebar row both pad by [`INNER_SPACING`], so these sum
    /// back to `CONTENT_INSET` for the visible text or icon.
    pub const TIGHT_LEAD: f32 = SPACE_4;
    /// Optical correction: 文 carries less left side bearing than 名, so without it the first menu
    /// character's ink sits just left of the search glyph and the Name heading. This is an optical
    /// 2px exception to the 4px ladder, like the 1px hairline.
    const MENU_OPTICAL_LEAD: f32 = 2.0 / theme::BASE_FONT_PX;
    /// Menu trigger container lead: the content edge minus the small Button's own 8px padding,
    /// plus the optical correction for the first glyph's side bearing.
    pub const MENU_LEAD: f32 = CONTENT_INSET - INNER_SPACING + MENU_OPTICAL_LEAD;
    /// Search band outer lead, followed by the query box's own leading padding.
    pub const SEARCH_BAND_LEAD: f32 = TIGHT_LEAD;
    pub const SEARCH_INPUT_LEAD: f32 = CONTENT_INSET - SEARCH_BAND_LEAD;
    /// Sidebar viewport lead, followed by the row's own [`INNER_SPACING`].
    pub const SIDEBAR_VIEWPORT_LEAD: f32 = TIGHT_LEAD;

    /// The bands stacked between the window's top edge and the scrollable row band, in the order the
    /// shell renders them, each paired with the name the view pins it under. The status line is in the
    /// total because it sits below the row band and takes the same window height away from it.
    ///
    /// Nothing else belongs in the window's column: the title bar is platform geometry and the result
    /// band is what is left over. The hairlines are deliberately absent — GPUI lays a box out with
    /// `box-sizing: border-box` (taffy's default), so a band's 1px edge is drawn *inside* the height
    /// pinned above, and counting it again would shrink the measured viewport by four pixels.
    ///
    /// The names are carried for the invariant test, which reads this array as the definition of
    /// "which bands the arithmetic knows about" instead of keeping a second list.
    pub const CHROME_BANDS: [(&str, f32); 3] = [
        ("SEARCH_BAND_HEIGHT", SEARCH_BAND_HEIGHT),
        ("HEADER_HEIGHT", HEADER_HEIGHT),
        ("STATUS_HEIGHT", STATUS_HEIGHT),
    ];

    /// Height the result band cannot use: every band stacked above and below it.
    ///
    /// Two approximations are left in this number on purpose, and both are far below one row: GPUI
    /// rounds each laid-out box to a device pixel while this rounds the sum once, and `Window::bounds`
    /// reports the window rect the platform hands back rather than a measured content area. The view
    /// over-fetches by one row plus [`crate::table::model::visible_range`]'s overscan, so neither can strand
    /// a visible row unrendered.
    pub fn chrome_height(window: &Window) -> Pixels {
        let bands: f32 = CHROME_BANDS.iter().map(|(_, height)| height).sum();
        TITLE_BAR_HEIGHT + theme::rem(window) * bands
    }

    /// How much of this frame the result band actually gets, once the rest of the shell is placed.
    pub fn viewport_height(window: &Window) -> Pixels {
        (window.bounds().size.height - chrome_height(window)).max(Pixels::ZERO)
    }

    /// Row height for this frame's `rem`: the number the virtualization math multiplies indices by.
    pub fn row_height(window: &Window) -> Pixels {
        theme::rem(window) * ROW_HEIGHT
    }

    /// Rows requested per service page (QRY-07).
    pub const RESULT_PAGE_SIZE: usize = 200;
}

/// Stored column widths are logical pixels at 100% zoom; one rem is 16 of them.
pub(crate) const PX_PER_REM: f32 = 16.0;

/// One in-flight header-boundary drag. The widths at press time are carried so every move
/// recomputes the same answer instead of accumulating pointer deltas.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ColumnDrag {
    pub boundary: ColumnBoundary,
    pub widths: ColumnWidths,
    pub start_x: Pixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan<'a> {
    pub text: &'a str,
    pub is_highlight: bool,
}

pub use crate::search::query_orchestration::split_name_highlights;
pub use crate::shell::nav_history::NavState;

/// Joins a file name to a parent directory that may or may not carry a trailing separator.
pub fn join_row_path(directory: &str, name: &str) -> String {
    if directory.ends_with('\\') || directory.ends_with('/') {
        format!("{directory}{name}")
    } else {
        format!("{directory}\\{name}")
    }
}

/// Element id of one member of a repeated family: the prefix names the family, the key keeps every
/// member distinct inside the same stateful parent, which is what GPUI needs to build a stable
/// `GlobalElementId`. `ElementId` only converts from a single string (or from a string plus an
/// integer), so the two parts are joined here rather than passed as an invented tuple.
pub fn element_id(prefix: &'static str, key: &str) -> String {
    format!("{prefix}:{key}")
}

/// Everything behind the query box: the input state, painted text geometry,
/// match toggles, the active filter and drive scope, and the session the
/// current answer belongs to.
pub struct QueryDomain {
    pub search_input: SearchInputState,
    /// Painted geometry of the query text, written by the text-area canvas each frame. Pointer and
    /// IME hit testing read it instead of the query box's outer bounds, which include the glyph and
    /// padding before the first character.
    pub query_text_metrics: Rc<RefCell<QueryTextMetrics>>,
    /// Focus region owning text entry. It receives focus when the window opens so the first frame
    /// is already typeable (SPEC UI-03: keyboard-first users).
    pub search_focus: FocusHandle,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub match_path: bool,
    pub use_regex: bool,
    pub active_filter: FilterCategory,
    /// Drive letter the sidebar scopes the search to, when one is active. The service reads it as a
    /// `X:` term, so the scope is part of the query the user can still edit by hand.
    pub scope_drive: Option<char>,
    pub query_id: u64,
    pub active_session_id: Option<u64>,
    pub is_fetching_more: bool,
    pub is_searching: bool,
}

/// The result table: focus, scroll owners, viewport state and lane geometry.
pub struct ResultsDomain {
    /// Focus region of the result table, and the only region that answers Left, Right, Home and End.
    /// Tab hands it focus from the query box; while the query box holds focus those four keys stay
    /// the caret's, which is why the bindings live in
    /// [`crate::shell::commands::RESULTS_CONTEXT`] instead of the window context.
    pub results_focus: FocusHandle,
    /// Scroll owner of the result band; its offset drives windowed rendering.
    pub rows_scroll: ScrollHandle,
    /// Scroll owner of the table as a whole — heading row and rows together — on the axis the window
    /// cannot show. Its `x` component is where the columns stand inside the frame, and it is the one
    /// value both the visible scrollbar and the keyboard path write.
    pub table_scroll: ScrollHandle,
    pub table: TableState,
    /// Lane widths in rem, the one geometry the heading row, real rows and skeleton rows share.
    pub column_widths: ColumnWidths,
    /// Set between a header boundary's mouse down and its release.
    pub(crate) column_drag: Option<ColumnDrag>,
    /// `true` while the last input was a keystroke. The result region paints its focus ring from
    /// this, so a pointer click selects a row without drawing a second, redundant border around
    /// the entire table.
    pub keyboard_navigation: bool,
}

/// The indexer this window talks to, and everything it reports back.
pub struct ServiceDomain {
    pub is_service_connected: bool,
    /// `true` while the window is served by its own user-privilege indexer instead of the installed
    /// LocalSystem service. This is a normal mode, not a failure: the local scan sees fewer files,
    /// but every result view works.
    pub local_mode: bool,
    /// The user-mode indexer this window started. It exits with the window through its
    /// `--parent-pid` monitor; the handle is kept so switching to the service can end it at once.
    pub(crate) user_indexer: Option<std::process::Child>,
    /// Whether SCM knows `CairnService`. It decides whether the recovery band can offer an elevated
    /// start; opening the window never asks for elevation.
    pub service_installed: bool,
    /// One silent SCM start attempt per connection-loss episode. If it succeeds the probe is
    /// repeated; if access is denied the explicit UAC button is the remaining route.
    pub(crate) has_attempted_silent_start: bool,
    /// One status-refresh loop while a volume is building or catching up; it stops at the first
    /// all-Synced answer so the skeleton and the index capsule can disappear.
    pub(crate) is_refreshing_status: bool,
    /// `true` while a connection probe is in flight. It is what makes the retry button answer at once
    /// and refuse a second click while the first is still unanswered, so the state has to belong to
    /// the probe and not to the button.
    pub is_probing_service: bool,
    pub service_issue: Option<crate::service::service_issue::ServiceIssue>,
    pub status: StatusBarModel,
    /// The service's content-index lifecycle, including backfill progress and the last error.
    pub content_status: ContentIndexStatus,
    pub content_status_loading: bool,
    /// Why the last status read failed. Kept apart from [`ServiceDomain::content_action_error`]
    /// because a status that could not be read is not the same condition as a service-side
    /// content-index failure, and neither may be shown as "off".
    pub content_status_error: Option<String>,
    pub content_action_pending: bool,
    /// A local submission failure (config write/pipe), kept separate from the service's own
    /// content-index error so the page can show both without overwriting one with the other.
    pub content_action_error: Option<String>,
    /// The scope the window is currently reading from (UI-10). It is the user's choice, so it is
    /// remembered in the settings and survives a restart.
    pub search_scope: SearchScope,
    /// `true` while a scope switch is running. The menu row that started it must not start a
    /// second one, and the window must not claim the new scope before it is actually in force.
    pub(crate) is_switching_scope: bool,
    /// Bumped whenever the window starts reading from a different indexer. A status poll that
    /// belongs to the previous scope sees the change and stops, so the window never mixes the
    /// state of two indexers.
    pub(crate) status_generation: u64,
}

pub struct CairnApp {
    pub i18n: Rc<I18nManager>,
    pub query: QueryDomain,
    pub results: ResultsDomain,
    pub service: ServiceDomain,
    /// Focus region carrying the window key context and the command handlers. Every dropdown menu
    /// dispatches its Actions here, so a menu item and a keystroke reach one owner method.
    pub root_focus: FocusHandle,
    pub zoom_level: f32,
    pub settings: AppSettings,
    /// Whether Windows is using its dark app appearance, read once per run and refreshed when the
    /// settings dialog opens. `theme = system` resolves through this value.
    pub system_dark: bool,
    pub open_group: Option<MenuGroup>,
    /// The menu entity of the open group, built from the live command state the moment it opens.
    pub group_menu: Option<Entity<PopupMenu>>,
    pub group_menu_dismiss: Option<Subscription>,
    /// Window-level focus-lost subscriptions, held so they stay registered for
    /// the lifetime of the view. Never read: dropping this field would cancel
    /// the focus restoration above.
    pub _focus_lost: Vec<Subscription>,
    /// Records whether the keyboard drives focus, including the Tab the focus walk consumes.
    pub _keyboard_modality: Subscription,
    pub sidebar_collapsed: bool,
    pub nav_history: Vec<NavState>,
    pub nav_history_index: usize,
    pub sidebar_scroll: ScrollHandle,
    /// When `Some(drive)`, the main content area renders the disk visualization (Treemap).
    pub drive_view: Option<char>,
    pub treemap_state: Option<crate::treemap::state::TreemapViewState>,
    /// The Recycle Bin view. The bin is a location, not a search scope: the view owns its rows,
    /// its selection and its error band together, so any query or drive switch releases the whole
    /// location instead of leaving half of it behind.
    pub(crate) recycle_bin: crate::shell::recycle_bin::RecycleBinView,
    /// True while a delete-to-bin task is running outside the bin.
    pub(crate) delete_in_flight: bool,
    /// The app-owned transient status surface. One at a time; errors persist.
    pub toast: Option<crate::shell::toast::ToastState>,
    pub(crate) toast_serial: u64,
}

impl CairnApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::shell::perf::mark("app new");
        let settings = AppSettings::load();
        let system_dark = crate::shell::theme::system_prefers_dark();
        let i18n = Rc::new(I18nManager::new(&settings.effective_language()));

        // `TableState` keeps its fetched window private, so the restored sort state is assigned
        // through its public fields instead of a struct literal.
        let mut table = TableState::default();
        table.sort_column = sort_column_from_key(&settings.columns.sort_column);
        table.sort_asc = settings.columns.sort_asc;
        let column_widths = ColumnWidths {
            name: (settings.columns.name_width / PX_PER_REM).max(layout::NAME_MIN_COLUMN),
            path: (settings.columns.path_width / PX_PER_REM).max(layout::PATH_MIN_COLUMN),
            size: (settings.columns.size_width / PX_PER_REM).max(layout::SIZE_COLUMN),
            date: (settings.columns.modified_width / PX_PER_REM).max(layout::DATE_COLUMN),
        };

        let search_focus = cx.focus_handle().tab_stop(true);
        let root_focus = cx.focus_handle();
        // A tab stop in its own right: the table owns a keyboard path that nothing else answers, so
        // Tab has to be able to reach it (Design Guides Accessibility checklist: "every action is
        // reachable and operable by keyboard"). Where it falls in the walk is decided by paint order
        // — the query box and the four toolbar triggers come first — and no test pins that index, so
        // none is claimed here.
        let results_focus = cx.focus_handle().tab_stop(true);

        // Keep the window key context reachable: if a rendered region disappears and focus is
        // dropped, return it to the nearest still-rendered target rather than leaving the commands
        // unreachable (Focus guide: restore lost focus).
        let focus_lost = cx.on_focus_lost(window, |this, window, cx| {
            let target = window
                .focus_lost_restore_target(cx)
                .filter(|handle| this.query.search_focus.contains(handle, window))
                .unwrap_or_else(|| this.query.search_focus.clone());
            target.focus(window, cx);
        });
        let keyboard_modality = cairn_ui::focus::observe_keyboard(cx);

        // The service registration is read once here: it decides both whether the window may
        // offer the machine-wide scope and which scope a first run opens in.
        let service_state = crate::service::service_control::service_state();

        let mut app = Self {
            i18n,
            query: QueryDomain {
                search_input: SearchInputState::default(),
                query_text_metrics: Rc::new(RefCell::new(QueryTextMetrics::default())),
                search_focus,
                case_sensitive: false,
                whole_word: false,
                match_path: false,
                use_regex: false,
                active_filter: FilterCategory::All,
                scope_drive: None,
                query_id: 0,
                active_session_id: None,
                is_fetching_more: false,
                is_searching: false,
            },
            results: ResultsDomain {
                results_focus,
                rows_scroll: ScrollHandle::new(),
                table_scroll: ScrollHandle::new(),
                table,
                column_widths,
                column_drag: None,
                keyboard_navigation: false,
            },
            service: ServiceDomain {
                is_service_connected: true,
                local_mode: false,
                user_indexer: None,
                // The probe below is already in flight by the time the first frame is drawn.
                is_probing_service: true,
                service_issue: None,
                status: StatusBarModel::default(),
                content_status: ContentIndexStatus::default(),
                content_status_loading: false,
                content_status_error: None,
                content_action_pending: false,
                content_action_error: None,
                service_installed: crate::service::service_control::service_is_registered(
                    service_state,
                ),
                has_attempted_silent_start: false,
                is_refreshing_status: false,
                // A saved scope is the user's own choice and wins; without one the window follows
                // the machine: a service that is already running is the answer the window would
                // otherwise have to ask for, and taking it never prompts for admin rights.
                search_scope: crate::service::search_scope::initial_scope(
                    settings.search_scope,
                    crate::service::service_control::service_is_running(service_state),
                ),
                is_switching_scope: false,
                status_generation: 0,
            },
            root_focus,
            zoom_level: settings.zoom_level,
            settings,
            system_dark,
            open_group: None,
            group_menu: None,
            group_menu_dismiss: None,
            _focus_lost: vec![focus_lost],
            _keyboard_modality: keyboard_modality,
            sidebar_collapsed: false,
            nav_history: vec![NavState {
                text: String::new(),
                drive: None,
                filter: FilterCategory::All,
            }],
            nav_history_index: 0,
            sidebar_scroll: ScrollHandle::default(),
            drive_view: None,
            treemap_state: None,
            recycle_bin: crate::shell::recycle_bin::RecycleBinView::closed(),
            delete_in_flight: false,
            toast: None,
            toast_serial: 0,
        };
        window.focus(&app.query.search_focus, cx);
        app.check_service_connection(cx);
        app
    }

    pub fn set_initial_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.search_input.text = text.to_string();
        self.query.search_input.selection = text.len()..text.len();
        self.trigger_search(cx);
    }

    /// The font the painted query text used. Before the first paint there is no capture, so the
    /// window's current text style is the one the next paint will use.
    fn query_text_style(&self, window: &Window) -> (Pixels, TextStyle) {
        let metrics = self.query.query_text_metrics.borrow();
        if metrics.font_size > px(0.0) {
            (metrics.font_size, metrics.style.clone())
        } else {
            let style = window.text_style();
            (style.font_size.to_pixels(window.rem_size()), style)
        }
    }

    /// Shapes the query as one line. A `LineLayout` reports byte offsets into this same string,
    /// which is the unit the selection range and [`crate::search::input::Utf16Utf8Mapper`] use.
    fn query_text_layout(&self, window: &Window) -> Arc<LineLayout> {
        let text = &self.query.search_input.text;
        let (font_size, style) = self.query_text_style(window);
        window
            .text_system()
            .layout_line(text, font_size, &[style.to_run(text.len())], None)
    }

    /// UTF-8 byte offset under `point`, measured from the painted text origin.
    ///
    /// GPUI's line layout supplies the real glyph advances, so CJK and emoji are hit where they were
    /// drawn instead of every character being assumed 8px wide. `closest_index_for_x` already clamps
    /// before the first glyph and past the last one; the boundary walk keeps the result splittable
    /// for a cluster edge. Callers convert to UTF-16 only at the platform boundary.
    pub fn query_byte_index_for_point(&self, point: Point<Pixels>, window: &Window) -> usize {
        let text = &self.query.search_input.text;
        if text.is_empty() {
            return 0;
        }
        let origin_x = self.query.query_text_metrics.borrow().bounds.origin.x;
        let x = (point.x - origin_x).max(px(0.0));
        let layout = self.query_text_layout(window);
        let mut byte = layout.closest_index_for_x(x).min(text.len());
        while byte > 0 && !text.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }
}

/// Trims the process working set, discarding unreferenced memory pages back to the operating system.
///
/// On Windows, this calls `SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX)`,
/// which drops the working set from ~120MB down to ~1-2MB when the application is idle or hidden to tray.
#[cfg(windows)]
pub fn trim_working_set() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
pub fn trim_working_set() {}

/// Hides the main Cairn window to the system tray and aggressively trims the process working set.
#[cfg(windows)]
pub fn hide_main_window() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SW_HIDE};

    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("Cairn")) {
            if !hwnd.0.is_null() {
                let _ = ShowWindow(hwnd, SW_HIDE);
                trim_working_set();
            }
        }
    }
}

#[cfg(not(windows))]
pub fn hide_main_window() {}

impl EntityInputHandler for CairnApp {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        let u8_range = mapper.utf16_to_utf8_range(range_utf16.clone());
        let start = u8_range.start.min(self.query.search_input.text.len());
        let end = u8_range.end.min(self.query.search_input.text.len());
        if start <= end {
            *adjusted_range = Some(range_utf16);
            Some(self.query.search_input.text[start..end].to_string())
        } else {
            None
        }
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        let range = mapper.utf8_to_utf16_range(self.query.search_input.selection.clone());
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.query.search_input.marked_range.as_ref()?;
        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        Some(mapper.utf8_to_utf16_range(marked.clone()))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.query.search_input.marked_range = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query
            .search_input
            .handle_replace_text_in_range(replacement_range, text);
        self.trigger_search(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query
            .search_input
            .handle_replace_and_mark_text_in_range(range_utf16, new_text, new_selected_range);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The platform hands us the painted text area; keep it so a click after an IME commit uses
        // this frame's origin rather than a stale one.
        self.query.query_text_metrics.borrow_mut().bounds = element_bounds;

        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        let byte_index = mapper
            .utf16_to_utf8_offset(range_utf16.end)
            .min(self.query.search_input.text.len());
        let caret_x = self.query_text_layout(window).x_for_index(byte_index);
        Some(crate::search::input::candidate_window_bounds(
            element_bounds,
            caret_x,
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let byte_index = self.query_byte_index_for_point(point, window);
        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        Some(mapper.utf8_to_utf16_offset(byte_index))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        self.query.search_input.selection = mapper.utf16_to_utf8_range(range_utf16);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{join_row_path, layout};
    use crate::shell::theme::BASE_FONT_PX;

    #[test]
    fn row_paths_join_without_doubling_the_separator() {
        assert_eq!(join_row_path("C:\\dir", "a.rs"), "C:\\dir\\a.rs");
        assert_eq!(join_row_path("C:\\dir\\", "a.rs"), "C:\\dir\\a.rs");
    }

    #[test]
    fn sidebar_icon_stays_inside_the_label_icon_band() {
        let label = 0.875;
        assert!((label..=label * 1.25).contains(&layout::SIDEBAR_ICON));
    }

    #[test]
    fn every_shell_distance_lands_on_the_four_pixel_ladder() {
        assert_eq!(
            BASE_FONT_PX, 14.0,
            "the compact type scale anchors the 4px step at 100% zoom"
        );
        // MENU_LEAD and LEGEND_INSET are excluded: their 2px optical corrections are intentional.
        // The ratio tokens are not distances. Everything that lays out the shell must be a whole
        // 4px step.
        let distances = [
            layout::STEP,
            layout::SPACE_4,
            layout::SPACE_8,
            layout::SPACE_12,
            layout::SPACE_24,
            layout::SPACE_32,
            layout::SPACE_48,
            layout::CONTROL_HEIGHT,
            layout::ROW_HEIGHT,
            layout::INNER_SPACING,
            layout::SEARCH_BAND_HEIGHT,
            layout::SEARCH_INPUT_HEIGHT,
            layout::HEADER_HEIGHT,
            layout::STATUS_HEIGHT,
            layout::LEGEND_ROW,
            layout::LEGEND_SWATCH,
            layout::NAME_MIN_COLUMN,
            layout::PATH_MIN_COLUMN,
            layout::SIZE_COLUMN,
            layout::DATE_COLUMN,
            layout::TABLE_MIN_WIDTH,
            layout::MESSAGE_MAX_WIDTH,
            layout::SIDEBAR_MIN_WIDTH,
            layout::SIDEBAR_WIDTH,
            layout::CONTENT_INSET,
            layout::TIGHT_LEAD,
            layout::SEARCH_BAND_LEAD,
            layout::SEARCH_INPUT_LEAD,
            layout::SIDEBAR_VIEWPORT_LEAD,
        ];
        for distance in distances {
            let pixels = distance * BASE_FONT_PX;
            assert!(
                (pixels / 4.0).fract().abs() < 0.001,
                "{distance}rem = {pixels}px is off the 4px ladder"
            );
        }
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_band_measures_rows_in_the_same_rem_step_the_row_renders_at() {
        assert!(
            layout::ROW_HEIGHT > 1.0 && layout::ROW_HEIGHT < 3.0,
            "row height {} is outside a readable dense band",
            layout::ROW_HEIGHT
        );
        assert_eq!(
            layout::HEADER_HEIGHT,
            layout::ROW_HEIGHT,
            "the heading row shares the result row's height"
        );
        assert_eq!(
            layout::SEARCH_INPUT_HEIGHT,
            layout::CONTROL_HEIGHT,
            "the query box is a control like any other"
        );
        assert_eq!(
            layout::SEARCH_BAND_HEIGHT,
            layout::CONTROL_HEIGHT + 2.0 * layout::TIGHT_LEAD,
            "the search band is its control plus one tight inset on each side"
        );
        assert!(
            layout::INNER_SPACING > 0.0,
            "a band with no inset presses its hairline against the control inside it"
        );
        assert_eq!(
            layout::STATUS_HEIGHT,
            layout::ROW_HEIGHT,
            "the status line shares the result row's compact step"
        );
        assert!(
            layout::CONTROL_HEIGHT >= layout::HEADER_HEIGHT,
            "a control may not be shorter than the heading row it acts on"
        );
    }

    fn heights_the_view_pins(source: &str) -> Vec<String> {
        let mut pinned = Vec::new();
        for (at, _) in source.match_indices("layout::") {
            let tail = &source[at + "layout::".len()..];
            let name: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let applied_as_height = source[..at]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .ends_with("rems(");
            if name.ends_with("_HEIGHT") && applied_as_height {
                pinned.push(name);
            }
        }
        pinned
    }

    #[test]
    fn every_band_the_view_pins_is_counted_once_in_chrome_height() {
        let combined = [
            include_str!("view.rs"),
            include_str!("search/bar_view.rs"),
            include_str!("shell/sidebar_view.rs"),
            include_str!("shell/status_bar_view.rs"),
            include_str!("table/results_view.rs"),
        ]
        .join("\n");
        let pinned = heights_the_view_pins(&combined);
        let bands = layout::CHROME_BANDS.map(|(name, _)| name);
        let pins = |name: &str| pinned.iter().any(|found| found == name);

        // These are rows inside a band, not chrome stacked around the result band.
        let inside_a_band = ["CONTROL_HEIGHT", "SEARCH_INPUT_HEIGHT", "ROW_HEIGHT"];
        for name in &pinned {
            assert!(
                bands.contains(&name.as_str()) || inside_a_band.contains(&name.as_str()),
                "the view pins {name} as a height, which chrome_height neither adds up nor explains"
            );
        }
        for name in bands {
            assert!(
                pins(name),
                "chrome_height adds up {name}, but no band is pinned with it"
            );
        }
        assert!(
            pinned.len() > bands.len(),
            "the scan found {pinned:?}; the shell pins at least the {bands:?} bands"
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_column_lanes_are_shared_and_leave_room_for_the_path() {
        assert!(layout::NAME_MIN_COLUMN > layout::PATH_MIN_COLUMN);
        assert!(layout::DATE_COLUMN > layout::SIZE_COLUMN);
        assert!(
            layout::SIDEBAR_WIDTH >= layout::SIDEBAR_MIN_WIDTH,
            "the default sidebar cannot be narrower than its minimum"
        );
    }
}

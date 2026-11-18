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
use crate::service::{ServiceConnection, SubsystemState};
use crate::shell::command_groups::MenuGroup;
use crate::shell::i18n::I18nManager;
use crate::shell::settings::AppSettings;
use crate::shell::status_bar::StatusBarModel;
use crate::shell::window_resize::Observed;
use crate::table::model::{ColumnBoundary, ColumnWidths, TableState};
use cairn_protocol::status::{
    ContentIndexStatus, IndexJournalEntry, IndexJournalStatus, PropertyIndexStatus,
    SourceStatusSummary,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::gpui::*;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

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
/// `CONTROL_HEIGHT` and `ROW_HEIGHT` are the only numbers chosen for themselves. Every band is an
/// expression over the row step, and taller pointer targets use the control step. A band that wants
/// its own step gets a new named relationship, not a new literal, and the invariant test below
/// verifies that every band height is a named step:
/// `CairnApp::the_band_measures_rows_in_the_same_rem_step_the_row_renders_at` fails the suite when
/// someone writes a literal where an expression belongs.
pub mod layout {
    use gpui_kit::component::TITLE_BAR_HEIGHT as KIT_TITLE_BAR_HEIGHT;
    use gpui_kit::gpui::{Pixels, Window};

    use crate::shell::theme;

    /// One 4px step in the window's rem so a 100% window renders whole 4px values while zoom scales.
    pub const STEP: f32 = 4.0 / theme::BASE_FONT_PX;
    pub const SPACE_4: f32 = 1.0 * STEP;
    pub const SPACE_8: f32 = 2.0 * STEP;
    pub const SPACE_12: f32 = 3.0 * STEP;
    pub const SPACE_24: f32 = 6.0 * STEP;
    pub const SPACE_32: f32 = 8.0 * STEP;
    pub const SPACE_48: f32 = 12.0 * STEP;

    pub const CONTROL_HEIGHT: f32 = SPACE_32;
    /// Virtualization math requires a fixed row height per frame; sidebar rows share this step.
    pub const ROW_HEIGHT: f32 = 7.0 * STEP;

    pub const INNER_SPACING: f32 = SPACE_8;
    pub const SEARCH_BAND_HEIGHT: f32 = ROW_HEIGHT + 2.0 * TIGHT_LEAD;
    pub const HEADER_HEIGHT: f32 = ROW_HEIGHT;
    pub const STATUS_HEIGHT: f32 = ROW_HEIGHT;
    // Heading and result rows read the same widths so column dividers never drift.
    pub const NAME_MIN_COLUMN: f32 = 42.0 * STEP;
    pub const PATH_MIN_COLUMN: f32 = 28.0 * STEP;
    pub const SIZE_COLUMN: f32 = 21.0 * STEP;
    pub const DATE_COLUMN: f32 = 33.0 * STEP;
    pub const CREATED_COLUMN: f32 = DATE_COLUMN;
    pub const LENGTH_COLUMN: f32 = 22.0 * STEP;
    pub const AUTHOR_COLUMN: f32 = 32.0 * STEP;
    pub const TAGS_COLUMN: f32 = 34.0 * STEP;
    pub const DIMENSIONS_COLUMN: f32 = 26.0 * STEP;
    pub const FRAMERATE_COLUMN: f32 = 21.0 * STEP;
    pub const TABLE_SCROLL_STEP_FACTOR: f32 = 0.25;
    pub const MESSAGE_MAX_WIDTH: f32 = 154.0 * STEP;
    /// Baseline optical correction centering the 24px state icon beside the serif heading line box.
    pub const STATE_ACTION_OPTICAL_OFFSET: f32 = 1.5 / theme::BASE_FONT_PX;
    pub const DIALOG_WIDTH: f32 = 96.0 * STEP;
    pub const DIALOG_SEAT_HEIGHT: f32 = 32.0 * STEP;

    pub const SIDEBAR_WIDTH: f32 = 52.0 * STEP;
    pub const SIDEBAR_ICON: f32 = theme::UI_ICON_SIZE;

    /// Platform title-bar height shared with `gpui-kit` components rather than scaled by `rem`.
    pub const TITLE_BAR_HEIGHT: Pixels = KIT_TITLE_BAR_HEIGHT;
    pub const TITLE_BAR_ICON: Pixels = cairn_ui::brand::APP_ICON_SIZE;
    /// The shared leading edge of the content column: query band, table heading, row icon and
    /// status summary all inset by this. This is the text/icon baseline the title-bar menu, search
    /// glyph and table heading align to.
    pub const CONTENT_INSET: f32 = SPACE_12;
    /// The tighter outer lead used when an inner control contributes the rest of the leading
    /// inset. A small Ghost Button and a sidebar row both pad by [`INNER_SPACING`], so these sum
    /// back to `CONTENT_INSET` for the visible text or icon.
    pub const TIGHT_LEAD: f32 = SPACE_4;
    /// Menu trigger container lead. The small Button's own 8px padding completes the content
    /// inset, so the trigger's label starts on the content spine and its hover surface shares the
    /// query box's and result panel's outer edge.
    pub const MENU_LEAD: f32 = TIGHT_LEAD;
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

/// Stored column widths are expressed in this fixed unit. Load and save both convert through it,
/// so the factor cancels; it stays constant so existing settings keep their widths while the
/// shell's rem follows zoom.
pub(crate) const COLUMN_WIDTH_STORAGE_UNIT: f32 = 16.0;

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
/// match toggles, the active filter, and the session the current answer belongs to.
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
    pub pinyin: bool,
    pub ignore_punctuation: bool,
    pub ignore_whitespace: bool,
    pub ignore_diacritics: bool,
    pub active_filter: FilterCategory,
    /// The indexed source the current query is restricted to, or `None` when the query spans
    /// every indexed source. The sidebar's folder-source rows set and clear it.
    pub source_scope: Option<cairn_protocol::message::SourceId>,
    pub active_session_id: Option<u64>,
    /// The service's refusal of the current query, if it answered with one. Cleared when a new
    /// query starts and when a result set arrives, so the band never outlives its query.
    pub rejection: Option<crate::search::query_orchestration::QueryRejection>,
    /// Non-fatal degradation notice reported by the service for the current query.
    pub degraded: Option<cairn_protocol::message::QueryDegradedReason>,
    pub is_fetching_more: bool,
    pub(crate) coordinator: crate::search::coordinator::SearchCoordinator,
    /// Byte offset where mouse text drag selection started, if a drag is in progress.
    pub text_selection_anchor: Option<usize>,
}

impl QueryDomain {
    pub(crate) fn query_id(&self) -> u64 {
        self.coordinator.current_query_id()
    }

    pub(crate) fn is_searching(&self) -> bool {
        self.coordinator.is_searching()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.coordinator.is_cancelled()
    }
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
    /// Lane widths in rem, shared by the heading row and content rows.
    pub column_widths: ColumnWidths,
    /// Set between a header boundary's mouse down and its release.
    pub(crate) column_drag: Option<ColumnDrag>,
    /// Active content-snippet preview popover state, when open on a row.
    pub(crate) snippet_preview: Option<crate::table::snippets::SnippetPreviewState>,
}

/// The indexer this window talks to, and everything it reports back.
pub struct ServiceDomain {
    pub(crate) connection: ServiceConnection,
    /// One status-refresh loop while a volume is building or catching up; it stops at the first
    /// all-Synced answer so the sidebar index status line can disappear.
    pub(crate) is_refreshing_status: bool,
    pub status: StatusBarModel,
    /// Persistent named-pipe session reused across status, query, page and config RPCs.
    pub session: cairn_client::ServiceSession,
    pub content: SubsystemState<ContentIndexStatus>,
    pub property: SubsystemState<PropertyIndexStatus>,
    pub journal: SubsystemState<IndexJournalStatus>,
    pub journal_entries: SubsystemState<Vec<IndexJournalEntry>>,
    pub sources: Vec<SourceStatusSummary>,
    /// One add/remove request at a time: the manager dialog disables its actions until the
    /// service answers, so a double press cannot register the same path twice.
    pub(crate) folder_source_action_pending: bool,
    /// One `GetSources` read at a time; the follow-up poll only starts once the previous answer
    /// has landed.
    pub(crate) is_refreshing_sources: bool,
}

pub(crate) struct OpenGroupMenu {
    pub group: MenuGroup,
    pub popup: Entity<PopupMenu>,
    pub _dismiss: Subscription,
    pub entered: bool,
}

/// The in-place rename editor: the selected row swaps its name cell for this input while a draft
/// is present. A shell write owns the draft until it answers, so a second Enter cannot duplicate it.
pub(crate) struct RenameEditor {
    pub input: Entity<InputState>,
    pub draft: Option<crate::shell::rename::RenameDraft>,
    pub in_flight: bool,
    pub _subscription: Subscription,
}

pub struct TableRevealState {
    pub generation: u64,
    pub fade_epoch: u64,
    pub pending: bool,
}

impl TableRevealState {
    pub fn new() -> Self {
        Self {
            generation: 0,
            fade_epoch: 0,
            pending: true,
        }
    }
}

pub struct CairnApp {
    pub i18n: Rc<I18nManager>,
    pub(crate) label_cache: crate::shell::i18n::StaticLabelCache,
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
    pub(crate) open_group_menu: Option<OpenGroupMenu>,
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
    /// The in-place rename editor for the single selected result row.
    pub(crate) rename: RenameEditor,
    /// The global summon hotkey; the guard unregisters the chord when it drops.
    #[cfg(windows)]
    pub(crate) _hotkey: Option<crate::shell::hotkey::GlobalHotkey>,
    /// The Recycle Bin view. The bin is a location, not a search scope: the view owns its rows,
    /// its selection and its error band together, so any query releases the whole location
    /// instead of leaving half of it behind.
    pub(crate) recycle_bin: crate::shell::recycle_bin::RecycleBinView,
    /// True while a delete-to-bin task is running outside the bin.
    pub(crate) delete_in_flight: bool,
    /// The app-owned transient status surface. One at a time; errors persist.
    pub toast: Option<crate::shell::toast::ToastState>,
    pub(crate) toast_serial: u64,
    pub table_reveal: TableRevealState,
    /// True while the GitHub release check is in flight. The Help menu row carries the loading
    /// icon and the menu is reopened after the click; no separate waiting dialog or toast is opened.
    pub(crate) is_checking_update: bool,
    /// The live window resize and the cover it owns, judged by this window's own geometry.
    pub(crate) window_resize: crate::shell::window_resize::WindowResizeTracker,
    /// Keeps the bounds observer alive for the window's lifetime.
    window_resize_subscription: Option<Subscription>,
}

impl CairnApp {
    #[cfg(test)]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bundle = crate::startup::StartupCoordinator::prepare(
            Vec::new(),
            &crate::startup::SystemBootstrapPorts,
        );
        Self::new_with_startup(bundle, window, cx)
    }

    pub(crate) fn new_with_startup(
        startup: crate::startup::StartupBundle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::shell::perf::mark("app new");
        let crate::startup::StartupBundle {
            plan: _,
            settings,
            system_dark,
            i18n,
            session,
        } = startup;

        // `TableState` keeps its fetched window private, so the restored sort state is assigned
        // through its public fields instead of a struct literal.
        let mut table = TableState::default();
        table.sort_spec = crate::table::model::SortSpec::parse_clause(&settings.columns.sort_spec)
            .unwrap_or_default();
        let column_widths = ColumnWidths {
            name: (settings.columns.name_width / COLUMN_WIDTH_STORAGE_UNIT)
                .max(layout::NAME_MIN_COLUMN),
            path: (settings.columns.path_width / COLUMN_WIDTH_STORAGE_UNIT)
                .max(layout::PATH_MIN_COLUMN),
            size: (settings.columns.size_width / COLUMN_WIDTH_STORAGE_UNIT)
                .max(layout::SIZE_COLUMN),
            date: (settings.columns.modified_width / COLUMN_WIDTH_STORAGE_UNIT)
                .max(layout::DATE_COLUMN),
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

        let rename_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n.get_text("rename_label")));
        let rename_subscription = cx.subscribe_in(
            &rename_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                // A blur only moves the keyboard; the draft stays so a press
                // inside the editor never discards what was typed. Only Escape
                // cancels the rename.
                InputEvent::Blur => {}
                InputEvent::Change => this.rename_text_changed(cx),
                InputEvent::Focus => {}
            },
        );

        #[cfg(all(windows, not(test)))]
        let hotkey = crate::shell::hotkey::register()
            .map_err(|err| tracing::warn!("global hotkey unavailable: {err}"))
            .ok();
        // Unit tests never register a real system-wide chord or read the real
        // update marker; both are process-level side effects, not view state.
        #[cfg(all(windows, test))]
        let hotkey: Option<crate::shell::hotkey::GlobalHotkey> = None;
        #[cfg(all(windows, not(test)))]
        let hotkey_unavailable = hotkey.is_none();

        crate::shell::theme::ensure_shell_tokens(cx);

        let mut app = Self {
            i18n,
            label_cache: Default::default(),
            query: QueryDomain {
                search_input: SearchInputState::default(),
                query_text_metrics: Rc::new(RefCell::new(QueryTextMetrics::default())),
                search_focus,
                case_sensitive: false,
                whole_word: false,
                match_path: false,
                use_regex: false,
                pinyin: false,
                ignore_punctuation: false,
                ignore_whitespace: false,
                ignore_diacritics: false,
                active_filter: FilterCategory::All,
                source_scope: None,
                active_session_id: None,
                rejection: None,
                degraded: None,
                is_fetching_more: false,
                coordinator: crate::search::coordinator::SearchCoordinator::default(),
                text_selection_anchor: None,
            },
            results: ResultsDomain {
                results_focus,
                rows_scroll: ScrollHandle::new(),
                table_scroll: ScrollHandle::new(),
                table,
                column_widths,
                column_drag: None,
                snippet_preview: None,
            },
            service: ServiceDomain {
                connection: ServiceConnection::initial(),
                is_refreshing_status: false,
                status: StatusBarModel::default(),
                session,
                content: SubsystemState::default(),
                property: SubsystemState::default(),
                journal: SubsystemState::default(),
                journal_entries: SubsystemState::default(),
                sources: Vec::new(),
                folder_source_action_pending: false,
                is_refreshing_sources: false,
            },
            root_focus,
            zoom_level: settings.zoom_level,
            settings,
            system_dark,
            open_group_menu: None,
            _focus_lost: vec![focus_lost],
            _keyboard_modality: keyboard_modality,
            sidebar_collapsed: false,
            nav_history: vec![NavState {
                text: String::new(),
                filter: FilterCategory::All,
                source_scope: None,
            }],
            nav_history_index: 0,
            sidebar_scroll: ScrollHandle::default(),
            rename: RenameEditor {
                input: rename_input,
                draft: None,
                in_flight: false,
                _subscription: rename_subscription,
            },
            #[cfg(windows)]
            _hotkey: hotkey,
            recycle_bin: crate::shell::recycle_bin::RecycleBinView::closed(),
            delete_in_flight: false,
            toast: None,
            toast_serial: 0,
            table_reveal: TableRevealState::new(),
            is_checking_update: false,
            window_resize: Default::default(),
            window_resize_subscription: None,
        };
        window.focus(&app.query.search_focus, cx);
        // The window's own bounds event is the only trustworthy signal that a resize is
        // happening: a drag runs inside the system's modal loop, GPUI consumes the end of it
        // internally, and nothing repaints once the geometry goes quiet. Observing it here
        // keeps the cover out of `render`, which must not mutate state or start tasks.
        app.window_resize_subscription =
            Some(cx.observe_window_bounds(window, |this, window, cx| {
                let size = (
                    f32::from(window.bounds().size.width),
                    f32::from(window.bounds().size.height),
                );
                if let Observed::Started(epoch) = this.window_resize.observe(size, Instant::now()) {
                    this.watch_window_resize(epoch, cx);
                    cx.notify();
                }
            }));
        #[cfg(not(test))]
        {
            #[cfg(windows)]
            app.schedule_startup_notices(hotkey_unavailable, cx);
            #[cfg(not(windows))]
            app.schedule_startup_notices(false, cx);
        }
        app.check_service_connection(cx);
        app
    }

    pub(crate) fn cached_text(&self, key: &'static str) -> gpui_kit::gpui::SharedString {
        self.label_cache.get(&self.i18n, key)
    }

    pub(crate) fn cached_control_name(
        &self,
        control: &crate::shell::accessibility::KeyControl,
    ) -> gpui_kit::gpui::SharedString {
        self.label_cache.get(&self.i18n, control.message())
    }

    pub(crate) fn cached_control_name_at(
        &self,
        control: &crate::shell::accessibility::KeyControl,
        index: usize,
    ) -> gpui_kit::gpui::SharedString {
        self.label_cache.get(&self.i18n, control.message_at(index))
    }

    /// Opens a fresh table session. Every entry point that replaces the result set starts
    /// one, so the entrance waits for real content instead of firing over an empty frame.
    pub(crate) fn begin_table_session(&mut self) {
        self.table_reveal.pending = true;
    }

    /// Replaces the visible result set as a new location. The old rows leave at
    /// once so a category or history switch never shows the previous place's
    /// rows while the new query runs; the first new content then fades in.
    /// Typed edits keep their rows and do not call this.
    pub(crate) fn begin_table_replacement(&mut self) {
        self.begin_table_session();
        self.results.rows_scroll.set_offset(Point::default());
        self.results.table.scroll_top = 0.0;
        self.results.table.set_rows(Vec::new(), 0);
        self.query.active_session_id = None;
        self.service.status.clear_objects_count();
    }

    /// Starts the results panel's entrance and closes the pending session.
    pub(crate) fn bump_table_reveal(&mut self, cx: &mut Context<Self>) {
        self.table_reveal.generation = self.table_reveal.generation.wrapping_add(1);
        self.table_reveal.pending = false;
        self.table_reveal.fade_epoch = self.table_reveal.generation;
        let fade_epoch = self.table_reveal.fade_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::shell::motion::MASK_DISSOLVE_CLEANUP)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.table_reveal.fade_epoch == fade_epoch {
                    this.table_reveal.fade_epoch = 0;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Starts the table entrance once a panel session actually has rows, so the
    /// fade begins with content rather than while the query is still in flight.
    pub(crate) fn note_table_content(&mut self, cx: &mut Context<Self>) {
        if self.table_reveal.pending && self.results.table.total_rows() > 0 {
            self.bump_table_reveal(cx);
        }
    }

    /// Starts the same entrance for a new session whose answer is empty; the
    /// state band is content too, not a reason to leave the old frame static.
    pub(crate) fn note_table_empty(&mut self, cx: &mut Context<Self>) {
        if self.table_reveal.pending {
            self.bump_table_reveal(cx);
        }
    }

    pub fn set_initial_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.search_input.text = text.to_string();
        self.query.search_input.selection = text.len()..text.len();
        self.request_result_set(crate::search::ResultSetChange::Replace, cx);
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
    pub(crate) fn query_text_layout(&self, window: &Window) -> Arc<LineLayout> {
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
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle to this process; `usize::MAX` asks Windows to empty the working set.
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
pub fn trim_working_set() {}

/// Hides the main Cairn window to the system tray and aggressively trims the process working set.
#[cfg(windows)]
pub fn hide_main_window() {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    if let Some(hwnd) = crate::shell::tray::find_main_window() {
        // SAFETY: `hwnd` was found by `find_main_window` for the current process window.
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        trim_working_set();
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
        let (start, end) = self.query.search_input.normalized_selection();
        let mapper = crate::search::input::Utf16Utf8Mapper::new(&self.query.search_input.text);
        let range = mapper.utf8_to_utf16_range(start..end);
        let reversed =
            self.query.search_input.selection.start > self.query.search_input.selection.end;
        Some(UTF16Selection { range, reversed })
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
        self.schedule_text_search(cx);
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
pub(crate) use tests::test_row;

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{join_row_path, CairnApp};
    use crate::search::ResultSetChange;
    use cairn_protocol::message::{FileRowData, SourceId};
    use cairn_protocol::status::{
        SourceKind, SourceStatusSummary, VolumeState, VolumeStatusSummary,
    };
    use gpui_kit::AppContext;
    pub(crate) fn test_row(name: &str) -> FileRowData {
        FileRowData {
            frn: 1,
            name: name.to_string(),
            path: r"C:\".to_string(),
            size: 1,
            modified_ts: 0,
            created_ts: 0,
            is_dir: false,
            flags: 0,
            name_highlights: Vec::new(),
            path_highlights: Vec::new(),
            snippets: Vec::new(),
            properties: None,
        }
    }

    fn with_app(
        test: impl FnOnce(&mut CairnApp, &mut gpui_kit::gpui::Context<CairnApp>) + 'static,
    ) {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));
                    app.update(cx, test);
                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn row_paths_join_without_doubling_the_separator() {
        assert_eq!(join_row_path("C:\\dir", "a.rs"), "C:\\dir\\a.rs");
        assert_eq!(join_row_path("C:\\dir\\", "a.rs"), "C:\\dir\\a.rs");
    }

    fn folder_source(id: u32, kind: SourceKind) -> SourceStatusSummary {
        SourceStatusSummary {
            source_id: SourceId(id),
            kind,
            root_path: r"D:\Work".to_string(),
            volume_guid: String::new(),
            status: VolumeState::Synced,
            indexed_count: 10,
            live_watch_enabled: true,
        }
    }

    /// A folder-source row scopes the current query; pressing the same row again releases it, and
    /// an NTFS source never owns the scope. The text and category stay untouched.
    #[test]
    fn a_folder_source_row_sets_and_clears_the_query_scope() {
        use crate::search::filters::FilterCategory;

        with_app(|this, cx| {
            this.service.sources = vec![
                folder_source(2, SourceKind::NtfsVolume),
                folder_source(1000, SourceKind::Folder),
            ];
            this.query.search_input.set_text("report");
            this.query.active_filter = FilterCategory::Document;

            this.select_folder_source(SourceId(1000), cx);
            assert_eq!(this.query.source_scope, Some(SourceId(1000)));
            assert_eq!(this.query.search_input.text, "report");
            assert_eq!(this.query.active_filter, FilterCategory::Document);
            assert!(
                this.query.is_searching(),
                "the place change re-asks the query"
            );
            assert_eq!(
                this.results.table.total_rows(),
                0,
                "the previous source's rows leave at once"
            );

            this.select_folder_source(SourceId(1000), cx);
            assert!(
                this.query.source_scope.is_none(),
                "a second press releases the scope"
            );
            assert!(this.query.is_searching());

            this.select_folder_source(SourceId(2), cx);
            assert!(
                this.query.source_scope.is_none(),
                "an NTFS volume is not a folder-source scope"
            );
        });
    }

    /// A file-type row is a location and releases the scope, even when that category was already
    /// active.
    #[test]
    fn a_file_type_row_releases_the_query_scope() {
        use crate::search::filters::FilterCategory;

        with_app(|this, cx| {
            this.service.sources = vec![folder_source(1000, SourceKind::Folder)];
            this.select_folder_source(SourceId(1000), cx);
            assert!(this.query.source_scope.is_some());

            this.set_filter(FilterCategory::All, cx);
            assert!(this.query.source_scope.is_none());
            assert_eq!(this.query.active_filter, FilterCategory::All);
        });
    }

    /// A source removed by the service can no longer own the query: the next snapshot releases
    /// the scope and asks for the unscoped result set.
    #[test]
    fn a_removed_source_releases_its_query_scope() {
        with_app(|this, cx| {
            this.service.sources = vec![folder_source(1000, SourceKind::Folder)];
            this.select_folder_source(SourceId(1000), cx);
            assert!(this.query.source_scope.is_some());

            this.apply_sources_snapshot(Vec::new(), cx);

            assert!(this.query.source_scope.is_none());
            assert!(this.query.is_searching());
        });
    }

    /// A location switch replaces the old result set at once and opens a fresh
    /// entrance; the fade begins only when the new page actually arrives.
    #[test]
    fn a_category_switch_starts_a_replacement_entrance() {
        use crate::search::filters::FilterCategory;

        with_app(|this, cx| {
            this.results.table.set_rows(vec![test_row("old.txt")], 1);
            this.table_reveal.pending = false;
            let generation = this.table_reveal.generation;

            this.set_filter(FilterCategory::Document, cx);
            assert!(
                this.table_reveal.pending,
                "a new location opens a table session"
            );
            assert_eq!(
                this.results.table.total_rows(),
                0,
                "the previous location's rows leave at once"
            );
            assert_eq!(
                this.table_reveal.generation, generation,
                "no fade runs before the new content exists"
            );

            this.results.table.set_rows(vec![test_row("new.docx")], 1);
            this.note_table_content(cx);
            assert_eq!(
                this.table_reveal.fade_epoch,
                generation + 1,
                "the new page fades in"
            );
        });
    }

    /// The status-bar refresh of a settled index is a replacement: the rows leave
    /// at once and the refreshed page fades in.
    #[test]
    fn a_table_refresh_replaces_the_result_set_at_once() {
        with_app(|this, cx| {
            this.results.table.set_rows(vec![test_row("old.txt")], 1);
            this.table_reveal.pending = false;
            let generation = this.table_reveal.generation;

            this.refresh_current_view(cx);
            assert!(this.table_reveal.pending, "refresh opens a table session");
            assert_eq!(
                this.results.table.total_rows(),
                0,
                "the stale rows leave before the new page exists"
            );
            assert_eq!(this.table_reveal.generation, generation);

            this.results.table.set_rows(vec![test_row("old.txt")], 1);
            this.note_table_content(cx);
            assert_eq!(
                this.table_reveal.fade_epoch,
                generation + 1,
                "the refreshed page fades in"
            );
        });
    }

    /// Sorting is in-place: the rows stay on screen and no entrance runs.
    #[test]
    fn a_sort_keeps_the_rows_without_a_table_entrance() {
        with_app(|this, cx| {
            this.results.table.set_rows(vec![test_row("old.txt")], 1);
            this.table_reveal.pending = false;
            let epoch = this.table_reveal.fade_epoch;

            this.request_result_set(ResultSetChange::InPlace, cx);
            assert!(!this.table_reveal.pending, "sorting opens no session");
            assert_eq!(this.results.table.total_rows(), 1, "the rows stay");
            assert_eq!(this.table_reveal.fade_epoch, epoch, "nothing fades");
        });
    }

    /// A probe is a refresh, not a place change: rows stay, no entrance runs;
    /// an empty table still gets the first-content entrance.
    #[test]
    fn a_probe_refreshes_silently_once_rows_exist() {
        with_app(|this, cx| {
            this.results.table.set_rows(vec![test_row("a.txt")], 1);
            this.table_reveal.pending = false;
            let epoch = this.table_reveal.fade_epoch;

            this.request_result_set(ResultSetChange::Probe, cx);
            assert!(
                !this.table_reveal.pending,
                "a probe over rows opens no session"
            );
            assert_eq!(this.results.table.total_rows(), 1, "the rows stay");
            assert_eq!(this.table_reveal.fade_epoch, epoch, "nothing fades");

            this.results.table.set_rows(Vec::new(), 0);
            this.request_result_set(ResultSetChange::Probe, cx);
            assert!(
                this.table_reveal.pending,
                "an empty table gets the replacement entrance"
            );
        });
    }

    /// A refresh keeps the rows that already describe this place until the index
    /// poll can answer it; a place change clears them at once even mid catch-up.
    #[test]
    fn a_refresh_keeps_the_rows_while_a_place_change_does_not() {
        use crate::search::filters::FilterCategory;

        fn catching_up(this: &mut CairnApp) {
            this.service
                .status
                .update_volumes(vec![VolumeStatusSummary {
                    drive_letter: 'C',
                    volume_guid: r"\\?\Volume{test}\".to_string(),
                    status: VolumeState::CatchingUp(50),
                    indexed_count: 1,
                    usn_enabled: true,
                }]);
        }

        with_app(|this, cx| {
            this.results.table.set_rows(vec![test_row("old.txt")], 1);
            this.table_reveal.pending = false;
            catching_up(this);

            this.request_result_set(ResultSetChange::Refresh, cx);
            assert_eq!(
                this.results.table.total_rows(),
                1,
                "a refresh keeps rows that already describe this place"
            );
            assert!(
                !this.table_reveal.pending,
                "the poll answers the refresh without an entrance"
            );

            this.set_filter(FilterCategory::All, cx);
            assert_eq!(
                this.results.table.total_rows(),
                1,
                "re-selecting the active location is the same refresh"
            );

            this.request_result_set(ResultSetChange::Replace, cx);
            assert_eq!(
                this.results.table.total_rows(),
                0,
                "another place's rows never wait for the index"
            );
            assert!(
                this.table_reveal.pending,
                "the place change owes its own first page"
            );
        });
    }

    #[test]
    fn a_local_takeover_supersedes_an_in_flight_query_and_clears_the_session() {
        with_app(|this, cx| {
            this.request_result_set(ResultSetChange::InPlace, cx);
            let stale_id = this.query.query_id();
            this.query.active_session_id = Some(7);

            this.take_table_for_local_rows();

            assert_ne!(this.query.query_id(), stale_id, "the answer is superseded");
            assert!(
                this.query.active_session_id.is_none(),
                "the old session can no longer page the new rows"
            );
            assert!(
                !this.query.is_searching(),
                "the coordinator holds no stale in-flight query"
            );
        });
    }

    /// The index-loading shortcut must supersede an answer still on its way;
    /// otherwise it installs rows for text the replacement already left.
    #[test]
    fn a_replacement_supersedes_the_answer_the_index_guard_leaves_behind() {
        with_app(|this, cx| {
            this.results.table.set_rows(Vec::new(), 0);
            this.request_result_set(ResultSetChange::InPlace, cx);
            let stale_id = this.query.query_id();
            assert!(this.query.is_searching(), "the first request is in flight");

            this.service
                .status
                .update_volumes(vec![VolumeStatusSummary {
                    drive_letter: 'C',
                    volume_guid: r"\\?\Volume{test}\".to_string(),
                    status: VolumeState::CatchingUp(50),
                    indexed_count: 1,
                    usn_enabled: true,
                }]);
            this.request_result_set(ResultSetChange::Replace, cx);

            assert_ne!(
                this.query.query_id(),
                stale_id,
                "the replacement supersedes the in-flight answer"
            );
            assert!(!this.query.is_searching(), "the stale request is stopped");
            assert!(
                this.table_reveal.pending,
                "the replacement waits for its own first page"
            );
        });
    }

    #[test]
    fn table_reveal_fade_epoch_advances_and_cleans_up() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));
                    app.update(cx, |this, cx| {
                        assert_eq!(this.table_reveal.fade_epoch, 0);
                        this.table_reveal.pending = true;
                        this.bump_table_reveal(cx);
                        assert_eq!(this.table_reveal.fade_epoch, 1);
                        assert!(!this.table_reveal.pending);
                    });
                    cx.quit();
                    app
                },
            );
        });
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! NFR-11 single source of truth: every key control of the main window, its AccessKit role and the
//! Fluent messages it speaks.
//!
//! The view does not name roles or accessible labels inline. It asks this registry for the control it
//! is about to build ([`element`], [`element_at`], [`name`], [`name_at`]), so one table answers three
//! questions at once: which controls must be observable, what role each reports, and which message
//! carries its name in zh-CN and en-US (UI-01, UI-02, UI-03, UI-07, SVC-03, SVC-04, SVC-05).
//!
//! The regression tests at the bottom run in the ordinary `cargo test -p cairn-app` build. They hold
//! the table together (unique keys, informative roles, no missing or blank translation) and check that
//! the render path really consumes it, which is what stops an entry from becoming a promise the
//! window never keeps. Observing those roles inside a live window — the half that proves the window
//! reports what the table claims — must be checked in a live window; the app has no headless UI harness.

use crate::shell::i18n::I18nManager;
use gpui_kit::base::{ObservedElement, TestSupportExt};
use gpui_kit::gpui::{div, Div, InteractiveElement, Role, Stateful, StatefulInteractiveElement};

/// One key control as assistive technology must see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyControl {
    /// Registry identity, unique inside [`KEY_CONTROLS`]. The state variants of one control are
    /// separate entries, because each reports a different role.
    pub key: &'static str,
    /// Element id the view gives the control, exactly as GPUI sees it. Members of a repeated family
    /// record the id the view assembles, and state variants of one control share an id.
    pub element: &'static str,
    /// `true` when the view pairs [`element`](KeyControl::element) with a row or capsule index.
    pub indexed: bool,
    /// AccessKit role the view must report. Never [`Role::Unknown`]: an unclassified role is not an
    /// accessible one.
    pub role: Role,
    /// Fluent messages this control speaks: the accessible name first, then any announced value or
    /// state. Every one of them has to resolve to non-blank text on both sides.
    pub messages: &'static [&'static str],
    /// When [`messages`](KeyControl::messages) is empty, the code that supplies the announced text
    /// instead: the data a control acts on, or a message map owned by a model.
    pub named_by: &'static str,
    /// SPEC requirement this control answers.
    pub requirement: &'static str,
}

impl KeyControl {
    /// The single message carrying this control's accessible name.
    ///
    /// A control with no registered message is named by [`named_by`](KeyControl::named_by), and one
    /// that speaks several picks by state through [`message_at`](KeyControl::message_at).
    pub fn message(&self) -> &'static str {
        assert!(
            self.messages.len() == 1,
            "{} speaks {} messages; the view must pick one by state",
            self.key,
            self.messages.len()
        );
        self.messages[0]
    }

    /// The `index`-th message this control speaks.
    pub fn message_at(&self, index: usize) -> &'static str {
        self.messages.get(index).unwrap_or_else(|| {
            panic!(
                "{} speaks {} messages, so index {index} is not registered",
                self.key,
                self.messages.len()
            )
        })
    }
}

const EMPTY: &[&str] = &[];

/// Every key control of the main window. Adding a control means adding a row here, and the tests
/// below reject a row the render path does not build.
pub const KEY_CONTROLS: &[KeyControl] = &[
    // ---- locations sidebar: the file-type rows, and the column that holds them (UI-01, QRY-06) ----
    // ---- title bar: sidebar toggle, back/forward navigation (UI-01) --------------
    KeyControl {
        key: "sidebar-toggle",
        element: "sidebar-toggle",
        indexed: false,
        role: Role::Button,
        messages: &["sidebar_toggle"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "nav-back",
        element: "nav-back",
        indexed: false,
        role: Role::Button,
        messages: &["nav_back"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "nav-forward",
        element: "nav-forward",
        indexed: false,
        role: Role::Button,
        messages: &["nav_forward"],
        named_by: "",
        requirement: "UI-01",
    },
    // Platform caption buttons. They are drawn as squares on the title bar, but they are commands
    // in every other respect: keyboard-reachable, named, and reported as buttons.
    KeyControl {
        key: "window-minimize",
        element: "window-minimize",
        indexed: false,
        role: Role::Button,
        messages: &["window_minimize"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "window-maximize",
        element: "window-maximize",
        indexed: false,
        role: Role::Button,
        messages: &["window_maximize", "window_restore"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "window-close",
        element: "window-close",
        indexed: false,
        role: Role::Button,
        messages: &["window_close"],
        named_by: "",
        requirement: "UI-01",
    },
    // The section heading is the view's own element. The rows are `SidebarMenuItem`s, which report
    // `Role::TreeItem` themselves; the registry still names each one, by the model key the view
    // assembles into its element id, so a category cannot gain a sidebar row without a message.
    KeyControl {
        key: "sidebar-locations",
        element: "sidebar-locations",
        indexed: false,
        role: Role::Heading,
        messages: &["sidebar_locations"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "sidebar-drives",
        element: "sidebar-drives",
        indexed: false,
        role: Role::Heading,
        messages: &["sidebar_drives"],
        named_by: "",
        requirement: "UI-01, IDX-01",
    },
    // A drive row is named by its drive letter and its index state, so no fixed Fluent message can
    // hold its name; the view assembles it from the same labels the row shows.
    KeyControl {
        key: "sidebar-drive",
        element: "sidebar-drive",
        indexed: true,
        role: Role::TreeItem,
        messages: EMPTY,
        named_by: "drive_label",
        requirement: "UI-01, IDX-01",
    },
    KeyControl {
        key: "sidebar-location-all",
        element: "sidebar-location:all",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_all"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-doc",
        element: "sidebar-location:doc",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_doc"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-image",
        element: "sidebar-location:image",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_image"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-audio",
        element: "sidebar-location:audio",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_audio"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-video",
        element: "sidebar-location:video",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_video"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-zip",
        element: "sidebar-location:zip",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_zip"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-exe",
        element: "sidebar-location:exe",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_exe"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    KeyControl {
        key: "sidebar-location-folder",
        element: "sidebar-location:folder",
        indexed: false,
        role: Role::TreeItem,
        messages: &["filter_folder"],
        named_by: "",
        requirement: "UI-01, QRY-06",
    },
    // 回收站 is a location beside the file-type rows, not a filter category.
    KeyControl {
        key: "sidebar-recycle-bin",
        element: "sidebar-recycle-bin",
        indexed: false,
        role: Role::TreeItem,
        messages: &["sidebar_recycle_bin"],
        named_by: "",
        requirement: "UI-01, DSV-08",
    },
    // ---- search band: the query box (UI-03) ----------------------------------------
    // The match flags live in the 匹配 dropdown, not in the search band: their keys are promised by the command model
    // ([`crate::shell::command_groups::GroupCommand::shortcut`]) and their hints are drawn by the popup from
    // the binding, which is what keeps a menu label and a keystroke on one definition.
    KeyControl {
        key: "search-input",
        element: "search-input",
        indexed: false,
        role: Role::SearchInput,
        messages: &["search_box_label"],
        named_by: "",
        requirement: "UI-01, UI-03",
    },
    KeyControl {
        key: "search-clear",
        element: "search-clear",
        indexed: false,
        role: Role::Button,
        messages: &["query_clear"],
        named_by: "",
        requirement: "UI-01, UI-03",
    },
    // ---- title-bar menu bar: the six top-level menus (UI-01, UI-03) ----------------
    // Each trigger is a `Button` carrying the element id the view builds, so `element` is exactly what
    // GPUI sees, and `Role::Button` is exactly what a `Button` reports. The popup they open and the
    // items inside it belong to the component, which sets their roles and reads their key hints from
    // the keymap.
    KeyControl {
        key: "toolbar-menu-file",
        element: "toolbar-menu:file",
        indexed: false,
        role: Role::Button,
        messages: &["menu_file"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "toolbar-menu-edit",
        element: "toolbar-menu:edit",
        indexed: false,
        role: Role::Button,
        messages: &["menu_edit"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "toolbar-menu-view",
        element: "toolbar-menu:view",
        indexed: false,
        role: Role::Button,
        messages: &["menu_view"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "toolbar-menu-search",
        element: "toolbar-menu:search",
        indexed: false,
        role: Role::Button,
        messages: &["menu_search"],
        named_by: "",
        requirement: "UI-01",
    },
    KeyControl {
        key: "toolbar-menu-tools",
        element: "toolbar-menu:tools",
        indexed: false,
        role: Role::Button,
        messages: &["menu_tools"],
        named_by: "",
        requirement: "UI-01, SET-01",
    },
    KeyControl {
        key: "toolbar-menu-help",
        element: "toolbar-menu:help",
        indexed: false,
        role: Role::Button,
        messages: &["menu_help"],
        named_by: "",
        requirement: "UI-01",
    },
    // ---- result table: heading row, column headings, band, rows (UI-02) -----------
    // The table region: the heading row and the rows under it, together wide enough that a tight
    // window scrolls them sideways. It is the region that answers Left, Right, Home and End, so it
    // carries the keys as its own announcement and takes focus as a Tab stop (UI-03).
    KeyControl {
        key: "result-table",
        element: "result-table",
        indexed: false,
        role: Role::Group,
        messages: &["result_table_label"],
        named_by: "",
        requirement: "UI-02, UI-03",
    },
    KeyControl {
        key: "table-header",
        element: "table-header",
        indexed: false,
        role: Role::Row,
        messages: &["table_header_label"],
        named_by: "",
        requirement: "UI-02",
    },
    KeyControl {
        key: "column-name",
        element: "column-header:col_name",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_name"],
        named_by: "",
        requirement: "UI-02",
    },
    KeyControl {
        key: "column-path",
        element: "column-header:col_path",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_path"],
        named_by: "",
        requirement: "UI-02",
    },
    KeyControl {
        key: "column-size",
        element: "column-header:col_size",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_size"],
        named_by: "",
        requirement: "UI-02",
    },
    KeyControl {
        key: "column-modified",
        element: "column-header:col_modified",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_modified"],
        named_by: "",
        requirement: "UI-02",
    },
    // The Recycle Bin reuses the path and date lanes for a different value. The
    // header still has to name what the lane holds, so each value has its own
    // registered column header rather than borrowing the index table's caption.
    KeyControl {
        key: "column-original-location",
        element: "column-header:col_original_location",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_original_location"],
        named_by: "",
        requirement: "UI-02, DSV-08",
    },
    KeyControl {
        key: "column-deleted",
        element: "column-header:col_deleted",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_deleted"],
        named_by: "",
        requirement: "UI-02, DSV-08",
    },
    KeyControl {
        key: "result-band",
        element: "result-band",
        indexed: false,
        role: Role::List,
        messages: &["result_list_label"],
        named_by: "",
        requirement: "UI-02",
    },
    // A row is named by the path it opens, so no Fluent message can hold its name.
    KeyControl {
        key: "result-row",
        element: "result-row",
        indexed: true,
        role: Role::ListItem,
        messages: EMPTY,
        named_by: "join_row_path",
        requirement: "UI-02",
    },
    KeyControl {
        key: "result-placeholder",
        element: "result-placeholder",
        indexed: true,
        role: Role::ListItem,
        messages: &["row_loading", "row_not_loaded"],
        named_by: "",
        requirement: "UI-02",
    },
    KeyControl {
        key: "result-state",
        element: "result-state",
        indexed: false,
        role: Role::Status,
        messages: &[
            "empty_index_empty_title",
            "empty_no_matches",
            "recycle_bin_empty_title",
            "recycle_bin_empty_hint",
            "recycle_bin_no_matches_title",
            "recycle_bin_no_matches_hint",
        ],
        named_by: "",
        requirement: "UI-01, DSV-08",
    },
    // Each service failure is its own entry: the same element has to name the condition it reports,
    // so the title and the recovery message are registered per issue (SVC-03, SVC-04).
    KeyControl {
        key: "service-issue-not-running",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_not_running_title", "service_not_running_desc"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-not-installed",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_not_installed_title", "service_not_installed_desc"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-incompatible",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_incompatible_title", "service_incompatible_desc"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-unauthorized",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_unauthorized_title", "service_unauthorized_desc"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    // The recovery control every one of those three alerts shows under its own sentence. The failure
    // messages promise an action ("start the service, then try again"), so the promise needs a button
    // a pointer and a keyboard can both reach; the band's alert element names the condition, and this
    // one names what clicking does.
    KeyControl {
        key: "service-retry-button",
        element: "service-retry-button",
        indexed: false,
        role: Role::Button,
        messages: &["service_retry_button"],
        named_by: "",
        requirement: "SVC-03, SVC-04, UI-03",
    },
    // The explicit elevation action. It is shown only when SCM reports the installed service, so
    // its label can say what the UAC prompt is for.
    KeyControl {
        key: "service-start-button",
        element: "service-start-button",
        indexed: false,
        role: Role::Button,
        messages: &["service_start_button"],
        named_by: "",
        requirement: "SVC-03, SVC-04, UI-03",
    },
    // ---- status band: the result count (UI-01) -------------------------------------
    // The Treemap zoom badge appears only above 100%; it resets the view
    // without touching the selected node.
    KeyControl {
        key: "treemap-zoom-reset",
        element: "treemap-zoom-reset",
        indexed: false,
        role: Role::Button,
        messages: &["treemap_zoom_reset"],
        named_by: "",
        requirement: "DSV-04",
    },
    KeyControl {
        key: "status-bar",
        element: "status-bar",
        indexed: false,
        role: Role::Status,
        messages: &["status_objects_count"],
        named_by: "",
        requirement: "UI-01",
    },
    // ---- app-owned transient status -------------------------------------------------
    KeyControl {
        key: "toast",
        element: "toast",
        indexed: false,
        role: Role::Alert,
        messages: &["toast_status"],
        named_by: "",
        requirement: "UI-03",
    },
    KeyControl {
        key: "toast-dismiss",
        element: "toast-dismiss",
        indexed: false,
        role: Role::Button,
        messages: &["toast_dismiss"],
        named_by: "",
        requirement: "UI-03",
    },
    KeyControl {
        key: "recycle-bin-error",
        element: "recycle-bin-error",
        indexed: false,
        role: Role::Alert,
        messages: &[
            "recycle_bin_unavailable_title",
            "recycle_bin_unavailable_hint",
        ],
        named_by: "",
        requirement: "DSV-08",
    },
    KeyControl {
        key: "recycle-bin-retry",
        element: "recycle-bin-retry",
        indexed: false,
        role: Role::Button,
        messages: &["recycle_bin_retry"],
        named_by: "",
        requirement: "DSV-08",
    },
    KeyControl {
        key: "treemap-legend",
        element: "treemap-legend",
        indexed: true,
        role: Role::CheckBox,
        messages: EMPTY,
        named_by: "legend_accessible_label",
        requirement: "DSV-07",
    },
    // The location-wide action in the status band while the Recycle Bin holds items.
    KeyControl {
        key: "empty-recycle-bin",
        element: "empty-recycle-bin",
        indexed: false,
        role: Role::Button,
        messages: &["ctx_empty_recycle_bin"],
        named_by: "",
        requirement: "UI-01, DSV-08",
    },
];

/// Registry entry for `key`. An unknown key is a programming error, and the tests in this module walk
/// every key the view uses, so a miss fails the build instead of reaching a window.
pub fn control(key: &'static str) -> &'static KeyControl {
    KEY_CONTROLS
        .iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| {
            panic!("{key} is not in KEY_CONTROLS; register the control before the view renders it")
        })
}

/// Registry entry for the element id the view is about to build. Repeated families use this, because
/// their members are named by the model key the view assembles into the id.
pub fn control_at(element: &str) -> &'static KeyControl {
    KEY_CONTROLS
        .iter()
        .find(|entry| !entry.indexed && entry.element == element)
        .unwrap_or_else(|| {
            panic!("{element} is not a registered control; add it to KEY_CONTROLS first")
        })
}

/// The element type [`element`] and [`element_at`] hand the view over.
///
/// Observation is the registry's job, not the view's: reading a live frame wraps every control in
/// `gpui-base`'s own element type, so a call site annotated with `Stateful<Div>` would only compile
/// in one of the two builds. This alias resolves to `Stateful<Div>` when nothing observes the frame
/// and to the observation wrapper when it does, so the view styles and nests a control without
/// knowing which of the two it holds, and [`element`]/[`element_at`] stay the only places that call
/// `test_support()`.
pub type ControlElement = ObservedElement<Stateful<Div>>;

/// The element the view builds for `control`: the registered GPUI id, the registered AccessKit role
/// and the observation binding that makes both readable from a test. The call site adds the
/// accessible name and its own styling.
pub fn element(control: &KeyControl) -> ControlElement {
    div().id(control.element).role(control.role).test_support()
}

/// [`element`] for a control the view repeats inside one parent, where GPUI needs the registered id
/// paired with the row or capsule index.
pub fn element_at(control: &KeyControl, index: usize) -> ControlElement {
    div()
        .id((control.element, index))
        .role(control.role)
        .test_support()
}

/// An element the tests must see but the registry does not name: no registered role, no accessible
/// name, only the observation binding. The window root is the only one — it holds focus, so a test has
/// to read which node has it. Popups and their backdrops belong to `Popover` and `PopupMenu`.
pub fn surface(id: &'static str) -> ControlElement {
    div().id(id).test_support()
}

/// The keystrokes each control promises, in the order the view announces them. One table so a promise
/// cannot live in the view and the model at the same time (UI-03).
///
/// A control may answer several keys: the query box takes one, while the result table region is moved
/// by four. Keys are written the way [`crate::shell::commands::init_keymap`] binds them, because that is the
/// form both AccessKit and the regression test below read.
///
/// The view reads its `aria_keyshortcuts` from here, and the regression test reads the same table to
/// check [`crate::shell::commands::init_keymap`] keeps every promise. A key written as an argument at a call
/// site is invisible to that check while still reaching a screen reader, which is how `ctrl-i` ended
/// up advertised from one place and verified from another.
///
/// Only controls that own keys appear in this table. Commands listed inside a component menu name their
/// key in the menu model instead — that menu draws its hint from the binding of the Action it
/// dispatches, so one definition covers the item, the hint and the key — and the two sources are
/// unioned by the test that holds the promises to the keymap.
///
/// Keys are addressed by the control's [`element`](KeyControl::element), because that is the handle
/// the render path already holds.
pub const CONTROL_SHORTCUTS: &[(&str, &[&str])] = &[
    ("search-input", &["ctrl-f"]),
    // Advertised on the region, and bound only in `crate::shell::commands::RESULTS_CONTEXT`: the four keys
    // move the columns while the table holds focus, and move the caret while the query box does.
    ("result-table", &["left", "right", "home", "end"]),
];

/// Every keystroke `element_id` promises. Empty when that control promises none.
pub fn shortcuts_for(element_id: &str) -> &'static [&'static str] {
    CONTROL_SHORTCUTS
        .iter()
        .find(|(element, _)| *element == element_id)
        .map(|(_, shortcuts)| *shortcuts)
        .unwrap_or(&[])
}

/// Accessible name of `control` in the language `i18n` currently speaks.
pub fn name(control: &KeyControl, i18n: &I18nManager) -> String {
    i18n.get_text(control.message())
}

/// The `index`-th message `control` speaks, resolved to text: the view picks the one its state needs.
pub fn name_at(control: &KeyControl, index: usize, i18n: &I18nManager) -> String {
    i18n.get_text(control.message_at(index))
}

/// `true` when `text` hides a keyboard mnemonic inside brackets — the `文件(F)` / `打开(&O)` shape a
/// Windows menu draws as an underline while Alt is held. Written into the label it is visible text
/// that binds nothing, so the naming tests reject it: the key belongs in the keymap and in
/// `aria_keyshortcuts`, the label carries the words only.
#[cfg(test)]
pub fn label_carries_mnemonic(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if !matches!(ch, '(' | '（') {
            continue;
        }
        let inner: String = chars[index + 1..]
            .iter()
            .take_while(|c| **c != ')' && **c != '）')
            .filter(|c| **c != '&' && !c.is_whitespace())
            .collect();
        let mut letters = inner.chars();
        if let (Some(letter), None) = (letters.next(), letters.next()) {
            if letter.is_ascii_alphabetic() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;
    use crate::search::filters::FilterCategory;
    use crate::service::service_issue::ServiceIssue;
    use crate::shell::command_groups::{all_commands, menu_groups};
    use gpui_kit::gpui::Keystroke;
    use std::collections::HashSet;

    /// The sources that build the window. `accessibility.rs` is deliberately excluded: its table holds
    /// every registered id, so reading it back would prove nothing about the render path.
    ///
    /// These are a static proxy for a render-time invariant: the app has no headless window yet, so
    /// "the view asks for this control" is answered by reading the view's text rather than by
    /// rendering it. `gpui-kit`'s `test-support` feature carries the headless harness that would
    /// replace these scans with real assertions on the accessibility tree.
    const VIEW_SOURCES: &[&str] = &[
        include_str!("../view.rs"),
        include_str!("../search/bar_view.rs"),
        include_str!("sidebar_view.rs"),
        include_str!("status_bar_view.rs"),
        include_str!("../table/results_view.rs"),
        include_str!("../app.rs"),
        include_str!("commands.rs"),
        include_str!("command_groups.rs"),
        include_str!("../service/config_submission.rs"),
        include_str!("../search/filters.rs"),
        include_str!("../service/ipc_tasks.rs"),
        include_str!("menu_actions.rs"),
        include_str!("nav_history.rs"),
        include_str!("toast.rs"),
        include_str!("../search/query_orchestration.rs"),
        include_str!("status_bar.rs"),
        include_str!("../table/model.rs"),
    ];

    /// The sources that draw a named control. The consumption checks below run over each of them
    /// separately, so moving a surface into its own file cannot move it outside the registry's reach.
    const RENDERED_SOURCES: &[&str] = &[
        include_str!("../view.rs"),
        include_str!("../search/bar_view.rs"),
        include_str!("sidebar_view.rs"),
        include_str!("status_bar_view.rs"),
        include_str!("toast.rs"),
        include_str!("../table/results_view.rs"),
    ];

    fn zh() -> I18nManager {
        I18nManager::new("zh-CN")
    }

    fn en() -> I18nManager {
        I18nManager::new("en-US")
    }

    #[test]
    fn every_control_is_registered_under_a_unique_key() {
        let mut keys: HashSet<&str> = HashSet::new();
        for entry in KEY_CONTROLS {
            assert!(keys.insert(entry.key), "{} is registered twice", entry.key);
            assert!(
                !entry.element.is_empty(),
                "{} registers no element id for GPUI to observe",
                entry.key
            );
            assert!(
                !entry.requirement.is_empty(),
                "{} does not say which requirement it serves",
                entry.key
            );
        }
    }

    /// `Unknown` is what an element without a role reports, so registering one would be exactly the
    /// silent failure NFR-11 exists to prevent. `TextRun` and `Label` are just as empty for a control.
    #[test]
    fn no_registered_control_reports_an_uninformative_role() {
        for entry in KEY_CONTROLS {
            assert_ne!(
                entry.role,
                Role::Unknown,
                "{} would be announced as unclassified content",
                entry.key
            );
            assert_ne!(
                entry.role,
                Role::TextRun,
                "{} would be announced as bare text",
                entry.key
            );
            assert_ne!(
                entry.role,
                Role::Label,
                "{} would be announced as a caption with no control",
                entry.key
            );
        }
    }

    /// The role each kind of control must carry, stated here independently of the table so a wrong
    /// entry is caught rather than confirmed: commands are actionable, containers group, live regions
    /// announce, and the query box takes text.
    #[test]
    fn registered_roles_match_each_controls_semantics() {
        let expectations: &[(&str, Role)] = &[
            ("search-input", Role::SearchInput),
            ("sidebar-locations", Role::Heading),
            ("sidebar-location-all", Role::TreeItem),
            ("sidebar-location-doc", Role::TreeItem),
            ("sidebar-location-image", Role::TreeItem),
            ("sidebar-location-audio", Role::TreeItem),
            ("sidebar-location-video", Role::TreeItem),
            ("sidebar-location-zip", Role::TreeItem),
            ("sidebar-location-exe", Role::TreeItem),
            ("sidebar-location-folder", Role::TreeItem),
            ("sidebar-recycle-bin", Role::TreeItem),
            ("toolbar-menu-file", Role::Button),
            ("toolbar-menu-edit", Role::Button),
            ("toolbar-menu-view", Role::Button),
            ("toolbar-menu-search", Role::Button),
            ("toolbar-menu-tools", Role::Button),
            ("toolbar-menu-help", Role::Button),
            ("table-header", Role::Row),
            ("column-name", Role::ColumnHeader),
            ("column-path", Role::ColumnHeader),
            ("column-size", Role::ColumnHeader),
            ("column-modified", Role::ColumnHeader),
            ("result-band", Role::List),
            ("result-row", Role::ListItem),
            ("result-placeholder", Role::ListItem),
            ("result-state", Role::Status),
            ("status-bar", Role::Status),
            ("toast", Role::Alert),
            ("toast-dismiss", Role::Button),
            ("recycle-bin-error", Role::Alert),
            ("recycle-bin-retry", Role::Button),
            ("empty-recycle-bin", Role::Button),
            ("service-issue-not-running", Role::Alert),
        ];
        for (key, expected) in expectations {
            assert_eq!(
                control(key).role,
                *expected,
                "{key} must report the role its interaction implies"
            );
        }
    }

    /// The controls SPEC NFR-11 and UI-01/02/03 name by requirement. This list is the coverage
    /// statement: a control that stops being registered fails here, not in a screen reader.
    #[test]
    fn the_registry_covers_the_controls_the_spec_names() {
        let required = [
            "search-input",
            "sidebar-locations",
            "sidebar-location-all",
            "sidebar-location-folder",
            "toolbar-menu-file",
            "toolbar-menu-edit",
            "toolbar-menu-view",
            "toolbar-menu-search",
            "toolbar-menu-tools",
            "toolbar-menu-help",
            "table-header",
            "column-name",
            "column-path",
            "column-size",
            "column-modified",
            "result-band",
            "result-row",
            "result-placeholder",
            "result-state",
            "status-bar",
        ];
        for key in required {
            let entry = control(key);
            assert!(
                !entry.messages.is_empty() || !entry.named_by.is_empty(),
                "{key} registers no accessible name at all"
            );
        }

        // Every dropdown the toolbar owns is a registered trigger, named by the group it opens.
        for spec in menu_groups() {
            let element = format!("toolbar-menu:{}", spec.group.key());
            let entry = KEY_CONTROLS
                .iter()
                .find(|entry| entry.element == element)
                .unwrap_or_else(|| panic!("{element} is not a registered control"));
            assert_eq!(entry.role, Role::Button);
            assert_eq!(entry.message(), spec.group.label_key());
        }
        for column in ["col_name", "col_path", "col_size", "col_modified"] {
            assert_eq!(
                control_at(&format!("column-header:{column}")).message(),
                column
            );
        }
    }

    /// A name is only accessible if it resolves. Every registered message has to reach non-blank text
    /// in zh-CN and in en-US (UI-07), a control with no message must say what names it instead, and no
    /// label may carry a mnemonic character: `(F)` or `(&O)` in the text is a hint the user reads and
    /// nothing binds (Design Guides: Interface language, platform fit).
    #[test]
    fn every_registered_name_resolves_to_text_in_both_locales() {
        let zh = zh();
        let en = en();
        let mut checked = 0usize;
        for entry in KEY_CONTROLS {
            for key in entry.messages {
                checked += 1;
                for (locale, i18n) in [("zh-CN", &zh), ("en-US", &en)] {
                    let text = i18n.try_text(key).unwrap_or_else(|| {
                        panic!(
                            "{} names itself with {key}, which has no {locale} message",
                            entry.key
                        )
                    });
                    assert!(
                        !text.trim().is_empty(),
                        "{} would announce nothing in {locale}: {key} is blank",
                        entry.key
                    );
                    assert!(
                        !label_carries_mnemonic(&text),
                        "{key} carries a mnemonic in its {locale} label: {text}"
                    );
                }
            }
            if entry.messages.is_empty() {
                assert!(
                    !entry.named_by.is_empty(),
                    "{} names itself with neither a message nor a source",
                    entry.key
                );
                assert!(
                    VIEW_SOURCES
                        .iter()
                        .any(|source| source.contains(entry.named_by)),
                    "{} points at {}, which the view no longer defines",
                    entry.key,
                    entry.named_by
                );
            }
        }
        assert!(
            checked >= 40,
            "the registry resolves only {checked} names; the window speaks more than that"
        );
    }

    /// The anti-empty-promise check: every registered control must be *requested* by the render path,
    /// either by its registry key or by the element id it builds. A member of a repeated family counts
    /// as requested only when the view builds that family (`element_id(prefix, model_key)`) and the
    /// model really names that member.
    #[test]
    fn every_registered_control_is_requested_by_the_render_path() {
        let references = |needle: &str| VIEW_SOURCES.iter().any(|source| source.contains(needle));

        for entry in KEY_CONTROLS {
            let by_key = references(entry.key);
            let by_element = references(entry.element);
            // A repeated family counts only when the view assembles that family's id *and* the member
            // exists as its own string literal. Matching the member as a bare substring is not a
            // check: "all" is inside "small", "install" and "allow", so any short member name would
            // be reported as requested no matter what the view did.
            let by_family = match entry.element.split_once(':') {
                Some((family, member)) => {
                    references(&format!("element_id(\"{family}\""))
                        && references(&format!("\"{member}\""))
                }
                None => false,
            };
            assert!(
                by_key || by_element || by_family,
                "{} registers element {}, which neither the view nor its models build",
                entry.key,
                entry.element
            );
        }
    }

    /// The view may not name a role, an accessible label or an observation binding inline. The first
    /// two are how a control ends up with a name the registry never promised, or with none at all; the
    /// third is how the window stops compiling, because observing an element changes its type. The
    /// view instead asks the registry for its element, so the counts below are the consumption proof:
    /// every control the view builds carries exactly one registered role, one accessible name and one
    /// observation binding, and the registry supplies all three. Each rendered source is held to this
    /// on its own, so a surface that moves into a new file cannot take its controls outside the check.
    #[test]
    fn the_view_takes_every_role_and_accessible_name_from_the_registry() {
        let registered_indexed = KEY_CONTROLS.iter().filter(|entry| entry.indexed).count();
        let mut named_controls = 0usize;
        let mut surfaces = 0usize;
        let mut indexed_controls = 0usize;

        for source in RENDERED_SOURCES {
            assert!(
                !source.contains("Role::"),
                "a rendered source names a role inline; register the control in KEY_CONTROLS instead"
            );
            assert!(
                !source.contains(".role("),
                "a rendered source sets a role outside the registry; build the element with \
                 accessibility::element(control)"
            );
            assert!(
                !source.contains(".aria_label(self.i18n.get_text(\""),
                "a rendered source names a control with an inline message id; register it instead"
            );
            assert!(
                !source.contains(".test_support("),
                "a rendered source observes an element by hand, so it also decides the element's \
                 type; the registry binds observation for everything it builds"
            );

            let fixed_sites = source.matches("accessibility::element(").count();
            let indexed_sites = source.matches("accessibility::element_at(").count();
            let label_sites = source.matches(".aria_label(").count();

            assert_eq!(
                label_sites,
                fixed_sites + indexed_sites,
                "a source builds {} controls but names only {label_sites}",
                fixed_sites + indexed_sites
            );

            indexed_controls += indexed_sites;
            named_controls += fixed_sites + indexed_sites;
            surfaces += source.matches("accessibility::surface(").count();
        }

        assert_eq!(
            indexed_controls, registered_indexed,
            "the registry indexes {registered_indexed} controls, but the rendered sources build \
             {indexed_controls} with an element index"
        );

        // One element carries no registered role and exists only to be seen: the window root, which
        // makes focus observable. The dropdowns and the row menu are Popover and ContextMenu now, so
        // they own their own dismissal surface and no hand-drawn backdrop is left to count. The
        // settings dialog is drawn by the `Dialog` component, which owns its own surface and focus.
        assert_eq!(
            surfaces, 1,
            "the view built {surfaces} unnamed surfaces; only the window root may exist to be \
             observed without a role"
        );
        assert!(
            named_controls >= 12,
            "the view builds only {named_controls} named controls; a shell this size registers more \
             than that"
        );
    }

    /// Each category entry registers exactly its own wording, so a stray message cannot hide in the
    /// family and a renamed category cannot leave an orphan behind.
    #[test]
    fn each_sidebar_category_speaks_its_own_wording() {
        let categories = [
            FilterCategory::All,
            FilterCategory::Document,
            FilterCategory::Picture,
            FilterCategory::Audio,
            FilterCategory::Video,
            FilterCategory::Compressed,
            FilterCategory::Executable,
            FilterCategory::Folder,
        ];
        for category in categories {
            let entry = control_at(&format!("sidebar-location:{}", category.key()));
            assert!(
                entry.messages.contains(&category.label_key()),
                "the sidebar does not register the {} category",
                category.key()
            );
        }
        // Each entry speaks exactly its own category's wording, so a stray message cannot hide in the
        // family and a renamed category cannot leave an orphan behind.
        for category in categories {
            let entry = control_at(&format!("sidebar-location:{}", category.key()));
            assert_eq!(
                entry.messages,
                [category.label_key()].as_slice(),
                "the {} entry registers {:?}",
                category.key(),
                entry.messages
            );
        }
    }

    /// The window reports one element for every service failure. Each entry
    /// must carry its own distinct pair of messages — title first, recovery
    /// second — so one condition can never hide behind another's wording
    /// (SVC-03, SVC-04).
    #[test]
    fn each_service_failure_registers_its_own_condition_and_recovery() {
        let issues = [
            (ServiceIssue::NotRunning, "service-issue-not-running"),
            (ServiceIssue::NotInstalled, "service-issue-not-installed"),
            (
                ServiceIssue::ProtocolIncompatible,
                "service-issue-incompatible",
            ),
            (ServiceIssue::Unauthorized, "service-issue-unauthorized"),
        ];
        let mut pairs = HashSet::new();
        for (_issue, key) in issues {
            let entry = control(key);
            assert_eq!(entry.element, "service-issue");
            assert_eq!(
                entry.role,
                Role::Alert,
                "{key} must interrupt, not sit in the band as status text"
            );
            assert_eq!(
                entry.messages.len(),
                2,
                "{key} needs a title and a recovery text"
            );
            assert!(
                pairs.insert((entry.message_at(0), entry.message_at(1))),
                "{key} reuses another failure's message pair"
            );
        }
    }

    /// The other half of the consumption check: a control a rendered source asks for by key must be
    /// registered, or the window would panic while rendering. The service issues and the submission
    /// endings are requested through their own model helpers, so their tests above hold that mapping
    /// instead. Both sources that draw named controls are scanned, so a surface that moves into its
    /// own file stays inside the check.
    #[test]
    fn every_control_the_view_asks_for_by_key_is_registered() {
        let marker = "accessibility::control(\"";
        let mut requested = 0usize;

        for source in RENDERED_SOURCES {
            let mut rest = *source;
            while let Some(start) = rest.find(marker) {
                rest = &rest[start + marker.len()..];
                let Some(end) = rest.find('"') else { break };
                let key = &rest[..end];
                assert!(
                    KEY_CONTROLS.iter().any(|entry| entry.key == key),
                    "a rendered source draws {key}, which KEY_CONTROLS does not describe"
                );
                requested += 1;
                rest = &rest[end..];
            }
        }

        assert!(
            requested >= 12,
            "the rendered sources ask the registry for {requested} controls by key; the main window \
             has more"
        );

        // Repeated families are requested by the id the view assembles, so the families the registry
        // claims must be the ones the view really assembles and looks up.
        let mut families: Vec<&str> = KEY_CONTROLS
            .iter()
            .filter(|entry| entry.element.contains(':'))
            .map(|entry| entry.element.split(':').next().unwrap_or_default())
            .collect();
        families.sort_unstable();
        families.dedup();
        assert!(
            !families.is_empty(),
            "the registry describes no repeated family, yet the view assembles ids with an index"
        );
        // The loop below is the check: every family the registry claims must be assembled and looked
        // up by a rendered source. Naming the families a second time here would only freeze today's
        // shell. The scan ignores whitespace because rustfmt wraps a long `element_id(` call across
        // lines, and a wrapped argument list is not the view forgetting to build a control.
        let assembled: Vec<String> = RENDERED_SOURCES
            .iter()
            .map(|source| source.chars().filter(|c| !c.is_whitespace()).collect())
            .collect();
        for family in families {
            let needle: String = format!("control_at(&element_id(\"{family}\"")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            assert!(
                assembled.iter().any(|source| source.contains(&needle)),
                "no rendered source builds the {family} family through the registry"
            );
        }
    }

    /// A shortcut a control advertises through AccessKit is a promise a screen reader reads out, so the
    /// keymap has to dispatch it. The promises come from [`CONTROL_SHORTCUTS`] and from the menu model,
    /// the two places a key is defined, and `init_keymap` is the only place a key can be kept. A menu
    /// whose mnemonic bound nothing would fail here, not on a manual pass.
    #[test]
    fn every_shortcut_the_controls_advertise_is_bound_in_the_keymap() {
        let advertised = advertised_shortcuts();

        // SPEC UI-03 names these keys, so they must still be on a control; without them the check
        // below would pass while the window promises nothing.
        for required in ["ctrl-f", "ctrl-i", "ctrl-b", "ctrl-u", "ctrl-r", "alt-p"] {
            assert!(
                advertised.contains(&required),
                "{required} is in this list but no control announces it any more"
            );
        }
        // A top-level menu nobody can reach by keyboard is a pointer-only path (UI-03).
        for spec in menu_groups() {
            assert!(
                !spec.group.keys().is_empty(),
                "{} would be reachable only by pointer",
                spec.group.label_key()
            );
        }

        // The keymap table is the one place a key can be kept, so an announced shortcut that no
        // entry binds is a promise the window cannot keep.
        let bindings = crate::shell::commands::key_bindings();
        for shortcut in advertised {
            assert!(
                bindings.iter().any(|entry| entry.shortcut == shortcut),
                "the window advertises {shortcut} on a control, but init_keymap never binds it"
            );
        }
    }

    /// The table is the promise, so a row has to describe a real control and reach a screen reader.
    /// A key the view passes as an argument would be advertised without being registered here, which
    /// is how one shortcut ended up checked against the source and announced from a call site at the
    /// same time (UI-03).
    #[test]
    fn each_promised_shortcut_belongs_to_one_registered_control() {
        for (element, shortcuts) in CONTROL_SHORTCUTS {
            let entry = KEY_CONTROLS
                .iter()
                .find(|entry| entry.element == *element)
                .unwrap_or_else(|| {
                    panic!("{element} promises {shortcuts:?}, which KEY_CONTROLS does not describe")
                });
            assert!(
                !entry.indexed,
                "{} is a repeated family, so it cannot carry one shared key",
                entry.key
            );
            assert!(
                !shortcuts.is_empty(),
                "{element} holds a row in the shortcut table without promising a key"
            );
            assert_eq!(
                shortcuts_for(element),
                *shortcuts,
                "{} does not resolve its own row",
                entry.key
            );
            for shortcut in *shortcuts {
                Keystroke::parse(shortcut)
                    .unwrap_or_else(|_| panic!("{} advertises an unparsable key", entry.key));
            }
        }
        for (element, _) in CONTROL_SHORTCUTS {
            assert_eq!(
                CONTROL_SHORTCUTS
                    .iter()
                    .filter(|(other, _)| other == element)
                    .count(),
                1,
                "{element} promises its keys twice"
            );
        }

        // The band is named by its model, so its keys must not also be written into this table.
        for spec in menu_groups() {
            let element = format!("toolbar-menu:{}", spec.group.key());
            assert!(
                shortcuts_for(&element).is_empty(),
                "{element} would hold a key in the model and the registry at once"
            );
        }

        // The view announces the table instead of repeating keys at its call sites. Two places may
        // put a shortcut in front of AccessKit: this helper, and the dropdown reading its own model.
        // Every rendered source is held to that, so a new surface cannot advertise a key the table
        // never promised.
        let asks_for_keys = RENDERED_SOURCES
            .iter()
            .any(|source| source.contains("accessibility::shortcuts_for("));
        assert!(
            asks_for_keys,
            "no rendered source asks the registry for the key it advertises"
        );
        let advertised_inline: usize = RENDERED_SOURCES
            .iter()
            .map(|source| source.matches("aria_keyshortcuts(").count())
            .sum();
        assert_eq!(
            advertised_inline, 1,
            "the rendered sources promise a key outside the registry table and the menu model"
        );
    }

    /// Every keystroke a control puts in front of the user: the registry table, the mnemonics each
    /// dropdown's model carries, and the keys the commands list beside their own labels. These are the
    /// three definitions, so the set is their union and nothing else.
    fn advertised_shortcuts() -> Vec<&'static str> {
        let mut found: Vec<&'static str> = CONTROL_SHORTCUTS
            .iter()
            .flat_map(|(_, shortcuts)| shortcuts.iter().copied())
            .chain(
                menu_groups()
                    .iter()
                    .flat_map(|spec| spec.group.keys().iter().copied()),
            )
            .chain(all_commands().into_iter().filter_map(|cmd| cmd.shortcut()))
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// What the view calls: `name` reads the registered message in the language the window speaks, and
    /// `name_at` reads the one its current state selects. Switching language must rename the control,
    /// not its identity (UI-07).
    #[test]
    fn names_are_read_from_the_registered_messages_in_the_active_language() {
        let zh = zh();
        let en = en();
        assert_eq!(
            name(control("search-input"), &zh),
            zh.try_text("search_box_label").unwrap(),
            "the query box name must come from its registered message"
        );
        assert_ne!(
            name(control("search-input"), &zh),
            name(control("search-input"), &en),
            "switching language must rename the control, not its identity"
        );
        assert_eq!(
            name(control("toolbar-menu-search"), &en),
            en.try_text("menu_search").unwrap(),
            "a menu trigger is named by its registered message, not by its glyph"
        );
        assert_ne!(
            name_at(control("result-state"), 0, &zh),
            name_at(control("result-state"), 1, &zh),
            "the idle prompt and the no-match state must not announce the same thing"
        );
    }
}

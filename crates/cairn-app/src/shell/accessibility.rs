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
//! The regression tests at the bottom run in the ordinary `cargo test -p cairn-app` build and hold
//! the table together (unique keys, informative roles, no missing or blank translation, every
//! registered message resolvable). Observing the roles inside a live window — the half that proves
//! the window reports what the table claims — must be checked in a live window; the app has no
//! headless UI harness.

use crate::shell::i18n::I18nManager;
use gpui_kit::base::{ObservedElement, TestSupportExt};
use gpui_kit::gpui::{
    Div, InteractiveElement, Role, SharedString, Stateful, StatefulInteractiveElement, div,
};

/// One key control as assistive technology must see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyControl {
    /// Registry identity, unique inside [`KEY_CONTROLS`]. The state variants of one control are
    /// separate entries, because each reports a different role.
    pub key: &'static str,
    /// Element id the view gives the control, exactly as GPUI sees it. Members of a repeated family
    /// record the id the view assembles, and state variants of one control share an id.
    pub element: &'static str,
    /// `true` when the view pairs [`element`](KeyControl::element) with a row or source index.
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

/// Every key control of the main window. Adding a control means adding a row here; the tests below
/// hold its key, role, element id and messages consistent.
pub const KEY_CONTROLS: &[KeyControl] = &[
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
    // The section heading is the view's own element. Custom sidebar rows report `Role::TreeItem`
    // on their focus wrapper, and the registry names each one by the model key assembled into its
    // element id so a category cannot gain a sidebar row without a message.
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
    KeyControl {
        key: "sidebar-recycle-bin",
        element: "sidebar-recycle-bin",
        indexed: false,
        role: Role::TreeItem,
        messages: &["sidebar_recycle_bin"],
        named_by: "",
        requirement: "UI-01, DSV-08",
    },
    // The folder source section exists only while a folder source is registered. Its rows are a
    // repeated family keyed by the service's source id, and the manager entry ends the section.
    KeyControl {
        key: "sidebar-folder-sources",
        element: "sidebar-folder-sources",
        indexed: false,
        role: Role::Heading,
        messages: &["sidebar_folder_sources"],
        named_by: "",
        requirement: "UI-01, IDX-11",
    },
    KeyControl {
        key: "sidebar-folder-source",
        element: "sidebar-folder-source",
        indexed: true,
        role: Role::TreeItem,
        messages: EMPTY,
        named_by: "source.root_path",
        requirement: "UI-01, IDX-11",
    },
    KeyControl {
        key: "sidebar-manage-folder-sources",
        element: "sidebar-manage-folder-sources",
        indexed: false,
        role: Role::Button,
        messages: &["sidebar_manage_folder_sources"],
        named_by: "",
        requirement: "UI-01, IDX-11",
    },
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
        key: "toolbar-menu-index",
        element: "toolbar-menu:index",
        indexed: false,
        role: Role::Button,
        messages: &["menu_index"],
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
    KeyControl {
        key: "column-created",
        element: "column-header:col_created",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_created"],
        named_by: "",
        requirement: "UI-02, UI-11",
    },
    KeyControl {
        key: "column-length",
        element: "column-header:col_length",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_length"],
        named_by: "",
        requirement: "UI-02, IDX-12",
    },
    KeyControl {
        key: "column-author",
        element: "column-header:col_author",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_author"],
        named_by: "",
        requirement: "UI-02, IDX-12",
    },
    KeyControl {
        key: "column-tags",
        element: "column-header:col_tags",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_tags"],
        named_by: "",
        requirement: "UI-02, IDX-12",
    },
    KeyControl {
        key: "column-dimensions",
        element: "column-header:col_dimensions",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_dimensions"],
        named_by: "",
        requirement: "UI-02, IDX-12",
    },
    KeyControl {
        key: "column-framerate",
        element: "column-header:col_framerate",
        indexed: false,
        role: Role::ColumnHeader,
        messages: &["col_framerate"],
        named_by: "",
        requirement: "UI-02, IDX-12",
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
            "search_cancelled_title",
            "empty_content_scope_line",
            "empty_content_loading_title",
            "empty_content_unreadable_title",
            "empty_content_disabled_title",
            "empty_content_backfilling_title",
            "empty_content_error_title",
            "query_too_complex_title",
            "recycle_bin_empty_title",
            "recycle_bin_no_matches_title",
        ],
        named_by: "",
        requirement: "UI-01, DSV-08, UI-05",
    },
    // The name cell of the row being renamed becomes this input; the row keeps its identity and
    // the editor keeps the component's IME and text-selection behavior.
    KeyControl {
        key: "rename-input",
        element: "rename-input",
        indexed: false,
        role: Role::TextInput,
        messages: &[
            "rename_label",
            "rename_error_empty",
            "rename_error_unchanged",
            "rename_error_invalid_char",
            "rename_error_trailing_dot_or_space",
            "rename_error_reserved",
            "rename_error_too_long",
            "rename_error_target_exists",
        ],
        named_by: "",
        requirement: "UI-05",
    },
    // A content query that cannot run offers one state-specific recovery: turn the feature on,
    // rebuild the index, or re-read the status. Only one label is live at a time.
    KeyControl {
        key: "content-search-action",
        element: "content-search-action",
        indexed: false,
        role: Role::Button,
        messages: &[
            "index_enable_action",
            "menu_content_search_retry",
            "index_status_retry",
        ],
        named_by: "",
        requirement: "CNT-01, UI-01",
    },
    // Each service failure is its own entry: the same element has to name the condition it reports,
    // and that one sentence carries the recovery direction (SVC-03, SVC-04).
    KeyControl {
        key: "service-issue-not-running",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_not_running_title"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-timeout",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_timeout_title"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-not-installed",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_not_installed_title"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-incompatible",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_incompatible_title"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    KeyControl {
        key: "service-issue-unauthorized",
        element: "service-issue",
        indexed: false,
        role: Role::Alert,
        messages: &["service_unauthorized_title"],
        named_by: "",
        requirement: "SVC-03, SVC-04",
    },
    // Inline recovery control shown beside recoverable service alerts (`not_running` / `timeout`),
    // paired with the band's alert element so pointer and keyboard can both trigger a retry.
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
    KeyControl {
        key: "status-bar",
        element: "status-bar",
        indexed: false,
        role: Role::Status,
        messages: &["status_objects_count"],
        named_by: "",
        requirement: "UI-01",
    },
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
        key: "toast-info",
        element: "toast-info",
        indexed: false,
        role: Role::Status,
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
        messages: &["recycle_bin_unavailable_title"],
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
];

/// Registry entry for `key`. An unknown key is a programming error: it panics here rather than
/// reaching a window unnamed.
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
/// knowing which of the two it holds.
pub type ControlElement = ObservedElement<Stateful<Div>>;

/// The element the view builds for `control`: the registered GPUI id, the registered AccessKit role
/// and the observation binding that makes both readable from a test. The call site adds the
/// accessible name and its own styling.
pub fn element(control: &KeyControl) -> ControlElement {
    div().id(control.element).role(control.role).test_support()
}

/// One modal title: a level-one heading named by the title the caller just localized. It is not a
/// control, so it has no registry row; keeping its role here still leaves one place that decides it.
pub(crate) fn heading(element: Stateful<Div>, label: impl Into<SharedString>) -> Stateful<Div> {
    element.role(Role::Heading).aria_level(1).aria_label(label)
}

/// The accessible name for a component-owned control (an Input, a picker) whose role and element
/// come from the component; this resolves the words from the registry.
pub fn component_name(control: &KeyControl, i18n: &I18nManager) -> String {
    name(control, i18n)
}

/// [`element`] for a control the view repeats inside one parent, where GPUI needs the registered id
/// paired with the row or source index.
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
/// check [`crate::shell::commands::init_keymap`] keeps every promise.
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
    ("sidebar-toggle", &["ctrl-\\"]),
    // Advertised on the region, and bound only in `crate::shell::commands::RESULTS_CONTEXT`: the four keys
    // move the columns while the table holds focus, and move the caret while the query box does.
    ("result-table", &["left", "right", "home", "end"]),
];

/// Every keystroke `element_id` promises. Empty when that control promises none.
pub fn shortcuts_for(element_id: &str) -> &'static [&'static str] {
    CONTROL_SHORTCUTS
        .iter()
        .find(|(element, _)| *element == element_id)
        .map_or(&[][..], |(_, shortcuts)| *shortcuts)
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
        if let (Some(letter), None) = (letters.next(), letters.next())
            && letter.is_ascii_alphabetic()
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::filters::FilterCategory;
    use crate::service::service_issue::ServiceIssue;
    use crate::shell::command_groups::{GroupCommand, all_commands, menu_groups};
    use gpui_kit::gpui::Keystroke;
    use std::collections::HashSet;

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

    /// Semantic roles must stay distinct across live-region priority levels and interactive surfaces.
    #[test]
    fn key_control_roles_match_interaction_semantics() {
        let expectations = [
            ("toast", Role::Alert),
            ("toast-info", Role::Status),
            ("search-input", Role::SearchInput),
            ("rename-input", Role::TextInput),
            ("result-band", Role::List),
            ("result-row", Role::ListItem),
            ("column-name", Role::ColumnHeader),
            ("table-header", Role::Row),
            ("status-bar", Role::Status),
        ];
        for (key, expected) in expectations {
            assert_eq!(
                control(key).role,
                expected,
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
            "toolbar-menu-index",
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

        // Every top-bar menu dropdown is a registered trigger, named by the group it opens.
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
            }
        }
        assert!(
            checked >= 40,
            "the registry resolves only {checked} names; the window speaks more than that"
        );
    }

    /// Structured render contract: every registered control builds a valid `ControlElement` via the
    /// registry constructors, preserving indexed vs fixed identity conventions.
    #[test]
    fn every_registered_control_constructs_structured_element_and_name() {
        let zh = zh();
        let en = en();
        for entry in KEY_CONTROLS {
            let _built = if entry.indexed {
                element_at(entry, 0)
            } else {
                element(entry)
            };
            if entry.messages.len() == 1 {
                assert!(
                    !name(entry, &zh).is_empty(),
                    "empty zh name for {}",
                    entry.key
                );
                assert!(
                    !name(entry, &en).is_empty(),
                    "empty en name for {}",
                    entry.key
                );
                assert!(
                    !component_name(entry, &zh).is_empty(),
                    "empty component name for {}",
                    entry.key
                );
            } else if entry.messages.len() > 1 {
                for index in 0..entry.messages.len() {
                    assert!(
                        !name_at(entry, index, &zh).is_empty(),
                        "empty zh name[{index}] for {}",
                        entry.key
                    );
                    assert!(
                        !name_at(entry, index, &en).is_empty(),
                        "empty en name[{index}] for {}",
                        entry.key
                    );
                }
            }
        }
        assert!(KEY_CONTROLS.iter().filter(|entry| !entry.indexed).count() >= 12);
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
    /// carries its own single sentence, which names the condition and the
    /// recovery direction, so one condition can never hide behind another's
    /// wording (SVC-03, SVC-04).
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
            (ServiceIssue::Timeout, "service-issue-timeout"),
        ];
        let mut messages = HashSet::new();
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
                1,
                "{key} carries one sentence, not a title and a description"
            );
            assert!(
                messages.insert(entry.message_at(0)),
                "{key} reuses another failure's message"
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
        for required in ["ctrl-f", "ctrl-i", "ctrl-b", "ctrl-u", "ctrl-r", "ctrl-\\"] {
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
    /// A key the view passes as an argument would be advertised without being registered here (UI-03).
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
    }

    /// Every keystroke a control puts in front of the user: the registry table, the mnemonics each
    /// dropdown's model carries, and the keys the commands list beside their own labels. These are the
    /// three definitions, so the set is their union and nothing else.
    fn advertised_shortcuts() -> Vec<&'static str> {
        let mut found: Vec<&'static str> = CONTROL_SHORTCUTS
            .iter()
            .flat_map(|(_, shortcuts)| shortcuts.iter().copied())
            .chain(menu_groups().iter().flat_map(|spec| spec.group.keys()))
            .chain(
                all_commands()
                    .into_iter()
                    .filter_map(GroupCommand::shortcut),
            )
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

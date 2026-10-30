// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Keyboard command vocabulary for the main window (SPEC UI-03).
//!
//! Each desktop command is modelled once as a GPUI Action and bound once to its key here. The
//! window root owns the `on_action` handlers, so a menu item, a capsule button and the keyboard all
//! reach the same owner method. Handlers therefore fire for *any* focused descendant, not only
//! when the search box happens to hold focus.

use gpui_kit::gpui::*;

/// Key context carried by the main window root. Bindings are scoped to it so an overlay can
/// override a key later without editing this table.
pub const WINDOW_CONTEXT: &str = "Cairn";

/// Key context carried by the result table region, and present only while that region holds focus.
///
/// The keys bound here belong to the table alone. `left`, `right`, `home` and `end` are the query
/// box's text-editing keys — the box is where the window's focus normally sits — so a window-scoped
/// binding would take them away from the caret: a keystroke resolves to an Action before it reaches
/// the focused element's own key handler, and an Action handler stops propagation by default.
/// Scoping them to the region is what lets the box keep them while the table still answers the
/// keyboard (SPEC UI-03; Design Guides *Interaction states*: "Match familiar desktop shortcuts" and
/// the Accessibility checklist's "every action is reachable and operable by keyboard").
pub const RESULTS_CONTEXT: &str = "CairnResults";

/// Key context carried by the platform caption buttons. Enter and Space are consumed here so the
/// focused caption activates itself through its own click handler instead of also running the
/// window-scoped `enter` command (open the selected result) while it has focus.
pub const WINDOW_CONTROL_CONTEXT: &str = "CairnWindowControl";

actions!(
    cairn,
    [
        /// Move the row selection one position up.
        SelectPreviousRow,
        /// Move the row selection one position down.
        SelectNextRow,
        /// Open the selected file (Enter).
        OpenSelected,
        /// Reveal the selected file in Explorer (Ctrl+Enter).
        RevealSelectedLocation,
        /// Copy the full path(s) of the selection (Ctrl+Shift+C).
        CopyFullPaths,
        /// Copy the name(s) of the selection (Ctrl+Shift+N).
        CopyFileNames,
        /// Copy the selection, falling back to full paths when the query has no selection (Ctrl+C).
        CopyTextOrPaths,
        /// Paste clipboard text into the search box (Ctrl+V, Shift+Insert).
        PasteQueryText,
        /// Undo the last search box text edit (Ctrl+Z).
        UndoQueryText,
        /// Redo the previously undone search box text edit (Ctrl+Y, Ctrl+Shift+Z).
        RedoQueryText,
        /// Cut the selected search box text to clipboard (Ctrl+X, Shift+Delete).
        CutQueryText,
        /// Replace the query with the whole text (Ctrl+A).
        SelectAllQueryText,
        /// Clear the query; hide the window when it is already empty (Escape).
        EscapeQuery,
        /// Move the window's keyboard focus into the search box (Ctrl+F, F3, Alt+D).
        FocusSearchBox,
        /// Toggle case sensitivity (Ctrl+I).
        ToggleCaseSensitive,
        /// Toggle whole-word matching (Ctrl+B).
        ToggleWholeWord,
        /// Toggle path matching (Ctrl+U).
        ToggleMatchPath,
        /// Toggle regular-expression mode (Ctrl+R).
        ToggleRegex,
        /// Advance the file-type filter to the next category (Alt+P).
        CycleFilterCategory,
        /// Open the title-bar 文件 menu (Alt+F).
        OpenFileMenu,
        /// Open the title-bar 编辑 menu (Alt+E).
        OpenEditMenu,
        /// Open the title-bar 查看 menu: file type, ordering, theme and zoom (Alt+V).
        OpenViewMenu,
        /// Open the title-bar 搜索 menu, the flags that qualify the query (Alt+S).
        OpenSearchMenu,
        /// Open the title-bar 工具 menu: language, autostart and the index (Alt+T).
        OpenToolsMenu,
        /// Open the title-bar 帮助 menu (Alt+H).
        OpenHelpMenu,
        /// Show the Windows properties sheet of the selected result (Alt+Enter, the platform
        /// convention). The result context menu offers the same command.
        ShowProperties,
        /// Move the selection to the Recycle Bin through the native file pipeline (Delete). The
        /// result context menu offers the same command.
        DeleteSelected,
        /// Put the selected Recycle Bin items back in their original locations. Only meaningful
        /// while the Recycle Bin view is open; the file and row menus offer it there.
        RestoreSelected,
        /// Move the result table one step towards its left edge (Left, while the table has focus).
        ScrollTableLeft,
        /// Move the result table one step towards its right edge (Right, while the table has focus).
        ScrollTableRight,
        /// Put the result table's left edge back at the window's left edge (Home).
        ScrollTableStart,
        /// Bring the table's rightmost column into view (End).
        ScrollTableEnd,
        /// Navigate back in search/filter history (Alt+Left).
        NavigateBack,
        /// Navigate forward in search/filter history (Alt+Right).
        NavigateForward,
    ]
);

/// One key the window binds.
///
/// The table is data rather than a sequence of calls, so the announcement a control makes and the
/// dispatch the key performs can be read without a running application: the shortcut, the action
/// it reaches and the context it is scoped to are all in one place.
pub struct KeyBindingEntry {
    /// The keystroke, spelled the way the accesskit announcement spells it.
    pub shortcut: &'static str,
    /// The action the key dispatches.
    pub action: &'static str,
    /// The key context the binding is scoped to.
    pub context: &'static str,
    /// The binding itself, installed into the keymap.
    binding: KeyBinding,
}

/// Builds one entry. The action's name is taken from the action itself, so the table cannot claim
/// that a key reaches an action it does not.
fn bind<A: Action>(shortcut: &'static str, action: A, context: &'static str) -> KeyBindingEntry {
    KeyBindingEntry {
        shortcut,
        action: A::name_for_type(),
        context,
        binding: KeyBinding::new(shortcut, action, Some(context)),
    }
}

/// Every key the window binds, with the action and the context each one is scoped to.
///
/// This table is the one place a key is defined. A control that advertises a shortcut and the
/// keymap that dispatches it are compared through it, so neither side has to be read as source
/// text to be checked.
pub fn key_bindings() -> Vec<KeyBindingEntry> {
    vec![
        bind("up", SelectPreviousRow, WINDOW_CONTEXT),
        bind("down", SelectNextRow, WINDOW_CONTEXT),
        bind("enter", OpenSelected, WINDOW_CONTEXT),
        bind("ctrl-enter", RevealSelectedLocation, WINDOW_CONTEXT),
        bind("ctrl-shift-c", CopyFullPaths, WINDOW_CONTEXT),
        bind("ctrl-shift-n", CopyFileNames, WINDOW_CONTEXT),
        bind("ctrl-c", CopyTextOrPaths, WINDOW_CONTEXT),
        bind("ctrl-insert", CopyTextOrPaths, WINDOW_CONTEXT),
        bind("ctrl-v", PasteQueryText, WINDOW_CONTEXT),
        bind("shift-insert", PasteQueryText, WINDOW_CONTEXT),
        bind("ctrl-x", CutQueryText, WINDOW_CONTEXT),
        bind("shift-delete", CutQueryText, WINDOW_CONTEXT),
        bind("ctrl-z", UndoQueryText, WINDOW_CONTEXT),
        bind("ctrl-y", RedoQueryText, WINDOW_CONTEXT),
        bind("ctrl-shift-z", RedoQueryText, WINDOW_CONTEXT),
        bind("ctrl-a", SelectAllQueryText, WINDOW_CONTEXT),
        bind("escape", EscapeQuery, WINDOW_CONTEXT),
        bind("ctrl-f", FocusSearchBox, WINDOW_CONTEXT),
        bind("f3", FocusSearchBox, WINDOW_CONTEXT),
        bind("alt-d", FocusSearchBox, WINDOW_CONTEXT),
        bind("ctrl-i", ToggleCaseSensitive, WINDOW_CONTEXT),
        bind("ctrl-b", ToggleWholeWord, WINDOW_CONTEXT),
        bind("ctrl-u", ToggleMatchPath, WINDOW_CONTEXT),
        bind("ctrl-r", ToggleRegex, WINDOW_CONTEXT),
        // The filter row advertises this shortcut through AccessKit, so the key has to reach the
        // action: an announced shortcut that dispatches nothing is a false promise to a screen reader.
        bind("alt-p", CycleFilterCategory, WINDOW_CONTEXT),
        // One key per top-level menu in the title bar. Every mnemonic opens the menu whose name it
        // abbreviates, so a keyboard user reaches the same commands a pointer does.
        bind("alt-f", OpenFileMenu, WINDOW_CONTEXT),
        bind("alt-e", OpenEditMenu, WINDOW_CONTEXT),
        bind("alt-t", OpenToolsMenu, WINDOW_CONTEXT),
        bind("alt-s", OpenSearchMenu, WINDOW_CONTEXT),
        bind("alt-v", OpenViewMenu, WINDOW_CONTEXT),
        bind("alt-h", OpenHelpMenu, WINDOW_CONTEXT),
        // The properties sheet was only ever in the result context menu; Windows also reaches it
        // with Alt+Enter, so the command keeps a keyboard path outside the popup.
        bind("alt-enter", ShowProperties, WINDOW_CONTEXT),
        // Desktop back/forward navigation shortcuts
        bind("alt-left", NavigateBack, WINDOW_CONTEXT),
        bind("alt-right", NavigateForward, WINDOW_CONTEXT),
        // The four keys the result table answers with, and only while it holds focus. Tab moves it there
        // from the query box (`gpui-base`'s `Root` owns Tab/Shift+Tab and walks the window's tab stops),
        // and the same four keys keep moving the caret while the query box holds focus.
        bind("left", ScrollTableLeft, RESULTS_CONTEXT),
        bind("right", ScrollTableRight, RESULTS_CONTEXT),
        bind("home", ScrollTableStart, RESULTS_CONTEXT),
        bind("end", ScrollTableEnd, RESULTS_CONTEXT),
        // Delete belongs to the table region, not the window: the search box
        // uses the same key for forward-deleting text, and a window-wide
        // binding would swallow that edit.
        bind("delete", DeleteSelected, RESULTS_CONTEXT),
        // A focused caption button is still inside the window key context, so `enter` there would
        // out-rank nothing and open the selected row. Consume the activation keys in the control's own
        // context; the button's click handler runs on key-up, like every native button.
        bind("enter", NoAction {}, WINDOW_CONTROL_CONTEXT),
        bind("space", NoAction {}, WINDOW_CONTROL_CONTEXT),
    ]
}

/// Binds every SPEC UI-03 key to its action. Call once during bootstrap, before any window or menu
/// is built: `Keymap` reads the bindings when they are installed.
pub fn init_keymap(cx: &mut App) {
    let entries = key_bindings();
    // Two bindings for the same key in the same context would leave the dispatch order as the only
    // thing deciding which action runs. The table has to stay unambiguous, so a duplicate fails at
    // bootstrap rather than silently at the keystroke.
    let mut claimed: std::collections::HashMap<(&'static str, &'static str), &'static str> =
        std::collections::HashMap::new();
    for entry in &entries {
        if let Some(previous) = claimed.insert((entry.shortcut, entry.context), entry.action) {
            panic!(
                "{} is bound twice in {}: {} and {}",
                entry.shortcut, entry.context, previous, entry.action
            );
        }
    }
    cx.bind_keys(entries.into_iter().map(|entry| entry.binding));
}

/// The keys that belong to the result table region alone, with the action each must resolve to. Used
/// by the keymap regression tests, which check that the region answers them and that the window —
/// where the query box sits — does not.
#[cfg(test)]
pub fn results_region_bindings() -> Vec<(&'static str, &'static str)> {
    vec![
        ("left", ScrollTableLeft::name_for_type()),
        ("right", ScrollTableRight::name_for_type()),
        ("home", ScrollTableStart::name_for_type()),
        ("end", ScrollTableEnd::name_for_type()),
    ]
}

#[cfg(test)]
mod keymap_table {
    // The same shadow the headless module above documents: `super::*` reaches GPUI's `test` attribute
    // macro through this crate's prelude, and expanding a built-in `#[test]` with it recurses until
    // rustc overflows its own stack. Re-importing the built-in wins over the glob.
    use core::prelude::rust_2021::test;

    use super::*;

    /// `left`, `right`, `home` and `end` move the columns, and the query box is where the window's
    /// focus normally sits. Bound to the window context they resolve before the box ever sees them, so
    /// typing would steer the table instead of the caret.
    #[test]
    fn the_table_keys_are_bound_only_to_the_results_region() {
        let bindings = key_bindings();
        for (keystroke, action_name) in results_region_bindings() {
            let matching: Vec<&KeyBindingEntry> = bindings
                .iter()
                .filter(|entry| entry.shortcut == keystroke && entry.action == action_name)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "{keystroke} must reach {action_name} from exactly one binding, found {}",
                matching.len()
            );
            assert_eq!(
                matching[0].context, RESULTS_CONTEXT,
                "{keystroke} is scoped to something other than {RESULTS_CONTEXT}, which takes it \
                 away from the query box"
            );
        }
    }

    /// The window block is the one place a key may be scoped to the whole shell, and none of the four
    /// table keys may appear in it. Stated separately because the check above only follows the table
    /// this file names; a binding added to the window block for the same action would pass it.
    #[test]
    fn the_window_context_does_not_rebind_the_table_keys() {
        let bindings = key_bindings();
        for (keystroke, _) in results_region_bindings() {
            let in_window = bindings
                .iter()
                .any(|entry| entry.shortcut == keystroke && entry.context == WINDOW_CONTEXT);
            assert!(
                !in_window,
                "{keystroke} is bound in the window context, so it answers wherever focus sits"
            );
        }
    }

    /// Every key the window binds must be a keystroke the platform can parse: a binding the keymap
    /// cannot read is a key that silently does nothing.
    #[test]
    fn every_bound_shortcut_is_a_keystroke_the_platform_can_read() {
        for entry in key_bindings() {
            assert!(
                gpui_kit::gpui::Keystroke::parse(entry.shortcut).is_ok(),
                "{shortcut} is not a keystroke the platform can read",
                shortcut = entry.shortcut
            );
        }
    }
}

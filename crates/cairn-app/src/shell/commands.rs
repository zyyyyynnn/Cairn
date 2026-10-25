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

/// Binds every SPEC UI-03 key to its action. Call once during bootstrap, before any window or menu
/// is built: `Keymap` reads the bindings when they are installed.
pub fn init_keymap(cx: &mut App) {
    let context = Some(WINDOW_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousRow, context),
        KeyBinding::new("down", SelectNextRow, context),
        KeyBinding::new("enter", OpenSelected, context),
        KeyBinding::new("ctrl-enter", RevealSelectedLocation, context),
        KeyBinding::new("ctrl-shift-c", CopyFullPaths, context),
        KeyBinding::new("ctrl-shift-n", CopyFileNames, context),
        KeyBinding::new("ctrl-c", CopyTextOrPaths, context),
        KeyBinding::new("ctrl-insert", CopyTextOrPaths, context),
        KeyBinding::new("ctrl-v", PasteQueryText, context),
        KeyBinding::new("shift-insert", PasteQueryText, context),
        KeyBinding::new("ctrl-x", CutQueryText, context),
        KeyBinding::new("shift-delete", CutQueryText, context),
        KeyBinding::new("ctrl-z", UndoQueryText, context),
        KeyBinding::new("ctrl-y", RedoQueryText, context),
        KeyBinding::new("ctrl-shift-z", RedoQueryText, context),
        KeyBinding::new("ctrl-a", SelectAllQueryText, context),
        KeyBinding::new("escape", EscapeQuery, context),
        KeyBinding::new("ctrl-f", FocusSearchBox, context),
        KeyBinding::new("f3", FocusSearchBox, context),
        KeyBinding::new("alt-d", FocusSearchBox, context),
        KeyBinding::new("ctrl-i", ToggleCaseSensitive, context),
        KeyBinding::new("ctrl-b", ToggleWholeWord, context),
        KeyBinding::new("ctrl-u", ToggleMatchPath, context),
        KeyBinding::new("ctrl-r", ToggleRegex, context),
        // The filter row advertises this shortcut through AccessKit, so the key has to reach the
        // action: an announced shortcut that dispatches nothing is a false promise to a screen reader.
        KeyBinding::new("alt-p", CycleFilterCategory, context),
        // One key per top-level menu in the title bar. Every mnemonic opens the menu whose name it
        // abbreviates, so a keyboard user reaches the same commands a pointer does.
        KeyBinding::new("alt-f", OpenFileMenu, context),
        KeyBinding::new("alt-e", OpenEditMenu, context),
        KeyBinding::new("alt-t", OpenToolsMenu, context),
        KeyBinding::new("alt-s", OpenSearchMenu, context),
        KeyBinding::new("alt-v", OpenViewMenu, context),
        KeyBinding::new("alt-h", OpenHelpMenu, context),
        // The properties sheet was only ever in the result context menu; Windows also reaches it
        // with Alt+Enter, so the command keeps a keyboard path outside the popup.
        KeyBinding::new("alt-enter", ShowProperties, context),
        // Desktop back/forward navigation shortcuts
        KeyBinding::new("alt-left", NavigateBack, context),
        KeyBinding::new("alt-right", NavigateForward, context),
    ]);

    // The four keys the result table answers with, and only while it holds focus. Tab moves it there
    // from the query box (`gpui-base`'s `Root` owns Tab/Shift+Tab and walks the window's tab stops),
    // and the same four keys keep moving the caret while the query box holds focus.
    let results_context = Some(RESULTS_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", ScrollTableLeft, results_context),
        KeyBinding::new("right", ScrollTableRight, results_context),
        KeyBinding::new("home", ScrollTableStart, results_context),
        KeyBinding::new("end", ScrollTableEnd, results_context),
        // Delete belongs to the table region, not the window: the search box
        // uses the same key for forward-deleting text, and a window-wide
        // binding would swallow that edit.
        KeyBinding::new("delete", DeleteSelected, results_context),
    ]);

    // A focused caption button is still inside the window key context, so `enter` there would
    // out-rank nothing and open the selected row. Consume the activation keys in the control's own
    // context; the button's click handler runs on key-up, like every native button.
    let control_context = Some(WINDOW_CONTROL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", NoAction {}, control_context),
        KeyBinding::new("space", NoAction {}, control_context),
    ]);
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
mod keymap_source {
    // The same shadow the headless module above documents: `super::*` reaches GPUI's `test` attribute
    // macro through this crate's prelude, and expanding a built-in `#[test]` with it recurses until
    // rustc overflows its own stack. Re-importing the built-in wins over the glob.
    use core::prelude::rust_2021::test;

    use super::*;

    /// The third argument of a `KeyBinding::new` call, read off the source line that builds it — the
    /// context that binding is scoped to. The call site ends with `),` so the last comma-separated
    /// field is empty and the context is the last non-empty one.
    fn scoped_to(line: &str) -> String {
        let (_, call) = line
            .split_once("KeyBinding::new(")
            .unwrap_or_else(|| panic!("not a binding line: {line}"));
        call.split(',')
            .map(|argument| argument.trim().trim_end_matches(')').trim())
            .rfind(|argument| !argument.is_empty())
            .unwrap_or_else(|| panic!("no context argument on: {line}"))
            .to_string()
    }

    /// `left`, `right`, `home` and `end` move the columns, and the query box is where the window's
    /// focus normally sits. Bound to the window context they resolve before the box ever sees them, so
    /// typing would steer the table instead of the caret.
    #[test]
    fn the_table_keys_are_bound_only_to_the_results_region() {
        let source = include_str!("commands.rs");
        for (keystroke, action_name) in results_region_bindings() {
            // An Action's registered name is `<namespace>::<Type>` (the `actions!` macro builds it from
            // the module path), while the binding line spells the bare type. The last segment is the
            // one the source uses.
            let action = action_name
                .rsplit("::")
                .next()
                .expect("an action has a name");
            let prefix = format!("KeyBinding::new(\"{keystroke}\",{action}");
            let lines: Vec<&str> = source
                .lines()
                .filter(|line| line.replace(' ', "").starts_with(&prefix))
                .collect();
            assert_eq!(
                lines.len(),
                1,
                "{keystroke} must reach {action_name} from exactly one binding, found {}",
                lines.len()
            );
            assert_eq!(
                scoped_to(lines[0]),
                "results_context",
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
        let source = include_str!("commands.rs");
        let window_block = source
            .split("let results_context")
            .next()
            .expect("init_keymap defines the window block before the results block");
        for (keystroke, _) in results_region_bindings() {
            let bound = format!("KeyBinding::new(\"{keystroke}\"");
            assert!(
                !window_block.contains(&bound),
                "{keystroke} is bound in the window block, so it answers wherever focus sits"
            );
        }
    }

    /// Each binding names a *variable* for its context, so checking that name alone proves nothing:
    /// repointing `let results_context` at `WINDOW_CONTEXT` keeps every binding line intact and takes
    /// the caret away from the query box all the same. The two definitions are therefore pinned to the
    /// constants they must carry, which closes the chain the per-binding check leaves open.
    #[test]
    fn each_context_variable_still_names_the_context_it_claims() {
        let source = include_str!("commands.rs");
        for (definition, constant) in [
            ("let context = Some(", "WINDOW_CONTEXT"),
            ("let results_context = Some(", "RESULTS_CONTEXT"),
        ] {
            let line = source
                .lines()
                .find(|line| line.trim_start().starts_with(definition))
                .unwrap_or_else(|| panic!("init_keymap no longer defines {definition}"));
            assert!(
                line.contains(constant),
                "{definition}.. resolves to something other than {constant}, which silently moves \
                 every binding that names this variable into another context"
            );
        }
    }

    #[test]
    fn test_bindings_are_valid_keystrokes() {
        for key in ["ctrl-v", "shift-insert"] {
            let res = gpui_kit::gpui::Keystroke::parse(key);
            assert!(res.is_ok(), "Key {key} failed to parse: {:?}", res.err());
        }
    }
}

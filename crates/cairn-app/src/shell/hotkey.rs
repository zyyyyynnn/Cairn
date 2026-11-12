// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The global summon hotkey (UI-08).

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Owns the registered chord for the lifetime of the app; dropping it unregisters the hotkey.
pub(crate) struct GlobalHotkey {
    _manager: GlobalHotKeyManager,
}

/// Ctrl+Alt+Space summons or hides the main window from anywhere.
fn default_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space)
}

/// Registers the default chord and listens for it on a dedicated thread. The error is returned
/// when another application already owns the chord; the caller keeps the window usable without it.
pub(crate) fn register() -> Result<GlobalHotkey, String> {
    let manager = GlobalHotKeyManager::new().map_err(|err| format!("hotkey manager: {err}"))?;
    let hotkey = default_hotkey();
    let id = hotkey.id();
    manager
        .register(hotkey)
        .map_err(|err| format!("register Ctrl+Alt+Space: {err}"))?;
    std::thread::Builder::new()
        .name("cairn-hotkey".to_string())
        .spawn(move || {
            while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
                if event.id == id && event.state == HotKeyState::Pressed {
                    crate::shell::tray::toggle_main_window();
                }
            }
        })
        .map_err(|err| format!("hotkey listener: {err}"))?;
    Ok(GlobalHotkey { _manager: manager })
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::default_hotkey;
    use global_hotkey::hotkey::{Code, Modifiers};

    #[test]
    fn the_default_hotkey_is_ctrl_alt_space() {
        let hotkey = default_hotkey();
        assert_eq!(hotkey.mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(hotkey.key, Code::Space);
    }
}

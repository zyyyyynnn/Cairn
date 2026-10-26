// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Client user preferences persistence in `%APPDATA%\Cairn\settings.toml` (SET-01, UI-02).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The interface languages `AppSettings::language` stores, the `I18nManager` selects its bundle with,
/// and the settings dialog offers. The dialog's option list, the startup selection and the persisted
/// value all read these, so a third locale cannot be offered before it can be resolved.
pub const LANGUAGE_SYSTEM: &str = "system";
pub const LANGUAGE_ZH_CN: &str = "zh-CN";
pub const LANGUAGE_EN_US: &str = "en-US";

pub const THEME_SYSTEM: &str = "system";
pub const THEME_LIGHT: &str = "light";
pub const THEME_DARK: &str = "dark";

fn default_language() -> String {
    LANGUAGE_SYSTEM.to_string()
}

fn default_theme() -> String {
    THEME_SYSTEM.to_string()
}

/// The language the window should speak for a stored choice. `system` resolves through the Windows
/// UI language; every other value is already a supported locale code.
pub fn effective_language(choice: &str) -> String {
    if choice == LANGUAGE_SYSTEM {
        system_language().to_string()
    } else {
        choice.to_string()
    }
}

/// The OS UI language, reduced to the two locales this application ships. Any non-Chinese UI
/// language uses the English bundle rather than showing a half-translated third locale.
pub fn system_language() -> &'static str {
    #[cfg(windows)]
    {
        use windows::Win32::Globalization::GetUserDefaultUILanguage;
        // LANGID: the low 10 bits are the primary language. 0x04 is Chinese.
        let langid = unsafe { GetUserDefaultUILanguage() };
        if langid & 0x03ff == 0x04 {
            LANGUAGE_ZH_CN
        } else {
            LANGUAGE_EN_US
        }
    }
    #[cfg(not(windows))]
    {
        LANGUAGE_EN_US
    }
}

fn default_zoom_level() -> f32 {
    1.0
}

fn default_name_width() -> f32 {
    310.0
}

fn default_path_width() -> f32 {
    360.0
}

fn default_size_width() -> f32 {
    110.0
}

fn default_modified_width() -> f32 {
    160.0
}

fn default_sort_column() -> String {
    "name".to_string()
}

fn default_true() -> bool {
    true
}

fn default_toggle_hotkey() -> String {
    "Ctrl+Shift+F".to_string()
}

/// Stored column widths are logical pixels at 100% zoom; the shell renders them in rem, so they
/// follow interface zoom. `crate::table::model::ColumnWidths` owns the conversion and the minimums.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ColumnSettings {
    #[serde(default = "default_name_width")]
    pub name_width: f32,
    #[serde(default = "default_path_width")]
    pub path_width: f32,
    #[serde(default = "default_size_width")]
    pub size_width: f32,
    #[serde(default = "default_modified_width")]
    pub modified_width: f32,
    #[serde(default = "default_sort_column")]
    pub sort_column: String,
    #[serde(default = "default_true")]
    pub sort_asc: bool,
}

impl Default for ColumnSettings {
    fn default() -> Self {
        Self {
            name_width: default_name_width(),
            path_width: default_path_width(),
            size_width: default_size_width(),
            modified_width: default_modified_width(),
            sort_column: default_sort_column(),
            sort_asc: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HotkeySettings {
    #[serde(default = "default_toggle_hotkey")]
    pub toggle_window: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            toggle_window: default_toggle_hotkey(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AppSettings {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_zoom_level")]
    pub zoom_level: f32,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub columns: ColumnSettings,
    #[serde(default)]
    pub hotkeys: HotkeySettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
            theme: default_theme(),
            zoom_level: default_zoom_level(),
            autostart: false,
            columns: ColumnSettings::default(),
            hotkeys: HotkeySettings::default(),
        }
    }
}

pub fn default_settings_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Cairn").join("settings.toml");
        }
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        return PathBuf::from(userprofile)
            .join(".cairn")
            .join("settings.toml");
    }
    PathBuf::from("settings.toml")
}

/// Configures current-user autostart via Windows Registry HKCU Run key (SET-03).
#[cfg(windows)]
pub fn set_autostart_registry(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_SZ,
    };

    let subkey = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let value_name = w!("Cairn");

    unsafe {
        let mut hkey = windows::Win32::System::Registry::HKEY::default();
        let status = RegOpenKeyExW(HKEY_CURRENT_USER, subkey, None, KEY_SET_VALUE, &mut hkey);
        if status.is_err() {
            return Err(format!("RegOpenKeyExW failed with code {:?}", status).into());
        }

        if enable {
            let current_exe = std::env::current_exe()?;
            let exe_str = current_exe.to_string_lossy().to_string();
            let wide: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();

            let res = RegSetValueExW(
                hkey,
                value_name,
                None,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    wide.as_ptr() as *const u8,
                    wide.len() * 2,
                )),
            );
            let _ = RegCloseKey(hkey);
            if res.is_err() {
                return Err(format!("RegSetValueExW failed: {:?}", res).into());
            }
        } else {
            let _ = RegDeleteValueW(hkey, value_name);
            let _ = RegCloseKey(hkey);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_autostart_registry(_enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

impl AppSettings {
    /// `true` when the persisted choice resolves to the dark palette. `system` defers to the OS
    /// appearance passed in by the caller, which reads it once per application run.
    pub fn is_dark(&self, system_dark: bool) -> bool {
        match self.theme.as_str() {
            THEME_DARK => true,
            THEME_LIGHT => false,
            _ => system_dark,
        }
    }

    /// The concrete locale code for the persisted language choice.
    pub fn effective_language(&self) -> String {
        effective_language(&self.language)
    }

    pub fn load() -> Self {
        let path = default_settings_path();
        Self::load_from_path(&path).unwrap_or_default()
    }

    pub fn load_from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        let settings = toml::from_str(&content)?;
        Ok(settings)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = default_settings_path();
        self.save_to_path(&path)?;
        let _ = set_autostart_registry(self.autostart);
        Ok(())
    }

    pub fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;

        // Write to temporary file in same directory and atomic rename (NFR-09)
        let temp_path = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&temp_path, toml_str)?;
        if let Err(_e) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(path);
            fs::rename(&temp_path, path)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_default_and_serialization() {
        let default_settings = AppSettings::default();
        assert_eq!(default_settings.language, LANGUAGE_SYSTEM);
        assert_eq!(default_settings.theme, THEME_SYSTEM);
        assert_eq!(default_settings.zoom_level, 1.0);
        assert!(!default_settings.autostart);
        assert_eq!(default_settings.columns.name_width, 310.0);
        assert_eq!(default_settings.columns.sort_column, "name");
        assert!(default_settings.columns.sort_asc);

        let serialized = toml::to_string_pretty(&default_settings).unwrap();
        let parsed: AppSettings = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed, default_settings);
    }

    #[test]
    fn the_system_theme_follows_the_os_and_explicit_choices_pin_the_palette() {
        let mut settings = AppSettings::default();
        assert!(!settings.is_dark(false));
        assert!(settings.is_dark(true));

        settings.theme = THEME_LIGHT.to_string();
        assert!(!settings.is_dark(true));

        settings.theme = THEME_DARK.to_string();
        assert!(settings.is_dark(false));
    }

    #[test]
    fn a_system_language_resolves_to_a_bundle_the_app_ships() {
        let resolved = effective_language(LANGUAGE_SYSTEM);
        assert!(
            resolved == LANGUAGE_ZH_CN || resolved == LANGUAGE_EN_US,
            "system language resolved to an unsupported locale: {resolved}"
        );
        assert_eq!(effective_language(LANGUAGE_EN_US), LANGUAGE_EN_US);
        assert_eq!(effective_language(LANGUAGE_ZH_CN), LANGUAGE_ZH_CN);
    }

    #[test]
    fn test_settings_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("Cairn").join("settings.toml");

        let custom_settings = AppSettings {
            language: "en-US".to_string(),
            theme: "dark".to_string(),
            zoom_level: 1.25,
            columns: ColumnSettings {
                name_width: 350.0,
                sort_column: "size".to_string(),
                sort_asc: false,
                ..Default::default()
            },
            ..Default::default()
        };

        custom_settings.save_to_path(&settings_path).unwrap();

        let loaded = AppSettings::load_from_path(&settings_path).unwrap();
        assert_eq!(loaded, custom_settings);
    }

    #[test]
    fn test_settings_missing_or_invalid_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_path = dir.path().join("invalid.toml");
        fs::write(&invalid_path, "invalid toml syntax [[[[").unwrap();

        assert!(AppSettings::load_from_path(&invalid_path).is_err());

        let non_existent = dir.path().join("missing.toml");
        let fallback = AppSettings::load_from_path(&non_existent).unwrap();
        assert_eq!(fallback, AppSettings::default());
    }

    #[test]
    fn test_autostart_setting_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");

        let settings = AppSettings {
            autostart: true,
            ..Default::default()
        };
        settings.save_to_path(&path).unwrap();

        let loaded = AppSettings::load_from_path(&path).unwrap();
        assert!(loaded.autostart);
    }
}

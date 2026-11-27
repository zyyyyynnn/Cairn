// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Debug-only screen preview for the visual review matrix.
//!
//! It drives the production views with representative state so every page can be captured without
//! running an MSI transaction. Release builds return `None` and ignore the environment entirely.

use std::path::PathBuf;

use crate::PRODUCT_VERSION;
use crate::model::{InstalledProduct, Launch, Screen, Stage, default_install_dir};

#[derive(Clone, Debug)]
pub struct Preview {
    pub screen: Screen,
    pub launch: Launch,
    pub language: Option<String>,
    pub dark: Option<bool>,
    pub installed: bool,
    pub progress: Option<f32>,
    pub stage: Stage,
    pub log: Option<PathBuf>,
    pub error: Option<&'static str>,
    pub uninstall_attempted: bool,
    pub record_remaining: bool,
}

impl Preview {
    pub fn from_env() -> Option<Self> {
        #[cfg(not(debug_assertions))]
        {
            return None;
        }

        #[cfg(debug_assertions)]
        {
            let key = std::env::var("CAIRN_SETUP_PREVIEW").ok()?;
            Self::from_key(
                &key,
                std::env::var("CAIRN_SETUP_PREVIEW_LANG").ok(),
                std::env::var("CAIRN_SETUP_PREVIEW_THEME").ok(),
            )
        }
    }

    #[cfg(debug_assertions)]
    fn from_key(key: &str, language: Option<String>, theme: Option<String>) -> Option<Self> {
        let screen = screen_from_key(key)?;
        let indeterminate = matches!(
            key,
            "installing-indeterminate" | "uninstalling-indeterminate"
        );
        let uninstall_flow = matches!(
            screen,
            Screen::Uninstall | Screen::Uninstalling | Screen::Uninstalled
        ) || matches!(
            key,
            "uninstall-failed" | "uninstall-missing" | "uninstall-invalid" | "uninstall-staging"
        );
        let launch = if uninstall_flow {
            Launch::Uninstall
        } else {
            Launch::Setup
        };
        let installed = matches!(
            screen,
            Screen::Maintenance | Screen::Uninstall | Screen::Uninstalling | Screen::Uninstalled
        ) || matches!(key, "uninstall-failed" | "uninstall-staging");
        let log = ((screen == Screen::Failed && launch == Launch::Setup)
            || matches!(key, "uninstall-failed" | "uninstall-invalid"))
        .then(|| {
            let path = std::env::temp_dir().join("CairnSetup-preview.log");
            let mut log = String::from("Cairn Setup preview log\n");
            for line in 0..24 {
                log.push_str(&format!(
                    "MSI (s) ({line:02}) [12:34:56:{line:02}]: Product: Cairn -- installation step {line}\n"
                ));
            }
            let _ = std::fs::write(&path, log);
            path
        });
        let error = match (screen, launch) {
            (Screen::Failed, Launch::Setup) => Some("error_msi_unknown"),
            (Screen::Failed, Launch::Uninstall) if key == "uninstall-missing" => {
                Some("error_not_installed")
            }
            (Screen::Failed, Launch::Uninstall) if key == "uninstall-invalid" => {
                Some("error_msi_invalid")
            }
            (Screen::Failed, Launch::Uninstall) if key == "uninstall-staging" => {
                Some("error_staging")
            }
            (Screen::Failed, Launch::Uninstall) => Some("error_msi_unknown"),
            _ => None,
        };

        Some(Self {
            screen,
            launch,
            language: non_empty(language),
            dark: theme.map(|theme| theme.eq_ignore_ascii_case("dark")),
            installed,
            progress: if indeterminate {
                None
            } else {
                match screen {
                    Screen::Installing => Some(42.0),
                    Screen::Uninstalling => Some(67.0),
                    Screen::Uninstalled => Some(100.0),
                    _ => None,
                }
            },
            stage: if indeterminate {
                Stage::Preparing
            } else {
                match screen {
                    Screen::Installing => Stage::Copying,
                    Screen::Uninstalling | Screen::Uninstalled => Stage::Finishing,
                    _ => Stage::Preparing,
                }
            },
            log,
            error,
            uninstall_attempted: launch == Launch::Uninstall,
            record_remaining: key == "uninstalled-record",
        })
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.screen, Screen::Installing | Screen::Uninstalling)
    }

    pub fn installed_product(&self) -> Option<InstalledProduct> {
        self.installed.then(|| InstalledProduct {
            version: PRODUCT_VERSION.to_string(),
            install_dir: default_install_dir(),
            product_code: None,
        })
    }
}

#[cfg(debug_assertions)]
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(debug_assertions)]
fn screen_from_key(key: &str) -> Option<Screen> {
    match key {
        "location" => Some(Screen::Location),
        "installing" | "installing-indeterminate" => Some(Screen::Installing),
        "finished" => Some(Screen::Finished),
        "failed" => Some(Screen::Failed),
        "maintenance" => Some(Screen::Maintenance),
        "uninstall" => Some(Screen::Uninstall),
        "uninstalling" | "uninstalling-indeterminate" => Some(Screen::Uninstalling),
        "uninstalled" | "uninstalled-record" => Some(Screen::Uninstalled),
        "uninstall-failed" | "uninstall-missing" | "uninstall-invalid" | "uninstall-staging" => {
            Some(Screen::Failed)
        }
        _ => None,
    }
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::Preview;
    use crate::model::{Launch, Screen, Stage};

    #[test]
    fn preview_maps_determinate_and_indeterminate_progress_states() {
        let installing = Preview::from_key("installing", None, None).expect("installing preview");
        assert_eq!(installing.screen, Screen::Installing);
        assert_eq!(installing.launch, Launch::Setup);
        assert_eq!(installing.progress, Some(42.0));
        assert_eq!(installing.stage, Stage::Copying);
        assert!(installing.is_busy());

        let installing_indeterminate = Preview::from_key("installing-indeterminate", None, None)
            .expect("installing-indeterminate preview");
        assert_eq!(installing_indeterminate.screen, Screen::Installing);
        assert_eq!(installing_indeterminate.launch, Launch::Setup);
        assert_eq!(installing_indeterminate.progress, None);
        assert_eq!(installing_indeterminate.stage, Stage::Preparing);
        assert!(installing_indeterminate.is_busy());

        let uninstalling =
            Preview::from_key("uninstalling", None, None).expect("uninstalling preview");
        assert_eq!(uninstalling.screen, Screen::Uninstalling);
        assert_eq!(uninstalling.launch, Launch::Uninstall);
        assert_eq!(uninstalling.progress, Some(67.0));
        assert_eq!(uninstalling.stage, Stage::Finishing);
        assert!(uninstalling.installed);
        assert!(uninstalling.is_busy());

        let uninstalling_indeterminate =
            Preview::from_key("uninstalling-indeterminate", None, None)
                .expect("uninstalling-indeterminate preview");
        assert_eq!(uninstalling_indeterminate.screen, Screen::Uninstalling);
        assert_eq!(uninstalling_indeterminate.launch, Launch::Uninstall);
        assert_eq!(uninstalling_indeterminate.progress, None);
        assert_eq!(uninstalling_indeterminate.stage, Stage::Preparing);
        assert!(uninstalling_indeterminate.installed);
        assert!(uninstalling_indeterminate.is_busy());
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Deduplicated startup coordinator: parses process arguments once, loads settings and OS theme
//! preference once, builds one shared `I18nManager`, and avoids blocking SCM/disk probes before
//! the first window paints.

use std::rc::Rc;

use cairn_client::ServiceSession;

use crate::shell::i18n::I18nManager;
use crate::shell::settings::AppSettings;
use crate::shell::theme;

/// Parsed CLI launch arguments passed to the main desktop window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchPlan {
    pub raw_args: Vec<String>,
    pub initial_query: Option<String>,
    pub sidebar_collapsed: bool,
    pub open_match: bool,
}

impl LaunchPlan {
    /// Parses CLI arguments once for both single-command handling and initial window flags.
    pub fn parse(raw_args: Vec<String>) -> Self {
        let mut initial_query: Option<String> = None;
        let mut sidebar_collapsed = false;
        let mut open_match = false;

        for arg in &raw_args {
            if arg == "--collapsed" || arg == "--sidebar-collapsed" {
                sidebar_collapsed = true;
            } else if arg == "--open-match" {
                open_match = true;
            } else if !arg.starts_with('-') && initial_query.is_none() {
                initial_query = Some(arg.clone());
            }
        }

        Self {
            raw_args,
            initial_query,
            sidebar_collapsed,
            open_match,
        }
    }
}

/// External host reads required during application startup.
pub(crate) trait BootstrapPorts {
    fn load_settings(&self) -> AppSettings;
    fn system_prefers_dark(&self) -> bool;
}

/// Production host ports backed by persisted settings and Windows theme settings.
pub(crate) struct SystemBootstrapPorts;

impl BootstrapPorts for SystemBootstrapPorts {
    fn load_settings(&self) -> AppSettings {
        AppSettings::load()
    }

    fn system_prefers_dark(&self) -> bool {
        theme::system_prefers_dark()
    }
}

/// Consolidated startup artifacts shared across tray initialization, theming, and `CairnApp`.
pub(crate) struct StartupBundle {
    pub plan: LaunchPlan,
    pub settings: AppSettings,
    pub system_dark: bool,
    pub i18n: Rc<I18nManager>,
    pub session: ServiceSession,
}

impl StartupBundle {
    pub fn is_dark(&self) -> bool {
        self.settings.is_dark(self.system_dark)
    }
}

/// Prepares startup state in a single pass with zero blocking Win32 SCM or pipe I/O.
pub(crate) struct StartupCoordinator;

impl StartupCoordinator {
    pub fn prepare(raw_args: Vec<String>, ports: &dyn BootstrapPorts) -> StartupBundle {
        let plan = LaunchPlan::parse(raw_args);
        let settings = ports.load_settings();
        let system_dark = ports.system_prefers_dark();
        let i18n = Rc::new(I18nManager::new(&settings.effective_language()));
        let session = ServiceSession::new();
        StartupBundle {
            plan,
            settings,
            system_dark,
            i18n,
            session,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct FakeBootstrapPorts {
        settings: AppSettings,
        dark: bool,
        load_calls: Cell<usize>,
        dark_calls: Cell<usize>,
    }

    impl FakeBootstrapPorts {
        fn new(language: &str, dark: bool) -> Self {
            let settings = AppSettings {
                language: language.to_string(),
                ..AppSettings::default()
            };
            Self {
                settings,
                dark,
                load_calls: Cell::new(0),
                dark_calls: Cell::new(0),
            }
        }
    }

    impl BootstrapPorts for FakeBootstrapPorts {
        fn load_settings(&self) -> AppSettings {
            self.load_calls.set(self.load_calls.get() + 1);
            self.settings.clone()
        }

        fn system_prefers_dark(&self) -> bool {
            self.dark_calls.set(self.dark_calls.get() + 1);
            self.dark
        }
    }

    #[test]
    fn launch_plan_parses_flags_and_first_positional_query_once() {
        let plan = LaunchPlan::parse(vec![
            "--collapsed".to_string(),
            "--unknown".to_string(),
            "cargo.toml".to_string(),
            "--open-match".to_string(),
            "ignored-second".to_string(),
        ]);
        assert!(plan.sidebar_collapsed);
        assert!(plan.open_match);
        assert_eq!(plan.initial_query.as_deref(), Some("cargo.toml"));
    }

    #[test]
    fn prepare_reads_settings_and_theme_once_without_parsing_secondary_locale() {
        let ports = FakeBootstrapPorts::new("zh-CN", true);
        let bundle = StartupCoordinator::prepare(vec!["*.rs".to_string()], &ports);

        assert_eq!(ports.load_calls.get(), 1);
        assert_eq!(ports.dark_calls.get(), 1);
        assert!(bundle.is_dark());
        assert_eq!(bundle.plan.initial_query.as_deref(), Some("*.rs"));
        assert!(!bundle.i18n.messages.is_loaded("zh-CN"));
        assert!(!bundle.i18n.messages.is_loaded("en-US"));

        let tray_show = bundle.i18n.get_text("tray_show");
        assert!(!tray_show.is_empty());
        assert!(bundle.i18n.messages.is_loaded("zh-CN"));
        assert!(!bundle.i18n.messages.is_loaded("en-US"));
    }
}

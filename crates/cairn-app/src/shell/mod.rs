// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Window shell and cross-cutting concerns: theme tokens, localization,
//! accessibility registry, command and menu system, navigation history, Win32
//! OS integration, reveal, timing traces, settings, and the sidebar and status
//! bar views.

pub(crate) mod accessibility;
pub(crate) mod command_groups;
pub(crate) mod commands;
pub(crate) mod i18n;
pub(crate) mod loading_indicator;
pub(crate) mod menu_actions;
pub(crate) mod menu_rules;
pub(crate) mod nav_history;
pub(crate) mod os;
pub(crate) mod perf;
pub(crate) mod recycle_bin;
pub(crate) mod reveal;
pub(crate) mod settings;
pub(crate) mod sidebar_view;
pub(crate) mod status_bar;
pub(crate) mod status_bar_view;
pub(crate) mod theme;
pub(crate) mod toast;

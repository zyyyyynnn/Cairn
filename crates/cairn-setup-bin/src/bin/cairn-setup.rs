// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `CairnSetup.exe`: the visible installer and the silent entry point.

#![windows_subsystem = "windows"]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if cairn_setup::quiet::handle_cleanup_helper(&args) {
        return;
    }
    if args.iter().any(|arg| is_quiet(arg)) {
        std::process::exit(cairn_setup::quiet::quiet_setup());
    }
    cairn_setup::run::run_setup();
}

fn is_quiet(arg: &str) -> bool {
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "--quiet" | "/quiet" | "-quiet" | "/s" | "-s"
    )
}

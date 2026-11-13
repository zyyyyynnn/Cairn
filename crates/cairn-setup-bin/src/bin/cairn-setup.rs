// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `CairnSetup.exe`: the visible installer and the silent entry point.

#![windows_subsystem = "windows"]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| is_quiet(arg)) {
        let restart_app = args.iter().any(|arg| is_restart_app(arg));
        std::process::exit(cairn_setup::quiet::quiet_setup(
            restart_app,
            expected_sha256(&args),
        ));
    }
    cairn_setup::run::run_setup();
}

fn is_quiet(arg: &str) -> bool {
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "--quiet" | "/quiet" | "-quiet" | "/s" | "-s"
    )
}

fn is_restart_app(arg: &str) -> bool {
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "--restart-app" | "/restart-app" | "-restart-app"
    )
}

fn expected_sha256(args: &[String]) -> Option<String> {
    let index = args
        .iter()
        .position(|arg| arg.eq_ignore_ascii_case("--expected-sha256"))?;
    args.get(index + 1).cloned()
}

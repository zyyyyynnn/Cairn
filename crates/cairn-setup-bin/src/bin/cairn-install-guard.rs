// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! MSI custom action that rejects an install folder outside Program Files. The published MSI can be
//! run without the GPUI setup, so the UI's path validation alone does not protect the LocalSystem
//! service from a user-writable folder.

use std::path::PathBuf;

use cairn_setup::install_path::validate_install_path_under;

fn main() {
    let mut install_dir = None;
    let mut program_files = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--install-dir" => install_dir = args.next(),
            "--program-files" => program_files = args.next(),
            _ => {}
        }
    }

    let (Some(install_dir), Some(program_files)) = (install_dir, program_files) else {
        eprintln!("usage: CairnInstallGuard --install-dir <path> --program-files <path>");
        std::process::exit(2);
    };

    let program_files = PathBuf::from(program_files);
    if let Err(problem) = validate_install_path_under(&install_dir, &program_files) {
        eprintln!(
            "Cairn must be installed under {} ({}).",
            program_files.display(),
            problem.message_key()
        );
        std::process::exit(1);
    }
}

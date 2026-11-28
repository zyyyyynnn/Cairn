// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! Cairn development, testing, and benchmark tasks (xtask).

#[cfg(feature = "bench")]
mod bench;
mod comment_check;
mod corpus;
mod docs_check;
mod hygiene_check;
mod i18n_check;
mod size_check;
mod version_check;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
#[cfg(feature = "package")]
use xtask::package;

#[derive(Parser)]
#[command(name = "CairnXtask", about = "Cairn engineering automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run all fast repository gates (check-i18n, check-docs, check-size, check-hygiene, version-check)
    Check {
        /// The workspace root to check.
        #[arg(default_value = ".")]
        workspace: PathBuf,
        /// Only run documentation links, repository hygiene, and version gates (for docs-only changes).
        #[arg(long, default_value_t = false)]
        docs_only: bool,
    },
    /// Check that every locale pair is aligned, and that the code reading them agrees
    CheckI18n {
        /// The workspace root that holds the crates.
        #[arg(default_value = ".")]
        workspace: PathBuf,
    },
    /// Check required documentation pages and Markdown relative links
    CheckDocs {
        /// The workspace root that holds the documented crates.
        #[arg(default_value = ".")]
        workspace: PathBuf,
    },
    /// Check that no Rust source file exceeds the 2000-line monolith limit
    CheckSize {
        /// The workspace root that holds the crates.
        #[arg(default_value = ".")]
        workspace: PathBuf,
    },
    /// Check workspace lint inheritance, ignored-test registry parity, comment markers, copy symbols, and version identities
    CheckHygiene {
        /// The workspace root to inspect.
        #[arg(default_value = ".")]
        workspace: PathBuf,
    },
    /// Check or rewrite the per-file non-exempt comment budget
    CheckComments {
        /// The workspace root to inspect.
        #[arg(default_value = ".")]
        workspace: PathBuf,
        /// Rewrite the baseline after removing comments; never use it to admit new ones.
        #[arg(long, default_value_t = false)]
        update: bool,
        /// Admit reviewed increases or edits while rewriting the baseline.
        #[arg(long, default_value_t = false)]
        allow_increase: bool,
    },
    /// Package the GPUI setup (CairnSetup.exe), WiX MSI, and SHA256 checksums
    #[cfg(feature = "package")]
    Package {
        #[arg(short, long, default_value = "dist")]
        out_dir: PathBuf,
    },
    /// Generate synthetic file corpus for stress testing
    GenCorpus {
        #[arg(short, long, default_value = "corpus.tsv")]
        output: PathBuf,
        #[arg(short, long, default_value_t = 1_000_000)]
        count: usize,
    },
    /// Run the cost-model benchmark against a synthetic corpus
    #[cfg(feature = "bench")]
    BenchQuery {
        #[arg(short, long, default_value = "corpus.tsv")]
        corpus: PathBuf,
        #[arg(short, long, default_value_t = 6)]
        runs: usize,
        /// Run the fixed 100k fixture as the CI regression gate
        #[arg(long, default_value_t = false)]
        check: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Check {
            workspace,
            docs_only,
        } => {
            if !docs_only {
                i18n_check::check_i18n(&workspace)?;
                size_check::check_size(&workspace)?;
            }
            docs_check::check_docs(&workspace)?;
            hygiene_check::check_hygiene(&workspace)?;
            version_check::check_versions(&workspace)?;
        }
        Commands::CheckI18n { workspace } => {
            i18n_check::check_i18n(&workspace)?;
        }
        Commands::CheckDocs { workspace } => {
            docs_check::check_docs(&workspace)?;
        }
        Commands::CheckSize { workspace } => {
            size_check::check_size(&workspace)?;
        }
        Commands::CheckHygiene { workspace } => {
            hygiene_check::check_hygiene(&workspace)?;
            version_check::check_versions(&workspace)?;
        }
        Commands::CheckComments {
            workspace,
            update,
            allow_increase,
        } => {
            let files = hygiene_check::collect_rust_files(&workspace)?;
            if files.is_empty() {
                return Err("no Rust source files found for the comment gate".into());
            }
            let count = comment_check::check_comments(&workspace, &files, update, allow_increase)?;
            if !update {
                println!("comment check passed: {count} non-exempt comment block(s)");
            }
        }
        #[cfg(feature = "package")]
        Commands::Package { out_dir } => {
            package::run_package(out_dir)?;
        }
        Commands::GenCorpus { output, count } => {
            println!("Generating {count} synthetic records to {output:?}...");
            corpus::generate_synthetic_corpus(&output, count)?;
            println!("Done.");
        }
        #[cfg(feature = "bench")]
        Commands::BenchQuery {
            corpus,
            runs,
            check,
        } => {
            bench::run_benchmark(&corpus, runs, check)?;
        }
    }

    Ok(())
}

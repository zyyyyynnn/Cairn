// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cairn development, testing, and benchmark tasks (xtask).

mod bench;
mod corpus;
mod i18n_check;
mod package;
mod test_security;
mod test_vhd;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "xtask", about = "Cairn engineering automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check that every locale pair is aligned, and that the code reading them agrees
    CheckI18n {
        /// The workspace root that holds the crates.
        #[arg(default_value = ".")]
        workspace: PathBuf,
    },
    /// Package the GPUI setup (CairnSetup.exe), WiX MSI, and SHA256 checksums (REL-01, REL-02, REL-04)
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
    /// Run benchmark against synthetic corpus (SM-1, SM-2)
    #[command(alias = "bench-everything")]
    BenchQuery {
        #[arg(short, long, default_value = "corpus.tsv")]
        corpus: PathBuf,
        #[arg(short, long, default_value_t = 6)]
        runs: usize,
    },
    /// Run security, IPC DACL, and remote client rejection integration tests (SPEC SVC-02, SET-02)
    TestSecurity,
    /// Run NTFS/USN integration tests including SM-5 zero-difference verification
    TestNtfsVhd,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::CheckI18n { workspace } => {
            i18n_check::check_i18n(&workspace)?;
        }
        Commands::Package { out_dir } => {
            package::run_package(out_dir)?;
        }
        Commands::GenCorpus { output, count } => {
            println!("Generating {} synthetic records to {:?}...", count, output);
            corpus::generate_synthetic_corpus(&output, count)?;
            println!("Done.");
        }
        Commands::BenchQuery { corpus, runs } => {
            bench::run_benchmark(&corpus, runs)?;
        }
        Commands::TestSecurity => {
            test_security::run_security_tests()?;
        }
        Commands::TestNtfsVhd => {
            test_vhd::run_vhd_tests()?;
        }
    }

    Ok(())
}

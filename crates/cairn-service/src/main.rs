// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `CairnService.exe`: Windows background service host for Cairn.

use cairn_service::service;
use cairn_service::startup::ServiceRuntime;
use cairn_service::user_mode::UserModeRuntime;
use clap::Parser;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "CairnService", about = "Cairn Service")]
struct Cli {
    /// Run as a standalone console process (for development and testing)
    #[arg(short, long)]
    console: bool,
    /// Index the directories this account can read, on the user pipe. Started by the desktop app
    /// when the installed service is unavailable; never installed as a service.
    #[arg(long)]
    user_mode: bool,
    /// Exit when this process exits. The desktop app passes its own PID so a local indexer does
    /// not outlive the window that started it.
    #[arg(long, requires = "user_mode")]
    parent_pid: Option<u32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;

    if args.user_mode {
        info!("Running the user-privilege indexer...");
        let runtime = UserModeRuntime::build()?;
        rt.block_on(async move {
            let shutdown = async move {
                match args.parent_pid {
                    Some(pid) => cairn_service::user_mode::wait_for_parent_exit(pid).await,
                    None => {
                        let _ = tokio::signal::ctrl_c().await;
                        info!("Ctrl+C received, shutting down the user-mode indexer...");
                    }
                }
            };
            runtime.run(shutdown).await;
        });
    } else if args.console {
        info!("Running Cairn Service in interactive console mode...");
        let runtime = ServiceRuntime::build()?;
        rt.block_on(async move {
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
                info!("Ctrl+C received, shutting down gracefully...");
            };
            runtime.run(shutdown).await;
        });
    } else {
        #[cfg(windows)]
        {
            service::run_service()?;
        }
    }

    Ok(())
}

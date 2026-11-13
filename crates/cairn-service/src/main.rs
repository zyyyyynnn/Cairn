// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `CairnService.exe`: Windows background service host for Cairn.

use cairn_service::service;
use cairn_service::startup::ServiceRuntime;
use clap::Parser;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "CairnService", about = "Cairn Service")]
struct Cli {
    /// Run as a standalone console process (for development and testing)
    #[arg(short, long)]
    console: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Cli::parse();

    if args.console {
        info!("Running Cairn Service in interactive console mode...");
        let rt = tokio::runtime::Runtime::new()?;
        let runtime = ServiceRuntime::build()?;
        rt.block_on(async move {
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
                info!("Ctrl+C received, shutting down gracefully...");
            };
            runtime.run(shutdown).await
        })?;
    } else {
        #[cfg(windows)]
        {
            service::run_service()?;
        }
    }

    Ok(())
}

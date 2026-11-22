// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Startup sequence shared by the Windows service host and `CairnService --console`.
//!
//! Ordering matters for security: the data locations are created and their access control enforced
//! *before* `service.toml` is read, so a configuration planted by an unprivileged account is never
//! applied (SPEC SET-02, SVC-05).

use crate::config::ServiceConfig;
use crate::pipe_server::PipeServer;
use crate::security::{
    DataDirectoryError, ServiceDataLayout, default_service_data_root, prepare_data_layout,
};
use crate::session::SessionManager;
use crate::sync_engine::MultiVolumeEngine;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{error, info};

#[derive(Error, Debug)]
pub enum StartupError {
    #[error("service data locations could not be secured: {0}")]
    Data(#[from] DataDirectoryError),
}

/// Everything a running service consists of: index engine, search sessions, pipe server.
pub struct ServiceRuntime {
    pub engine: Arc<MultiVolumeEngine>,
    pub sessions: Arc<SessionManager>,
    pub pipe: PipeServer,
}

impl ServiceRuntime {
    /// Builds the runtime over the SPEC SET-02 data locations of the current machine.
    pub fn build() -> Result<Self, StartupError> {
        Self::build_at(&default_service_data_root())
    }

    /// Creates and secures a data tree, loads the configuration that survived that check, and
    /// indexes the fixed volumes.
    pub fn build_at(root: &Path) -> Result<Self, StartupError> {
        let layout = prepare_data_layout(root)?;
        let config = load_configuration(&layout);
        info!(
            "service data rooted at {}, index in {}",
            layout.root.display(),
            layout.index_dir.display()
        );

        let engine = Arc::new(MultiVolumeEngine::for_data_layout(&layout, config));
        let sessions = Arc::new(SessionManager::default());
        let pipe = PipeServer::new(engine.clone(), sessions.clone());

        Ok(Self {
            engine,
            sessions,
            pipe,
        })
    }

    /// Serves the pipe until `shutdown` resolves, keeping the index current in the background, and
    /// persists the indexes on the way out (IDX-04, NFR-09). A control-channel failure is returned
    /// after the index is saved, so the host exits with a failure status instead of looking stopped.
    pub async fn run(
        self,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Self {
            engine,
            pipe,
            sessions: _sessions,
        } = self;

        // Poll backlog volumes at 50ms so cold-start CatchingUp drains in seconds before returning
        // to the 500ms steady cadence once every volume is Synced.
        let engine_usn = engine.clone();
        let usn_task = tokio::spawn(async move {
            loop {
                let delay = if engine_usn.is_catching_up() { 50 } else { 500 };
                tokio::time::sleep(Duration::from_millis(delay)).await;
                engine_usn.poll_usn_changes();
            }
        });

        let engine_persist = engine.clone();
        let persist_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                if let Err(e) = engine_persist.save_all_indexes() {
                    error!("Periodic index save failed: {}", e);
                }
            }
        });

        engine.ensure_folder_sources_loaded();
        engine.spawn_property_extraction();

        let engine_snapshot = engine.clone();
        let snapshot_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                engine_snapshot.rescan_snapshot_volumes();
            }
        });

        let engine_folders = engine.clone();
        let folder_rescan_task = tokio::spawn(async move {
            let mut last_rescan = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let secs = engine_folders.get_config().folder_rescan_interval_secs;
                if secs == 0 {
                    last_rescan = tokio::time::Instant::now();
                    continue;
                }
                if last_rescan.elapsed() >= Duration::from_secs(secs) {
                    engine_folders.rescan_folder_sources();
                    last_rescan = tokio::time::Instant::now();
                }
            }
        });

        let served = tokio::select! {
            res = pipe.run() => {
                if let Err(e) = &res {
                    error!("Pipe server terminated with error: {}", e);
                }
                res
            },
            _ = shutdown => {
                info!("Shutdown signal received, stopping pipe server");
                Ok(())
            }
        };

        usn_task.abort();
        persist_task.abort();
        snapshot_task.abort();
        folder_rescan_task.abort();
        engine.stop_content_worker();
        engine.stop_property_extraction();

        info!("Saving all volume indexes to disk on service shutdown...");
        match engine.save_all_indexes() {
            Ok(()) => info!("All volume indexes saved successfully."),
            Err(e) => error!("Failed to save indexes on shutdown: {}", e),
        }
        served
    }
}

/// Loads `service.toml` unless the SPEC SET-02/SVC-05 check found it under the control of accounts that must
/// not control it, or unless it cannot be parsed. Falling back to the shipped defaults is logged
/// either way: a silently wrong configuration is worse than a default one.
fn load_configuration(layout: &ServiceDataLayout) -> ServiceConfig {
    if layout.config_untrusted {
        return ServiceConfig::default();
    }
    if !layout.config_path.exists() {
        return ServiceConfig::default();
    }
    match ServiceConfig::load(&layout.config_path) {
        Ok(config) => config,
        Err(err) => {
            error!(
                "{} could not be parsed ({err}); starting with defaults",
                layout.config_path.display()
            );
            ServiceConfig::default()
        }
    }
}

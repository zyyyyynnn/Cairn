// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Service configuration reads, hot apply and secure persistence.

use super::{content_pipeline, MultiVolumeEngine};
use crate::config::ServiceConfig;
use cairn_protocol::status::ContentIndexState;
use std::sync::atomic::Ordering;

impl MultiVolumeEngine {
    /// Returns the current service configuration.
    pub fn get_config(&self) -> ServiceConfig {
        self.config.read().unwrap().clone()
    }

    /// `true` when the service must answer queries from administrators only (SPEC SVC-02, SVC-05).
    /// Read per request, so a configuration change takes effect without a new pipe instance.
    pub fn admin_only_query(&self) -> bool {
        self.config.read().unwrap().admin_only_query
    }

    /// Updates service configuration dynamically (IDX-09 hot apply).
    pub fn update_config(&self, new_config: ServiceConfig) {
        let previous_enabled = self.config.read().unwrap().content_index_enabled;
        let next_enabled = new_config.content_index_enabled;
        {
            let mut cfg = self.config.write().unwrap();
            *cfg = new_config.clone();
        }

        if previous_enabled != next_enabled {
            if next_enabled {
                self.content_cancel
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                self.set_content_progress(0, 0);
                self.send_content_command(content_pipeline::ContentCommand::Start);
            } else {
                self.content_cancel
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                self.set_content_state(ContentIndexState::Disabled);
                self.send_content_command(content_pipeline::ContentCommand::Cancel);
            }
        } else if next_enabled
            && matches!(
                self.content_index_status().state,
                ContentIndexState::Error(_)
            )
        {
            // Saving the same enabled value is the explicit retry path: the previous run failed,
            // and the user asked again. A full backfill clears the partial index first.
            self.content_cancel
                .store(false, std::sync::atomic::Ordering::SeqCst);
            self.set_content_progress(0, 0);
            self.send_content_command(content_pipeline::ContentCommand::Start);
        }

        // Remove any volumes that are now excluded so they don't consume memory (IDX-01)
        let mut lock = self.volumes.write().unwrap();
        lock.retain(|&d, _| {
            !new_config
                .excluded_drives
                .iter()
                .any(|&ex| ex.eq_ignore_ascii_case(&d))
        });
    }

    /// Reloads configuration from the service configuration file and applies it (SVC-05, IDX-09).
    pub fn reload_config(&self) -> Result<(), String> {
        let path = self.config_path.clone();
        self.reload_config_from(&path)
    }

    /// Reloads configuration from an explicit path (IDX-09). A missing file resets the engine to the
    /// shipped defaults instead of keeping a configuration nobody can account for.
    pub fn reload_config_from(&self, path: &std::path::Path) -> Result<(), String> {
        if !self.config_trusted.load(Ordering::SeqCst) {
            return Err(
                "the service configuration was not trusted at startup; submit a new configuration"
                    .to_string(),
            );
        }
        if path.exists() {
            let new_cfg = ServiceConfig::load(path)
                .map_err(|e| format!("Failed to load config from {}: {}", path.display(), e))?;
            self.update_config(new_cfg);
        } else {
            self.update_config(ServiceConfig::default());
        }
        Ok(())
    }

    /// Applies a configuration submitted by a privileged caller: validate, persist with the
    /// SYSTEM/Administrators descriptor required by SET-02, then hot-apply (SVC-05, IDX-09).
    ///
    /// Nothing is applied unless the file could be written securely, so a configuration that a
    /// standard account could still edit never reaches the running index.
    pub fn apply_config_toml(&self, config_toml: &str) -> Result<(), String> {
        let parsed: ServiceConfig = toml::from_str(config_toml)
            .map_err(|e| format!("invalid service configuration: {e}"))?;
        let path = self.config_path.clone();
        parsed
            .save_atomic_secure(&path)
            .map_err(|e| format!("could not persist {}: {e}", path.display()))?;
        self.config_trusted.store(true, Ordering::SeqCst);
        self.update_config(parsed);
        Ok(())
    }
}

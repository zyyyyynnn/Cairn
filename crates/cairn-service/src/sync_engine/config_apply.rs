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

    /// Applies one configuration change: the new value is built from the live configuration,
    /// optionally persisted, and installed under one configuration write. Applies are serialized
    /// from start to finish, so the content-pipeline transition a change triggers runs in the
    /// same order as the assignment that caused it.
    ///
    /// With `persist`, nothing is installed unless the file was written securely, so a
    /// configuration a standard account could still edit never reaches the running index.
    pub(super) fn apply_config_change(
        &self,
        persist: bool,
        build: impl FnOnce(&ServiceConfig) -> Result<ServiceConfig, String>,
    ) -> Result<(), String> {
        let _serialized = self.config_apply_lock.lock().unwrap();
        let path = self.config_path.clone();
        let (previous_enabled, applied) = {
            let mut cfg = self.config.write().unwrap();
            let next = build(&cfg)?;
            if persist {
                next.save_atomic_secure(&path)
                    .map_err(|e| format!("could not persist {}: {e}", path.display()))?;
            }
            let previous_enabled = cfg.content_index_enabled;
            *cfg = next.clone();
            (previous_enabled, next)
        };
        self.config_trusted.store(true, Ordering::SeqCst);
        self.apply_content_switch(previous_enabled, applied.content_index_enabled);
        self.retain_indexed_volumes(&applied);
        Ok(())
    }

    /// Updates service configuration dynamically without persisting it (IDX-09 hot apply).
    pub fn update_config(&self, new_config: ServiceConfig) {
        let _ = self.apply_config_change(false, |_| Ok(new_config.clone()));
    }

    /// Moves the content pipeline when the master switch changed, and retries a failed run when
    /// the same enabled value is submitted again. Runs outside the configuration lock: the
    /// content thread reads that configuration when it handles the command.
    fn apply_content_switch(&self, previous_enabled: bool, next_enabled: bool) {
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
    }

    /// Drops volumes whose drive is no longer indexed (IDX-01).
    fn retain_indexed_volumes(&self, cfg: &ServiceConfig) {
        let mut lock = self.volumes.write().unwrap();
        lock.retain(|&d, _| {
            !cfg.excluded_drives
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
    /// shipped defaults instead of keeping a configuration nobody can account for. The file is read
    /// under the same serialized apply, so a value submitted at the same time is not overwritten by
    /// a copy read before it; a reload never rewrites the file.
    pub fn reload_config_from(&self, path: &std::path::Path) -> Result<(), String> {
        if !self.config_trusted.load(Ordering::SeqCst) {
            return Err(
                "the service configuration was not trusted at startup; submit a new configuration"
                    .to_string(),
            );
        }
        self.apply_config_change(false, |_| {
            if path.exists() {
                ServiceConfig::load(path)
                    .map_err(|e| format!("Failed to load config from {}: {}", path.display(), e))
            } else {
                Ok(ServiceConfig::default())
            }
        })
    }

    /// Applies a configuration submitted by a caller: validate, persist with the
    /// SYSTEM/Administrators descriptor required by SET-02, then hot-apply (SVC-05, IDX-09).
    ///
    /// The admin-only comparison, the protected write and the in-memory update happen under one
    /// serialized configuration write, so a concurrent submission is decided against the value
    /// that is actually current. `allow_admin_only_query_change` carries whether the caller may
    /// change that switch; keeping the current value is not a change.
    pub fn apply_config_toml(
        &self,
        config_toml: &str,
        allow_admin_only_query_change: bool,
    ) -> Result<(), String> {
        let parsed: ServiceConfig = toml::from_str(config_toml)
            .map_err(|e| format!("invalid service configuration: {e}"))?;
        self.apply_config_change(true, |current| {
            check_admin_only_query_change(current, &parsed, allow_admin_only_query_change)?;
            Ok(parsed.clone())
        })
    }
}

/// The administrators-only query mode decides who may query at all, so only an administrator may
/// change it (SPEC SVC-05); a submission that keeps the current value is not a change.
fn check_admin_only_query_change(
    current: &ServiceConfig,
    submitted: &ServiceConfig,
    allow_change: bool,
) -> Result<(), String> {
    if !allow_change && current.admin_only_query != submitted.admin_only_query {
        return Err("admin_only_query can only be changed by an administrator".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_admin_only_query_change;
    use crate::config::ServiceConfig;

    fn config(admin_only_query: bool) -> ServiceConfig {
        ServiceConfig {
            admin_only_query,
            ..Default::default()
        }
    }

    #[test]
    fn only_an_administrator_may_change_the_administrators_only_query_mode() {
        let off = config(false);
        let on = config(true);
        assert!(check_admin_only_query_change(&off, &on, false)
            .expect_err("a standard user must not switch the mode on")
            .contains("administrator"));
        assert!(check_admin_only_query_change(&on, &off, false)
            .expect_err("a standard user must not switch the mode off")
            .contains("administrator"));
        check_admin_only_query_change(&off, &off, false)
            .expect("keeping the mode as it is is not a change");
        check_admin_only_query_change(&on, &on, false)
            .expect("keeping the mode as it is is not a change");
        check_admin_only_query_change(&off, &on, true).expect("an administrator may switch it on");
        check_admin_only_query_change(&on, &off, true).expect("an administrator may switch it off");
    }
}

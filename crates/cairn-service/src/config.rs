// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Service configuration and persistence.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// The service configuration inside the SPEC SET-02 data root.
pub fn service_config_path() -> std::path::PathBuf {
    crate::security::default_service_data_root().join("service.toml")
}

fn default_excluded_paths() -> Vec<String> {
    vec![
        r"$Recycle.Bin".to_string(),
        r"System Volume Information".to_string(),
    ]
}

/// System, application and dependency trees are not user documents. They account for most of the
/// full-text corpus (installed JS/JSON, package registries, browser caches), so they are excluded
/// from the content index by default while filename search still sees every one of them.
fn default_content_excluded_paths() -> Vec<String> {
    [
        "Windows",
        "Program Files",
        "Program Files (x86)",
        "ProgramData",
        "AppData",
        "node_modules",
        ".git",
        "target",
        ".cargo",
        ".rustup",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn default_content_index_extensions() -> Vec<String> {
    vec![
        "txt", "md", "rs", "py", "js", "ts", "c", "cpp", "h", "go", "java", "json", "toml", "yaml",
        "xml", "csv", "log", "docx", "xlsx", "pptx", "pdf",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub const MAX_CONTENT_INDEX_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn default_content_index_max_bytes() -> u64 {
    10 * 1024 * 1024
}

fn default_true() -> bool {
    true
}

fn default_folder_rescan_interval_secs() -> u64 {
    86_400
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(default)]
    pub excluded_drives: Vec<char>,
    #[serde(default = "default_excluded_paths")]
    pub excluded_paths: Vec<String>,
    #[serde(default = "default_content_excluded_paths")]
    pub content_excluded_paths: Vec<String>,
    #[serde(default)]
    pub content_index_enabled: bool,
    #[serde(default = "default_content_index_extensions")]
    pub content_index_extensions: Vec<String>,
    #[serde(default = "default_content_index_max_bytes")]
    pub content_index_max_bytes: u64,
    #[serde(default)]
    pub admin_only_query: bool,
    #[serde(default)]
    pub folder_sources: Vec<String>,
    #[serde(default)]
    pub property_index_enabled: bool,
    #[serde(default = "default_true")]
    pub index_journal_enabled: bool,
    #[serde(default = "default_folder_rescan_interval_secs")]
    pub folder_rescan_interval_secs: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            excluded_drives: Vec::new(),
            excluded_paths: default_excluded_paths(),
            content_excluded_paths: default_content_excluded_paths(),
            content_index_enabled: false,
            content_index_extensions: default_content_index_extensions(),
            content_index_max_bytes: default_content_index_max_bytes(),
            admin_only_query: false,
            folder_sources: Vec::new(),
            property_index_enabled: false,
            index_journal_enabled: true,
            folder_rescan_interval_secs: default_folder_rescan_interval_secs(),
        }
    }
}

impl ServiceConfig {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path.as_ref())?;
        let cfg: ServiceConfig = toml::from_str(&content)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.content_index_max_bytes > MAX_CONTENT_INDEX_MAX_BYTES {
            return Err(format!(
                "content_index_max_bytes must not exceed {MAX_CONTENT_INDEX_MAX_BYTES} bytes"
            ));
        }
        Ok(())
    }

    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Self {
        Self::load(path).unwrap_or_default()
    }

    pub fn save_atomic<P: AsRef<Path>>(&self, path: P) -> Result<(), std::io::Error> {
        let path = path.as_ref();
        let serialized = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::persistence::atomic_write_bytes(path, serialized.as_bytes())
    }

    /// Writes the privileged service configuration (SPEC SET-02): the file is replaced
    /// atomically and its descriptor is then restricted to SYSTEM and Administrators, so
    /// `service.toml` never exists with an inherited, user-writable ACL that a standard account
    /// could edit before the service reads it.
    pub fn save_atomic_secure<P: AsRef<Path>>(&self, path: P) -> Result<(), std::io::Error> {
        let path = path.as_ref();
        self.save_atomic(path)?;
        crate::security::enforce_restricted_acl(path)
            .map_err(|err| std::io::Error::other(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_index_size_limit_accepts_the_boundary_and_rejects_larger_values() {
        let cfg = ServiceConfig {
            content_index_max_bytes: MAX_CONTENT_INDEX_MAX_BYTES,
            ..ServiceConfig::default()
        };
        assert!(cfg.validate().is_ok());
        let too_large = ServiceConfig {
            content_index_max_bytes: MAX_CONTENT_INDEX_MAX_BYTES + 1,
            ..cfg
        };
        assert!(too_large.validate().is_err());
    }

    #[test]
    fn test_service_config_load_and_error_handling() {
        let temp = tempfile::tempdir().unwrap();
        let valid_path = temp.path().join("service.toml");
        let invalid_path = temp.path().join("invalid.toml");
        let missing_path = temp.path().join("missing.toml");

        let cfg = ServiceConfig {
            admin_only_query: true,
            content_index_enabled: true,
            ..Default::default()
        };
        cfg.save_atomic(&valid_path).unwrap();

        let loaded = ServiceConfig::load(&valid_path).unwrap();
        assert!(loaded.admin_only_query);
        assert!(loaded.content_index_enabled);

        fs::write(&invalid_path, "invalid toml syntax [[[[").unwrap();
        assert!(ServiceConfig::load(&invalid_path).is_err());
        assert!(ServiceConfig::load(&missing_path).is_err());

        let def = ServiceConfig::load_or_default(&missing_path);
        assert!(!def.admin_only_query);

        std::thread::scope(|scope| {
            for idx in 0..6 {
                let path_ref = &valid_path;
                scope.spawn(move || {
                    let next = ServiceConfig {
                        admin_only_query: idx % 2 == 0,
                        content_index_enabled: idx % 2 == 1,
                        ..Default::default()
                    };
                    next.save_atomic(path_ref).unwrap();
                });
            }
        });
        assert!(ServiceConfig::load(&valid_path).is_ok());
        let leftover_temps: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover_temps.is_empty());
    }
}

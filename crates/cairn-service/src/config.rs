// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Service configuration and persistence.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const SERVICE_CONFIG_FILE: &str = r"C:\ProgramData\Cairn\service.toml";

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

fn default_content_index_max_bytes() -> u64 {
    10 * 1024 * 1024 // 10MB
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
        }
    }
}

impl ServiceConfig {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path.as_ref())?;
        let cfg = toml::from_str(&content)?;
        Ok(cfg)
    }

    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Self {
        Self::load(path).unwrap_or_default()
    }

    pub fn save_atomic<P: AsRef<Path>>(&self, path: P) -> Result<(), std::io::Error> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let serialized = toml::to_string_pretty(self).map_err(std::io::Error::other)?;

        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, serialized)?;
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        fs::rename(&tmp_path, path)?;

        Ok(())
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

        // load_or_default falls back gracefully
        let def = ServiceConfig::load_or_default(&missing_path);
        assert!(!def.admin_only_query);
    }
}

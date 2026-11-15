// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Manual in-app updates: read the latest stable GitHub release, download the installer and its
//! checksum, verify it, then hand off to the silent installer. The installer restarts Cairn; the
//! target version is kept in LocalAppData so the next launch can report success or failure.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::CairnApp;
use crate::service::elevation::{
    launch_elevated_installer, lock_verified_installer, ElevationError,
};
use crate::service::ipc_tasks::get_tokio_runtime;
use crate::shell::toast::ToastAction;
use fluent_bundle::FluentArgs;
use gpui_kit::gpui::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) const REPOSITORY_URL: &str = "https://github.com/zyyyyynnn/Cairn";
const RELEASES_API: &str = "https://api.github.com/repos/zyyyyynnn/Cairn/releases/latest";
const SETUP_ASSET: &str = "CairnSetup.exe";
const SUMS_ASSET: &str = "SHA256SUMS.txt";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Debug builds may resolve a lower current version, so the available-update toast can be
/// inspected without publishing a test release. Release builds always use the package version.
fn current_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(version) = std::env::var("CAIRN_UPDATE_FAKE_VERSION") {
        let version = version.trim();
        if !version.is_empty() {
            return version.to_string();
        }
    }
    CURRENT_VERSION.to_string()
}
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const PENDING_VERSION_FILE: &str = "update-target.txt";

#[derive(Clone, Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Clone, Debug)]
struct ReleaseInfo {
    version: String,
    setup_url: String,
    sums_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateError {
    Network,
    Response,
    Data,
    Incomplete,
    Download,
    Verify,
}

fn client_with_timeout(timeout: Duration) -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .user_agent(format!("Cairn/{}", current_version()))
        .timeout(timeout)
        .build()
        .map_err(|_| UpdateError::Network)
}

fn client() -> Result<reqwest::Client, UpdateError> {
    client_with_timeout(CHECK_TIMEOUT)
}

async fn check_latest() -> Result<Option<ReleaseInfo>, UpdateError> {
    let response = client()?
        .get(RELEASES_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| UpdateError::Network)?;

    match response.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        _ => return Err(UpdateError::Response),
    }

    let body = response.text().await.map_err(|_| UpdateError::Network)?;
    let release: GitHubRelease = serde_json::from_str(&body).map_err(|_| UpdateError::Data)?;
    let version = normalize_version(&release.tag_name);
    if version.is_empty() {
        return Err(UpdateError::Data);
    }
    let setup = release
        .assets
        .iter()
        .find(|asset| asset.name == SETUP_ASSET)
        .ok_or(UpdateError::Incomplete)?;
    let sums = release
        .assets
        .iter()
        .find(|asset| asset.name == SUMS_ASSET)
        .ok_or(UpdateError::Incomplete)?;
    Ok(Some(ReleaseInfo {
        version,
        setup_url: setup.browser_download_url.clone(),
        sums_url: sums.browser_download_url.clone(),
    }))
}

async fn download_and_verify(release: &ReleaseInfo) -> Result<(PathBuf, String), UpdateError> {
    let client = client_with_timeout(DOWNLOAD_TIMEOUT)?;
    let sums = client
        .get(&release.sums_url)
        .send()
        .await
        .map_err(|_| UpdateError::Download)?
        .error_for_status()
        .map_err(|_| UpdateError::Download)?
        .text()
        .await
        .map_err(|_| UpdateError::Download)?;
    let expected = parse_checksum(&sums, SETUP_ASSET).ok_or(UpdateError::Data)?;

    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|_| UpdateError::Download)?;
    let setup_path = dir.join(format!(
        "{}-{}.exe",
        SETUP_ASSET.trim_end_matches(".exe"),
        safe_version(&release.version)
    ));

    let result = async {
        let mut response = client
            .get(&release.setup_url)
            .send()
            .await
            .map_err(|_| UpdateError::Download)?
            .error_for_status()
            .map_err(|_| UpdateError::Download)?;
        let mut file = tokio::fs::File::create(&setup_path)
            .await
            .map_err(|_| UpdateError::Download)?;
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UpdateError::Download)? {
            hasher.update(&chunk);
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|_| UpdateError::Download)?;
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|_| UpdateError::Download)?;

        let actual: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if !actual.eq_ignore_ascii_case(&expected) {
            return Err(UpdateError::Verify);
        }
        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = std::fs::remove_file(&setup_path);
    }
    result.map(|()| (setup_path, expected))
}

fn normalize_version(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).to_string()
}

fn safe_version(version: &str) -> String {
    version
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        .collect()
}

fn version_parts(version: &str) -> Vec<u64> {
    normalize_version(version)
        .split(['.', '-', '+'])
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let left = version_parts(candidate);
    let right = version_parts(current);
    let length = left.len().max(right.len());
    for index in 0..length {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

fn parse_checksum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (name == file).then(|| hash.to_string())
    })
}

fn updates_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Cairn")
        .join("updates")
}

fn pending_version_path() -> PathBuf {
    updates_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join(PENDING_VERSION_FILE)
}

fn write_pending_version(version: &str) {
    let path = pending_version_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, version);
}

fn remove_pending_version() {
    let _ = std::fs::remove_file(pending_version_path());
}

#[cfg(not(test))]
fn take_pending_update_target() -> Option<String> {
    let path = pending_version_path();
    let target = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(path);
    let target = normalize_version(&target);
    if target.is_empty() {
        None
    } else {
        Some(target)
    }
}

impl CairnApp {
    /// Schedules the startup update marker read off the UI thread so the first frame paints
    /// immediately; if no update toast applies and the global hotkey failed to bind, shows the
    /// hotkey notice in the shared toast slot.
    #[cfg(not(test))]
    pub(crate) fn schedule_startup_notices(
        &mut self,
        hotkey_unavailable: bool,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        get_tokio_runtime().spawn_blocking(move || {
            let _ = tx.send(take_pending_update_target());
        });
        cx.spawn(async move |this, cx| {
            let target = rx.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                let notice_shown = match target {
                    Some(target) => this.show_update_notice_for(&target, cx),
                    None => false,
                };
                if hotkey_unavailable && !notice_shown {
                    let title = this.i18n.get_text("hotkey_unavailable_title");
                    let message = this.i18n.get_text("hotkey_unavailable_message");
                    this.show_info_toast(title, message, cx);
                }
            });
        })
        .detach();
    }

    #[cfg(not(test))]
    fn show_update_notice_for(&mut self, target: &str, cx: &mut Context<Self>) -> bool {
        if target.is_empty() {
            return false;
        }
        if is_newer(target, &current_version()) {
            let title = self.i18n.get_text("update_interrupted_title");
            let body = self.i18n.get_text("update_interrupted_body");
            self.show_error_toast(title, body, cx);
        } else {
            let title = self.i18n.get_text("update_success_title");
            let mut args = FluentArgs::new();
            args.set("version", target);
            let body = self.i18n.get_text_with_args("update_success_body", &args);
            self.show_info_toast(title, body, cx);
        }
        true
    }

    pub(crate) fn start_update_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_checking_update {
            return;
        }
        self.is_checking_update = true;
        let checking_title = self.i18n.get_text("update_checking_title");
        let checking_body = self.i18n.get_text("update_checking_body");
        self.show_persistent_info_toast(checking_title, checking_body, cx);

        let handle = window.window_handle();
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        get_tokio_runtime().spawn(async move {
            let _ = tx.send(check_latest().await);
        });
        cx.spawn(async move |_this, cx| {
            let result = rx.await.unwrap_or(Err(UpdateError::Network));
            let _ = cx.update_window(handle, |_, _window, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        this.finish_update_check(cx, result);
                    });
                }
            });
        })
        .detach();
    }

    fn finish_update_check(
        &mut self,
        cx: &mut Context<Self>,
        result: Result<Option<ReleaseInfo>, UpdateError>,
    ) {
        self.is_checking_update = false;
        cx.notify();
        match result {
            Err(UpdateError::Incomplete) => {
                let title = self.i18n.get_text("update_incomplete_title");
                let body = self.i18n.get_text("update_incomplete_body");
                let retry =
                    ToastAction::new(self.i18n.get_text("update_retry"), |this, window, cx| {
                        this.start_update_check(window, cx);
                    });
                self.show_error_toast_with_action(title, body, retry, cx);
            }
            Err(_) => {
                let title = self.i18n.get_text("update_check_failed_title");
                let body = self.i18n.get_text("update_check_failed_body");
                let retry =
                    ToastAction::new(self.i18n.get_text("update_retry"), |this, window, cx| {
                        this.start_update_check(window, cx);
                    });
                self.show_error_toast_with_action(title, body, retry, cx);
            }
            Ok(None) => {
                let title = self.i18n.get_text("update_up_to_date_title");
                let body = self.i18n.get_text("update_no_releases_body");
                self.show_info_toast(title, body, cx);
            }
            Ok(Some(release)) if is_newer(&release.version, &current_version()) => {
                let title = self.i18n.get_text("update_available_title");
                let current = current_version();
                let mut body_args = version_args(&release.version);
                body_args.set("current", current.as_str());
                let body = self
                    .i18n
                    .get_text_with_args("update_available_body", &body_args);
                let action_label = self.i18n.get_text("update_download_action");
                let action = ToastAction::labeled(action_label, move |this, window, cx| {
                    this.start_update_download(window, cx, release.clone());
                });
                self.show_info_toast_with_action(title, body, action, cx);
            }
            Ok(Some(release)) => {
                let current = current_version();
                let mut args = version_args(&release.version);
                args.set("current", current.as_str());
                let title = self.i18n.get_text("update_up_to_date_title");
                let body = self
                    .i18n
                    .get_text_with_args("update_up_to_date_body", &args);
                self.show_info_toast(title, body, cx);
            }
        }
    }

    fn start_update_download(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        release: ReleaseInfo,
    ) {
        let title = self.i18n.get_text("update_installing_title");
        let body = self.i18n.get_text("update_installing_body");
        self.show_persistent_info_toast(title, body, cx);

        let handle = window.window_handle();
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task_release = release.clone();
        get_tokio_runtime().spawn(async move {
            let _ = tx.send(download_and_verify(&task_release).await);
        });
        cx.spawn(async move |_this, cx| {
            let result = rx.await.unwrap_or(Err(UpdateError::Download));
            let _ = cx.update_window(handle, |_, _window, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        this.finish_update_download(cx, release.clone(), result);
                    });
                }
            });
        })
        .detach();
    }

    fn finish_update_download(
        &mut self,
        cx: &mut Context<Self>,
        release: ReleaseInfo,
        result: Result<(PathBuf, String), UpdateError>,
    ) {
        match result {
            Ok((path, expected)) => self.install_downloaded(cx, release, path, expected),
            Err(UpdateError::Verify) => {
                let title = self.i18n.get_text("update_verify_failed_title");
                let body = self.i18n.get_text("update_verify_failed_body");
                self.show_download_retry_toast(title, body, release, cx);
            }
            Err(_) => {
                let title = self.i18n.get_text("update_download_failed_title");
                let body = self.i18n.get_text("update_download_failed_body");
                self.show_download_retry_toast(title, body, release, cx);
            }
        }
    }

    fn install_downloaded(
        &mut self,
        cx: &mut Context<Self>,
        release: ReleaseInfo,
        path: PathBuf,
        expected_sha256: String,
    ) {
        let installer = match lock_verified_installer(&path, &expected_sha256) {
            Ok(installer) => installer,
            Err(error) => {
                tracing::warn!("downloaded installer changed after verification: {error}");
                let _ = std::fs::remove_file(&path);
                let title = self.i18n.get_text("update_verify_failed_title");
                let body = self.i18n.get_text("update_verify_failed_body");
                self.show_download_retry_toast(title, body, release, cx);
                return;
            }
        };
        write_pending_version(&release.version);
        match launch_elevated_installer(&path, &expected_sha256) {
            Ok(()) => {
                drop(installer);
                std::process::exit(0);
            }
            Err(ElevationError::Cancelled) => {
                drop(installer);
                remove_pending_version();
                let title = self.i18n.get_text("update_cancelled_title");
                let body = self.i18n.get_text("update_cancelled_body");
                let retry =
                    ToastAction::labeled(self.i18n.get_text("update_retry"), move |this, _, cx| {
                        this.install_downloaded(
                            cx,
                            release.clone(),
                            path.clone(),
                            expected_sha256.clone(),
                        );
                    });
                self.show_info_toast_with_action(title, body, retry, cx);
            }
            Err(ElevationError::Failed { message, .. }) => {
                tracing::warn!("update installer could not start: {message}");
                drop(installer);
                remove_pending_version();
                let title = self.i18n.get_text("update_install_failed_title");
                let body = self.i18n.get_text("update_install_failed_body");
                let retry =
                    ToastAction::new(self.i18n.get_text("update_retry"), move |this, _, cx| {
                        this.install_downloaded(
                            cx,
                            release.clone(),
                            path.clone(),
                            expected_sha256.clone(),
                        );
                    });
                self.show_error_toast_with_action(title, body, retry, cx);
            }
        }
    }

    fn show_download_retry_toast(
        &mut self,
        title: String,
        body: String,
        release: ReleaseInfo,
        cx: &mut Context<Self>,
    ) {
        let retry = ToastAction::new(
            self.i18n.get_text("update_retry"),
            move |this, window, cx| {
                this.start_update_download(window, cx, release.clone());
            },
        );
        self.show_error_toast_with_action(title, body, retry, cx);
    }
}

fn version_args(version: &str) -> FluentArgs<'_> {
    let mut args = FluentArgs::new();
    args.set("version", version);
    args
}

#[cfg(test)]
mod tests {
    use super::{
        check_latest, is_newer, normalize_version, parse_checksum, safe_version, GitHubRelease,
    };
    use crate::shell::i18n::I18nManager;
    use fluent_bundle::FluentArgs;

    #[test]
    fn release_tags_compare_numerically_not_lexically() {
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("0.2", "0.1.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert_eq!(normalize_version(" V1.2.3 "), "1.2.3");
    }

    #[test]
    fn the_available_update_title_carries_no_version() {
        for language in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(language);
            let title = i18n.get_text("update_available_title");
            assert!(!title.contains('{'), "{language}: {title}");
            assert!(!title.contains("9.9.9"), "{language}: {title}");

            let mut args = FluentArgs::new();
            args.set("version", "9.9.9");
            args.set("current", "0.1.0");
            let body = i18n.get_text_with_args("update_available_body", &args);
            assert!(body.contains("9.9.9"), "{language}: {body}");
            assert!(body.contains("0.1.0"), "{language}: {body}");
        }
    }

    #[test]
    fn checksum_parser_reads_the_installer_line_only() {
        let sums = "abc  cairn.msi\ndef  CairnSetup.exe\nghi  SHA256SUMS.txt\n";
        assert_eq!(parse_checksum(sums, "CairnSetup.exe"), Some("def".into()));
        assert_eq!(parse_checksum(sums, "CairnCli.exe"), None);
    }

    #[test]
    fn asset_versions_cannot_escape_the_update_directory() {
        assert_eq!(safe_version("v1.2.3/../x"), "v1.2.3..x");
        assert!(safe_version("0.2.0").starts_with("0.2.0"));
    }

    #[test]
    fn a_github_release_deserializes_with_its_assets() {
        let body = r#"{
            "tag_name":"v0.2.0",
            "html_url":"https://example.invalid/release",
            "assets":[
                {"name":"CairnSetup.exe","browser_download_url":"https://example.invalid/CairnSetup.exe"},
                {"name":"SHA256SUMS.txt","browser_download_url":"https://example.invalid/SHA256SUMS.txt"}
            ]
        }"#;
        let release: GitHubRelease = serde_json::from_str(body).expect("valid release");
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
    }

    /// Network check for HTTP/TLS dependency changes: the real release API must answer through
    /// the installed crypto provider.
    #[tokio::test]
    #[ignore = "requires network access to the GitHub releases API"]
    async fn the_update_check_reaches_the_release_api() {
        let release = check_latest().await.expect("the release API answers");
        assert!(release.is_some(), "the repository publishes releases");
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Manual in-app updates: read the latest stable GitHub release, download the installer and its
//! checksum, verify it, then hand off to the silent installer. The installer restarts Cairn; the
//! target version is kept in `LocalAppData` so the next launch can report success or failure.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::CairnApp;
use crate::service::elevation::{
    ElevationError, launch_elevated_installer, lock_verified_installer,
};
use crate::service::ipc_tasks::get_tokio_runtime;
use crate::shell::command_groups::MenuGroup;
use fluent_bundle::FluentArgs;
use gpui_kit::gpui::*;
use semver::Version;
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
const MAX_API_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_SUMS_BYTES: usize = 1024 * 1024;
const MAX_SETUP_BYTES: u64 = 256 * 1024 * 1024;
const PENDING_VERSION_FILE: &str = "update-target.txt";
const TRUSTED_API_HOSTS: &[&str] = &["api.github.com"];
const TRUSTED_RELEASE_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

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
pub(crate) struct ReleaseInfo {
    pub(crate) version: String,
    pub(crate) setup_url: String,
    pub(crate) sums_url: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum UpdateState {
    #[default]
    Idle,
    Checking,
    Available(ReleaseInfo),
    Downloading(ReleaseInfo),
}

impl UpdateState {
    pub(crate) fn is_checking(&self) -> bool {
        matches!(self, Self::Checking)
    }

    pub(crate) fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Downloading(_))
    }
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

fn validate_trusted_url(url: &str, allowed_hosts: &[&str]) -> Result<reqwest::Url, UpdateError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| UpdateError::Data)?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
    {
        return Err(UpdateError::Data);
    }
    let host = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or(UpdateError::Data)?;
    if !allowed_hosts
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
    {
        return Err(UpdateError::Data);
    }
    Ok(parsed)
}

fn validate_release_url(url: &str) -> Result<reqwest::Url, UpdateError> {
    validate_trusted_url(url, TRUSTED_RELEASE_HOSTS)
}

fn host_is_trusted(url: &reqwest::Url, allowed_hosts: &[&str]) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            allowed_hosts
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed))
        })
}

fn client_with_timeout(
    timeout: Duration,
    allowed_hosts: &'static [&'static str],
) -> Result<reqwest::Client, UpdateError> {
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 8 {
            attempt.error("too many update redirects")
        } else if !host_is_trusted(attempt.url(), allowed_hosts) {
            attempt.error("untrusted update redirect target")
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .user_agent(format!("Cairn/{}", current_version()))
        .timeout(timeout)
        .redirect(policy)
        .build()
        .map_err(|_| UpdateError::Network)
}

fn api_client() -> Result<reqwest::Client, UpdateError> {
    client_with_timeout(CHECK_TIMEOUT, TRUSTED_API_HOSTS)
}

async fn read_bounded_text(
    mut response: reqwest::Response,
    max_bytes: usize,
    size_error: UpdateError,
) -> Result<String, UpdateError> {
    if response
        .content_length()
        .is_some_and(|len| len > max_bytes as u64)
    {
        return Err(size_error);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| size_error)? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(size_error);
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| UpdateError::Data)
}

async fn check_latest() -> Result<Option<ReleaseInfo>, UpdateError> {
    let _ = validate_trusted_url(RELEASES_API, TRUSTED_API_HOSTS)?;
    let response = api_client()?
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

    let body = read_bounded_text(response, MAX_API_RESPONSE_BYTES, UpdateError::Network).await?;
    release_from_json(&body).map(Some)
}

fn release_from_json(body: &str) -> Result<ReleaseInfo, UpdateError> {
    let release: GitHubRelease = serde_json::from_str(body).map_err(|_| UpdateError::Data)?;
    let version = normalize_version(&release.tag_name);
    if Version::parse(&version).is_err() {
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
    let setup_url = validate_release_url(&setup.browser_download_url)?;
    let sums_url = validate_release_url(&sums.browser_download_url)?;
    Ok(ReleaseInfo {
        version,
        setup_url: setup_url.into(),
        sums_url: sums_url.into(),
    })
}

async fn download_and_verify(release: &ReleaseInfo) -> Result<(PathBuf, String), UpdateError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::AsyncWriteExt as _;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let sums_url = validate_release_url(&release.sums_url).map_err(|_| UpdateError::Download)?;
    let setup_url = validate_release_url(&release.setup_url).map_err(|_| UpdateError::Download)?;
    let client = client_with_timeout(DOWNLOAD_TIMEOUT, TRUSTED_RELEASE_HOSTS)?;
    let sums_response = client
        .get(sums_url)
        .send()
        .await
        .map_err(|_| UpdateError::Download)?
        .error_for_status()
        .map_err(|_| UpdateError::Download)?;
    let sums = read_bounded_text(sums_response, MAX_SUMS_BYTES, UpdateError::Download).await?;
    let expected = parse_checksum(&sums, SETUP_ASSET).ok_or(UpdateError::Data)?;

    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|_| UpdateError::Download)?;
    let stem = format!(
        "{}-{}",
        SETUP_ASSET.trim_end_matches(".exe"),
        safe_version(&release.version)
    );
    let setup_path = dir.join(format!("{stem}.exe"));

    let (mut file, temp_path) = loop {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(format!("{stem}.{}.{seq}.tmp", std::process::id()));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            Ok(file) => break (file, candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(UpdateError::Download),
        }
    };

    let result = async {
        let mut response = client
            .get(setup_url)
            .send()
            .await
            .map_err(|_| UpdateError::Download)?
            .error_for_status()
            .map_err(|_| UpdateError::Download)?;
        if response
            .content_length()
            .is_some_and(|len| len > MAX_SETUP_BYTES)
        {
            return Err(UpdateError::Download);
        }
        let mut downloaded_bytes = 0u64;
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UpdateError::Download)? {
            downloaded_bytes = downloaded_bytes
                .checked_add(chunk.len() as u64)
                .filter(|&len| len <= MAX_SETUP_BYTES)
                .ok_or(UpdateError::Download)?;
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| UpdateError::Download)?;
        }
        file.flush().await.map_err(|_| UpdateError::Download)?;
        file.sync_all().await.map_err(|_| UpdateError::Download)?;

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

    drop(file);
    if let Err(err) = result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    if crate::shell::os::atomic_replace_file(&temp_path, &setup_path).is_err() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(UpdateError::Download);
    }
    Ok((setup_path, expected))
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

fn is_newer(candidate: &str, current: &str) -> bool {
    let (Ok(candidate), Ok(current)) = (
        Version::parse(&normalize_version(candidate)),
        Version::parse(&normalize_version(current)),
    ) else {
        return false;
    };
    candidate.cmp_precedence(&current) == std::cmp::Ordering::Greater
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
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("Cairn")
        .join("updates")
}

fn pending_version_path() -> PathBuf {
    updates_dir()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join(PENDING_VERSION_FILE)
}

fn write_pending_version(version: &str) {
    let path = pending_version_path();
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            "Failed to create pending-update directory {}: {err}",
            parent.display()
        );
        return;
    }
    if let Err(err) = std::fs::write(&path, version) {
        tracing::warn!(
            "Failed to write pending update marker {}: {err}",
            path.display()
        );
    }
}

fn remove_pending_version() {
    let _ = std::fs::remove_file(pending_version_path());
}

pub(crate) fn take_pending_update_target() -> Option<String> {
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
    pub(crate) fn schedule_startup_notices(
        &mut self,
        hotkey_unavailable: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let runtime_ports = self.runtime_ports.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        get_tokio_runtime().spawn_blocking(move || {
            let _ = tx.send(runtime_ports.take_pending_update_target());
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

    fn show_update_notice_for(&mut self, target: &str, cx: &mut Context<'_, Self>) -> bool {
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

    pub(crate) fn start_update_check(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match &self.update_state {
            UpdateState::Idle => self.start_update_check_with_menu(window, cx, true),
            UpdateState::Available(release) => {
                let release = release.clone();
                self.start_update_download(window, cx, release);
            }
            UpdateState::Checking | UpdateState::Downloading(_) => {}
        }
    }

    fn start_update_check_with_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        reopen_help: bool,
    ) {
        if !self.begin_update_check(window, cx, reopen_help) {
            return;
        }

        let handle = window.window_handle();
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        get_tokio_runtime().spawn(async move {
            let _ = tx.send(check_latest().await);
        });
        cx.spawn(async move |_this, cx| {
            let result = rx.await.unwrap_or(Err(UpdateError::Network));
            let _ = cx.update_window(handle, |_, window, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        this.finish_update_check(window, cx, result);
                    });
                }
            });
        })
        .detach();
    }

    /// Moves the shell into the checking state. The Help menu reopens after the click that started
    /// it: the popup dismisses itself on activation, so the row that carries the spinner has to be
    /// shown again. Returns `false` when the updater is not idle.
    fn begin_update_check(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        reopen_help: bool,
    ) -> bool {
        if !matches!(self.update_state, UpdateState::Idle) {
            return false;
        }
        self.update_state = UpdateState::Checking;
        cx.notify();

        if reopen_help {
            let menu_view = cx.entity();
            window.defer(cx, move |window, cx| {
                menu_view.update(cx, |this, cx| {
                    if this.update_state.is_checking() {
                        this.show_group(MenuGroup::Help, window, cx);
                    }
                });
            });
        }
        true
    }

    fn finish_update_check(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        result: Result<Option<ReleaseInfo>, UpdateError>,
    ) {
        if self
            .open_group_menu
            .as_ref()
            .is_some_and(|menu| menu.group == MenuGroup::Help)
        {
            self.close_group();
            if self.rename.draft.is_none() {
                self.query.search_focus.focus(window, cx);
            }
        }
        match result {
            Err(UpdateError::Incomplete) => {
                self.update_state = UpdateState::Idle;
                let title = self.i18n.get_text("update_incomplete_title");
                let body = self.i18n.get_text("update_incomplete_body");
                self.show_error_toast(title, body, cx);
            }
            Err(_) => {
                self.update_state = UpdateState::Idle;
                let title = self.i18n.get_text("update_check_failed_title");
                let body = self.i18n.get_text("update_check_failed_body");
                self.show_error_toast(title, body, cx);
            }
            Ok(None) => {
                self.update_state = UpdateState::Idle;
                let title = self.i18n.get_text("update_up_to_date_title");
                let body = self.i18n.get_text("update_no_releases_body");
                self.show_info_toast(title, body, cx);
            }
            Ok(Some(release)) if is_newer(&release.version, &current_version()) => {
                let title = self.i18n.get_text("update_available_title");
                let body_args = version_args(&release.version);
                let body = self
                    .i18n
                    .get_text_with_args("update_available_body", &body_args);
                self.update_state = UpdateState::Available(release);
                self.show_info_toast(title, body, cx);
            }
            Ok(Some(release)) => {
                self.update_state = UpdateState::Idle;
                let args = version_args(&release.version);
                let title = self.i18n.get_text("update_up_to_date_title");
                let body = self
                    .i18n
                    .get_text_with_args("update_up_to_date_body", &args);
                self.show_info_toast(title, body, cx);
            }
        }
        cx.notify();
    }

    fn start_update_download(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        release: ReleaseInfo,
    ) {
        let task_release = release.clone();
        self.update_state = UpdateState::Downloading(release);
        let title = self.i18n.get_text("update_installing_title");
        let body = self.i18n.get_text("update_installing_body");
        self.show_persistent_info_toast(title, body, cx);

        let handle = window.window_handle();
        let view = cx.entity().downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        get_tokio_runtime().spawn(async move {
            let _ = tx.send(download_and_verify(&task_release).await);
        });
        cx.spawn(async move |_this, cx| {
            let result = rx.await.unwrap_or(Err(UpdateError::Download));
            let _ = cx.update_window(handle, |_, _window, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        this.finish_update_download(cx, result);
                    });
                }
            });
        })
        .detach();
    }

    fn finish_update_download(
        &mut self,
        cx: &mut Context<'_, Self>,
        result: Result<(PathBuf, String), UpdateError>,
    ) {
        let UpdateState::Downloading(release) = std::mem::take(&mut self.update_state) else {
            return;
        };
        match result {
            Ok((path, expected)) => self.install_downloaded(cx, release, path, expected),
            Err(UpdateError::Verify) => {
                self.update_state = UpdateState::Available(release);
                let title = self.i18n.get_text("update_verify_failed_title");
                let body = self.i18n.get_text("update_verify_failed_body");
                self.show_error_toast(title, body, cx);
            }
            Err(_) => {
                self.update_state = UpdateState::Available(release);
                let title = self.i18n.get_text("update_download_failed_title");
                let body = self.i18n.get_text("update_download_failed_body");
                self.show_error_toast(title, body, cx);
            }
        }
    }

    fn install_downloaded(
        &mut self,
        cx: &mut Context<'_, Self>,
        release: ReleaseInfo,
        path: PathBuf,
        expected_sha256: String,
    ) {
        let installer = match lock_verified_installer(&path, &expected_sha256) {
            Ok(installer) => installer,
            Err(error) => {
                tracing::warn!("downloaded installer changed after verification: {error}");
                let _ = std::fs::remove_file(&path);
                self.update_state = UpdateState::Available(release);
                let title = self.i18n.get_text("update_verify_failed_title");
                let body = self.i18n.get_text("update_verify_failed_body");
                self.show_error_toast(title, body, cx);
                return;
            }
        };
        write_pending_version(&release.version);
        match launch_elevated_installer(&path, &expected_sha256) {
            Ok(()) => {
                self.update_state = UpdateState::Idle;
                drop(installer);
                std::process::exit(0);
            }
            Err(ElevationError::Cancelled) => {
                drop(installer);
                remove_pending_version();
                self.update_state = UpdateState::Available(release);
                let title = self.i18n.get_text("update_cancelled_title");
                let body = self.i18n.get_text("update_cancelled_body");
                self.show_info_toast(title, body, cx);
            }
            Err(ElevationError::Failed { message, .. }) => {
                tracing::warn!("update installer could not start: {message}");
                drop(installer);
                remove_pending_version();
                self.update_state = UpdateState::Available(release);
                let title = self.i18n.get_text("update_install_failed_title");
                let body = self.i18n.get_text("update_install_failed_body");
                self.show_error_toast(title, body, cx);
            }
        }
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
        UpdateError, check_latest, is_newer, normalize_version, parse_checksum, release_from_json,
        safe_version, validate_release_url,
    };
    use crate::shell::i18n::I18nManager;
    use fluent_bundle::FluentArgs;

    #[test]
    fn release_tags_compare_numerically_not_lexically() {
        let v = |parts: &[u64]| {
            parts
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(".")
        };
        assert!(is_newer(&format!("v{}", v(&[1, 1, 10])), &v(&[1, 1, 9])));
        assert!(is_newer(&format!("v{}", v(&[2, 0, 0])), &v(&[1, 9, 9])));
        let current = v(&[1, 1, 0]);
        assert!(!is_newer(&format!("v{current}"), &current));
        assert!(!is_newer(&v(&[0, 0, 9]), &current));
        assert!(!is_newer(&v(&[2]), &v(&[1, 9])));
        let stable = v(&[3, 0, 0]);
        assert!(!is_newer(&format!("{stable}+build.1"), &stable));
        assert!(!is_newer(&stable, &format!("{stable}+build.1")));
        assert!(is_newer(&stable, &format!("{stable}-beta.1")));
        assert!(!is_newer(&format!("{stable}-beta.1"), &stable));
        assert_eq!(
            normalize_version(&format!(" V{} ", v(&[1, 2, 3]))),
            v(&[1, 2, 3])
        );
    }

    #[test]
    fn the_available_update_title_carries_no_version() {
        let sample_version = {
            let parts = [9u64, 9, 9];
            parts
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(".")
        };
        for language in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(language);
            let title = i18n.get_text("update_available_title");
            assert!(!title.contains('{'), "{language}: {title}");
            assert!(!title.contains(&sample_version), "{language}: {title}");

            let mut args = FluentArgs::new();
            args.set("version", sample_version.clone());
            let body = i18n.get_text_with_args("update_available_body", &args);
            assert!(body.contains(&sample_version), "{language}: {body}");
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
        let version = format!("{}.{}.{}", 0, 2, 0);
        assert_eq!(safe_version("v1.2.3/../x"), "v1.2.3..x");
        assert!(safe_version(&version).starts_with(&version));
    }

    #[test]
    fn a_github_release_deserializes_with_its_assets() {
        let version = format!("{}.{}.{}", 0, 2, 0);
        let body = format!(
            r#"{{
                "tag_name":"v{version}",
                "html_url":"https://github.com/zyyyyynnn/Cairn/releases/tag/v{version}",
                "assets":[
                    {{"name":"CairnSetup.exe","browser_download_url":"https://github.com/zyyyyynnn/Cairn/releases/download/v{version}/CairnSetup.exe"}},
                    {{"name":"SHA256SUMS.txt","browser_download_url":"https://objects.githubusercontent.com/github-production-release-asset/SHA256SUMS.txt"}}
                ]
            }}"#
        );
        let release = release_from_json(&body).expect("valid release");
        assert_eq!(release.version, version);
        assert!(release.setup_url.ends_with("CairnSetup.exe"));
        assert!(release.sums_url.ends_with("SHA256SUMS.txt"));
    }

    #[test]
    fn release_tags_must_parse_as_semver_and_publish_both_assets() {
        let version = format!("{}.{}.{}", 0, 2, 0);
        let malformed_version = format!("v{}.{}", 0, 2);
        let malformed = format!(
            r#"{{
                "tag_name":"{malformed_version}",
                "assets":[
                    {{"name":"CairnSetup.exe","browser_download_url":"https://github.com/zyyyyynnn/Cairn/releases/download/{malformed_version}/CairnSetup.exe"}},
                    {{"name":"SHA256SUMS.txt","browser_download_url":"https://github.com/zyyyyynnn/Cairn/releases/download/{malformed_version}/SHA256SUMS.txt"}}
                ]
            }}"#
        );
        assert!(matches!(
            release_from_json(&malformed),
            Err(UpdateError::Data)
        ));

        let missing_sums = format!(
            r#"{{
                "tag_name":"v{version}",
                "assets":[
                    {{"name":"CairnSetup.exe","browser_download_url":"https://github.com/zyyyyynnn/Cairn/releases/download/v{version}/CairnSetup.exe"}}
                ]
            }}"#
        );
        assert!(matches!(
            release_from_json(&missing_sums),
            Err(UpdateError::Incomplete)
        ));

        let missing_setup = format!(
            r#"{{
                "tag_name":"v{version}",
                "assets":[
                    {{"name":"SHA256SUMS.txt","browser_download_url":"https://github.com/zyyyyynnn/Cairn/releases/download/v{version}/SHA256SUMS.txt"}}
                ]
            }}"#
        );
        assert!(matches!(
            release_from_json(&missing_setup),
            Err(UpdateError::Incomplete)
        ));
    }

    #[test]
    fn untrusted_or_non_https_release_urls_are_rejected() {
        let version = format!("{}.{}.{}", 0, 2, 0);
        assert!(
            validate_release_url(
                "https://release-assets.githubusercontent.com/assets/CairnSetup.exe"
            )
            .is_ok()
        );
        for bad in [
            format!(
                "http://github.com/zyyyyynnn/Cairn/releases/download/v{version}/CairnSetup.exe"
            ),
            "https://example.invalid/CairnSetup.exe".to_string(),
            "https://evil-github.com/CairnSetup.exe".to_string(),
            "https://user@github.com/CairnSetup.exe".to_string(),
            "https://github.com:8443/CairnSetup.exe".to_string(),
        ] {
            assert!(
                matches!(validate_release_url(&bad), Err(UpdateError::Data)),
                "{bad} must be rejected"
            );
        }

        let untrusted_asset = format!(
            r#"{{
                "tag_name":"v{version}",
                "assets":[
                    {{"name":"CairnSetup.exe","browser_download_url":"https://example.invalid/CairnSetup.exe"}},
                    {{"name":"SHA256SUMS.txt","browser_download_url":"https://github.com/zyyyyynnn/Cairn/releases/download/v{version}/SHA256SUMS.txt"}}
                ]
            }}"#
        );
        assert!(matches!(
            release_from_json(&untrusted_asset),
            Err(UpdateError::Data)
        ));
    }

    /// A manual check reports its progress in the Help menu row, not by taking the toast slot:
    /// the row already renders a spinner while [`UpdateState::Checking`] is active.
    #[test]
    fn starting_a_check_keeps_the_help_row_loading_instead_of_announcing_a_toast() {
        use crate::shell::command_groups::MenuGroup;
        use gpui_kit::AppContext;

        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| crate::app::CairnApp::new(window, cx));
                    app.update(cx, |this, cx| {
                        this.dismiss_toast(cx);
                        this.show_group(MenuGroup::Help, window, cx);
                        this.start_update_check(window, cx);
                        assert!(this.update_state.is_checking());
                        assert!(
                            this.toast.is_hidden(),
                            "the checking phase belongs to the Help menu row"
                        );
                        assert!(
                            this.open_group_menu
                                .as_ref()
                                .is_some_and(|menu| menu.group == MenuGroup::Help)
                        );
                    });
                    cx.quit();
                    app
                },
            );
        });
    }

    /// Network check for HTTP/TLS dependency changes: the real release API must answer through
    /// the installed crypto provider.
    #[tokio::test]
    #[ignore = "requires network access to the GitHub releases API"]
    async fn the_update_check_reaches_the_release_api() {
        // No published release is a valid answer; this test guards the HTTP/TLS dependency path.
        let _ = check_latest().await.expect("the release API answers");
    }
}

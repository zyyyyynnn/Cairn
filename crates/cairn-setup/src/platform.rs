// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Windows boundaries the flow needs: one setup instance, the folder picker, the unelevated
//! launch of the installed app, and cleanup of data the MSI does not own.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, LPARAM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY,
    REG_SAM_FLAGS, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// The result of the single-instance check. `Acquired` owns the process's claim on the mutex;
/// `Unavailable` means the check itself failed, so setup proceeds and lets the MSI's own
/// transaction mutex serialize concurrent work instead of failing closed for a user who cannot
/// access another account's named object.
pub enum InstanceLock {
    Acquired,
    Running,
    Unavailable,
}

/// Keeps one elevated setup or uninstaller instance alive. A second launch brings the existing
/// window forward instead of racing the first one's MSI transaction.
pub fn acquire_instance(mutex_name: &str, window_titles: &[&str]) -> InstanceLock {
    let name = wide(OsStr::new(mutex_name));
    let handle = match unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) } {
        Ok(handle) => handle,
        Err(_) => return InstanceLock::Unavailable,
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        for title in window_titles {
            activate_window(title);
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        return InstanceLock::Running;
    }
    // Windows owns the claim until the process exits; closing the handle here would release it.
    let _ = handle;
    InstanceLock::Acquired
}

/// Brings an existing `window_title` to the foreground. The window class is GPUI's shared class,
/// so the product title is the discriminator.
pub fn activate_window(window_title: &str) -> bool {
    struct Context {
        title: String,
        found: Option<windows::Win32::Foundation::HWND>,
    }

    unsafe extern "system" fn enum_proc(
        hwnd: windows::Win32::Foundation::HWND,
        lparam: LPARAM,
    ) -> BOOL {
        let context = &mut *(lparam.0 as *mut Context);
        let mut buffer = [0u16; 96];
        let length = GetWindowTextW(hwnd, &mut buffer);
        if length > 0 && String::from_utf16_lossy(&buffer[..length as usize]) == context.title {
            context.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }

    let mut context = Context {
        title: window_title.to_string(),
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut context as *mut Context as isize),
        );
    }
    if let Some(hwnd) = context.found {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        true
    } else {
        false
    }
}

/// Opens the Windows folder picker. Returns `None` when the user cancels or the dialog cannot open;
/// a cancelled picker is not an error the window should report.
pub fn browse_for_folder(title: &str) -> Option<PathBuf> {
    unsafe {
        // GPUI may already have initialized COM. A different apartment model is not fatal here;
        // the shell dialog still opens on the UI thread.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
            .ok()?;
        let title = wide(OsStr::new(title));
        dialog.SetTitle(PCWSTR(title.as_ptr())).ok()?;
        if dialog.Show(Some(GetForegroundWindow())).is_err() {
            return None;
        }
        let item: IShellItem = dialog.GetResult().ok()?;
        let display = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = display.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(display.0 as *const core::ffi::c_void));
        path
    }
}

/// Starts the installed GUI through the shell so it inherits the signed-in user's token, not this
/// elevated installer's. `explorer.exe` is the shell's own launch path and does not re-elevate.
pub fn launch_installed_app(exe: &Path) -> Result<(), String> {
    Command::new(explorer_executable())
        .arg(exe)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// Opens a log or another local file through the shell, so the viewer starts with the signed-in
/// user's token. Calling `ShellExecute` directly from this elevated process would launch Notepad
/// elevated as well, which its packaged build does not support.
pub fn open_path(path: &Path) -> Result<(), String> {
    Command::new(explorer_executable())
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// The shell executable by absolute path. A bare `explorer.exe` is resolved against the directory
/// of the elevated setup first, which is user-writable when the installer is run from Downloads.
pub fn explorer_executable() -> PathBuf {
    let mut buffer = [0u16; 260];
    let length = unsafe {
        windows::Win32::System::SystemInformation::GetWindowsDirectoryW(Some(&mut buffer))
    } as usize;
    let windows = if length > 0 && length < buffer.len() {
        PathBuf::from(String::from_utf16_lossy(&buffer[..length]))
    } else {
        PathBuf::from(r"C:\Windows")
    };
    windows.join("explorer.exe")
}

pub fn install_log_path() -> PathBuf {
    log_path("log")
}

pub fn uninstall_log_path() -> PathBuf {
    log_path("uninstall.log")
}

/// A readable diagnostic file for the license viewer. In a release build this lives in the
/// administrator-owned staging root, so an elevated write never follows a link planted in %TEMP%.
pub fn license_path() -> PathBuf {
    log_directory()
        .map(|dir| dir.join("LICENSE.txt"))
        .unwrap_or_else(|| temp_path("LICENSE.txt"))
}

fn log_path(suffix: &str) -> PathBuf {
    if let Some(dir) = log_directory() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        return dir.join(format!(
            "CairnSetup-{stamp}-{}.{suffix}",
            std::process::id()
        ));
    }
    temp_path(suffix)
}

fn log_directory() -> Option<PathBuf> {
    let dir = staging_root().ok()?.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn temp_path(suffix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "CairnSetup-{stamp}-{}.{suffix}",
        std::process::id()
    ))
}

/// Administrator-owned staging root for the extracted MSI and cleanup helper. A user-writable
/// `%TEMP%` would let a standard user replace the file between extraction and elevated use.
fn staging_root() -> std::io::Result<PathBuf> {
    let root = crate::model::program_files_root().join("CairnSetup");
    if std::fs::create_dir_all(&root).is_ok() {
        sweep_stale_staging(&root);
        return Ok(root);
    }
    if cfg!(debug_assertions) {
        let fallback = std::env::temp_dir().join("CairnSetup");
        std::fs::create_dir_all(&fallback)?;
        sweep_stale_staging(&fallback);
        return Ok(fallback);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "could not create the installer staging directory {}",
            root.display()
        ),
    ))
}

/// Failed or interrupted transactions can leave a payload directory or helper copy behind. Only
/// entries that are unmistakably ours and a day old are removed, so a concurrent transaction is
/// never disturbed. The `logs` directory is not part of this sweep.
fn sweep_stale_staging(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let age = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .unwrap_or_default();
        if !is_stale_staging_entry(&name, age) {
            continue;
        }
        if metadata.is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn is_stale_staging_entry(name: &str, age: std::time::Duration) -> bool {
    const A_DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    age >= A_DAY
        && (name.starts_with("payload-")
            || (name.starts_with("cleanup-") && name.ends_with(".exe")))
}

/// A private directory for the extracted MSI. It is removed when the install session ends.
pub fn create_payload_dir() -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let dir = staging_root()?.join(format!("payload-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

/// Data that belongs to the product but not to the MSI's component tree. The installer is
/// machine-wide, so uninstalling removes every profile's Cairn data, not just the profile of the
/// account that happened to provide the administrator token.
pub fn product_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(base) = std::env::var_os("ProgramData") {
        push_unique(&mut dirs, PathBuf::from(base).join("Cairn"));
    }
    for (_, profile) in user_profiles() {
        push_user_data_dirs(&mut dirs, &profile);
    }
    for (variable, suffix) in [("APPDATA", "Cairn"), ("LOCALAPPDATA", "Cairn")] {
        if let Some(base) = std::env::var_os(variable) {
            push_unique(&mut dirs, PathBuf::from(base).join(suffix));
        }
    }
    dirs
}

fn push_user_data_dirs(dirs: &mut Vec<PathBuf>, profile: &Path) {
    let app_data = profile.join("AppData");
    push_unique(dirs, app_data.join("Roaming").join("Cairn"));
    push_unique(dirs, app_data.join("Local").join("Cairn"));
}

fn push_unique(dirs: &mut Vec<PathBuf>, path: PathBuf) {
    let key = path.to_string_lossy().to_ascii_lowercase();
    if dirs
        .iter()
        .any(|existing| existing.to_string_lossy().to_ascii_lowercase() == key)
    {
        return;
    }
    dirs.push(path);
}

/// Removes everything the product wrote outside the MSI's component tree: every profile's data, the
/// machine data, and the autostart values of loaded user hives. Directories are retried because the
/// service may release its index handles just after the MSI reports success.
pub fn remove_product_data() -> Vec<String> {
    let mut failures = Vec::new();
    for dir in product_data_dirs() {
        if !dir.exists() {
            continue;
        }
        let mut last_error = None;
        for attempt in 0..6 {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    last_error = None;
                    break;
                }
                Err(err) => {
                    last_error = Some(err.to_string());
                    std::thread::sleep(std::time::Duration::from_millis(200 * (attempt + 1)));
                }
            }
        }
        if let Some(error) = last_error {
            failures.push(format!("{}: {error}", dir.display()));
        }
    }
    failures.extend(remove_autostart_values());
    failures
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Deletes the `Cairn` value from every loaded user hive and from the current hive. An absent key,
/// hive or value is success; a loaded hive that denies the delete is reported.
fn remove_autostart_values() -> Vec<String> {
    let mut failures = Vec::new();
    for (sid, _) in user_profiles() {
        let subkey = format!(r"{sid}\{RUN_KEY}");
        if let Err(error) = remove_run_value(HKEY_USERS, &subkey) {
            failures.push(format!("autostart value for {sid}: {error}"));
        }
    }
    if let Err(error) = remove_run_value(HKEY_CURRENT_USER, RUN_KEY) {
        failures.push(format!("autostart value: {error}"));
    }
    failures
}

fn remove_run_value(root: HKEY, subkey: &str) -> Result<(), String> {
    let subkey_wide = wide(OsStr::new(subkey));
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(subkey_wide.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Ok(());
    }
    let deleted = unsafe { RegDeleteValueW(key, windows::core::w!("Cairn")) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if deleted != ERROR_SUCCESS && deleted != ERROR_FILE_NOT_FOUND {
        return Err(format!("RegDeleteValueW failed: {deleted:?}"));
    }
    Ok(())
}

/// Profile paths from the OS profile registry. A profile that is not present on disk is skipped.
fn user_profiles() -> Vec<(String, PathBuf)> {
    const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";
    let mut profiles = Vec::new();
    for sid in registry_subkeys(PROFILE_LIST, KEY_READ | KEY_WOW64_64KEY) {
        let subkey = format!(r"{PROFILE_LIST}\{sid}");
        let Some(path) = read_hklm_string(&subkey, "ProfileImagePath") else {
            continue;
        };
        let path = PathBuf::from(path);
        if !path.is_dir() {
            continue;
        }
        if !profiles
            .iter()
            .any(|(existing, _): &(String, PathBuf)| existing.eq_ignore_ascii_case(&sid))
        {
            profiles.push((sid, path));
        }
    }
    profiles
}

fn registry_subkeys(base: &str, flags: REG_SAM_FLAGS) -> Vec<String> {
    let base_wide = wide(OsStr::new(base));
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(base_wide.as_ptr()),
            None,
            flags,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut index = 0;
    loop {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS || status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..length as usize]));
        index += 1;
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    names
}

fn read_hklm_string(subkey: &str, value: &str) -> Option<String> {
    let subkey_wide = wide(OsStr::new(subkey));
    let value_wide = wide(OsStr::new(value));
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_wide.as_ptr()),
            PCWSTR(value_wide.as_ptr()),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(
        &buffer[..length.min(buffer.len())],
    ))
}

/// Copies this binary to a temporary helper before the MSI removes it from the install folder.
/// The copy must happen before uninstall starts; the running image's original path can be gone by
/// the time the transaction finishes.
pub fn prepare_install_dir_cleanup() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let helper = staging_root()
        .ok()?
        .join(format!("cleanup-{}-{nanos}.exe", std::process::id()));
    std::fs::copy(&current, &helper).ok()?;
    Some(helper)
}

/// Starts the prepared helper whose only job is to delete the install folder after this process
/// exits. The install folder contains the running uninstaller, so it cannot remove itself in place.
pub fn spawn_install_dir_cleanup(install_dir: &Path, helper: Option<PathBuf>) {
    if crate::model::validate_install_path(&install_dir.to_string_lossy()).is_err() {
        return;
    }
    let helper = helper.or_else(prepare_install_dir_cleanup);
    let Some(helper) = helper else {
        return;
    };
    let working_dir = helper
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let _ = Command::new(&helper)
        .arg("--cleanup-install-dir")
        .arg(install_dir)
        .arg("--wait-pid")
        .arg(std::process::id().to_string())
        .current_dir(working_dir)
        .spawn();
}

/// The helper side of [`spawn_install_dir_cleanup`]. It waits for the uninstaller to exit, removes
/// the folder, then schedules its own temporary copy for deletion at reboot.
pub fn run_install_dir_cleanup(install_dir: &Path, wait_pid: u32) {
    let Ok(install_dir) = crate::model::validate_install_path(&install_dir.to_string_lossy())
    else {
        schedule_self_delete();
        return;
    };
    if let Ok(handle) = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            wait_pid,
        )
    } {
        unsafe {
            let _ = WaitForSingleObject(handle, INFINITE);
            let _ = CloseHandle(handle);
        }
    }
    // The MSI owns its component tree; this helper only removes product executables that survived
    // the transaction. User files in a shared folder are never deleted.
    for name in PRODUCT_FILES {
        let _ = std::fs::remove_file(install_dir.join(name));
    }
    let _ = std::fs::remove_dir(&install_dir);
    schedule_self_delete();
}

const PRODUCT_FILES: [&str; 6] = [
    "Cairn.exe",
    "CairnService.exe",
    "CairnCli.exe",
    "CairnWorker.exe",
    "CairnSetup.exe",
    "CairnUninstall.exe",
];

fn schedule_self_delete() {
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let path = wide(current.as_os_str());
    unsafe {
        let _ = MoveFileExW(
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        );
    }
}

/// Reads a PID from `--wait-pid`.
pub fn wait_pid_from_args(args: &[String]) -> Option<u32> {
    let index = args.iter().position(|arg| arg == "--wait-pid")?;
    args.get(index + 1)?.parse().ok()
}

/// Reads a path from `--cleanup-install-dir`.
pub fn cleanup_dir_from_args(args: &[String]) -> Option<PathBuf> {
    let index = args.iter().position(|arg| arg == "--cleanup-install-dir")?;
    args.get(index + 1).map(PathBuf::from)
}

pub fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{explorer_executable, is_stale_staging_entry, push_user_data_dirs};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn every_profile_contributes_its_roaming_and_local_data_once() {
        let mut dirs = Vec::new();
        push_user_data_dirs(&mut dirs, Path::new(r"C:\Users\Alice"));
        push_user_data_dirs(&mut dirs, Path::new(r"C:\Users\Alice"));
        assert_eq!(
            dirs,
            [
                PathBuf::from(r"C:\Users\Alice\AppData\Roaming\Cairn"),
                PathBuf::from(r"C:\Users\Alice\AppData\Local\Cairn"),
            ]
        );
    }

    #[test]
    fn only_old_installer_owned_staging_entries_are_swept() {
        let old = Duration::from_secs(25 * 60 * 60);
        let young = Duration::from_secs(60);
        assert!(is_stale_staging_entry("payload-123-456", old));
        assert!(is_stale_staging_entry("cleanup-123-456.exe", old));
        assert!(!is_stale_staging_entry("payload-123-456", young));
        assert!(!is_stale_staging_entry("cleanup-123-456.exe", young));
        assert!(!is_stale_staging_entry("logs", old));
        assert!(!is_stale_staging_entry("notes.txt", old));
    }

    #[test]
    fn the_shell_is_launched_by_absolute_path() {
        let explorer = explorer_executable();
        assert!(explorer.is_absolute(), "{}", explorer.display());
        assert_eq!(
            explorer.file_name(),
            Some(std::ffi::OsStr::new("explorer.exe"))
        );
    }
}

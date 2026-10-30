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
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, LPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
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

/// Keeps one elevated setup or uninstaller instance alive. A second launch brings the existing
/// window forward instead of racing the first one's MSI transaction.
pub fn acquire_instance(mutex_name: &str, window_title: &str) -> Option<HANDLE> {
    let name = wide(OsStr::new(mutex_name));
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        activate_window(window_title);
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    Some(handle)
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
    Command::new("explorer.exe")
        .arg(exe)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// Opens a log or another local file through the shell, so the viewer starts with the signed-in
/// user's token. Calling `ShellExecute` directly from this elevated process would launch Notepad
/// elevated as well, which its packaged build does not support.
pub fn open_path(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

pub fn install_log_path() -> PathBuf {
    temp_path("log")
}

pub fn uninstall_log_path() -> PathBuf {
    temp_path("uninstall.log")
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

/// A private directory for the extracted MSI. It is removed when the install session ends.
pub fn create_payload_dir() -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("CairnSetup-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Data that belongs to the product but not to the MSI's component tree. The service tightens the
/// ProgramData ACL; this list is what an uninstall removes.
pub fn product_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for (variable, suffix) in [
        ("ProgramData", "Cairn"),
        ("APPDATA", "Cairn"),
        ("LOCALAPPDATA", "Cairn"),
    ] {
        if let Some(base) = std::env::var_os(variable) {
            dirs.push(PathBuf::from(base).join(suffix));
        }
    }
    dirs
}

/// Removes everything the product wrote outside the MSI's component tree: its data directories and
/// the current-user autostart value. Both are retried, because the service may release its index
/// handles just after the MSI reports success.
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
    if let Err(error) = remove_autostart_value() {
        failures.push(format!("autostart value: {error}"));
    }
    failures
}

/// Deletes the `Cairn` value the app writes to `HKCU\...\CurrentVersion\Run` when the user turns
/// on launch at login. An absent key or value is success.
fn remove_autostart_value() -> Result<(), String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
    };

    let subkey = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let value_name = w!("Cairn");
    let mut key = windows::Win32::System::Registry::HKEY::default();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey, None, KEY_SET_VALUE, &mut key) };
    if status.is_err() {
        return Ok(());
    }
    let deleted = unsafe { RegDeleteValueW(key, value_name) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if deleted.is_err() && deleted != windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
        return Err(format!("RegDeleteValueW failed: {deleted:?}"));
    }
    Ok(())
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
    let helper =
        std::env::temp_dir().join(format!("CairnCleanup-{}-{nanos}.exe", std::process::id()));
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
    let _ = Command::new(&helper)
        .arg("--cleanup-install-dir")
        .arg(install_dir)
        .arg("--wait-pid")
        .arg(std::process::id().to_string())
        .current_dir(std::env::temp_dir())
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

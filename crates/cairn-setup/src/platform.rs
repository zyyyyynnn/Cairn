// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Windows boundaries the flow needs: one setup instance, the folder picker, the unelevated
//! launch of the installed app, and cleanup of data the MSI does not own.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::PRODUCT_FILES;
use crate::registry::{read_hklm_string, subkeys, wide};
use cairn_protocol::message::SERVICE_NAME;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GENERIC_READ,
    GetLastError, HANDLE, HLOCAL, LPARAM, LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
    GetSecurityDescriptorOwner, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileAttributeTagInfo,
    FileDispositionInfo, FileDispositionInfoEx, GETFINALPATHNAMEBYHANDLE_FLAGS,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetFullPathNameW, GetLongPathNameW, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE,
    SetFileInformationByHandle, VOLUME_NAME_DOS, WRITE_DAC, WRITE_OWNER,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_SET_VALUE,
    KEY_WOW64_64KEY, REG_EXPAND_SZ, REG_SZ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, HWND_BROADCAST, SMTO_ABORTIFHUNG, SW_RESTORE,
    SendMessageTimeoutW, SetForegroundWindow, ShowWindow, WM_SETTINGCHANGE,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let InstanceLock::Acquired(handle) = self {
            // SAFETY: `handle` is an owned mutex HANDLE from `CreateMutexW` held until `InstanceLock` drops.
            unsafe {
                let _ = CloseHandle(*handle);
            }
        }
    }
}

/// A held Windows Installer execute mutex. Registration-only cleanup must not run while another
/// `msiexec` transaction is applying an upgrade. The setup's own mutex does not cover a direct MSI
/// invocation, so the worker waits on the installer's global execute lock and rechecks the product
/// list before deleting anything.
pub struct MsiExecutionLock(HANDLE);

impl Drop for MsiExecutionLock {
    fn drop(&mut self) {
        // SAFETY: `self.0` was successfully waited on and acquired in `acquire_msi_execution`.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire_msi_execution(timeout_ms: u32) -> Option<MsiExecutionLock> {
    let name = wide(OsStr::new(r"Global\_MSIExecute"));
    // SAFETY: `name` is null-terminated and lives for the `CreateMutexW` call.
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.ok()?;
    // SAFETY: `handle` is an open mutex handle returned by `CreateMutexW`.
    let wait = unsafe { WaitForSingleObject(handle, timeout_ms) };
    if matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
        Some(MsiExecutionLock(handle))
    } else {
        // SAFETY: Wait failed or timed out; close the owned handle before returning `None`.
        unsafe {
            let _ = CloseHandle(handle);
        }
        None
    }
}
/// The result of the single-instance check. `Acquired` holds the mutex until the guard drops;
/// `Unavailable` means the check itself failed, so setup proceeds and lets the MSI's own
/// transaction mutex serialize concurrent work instead of failing closed for a user who cannot
/// access another account's named object.
pub enum InstanceLock {
    /// The handle keeps the mutex claim until the guard drops at app exit.
    Acquired(HANDLE),
    Running,
    Unavailable,
}

/// Keeps one elevated setup or uninstaller instance alive. A second launch brings the existing
/// window forward instead of racing the first one's MSI transaction.
pub fn acquire_instance(mutex_name: &str, window_titles: &[&str]) -> InstanceLock {
    let name = wide(OsStr::new(mutex_name));
    // SAFETY: `name` is null-terminated and lives for the `CreateMutexW` call.
    let Ok(handle) = (unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }) else {
        return InstanceLock::Unavailable;
    };
    // SAFETY: `GetLastError` checks whether `CreateMutexW` opened an existing named mutex.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        for title in window_titles {
            activate_window(title);
        }
        // SAFETY: `handle` is a valid mutex handle from `CreateMutexW` that this instance does not own.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return InstanceLock::Running;
    }
    // Windows owns the claim until the process exits; closing the handle here would release it.
    InstanceLock::Acquired(handle)
}

/// Brings an existing `window_title` to the foreground. The window class is GPUI's shared class,
/// so the product title is the discriminator.
pub fn activate_window(window_title: &str) -> bool {
    struct Context {
        title: String,
        found: Option<windows::Win32::Foundation::HWND>,
    }

    /// # Safety
    ///
    /// `EnumWindows` must invoke this callback synchronously with a valid top-level `hwnd` and a
    /// non-null `lparam` pointing to a live `Context` for the duration of enumeration.
    unsafe extern "system" fn enum_proc(
        hwnd: windows::Win32::Foundation::HWND,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` is `&mut Context` passed synchronously by `EnumWindows`, which does not return
        // until enumeration finishes, and `hwnd` is the top-level window enumerated by Win32.
        unsafe {
            let context = &mut *(lparam.0 as *mut Context);
            let mut buffer = [0u16; 96];
            let length = GetWindowTextW(hwnd, &mut buffer);
            if length > 0 && String::from_utf16_lossy(&buffer[..length as usize]) == context.title {
                context.found = Some(hwnd);
                return BOOL(0);
            }
            BOOL(1)
        }
    }

    let mut context = Context {
        title: window_title.to_string(),
        found: None,
    };
    // SAFETY: `enum_proc` runs synchronously inside `EnumWindows` while `context` lives on this stack frame.
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut context as *mut Context as isize),
        );
    }
    if let Some(hwnd) = context.found {
        // SAFETY: `hwnd` was found by `EnumWindows`; restoring and focusing an existing HWND is checked by Win32.
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
    // SAFETY: calls standard shell COM dialog APIs on the UI thread; `title` stays alive across
    // `SetTitle` and `display` is freed via `CoTaskMemFree`.
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

/// The shell executable by absolute path. A bare `explorer.exe` is resolved against the directory
/// of the elevated setup first, which is user-writable when the installer is run from Downloads.
pub fn explorer_executable() -> PathBuf {
    let mut buffer = [0u16; 260];
    // SAFETY: `buffer` is a writable stack array of MAX_PATH UTF-16 code units.
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

pub fn install_log_path() -> std::io::Result<PathBuf> {
    log_path("log")
}

pub fn uninstall_log_path() -> std::io::Result<PathBuf> {
    log_path("uninstall.log")
}

fn log_path(suffix: &str) -> std::io::Result<PathBuf> {
    let dir = log_directory()?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    Ok(dir.join(format!(
        "CairnSetup-{stamp}-{}.{suffix}",
        std::process::id()
    )))
}

fn log_directory() -> std::io::Result<PathBuf> {
    let dir = staging_root()?.join("logs");
    secure_staging_root(&dir)?;
    sweep_old_logs(&dir);
    Ok(dir)
}

fn sweep_old_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
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
        if is_stale_log(&name, age) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn is_stale_log(name: &str, age: std::time::Duration) -> bool {
    const A_WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    age >= A_WEEK && name.starts_with("CairnSetup-") && name.ends_with(".log")
}

/// A shell known folder, never an environment variable: an elevated installer must not let a
/// standard account redirect a machine path to a directory it controls.
fn known_folder(folder: &windows::core::GUID) -> std::io::Result<PathBuf> {
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    // SAFETY: the shell returns a CoTaskMem-allocated, null-terminated path that is freed below.
    let path = unsafe { SHGetKnownFolderPath(folder, KF_FLAG_DEFAULT, None) }
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if path.is_null() {
        return Err(std::io::Error::other("the known folder has no path"));
    }
    // SAFETY: `path` is the null-terminated string returned above.
    let text = unsafe { path.to_string() };
    // SAFETY: `path` is the CoTaskMem allocation returned by SHGetKnownFolderPath.
    unsafe {
        CoTaskMemFree(Some(path.0 as *const core::ffi::c_void));
    }
    Ok(PathBuf::from(text.map_err(|error| {
        std::io::Error::other(error.to_string())
    })?))
}

fn program_data_root() -> std::io::Result<PathBuf> {
    use windows::Win32::UI::Shell::FOLDERID_ProgramData;

    known_folder(&FOLDERID_ProgramData)
}

fn public_desktop_root() -> std::io::Result<PathBuf> {
    use windows::Win32::UI::Shell::FOLDERID_PublicDesktop;

    known_folder(&FOLDERID_PublicDesktop)
}

/// Administrator-owned staging root for the extracted MSI and logs. `%ProgramData%`
/// grants standard users the right to create children, so the root is created with a protected
/// DACL and an Administrators owner instead of inheriting the parent. An existing root is re-owned
/// and re-locked; release builds refuse one that cannot be secured. Debug builds fall back to
/// `%TEMP%` so a non-elevated development run can still exercise the installer.
fn staging_root() -> std::io::Result<PathBuf> {
    let root = match program_data_root() {
        Ok(base) => base.join("CairnSetup"),
        Err(error) if cfg!(debug_assertions) => {
            tracing::warn!("ProgramData is unavailable: {error}");
            let fallback = std::env::temp_dir().join("CairnSetup");
            std::fs::create_dir_all(&fallback)?;
            sweep_stale_staging(&fallback);
            return Ok(fallback);
        }
        Err(error) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("could not resolve ProgramData: {error}"),
            ));
        }
    };
    match secure_staging_root(&root) {
        Ok(()) => {
            sweep_stale_staging(&root);
            return Ok(root);
        }
        Err(error) if cfg!(debug_assertions) => {
            tracing::warn!("installer staging directory is not secure: {error}");
        }
        Err(error) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "could not secure the installer staging directory {}: {error}",
                    root.display()
                ),
            ));
        }
    }
    let fallback = std::env::temp_dir().join("CairnSetup");
    if is_reparse_point(&fallback).unwrap_or(false) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing fallback staging directory because {} is a reparse point",
                fallback.display()
            ),
        ));
    }
    std::fs::create_dir_all(&fallback)?;
    open_staging_directory(&fallback, false)
        .and_then(|h| staging_attributes(h.0))
        .and_then(check_staging_attributes)?;
    sweep_stale_staging(&fallback);
    Ok(fallback)
}

/// The staging tree holds elevated files before the MSI consumes them. SYSTEM and Administrators
/// get full control through a protected, inheritable DACL; the owner is Administrators so a
/// standard user who pre-created the path cannot rewrite it.
const STAGING_SDDL: &str = "O:BAD:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)";

struct OwnedDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for OwnedDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: LocalAlloc'd by ConvertStringSecurityDescriptorToSecurityDescriptorW or
            // GetNamedSecurityInfoW; every pointer derived from it is gone by this point.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
}

fn secure_staging_root(root: &Path) -> std::io::Result<()> {
    let sddl: Vec<u16> = STAGING_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
    // SAFETY: `sddl` is a valid null-terminated SDDL string.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    let descriptor = OwnedDescriptor(descriptor);

    let mut owner = PSID(std::ptr::null_mut());
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    // SAFETY: `descriptor` is alive for the whole block, and the out-parameters point at locals.
    unsafe {
        GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    if owner.is_invalid() || !present.as_bool() || dacl.is_null() {
        return Err(std::io::Error::other(
            "the staging security descriptor is incomplete",
        ));
    }

    let path = wide(root.as_os_str());
    let security = windows::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: BOOL(0),
    };
    // SAFETY: `path` is null-terminated and `security` stays alive across the call.
    let created = unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&security as *const _)) };
    if created.is_err() {
        // SAFETY: reads the error left by the failed CreateDirectoryW.
        let error = unsafe { GetLastError() };
        if error != ERROR_ALREADY_EXISTS {
            return Err(std::io::Error::from_raw_os_error(error.0 as i32));
        }
        secure_existing_staging_root(root, owner, dacl)?;
    }

    verify_staging_root(root)
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle was opened by CreateFileW for this guard alone.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn open_cleanup_handle(path: &Path) -> std::io::Result<OwnedHandle> {
    let wide_path = wide(path.as_os_str());
    // SAFETY: `wide_path` is null-terminated; the returned handle is owned by the guard.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            DELETE.0 | SYNCHRONIZE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
    Ok(OwnedHandle(handle))
}

fn normalized_cleanup_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_lowercase()
}

/// Resolves `.`/`..` and expands Windows 8.3 short names without following reparse points.
fn expand_expected_cleanup_path(path: &Path) -> std::io::Result<String> {
    let raw_wide = wide(path.as_os_str());
    let mut full_buf = vec![0u16; 32_768];
    let full_len = loop {
        // SAFETY: `raw_wide` is NUL-terminated and `full_buf` is writable for its full length.
        let len = unsafe { GetFullPathNameW(PCWSTR(raw_wide.as_ptr()), Some(&mut full_buf), None) };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (len as usize) < full_buf.len() {
            break len as usize;
        }
        full_buf.resize(len as usize + 1, 0);
    };
    full_buf[full_len] = 0;

    let mut long_buf = vec![0u16; full_len.max(32_768) + 1];
    let long_len = loop {
        // SAFETY: `full_buf` is NUL-terminated at `full_len` and `long_buf` is writable.
        let len = unsafe { GetLongPathNameW(PCWSTR(full_buf.as_ptr()), Some(&mut long_buf)) };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (len as usize) < long_buf.len() {
            break len as usize;
        }
        long_buf.resize(len as usize + 1, 0);
    };

    Ok(normalized_cleanup_path(Path::new(
        &String::from_utf16_lossy(&long_buf[..long_len]),
    )))
}

fn verify_cleanup_handle(handle: HANDLE, expected: &Path) -> std::io::Result<u32> {
    let attributes = staging_attributes(handle)?;
    let mut buf = vec![0u16; 32_768];
    let actual = loop {
        // SAFETY: the handle remains open and `buf` is writable for its full length.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                &mut buf,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if len < buf.len() as u32 {
            break normalized_cleanup_path(Path::new(&String::from_utf16_lossy(
                &buf[..len as usize],
            )));
        }
        buf.resize(len as usize + 1, 0);
    };
    if actual != expand_expected_cleanup_path(expected)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "cleanup handle for {} resolved to {}",
                expected.display(),
                actual
            ),
        ));
    }
    Ok(attributes)
}

fn delete_cleanup_handle(handle: HANDLE, is_dir: bool) -> std::io::Result<()> {
    // SAFETY: the handle is owned by the caller and the information structure matches the class.
    let result = unsafe {
        if is_dir {
            let info = FILE_DISPOSITION_INFO_EX {
                Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
                    FILE_DISPOSITION_FLAG_DELETE.0
                        | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0
                        | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0,
                ),
            };
            SetFileInformationByHandle(
                handle,
                FileDispositionInfoEx,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        } else {
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        }
    };
    result.map_err(|error| std::io::Error::from_raw_os_error(error.code().0))
}

fn remove_opened_tree(
    handle: OwnedHandle,
    path: &Path,
    allow_reparse: bool,
) -> std::io::Result<()> {
    let attributes = verify_cleanup_handle(handle.0, path)?;
    let is_reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
    let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    if is_reparse {
        if !allow_reparse {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to recurse into reparse directory {}",
                    path.display()
                ),
            ));
        }
        return delete_cleanup_handle(handle.0, is_dir);
    }
    if !is_dir {
        return delete_cleanup_handle(handle.0, false);
    }
    for entry in std::fs::read_dir(path)? {
        let child = entry?;
        let child_handle = open_cleanup_handle(&child.path())?;
        remove_opened_tree(child_handle, &child.path(), true)?;
    }
    delete_cleanup_handle(handle.0, true)
}

fn open_staging_directory(path: &Path, write: bool) -> std::io::Result<OwnedHandle> {
    let wide_path = wide(path.as_os_str());
    let access = if write {
        GENERIC_READ.0 | READ_CONTROL.0 | WRITE_DAC.0 | WRITE_OWNER.0
    } else {
        GENERIC_READ.0 | READ_CONTROL.0
    };
    // SAFETY: `wide_path` is null-terminated; the returned handle is owned by the guard.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            access,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(OwnedHandle(handle))
}

fn staging_attributes(handle: HANDLE) -> std::io::Result<u32> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: the handle is open and `info` is the exact structure the class writes.
    unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            &mut info as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(info.FileAttributes)
}

fn file_identity(handle: HANDLE) -> std::io::Result<(u32, u32, u32)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open and `info` is the structure the call writes.
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

fn secure_existing_staging_root(root: &Path, owner: PSID, dacl: *mut ACL) -> std::io::Result<()> {
    let handle = open_staging_directory(root, true)?;
    check_staging_attributes(staging_attributes(handle.0)?)?;
    // SAFETY: the handle is open with WRITE_DAC/WRITE_OWNER; `owner` and `dacl` belong to the live
    // descriptor built by `secure_staging_root`. The group is left alone.
    let error = unsafe {
        SetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            Some(owner),
            None,
            Some(dacl as *const ACL),
            None,
        )
    };
    if error.is_err() {
        return Err(std::io::Error::from_raw_os_error(error.0 as i32));
    }
    let secured = file_identity(handle.0)?;
    // A pre-owned root can be renamed while the ACL is applied. Re-open the name and require the
    // same file identity, so a junction swapped into its place is never accepted.
    let named = open_staging_directory(root, false)?;
    check_staging_attributes(staging_attributes(named.0)?)?;
    if file_identity(named.0)? != secured {
        return Err(std::io::Error::other(
            "the installer staging directory changed while it was secured",
        ));
    }
    Ok(())
}

fn check_staging_attributes(attributes: u32) -> std::io::Result<()> {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(std::io::Error::other(
            "the installer staging directory is a reparse point",
        ));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(std::io::Error::other(
            "the installer staging path is not a directory",
        ));
    }
    Ok(())
}

fn verify_staging_root(root: &Path) -> std::io::Result<()> {
    let path = wide(root.as_os_str());
    let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
    // SAFETY: `path` is null-terminated and the descriptor is freed by the guard below.
    let error = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if error.is_err() {
        return Err(std::io::Error::from_raw_os_error(error.0 as i32));
    }
    let descriptor = OwnedDescriptor(descriptor);

    let mut owner = PSID(std::ptr::null_mut());
    let mut defaulted = BOOL::default();
    let mut present = BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `descriptor` is alive for the whole block; the pointers are read-only views into it.
    unsafe {
        GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    if !present.as_bool() || dacl.is_null() || control & SE_DACL_PROTECTED.0 == 0 {
        return Err(std::io::Error::other(
            "the staging directory lost its protected DACL",
        ));
    }

    let mut text = PWSTR::null();
    // SAFETY: `owner` points into `descriptor`; the returned string is released below.
    unsafe { ConvertSidToStringSidW(owner, &mut text) }
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if text.is_null() {
        return Err(std::io::Error::other("the staging owner SID is unreadable"));
    }
    // SAFETY: `text` is the null-terminated string returned by ConvertSidToStringSidW above.
    let owner_text = unsafe { text.to_string() }.unwrap_or_default();
    // SAFETY: `text` is the LocalAlloc'd string returned by ConvertSidToStringSidW.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut core::ffi::c_void)));
    }
    if owner_text != "S-1-5-32-544" && owner_text != "S-1-5-18" {
        return Err(std::io::Error::other(format!(
            "the staging directory is owned by {owner_text}"
        )));
    }
    Ok(())
}

/// Removes stale payload directories and old cleanup copies. Only entries that are unmistakably
/// ours and a day old are removed, so a concurrent transaction is never disturbed. The `logs`
/// directory is not part of this sweep.
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
    secure_staging_root(&dir)?;
    Ok(dir)
}

/// Data that belongs to the product but not to the MSI's component tree. The installer is
/// machine-wide, so uninstalling removes every profile's Cairn data, not just the profile of the
/// account that happened to provide the administrator token.
fn product_data_dirs(profiles: &[(String, PathBuf)]) -> Vec<(PathBuf, PathBuf)> {
    let mut dirs = Vec::new();
    if let Ok(base) = program_data_root() {
        let target = base.join("Cairn");
        push_unique_pair(&mut dirs, base, target);
    }
    for (_, profile) in profiles {
        push_user_data_dirs(&mut dirs, profile);
    }
    dirs
}

fn push_user_data_dirs(dirs: &mut Vec<(PathBuf, PathBuf)>, profile: &Path) {
    let app_data = profile.join("AppData");
    push_unique_pair(
        dirs,
        profile.to_path_buf(),
        app_data.join("Roaming").join("Cairn"),
    );
    push_unique_pair(
        dirs,
        profile.to_path_buf(),
        app_data.join("Local").join("Cairn"),
    );
}

fn push_unique_pair(dirs: &mut Vec<(PathBuf, PathBuf)>, root: PathBuf, target: PathBuf) {
    let key = target.to_string_lossy().to_ascii_lowercase();
    if dirs
        .iter()
        .any(|(_, existing)| existing.to_string_lossy().to_ascii_lowercase() == key)
    {
        return;
    }
    dirs.push((root, target));
}

fn is_reparse_point(path: &Path) -> std::io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok((meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn remove_dir_tree_safe(expected_root: &Path, target: &Path) -> std::io::Result<()> {
    target.strip_prefix(expected_root).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} escapes expected root {}",
                target.display(),
                expected_root.display()
            ),
        )
    })?;
    let handle = match open_cleanup_handle(target) {
        Ok(handle) => handle,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    remove_opened_tree(handle, target, false)
}

/// Removes everything the product wrote outside the MSI's component tree: every profile's data, the
/// machine data, and the autostart values of loaded user hives. Directories are retried because the
/// service may release its index handles just after the MSI reports success.
pub fn remove_product_data() -> Vec<String> {
    let profiles = user_profiles();
    let mut failures = Vec::new();
    for (expected_root, dir) in product_data_dirs(&profiles) {
        if !dir.exists() {
            continue;
        }
        let mut last_error = None;
        for attempt in 0..6 {
            match remove_dir_tree_safe(&expected_root, &dir) {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    last_error = None;
                    break;
                }
                Err(err) => {
                    let abort_retry = err.kind() == std::io::ErrorKind::InvalidInput;
                    last_error = Some(err.to_string());
                    if abort_retry {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200 * (attempt + 1)));
                }
            }
        }
        if let Some(error) = last_error {
            failures.push(format!("{}: {error}", dir.display()));
        }
    }
    failures.extend(remove_autostart_values_for(&profiles));
    failures
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Deletes the `Cairn` value from every loaded user hive and from the current hive. An absent key,
/// hive or value is success; a loaded hive that denies the delete is reported.
fn remove_autostart_values_for(profiles: &[(String, PathBuf)]) -> Vec<String> {
    let mut failures = Vec::new();
    for (sid, _) in profiles {
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
    // SAFETY: `subkey_wide` is null-terminated and outlives `RegOpenKeyExW`; `key` is a stack out-parameter.
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(subkey_wide.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW failed: {status:?}"));
    }
    // SAFETY: `key` is an open registry key with `KEY_SET_VALUE` permission and static wide literal `"Cairn"`.
    let deleted = unsafe { RegDeleteValueW(key, windows::core::w!("Cairn")) };
    // SAFETY: `key` was opened above and is closed exactly once here.
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
    for sid in subkeys(PROFILE_LIST, KEY_WOW64_64KEY) {
        let subkey = format!(r"{PROFILE_LIST}\{sid}");
        let Some(path) = read_hklm_string(
            &subkey,
            "ProfileImagePath",
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
        ) else {
            continue;
        };
        let path = PathBuf::from(path);
        if !is_local_profile(&path) || !path.is_dir() {
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

const ENVIRONMENT_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// Removes the leftover registration of a Cairn entry whose MSI product record is gone. The ARP
/// key is critical because it owns the visible entry; the remaining leftovers are reported as
/// warnings and written to the same uninstall log.
pub fn remove_orphan_registration(install_dir: &Path) -> Result<Vec<String>, String> {
    remove_hklm_key_tree(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn")
        .map_err(|error| format!("Apps & features entry: {error}"))?;

    let mut warnings = Vec::new();
    if let Err(error) = remove_hklm_key_tree(r"Software\Cairn") {
        warnings.push(format!("Cairn registry key: {error}"));
    }
    if let Err(error) = remove_machine_path_entry(install_dir) {
        warnings.push(format!("machine PATH: {error}"));
    }
    if let Err(error) = remove_service(SERVICE_NAME) {
        warnings.push(format!("service: {error}"));
    }
    warnings.extend(remove_orphan_shortcuts());
    warnings.extend(remove_product_data());
    if let Err(error) = spawn_install_dir_cleanup(install_dir) {
        warnings.push(format!("install folder: {error}"));
    }
    Ok(warnings)
}

fn remove_hklm_key_tree(subkey: &str) -> Result<(), String> {
    let subkey = wide(OsStr::new(subkey));
    // SAFETY: `subkey` is a null-terminated registry path under `HKEY_LOCAL_MACHINE`.
    let status = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(subkey.as_ptr())) };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    Err(format!("RegDeleteTreeW failed: {status:?}"))
}

fn remove_machine_path_entry(install_dir: &Path) -> Result<(), String> {
    let key_name = wide(OsStr::new(ENVIRONMENT_KEY));
    let mut key = HKEY::default();
    // SAFETY: `key_name` is null-terminated and lives for the call; `key` receives the opened handle.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key_name.as_ptr()),
            None,
            KEY_READ | KEY_SET_VALUE | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW failed: {status:?}"));
    }

    let result = (|| -> Result<(), String> {
        let mut value_type = REG_SZ;
        let mut size = 0u32;
        // SAFETY: querying value length with `None` buffer on the open `key`.
        let status = unsafe {
            RegQueryValueExW(
                key,
                windows::core::w!("Path"),
                None,
                Some(&mut value_type),
                None,
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if status != ERROR_SUCCESS {
            return Err(format!("RegQueryValueExW failed: {status:?}"));
        }
        let mut buffer = vec![0u16; (size as usize / 2).max(1)];
        let mut read = size;
        // SAFETY: `buffer` has at least `read` bytes of writable allocation matching `&mut read`.
        let status = unsafe {
            RegQueryValueExW(
                key,
                windows::core::w!("Path"),
                None,
                Some(&mut value_type),
                Some(buffer.as_mut_ptr() as *mut u8),
                Some(&mut read),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("RegQueryValueExW failed: {status:?}"));
        }
        let length = (read as usize / 2).min(buffer.len());
        let current = String::from_utf16_lossy(&buffer[..length]);
        let current = current.trim_end_matches('\0');

        let Some(replacement) = path_without_entry(current, install_dir) else {
            return Ok(());
        };
        let write_type = if value_type == REG_EXPAND_SZ {
            REG_EXPAND_SZ
        } else {
            REG_SZ
        };
        let replacement = wide(OsStr::new(&replacement));
        // SAFETY: `replacement` is a contiguous initialized `[u16]` slice, valid to view as `len * 2` bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                replacement.as_ptr() as *const u8,
                replacement.len() * std::mem::size_of::<u16>(),
            )
        };
        // SAFETY: `key` is open with `KEY_SET_VALUE` and `bytes` is a NUL-terminated UTF-16 buffer.
        let status = unsafe {
            RegSetValueExW(
                key,
                windows::core::w!("Path"),
                None,
                write_type,
                Some(bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("RegSetValueExW failed: {status:?}"));
        }
        broadcast_environment_change();
        Ok(())
    })();

    // SAFETY: `key` was opened at the top of `remove_machine_path_entry` and is closed once here.
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn path_without_entry(current: &str, install_dir: &Path) -> Option<String> {
    let install = install_dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase();
    if install.is_empty() {
        return None;
    }
    let mut kept = Vec::new();
    let mut changed = false;
    for part in current.split(';') {
        let entry = part
            .trim()
            .trim_matches('"')
            .trim_end_matches(['\\', '/'])
            .to_ascii_lowercase();
        if entry == install {
            changed = true;
        } else {
            kept.push(part.to_string());
        }
    }
    changed.then(|| kept.join(";"))
}

fn broadcast_environment_change() {
    let environment = wide(OsStr::new("Environment"));
    // SAFETY: `environment` is null-terminated and outlives the synchronous `SendMessageTimeoutW` broadcast.
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(environment.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    }
}

fn remove_service(name: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sc = system32("sc.exe");
    let _ = Command::new(&sc)
        .args(["stop", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let mut stopped = false;
    for _ in 0..20 {
        let Ok(query) = Command::new(&sc)
            .args(["query", name])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            break;
        };
        if query.status.code() == Some(1060) {
            return Ok(());
        }
        let stdout = String::from_utf8_lossy(&query.stdout);
        if stdout.contains("STOPPED") {
            stopped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if !stopped {
        return Err(format!("service {name} did not stop within 5 seconds"));
    }
    let output = Command::new(&sc)
        .args(["delete", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    match output.status.code() {
        Some(0 | 1060) => Ok(()),
        Some(1072) => Err("service removal is pending a restart".to_string()),
        other => Err(format!("sc delete returned {other:?}")),
    }
}

fn system32(program: &str) -> PathBuf {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = [0u16; 260];
    // SAFETY: `buffer` is a writable stack array of MAX_PATH UTF-16 code units.
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    let directory = if length > 0 && length < buffer.len() {
        PathBuf::from(String::from_utf16_lossy(&buffer[..length]))
    } else {
        PathBuf::from(r"C:\Windows\System32")
    };
    directory.join(program)
}

fn remove_orphan_shortcuts() -> Vec<String> {
    let mut failures = Vec::new();
    if let Ok(base) = program_data_root() {
        let dir = base
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Cairn");
        if dir.is_dir()
            && let Err(error) = std::fs::remove_dir_all(&dir)
        {
            failures.push(format!("start menu: {error}"));
        }
    }
    if let Ok(desktop) = public_desktop_root() {
        let shortcut = desktop.join("Cairn.lnk");
        if shortcut.is_file()
            && let Err(error) = std::fs::remove_file(&shortcut)
        {
            failures.push(format!("desktop shortcut: {error}"));
        }
    }
    failures
}

fn is_local_profile(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    )
}

/// Starts a hidden system PowerShell worker that waits for this process to exit and removes
/// leftover product executables and the empty install folder, without copying this executable.
pub fn spawn_install_dir_cleanup(install_dir: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CLEANUP_SCRIPT: &str = "\
$p = [int]$env:CAIRN_CLEANUP_PID; \
if ($p -gt 0) { try { $proc = [System.Diagnostics.Process]::GetProcessById($p); if ($proc.Path -ine $env:CAIRN_CLEANUP_EXE) { exit 1 }; if (-not $proc.WaitForExit(5000)) { exit 1 } } catch {} }; \
$d = $env:CAIRN_CLEANUP_DIR; \
$fs = $env:CAIRN_CLEANUP_FILES -split ';'; \
for ($i = 0; $i -lt 20; $i++) { \
    $rem = $false; \
    foreach ($f in $fs) { \
        if ($f) { \
            $t = Join-Path $d $f; \
            if ([System.IO.File]::Exists($t)) { \
                Remove-Item -LiteralPath $t -Force -ErrorAction SilentlyContinue; \
                if ([System.IO.File]::Exists($t)) { $rem = $true } \
            } \
        } \
    }; \
    if (-not $rem) { break }; \
    Start-Sleep -Milliseconds 250 \
}; \
if ([System.IO.Directory]::Exists($d)) { try { [System.IO.Directory]::Delete($d, $false) } catch {} }";

    let install_dir = crate::model::validate_install_path(&install_dir.to_string_lossy())
        .map_err(|_| "install folder is outside the administrator-owned root".to_string())?;
    let powershell = system32(r"WindowsPowerShell\v1.0\powershell.exe");
    Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            CLEANUP_SCRIPT,
        ])
        .env("CAIRN_CLEANUP_DIR", &install_dir)
        .env("CAIRN_CLEANUP_PID", std::process::id().to_string())
        .env(
            "CAIRN_CLEANUP_EXE",
            std::env::current_exe().map_err(|error| error.to_string())?,
        )
        .env("CAIRN_CLEANUP_FILES", PRODUCT_FILES.join(";"))
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        check_staging_attributes, explorer_executable, is_local_profile, is_stale_log,
        is_stale_staging_entry, path_without_entry, program_data_root, public_desktop_root,
        push_user_data_dirs, secure_staging_root,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    #[test]
    fn the_machine_path_loses_only_the_install_entry() {
        let install = Path::new(r"C:\Program Files\Cairn");
        assert_eq!(
            path_without_entry(r"C:\Tools;C:\Program Files\Cairn;C:\More", install),
            Some(r"C:\Tools;C:\More".to_string())
        );
        assert_eq!(
            path_without_entry(r#""C:\Program Files\Cairn\";C:\Tools"#, install),
            Some(r"C:\Tools".to_string())
        );
        assert_eq!(
            path_without_entry(r"%SystemRoot%;C:\Program Files\CAIRN", install),
            Some(r"%SystemRoot%".to_string())
        );
        assert_eq!(
            path_without_entry(r"C:\Tools;C:\Program Files\Cairn64", install),
            None
        );
        assert_eq!(path_without_entry(";C:\\Tools", Path::new("")), None);
    }

    #[test]
    fn only_old_installer_logs_are_swept() {
        let old = Duration::from_secs(8 * 24 * 60 * 60);
        let young = Duration::from_secs(6 * 24 * 60 * 60);
        assert!(is_stale_log("CairnSetup-1-2.log", old));
        assert!(is_stale_log("CairnSetup-1-2.uninstall.log", old));
        assert!(!is_stale_log("CairnSetup-1-2.log", young));
        assert!(!is_stale_log("notes.txt", old));
    }

    #[test]
    fn every_profile_contributes_its_roaming_and_local_data_once() {
        let mut dirs = Vec::new();
        let profile = Path::new(r"C:\Users\Alice");
        push_user_data_dirs(&mut dirs, profile);
        push_user_data_dirs(&mut dirs, profile);
        assert_eq!(
            dirs,
            [
                (
                    profile.to_path_buf(),
                    PathBuf::from(r"C:\Users\Alice\AppData\Roaming\Cairn")
                ),
                (
                    profile.to_path_buf(),
                    PathBuf::from(r"C:\Users\Alice\AppData\Local\Cairn")
                ),
            ]
        );
    }

    #[test]
    fn program_data_comes_from_the_shell_not_the_environment() {
        let root = program_data_root().expect("the shell resolves ProgramData");
        assert!(root.is_absolute(), "{} is not absolute", root.display());
        assert!(root.is_dir(), "{} is not a directory", root.display());
    }

    #[test]
    fn public_desktop_comes_from_the_shell() {
        let desktop = public_desktop_root().expect("the shell resolves the public desktop");
        assert!(
            desktop.is_absolute(),
            "{} is not absolute",
            desktop.display()
        );
        assert!(desktop.is_dir(), "{} is not a directory", desktop.display());
    }

    #[test]
    fn only_local_profile_paths_are_accepted() {
        assert!(is_local_profile(Path::new(r"C:\Users\Alice")));
        assert!(!is_local_profile(Path::new(
            r"\\server\share\Profiles\Alice"
        )));
        assert!(!is_local_profile(Path::new(r"Profiles\Alice")));
    }

    #[test]
    fn staging_attributes_reject_reparse_points_and_files() {
        assert!(check_staging_attributes(FILE_ATTRIBUTE_DIRECTORY.0).is_ok());
        assert!(
            check_staging_attributes(FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0)
                .is_err()
        );
        assert!(check_staging_attributes(0).is_err());
    }

    #[test]
    #[ignore = "requires elevated Administrator privileges"]
    fn staging_root_is_locked_to_administrators() {
        for precreated in [false, true] {
            let temp = tempfile::tempdir().expect("temp dir");
            let root = temp.path().join("CairnSetup");
            if precreated {
                std::fs::create_dir(&root).expect("create root");
            }
            secure_staging_root(&root).unwrap_or_else(|error| {
                panic!("staging ACL could not be applied (precreated={precreated}): {error}")
            });
            std::fs::write(root.join("probe"), b"ok")
                .expect("the elevated process keeps access to a staging root it has secured");

            for logs_precreated in [false, true] {
                let logs = root.join(if logs_precreated {
                    "logs"
                } else {
                    "logs-created"
                });
                if logs_precreated {
                    std::fs::create_dir(&logs).expect("create logs");
                }
                secure_staging_root(&logs).unwrap_or_else(|error| {
                    panic!(
                        "logs ACL could not be applied (root_precreated={precreated}, logs_precreated={logs_precreated}): {error}"
                    )
                });
                std::fs::write(logs.join("probe.log"), b"ok")
                    .expect("the elevated process keeps access to a logs directory it has secured");
            }
        }
    }

    #[test]
    fn a_junction_cannot_be_used_as_a_staging_directory() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target = temp.path().join("target");
        std::fs::create_dir(&target).expect("target dir");
        let link = temp.path().join("CairnSetup");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J failed with status {status}");
        let error = secure_staging_root(&link).expect_err("a junction must be rejected");
        assert!(
            error.to_string().contains("reparse point"),
            "the junction was not rejected as a reparse point: {error}"
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

    #[test]
    fn remove_dir_tree_safe_blocks_reparse_junction_and_removes_regular_subtrees() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join("Profile");
        let outside = temp.path().join("OutsideTarget");
        let outside_sentinel = outside.join("keep.txt");
        let data_dir = root.join("AppData").join("Local").join("Cairn");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(&outside_sentinel, b"do-not-delete").unwrap();
        std::fs::create_dir_all(data_dir.parent().unwrap()).unwrap();

        // A junction planted at `data_dir` itself must be refused and leave `outside` untouched.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&data_dir)
            .arg(&outside)
            .status()
            .expect("mklink /J");
        assert!(status.success(), "mklink failed: {status}");
        let err = super::remove_dir_tree_safe(&root, &data_dir).expect_err("junction refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(outside_sentinel.exists());
        std::fs::remove_dir(&data_dir).unwrap();

        // A nested child junction inside a regular `data_dir` is unlinked safely without wiping `outside`.
        std::fs::create_dir_all(data_dir.join("sub")).unwrap();
        std::fs::write(data_dir.join("sub").join("cache.idx"), b"abc").unwrap();
        let nested_link = data_dir.join("child_link");
        let nested_status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&nested_link)
            .arg(&outside)
            .status()
            .expect("mklink nested");
        assert!(nested_status.success(), "nested mklink failed");
        super::remove_dir_tree_safe(&root, &data_dir).expect("regular data subtree removes safely");
        assert!(!data_dir.exists());
        assert!(outside_sentinel.exists());
    }

    #[test]
    fn remove_dir_tree_safe_accepts_short_paths_and_still_blocks_junctions() {
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;

        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join("LongRunnerProfileDirectory");
        let target = root.join("AppData").join("Local").join("CairnLongFolder");
        std::fs::create_dir_all(target.join("nested")).unwrap();
        std::fs::write(target.join("nested").join("data.bin"), b"ok").unwrap();

        let wide_root = super::wide(root.as_os_str());
        let mut short_buf = vec![0u16; 32_768];
        // SAFETY: `wide_root` is NUL-terminated and `short_buf` is writable.
        let short_len = unsafe {
            GetShortPathNameW(
                windows::core::PCWSTR(wide_root.as_ptr()),
                Some(&mut short_buf),
            )
        };
        assert!(short_len > 0, "GetShortPathNameW failed");
        let short_root = PathBuf::from(String::from_utf16_lossy(&short_buf[..short_len as usize]));
        let short_target = short_root
            .join("AppData")
            .join("Local")
            .join(".")
            .join("CairnLongFolder");

        super::remove_dir_tree_safe(&short_root, &short_target)
            .expect("8.3 short path should expand to the same long directory");
        assert!(!target.exists());

        let outside = temp.path().join("OutsideTarget");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&target)
            .arg(&outside)
            .status()
            .expect("mklink /J");
        assert!(status.success(), "mklink failed: {status}");
        let err = super::remove_dir_tree_safe(&short_root, &short_target)
            .expect_err("junction via short path must still be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        std::fs::remove_dir(&target).unwrap();
    }
}

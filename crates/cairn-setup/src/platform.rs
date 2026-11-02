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

use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND,
    ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, GENERIC_READ, HANDLE, HLOCAL, LPARAM, WAIT_ABANDONED,
    WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SetSecurityInfo, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, ACL,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, FileAttributeTagInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    WRITE_OWNER,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS,
    KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, ReleaseMutex, WaitForSingleObject, INFINITE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, SendMessageTimeoutW, SetForegroundWindow,
    ShowWindow, HWND_BROADCAST, SMTO_ABORTIFHUNG, SW_RESTORE, WM_SETTINGCHANGE,
};

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let InstanceLock::Acquired(handle) = self {
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
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire_msi_execution(timeout_ms: u32) -> Option<MsiExecutionLock> {
    let name = wide(OsStr::new(r"Global\_MSIExecute"));
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.ok()?;
    let wait = unsafe { WaitForSingleObject(handle, timeout_ms) };
    if matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
        Some(MsiExecutionLock(handle))
    } else {
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
    InstanceLock::Acquired(handle)
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
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};

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

/// Administrator-owned staging root for the extracted MSI, logs and cleanup helper. `%ProgramData%`
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
    std::fs::create_dir_all(&fallback)?;
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
        if !self.0 .0.is_null() {
            // SAFETY: LocalAlloc'd by ConvertStringSecurityDescriptorToSecurityDescriptorW or
            // GetNamedSecurityInfoW; every pointer derived from it is gone by this point.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0 .0)));
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
        lpSecurityDescriptor: descriptor.0 .0,
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
    secure_staging_root(&dir)?;
    Ok(dir)
}

/// Data that belongs to the product but not to the MSI's component tree. The installer is
/// machine-wide, so uninstalling removes every profile's Cairn data, not just the profile of the
/// account that happened to provide the administrator token.
pub fn product_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(base) = program_data_root() {
        push_unique(&mut dirs, base.join("Cairn"));
    }
    for (_, profile) in user_profiles() {
        push_user_data_dirs(&mut dirs, &profile);
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
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW failed: {status:?}"));
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

const SERVICE_NAME: &str = "CairnService";
const ENVIRONMENT_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// Removes the leftover registration of a Cairn entry whose MSI product record is gone. The ARP
/// key is critical because it owns the visible entry; the remaining leftovers are reported as
/// warnings and written to the same uninstall log.
pub fn remove_orphan_registration(
    install_dir: &Path,
    helper: Option<PathBuf>,
) -> Result<Vec<String>, String> {
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
    if let Err(error) = spawn_install_dir_cleanup(install_dir, helper) {
        warnings.push(format!("install folder: {error}"));
    }
    Ok(warnings)
}

fn remove_hklm_key_tree(subkey: &str) -> Result<(), String> {
    let subkey = wide(OsStr::new(subkey));
    let status = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(subkey.as_ptr())) };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    Err(format!("RegDeleteTreeW failed: {status:?}"))
}

fn remove_machine_path_entry(install_dir: &Path) -> Result<(), String> {
    let key_name = wide(OsStr::new(ENVIRONMENT_KEY));
    let mut key = HKEY::default();
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
        let bytes = unsafe {
            std::slice::from_raw_parts(
                replacement.as_ptr() as *const u8,
                replacement.len() * std::mem::size_of::<u16>(),
            )
        };
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
        if dir.is_dir() {
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                failures.push(format!("start menu: {error}"));
            }
        }
    }
    if let Ok(desktop) = public_desktop_root() {
        let shortcut = desktop.join("Cairn.lnk");
        if shortcut.is_file() {
            if let Err(error) = std::fs::remove_file(&shortcut) {
                failures.push(format!("desktop shortcut: {error}"));
            }
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
pub fn spawn_install_dir_cleanup(
    install_dir: &Path,
    helper: Option<PathBuf>,
) -> Result<(), String> {
    if crate::model::validate_install_path(&install_dir.to_string_lossy()).is_err() {
        return Err("install folder is outside the administrator-owned root".to_string());
    }
    let helper = helper
        .or_else(prepare_install_dir_cleanup)
        .ok_or_else(|| "cleanup helper could not be prepared".to_string())?;
    let working_dir = helper
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    Command::new(&helper)
        .arg("--cleanup-install-dir")
        .arg(install_dir)
        .arg("--wait-pid")
        .arg(std::process::id().to_string())
        .current_dir(working_dir)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
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
        assert!(check_staging_attributes(
            FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0
        )
        .is_err());
        assert!(check_staging_attributes(0).is_err());
    }

    #[test]
    fn staging_root_is_locked_to_administrators() {
        for precreated in [false, true] {
            let temp = tempfile::tempdir().expect("temp dir");
            let root = temp.path().join("CairnSetup");
            if precreated {
                std::fs::create_dir(&root).expect("create root");
            }
            match secure_staging_root(&root) {
                Ok(()) => {
                    std::fs::write(root.join("probe"), b"ok").expect(
                        "the elevated process keeps access to a staging root it has secured",
                    );
                }
                Err(error) if matches!(error.raw_os_error(), Some(5) | Some(1307)) => {
                    eprintln!("staging ACL test skipped: the test process is not elevated");
                    return;
                }
                Err(error) => {
                    panic!("staging ACL could not be applied (precreated={precreated}): {error}")
                }
            }

            for logs_precreated in [false, true] {
                let logs = root.join(if logs_precreated {
                    "logs"
                } else {
                    "logs-created"
                });
                if logs_precreated {
                    std::fs::create_dir(&logs).expect("create logs");
                }
                match secure_staging_root(&logs) {
                    Ok(()) => {
                        std::fs::write(logs.join("probe.log"), b"ok").expect(
                            "the elevated process keeps access to a logs directory it has secured",
                        );
                    }
                    Err(error) => panic!(
                        "logs ACL could not be applied (root_precreated={precreated}, logs_precreated={logs_precreated}): {error}"
                    ),
                }
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
        if !status.success() {
            eprintln!("junction test skipped: mklink is unavailable");
            return;
        }
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
}

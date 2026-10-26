// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Win32 Shell API integrations for Opening, Highlighting, Properties dialog, Clipboard, native
//! icons, and the explicit "start the installed service" action.
//!
//! The GUI itself is never elevated. Index configuration goes through the service pipe; the only
//! UAC request is the user's explicit click on "Start service", which launches
//! `sc.exe start CairnService` elevated and waits for its exit code.

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::core::{Interface as _, PWSTR};
#[cfg(windows)]
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_CANCELLED, FILETIME, HANDLE, HWND, WAIT_OBJECT_0,
};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};
#[cfg(windows)]
use windows::Win32::System::SystemServices::{SFGAO_FLAGS, SFGAO_FOLDER};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
#[cfg(windows)]
use windows::Win32::UI::Shell::PropertiesSystem::{PSGetPropertyKeyFromName, PROPERTYKEY};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_RecycleBinFolder, FileOperation, IEnumShellItems, IFileOperation,
    IShellItem, IShellItem2, SHCreateItemFromIDList, SHEmptyRecycleBinW, SHGetKnownFolderIDList,
    SHParseDisplayName, ShellExecuteExW, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
    FOF_WANTNUKEWARNING, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SW_SHOWNORMAL};

use std::path::PathBuf;

/// The app's active top-level window, used as the owner of native Shell dialogs. Returned as a
/// plain integer so a worker thread can carry it to its own STA without sharing a GPUI handle.
pub fn active_window_handle() -> isize {
    #[cfg(windows)]
    {
        unsafe { GetForegroundWindow().0 as isize }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

#[cfg(windows)]
fn owner_hwnd(owner: isize) -> HWND {
    HWND(owner as *mut std::ffi::c_void)
}

/// Invokes system default associated application to open the file (`ShellExecuteExW`).
pub fn open_file(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        let wide_path: Vec<u16> = to_wide_path(path);
        let wide_verb: Vec<u16> = "open\0".encode_utf16().collect();

        let mut exec_info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: 0,
            hwnd: HWND(std::ptr::null_mut()),
            lpVerb: PCWSTR(wide_verb.as_ptr()),
            lpFile: PCWSTR(wide_path.as_ptr()),
            lpParameters: PCWSTR(std::ptr::null()),
            lpDirectory: PCWSTR(std::ptr::null()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        // SAFETY: ShellExecuteExW called with valid null-terminated paths and size.
        let ok = unsafe { ShellExecuteExW(&mut exec_info) };
        if ok.is_err() {
            return Err("Failed to execute open_file".into());
        }
    }
    Ok(())
}

/// Opens the parent directory in Windows Explorer and selects (highlights) the item.
pub fn open_folder_and_select(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        // Spawns `explorer.exe /select,"<path>"` which natively supports long and complex paths
        let explorer_arg = format!("/select,\"{}\"", path);
        let wide_file: Vec<u16> = "explorer.exe\0".encode_utf16().collect();
        let wide_params: Vec<u16> = format!("{}\0", explorer_arg).encode_utf16().collect();

        let mut exec_info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: 0,
            hwnd: HWND(std::ptr::null_mut()),
            lpVerb: PCWSTR(std::ptr::null()),
            lpFile: PCWSTR(wide_file.as_ptr()),
            lpParameters: PCWSTR(wide_params.as_ptr()),
            lpDirectory: PCWSTR(std::ptr::null()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        // SAFETY: ShellExecuteExW launches explorer with /select parameter.
        let ok = unsafe { ShellExecuteExW(&mut exec_info) };
        if ok.is_err() {
            return Err("Failed to highlight file in Explorer".into());
        }
    }
    Ok(())
}

/// Displays the native Windows properties dialog for the specified file (`SEE_MASK_INVOKEIDLIST`).
pub fn show_properties_dialog(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        let wide_path: Vec<u16> = to_wide_path(path);
        let wide_verb: Vec<u16> = "properties\0".encode_utf16().collect();

        let mut exec_info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST,
            hwnd: HWND(std::ptr::null_mut()),
            lpVerb: PCWSTR(wide_verb.as_ptr()),
            lpFile: PCWSTR(wide_path.as_ptr()),
            lpParameters: PCWSTR(std::ptr::null()),
            lpDirectory: PCWSTR(std::ptr::null()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        // SAFETY: ShellExecuteExW with SEE_MASK_INVOKEIDLIST requests standard properties dialog.
        let ok = unsafe { ShellExecuteExW(&mut exec_info) };
        if ok.is_err() {
            return Err("Failed to display properties dialog".into());
        }
    }
    Ok(())
}

/// Extracts the lowercase file extension without the leading dot, or an empty string if none.
pub fn extract_extension_lower(name: &str) -> String {
    match name.as_bytes().iter().rposition(|&b| b == b'.') {
        Some(dot_idx) if dot_idx + 1 < name.len() => name[dot_idx + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Shortcuts commonly hide `.lnk` from their display name, and a version-like display suffix
/// ("Notes v1.2") is not an extension at all. The shell payload's filename carries the real
/// suffix, so it is the primary source; the display name is only a fallback when the payload
/// has none. Directories never carry one.
fn recycle_bin_extension_lower(name: &str, fs_path: &str, is_dir: bool) -> String {
    if is_dir {
        return String::new();
    }
    let payload_name = std::path::Path::new(fs_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let payload_extension = extract_extension_lower(payload_name);
    if !payload_extension.is_empty() {
        return payload_extension;
    }
    extract_extension_lower(name)
}

/// One item currently in the Windows Recycle Bin, as the shell reports it.
#[derive(Clone, Debug)]
pub struct RecycleBinItem {
    /// Original name, as Explorer shows it in the bin.
    pub name: String,
    /// Lowercased name and directory, computed once at enumeration for the bin's
    /// case-insensitive in-place filter and cached sort keys.
    pub name_lower: String,
    /// Lowercased extension without the leading dot, cached for extension sorting.
    pub extension_lower: String,
    /// The directory the item was deleted from.
    pub original_dir: String,
    pub original_dir_lower: String,
    /// Filesystem path of the bin payload (`C:\$Recycle.Bin\...\$R...`),
    /// what restore and open-from-bin act on.
    pub fs_path: String,
    /// Deletion time in Unix epoch seconds; `0` when unreported. This is
    /// the unit `FileRowData.modified_ts` and `format_timestamp` already use.
    pub deleted_ts: i64,
    /// Logical size in bytes; `0` when unreported.
    pub size: u64,
    pub is_dir: bool,
}

impl RecycleBinItem {
    /// Stable identity for selection across sort, filter and reload. The
    /// payload path is unique when the shell reports one; shells that cannot
    /// provide it get a composite key instead of sharing an empty string.
    pub fn identity_key(&self) -> String {
        if self.fs_path.is_empty() {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                self.original_dir, self.name, self.deleted_ts, self.size
            )
        } else {
            self.fs_path.clone()
        }
    }
}

/// A native file-operation outcome that the UI can distinguish from a plain
/// diagnostic. Cancellation is a decision, not a failure, and a failed batch
/// carries the items it could not process so the toast does not blame items
/// that succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinOperationError {
    Cancelled,
    Failed { reason: String, items: Vec<String> },
}

impl BinOperationError {
    pub fn failed(reason: impl Into<String>, items: Vec<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
            items,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Cancelled => "the operation was cancelled",
            Self::Failed { reason, .. } => reason,
        }
    }

    pub fn items(&self) -> &[String] {
        match self {
            Self::Cancelled => &[],
            Self::Failed { items, .. } => items,
        }
    }
}

impl std::fmt::Display for BinOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("the operation was cancelled"),
            Self::Failed { reason, items } => {
                write!(formatter, "{reason}")?;
                if !items.is_empty() {
                    write!(formatter, " ({})", items.join(", "))?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BinOperationError {}

/// The shell's two cancellation results plus `HRESULT_FROM_WIN32(ERROR_CANCELLED)`.
#[cfg(windows)]
fn hresult_is_cancelled(code: windows::core::HRESULT) -> bool {
    matches!(code.0 as u32, 0x8027_0000 | 0x8027_0001 | 0x8007_04C7)
}

#[cfg(windows)]
fn operation_was_aborted(operation: &IFileOperation) -> bool {
    // SAFETY: the operation is live and already performed its queue.
    unsafe { operation.GetAnyOperationsAborted() }
        .map(|aborted| aborted.as_bool())
        .unwrap_or(false)
}

/// The 100 ns ticks between the FILETIME epoch (1601) and the Unix epoch.
const FILETIME_UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

/// Converts FILETIME ticks to the Unix seconds the result table formats.
fn filetime_ticks_to_unix_seconds(ticks: u64) -> i64 {
    let delta = ticks.saturating_sub(FILETIME_UNIX_EPOCH_TICKS);
    i64::try_from(delta / 10_000_000).unwrap_or(i64::MAX)
}

/// Resolves a canonical property name (`System.Recycle.DeletedFrom`) to its
/// key, so no GUID needs hard-coding.
#[cfg(windows)]
fn property_key(name: &str) -> Result<PROPERTYKEY, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = PROPERTYKEY::default();
    // SAFETY: null-terminated canonical name owned for the call's lifetime.
    unsafe { PSGetPropertyKeyFromName(PCWSTR(wide.as_ptr()), &mut key) }
        .map_err(|err| format!("PSGetPropertyKeyFromName({name}): {err}"))?;
    Ok(key)
}

/// COM STA for the calling thread. The guard pairs every successful initialization with
/// `CoUninitialize`; `S_FALSE` still increments the thread's count, and `RPC_E_CHANGED_MODE`
/// leaves the apartment it did not create alone.
#[cfg(windows)]
struct ComApartment {
    initialized: bool,
}

#[cfg(windows)]
impl ComApartment {
    fn enter() -> Self {
        Self {
            initialized: unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok(),
        }
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: this guard owns the matching successful CoInitializeEx call.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(windows)]
fn initialize_com_apartment() -> ComApartment {
    ComApartment::enter()
}

/// The bin's children, each kept as the shell item the Recycle Bin namespace
/// produced. Restore consumes these items directly instead of re-parsing `$R`
/// filesystem paths, so the move stays inside the shell's own Restore semantics.
#[cfg(windows)]
struct BinEntry {
    item: RecycleBinItem,
    shell_item: IShellItem,
}

/// Copies a shell-owned `PWSTR` and releases the CoTaskMem allocation the
/// shell handed out. `PWSTR` has no `Drop`, so skipping the free leaks one
/// allocation per display/property read.
#[cfg(windows)]
unsafe fn shell_string(value: PWSTR) -> String {
    // SAFETY: the caller received this pointer from a shell API that returned
    // a null-terminated string, and it is still alive here.
    let text = unsafe { value.to_string() }.unwrap_or_default();
    // SAFETY: CoTaskMemFree is the documented release for a shell PWSTR.
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    text
}

#[cfg(windows)]
fn enumerate_recycle_bin() -> Result<Vec<BinEntry>, String> {
    let _com = initialize_com_apartment();

    // SAFETY: the known-folder GUID is a constant; the returned PIDL is
    // consumed by SHCreateItemFromIDList and freed with CoTaskMemFree.
    let bin_pidl = unsafe {
        SHGetKnownFolderIDList(&FOLDERID_RecycleBinFolder, 0, HANDLE(std::ptr::null_mut()))
    }
    .map_err(|err| format!("SHGetKnownFolderIDList: {err}"))?;
    // SAFETY: the shell item copies the identity it needs from the PIDL.
    let bin_item: IShellItem2 = match unsafe { SHCreateItemFromIDList(bin_pidl) } {
        Ok(item) => item,
        Err(err) => {
            // SAFETY: the PIDL belongs to this function on every branch.
            unsafe { CoTaskMemFree(Some(bin_pidl.cast())) };
            return Err(format!("SHCreateItemFromIDList: {err}"));
        }
    };
    unsafe { CoTaskMemFree(Some(bin_pidl.cast())) };

    // SAFETY: the bin item owns its identity; the enum handler enumerates
    // its children once.
    let enum_items: IEnumShellItems = unsafe { bin_item.BindToHandler(None, &BHID_EnumItems) }
        .map_err(|err| format!("BindToHandler: {err}"))?;

    let deleted_from_key = property_key("System.Recycle.DeletedFrom")?;
    let deleted_date_key = property_key("System.Recycle.DateDeleted")?;
    let size_key = property_key("System.Size")?;

    let mut entries = Vec::new();
    let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
    loop {
        let mut fetched_count: u32 = 0;
        // SAFETY: batch is initialized with None; Next writes up to 64 items and sets fetched_count.
        unsafe { enum_items.Next(&mut batch, Some(&mut fetched_count)) }
            .map_err(|err| format!("IEnumShellItems::Next: {err}"))?;
        if fetched_count == 0 {
            break;
        }

        for item_opt in batch.iter_mut().take(fetched_count as usize) {
            let Some(shell_item) = item_opt.take() else {
                continue;
            };
            // SAFETY: display-name queries on a live shell item.
            let normal_pwstr = match unsafe { shell_item.GetDisplayName(SIGDN_NORMALDISPLAY) } {
                Ok(pwstr) => pwstr,
                Err(err) => {
                    tracing::warn!("enumerate_recycle_bin: GetDisplayName failed for item: {err}");
                    continue;
                }
            };
            let normal_display = unsafe { shell_string(normal_pwstr) };
            // The Recycle Bin's normal display is the full original path, not the
            // name Explorer shows in its Name column. The file name of that path is
            // also the name the restore move must use for its destination.
            let name = std::path::Path::new(&normal_display)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .filter(|name| !name.is_empty())
                .unwrap_or(normal_display);
            let is_dir = unsafe { shell_item.GetAttributes(SFGAO_FOLDER) }
                .map(|flags: SFGAO_FLAGS| flags.contains(SFGAO_FOLDER))
                .unwrap_or(false);

            // Cast before taking the optional filesystem path: a failed cast must not strand the
            // CoTaskMem PWSTR that GetDisplayName would have handed out.
            // SAFETY: the item casts to its typed interface for property reads.
            let item2: IShellItem2 = match shell_item.cast() {
                Ok(it) => it,
                Err(err) => {
                    tracing::warn!(
                        "enumerate_recycle_bin: IShellItem2 cast failed for item: {err}"
                    );
                    continue;
                }
            };
            let fs_path = unsafe { shell_item.GetDisplayName(SIGDN_FILESYSPATH) }.ok();
            let original_dir = unsafe { item2.GetString(&deleted_from_key) }
                .map(|pwstr| unsafe { shell_string(pwstr) })
                .unwrap_or_default();
            let deleted_ts = unsafe { item2.GetFileTime(&deleted_date_key) }
                .map(|filetime: FILETIME| {
                    let ticks =
                        ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64;
                    filetime_ticks_to_unix_seconds(ticks)
                })
                .unwrap_or(0);
            let size = unsafe { item2.GetUInt64(&size_key) }.unwrap_or(0);
            let fs_path = fs_path
                .map(|pwstr| unsafe { shell_string(pwstr) })
                .unwrap_or_default();
            // The display name of a shortcut commonly hides its `.lnk` suffix. The shell's
            // filesystem path keeps the real extension, so the result icon and extension sort
            // still describe the item rather than only its visible label.
            let ext_lower = recycle_bin_extension_lower(&name, &fs_path, is_dir);
            entries.push(BinEntry {
                item: RecycleBinItem {
                    name_lower: name.to_lowercase(),
                    extension_lower: ext_lower,
                    name,
                    original_dir_lower: original_dir.to_lowercase(),
                    original_dir,
                    fs_path,
                    deleted_ts,
                    size,
                    is_dir,
                },
                shell_item,
            });
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.item.deleted_ts));
    Ok(entries)
}

/// Lists every item in the Windows Recycle Bin: name, original location,
/// deletion time and size, exactly as Explorer's 回收站 folder presents them.
/// Newest deletion first.
pub fn list_recycle_bin() -> Result<Vec<RecycleBinItem>, String> {
    #[cfg(windows)]
    {
        enumerate_recycle_bin().map(|entries| entries.into_iter().map(|entry| entry.item).collect())
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// The original directory must exist before `IFileOperation` can take it as a
/// destination. Explorer recreates a folder that was deleted after the item;
/// `create_dir_all` is the same recovery, and an absolute path is the guard
/// against a malformed shell property creating something outside a volume.
#[cfg(windows)]
fn validate_restore_directory(path: &str) -> Result<&std::path::Path, String> {
    if path.is_empty() {
        return Err("the original location is empty".to_string());
    }
    let dir = std::path::Path::new(path);
    if !dir.is_absolute() {
        return Err(format!("the original location is not absolute: {path}"));
    }
    if path.starts_with(r"\\") || path.starts_with("//") {
        return Err(format!("the original location is a network path: {path}"));
    }
    if dir
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("the original location contains '..': {path}"));
    }
    Ok(dir)
}

#[cfg(windows)]
fn ensure_original_directory(path: &str) -> Result<Option<std::path::PathBuf>, String> {
    let dir = validate_restore_directory(path)?;
    if dir.is_dir() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)
        .map(|()| Some(dir.to_path_buf()))
        .map_err(|err| format!("create {path}: {err}"))
}

#[cfg(windows)]
fn validate_delete_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("the path is empty".to_string());
    }
    let path_ref = std::path::Path::new(path);
    if !path_ref.is_absolute() {
        return Err(format!("the path is not absolute: {path}"));
    }
    if path.starts_with(r"\\") || path.starts_with("//") {
        return Err(format!("the path is a network path: {path}"));
    }
    if path_ref
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("the path contains '..': {path}"));
    }
    Ok(())
}

fn dedup_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

/// Restores items from the Recycle Bin to their original locations through
/// `IFileOperation::MoveItem`, the native restore pipeline (conflict and
/// progress UI included). Items must come from [`list_recycle_bin`]; the shell
/// item is located again by its payload path so the bin namespace, not a raw
/// `$R` parse, is the move's source.
pub fn restore_from_recycle_bin(
    items: &[std::sync::Arc<RecycleBinItem>],
    owner: isize,
) -> Result<(), BinOperationError> {
    #[cfg(windows)]
    {
        if items.is_empty() {
            return Ok(());
        }
        let _com = initialize_com_apartment();
        let requested: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        let entries = enumerate_recycle_bin()
            .map_err(|reason| BinOperationError::failed(reason, dedup_names(requested.clone())))?;
        let mut by_fs_path: std::collections::HashMap<&str, &BinEntry> =
            std::collections::HashMap::new();
        for entry in &entries {
            if !entry.item.fs_path.is_empty() {
                by_fs_path
                    .entry(entry.item.fs_path.as_str())
                    .or_insert(entry);
            }
        }

        // SAFETY: in-proc COM activation of the shell file-operation handler.
        let operation: IFileOperation =
            unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(|err| {
                BinOperationError::failed(
                    format!("CoCreateInstance(FileOperation): {err}"),
                    requested.clone(),
                )
            })?;
        // SAFETY: plain flags assignment; undo stays available for the move.
        unsafe { operation.SetOperationFlags(FOF_ALLOWUNDO) }.map_err(|err| {
            BinOperationError::failed(format!("SetOperationFlags: {err}"), requested.clone())
        })?;
        if owner != 0 {
            // SAFETY: the caller supplied the live top-level window for the shell's own UI.
            let _ = unsafe { operation.SetOwnerWindow(owner_hwnd(owner)) };
        }

        let mut queued_names: Vec<String> = Vec::new();
        let mut failed_names: Vec<String> = Vec::new();
        let mut first_error: Option<String> = None;
        let mut created_dirs: Vec<std::path::PathBuf> = Vec::new();

        for item in items {
            let entry = if item.fs_path.is_empty() {
                None
            } else {
                by_fs_path.get(item.fs_path.as_str()).copied()
            };
            let Some(entry) = entry else {
                failed_names.push(item.name.clone());
                first_error.get_or_insert_with(|| {
                    format!("{} is no longer in the Recycle Bin", item.name)
                });
                continue;
            };
            if item.name.is_empty() {
                failed_names.push(item.name.clone());
                first_error.get_or_insert_with(|| "an item has no name".to_string());
                continue;
            }
            match ensure_original_directory(&item.original_dir) {
                Ok(created) => {
                    if let Some(created) = created {
                        created_dirs.push(created);
                    }
                }
                Err(reason) => {
                    failed_names.push(item.name.clone());
                    first_error.get_or_insert(reason);
                    continue;
                }
            }
            let wide_name = to_wide_path(&item.name);
            let dst_dir = match shell_item_from_path(&item.original_dir) {
                Ok(item) => item,
                Err(err) => {
                    failed_names.push(item.name.clone());
                    first_error.get_or_insert(err);
                    continue;
                }
            };
            // SAFETY: moves the bin item back under its original directory
            // with its original name; the native pipeline owns conflict UI.
            match unsafe {
                operation.MoveItem(
                    &entry.shell_item,
                    &dst_dir,
                    PCWSTR(wide_name.as_ptr()),
                    None,
                )
            } {
                Ok(()) => queued_names.push(item.name.clone()),
                Err(err) => {
                    failed_names.push(item.name.clone());
                    first_error.get_or_insert(format!("MoveItem({}): {err}", item.fs_path));
                }
            }
        }

        if !queued_names.is_empty() {
            // SAFETY: batched restore executes the moves queued above.
            match unsafe { operation.PerformOperations() } {
                Ok(()) => {}
                Err(err) => {
                    if hresult_is_cancelled(err.code()) {
                        cleanup_created_dirs(&created_dirs);
                        return Err(BinOperationError::Cancelled);
                    }
                    let mut failed = failed_names;
                    failed.extend(queued_names);
                    cleanup_created_dirs(&created_dirs);
                    return Err(BinOperationError::failed(
                        format!("PerformOperations: {err}"),
                        dedup_names(failed),
                    ));
                }
            }
            if operation_was_aborted(&operation) {
                cleanup_created_dirs(&created_dirs);
                return Err(BinOperationError::Cancelled);
            }
        }

        if !failed_names.is_empty() {
            cleanup_created_dirs(&created_dirs);
            return Err(BinOperationError::failed(
                first_error.unwrap_or_else(|| "some items could not be restored".to_string()),
                dedup_names(failed_names),
            ));
        }
        if queued_names.is_empty() {
            cleanup_created_dirs(&created_dirs);
            return Err(BinOperationError::failed(
                "no item could be restored",
                dedup_names(requested),
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = items;
        Ok(())
    }
}

#[cfg(windows)]
fn cleanup_created_dirs(dirs: &[std::path::PathBuf]) {
    // Only an empty directory created for a restore that did not finish is
    // removed; a successful restore (or one that placed an item there) keeps it.
    for dir in dirs.iter().rev() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Permanently deletes selected Recycle Bin items through `IFileOperation`.
/// The app confirms the irreversible action first, so `FOF_NOCONFIRMATION`
/// suppresses the shell's second prompt; progress and error UI stay native.
pub fn delete_permanently_from_recycle_bin(
    items: &[std::sync::Arc<RecycleBinItem>],
    owner: isize,
) -> Result<(), BinOperationError> {
    #[cfg(windows)]
    {
        if items.is_empty() {
            return Ok(());
        }
        let _com = initialize_com_apartment();
        let requested: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        let entries = enumerate_recycle_bin()
            .map_err(|reason| BinOperationError::failed(reason, dedup_names(requested.clone())))?;
        let mut by_fs_path: std::collections::HashMap<&str, &BinEntry> =
            std::collections::HashMap::new();
        for entry in &entries {
            if !entry.item.fs_path.is_empty() {
                by_fs_path
                    .entry(entry.item.fs_path.as_str())
                    .or_insert(entry);
            }
        }

        // SAFETY: in-proc COM activation of the shell file-operation handler.
        let operation: IFileOperation =
            unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(|err| {
                BinOperationError::failed(
                    format!("CoCreateInstance(FileOperation): {err}"),
                    requested.clone(),
                )
            })?;
        // SAFETY: plain flags assignment on the freshly created handler. The
        // project dialog has already asked, so the shell must not ask again.
        unsafe { operation.SetOperationFlags(FOF_NOCONFIRMATION) }.map_err(|err| {
            BinOperationError::failed(format!("SetOperationFlags: {err}"), requested.clone())
        })?;
        if owner != 0 {
            // SAFETY: the caller supplied the live top-level window for the shell's own UI.
            let _ = unsafe { operation.SetOwnerWindow(owner_hwnd(owner)) };
        }

        let mut queued_names: Vec<String> = Vec::new();
        let mut failed_names: Vec<String> = Vec::new();
        let mut first_error: Option<String> = None;
        for item in items {
            let entry = if item.fs_path.is_empty() {
                None
            } else {
                by_fs_path.get(item.fs_path.as_str()).copied()
            };
            let Some(entry) = entry else {
                failed_names.push(item.name.clone());
                first_error.get_or_insert_with(|| {
                    format!("{} is no longer in the Recycle Bin", item.name)
                });
                continue;
            };
            // SAFETY: the bin namespace item is the source the shell expects.
            match unsafe { operation.DeleteItem(&entry.shell_item, None) } {
                Ok(()) => queued_names.push(item.name.clone()),
                Err(err) => {
                    failed_names.push(item.name.clone());
                    first_error.get_or_insert(format!("DeleteItem({}): {err}", item.fs_path));
                }
            }
        }
        if queued_names.is_empty() {
            return Err(BinOperationError::failed(
                first_error.unwrap_or_else(|| "no item could be deleted".to_string()),
                dedup_names(if failed_names.is_empty() {
                    requested
                } else {
                    failed_names
                }),
            ));
        }
        // SAFETY: batched delete executes the operations queued above.
        match unsafe { operation.PerformOperations() } {
            Ok(()) => {}
            Err(err) => {
                if hresult_is_cancelled(err.code()) {
                    return Err(BinOperationError::Cancelled);
                }
                let mut failed = failed_names;
                failed.extend(queued_names);
                return Err(BinOperationError::failed(
                    format!("PerformOperations: {err}"),
                    dedup_names(failed),
                ));
            }
        }
        if operation_was_aborted(&operation) {
            return Err(BinOperationError::Cancelled);
        }
        if !failed_names.is_empty() {
            return Err(BinOperationError::failed(
                first_error.unwrap_or_else(|| "some items could not be deleted".to_string()),
                dedup_names(failed_names),
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = items;
        Ok(())
    }
}

/// Empties the Recycle Bin through `SHEmptyRecycleBinW`. The app's unified dialog
/// has already confirmed the irreversible action, so `SHERB_NOCONFIRMATION` suppresses
/// the redundant legacy shell prompt while keeping native deletion progress and sound.
pub fn empty_recycle_bin(owner: isize) -> Result<(), BinOperationError> {
    #[cfg(windows)]
    {
        let _com = initialize_com_apartment();
        const SHERB_NOCONFIRMATION: u32 = 0x00000001;
        // SAFETY: the app's live window owns the shell call; no root-path restriction, so
        // this empties all recycle bins across drives.
        unsafe { SHEmptyRecycleBinW(owner_hwnd(owner), PCWSTR::null(), SHERB_NOCONFIRMATION) }
            .map_err(|err| {
                if hresult_is_cancelled(err.code()) {
                    BinOperationError::Cancelled
                } else {
                    BinOperationError::failed(format!("SHEmptyRecycleBinW: {err}"), Vec::new())
                }
            })
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        Ok(())
    }
}

/// Moves the given paths to the Windows Recycle Bin through `IFileOperation`,
/// the same native pipeline Explorer's 删除 uses. The app shows its own themed
/// confirmation first, so `FOF_NOCONFIRMATION` suppresses the shell's second
/// prompt; progress and error UI still belong to Windows. Directories are
/// deleted whole.
pub fn delete_to_recycle_bin(paths: &[String], owner: isize) -> Result<(), BinOperationError> {
    #[cfg(windows)]
    {
        if paths.is_empty() {
            return Ok(());
        }
        let _com = initialize_com_apartment();
        let requested: Vec<String> = paths
            .iter()
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string()
            })
            .collect();

        // SAFETY: in-proc COM activation of the shell file-operation handler,
        // released with the interface.
        let operation: IFileOperation =
            unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(|err| {
                BinOperationError::failed(
                    format!("CoCreateInstance(FileOperation): {err}"),
                    requested.clone(),
                )
            })?;
        // SAFETY: plain flags assignment on the freshly created handler.
        // FOF_WANTNUKEWARNING overrides FOF_NOCONFIRMATION only when the shell
        // cannot recycle (disabled bin, quota, unsupported volume), so a
        // permanent fallback still stops for the native warning.
        unsafe {
            operation.SetOperationFlags(
                FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING,
            )
        }
        .map_err(|err| {
            BinOperationError::failed(format!("SetOperationFlags: {err}"), requested.clone())
        })?;
        if owner != 0 {
            // SAFETY: the caller supplied the live top-level window for the shell's own UI.
            let _ = unsafe { operation.SetOwnerWindow(owner_hwnd(owner)) };
        }

        let mut queued_names: Vec<String> = Vec::new();
        let mut failed_names: Vec<String> = Vec::new();
        let mut first_error: Option<String> = None;
        for (path, name) in paths.iter().zip(requested.iter()) {
            if let Err(reason) = validate_delete_path(path) {
                failed_names.push(name.clone());
                first_error.get_or_insert(reason);
                continue;
            }
            let item = match shell_item_from_path(path) {
                Ok(item) => item,
                Err(reason) => {
                    failed_names.push(name.clone());
                    first_error.get_or_insert(reason);
                    continue;
                }
            };
            // SAFETY: the item was parsed from a valid path; the native
            // pipeline owns confirmation, progress and error UI for the batch.
            match unsafe { operation.DeleteItem(&item, None) } {
                Ok(()) => queued_names.push(name.clone()),
                Err(err) => {
                    failed_names.push(name.clone());
                    first_error.get_or_insert(format!("DeleteItem({path}): {err}"));
                }
            }
        }
        if queued_names.is_empty() {
            return Err(BinOperationError::failed(
                first_error.unwrap_or_else(|| "no item could be deleted".to_string()),
                dedup_names(if failed_names.is_empty() {
                    requested
                } else {
                    failed_names
                }),
            ));
        }
        // SAFETY: batched delete executes the operations queued above.
        match unsafe { operation.PerformOperations() } {
            Ok(()) => {}
            Err(err) => {
                if hresult_is_cancelled(err.code()) {
                    return Err(BinOperationError::Cancelled);
                }
                let mut failed = failed_names;
                failed.extend(queued_names);
                return Err(BinOperationError::failed(
                    format!("PerformOperations: {err}"),
                    dedup_names(failed),
                ));
            }
        }
        if operation_was_aborted(&operation) {
            return Err(BinOperationError::Cancelled);
        }
        if !failed_names.is_empty() {
            return Err(BinOperationError::failed(
                first_error.unwrap_or_else(|| "some items could not be deleted".to_string()),
                dedup_names(failed_names),
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Ok(())
    }
}

/// Copies text to the Windows system clipboard.
pub fn set_clipboard_text(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        use windows::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };

        let u16_chars: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes_len = u16_chars.len() * 2;

        // SAFETY: OpenClipboard opens the clipboard for the current process.
        unsafe {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_err() {
                return Err("Failed to open clipboard".into());
            }

            let _ = EmptyClipboard();

            let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes_len);
            if let Ok(h_val) = h_mem {
                let ptr = GlobalLock(h_val) as *mut u16;
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(u16_chars.as_ptr(), ptr, u16_chars.len());
                    let _ = GlobalUnlock(h_val);
                    // CF_UNICODETEXT = 13
                    let _ = SetClipboardData(13, windows::Win32::Foundation::HANDLE(h_val.0));
                }
            }

            let _ = CloseClipboard();
        }
    }
    Ok(())
}

/// Retrieves text from the Windows system clipboard.
pub fn get_clipboard_text() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::System::DataExchange::{
            CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        };
        use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

        // SAFETY: OpenClipboard and GetClipboardData for CF_UNICODETEXT format.
        unsafe {
            // CF_UNICODETEXT = 13
            if IsClipboardFormatAvailable(13).is_err() {
                return None;
            }
            if OpenClipboard(HWND(std::ptr::null_mut())).is_err() {
                return None;
            }
            let res = (|| {
                let handle = GetClipboardData(13).ok()?;
                let h_global = windows::Win32::Foundation::HGLOBAL(handle.0);
                let ptr = GlobalLock(h_global) as *const u16;
                if ptr.is_null() {
                    return None;
                }
                let mut len = 0;
                while *ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(ptr, len);
                let text = String::from_utf16_lossy(slice);
                let _ = GlobalUnlock(h_global);
                Some(text)
            })();
            let _ = CloseClipboard();
            res
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn to_wide_path(path: &str) -> Vec<u16> {
    let mut p = path.to_string();
    if p.len() >= 260 && !p.starts_with(r"\\?\") {
        p = format!(r"\\?\{}", p);
    }
    p.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Null-terminated UTF-16 for `SHCreateItemFromParsingName`. The shell parser
/// rejects the `\\?\` prefix (`E_INVALIDARG`), so long-path callers must hand it
/// the plain form; the shell's own long-path support then decides.
#[cfg(windows)]
fn to_wide_shell_path(path: &str) -> Vec<u16> {
    path.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Builds a shell item through the display-name parser and its PIDL. This is
/// the path form `SHCreateItemFromParsingName` cannot replace for long or CJK
/// paths, and it keeps destination parsing independent of the recycle-bin
/// namespace item.
#[cfg(windows)]
fn shell_item_from_path(path: &str) -> Result<IShellItem, String> {
    let wide = to_wide_shell_path(path);
    let mut pidl: *mut windows::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null_mut();
    let mut attributes = 0u32;
    // SAFETY: the wide name is null-terminated for the call; the shell owns
    // the returned PIDL until it is released below.
    unsafe {
        SHParseDisplayName(
            PCWSTR(wide.as_ptr()),
            None,
            &mut pidl,
            0,
            Some(&mut attributes),
        )
    }
    .map_err(|err| format!("parse {path}: {err}"))?;
    // SAFETY: the PIDL is the shell item's identity for this conversion.
    let item =
        unsafe { SHCreateItemFromIDList(pidl) }.map_err(|err| format!("create item {path}: {err}"));
    // SAFETY: SHParseDisplayName allocated the PIDL with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(pidl.cast())) };
    item
}

/// Null-terminated UTF-16 without the `\\?\` prefix `to_wide_path` adds. The command line of a
/// child process is *not* a path, so the prefix would be handed to the argument parser verbatim.
#[cfg(windows)]
fn to_wide_literal(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- installed Windows service control -------------------------------------------------

/// `ERROR_CANCELLED`: the user pressed "No" on the UAC consent dialog. It is not a failure, so it
/// must not be reported as one.
const WIN32_ERROR_USER_CANCELLED: u32 = ERROR_CANCELLED.0;

/// Recovers the Win32 error from an `HRESULT_FROM_WIN32` value (facility 7).
pub const fn win32_code_from_hresult(code: i32) -> Option<u32> {
    let bits = code as u32;
    let is_failure = bits & 0x8000_0000 != 0;
    let facility = (bits >> 16) & 0x1FFF;
    if is_failure && facility == 7 {
        Some(bits & 0xFFFF)
    } else {
        None
    }
}

/// The service name the MSI registers. The GUI only asks SCM to start it; it never launches a
/// second console indexer, because that process would sit outside SCM's lifecycle and ACL model.
pub const SERVICE_NAME: &str = "CairnService";

/// `sc.exe start CairnService` is the explicit user action behind the offline band's button. The
/// command is pure data so the parameter string can be tested without SCM.
fn service_start_parameters() -> String {
    format!("start {SERVICE_NAME}")
}

/// Whether the per-machine service is registered. This is a read-only SCM query available to a
/// standard user; it decides whether the offline band offers "Start service" or install guidance.
#[cfg(windows)]
pub fn service_is_installed() -> bool {
    use windows::core::w;
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    };

    // SAFETY: both queries only read SCM state; every handle is closed before returning.
    unsafe {
        let Ok(scm) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return false;
        };
        let service = OpenServiceW(scm, w!("CairnService"), SERVICE_QUERY_STATUS);
        let _ = CloseServiceHandle(scm);
        match service {
            Ok(handle) => {
                let _ = CloseServiceHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(not(windows))]
pub fn service_is_installed() -> bool {
    false
}

/// Best-effort start with the caller's own token, without a UAC prompt. The installed service's
/// default DACL usually lets only administrators start it, so an access-denied answer is expected
/// for a standard user and simply hands the decision to the explicit button.
pub fn try_start_installed_service() -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, StartServiceW, SC_MANAGER_CONNECT,
            SERVICE_QUERY_STATUS, SERVICE_START,
        };

        // SAFETY: every SCM handle is closed before returning; StartServiceW only asks SCM to
        // transition the already-registered service.
        unsafe {
            let scm = OpenSCManagerW(None, None, SC_MANAGER_CONNECT)
                .map_err(|err| format!("OpenSCManager: {err}"))?;
            let service = OpenServiceW(
                scm,
                w!("CairnService"),
                SERVICE_START | SERVICE_QUERY_STATUS,
            );
            let _ = CloseServiceHandle(scm);
            let service = service.map_err(|err| format!("OpenService: {err}"))?;
            let result = StartServiceW(service, None);
            let _ = CloseServiceHandle(service);
            match result {
                Ok(()) => Ok(()),
                Err(err)
                    if win32_code_from_hresult(err.code().0)
                        == Some(ERROR_SERVICE_ALREADY_RUNNING) =>
                {
                    Ok(())
                }
                Err(err) => Err(format!("StartService: {err}")),
            }
        }
    }

    #[cfg(not(windows))]
    {
        Err("starting the Windows service is only available on Windows".to_string())
    }
}

/// Starts the sibling `cairnd.exe` as this account on the user pipe.
///
/// This is not an elevation path: the process runs with the app's own token, indexes only what
/// that token can read, and exits with the window that started it. It is how a machine without
/// the installed service is still fully usable.
#[cfg(windows)]
pub fn spawn_user_indexer(parent_pid: u32) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    // CREATE_NO_WINDOW: the indexer is a console binary and must not flash a console over the GUI.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let executable = std::env::current_exe()
        .map_err(|error| format!("this program's location could not be read: {error}"))?
        .with_file_name("cairnd.exe");
    if !executable.is_file() {
        return Err(format!("{} was not found", executable.display()));
    }

    std::process::Command::new(&executable)
        .arg("--user-mode")
        .arg("--parent-pid")
        .arg(parent_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", executable.display()))
}

#[cfg(not(windows))]
pub fn spawn_user_indexer(_parent_pid: u32) -> Result<std::process::Child, String> {
    Err("the user-mode indexer is only available on Windows".to_string())
}

/// Why an explicit elevation request did not complete.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ElevationError {
    /// The user dismissed the UAC consent dialog. Nothing was started.
    #[error("the elevation request was cancelled at the UAC prompt")]
    Cancelled,
    /// The elevated process could not be started, or its result could not be read.
    #[error("could not start {SERVICE_NAME}: {message}")]
    Failed {
        /// Win32 last-error code behind the failure, when the failure is one.
        os_error: Option<u32>,
        /// Technical reason for logs and for the status band's detail line.
        message: String,
    },
}

/// `ERROR_SERVICE_ALREADY_RUNNING`: the explicit action raced a service that was already up. That
/// is the outcome the user asked for, not a failure.
const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;

/// Starts the installed service through one UAC elevation and waits for `sc.exe` to finish.
///
/// The GUI itself stays unprivileged. This is only called from the offline band's explicit button;
/// opening the window never requests elevation, and a service that is not installed is reported as
/// such instead of trying to launch a second console indexer.
pub fn start_service_elevated() -> Result<(), ElevationError> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let helper_failed =
            |os_error: Option<u32>, message: String| ElevationError::Failed { os_error, message };

        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let sc = system_root.join("System32").join("sc.exe");
        if !sc.is_file() {
            return Err(helper_failed(
                None,
                format!("sc.exe was not found at {}", sc.display()),
            ));
        }

        let wide_file = to_wide_literal(&sc.to_string_lossy());
        let wide_parameters = to_wide_literal(&service_start_parameters());
        let wide_verb = to_wide_literal("runas");

        let mut exec_info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            // Without the process handle the wait below would be the optimistic "started, therefore
            // running" answer instead of the observed one.
            fMask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: HWND(std::ptr::null_mut()),
            lpVerb: PCWSTR(wide_verb.as_ptr()),
            lpFile: PCWSTR(wide_file.as_ptr()),
            lpParameters: PCWSTR(wide_parameters.as_ptr()),
            lpDirectory: PCWSTR(std::ptr::null()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };

        // SAFETY: the three PCWSTR buffers are live until ShellExecuteExW returns, cbSize is the
        // size of the struct, and every other field is either null or a plain value.
        if let Err(err) = unsafe { ShellExecuteExW(&mut exec_info) } {
            let os_error = win32_code_from_hresult(err.code().0);
            if os_error == Some(WIN32_ERROR_USER_CANCELLED) {
                return Err(ElevationError::Cancelled);
            }
            return Err(helper_failed(os_error, err.message()));
        }

        let process = exec_info.hProcess;
        if process.is_invalid() {
            return Err(helper_failed(
                None,
                "the elevated sc.exe returned no process handle".to_string(),
            ));
        }

        // SAFETY: process is the handle ShellExecuteExW returned and is closed after the wait and
        // the exit-code read below.
        let wait = unsafe { WaitForSingleObject(process, INFINITE) };
        if wait != WAIT_OBJECT_0 {
            // SAFETY: GetLastError reads the calling thread's last error set by the wait, and the
            // process handle is paired with the successful ShellExecuteExW above.
            let last_error = unsafe { GetLastError().0 };
            unsafe {
                let _ = CloseHandle(process);
            }
            return Err(helper_failed(
                Some(last_error),
                format!("waiting for the elevated sc.exe did not complete (wait code {wait:?})"),
            ));
        }

        let mut exit_code = 0u32;
        // SAFETY: process is an exited process handle and exit_code is a valid out pointer.
        let read = unsafe { GetExitCodeProcess(process, &mut exit_code) };
        // SAFETY: paired with the successful ShellExecuteExW that produced this handle.
        unsafe {
            let _ = CloseHandle(process);
        }
        read.map_err(|err| {
            helper_failed(
                win32_code_from_hresult(err.code().0),
                format!("sc.exe exited, but its exit code could not be read: {err}"),
            )
        })?;

        if exit_code == 0 || exit_code == ERROR_SERVICE_ALREADY_RUNNING {
            Ok(())
        } else {
            Err(helper_failed(
                Some(exit_code),
                format!("sc.exe exited with code {exit_code}"),
            ))
        }
    }

    #[cfg(not(windows))]
    {
        Err(ElevationError::Failed {
            os_error: None,
            message: "starting the Windows service is only available on Windows".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    #[test]
    fn the_explicit_start_targets_the_installed_service_by_name() {
        assert_eq!(SERVICE_NAME, "CairnService");
        assert_eq!(service_start_parameters(), "start CairnService");
    }

    #[test]
    fn recycle_bin_timestamps_use_the_unix_seconds_the_table_formats() {
        assert_eq!(filetime_ticks_to_unix_seconds(FILETIME_UNIX_EPOCH_TICKS), 0);
        assert_eq!(
            filetime_ticks_to_unix_seconds(FILETIME_UNIX_EPOCH_TICKS + 10_000_000),
            1
        );
        assert_eq!(
            filetime_ticks_to_unix_seconds(FILETIME_UNIX_EPOCH_TICKS + 1_000_000_000),
            100
        );
        assert_eq!(filetime_ticks_to_unix_seconds(0), 0);
    }

    #[test]
    fn a_cancelled_bin_operation_is_not_a_failure() {
        assert!(BinOperationError::Cancelled.is_cancelled());
        let failed = BinOperationError::failed("access denied", vec!["a.txt".to_string()]);
        assert!(!failed.is_cancelled());
        assert_eq!(failed.reason(), "access denied");
        assert_eq!(failed.items(), ["a.txt"]);
    }

    #[test]
    fn empty_payload_paths_still_get_distinct_identity_keys() {
        let mut first = RecycleBinItem {
            name: "a.txt".into(),
            name_lower: "a.txt".into(),
            extension_lower: "txt".into(),
            original_dir: r"C:\work".into(),
            original_dir_lower: r"c:\work".into(),
            fs_path: String::new(),
            deleted_ts: 10,
            size: 1,
            is_dir: false,
        };
        let second = RecycleBinItem {
            name: "b.txt".into(),
            deleted_ts: 10,
            ..first.clone()
        };
        assert_ne!(first.identity_key(), second.identity_key());
        let mut same_name = second.clone();
        same_name.name = first.name.clone();
        same_name.size = 2;
        assert_ne!(first.identity_key(), same_name.identity_key());
        let stable = first.identity_key();
        first.deleted_ts = 11;
        assert_ne!(stable, first.identity_key());
    }

    #[test]
    fn extract_extension_lower_handles_edge_cases() {
        assert_eq!(extract_extension_lower("file.txt"), "txt");
        assert_eq!(extract_extension_lower("FILE.TXT"), "txt");
        assert_eq!(extract_extension_lower("archive.tar.gz"), "gz");
        assert_eq!(extract_extension_lower("no_extension"), "");
        assert_eq!(extract_extension_lower("trailing_dot."), "");
        assert_eq!(extract_extension_lower(".gitignore"), "gitignore");
        assert_eq!(extract_extension_lower("."), "");
        assert_eq!(extract_extension_lower(".."), "");
        assert_eq!(extract_extension_lower(""), "");
        assert_eq!(extract_extension_lower("文档.DOCX"), "docx");
    }

    #[test]
    fn a_shortcut_extension_survives_a_display_name_that_hides_it() {
        assert_eq!(
            recycle_bin_extension_lower(
                "Grok Bot",
                r"C:\$Recycle.Bin\S-1-5-21-1\$R1EJI7X.lnk",
                false
            ),
            "lnk"
        );
        assert_eq!(
            recycle_bin_extension_lower("photo.png", r"C:\$Recycle.Bin\S-1\$R2.png", false),
            "png"
        );
        assert_eq!(
            recycle_bin_extension_lower("payload", r"C:\$Recycle.Bin\S-1\$R3", false),
            ""
        );
        assert_eq!(
            recycle_bin_extension_lower("Folder", r"C:\$Recycle.Bin\S-1\$R4", true),
            ""
        );
    }

    #[test]
    fn a_hidden_extension_beats_a_version_like_display_suffix() {
        // The last dot in "Notes v1.2" is part of the version, not an extension; the payload
        // filename still knows the file is a text file.
        assert_eq!(
            recycle_bin_extension_lower(
                "Notes v1.2",
                r"C:\$Recycle.Bin\S-1-5-21-1\$R5TXT.txt",
                false
            ),
            "txt"
        );
        // The payload is the authority when it carries a suffix of its own.
        assert_eq!(
            recycle_bin_extension_lower("report.pdf", r"C:\$Recycle.Bin\S-1\$R6.docx", false),
            "docx"
        );
        // A payload without one falls back to the display name.
        assert_eq!(
            recycle_bin_extension_lower("report.pdf", r"C:\$Recycle.Bin\S-1\$R7", false),
            "pdf"
        );
    }

    #[cfg(windows)]
    #[test]
    fn restore_and_delete_paths_reject_unc_parent_and_relative_forms() {
        assert!(validate_restore_directory(r"C:\work\archive").is_ok());
        assert!(validate_restore_directory(r"\\server\share").is_err());
        assert!(validate_restore_directory(r"//server/share").is_err());
        assert!(validate_restore_directory(r"C:\work\..\escape").is_err());
        assert!(validate_restore_directory(r"relative\dir").is_err());
        assert!(validate_delete_path(r"C:\work\file.txt").is_ok());
        assert!(validate_delete_path(r"//server/share/file.txt").is_err());
        assert!(validate_delete_path(r"C:\work\..\file.txt").is_err());
        assert!(validate_delete_path("file.txt").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn the_shell_parser_never_receives_a_verbatim_path_prefix() {
        let long = format!(r"C:\{}", "a".repeat(300));
        let wide = to_wide_shell_path(&long);
        let text = String::from_utf16(&wide[..wide.len() - 1]).expect("valid UTF-16");
        assert_eq!(text, long);
        assert!(!text.starts_with(r"\\?\"));
    }

    /// Manual smoke test: it touches the real Recycle Bin, so it is ignored by
    /// default. Run with `--ignored` when changing the shell pipeline.
    #[test]
    #[ignore = "manual: creates and restores a real Recycle Bin item"]
    fn a_deleted_file_lists_and_restores_through_the_shell() {
        let dir = std::env::temp_dir().join("cairn-bin-round-trip");
        std::fs::create_dir_all(&dir).expect("create the probe directory");
        let file = dir.join("cairn-bin-round-trip.txt");
        std::fs::write(&file, b"cairn").expect("create the probe file");
        let path = file.to_string_lossy().into_owned();

        delete_to_recycle_bin(std::slice::from_ref(&path), 0).expect("move the probe to the bin");
        assert!(!file.exists(), "the probe must leave its original path");

        let listing = list_recycle_bin().expect("list the bin");
        let item = std::sync::Arc::new(
            listing
                .iter()
                .find(|item| item.name == "cairn-bin-round-trip.txt")
                .expect("the probe is listed in the bin")
                .clone(),
        );

        restore_from_recycle_bin(std::slice::from_ref(&item), 0).expect("restore the probe");
        assert!(file.exists(), "restore must put the probe back");

        let listing = list_recycle_bin().expect("list the bin after restore");
        assert!(
            !listing.iter().any(|entry| entry.fs_path == item.fs_path),
            "the restored item must leave the bin"
        );

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}

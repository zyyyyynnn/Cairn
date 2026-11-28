// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Win32 security descriptors: the named pipe DACL and the access control of the service data
//! locations fixed by SPEC SET-02.

#[cfg(windows)]
use windows::Win32::Foundation::{HLOCAL, LocalFree};
#[cfg(windows)]
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
#[cfg(windows)]
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
#[cfg(windows)]
use windows::core::PCWSTR;

/// SDDL string granting Generic All to SYSTEM and Administrators, and Read/Write to Interactive Users:
/// `D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`
pub const DEFAULT_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";
pub const ADMIN_ONLY_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)";

/// Owner SYSTEM, group SYSTEM, and a protected DACL granting Generic All to SYSTEM and to the
/// built-in Administrators and to nobody else (SPEC SET-02).
///
/// The DACL is protected so that nothing inherited from `%ProgramData%` — which grants
/// `BUILTIN\Users:(CI)(WD,AD,WEA,WA)` — survives inside the service data locations.
pub const RESTRICTED_SDDL: &str = "O:SYG:SYD:P(A;;GA;;;SY)(A;;GA;;;BA)";

#[cfg(windows)]
pub struct SafeSecurityDescriptor {
    pub sa: SECURITY_ATTRIBUTES,
    sd_ptr: PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl SafeSecurityDescriptor {
    pub fn create(admin_only: bool) -> Result<Self, windows::core::Error> {
        let sddl = if admin_only {
            ADMIN_ONLY_SDDL
        } else {
            DEFAULT_SDDL
        };

        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd_ptr = PSECURITY_DESCRIPTOR(std::ptr::null_mut());

        // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW parses a valid SDDL string.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd_ptr,
                None,
            )?;
        }

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd_ptr.0,
            bInheritHandle: false.into(),
        };

        Ok(Self { sa, sd_ptr })
    }
}

#[cfg(windows)]
impl Drop for SafeSecurityDescriptor {
    fn drop(&mut self) {
        if !self.sd_ptr.0.is_null() {
            // SAFETY: Memory allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW must be freed with LocalFree.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd_ptr.0)));
            }
        }
    }
}

/// The service data locations of SPEC SET-02, with their access control enforced.
#[derive(Clone, Debug)]
pub struct ServiceDataLayout {
    pub root: std::path::PathBuf,
    pub index_dir: std::path::PathBuf,
    pub content_dir: std::path::PathBuf,
    pub logs_dir: std::path::PathBuf,
    pub config_path: std::path::PathBuf,
    /// `service.toml` existed but was not owned and controlled by SYSTEM/Administrators. Its ACL has
    /// been repaired and its contents are not applied by this startup: because a standard account may
    /// create files in `%ProgramData%`, an unverified configuration file can have been planted for the
    /// `LocalSystem` service to hot-apply (SVC-05).
    pub config_untrusted: bool,
}

impl ServiceDataLayout {
    pub fn new(root: &std::path::Path) -> Self {
        Self {
            root: root.to_path_buf(),
            index_dir: root.join("index"),
            content_dir: root.join("content"),
            logs_dir: root.join("logs"),
            config_path: root.join("service.toml"),
            config_untrusted: false,
        }
    }

    /// Every location the service writes to, used for startup checks.
    pub fn writable_locations(&self) -> [std::path::PathBuf; 4] {
        [
            self.root.clone(),
            self.index_dir.clone(),
            self.content_dir.clone(),
            self.logs_dir.clone(),
        ]
    }
}

/// `%ProgramData%\Cairn`, the location fixed by SPEC SET-02.
pub fn default_service_data_root() -> std::path::PathBuf {
    let program_data = std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"));
    program_data.join("Cairn")
}

struct RestorePrivilege {
    taken_here: bool,
}

impl Drop for RestorePrivilege {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.taken_here
            && let Err(reason) = windows_acl::disable_restore_privilege()
        {
            tracing::warn!(
                "SeRestorePrivilege could not be put back ({reason}); this process keeps the \
                     right to name any SID as an owner until it exits"
            );
        }
    }
}

// See docs/engineering/ipc-security.md#4-权限使用.
fn apply_ownership_privileges() -> RestorePrivilege {
    #[cfg(windows)]
    let taken_here = {
        match windows_acl::enable_take_ownership_privilege() {
            Ok(true) => {
                tracing::debug!("SeTakeOwnershipPrivilege was already enabled on this process");
            }
            Ok(false) => tracing::info!("enabled SeTakeOwnershipPrivilege on this process"),
            Err(reason) => tracing::warn!(
                "SeTakeOwnershipPrivilege is unavailable ({reason}); this process cannot claim \
                 ownership of a %ProgramData%\\Cairn it does not own"
            ),
        }

        match windows_acl::enable_restore_privilege() {
            Ok(true) => {
                tracing::debug!("SeRestorePrivilege was already enabled on this process");
                false
            }
            Ok(false) => {
                tracing::info!(
                    "enabled SeRestorePrivilege for the ownership repair; it is released once the \
                     tree is enforced"
                );
                true
            }
            Err(reason) => {
                tracing::warn!(
                    "SeRestorePrivilege is unavailable ({reason}); a %ProgramData%\\Cairn owned by \
                     another account cannot be restored to SYSTEM from this process. Run \
                     `CairnService --console` from an elevated prompt, or use the installed Windows \
                     service, which runs as LocalSystem and holds the privilege."
                );
                false
            }
        }
    };

    #[cfg(not(windows))]
    let taken_here = false;

    RestorePrivilege { taken_here }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocationPresence {
    Absent,
    Present,
    NotListable,
}

fn location_presence(path: &std::path::Path) -> LocationPresence {
    match std::fs::metadata(path) {
        Ok(_) => LocationPresence::Present,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => LocationPresence::Absent,
        Err(_) => LocationPresence::NotListable,
    }
}

/// Creates the service data tree and enforces the SPEC SET-02 access control on every component of it.
///
/// An existing tree is never trusted: `%ProgramData%` grants `BUILTIN\Users` the right to create
/// files and directories, so a standard account can pre-empt `%ProgramData%\Cairn` and hand the
/// `LocalSystem` service a configuration it controls. Detection, repair, and — when the repair is not
/// possible from this process — refusal to continue are all part of this call.
/// The tree is created first and locked down afterwards, location by location, and every location is
/// attempted: stopping at the first refusal would leave the later ones inherited and writable. The
/// first failure is then returned instead of a layout, so a partially locked tree is never handed to
/// the service as if it were usable.
///
/// Only a location `location_presence` proves absent is created; one that exists while this process
/// is refused the query for it goes straight to the SPEC SET-02 check, so its failure is the access-control
/// failure it is and not a creation failure.
pub fn prepare_data_layout(
    root: &std::path::Path,
) -> Result<ServiceDataLayout, DataDirectoryError> {
    let _ownership_privileges = apply_ownership_privileges();

    let mut layout = ServiceDataLayout::new(root);

    for dir in layout.writable_locations() {
        match location_presence(&dir) {
            LocationPresence::Absent => {
                std::fs::create_dir_all(&dir).map_err(|source| DataDirectoryError::Create {
                    path: dir.clone(),
                    source,
                })?;
            }
            LocationPresence::Present => {}
            LocationPresence::NotListable => {
                tracing::warn!(
                    "{path} is already present but this process cannot read it, so it is not \
                     created again; its access control is enforced as it stands. If the tree belongs \
                     to another account, run `CairnService --console` from an elevated prompt or use the \
                     installed service, which runs as LocalSystem.",
                    path = dir.display()
                );
            }
        }
    }

    let mut failure = None;
    for dir in layout.writable_locations() {
        if let Err(error) = enforce_restricted_acl(&dir) {
            tracing::error!("could not lock down {}: {}", dir.display(), error);
            if failure.is_none() {
                failure = Some(error);
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }

    if layout.config_path.exists() {
        let trust = inspect_path_trust(&layout.config_path)?.ok_or_else(|| {
            DataDirectoryError::Inspect {
                path: layout.config_path.clone(),
                reason: "configuration file could not be read".to_string(),
            }
        })?;
        enforce_restricted_acl(&layout.config_path)?;
        layout.config_untrusted = !(trust.owner_trusted && trust.acl_compliant);
        if layout.config_untrusted {
            tracing::error!(
                "{} was writable outside SYSTEM/Administrators ({}); the ACL was repaired and the \
                 file is ignored for this startup. Re-create it through the elevated helper (SVC-05).",
                layout.config_path.display(),
                trust.summary()
            );
        }
    }

    Ok(layout)
}

/// Applies [`RESTRICTED_SDDL`] to one path and verifies the result. Every write of the service
/// configuration goes through this, so the file never exists with an inherited user-writable DACL.
///
/// The protected DACL and the owner are written as two independent operations: a refusal to
/// restore ownership is reported, but it never cancels the DACL lockdown. See
/// `windows_acl::enforce`.
pub fn enforce_restricted_acl(path: &std::path::Path) -> Result<(), DataDirectoryError> {
    #[cfg(windows)]
    {
        windows_acl::enforce(path)
    }
    #[cfg(not(windows))]
    {
        Err(DataDirectoryError::Repair {
            path: path.to_path_buf(),
            reason: "descriptor control is implemented for Windows only".to_string(),
        })
    }
}

/// Ownership and DACL state of one path, enough to decide whether the service may use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathTrust {
    pub path: std::path::PathBuf,
    /// String form of the owner SID.
    pub owner: String,
    /// Owner is SYSTEM or the built-in Administrators group.
    pub owner_trusted: bool,
    /// DACL is protected and grants access to nothing but SYSTEM and Administrators.
    pub acl_compliant: bool,
    /// Everything that makes this path unusable as it stands, for logs and error messages.
    pub untrusted_grants: Vec<String>,
}

impl PathTrust {
    pub fn summary(&self) -> String {
        format!(
            "owner {} (trusted: {}), DACL compliant: {}, unexpected grants: {:?}",
            self.owner, self.owner_trusted, self.acl_compliant, self.untrusted_grants
        )
    }
}

/// Reads the current owner and DACL of `path`. `Ok(None)` when the path does not exist yet.
pub fn inspect_path_trust(path: &std::path::Path) -> Result<Option<PathTrust>, DataDirectoryError> {
    #[cfg(windows)]
    {
        windows_acl::inspect(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(None)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DataDirectoryError {
    #[error("{path} is a reparse point; a LocalSystem process must not follow it")]
    ReparsePoint { path: std::path::PathBuf },
    #[error("could not create {path}: {source}")]
    Create {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("could not read the security descriptor of {path}: {reason}")]
    Inspect {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("could not apply the SYSTEM/Administrators descriptor on {path}: {reason}")]
    Repair {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error(
        "{path} is owned by {owner}; restoring a trusted owner needs SeRestorePrivilege enabled, which lets this process name any SID as owner ({reason})"
    )]
    Ownership {
        path: std::path::PathBuf,
        owner: String,
        reason: String,
    },
    #[error("{path} still grants access outside SYSTEM/Administrators after repair: {detail}")]
    NotRemediated {
        path: std::path::PathBuf,
        detail: String,
    },
}

#[cfg(windows)]
mod windows_acl {
    use super::{DataDirectoryError, PathTrust, RESTRICTED_SDDL};
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, GetLastError, HANDLE, HLOCAL, LUID,
        LocalFree, SetLastError, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, AdjustTokenPrivileges, DACL_SECURITY_INFORMATION,
        EqualSid, GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
        GetSecurityDescriptorOwner, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SE_DACL_PROTECTED, SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_PRIVILEGES_ATTRIBUTES, TOKEN_QUERY,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{BOOL, PCWSTR, PWSTR};

    const GENERIC_ALL: u32 = 0x1000_0000;
    /// `FILE_ALL_ACCESS`: the form the object store keeps once a `GA` ACE has been written to a
    /// file DACL. [`is_full_control`] accepts both shapes and nothing else.
    const FILE_ALL_ACCESS: u32 = windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS.0;

    fn is_full_control(mask: u32) -> bool {
        mask == GENERIC_ALL || mask == FILE_ALL_ACCESS
    }
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const SID_LOCAL_SYSTEM: &str = "S-1-5-18";
    const SID_BUILTIN_ADMINISTRATORS: &str = "S-1-5-32-544";
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_PATH_NOT_FOUND: u32 = 3;

    struct OwnedSid(PSID);

    impl Drop for OwnedSid {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: LocalAlloc'd block handed out by ConvertStringSidToSidW; the SID is not
                // used after this point.
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.0.0)));
                }
            }
        }
    }

    struct OwnedDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for OwnedDescriptor {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: LocalAlloc'd block handed out by GetNamedSecurityInfoW or
                // ConvertStringSecurityDescriptorToSecurityDescriptorW. Every pointer derived from it
                // is consumed while the guard is still alive.
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.0.0)));
                }
            }
        }
    }

    struct OwnedToken(HANDLE);

    impl Drop for OwnedToken {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: the handle is the one `OpenProcessToken` wrote for this guard alone and no
                // other handle is wrapped here, so closing it cannot invalidate anything still in use.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    fn to_wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn describe_win32(code: WIN32_ERROR) -> String {
        std::io::Error::from_raw_os_error(code.0 as i32).to_string()
    }

    fn is_reparse_point(path: &Path) -> bool {
        let wide = to_wide(path);
        // SAFETY: `wide` is a null-terminated UTF-16 path kept alive across the call.
        let attributes = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
        attributes != INVALID_FILE_ATTRIBUTES && (attributes & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0
    }

    fn trusted_sid(text: &str) -> Option<OwnedSid> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sid = PSID(std::ptr::null_mut());
        // SAFETY: `wide` holds a valid null-terminated SID string; the LocalAlloc'd result is owned
        // by `OwnedSid` from here on.
        unsafe { ConvertStringSidToSidW(PCWSTR(wide.as_ptr()), &mut sid) }.ok()?;
        Some(OwnedSid(sid))
    }

    fn sid_to_string(sid: PSID) -> String {
        let mut wide = PWSTR::null();
        // SAFETY: `sid` points at a valid SID inside a descriptor that outlives this call. The
        // converted string is copied into a Rust String and its LocalAlloc'd buffer is freed.
        unsafe {
            if ConvertSidToStringSidW(sid, &mut wide).is_err() || wide.is_null() {
                return "<unrenderable-sid>".to_string();
            }
            let text = wide.to_string().unwrap_or_default();
            let _ = LocalFree(Some(HLOCAL(wide.0 as *mut core::ffi::c_void)));
            text
        }
    }

    fn is_trusted_sid(sid: PSID, trusted: &[OwnedSid]) -> bool {
        if sid.is_invalid() {
            return false;
        }
        trusted.iter().any(|candidate| {
            // SAFETY: both `sid` and `candidate.0` are valid SIDs and `EqualSid` only reads them.
            unsafe { EqualSid(sid, candidate.0) }.is_ok()
        })
    }

    fn read_descriptor(path: &Path) -> Result<Option<OwnedDescriptor>, DataDirectoryError> {
        let wide = to_wide(path);
        let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        // SAFETY: `wide` is a null-terminated path. Only the descriptor pointer is written here; the
        // SID and ACL pointers it yields are read while the descriptor is alive.
        let error = unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut descriptor,
            )
        };
        if error.is_ok() {
            return Ok(Some(OwnedDescriptor(descriptor)));
        }
        if error.0 == ERROR_FILE_NOT_FOUND || error.0 == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        Err(DataDirectoryError::Inspect {
            path: path.to_path_buf(),
            reason: describe_win32(error),
        })
    }

    pub fn inspect(path: &Path) -> Result<Option<PathTrust>, DataDirectoryError> {
        let Some(descriptor) = read_descriptor(path)? else {
            return Ok(None);
        };
        let trusted: Vec<OwnedSid> = [SID_LOCAL_SYSTEM, SID_BUILTIN_ADMINISTRATORS]
            .into_iter()
            .filter_map(trusted_sid)
            .collect();

        let mut owner = PSID(std::ptr::null_mut());
        let mut owner_defaulted = BOOL::default();
        // SAFETY: `descriptor` is a live self-describing descriptor owned by the guard; `owner` is a
        // writable out-parameter.
        unsafe {
            GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut owner_defaulted).map_err(
                |_| DataDirectoryError::Inspect {
                    path: path.to_path_buf(),
                    reason: "descriptor carries no owner".to_string(),
                },
            )?;
        }

        let mut dacl_present = BOOL::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut dacl_defaulted = BOOL::default();
        // SAFETY: as above; the returned DACL pointer lives as long as `descriptor`.
        unsafe {
            GetSecurityDescriptorDacl(
                descriptor.0,
                &mut dacl_present,
                &mut dacl,
                &mut dacl_defaulted,
            )
            .map_err(|_| DataDirectoryError::Inspect {
                path: path.to_path_buf(),
                reason: "descriptor DACL could not be read".to_string(),
            })?;
        }

        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: as above; both out-parameters are writable and the descriptor stays alive.
        unsafe {
            GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision).map_err(
                |_| DataDirectoryError::Inspect {
                    path: path.to_path_buf(),
                    reason: "descriptor control flags could not be read".to_string(),
                },
            )?;
        }

        let mut grants = Vec::new();
        let ace_count = if !dacl_present.as_bool() {
            grants.push("no DACL present".to_string());
            0
        } else if dacl.is_null() {
            grants.push("NULL DACL grants universal access".to_string());
            0
        } else {
            // SAFETY: a non-null DACL returned by the descriptor is live for this borrow.
            unsafe { (*dacl).AceCount as usize }
        };

        for index in 0..ace_count {
            let mut entry: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `dacl` is non-null and `index` indexes its `AceCount` entries.
            if unsafe { GetAce(dacl as *const ACL, index as u32, &mut entry) }.is_err() {
                grants.push(format!("ACE #{index} could not be read"));
                continue;
            }
            // SAFETY: GetAce returned a pointer to a well-formed ACE inside `dacl`.
            let header = unsafe { &*(entry as *const ACE_HEADER) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
                grants.push(format!("ACE #{index} type {}", header.AceType));
                continue;
            }
            if header.AceFlags != 0 {
                // A DACL this service writes carries no ACE flags; inherited (0x10), propagation
                // and the audit-only success/failure flags (0x40/0x80) are all deviations.
                grants.push(format!("ACE #{index} flags {:#04x}", header.AceFlags));
                continue;
            }
            // SAFETY: the entry was validated as an ACCESS_ALLOWED_ACE just above.
            let ace = unsafe { &*(entry as *const ACCESS_ALLOWED_ACE) };
            let sid = PSID(entry.wrapping_add(core::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart)));
            if !is_trusted_sid(sid, &trusted) {
                grants.push(format!("{} holds {:#010x}", sid_to_string(sid), ace.Mask));
            } else if !is_full_control(ace.Mask) {
                grants.push(format!(
                    "{} holds a partial mask {:#010x}",
                    sid_to_string(sid),
                    ace.Mask
                ));
            }
        }

        let acl_compliant =
            ace_count == trusted.len() && grants.is_empty() && (control & SE_DACL_PROTECTED.0) != 0;

        Ok(Some(PathTrust {
            path: path.to_path_buf(),
            owner: sid_to_string(owner),
            owner_trusted: is_trusted_sid(owner, &trusted),
            acl_compliant,
            untrusted_grants: grants,
        }))
    }

    struct ParsedDescriptor {
        descriptor: OwnedDescriptor,
    }

    impl ParsedDescriptor {
        fn dacl(&self) -> Option<*const ACL> {
            let mut present = BOOL::default();
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut defaulted = BOOL::default();
            // SAFETY: `self.descriptor` is a valid self-relative descriptor and the ACL it yields
            // lives as long as the borrow of self.
            unsafe {
                GetSecurityDescriptorDacl(
                    self.descriptor.0,
                    &mut present,
                    &mut dacl,
                    &mut defaulted,
                )
            }
            .ok()?;
            (!dacl.is_null() && present.as_bool()).then_some(dacl as *const ACL)
        }

        fn owner(&self) -> Option<PSID> {
            let mut owner = PSID(std::ptr::null_mut());
            let mut defaulted = BOOL::default();
            // SAFETY: as in `dacl`; the SID pointer belongs to the borrowed descriptor.
            unsafe { GetSecurityDescriptorOwner(self.descriptor.0, &mut owner, &mut defaulted) }
                .ok()?;
            (!owner.is_invalid()).then_some(owner)
        }
    }

    fn parse_descriptor(path: &Path, sddl: &str) -> Result<ParsedDescriptor, DataDirectoryError> {
        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut raw_descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        // SAFETY: `wide_sddl` is a valid null-terminated SDDL string.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut raw_descriptor,
                None,
            )
        }
        .map_err(|_| DataDirectoryError::Repair {
            path: path.to_path_buf(),
            reason: format!("{sddl} could not be parsed"),
        })?;
        let parsed = ParsedDescriptor {
            descriptor: OwnedDescriptor(raw_descriptor),
        };
        if parsed.dacl().is_none() {
            return Err(DataDirectoryError::Repair {
                path: path.to_path_buf(),
                reason: format!("{sddl} carries no readable DACL"),
            });
        }
        Ok(parsed)
    }

    fn apply_dacl(
        path: &Path,
        parsed: &ParsedDescriptor,
    ) -> Result<Option<String>, DataDirectoryError> {
        let wide_path = to_wide(path);
        let dacl = parsed.dacl().ok_or_else(|| DataDirectoryError::Repair {
            path: path.to_path_buf(),
            reason: "descriptor built from the SDDL holds no DACL".to_string(),
        })?;
        // SAFETY: `wide_path` is a null-terminated path; `parsed` is borrowed across the call, so the
        // ACL pointer stays valid. Owner and group are NULL and their bits are clear, so the stored
        // identity is untouched.
        let error = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
        };
        Ok(if error.is_ok() {
            None
        } else {
            Some(describe_win32(error))
        })
    }

    /// `AdjustTokenPrivileges` returns success even when the privilege is not part of the token at
    /// all, so the trailing `ERROR_NOT_ALL_ASSIGNED` is the only honest result: the last-error slot is
    /// cleared immediately before the call and read immediately after it, with no other Win32 call in
    /// between that could overwrite it.
    fn adjust_privilege(label: &str, name: PCWSTR, enable: bool) -> Result<bool, String> {
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SAFETY: `luid` is a writable out-parameter kept alive across the call; a NULL system name
        // selects the local machine, and `name` is a static wide string.
        unsafe { LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid) }
            .map_err(|error| format!("{label} could not be resolved: {error}"))?;

        let mut raw_token = HANDLE(std::ptr::null_mut());
        // SAFETY: `GetCurrentProcess` yields the current-process pseudo-handle, which is what
        // `OpenProcessToken` expects and must never be closed; `raw_token` is written only on success
        // and the real handle it then holds is moved into the guard below.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
                &mut raw_token,
            )
        }
        .map_err(|error| format!("this process token could not be opened: {error}"))?;
        let token = OwnedToken(raw_token);

        let request = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: if enable {
                    SE_PRIVILEGE_ENABLED
                } else {
                    TOKEN_PRIVILEGES_ATTRIBUTES::default()
                },
            }],
        };
        let mut previous = TOKEN_PRIVILEGES::default();
        let mut previous_length = 0u32;

        // SAFETY: `token.0` stays live because the guard owns it for the whole call; `request` and
        // `previous` are one-entry `TOKEN_PRIVILEGES` values that outlive it, and the buffer length
        // passed is the exact size of `previous`, so nothing is written outside it.
        unsafe {
            SetLastError(ERROR_SUCCESS);
            AdjustTokenPrivileges(
                token.0,
                false,
                Some(&request),
                std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
                Some(&mut previous),
                Some(&mut previous_length),
            )
        }
        .map_err(|error| format!("{label} could not be adjusted: {error}"))?;

        // SAFETY: reads the calling thread's last error, which since it was cleared can only have been
        // set by the adjustment above.
        let code = unsafe { GetLastError() };
        if code != ERROR_SUCCESS {
            return Err(if code == ERROR_NOT_ALL_ASSIGNED {
                "it is not part of this process token".to_string()
            } else {
                describe_win32(code)
            });
        }

        Ok(previous.Privileges[0].Attributes & SE_PRIVILEGE_ENABLED == SE_PRIVILEGE_ENABLED)
    }

    pub fn enable_take_ownership_privilege() -> Result<bool, String> {
        adjust_privilege("SeTakeOwnershipPrivilege", SE_TAKE_OWNERSHIP_NAME, true)
    }

    pub fn enable_restore_privilege() -> Result<bool, String> {
        adjust_privilege("SeRestorePrivilege", SE_RESTORE_NAME, true)
    }

    pub fn disable_restore_privilege() -> Result<(), String> {
        adjust_privilege("SeRestorePrivilege", SE_RESTORE_NAME, false).map(|_| ())
    }

    fn apply_owner(
        path: &Path,
        parsed: &ParsedDescriptor,
    ) -> Result<Option<String>, DataDirectoryError> {
        let wide_path = to_wide(path);
        let Some(owner) = parsed.owner() else {
            return Ok(Some(
                "the descriptor of this path exposes no owner SID".to_string(),
            ));
        };
        // SAFETY: as in `apply_dacl`; the borrowed descriptor keeps the owner SID valid.
        let error = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(owner),
                None,
                None,
                None,
            )
        };
        Ok(if error.is_ok() {
            None
        } else {
            Some(describe_win32(error))
        })
    }

    // See docs/engineering/ipc-security.md#4-权限使用.
    pub fn enforce(path: &Path) -> Result<(), DataDirectoryError> {
        if is_reparse_point(path) {
            return Err(DataDirectoryError::ReparsePoint {
                path: path.to_path_buf(),
            });
        }

        let before = inspect(path)?.ok_or_else(|| DataDirectoryError::Inspect {
            path: path.to_path_buf(),
            reason: "path does not exist".to_string(),
        })?;
        let parsed = parse_descriptor(path, RESTRICTED_SDDL)?;

        if let Some(reason) = apply_dacl(path, &parsed)? {
            return Err(DataDirectoryError::Repair {
                path: path.to_path_buf(),
                reason,
            });
        }

        if before.owner_trusted {
            return verify(path, true);
        }

        let owner_failure = apply_owner(path, &parsed)?;
        verify(path, owner_failure.is_none())?;
        match owner_failure {
            None => Ok(()),
            Some(reason) => {
                tracing::warn!(
                    "locked down {} to SYSTEM/Administrators but could not restore its owner: {reason}",
                    path.display()
                );
                Err(DataDirectoryError::Ownership {
                    path: path.to_path_buf(),
                    owner: before.owner,
                    reason,
                })
            }
        }
    }

    fn verify(path: &Path, require_trusted_owner: bool) -> Result<(), DataDirectoryError> {
        let after = inspect(path)?.ok_or_else(|| DataDirectoryError::NotRemediated {
            path: path.to_path_buf(),
            detail: "path disappeared during repair".to_string(),
        })?;
        let owner_ok = !require_trusted_owner || after.owner_trusted;
        if after.acl_compliant && owner_ok {
            tracing::info!(
                "enforced SYSTEM/Administrators descriptor on {}",
                path.display()
            );
            return Ok(());
        }
        Err(DataDirectoryError::NotRemediated {
            path: PathBuf::from(path),
            detail: after.summary(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(windows)]
    fn apply_sddl(path: &std::path::Path, sddl: &str) -> Result<(), DataDirectoryError> {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        };
        use windows::Win32::Security::{
            ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };
        use windows::core::{BOOL, PCWSTR};

        struct DescriptorGuard(PSECURITY_DESCRIPTOR);
        impl Drop for DescriptorGuard {
            fn drop(&mut self) {
                if !self.0.0.is_null() {
                    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
                    let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
                }
            }
        }

        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut raw = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        // SAFETY: `wide_sddl` is a null-terminated SDDL string.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }
        .map_err(|_| DataDirectoryError::Repair {
            path: path.to_path_buf(),
            reason: format!("{sddl} could not be parsed"),
        })?;
        let guard = DescriptorGuard(raw);

        let mut dacl_present = BOOL::default();
        let mut dacl_ptr: *mut ACL = std::ptr::null_mut();
        let mut defaulted = BOOL::default();
        // SAFETY: `guard.0` holds a live security descriptor from the conversion above.
        unsafe {
            GetSecurityDescriptorDacl(guard.0, &mut dacl_present, &mut dacl_ptr, &mut defaulted)
        }
        .map_err(|err| DataDirectoryError::Repair {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
        if !dacl_present.as_bool() || dacl_ptr.is_null() {
            return Err(DataDirectoryError::Repair {
                path: path.to_path_buf(),
                reason: format!("{sddl} carries no readable DACL"),
            });
        }

        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide_path` is null-terminated and `dacl_ptr` points inside `guard.0`.
        let status = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl_ptr),
                None,
            )
        };
        if status.is_ok() {
            Ok(())
        } else {
            Err(DataDirectoryError::Repair {
                path: path.to_path_buf(),
                reason: format!("SetNamedSecurityInfoW failed: {status:?}"),
            })
        }
    }

    #[cfg(windows)]
    fn take_ownership_privilege_is_effective() -> bool {
        use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
        use windows::Win32::Security::{
            LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, PRIVILEGE_SET, PrivilegeCheck,
            SE_TAKE_OWNERSHIP_NAME, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use windows::core::{BOOL, PCWSTR};

        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SAFETY: `luid` is a writable out-parameter that outlives the call.
        if unsafe { LookupPrivilegeValueW(PCWSTR::null(), SE_TAKE_OWNERSHIP_NAME, &mut luid) }
            .is_err()
        {
            return false;
        }
        let mut token = HANDLE(std::ptr::null_mut());
        // SAFETY: TOKEN_QUERY queries the current process token without modifying it.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return false;
        }
        let mut required = PRIVILEGE_SET {
            PrivilegeCount: 1,
            Control: 0,
            Privilege: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: Default::default(),
            }],
        };
        let mut result = BOOL::default();
        // SAFETY: `token` is open and `required`/`result` outlive the call.
        let ok = unsafe { PrivilegeCheck(token, &mut required, &mut result) }
            .is_ok_and(|_| result.as_bool());
        // SAFETY: `token` was opened above.
        let _ = unsafe { CloseHandle(token) };
        ok
    }

    #[test]
    fn restricted_descriptor_grants_nothing_to_users() {
        let dacl = &RESTRICTED_SDDL[RESTRICTED_SDDL.find("D:").expect("DACL present")..];
        assert!(
            dacl.starts_with("D:P"),
            "inheritance must be disabled: {dacl}"
        );
        assert!(dacl.contains("(A;;GA;;;SY)"), "{dacl}");
        assert!(dacl.contains("(A;;GA;;;BA)"), "{dacl}");
        for forbidden in ["IU", "BU", "WD", "WO", "AU", "S-1-5-32-545", "S-1-1-0"] {
            assert!(
                !RESTRICTED_SDDL.contains(forbidden),
                "{forbidden} must not be granted"
            );
        }
        assert!(
            RESTRICTED_SDDL.starts_with("O:SYG:SY"),
            "owner and group must be SYSTEM"
        );
    }

    #[test]
    fn data_layout_paths_match_set_02() {
        let layout = ServiceDataLayout::new(std::path::Path::new(r"C:\ProgramData\Cairn"));
        assert_eq!(layout.root, PathBuf::from(r"C:\ProgramData\Cairn"));
        assert_eq!(
            layout.index_dir,
            PathBuf::from(r"C:\ProgramData\Cairn\index")
        );
        assert_eq!(
            layout.content_dir,
            PathBuf::from(r"C:\ProgramData\Cairn\content")
        );
        assert_eq!(layout.logs_dir, PathBuf::from(r"C:\ProgramData\Cairn\logs"));
        assert_eq!(
            layout.config_path,
            PathBuf::from(r"C:\ProgramData\Cairn\service.toml")
        );
        assert!(!layout.config_untrusted);
        assert_eq!(layout.writable_locations().len(), 4);
    }

    #[test]
    fn location_presence_only_calls_a_missing_path_absent() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let base = std::env::temp_dir().join(format!(
            "cairn-presence-probe-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).expect("probe base");
        let present = base.join("index");
        std::fs::create_dir_all(&present).expect("probe directory");
        let file = base.join("service.toml");
        std::fs::write(&file, "").expect("probe file");

        assert_eq!(location_presence(&present), LocationPresence::Present);
        assert_eq!(
            location_presence(&base.join("logs")),
            LocationPresence::Absent,
            "a location that was never created must still be creatable"
        );
        assert_eq!(location_presence(&file), LocationPresence::Present);
        assert_eq!(
            location_presence(&base.join("absent-parent").join("index")),
            LocationPresence::Absent
        );

        std::fs::remove_dir_all(&base).expect("probe base removable");
    }

    #[test]
    fn pipe_dacl_keeps_standard_users_able_to_connect() {
        assert!(DEFAULT_SDDL.contains("SY"), "{DEFAULT_SDDL}");
        assert!(DEFAULT_SDDL.contains("BA"), "{DEFAULT_SDDL}");
        assert!(DEFAULT_SDDL.contains("(A;;GRGW;;;IU)"), "{DEFAULT_SDDL}");
        assert!(!DEFAULT_SDDL.contains("NU"), "{DEFAULT_SDDL}");
        assert!(!DEFAULT_SDDL.contains("WD"), "{DEFAULT_SDDL}");

        assert!(ADMIN_ONLY_SDDL.contains("SY"), "{ADMIN_ONLY_SDDL}");
        assert!(ADMIN_ONLY_SDDL.contains("BA"), "{ADMIN_ONLY_SDDL}");
        assert!(!ADMIN_ONLY_SDDL.contains("IU"), "{ADMIN_ONLY_SDDL}");
        assert!(!ADMIN_ONLY_SDDL.contains("NU"), "{ADMIN_ONLY_SDDL}");
        assert!(!ADMIN_ONLY_SDDL.contains("WD"), "{ADMIN_ONLY_SDDL}");

        #[cfg(windows)]
        {
            let default_sd = SafeSecurityDescriptor::create(false)
                .expect("default pipe security descriptor builds cleanly");
            assert!(!default_sd.sa.lpSecurityDescriptor.is_null());

            let admin_sd = SafeSecurityDescriptor::create(true)
                .expect("admin-only pipe security descriptor builds cleanly");
            assert!(!admin_sd.sa.lpSecurityDescriptor.is_null());
        }
    }

    #[test]
    fn untrusted_grant_summary_names_the_offender() {
        let trust = PathTrust {
            path: PathBuf::from(r"C:\ProgramData\Cairn"),
            owner: "S-1-5-21-1-1-1-1001".to_string(),
            owner_trusted: false,
            acl_compliant: false,
            untrusted_grants: vec!["S-1-5-32-545 holds 0x1201bf".to_string()],
        };
        let summary = trust.summary();
        assert!(summary.contains("S-1-5-32-545"), "{summary}");
        assert!(summary.contains("trusted: false"), "{summary}");
    }

    #[cfg(windows)]
    fn fresh_probe_root(prefix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("cairn-{prefix}-{}-{stamp}", std::process::id()))
    }

    #[cfg(windows)]
    #[test]
    fn enforce_locks_a_real_directory_down_and_reports_ownership_needs() {
        let probe = fresh_probe_root("acl-probe");
        std::fs::create_dir_all(&probe).expect("probe directory");

        let result = enforce_restricted_acl(&probe);
        let trust = inspect_path_trust(&probe)
            .expect("inspection after repair")
            .expect("probe still present");

        let cleanup = apply_sddl(&probe, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)");
        let removed = std::fs::remove_dir(&probe);

        assert!(
            trust.acl_compliant,
            "DACL not locked down: {}",
            trust.summary()
        );
        match result {
            Ok(()) => assert!(trust.owner_trusted, "{}", trust.summary()),
            Err(DataDirectoryError::Ownership { owner, .. }) => {
                assert!(!trust.owner_trusted);
                assert_ne!(owner, "S-1-5-18");
            }
            Err(other) => panic!("unexpected failure: {other}"),
        }
        cleanup.expect("cleanup descriptor");
        removed.expect("probe directory removable after cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn a_partial_grant_is_not_mistaken_for_the_restricted_descriptor() {
        let probe = fresh_probe_root("mask-probe");
        std::fs::create_dir_all(&probe).expect("probe directory");

        apply_sddl(&probe, "D:P(A;;GR;;;SY)(A;;GR;;;BA)")
            .expect("write a partial-grant descriptor");
        let trust = inspect_path_trust(&probe)
            .expect("inspect")
            .expect("present");
        assert!(
            !trust.acl_compliant,
            "a read-only grant passed: {}",
            trust.summary()
        );
        assert!(
            trust
                .untrusted_grants
                .iter()
                .any(|grant| grant.contains("partial mask")),
            "{:?}",
            trust.untrusted_grants
        );

        apply_sddl(&probe, RESTRICTED_SDDL).expect("write the restricted descriptor");
        let trust = inspect_path_trust(&probe)
            .expect("inspect")
            .expect("present");
        assert!(trust.acl_compliant, "{}", trust.summary());

        apply_sddl(&probe, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)").expect("cleanup descriptor");
        std::fs::remove_dir(&probe).expect("probe removable after cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn prepare_data_layout_creates_every_service_location_or_refuses_to_continue() {
        let probe = fresh_probe_root("tree-probe");
        let outcome = prepare_data_layout(&probe);
        let layout = ServiceDataLayout::new(&probe);

        for dir in layout.writable_locations() {
            let trust = inspect_path_trust(&dir)
                .expect("inspect")
                .unwrap_or_else(|| panic!("{} must exist", dir.display()));
            assert!(
                trust.acl_compliant,
                "{}: {}",
                dir.display(),
                trust.summary()
            );
        }

        match outcome {
            Ok(created) => assert_eq!(created.root, probe),
            Err(
                DataDirectoryError::Ownership { .. } | DataDirectoryError::NotRemediated { .. },
            ) => {}
            Err(other) => panic!("unexpected failure: {other}"),
        }

        let _ = apply_sddl(&probe, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)");
        for dir in [&layout.logs_dir, &layout.content_dir, &layout.index_dir] {
            let _ = apply_sddl(dir, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)");
            let _ = std::fs::remove_dir(dir);
        }
        let _ = std::fs::remove_dir(&probe);
    }

    #[cfg(windows)]
    #[test]
    fn an_existing_but_unlistable_tree_is_enforced_rather_than_failed_to_create() {
        let probe = fresh_probe_root("unlistable-tree");
        let layout = ServiceDataLayout::new(&probe);
        for dir in layout.writable_locations() {
            std::fs::create_dir_all(&dir).expect("probe tree");
        }

        apply_sddl(&probe, RESTRICTED_SDDL).expect("lock the probe root down");

        let cleanup = |probe: &std::path::Path| {
            for dir in ServiceDataLayout::new(probe).writable_locations() {
                let _ = apply_sddl(&dir, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)");
            }
            let _ = std::fs::remove_dir_all(probe);
        };

        if location_presence(&layout.index_dir) != LocationPresence::NotListable {
            eprintln!(
                "this token can still list the locked-down parent; unlistable-tree check not exercised"
            );
            cleanup(&probe);
            return;
        }

        assert_eq!(
            location_presence(&probe.join("never-created")),
            LocationPresence::Absent,
            "a missing location stopped being recognised as creatable behind a locked-down parent"
        );

        let outcome = prepare_data_layout(&probe);

        if let Err(error) = &outcome {
            assert!(
                !matches!(error, DataDirectoryError::Create { .. }),
                "an existing tree was reported as un-creatable: {error}"
            );
            assert!(
                matches!(
                    error,
                    DataDirectoryError::Ownership { .. }
                        | DataDirectoryError::Repair { .. }
                        | DataDirectoryError::NotRemediated { .. }
                        | DataDirectoryError::Inspect { .. }
                ),
                "unexpected failure category: {error}"
            );
        }

        let trust = inspect_path_trust(&layout.index_dir)
            .expect("inspect index")
            .expect("index still present");
        assert!(
            trust.acl_compliant,
            "index was never locked down: {}",
            trust.summary()
        );

        cleanup(&probe);
    }

    #[cfg(windows)]
    #[test]
    fn reparse_point_is_refused_instead_of_followed() {
        let base = std::env::temp_dir().join(format!("cairn-link-probe-{}", std::process::id()));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).expect("target directory");

        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .is_ok_and(|out| out.status.success());

        if made {
            let error = enforce_restricted_acl(&link)
                .expect_err("a junction must never be followed by a privileged process");
            assert!(
                matches!(error, DataDirectoryError::ReparsePoint { .. }),
                "unexpected: {error}"
            );
        }
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&base);
        if !made {
            eprintln!("junction could not be created; reparse check not exercised");
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_privilege_report_agrees_with_the_token_that_holds_it() {
        let adjusted = super::windows_acl::enable_take_ownership_privilege();
        let in_effect = take_ownership_privilege_is_effective();

        match adjusted {
            Ok(_) => assert!(
                in_effect,
                "claimed the privilege is usable while the token does not hold it enabled"
            ),
            Err(reason) => assert!(
                !in_effect,
                "reported {reason} while the token does hold the privilege enabled"
            ),
        }
    }
}

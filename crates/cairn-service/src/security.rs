// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Win32 security descriptors: the named pipe DACL and the access control of the service data
//! locations fixed by SPEC SET-02.

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{LocalFree, HLOCAL};
#[cfg(windows)]
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
#[cfg(windows)]
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

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
                1, // SDDL_REVISION_1
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
                let _ = LocalFree(HLOCAL(self.sd_ptr.0));
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
    /// LocalSystem service to hot-apply (SVC-05).
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

/// Held for exactly as long as the SPEC SET-02 ownership repair needs `SeRestorePrivilege`.
struct RestorePrivilege {
    /// `true` only when this process turned it on. The LocalSystem service already holds it, and
    /// starting up must not take it away from the account that is meant to have it.
    taken_here: bool,
}

impl Drop for RestorePrivilege {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.taken_here {
            if let Err(reason) = windows_acl::disable_restore_privilege() {
                tracing::warn!(
                    "SeRestorePrivilege could not be put back ({reason}); this process keeps the \
                     right to name any SID as an owner until it exits"
                );
            }
        }
    }
}

/// Puts this process into the state the SPEC SET-02 ownership repair needs, on a best-effort basis.
///
/// [`windows_acl::enforce`] writes the protected DACL first and only then restores the SYSTEM owner.
/// That second step names a SID the caller is not, which requires **two** privileges:
/// `SeTakeOwnershipPrivilege` lets a holder set an owner only among the
/// SIDs it may legitimately claim — an elevated administrator can claim itself, not `SY` — while
/// `SeRestorePrivilege` is what Microsoft documents as permitting any valid SID as owner. With only
/// the first one enabled, an elevated `cairnd --console` still fails the owner write with
/// `ERROR_INVALID_OWNER`.
///
/// Neither adjustment changes any outcome. A process that still cannot restore ownership fails closed
/// exactly as before, because a privilege is a capability of the token and never a relaxation of the
/// descriptor requirement — nothing here widens `RESTRICTED_SDDL`, skips a verification, or turns a
/// refusal into an acceptance. `SeRestorePrivilege` is released again once the tree has been enforced,
/// because it grants write access control over any file regardless of that file's ACL and no indexer
/// needs that lying around.
///
/// The logging keeps the service quiet without a console-only gate: the already-enabled case, which is
/// what LocalSystem sees on every start, is recorded at `debug`, while the warning only ever fires for
/// a token that genuinely lacks the privilege. Gating that warning on `--console` instead would
/// silence the only signal that the *service* account has been stripped of it, which is a real
/// misconfiguration worth hearing about.
fn apply_ownership_privileges() -> RestorePrivilege {
    #[cfg(windows)]
    let taken_here = {
        match windows_acl::enable_take_ownership_privilege() {
            Ok(true) => {
                tracing::debug!("SeTakeOwnershipPrivilege was already enabled on this process")
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
                     `cairnd --console` from an elevated prompt, or use the installed Windows \
                     service, which runs as LocalSystem and holds the privilege."
                );
                false
            }
        }
    };

    #[cfg(not(windows))]
    // Nothing to enable: descriptor control has no implementation here, so
    // [`enforce_restricted_acl`] already refuses every location and startup fails closed for
    // that reason alone.
    let taken_here = false;

    RestorePrivilege { taken_here }
}

/// Whether one service data location still has to be created.
///
/// `Path::exists()` cannot answer this. It reports `false` both when nothing is at the path and when
/// the directory *is* there but this process may not query it, and the second case is the normal state
/// of a `%ProgramData%\Cairn` that a LocalSystem run has already locked down: SPEC SET-02 grants
/// `BUILTIN\Users` nothing at all, so the attribute query a standard account makes for
/// `%ProgramData%\Cairn\index` is refused while the directory sits there. Reading that refusal as
/// absence sends `create_dir_all` into an existing directory, `ERROR_ALREADY_EXISTS` (os error 183)
/// comes back as [`DataDirectoryError::Create`], and the run ends there: the locations never reach
/// [`enforce_restricted_acl`], so the ownership refusal the operator actually has to act on is never
/// reported. The tree is no more usable afterwards — startup still fails closed — only the diagnosis
/// is wrong.
///
/// The two states separate on the I/O error *kind* rather than on a Win32 code guess: a path that is
/// genuinely missing is `NotFound`, one the caller is barred from is `PermissionDenied`. Measured on a
/// locked-down parent, a name that was never created still answers `NotFound`, so the creation path
/// stays reachable; a name that exists answers the refusal, and a refusal is not evidence of absence.
/// Any other failure has an unknown cause, which is no more evidence of absence either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocationPresence {
    /// Nothing is at the path, so the location has to be created before it can be locked down.
    Absent,
    /// The location is there and its attributes are readable.
    Present,
    /// The location is there, but this process is refused the query that would show it. Creation must
    /// not be attempted; the descriptor check decides what its access control is, and it reaches the
    /// object through the owner's `READ_CONTROL` right rather than through the directory entry.
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
/// LocalSystem service a configuration it controls. Detection, repair, and — when the repair is not
/// possible from this process — refusal to continue are all part of this call.
pub fn prepare_service_data_layout() -> Result<ServiceDataLayout, DataDirectoryError> {
    prepare_data_layout(&default_service_data_root())
}

/// [`prepare_service_data_layout`] for an explicit root; used by tests and by service startup.
///
/// The tree is created first and locked down afterwards, location by location, and every location is
/// attempted: stopping at the first refusal would leave the later ones inherited and writable. The
/// first failure is then returned instead of a layout, so a partially locked tree is never handed to
/// the service as if it were usable.
///
/// Only a location [`location_presence`] proves absent is created; one that exists while this process
/// is refused the query for it goes straight to the SPEC SET-02 check, so its failure is the access-control
/// failure it is and not a creation failure.
pub fn prepare_data_layout(
    root: &std::path::Path,
) -> Result<ServiceDataLayout, DataDirectoryError> {
    // Held until the tree has been enforced; dropping it releases `SeRestorePrivilege` again.
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
                // Nothing here may reach into a location this process cannot even resolve: the
                // descriptor check below is what decides whether startup continues, and it reads the
                // stored owner and DACL rather than the directory entry.
                tracing::warn!(
                    "{path} is already present but this process cannot read it, so it is not \
                     created again; its access control is enforced as it stands. If the tree belongs \
                     to another account, run `cairnd --console` from an elevated prompt or use the \
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
/// [`windows_acl::enforce`].
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
    #[error("{path} is owned by {owner}; restoring a trusted owner needs SeRestorePrivilege enabled, which lets this process name any SID as owner ({reason})")]
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
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, SetLastError, BOOL, ERROR_NOT_ALL_ASSIGNED,
        ERROR_SUCCESS, HANDLE, HLOCAL, LUID, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, LookupPrivilegeValueW,
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, LUID_AND_ATTRIBUTES,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SE_DACL_PROTECTED, SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_PRIVILEGES_ATTRIBUTES, TOKEN_QUERY,
    };
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesW, FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const GENERIC_ALL: u32 = 0x1000_0000;
    /// `FILE_ALL_ACCESS`, the standard-plus-specific form the object store keeps once a `GA` ACE has
    /// been written to a file DACL. [`is_full_control`] accepts both shapes and nothing else.
    const FILE_ALL_ACCESS: u32 = 0x001f_01ff;

    /// Whether `mask` is exactly the full control this service writes, generic or already expanded.
    /// Any other value — a narrower grant, extra bits, or an unexpected combination — is a deviation
    /// from [`RESTRICTED_SDDL`] and makes the location non-compliant.
    fn is_full_control(mask: u32) -> bool {
        mask == GENERIC_ALL || mask == FILE_ALL_ACCESS
    }
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    /// `ACE_HEADER` is 4 bytes and `Mask` adds 4, so the variable-length SID of an
    /// `ACCESS_ALLOWED_ACE` starts at offset 8.
    const SID_OFFSET_IN_ACCESS_ALLOWED_ACE: usize = 8;
    const SID_LOCAL_SYSTEM: &str = "S-1-5-18";
    const SID_BUILTIN_ADMINISTRATORS: &str = "S-1-5-32-544";
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_PATH_NOT_FOUND: u32 = 3;

    /// A `PSID` allocated by `ConvertStringSidToSidW`, released with `LocalFree`.
    struct OwnedSid(PSID);

    impl Drop for OwnedSid {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: LocalAlloc'd block handed out by ConvertStringSidToSidW; the SID is not
                // used after this point.
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 .0));
                }
            }
        }
    }

    /// A `PSECURITY_DESCRIPTOR` allocated by advapi32, released with `LocalFree`.
    struct OwnedDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for OwnedDescriptor {
        fn drop(&mut self) {
            if !self.0 .0.is_null() {
                // SAFETY: LocalAlloc'd block handed out by GetNamedSecurityInfoW or
                // ConvertStringSecurityDescriptorToSecurityDescriptorW. Every pointer derived from it
                // is consumed while the guard is still alive.
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0 .0));
                }
            }
        }
    }

    /// A process token handle opened by `OpenProcessToken`, released with `CloseHandle`.
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

    /// Builds one of the two SIDs that may own a service data location.
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
            let _ = LocalFree(HLOCAL(wide.0 as *mut core::ffi::c_void));
            text
        }
    }

    fn is_trusted_sid(sid: PSID, trusted: &[OwnedSid]) -> bool {
        !sid.is_invalid()
            // SAFETY: both operands are valid SIDs and EqualSid only compares their contents.
            && trusted
                .iter()
                .any(|candidate| unsafe { EqualSid(sid, candidate.0) }.is_ok())
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

    /// Reads the stored owner and DACL of `path` and reports whether they are what SPEC SET-02 requires.
    ///
    /// Existence comes from the descriptor read itself: a location this process just locked down can
    /// no longer be stat'd (the owner holds implicit `READ_CONTROL`, not `FILE_READ_ATTRIBUTES`), and
    /// the verification step has to be able to read exactly that object back.
    pub fn inspect(path: &Path) -> Result<Option<PathTrust>, DataDirectoryError> {
        let Some(descriptor) = read_descriptor(path)? else {
            return Ok(None);
        };
        let trusted: Vec<OwnedSid> = [SID_LOCAL_SYSTEM, SID_BUILTIN_ADMINISTRATORS]
            .into_iter()
            .filter_map(trusted_sid)
            .collect();

        // SAFETY: `descriptor` is a self-describing security descriptor kept alive by the guard for
        // the whole block; `owner`, `dacl` and every ACE pointer below belong to it.
        unsafe {
            let mut owner = PSID(std::ptr::null_mut());
            let mut owner_defaulted = BOOL::default();
            GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut owner_defaulted).map_err(
                |_| DataDirectoryError::Inspect {
                    path: path.to_path_buf(),
                    reason: "descriptor carries no owner".to_string(),
                },
            )?;

            let mut dacl_present = BOOL::default();
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut dacl_defaulted = BOOL::default();
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

            let mut control = 0u16;
            let mut revision = 0u32;
            GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision).map_err(
                |_| DataDirectoryError::Inspect {
                    path: path.to_path_buf(),
                    reason: "descriptor control flags could not be read".to_string(),
                },
            )?;

            let mut grants = Vec::new();
            // A NULL DACL grants universal access; an absent DACL is just as unusable here.
            let ace_count = if !dacl_present.as_bool() {
                grants.push("no DACL present".to_string());
                0
            } else if dacl.is_null() {
                grants.push("NULL DACL grants universal access".to_string());
                0
            } else {
                (*dacl).AceCount as usize
            };

            for index in 0..ace_count {
                let mut entry: *mut core::ffi::c_void = std::ptr::null_mut();
                if GetAce(dacl as *const ACL, index as u32, &mut entry).is_err() {
                    grants.push(format!("ACE #{index} could not be read"));
                    continue;
                }
                // SAFETY: GetAce returned a pointer to a well-formed ACE inside `dacl`.
                let header = &*(entry as *const ACE_HEADER);
                if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
                    grants.push(format!("ACE #{index} type {}", header.AceType));
                    continue;
                }
                if header.AceFlags != 0 {
                    // Inherited (0x80) or object-carrying ACEs are not what this service writes.
                    grants.push(format!("ACE #{index} flags {:#04x}", header.AceFlags));
                    continue;
                }
                // SAFETY: the entry was validated as an ACCESS_ALLOWED_ACE just above.
                let ace = &*(entry as *const ACCESS_ALLOWED_ACE);
                // SAFETY: the SID of an ACCESS_ALLOWED_ACE starts at a fixed offset in the ACE.
                let sid = PSID(entry.wrapping_add(SID_OFFSET_IN_ACCESS_ALLOWED_ACE));
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

            let acl_compliant = ace_count == trusted.len()
                && grants.is_empty()
                && (control & SE_DACL_PROTECTED.0) != 0;

            Ok(Some(PathTrust {
                path: path.to_path_buf(),
                owner: sid_to_string(owner),
                owner_trusted: is_trusted_sid(owner, &trusted),
                acl_compliant,
                untrusted_grants: grants,
            }))
        }
    }

    /// A descriptor parsed from an SDDL string, plus the two components SPEC SET-02 writes.
    ///
    /// The DACL and owner are re-read from the owned descriptor on every access: the `&self` borrow is
    /// what proves the returned pointers cannot outlive the allocation that owns them.
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

    /// Parses `sddl` and rejects a descriptor without a readable DACL, so an empty or NULL DACL can
    /// never be pushed onto a real path. The owner is only required by [`apply_owner`].
    fn parse_descriptor(path: &Path, sddl: &str) -> Result<ParsedDescriptor, DataDirectoryError> {
        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut raw_descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        // SAFETY: `wide_sddl` is a valid null-terminated SDDL string.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                1, // SDDL_REVISION_1
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

    /// Writes only the protected DACL onto `path`.
    ///
    /// The object owner holds an implicit `WRITE_DAC` right, so this step succeeds even in a
    /// non-privileged process; severing inheritance is the part that keeps `%ProgramData%\Cairn` from
    /// the `BUILTIN\Users` grants of its parent. `Ok(None)` means the write was accepted.
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
                PSID(std::ptr::null_mut()),
                PSID(std::ptr::null_mut()),
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

    /// Moves one privilege on this process token to `enable`, and reports whether the token already
    /// held that state.
    ///
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
                BOOL::from(false),
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

    /// Enables `SeTakeOwnershipPrivilege`, which lets a holder set an owner **only among the SIDs it
    /// may legitimately claim** — for an elevated administrator, itself.
    pub fn enable_take_ownership_privilege() -> Result<bool, String> {
        adjust_privilege("SeTakeOwnershipPrivilege", SE_TAKE_OWNERSHIP_NAME, true)
    }

    /// Enables `SeRestorePrivilege`, the one privilege that permits naming **any valid SID** as an
    /// object's owner. [`apply_owner`] needs it, and `SeTakeOwnershipPrivilege` alone does not
    /// provide it: an elevated `cairnd --console` taking ownership may set its own SID, not `SY`.
    pub fn enable_restore_privilege() -> Result<bool, String> {
        adjust_privilege("SeRestorePrivilege", SE_RESTORE_NAME, true)
    }

    /// Puts `SeRestorePrivilege` back the way it was. Granting it means "write access control to any
    /// file regardless of the ACL", so a long-running indexer must not keep it for the ownership
    /// repair alone.
    pub fn disable_restore_privilege() -> Result<(), String> {
        adjust_privilege("SeRestorePrivilege", SE_RESTORE_NAME, false).map(|_| ())
    }

    /// Writes only the owner onto `path`, separately from the DACL.
    ///
    /// Naming an owner the caller is not needs `SeRestorePrivilege` *enabled*; `SeTakeOwnershipPrivilege`
    /// on its own only lets a holder claim the object for a SID it may legitimately use, which for an
    /// elevated administrator means itself and never `SY`. Both are attempted by
    /// [`super::apply_ownership_privilege`] before anything is enforced, and the LocalSystem service
    /// holds both from the start. Group is deliberately left alone: bundling it would refuse the owner
    /// for any process that cannot claim `SY` as its primary group. `Ok(None)` means the write was
    /// accepted.
    fn apply_owner(
        path: &Path,
        parsed: &ParsedDescriptor,
    ) -> Result<Option<String>, DataDirectoryError> {
        let wide_path = to_wide(path);
        // An owner this process cannot read from the descriptor is reported the same way a refused
        // write is: the DACL lockdown above already happened and must not be downgraded by it.
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
                owner,
                PSID(std::ptr::null_mut()),
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

    /// Applies [`RESTRICTED_SDDL`] to `path`, then verifies that the stored descriptor is the
    /// expected one. Fails closed: a path that cannot be brought under SYSTEM/Administrators control
    /// is an error, never a warning.
    ///
    /// Order matters and is the whole point: the DACL is written unconditionally first, and only then
    /// is ownership attempted as a separate call. A refused owner repair cannot undo the lockdown,
    /// because leaving the inherited `BUILTIN\Users` grants in place would let a standard account
    /// pre-empt the directory and feed the LocalSystem service a `service.toml` it controls.
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
        // The DACL is what makes the location usable, so it is verified even when the owner could
        // not be restored; a failed lockdown outranks the ownership complaint.
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

    /// Re-reads the stored descriptor and refuses anything SPEC SET-02 does not allow.
    /// `require_trusted_owner` is false only when this process was refused the owner repair and the
    /// caller reports that separately as [`DataDirectoryError::Ownership`].
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

    /// Test-only: writes an arbitrary SDDL so a probe location can be made deletable again after it
    /// has been locked down. Never used by the service itself.
    #[cfg(test)]
    pub fn apply_sddl_for_test(path: &Path, sddl: &str) -> Result<(), DataDirectoryError> {
        let parsed = parse_descriptor(path, sddl)?;
        apply_dacl(path, &parsed).map(|_| ())
    }

    /// Test-only: asks the token, over a call path independent of
    /// [`enable_take_ownership_privilege`], whether `SeTakeOwnershipPrivilege` is actually in effect.
    /// `PrivilegeCheck` reports a privilege that is assigned but disabled as absent, which is exactly
    /// the distinction the ownership repair depends on.
    #[cfg(test)]
    pub fn take_ownership_privilege_is_effective() -> bool {
        use windows::Win32::Security::{PrivilegeCheck, PRIVILEGE_SET};

        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SAFETY: as in `enable_take_ownership_privilege`; `luid` is a writable out-parameter that
        // outlives the call.
        if unsafe { LookupPrivilegeValueW(PCWSTR::null(), SE_TAKE_OWNERSHIP_NAME, &mut luid) }
            .is_err()
        {
            return false;
        }

        let mut raw_token = HANDLE(std::ptr::null_mut());
        // SAFETY: as in `enable_take_ownership_privilege`, and TOKEN_QUERY alone asks for no
        // adjustment at all.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) }.is_err() {
            return false;
        }
        let token = OwnedToken(raw_token);

        let mut required = PRIVILEGE_SET {
            PrivilegeCount: 1,
            Control: 0,
            Privilege: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: Default::default(),
            }],
        };
        let mut result = BOOL::default();
        // SAFETY: `token.0` is live for the whole call because the guard owns it; `required` is both
        // read and written by the call and `result` is a writable out-parameter, and both outlive it.
        unsafe { PrivilegeCheck(token.0, &mut required, &mut result) }
            .map(|_| result.as_bool())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn restricted_descriptor_grants_nothing_to_users() {
        let dacl = &RESTRICTED_SDDL[RESTRICTED_SDDL.find("D:").expect("DACL present")..];
        assert!(
            dacl.starts_with("D:P"),
            "inheritance must be disabled: {dacl}"
        );
        assert!(dacl.contains("(A;;GA;;;SY)"), "{dacl}");
        assert!(dacl.contains("(A;;GA;;;BA)"), "{dacl}");
        // Interactive, built-in users, everyone and authenticated users must appear nowhere.
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

    /// Creating a location is the response to it being *missing*, and to nothing else. A path the
    /// caller is barred from is a security question for SPEC SET-02, not an invitation to `create_dir_all`,
    /// so the classification has to hold even for the states that never reach a locked-down tree: a
    /// plain file, and a name whose parent does not exist either.
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
        // Something at the path that is not a directory is still not absent: creating it would be the
        // wrong response, and the descriptor check is what has to refuse it.
        assert_eq!(location_presence(&file), LocationPresence::Present);
        // A location whose parent chain is missing is absent as well, not unreachable.
        assert_eq!(
            location_presence(&base.join("absent-parent").join("index")),
            LocationPresence::Absent
        );

        std::fs::remove_dir_all(&base).expect("probe base removable");
    }

    #[test]
    fn pipe_dacl_keeps_standard_users_able_to_connect() {
        // SPEC SVC-02: standard users may connect and query; only the admin-only switch removes that, and it
        // never grants write access to the data locations.
        assert!(DEFAULT_SDDL.contains("(A;;GRGW;;;IU)"), "{DEFAULT_SDDL}");
        assert!(!ADMIN_ONLY_SDDL.contains("IU"), "{ADMIN_ONLY_SDDL}");
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

    /// A temporary root that no earlier run can have left behind. A protected directory from a
    /// previous run is not listable by this account, so reusing a name would turn a fresh test into
    /// a create failure.
    #[cfg(windows)]
    fn fresh_probe_root(prefix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("cairn-{prefix}-{}-{stamp}", std::process::id()))
    }

    /// SPEC SET-02 effect measured on the real object store: a location created by the current account
    /// (which inherits writable ACEs from its parent) is locked down to SYSTEM/Administrators, and
    /// the outcome is read back from the stored descriptor rather than assumed.
    #[cfg(windows)]
    #[test]
    fn enforce_locks_a_real_directory_down_and_reports_ownership_needs() {
        let probe = fresh_probe_root("acl-probe");
        std::fs::create_dir_all(&probe).expect("probe directory");

        let result = enforce_restricted_acl(&probe);
        let trust = inspect_path_trust(&probe)
            .expect("inspection after repair")
            .expect("probe still present");

        // Whatever happened, give the creator its access back so the probe can be deleted.
        let cleanup = super::windows_acl::apply_sddl_for_test(
            &probe,
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)",
        );
        let removed = std::fs::remove_dir(&probe);

        // The DACL is the part any owner can fix with its implicit WRITE_DAC right.
        assert!(
            trust.acl_compliant,
            "DACL not locked down: {}",
            trust.summary()
        );
        match result {
            // LocalSystem / SeTakeOwnershipPrivilege: fully remediated.
            Ok(()) => assert!(trust.owner_trusted, "{}", trust.summary()),
            // Standard or non-privileged account: the DACL is fixed, ownership needs the service.
            Err(DataDirectoryError::Ownership { owner, .. }) => {
                assert!(!trust.owner_trusted);
                assert_ne!(owner, "S-1-5-18");
            }
            Err(other) => panic!("unexpected failure: {other}"),
        }
        cleanup.expect("cleanup descriptor");
        removed.expect("probe directory removable after cleanup");
    }

    /// The compliance check has to tell the SPEC SET-02 lockdown apart from a look-alike descriptor that
    /// grants less: the right principals with a narrower mask are not what this service writes, while
    /// a `GA` grant the object store has expanded to `FILE_ALL_ACCESS` is exactly what it is.
    #[cfg(windows)]
    #[test]
    fn a_partial_grant_is_not_mistaken_for_the_restricted_descriptor() {
        let probe = fresh_probe_root("mask-probe");
        std::fs::create_dir_all(&probe).expect("probe directory");

        super::windows_acl::apply_sddl_for_test(&probe, "D:P(A;;GR;;;SY)(A;;GR;;;BA)")
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

        super::windows_acl::apply_sddl_for_test(&probe, RESTRICTED_SDDL)
            .expect("write the restricted descriptor");
        let trust = inspect_path_trust(&probe)
            .expect("inspect")
            .expect("present");
        assert!(trust.acl_compliant, "{}", trust.summary());

        super::windows_acl::apply_sddl_for_test(&probe, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)")
            .expect("cleanup descriptor");
        std::fs::remove_dir(&probe).expect("probe removable after cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn prepare_data_layout_creates_every_service_location_or_refuses_to_continue() {
        let probe = fresh_probe_root("tree-probe");
        let outcome = prepare_data_layout(&probe);
        let layout = ServiceDataLayout::new(&probe);

        // All four locations exist and are locked even when the identity step could not be
        // completed: the tree is created first, every location is then locked down, and a partial
        // lock must never be silently accepted.
        //
        // Existence is proven by reading the stored descriptor rather than by `is_dir()`: listing
        // the entries of the protected parent needs `FILE_LIST_DIRECTORY`, which SPEC SET-02 grants only
        // to SYSTEM and Administrators, while this process keeps the implicit `READ_CONTROL` right
        // of an owner that `GetNamedSecurityInfoW` uses. A location that does not exist yields
        // `Ok(None)` here, so this loop fails on a missing directory too.
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
            Err(DataDirectoryError::Ownership { .. })
            | Err(DataDirectoryError::NotRemediated { .. }) => {}
            Err(other) => panic!("unexpected failure: {other}"),
        }

        let _ = super::windows_acl::apply_sddl_for_test(
            &probe,
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)",
        );
        for dir in [&layout.logs_dir, &layout.content_dir, &layout.index_dir] {
            let _ = super::windows_acl::apply_sddl_for_test(
                dir,
                "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)",
            );
            let _ = std::fs::remove_dir(dir);
        }
        let _ = std::fs::remove_dir(&probe);
    }

    /// The state a locked-down tree leaves a standard account in: every location exists, and none of
    /// them can be listed through the protected parent. Treating that as "absent, mine to create"
    /// makes the run end on `create_dir_all`'s `ERROR_ALREADY_EXISTS`, before any location is
    /// enforced, so the ownership or repair refusal the operator must act on is never produced.
    #[cfg(windows)]
    #[test]
    fn an_existing_but_unlistable_tree_is_enforced_rather_than_failed_to_create() {
        let probe = fresh_probe_root("unlistable-tree");
        let layout = ServiceDataLayout::new(&probe);
        // The whole tree exists first, exactly as `%ProgramData%\Cairn` does after a service run: the
        // defect is about an existing location being read as a missing one, so a location that really
        // is missing would take a different, correct path through the same loop.
        for dir in layout.writable_locations() {
            std::fs::create_dir_all(&dir).expect("probe tree");
        }

        // Lock the root down on its own: each child keeps the owner's implicit READ_CONTROL, which is
        // what lets the descriptor check reach it, while the query that would have to prove the child
        // exists is refused by the protected parent.
        super::windows_acl::apply_sddl_for_test(&probe, RESTRICTED_SDDL)
            .expect("lock the probe root down");

        let cleanup = |probe: &std::path::Path| {
            for dir in ServiceDataLayout::new(probe).writable_locations() {
                let _ = super::windows_acl::apply_sddl_for_test(
                    &dir,
                    "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;CO)",
                );
            }
            let _ = std::fs::remove_dir_all(probe);
        };

        if location_presence(&layout.index_dir) != LocationPresence::NotListable {
            // A token holding the built-in Administrators group has Generic All on the descriptor
            // SPEC SET-02 writes, so it can still list the protected parent and this machine cannot produce
            // the blocked traversal at all. Nothing here is an existing tree that has to be enforced,
            // so record that the case was not exercised rather than assert a different one.
            eprintln!("this token can still list the locked-down parent; unlistable-tree check not exercised");
            cleanup(&probe);
            return;
        }

        // A location that genuinely is not there still reads as absent behind this same protected
        // parent, so refusing to create an unreachable one cannot turn a first install into a failure.
        assert_eq!(
            location_presence(&probe.join("never-created")),
            LocationPresence::Absent,
            "a missing location stopped being recognised as creatable behind a locked-down parent"
        );

        let outcome = prepare_data_layout(&probe);

        // The failure must come from the access control, never from the creation attempt.
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

        // And enforcement really ran. A run that stopped at the create error would leave `index` with
        // the grants it inherited from an unprotected parent instead of the SPEC SET-02 descriptor, so this
        // fails on the misclassification alone, independently of the error category above.
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

    /// A reparse point in the data path would let the LocalSystem process write through a link that
    /// an unprivileged account created, so it must be refused outright.
    #[cfg(windows)]
    #[test]
    fn reparse_point_is_refused_instead_of_followed() {
        let base = std::env::temp_dir().join(format!("cairn-link-probe-{}", std::process::id()));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).expect("target directory");

        // `mklink /J` creates a directory junction, which is the form an unprivileged account can
        // create inside a parent it may write to.
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);

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
            // The machine refused to create the junction; nothing to assert, so record that fact.
            eprintln!("junction could not be created; reparse check not exercised");
        }
    }

    /// `AdjustTokenPrivileges` answers success even when the privilege is not part of the token at
    /// all, so a naive implementation would report the ownership repair as reachable from a process
    /// that still cannot perform it. The report therefore has to agree with what the token actually
    /// holds, read through an independent call.
    ///
    /// Whichever token the suite runs under, one of the two branches is exercised: a standard account
    /// has no `SeTakeOwnershipPrivilege` to enable, an elevated one has it assigned and disabled until
    /// this call enables it, and LocalSystem holds it enabled from the start.
    #[cfg(windows)]
    #[test]
    fn the_privilege_report_agrees_with_the_token_that_holds_it() {
        let adjusted = super::windows_acl::enable_take_ownership_privilege();
        let in_effect = super::windows_acl::take_ownership_privilege_is_effective();

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

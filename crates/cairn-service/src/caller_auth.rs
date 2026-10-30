// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Caller authentication and request authorization for pipe connections.
//!
//! The service runs as LocalSystem and indexes files the caller has no access to, so every request
//! must be attributed to a real OS identity rather than to a self-reported field (SPEC SVC-02,
//! SVC-05).
//!
//! Identity is derived exclusively from the access token of the connected pipe client:
//! `GetNamedPipeClientProcessId` -> `ImpersonateNamedPipeClient` -> `OpenThreadToken` ->
//! `GetTokenInformation(TokenUser)` -> `CreateWellKnownSid` + `CheckTokenMembership`.
//! A caller that cannot be attributed is `CallerAuthorization::Unauthenticated` and is served the
//! handshake only.

use cairn_protocol::message::{
    ClientRequest, ERROR_CODE_ADMIN_ONLY_QUERY, ERROR_CODE_UNAUTHORIZED,
};

/// The status of a pipe caller as derived from its access token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallerAuthorization {
    /// Token of the LocalSystem account.
    LocalSystem,
    /// Token with enabled built-in Administrators membership, i.e. an elevated process.
    Administrator,
    /// Authenticated caller that is neither of the above.
    StandardUser,
    /// Identity could not be derived; nothing but the handshake is allowed.
    Unauthenticated,
}

impl CallerAuthorization {
    pub fn is_privileged(&self) -> bool {
        matches!(
            self,
            CallerAuthorization::LocalSystem | CallerAuthorization::Administrator
        )
    }
}

/// Identity of the process connected to a pipe instance, read from its access token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    /// Process id reported by the pipe provider, never by the client itself (SVC-02).
    pub client_pid: u32,
    /// String form of the token user SID; for logs and diagnostics only.
    pub user_sid: String,
    pub authorization: CallerAuthorization,
    /// `false` when the client connected with an impersonation level that hides group membership.
    /// Such a caller can never be more than `StandardUser`.
    pub groups_readable: bool,
}

impl CallerIdentity {
    pub fn authorization(&self) -> CallerAuthorization {
        self.authorization
    }

    pub fn is_admin(&self) -> bool {
        self.authorization.is_privileged()
    }
}

/// Why a connection could not be attributed to a local caller.
#[derive(Debug, thiserror::Error)]
pub enum CallerAuthError {
    #[error("client process id unavailable, connection treated as remote or stale: {0}")]
    NoLocalClient(String),
    #[error("ImpersonateNamedPipeClient failed: {0}")]
    ImpersonationFailed(String),
    #[error("client access token could not be opened: {0}")]
    TokenUnavailable(String),
}

/// Which callers may issue a request. `Hello` and `Cancel` are always allowed: the first is
/// negotiation, the second only stops the caller's own in-flight query.
///
/// Index configuration is a local-user operation: the service owns and persists the protected
/// `service.toml`, so an authenticated interactive caller does not need elevation to change what
/// the service indexes. `admin_only_query` is the exception and is guarded where the new
/// configuration is applied, because a standard user must not be able to disable that mode.
fn access_class(req: &ClientRequest) -> AccessClass {
    match req {
        ClientRequest::Hello { .. } | ClientRequest::Cancel { .. } => AccessClass::Handshake,
        ClientRequest::Query { .. }
        | ClientRequest::GetRows { .. }
        | ClientRequest::GetStatus
        | ClientRequest::GetConfig
        | ClientRequest::Rescan { .. }
        | ClientRequest::SetConfig { .. }
        | ClientRequest::ReloadConfig
        | ClientRequest::GetContentIndexStatus
        | ClientRequest::CancelContentIndex
        | ClientRequest::ClearContentIndex
        | ClientRequest::GetTree { .. } => AccessClass::Read,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessClass {
    Handshake,
    Read,
}

/// Whether the caller may submit a configuration that changes the administrators-only query
/// mode.
///
/// `admin_only_query` decides who may query at all, so a standard user must not be able to switch
/// that mode on (locking everyone else out) or off (defeating it). Only an administrator may
/// change it (SPEC SVC-05).
///
/// The rule lives here so every decision about what a caller may do is answered by the same
/// module, and so it can be checked without standing up the whole engine.
pub fn may_change_admin_only_query(
    caller: CallerAuthorization,
    currently_enabled: bool,
    submitted_enabled: bool,
) -> bool {
    currently_enabled == submitted_enabled || caller.is_privileged()
}

/// Authorization decision for one request, evaluated per request so that configuration changes
/// (`admin_only_query`) take effect without waiting for a new pipe instance.
///
/// `Ok(())` allows the request; `Err` carries the protocol error code and a technical message.
pub fn authorize(
    req: &ClientRequest,
    caller: CallerAuthorization,
    admin_only_query: bool,
) -> Result<(), (u32, String)> {
    match access_class(req) {
        AccessClass::Handshake => Ok(()),
        AccessClass::Read => {
            if admin_only_query && !caller.is_privileged() {
                return Err((
                    ERROR_CODE_ADMIN_ONLY_QUERY,
                    format!(
                        "administrators-only query mode is enabled and the caller is {caller:?} (SPEC SVC-05)"
                    ),
                ));
            }
            if caller == CallerAuthorization::Unauthenticated {
                return Err((
                    ERROR_CODE_UNAUTHORIZED,
                    "caller identity could not be derived from the pipe client token".to_string(),
                ));
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{CallerAuthError, CallerAuthorization, CallerIdentity};
    use std::io;
    use windows::core::BOOL;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{
        CheckTokenMembership, CreateWellKnownSid, GetTokenInformation, RevertToSelf, TokenGroups,
        TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid, PSID, TOKEN_GROUPS,
        TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER, WELL_KNOWN_SID_TYPE,
    };
    use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, ImpersonateNamedPipeClient};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
    };

    /// Reverts thread impersonation when dropped, including on early returns.
    pub struct Impersonation;

    impl Impersonation {
        /// SAFETY: `pipe` must be a handle of a connected named pipe server instance that stays
        /// valid for the lifetime of the returned guard. `ImpersonateNamedPipeClient` applies to
        /// the calling OS thread only, so the caller must not yield the thread (no `.await`) while
        /// the guard is alive.
        unsafe fn begin(pipe: HANDLE) -> Result<Self, CallerAuthError> {
            match ImpersonateNamedPipeClient(pipe) {
                Ok(()) => Ok(Self),
                Err(_) => Err(CallerAuthError::ImpersonationFailed(last_os_error())),
            }
        }
    }

    impl Drop for Impersonation {
        fn drop(&mut self) {
            // SAFETY: paired with a successful ImpersonateNamedPipeClient on this same thread.
            // A failure is logged instead of ignored: the process is LocalSystem, so a leaked
            // impersonation would make later work on this thread run with client rights.
            if let Err(err) = unsafe { RevertToSelf() } {
                tracing::error!(
                    "RevertToSelf failed, this thread may still be impersonating a pipe client: {err}"
                );
            }
        }
    }

    fn last_os_error() -> String {
        // SAFETY: GetLastError reads the calling thread's last error value.
        let code = unsafe { GetLastError().0 };
        io::Error::from_raw_os_error(code as i32).to_string()
    }

    /// Renders a SID as its string form. The buffer returned by `ConvertSidToStringSidW` is
    /// released with `LocalFree` before returning.
    fn sid_to_string(sid: PSID) -> Option<String> {
        if sid.is_invalid() {
            return None;
        }
        let mut wide = PWSTR::null();
        // SAFETY: `sid` points at a valid SID inside a caller-owned buffer; the converted string is
        // copied out and the LocalAlloc'd buffer is freed immediately afterwards.
        unsafe {
            ConvertSidToStringSidW(sid, &mut wide).ok()?;
            let text = wide.to_string().ok();
            let _ = LocalFree(Some(HLOCAL(wide.0 as *mut core::ffi::c_void)));
            text
        }
    }

    /// Builds a well-known SID into a caller-owned buffer. The buffer must outlive every use of the
    /// `PSID` handed out by the caller.
    fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> Option<Vec<u8>> {
        let mut size = 0u32;
        // SAFETY: with a null target buffer CreateWellKnownSid only reports the required size.
        unsafe {
            CreateWellKnownSid(kind, None, None, &mut size).ok()?;
        }
        if size == 0 {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        // SAFETY: `buffer` is exactly the size reported by the sizing call above.
        let result = unsafe {
            CreateWellKnownSid(
                kind,
                None,
                Some(PSID(buffer.as_mut_ptr() as *mut core::ffi::c_void)),
                &mut size,
            )
        };
        if result.is_ok() {
            Some(buffer)
        } else {
            None
        }
    }

    /// Reads a variable-length token information class into a fresh buffer.
    fn token_information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u8>> {
        let mut needed = 0u32;
        // SAFETY: a null buffer with length 0 asks GetTokenInformation for the required size.
        let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: `buffer` has at least `needed` bytes as reported by the sizing call.
        unsafe {
            GetTokenInformation(
                token,
                class,
                Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
                needed,
                &mut needed,
            )
        }?;
        Ok(buffer)
    }

    /// Administrators membership of the current process token, read independently of any pipe
    /// impersonation. Used by tests to cross-check the impersonated result.
    pub fn current_process_is_admin() -> bool {
        // SAFETY: GetCurrentProcess is the current-process pseudo handle (never closed); the token
        // handle opened next to it is closed before returning.
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let result = well_known_sid(WinBuiltinAdministratorsSid)
                .and_then(|sid_buffer| {
                    let mut member = BOOL::default();
                    // SAFETY: `sid_buffer` holds a valid well-known SID for the duration of the
                    // call and CheckTokenMembership only reads it.
                    CheckTokenMembership(
                        Some(token),
                        PSID(sid_buffer.as_ptr() as *mut core::ffi::c_void),
                        &mut member,
                    )
                    .ok()
                    .map(|_| member.as_bool())
                })
                .unwrap_or(false);
            let _ = CloseHandle(token);
            result
        }
    }

    /// Derives the identity of the process connected to `pipe`.
    ///
    /// Thread model: the whole sequence is synchronous. Impersonation is thread-bound, there is no
    /// await point between `ImpersonateNamedPipeClient` and the `RevertToSelf` performed by
    /// `Impersonation::drop`, so the tokio worker thread cannot be swapped underneath it and the
    /// impersonation never outlives this call.
    pub fn authenticate_pipe_client(
        pipe: std::os::windows::io::RawHandle,
    ) -> Result<CallerIdentity, CallerAuthError> {
        let pipe_handle = HANDLE(pipe);

        let mut client_pid = 0u32;
        // SAFETY: `pipe_handle` is a connected pipe instance handle provided by the caller.
        if unsafe { GetNamedPipeClientProcessId(pipe_handle, &mut client_pid) }.is_err() {
            // Fails for remote and already-disconnected clients: no local identity means no grant.
            return Err(CallerAuthError::NoLocalClient(last_os_error()));
        }

        // SAFETY: see `Impersonation::begin`; the guard reverts before this function returns.
        let _impersonation = unsafe { Impersonation::begin(pipe_handle)? };

        // SAFETY: GetCurrentThread is the calling-thread pseudo handle (never closed).
        // `openasself` is TRUE so opening the thread token is authorized as the service account,
        // which holds the right; the handle returned still refers to the impersonated client token.
        let token = unsafe {
            let mut token = HANDLE::default();
            OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token)
                .map_err(|_| CallerAuthError::TokenUnavailable(last_os_error()))?;
            token
        };
        // SAFETY: `token` was opened above and stays valid until the matching CloseHandle below.
        let identity = unsafe { describe_token(token, client_pid) };
        // SAFETY: paired with the successful OpenThreadToken above.
        let _ = unsafe { CloseHandle(token) };
        identity
    }

    /// SAFETY: `token` must be an open handle to the access token of the impersonated client.
    unsafe fn describe_token(
        token: HANDLE,
        client_pid: u32,
    ) -> Result<CallerIdentity, CallerAuthError> {
        let user_buffer = token_information(token, TokenUser)
            .map_err(|err| CallerAuthError::TokenUnavailable(err.to_string()))?;
        // SAFETY: GetTokenInformation(TokenUser) filled the buffer with a TOKEN_USER header whose
        // `User.Sid` points into the very same buffer, which lives until the SID has been rendered.
        let user = &*(user_buffer.as_ptr() as *const TOKEN_USER);
        let user_sid =
            sid_to_string(user.User.Sid).unwrap_or_else(|| "<unrenderable-sid>".to_string());

        // Group membership requires SECURITY_IMPERSONATION on the client's CreateFile. At a lower
        // level the probes fail, and the caller stays a standard user.
        let mut groups_readable = false;
        let mut authorization = CallerAuthorization::StandardUser;

        for (kind, granted) in [
            (WinLocalSystemSid, CallerAuthorization::LocalSystem),
            (
                WinBuiltinAdministratorsSid,
                CallerAuthorization::Administrator,
            ),
        ] {
            let Some(sid_buffer) = well_known_sid(kind) else {
                continue;
            };
            let mut member = BOOL::default();
            // SAFETY: `sid_buffer` holds a valid well-known SID for the duration of the call.
            let probe = CheckTokenMembership(
                Some(token),
                PSID(sid_buffer.as_ptr() as *mut core::ffi::c_void),
                &mut member,
            );
            match probe {
                Ok(()) => {
                    groups_readable = true;
                    if member.as_bool() && authorization == CallerAuthorization::StandardUser {
                        authorization = granted;
                    }
                }
                Err(_) => {
                    tracing::debug!(
                        "group membership probe for pid {client_pid} failed: {}",
                        last_os_error()
                    );
                }
            }
        }

        // TokenGroups is only read to make the derived identity auditable; the access decision uses
        // CheckTokenMembership, which also honours disabled and deny-only group SIDs.
        if let Ok(groups_buffer) = token_information(token, TokenGroups) {
            // SAFETY: the buffer holds a TOKEN_GROUPS header of the length GetTokenInformation
            // reported; only the fixed-size header is read.
            let group_count = unsafe { (groups_buffer.as_ptr() as *const TOKEN_GROUPS).as_ref() }
                .map(|groups| groups.GroupCount)
                .unwrap_or(0);
            if group_count > 0 {
                groups_readable = true;
                tracing::debug!(
                    "pipe client pid {client_pid} {user_sid} presents {group_count} group SIDs"
                );
            }
        }

        if !groups_readable {
            tracing::warn!(
                "pipe client pid {client_pid} ({user_sid}) presents no group information; \
                 privileged requests are denied. Clients must connect with SECURITY_IMPERSONATION."
            );
        }

        Ok(CallerIdentity {
            client_pid,
            user_sid,
            authorization,
            groups_readable,
        })
    }
}

#[cfg(windows)]
pub use platform::{authenticate_pipe_client, current_process_is_admin};

#[cfg(not(windows))]
pub fn authenticate_pipe_client(
    _pipe: *mut core::ffi::c_void,
) -> Result<CallerIdentity, CallerAuthError> {
    Err(CallerAuthError::ImpersonationFailed(
        "named pipe impersonation is only available on Windows".to_string(),
    ))
}

#[cfg(not(windows))]
pub fn current_process_is_admin() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> ClientRequest {
        ClientRequest::Query {
            query_id: 1,
            raw_query: "rs".to_string(),
            defaults: Default::default(),
            sort_by_name: "name".to_string(),
            sort_asc: true,
            first_page_limit: 50,
        }
    }

    #[test]
    fn index_configuration_and_rescan_are_read_class_for_local_users() {
        for req in [
            ClientRequest::ReloadConfig,
            ClientRequest::ClearContentIndex,
            ClientRequest::Rescan { volume_id: None },
            ClientRequest::SetConfig {
                config_toml: String::new(),
            },
        ] {
            assert_eq!(access_class(&req), AccessClass::Read, "{req:?}");
        }
        assert_eq!(access_class(&query()), AccessClass::Read);
        assert_eq!(access_class(&ClientRequest::GetConfig), AccessClass::Read);
        assert_eq!(
            access_class(&ClientRequest::Hello {
                client_version: 1,
                pid: 1
            }),
            AccessClass::Handshake
        );
    }

    /// Only an administrator may change the administrators-only query mode, and only when the
    /// change actually turns it on or off. A standard user may submit a configuration that keeps
    /// the mode as it is.
    #[test]
    fn only_an_administrator_may_change_the_admin_only_query_mode() {
        assert!(
            !may_change_admin_only_query(CallerAuthorization::StandardUser, false, true),
            "a standard user must not switch administrators-only mode on"
        );
        assert!(
            !may_change_admin_only_query(CallerAuthorization::StandardUser, true, false),
            "a standard user must not switch administrators-only mode off"
        );
        assert!(
            may_change_admin_only_query(CallerAuthorization::StandardUser, false, false),
            "keeping the mode as it is is not a change to it"
        );
        assert!(
            may_change_admin_only_query(CallerAuthorization::StandardUser, true, true),
            "keeping the mode as it is not a change to it"
        );
        assert!(
            may_change_admin_only_query(CallerAuthorization::Administrator, false, true),
            "an administrator may switch the mode on"
        );
        assert!(
            may_change_admin_only_query(CallerAuthorization::Administrator, true, false),
            "an administrator may switch the mode off"
        );
    }

    #[test]
    fn a_standard_user_may_change_index_configuration_when_admin_mode_is_off() {
        for req in [
            ClientRequest::ReloadConfig,
            ClientRequest::ClearContentIndex,
            ClientRequest::Rescan { volume_id: Some(2) },
            ClientRequest::SetConfig {
                config_toml: "content_index_enabled = true".to_string(),
            },
        ] {
            assert!(
                authorize(&req, CallerAuthorization::StandardUser, false).is_ok(),
                "{req:?} is a local-user index operation, not an administrative one"
            );
        }
    }

    #[test]
    fn an_unauthenticated_caller_cannot_change_configuration() {
        let req = ClientRequest::SetConfig {
            config_toml: "content_index_enabled = true".to_string(),
        };
        let denied = authorize(&req, CallerAuthorization::Unauthenticated, false)
            .expect_err("an unattributed pipe client must not change configuration");
        assert_eq!(denied.0, ERROR_CODE_UNAUTHORIZED);

        assert!(authorize(&req, CallerAuthorization::Administrator, false).is_ok());
        assert!(authorize(&req, CallerAuthorization::LocalSystem, false).is_ok());
    }

    #[test]
    fn admin_only_query_mode_denies_configuration_changes_to_standard_users() {
        for req in [
            ClientRequest::ReloadConfig,
            ClientRequest::ClearContentIndex,
            ClientRequest::Rescan { volume_id: None },
            ClientRequest::SetConfig {
                config_toml: "content_index_enabled = true".to_string(),
            },
        ] {
            let denied = authorize(&req, CallerAuthorization::StandardUser, true)
                .expect_err("admin-only query mode keeps configuration administrative");
            assert_eq!(denied.0, ERROR_CODE_ADMIN_ONLY_QUERY, "{req:?}");
            assert!(authorize(&req, CallerAuthorization::Administrator, true).is_ok());
        }
    }

    #[test]
    fn unattributed_connection_is_denied() {
        assert_eq!(
            authorize(&query(), CallerAuthorization::Unauthenticated, false),
            Err((
                ERROR_CODE_UNAUTHORIZED,
                "caller identity could not be derived from the pipe client token".to_string()
            ))
        );
        // The handshake itself still works so the client learns the server is there (SVC-03/04).
        assert!(authorize(
            &ClientRequest::Hello {
                client_version: 1,
                pid: 7
            },
            CallerAuthorization::Unauthenticated,
            true
        )
        .is_ok());
    }

    #[test]
    fn admin_only_query_switch_gates_reads_for_standard_users_only() {
        assert!(authorize(&query(), CallerAuthorization::StandardUser, true).is_err());
        assert!(authorize(
            &ClientRequest::GetConfig,
            CallerAuthorization::StandardUser,
            true
        )
        .is_err());
        assert!(authorize(
            &ClientRequest::GetStatus,
            CallerAuthorization::StandardUser,
            true
        )
        .is_err());
        assert!(authorize(&query(), CallerAuthorization::StandardUser, false).is_ok());
        assert!(authorize(
            &ClientRequest::GetConfig,
            CallerAuthorization::Administrator,
            true
        )
        .is_ok());
        assert!(authorize(&query(), CallerAuthorization::LocalSystem, true).is_ok());
    }

    #[test]
    fn cancel_of_own_query_is_always_allowed() {
        for caller in [
            CallerAuthorization::StandardUser,
            CallerAuthorization::Unauthenticated,
        ] {
            assert!(
                authorize(&ClientRequest::Cancel { query_id: 3 }, caller, true).is_ok(),
                "{caller:?} must be able to cancel its own query"
            );
        }
    }

    #[test]
    fn only_system_and_administrators_are_privileged() {
        assert!(CallerAuthorization::LocalSystem.is_privileged());
        assert!(CallerAuthorization::Administrator.is_privileged());
        assert!(!CallerAuthorization::StandardUser.is_privileged());
        assert!(!CallerAuthorization::Unauthenticated.is_privileged());
    }
}

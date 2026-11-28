// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Caller authentication and request authorization for pipe connections.
//!
//! The service runs as `LocalSystem` and indexes files the caller has no access to, so every request
//! must be attributed to a real OS identity rather than to a self-reported field (SPEC SVC-02,
//! SVC-05).
//!
//! Identity is derived exclusively from the access token of the connected pipe client:
//! `GetNamedPipeClientProcessId` -> `ImpersonateNamedPipeClient` -> `OpenThreadToken` ->
//! `GetTokenInformation(TokenUser)` -> `CreateWellKnownSid` + `CheckTokenMembership`.
//! A caller whose identity cannot be derived is disconnected before any frame is served; on
//! non-Windows builds requests other than the handshake are refused.

use cairn_protocol::message::{
    ClientRequest, ERROR_CODE_ADMIN_ONLY_QUERY, ERROR_CODE_UNAUTHORIZED,
};

/// The status of a pipe caller as derived from its access token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallerAuthorization {
    /// Token of the `LocalSystem` account.
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
}

impl CallerIdentity {
    pub fn authorization(&self) -> CallerAuthorization {
        self.authorization
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

fn access_class(req: &ClientRequest) -> AccessClass {
    match req {
        ClientRequest::Hello { .. } | ClientRequest::Cancel { .. } => AccessClass::Handshake,
        ClientRequest::Query { .. }
        | ClientRequest::GetRows { .. }
        | ClientRequest::GetStatus
        | ClientRequest::GetConfig
        | ClientRequest::GetContentIndexStatus
        | ClientRequest::GetSources
        | ClientRequest::GetPropertyIndexStatus
        | ClientRequest::GetIndexJournal { .. } => AccessClass::Read,
        ClientRequest::Rescan { .. }
        | ClientRequest::SetConfig { .. }
        | ClientRequest::ReloadConfig
        | ClientRequest::CancelContentIndex
        | ClientRequest::ClearContentIndex
        | ClientRequest::AddFolderSource { .. }
        | ClientRequest::RemoveFolderSource { .. }
        | ClientRequest::RescanSource { .. }
        | ClientRequest::SetPropertyIndexEnabled { .. }
        | ClientRequest::SetIndexJournalEnabled { .. } => AccessClass::IndexMutation,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessClass {
    Handshake,
    Read,
    IndexMutation,
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
        AccessClass::Read | AccessClass::IndexMutation => {
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
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{
        CheckTokenMembership, CreateWellKnownSid, GetTokenInformation, PSID, RevertToSelf,
        TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER, TokenGroups, TokenUser,
        WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, ImpersonateNamedPipeClient};
    use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
    use windows::core::BOOL;
    use windows::core::PWSTR;

    pub struct Impersonation;

    impl Impersonation {
        /// # Safety
        ///
        /// `pipe` must be a handle of a connected named pipe server instance that stays
        /// valid for the lifetime of the returned guard. `ImpersonateNamedPipeClient` applies to
        /// the calling OS thread only, so the caller must not yield the thread (no `.await`) while
        /// the guard is alive.
        unsafe fn begin(pipe: HANDLE) -> Result<Self, CallerAuthError> {
            // SAFETY: caller guarantees `pipe` is a connected named pipe handle that remains valid
            // on the current OS thread for the lifetime of the returned `Impersonation` guard.
            match unsafe { ImpersonateNamedPipeClient(pipe) } {
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

    pub(super) fn sid_to_string(sid: PSID) -> Option<String> {
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

    pub(super) fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> Option<Vec<u8>> {
        let mut size = 0u32;
        // SAFETY: passing a null target buffer queries only the required byte length and intentionally returns ERROR_INSUFFICIENT_BUFFER.
        let _ = unsafe { CreateWellKnownSid(kind, None, None, &mut size) };
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
        result.is_ok().then_some(buffer)
    }

    pub(super) struct AlignedTokenBuffer {
        words: Vec<u64>,
        byte_len: usize,
    }

    impl AlignedTokenBuffer {
        fn new(capacity_bytes: usize) -> Self {
            let word_len = capacity_bytes.div_ceil(size_of::<u64>());
            Self {
                words: vec![0u64; word_len],
                byte_len: 0,
            }
        }

        #[cfg(test)]
        pub(super) fn from_words(words: Vec<u64>, byte_len: usize) -> Self {
            Self { words, byte_len }
        }

        #[cfg(test)]
        pub(super) fn as_ptr(&self) -> *const u64 {
            self.words.as_ptr()
        }

        fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
            self.words.as_mut_ptr().cast()
        }

        fn capacity_bytes(&self) -> usize {
            self.words.len().saturating_mul(size_of::<u64>())
        }

        pub(super) fn read_header<T>(&self) -> Option<&T> {
            const {
                assert!(align_of::<T>() <= align_of::<u64>());
            }
            if self.byte_len < size_of::<T>() || self.byte_len > self.capacity_bytes() {
                return None;
            }
            // SAFETY: `words` is `Vec<u64>` (8-byte aligned >= `align_of::<T>()`), zero-initialized,
            // and `self.byte_len >= size_of::<T>()` within `self.words` allocated byte capacity.
            Some(unsafe { &*self.words.as_ptr().cast::<T>() })
        }
    }

    fn token_information(
        token: HANDLE,
        class: TOKEN_INFORMATION_CLASS,
    ) -> io::Result<AlignedTokenBuffer> {
        let mut needed = 0u32;
        // SAFETY: a null buffer with length 0 asks GetTokenInformation for the required size.
        let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = AlignedTokenBuffer::new(needed as usize);
        let mut written = 0u32;
        // SAFETY: `buffer` is 8-byte aligned and has at least `needed` bytes capacity.
        unsafe {
            GetTokenInformation(
                token,
                class,
                Some(buffer.as_mut_ptr()),
                needed,
                &mut written,
            )
        }?;
        if (written as usize) > buffer.capacity_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GetTokenInformation returned length exceeding allocated buffer",
            ));
        }
        buffer.byte_len = written as usize;
        Ok(buffer)
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

    /// # Safety
    ///
    /// `token` must be an open handle to the access token of the impersonated client.
    unsafe fn describe_token(
        token: HANDLE,
        client_pid: u32,
    ) -> Result<CallerIdentity, CallerAuthError> {
        let user_buffer = token_information(token, TokenUser)
            .map_err(|err| CallerAuthError::TokenUnavailable(err.to_string()))?;
        let user = user_buffer.read_header::<TOKEN_USER>().ok_or_else(|| {
            CallerAuthError::TokenUnavailable(
                "TokenUser buffer shorter than TOKEN_USER header".to_string(),
            )
        })?;
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
            // SAFETY: `token` is an impersonated access token handle and `sid_buffer` holds a valid well-known SID.
            let probe = unsafe {
                CheckTokenMembership(
                    Some(token),
                    PSID(sid_buffer.as_ptr() as *mut core::ffi::c_void),
                    &mut member,
                )
            };
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

        if let Ok(groups_buffer) = token_information(token, TokenGroups)
            && let Some(groups) = groups_buffer.read_header::<TOKEN_GROUPS>()
        {
            let group_count = groups.GroupCount;
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
        })
    }
}

#[cfg(windows)]
pub use platform::authenticate_pipe_client;

#[cfg(not(windows))]
pub fn authenticate_pipe_client(
    _pipe: *mut core::ffi::c_void,
) -> Result<CallerIdentity, CallerAuthError> {
    Err(CallerAuthError::ImpersonationFailed(
        "named pipe impersonation is only available on Windows".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> ClientRequest {
        ClientRequest::Query {
            query_id: 1,
            raw_query: "rs".to_string(),
            defaults: Default::default(),
            sort_spec: cairn_core::sort::SortSpec::default(),
            first_page_limit: 50,
            source_scope: None,
        }
    }

    #[test]
    fn access_classes_separate_handshake_read_and_index_mutation() {
        for req in [
            ClientRequest::Hello {
                client_protocol_version: 1,
                pid: 1,
            },
            ClientRequest::Cancel { query_id: 1 },
        ] {
            assert_eq!(access_class(&req), AccessClass::Handshake, "{req:?}");
        }

        for req in [
            query(),
            ClientRequest::GetRows {
                session_id: 1,
                offset: 0,
                limit: 10,
            },
            ClientRequest::GetStatus,
            ClientRequest::GetConfig,
            ClientRequest::GetContentIndexStatus,
            ClientRequest::GetSources,
            ClientRequest::GetPropertyIndexStatus,
            ClientRequest::GetIndexJournal {
                after_seq: 0,
                limit: 100,
            },
        ] {
            assert_eq!(access_class(&req), AccessClass::Read, "{req:?}");
        }

        for req in [
            ClientRequest::Rescan { volume_id: None },
            ClientRequest::SetConfig {
                config_toml: String::new(),
            },
            ClientRequest::ReloadConfig,
            ClientRequest::CancelContentIndex,
            ClientRequest::ClearContentIndex,
            ClientRequest::AddFolderSource {
                path: "D:\\Projects".to_string(),
            },
            ClientRequest::RemoveFolderSource {
                path: "D:\\Projects".to_string(),
            },
            ClientRequest::RescanSource {
                source_id: cairn_core::property_store::SourceId(1000),
            },
            ClientRequest::SetPropertyIndexEnabled { enabled: true },
            ClientRequest::SetIndexJournalEnabled { enabled: true },
        ] {
            assert_eq!(access_class(&req), AccessClass::IndexMutation, "{req:?}");
        }
    }

    #[test]
    fn a_standard_user_may_change_index_configuration_when_admin_mode_is_off() {
        for req in [
            ClientRequest::ReloadConfig,
            ClientRequest::ClearContentIndex,
            ClientRequest::Rescan { volume_id: Some(2) },
            ClientRequest::AddFolderSource {
                path: "D:\\Projects".to_string(),
            },
            ClientRequest::RemoveFolderSource {
                path: "D:\\Projects".to_string(),
            },
            ClientRequest::RescanSource {
                source_id: cairn_core::property_store::SourceId(1000),
            },
            ClientRequest::SetPropertyIndexEnabled { enabled: true },
            ClientRequest::SetIndexJournalEnabled { enabled: true },
            ClientRequest::SetConfig {
                config_toml: "content_index_enabled = true".to_string(),
            },
        ] {
            assert!(
                authorize(&req, CallerAuthorization::StandardUser, false).is_ok(),
                "{req:?} is a local-user index operation when admin_only_query is off"
            );
            let denied = authorize(&req, CallerAuthorization::StandardUser, true)
                .expect_err("admin_only_query = true must deny standard users");
            assert_eq!(denied.0, ERROR_CODE_ADMIN_ONLY_QUERY, "{req:?}");
            let unauth = authorize(&req, CallerAuthorization::Unauthenticated, false)
                .expect_err("unauthenticated callers must always be rejected");
            assert_eq!(unauth.0, ERROR_CODE_UNAUTHORIZED, "{req:?}");
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
        assert!(
            authorize(
                &ClientRequest::Hello {
                    client_protocol_version: 1,
                    pid: 7
                },
                CallerAuthorization::Unauthenticated,
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn admin_only_query_switch_gates_reads_for_standard_users_only() {
        assert!(authorize(&query(), CallerAuthorization::StandardUser, true).is_err());
        assert!(
            authorize(
                &ClientRequest::GetConfig,
                CallerAuthorization::StandardUser,
                true
            )
            .is_err()
        );
        assert!(
            authorize(
                &ClientRequest::GetStatus,
                CallerAuthorization::StandardUser,
                true
            )
            .is_err()
        );
        assert!(authorize(&query(), CallerAuthorization::StandardUser, false).is_ok());
        assert!(
            authorize(
                &ClientRequest::GetConfig,
                CallerAuthorization::Administrator,
                true
            )
            .is_ok()
        );
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

    #[cfg(windows)]
    #[test]
    fn aligned_token_buffer_guarantees_8_byte_alignment_and_rejects_short_buffers() {
        use windows::Win32::Security::{TOKEN_GROUPS, TOKEN_USER};

        let aligned =
            platform::AlignedTokenBuffer::from_words(vec![0u64; 8], size_of::<TOKEN_USER>());
        assert_eq!(
            aligned.as_ptr() as usize % align_of::<u64>(),
            0,
            "token buffer must stay 8-byte aligned"
        );
        assert_eq!(aligned.as_ptr() as usize % align_of::<TOKEN_USER>(), 0);
        assert!(aligned.read_header::<TOKEN_USER>().is_some());

        let short_user = platform::AlignedTokenBuffer::from_words(
            vec![0u64; 4],
            size_of::<TOKEN_USER>().saturating_sub(1),
        );
        assert!(short_user.read_header::<TOKEN_USER>().is_none());

        let over_capacity =
            platform::AlignedTokenBuffer::from_words(vec![0u64; 1], size_of::<TOKEN_USER>() + 16);
        assert!(over_capacity.read_header::<TOKEN_USER>().is_none());

        let valid_groups =
            platform::AlignedTokenBuffer::from_words(vec![0u64; 8], size_of::<TOKEN_GROUPS>());
        assert!(valid_groups.read_header::<TOKEN_GROUPS>().is_some());

        let short_groups = platform::AlignedTokenBuffer::from_words(
            vec![0u64; 4],
            size_of::<TOKEN_GROUPS>().saturating_sub(1),
        );
        assert!(short_groups.read_header::<TOKEN_GROUPS>().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn well_known_sid_resolves_system_and_administrators() {
        use windows::Win32::Security::{PSID, WinBuiltinAdministratorsSid, WinLocalSystemSid};

        let mut system_buf =
            platform::well_known_sid(WinLocalSystemSid).expect("LocalSystem SID must resolve");
        let system_sid = PSID(system_buf.as_mut_ptr().cast());
        assert_eq!(
            platform::sid_to_string(system_sid).as_deref(),
            Some("S-1-5-18")
        );

        let mut admins_buf = platform::well_known_sid(WinBuiltinAdministratorsSid)
            .expect("Builtin Administrators SID must resolve");
        let admins_sid = PSID(admins_buf.as_mut_ptr().cast());
        assert_eq!(
            platform::sid_to_string(admins_sid).as_deref(),
            Some("S-1-5-32-544")
        );
    }
}

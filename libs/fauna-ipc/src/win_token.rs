//! Shared Win32 "resolve the current process token's user" primitive.
//!
//! `fauna-cfapi`'s shell-registration Id, [`crate::sync::current_user_pipe_name`],
//! and [`crate::pipe_transport::PipeSecurity::for_owner`] all need the same
//! `OpenProcessToken` -> `GetTokenInformation(TokenUser)` dance, and used to
//! hand-roll it three times — two of them backing the reinterpret-cast buffer
//! with a `Vec<u8>`, which Rust guarantees only 1-byte-aligned, an unsound cast
//! target for `TOKEN_USER` (whose `PSID` field needs pointer/8-byte alignment).
//! One of the three also leaked the process token handle. Found by a
//! near-duplicate-function sweep over the shared Rust libraries.
//!
//! [`pipe_server_user_sid_string`] runs the same dance against a **peer's**
//! token rather than our own — the machine-wide pipe namespace means "who is
//! serving this name?" is a security question, not a bookkeeping one.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PWSTR;

/// Read the calling process's token `TOKEN_USER` into an owned, 8-byte-aligned
/// buffer, closing the token handle before returning either way.
///
/// # Alignment
/// [`GetTokenInformation`] writes a `TOKEN_USER` header in place at the start
/// of the buffer, which a caller then reinterprets via a raw pointer cast
/// ([`token_user_sid`]) — sound only if the buffer's start address satisfies
/// `TOKEN_USER`'s alignment. `TOKEN_USER` is `SID_AND_ATTRIBUTES { Sid: PSID,
/// Attributes: u32 }` (public Win32 struct layout); `PSID` is a pointer type,
/// so `TOKEN_USER`'s alignment is that of a pointer (8 bytes on the
/// 64-bit/ARM64 targets this workspace ships). A `Vec<u8>` is only guaranteed
/// 1-byte aligned by Rust's type system (even though in practice Windows'
/// HeapAlloc backing the System allocator over-aligns every allocation) —
/// back the buffer with `u64` elements instead so the 8-byte alignment is
/// part of the type's actual contract, not an unstated assumption about
/// allocator internals.
pub fn current_user_token_info() -> Result<Vec<u64>> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .context("OpenProcessToken")?;
        let info = token_user_info(token);
        // Best-effort, and deliberately not `?`-ed before it: closing a process
        // token handle does not fail in practice, and the read's own error is
        // the one worth reporting.
        let _ = CloseHandle(token);
        info
    }
}

/// Read `token`'s `TOKEN_USER` into the same owned, 8-byte-aligned buffer
/// [`current_user_token_info`] returns — shared by it and by
/// [`pipe_server_user_sid_string`], which reads *another process's* token.
///
/// Does **not** close `token`; the caller owns it.
///
/// # Safety
/// `token` must be an open token handle carrying `TOKEN_QUERY`.
unsafe fn token_user_info(token: HANDLE) -> Result<Vec<u64>> {
    unsafe {
        // First call sizes the buffer; it "fails" with ERROR_INSUFFICIENT_BUFFER
        // by design.
        let mut needed: u32 = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        let word_len = (needed as usize).div_ceil(8);
        let mut token_info: Vec<u64> = vec![0u64; word_len];
        let buf_len = (word_len * 8) as u32;
        GetTokenInformation(
            token,
            TokenUser,
            Some(token_info.as_mut_ptr() as *mut _),
            buf_len,
            &mut needed,
        )
        .context("GetTokenInformation")?;
        Ok(token_info)
    }
}

/// The `TOKEN_USER.User.Sid` inside a buffer [`current_user_token_info`]
/// returned — a `PSID` pointing INTO `token_info`, valid only as long as that
/// buffer lives (Win32 convention: `PSID` borrows, never owns).
///
/// # Safety
/// `token_info` must be a buffer [`current_user_token_info`] returned (or an
/// equivalently `GetTokenInformation(TokenUser)`-filled, 8-byte-aligned one)
/// — this reinterprets its bytes as a `TOKEN_USER`.
pub unsafe fn token_user_sid(token_info: &[u64]) -> PSID {
    unsafe { (*(token_info.as_ptr() as *const TOKEN_USER)).User.Sid }
}

/// The current process's user SID as a string (e.g. `"S-1-5-21-..."`) —
/// [`current_user_token_info`] + [`token_user_sid`] + `ConvertSidToStringSidW`,
/// `LocalFree`d before returning.
pub fn current_user_sid_string() -> Result<String> {
    let token_info = current_user_token_info()?;
    unsafe { sid_to_string(token_user_sid(&token_info)) }
}

/// `ConvertSidToStringSidW` + `LocalFree`, shared by every SID-stringifier here.
///
/// # Safety
/// `sid` must point at a valid SID that outlives the call.
unsafe fn sid_to_string(sid: PSID) -> Result<String> {
    unsafe {
        let mut sid_str = PWSTR::null();
        ConvertSidToStringSidW(sid, &mut sid_str).context("ConvertSidToStringSidW")?;
        let out = sid_str.to_string().context("SID to UTF-8");
        let _ = LocalFree(Some(HLOCAL(sid_str.as_ptr() as *mut _)));
        out
    }
}

/// The user SID of the process **serving** the named pipe `pipe` is connected to
/// — the answer to *"whose agent did I just reach?"*.
///
/// `\\.\pipe\` is a **machine-wide** namespace and a SID is not a secret on a
/// shared box, so opening `\\.\pipe\fauna-sync.<SID>` proves only that *someone*
/// created that name. Any local account can create it while the victim's agent
/// is down — and then the victim's own `FILE_FLAG_FIRST_PIPE_INSTANCE` create
/// fails, so the real agent exits and stays out of the way. This is the probe
/// that distinguishes the two.
///
/// # Why the server's *token*, and not the pipe object's owner
///
/// `GetSecurityInfo(OWNER_SECURITY_INFORMATION)` on the handle looks like the
/// cheaper answer and is the wrong one, twice over: it needs `READ_CONTROL`,
/// which the **squatter** chose when it built the pipe's DACL, and a process
/// running on an elevated token has a default object owner of
/// `BUILTIN\Administrators` rather than the user — so it false-negatives on a
/// legitimate server. `GetNamedPipeServerProcessId` → that process's token user
/// is answered by the kernel about the peer, not by anything the peer authored.
///
/// # Failure is refusal, never a fallback
///
/// Every arm here returns `Err`, including the one a cross-user squatter
/// produces: `OpenProcess` against another user's process is itself
/// `ACCESS_DENIED`. That is the *correct* outcome and must stay one — a probe
/// that fell back to a weaker check on failure would hand the attacker the
/// choice of which check runs. Callers refuse on `Err` (see
/// [`SyncPipeClient::connect_pipe_to`](crate::sync_pipe_client::SyncPipeClient::connect_pipe_to)).
pub fn pipe_server_user_sid_string(pipe: HANDLE) -> Result<String> {
    use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let mut server_pid: u32 = 0;
        GetNamedPipeServerProcessId(pipe, &mut server_pid)
            .context("GetNamedPipeServerProcessId")?;

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, server_pid)
            .with_context(|| format!("OpenProcess on pipe server pid {server_pid}"))?;

        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token)
            .with_context(|| format!("OpenProcessToken on pipe server pid {server_pid}"));
        let info = opened.and_then(|()| token_user_info(token));
        if !token.is_invalid() {
            let _ = CloseHandle(token);
        }
        let _ = CloseHandle(process);

        let info = info?;
        sid_to_string(token_user_sid(&info))
    }
}

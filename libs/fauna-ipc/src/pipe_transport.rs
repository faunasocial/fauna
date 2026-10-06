//! Named-pipe transport for the local IPC control plane on windows — the
//! windows sibling of [`unix_transport`](crate::unix_transport), and the server
//! half whose client already lives here
//! ([`sync_pipe_client::connect_pipe_to`](crate::sync_pipe_client)).
//!
//! Same length-prefixed canonical dag-cbor frame as everywhere else
//! (`[u32 LE length][canonical dag-cbor payload]`, [`crate::frame_io`]).
//!
//! One binary serves a pipe with this module: the per-user sync agent
//! (`bins/fauna-sync-agent`, `\\.\pipe\fauna-sync.<user-SID>`). The FaunaBridge
//! service used to serve a second one (`\\.\pipe\fauna-bridge`) for a diagnostic
//! tool no installer shipped; both were removed, so nothing on windows serves a
//! machine-wide pipe any more.
//!
//! # The DACL is owner-only
//!
//! [`PipeSecurity::for_owner`] grants the serving process's own token user and
//! nobody else. Under the per-user agent model the agent runs as the interactive
//! user, so its own token SID *is* that user; `BUILTIN\Users` and `INTERACTIVE`
//! were the multi-user leak removed in Phase 2 of the per-user migration
//! (`apps/windows.md` § IPC: "DACL'd to that user's SID only"). There is
//! deliberately no parameter to widen it: a future server that needs another
//! trustee adds that grant here, with its reason, under review.

use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{broadcast, watch};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::core::PWSTR;

/// `GENERIC_READ | GENERIC_WRITE` — the access the pipe's owner is granted.
const PIPE_GENERIC_RW: u32 = 0xC000_0000;

/// Owns all memory backing the security descriptor and DACL handed to
/// `CreateNamedPipeW`.
///
/// `Pin<Box<Self>>` because `sa.lpSecurityDescriptor` points at `sd`, a sibling
/// field: the value must not move after those pointers are fixed up.
pub struct PipeSecurity {
    _token_info: Vec<u64>,
    acl: *mut ACL,
    sd: SECURITY_DESCRIPTOR,
    sa: SECURITY_ATTRIBUTES,
}

// SAFETY: the raw pointers inside point only into memory this value owns
// (`_token_info`, the `LocalAlloc`ed `acl`, and the pinned `sd`), and the value
// is immutable once built — `as_ptr` hands out a `*const` the pipe-create call
// only reads.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// Build `SECURITY_ATTRIBUTES` granting `GENERIC_READ | GENERIC_WRITE` to
    /// the current process's token user and nobody else (module docs).
    pub fn for_owner() -> Result<Pin<Box<Self>>> {
        unsafe {
            // Current process's token user — the shared, alignment-safe,
            // handle-closing primitive (`crate::win_token`). `token_info` must outlive `user_sid`, a `PSID`
            // borrowing into it, for the life of this `PipeSecurity`.
            let token_info = crate::win_token::current_user_token_info()?;
            let user_sid = crate::win_token::token_user_sid(&token_info);

            let entries = [explicit_access(
                PWSTR(user_sid.0 as *mut u16),
                TRUSTEE_IS_USER,
            )];

            let mut acl = std::ptr::null_mut::<ACL>();
            let err = SetEntriesInAclW(Some(&entries), None, &mut acl);
            if err.0 != 0 {
                anyhow::bail!("SetEntriesInAclW failed: error {}", err.0);
            }

            // Build security descriptor.
            let mut sd = SECURITY_DESCRIPTOR::default();
            InitializeSecurityDescriptor(PSECURITY_DESCRIPTOR(&mut sd as *mut _ as *mut _), 1)
                .context("InitializeSecurityDescriptor")?;

            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(&mut sd as *mut _ as *mut _),
                true,
                Some(acl),
                false,
            )
            .context("SetSecurityDescriptorDacl")?;

            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: &mut sd as *mut _ as *mut _,
                bInheritHandle: false.into(),
            };

            let mut pinned = Box::pin(Self {
                _token_info: token_info,
                acl,
                sd,
                sa,
            });

            // Fix up internal pointers after pin.
            let sd_ptr = &mut pinned.as_mut().get_unchecked_mut().sd as *mut _ as *mut _;
            pinned.as_mut().get_unchecked_mut().sa.lpSecurityDescriptor = sd_ptr;

            let acl_ptr = pinned.acl;
            SetSecurityDescriptorDacl(PSECURITY_DESCRIPTOR(sd_ptr), true, Some(acl_ptr), false)
                .context("SetSecurityDescriptorDacl (repin)")?;

            Ok(pinned)
        }
    }

    /// The `SECURITY_ATTRIBUTES` to hand `CreateNamedPipeW`.
    pub fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.sa as *const _
    }
}

fn explicit_access(sid: PWSTR, trustee_type: TRUSTEE_TYPE) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: PIPE_GENERIC_RW,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: trustee_type,
            ptstrName: sid,
            ..Default::default()
        },
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.acl.is_null() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.acl as *mut _)));
            }
        }
    }
}

/// Serve `pipe_name` until `shutdown` flips to `true`.
///
/// The direct analog of [`unix_transport::serve`](crate::unix_transport::serve),
/// generic over the message types so the frame loop never names a protocol
/// (today's one caller speaks [`crate::sync`]).
///
/// Each accepted connection reads length-prefixed request frames, passes each to
/// `handler`, and writes the response frame back; server-pushed events from
/// `events` are forwarded to every connected client. `handler` is the seam that
/// keeps this crate free of either binary's state type — the caller passes a
/// closure that calls its own `handle_request(&req, &state)`.
///
/// The first `CreateNamedPipeW` carries `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a
/// second server on the same name fails the create rather than silently sharing
/// it. That failure is returned, not swallowed: it is how a duplicate agent in a
/// second logon session terminates instead of degrading (`sync-agent.md` § The
/// agent is single-instance per user).
pub async fn serve<Req, Resp, Ev, H, F>(
    pipe_name: &str,
    handler: H,
    mut shutdown: watch::Receiver<bool>,
    events: broadcast::Sender<Ev>,
) -> Result<()>
where
    Req: DeserializeOwned + Send + 'static,
    // `Send` because the spawned per-connection `frame_io::handle_conn` future
    // holds owned `Resp`/`Ev` values across await points (never a reference —
    // both are encoded to bytes before any await, so `Sync` isn't needed).
    Resp: Serialize + crate::RefuseUndecodedRequest + Send + 'static,
    Ev: Serialize + Clone + Send + 'static,
    H: Fn(Req) -> F + Clone + Send + 'static,
    F: Future<Output = Resp> + Send + 'static,
{
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Pipes::*;

    tracing::info!("named pipe server starting on {}", pipe_name);

    let security = PipeSecurity::for_owner()?;
    tracing::info!("pipe security: DACL restricted to this process's token user");

    let pipe_name_wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut first = true;

    loop {
        let claiming_the_name = first;
        let mut flags = FILE_FLAG_OVERLAPPED.0 | PIPE_ACCESS_DUPLEX.0;
        if first {
            flags |= FILE_FLAG_FIRST_PIPE_INSTANCE.0;
            first = false;
        }

        let server = {
            let handle = unsafe {
                CreateNamedPipeW(
                    windows::core::PCWSTR(pipe_name_wide.as_ptr()),
                    FILE_FLAGS_AND_ATTRIBUTES(flags),
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE,
                    PIPE_UNLIMITED_INSTANCES,
                    65536,
                    65536,
                    0,
                    Some(security.as_ptr()),
                )
            };
            if handle.is_invalid() {
                let os = std::io::Error::last_os_error();
                // This error is the only witness this process leaves before it
                // exits, and the bare OS text ("Access is denied") names neither
                // the pipe nor the reason — it reads like a permissions
                // misconfiguration, which is the wrong diagnosis twice over.
                // A refused FIRST_PIPE_INSTANCE create means the NAME IS TAKEN,
                // and because `\\.\pipe\` is a machine-wide namespace the holder
                // need not be a second copy of this agent: any local account can
                // create `\\.\pipe\fauna-sync.<our SID>` and thereby both keep
                // this agent down and answer the app in its place. The client
                // half refuses such a server (`sync_pipe_client`'s
                // `verify_pipe_server_is`), but that refusal is silent over
                // here — so this line carries the hypothesis.
                let hint = if claiming_the_name
                    && os.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32)
                {
                    " — the name is already taken, either by another instance of \
                     this agent or by another local account holding it"
                } else {
                    ""
                };
                return Err(anyhow::anyhow!(
                    "CreateNamedPipeW failed for {pipe_name}: {os:?}{hint}"
                ));
            }
            unsafe {
                #[allow(unused_imports)]
                use std::os::windows::io::FromRawHandle;
                tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(
                    handle.0 as std::os::windows::io::RawHandle,
                )
            }
            .map_err(|e| anyhow::anyhow!("NamedPipeServer::from_raw_handle: {e}"))?
        };

        tokio::select! {
            result = server.connect() => {
                result?;
                tokio::spawn(crate::frame_io::handle_conn::<_, Req, Resp, Ev, H, F>(
                    server,
                    handler.clone(),
                    events.subscribe(),
                ));
            }
            changed = shutdown.changed() => {
                // Sender dropped (Err) or flipped to true → stop accepting.
                if changed.is_err() || *shutdown.borrow() {
                    tracing::info!("pipe server shutting down");
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod dacl_tests {
    use super::*;
    use crate::test_support::{ace_count, ace_sid, dacl_of};

    /// tier_1: the pipe DACL is owner-only — exactly ONE ACE, and it is this
    /// process's own token user, no `BUILTIN\Users` and no `INTERACTIVE`.
    ///
    /// RED before the Phase-2 collapse: `AceCount` was 3.
    #[test]
    fn owner_only_dacl_has_exactly_one_ace_for_the_token_user() {
        let sec = PipeSecurity::for_owner().expect("for_owner");
        let dacl = dacl_of(&sec);
        assert_eq!(
            unsafe { ace_count(dacl) },
            1,
            "the pipe DACL must grant the owner SID and nothing else — \
             BUILTIN\\Users and INTERACTIVE are the multi-user leak"
        );
        let token_info = crate::win_token::current_user_token_info().expect("token info");
        // SAFETY: `token_info` is `current_user_token_info`'s own buffer.
        let user_sid = unsafe { crate::win_token::token_user_sid(&token_info) };
        assert!(
            unsafe { EqualSid(ace_sid(dacl, 0), user_sid) }.is_ok(),
            "ACE 0 must be this process's token user"
        );
    }
}

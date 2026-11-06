//! The foreign-IMAP-source seam every app shares: one [`ImportSourceNest`]
//! over a live `fauna_mail::imap_client::ImapSession`, generic over how the
//! platform reaches the source (`mailbox-migration.md` § Where the IMAP client
//! runs).
//!
//! Everything the wizard and the drive loop ask of a source — sign in, list,
//! estimate, examine, the fetch and enumeration windows, re-opening a session a
//! fault dropped, logging out — is written once here. What differs per
//! platform is one act, a [`SourceOpener`]: native dials TCP and terminates TLS
//! with `tokio-rustls` (`NativeImapConnector`); web opens the nest's blind
//! WebSocket byte tunnel and terminates TLS inside the client
//! (`fauna_mail::imap_client::tunnel`). Each target binds its own opener and
//! clock in [`PlatformSourceSeam`], so the one `#[async_trait]` impl below
//! sees concrete futures and their `Send`-ness is inferred per target (the
//! reason `rpc_glue`'s module note gives for never writing `impl<R> …Nest`).

use futures_util::lock::{Mutex as AsyncMutex, MutexGuard};
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use fauna_mail::imap_client::{
    self, ImapClientError, ImapClock, ImapSession, ImapTransport, TlsMode,
};

use crate::error::NestError;
use crate::import::{
    FetchWindow, ImportSourceNest, ImportTlsMode, MailboxCursor, SourceAuth, SourceConnectError,
    SourceConnectFailure, SourceConnectParams, SourceMailboxView,
};

/// How a platform reaches a source server: the transport up to the IMAP
/// greeting — TCP (or the tunnel) and, in implicit mode, TLS. A failure is
/// classed for the wizard's sentence (§ Wizard steps step 2): unreachable is
/// [`SourceConnectFailure::Reach`], a failed handshake or an unusable trust
/// anchor [`SourceConnectFailure::Secure`].
#[allow(async_fn_in_trait)] // concrete per target; see the module docs.
pub(crate) trait SourceOpener {
    type Transport: ImapTransport;

    async fn open(
        &self,
        host: &str,
        port: u16,
        mode: TlsMode,
    ) -> Result<Self::Transport, SourceConnectError>;
}

/// § Failure handling's TCP/TLS-fault classification, shared by every
/// [`ImportSourceNest`] method: [`ImapClientError::Transport`]/`Eof` are the
/// retryable transport faults `run_import`'s backoff loop targets; every other
/// variant is the source telling us something true (bad creds, a rejected
/// command, a rebuilt mailbox) and is never retried.
fn imap_error(e: ImapClientError) -> NestError {
    match e {
        ImapClientError::Transport(_) | ImapClientError::Eof => NestError::Transient(e.to_string()),
        _ => NestError::Rejected(e.to_string()),
    }
}

fn source_not_connected() -> NestError {
    NestError::Rejected("not connected to the source server".into())
}

type Session<O> = ImapSession<<O as SourceOpener>::Transport>;

/// One source connection. § Throttling's budget lives inside `ImapSession`
/// itself (per source *server*, not per mailbox), so this seam holds exactly
/// one session.
pub(crate) struct SourceSeam<O: SourceOpener, K> {
    opener: O,
    clock: K,
    session: AsyncMutex<Option<Session<O>>>,
    /// What `connect` signed in with, kept until `logout` (§ Credential
    /// handling: the source password lives in client memory until LOGOUT). A
    /// transport fault drops `session`; the next call opens it again from
    /// these, so the drive loop's retries — and a Resume after the loop paused
    /// on a lost connection (§ Failure handling) — reach a live server rather
    /// than the dead socket.
    params: AsyncMutex<Option<SourceConnectParams>>,
    /// The source refused the kept credential on a re-open
    /// ([`ImportSourceNest::take_sign_in_refused`]).
    sign_in_refused: AtomicBool,
}

impl<O: SourceOpener, K: ImapClock> SourceSeam<O, K> {
    pub(crate) fn new(opener: O, clock: K) -> Self {
        Self {
            opener,
            clock,
            session: AsyncMutex::new(None),
            params: AsyncMutex::new(None),
            sign_in_refused: AtomicBool::new(false),
        }
    }

    /// Open, secure and sign in one source session. Each phase's failure is
    /// classed by where it happened: before the greeting it is the opener's
    /// own; during a `STARTTLS` upgrade it is "could not secure"; a refused
    /// `LOGIN` or `AUTHENTICATE XOAUTH2` is "could not sign in", a dropped one
    /// "could not reach".
    async fn open_session(
        &self,
        params: &SourceConnectParams,
    ) -> Result<Session<O>, SourceConnectError> {
        let mode = match params.tls_mode {
            ImportTlsMode::Implicit => TlsMode::Implicit,
            ImportTlsMode::StartTls => TlsMode::StartTls,
        };
        let transport = self.opener.open(&params.host, params.port, mode).await?;
        let mut session = match mode {
            TlsMode::Implicit => ImapSession::connect(transport).await,
            TlsMode::StartTls => ImapSession::connect_starttls(transport).await,
        }
        .map_err(|e| {
            let failure = match mode {
                TlsMode::StartTls => SourceConnectFailure::Secure,
                TlsMode::Implicit => SourceConnectFailure::Reach,
            };
            SourceConnectError::new(failure, e.to_string())
        })?;
        let signed_in = match &params.auth {
            SourceAuth::Password(password) => {
                session.login(&params.username, password.as_str()).await
            }
            SourceAuth::MicrosoftToken { user, access_token } => {
                session
                    .authenticate_xoauth2(user, access_token.as_str())
                    .await
            }
        };
        signed_in.map_err(|e| {
            let failure = match e {
                ImapClientError::Transport(_) | ImapClientError::Eof => SourceConnectFailure::Reach,
                _ => SourceConnectFailure::SignIn,
            };
            SourceConnectError::new(failure, e.to_string())
        })?;
        Ok(session)
    }

    /// The live session, re-opened from the kept parameters when a fault
    /// dropped it — `true` alongside when it was, since a fresh session has no
    /// mailbox `EXAMINE`d. A failure to reach or secure the source is
    /// transient — the loop retries and then pauses. A refused sign-in is not:
    /// the source has stopped taking the credential (§ Failure handling →
    /// *Source-side errors*, the `LOGIN` row), so it is
    /// [`NestError::Rejected`] with the refusal flagged, and the loop ends the
    /// session asking for new credentials.
    async fn live_session(&self) -> Result<(MutexGuard<'_, Option<Session<O>>>, bool), NestError> {
        let mut guard = self.session.lock().await;
        if guard.is_some() {
            return Ok((guard, false));
        }
        let params = self
            .params
            .lock()
            .await
            .clone()
            .ok_or_else(source_not_connected)?;
        let session = self.open_session(&params).await.map_err(|e| {
            if e.failure == SourceConnectFailure::SignIn {
                self.sign_in_refused.store(true, Ordering::SeqCst);
                NestError::Rejected(e.to_string())
            } else {
                NestError::Transient(e.to_string())
            }
        })?;
        *guard = Some(session);
        Ok((guard, true))
    }

    async fn connect(&self, params: SourceConnectParams) -> Result<(), SourceConnectError> {
        let session = self.open_session(&params).await?;
        *self.session.lock().await = Some(session);
        *self.params.lock().await = Some(params);
        Ok(())
    }

    async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(source_not_connected)?;
        let boxes = session.list_mailboxes().await.map_err(imap_error)?;
        Ok(boxes
            .into_iter()
            .map(|m| SourceMailboxView {
                name: m.name,
                selectable: m.selectable,
                special_use: m.special_use,
            })
            .collect())
    }

    async fn estimate_mailbox(
        &self,
        mailbox: &str,
        since_epoch: Option<i64>,
    ) -> Result<imap_client::MailboxEstimate, NestError> {
        let (mut guard, _) = self.live_session().await?;
        let session = guard.as_mut().ok_or_else(source_not_connected)?;
        let estimate = session
            .estimate_mailbox(mailbox, since_epoch)
            .await
            .map_err(imap_error);
        drop_if_broken(&mut guard, estimate)
    }

    async fn examine(&self, mailbox: &str) -> Result<imap_client::MailboxStatus, NestError> {
        let (mut guard, _) = self.live_session().await?;
        let session = guard.as_mut().ok_or_else(source_not_connected)?;
        // No expected UIDVALIDITY: the loop compares the one reported against
        // its cursor itself (§ Resume protocol step 4).
        let status = session.examine(mailbox, None).await.map_err(imap_error);
        drop_if_broken(&mut guard, status)
    }

    /// `mailbox` is read only when the session was just re-opened:
    /// `ImapSession::enumerate_uids`/`fetch_messages` act on whichever mailbox
    /// this session currently has `EXAMINE`d, and `run_import_inner`'s calling
    /// contract always issues an `examine(mailbox, cursor)` immediately before
    /// every `fetch_window(mailbox, cursor, _)` for that same mailbox — so a
    /// session that lived through it is already on the right one, and only one
    /// a fault made us re-open needs it `EXAMINE`d again. `require_selected`
    /// inside the session is the backstop if a future caller ever breaks that
    /// ordering.
    async fn fetch_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
        max_size_bytes: u64,
    ) -> Result<FetchWindow, NestError> {
        let (mut guard, reopened) = self.live_session().await?;
        let session = guard.as_mut().ok_or_else(source_not_connected)?;
        let window = self
            .fetch_window_on(session, reopened, mailbox, cursor, max, max_size_bytes)
            .await;
        drop_if_broken(&mut guard, window)
    }

    async fn fetch_window_on(
        &self,
        session: &mut Session<O>,
        reopened: bool,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
        max_size_bytes: u64,
    ) -> Result<FetchWindow, NestError> {
        // § Per-message flow step 1: the size is compared on the enumeration's
        // `RFC822.SIZE`, so an over-limit body is never transferred at all.
        let (too_large, wanted): (Vec<_>, Vec<_>) =
            enumerate_window_on(session, reopened, mailbox, cursor, max)
                .await?
                .into_iter()
                .partition(|e| e.is_oversize(max_size_bytes));
        let uids: Vec<u32> = wanted.into_iter().map(|e| e.uid).collect();
        let mut outcomes = Vec::with_capacity(uids.len());
        session
            .fetch_messages(&uids, &self.clock, |o| outcomes.push(o))
            .await
            .map_err(imap_error)?;
        Ok(FetchWindow {
            outcomes,
            too_large,
        })
    }

    async fn enumerate_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
    ) -> Result<Vec<imap_client::UidEntry>, NestError> {
        let (mut guard, reopened) = self.live_session().await?;
        let session = guard.as_mut().ok_or_else(source_not_connected)?;
        let entries = enumerate_window_on(session, reopened, mailbox, cursor, max).await;
        drop_if_broken(&mut guard, entries)
    }

    async fn logout(&self) -> Result<(), NestError> {
        *self.params.lock().await = None;
        let session = self.session.lock().await.take();
        if let Some(mut session) = session {
            session.logout().await.map_err(imap_error)?;
        }
        Ok(())
    }

    async fn replace_auth(&self, auth: SourceAuth) {
        if let Some(params) = self.params.lock().await.as_mut() {
            params.auth = auth;
        }
    }

    fn take_sign_in_refused(&self) -> bool {
        self.sign_in_refused.swap(false, Ordering::SeqCst)
    }
}

/// Drop a session a transport fault broke, so the next call re-opens it.
fn drop_if_broken<S, T>(
    guard: &mut Option<S>,
    result: Result<T, NestError>,
) -> Result<T, NestError> {
    if matches!(result, Err(NestError::Transient(_))) {
        *guard = None;
    }
    result
}

/// At most `max` of `mailbox`'s messages after `cursor`, by enumeration alone
/// — the first half of [`ImportSourceNest::fetch_window`] and the whole of
/// [`ImportSourceNest::enumerate_window`]. A session just re-opened is
/// `EXAMINE`d first (`fetch_window`'s comment on `reopened`).
async fn enumerate_window_on<T: ImapTransport>(
    session: &mut ImapSession<T>,
    reopened: bool,
    mailbox: &str,
    cursor: Option<MailboxCursor>,
    max: usize,
) -> Result<Vec<imap_client::UidEntry>, NestError> {
    if reopened {
        session
            .examine(mailbox, cursor.map(|c| c.source_uid_validity))
            .await
            .map_err(imap_error)?;
    }
    // `u32::MAX` is a legal IMAP UID: a cursor sitting on it has nothing after
    // it, so the mailbox is exhausted and the window is empty (never an
    // overflow panic that a source server could cause).
    let Some(from_uid) = cursor.map_or(Some(1), |c| c.last_processed_source_uid.checked_add(1))
    else {
        return Ok(Vec::new());
    };
    // `enumerate_uids` has no cap of its own (§ its own docs), so a large
    // mailbox is re-enumerated from `from_uid` on every window — correct (each
    // call returns the right slice) but not the cheapest possible re-fetch
    // pattern; a future pass could cache the enumeration per mailbox inside
    // this seam if it becomes a measured cost.
    Ok(session
        .enumerate_uids(from_uid)
        .await
        .map_err(imap_error)?
        .into_iter()
        .take(max)
        .collect())
}

/// This target's seam: its opener and its clock.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type PlatformSourceSeam = SourceSeam<native::NativeOpener, imap_client::TokioClock>;
#[cfg(target_arch = "wasm32")]
pub(crate) type PlatformSourceSeam = SourceSeam<web::TunnelOpener, web::WebClock>;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ImportSourceNest for PlatformSourceSeam {
    async fn connect(&self, params: SourceConnectParams) -> Result<(), SourceConnectError> {
        SourceSeam::connect(self, params).await
    }

    async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError> {
        SourceSeam::list_source_mailboxes(self).await
    }

    async fn estimate_mailbox(
        &self,
        mailbox: &str,
        since_epoch: Option<i64>,
    ) -> Result<imap_client::MailboxEstimate, NestError> {
        SourceSeam::estimate_mailbox(self, mailbox, since_epoch).await
    }

    async fn examine(&self, mailbox: &str) -> Result<imap_client::MailboxStatus, NestError> {
        SourceSeam::examine(self, mailbox).await
    }

    async fn fetch_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
        max_size_bytes: u64,
    ) -> Result<FetchWindow, NestError> {
        SourceSeam::fetch_window(self, mailbox, cursor, max, max_size_bytes).await
    }

    async fn enumerate_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
    ) -> Result<Vec<imap_client::UidEntry>, NestError> {
        SourceSeam::enumerate_window(self, mailbox, cursor, max).await
    }

    async fn logout(&self) -> Result<(), NestError> {
        SourceSeam::logout(self).await
    }

    async fn replace_auth(&self, auth: SourceAuth) {
        SourceSeam::replace_auth(self, auth).await
    }

    fn take_sign_in_refused(&self) -> bool {
        SourceSeam::take_sign_in_refused(self)
    }
}

/// Native: TCP + `tokio-rustls`, both TLS modes (`NativeImapConnector`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native {
    use super::*;
    use fauna_mail::imap_client::{NativeImapConnector, NativeTlsTransport, NativeTransportError};

    pub(crate) struct NativeOpener;

    /// The connector's pre-IMAP transport faults, classed for the wizard's
    /// sentence: a bad hostname or a dead socket is "could not reach", a
    /// failed handshake or an unusable trust anchor is "could not secure".
    fn failure(e: NativeTransportError) -> SourceConnectError {
        let failure = match e {
            NativeTransportError::InvalidHostname(_)
            | NativeTransportError::Connect { .. }
            | NativeTransportError::Io(_) => SourceConnectFailure::Reach,
            NativeTransportError::Tls { .. } | NativeTransportError::TrustAnchor(_) => {
                SourceConnectFailure::Secure
            }
        };
        SourceConnectError::new(failure, e.to_string())
    }

    impl SourceOpener for NativeOpener {
        type Transport = NativeTlsTransport;

        async fn open(
            &self,
            host: &str,
            port: u16,
            mode: TlsMode,
        ) -> Result<NativeTlsTransport, SourceConnectError> {
            NativeImapConnector::new(mode)
                .connect(host, port)
                .await
                .map_err(failure)
        }
    }

    /// The native seam, as every native app builds it.
    pub(crate) fn source_seam() -> PlatformSourceSeam {
        SourceSeam::new(NativeOpener, imap_client::TokioClock::new())
    }
}

/// Web: the nest's blind WebSocket byte tunnel, TLS inside the client
/// (`fauna_mail::imap_client::tunnel`; `mailbox-migration.md` § *The web
/// tunnel*).
#[cfg(target_arch = "wasm32")]
pub(crate) mod web {
    use std::sync::Arc;

    use super::*;
    use fauna_mail::imap_client::{
        TunnelDialer, TunnelError, TunnelPipe, TunnelTlsTransport, tunnel_client_config,
    };
    use fauna_rpc_wasm::{MailTunnelSocket, WsRpcClient};

    /// One opened tunnel WebSocket, as the shared transport reads it.
    pub(crate) struct WebPipe(MailTunnelSocket);

    impl TunnelPipe for WebPipe {
        async fn send(&mut self, frame: Vec<u8>) -> Result<(), TunnelError> {
            self.0.send(frame).await.map_err(TunnelError::Pipe)
        }

        async fn recv(&mut self) -> Result<Option<Vec<u8>>, TunnelError> {
            self.0.recv().await.map_err(TunnelError::Pipe)
        }
    }

    /// Opens a tunnel on the signed-in actor's own nest, with that actor's
    /// bearer — the connection the rest of the page already uses.
    #[derive(Clone)]
    pub(crate) struct WebDialer {
        nest: WsRpcClient,
    }

    impl WebDialer {
        pub(crate) fn new(nest: WsRpcClient) -> Self {
            Self { nest }
        }
    }

    impl TunnelDialer for WebDialer {
        type Pipe = WebPipe;

        async fn dial(&self) -> Result<WebPipe, TunnelError> {
            let bearer = self
                .nest
                .bearer(false)
                .await
                .map_err(|e| TunnelError::Pipe(e.to_string()))?;
            MailTunnelSocket::open(&self.nest.nest_url(), &bearer)
                .map(WebPipe)
                .map_err(TunnelError::Pipe)
        }
    }

    pub(crate) struct TunnelOpener {
        dialer: WebDialer,
        config: Arc<fauna_mail::imap_client::tunnel::ClientConfig>,
    }

    impl SourceOpener for TunnelOpener {
        type Transport = TunnelTlsTransport<WebPipe>;

        async fn open(
            &self,
            host: &str,
            port: u16,
            mode: TlsMode,
        ) -> Result<Self::Transport, SourceConnectError> {
            TunnelTlsTransport::connect(&self.dialer, Arc::clone(&self.config), host, port, mode)
                .await
                .map_err(failure)
        }
    }

    /// The tunnel's faults, classed as native's are: a refusal, a tunnel that
    /// would not open or closed is "could not reach"; a failed handshake is
    /// "could not secure".
    fn failure(e: TunnelError) -> SourceConnectError {
        let failure = match e {
            TunnelError::Tls(_) => SourceConnectFailure::Secure,
            TunnelError::Refused(_)
            | TunnelError::Pipe(_)
            | TunnelError::ClosedInHandshake
            | TunnelError::Protocol(_) => SourceConnectFailure::Reach,
        };
        SourceConnectError::new(failure, e.to_string())
    }

    /// § Throttling's clock on the browser: milliseconds from the wall clock
    /// (only differences matter) and `fauna_sleep`'s timer.
    pub(crate) struct WebClock;

    impl ImapClock for WebClock {
        fn now_ms(&self) -> u64 {
            fauna_core::data::Timestamp::now_millis_or_zero()
        }

        async fn sleep_ms(&self, ms: u64) {
            fauna_sleep::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }

    /// The web seam over the page's connection. `extra_root_pem` is the e2e
    /// source-IMAP trust seed, read by `fauna-wasm` under `test-helpers` and
    /// `None` in every shipped build.
    pub(crate) fn source_seam(
        nest: WsRpcClient,
        extra_root_pem: Option<&str>,
    ) -> Result<PlatformSourceSeam, String> {
        let config = tunnel_client_config(extra_root_pem).map_err(|e| e.to_string())?;
        Ok(SourceSeam::new(
            TunnelOpener {
                dialer: WebDialer::new(nest),
                config,
            },
            WebClock,
        ))
    }

    /// The Microsoft sign-in's `OAuthHttp` over the same kind of tunnel.
    pub(crate) fn oauth_http(
        nest: WsRpcClient,
        extra_root_pem: Option<&str>,
    ) -> Result<fauna_mail::imap_client::TunnelOAuthHttp<WebDialer>, String> {
        let config = tunnel_client_config(extra_root_pem).map_err(|e| e.to_string())?;
        Ok(fauna_mail::imap_client::TunnelOAuthHttp::new(
            WebDialer::new(nest),
            config,
        ))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// The source server's side, scripted: each read returns the next chunk.
    struct Scripted(VecDeque<&'static [u8]>);

    impl ImapTransport for Scripted {
        type Error = std::convert::Infallible;

        async fn write_all(&mut self, _bytes: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let Some(chunk) = self.0.pop_front() else {
                return Ok(0);
            };
            buf[..chunk.len()].copy_from_slice(chunk);
            Ok(chunk.len())
        }
    }

    /// The real seam's arm, not the drive loop's fake: a cursor on
    /// `u32::MAX` (a legal UID a source may answer) is an exhausted mailbox —
    /// an empty window, never an overflow. The session is never `EXAMINE`d,
    /// so reaching `UID FETCH` at all would fail the call: `Ok(empty)` proves
    /// the walk stopped before enumerating.
    #[tokio::test]
    async fn a_cursor_on_u32_max_is_an_empty_window_on_the_real_seam() {
        let mut session = ImapSession::connect(Scripted(VecDeque::from([
            &b"* OK ready\r\n"[..],
            &b"* CAPABILITY IMAP4rev1\r\nA0001 OK done\r\n"[..],
        ])))
        .await
        .expect("scripted greeting");
        let cursor = MailboxCursor {
            last_processed_source_uid: u32::MAX,
            source_uid_validity: 7,
        };
        let window = enumerate_window_on(&mut session, false, "INBOX", Some(cursor), 10)
            .await
            .expect("an exhausted mailbox is no error");
        assert!(window.is_empty());
    }
}

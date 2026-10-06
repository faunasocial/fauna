//! UniFFI façade for the **source-side IMAP import client** (mailbox-migration.md
//! § Client-driven streaming model) — the seam that makes `fauna_mail::imap_client`
//! reachable from apple / android / windows.
//!
//! UniFFI cannot export a generic, and `ImapSession<T>` is generic over the
//! [`fauna_mail::imap_client::ImapTransport`] seam by design (native TCP+TLS here;
//! a sans-io TLS tunnel in the browser). So this exports a **concrete** object
//! bound to the native transport — `ImapSession<NativeTlsTransport>` — while the
//! protocol behaviour it drives stays single-sourced in `fauna-mail` and is shared
//! with web and with the Rust-native apps (linux / tui), which call
//! `fauna_mail::imap_client` directly and never come through here (priority #2).
//!
//! # What this seam adds on top of the session
//!
//! It assembles the **complete `import_message` wire item**, not just the fetched
//! bytes: [`FfiImportMessage`] carries the `dedup_key` / `envelope_key` pair and
//! the `sender_domain` that § Per-message flow steps 2–3 require, derived here via
//! `fauna_mail::mail_dedup_keys_from_slice` and `fauna_mail::envelope::sender_domain`.
//! That placement is load-bearing — the dedup key's entire value is that every
//! producer (Go MDA at APPEND, Go MTA at the perimeter, the nest's in-domain
//! delivery, and this client) agrees
//! **byte-for-byte**, so a client shell must never compute it. A Kotlin or Swift
//! reimplementation would silently break dedup for every import; here it cannot.
//!
//! # Credentials
//!
//! § Credential handling: "Nest never receives the source credentials … a hard
//! rule, not a default." The source password / OAuth bearer passed to
//! [`FfiMailImportClient::login`] / [`FfiMailImportClient::authenticate_xoauth2`]
//! lives in this process's memory for the life of the session and crosses exactly
//! one connection — the TLS one, to the source server. Nothing here writes it
//! anywhere, and `ImapClientError` never echoes a command's arguments.

use std::sync::Arc;

use fauna_mail::dedup_key::mail_dedup_keys_from_slice;
use fauna_mail::envelope::sender_domain;
use fauna_mail::imap_client::{
    FetchOutcome, ImapSession, NativeImapConnector, NativeTlsTransport, TlsMode, TokioClock,
};
use tokio::sync::Mutex;

use crate::FfiError;

/// § Wizard steps: "TLS mode (implicit / STARTTLS)". Mirrors
/// [`fauna_mail::imap_client::TlsMode`] across the FFI boundary.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FfiImapTlsMode {
    /// TLS from the first byte — the 993 default, and what all three provider
    /// presets (Gmail / iCloud / Outlook) use.
    Implicit,
    /// Plaintext connect, then RFC 3501 `STARTTLS` before authenticating — the
    /// 143 default for a generic IMAP source.
    StartTls,
}

impl From<FfiImapTlsMode> for TlsMode {
    fn from(m: FfiImapTlsMode) -> Self {
        match m {
            FfiImapTlsMode::Implicit => TlsMode::Implicit,
            FfiImapTlsMode::StartTls => TlsMode::StartTls,
        }
    }
}

/// One mailbox on the source server (§ Wizard steps 3, the scope picker).
#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiSourceMailbox {
    pub name: String,
    /// `\Noselect` mailboxes are pure hierarchy nodes — the scope picker must
    /// not offer them.
    pub selectable: bool,
}

/// The `EXAMINE` result: the UID space we are about to enumerate.
#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiMailboxStatus {
    /// Half the resume cursor. A change across a resume means the source mailbox
    /// was rebuilt (§ Resume protocol).
    pub uid_validity: u32,
    pub exists: u32,
}

/// One enumerated UID, with its size when the source reports one — the input to
/// the § Scope picker's "max message size" skip.
#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiUidEntry {
    pub uid: u32,
    pub size: Option<u32>,
}

/// A fetched message, **complete as an `import_message` item**.
///
/// `body` is the raw RFC 5322 source, forwarded to nest unmodified in *both*
/// storage modes — the nest seals it and derives the search-index hint at ingest
/// (§ Per-message flow step 4). This client never encrypts and never rewrites the
/// body.
#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiImportMessage {
    pub mailbox: String,
    pub uid: u32,
    pub uid_validity: u32,
    pub flags: Vec<String>,
    pub internal_date_epoch: i64,
    pub body: Vec<u8>,
    /// Derived here, never by the caller — see the module docs.
    pub dedup_key: String,
    /// The canonical-envelope key, derived beside `dedup_key` by the same
    /// shared function; the nest skips a `dedup_key` hit only when it agrees
    /// (`mailbox-migration.md` § The envelope key confirms a Message-ID hit).
    pub envelope_key: String,
    /// The `from_norm` derivation nest stores alongside the sealed body.
    pub sender_domain: String,
}

/// § Failure handling: a per-message source failure is data, not an exception —
/// one unreadable message must not strand the other 49,999.
#[derive(uniffi::Enum, Clone, Debug)]
pub enum FfiFetchOutcome {
    Fetched { message: FfiImportMessage },
    Failed { uid: u32, reason: String },
}

impl From<FetchOutcome> for FfiFetchOutcome {
    fn from(o: FetchOutcome) -> Self {
        match o {
            FetchOutcome::Fetched(m) => {
                // The two derived wire fields (§ Per-message flow steps 2–3).
                // Computed from the same bytes we are about to ship, so the key
                // provably describes the body nest receives.
                let keys = mail_dedup_keys_from_slice(&m.body);
                let sender_domain = sender_domain(&m.body);
                Self::Fetched {
                    message: FfiImportMessage {
                        mailbox: m.mailbox,
                        uid: m.uid,
                        uid_validity: m.uid_validity,
                        flags: m.flags,
                        internal_date_epoch: m.internal_date_epoch,
                        body: m.body,
                        dedup_key: keys.dedup_key,
                        envelope_key: keys.envelope_key,
                        sender_domain,
                    },
                }
            }
            FetchOutcome::Failed { uid, reason } => Self::Failed { uid, reason },
        }
    }
}

/// A live source-IMAP session, one per source *server* (which is what makes it
/// the right owner of § Throttling's 4-concurrent / 100-per-minute budget — the
/// cap is per server, not per mailbox).
///
/// Lifecycle: [`Self::new`] → [`Self::connect`] → authenticate → `examine` /
/// `enumerate_uids` / `fetch_messages` → [`Self::logout`].
#[derive(uniffi::Object)]
pub struct FfiMailImportClient {
    connector: NativeImapConnector,
    host: String,
    port: u16,
    tls_mode: TlsMode,
    /// `None` until `connect`. An async mutex because it is held across `await`
    /// (the whole point is that a FETCH is in flight).
    session: Mutex<Option<ImapSession<NativeTlsTransport>>>,
    clock: TokioClock,
}

/// Uniform "you must `connect()` first" rejection.
fn not_connected() -> FfiError {
    FfiError::from("not connected to the source server".to_string())
}

/// Source-side errors cross the boundary as text. `ImapClientError` is written to
/// never echo a command's arguments, so a failed `LOGIN` cannot carry the
/// password into a client log (§ Credential handling).
fn ffi_err<E: std::fmt::Display>(e: E) -> FfiError {
    FfiError::from(e.to_string())
}

#[fauna_uniffi_async::export]
impl FfiMailImportClient {
    /// Describe a source server. Opens nothing — [`Self::connect`] does that, so
    /// the wizard's "test connection" step is an explicit, retryable action.
    ///
    /// `extra_root_pem` additionally trusts a PEM certificate, for the
    /// self-hosted case the *Generic IMAP* option exists to serve (a Dovecot
    /// behind a private CA). `None` for every public provider. There is no
    /// accept-any-certificate option: the user's password crosses this
    /// connection.
    #[uniffi::constructor]
    pub fn new(
        host: String,
        port: u16,
        tls_mode: FfiImapTlsMode,
        extra_root_pem: Option<String>,
    ) -> Result<Arc<Self>, FfiError> {
        let mut connector = NativeImapConnector::new(tls_mode.into());
        if let Some(pem) = extra_root_pem.as_deref() {
            connector = connector.with_extra_root_pem(pem).map_err(ffi_err)?;
        }
        Ok(Arc::new(Self {
            connector,
            host,
            port,
            tls_mode: tls_mode.into(),
            session: Mutex::new(None),
            clock: TokioClock::new(),
        }))
    }

    /// TCP-connect, establish TLS, read the greeting and the capabilities.
    ///
    /// § Wizard steps 2: a failure here (host unreachable, bad certificate) is
    /// surfaced verbatim, and the user retries without re-entering credentials —
    /// so this is safe to call again on the same object.
    pub async fn connect(&self) -> Result<(), FfiError> {
        let transport = self
            .connector
            .connect(&self.host, self.port)
            .await
            .map_err(ffi_err)?;

        // The one asymmetry between the two TLS modes, and it must not be got
        // wrong: a STARTTLS pipe is still plaintext here, and only
        // `connect_starttls` upgrades it before anything authenticates.
        let session = match self.tls_mode {
            TlsMode::Implicit => ImapSession::connect(transport).await,
            TlsMode::StartTls => ImapSession::connect_starttls(transport).await,
        }
        .map_err(ffi_err)?;

        *self.session.lock().await = Some(session);
        Ok(())
    }

    /// `LOGIN` — the app-password path (Gmail / iCloud) and generic IMAP.
    pub async fn login(&self, username: String, password: String) -> Result<(), FfiError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(not_connected)?;
        session.login(&username, &password).await.map_err(ffi_err)?;
        Ok(())
    }

    /// `AUTHENTICATE XOAUTH2` — the OAuth path (Outlook / Office365). The bearer
    /// stays in this process; nest never sees it.
    pub async fn authenticate_xoauth2(
        &self,
        username: String,
        access_token: String,
    ) -> Result<(), FfiError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(not_connected)?;
        session
            .authenticate_xoauth2(&username, &access_token)
            .await
            .map_err(ffi_err)?;
        Ok(())
    }

    /// `LIST "" "*"` — the scope picker's mailbox list.
    pub async fn list_mailboxes(&self) -> Result<Vec<FfiSourceMailbox>, FfiError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(not_connected)?;
        let boxes = session.list_mailboxes().await.map_err(ffi_err)?;
        Ok(boxes
            .into_iter()
            .map(|m| FfiSourceMailbox {
                name: m.name,
                selectable: m.selectable,
            })
            .collect())
    }

    /// `EXAMINE` — read-only select (§ Where the IMAP client runs: "the client
    /// `EXAMINE`s (never `SELECT`s)"), so importing never disturbs the source's
    /// `\Seen` / `\Recent` state.
    ///
    /// Pass the cursor's `uid_validity` as `expect_uid_validity` when resuming:
    /// a mismatch aborts rather than importing against a rebuilt UID space.
    pub async fn examine(
        &self,
        mailbox: String,
        expect_uid_validity: Option<u32>,
    ) -> Result<FfiMailboxStatus, FfiError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(not_connected)?;
        let status = session
            .examine(&mailbox, expect_uid_validity)
            .await
            .map_err(ffi_err)?;
        Ok(FfiMailboxStatus {
            uid_validity: status.uid_validity,
            exists: status.exists,
        })
    }

    /// `UID FETCH <from_uid>:* (UID RFC822.SIZE)` — resume-aware enumeration.
    /// Pass `1` for a fresh import, or `cursor + 1` to resume.
    pub async fn enumerate_uids(&self, from_uid: u32) -> Result<Vec<FfiUidEntry>, FfiError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(not_connected)?;
        let uids = session.enumerate_uids(from_uid).await.map_err(ffi_err)?;
        Ok(uids
            .into_iter()
            .map(|u| FfiUidEntry {
                uid: u.uid,
                size: u.size,
            })
            .collect())
    }

    /// Fetch `uids` with `BODY.PEEK[]`, under § Throttling's caps (4 concurrent,
    /// 100/minute per source server — enforced inside the shared session, so
    /// every app obeys them identically).
    ///
    /// Returns one outcome per UID, in completion order. A per-message failure is
    /// a [`FfiFetchOutcome::Failed`] entry, not an error: the import continues.
    pub async fn fetch_messages(&self, uids: Vec<u32>) -> Result<Vec<FfiFetchOutcome>, FfiError> {
        let mut out = Vec::with_capacity(uids.len());
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(not_connected)?;
        session
            .fetch_messages(&uids, &self.clock, |o| out.push(FfiFetchOutcome::from(o)))
            .await
            .map_err(ffi_err)?;
        Ok(out)
    }

    /// `LOGOUT` and drop the session — which is also what discards the source
    /// credentials (§ Credential handling: "until LOGOUT").
    pub async fn logout(&self) -> Result<(), FfiError> {
        fauna_mail::imap_client::logout_and_clear(&self.session)
            .await
            .map_err(ffi_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for the same cooperative-pool stack-overflow hazard as
    /// `fauna_anon_client::ws::connect_anonymous_future_stays_small_for_foreign_stacks`
    /// (`native-async-execution.md` § The hazard / § The rule). This future is
    /// what the export polled *inline* on the small foreign FFI stack (Swift
    /// cooperative pool / Kotlin dispatcher / .NET continuation thread) until
    /// `#[fauna_uniffi_async::export]` moved it onto a tokio worker; it still
    /// runs inline on every Rust shell's executor, so the size bound stays —
    /// `Box::pin`ing at a lower layer doesn't help if something upstream still
    /// holds a large state inline.
    ///
    /// Measured 17088 bytes before the fix (`connection.rs::next_event`'s
    /// `chunk` was a `[0u8; 16 * 1024]` stack array re-declared inside the read
    /// loop and held across its own `.await` — 16 KiB baked straight into this
    /// future's state, well past this doc's ~10.9 KB unsafe baseline). Fixed by
    /// heap-allocating `chunk` once outside the loop (`Vec<u8>`, not
    /// `Box::pin` — the future never held a *nested future* that needed
    /// boxing, it held a raw buffer). 1544 bytes after.
    ///
    /// Both TLS modes compile to the *same* future type (the `match` on
    /// `tls_mode` is runtime data, not a type-level branch), so one assertion
    /// covers both — confirmed identical (17088 / 1544 bytes) before checking.
    #[test]
    fn connect_future_stays_small_for_foreign_stacks() {
        let client = FfiMailImportClient::new(
            "mail.example.test".to_string(),
            993,
            FfiImapTlsMode::Implicit,
            None,
        )
        .unwrap();
        let fut = client.connect();
        let size = std::mem::size_of_val(&fut);
        assert!(
            size <= 2048,
            "FfiMailImportClient::connect future is {size} bytes — expected <= 2048. \
             This is awaited inline on the small foreign FFI poll stack \
             (native-async-execution.md § The hazard); if this grew, check for a \
             large stack-local buffer or an unboxed nested future held across an \
             `.await` somewhere in the connect/greeting/capabilities chain \
             (NativeImapConnector::connect, ImapSession::connect(_starttls), \
             Connection::next_event)."
        );
    }
}

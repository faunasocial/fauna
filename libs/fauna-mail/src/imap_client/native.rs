//! The **native** platform shell: tokio TCP + `tokio-rustls`.
//!
//! § Where the IMAP client runs pins this row of the seam table — "native:
//! `tokio::net::TcpStream` + `tokio-rustls`" — and it is the shell the four
//! native apps (apple / android / windows / linux, plus tui) reach,
//! whether directly (linux, cli: Rust-native) or through the UniFFI facade in
//! `fauna-ffi`.
//!
//! It is a **sibling of `outbound-net`, not part of `imap-client`**: the pure
//! protocol core stays wasm-clean, and tokio/rustls arrive only for consumers
//! that opt into `imap-client-native`. A wasm build enables `imap-client`
//! alone and never compiles this module. (The web shell — sans-io
//! `rustls::ClientConnection` over a blind byte relay — is the other impl of
//! the same seam; it lives in the web crate and is sequenced after the relay
//! host is deployed.)
//!
//! # What stays out of shared code
//!
//! Certificates, cipher suites, and hostnames. The seam carries post-TLS
//! plaintext IMAP; both TLS modes ([`TlsMode`]) resolve to the same pipe by the
//! time [`super::ImapSession`] speaks a word of IMAP — the only asymmetry is
//! *when* the handshake happens, and STARTTLS's command exchange is driven by
//! shared code ([`super::ImapSession::connect_starttls`]) precisely so the
//! protocol half is not reimplemented per platform.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::RootCertStore;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::transport::{ImapClock, ImapTransport, TlsMode};
use crate::imap_client::ImapClientError;

/// Everything the native pipe itself can fail at, before IMAP is even spoken.
///
/// Kept distinct from [`ImapClientError`] because § Wizard steps step 2 wants
/// these verbatim: "if the connection fails (wrong creds, TLS handshake fail,
/// host unreachable), the wizard surfaces the source server's error verbatim".
#[derive(Debug, thiserror::Error)]
pub enum NativeTransportError {
    #[error("could not reach {host}:{port}: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },

    #[error("{0:?} is not a valid source hostname")]
    InvalidHostname(String),

    #[error("TLS handshake with {host} failed: {source}")]
    Tls {
        host: String,
        #[source]
        source: std::io::Error,
    },

    #[error("source I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not parse the supplied trust anchor: {0}")]
    TrustAnchor(String),
}

/// The e2e source-trust seed's env var: a PEM whose CERTIFICATE block(s) are
/// added to the source-server trust anchors, on top of the WebPKI roots.
///
/// Read only by [`NativeImapConnector::e2e_extra_root`], and only in a
/// test-capable build — the constant itself is gated too, so a `strings` sweep
/// of a release artifact finds not even the name (convention 15's verification
/// method reads the built artifact, not the source).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub const E2E_EXTRA_CA_ENV: &str = "FAUNA_E2E_IMAP_EXTRA_CA_PEM";

/// Dials source IMAP servers. One connector serves many connections; build it
/// once per import session.
pub struct NativeImapConnector {
    mode: TlsMode,
    roots: RootCertStore,
}

impl NativeImapConnector {
    /// Trust the WebPKI root store — the public CAs, which is what every
    /// provider preset (Gmail / iCloud / Outlook) and any normally-certificated
    /// generic IMAP server chains to.
    ///
    /// In a **test-capable build only** this additionally honours
    /// [`E2E_EXTRA_CA_ENV`]; see [`Self::e2e_extra_root`] for why that arm lives
    /// here rather than at each consumer's construction site.
    pub fn new(mode: TlsMode) -> Self {
        // The store comes from `fauna-tls-bootstrap`, which this crate already
        // depends on for the CryptoProvider install: building it here duplicated
        // `fauna-ws-substrate`'s copy four lines at a time, and depending on a
        // full WS-transport stack to borrow them is the very thing the bootstrap
        // crate exists to avoid. The `e2e_extra_root` chain stays here — it
        // extends this connector's OWN store, which is exactly why the shared
        // function hands back an owned one.
        let roots = fauna_tls_bootstrap::webpki_root_store();
        Self { mode, roots }.e2e_extra_root()
    }

    /// The e2e source-trust seed: additionally trust the PEM in
    /// [`E2E_EXTRA_CA_ENV`], if that variable is set.
    ///
    /// **Why this exists.** § The two TLS modes ratifies that a source IMAP
    /// session has *no plaintext mode* — the user's password crosses it — so an
    /// e2e walk of the mail-import wizard has to point the app at a TLS server
    /// it can verify. A test rig cannot produce a publicly-chained certificate
    /// for `localhost`, and the app never gets to build its own connector: the
    /// production path is [`Self::new`], several layers below the harness. That
    /// is the same structural gap the R14 (account-data-plane.md § The ratified decisions) trust seed closes for the nest
    /// channel-binding pin, and this is the same answer —
    /// `e2e-automation-surface-gating.md` § The e2e trust seed.
    ///
    /// **Why on `new`, not per consumer.** This is the ONE door every native
    /// consumer builds its source trust store through — `rpc_glue`'s
    /// `RpcImportSourceNest` (linux, tui) and `fauna-ffi`'s
    /// `FfiMailImportClient` (apple, android, windows) — so the rule lives in
    /// one place and no consumer can be the one that forgot it (priority #2).
    ///
    /// **Why it is safe.** Three independent properties, and the gate is only
    /// the first:
    ///
    /// 1. **Compile-time exclusion is the boundary** (convention 15). A release
    ///    artifact has neither `debug_assertions`, nor `test`, nor
    ///    `test-helpers` — the arm does not exist to be reached. The runtime
    ///    env-var read is the inner switch *within* a test-capable build, never
    ///    the boundary.
    /// 2. **It ADDS a trust anchor; it never weakens verification.** It routes
    ///    through [`Self::with_extra_root_pem`], so there is no accept-any
    ///    escape hatch here any more than there is in production — an
    ///    unverifiable source server still fails the handshake. That asymmetry
    ///    is deliberate: `FAUNA_INSECURE_TLS`, the fleet's old accept-any path,
    ///    was retired precisely because "trust this specific CA" and "trust
    ///    anything" are not the same lever.
    /// 3. **A malformed value panics rather than degrading.** Silently ignoring
    ///    an unparseable PEM would hand the suite a handshake failure that reads
    ///    exactly like a wizard bug — the false-green shape convention 15's
    ///    witnesses exist to prevent. Only the harness ever sets this variable,
    ///    so a bad value means the harness is broken and should say so.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    fn e2e_extra_root(self) -> Self {
        let Ok(pem) = std::env::var(E2E_EXTRA_CA_ENV) else {
            return self;
        };
        if pem.trim().is_empty() {
            return self;
        }
        self.with_extra_root_pem(&pem).unwrap_or_else(|e| {
            panic!(
                "{E2E_EXTRA_CA_ENV} is set but is not a usable PEM trust anchor: {e}. \
                 Only the e2e harness sets this variable, so this is a harness bug — \
                 failing loudly beats a TLS handshake error that reads like a \
                 mail-import wizard bug."
            )
        })
    }

    /// The production twin: a shipped build has no source-trust seed, so this
    /// hands the connector straight back.
    ///
    /// Convention 15's "same-signature no-op twin wherever the caller is
    /// plumbing the app compiles unconditionally" — [`Self::new`] is exactly
    /// that plumbing. Two bodies rather than a `#[cfg]` on the call keeps the
    /// release path free of a `let` binding that exists only to be reassigned,
    /// and makes the absence explicit at the same place a reader finds the
    /// presence.
    #[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
    fn e2e_extra_root(self) -> Self {
        self
    }

    /// Additionally trust the PEM-encoded certificate(s) in `pem`.
    ///
    /// For the self-hosted case § Wizard steps' *Generic IMAP* option exists to
    /// serve: a Dovecot behind a private CA. It **adds** to the WebPKI roots
    /// rather than replacing them, and there is no "accept any certificate"
    /// escape hatch — an unverifiable source server fails the handshake, since
    /// the user's password is what crosses that connection.
    pub fn with_extra_root_pem(mut self, pem: &str) -> Result<Self, NativeTransportError> {
        let mut added = 0usize;
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let cert = cert.map_err(|e| NativeTransportError::TrustAnchor(e.to_string()))?;
            self.roots
                .add(cert)
                .map_err(|e| NativeTransportError::TrustAnchor(e.to_string()))?;
            added += 1;
        }
        if added == 0 {
            return Err(NativeTransportError::TrustAnchor(
                "no CERTIFICATE block in the supplied PEM".into(),
            ));
        }
        Ok(self)
    }

    /// Open a source connection.
    ///
    /// [`TlsMode::Implicit`] hands back a transport whose TLS is already up.
    /// [`TlsMode::StartTls`] hands back a *plaintext* pipe that
    /// [`super::ImapSession::connect_starttls`] must upgrade before
    /// authenticating — pass it there, never to
    /// [`super::ImapSession::connect`], or the password would ride the wire in
    /// the clear.
    pub async fn connect(
        &self,
        host: &str,
        port: u16,
    ) -> Result<NativeTlsTransport, NativeTransportError> {
        fauna_tls_bootstrap::ensure_tls_provider();

        let tcp = TcpStream::connect((host, port)).await.map_err(|source| {
            NativeTransportError::Connect {
                host: host.to_string(),
                port,
                source,
            }
        })?;
        // IMAP is a request/response protocol with small commands; Nagle would
        // stall each one waiting for a coalescing partner that never comes.
        let _ = tcp.set_nodelay(true);

        let config = rustls::ClientConfig::builder()
            .with_root_certificates(self.roots.clone())
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));

        let mut transport = NativeTlsTransport {
            pipe: Pipe::Plain(tcp),
            host: host.to_string(),
            connector,
        };

        if self.mode == TlsMode::Implicit {
            transport.handshake().await?;
        }
        Ok(transport)
    }
}

/// The pipe's TLS state. `Upgrading` exists only for the instant
/// [`NativeTlsTransport::handshake`] owns the stream by value.
enum Pipe {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    Upgrading,
}

/// A live source connection: [`ImapTransport`] over TCP, with TLS terminated
/// here in the client (never at a relay — see the module docs of
/// [`crate::imap_client`]).
pub struct NativeTlsTransport {
    pipe: Pipe,
    host: String,
    connector: TlsConnector,
}

impl NativeTlsTransport {
    /// Drive the TLS handshake over whatever plaintext stream we hold.
    async fn handshake(&mut self) -> Result<(), NativeTransportError> {
        let Pipe::Plain(tcp) = std::mem::replace(&mut self.pipe, Pipe::Upgrading) else {
            // Only reachable by calling this twice; restoring the pipe would
            // hide the bug, so treat it as one.
            return Err(NativeTransportError::Tls {
                host: self.host.clone(),
                source: std::io::Error::other("TLS is already established on this pipe"),
            });
        };

        let server_name = ServerName::try_from(self.host.clone())
            .map_err(|_| NativeTransportError::InvalidHostname(self.host.clone()))?;

        // A bad, expired, self-signed, or wrong-host chain dies here — which is
        // the whole point: the source password has not been sent yet.
        //
        // `Box::pin`'d per `native-async-execution.md` § The rule: this method
        // is reached inline from `FfiMailImportClient::connect`
        // (`fauna-ffi/src/mail_import.rs`), a `#[uniffi::export(async_runtime =
        // "tokio")]` method polled on the small foreign FFI stack (Swift
        // cooperative-pool / Kotlin dispatcher / .NET continuation thread).
        // Measured 1352 B unboxed — smaller than the 10.9 KB incident that
        // motivated the rule, but well above the 248 B a boxed leaf costs, and
        // boxing is free (a rare, user-initiated connect, not a hot path) —
        // see `imap_connect_future_stays_small_for_foreign_stacks` below.
        let tls = Box::pin(self.connector.connect(server_name, tcp))
            .await
            .map_err(|source| NativeTransportError::Tls {
                host: self.host.clone(),
                source,
            })?;

        self.pipe = Pipe::Tls(Box::new(tls));
        Ok(())
    }
}

impl ImapTransport for NativeTlsTransport {
    type Error = NativeTransportError;

    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        match &mut self.pipe {
            Pipe::Plain(s) => {
                s.write_all(bytes).await?;
                s.flush().await?;
            }
            // `tokio-rustls` buffers plaintext into TLS records; without the
            // flush the command sits in the writer and both ends wait forever.
            Pipe::Tls(s) => {
                s.write_all(bytes).await?;
                s.flush().await?;
            }
            Pipe::Upgrading => {
                return Err(NativeTransportError::Io(std::io::Error::other(
                    "write during TLS upgrade",
                )));
            }
        }
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = match &mut self.pipe {
            Pipe::Plain(s) => s.read(buf).await?,
            Pipe::Tls(s) => s.read(buf).await?,
            Pipe::Upgrading => {
                return Err(NativeTransportError::Io(std::io::Error::other(
                    "read during TLS upgrade",
                )));
            }
        };
        Ok(n)
    }

    async fn upgrade_tls(&mut self) -> Result<(), ImapClientError> {
        if matches!(self.pipe, Pipe::Tls(_)) {
            // Shared code only calls this on the STARTTLS path, so reaching it
            // with TLS already up means the caller passed an implicit-mode
            // transport to `connect_starttls`.
            return Err(ImapClientError::Protocol(
                "STARTTLS attempted on a connection that is already TLS".into(),
            ));
        }
        self.handshake().await.map_err(ImapClientError::transport)
    }
}

/// Wall clock + sleep for the § Throttling budget, on tokio.
///
/// `now_ms` counts from an origin fixed at construction, so it is monotone by
/// construction — [`ImapClock`] only ever takes differences, and a wall-clock
/// step (NTP, suspend/resume) must not be able to rewind the rolling window.
pub struct TokioClock {
    origin: Instant,
}

impl TokioClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for TokioClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ImapClock for TokioClock {
    fn now_ms(&self) -> u64 {
        // u128 → u64 truncates only after ~584 million years of uptime.
        self.origin.elapsed().as_millis() as u64
    }

    async fn sleep_ms(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// `LOGOUT` on the session held behind `slot`, clearing it either way — the
/// take-lock-logout-drop shape every "own an `Option<ImapSession<NativeTlsTransport>>`
/// behind a `Mutex`" caller hand-copies (`fauna-ffi`'s and
/// `fauna-client-mail-settings`'s native-transport `logout`). A missing
/// session is a no-op success, not an error — `LOGOUT` already happened, or
/// never needed to.
pub async fn logout_and_clear(
    slot: &tokio::sync::Mutex<Option<super::session::ImapSession<NativeTlsTransport>>>,
) -> Result<(), ImapClientError> {
    let mut guard = slot.lock().await;
    if let Some(mut session) = guard.take() {
        session.logout().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for `native-async-execution.md` § The rule (the
    /// SIGBUS-class hazard: a small foreign FFI poll stack + a large inline
    /// future). `FfiMailImportClient::connect` (`fauna-ffi/src/mail_import.rs`)
    /// awaited this connector's `connect` inline on the foreign poll stack
    /// (before `#[fauna_uniffi_async::export]` moved every export onto a tokio
    /// worker; Rust shells still await it inline), all the way down into
    /// [`NativeTlsTransport::handshake`]'s `rustls` TLS-handshake future —
    /// which was NOT `Box::pin`'d and had no size-assertion test anywhere in
    /// this crate (found by the documentation
    /// sweep's `native-async-execution` concept check). If a refactor un-boxes
    /// `handshake`'s connect call, this pins the regression: measured 1352 B
    /// unboxed vs. 248 B boxed today (`Implicit` mode drives the same
    /// handshake branch `StartTls`'s `connect_starttls` → `upgrade_tls`
    /// reaches, so this single leaf covers both TLS modes). Same threshold
    /// convention as `fauna_anon_client::ws`'s
    /// `connect_anonymous_future_stays_small_for_foreign_stacks` (generous vs.
    /// the boxed size, far below the unboxed one). Belt-and-suspenders: this
    /// was NOT the dominant contributor to the top-level FFI future (see the
    /// `Connection::next_event` guard below) — boxing here still matters
    /// because `NativeImapConnector::connect` is reachable on its own.
    #[test]
    fn imap_connect_future_stays_small_for_foreign_stacks() {
        let connector = NativeImapConnector::new(TlsMode::Implicit);
        let fut = connector.connect("imap.example.com", 993);
        let size = std::mem::size_of_val(&fut);
        // Boxed today: 248 bytes. Unboxed (the bug): 1352 bytes.
        assert!(
            size <= 512,
            "NativeImapConnector::connect future is {size} bytes — expected <= 512. \
             The TLS handshake future must stay Box::pin'd (native.rs's \
             NativeTlsTransport::handshake) so it does not overflow the small \
             foreign FFI poll stack FfiMailImportClient::connect is polled on."
        );
    }

    /// Regression guard for `Connection::next_event`'s fix (connection.rs):
    /// `chunk` used to be a `[0u8; 16 * 1024]` re-declared inside the read
    /// loop and held across its own `.await`, baking 16 KiB into every future
    /// that awaits `ImapSession::connect`/`connect_starttls` inline — measured
    /// 17040 bytes before the fix, 680 after. This was the DOMINANT
    /// contributor to the top-level FFI future (17088 B total, of which the
    /// TLS-handshake boxing above accounts for at most ~1.1 KB) — this is the
    /// future `FfiMailImportClient::connect` (`fauna-ffi`) awaits inline on
    /// the small foreign FFI poll stack (`native-async-execution.md` § The
    /// hazard); see
    /// `fauna_ffi::mail_import::tests::connect_future_stays_small_for_foreign_stacks`
    /// for the guard at that boundary. A local `TcpListener` keeps this
    /// deterministic and network-free — the connect completes for real (fast,
    /// loopback), but `ImapSession::connect`'s own future is only
    /// constructed, never polled.
    #[tokio::test]
    async fn imap_session_connect_future_stays_small_for_foreign_stacks() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connector = NativeImapConnector::new(TlsMode::StartTls);
        let transport = connector
            .connect(&addr.ip().to_string(), addr.port())
            .await
            .unwrap();
        let fut = crate::imap_client::ImapSession::connect(transport);
        let size = std::mem::size_of_val(&fut);
        assert!(
            size <= 2048,
            "ImapSession::connect future is {size} bytes — expected <= 2048. Check \
             for a large stack-local buffer (e.g. a fixed-size array) held across \
             an `.await` in the greeting/capabilities read path \
             (Connection::next_event is the site this fix lives at)."
        );
    }
}

//! The two platform seams: a byte pipe and a clock.
//!
//! Both are AFIT (`async fn` in trait, unboxed), deliberately *not*
//! `#[async_trait]`-boxed, so `Send` is inferred per impl — native futures are
//! `Send`, wasm futures are `!Send`, and one shared trait serves both. This
//! mirrors [`fauna_protocol::RpcRequester`], whose module docs spell out the
//! same reasoning.

/// How the source connection reaches TLS.
///
/// § Wizard steps ratifies exactly these two for *Generic IMAP* ("server
/// hostname, port (default 993), **TLS mode (implicit / STARTTLS)**, username,
/// password"), and § Credential handling persists the choice client-side. The
/// three provider presets (Gmail / iCloud / Outlook) are all
/// [`Self::Implicit`] on 993.
///
/// There is deliberately **no plaintext variant**: the source password (or
/// OAuth bearer) crosses this connection, so an unencrypted source session is
/// never an option a client may offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// TLS from the first byte — the IMAPS (993) default.
    Implicit,
    /// Connect in the clear, then negotiate RFC 3501 §6.2.1 `STARTTLS` *before*
    /// authenticating — the 143 default. See [`ImapTransport::upgrade_tls`].
    StartTls,
}

/// A bidirectional stream of **post-TLS, plaintext IMAP bytes**.
///
/// The implementor owns the socket *and the TLS*. Shared code above this trait
/// never sees a certificate, a cipher suite, or a hostname — it reads and
/// writes IMAP protocol bytes.
///
/// Placing the seam here (rather than at raw TCP) is what makes the web path
/// safe: the WASM impl drives a sans-io `rustls::ClientConnection` and ferries
/// its ciphertext over a WebSocket relay, so **the relay is a blind byte
/// tunnel** that never sees the user's source-mailbox credentials. See the
/// module docs of [`crate::imap_client`].
#[allow(async_fn_in_trait)] // Static dispatch only; we *want* per-impl `Send`
// inference (native `Send`, wasm `!Send`), which an explicit `Send` bound or
// `async_trait(?Send)` boxing would defeat. Same rationale as
// `fauna_protocol::RpcRequester`.
pub trait ImapTransport {
    /// Transport-specific error, bounded `Display` so
    /// [`crate::imap_client::ImapClientError::Transport`] can render it.
    type Error: core::fmt::Display;

    /// Write every byte of `bytes` to the source server.
    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;

    /// Read some bytes from the source server into `buf`.
    ///
    /// Returns the number of bytes read. **`Ok(0)` means clean EOF** — the
    /// server closed the connection — and shared code turns that into
    /// [`crate::imap_client::ImapClientError::Eof`]. It must never be returned
    /// merely because no bytes are available yet; that is what `await` is for.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Upgrade this pipe to TLS **in place** (RFC 3501 §6.2.1 `STARTTLS`).
    ///
    /// § Wizard steps ratifies two source TLS modes — *implicit* (TLS from the
    /// first byte, the 993 default) and *STARTTLS* (negotiate over a plaintext
    /// 143 connection) — so the seam must express the second. Shared code owns
    /// the **command exchange** ([`crate::imap_client::ImapSession::connect_starttls`]
    /// sends `STARTTLS` and awaits the tagged `OK`); the transport owns the
    /// **handshake**, keeping certificates and cipher suites out of shared code
    /// exactly as the implicit path does.
    ///
    /// Returns [`crate::imap_client::ImapClientError`] rather than
    /// `Self::Error`, because "this pipe cannot become TLS" is a fact about the
    /// *protocol mode*, not an I/O failure — and a default body cannot
    /// construct an arbitrary `Self::Error`. An implementor maps its own
    /// handshake failure to
    /// [`crate::imap_client::ImapClientError::Transport`].
    ///
    /// The default is "cannot upgrade", which is right for every implicit-TLS
    /// transport: STARTTLS is never attempted on one.
    async fn upgrade_tls(&mut self) -> Result<(), crate::imap_client::ImapClientError> {
        Err(crate::imap_client::ImapClientError::StartTlsUnsupported)
    }
}

/// Wall-clock reads and sleeps, supplied by the platform.
///
/// Shared code may not call `std::time::Instant::now()`: it panics on
/// `wasm32-unknown-unknown`. Nor may it sleep — there is no portable sleep. So
/// § Throttling's rate limiter is a pure decision function
/// ([`crate::imap_client::FetchThrottle`]) fed by `now_ms`, and the single
/// await point calls `sleep_ms`.
///
/// `now_ms` is *any* monotonic-enough millisecond counter; only differences
/// matter. It need not be a Unix epoch.
#[allow(async_fn_in_trait)] // Same per-impl `Send` inference as `ImapTransport`.
pub trait ImapClock {
    /// Milliseconds since an arbitrary fixed origin.
    fn now_ms(&self) -> u64;

    /// Sleep for at least `ms` milliseconds.
    async fn sleep_ms(&self, ms: u64);
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use core::cell::RefCell;

    /// Drive a future to completion without a runtime.
    ///
    /// Every seam impl below is synchronous, so the future resolves on the
    /// first poll — a `Pending` means a test under-scripted its transport, and
    /// panicking beats hanging. This keeps the unit tests tokio-free and
    /// runnable on any target, `wasm32` included.
    pub(crate) use fauna_client_testkit::block_on;

    /// A scripted source server: hands out canned response bytes and records
    /// everything the client sent. Drives every unit test in this module —
    /// per § C4, a fake *source server* beats mocking the client.
    pub(crate) struct ScriptedTransport {
        /// Response chunks, returned in order. Splitting one IMAP response
        /// across chunks exercises the sans-io re-feed path.
        chunks: RefCell<std::collections::VecDeque<Vec<u8>>>,
        /// Everything `write_all` received, concatenated.
        pub written: RefCell<Vec<u8>>,
    }

    impl ScriptedTransport {
        pub(crate) fn new<I, B>(chunks: I) -> Self
        where
            I: IntoIterator<Item = B>,
            B: AsRef<[u8]>,
        {
            Self {
                chunks: RefCell::new(chunks.into_iter().map(|c| c.as_ref().to_vec()).collect()),
                written: RefCell::new(Vec::new()),
            }
        }

        /// Everything the client sent, as a lossy string (for assertions).
        pub(crate) fn sent(&self) -> String {
            String::from_utf8_lossy(&self.written.borrow()).into_owned()
        }
    }

    #[derive(Debug)]
    pub(crate) struct ScriptExhausted;

    impl core::fmt::Display for ScriptExhausted {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("scripted transport ran out of responses")
        }
    }

    impl ImapTransport for ScriptedTransport {
        type Error = ScriptExhausted;

        async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
            self.written.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }

        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let mut chunks = self.chunks.borrow_mut();
            let Some(front) = chunks.front_mut() else {
                // Script exhausted => model a clean server close, so tests that
                // under-script fail as `Eof` rather than hanging.
                return Ok(0);
            };
            let n = front.len().min(buf.len());
            buf[..n].copy_from_slice(&front[..n]);
            front.drain(..n);
            if front.is_empty() {
                chunks.pop_front();
            }
            Ok(n)
        }
    }

    /// A clock the test drives by hand. `sleep_ms` advances virtual time
    /// instead of blocking, so throttle tests run instantly and
    /// deterministically.
    pub(crate) struct FakeClock {
        now: RefCell<u64>,
        pub slept: RefCell<Vec<u64>>,
    }

    impl FakeClock {
        pub(crate) fn new() -> Self {
            Self {
                now: RefCell::new(0),
                slept: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn advance(&self, ms: u64) {
            *self.now.borrow_mut() += ms;
        }
    }

    impl ImapClock for FakeClock {
        fn now_ms(&self) -> u64 {
            *self.now.borrow()
        }

        async fn sleep_ms(&self, ms: u64) {
            self.slept.borrow_mut().push(ms);
            self.advance(ms);
        }
    }
}

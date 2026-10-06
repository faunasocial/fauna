//! Source-side IMAP client — the client half of mailbox migration.
//!
//! `docs/goal/behavior/mailbox-migration.md` § Client-driven streaming model
//! owns this surface. The user's own Fauna app opens an IMAP session to a
//! **foreign** server (Gmail / Outlook / iCloud / generic), FETCHes each
//! message, and pushes it into nest over `fauna.bridges.import_message`.
//! **Nest never receives the source credentials** (§ Credential handling), so
//! every byte of this module runs client-side.
//!
//! # Why the transport is a seam
//!
//! The browser has no raw TCP, so a shared IMAP client cannot own its socket.
//! Everything that carries ratified *behaviour* — the protocol state machine,
//! the per-source-server throttle, batch packing, the [`crate::dedup_key`]
//! call, cursor resume — lives here, once, generic over an
//! [`ImapTransport`]: a plaintext-IMAP byte pipe the
//! platform shell provides.
//!
//! | Platform | `ImapTransport` impl |
//! |---|---|
//! | native (UniFFI) | `tokio::net::TcpStream` + `tokio-rustls` |
//! | web (WASM) | `rustls::ClientConnection` (sans-io) over a **blind byte tunnel** |
//!
//! **The seam carries post-TLS plaintext IMAP bytes, and each impl terminates
//! its own TLS.** For web that means TLS terminates *inside the client* and
//! the relay ferries opaque ciphertext. A relay that terminated TLS instead
//! would hand nest the user's foreign-mailbox password and their entire
//! mailbox in the clear — contradicting § Credential handling ("Nest never
//! receives the source credentials … a hard rule, not a default") and the
//! *User always controls their data* product invariant. **Never build a
//! credential-terminating relay.**
//!
//! The seam is AFIT (`async fn` in trait, unboxed) rather than `async_trait`
//! for the same reason [`fauna_protocol::RpcRequester`] is: per-impl `Send`
//! inference, so native gets `Send` futures and wasm's `!Send` ones still
//! compile.
//!
//! # Why the clock is a seam
//!
//! § Throttling caps FETCH at 4 concurrent and 100 messages/minute per source
//! server. Enforcing that needs *now* and *sleep*, neither of which shared
//! code may take from `std` (`Instant::now()` panics on
//! `wasm32-unknown-unknown`). So [`throttle::FetchThrottle`] is a **pure**
//! decision function over a caller-supplied millisecond clock, and the one
//! await point rides an [`ImapClock`] impl.

pub mod batch;
mod connection;
pub mod internaldate;
#[cfg(feature = "imap-client-native")]
pub mod native;
pub mod send;
pub mod session;
#[cfg(feature = "tls-test-fixtures")]
pub mod test_fixtures;
pub mod throttle;
pub mod transport;

pub use batch::{BatchPacker, ImportUnit};
#[cfg(feature = "imap-client-native")]
pub use native::{
    NativeImapConnector, NativeTlsTransport, NativeTransportError, TokioClock, logout_and_clear,
};
pub use send::{ImportUnitReply, MailImportClient};
pub use session::{FetchOutcome, ImapSession, MailboxStatus, SourceMailbox, UidEntry};
pub use throttle::{FetchThrottle, ThrottleDecision};
pub use transport::{ImapClock, ImapTransport, TlsMode};

/// § Batching: the per-call ceilings, re-exported from the crate that owns the
/// `fauna.bridges.import_message_batch` wire contract so the client and the
/// nest handler enforcing them cannot drift apart.
///
/// A single message may legitimately exceed [`MAX_BATCH_BYTES`] (§ Scope &
/// limits allows up to [`DEFAULT_MAX_MESSAGE_BYTES`]). Such a message can never
/// ride in a batch, so [`BatchPacker`] routes it to the single-message
/// `fauna.bridges.import_message` kind, whose handler enforces no byte
/// ceiling. See [`ImportUnit`].
pub use fauna_protocol::bridge_routing::{MAX_BATCH_BYTES, MAX_BATCH_MESSAGES};

/// § Scope & limits: "Max message size: **50 MiB per message** (matches
/// Gmail's send limit; user can lower)". Messages whose `RFC822.SIZE` exceeds
/// the effective limit are skipped before the body is ever fetched.
pub const DEFAULT_MAX_MESSAGE_BYTES: u64 = 50 * 1024 * 1024;

/// § Throttling: "at most 4 concurrent FETCH operations".
pub const MAX_CONCURRENT_FETCH: u32 = 4;

/// § Throttling: "at most 100 messages per minute per source server" — per
/// *server*, not per mailbox, so one [`FetchThrottle`] spans the whole import.
pub const MAX_FETCH_PER_MINUTE: u32 = 100;

/// Upper bound on unparsed bytes we will hold while waiting for one complete
/// IMAP response. A hostile or broken source server that never terminates a
/// literal must not drive the client OOM. Sized well above
/// [`DEFAULT_MAX_MESSAGE_BYTES`] plus the response framing around it.
pub const MAX_RESPONSE_BUFFER: usize = 64 * 1024 * 1024;

/// Everything that can go wrong talking to a *foreign* IMAP server.
///
/// Variants split along the line the wizard's error budget cares about
/// (§ Failure handling): [`Self::Rejected`] and [`Self::UidValidityChanged`]
/// are the source server telling us something true; the rest are faults.
#[derive(Debug, thiserror::Error)]
pub enum ImapClientError {
    /// The platform transport failed. Stringified at the boundary because
    /// `ImapTransport::Error` is only bounded `Display` (per-impl types).
    #[error("source transport error: {0}")]
    Transport(String),

    /// The source server closed the connection mid-session.
    #[error("the source server closed the connection")]
    Eof,

    /// Bytes arrived that are not a well-formed IMAP response.
    #[error("malformed IMAP response from the source server: {0}")]
    Protocol(String),

    /// A tagged `NO` / `BAD` completion. `command` is the IMAP verb (never the
    /// arguments — a `LOGIN` line carries the user's password).
    #[error("the source server rejected {command}: {status} {text}")]
    Rejected {
        command: String,
        status: String,
        text: String,
    },

    /// The response buffer grew past [`MAX_RESPONSE_BUFFER`] without yielding
    /// one complete response.
    #[error("source response exceeded the {limit}-byte buffer without completing")]
    ResponseTooLarge { limit: usize },

    /// § Progress lives nest-side: "if the source UIDVALIDITY rotates
    /// mid-import (rare — usually means the source mailbox was rebuilt) the
    /// client surfaces a warning + restart-import option". Resume must abort
    /// rather than silently import against a rebuilt UID space.
    #[error(
        "source mailbox {mailbox} UIDVALIDITY changed: cursor has {expected}, server reports {actual}"
    )]
    UidValidityChanged {
        mailbox: String,
        expected: u32,
        actual: u32,
    },

    /// The server answered a command we did not send, or omitted a required
    /// untagged response (e.g. `EXAMINE` without `UIDVALIDITY`).
    #[error("the source server violated the {0} contract")]
    UnexpectedResponse(String),

    /// STARTTLS was asked of a transport that cannot upgrade in place.
    /// See [`ImapTransport::upgrade_tls`].
    #[error("this transport cannot upgrade to TLS (STARTTLS unsupported)")]
    StartTlsUnsupported,

    /// The source server sent bytes that arrived **before** the TLS handshake
    /// but would be read **after** it — a plaintext-injection attempt (the
    /// STARTTLS command-injection class, CVE-2011-0411 and friends). A MITM
    /// that can append to the plaintext stream would otherwise have those bytes
    /// interpreted as if the authenticated server had sent them inside TLS.
    /// RFC 3501 §6.2.1 requires discarding such data; we refuse the session
    /// outright, because a well-behaved server never produces it.
    #[error("the source server buffered plaintext data across the STARTTLS boundary")]
    StartTlsPlaintextInjection,
}

/// One message fetched from the source server, before it becomes an
/// `ImportMessageItem`.
///
/// `body` is the raw RFC 5322 bytes exactly as `BODY.PEEK[]` returned them.
/// The client sends these bytes **unmodified** in both storage modes — the
/// nest seals body + index hint at ingest (§ Per-message flow step 4). This
/// module never encrypts and never parses the body for anything but the two
/// derived fields the wire needs (`dedup_key`, `sender_domain`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedMessage {
    /// Source mailbox the message was fetched from; becomes the destination
    /// mailbox name (auto-created nest-side, same as APPEND).
    pub mailbox: String,
    /// Source IMAP UID — half of the resume cursor.
    pub uid: u32,
    /// Source mailbox UIDVALIDITY — the other half.
    pub uid_validity: u32,
    /// IMAP flags carried over from the source. `\Recent` is stripped here:
    /// nest's `validate_item` rejects the whole message over it, and `\Recent`
    /// is a per-session artifact that means nothing in the destination.
    pub flags: Vec<String>,
    /// `INTERNALDATE` as epoch seconds (see [`internaldate`]).
    pub internal_date_epoch: i64,
    /// Raw RFC 5322 bytes.
    pub body: Vec<u8>,
}

impl ImapClientError {
    /// Wrap a platform transport error. Kept here so every call site renders
    /// `T::Error` the same way.
    pub(crate) fn transport<E: core::fmt::Display>(e: E) -> Self {
        Self::Transport(e.to_string())
    }
}

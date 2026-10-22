//! `RpcRequester` — the transport seam shared by native and wasm clients.
//!
//! The per-feature typed-call wrappers (`fauna-client-bridges`,
//! `fauna-client-email`, …) are generic over this trait so the kind-
//! composition logic is written once and consumed by both the native
//! `NestClient` (reqwest + tokio-tungstenite, `libs/fauna-client`) and the
//! wasm `WsRpcClient` (gloo-net over a browser `WebSocket`,
//! `libs/fauna-rpc-wasm`). Only the transport client differs.
//!
//! ## Why `async fn in trait`, not `#[async_trait(?Send)]`
//!
//! This trait is **static-dispatch only** — its `request` method is generic
//! over `Req`/`Reply`, which makes it dyn-incompatible, so feature crates take
//! `R: RpcRequester` (never `dyn RpcRequester`). That lets us use a native
//! `async fn` in the trait and rely on **per-impl auto-trait inference**: the
//! native `NestClient` impl yields a `Send` future (so callers can
//! `tokio::spawn` a feature-crate call — e.g. linux's `runtime.spawn(async
//! move { bridges.feeds_list().await })`), while the wasm `WsRpcClient` impl
//! over a `!Send` browser `WebSocket` yields a `!Send` future (driven by
//! `spawn_local`). `#[async_trait(?Send)]` would box *every* impl's future
//! *without* `Send`, regressing the native `tokio::spawn` call sites — exactly
//! the ergonomics regression we must avoid.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::RpcError;

/// A typed request/reply transport to nest. One method: encode `payload`
/// under `kind`, await the reply, decode it into `Reply`. Per-kind metadata
/// (deadline, replay policy) is resolved by the implementation from its
/// [`crate::KindRegistry`].
#[allow(async_fn_in_trait)] // See module docs: static-dispatch only; we *want*
// per-impl `Send` inference (native `Send`, wasm `!Send`), which an explicit
// `Send` bound or `async_trait(?Send)` boxing would defeat.
pub trait RpcRequester {
    /// Transport-specific error. Bounded `Display` so UI glue can render it
    /// (the only thing call sites do with it today). Native =
    /// `fauna_client::NestClientError`; wasm = the rpc-wasm error.
    type Error: core::fmt::Display;

    /// Send `payload` as a request of `kind`, await the reply, decode it.
    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: Serialize,
        Reply: DeserializeOwned;

    /// The number of the connection a request issued now would ride
    /// ([`crate::RpcDispatcher::connection_id`], minted per (re)connect), or
    /// `None` when none is live — or when this transport keeps no
    /// per-connection memo, the default. The key of
    /// [`Self::nest_info_memo`].
    fn live_connection_id(&self) -> Option<u64> {
        None
    }

    /// This transport's per-connection memo of the `fauna.nest.info` reply
    /// ([`crate::NestInfoMemo`]), read through [`crate::read_nest_info`] —
    /// `None` for a transport that remembers nothing (the default: a fake, a
    /// port-built web client, a wrapper that does not forward it), whose
    /// readers then read on every call.
    fn nest_info_memo(&self) -> Option<&crate::NestInfoMemo> {
        None
    }
}

/// The outbox-drain seam: a transport that sends a request whose envelope
/// carries a **caller-supplied** idempotency key instead of the fresh random
/// one [`RpcRequester::request`] mints per call.
///
/// A separate trait rather than a defaulted method on [`RpcRequester`] on
/// purpose: a default impl could only delegate to `request`, silently minting
/// a fresh key — and a replayed outbox intent whose key changed per attempt
/// would double-apply the moment the nest's durable idempotency table keys on
/// it (charter: `account-data-plane.md` § The offline-mutation contract). A
/// transport that cannot thread the key must fail to compile at the drain
/// call site, never degrade quietly. The ~130 test fakes that implement
/// `RpcRequester` are untouched; a fake implements this only when its test
/// drains an outbox.
#[allow(async_fn_in_trait)] // same static-dispatch / per-impl Send story as RpcRequester
pub trait KeyedRpcRequester: RpcRequester {
    /// Send `payload` as a request of `kind` whose envelope idempotency key
    /// is `idempotency_key`. Replaying the same key re-presents the same
    /// logical request: the nest's idempotency layers reply with the recorded
    /// outcome instead of applying a second effect.
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: Serialize,
        Reply: DeserializeOwned;
}

/// Classifies a transport error for the two-class `NestError` the shared
/// machine seams expose (`fauna-client-dns`'s `DnsNest`,
/// `fauna-client-mail-settings`'s `LocalDomainNest` / `BridgeApprovalNest`).
/// A *rejection* reached nest and was refused (malformed
/// request, permission denied, unknown resource — a `Reply.ok = false` RPC
/// error); everything else (disconnect, timeout, framing, auth refresh) is a
/// transport fault the caller may retry.
///
/// The shared `Rpc*Nest` seam glue is generic over `R: RpcRequester` and maps
/// `R::Error` into `Rejected` vs `Transient` through this trait, so it never
/// needs to know the concrete transport error type (native
/// `fauna_client::NestClientError` / wasm `WsRpcError`). Each transport crate
/// impls it next to its error type.
pub trait RpcErrorClass {
    /// `true` if the request reached nest and was refused; `false` for a
    /// transport fault (disconnect / timeout / framing / auth).
    fn is_rejection(&self) -> bool;

    /// The wire [`RpcError`] when this is a server rejection (`Reply.ok =
    /// false`), else `None` (a transport fault). The two-class
    /// [`is_rejection`](Self::is_rejection) collapses to "did it reach nest";
    /// this exposes the *code* so generic kind-calling glue can map a server
    /// rejection onto a richer per-feature error enum without knowing the
    /// concrete transport error type. The onboarding `WsRpcNestApi`
    /// (`fauna-onboarding-machine`) uses it to map e.g.
    /// `fauna.account.invite_request_not_found` → `InviteRequestError::NotFound`
    /// across both the native and wasm anonymous connectors.
    ///
    /// Defaults to `None`; the transport error types
    /// (`fauna_client::NestClientError`, `fauna_rpc_wasm::WsRpcError`) override
    /// it. A classifier that only needs the boolean keeps using `is_rejection`.
    fn as_rpc_error(&self) -> Option<&RpcError> {
        None
    }

    /// `true` when the nest refused this app's subprotocol — the pre-upgrade
    /// HTTP 426, or the 4426 close that ends a completed upgrade: the app is
    /// too old for the nest, and no retry clears it
    /// (`transport-connection.md` § Connection lifecycle, the 4426 paragraph).
    /// The typed predicate the shared classifiers read
    /// ([`crate::auth::silent_challenge_error`], the onboarding probe), never
    /// a match on the error's text. Defaults to `false`; the transport error
    /// types with a `SubprotocolMismatch` variant override it.
    fn is_subprotocol_mismatch(&self) -> bool {
        false
    }

    /// `true` when the connection itself went away under the request — the
    /// socket closed or failed mid-call — so a fresh dial may succeed where
    /// this one cannot. Narrower than `!is_rejection()`: a timeout, a decode
    /// failure or a refused subprotocol says nothing about the socket. The
    /// typed predicate a redial reads (`apps/cli.md` § The device shell →
    /// *A dropped session*), never a match on the error's text. Defaults to
    /// `false`.
    fn is_connection_lost(&self) -> bool {
        false
    }
}

/// `Infallible` (the error of a never-failing transport — e.g. an in-memory
/// test fake) trivially satisfies `RpcErrorClass`: it is uninhabited, so a
/// value can never exist to classify. This lets such a fake back any
/// `R::Error: RpcErrorClass`-bounded call surface without a hand-rolled error enum.
impl RpcErrorClass for core::convert::Infallible {
    fn is_rejection(&self) -> bool {
        match *self {}
    }
}

/// The two-class outcome of [`classify_rpc_error`] over a **nest** seam: the
/// call either never reached the nest, or reached it and was refused.
///
/// One type for every client crate whose nest seam classifies exactly that
/// (`fauna-client-dns`, `fauna-client-mail-settings`, `fauna-client-pair`,
/// `fauna-client-bridges`), each re-exporting it under the name its own API
/// already used — [`crate::NestSeamError`] *is* `DnsNestError`,
/// `PairNestError`, mail-settings' `NestError` and `DiscoverHoldersError`.
///
/// ⚠ **These four were four hand-written copies until 2026-08-23**, identical
/// down to the two `Display` spellings below, and each doc comment licensed the
/// copy on a reason that was **false**: *"they must stay distinct per-crate
/// under UniFFI's flat namespace"*. None of them is UniFFI-exported. The
/// `derive(uniffi::Error)` sits on the crates' `*DispatchError` wrappers, which
/// are `uniffi(flat_error)` — represented at the FFI boundary by their
/// `Display` string — which is why, as `fauna-client-dns`'s own doc says two
/// paragraphs from the excuse, *"the inner seam errors need no annotation"*.
/// The flat-namespace constraint is real, and it applies to the `*DispatchError`
/// names alone.
///
/// The `Display` text is the load-bearing part: `flat_error` means these exact
/// strings are what the seven apps' `error-message` element renders, so all
/// four copies had to agree and nothing held them to each other.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NestSeamError {
    /// The call never reached the nest — WS disconnected mid-call, timeout,
    /// framing/auth fault, rate limit, server 5xx. Retryable.
    #[error("nest unreachable: {0}")]
    Transient(String),
    /// The nest reached a decision and refused — an admin knob is off, the
    /// actor is unknown, a precondition failed, a payload was malformed.
    #[error("nest rejected: {0}")]
    Rejected(String),
}

/// [`classify_rpc_error`] specialised to [`NestSeamError`] — the whole body of
/// what `fauna-client-dns`/`-mail-settings`/`-pair`'s `nest_error` and
/// `fauna-client-bridges`' `discover_holders_error` each used to spell out.
pub fn nest_seam_error<E>(e: E) -> NestSeamError
where
    E: RpcErrorClass + core::fmt::Display,
{
    classify_rpc_error(&e, NestSeamError::Transient, NestSeamError::Rejected)
}

/// Classify a transport error via [`RpcErrorClass::is_rejection`] into
/// whichever of the two caller-supplied constructors applies.
///
/// Callers whose two classes are *the nest seam's* two classes want
/// [`nest_seam_error`] and its shared [`NestSeamError`] instead of a private
/// twin; this generic form is for the seams whose taxonomy genuinely differs —
/// `fauna-client-dns`'s `DnsProviderError` (a third-party DNS API, not a nest)
/// and `fauna-client-mls-sync`'s `MlsTransportError::classify`.
pub fn classify_rpc_error<E, T>(
    e: &E,
    transient: impl FnOnce(String) -> T,
    rejected: impl FnOnce(String) -> T,
) -> T
where
    E: RpcErrorClass + core::fmt::Display,
{
    if e.is_rejection() {
        rejected(e.to_string())
    } else {
        transient(e.to_string())
    }
}

/// `Send + Sync` on every target except wasm32, where it is empty — the
/// supertrait the WS-RPC machine seams (`fauna-client-dns`'s `DnsManagementMachine`,
/// `fauna-client-mail-settings`'s admin machines) use so one trait body serves
/// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
/// `Rc`-based `WsRpcClient`, `!Send`). Canonical definition lives in
/// [`fauna_core::MaybeSendSync`] (so dependency-light seam crates that avoid a
/// `fauna-protocol` dep — e.g. `fauna-conversations` — share it); re-exported
/// here for the protocol-coupled seams. Pairs with the
/// `#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]` arm on the seam
/// trait (the native arm boxes `Send` futures; the wasm arm `!Send` ones).
pub use fauna_core::MaybeSendSync;

/// Blanket impl so a call surface that only *borrows* a transport can back a
/// generic `R: RpcRequester` wrapper without giving up ownership. The
/// onboarding `WsRpcNestApi` is the motivating case: it owns its connector for
/// the wizard's own kinds, and hands the same borrow to
/// `fauna_client_recovery::RecoveryClient` to drive the pre-identity restore
/// ceremony on that one connection. Delegates through the reference; the
/// `Send`-ness of the returned future is inferred per concrete `T` exactly as
/// the module docs require.
impl<T: RpcRequester + ?Sized> RpcRequester for &T {
    type Error = T::Error;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        (**self).request(kind, payload).await
    }

    fn live_connection_id(&self) -> Option<u64> {
        (**self).live_connection_id()
    }

    fn nest_info_memo(&self) -> Option<&crate::NestInfoMemo> {
        (**self).nest_info_memo()
    }
}

impl<T: KeyedRpcRequester + ?Sized> KeyedRpcRequester for &T {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        (**self).request_keyed(kind, idempotency_key, payload).await
    }
}

/// Blanket impl so the `Arc<NestClient>` shape native call sites already hold
/// satisfies `R: RpcRequester` without rewrapping. Delegates through the `Arc`.
impl<T: RpcRequester + ?Sized> RpcRequester for std::sync::Arc<T> {
    type Error = T::Error;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        (**self).request(kind, payload).await
    }

    fn live_connection_id(&self) -> Option<u64> {
        (**self).live_connection_id()
    }

    fn nest_info_memo(&self) -> Option<&crate::NestInfoMemo> {
        (**self).nest_info_memo()
    }
}

impl<T: KeyedRpcRequester + ?Sized> KeyedRpcRequester for std::sync::Arc<T> {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        (**self).request_keyed(kind, idempotency_key, payload).await
    }
}

#[cfg(test)]
mod nest_seam_error_tests {
    use super::*;

    /// A transport error whose class the test dictates.
    struct Fake {
        rejection: bool,
    }

    impl core::fmt::Display for Fake {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("detail")
        }
    }

    impl RpcErrorClass for Fake {
        fn is_rejection(&self) -> bool {
            self.rejection
        }
    }

    /// The two `Display` spellings are a **cross-crate contract**, not
    /// cosmetics. `fauna-client-{dns,pair,mail-settings}`' `*DispatchError`
    /// wrappers are `uniffi(flat_error)` — represented at the FFI boundary by
    /// their `Display` string — so these exact bytes are what all seven apps
    /// render in the `error-message` element, and `fauna-wasm`'s `pairing.rs`
    /// builds a `Transient` carrying only a peer address *because* the prefix
    /// already reads "nest unreachable". They were four hand-written copies
    /// until 2026-08-23 with nothing holding them to each other.
    #[test]
    fn the_two_display_spellings_are_pinned() {
        assert_eq!(
            NestSeamError::Transient("boom".into()).to_string(),
            "nest unreachable: boom"
        );
        assert_eq!(
            NestSeamError::Rejected("boom".into()).to_string(),
            "nest rejected: boom"
        );
    }

    /// `is_rejection()` picks the class and the transport error's own `Display`
    /// becomes the payload — the body each consumer's `nest_error` used to
    /// spell out for itself.
    #[test]
    fn the_classifier_routes_both_ways_and_carries_the_display() {
        assert_eq!(
            nest_seam_error(Fake { rejection: false }),
            NestSeamError::Transient("detail".into())
        );
        assert_eq!(
            nest_seam_error(Fake { rejection: true }),
            NestSeamError::Rejected("detail".into())
        );
    }
}

/// What one connection **proved** about the nest at its far end: the identity
/// the connection is bound to, cross-checked against the nest's own
/// `fauna.nest.info` claim, and the address the connection was dialled on.
/// The value a [`ProvenIdentityMemo`] remembers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenNestIdentity {
    /// The nest's 32-byte Ed25519 identity, as the connection proved it.
    pub nest_id: [u8; 32],
    /// The address the connection was dialled on.
    pub url: String,
}

/// The identity a transport's **current connection** has already proven,
/// remembered for that connection and no other — the memo behind "prove once
/// per connection" (`security.md` § Transport trust → *The proven identity is
/// remembered per connection*).
///
/// Keyed on the connection's [`crate::RpcDispatcher::connection_id`], a
/// process-unique number minted per (re)connect: a value remembered on
/// connection N is never served on connection N+1, so a reconnect to the same
/// address, a different box behind it, or a client re-bound to another nest
/// proves afresh. The caller takes a *ticket* — the live connection's number
/// — BEFORE it runs the proof and redeems it after: a proof whose connection
/// changed underneath it ([`Self::remember`] with a ticket that is no longer
/// the live connection) remembers nothing, so an identity proven over one
/// socket is never attributed to the next.
///
/// Both transport clients own one (native `NestClient`, wasm `WsRpcClient`);
/// the one resolver every app reads the bound identity through
/// (`fauna_client_pair::LinkedNestsMachine::bound_nest_id`) consults it, so
/// the per-call `fauna.nest.info` re-proof that drew on the nest's per-actor
/// discovery budget on every backups-page read is one round trip per
/// connection.
#[derive(Debug, Default)]
pub struct ProvenIdentityMemo {
    slot: std::sync::Mutex<Option<(u64, ProvenNestIdentity)>>,
}

impl ProvenIdentityMemo {
    /// An empty memo — nothing proven on any connection yet.
    pub const fn new() -> Self {
        Self {
            slot: std::sync::Mutex::new(None),
        }
    }

    /// The identity remembered for connection `live`, if that very connection
    /// proved one. `None` for a connection that has proven nothing yet, which
    /// includes every connection after the one the memo was taken on.
    pub fn get(&self, live: u64) -> Option<ProvenNestIdentity> {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        match slot.as_ref() {
            Some((connection, identity)) if *connection == live => Some(identity.clone()),
            _ => None,
        }
    }

    /// Remember `identity` as proven on connection `ticket` — but only while
    /// `ticket` still names the live connection (`live`): a proof that
    /// straddled a reconnect ran, at least in part, on a connection that is
    /// gone, and is not attributed to the one that replaced it. Returns
    /// whether it was remembered.
    pub fn remember(&self, ticket: u64, live: Option<u64>, identity: ProvenNestIdentity) -> bool {
        if live != Some(ticket) {
            return false;
        }
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        *slot = Some((ticket, identity));
        true
    }

    /// Forget whatever is remembered. The clients never need this — a new
    /// connection carries a new number — but a refused re-proof over a
    /// connection that once passed must not keep serving the earlier answer.
    pub fn clear(&self) {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
    }
}

#[cfg(test)]
mod proven_identity_tests {
    use super::*;

    fn id(byte: u8) -> ProvenNestIdentity {
        ProvenNestIdentity {
            nest_id: [byte; 32],
            url: "https://nest.test".into(),
        }
    }

    #[test]
    fn a_memo_answers_only_the_connection_it_was_taken_on() {
        let memo = ProvenIdentityMemo::new();
        assert_eq!(memo.get(7), None);
        assert!(memo.remember(7, Some(7), id(0xaa)));
        assert_eq!(memo.get(7), Some(id(0xaa)));
        // The next connection remembers nothing — a reconnect proves afresh.
        assert_eq!(memo.get(8), None);
        assert!(memo.remember(8, Some(8), id(0xbb)));
        assert_eq!(memo.get(8), Some(id(0xbb)));
        assert_eq!(memo.get(7), None, "the earlier connection's answer is gone");
    }

    #[test]
    fn a_proof_that_straddled_a_reconnect_is_not_remembered() {
        let memo = ProvenIdentityMemo::new();
        // Ticket taken on connection 3; by the time the proof completes the
        // live connection is 4 (or none at all).
        assert!(!memo.remember(3, Some(4), id(0xaa)));
        assert!(!memo.remember(3, None, id(0xaa)));
        assert_eq!(memo.get(3), None);
        assert_eq!(memo.get(4), None);
    }

    #[test]
    fn clear_forgets() {
        let memo = ProvenIdentityMemo::new();
        assert!(memo.remember(1, Some(1), id(0xaa)));
        memo.clear();
        assert_eq!(memo.get(1), None);
    }
}

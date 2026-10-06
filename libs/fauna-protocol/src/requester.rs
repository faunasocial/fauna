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

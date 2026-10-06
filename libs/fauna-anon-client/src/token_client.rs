//! [`TokenNestClient`] — an authed **one-shot** WS-RPC connection over a
//! caller-supplied raw bearer token.
//!
//! The native twin of `fauna_rpc_wasm::TokenWsRpcClient` and the bearer sibling
//! of [`crate::AnonymousNestClient`]: same fixed-dispatcher, no-reconnect-
//! supervisor shape, but it joins the connection with `Sec-WebSocket-Protocol:
//! fauna.v1, bearer.<token>` (the authenticated `GET /api/v1/ws/{actor_id}`
//! endpoint) so the nest routes the full authenticated kind set — not just the
//! pre-identity allowlist.
//!
//! Over `wss://` it secures the connection with the graduated cross-connection
//! SPKI pin: if a prior `fauna.auth.handshake` graduated a bound SPKI for the
//! host (the self-signed / DNS-`self=` path — see
//! [`crate::mint_bearer_over_handshake`]), the bearer TLS must present that exact
//! SPKI (security.md § Cross-connection binding); otherwise strict WebPKI. It
//! **never** uses the accept-any capturing verifier the *anonymous* connect does
//! — that is safe only for the pre-identity connection whose mint graduates the
//! binding; a post-mint authed read has a real identity to hold the transport
//! to. See [`crate::ws::connect_authed`].
//!
//! It exists for Rust-side flows that need one authed round-trip without the
//! full `fauna-client` `NestClient` (and its reconnect supervisor): the
//! box-recovery custody read (`fauna-onboarding-machine`'s native
//! `recovery_config` reader mints a bearer over the anonymous connection, then
//! opens this for its one-shot authed reads) is the first consumer. Like the
//! anonymous twin it carries no reconnect supervisor — a dropped connection
//! surfaces as [`AnonClientError::RpcDisconnected`] on the next request, which is
//! all a one-shot read needs.

use std::sync::Arc;

use fauna_protocol::{KindRegistry, RpcDispatcher};
use tokio::task::JoinHandle;

use crate::error::AnonClientError;

/// A one-shot authed WS-RPC connection (fixed dispatcher, no reconnect
/// supervisor). Dropping it aborts the inbound driver, closing the WS.
pub struct TokenNestClient {
    dispatcher: Arc<RpcDispatcher>,
    kind_registry: Arc<KindRegistry>,
    /// Drives the inbound stream for the connection's lifetime; aborted on drop
    /// (so dropping the client closes the WS).
    _driver: JoinHandle<()>,
}

impl TokenNestClient {
    /// Open an authenticated connection to `nest_url` (an `http(s)://` base — the
    /// scheme is swapped to `ws(s)://` internally) for `actor_id_hex`, joining
    /// `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>`. Over `wss://` the
    /// connection is pinned to the SPKI a prior handshake graduated for the host
    /// (else strict WebPKI); see [`crate::ws::connect_authed`]. Uses the default
    /// protocol-kind registry (§ 1.4 default 30 s deadline for the plain
    /// request/reply kinds a one-shot read uses).
    pub async fn connect(
        nest_url: &str,
        actor_id_hex: &str,
        token: &str,
    ) -> Result<Self, AnonClientError> {
        let adapter = crate::ws::connect_authed(nest_url, actor_id_hex, token).await?;
        let (dispatcher, driver) = RpcDispatcher::new(adapter);
        // The wire `replay_forbidden` hint comes off this registry; a fresh
        // dispatcher has none attached, so without this every request would
        // omit it (`transport.md` § Idempotency and reconnect-with-resume).
        let _ = dispatcher.set_kind_registry(KindRegistry::full());
        let driver = tokio::spawn(driver);
        Ok(Self {
            dispatcher: Arc::new(dispatcher),
            kind_registry: Arc::new(KindRegistry::full()),
            _driver: driver,
        })
    }

    /// Send an authed RPC request and await the typed reply (the shared
    /// [`crate::dispatch::request_typed`] core — one copy across both clients).
    pub async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, AnonClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        crate::dispatch::request_typed(&self.dispatcher, &self.kind_registry, kind, payload).await
    }
}

/// The native arm of the shared `RpcRequester` seam for the one-shot authed
/// connection — mirrors [`crate::AnonymousNestClient`]'s impl so any
/// `R: RpcRequester` consumer is target-generic
/// (the wasm arm is `fauna-rpc-wasm`'s `TokenWsRpcClient`). The future is `Send`
/// (the payload is encoded before the first await).
impl fauna_protocol::RpcRequester for TokenNestClient {
    type Error = AnonClientError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, AnonClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // Fully qualified so the path resolves to the inherent method, not back
        // into this trait method.
        TokenNestClient::request(self, kind, payload).await
    }
}

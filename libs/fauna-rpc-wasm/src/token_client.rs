//! `TokenWsRpcClient` — an authed **one-shot** WS-RPC connection over a
//! caller-supplied raw bearer token. The bearer twin of
//! [`crate::AnonymousWsRpcClient`]: same fixed-dispatcher, no-reconnect-supervisor
//! shape, but it joins the connection with `Sec-WebSocket-Protocol: fauna.v1,
//! bearer.<token>` ([`GlooAdapter::connect`]) instead of the anonymous
//! `fauna.v1`-only handshake, so the nest routes the full authenticated kind set
//! (not just the pre-identity allowlist).
//!
//! Unlike [`crate::WsRpcClient`] — the SPA's long-lived, reconnecting bearer
//! client built around a JS `token_provider: js_sys::Function` — this takes an
//! already-minted **raw** token and drives a single short-lived exchange. It
//! exists for Rust-side flows that need one authed round-trip without a JS
//! token-provider bridge: the box-recovery custody read
//! (`fauna-onboarding-machine`'s `recovery_config` reader mints a bearer over the
//! anonymous connection via `fauna.auth.handshake`, then opens this to run one
//! cold read) is the first consumer. Like the anonymous twin it carries
//! no reconnect supervisor — a dropped connection surfaces as
//! `WsRpcError::Disconnected` on the next request, which is all a one-shot read
//! needs.
//!
//! Single-threaded by construction (browser), so it is `Rc`-based and its
//! `RpcRequester` future is `!Send`. The encode/dispatch/decode core is the
//! shared [`crate::client::dispatch_typed`] (one copy across every transport).

use std::rc::Rc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use fauna_protocol::RpcRequester;

use crate::adapter::GlooAdapter;
use crate::error::WsRpcError;
use crate::fixed_conn::{self, Inner};

/// Cheaply cloneable handle (`Rc` inside) to a one-shot authed WS-RPC connection.
#[derive(Clone)]
pub struct TokenWsRpcClient {
    inner: Rc<Inner>,
}

impl TokenWsRpcClient {
    /// Open an authenticated connection to `nest_url` (an `http(s)://` base — the
    /// scheme is swapped to `ws(s)://` internally) for `actor_id_hex`, joining
    /// `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>`, and start driving it on
    /// `spawn_local`. Returns immediately; the browser `WebSocket` completes its
    /// handshake asynchronously (gloo buffers sends until open). No reconnect
    /// supervisor (the connection is fixed, like [`crate::AnonymousWsRpcClient`]).
    pub fn connect(nest_url: &str, actor_id_hex: &str, token: &str) -> Result<Self, WsRpcError> {
        let adapter = GlooAdapter::connect(nest_url, actor_id_hex, token)?;
        Ok(Self {
            inner: fixed_conn::drive_and_wrap(adapter),
        })
    }
}

/// The wasm arm of the shared `RpcRequester` seam for the one-shot authed
/// connection — mirrors [`crate::AnonymousWsRpcClient`]'s impl so a
/// any `R: RpcRequester` consumer is target-generic.
impl RpcRequester for TokenWsRpcClient {
    type Error = WsRpcError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, WsRpcError>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        fixed_conn::request(&self.inner, kind, payload).await
    }
}

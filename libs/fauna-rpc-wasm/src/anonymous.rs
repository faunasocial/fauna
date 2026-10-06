//! `AnonymousWsRpcClient` — the wasm twin of `fauna_anon_client::AnonymousNestClient`
//! (native). Client of the **pre-identity (anonymous) WS connection**
//! (`GET /api/v1/ws`, `Sec-WebSocket-Protocol: fauna.v1` with no bearer;
//! transport.md § Pre-identity).
//!
//! Onboarding bootstrap — public discovery, registration, the one-time admin
//! claim, invite requests, the storage-mode commit — runs *before any bearer
//! token exists*, so it can't ride the actor-keyed [`crate::WsRpcClient`]. It
//! rides this second, bearer-less connection instead. Like the native twin it
//! carries no actor, no token provider, and **no reconnect supervisor**: the
//! dispatcher is fixed at `connect`, so a dropped connection surfaces as
//! `WsRpcError::Disconnected` on the next request. The onboarding state machine
//! (`fauna-onboarding-machine`'s `WsRpcNestApi`) already retries transient nest
//! errors at the UI level, so a connector-level supervisor would be redundant
//! for a short-lived bootstrap flow — the connection is dropped once a bearer is
//! in hand and the client reopens the authenticated `WsRpcClient`.
//!
//! Single-threaded by construction (browser), so it is `Rc`-based and its
//! `RpcRequester` future is `!Send`. The encode/dispatch/decode core is the
//! shared [`crate::client::dispatch_typed`] (one copy across both transports).

use std::rc::Rc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use fauna_protocol::RpcRequester;

use crate::adapter::GlooAdapter;
use crate::error::WsRpcError;
use crate::fixed_conn::{self, Inner};

/// Cheaply cloneable handle (`Rc` inside) to the anonymous WS-RPC connection.
#[derive(Clone)]
pub struct AnonymousWsRpcClient {
    inner: Rc<Inner>,
}

impl AnonymousWsRpcClient {
    /// Open an anonymous connection to `nest_url` (an `http(s)://` base — the
    /// scheme is swapped to `ws(s)://` internally) and start driving it on
    /// `spawn_local`. Returns immediately; the browser `WebSocket` completes its
    /// handshake asynchronously (gloo buffers sends until open). Uses the
    /// protocol-kind registry; pre-identity kinds carry no special metadata, so
    /// the § 1.4 default 30 s deadline applies to all of them.
    pub fn connect(nest_url: &str) -> Result<Self, WsRpcError> {
        let adapter = GlooAdapter::connect_anonymous(nest_url)?;
        Ok(Self {
            inner: fixed_conn::drive_and_wrap(adapter),
        })
    }
}

/// The wasm arm of the shared `RpcRequester` seam for the pre-identity
/// connection — mirrors the native `AnonymousNestClient` impl so the onboarding
/// `WsRpcNestApi<R>` glue is generic over `R: RpcRequester` regardless of target.
impl RpcRequester for AnonymousWsRpcClient {
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

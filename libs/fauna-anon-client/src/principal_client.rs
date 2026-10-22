//! [`PrincipalNestClient`] — the native client of the **principal session**,
//! the token-bearing WS-RPC connection a principal of the user opens on
//! `GET /api/v1/principal/ws` (`transport-connection.md` § Connection
//! lifecycle → *The principal session*).
//!
//! The DPoP twin of [`crate::TokenNestClient`]: the same fixed-dispatcher,
//! no-reconnect-supervisor shape, but the connection is joined with a
//! DPoP-bound access token and a proof over the upgrade in place of a nest
//! bearer, because a principal holds no actor key and no bearer. Its first
//! consumer is the `fauna-cli` device shell (`apps/cli.md` § The device shell).
//!
//! **The key stays with the caller.** This crate signs nothing: the caller hands
//! in a `prove(nonce)` closure that returns a compact DPoP proof for `GET` of
//! the session's published URL with `ath` over the token. The one
//! `use_dpop_nonce` refusal the protocol defines is answered by calling it
//! again with the nonce the nest named — never a second retry, never a retry on
//! any other refusal.
//!
//! The class is re-resolved per RPC nest side, so a refused kind is an ordinary
//! `Rpc` error on that request; a revoke closes the socket `4401`, which a
//! later request reads as [`AnonClientError::RpcDisconnected`].

use std::sync::Arc;

use fauna_protocol::{KindRegistry, RpcDispatcher};
use tokio::task::JoinHandle;

use crate::error::AnonClientError;
use crate::ws::{PrincipalDialRefusal, connect_principal};

pub use crate::ws::PRINCIPAL_WS_PATH;

/// A one-shot principal-session connection (fixed dispatcher, no reconnect
/// supervisor). Dropping it aborts the inbound driver, closing the WS.
pub struct PrincipalNestClient {
    dispatcher: Arc<RpcDispatcher>,
    close: crate::ws::CloseSignal,
    kind_registry: Arc<KindRegistry>,
    _driver: JoinHandle<()>,
}

impl PrincipalNestClient {
    /// Dial the principal session at `nest_url` (an `http(s)://` base) with
    /// `access_token`. `prove(nonce)` returns the DPoP proof for the upgrade;
    /// it is called once with `None` (or the nonce the caller last held) and
    /// at most once more with the nonce a `use_dpop_nonce` refusal named.
    /// Returns the client and the nonce the second proof bound, if one was
    /// needed, so the caller can keep it for its next proof.
    pub async fn connect(
        nest_url: &str,
        access_token: &str,
        initial_nonce: Option<&str>,
        prove: impl Fn(Option<&str>) -> String,
    ) -> Result<(Self, Option<String>), AnonClientError> {
        let first = connect_principal(nest_url, access_token, &prove(initial_nonce)).await;
        let (adapter, nonce) = match first {
            Ok(adapter) => (adapter, None),
            Err(PrincipalDialRefusal::NonceRequired(nonce)) => {
                match connect_principal(nest_url, access_token, &prove(Some(&nonce))).await {
                    Ok(adapter) => (adapter, Some(nonce)),
                    Err(PrincipalDialRefusal::NonceRequired(_)) => {
                        return Err(AnonClientError::WebSocket(
                            "principal session refused the proof bound to the nonce it named"
                                .into(),
                        ));
                    }
                    Err(PrincipalDialRefusal::Other(e)) => return Err(e),
                }
            }
            Err(PrincipalDialRefusal::Other(e)) => return Err(e),
        };
        let close = adapter.close_signal();
        let (dispatcher, driver) = RpcDispatcher::new(adapter);
        let _ = dispatcher.set_kind_registry(KindRegistry::full());
        let driver = tokio::spawn(driver);
        Ok((
            Self {
                dispatcher: Arc::new(dispatcher),
                close,
                kind_registry: Arc::new(KindRegistry::full()),
                _driver: driver,
            },
            nonce,
        ))
    }

    /// Send one request and await the typed reply (the shared
    /// [`crate::dispatch::request_typed`] core).
    pub async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, AnonClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        crate::dispatch::request_typed(
            &self.dispatcher,
            &self.close,
            &self.kind_registry,
            kind,
            payload,
        )
        .await
    }
}

impl fauna_protocol::RpcRequester for PrincipalNestClient {
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
        PrincipalNestClient::request(self, kind, payload).await
    }
}

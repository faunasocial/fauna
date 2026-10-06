//! The shared typed-request core for this crate's fixed-dispatcher clients.
//!
//! Both [`AnonymousNestClient`](crate::AnonymousNestClient) (anonymous,
//! pre-identity) and [`TokenNestClient`](crate::TokenNestClient) (authed,
//! bearer) drive the *same* encode → dispatch → await → decode path over a
//! **fixed** `RpcDispatcher` (no reconnect supervisor). That path itself is
//! [`RpcDispatcher::request_typed`], shared with every other fauna transport;
//! what lives here is this crate's slice of it — the fixed-dispatcher deadline
//! lookup, the fresh idempotency key, and the mapping onto
//! [`AnonClientError`].

use fauna_protocol::{KindRegistry, RpcDispatcher};

use crate::error::AnonClientError;

/// Spec § 1.4 default request deadline for kinds carrying no metadata. The
/// pre-identity + one-shot authed reads are all plain request/reply with no
/// special timing.
pub(crate) const DEFAULT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Send one typed RPC request over a fixed dispatcher and await the typed reply:
/// encode the typed `Req` to a dag-cbor `Value`, dispatch, await the reply
/// bounded by the kind's deadline, decode the typed `Reply`.
///
/// The connection is fixed (no reconnect-wait): a closed connection surfaces as
/// the dispatcher-synthesised [`fauna_protocol::DISCONNECTED_CODE`], mapped here
/// to [`AnonClientError::RpcDisconnected`]. The `Req` is CBOR-encoded before the
/// shared path's first await, so the returned future is `Send` — native callers
/// can `tokio::spawn` it.
pub(crate) async fn request_typed<Req, Reply>(
    dispatcher: &RpcDispatcher,
    kind_registry: &KindRegistry,
    kind: &'static str,
    payload: Req,
) -> Result<Reply, AnonClientError>
where
    Req: serde::Serialize,
    Reply: serde::de::DeserializeOwned,
{
    let deadline = kind_registry
        .meta(kind)
        .map(|m| m.default_deadline)
        .unwrap_or(DEFAULT_DEADLINE);

    let mut idem = [0u8; 16];
    getrandom::fill(&mut idem)
        .map_err(|e| AnonClientError::Decode(format!("idempotency_key: {e}")))?;

    let started = std::time::Instant::now();
    let result = dispatcher
        .request_typed(kind, idem, payload, deadline, tokio::time::sleep(deadline))
        .await;
    tracing::debug!(
        kind,
        elapsed_ms = started.elapsed().as_millis() as u64,
        ok = result.is_ok(),
        "request answered"
    );
    result.map_err(|e| {
        use fauna_protocol::TypedRequestError as T;
        match e {
            T::Codec(msg) => AnonClientError::Decode(msg),
            T::Dispatch(e) => AnonClientError::WebSocket(format!("dispatch: {e}")),
            // The connection here is fixed — there is no reconnect wait
            // upstream — so a disconnect is always sent-then-dropped.
            T::Disconnected => AnonClientError::RpcDisconnected {
                was_in_flight: true,
            },
            // The nest's `details` are already logged operator-side by the
            // shared path and are never surfaced: the pre-identity path is
            // where an unreadable internal error is most expensive — the
            // user has no session yet and no other surface to look at.
            T::Rpc(e) => AnonClientError::Rpc(*e),
            T::Timeout => AnonClientError::RpcTimeout,
        }
    })
}

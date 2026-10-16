//! Shared driving logic for a **fixed, non-reconnecting** WS-RPC connection —
//! [`TokenWsRpcClient`](crate::TokenWsRpcClient) and
//! [`AnonymousWsRpcClient`](crate::AnonymousWsRpcClient) each wrapped a
//! byte-for-byte identical `Inner { kind_registry, dispatcher }` plus an
//! identical connect-drive-wrap sequence and an identical `RpcRequester::request`
//! body, differing only in how the two obtain their already-connected
//! [`GlooAdapter`]. This is the one shared body; each caller keeps only its own
//! `GlooAdapter::connect*` call and its own public type name.

use std::rc::Rc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen_futures::spawn_local;

use fauna_protocol::{KindRegistry, RpcDispatcher};

use crate::adapter::{GlooAdapter, ReconnectSignal, SignalCell};
use crate::client::{DEFAULT_DEADLINE, dispatch_typed_keyed};
use crate::error::WsRpcError;

pub(crate) struct Inner {
    kind_registry: KindRegistry,
    /// Fixed for the connection's lifetime (no reconnect loop). After the WS
    /// closes, the driver future ends and its command-channel receiver drops, so
    /// [`dispatch_typed`] here fails with `Disconnected` — no `Option`/cell
    /// needed.
    dispatcher: Rc<RpcDispatcher>,
    /// The adapter's close reading, kept after the adapter moved into the
    /// dispatcher: a request that failed on a 4426 close is the app being too
    /// old for the nest, not a blip (`transport-connection.md` § Connection
    /// lifecycle, the 4426 paragraph).
    signal: SignalCell,
}

/// Start driving an already-connected `adapter` to completion on `spawn_local`
/// and wrap it as the shared `Inner` both fixed-connection clients hold.
/// Returns immediately; the browser `WebSocket` completes its handshake
/// asynchronously (gloo buffers sends until open).
pub(crate) fn drive_and_wrap(adapter: GlooAdapter) -> Rc<Inner> {
    let signal = adapter.signal_cell();
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    // The wire `replay_forbidden` hint comes off this registry; a fresh
    // dispatcher has none attached, so without this every request would omit
    // it (`transport.md` § Idempotency and reconnect-with-resume).
    let _ = dispatcher.set_kind_registry(KindRegistry::full());
    // Drive the dispatcher to completion; when the stream ends (close/error/
    // network) the driver finishes and drops the adapter (closing the WS).
    // wasm `spawn_local` has no abort handle, so a still-open connection is
    // reclaimed when the server closes it or the flow's client is GC'd —
    // acceptable for a short-lived one-shot/bootstrap connection.
    spawn_local(driver);
    Rc::new(Inner {
        kind_registry: KindRegistry::full(),
        dispatcher: Rc::new(dispatcher),
        signal,
    })
}

/// The shared `RpcRequester::request` body for a fixed connection: no
/// reconnect-wait (the connection is fixed), so a closed connection surfaces
/// as `Disconnected` from within [`dispatch_typed`] — or as
/// [`WsRpcError::SubprotocolMismatch`] when the adapter recorded a 4426 close.
pub(crate) async fn request<Req, Reply>(
    inner: &Inner,
    kind: &'static str,
    payload: Req,
) -> Result<Reply, WsRpcError>
where
    Req: Serialize,
    Reply: DeserializeOwned,
{
    let mut idem = [0u8; 16];
    getrandom::fill(&mut idem).map_err(|e| WsRpcError::Codec(format!("idempotency_key: {e}")))?;
    request_keyed(inner, kind, idem, payload).await
}

/// [`request`] with a caller-supplied envelope idempotency key — the
/// keyed-requester seam ([`fauna_protocol::KeyedRpcRequester`]) for a fixed
/// connection, so an account runtime can drive its outbox over one.
pub(crate) async fn request_keyed<Req, Reply>(
    inner: &Inner,
    kind: &'static str,
    idempotency_key: [u8; 16],
    payload: Req,
) -> Result<Reply, WsRpcError>
where
    Req: Serialize,
    Reply: DeserializeOwned,
{
    let deadline = inner
        .kind_registry
        .meta(kind)
        .map(|m| m.default_deadline)
        .unwrap_or(DEFAULT_DEADLINE);
    let budget_ms = deadline.as_millis() as u32;
    match dispatch_typed_keyed(&inner.dispatcher, kind, idempotency_key, payload, budget_ms).await {
        Err(WsRpcError::Disconnected)
            if matches!(
                *inner.signal.borrow(),
                Some(ReconnectSignal::SubprotocolMismatch)
            ) =>
        {
            Err(WsRpcError::SubprotocolMismatch)
        }
        other => other,
    }
}

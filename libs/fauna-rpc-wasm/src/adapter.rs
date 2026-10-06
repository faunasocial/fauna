//! L1+L2 adapter: gloo-net browser `WebSocket` → Bytes-shaped Stream+Sink.
//!
//! The wasm twin of `fauna_client::ws_adapter` (which wraps tungstenite). The
//! shape `RpcDispatcher::new` consumes is identical — `Stream<Item =
//! Result<Bytes, _>> + Sink<Bytes> + Unpin` — so the only difference from
//! native is the substrate (browser `WebSocket` vs TLS-tungstenite) and that
//! this side is `!Send` (single-threaded wasm), which the runtime-agnostic
//! dispatcher (Phase 1) now allows.
//!
//! Close handling: gloo yields the close as `Some(Err(ConnectionClose(e)))`
//! then `None`. We capture `e.code` into a shared `SignalCell` (read by the
//! reconnect loop after the dispatcher's driver future finishes — the adapter
//! itself is owned by the driver by then) and end the stream, mirroring the
//! native `BoxedAdapter` side-channel.

use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{Sink, Stream};
use gloo_net::websocket::Message;
use gloo_net::websocket::futures::WebSocket;

use crate::error::WsRpcError;

/// What the adapter saw when the WebSocket terminated — the reconnect loop's
/// reaction table. Mirrors `fauna_client::ws_adapter::ReconnectSignal` (the
/// close-code table is `transport.md` § Connection lifecycle); kept as a small
/// local copy because that type lives in the native-only `fauna-client`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectSignal {
    /// 1000 Normal — stop the reconnect loop.
    CleanDisconnect,
    /// 4401 — refresh the bearer (force) then reconnect.
    AuthExpired,
    /// 4426 — subprotocol/version skew; stop.
    SubprotocolMismatch,
    /// Everything else (1001/1006/1011/4400/network) — backoff + reconnect.
    Retry,
}

impl ReconnectSignal {
    fn from_code(code: u16) -> Self {
        match code {
            1000 => Self::CleanDisconnect,
            4401 => Self::AuthExpired,
            4426 => Self::SubprotocolMismatch,
            _ => Self::Retry,
        }
    }
}

/// Side-channel the reconnect loop clones before handing the adapter to
/// `RpcDispatcher::new`, then reads after the driver future ends.
pub type SignalCell = Rc<RefCell<Option<ReconnectSignal>>>;

/// Error type for the adapter's Stream/Sink. The Stream never yields it (close
/// and error events end the stream via `None` + a captured `SignalCell`); the
/// Sink yields it on a failed browser `send`.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("ws transport: {0}")]
    Transport(String),
}

/// Build the `wss|ws` URL — `{base}/api/v1/ws/{actor_id}`, no `?token=`
/// (Spec Y retired the query form). Mirrors `fauna_client::ws_adapter`.
pub(crate) fn build_ws_url(nest_url: &str, actor_id_hex: &str) -> String {
    let base = fauna_core::web::http_to_ws(nest_url);
    format!("{base}/api/v1/ws/{actor_id_hex}")
}

/// Build the `wss|ws` URL for the **anonymous** (pre-identity) endpoint:
/// `{base}/api/v1/ws` — no `{actor_id}` segment (an anonymous connection has no
/// proven actor to key on). Mirrors `fauna_client::ws_adapter::build_anon_ws_url`.
pub(crate) fn build_anon_ws_url(nest_url: &str) -> String {
    let base = fauna_core::web::http_to_ws(nest_url);
    format!("{base}/api/v1/ws")
}

/// Bytes-shaped Stream+Sink over a gloo-net browser `WebSocket`.
pub struct GlooAdapter {
    ws: WebSocket,
    signal_cell: SignalCell,
}

impl GlooAdapter {
    /// Open the WS-RPC connection with the subprotocol-bearer handshake:
    /// `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` (the browser joins
    /// the offered protocols with `, `; nest echoes `fauna.v1`). Per
    /// `transport.md` § Connection lifecycle.
    pub fn connect(nest_url: &str, actor_id_hex: &str, token: &str) -> Result<Self, WsRpcError> {
        let url = build_ws_url(nest_url, actor_id_hex);
        let protocols = ["fauna.v1".to_string(), format!("bearer.{token}")];
        let ws = WebSocket::open_with_protocols(&url, &protocols)
            .map_err(|e| WsRpcError::Connect(e.to_string()))?;
        Ok(Self {
            ws,
            signal_cell: Rc::new(RefCell::new(None)),
        })
    }

    /// Open the **anonymous** (pre-identity) connection: `{base}/api/v1/ws` with
    /// `Sec-WebSocket-Protocol: fauna.v1` and **no** `bearer.<token>` element.
    /// The connection routes only the fixed pre-identity allowlist; an
    /// off-allowlist kind gets `fauna.protocol.unauthenticated` without tearing
    /// the connection down (transport.md § Pre-identity). The bearer twin is
    /// [`connect`](Self::connect).
    pub fn connect_anonymous(nest_url: &str) -> Result<Self, WsRpcError> {
        let url = build_anon_ws_url(nest_url);
        let protocols = ["fauna.v1".to_string()];
        let ws = WebSocket::open_with_protocols(&url, &protocols)
            .map_err(|e| WsRpcError::Connect(e.to_string()))?;
        Ok(Self {
            ws,
            signal_cell: Rc::new(RefCell::new(None)),
        })
    }

    /// Clone the shared close-signal cell. The reconnect loop holds this after
    /// the adapter is moved into the dispatcher.
    pub fn signal_cell(&self) -> SignalCell {
        Rc::clone(&self.signal_cell)
    }

    fn record(&self, signal: ReconnectSignal) {
        *self.signal_cell.borrow_mut() = Some(signal);
    }
}

impl Stream for GlooAdapter {
    type Item = Result<Bytes, AdapterError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use gloo_net::websocket::WebSocketError as WsErr;
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.ws).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Bytes(b)))) => {
                    return Poll::Ready(Some(Ok(Bytes::from(b))));
                }
                // Text frames are not part of the WS-RPC wire; skip them so the
                // stream is robust against a non-conforming peer.
                Poll::Ready(Some(Ok(Message::Text(_)))) => continue,
                // Close: capture the close-code-derived signal and end the
                // stream so the dispatcher driver runs its cleanup.
                Poll::Ready(Some(Err(WsErr::ConnectionClose(e)))) => {
                    this.record(ReconnectSignal::from_code(e.code));
                    return Poll::Ready(None);
                }
                // A raw error event (no close frame) — `ConnectionError`,
                // `MessageSendError`, or any future variant (the enum is
                // `#[non_exhaustive]`). Treat as a retryable transport drop.
                Poll::Ready(Some(Err(_other))) => {
                    this.record(ReconnectSignal::Retry);
                    return Poll::Ready(None);
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Sink<Bytes> for GlooAdapter {
    type Error = AdapterError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .poll_ready(cx)
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .start_send(Message::Bytes(item.to_vec()))
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .poll_flush(cx)
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .poll_close(cx)
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    // Plain `#[test]` never runs here: the whole crate is wasm32-only (see
    // `lib.rs`), and even under `wasm-pack test` the standard libtest harness
    // isn't wired up for wasm32 — only `#[wasm_bindgen_test]`-annotated fns are
    // collected by wasm-bindgen-test-runner. Before this fix these 3 assertions
    // silently never ran under any invocation (`cargo test` on native: empty
    // crate; `wasm-pack test --headless --firefox`: "no tests to run!"). This
    // repo's web tooling is Deno-only, with no Node.js install, so
    // `run_in_browser` is required — without it wasm-bindgen-test-runner
    // defaults to a Node target this machine can't run and silently skips
    // every test.
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn build_ws_url_swaps_scheme_and_appends_actor() {
        assert_eq!(
            build_ws_url("https://nest.example.com", "abcd"),
            "wss://nest.example.com/api/v1/ws/abcd"
        );
        assert_eq!(
            build_ws_url("http://localhost:8080", "ff"),
            "ws://localhost:8080/api/v1/ws/ff"
        );
    }

    #[wasm_bindgen_test]
    fn build_anon_ws_url_swaps_scheme_and_omits_actor() {
        assert_eq!(
            build_anon_ws_url("https://nest.example.com"),
            "wss://nest.example.com/api/v1/ws"
        );
        assert_eq!(
            build_anon_ws_url("http://localhost:8080"),
            "ws://localhost:8080/api/v1/ws"
        );
    }

    #[wasm_bindgen_test]
    fn close_codes_map_to_signals() {
        assert_eq!(
            ReconnectSignal::from_code(1000),
            ReconnectSignal::CleanDisconnect
        );
        assert_eq!(
            ReconnectSignal::from_code(4401),
            ReconnectSignal::AuthExpired
        );
        assert_eq!(
            ReconnectSignal::from_code(4426),
            ReconnectSignal::SubprotocolMismatch
        );
        assert_eq!(ReconnectSignal::from_code(1011), ReconnectSignal::Retry);
    }
}

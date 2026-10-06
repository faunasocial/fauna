//! Substrate-neutral native WS-RPC transport.
//!
//! This crate holds the parts of the WS-RPC client loop that carry **no**
//! client-specific (bearer / actor) coupling, so they can be shared by every
//! native consumer of one long-lived WebSocket carrying [`fauna_protocol`] L3
//! frames:
//!
//! - the **bearer client channel** (`fauna-client`, one WS per actor), and
//! - the **nest↔nest federation channel** (`bins/fauna-nest`, one WS per peer
//!   nest — Spec Y2 slice 4).
//!
//! Two layers live here:
//!
//! 1. [`adapter`] — the tungstenite `WebSocketStream` ⇄ `Bytes`
//!    [`Stream`](futures_util::Stream) + [`Sink`](futures_util::Sink) glue,
//!    plus the heartbeat (30 s Ping / 60 s dead-link detection) and the
//!    close-code → [`ReconnectSignal`] mapping.
//! 2. [`supervisor`] — the reconnect/backoff loop ([`run_supervisor`]) that
//!    drives the adapter, hands it to [`fauna_protocol::RpcDispatcher`], parks
//!    the dispatcher in a slot, and reacts to the close signal. It is
//!    **parameterised over the auth handshake** via [`SupervisedChannel`]: the
//!    client supplies the bearer-subprotocol connect + push bridge; the
//!    federation channel supplies the `fauna.federation.hello` handshake +
//!    inbound serving.
//!
//! A third, narrower module, [`handshake`], holds only the handful of pieces
//! of the *bearer*-subprotocol connect step that are byte-identical across
//! every caller that speaks it (URL, `Sec-WebSocket-Protocol` header, size
//! cap) — the rest of the connect step (TLS pinning, the trust-graduation
//! policy) still stays with each caller, per [`adapter`]'s own doc comment.
//!
//! This crate is **native-only** (tokio + tokio-tungstenite). The web (wasm)
//! client keeps its own `gloo`-based twin in `fauna-rpc-wasm`; the browser
//! `WebSocket` is `!Send` and has no Ping primitive, so it cannot share this
//! substrate — that divergence is intrinsic to the platform, not drift.

pub mod adapter;
pub mod dial_budget;
pub mod handshake;
pub mod supervisor;
pub mod tls_verify;

#[cfg(any(test, feature = "test-util"))]
pub mod testing;

pub use adapter::{
    AdapterError, FrameAction, HeartbeatDriver, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT,
    ReconnectSignal, TungsteniteAdapter, TungsteniteFrames, WsFrames, ensure_tls_provider,
    poll_ws_frames,
};
pub use handshake::{actor_ws_url, bearer_subprotocol_header, rpc_ws_config};
pub use supervisor::{
    ConnectedAdapter, ConnectionState, DialDemand, DialDemandGuard, SupervisedChannel, Supervisor,
    SupervisorError, run_supervisor,
};
pub use tls_verify::{
    CaptureHandle, CapturedCert, NoPinPolicy, PinResolver, capturing_client_config,
    dynamic_pinned_client_config, spki_pinned_client_config,
};

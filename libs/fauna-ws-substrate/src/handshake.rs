//! Shared pieces of the bearer-subprotocol WS connect step.
//!
//! [`adapter`](crate::adapter) deliberately excludes the *connect* step (URL,
//! auth handshake, TLS pinning) — each caller's trust model differs too much
//! to share wholesale (`fauna-client`'s bearer channel graduate-and-retries a
//! pin; `fauna-anon-client`'s authenticated connect requires an
//! already-graduated pin or falls to strict WebPKI). But three sub-pieces of
//! the bearer handshake specifically are byte-identical across every caller
//! that speaks it: the `/api/v1/ws/{actor_id}` URL, the
//! `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` header, and the
//! inbound-size-capped [`WebSocketConfig`] (the last one shared by the
//! anonymous connect too). Lifted here so the copies can't drift silently
//! (found by the containment arm of the dev-fleet near-duplicate-function
//! scanner).

use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// The wss/ws URL for the bearer-authenticated endpoint
/// (`{base}/api/v1/ws/{actor_id_hex}`); owned by `fauna_core::transport`.
pub use fauna_core::transport::actor_ws_url;

/// Builds the `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` header value
/// for the bearer-subprotocol handshake.
pub fn bearer_subprotocol_header(token: &str) -> Result<HeaderValue, InvalidHeaderValue> {
    HeaderValue::from_str(&format!("fauna.v1, bearer.{token}"))
}

/// The inbound message/frame size cap every native WS-RPC connect applies,
/// symmetric with the nest's `MAX_RPC_WS_MESSAGE_SIZE` client-WS-RPC limit
/// (review finding F-CL1). Without it tungstenite defaults to 64 MiB / 16
/// MiB, letting a malicious or compromised nest force unbounded buffering —
/// a legitimate RPC frame never approaches 2 MiB (bulk transfer rides
/// separate HTTP routes).
pub fn rpc_ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE))
        .max_frame_size(Some(fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_subprotocol_header_carries_the_token() {
        let h = bearer_subprotocol_header("tok").unwrap();
        assert_eq!(h.to_str().unwrap(), "fauna.v1, bearer.tok");
    }

    #[test]
    fn bearer_subprotocol_header_rejects_a_control_byte() {
        assert!(bearer_subprotocol_header("tok\r\nEvil: header").is_err());
    }
}

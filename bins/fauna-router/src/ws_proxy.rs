//! WebSocket reverse proxy — bridges a client WebSocket to a backend WebSocket.
//!
//! `bridge_websocket` connects to the backend at `ws://tunnel_ip:port{path}`,
//! then shuttles frames bidirectionally until either side closes.

use axum::extract::ws::{
    CloseFrame as AxumCloseFrame, Message as AxumMessage, Utf8Bytes, WebSocket,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TungMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode as TungCloseCode;
use tracing::{debug, warn};

use crate::backends::BackendState;

// ── Public entry point ────────────────────────────────────────────────────────

/// Bridge a client WebSocket to the matching backend WebSocket.
///
/// Connects to `ws://{tunnel_ip}:{port}{path}`, then enters a
/// `tokio::select!` loop that copies frames in both directions until one
/// side closes or an error occurs.
pub async fn bridge_websocket(client_ws: WebSocket, backend: &BackendState, path: &str) {
    let backend_url = format!("ws://{}:{}{}", backend.tunnel_ip, backend.port, path);
    debug!("WS bridge: connecting to backend {}", backend_url);

    let backend_stream = match tokio_tungstenite::connect_async(&backend_url).await {
        Ok((stream, _)) => stream,
        Err(e) => {
            warn!(
                "WS bridge: failed to connect to backend {}: {}",
                backend_url, e
            );
            return;
        }
    };

    let (mut client_tx, mut client_rx) = client_ws.split();
    let (mut backend_tx, mut backend_rx) = backend_stream.split();

    loop {
        tokio::select! {
            // Client → Backend
            client_msg = client_rx.next() => {
                match client_msg {
                    Some(Ok(msg)) => {
                        if matches!(msg, AxumMessage::Close(_)) {
                            let tung_msg = axum_to_tungstenite(msg);
                            let _ = backend_tx.send(tung_msg).await;
                            break;
                        }
                        let tung_msg = axum_to_tungstenite(msg);
                        if let Err(e) = backend_tx.send(tung_msg).await {
                            debug!("WS bridge: backend send error: {}", e);
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        debug!("WS bridge: client recv error: {}", e);
                        break;
                    }
                    None => {
                        // Client closed the connection.
                        debug!("WS bridge: client stream ended");
                        break;
                    }
                }
            }

            // Backend → Client
            backend_msg = backend_rx.next() => {
                match backend_msg {
                    Some(Ok(msg)) => {
                        // Skip raw Frame messages — not meaningful to forward.
                        if matches!(msg, TungMessage::Frame(_)) {
                            continue;
                        }
                        let is_close = matches!(msg, TungMessage::Close(_));
                        let axum_msg = tungstenite_to_axum(msg);
                        if let Err(e) = client_tx.send(axum_msg).await {
                            debug!("WS bridge: client send error: {}", e);
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        debug!("WS bridge: backend recv error: {}", e);
                        break;
                    }
                    None => {
                        // Backend closed the connection.
                        debug!("WS bridge: backend stream ended");
                        break;
                    }
                }
            }
        }
    }

    // Best-effort graceful close of both sides.
    let _ = backend_tx.close().await;
    let _ = client_tx.close().await;
}

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Convert an axum WebSocket `Message` to a tungstenite `Message`.
pub fn axum_to_tungstenite(msg: AxumMessage) -> TungMessage {
    use tokio_tungstenite::tungstenite::protocol::frame::Utf8Bytes as TungUtf8Bytes;

    match msg {
        AxumMessage::Text(text) => {
            // axum Utf8Bytes → String → tungstenite Utf8Bytes
            TungMessage::Text(TungUtf8Bytes::from(text.to_string()))
        }
        AxumMessage::Binary(data) => TungMessage::Binary(data),
        AxumMessage::Ping(data) => TungMessage::Ping(data),
        AxumMessage::Pong(data) => TungMessage::Pong(data),
        AxumMessage::Close(Some(frame)) => TungMessage::Close(Some(TungCloseFrame {
            code: TungCloseCode::from(frame.code),
            reason: TungUtf8Bytes::from(frame.reason.to_string()),
        })),
        AxumMessage::Close(None) => TungMessage::Close(None),
    }
}

/// Convert a tungstenite `Message` to an axum WebSocket `Message`.
///
/// `TungMessage::Frame` is not representable in axum — callers should skip it.
pub fn tungstenite_to_axum(msg: TungMessage) -> AxumMessage {
    match msg {
        TungMessage::Text(text) => AxumMessage::Text(Utf8Bytes::from(text.to_string())),
        TungMessage::Binary(data) => AxumMessage::Binary(data),
        TungMessage::Ping(data) => AxumMessage::Ping(data),
        TungMessage::Pong(data) => AxumMessage::Pong(data),
        TungMessage::Close(Some(frame)) => AxumMessage::Close(Some(AxumCloseFrame {
            code: u16::from(frame.code),
            reason: Utf8Bytes::from(frame.reason.to_string()),
        })),
        TungMessage::Close(None) => AxumMessage::Close(None),
        TungMessage::Frame(_) => {
            // Raw frames are not expected here — return a close to signal an error.
            AxumMessage::Close(None)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn round_trip_text() {
        let original = AxumMessage::Text(Utf8Bytes::from("hello world"));
        let tung = axum_to_tungstenite(original.clone());
        let back = tungstenite_to_axum(tung);
        assert_eq!(original, back);
    }

    #[test]
    fn round_trip_binary() {
        let original = AxumMessage::Binary(Bytes::from_static(b"\x00\x01\x02"));
        let tung = axum_to_tungstenite(original.clone());
        let back = tungstenite_to_axum(tung);
        assert_eq!(original, back);
    }

    #[test]
    fn round_trip_ping() {
        let original = AxumMessage::Ping(Bytes::from_static(b"ping!"));
        let tung = axum_to_tungstenite(original.clone());
        let back = tungstenite_to_axum(tung);
        assert_eq!(original, back);
    }

    #[test]
    fn round_trip_pong() {
        let original = AxumMessage::Pong(Bytes::from_static(b"pong!"));
        let tung = axum_to_tungstenite(original.clone());
        let back = tungstenite_to_axum(tung);
        assert_eq!(original, back);
    }

    #[test]
    fn round_trip_close_with_frame() {
        let original = AxumMessage::Close(Some(AxumCloseFrame {
            code: 1000,
            reason: Utf8Bytes::from("normal closure"),
        }));
        let tung = axum_to_tungstenite(original.clone());
        let back = tungstenite_to_axum(tung);
        assert_eq!(original, back);
    }

    #[test]
    fn round_trip_close_none() {
        let original = AxumMessage::Close(None);
        let tung = axum_to_tungstenite(original.clone());
        let back = tungstenite_to_axum(tung);
        assert_eq!(original, back);
    }
}

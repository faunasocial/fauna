//! The browser half of the mail-import tunnel's socket: one WebSocket to the
//! nest's `GET /api/v1/mail/import/tunnel` route, carrying binary frames
//! (`mailbox-migration.md` § Where the IMAP client runs → *The web tunnel*).
//!
//! This is only the socket. The open exchange, the TLS that ends inside the
//! client and the IMAP or HTTP above it are the shared
//! `fauna_mail::imap_client::tunnel`'s, which reaches this type through the
//! `TunnelPipe` seam `fauna-client-mail-settings` implements over it — so
//! nothing here reads, or could read, what the frames carry.

use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::{Message, WebSocketError};

use fauna_protocol::mail_import_tunnel::{ROUTE, SUBPROTOCOL};

/// The normal-closure code: the far end's EOF, or the tunnel's idle bound.
const CLOSE_NORMAL: u16 = 1000;

/// One opened tunnel WebSocket.
pub struct MailTunnelSocket {
    ws: WebSocket,
}

impl MailTunnelSocket {
    /// Open the route on `nest_url` (an `http(s)://` base; the scheme is
    /// swapped to `ws(s)://`) under the tunnel's subprotocol, with the
    /// signed-in actor's bearer as the second protocol token — the WS-RPC
    /// upgrade's shape. Returns at once; the browser completes the handshake
    /// before the first send goes out.
    pub fn open(nest_url: &str, bearer: &str) -> Result<Self, String> {
        let url = format!("{}{ROUTE}", fauna_core::web::http_to_ws(nest_url));
        let protocols = [SUBPROTOCOL.to_string(), format!("bearer.{bearer}")];
        WebSocket::open_with_protocols(&url, &protocols)
            .map(|ws| Self { ws })
            .map_err(|e| e.to_string())
    }

    /// Send one binary frame.
    pub async fn send(&mut self, frame: Vec<u8>) -> Result<(), String> {
        self.ws
            .send(Message::Bytes(frame))
            .await
            .map_err(|e| e.to_string())
    }

    /// The next binary frame; `Ok(None)` at a normal close (the far end's EOF
    /// or the idle bound). Any other close names its code and the tunnel's
    /// reason text — a browser sees no HTTP status of a failed upgrade, so an
    /// older nest without the route, a refused bearer and a dead network all
    /// arrive here as an error.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, String> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Bytes(b))) => return Ok(Some(b)),
                // Not part of the tunnel's wire.
                Some(Ok(Message::Text(_))) => continue,
                Some(Err(WebSocketError::ConnectionClose(e))) if e.code == CLOSE_NORMAL => {
                    return Ok(None);
                }
                Some(Err(WebSocketError::ConnectionClose(e))) => {
                    return Err(format!("tunnel closed ({}: {})", e.code, e.reason));
                }
                Some(Err(e)) => return Err(e.to_string()),
                None => return Ok(None),
            }
        }
    }
}

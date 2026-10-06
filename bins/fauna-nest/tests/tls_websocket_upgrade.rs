//! Regression guard for the "rpc disconnected" production blocker: a WebSocket
//! upgrade served over the nest's **TLS** listener (`fauna_nest::serve_tls`)
//! must survive and carry frames. Before the fix, `serve_tls` used
//! `auto::Builder::serve_connection` (no upgrades) — over TLS the 101 was
//! written but the upgraded byte-stream was never driven, so the socket died
//! the instant after the handshake. Every client↔nest path is WS-RPC, so on any
//! HTTPS deployment (example.com / any real VPS) the client connected, the WS
//! dropped immediately, and the UI showed
//! `rpc disconnected (was_in_flight=false)`. The plain-HTTP path
//! (`axum::serve`) handles upgrades, so the e2e suite (plain HTTP) never caught
//! it — hence this TLS-specific test.
//!
//! The fix is `serve_connection_with_upgrades`. This test self-signs a cert,
//! serves a WS echo route through the real `serve_tls`, and asserts a
//! frame round-trips over `wss://`. With the old code the upgrade closes before
//! the echo arrives and the assertion fails.

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio_tungstenite::tungstenite::Message;

/// Echoes every frame back (version-agnostic — re-sends whatever arrived).
async fn ws_echo(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(msg)) = socket.recv().await {
            if socket.send(msg).await.is_err() {
                break;
            }
        }
    })
}

#[tokio::test]
async fn websocket_upgrade_survives_over_tls() {
    // rustls 0.23 needs a process-default crypto provider for the builders.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // ── Self-signed cert for `localhost` ──────────────────────────────────
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    // ── Server: a WS echo route served through the real `serve_tls` ───────
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("server tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

    let app = Router::new().route("/ws", get(ws_echo));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        // Loopback client → bounded by the loopback ceiling (1024), so any admin cap works.
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));

    // ── Client: trust the self-signed cert, open the TLS stream, WS over it ─
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).expect("trust self-signed cert");
    let client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let tls = connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .expect("TLS handshake");

    // `client_async` over the established TLS stream — no tungstenite TLS
    // feature needed. Before the fix this upgrades (101) then the connection is
    // closed, so the echo recv below times out / errors.
    let (mut ws, _resp) = tokio_tungstenite::client_async("wss://localhost/ws", tls)
        .await
        .expect("WS handshake over TLS");

    ws.send(Message::Text("ping-over-tls".into()))
        .await
        .expect("send WS frame over TLS");

    let echoed = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("the TLS WebSocket must stay open long enough to echo (the blocker: it closed right after the 101)")
        .expect("a frame, not end-of-stream")
        .expect("a frame, not a WS error");

    assert_eq!(
        echoed.into_text().unwrap().as_str(),
        "ping-over-tls",
        "the echo frame must round-trip over the TLS WebSocket",
    );
}

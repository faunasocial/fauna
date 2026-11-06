//! The web mail-import tunnel through a real nest
//! (`mailbox-migration.md` § Where the IMAP client runs → *The web tunnel*):
//! a TLS session carried end to end to a loopback TLS echo, the destination
//! refusals, the TLS-shape refusal, the per-actor bounds, and the revocation
//! teardown reaching a live tunnel (`transport-connection.md` § *Revocation
//! teardown*).
//!
//! Tier: tier_3 in-process (real `AppState`, real axum server, real
//! WebSocket and TCP on loopback; no app). The loopback destinations are
//! admitted through `TunnelState::with_loopback_admission`, the seam a
//! `test-hooks` nest fills from the e2e seeds; every refusal test runs
//! without it, against the production policy.

use std::io::{Read, Write};
use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::mail_import_tunnel::{
    LoopbackAdmission, MAX_TUNNELS_PER_ACTOR, OPEN_LIMITER_CONFIG, TunnelState,
};
use fauna_nest::routes::AppState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::codec::{decode_strict, encode_canonical};
use fauna_protocol::mail_import_tunnel::{
    ROUTE, SUBPROTOCOL, TunnelAnswer, TunnelOpen, TunnelRefusal,
};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Nest {
    addr: std::net::SocketAddr,
    token: String,
    state: Arc<AppState>,
    actor: [u8; 32],
}

async fn start(tunnel: TunnelState) -> Nest {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(TokenStore::new()),
            ..Default::default()
        },
        mail_import_tunnel: tunnel,
        ..AppState::for_test(db)
    });
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user_with_handle(&kp.actor_id().0, "personal", "importer", None)
        .await
        .unwrap();
    let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;
    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Nest {
        addr,
        token,
        state,
        actor: kp.actor_id().0,
    }
}

async fn upgrade(nest: &Nest, token: &str) -> Result<Ws, u16> {
    let mut req = format!("ws://{}{ROUTE}", nest.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("{SUBPROTOCOL}, bearer.{token}").parse().unwrap(),
    );
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(e) => panic!("the upgrade failed below HTTP: {e}"),
    }
}

/// Open a tunnel to `host:port`; answer the nest's first frame and the socket.
async fn open(nest: &Nest, host: &str, port: u16, starttls: bool) -> (TunnelAnswer, Ws) {
    let mut ws = upgrade(nest, &nest.token)
        .await
        .expect("a valid bearer upgrades");
    let open = TunnelOpen {
        host: host.into(),
        port,
        starttls,
        ..Default::default()
    };
    ws.send(Message::binary(encode_canonical(&open).unwrap().to_vec()))
        .await
        .unwrap();
    let answer = match ws.next().await.unwrap().unwrap() {
        Message::Binary(b) => decode_strict::<TunnelAnswer>(&b).unwrap(),
        other => panic!("the first frame is the answer, got {other:?}"),
    };
    (answer, ws)
}

/// The close frame's code and reason, skipping data frames.
async fn close_of(ws: &mut Ws) -> (u16, String) {
    while let Some(msg) = ws.next().await {
        if let Message::Close(Some(frame)) = msg.unwrap() {
            return (u16::from(frame.code), frame.reason.to_string());
        }
    }
    panic!("the tunnel ended without a close frame");
}

fn refused(reason: TunnelRefusal) -> TunnelAnswer {
    TunnelAnswer::Refused { reason }
}

/// A loopback TLS server that echoes every byte; its port and its cert.
async fn tls_echo() -> (u16, fauna_mail::imap_client::test_fixtures::TestCert) {
    let cert = fauna_mail::imap_client::test_fixtures::mint_cert();
    let acceptor = fauna_mail::imap_client::test_fixtures::tls_acceptor(&cert);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = [0u8; 1024];
                while let Ok(n) = tls.read(&mut buf).await {
                    if n == 0 || tls.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                    let _ = tls.flush().await;
                }
            });
        }
    });
    (port, cert)
}

/// A loopback TCP server that accepts and holds connections, reading nothing.
async fn sink() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            held.push(tcp);
        }
    });
    port
}

fn admitting(port: u16) -> TunnelState {
    TunnelState::with_loopback_admission(LoopbackAdmission {
        host_map: Vec::new(),
        ports: vec![port],
    })
}

/// Push everything the TLS client wants to write into the tunnel.
async fn flush(ws: &mut Ws, conn: &mut rustls::ClientConnection) {
    while conn.wants_write() {
        let mut out = Vec::new();
        conn.write_tls(&mut out).unwrap();
        ws.send(Message::binary(out)).await.unwrap();
    }
}

/// Read one tunnel frame into the TLS client.
async fn pull(ws: &mut Ws, conn: &mut rustls::ClientConnection) {
    match ws.next().await.unwrap().unwrap() {
        Message::Binary(b) => {
            let mut rd = &b[..];
            while !rd.is_empty() {
                conn.read_tls(&mut rd).unwrap();
            }
            conn.process_new_packets().unwrap();
        }
        other => panic!("expected tunnel bytes, got {other:?}"),
    }
}

#[tokio::test]
async fn a_tls_session_runs_end_to_end_through_the_tunnel() {
    let (port, cert) = tls_echo().await;
    let nest = start(admitting(port)).await;
    let (answer, mut ws) = open(&nest, "localhost", port, false).await;
    assert_eq!(answer, TunnelAnswer::Opened {});

    // TLS ends in the client: the sans-io shape the wasm transport takes.
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(cert.cert_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut conn =
        rustls::ClientConnection::new(Arc::new(config), "localhost".try_into().unwrap()).unwrap();
    while conn.is_handshaking() {
        flush(&mut ws, &mut conn).await;
        if conn.is_handshaking() {
            pull(&mut ws, &mut conn).await;
        }
    }
    flush(&mut ws, &mut conn).await;

    conn.writer().write_all(b"a1 NOOP\r\n").unwrap();
    flush(&mut ws, &mut conn).await;
    let mut got = Vec::new();
    while got.len() < 9 {
        pull(&mut ws, &mut conn).await;
        let mut buf = [0u8; 64];
        match conn.reader().read(&mut buf) {
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(got, b"a1 NOOP\r\n");
}

#[tokio::test]
async fn an_address_that_is_not_global_is_refused() {
    let nest = start(TunnelState::new()).await;
    for host in ["10.0.0.1", "127.0.0.1", "169.254.169.254", "localhost"] {
        let (answer, mut ws) = open(&nest, host, 993, false).await;
        assert_eq!(answer, refused(TunnelRefusal::Destination), "{host}");
        assert_eq!(close_of(&mut ws).await, (1008, "destination".into()));
    }
}

#[tokio::test]
async fn a_port_other_than_993_143_or_443_is_refused() {
    let nest = start(TunnelState::new()).await;
    for (port, starttls) in [(25, false), (22, false), (993, true), (143, false)] {
        let (answer, mut ws) = open(&nest, "imap.example.org", port, starttls).await;
        assert_eq!(answer, refused(TunnelRefusal::Port), "{port}");
        assert_eq!(close_of(&mut ws).await, (1008, "port".into()));
    }
}

#[tokio::test]
async fn https_to_any_host_but_the_microsoft_authority_is_refused() {
    let nest = start(TunnelState::new()).await;
    let (answer, _ws) = open(&nest, "example.org", 443, false).await;
    assert_eq!(answer, refused(TunnelRefusal::Destination));
}

#[tokio::test]
async fn plaintext_first_bytes_close_the_tunnel() {
    let port = sink().await;
    let nest = start(admitting(port)).await;
    let (answer, mut ws) = open(&nest, "127.0.0.1", port, false).await;
    assert_eq!(answer, TunnelAnswer::Opened {});
    ws.send(Message::binary(b"EHLO relay.example\r\n".to_vec()))
        .await
        .unwrap();
    assert_eq!(close_of(&mut ws).await, (1008, "not_tls".into()));
}

#[tokio::test]
async fn an_actor_holds_at_most_four_tunnels() {
    let port = sink().await;
    let nest = start(admitting(port)).await;
    let mut held = Vec::new();
    for _ in 0..MAX_TUNNELS_PER_ACTOR {
        let (answer, ws) = open(&nest, "localhost", port, false).await;
        assert_eq!(answer, TunnelAnswer::Opened {});
        held.push(ws);
    }
    let (answer, _ws) = open(&nest, "localhost", port, false).await;
    assert_eq!(answer, refused(TunnelRefusal::Busy));

    // A closed tunnel frees its slot.
    let mut first = held.remove(0);
    first.close(None).await.unwrap();
    while first.next().await.is_some() {}
    let mut reopened = false;
    for _ in 0..50 {
        let (answer, _ws) = open(&nest, "localhost", port, false).await;
        if answer == (TunnelAnswer::Opened {}) {
            reopened = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(reopened, "the closed tunnel's slot never came back");
}

#[tokio::test]
async fn an_actors_opens_are_metered() {
    let nest = start(TunnelState::new()).await;
    for i in 0..OPEN_LIMITER_CONFIG.max_events {
        let (answer, _ws) = open(&nest, "imap.example.org", 25, false).await;
        assert_eq!(
            answer,
            refused(TunnelRefusal::Port),
            "open {i} spends, refused or not"
        );
    }
    let (answer, mut ws) = open(&nest, "imap.example.org", 993, false).await;
    assert_eq!(answer, refused(TunnelRefusal::RateLimited));
    assert_eq!(close_of(&mut ws).await, (1008, "rate_limited".into()));
}

#[tokio::test]
async fn a_bad_bearer_or_subprotocol_opens_nothing() {
    let nest = start(TunnelState::new()).await;
    assert_eq!(upgrade(&nest, "not-a-token").await.err(), Some(401));

    let mut req = format!("ws://{}{ROUTE}", nest.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{}", nest.token).parse().unwrap(),
    );
    match tokio_tungstenite::connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status().as_u16(), 426);
        }
        other => panic!(
            "the WS-RPC subprotocol must not open a tunnel: {:?}",
            other.is_ok()
        ),
    }
}

/// Wait (bounded) until the actor holds no live tunnel slot and no teardown
/// registration — both free as the tunnel's task unwinds after its close.
async fn assert_all_freed(nest: &Nest) {
    for _ in 0..100 {
        let slots = nest.state.mail_import_tunnel.live_counts(&nest.actor);
        if slots == (0, 0) && nest.state.ws.tunnel_count(&nest.actor) == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "the revoked tunnel kept its slot ({:?}) or its registration ({})",
        nest.state.mail_import_tunnel.live_counts(&nest.actor),
        nest.state.ws.tunnel_count(&nest.actor)
    );
}

/// A live tunnel is a socket the bearer opened: the one per-actor teardown
/// (lockout, suspend, the eviction ladder, deletion, succession) closes it
/// 4401 with the actor's RPC sockets.
#[tokio::test]
async fn revoking_the_actor_closes_its_open_tunnels() {
    let (port, _cert) = tls_echo().await;
    let nest = start(admitting(port)).await;
    let mut tunnels = Vec::new();
    for _ in 0..2 {
        let (answer, ws) = open(&nest, "localhost", port, false).await;
        assert_eq!(answer, TunnelAnswer::Opened {});
        tunnels.push(ws);
    }
    assert_eq!(
        nest.state.mail_import_tunnel.live_counts(&nest.actor),
        (2, 2)
    );

    nest.state.revoke_actor_authority(&nest.actor).await;

    for ws in &mut tunnels {
        assert_eq!(close_of(ws).await, (4401, "revoked".into()));
    }
    assert_all_freed(&nest).await;
}

/// The per-session revoke (`fauna.sessions.revoke`) closes the tunnels that
/// session's bearer opened, and leaves the actor's other sessions' alone.
#[tokio::test]
async fn revoking_one_session_closes_only_its_tunnels() {
    let (port, _cert) = tls_echo().await;
    let nest = start(admitting(port)).await;
    let other = nest
        .state
        .auth
        .token_store
        .insert(fauna_core::identity::ActorId(nest.actor), 3600)
        .await;
    let (answer, mut mine) = open(&nest, "localhost", port, false).await;
    assert_eq!(answer, TunnelAnswer::Opened {});
    let theirs = Nest {
        addr: nest.addr,
        token: other,
        state: Arc::clone(&nest.state),
        actor: nest.actor,
    };
    let (answer, mut kept) = open(&theirs, "localhost", port, false).await;
    assert_eq!(answer, TunnelAnswer::Opened {});

    let session = nest
        .state
        .auth
        .token_store
        .validate_with_session(&nest.token)
        .await
        .unwrap();
    nest.state
        .revoke_session_authority(&nest.actor, &session.token_id)
        .await;

    assert_eq!(close_of(&mut mine).await, (4401, "revoked".into()));
    // The other session's tunnel stays open.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), kept.next())
            .await
            .is_err(),
        "the other session's tunnel must not close"
    );
    assert_eq!(
        nest.state.mail_import_tunnel.live_counts(&nest.actor),
        (1, 1)
    );
    assert_eq!(nest.state.ws.tunnel_count(&nest.actor), 1);
}

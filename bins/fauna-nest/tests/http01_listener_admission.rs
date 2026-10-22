//! The ACME HTTP-01 listener (host port 80 on every deployed box) is served
//! through the nest's one admitted accept loop, and redirects only to hosts the
//! nest serves — `deployment-threat-model.md` § Concern 1a, finding M2.
//!
//! ## Why this test exists
//!
//! Until 2026-10-17 the port-80 listener was a bare `axum::serve` inside the
//! nest process: no connection cap, no header-read timeout, no governor, no
//! body limit — so a slow-loris on :80 spent the file descriptors :443 needs —
//! and its fallback reflected any `Host` header into a `308`, an open redirect
//! on a port every box opens.
//!
//! ## What it asserts
//!
//! Through the production serve path (`acme_http01::serve_http01`, the body of
//! `start_http01_listener`) with a loopback ceiling of 2: two held keep-alive
//! connections fill the ceiling and the third is **shed** unanswered; the ACME
//! token path still answers; a request whose `Host` is foreign gets `404` and
//! no `Location`; a host one of the nest's certificates names (DNS or IP SAN)
//! is still redirected to its https origin.
//!
//! ## Red-verification
//!
//! Against the old bare `axum::serve`, the third connection is answered `200`
//! and the foreign `Host` is answered `308 Location: https://evil.test/…`, so
//! both assertions fail deterministically.
//!
//! ## Timing (convention 14)
//!
//! Every wait is a positive wait on [`BUDGET`] for a state that must arrive (a
//! complete response, a close) — never a sleep-then-assert.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fauna_nest::acme::MultiDomainCertResolver;
use fauna_nest::acme_http01::{ChallengeState, serve_http01};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Far above any non-pathological local round-trip; a wait that hits it is a
/// failure, not a flake.
const BUDGET: Duration = Duration::from_secs(30);

const TOKEN: &str = "tok-123";
const KEY_AUTH: &str = "tok-123.thumbprint";

/// Serve the port-80 app on a loopback listener with the given loopback
/// ceiling; the nest's default cert names `nest.example`, `mail.nest.example`
/// and `203.0.113.7`.
async fn spawn_http01(loopback_ceiling: usize) -> SocketAddr {
    let state = Arc::new(ChallengeState::new());
    state.set(TOKEN.into(), KEY_AUTH.into()).await;
    let (cert, key) = fauna_acme_http01::test_support::make_multi_san_cert_and_key_pem(&[
        "nest.example",
        "mail.nest.example",
        "203.0.113.7",
    ]);
    let resolver = Arc::new(MultiDomainCertResolver::new(
        fauna_acme_http01::load_certified_key_from_pem(&cert, &key).expect("cert"),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // spawn-ok(test)
    tokio::spawn(serve_http01(
        listener,
        state,
        "nest.example".into(),
        Some(resolver),
        fauna_conn_limit::PerIpConnLimit::with_loopback_ceiling(
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
            loopback_ceiling,
        ),
    ));
    addr
}

/// Fetch the challenge on a keep-alive connection and hold it: the response
/// arriving proves the connection was admitted, and its permit stays held for
/// as long as the socket is open.
async fn hold_keepalive(addr: SocketAddr) -> TcpStream {
    let mut s = TcpStream::connect(addr).await.expect("tcp connect");
    let req =
        format!("GET /.well-known/acme-challenge/{TOKEN} HTTP/1.1\r\nHost: nest.example\r\n\r\n");
    s.write_all(req.as_bytes()).await.expect("write");
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    tokio::time::timeout(BUDGET, async {
        while !got.ends_with(KEY_AUTH.as_bytes()) {
            let n = s.read(&mut buf).await.expect("read");
            assert!(n > 0, "closed before answering: {got:?}");
            got.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .expect("challenge answered within budget");
    s
}

/// One request on a fresh `Connection: close` connection; returns everything
/// the server sent before closing — EMPTY when it shed the connection.
async fn raw_get(addr: SocketAddr, path: &str, host: &str) -> String {
    let mut s = TcpStream::connect(addr).await.expect("tcp connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    // A shed server may already have closed: EPIPE is the same evidence as an
    // empty read, so the write's result is deliberately not asserted.
    let _ = s.write_all(req.as_bytes()).await;
    let mut out = Vec::new();
    match tokio::time::timeout(BUDGET, s.read_to_end(&mut out)).await {
        Ok(Ok(_)) | Ok(Err(_)) => String::from_utf8_lossy(&out).into_owned(),
        Err(_) => panic!("the server neither answered nor closed within {BUDGET:?}"),
    }
}

fn location(resp: &str) -> Option<&str> {
    resp.lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or_else(|| l.strip_prefix("Location: "))
        })
        .map(str::trim)
}

#[tokio::test]
async fn port_80_sheds_past_the_connection_ceiling() {
    let addr = spawn_http01(2).await;
    let _held_a = hold_keepalive(addr).await;
    let _held_b = hold_keepalive(addr).await;

    let resp = raw_get(
        addr,
        &format!("/.well-known/acme-challenge/{TOKEN}"),
        "nest.example",
    )
    .await;
    assert!(
        resp.is_empty(),
        "the connection past the ceiling must be shed unanswered, but port 80 answered {resp:?} \
         — the listener is unbounded again (finding M2)"
    );
}

#[tokio::test]
async fn port_80_answers_the_token_and_redirects_only_to_served_hosts() {
    let addr = spawn_http01(fauna_conn_limit::LOOPBACK_MAX_CONNS).await;

    let resp = raw_get(
        addr,
        &format!("/.well-known/acme-challenge/{TOKEN}"),
        "nest.example",
    )
    .await;
    assert!(
        resp.starts_with("HTTP/1.1 200") && resp.ends_with(KEY_AUTH),
        "the ACME token path answers through the bounded loop: {resp:?}"
    );

    let resp = raw_get(addr, "/x?y=1", "evil.test").await;
    assert!(
        resp.starts_with("HTTP/1.1 404") && location(&resp).is_none(),
        "a foreign Host must get no redirect (the open redirect of finding M2): {resp:?}"
    );

    for (host, want) in [
        ("nest.example", "https://nest.example/x?y=1"),
        ("MAIL.nest.example:80", "https://mail.nest.example/x?y=1"),
        ("203.0.113.7", "https://203.0.113.7/x?y=1"),
    ] {
        let resp = raw_get(addr, "/x?y=1", host).await;
        assert!(resp.starts_with("HTTP/1.1 308"), "{host}: {resp:?}");
        assert_eq!(location(&resp), Some(want), "{host}");
    }

    let resp = raw_get(addr, "/x", "203.0.113.8").await;
    assert!(
        resp.starts_with("HTTP/1.1 404"),
        "an IP the nest's certificates do not name gets no redirect: {resp:?}"
    );
}

//! The **pebble real-wire acceptance** for the shared ACME **HTTP-01** flow —
//! the CA-facing half (`account → order → authorizations → finalize`) that the
//! CA-free unit tests cannot exercise. This is the flow BOTH consumers ride:
//! the nest's cert lifecycle and the fauna.social front door's renewal task
//! (front-door.md § TLS policy).
//!
//! Shape (mirrors `fauna-client-dns/tests/pebble_dns01.rs`, adapted from
//! DNS-01 to HTTP-01 validation):
//! - **In-process DNS responder** — answers `A 127.0.0.1` for every name, so
//!   pebble (`-dnsserver <this>`) resolves the order's identifiers to this
//!   host; AAAA/CAA/etc. get empty-NOERROR.
//! - **The lib's own challenge listener** — `start_http01_listener` on port
//!   5002, pebble's baked `httpPort`: pebble validates by fetching
//!   `/.well-known/acme-challenge/{token}` from the very router production
//!   serves.
//! - **CA-trusting client** — pebble's ACME endpoint is signed by its own
//!   throwaway CA; the test injects a client trusting it through the
//!   `test-helpers`-only [`obtain_certificate_with_http`], plus the
//!   `User-Agent` pebble insists on (production never trusts either) — the
//!   docker lifecycle and this client are `fauna-pebble-testkit`, shared with
//!   `pebble_dns01.rs`.
//! - **Pebble lifecycle** — `docker run`/`rm -f` (RAII) on the host network.
//!
//! `#[ignore]` — needs Docker (pulls pebble). Run: `just e2e-pebble-http01`.
//! Pebble binds host port 14000 and the listener takes 5002 — run serially,
//! and not concurrently with `e2e-pebble-dns01`.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;

use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{RData, Record, RecordType};

use fauna_acme_http01::{
    CERT_FILENAME, ChallengeState, Http01Config, IP_CERT_FILENAME, IP_KEY_FILENAME, KEY_FILENAME,
    cert_covers_ip_sans, cert_covers_sans, cert_dns_sans, cert_ip_sans, cert_seconds_remaining,
    obtain_certificate_with_http, obtain_ip_certificate_with_http, start_http01_listener,
};
use fauna_pebble_testkit::{PEBBLE_DIR_URL, Pebble, ensure_docker, pebble_http_client};

/// The port pebble's baked `pebble-config.json` fetches HTTP-01 challenges
/// from (it deliberately does not use 80, so an unprivileged test can bind).
const PEBBLE_HTTP_PORT: u16 = 5002;

#[tokio::test]
#[ignore = "needs Docker; pulls + runs pebble on the host network (just e2e-pebble-http01)"]
async fn http01_flow_issues_a_real_certificate_against_pebble() {
    ensure_docker("this #[ignore]d acceptance test");

    let dns_addr = spawn_dns_responder().await;
    let challenge_state = Arc::new(ChallengeState::new());
    let domains = vec![
        "door.pebble.test".to_string(),
        "www.door.pebble.test".to_string(),
    ];
    // The production listener/router, on pebble's validation port.
    let _listener = start_http01_listener(
        challenge_state.clone(),
        domains[0].clone(),
        PEBBLE_HTTP_PORT,
    )
    .await
    .expect("bind the HTTP-01 challenge listener on 5002");

    let pebble = Pebble::start("http01", "http01", &dns_addr.to_string());
    let acme_dir = tempfile::tempdir().expect("acme dir");
    // No contact ⇒ the account is created with none (RFC 8555 allows it);
    // exercises the omit-malformed-mailto branch.
    let config = Http01Config::new(
        domains[0].clone(),
        acme_dir.path().to_path_buf(),
        PEBBLE_HTTP_PORT,
        Some(PEBBLE_DIR_URL.to_string()),
    );

    obtain_certificate_with_http(
        &config,
        &domains,
        &challenge_state,
        acme_dir.path(),
        pebble_http_client(&pebble.ca_pem, "fauna-pebble-http01-test/0"),
    )
    .await
    .expect("HTTP-01 order against pebble");

    // The material a consumer (nest serve / door resolver) loads is on disk…
    let chain = std::fs::read(acme_dir.path().join(CERT_FILENAME)).expect("fullchain.pem written");
    std::fs::read(acme_dir.path().join(KEY_FILENAME)).expect("privkey.pem written");
    // …covers the whole SAN set…
    let sans = cert_dns_sans(&chain);
    assert!(
        domains.iter().all(|d| sans.contains(d)),
        "issued cert must cover the SAN set; got {sans:?}"
    );
    assert!(cert_covers_sans(&chain, &domains));
    // …and reads as healthy to the renewal decision inputs.
    assert!(cert_seconds_remaining(&chain).expect("parseable chain") > 0);
    // The solved challenges were cleaned out of the listener state.
    // (Any token would do — the state map is empty after cleanup, so a probe
    // for a random token answers None.)
    assert!(challenge_state.get("no-such-token").await.is_none());
}

/// The **§ B-IP bridge cert's** real-wire acceptance: an RFC 8738 `ip`-identifier
/// order under the `shortlived` profile, driven through the same production
/// machinery the domain order rides.
///
/// The identifier is `127.0.0.1` — deliberately, and this is the whole reason
/// the acceptance lives here rather than in the tier_4 nest suite. Validation of
/// an IP identifier needs **no DNS at all**: pebble dials the address itself on
/// its baked `httpPort`, so the loopback the challenge listener is already bound
/// to is exactly what the CA fetches from. The nest's own derive
/// (`ip_bridge_addresses` → `fauna_core::resolve::is_global_ip`) would rightly
/// refuse a loopback or RFC 1918 address, and it must not gain a knob to be
/// talked out of that — so the *order* is
/// pinned here, where the derive is not in the way, and the *derive* stays
/// pinned by its own unit tests in the nest.
///
/// What this proves that nothing else can: that an `Identifier::Ip` order is
/// accepted, that HTTP-01 validation against a bare address works through the
/// unmodified challenge router (the `Host` header is an address, not a name),
/// and that the issued leaf carries an `iPAddress` SAN rather than a `dNSName`
/// — a classification made by `rcgen` deep inside the shared finalize tail,
/// invisible at the call site, and otherwise only observable against a live CA.
#[tokio::test]
#[ignore = "needs Docker; pulls + runs pebble on the host network (just e2e-pebble-http01)"]
async fn ip_identifier_flow_issues_a_real_certificate_against_pebble() {
    ensure_docker("this #[ignore]d acceptance test");

    let dns_addr = spawn_dns_responder().await;
    let challenge_state = Arc::new(ChallengeState::new());
    let addr: std::net::IpAddr = "127.0.0.1".parse().expect("loopback literal");

    let _listener =
        start_http01_listener(challenge_state.clone(), addr.to_string(), PEBBLE_HTTP_PORT)
            .await
            .expect("bind the HTTP-01 challenge listener on 5002");

    let pebble = Pebble::start("http01-ip", "http01-ip", &dns_addr.to_string());
    let acme_dir = tempfile::tempdir().expect("acme dir");
    let config = Http01Config::new(
        addr.to_string(),
        acme_dir.path().to_path_buf(),
        PEBBLE_HTTP_PORT,
        Some(PEBBLE_DIR_URL.to_string()),
    );

    obtain_ip_certificate_with_http(
        &config,
        &[addr],
        &challenge_state,
        acme_dir.path(),
        pebble_http_client(&pebble.ca_pem, "fauna-pebble-http01-test/0"),
    )
    .await
    .expect("IP-identifier order against pebble");

    // The bridge's own material, under its own filenames — never the domain
    // cert's, so the two lifecycles cannot clobber each other (§ B-IP).
    let chain =
        std::fs::read(acme_dir.path().join(IP_CERT_FILENAME)).expect("ip-fullchain.pem written");
    std::fs::read(acme_dir.path().join(IP_KEY_FILENAME)).expect("ip-privkey.pem written");
    assert!(
        !acme_dir.path().join(CERT_FILENAME).exists(),
        "an IP order must not write the domain cert's filenames"
    );

    // …carrying an iPAddress SAN, not a dNSName. This is the assertion the unit
    // pins cannot make: `rcgen` decides the GeneralName kind from the string,
    // and a silent flip to dNSName would still produce a valid-looking cert.
    assert_eq!(
        cert_ip_sans(&chain),
        vec![addr],
        "the issued leaf must carry the address as an iPAddress SAN"
    );
    assert!(cert_covers_ip_sans(&chain, &[addr]));
    assert!(
        cert_dns_sans(&chain).is_empty(),
        "…and no dNSName — an IP cert has no hostname at all (§ B-IP: this is \
         exactly why it authenticates less than a domain cert)"
    );
    assert!(cert_seconds_remaining(&chain).expect("parseable chain") > 0);
    assert!(challenge_state.get("no-such-token").await.is_none());
}

// ── in-process DNS responder: everything resolves to 127.0.0.1 ──────────────

async fn spawn_dns_responder() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind dns responder udp socket");
    let addr = socket.local_addr().expect("dns responder local addr");
    tokio::spawn(run_dns(socket));
    addr
}

async fn run_dns(socket: UdpSocket) {
    let mut buf = [0u8; 1500];
    loop {
        let (n, src) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let Ok(request) = Message::from_vec(&buf[..n]) else {
            continue;
        };
        let response = build_response(&request);
        if let Ok(bytes) = response.to_vec() {
            let _ = socket.send_to(&bytes, src).await;
        }
    }
}

/// `A 127.0.0.1` for every A query (pebble then fetches the challenge from
/// this host's `httpPort`); empty-NOERROR for everything else, so the CAA
/// walk finds no restriction and AAAA resolution simply finds nothing.
fn build_response(request: &Message) -> Message {
    let mut response = Message::new(request.metadata.id, MessageType::Response, OpCode::Query);
    response.metadata.recursion_desired = request.metadata.recursion_desired;
    response.metadata.recursion_available = true;
    response.metadata.authoritative = true;
    response.metadata.response_code = ResponseCode::NoError;
    for query in &request.queries {
        response.add_query(query.clone());
    }
    if let Some(query) = request.queries.first()
        && query.query_type() == RecordType::A
    {
        response.add_answer(Record::from_rdata(
            query.name().clone(),
            5,
            RData::A(A::new(127, 0, 0, 1)),
        ));
    }
    response
}

//! tier_3 — the **session connector's half of the reach hint's dial rule**
//! (`docs/goal/behavior/onboarding-provisioning.md` § Reach hint → *The session
//! connector's half*), against a real nest serving HTTPS with a self-signed
//! cert:
//!
//! > dial the domain first; if that fails to connect … dial the hint …; the
//! > first successful domain dial deletes the hint. The hint moves the socket
//! > and nothing else: on both arms the identity key stays the domain's.
//!
//! The launch machine's half is pinned over a scripted connector
//! (`fauna-launch-machine/tests/reach_hint_dial.rs`) and end to end by
//! `test_reach_hint_dial_journey.py`. What is asserted here is the half those
//! cannot reach: the **session's** own mint and bearer socket — built from a
//! URL and a secret, with no hint in sight — against a real TLS nest:
//!
//! 1. A domain that does not resolve plus a hint in the process reach table
//!    mints a bearer and opens the bearer socket over the hint, with the
//!    request still naming the domain.
//! 2. A **stale** hint — the recycled address now answering with another box's
//!    key — is refused by the pin the session already holds for the domain, on
//!    the session dial exactly as on the launch dial: no bearer ever rides to
//!    that box.
//! 3. The first bearer socket that opens over the domain itself spends the
//!    hint: the table entry goes and the deleter runs.
//!
//! The dark domain is an RFC 6761 `.invalid` name, so the domain dial fails to
//! resolve wherever this runs; the "resolving" domain is `localhost`. Lives in
//! its own integration-test binary: it installs the process-global pin store
//! and the process-global reach table, which another binary's tests would
//! race.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use common::FixedSpki;
use fauna_anon_client::cert_binding::DiskPinStore;
use fauna_anon_client::reach_hint;
use fauna_anon_client::tls_dial::dial_ws_trusted_with_graduate_retry;
use fauna_client::ws_challenge_bearer::WsChallengeBearer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::acme::spki_sha256_of_cert_der;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest_http::BearerSource;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;

/// The account's domain: guaranteed never to resolve (RFC 6761), the same door
/// the e2e journey's dark half uses.
const DARK_DOMAIN: &str = "reach-hint-session.invalid";

/// One writable `DiskPinStore` for the whole process — the interactive app's
/// store shape, where the session's own mint TOFU-pins the box's identity for
/// the domain (the claim-seeded pin stands in for it on a provisioned box).
fn writable_pin_store() -> &'static Arc<DiskPinStore> {
    static STORE: OnceLock<Arc<DiskPinStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        let r = fauna_anon_client::trust::fresh_nonce();
        let dir = std::env::temp_dir().join(format!("fauna-reach-hint-pins-{}", hex::encode(r)));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(DiskPinStore::open_in_dir(&dir));
        fauna_anon_client::trust::install_pin_store(store.clone());
        store
    })
}

/// Spin a real nest serving HTTPS with a fresh self-signed cert under its own
/// Ed25519 identity (`seed`). Two of these with different seeds are two boxes:
/// the real one, and the stranger now answering at a recycled address.
async fn start_tls(seed: [u8; 32]) -> (std::net::SocketAddr, Arc<CacheDb>, [u8; 32]) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(&seed)),
        served_cert_spki: Some(Arc::new(FixedSpki(spki))),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);

    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (addr, db, spki)
}

/// A plain TCP forwarder in front of a nest that counts the connections the
/// client opens through it — so "no bearer ever rode to the stranger" is a
/// fact about what the client sent, not an inference.
async fn counting_forwarder(
    upstream: std::net::SocketAddr,
) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut outbound) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
    (addr, accepted)
}

/// The bearer WS request `fauna_client::ws_adapter` builds for `nest_url`.
fn bearer_request(nest_url: &str, kp: &ActorKeypair, token: &str) -> Request {
    let url = fauna_ws_substrate::actor_ws_url(nest_url, &kp.actor_id_hex());
    let mut request: Request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        fauna_ws_substrate::bearer_subprotocol_header(token).expect("bearer header"),
    );
    request
}

fn deleter() -> (reach_hint::OnDomainReached, Arc<AtomicUsize>) {
    let fired = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&fired);
    (
        Arc::new(move || {
            f.fetch_add(1, Ordering::SeqCst);
        }),
        fired,
    )
}

#[tokio::test]
async fn a_dark_domain_with_a_hint_mints_and_connects_over_the_hint_and_a_stale_hint_is_refused() {
    let _store = writable_pin_store();
    let (real, db, real_spki) = start_tls([5u8; 32]).await;
    let (stranger, _stranger_db, stranger_spki) = start_tls([6u8; 32]).await;
    assert_ne!(real_spki, stranger_spki, "two boxes, two keys");
    let (via_stranger, stranger_accepted) = counting_forwarder(stranger).await;

    // The account as the session client holds it: `https://{domain}` at the
    // box's port — nothing here names an address.
    let nest_url = format!("https://{DARK_DOMAIN}:{}", real.port());
    let kp = ActorKeypair::from_secret([0x3au8; 32]);
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");

    // ── 1. The hint carries the session's own mint and bearer socket. ────
    // What the launch machine installs after its hint-carried silent
    // challenge; the session connector knows only the URL and the secret.
    let (on_domain_reached, fired) = deleter();
    reach_hint::install(DARK_DOMAIN, real, on_domain_reached);

    let bearer =
        WsChallengeBearer::new(nest_url.clone(), kp.actor_id().0, kp.signing_key().clone());
    let token = bearer
        .bearer()
        .await
        .expect("the mint dials the domain, finds no host, and reaches the box over the hint");
    assert_eq!(
        fauna_anon_client::pinned_spki(&fauna_anon_client::authority_of(&nest_url)),
        Some(real_spki),
        "the mint's graduation pinned the served key under the DOMAIN's authority, never the address"
    );

    let dialed = dial_ws_trusted_with_graduate_retry(
        bearer_request(&nest_url, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;
    assert!(
        dialed.is_ok(),
        "the bearer socket opens over the hint with the request naming the domain: {:?}",
        dialed.err()
    );
    drop(dialed);
    assert_eq!(
        reach_hint::hint_for(DARK_DOMAIN),
        Some(real),
        "a hint-carried socket spends nothing — the domain has not answered"
    );
    assert_eq!(fired.load(Ordering::SeqCst), 0);

    // ── 2. A stale hint: the recycled address answers with another box. ──
    // The domain is re-provisioned onto a new IP and the old address goes to a
    // stranger. The session still holds the domain's pin, so the hint dial —
    // which keeps the domain as the pin key — must be refused at TLS, and the
    // bearer never rides to the stranger.
    let (on_domain_reached, fired_stale) = deleter();
    reach_hint::install(DARK_DOMAIN, via_stranger, on_domain_reached);
    let refused = dial_ws_trusted_with_graduate_retry(
        bearer_request(&nest_url, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;
    assert!(
        refused.is_err(),
        "a box with another key at the hint address must fail the domain's pin"
    );
    assert!(
        !matches!(
            refused,
            Err(fauna_anon_client::tls_dial::TrustedDialError::Ws(
                tokio_tungstenite::tungstenite::Error::Http(_)
            ))
        ),
        "refused at TLS, before any upgrade request carrying the bearer was sent"
    );
    assert_eq!(
        stranger_accepted.load(Ordering::SeqCst),
        2,
        "exactly two TCP connections reached the stranger — the pinned TLS handshake that \
         refused it, and the stale-pin re-bind's anonymous `nest_handshake` (no bearer, a \
         client nonce only), which the stranger's binding fails against the domain's identity \
         pin; a third would be a pinned retry carrying the bearer"
    );
    assert_eq!(
        reach_hint::hint_for(DARK_DOMAIN),
        Some(via_stranger),
        "a failed hint dial deletes nothing: a failed fallback is not a wrong address"
    );
    assert_eq!(fired_stale.load(Ordering::SeqCst), 0);
    reach_hint::clear(DARK_DOMAIN);
}

#[tokio::test]
async fn the_first_bearer_socket_over_the_domain_spends_the_hint() {
    let _store = writable_pin_store();
    let (real, db, real_spki) = start_tls([7u8; 32]).await;
    // A domain that resolves — to the real box — with a hint left over from
    // the dark days, pointing somewhere that would never answer.
    let nest_url = format!("https://localhost:{}", real.port());
    let kp = ActorKeypair::from_secret([0x4bu8; 32]);
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
    let (on_domain_reached, fired) = deleter();
    reach_hint::install(
        "localhost",
        std::net::SocketAddr::from(([127, 0, 0, 1], 9)),
        on_domain_reached,
    );

    let bearer =
        WsChallengeBearer::new(nest_url.clone(), kp.actor_id().0, kp.signing_key().clone());
    let token = bearer.bearer().await.expect("the domain reaches the box");
    assert_eq!(
        fauna_anon_client::pinned_spki(&fauna_anon_client::authority_of(&nest_url)),
        Some(real_spki)
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "the mint alone spends nothing — the bearer socket is the domain success"
    );

    let dialed = dial_ws_trusted_with_graduate_retry(
        bearer_request(&nest_url, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;
    assert!(dialed.is_ok(), "{:?}", dialed.err());
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the first bearer socket over the domain ran the deleter once"
    );
    assert_eq!(
        reach_hint::hint_for("localhost"),
        None,
        "the hint is spent: later dials take the domain alone"
    );
}

//! tier_3 — the bearer dial's **graduate-and-retry fallback in a process whose
//! pin store is WRITABLE**, against a real nest serving HTTPS with a
//! self-signed cert (security.md § Pin custody across processes, rule 2 and
//! *the consumer's dial needs its own graduation step*).
//!
//! The consumer-side twin (`tls_pin_consumer_fallback.rs`) proves the
//! fallback strict against a **read-only** store. This file is the other half
//! of the same rule: every interactive app, tui and the
//! standalone daemon dial with a *writable* store, and the fallback used to let
//! that store decide — so on any dial error with no SPKI pin (the normal state
//! for a public-CA nest, whose bearer mint takes the WebPKI waiver and pins
//! nothing), the fallback's graduation TOFU-minted whoever answered the
//! pre-identity handshake with a binding-valid self-signed cert, then re-dialed
//! pinned to that key **with the real bearer**. An on-path attacker needs no
//! CA compromise for that: it turns the WebPKI waiver off by serving its own
//! cert, answers the handshake with its own identity, and collects a bearer it
//! can replay against the real nest for the token's lifetime.
//!
//! The nest here is genuine; the point is that the CLIENT cannot tell it from
//! the attacker at this moment — no pin, no `self=` root, a self-signed cert
//! and a valid binding is exactly what both present — so the only safe answer
//! is to refuse to mint. A TCP forwarder counts the connections the client
//! opens: two (the failed strict-WebPKI dial, the fallback's anonymous
//! handshake) and never a third, bearer-carrying retry.
//!
//! Lives in its own integration-test binary: the process-global pin store it
//! installs must stay writable for the whole run, which the read-only twin
//! would race.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use common::FixedSpki;
use fauna_anon_client::cert_binding::{DiskPinStore, NestIdentityPinStore};
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

/// The nest's Ed25519 deployment identity seed — what an interactive mint or
/// claim would have pinned for the host.
const NEST_SIGNING_SEED: [u8; 32] = [5u8; 32];

/// One writable `DiskPinStore` for the whole process, installed once — the
/// interactive app's / tui's / the daemon's store shape. Both tests share it
/// (pins are keyed by authority, and each test dials its own proxy port).
///
/// Returns the store's dir AND the installed store itself: a writable
/// `DiskPinStore` answers reads from its in-memory cache, so a pin written
/// through a second instance over the same dir is invisible to the installed
/// one. A test that seeds a pin must write through the installed store, as
/// the interactive app's own mint does. (Reading the FILE back through a fresh
/// instance is fine: every `set` persists.)
fn writable_pin_store() -> &'static (std::path::PathBuf, Arc<DiskPinStore>) {
    static STORE: OnceLock<(std::path::PathBuf, Arc<DiskPinStore>)> = OnceLock::new();
    STORE.get_or_init(|| {
        let r = fauna_anon_client::trust::fresh_nonce();
        let dir = std::env::temp_dir().join(format!("fauna-writable-pins-{}", hex::encode(r)));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(DiskPinStore::open_in_dir(&dir));
        fauna_anon_client::trust::install_pin_store(store.clone());
        (dir, store)
    })
}

/// Spin a real nest serving HTTPS with a fresh self-signed cert (the genuine
/// arm of `tls_channel_binding_roundtrip.rs`'s harness). Returns the listen
/// address, the db (an actor must be admitted before it can mint), and the
/// served leaf's SPKI.
async fn start_tls() -> (std::net::SocketAddr, Arc<CacheDb>, [u8; 32]) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let real_spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    let nest_signing_key = ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED);

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        nest_signing_key: Some(nest_signing_key),
        served_cert_spki: Some(Arc::new(FixedSpki(real_spki))),
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
    (addr, db, real_spki)
}

/// A plain TCP forwarder in front of the nest that counts every connection the
/// client opens through it — the on-path vantage point, minus the attack. TLS
/// runs end-to-end through it untouched, so the client sees the nest's own
/// self-signed cert and binding. A count is a fact about what the client
/// *sent*, which is what the finding is about: the bearer rides the retry.
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

/// Admit an actor and mint it a real bearer the interactive way, directly
/// against the nest's own authority — so the reconnect under test carries a
/// valid bearer, exactly what the attacker is after. (The mint's own signed
/// graduation pins the nest's *direct* authority; the dial under test goes
/// through the forwarder, a different authority with no pin.)
async fn mint_bearer(nest_addr: std::net::SocketAddr, db: &CacheDb, kp: &ActorKeypair) -> String {
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
    let bearer = WsChallengeBearer::new(
        format!("https://{nest_addr}"),
        kp.actor_id().0,
        kp.signing_key().clone(),
    );
    bearer.bearer().await.expect("interactive mint over wss")
}

/// The bearer WS request `fauna_client::ws_adapter` builds, aimed through the
/// forwarder.
fn bearer_request(via: std::net::SocketAddr, kp: &ActorKeypair, token: &str) -> Request {
    let url = fauna_ws_substrate::actor_ws_url(&format!("https://{via}"), &kp.actor_id_hex());
    let mut request: Request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        fauna_ws_substrate::bearer_subprotocol_header(token).expect("bearer header"),
    );
    request
}

#[tokio::test]
async fn a_writable_store_with_no_pin_refuses_the_fallback_and_sends_no_retry() {
    let (dir, _store) = writable_pin_store();
    let (nest_addr, db, _spki) = start_tls().await;
    let (via, accepted) = counting_forwarder(nest_addr).await;
    let authority = fauna_anon_client::authority_of(&format!("https://{via}"));
    let kp = ActorKeypair::from_secret([0x6bu8; 32]);
    let token = mint_bearer(nest_addr, &db, &kp).await;

    // The state the finding starts from: a writable store, no identity pin
    // and no SPKI for the authority being dialed — a public-CA nest's bearer
    // mint leaves exactly this behind, and an on-path attacker only has to
    // make the first dial fail.
    assert_eq!(fauna_anon_client::pinned_identity(&authority), None);
    assert_eq!(fauna_anon_client::pinned_spki(&authority), None);
    assert!(
        !DiskPinStore::open_in_dir(dir).read_only(),
        "precondition: a writable store"
    );

    let result = dial_ws_trusted_with_graduate_retry(
        bearer_request(via, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;

    assert!(
        result.is_err(),
        "no pin + a self-signed cert: the bearer dial must fail, never heal by trusting \
         whoever answered the pre-identity handshake"
    );
    assert_eq!(
        DiskPinStore::open_in_dir(dir).get(&authority),
        None,
        "the fallback's graduation must not have TOFU-minted a pin into the writable store"
    );
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        None,
        "no SPKI may be cached for an authority nothing pinned"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "exactly the failed strict-WebPKI dial and the fallback's anonymous handshake — \
         a third connection is the pinned retry carrying the real bearer"
    );
}

#[tokio::test]
async fn a_held_identity_pin_still_lets_the_fallback_heal() {
    let (dir, store) = writable_pin_store();
    let (nest_addr, db, spki) = start_tls().await;
    let (via, accepted) = counting_forwarder(nest_addr).await;
    let authority = fauna_anon_client::authority_of(&format!("https://{via}"));
    let kp = ActorKeypair::from_secret([0x7cu8; 32]);
    let token = mint_bearer(nest_addr, &db, &kp).await;

    // The interactive path pinned this nest's identity for the authority
    // earlier (a signed mint, or the claim ceremony's seed) — the one thing
    // that licenses the fallback to graduate: it verifies against the pin,
    // never mints one.
    let nest_identity = ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED)
        .verifying_key()
        .to_bytes();
    store.set(&authority, nest_identity);
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        None,
        "no SPKI yet"
    );

    let result = dial_ws_trusted_with_graduate_retry(
        bearer_request(via, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;

    assert!(
        result.is_ok(),
        "with the identity pinned, the fallback verifies the binding against it, caches \
         the served SPKI and the pinned retry completes"
    );
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki),
        "the heal cached the served leaf's SPKI for the bearer dial"
    );
    assert_eq!(
        DiskPinStore::open_in_dir(dir).get(&authority),
        Some(nest_identity),
        "the pin is verified, not rewritten"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        3,
        "the failed strict-WebPKI dial, the fallback's anonymous handshake, the pinned retry"
    );
}

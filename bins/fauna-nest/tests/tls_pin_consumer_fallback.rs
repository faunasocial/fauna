//! tier_3 — the **pin-consumer bearer dial** end-to-end against a real nest
//! serving HTTPS with a self-signed cert (security.md § Transport trust, pin
//! custody across processes).
//!
//! The process shape under test is the apple File Provider extension (and any
//! background agent): it holds a **bearer but no identity key**, so nothing in
//! the process ever runs the signed `fauna.auth.handshake` that would graduate
//! a bound SPKI — and before the `ws_adapter` graduate-and-retry fallback the
//! bearer dial to a self-signed nest failed WebPKI (`UnknownIssuer`) forever,
//! which is exactly the measured 2026-07-22 appex pathology. The fallback runs
//! one pre-identity `fauna.auth.nest_handshake` graduation, strict against the
//! consumer's **read-only** pin store:
//!
//! - no pin in the store → `PinRequired`, the dial keeps failing (a consumer
//!   must never TOFU-mint with no user in the loop), and
//! - once the interactive app's pin is present (written by a separate writer
//!   store — the consumer store is uncached), the same connect graduates,
//!   pins the bound SPKI, and reaches Connected.
//!
//! Lives in its own integration-test binary: it swaps the process-global pin
//! store to a read-only backend, which would race the other TLS tests' TOFU
//! minting if they shared a process.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{FixedSpki, wait_for_connected};
use fauna_anon_client::cert_binding::NestIdentityPinStore;
use fauna_client::NestClient;
use fauna_client::auth_client::AuthClient;
use fauna_client::ws_challenge_bearer::WsChallengeBearer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::acme::spki_sha256_of_cert_der;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest_http::BearerSource;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Spin a real nest serving HTTPS with a fresh self-signed cert (the genuine
/// arm of `tls_channel_binding_roundtrip.rs`'s harness).
async fn start_tls() -> (String, Arc<CacheDb>) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let real_spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    let nest_signing_key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);

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
    (format!("https://{addr}"), db)
}

#[tokio::test]
async fn consumer_dial_refuses_unpinned_then_connects_once_the_app_pins() {
    let (base, db) = start_tls().await;
    let authority = fauna_anon_client::authority_of(&base);
    // Fixed secret so the keypair can be re-derived per phase (`ActorKeypair`
    // is deliberately not `Clone`).
    let secret = [0x6bu8; 32];
    let kp = ActorKeypair::from_secret(secret);
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");

    // "The app": mint a bearer the normal signed way. This graduates the
    // binding (TOFU-minting into the default in-memory store) and caches the
    // token inside the WsChallengeBearer — the pre-provisioned bearer the
    // consumer process will hold.
    let bearer: Arc<WsChallengeBearer> = Arc::new(WsChallengeBearer::new(
        base.clone(),
        kp.actor_id().0,
        kp.signing_key().clone(),
    ));
    bearer.bearer().await.expect("app-side mint over wss");
    let app_pin = fauna_anon_client::pinned_identity(&authority)
        .expect("the app's mint TOFU-pinned the nest identity");

    // "The consumer process": a read-only store over a shared dir the app has
    // NOT yet written, and no graduated SPKI (forget clears the ephemeral
    // cache; the read-only remove is a no-op by design). The cached bearer
    // means `ensure_auth` never re-mints — no graduation happens on this path,
    // exactly the appex shape.
    let r = fauna_anon_client::trust::fresh_nonce();
    let dir = std::env::temp_dir().join(format!("fauna-consumer-pins-{}", hex::encode(r)));
    std::fs::create_dir_all(&dir).unwrap();
    fauna_anon_client::trust::install_pin_store(Arc::new(
        fauna_anon_client::cert_binding::ReadOnlyDiskPinStore::open_in_dir(&dir),
    ));
    fauna_anon_client::forget_identity_pin(&authority);
    assert_eq!(fauna_anon_client::pinned_spki(&authority), None);

    // Phase A — no pin in the consumer's store: the dial fails WebPKI, the
    // fallback graduation refuses (`PinRequired` — a consumer never mints),
    // and the connect surfaces an error instead of silently trusting.
    let auth = Arc::new(AuthClient::with_bearer_source(
        base.clone(),
        ActorKeypair::from_secret(secret),
        bearer.clone() as Arc<dyn BearerSource>,
        fauna_client::auth_client::pinned_http_client(&base),
    ));
    let nest = NestClient::with_auth(auth);
    // `connect()` spawns the reconnect supervisor and returns; the dial itself
    // is supervised. The refusal shows as the state never reaching Connected —
    // both the WebPKI dial and the PinRequired fallback fail in milliseconds
    // locally, so a 3 s window is orders of magnitude of margin.
    nest.connect().await.expect("connect spawns the supervisor");
    let mut state = nest.connection_state();
    let reached = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if *state.borrow() == fauna_client::types::ConnectionState::Connected {
                return;
            }
            if state.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    })
    .await;
    assert!(
        reached.is_err(),
        "an unpinned TOFU host must be refused by the consumer dial"
    );
    assert_eq!(
        fauna_anon_client::pinned_identity(&authority),
        None,
        "the refused connect must not have minted a pin"
    );

    // Phase B — "the app" writes its pin into the shared dir (a separate
    // writer store; the consumer's uncached read-only store sees it live).
    // The very next connect graduates via the pre-identity nest_handshake,
    // pins the bound SPKI, and reaches Connected.
    fauna_anon_client::cert_binding::DiskPinStore::open_in_dir(&dir).set(&authority, app_pin);
    let auth = Arc::new(AuthClient::with_bearer_source(
        base.clone(),
        ActorKeypair::from_secret(secret),
        bearer as Arc<dyn BearerSource>,
        fauna_client::auth_client::pinned_http_client(&base),
    ));
    let nest = NestClient::with_auth(auth);
    nest.connect()
        .await
        .expect("consumer connects once the app's pin is visible");
    wait_for_connected(&nest).await;
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_sha256_of_cert_der_for(&base).await),
        "the fallback graduation pinned the bound SPKI for the bearer dial"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The served leaf's SPKI as the client sees it (fetch over a captured anon
/// connection) — mirrors `tls_channel_binding_roundtrip.rs`.
async fn spki_sha256_of_cert_der_for(base: &str) -> [u8; 32] {
    let client = fauna_anon_client::AnonymousNestClient::connect(base)
        .await
        .expect("anon connect for SPKI capture");
    client
        .captured_cert()
        .spki
        .expect("captured the served leaf SPKI")
}

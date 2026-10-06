//! The discovery trust rule on the real wire (`federation.md` § Peer-auth
//! model → *Discovery trust rule*; `federation_pool::discovery_dial_admits`).
//!
//! Federation discovery resolves a peer's `nest_id` from its URL by dialing
//! the anonymous `fauna.nest.info` kind. That dial rides the capturing TLS
//! verifier, which accepts *any* cert encrypt-only and records whether it was
//! WebPKI-valid — so until the rule existed the reply was believed no matter
//! what cert the peer served, and an attacker owning a peer domain's
//! resolution impersonated it with a self-signed cert, no CA needed.
//!
//! An in-process nest can only ever serve a **self-signed** cert (there is no
//! CA to mint a trusted one under test) and only ever listen on **loopback**,
//! which the rule's fixture carve-out admits by design. So the refusal arm is
//! reached through the seam beneath `resolve_peer_nest_id`,
//! `resolve_peer_nest_id_in_class`, which takes the host class the production
//! path would otherwise establish by address: the same floor-TLS fixture is
//! placed in the `Global` class (a production peer) and must be refused, in
//! the `Loopback` class and must be admitted, and — as the deployment's own
//! pull target, an admin's pairing row — in the `NonGlobal` class and must be
//! admitted, while a URL only a request or a non-admin's row names in that
//! class is refused. The real cert is
//! captured on the real handshake every time; only the classification is
//! driven. The classifier and the rule's pure table are unit-tested beside
//! the rule.

mod common;
use common::FixedSpki;

use std::sync::Arc;

use fauna_nest::acme::spki_sha256_of_cert_der;
use fauna_nest::db::CacheDb;
use fauna_nest::federation_pool::{
    FederationChannelPool, PairingTargetTrust, PeerHostClass, PoolError,
};
use fauna_nest::routes::AppState;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// A self-signed floor cert for the loopback authority + the TLS acceptor
/// serving it (mirrors `conformance_cross_nest_conversations_client.rs`).
fn floor_tls() -> (tokio_rustls::TlsAcceptor, [u8; 32]) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()])
        .expect("self-signed floor cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    (tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg)), spki)
}

/// A pool whose pairing-target table holds just `url`, as
/// `nest_sync_worker::refresh_pairing_targets` builds it.
fn pool_with_target(url: &str, exempt: bool, pin: Option<[u8; 32]>) -> FederationChannelPool {
    let pool = FederationChannelPool::new();
    pool.set_pairing_targets(std::collections::HashMap::from([(
        url.to_string(),
        PairingTargetTrust { exempt, pin },
    )]));
    pool
}

/// A peer nest serving `fauna.nest.info` under the self-signed floor on a
/// loopback port. Returns its `https://` base and its `nest_id`.
async fn start_floor_peer() -> (String, [u8; 32]) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (acceptor, spki) = floor_tls();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        served_cert_spki: Some(Arc::new(FixedSpki(spki))),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db)
    });
    let nest_id = state.nest_identity.public_key_bytes();
    let app = fauna_nest::build_router(state);
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (format!("https://{addr}"), nest_id)
}

/// A production peer (the `Global` class) serving a cert the WebPKI does not
/// trust is refused — `PoolError::Unauthenticated`, and refused again on the
/// next call: nothing was cached from the refused dial.
#[tokio::test]
async fn a_self_signed_peer_in_the_global_class_is_refused_before_nest_info_is_believed() {
    let (url, _) = start_floor_peer().await;
    let pool = FederationChannelPool::new();
    for attempt in 1..=2 {
        let err = pool
            .resolve_peer_nest_id_in_class(&url, PeerHostClass::Global)
            .await
            .expect_err("a self-signed cert on a production peer must be refused");
        assert!(
            matches!(err, PoolError::Unauthenticated(_)),
            "attempt {attempt}: expected Unauthenticated, got {err:?}"
        );
    }
}

/// The same fixture through the production entry point — a loopback literal —
/// is admitted under the fixture carve-out, and resolves the peer's real id.
#[tokio::test]
async fn the_loopback_fixture_carve_out_still_admits_the_floor() {
    let (url, nest_id) = start_floor_peer().await;
    let pool = FederationChannelPool::new();
    let id = pool
        .resolve_peer_nest_id(&url)
        .await
        .expect("the loopback floor-TLS fixture is the documented carve-out");
    assert_eq!(id, nest_id);
}

/// A private address is admitted on its floor only as the deployment's own
/// pull target — a URL an admin's pairing row names (the LAN / VPN /
/// docker-bridge topology carve-out); a request-named URL, or one only a
/// non-admin's row names, at a private address is refused like any other
/// untrusted cert.
#[tokio::test]
async fn a_private_address_is_admitted_only_as_the_configured_pull_target() {
    let (url, nest_id) = start_floor_peer().await;

    let supplied = FederationChannelPool::new();
    let err = supplied
        .resolve_peer_nest_id_in_class(&url, PeerHostClass::NonGlobal)
        .await
        .expect_err("a request-named private peer on a self-signed cert must be refused");
    assert!(matches!(err, PoolError::Unauthenticated(_)), "got {err:?}");

    let non_admin = pool_with_target(&url, false, None);
    let err = non_admin
        .resolve_peer_nest_id_in_class(&url, PeerHostClass::NonGlobal)
        .await
        .expect_err("a non-admin's row is a request-named URL");
    assert!(matches!(err, PoolError::Unauthenticated(_)), "got {err:?}");

    let configured = pool_with_target(&url, true, None);
    let id = configured
        .resolve_peer_nest_id_in_class(&url, PeerHostClass::NonGlobal)
        .await
        .expect("the deployment's own private pull target is the topology carve-out");
    assert_eq!(id, nest_id);
}

/// The pull target is **pinned** once the private nest's pairing rows name
/// the public nest's id: a different nest answering at the private
/// address is refused (`PoolError::PeerMismatch`) before its `nest.info` is
/// cached, so it is refused again on the next call — the carve-out admits the
/// floor cert only for the nest the pairing names.
#[tokio::test]
async fn a_pinned_configured_target_answered_by_another_nest_is_refused() {
    let (url, _) = start_floor_peer().await;
    let pool = pool_with_target(&url, true, Some([7u8; 32]));
    for attempt in 1..=2 {
        let err = pool
            .resolve_peer_nest_id_in_class(&url, PeerHostClass::NonGlobal)
            .await
            .expect_err("a nest other than the pinned one must be refused");
        assert!(
            matches!(err, PoolError::PeerMismatch { .. }),
            "attempt {attempt}: expected PeerMismatch, got {err:?}"
        );
    }
}

/// The pinned nest itself is admitted on its floor cert at the private address.
#[tokio::test]
async fn a_pinned_configured_target_answering_as_its_pin_is_admitted() {
    let (url, nest_id) = start_floor_peer().await;
    let pool = pool_with_target(&url, true, Some(nest_id));
    let id = pool
        .resolve_peer_nest_id_in_class(&url, PeerHostClass::NonGlobal)
        .await
        .expect("the pinned public nest is the configured target");
    assert_eq!(id, nest_id);
}

/// The pin comes from nest state — the private nest's `nest_pairings` row
/// whose `nest_url` is the pull target carries the public nest's id in
/// `private_nest_id` — and it binds even an answer cached before the row was
/// seeded: once `refresh_pairing_targets` reads a row naming another nest at
/// that URL, the production entry point refuses the cached id.
#[tokio::test]
async fn the_pairing_rows_pin_the_configured_target_over_an_earlier_cached_answer() {
    let (url, nest_id) = start_floor_peer().await;
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = AppState::for_test(db.clone());

    // No pairing row yet: nothing to expect, the loopback carve-out admits
    // and caches.
    fauna_nest::nest_sync_worker::refresh_pairing_targets(&state).await;
    let id = state
        .federation_pool
        .resolve_peer_nest_id(&url)
        .await
        .unwrap();
    assert_eq!(id, nest_id);

    // The row names a different public nest at this URL.
    let caps = vec!["mls_pull".to_string()];
    db.store_pairing(&[1u8; 32], &[9u8; 32], &caps, None, Some(&url), None)
        .await
        .unwrap();
    fauna_nest::nest_sync_worker::refresh_pairing_targets(&state).await;
    let err = state
        .federation_pool
        .resolve_peer_nest_id(&url)
        .await
        .expect_err("the pairing row pins another nest");
    assert!(matches!(err, PoolError::PeerMismatch { .. }), "got {err:?}");

    // Re-pairing with the nest that really answers there admits it again.
    db.revoke_pairing(&[1u8; 32], &[9u8; 32]).await.unwrap();
    db.store_pairing(&[1u8; 32], &nest_id, &caps, None, Some(&url), None)
        .await
        .unwrap();
    fauna_nest::nest_sync_worker::refresh_pairing_targets(&state).await;
    let id = state
        .federation_pool
        .resolve_peer_nest_id(&url)
        .await
        .unwrap();
    assert_eq!(id, nest_id);
}

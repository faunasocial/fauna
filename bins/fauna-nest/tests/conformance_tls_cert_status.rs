//! **TLS Phase 4, A1 — the cert-status read RPC.** Proves
//! `fauna.tls.cert_status` is the Admin read surface behind the `admin-dns`
//! cert-status row (`tls-certificates.md` § C.4): it reports the health of the
//! cert the nest's listener **currently serves** per domain, sourced from the
//! live cert resolver (`AppState::served_cert_spki`) — never a client guess.
//!
//! This tier_3 test exercises the RPC contract + the admin gate + the
//! resolver→handler→wire flow on the realistic fresh-nest state (serving the
//! self-signed floor → `OnFloorRenewNeeded`). The floor-vs-trusted and
//! `expiring` *classification* is exhaustively unit-tested on the resolver in
//! `acme.rs` (`served_cert_facts_*` + `cert_health_state_maps_three_states`);
//! here we prove the wire path, ordering, and that a non-admin is denied.

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_nest::acme::{MultiDomainCertResolver, ServedCertSpki};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::tls_handlers::register_tls_handlers;
use fauna_protocol::tls::{CertHealthState, CertStatusReply, CertStatusRequest};
use fauna_protocol::{RpcError, decode_strict as decode};

/// An `AppState` whose RPC router speaks `fauna.tls.*` and whose
/// `served_cert_spki` is `resolver` (or `None` to model a plain-HTTP nest).
fn nest_with_resolver(resolver: Option<Arc<dyn ServedCertSpki>>) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_tls_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        served_cert_spki: resolver,
        ..AppState::for_test(db)
    })
}

/// A `MultiDomainCertResolver` serving a self-signed floor cert for `domain` as
/// its default (apex) — the fresh-nest state before any trusted cert is issued.
fn floor_resolver(domain: &str) -> Arc<dyn ServedCertSpki> {
    let (cert_pem, key_pem) =
        fauna_nest::self_signed_cert::synthesize_self_signed_pem(domain, vec![domain.to_string()])
            .expect("synthesize floor cert");
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("fullchain.pem");
    let key_path = dir.path().join("privkey.pem");
    std::fs::write(&cert_path, cert_pem.as_bytes()).unwrap();
    std::fs::write(&key_path, key_pem.as_bytes()).unwrap();
    let resolver = MultiDomainCertResolver::pending();
    resolver
        .reload_default_from_pem(&cert_path, &key_path)
        .expect("load floor cert");
    Arc::new(resolver)
}

async fn dispatch(
    state: &Arc<AppState>,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state
        .rpc_router
        .kind_meta("fauna.tls.cert_status")
        .expect("cert_status registered");
    (meta.handler)(state.clone(), actor, payload).await
}

/// An admin querying a floor-serving nest gets one status per requested domain,
/// in request order, each `OnFloorRenewNeeded` + `is_floor` (the fresh-nest
/// state), with the served leaf's real `notAfter` surfaced for the covered apex.
#[tokio::test]
async fn admin_cert_status_reports_floor_for_fresh_nest() {
    let state = nest_with_resolver(Some(floor_resolver("home.example.com")));

    let actor_sk = SigningKey::from_bytes(&[0x11u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    state.db.add_admin_actor(&actor[..]).await.unwrap();

    let req = CertStatusRequest {
        extra: Default::default(),
        domains: vec!["home.example.com".into(), "alice.com".into()],
    };
    let reply_bytes = dispatch(&state, actor, encode(&req))
        .await
        .expect("cert_status ok for admin");
    let reply: CertStatusReply = decode(&reply_bytes).unwrap();

    assert_eq!(reply.statuses.len(), 2, "one status per requested domain");
    // Order preserved.
    assert_eq!(reply.statuses[0].domain, "home.example.com");
    assert_eq!(reply.statuses[1].domain, "alice.com");
    // Both fall to the self-signed floor → renew needed.
    for s in &reply.statuses {
        assert_eq!(s.state, CertHealthState::OnFloorRenewNeeded);
        assert!(s.is_floor, "{} serves the self-signed floor", s.domain);
    }
    // The apex domain is covered by the served leaf → its real notAfter surfaces.
    assert!(
        reply.statuses[0].not_after_unix > 0,
        "served floor leaf carries a real notAfter"
    );
}

/// A nest with no TLS resolver (plain HTTP) reports every domain as on-floor
/// with no expiry — honest "no trusted cert here".
#[tokio::test]
async fn admin_cert_status_handles_no_resolver() {
    let state = nest_with_resolver(None);
    let actor_sk = SigningKey::from_bytes(&[0x12u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    state.db.add_admin_actor(&actor[..]).await.unwrap();

    let req = CertStatusRequest {
        extra: Default::default(),
        domains: vec!["home.example.com".into()],
    };
    let reply: CertStatusReply = decode(
        &dispatch(&state, actor, encode(&req))
            .await
            .expect("cert_status ok"),
    )
    .unwrap();
    assert_eq!(reply.statuses.len(), 1);
    assert_eq!(reply.statuses[0].state, CertHealthState::OnFloorRenewNeeded);
    assert_eq!(reply.statuses[0].not_after_unix, 0, "no cert → no expiry");
    assert!(reply.statuses[0].is_floor);
}

/// A non-admin caller is denied the read — the deployment's cert health is an
/// admin concern, same gate as `publish_cert`.
#[tokio::test]
async fn non_admin_cert_status_is_denied() {
    let state = nest_with_resolver(Some(floor_resolver("home.example.com")));
    // A non-admin actor (never added via add_admin_actor) → User class.
    let actor_sk = SigningKey::from_bytes(&[0x22u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();

    let req = CertStatusRequest {
        extra: Default::default(),
        domains: vec!["home.example.com".into()],
    };
    let err = dispatch(&state, actor, encode(&req))
        .await
        .expect_err("non-admin must be denied");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "expected a permission error, got {:?}",
        err.code
    );
}

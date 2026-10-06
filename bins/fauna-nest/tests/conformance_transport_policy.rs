//! Integration round-trip for the nest-OWN transport/abuse policy —
//! `fauna.transport.{put,get}_policy` (both Admin-class). Drives the full
//! Admin → `put_policy` → `get_policy` path so the wire shape + DB upsert +
//! handler + allowlist gate all participate.
//!
//! This is nest's own client-facing TLS-listener per-IP connection cap, NOT
//! a mail policy and NOT projected to the bridge — so unlike
//! `conformance_mail_policy.rs` the read is the dedicated `get_policy`, not a
//! `fetch_config` overlay. The cap is client-set per the product invariant
//! (`docs/goal/architecture/transport-connection.md` § Abuse posture item (2)).

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::transport_policy_handlers::register_transport_policy_handlers;
use fauna_protocol::transport::{
    GetTransportPolicyRequest, PutTransportPolicyReply, PutTransportPolicyRequest,
    TransportPolicyView,
};
use fauna_protocol::{RpcError, decode_strict as decode};

async fn fixture_state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_transport_policy_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        ..AppState::for_test(db)
    })
}

async fn dispatch(
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state
        .rpc_router
        .kind_meta(kind)
        .expect("kind registered with router");
    (meta.handler)(state.clone(), actor, payload).await
}

async fn admin_actor(state: &Arc<AppState>) -> [u8; 32] {
    let admin = [42u8; 32];
    state
        .db
        .add_admin_actor(&admin[..])
        .await
        .expect("add admin actor");
    admin
}

async fn get_view(state: &Arc<AppState>, admin: [u8; 32]) -> TransportPolicyView {
    let bytes = dispatch(
        state.clone(),
        admin,
        "fauna.transport.get_policy",
        encode(&GetTransportPolicyRequest {}),
    )
    .await
    .expect("get_policy ok");
    decode::<TransportPolicyView>(&bytes).expect("decode TransportPolicyView")
}

#[tokio::test]
async fn get_returns_catalog_default_before_any_override() {
    // No DB override → the view reports the catalog default; the row is the
    // cap's only other source.
    let state = fixture_state().await;
    let admin = admin_actor(&state).await;
    let view = get_view(&state, admin).await;
    assert_eq!(
        view.max_conns_per_ip,
        fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP as u32,
        "unset → catalog default (256)"
    );
}

#[tokio::test]
async fn admin_put_then_get_round_trips() {
    let state = fixture_state().await;
    let admin = admin_actor(&state).await;

    let reply_bytes = dispatch(
        state.clone(),
        admin,
        "fauna.transport.put_policy",
        encode(&PutTransportPolicyRequest {
            max_conns_per_ip: Some(64),
            ..Default::default()
        }),
    )
    .await
    .expect("put_policy ok");
    let reply: PutTransportPolicyReply =
        decode(&reply_bytes).expect("decode PutTransportPolicyReply");
    assert!(reply.ok);

    let view = get_view(&state, admin).await;
    assert_eq!(view.max_conns_per_ip, 64, "the override binds");
}

#[tokio::test]
async fn admin_put_hot_reloads_the_live_per_ip_cap() {
    // The wiring proof (distinct from the DB+view round-trip above): a
    // `put_policy` must move the **live** `serve_tls` limiter — the shared
    // `AppState::per_ip_conn_limit` Arc the accept loop holds — not just the
    // persisted row, so the new cap binds without a nest restart.
    // `transport-connection.md` § Abuse posture item (2) (hot-reload).
    let state = fixture_state().await;
    let admin = admin_actor(&state).await;

    assert_eq!(
        state.per_ip_conn_limit.max(),
        fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
        "for_test limiter starts at the catalog default (256)"
    );

    // Lower it: the live cap drops immediately.
    let reply_bytes = dispatch(
        state.clone(),
        admin,
        "fauna.transport.put_policy",
        encode(&PutTransportPolicyRequest {
            max_conns_per_ip: Some(64),
            ..Default::default()
        }),
    )
    .await
    .expect("put_policy ok");
    assert!(decode::<PutTransportPolicyReply>(&reply_bytes).unwrap().ok);
    assert_eq!(
        state.per_ip_conn_limit.max(),
        64,
        "put_policy hot-reloaded the live limiter, not just the DB row"
    );
    // The live cap and the get_policy view stay in lock-step (same resolver).
    assert_eq!(get_view(&state, admin).await.max_conns_per_ip, 64);

    // `Some(0)` means "unset" (resolve_tls_per_ip_cap drops it through to the
    // env/catalog default) — never a lock-everyone-out cap-of-zero. The live
    // limiter follows the same precedence the view reports.
    let _ = dispatch(
        state.clone(),
        admin,
        "fauna.transport.put_policy",
        encode(&PutTransportPolicyRequest {
            max_conns_per_ip: Some(0),
            ..Default::default()
        }),
    )
    .await
    .expect("put_policy ok");
    assert_eq!(
        state.per_ip_conn_limit.max(),
        fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
        "0 resolves back to the catalog default on the live limiter"
    );
    assert_eq!(
        get_view(&state, admin).await.max_conns_per_ip,
        fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP as u32,
        "view agrees the cap is the default again"
    );
}

#[tokio::test]
async fn non_admin_is_denied_put_and_get() {
    let state = fixture_state().await;
    // A non-admin actor (never added via add_admin_actor) resolves to no
    // permitted caller class for these Admin-only kinds.
    let stranger = [7u8; 32];

    let err = dispatch(
        state.clone(),
        stranger,
        "fauna.transport.put_policy",
        encode(&PutTransportPolicyRequest {
            max_conns_per_ip: Some(1),
            ..Default::default()
        }),
    )
    .await
    .expect_err("put_policy must be denied for a non-admin");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "expected a permission error, got {:?}",
        err.code
    );

    let err = dispatch(
        state.clone(),
        stranger,
        "fauna.transport.get_policy",
        encode(&GetTransportPolicyRequest {}),
    )
    .await
    .expect_err("get_policy must be denied for a non-admin");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "expected a permission error, got {:?}",
        err.code
    );
}

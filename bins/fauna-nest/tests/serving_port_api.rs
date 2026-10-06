//! tier_3 coverage for the client-set client-facing serving port
//! (`fauna.admin.set_serving_port`) — the full registered flow: an Admin-class
//! dispatch upserts the `serving_port` DB singleton and is read back on
//! `fauna.setup.status`; a non-admin is denied; port 0 is rejected; and the
//! boot reconcile (`resolve_serving_port`: DB row wins / bind-seed fallback)
//! holds. Mirrors `node_policy_api.rs` but for the **port** knob, which is
//! **apply-on-restart** (no live `AppState` swap — the nest cannot hot-rebind
//! its own `TcpListener`) and writes a `/data/serving-port` value flag (skipped
//! on this in-memory/no-data-dir test path). Goes through the router so kind
//! registration + the `bridge_method_allowlist` Admin gate are exercised
//! end-to-end. Per the product invariant that a port a human picks is
//! client-set, not CLI/config, + `nest/common.md` § Serving ports.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::db::CacheDb;
use fauna_nest::node_policy_core::resolve_serving_port;
use fauna_nest::node_policy_handlers::register_node_policy_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::node_policy::{
    DEFAULT_SERVING_PORT, SetServingPortReply, SetServingPortRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_node_policy_handlers(&mut b);
    b.build()
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

fn payload<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

/// A `for_test` AppState with a registered admin actor.
async fn state_with_admin(admin: [u8; 32]) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    state.db.add_admin_actor(&admin).await.unwrap();
    state
}

#[tokio::test]
async fn admin_set_serving_port_persists_and_surfaces() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    // Unset ⇒ setup.status reports the hard-coded default (443), not the internal
    // bind seed.
    let status = fauna_nest::discovery_core::setup_status_core(&state, true).await;
    assert_eq!(status.serving_port, DEFAULT_SERVING_PORT);
    assert_eq!(state.db.get_serving_port().await.unwrap(), None);
    // This binary sets no `FAUNA_FRONTED_BY_ROUTER` ⇒ a direct-listener nest, so
    // setup.status reports the port as admin-settable (the client keeps the
    // `admin-nest-serving-port` field editable). The fronted=true case lives in
    // `serving_port_fronted.rs` (its own binary, since the signal is a process env).
    assert!(!status.fronted_by_router);

    // The cycle exercises a non-default port and a re-set (settable both ways).
    for port in [8443u16, 443, 3443] {
        let reply_bytes = dispatch(
            &r,
            state.clone(),
            "fauna.admin.set_serving_port",
            admin,
            payload(&SetServingPortRequest {
                port,
                extra: Default::default(),
            }),
        )
        .await
        .expect("admin set ok");
        let reply: SetServingPortReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);

        // (1) DB singleton persisted, (2) setup.status surfaces it for the admin
        // client to read back (no live AppState swap — apply-on-restart).
        assert_eq!(state.db.get_serving_port().await.unwrap(), Some(port));
        let status = fauna_nest::discovery_core::setup_status_core(&state, true).await;
        assert_eq!(status.serving_port, port);
    }
}

#[tokio::test]
async fn port_zero_is_rejected() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_serving_port",
        admin,
        payload(&SetServingPortRequest {
            port: 0,
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
    // No row written on a rejected set.
    assert_eq!(state.db.get_serving_port().await.unwrap(), None);
}

#[tokio::test]
async fn non_admin_denied() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;
    let stranger = [99u8; 32];

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_serving_port",
        stranger,
        payload(&SetServingPortRequest {
            port: 8443,
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.admin.permission_denied");
    // No row written on a denied set.
    assert_eq!(state.db.get_serving_port().await.unwrap(), None);
}

#[tokio::test]
async fn boot_reconcile_db_row_wins_else_seed() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    // No row ⇒ the bind seed port passes through (whatever the artifact bound).
    assert_eq!(resolve_serving_port(&db, 443).await, 443);
    assert_eq!(resolve_serving_port(&db, 3000).await, 3000);

    // A client-set row wins over the seed.
    db.set_serving_port(8443).await.unwrap();
    assert_eq!(resolve_serving_port(&db, 443).await, 8443);
    assert_eq!(resolve_serving_port(&db, 3000).await, 8443);
}

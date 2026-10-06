//! tier_3 coverage for **decision D** (2026-06-23): `fauna.admin.set_serving_port`
//! is **rejected on a router-fronted (Docker) deployment** — the external
//! client-facing port there is realized by the `fauna-sni-router` + compose
//! port-map, never nest's own bind, so a chosen `serving_port` can never apply;
//! accepting it would be config theatre. The serving port is a direct-listener
//! (desktop / self-hosted / bare-IP) admin choice only. Spec: `nest/common.md`
//! § Serving ports.
//!
//! This lives in its **own** test binary because the fronted signal is the
//! process-global `FAUNA_FRONTED_BY_ROUTER` env (read by
//! `is_fronted_by_router()`); a dedicated binary keeps the env-set from
//! affecting the non-fronted round-trip cases in `serving_port_api.rs` (which
//! run in a separate process). One test only, for the same reason.

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::db::CacheDb;
use fauna_nest::discovery_handlers::register_discovery_handlers;
use fauna_nest::node_policy_handlers::register_node_policy_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};
use fauna_protocol::node_policy::SetServingPortRequest;
use fauna_protocol::{decode_strict as decode, encode_canonical};

#[tokio::test]
async fn set_serving_port_rejected_when_fronted_by_router() {
    // SAFETY: this is the only test in this binary, so no other test thread
    // observes the process-global env mutation (edition 2024 marks set_var
    // unsafe precisely because of cross-thread visibility).
    unsafe {
        std::env::set_var("FAUNA_FRONTED_BY_ROUTER", "1");
    }

    let admin = [7u8; 32];
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    state.db.add_admin_actor(&admin).await.unwrap();

    let mut b = RpcRouter::builder();
    register_node_policy_handlers(&mut b);
    register_discovery_handlers(&mut b);
    let router = b.build();

    let payload = Bytes::from(
        encode_canonical(&SetServingPortRequest {
            port: 8443,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );

    let meta = router
        .kind_meta("fauna.admin.set_serving_port")
        .expect("kind registered");
    let err = (meta.handler)(state.clone(), admin, payload)
        .await
        .expect_err("a router-fronted nest must reject set_serving_port");

    assert_eq!(
        err.code, "fauna.node_policy.serving_port_fronted",
        "expected the fronted-rejection code, got {err:?}"
    );

    // The DB singleton must remain unset (the inert value was never persisted) —
    // a later `setup.status` read then falls back to DEFAULT_SERVING_PORT (443).
    assert!(
        state.db.get_serving_port().await.unwrap().is_none(),
        "a rejected set_serving_port must not persist the inert value"
    );

    // And `fauna.setup.status` advertises the deployment as router-fronted, so the
    // admin client renders the `admin-nest-serving-port` field read-only (rather
    // than offering the save the handler above just rejected). End-to-end:
    // `FAUNA_FRONTED_BY_ROUTER` (set above) → `is_fronted_by_router()` →
    // `setup_status_core` → the `SetupStatusReply` wire flag.
    let status_meta = router
        .kind_meta("fauna.setup.status")
        .expect("setup.status registered");
    let reply_bytes = (status_meta.handler)(
        state.clone(),
        admin,
        Bytes::from(
            encode_canonical(&SetupStatusRequest {
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("setup.status is an infallible read");
    let reply: SetupStatusReply = decode(&reply_bytes).unwrap();
    assert!(
        reply.fronted_by_router,
        "a router-fronted nest must advertise fronted_by_router=true on setup.status"
    );
    // The inert value falls back to the hard-coded default (443) — the value the
    // read-only field displays.
    assert_eq!(
        reply.serving_port,
        fauna_protocol::node_policy::DEFAULT_SERVING_PORT
    );
}

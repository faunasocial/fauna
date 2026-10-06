//! Integration — the **native anonymous WS-RPC connector**
//! (`fauna_anon_client::AnonymousNestClient`) round-trips a pre-identity kind
//! against a real nest over `GET /api/v1/ws` (no bearer). This is the FIRST
//! client consumer of the anonymous connection (transport.md § Pre-identity;
//! transport.md:655 "No client consumes the anonymous connection yet").
//! Socket-level nest plumbing is proven by `pre_identity_ws.rs`; this proves
//! the *client* side end-to-end: connector → dispatcher → real reply decode.
//! Slice tracked internally (S1).

use std::sync::Arc;

use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::discovery::{
    NestInfoReply, NestInfoRequest, SetupStatusReply, SetupStatusRequest,
};

/// Spin an in-process nest serving the anonymous endpoint with the discovery
/// kinds registered (allowlisted in `pre_identity_allowlist.rs`). Returns the
/// `http://` base URL; the connector does the http→ws scheme swap.
async fn start() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn anonymous_client_nest_info_round_trips() {
    let base = start().await;
    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("open anonymous connection");

    let reply: NestInfoReply = client
        .request("fauna.nest.info", NestInfoRequest::default())
        .await
        .expect("nest.info should resolve over the anonymous connection");

    assert_eq!(reply.software, "fauna");
    assert_eq!(reply.nest_id.len(), 64);
}

#[tokio::test]
async fn anonymous_client_setup_status_round_trips() {
    let base = start().await;
    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("open anonymous connection");

    let reply: SetupStatusReply = client
        .request("fauna.setup.status", SetupStatusRequest::default())
        .await
        .expect("setup.status should resolve over the anonymous connection");

    // Fresh in-memory nest: no user account exists yet. (`claimed` depends on
    // the on-disk claim-code file, which `for_test` does not provision — not a
    // useful "freshness" signal here.) The decode succeeding + a real value is
    // the round-trip assertion; `admin_exists` pins it to a real reply.
    assert!(!reply.admin_exists, "fresh nest should report no admin");
    // The anonymous `setup.status` version is coarsened to the `major.minor`
    // line (anti-fingerprinting, spec § 8.1) — it is the major.minor *prefix* of
    // the precise build version, never the exact patch-level `CARGO_PKG_VERSION`.
    let coarsened = concat!(
        env!("CARGO_PKG_VERSION_MAJOR"),
        ".",
        env!("CARGO_PKG_VERSION_MINOR")
    );
    assert_eq!(reply.version, coarsened);
    assert!(env!("CARGO_PKG_VERSION").starts_with(coarsened));
}

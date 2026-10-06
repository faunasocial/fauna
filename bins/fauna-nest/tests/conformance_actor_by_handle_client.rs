//! Real-wire (tier_3) proof of the conversations address-resolution adapter.
//!
//! This is the **first** test that connects an *authenticated* `NestClient`
//! over a real TCP socket to an in-process nest and drives the conversations
//! client surface end-to-end. Existing conversations coverage is either tier_1
//! (`fauna_mls_backend_tests.rs` against a MockNest) or in-process kind dispatch
//! (`conformance_discovery.rs`); neither exercises the cross-binary
//! request/reply mapping the production client actually performs.
//!
//! The flow under test (the FaunaMls handle→actor slice closed):
//!
//! ```text
//! FaunaMlsBackend::resolve_address("bobhandle")        (fauna-conversations)
//!   → ConversationsRpc::actor_by_handle                (seam)
//!   → NestConversationsRpc → ConversationsClient        (fauna-client-conversations)
//!   → fauna.actor.by_handle  over the WS               (real socket → fauna-nest)
//!   → reply {actor_id, domain}
//!   → resolve_reachable → fauna.conversations.keypackage.count > 0
//!   → ResolveResult::Resolved(TypedAddress::Fauna { actor_id })
//! ```
//!
//! What only this real-wire test can catch (vs. tier_1's MockNest): the
//! `ActorByHandleRequest`/`Reply` and `KeypackageCount*` CBOR encode/decode over
//! the dispatcher, the bearer auth (challenge-free `fauna.auth.handshake` mint +
//! subprotocol WS upgrade), and — critically — the adapter's
//! `fauna.actor.not_found` → `Ok(None)` → `NotFound` mapping, which a catch-all
//! mock cannot reproduce (tracked internally — why the Python e2e harness
//! structurally couldn't prove this). Goal doc: `docs/goal/ui/conversations.md` § Where logic lives
//! ("Address parsing / resolution … Fauna→Mastodon→Email disambiguation").

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_conversations::NestConversationsRpc;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::backend::{ConversationsRpc, RailBackend, ResolveResult};
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

/// Spin a real in-process nest serving the discovery + conversations WS-RPC
/// kinds on `127.0.0.1:0`. Returns its HTTP base URL (the WS adapter rewrites
/// `http→ws`) and the shared `AppState` so the test can seed `state.db`
/// directly. `require_registration` is on, matching production, so the
/// connecting actor must be a registered user.
async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            // The authenticated `NestClient` mints its bearer over the
            // pre-identity `fauna.auth.handshake` (WsChallengeBearer), so the
            // anon endpoint must serve the auth-bootstrap kinds.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            // `fauna.actor.by_handle` lives in discovery; the keypackage.count
            // probe + the rest of `fauna.conversations.*` in conversations.
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: tokens,
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..fauna_nest::routes::AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (format!("http://127.0.0.1:{}", addr.port()), state)
}

/// Seed a registered, handle-owning peer with `n` real (TLS-serialized) MLS key
/// packages so the `keypackage.count > 0` reachability probe promotes the
/// resolved handle to a Fauna address. The bytes are minted by the peer's own
/// `MlsEngine` (the production source), though only the row count matters to the
/// probe under test. Takes the keypair by value (`MlsEngine` consumes it); the
/// caller captures `actor_id()` beforehand.
async fn seed_peer_with_keypackages(db: &CacheDb, actor: ActorKeypair, handle: &str, n: usize) {
    let actor_id = actor.actor_id();
    db.create_user_with_handle(&actor_id.0, "free", handle, None)
        .await
        .expect("create handled peer");

    let engine = MlsEngine::new_in_memory(actor).expect("peer MLS engine");
    let packages = engine
        .generate_key_packages_bytes(n)
        .expect("mint key packages");
    let far_future = u64::MAX / 2; // never expires within the test
    for (i, pkg) in packages.into_iter().enumerate() {
        db.put_key_package(&format!("kp-{i}"), &actor_id.0, &pkg, 0, far_future)
            .await
            .expect("store key package");
    }
}

/// Build the production resolution stack (real `NestConversationsRpc` over the
/// connected client) for the local actor. The local `MlsEngine` is required by
/// `FaunaMlsBackend::new` but unused by `resolve_address`, so a throwaway
/// in-memory engine identity suffices; `self_actor` is the real local actor id
/// (sender-stamping metadata only).
fn fauna_mls_backend(nest: Arc<NestClient>, self_actor: ActorId) -> FaunaMlsBackend {
    let rpc: Arc<dyn ConversationsRpc> = Arc::new(NestConversationsRpc::new(nest));
    let engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).expect("local MLS engine"));
    FaunaMlsBackend::new(engine, rpc, "alice", self_actor)
}

fn assert_resolved_to(result: ResolveResult, expected: ActorId) {
    match result {
        ResolveResult::Resolved(TypedAddress::Fauna { actor_id, .. }) => {
            assert_eq!(actor_id, expected, "resolved to the wrong actor");
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_handle_over_real_wire() {
    let (base, state) = start_server().await;

    // alice = the connecting (resolving) actor; must be registered for the
    // `fauna.auth.handshake` bearer mint + WS upgrade to succeed under
    // `require_registration`.
    let alice = ActorKeypair::generate();
    let alice_id = alice.actor_id();
    state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    // bob = the peer alice resolves by handle; registered + reachable.
    let bob = ActorKeypair::generate();
    let bob_id = bob.actor_id();
    seed_peer_with_keypackages(&state.db, bob, "bobhandle", 2).await;

    let nest = connected_client(&base, alice).await;
    let backend = fauna_mls_backend(nest, alice_id);

    // 1. Bare handle resolves to bob over the wire (by_handle → count probe).
    assert_resolved_to(backend.resolve_address("bobhandle").await, bob_id);

    // 2. Unknown handle: nest replies `fauna.actor.not_found`, the adapter maps
    //    it to `Ok(None)`, and the backend reports NotFound (the manager's chain
    //    falls through to the other rails). A catch-all mock can't reproduce
    //    this — it's the single most important cross-binary contract here.
    assert_eq!(
        backend.resolve_address("nobody").await,
        ResolveResult::NotFound,
    );

    // 3. Actor-id form regression: a 64-hex string IS the actor key, so it
    //    skips by_handle and goes straight to the count probe.
    assert_resolved_to(
        backend.resolve_address(&hex::encode(bob_id.0)).await,
        bob_id,
    );
}

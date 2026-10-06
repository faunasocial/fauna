//! **Post-delete propagation to the paired public nest — tier_3 capstone.** The
//! delete twin of `conformance_cross_nest_post_forward.rs`: two in-process nests
//! over the federation WS-RPC channel: a public relay nest `H` and a paired
//! private home nest `P`. A post created on `P` is forwarded to `H`
//! (`fauna.federation.post.forward`); then the author deletes it, `P` queues the
//! signed tombstone, `P`'s outbox worker drains it over
//! `fauna.federation.post.delete`, and `H` re-verifies the tombstone's author
//! envelope, checks the pairing's `post_forward` capability, and removes the
//! post through the shared `delete_post_core` — so the forwarded copy does not
//! outlive the original.
//!
//! Carrier = channel only. Harness mirrors
//! `conformance_cross_nest_post_forward.rs`: `for_test`'s routers are empty, so
//! the test registers the federation handlers (serving side) and the anon
//! discovery handlers (the pool resolves a peer's `nest_id` from its URL via
//! `fauna.nest.info` before dialing).
//!
//! Goal: `docs/goal/ui/feed.md` § State & data shape → *Post deletion* →
//! Propagation (the forwarded-post outbox twin — a deleted post must not
//! outlive its derivations).

mod common;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest::config::SubmissionPolicy;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::outbox::{DrainOutcome, drain_outbox_once};
use fauna_nest::routes::AppState;

async fn start_nest(policy: SubmissionPolicy) -> (String, Arc<AppState>) {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let state = AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        config: common::test_config(policy),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(Arc::new(CacheDb::open_in_memory().unwrap()))
    };
    let state = Arc::new(state);
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state)
}

async fn stored_body(state: &Arc<AppState>, post_id: &[u8; 32]) -> Option<Vec<u8>> {
    fauna_nest::segments::post::load_post_body(&state.post_segments, &state.db, post_id)
        .await
        .unwrap()
}

/// Store the pairing that lets `author`'s posts (and deletes) arrive from `P`.
async fn authorize(h_state: &Arc<AppState>, author: &ActorKeypair, p_nest_id: &[u8; 32]) {
    h_state
        .db
        .store_pairing(
            &author.actor_id().0,
            p_nest_id,
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
}

/// Forward a post `P`→`H` so `H` holds it before the delete test runs.
async fn forward_post(
    p_state: &Arc<AppState>,
    h_url: &str,
    author: &ActorKeypair,
    wire_bytes: &[u8],
    post_id: &[u8; 32],
    h_state: &Arc<AppState>,
) {
    // P is the author's private home nest, and the author's own row there
    // forwards to H — what `fauna.pair.add` stores; no config-file switch.
    *p_state.node_mode.write().await = fauna_nest::config::NodeMode::Private;
    p_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &h_state.nest_identity.public_key_bytes(),
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            Some(h_url),
            None,
        )
        .await
        .unwrap();
    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, wire_bytes, "forwarded_post")
        .await
        .unwrap();
    assert_eq!(
        drain_outbox_once(p_state).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 0
        },
        "precondition: the post forwards to H"
    );
    assert!(
        stored_body(h_state, post_id).await.is_some(),
        "precondition: H holds the forwarded post"
    );
}

/// **The capstone.** A forwarded post's author-signed deletion drains
/// `P`→`H` over `fauna.federation.post.delete`, and `H` removes the post — the
/// wire-level proof that a deleted post does not outlive its forwarded copy.
#[tokio::test]
async fn queued_delete_removes_the_forwarded_post_on_the_public_nest() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x11u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "delete me");
    authorize(&h_state, &author, &p_nest_id).await;
    forward_post(&p_state, &h_url, &author, &wire_bytes, &post_id, &h_state).await;

    // The author deletes: P queues the signed tombstone, exactly as
    // `maybe_enqueue_delete_outbox` does, and one drain relays it to H.
    let tombstone_wire = common::signed_tombstone_wire(&author, post_id);
    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &tombstone_wire, "forwarded_delete")
        .await
        .unwrap();

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 0
        },
        "the queued deletion relayed to H"
    );

    assert_eq!(
        stored_body(&h_state, &post_id).await,
        None,
        "H removed the forwarded post — it no longer outlives the original"
    );
    assert_eq!(
        p_state.db.outbox_depth().await.unwrap(),
        0,
        "a relayed deletion leaves the queue outright"
    );
}

/// **Idempotent on replay.** `delete_post_core` returns `AlreadyGone` for a
/// re-delivered tombstone, so a re-drain (a crash between peer-accept and
/// `outbox_mark_sent`) is a no-op, not an error — matching the forward leg's
/// content-addressed idempotency.
#[tokio::test]
async fn re_delivering_the_same_delete_is_a_no_op() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x22u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "delete twice");
    authorize(&h_state, &author, &p_nest_id).await;
    forward_post(&p_state, &h_url, &author, &wire_bytes, &post_id, &h_state).await;

    let tombstone_wire = common::signed_tombstone_wire(&author, post_id);
    for _ in 0..2 {
        p_state
            .db
            .outbox_enqueue(&author.actor_id().0, &tombstone_wire, "forwarded_delete")
            .await
            .unwrap();
        assert_eq!(
            drain_outbox_once(&p_state).await,
            DrainOutcome::Processed {
                forwarded: 1,
                failed: 0
            },
            "each delete drain succeeds — the second is AlreadyGone, not an error"
        );
    }

    assert_eq!(
        stored_body(&h_state, &post_id).await,
        None,
        "H stays clear of the post after a redelivered delete"
    );
}

/// **Capability gate over the wire.** A pairing that lacks `post_forward` cannot
/// relay a deletion: under `PairedOnly` the receive handler enforces exactly the
/// `post_forward` capability (the sibling of the forward handler's gate), so an
/// unauthorized delete is refused and the entry is retried — the forwarded post
/// survives on H.
#[tokio::test]
async fn delete_without_capability_is_refused_and_the_post_survives() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x33u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "protected");
    // Authorize the forward so H holds the post, then REVOKE before the delete
    // by re-storing the pairing with no capabilities.
    authorize(&h_state, &author, &p_nest_id).await;
    forward_post(&p_state, &h_url, &author, &wire_bytes, &post_id, &h_state).await;
    h_state
        .db
        .store_pairing(&author.actor_id().0, &p_nest_id, &[], None, None, None)
        .await
        .unwrap();

    let tombstone_wire = common::signed_tombstone_wire(&author, post_id);
    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &tombstone_wire, "forwarded_delete")
        .await
        .unwrap();

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 0,
            failed: 1
        },
        "the unauthorized deletion is refused and kept for retry"
    );
    assert!(
        stored_body(&h_state, &post_id).await.is_some(),
        "the forwarded post survives an unauthorized delete"
    );
}

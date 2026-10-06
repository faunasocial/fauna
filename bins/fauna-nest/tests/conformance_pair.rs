//! Integration round-trips for the user-facing per-user-multi-homing ("Linked
//! nests") surface: `fauna.pair.{list,add,revoke}`. Exercises the WS-RPC layer:
//! owner-implicit scope (the connection actor scopes the read/write — a user can
//! only see/pair their own account), reply encoding, the admin `pairing`
//! service-knob gate on `add`, and label/nest_url persistence round-tripping
//! through `fauna.pair.list`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/pair.rs`. Design
//! tracked internally (per-user nest pairing, 2026-05-25).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    db::CacheDb,
    pair_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    services::{ServiceIntent, services_json_path},
};
use fauna_protocol::{
    ByteBuf, RpcError, decode_strict as decode, encode_canonical,
    pair::{
        PairAddReply, PairAddRequest, PairListReply, PairListRequest, PairRevokeReply,
        PairRevokeRequest, default_self_sync,
    },
};

const ACTOR_A: [u8; 32] = [11u8; 32];
const ACTOR_B: [u8; 32] = [22u8; 32];
const NEST_1: [u8; 32] = [0xa1u8; 32];
const NEST_2: [u8; 32] = [0xa2u8; 32];

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    pair_handlers::register_pair_handlers(&mut b);
    (b.build(), state)
}

/// Same, but with an isolated `services_json_path` under `dir` so the
/// admin-knob tests can write their own `services.json` without colliding
/// with the per-PID shared default (mirrors `conformance_admin`).
async fn router_and_state_with_services_dir(dir: &std::path::Path) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    state.services_json_path = services_json_path(&dir.join("nest.db").to_string_lossy());
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    pair_handlers::register_pair_handlers(&mut b);
    (b.build(), state)
}

async fn list(router: &RpcRouter, state: Arc<AppState>, actor: [u8; 32]) -> PairListReply {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta("fauna.pair.list")
        .expect("kind registered");
    let payload = Bytes::from(
        encode_canonical(&PairListRequest {
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload)
        .await
        .expect("list ok");
    decode(&bytes).unwrap()
}

async fn add(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    req: PairAddRequest,
) -> Result<PairAddReply, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta("fauna.pair.add").expect("kind registered");
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode(&bytes).unwrap())
}

async fn revoke(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    nest_id: [u8; 32],
) -> PairRevokeReply {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta("fauna.pair.revoke")
        .expect("kind registered");
    let payload = Bytes::from(
        encode_canonical(&PairRevokeRequest {
            private_nest_id: ByteBuf::from(nest_id.to_vec()),
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload)
        .await
        .expect("revoke ok");
    decode(&bytes).unwrap()
}

fn add_req(nest_id: [u8; 32]) -> PairAddRequest {
    PairAddRequest {
        private_nest_id: ByteBuf::from(nest_id.to_vec()),
        capabilities: vec![],
        expires_at: None,
        label: None,
        nest_url: None,
        extra: Default::default(),
    }
}

#[tokio::test]
async fn list_returns_only_bearer_pairings() {
    let (router, state) = router_and_state().await;
    state
        .db
        .store_pairing(
            &ACTOR_A,
            &NEST_1,
            &default_self_sync(),
            // Far-future expiry — `list_pairings_for_actor` filters out
            // expired rows (`expires_at > Timestamp::now()`, microseconds),
            // so this must be unexpired.
            Some(i64::MAX),
            Some("https://nest1.example"),
            Some("home NAS"),
        )
        .await
        .unwrap();
    // A second actor's pairing must not leak into A's list.
    state
        .db
        .store_pairing(&ACTOR_B, &NEST_2, &default_self_sync(), None, None, None)
        .await
        .unwrap();

    let reply = list(&router, state.clone(), ACTOR_A).await;
    assert_eq!(reply.pairings.len(), 1);
    let row = &reply.pairings[0];
    assert_eq!(row.private_nest_id.as_ref(), &NEST_1[..]);
    assert_eq!(row.capabilities, default_self_sync());
    assert_eq!(row.expires_at, Some(i64::MAX));
    assert_eq!(row.label.as_deref(), Some("home NAS"));
    assert_eq!(row.nest_url.as_deref(), Some("https://nest1.example"));
}

#[tokio::test]
async fn list_empty_for_actor_with_no_pairings() {
    let (router, state) = router_and_state().await;
    let reply = list(&router, state, ACTOR_A).await;
    assert!(reply.pairings.is_empty());
}

#[tokio::test]
async fn add_then_list_then_revoke_round_trips() {
    let (router, state) = router_and_state().await;

    // Add with an explicit label + nest_url; empty caps → full self-sync.
    let reply = add(
        &router,
        state.clone(),
        ACTOR_A,
        PairAddRequest {
            private_nest_id: ByteBuf::from(NEST_1.to_vec()),
            capabilities: vec![],
            expires_at: None,
            label: Some("home NAS".to_string()),
            nest_url: Some("https://nest1.example".to_string()),
            extra: Default::default(),
        },
    )
    .await
    .expect("add ok");
    assert!(reply.ok);

    // List shows it, with the canonical default capabilities + label + url.
    let listed = list(&router, state.clone(), ACTOR_A).await;
    assert_eq!(listed.pairings.len(), 1);
    let row = &listed.pairings[0];
    assert_eq!(row.private_nest_id.as_ref(), &NEST_1[..]);
    assert_eq!(row.capabilities, default_self_sync());
    assert_eq!(row.label.as_deref(), Some("home NAS"));
    assert_eq!(row.nest_url.as_deref(), Some("https://nest1.example"));

    // Revoke removes it.
    let rev = revoke(&router, state.clone(), ACTOR_A, NEST_1).await;
    assert!(rev.ok);
    let after = list(&router, state, ACTOR_A).await;
    assert!(after.pairings.is_empty());
}

#[tokio::test]
async fn add_is_owner_scoped() {
    // A's add must not appear in B's list — the connection actor scopes the write.
    let (router, state) = router_and_state().await;
    add(&router, state.clone(), ACTOR_A, add_req(NEST_1))
        .await
        .expect("add ok");
    let b_list = list(&router, state, ACTOR_B).await;
    assert!(b_list.pairings.is_empty());
}

#[tokio::test]
async fn add_rejected_when_admin_knob_off() {
    let dir = tempfile::tempdir().unwrap();
    let (router, state) = router_and_state_with_services_dir(dir.path()).await;

    // Admin disables pairing nest-wide.
    let mut intent = ServiceIntent::default();
    intent.services.pairing = false;
    intent.write_to(&state.services_json_path).unwrap();

    let err = add(&router, state.clone(), ACTOR_A, add_req(NEST_1))
        .await
        .expect_err("add must be rejected when pairing disabled");
    assert_eq!(err.code, "fauna.pair.pairing_disabled");

    // Nothing was stored.
    let listed = list(&router, state, ACTOR_A).await;
    assert!(listed.pairings.is_empty());
}

#[tokio::test]
async fn add_allowed_when_admin_knob_on() {
    let dir = tempfile::tempdir().unwrap();
    let (router, state) = router_and_state_with_services_dir(dir.path()).await;

    // Default services.json has pairing on.
    ServiceIntent::default()
        .write_to(&state.services_json_path)
        .unwrap();

    add(&router, state.clone(), ACTOR_A, add_req(NEST_1))
        .await
        .expect("add ok with knob on");
    let listed = list(&router, state, ACTOR_A).await;
    assert_eq!(listed.pairings.len(), 1);
}

/// **`pair.add` re-arms the caller's queued forwards — and only theirs.** The
/// one-action link adds the relay's pairing row at the same moment, which is
/// the grant a refused forward waits on, so the caller's backed-off entries
/// retry now instead of at the end of their backoff (`private-mode.md`
/// § Post Forwarding: a nudge, never the mechanism).
#[tokio::test]
async fn pair_add_makes_the_callers_backed_off_forwards_due_now() {
    let (router, state) = router_and_state().await;
    for actor in [ACTOR_A, ACTOR_B] {
        state
            .db
            .outbox_enqueue(&actor, b"queued-post", "forwarded_post")
            .await
            .unwrap();
    }
    // Five refusals each: backed off 30 s × 2^4 = 8 minutes.
    for entry in state.db.outbox_pending(10).await.unwrap() {
        for _ in 0..5 {
            state
                .db
                .outbox_record_failure(entry.id, false, "refused")
                .await
                .unwrap();
        }
    }
    assert!(
        state.db.outbox_pending(10).await.unwrap().is_empty(),
        "both entries are backed off after a refusal"
    );
    // Past the re-arm's gap since the refusal, so the nudge can make it due.
    for actor in [ACTOR_A, ACTOR_B] {
        state
            .db
            .test_age_outbox_failures(&actor, CacheDb::OUTBOX_REARM_MIN_GAP_SECS)
            .await
            .unwrap();
    }

    add(&router, state.clone(), ACTOR_A, add_req(NEST_1))
        .await
        .expect("add ok");

    let due: Vec<i64> = state
        .db
        .outbox_pending(10)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.attempts)
        .collect();
    assert_eq!(
        due,
        vec![5],
        "the caller's entry is due again (its failures still counted); the other \
         user's stays backed off"
    );
    assert_eq!(state.db.outbox_depth().await.unwrap(), 2);
}

// ── The forward queue on the Nests page (`private-mode.md` § Post Forwarding →
// the queue is the user's to see; 2026-09-23) ──────────────────────────────

use fauna_protocol::pair::{
    PairForwardDiscardReply, PairForwardDiscardRequest, PairForwardRetryReply,
    PairForwardRetryRequest,
};

async fn forward_retry(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> PairForwardRetryReply {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta("fauna.pair.forward_retry")
        .expect("kind registered");
    let payload = Bytes::from(
        encode_canonical(&PairForwardRetryRequest {
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload)
        .await
        .expect("retry ok");
    decode(&bytes).unwrap()
}

async fn forward_discard(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> PairForwardDiscardReply {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta("fauna.pair.forward_discard")
        .expect("kind registered");
    let payload = Bytes::from(
        encode_canonical(&PairForwardDiscardRequest {
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload)
        .await
        .expect("discard ok");
    decode(&bytes).unwrap()
}

/// Queue `n` forwards for `actor` and fail each once with `error`, so they sit
/// backed off — the state a refused post is in between retries.
async fn queue_refused(state: &Arc<AppState>, actor: [u8; 32], n: usize, error: &str) {
    for i in 0..n {
        state
            .db
            .outbox_enqueue(&actor, format!("post-{i}").as_bytes(), "forwarded_post")
            .await
            .unwrap();
    }
    for entry in state.db.outbox_pending(50).await.unwrap() {
        state
            .db
            .outbox_record_failure(entry.id, false, error)
            .await
            .unwrap();
    }
}

/// The list reply carries the CALLER's queue: how many of their own posts are
/// waiting, how many are stuck past the ceiling, and the most recent failure —
/// never another user's rows. An empty queue reads as zeros with no reason,
/// not as an absent field (apps show nothing for an absent or a zero field alike).
#[tokio::test]
async fn list_carries_the_callers_own_forward_queue_and_nobody_elses() {
    let (router, state) = router_and_state().await;
    queue_refused(&state, ACTOR_A, 2, "nest not paired for this actor").await;
    queue_refused(&state, ACTOR_B, 5, "other user's refusal").await;

    let reply = list(&router, state.clone(), ACTOR_A).await;
    let queue = reply.forward_queue;
    assert_eq!(queue.queued, 2, "A's two, not B's five");
    assert_eq!(queue.stuck, 0, "one failure is nowhere near the ceiling");
    assert_eq!(
        queue.last_error.as_deref(),
        Some("nest not paired for this actor"),
        "the WHY rides along — A's own, not B's"
    );

    // Past the ceiling the same entries count as stuck.
    for _ in 0..CacheDb::OUTBOX_MAX_ATTEMPTS {
        state
            .db
            .test_age_outbox_failures(&ACTOR_A, CacheDb::OUTBOX_REARM_MIN_GAP_SECS)
            .await
            .unwrap();
        state
            .db
            .outbox_retry_now_for_author(&ACTOR_A)
            .await
            .unwrap();
        for entry in state.db.outbox_pending(50).await.unwrap() {
            state
                .db
                .outbox_record_failure(entry.id, false, "still refused")
                .await
                .unwrap();
        }
    }
    let queue = list(&router, state.clone(), ACTOR_A).await.forward_queue;
    assert_eq!((queue.queued, queue.stuck), (2, 2));
    assert_eq!(queue.last_error.as_deref(), Some("still refused"));

    // A user with nothing queued: zeros, no reason — and B's rows untouched.
    let (router2, state2) = router_and_state().await;
    let queue = list(&router2, state2, ACTOR_A).await.forward_queue;
    assert_eq!((queue.queued, queue.stuck, queue.last_error), (0, 0, None));
    assert_eq!(state.db.outbox_depth().await.unwrap(), 7);
}

/// `forward_retry` makes the caller's backed-off entries due now — the nudge
/// for a user who granted the capability on the relay's own side — and leaves
/// every other user's backoff running. The reply counts what it re-armed.
#[tokio::test]
async fn forward_retry_re_arms_only_the_callers_entries() {
    let (router, state) = router_and_state().await;
    // Five refusals each: backed off 30 s × 2^4 = 8 minutes.
    for (actor, n) in [(ACTOR_A, 2), (ACTOR_B, 1)] {
        for i in 0..n {
            state
                .db
                .outbox_enqueue(&actor, format!("post-{i}").as_bytes(), "forwarded_post")
                .await
                .unwrap();
        }
    }
    for entry in state.db.outbox_pending(50).await.unwrap() {
        for _ in 0..5 {
            state
                .db
                .outbox_record_failure(entry.id, false, "refused")
                .await
                .unwrap();
        }
    }
    assert!(state.db.outbox_pending(50).await.unwrap().is_empty());

    let reply = forward_retry(&router, state.clone(), ACTOR_A).await;
    assert_eq!(reply.rearmed, 2);
    assert!(
        state.db.outbox_pending(50).await.unwrap().is_empty(),
        "refused just now: pulled in to the re-arm's gap, not yet due"
    );
    assert_eq!(
        forward_retry(&router, state.clone(), ACTOR_A).await.rearmed,
        0,
        "a second re-arm inside the gap pulls nothing further in"
    );

    for actor in [ACTOR_A, ACTOR_B] {
        state
            .db
            .test_age_outbox_failures(&actor, CacheDb::OUTBOX_REARM_MIN_GAP_SECS)
            .await
            .unwrap();
    }
    let due = state.db.outbox_pending(50).await.unwrap();
    assert_eq!(
        due.len(),
        2,
        "the gap passed: A's two are due; B's one stays backed off"
    );
    assert!(
        due.iter().all(|e| e.attempts == 5),
        "the failures still count"
    );
}

/// A relay answering with an over-long, control-laden refusal code leaves a
/// bounded, control-stripped reason on the queue, and `fauna.pair.list` stays
/// far under the 2 MiB WS-RPC message cap (`private-mode.md` § Post
/// Forwarding).
#[tokio::test]
async fn list_serves_a_bounded_reason_however_long_the_relays_refusal() {
    let (router, state) = router_and_state().await;
    let hostile = format!(
        "peer post.forward: \u{7}{}",
        "r".repeat(fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE)
    );
    queue_refused(&state, ACTOR_A, 3, &hostile).await;

    let reply = list(&router, state.clone(), ACTOR_A).await;
    let encoded = encode_canonical(&reply).unwrap();
    assert!(
        encoded.len() < 4096,
        "a {} byte list reply for one refusal",
        encoded.len()
    );
    let reason = reply.forward_queue.last_error.unwrap();
    assert!(reason.chars().count() <= CacheDb::OUTBOX_LAST_ERROR_MAX_CHARS + 1);
    assert!(reason.starts_with("peer post.forward: rrr"), "{reason:.40}");
    assert!(!reason.chars().any(char::is_control));
}

/// `forward_discard` drops every entry the caller queued and nothing else; the
/// list then reads empty for them while the other user's queue is intact.
#[tokio::test]
async fn forward_discard_drops_only_the_callers_entries() {
    let (router, state) = router_and_state().await;
    queue_refused(&state, ACTOR_A, 3, "refused").await;
    queue_refused(&state, ACTOR_B, 1, "refused").await;

    let reply = forward_discard(&router, state.clone(), ACTOR_A).await;
    assert_eq!(reply.discarded, 3);

    let queue = list(&router, state.clone(), ACTOR_A).await.forward_queue;
    assert_eq!((queue.queued, queue.stuck, queue.last_error), (0, 0, None));
    assert_eq!(
        state.db.outbox_depth().await.unwrap(),
        1,
        "B's row survives"
    );

    // Idempotent: a second discard has nothing to drop and says so.
    let reply = forward_discard(&router, state.clone(), ACTOR_A).await;
    assert_eq!(reply.discarded, 0);
}

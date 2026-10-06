//! **Post-forwarding origination (private → public) — tier_3 capstone.** Two
//! in-process nests over the federation WS-RPC channel: a public relay nest `H`
//! and a paired private home nest `P`. A post created on `P` lands in `P`'s
//! outbox; `P`'s outbox worker drains it over `fauna.federation.post.forward`;
//! `H` verifies the *author's* sign-over-CID envelope, checks the pairing's
//! `post_forward` capability, and stores the post under the same
//! content-addressed `post_id` `P` derived.
//!
//! Before this suite the origination half had **never** run: the worker POSTed
//! the HTTP route `/api/v1/forward` that Spec Y2 slice 5 deleted, and — the
//! deeper break — nothing ever enqueued, so the worker idled on an empty table.
//! The producer half (`fauna.posts.create` → outbox) is unit-tested in-crate
//! (`routes::outbox_producer_tests`), because `ingest_post_core` is `pub(crate)`.
//!
//! Carrier = channel only. Harness mirrors
//! `conformance_cross_nest_mail_relay.rs::start_nest`: `for_test`'s routers are
//! empty, so the test registers the federation handlers (the serving side) and
//! the anon discovery handlers (the pool resolves a peer's `nest_id` from its
//! URL via `fauna.nest.info` before dialing).
//!
//! Goal: `docs/goal/architecture/nest/private-mode.md` § Post Forwarding
//! (kind + `post_forward` capability + author-signature verification) and
//! § Implementation status today (the gap this suite closes).

mod common;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest::config::SubmissionPolicy;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::outbox::{DrainOutcome, drain_outbox_once};
use fauna_nest::routes::AppState;

/// Longer than the outbox's longest backoff step (about 8.5 hours): aging a
/// refused entry's failures by it makes the entry due, standing in for the
/// wall clock.
const A_DAY_SECS: i64 = 86_400;

/// Spin a real in-process nest (its full router, incl. `/api/v1/federation/ws`)
/// on a loopback socket with a distinct nest identity. `AppState::for_test`
/// already installs the one `Storage` impl (`SealedStorage`) — the
/// storage-mode axis was retired (`docs/goal/architecture/nest/storage-modes.md`),
/// so there is no longer a per-nest storage impl to choose.
async fn start_nest(policy: SubmissionPolicy) -> (String, Arc<AppState>) {
    start_nest_on(Arc::new(CacheDb::open_in_memory().unwrap()), policy).await
}

async fn start_nest_on(db: Arc<CacheDb>, policy: SubmissionPolicy) -> (String, Arc<AppState>) {
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
        ..AppState::for_test(db)
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

/// Make `home` the private nest that forwards `author`'s posts to the nest at
/// `relay_url`: the resolved NAT mode private (the admin's
/// `fauna.setup.nat_mode`), and `author`'s own local pairing row carrying
/// `post_forward` and the relay's URL — what `fauna.pair.add` stores on the
/// home box. No config-file switch exists.
async fn home_forwards(
    home: &Arc<AppState>,
    author: &[u8],
    relay_url: &str,
    relay: &Arc<AppState>,
) {
    *home.node_mode.write().await = fauna_nest::config::NodeMode::Private;
    home.db
        .store_pairing(
            author,
            &relay.nest_identity.public_key_bytes(),
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            Some(relay_url),
            None,
        )
        .await
        .unwrap();
}

async fn stored_body(state: &Arc<AppState>, post_id: &[u8; 32]) -> Option<Vec<u8>> {
    fauna_nest::segments::post::load_post_body(&state.post_segments, &state.db, post_id)
        .await
        .unwrap()
}

/// **The capstone.** `P` has one queued post; one drain pass relays it to `H`
/// over the channel. `H` stores it under the identical content-addressed id, and
/// `P`'s queue empties. This is the wire-level proof that the private box's
/// auto-forward actually delivers — the property `private-mode.md` § Post
/// Forwarding asserts and `home-relay.md` recorded as dead-on-arrival.
#[tokio::test]
async fn queued_post_forwards_to_the_public_nest_over_the_channel() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x11u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "hello from the basement");

    // H authorizes `author`'s posts arriving from P's verified nest identity.
    h_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &p_nest_id,
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // P queues the post exactly as `maybe_enqueue_outbox` does: the stored
    // embed-as-bytes body, verbatim, no re-encode and no nest signature.
    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();

    assert_eq!(
        stored_body(&h_state, &post_id).await,
        None,
        "precondition: H does not have the post before the drain"
    );

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 0
        },
        "the single queued post was forwarded"
    );

    assert_eq!(
        stored_body(&h_state, &post_id).await.as_deref(),
        Some(wire_bytes.as_slice()),
        "H stores the verbatim embed-as-bytes body under the same content-addressed post_id"
    );
    assert_eq!(
        p_state.db.outbox_depth().await.unwrap(),
        0,
        "a forwarded entry leaves the queue outright (deleted, not backed off)"
    );
    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Idle,
        "an empty queue is Idle, so the worker backs off"
    );
}

/// **The kind is idempotent on replay.** `store_post`'s `UNIQUE` tolerance means
/// a re-drain (a crash between peer-accept and `outbox_mark_sent`) is a no-op,
/// not a duplicate or an error — so `originate_post_forward` may declare itself
/// idempotent to the channel pool.
#[tokio::test]
async fn re_forwarding_the_same_post_is_a_no_op() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x22u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "sent twice");

    h_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &p_nest_id,
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    for _ in 0..2 {
        p_state
            .db
            .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
            .await
            .unwrap();
        assert_eq!(
            drain_outbox_once(&p_state).await,
            DrainOutcome::Processed {
                forwarded: 1,
                failed: 0
            }
        );
    }

    assert_eq!(
        stored_body(&h_state, &post_id).await.as_deref(),
        Some(wire_bytes.as_slice()),
        "the second forward neither errors nor corrupts the stored body"
    );
}

/// **Capability gate over the wire** (the sibling of
/// `conformance_cross_nest_mail_relay::mail_pull_without_capability_is_forbidden`).
/// A pairing that grants `mail_pull` but not `post_forward` cannot relay posts:
/// `private-mode.md` § Post Forwarding names `post_forward` as the capability,
/// and under `PairedOnly` the handler must enforce exactly that — not the
/// capability-agnostic "is there any pairing row" check.
#[tokio::test]
async fn post_forward_without_capability_is_refused_and_the_entry_is_retried() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x33u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "not authorized to relay");

    // A pairing exists — but grants only `mail_pull`.
    h_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 0,
            failed: 1
        },
        "the peer refuses a post_forward-less pairing"
    );
    assert_eq!(
        stored_body(&h_state, &post_id).await,
        None,
        "H stored nothing"
    );

    // A refused entry is retried, never silently dropped. It survives in the
    // queue but is no longer *due* — `outbox_pending` hides it behind its fresh
    // backoff, which is exactly why "kept" must be observed via the depth.
    assert_eq!(
        p_state.db.outbox_depth().await.unwrap(),
        1,
        "the entry stays queued for retry — a refusal never loses the user's post"
    );
    assert!(
        p_state.db.outbox_pending(50).await.unwrap().is_empty(),
        "and is backed off, not hot-looped"
    );
}

/// **No pairing row at all** → refused under `PairedOnly`, even though the
/// author's own signature over the post is perfectly valid. The author binding
/// and the relay authorization are independent checks.
#[tokio::test]
async fn post_forward_from_an_unpaired_nest_is_refused() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;

    let author = ActorKeypair::from_secret([0x44u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "unpaired");

    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 0,
            failed: 1
        }
    );
    assert_eq!(stored_body(&h_state, &post_id).await, None);
}

/// **A refusal of any age still delivers after a later grant** — the promise
/// `private-mode.md` § Post Forwarding makes. The retry ceiling used to HIDE an
/// entry for ever (`outbox_pending` filtered it out), so a grant that arrived
/// after about 8.5 hours of backoff delivered nothing. Now a refused entry
/// whose author still has an account keeps retrying past the ceiling at the
/// capped backoff; the grant — which lives on the relay, where this nest cannot
/// see it — is found by the next retry.
#[tokio::test]
async fn a_refusal_past_the_retry_ceiling_still_delivers_after_a_later_grant() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x77u8; 32]);
    let author_id = author.actor_id().0;
    p_state
        .db
        .create_user(&author_id, "free", "author")
        .await
        .unwrap();
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "refused for days");

    // The relay knows the pairing but has not granted `post_forward`.
    let grant_on_relay = |capability: &str| {
        let caps = vec![capability.to_string()];
        let h_state = h_state.clone();
        async move {
            h_state
                .db
                .store_pairing(&author_id, &p_nest_id, &caps, None, None, None)
                .await
                .unwrap();
        }
    };
    grant_on_relay(fauna_protocol::pair::capability::MAIL_PULL).await;

    home_forwards(&p_state, &author_id, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author_id, &wire_bytes, "forwarded_post")
        .await
        .unwrap();

    // Refused at every retry, one past the ceiling. Each pass first ages the
    // backoff past its longest step, standing in for the wall clock.
    for _ in 0..=CacheDb::OUTBOX_MAX_ATTEMPTS {
        p_state
            .db
            .test_age_outbox_failures(&author_id, A_DAY_SECS)
            .await
            .unwrap();
        assert_eq!(
            drain_outbox_once(&p_state).await,
            DrainOutcome::Processed {
                forwarded: 0,
                failed: 1
            },
            "the relay refuses every retry while the capability is missing"
        );
    }
    assert_eq!(
        p_state.db.outbox_depth().await.unwrap(),
        1,
        "past the ceiling the refused post is still queued"
    );
    assert_eq!(p_state.db.outbox_stuck_count().await.unwrap(), 1);

    // The user grants the capability on the relay; the next retry delivers.
    grant_on_relay(fauna_protocol::pair::capability::POST_FORWARD).await;
    p_state
        .db
        .test_age_outbox_failures(&author_id, A_DAY_SECS)
        .await
        .unwrap();
    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 0
        },
        "an entry past the retry ceiling is still due, so the later grant delivers it"
    );
    assert_eq!(
        stored_body(&h_state, &post_id).await.as_deref(),
        Some(wire_bytes.as_slice()),
        "H stored the post the grant unblocked"
    );
    assert_eq!(p_state.db.outbox_depth().await.unwrap(), 0);
}

/// **A poison entry leaves at the ceiling, and not before.** Retrying for ever
/// is right for a refusal — a later grant can end it — and wrong for bytes this
/// nest cannot even send: nothing ever makes them deliverable, so they would
/// rest for ever. Such an entry is kept below the ceiling (a decode bug must
/// not silently eat a stored post on its first failure) and leaves at it, even
/// though its author still has an account.
#[tokio::test]
async fn an_undecodable_entry_leaves_at_the_retry_ceiling_and_not_before() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::Open).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;

    let author = [0x88u8; 32];
    p_state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();
    home_forwards(&p_state, &author, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author, b"not-canonical-dag-cbor", "forwarded_post")
        .await
        .unwrap();

    for failure in 1..=CacheDb::OUTBOX_MAX_ATTEMPTS {
        p_state
            .db
            .test_age_outbox_failures(&author, A_DAY_SECS)
            .await
            .unwrap();
        assert_eq!(
            drain_outbox_once(&p_state).await,
            DrainOutcome::Processed {
                forwarded: 0,
                failed: 1
            }
        );
        let expected = if failure < CacheDb::OUTBOX_MAX_ATTEMPTS {
            1
        } else {
            0
        };
        assert_eq!(
            p_state.db.outbox_depth().await.unwrap(),
            expected,
            "kept below the ceiling, gone at it (failure {failure})"
        );
    }
}

/// **The federation leg gets the same future-`created_at` bound as a local
/// `fauna.posts.create`** (`docs/goal/ui/feed.md` § The read model, row 730):
/// `post_forward_handler` verifies the envelope directly and never calls
/// `Storage::ingest_post`, so without its own call to
/// `crate::storage::reject_future_created_at` a remote peer could future-date
/// a post to pin it atop every follower's feed just as easily as a local
/// author could.
#[tokio::test]
async fn post_forward_with_created_at_an_hour_in_the_future_is_refused() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x66u8; 32]);
    let an_hour_ahead =
        fauna_core::data::Timestamp(fauna_core::data::Timestamp::now().0 + 3_600_000_000);
    let (wire_bytes, post_id) =
        common::signed_post_wire_at(&author, "from the future, relayed", an_hour_ahead);

    h_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &p_nest_id,
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 0,
            failed: 1
        },
        "the relay refuses a future-dated created_at exactly as local ingest does"
    );
    assert_eq!(
        stored_body(&h_state, &post_id).await,
        None,
        "H stored nothing"
    );
}

// ── Relay-side ingest: no server-side classification ─────────────────────────
//
// The storage-mode axis was retired
// (`docs/goal/architecture/nest/storage-modes.md`); with it went server-side
// ingest classification entirely — there is no classifier registry, no
// obligation evaluation, and no `content_labels` write at post ingest, for a
// locally-created post OR a federation-relayed one
// (`docs/goal/architecture/content-scoring.md` § The placement matrix: a
// scorer runs only at a capability position, never at nest ingest). The
// surviving contract this test pins: a relayed post is verified for seal
// shape, stored under the content-addressed id, served back verbatim — and
// never labeled or quarantined by the relay itself.

/// A relayed post — even one that would have tripped the retired
/// `TextHeuristicClassifier`/obligation pipeline (`INVEST NOW` bait text) — is
/// stored and served with no server-side labels and no quarantine flag.
#[tokio::test]
async fn a_relayed_post_is_stored_without_server_side_classification() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::PairedOnly).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let author = ActorKeypair::from_secret([0x55u8; 32]);
    let (wire_bytes, post_id) =
        common::signed_post_wire(&author, "you should INVEST NOW my friends");

    h_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &p_nest_id,
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();
    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 0
        },
        "the relay accepts the forward — seal-shape verification only, no classify"
    );

    assert_eq!(
        stored_body(&h_state, &post_id).await.as_deref(),
        Some(wire_bytes.as_slice()),
        "the relayed post is stored and served verbatim"
    );
    assert!(
        h_state
            .db
            .get_content_labels("post", &hex::encode(post_id))
            .await
            .unwrap()
            .is_empty(),
        "no classifier ran — the nest never scores content at ingest"
    );
    assert!(
        !h_state.db.is_post_quarantined(&post_id).await.unwrap(),
        "and no obligation flag was applied — there is no obligation evaluation any more"
    );
}

/// Drain exactly one queued entry from `from` to the nest its author's row
/// names, asserting it relayed.
async fn relay_one(from: &Arc<AppState>, what: &str) {
    assert_eq!(
        drain_outbox_once(from).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 0
        },
        "{what}"
    );
}

/// **A deleted post is never re-ingested by replay**
/// (`docs/goal/ui/feed.md` § State & data shape → *Post deletion*). The post's
/// signed bytes stay public after the author deletes it, and under the default
/// `Open` policy any nest may relay a validly-authored post
/// (`private-mode.md` § Post Forwarding). So a stranger nest that kept the
/// bytes forwards them back after the delete: the relay must answer `Ok` and do
/// nothing — no re-store (the deletion stands) and no second bridge fan-out
/// (no fresh ActivityPub `Create`, no fresh Bluesky cross-post).
#[tokio::test]
async fn a_deleted_post_replayed_by_a_stranger_is_not_restored_or_fanned_out() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::Open).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;
    let (_s_url, stranger) = start_nest(SubmissionPolicy::Open).await;

    let author = ActorKeypair::from_secret([0x66u8; 32]);
    let (wire_bytes, post_id) = common::signed_post_wire(&author, "gone for good");
    #[cfg(feature = "test-hooks")]
    let fanouts = || h_state.post_fanout_initiations.snapshot();

    // 1. The author's home nest forwards the post.
    home_forwards(&p_state, &author.actor_id().0, &h_url, &h_state).await;

    p_state
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();
    relay_one(&p_state, "precondition: the post forwards to H").await;
    assert!(
        h_state.db.post_exists(&post_id).await.unwrap(),
        "precondition: H stores the forwarded post"
    );
    #[cfg(feature = "test-hooks")]
    let fanouts_after_first = {
        let n = fanouts();
        assert_eq!(n, 1, "precondition: the first arrival fans out once");
        n
    };

    // 2. The author deletes it; the signed tombstone relays to H.
    p_state
        .db
        .outbox_enqueue(
            &author.actor_id().0,
            &common::signed_tombstone_wire(&author, post_id),
            "forwarded_delete",
        )
        .await
        .unwrap();
    relay_one(&p_state, "precondition: the deletion relays to H").await;
    assert!(
        !h_state.db.post_exists(&post_id).await.unwrap(),
        "precondition: H removed the post"
    );

    // 3. A stranger replays the same public signed bytes.
    home_forwards(&stranger, &author.actor_id().0, &h_url, &h_state).await;
    stranger
        .db
        .outbox_enqueue(&author.actor_id().0, &wire_bytes, "forwarded_post")
        .await
        .unwrap();
    relay_one(
        &stranger,
        "the replay is answered Ok, so the stranger learns nothing and stops retrying",
    )
    .await;

    assert!(
        !h_state.db.post_exists(&post_id).await.unwrap(),
        "the deletion stands — the replayed bytes were not re-stored"
    );
    assert_eq!(
        stored_body(&h_state, &post_id).await,
        None,
        "nothing serves the deleted body again"
    );
    #[cfg(feature = "test-hooks")]
    assert_eq!(
        fanouts(),
        fanouts_after_first,
        "the replay initiated no second bridge fan-out"
    );
}

/// **Forwarding follows the author's own row on the home nest** (`private-mode.md`
/// § Implementation status today, ruled 2026-10-01). There is no config-file
/// switch: an entry whose author's local row carries `post_forward` relays to
/// the URL that row records; one whose author's row lacks the capability has
/// nowhere to go — nothing is sent, and it is undeliverable under the outbox's
/// existing ceiling rule, so it leaves at the ceiling and not before. On a nest
/// whose resolved NAT mode is public the worker does nothing at all.
#[tokio::test]
async fn forwarding_follows_the_home_rows_post_forward_capability() {
    let (h_url, h_state) = start_nest(SubmissionPolicy::Open).await;
    let (_p_url, p_state) = start_nest(SubmissionPolicy::Open).await;

    let forwarder = ActorKeypair::from_secret([0x91u8; 32]);
    let (fwd_bytes, fwd_id) = common::signed_post_wire(&forwarder, "my row forwards");
    let keeper = ActorKeypair::from_secret([0x92u8; 32]);
    let keeper_id = keeper.actor_id().0;
    p_state
        .db
        .create_user(&keeper_id, "free", "keeper")
        .await
        .unwrap();
    let (kept_bytes, kept_id) = common::signed_post_wire(&keeper, "my row only pulls mail");

    p_state
        .db
        .outbox_enqueue(&forwarder.actor_id().0, &fwd_bytes, "forwarded_post")
        .await
        .unwrap();
    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::NotPrivate,
        "a public nest's outbox worker acts on nothing"
    );

    home_forwards(&p_state, &forwarder.actor_id().0, &h_url, &h_state).await;
    // The keeper paired with the same relay, without `post_forward`.
    p_state
        .db
        .store_pairing(
            &keeper_id,
            &h_state.nest_identity.public_key_bytes(),
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            Some(&h_url),
            None,
        )
        .await
        .unwrap();
    p_state
        .db
        .outbox_enqueue(&keeper_id, &kept_bytes, "forwarded_post")
        .await
        .unwrap();

    assert_eq!(
        drain_outbox_once(&p_state).await,
        DrainOutcome::Processed {
            forwarded: 1,
            failed: 1
        },
        "the forwarder's post relays; the keeper's has no target"
    );
    assert_eq!(
        stored_body(&h_state, &fwd_id).await.as_deref(),
        Some(fwd_bytes.as_slice()),
        "H stores the post the row's capability forwarded"
    );
    assert_eq!(
        stored_body(&h_state, &kept_id).await,
        None,
        "nothing of the keeper's reached H"
    );

    // The keeper's entry is undeliverable: kept below the ceiling, gone at it.
    for failure in 2..=CacheDb::OUTBOX_MAX_ATTEMPTS {
        p_state
            .db
            .test_age_outbox_failures(&keeper_id, A_DAY_SECS)
            .await
            .unwrap();
        assert_eq!(
            drain_outbox_once(&p_state).await,
            DrainOutcome::Processed {
                forwarded: 0,
                failed: 1
            }
        );
        let expected = if failure < CacheDb::OUTBOX_MAX_ATTEMPTS {
            1
        } else {
            0
        };
        assert_eq!(
            p_state.db.outbox_depth().await.unwrap(),
            expected,
            "kept below the ceiling, gone at it (failure {failure})"
        );
    }
    assert_eq!(stored_body(&h_state, &kept_id).await, None);
}

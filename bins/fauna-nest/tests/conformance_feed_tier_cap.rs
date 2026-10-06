//! The feed quota the nest enforces — `fauna.feed.create` refuses past the
//! caller's tier `max_feeds`.
//!
//! Goal doc: `docs/goal/behavior/admin.md` § 2 Users (the tier *is* the quota).
//!
//! Before this file the refusal in `feed_routes::create_feed_core` was reached
//! by no test: `conformance_admin.rs` round-trips the `max_feeds` column through
//! tier create/update, and nothing ever created a feed against it. The e2e
//! suite met the refusal only by accident — its session-scoped actor running out
//! of the free tier's five feeds — and the fixture that stops that
//! (`tests/common/auth.py::set_tier_caps`) is honest only once the cap is proven
//! here, since lifting it there would otherwise remove the one place it was ever
//! reached.
//!
//! Four properties:
//!
//! 1. the tier's whole allowance creates, and one past it is refused **typed**,
//!    writing nothing;
//! 2. a deleted feed gives its slot back — the count is live, not a high-water
//!    mark;
//! 3. the count is per owner — one account at its cap refuses no other;
//! 4. with tier quotas off (the single-user desktop nest) nothing is refused.
//!
//! Tier: tier_3 (real handler + real DB). Every assertion is on
//! latency-independent state (e2e convention 14): no sleeps, no wall-clock.

mod common;

use std::sync::Arc;

use common::{dispatch, encode};
use fauna_nest::{db::CacheDb, feed_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    feed::{FeedCreateReply, FeedCreateRequest, FeedDeleteRequest},
};

/// The shipped `free` tier's feed cap — the `tiers.max_feeds` column default
/// (`db/migrations.rs`, the `tiers` table in `MIGRATIONS`), which every seeded test actor
/// is admitted at. Asserted in [`create_refuses_past_the_tier_cap_and_writes_nothing`]
/// rather than assumed, so a re-seed fails there instead of quietly turning the
/// refusal assertions into tautologies.
const FREE_TIER_MAX_FEEDS: usize = 5;

/// One in-process nest with the feed surface registered and tier quotas set as
/// asked — `true` is the standalone multi-tenant posture (`main.rs`), `false`
/// is `AppState::for_test`'s desktop default.
async fn nest(enforce_tier_quotas: bool) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    *state.enforce_tier_quotas.write().await = enforce_tier_quotas;
    let mut b = RpcRouter::builder();
    feed_handlers::register_feed_handlers(&mut b);
    (b.build(), state)
}

async fn create(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
    name: &str,
) -> Result<String, RpcError> {
    let reply = dispatch(
        router,
        state.clone(),
        owner,
        "fauna.feed.create",
        encode(&FeedCreateRequest {
            name: name.into(),
            rules: vec![fauna_core::scoring::FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 250,
            }],
            combination: "all".into(),
            ..Default::default()
        }),
    )
    .await?;
    let reply: FeedCreateReply = decode(&reply).unwrap();
    Ok(reply.feed_id)
}

async fn delete(router: &RpcRouter, state: &Arc<AppState>, owner: [u8; 32], feed_id: String) {
    dispatch(
        router,
        state.clone(),
        owner,
        "fauna.feed.delete",
        encode(&FeedDeleteRequest {
            feed_id,
            extra: Default::default(),
        }),
    )
    .await
    .expect("the owner deletes its own feed");
}

/// The count the refusal itself reads — so "nothing was written" is asserted on
/// the refusal's own input, not on a projection of it.
async fn feed_count(state: &Arc<AppState>, owner: &[u8; 32]) -> usize {
    state
        .db
        .count_feeds_by_owner(owner)
        .await
        .expect("count feeds") as usize
}

/// A quota refusal, and not some other 403. The code is shared by every
/// permission refusal in the `feed` namespace, so the reason is what tells a
/// client — and a reader of an app log — that this one is the tier.
fn assert_refused_for_the_tier(err: &RpcError) {
    assert_eq!(
        err.code, "fauna.feed.permission_denied",
        "unexpected refusal: {err:?}"
    );
    assert!(
        format!("{:?}", err.details).contains("feed limit reached"),
        "the refusal must name the tier quota: {err:?}"
    );
}

#[tokio::test]
async fn create_refuses_past_the_tier_cap_and_writes_nothing() {
    let (router, state) = nest(true).await;
    let owner = [0x11; 32];
    common::seed_dispatch_actor(&state.db, &owner).await;
    assert_eq!(
        state
            .db
            .get_user_tier_max_feeds(&owner)
            .await
            .expect("the seeded actor has a tier") as usize,
        FREE_TIER_MAX_FEEDS,
        "the `free` feed cap moved — re-read this file's arithmetic before editing it"
    );

    for i in 0..FREE_TIER_MAX_FEEDS {
        create(&router, &state, owner, &format!("feed {i}"))
            .await
            .unwrap_or_else(|e| panic!("feed {i} is within the cap, got {e:?}"));
    }

    let err = create(&router, &state, owner, "one too many")
        .await
        .expect_err("the feed past the cap must be refused");
    assert_refused_for_the_tier(&err);
    assert_eq!(
        feed_count(&state, &owner).await,
        FREE_TIER_MAX_FEEDS,
        "a refused create must write nothing"
    );
}

#[tokio::test]
async fn a_deleted_feed_gives_its_slot_back() {
    let (router, state) = nest(true).await;
    let owner = [0x22; 32];

    let mut ids = Vec::new();
    for i in 0..FREE_TIER_MAX_FEEDS {
        ids.push(
            create(&router, &state, owner, &format!("feed {i}"))
                .await
                .expect("within the cap"),
        );
    }
    create(&router, &state, owner, "at the cap")
        .await
        .expect_err("the cap binds before the delete");

    delete(&router, &state, owner, ids.pop().unwrap()).await;
    create(&router, &state, owner, "the freed slot")
        .await
        .expect("a delete must give back the slot it held");

    let err = create(&router, &state, owner, "full again")
        .await
        .expect_err("and the cap binds again once the slot is refilled");
    assert_refused_for_the_tier(&err);
}

#[tokio::test]
async fn the_cap_counts_each_owner_separately() {
    let (router, state) = nest(true).await;
    let full = [0x33; 32];
    let fresh = [0x44; 32];

    for i in 0..FREE_TIER_MAX_FEEDS {
        create(&router, &state, full, &format!("feed {i}"))
            .await
            .expect("within the cap");
    }
    assert_refused_for_the_tier(
        &create(&router, &state, full, "one too many")
            .await
            .expect_err("the full account is refused"),
    );

    create(&router, &state, fresh, "a different account")
        .await
        .expect("another account's full quota must not refuse this one");
}

#[tokio::test]
async fn nothing_is_refused_with_tier_quotas_off() {
    let (router, state) = nest(false).await;
    let owner = [0x55; 32];

    for i in 0..=FREE_TIER_MAX_FEEDS {
        create(&router, &state, owner, &format!("feed {i}"))
            .await
            .unwrap_or_else(|e| {
                panic!("the single-user desktop nest enforces no tier quota, got {e:?}")
            });
    }
    assert_eq!(feed_count(&state, &owner).await, FREE_TIER_MAX_FEEDS + 1);
}

//! The device quota the nest now enforces — `fauna.sync.register` refuses past
//! the caller's tier `max_devices`.
//!
//! Goal doc: `docs/goal/behavior/admin.md` § 2 Users (the tier *is* the quota),
//! with the refusal itself in `docs/goal/behavior/devices.md` § Step 4 and the
//! frontier-ceiling premise it restores in
//! `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors.
//!
//! Before this, `AdminTier::max_devices` was read at five sites in the nest and
//! *compared* at none: `fauna.sync.register` never looked up a tier, so the
//! number every app shows the user as `AccountGetQuota.devices.max` was
//! decoration, and one account could insert `sync_devices` rows without limit.
//!
//! Three properties, because the naive `count >= max` gets two of them wrong:
//!
//! 1. a **new** device past the cap is refused, typed;
//! 2. a **re-register of an existing `device_id`** still succeeds at the cap —
//!    the row is an upsert, and refusing it would strand a device at the limit
//!    with no way to re-label or re-provision itself;
//! 3. the nest's own **WebDAV pseudo-device** does not consume a user slot —
//!    it is written by `fauna.bridges.webdav_record_change`, not by the user's
//!    fleet — and a client cannot mint that exclusion for itself: the door
//!    refuses a `device_id` equal to the derived pseudo-device id outright.
//!
//! Tier: tier_3 (real handler + real DB). Every assertion is on
//! latency-independent state (e2e convention 14): no sleeps, no wall-clock.

mod common;

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{account_handlers, db::CacheDb, sync_handlers};
use fauna_protocol::account::{QuotaGetReply, QuotaGetRequest};
use fauna_protocol::sync::{SyncRegisterReply, SyncRegisterRequest};

/// The shipped `free` tier's cap (`db/migrations.rs` `SEED_TIERS`), which
/// `common::seed_dispatch_actor` admits every test actor at. Asserted in
/// [`nest`] rather than assumed: this file's arithmetic is only meaningful
/// against the real seeded number, so a re-seed must fail *there* rather than
/// silently turn the refusal assertions into tautologies.
const FREE_TIER_MAX_DEVICES: i64 = 3;

/// One in-process nest with the sync surface registered and tier quotas ON —
/// the standalone multi-tenant posture (`main.rs` passes `true`), not
/// `AppState::for_test`'s desktop default.
async fn nest() -> (RpcRouter, Arc<AppState>, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    *state.enforce_tier_quotas.write().await = true;
    let mut b = RpcRouter::builder();
    sync_handlers::register_sync_handlers(&mut b);
    account_handlers::register_account_user_handlers(&mut b);
    let account = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &account.actor_id().0).await;
    assert_eq!(
        state
            .db
            .get_user_tier_max_devices(&account.actor_id().0)
            .await
            .expect("the seeded actor has a tier"),
        FREE_TIER_MAX_DEVICES,
        "the `free` seed moved — re-read this file's arithmetic before editing it"
    );
    (b.build(), state, account)
}

async fn register(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_id: [u8; 32],
    label: &str,
) -> Result<SyncRegisterReply, fauna_protocol::RpcError> {
    common::call(
        router,
        state,
        actor,
        "fauna.sync.register",
        &SyncRegisterRequest {
            device_id: hex::encode(device_id),
            label: label.to_string(),
            capabilities: "read,write".to_string(),
            ..Default::default()
        },
    )
    .await
}

async fn device_count(state: &Arc<AppState>, actor: &[u8; 32]) -> usize {
    state.db.list_sync_devices(actor).await.expect("list").len()
}

#[tokio::test]
async fn register_refuses_past_the_tier_cap_but_never_a_re_register() {
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;

    // The tier's whole allowance registers.
    for i in 0..FREE_TIER_MAX_DEVICES {
        let id = [i as u8 + 1; 32];
        register(&router, &state, actor, id, "laptop")
            .await
            .unwrap_or_else(|e| panic!("device {i} is within the cap, got {e:?}"));
    }

    // One past it is refused, and refused *typed* — a client renders this and
    // tells the user to ask their admin for a bigger tier, so an internal 500
    // would be a silent product failure rather than a quota.
    let err = register(&router, &state, actor, [0xAA; 32], "one too many")
        .await
        .expect_err("the device past the cap must be refused");
    // The shared constant, which `fauna_client_sync::is_device_limit_exceeded`
    // matches on: pinning the emitter to it here is what keeps the daemon's
    // register hold naming this refusal rather than reading it as offline.
    assert_eq!(
        err.code,
        fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED,
        "unexpected refusal: {err:?}"
    );

    // Nothing was written: the refused id holds no row.
    let devices = state.db.list_sync_devices(&actor).await.expect("list");
    assert_eq!(devices.len(), FREE_TIER_MAX_DEVICES as usize);
    assert!(
        !devices.iter().any(|d| d.device_id == [0xAAu8; 32].to_vec()),
        "the refused register must not have inserted a row"
    );

    // A device already holding a row keeps registering AT the cap — the upsert
    // is how a device re-labels itself and how every provision re-registers
    // (`fauna_client_sync::register_this_machine`). Refusing it would make the
    // cap strand the very devices it admitted.
    register(&router, &state, actor, [1u8; 32], "laptop renamed")
        .await
        .expect("a re-register of an existing device is not a new slot");
    assert_eq!(
        device_count(&state, &actor).await,
        FREE_TIER_MAX_DEVICES as usize,
        "the re-register must not have grown the roster"
    );
}

#[tokio::test]
async fn the_nests_own_webdav_pseudo_device_does_not_consume_a_user_slot() {
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;

    // As `fauna.bridges.webdav_record_change` writes it: the nest's own row,
    // through the uncapped internal door, under the deterministic per-actor id.
    let webdav = fauna_core::label_custody::webdav_pseudo_device_id(&actor);
    state
        .db
        .register_sync_device(
            &actor,
            &webdav,
            fauna_core::label_custody::WEBDAV_PSEUDO_DEVICE_LABEL,
            None,
            "read,write",
        )
        .await
        .expect("the nest's own writer is never capped");

    // The user's whole allowance must still fit beside it. Without the
    // exclusion the last one here is refused — an honest device turned away
    // because a WebDAV client once wrote a file.
    for i in 0..FREE_TIER_MAX_DEVICES {
        let id = [i as u8 + 1; 32];
        register(&router, &state, actor, id, "laptop")
            .await
            .unwrap_or_else(|e| panic!("device {i} must fit beside the pseudo-device, got {e:?}"));
    }

    // The cap still binds — the exclusion is one row, not an off-switch.
    let err = register(&router, &state, actor, [0xAA; 32], "one too many")
        .await
        .expect_err("the cap still applies with the pseudo-device present");
    assert_eq!(
        err.code,
        fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED
    );

    // A client cannot occupy that same row either: the id is nest-authored,
    // not merely derivable, so `fauna.sync.register` refuses it outright
    // rather than treating the attempt as an ordinary re-register — the same
    // way it refuses malformed hex. This is what makes the exclusion above
    // safe: without the refusal, a client could compute `webdav` itself and
    // mint the one uncounted slot the count deliberately excludes.
    let err = register(&router, &state, actor, webdav, "WebDAV")
        .await
        .expect_err("a client may not register the pseudo-device's own id");
    assert_eq!(
        err.code, "fauna.sync.invalid_request",
        "unexpected refusal: {err:?}"
    );
    assert_eq!(
        device_count(&state, &actor).await,
        FREE_TIER_MAX_DEVICES as usize + 1,
        "the refused register must not have added or altered any row"
    );
}

/// The re-seed pseudo-device is a label re-homed rows carry, never a device:
/// `fauna.sync.register` refuses it exactly as the WebDAV one
/// (`writer-signed-change-records.md` ruling (7)(a)(ii)).
#[tokio::test]
async fn a_client_may_not_register_the_reseed_pseudo_device() {
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;
    let reseed = fauna_core::label_custody::reseed_pseudo_device_id(&actor);
    assert_ne!(
        reseed,
        fauna_core::label_custody::webdav_pseudo_device_id(&actor),
        "the two pseudo-devices are distinct labels"
    );

    let err = register(&router, &state, actor, reseed, "laptop")
        .await
        .expect_err("a client may not register the re-seed pseudo-device's id");
    assert_eq!(
        err.code, "fauna.sync.invalid_request",
        "unexpected refusal: {err:?}"
    );
    assert_eq!(device_count(&state, &actor).await, 0, "nothing registered");
}

#[tokio::test]
async fn quota_get_devices_used_matches_the_cap_it_is_shown_against() {
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;

    // The nest's own WebDAV pseudo-device, written the way
    // `fauna.bridges.webdav_record_change` does: through the uncapped internal
    // door, not the public `fauna.sync.register` (which refuses the id).
    let webdav = fauna_core::label_custody::webdav_pseudo_device_id(&actor);
    state
        .db
        .register_sync_device(
            &actor,
            &webdav,
            fauna_core::label_custody::WEBDAV_PSEUDO_DEVICE_LABEL,
            None,
            "read,write",
        )
        .await
        .expect("the nest's own writer is never capped");

    // The user's whole tier allowance registers beside it.
    for i in 0..FREE_TIER_MAX_DEVICES {
        let id = [i as u8 + 1; 32];
        register(&router, &state, actor, id, "laptop")
            .await
            .unwrap_or_else(|e| panic!("device {i} must fit beside the pseudo-device, got {e:?}"));
    }

    // `devices.used` must describe the same set `devices.max` caps: the
    // pseudo-device excluded. Before the fix this reports `max + 1` — a
    // capped-but-not-over-cap user reading their own overdrawn quota.
    let reply: QuotaGetReply = common::call(
        &router,
        &state,
        actor,
        "fauna.quota.get",
        &QuotaGetRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("quota.get ok");
    assert_eq!(reply.devices.max, FREE_TIER_MAX_DEVICES);
    assert_eq!(
        reply.devices.used, reply.devices.max,
        "the WebDAV pseudo-device must not inflate the count a user sees against their cap"
    );
}

#[tokio::test]
async fn the_desktop_nest_caps_nothing() {
    // `enforce_tier_quotas` off is the embedded single-user desktop nest
    // (`desktop_serve.rs`): a tier ceiling on the owner's own machine is
    // nonsense, and this is the arm that keeps it so.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    assert!(
        !*state.enforce_tier_quotas.read().await,
        "AppState::for_test models the desktop nest"
    );
    let mut b = RpcRouter::builder();
    sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();
    let account = ActorKeypair::generate();
    let actor = account.actor_id().0;
    common::seed_dispatch_actor(&state.db, &actor).await;

    for i in 0..(FREE_TIER_MAX_DEVICES + 3) {
        register(&router, &state, actor, [i as u8 + 1; 32], "device")
            .await
            .unwrap_or_else(|e| panic!("device {i} must register with quotas off, got {e:?}"));
    }
    assert_eq!(
        device_count(&state, &actor).await,
        FREE_TIER_MAX_DEVICES as usize + 3
    );
}

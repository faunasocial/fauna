//! Integration round-trip for the authenticated account surface —
//! `fauna.account.{get,delete,upgrade,am_i_admin}`, `fauna.quota.get`,
//! `fauna.profile.handle.change`. A behavior-preserving transport migration of
//! the bearer-authed HTTP routes (`account_routes::get_account`,
//! `quota_routes::get_quota`, `routes::am_i_admin`, and the `registration.rs`
//! trio `put_handle` / `post_upgrade` / `delete_account`). The handlers reuse
//! the same `CacheDb` methods the HTTP twins call — these tests exercise the
//! WS-RPC layer: request decode, the reused `CacheDb` reaching a real in-memory
//! store, reply encoding, the actor scoping keyed on the connection actor
//! (replacing the HTTP path-param + bearer match), the replay metadata, and the
//! `User | Admin` allowlist.
//!
//! Users / tiers / admins / invite codes are seeded directly via the public
//! `CacheDb` methods (`create_user`, `add_admin_actor`, `create_invite_code`);
//! default tiers are migration-seeded, so `get_tier("free")` is `Some`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/account.rs`.
//! Slice tracked internally (§ T1).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    account_handlers,
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError,
    account::{
        AccountDeleteReply, AccountDeleteRequest, AccountGetReply, AccountGetRequest,
        AmIAdminReply, AmIAdminRequest, ChangeHandleReply, ChangeHandleRequest, QuotaGetReply,
        QuotaGetRequest, UpgradeReply, UpgradeRequest,
    },
    decode_strict as decode,
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    account_handlers::register_account_user_handlers(&mut b);
    (b.build(), state)
}

/// [`dispatch`] without the `users`-row seed — the actor reaches the handler
/// exactly as an **unregistered** actor does in production.
///
/// The two tests below are the only ones in this file that want that: they assert
/// what an actor with no `users` row gets back, so seeding one would erase the
/// very condition under test.
async fn dispatch_unseeded(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

// ── fauna.account.get ───────────────────────────────────────────

#[tokio::test]
async fn account_get_returns_seeded_user() {
    let (router, state) = router_with_db_only().await;
    let actor = [11u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();
    state.db.set_handle(&actor, "alice").await.unwrap();

    let reply: AccountGetReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.account.get",
            encode(&AccountGetRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("account.get ok"),
    )
    .unwrap();

    assert_eq!(reply.actor_id, hex::encode(actor));
    assert_eq!(reply.tier, "free");
    // Handle round-trips from the users.handle column.
    assert_eq!(reply.handle.as_deref(), Some("alice"));
    // No eviction for a fresh user.
    assert!(reply.eviction.is_none());
    // Free-tier quota maxes come from the migration-seeded `free` tier row.
    assert!(reply.quota.inbox.max_bytes > 0);
    assert!(reply.quota.storage.max_bytes > 0);
    assert_eq!(reply.quota.devices.max, 3); // free tier: 3 devices
    // Node policy echoed from the hard-coded eviction-ladder constants (not an
    // admin choice — `admin.md` § 2 Users → *Cutting a user off*).
    assert_eq!(
        reply.node_policy.eviction_warning_days,
        fauna_protocol::node_policy::EVICTION_WARNING_DAYS
    );
    assert_eq!(
        reply.node_policy.eviction_suspension_days,
        fauna_protocol::node_policy::EVICTION_SUSPENSION_DAYS
    );
}

#[tokio::test]
async fn account_get_refused_for_unregistered_actor() {
    let (router, state) = router_with_db_only().await;
    let actor = [12u8; 32];
    let err = dispatch_unseeded(
        &router,
        state,
        actor,
        "fauna.account.get",
        encode(&AccountGetRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("an actor with no users row is refused");
    // The authority gate (`caller_class_for_actor`) resolves an actor with no
    // `users` row to *no* caller class, so the gate refuses it **before**
    // `account_get_handler` runs. The refusal is therefore the gate's
    // `permission_denied`, not the handler's own `not_found` — which is now
    // unreachable for an unregistered actor.
    //
    // And it is the CENTRAL code, not `fauna.account.…`: an unknown actor is
    // denied on *every* kind, so the refusal is not a statement about the
    // account family at all. That every-kind shape is a load-bearing wire
    // signal — the Go bridges' revocation probe keys on exactly it
    // (`wsrpc/reconnect.go`) — so this arm must never become per-family
    // (`api-layers.md` § Caller-class authorization → *Refusal codes at the
    // gate*, ruled 2026-08-17). Listed-kind *class* refusals are the ones that
    // answer `fauna.<ns>.permission_denied`.
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

#[tokio::test]
async fn account_get_scopes_to_connection_actor() {
    let (router, state) = router_with_db_only().await;
    let mine = [11u8; 32];
    let other = [99u8; 32];
    state.db.create_user(&mine, "free", "alice").await.unwrap();

    // The scoping the HTTP twin enforced via path-param + bearer now keys on the
    // connection actor, and it holds two ways — neither of which can reach
    // `mine`'s row.

    // 1. An *unregistered* other is refused by the authority gate outright —
    //    with the central every-kind code, per the ruling cited above.
    let err = dispatch_unseeded(
        &router,
        state.clone(),
        other,
        "fauna.account.get",
        encode(&AccountGetRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("an unregistered actor is refused");
    assert_eq!(err.code, "fauna.bridges.permission_denied");

    // 2. A *registered* other reads its OWN account, never `mine`'s — the reply
    //    echoes the calling actor.
    let reply: AccountGetReply = decode(
        &dispatch(
            &router,
            state,
            other,
            "fauna.account.get",
            encode(&AccountGetRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("a registered actor reads its own account"),
    )
    .unwrap();
    assert_eq!(reply.actor_id, hex::encode(other));
    assert_ne!(reply.actor_id, hex::encode(mine));
    assert_ne!(
        reply.handle.as_deref(),
        Some("alice"),
        "other never sees alice's handle"
    );
}

// ── fauna.quota.get ─────────────────────────────────────────────

#[tokio::test]
async fn quota_get_returns_tier_values() {
    let (router, state) = router_with_db_only().await;
    let actor = [21u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    let reply: QuotaGetReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.quota.get",
            encode(&QuotaGetRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("quota.get ok"),
    )
    .unwrap();

    assert_eq!(reply.tier, "free");
    assert!(reply.inbox.max_bytes > 0);
    assert!(reply.storage.max_bytes > 0);
    assert_eq!(reply.devices.used, 0); // no sync devices registered
    assert_eq!(reply.devices.max, 3);
    // Free tier → paid-only features off.
    assert!(!reply.features.versioned_backup);
    assert!(!reply.features.bridges);
}

// ── fauna.account.am_i_admin ────────────────────────────────────

#[tokio::test]
async fn am_i_admin_true_for_admin() {
    let (router, state) = router_with_db_only().await;
    let actor = [31u8; 32];
    state.db.add_admin_actor(&actor).await.unwrap();

    let reply: AmIAdminReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.account.am_i_admin",
            encode(&AmIAdminRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("am_i_admin ok (admin caller is permitted)"),
    )
    .unwrap();
    assert!(reply.admin);
}

#[tokio::test]
async fn am_i_admin_false_for_regular_user() {
    let (router, state) = router_with_db_only().await;
    let actor = [32u8; 32];
    state.db.create_user(&actor, "free", "bob").await.unwrap();

    let reply: AmIAdminReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.account.am_i_admin",
            encode(&AmIAdminRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("am_i_admin ok"),
    )
    .unwrap();
    assert!(!reply.admin);
}

// ── fauna.profile.handle.change ─────────────────────────────────

#[tokio::test]
async fn change_handle_queues_pending_action() {
    let (router, state) = router_with_db_only().await;
    let actor = [41u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    let state_after = state.clone();
    let reply: ChangeHandleReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.profile.handle.change",
            encode(&ChangeHandleRequest {
                handle: "newhandle".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("change_handle ok"),
    )
    .unwrap();

    assert!(reply.pending_action_id > 0);
    assert_eq!(reply.status, "pending");
    assert_eq!(reply.new_handle, "newhandle");
    assert!(reply.execute_after > 0);

    // The stored row carries the requested handle as `target` — the list
    // SUMMARY (which omits `payload`) is what the apps' pending-actions
    // section describes rows from, so a None target painted every handle
    // change as a bare "handle.change" (found by tui's journey test,
    // 2026-08-19; the snapshot-delete creator already passed its target).
    let row = state_after
        .db
        .get_pending_action(reply.pending_action_id)
        .await
        .unwrap()
        .expect("row exists");
    assert_eq!(row.target.as_deref(), Some("newhandle"));
}

#[tokio::test]
async fn change_handle_rejects_invalid_handle() {
    let (router, state) = router_with_db_only().await;
    let actor = [42u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    // "ab" is < 3 chars → validate_handle rejects.
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.handle.change",
        encode(&ChangeHandleRequest {
            handle: "ab".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("invalid handle rejected");
    assert_eq!(err.code, "fauna.profile.invalid_request");
}

// ── fauna.account.delete ────────────────────────────────────────

#[tokio::test]
async fn delete_queues_pending_action() {
    let (router, state) = router_with_db_only().await;
    let actor = [51u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    let reply: AccountDeleteReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.account.delete",
            encode(&AccountDeleteRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("account.delete ok"),
    )
    .unwrap();

    assert!(reply.pending_action_id > 0);
    assert_eq!(reply.status, "pending");
    assert!(reply.message.contains("deletion"));
    assert!(reply.execute_after > 0);
}

// ── fauna.account.upgrade ───────────────────────────────────────

#[tokio::test]
async fn upgrade_consumes_invite_and_updates_tier() {
    let (router, state) = router_with_db_only().await;
    let actor = [61u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();
    state
        .db
        .create_invite_code("UPGRADE2026", "personal", 5)
        .await
        .unwrap();

    let reply: UpgradeReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.account.upgrade",
            encode(&UpgradeRequest {
                tier: "personal".into(),
                invite_code: "UPGRADE2026".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("upgrade ok"),
    )
    .unwrap();

    assert!(reply.ok);
    assert_eq!(reply.tier, "personal");
    // The upgrade actually took: the user row now reads `personal`.
    let user = state.db.get_user(&actor).await.unwrap().unwrap();
    assert_eq!(user.tier, "personal");
}

#[tokio::test]
async fn upgrade_rejects_same_or_lower_tier() {
    let (router, state) = router_with_db_only().await;
    let actor = [62u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.account.upgrade",
        encode(&UpgradeRequest {
            tier: "free".into(),
            invite_code: "UNUSED".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("downgrade/same-tier rejected");
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn upgrade_rejects_invalid_invite_code() {
    let (router, state) = router_with_db_only().await;
    let actor = [63u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.account.upgrade",
        encode(&UpgradeRequest {
            tier: "personal".into(),
            invite_code: "BOGUS".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("bad invite code rejected");
    assert_eq!(err.code, "fauna.account.invalid_request");
}

// ── malformed payload ───────────────────────────────────────────

#[tokio::test]
async fn account_get_rejects_malformed_payload() {
    let (router, state) = router_with_db_only().await;
    let actor = [71u8; 32];
    state.db.create_user(&actor, "free", "alice").await.unwrap();
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.account.get",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata ─────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db_only().await;
    // Three reads + the two pending-action creators are @5s; none forbids
    // replay.
    for kind in [
        "fauna.account.get",
        "fauna.quota.get",
        "fauna.account.am_i_admin",
        "fauna.profile.handle.change",
        "fauna.account.delete",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
    // upgrade is the invite-consuming write — 30s, matching posts.create.
    let up = router
        .kind_meta("fauna.account.upgrade")
        .expect("upgrade registered");
    assert!(!up.forbid_replay);
    assert_eq!(up.default_deadline, std::time::Duration::from_secs(30));
}

// ── allowlist ───────────────────────────────────────────────────

#[tokio::test]
async fn account_kinds_user_and_admin_only_at_allowlist_layer() {
    for kind in [
        "fauna.account.get",
        "fauna.quota.get",
        "fauna.account.am_i_admin",
        "fauna.profile.handle.change",
        "fauna.account.upgrade",
        "fauna.account.delete",
    ] {
        // User + Admin both permitted (an admin manages their own account).
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, kind),
                "{kind} should be permitted for {class:?}"
            );
        }
        // Bridge actors have no personal account → denied.
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}

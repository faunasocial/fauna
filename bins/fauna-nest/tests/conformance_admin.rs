//! Integration round-trip for the admin user-management read surface —
//! `fauna.admin.users.{list,get}` (part of the WS-RPC-everywhere migration,
//! tracked internally). A behavior-preserving
//! transport migration of the bearer-authed `admin::{list_users_paginated,
//! get_user}` HTTP handlers onto the per-actor WS-RPC connection. The handlers
//! reshape the same `CacheDb::{list_users_paginated, get_user}` rows the twins
//! return; these tests exercise the WS-RPC layer: request decode, the reply
//! shapes, the `Admin`-only allowlist (matching the twins' `AdminBearerAuth`),
//! the `not_found` / `invalid_params` / `malformed` mappings, and replay
//! metadata.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/admin.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::admin_actor;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    admin_ws_handlers,
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    ByteBuf, RpcError,
    admin::{
        AdminAdminAddRequest,
        AdminAdminRemoveRequest,
        AdminAdminsListReply,
        AdminAdminsListRequest,
        AdminAuditIntegrityReply,
        AdminAuditIntegrityRequest,
        AdminAuditListReply,
        AdminAuditListRequest,
        AdminClusterStatusReply,
        AdminClusterStatusRequest,
        AdminDeploymentSeedGetReply,
        AdminDeploymentSeedGetRequest,
        AdminEvictionsListReply,
        AdminEvictionsListRequest,
        // C5 — folders / services.
        AdminFolderAddMemberRequest,
        AdminFolderCreateReply,
        AdminFolderCreateRequest,
        AdminFolderGetReply,
        AdminFolderGetRequest,
        AdminGcRequest,
        AdminInviteCodeCreateReply,
        AdminInviteCodeCreateRequest,
        AdminInviteCodeDeleteRequest,
        AdminInviteCodesListReply,
        AdminInviteCodesListRequest,
        AdminInviteRequestApproveReply,
        AdminInviteRequestApproveRequest,
        AdminInviteRequestDenyRequest,
        AdminInviteRequestsListReply,
        AdminInviteRequestsListRequest,
        AdminLogLevel,
        AdminLogsReply,
        AdminLogsRequest,
        AdminOkReply,
        AdminPendingActionReply,
        AdminPendingActionsListReply,
        AdminPendingActionsListRequest,
        AdminServiceUpdateReply,
        AdminServiceUpdateRequest,
        AdminServicesListReply,
        AdminServicesListRequest,
        AdminStatsReply,
        AdminStatsRequest,
        AdminStatusReply,
        AdminStatusRequest,
        AdminTierCreateRequest,
        AdminTierUpdateRequest,
        AdminTiersListReply,
        AdminTiersListRequest,
        AdminUserCancelEvictionRequest,
        AdminUserClearHandleRequest,
        AdminUserCreateRequest,
        AdminUserDeleteRequest,
        AdminUserEvictRequest,
        AdminUserGetReply,
        AdminUserGetRequest,
        AdminUserSuspendRequest,
        AdminUserUpdateRequest,
        AdminUsersListReply,
        AdminUsersListRequest,
        AdminWorkerStatusReply,
        AdminWorkerStatusRequest,
    },
    decode_strict as decode,
};

/// `AppState::for_test` + a router carrying only the admin handlers.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    (b.build(), state)
}

/// Same, but with an isolated `services_json_path` — `AppState::for_test`
/// hard-codes a per-PID temp path shared across same-process tests, so the
/// `services.*` tests each point at their own `TempDir` to avoid collisions.
async fn router_and_state_with_services_path(
    path: std::path::PathBuf,
) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    state.services_json_path = path;
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

/// Every admin kind — C1 user-management + C2 admin-management + C3 stats/ops
/// + C5 folders/services (C4 pairings retired).
const ADMIN_KINDS: [&str; 39] = [
    // C1 — user management.
    "fauna.admin.users.list",
    "fauna.admin.users.get",
    "fauna.admin.users.create",
    "fauna.admin.users.update",
    "fauna.admin.users.delete",
    "fauna.admin.users.clear_handle",
    "fauna.admin.users.evict",
    "fauna.admin.users.cancel_eviction",
    "fauna.admin.users.suspend",
    "fauna.admin.evictions.list",
    // C2 — admin management.
    "fauna.admin.tiers.list",
    "fauna.admin.tiers.create",
    "fauna.admin.tiers.update",
    // Membership designation (monetization.md § Pillar 4) — behavior covered by
    // `conformance_membership.rs`; listed here for the allowlist + replay sweeps.
    "fauna.admin.membership_tiers.list",
    "fauna.admin.membership_tiers.set",
    "fauna.admin.membership_tiers.clear",
    "fauna.admin.invite_codes.list",
    "fauna.admin.invite_codes.create",
    "fauna.admin.invite_codes.delete",
    "fauna.admin.invite_requests.list",
    "fauna.admin.invite_requests.approve",
    "fauna.admin.invite_requests.deny",
    "fauna.admin.admins.list",
    "fauna.admin.admins.add",
    "fauna.admin.admins.remove",
    // C3 — stats / audit / ops.
    "fauna.admin.stats",
    "fauna.admin.status",
    "fauna.admin.audit.list",
    "fauna.admin.audit.integrity",
    "fauna.admin.cluster.status",
    "fauna.admin.gc",
    "fauna.admin.worker.status",
    "fauna.admin.pending_actions.list",
    // C5 — folders / services.
    "fauna.admin.folders.create",
    "fauna.admin.folders.get",
    "fauna.admin.folders.add_member",
    "fauna.admin.services.list",
    "fauna.admin.services.update",
    // C6 — observability.
    "fauna.admin.logs",
    // C4 pairings retired (per-user-pairing design) — see conformance_pair.rs.
];

// ── list ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_returns_created_users() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    let bob = [2u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    state.db.create_user(&bob, "free", "bob").await.unwrap();

    let reply: AdminUsersListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.list",
            encode(&AdminUsersListRequest {
                limit: None,
                offset: 0,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    assert_eq!(reply.total, 2);
    assert_eq!(reply.users.len(), 2);
    let labels: Vec<&str> = reply.users.iter().map(|u| u.label.as_str()).collect();
    assert!(labels.contains(&"alice"));
    assert!(labels.contains(&"bob"));
    // Actor ids ride as raw 32-byte ByteBuf.
    assert!(reply.users.iter().all(|u| u.actor_id.len() == 32));
}

/// A handle-registered actor (the box claimer, an invite-approved user, a
/// self-registered user — all via `create_user_with_handle`) gets its display
/// `label` defaulted to the handle, so `fauna.admin.users.list` never returns a
/// blank name. Regression for the admin claim switching to a handle
/// leaving `label` empty, which left the admin-users hub + the `admin-dns`
/// catch-all actor picker (`admin-dns-domain-catch-all-select`,
/// `mail-multidomain.md` § Per-domain catch-all) showing only the raw actor-id —
/// the picker had no selectable name. (`create_user` callers above set an explicit
/// label, so they never exercised the handle path that production registration
/// takes.)
#[tokio::test]
async fn handle_registered_user_label_defaults_to_handle() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [3u8; 32];
    state
        .db
        .create_user_with_handle(&carol, "free", "carol", None)
        .await
        .unwrap();

    let reply: AdminUsersListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.list",
            encode(&AdminUsersListRequest {
                limit: None,
                offset: 0,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    let carol_row = reply
        .users
        .iter()
        .find(|u| u.actor_id.as_slice() == carol)
        .expect("carol present");
    assert_eq!(
        carol_row.label, "carol",
        "handle-registered actor's display label should default to its handle"
    );
}

/// The users-list projection carries each actor's read-only IMAP/CalDAV-serving
/// audit indicator (admin.md § Users; deployment-home-with-public-relay.md
/// § MUA reach): default **on**, and `false` for an actor who opted out via the
/// User-class `set_mail_serving_enabled` (here written straight to the DB). The
/// admin only reads it — there is no admin write path.
#[tokio::test]
async fn list_carries_mail_serving_audit_flag() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let optout = [1u8; 32];
    let default_on = [2u8; 32];
    state
        .db
        .create_user(&optout, "free", "optout")
        .await
        .unwrap();
    state
        .db
        .create_user(&default_on, "free", "default_on")
        .await
        .unwrap();
    // The opt-out actor turned serving off on this nest; the other never touched it.
    state
        .db
        .set_actor_mail_serving_enabled(&optout, false)
        .await
        .unwrap();

    let reply: AdminUsersListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.list",
            encode(&AdminUsersListRequest {
                limit: None,
                offset: 0,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    let by_label = |label: &str| -> bool {
        reply
            .users
            .iter()
            .find(|u| u.label == label)
            .unwrap_or_else(|| panic!("user {label} missing"))
            .mail_serving_enabled
    };
    assert!(
        !by_label("optout"),
        "opted-out actor should read serving=off"
    );
    assert!(
        by_label("default_on"),
        "untouched actor should default serving=on"
    );
}

#[tokio::test]
async fn list_on_empty_db_is_zeroed() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminUsersListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.list",
            encode(&AdminUsersListRequest::default()),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(reply.total, 0);
    assert!(reply.users.is_empty());
}

// ── get ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_returns_the_requested_user() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state
        .db
        .create_user(&alice, "personal", "alice")
        .await
        .unwrap();

    let reply: AdminUserGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.get",
            encode(&AdminUserGetRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(reply.user.actor_id.as_ref(), &alice[..]);
    assert_eq!(reply.user.label, "alice");
    assert_eq!(reply.user.tier, "personal");
    assert!(!reply.user.suspended);
    // No eviction in flight on a fresh user.
    assert!(reply.user.eviction.is_none());
}

#[tokio::test]
async fn get_unknown_user_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.get",
        encode(&AdminUserGetRequest {
            actor_id: ByteBuf::from(vec![0xab; 32]),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown user → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

#[tokio::test]
async fn get_with_wrong_length_actor_id_is_invalid_params() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.get",
        encode(&AdminUserGetRequest {
            actor_id: ByteBuf::from(vec![1u8; 16]), // not 32 bytes
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("short actor_id → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

// ── permission gate (Admin-only) ────────────────────────────────────

#[tokio::test]
async fn non_admin_actor_is_permission_denied() {
    let (router, state) = router_and_state().await;
    // A registered non-admin resolves to `CallerClass::User`, so this exercises
    // the LISTED-kind class refusal, which answers with the kind's own family
    // (`api-layers.md` § Caller-class authorization → *Refusal codes at the
    // gate*). ⚠ The actor must be registered: an actor with no `users` row
    // resolves to `None`, not `User`, and takes the unknown/revoked arm — whose
    // deliberately central `fauna.bridges.permission_denied` is asserted
    // separately by `unknown_actor_is_the_central_permission_denied` below.
    let outsider = [99u8; 32];
    state
        .db
        .create_user(&outsider, "free", "outsider")
        .await
        .unwrap();
    let err = dispatch(
        &router,
        state,
        outsider,
        "fauna.admin.users.list",
        encode(&AdminUsersListRequest::default()),
    )
    .await
    .expect_err("non-admin denied");
    assert_eq!(err.code, "fauna.admin.permission_denied");
}

/// The other arm of the same ruling: an actor with **no `users` row** is denied
/// on every kind, and that refusal is deliberately NOT a statement about the
/// kind's family — it keeps the central `fauna.bridges.permission_denied`, the
/// every-kind signal the Go bridges' revocation probe keys on
/// (`rpc_errors::central_permission_denied`). Pinning it here is what stops the
/// next per-family widening from swallowing it.
#[tokio::test]
async fn unknown_actor_is_the_central_permission_denied() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [99u8; 32],
        "fauna.admin.users.list",
        encode(&AdminUsersListRequest::default()),
    )
    .await
    .expect_err("unknown actor denied");
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

// ── malformed payload (after the permission gate passes) ────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.list",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata ─────────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in ADMIN_KINDS {
        let m = router.kind_meta(kind).expect("kind registered");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
        // `invite_codes.create` is the one admin kind that DOES forbid replay
        // (mirrors `KindRegistry::register_admin_kinds`'s own carve-out): an
        // empty `code` mints a fresh random credential and inserts a row keyed
        // on it, so a replay would leave a second independently redeemable
        // admission code.
        if kind == "fauna.admin.invite_codes.create" {
            assert!(
                m.forbid_replay,
                "{kind} mints a credential — must forbid replay"
            );
        } else {
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
        }
    }
}

// ── allowlist ───────────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_admin_only() {
    for kind in ADMIN_KINDS {
        assert!(
            is_permitted(CallerClass::Admin, kind),
            "{kind} permitted for Admin"
        );
        for class in [
            CallerClass::User,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
        ] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}

// ── create ───────────────────────────────────────────────────────────

#[tokio::test]
async fn create_then_get_round_trips() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [3u8; 32];
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.create",
            encode(&AdminUserCreateRequest {
                actor_id: ByteBuf::from(carol.to_vec()),
                tier: "free".into(),
                label: "carol".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    assert!(reply.ok);

    // The row is now readable via the read surface.
    let got: AdminUserGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.get",
            encode(&AdminUserGetRequest {
                actor_id: ByteBuf::from(carol.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(got.user.label, "carol");
    assert_eq!(got.user.tier, "free");
}

/// An admin admitting a user directly may name the handle they are admitted
/// under — the third of the three account-creation paths `public-mode.md`
/// § Registration & Identity enumerates, all of which are meant to carry one
/// ("registering *is* choosing a handle").
///
/// This is what makes an admin-admitted actor able to send mail at all: the
/// nest verifies a same-domain `From:` local part against the caller's handle
/// (`mail-app-surface.md` § First-party client send), so a handle-less actor is refused
/// every `fauna.email.send`.
#[tokio::test]
async fn create_with_a_handle_admits_the_actor_under_it() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [3u8; 32];
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(carol.to_vec()),
            tier: "free".into(),
            handle: Some("carol".into()),
            ..Default::default()
        }),
    )
    .await
    .expect("create with handle ok");

    assert_eq!(
        state.db.get_handle(&carol).await.unwrap().as_deref(),
        Some("carol"),
        "the admitted actor must own the handle on the nest — a client-side \
         handle with no row behind it is exactly what the From-handle gate rejects"
    );
    assert_eq!(
        state.db.resolve_handle("carol").await.unwrap(),
        Some(carol),
        "and the handle must resolve back to the actor"
    );
}

/// Omitting the handle keeps the handle-less admission (the admit form's
/// blank handle), so a caller that never sends the field is unaffected.
#[tokio::test]
async fn create_without_a_handle_still_admits_a_handleless_actor() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [4u8; 32];
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(carol.to_vec()),
            tier: "free".into(),
            label: "carol".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("create without handle ok");

    // NOTE the shape: a registered actor with no handle stores the EMPTY
    // STRING, not NULL — `get_handle` returns `Ok(None)` only for an actor with
    // no `users` row at all. That distinction is load-bearing for the
    // From-handle gate (`email_handlers.rs`), which folds the empty case into
    // its no-handle arm; before it did, a handle-less sender was refused with
    // "from address does not match your handle ()".
    assert_eq!(
        state.db.get_handle(&carol).await.unwrap().as_deref(),
        Some(""),
        "no handle was asked for, so the row carries the empty handle"
    );
}

/// The users-list projection carries each actor's HANDLE — the unique identity
/// an admin picker offers (`admin.md` § 2 → *What identifies a user in an
/// admin picker*; `AdminUser.handle`, additive 2026-08-30). A handle-less
/// admission reports `None` — the DB's empty-string shape folds to
/// absent on the wire, never an empty option string.
#[tokio::test]
async fn users_list_carries_the_handle_and_folds_handleless_to_none() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [3u8; 32];
    let dave = [4u8; 32];
    for req in [
        AdminUserCreateRequest {
            actor_id: ByteBuf::from(carol.to_vec()),
            tier: "free".into(),
            handle: Some("carol".into()),
            ..Default::default()
        },
        AdminUserCreateRequest {
            actor_id: ByteBuf::from(dave.to_vec()),
            tier: "free".into(),
            label: "dave".into(),
            ..Default::default()
        },
    ] {
        dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.create",
            encode(&req),
        )
        .await
        .expect("create ok");
    }

    let reply: AdminUsersListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.list",
            encode(&AdminUsersListRequest::default()),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let row_of = |actor: &[u8; 32]| {
        reply
            .users
            .iter()
            .find(|u| u.actor_id.as_slice() == actor.as_slice())
            .expect("listed")
    };
    assert_eq!(row_of(&carol).handle.as_deref(), Some("carol"));
    assert_eq!(row_of(&dave).handle, None);
}

/// A handle already owned by someone else is a conflict, not a silent
/// overwrite — the same vetting the invite-approval path applies at mint time.
#[tokio::test]
async fn create_with_a_taken_handle_is_a_conflict() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    state
        .db
        .create_user_with_handle(&[3u8; 32], "free", "carol", None)
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(vec![9u8; 32]),
            tier: "free".into(),
            handle: Some("carol".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a taken handle must be refused");
}

/// A malformed handle is refused before any row is written — the format rule
/// is `public-mode.md` § User Registration step 1, enforced identically on
/// every mint path.
#[tokio::test]
async fn create_with_a_malformed_handle_is_refused() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [3u8; 32];

    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(carol.to_vec()),
            tier: "free".into(),
            handle: Some("-nope-".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a malformed handle must be refused");

    assert!(
        state.db.get_user(&carol).await.unwrap().is_none(),
        "a refused handle must leave NO user row behind — validating after the \
         insert would admit the actor and lose only the handle"
    );
}

#[tokio::test]
async fn create_duplicate_is_conflict() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let carol = [3u8; 32];
    state.db.create_user(&carol, "free", "carol").await.unwrap();
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(carol.to_vec()),
            tier: "free".into(),
            label: "dup".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("duplicate → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

// ── update ───────────────────────────────────────────────────────────

#[tokio::test]
async fn update_changes_tier_and_label() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.update",
            encode(&AdminUserUpdateRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                tier: "personal".into(),
                label: "alice-renamed".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("update ok"),
    )
    .unwrap();
    assert!(reply.ok);
    let row = state.db.get_user(&alice).await.unwrap().unwrap();
    assert_eq!(row.tier, "personal");
    assert_eq!(row.label, "alice-renamed");
}

#[tokio::test]
async fn update_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.update",
        encode(&AdminUserUpdateRequest {
            actor_id: ByteBuf::from(vec![0xcd; 32]),
            tier: "free".into(),
            label: String::new(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown user → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ── delete (pending action) ──────────────────────────────────────────

#[tokio::test]
async fn delete_schedules_pending_action() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let reply: AdminPendingActionReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.delete",
            encode(&AdminUserDeleteRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "pending");
    assert!(reply.pending_action_id > 0);
    assert!(reply.execute_after > 0);
}

#[tokio::test]
async fn delete_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.delete",
        encode(&AdminUserDeleteRequest {
            actor_id: ByteBuf::from(vec![0xcd; 32]),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown user → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ── clear_handle ─────────────────────────────────────────────────────

#[tokio::test]
async fn clear_handle_ok() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    state.db.set_handle(&alice, "alice").await.unwrap();
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.clear_handle",
            encode(&AdminUserClearHandleRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("clear_handle ok"),
    )
    .unwrap();
    assert!(reply.ok);
}

// ── evict / cancel_eviction ──────────────────────────────────────────

#[tokio::test]
async fn evict_then_get_shows_eviction() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.evict",
            encode(&AdminUserEvictRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                reason: "spam".into(),
                category: "abuse".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("evict ok"),
    )
    .unwrap();
    assert!(reply.ok);

    let got: AdminUserGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.users.get",
            encode(&AdminUserGetRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    let eviction = got.user.eviction.expect("eviction in flight");
    assert_eq!(eviction.category, "abuse");
    assert_eq!(eviction.reason, "spam");
}

#[tokio::test]
async fn evict_invalid_category_is_invalid_params() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.evict",
        encode(&AdminUserEvictRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            reason: "x".into(),
            category: "bogus".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("bad category → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

#[tokio::test]
async fn evict_already_evicting_is_conflict() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let evict = |s| {
        dispatch(
            &router,
            s,
            admin,
            "fauna.admin.users.evict",
            encode(&AdminUserEvictRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                reason: "spam".into(),
                category: "abuse".into(),
                extra: Default::default(),
            }),
        )
    };
    evict(state.clone()).await.expect("first evict ok");
    let err = evict(state).await.expect_err("second evict → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

#[tokio::test]
async fn cancel_eviction_after_evict_clears_it() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.evict",
        encode(&AdminUserEvictRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            reason: "spam".into(),
            category: "abuse".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("evict ok");
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.cancel_eviction",
            encode(&AdminUserCancelEvictionRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("cancel ok"),
    )
    .unwrap();
    assert!(reply.ok);
    let row = state.db.get_user(&alice).await.unwrap().unwrap();
    assert!(row.eviction_status.is_empty(), "eviction cleared");
}

#[tokio::test]
async fn cancel_eviction_without_active_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.cancel_eviction",
        encode(&AdminUserCancelEvictionRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("no active eviction → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ── suspend (immediate; the eviction machine's `suspended` state) ────────────

/// Suspension is **immediate** — no pending action, no delay. It enters the
/// eviction machine's `suspended` state with no delete timeline, so
/// `cancel_eviction` restores the user (`admin.md` § 2 Users → *Cutting a user
/// off*). Behavioural pins for the dispatch gate live in
/// `conformance_suspension.rs`.
#[tokio::test]
async fn suspend_is_immediate_and_reversible() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.users.suspend",
            encode(&AdminUserSuspendRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                reason: "abuse in progress".into(),
                category: "abuse".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("suspend ok"),
    )
    .unwrap();
    assert!(reply.ok);
    assert!(
        state.db.get_user(&alice).await.unwrap().unwrap().suspended,
        "suspension takes effect immediately, not after a pending-action delay"
    );

    // The single exit, already reachable from the admin UI.
    assert!(state.db.cancel_eviction(&alice).await.unwrap());
    assert!(!state.db.get_user(&alice).await.unwrap().unwrap().suspended);
}

/// `reason` / `category` are optional; the nest fills in its defaults.
#[tokio::test]
async fn suspend_defaults_reason_and_category() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.suspend",
        encode(&AdminUserSuspendRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            ..Default::default()
        }),
    )
    .await
    .expect("suspend ok with empty reason/category");
    assert!(state.db.get_user(&alice).await.unwrap().unwrap().suspended);
}

#[tokio::test]
async fn suspend_rejects_an_invalid_category() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.suspend",
        encode(&AdminUserSuspendRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            category: "nonsense".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("invalid category refused");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

#[tokio::test]
async fn suspend_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.users.suspend",
        encode(&AdminUserSuspendRequest {
            actor_id: ByteBuf::from(vec![0xcd; 32]),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown user → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ═════════════════════════════════════════════════════════════════════════
// C2 — admin-management cluster (tiers / invite codes / invite requests / admins)
// ═════════════════════════════════════════════════════════════════════════

// ── tiers ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tiers_list_returns_seeded_tiers() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminTiersListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.tiers.list",
            encode(&AdminTiersListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    // The migrations seed `free` + `personal`.
    assert!(reply.tiers.iter().any(|t| t.name == "free"));
    assert!(reply.tiers.iter().any(|t| t.name == "personal"));
}

#[tokio::test]
async fn tiers_create_then_list_includes_it() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.tiers.create",
            encode(&AdminTierCreateRequest {
                name: "enterprise".into(),
                max_inbox_bytes: 1,
                max_storage_bytes: 2,
                max_devices: 3,
                max_blob_size: 4,
                max_feeds: 5,
                extra: Default::default(),
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    assert!(reply.ok);
    let row = state.db.get_tier("enterprise").await.unwrap().unwrap();
    assert_eq!(row.max_feeds, 5);
    assert_eq!(row.max_blob_size, 4);
}

#[tokio::test]
async fn tiers_create_empty_name_is_invalid_params() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.tiers.create",
        encode(&AdminTierCreateRequest {
            name: String::new(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("empty name → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

/// A tier cap is a **non-negative** `i64` by ratified rule
/// (`value-formatting.md` § Tier cap validation: every app's parser clamps a
/// negative to `0`). A negative that reaches the DB silently breaks every user
/// on the tier — the storage gate compares `used + delta > max_storage_bytes`,
/// so a negative cap refuses every write — and the admin gets no error at the
/// door. The apps all clamp; the nest is the trust boundary, and a
/// non-conforming client can still send one.
#[tokio::test]
async fn tiers_create_rejects_a_negative_cap() {
    // One negative cap per field, the other four left valid — so the test
    // discriminates all five arms rather than passing on whichever is checked
    // first.
    let cases: [(&str, AdminTierCreateRequest); 5] = [
        (
            "max_inbox_bytes",
            AdminTierCreateRequest {
                max_inbox_bytes: -1,
                ..Default::default()
            },
        ),
        (
            "max_storage_bytes",
            AdminTierCreateRequest {
                max_storage_bytes: -1,
                ..Default::default()
            },
        ),
        (
            "max_devices",
            AdminTierCreateRequest {
                max_devices: -1,
                ..Default::default()
            },
        ),
        (
            "max_blob_size",
            AdminTierCreateRequest {
                max_blob_size: -1,
                ..Default::default()
            },
        ),
        (
            "max_feeds",
            AdminTierCreateRequest {
                max_feeds: -1,
                ..Default::default()
            },
        ),
    ];
    for (field, base) in cases {
        let (router, state) = router_and_state().await;
        let admin = admin_actor(&state).await;
        let req = AdminTierCreateRequest {
            name: "negative".into(),
            ..base
        };
        let err = dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.tiers.create",
            encode(&req),
        )
        .await
        .unwrap_err_or_panic(field);
        assert_eq!(err.code, "fauna.admin.invalid_params", "field={field}");
        // The refusal persists nothing — a negative-capped tier row would then
        // be assignable to a user.
        assert!(
            state.db.get_tier("negative").await.unwrap().is_none(),
            "field={field} must not persist a tier row"
        );
    }
}

/// Local helper: `expect_err` with the failing field named in the panic.
trait UnwrapErrOrPanic<E> {
    fn unwrap_err_or_panic(self, field: &str) -> E;
}

impl<T: std::fmt::Debug, E> UnwrapErrOrPanic<E> for Result<T, E> {
    fn unwrap_err_or_panic(self, field: &str) -> E {
        match self {
            Ok(v) => panic!("{field}: expected a refusal, got Ok({v:?})"),
            Err(e) => e,
        }
    }
}

/// `0` is an explicitly **valid** cap — an admin-chosen "no allowance"
/// (`value-formatting.md` § Tier cap validation). The negative refusal must not
/// over-correct into a `>= 1` floor.
#[tokio::test]
async fn tiers_create_accepts_a_zero_cap() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.tiers.create",
            encode(&AdminTierCreateRequest {
                name: "noallowance".into(),
                max_inbox_bytes: 0,
                max_storage_bytes: 0,
                max_devices: 0,
                max_blob_size: 0,
                max_feeds: 0,
                extra: Default::default(),
            }),
        )
        .await
        .expect("an all-zero tier is a valid no-allowance tier"),
    )
    .unwrap();
    assert!(reply.ok);
    let row = state.db.get_tier("noallowance").await.unwrap().unwrap();
    assert_eq!(row.max_storage_bytes, 0);
}

/// The same floor on `update` — a tier created valid must not be editable into
/// the broken state the create door refuses.
#[tokio::test]
async fn tiers_update_rejects_a_negative_cap() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.tiers.update",
        encode(&AdminTierUpdateRequest {
            name: "free".into(),
            max_storage_bytes: -1,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a negative cap on update → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
    // The seeded tier is untouched by the refused edit.
    let row = state.db.get_tier("free").await.unwrap().unwrap();
    assert!(row.max_storage_bytes >= 0);
}

#[tokio::test]
async fn tiers_create_duplicate_is_conflict() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    // `free` is seeded.
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.tiers.create",
        encode(&AdminTierCreateRequest {
            name: "free".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("dup tier → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

#[tokio::test]
async fn tiers_update_existing_ok() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.tiers.update",
            encode(&AdminTierUpdateRequest {
                name: "free".into(),
                max_inbox_bytes: 111,
                max_storage_bytes: 222,
                max_devices: 3,
                max_blob_size: 4,
                max_feeds: 9,
                extra: Default::default(),
            }),
        )
        .await
        .expect("update ok"),
    )
    .unwrap();
    assert!(reply.ok);
    let row = state.db.get_tier("free").await.unwrap().unwrap();
    assert_eq!(row.max_inbox_bytes, 111);
    assert_eq!(row.max_feeds, 9);
}

#[tokio::test]
async fn tiers_update_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.tiers.update",
        encode(&AdminTierUpdateRequest {
            name: "ghost".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown tier → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ── invite codes ───────────────────────────────────────────────────────────

#[tokio::test]
async fn invite_codes_create_then_list_and_delete() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;

    let created: AdminInviteCodeCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_codes.create",
            encode(&AdminInviteCodeCreateRequest {
                code: "WELCOME".into(),
                tier: "personal".into(),
                uses: 3,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    assert!(created.ok);
    // A supplied code is echoed back verbatim.
    assert_eq!(created.code, "WELCOME");

    let list: AdminInviteCodesListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_codes.list",
            encode(&AdminInviteCodesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let entry = list
        .invite_codes
        .iter()
        .find(|c| c.code == "WELCOME")
        .expect("created code listed");
    assert_eq!(entry.tier, "personal");
    assert_eq!(entry.uses_left, 3);

    let deleted: AdminOkReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.invite_codes.delete",
            encode(&AdminInviteCodeDeleteRequest {
                code: "WELCOME".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap();
    assert!(deleted.ok);
}

#[tokio::test]
async fn invite_codes_create_empty_mints_a_code() {
    // An empty `code` means "mint one": the nest generates a random token and
    // returns it, rather than rejecting the request. The admin chose tier + uses.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminInviteCodeCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_codes.create",
            encode(&AdminInviteCodeCreateRequest {
                code: String::new(),
                tier: "free".into(),
                uses: 5,
                ..Default::default()
            }),
        )
        .await
        .expect("empty code → minted"),
    )
    .unwrap();
    assert!(reply.ok);
    assert!(!reply.code.is_empty(), "nest minted a non-empty code");

    // The minted code is real: it shows up in the list with the chosen tier/uses.
    let list: AdminInviteCodesListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.invite_codes.list",
            encode(&AdminInviteCodesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let entry = list
        .invite_codes
        .iter()
        .find(|c| c.code == reply.code)
        .expect("minted code listed");
    assert_eq!(entry.tier, "free");
    assert_eq!(entry.uses_left, 5);
}

#[tokio::test]
async fn invite_codes_create_duplicate_is_conflict() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    state.db.create_invite_code("DUP", "free", 1).await.unwrap();
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.invite_codes.create",
        encode(&AdminInviteCodeCreateRequest {
            code: "DUP".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("dup code → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

/// A mint at `uses <= 0` is a **born-dead code**: redemption requires
/// `uses_left > 0`, so the admin hands out a token every invitee is told is
/// invalid. All 7 apps clamp to `>= 1`, but the nest is the trust boundary, so the
/// door is where the floor has to live.
#[tokio::test]
async fn invite_codes_create_rejects_uses_below_one() {
    for bad in [0i64, -1, i64::MIN] {
        let (router, state) = router_and_state().await;
        let admin = admin_actor(&state).await;
        let err = dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_codes.create",
            encode(&AdminInviteCodeCreateRequest {
                code: "BORNDEAD".into(),
                uses: bad,
                ..Default::default()
            }),
        )
        .await
        .expect_err("uses <= 0 → invalid_params");
        assert_eq!(err.code, "fauna.admin.invalid_params", "uses={bad}");
        // The refusal must leave nothing behind — a persisted born-dead row
        // would still list in the admin hub.
        assert!(
            state
                .db
                .peek_invite_code("BORNDEAD")
                .await
                .unwrap()
                .is_none(),
            "uses={bad} must not persist a row"
        );
    }
}

/// The floor is `1`, not `2` — one-use invites are the common case.
#[tokio::test]
async fn invite_codes_create_accepts_a_single_use() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminInviteCodeCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_codes.create",
            encode(&AdminInviteCodeCreateRequest {
                code: "ONESHOT".into(),
                uses: 1,
                ..Default::default()
            }),
        )
        .await
        .expect("uses = 1 is valid"),
    )
    .unwrap();
    assert!(reply.ok);
    assert!(
        state
            .db
            .peek_invite_code("ONESHOT")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn invite_codes_delete_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.invite_codes.delete",
        encode(&AdminInviteCodeDeleteRequest {
            code: "NOPE".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown code → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ── invite requests ──────────────────────────────────────────────────────

#[tokio::test]
async fn invite_requests_list_returns_pending() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let applicant = [0x11u8; 32];
    let id = state
        .db
        .create_invite_request(&applicant, "newbie", "let me in", None)
        .await
        .unwrap();
    let reply: AdminInviteRequestsListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.invite_requests.list",
            encode(&AdminInviteRequestsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let r = reply
        .invite_requests
        .iter()
        .find(|r| r.id == id)
        .expect("request listed");
    assert_eq!(r.handle, "newbie");
    assert_eq!(r.status, "pending");
    assert_eq!(r.actor_id.as_ref(), &applicant[..]);
}

#[tokio::test]
async fn invite_requests_approve_creates_user() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let applicant = [0x22u8; 32];
    let id = state
        .db
        .create_invite_request(&applicant, "approveme", "", None)
        .await
        .unwrap();
    let reply: AdminInviteRequestApproveReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_requests.approve",
            encode(&AdminInviteRequestApproveRequest {
                id,
                tier: Some("personal".into()),
                label: Some("Alice".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("approve ok"),
    )
    .unwrap();
    assert_eq!(reply.actor_id.as_ref(), &applicant[..]);
    assert_eq!(reply.handle, "approveme");
    assert_eq!(reply.tier, "personal");

    // User now exists; the request is gone.
    let user = state.db.get_user(&applicant).await.unwrap().unwrap();
    assert_eq!(user.tier, "personal");
    assert_eq!(user.label, "Alice");
    assert!(state.db.get_invite_request(id).await.unwrap().is_none());
}

#[tokio::test]
async fn invite_requests_approve_defaults_to_free_tier() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let applicant = [0x23u8; 32];
    let id = state
        .db
        .create_invite_request(&applicant, "freebie", "", None)
        .await
        .unwrap();
    let reply: AdminInviteRequestApproveReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.invite_requests.approve",
            encode(&AdminInviteRequestApproveRequest {
                id,
                tier: None,
                label: None,
                ..Default::default()
            }),
        )
        .await
        .expect("approve ok"),
    )
    .unwrap();
    assert_eq!(reply.tier, "free");
}

#[tokio::test]
async fn invite_requests_approve_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.invite_requests.approve",
        encode(&AdminInviteRequestApproveRequest {
            id: 9999,
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown request → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

#[tokio::test]
async fn invite_requests_approve_non_pending_is_conflict() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let applicant = [0x24u8; 32];
    let id = state
        .db
        .create_invite_request(&applicant, "denied-then-approve", "", None)
        .await
        .unwrap();
    // Deny it first → status becomes non-pending.
    state
        .db
        .deny_invite_request(id, &admin, Some("no"))
        .await
        .unwrap();
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.invite_requests.approve",
        encode(&AdminInviteRequestApproveRequest {
            id,
            ..Default::default()
        }),
    )
    .await
    .expect_err("non-pending → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

#[tokio::test]
async fn invite_requests_approve_refuses_reserved_handle() {
    // A pending row can hold a reserved handle from before the reserved list
    // was in force at submission time (e.g. the 2026-03-16→2026-07-17 window
    // where the nest's `--reserved-handle` flag silently emptied the default
    // list on every real deployment, fixed). Approval must
    // re-validate the stored handle rather than trust the submission-time
    // check, so such a row can never mint `postmaster`.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let applicant = [0x26u8; 32];
    let id = state
        .db
        .create_invite_request(&applicant, "postmaster", "", None)
        .await
        .unwrap();
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.invite_requests.approve",
        encode(&AdminInviteRequestApproveRequest {
            id,
            ..Default::default()
        }),
    )
    .await
    .expect_err("reserved handle → refused at approval");
    assert_eq!(err.code, "fauna.admin.invalid_params");
    // No user minted; the request stays pending for the admin to resolve.
    assert!(state.db.get_user(&applicant).await.unwrap().is_none());
    let row = state.db.get_invite_request(id).await.unwrap().unwrap();
    assert_eq!(row.status, "pending");
}

#[tokio::test]
async fn invite_requests_deny_marks_denied() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let applicant = [0x25u8; 32];
    let id = state
        .db
        .create_invite_request(&applicant, "denyme", "", None)
        .await
        .unwrap();
    let reply: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.invite_requests.deny",
            encode(&AdminInviteRequestDenyRequest {
                id,
                reason: Some("not eligible".into()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("deny ok"),
    )
    .unwrap();
    assert!(reply.ok);
    let row = state.db.get_invite_request(id).await.unwrap().unwrap();
    assert_eq!(row.status, "denied");
    assert_eq!(row.denial_reason.as_deref(), Some("not eligible"));
}

#[tokio::test]
async fn invite_requests_deny_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.invite_requests.deny",
        encode(&AdminInviteRequestDenyRequest {
            id: 9999,
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown request → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

// ── admins ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn admins_list_includes_the_caller() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminAdminsListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.admins.list",
            encode(&AdminAdminsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(
        reply
            .admins
            .iter()
            .any(|a| a.actor_id.as_ref() == &admin[..])
    );
}

#[tokio::test]
async fn admins_add_schedules_pending_action() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    // `Admin ⊇ User`: the promotion target must already hold a `users` row, or
    // `admins.add` refuses it (`fauna.admin.not_found`) — at the door and again in
    // the executor. Promoting an unregistered actor would mint an admin with no
    // account, invisible to every guard that reasons over `users`.
    let target = [0x44u8; 32];
    state
        .db
        .create_user(&target, "free", "target")
        .await
        .unwrap();
    let reply: AdminPendingActionReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.admins.add",
            encode(&AdminAdminAddRequest {
                actor_id: ByteBuf::from(target.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("add ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "pending");
    assert!(reply.pending_action_id > 0);
    assert!(reply.execute_after > 0);
}

/// `Admin ⊇ User` also refuses a RETIRED identity (`admin.md` § Admin
/// continuity and succession). A succeeded key keeps a handle-less `users`
/// row until its successor is deleted, so `get_user` alone answers "registered"
/// for a key that can never log in again: the grant would count dead weight in
/// `admin_count` and the removal quorum, and the row would later leave with the
/// successor's deletion chain, bypassing the superadmin floor.
#[tokio::test]
async fn admins_add_refuses_a_superseded_key() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let (retired, successor) = ([0x45u8; 32], [0x46u8; 32]);
    state.db.create_user(&retired, "free", "").await.unwrap();
    state
        .db
        .record_succession(&retired, &successor, b"s", 1)
        .await
        .unwrap()
        .unwrap();
    assert!(
        state.db.get_user(&retired).await.unwrap().is_some(),
        "precondition: the retired key keeps its handle-less users row"
    );
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.admins.add",
        encode(&AdminAdminAddRequest {
            actor_id: ByteBuf::from(retired.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a superseded key must not be granted the admin role");
    assert_eq!(err.code, "fauna.admin.not_found", "got: {err:?}");
    assert!(
        state
            .db
            .list_all_pending_actions()
            .await
            .unwrap()
            .is_empty(),
        "nothing may be scheduled for a retired key"
    );
}

#[tokio::test]
async fn admins_add_wrong_length_is_invalid_params() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.admins.add",
        encode(&AdminAdminAddRequest {
            actor_id: ByteBuf::from(vec![0x44u8; 16]),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("short actor_id → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

#[tokio::test]
async fn admins_remove_last_superadmin_is_conflict() {
    let (router, state) = router_and_state().await;
    // Exactly one superadmin (the test caller).
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.admins.remove",
        encode(&AdminAdminRemoveRequest {
            actor_id: ByteBuf::from(admin.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("last superadmin → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

#[tokio::test]
async fn admins_remove_with_two_superadmins_schedules() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    // A second superadmin makes removal safe (the target-aware
    // `can_remove_admin`: another superadmin remains).
    let other = [0x55u8; 32];
    state.db.add_admin_actor(&other[..]).await.unwrap();
    let reply: AdminPendingActionReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.admins.remove",
            encode(&AdminAdminRemoveRequest {
                actor_id: ByteBuf::from(other.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("remove ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "pending");
    assert!(reply.pending_action_id > 0);
}

/// The door's floor is target-aware: removing a NON-superadmin admin
/// never threatens the superadmin floor, so a sole superadmin must not block
/// it. The old global-count guard (`admin_count_by_role >= 2`, target ignored)
/// refused exactly this legitimate roster maintenance.
#[tokio::test]
async fn admins_remove_a_moderator_schedules_beside_a_sole_superadmin() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await; // the sole superadmin
    let moderator = [0x66u8; 32];
    state.db.add_admin_actor(&moderator[..]).await.unwrap();
    state
        .db
        .set_admin_role(&moderator[..], "moderator")
        .await
        .unwrap();
    let reply: AdminPendingActionReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.admins.remove",
            encode(&AdminAdminRemoveRequest {
                actor_id: ByteBuf::from(moderator.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("a moderator's removal must schedule — the floor is about superadmins"),
    )
    .unwrap();
    assert_eq!(reply.status, "pending");
}

// ── deployment_seed.get (the co-admin seed hand-off) ──────────────────────────

/// `router_and_state` but with a populated `nest_signing_key`, so the
/// hand-off has a real seed to return — mirroring the pattern
/// `subscription_handlers.rs`'s `fixture_state_with_signing_key` and
/// `bridge_atproto_handlers.rs`'s tests already use for the same field.
async fn router_and_state_with_signing_key() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    state.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]));
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    (b.build(), state)
}

#[tokio::test]
async fn deployment_seed_get_hands_the_current_admin_the_seed() {
    let (router, state) = router_and_state_with_signing_key().await;
    let admin = admin_actor(&state).await;
    let reply: AdminDeploymentSeedGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.deployment_seed.get",
            encode(&AdminDeploymentSeedGetRequest::default()),
        )
        .await
        .expect("admin gets the seed"),
    )
    .unwrap();
    assert_eq!(
        reply.deployment_seed.as_deref(),
        Some(hex::encode([5u8; 32]).as_str())
    );
}

#[tokio::test]
async fn deployment_seed_get_a_co_admin_gets_the_same_seed_a_claiming_admin_would() {
    // The whole point of the kind: a SECOND admin, never present at claim,
    // reads the identical seed the claim-reply hand-off gives the first.
    let (router, state) = router_and_state_with_signing_key().await;
    let claiming_admin = admin_actor(&state).await;
    let co_admin = [0x42u8; 32];
    state.db.add_admin_actor(&co_admin[..]).await.unwrap();
    let get = |actor: [u8; 32]| {
        let router = &router;
        let state = state.clone();
        async move {
            let reply: AdminDeploymentSeedGetReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.admin.deployment_seed.get",
                    encode(&AdminDeploymentSeedGetRequest::default()),
                )
                .await
                .expect("admin gets the seed"),
            )
            .unwrap();
            reply.deployment_seed
        }
    };
    let claiming_seed = get(claiming_admin).await;
    let co_admin_seed = get(co_admin).await;
    assert!(claiming_seed.is_some());
    assert_eq!(claiming_seed.as_deref(), co_admin_seed.as_deref());
}

#[tokio::test]
async fn deployment_seed_get_non_admin_is_permission_denied() {
    let (router, state) = router_and_state_with_signing_key().await;
    // Registered, so this is a `User`-class refusal on a listed kind — see
    // `non_admin_actor_is_permission_denied` for why the row matters.
    let outsider = [99u8; 32];
    state
        .db
        .create_user(&outsider, "free", "outsider")
        .await
        .unwrap();
    let err = dispatch(
        &router,
        state,
        outsider,
        "fauna.admin.deployment_seed.get",
        encode(&AdminDeploymentSeedGetRequest::default()),
    )
    .await
    .expect_err("non-admin denied");
    assert_eq!(err.code, "fauna.admin.permission_denied");
}

#[tokio::test]
async fn deployment_seed_get_with_no_signing_key_returns_none_not_an_error() {
    // `for_test` leaves `nest_signing_key: None` — the should-not-happen
    // post-boot case. A benign `None`, never an error, so a caller can treat
    // it as "nothing to capture yet" uniformly with every other soft outcome.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminDeploymentSeedGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.deployment_seed.get",
            encode(&AdminDeploymentSeedGetRequest::default()),
        )
        .await
        .expect("no error even with no signing key"),
    )
    .unwrap();
    assert!(reply.deployment_seed.is_none());
}

// ── cross-cluster: a non-admin caller is denied a C2 kind ────────────────────

#[tokio::test]
async fn c2_non_admin_actor_is_permission_denied() {
    let (router, state) = router_and_state().await;
    // A registered non-admin resolves to `User`, not `Admin`. ⚠ A
    // *never-enrolled* actor resolves to `None` instead and takes the
    // unknown-actor arm (central bridges code) — see
    // `non_admin_actor_is_permission_denied`.
    let nobody = [0x99u8; 32];
    state
        .db
        .create_user(&nobody, "free", "nobody")
        .await
        .unwrap();
    let err = dispatch(
        &router,
        state,
        nobody,
        "fauna.admin.tiers.list",
        encode(&AdminTiersListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("non-admin → permission_denied");
    assert_eq!(err.code, "fauna.admin.permission_denied");
}

// ── evictions.list ───────────────────────────────────────────────────

#[tokio::test]
async fn evictions_list_includes_only_evicted_users() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    let bob = [2u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    state.db.create_user(&bob, "free", "bob").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.evict",
        encode(&AdminUserEvictRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            reason: "spam".into(),
            category: "abuse".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("evict ok");

    let reply: AdminEvictionsListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.evictions.list",
            encode(&AdminEvictionsListRequest::default()),
        )
        .await
        .expect("evictions.list ok"),
    )
    .unwrap();
    assert_eq!(reply.evictions.len(), 1);
    assert_eq!(reply.evictions[0].actor_id.as_ref(), &alice[..]);
    assert!(reply.evictions[0].eviction.is_some());
}

// ── permission gate on a mutation ────────────────────────────────────

#[tokio::test]
async fn non_admin_denied_on_create() {
    let (router, state) = router_and_state().await;
    // Registered — a `User`-class refusal on a listed kind. See
    // `non_admin_actor_is_permission_denied`.
    let outsider = [99u8; 32];
    state
        .db
        .create_user(&outsider, "free", "outsider")
        .await
        .unwrap();
    let err = dispatch(
        &router,
        state,
        outsider,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(vec![3u8; 32]),
            tier: "free".into(),
            label: "x".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("non-admin denied");
    assert_eq!(err.code, "fauna.admin.permission_denied");
}

// ── C3: stats / audit / ops ──────────────────────────────────────────

#[tokio::test]
async fn stats_counts_users_and_zero_connections() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    state
        .db
        .create_user(&[1u8; 32], "free", "alice")
        .await
        .unwrap();
    state
        .db
        .create_user(&[2u8; 32], "personal", "bob")
        .await
        .unwrap();
    let reply: AdminStatsReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.stats",
            encode(&AdminStatsRequest::default()),
        )
        .await
        .expect("stats ok"),
    )
    .unwrap();
    assert_eq!(reply.total_users, 2);
    // `users_by_tier` is the `(tier, count)` pairs the twin emits.
    assert!(
        reply
            .users_by_tier
            .iter()
            .any(|(t, c)| t == "free" && *c == 1)
    );
    assert!(
        reply
            .users_by_tier
            .iter()
            .any(|(t, c)| t == "personal" && *c == 1)
    );
    // No live WS connections in `for_test`.
    assert_eq!(reply.ws_connections, 0);
}

#[tokio::test]
async fn status_reports_version_and_no_update() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminStatusReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.status",
            encode(&AdminStatusRequest::default()),
        )
        .await
        .expect("status ok"),
    )
    .unwrap();
    assert!(!reply.version.is_empty());
    // `for_test` seeds `update_status` with `None`.
    assert!(reply.update_available.is_none());
}

#[tokio::test]
async fn audit_list_returns_written_entries_newest_first() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    // Two audit writes; the list is newest-first.
    state
        .db
        .audit(Some(&admin[..]), "test.one", Some("t1"), None)
        .await
        .unwrap();
    state
        .db
        .audit(Some(&admin[..]), "test.two", Some("t2"), Some("detail"))
        .await
        .unwrap();
    let reply: AdminAuditListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.audit.list",
            encode(&AdminAuditListRequest::default()),
        )
        .await
        .expect("audit.list ok"),
    )
    .unwrap();
    assert!(reply.entries.len() >= 2);
    // Newest first: the most recent write is `test.two`.
    assert_eq!(reply.entries[0].action, "test.two");
    assert_eq!(
        reply.entries[0].actor_id.as_ref().map(|b| b.as_ref()),
        Some(&admin[..])
    );
    assert_eq!(reply.entries[0].detail.as_deref(), Some("detail"));
    // The hash-chain links are populated.
    assert!(!reply.entries[0].entry_hash.is_empty());
}

#[tokio::test]
async fn audit_list_clamps_limit() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    for i in 0..5 {
        state
            .db
            .audit(Some(&admin[..]), "test.x", Some(&format!("{i}")), None)
            .await
            .unwrap();
    }
    // limit below the floor clamps to 1.
    let reply: AdminAuditListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.audit.list",
            encode(&AdminAuditListRequest {
                limit: Some(0),
                before_id: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("audit.list ok"),
    )
    .unwrap();
    assert_eq!(reply.entries.len(), 1);
}

#[tokio::test]
async fn audit_integrity_returns_chain_summary() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    state
        .db
        .audit(Some(&admin[..]), "test.one", None, None)
        .await
        .unwrap();
    state
        .db
        .audit(Some(&admin[..]), "test.two", None, None)
        .await
        .unwrap();
    let reply: AdminAuditIntegrityReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.audit.integrity",
            encode(&AdminAuditIntegrityRequest::default()),
        )
        .await
        .expect("audit.integrity ok"),
    )
    .unwrap();
    assert_eq!(reply.chain_length, 2);
    assert!(reply.head_id > 0);
    assert!(!reply.head_hash.is_empty());
}

#[tokio::test]
async fn cluster_status_is_zeroed_on_empty_db() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminClusterStatusReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.cluster.status",
            encode(&AdminClusterStatusRequest::default()),
        )
        .await
        .expect("cluster.status ok"),
    )
    .unwrap();
    assert_eq!(reply.total_blobs, 0);
    assert_eq!(reply.total_bytes, 0);
    assert_eq!(reply.local_blobs, 0);
    assert_eq!(reply.s3_blobs, 0);
}

#[tokio::test]
async fn gc_without_backup_is_invalid_params() {
    // `for_test` leaves `backup_service = None` (the twin returned 400).
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.gc",
        encode(&AdminGcRequest::default()),
    )
    .await
    .expect_err("gc without backup → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

/// `fauna.admin.gc` deletes user data, and a **negative** grace period inverts
/// the window rather than shrinking it: `gc.rs` keeps a blob iff
/// `created_at > now - grace_period_secs`, so a negative cutoff sits in the
/// future, no blob can exceed it, and every unreferenced blob — including one
/// written moments ago that no snapshot references yet — becomes deletable.
/// That is the in-flight-writer race the grace window exists to prevent.
///
/// Asserted on the details string, not just the code: `gc` answers
/// `invalid_params` for backup-not-configured too, so the code alone would not
/// discriminate this refusal from that one.
#[tokio::test]
async fn gc_rejects_a_negative_grace_period() {
    for bad in [-1i64, -1800, i64::MIN] {
        let (router, state) = router_and_state().await;
        let admin = admin_actor(&state).await;
        let err = dispatch(
            &router,
            state,
            admin,
            "fauna.admin.gc",
            encode(&AdminGcRequest {
                grace_period_secs: Some(bad),
                ..Default::default()
            }),
        )
        .await
        .expect_err("a negative grace period → invalid_params");
        assert_eq!(err.code, "fauna.admin.invalid_params", "grace={bad}");
        let details = format!("{:?}", err.details);
        assert!(
            details.contains("grace_period_secs"),
            "grace={bad}: refused for the wrong reason — {details}"
        );
    }
}

/// `0` is a coherent admin choice ("collect everything unreferenced now"), so
/// the negative refusal must not become a `>= 1` floor. `for_test` configures no
/// backup service, so a *valid* grace period gets as far as that refusal — which
/// is what proves `0` passed the range check.
#[tokio::test]
async fn gc_accepts_a_zero_grace_period() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.gc",
        encode(&AdminGcRequest {
            grace_period_secs: Some(0),
            ..Default::default()
        }),
    )
    .await
    .expect_err("no backup service configured in `for_test`");
    let details = format!("{:?}", err.details);
    assert!(
        !details.contains("grace_period_secs"),
        "a 0 grace period must pass the range check, got {details}"
    );
}

#[tokio::test]
async fn worker_status_reports_disconnected() {
    // `for_test` uses `BridgeState::default()` — no worker connected.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminWorkerStatusReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.worker.status",
            encode(&AdminWorkerStatusRequest::default()),
        )
        .await
        .expect("worker.status ok"),
    )
    .unwrap();
    assert!(!reply.connected);
    assert_eq!(reply.replication_count, 0);
    assert!(reply.worker.is_none());
}

#[tokio::test]
async fn pending_actions_list_returns_cross_actor_rows() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [1u8; 32];
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    // Schedule a pending action via the C1 delete kind.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.users.delete",
        encode(&AdminUserDeleteRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("delete schedules a pending action");

    let reply: AdminPendingActionsListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.pending_actions.list",
            encode(&AdminPendingActionsListRequest::default()),
        )
        .await
        .expect("pending_actions.list ok"),
    )
    .unwrap();
    assert_eq!(reply.actions.len(), 1);
    let a = &reply.actions[0];
    // The row's actor is the scheduling admin (caller); the target is alice.
    assert_eq!(a.actor_id.as_ref(), &admin[..]);
    assert_eq!(a.target.as_deref(), Some(hex::encode(alice).as_str()));
    assert_eq!(a.status, "pending");
    assert!(a.approvals.is_empty());
}

#[tokio::test]
async fn pending_actions_list_empty_on_fresh_db() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let reply: AdminPendingActionsListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.pending_actions.list",
            encode(&AdminPendingActionsListRequest::default()),
        )
        .await
        .expect("pending_actions.list ok"),
    )
    .unwrap();
    assert!(reply.actions.is_empty());
}

// ── C5: folders ────────────────────────────────────────────────────

#[tokio::test]
async fn folder_create_then_get_round_trips() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let owner = [0x11u8; 32];
    let create: AdminFolderCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.folders.create",
            encode(&AdminFolderCreateRequest {
                name: "photos".into(),
                actor_id: ByteBuf::from(owner.to_vec()),
                node_cache: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    assert!(create.id > 0);
    assert_eq!(create.name, "photos");

    let got: AdminFolderGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.folders.get",
            encode(&AdminFolderGetRequest {
                name: "photos".into(),
                actor_id: None,
                ..Default::default()
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(got.id, create.id);
    assert_eq!(got.name, "photos");
    assert_eq!(got.actor_id.as_ref(), &owner[..]);
    assert!(got.node_cache);
    assert!(got.members.is_empty());
}

#[tokio::test]
async fn folder_create_duplicate_name_is_conflict() {
    // `folders.name` is UNIQUE — the HTTP twin silently 500'd on a duplicate
    // (it wrapped the rusqlite error in `.context(...)`); the kind returns the
    // intended conflict (the C1/C2 latent-bug-fix precedent).
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let req = AdminFolderCreateRequest {
        name: "dup".into(),
        actor_id: ByteBuf::from(vec![1u8; 32]),
        node_cache: None,
        ..Default::default()
    };
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.create",
        encode(&req),
    )
    .await
    .expect("first create ok");
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.folders.create",
        encode(&req),
    )
    .await
    .expect_err("duplicate → conflict");
    assert_eq!(err.code, "fauna.admin.conflict");
}

/// the admin create refuses reserved (`__`) names unconditionally.
/// `get_or_create_reserved_folder` is `INSERT OR IGNORE`, so a pre-created
/// reserved name would be silently ADOPTED as the actor's rail — in a namespace
/// the nest routes on by literal name. The user-class create's one carve-out
/// (a custody destination) does not apply here: this kind takes
/// no mode, and the coordinator provisions custody sets over
/// `fauna.folders.create`.
#[tokio::test]
async fn folder_create_refuses_reserved_names() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let target = [0x66u8; 32];
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.create",
        encode(&AdminFolderCreateRequest {
            name: "__config".into(),
            actor_id: ByteBuf::from(target.to_vec()),
            node_cache: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("an admin pre-create of a reserved rail must refuse");
    assert_eq!(err.code, "fauna.admin.invalid_params");
    assert!(
        state
            .db
            .get_folder_for_actor("__config", &target)
            .await
            .unwrap()
            .is_none(),
        "no admin-shaped row exists for the rail mint to adopt"
    );
}

#[tokio::test]
async fn folder_create_wrong_length_actor_id_is_invalid_params() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.folders.create",
        encode(&AdminFolderCreateRequest {
            name: "x".into(),
            actor_id: ByteBuf::from(vec![1u8; 16]), // too short
            node_cache: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("short actor_id → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

#[tokio::test]
async fn folder_get_unknown_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.folders.get",
        encode(&AdminFolderGetRequest {
            name: "nope".into(),
            actor_id: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

#[tokio::test]
async fn folder_add_member_then_get_lists_it() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let device = [0x33u8; 32];
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.create",
        encode(&AdminFolderCreateRequest {
            name: "fs".into(),
            actor_id: ByteBuf::from(vec![1u8; 32]),
            node_cache: None,
            ..Default::default()
        }),
    )
    .await
    .expect("create ok");
    let ok: AdminOkReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.folders.add_member",
            encode(&AdminFolderAddMemberRequest {
                name: "fs".into(),
                actor_id: None,
                device_id: ByteBuf::from(device.to_vec()),
                flags: fauna_protocol::folders::PlaceFlags::new(true, false, false),
                ..Default::default()
            }),
        )
        .await
        .expect("add_member ok"),
    )
    .unwrap();
    assert!(ok.ok);

    let got: AdminFolderGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.folders.get",
            encode(&AdminFolderGetRequest {
                name: "fs".into(),
                actor_id: None,
                ..Default::default()
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(got.members.len(), 1);
    assert_eq!(got.members[0].device_id.as_ref(), &device[..]);
    assert_eq!(got.members[0].flags.point(), (true, false, false));
}

#[tokio::test]
async fn folder_add_member_malformed_device_id_is_invalid_params() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.create",
        encode(&AdminFolderCreateRequest {
            name: "fs".into(),
            actor_id: ByteBuf::from(vec![1u8; 32]),
            node_cache: None,
            ..Default::default()
        }),
    )
    .await
    .expect("create ok");
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.folders.add_member",
        encode(&AdminFolderAddMemberRequest {
            name: "fs".into(),
            actor_id: None,
            device_id: ByteBuf::from(vec![2u8; 7]),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a short device id → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

#[tokio::test]
async fn folder_add_member_unknown_set_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.folders.add_member",
        encode(&AdminFolderAddMemberRequest {
            name: "absent".into(),
            actor_id: None,
            device_id: ByteBuf::from(vec![2u8; 32]),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown set → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

/// Multi-actor disambiguation: `folders` is `UNIQUE(name, actor_id)` (not
/// `name` alone), so two actors can each own a set named "photos". The admin
/// by-name kinds must (a) accept an `actor_id` that scopes the lookup to one
/// owner, (b) error honestly (`invalid_params`) on a bare ambiguous name
/// instead of silently returning whichever row sorts first, and (c) keep the
/// legacy bare-name behavior when the name is unique.
#[tokio::test]
async fn folder_admin_kinds_disambiguate_same_named_sets_across_actors() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [0xAAu8; 32];
    let bob = [0xBBu8; 32];

    // Two actors, same set name — both creates succeed (per-actor uniqueness).
    for owner in [&alice, &bob] {
        dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.folders.create",
            encode(&AdminFolderCreateRequest {
                name: "photos".into(),
                actor_id: ByteBuf::from(owner.to_vec()),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok (per-actor unique)");
    }

    // (a) actor_id scopes GET to the right owner's row.
    for owner in [&alice, &bob] {
        let got: AdminFolderGetReply = decode(
            &dispatch(
                &router,
                state.clone(),
                admin,
                "fauna.admin.folders.get",
                encode(&AdminFolderGetRequest {
                    name: "photos".into(),
                    actor_id: Some(ByteBuf::from(owner.to_vec())),
                    ..Default::default()
                }),
            )
            .await
            .expect("scoped get ok"),
        )
        .unwrap();
        assert_eq!(got.actor_id.as_ref(), &owner[..], "got the OWNER's row");
    }

    // (b) a bare ambiguous name errors instead of picking arbitrarily.
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.get",
        encode(&AdminFolderGetRequest {
            name: "photos".into(),
            actor_id: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("ambiguous bare name → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    // Same honest error on the mutating kinds (never mutate an arbitrary row).
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.add_member",
        encode(&AdminFolderAddMemberRequest {
            name: "photos".into(),
            actor_id: None,
            device_id: ByteBuf::from(vec![3u8; 32]),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("ambiguous add_member → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    // (a) scoped add_member lands on Bob's set only.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.add_member",
        encode(&AdminFolderAddMemberRequest {
            name: "photos".into(),
            actor_id: Some(ByteBuf::from(bob.to_vec())),
            device_id: ByteBuf::from(vec![3u8; 32]),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        }),
    )
    .await
    .expect("scoped add_member ok");
    for (owner, expect_members) in [(&alice, 0usize), (&bob, 1usize)] {
        let got: AdminFolderGetReply = decode(
            &dispatch(
                &router,
                state.clone(),
                admin,
                "fauna.admin.folders.get",
                encode(&AdminFolderGetRequest {
                    name: "photos".into(),
                    actor_id: Some(ByteBuf::from(owner.to_vec())),
                    ..Default::default()
                }),
            )
            .await
            .expect("scoped get ok"),
        )
        .unwrap();
        assert_eq!(
            got.members.len(),
            expect_members,
            "member landed on Bob's set only"
        );
    }

    // A scoped lookup for an actor that owns no such set is not_found.
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.get",
        encode(&AdminFolderGetRequest {
            name: "photos".into(),
            actor_id: Some(ByteBuf::from(vec![0xCCu8; 32])),
            ..Default::default()
        }),
    )
    .await
    .expect_err("scoped to a non-owner → not_found");
    assert_eq!(err.code, "fauna.admin.not_found");

    // (c) a unique name keeps resolving bare, unchanged legacy behavior.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.create",
        encode(&AdminFolderCreateRequest {
            name: "solo".into(),
            actor_id: ByteBuf::from(alice.to_vec()),
            ..Default::default()
        }),
    )
    .await
    .expect("create ok");
    let got: AdminFolderGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.folders.get",
            encode(&AdminFolderGetRequest {
                name: "solo".into(),
                actor_id: None,
                ..Default::default()
            }),
        )
        .await
        .expect("bare unique name still resolves"),
    )
    .unwrap();
    assert_eq!(got.actor_id.as_ref(), &alice[..]);
}

// ── C5: services ─────────────────────────────────────────────────────

#[tokio::test]
async fn services_list_returns_defaults() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("services.json");
    fauna_nest::services::ensure_services_json(&path);
    let (router, state) = router_and_state_with_services_path(path).await;
    let admin = admin_actor(&state).await;
    let reply: AdminServicesListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.services.list",
            encode(&AdminServicesListRequest::default()),
        )
        .await
        .expect("services.list ok"),
    )
    .unwrap();
    assert_eq!(reply.version, 1);
    assert!(!reply.services.bridge);
    // The admin pairing knob defaults ON (per-user-pairing design).
    assert!(reply.services.pairing);
}

#[tokio::test]
async fn services_update_can_disable_pairing_knob() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("services.json");
    fauna_nest::services::ensure_services_json(&path);
    let (router, state) = router_and_state_with_services_path(path).await;
    let admin = admin_actor(&state).await;
    let reply: AdminServiceUpdateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.services.update",
            encode(&AdminServiceUpdateRequest {
                name: "pairing".into(),
                enabled: false,
                extra: Default::default(),
            }),
        )
        .await
        .expect("services.update ok"),
    )
    .unwrap();
    assert!(reply.ok);
    assert_eq!(reply.service, "pairing");
    assert!(!reply.enabled);

    let listed: AdminServicesListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.services.list",
            encode(&AdminServicesListRequest::default()),
        )
        .await
        .expect("services.list ok"),
    )
    .unwrap();
    assert!(!listed.services.pairing);
}

#[tokio::test]
async fn services_update_flips_flag_and_persists() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("services.json");
    fauna_nest::services::ensure_services_json(&path);
    let (router, state) = router_and_state_with_services_path(path).await;
    let admin = admin_actor(&state).await;
    let reply: AdminServiceUpdateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.services.update",
            encode(&AdminServiceUpdateRequest {
                name: "bridge".into(),
                enabled: true,
                extra: Default::default(),
            }),
        )
        .await
        .expect("services.update ok"),
    )
    .unwrap();
    assert!(reply.ok);
    assert_eq!(reply.service, "bridge");
    assert!(reply.enabled);

    // The flag persists — a follow-up list reflects it.
    let listed: AdminServicesListReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.services.list",
            encode(&AdminServicesListRequest::default()),
        )
        .await
        .expect("services.list ok"),
    )
    .unwrap();
    assert!(listed.services.bridge);
}

#[tokio::test]
async fn services_update_unknown_service_is_invalid_params() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("services.json");
    fauna_nest::services::ensure_services_json(&path);
    let (router, state) = router_and_state_with_services_path(path).await;
    let admin = admin_actor(&state).await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.services.update",
        encode(&AdminServiceUpdateRequest {
            name: "bogus".into(),
            enabled: true,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown service → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

/// The retired `algorithm` flag (it gated the sidecar removed 2026-10-01):
/// a `services.json` still carrying the key loads, `services.list` no longer
/// replies with it, and `services.update` refuses the name like any unknown
/// one (core-client-kind-catalog.md § Algorithm & Reputation).
#[tokio::test]
async fn services_retired_algorithm_flag_is_gone_from_the_wire() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("services.json");
    std::fs::write(
        &path,
        r#"{"version":1,"services":{"bridge":false,"algorithm":true,"pairing":true}}"#,
    )
    .unwrap();
    let (router, state) = router_and_state_with_services_path(path).await;
    let admin = admin_actor(&state).await;
    let listed: AdminServicesListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.services.list",
            encode(&AdminServicesListRequest::default()),
        )
        .await
        .expect("a services.json carrying the retired key still loads"),
    )
    .unwrap();
    assert!(listed.services.pairing);
    assert!(
        !listed.services.extra.contains_key("algorithm"),
        "services.list must not reply with the retired algorithm flag"
    );

    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.services.update",
        encode(&AdminServiceUpdateRequest {
            name: "algorithm".into(),
            enabled: true,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("the retired algorithm name → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

// ── C6 — observability / logs ────────────────────────────────────────────────

/// Serializes the `clear() → drive → read` window of the two tests below —
/// same shared-singleton race, same fix, as `conformance_log_redaction.rs`'s
/// `LOG_RING`: both drive the **process-global** `fauna_log` ring, so a
/// sibling's `clear()` can land between this test's driven call and its read
/// and wipe the very entry it is about to assert on.
static LOG_RING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `fauna.admin.logs` returns the nest's in-memory `fauna-log` ring snapshot
/// (`observability.md` § Surfaces). The handler reads the process-global ring
/// `main()` fills via `fauna_log::RingLayer`; the test drives that ring directly
/// with a scoped subscriber, then asserts the seeded lines (and their level)
/// come back over the wire. (Admin-gate + replay metadata are covered by the
/// `ADMIN_KINDS` loops above.)
#[tokio::test]
async fn logs_returns_seeded_ring_snapshot() {
    use tracing_subscriber::prelude::*;

    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;

    // Seed two known lines into the process-global ring. `with_default` scopes
    // the RingLayer to this block so only these lines are guaranteed present;
    // the handler then reads the same global ring (`fauna_log::snapshot`).
    let _ring = LOG_RING.lock().await;
    fauna_log::clear();
    let sub = tracing_subscriber::registry().with(fauna_log::RingLayer);
    tracing::subscriber::with_default(sub, || {
        tracing::info!(target: "fauna_nest::test", "conformance log alpha");
        tracing::error!(target: "fauna_nest::test", "conformance log bravo");
    });

    let reply: AdminLogsReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.logs",
            encode(&AdminLogsRequest::default()),
        )
        .await
        .expect("logs ok"),
    )
    .unwrap();

    let messages: Vec<&str> = reply.entries.iter().map(|e| e.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("conformance log alpha")),
        "alpha line in ring, got {messages:?}"
    );
    let bravo = reply
        .entries
        .iter()
        .find(|e| e.message.contains("conformance log bravo"))
        .expect("bravo line in ring");
    // The error line carries Error — the level the client's filter keys on.
    assert_eq!(bravo.level, AdminLogLevel::Error);
    assert_eq!(bravo.target, "fauna_nest::test");
    assert!(bravo.timestamp_ms > 0);
}

/// `fauna.admin.logs` serves the **merged** view — the nest's own ring PLUS the
/// sidecar log plane's remote ring, timestamp-ordered
/// (`observability.md` § The sidecar log plane → *The remote ring*). This is the
/// leg that makes plane entries render on `admin-logs` with **zero client
/// change**: they arrive in the existing reply shape, carrying their
/// `<source>:<event>` attribution in the existing `target` column.
#[tokio::test]
async fn logs_merges_the_sidecar_log_plane_into_the_admin_view() {
    use fauna_nest::log_plane::{LogSource, admit_at};
    use fauna_protocol::log_plane::{ReportLogEventsRequest, SidecarLogEvent};
    use tracing_subscriber::prelude::*;

    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;

    let _ring = LOG_RING.lock().await;
    fauna_log::clear();
    let sub = tracing_subscriber::registry().with(fauna_log::RingLayer);
    tracing::subscriber::with_default(sub, || {
        tracing::info!(target: "fauna_nest::test", "merge nest line");
    });
    // Drive a real admission, not a raw ring push: this asserts the whole
    // nest-side path (attribution + sanitization + ring) the wire legs call.
    admit_at(
        LogSource::Mta,
        &ReportLogEventsRequest {
            events: vec![SidecarLogEvent {
                timestamp_ms: 2_000,
                level: "error".into(),
                event: "tls.handshake_failed".into(),
                message: "merge bridge line".into(),
                ..Default::default()
            }],
            dropped: 0,
            ..Default::default()
        },
        9_000_000_000_000,
    );

    let reply: AdminLogsReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.admin.logs",
            encode(&AdminLogsRequest::default()),
        )
        .await
        .expect("logs ok"),
    )
    .unwrap();

    let bridge = reply
        .entries
        .iter()
        .find(|e| e.message.contains("merge bridge line"))
        .expect("the plane entry reaches the admin reply");
    assert_eq!(
        bridge.target, "mta:tls.handshake_failed",
        "attribution rides the existing target column — no new field, no client change"
    );
    assert_eq!(bridge.level, AdminLogLevel::Error);
    assert_eq!(
        bridge.timestamp_ms, 2_000,
        "the source's own timestamp is kept"
    );
    assert!(
        reply
            .entries
            .iter()
            .any(|e| e.message.contains("merge nest line")),
        "the nest's own entries are still served"
    );
    // Merged means ordered, not appended.
    let ts: Vec<i64> = reply.entries.iter().map(|e| e.timestamp_ms).collect();
    assert!(
        ts.windows(2).all(|w| w[0] <= w[1]),
        "the merged reply is timestamp-ordered, got {ts:?}"
    );
}

// C4 pairings RETIRED (per-user-pairing design) — the admin approve/list kinds
// are gone; the user-side `fauna.pair.{add,revoke,list}` round-trips and the
// admin `pairing` knob gate live in `conformance_pair.rs`.

/// S5: an admin addresses a set by `name_hash`, with the **same** 0/1/many
/// disposition the plaintext-name arm has.
///
/// This is the reference that survives the flip: an admin is not a set's key
/// audience, so once `folders.name` is scrubbed they cannot send a name they
/// can read — but the hash a listing handed them still addresses the row
/// exactly. The two arms must not drift apart on ambiguity, which is why they
/// share one resolver and this test mirrors
/// [`folder_admin_kinds_disambiguate_same_named_sets_across_actors`].
#[tokio::test]
async fn folder_admin_get_addresses_by_name_hash_with_the_same_ambiguity_contract() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let alice = [0xAAu8; 32];
    let bob = [0xBBu8; 32];
    let hash = ByteBuf::from(fauna_core::path_crypto::set_name_hash("photos").to_vec());

    for owner in [&alice, &bob] {
        dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.folders.create",
            encode(&AdminFolderCreateRequest {
                name: "photos".into(),
                actor_id: ByteBuf::from(owner.to_vec()),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok");
    }

    // (a) `(name_hash, actor_id)` resolves exactly, with NO plaintext name sent —
    // the post-flip call shape. The empty `name` proves the hash did the work.
    for owner in [&alice, &bob] {
        let got: AdminFolderGetReply = decode(
            &dispatch(
                &router,
                state.clone(),
                admin,
                "fauna.admin.folders.get",
                encode(&AdminFolderGetRequest {
                    name: String::new(),
                    actor_id: Some(ByteBuf::from(owner.to_vec())),
                    name_hash: Some(hash.clone()),
                    ..Default::default()
                }),
            )
            .await
            .expect("hash-scoped get ok"),
        )
        .unwrap();
        assert_eq!(got.actor_id.as_ref(), &owner[..], "got the OWNER's row");
        assert_eq!(got.name, "photos");
    }

    // (b) a bare ambiguous hash errors exactly as a bare ambiguous name does —
    // never "whichever row sorts first".
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.get",
        encode(&AdminFolderGetRequest {
            name: String::new(),
            actor_id: None,
            name_hash: Some(hash.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("ambiguous bare hash → invalid_params");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    // (c) the hash WINS over a plaintext name when both ride: a post-flip client
    // may carry a stale or unrenderable name, and the hash is the authority.
    let got: AdminFolderGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.folders.get",
            encode(&AdminFolderGetRequest {
                name: "a-name-that-matches-nothing".into(),
                actor_id: Some(ByteBuf::from(alice.to_vec())),
                name_hash: Some(hash.clone()),
                ..Default::default()
            }),
        )
        .await
        .expect("hash takes precedence over the name"),
    )
    .unwrap();
    assert_eq!(got.name, "photos");

    // (d) a malformed hash is refused honestly, not silently ignored (which would
    // fall back to the name arm and answer the wrong question).
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.folders.get",
        encode(&AdminFolderGetRequest {
            name: "photos".into(),
            actor_id: Some(ByteBuf::from(alice.to_vec())),
            name_hash: Some(ByteBuf::from(vec![1u8, 2, 3])),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a 3-byte hash is not a BLAKE3 digest");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

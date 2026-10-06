//! tier_3 coverage for the client-set `[nest]`-policy toggles
//! (`fauna.admin.set_registration_mode` / `fauna.admin.set_subhandles`) — the
//! full registered flow: an Admin-class dispatch upserts the DB singleton, swaps
//! the live `AppState` RwLock, and is read back on `fauna.setup.status` (and, for
//! subhandles, `fauna.nest.info`); a non-admin is denied; the boot reconcile
//! (`resolve_*`: DB row wins / config seed fallback) holds. Mirrors the
//! `set_mail_enabled` admin-toggle test but goes through the router so kind
//! registration + the `bridge_method_allowlist` Admin gate are exercised
//! end-to-end. Per the product invariant that config is client-set, not
//! CLI/config, + `public-mode.md` (`subhandles` is a client-set nest-config
//! flag).

mod common;

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::db::CacheDb;
use fauna_nest::node_policy_core::{
    resolve_cors_origins, resolve_max_storage_bytes, resolve_subhandles,
};
use fauna_nest::node_policy_handlers::register_node_policy_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::node_policy::{
    RegistrationMode, SetCorsOriginsReply, SetCorsOriginsRequest, SetMaxStorageBytesReply,
    SetMaxStorageBytesRequest, SetRegistrationModeReply, SetRegistrationModeRequest,
    SetSubhandlesReply, SetSubhandlesRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_node_policy_handlers(&mut b);
    b.build()
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

fn bool_payload<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

/// A `for_test` AppState with a registered admin actor.
async fn state_with_admin(admin: [u8; 32]) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    state.db.add_admin_actor(&admin).await.unwrap();
    state
}

/// `fauna.admin.set_require_registration` is **gone**: retired 2026-07-12 (its
/// posture folded into `fauna.admin.set_registration_mode`) and off the wire
/// since the 2026-09-24 compat-remnant sweep — it never had a client caller, so
/// a peer sending it gets the router's `unknown_kind`, not a typed "retired"
/// reply (`version-compatibility.md` § Dim 2, the fourth ratified exception).
#[tokio::test]
async fn admin_set_require_registration_is_gone() {
    let r = router();
    assert!(
        r.kind_meta("fauna.admin.set_require_registration")
            .is_none(),
        "the retired kind must not dispatch — it left the wire with the sweep"
    );
}

#[tokio::test]
async fn admin_set_registration_mode_persists_swaps_and_surfaces() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    for (mode, cap) in [
        (RegistrationMode::Open, Some(50u64)),
        (RegistrationMode::InviteRequired, None),
        (RegistrationMode::Closed, None),
    ] {
        let reply_bytes = dispatch(
            &r,
            state.clone(),
            "fauna.admin.set_registration_mode",
            admin,
            bool_payload(&SetRegistrationModeRequest {
                mode: mode.as_wire_str().to_string(),
                max_free_users: cap,
                extra: Default::default(),
            }),
        )
        .await
        .expect("admin set ok");
        let reply: SetRegistrationModeReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);

        // (1) DB singleton persisted, (2) live AppState swapped, (3) setup.status
        // surfaces it for the admin client to read back.
        assert_eq!(
            state.db.get_registration_mode().await.unwrap(),
            Some((mode, cap))
        );
        assert_eq!(*state.registration_mode.read().await, (mode, cap));
        let status = fauna_nest::discovery_core::setup_status_core(&state, true).await;
        assert_eq!(
            status.registration_mode.as_deref(),
            Some(mode.as_wire_str())
        );
        assert_eq!(status.max_free_users, cap);
    }
}

/// A non-admin cannot change the registration posture — the Admin-class gate in
/// `bridge_method_allowlist` bites before the handler body.
#[tokio::test]
async fn non_admin_cannot_set_registration_mode() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;
    let stranger = [9u8; 32];

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_registration_mode",
        stranger,
        bool_payload(&SetRegistrationModeRequest {
            mode: RegistrationMode::Open.as_wire_str().to_string(),
            max_free_users: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a non-admin must be denied");
    assert!(
        err.code.contains("permission_denied") || err.code.contains("forbidden"),
        "expected a permission denial, got {}",
        err.code
    );
    // The posture is untouched.
    assert_eq!(state.db.get_registration_mode().await.unwrap(), None);
}

/// An unparseable mode string is rejected at the door rather than persisted.
#[tokio::test]
async fn invalid_registration_mode_string_is_rejected() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_registration_mode",
        admin,
        bool_payload(&SetRegistrationModeRequest {
            mode: "wide_open_please".to_string(),
            max_free_users: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("an unknown mode must be refused");
    assert_eq!(err.code, "fauna.node_policy.registration_mode_invalid");
    assert_eq!(state.db.get_registration_mode().await.unwrap(), None);
}

#[tokio::test]
async fn admin_set_subhandles_persists_swaps_and_surfaces() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    assert!(!*state.subhandles.read().await);

    for enabled in [true, false] {
        let reply_bytes = dispatch(
            &r,
            state.clone(),
            "fauna.admin.set_subhandles",
            admin,
            bool_payload(&SetSubhandlesRequest {
                enabled,
                extra: Default::default(),
            }),
        )
        .await
        .expect("admin set ok");
        let reply: SetSubhandlesReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);

        assert_eq!(state.db.get_subhandles().await.unwrap(), Some(enabled));
        assert_eq!(*state.subhandles.read().await, enabled);
        // Surfaced on both setup.status and nest.info.
        let status = fauna_nest::discovery_core::setup_status_core(&state, true).await;
        assert_eq!(status.subhandles, enabled);
        let info = fauna_nest::discovery_core::nest_info_core(&state).await;
        assert_eq!(info.subhandles, enabled);
    }
}

#[tokio::test]
async fn admin_set_max_storage_bytes_persists_swaps_and_surfaces() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    // for_test default is None (no node-wide cap); setup.status reflects the
    // live value. The three-state cycle exercises set / clear-to-None / re-set.
    assert_eq!(*state.max_storage_bytes.read().await, None);

    for value in [Some(8_000_000_000u64), None, Some(1u64)] {
        let reply_bytes = dispatch(
            &r,
            state.clone(),
            "fauna.admin.set_max_storage_bytes",
            admin,
            bool_payload(&SetMaxStorageBytesRequest {
                max_bytes: value,
                extra: Default::default(),
            }),
        )
        .await
        .expect("admin set ok");
        let reply: SetMaxStorageBytesReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);

        // (1) DB singleton persisted — the row is present (outer Some), its inner
        // value is exactly what was set (None = an explicitly-cleared cap),
        // (2) the live AppState swapped, (3) setup.status surfaces it.
        assert_eq!(state.db.get_max_storage_bytes().await.unwrap(), Some(value));
        assert_eq!(*state.max_storage_bytes.read().await, value);
        let status = fauna_nest::discovery_core::setup_status_core(&state, true).await;
        assert_eq!(status.max_storage_bytes, value);
    }
}

#[tokio::test]
async fn admin_set_cors_origins_persists_swaps_and_surfaces() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;

    // for_test default is the empty list (⇒ the CORS layer falls back to the
    // default origin); setup.status reflects the live value. The cycle exercises
    // set-list / clear-to-empty / re-set — the list analog of the int three-state.
    assert!(state.cors_origins.load().is_empty());

    let cases: [Vec<String>; 3] = [
        vec![
            "https://app.example.com".to_string(),
            "https://admin.example.com".to_string(),
        ],
        vec![],
        vec!["https://only.example.com".to_string()],
    ];
    for origins in cases {
        let reply_bytes = dispatch(
            &r,
            state.clone(),
            "fauna.admin.set_cors_origins",
            admin,
            bool_payload(&SetCorsOriginsRequest {
                origins: origins.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("admin set ok");
        let reply: SetCorsOriginsReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);

        // (1) DB singleton persisted — the row is present (outer Some), its list is
        // exactly what was set (empty = an explicitly-cleared list, distinct from
        // unset), (2) the live AppState ArcSwap swapped, (3) setup.status surfaces it.
        assert_eq!(
            state.db.get_cors_origins().await.unwrap(),
            Some(origins.clone())
        );
        assert_eq!(state.cors_origins.load().as_slice(), origins.as_slice());
        let status = fauna_nest::discovery_core::setup_status_core(&state, true).await;
        assert_eq!(status.cors_origins, origins);
    }
}

#[tokio::test]
async fn non_admin_denied_for_every_knob() {
    let r = router();
    let admin = [7u8; 32];
    let state = state_with_admin(admin).await;
    let stranger = [99u8; 32];

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_subhandles",
        stranger,
        bool_payload(&SetSubhandlesRequest {
            enabled: true,
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.admin.permission_denied");

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_max_storage_bytes",
        stranger,
        bool_payload(&SetMaxStorageBytesRequest {
            max_bytes: Some(1),
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.admin.permission_denied");

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.admin.set_cors_origins",
        stranger,
        bool_payload(&SetCorsOriginsRequest {
            origins: vec!["https://evil.example.com".to_string()],
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.admin.permission_denied");

    // No row written on a denied set.
    assert_eq!(state.db.get_subhandles().await.unwrap(), None);
    assert_eq!(state.db.get_max_storage_bytes().await.unwrap(), None);
    assert_eq!(state.db.get_cors_origins().await.unwrap(), None);
}

#[tokio::test]
async fn boot_reconcile_db_row_wins_else_seed() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    // No row ⇒ the boot seed wins, in either direction.
    assert!(!resolve_subhandles(&db, false).await);
    assert!(resolve_subhandles(&db, true).await);
    // max_storage_bytes: no row ⇒ the seed (either a cap or None) passes through.
    assert_eq!(resolve_max_storage_bytes(&db, Some(42)).await, Some(42));
    assert_eq!(resolve_max_storage_bytes(&db, None).await, None);
    // cors_origins: no row ⇒ the seed list passes through.
    let seed = vec!["https://seed.example.com".to_string()];
    assert_eq!(resolve_cors_origins(&db, seed.clone()).await, seed);
    assert!(resolve_cors_origins(&db, vec![]).await.is_empty());

    // A client-set row wins over the seed.
    db.set_subhandles(true).await.unwrap();
    assert!(resolve_subhandles(&db, false).await);
    // An explicitly-cleared cap (client-set None) wins over a non-None seed.
    db.set_max_storage_bytes(None).await.unwrap();
    assert_eq!(resolve_max_storage_bytes(&db, Some(42)).await, None);
    db.set_max_storage_bytes(Some(7)).await.unwrap();
    assert_eq!(resolve_max_storage_bytes(&db, None).await, Some(7));
    // An explicitly-cleared list (client-set empty) wins over a non-empty seed.
    db.set_cors_origins(vec![]).await.unwrap();
    assert!(resolve_cors_origins(&db, seed.clone()).await.is_empty());
    let set = vec!["https://set.example.com".to_string()];
    db.set_cors_origins(set.clone()).await.unwrap();
    assert_eq!(resolve_cors_origins(&db, vec![]).await, set);
}

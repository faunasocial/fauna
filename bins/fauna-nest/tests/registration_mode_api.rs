//! tier_3: the registration posture is **client-set and load-bearing**, end to end.
//!
//! `node_policy_api.rs` pins the *kind* (Admin gate, non-admin denial, persist +
//! live swap, read-back). This file pins the thing that kind exists for: that an
//! admin flipping the mode from their client actually changes **who can register**.
//!
//! That link is the whole point of the migration. The posture used to be a pair of
//! CLI flags with no client-set path — the banned *configuration-file theatre*
//! (`principles.md` § One configuration surface). Re-adding the kind while leaving
//! `account_core`'s gates reading something else would reproduce the exact
//! "implemented but unreachable from any client" gap that caused the violation, and
//! every symbol-existence check would still pass. So each test here drives the real
//! production flow:
//!
//!   admin dispatches `fauna.admin.set_registration_mode`
//!     → `apply_registration_mode_change` upserts `nest_registration_mode` + swaps
//!       the live `AppState.registration_mode`
//!     → `account_core::register_core`'s gate reads that value
//!     → `fauna.account.register` is admitted or refused accordingly.
//!
//! Owner: `docs/goal/architecture/nest/public-mode.md` § Registration Modes.

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_nest::account_handlers::register_account_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::node_policy_handlers::register_node_policy_handlers;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use fauna_protocol::RpcError;
use fauna_protocol::node_policy::{RegistrationMode, SetRegistrationModeRequest};

const DOMAIN: &str = "test.fauna.social";

/// Both kind families on one router: the admin sets the posture, a stranger then
/// tries to register against it — the two halves of the flow under test.
fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_node_policy_handlers(&mut b);
    register_account_handlers(&mut b);
    b.build()
}

/// A nest whose posture starts `Closed` (the default a fresh box boots into) and
/// which can bind handles on `DOMAIN`.
async fn nest_with_admin(admin: [u8; 32]) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                reserved_handles: vec![],
            },
            ..Default::default()
        },
        registration_mode: Arc::new(tokio::sync::RwLock::new((RegistrationMode::Closed, None))),
        ..AppState::for_test(db)
    });
    state.db.add_admin_actor(&admin).await.unwrap();
    state
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

/// Drive the admin's client call: `fauna.admin.set_registration_mode`.
async fn set_mode(
    r: &RpcRouter,
    state: Arc<AppState>,
    admin: [u8; 32],
    mode: RegistrationMode,
    max_free_users: Option<u64>,
) {
    dispatch(
        r,
        state,
        "fauna.admin.set_registration_mode",
        admin,
        encode(&SetRegistrationModeRequest {
            mode: mode.as_wire_str().to_string(),
            max_free_users,
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("admin must be able to set {}: {e:?}", mode.as_wire_str()));
}

/// **The load-bearing test.** An admin opens registration from their client, and a
/// stranger who was refused a moment ago can now register — no restart, no flag.
#[tokio::test]
async fn admin_opening_registration_lets_a_stranger_register() {
    let admin = [7u8; 32];
    let state = nest_with_admin(admin).await;
    let r = router();

    // Closed (the fresh-box default): the ceremony is refused.
    let alice = ActorKeypair::generate();
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&alice, "alice", DOMAIN, None, None),
    )
    .await
    .expect_err("a closed nest must refuse registration");
    assert_eq!(err.code, "fauna.account.registration_closed");

    // The admin flips it to Open from their client.
    set_mode(&r, state.clone(), admin, RegistrationMode::Open, None).await;

    // The SAME actor now registers — the gate is reading the singleton the admin
    // just wrote, live. (A fresh keypair would pass even if the gate were stuck,
    // so reuse alice: only the posture changed between these two calls.)
    dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&alice, "alice", DOMAIN, None, None),
    )
    .await
    .expect("an open nest must admit the ceremony");
    assert!(
        state
            .db
            .is_actor_registered(&alice.actor_id().0)
            .await
            .unwrap(),
        "the account is real, not just a 200"
    );
}

/// The reverse, and the one that matters for a nest under abuse: an admin can
/// **close** an open nest and the next stranger is refused immediately.
#[tokio::test]
async fn admin_closing_registration_refuses_the_next_stranger() {
    let admin = [7u8; 32];
    let state = nest_with_admin(admin).await;
    let r = router();

    set_mode(&r, state.clone(), admin, RegistrationMode::Open, None).await;
    let alice = ActorKeypair::generate();
    dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&alice, "alice", DOMAIN, None, None),
    )
    .await
    .expect("open: admitted");

    set_mode(&r, state.clone(), admin, RegistrationMode::Closed, None).await;

    let mallory = ActorKeypair::generate();
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&mallory, "mallory", DOMAIN, None, None),
    )
    .await
    .expect_err("closing the nest must refuse the next registration");
    assert_eq!(err.code, "fauna.account.registration_closed");
    assert!(
        !state
            .db
            .is_actor_registered(&mallory.actor_id().0)
            .await
            .unwrap(),
        "no account may be created on a closed nest"
    );
}

/// `InviteRequired` demands a code — and a valid code satisfies it. Pins that the
/// invite gate reads the admin-set mode, not a stale boolean.
#[tokio::test]
async fn invite_required_mode_demands_a_code() {
    let admin = [7u8; 32];
    let state = nest_with_admin(admin).await;
    let r = router();
    state
        .db
        .create_invite_code("WELCOME2026", "personal", 5)
        .await
        .unwrap();

    set_mode(
        &r,
        state.clone(),
        admin,
        RegistrationMode::InviteRequired,
        None,
    )
    .await;

    // No code → refused.
    let alice = ActorKeypair::generate();
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&alice, "alice", DOMAIN, None, None),
    )
    .await
    .expect_err("invite_required must refuse a code-less registration");
    assert_eq!(err.code, "fauna.account.invite_required");

    // With the code → admitted, at the code's tier.
    dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&alice, "alice", DOMAIN, Some("WELCOME2026"), None),
    )
    .await
    .expect("a valid invite code satisfies invite_required");
    let user = state
        .db
        .get_user(&alice.actor_id().0)
        .await
        .unwrap()
        .expect("registered");
    assert_eq!(user.tier, "personal", "the code's tier is granted");
}

/// The free-tier ceiling is **orthogonal to the mode** (`public-mode.md`
/// § Registration Modes: "regardless of mode"). Pins that it is carried on the same
/// admin write and enforced independently — an `Open` nest at its cap refuses.
#[tokio::test]
async fn the_free_tier_ceiling_is_orthogonal_to_the_mode() {
    let admin = [7u8; 32];
    let state = nest_with_admin(admin).await;
    let r = router();

    // The cap counts EVERY free-tier account on the nest, and the admin is one of
    // them (an admin is a user with an extra role — it holds a `users` row at the
    // seeded `free` tier). So "room for exactly one more" is a cap of 2, not 1.
    // This is the real semantic: an admin sizing their nest is capping total free
    // accounts, not "free accounts other than me".
    set_mode(&r, state.clone(), admin, RegistrationMode::Open, Some(2)).await;

    let alice = ActorKeypair::generate();
    dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&alice, "alice", DOMAIN, None, None),
    )
    .await
    .expect("the first free account fits under the cap");

    // The cap now bites — the mode is still Open, so this refusal can only come
    // from the ceiling.
    let bob = ActorKeypair::generate();
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&bob, "bob", DOMAIN, None, None),
    )
    .await
    .expect_err("the free-tier ceiling must bite on an open nest");
    assert_eq!(err.code, "fauna.account.free_limit_reached");

    // Raising the ceiling (same kind, same write) admits him.
    set_mode(&r, state.clone(), admin, RegistrationMode::Open, Some(3)).await;
    dispatch(
        &r,
        state.clone(),
        "fauna.account.register",
        [0u8; 32],
        common::register_payload(&bob, "bob", DOMAIN, None, None),
    )
    .await
    .expect("raising the ceiling admits the next free account");
}

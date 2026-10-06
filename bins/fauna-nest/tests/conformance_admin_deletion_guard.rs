//! In-process WS-RPC conformance for the **admin-deletion** guard
//! (`docs/goal/architecture/nest/common.md` § Client-state recoverability;
//! `docs/goal/behavior/admin.md` § 2 Users → *Cutting a user off*;
//! `docs/goal/architecture/api-layers.md` § `Admin ⊇ User`).
//!
//! Suspension and eviction already refuse an admin target (`require_not_admin`),
//! because a suspended sole admin is an off-box brick nobody can restore.
//! **Deletion had no such guard**, and the brick it opens is strictly worse,
//! because deletion is irreversible:
//!
//! 1. The sole admin calls `fauna.account.delete` on themselves (allowed today —
//!    the allowlist grants `User | Admin`). Fourteen days later the pending-action
//!    executor runs `finalize_user_deletion`, which drops the `users` row but
//!    **never touches `admin_actor_ids`**.
//! 2. `admin_count()` still reads 1, and the claim gate keys on exactly that
//!    (`claim_core.rs`, `admin_count == 0`), so the box reports **claimed** and
//!    refuses a fresh claim with `already_claimed`.
//! 3. The ex-admin's bearer expires within `TOKEN_TTL_SECS` (1 h). On a
//!    **private** nest, re-auth runs `check_actor_active`, which fails with
//!    "user not registered" — no `users` row. No new token, ever.
//!
//! Net: a claimed nest with zero authenticable admins that no client can
//! re-claim — recoverable only by manual DB surgery. That is precisely the state
//! `common.md` § Client-state recoverability declares a **bug**, not a deferred
//! feature.
//!
//! Two properties are pinned here, mirroring the technique `common.md` prefers
//! ("make the bad state unrepresentable" over detect-and-repair):
//!
//! - **Guard at the door** — neither `fauna.account.delete` nor
//!   `fauna.admin.users.delete` accepts an admin target. Demote first via
//!   `fauna.admin.admins.remove`; a *sole* superadmin cannot demote
//!   (the superadmin floor, `can_remove_admin` + the writer's refusal), so
//!   their exit is `fauna.admin.factory_reset` —
//!   the universal recovery floor, which returns the box to fresh/unclaimed.
//! - **Guard at the executor** — `finalize_user_deletion` refuses an actor that
//!   still holds the admin role, so an `admin_actor_ids` entry can never outlive
//!   its `users` row. This is not belt-and-braces: pending actions are
//!   **persisted**, so a nest upgraded across this change can still hold an
//!   `AccountDelete` row queued for an admin *before* the door guard existed. The
//!   handler guard cannot retroactively protect it; only the executor can.
//!
//! Tier: tier_3 (real `AppState` + real in-memory `CacheDb` — no mocks).

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{account_handlers, admin_ws_handlers, pending_actions};
use fauna_protocol::account::AccountDeleteRequest;
use fauna_protocol::admin::AdminUserDeleteRequest;
use fauna_protocol::{ByteBuf, RpcError};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    account_handlers::register_account_user_handlers(&mut b);
    b.build()
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

async fn plain_user(db: &CacheDb, handle: &str) -> [u8; 32] {
    let kp = ActorKeypair::generate();
    let id = kp.actor_id().0;
    db.create_user_with_handle(&id, "personal", handle, None)
        .await
        .unwrap();
    id
}

/// An admin is a user with an extra role (`api-layers.md` § `Admin ⊇ User`), and
/// the real claim path (`claim_core.rs`) creates the `users` row *before*
/// `add_admin_actor`. Mirror that ordering, or a delete-guard test would pass
/// for the wrong reason ("user not found" rather than "is an admin").
async fn admin_actor(state: &AppState, handle: &str) -> [u8; 32] {
    let admin = plain_user(&state.db, handle).await;
    state.db.add_admin_actor(&admin[..]).await.unwrap();
    admin
}

fn account_delete_payload() -> Bytes {
    encode(&AccountDeleteRequest {
        extra: Default::default(),
    })
}

fn admin_delete_payload(target: [u8; 32]) -> Bytes {
    encode(&AdminUserDeleteRequest {
        actor_id: ByteBuf::from(target.to_vec()),
        extra: Default::default(),
    })
}

// ── 1. Guard at the door ─────────────────────────────────────────────────────

/// The sharp case: the **sole** admin self-deletes. They cannot demote first
/// (the superadmin floor refuses the last superadmin), so accepting this
/// schedules the brick. Refuse it; their exit is `fauna.admin.factory_reset`.
#[tokio::test]
async fn sole_admin_cannot_self_delete() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let router = router();
    let admin = admin_actor(&state, "admin").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        admin,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect_err("an admin self-deleting must be refused — it schedules an off-box brick");

    assert_eq!(err.code, "fauna.account.admin_role_held", "got: {err:?}");
    assert!(
        state.db.is_admin(&admin[..]).await.unwrap(),
        "the refused delete must not have disturbed the admin role"
    );
}

/// A co-admin is refused too. Deletion is irreversible, so unlike suspension
/// there is no "restore" even when another admin survives; the role must come
/// off first, which keeps `Admin ⊇ User` true by construction.
#[tokio::test]
async fn admin_cannot_be_deleted_by_another_admin() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let router = router();
    let admin_a = admin_actor(&state, "admin-a").await;
    let admin_b = admin_actor(&state, "admin-b").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        admin_a,
        "fauna.admin.users.delete",
        admin_delete_payload(admin_b),
    )
    .await
    .expect_err("fauna.admin.users.delete must refuse an admin target, as evict/suspend do");

    assert_eq!(err.code, "fauna.admin.conflict", "got: {err:?}");
    assert!(state.db.is_admin(&admin_b[..]).await.unwrap());
}

/// The guard is about the *role*, not about being special: a plain user is still
/// deletable through both doors. Without this, a guard that refused everything
/// would pass the two tests above.
#[tokio::test]
async fn plain_user_deletion_is_still_accepted_through_both_doors() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let router = router();
    let admin = admin_actor(&state, "admin").await;
    let alice = plain_user(&state.db, "alice").await;
    let bob = plain_user(&state.db, "bob").await;

    dispatch(
        &router,
        Arc::clone(&state),
        alice,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect("a plain user may still schedule their own deletion");

    dispatch(
        &router,
        Arc::clone(&state),
        admin,
        "fauna.admin.users.delete",
        admin_delete_payload(bob),
    )
    .await
    .expect("an admin may still schedule a plain user's deletion");
}

/// The gate spans the account's local succession chain (`admin.md` § Admin
/// continuity and succession). A deletion takes every local predecessor with
/// it — their `admin_actor_ids` rows purged, their `users` rows deleted — so a
/// retired identity's admin row would leave without `admin.remove`'s quorum or
/// the superadmin floor. Both deletion doors refuse while one is held.
#[tokio::test]
async fn deletion_is_refused_while_a_retired_identity_holds_the_admin_role() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let router = router();
    let admin = admin_actor(&state, "admin").await;
    let retired = plain_user(&state.db, "carol").await;
    let successor = ActorKeypair::generate().actor_id().0;
    state
        .db
        .record_succession(&retired, &successor, b"s", 1)
        .await
        .unwrap()
        .unwrap();
    // A row granted to the retired key before the add door refused one.
    state.db.add_admin_actor(&retired[..]).await.unwrap();

    let err = dispatch(
        &router,
        Arc::clone(&state),
        successor,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect_err("self-deletion must be refused while a predecessor holds the role");
    assert_eq!(err.code, "fauna.account.admin_role_held", "got: {err:?}");

    let err = dispatch(
        &router,
        Arc::clone(&state),
        admin,
        "fauna.admin.users.delete",
        admin_delete_payload(successor),
    )
    .await
    .expect_err("an admin deletion must be refused while a predecessor holds the role");
    assert_eq!(err.code, "fauna.admin.conflict", "got: {err:?}");
}

// ── 2. Guard at the executor ─────────────────────────────────────────────────

/// Pending actions are persisted and wait out a delay, so an `AccountDelete`
/// queued by a plain user reaches the executor as an admin's if the actor was
/// promoted in between. The handler guard cannot see that later promotion; the
/// executor must refuse.
///
/// It **refuses** rather than stripping the admin role, mirroring the
/// guardianship fail-safe directly above it in `finalize_user_deletion`. Two
/// reasons. A failed action is never marked executed
/// (`execute_ready_actions`), so it simply retries each tick and self-heals the
/// moment the actor demotes. And stripping the role instead would silently
/// return a **populated** nest to fresh/unclaimed, where any stranger who
/// reaches it may claim it and inherit the existing users' data — trading a
/// brick for a worse security posture.
#[tokio::test]
async fn finalizing_an_admins_deletion_is_refused_and_leaves_the_actor_intact() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    let admin = admin_actor(&state, "admin").await;

    pending_actions::finalize_user_deletion(&state, &admin)
        .await
        .expect_err("finalizing an admin's deletion must be refused, not orphan the admin row");

    assert!(
        db.get_user(&admin).await.unwrap().is_some(),
        "the refused finalize must leave the users row intact — otherwise the actor \
         is an admin who can no longer authenticate"
    );
    assert!(
        db.is_admin(&admin[..]).await.unwrap(),
        "the refused finalize must leave the admin role intact"
    );
}

/// The invariant the whole file exists for, stated as `common.md`'s § Client-state
/// recoverability verification question and deliberately **mechanism-independent**:
/// after any deletion path runs against the sole admin, a client must still be
/// able to recover the box. That holds iff *either* the admin can still
/// authenticate (its `users` row survives) *or* the box is re-claimable
/// (`admin_count == 0`, which is exactly what the claim gate in `claim_core.rs`
/// reads). The forbidden state is the conjunction of their negations: a box that
/// reports `claimed` while nobody alive can authenticate as its admin.
///
/// Asserting the disjunction rather than `admin_count == 0` means a future
/// session may change *how* the brick is prevented without rewriting this test —
/// only actually reintroducing the brick can fail it.
#[tokio::test]
async fn no_delete_path_leaves_a_claimed_box_without_an_authenticable_admin() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    let admin = admin_actor(&state, "admin").await;
    assert_eq!(db.admin_count().await.unwrap(), 1);

    // Whether this succeeds or is refused is the mechanism's business.
    let _ = pending_actions::finalize_user_deletion(&state, &admin).await;

    let re_claimable = db.admin_count().await.unwrap() == 0;
    let admin_can_authenticate = db.get_user(&admin).await.unwrap().is_some();
    assert!(
        re_claimable || admin_can_authenticate,
        "off-box brick: the box reports claimed (admin_count > 0, so claim_core answers \
         already_claimed forever) yet its only admin has no users row, so check_actor_active \
         refuses it a token once the current bearer expires — recoverable only by DB surgery, \
         which common.md § Client-state recoverability forbids"
    );
}

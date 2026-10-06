//! In-process WS-RPC conformance for the suspension lifecycle
//! (`docs/goal/behavior/admin.md` § 2 Users → Suspension;
//! tracked internally).
//!
//! Pins the two properties suspension must have, which showed it lacked:
//!
//! 1. **It bites at dispatch.** A suspended actor cannot dispatch any `User`-class
//!    kind — including over a connection it already held when it was suspended (the
//!    nest re-resolves the caller class per RPC, so there is no "token still valid"
//!    window). This used to need proving separately per registration mode: an
//!    open-registration nest auto-provisioned unknown actors and its handshake never
//!    consulted `users.suspended` at all. Auto-provision is gone, so there is one
//!    posture and one proof.
//! 2. **It is reversible from a client.** Suspension folds into the eviction
//!    state machine, so the existing `fauna.admin.users.cancel_eviction`
//!    restores the user with no client-side recovery step — the
//!    `../architecture/nest/common.md` § Client-state recoverability invariant.
//!
//! Plus the guard that keeps the invariant true: an **admin cannot be
//! suspended** (demote via `fauna.admin.admins.remove` first), so the
//! "suspended sole admin with nobody left to restore them" brick is
//! unrepresentable.
//!
//! Tier: tier_3 (real `AppState` + real in-memory `CacheDb` — no mocks).

mod common;
use common::{encode, register_payload};

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use fauna_nest::{
    account_handlers, admin_ws_handlers, contacts_handlers, discovery_handlers, invite_handlers,
};
use fauna_protocol::admin::{
    AdminInviteRequestApproveRequest, AdminUserCancelEvictionRequest, AdminUserSuspendRequest,
    AdminUsersListReply, AdminUsersListRequest,
};
use fauna_protocol::contacts::ContactListRequest;
use fauna_protocol::discovery::{HandleAvailableReply, HandleAvailableRequest};
use fauna_protocol::invite::{InviteRequestStatus, InviteRequestStatusQuery, InviteRequestSubmit};
use fauna_protocol::node_policy::RegistrationMode;
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode_reply};

/// A `User`-class kind that is a pure read and gates on `caller_class_for_actor`
/// (`contacts_handlers::require_permission`) — the cheapest probe of "can this
/// actor dispatch at all".
const USER_KIND: &str = "fauna.contacts.list";

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    contacts_handlers::register_contacts_handlers(&mut b);
    b.build()
}

/// There is now exactly **one** auth posture. The old `require_registration`
/// boolean split this file's fixtures in two — an "open-registration" nest that
/// auto-provisioned unknown actors (and whose handshake therefore never read
/// `users.suspended`) and a "private" nest that did. Auto-provision is deleted,
/// so both collapse to this: every actor has a `users` row, and `check_actor_active`
/// runs on every handshake.
fn nest(db: Arc<CacheDb>) -> Arc<AppState> {
    Arc::new(AppState::for_test(db))
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

fn contacts_list_payload() -> Bytes {
    encode(&ContactListRequest {
        extra: Default::default(),
    })
}

async fn plain_user(db: &CacheDb, handle: &str) -> [u8; 32] {
    let kp = ActorKeypair::generate();
    let id = kp.actor_id().0;
    db.create_user_with_handle(&id, "personal", handle, None)
        .await
        .unwrap();
    id
}

/// An admin is a user with an extra role, so give it a `users` row — otherwise
/// `start_eviction` / `suspend` refuse it with "user not found" and an
/// admin-target test would pass for the wrong reason.
async fn admin_actor(state: &AppState) -> [u8; 32] {
    let admin = plain_user(&state.db, "admin").await;
    state.db.add_admin_actor(&admin[..]).await.unwrap();
    admin
}

/// Drive `fauna.admin.users.suspend` as the admin would.
async fn suspend(
    router: &RpcRouter,
    state: Arc<AppState>,
    admin: [u8; 32],
    target: [u8; 32],
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        state,
        admin,
        "fauna.admin.users.suspend",
        encode(&AdminUserSuspendRequest {
            actor_id: ByteBuf::from(target.to_vec()),
            ..Default::default()
        }),
    )
    .await
}

async fn cancel_eviction(
    router: &RpcRouter,
    state: Arc<AppState>,
    admin: [u8; 32],
    target: [u8; 32],
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        state,
        admin,
        "fauna.admin.users.cancel_eviction",
        encode(&AdminUserCancelEvictionRequest {
            actor_id: ByteBuf::from(target.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
}

// ── 1. Suspension bites at dispatch ─────────────────────────────────────────

/// F4, the live half: nothing on the auth path read `users.suspended` on an
/// open-registration nest, so a suspended user kept full `User`-class dispatch
/// indefinitely; on a private nest `check_actor_active` refused a *new* handshake
/// but the already-held token kept dispatching until TTL. Both halves land here:
/// dispatching *after* the suspension models the live connection the user already
/// held, and the handler re-resolves the caller class from the DB on every RPC —
/// so this must fail regardless of any token, in what is now the single posture.
#[tokio::test]
async fn suspended_user_cannot_dispatch() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let user = plain_user(&db, "mallory").await;

    // Before suspension the user dispatches fine.
    dispatch(&r, st.clone(), user, USER_KIND, contacts_list_payload())
        .await
        .expect("an active user may list contacts");

    suspend(&r, st.clone(), admin, user)
        .await
        .expect("suspended");

    let err = dispatch(&r, st.clone(), user, USER_KIND, contacts_list_payload())
        .await
        .expect_err("a suspended user must not dispatch a User-class kind");
    // The CENTRAL code, not `fauna.contacts.…`. Suspension does not resolve the
    // caller to a *wrong* class, it strips the class entirely — a suspended actor
    // takes `caller_class_for_actor`'s own documented "None when the actor is
    // unknown or revoked" arm (`bridge_method_allowlist.rs` § Authority gate),
    // and `api-layers.md` § Caller-class authorization → *Refusal codes at the
    // gate* (ruled 2026-08-17) reserves `fauna.bridges.permission_denied` for
    // exactly that: an actor denied on *every* kind, which says nothing about
    // the contacts family in particular. The Go bridges' revocation probe keys
    // on that every-kind shape, so this arm "must never become per-family".
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

// ── 2. Suspension is immediate, and reversible from a client ────────────────

/// The fold: `users.suspend` writes the eviction machine's `suspended` state
/// directly (no 4-hour pending action), and leaves **no delete timeline** —
/// `eviction_delete_at` stays NULL so `transition_evictions` never deletes.
#[tokio::test]
async fn suspend_is_immediate_and_schedules_no_deletion() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let user = plain_user(&db, "mallory").await;

    suspend(&r, st.clone(), admin, user)
        .await
        .expect("suspended");

    let row = db.get_user(&user).await.unwrap().expect("user row");
    assert!(row.suspended, "suspension takes effect immediately");

    // No delete phase was armed: advancing the eviction machine deletes nobody.
    let (newly_suspended, deleted) = db.transition_evictions().await.unwrap();
    assert!(newly_suspended.is_empty());
    assert!(deleted.is_empty(), "a suspension must never auto-delete");
    assert!(
        db.get_user(&user).await.unwrap().is_some(),
        "the suspended user still exists"
    );
}

/// The restore button only renders if the suspended user shows up as an active
/// eviction — `list_evictions` filters `eviction_status != ''`, which the fold
/// satisfies. Pins the projection link the client UI depends on: the row is
/// listed, and its `delete_at` is null (suspended, no deletion scheduled).
#[tokio::test]
async fn a_suspended_user_is_listed_as_an_active_eviction_with_no_delete_deadline() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let user = plain_user(&db, "mallory").await;

    suspend(&r, st.clone(), admin, user)
        .await
        .expect("suspended");

    let rows = db.list_evictions().await.unwrap();
    let row = rows
        .iter()
        .find(|u| u.actor_id == user.to_vec())
        .expect("a suspended user is an active eviction, so the row offers restore");
    assert_eq!(row.eviction_status, "suspended");
    assert!(row.suspended);
    assert_eq!(
        row.eviction_delete_at, None,
        "suspension schedules no deletion"
    );
}

/// The projection the *client* row reads to decide whether to offer a cut-off
/// control at all. An admin cannot be suspended or evicted (`fauna.admin.conflict`
/// — `admin.md` § 2 → *Cutting a user off*), so `fauna.admin.users.list` marks the
/// admin's row `is_admin`, and `fauna_client_admin::admin_user_row_controls`
/// withholds both entry buttons rather than render one the nest always refuses.
///
/// Pins the whole link: `admin_actor_ids` → `admin_actor_set` batch read →
/// `user_to_wire_with_overrides` → the wire `AdminUser.is_admin`.
#[tokio::test]
async fn users_list_marks_the_admin_row_so_the_client_can_withhold_cut_off_controls() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let user = plain_user(&db, "mallory").await;

    let reply = dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.list",
        encode(&AdminUsersListRequest::default()),
    )
    .await
    .expect("admin lists users");
    let reply: AdminUsersListReply = decode_reply(&reply).expect("decodes");

    let admin_row = reply
        .users
        .iter()
        .find(|u| u.actor_id.as_ref() == admin.as_slice())
        .expect("the admin has a users row");
    let user_row = reply
        .users
        .iter()
        .find(|u| u.actor_id.as_ref() == user.as_slice())
        .expect("the plain user is listed");

    assert!(
        admin_row.is_admin,
        "the admin's row must be marked, else the client offers a Suspend button \
         the nest answers with fauna.admin.conflict"
    );
    assert!(!user_row.is_admin, "a plain user is not an admin");

    // And the flag says exactly what the nest enforces: suspending the admin is
    // refused, suspending the plain user is not.
    suspend(&r, st.clone(), admin, admin)
        .await
        .expect_err("an admin cannot be suspended");
    suspend(&r, st.clone(), admin, user)
        .await
        .expect("a plain user can be");
}

/// `common.md` § Client-state recoverability: the state an admin (a client) put
/// the nest into must be undoable by a client. The existing
/// `admin-users-cancel-eviction-button` is that path.
#[tokio::test]
async fn cancel_eviction_restores_a_suspended_user() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let user = plain_user(&db, "mallory").await;

    suspend(&r, st.clone(), admin, user)
        .await
        .expect("suspended");
    dispatch(&r, st.clone(), user, USER_KIND, contacts_list_payload())
        .await
        .expect_err("suspended");

    cancel_eviction(&r, st.clone(), admin, user)
        .await
        .expect("restore");

    assert!(!db.get_user(&user).await.unwrap().unwrap().suspended);
    dispatch(&r, st.clone(), user, USER_KIND, contacts_list_payload())
        .await
        .expect("a restored user dispatches again, with no client-side recovery step");
}

// ── 3. The guard that keeps recoverability true ─────────────────────────────

/// A suspended admin could not be restored by anyone if they were the only
/// admin — an off-box brick. Make it unrepresentable: demote first
/// (`fauna.admin.admins.remove`, superadmin-guarded), then suspend.
#[tokio::test]
async fn suspend_refuses_an_admin_target() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let other = plain_user(&db, "coadmin").await;
    db.add_admin_actor(&other[..]).await.unwrap();

    let err = suspend(&r, st.clone(), admin, other)
        .await
        .expect_err("an admin must not be suspendable");
    assert_eq!(err.code, "fauna.admin.conflict");

    // Self-suspension is the same brick.
    let self_err = suspend(&r, st.clone(), admin, admin)
        .await
        .expect_err("an admin must not suspend themselves");
    assert_eq!(self_err.code, "fauna.admin.conflict");
}

/// Eviction's suspend phase reaches the same state, so it carries the same
/// guard — otherwise evicting the sole admin bricks the nest 14 days later.
#[tokio::test]
async fn evict_refuses_an_admin_target() {
    use fauna_protocol::admin::AdminUserEvictRequest;

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = nest(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;

    let err = dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.evict",
        encode(&AdminUserEvictRequest {
            actor_id: ByteBuf::from(admin.to_vec()),
            reason: "test".into(),
            category: "other".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("an admin must not be evictable");
    assert_eq!(err.code, "fauna.admin.conflict");
}

// ── 4. The registration doors refuse a suspended actor as already registered ──
//
// `login.md` § Errors rules suspended-vs-unregistered opaque at the mint and
// names the two registration doors the accepted, stated exception: they refuse
// every `users` row — suspended included — with `fauna.account.actor_exists`,
// the same code an active account gets and never a suspended-specific one
// (opacity is not available at a door: a stranger's submit succeeds and a
// stranger's open-mode register mints). The pins below hold the ruling's three
// load-bearing facts: (1) both doors answer the one code, and no pending row
// is created for a suspended actor; (2) the adjacent `invite_request.status`
// read does NOT leak — an invite-admitted actor reads `not_found` suspended or
// not, because approval consumes the request row; (3) the disclosure is no
// wider than the public handle probe, which reports a suspended user's handle
// taken exactly like an active one. The mint half — verify answers the opaque
// code — is `conformance_auth.rs::verify_rejects_suspended_actor`.

/// The admin kinds plus the two doors, the status read and the public probe.
fn door_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    invite_handlers::register_invite_handlers(&mut b);
    account_handlers::register_account_handlers(&mut b);
    discovery_handlers::register_discovery_handlers(&mut b);
    b.build()
}

const DOOR_DOMAIN: &str = "test.fauna.social";

/// An open-registration nest on `DOOR_DOMAIN` — the posture where
/// `fauna.account.register` mints a stranger's account, so a refusal there is
/// the door's answer, never the mode's.
fn open_nest(db: Arc<CacheDb>) -> Arc<AppState> {
    Arc::new(AppState {
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOOR_DOMAIN.to_string()),
                reserved_handles: vec![],
            },
            ..Default::default()
        },
        registration_mode: Arc::new(tokio::sync::RwLock::new((RegistrationMode::Open, None))),
        ..AppState::for_test(db)
    })
}

/// The anonymous connection's actor slot, what a pre-identity kind dispatches
/// under.
const ANON: [u8; 32] = [0u8; 32];

/// A signed `fauna.account.invite_request.submit` for `handle`, as the wizard
/// sends it.
fn invite_submit_payload(kp: &ActorKeypair, handle: &str) -> Bytes {
    let ts = fauna_core::data::Timestamp::now_millis();
    let msg =
        fauna_protocol::invite::invite_submit_signed_message(&kp.actor_id().0, handle, "", ts);
    let sig = kp.signing_key().sign(&msg);
    encode(&InviteRequestSubmit {
        actor_id: hex::encode(kp.actor_id().0),
        handle: handle.to_string(),
        message: String::new(),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        age_claim: None,
        extra: Default::default(),
    })
}

fn invite_status_payload(kp: &ActorKeypair) -> Bytes {
    encode(&InviteRequestStatusQuery {
        actor_id: hex::encode(kp.actor_id().0),
        extra: Default::default(),
    })
}

fn handle_available_payload(handle: &str) -> Bytes {
    encode(&HandleAvailableRequest {
        handle: handle.to_string(),
        extra: Default::default(),
    })
}

/// Admit `kp` through the invite path exactly as the admin would: the actor's
/// signed submit, then `fauna.admin.invite_requests.approve` — which creates the
/// `users` row and consumes the request row.
async fn admit_by_invite(
    router: &RpcRouter,
    state: Arc<AppState>,
    admin: [u8; 32],
    kp: &ActorKeypair,
    handle: &str,
) {
    let out = dispatch(
        router,
        state.clone(),
        ANON,
        "fauna.account.invite_request.submit",
        invite_submit_payload(kp, handle),
    )
    .await
    .expect("a stranger's submit creates the pending row");
    let pending: InviteRequestStatus = decode_reply(&out).unwrap();
    assert_eq!(pending.status, "pending");
    dispatch(
        router,
        state,
        admin,
        "fauna.admin.invite_requests.approve",
        encode(&AdminInviteRequestApproveRequest {
            id: pending.id,
            ..Default::default()
        }),
    )
    .await
    .expect("approve admits the requester");
}

/// Door (1a): a suspended actor's signed invite submit is refused with the same
/// `actor_exists` an active account gets — no suspended-specific code, and no
/// pending row left behind (a suspended account's way back is Restore, never a
/// second admission).
#[tokio::test]
async fn a_suspended_actors_invite_submit_is_refused_as_already_registered() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = door_router();
    let admin = admin_actor(&st).await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    st.db
        .create_user_with_handle(&actor, "personal", "alice", None)
        .await
        .unwrap();

    // The control: an active account is refused with `actor_exists`.
    let active = dispatch(
        &r,
        st.clone(),
        ANON,
        "fauna.account.invite_request.submit",
        invite_submit_payload(&kp, "alice"),
    )
    .await
    .unwrap_err();
    assert_eq!(active.code, "fauna.account.actor_exists");

    suspend(&r, st.clone(), admin, actor)
        .await
        .expect("suspend");

    let suspended = dispatch(
        &r,
        st.clone(),
        ANON,
        "fauna.account.invite_request.submit",
        invite_submit_payload(&kp, "alice"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        suspended.code, "fauna.account.actor_exists",
        "a suspended account is still an account: the one code, no suspended-specific variant"
    );
    assert!(
        st.db
            .get_invite_request_by_actor(&actor)
            .await
            .unwrap()
            .is_none(),
        "no pending row for a suspended actor — Restore is the way back, never a second admission"
    );
}

/// Door (1b): on an open-registration nest — where a stranger's register mints
/// an account — a suspended actor's register is refused with `actor_exists`,
/// the door's answer and not the mode's.
#[tokio::test]
async fn a_suspended_actors_open_registration_is_refused_as_already_registered() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = door_router();
    let admin = admin_actor(&st).await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    st.db
        .create_user_with_handle(&actor, "personal", "alice", None)
        .await
        .unwrap();
    suspend(&r, st.clone(), admin, actor)
        .await
        .expect("suspend");

    // The posture control: a stranger registers here.
    let stranger = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        ANON,
        "fauna.account.register",
        register_payload(&stranger, "bob", DOOR_DOMAIN, None, None),
    )
    .await
    .expect("open registration mints a stranger's account");

    let refused = dispatch(
        &r,
        st.clone(),
        ANON,
        "fauna.account.register",
        register_payload(&kp, "alice", DOOR_DOMAIN, None, None),
    )
    .await
    .unwrap_err();
    assert_eq!(
        refused.code, "fauna.account.actor_exists",
        "the register door refuses a suspended actor exactly as it refuses an active one"
    );
    assert!(
        st.db.get_user(&actor).await.unwrap().unwrap().suspended,
        "the refused register left the suspended row untouched"
    );
}

/// (2) `invite_request.status` does not leak: an actor admitted through the
/// invite path reads `invite_request_not_found` whether active, suspended, or a
/// stranger — approval consumed the request row, so there is nothing to answer.
#[tokio::test]
async fn invite_request_status_answers_a_suspended_invite_admitted_actor_like_a_stranger() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = door_router();
    let admin = admin_actor(&st).await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    admit_by_invite(&r, st.clone(), admin, &kp, "carol").await;
    assert!(
        st.db.get_user(&actor).await.unwrap().is_some(),
        "approve created the users row"
    );

    let status = |st: Arc<AppState>, who: &ActorKeypair| {
        let r = &r;
        let payload = invite_status_payload(who);
        async move {
            dispatch(r, st, ANON, "fauna.account.invite_request.status", payload)
                .await
                .unwrap_err()
                .code
        }
    };

    let admitted = status(st.clone(), &kp).await;
    assert_eq!(admitted, "fauna.account.invite_request_not_found");

    suspend(&r, st.clone(), admin, actor)
        .await
        .expect("suspend");
    let suspended = status(st.clone(), &kp).await;
    assert_eq!(
        suspended, "fauna.account.invite_request_not_found",
        "the status read answers a suspended invite-admitted actor exactly as it did before"
    );

    let stranger = status(st.clone(), &ActorKeypair::generate()).await;
    assert_eq!(
        stranger, suspended,
        "…and exactly as it answers a stranger: no suspended-vs-unregistered bit here"
    );
}

/// (3) The disclosure is no wider than what is public already: the anonymous
/// handle probe reports a suspended user's handle taken exactly like an active
/// user's — suspension does not free the handle.
#[tokio::test]
async fn the_public_handle_probe_reports_a_suspended_users_handle_taken_like_an_active_ones() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = door_router();
    let admin = admin_actor(&st).await;
    let user = plain_user(&st.db, "dave").await;

    let probe = |st: Arc<AppState>| {
        let r = &r;
        async move {
            let out = dispatch(
                r,
                st,
                ANON,
                "fauna.handle.available",
                handle_available_payload("dave"),
            )
            .await
            .expect("probe answers");
            decode_reply::<HandleAvailableReply>(&out).unwrap()
        }
    };

    assert!(
        !probe(st.clone()).await.available,
        "an active user's handle is taken"
    );
    suspend(&r, st.clone(), admin, user).await.expect("suspend");
    let after = probe(st.clone()).await;
    assert!(
        !after.available && !after.cooldown,
        "a suspended user's handle stays taken (not freed, not in cooldown): the public bit is unchanged by suspension"
    );
}

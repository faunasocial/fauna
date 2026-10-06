//! In-process WS-RPC conformance for family-safety v1 slice 1 — the
//! guardianship link at admission (`docs/goal/behavior/family-safety.md`
//! § Wire & data shape; slice 1, tracked internally).
//!
//! Covers both admission entry points carrying the supervised designation:
//! invite-code mint → verify (pre-redemption disclosure) → register, and
//! invite-request approve — plus guardian validation (must exist, not be
//! suspended, not itself be supervised, never the admitted actor) and the
//! one-transaction invariant (a failed admission leaves no user row and no
//! link row).
//!
//! Tier: tier_3 (real `AppState` + real in-memory `CacheDb` — no mocks).

mod common;
use common::admin_actor;
use common::encode;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_core::obligation::{ContentFloor, ContentPolicy};
use fauna_core::screen_time::ScreenTimePolicy;
use fauna_nest::bridge_management::{
    BridgeError, BridgeFollow, BridgeLinkMode, BridgeProvider, BridgeProviderRegistry,
    BridgeStatus, LinkReply,
};
use fauna_nest::db::CacheDb;
use fauna_nest::federation_handlers::FedWelcomeDeliverRequest;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::{AuthState, BridgeState};
use fauna_nest::{
    account_handlers, admin_ws_handlers, contacts_handlers, family_handlers, inbox_handlers,
    invite_handlers,
};
use fauna_protocol::admin::{
    AdminInviteCodeCreateReply, AdminInviteCodeCreateRequest, AdminInviteCodesListReply,
    AdminInviteCodesListRequest, AdminInviteRequestApproveReply, AdminInviteRequestApproveRequest,
    AdminUserCreateRequest,
};
use fauna_protocol::contacts::KnockActionRequest;
use fauna_protocol::conversations::{WelcomeDeliverRequest, WelcomeKind};
use fauna_protocol::family::{
    FamilyApprovalDecideRequest, FamilyApprovalsListReply, FamilyApprovalsListRequest,
    FamilyContactAddRequest, FamilyContactRequestRequest, FamilyContentNotice,
    FamilyDeviceMarkRequest, FamilyFeedSourceRequestRequest, FamilyGraduateRequest,
    FamilyNotifyReportRequest, FamilyPolicyUpdateRequest, FamilyStatusReply, FamilyStatusRequest,
    FamilyTransferAcceptRequest, FamilyTransferCancelRequest, FamilyTransferDeclineRequest,
    FamilyTransferRequest, FamilyUsageReportReply, FamilyUsageReportRequest, ReachPolicy,
};
use fauna_protocol::inbox::{InboxSendReply, InboxSendRequest};
use fauna_protocol::invite::{InviteCodeVerify, InviteCodeVerifyReply};
use fauna_protocol::node_policy::RegistrationMode;
use fauna_protocol::sync::{
    DeviceGrantRegisterRequest, DeviceGrantRevokeRequest, SyncDeviceDeleteRequest,
    SyncDevicesListReply, SyncDevicesListRequest, SyncRegisterRequest,
};
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode};

const DOMAIN: &str = "test.fauna.social";

/// Router carrying the admin + invite + account handler families — slice 1
/// spans all three (mint / verify / register / approve).
fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    invite_handlers::register_invite_handlers(&mut b);
    account_handlers::register_account_handlers(&mut b);
    account_handlers::register_account_user_handlers(&mut b);
    family_handlers::register_family_handlers(&mut b);
    contacts_handlers::register_contacts_handlers(&mut b);
    inbox_handlers::register_inbox_handlers(&mut b);
    // The MLS Welcome plane is a second, independent inbox-write path; the reach
    // pillar has to bind on it too.
    fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    // The device-sync surface — the guardian-enrolled-device marker (§ Full
    // visibility) is enforced on `devices.delete` and rendered on
    // `devices.list`, so the family conformance router carries both.
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    b.build()
}

/// The peer-facing federation router — `fauna.federation.welcome.deliver` is
/// **open-federation**: any unauthenticated peer nest may call it, so it is the
/// widest surface the reach pillar has to cover.
fn fed_router() -> fauna_nest::federation_router::FederationRouter {
    let mut b = fauna_nest::federation_router::FederationRouter::builder();
    fauna_nest::federation_handlers::register_federation_handlers(&mut b);
    b.build()
}

/// Dispatch a federation kind as though a peer nest with `origin_nest_id` had
/// called it over the channel.
async fn fed_dispatch(
    router: &fauna_nest::federation_router::FederationRouter,
    st: Arc<AppState>,
    origin_nest_id: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("federation kind registered");
    (meta.handler)(st, origin_nest_id, payload).await
}

/// An invite-gated nest — supervised admission rides an invite code carrying the
/// guardian designation, so the posture must actually demand one. The posture is
/// the `registration_mode` singleton now, not the old `open` + `invite_required`
/// boolean pair (whose fourth combination was incoherent).
fn state(db: Arc<CacheDb>) -> Arc<AppState> {
    state_with_providers(db, None)
}

/// The same nest, but with a bridge provider registry actually configured.
///
/// `AppState::for_test` leaves `bridge.providers` at `None`, which makes every
/// `link` / `add_follow` fail at the registry before it can reach anything
/// interesting. That default is why the feed-source gate's *other* two
/// operations were only ever provable through `feeds.create` — a real gap, not
/// a law: the registry is public and takes any [`BridgeProvider`], so a stub
/// lets the link/follow gate be exercised end-to-end like any other path.
fn state_with_providers(
    db: Arc<CacheDb>,
    providers: Option<Arc<BridgeProviderRegistry>>,
) -> Arc<AppState> {
    Arc::new(AppState {
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                reserved_handles: vec![],
            },
            ..Default::default()
        },
        registration_mode: Arc::new(tokio::sync::RwLock::new((
            RegistrationMode::InviteRequired,
            None,
        ))),
        bridge: BridgeState {
            providers,
            ..Default::default()
        },
        ..AppState::for_test(db)
    })
}

/// A nest whose one bridge is `stub`, configured as the test asks.
fn state_with_stub(db: Arc<CacheDb>, stub: StubBridge) -> Arc<AppState> {
    let mut registry = BridgeProviderRegistry::new();
    registry.register(Box::new(stub));
    state_with_providers(db, Some(Arc::new(registry)))
}

/// A minimal [`BridgeProvider`] whose availability and link/follow outcomes the
/// test dictates, so the feed-source gate's placement is observable:
/// `performed` records whether the gated operation actually ran.
struct StubBridge {
    /// What `available()` / `available_for_link()` answer.
    available: bool,
    /// Whether `link()` / `add_follow()` fail *once reached* — the
    /// non-deterministic class of failure, which must still burn the grant.
    fails: bool,
    performed: Arc<AtomicBool>,
}

impl StubBridge {
    fn new(available: bool, fails: bool) -> (Self, Arc<AtomicBool>) {
        let performed = Arc::new(AtomicBool::new(false));
        (
            Self {
                available,
                fails,
                performed: performed.clone(),
            },
            performed,
        )
    }

    fn perform(&self) -> Result<(), BridgeError> {
        self.performed.store(true, Ordering::SeqCst);
        if self.fails {
            return Err(BridgeError::not_found("stub refused"));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl BridgeProvider for StubBridge {
    fn id(&self) -> &str {
        "stub"
    }
    fn name(&self) -> &str {
        "Stub"
    }
    async fn available(&self, _state: &AppState) -> bool {
        self.available
    }
    fn link_modes(&self) -> Vec<BridgeLinkMode> {
        Vec::new()
    }
    fn supports_follows(&self) -> bool {
        true
    }
    async fn status(
        &self,
        _state: &AppState,
        _actor_id: &str,
    ) -> Result<BridgeStatus, BridgeError> {
        Ok(BridgeStatus {
            linked: false,
            identity: None,
            mode: None,
            settings: Vec::new(),
            link_modes: None,
        })
    }
    async fn link(
        &self,
        _state: &AppState,
        _actor_id: &str,
        _mode: &str,
        _params: fauna_protocol::Value,
    ) -> Result<LinkReply, BridgeError> {
        self.perform()?;
        Ok(LinkReply {
            linked: true,
            identity: None,
            redirect_url: None,
            extra: Default::default(),
        })
    }
    async fn unlink(&self, _state: &AppState, _actor_id: &str) -> Result<(), BridgeError> {
        Ok(())
    }
    async fn update_settings(
        &self,
        _state: &AppState,
        _actor_id: &str,
        _settings: fauna_protocol::Value,
    ) -> Result<(), BridgeError> {
        Ok(())
    }
    async fn list_follows(
        &self,
        _state: &AppState,
        _actor_id: &str,
    ) -> Result<Vec<BridgeFollow>, BridgeError> {
        Ok(Vec::new())
    }
    async fn add_follow(
        &self,
        _state: &AppState,
        _actor_id: &str,
        _id: &str,
        _petname: Option<&str>,
        _extra: Option<fauna_protocol::Value>,
    ) -> Result<(), BridgeError> {
        self.perform()
    }
    async fn remove_follow(
        &self,
        _state: &AppState,
        _actor_id: &str,
        _follow_id: &str,
    ) -> Result<(), BridgeError> {
        Ok(())
    }
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

/// A pre-existing full account to act as guardian.
async fn guardian_user(db: &CacheDb, handle: &str) -> [u8; 32] {
    let kp = ActorKeypair::generate();
    let id = kp.actor_id().0;
    db.create_user_with_handle(&id, "personal", handle, None)
        .await
        .unwrap();
    id
}

/// Mint an invite code carrying a guardian; returns the minted token.
async fn mint_with_guardian(
    router: &RpcRouter,
    state: Arc<AppState>,
    admin: [u8; 32],
    guardian: Option<[u8; 32]>,
) -> Result<String, RpcError> {
    let out = dispatch(
        router,
        state,
        admin,
        "fauna.admin.invite_codes.create",
        encode(&AdminInviteCodeCreateRequest {
            guardian_actor: guardian.map(|g| ByteBuf::from(g.to_vec())),
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode::<AdminInviteCodeCreateReply>(&out).unwrap().code)
}

// ── invite-code path ────────────────────────────────────────────────────────

#[tokio::test]
async fn invite_code_with_guardian_carries_supervised_through_admission() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    // Mint at the default tier with a guardian designation.
    let code = mint_with_guardian(&r, st.clone(), admin, Some(guardian))
        .await
        .expect("mint ok");

    // The list row surfaces the designation (the Invite section renders it).
    let list: AdminInviteCodesListReply = decode(
        &dispatch(
            &r,
            st.clone(),
            admin,
            "fauna.admin.invite_codes.list",
            encode(&AdminInviteCodesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let row = list.invite_codes.iter().find(|c| c.code == code).unwrap();
    assert_eq!(
        row.guardian_actor.as_deref().map(|g| g.as_slice()),
        Some(guardian.as_slice()),
        "list row carries the guardian"
    );

    // Pre-redemption disclosure: verify shows who will supervise the account.
    let verify: InviteCodeVerifyReply = decode(
        &dispatch(
            &r,
            st.clone(),
            [0u8; 32],
            "fauna.account.invite_code.verify",
            encode(&InviteCodeVerify {
                code: code.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("verify ok"),
    )
    .unwrap();
    assert_eq!(
        verify.supervised_by.as_deref(),
        Some("parent"),
        "verify discloses the guardian handle before redemption"
    );

    // Redeem: the child registers with the code.
    let child = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&child, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("register ok");

    // The link exists and points at the guardian.
    let link = db
        .get_guardian_of(&child.actor_id().0)
        .await
        .unwrap()
        .expect("guardianship link created at admission");
    assert_eq!(link.guardian_actor_id, guardian.to_vec());

    // Guardian-side read of the same edge.
    let wards = db.list_wards(&guardian).await.unwrap();
    assert_eq!(wards.len(), 1);
    assert_eq!(wards[0].supervised_actor_id, child.actor_id().0.to_vec());

    // The default policy row exists with unsupervised-equivalent defaults —
    // a fresh link changes nothing until the guardian tightens it.
    let policy = db
        .get_guardian_policy(&child.actor_id().0)
        .await
        .unwrap()
        .expect("default policy row created at admission");
    assert!(!policy.contact_approval);
    assert_eq!(policy.unknown_sender_mail, "allow");
    assert!(policy.federation_contact);
    assert_eq!(policy.feed_sources, "allow");
}

#[tokio::test]
async fn plain_invite_code_admission_creates_no_link() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;

    let code = mint_with_guardian(&r, st.clone(), admin, None)
        .await
        .expect("mint ok");

    let verify: InviteCodeVerifyReply = decode(
        &dispatch(
            &r,
            st.clone(),
            [0u8; 32],
            "fauna.account.invite_code.verify",
            encode(&InviteCodeVerify {
                code: code.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(verify.supervised_by, None);

    let kp = ActorKeypair::generate();
    dispatch(
        &r,
        st,
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&kp, "adult", DOMAIN, Some(&code), None),
    )
    .await
    .expect("register ok");
    assert!(
        db.get_guardian_of(&kp.actor_id().0)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn guardian_cannot_redeem_their_own_supervised_code() {
    // The admitted actor must never equal the guardian (supervised-by-self is
    // unrepresentable). The enforcing mechanism today is the existing
    // actor-exists gate: a guardian is by definition an existing user (mint
    // validates that), and an existing actor can never register again —
    // `register_core`'s explicit guardian≠redeemer check is defense-in-depth
    // behind it. Either way: a typed refusal, and no link row appears.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;

    let gk = ActorKeypair::generate();
    db.create_user_with_handle(&gk.actor_id().0, "personal", "parent2", None)
        .await
        .unwrap();
    let code = mint_with_guardian(&r, st.clone(), admin, Some(gk.actor_id().0))
        .await
        .unwrap();

    let err = dispatch(
        &r,
        st,
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&gk, "parent2-again", DOMAIN, Some(&code), None),
    )
    .await
    .expect_err("guardian redeeming own supervised code is refused");
    assert_eq!(err.code, "fauna.account.actor_exists");
    assert!(
        db.get_guardian_of(&gk.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "no self-guardianship link appeared"
    );
}

// ── guardian validation at mint ─────────────────────────────────────────────

#[tokio::test]
async fn mint_rejects_unknown_suspended_or_supervised_guardian() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;

    // Unknown actor.
    let err = mint_with_guardian(&r, st.clone(), admin, Some([9u8; 32]))
        .await
        .expect_err("unknown guardian refused");
    assert_eq!(err.code, "fauna.admin.not_found");

    // Suspended user.
    let suspended = guardian_user(&db, "away").await;
    db.suspend_user_now(&suspended, "suspended by admin", "other")
        .await
        .unwrap();
    let err = mint_with_guardian(&r, st.clone(), admin, Some(suspended))
        .await
        .expect_err("suspended guardian refused");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    // A supervised account cannot itself guard (no chains): admit a child
    // under a valid guardian, then try to name the child as a guardian.
    let guardian = guardian_user(&db, "parent").await;
    let code = mint_with_guardian(&r, st.clone(), admin, Some(guardian))
        .await
        .unwrap();
    let child = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&child, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("child admitted");
    let err = mint_with_guardian(&r, st.clone(), admin, Some(child.actor_id().0))
        .await
        .expect_err("supervised guardian refused");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

// ── invite-request approval path ────────────────────────────────────────────

#[tokio::test]
async fn approve_with_guardian_creates_link_and_policy() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let child = [3u8; 32];
    let id = db
        .create_invite_request(&child, "kiddo", "hi", None)
        .await
        .unwrap();

    let reply: AdminInviteRequestApproveReply = decode(
        &dispatch(
            &r,
            st.clone(),
            admin,
            "fauna.admin.invite_requests.approve",
            encode(&AdminInviteRequestApproveRequest {
                id,
                tier: Some("free".into()),
                guardian_actor: Some(ByteBuf::from(guardian.to_vec())),
                ..Default::default()
            }),
        )
        .await
        .expect("approve ok"),
    )
    .unwrap();
    assert_eq!(reply.handle, "kiddo");

    let link = db.get_guardian_of(&child).await.unwrap().expect("link");
    assert_eq!(link.guardian_actor_id, guardian.to_vec());
    assert!(db.get_guardian_policy(&child).await.unwrap().is_some());

    // The DB link alone is not the contract the guardian's client reads —
    // `fauna.family.status` is. `family_status_reads_both_roles` proves the
    // guardian side over the *mint + register* admission; this proves it over
    // the *approve* admission, the only one a real client can drive on a closed
    // nest. (The Windows app's live end-to-end run saw an empty `wards` list
    // here, leaving `test_family_guardian_sees_ward` xfail.)
    let g_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok"),
    )
    .unwrap();
    assert_eq!(
        g_status.wards.len(),
        1,
        "the guardian sees the ward admitted via invite_requests.approve"
    );
    assert_eq!(g_status.wards[0].actor_id.as_slice(), child.as_slice());
    assert_eq!(g_status.wards[0].handle, "kiddo");

    // ...and the ward's own side of the same admission.
    let c_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st,
            child,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("ward status ok"),
    )
    .unwrap();
    let sup = c_status.supervised_by.expect("ward is supervised");
    assert_eq!(sup.actor_id.as_slice(), guardian.as_slice());
    assert_eq!(sup.handle, "parent");
}

#[tokio::test]
async fn approve_rejects_guardian_equal_to_requester_and_leaves_no_user() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;

    // The requester names themself as guardian — refused, and the failed
    // admission left no account behind (one-transaction invariant).
    let requester = [4u8; 32];
    let id = db
        .create_invite_request(&requester, "selfguard", "hi", None)
        .await
        .unwrap();
    let err = dispatch(
        &r,
        st,
        admin,
        "fauna.admin.invite_requests.approve",
        encode(&AdminInviteRequestApproveRequest {
            id,
            tier: Some("free".into()),
            guardian_actor: Some(ByteBuf::from(requester.to_vec())),
            ..Default::default()
        }),
    )
    .await
    .expect_err("self-guardian refused");
    assert_eq!(err.code, "fauna.admin.invalid_params");
    assert!(!db.is_actor_registered(&requester).await.unwrap());
    assert!(db.get_guardian_of(&requester).await.unwrap().is_none());
}

// ── direct-admission path (fauna.admin.users.create) ────────────────────────

#[tokio::test]
async fn direct_admission_with_guardian_creates_link_and_policy() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let child = [7u8; 32];
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(child.to_vec()),
            tier: "free".into(),
            handle: Some("kiddo".into()),
            guardian_actor: Some(ByteBuf::from(guardian.to_vec())),
            ..Default::default()
        }),
    )
    .await
    .expect("direct admission ok");

    let link = db.get_guardian_of(&child).await.unwrap().expect("link");
    assert_eq!(link.guardian_actor_id, guardian.to_vec());
    assert!(db.get_guardian_policy(&child).await.unwrap().is_some());

    // The contract the clients read is `fauna.family.status` — this pins it
    // over the *direct* admission, the third account-creation path
    // (family-safety.md § The guardianship link), guardian side first.
    let g_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok"),
    )
    .unwrap();
    assert_eq!(
        g_status.wards.len(),
        1,
        "the guardian sees the ward admitted via users.create"
    );
    assert_eq!(g_status.wards[0].actor_id.as_slice(), child.as_slice());
    assert_eq!(g_status.wards[0].handle, "kiddo");

    // ...and the ward's own side of the same admission.
    let c_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st,
            child,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("ward status ok"),
    )
    .unwrap();
    let sup = c_status.supervised_by.expect("ward is supervised");
    assert_eq!(sup.actor_id.as_slice(), guardian.as_slice());
    assert_eq!(sup.handle, "parent");
}

#[tokio::test]
async fn direct_admission_with_band_carries_it_alongside_the_guardian() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let child = [8u8; 32];
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(child.to_vec()),
            tier: "free".into(),
            handle: Some("kiddo".into()),
            guardian_actor: Some(ByteBuf::from(guardian.to_vec())),
            age_band: Some("u13".into()),
            ..Default::default()
        }),
    )
    .await
    .expect("banded direct admission ok");

    assert!(db.get_guardian_of(&child).await.unwrap().is_some());
    let (band, provenance) = db.get_age_band(&child).await.unwrap().expect("band row");
    assert_eq!(band, "u13");
    assert_eq!(provenance, "guardian-asserted");
}

#[tokio::test]
async fn direct_admission_rejects_self_guardian_and_leaves_no_user() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;

    let target = [9u8; 32];
    let err = dispatch(
        &r,
        st,
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(target.to_vec()),
            tier: "free".into(),
            handle: Some("selfguard".into()),
            guardian_actor: Some(ByteBuf::from(target.to_vec())),
            ..Default::default()
        }),
    )
    .await
    .expect_err("self-guardian refused");
    assert_eq!(err.code, "fauna.admin.invalid_params");
    assert!(!db.is_actor_registered(&target).await.unwrap());
    assert!(db.get_guardian_of(&target).await.unwrap().is_none());
}

#[tokio::test]
async fn direct_admission_rejects_guardian_on_handleless_admit() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    // The handle-less admission (the admit form's blank handle) cannot be
    // supervised: both other supervision-carrying paths always carry a handle
    // (registering is choosing a handle).
    let target = [10u8; 32];
    let err = dispatch(
        &r,
        st,
        admin,
        "fauna.admin.users.create",
        encode(&AdminUserCreateRequest {
            actor_id: ByteBuf::from(target.to_vec()),
            tier: "free".into(),
            guardian_actor: Some(ByteBuf::from(guardian.to_vec())),
            ..Default::default()
        }),
    )
    .await
    .expect_err("guardian on a handle-less admit refused");
    assert_eq!(err.code, "fauna.admin.invalid_params");
    assert!(!db.is_actor_registered(&target).await.unwrap());
    assert!(db.get_guardian_of(&target).await.unwrap().is_none());
}

// ── slice 2: fauna.family.* — status / policy.update / graduate / transfer ──

/// Admit a supervised child under `guardian` and return the child's actor id.
async fn admit_ward(
    router: &RpcRouter,
    st: Arc<AppState>,
    admin: [u8; 32],
    guardian: [u8; 32],
    handle: &str,
) -> [u8; 32] {
    admit_ward_keyed(router, st, admin, guardian, handle)
        .await
        .actor_id()
        .0
}

/// [`admit_ward`], answering the child's identity keypair — what every device
/// of the account holds, the guardian's enrolled one included, and so what
/// signs a device grant for any key.
async fn admit_ward_keyed(
    router: &RpcRouter,
    st: Arc<AppState>,
    admin: [u8; 32],
    guardian: [u8; 32],
    handle: &str,
) -> ActorKeypair {
    let code = mint_with_guardian(router, st.clone(), admin, Some(guardian))
        .await
        .unwrap();
    let child = ActorKeypair::generate();
    dispatch(
        router,
        st,
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&child, handle, DOMAIN, Some(&code), None),
    )
    .await
    .expect("child admitted");
    child
}

#[tokio::test]
async fn family_status_reads_both_roles() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Supervised side: who supervises me + my active policy.
    let child_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            child,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("child status ok"),
    )
    .unwrap();
    let sup = child_status.supervised_by.expect("child is supervised");
    assert_eq!(sup.actor_id.as_slice(), guardian.as_slice());
    assert_eq!(sup.handle, "parent");
    let policy = child_status.policy.expect("active policy present");
    assert!(!policy.contact_approval);
    assert_eq!(policy.unknown_sender_mail, "allow");
    assert!(policy.federation_contact);
    assert_eq!(policy.feed_sources, "allow");
    assert!(child_status.wards.is_empty());

    // Guardian side: my wards with their policies.
    let g_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok"),
    )
    .unwrap();
    assert!(g_status.supervised_by.is_none());
    assert_eq!(g_status.wards.len(), 1);
    assert_eq!(g_status.wards[0].actor_id.as_slice(), child.as_slice());
    assert_eq!(g_status.wards[0].handle, "kid");

    // An unrelated user: empty on both sides.
    let outsider = guardian_user(&db, "neighbor").await;
    let o_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st,
            outsider,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("outsider status ok"),
    )
    .unwrap();
    assert!(o_status.supervised_by.is_none() && o_status.wards.is_empty());
}

#[tokio::test]
async fn policy_update_is_guardian_only_and_persists() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let tightened = ReachPolicy {
        contact_approval: true,
        unknown_sender_mail: "hold".into(),
        federation_contact: false,
        feed_sources: "block".into(),
        ..Default::default()
    };

    // A non-guardian (even the admin) is refused — no oversight power on the
    // admin role (family-safety.md § The trust shape invariant 1).
    let outsider = guardian_user(&db, "neighbor").await;
    for (actor, who) in [(outsider, "outsider"), (admin, "admin")] {
        let err = dispatch(
            &r,
            st.clone(),
            actor,
            "fauna.family.policy.update",
            encode(&FamilyPolicyUpdateRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                policy: tightened.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("non-guardian policy update refused");
        assert_eq!(err.code, "fauna.family.permission_denied", "{who}");
    }

    // The child cannot edit their own policy either.
    let err = dispatch(
        &r,
        st.clone(),
        child,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            policy: tightened.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward cannot edit own policy");
    assert_eq!(err.code, "fauna.family.permission_denied");

    // The guardian can; the write persists and status reflects it.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            policy: tightened.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian policy update ok");
    let row = db.get_guardian_policy(&child).await.unwrap().unwrap();
    assert!(row.contact_approval);
    assert_eq!(row.unknown_sender_mail, "hold");
    assert!(!row.federation_contact);
    assert_eq!(row.feed_sources, "block");

    // Invalid enum values are refused.
    let err = dispatch(
        &r,
        st,
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            policy: ReachPolicy {
                unknown_sender_mail: "bounce".into(),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("invalid policy value refused");
    assert_eq!(err.code, "fauna.family.invalid_params");
}

/// `status` must surface the *exact* stored policy to both roles — the ward is
/// entitled to see precisely the policy the nest enforces against them
/// (`family-safety.md` § The trust shape invariant 4, "supervision is
/// transparent by construction"). Pins `policy_row_to_wire` against a future
/// divergence between the enforced policy and the ward-visible one — INFO-1,
/// raised in code review (tracked internally) against the reach-enforcement
/// slice that reads these knobs.
#[tokio::test]
async fn status_round_trips_the_exact_stored_policy_to_both_roles() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Every knob off its default, so a field-swap or a dropped field is visible.
    let stored = ReachPolicy {
        contact_approval: true,
        unknown_sender_mail: "reject".into(),
        federation_contact: false,
        feed_sources: "block".into(),
        ..Default::default()
    };
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            policy: stored.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian policy update ok");

    let status = async |actor: [u8; 32]| -> FamilyStatusReply {
        decode(
            &dispatch(
                &r,
                st.clone(),
                actor,
                "fauna.family.status",
                encode(&FamilyStatusRequest {
                    extra: Default::default(),
                }),
            )
            .await
            .expect("status readable"),
        )
        .unwrap()
    };

    // Ward side: the read-only policy the supervised client renders.
    let ward_view = status(child).await;
    assert_eq!(ward_view.policy.expect("ward sees a policy"), stored);

    // Guardian side: the same document, per ward.
    let guardian_view = status(guardian).await;
    let ward_row = guardian_view
        .wards
        .iter()
        .find(|w| w.actor_id.as_slice() == child.as_slice())
        .expect("guardian sees the ward");
    assert_eq!(ward_row.policy, stored);
}

/// v1.x Slice B-persistence — the content + screen-time pillars persist,
/// round-trip to both roles, and a **v1-shaped** `policy.update` (reach knobs
/// only, no sub-documents) must NOT wipe a previously-set pillar
/// (`family-safety.md` § Wire & data shape → *Policy-update compatibility*:97 —
/// "absent means leave that pillar unchanged"; the four reach knobs keep their
/// replace semantics). RED-first against a naive full-replace, which would reset
/// the content/screen columns to their defaults on the second update. Also pins
/// the closed-enum write validation (§ :97 — "the nest refuses what it cannot
/// name"): a present content sub-document carrying a floor the nest cannot parse
/// (`ContentFloor::Unknown`) is refused, and the refusal clobbers nothing.
#[tokio::test]
async fn v1x_pillars_persist_and_a_v1_shaped_update_leaves_them_intact() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let update = async |policy: ReachPolicy| -> Result<Bytes, RpcError> {
        dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.policy.update",
            encode(&FamilyPolicyUpdateRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                policy,
                extra: Default::default(),
            }),
        )
        .await
    };
    let ward_policy = async || -> ReachPolicy {
        let reply: FamilyStatusReply = decode(
            &dispatch(
                &r,
                st.clone(),
                child,
                "fauna.family.status",
                encode(&FamilyStatusRequest {
                    extra: Default::default(),
                }),
            )
            .await
            .expect("ward status ok"),
        )
        .unwrap();
        reply.policy.expect("ward sees an active policy")
    };

    // 1) The guardian sets both v1.x pillars, alongside tightening the reach
    //    knobs. A wrapping "bedtime" window exercises `window_start > window_end`.
    let with_pillars = ReachPolicy {
        contact_approval: true,
        unknown_sender_mail: "hold".into(),
        federation_contact: false,
        feed_sources: "block".into(),
        content_policy: Some(ContentPolicy {
            nsfw: ContentFloor::Block,
            spam: ContentFloor::Collapse,
            phishing: ContentFloor::Inherit,
            commercial: ContentFloor::Inherit,
        }),
        screen_time: Some(ScreenTimePolicy {
            window_start: Some(1260), // 21:00
            window_end: Some(420),    // 07:00 — wraps midnight
            daily_minutes: Some(120),
        }),
        // Guardian Notify on — round-trips through the same full-pillar save path.
        content_notify: Some(true),
        // The bridge-DM gate's knob (v1.x, § The bridge-DM gate) — same
        // absent-means-unchanged shape, so it rides this same save path and must
        // survive the v1-shaped update below untouched.
        unknown_peer_dm: Some("hold".into()),
        // The controversial-class feature sub-document (`dynamic-features.md`
        // § Wire & data shape) takes the same absent-means-unchanged shape as
        // the pillars above. Left absent here deliberately: this test is about
        // the v1.x pillars surviving a v1-shaped save, and the feature tier has
        // its own pins in `db::feature_gate`.
        features: None,
        extra: Default::default(),
    };
    update(with_pillars.clone()).await.expect("set pillars ok");

    // Both roles read the exact stored document back (transparency, § invariant 4).
    assert_eq!(ward_policy().await, with_pillars);
    let g_view: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok"),
    )
    .unwrap();
    let ward_row = g_view
        .wards
        .iter()
        .find(|w| w.actor_id.as_slice() == child.as_slice())
        .expect("guardian sees the ward");
    assert_eq!(ward_row.policy, with_pillars);

    // 2) A v1-era client saves ONLY the reach knobs — content_policy/screen_time
    //    absent on the wire (the shipped v1 shape). This is the clobber test:
    //    absent pillars must be left untouched, while the reach knobs replace.
    let v1_shaped = ReachPolicy {
        contact_approval: false,
        unknown_sender_mail: "allow".into(),
        federation_contact: true,
        feed_sources: "allow".into(),
        ..Default::default()
    };
    assert!(
        v1_shaped.content_policy.is_none()
            && v1_shaped.screen_time.is_none()
            && v1_shaped.unknown_peer_dm.is_none(),
        "the v1 shape carries no sub-documents and no v1.x knobs"
    );
    update(v1_shaped).await.expect("v1-shaped update ok");

    let after = ward_policy().await;
    // The reach knobs took the new (relaxed) values …
    assert!(!after.contact_approval);
    assert_eq!(after.unknown_sender_mail, "allow");
    assert!(after.federation_contact);
    assert_eq!(after.feed_sources, "allow");
    // … but the v1.x pillars are exactly as the guardian left them.
    assert_eq!(after.content_policy, with_pillars.content_policy);
    assert_eq!(after.screen_time, with_pillars.screen_time);
    // …including the bridge-DM gate's knob. This is the safety half of the
    // clobber rule: a v1-era client that cannot render `unknown_peer_dm` must
    // not silently relax a gate the guardian turned on — its save carries the
    // field absent, and absent means unchanged, never "allow".
    assert_eq!(
        after.unknown_peer_dm, with_pillars.unknown_peer_dm,
        "a v1-shaped save must not relax the bridge-DM gate"
    );

    // 3) A present content sub-document carrying a floor the nest cannot name is
    //    refused at write (closed-enum validation) — and refusing it clobbers
    //    nothing.
    //    `ContentFloor::Unknown` is never serialized (it is the collapsing
    //    arm), so the newer writer is modelled on the wire: a floor value this
    //    build has never heard of.
    let mut bad: fauna_protocol::Value = decode(&encode(&FamilyPolicyUpdateRequest {
        supervised_actor_id: ByteBuf::from(child.to_vec()),
        policy: ReachPolicy {
            content_policy: Some(ContentPolicy::default()),
            ..Default::default()
        },
        extra: Default::default(),
    }))
    .unwrap();
    {
        use fauna_protocol::Value;
        let Value::Map(top) = &mut bad else {
            panic!("a request is a map")
        };
        let Some(Value::Map(policy)) = top.get_mut("policy") else {
            panic!("policy")
        };
        let Some(Value::Map(content)) = policy.get_mut("content_policy") else {
            panic!("content_policy")
        };
        content.insert("nsfw".into(), Value::String("quarantine".into()));
    }
    let err = dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&bad),
    )
    .await
    .expect_err("unparseable content floor refused");
    assert_eq!(err.code, "fauna.family.invalid_params");
    assert_eq!(
        ward_policy().await.content_policy,
        with_pillars.content_policy,
        "a refused write leaves the stored pillar intact"
    );
}

#[tokio::test]
async fn graduation_converts_in_place_by_guardian_or_admin() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // The ward cannot graduate themself (the bounded exception).
    let err = dispatch(
        &r,
        st.clone(),
        child,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward cannot self-graduate");
    assert_eq!(err.code, "fauna.family.permission_denied");

    // The guardian graduates: link + policy gone, account untouched.
    let tier_before = db.get_user(&child).await.unwrap().unwrap().tier;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian graduates");
    assert!(db.get_guardian_of(&child).await.unwrap().is_none());
    assert!(db.get_guardian_policy(&child).await.unwrap().is_none());
    let user = db.get_user(&child).await.unwrap().expect("account intact");
    assert_eq!(user.tier, tier_before);
    assert_eq!(db.get_handle(&child).await.unwrap().as_deref(), Some("kid"));

    // Admin-graduate works too (removes oversight — the safe direction),
    // and graduating a non-supervised account is not_found.
    let child2 = admit_ward(&r, st.clone(), admin, guardian, "kid2").await;
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child2.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("admin graduates");
    assert!(db.get_guardian_of(&child2).await.unwrap().is_none());
    let err = dispatch(
        &r,
        st,
        admin,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child2.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("already graduated → not_found");
    assert_eq!(err.code, "fauna.family.not_found");
}

/// A guardian suspended *after* the link exists loses every `fauna.family.*`
/// call, matching the admission-time `check_guardian_admissible` rule
/// (`family-safety.md` § Lifecycle gates — the ward's approvals queue goes
/// *unattended*). The **ward's** reads stay open (transparency), and the admin
/// arm still resolves the link.
///
/// Since 2026-07-09 the refusal comes from the *dispatch* gate rather than
/// family's own `deny_if_suspended`: a suspended actor resolves to no caller
/// class at all, so it is refused before any handler body runs, on every kind
/// and in every registration mode (`admin.md` § 2 Users → *Cutting a user off*). `deny_if_suspended` stays as defense in depth.
#[tokio::test]
async fn suspended_guardian_loses_family_authority_but_admin_can_still_resolve() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let peer = guardian_user(&db, "stranger").await;

    db.suspend_user_now(&guardian, "suspended by admin", "other")
        .await
        .unwrap();

    // Every authority-bearing call is refused with the typed code.
    let tightened = ReachPolicy {
        contact_approval: true,
        unknown_sender_mail: "hold".into(),
        federation_contact: false,
        feed_sources: "block".into(),
        ..Default::default()
    };
    let attempts: Vec<(&str, Bytes)> = vec![
        (
            "fauna.family.policy.update",
            encode(&FamilyPolicyUpdateRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                policy: tightened,
                extra: Default::default(),
            }),
        ),
        (
            "fauna.family.contact.add",
            encode(&FamilyContactAddRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                peer_actor_id: ByteBuf::from(peer.to_vec()),
                extra: Default::default(),
            }),
        ),
        (
            "fauna.family.transfer",
            encode(&FamilyTransferRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                new_guardian_actor_id: ByteBuf::from(peer.to_vec()),
                extra: Default::default(),
            }),
        ),
        (
            "fauna.family.graduate",
            encode(&FamilyGraduateRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                extra: Default::default(),
            }),
        ),
    ];
    for (kind, payload) in attempts {
        let err = dispatch(&r, st.clone(), guardian, kind, payload)
            .await
            .expect_err("suspended guardian refused");
        // The dispatch gate (`caller_class_for_actor`) strips the caller class
        // before the handler body — and therefore before `deny_if_suspended`
        // could return its more specific `fauna.family.guardian_suspended`.
        //
        // And because the class is *stripped* rather than resolved to a wrong
        // one, this is the CENTRAL code, not `fauna.family.…`. A suspended actor
        // takes `caller_class_for_actor`'s own documented "None when the actor is
        // unknown or revoked" arm (`bridge_method_allowlist.rs` § Authority
        // gate), and `api-layers.md` § Caller-class authorization → *Refusal
        // codes at the gate* (ruled 2026-08-17) reserves
        // `fauna.bridges.permission_denied` for exactly that: an actor denied on
        // *every* kind, which is not a statement about the family's contract at
        // all. That every-kind shape is load-bearing on the wire — the Go
        // bridges' revocation probe keys on it — so this arm "must never become
        // per-family". The family code is for a caller that HAS a class and the
        // wrong one.
        assert_eq!(err.code, "fauna.bridges.permission_denied", "{kind}");
    }

    // The link is intact — suspension strips authority, it does not graduate.
    assert!(db.get_guardian_of(&child).await.unwrap().is_some());

    // The ward's reads stay open — it still sees who supervises it
    // (transparency). The suspended guardian itself now reads nothing: the
    // dispatch gate denies it every kind, as on a private nest, where
    // `check_actor_active` has always refused it authentication outright.
    let reply: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            child,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("ward status readable"),
    )
    .unwrap();
    assert_eq!(
        reply.supervised_by.expect("still supervised").actor_id,
        ByteBuf::from(guardian.to_vec())
    );

    // The admin arm is never suspension-gated — it is how the link gets resolved.
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("admin graduates a suspended guardian's ward");
    assert!(db.get_guardian_of(&child).await.unwrap().is_none());
}

/// The known-sender set backing `unknown_sender_mail` (`family-safety.md`
/// § The mail gate). Addresses fold case and shed envelope angle-brackets, the
/// empty address is the SMTP null reverse-path `<>` (a bounce — always known, so
/// a delivery-failure notice is never held from the ward), and seeding an
/// unsupervised account is a silent no-op so callers need not pre-check the link.
#[tokio::test]
async fn mail_allowlist_is_ward_scoped_normalized_and_idempotent() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let adult = guardian_user(&db, "grown-up").await;

    // The ward mails a correspondent; the address is seeded, case-folded.
    db.add_mail_allowlist_entry(&child, " <Alice@Example.COM> ", "outbound")
        .await
        .unwrap();
    // Idempotent — a second send to the same correspondent is not an error.
    db.add_mail_allowlist_entry(&child, "alice@example.com", "outbound")
        .await
        .unwrap();

    for probe in [
        "alice@example.com",
        "ALICE@EXAMPLE.COM",
        "<alice@example.com>",
    ] {
        assert!(
            db.is_known_mail_sender(&child, probe).await.unwrap(),
            "{probe} should be known"
        );
    }
    assert!(
        !db.is_known_mail_sender(&child, "bob@example.com")
            .await
            .unwrap()
    );

    // The null reverse-path is NEVER known — anyone can claim `MAIL FROM:<>`,
    // and reading it as known was a full gate bypass. Genuine remote bounces
    // flow via the DSN correlation in `guardian_mail_verdict` instead
    // (pinned in `conformance_family_mail_gate.rs`).
    assert!(!db.is_known_mail_sender(&child, "").await.unwrap());

    // Ward-scoped: one ward's correspondents are not another account's.
    assert!(
        !db.is_known_mail_sender(&adult, "alice@example.com")
            .await
            .unwrap()
    );

    // Seeding an unsupervised account is a no-op, not a row.
    db.add_mail_allowlist_entry(&adult, "carol@example.com", "outbound")
        .await
        .unwrap();
    assert!(
        !db.is_known_mail_sender(&adult, "carol@example.com")
            .await
            .unwrap()
    );
}

/// Graduation drops the known-sender set with the policy it served — the account
/// is now full, and an allowlist with no policy is dead metadata. It must never
/// drop a *held message*, though: `graduate` carries a fail-closed tripwire for
/// the slice that lands the held mailbox (`family-safety.md` § Reach approvals —
/// "graduation releases, never drops"). No hold writer exists yet, so graduation
/// is reachable today.
#[tokio::test]
async fn graduation_clears_the_known_sender_set() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    db.add_mail_allowlist_entry(&child, "alice@example.com", "outbound")
        .await
        .unwrap();
    assert!(
        db.is_known_mail_sender(&child, "alice@example.com")
            .await
            .unwrap()
    );

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("graduate ok");

    assert!(db.get_guardian_of(&child).await.unwrap().is_none());
    assert!(
        !db.is_known_mail_sender(&child, "alice@example.com")
            .await
            .unwrap()
    );
}

// ── the guardian-enrolled-device marker (family-safety.md § Full visibility) ──

/// Register one device for `actor` through the real wire kind, returning its
/// hex id. The guardian's enrolled device authenticates as the *ward's*
/// account — which is precisely why the nest cannot tell it apart without the
/// marker, and why these tests register it exactly like the child's own.
async fn register_device(r: &RpcRouter, st: Arc<AppState>, actor: [u8; 32], label: &str) -> String {
    let device_id = hex::encode(fauna_core::identity::ActorKeypair::generate().actor_id().0);
    dispatch(
        r,
        st,
        actor,
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: device_id.clone(),
            label: label.to_string(),
            capabilities: "read,write".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("device registered");
    device_id
}

/// Register a device under `actor` with a **caller-chosen** `device_id` — the
/// adversarial half of [`register_device`], which generates a fresh random id.
/// `fauna.sync.register` parses the wire string as 32-byte hex and stores it
/// verbatim, scoped to the calling actor (`sync_handlers.rs`), so a ward picking
/// their own id is an ordinary call, not an exploit primitive.
async fn register_device_with_id(
    r: &RpcRouter,
    st: Arc<AppState>,
    actor: [u8; 32],
    device_id: &str,
    label: &str,
) {
    dispatch(
        r,
        st,
        actor,
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: device_id.to_string(),
            label: label.to_string(),
            capabilities: "read,write".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("device registered");
}

/// Flip the hex character at `idx`, yielding a distinct id that agrees with
/// `hex` on every *other* character — a decoy whose first point of difference
/// the caller chooses exactly.
fn flip_hex_char_at(hex: &str, idx: usize) -> String {
    let mut chars: Vec<char> = hex.chars().collect();
    chars[idx] = if chars[idx] == '0' { '1' } else { '0' };
    chars.into_iter().collect()
}

/// One `fauna.sync.devices.list` read as `actor`.
async fn devices_list(r: &RpcRouter, st: Arc<AppState>, actor: [u8; 32]) -> SyncDevicesListReply {
    let reply = dispatch(
        r,
        st,
        actor,
        "fauna.sync.devices.list",
        encode(&SyncDevicesListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("devices listed");
    decode(&reply).expect("devices list decodes")
}

fn mark_req(ward: [u8; 32], device_id: &str, marked: bool) -> Bytes {
    encode(&FamilyDeviceMarkRequest {
        supervised_actor_id: ByteBuf::from(ward.to_vec()),
        device_id: device_id.to_string(),
        marked,
        extra: Default::default(),
    })
}

/// Slice F guardian-facing device list (`family-safety.md` § Full visibility
/// for young children): `fauna.family.status` carries each ward's registered
/// devices to the *guardian*, with their `guardian_marked` state — the surface
/// the guardian's per-device mark toggle (`family-device-mark-toggle`) renders
/// from. Without it the guardian has no `device_id` to feed
/// `fauna.family.device.mark` (`fauna.sync.devices.list` returns only the
/// caller's OWN devices), so the whole guardian mark-control leg was
/// unbuildable. Guardianship-guarded: the list rides `wards`, so only a linked
/// guardian ever sees a ward's devices. RED before `FamilyWardInfo.devices`
/// existed — `devices.len() == 2` reads `0` against the pre-fix empty list.
#[tokio::test]
async fn family_status_carries_the_ward_device_list_to_the_guardian() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Two devices on the ward, one of them the guardian's enrolled tablet.
    let own = register_device(&r, st.clone(), child, "kid's phone").await;
    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    // The guardian's own `fauna.family.status` now enumerates the ward's
    // devices — id, label, and mark state — from this one read, with no
    // `fauna.sync.devices.list` against the ward.
    let g_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok"),
    )
    .unwrap();
    assert_eq!(g_status.wards.len(), 1, "the guardian guards one ward");
    let devices = &g_status.wards[0].devices;
    assert_eq!(devices.len(), 2, "both of the ward's devices are listed");

    let enrolled_row = devices
        .iter()
        .find(|d| d.device_id == enrolled)
        .expect("the enrolled device is in the guardian's ward view");
    assert!(
        enrolled_row.guardian_marked,
        "the marked device reports guardian_marked=true so the toggle renders on"
    );
    // The 2026-08-02 ruling (family-safety.md § Full visibility for young
    // children): the guardian's projection carries the device's DISPLAY
    // IDENTITY, never the ward's user-chosen label — that label rests sealed
    // under the ward's own root, which the guardian neither holds nor may be
    // handed (no guardian key escrow). A user-labeled device therefore renders
    // the short device-id form, non-empty and distinct per device.
    assert_eq!(
        enrolled_row.label,
        fauna_core::format::short_id(&enrolled),
        "a user-labeled ward device shows the guardian its short device id, \
         not the sealed label and not an empty row"
    );

    let own_row = devices
        .iter()
        .find(|d| d.device_id == own)
        .expect("the ward's own device is listed too");
    assert!(
        !own_row.guardian_marked,
        "an unmarked device reports guardian_marked=false"
    );
    assert_eq!(own_row.label, fauna_core::format::short_id(&own));
    assert_ne!(
        enrolled_row.label, own_row.label,
        "the id-derived display keeps the ward's devices distinguishable"
    );

    // A machine-authored label ("fauna" — the only plaintext the post-flip
    // label column ever rests) is kept: it is not the ward's content, so the
    // guardian may read it. It renders BESIDE the device code, not instead of
    // it — every row carries the code because the label is not a discriminator
    // (`SELF_REGISTER_LABEL` is what every self-registering client passes, so
    // two of a ward's devices routinely share it) and because the guardian's
    // documented way to check they marked the right device is to compare that
    // code, which the ward does not author.
    let machine = register_device(&r, st.clone(), child, "fauna").await;
    let g_status2: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok after the machine register"),
    )
    .unwrap();
    let machine_row = g_status2.wards[0]
        .devices
        .iter()
        .find(|d| d.device_id == machine)
        .expect("the machine-labeled device is listed");
    assert_eq!(
        machine_row.label,
        format!("fauna · {}", fauna_core::format::short_id(&machine)),
        "a machine-authored plaintext label reaches the guardian verbatim, \
         beside the device code"
    );

    // Guardianship-guarded: a stranger who guards no one sees no wards at all,
    // so a ward's device list never leaks to a non-guardian.
    let stranger = guardian_user(&db, "stranger").await;
    let s_status: FamilyStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            stranger,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("stranger status ok"),
    )
    .unwrap();
    assert!(
        s_status.wards.is_empty(),
        "a non-guardian sees no wards, hence no ward devices"
    );
}

/// Read the guardian's view of `child`'s devices as `(device_id, label)` pairs.
async fn ward_device_rows(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
) -> Vec<(String, String)> {
    let status: FamilyStatusReply = decode(
        &dispatch(
            r,
            st,
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("guardian status ok"),
    )
    .unwrap();
    status.wards[0]
        .devices
        .iter()
        .map(|d| (d.device_id.clone(), d.label.clone()))
        .collect()
}

/// `family-safety.md` § Full visibility for young children states the guardian's
/// device rows are *"guaranteed non-empty and per-device distinct"*. Distinctness
/// is the property the guardian's mark control rests on: the rows carry no other
/// text (`apps/fauna-linux/src/views/family.rs`), and the mark is the only thing
/// stopping the ward removing the guardian's enrolled device (`sync_handlers.rs`,
/// predicate `guardian_marked AND currently supervised`). Marking the wrong row
/// therefore leaves the real one removable while the guardian's page reads correct.
///
/// **`device_id` is chosen by the client, not derived** — `fauna.sync.register`
/// stores the caller's hex verbatim, and a ward is an ordinary user on their own
/// account who is *handed* the guardian device's id by design (it appears in
/// their own `fauna.sync.devices.list`). So distinctness cannot come from any
/// fixed-width prefix: the ward can match whatever subset of positions is shown.
/// RED before the set-relative widening — a decoy sharing the first 12 hex chars
/// rendered a row byte-identical to the guardian's.
#[tokio::test]
async fn a_ward_cannot_forge_a_row_identical_to_the_guardians_enrolled_device() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;

    // The finding's exact shape: a decoy agreeing with the enrolled device on
    // the 12 hex characters `short_id` renders, differing beyond them.
    let decoy = flip_hex_char_at(&enrolled, 20);
    assert_eq!(
        decoy[..12],
        enrolled[..12],
        "the decoy shares the rendered prefix"
    );
    assert_ne!(decoy, enrolled, "the decoy is a different device");
    register_device_with_id(&r, st.clone(), child, &decoy, "kid's phone").await;

    let rows = ward_device_rows(&r, st.clone(), guardian).await;
    assert_eq!(rows.len(), 2, "both devices are listed to the guardian");
    let enrolled_label = &rows.iter().find(|(id, _)| *id == enrolled).unwrap().1;
    let decoy_label = &rows.iter().find(|(id, _)| *id == decoy).unwrap().1;
    assert_ne!(
        enrolled_label, decoy_label,
        "a ward-chosen decoy sharing the rendered prefix must not produce a row \
         the guardian cannot tell from their own enrolled device"
    );

    // The worst case a prefix can be handed: an id differing only in its final
    // character. The width is set-relative, so it widens to whatever separates
    // the set — here the full 64 — rather than to a fixed guess.
    let twin = flip_hex_char_at(&enrolled, enrolled.len() - 1);
    register_device_with_id(&r, st.clone(), child, &twin, "kid's tablet").await;
    let rows = ward_device_rows(&r, st.clone(), guardian).await;
    assert_eq!(rows.len(), 3, "all three devices are listed");
    let labels: std::collections::BTreeSet<&String> = rows.iter().map(|(_, l)| l).collect();
    assert_eq!(
        labels.len(),
        3,
        "every row is distinct even when two ids differ only in their last \
         character — got {labels:?}"
    );
    assert!(
        rows.iter().all(|(_, l)| !l.is_empty()),
        "and every row stays non-empty, the other half of the doc's guarantee"
    );
}

/// The same guarantee, with **no attacker at all**: `SELF_REGISTER_LABEL`
/// (`"fauna"`) is the label every self-registering client passes, and it is one
/// of the three machine-authored constants that rest as plaintext
/// (`label_custody::is_synthetic_device_label`). So a ward with two
/// self-registered devices is the ordinary case in which a label-only render
/// collides. RED before the fix — both rows read `fauna`.
#[tokio::test]
async fn two_self_registered_ward_devices_do_not_render_the_same_row() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let first = register_device(
        &r,
        st.clone(),
        child,
        fauna_core::label_custody::SELF_REGISTER_LABEL,
    )
    .await;
    let second = register_device(
        &r,
        st.clone(),
        child,
        fauna_core::label_custody::SELF_REGISTER_LABEL,
    )
    .await;

    let rows = ward_device_rows(&r, st.clone(), guardian).await;
    let first_label = &rows.iter().find(|(id, _)| *id == first).unwrap().1;
    let second_label = &rows.iter().find(|(id, _)| *id == second).unwrap().1;
    assert_ne!(
        first_label, second_label,
        "two devices carrying the same machine-authored label must still render \
         distinct rows — the guardian marks one of them"
    );
    assert!(
        first_label.contains(fauna_core::label_custody::SELF_REGISTER_LABEL),
        "and the machine label the guardian may legitimately read is kept, not \
         dropped, when the id is added to disambiguate — got {first_label:?}"
    );
}

/// The core promise (`family-safety.md` § Full visibility): *the child cannot
/// unilaterally remove the guardian's device*. Mark → the ward's own delete is
/// refused → the guardian unmarks → the delete proceeds. The mark is rendered
/// in the child's own device list throughout (transparent by construction).
#[tokio::test]
async fn marked_device_refuses_ward_deletion_until_the_guardian_unmarks() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Two devices on the child's account: their own, and the guardian's
    // enrolled one. Both authenticate as the child — indistinguishable to the
    // nest until the mark exists.
    let own = register_device(&r, st.clone(), child, "kid's phone").await;
    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;

    // Unmarked: the ward deletes their own device freely (no regression).
    dispatch(
        &r,
        st.clone(),
        child,
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: own.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("an unmarked device is deletable by its owner");

    // The guardian marks the enrolled device.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    // Transparency: the child's OWN device list renders the mark.
    let listed = devices_list(&r, st.clone(), child).await;
    let row = listed
        .devices
        .iter()
        .find(|d| d.device_id == enrolled)
        .expect("enrolled device listed to the ward");
    assert!(
        row.guardian_marked,
        "the ward sees which device their guardian enrolled"
    );

    // The refusal — the promise this whole slice exists to keep.
    let err = dispatch(
        &r,
        st.clone(),
        child,
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: enrolled.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward cannot remove the guardian's marked device");
    assert_eq!(err.code, "fauna.sync.guardian_marked");
    assert!(
        db.get_device_for_user(
            &fauna_core::hex32::decode(&enrolled).unwrap(),
            child.as_ref()
        )
        .await
        .unwrap()
        .is_some(),
        "the refusal left the device row intact"
    );

    // The ward cannot unmark it either — a child who could unmark could
    // delete, which would defeat the marker entirely.
    let err = dispatch(
        &r,
        st.clone(),
        child,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, false),
    )
    .await
    .expect_err("ward cannot unmark their guardian's device");
    assert_eq!(err.code, "fauna.family.permission_denied");

    // The guardian's un-enroll path: unmark, then ordinary removal proceeds.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, false),
    )
    .await
    .expect("guardian unmarks");
    dispatch(
        &r,
        st.clone(),
        child,
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: enrolled.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("an unmarked device is deletable again");
}

/// Graduation auto-revokes the guardian's enrolled device
/// (`family-safety.md` § Graduation: "revokes a *marked* guardian device"),
/// leaving the ward's own devices untouched. The revoke runs in the handler
/// **before** the link-drop transaction (rule b) — `CacheDb::graduate`'s
/// fail-closed tripwire would refuse the drop if the handler ever skipped it.
#[tokio::test]
async fn graduation_revokes_the_marked_guardian_device() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let own = register_device(&r, st.clone(), child, "kid's phone").await;
    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian graduates the ward");

    // Oversight is gone, and so is the device that carried it.
    assert!(db.get_guardian_of(&child).await.unwrap().is_none());
    let listed = devices_list(&r, st.clone(), child).await;
    let ids: Vec<&str> = listed
        .devices
        .iter()
        .map(|d| d.device_id.as_str())
        .collect();
    assert!(
        !ids.contains(&enrolled.as_str()),
        "graduation revoked the marked guardian device"
    );
    assert!(
        ids.contains(&own.as_str()),
        "graduation left the ward's own device alone"
    );
}

/// Rule (a): the refusal predicate is `marked AND currently supervised`, so a
/// mark that outlives the link is **inert**. This is what makes the marker
/// crash-safe — no state can strand a now-full account with a device it cannot
/// delete (`nest/common.md` § Client-state recoverability).
///
/// The stale state is forced with raw SQL on purpose: the guardianship guard on
/// the write and the tripwire on `graduate` mean no *supported* path can
/// produce it, which is the point — the predicate's `currently supervised` half
/// is the defence that holds even when something upstream has gone wrong.
#[tokio::test]
async fn a_stale_mark_after_graduation_is_inert() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let device = register_device(&r, st.clone(), child, "parent's tablet").await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &device, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    // Sever the link *behind* the marker's back, leaving the mark stranded —
    // the shape any crash or future path that forgets the revoke would leave.
    {
        let conn = db.conn().await;
        conn.execute(
            "DELETE FROM guardianships WHERE supervised_actor_id = ?1",
            rusqlite::params![child.to_vec()],
        )
        .expect("force-sever the link");
    }

    // The mark is still on the row...
    let listed = devices_list(&r, st.clone(), child).await;
    assert!(
        listed
            .devices
            .iter()
            .find(|d| d.device_id == device)
            .expect("device listed")
            .guardian_marked,
        "the stale mark really is still set — otherwise this proves nothing"
    );

    // ...and yet the now-full account deletes its device: `marked` is true but
    // `currently supervised` is false, so the predicate does not refuse.
    dispatch(
        &r,
        st.clone(),
        child,
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: device.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("a stale mark never strands an unsupervised account's device");
}

/// Re-registering a device id must NOT clear its marker.
///
/// The ward can read the marked device's id straight off their own
/// `devices.list` — transparency guarantees it — and `fauna.sync.register` is a
/// User-called kind scoped to the caller's own actor, so the ward may re-issue
/// it for that exact id. If the device row were *replaced* rather than updated,
/// `guardian_marked` would silently fall back to its column default and the
/// ward could then delete the device: a complete bypass of § Full visibility,
/// reachable in two ordinary calls with no privilege at all.
#[tokio::test]
async fn re_registering_a_marked_device_does_not_clear_the_mark() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    // The ward re-registers the very same device id, as itself.
    dispatch(
        &r,
        st.clone(),
        child,
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: enrolled.clone(),
            label: "totally my own device".to_string(),
            capabilities: "read,write".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("re-registration is allowed (it is how a device refreshes its label)");

    // The mark survived the re-register...
    let listed = devices_list(&r, st.clone(), child).await;
    let row = listed
        .devices
        .iter()
        .find(|d| d.device_id == enrolled)
        .expect("device still listed");
    assert!(
        row.guardian_marked,
        "re-registering must not clear the guardian's marker"
    );
    // Post-S9-flip, a SEALLESS re-register (this raw wire call carries no
    // `label_sealed`) rests NAMELESS — the plaintext column is the scrub and
    // the pair moves together, so the previous seal is dropped rather than
    // left opening to a stale name (file-sync.md § Sealed names & paths, the
    // device plane's write half). The pre-flip form of this assertion pinned
    // `label == "totally my own device"`, which the flip retired.
    assert_eq!(
        row.label, "",
        "a sealless re-register rests nameless — the scrubbed column never \
         carries a user-chosen label"
    );
    assert!(
        row.label_sealed.is_none(),
        "the pair moves together: a sealless re-register drops the prior seal \
         rather than leaving one that opens to the previous name"
    );

    // ...so the refusal still holds.
    let err = dispatch(
        &r,
        st.clone(),
        child,
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: enrolled.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("re-registration is not a route around the marker");
    assert_eq!(err.code, "fauna.sync.guardian_marked");
}

/// What the credential cases share: a supervised ward whose guardian-enrolled
/// device row carries a registered grant and is marked.
struct MarkedCredential {
    db: Arc<CacheDb>,
    st: Arc<AppState>,
    r: RpcRouter,
    /// The ward's identity — every device of the account holds it.
    child: ActorKeypair,
    /// The marked row.
    enrolled: String,
    /// The grant the guardian's device registered on it, and its key's seed.
    grant: fauna_core::encoding::EmbedAsBytes,
    seed: [u8; 32],
}

impl MarkedCredential {
    async fn up() -> Self {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let st = state(db.clone());
        let r = router();
        let admin = admin_actor(&st).await;
        let guardian = guardian_user(&db, "parent").await;
        let child = admit_ward_keyed(&r, st.clone(), admin, guardian, "kid").await;
        let enrolled = register_device(&r, st.clone(), child.actor_id().0, "parent's tablet").await;
        let (grant, seed) = common::fresh_device_grant(&child);
        let fixture = Self {
            db,
            st,
            r,
            child,
            enrolled,
            grant,
            seed,
        };
        fixture
            .register(&fixture.grant)
            .await
            .expect("the guardian's device registers its grant");
        dispatch(
            &fixture.r,
            fixture.st.clone(),
            guardian,
            "fauna.family.device.mark",
            mark_req(fixture.child.actor_id().0, &fixture.enrolled, true),
        )
        .await
        .expect("guardian marks the enrolled device");
        fixture
    }

    /// `fauna.sync.device_grant.register` onto the marked row, over a session
    /// of the ward's account — the guardian's device and the ward's own are
    /// the same caller to the nest.
    async fn register(
        &self,
        grant: &fauna_core::encoding::EmbedAsBytes,
    ) -> Result<Bytes, RpcError> {
        dispatch(
            &self.r,
            self.st.clone(),
            self.child.actor_id().0,
            "fauna.sync.device_grant.register",
            encode(&DeviceGrantRegisterRequest {
                device_id: self.enrolled.clone(),
                authorization: grant.clone(),
                extra: Default::default(),
            }),
        )
        .await
    }

    /// The production `fauna.auth.device_handshake` mint under `seed`'s key.
    async fn mint(&self, seed: &[u8; 32], nonce: &[u8]) -> Option<String> {
        use ed25519_dalek::Signer;
        let signing = ed25519_dalek::SigningKey::from_bytes(seed);
        let device_key = signing.verifying_key().to_bytes();
        let now_ms = fauna_core::data::Timestamp::now_millis();
        let nest_id = self.st.bound_identity();
        let msg = fauna_protocol::auth::device_handshake_signed_message(
            &self.child.actor_id().0,
            &device_key,
            now_ms,
            &nest_id,
            nonce,
        );
        fauna_nest::auth_core::device_auth_core(
            &self.st,
            &self.child.actor_id_hex(),
            &fauna_core::hex32::encode(&device_key),
            now_ms,
            &hex::encode(signing.sign(&msg).to_bytes()),
            nonce,
            &fauna_core::hex32::encode(&nest_id),
        )
        .await
        .ok()
        .map(|mint| mint.token)
    }

    async fn row_is_marked(&self) -> bool {
        devices_list(&self.r, self.st.clone(), self.child.actor_id().0)
            .await
            .devices
            .iter()
            .any(|d| d.device_id == self.enrolled && d.guardian_marked)
    }
}

/// **The marked row's credential can be replaced, and the displaced key
/// registers again** (`family-safety.md` § Full visibility for young children →
/// *The device marker*, the replacement bound). A device grant is signed by the
/// account's seed, which every device of the account holds, so the ward's
/// device can sign one for a key of its own and register it onto the
/// guardian's marked row. No nest check refuses it, by ruling: the nest cannot
/// tell that write from the guardian's own device putting its next principal
/// on its row, and a rule protecting the key the row carries would protect
/// whichever device wrote there first.
///
/// What the write costs is stated here. The row keeps its mark. The nest stops
/// minting for the displaced key, and the bearer that key already minted lives
/// out its hour. Nothing is tombstoned, so the same grant registers again in
/// one call — which is what the guardian's device does unasked — and
/// registering the key the row already carries is a no-op.
///
/// Measured 2026-10-01.
#[tokio::test]
async fn a_replaced_credential_on_the_marked_row_tombstones_nothing_and_registers_again() {
    let f = MarkedCredential::up().await;
    let session = f
        .mint(&f.seed, b"before")
        .await
        .expect("the guardian's device mints under its registered key");

    let (other, other_seed) = common::fresh_device_grant(&f.child);
    f.register(&other)
        .await
        .expect("a root-signed grant for another key registers onto the marked row");
    assert!(
        f.mint(&f.seed, b"displaced").await.is_none(),
        "the nest no longer mints for the displaced key"
    );
    assert!(
        f.mint(&other_seed, b"other").await.is_some(),
        "and mints for the key now on the row"
    );
    assert!(f.row_is_marked().await, "the row keeps its mark");
    assert_eq!(
        f.st.auth.token_store.validate(&session).await,
        Some(f.child.actor_id()),
        "the bearer the displaced key minted is not revoked"
    );

    f.register(&f.grant)
        .await
        .expect("the displaced key is not tombstoned: its grant registers again");
    assert!(
        f.mint(&f.seed, b"back").await.is_some() && f.mint(&other_seed, b"gone").await.is_none(),
        "the row carries the guardian's key again"
    );
    f.register(&f.grant)
        .await
        .expect("registering the key the row already carries is a no-op");
    assert!(f.mint(&f.seed, b"still").await.is_some() && f.row_is_marked().await);
}

/// **A key displaced from the marked row is the account's to revoke.** The
/// revoke refusal protects the key that rides a marked row
/// (`family-safety.md` § Full visibility for young children → *The device
/// marker*). A displaced key rides none, so the session arm tombstones it and
/// ends the bearers it minted: replace, then revoke, is the two-call route to
/// what the refusal stops in one. It is inside the same bound, because the
/// device answers a tombstoned principal as it answers a device deletion — a
/// successor principal, which registers on the same row, the mark kept.
///
/// Measured 2026-10-01.
#[tokio::test]
async fn a_key_displaced_from_the_marked_row_can_be_revoked_and_a_successor_registers() {
    let f = MarkedCredential::up().await;
    let session = f.mint(&f.seed, b"before").await.expect("minted");
    let (other, _) = common::fresh_device_grant(&f.child);
    f.register(&other).await.expect("replaced");

    let device_key = ed25519_dalek::SigningKey::from_bytes(&f.seed)
        .verifying_key()
        .to_bytes();
    dispatch(
        &f.r,
        f.st.clone(),
        f.child.actor_id().0,
        "fauna.sync.device_grant.revoke",
        encode(&DeviceGrantRevokeRequest {
            device_key: fauna_core::hex32::encode(&device_key),
            timestamp_ms: None,
            nonce: None,
            signature: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("the displaced key rides no marked row, so the session arm revokes it");
    assert_eq!(
        f.st.auth.token_store.validate(&session).await,
        None,
        "the bearer the displaced key minted is ended"
    );
    assert!(
        f.db.is_device_grant_revoked(&f.child.actor_id().0, &device_key)
            .await
            .unwrap(),
        "and the key is tombstoned"
    );
    let err = f
        .register(&f.grant)
        .await
        .expect_err("a tombstoned key never registers again");
    assert_eq!(err.code, "fauna.sync.device_grant_revoked");

    let (successor, successor_seed) = common::fresh_device_grant(&f.child);
    f.register(&successor)
        .await
        .expect("the device's successor principal registers on its row");
    assert!(
        f.mint(&successor_seed, b"successor").await.is_some() && f.row_is_marked().await,
        "and mints, on the row that kept its mark"
    );
}

/// Put a renewal grant for a fresh device key on `actor`'s row `device_id` —
/// what the enrollment ceremony's registration leaves there — and return the
/// key. The nest's revoke reads the grant by its key and never opens the
/// envelope, so the envelope bytes are a stand-in.
async fn grant_on(db: &CacheDb, actor: [u8; 32], device_id: &str) -> ed25519_dalek::SigningKey {
    let key = ed25519_dalek::SigningKey::from_bytes(&ActorKeypair::generate().actor_id().0);
    let outcome = db
        .set_sync_device_grant(
            &actor,
            &fauna_core::hex32::decode(device_id).unwrap(),
            &key.verifying_key().to_bytes(),
            b"grant",
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        fauna_nest::db::sync_storage::GrantStoreOutcome::Stored
    ));
    key
}

/// Whether the nest would still mint for `key` under `actor`: it holds the
/// key's grant and no tombstone of it.
async fn mints_for(db: &CacheDb, actor: [u8; 32], key: &ed25519_dalek::SigningKey) -> bool {
    let key = key.verifying_key().to_bytes();
    db.get_sync_device_grant(&actor, &key)
        .await
        .unwrap()
        .is_some()
        && !db.is_device_grant_revoked(&actor, &key).await.unwrap()
}

/// `fauna.sync.device_grant.revoke` naming `key`, on `actor`'s session: the
/// app/user arm when `pop` is false, the self arm (a proof of possession by
/// the key itself) when it is true.
async fn revoke_grant(
    r: &RpcRouter,
    st: Arc<AppState>,
    actor: [u8; 32],
    key: &ed25519_dalek::SigningKey,
    pop: bool,
) -> Result<Bytes, RpcError> {
    use ed25519_dalek::Signer;
    let device_key = key.verifying_key().to_bytes();
    let (timestamp_ms, nonce, signature) = if pop {
        let now_ms = fauna_core::data::Timestamp::now_millis();
        let nonce = ActorKeypair::generate().actor_id().0;
        let msg = fauna_protocol::auth::device_grant_revoke_signed_message(
            &actor,
            &device_key,
            now_ms,
            &nonce,
        );
        (
            Some(now_ms),
            Some(hex::encode(nonce)),
            Some(hex::encode(key.sign(&msg).to_bytes())),
        )
    } else {
        (None, None, None)
    };
    dispatch(
        r,
        st,
        actor,
        "fauna.sync.device_grant.revoke",
        encode(&DeviceGrantRevokeRequest {
            device_key: fauna_core::hex32::encode(&device_key),
            timestamp_ms,
            nonce,
            signature,
            extra: Default::default(),
        }),
    )
    .await
}

/// **The refusal covers the credential** (`family-safety.md` § Full visibility
/// for young children → *The device marker*): the marked row's deletion is
/// refused, and so is the retirement of the grant that row carries when the
/// ward's own session asks for it by key — the app/user arm of
/// `fauna.sync.device_grant.revoke`. Without it the ward ends the guardian's
/// device in one call the deletion's refusal never sees: the grant cleared,
/// the key tombstoned, its sessions closed. The guardian unmarks, and the same
/// request lands.
#[tokio::test]
async fn marked_device_refuses_the_wards_grant_revoke_until_the_guardian_unmarks() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let own = register_device(&r, st.clone(), child, "kid's phone").await;
    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;
    let own_key = grant_on(&db, child, &own).await;
    let enrolled_key = grant_on(&db, child, &enrolled).await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    // An unmarked row's grant retires on the ward's session as before.
    revoke_grant(&r, st.clone(), child, &own_key, false)
        .await
        .expect("an unmarked device's grant is the ward's to retire");
    assert!(!mints_for(&db, child, &own_key).await);

    let err = revoke_grant(&r, st.clone(), child, &enrolled_key, false)
        .await
        .expect_err("the ward's session cannot retire the marked device's grant");
    assert_eq!(err.code, "fauna.sync.guardian_marked");
    assert!(
        mints_for(&db, child, &enrolled_key).await,
        "the refusal left the grant standing: the guardian's device still authenticates"
    );

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, false),
    )
    .await
    .expect("guardian unmarks");
    revoke_grant(&r, st.clone(), child, &enrolled_key, false)
        .await
        .expect("an unmarked device's grant retires again");
    assert!(!mints_for(&db, child, &enrolled_key).await);
}

/// The self arm is untouched: the marked device retiring **its own** grant
/// proves possession of the key, and that is the guardian's device signing
/// itself out — not the ward ending it.
#[tokio::test]
async fn a_marked_device_retires_its_own_grant_by_proof_of_possession() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let enrolled = register_device(&r, st.clone(), child, "parent's tablet").await;
    let enrolled_key = grant_on(&db, child, &enrolled).await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &enrolled, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    revoke_grant(&r, st.clone(), child, &enrolled_key, true)
        .await
        .expect("the key's own signature retires its grant, marked or not");
    assert!(!mints_for(&db, child, &enrolled_key).await);
}

/// Rule (a) on the grant revoke, as on the deletion
/// ([`a_stale_mark_after_graduation_is_inert`]): the predicate is `marked AND
/// currently supervised`, so a mark that outlived its link refuses nothing.
#[tokio::test]
async fn a_stale_mark_never_refuses_a_grant_revoke() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let device = register_device(&r, st.clone(), child, "parent's tablet").await;
    let key = grant_on(&db, child, &device).await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &device, true),
    )
    .await
    .expect("guardian marks the enrolled device");
    {
        let conn = db.conn().await;
        conn.execute(
            "DELETE FROM guardianships WHERE supervised_actor_id = ?1",
            rusqlite::params![child.to_vec()],
        )
        .expect("force-sever the link");
    }

    revoke_grant(&r, st.clone(), child, &key, false)
        .await
        .expect("a stale mark never strands an unsupervised account's grant");
    assert!(!mints_for(&db, child, &key).await);
}

/// The fail-closed tripwire (`CacheDb::graduate`), mirroring the `held == 0`
/// one for mail: if a future caller ever drops the link without revoking the
/// marked devices first, graduation **refuses** rather than silently stranding
/// a guardian's device on a now-full account. The handler's revoke loop is what
/// keeps this unreachable in practice.
#[tokio::test]
async fn graduate_refuses_to_strand_a_marked_device() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let device = register_device(&r, st.clone(), child, "parent's tablet").await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.device.mark",
        mark_req(child, &device, true),
    )
    .await
    .expect("guardian marks the enrolled device");

    // The DB call directly — i.e. skipping the handler's revoke loop.
    let err = db
        .graduate(&child)
        .await
        .expect_err("graduate refuses to strand a marked device");
    assert!(
        err.to_string().contains("revoke them first"),
        "unexpected tripwire message: {err}"
    );
    assert!(
        db.get_guardian_of(&child).await.unwrap().is_some(),
        "the refused graduation left the link intact"
    );
}

/// **The status read must not say "no feature limits" while the gate denies
/// everything** (`family-safety.md` § The trust shape invariant 4, restated at
/// `dynamic-features.md:168`; transparency face).
///
/// An undecodable `features` sub-document used to report as absent here, which
/// matched the enforcement side's skip exactly. Now that an unreadable document
/// **denies** (§ Fail posture — the undecodable-document clause), reporting it as
/// absent would be the silent gate wearing its second costume: a read telling
/// both roles a plane is unrestricted while every operation on it is refused.
/// Neither the ward — who is owed a reason — nor the guardian — who is the only
/// person who can fix it, by re-authoring — would have anything to go on.
#[tokio::test]
async fn an_unreadable_feature_sub_document_reads_as_the_deny_it_enforces() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Stored through the DB writer: the handler validates and re-encodes, so a
    // document this nest cannot read can only ever arrive as an already-stored
    // row (a decoder tightening across an upgrade, or corruption).
    let undecodable = encode(&"not a policy document".to_string()).to_vec();
    db.update_guardian_policy(
        &child[..],
        false,
        "allow",
        true,
        "allow",
        None,
        None,
        None,
        None,
        Some(&undecodable),
    )
    .await
    .unwrap();

    let policy = family_status(&r, st.clone(), child)
        .await
        .policy
        .expect("the ward reads their active policy");
    let features = policy
        .features
        .expect("an unreadable sub-document must not read as 'the guardian set no feature limits'");
    for entry in fauna_core::feature_gate::registry() {
        assert_eq!(
            features
                .get(entry.feature.as_str())
                .map(|p| p.availability.clone()),
            Some(fauna_core::feature_gate::Availability::Deny),
            "{} must read as denied, matching what the gate enforces",
            entry.feature.as_str()
        );
    }

    // The four reach knobs still read — one unreadable sub-document must not
    // cost the ward the rest of the policy (the original skip's good half).
    assert_eq!(policy.unknown_sender_mail, "allow");
    assert!(policy.federation_contact);
}

// ── the transfer consent handshake (family-safety.md § Graduation & transfer) ──

/// One `fauna.family.status` read as `actor`.
async fn family_status(r: &RpcRouter, st: Arc<AppState>, actor: [u8; 32]) -> FamilyStatusReply {
    let reply = dispatch(
        r,
        st,
        actor,
        "fauna.family.status",
        encode(&FamilyStatusRequest::default()),
    )
    .await
    .expect("status ok");
    decode(&reply).unwrap()
}

fn transfer_req(ward: [u8; 32], proposed: [u8; 32]) -> Bytes {
    encode(&FamilyTransferRequest {
        supervised_actor_id: ByteBuf::from(ward.to_vec()),
        new_guardian_actor_id: ByteBuf::from(proposed.to_vec()),
        extra: Default::default(),
    })
}

fn accept_req(ward: [u8; 32]) -> Bytes {
    encode(&FamilyTransferAcceptRequest {
        supervised_actor_id: ByteBuf::from(ward.to_vec()),
        extra: Default::default(),
    })
}

/// The core regression pin: `fauna.family.transfer` no longer re-points
/// the link — it records a proposal only the *proposed guardian's* accept
/// completes, visible to both sides via `status`. Confirmed RED against the
/// pre-handshake handler (the link re-pointed without any consent).
#[tokio::test]
async fn transfer_is_pending_until_the_proposed_guardian_accepts() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let other_parent = guardian_user(&db, "otherparent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Tighten one knob so policy preservation across the handshake is
    // observable.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            policy: ReachPolicy {
                contact_approval: true,
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");

    // The ward cannot transfer their own guardianship.
    let err = dispatch(
        &r,
        st.clone(),
        child,
        "fauna.family.transfer",
        transfer_req(child, other_parent),
    )
    .await
    .expect_err("ward cannot transfer");
    assert_eq!(err.code, "fauna.family.permission_denied");

    // The proposed guardian must be admissible — the ward itself is refused,
    // and so is proposing the current guardian (a no-op).
    let err = dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, child),
    )
    .await
    .expect_err("ward as own guardian refused");
    assert_eq!(err.code, "fauna.family.invalid_params");
    let err = dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, guardian),
    )
    .await
    .expect_err("current guardian as proposed guardian refused");
    assert_eq!(err.code, "fauna.family.invalid_params");

    // The guardian proposes the other parent: the link is UNTOUCHED — no
    // consent, no transfer.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, other_parent),
    )
    .await
    .expect("proposal ok");
    let link = db.get_guardian_of(&child).await.unwrap().unwrap();
    assert_eq!(
        link.guardian_actor_id,
        guardian.to_vec(),
        "the link must not move before the proposed guardian consents"
    );

    // Both sides see the pending state on one status read.
    let s = family_status(&r, st.clone(), guardian).await;
    let pending = s.wards[0]
        .pending_transfer
        .as_ref()
        .expect("initiator sees the pending proposal");
    assert_eq!(pending.proposed_guardian_handle, "otherparent");
    let s = family_status(&r, st.clone(), other_parent).await;
    assert_eq!(s.incoming_transfers.len(), 1, "target sees the prompt");
    assert_eq!(s.incoming_transfers[0].supervised_handle, "kid");
    assert_eq!(s.incoming_transfers[0].guardian_handle, "parent");

    // Nobody the proposal does not name can accept — not even the ward.
    for intruder in [child, admin] {
        let err = dispatch(
            &r,
            st.clone(),
            intruder,
            "fauna.family.transfer.accept",
            accept_req(child),
        )
        .await
        .expect_err("only the proposed guardian can accept");
        assert!(
            err.code == "fauna.family.not_found" || err.code == "fauna.family.invalid_params",
            "unexpected code {}",
            err.code
        );
    }
    let link = db.get_guardian_of(&child).await.unwrap().unwrap();
    assert_eq!(link.guardian_actor_id, guardian.to_vec());

    // The proposed guardian accepts: the link re-points, the policy survives,
    // the pending state clears on both sides.
    dispatch(
        &r,
        st.clone(),
        other_parent,
        "fauna.family.transfer.accept",
        accept_req(child),
    )
    .await
    .expect("accept completes the transfer");
    let link = db.get_guardian_of(&child).await.unwrap().unwrap();
    assert_eq!(link.guardian_actor_id, other_parent.to_vec());
    assert!(
        db.get_guardian_policy(&child)
            .await
            .unwrap()
            .unwrap()
            .contact_approval,
        "policy survives the transfer"
    );
    let s = family_status(&r, st.clone(), other_parent).await;
    assert!(s.incoming_transfers.is_empty());
    assert!(s.wards[0].pending_transfer.is_none());

    // A stale accept replay finds nothing pending.
    let err = dispatch(
        &r,
        st.clone(),
        other_parent,
        "fauna.family.transfer.accept",
        accept_req(child),
    )
    .await
    .expect_err("stale replay refused");
    assert_eq!(err.code, "fauna.family.not_found");

    // The former guardian lost the initiation power; the admin still holds it.
    let err = dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, guardian),
    )
    .await
    .expect_err("former guardian refused");
    assert_eq!(err.code, "fauna.family.permission_denied");
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.family.transfer",
        transfer_req(child, guardian),
    )
    .await
    .expect("admin proposes the transfer back");
    let s = family_status(&r, st, guardian).await;
    assert_eq!(
        s.incoming_transfers.len(),
        1,
        "an admin-initiated proposal still awaits the target's consent"
    );
}

#[tokio::test]
async fn transfer_decline_and_cancel_both_drop_the_proposal() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let other_parent = guardian_user(&db, "otherparent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Decline: the target refuses; the link stands; the prompt clears.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, other_parent),
    )
    .await
    .expect("proposal ok");
    dispatch(
        &r,
        st.clone(),
        other_parent,
        "fauna.family.transfer.decline",
        encode(&FamilyTransferDeclineRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("decline ok");
    let link = db.get_guardian_of(&child).await.unwrap().unwrap();
    assert_eq!(link.guardian_actor_id, guardian.to_vec());
    assert!(
        family_status(&r, st.clone(), other_parent)
            .await
            .incoming_transfers
            .is_empty()
    );

    // Cancel: guardian withdraws; then admin withdraws a fresh one.
    for canceller in [guardian, admin] {
        dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.transfer",
            transfer_req(child, other_parent),
        )
        .await
        .expect("proposal ok");
        dispatch(
            &r,
            st.clone(),
            canceller,
            "fauna.family.transfer.cancel",
            encode(&FamilyTransferCancelRequest {
                supervised_actor_id: ByteBuf::from(child.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("cancel ok");
        assert!(
            family_status(&r, st.clone(), other_parent)
                .await
                .incoming_transfers
                .is_empty()
        );
    }

    // Cancelling with nothing pending is a typed not_found; the ward cannot
    // cancel (no lifecycle power).
    let err = dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer.cancel",
        encode(&FamilyTransferCancelRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("nothing pending");
    assert_eq!(err.code, "fauna.family.not_found");
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, other_parent),
    )
    .await
    .expect("proposal ok");
    let err = dispatch(
        &r,
        st,
        child,
        "fauna.family.transfer.cancel",
        encode(&FamilyTransferCancelRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward cannot cancel");
    assert_eq!(err.code, "fauna.family.permission_denied");
}

#[tokio::test]
async fn a_new_proposal_supersedes_and_a_self_proposal_completes_immediately() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    // "Admin is a user with an extra role" — give the admin its users row so
    // it is guardian-admissible.
    db.create_user_with_handle(&admin, "personal", "boss", None)
        .await
        .unwrap();
    let guardian = guardian_user(&db, "parent").await;
    let other_parent = guardian_user(&db, "otherparent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Supersede: a second proposal replaces the first; the superseded target's
    // accept finds nothing.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, other_parent),
    )
    .await
    .expect("first proposal ok");
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.family.transfer",
        transfer_req(child, admin),
    )
    .await
    .expect("admin self-proposal ok");

    // The self-proposal completed immediately — initiating a transfer to
    // yourself IS consenting (family-safety.md § Graduation & transfer) —
    // and the superseded proposal died with it.
    let link = db.get_guardian_of(&child).await.unwrap().unwrap();
    assert_eq!(
        link.guardian_actor_id,
        admin.to_vec(),
        "a self-proposal needs no second consent step"
    );
    let err = dispatch(
        &r,
        st.clone(),
        other_parent,
        "fauna.family.transfer.accept",
        accept_req(child),
    )
    .await
    .expect_err("superseded target cannot accept");
    assert_eq!(err.code, "fauna.family.not_found");
    assert!(
        family_status(&r, st, other_parent)
            .await
            .incoming_transfers
            .is_empty()
    );
}

#[tokio::test]
async fn accept_revalidates_admissibility_and_graduation_voids_the_proposal() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let other_parent = guardian_user(&db, "otherparent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // The world changed between proposal and accept: the target was suspended.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(child, other_parent),
    )
    .await
    .expect("proposal ok");
    db.suspend_user_now(&other_parent, "suspended by admin", "other")
        .await
        .unwrap();
    let err = dispatch(
        &r,
        st.clone(),
        other_parent,
        "fauna.family.transfer.accept",
        accept_req(child),
    )
    .await
    .expect_err("a suspended target cannot take on guardianship");
    let link = db.get_guardian_of(&child).await.unwrap().unwrap();
    assert_eq!(link.guardian_actor_id, guardian.to_vec());
    // Whichever gate fires first (the dispatch-time suspension strip or the
    // accept-time admissibility check), the transfer must not complete.
    assert_ne!(err.code, "fauna.protocol.internal");

    // Graduation voids the pending proposal — nothing left to transfer.
    let ward2 = admit_ward(&r, st.clone(), admin, guardian, "kid2").await;
    let second_target = guardian_user(&db, "aunt").await;
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.transfer",
        transfer_req(ward2, second_target),
    )
    .await
    .expect("proposal ok");
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(ward2.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("graduate ok");
    let err = dispatch(
        &r,
        st,
        second_target,
        "fauna.family.transfer.accept",
        accept_req(ward2),
    )
    .await
    .expect_err("a graduated account has nothing pending");
    assert_eq!(err.code, "fauna.family.not_found");
}

// ── slice 3a/3c: reach-policy enforcement — contact approval + federation ──

/// Signed `(ContactRequest, Post)` inbox payload, the sender choosing the
/// post's `schema` string. `schema` is a free-form field the sender signs over
/// their *own* post — no third party attests it — so it must never change how
/// the nest routes the arrival.
fn build_inbox_payload_with_schema(
    sender: &ActorKeypair,
    recipient: &[u8; 32],
    schema: &str,
) -> Vec<u8> {
    use fauna_core::data::{ContactRequest, Post, PostBody, StructuredField, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, compute_post_id, sign_envelope};

    let author = sender.actor_id();
    let post = Post {
        author,
        created_at: Timestamp::now(),
        body: PostBody::Structured {
            schema: schema.into(),
            fields: vec![StructuredField {
                key: "to".into(),
                value: hex::encode(recipient),
            }],
            content: Some("hello".into()),
            facets: vec![],
            items: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (post_bytes, post_env) = sign_envelope(sender, &post).unwrap();
    let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);
    let post_id = compute_post_id(&post).unwrap();

    let cr = ContactRequest {
        sender: author,
        post_id,
        sender_node: b"http://localhost:3000".to_vec(),
        summary: "hi".into(),
        created_at: Timestamp::now(),
    };
    let (cr_bytes, cr_env) = sign_envelope(sender, &cr).unwrap();
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);

    canonical_encode(&(&cr_wire, &post_wire)).unwrap()
}

/// `fauna.inbox.send` (local branch) from `sender` to `recipient`.
async fn inbox_send(
    router: &RpcRouter,
    st: Arc<AppState>,
    sender: &ActorKeypair,
    recipient: &[u8; 32],
) -> Result<InboxSendReply, RpcError> {
    inbox_send_with_schema(router, st, sender, recipient, "note/v1").await
}

/// As [`inbox_send`], letting the caller pick the post's self-declared `schema`.
async fn inbox_send_with_schema(
    router: &RpcRouter,
    st: Arc<AppState>,
    sender: &ActorKeypair,
    recipient: &[u8; 32],
    schema: &str,
) -> Result<InboxSendReply, RpcError> {
    let out = dispatch(
        router,
        st,
        sender.actor_id().0,
        "fauna.inbox.send",
        encode(&InboxSendRequest {
            recipient_actor_id: hex::encode(recipient),
            recipient_nest_url: None,
            payload_bytes: build_inbox_payload_with_schema(sender, recipient, schema),
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode::<InboxSendReply>(&out).unwrap())
}

/// A local full account with a keypair (for send/knock flows).
async fn keyed_user(db: &CacheDb, handle: &str) -> ActorKeypair {
    let kp = ActorKeypair::generate();
    db.create_user_with_handle(&kp.actor_id().0, "free", handle, None)
        .await
        .unwrap();
    kp
}

#[tokio::test]
async fn contact_approval_moves_reach_authority_to_guardian() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Tighten: new contact edges need guardian approval.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                contact_approval: true,
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");

    // Even in `open` inbox mode, a stranger's arrival lands on the knock
    // path — the gate binds tighter than the ward's own mode choice.
    db.set_inbox_mode(&ward, "open").await.unwrap();
    let stranger = keyed_user(&db, "stranger").await;
    let reply = inbox_send(&r, st.clone(), &stranger, &ward)
        .await
        .expect("send accepted as knock");
    assert_eq!(reply.inbox_id, None, "stored as knock, not delivered");

    // The ward cannot accept the knock — acceptance authority moved.
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.knocks.accept",
        encode(&KnockActionRequest {
            peer_id: hex::encode(stranger.actor_id().0),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward accept refused");
    assert_eq!(err.code, "fauna.knocks.guardian_approval_required");

    // The guardian sees it in the approvals queue…
    let list: FamilyApprovalsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("approvals list ok"),
    )
    .unwrap();
    assert_eq!(list.approvals.len(), 1);
    let entry = &list.approvals[0];
    assert_eq!(entry.kind, "contact");
    assert_eq!(entry.supervised_handle, "kid");
    assert_eq!(entry.peer_actor_id.as_slice(), stranger.actor_id().0);

    // …approves it, and the stranger's next send delivers.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(stranger.actor_id().0.to_vec()),
            approve: true,
            ..Default::default()
        }),
    )
    .await
    .expect("decide approve ok");
    assert_eq!(
        db.get_contact_status(&ward, &stranger.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("accepted")
    );
    let reply = inbox_send(&r, st.clone(), &stranger, &ward)
        .await
        .expect("send ok");
    assert!(reply.inbox_id.is_some(), "approved contact delivers");

    // Deny path: a second stranger is blocked on the ward's behalf.
    let stranger2 = keyed_user(&db, "stranger2").await;
    inbox_send(&r, st.clone(), &stranger2, &ward)
        .await
        .expect("knock stored");
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(stranger2.actor_id().0.to_vec()),
            approve: false,
            ..Default::default()
        }),
    )
    .await
    .expect("decide deny ok");
    assert_eq!(
        db.get_contact_status(&ward, &stranger2.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("blocked")
    );

    // A non-guardian cannot read the queue as this ward's guardian (empty
    // queue — the list is caller-scoped to the caller's own wards).
    let outsider = guardian_user(&db, "neighbor").await;
    let list: FamilyApprovalsListReply = decode(
        &dispatch(
            &r,
            st,
            outsider,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(list.approvals.is_empty());
}

#[tokio::test]
async fn contact_approval_gates_ward_outbound_and_contact_add_unblocks() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    // Admit the ward with a keypair we control (outbound send needs to sign).
    let code = mint_with_guardian(&r, st.clone(), admin, Some(guardian))
        .await
        .unwrap();
    let ward_kp = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&ward_kp, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("ward admitted");
    let ward = ward_kp.actor_id().0;

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                contact_approval: true,
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");

    // Outbound to a stranger is refused with the ask-your-guardian error.
    let penpal = keyed_user(&db, "penpal").await;
    db.set_inbox_mode(&penpal.actor_id().0, "open")
        .await
        .unwrap();
    let err = inbox_send(&r, st.clone(), &ward_kp, &penpal.actor_id().0)
        .await
        .expect_err("ward outbound to stranger refused");
    assert_eq!(err.code, "fauna.inbox.guardian_approval_required");

    // The guardian pre-approves the contact; the ward's send now flows.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.contact.add",
        encode(&FamilyContactAddRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            peer_actor_id: ByteBuf::from(penpal.actor_id().0.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("contact.add ok");
    let reply = inbox_send(&r, st, &ward_kp, &penpal.actor_id().0)
        .await
        .expect("ward send ok after pre-approval");
    assert!(reply.inbox_id.is_some());
}

// ── slice 4: lifecycle gates ────────────────────────────────────────────────

#[tokio::test]
async fn supervised_self_delete_refused_until_graduation() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Self-delete would unilaterally sever the link — refused typed.
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.account.delete",
        encode(&fauna_protocol::account::AccountDeleteRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("supervised self-delete refused");
    assert_eq!(err.code, "fauna.account.guardian_approval_required");

    // After graduation the (now-full) account can delete.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("graduate ok");
    dispatch(
        &r,
        st,
        ward,
        "fauna.account.delete",
        encode(&fauna_protocol::account::AccountDeleteRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("full account may schedule deletion");
}

#[tokio::test]
async fn guardian_eviction_and_deletion_blocked_while_links_exist() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let other_parent = guardian_user(&db, "otherparent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Admin evict/delete of the guardian: blocked, wards listed.
    let err = dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.evict",
        encode(&fauna_protocol::admin::AdminUserEvictRequest {
            actor_id: ByteBuf::from(guardian.to_vec()),
            reason: "cleanup".into(),
            category: "other".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("guardian evict blocked");
    assert_eq!(err.code, "fauna.admin.guardianships_unresolved");
    let err = dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.delete",
        encode(&fauna_protocol::admin::AdminUserDeleteRequest {
            actor_id: ByteBuf::from(guardian.to_vec()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("guardian delete blocked");
    assert_eq!(err.code, "fauna.admin.guardianships_unresolved");

    // Guardian self-delete: same block.
    let err = dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.account.delete",
        encode(&fauna_protocol::account::AccountDeleteRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("guardian self-delete blocked");
    assert_eq!(err.code, "fauna.account.guardianships_unresolved");

    // Resolution via transfer is a handshake: the admin's proposal alone does
    // NOT resolve the link — eviction stays blocked until the proposed
    // guardian consents (family-safety.md § Lifecycle gates; graduate remains
    // the wait-on-nobody path).
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.family.transfer",
        encode(&FamilyTransferRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            new_guardian_actor_id: ByteBuf::from(other_parent.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("proposal ok");
    let err = dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.users.evict",
        encode(&fauna_protocol::admin::AdminUserEvictRequest {
            actor_id: ByteBuf::from(guardian.to_vec()),
            reason: "cleanup".into(),
            category: "other".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a pending proposal must not unblock eviction");
    assert_eq!(err.code, "fauna.admin.guardianships_unresolved");

    dispatch(
        &r,
        st.clone(),
        other_parent,
        "fauna.family.transfer.accept",
        encode(&FamilyTransferAcceptRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("accept resolves the link");
    dispatch(
        &r,
        st,
        admin,
        "fauna.admin.users.evict",
        encode(&fauna_protocol::admin::AdminUserEvictRequest {
            actor_id: ByteBuf::from(guardian.to_vec()),
            reason: "cleanup".into(),
            category: "other".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("former guardian evictable after the accepted transfer");
}

#[tokio::test]
async fn ward_deletion_cascades_family_rows_and_guardian_finalize_is_fail_safe() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    db.add_mail_allowlist_entry(&ward, "alice@example.com", "outbound")
        .await
        .unwrap();

    // Deleting the WARD works as for any user and cascades link + policy +
    // the mail gate's known-sender set (the account and its mail go together —
    // unlike graduation, where the account survives and holds are released).
    fauna_nest::pending_actions::finalize_user_deletion(&st, &ward)
        .await
        .expect("ward deletion finalizes");
    assert!(db.get_guardian_of(&ward).await.unwrap().is_none());
    assert!(db.get_guardian_policy(&ward).await.unwrap().is_none());
    assert!(!db.is_actor_registered(&ward).await.unwrap());
    assert!(
        !db.is_known_mail_sender(&ward, "alice@example.com")
            .await
            .unwrap()
    );

    // Fail-safe: even if a guardian deletion were scheduled past the handler
    // gates (e.g. a link created between scheduling and execution), the
    // finalizer refuses — a stranded ward is unrepresentable.
    let ward2 = admit_ward(&r, st.clone(), admin, guardian, "kid2").await;
    let err = fauna_nest::pending_actions::finalize_user_deletion(&st, &guardian)
        .await
        .expect_err("guardian finalize refused while links exist");
    assert!(err.to_string().contains("guardianship"), "{err}");
    assert!(db.is_actor_registered(&guardian).await.unwrap());
    assert!(db.get_guardian_of(&ward2).await.unwrap().is_some());
}

// ── slice 3d: feed-sources gate ─────────────────────────────────────────────

#[tokio::test]
async fn feed_sources_block_gates_new_sources_only() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    // A real (stub) provider, so `link` / `add_follow` genuinely reach the gate
    // instead of dying at the empty registry — the gate now sits *after* the
    // read-only provider checks, so an unconfigured nest never reaches it.
    let (stub, performed) = StubBridge::new(true, false);
    let st = state_with_stub(db.clone(), stub);
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // Block new feed sources for the ward.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                feed_sources: "block".into(),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");

    let link_payload = || {
        encode(&fauna_protocol::bridges_ui::LinkRequest {
            bridge_id: "stub".into(),
            mode: "oauth".into(),
            params: fauna_protocol::Value::Null,
            extra: Default::default(),
        })
    };
    let follow_payload = || {
        encode(&fauna_protocol::bridges_ui::AddFollowRequest {
            bridge_id: "stub".into(),
            id: "npub1xyz".into(),
            petname: None,
            extra: None,
            unknown_keys: Default::default(),
        })
    };
    let feed_payload = || {
        encode(&fauna_protocol::bridges_ui::CreateFeedRequest {
            bridge: "bluesky".into(),
            feed_uri: "at://did:plc:x/app.bsky.feed.generator/y".into(),
            name: "Some feed".into(),
            extra: Default::default(),
        })
    };

    // All three source-adding surfaces are refused for the ward.
    for (kind, payload) in [
        ("fauna.bridges.link", link_payload()),
        ("fauna.bridges.add_follow", follow_payload()),
        ("fauna.bridges.feeds.create", feed_payload()),
    ] {
        let err = dispatch(&r, st.clone(), ward, kind, payload)
            .await
            .expect_err(kind);
        assert_eq!(
            err.code, "fauna.bridges.guardian_approval_required",
            "{kind} gated"
        );
    }
    assert!(
        !performed.load(Ordering::SeqCst),
        "no gated operation may reach the provider"
    );

    // With the knob back on `allow`, the gate opens and the link now genuinely
    // SUCCEEDS — an observed pass-through, not a "fails later at the registry"
    // inference. And an unsupervised guardian is never gated: feeds.create (a
    // pure DB write, which never consults the registry) succeeds outright.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy::default(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy relax ok");
    dispatch(&r, st.clone(), ward, "fauna.bridges.link", link_payload())
        .await
        .expect("the relaxed knob lets the link through");
    assert!(
        performed.load(Ordering::SeqCst),
        "the ungated link reaches the provider"
    );
    dispatch(
        &r,
        st,
        guardian,
        "fauna.bridges.feeds.create",
        feed_payload(),
    )
    .await
    .expect("unsupervised caller never gated");
}

// ── the reach pillar binds on EVERY inbox-write path ──
//
// `family-safety.md` § Guardian policy pillar 1 says the reach policy "holds
// even against a non-conforming client … it cannot be bypassed by using a
// different client." The slice-3a/3c gate was correctly written but wrongly
// *placed*: it sat on one delivery path while three sibling paths wrote the same
// inbox. Every pin below drives a path the ward's existing tests never touch.
// Review: family-safety reach-enforcement 3a/3c/3d/4 and suspension fix
// (tracked internally).

/// A supervised account with `contact_approval` on, its guardian, and an
/// unrelated stranger. `inbox_mode = open` throughout, so nothing but the reach
/// floor can be doing the work.
async fn ward_with_contact_approval(
    r: &RpcRouter,
    st: &Arc<AppState>,
    db: &Arc<CacheDb>,
    policy: ReachPolicy,
) -> ([u8; 32], [u8; 32], ActorKeypair) {
    let admin = admin_actor(st).await;
    let guardian = guardian_user(db, "parent").await;
    let ward = admit_ward(r, st.clone(), admin, guardian, "kid").await;
    dispatch(
        r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy,
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");
    db.set_inbox_mode(&ward, "open").await.unwrap();
    let stranger = keyed_user(db, "stranger").await;
    (guardian, ward, stranger)
}

/// Pin (a) — F2. `schema` is a free-form string the *sender* signs over their
/// own post; no third party attests it. Relabelling a post `group/v1` used to
/// return from `deliver_inbox_payload_core` before the family gate ever ran.
#[tokio::test]
async fn group_v1_relabel_does_not_bypass_the_ward_reach_gate() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward, stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            contact_approval: true,
            ..Default::default()
        },
    )
    .await;

    let reply = inbox_send_with_schema(&r, st.clone(), &stranger, &ward, "group/v1")
        .await
        .expect("accepted as a knock, not an error");
    assert_eq!(
        reply.inbox_id, None,
        "a self-declared `group/v1` schema must not deliver past contact_approval"
    );
    assert!(
        db.list_inbox_all(&ward).await.unwrap().is_empty(),
        "nothing reached the ward's inbox"
    );

    // …and the arrival is exactly where the guardian expects it.
    let list: FamilyApprovalsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("approvals list ok"),
    )
    .unwrap();
    assert_eq!(list.approvals.len(), 1, "the relabelled post knocked");
    assert_eq!(
        list.approvals[0].peer_actor_id.as_slice(),
        stranger.actor_id().0
    );
}

/// Pin (b) — F1, inbound same-nest. A DM Welcome is the primitive that *starts*
/// a DM, so `conversations_handlers`' old premise ("the recipient already shares
/// the group/DM with the sender") cannot hold at first contact.
#[tokio::test]
async fn stranger_dm_welcome_to_a_ward_is_refused() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (_guardian, ward, stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            contact_approval: true,
            ..Default::default()
        },
    )
    .await;
    let channel = [0xC1u8; 32];

    let err = dispatch(
        &r,
        st.clone(),
        stranger.actor_id().0,
        "fauna.conversations.welcome.deliver",
        encode(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(ward),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![0xAA; 32],
            kind: WelcomeKind::Dm,
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a stranger's DM Welcome to a ward must be refused");
    assert_eq!(err.code, "fauna.conversations.forbidden");

    assert!(
        db.list_inbox_all(&ward).await.unwrap().is_empty(),
        "no inbox row"
    );
    assert!(
        !db.is_actor_in_channel(&ward, &channel).await.unwrap(),
        "no channel-roster row — the ward was never joined to a stranger's DM"
    );

    // An approved contact's Welcome flows: the knob acts on NEW parties only.
    db.upsert_contact(&ward, &stranger.actor_id().0, "accepted")
        .await
        .unwrap();
    dispatch(
        &r,
        st.clone(),
        stranger.actor_id().0,
        "fauna.conversations.welcome.deliver",
        encode(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(ward),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![0xAA; 32],
            kind: WelcomeKind::Dm,
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("an approved contact's Welcome delivers");
    assert_eq!(db.list_inbox_all(&ward).await.unwrap().len(), 1);
}

/// Pin (c) — F1, inbound cross-nest. `fauna.federation.welcome.deliver` is open
/// to any *unauthenticated* peer nest, and carries no signed sender, so a ward
/// with `federation_contact = off` must be unreachable through it.
#[tokio::test]
async fn federation_welcome_to_a_ward_with_federation_contact_off_is_suppressed() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let fed = fed_router();
    let (_guardian, ward, _stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            federation_contact: false,
            ..Default::default()
        },
    )
    .await;
    let channel = [0xC2u8; 32];

    let err = fed_dispatch(
        &fed,
        st.clone(),
        [0xEEu8; 32],
        "fauna.federation.welcome.deliver",
        encode(&FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(ward),
            channel_id: Some(hex::encode(channel)),
            welcome_bytes: vec![0xAAu8; 32],
            channel_type: Some("dm".into()),
            group_id: None,
            origin_nest_url: Some("https://hostile.example".into()),
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("an unauthenticated peer nest cannot reach a federation-closed ward");
    assert_eq!(err.code, "fauna.federation.forbidden");

    assert!(db.list_inbox_all(&ward).await.unwrap().is_empty());
    assert!(!db.is_actor_in_channel(&ward, &channel).await.unwrap());
}

/// Pin (d) — F1, outbound. The slice-3a outbound gate covered only
/// `fauna.inbox.send`; `welcome.deliver` is `User`-class and was ungated, so a
/// ward could fetch any actor's key package and initiate a DM to a stranger.
#[tokio::test]
async fn ward_outbound_welcome_to_a_non_contact_is_refused() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (_guardian, ward, stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            contact_approval: true,
            ..Default::default()
        },
    )
    .await;
    let channel = [0xC3u8; 32];

    // This test's subject is the WARD-side outbound gate. The stranger's own
    // inbound mode gate (direct-messages.md § Reach policy) would also refuse
    // under the default — guardian approval clears the ward's side, never the
    // stranger's acceptance — so open the stranger's inbox to isolate the
    // outbound half.
    db.set_inbox_mode(&stranger.actor_id().0, "open")
        .await
        .unwrap();

    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.conversations.welcome.deliver",
        encode(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(stranger.actor_id().0),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![0xAA; 32],
            kind: WelcomeKind::Dm,
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a ward may not initiate a DM with a non-contact");
    assert_eq!(err.code, "fauna.conversations.guardian_approval_required");
    assert!(
        db.list_inbox_all(&stranger.actor_id().0)
            .await
            .unwrap()
            .is_empty()
    );

    // The guardian pre-approves the peer; the ward's Welcome then goes out.
    dispatch(
        &r,
        st.clone(),
        _guardian,
        "fauna.family.contact.add",
        encode(&FamilyContactAddRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            peer_actor_id: ByteBuf::from(stranger.actor_id().0.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian pre-approve ok");
    dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.conversations.welcome.deliver",
        encode(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(stranger.actor_id().0),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![0xAA; 32],
            kind: WelcomeKind::Dm,
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("an approved contact is reachable");
    assert_eq!(
        db.list_inbox_all(&stranger.actor_id().0)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// Neither kind grants fresh reach, but each lets the ward
/// erode the guardian's authority over the approvals queue.
#[tokio::test]
async fn ward_cannot_undo_a_guardian_block_or_hide_a_knock() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward, stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            contact_approval: true,
            ..Default::default()
        },
    )
    .await;

    // A stranger knocks; the knock is the guardian's to decide.
    inbox_send(&r, st.clone(), &stranger, &ward)
        .await
        .expect("knock stored");

    // The ward cannot dismiss it out of the guardian's queue.
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.knocks.dismiss",
        encode(&KnockActionRequest {
            peer_id: hex::encode(stranger.actor_id().0),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward dismiss refused");
    assert_eq!(err.code, "fauna.knocks.guardian_approval_required");

    // The guardian denies, which blocks the stranger…
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(stranger.actor_id().0.to_vec()),
            approve: false,
            ..Default::default()
        }),
    )
    .await
    .expect("decide deny ok");

    // …and the ward cannot undo the guardian's block.
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.knocks.unblock",
        encode(&KnockActionRequest {
            peer_id: hex::encode(stranger.actor_id().0),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("ward unblock refused");
    assert_eq!(err.code, "fauna.knocks.guardian_approval_required");
    assert_eq!(
        db.get_contact_status(&ward, &stranger.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("blocked"),
        "the guardian's denial stands"
    );
}

/// A knocked arrival is held, not dropped: approving the sender delivers the
/// very payload that knocked. Without this, the guardian's `approve` would grant
/// reach for the *next* message while silently discarding the one they reviewed.
#[tokio::test]
async fn guardian_approval_releases_the_arrival_that_knocked() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward, stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            contact_approval: true,
            ..Default::default()
        },
    )
    .await;

    let reply = inbox_send(&r, st.clone(), &stranger, &ward)
        .await
        .expect("knock stored");
    assert_eq!(reply.inbox_id, None);
    assert!(db.list_inbox_all(&ward).await.unwrap().is_empty());

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(stranger.actor_id().0.to_vec()),
            approve: true,
            ..Default::default()
        }),
    )
    .await
    .expect("decide approve ok");

    assert_eq!(
        db.list_inbox_all(&ward).await.unwrap().len(),
        1,
        "the approved arrival was released into the ward's inbox"
    );
    // Releasing a held arrival is not a fresh exchange: the edge stays
    // `accepted` (and keeps its TTL) rather than jumping to `confirmed`.
    assert_eq!(
        db.get_contact_status(&ward, &stranger.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("accepted")
    );
}

/// The denial path discards the held arrival — a blocked sender's payload must
/// never surface later.
#[tokio::test]
async fn guardian_denial_discards_the_held_arrival() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward, stranger) = ward_with_contact_approval(
        &r,
        &st,
        &db,
        ReachPolicy {
            contact_approval: true,
            ..Default::default()
        },
    )
    .await;
    inbox_send(&r, st.clone(), &stranger, &ward)
        .await
        .expect("knock stored");

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(stranger.actor_id().0.to_vec()),
            approve: false,
            ..Default::default()
        }),
    )
    .await
    .expect("decide deny ok");

    assert!(
        db.list_inbox_all(&ward).await.unwrap().is_empty(),
        "a denied arrival never reaches the ward"
    );
    assert!(
        db.pending_knock_payload(&ward, &stranger.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "the held payload is gone with the knock"
    );
}

// ── Guardian Notify (family-safety.md § Guardian Notify) ─────────────────────

/// The current local-day bucket (`now / 86_400`), matching the handler's stamp.
fn today_bucket() -> i64 {
    fauna_core::data::Timestamp::now_secs() / 86_400
}

fn notice(category: &str, count: u32) -> FamilyContentNotice {
    FamilyContentNotice {
        category: category.into(),
        count,
        extra: Default::default(),
    }
}

/// Guardian sets `content_notify` to `on` via `fauna.family.policy.update`.
async fn set_notify(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    on: bool,
) {
    dispatch(
        r,
        st,
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                content_notify: Some(on),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy.update ok");
}

/// The ward reports coarse per-category counts.
async fn report(
    r: &RpcRouter,
    st: Arc<AppState>,
    ward: [u8; 32],
    entries: Vec<FamilyContentNotice>,
) -> Result<Bytes, RpcError> {
    dispatch(
        r,
        st,
        ward,
        "fauna.family.notify_report",
        encode(&FamilyNotifyReportRequest {
            entries,
            ..Default::default()
        }),
    )
    .await
}

/// The guardian's view of a ward's `content_notices` (sorted by category).
async fn ward_notices(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
) -> Vec<FamilyContentNotice> {
    let s: FamilyStatusReply = decode(
        &dispatch(
            r,
            st,
            guardian,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("status ok"),
    )
    .unwrap();
    let mut v = s
        .wards
        .iter()
        .find(|w| w.actor_id.as_slice() == ward.as_slice())
        .expect("guardian sees the ward")
        .content_notices
        .clone();
    v.sort_by(|a, b| a.category.cmp(&b.category));
    v
}

/// Count of Guardian-Notify doorbell rows in `guardian`'s feed whose sender is
/// `ward` (`notif_type == "family.content_notice"`).
async fn content_doorbells(db: &CacheDb, guardian: [u8; 32], ward: [u8; 32]) -> usize {
    db.list_notifications(&guardian, None, 100)
        .await
        .unwrap()
        .iter()
        .filter(|n| {
            n.notif_type.as_wire() == "family.content_notice"
                && n.sender_id.as_deref() == Some(&ward)
        })
        .count()
}

#[tokio::test]
async fn notify_report_accumulates_rings_one_doorbell_per_bucket_and_surfaces_via_status() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_notify(&r, st.clone(), guardian, child, true).await;

    // First report: nsfw 2, spam 1 → both accumulate and each rings one doorbell.
    report(
        &r,
        st.clone(),
        child,
        vec![notice("nsfw", 2), notice("spam", 1)],
    )
    .await
    .expect("first report ok");
    assert_eq!(
        ward_notices(&r, st.clone(), guardian, child).await,
        vec![notice("nsfw", 2), notice("spam", 1)],
        "status surfaces the coarse per-category counts to the guardian"
    );
    assert_eq!(
        content_doorbells(&db, guardian, child).await,
        2,
        "one doorbell per (ward, day, category)"
    );

    // Second report the SAME day: nsfw +3 → accumulates to 5; spam untouched; and
    // the day-bucket dedup rings NO new doorbell (the notification is the
    // doorbell, the status read is the truth).
    report(&r, st.clone(), child, vec![notice("nsfw", 3)])
        .await
        .expect("second report ok");
    assert_eq!(
        ward_notices(&r, st.clone(), guardian, child).await,
        vec![notice("nsfw", 5), notice("spam", 1)],
        "same-day counts accumulate"
    );
    assert_eq!(
        content_doorbells(&db, guardian, child).await,
        2,
        "a same-day repeat rings no second doorbell"
    );
}

#[tokio::test]
async fn notify_report_is_a_noop_when_the_knob_is_off() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    // content_notify defaults OFF — never enabled here.

    report(&r, st.clone(), child, vec![notice("nsfw", 4)])
        .await
        .expect("report replies ok even as a no-op");
    assert!(
        ward_notices(&r, st.clone(), guardian, child)
            .await
            .is_empty(),
        "knob off → nothing accumulated"
    );
    assert_eq!(
        content_doorbells(&db, guardian, child).await,
        0,
        "knob off → no doorbell"
    );
}

#[tokio::test]
async fn notify_report_is_a_noop_for_an_unsupervised_caller() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    // A full account with no guardian.
    let outsider = guardian_user(&db, "adult").await;

    dispatch(
        &r,
        st.clone(),
        outsider,
        "fauna.family.notify_report",
        encode(&FamilyNotifyReportRequest {
            entries: vec![notice("nsfw", 3)],
            ..Default::default()
        }),
    )
    .await
    .expect("an unsupervised report is a silent ok no-op");

    assert!(
        db.list_content_notices_for_day(&outsider, today_bucket())
            .await
            .unwrap()
            .is_empty(),
        "no notice row is written for an unsupervised account"
    );
    assert!(
        db.list_notifications(&outsider, None, 100)
            .await
            .unwrap()
            .iter()
            .all(|n| n.notif_type.as_wire() != "family.content_notice"),
        "no self-doorbell"
    );
}

#[tokio::test]
async fn notify_report_drops_unknown_categories_and_clamps_counts() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_notify(&r, st.clone(), guardian, child, true).await;

    // A category outside the four negative canonicals is dropped (additive-safe);
    // an absurd count is clamped to the per-report cap (MAX_NOTIFY_DELTA_PER_REPORT
    // = 100_000 in family_handlers.rs).
    report(
        &r,
        st.clone(),
        child,
        vec![notice("mystery", 5), notice("nsfw", u32::MAX)],
    )
    .await
    .expect("report ok");

    let notices = ward_notices(&r, st.clone(), guardian, child).await;
    assert_eq!(
        notices,
        vec![notice("nsfw", 100_000)],
        "unknown dropped, count clamped"
    );
    assert_eq!(
        content_doorbells(&db, guardian, child).await,
        1,
        "only the recognized category rings a doorbell"
    );
}

#[tokio::test]
async fn graduation_and_deletion_both_drop_content_notices() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    // Graduation drops the notices.
    let grad = admit_ward(&r, st.clone(), admin, guardian, "grad").await;
    set_notify(&r, st.clone(), guardian, grad, true).await;
    report(&r, st.clone(), grad, vec![notice("nsfw", 4)])
        .await
        .expect("report ok");
    let day = today_bucket();
    assert_eq!(
        db.list_content_notices_for_day(&grad, day)
            .await
            .unwrap()
            .len(),
        1,
        "a notice exists before graduation"
    );
    assert!(db.graduate(&grad).await.unwrap(), "graduation succeeds");
    assert!(
        db.list_content_notices_for_day(&grad, day)
            .await
            .unwrap()
            .is_empty(),
        "graduation drops the ward's Guardian-Notify counts"
    );

    // The account-deletion cascade drops them too.
    let del = admit_ward(&r, st.clone(), admin, guardian, "del").await;
    set_notify(&r, st.clone(), guardian, del, true).await;
    report(&r, st.clone(), del, vec![notice("spam", 2)])
        .await
        .expect("report ok");
    assert_eq!(
        db.list_content_notices_for_day(&del, day)
            .await
            .unwrap()
            .len(),
        1
    );
    db.delete_family_rows_for_supervised(&del).await.unwrap();
    assert!(
        db.list_content_notices_for_day(&del, day)
            .await
            .unwrap()
            .is_empty(),
        "the ward-deletion cascade drops the counts"
    );
}

/// Screen-time write validation (`family-safety.md` § Screen time — window
/// semantics + write validation, ratified 2026-07-16): the nest refuses what it
/// cannot honestly store — an out-of-range window bound, a half-set window, the
/// ambiguous empty window (`start == end`), and an over-day budget — while
/// accepting every expressible real window (the wrap case included) and the
/// deliberate `daily_minutes = 0` full lock. A refused write clobbers nothing.
#[tokio::test]
async fn screen_time_ranges_are_validated_at_write() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    let update = |screen: ScreenTimePolicy| {
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(child.to_vec()),
            policy: ReachPolicy {
                screen_time: Some(screen),
                ..Default::default()
            },
            extra: Default::default(),
        })
    };
    let window = |start: u16, end: u16| ScreenTimePolicy {
        window_start: Some(start),
        window_end: Some(end),
        daily_minutes: None,
    };

    // A valid wrapping bedtime window + the deliberate zero budget are stored.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        update(ScreenTimePolicy {
            window_start: Some(1260),
            window_end: Some(420),
            daily_minutes: Some(0),
        }),
    )
    .await
    .expect("a wrapping window and a zero budget are both storable");

    // Every dishonest shape is refused with the typed code.
    for (screen, why) in [
        (window(1440, 420), "window_start out of range"),
        (window(1260, 1440), "window_end out of range"),
        (window(9999, 420), "window_start absurd"),
        (window(600, 600), "start == end is ambiguous — refused"),
        (
            ScreenTimePolicy {
                window_start: Some(600),
                window_end: None,
                daily_minutes: None,
            },
            "half-set window (start only)",
        ),
        (
            ScreenTimePolicy {
                window_start: None,
                window_end: Some(600),
                daily_minutes: None,
            },
            "half-set window (end only)",
        ),
        (
            ScreenTimePolicy {
                window_start: None,
                window_end: None,
                daily_minutes: Some(1441),
            },
            "daily_minutes over a day",
        ),
        (
            ScreenTimePolicy {
                window_start: None,
                window_end: None,
                daily_minutes: Some(65535),
            },
            "daily_minutes absurd",
        ),
    ] {
        let err = dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.policy.update",
            update(screen),
        )
        .await
        .expect_err(why);
        assert_eq!(err.code, "fauna.family.invalid_params", "{why}");
    }

    // The refusals clobbered nothing: the stored pillar is still the valid one.
    let row = db.get_guardian_policy(&child).await.unwrap().unwrap();
    let stored = row.screen_time().expect("pillar still set");
    assert_eq!(stored.window_start, Some(1260));
    assert_eq!(stored.window_end, Some(420));
    assert_eq!(stored.daily_minutes, Some(0));
}

// ── Slice E2: screen-time budget accounting (`fauna.family.usage_report`) ──

/// Guardian sets (or clears) the daily screen-time budget via `policy.update`.
async fn set_budget(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    daily_minutes: Option<u16>,
) {
    dispatch(
        r,
        st,
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                screen_time: Some(ScreenTimePolicy {
                    window_start: None,
                    window_end: None,
                    daily_minutes,
                }),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy.update ok");
}

/// The ward heartbeats `minutes` of foreground use at `utc_offset_minutes`.
async fn usage(
    r: &RpcRouter,
    st: Arc<AppState>,
    ward: [u8; 32],
    minutes: u32,
    utc_offset_minutes: i32,
) -> Result<FamilyUsageReportReply, RpcError> {
    let bytes = dispatch(
        r,
        st,
        ward,
        "fauna.family.usage_report",
        encode(&FamilyUsageReportRequest {
            minutes,
            utc_offset_minutes,
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode(&bytes).expect("usage reply decodes"))
}

/// The status pair `(guardian's ward-row usage, ward's own usage)`.
async fn usage_via_status(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
) -> (Option<u32>, Option<u32>) {
    let status = |actor: [u8; 32]| {
        let st = st.clone();
        async move {
            decode::<FamilyStatusReply>(
                &dispatch(
                    r,
                    st,
                    actor,
                    "fauna.family.status",
                    encode(&FamilyStatusRequest {
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("status ok"),
            )
            .unwrap()
        }
    };
    let g = status(guardian).await;
    let ward_row = g
        .wards
        .iter()
        .find(|w| w.actor_id.as_slice() == ward.as_slice())
        .expect("guardian sees the ward")
        .usage_today_minutes;
    let own = status(ward).await.usage_today_minutes;
    (ward_row, own)
}

/// The heartbeat accumulates a cross-device day total, the reply carries it
/// (a zero-minute report is a read), and `status` surfaces the same number to
/// both roles — the guardian's ward row and the ward's own summary
/// (`family-safety.md` § Screen time: transparency — the ward sees the same
/// number the guardian sees). With no budget set there is no accounting at
/// all: `None` on both surfaces, nothing stored.
#[tokio::test]
async fn usage_report_accumulates_and_surfaces_the_same_total_to_both_roles() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;

    // No budget → no accounting, and status shows None to both roles.
    let reply = usage(&r, st.clone(), child, 30, 0).await.expect("ok");
    assert_eq!(
        reply.day_total_minutes, 0,
        "budgetless report credits nothing"
    );
    assert_eq!(
        usage_via_status(&r, st.clone(), guardian, child).await,
        (None, None)
    );

    // Budget set: two "devices" heartbeat; the total accumulates across them.
    set_budget(&r, st.clone(), guardian, child, Some(120)).await;
    let first = usage(&r, st.clone(), child, 30, 0).await.expect("ok");
    assert_eq!(first.day_total_minutes, 30);
    let second = usage(&r, st.clone(), child, 45, 0).await.expect("ok");
    assert_eq!(second.day_total_minutes, 75, "cross-device accumulate");
    assert_eq!(second.day, first.day, "same local day, same bucket");

    // A zero-minute report is a read — the unlock-screen check.
    let read = usage(&r, st.clone(), child, 0, 0).await.expect("ok");
    assert_eq!(
        read.day_total_minutes, 75,
        "zero-minute report credits nothing"
    );

    // Both roles read the same truth.
    assert_eq!(
        usage_via_status(&r, st.clone(), guardian, child).await,
        (Some(75), Some(75))
    );
}

/// Every gate exits with the same reply shape (best-effort telemetry): an
/// unsupervised caller and a budgetless ward both get a zero total, and
/// neither leaves a row behind.
#[tokio::test]
async fn usage_report_is_a_silent_zero_for_unsupervised_callers() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let adult = guardian_user(&db, "adult").await;

    let reply = usage(&r, st.clone(), adult, 60, 0).await.expect("ok");
    assert_eq!(reply.day_total_minutes, 0);
    assert_eq!(
        db.get_guardian_usage(&adult, reply.day).await.unwrap(),
        0,
        "no adult's account ever accrues a usage row"
    );
}

/// The per-report minutes delta and the UTC offset are both clamped, and the
/// day bucket is the *reported local day*: the two offset extremes always land
/// a simultaneous report in different buckets, each with its own total.
#[tokio::test]
async fn usage_report_clamps_inputs_and_buckets_by_reported_local_day() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_budget(&r, st.clone(), guardian, child, Some(120)).await;

    // An absurd minutes delta is clamped per report (1440 — one day).
    let clamped = usage(&r, st.clone(), child, 999_999, -720)
        .await
        .expect("ok");
    assert_eq!(
        clamped.day_total_minutes, 1440,
        "per-report delta clamps to a day"
    );

    // UTC-12 vs UTC+14 are ≥ 26 h apart — they NEVER share a local day, so the
    // two extremes bucket separately with independent totals.
    let east = usage(&r, st.clone(), child, 10, 840).await.expect("ok");
    assert_ne!(
        east.day, clamped.day,
        "offset extremes never share a bucket"
    );
    assert_eq!(
        east.day_total_minutes, 10,
        "each bucket totals independently"
    );

    // An absurd offset clamps to the nearest extreme (840): same bucket, and
    // the accumulate proves it.
    let absurd = usage(&r, st.clone(), child, 5, 100_000).await.expect("ok");
    assert_eq!(absurd.day, east.day, "offset clamps to +840");
    assert_eq!(absurd.day_total_minutes, 15);
}

/// Guardian Notify uses the same local-day bucket rule (`family-safety.md`
/// § Screen time, adopted by § Guardian Notify): a report's notices land in
/// the reported local day, the stored link offset drives the guardian's
/// "today" status read, and the doorbell dedup keys on the local-day bucket.
#[tokio::test]
async fn notify_report_buckets_by_the_reported_local_day() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let child = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_notify(&r, st.clone(), guardian, child, true).await;

    // Report at UTC+14: the notice lands in that local day, and status (which
    // derives "today" from the stored offset) surfaces it.
    dispatch(
        &r,
        st.clone(),
        child,
        "fauna.family.notify_report",
        encode(&FamilyNotifyReportRequest {
            entries: vec![notice("spam", 3)],
            utc_offset_minutes: 840,
            extra: Default::default(),
        }),
    )
    .await
    .expect("report ok");
    assert_eq!(
        ward_notices(&r, st.clone(), guardian, child).await,
        vec![notice("spam", 3)]
    );
    assert_eq!(content_doorbells(&db, guardian, child).await, 1);

    // A later report at the other extreme is a DIFFERENT local day: its bucket
    // starts fresh, the stored offset moves, and status now reads the new
    // day's counts (the +840 bucket is no longer "today"). The new bucket
    // rings its own doorbell — one per (ward, day, category), ≤ 2 buckets per
    // real instant by the clamp.
    dispatch(
        &r,
        st.clone(),
        child,
        "fauna.family.notify_report",
        encode(&FamilyNotifyReportRequest {
            entries: vec![notice("spam", 1)],
            utc_offset_minutes: -720,
            extra: Default::default(),
        }),
    )
    .await
    .expect("report ok");
    assert_eq!(
        ward_notices(&r, st.clone(), guardian, child).await,
        vec![notice("spam", 1)],
        "status follows the stored offset to the new local day"
    );
    assert_eq!(content_doorbells(&db, guardian, child).await, 2);
}

/// Graduation and the ward-deletion cascade both drop the usage rows — the
/// same lifecycle as every family side table.
#[tokio::test]
async fn graduation_and_deletion_drop_usage_rows() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let grad = admit_ward(&r, st.clone(), admin, guardian, "grad").await;
    set_budget(&r, st.clone(), guardian, grad, Some(60)).await;
    let day = usage(&r, st.clone(), grad, 30, 0).await.expect("ok").day;
    assert_eq!(db.get_guardian_usage(&grad, day).await.unwrap(), 30);
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(grad.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("graduate ok");
    assert_eq!(
        db.get_guardian_usage(&grad, day).await.unwrap(),
        0,
        "graduation drops the usage accounting"
    );

    let del = admit_ward(&r, st.clone(), admin, guardian, "del2").await;
    set_budget(&r, st.clone(), guardian, del, Some(60)).await;
    let day = usage(&r, st.clone(), del, 25, 0).await.expect("ok").day;
    assert_eq!(db.get_guardian_usage(&del, day).await.unwrap(), 25);
    db.delete_family_rows_for_supervised(&del).await.unwrap();
    assert_eq!(
        db.get_guardian_usage(&del, day).await.unwrap(),
        0,
        "the ward-deletion cascade drops the usage accounting"
    );
}

// ── v1.x: child-initiated contact requests ──────────────────────────────────
// family-safety.md § Child-initiated contact requests (design firmed
// 2026-07-16): the ward's in-app ask lands in the guardian's queue as kind
// `contact_request`; approve mints the same accepted edge `contact.add`
// would; deny drops the ask and deliberately does NOT block the named peer.

async fn set_contact_approval(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    on: bool,
) {
    dispatch(
        r,
        st,
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                contact_approval: on,
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");
}

async fn ask_contact(
    r: &RpcRouter,
    st: Arc<AppState>,
    ward: [u8; 32],
    peer: [u8; 32],
) -> Result<Bytes, RpcError> {
    dispatch(
        r,
        st,
        ward,
        "fauna.family.contact.request",
        encode(&FamilyContactRequestRequest {
            peer_actor_id: ByteBuf::from(peer.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
}

async fn approvals(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
) -> FamilyApprovalsListReply {
    decode(
        &dispatch(
            r,
            st,
            guardian,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("approvals.list ok"),
    )
    .unwrap()
}

async fn decide_contact_request(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    peer: [u8; 32],
    approve: bool,
) -> Result<Bytes, RpcError> {
    dispatch(
        r,
        st,
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "contact_request".into(),
            peer_actor_id: ByteBuf::from(peer.to_vec()),
            message_id: ByteBuf::from(Vec::new()),
            approve,
            ..Default::default()
        }),
    )
    .await
}

async fn request_doorbells(db: &CacheDb, guardian: [u8; 32], ward: [u8; 32]) -> usize {
    db.list_notifications(&guardian, None, 100)
        .await
        .unwrap()
        .iter()
        .filter(|n| {
            n.notif_type.as_wire() == "family.contact_request"
                && n.sender_id.as_deref() == Some(&ward)
        })
        .count()
}

#[tokio::test]
async fn contact_request_journey_approve_mints_the_edge() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let code = mint_with_guardian(&r, st.clone(), admin, Some(guardian))
        .await
        .unwrap();
    let ward_kp = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&ward_kp, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("ward admitted");
    let ward = ward_kp.actor_id().0;
    set_contact_approval(&r, st.clone(), guardian, ward, true).await;

    let penpal = keyed_user(&db, "penpal").await;
    let penpal_id = penpal.actor_id().0;
    db.set_inbox_mode(&penpal_id, "open").await.unwrap();

    // The ward's outbound is refused (the affordance's trigger)…
    let err = inbox_send(&r, st.clone(), &ward_kp, &penpal_id)
        .await
        .expect_err("ward outbound refused");
    assert_eq!(err.code, "fauna.inbox.guardian_approval_required");

    // …so the ward asks. One doorbell rings; the queue and the ward's own
    // status both show the pending ask.
    ask_contact(&r, st.clone(), ward, penpal_id)
        .await
        .expect("ask ok");
    assert_eq!(request_doorbells(&db, guardian, ward).await, 1);
    let q = approvals(&r, st.clone(), guardian).await;
    let entry = q
        .approvals
        .iter()
        .find(|e| e.kind == "contact_request")
        .expect("contact_request entry present");
    assert_eq!(entry.supervised_actor_id.as_slice(), ward.as_slice());
    assert_eq!(entry.peer_actor_id.as_slice(), penpal_id.as_slice());
    assert_eq!(entry.peer_handle, "penpal");
    assert!(entry.summary.is_empty(), "an ask carries no message text");
    let ward_status = family_status(&r, st.clone(), ward).await;
    assert_eq!(ward_status.contact_requests.len(), 1);
    assert_eq!(
        ward_status.contact_requests[0].peer_actor_id.as_slice(),
        penpal_id.as_slice()
    );
    assert_eq!(ward_status.contact_requests[0].peer_handle, "penpal");

    // A re-ask while pending is a quiet no-op: no second row, no re-ring.
    ask_contact(&r, st.clone(), ward, penpal_id)
        .await
        .expect("re-ask ok");
    assert_eq!(request_doorbells(&db, guardian, ward).await, 1);
    assert_eq!(approvals(&r, st.clone(), guardian).await.approvals.len(), 1);

    // Approve: the edge is accepted, the ask is gone everywhere, the send flows.
    decide_contact_request(&r, st.clone(), guardian, ward, penpal_id, true)
        .await
        .expect("approve ok");
    assert!(
        approvals(&r, st.clone(), guardian)
            .await
            .approvals
            .is_empty()
    );
    assert!(
        family_status(&r, st.clone(), ward)
            .await
            .contact_requests
            .is_empty()
    );
    let reply = inbox_send(&r, st.clone(), &ward_kp, &penpal_id)
        .await
        .expect("ward send flows after approval");
    assert!(reply.inbox_id.is_some());

    // An ask for an existing contact is a quiet ok with nothing to approve.
    ask_contact(&r, st.clone(), ward, penpal_id)
        .await
        .expect("ask for existing contact ok");
    assert!(approvals(&r, st, guardian).await.approvals.is_empty());
}

#[tokio::test]
async fn contact_request_deny_drops_the_ask_without_blocking() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_contact_approval(&r, st.clone(), guardian, ward, true).await;
    let grandma = keyed_user(&db, "grandma").await.actor_id().0;

    ask_contact(&r, st.clone(), ward, grandma)
        .await
        .expect("ask ok");
    decide_contact_request(&r, st.clone(), guardian, ward, grandma, false)
        .await
        .expect("deny ok");

    // The ask is gone — and the peer is NOT blocked (a request-deny refuses
    // the child's question, never punishes the named third party).
    assert!(
        approvals(&r, st.clone(), guardian)
            .await
            .approvals
            .is_empty()
    );
    assert_eq!(db.get_contact_status(&ward, &grandma).await.unwrap(), None);

    // The child may ask again; the new ask rings again.
    ask_contact(&r, st.clone(), ward, grandma)
        .await
        .expect("re-ask after deny ok");
    assert_eq!(request_doorbells(&db, guardian, ward).await, 2);
    assert_eq!(approvals(&r, st.clone(), guardian).await.approvals.len(), 1);

    // Deciding a request that does not exist is not_found.
    let stranger = keyed_user(&db, "stranger").await.actor_id().0;
    let err = decide_contact_request(&r, st, guardian, ward, stranger, true)
        .await
        .expect_err("no such ask");
    assert_eq!(err.code, "fauna.family.not_found");
}

#[tokio::test]
async fn contact_request_refusals() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    let peer = keyed_user(&db, "peer").await.actor_id().0;

    // With contact_approval OFF the ward contacts freely — nothing to ask.
    let err = ask_contact(&r, st.clone(), ward, peer)
        .await
        .expect_err("knob off refused");
    assert_eq!(err.code, "fauna.family.invalid_params");

    set_contact_approval(&r, st.clone(), guardian, ward, true).await;

    // An unsupervised caller has no guardian to ask.
    let outsider = keyed_user(&db, "outsider").await.actor_id().0;
    let err = ask_contact(&r, st.clone(), outsider, peer)
        .await
        .expect_err("unsupervised refused");
    assert_eq!(err.code, "fauna.family.permission_denied");

    // Asking for yourself is malformed.
    let err = ask_contact(&r, st.clone(), ward, ward)
        .await
        .expect_err("self refused");
    assert_eq!(err.code, "fauna.family.invalid_params");

    // A guardian-blocked peer is refused (the block is already visible on the
    // ward's own contact list — no new disclosure).
    db.block_contact(&ward, &peer).await.unwrap();
    let err = ask_contact(&r, st.clone(), ward, peer)
        .await
        .expect_err("blocked peer refused");
    assert_eq!(err.code, "fauna.family.permission_denied");
    assert!(approvals(&r, st, guardian).await.approvals.is_empty());
}

#[tokio::test]
async fn contact_request_pending_cap_bounds_the_queue() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_contact_approval(&r, st.clone(), guardian, ward, true).await;

    for i in 0..32u8 {
        let mut peer = [7u8; 32];
        peer[0] = i;
        ask_contact(&r, st.clone(), ward, peer)
            .await
            .expect("ask under the cap ok");
    }
    let err = ask_contact(&r, st.clone(), ward, [8u8; 32])
        .await
        .expect_err("the 33rd pending ask is refused");
    assert_eq!(err.code, "fauna.family.invalid_params");
    assert_eq!(approvals(&r, st, guardian).await.approvals.len(), 32);
}

#[tokio::test]
async fn graduation_and_deletion_drop_contact_requests() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;
    let ward = admit_ward(&r, st.clone(), admin, guardian, "kid").await;
    set_contact_approval(&r, st.clone(), guardian, ward, true).await;
    let peer = keyed_user(&db, "peer").await.actor_id().0;
    ask_contact(&r, st.clone(), ward, peer)
        .await
        .expect("ask ok");

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("graduate ok");
    assert!(
        approvals(&r, st.clone(), guardian)
            .await
            .approvals
            .is_empty(),
        "graduation drops the pending ask"
    );

    // Deletion cascade: a second ward with a pending ask, then delete the rows.
    let ward2 = admit_ward(&r, st.clone(), admin, guardian, "kid2").await;
    set_contact_approval(&r, st.clone(), guardian, ward2, true).await;
    ask_contact(&r, st.clone(), ward2, peer)
        .await
        .expect("ask ok");
    db.delete_family_rows_for_supervised(&ward2).await.unwrap();
    assert!(
        approvals(&r, st, guardian).await.approvals.is_empty(),
        "the ward-deletion cascade drops the pending ask"
    );
}

// ── v1.x: feed-source approvals — the grant flow ────────────────────────────
// family-safety.md § Feed-source approvals (design firmed 2026-07-16): the
// ward's ask lands in the guardian's queue as kind `feed_source`; approve mints
// a SINGLE-USE 7-day grant rather than replaying the operation (a bridge link is
// interactive OAuth), which the ward redeems by simply retrying.
//
// `feeds.create` is the journey's operation of choice because it is a pure DB
// write that SUCCEEDS outright once ungated (`for_test` configures no bridge
// providers, so `link`/`add_follow` can only ever fail later at the registry).
// That makes "the retry passed" an observable success rather than an inference
// from a different error code.

async fn set_feed_sources(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    value: &str,
) {
    dispatch(
        r,
        st,
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                feed_sources: value.into(),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("policy update ok");
}

async fn ask_feed_source(
    r: &RpcRouter,
    st: Arc<AppState>,
    ward: [u8; 32],
    bridge_id: &str,
    operation: &str,
    target: &str,
    label: &str,
) -> Result<Bytes, RpcError> {
    dispatch(
        r,
        st,
        ward,
        "fauna.family.feed_source.request",
        encode(&FamilyFeedSourceRequestRequest {
            bridge_id: bridge_id.into(),
            operation: operation.into(),
            target: target.into(),
            label: label.into(),
            extra: Default::default(),
        }),
    )
    .await
}

// One arg per FamilyApprovalDecideRequest field this test helper fills; a struct
// wrapper would just move the 8 fields one level out for a single call site.
#[allow(clippy::too_many_arguments)]
async fn decide_feed_source(
    r: &RpcRouter,
    st: Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    bridge_id: &str,
    operation: &str,
    target: &str,
    approve: bool,
) -> Result<Bytes, RpcError> {
    dispatch(
        r,
        st,
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "feed_source".into(),
            bridge_id: bridge_id.into(),
            operation: operation.into(),
            target: target.into(),
            approve,
            ..Default::default()
        }),
    )
    .await
}

fn feed_payload(bridge: &str, uri: &str) -> Bytes {
    encode(&fauna_protocol::bridges_ui::CreateFeedRequest {
        bridge: bridge.into(),
        feed_uri: uri.into(),
        name: "Science feed".into(),
        extra: Default::default(),
    })
}

async fn feed_doorbells(db: &CacheDb, actor: [u8; 32], notif_type: &str) -> usize {
    db.list_notifications(&actor, None, 100)
        .await
        .unwrap()
        .iter()
        .filter(|n| n.notif_type.as_wire() == notif_type)
        .count()
}

async fn ward_status(r: &RpcRouter, st: Arc<AppState>, ward: [u8; 32]) -> FamilyStatusReply {
    decode(
        &dispatch(
            r,
            st,
            ward,
            "fauna.family.status",
            encode(&FamilyStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("ward status ok"),
    )
    .unwrap()
}

/// A supervised ward + their guardian, with `feed_sources = block` already set.
async fn blocked_ward(
    db: &Arc<CacheDb>,
    st: &Arc<AppState>,
    r: &RpcRouter,
) -> ([u8; 32], [u8; 32]) {
    let admin = admin_actor(st).await;
    let guardian = guardian_user(db, "parent").await;
    let code = mint_with_guardian(r, st.clone(), admin, Some(guardian))
        .await
        .unwrap();
    let ward_kp = ActorKeypair::generate();
    dispatch(
        r,
        st.clone(),
        [0u8; 32],
        "fauna.account.register",
        common::register_payload(&ward_kp, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("ward admitted");
    let ward = ward_kp.actor_id().0;
    set_feed_sources(r, st.clone(), guardian, ward, "block").await;
    (guardian, ward)
}

#[tokio::test]
async fn feed_source_journey_approve_mints_a_single_use_grant() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;
    let uri = "at://did:plc:x/app.bsky.feed.generator/science";

    // The ward's feeds.create is refused (the affordance's trigger)…
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", uri),
    )
    .await
    .expect_err("ward feeds.create refused");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // …so the ward asks. One doorbell rings the GUARDIAN; the queue and the
    // ward's own status both show the pending ask.
    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", uri, "Science feed")
        .await
        .expect("ask ok");
    assert_eq!(
        feed_doorbells(&db, guardian, "family.feed_source_request").await,
        1
    );
    let q = approvals(&r, st.clone(), guardian).await;
    let entry = q
        .approvals
        .iter()
        .find(|e| e.kind == "feed_source")
        .expect("feed_source entry present");
    assert_eq!(entry.supervised_actor_id.as_slice(), ward.as_slice());
    assert_eq!(entry.bridge_id, "bluesky");
    assert_eq!(entry.operation, "feed");
    assert_eq!(entry.target, uri);
    assert_eq!(entry.summary, "Science feed", "the label rides summary");
    assert!(
        entry.peer_actor_id.is_empty(),
        "a bridge object has no actor"
    );

    let s = ward_status(&r, st.clone(), ward).await;
    assert_eq!(s.feed_requests.len(), 1);
    assert_eq!(s.feed_requests[0].target, uri);
    assert!(
        s.feed_requests[0].approved_at.is_none(),
        "the ward sees it as pending"
    );

    // A re-ask while pending is a quiet no-op that never re-rings.
    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", uri, "Science feed")
        .await
        .expect("re-ask is a quiet ok");
    assert_eq!(
        feed_doorbells(&db, guardian, "family.feed_source_request").await,
        1,
        "a re-ask while pending must not re-ring"
    );

    // The guardian approves: a grant is minted and the WARD is rung — a grant
    // produces no organically observable state, so without the doorbell the
    // ward could only poll to learn they may retry.
    decide_feed_source(&r, st.clone(), guardian, ward, "bluesky", "feed", uri, true)
        .await
        .expect("approve ok");
    assert_eq!(
        feed_doorbells(&db, ward, "family.feed_source_approved").await,
        1,
        "approve rings the ward"
    );

    // Approving does NOT perform the operation — the nest never replays it.
    let feeds: fauna_protocol::bridges_ui::ListFeedsReply = decode(
        &dispatch(
            &r,
            st.clone(),
            ward,
            "fauna.bridges.feeds.list",
            encode(&fauna_protocol::bridges_ui::ListFeedsRequest {}),
        )
        .await
        .expect("feeds.list ok"),
    )
    .unwrap();
    assert!(
        feeds.subscriptions.is_empty(),
        "approve mints a grant; it must not subscribe the feed itself"
    );

    // The granted row leaves the guardian's queue (it awaits the ward now) but
    // stays on the ward's status as approved.
    assert!(
        approvals(&r, st.clone(), guardian)
            .await
            .approvals
            .iter()
            .all(|e| e.kind != "feed_source"),
        "a granted row no longer awaits the guardian"
    );
    let s = ward_status(&r, st.clone(), ward).await;
    assert_eq!(s.feed_requests.len(), 1);
    assert!(
        s.feed_requests[0].approved_at.is_some(),
        "the ward sees it as approved"
    );

    // The ward retries — and it now SUCCEEDS, the knob still on `block`.
    dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", uri),
    )
    .await
    .expect("the retry redeems the grant and succeeds");

    // Single-use: the grant is spent, so a second retry is refused again.
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", uri),
    )
    .await
    .expect_err("a spent grant must not unlock a second operation");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // Spending clears it from the ward's status too — nothing dangles.
    assert!(
        ward_status(&r, st, ward).await.feed_requests.is_empty(),
        "a redeemed grant leaves no live row"
    );
}

/// A grant unlocks the **one** object the guardian saw: the key is the whole
/// `(bridge_id, operation, target)` triple, never the display label.
#[tokio::test]
async fn a_grant_unlocks_only_its_own_object() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;
    let approved = "at://did:plc:x/app.bsky.feed.generator/approved";
    let other = "at://did:plc:x/app.bsky.feed.generator/other";

    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", approved, "OK")
        .await
        .expect("ask ok");
    decide_feed_source(
        &r,
        st.clone(),
        guardian,
        ward,
        "bluesky",
        "feed",
        approved,
        true,
    )
    .await
    .expect("approve ok");

    // A different feed on the same bridge is still refused…
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", other),
    )
    .await
    .expect_err("a grant must not unlock a different feed");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // …and so is the same feed URI on a different bridge.
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("nostr", approved),
    )
    .await
    .expect_err("a grant must not unlock a different bridge");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // The refusals above must not have burned the grant: the approved object
    // still redeems.
    dispatch(
        &r,
        st,
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", approved),
    )
    .await
    .expect("the approved object still redeems");
}

/// A `link` ask carries an empty target — approving a link approves connecting
/// that bridge, and the OAuth mode is mechanism, not scope.
#[tokio::test]
async fn a_link_grant_carries_an_empty_target() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (stub, performed) = StubBridge::new(true, false);
    let st = state_with_stub(db.clone(), stub);
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;

    let link_payload = || {
        encode(&fauna_protocol::bridges_ui::LinkRequest {
            bridge_id: "stub".into(),
            mode: "oauth".into(),
            params: fauna_protocol::Value::Null,
            extra: Default::default(),
        })
    };

    let err = dispatch(&r, st.clone(), ward, "fauna.bridges.link", link_payload())
        .await
        .expect_err("ward link refused");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");
    assert!(
        !performed.load(Ordering::SeqCst),
        "the refusal must precede the link itself"
    );

    // A link ask with a stray target is refused — it would mint a grant keyed
    // on something the redeeming gate never passes (an approval that silently
    // changes nothing).
    let err = ask_feed_source(&r, st.clone(), ward, "stub", "link", "stray", "")
        .await
        .expect_err("a link ask carries no target");
    assert_eq!(err.code, "fauna.family.invalid_params");

    ask_feed_source(&r, st.clone(), ward, "stub", "link", "", "Stub")
        .await
        .expect("ask ok");
    decide_feed_source(&r, st.clone(), guardian, ward, "stub", "link", "", true)
        .await
        .expect("approve ok");

    // Redeemed — and with a provider actually registered this is an OBSERVED
    // success, not a "fails later at the registry" inference: the empty-target
    // grant unlocked the link and the provider ran.
    dispatch(&r, st.clone(), ward, "fauna.bridges.link", link_payload())
        .await
        .expect("the redeemed link succeeds");
    assert!(performed.load(Ordering::SeqCst), "the link actually ran");

    // …and it was single-use: the next link is gated again.
    let err = dispatch(&r, st, ward, "fauna.bridges.link", link_payload())
        .await
        .expect_err("the spent grant must not unlock a second link");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");
}

/// a grant must not be burned by a retry that was going to
/// fail on a *deterministic pre-condition* anyway.
///
/// The ward asks, the guardian approves, and the bridge is down when the ward
/// retries. Burning the grant there traps the ward in a loop it cannot exit:
/// re-asking mints another grant that the next retry burns exactly the same
/// way, and no amount of asking ever connects the source. The grant must
/// survive the outage and still redeem once the bridge is back.
#[tokio::test]
async fn an_unavailable_bridge_never_burns_the_wards_grant() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (down, performed) = StubBridge::new(false, false);
    let st = state_with_stub(db.clone(), down);
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;

    let link_payload = || {
        encode(&fauna_protocol::bridges_ui::LinkRequest {
            bridge_id: "stub".into(),
            mode: "oauth".into(),
            params: fauna_protocol::Value::Null,
            extra: Default::default(),
        })
    };

    ask_feed_source(&r, st.clone(), ward, "stub", "link", "", "Stub")
        .await
        .expect("ask ok");
    decide_feed_source(&r, st.clone(), guardian, ward, "stub", "link", "", true)
        .await
        .expect("approve ok");

    // The bridge is unavailable: the retry is refused on availability, and the
    // gate never runs, so nothing is spent.
    let err = dispatch(&r, st.clone(), ward, "fauna.bridges.link", link_payload())
        .await
        .expect_err("an unavailable bridge refuses the link");
    assert_eq!(err.code, "fauna.bridges.not_found");
    assert!(
        !performed.load(Ordering::SeqCst),
        "an unavailable provider is never asked to link"
    );

    // The bridge comes back. The SAME grant — never re-asked for — redeems.
    // This is the whole point: it outlived a transient outage.
    let (up, performed_up) = StubBridge::new(true, false);
    let st_up = state_with_stub(db.clone(), up);
    dispatch(&r, st_up, ward, "fauna.bridges.link", link_payload())
        .await
        .expect("the grant survived the outage and still redeems");
    assert!(performed_up.load(Ordering::SeqCst), "the link ran");
}

/// The other half of the same boundary, and the ratified invariant that must
/// survive the fix above: once the gate has *run*, consume-before-perform
/// means a failure inside the operation still burns the grant.
///
/// The nest cannot know whether a side effect landed on the far side of a
/// provider call, so it must assume it did and make the ward re-ask. This is
/// the non-deterministic failure class — unlike an unavailable bridge, a
/// retry here is not guaranteed to fail, so the grant cannot be silently
/// reused.
#[tokio::test]
async fn a_reached_provider_failure_still_burns_the_grant() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (flaky, performed) = StubBridge::new(true, true);
    let st = state_with_stub(db.clone(), flaky);
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;

    let link_payload = || {
        encode(&fauna_protocol::bridges_ui::LinkRequest {
            bridge_id: "stub".into(),
            mode: "oauth".into(),
            params: fauna_protocol::Value::Null,
            extra: Default::default(),
        })
    };

    ask_feed_source(&r, st.clone(), ward, "stub", "link", "", "Stub")
        .await
        .expect("ask ok");
    decide_feed_source(&r, st.clone(), guardian, ward, "stub", "link", "", true)
        .await
        .expect("approve ok");

    // The gate ran, consumed, and handed off — the provider itself then failed.
    let err = dispatch(&r, st.clone(), ward, "fauna.bridges.link", link_payload())
        .await
        .expect_err("the provider refuses");
    assert_eq!(err.code, "fauna.bridges.not_found");
    assert!(
        performed.load(Ordering::SeqCst),
        "the link was genuinely attempted — this is a post-consume failure"
    );

    // The grant is spent: the ward must ask again, not silently retry.
    let err = dispatch(&r, st, ward, "fauna.bridges.link", link_payload())
        .await
        .expect_err("the burned grant unlocks nothing");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");
}

/// `add_follow` carries the same fix as `link` (both gate after the read-only
/// provider checks), so it gets the same pin.
#[tokio::test]
async fn an_unavailable_bridge_never_burns_a_follow_grant() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (down, performed) = StubBridge::new(false, false);
    let st = state_with_stub(db.clone(), down);
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;

    let follow_payload = || {
        encode(&fauna_protocol::bridges_ui::AddFollowRequest {
            bridge_id: "stub".into(),
            id: "feed-1".into(),
            petname: None,
            extra: None,
            unknown_keys: Default::default(),
        })
    };

    ask_feed_source(&r, st.clone(), ward, "stub", "follow", "feed-1", "A feed")
        .await
        .expect("ask ok");
    decide_feed_source(
        &r,
        st.clone(),
        guardian,
        ward,
        "stub",
        "follow",
        "feed-1",
        true,
    )
    .await
    .expect("approve ok");

    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.add_follow",
        follow_payload(),
    )
    .await
    .expect_err("an unavailable bridge refuses the follow");
    assert_eq!(err.code, "fauna.bridges.not_found");
    assert!(!performed.load(Ordering::SeqCst));

    let (up, performed_up) = StubBridge::new(true, false);
    let st_up = state_with_stub(db.clone(), up);
    dispatch(
        &r,
        st_up,
        ward,
        "fauna.bridges.add_follow",
        follow_payload(),
    )
    .await
    .expect("the follow grant survived the outage");
    assert!(performed_up.load(Ordering::SeqCst), "the follow ran");
}

#[tokio::test]
async fn feed_source_deny_drops_the_ask_and_unlocks_nothing() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;
    let uri = "at://did:plc:x/app.bsky.feed.generator/denied";

    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", uri, "Nope")
        .await
        .expect("ask ok");
    decide_feed_source(
        &r,
        st.clone(),
        guardian,
        ward,
        "bluesky",
        "feed",
        uri,
        false,
    )
    .await
    .expect("deny ok");

    // The ask is gone from both surfaces, the ward was never rung, and the
    // operation stays refused.
    assert!(
        approvals(&r, st.clone(), guardian)
            .await
            .approvals
            .iter()
            .all(|e| e.kind != "feed_source")
    );
    assert!(
        ward_status(&r, st.clone(), ward)
            .await
            .feed_requests
            .is_empty()
    );
    assert_eq!(
        feed_doorbells(&db, ward, "family.feed_source_approved").await,
        0,
        "a deny must not ring the ward with an approval"
    );
    let err = dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", uri),
    )
    .await
    .expect_err("a denied ask unlocks nothing");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // Deciding an ask that is not live is `not_found` — never a silent ok.
    let err = decide_feed_source(&r, st.clone(), guardian, ward, "bluesky", "feed", uri, true)
        .await
        .expect_err("the ask is gone");
    assert_eq!(err.code, "fauna.family.not_found");

    // The child may re-ask after a deny, and it rings again (a new row, a new
    // AUTOINCREMENT id — a reused id would swallow this as a duplicate).
    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", uri, "Please")
        .await
        .expect("re-ask ok");
    assert_eq!(
        feed_doorbells(&db, guardian, "family.feed_source_request").await,
        2,
        "a re-ask after a deny must re-ring"
    );
}

#[tokio::test]
async fn feed_source_request_refusals() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;

    // An unnameable operation is refused outright — never degraded to one of
    // the three, which would mint a grant for an object nobody approved.
    for op in ["", "hologram", "Feed", "unlink"] {
        let err = ask_feed_source(&r, st.clone(), ward, "bluesky", op, "at://x", "")
            .await
            .unwrap_err();
        assert_eq!(
            err.code, "fauna.family.invalid_params",
            "operation {op:?} must be refused"
        );
    }

    // follow/feed name a specific object, so an empty target is refused.
    let err = ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", "", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.family.invalid_params");

    // An empty or over-long bridge_id is refused.
    let err = ask_feed_source(&r, st.clone(), ward, "", "feed", "at://x", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.family.invalid_params");
    let err = ask_feed_source(&r, st.clone(), ward, &"b".repeat(65), "feed", "at://x", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.family.invalid_params");

    // An over-long label is refused (it rides the guardian's queue row).
    let err = ask_feed_source(
        &r,
        st.clone(),
        ward,
        "bluesky",
        "feed",
        "at://x",
        &"l".repeat(129),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.family.invalid_params");

    // An unsupervised account has no guardian to ask.
    let err = ask_feed_source(&r, st.clone(), guardian, "bluesky", "feed", "at://x", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.family.permission_denied");

    // With the knob back on `allow` the ward adds sources freely, so the
    // refusal that carries this affordance can never have fired.
    set_feed_sources(&r, st.clone(), guardian, ward, "allow").await;
    let err = ask_feed_source(&r, st, ward, "bluesky", "feed", "at://x", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.family.invalid_params");
}

/// Relaxing the knob makes a live grant inert rather than stranding anything —
/// the gate simply stops firing (family-safety.md § Feed-source approvals).
#[tokio::test]
async fn a_grant_outliving_a_knob_relax_is_inert() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;
    let uri = "at://did:plc:x/app.bsky.feed.generator/f";

    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", uri, "F")
        .await
        .expect("ask ok");
    decide_feed_source(&r, st.clone(), guardian, ward, "bluesky", "feed", uri, true)
        .await
        .expect("approve ok");

    set_feed_sources(&r, st.clone(), guardian, ward, "allow").await;

    // Two unrelated feeds now both succeed — the grant is not being spent to
    // buy them, because the gate no longer fires at all.
    for u in ["at://a", "at://b"] {
        dispatch(
            &r,
            st.clone(),
            ward,
            "fauna.bridges.feeds.create",
            feed_payload("bluesky", u),
        )
        .await
        .expect("ungated");
    }

    // And re-blocking finds the grant still there, unspent.
    set_feed_sources(&r, st.clone(), guardian, ward, "block").await;
    dispatch(
        &r,
        st,
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", uri),
    )
    .await
    .expect("the untouched grant still redeems");
}

#[tokio::test]
async fn graduation_drops_feed_requests() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (guardian, ward) = blocked_ward(&db, &st, &r).await;

    ask_feed_source(&r, st.clone(), ward, "bluesky", "feed", "at://x", "")
        .await
        .expect("ask ok");
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("graduate ok");

    // The link is gone, so the gate no longer fires and the ask is moot.
    assert!(
        ward_status(&r, st.clone(), ward)
            .await
            .feed_requests
            .is_empty(),
        "graduation drops the asks"
    );
    dispatch(
        &r,
        st,
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", "at://x"),
    )
    .await
    .expect("a graduated account adds sources freely");
}

/// A `feed_sources` value this binary cannot name **fails closed to block** at
/// the gate — the nest-side twin of the client render rule (family-safety.md
/// § Implementation status: *"an unrecognized value renders as the strictest
/// option, never the permissive one"*).
///
/// Reachable within a major version: `policy.update` refuses an unnameable value
/// today, so only a *newer nest* can write one — and then a rollback (or a
/// standby on the older binary) reads it. Fail-open there would silently void a
/// ward's protection for real, which is why this is written straight into the
/// policy row the way that newer nest would (`update_guardian_policy` is the
/// unvalidated DB-level writer; the RPC path could not express it).
#[tokio::test]
async fn an_unnameable_feed_sources_value_fails_closed_at_the_gate() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let (_guardian, ward) = blocked_ward(&db, &st, &r).await;

    for junk in ["hologram", "Block", "BLOCK", "", "reject", "allow-ish"] {
        db.update_guardian_policy(
            &ward, false, "hold", false, junk, None, None, None, None, None,
        )
        .await
        .expect("a newer nest's value lands in the row");
        let err = dispatch(
            &r,
            st.clone(),
            ward,
            "fauna.bridges.feeds.create",
            feed_payload("bluesky", "at://x"),
        )
        .await
        .expect_err("an unnameable feed_sources must fail closed to block");
        assert_eq!(
            err.code, "fauna.bridges.guardian_approval_required",
            "feed_sources {junk:?} must fail closed to block, never open to allow"
        );
    }

    // …and the one permissive value still means allow — the rule is fail-CLOSED,
    // not block-everything (which would pass the loop above vacuously).
    db.update_guardian_policy(
        &ward, false, "hold", false, "allow", None, None, None, None, None,
    )
    .await
    .unwrap();
    dispatch(
        &r,
        st,
        ward,
        "fauna.bridges.feeds.create",
        feed_payload("bluesky", "at://x"),
    )
    .await
    .expect("an explicit allow still adds sources freely");
}

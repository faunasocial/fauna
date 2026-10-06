//! In-process WS-RPC conformance for the account age band — nest/wire half
//! (`docs/goal/behavior/family-safety.md` § The account age band, ratified
//! 2026-08-22; registration interplay
//! `docs/goal/architecture/nest/public-mode.md` § Registration Modes → *Age at
//! registration*).
//!
//! Covers the ratified arms end-to-end through the real handlers: the
//! guardian-asserted band through mint → verify (pre-redemption disclosure) →
//! register → `fauna.family.status` (both roles) with the banded
//! guardian-policy defaults materialized; the by-construction `18+`/`none` of
//! an open-mode self-registration (**absence is the representation** — no
//! row); the admin require-knob (set → setup.status read-back → refusal of
//! every self-service path, declared claims not satisfying it); the
//! store-says-minor refusal (open registration and unsupervised codes, the
//! latter WITHOUT burning a code use; a guardian-designated code stays the
//! intended path); absence-as-signal on the invite-request row; the
//! three-outcome attestation seam (a forged iOS attestation refused; an
//! attestation for an unarmed or unknown platform admitted as declared-only,
//! nonce unconsumed, never satisfying the require-knob; the age-nonce reply's
//! `attestation_platforms`); and graduation deleting the band row so the no-link rule holds
//! again.
//!
//! Tier: tier_3 (real `AppState` + real in-memory `CacheDb` — no mocks).

mod common;
use common::admin_actor;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;

use fauna_core::identity::ActorKeypair;
use fauna_core::obligation::ContentFloor;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use fauna_nest::{
    account_handlers, admin_ws_handlers, discovery_handlers, family_handlers, invite_handlers,
    node_policy_handlers,
};
use fauna_protocol::admin::{
    AdminInviteCodeCreateReply, AdminInviteCodeCreateRequest, AdminInviteCodesListReply,
    AdminInviteCodesListRequest, AdminInviteRequestApproveRequest, AdminInviteRequestsListReply,
    AdminInviteRequestsListRequest,
};
use fauna_protocol::age::{AgeAttestation, AgeClaim, AgeNonceReply, AgeNonceRequest};
use fauna_protocol::discovery::SetupStatusReply;
use fauna_protocol::family::{FamilyGraduateRequest, FamilyStatusReply, FamilyStatusRequest};
use fauna_protocol::invite::{InviteCodeVerify, InviteCodeVerifyReply};
use fauna_protocol::node_policy::{RegistrationMode, SetAgeVerificationRequiredRequest};
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode};

const DOMAIN: &str = "test.fauna.social";

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    invite_handlers::register_invite_handlers(&mut b);
    account_handlers::register_account_handlers(&mut b);
    family_handlers::register_family_handlers(&mut b);
    node_policy_handlers::register_node_policy_handlers(&mut b);
    discovery_handlers::register_discovery_handlers(&mut b);
    b.build()
}

fn state_in_mode(db: Arc<CacheDb>, mode: RegistrationMode) -> Arc<AppState> {
    Arc::new(AppState {
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                reserved_handles: vec![],
            },
            ..Default::default()
        },
        registration_mode: Arc::new(tokio::sync::RwLock::new((mode, None))),
        ..AppState::for_test(db)
    })
}

async fn guardian_user(db: &CacheDb, handle: &str) -> [u8; 32] {
    let kp = ActorKeypair::generate();
    let id = kp.actor_id().0;
    db.create_user_with_handle(&id, "personal", handle, None)
        .await
        .unwrap();
    id
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

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

fn declared(band: &str) -> AgeClaim {
    AgeClaim {
        band: band.to_string(),
        attestation: None,
        extra: Default::default(),
    }
}

async fn mint(
    router: &RpcRouter,
    state: Arc<AppState>,
    admin: [u8; 32],
    guardian: Option<[u8; 32]>,
    age_band: Option<&str>,
) -> Result<String, RpcError> {
    let out = dispatch(
        router,
        state,
        admin,
        "fauna.admin.invite_codes.create",
        encode(&AdminInviteCodeCreateRequest {
            guardian_actor: guardian.map(|g| ByteBuf::from(g.to_vec())),
            age_band: age_band.map(str::to_string),
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode::<AdminInviteCodeCreateReply>(&out).unwrap().code)
}

async fn family_status(r: &RpcRouter, st: Arc<AppState>, actor: [u8; 32]) -> FamilyStatusReply {
    let out = dispatch(
        r,
        st,
        actor,
        "fauna.family.status",
        encode(&FamilyStatusRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("family.status ok");
    decode(&out).unwrap()
}

// ── The guardian-asserted band, end to end ──────────────────────────────────

#[tokio::test]
async fn banded_mint_carries_the_band_through_admission_to_status_and_defaults() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::InviteRequired);
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let code = mint(&r, st.clone(), admin, Some(guardian), Some("u13"))
        .await
        .expect("banded mint ok");

    // The admin list row surfaces the band beside the guardian.
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
        .expect("list ok"),
    )
    .unwrap();
    let row = list
        .invite_codes
        .iter()
        .find(|c| c.code == code)
        .expect("minted row listed");
    assert_eq!(row.age_band.as_deref(), Some("u13"));
    assert!(row.guardian_actor.is_some());

    // The pre-redemption disclosure carries the band beside the guardian
    // handle (transparency at creation — the applicant sees BOTH before
    // redeeming).
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
    assert_eq!(verify.supervised_by.as_deref(), Some("parent"));
    assert_eq!(verify.age_band.as_deref(), Some("u13"));

    // Redeem: the ward is admitted supervised, banded.
    let ward_kp = ActorKeypair::generate();
    let ward = ward_kp.actor_id().0;
    dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.account.register",
        common::register_payload(&ward_kp, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("banded admission registers");

    // Both roles see band + provenance on `fauna.family.status`.
    let ward_view = family_status(&r, st.clone(), ward).await;
    let own = ward_view.age_band.expect("ward sees own band");
    assert_eq!(own.band, "u13");
    assert_eq!(own.provenance, "guardian-asserted");
    let guardian_view = family_status(&r, st.clone(), guardian).await;
    let ward_row = &guardian_view.wards[0];
    let ward_band = ward_row.age_band.as_ref().expect("guardian sees ward band");
    assert_eq!(ward_band.band, "u13");
    assert_eq!(ward_band.provenance, "guardian-asserted");

    // The defaults dial materialized: the fresh policy row IS the U13
    // catalog (`ReachPolicy::age_band_defaults`), not the unsupervised-
    // equivalent — which the guardian then edits per-knob exactly as today.
    let policy = ward_view.policy.expect("supervised ward carries a policy");
    assert!(policy.contact_approval, "U13 default: contact approval on");
    assert_eq!(policy.unknown_sender_mail, "hold");
    assert!(!policy.federation_contact);
    assert_eq!(policy.feed_sources, "block");
    assert_eq!(policy.unknown_peer_dm.as_deref(), Some("hold"));
    let cp = policy.content_policy.expect("U13 default sets floors");
    assert_eq!(cp.nsfw, ContentFloor::Block);
    assert_eq!(policy.content_notify, Some(true));
    let features = policy.features.expect("U13 default sets the feature tier");
    assert_eq!(
        features.get("payments"),
        Some(&fauna_core::feature_gate::FeaturePolicy::DENIED),
        "the minor default denies the payments plane (zaps follows the subset edge)"
    );

    // A band-less supervised admission still starts from the unsupervised-
    // equivalent defaults — the dial only turns when a band rides the carry.
    let plain_code = mint(&r, st.clone(), admin, Some(guardian), None)
        .await
        .expect("plain supervised mint ok");
    let kid2_kp = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        kid2_kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kid2_kp, "kid2", DOMAIN, Some(&plain_code), None),
    )
    .await
    .expect("plain supervised admission registers");
    let kid2_view = family_status(&r, st.clone(), kid2_kp.actor_id().0).await;
    assert!(
        kid2_view.age_band.is_none(),
        "no band chosen at admission -> band unknown, not defaulted"
    );
    let kid2_policy = kid2_view.policy.expect("supervised");
    assert!(!kid2_policy.contact_approval);
    assert_eq!(kid2_policy.unknown_sender_mail, "allow");
}

#[tokio::test]
async fn a_band_without_a_guardian_is_refused_at_mint_and_approve() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::InviteRequired);
    let r = router();
    let admin = admin_actor(&st).await;

    let err = mint(&r, st.clone(), admin, None, Some("13-15"))
        .await
        .expect_err("band without guardian must refuse at mint");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    let err = mint(&r, st.clone(), admin, None, Some("not-a-band"))
        .await
        .expect_err("unknown band token must refuse");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    // The approve twin enforces the same rule (id 999 would 404 later — the
    // band validation deliberately runs before any row lookup cost; a missing
    // row is fine for this arm since validation precedes it… but use a real
    // pending row to pin the ordering honestly).
    let applicant = ActorKeypair::generate();
    let submit_ts = now_ms();
    let submit_msg = fauna_protocol::invite::invite_submit_signed_message(
        &applicant.actor_id().0,
        "wantsin",
        "",
        submit_ts,
    );
    let submit_sig = applicant.signing_key().sign(&submit_msg);
    dispatch(
        &r,
        st.clone(),
        applicant.actor_id().0,
        "fauna.account.invite_request.submit",
        encode(&fauna_protocol::invite::InviteRequestSubmit {
            actor_id: hex::encode(applicant.actor_id().0),
            handle: "wantsin".into(),
            message: String::new(),
            timestamp: submit_ts,
            signature: hex::encode(submit_sig.to_bytes()),
            age_claim: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("submit ok");
    let list: AdminInviteRequestsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
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
    let id = list.invite_requests[0].id;
    let err = dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.invite_requests.approve",
        encode(&AdminInviteRequestApproveRequest {
            id,
            age_band: Some("u13".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("band without guardian must refuse at approve");
    assert_eq!(err.code, "fauna.admin.invalid_params");
}

// ── Open mode: absence IS the representation ────────────────────────────────

#[tokio::test]
async fn an_open_mode_self_registration_mints_no_band_row() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::Open);
    let r = router();

    let kp = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "stranger", DOMAIN, None, None),
    )
    .await
    .expect("open registration ok");

    // `18+`/`none` is BY CONSTRUCTION: no row exists (public-mode.md § Age at
    // registration — the make-unrepresentable shape), and the status read
    // reports nothing.
    assert_eq!(db.get_age_band(&kp.actor_id().0).await.unwrap(), None);
    let view = family_status(&r, st.clone(), kp.actor_id().0).await;
    assert!(view.age_band.is_none());

    // A declared ADULT claim changes nothing: declared-only claims mint no
    // row (they exist for the minor refusal and the request-row signal).
    let kp2 = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        kp2.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp2, "grownup", DOMAIN, None, Some(declared("18+"))),
    )
    .await
    .expect("declared-adult open registration ok");
    assert_eq!(db.get_age_band(&kp2.actor_id().0).await.unwrap(), None);
}

// ── The require-knob ────────────────────────────────────────────────────────

#[tokio::test]
async fn the_require_knob_gates_self_service_and_reads_back() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::Open);
    let r = router();
    let admin = admin_actor(&st).await;

    // Default off — and the setup read-back says so.
    let status: SetupStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            admin,
            "fauna.setup.status",
            encode(&fauna_protocol::discovery::SetupStatusRequest::default()),
        )
        .await
        .expect("setup.status ok"),
    )
    .unwrap();
    assert!(!status.age_verification_required);

    // Arm the knob from the admin surface; the read-back flips live.
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.set_age_verification_required",
        encode(&SetAgeVerificationRequiredRequest {
            required: true,
            extra: Default::default(),
        }),
    )
    .await
    .expect("set knob ok");
    assert!(
        db.get_age_verification_required().await.unwrap() == Some(true),
        "the singleton persisted"
    );
    let status: SetupStatusReply = decode(
        &dispatch(
            &r,
            st.clone(),
            admin,
            "fauna.setup.status",
            encode(&fauna_protocol::discovery::SetupStatusRequest::default()),
        )
        .await
        .expect("setup.status ok"),
    )
    .unwrap();
    assert!(status.age_verification_required);

    // Open self-registration with no claim: refused.
    let kp = ActorKeypair::generate();
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "noclaim", DOMAIN, None, None),
    )
    .await
    .expect_err("knob-armed nest refuses claimless self-service");
    assert_eq!(err.code, "fauna.account.age_verification_required");

    // A DECLARED claim does not satisfy the knob — only a verified attested
    // one does (`public-mode.md` § Age at registration).
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "noclaim", DOMAIN, None, Some(declared("18+"))),
    )
    .await
    .expect_err("declared-only claim does not satisfy the knob");
    assert_eq!(err.code, "fauna.account.age_verification_required");

    // Invite-code redemption is self-service too — gated alike, and the
    // refusal must NOT burn the code (checked before consumption).
    let guardian = guardian_user(&db, "parent").await;
    let code = mint(&r, st.clone(), admin, Some(guardian), Some("13-15"))
        .await
        .expect("mint ok");
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect_err("code redemption is self-service — knob-gated");
    assert_eq!(err.code, "fauna.account.age_verification_required");
    assert!(
        db.peek_invite_code(&code).await.unwrap().is_some(),
        "a knob refusal must not burn the code"
    );

    // Disarm: the same ceremony admits.
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.set_age_verification_required",
        encode(&SetAgeVerificationRequiredRequest {
            required: false,
            extra: Default::default(),
        }),
    )
    .await
    .expect("unset knob ok");
    dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "kid", DOMAIN, Some(&code), None),
    )
    .await
    .expect("disarmed nest admits");
}

// ── Store-says-minor ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_minor_claim_refuses_unsupervised_paths_without_burning_codes() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::Open);
    let r = router();
    let admin = admin_actor(&st).await;

    // Open self-registration with a minor claim → typed refusal pointing at
    // guardian-mediated admission.
    let kp = ActorKeypair::generate();
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "kiddo", DOMAIN, None, Some(declared("13-15"))),
    )
    .await
    .expect_err("open self-registration refuses a minor claim");
    assert_eq!(err.code, "fauna.account.guardian_admission_required");

    // An UNSUPERVISED code + a minor claim → same refusal, and the code
    // survives for a guardian-mediated retry (non-consuming peek arm).
    let plain_code = mint(&r, st.clone(), admin, None, None)
        .await
        .expect("plain mint ok");
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(
            &kp,
            "kiddo",
            DOMAIN,
            Some(&plain_code),
            Some(declared("u13")),
        ),
    )
    .await
    .expect_err("unsupervised code refuses a minor claim");
    assert_eq!(err.code, "fauna.account.guardian_admission_required");
    assert!(
        db.peek_invite_code(&plain_code).await.unwrap().is_some(),
        "the refusal must not burn the unsupervised code"
    );

    // A guardian-designated code IS the intended path for a minor: admits,
    // supervised — and a declared claim still mints no row when the code
    // carries no band.
    let guardian = guardian_user(&db, "parent").await;
    let g_code = mint(&r, st.clone(), admin, Some(guardian), None)
        .await
        .expect("guardian mint ok");
    dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(&kp, "kiddo", DOMAIN, Some(&g_code), Some(declared("u13"))),
    )
    .await
    .expect("guardian-mediated admission admits the minor claim");
    let view = family_status(&r, st.clone(), kp.actor_id().0).await;
    assert!(view.supervised_by.is_some(), "admitted supervised");
    assert!(
        view.age_band.is_none(),
        "a declared claim mints nothing — only the guardian's dial or a verified attestation does"
    );
}

// ── Absence-as-signal on the request row ────────────────────────────────────

#[tokio::test]
async fn the_invite_request_row_records_the_claim_as_a_signal() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::InviteRequired);
    let r = router();
    let admin = admin_actor(&st).await;

    let submit = |kp: ActorKeypair, handle: &'static str, claim: Option<AgeClaim>| {
        let r = &r;
        let st = st.clone();
        async move {
            let ts = now_ms();
            let msg = fauna_protocol::invite::invite_submit_signed_message(
                &kp.actor_id().0,
                handle,
                "",
                ts,
            );
            let sig = kp.signing_key().sign(&msg);
            dispatch(
                r,
                st,
                kp.actor_id().0,
                "fauna.account.invite_request.submit",
                encode(&fauna_protocol::invite::InviteRequestSubmit {
                    actor_id: hex::encode(kp.actor_id().0),
                    handle: handle.into(),
                    message: String::new(),
                    timestamp: ts,
                    signature: hex::encode(sig.to_bytes()),
                    age_claim: claim,
                    extra: Default::default(),
                }),
            )
            .await
        }
    };

    submit(ActorKeypair::generate(), "withclaim", Some(declared("u13")))
        .await
        .expect("submit with claim ok");
    submit(ActorKeypair::generate(), "silent", None)
        .await
        .expect("submit without claim ok");

    let list: AdminInviteRequestsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
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
    let with_claim = list
        .invite_requests
        .iter()
        .find(|q| q.handle == "withclaim")
        .unwrap();
    assert_eq!(with_claim.age_band.as_deref(), Some("u13"));
    assert_eq!(
        with_claim.age_band_provenance.as_deref(),
        Some("none"),
        "a declared-only claim records provenance `none` — the admin sees it was unattested"
    );
    let silent = list
        .invite_requests
        .iter()
        .find(|q| q.handle == "silent")
        .unwrap();
    assert!(silent.age_band.is_none(), "absence IS the signal");
    assert!(silent.age_band_provenance.is_none());
}

// ── The attestation seam: verified / failed check / cannot check ────────────

/// An attested claim over a fresh nest-minted nonce, for `platform`.
fn attested(platform: &str, band: &str, nonce: &str) -> AgeClaim {
    AgeClaim {
        band: band.to_string(),
        attestation: Some(AgeAttestation {
            platform: platform.to_string(),
            nonce: nonce.to_string(),
            key_id: "bb".repeat(32),
            attestation_object: fauna_protocol::ByteBuf::from(vec![0xA0]),
            extra: Default::default(),
        }),
        extra: Default::default(),
    }
}

async fn mint_age_nonce(r: &RpcRouter, st: Arc<AppState>) -> AgeNonceReply {
    decode(
        &dispatch(
            r,
            st,
            [0u8; 32],
            "fauna.account.age_nonce",
            encode(&AgeNonceRequest::default()),
        )
        .await
        .expect("age_nonce ok"),
    )
    .unwrap()
}

/// `family-safety.md` § The account age band → *An attestation the nest cannot
/// check*: a failed check the nest can run refuses the admission; an
/// attestation for a platform this build holds no verifier for is ignored
/// unread (nonce not consumed) and the claim proceeds as declared-only; the
/// age-nonce reply says which platforms are armed.
#[tokio::test]
async fn the_attestation_seam_refuses_a_failed_check_and_degrades_a_cannot_check() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::Open);
    let r = router();

    // The advertisement: iOS is armed (`fauna_protocol::age::FAUNA_IOS_APP_ID`
    // is the team-prefixed App ID), android is not
    // (`age_attest::FAUNA_ANDROID_PLAY_INTEGRITY_KEYS` is `None`).
    let reply = mint_age_nonce(&r, st.clone()).await;
    assert_eq!(reply.attestation_platforms, vec!["ios".to_string()]);

    // Outcome 2 — iOS is checkable: a forged attestation over a nonce this nest
    // never minted is refused before any chain work, and the refusal refuses
    // the admission — never a silent downgrade to declared-only. The chain
    // verifier itself is covered by the `age_attest` unit tests.
    let kp = ActorKeypair::generate();
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(
            &kp,
            "iosuser",
            DOMAIN,
            None,
            Some(attested("ios", "18+", &"aa".repeat(32))),
        ),
    )
    .await
    .expect_err("a forged ios attestation must refuse");
    assert_eq!(err.code, "fauna.account.age_attestation_invalid");

    // Outcome 3 — android is unarmed: the attestation is ignored unread, the
    // admission proceeds exactly as declared-only (no band row), and the nonce
    // it named was never consumed.
    let droid_nonce = mint_age_nonce(&r, st.clone()).await.nonce;
    let kp = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(
            &kp,
            "droiduser",
            DOMAIN,
            None,
            Some(attested("android", "18+", &droid_nonce)),
        ),
    )
    .await
    .expect("an unarmed android attestation admits as declared-only");
    assert_eq!(db.get_age_band(&kp.actor_id().0).await.unwrap(), None);
    let nonce: [u8; 32] = hex::decode(&droid_nonce).unwrap().try_into().unwrap();
    assert!(
        st.auth.age_nonce_store.consume(&nonce).await,
        "a cannot-check attestation must not consume its nonce"
    );

    // A token this build does not know — a platform a newer app speaks — is
    // the same outcome, and the arm that stays live once android is armed.
    let kp = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(
            &kp,
            "futureuser",
            DOMAIN,
            None,
            Some(attested("visionos", "18+", &"aa".repeat(32))),
        ),
    )
    .await
    .expect("an unknown platform admits as declared-only");
    assert_eq!(db.get_age_band(&kp.actor_id().0).await.unwrap(), None);

    // The require-knob is satisfied only by a VERIFIED claim: with it on, a
    // cannot-check claim is refused as every declared-only claim is.
    let admin = admin_actor(&st).await;
    dispatch(
        &r,
        st.clone(),
        admin,
        "fauna.admin.set_age_verification_required",
        encode(&SetAgeVerificationRequiredRequest {
            required: true,
            extra: Default::default(),
        }),
    )
    .await
    .expect("set knob ok");
    let kp = ActorKeypair::generate();
    let err = dispatch(
        &r,
        st.clone(),
        kp.actor_id().0,
        "fauna.account.register",
        common::register_payload(
            &kp,
            "knobdroid",
            DOMAIN,
            None,
            Some(attested("android", "18+", &droid_nonce)),
        ),
    )
    .await
    .expect_err("a cannot-check claim does not satisfy the require-knob");
    assert_eq!(err.code, "fauna.account.age_verification_required");
}

/// The invite-request path records a cannot-check claim's provenance as
/// `none` — never `attested-*` — and a failed check still refuses the submit.
#[tokio::test]
async fn an_invite_request_records_a_cannot_check_claim_as_declared() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::InviteRequired);
    let r = router();
    let admin = admin_actor(&st).await;

    let submit = |handle: &'static str, claim: AgeClaim| {
        let r = &r;
        let st = st.clone();
        async move {
            let kp = ActorKeypair::generate();
            let ts = now_ms();
            let msg = fauna_protocol::invite::invite_submit_signed_message(
                &kp.actor_id().0,
                handle,
                "",
                ts,
            );
            let sig = kp.signing_key().sign(&msg);
            dispatch(
                r,
                st,
                kp.actor_id().0,
                "fauna.account.invite_request.submit",
                encode(&fauna_protocol::invite::InviteRequestSubmit {
                    actor_id: hex::encode(kp.actor_id().0),
                    handle: handle.into(),
                    message: String::new(),
                    timestamp: ts,
                    signature: hex::encode(sig.to_bytes()),
                    age_claim: Some(claim),
                    extra: Default::default(),
                }),
            )
            .await
        }
    };

    submit("droidkid", attested("android", "13-15", &"aa".repeat(32)))
        .await
        .expect("a cannot-check claim submits as declared-only");
    let err = submit("ioskid", attested("ios", "13-15", &"aa".repeat(32)))
        .await
        .expect_err("a failed ios check refuses the submit");
    assert_eq!(err.code, "fauna.account.age_attestation_invalid");

    let list: AdminInviteRequestsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
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
    let row = list
        .invite_requests
        .iter()
        .find(|q| q.handle == "droidkid")
        .unwrap();
    assert_eq!(row.age_band.as_deref(), Some("13-15"));
    assert_eq!(row.age_band_provenance.as_deref(), Some("none"));
    assert!(list.invite_requests.iter().all(|q| q.handle != "ioskid"));
}

// ── Graduation restores the by-construction rule ────────────────────────────

#[tokio::test]
async fn graduation_deletes_the_band_row() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in_mode(db.clone(), RegistrationMode::InviteRequired);
    let r = router();
    let admin = admin_actor(&st).await;
    let guardian = guardian_user(&db, "parent").await;

    let code = mint(&r, st.clone(), admin, Some(guardian), Some("16-17"))
        .await
        .expect("banded mint ok");
    let ward_kp = ActorKeypair::generate();
    let ward = ward_kp.actor_id().0;
    dispatch(
        &r,
        st.clone(),
        ward,
        "fauna.account.register",
        common::register_payload(&ward_kp, "teen", DOMAIN, Some(&code), None),
    )
    .await
    .expect("banded admission ok");
    assert!(db.get_age_band(&ward).await.unwrap().is_some());

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

    // The band dies with the link: a graduated account is `18+`/`none` BY
    // CONSTRUCTION again (no row), not a stored minor forever.
    assert_eq!(db.get_age_band(&ward).await.unwrap(), None);
    let view = family_status(&r, st.clone(), ward).await;
    assert!(view.supervised_by.is_none());
    assert!(view.age_band.is_none());
}

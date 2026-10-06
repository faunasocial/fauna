//! Integration round-trip for the depth selector's transition kind —
//! `fauna.bridges.atproto.set_integration_level` (S4-A).
//!
//! Authority: `docs/goal/ui/atproto.md` § Transition semantics (the matrix),
//! § State & data shape (the persisted level), § Where logic lives (the "one
//! client call per confirmed transition, atomic, idempotent on retry"
//! contract); `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation
//! layer 2 (a step-down deactivates *reversibly* — the DID is retained).
//!
//! What is pinned here is everything reachable without the Go bridge: the level
//! actually persists, the ladder walks in both directions, a step-down retains
//! the identity row rather than destroying it, re-confirming a level is a
//! no-op, and the mint parameters are validated before anything mutates. What
//! the *bridge* does with a `deactivated` row — stop serving the repo, announce
//! `#account` — is slice D and is proven by its own tier_3.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real in-memory `CacheDb`, no
//! mocks) — the `conformance_bluesky.rs` shape.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_atproto_handlers::register_bridge_atproto_handlers,
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError,
    atproto::IntegrationLevel,
    atproto_pds::{
        DeletePresenceReply, DeletePresenceRequest, GetIntegrationStatusReply,
        GetIntegrationStatusRequest, RecordTombstoneReply, RecordTombstoneRequest,
        RequestTombstoneReply, RequestTombstoneRequest, SetIntegrationLevelReply,
        SetIntegrationLevelRequest,
    },
    decode_strict, encode_canonical,
};

const KIND: &str = "fauna.bridges.atproto.set_integration_level";
const STATUS_KIND: &str = "fauna.bridges.atproto.get_integration_status";
const DELETE_KIND: &str = "fauna.bridges.atproto.delete_presence";
const REQUEST_TOMBSTONE_KIND: &str = "fauna.bridges.atproto.request_tombstone";
const RECORD_TOMBSTONE_KIND: &str = "fauna.bridges.atproto.record_tombstone";

/// A syntactically valid p256 `did:key` — the senior rotation pubkey a real
/// client generates. Content is irrelevant here; only the shape is validated.
const ROTATION_KEY: &str = "did:key:zDnaerDaTF5BXEavCrfRZEk316dpbLsfPDZ3WJ5hRTPFU2169"; // gitleaks:allow

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_atproto_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    req: &SetIntegrationLevelRequest,
) -> Result<SetIntegrationLevelReply, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(KIND).expect("kind registered");
    let payload = Bytes::from(encode_canonical(req).unwrap().to_vec());
    let bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode_strict(&bytes).expect("reply decodes"))
}

/// Move to `level`, carrying the mint parameters a hosted entry needs.
fn to(level: IntegrationLevel) -> SetIntegrationLevelRequest {
    SetIntegrationLevelRequest {
        target_level: level.as_str().into(),
        did_method: "plc".into(),
        user_rotation_pub_did_key: ROTATION_KEY.into(),
        history_backfill: false,
        ..Default::default()
    }
}

/// Read the caller's integration status through the USER-class read kind.
async fn status(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> GetIntegrationStatusReply {
    let meta = router
        .kind_meta(STATUS_KIND)
        .expect("status kind registered");
    let payload = Bytes::from(
        encode_canonical(&GetIntegrationStatusRequest::default())
            .unwrap()
            .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload)
        .await
        .expect("status read succeeds");
    decode_strict(&bytes).expect("status reply decodes")
}

// ── kind metadata + allowlist ──────────────────────────────────────

#[tokio::test]
async fn transition_kind_is_user_class_and_replay_safe() {
    let (router, _state) = router_with_db().await;
    let meta = router.kind_meta(KIND).expect("kind registered");
    assert!(
        !meta.forbid_replay,
        "the transition is idempotent, so a replayed retry must be allowed — \
         that is what makes the level-written-last crash story converge"
    );

    assert!(is_permitted(CallerClass::User, KIND), "{KIND} for User");
    assert!(is_permitted(CallerClass::Admin, KIND), "Admin ⊇ User");
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
    ] {
        assert!(
            !is_permitted(class, KIND),
            "{KIND} must be denied for {class:?} — the level is the USER's choice"
        );
    }
}

// ── the ladder ─────────────────────────────────────────────────────

#[tokio::test]
async fn default_level_is_off_and_the_ladder_walks_up_and_back_down() {
    let (router, state) = router_with_db().await;
    let actor = [3u8; 32];

    assert_eq!(
        state
            .db
            .get_atproto_integration_level(&actor)
            .await
            .unwrap(),
        IntegrationLevel::Off,
        "a user who has never chosen is OFF (the ratified consent posture)"
    );

    for level in [
        IntegrationLevel::Linked,
        IntegrationLevel::HostedVisible,
        IntegrationLevel::HostedFull,
        IntegrationLevel::HostedVisible,
        IntegrationLevel::Off,
    ] {
        let reply = dispatch(&router, state.clone(), actor, &to(level))
            .await
            .unwrap_or_else(|e| panic!("→ {level:?} must succeed: {e:?}"));
        assert_eq!(
            reply.level,
            level.as_str(),
            "the reply states the new level"
        );
        assert_eq!(
            state
                .db
                .get_atproto_integration_level(&actor)
                .await
                .unwrap(),
            level,
            "→ {level:?} must persist"
        );
    }
}

#[tokio::test]
async fn re_confirming_the_current_level_is_an_idempotent_noop() {
    let (router, state) = router_with_db().await;
    let actor = [4u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    let first = state.db.get_atproto_identity(&actor).await.unwrap();

    // Re-confirming must succeed and must NOT mint a second identity.
    let reply = dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("re-confirm is a no-op success");
    assert_eq!(reply.level, "hosted_visible");
    assert_eq!(
        state.db.get_atproto_identity(&actor).await.unwrap(),
        first,
        "a retry must not disturb the identity row"
    );
}

// ── layer 2: a step-down is REVERSIBLE ─────────────────────────────

#[tokio::test]
async fn stepping_off_a_hosted_level_retains_the_identity_and_re_entry_restores_it() {
    let (router, state) = router_with_db().await;
    let actor = [5u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    let minted = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("a hosted entry records a mint intent");
    assert_eq!(minted.status, "pending", "the bridge has not minted yet");

    // Pretend the bridge completed the mint, so the step-down has a live
    // identity to deactivate (the state a real ladder-walk reaches).
    state
        .db
        .record_atproto_minted(&actor, "did:plc:testdid", Some("bafytestcid"))
        .await
        .expect("record the mint");

    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("step down to off");

    let after = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("the identity row SURVIVES a step-down — layer 2 is reversible");
    assert_eq!(after.status, "deactivated");
    assert_eq!(
        after.did.as_deref(),
        Some("did:plc:testdid"),
        "the DID is retained; destruction is the separate stronger action"
    );

    // Re-entering restores the SAME identity rather than minting a second.
    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("re-enter hosted");
    let restored = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("identity still present");
    assert_eq!(restored.status, "active", "re-entry reactivates");
    assert_eq!(
        restored.did.as_deref(),
        Some("did:plc:testdid"),
        "the same DID — a user who steps down and back up keeps their identity"
    );
}

// ── the delete-presence sweep (S5 slice 5) ─────────────────────────

/// Invoke `fauna.bridges.atproto.delete_presence` for `actor`.
async fn delete_presence(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> Result<DeletePresenceReply, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta(DELETE_KIND)
        .expect("delete kind registered");
    let payload = Bytes::from(
        encode_canonical(&DeletePresenceRequest::default())
            .unwrap()
            .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode_strict(&bytes).expect("delete reply decodes"))
}

#[tokio::test]
async fn delete_kind_is_user_class_and_self_scoped() {
    let (router, _state) = router_with_db().await;
    assert!(
        router.kind_meta(DELETE_KIND).is_some(),
        "the delete kind is registered"
    );
    assert!(
        is_permitted(CallerClass::User, DELETE_KIND),
        "the destructive action is the user's own to take"
    );
    assert!(
        is_permitted(CallerClass::Admin, DELETE_KIND),
        "Admin ⊇ User"
    );
    for class in [
        CallerClass::BridgeAtprotoPds,
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::ContentProcessor,
    ] {
        assert!(
            !is_permitted(class, DELETE_KIND),
            "{class:?} must not be able to destroy a user's presence"
        );
    }
}

/// The contract this slice exists to establish: the sweep destroys the
/// *presence* and keeps the *identity*. The `deleted` status is the durable
/// tombstone the bridge converges on, the DID and the projection floor survive,
/// and the level follows the presence down to `off` rather than leaving the
/// selector parked on a hosted rung with nothing behind it.
#[tokio::test]
async fn deleting_the_presence_tombstones_the_row_but_retains_the_identity() {
    let (router, state) = router_with_db().await;
    let actor = [21u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    state
        .db
        .record_atproto_minted(&actor, "did:plc:doomed", Some("bafytestcid"))
        .await
        .expect("record the mint");
    let floor_before = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("identity")
        .projection_floor_micros;

    let reply = delete_presence(&router, state.clone(), actor)
        .await
        .expect("delete succeeds");
    assert!(reply.newly_deleted, "this call performed the deletion");
    assert_eq!(
        reply.level,
        IntegrationLevel::Off.as_str(),
        "the level follows the presence down"
    );
    assert_eq!(
        state
            .db
            .get_atproto_integration_level(&actor)
            .await
            .unwrap(),
        IntegrationLevel::Off,
        "and it is persisted, not just reported"
    );

    let after =
        state.db.get_atproto_identity(&actor).await.unwrap().expect(
            "the identity row SURVIVES — the sweep destroys the presence, not the identity",
        );
    assert_eq!(
        after.status, "deleted",
        "the tombstone the bridge sweeps on"
    );
    assert_eq!(
        after.did.as_deref(),
        Some("did:plc:doomed"),
        "the DID is retained — destruction of the identity is the separate opt-in PLC tombstone"
    );
    assert_eq!(
        after.projection_floor_micros, floor_before,
        "the consent floor is the record a later re-enable is measured against, not part of the presence"
    );
}

// ── the terminal PLC tombstone (S5 slice 5b) ────────────────────────

/// Invoke `fauna.bridges.atproto.request_tombstone` for `actor`.
async fn request_tombstone(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> Result<RequestTombstoneReply, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta(REQUEST_TOMBSTONE_KIND)
        .expect("request-tombstone kind registered");
    let payload = Bytes::from(
        encode_canonical(&RequestTombstoneRequest::default())
            .unwrap()
            .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode_strict(&bytes).expect("request-tombstone reply decodes"))
}

/// Invoke `fauna.bridges.atproto.record_tombstone` for `actor`.
async fn record_tombstone(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    prev_cid: &str,
) -> Result<RecordTombstoneReply, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta(RECORD_TOMBSTONE_KIND)
        .expect("record-tombstone kind registered");
    let payload = Bytes::from(
        encode_canonical(&RecordTombstoneRequest {
            prev_cid: prev_cid.to_string(),
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode_strict(&bytes).expect("record-tombstone reply decodes"))
}

/// Enter a hosted level and record a minted did:plc for `actor`.
async fn hosted_and_minted(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32], did: &str) {
    dispatch(
        router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    state
        .db
        .record_atproto_minted(&actor, did, Some("bafygenesis"))
        .await
        .expect("record the mint");
}

/// Both halves are the user's own, and neither is reachable by any bridge. That
/// exclusion is the custody split made operative: the bridge holds only the
/// JUNIOR rotation key, and no box-held key may end an identity.
#[tokio::test]
async fn tombstone_kinds_are_user_class_and_never_bridge_callable() {
    let (router, _state) = router_with_db().await;
    for kind in [REQUEST_TOMBSTONE_KIND, RECORD_TOMBSTONE_KIND] {
        assert!(router.kind_meta(kind).is_some(), "{kind} is registered");
        assert!(
            is_permitted(CallerClass::User, kind),
            "{kind} is the user's"
        );
        assert!(is_permitted(CallerClass::Admin, kind), "Admin ⊇ User");
        for class in [
            CallerClass::BridgeAtprotoPds,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::ContentProcessor,
        ] {
            assert!(
                !is_permitted(class, kind),
                "{class:?} must not be able to retire an identity via {kind}"
            );
        }
    }
}

/// `ui/atproto.md` § Don't do these: the tombstone lives *only* inside "Delete
/// my Bluesky presence" and is never a step-down. Pinned on the wire rather than
/// left to seven app implementations to honour — and the same precondition
/// encodes the act's real ordering constraint, since a tombstoned DID stops
/// resolving and a relay that cannot resolve a DID cannot verify the delete
/// commits the sweep still owes.
#[tokio::test]
async fn a_tombstone_cannot_be_requested_outside_the_delete_ceremony() {
    let (router, state) = router_with_db().await;
    let actor = [31u8; 32];
    hosted_and_minted(&router, &state, actor, "did:plc:live").await;

    // Active hosted identity: refused.
    let err = request_tombstone(&router, state.clone(), actor)
        .await
        .expect_err("an active identity cannot be retired");
    assert_eq!(err.code, "fauna.bridges.atproto.not_tombstoneable");

    // Stepped down — the path the goal doc explicitly forbids riding.
    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("step down");
    let err = request_tombstone(&router, state.clone(), actor)
        .await
        .expect_err("a step-down must not reach the terminal act");
    assert_eq!(err.code, "fauna.bridges.atproto.not_tombstoneable");
    assert_eq!(
        state
            .db
            .get_atproto_identity(&actor)
            .await
            .unwrap()
            .unwrap()
            .status,
        "deactivated",
        "and nothing moved"
    );
}

/// The whole terminal flow, in the order the act requires: delete the presence,
/// opt in, then report the client's published tombstone. Only after the report
/// does the identity leave its restorable state — nest records testimony, since
/// it holds no key that could sign the operation and never sees the directory.
#[tokio::test]
async fn the_retirement_runs_delete_then_opt_in_then_the_clients_report() {
    let (router, state) = router_with_db().await;
    let actor = [32u8; 32];
    hosted_and_minted(&router, &state, actor, "did:plc:retiring").await;

    delete_presence(&router, state.clone(), actor)
        .await
        .expect("delete the presence first");

    let opted = request_tombstone(&router, state.clone(), actor)
        .await
        .expect("the opt-in is reachable once the presence is deleted");
    assert!(opted.newly_requested);

    // The status read is what a client's converge pass polls to learn it still
    // owes the act — including a SECOND device, which learns a retirement is in
    // flight rather than offering to restore the presence.
    let mid = status(&router, state.clone(), actor).await;
    let identity = mid.identity.expect("identity summary");
    assert_eq!(
        identity.status, "deleted",
        "not retired until the client acts"
    );
    assert!(identity.tombstone_requested, "but the intent is visible");

    let recorded = record_tombstone(&router, state.clone(), actor, "bafygenesis")
        .await
        .expect("record the published tombstone");
    assert!(recorded.newly_tombstoned);

    let after = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("the row survives — it is the record of WHICH did was retired");
    assert_eq!(after.status, "tombstoned");
    assert_eq!(after.did.as_deref(), Some("did:plc:retiring"));

    // Idempotent both ways: a client that crashed between submitting and
    // reporting re-reports, and converges instead of surfacing a failure for an
    // identity the network has already seen retired.
    assert!(
        !request_tombstone(&router, state.clone(), actor)
            .await
            .expect("a repeat opt-in is a no-op, not an error")
            .newly_requested
    );
    assert!(
        !record_tombstone(&router, state.clone(), actor, "bafygenesis")
            .await
            .expect("a repeat report is a no-op, not an error")
            .newly_tombstoned
    );
}

/// A report for an act this nest never authorized is REFUSED, not answered with
/// a success — because a success here would be silently destructive.
///
/// The report changes nothing either way, so the two zero-row cases are
/// indistinguishable by row count while meaning opposite things. Accepting the
/// unauthorized one leaves the row `'deleted'`, and a `'deleted'` row is still
/// reactivatable — so the user could be re-parked on a hosted rung backing a DID
/// their client says it destroyed, with the `'tombstoned'` re-entry refusal
/// never firing because the terminal status was never written. Reachable
/// whenever a client's `request_tombstone` write was lost, a crash landed
/// between the two calls, or a client skipped the opt-in entirely.
#[tokio::test]
async fn a_tombstone_report_the_nest_never_authorized_is_refused() {
    let (router, state) = router_with_db().await;
    let actor = [34u8; 32];
    hosted_and_minted(&router, &state, actor, "did:plc:unasked").await;

    // A live identity: this is the step-down-laundered-into-a-terminal-act
    // shape, arriving on the report rather than on the ask.
    let err = record_tombstone(&router, state.clone(), actor, "bafylive")
        .await
        .expect_err("no opt-in stands, so the report is refused");
    assert_eq!(err.code, "fauna.bridges.atproto.tombstone_not_authorized");

    // And after the presence is deleted but the opt-in never recorded — the
    // crash window between the two client calls.
    delete_presence(&router, state.clone(), actor)
        .await
        .expect("delete the presence");
    let err = record_tombstone(&router, state.clone(), actor, "bafydeleted")
        .await
        .expect_err("a deleted presence is still not an authorized retirement");
    assert_eq!(err.code, "fauna.bridges.atproto.tombstone_not_authorized");

    let after = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("the row is untouched");
    assert_eq!(after.status, "deleted", "a refused report moves nothing");

    // The refusal has to be loud precisely because THIS still works: the row it
    // left behind is the restorable one.
    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("an unauthorized report must not have retired anything");
    assert_eq!(
        state
            .db
            .get_atproto_identity(&actor)
            .await
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
}

/// Re-entering a hosted level after a retirement mints a FRESH identity — a
/// retirement ends an identity, not the user's ability to have one.
///
/// The retired DID no longer resolves anywhere, so there is nothing to restore
/// and "reactivate" would be a silent lie (the statement matches no row, and the
/// level would land on a hosted rung with nothing behind it). What the user gets
/// instead is what they would get having never enabled: a new mint, on the
/// ordinary path, with the destroyed DID kept as a stored fact.
#[tokio::test]
async fn re_entry_after_a_retirement_mints_a_fresh_identity() {
    let (router, state) = router_with_db().await;
    let actor = [33u8; 32];
    hosted_and_minted(&router, &state, actor, "did:plc:gone").await;
    delete_presence(&router, state.clone(), actor)
        .await
        .expect("delete");
    request_tombstone(&router, state.clone(), actor)
        .await
        .expect("opt in");
    record_tombstone(&router, state.clone(), actor, "bafygenesis")
        .await
        .expect("report");

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("a retirement is not a permanent product lockout");

    let live = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("a fresh identity row");
    assert_eq!(
        live.status, "pending",
        "the mint is owed again — only the bridge may declare an identity active"
    );
    assert_eq!(
        live.did, None,
        "and it is a NEW identity: the retired DID is never re-adopted"
    );
    assert!(!live.tombstone_requested, "the fresh identity owes nothing");

    // The destroyed DID survives as a record. It is the last thing anyone can
    // say about an identity that no longer resolves, and unlike the repo it is
    // not re-derivable from anything.
    let archived = state
        .db
        .list_retired_atproto_identities(&actor)
        .await
        .unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].did, "did:plc:gone");
    assert_eq!(archived[0].genesis_cid.as_deref(), Some("bafygenesis"));

    // The bridge sees exactly one identity for this actor — the live one. A
    // retired row left in the roster would have the bridge trying to serve a
    // repo for a DID no relay can resolve.
    let roster = state.db.list_atproto_identities().await.unwrap();
    assert_eq!(
        roster.iter().filter(|r| r.actor_id == actor).count(),
        1,
        "the archive is not part of the roster"
    );

    // The non-hosted rungs still work either way: a retirement must not strand
    // the account's whole AT Protocol page.
    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("staying off is always available");
}

/// A retirement that has *not* happened must still restore rather than re-mint.
/// The re-mint above hangs on one status, so the neighbouring statuses are worth
/// pinning beside it: a step-down and a presence delete both retain the DID, and
/// silently minting a second identity for either would strand the first.
#[tokio::test]
async fn a_presence_delete_still_restores_the_same_did_rather_than_re_minting() {
    let (router, state) = router_with_db().await;
    let actor = [35u8; 32];
    hosted_and_minted(&router, &state, actor, "did:plc:retained").await;
    delete_presence(&router, state.clone(), actor)
        .await
        .expect("delete the presence, NOT the identity");

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("re-enter");

    let live = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        live.did.as_deref(),
        Some("did:plc:retained"),
        "the same identity is restored — the sweep destroyed the presence only"
    );
    assert_eq!(live.status, "active");
    assert!(
        state
            .db
            .list_retired_atproto_identities(&actor)
            .await
            .unwrap()
            .is_empty(),
        "and nothing was archived, because nothing was retired"
    );
}

/// Confirming twice is a success, not an error: the second call reports that it
/// changed nothing. A destructive action that errors on retry pushes a client
/// into showing a failure for work that is already done.
#[tokio::test]
async fn deleting_an_already_deleted_presence_is_an_idempotent_success() {
    let (router, state) = router_with_db().await;
    let actor = [22u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    state
        .db
        .record_atproto_minted(&actor, "did:plc:twice", Some("bafytestcid"))
        .await
        .expect("record the mint");

    assert!(
        delete_presence(&router, state.clone(), actor)
            .await
            .expect("first delete")
            .newly_deleted
    );
    let second = delete_presence(&router, state.clone(), actor)
        .await
        .expect("a repeat confirm is not an error");
    assert!(
        !second.newly_deleted,
        "the second call reports it changed nothing"
    );
    assert_eq!(
        state
            .db
            .get_atproto_identity(&actor)
            .await
            .unwrap()
            .expect("identity")
            .status,
        "deleted"
    );
}

/// `ui/atproto.md` § Errors & edge cases keeps `atproto-delete-presence`
/// reachable for a *deactivated* identity at level Off/Linked, so the flow must
/// accept that starting state — a user who stepped down and then decided to
/// destroy must not have to step back up first.
#[tokio::test]
async fn a_deactivated_identity_can_still_be_deleted() {
    let (router, state) = router_with_db().await;
    let actor = [23u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    state
        .db
        .record_atproto_minted(&actor, "did:plc:steppeddown", Some("bafytestcid"))
        .await
        .expect("record the mint");
    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("step down first");
    assert_eq!(
        state
            .db
            .get_atproto_identity(&actor)
            .await
            .unwrap()
            .expect("identity")
            .status,
        "deactivated"
    );

    assert!(
        delete_presence(&router, state.clone(), actor)
            .await
            .expect("a deactivated presence can still be destroyed")
            .newly_deleted
    );
    assert_eq!(
        state
            .db
            .get_atproto_identity(&actor)
            .await
            .unwrap()
            .expect("identity")
            .status,
        "deleted"
    );
}

/// "Still reversible in identity terms" is the goal doc's phrase and this is
/// what it has to mean in code: re-entering a hosted level after a delete
/// restores the SAME DID rather than minting a second one. The repo itself is
/// derived, so the bridge rebuilds it from the retained floor.
#[tokio::test]
async fn re_entering_after_a_delete_restores_the_same_did() {
    let (router, state) = router_with_db().await;
    let actor = [24u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    state
        .db
        .record_atproto_minted(&actor, "did:plc:reborn", Some("bafytestcid"))
        .await
        .expect("record the mint");
    delete_presence(&router, state.clone(), actor)
        .await
        .expect("delete");

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("re-enter hosted after a delete");

    let restored = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("identity");
    assert_eq!(restored.status, "active", "re-entry reactivates");
    assert_eq!(
        restored.did.as_deref(),
        Some("did:plc:reborn"),
        "the same DID — the sweep destroyed the presence, never the identity"
    );
}

/// A caller with no identity row has nothing to destroy. Refusing loudly beats
/// a silent success: a client rendering the button in that state has misread
/// its own status snapshot, and a fake success would hide the bug.
#[tokio::test]
async fn deleting_with_no_identity_is_refused() {
    let (router, state) = router_with_db().await;
    let actor = [25u8; 32];

    let err = delete_presence(&router, state.clone(), actor)
        .await
        .expect_err("nothing to delete");
    assert_eq!(err.code, "fauna.bridges.atproto.no_hosted_identity");
}

// ── validation happens before anything mutates ─────────────────────

#[tokio::test]
async fn an_unknown_level_is_rejected_and_changes_nothing() {
    let (router, state) = router_with_db().await;
    let actor = [6u8; 32];

    let err = dispatch(
        &router,
        state.clone(),
        actor,
        &SetIntegrationLevelRequest {
            target_level: "hosted".into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("an unratified level string must be refused");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert_eq!(
        state
            .db
            .get_atproto_integration_level(&actor)
            .await
            .unwrap(),
        IntegrationLevel::Off,
        "a refused transition must not have moved the level"
    );
}

#[tokio::test]
async fn a_hosted_entry_needs_valid_mint_parameters() {
    let (router, state) = router_with_db().await;
    let actor = [7u8; 32];

    for bad in [
        SetIntegrationLevelRequest {
            target_level: "hosted_visible".into(),
            did_method: "plc".into(),
            user_rotation_pub_did_key: "not-a-did-key".into(),
            history_backfill: false,
            ..Default::default()
        },
        SetIntegrationLevelRequest {
            target_level: "hosted_visible".into(),
            did_method: "sideways".into(),
            ..Default::default()
        },
        // did:web has no rotation keys — a supplied one means the client is
        // confused about which method it is entering.
        SetIntegrationLevelRequest {
            target_level: "hosted_visible".into(),
            did_method: "web".into(),
            user_rotation_pub_did_key: ROTATION_KEY.into(),
            history_backfill: false,
            ..Default::default()
        },
    ] {
        let err = dispatch(&router, state.clone(), actor, &bad)
            .await
            .expect_err("bad mint parameters must be refused");
        assert_eq!(err.code, "fauna.protocol.malformed", "for {bad:?}");
    }

    assert!(
        state
            .db
            .get_atproto_identity(&actor)
            .await
            .unwrap()
            .is_none(),
        "a refused mint must leave no identity row behind"
    );
    assert_eq!(
        state
            .db
            .get_atproto_integration_level(&actor)
            .await
            .unwrap(),
        IntegrationLevel::Off,
        "and must not have moved the level"
    );
}

#[tokio::test]
async fn a_level_change_that_does_not_mint_needs_no_mint_parameters() {
    let (router, state) = router_with_db().await;
    let actor = [8u8; 32];

    // Off → Linked is the one effect-free rung; it carries nothing.
    let reply = dispatch(
        &router,
        state.clone(),
        actor,
        &SetIntegrationLevelRequest {
            target_level: "linked".into(),
            ..Default::default()
        },
    )
    .await
    .expect("Off → Linked needs no parameters");
    assert_eq!(reply.level, "linked");
}

#[tokio::test]
async fn the_history_backfill_opt_in_is_persisted_for_the_projection_loop() {
    // The user's second explicit consent must survive the confirm AND be made
    // operative in the same breath: the transition derives the projection floor
    // (`atproto-pds-bridge.md` § Projection & backfill) that `fetch_public_posts`
    // filters on. Persisting the answer without deriving the floor is what left
    // the ratified forward-only default inverted until S5 slice 2a.
    let (router, state) = router_with_db().await;

    // Opted in → genesis: publish the whole back-catalogue.
    let with_history = [9u8; 32];
    dispatch(
        &router,
        state.clone(),
        with_history,
        &SetIntegrationLevelRequest {
            target_level: "hosted_visible".into(),
            did_method: "plc".into(),
            user_rotation_pub_did_key: ROTATION_KEY.into(),
            history_backfill: true,
            ..Default::default()
        },
    )
    .await
    .expect("enter hosted with history");

    // Declined (the ratified default) → the enable instant: nothing historical.
    let forward_only = [10u8; 32];
    let before = fauna_core::data::Timestamp::now().0 as i64;
    dispatch(
        &router,
        state.clone(),
        forward_only,
        &SetIntegrationLevelRequest {
            target_level: "hosted_visible".into(),
            did_method: "plc".into(),
            user_rotation_pub_did_key: ROTATION_KEY.into(),
            history_backfill: false,
            ..Default::default()
        },
    )
    .await
    .expect("enter hosted forward-only");
    let after = fauna_core::data::Timestamp::now().0 as i64;

    let read = |actor: [u8; 32]| {
        let state = state.clone();
        async move {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT history_backfill, projection_floor_micros
                   FROM atproto_identities WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .expect("read the opt-in + floor")
        }
    };

    let (opt_in, floor) = read(with_history).await;
    assert_eq!(opt_in, 1, "the history opt-in must reach the identity row");
    assert_eq!(floor, 0, "opting in starts the stream at genesis");

    let (opt_in, floor) = read(forward_only).await;
    assert_eq!(opt_in, 0, "the declined answer must reach the identity row");
    assert!(
        floor >= before && floor <= after,
        "forward-only starts the stream at the enable instant, got {floor} \
         outside [{before}, {after}] — a 0 here would publish the whole \
         back-catalogue the user declined"
    );
}

// ── the status read (S4-B) ─────────────────────────────────────────

#[tokio::test]
async fn status_kind_is_a_user_class_replay_safe_read() {
    let (router, _state) = router_with_db().await;
    let meta = router.kind_meta(STATUS_KIND).expect("kind registered");
    assert!(!meta.forbid_replay, "a pure read is replay-safe");

    assert!(is_permitted(CallerClass::User, STATUS_KIND));
    assert!(
        is_permitted(CallerClass::Admin, STATUS_KIND),
        "Admin ⊇ User"
    );
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
    ] {
        assert!(
            !is_permitted(class, STATUS_KIND),
            "{STATUS_KIND} must be denied for {class:?}"
        );
    }
}

#[tokio::test]
async fn the_status_read_tracks_the_ladder_and_keeps_the_deactivated_identity_visible() {
    let (router, state) = router_with_db().await;
    let actor = [10u8; 32];
    common::seed_dispatch_actor(&state.db, &actor).await;

    // Never chose: OFF, no identity. The for_test box is domainless
    // (`handle_domain()` = "localhost"), so the hosted rungs are gated — and
    // the reply names the domain the verdict came from.
    let s = status(&router, state.clone(), actor).await;
    assert_eq!(s.level, "off");
    assert!(s.identity.is_none());
    assert!(!s.hosted_allowed, "localhost is not a public DNS name");
    assert_eq!(s.handle_domain, "localhost");

    // The level is stored USER INTENT: it walks even on a gated box (the UI
    // greys the rung; the roster read is the enforcement point that keeps a
    // non-public domain from ever deriving a servable handle).
    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    let s = status(&router, state.clone(), actor).await;
    assert_eq!(s.level, "hosted_visible");
    let identity = s.identity.expect("a hosted entry records the identity");
    assert_eq!(
        identity.status, "pending",
        "mint owed until the bridge acts"
    );
    assert_eq!(identity.method, "plc");

    // Step down: the identity stays VISIBLE, marked deactivated, so the user
    // sees what re-enabling would restore (`ui/atproto.md` § Errors & edge
    // cases).
    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("step off");
    let s = status(&router, state.clone(), actor).await;
    assert_eq!(s.level, "off");
    let identity = s.identity.expect("the retained identity is still reported");
    assert_eq!(identity.status, "deactivated");
}

#[tokio::test]
async fn the_status_read_derives_preview_and_verdict_from_the_current_domain() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    common::seed_dispatch_actor(&state.db, &actor).await;
    state.db.set_handle(&actor, "alice").await.unwrap();
    state
        .identity_domain
        .store(Some(std::sync::Arc::new("fauna.example".to_string())));

    let s = status(&router, state.clone(), actor).await;
    assert!(s.hosted_allowed, "a registerable domain passes the gate");
    assert_eq!(s.handle_domain, "fauna.example");
    assert_eq!(
        s.handle_preview, "alice.fauna.example",
        "the either-way handle line renders before any identity exists"
    );

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    let s = status(&router, state.clone(), actor).await;
    assert_eq!(
        s.identity.expect("identity summarized").handle,
        "alice.fauna.example",
        "the summary handle is the same read-time derivation as the preview"
    );
}

// ── consent honesty around the mint moment (S4-B hardening) ────────

#[tokio::test]
async fn re_entry_after_a_pre_mint_step_off_owes_the_mint_again_not_a_fake_active() {
    // Enter hosted (mint still pending), step off, re-enter: the row must be
    // `pending` again — `active` with no DID would be a lie the roster
    // serves — and the mint loop (which scans `pending`) re-arms only now,
    // after the user re-consented.
    let (router, state) = router_with_db().await;
    let actor = [12u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("step off pre-mint");
    let row = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .expect("row retained");
    assert_eq!(
        row.status, "deactivated",
        "a pre-mint step-off must stop the mint loop, which scans `pending`"
    );

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("re-enter");
    let row = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "pending", "no DID yet ⇒ the mint is owed again");
    assert_eq!(row.did, None);
}

#[tokio::test]
async fn a_mint_recorded_after_the_user_stepped_off_does_not_resurrect_serving() {
    // The race: the bridge read the roster (row pending), the user stepped
    // off, then the bridge's `record_minted_identity` lands. The DID is
    // recorded (it exists in the world — DID-is-data), but the row must stay
    // `deactivated`: the step-down withdrew serving consent, and only a
    // user's re-entry restores it.
    let (router, state) = router_with_db().await;
    let actor = [13u8; 32];

    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("enter hosted");
    dispatch(&router, state.clone(), actor, &to(IntegrationLevel::Off))
        .await
        .expect("step off pre-mint");

    state
        .db
        .record_atproto_minted(&actor, "did:plc:racedmint", Some("bafycid"))
        .await
        .expect("the late mint record still lands");
    let row = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.did.as_deref(), Some("did:plc:racedmint"));
    assert_eq!(
        row.status, "deactivated",
        "a late mint record must not resurrect a withdrawn consent"
    );

    // Re-entry now reactivates the minted identity — same DID, active.
    dispatch(
        &router,
        state.clone(),
        actor,
        &to(IntegrationLevel::HostedVisible),
    )
    .await
    .expect("re-enter");
    let row = state
        .db
        .get_atproto_identity(&actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "active");
    assert_eq!(row.did.as_deref(), Some("did:plc:racedmint"));
}

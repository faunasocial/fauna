//! The **nest half** of the client-device backup custodian — the third
//! destination kind (`docs/goal/behavior/backup-destinations.md` § State & data shape → *Third
//! destination kind — client device as custodian*;
//! `docs/goal/architecture/message-segment-store.md` § Client-device custodian
//! (pull)).
//!
//! The kind inverts every assumption the v1 peer-nest kind rests on, and each
//! test below pins one of the inversions:
//!
//! * **No address.** A custodian is named by its device, not a URL + pinned nest
//!   id, so registration must not demand one — and must not dial.
//! * **No `NestBackupKey` grant.** A pull-only custodian seals for itself, so an
//!   owner whose only destination is their own device has no coordinator at all.
//!   The status projection must still serve their row.
//! * **The device reports; the nest projects.** `last_upload_time` /
//!   `backlog_count` come from `fauna.backup.custodian.checkin`, and their
//!   meanings invert.
//! * **Only the enrolled device may report.** The nest authenticates the
//!   *owner*, and every one of the owner's devices authenticates identically, so
//!   the check-in names the device speaking and the nest refuses a mismatch.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb`, no mocks).

mod common;
use common::encode;
use common::register_user;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::{
    AUDIT_STATE_FAILED, AUDIT_STATE_OK, CAP_STATE_OK, CAP_STATE_REACHED,
    DESTINATION_KIND_CLIENT_DEVICE, DESTINATION_KIND_NEST,
};
use fauna_nest::{backup_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    RpcError,
    backup::{
        BackupStatusReply, BackupStatusRequest, CustodianCheckinReply, CustodianCheckinRequest,
        DestinationListReply, DestinationListRequest, DestinationRegisterReply,
        DestinationRegisterRequest, DestinationRemoveReply, DestinationRemoveRequest,
    },
    decode_strict as decode,
};

const OWNER: [u8; 32] = [0xC1; 32];
const DEVICE_A: &str = "device-ipad";
const DEVICE_B: &str = "device-laptop";
const DEST: &str = "cust-1";

/// See `conformance_nest_backup.rs` for why the `db_path` override is
/// load-bearing: the status projection opens the in-process coordinator, which
/// refuses to guess a data dir rather than writing backup state relative to CWD.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir
        .path()
        .join("nest.db")
        .to_string_lossy()
        .into_owned();
    std::mem::forget(data_dir); // outlives the test; the OS reaps the tempdir
    let state = Arc::new(AppState {
        config: Arc::new(fauna_nest::config::NestConfig {
            nest: fauna_nest::config::NestSection {
                db_path,
                ..Default::default()
            },
            ..(*AppState::for_test(db.clone()).config).clone()
        }),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    backup_handlers::register_backup_handlers(&mut b);
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

/// Enroll a custodian exactly as `fauna_client_config::enroll_client_custodian`
/// does: kind + device id + cap, and **no address at all**.
async fn register_custodian(
    router: &RpcRouter,
    state: &Arc<AppState>,
    device_id: &str,
    cap: Option<u64>,
) -> Result<DestinationRegisterReply, RpcError> {
    let req = DestinationRegisterRequest {
        destination_id: DEST.into(),
        kind: DESTINATION_KIND_CLIENT_DEVICE.into(),
        custodian_device_id: Some(device_id.into()),
        capacity_cap_bytes: cap,
        ..Default::default()
    };
    let out = dispatch(
        router,
        state.clone(),
        OWNER,
        "fauna.backup.destination.register",
        encode(&req),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn check_in(
    router: &RpcRouter,
    state: &Arc<AppState>,
    device_id: Option<&str>,
    high_water: u64,
    held_bytes: u64,
    cap_state: &str,
) -> Result<CustodianCheckinReply, RpcError> {
    check_in_with_audit(
        router, state, device_id, high_water, held_bytes, cap_state, None, None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn check_in_with_audit(
    router: &RpcRouter,
    state: &Arc<AppState>,
    device_id: Option<&str>,
    high_water: u64,
    held_bytes: u64,
    cap_state: &str,
    audit_state: Option<&str>,
    last_audit_passed_at: Option<u64>,
) -> Result<CustodianCheckinReply, RpcError> {
    let req = CustodianCheckinRequest {
        destination_id: DEST.into(),
        high_water,
        held_bytes,
        cap_state: cap_state.into(),
        device_id: device_id.map(str::to_string),
        audit_state: audit_state.map(str::to_string),
        last_audit_passed_at,
        extra: Default::default(),
    };
    let out = dispatch(
        router,
        state.clone(),
        OWNER,
        "fauna.backup.custodian.checkin",
        encode(&req),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn status(router: &RpcRouter, state: &Arc<AppState>) -> BackupStatusReply {
    let out = dispatch(
        router,
        state.clone(),
        OWNER,
        "fauna.backup.status",
        encode(&BackupStatusRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("status");
    decode(&out).unwrap()
}

async fn list(router: &RpcRouter, state: &Arc<AppState>) -> DestinationListReply {
    let out = dispatch(
        router,
        state.clone(),
        OWNER,
        "fauna.backup.destination.list",
        encode(&DestinationListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("list");
    decode(&out).unwrap()
}

// ── registration ──────────────────────────────────────────────────────────────

/// A custodian registers with **no address at all** — question 1 of the
/// custodian contract, and the first kind for which that is true.
///
/// Before this slice the kind was refused *incidentally*: `parse_hex32` on the
/// empty `destination_nest_id` failed closed before any row was stored. The
/// branch that replaces it must accept the custodian **and** keep refusing an
/// empty nest id on a nest row (pinned below), because deleting the parse alone
/// would reopen the dial-an-empty-URL path.
#[tokio::test]
async fn a_custodian_registers_without_an_address() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;

    let reply = register_custodian(&router, &state, DEVICE_A, Some(64 << 30))
        .await
        .expect("a custodian has no address to supply");
    assert!(reply.ok);

    let rows = state.db.list_backup_destinations(&OWNER).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, DESTINATION_KIND_CLIENT_DEVICE);
    assert_eq!(rows[0].custodian_device_id.as_deref(), Some(DEVICE_A));
    assert_eq!(rows[0].capacity_cap_bytes, Some(64 << 30));
    assert!(
        rows[0].nest_url.is_empty() && rows[0].nest_id.is_empty(),
        "nothing about a custodian row may look dialable"
    );
}

/// The kind branch must not weaken the nest arm: a nest destination's 32-byte
/// id is what the federation handshake pins as its expected peer.
#[tokio::test]
async fn a_nest_destination_still_needs_its_pinned_nest_id() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;

    let req = DestinationRegisterRequest {
        destination_id: "dest-nest".into(),
        destination_nest_url: "https://dest.example/".into(),
        destination_nest_id: String::new(),
        ..Default::default()
    };
    let err = dispatch(
        &router,
        state.clone(),
        OWNER,
        "fauna.backup.destination.register",
        encode(&req),
    )
    .await
    .expect_err("an addressless NEST row must still be refused");
    let _ = err;
    assert!(
        state
            .db
            .list_backup_destinations(&OWNER)
            .await
            .unwrap()
            .is_empty(),
        "nothing may rest as a row the coordinator would later dial"
    );
}

/// A `client-device` row with no device id projects `Inert` — a destination the
/// user sees in their list and that nothing can ever drive.
#[tokio::test]
async fn a_custodian_without_a_device_id_is_refused() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;

    let req = DestinationRegisterRequest {
        destination_id: DEST.into(),
        kind: DESTINATION_KIND_CLIENT_DEVICE.into(),
        custodian_device_id: None,
        ..Default::default()
    };
    dispatch(
        &router,
        state.clone(),
        OWNER,
        "fauna.backup.destination.register",
        encode(&req),
    )
    .await
    .expect_err("a device-less custodian row is unrepresentable, not storable");
    assert!(
        state
            .db
            .list_backup_destinations(&OWNER)
            .await
            .unwrap()
            .is_empty()
    );
}

/// ⚠ The read-back the **desktop custodian host** polls to discover its own
/// assignment (`apps/sync-agent.md` § Control plane split — *policy through the
/// nest, never over local IPC*). It is bearer-only and cannot open the at-rest
/// config row, so a hard-coded `kind: "nest"` here left every enrolled custodian
/// silently unhosted.
#[tokio::test]
async fn the_list_read_back_serves_the_stored_per_kind_columns() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, Some(4_096))
        .await
        .unwrap();

    let reply = list(&router, &state).await;
    assert_eq!(reply.destinations.len(), 1);
    let row = &reply.destinations[0];
    assert_eq!(row.kind, DESTINATION_KIND_CLIENT_DEVICE);
    assert_eq!(row.custodian_device_id.as_deref(), Some(DEVICE_A));
    assert_eq!(row.capacity_cap_bytes, Some(4_096));

    // …and it feeds the shared matching rule both hosts use, so the device that
    // enrolled is the device that finds work here.
    let assignment = fauna_core::data::custodian_assignment_for(
        reply.destinations.iter().map(|d| d.custodian_row()),
        DEVICE_A,
    )
    .expect("the enrolled device finds its own assignment");
    assert_eq!(assignment.destination_id, DEST);
    assert_eq!(assignment.capacity_cap_bytes, Some(4_096));
    assert_eq!(assignment.device_id, DEVICE_A);
    assert!(
        fauna_core::data::custodian_assignment_for(
            reply.destinations.iter().map(|d| d.custodian_row()),
            DEVICE_B,
        )
        .is_none(),
        "a device with no row must find none, never someone else's"
    );
}

// ── the check-in and its refusal ──────────────────────────────────────────────

/// The enrolled device's check-in is recorded.
#[tokio::test]
async fn the_enrolled_device_can_check_in() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();

    let reply = check_in(&router, &state, Some(DEVICE_A), 3, 2_048, CAP_STATE_OK)
        .await
        .expect("the enrolled device may report its own progress");
    assert!(reply.ok);

    let stored = state
        .db
        .get_custodian_checkin(&OWNER, DEST)
        .await
        .unwrap()
        .expect("recorded");
    assert_eq!(stored.high_water, 3);
    assert_eq!(stored.held_bytes, 2_048);
}

/// ⚠ **The security-relevant refusal.** Every one of the owner's devices
/// authenticates as the same actor, so without this any of them could report
/// progress on another's behalf — and since the same
/// `(owner, destination_id, device)` triple decides who *pulls* as well as who
/// reports, a wrong answer here puts the wrong device to work on the owner's
/// whole corpus. The row must be left exactly as the enrolled device left it.
#[tokio::test]
async fn a_check_in_from_another_device_is_refused_and_changes_nothing() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 5, 1_000, CAP_STATE_OK)
        .await
        .unwrap();

    let refused = check_in(
        &router,
        &state,
        Some(DEVICE_B),
        99,
        9_999,
        CAP_STATE_REACHED,
    )
    .await
    .expect_err("a device may not report on another device's destination");
    assert_eq!(
        refused.code,
        RpcError::CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED,
        "the refusal is the typed not-assigned answer, so a host re-reads its \
         assignment instead of pulling on for a row that is not its own"
    );

    let stored = state
        .db
        .get_custodian_checkin(&OWNER, DEST)
        .await
        .unwrap()
        .expect("the enrolled device's row survives");
    assert_eq!(
        stored.high_water, 5,
        "the impostor's figures were not stored"
    );
    assert_eq!(stored.held_bytes, 1_000);
    assert_eq!(stored.cap_state, CAP_STATE_OK);
}

/// ⚠ **The composition neither sibling test covers.** Refusal-on-mismatch is
/// pinned above and acceptance-on-absence was pinned below; nothing pinned what
/// happens when an impostor simply *declines to name itself*, which is the only
/// move it needs. Before the custodian-row refusal was ratified
/// (`behavior/backup-destinations.md` § Custodian contract → *Naming yourself*), dropping the one
/// field walked straight past the guard: the anonymous caller's figures landed
/// on the enrolled device's row.
#[tokio::test]
async fn an_anonymous_check_in_cannot_overwrite_the_enrolled_devices_row() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 5, 1_000, CAP_STATE_OK)
        .await
        .unwrap();

    check_in(&router, &state, None, 99, 9_999, CAP_STATE_REACHED)
        .await
        .expect_err("a caller that will not name itself may not report at all");

    let stored = state
        .db
        .get_custodian_checkin(&OWNER, DEST)
        .await
        .unwrap()
        .expect("the enrolled device's row survives");
    assert_eq!(
        stored.high_water, 5,
        "the anonymous caller's figures were not stored"
    );
    assert_eq!(stored.held_bytes, 1_000);
    assert_eq!(stored.cap_state, CAP_STATE_OK);
}

/// ⚠ **Why the composition above is worth a refusal rather than a shrug.** The
/// carry-forward in `put_custodian_checkin` makes an audit verdict *sticky* on
/// purpose — an absent verdict means "nothing new to say", so a rotten store
/// cannot appear to recover on the next silent pass. Composed with an
/// unauthenticated writer that reaches the same column, stickiness inverts: one
/// anonymous `ok` outlives the enrolled device's `failed` and every later
/// debounced pass carries the planted verdict forward. The rot alarm is silenced
/// by the mechanism built to keep it ringing.
#[tokio::test]
async fn an_anonymous_check_in_cannot_plant_a_sticky_healthy_audit_verdict() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();

    // The enrolled device reports its own store has rotted.
    check_in_with_audit(
        &router,
        &state,
        Some(DEVICE_A),
        5,
        1_000,
        CAP_STATE_OK,
        Some(AUDIT_STATE_FAILED),
        None,
    )
    .await
    .unwrap();

    // An anonymous caller claims health.
    check_in_with_audit(
        &router,
        &state,
        None,
        6,
        1_000,
        CAP_STATE_OK,
        Some(AUDIT_STATE_OK),
        None,
    )
    .await
    .expect_err("an unnamed caller may not speak to the audit column");

    // The enrolled device's next ordinary pass carries no fresh verdict.
    check_in(&router, &state, Some(DEVICE_A), 7, 1_000, CAP_STATE_OK)
        .await
        .unwrap();

    let stored = state
        .db
        .get_custodian_checkin(&OWNER, DEST)
        .await
        .unwrap()
        .expect("recorded");
    assert_eq!(
        stored.audit_state.as_deref(),
        Some(AUDIT_STATE_FAILED),
        "the planted verdict must not survive the debounced pass as the row's truth"
    );
}

/// A check-in naming a destination that was never registered is refused rather
/// than silently accepted — otherwise the store would accumulate progress rows
/// for destinations no status read can ever describe.
#[tokio::test]
async fn a_check_in_for_an_unregistered_destination_is_refused() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;

    let refused = check_in(&router, &state, Some(DEVICE_A), 1, 1, CAP_STATE_OK)
        .await
        .expect_err("no such destination");
    assert_eq!(refused.code, RpcError::CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED);
    assert_eq!(
        state.db.get_custodian_checkin(&OWNER, DEST).await.unwrap(),
        None
    );
}

/// The removal a running custodian actually meets: its destination existed,
/// the owner removed it, and the device's next pass checks in against nothing.
/// The refusal must be the typed not-assigned code, never the generic malformed
/// one: it is what lets the desktop host stop the dead stint at that pass
/// instead of pulling on for up to one rediscovery interval (`sync-agent.md`
/// § A7).
#[tokio::test]
async fn a_check_in_after_the_destination_was_removed_is_refused_as_not_assigned() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 5, 1_000, CAP_STATE_OK)
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        OWNER,
        "fauna.backup.destination.remove",
        encode(&DestinationRemoveRequest {
            destination_id: DEST.into(),
            extra: Default::default(),
        }),
    )
    .await
    .unwrap();

    let refused = check_in(&router, &state, Some(DEVICE_A), 6, 1_000, CAP_STATE_OK)
        .await
        .expect_err("the destination is gone");
    assert_eq!(refused.code, RpcError::CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED);
}

/// A peer nest does not check in — it is pushed to. Accepting one would let a
/// nest row acquire custodian semantics its status projection never reads.
#[tokio::test]
async fn a_check_in_against_a_nest_destination_is_refused() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    state
        .db
        .put_backup_destination(&OWNER, DEST, "https://d.example", &[7u8; 32])
        .await
        .unwrap();

    check_in(&router, &state, Some(DEVICE_A), 1, 1, CAP_STATE_OK)
        .await
        .expect_err("a nest destination is pushed to, never checked in for");
}

/// ⚠ **This test was inverted when the custodian-row refusal was ratified**
/// (`behavior/backup-destinations.md` § Custodian contract → *Naming yourself*). It used to assert
/// that absence is accepted, on the general additive-everywhere rule — but that
/// made the guard something the *caller* opted into, and the two composition
/// tests above show what walked through.
///
/// The exemption is structural, which is why it is safe to invert: a
/// client-device row cannot exist without a non-blank `custodian_device_id`
/// (pinned by `a_custodian_without_a_device_id_is_refused`), and no nest ever
/// *served* this kind with a request type lacking `device_id` — the handler and
/// the field landed in the same commit, the kind having been registered a day
/// earlier with no handler behind it. So the client this test used to protect
/// cannot exist: any caller old enough to omit the field is also old enough that
/// every nest it could reach refused the kind outright.
#[tokio::test]
async fn a_check_in_without_a_device_id_is_refused_because_none_can_exist() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();

    check_in(&router, &state, None, 2, 512, CAP_STATE_OK)
        .await
        .expect_err("a check-in that names no device is refused, not recorded");
}

// ── the inverted status row ───────────────────────────────────────────────────

/// ⚠ The defect this slice's status restructuring exists for.
///
/// A pull-only custodian grants **no** `NestBackupKey` (`behavior/backup-destinations.md` § Third
/// destination kind), so an owner whose only destination is their own device has
/// no coordinator — and the status handler used to return early on that, giving
/// them an empty destination list forever no matter how many check-ins arrived.
/// `enrolled` keeps its ratified meaning (a stored grant) and is honestly
/// `false` here; the row is served anyway.
#[tokio::test]
async fn a_custodian_only_owner_gets_a_status_row_despite_granting_no_key() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, Some(8_192))
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 4, 3_000, CAP_STATE_OK)
        .await
        .unwrap();

    let reply = status(&router, &state).await;
    assert!(
        !reply.enrolled,
        "`enrolled` means a stored NestBackupKey grant, which this owner \
         correctly does not have"
    );
    assert_eq!(
        reply.destinations.len(),
        1,
        "the custodian's row must be served even with no coordinator"
    );
    let row = &reply.destinations[0];
    assert_eq!(row.destination_id, DEST);
    assert_eq!(row.held_bytes, Some(3_000));
    assert_eq!(row.cap_state.as_deref(), Some(CAP_STATE_OK));
    assert!(
        row.last_upload_time.is_some(),
        "the device reported itself level with an empty nest, so it is caught up"
    );
}

/// A custodian that has never checked in reports every field **absent** rather
/// than zeroed — which is what lets the render tell "not applicable" from
/// "zero". `cap_state` in particular must not be synthesized to `OK`: the shared
/// usage label reads cap-reached from that field alone, so a fabricated "ok"
/// would be the nest asserting a healthy cap state for a device that has said
/// nothing.
#[tokio::test]
async fn a_custodian_that_never_checked_in_reports_absence_not_zero() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();

    let reply = status(&router, &state).await;
    let row = &reply.destinations[0];
    assert_eq!(
        row.last_upload_time, None,
        "never synced is not 'synced at 0'"
    );
    assert_eq!(row.held_bytes, None, "holds-nothing-known is not 'holds 0'");
    assert_eq!(
        row.cap_state, None,
        "a cap state must be reported by the device, never invented by the nest"
    );
    assert_eq!(
        row.audit_state, None,
        "a never-audited custodian is not a FAILING one — and it is not a \
         passing one either; the nest must not invent either verdict"
    );
    assert_eq!(row.last_audit_passed_at, None);
}

/// Cap-reached is a **distinct** state, not a flavour of lag: a custodian at its
/// cap has *stopped pulling*, and flattening that into `backlog_count` would
/// render a stopped backup as one that is merely behind.
#[tokio::test]
async fn a_capped_custodian_reports_cap_reached_distinctly() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, Some(1_000))
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 2, 1_000, CAP_STATE_REACHED)
        .await
        .unwrap();

    let reply = status(&router, &state).await;
    let row = &reply.destinations[0];
    assert_eq!(row.cap_state.as_deref(), Some(CAP_STATE_REACHED));
    assert_eq!(row.held_bytes, Some(1_000));
}

/// `backlog_count = head − high_water` must **saturate**. A custodian can
/// legitimately sit ahead of this nest's head (its pass sealed segments the nest
/// has since compacted away), and an underflow there would render as a backlog
/// of four billion.
#[tokio::test]
async fn a_custodian_ahead_of_the_nest_reports_no_backlog_not_an_underflow() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    // The nest holds no segments at all, so its head is 0 while the device
    // reports having sealed 9.
    check_in(&router, &state, Some(DEVICE_A), 9, 100, CAP_STATE_OK)
        .await
        .unwrap();

    let reply = status(&router, &state).await;
    assert_eq!(
        reply.destinations[0].backlog_count, 0,
        "ahead of the nest is not a four-billion backlog"
    );
}

/// Removing a destination takes its check-in with it: a `destination_id` the
/// owner later re-uses for a *different* device would otherwise inherit the old
/// device's progress, and a brand-new custodian holding nothing would render as
/// already caught up.
#[tokio::test]
async fn removing_a_custodian_drops_the_progress_a_re_enrolment_would_inherit() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 7, 5_000, CAP_STATE_OK)
        .await
        .unwrap();

    let out = dispatch(
        &router,
        state.clone(),
        OWNER,
        "fauna.backup.destination.remove",
        encode(&DestinationRemoveRequest {
            destination_id: DEST.into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("remove");
    let reply: DestinationRemoveReply = decode(&out).unwrap();
    assert!(reply.removed);

    // Re-enrol the SAME destination id for a different device.
    register_custodian(&router, &state, DEVICE_B, None)
        .await
        .unwrap();
    let reply = status(&router, &state).await;
    let row = &reply.destinations[0];
    assert_eq!(
        row.last_upload_time, None,
        "the new device has never checked in; it must not inherit the old one's"
    );
    assert_eq!(row.held_bytes, None);
}

/// ⚠ **The push coordinator must never see a custodian row.**
///
/// Question 2 of the custodian contract: a nest is *nest-driven push*, a client
/// device is *destination-driven pull*. The coordinator's pass hands every
/// destination it holds to the federation dial — and a custodian row carries an
/// empty URL and an empty nest id, so a coordinator that kept it would dial an
/// empty URL on every tick, forever.
///
/// The filter is written as "is a nest", not "is not a client device", so an
/// unknown *future* kind is excluded too: a build that does not understand a
/// kind must not dial it.
#[tokio::test]
async fn the_push_coordinator_holds_the_nest_row_and_never_the_custodian() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;

    // The grant is what makes a coordinator openable at all.
    state
        .db
        .put_nest_backup_key(&OWNER, &[3u8; 32])
        .await
        .unwrap();
    state
        .db
        .put_backup_destination(&OWNER, "dest-nest", "https://d.example", &[7u8; 32])
        .await
        .unwrap();
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();

    let coordinator =
        fauna_nest::segment_backup::NestBackupCoordinator::open_for_owner(state.clone(), OWNER)
            .await
            .unwrap()
            .expect("the grant makes a coordinator openable");
    let held: Vec<_> = coordinator
        .destinations()
        .iter()
        .map(|d| d.destination_id.as_str())
        .collect();
    assert_eq!(
        held,
        ["dest-nest"],
        "the coordinator must hold only dialable peer nests"
    );
}

/// ⚠ The custodian's **self-audit verdict reaches the owner's other devices**.
///
/// § Custodian contract question 4 ratifies that the owner's clients must be
/// able to verify the destination holds what it claims — and for this kind the
/// answer is that the custodian audits *itself* and its check-in feeds the nest
/// projection, because inclusion-sampling a sleeping device is impossible. A
/// rotting store keeps advancing `high_water` and reporting `CAP_STATE_OK`, so
/// without this pass-through the one failure mode the audit exists to catch
/// renders as a perfectly healthy row.
#[tokio::test]
async fn a_failing_self_audit_reaches_the_owners_status_row() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in_with_audit(
        &router,
        &state,
        Some(DEVICE_A),
        3,
        900,
        CAP_STATE_OK,
        Some(AUDIT_STATE_FAILED),
        Some(1_700_000_000),
    )
    .await
    .unwrap();

    let reply = status(&router, &state).await;
    let row = &reply.destinations[0];
    assert_eq!(
        row.audit_state.as_deref(),
        Some(AUDIT_STATE_FAILED),
        "a rotten store must not render as healthy just because it keeps pulling"
    );
    assert_eq!(row.last_audit_passed_at, Some(1_700_000_000));
    assert_eq!(
        row.cap_state.as_deref(),
        Some(CAP_STATE_OK),
        "the audit verdict is a SEPARATE axis from the cap state"
    );
}

/// The audit is debounced to its own interval, so most passes carry no fresh
/// verdict. An absent verdict means *"nothing new to say"* — never "healthy"
/// — so the last one is carried forward. Clearing it would let a rotten store
/// appear to recover on the very next silent pass.
#[tokio::test]
async fn a_check_in_with_no_fresh_verdict_keeps_the_last_one() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in_with_audit(
        &router,
        &state,
        Some(DEVICE_A),
        1,
        100,
        CAP_STATE_OK,
        Some(AUDIT_STATE_FAILED),
        None,
    )
    .await
    .unwrap();
    // A later pass that ran no audit at all.
    check_in(&router, &state, Some(DEVICE_A), 2, 200, CAP_STATE_OK)
        .await
        .unwrap();

    let reply = status(&router, &state).await;
    assert_eq!(
        reply.destinations[0].audit_state.as_deref(),
        Some(AUDIT_STATE_FAILED),
        "a silent pass is not a passing audit"
    );
}

/// A custodian that has audited and passed says so — the healthy direction, so
/// the carried-forward rule above cannot be satisfied by simply never clearing.
#[tokio::test]
async fn a_passing_self_audit_is_reported_as_passing() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in_with_audit(
        &router,
        &state,
        Some(DEVICE_A),
        1,
        100,
        CAP_STATE_OK,
        Some(AUDIT_STATE_OK),
        Some(1_700_000_500),
    )
    .await
    .unwrap();

    let reply = status(&router, &state).await;
    assert_eq!(
        reply.destinations[0].audit_state.as_deref(),
        Some(AUDIT_STATE_OK)
    );
}

/// Both kinds coexist in one owner's reply, each with its own semantics — the
/// row shape is uniform, the meanings invert.
#[tokio::test]
async fn a_nest_row_and_a_custodian_row_coexist_in_one_status_reply() {
    let (router, state) = router_and_state().await;
    register_user(&state, OWNER, "owner").await;
    state
        .db
        .put_backup_destination(&OWNER, "dest-nest", "https://d.example", &[7u8; 32])
        .await
        .unwrap();
    register_custodian(&router, &state, DEVICE_A, None)
        .await
        .unwrap();
    check_in(&router, &state, Some(DEVICE_A), 1, 64, CAP_STATE_OK)
        .await
        .unwrap();

    let reply = list(&router, &state).await;
    assert_eq!(reply.destinations.len(), 2);
    assert!(
        reply
            .destinations
            .iter()
            .any(|d| d.kind == DESTINATION_KIND_NEST),
        "the peer nest row keeps its kind"
    );

    let reply = status(&router, &state).await;
    let custodian = reply
        .destinations
        .iter()
        .find(|d| d.destination_id == DEST)
        .expect("the custodian row is present");
    assert_eq!(custodian.held_bytes, Some(64));
    // The nest row is projected by the coordinator, which this owner has not
    // enrolled — so only the custodian row is served, and that is honest.
    assert!(!reply.enrolled);
}

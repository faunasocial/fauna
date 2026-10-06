//! The `backup-upload` task-delegation kind is nest-run — the closing move of
//! the segment-backup slice-5 flip (`docs/goal/behavior/backup-restore.md`
//! § Background Tasks → *Flip status (slice 5)*;
//! `docs/goal/behavior/participants.md` § Task delegation → *Policy order* +
//! *The nest as runner*).
//!
//! **The defect these pin.** The flip made the source nest the segment-backup
//! writer (`NestBackupWorker` sweeps every enrolled owner × destination with no
//! client or agent awake) and deleted linux's in-app upload driver. But nothing
//! on either side claimed the `backup-upload` lease, so the owner's Task
//! delegation page rendered the kind as **waiting, with no runner**, while the
//! nest was demonstrably backing them up. The row lied.
//!
//! These assert the *client-visible observable* — what `fauna.delegation.observe`
//! actually returns over the wire, which is what the page renders from — rather
//! than the in-process lease registry the unit tests in
//! `delegation_runner.rs` cover. The registry can be right while the projection
//! a client reads is wrong; only this level catches that.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState`, real `CacheDb`, real handlers and
//! real router dispatch — no mocks).

mod common;
use common::encode;
use common::register_user;

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_core::delegation::{KIND_BACKUP_UPLOAD, ParticipantClass};
use fauna_nest::delegation_runner::{HeldLeases, nest_self_ref, run_pass};
use fauna_nest::{
    backup_handlers, db::CacheDb, delegation_handlers, routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError,
    backup::{
        DestinationRegisterReply, DestinationRegisterRequest, DestinationRemoveReply,
        DestinationRemoveRequest, NestKeyGrantReply, NestKeyGrantRequest, NestKeyRevokeReply,
        NestKeyRevokeRequest,
    },
    decode_strict as decode,
    delegation::{ObserveReply, ObserveRequest},
};

/// `AppState::for_test` + a router carrying the backup and delegation handlers.
/// The `db_path` override is load-bearing for the same reason as in
/// `conformance_nest_backup.rs`: `fauna.backup.status` projects through the
/// in-process coordinator, which keeps per-owner state under the nest data dir.
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
    delegation_handlers::register_delegation_handlers(&mut b);
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

async fn grant_seal_key(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.nest_key.grant",
        encode(&NestKeyGrantRequest {
            nest_backup_key: ByteBuf::from(vec![9u8; 32]),
            extra: Default::default(),
        }),
    )
    .await
    .expect("grant succeeds");
    let reply: NestKeyGrantReply = decode(&out).unwrap();
    assert!(reply.ok);
}

async fn revoke_seal_key(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.nest_key.revoke",
        encode(&NestKeyRevokeRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("revoke succeeds");
    let reply: NestKeyRevokeReply = decode(&out).unwrap();
    assert!(reply.revoked);
}

async fn register_destination(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    destination_id: &str,
) {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.destination.register",
        encode(&DestinationRegisterRequest {
            destination_id: destination_id.to_string(),
            destination_nest_url: "wss://dest.example".to_string(),
            destination_nest_id: hex::encode([5u8; 32]),
            ..Default::default()
        }),
    )
    .await
    .expect("register succeeds");
    let reply: DestinationRegisterReply = decode(&out).unwrap();
    assert!(reply.ok);
}

async fn remove_destination(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    destination_id: &str,
) {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.destination.remove",
        encode(&DestinationRemoveRequest {
            destination_id: destination_id.to_string(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("remove succeeds");
    let reply: DestinationRemoveReply = decode(&out).unwrap();
    assert!(reply.removed);
}

/// The read the Task-delegation page makes.
async fn observe_backup_upload(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> ObserveReply {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.delegation.observe",
        encode(&ObserveRequest {
            task_kinds: vec![KIND_BACKUP_UPLOAD.to_string()],
            extra: Default::default(),
        }),
    )
    .await
    .expect("observe succeeds");
    decode(&out).unwrap()
}

#[tokio::test]
async fn an_enrolled_owner_sees_the_nest_as_the_backup_upload_runner() {
    let (router, state) = router_and_state().await;
    let owner = [0x11u8; 32];
    register_user(&state, owner, "owner").await;

    // Before enrolling: nothing runs the kind, and the page correctly says so.
    let mut held = HeldLeases::default();
    run_pass(&state, &mut held).await;
    assert!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .is_empty(),
        "an owner with no seal grant and no destination has no runner — and that is honest"
    );

    // Enrolling is two calls; sufficiency needs BOTH, so the kind stays
    // un-run after the first one.
    grant_seal_key(&router, &state, owner).await;
    run_pass(&state, &mut held).await;
    assert!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .is_empty(),
        "a seal grant with nowhere to send backups is not sufficiency"
    );

    register_destination(&router, &state, owner, "d-1").await;
    run_pass(&state, &mut held).await;

    // The assertion this whole track exists for.
    let leases = observe_backup_upload(&router, &state, owner).await.leases;
    assert_eq!(leases.len(), 1, "the kind now has a runner");
    assert_eq!(leases[0].task_kind, KIND_BACKUP_UPLOAD);
    assert_eq!(
        leases[0].holder,
        nest_self_ref(&state),
        "the runner is this nest — the same actor pubkey the client correlates \
         against the nest it is talking to"
    );
    assert_eq!(
        leases[0].holder_class,
        ParticipantClass::AlwaysOnNest,
        "reported as an always-on nest, which is what makes it tier-1 to a client"
    );
}

#[tokio::test]
async fn revoking_the_seal_grant_releases_the_lease() {
    // participants.md:59 — holding the grant IS the assignment, so a revoke
    // un-assigns *promptly* rather than letting the lease age out.
    let (router, state) = router_and_state().await;
    let owner = [0x22u8; 32];
    register_user(&state, owner, "owner2").await;
    grant_seal_key(&router, &state, owner).await;
    register_destination(&router, &state, owner, "d-1").await;

    let mut held = HeldLeases::default();
    run_pass(&state, &mut held).await;
    assert_eq!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .len(),
        1
    );

    revoke_seal_key(&router, &state, owner).await;
    run_pass(&state, &mut held).await;
    assert!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .is_empty(),
        "the revoke frees the lease on the next pass, not after LEASE_STALE_MS"
    );
}

#[tokio::test]
async fn removing_the_last_destination_releases_the_lease() {
    let (router, state) = router_and_state().await;
    let owner = [0x33u8; 32];
    register_user(&state, owner, "owner3").await;
    grant_seal_key(&router, &state, owner).await;
    register_destination(&router, &state, owner, "d-1").await;
    register_destination(&router, &state, owner, "d-2").await;

    let mut held = HeldLeases::default();
    run_pass(&state, &mut held).await;
    assert_eq!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .len(),
        1
    );

    // One of two: still sufficient, still running.
    remove_destination(&router, &state, owner, "d-1").await;
    run_pass(&state, &mut held).await;
    assert_eq!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .len(),
        1,
        "one destination is still somewhere to back up to"
    );

    // The last one: nowhere left to send, so the nest stops claiming to run it.
    remove_destination(&router, &state, owner, "d-2").await;
    run_pass(&state, &mut held).await;
    assert!(
        observe_backup_upload(&router, &state, owner)
            .await
            .leases
            .is_empty(),
        "with no destination the nest backs nothing up, and must not claim otherwise"
    );
}

#[tokio::test]
async fn one_owners_enrollment_never_runs_another_owners_kind() {
    // The lease is per `(owner, kind)`; the sufficiency scan is per owner. A
    // shared box must not leak one user's enrollment into another's page.
    let (router, state) = router_and_state().await;
    let enrolled = [0x44u8; 32];
    let bystander = [0x55u8; 32];
    register_user(&state, enrolled, "enrolled").await;
    register_user(&state, bystander, "bystander").await;

    grant_seal_key(&router, &state, enrolled).await;
    register_destination(&router, &state, enrolled, "d-1").await;

    let mut held = HeldLeases::default();
    run_pass(&state, &mut held).await;

    assert_eq!(
        observe_backup_upload(&router, &state, enrolled)
            .await
            .leases
            .len(),
        1
    );
    assert!(
        observe_backup_upload(&router, &state, bystander)
            .await
            .leases
            .is_empty(),
        "the bystander enrolled nothing and must see no runner"
    );
}

//! Production-flow round-trips for **exclusive editing** — the folder lease's
//! client-facing contract (`docs/goal/behavior/file-sync.md` § Exclusive
//! editing, ratified 2026-09-21).
//!
//! Covered:
//! - the owner's standing `exclusive_editing` choice round-trips, and rides
//!   **both** projection arms (a writer member's seat must know to take the
//!   lease; a reader member must know why the folder is read-only);
//! - a reserved `__` rail refuses the flag;
//! - `FolderSummary.lease` reports the live holder — on both arms, so a member
//!   who can never call `lease.acquire` still learns who has the folder;
//! - an **expired** row is not projected, which is the arm with the nastiest
//!   failure mode: leases are swept lazily, so a stale row left behind by a
//!   holder that went away would otherwise tell every seat the folder is locked
//!   forever, and nothing would ever correct it;
//! - the choice and the holder are **independent** — turning exclusive editing
//!   off does not yank a lease out from under a device mid-upload.
//!
//! What this file deliberately does NOT do is call `fauna.folders.lease.acquire`
//! to *find out* whether a folder is free. That is the whole reason
//! `FolderSummary.lease` exists: asking TAKES the lease, and the kind is gated
//! on the writable-folder resolver so a reader member cannot ask at all. The
//! acquire kind's own refusal semantics are pinned end-to-end by
//! `tests/e2e-unified/tests/api/test_folder_exclusive_lease.py`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    folders::{
        FolderCreateRequest, FolderSummary, FolderUpdateRequest, FoldersListReply,
        FoldersListRequest, LeaseAcquireRequest, LeaseReleaseRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    (b.build(), state)
}

const ACTOR: [u8; 32] = [41u8; 32];
const DEVICE_A: [u8; 32] = [0xAAu8; 32];
const DEVICE_B: [u8; 32] = [0xBBu8; 32];

async fn create_set(router: &RpcRouter, state: &Arc<AppState>, name: &str) {
    dispatch(
        router,
        Arc::clone(state),
        ACTOR,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: name.into(),
            retention_policy: None,
            ..Default::default()
        }),
    )
    .await
    .expect("create ok");
}

async fn update(
    router: &RpcRouter,
    state: &Arc<AppState>,
    req: FolderUpdateRequest,
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        Arc::clone(state),
        ACTOR,
        "fauna.folders.update",
        encode(&req),
    )
    .await
}

fn exclusive_update(name: &str, on: bool) -> FolderUpdateRequest {
    FolderUpdateRequest {
        name: name.into(),
        exclusive_editing: Some(on),
        ..Default::default()
    }
}

async fn listed(router: &RpcRouter, state: &Arc<AppState>, name: &str) -> FolderSummary {
    let reply: FoldersListReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            ACTOR,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    reply
        .folders
        .into_iter()
        .find(|s| s.name == name)
        .expect("set listed")
}

/// The owner's standing choice round-trips and defaults off.
#[tokio::test]
async fn the_exclusive_editing_choice_round_trips_and_defaults_off() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    create_set(&router, &state, "vault").await;

    assert!(
        !listed(&router, &state, "vault").await.exclusive_editing,
        "a fresh folder is un-governed — the default a folder nobody governed rests at"
    );

    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("the flip commits");
    assert!(listed(&router, &state, "vault").await.exclusive_editing);

    update(&router, &state, exclusive_update("vault", false))
        .await
        .expect("the flip back commits");
    assert!(!listed(&router, &state, "vault").await.exclusive_editing);

    // An update that does not name the field leaves it alone — the wire-additive
    // reading. Without this an old client's unrelated save would silently
    // un-govern a folder its owner locked.
    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("re-arm");
    update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "vault".into(),
            conflict_policy: Some("latest_wins_always".into()),
            ..Default::default()
        },
    )
    .await
    .expect("an unrelated save commits");
    assert!(
        listed(&router, &state, "vault").await.exclusive_editing,
        "absent on the wire ⇒ unchanged, never off"
    );
}

/// A reserved `__` rail is nest-side infrastructure with no user at a keyboard —
/// there are no two devices to coordinate, and a lease could only wedge it.
#[tokio::test]
async fn a_reserved_rail_refuses_exclusive_editing() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    state.db.create_folder("__config", &ACTOR).await.unwrap();

    let err = update(&router, &state, exclusive_update("__config", true))
        .await
        .expect_err("a rail refuses the flag");
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
    // The refusal text rides `details` as a CBOR string — the debug render is
    // enough to name which refusal fired (the idiom `conformance_folder_residency`
    // uses; `RpcError` has no `Display`).
    let detail = err
        .details
        .as_deref()
        .map(|v| format!("{v:?}"))
        .unwrap_or_default();
    assert!(
        detail.contains("reserved"),
        "the refusal must name the reserved-folder guard that fired, got: {detail}"
    );

    // The reserved-folder guard now refuses EVERY field of a rail, OFF included
    // (the folders mode contraction): the flag can never have been turned on,
    // so there is nothing for an off to undo.
    assert!(
        update(&router, &state, exclusive_update("__config", false))
            .await
            .is_err(),
        "a rail takes no update at all"
    );
}

/// `FolderSummary.lease` reports the live holder, and stops reporting on release.
#[tokio::test]
async fn the_projection_reports_the_live_holder_and_clears_on_release() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    create_set(&router, &state, "vault").await;
    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("arm");
    let folder_id = listed(&router, &state, "vault").await.id;

    assert!(
        listed(&router, &state, "vault").await.lease.is_none(),
        "nobody holds a freshly-armed folder"
    );

    assert!(
        state
            .db
            .try_acquire_upload_lease(folder_id, &ACTOR, &DEVICE_A, 300)
            .await
            .unwrap()
    );
    let held = listed(&router, &state, "vault").await.lease.expect("held");
    assert_eq!(
        held.device_id,
        hex::encode(DEVICE_A),
        "the projection names the holder — this is where a refused seat reads it, \
         never the payload-less `conflict` error's detail string"
    );
    assert!(
        held.expires_at > 0,
        "and says until when, so a client can render a lease that is about to lapse"
    );

    state
        .db
        .release_upload_lease(folder_id, &ACTOR, &DEVICE_A)
        .await
        .unwrap();
    assert!(
        listed(&router, &state, "vault").await.lease.is_none(),
        "a released folder is free again"
    );
}

/// An EXPIRED row must not be projected.
///
/// Leases are swept lazily — only `try_acquire_upload_lease` deletes them, and
/// only for the folder it was asked about — so a folder nobody has tried to
/// acquire since its holder went away still carries a dead row. Projecting it
/// would tell every seat the folder is locked by a device that let go hours ago,
/// and no later read would ever correct it.
#[tokio::test]
async fn an_expired_lease_is_not_projected() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    create_set(&router, &state, "vault").await;
    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("arm");
    let folder_id = listed(&router, &state, "vault").await.id;

    // A lease that lapsed the moment it was taken — the state a crashed holder
    // leaves behind, reachable here without waiting out a real TTL.
    assert!(
        state
            .db
            .try_acquire_upload_lease(folder_id, &ACTOR, &DEVICE_A, -1)
            .await
            .unwrap()
    );
    assert!(
        listed(&router, &state, "vault").await.lease.is_none(),
        "an expired row is dead state, not a holder — the read filters on \
         `expires_at`, it does not trust the table"
    );

    // …and the folder really is free: another device takes it.
    assert!(
        state
            .db
            .try_acquire_upload_lease(folder_id, &ACTOR, &DEVICE_B, 300)
            .await
            .unwrap(),
        "the expired row must not block the next holder either"
    );
    assert_eq!(
        listed(&router, &state, "vault")
            .await
            .lease
            .expect("held")
            .device_id,
        hex::encode(DEVICE_B)
    );
}

/// Turning the choice OFF does not yank a lease out from under its holder.
///
/// The holder's own release, or the TTL, ends a lease. Clearing one from under a
/// device mid-upload is exactly the grief-grab that `release_upload_lease`'s
/// holder scoping exists to stop, and the owner's settings toggle must not be a
/// back door to it. The next pass simply does not take a new one.
#[tokio::test]
async fn disarming_does_not_revoke_a_held_lease() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    create_set(&router, &state, "vault").await;
    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("arm");
    let folder_id = listed(&router, &state, "vault").await.id;
    assert!(
        state
            .db
            .try_acquire_upload_lease(folder_id, &ACTOR, &DEVICE_A, 300)
            .await
            .unwrap()
    );

    update(&router, &state, exclusive_update("vault", false))
        .await
        .expect("disarm");
    let row = listed(&router, &state, "vault").await;
    assert!(!row.exclusive_editing, "the standing choice is off");
    assert_eq!(
        row.lease.expect("still held").device_id,
        hex::encode(DEVICE_A),
        "…and the device that was mid-upload still holds what it took"
    );
}

/// Both fields ride the MEMBER arm.
///
/// A writer member's seat uploads exactly as the owner's does, so it must know
/// to take the lease. And a **reader** member can never call
/// `fauna.folders.lease.acquire` at all — it is gated on the writable-folder
/// resolver — so this projection is the only way that seat can ever learn why
/// the folder will not accept its writes, or who to go and ask.
#[tokio::test]
async fn exclusive_editing_and_the_holder_ride_the_member_arm() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    create_set(&router, &state, "vault").await;
    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("arm");
    let folder_id = listed(&router, &state, "vault").await.id;
    assert!(
        state
            .db
            .try_acquire_upload_lease(folder_id, &ACTOR, &DEVICE_A, 300)
            .await
            .unwrap()
    );

    let raw_group_id = b"raw-group-id".as_slice();
    state
        .db
        .set_folder_mls_group("vault", &ACTOR, Some(raw_group_id))
        .await
        .unwrap();
    let member = [42u8; 32];
    common::seed_dispatch_actor(&state.db, &member).await;
    let channel = fauna_mls::types::ChannelId::from_group_id(raw_group_id).0;
    state
        .db
        .register_actor_channel(&member, &channel)
        .await
        .unwrap();

    let reply: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            member,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: Some(true),
                extra: Default::default(),
            }),
        )
        .await
        .expect("member list ok"),
    )
    .unwrap();
    let member_row = reply
        .folders
        .iter()
        .find(|s| s.name == "vault" && s.role.as_deref() == Some("member"))
        .expect("member row projected");

    assert!(
        member_row.exclusive_editing,
        "a member's seat must know the folder is lease-governed before it writes"
    );
    let held = member_row.lease.as_ref().expect("held");
    assert!(
        held.expires_at > 0,
        "…and THAT it is held, until when — a reader member has no other way to find out"
    );
    assert_eq!(
        held.device_id, "",
        "…but the holder's device id belongs to the holder's account: another \
         account's seat reads it as 'another device is editing', never a device \
         id it could then name in a release or an acquire"
    );
    assert_eq!(
        listed(&router, &state, "vault")
            .await
            .lease
            .expect("held")
            .device_id,
        hex::encode(DEVICE_A),
        "the holder's own account still reads its device"
    );
}

/// A writer member naming the holder's device id can neither take, renew nor
/// release the owner's lease over the wire — the lease is bound to the actor
/// that took it, not to a client-asserted device id.
#[tokio::test]
async fn a_member_naming_the_holders_device_cannot_take_or_drop_the_lease() {
    let (router, state) = router_and_state().await;
    common::seed_dispatch_actor(&state.db, &ACTOR).await;
    create_set(&router, &state, "vault").await;
    update(&router, &state, exclusive_update("vault", true))
        .await
        .expect("arm");
    let folder_id = listed(&router, &state, "vault").await.id;
    assert!(
        state
            .db
            .try_acquire_upload_lease(folder_id, &ACTOR, &DEVICE_A, 300)
            .await
            .unwrap()
    );

    let raw_group_id = b"raw-group-id".as_slice();
    state
        .db
        .set_folder_mls_group("vault", &ACTOR, Some(raw_group_id))
        .await
        .unwrap();
    let member = [42u8; 32];
    common::seed_dispatch_actor(&state.db, &member).await;
    let channel = fauna_mls::types::ChannelId::from_group_id(raw_group_id).0;
    state
        .db
        .register_actor_channel(&member, &channel)
        .await
        .unwrap();
    state
        .db
        .set_folder_member_access(&channel, &member, "writer", None)
        .await
        .unwrap();

    let acquire = dispatch(
        &router,
        Arc::clone(&state),
        member,
        "fauna.folders.lease.acquire",
        encode(&LeaseAcquireRequest {
            name: "vault".into(),
            device_id: hex::encode(DEVICE_A),
            ..Default::default()
        }),
    )
    .await;
    assert!(
        acquire.is_err(),
        "a same-device acquire from another account is a refused takeover, not a renewal"
    );

    // The release is an ok no-op for the member…
    let _ = dispatch(
        &router,
        Arc::clone(&state),
        member,
        "fauna.folders.lease.release",
        encode(&LeaseReleaseRequest {
            name: "vault".into(),
            device_id: hex::encode(DEVICE_A),
            ..Default::default()
        }),
    )
    .await;
    // …and the owner's lease is still there.
    assert_eq!(
        listed(&router, &state, "vault")
            .await
            .lease
            .expect("the owner's lease survived the member's release")
            .device_id,
        hex::encode(DEVICE_A)
    );
}

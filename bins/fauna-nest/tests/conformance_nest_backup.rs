//! Integration round-trips for the nest-side segment-backup grant plane —
//! `fauna.backup.nest_key.{grant,revoke}` + `fauna.backup.status` — nest-side
//! segment backup, slice 2 (design tracked internally;
//! `docs/goal/architecture/key-material-hierarchy.md` § Path A-sibling-0).
//!
//! What the grant *is*: the user's own client derives `NestBackupKey` from its
//! identity seed and grants the 32-byte key to its source nest so the
//! (slice-3) in-process coordinator can seal that owner's segments under it.
//! The nest stores it **plaintext and nest-readable by design** — it seals the
//! owner's own segment files, which rest plaintext-framed in the same data dir.
//!
//! Covered:
//! - grant → status(enrolled) → revoke → status(not enrolled) round-trip.
//! - revoke is idempotent (a second revoke reports `revoked = false`).
//! - the grant persists the exact key nest-readably (`get_nest_backup_key`).
//! - a wrong-length key is refused with a malformed error.
//! - keys are per-actor isolated: one actor's grant/revoke never touches
//!   another's, and each actor's status reflects only its own key.
//! - the kinds are User-class: an actor with no `users` row is refused at the
//!   dispatch gate.
//! - the status projection reports no destination rows for an owner that has
//!   registered none (the populated case is `nest_backup_coordinator.rs`).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/backup.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::encode;
use common::register_user;

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_nest::{backup_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    RpcError,
    backup::{
        AttachFolderReply, AttachFolderRequest, BackupStatusReply, BackupStatusRequest,
        CustodyListReply, CustodyListRequest, DestinationListReply, DestinationListRequest,
        DestinationRegisterReply, DestinationRegisterRequest, DestinationRemoveReply,
        DestinationRemoveRequest, DetachFolderReply, DetachFolderRequest, NestKeyGrantReply,
        NestKeyGrantRequest, NestKeyRevokeReply, NestKeyRevokeRequest,
    },
    decode_strict as decode,
};

/// `AppState::for_test` + a router carrying just the backup handlers.
///
/// The `db_path` override is load-bearing, not decoration: `fauna.backup.status`
/// projects through the in-process coordinator, which keeps per-owner state under
/// the nest **data dir** (derived from `db_path`'s parent). `for_test` leaves
/// `db_path` empty, and the coordinator refuses to guess a data dir rather than
/// writing a user's backup state relative to the process CWD.
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

async fn grant(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    key: &[u8],
) -> Result<NestKeyGrantReply, RpcError> {
    let req = NestKeyGrantRequest {
        nest_backup_key: ByteBuf::from(key.to_vec()),
        extra: Default::default(),
    };
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.nest_key.grant",
        encode(&req),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn revoke(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> Result<NestKeyRevokeReply, RpcError> {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.nest_key.revoke",
        encode(&NestKeyRevokeRequest {
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn status(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> BackupStatusReply {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.status",
        encode(&BackupStatusRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("status succeeds");
    decode(&out).unwrap()
}

// ═════════════════════════════════════════════════════════════════════════════
// Round-trip
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn grant_status_revoke_round_trip() {
    let (router, state) = router_and_state().await;
    let actor = [0x11u8; 32];
    register_user(&state, actor, "alice").await;

    // Before enrolling: not enrolled, empty destinations.
    let s0 = status(&router, &state, actor).await;
    assert!(!s0.enrolled);
    assert!(s0.destinations.is_empty());

    // Grant → ok; status reports enrolled.
    let g = grant(&router, &state, actor, &[0xABu8; 32]).await.unwrap();
    assert!(g.ok);
    let s1 = status(&router, &state, actor).await;
    assert!(s1.enrolled);
    // Enrolled but with nothing registered: the projection has no rows to
    // report. (The rows themselves are proven in `nest_backup_coordinator.rs`,
    // which registers a destination and runs a real pass against it.)
    assert!(s1.destinations.is_empty());

    // Revoke → removed; status reports not enrolled; second revoke is a no-op.
    assert!(revoke(&router, &state, actor).await.unwrap().revoked);
    assert!(!status(&router, &state, actor).await.enrolled);
    assert!(!revoke(&router, &state, actor).await.unwrap().revoked);
}

#[tokio::test]
async fn grant_persists_the_exact_key_nest_readably() {
    // The nest MUST be able to read the granted key (it seals the owner's own
    // segments with it) — prove the stored bytes are the granted bytes.
    let (router, state) = router_and_state().await;
    let actor = [0x22u8; 32];
    register_user(&state, actor, "bob").await;
    let key = [0x5Au8; 32];

    grant(&router, &state, actor, &key).await.unwrap();
    let stored = state
        .db
        .get_nest_backup_key(&actor)
        .await
        .unwrap()
        .expect("key stored");
    assert_eq!(stored, key.to_vec());
}

#[tokio::test]
async fn regrant_replaces_the_key() {
    // A re-grant (e.g. after identity succession re-derives the key under the
    // successor seed) replaces the stored key rather than erroring.
    let (router, state) = router_and_state().await;
    let actor = [0x33u8; 32];
    register_user(&state, actor, "carol").await;

    grant(&router, &state, actor, &[0x01u8; 32]).await.unwrap();
    grant(&router, &state, actor, &[0x02u8; 32]).await.unwrap();
    assert_eq!(
        state.db.get_nest_backup_key(&actor).await.unwrap().unwrap(),
        [0x02u8; 32].to_vec()
    );
}

#[tokio::test]
async fn grant_refuses_wrong_length_key() {
    let (router, state) = router_and_state().await;
    let actor = [0x44u8; 32];
    register_user(&state, actor, "dave").await;

    let err = grant(&router, &state, actor, &[0u8; 31]).await.unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
    // nothing stored on rejection.
    assert!(
        state
            .db
            .get_nest_backup_key(&actor)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!status(&router, &state, actor).await.enrolled);
}

#[tokio::test]
async fn keys_are_per_actor_isolated() {
    let (router, state) = router_and_state().await;
    let alice = [0xAAu8; 32];
    let bob = [0xBBu8; 32];
    register_user(&state, alice, "alice").await;
    register_user(&state, bob, "bob").await;

    grant(&router, &state, alice, &[0x0Au8; 32]).await.unwrap();
    // Bob has no key even though Alice does; his revoke doesn't touch hers.
    assert!(!status(&router, &state, bob).await.enrolled);
    assert!(!revoke(&router, &state, bob).await.unwrap().revoked);
    assert!(status(&router, &state, alice).await.enrolled);
    assert_eq!(
        state.db.get_nest_backup_key(&alice).await.unwrap().unwrap(),
        [0x0Au8; 32].to_vec()
    );
}

#[tokio::test]
async fn unregistered_actor_is_refused_at_the_gate() {
    // The kinds are User-class: an actor with no `users` row resolves to no
    // caller class and is denied at dispatch, before any handler body.
    let (router, state) = router_and_state().await;
    let stranger = [0x99u8; 32];

    let err = grant(&router, &state, stranger, &[0x11u8; 32])
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
    // and nothing was stored.
    assert!(
        state
            .db
            .get_nest_backup_key(&stranger)
            .await
            .unwrap()
            .is_none()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Destination registry — the source-side "where do I back up?" plane (slice 3)
// ═════════════════════════════════════════════════════════════════════════════

async fn register_destination(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    destination_id: &str,
    url: &str,
    nest_id: [u8; 32],
) -> Result<DestinationRegisterReply, RpcError> {
    let req = DestinationRegisterRequest {
        destination_id: destination_id.to_string(),
        destination_nest_url: url.to_string(),
        destination_nest_id: hex::encode(nest_id),
        ..Default::default()
    };
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.destination.register",
        encode(&req),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn list_destinations(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> DestinationListReply {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.destination.list",
        encode(&DestinationListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("list succeeds");
    decode(&out).unwrap()
}

async fn remove_destination(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    destination_id: &str,
) -> Result<DestinationRemoveReply, RpcError> {
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
    .await?;
    Ok(decode(&out).unwrap())
}

#[tokio::test]
async fn destination_register_list_remove_round_trip() {
    let (router, state) = router_and_state().await;
    let actor = [0x11u8; 32];
    register_user(&state, actor, "alice").await;

    assert!(
        list_destinations(&router, &state, actor)
            .await
            .destinations
            .is_empty()
    );

    register_destination(
        &router,
        &state,
        actor,
        "dest-1",
        "https://d1.example",
        [0x51; 32],
    )
    .await
    .unwrap();
    register_destination(
        &router,
        &state,
        actor,
        "dest-2",
        "https://d2.example",
        [0x52; 32],
    )
    .await
    .unwrap();

    let listed = list_destinations(&router, &state, actor).await;
    assert_eq!(listed.destinations.len(), 2);
    let d1 = listed
        .destinations
        .iter()
        .find(|d| d.destination_id == "dest-1")
        .expect("dest-1 present");
    assert_eq!(d1.destination_nest_url, "https://d1.example");
    assert_eq!(d1.destination_nest_id, hex::encode([0x51u8; 32]));

    // Remove one → gone; the other stays. Second remove is a no-op.
    assert!(
        remove_destination(&router, &state, actor, "dest-1")
            .await
            .unwrap()
            .removed
    );
    assert!(
        !remove_destination(&router, &state, actor, "dest-1")
            .await
            .unwrap()
            .removed
    );
    let after = list_destinations(&router, &state, actor).await;
    assert_eq!(after.destinations.len(), 1);
    assert_eq!(after.destinations[0].destination_id, "dest-2");
}

#[tokio::test]
async fn destination_register_refuses_a_malformed_nest_id() {
    // A malformed destination nest id would be an undialable row the coordinator
    // could never handshake against — reject it, store nothing.
    let (router, state) = router_and_state().await;
    let actor = [0x22u8; 32];
    register_user(&state, actor, "bob").await;

    let req = DestinationRegisterRequest {
        destination_id: "dest-1".into(),
        destination_nest_url: "https://d.example".into(),
        destination_nest_id: "not-hex".into(),
        ..Default::default()
    };
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.backup.destination.register",
        encode(&req),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(
        list_destinations(&router, &state, actor)
            .await
            .destinations
            .is_empty()
    );
}

#[tokio::test]
async fn destinations_are_owner_scoped() {
    // A caller lists / removes only its own destinations, never another owner's.
    let (router, state) = router_and_state().await;
    let alice = [0xAAu8; 32];
    let bob = [0xBBu8; 32];
    register_user(&state, alice, "alice").await;
    register_user(&state, bob, "bob").await;

    register_destination(
        &router,
        &state,
        alice,
        "dest-1",
        "https://a.example",
        [0x51; 32],
    )
    .await
    .unwrap();
    register_destination(
        &router,
        &state,
        bob,
        "dest-1",
        "https://b.example",
        [0x52; 32],
    )
    .await
    .unwrap();

    // Same destination_id on both owners; each sees only its own row.
    let a = list_destinations(&router, &state, alice).await;
    assert_eq!(a.destinations.len(), 1);
    assert_eq!(a.destinations[0].destination_nest_url, "https://a.example");
    let b = list_destinations(&router, &state, bob).await;
    assert_eq!(b.destinations.len(), 1);
    assert_eq!(b.destinations[0].destination_nest_url, "https://b.example");

    // Bob removing his "dest-1" leaves Alice's intact.
    assert!(
        remove_destination(&router, &state, bob, "dest-1")
            .await
            .unwrap()
            .removed
    );
    assert_eq!(
        list_destinations(&router, &state, alice)
            .await
            .destinations
            .len(),
        1
    );
    assert!(
        list_destinations(&router, &state, bob)
            .await
            .destinations
            .is_empty()
    );
}

#[tokio::test]
async fn destination_kinds_are_user_class() {
    let (router, state) = router_and_state().await;
    let stranger = [0x99u8; 32];

    let err = register_destination(
        &router,
        &state,
        stranger,
        "dest-1",
        "https://d.example",
        [0x51; 32],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
    // A stranger's list is refused too (never an empty-success that leaks shape).
    let err = dispatch(
        &router,
        state.clone(),
        stranger,
        "fauna.backup.destination.list",
        encode(&DestinationListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

// ── fauna.backup.custody.list ────────────────────────────────────────────────
//
// The destination-side read of the **live** latest-per-path custody this nest is
// holding for the authenticated owner — the source-untrusted observable the
// client audit loop checks freshness against
// (`docs/goal/behavior/backup-restore.md` § Background Tasks).
//
// The sibling `generation.list` reports only *superseded* generations retained
// inside the grace window; a client asking "is my backup current?" needs the
// live rows, and there was no way to ask for them before this kind.

/// Seed one live custody row on a custody-copy reserved set owned by `actor`,
/// the way a real upload does (`SyncEngine::record_change` → `upsert_backup_custody`).
async fn seed_custody(
    state: &Arc<AppState>,
    owner: [u8; 32],
    uploader: [u8; 32],
    folder: &str,
    path: &str,
    manifest: [u8; 32],
    size_bytes: i64,
) {
    // `create_folder_with_options` is idempotent-enough for a test: only create
    // the set the first time a given (owner, set) is seeded.
    if state
        .db
        .get_folder_for_actor(folder, &owner)
        .await
        .unwrap()
        .is_none()
    {
        state
            .db
            .create_folder_with_options(
                folder,
                &owner,
                fauna_nest::db::FolderOptions {
                    // A reserved name is the custody copy a provisioner mints.
                    custody_copy: fauna_core::sync::is_reserved_folder_name(folder),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let fs = state
        .db
        .get_folder_for_actor(folder, &owner)
        .await
        .unwrap()
        .unwrap();
    let path_hash: [u8; 32] = blake3::hash(path.as_bytes()).into();
    state
        .db
        .upsert_backup_custody(
            &uploader,
            fs.id,
            &path_hash,
            Some(path),
            &manifest,
            size_bytes,
            None,
            None,     // path_sealed — this fixture holds no seal root
            i64::MAX, // quota is not what this test is about
        )
        .await
        .unwrap();
}

async fn custody_list(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> Result<CustodyListReply, RpcError> {
    custody_list_page(router, state, actor, None, 0).await
}

async fn custody_list_page(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    cursor: Option<String>,
    limit: i64,
) -> Result<CustodyListReply, RpcError> {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.custody.list",
        encode(&CustodyListRequest {
            cursor,
            limit,
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

/// The core read: every live custody row this destination holds for the owner,
/// with the fields the audit's freshness check reads — including `updated_at`,
/// which is the **destination's** receipt clock, not anything the writer said.
#[tokio::test]
async fn custody_list_returns_this_owners_live_rows() {
    let (router, state) = router_and_state().await;
    let owner = [0x21u8; 32];
    let source_nest = [0x22u8; 32];
    register_user(&state, owner, "owner").await;

    let before = fauna_core::data::Timestamp::now_secs();
    seed_custody(
        &state,
        owner,
        source_nest,
        "__mail",
        "4242/seg-00000001.dat",
        [0xA1; 32],
        4096,
    )
    .await;
    seed_custody(
        &state,
        owner,
        source_nest,
        "__mail",
        "4242/manifest.mail",
        [0xA2; 32],
        128,
    )
    .await;

    let reply = custody_list(&router, &state, owner).await.unwrap();

    assert_eq!(reply.items.len(), 2, "both live paths listed");
    let seg = reply
        .items
        .iter()
        .find(|i| i.path.as_deref() == Some("4242/seg-00000001.dat"))
        .expect("the segment path is listed");
    assert_eq!(seg.folder_name, "__mail");
    assert_eq!(seg.manifest_hash, hex::encode([0xA1u8; 32]));
    assert_eq!(seg.size_bytes, 4096);
    assert_eq!(
        seg.path_hash,
        hex::encode::<[u8; 32]>(blake3::hash(b"4242/seg-00000001.dat").into())
    );
    assert!(
        seg.updated_at >= before,
        "updated_at is the destination's own receipt clock, got {} < {before}",
        seg.updated_at
    );
}

/// The handler-level paging contract (`transport.md` § Max frame corollary):
/// a `limit`-bounded page carries `next_cursor` iff more rows remain, the
/// cursor resumes without a skip or repeat, and a cursor-less request (what
/// `custody_list` sends for its first page) still gets every row in one reply with no cursor.
#[tokio::test]
async fn custody_list_pages_by_cursor_without_skip_or_repeat() {
    let (router, state) = router_and_state().await;
    let owner = [0x27u8; 32];
    let source_nest = [0x22u8; 32];
    register_user(&state, owner, "pager").await;
    for i in 0u8..5 {
        seed_custody(
            &state,
            owner,
            source_nest,
            "__mail",
            &format!("4242/seg-0000000{i}.dat"),
            [0xD0 + i; 32],
            64,
        )
        .await;
    }

    // The shared client's first page: one complete reply, no cursor minted.
    let unpaged = custody_list(&router, &state, owner).await.unwrap();
    assert_eq!(unpaged.items.len(), 5);
    assert_eq!(
        unpaged.next_cursor, None,
        "a reply that served everything must not claim more"
    );

    // The paged walk: pages of 2, cursors chained to absence.
    let mut walked = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = custody_list_page(&router, &state, owner, cursor.clone(), 2)
            .await
            .unwrap();
        pages += 1;
        walked.extend(page.items);
        match page.next_cursor {
            None => break,
            Some(next) => {
                assert_ne!(Some(&next), cursor.as_ref(), "a page must advance");
                cursor = Some(next);
            }
        }
    }
    assert!(
        pages >= 3,
        "5 rows at limit 2 cannot fit in {pages} pages < 3"
    );
    assert_eq!(
        walked
            .iter()
            .map(|i| i.manifest_hash.as_str())
            .collect::<Vec<_>>(),
        unpaged
            .items
            .iter()
            .map(|i| i.manifest_hash.as_str())
            .collect::<Vec<_>>(),
        "pages concatenate to exactly the unpaged serve — no row skipped or repeated"
    );

    // A cursor the nest never minted refuses loudly rather than silently
    // restarting from the first page.
    let err = custody_list_page(&router, &state, owner, Some("not-a-cursor".into()), 2)
        .await
        .expect_err("a malformed cursor is refused");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

/// Owner isolation: the reply is scoped by the *folder's* owner, so one
/// owner's custody never leaks into another's audit — even though both rows were
/// written by the same uploading source nest.
#[tokio::test]
async fn custody_list_never_returns_another_owners_rows() {
    let (router, state) = router_and_state().await;
    let owner = [0x31u8; 32];
    let other = [0x32u8; 32];
    let source_nest = [0x33u8; 32];
    register_user(&state, owner, "owner-a").await;
    register_user(&state, other, "owner-b").await;

    seed_custody(
        &state,
        owner,
        source_nest,
        "__mail",
        "aaaa/seg-00000001.dat",
        [0xB1; 32],
        10,
    )
    .await;
    seed_custody(
        &state,
        other,
        source_nest,
        "__mail",
        "bbbb/seg-00000001.dat",
        [0xB2; 32],
        20,
    )
    .await;

    let mine = custody_list(&router, &state, owner).await.unwrap();
    assert_eq!(mine.items.len(), 1);
    assert_eq!(mine.items[0].path.as_deref(), Some("aaaa/seg-00000001.dat"));

    let theirs = custody_list(&router, &state, other).await.unwrap();
    assert_eq!(theirs.items.len(), 1);
    assert_eq!(
        theirs.items[0].path.as_deref(),
        Some("bbbb/seg-00000001.dat")
    );
}

/// A tombstoned path (the compacted-out `delete` record) is NOT live custody.
/// This is load-bearing for the audit: a destination that has tombstoned
/// everything holds nothing, and must not read as a healthy backup.
#[tokio::test]
async fn custody_list_omits_tombstoned_paths() {
    let (router, state) = router_and_state().await;
    let owner = [0x41u8; 32];
    let source_nest = [0x42u8; 32];
    register_user(&state, owner, "owner-c").await;

    seed_custody(
        &state,
        owner,
        source_nest,
        "__mail",
        "cccc/seg-00000001.dat",
        [0xC1; 32],
        10,
    )
    .await;
    let fs = state
        .db
        .get_folder_for_actor("__mail", &owner)
        .await
        .unwrap()
        .unwrap();
    let path_hash: [u8; 32] = blake3::hash(b"cccc/seg-00000001.dat").into();
    state
        .db
        .tombstone_backup_custody(fs.id, &path_hash)
        .await
        .unwrap();

    let reply = custody_list(&router, &state, owner).await.unwrap();
    assert!(
        reply.items.is_empty(),
        "a tombstoned path is not live custody, got {:?}",
        reply.items
    );
}

/// An owner this destination holds nothing for gets an empty list, not an error
/// — "enrolled but nothing received yet" is a legitimate state the audit reads
/// as a freshness question, not a transport failure.
#[tokio::test]
async fn custody_list_is_empty_for_an_owner_with_no_custody() {
    let (router, state) = router_and_state().await;
    let owner = [0x51u8; 32];
    register_user(&state, owner, "owner-d").await;

    let reply = custody_list(&router, &state, owner).await.unwrap();
    assert!(reply.items.is_empty());
}

/// User-class, like every other owner-facing backup kind: an actor with no
/// `users` row is refused at the dispatch gate rather than getting an empty
/// success that leaks whether this owner has custody here.
#[tokio::test]
async fn custody_list_is_user_class() {
    let (router, state) = router_and_state().await;
    let stranger = [0x61u8; 32];

    let err = custody_list(&router, &state, stranger).await.unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

// ── ordinary-folder destination coverage (attach_folder / detach_folder) ──────
//
// `docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage —
// destination places. The verbs are owner-scoped and idempotent; the reply's
// `folder_set` is the canonical `__folder/<nest-hex>/<folder-id>` name the
// client records verbatim, so the naming rule is pinned here once.

async fn attach_folder(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    destination_id: &str,
    folder_id: i64,
) -> Result<AttachFolderReply, RpcError> {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.destination.attach_folder",
        encode(&AttachFolderRequest {
            destination_id: destination_id.to_string(),
            folder_id,
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn detach_folder(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    destination_id: &str,
    folder_id: i64,
) -> Result<DetachFolderReply, RpcError> {
    let out = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.backup.destination.detach_folder",
        encode(&DetachFolderRequest {
            destination_id: destination_id.to_string(),
            folder_id,
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn create_ordinary_folder(state: &Arc<AppState>, actor: [u8; 32], name: &str) -> i64 {
    state
        .db
        .create_folder_with_options(name, &actor, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create ordinary folder")
}

#[tokio::test]
async fn attach_detach_folder_round_trip_with_coverage_read_back() {
    let (router, state) = router_and_state().await;
    let actor = [0x71u8; 32];
    register_user(&state, actor, "carol").await;
    register_destination(
        &router,
        &state,
        actor,
        "dest-1",
        "https://d1.example",
        [0x51; 32],
    )
    .await
    .unwrap();
    let folder_id = create_ordinary_folder(&state, actor, "Photos").await;

    let reply = attach_folder(&router, &state, actor, "dest-1", folder_id)
        .await
        .unwrap();
    assert!(reply.attached);
    let expected_set = format!(
        "__folder/{}/{folder_id}",
        hex::encode(state.nest_identity.public_key_bytes())
    );
    assert_eq!(reply.folder_set, expected_set);

    // Idempotent re-attach: same reply shape, `attached = false`, same set name.
    let again = attach_folder(&router, &state, actor, "dest-1", folder_id)
        .await
        .unwrap();
    assert!(!again.attached);
    assert_eq!(again.folder_set, expected_set);

    // The list read-back joins the coverage onto the destination row.
    let listed = list_destinations(&router, &state, actor).await;
    assert_eq!(listed.destinations.len(), 1);
    let covered = &listed.destinations[0].covered_folders;
    assert_eq!(covered.len(), 1);
    assert_eq!(covered[0].folder_id, folder_id);
    assert_eq!(covered[0].folder_set, expected_set);
    // The folder's display name rides the owner's own listing — the one place
    // the owner's device learns the label a re-seed restores the folder under,
    // since destination custody never carries it (`CoveredFolder::name`).
    assert_eq!(covered[0].name.as_deref(), Some("Photos"));
    // So do its address and sealed label, verbatim — all the listing carries
    // for a set once its plaintext name leaves the row.
    {
        let conn = state.db.conn().await;
        conn.execute(
            "UPDATE folders SET name_sealed = ?1 WHERE id = ?2",
            rusqlite::params![b"sealed-photos".to_vec(), folder_id],
        )
        .unwrap();
    }
    let listed = list_destinations(&router, &state, actor).await;
    let covered = &listed.destinations[0].covered_folders;
    assert_eq!(
        covered[0].name_hash.as_deref().map(|h| &h[..]),
        Some(&fauna_core::path_crypto::set_name_hash("Photos")[..])
    );
    assert_eq!(
        covered[0].name_sealed.as_deref().map(|s| &s[..]),
        Some(&b"sealed-photos"[..])
    );

    // Detach → gone from the read-back; a second detach is an idempotent no-op.
    assert!(
        detach_folder(&router, &state, actor, "dest-1", folder_id)
            .await
            .unwrap()
            .detached
    );
    assert!(
        !detach_folder(&router, &state, actor, "dest-1", folder_id)
            .await
            .unwrap()
            .detached
    );
    let after = list_destinations(&router, &state, actor).await;
    assert!(after.destinations[0].covered_folders.is_empty());
}

#[tokio::test]
async fn attach_refuses_an_unregistered_destination() {
    // A coverage row on a destination the coordinator will never dial would be
    // exactly the phantom rail the design forbids — refuse, store nothing.
    let (router, state) = router_and_state().await;
    let actor = [0x72u8; 32];
    register_user(&state, actor, "dave").await;
    let folder_id = create_ordinary_folder(&state, actor, "Docs").await;

    let err = attach_folder(&router, &state, actor, "dest-none", folder_id)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
    let listed = list_destinations(&router, &state, actor).await;
    assert!(listed.destinations.is_empty());
}

#[tokio::test]
async fn attach_refuses_a_reserved_set_and_a_foreign_folder() {
    // Reserved rails stay whole-account and implicit — a per-rail attach is
    // refused; and coverage is owner-only, so another owner's folder reads as
    // not-found rather than attaching.
    let (router, state) = router_and_state().await;
    let alice = [0x73u8; 32];
    let bob = [0x74u8; 32];
    register_user(&state, alice, "alice-cov").await;
    register_user(&state, bob, "bob-cov").await;
    register_destination(
        &router,
        &state,
        alice,
        "dest-1",
        "https://d1.example",
        [0x51; 32],
    )
    .await
    .unwrap();

    let reserved_id = state
        .db
        .create_folder_with_options(
            "__mail",
            &alice,
            fauna_nest::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .expect("create reserved destination set");
    let err = attach_folder(&router, &state, alice, "dest-1", reserved_id)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.invalid_request");

    let bobs_folder = create_ordinary_folder(&state, bob, "Bobs").await;
    let err = attach_folder(&router, &state, alice, "dest-1", bobs_folder)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.not_found");

    let listed = list_destinations(&router, &state, alice).await;
    assert!(listed.destinations[0].covered_folders.is_empty());
}

#[tokio::test]
async fn destination_remove_drops_its_coverage() {
    // A re-used destination id must not inherit a prior enrollment's
    // attachments — remove drops the coverage rows with the registry row.
    let (router, state) = router_and_state().await;
    let actor = [0x75u8; 32];
    register_user(&state, actor, "erin").await;
    register_destination(
        &router,
        &state,
        actor,
        "dest-1",
        "https://d1.example",
        [0x51; 32],
    )
    .await
    .unwrap();
    let folder_id = create_ordinary_folder(&state, actor, "Music").await;
    attach_folder(&router, &state, actor, "dest-1", folder_id)
        .await
        .unwrap();

    remove_destination(&router, &state, actor, "dest-1")
        .await
        .unwrap();
    register_destination(
        &router,
        &state,
        actor,
        "dest-1",
        "https://d1.example",
        [0x51; 32],
    )
    .await
    .unwrap();
    let listed = list_destinations(&router, &state, actor).await;
    assert!(listed.destinations[0].covered_folders.is_empty());
}

#[tokio::test]
async fn detach_succeeds_after_the_folder_itself_is_gone() {
    // Detach deliberately skips folder validation: a deleted folder's coverage
    // must stay cleanable by its owner, or the sweep revisits it forever.
    let (router, state) = router_and_state().await;
    let actor = [0x76u8; 32];
    register_user(&state, actor, "frank").await;
    register_destination(
        &router,
        &state,
        actor,
        "dest-1",
        "https://d1.example",
        [0x51; 32],
    )
    .await
    .unwrap();
    let folder_id = create_ordinary_folder(&state, actor, "Scans").await;
    attach_folder(&router, &state, actor, "dest-1", folder_id)
        .await
        .unwrap();

    state
        .db
        .delete_folder_for_user("Scans", &actor)
        .await
        .expect("delete the folder");

    assert!(
        detach_folder(&router, &state, actor, "dest-1", folder_id)
            .await
            .unwrap()
            .detached
    );
}

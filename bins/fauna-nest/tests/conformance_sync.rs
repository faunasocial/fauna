//! Integration round-trip for the device-sync control surface —
//! `fauna.sync.{register,changes.{list,record},status,files,backup_status,
//! devices.{list,delete}}`. A behavior-preserving transport migration of the
//! bearer-authed device-sync routes (`sync_routes`); the handlers reuse the
//! same `CacheDb` methods the twins call. These tests exercise the WS-RPC
//! layer: request decode, reply shapes, the `User | Admin` allowlist, the
//! `invalid_request` / `not_found` / `permission_denied` mappings, cross-actor isolation, and replay metadata.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/sync.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    folder_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    sync_handlers,
};
use fauna_protocol::{
    ByteBuf, decode_strict as decode,
    folders::{FolderCreateReply, FolderCreateRequest},
    sync::{
        SyncBackupStatusReply, SyncBackupStatusRequest, SyncChangeRecordReply,
        SyncChangeRecordRequest, SyncChangesListReply, SyncChangesListRequest,
        SyncChangesSupersedeReply, SyncChangesSupersedeRequest, SyncDeviceDeleteReply,
        SyncDeviceDeleteRequest, SyncDevicesListReply, SyncDevicesListRequest, SyncFilesReply,
        SyncFilesRequest, SyncRegisterReply, SyncRegisterRequest, SyncStatusReply,
        SyncStatusRequest,
    },
};

const ALL_KINDS: [&str; 9] = [
    "fauna.sync.register",
    "fauna.sync.changes.list",
    "fauna.sync.changes.record",
    "fauna.sync.changes.supersede",
    "fauna.sync.status",
    "fauna.sync.files",
    "fauna.sync.backup_status",
    "fauna.sync.devices.list",
    "fauna.sync.devices.delete",
];

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    // folder handlers too: the sync surface needs an owned folder to scope
    // changes / status / files against, which we create via `folders.create`.
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    (b.build(), state)
}

/// A 32-byte id of repeated `b`, as both raw bytes and the hex the wire uses.
fn id_bytes(b: u8) -> [u8; 32] {
    [b; 32]
}
fn id_hex(b: u8) -> String {
    id_bytes(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// Create a folder named `name` for `actor`, returning its id. The set is born
/// under the client-minted set nonce ([`common::SET_NONCE`]) a writer signs its
/// records under — every record into it is signed with
/// [`common::signed_record`] by the recorder's real keypair
/// ([`common::signing_actor`]); an unsigned record is refused
/// `signature_required`.
async fn create_set(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32], name: &str) -> i64 {
    let reply: FolderCreateReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            actor,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: name.into(),
                retention_policy: None,
                set_nonce: common::set_nonce_field(),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    reply.id
}

/// Register `device` (hex) for `actor` with default `read,write` capabilities.
async fn register_device(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_hex: &str,
    label: &str,
) {
    let reply: SyncRegisterReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            actor,
            "fauna.sync.register",
            encode(&SyncRegisterRequest {
                device_id: device_hex.into(),
                label: label.into(),
                capabilities: "read,write".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("register ok"),
    )
    .unwrap();
    assert_eq!(reply.device_id, device_hex);
}

// ── register + devices.list ────────────────────────────────────────────────────

#[tokio::test]
async fn register_then_devices_list_round_trip() {
    let (router, state) = router_and_state().await;
    let actor = [11u8; 32];
    let dev = id_hex(0xa1);
    register_device(&router, &state, actor, &dev, "Alice's Laptop").await;

    let list: SyncDevicesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.devices.list",
            encode(&SyncDevicesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.devices.len(), 1);
    let d = &list.devices[0];
    assert_eq!(d.device_id, dev);
    // S9 flip: user labels rest sealed-only — the plaintext serves ''.
    assert_eq!(d.label, "");
    assert_eq!(d.capabilities, "read,write");
    assert!(!d.online, "an unconnected test device is offline");
    assert!(d.folders.is_empty());
}

/// The S6-b write half, end to end: a client's `label_sealed` survives the
/// register handler, the `sync_devices` upsert, the `SELECT`, and
/// `devices_list_handler` **byte-identically** — the nest holds no key, so any
/// mutation of the blob in transit is a bug, not a re-seal.
///
/// The seal is minted through the production funnel rather than by a literal:
/// a golden blob would pass just as happily if the writer reached for the wrong
/// root, which is the failure mode this plane degrades to *silently*.
#[tokio::test]
async fn a_sealed_device_label_round_trips_the_nest_byte_identically() {
    let (router, state) = router_and_state().await;
    let actor = [21u8; 32];
    let device_id = [0xb2u8; 32];
    let dev = hex::encode(device_id);

    let root = fauna_core::path_crypto::LabelRoot::owner_of(
        &fauna_core::crypto::BackupKey::from_bytes([5u8; 32]),
    );
    let sealed = fauna_core::label_custody::seal_device_label(&root, &device_id, "Alice's Laptop")
        .expect("seal ok")
        .expect("a user-chosen label seals");

    let _: SyncRegisterReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.register",
            encode(&SyncRegisterRequest {
                device_id: dev.clone(),
                label: "Alice's Laptop".into(),
                label_sealed: Some(fauna_protocol::ByteBuf::from(sealed.clone())),
                ..Default::default()
            }),
        )
        .await
        .expect("register ok"),
    )
    .unwrap();

    let list: SyncDevicesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.devices.list",
            encode(&SyncDevicesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let d = &list.devices[0];
    assert_eq!(
        d.label_sealed.as_ref().map(|b| b.to_vec()),
        Some(sealed.clone()),
        "the nest stores and serves the seal verbatim"
    );

    // And it opens back to the label under the owner's own custody — the
    // property the round trip exists for, not just "some bytes came back".
    assert_eq!(
        fauna_core::label_custody::render_device_label(
            &fauna_core::file_download::FileDownloadKeys::owner(
                fauna_core::crypto::BackupKey::from_bytes([5u8; 32])
            ),
            d.label_sealed.as_ref().map(|b| &b[..]),
            "",
            &device_id,
        ),
        fauna_core::path_crypto::SealedLabelRender::Sealed("Alice's Laptop".into()),
    );
}

/// A re-register with **no** seal clears the stored one rather than keeping it.
/// Load-bearing, and the opposite of what a `COALESCE` upsert would do: the
/// label and its seal must move together, or a keyless writer's rename leaves a
/// row whose plaintext says one name and whose seal opens to the previous one —
/// and post-flip the user would be shown the stale name with nothing failing.
#[tokio::test]
async fn a_keyless_re_register_clears_the_seal_it_cannot_re_mint() {
    let (router, state) = router_and_state().await;
    let actor = [22u8; 32];
    let device_id = [0xb3u8; 32];
    let dev = hex::encode(device_id);
    let root = fauna_core::path_crypto::LabelRoot::owner_of(
        &fauna_core::crypto::BackupKey::from_bytes([6u8; 32]),
    );
    let sealed = fauna_core::label_custody::seal_device_label(&root, &device_id, "Old name")
        .unwrap()
        .unwrap();

    for (label, seal) in [
        ("Old name", Some(sealed)),
        // The ffi bearer-only arm: a real rename it cannot seal.
        ("New name", None),
    ] {
        let _: SyncRegisterReply = decode(
            &dispatch(
                &router,
                Arc::clone(&state),
                actor,
                "fauna.sync.register",
                encode(&SyncRegisterRequest {
                    device_id: dev.clone(),
                    label: label.into(),
                    label_sealed: seal.map(fauna_protocol::ByteBuf::from),
                    ..Default::default()
                }),
            )
            .await
            .expect("register ok"),
        )
        .unwrap();
    }

    let list: SyncDevicesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.devices.list",
            encode(&SyncDevicesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let d = &list.devices[0];
    // S9 flip: the re-registered plaintext rests '' like any user label; the
    // property under test is the SEAL clearing below.
    assert_eq!(d.label, "");
    assert_eq!(
        d.label_sealed, None,
        "a stale seal opening to \"Old name\" would be worse than none — S8 re-stamps this row"
    );
}

#[tokio::test]
async fn register_rejects_bad_device_hex() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [12u8; 32],
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: "not-hex".into(),
            label: "x".into(),
            capabilities: "read,write".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("bad hex rejected");
    assert_eq!(err.code, "fauna.sync.invalid_request");
}

// ── changes.record + changes.list ──────────────────────────────────────────────

#[tokio::test]
async fn record_then_list_changes_round_trip() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(13);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xb2);
    let manifest = id_hex(0xc3);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;

    let rec: SyncChangeRecordReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            encode(&common::signed_record(
                SyncChangeRecordRequest {
                    path_sealed: Some(fauna_protocol::ByteBuf::from(
                        b"e2e-synthetic-seal".to_vec(),
                    )),
                    nest_url: None,
                    channel_id: None,
                    folder: "docs".into(),
                    device_id: dev.clone(),
                    path: "notes/todo.md".into(),
                    manifest_hash: Some(manifest.clone()),
                    size_bytes: 4096,
                    change_type: "create".into(),
                    content_key_version: None,
                    thumbnail_hash: None,
                    ..Default::default()
                },
                &kp,
            )),
        )
        .await
        .expect("record ok"),
    )
    .unwrap();
    assert!(rec.seq > 0);

    // list (folder mode, no exclude) returns the change with hex hashes.
    let list: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: Some("docs".into()),
                device_id: None,
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.changes.len(), 1);
    let c = &list.changes[0];
    assert_eq!(c.seq, rec.seq);
    // S9 flip: no plaintext rests; the hash + seal address the row.
    assert_eq!(c.path, None);
    assert_eq!(
        c.path_hash,
        hex::encode(fauna_core::sync::path_hash("notes/todo.md"))
    );
    assert_eq!(c.manifest_hash.as_deref(), Some(manifest.as_str()));
    assert_eq!(c.device_id.as_deref(), Some(dev.as_str()));
    assert_eq!(c.size_bytes, 4096);

    // excluding the recording device filters it out.
    let excluded: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: Some("docs".into()),
                device_id: Some(dev.clone()),
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(excluded.changes.is_empty(), "own device excluded");
}

#[tokio::test]
async fn changes_list_without_a_folder_is_refused() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(14);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xb2);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&common::signed_record(
            SyncChangeRecordRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                nest_url: None,
                channel_id: None,
                folder: "docs".into(),
                device_id: dev.clone(),
                path: "a.txt".into(),
                manifest_hash: Some(id_hex(0xc3)),
                size_bytes: 1,
                change_type: "create".into(),
                content_key_version: None,
                thumbnail_hash: None,
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("record ok");

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            folder: None,
            device_id: None,
            since: 0,
            ..Default::default()
        }),
    )
    .await
    .expect_err("there is no actor-wide feed");
    assert_eq!(err.code, "fauna.sync.invalid_request");
}

/// scope half: `name_hash` selects a set **on its own**, with no
/// plaintext `folder` beside it — the shape every app sends once S9 stops
/// sending cleartext names. Two sets, one change each: a hash-only list must
/// return the addressed set's change and nothing else. The pre-fix code parsed
/// `name_hash` only inside the `folder.is_some()` arm, so this request fell
/// through to the actor-wide branch and answered a one-set question with both.
#[tokio::test]
async fn changes_list_addresses_by_name_hash_alone() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(15);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xb2);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;
    create_set(&router, &state, actor, "photos").await;

    for (set, path) in [("docs", "a.txt"), ("photos", "b.jpg")] {
        dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            encode(&common::signed_record(
                SyncChangeRecordRequest {
                    path_sealed: Some(fauna_protocol::ByteBuf::from(
                        b"e2e-synthetic-seal".to_vec(),
                    )),
                    folder: set.into(),
                    device_id: dev.clone(),
                    path: path.into(),
                    manifest_hash: Some(id_hex(0xc3)),
                    size_bytes: 1,
                    change_type: "create".into(),
                    ..Default::default()
                },
                &kp,
            )),
        )
        .await
        .expect("record ok");
    }

    let list: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: None,
                name_hash: Some(ByteBuf::from(
                    fauna_core::path_crypto::set_name_hash("docs").to_vec(),
                )),
                device_id: None,
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("hash-addressed list ok"),
    )
    .unwrap();
    assert_eq!(
        list.changes.len(),
        1,
        "a hash-only request is scoped to the ONE set it addresses, not every set the caller owns"
    );
    assert_eq!(list.changes[0].path, None, "no plaintext rests post-flip");
    assert_eq!(
        list.changes[0].path_hash,
        hex::encode(fauna_core::sync::path_hash("a.txt"))
    );
}

/// validation half — the part a scope-only fix leaves open. A
/// malformed hash must refuse even when no plaintext `folder` rides beside
/// it; before the fix the parse sat on the branch this request never takes, so
/// a 3-byte "digest" was silently accepted and the reply widened to every set.
#[tokio::test]
async fn changes_list_refuses_a_malformed_hash_with_no_plaintext_name() {
    let (router, state) = router_and_state().await;
    let actor = [16u8; 32];
    create_set(&router, &state, actor, "docs").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            folder: None,
            name_hash: Some(ByteBuf::from(vec![1u8, 2, 3])),
            since: 0,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a 3-byte hash is not a BLAKE3 digest");
    assert_eq!(err.code, "fauna.sync.invalid_request");
}

/// the over-length half of the same contract: a 33-byte hash must
/// refuse, never be silently truncated to the first 32 bytes.
#[tokio::test]
async fn changes_list_refuses_an_over_length_hash_with_no_plaintext_name() {
    let (router, state) = router_and_state().await;
    let actor = [17u8; 32];
    create_set(&router, &state, actor, "docs").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            folder: None,
            name_hash: Some(ByteBuf::from(vec![7u8; 33])),
            since: 0,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a 33-byte hash is not a BLAKE3 digest");
    assert_eq!(err.code, "fauna.sync.invalid_request");
}

#[tokio::test]
async fn record_rejects_unregistered_and_readonly_device() {
    let (router, state) = router_and_state().await;
    let actor = [15u8; 32];
    create_set(&router, &state, actor, "docs").await;

    // Unregistered device → permission_denied.
    let unreg = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&SyncChangeRecordRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            nest_url: None,
            channel_id: None,
            folder: "docs".into(),
            device_id: id_hex(0xd4),
            path: "a.txt".into(),
            manifest_hash: None,
            size_bytes: 1,
            change_type: "create".into(),
            content_key_version: None,
            thumbnail_hash: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("unregistered rejected");
    // Dedicated code (distinct from the read-only `permission_denied` below) so a
    // client can self-heal a never-registered device — the contract the shared
    // Media write seam's register-and-retry keys on (`fauna_media_machine`).
    assert_eq!(unreg.code, "fauna.sync.device_unregistered");

    // Read-only device → permission_denied.
    let ro = id_hex(0xe5);
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: ro.clone(),
            label: "backup-nas".into(),
            capabilities: "read".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("register ok");
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&SyncChangeRecordRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            nest_url: None,
            channel_id: None,
            folder: "docs".into(),
            device_id: ro,
            path: "a.txt".into(),
            manifest_hash: None,
            size_bytes: 1,
            change_type: "create".into(),
            content_key_version: None,
            thumbnail_hash: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("read-only rejected");
    assert_eq!(err.code, "fauna.sync.permission_denied");
}

/// The write-capability lookup is scoped to the **connection actor**: actor A
/// recording into A's own set while naming a `device_id` that belongs to a
/// *different* actor B must NOT pass the write gate — it hits the
/// `device_unregistered` arm (the device isn't A's). A `device_id` is an
/// attacker-controllable wire
/// param, so the actor-unscoped lookup (which would find B's write-capable device
/// and let A attribute a row to a foreign device) must not gate this authenticated
/// plane — the in-code contract at `db/sync_storage.rs`
/// `get_device_capabilities_for_actor`.
#[tokio::test]
async fn record_rejects_a_foreign_actors_device() {
    let (router, state) = router_and_state().await;
    let a = [0x1au8; 32];
    let b = [0x2bu8; 32];

    // A owns a set; B registers a write-capable device of its own.
    create_set(&router, &state, a, "docs").await;
    let b_device = id_hex(0xbb);
    register_device(&router, &state, b, &b_device, "B's laptop").await;

    // A records into A's OWN set but names B's device_id. The actor-scoped
    // capability lookup finds no such device for A → device_unregistered.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        a,
        "fauna.sync.changes.record",
        encode(&SyncChangeRecordRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            nest_url: None,
            channel_id: None,
            folder: "docs".into(),
            device_id: b_device,
            path: "a.txt".into(),
            manifest_hash: None,
            size_bytes: 1,
            change_type: "create".into(),
            content_key_version: None,
            thumbnail_hash: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a device registered to another actor must not pass A's write gate");
    assert_eq!(err.code, "fauna.sync.device_unregistered");
}

// ── files + backup_status ──────────────────────────────────────────────────────

#[tokio::test]
async fn files_and_backup_status_reflect_recorded_change() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(16);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xb2);
    let manifest = id_hex(0xc3);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&common::signed_record(
            SyncChangeRecordRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                nest_url: None,
                channel_id: None,
                folder: "docs".into(),
                device_id: dev,
                path: "photo.png".into(),
                manifest_hash: Some(manifest.clone()),
                size_bytes: 2048,
                change_type: "create".into(),
                content_key_version: None,
                thumbnail_hash: None,
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("record ok");

    let files: SyncFilesReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.files",
            encode(&SyncFilesRequest {
                folder: "docs".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("files ok"),
    )
    .unwrap();
    assert_eq!(files.files.len(), 1);
    // S9 flip: the required wire field carries the scrub sentinel.
    assert_eq!(files.files[0].path, "");
    assert_eq!(files.files[0].manifest_hash, manifest);
    assert_eq!(files.files[0].size_bytes, 2048);

    let bs: SyncBackupStatusReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.backup_status",
            encode(&SyncBackupStatusRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("backup_status ok"),
    )
    .unwrap();
    assert_eq!(bs.folders.len(), 1);
    assert_eq!(bs.folders[0].name, "docs");
    // The address rides beside the name, so a reader still renders the entry
    // once the plaintext scrubs (`path-sealing.md` § the set-name plane).
    assert_eq!(
        bs.folders[0].name_hash.as_deref().map(|b| b.to_vec()),
        Some(fauna_core::path_crypto::set_name_hash("docs").to_vec())
    );
    assert!(
        bs.folders[0].last_change_at.is_some(),
        "last_change_at set after a change"
    );
}

/// The third instance of the missing-`path_hash` class (`fauna.media.list`
/// S5d and `WebdavFile` S4 before it): `fauna.sync.files` ships `path_sealed`
/// but withheld the convergent salt a sealed-first renderer needs to open it.
/// `readable_folder` never admits an `AdminDiscovery`-only caller (owner or
/// group-member roster only), so every caller who reaches this handler is
/// already the label audience — no extra per-reader gate is needed, only the
/// pair projected together, mirroring `every_item_carries_the_path_hash_the_render_salts_from`
/// in `conformance_media_list.rs`.
#[tokio::test]
async fn files_carries_the_path_hash_the_sealed_path_render_salts_from() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(17);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xb3);
    let manifest = id_hex(0xc4);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&common::signed_record(
            SyncChangeRecordRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                nest_url: None,
                channel_id: None,
                folder: "docs".into(),
                device_id: dev,
                path: "photo.png".into(),
                manifest_hash: Some(manifest),
                size_bytes: 2048,
                change_type: "create".into(),
                content_key_version: None,
                thumbnail_hash: None,
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("record ok");

    let files: SyncFilesReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.files",
            encode(&SyncFilesRequest {
                folder: "docs".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("files ok"),
    )
    .unwrap();
    assert_eq!(files.files.len(), 1);
    assert_eq!(
        files.files[0].path_hash.as_deref().map(|h| h.to_vec()),
        Some(blake3::hash(b"photo.png").as_bytes().to_vec()),
        "the wire carries the row's stored path_hash verbatim, the salt \
         path_sealed opens under (path-sealing.md: a seal whose salt is \
         missing is unrenderable the moment the plaintext scrubs)"
    );
}

// ── status ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn status_for_fresh_set_has_no_source_or_destinations() {
    let (router, state) = router_and_state().await;
    let actor = [17u8; 32];
    create_set(&router, &state, actor, "docs").await;

    let st: SyncStatusReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.status",
            encode(&SyncStatusRequest {
                folder: "docs".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("status ok"),
    )
    .unwrap();
    assert_eq!(st.folder, "docs");
    // A fresh set rests at the default residency (full), so the nest itself
    // holds its content and it is reachable with no seat online — the one
    // content-reachability verdict (`file-sync.md` § Content reachability,
    // `connections::folder_content_reachable`).
    assert!(st.source_online);
}

/// S5b (`file-sync.md` § Sealed names & paths): `fauna.sync.status` resolves
/// hash-first. An **empty** plaintext `folder` alongside `name_hash` proves
/// the hash did the work: a by-name lookup would see the empty wire `folder`
/// and miss.
#[tokio::test]
async fn status_addresses_by_name_hash_with_empty_plaintext_name() {
    let (router, state) = router_and_state().await;
    let actor = [23u8; 32];
    create_set(&router, &state, actor, "docs").await;
    let hash = fauna_core::path_crypto::set_name_hash("docs");

    let st: SyncStatusReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.status",
            encode(&SyncStatusRequest {
                folder: String::new(),
                name_hash: Some(ByteBuf::from(hash.to_vec())),
                ..Default::default()
            }),
        )
        .await
        .expect("hash-addressed status ok"),
    )
    .unwrap();
    assert!(st.source_online);

    // A malformed (non-32-byte) hash is refused, never silently ignored.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.status",
        encode(&SyncStatusRequest {
            folder: String::new(),
            name_hash: Some(ByteBuf::from(vec![1u8, 2, 3])),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a 3-byte hash is not a BLAKE3 digest");
    assert_eq!(err.code, "fauna.sync.invalid_request");
}

/// The `status` twin took `_bearer` and skipped the ownership check; the kind
/// adds it (module-doc latent-authorization fix). A different actor gets
/// `not_found`, not another actor's status.
#[tokio::test]
async fn status_and_files_reject_other_actor() {
    let (router, state) = router_and_state().await;
    let owner = [18u8; 32];
    let intruder = [19u8; 32];
    create_set(&router, &state, owner, "docs").await;

    let st_err = dispatch(
        &router,
        Arc::clone(&state),
        intruder,
        "fauna.sync.status",
        encode(&SyncStatusRequest {
            folder: "docs".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("intruder denied");
    // ST-RES-1 (S2-P3): owner-infra surfaces fold permission_denied → not_found
    // so a non-owner can't probe whether a named set exists.
    assert_eq!(st_err.code, "fauna.sync.not_found");

    let files_err = dispatch(
        &router,
        Arc::clone(&state),
        intruder,
        "fauna.sync.files",
        encode(&SyncFilesRequest {
            folder: "docs".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("intruder denied");
    assert_eq!(files_err.code, "fauna.sync.not_found");
}

// ── devices.delete ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn devices_delete_then_gone() {
    let (router, state) = router_and_state().await;
    let actor = [20u8; 32];
    let dev = id_hex(0xa1);
    register_device(&router, &state, actor, &dev, "laptop").await;

    let del: SyncDeviceDeleteReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.devices.delete",
            encode(&SyncDeviceDeleteRequest {
                device_id: dev.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap();
    assert!(del.deleted);

    let list: SyncDevicesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.devices.list",
            encode(&SyncDevicesListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(list.devices.is_empty());
}

#[tokio::test]
async fn devices_delete_missing_is_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [21u8; 32],
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: id_hex(0xf6),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("missing device");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// The sole-source refusal retired with the single-source concept
/// (`devices.md`, the removal checks): the nest holds the head, so removing
/// the only device that originates into a folder loses nothing — the delete
/// succeeds and the device's place leaves the roster with it.
#[tokio::test]
async fn devices_delete_of_a_folders_only_originating_seat_succeeds() {
    let (router, state) = router_and_state().await;
    let actor = [22u8; 32];
    let dev = id_hex(0xa1);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;
    let fs = state
        .db
        .get_folder_for_actor("docs", &actor)
        .await
        .unwrap()
        .expect("the set exists");
    state
        .db
        .set_folder_place_flags(
            fs.id,
            &actor[..],
            &id_bytes(0xa1)[..],
            &fauna_protocol::folders::PlaceFlags::new(true, false, false),
        )
        .await
        .expect("place the device");

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.devices.delete",
        encode(&SyncDeviceDeleteRequest {
            device_id: dev,
            extra: Default::default(),
        }),
    )
    .await
    .expect("an only-originating seat never blocks a removal");
    assert!(state.db.get_folder_members(fs.id).await.unwrap().is_empty());
}

// ── changes.supersede (M2 pre-bind re-seal reclaim, Piece B) ──────────────────

/// Record two versions of a path, supersede the old one naming the verified
/// head: the reply counts one mark and the superseded row vanishes from
/// `changes.list` (a catching-up device never fetches a reclaimable manifest).
#[tokio::test]
async fn supersede_marks_old_rows_and_hides_them_from_list() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(41);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xd1);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;

    let (m_old, m_head) = (id_hex(0xe1), id_hex(0xe2));
    for mh in [&m_old, &m_head] {
        dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            encode(&common::signed_record(
                SyncChangeRecordRequest {
                    path_sealed: Some(fauna_protocol::ByteBuf::from(
                        b"e2e-synthetic-seal".to_vec(),
                    )),
                    nest_url: None,
                    channel_id: None,
                    folder: "docs".into(),
                    device_id: dev.clone(),
                    path: "pre-bind.txt".into(),
                    manifest_hash: Some(mh.clone()),
                    size_bytes: 16,
                    change_type: "modify".into(),
                    content_key_version: None,
                    thumbnail_hash: None,
                    ..Default::default()
                },
                &kp,
            )),
        )
        .await
        .expect("record ok");
    }

    let reply: SyncChangesSupersedeReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.supersede",
            encode(&SyncChangesSupersedeRequest {
                folder: "docs".into(),
                device_id: dev.clone(),
                path: "pre-bind.txt".into(),
                manifest_hash: m_head.clone(),
                ..Default::default()
            }),
        )
        .await
        .expect("supersede ok"),
    )
    .unwrap();
    assert_eq!(reply.superseded, 1);

    let list: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: Some("docs".into()),
                device_id: None,
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.changes.len(), 1, "superseded row hidden from the feed");
    assert_eq!(
        list.changes[0].manifest_hash.as_deref(),
        Some(m_head.as_str())
    );
}

/// Naming anything but the live head refuses with the typed, retryable
/// head-mismatch error (a concurrent record moved the head, or the caller
/// verified a stale copy) — and mutates nothing.
#[tokio::test]
async fn supersede_stale_manifest_is_typed_head_mismatch() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(42);
    let actor = kp.actor_id().0;
    let dev = id_hex(0xd2);
    register_device(&router, &state, actor, &dev, "laptop").await;
    create_set(&router, &state, actor, "docs").await;

    let (m_old, m_head) = (id_hex(0xe3), id_hex(0xe4));
    for mh in [&m_old, &m_head] {
        dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            encode(&common::signed_record(
                SyncChangeRecordRequest {
                    path_sealed: Some(fauna_protocol::ByteBuf::from(
                        b"e2e-synthetic-seal".to_vec(),
                    )),
                    nest_url: None,
                    channel_id: None,
                    folder: "docs".into(),
                    device_id: dev.clone(),
                    path: "f.bin".into(),
                    manifest_hash: Some(mh.clone()),
                    size_bytes: 16,
                    change_type: "modify".into(),
                    content_key_version: None,
                    thumbnail_hash: None,
                    ..Default::default()
                },
                &kp,
            )),
        )
        .await
        .expect("record ok");
    }

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.supersede",
        encode(&SyncChangesSupersedeRequest {
            folder: "docs".into(),
            device_id: dev.clone(),
            path: "f.bin".into(),
            manifest_hash: m_old.clone(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("stale manifest refused");
    assert_eq!(err.code, "fauna.sync.supersede_head_mismatch");

    // Nothing was marked: both rows still in the feed.
    let list: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: Some("docs".into()),
                device_id: None,
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.changes.len(), 2, "refused supersede mutates nothing");
}

/// Supersede is owner-only: another actor (their own registered device) gets
/// `not_found` for a foreign set name — the ST-RES-1 fold, no existence oracle.
#[tokio::test]
async fn supersede_rejects_non_owner_as_not_found() {
    let (router, state) = router_and_state().await;
    let owner = [43u8; 32];
    let stranger = [44u8; 32];
    let (own_dev, str_dev) = (id_hex(0xd3), id_hex(0xd4));
    register_device(&router, &state, owner, &own_dev, "owner dev").await;
    register_device(&router, &state, stranger, &str_dev, "stranger dev").await;
    create_set(&router, &state, owner, "private").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        stranger,
        "fauna.sync.changes.supersede",
        encode(&SyncChangesSupersedeRequest {
            folder: "private".into(),
            device_id: str_dev,
            path: "x".into(),
            manifest_hash: id_hex(0xff),
            ..Default::default()
        }),
    )
    .await
    .expect_err("non-owner refused");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// A read-only device cannot supersede (the `changes.record` write gate).
#[tokio::test]
async fn supersede_rejects_readonly_device() {
    let (router, state) = router_and_state().await;
    let actor = [45u8; 32];
    let dev = id_hex(0xd5);
    // Register with read-only capabilities.
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: dev.clone(),
            label: "ro".into(),
            capabilities: "read".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("register ok");
    create_set(&router, &state, actor, "docs").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.supersede",
        encode(&SyncChangesSupersedeRequest {
            folder: "docs".into(),
            device_id: dev,
            path: "x".into(),
            manifest_hash: id_hex(0xff),
            ..Default::default()
        }),
    )
    .await
    .expect_err("read-only device refused");
    assert_eq!(err.code, "fauna.sync.permission_denied");
}

/// Only a custody copy never enters `sync_changes`
/// (custody supersedes on upsert), so supersede refuses it as
/// `invalid_request` — while an ordinary Backup folder rides the head feed
/// since the phase 3 head unification (2026-08-17) and takes the normal
/// supersede path (here: `supersede_head_mismatch`, since its head is empty).
#[tokio::test]
async fn supersede_rejects_reserved_backup_destination_but_not_backup_folders() {
    let (router, state) = router_and_state().await;
    let actor = [46u8; 32];
    let dev = id_hex(0xd6);
    register_device(&router, &state, actor, &dev, "laptop").await;
    // A custody copy, minted the way the nest-side provisioners do (no client
    // create can name a reserved set).
    state
        .db
        .create_folder_with_options(
            "__mail",
            &actor,
            fauna_nest::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    create_set(&router, &state, actor, "vault").await;

    let supersede = |folder: &str| {
        encode(&SyncChangesSupersedeRequest {
            folder: folder.into(),
            device_id: dev.clone(),
            path: "x".into(),
            manifest_hash: id_hex(0xff),
            ..Default::default()
        })
    };
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.supersede",
        supersede("__mail"),
    )
    .await
    .expect_err("a custody copy is refused");
    assert_eq!(err.code, "fauna.sync.invalid_request");

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.supersede",
        supersede("vault"),
    )
    .await
    .expect_err("empty head has nothing to mark");
    assert_eq!(
        err.code, "fauna.sync.supersede_head_mismatch",
        "an ordinary folder takes the normal head-feed supersede path"
    );
}

// ── allowlist + replay metadata ──────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_user_admin_only() {
    for kind in ALL_KINDS {
        assert!(
            is_permitted(CallerClass::User, kind),
            "{kind} permitted for User"
        );
        assert!(
            is_permitted(CallerClass::Admin, kind),
            "{kind} permitted for Admin"
        );
        assert!(
            !is_permitted(CallerClass::BridgeMta, kind),
            "{kind} denied for BridgeMta"
        );
        assert!(
            !is_permitted(CallerClass::BridgeMda, kind),
            "{kind} denied for BridgeMda"
        );
    }
}

#[tokio::test]
async fn all_kinds_registered_with_replay_metadata() {
    let (router, _state) = router_and_state().await;
    for kind in ALL_KINDS {
        let meta = router.kind_meta(kind).expect("kind registered");
        assert!(!meta.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(
            meta.default_deadline,
            std::time::Duration::from_secs(5),
            "{kind} @5s"
        );
    }
}

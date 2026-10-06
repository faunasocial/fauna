//! Production-flow round-trip for the version-history surface —
//! `fauna.files.versions.{list,get}` (`docs/goal/behavior/file-sync.md`
//! § File Versions, ratified 2026-07-09).
//!
//! Version history is a projection over the append-only `sync_changes` table,
//! so these tests drive the **production write path** end-to-end: register a
//! device + create a folder + record changes over the real
//! `fauna.sync.changes.record` handler, then read them back through the real
//! versions handlers. (The pre-projection suite seeded the dead `file_versions`
//! table directly and greened while production returned empty for every real
//! file — the classic symbol-exists-but-flow-broken gap. Never reintroduce
//! direct seeding here.)
//!
//! Covered:
//! - record → list: every recorded change is a version, oldest→newest,
//!   `version_num` = the record reply's `seq`;
//! - restore (file-sync.md § Restore): an ordinary `modify` re-pointing the
//!   historical `manifest_hash` becomes the head AND a new version, carrying
//!   the historical `content_key_version` verbatim;
//! - readable-set scoping: a non-member sees nothing (list empty /
//!   get `not_found`); `folder` narrows; an unknown set name is empty, not an
//!   error (no set-name existence oracle);
//! - the label-audience gate (ruled 2026-07-30): owner + roster member see
//!   history; a Q5 admin (AdminDiscovery grant on a group-bound set) does not
//!   — no online existence oracle for a guessed `path_hash`;
//! - the wire guards: wrong-length hash, malformed payload, replay metadata,
//!   the `User | Admin` allowlist.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/files.rs`.
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use fauna_core::identity::ActorKeypair;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::types::ChannelId;
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    files_handlers, folder_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    sync_handlers,
};
use fauna_protocol::{
    ByteBuf, decode_strict as decode,
    files::{
        FileVersionInfo, FilesVersionsGetRequest, FilesVersionsListReply, FilesVersionsListRequest,
    },
    folders::{FolderCreateReply, FolderCreateRequest},
    sync::{
        SyncChangeRecordReply, SyncChangeRecordRequest, SyncRegisterReply, SyncRegisterRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    files_handlers::register_files_handlers(&mut b);
    // The production write path the projection reads.
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    (b.build(), state)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}

/// The recording actor — a real key, so its records sign the way a writer
/// engine signs them (`common::signing_actor`; every record kind refuses an
/// unsigned record `signature_required`).
fn actor_kp() -> ActorKeypair {
    common::signing_actor(11)
}

fn actor() -> [u8; 32] {
    actor_kp().actor_id().0
}
const OTHER_ACTOR: [u8; 32] = [12u8; 32];
const PATH: &str = "notes/todo.md";

fn path_hash() -> [u8; 32] {
    *blake3::hash(PATH.as_bytes()).as_bytes()
}

/// Register a write-capable device + create the `sync`-mode set `name`, the
/// preconditions of the production record path. Returns the set id.
async fn setup_set(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32], name: &str) -> i64 {
    let _: SyncRegisterReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            actor,
            "fauna.sync.register",
            encode(&SyncRegisterRequest {
                device_id: hex(&[0xd1; 32]),
                label: "laptop".into(),
                capabilities: "read,write".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("register ok"),
    )
    .unwrap();
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
        .expect("create set ok"),
    )
    .unwrap();
    reply.id
}

/// Record a change over the real wire, signed by `writer` under the set
/// nonce; returns the reply `seq` (= the version_num the projection exposes).
#[allow(clippy::too_many_arguments)] // test helper mirroring the wire shape 1:1
async fn record(
    router: &RpcRouter,
    state: &Arc<AppState>,
    writer: &ActorKeypair,
    set: &str,
    manifest: &[u8; 32],
    size: i64,
    change_type: &str,
    content_key_version: Option<u64>,
) -> i64 {
    let req = common::signed_record(
        SyncChangeRecordRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            nest_url: None,
            channel_id: None,
            folder: set.into(),
            device_id: hex(&[0xd1; 32]),
            path: PATH.into(),
            manifest_hash: Some(hex(manifest)),
            size_bytes: size,
            change_type: change_type.into(),
            content_key_version,
            thumbnail_hash: None,
            ..Default::default()
        },
        writer,
    );
    let reply: SyncChangeRecordReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            writer.actor_id().0,
            "fauna.sync.changes.record",
            encode(&req),
        )
        .await
        .expect("record ok"),
    )
    .unwrap();
    reply.seq
}

async fn list(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    folder: Option<&str>,
) -> FilesVersionsListReply {
    decode(
        &dispatch(
            router,
            Arc::clone(state),
            actor,
            "fauna.files.versions.list",
            encode(&FilesVersionsListRequest {
                path_hash: ByteBuf::from(path_hash().to_vec()),
                folder: folder.map(str::to_string),
                include_pruned: None,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap()
}

// ── record → list: the production flow ─────────────────────────────────────

#[tokio::test]
async fn recorded_changes_are_listable_versions() {
    let (router, state) = router_and_state().await;
    setup_set(&router, &state, actor(), "docs").await;
    let m1 = [0xa1u8; 32];
    let m2 = [0xa2u8; 32];

    let seq1 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &m1,
        100,
        "create",
        None,
    )
    .await;
    let seq2 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &m2,
        200,
        "modify",
        None,
    )
    .await;

    let reply = list(&router, &state, actor(), None).await;
    assert_eq!(
        reply.versions.len(),
        2,
        "every recorded change is a version"
    );
    assert_eq!(reply.versions[0].version_num, seq1);
    assert_eq!(reply.versions[1].version_num, seq2);
    assert_eq!(reply.versions[0].size_bytes, 100);
    assert_eq!(reply.versions[1].size_bytes, 200);
    assert_eq!(reply.versions[0].manifest_hash.as_ref(), &m1);
    assert_eq!(reply.versions[1].manifest_hash.as_ref(), &m2);
    assert_eq!(reply.versions[0].path_hash.as_ref(), &path_hash());
    assert_eq!(reply.versions[0].folder.as_deref(), Some("docs"));
}

// ── restore: re-point becomes head AND a new version ───────────────────────

#[tokio::test]
async fn restore_record_becomes_head_and_new_version() {
    let (router, state) = router_and_state().await;
    let set_id = setup_set(&router, &state, actor(), "docs").await;
    let m1 = [0xa1u8; 32];
    let m2 = [0xa2u8; 32];

    // v1 sealed under content-key generation 5 (the sealed-set edge).
    let seq1 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &m1,
        100,
        "create",
        Some(5),
    )
    .await;
    record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &m2,
        200,
        "modify",
        Some(6),
    )
    .await;

    // The client-side restore of v1: fetch its metadata, then record an
    // ordinary modify re-pointing at the historical manifest, carrying the
    // historical content_key_version verbatim (file-sync.md § Restore).
    let v1: FileVersionInfo = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor(),
            "fauna.files.versions.get",
            encode(&FilesVersionsGetRequest {
                path_hash: ByteBuf::from(path_hash().to_vec()),
                version_num: seq1,
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(v1.content_key_version, Some(5), "generation echoed on read");

    let mut m1_arr = [0u8; 32];
    m1_arr.copy_from_slice(v1.manifest_hash.as_ref());
    let restore_seq = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &m1_arr,
        v1.size_bytes,
        "modify",
        v1.content_key_version,
    )
    .await;

    // The restore is a NEW version (append-only history — reversible)...
    let reply = list(&router, &state, actor(), None).await;
    assert_eq!(reply.versions.len(), 3);
    let head = reply.versions.last().unwrap();
    assert_eq!(head.version_num, restore_seq);
    // ...whose content IS v1's, generation included...
    assert_eq!(head.manifest_hash.as_ref(), &m1);
    assert_eq!(head.content_key_version, Some(5));
    // ...and the pre-restore head (m2) is still listed and restorable.
    assert!(
        reply
            .versions
            .iter()
            .any(|v| v.manifest_hash.as_ref() == m2)
    );
    // The files projection (what member devices sync) now heads at m1.
    let files = state.db.get_files_for_folder(set_id).await.unwrap();
    let f = files
        .iter()
        .find(|f| f.path_hash == fauna_core::sync::path_hash(PATH).to_vec())
        .expect("file listed");
    assert_eq!(f.size_bytes, 100, "head re-pointed to the restored version");
}

// ── readable-set scoping ────────────────────────────────────────────────────

#[tokio::test]
async fn versions_scoped_to_readable_sets() {
    let (router, state) = router_and_state().await;
    setup_set(&router, &state, actor(), "docs").await;
    let m1 = [0xa1u8; 32];
    let seq1 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &m1,
        100,
        "create",
        None,
    )
    .await;

    // A different actor with no membership sees nothing…
    let reply = list(&router, &state, OTHER_ACTOR, None).await;
    assert!(reply.versions.is_empty(), "no cross-user leak");
    // …and get maps unauthorized to not_found (no existence oracle).
    let err = dispatch(
        &router,
        Arc::clone(&state),
        OTHER_ACTOR,
        "fauna.files.versions.get",
        encode(&FilesVersionsGetRequest {
            path_hash: ByteBuf::from(path_hash().to_vec()),
            version_num: seq1,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unauthorized get → not_found");
    assert_eq!(err.code, "fauna.files.not_found");

    // folder narrows: the same path in a second owned set is out of scope.
    setup_set(&router, &state, actor(), "other").await;
    record(
        &router,
        &state,
        &actor_kp(),
        "other",
        &[0xb1; 32],
        300,
        "create",
        None,
    )
    .await;
    let scoped = list(&router, &state, actor(), Some("docs")).await;
    assert_eq!(scoped.versions.len(), 1);
    assert_eq!(scoped.versions[0].folder.as_deref(), Some("docs"));
    let unioned = list(&router, &state, actor(), None).await;
    assert_eq!(unioned.versions.len(), 2, "unscoped unions readable sets");

    // The hash address alone narrows the same way (S5b): no plaintext name on
    // the request, and it outranks a name that disagrees with it.
    let by_hash: FilesVersionsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor(),
            "fauna.files.versions.list",
            encode(&FilesVersionsListRequest {
                path_hash: ByteBuf::from(path_hash().to_vec()),
                folder: Some("other".into()),
                name_hash: Some(ByteBuf::from(
                    fauna_core::path_crypto::set_name_hash("docs").to_vec(),
                )),
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(by_hash.versions.len(), 1);
    assert_eq!(by_hash.versions[0].folder.as_deref(), Some("docs"));

    // An unknown set name is empty, not an error (no set-name oracle).
    let unknown = list(&router, &state, actor(), Some("nope")).await;
    assert!(unknown.versions.is_empty());
}

// ── the label-audience gate (paths-are-content; ruled 2026-07-30) ───────────
//
// Version history is content metadata, not the discovery metadata the Q5
// admin grant is scoped to (`encryption-at-rest.md` § Carve-outs), and an
// admin-reachable reply would be an online existence oracle for a guessed
// path hash — post-flip strictly worse than the offline salt leaks S5d/S5e
// closed. The handlers keep owner + roster member (`is_label_audience`) and
// drop `AdminDiscovery` from the scope; the Q5 grant itself is unchanged.

/// Group-bind `name` (the post-share state) and put `members` on the derived
/// roster — the fixture the audience gate needs (`conformance_media_list.rs`'s
/// `bind_shared`, on top of this file's production-path `setup_set`).
async fn bind_shared(
    state: &Arc<AppState>,
    name: &str,
    owner: &[u8; 32],
    group_id: &[u8],
    members: &[[u8; 32]],
) {
    state
        .db
        .set_folder_mls_group(name, owner, Some(group_id))
        .await
        .unwrap();
    let channel_id = ChannelId::from_group_id(group_id).0;
    state
        .db
        .register_actor_channel(owner, &channel_id)
        .await
        .unwrap();
    for m in members {
        state
            .db
            .register_actor_channel(m, &channel_id)
            .await
            .unwrap();
    }
}

/// The audience arms: the owner and a roster member of a group-bound set both
/// keep full version history — withholding from members would be a regression,
/// not a fix.
#[tokio::test]
async fn versions_ship_to_the_owner_and_to_a_roster_member() {
    let (router, state) = router_and_state().await;
    setup_set(&router, &state, actor(), "docs").await;
    let seq = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &[0xa1; 32],
        100,
        "create",
        None,
    )
    .await;
    let member = [0x33u8; 32];
    bind_shared(&state, "docs", &actor(), &[0x5d; 24], &[member]).await;

    for (who, label) in [(actor(), "owner"), (member, "roster member")] {
        let reply = list(&router, &state, who, None).await;
        assert_eq!(reply.versions.len(), 1, "{label} sees the history");
        assert_eq!(reply.versions[0].version_num, seq);
        let got: FileVersionInfo = decode(
            &dispatch(
                &router,
                Arc::clone(&state),
                who,
                "fauna.files.versions.get",
                encode(&FilesVersionsGetRequest {
                    path_hash: ByteBuf::from(path_hash().to_vec()),
                    version_num: seq,
                    extra: Default::default(),
                }),
            )
            .await
            .unwrap_or_else(|e| panic!("{label} get ok, got {e:?}")),
        )
        .unwrap();
        assert_eq!(got.version_num, seq, "{label} gets the version");
    }
}

/// The non-audience arm. A Q5 admin still *discovers* the group-bound set on
/// the discovery surfaces (the grant is unchanged), but version history —
/// path existence + manifest + author + timestamps — is withheld: list is
/// empty and get is `not_found`, indistinguishable from a missing version.
#[tokio::test]
async fn versions_are_withheld_from_a_q5_admin_who_is_not_the_audience() {
    let (router, state) = router_and_state().await;
    setup_set(&router, &state, actor(), "docs").await;
    let seq = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &[0xa1; 32],
        100,
        "create",
        None,
    )
    .await;
    bind_shared(&state, "docs", &actor(), &[0x5e; 24], &[]).await;
    let admin = [0xadu8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();

    let reply = list(&router, &state, admin, None).await;
    assert!(
        reply.versions.is_empty(),
        "a Q5 admin is not the label audience — version history is withheld"
    );

    let err = dispatch(
        &router,
        Arc::clone(&state),
        admin,
        "fauna.files.versions.get",
        encode(&FilesVersionsGetRequest {
            path_hash: ByteBuf::from(path_hash().to_vec()),
            version_num: seq,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("non-audience get → not_found, no oracle");
    assert_eq!(err.code, "fauna.files.not_found");
}

#[tokio::test]
async fn list_empty_for_unknown_path() {
    let (router, state) = router_and_state().await;
    setup_set(&router, &state, actor(), "docs").await;
    let reply: FilesVersionsListReply = decode(
        &dispatch(
            &router,
            state,
            actor(),
            "fauna.files.versions.list",
            encode(&FilesVersionsListRequest {
                path_hash: ByteBuf::from(vec![0x99; 32]),
                folder: None,
                include_pruned: None,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(reply.versions.is_empty());
}

// ── wire guards (unchanged from the transport migration) ────────────────────

#[tokio::test]
async fn list_rejects_wrong_length_path_hash() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        actor(),
        "fauna.files.versions.list",
        encode(&FilesVersionsListRequest {
            path_hash: ByteBuf::from(vec![0x01; 16]), // not 32 bytes
            folder: None,
            include_pruned: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("short path_hash rejected");
    assert_eq!(err.code, "fauna.files.invalid_request");
}

#[tokio::test]
async fn get_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        actor(),
        "fauna.files.versions.get",
        encode(&FilesVersionsGetRequest {
            path_hash: ByteBuf::from(path_hash().to_vec()),
            version_num: 999,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("missing version → not_found");
    assert_eq!(err.code, "fauna.files.not_found");
}

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.files.versions.list",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in [
        "fauna.files.versions.list",
        "fauna.files.versions.get",
        "fauna.files.versions.undelete",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for kind in [
        "fauna.files.versions.list",
        "fauna.files.versions.get",
        "fauna.files.versions.undelete",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}

// ── The retention pipeline's wire surface (file-versions.md § Retention (3)) ──

/// The recovery flow over the real handlers: a soft-pruned version leaves the
/// default `versions.list` projection, rides the `include_pruned` recovery
/// browse with its `pruned`/`purge_after` markers, stays fetchable by `get`
/// (restore during the window), and `fauna.files.versions.undelete` restores
/// it — owner-only, with a non-owner getting the reads' `not_found` (no
/// oracle on the mutation either).
#[tokio::test]
async fn soft_pruned_versions_recovery_browse_and_undelete() {
    let (router, state) = router_and_state().await;
    setup_set(&router, &state, actor(), "docs").await;
    let seq1 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &[0xA1; 32],
        100,
        "create",
        None,
    )
    .await;
    let _seq2 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &[0xA2; 32],
        200,
        "modify",
        None,
    )
    .await;
    let _seq3 = record(
        &router,
        &state,
        &actor_kp(),
        "docs",
        &[0xA3; 32],
        300,
        "modify",
        None,
    )
    .await;

    // The executor's stage, driven directly — the schedule/execute legs are
    // pinned at the lib tier (backup::version_prune + db::sync_storage).
    assert!(state.db.soft_prune_version(seq1).await.unwrap());

    // Default projection: the pruned version is gone, and no row carries the
    // recovery markers (an ordinary listing's wire bytes are unchanged).
    let default_list = list(&router, &state, actor(), Some("docs")).await;
    assert_eq!(default_list.versions.len(), 2);
    assert!(default_list.versions.iter().all(|v| v.pruned.is_none()));

    // Recovery browse: a superset, the pruned row marked with its deadline.
    let browse: FilesVersionsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor(),
            "fauna.files.versions.list",
            encode(&FilesVersionsListRequest {
                path_hash: ByteBuf::from(path_hash().to_vec()),
                folder: Some("docs".into()),
                include_pruned: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("recovery browse ok"),
    )
    .unwrap();
    assert_eq!(browse.versions.len(), 3);
    let pruned_row = browse
        .versions
        .iter()
        .find(|v| v.version_num == seq1)
        .unwrap();
    assert_eq!(pruned_row.pruned, Some(true));
    assert!(pruned_row.purge_after.is_some());

    // `get` still serves the pruned version — restore-as-re-point reads its
    // manifest here while the window is open.
    let got: FileVersionInfo = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor(),
            "fauna.files.versions.get",
            encode(&FilesVersionsGetRequest {
                path_hash: ByteBuf::from(path_hash().to_vec()),
                version_num: seq1,
                extra: Default::default(),
            }),
        )
        .await
        .expect("get serves a soft-pruned version"),
    )
    .unwrap();
    assert_eq!(got.manifest_hash.as_ref(), &[0xA1; 32]);

    // A non-owner's undelete is the reads' not_found — never an oracle, and
    // never a mutation.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        OTHER_ACTOR,
        "fauna.files.versions.undelete",
        encode(&fauna_protocol::files::FilesVersionsUndeleteRequest {
            path_hash: ByteBuf::from(path_hash().to_vec()),
            version_num: seq1,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("non-owner undelete → not_found");
    assert_eq!(err.code, "fauna.files.not_found");

    // The owner's undelete restores the row to the listable population.
    let reply: fauna_protocol::files::FilesVersionsUndeleteReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor(),
            "fauna.files.versions.undelete",
            encode(&fauna_protocol::files::FilesVersionsUndeleteRequest {
                path_hash: ByteBuf::from(path_hash().to_vec()),
                version_num: seq1,
                extra: Default::default(),
            }),
        )
        .await
        .expect("owner undelete ok"),
    )
    .unwrap();
    assert!(reply.undeleted);
    assert_eq!(
        list(&router, &state, actor(), Some("docs"))
            .await
            .versions
            .len(),
        3
    );

    // A second undelete has nothing to restore — same not_found shape.
    let err = dispatch(
        &router,
        state,
        actor(),
        "fauna.files.versions.undelete",
        encode(&fauna_protocol::files::FilesVersionsUndeleteRequest {
            path_hash: ByteBuf::from(path_hash().to_vec()),
            version_num: seq1,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("not currently pruned → not_found");
    assert_eq!(err.code, "fauna.files.not_found");
}

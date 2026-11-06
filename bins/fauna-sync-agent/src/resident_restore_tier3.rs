//! Tier_3: the **`RestoreFileVersion` verb on an always-resident root**, driven
//! end to end through a REAL nest to the restored bytes on the disk
//! (`docs/goal/behavior/file-versions.md` § Restore, ordering 2's kept-copy
//! branch, and § Implementation status today).
//!
//! Every link of a resident restore is pinned on its own elsewhere: the verb
//! passing the resolver (`pipe_server::tests::the_version_verbs_reach_the_nest_for_a_resident_root`),
//! the record against a real nest (`versions_tier3.rs`, which calls
//! `versions::restore_file_version` directly), the owed-restore intent
//! (`pipe_server::tests::owe_restore_writes_the_intent_only_for_a_resident_row`),
//! the hand-off to the root's loop
//! (`pipe_server::tests::a_restore_on_an_always_resident_root_takes_the_kept_copy_road`)
//! and the loop's write (`fauna-sync-engine`'s `resident_restore_test`, over a
//! mock nest). This module runs them as one: the verb records the restore on
//! the nest and the root's own loop writes the restored version in over the
//! copy.
//!
//! ## What is real here, and what is not
//!
//! **Real:** the nest — a `fauna-nest` router on an ephemeral port, byte plane
//! included (`fauna_nest::test_support::start_test_nest`, the
//! `restore_byteplane_tier3.rs` idiom). The verb —
//! `pipe_server::handle_request(RestoreFileVersion)`, which takes its client from
//! `SyncServiceState::nest_rpc_client()`, a WS connection opened from the
//! provisioned capability, exactly as in production. Everything the verb does
//! after that is the shipped code: the resolver, the `owe` intent, the re-seal
//! seam, the record, and the hand-off (`apply_restore_locally` →
//! `restore_kept_copy`). The root's engine is built from the agent's own inputs
//! (`bridge::agent_engine_params`), its versions come from its own uploader, and
//! its loop is `always_resident::run_watch_loop`, which answers the restore
//! through `SyncEngine::replace_kept_copy`.
//!
//! **Arranged by the test, three things:**
//! - The records' signer. The nest takes only writer-signed records, so the
//!   set's nonce is stored on it and held by the agent, and both the engine and
//!   the verb sign with the owner's identity key directly
//!   (`ChangeSigner::direct`), handed to the verb through the agent's one signer
//!   source (`SyncServiceState::change_signer_source`) rather than read from the
//!   developer's credential store. The delegated chain (a principal writer key
//!   and its `SyncWrite` grant) is the nest's own tests' subject. The first run
//!   of this test found the verb recording its restore unsigned, refused
//!   `signature_required` by every nest; the verb now signs through
//!   `SyncServiceState::record_signing`, like the custodian's re-seed.
//! - The engine is built by the construction half
//!   (`engine_lifecycle::assemble_engine`) for an owner-only set on the own nest,
//!   not by the driver's reconcile. The reconcile is the agent process harness's
//!   subject (`tests/agent_process_tier3.rs`, unix), not the restore's.
//! - The root's command sender is registered through a tap. When the hand-off
//!   arrives, the tap reads the owed-restore table and passes the command on
//!   unchanged. The intent exists only between its write ahead of the record and
//!   the loop's write, and nothing outside the verb can see it there except a
//!   reader on that channel.
//!
//! ## Run
//!
//! Opt-in (pulls the whole `fauna-nest` crate, so it stays off the default test
//! loop):
//!
//! ```text
//! cargo-win.cmd test -p fauna-sync-agent --features tier3-nest resident_restore_tier3
//! ```

use std::sync::Arc;
use std::time::Duration;

use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderRef;
use fauna_core::identity::ActorKeypair;
use fauna_ipc::sync::{
    BearerToken, FileVersionEntry, Request, RequestMethod, ResponsePayload, ResponseResult,
    SyncCapability,
};
use fauna_sync_engine::always_resident::EngineCommand;
use fauna_sync_engine::db::{OwedRestore, SyncDb, SyncState};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::config::{LocationConfig, LocationMode, SyncConfig, SyncPaths};
use crate::pipe_server::handle_request;
use crate::state::SyncServiceState;

/// The owner's identity seed: the nest registers its actor and mints the bearer
/// the capability carries.
const OWNER_SECRET: [u8; 32] = [0x42; 32];
const DEVICE_ID: [u8; 32] = [0x07; 32];
/// The capability's `BackupKey`, which the engine seals with (an owner-only set).
const BACKUP_KEY: [u8; 32] = [0x3c; 32];
const FOLDER: &str = "docs";
/// The set's nonce: stored on the nest and held by the agent, so a record signed
/// under it verifies there (`writer-signed-change-records.md`).
const SET_NONCE: [u8; 32] = [0x5e; 32];
/// In a subfolder, so the verb's Windows path and the engine's forward-slash
/// relative path must meet in the resolver.
const REL: &str = "notes/report.txt";
const V1: &[u8] = b"v1 - the version the user restores";
const V2: &[u8] = b"v2 - the version the restore replaces, longer than the first";
/// A generous ceiling on each wait (convention 14): a green run pays only the
/// real latency, and a hang fails loudly instead of holding the suite.
const BUDGET: Duration = Duration::from_secs(120);

/// `path`'s version history through the agent's own `ListFileVersions` verb.
async fn versions_of(state: &Arc<SyncServiceState>, path: &str) -> Vec<FileVersionEntry> {
    let resp = handle_request(
        &Request {
            id: 1,
            method: RequestMethod::ListFileVersions { path: path.into() },
        },
        state,
    )
    .await;
    match resp.result {
        ResponseResult::Ok(ResponsePayload::FileVersionList(info)) => info.versions,
        other => panic!("ListFileVersions: expected a version list, got {other:?}"),
    }
}

/// **The row's definition of success.** A tracked file on an always-resident
/// root has two versions on a real nest, both recorded by the root's own
/// uploader. `RestoreFileVersion` on the older one writes the intent, records the
/// restore through the capability's WS connection and hands the restored head to
/// the root's loop, whose `replace_kept_copy` writes the version in over the
/// copy. Afterwards the restored bytes are on the disk, the row is `Synced` at
/// the restored head and signed as the identity the restore recorded as, the
/// intent is gone, and nothing more was recorded.
#[tokio::test]
async fn restore_file_version_on_a_resident_root_writes_the_restored_bytes_through_a_real_nest() {
    // ── A real nest, byte plane included, holding the set. ──
    let blob_dir = tempfile::tempdir().expect("blob dir");
    let nest = fauna_nest::test_support::start_test_nest(
        OWNER_SECRET,
        Some(blob_dir.path().to_path_buf()),
        |b| {
            fauna_nest::sync_handlers::register_sync_handlers(b);
            fauna_nest::folder_handlers::register_folders_handlers(b);
            // `fauna.files.versions.*`: the version list and the restore's lookup.
            fauna_nest::files_handlers::register_files_handlers(b);
        },
    )
    .await;
    let actor = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let folder_id = nest
        .state
        .db
        .create_folder(FOLDER, &actor)
        .await
        .expect("create the set on the nest");
    let folder_ref = FolderRef::Local(folder_id);
    // The owner's custody reconcile pushes the set's nonce; every record the
    // nest takes for the set is verified under it.
    assert!(
        nest.state
            .db
            .update_folder_by_id(
                folder_id,
                fauna_nest::db::FolderUpdate {
                    set_nonce: Some(&SET_NONCE),
                    ..Default::default()
                },
            )
            .await
            .expect("store the set nonce"),
        "the set's row exists"
    );
    // The records' signer. The identity key signs directly, so the nest needs no
    // device grant to verify it; the delegated chain is the nest's own tests'.
    let signer = Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
        &ActorKeypair::from_secret(OWNER_SECRET),
    ));

    // ── The agent: one always-resident location bound to the set, provisioned. ──
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("notes")).expect("create the root");
    let paths = SyncPaths::at(tmp.path().join("data"));
    let db_path = paths.sync_db_path_for_ref(folder_ref);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create the state DB dir");
    let config = SyncConfig {
        locations: vec![LocationConfig {
            path: root.to_string_lossy().to_string(),
            mode: LocationMode::Always,
            folder: Some(FOLDER.into()),
            folder_id: Some(folder_ref.to_wire()),
            ..Default::default()
        }],
        ..SyncConfig::default()
    };
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let (event_tx, _event_rx) = broadcast::channel(64);
    let state = SyncServiceState::new(config, shutdown_tx, event_tx, paths.clone());
    *state.capability.write().await = Some(SyncCapability::new(
        BACKUP_KEY.to_vec(),
        actor.to_vec(),
        nest.base_url.clone(),
        hex::encode(DEVICE_ID),
        BearerToken::new(nest.http_token.clone(), 4_000_000_000),
    ));
    // The verb signs from the agent's one signer source and its own content-key
    // resolution — here this test's signer, and a resolution naming the set.
    *state
        .change_signer_source
        .write()
        .expect("the signer source") = crate::state::ChangeSignerSource::Fixed(Arc::clone(&signer));
    *state.content_keys.write().await = Some(crate::content_keys::ResolvedContentKeys::for_tests(
        vec![fauna_core::folder_keys::FolderEngineKeys {
            folder: FOLDER.to_string(),
            folder_id: folder_ref.to_wire(),
            set_nonce: Some(SET_NONCE),
            ..Default::default()
        }],
        &[],
    ));

    // ── The root's engine, from the agent's own inputs, over the same capability. ──
    let engine = fauna_sync_engine::engine_lifecycle::assemble_engine(
        crate::bridge::agent_engine_params(
            crate::bridge::agent_control_plane(
                Arc::clone(&state.capability),
                nest.base_url.clone(),
                actor,
                DEVICE_ID,
            ),
            paths.base_dir(),
            root.clone(),
            folder_ref,
            DEVICE_ID,
            BACKUP_KEY,
            &crate::bridge::AgentPredecessors::default(),
            None,
            fauna_sync_engine::access_gate::AccessGate::new(),
            Some(Arc::clone(&signer)),
            Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
        ),
        fauna_sync_engine::engine_lifecycle::ResolvedBinding {
            folder: FOLDER.to_string(),
            set_nonce: Some(SET_NONCE),
            ..Default::default()
        },
    )
    .expect("build the root's engine against the real nest")
    .engine;
    engine
        .connect_control_plane()
        .await
        .expect("open the engine's control plane");

    // ── Two versions, each uploaded and recorded by the root's own uploader. ──
    let file = root.join(REL.replace('/', "\\"));
    let abs = file.to_string_lossy().to_string();
    std::fs::write(&file, V1).expect("write v1");
    assert!(
        engine.upload_file(REL).await.expect("upload v1").recorded,
        "v1's record reached the nest"
    );
    let m1 = engine
        .db()
        .get_entry(REL)
        .expect("db read")
        .and_then(|e| e.manifest_hash)
        .expect("v1's head");
    std::fs::write(&file, V2).expect("write v2");
    assert!(
        engine.upload_file(REL).await.expect("upload v2").recorded,
        "v2's record reached the nest"
    );
    let at_v2 = engine
        .db()
        .get_entry(REL)
        .expect("db read")
        .expect("the row");
    assert_eq!(
        (at_v2.state, at_v2.recorded_content_hash),
        (SyncState::Synced, Some(ContentHash::of_raw(V2))),
        "the premise: a synced copy whose row proves v2 recorded"
    );
    assert_ne!(
        at_v2.manifest_hash,
        Some(m1),
        "the premise: the row is at v2"
    );

    let before = versions_of(&state, &abs).await;
    assert_eq!(before.len(), 2, "both uploads are listable versions");
    let v1_num = before[0].version_num;

    // ── The root's loop, behind a tap on its command channel. ──
    // `SyncEngine` is `!Sync` and the loop's future `!Send`: everything below is
    // raced in this one task, so the `Arc` never crosses a thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = Arc::new(engine);
    let (loop_tx, loop_rx) = mpsc::channel(1);
    let (tap_tx, mut tap_rx) = mpsc::channel(1);
    state
        .engine_cmd_senders
        .lock()
        .await
        .insert(folder_ref.to_wire(), 0, tap_tx);
    let looped = fauna_sync_engine::always_resident::run_watch_loop(
        Arc::clone(&engine),
        root.clone(),
        FOLDER.to_string(),
        Duration::from_secs(3600),
        None,
        Some(loop_rx),
    );
    tokio::pin!(looped);

    // The loop reaches its command arm only past its start passes, one of which
    // drains owed restores. A free, which a resident root refuses, is answered
    // there, so its answer marks the passes done: the intent below is the verb's
    // alone to settle.
    let (reply, refused) = oneshot::channel();
    loop_tx
        .send(EngineCommand::FreeSpace {
            rel: REL.to_string(),
            reply,
        })
        .await
        .expect("the channel buffers the command");
    tokio::select! {
        _ = &mut looped => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(BUDGET, refused) => {
            assert!(
                got.expect("the loop reaches its command arm")
                    .expect("the arm replies")
                    .is_err(),
                "a resident root refuses a free"
            );
        }
    }

    // ── The verb. ──
    let tap = async {
        let Some(EngineCommand::ApplyStaleHydrated { row, reply }) = tap_rx.recv().await else {
            panic!("the restore's only command to the root is its restored head");
        };
        let owed = SyncDb::open(&db_path)
            .expect("open the root's state DB")
            .owed_restores()
            .expect("db read");
        loop_tx
            .send(EngineCommand::ApplyStaleHydrated { row, reply })
            .await
            .expect("the loop holds its receiver");
        owed
    };
    let restore_request = Request {
        id: 2,
        method: RequestMethod::RestoreFileVersion {
            path: abs.clone(),
            version_num: v1_num,
        },
    };
    // The verb cannot answer `Ok` before the tap has passed its head on: the
    // kept-copy road waits for the loop's answer. So a verb that answers first
    // failed, and says why at once instead of leaving the tap to the budget.
    let restore = async {
        let verb = handle_request(&restore_request, &state);
        tokio::pin!(verb);
        tokio::pin!(tap);
        tokio::select! {
            resp = &mut verb => panic!(
                "the restore answered before handing the root its head: {:?}",
                resp.result
            ),
            owed = &mut tap => (verb.await, owed),
        }
    };
    let (resp, owed_at_hand_off) = tokio::select! {
        _ = &mut looped => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(BUDGET, restore) => got.expect("the restore completes"),
    };
    assert!(
        matches!(resp.result, ResponseResult::Ok(ResponsePayload::Empty)),
        "the restore succeeds, got {:?}",
        resp.result
    );

    // ── The intent: written for the resident location, settled by the write. ──
    assert_eq!(
        owed_at_hand_off.len(),
        1,
        "the verb wrote one intent ahead of the record"
    );
    let OwedRestore {
        path,
        manifest_hash,
        size_bytes,
        signed_as,
        ..
    } = &owed_at_hand_off[0];
    assert_eq!(
        (path.as_str(), *manifest_hash, *size_bytes, *signed_as),
        (REL, m1, V1.len() as i64, Some(actor)),
        "the intent names the restored head and the identity the restore recorded as"
    );
    assert!(
        engine.db().owed_restores().expect("db read").is_empty(),
        "replace_kept_copy settles the intent once the bytes are written"
    );

    // ── The restored version on the disk, and the row at its head. ──
    assert_eq!(
        std::fs::read(&file).expect("the copy"),
        V1,
        "the restored version is on the disk"
    );
    let entry = engine
        .db()
        .get_entry(REL)
        .expect("db read")
        .expect("the row");
    assert_eq!(entry.state, SyncState::Synced, "the file stays on the disk");
    assert_eq!(
        entry.manifest_hash,
        Some(m1),
        "the row is at the restored head"
    );
    assert_eq!(
        entry.recorded_content_hash,
        Some(ContentHash::of_raw(V1)),
        "the row proves the bytes on the disk are the restored head's"
    );
    assert_eq!(
        entry.head_signed_as,
        Some(actor),
        "the head is signed as the identity the restore recorded as"
    );

    // ── Nothing uploaded: the restore is the only new record. ──
    let after = versions_of(&state, &abs).await;
    assert_eq!(
        after.len(),
        3,
        "the restore appended one version and nothing else was recorded"
    );
    assert_eq!(
        after[2].size_bytes,
        V1.len() as i64,
        "the new head is the restored version"
    );
    assert!(
        engine
            .upload_file(REL)
            .await
            .expect("the uploader's pass")
            .recorded,
        "the uploader finds the restored bytes recorded, not an edit"
    );
    assert_eq!(
        versions_of(&state, &abs).await.len(),
        3,
        "the uploader's pass recorded nothing over the restore"
    );
}

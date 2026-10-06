//! Producer-integration harness for the overlay-status query path.
//!
//! Drives the **real** pipe handlers ([`handle_request`]) end-to-end against an
//! on-disk per-folder SQLite DB, closing the one seam the producer story
//! deliberately left untested: [`lookup_entry`] /
//! [`handle_get_file_status`] are documented as "thin OS-glue (DB open via
//! the resolved data-root), kept untested" while the *pure* routing/mapping
//! (`path_map::resolve_to_folder_rel`, `path_map::file_status_from_state`) is
//! unit-tested in [`crate::path_map`]. This harness wires those pieces together:
//! add → bind → on-demand via the real handlers, a real `SyncDb` row, then assert
//! `GetFileStatus` returns the right overlay [`FileStatus`].
//!
//! # Scope — the IPC/config half; the cfapi half lives next door
//!
//! This harness covers the **query** side without ever starting an engine: every test keeps at
//! least one hydration precondition unsatisfied, so `reconcile_engines` plans nothing and no
//! cfapi sync root is registered. What it deterministically covers: the absolute-path →
//! folder-DB query routing, the `SyncState` → `FileStatus` overlay mapping read back through
//! the real IPC handler, and the add/bind/mode + config-persistence round-trip.
//!
//! ⚠ **This module used to claim the cfapi half was "not headless-runnable"** — that a real
//! Cloud Filter sync-root registration "needs a live cloud-filter session", so the engine-driven
//! population and the overlay flip could only ever be checked by a human in Explorer. **That was
//! wrong, and the cost was high**: it is precisely the missing test it justified that let the
//! fatal `TRANSFER_PLACEHOLDERS` `FileIdentity` bug survive unseen (Windows on-demand sync had
//! *never once worked*), with three wrong diagnoses stacking up behind the unexercised call.
//! A sync root registers on a temp dir in milliseconds. See
//! [`crate::cfapi_live_integration`], which drives the **real** `register_and_connect` →
//! callbacks → `run_hydration_loop` path against a live sync root in ~0.5 s, no human.
//! *Lesson: a boundary drawn around an assumption instead of a measurement is where bugs live.*

use std::sync::Arc;

use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{
    BearerToken, FileStatus, Request, RequestMethod, ResponsePayload, ResponseResult,
    SyncCapability,
};
use fauna_sync_engine::db::{SyncDb, SyncState};

use crate::config::{SyncConfig, SyncPaths};
use crate::pipe_server::handle_request;
use crate::state::SyncServiceState;

/// Build service state with no capability and a `--data-dir` override [`SyncPaths`]
/// so config saves + state DBs land under the test's temp root (the real injection
/// seam, not an env-var redirect). `tokio` channels are kept alive via the returned
/// receivers' senders inside the state; the dropped receiver halves are fine (no
/// consumer needed here).
fn state(paths: SyncPaths) -> Arc<SyncServiceState> {
    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    SyncServiceState::new(SyncConfig::default(), shutdown_tx, event_tx, paths)
}

async fn call(state: &Arc<SyncServiceState>, id: u64, method: RequestMethod) -> ResponseResult {
    handle_request(&Request { id, method }, state).await.result
}

/// Seed a real per-folder `SyncDb` with one row at `rel`, opening it via the
/// same [`SyncPaths`] data-root the production query path resolves through, so the
/// handler reads exactly what we wrote.
/// The bound set's identity: the agent keys its engine and state DB by it.
const DOCS: FolderRef = FolderRef::Local(1);

fn seed_db_row(paths: &SyncPaths, set: FolderRef, rel: &str, state: SyncState, size_bytes: i64) {
    let db_path = paths.sync_db_path_for_ref(set);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
    let db = SyncDb::open(&db_path).expect("open per-folder db");
    db.upsert_entry(rel, None, None, None, state, 0, 0, size_bytes, 1, None)
        .expect("seed sync entry");
    // Drop the writer connection before the handler opens its own (cleaner than
    // relying on WAL concurrency for the assertion).
    drop(db);
}

/// The core new coverage: the add → bind → on-demand round-trip persists config,
/// and `GetFileStatus` reads a real Placeholder/Synced row back through the live
/// `lookup_entry` glue, mapping it to the correct overlay status. No hydration
/// host is started (no capability provisioned), so the cfapi path never fires.
#[tokio::test]
async fn get_file_status_reports_overlay_state_from_real_per_folder_db() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));

    // No capability → reconcile_engines provably no-ops
    // (no engine, no cfapi sync-root registration).
    let state = state(paths.clone());

    let folder = r"C:\od-docs";

    // 1. Drive the real add → bind → on-demand handlers.
    assert!(matches!(
        call(
            &state,
            1,
            RequestMethod::AddLocation {
                path: folder.into()
            }
        )
        .await,
        ResponseResult::Ok(ResponsePayload::Empty)
    ));
    assert!(matches!(
        call(
            &state,
            2,
            RequestMethod::SetLocationFolder {
                path: folder.into(),
                folder: "docs".into(),
                folder_id: DOCS.to_wire(),
            },
        )
        .await,
        ResponseResult::Ok(ResponsePayload::Empty)
    ));
    assert!(matches!(
        call(
            &state,
            3,
            RequestMethod::SetLocationSyncMode {
                path: folder.into(),
                mode: "on-demand".into(),
            },
        )
        .await,
        ResponseResult::Ok(ResponsePayload::Empty)
    ));

    // The host must NOT have started — no capability was provisioned.
    assert!(
        state.engines.lock().await.is_none(),
        "no capability → hydration host (and its cfapi sync root) must not start"
    );

    // 2. The add/bind/mode round-trip persisted via the `--data-dir` override:
    //    config.toml physically landed under the temp root (not the machine
    //    default), and reloading it confirms the bound on-demand folder survived.
    assert!(
        tmp.path().join("config.toml").is_file(),
        "config.toml must be written under the --data-dir override root"
    );
    let persisted = paths.load_config().expect("reload persisted config");
    let saved = persisted
        .locations
        .iter()
        .find(|f| f.path == folder)
        .expect("folder persisted to config.toml");
    assert_eq!(saved.folder.as_deref(), Some("docs"));
    assert_eq!(saved.folder_id, Some(DOCS.to_wire()));
    assert_eq!(saved.mode, crate::config::LocationMode::OnDemand);

    // 3. Seed a real CloudOnly placeholder row and read it back through the
    //    handler — Placeholder → CloudOnly is the live overlay-query path. The
    //    per-folder state DB likewise lands under the override root.
    seed_db_row(&paths, DOCS, "sub/a.txt", SyncState::Placeholder, 4096);
    let fs_db = paths.sync_db_path_for_ref(DOCS);
    assert!(
        fs_db.is_file() && fs_db.starts_with(tmp.path()),
        "per-set fsid-*.db must be written under the --data-dir override root"
    );
    let cloud_only = format!(r"{folder}\sub\a.txt");
    match call(
        &state,
        4,
        RequestMethod::GetFileStatus {
            path: cloud_only.clone(),
        },
    )
    .await
    {
        ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
            assert_eq!(
                info.status,
                FileStatus::CloudOnly,
                "placeholder → CloudOnly"
            );
            assert_eq!(info.size_bytes, 4096);
            assert!(!info.is_pinned);
            assert_eq!(info.path, cloud_only);
        }
        other => panic!("expected FileStatus payload, got {other:?}"),
    }

    // 4. Hydrate the same row (Placeholder → Synced) and confirm the query flips
    //    to Synced — the symmetric overlay state the hydrate producer emits.
    seed_db_row(&paths, DOCS, "sub/a.txt", SyncState::Synced, 4096);
    match call(
        &state,
        5,
        RequestMethod::GetFileStatus {
            path: cloud_only.clone(),
        },
    )
    .await
    {
        ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
            assert_eq!(info.status, FileStatus::Synced, "synced row → Synced");
        }
        other => panic!("expected FileStatus payload, got {other:?}"),
    }

    // 5. A path under the served folder with no DB row, and a path outside any
    //    served folder, both resolve to NotTracked (the two `Ok(None)` arms of
    //    `lookup_entry`).
    for (id, path) in [
        (6u64, format!(r"{folder}\missing.txt")), // resolves to (docs, "missing.txt"), no row
        (7, r"C:\elsewhere\x.txt".to_string()),   // not under any served folder
    ] {
        match call(&state, id, RequestMethod::GetFileStatus { path }).await {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
                assert_eq!(info.status, FileStatus::NotTracked);
            }
            other => panic!("expected FileStatus payload, got {other:?}"),
        }
    }
}

/// Assert the real `GetFileStatus` handler reports `want` for `path`.
async fn folder_status_is(state: &Arc<SyncServiceState>, path: &str, want: FileStatus) {
    match call(
        state,
        99,
        RequestMethod::GetFileStatus {
            path: path.to_string(),
        },
    )
    .await
    {
        ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
            assert_eq!(info.status, want, "GetFileStatus({path:?})");
        }
        other => panic!("expected FileStatus payload for {path:?}, got {other:?}"),
    }
}

/// Folder badges (`apps/windows.md` § Shell Extension, USER-ratified
/// 2026-07-14): a folder carries **no `SyncDb` row of its own**, so its overlay
/// status is the severity-aggregate of its tracked descendants. Driven headlessly
/// through the real `GetFileStatus` handler against a real per-folder `SyncDb` —
/// the very query path the live overlay handler drives from Explorer (one
/// `GetFileStatus` per visible item, folders included).
#[tokio::test]
async fn get_file_status_aggregates_a_folder_from_its_tracked_descendants() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
    let state = state(paths.clone());

    let folder = r"C:\od-docs";
    // add → bind → on-demand so the folder resolves to folder "docs".
    call(
        &state,
        1,
        RequestMethod::AddLocation {
            path: folder.into(),
        },
    )
    .await;
    call(
        &state,
        2,
        RequestMethod::SetLocationFolder {
            path: folder.into(),
            folder: "docs".into(),
            folder_id: DOCS.to_wire(),
        },
    )
    .await;
    call(
        &state,
        3,
        RequestMethod::SetLocationSyncMode {
            path: folder.into(),
            mode: "on-demand".into(),
        },
    )
    .await;

    // A single cloud-only descendant → the containing folder AND the sync-root
    // folder both badge CloudOnly (the bug this fixes: today folders read
    // NotTracked because they have no row).
    seed_db_row(&paths, DOCS, "sub/a.txt", SyncState::Placeholder, 4096);
    folder_status_is(&state, &format!(r"{folder}\sub"), FileStatus::CloudOnly).await;
    folder_status_is(&state, folder, FileStatus::CloudOnly).await;

    // Hydrate the file → the folder flips to Synced.
    seed_db_row(&paths, DOCS, "sub/a.txt", SyncState::Synced, 4096);
    folder_status_is(&state, &format!(r"{folder}\sub"), FileStatus::Synced).await;

    // An errored sibling makes the whole tree report the worst thing inside.
    seed_db_row(&paths, DOCS, "sub/bad.txt", SyncState::Conflicted, 10);
    folder_status_is(&state, folder, FileStatus::Error).await;

    // A folder with no tracked descendants stays unbadged (NotTracked).
    folder_status_is(&state, &format!(r"{folder}\empty"), FileStatus::NotTracked).await;
}

/// The cfapi-boundary guard: even with **every** other precondition satisfied
/// (device config + a well-formed provisioned capability), an **unbound**
/// on-demand folder is not served — `reconcile_engines` plans nothing, so no
/// engine/cfapi sync root is registered and the host stays idle. This is the
/// structural reason the harness above can stay headless: serving (and thus
/// cfapi registration) requires a *bound* folder, which the narrow harness
/// never creates while a capability is present.
#[tokio::test]
async fn provisioned_capability_does_not_serve_unbound_on_demand_folder() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));

    let state = state(paths);

    // Provision a well-formed capability (32-byte key + actor) → reconcile runs
    // with no folders yet (no-op).
    let cap = SyncCapability::new(
        vec![7u8; 32],
        vec![0x11u8; 32],
        "https://nest.example".into(),
        "dev-test".into(),
        BearerToken::new("tok".into(), 9_999),
    );
    assert!(matches!(
        call(&state, 1, RequestMethod::ProvisionCapability(cap)).await,
        ResponseResult::Ok(ResponsePayload::Empty)
    ));

    // Add an on-demand folder but never bind a folder to it.
    let folder = r"C:\od-unbound";
    call(
        &state,
        2,
        RequestMethod::AddLocation {
            path: folder.into(),
        },
    )
    .await;
    call(
        &state,
        3,
        RequestMethod::SetLocationSyncMode {
            path: folder.into(),
            mode: "on-demand".into(),
        },
    )
    .await;

    // Unbound → plan_engines skips it → reconcile starts nothing.
    assert!(
        state.engines.lock().await.is_none(),
        "unbound on-demand folder must not start the hydration host (no cfapi registration)"
    );
    match call(&state, 4, RequestMethod::GetServiceStatus).await {
        ResponseResult::Ok(ResponsePayload::ServiceStatus(info)) => {
            assert!(!info.sync.syncing, "nothing bound → not syncing");
        }
        other => panic!("expected ServiceStatus payload, got {other:?}"),
    }
}

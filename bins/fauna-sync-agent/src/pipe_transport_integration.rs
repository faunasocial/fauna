//! Real named-pipe transport integration for the overlay-status IPC.
//!
//! [`producer_integration`](crate::producer_integration) drives the request
//! handlers *in-process* (`handle_request` directly) and drops the event
//! receiver, so the actual `\\.\pipe\fauna-sync` transport — the length-prefixed
//! dag-cbor framing, the client reader thread, and the broadcast→client event
//! forwarding in [`run_pipe_server`](crate::pipe_server::run_pipe_server) — was
//! only ever exercised by the manual Task-D Explorer pass. This harness closes
//! that seam: it runs the **real** pipe server over a real Windows named pipe and
//! connects the **real** [`SyncPipeClient`] — the exact client the shell
//! extension's overlay / context-menu / event-listener handlers use
//! (`shell-ext/src/{overlay,context_menu,event_listener}.rs` all call
//! `SyncPipeClient::connect_pipe`, which is
//! `connect_pipe_to(current_user_pipe_name())`) — then
//! asserts, over the wire:
//!
//!   1. `GetFileStatus` round-trips a real `SyncDb` placeholder row → CloudOnly.
//!   2. A producer-emitted `FileStatusChanged` event placed on
//!      `state.ipc_event_tx` (built via the real `path_map::dehydrate_status_event`
//!      mapper) is forwarded by the server and delivered to the client's
//!      `recv_event()` — the live overlay-push, end-to-end over the pipe.
//!   3. A hydrated row flips the query to Synced.
//!
//! # Regression guard
//!
//! This test is the red→green guard for the `SyncPipeClient` overlapped-I/O fix
//! (`fauna-ipc` `sync_pipe_client::overlapped`). With the previous *synchronous*
//! pipe handle, the reader thread's pending `ReadFile` serialised against
//! `request()`'s `WriteFile` on the same file object and **deadlocked** — the
//! request never reached the server. The per-phase `request_with_timeout` below
//! turns any such regression into a fast failure instead of a CI hang.
//!
//! # Boundary (consistent with `producer_integration`'s spike note)
//!
//! The cfapi-gated *triggers* are deliberately not driven here: a real
//! `FreeSpace` request calls `fauna_cfapi::dehydrate_placeholder` (a Cloud Filter
//! OS call needing a live placeholder) *before* the emit, and the hydrate emit
//! fires inside the cfapi `fetch_data` callback — both need a registered cfapi
//! sync root that is not headless-runnable (see `producer_integration`). So this
//! test injects the *already-built* producer event onto the same broadcast
//! channel the real handlers emit on and asserts the transport + client delivery.
//! The trigger→emit *decision* logic is unit-tested in `path_map`
//! (`dehydrate_status_event*`, `file_status_from_state`) and `bridge.rs`
//! (`run_hydration_loop` success/failure); only the OS trigger itself remains the
//! manual Task-D boundary.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{
    EventKind, FileStatus, RequestMethod, Response, ResponsePayload, ResponseResult,
};
use fauna_ipc::sync_pipe_client::SyncPipeClient;
use fauna_sync_engine::db::{SyncDb, SyncState};

use crate::config::{LocationConfig, LocationMode, SyncConfig, SyncPaths};
use crate::path_map;
use crate::pipe_server::run_pipe_server;
use crate::state::SyncServiceState;

/// A pipe name unique per (process, call) so parallel tests never collide on the
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` single-owner constraint.
fn unique_pipe_name() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        r"\\.\pipe\fauna-sync-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Seed a real per-folder `SyncDb` with one row at `rel` (same shape as
/// `producer_integration::seed_db_row`), opening it through the production
/// [`SyncPaths`] resolver so the handler reads exactly what we wrote.
/// The bound set's identity: the agent keys its engine and state DB by it.
const DOCS: FolderRef = FolderRef::Local(1);

fn seed_db_row(paths: &SyncPaths, set: FolderRef, rel: &str, st: SyncState, size_bytes: i64) {
    let db_path = paths.sync_db_path_for_ref(set);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
    let db = SyncDb::open(&db_path).expect("open per-folder db");
    db.upsert_entry(rel, None, None, None, st, 0, 0, size_bytes, 1, None)
        .expect("seed sync entry");
    drop(db);
}

/// The server creates the pipe instance then awaits a connection; the client open
/// can momentarily race ahead of `CreateNamedPipeW`. Retry briefly on the
/// not-found / busy window (blocking — runs on a `spawn_blocking` thread).
fn connect_with_retry(name: &str) -> SyncPipeClient {
    for _ in 0..150 {
        if let Ok(c) = SyncPipeClient::connect_pipe_to(name) {
            return c;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("could not connect to test pipe {name} within ~3s");
}

/// Issue one blocking `request()` on a `spawn_blocking` thread, bounded by a
/// timeout so a stuck round-trip (e.g. a re-introduced read/write deadlock) fails
/// fast with a diagnostic instead of hanging.
async fn request_with_timeout(
    client: &Arc<SyncPipeClient>,
    method: RequestMethod,
    phase: &str,
) -> Response {
    let client = client.clone();
    tokio::time::timeout(
        Duration::from_secs(15),
        tokio::task::spawn_blocking(move || client.request(method)),
    )
    .await
    .unwrap_or_else(|_| panic!("{phase}: request timed out (server did not respond)"))
    .unwrap_or_else(|e| panic!("{phase}: request task panicked: {e}"))
    .unwrap_or_else(|e| panic!("{phase}: pipe request failed: {e}"))
}

/// End-to-end over a **real** named pipe: the real `SyncPipeClient` queries
/// overlay status and receives a producer-emitted `FileStatusChanged` push from
/// the real `run_pipe_server`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_pipe_query_and_event_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));

    // One bound on-demand folder so `resolve_to_folder_rel` maps both the query
    // path and the dehydrate event path to (folder="docs", rel="sub/a.txt").
    let folder = r"C:\od-docs";
    let mut config = SyncConfig::default();
    config.locations.push(LocationConfig {
        path: folder.to_string(),
        mode: LocationMode::OnDemand,
        folder: Some("docs".to_string()),
        folder_id: Some(DOCS.to_wire()),
        ..Default::default()
    });

    // Seed a real CloudOnly placeholder row for the queried file.
    seed_db_row(&paths, DOCS, "sub/a.txt", SyncState::Placeholder, 4096);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // Drop the initial receiver — the only live receivers are the per-app ones
    // `run_pipe_server` creates via `event_tx.subscribe()` on connect.
    let (event_tx, _) = tokio::sync::broadcast::channel(16);

    let state = SyncServiceState::new(config.clone(), shutdown_tx, event_tx.clone(), paths.clone());

    let pipe = unique_pipe_name();
    let mut server = tokio::spawn({
        let state = state.clone();
        let event_tx = event_tx.clone();
        let pipe = pipe.clone();
        async move { run_pipe_server(state, event_tx, shutdown_rx, &pipe).await }
    });

    let abs = format!(r"{folder}\sub\a.txt");

    // ---- Phase 1: connect the real client over the real pipe ----
    let client = {
        let pipe = pipe.clone();
        Arc::new(
            tokio::time::timeout(
                Duration::from_secs(15),
                tokio::task::spawn_blocking(move || connect_with_retry(&pipe)),
            )
            .await
            .expect("connect timed out")
            .expect("connect task"),
        )
    };

    // GetFileStatus: a Placeholder row maps to CloudOnly, over the wire. Getting a
    // response here also proves the server's per-client `event_tx.subscribe()`
    // (done before `handle_client` is spawned) is live — so the Phase-2 event,
    // sent next, cannot race ahead of the subscription.
    {
        let client = client.clone();
        let abs_q = abs.clone();
        let resp = request_with_timeout(
            &client,
            RequestMethod::GetFileStatus { path: abs_q },
            "phase1-getstatus",
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
                assert_eq!(
                    info.status,
                    FileStatus::CloudOnly,
                    "placeholder row → CloudOnly over the real pipe"
                );
                assert_eq!(info.size_bytes, 4096);
                assert_eq!(info.path, abs);
                assert!(!info.is_pinned);
            }
            other => panic!("expected FileStatus payload, got {other:?}"),
        }
    }

    // ---- Phase 2: a producer-emitted event reaches the client over the pipe ----
    // Build the *real* dehydrate producer event (CloudOnly) for the same path and
    // publish it on the same broadcast channel the live handlers emit on.
    let event = path_map::dehydrate_status_event(&config, &abs)
        .expect("served on-demand path must yield a dehydrate event");
    event_tx
        .send(event)
        .expect("broadcast send (server subscriber is live)");

    let received = tokio::time::timeout(Duration::from_secs(10), {
        let client = client.clone();
        tokio::task::spawn_blocking(move || client.recv_event())
    })
    .await
    .expect("event did not arrive within 10s")
    .expect("recv_event task")
    .expect("recv_event");
    match received.event {
        EventKind::FileStatusChanged { path, status } => {
            assert_eq!(
                path, abs,
                "event carries the absolute path Explorer keys on"
            );
            assert_eq!(
                status,
                FileStatus::CloudOnly,
                "dehydrate producer event → CloudOnly over the real pipe"
            );
        }
        _ => panic!("expected FileStatusChanged event"),
    }

    // ---- Phase 3: a hydrated row flips the query to Synced over the pipe ----
    seed_db_row(&paths, DOCS, "sub/a.txt", SyncState::Synced, 4096);
    {
        let client = client.clone();
        let abs_q = abs.clone();
        let resp = request_with_timeout(
            &client,
            RequestMethod::GetFileStatus { path: abs_q },
            "phase3-getstatus",
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
                assert_eq!(
                    info.status,
                    FileStatus::Synced,
                    "synced row → Synced over the real pipe"
                );
            }
            other => panic!("expected FileStatus payload, got {other:?}"),
        }
    }

    // Tear down: aborting the accept loop can't hang (the request round-trips
    // above already prove the server processed traffic correctly).
    drop(client);
    server.abort();
    let _ = (&mut server).await;
}

/// Cross-session duplicate-agent guard: the `Local\` named mutex excludes duplicates only within one
/// logon session, while the pipe and sync DB it guards are per-user
/// machine-wide — so a duplicate agent in a second concurrent logon session of
/// the same user (RDP + console) passes `InstanceLock` and must instead be
/// stopped by the machine-global `FILE_FLAG_FIRST_PIPE_INSTANCE` create
/// failing. That failure must TERMINATE the agent run loop — a swallowed
/// pipe-create error leaves the loser running degraded (renewal loop + engine
/// reconcile, no control plane) against the shared per-user DB.
///
/// The cross-session shape is simulated exactly: a raw `run_pipe_server`
/// holds the pipe WITHOUT holding this process's mutex claim for it (a
/// session-1 agent's mutex is a different `Local\` object, invisible from
/// session 2), then the full agent run loop starts on the same pipe name. It
/// must return the pipe-create error — before the fix it hung on the signal
/// wait forever, which this test's timeout turns into a loud failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_agent_in_second_logon_session_terminates_instead_of_degrading() {
    let holder_tmp = tempfile::tempdir().expect("holder tempdir");
    let holder_paths = SyncPaths::new(Some(holder_tmp.path().to_path_buf()));
    let (_holder_shutdown_tx, holder_shutdown_rx) = tokio::sync::watch::channel(false);
    let (holder_event_tx, _) = tokio::sync::broadcast::channel(16);
    let holder_state = SyncServiceState::new(
        SyncConfig::default(),
        _holder_shutdown_tx,
        holder_event_tx.clone(),
        holder_paths,
    );

    let pipe = unique_pipe_name();
    let holder = tokio::spawn({
        let pipe = pipe.clone();
        async move { run_pipe_server(holder_state, holder_event_tx, holder_shutdown_rx, &pipe).await }
    });

    // Wait until the holder's pipe instance exists. Poll the pipe namespace
    // listing — opening the pipe itself would consume the instance and let the
    // agent's `FIRST_PIPE_INSTANCE` create win the race this test is pinning.
    let leaf = pipe.rsplit('\\').next().unwrap().to_string();
    let served = |leaf: &str| {
        std::fs::read_dir(r"\\.\pipe\")
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy() == *leaf)
            })
            .unwrap_or(false)
    };
    for _ in 0..150 {
        if served(&leaf) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(served(&leaf), "holder never served test pipe {pipe}");

    // The duplicate agent: full run loop, hermetic world (own data dir,
    // file-backend credential store — never the OS keyring).
    let agent_tmp = tempfile::tempdir().expect("agent tempdir");
    let creds_dir = agent_tmp.path().join("creds");
    std::fs::create_dir_all(&creds_dir).expect("creds dir");
    let store = Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
        "fauna-sync-agent-test",
        creds_dir,
    ));

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        crate::service::run_agent_with_store(&pipe, Some(agent_tmp.path()), store),
    )
    .await
    .expect(
        "agent must terminate when its pipe create fails — hanging on the signal wait \
         is the degraded-duplicate bug this test pins",
    );
    let err = result.expect_err("duplicate agent must exit with the pipe-create error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("CreateNamedPipeW"),
        "error must carry the pipe-create failure: {msg}"
    );
    // The bare OS error ("Access is denied") does not say WHICH name, and reads
    // like a permissions misconfiguration. Both readings are wrong and both cost
    // a diagnosis: `\\.\pipe\` is machine-wide, so a first-instance create that
    // is refused means the NAME IS TAKEN — by a second agent, or by another
    // local account that squatted it to catch what the app connects and
    // sends. The log line is the only witness this process leaves before it
    // exits, so it must name both the pipe and that hypothesis.
    assert!(
        msg.contains(&pipe),
        "error must name the pipe it could not create: {msg}"
    );
    assert!(
        msg.contains("already taken"),
        "a refused first-instance create must offer the name-taken hypothesis, \
         not just the raw OS error: {msg}"
    );

    holder.abort();
    let _ = holder.await;
}

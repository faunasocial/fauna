//! **The same-nest remote-change push-nudge proof.**
//!
//! The same-nest push nudge (`file-sync.md` § Remote-change nudge) landed
//! unit-proven at **three of its four links** — the protocol
//! round-trip, the nest's `record_change_core` fan-out
//! (`sync_handlers::sync_nudge_tests`), and the agent's `PullFolderNow`
//! IPC→wake-channel dispatch. The **fourth link never ran end-to-end**: a real
//! resident engine actually *waking* on the nudge and pulling the remote half
//! before its rescan tick. That is the exact lesson — a mechanism is not
//! proven until it runs — and this module is where it runs.
//!
//! ## What is real here
//!
//! One in-process `fauna-nest` router **with a real `DiskBlobStore` byte plane**
//! (bytes that do not really move would make the whole proof vacuous), the REAL
//! `fauna-sync-agent` binary as a child process driven over its REAL per-user
//! unix socket (the `agent_process_tier3` shape), and a REAL `SyncEngine` for the
//! writer device. The test plays the linux app's role
//! (`apps/fauna-linux/src/app.rs` `PushEvent::SyncChanged` arm →
//! `sync_agent::pull_set_now`): it holds the owner's live push subscription, and
//! on a genuine `PushEvent::SyncChanged` it sends `PullFolderNow` over the agent
//! socket — the one link this file leaves to the production glue is that ~3-line
//! `app.rs` translation, which is compile-checked and covered for content sync at
//! large by `test_folder_agent_content_sync.py`.
//!
//! ## Topology — one nest, one user, two devices, an owner-only set
//!
//! One topology INWARD of `cross_nest_agent_capstone`: no second nest, no MLS
//! share/accept, no federation. Alice owns an **owner-only** set (`mls_group_id ==
//! None`), so both her devices seal under her own `BackupKey` — the foundational
//! multi-device single-user sync case, and the branch the nest producer nudges by
//! actor id (`notify_sync_changed`'s `None` arm). Device A (a directly-built
//! engine) writes; device B (the agent's resident engine) must materialize it.
//!
//! ## RED / GREEN in one test
//!
//! The engine pulls once eagerly each time it is built (`always_resident::
//! run_watch_loop`, the  fix) — at bind, and again when the agent's custody
//! read first names the set's nonce and rebuilds it. A warm-up save that
//! materializes on device B is the barrier past both: the save under test lands
//! *after* it, so its next pull is either the rescan tick
//! (`DEFAULT_RESCAN_INTERVAL` = 300 s) or the nudge. So:
//!   * **RED control** — after receiving the push but *withholding* the nudge for
//!     [`CONTROL_QUIET`] (≫ the nudge's sub-second latency, ≪ the 300 s tick), the
//!     save must still be **absent**: nothing else can deliver it in that window.
//!   * **GREEN** — send `PullFolderNow`; the save must now materialize within
//!     [`NUDGE_WINDOW`]. Fast arrival is attributable to the nudge alone, because
//!     the RED control just proved it was not coming otherwise.
//!
//! ## Run
//!
//! ```text
//! cargo test -p fauna-sync-agent --features tier3-nest --test same_nest_push_nudge
//! ```

#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{
    AgentWorld, AppSeat, await_agent, await_file_content_within, await_serving_engine,
    connected_client, expect_ok, request,
};
use fauna_core::identity::ActorKeypair;
use fauna_ipc::sync::{BearerToken, RequestMethod, ResponseResult, SyncCapability};

const OWNER_SECRET: [u8; 32] = [0x42; 32];
/// Alice's two devices. Both seal under [`backup_key`]; the ids differ only so the
/// nest's per-device `changes.record` gate sees two distinct writers.
const WRITER_DEVICE: [u8; 32] = [0x0a; 32];
const AGENT_DEVICE: [u8; 32] = [0x0b; 32];
/// The owner-only set's own-content seal key, shared by both of Alice's devices
/// (there is no MLS group, so no M2 content keys — the `BackupKey` path).
fn backup_key() -> [u8; 32] {
    fauna_core::crypto::BackupKey::derive(&OWNER_SECRET).to_bytes()
}
const SET_NAME: &str = "docs";
/// Upper bound on the whole materialize-via-nudge chain (pull → download →
/// decrypt → write). Generous — the RED control below is what makes the proof
/// tight; this only needs to be comfortably under the 300 s rescan tick.
const NUDGE_WINDOW: Duration = Duration::from_secs(60);
/// The RED control: the save must NOT arrive within this window while the nudge is
/// withheld. Far above the nudge's sub-second latency and far below the 300 s
/// `DEFAULT_RESCAN_INTERVAL`, so absence here means *only the nudge* delivers.
const CONTROL_QUIET: Duration = Duration::from_secs(12);

// ─────────────────────────────────────────────────────────────────────────
// The nest (real byte plane) — `common::start_nest` (round 32): device B's
// engine downloads what device A uploaded, so a real `DiskBlobStore`-backed
// `BackupService` is load-bearing here too, same as the capstone's own use.
// The shared fn additionally registers `federation_router`/
// `conversations_handlers`, which this single-nest journey never exercises
// (harmless — priority #4, the richest existing pattern over the leanest).
// ─────────────────────────────────────────────────────────────────────────

/// Register a sync device for the connected actor — `changes.record` gates on it.
async fn register_device(nest: &fauna_client::NestClient, device: [u8; 32], label: &str) {
    let _: fauna_protocol::sync::SyncRegisterReply = nest
        .request(
            "fauna.sync.register",
            fauna_protocol::sync::SyncRegisterRequest {
                device_id: hex::encode(device),
                label: label.into(),
                capabilities: "read,write".into(),
                ..Default::default()
            },
        )
        .await
        .expect("register device");
}

/// Device A's engine: Alice's writer, keyed on her own-set `BackupKey`. Same
/// shape as the capstone's `Owner::engine`, minus the MLS/content-key material an
/// owner-only set does not have.
async fn writer_engine(
    base: &str,
    nest: Arc<fauna_client::NestClient>,
    http_token: String,
    watch_dir: PathBuf,
    set_nonce: [u8; 32],
) -> fauna_sync_engine::engine::SyncEngine {
    use fauna_nest_http::{BearerSource, StaticBearer};

    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer(http_token));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        base.to_string(),
        ActorKeypair::from_secret(OWNER_SECRET),
        bearer,
        reqwest::Client::new(),
    ));

    let engine = fauna_sync_engine::engine::SyncEngine::new(
        watch_dir,
        fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
        fauna_sync_engine::nest_client::SyncClient::new(auth, &WRITER_DEVICE),
        Some(SET_NAME.to_string()),
        WRITER_DEVICE,
        None,                                                                 // mls
        None,                                                                 // epoch_secret
        Some(fauna_core::crypto::BackupKey::from_bytes(backup_key()).into()), // owner-only ⇒ BackupKey seal
        None,                                                                 // mls_group_id
        None,                                                                 // content_keys
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        nest,
        fauna_sync_engine::config::SyncMode::Sync, // bidirectional test root
    );
    // A seed-holding device signs its records directly with the identity key,
    // under the set's nonce — the nest refuses an unsigned record.
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&ActorKeypair::from_secret(
                OWNER_SECRET,
            )),
        )),
        Some(set_nonce),
    );
    engine
}

/// Receive the next `fauna.sync.changed` push naming `SET_NAME` (bounded),
/// ignoring any unrelated push kinds / a broadcast lag.
async fn recv_sync_changed(rx: &mut tokio::sync::broadcast::Receiver<fauna_protocol::PushEvent>) {
    use fauna_protocol::PushEvent;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await {
                // By the set's hash address when the nudge carries one — the
                // one match every receiver makes (`names_set`).
                Ok(PushEvent::SyncChanged(p)) if p.names_set(SET_NAME) => return,
                Ok(_) => continue, // some other kind, or a different set
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("the owner's push channel closed before the nudge arrived")
                }
            }
        }
    })
    .await
    .expect(
        "the owner's live connection never received `fauna.sync.changed` for the writer's save \
         — the nest producer (`record_change_core` → `notify_sync_changed`) did not fan the \
         nudge out to the owner's own devices",
    );
}

// ─────────────────────────────────────────────────────────────────────────
// The proof
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn same_nest_nudge_materializes_within_seconds_not_a_tick() {
    let (base, state) = common::start_nest().await;
    let alice_id = ActorKeypair::from_secret(OWNER_SECRET).actor_id();
    state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    // Owner-only set: no MLS group, so the producer nudges by actor id and both
    // devices seal under Alice's own BackupKey. Created on the production path,
    // which mints the set's nonce into her custody first — every record binds
    // to it, and the agent reads it from custody. Custody is the account
    // plane's: Alice's app writes it through its own store.
    let alice_app = AppSeat::sign_in(&state, &base, OWNER_SECRET).await;
    let (folder_id, set_nonce) = {
        let owner_nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SECRET)).await;
        let custody = alice_app.folder_keys();
        let reply = fauna_client_folders::create_set(
            &fauna_client_folders::FoldersClient::new(Arc::clone(&owner_nest)),
            &*custody,
            fauna_protocol::folders::FolderCreateRequest {
                name: SET_NAME.to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("alice creates the set");
        let held = custody.load().await.expect("alice's custody");
        let nonce = fauna_client_folders::custody::live_set_nonce(&held, SET_NAME)
            .expect("the create minted the set's nonce into custody");
        (reply.id, nonce)
    };

    // ── Device B: the real agent, bound to the set, engine serving ──────
    let world = AgentWorld::new("fauna-nudge-t3-");
    let socket = world.socket_path();
    let member_location = world.root().join("bob-device");
    std::fs::create_dir_all(&member_location).unwrap();

    // Alice's app enrols device B first — the agent reads custody as the
    // machine's enrolled principal — and her other machine's next pass keys
    // that principal for the account's custody. The enrolling app is gone
    // before the agent starts.
    common::enroll_as_w5_app(
        OWNER_SECRET,
        &world,
        &base,
        &hex::encode(alice_id.0),
        None,
        hex::encode(AGENT_DEVICE),
    )
    .await;
    alice_app.sync().await;
    let _agent = world.spawn_agent_trusting(&[&base]);
    await_agent(&socket);

    let agent_token = state.auth.token_store.insert(alice_id, 3600).await;
    let cap = SyncCapability::new(
        backup_key().to_vec(),
        alice_id.0.to_vec(),
        base.clone(),
        hex::encode(AGENT_DEVICE),
        BearerToken::new(agent_token, 4_000_000_000),
    );
    expect_ok(&socket, RequestMethod::ProvisionCapability(cap));
    expect_ok(
        &socket,
        RequestMethod::AddLocation {
            path: member_location.display().to_string(),
        },
    );
    expect_ok(
        &socket,
        RequestMethod::SetLocationFolder {
            path: member_location.display().to_string(),
            folder: SET_NAME.to_string(),
            folder_id: fauna_core::folder_keys::FolderRef::Local(folder_id).to_wire(),
        },
    );
    await_serving_engine(&socket, SET_NAME, Duration::from_secs(30));

    // ── The app: Alice's live push channel + her writer device's control plane ──
    let alice_app_nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SECRET)).await;
    register_device(&alice_app_nest, AGENT_DEVICE, "alice-agent").await;

    let alice_writer_nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SECRET)).await;
    register_device(&alice_writer_nest, WRITER_DEVICE, "alice-writer").await;
    let writer_token = state.auth.token_store.insert(alice_id, 3600).await;

    // ── Device A writes; the nest must nudge the owner's other device ───
    let writer_watch = tempfile::tempdir().unwrap();
    let writer = writer_engine(
        &base,
        Arc::clone(&alice_writer_nest),
        writer_token,
        writer_watch.path().to_path_buf(),
        set_nonce,
    )
    .await;

    // ── Barrier: device B's engine is on its custody-read build ─────────
    // The agent builds an owner-only set's engine at the first reconcile, before
    // its account host has mounted the store custody is read through; that
    // engine holds no set nonce, judges no signed row, and is rebuilt — with a
    // fresh eager pull — once custody reads. A warm-up save that materializes
    // on device B proves the rebuild is behind us: only an engine holding the
    // nonce accepts the writer's signed row, and nothing rebuilds it again. So
    // the save under test can be delivered only by the tick (300 s away) or
    // the nudge — never by an eager pull.
    let warm_up_rel = "warm-up.bin";
    let warm_up = b"device B's engine reads custody".to_vec();
    std::fs::write(writer_watch.path().join(warm_up_rel), &warm_up).unwrap();
    writer
        .upload_file(warm_up_rel)
        .await
        .expect("device A uploads the warm-up save");
    let warm_up_landed = member_location.join(warm_up_rel);
    let deadline = std::time::Instant::now() + NUDGE_WINDOW;
    while std::fs::read(&warm_up_landed).ok().as_deref() != Some(warm_up.as_slice()) {
        assert!(
            std::time::Instant::now() < deadline,
            "device B never materialized the warm-up save within {NUDGE_WINDOW:?} — its engine \
             never came up holding the set's nonce (the custody read through the agent's \
             mounted account store), so it can judge no signed row"
        );
        // A nudge the nonce-less build cannot act on is simply re-sent.
        let _ = request(
            &socket,
            RequestMethod::PullFolderNow {
                folder: SET_NAME.to_string(),
                folder_hash: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let mut pushes = alice_app_nest.subscribe_pushes();

    let payload: Vec<u8> = (0..48_000u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let rel = "from-device-a.bin";
    std::fs::write(writer_watch.path().join(rel), &payload).unwrap();
    writer
        .upload_file(rel)
        .await
        .expect("device A uploads under the owner-only set's BackupKey");

    // The app receives the genuine push (nest producer → owner fan-out).
    recv_sync_changed(&mut pushes).await;

    // ── RED control — withhold the nudge; the save must NOT arrive ──────
    // With every eager pull already spent and the 300 s tick far off, nothing can
    // deliver the save during this window. If it appears here, fast arrival below
    // could not be attributed to the nudge — so this is the regression guard that
    // keeps T6 honest.
    tokio::time::sleep(CONTROL_QUIET).await;
    let landed = member_location.join(rel);
    assert!(
        std::fs::read(&landed).ok().as_deref() != Some(payload.as_slice()),
        "the writer's save reached device B within {CONTROL_QUIET:?} WITHOUT a nudge — the eager \
         pull or an unexpected early pull delivered it, so this test can no longer prove the \
         nudge is what shrinks the latency. Fast delivery must be the nudge's alone."
    );

    // ── GREEN — the app translates the push to the IPC; the save lands ──
    // Exactly `apps/fauna-linux/src/app.rs`'s `PushEvent::SyncChanged` arm →
    // `sync_agent::pull_set_now` → `provisioner.pull_folder_now`.
    match request(
        &socket,
        RequestMethod::PullFolderNow {
            folder: SET_NAME.to_string(),
            folder_hash: None,
        },
    ) {
        ResponseResult::Ok(_) => {}
        ResponseResult::Err(e) => panic!("PullFolderNow was refused by the agent: {e}"),
    }

    await_file_content_within(
        &landed,
        &payload,
        NUDGE_WINDOW,
        "the nudge did not materialize device A's save in device B's folder",
        "nudge",
        "The engine wake→pull→materialize link is broken: the agent received \
         `PullFolderNow`, but either the wake channel never reached the running \
         engine's `select!` arm (registry / `same_channel` deregister bug), or the \
         woken `pull_remote_changes` did not materialize the record.",
    );
}

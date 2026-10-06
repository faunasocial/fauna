//! Tier_3: the unix **full-stack agent process** harness (plan D8; the twin of
//! the windows in-process `versions_tier3`/`restore_byteplane_tier3` modules,
//! one level further out): the REAL `fauna-sync-agent` binary as a child
//! process, driven over the REAL per-user unix socket by `SyncPipeClient` —
//! provision → folder bind → engines listed → **SIGKILL + respawn** → resume
//! from the persisted capability with no app.
//!
//! ## What is real here
//!
//! The whole agent: the shipped binary (`CARGO_BIN_EXE_fauna-sync-agent`), its
//! unix-socket IPC server, the dag-cbor frame codec, the capability persistence
//! (`fauna-credential-store` file arm), the folder config (`config.toml`), and
//! the engine reconcile. The nest is a real in-process `fauna-nest` router on an
//! ephemeral port (the established `build_router` + `axum::serve` idiom the
//! windows tier_3 modules use). This is where the provisioner's folder-control
//! verbs (`AddLocation` + `SetLocationFolder` / `RemoveLocation` /
//! `ListLocations`) get their cross-process behavioral coverage.
//!
//! ## Launch isolation (testing.md § point 10)
//!
//! The child gets a **private world**: a temp `HOME` (every unix), a temp
//! `XDG_CONFIG_HOME` + `XDG_RUNTIME_DIR` (linux), a `FAUNA_E2E_CREDENTIAL_DIR`
//! file credential store, and an explicit `--data-dir` — so a run never touches
//! the developer's real agent, keyring, account store or sync config, and two
//! runs never collide. The temp root lives under `/tmp` directly: `sun_path` is
//! capped at ~104 bytes on macOS and a nested tempdir plus the Library suffix
//! would flirt with it.
//!
//! ⚠ **`HOME`/`XDG_CONFIG_HOME` were added at W5 (account-data-plane.md § Workstreams).5b, and the gap they closed is
//! worth remembering.** Before it, only `XDG_RUNTIME_DIR` was overridden on
//! linux — enough while the agent's only state came from `--data-dir`, and
//! silently wrong the moment it began resolving the shared account-store root
//! (`StoreRoot::platform()`), which reads the home/XDG environment. A child with
//! the developer's inherited `HOME` would have reached into the developer's real
//! account store. **The lesson generalizes: isolating the paths an agent uses
//! today is not the same as isolating its environment** — the next capability
//! that reads an env var inherits whatever this harness failed to override.
//!
//! ## Run
//!
//! Opt-in (pulls the whole `fauna-nest` crate — off the default test loop):
//!
//! ```text
//! cargo test -p fauna-sync-agent --features tier3-nest --test agent_process_tier3
//! ```

#![cfg(unix)]

mod common;

use std::os::unix::process::ExitStatusExt;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use common::KillOnDrop;
use common::{AgentWorld, await_agent, await_serving_engine, expect_ok, list_locations, request};
use fauna_account_store::locks::{EngineLock, EngineLockOutcome};
use fauna_core::identity::ActorKeypair;
use fauna_ipc::sync::{
    BearerToken, ConnectionState, NO_CAPABILITY_ERROR_PREFIX, RequestMethod, ResponsePayload,
    ResponseResult, SyncCapability,
};

const OWNER_SECRET: [u8; 32] = [0x42; 32];
const DEVICE_ID: [u8; 32] = [0x07; 32];
const FOLDER: &str = "documents";
/// How long a bound set gets to reach a serving engine: the agent withholds it
/// until its own content-key resolution names the set, which re-reads
/// asynchronously after the reconcile (convention 14 — a generous named budget,
/// not a settle-sleep).
const SERVING_BUDGET: Duration = Duration::from_secs(90);

/// Start a real in-process nest (real router, real handlers, in-memory db) and
/// mint an HTTP bearer for the registered owner — minus the chunk-store byte
/// plane (this harness proves the agent process lifecycle, not chunk serving;
/// the byte plane has its own real-nest coverage, which is why `blob_dir` is
/// `None`).
///
/// The body is `fauna_nest::test_support::start_test_nest`, shared with
/// `fauna-nest`'s own conformance tests and the windows `tier3-nest` modules;
/// only the handler set is this harness's.
async fn start_test_nest(owner_secret: [u8; 32]) -> fauna_nest::test_support::TestNest {
    let nest = fauna_nest::test_support::start_test_nest(owner_secret, None, |b| {
        fauna_nest::sync_handlers::register_sync_handlers(b);
        fauna_nest::folder_handlers::register_folders_handlers(b);
        // The identity handshake: the W5.5b case's app phase connects as a
        // real seed-holding client (that is what makes it the *app*), and
        // that handshake is a kind like any other.
        fauna_nest::auth_handlers::register_auth_handlers(b);
    })
    .await;

    // The shared starter registers the ACTOR ID, which is the row that matters
    // (an identity handshake checks registration; a bearer-only case never
    // notices, because a minted token bypasses the users table — that mismatch
    // was this harness's `fauna.auth.not_registered` bug). This extra
    // secret-keyed row is the legacy one the original call created, kept so
    // nothing that happened to depend on it changes.
    nest.state
        .db
        .create_user(&owner_secret, "free", "test")
        .await
        .unwrap();

    nest
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_process_provisions_binds_and_resumes_after_kill() {
    let nest = start_test_nest(OWNER_SECRET).await;
    let (nest_url, bearer) = (nest.base_url.clone(), nest.http_token.clone());
    // The set exists on the nest: the agent keys an engine only for a set its
    // own resolution names, and that resolution starts from the nest's folder
    // list — a bind to a folder the nest never heard of is withheld for ever.
    let actor = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let folder_id = nest.state.db.create_folder(FOLDER, &actor).await.unwrap();
    let folder_ref = fauna_core::folder_keys::FolderRef::Local(folder_id);
    let world = AgentWorld::new("fauna-agent-t3-");
    let socket = world.socket_path();
    let watched = world.root().join("watched");
    std::fs::create_dir_all(&watched).unwrap();

    // ── Life 1: fresh agent ──
    let mut agent = world.spawn_agent();
    await_agent(&socket);

    // Fresh agent holds no capability: RefreshBearer must answer with the pinned
    // no-capability prefix — the cross-process convergence-loop contract.
    match request(
        &socket,
        RequestMethod::RefreshBearer(BearerToken::new(bearer.clone(), 4_000_000_000)),
    ) {
        ResponseResult::Err(e) => assert!(
            e.starts_with(NO_CAPABILITY_ERROR_PREFIX),
            "expected the pinned no-capability prefix, got: {e}"
        ),
        ResponseResult::Ok(p) => panic!("RefreshBearer on a fresh agent must fail, got {p:?}"),
    }

    // Full provision over the real socket (what the app's convergence loop
    // pushes on NoCapability). No renewal key: this harness has no
    // device-grant registration, and resume must not depend on one.
    let cap = SyncCapability::new(
        vec![0x11; 32],
        actor.to_vec(),
        nest_url.clone(),
        hex::encode(DEVICE_ID),
        BearerToken::new(bearer.clone(), 4_000_000_000),
    );
    expect_ok(&socket, RequestMethod::ProvisionCapability(cap));

    // Bind a folder — the exact two-exchange sequence
    // `FfiSyncAgentProvisioner::bind_location` performs.
    expect_ok(
        &socket,
        RequestMethod::AddLocation {
            path: watched.display().to_string(),
        },
    );
    expect_ok(
        &socket,
        RequestMethod::SetLocationFolder {
            path: watched.display().to_string(),
            folder: FOLDER.to_string(),
            folder_id: folder_ref.to_wire(),
        },
    );

    let folders = list_locations(&socket);
    assert_eq!(folders.len(), 1, "one bound folder expected: {folders:?}");
    assert_eq!(folders[0].folder.as_deref(), Some(FOLDER));
    assert_eq!(folders[0].folder_id, Some(folder_ref.to_wire()));

    let engines = await_serving_engine(&socket, FOLDER, SERVING_BUDGET);
    assert_eq!(
        engines.len(),
        1,
        "the bound set must have one engine row: {engines:?}"
    );

    // ── Kill: no shutdown path, no unprovision ──
    agent.kill_uncleanly();
    drop(agent);

    // ── Life 2: fresh process over the same persisted world, NO app push ──
    let _agent2 = world.spawn_agent();
    await_agent(&socket);

    // The capability came back from the credential store: RefreshBearer now
    // succeeds instead of hitting the no-capability contract.
    match request(
        &socket,
        RequestMethod::RefreshBearer(BearerToken::new(bearer, 4_000_000_000)),
    ) {
        ResponseResult::Ok(_) => {}
        ResponseResult::Err(e) => {
            panic!("respawned agent must resume the persisted capability, got: {e}")
        }
    }

    // The folder binding came back from config.toml, and its engine is serving
    // again — the app-dead resume D8 requires.
    let folders = list_locations(&socket);
    assert_eq!(
        folders.len(),
        1,
        "binding must survive the process kill: {folders:?}"
    );
    assert_eq!(folders[0].folder.as_deref(), Some(FOLDER));

    let engines = await_serving_engine(&socket, FOLDER, SERVING_BUDGET);
    assert_eq!(engines.len(), 1, "engine row must resume: {engines:?}");

    // Unbind closes the loop (the provisioner's unbind_location verb).
    expect_ok(
        &socket,
        RequestMethod::RemoveLocation {
            path: watched.display().to_string(),
        },
    );
    assert!(
        list_locations(&socket).is_empty(),
        "unbind must clear the row"
    );

    // Best-effort world cleanup (the tempdir was .keep()ed so it survives the
    // child processes; remove it now that both are dead).
    let _ = std::fs::remove_dir_all(world.root());
}

/// How long the agent gets to mount the store after being provisioned.
///
/// Convention 14: a **named, generous budget** with a deadline poll, not a
/// settle-sleep. A green run pays only the real latency (one credential-store
/// read, one store open, one `flock`); the ceiling exists solely so a genuine
/// hang fails loudly instead of hanging the suite, and it is sized far above any
/// non-pathological delay on a machine running 20+ concurrent sessions.
const MOUNT_BUDGET: Duration = Duration::from_secs(90);

/// **W5.5b — the agent process hosts the account store with no app running.**
///
/// The claim under test is `account-data-plane.md`'s R2 (account-data-plane.md § The ratified decisions) ("replica freshness
/// while no app runs arrives at W5 when the agent mounts the store as the
/// always-on host") at the level nothing else reaches: the **real agent
/// binary**, in its own process, mounting the machine's **shared** account store
/// and taking the W5.1 engine-singleton role — app-dead.
///
/// # What makes this different from the runtime-level proof
///
/// `conformance_account_runtime`'s V11 already proves the seedless *runtime*
/// reads and writes an enrolled store (and V11b that it refuses an unenrolled
/// one). Both run in one process. What this adds is the wiring W5.5b is: that
/// the shipped agent spawns the host task at all, that it resolves the same
/// root a co-located app resolves rather than one derived from its own
/// `--data-dir`, and that its assembly really does reach `AccountStoreRuntime::
/// start` inside a process holding no seed.
///
/// # The observable, and why it is this one
///
/// The **engine lock** (`<store dir>/engine.lock`, W5.1) is a kernel-arbitrated
/// `flock` held for the role's lifetime and released by the kernel on exit. So
/// "some live process holds the account's engine-singleton role" is directly
/// observable from outside every process involved, needs no new IPC surface, and
/// is a *causal* fact rather than a timing one. The test arranges that the only
/// candidate holder is the agent: the app phase is fully shut down first, and the
/// lock is asserted free before the agent is spawned. A refusal afterwards can
/// therefore only be the agent's runtime.
#[tokio::test(flavor = "multi_thread")]
async fn agent_process_hosts_the_shared_account_store_with_no_app_running() {
    let nest = start_test_nest(OWNER_SECRET).await;
    let (nest_url, bearer) = (nest.base_url.clone(), nest.http_token.clone());
    let world = AgentWorld::new("fauna-agent-t3-");
    let socket = world.socket_path();

    let keypair = ActorKeypair::from_secret(OWNER_SECRET);
    let actor_hex = hex::encode(keypair.actor_id().0);
    let store_root = world.shared_store_root();
    let store_dir = store_root
        .store_dir(&actor_hex)
        .expect("per-actor store dir");

    // ── Phase 1: the signed-in app enrolls this machine ──────────────────
    // The only process in this test that ever holds the seed. Its assembly is
    // what populates the T10 slot the seedless agent later reads: the writer
    // key, the root-signed `DeviceAuthorization`, and the `BackupKey` without
    // which W5.5a's refusal fires (correctly — see V11b).
    {
        let app_client = fauna_client::NestClient::new(
            nest_url.clone(),
            ActorKeypair::from_secret(OWNER_SECRET),
        );
        app_client.connect().await.expect("the app's nest connect");

        let app = fauna_sync_engine::account_runtime::AccountStoreRuntime::start(
            fauna_sync_engine::account_runtime::AccountRuntimeParams {
                store_backup_exclusion:
                    fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                        platform: "test".into(),
                    },
                store_root: world.shared_store_root(),
                // The app's own state dir — app-local by design, and pointedly
                // NOT the agent's (each native app mirrors the sealed blob
                // into its own dir; only the store itself is shared).
                actor_id_hex: actor_hex.clone(),
                rpc: Arc::clone(&app_client),
                process_rpc: None,
                principal: fauna_sync_engine::account_runtime::RuntimePrincipal::SeedHolding(
                    ActorKeypair::from_secret(OWNER_SECRET).into(),
                ),
                credentials: world.shared_credentials(),
                reconnects: None,
                pushes: None,
                backstop_interval: fauna_sync_engine::account_runtime::DEFAULT_BACKSTOP_INTERVAL,
                memberships: None,
                // No pin in this world: fail-safe, and irrelevant here — no
                // `GenerationTip` kind is written by this test.
                trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(
                    Vec::new(),
                ),
                attested_predecessors: Default::default(),
                linked_nests: None,
                owed_nests: None,
                peer_transport: None,
                // The machine's named row.
                enrollment_target_device_id: hex::encode(DEVICE_ID),
            },
        )
        .await
        .expect("the signed-in app's assembly enrolls this machine");

        // One pass, so the enrollment's nest legs run exactly as production's do.
        let _ = app.reconcile_now().await;

        // Deterministic teardown — the app is *gone* before the agent starts, so
        // the handoff below is sequential, never a race (convention 14).
        app.shutdown().await;
    }

    // The enrollment is on disk where a co-located process can find it…
    assert!(
        store_dir.exists(),
        "the app's assembly must have created the shared store at {}",
        store_dir.display()
    );
    // …and, crucially, NOTHING holds the engine-singleton role: the app is down,
    // and the agent has not started. Without this the final assertion would be
    // satisfiable by a leftover holder rather than by the agent.
    let free = EngineLock::try_acquire(&store_dir);
    assert!(
        matches!(free, EngineLockOutcome::Held(_)),
        "with the app shut down the engine lock must be free; got {free:?}"
    );
    drop(free); // release it again — the agent must be able to take it

    // ── Phase 2: the agent, app-dead ─────────────────────────────────────
    let mut agent = world.spawn_agent();
    await_agent(&socket);

    // Provisioning is the only thing an app ever pushes here, and this test
    // pushes it once and then never speaks for an app again. Note the agent is
    // NOT handed the account's `BackupKey` as its store key: it reads that from
    // the T10 slot the app populated. This capability's key material is the
    // engines' (`sync-agent.md` § Credential model), untouched by W5.5b.
    let cap = SyncCapability::new(
        vec![0x11; 32],
        keypair.actor_id().0.to_vec(),
        nest_url.clone(),
        hex::encode(DEVICE_ID),
        BearerToken::new(bearer, 4_000_000_000),
    );
    expect_ok(&socket, RequestMethod::ProvisionCapability(cap));

    // The agent's account host mounts the store and takes the role. Deadline
    // poll, no sleep-then-assert: a green run exits this loop as soon as the
    // `flock` is genuinely held.
    let deadline = Instant::now() + MOUNT_BUDGET;
    loop {
        match EngineLock::try_acquire(&store_dir) {
            EngineLockOutcome::Refused => break, // the agent holds it
            outcome => {
                drop(outcome);
                assert!(
                    Instant::now() < deadline,
                    "the agent never mounted the shared account store at {} \
                     within {MOUNT_BUDGET:?} — the engine lock stayed free, so \
                     no runtime was assembled in that process",
                    store_dir.display()
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // And it mounted the SHARED store, not one of its own: had the host derived
    // a root from `--data-dir`, the lock above would have been taken on a
    // different file and this dir would hold a second journal under the very
    // same writer key — the equivocation shape W6's path unification exists to
    // prevent, which surfaces only after both have published.
    let private_store = world.root().join("data").join(&actor_hex);
    assert!(
        !private_store.exists(),
        "the agent must not create an account store under its own --data-dir \
         ({}) — the shared root is the whole point of W6's unification",
        private_store.display()
    );

    // ── And it still exits cleanly WITH a live mount ──────────────────────
    //
    // A regression pin for a defect this slice introduced and fixed: the host
    // task shares its shutdown `watch::Receiver` with the stint holding the
    // mount, and `changed()` resolves once per transition. The stint's own
    // select consumed that transition, so the outer loop waited for a second
    // one that never came — the task re-assembled and parked, and `run_agent`
    // awaits this handle on the orderly path, so SIGTERM would hang forever.
    //
    // `agent_sigterm.rs` cannot catch this: it provisions no capability, so it
    // never mounts, and an unmounted host exits through a different arm. A live
    // mount is the precondition, which is exactly what this test already has.
    //
    // Asserting the exit *status*, not merely the exit, for that test's own
    // reason: SIGTERM's default disposition already terminates the process, so
    // "it went away" passes with the handler gone. Only `code() == Some(0)`
    // distinguishes the orderly path from the kernel's.
    let pid = agent.pid() as libc::pid_t;
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGTERM) },
        0,
        "kill(SIGTERM) failed: {}",
        std::io::Error::last_os_error()
    );
    let deadline = Instant::now() + MOUNT_BUDGET;
    let status = loop {
        match agent.try_wait().expect("try_wait must not error") {
            Some(status) => break status,
            None => {
                assert!(
                    Instant::now() < deadline,
                    "the agent did not exit within {MOUNT_BUDGET:?} of SIGTERM \
                     while hosting a live account-store mount — the host task \
                     is parked on a shutdown transition it already consumed"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    assert_eq!(
        status.code(),
        Some(0),
        "the agent must take the ORDERLY shutdown path with a mount live; \
         signal={:?} (a signalled exit means the handler never ran)",
        status.signal()
    );

    let _ = std::fs::remove_dir_all(world.root());
}

/// The `/tmp` fallback `SyncPaths::macos_group_container_base` /
/// `unix_transport::macos_socket_path` land on when `HOME` cannot resolve to
/// an absolute path — see [`agent_reaches_ready_with_no_data_dir_and_unresolvable_home`].
#[cfg(target_os = "macos")]
const UNRESOLVABLE_HOME_FALLBACK_SOCKET: &str =
    "/tmp/Library/Application Support/Fauna/sync-agent.sock";
#[cfg(target_os = "macos")]
const UNRESOLVABLE_HOME_FALLBACK_BASE: &str = "/tmp/Library/Application Support/Fauna/sync";

/// Closes the harness blind spot that hid the 2026-07-20 `.pkg` install
/// crash-loop: every other case in this file passes
/// `--data-dir`, so the agent under test never resolves the PRODUCTION path
/// launchd actually hands it. This one omits `--data-dir` and gives the agent
/// a `HOME` that cannot resolve to an absolute path, from cwd `/` — launchd's
/// own cwd, and (being the read-only system volume) exactly what turned the
/// old code's relative fallback into a panic — and asserts the agent reaches
/// "ready" rather than crash-looping.
///
/// `HOME` is set to a RELATIVE string rather than removed outright.
/// Removing it makes `dirs::home_dir()` fall through to the REAL passwd
/// database, and this dev box has a real, live sync-agent LaunchAgent
/// (`social.fauna.sync-agent`) holding real conversation/MLS
/// state at that exact production
/// path (confirmed via `launchctl list` before writing this test — then the
/// app-group container; since 2026-08-25 the user-domain
/// `~/Library/Application Support/Fauna/sync`, per
/// `UNRESOLVABLE_HOME_FALLBACK_BASE` above — the agent never opens the
/// container any more, `installers/macos.md` § Identifier domain, item 5) — a
/// genuinely-unset `HOME` here would either collide with it or
/// pass only vacuously, because the running instance's lock rejects the
/// second bind. A relative `HOME` hits the identical "home did not resolve to
/// an absolute path" branch in both `SyncPaths`'s macOS base
/// (`fauna_account_store::root::macos_user_domain_base`)
/// and `unix_transport::macos_socket_path` (the latter's matching gap closed
/// in this same commit — it lacked the absolute-guarantee fallback the base
/// dir got), landing in an isolated `/tmp` location instead: verified red
/// against `b510e8589^` (the pre-fix `config.rs`/`unix_transport.rs`, checked
/// out standalone) — the agent panicked initializing its log dir before this
/// test's `await_agent` deadline; green after.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn agent_reaches_ready_with_no_data_dir_and_unresolvable_home() {
    let socket = Path::new(UNRESOLVABLE_HOME_FALLBACK_SOCKET);
    let base = Path::new(UNRESOLVABLE_HOME_FALLBACK_BASE);
    // Idempotent — clear any leftovers from a prior run of this same test.
    let _ = std::fs::remove_dir_all(base);
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());

    let creds = tempfile::Builder::new()
        .prefix("fauna-agent-t3-unresolvable-home-creds-")
        .tempdir_in("/tmp")
        .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fauna-sync-agent"));
    // No --data-dir: the point of this test is the production resolution path.
    cmd.env("HOME", "not-a-real-home")
        .env("FAUNA_E2E_CREDENTIAL_DIR", creds.path())
        .env("RUST_LOG", "info")
        .current_dir("/");
    let mut agent = KillOnDrop(cmd.spawn().expect("spawn fauna-sync-agent"));

    await_agent(socket);

    match request(socket, RequestMethod::GetServiceStatus) {
        ResponseResult::Ok(_) => {}
        ResponseResult::Err(e) => panic!("agent came up but GetServiceStatus failed: {e}"),
    }

    agent.kill_uncleanly();
    drop(agent);
    let _ = std::fs::remove_dir_all(base);
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}

/// **The T11 gesture proof: deleting the ONE
/// device row the user recognises ends the shipped agent's app-dead renewal**
/// (`sync-agent.md` § Credential model → the RULED 2026-08-15 block,
/// decisions 1–3). The nest tier_3 suite proves the kinds and the engine
/// suite proves the convergence; what neither can assert is that the
/// credential the REAL `fauna-sync-agent` binary renews with is the same one
/// the user's delete reaches — two different keys until something checks,
/// and "the machine appears once" is worth nothing if deleting the row it
/// appears as leaves a second credential renewing behind it (exactly the
/// state row 43 found and fixed).
///
/// Convention 14 throughout: the observable is nest state ("does this key
/// mint?", "how many rows?") behind named budgets — never how long anything
/// took.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_one_device_row_ends_the_real_agents_renewal() {
    let nest = start_test_nest(OWNER_SECRET).await;
    let (nest_url, bearer) = (nest.base_url.clone(), nest.http_token.clone());
    let world = AgentWorld::new("fauna-agent-t3-");
    let socket = world.socket_path();
    let keypair = ActorKeypair::from_secret(OWNER_SECRET);
    let actor_hex = hex::encode(keypair.actor_id().0);
    let named_row = hex::encode(DEVICE_ID);

    // ── 1+2: the machine's NAMED row exists, and the app enrolls targeting
    // it (decision 2's direct-target shape — the app resolved the id).
    common::enroll_as_w5_app(
        OWNER_SECRET,
        &world,
        &nest_url,
        &actor_hex,
        Some((&named_row, "test machine")),
        named_row.clone(),
    )
    .await;

    // ── 3: ONE row — the named one; no writer-pub-hex placeholder beside it.
    let app_client =
        fauna_client::NestClient::new(nest_url.clone(), ActorKeypair::from_secret(OWNER_SECRET));
    app_client.connect().await.expect("the app's nest connect");
    let sync = fauna_client_sync::SyncClient::new(app_client.as_ref());
    let devices = sync.devices_list().await.expect("devices list");
    let ids: Vec<&str> = devices
        .devices
        .iter()
        .map(|d| d.device_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![named_row.as_str()],
        "T11: the machine appears ONCE, as the row the user recognises"
    );

    // ── 4: the agent, app-dead, holding ONLY the principal (the machine's
    // one renewal credential; the bearer already expired so the renewal loop
    // acts on its first pass).
    let mut agent = world.spawn_agent();
    await_agent(&socket);
    let cap = SyncCapability::new(
        vec![0x11; 32],
        keypair.actor_id().0.to_vec(),
        nest_url.clone(),
        named_row.clone(),
        BearerToken::new(bearer, 1),
    );
    expect_ok(&socket, RequestMethod::ProvisionCapability(cap));

    // ── 5: app-dead renewal WORKS first — without this, the death below
    // proves nothing. The renewing credential is the T10 slot's writer key,
    // whose grant the enrollment registered on the named row.
    let writer = fauna_sync_engine::principal_bundle::load_writer_key(
        &world.shared_credentials(),
        &actor_hex,
    )
    .expect("the enrollment left this machine's writer key in the T10 slot");
    fauna_anon_client::mint_bearer_over_device_handshake(&nest_url, keypair.actor_id().0, &writer)
        .await
        .expect("the principal mints before the gesture");
    // And the agent advertises exactly the identity + row the delete will
    // reach (the id decision 2 has apps resolve their target from).
    let deadline = Instant::now() + MOUNT_BUDGET;
    loop {
        if let ResponsePayload::ServiceStatus(status) =
            expect_ok(&socket, RequestMethod::GetServiceStatus)
            && status.store_principal_actor.as_deref() == Some(actor_hex.as_str())
        {
            assert_eq!(
                status.sync_device_id.as_deref(),
                Some(named_row.as_str()),
                "the agent presents the NAMED row as this machine's sync \
                 device id — the id apps resolve their enrollment target \
                 from (decision 2)"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent never advertised the store principal within {MOUNT_BUDGET:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // ── 6: the gesture.
    let reply = sync
        .devices_delete(named_row.clone())
        .await
        .expect("devices.delete");
    assert!(reply.deleted, "the delete must report deleted");

    // ── 7: it ended the machine — the same credential the agent renews with
    // now mints nothing, and the devices list is empty. No second credential,
    // no second row, nothing renewing behind the user's gesture.
    let err = fauna_anon_client::mint_bearer_over_device_handshake(
        &nest_url,
        keypair.actor_id().0,
        &writer,
    )
    .await;
    assert!(
        err.is_err(),
        "the user's delete on the ONE row must end the very credential the \
         shipped agent renews with — a mint that still succeeds means a \
         second credential survived the gesture"
    );
    let devices = sync.devices_list().await.expect("devices list");
    assert!(
        devices.devices.is_empty(),
        "and no row remains: {:?}",
        devices
            .devices
            .iter()
            .map(|d| &d.device_id)
            .collect::<Vec<_>>()
    );

    agent.kill_uncleanly();
}

/// **One leg refused while the rest of the agent succeeds — the refusal is
/// still found at once** (`sync-agent-credentials.md` § Credential model →
/// *A refused renewal is terminal*, its *A rejected credential asks at once*
/// sub-bullet).
///
/// The shape a user's machine logged for hours: the machine's row was
/// deleted on another device while an app stayed open here, so the bearer
/// in the agent's slot — the app's own session, which a device delete
/// spares — keeps every capability socket connecting, while the account
/// host's device-principal leg, which signs as the deleted principal, is
/// refused on every attempt. The renewal loop plans on that live bearer's
/// expiry, so until the leg's refusal reaches it, nothing asks the
/// question that would end the agent's credential: the leg redialled the
/// dead principal at its backoff ceiling for as long as an app kept the
/// slot armed.
///
/// The observable is the agent's own report (`needs_reenrollment`) inside a
/// named budget far below the bearer's renewal lead — so a pass cannot be
/// the ordinary schedule arriving.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_device_leg_ends_the_credential_while_the_app_bearer_still_works() {
    /// Far below the bearer's renewal lead (`AGENT_RENEW_LEAD_SECS` before an
    /// hour-long expiry), so only the leg's report can satisfy it.
    const REFUSAL_BUDGET: Duration = Duration::from_secs(90);

    let nest = start_test_nest(OWNER_SECRET).await;
    let (nest_url, bearer) = (nest.base_url.clone(), nest.http_token.clone());
    let world = AgentWorld::new("fauna-agent-t3-");
    let socket = world.socket_path();
    let keypair = ActorKeypair::from_secret(OWNER_SECRET);
    let actor_hex = hex::encode(keypair.actor_id().0);
    let named_row = hex::encode(DEVICE_ID);

    common::enroll_as_w5_app(
        OWNER_SECRET,
        &world,
        &nest_url,
        &actor_hex,
        Some((&named_row, "test machine")),
        named_row.clone(),
    )
    .await;

    // The app's own session, an hour from expiry — what an open app keeps
    // pushing, and what a device delete leaves alive.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut agent = world.spawn_agent();
    await_agent(&socket);
    expect_ok(
        &socket,
        RequestMethod::ProvisionCapability(SyncCapability::new(
            vec![0x11; 32],
            keypair.actor_id().0.to_vec(),
            nest_url.clone(),
            named_row.clone(),
            BearerToken::new(bearer.clone(), now + 3600),
        )),
    );
    let deadline = Instant::now() + MOUNT_BUDGET;
    loop {
        if let ResponsePayload::ServiceStatus(status) =
            expect_ok(&socket, RequestMethod::GetServiceStatus)
            && status.store_principal_actor.as_deref() == Some(actor_hex.as_str())
        {
            assert!(
                !status.needs_reenrollment,
                "an enrolled machine must not read refused before the gesture"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent never advertised the store principal within {MOUNT_BUDGET:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The gesture, from "another device".
    let app_client =
        fauna_client::NestClient::new(nest_url.clone(), ActorKeypair::from_secret(OWNER_SECRET));
    app_client.connect().await.expect("the app's nest connect");
    let sync = fauna_client_sync::SyncClient::new(app_client.as_ref());
    assert!(
        sync.devices_delete(named_row.clone())
            .await
            .expect("devices.delete")
            .deleted
    );

    // The app's session survives the delete — the half of the agent that
    // keeps succeeding.
    let survivor =
        fauna_client::NestClient::with_auth(Arc::new(fauna_client::AuthClient::bearer_only(
            nest_url.clone(),
            keypair.actor_id().0,
            Arc::new(fauna_nest_http::StaticBearer(bearer.clone())),
            fauna_client::pinned_http_client(&nest_url),
        )));
    survivor
        .connect()
        .await
        .expect("a device delete spares the app's own session");

    let deadline = Instant::now() + REFUSAL_BUDGET;
    loop {
        if let ResponsePayload::ServiceStatus(status) =
            expect_ok(&socket, RequestMethod::GetServiceStatus)
            && status.needs_reenrollment
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the device-principal leg was refused but the agent never asked the \
             renewal question within {REFUSAL_BUDGET:?} — it is still dialling a \
             principal the nest no longer holds a grant for"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    agent.kill_uncleanly();
}

/// **A10's tier_3 proof — the onboarding-surface signed-out reconcile, against
/// the REAL spawned agent** (`sync-agent.md` § Credential model → *The
/// signed-out reconcile*, and § Sync agent → A10, whose "Owed" line names this
/// test).
///
/// A10 shipped with its **pure licensing decision** unit-tested in
/// `fauna-client-sync` and its wiring compiled into linux + tui launch routing
/// — but nothing proved the cross-process round trip: that a real agent,
/// holding a real capability, actually stops when this machine's own marker
/// licenses it, and actually does *not* when the marker names someone else. A
/// predicate returning `true` is not a teardown.
///
/// ## Why the two cases live in ONE test, in this order
///
/// The mismatch case is a **negative** assertion ("the agent is untouched"),
/// and a negative assertion against a freshly-built world is the classic
/// vacuous pass: an agent that was never serving, or a socket nobody reached,
/// satisfies it perfectly. So the mismatched marker runs **first**, and the
/// matching marker runs **second against the very same agent** — the teardown
/// that follows is what proves the earlier no-op was a decision rather than an
/// absence of one. Neither ordering works alone.
///
/// ## Why there is no sleep anywhere (e2e-conventions.md convention 14)
///
/// The reconcile awaits its own `GetServiceStatus` exchange, and — when
/// licensed — its own `UnprovisionCapability` exchange, whose handler
/// (`pipe_server::handle_unprovision_capability`) awaits `unprovision_now` in
/// full before it replies. So by the time the call returns, any teardown it
/// decided on has already been applied, and the next `GetServiceStatus` is a
/// **causal barrier**, not a settle-poll. The one deadline-poll here is the
/// positive wait for the agent to advertise its store principal, which is
/// renewal-loop-driven and so genuinely asynchronous.
///
/// ## The observable, and why it is NOT `store_principal_actor`
///
/// `unprovision_now` clears the capability slot; it deliberately does **not**
/// touch `store_principal_actor`, which only
/// `renewal::refresh_store_principal_presence` writes. So the teardown's honest
/// witness is `sync_device_id` — a direct read of the capability the teardown
/// cleared. Asserting on `store_principal_actor` instead would fail against
/// correct code, and reads like the obvious assertion right up until it does.
#[tokio::test(flavor = "multi_thread")]
async fn the_onboarding_reconcile_stops_this_machines_signed_out_account_but_spares_a_siblings() {
    let nest = start_test_nest(OWNER_SECRET).await;
    let (nest_url, bearer) = (nest.base_url.clone(), nest.http_token.clone());
    let world = AgentWorld::new("fauna-agent-t3-");
    let socket = world.socket_path();
    let keypair = ActorKeypair::from_secret(OWNER_SECRET);
    let actor_hex = hex::encode(keypair.actor_id().0);
    let named_row = hex::encode(DEVICE_ID);

    // ── 1: the machine enrols, exactly as the W5 app does ────────────────
    // The enrolment is what puts a store principal in the shared slot, which is
    // what makes the agent advertise `store_principal_actor` at all — the field
    // the client-side guard matches this machine's marker against.
    common::enroll_as_w5_app(
        OWNER_SECRET,
        &world,
        &nest_url,
        &actor_hex,
        Some((&named_row, "test machine")),
        named_row.clone(),
    )
    .await;

    // ── 2: the real agent, provisioned and serving ───────────────────────
    let mut agent = world.spawn_agent();
    await_agent(&socket);
    let cap = SyncCapability::new(
        vec![0x11; 32],
        keypair.actor_id().0.to_vec(),
        nest_url.clone(),
        named_row.clone(),
        BearerToken::new(bearer, 1),
    );
    expect_ok(&socket, RequestMethod::ProvisionCapability(cap));

    // The precondition the whole test rests on: the agent both HOLDS the
    // capability and ADVERTISES the principal. Without the advertisement the
    // guard reads "no advertised actor" and no-ops for the wrong reason, and
    // step 4 below would pass while proving nothing.
    let deadline = Instant::now() + MOUNT_BUDGET;
    loop {
        if let ResponsePayload::ServiceStatus(status) =
            expect_ok(&socket, RequestMethod::GetServiceStatus)
            && status.store_principal_actor.as_deref() == Some(actor_hex.as_str())
        {
            assert_eq!(
                status.sync_device_id.as_deref(),
                Some(named_row.as_str()),
                "the agent must be holding the capability before either case runs"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent never advertised the store principal within {MOUNT_BUDGET:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let endpoint = fauna_ipc::endpoint::AgentEndpoint::Unix(socket.clone());

    // ── 3: A SIBLING account's marker — the single-slot guard ────────────
    // The hazard A10's guard exists for: another app on this box sits at
    // account B's onboarding screen while this agent serves account A. An
    // unconditional teardown would kill A's sync from B's login screen.
    let sibling = ActorKeypair::from_secret([0x99; 32]);
    assert_ne!(
        sibling.actor_id().0,
        keypair.actor_id().0,
        "the sibling must be a genuinely different actor or this case is a tautology"
    );
    let sibling_marker = fauna_ipc::sync::SignedOutMarker::new(
        sibling.actor_id().0.to_vec(),
        fauna_core::data::Timestamp::now_millis_or_zero(),
    );
    fauna_client_sync::agent::signed_out_onboarding_reconcile_with_marker(
        &endpoint,
        &sibling_marker,
    )
    .await;

    match expect_ok(&socket, RequestMethod::GetServiceStatus) {
        ResponsePayload::ServiceStatus(status) => assert_eq!(
            status.sync_device_id.as_deref(),
            Some(named_row.as_str()),
            "a sibling app's onboarding surface must NOT tear down this \
             machine's live account: the marker names another actor, so the \
             reconcile is a no-op. (This assertion is only meaningful because \
             the matching case below tears the SAME agent down.)"
        ),
        other => panic!("unexpected status payload: {other:?}"),
    }

    // ── 4: THIS machine's own marker — the teardown ──────────────────────
    // The state a lost `UnprovisionCapability` push leaves behind: the marker
    // was written (`unprovision` writes it BEFORE it sends), the message never
    // landed, and the app has now come back up on its onboarding surface.
    let own_marker = fauna_ipc::sync::SignedOutMarker::new(
        keypair.actor_id().0.to_vec(),
        fauna_core::data::Timestamp::now_millis_or_zero(),
    );
    fauna_client_sync::agent::signed_out_onboarding_reconcile_with_marker(&endpoint, &own_marker)
        .await;

    match expect_ok(&socket, RequestMethod::GetServiceStatus) {
        ResponsePayload::ServiceStatus(status) => {
            assert_eq!(
                status.sync_device_id, None,
                "the agent must have dropped the capability for the account \
                 this machine itself recorded signing out — immediately, on \
                 the onboarding surface's evidence, without waiting for the \
                 renewal loop's own 300 s-bounded reconcile"
            );
            assert!(
                matches!(status.connection, ConnectionState::Disconnected),
                "and with the capability gone it serves no nest connection"
            );
        }
        other => panic!("unexpected status payload: {other:?}"),
    }

    agent.kill_uncleanly();
}

/// **A local presence writes the place it needs** (`file-sync.md` § 4) at the
/// one seam every desktop bind goes through: the real agent's
/// `SetLocationFolder` on a folder where this device holds no place enrols it
/// at the default point, and a re-bind writes nothing.
///
/// Flow: app sends `AddLocation` + `SetLocationFolder` → `handle_set_location_folder`
/// records the binding → `enrol_bound_place` → `FoldersClient::ensure_place` →
/// `members.list` (empty) → `places.set` at `PlaceFlags::default_place()` → the
/// nest's `folder_members` holds exactly one `sync` row for this device.
#[tokio::test(flavor = "multi_thread")]
async fn binding_a_placeless_folder_enrols_this_device_once() {
    let actor = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let nest = fauna_nest::test_support::start_test_nest(OWNER_SECRET, None, |b| {
        fauna_nest::sync_handlers::register_sync_handlers(b);
        fauna_nest::folder_handlers::register_folders_handlers(b);
    })
    .await;
    let folder_id = nest.state.db.create_folder(FOLDER, &actor).await.unwrap();
    nest.state
        .db
        .register_sync_device(&actor, &DEVICE_ID, "laptop", None, "sync")
        .await
        .unwrap();
    let folder_ref = fauna_core::folder_keys::FolderRef::Local(folder_id);

    let world = AgentWorld::new("fauna-agent-t3e-");
    let socket = world.socket_path();
    let watched = world.root().join("watched");
    std::fs::create_dir_all(&watched).unwrap();
    let mut agent = world.spawn_agent();
    await_agent(&socket);
    expect_ok(
        &socket,
        RequestMethod::ProvisionCapability(SyncCapability::new(
            vec![0x11; 32],
            actor.to_vec(),
            nest.base_url.clone(),
            hex::encode(DEVICE_ID),
            BearerToken::new(nest.http_token.clone(), 4_000_000_000),
        )),
    );
    assert!(
        nest.state
            .db
            .get_folder_members(folder_id)
            .await
            .unwrap()
            .is_empty(),
        "precondition: this device holds no place"
    );

    let bind = || {
        expect_ok(
            &socket,
            RequestMethod::AddLocation {
                path: watched.display().to_string(),
            },
        );
        expect_ok(
            &socket,
            RequestMethod::SetLocationFolder {
                path: watched.display().to_string(),
                folder: FOLDER.to_string(),
                folder_id: folder_ref.to_wire(),
            },
        );
    };
    bind();
    let seats = nest.state.db.get_folder_members(folder_id).await.unwrap();
    assert_eq!(seats.len(), 1, "one seat after the bind: {seats:?}");
    assert_eq!(seats[0].device_id, DEVICE_ID.to_vec());
    assert_eq!(
        seats[0].flags.point(),
        fauna_protocol::folders::PlaceFlags::default_place().point(),
        "enrolled at the default point"
    );

    bind();
    let again = nest.state.db.get_folder_members(folder_id).await.unwrap();
    assert_eq!(again.len(), 1, "a re-bind writes no second seat: {again:?}");

    agent.kill_uncleanly();
    let _ = std::fs::remove_dir_all(world.root());
}

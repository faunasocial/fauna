//! Shared fixtures for the `fauna-sync-agent` integration tests — the real
//! `fauna-sync-agent` binary spoken to over its per-user unix IPC socket.
//!
//! Each file under `tests/` is its own crate, so a helper used by one file is
//! dead code in the next — the module-wide allow below is what lets this
//! module be `mod common;`-ed into a test that needs only some of its
//! helpers (the fauna-nest integration tests' `tests/common/mod.rs` is the
//! same pattern).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_core::identity::ActorKeypair;
use fauna_ipc::sync::{EngineInfo, LocationInfo, RequestMethod, ResponsePayload, ResponseResult};
use fauna_ipc::sync_pipe_client::SyncPipeClient;

/// Where the agent binds its per-user control-plane socket under a private
/// test `root` — macOS's App Support path, everywhere else the XDG runtime dir.
pub fn agent_socket_path(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("Library/Application Support/Fauna/sync-agent.sock")
    } else {
        root.join("runtime/fauna/sync-agent.sock")
    }
}

/// A spawned agent's private test root — a fresh `tempdir` holding its
/// `--data-dir`, credential dir, and (non-macOS) `XDG_CONFIG_HOME` +
/// `XDG_RUNTIME_DIR`.
///
/// Byte-identical across `same_nest_push_nudge.rs` and
/// `cross_nest_agent_capstone.rs` apart from each file's own `tempdir` prefix
/// (round 29) — `new` takes that prefix as its one point of variance. Merged
/// with `agent_process_tier3.rs`'s own richer copy (round 30): the two
/// `shared_*` account-store accessors below are dead code (`#![allow(
/// dead_code)]` above) in the two nudge tests, which never call them.
pub struct AgentWorld {
    root: PathBuf,
}

impl AgentWorld {
    pub fn new(tempdir_prefix: &str) -> Self {
        // Under /tmp directly: `sun_path` is capped at ~104 bytes on macOS and
        // a nested tempdir plus the Library suffix would flirt with it.
        let root = tempfile::Builder::new()
            .prefix(tempdir_prefix)
            .tempdir_in("/tmp")
            .unwrap()
            .keep();
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn socket_path(&self) -> PathBuf {
        agent_socket_path(&self.root)
    }

    /// The per-user **account-store** root this world's agent resolves.
    ///
    /// The agent mounts the machine's SHARED store root ([`fauna_sync_engine::
    /// account_runtime::StoreRoot::platform`], W5 (account-data-plane.md § Workstreams).5b) — deliberately not a dir
    /// derived from `--data-dir`, since a private root is the
    /// two-journals-under-one-writer-key shape. That root comes from
    /// `HOME`/`XDG_CONFIG_HOME`, which is why [`Self::spawn_agent`] sets both:
    /// isolation here is by environment, exactly as e2e-conventions.md § point
    /// 10 prescribes, with no second code path to drift from production's.
    ///
    /// This function is the test-side mirror of that resolution — it computes
    /// where the CHILD's env will land, never where this test process's own
    /// env would.
    pub fn shared_store_root(&self) -> fauna_sync_engine::account_runtime::StoreRoot {
        if cfg!(target_os = "macos") {
            // The user-domain root (2026-08-25) — never the app-group
            // container, which the agent may not open (installers/macos.md §
            // Identifier domain, item 5).
            fauna_sync_engine::account_runtime::StoreRoot::at(
                fauna_core::platform_ids::apple_user_domain_home(Some(self.root.clone()))
                    .join("Fauna")
                    .join("sync"),
            )
        } else {
            fauna_sync_engine::account_runtime::StoreRoot::at(
                self.root.join("config").join("fauna").join("sync"),
            )
        }
    }

    /// The credential slot the child agent and this test process share — the
    /// T10 slot (`fauna-account-store` namespace) an app populates at
    /// enrollment and the seedless agent reads its `BackupKey` + writer key
    /// from.
    ///
    /// Built with the env-free [`fauna_credential_store::CredentialStore::
    /// with_file_backend`] rather than by setting `FAUNA_E2E_CREDENTIAL_DIR`
    /// in this process: these crates never mutate process-global env in tests
    /// (two parallel tests would clobber each other's world), while the
    /// *child* gets the same dir through its own env.
    pub fn shared_credentials(&self) -> fauna_credential_store::CredentialStore {
        fauna_credential_store::CredentialStore::with_file_backend(
            fauna_sync_engine::account_runtime::CRED_NAMESPACE,
            self.root.join("creds"),
        )
    }

    pub fn spawn_agent(&self) -> KillOnDrop {
        self.spawn_agent_trusting(&[])
    }

    /// [`Self::spawn_agent`], with each of `nests` (base URLs of
    /// [`start_nest`] nests) trusted as an escrow holder under
    /// [`deployment_key`] — the e2e trust seed standing in for the TLS pin a
    /// plaintext nest can never graduate (`fauna_client::trust::
    /// trusted_escrow_holders`). Without it the agent's account store resolves
    /// no generation tip, so it reads no tip-sealed kind — the account's
    /// folder-key custody among them.
    pub fn spawn_agent_trusting(&self, nests: &[&str]) -> KillOnDrop {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fauna-sync-agent"));
        if !nests.is_empty() {
            let holder = hex::encode(deployment_key().verifying_key().to_bytes());
            let seed: Vec<String> = nests.iter().map(|n| format!("{n}={holder}")).collect();
            cmd.env("FAUNA_E2E_TRUST_NEST_IDENTITY", seed.join(","));
        }
        cmd.arg("--data-dir")
            .arg(self.root.join("data"))
            .env("FAUNA_E2E_CREDENTIAL_DIR", self.root.join("creds"))
            // The engine's own pull/`file uploaded` lines are the diagnostic
            // surface when an assertion fails.
            .env("RUST_LOG", "info,fauna_sync_engine=debug");
        // `HOME` is set on every unix, not just macOS: since W5.5b the agent
        // resolves the shared ACCOUNT-STORE root from the home/XDG
        // environment (`StoreRoot::platform`), so a child with the
        // developer's real `HOME` would reach into the developer's real
        // account store — machine-global state leaking into a run, which
        // e2e-conventions.md point 10 forbids.
        cmd.env("HOME", &self.root);
        if !cfg!(target_os = "macos") {
            // `XDG_CONFIG_HOME` wins over `HOME` on linux, so leaving it
            // inherited would defeat the `HOME` override above.
            let config = self.root.join("config");
            std::fs::create_dir_all(&config).unwrap();
            cmd.env("XDG_CONFIG_HOME", config);
            let runtime = self.root.join("runtime");
            std::fs::create_dir_all(&runtime).unwrap();
            cmd.env("XDG_RUNTIME_DIR", runtime);
        }
        KillOnDrop(cmd.spawn().expect("spawn fauna-sync-agent"))
    }
}

/// One bounded request over a fresh connection (the client is single-exchange
/// per connection on the frame codec, matching the shipped consumers).
pub fn request(socket: &Path, method: RequestMethod) -> ResponseResult {
    SyncPipeClient::connect_socket(socket)
        .expect("connect agent socket")
        .request(method)
        .expect("agent request")
        .result
}

pub fn expect_ok(socket: &Path, method: RequestMethod) -> ResponsePayload {
    match request(socket, method) {
        ResponseResult::Ok(payload) => payload,
        ResponseResult::Err(e) => panic!("agent returned error: {e}"),
    }
}

pub fn list_locations(socket: &Path) -> Vec<LocationInfo> {
    match expect_ok(socket, RequestMethod::ListLocations) {
        ResponsePayload::Locations(list) => list,
        other => panic!("unexpected ListLocations payload: {other:?}"),
    }
}

pub fn list_engines(socket: &Path) -> Vec<EngineInfo> {
    match expect_ok(socket, RequestMethod::ListEngines) {
        ResponsePayload::Engines(list) => list,
        other => panic!("unexpected ListEngines payload: {other:?}"),
    }
}

/// Block until the set named `folder` has a **serving** engine, and return the
/// engine list that showed it; panic once `budget` runs out.
///
/// A bound set is never serving *synchronously* after a bind, a provision, or a
/// respawn: the agent withholds its engine until its own content-key resolution
/// names the set, and that resolution re-reads asynchronously on every
/// reconcile (`content_keys.rs`, *Edges*; `engine_driver.rs`'s fail-closed
/// filter). So a deadline poll (convention 14), never an immediate assert.
pub fn await_serving_engine(socket: &Path, folder: &str, budget: Duration) -> Vec<EngineInfo> {
    let deadline = Instant::now() + budget;
    loop {
        let engines = list_engines(socket);
        if engines.iter().any(|e| e.folder == folder && e.serving) {
            return engines;
        }
        assert!(
            Instant::now() < deadline,
            "set {folder:?} never reached a serving engine within {budget:?} — the agent's \
             own content-key resolution did not name it: {engines:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Wait for the agent's socket to accept a connection and answer a request.
///
/// Merged (round 31) from three near-identical copies: `agent_process_tier3.rs`'s
/// own `await_agent` (20s deadline, returned the connected `SyncPipeClient` —
/// every call site discarded it) and `agent_sigterm.rs`'s `await_agent_serving`
/// (30s `STARTUP_BUDGET`, byte-identical loop body otherwise). Kept this fn's
/// 30s deadline as the strictly more generous of the two.
pub fn await_agent(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(client) = SyncPipeClient::connect_socket(socket)
            && client.request(RequestMethod::GetServiceStatus).is_ok()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "agent socket never came up at {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A spawned agent process that cannot outlive the test, however it ends.
pub struct KillOnDrop(pub Child);

impl KillOnDrop {
    /// SIGKILL — no shutdown path runs; resume must come from persisted state.
    pub fn kill_uncleanly(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }

    /// The child's pid — for the signal-based cases, which need to send a
    /// specific signal rather than `Child::kill`'s SIGKILL.
    pub fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Non-blocking reap, so a hung agent fails on a deadline instead of
    /// hanging the suite.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An authenticated, connected `NestClient` — byte-identical across
/// `same_nest_push_nudge.rs` and `cross_nest_agent_capstone.rs` (round 29).
pub async fn connected_client(base: &str, keypair: ActorKeypair) -> Arc<fauna_client::NestClient> {
    let nest = fauna_client::NestClient::new(base.to_string(), keypair);
    nest.connect().await.expect("connect (auth + WS upgrade)");
    let mut state = nest.connection_state();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if *state.borrow() == fauna_client::types::ConnectionState::Connected {
                return;
            }
            state
                .changed()
                .await
                .expect("connection-state channel open");
        }
    })
    .await
    .expect("client reached Connected within 10s");
    nest
}

/// Poll until `path` holds exactly `expected`, naming the broken link on
/// failure (`testing.md` § point 6 — a failure must diagnose itself).
///
/// Structurally identical across `same_nest_push_nudge.rs` and
/// `cross_nest_agent_capstone.rs` (round 29) apart from each file's own log
/// tag and diagnostic hint — both stay per-caller: each names a different
/// broken link and is genuinely load-bearing content, not duplication. The
/// two source poll intervals (250ms / 500ms) unify on the finer one; either
/// is well under any window this gates and changes only iteration count, not
/// outcome.
pub fn await_file_content_within(
    path: &Path,
    expected: &[u8],
    window: Duration,
    what: &str,
    log_tag: &str,
    diagnostic_hint: &str,
) {
    let started = Instant::now();
    let deadline = started + window;
    let mut actual: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        actual = std::fs::read(path).ok();
        if actual.as_deref() == Some(expected) {
            eprintln!(
                "[{log_tag}] {what}: arrived after {:.1}s",
                started.elapsed().as_secs_f64()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let detail = match actual {
        None => "NEVER ARRIVED".to_string(),
        Some(bytes) => format!("{} bytes, content mismatch", bytes.len()),
    };
    panic!(
        "{what}: {} — {detail}.\n  {diagnostic_hint}",
        path.display()
    );
}

/// One in-process nest serving every kind the sync-agent capstone journeys
/// touch, plus a real `DiskBlobStore`-backed `BackupService` so the public
/// chunk/manifest byte routes actually serve. Plain HTTP on loopback.
///
/// Merged (round 32) from two byte-identical copies apart from
/// `cross_nest_agent_capstone.rs`'s extra `federation_router` +
/// `conversations_handlers` registration — `same_nest_push_nudge.rs`'s own
/// journey never exercises either kind, so registering them unconditionally
/// (rather than parameterizing) costs it nothing (priority #4: the richest
/// existing pattern, not the leanest). `#[cfg(feature = "tier3-nest")]`
/// because `fauna-nest`/`axum` are optional, `tier3-nest`-gated deps
/// (Cargo.toml) — this fn is dead code in every non-tier3-nest test binary
/// that still `mod common;`s this file (e.g. `agent_sigterm.rs`), same as
/// every other helper here (module-wide `#![allow(dead_code)]` above).
#[cfg(feature = "tier3-nest")]
pub async fn start_nest() -> (String, Arc<fauna_nest::routes::AppState>) {
    use ed25519_dalek::SigningKey;
    use fauna_nest::db::CacheDb;
    use fauna_nest::nest_identity::NestIdentity;
    use fauna_nest::routes::AppState;
    use fauna_nest::token_store::TokenStore;

    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    // Outlives the test process; the temp root is reaped by the OS.
    std::mem::forget(blob_dir);
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::filesync_handlers::register_filesync_handlers(&mut b);
            // The generation-escrow doors: an account's first tip-sealed
            // write (its folder-key custody) mints the generation it seals
            // under and deposits it here.
            fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(TokenStore::new()),
            ..Default::default()
        },
        nest_signing_key: Some(deployment_key()),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state)
}

/// Enrol this machine as the W5 app would for the account `secret` seeds — the
/// boilerplate shared by every test proving app-dead agent behavior against a
/// real store principal. It leaves the T10 slot the seedless agent reads: the
/// `BackupKey` and the writer key certified with `SyncWrite`, which the agent
/// loads as its change signer — without it every record goes out unsigned and
/// the nest refuses it (`signature_required`).
/// `pre_register`, when `Some((device_id, label))`, registers that device row
/// through `SyncClient::register` first (theT11 shape tests need the
/// row to pre-exist); `enrollment_target_device_id` mirrors the identically
/// named `AccountRuntimeParams` field — the machine's named row.
pub async fn enroll_as_w5_app(
    secret: [u8; 32],
    world: &AgentWorld,
    nest_url: &str,
    actor_hex: &str,
    pre_register: Option<(&str, &str)>,
    enrollment_target_device_id: String,
) {
    let app_client =
        fauna_client::NestClient::new(nest_url.to_string(), ActorKeypair::from_secret(secret));
    app_client.connect().await.expect("the app's nest connect");
    if let Some((device_id, label)) = pre_register {
        fauna_client_sync::SyncClient::new(app_client.as_ref())
            .register(device_id.to_string(), label, None)
            .await
            .expect("the named row registers");
    }
    let app = fauna_sync_engine::account_runtime::AccountStoreRuntime::start(
        fauna_sync_engine::account_runtime::AccountRuntimeParams {
            store_backup_exclusion:
                fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                    platform: "test".into(),
                },
            store_root: world.shared_store_root(),
            actor_id_hex: actor_hex.to_string(),
            rpc: Arc::clone(&app_client),
            process_rpc: None,
            principal: fauna_sync_engine::account_runtime::RuntimePrincipal::SeedHolding(
                ActorKeypair::from_secret(secret).into(),
            ),
            credentials: world.shared_credentials(),
            reconnects: None,
            pushes: None,
            backstop_interval: fauna_sync_engine::account_runtime::DEFAULT_BACKSTOP_INTERVAL,
            memberships: None,
            trusted_escrow_holders: trusted_holders(),
            attested_predecessors: Default::default(),
            linked_nests: None,
            owed_nests: None,
            peer_transport: None,
            enrollment_target_device_id,
        },
    )
    .await
    .expect("the signed-in app's assembly enrolls this machine");
    let _ = app.reconcile_now().await;
    app.shutdown().await;
}

/// The [`start_nest`] nests' deployment identity — the escrow receipt signer
/// every account plane in these tests trusts, as production pins it.
pub fn deployment_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x66; 32])
}

/// The escrow-holder trust an in-test app assembles with: [`deployment_key`].
fn trusted_holders() -> fauna_sync_engine::account_runtime::TrustedHolderSource {
    fauna_sync_engine::account_runtime::fixed_holders(vec![
        deployment_key().verifying_key().to_bytes(),
    ])
}

// ─────────────────────────────────────────────────────────────────────────
// An app of the account on another machine — where custody is written
// ─────────────────────────────────────────────────────────────────────────
//
// Folder-key custody is plane-only (`fauna.state.folder-keys`,
// `config-dissolution.md`'s kinds table): an app writes it through its account
// store's door, sealed under the account's current generation, and the agent
// reads it back through the store it mounts. So the identity-holding apps these
// tests play are real account runtimes, each on a store root of its own — a
// second machine of the account, never the agent's.

/// The account's signed-in app on ANOTHER machine: a seed-holding account
/// runtime over its own store root and credential slot, alive for the test's
/// length. Its [`Self::folder_keys`] is the custody store the production
/// writers (`FoldersAuthor`, the custody-ingest sink, `create_set`) take.
#[cfg(feature = "tier3-nest")]
pub struct AppSeat {
    handle: fauna_sync_engine::account_runtime::AccountStoreHandle,
    nudges_lost: Arc<std::sync::atomic::AtomicBool>,
    /// The seat machine's state root, removed when the seat drops.
    _root: tempfile::TempDir,
}

#[cfg(feature = "tier3-nest")]
impl AppSeat {
    /// Sign the account `secret` seeds in on a fresh machine against the
    /// [`start_nest`] nest `state` serves at `nest_url`: the runtime enrolls
    /// the machine and publishes the account's escrow target, so its first
    /// custody write can mint the first generation.
    pub async fn sign_in(
        state: &Arc<fauna_nest::routes::AppState>,
        nest_url: &str,
        secret: [u8; 32],
    ) -> Self {
        let keypair = ActorKeypair::from_secret(secret);
        let root = tempfile::tempdir().unwrap();
        let nudges_lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rpc = SeatRpc {
            nest: connected_client(nest_url, ActorKeypair::from_secret(secret)).await,
            state: Arc::clone(state),
            actor: keypair.actor_id().0,
            nudges_lost: Arc::clone(&nudges_lost),
        };
        let mut device = [0u8; 32];
        getrandom::fill(&mut device).unwrap();
        let handle = fauna_sync_engine::account_runtime::AccountStoreRuntime::start(
            fauna_sync_engine::account_runtime::AccountRuntimeParams {
                store_backup_exclusion:
                    fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                        platform: "test".into(),
                    },
                store_root: fauna_sync_engine::account_runtime::StoreRoot::at(
                    root.path().join("sync"),
                ),
                actor_id_hex: hex::encode(keypair.actor_id().0),
                rpc,
                process_rpc: None,
                principal: fauna_sync_engine::account_runtime::RuntimePrincipal::SeedHolding(
                    keypair.into(),
                ),
                credentials: fauna_credential_store::CredentialStore::with_file_backend(
                    fauna_sync_engine::account_runtime::CRED_NAMESPACE,
                    root.path().join("creds"),
                ),
                reconnects: None,
                // No push arm: every pass here is one the test asks for
                // ([`Self::sync`]), so what the seat has walked is never a race.
                pushes: None,
                backstop_interval: fauna_sync_engine::account_runtime::DEFAULT_BACKSTOP_INTERVAL,
                memberships: None,
                trusted_escrow_holders: trusted_holders(),
                attested_predecessors: Default::default(),
                linked_nests: None,
                owed_nests: None,
                peer_transport: None,
                enrollment_target_device_id: hex::encode(device),
            },
        )
        .await
        .expect("the app's account runtime assembles");
        let seat = Self {
            handle,
            nudges_lost,
            _root: root,
        };
        seat.sync().await;
        seat
    }

    /// One full pump pass: walk the account's feeds, top the current
    /// generation up to every enrolled machine, publish what is pending. Run
    /// it after another machine of the account enrolls, so that machine's
    /// principal keys the custody rows too.
    pub async fn sync(&self) {
        self.handle
            .reconcile_now()
            .await
            .expect("the app's pump pass");
    }

    /// The account's folder-key custody as this app reads and writes it —
    /// production's `PlaneFolderKeys` over the seat's runtime, each write
    /// published before it answers.
    pub fn folder_keys(&self) -> Arc<dyn fauna_client_folders::FolderKeyStore> {
        let handle = self.handle.clone();
        Arc::new(SeatFolderKeys {
            store: fauna_account_seams::folder_keys::PlaneFolderKeys::new(Box::new(move || {
                Some(handle.clone())
            })),
            handle: self.handle.clone(),
        })
    }

    /// While `lost`, this app's account-state writes are stored on the nest
    /// with NO `state-fleet` nudge fanned out — the best-effort push, lost.
    pub fn lose_nudges(&self, lost: bool) {
        self.nudges_lost
            .store(lost, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(feature = "tier3-nest")]
type SeatHandleSource =
    Box<dyn Fn() -> Option<fauna_sync_engine::account_runtime::AccountStoreHandle> + Send + Sync>;

/// [`AppSeat::folder_keys`]: the production seam, plus the pump pass that
/// publishes a write before the caller's next step.
#[cfg(feature = "tier3-nest")]
struct SeatFolderKeys {
    store: fauna_account_seams::folder_keys::PlaneFolderKeys<SeatHandleSource>,
    handle: fauna_sync_engine::account_runtime::AccountStoreHandle,
}

#[cfg(feature = "tier3-nest")]
#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for SeatFolderKeys {
    async fn load(&self) -> anyhow::Result<fauna_core::data::FoldersConfig> {
        self.store.load().await
    }
}

#[cfg(feature = "tier3-nest")]
#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyStore for SeatFolderKeys {
    async fn merge(
        &self,
        replica: fauna_core::data::FoldersConfig,
    ) -> anyhow::Result<fauna_core::data::FoldersConfig> {
        let custody = self.store.merge(replica).await?;
        self.handle.reconcile_now().await?;
        Ok(custody)
    }

    async fn settle_removal(
        &self,
        removal: fauna_core::data::FolderPendingRemoval,
    ) -> anyhow::Result<fauna_core::data::FoldersConfig> {
        let custody = self.store.settle_removal(removal).await?;
        self.handle.reconcile_now().await?;
        Ok(custody)
    }
}

/// An [`AppSeat`]'s nest connection. It is the seat's real client, except
/// that while the seat's nudges are lost a `fauna.account.state.put` is
/// stored the way the nest's handler stores it — and nothing is fanned out.
#[cfg(feature = "tier3-nest")]
#[derive(Clone)]
struct SeatRpc {
    nest: Arc<fauna_client::NestClient>,
    state: Arc<fauna_nest::routes::AppState>,
    actor: [u8; 32],
    nudges_lost: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "tier3-nest")]
impl SeatRpc {
    fn stores_silently(&self, kind: &str) -> bool {
        kind == fauna_protocol::account_state::KIND_STATE_PUT
            && self.nudges_lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The storage half of the nest's `fauna.account.state.put` handler, with
    /// no `notify_sync_changed_scoped` after it.
    async fn store_without_a_nudge<Reply: serde::de::DeserializeOwned>(
        &self,
        request: Vec<u8>,
    ) -> Reply {
        use fauna_protocol::account_state::{AccountStatePutReply, AccountStatePutRequest};
        let req: AccountStatePutRequest =
            fauna_core::encoding::canonical_decode(&request).expect("a state.put request");
        let writer: [u8; 32] = hex::decode(&req.writer_id)
            .expect("writer id hex")
            .try_into()
            .expect("a 32-byte writer id");
        let item_key: [u8; 32] = req
            .item_key
            .as_ref()
            .try_into()
            .expect("a 32-byte item key");
        let scope = self
            .state
            .db
            .get_or_create_state_scope(&self.actor, &req.scope)
            .await
            .expect("the account's state scope");
        let seq = self
            .state
            .db
            .record_account_state_entry(
                &self.actor,
                scope,
                &item_key,
                &writer,
                req.writer_seq,
                &req.op,
                req.entry.as_ref(),
                req.cas_base,
                &[],
            )
            .await
            .expect("the entry stores")
            .unwrap_or_else(|e| panic!("the nest refused the silently stored entry: {e}"))
            .seq;
        let reply = fauna_core::encoding::canonical_encode(&AccountStatePutReply {
            seq,
            ..Default::default()
        })
        .unwrap();
        fauna_core::encoding::canonical_decode(&reply).expect("a state.put reply")
    }
}

#[cfg(feature = "tier3-nest")]
impl fauna_protocol::RpcRequester for SeatRpc {
    type Error = fauna_client::NestClientError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if self.stores_silently(kind) {
            let request = fauna_core::encoding::canonical_encode(&payload).unwrap();
            drop(payload);
            return Ok(self.store_without_a_nudge(request).await);
        }
        fauna_protocol::RpcRequester::request(&*self.nest, kind, payload).await
    }
}

#[cfg(feature = "tier3-nest")]
impl fauna_protocol::KeyedRpcRequester for SeatRpc {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        fauna_protocol::KeyedRpcRequester::request_keyed(
            &*self.nest,
            kind,
            idempotency_key,
            payload,
        )
        .await
    }
}

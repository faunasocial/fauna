//! **Apple File Provider read + write paths** (tier_3) — the interim **FFI-seam**
//! proof of the M2 read path, the M3 write path (create/rename/delete with the
//! recorded-ack gate; the baseVersion-mismatch conflict arm → shared auto-resolve),
//! and the live-refresh tick (in-session re-pull → new/modified remote changes
//! appear without a domain re-add), driven headlessly against a real in-process nest.
//!
//! `docs/goal/behavior/file-sync.md` § On-Demand Files → Apple File Provider
//! binding owns the milestone chain; its **Headless-first testing** bullet
//! ratifies (2026-07-19) exactly this vehicle: the read-path *mechanism*
//! (populate → enumerate → hydrate → decrypt under `BackupKey`, all shared-Rust
//! `provider_face` logic) is proven by driving [`FfiFileProviderHost`] **directly**
//! against a real nest — injecting the owner `BackupKey` + a real bearer, with no
//! appex / OS-FileProvider / Keychain in the loop. The **appex-driven** end-to-end
//! tier_3 (`tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py`)
//! stays `skipif`-guarded on a code-signing identity: the app→appex credential
//! rendezvous rides the app-group **data-protection Keychain**, which returns
//! `errSecMissingEntitlement (-34018)` under ad-hoc signing — so it is
//! signing-gated pending the org's Apple Developer cert, and this FFI-seam proof
//! covers the meantime.
//!
//! Why the `conformance_*_client.rs` shape (not the cabi/ctypes bridge the same
//! day floated): [`FfiFileProviderHost::app_dead`] is a plain async Rust type, so
//! the established in-process-nest-plus-real-client-crates idiom drives it with
//! **zero new binding surface** — one `cargo test`, identical on all three dev OSes,
//! at the exact seam every app's on-demand host consumes (apple's appex today;
//! the windows service's planned `provider_face` migration and a Linux FUSE root
//! inherit it).
//!
//! What only this test catches: the whole app-dead read path composes end to end
//! against a *real* nest — the seed-less `build_engine` construction, the
//! control-inverted `populate_placeholders_from_nest` (no watcher/loop folds the
//! set's `changes.list`, so a missing populate makes the app-dead extension
//! enumerate nothing — the exact slice-3a gap slice 4 closed), the shared
//! `provider_face` serving cores, and the `BackupKey`-keyed chunk decrypt on
//! `fetch`. The write side goes through the **production** [`SyncEngine::upload_file`]
//! (seal under `BackupKey::derive(seed)` → chunk+manifest HTTP upload →
//! `fauna.sync.changes.record`), so this is a genuine owner-writes →
//! app-dead-`BackupKey`-reads round trip, not a hand-rolled fixture.
//!
//! Harness: mirrors `conformance_sync_engine_record_commit.rs` (real `AppState` +
//! `BackupService` + auth/sync/folder handlers over a bound `TcpListener`) plus
//! `register_folders_handlers` (the read side's `build_engine` calls
//! `fauna.folders.list` to resolve the owner-only content binding). The owner's
//! uploading device and the app-dead File Provider extension's own device are
//! distinct (one owner, two devices — the realistic shape).
//!
//! Tier: tier_3 (every binary real, real wire, real seal/decrypt — the only
//! stand-in is that the OS File Provider surface is replaced by driving the FFI
//! host directly, which is the *point* of the FFI-seam proof).

use std::sync::Arc;
use std::time::Duration;

use fauna_core::crypto::BackupKey;
use fauna_core::folder_keys::FolderRef;
use fauna_core::format::{ConflictPolicy, FormatRegistry};
use fauna_core::identity::ActorKeypair;
use fauna_ffi::{
    FfiBearerProvider, FfiChangeSignerCarriage, FfiChangeSignerProvider, FfiFileProviderHost,
};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::transfer::TransferPool;

/// The owner's Ed25519 secret — the sole custody root. `BackupKey::derive` of
/// this is the same key the app-dead host is handed (and the write engine holds),
/// proven equivalent-by-construction in `engine_lifecycle`.
const OWNER_SECRET: [u8; 32] = [0x51; 32];
/// The owner's uploading device (e.g. their laptop).
const WRITER_DEVICE_ID: [u8; 32] = [0x09; 32];
/// The app-dead File Provider extension's own device — distinct from the writer's
/// (one owner, N devices). `fetch_changes` self-excludes nothing
/// (`changes_list(.., None, ..)`), so the writer's change is visible regardless,
/// but a distinct id is the realistic, register-clean shape.
const FP_HOST_DEVICE_ID: [u8; 32] = [0x0A; 32];
const FOLDER: &str = "fp_read_path";
/// `FOLDER`'s set nonce — stored on the nest's row and in the owner's
/// folder-keys custody at [`start_test_nest`], as a production create does
/// (`set_lifecycle::create_set`). Every record into `FOLDER` is signed under it
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*), and
/// every reader verifies under it.
const SET_NONCE: [u8; 32] = [0x6F; 32];
/// The owner's machine principal the owner-only nests enroll (row + root-signed
/// `[RenewBearer, SyncWrite]` grant) — the writer key the app provisions its
/// File Provider / DocumentsProvider host ([`TestSigner::owner_principal`]).
const HOST_PRINCIPAL_SECRET: [u8; 32] = [0x5A; 32];

const PROOF_CONTENT: &[u8] = b"fauna file provider read-path proof \xe2\x9c\x93\n";
const NESTED_CONTENT: &[u8] = b"nested under a synthesized directory\n";

/// A test [`FfiBearerProvider`] returning a fixed, already-minted bearer — the
/// Rust stand-in for the Swift provider that reads the shared app-group Keychain.
/// The app-dead host never holds the identity seed; it presents only this token.
struct StaticFfiBearer(String);

impl FfiBearerProvider for StaticFfiBearer {
    fn current_bearer(&self) -> String {
        self.0.clone()
    }
}

/// A test [`FfiChangeSignerProvider`] — the Rust stand-in for the Swift provider
/// that reads the machine principal's signer out of the shared app-group
/// Keychain. Swappable mid-life, as the app re-provisions it when the principal
/// changes.
struct TestSigner(std::sync::Mutex<Option<FfiChangeSignerCarriage>>);

impl TestSigner {
    fn carrying(carriage: Option<FfiChangeSignerCarriage>) -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(carriage)))
    }

    /// The owner's machine principal every [`start_test_nest`] nest enrolled
    /// ([`HOST_PRINCIPAL_SECRET`]) — the host signs every record with it under
    /// the set nonce it reads from custody, and the nest verifies it by
    /// reference against the registered grant.
    fn owner_principal() -> Arc<Self> {
        let owner = ActorKeypair::from_secret(OWNER_SECRET);
        Self::carrying(Some(principal_carriage(
            &owner,
            &ActorKeypair::from_secret(HOST_PRINCIPAL_SECRET),
            true,
        )))
    }

    fn provision(&self, carriage: Option<FfiChangeSignerCarriage>) {
        *self.0.lock().unwrap() = carriage;
    }
}

impl FfiChangeSignerProvider for TestSigner {
    fn current_signer(&self) -> Option<FfiChangeSignerCarriage> {
        self.0.lock().unwrap().clone()
    }
}

/// The carriage the app provisions for `account`'s machine principal `writer`:
/// its secret and the root-signed grant over it (`[RenewBearer, SyncWrite]`, or
/// `[RenewBearer]` alone when `sync_write` is false).
fn principal_carriage(
    account: &ActorKeypair,
    writer: &ActorKeypair,
    sync_write: bool,
) -> FfiChangeSignerCarriage {
    let grant = if sync_write {
        fauna_client_sync::build_principal_grant(account, &writer.actor_id().0)
            .expect("grant builds")
    } else {
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key: writer.actor_id().0,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(account, &auth).expect("sign");
        fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env)
    };
    FfiChangeSignerCarriage {
        writer_secret: writer.secret_bytes().to_vec(),
        device_authorization: fauna_core::encoding::canonical_encode(&grant).expect("encode"),
    }
}

/// Start a real in-process nest serving the auth + sync + folder WS-RPC kinds
/// and the chunk-store HTTP routes over a bound `TcpListener`, owner registered +
/// the owner-only folder pre-created (the `folders.mode` column
/// defaults to `'sync'`, and no MLS group ⇒ owner-only content binding). Returns
/// `(http_base, owner_bearer)`.
async fn start_test_nest(owner: [u8; 32]) -> (String, String) {
    let (url, bearer, _db, _plane) = start_test_nest_with_plane(owner).await;
    (url, bearer)
}

/// [`start_test_nest`], also handing back the nest's DB — for a test that
/// changes the set's nest-side state mid-flow (binding it, as a share does).
async fn start_test_nest_with_db(owner: [u8; 32]) -> (String, String, Arc<CacheDb>) {
    let (url, bearer, db, _plane) = start_test_nest_with_plane(owner).await;
    (url, bearer, db)
}

/// [`start_test_nest_with_db`], also handing back the owner's first machine on
/// the account plane (principal [`HOST_PRINCIPAL_SECRET`]) — for a test that
/// enrolls a further principal whose host must key the plane custody.
async fn start_test_nest_with_plane(
    owner: [u8; 32],
) -> (String, String, Arc<CacheDb>, PlaneMachine) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());

    db.create_user(&owner, "free", "test").await.unwrap();
    db.create_folder(FOLDER, &owner).await.unwrap();
    // The nest's copy of the set nonce — what `fauna.folders.create` stores
    // from a production create's `set_nonce`; a signed record verifies only
    // against it (`change_signature.rs::stored_set_nonce`).
    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&SET_NONCE),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let token_store = Arc::new(TokenStore::new());
    // Mint the bearer for the OWNER's real actor (from the secret) — `owner` here
    // is already the actor_id, so `from_secret(owner)` would mint for the wrong
    // actor. The FP host presents this token on the authenticated WS upgrade
    // `GET /api/v1/ws/{actor_id}`, which rejects a token whose actor ≠ the path
    // actor with 403 (routes.rs `ws_handler`) — unlike the HTTP chunk routes,
    // which only check token validity.
    let owner_bearer = token_store
        .insert(ActorKeypair::from_secret(OWNER_SECRET).actor_id(), 3600)
        .await;
    debug_assert_eq!(ActorKeypair::from_secret(OWNER_SECRET).actor_id().0, owner);

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            // The read side's `build_engine` resolves the owner-only content
            // binding via `fauna.folders.list`; without this handler that call
            // errors → binding indeterminate → the host serves fail-closed.
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            // The generation-escrow doors — the account plane's first custody
            // write mints the generation it seals under and deposits it here.
            fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
            // `fauna.files.versions.list` — where a stale-base write's resolve
            // reads the head's recording device, a field of the winner
            // statement it signs (`SyncEngine::ingest_conflicting`).
            fauna_nest::files_handlers::register_files_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        nest_signing_key: Some(deployment_key()),
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let url = format!("http://{addr}");

    // The owner's side of a production create: the machine principal the app
    // provisions its hosts enrolled on the nest and in the account's device
    // set, and the set's nonce in the account's plane custody (where every
    // host and engine resolves it from).
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner_nest = common::connected_client(&url, ActorKeypair::from_secret(OWNER_SECRET)).await;
    register_principal(
        &owner_nest,
        &owner_kp,
        &ActorKeypair::from_secret(HOST_PRINCIPAL_SECRET),
    )
    .await;
    let plane = PlaneMachine::first(&url, OWNER_SECRET, HOST_PRINCIPAL_SECRET).await;
    let mut created = fauna_core::data::FoldersConfig::default();
    custody::record_created_set(
        &mut created,
        FOLDER,
        SET_NONCE,
        None,
        fauna_core::data::Timestamp::now().0,
    );
    plane.write_custody(&created).await;
    (url, owner_bearer, db, plane)
}

/// Enroll `writer` as a machine principal of `account` the way the ceremony
/// does — its row (device id = the writer key) plus the root-signed
/// `[RenewBearer, SyncWrite]` grant, through the real `fauna.sync.register` +
/// `fauna.sync.device_grant.register` handlers, over `account`'s own client.
async fn register_principal(
    nest: &Arc<fauna_client::NestClient>,
    account: &ActorKeypair,
    writer: &ActorKeypair,
) {
    let writer_pub = writer.actor_id().0;
    let sync = CtlSyncClient::new(nest.clone());
    sync.register(hex::encode(writer_pub), "machine", None)
        .await
        .expect("the principal's row registers");
    let grant = fauna_client_sync::build_principal_grant(account, &writer_pub).expect("grant");
    let reply = sync
        .device_grant_register(hex::encode(writer_pub), grant)
        .await
        .expect("the grant registers");
    assert!(reply.registered);
}

// ── The account plane: where folder-key custody rests ─────────────────────────
//
// `fauna.state.folder-keys` is plane-only (`config-dissolution.md`'s kinds
// table): the apps write custody through the account store's door, sealed
// under the account's current generation, and a capability host reads it back
// through a throwaway fleet replica keyed as the machine it is a process of —
// the machine principal's wrap (`on-demand-files.md` § Shared sets on a
// capability host, decision 1′). Every fixture below therefore stands up the
// account's plane for real: the nest serves the generation-escrow doors under
// a deployment identity, each machine principal enrolls in the account's
// device set, and custody lands through `folder_key_rows::merge_folder_keys`
// under a real tip — the host reads nothing else.

/// The fixture nests' deployment identity — the escrow receipt signer every
/// account plane below trusts, as production pins it.
fn deployment_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x66; 32])
}

/// One machine of an account on the account plane — the store the app hosts
/// under its machine principal (the writer key the app provisions its
/// capability hosts), driven by hand through the production doors.
struct PlaneMachine {
    account: [u8; 32],
    store: fauna_account_store::store::AccountStore<fauna_account_store::sqlite::SqliteBackend>,
    key: ed25519_dalek::SigningKey,
    custody: fauna_sync_engine::cold_replica::MemoryRetainedKeys,
    rpc: Arc<fauna_client::NestClient>,
    keys: fauna_core::crypto::AccountStateKeySchedule,
    trust: fauna_sync_engine::generation_tip::GenerationTrust,
}

impl PlaneMachine {
    /// The machine whose principal is `principal`, of the account whose
    /// identity seed is `account_secret`, enrolled in the account's device set
    /// (a root-signed cert, self-signed by the machine — the ceremony's plane
    /// half) over `url`.
    async fn join(
        url: &str,
        account_secret: [u8; 32],
        principal: ed25519_dalek::SigningKey,
    ) -> Self {
        let account = ActorKeypair::from_secret(account_secret);
        let store = fauna_account_store::store::AccountStore::open(
            fauna_account_store::sqlite::SqliteBackend::open_in_memory().unwrap(),
            &account.actor_id_hex(),
            fauna_account_store::types::WriterId(principal.verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let machine = Self {
            account: account_secret,
            store,
            rpc: common::connected_client(url, ActorKeypair::from_secret(account_secret)).await,
            keys: fauna_core::crypto::AccountStateKeySchedule::derive(&BackupKey::derive(
                &account_secret,
            )),
            trust: fauna_sync_engine::generation_tip::GenerationTrust {
                root: account.actor_id(),
                prior: Vec::new(),
                trusted_holders: vec![deployment_key().verifying_key().to_bytes()].into(),
            },
            key: principal,
            custody: Default::default(),
        };
        let cert = fauna_core::data::DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key: machine.key.verifying_key().to_bytes(),
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&account, &cert).unwrap();
        let authorization = fauna_core::encoding::canonical_encode(
            &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
        )
        .unwrap();
        let enrollment = fauna_core::encoding::canonical_encode(
            &fauna_core::generation::sign_device_enrollment(
                &machine.key,
                authorization,
                fauna_core::data::Timestamp::now().0 as i64,
            ),
        )
        .unwrap();
        machine
            .plane()
            .put(
                &fauna_sync_engine::account_state_plane::ItemId {
                    kind: fauna_protocol::merge_policy::KIND_DEVICE_SET.into(),
                    key: hex::encode(machine.key.verifying_key().to_bytes()),
                },
                enrollment,
                None,
            )
            .await
            .expect("the machine enrolls in the device set");
        machine
    }

    /// The account's first machine: enrolled, and the escrow target published
    /// from its seed-holding app — what lets the door mint the first
    /// generation at the first custody write.
    async fn first(url: &str, account_secret: [u8; 32], principal: [u8; 32]) -> Self {
        let machine = Self::join(
            url,
            account_secret,
            ed25519_dalek::SigningKey::from_bytes(&principal),
        )
        .await;
        let target =
            fauna_sync_engine::generation_mint::escrow_target_entry(&account_secret).unwrap();
        machine
            .plane()
            .put(
                &fauna_sync_engine::account_state_plane::ItemId {
                    kind: fauna_protocol::merge_policy::KIND_ESCROW_TARGET.into(),
                    key: fauna_core::generation::escrow_target_identity_key(
                        &ActorKeypair::from_secret(account_secret).actor_id(),
                    ),
                },
                target.value,
                None,
            )
            .await
            .expect("the escrow target publishes");
        machine
    }

    fn plane(
        &self,
    ) -> fauna_sync_engine::account_state_plane::AccountStatePlane<
        '_,
        fauna_account_store::sqlite::SqliteBackend,
        Arc<fauna_client::NestClient>,
    > {
        fauna_sync_engine::account_state_plane::AccountStatePlane::new(
            &self.store,
            &self.rpc,
            &self.keys,
            &self.key,
            &self.trust,
            fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
        .with_generation_custody(&self.custody)
    }

    /// Join `custody` into the account's plane custody through the production
    /// door (minting the first generation if none resolves yet) and publish
    /// it; answers the fold.
    async fn write_custody(
        &self,
        custody: &fauna_core::data::FoldersConfig,
    ) -> fauna_core::data::FoldersConfig {
        let plane = self.plane();
        let (folded, _moved) =
            fauna_sync_engine::folder_key_rows::merge_folder_keys(&self.store, &plane, custody)
                .await
                .expect("custody writes through the folder-keys door");
        plane.publish_pending().await.expect("custody publishes");
        folded
    }

    /// The pump legs a sibling machine's join needs from this one: walk the
    /// feed (the sibling's enrollment), then top the current generation up to
    /// every enrolled member — so a principal enrolled after the first mint
    /// keys the custody rows too.
    async fn admit_siblings(&self) {
        let plane = self.plane();
        plane.reconcile().await.expect("walk");
        fauna_sync_engine::generation_topup::ensure_topped_up(
            &self.store,
            &plane,
            &self.trust,
            &self.key,
        )
        .await
        .expect("top-up");
        plane.publish_pending().await.expect("publish");
    }

    /// Enroll another machine principal of this account on the plane and key
    /// it for the current generation: its own enrollment, then this machine's
    /// top-up.
    async fn enroll_sibling(&self, url: &str, principal: ed25519_dalek::SigningKey) {
        let sibling = Self::join(url, self.account, principal).await;
        sibling
            .plane()
            .publish_pending()
            .await
            .expect("the sibling's enrollment publishes");
        self.admit_siblings().await;
    }
}

/// The owner's **seed-holding** write engine (owner-only `BackupKey`, no MLS)
/// bound to `FOLDER`, whose control-plane `NestClient` is genuinely connected to
/// the real server — the production upload path. Mirrors
/// `conformance_sync_engine_record_commit.rs::engine_and_control_client`. It
/// signs every record directly with the owner's identity key under
/// [`SET_NONCE`] and reads under the same binding, as the seed-holding engine
/// `build_engine` assembles does in production.
fn owner_write_engine(
    url: &str,
    bearer: &str,
    watch_path: std::path::PathBuf,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine, nest_client) = owner_write_engine_on(url, bearer, watch_path, FOLDER);
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(ActorKeypair::from_secret(OWNER_SECRET).actor_id().0),
        ..Default::default()
    });
    engine.set_change_signer(
        Some(common::direct_signer(&ActorKeypair::from_secret(
            OWNER_SECRET,
        ))),
        Some(SET_NONCE),
    );
    (engine, nest_client)
}

/// [`owner_write_engine`] on the set named `folder`.
fn owner_write_engine_on(
    url: &str,
    bearer: &str,
    watch_path: std::path::PathBuf,
    folder: &str,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine_client, nest_client) = fauna_nest::test_support::sync_engine_auth_client(
        url,
        bearer,
        OWNER_SECRET,
        &WRITER_DEVICE_ID,
    );
    // `load` (not `default`) — the real production path, and load-bearing for
    // the save-dance test below: the watcher's `is_user_write` gate only
    // filters `~$`/`.tmp` litter when the matcher actually carries the
    // built-in default ignores (`file-sync.md` § Built-in default ignores).
    let ignore = IgnoreMatcher::load(&watch_path).unwrap_or_default();

    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(folder.to_string()),
        WRITER_DEVICE_ID,
        None, // mls
        None, // epoch_secret
        Some(BackupKey::derive(&OWNER_SECRET).into()),
        None, // mls_group_id
        None, // content_keys
        ConflictPolicy::Auto,
        FormatRegistry::new(),
        ignore,
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    (engine, nest_client)
}

/// Owner uploads `PROOF_CONTENT` at `proof.txt` and `NESTED_CONTENT` at
/// `docs/nested.txt` through the production engine (seal → chunk+manifest upload →
/// `changes.record`, each record signed by the owner's identity under
/// [`SET_NONCE`] — `mls-group-key-material.md` § M2 → *Writer-signed change
/// records*), returning the alive `watch` tempdir so its files outlive the
/// upload.
async fn seed_owner_files(url: &str, bearer: &str) -> tempfile::TempDir {
    let watch = tempfile::tempdir().unwrap();
    std::fs::write(watch.path().join("proof.txt"), PROOF_CONTENT).unwrap();
    std::fs::create_dir_all(watch.path().join("docs")).unwrap();
    std::fs::write(watch.path().join("docs/nested.txt"), NESTED_CONTENT).unwrap();

    let (engine, nest_client) = owner_write_engine(url, bearer, watch.path().to_path_buf());
    // Force the WS auth handshake up front rather than relying on the lazy connect
    // (mirrors the golden reference): without it the first record can time out
    // inside its own deadline, reading identically to a genuine rejection.
    nest_client
        .connect()
        .await
        .expect("writer nest_client must reach Connected (WS auth handshake)");

    for rel in ["proof.txt", "docs/nested.txt"] {
        let outcome = engine
            .upload_file(rel)
            .await
            .unwrap_or_else(|e| panic!("upload_file({rel}): {e}"));
        assert!(
            outcome.recorded,
            "the real nest must record the owner's upload of {rel} (the device \
             self-heals via fauna.sync.register on first rejection)"
        );
    }
    watch
}

/// Build the app-dead host for `backup_key` and drive its first read within a
/// bound — the worker connects + builds the engine + populates placeholders
/// *before* serving, so a nest that serves the wrong kinds surfaces as a
/// fail-closed error here rather than a silent hang up to the harness reaper.
/// Also returns the host's staging root (`watch_dir`) — the M3 write path
/// materializes bytes there before `ingest`/`rename`, exactly as the appex does.
async fn app_dead_host(
    url: &str,
    bearer: &str,
    backup_key: Vec<u8>,
) -> (Arc<FfiFileProviderHost>, std::path::PathBuf) {
    // `start_test_nest` mints FOLDER as the first and only row in a fresh
    // in-memory DB, so its `folders.id` is always 1.
    app_dead_host_on(
        url,
        bearer,
        backup_key,
        FolderRef::Local(1),
        TestSigner::owner_principal(),
    )
}

/// [`app_dead_host`] over any of the owner's sets, with the signer provider the
/// app provisioned it.
fn app_dead_host_on(
    url: &str,
    bearer: &str,
    backup_key: Vec<u8>,
    folder_ref: FolderRef,
    signer: Arc<TestSigner>,
) -> (Arc<FfiFileProviderHost>, std::path::PathBuf) {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let state_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    // The dirs must outlive the host; leak them (test-only) so the returned host
    // stays usable without threading the guards back to the caller.
    let state_path = state_dir.path().to_string_lossy().into_owned();
    let root_path = root_dir.path().to_path_buf();
    std::mem::forget(state_dir);
    std::mem::forget(root_dir);

    let host = FfiFileProviderHost::app_dead(
        url.to_string(),
        owner.to_vec(),
        FP_HOST_DEVICE_ID.to_vec(),
        "fauna-file-provider".to_string(),
        backup_key,
        Arc::new(StaticFfiBearer(bearer.to_string())),
        signer,
        state_path,
        // `parse_folder_id` refuses a bare name (sync_engine_host.rs) — needs the
        // wire-encoded ref `folder_ref_for_row` mints for a row.
        folder_ref.to_wire(),
        root_path.to_string_lossy().into_owned(),
        None,
    )
    .expect("app_dead host builds (32-byte inputs, owner-only set)");
    (host, root_path)
}

/// The fixture itself: the custody [`start_test_nest`] writes through the plane
/// door folds back out of a capability host's reader — a throwaway fleet
/// replica keyed by the machine principal alone. Every host test below rests
/// on this read; pinned here so a fixture that stopped keying the principal
/// fails in one place with the reason, not as two dozen unsigned-record reds.
#[tokio::test]
async fn the_fixtures_plane_custody_reaches_a_principal_keyed_reader() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET);
    let (url, bearer) = start_test_nest(owner.actor_id().0).await;
    // The host's own connection: a bearer, never the identity.
    let auth = Arc::new(fauna_client::AuthClient::bearer_only(
        url.clone(),
        owner.actor_id().0,
        Arc::new(fauna_nest_http::StaticBearer(bearer)),
        fauna_client::pinned_http_client(&url),
    ));
    // The production reader, as every host build asks for it — over a client
    // no host connected (red-verified: without the replica bringing its own
    // connection up, this read fails "the connection to the nest was lost").
    let reader = fauna_ffi::capability_host_folder_keys(
        &auth,
        owner.actor_id().0,
        &BackupKey::derive(&OWNER_SECRET),
        &*TestSigner::owner_principal(),
    )
    .expect("the reader starts");
    let custody = tokio::time::timeout(
        Duration::from_secs(30),
        fauna_client_folders::FolderKeyReader::load(&reader),
    )
    .await
    .expect("the read must not hang")
    .expect("custody reads");
    assert_eq!(
        custody::live_set_nonce(&custody, FOLDER),
        Some(SET_NONCE),
        "the host's reader folds the set nonce the owner's door wrote: {custody:?}"
    );
}

/// The full M2 read path: the app-dead host, handed only the owner `BackupKey` +
/// a bearer (never the seed), populates from the nest, enumerates the root and a
/// synthesized subdirectory, and hydrates both files back to their exact
/// plaintext — proving populate → enumerate → hydrate → decrypt end to end.
#[tokio::test]
async fn app_dead_host_reads_owner_backup_key_sealed_files_over_real_wire() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let _watch = seed_owner_files(&url, &bearer).await;

    let backup_key = BackupKey::derive(&OWNER_SECRET).to_bytes().to_vec();
    let (host, _root) = app_dead_host(&url, &bearer, backup_key).await;

    // ── ENUMERATE root (populate already ran at construction) ──
    let root = tokio::time::timeout(Duration::from_secs(30), host.enumerate(String::new()))
        .await
        .expect("enumerate must not hang (worker connect + populate is in-process/fast)")
        .expect("enumerate root");

    let proof = root
        .iter()
        .find(|i| i.rel == "proof.txt")
        .expect("root enumerates the populated placeholder proof.txt");
    assert!(!proof.is_dir, "proof.txt is a file");
    assert_eq!(
        proof.size_bytes,
        PROOF_CONTENT.len() as i64,
        "the placeholder carries the recorded size (from changes.list, no bytes)"
    );

    let docs = root
        .iter()
        .find(|i| i.is_dir)
        .expect("root enumerates the synthesized parent directory of docs/nested.txt");

    // ── ENUMERATE the synthesized subdirectory ──
    let children = host
        .enumerate(docs.rel.clone())
        .await
        .expect("enumerate the synthesized directory");
    let nested = children
        .iter()
        .find(|i| i.name == "nested.txt")
        .expect("the subdirectory enumerates nested.txt");
    assert!(!nested.is_dir);
    assert_eq!(nested.rel, format!("{}/nested.txt", docs.rel));

    // ── item(for:) resolves a single identifier ──
    let item = host
        .item("proof.txt".to_string())
        .await
        .expect("item(proof.txt)")
        .expect("proof.txt resolves to a concrete item");
    assert_eq!(item.rel, "proof.txt");

    // ── HYDRATE both files: fetchContents → chunk fetch → BackupKey decrypt ──
    let hydrated = host
        .fetch("proof.txt".to_string())
        .await
        .expect("fetch proof.txt");
    assert_eq!(
        hydrated.bytes, PROOF_CONTENT,
        "the app-dead host decrypts the owner-BackupKey-sealed bytes to the exact plaintext"
    );
    assert!(
        !hydrated.content_version.is_empty(),
        "a hydrated file carries a non-empty contentVersion"
    );

    let hydrated_nested = host
        .fetch(nested.rel.clone())
        .await
        .expect("fetch docs/nested.txt");
    assert_eq!(
        hydrated_nested.bytes, NESTED_CONTENT,
        "a nested file hydrates to its exact plaintext too"
    );
}

/// The custody is genuinely keyed, and post-S9-flip it covers the *names* too:
/// a host handed the **wrong** `BackupKey` can open neither the sealed path
/// labels nor the chunk seals, so it enumerates **nothing** — the ratified
/// `Omit` degrade (`file-sync.md` § Sealed names & paths; during expand the
/// plaintext columns still rested and a wrong-key host could list them) — and
/// a fetch of a known name fails closed rather than returning garbage or
/// plaintext. Guards against a regression that bypasses the owner seal on
/// either plane.
#[tokio::test]
async fn wrong_backup_key_enumerates_nothing_and_fails_closed_on_hydrate() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let _watch = seed_owner_files(&url, &bearer).await;

    // A BackupKey derived from a different seed — same owner-only binding shape,
    // wrong chunk-seal root.
    let wrong_key = BackupKey::derive(&[0x77; 32]).to_bytes().to_vec();
    let (host, _root) = app_dead_host(&url, &bearer, wrong_key).await;

    let root = tokio::time::timeout(Duration::from_secs(30), host.enumerate(String::new()))
        .await
        .expect("enumerate must not hang")
        .expect("enumerate root succeeds as a call even with nothing openable");
    assert!(
        root.is_empty(),
        "post-flip a wrong-key host opens no sealed path label, so it enumerates \
         nothing (the Omit degrade) — got {:?}",
        root.iter().map(|i| &i.rel).collect::<Vec<_>>()
    );

    let hydrate = host.fetch("proof.txt".to_string()).await;
    assert!(
        hydrate.is_err(),
        "a wrong BackupKey tracks no row and decrypts no chunk — hydrate of a \
         known name must fail closed"
    );
}

/// The M3 slice-1 write path at the same FFI seam: the app-dead host **writes**
/// through the production pipeline — bytes staged into its `watch_dir` (exactly
/// what the appex's `createItem`/`modifyItem` materialize step does) → `ingest`
/// seals + chunk-uploads + `changes.record`s, acking only on `recorded` — then a
/// **fresh** app-dead host (new state dir, same `BackupKey`) proves the write is
/// durable and readable over the wire: populate → enumerate → hydrate → decrypt.
/// Rename (delete+ingest pair) and delete (tombstone) each re-verify through
/// another fresh host, which also proves the populate fold applies the tombstones
/// (batch-latest per path), not just the creates.
#[tokio::test]
async fn app_dead_host_write_path_records_create_rename_delete_over_real_wire() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let backup_key = BackupKey::derive(&OWNER_SECRET).to_bytes().to_vec();
    let (writer, root) = app_dead_host(&url, &bearer, backup_key.clone()).await;

    const WRITTEN: &[u8] = b"written through the app-dead FP host \xe2\x9c\x8d\n";

    // ── CREATE: stage bytes at the rel (the appex materialize step), ingest. ──
    std::fs::write(root.join("new.txt"), WRITTEN).unwrap();
    let ack = tokio::time::timeout(
        Duration::from_secs(30),
        writer.ingest("new.txt".to_string()),
    )
    .await
    .expect("ingest must not hang")
    .expect("ingest new.txt");
    assert!(
        ack.acked,
        "the real nest must record the FP host's create (recorded-ack gate)"
    );
    // The staging root after a recorded upload (measured 2026-09-27 for the
    // phone-peer design, `p2p-shared-set-build.md` § Phone peers — design, decision 3): nothing
    // removes the staged body once the nest records it — neither the host's
    // ingest nor the appex's `createItem` — so iOS's kept root keeps every body
    // it ever uploaded, beside the OS's own hydrated copy. A demotion on record
    // (or on the reconcile's confirm) is the design's to build; when it lands,
    // this assertion flips.
    assert_eq!(
        std::fs::read(root.join("new.txt")).expect("the staged body is still in the staging root"),
        WRITTEN,
        "a recorded upload leaves its body in the staging root, byte-for-byte"
    );

    // A FRESH host (new state dir) sees the create purely via the nest.
    let (reader, _r) = app_dead_host(&url, &bearer, backup_key.clone()).await;
    let root_items = tokio::time::timeout(Duration::from_secs(30), reader.enumerate(String::new()))
        .await
        .expect("enumerate must not hang")
        .expect("fresh host enumerates after the create");
    assert!(
        root_items.iter().any(|i| i.rel == "new.txt"),
        "a fresh app-dead host populates the FP-host-written file from changes.list"
    );
    let hydrated = reader
        .fetch("new.txt".to_string())
        .await
        .expect("fresh host hydrates the FP-host-written file");
    assert_eq!(
        hydrated.bytes, WRITTEN,
        "the write round-trips: sealed by the FP host, decrypted by a fresh host"
    );

    // ── RENAME: stage bytes at the NEW rel (the appex does the same), rename. ──
    std::fs::write(root.join("renamed.txt"), WRITTEN).unwrap();
    let ack = writer
        .rename("new.txt".to_string(), "renamed.txt".to_string())
        .await
        .expect("rename new.txt -> renamed.txt");
    assert!(ack.acked, "the rename's ingest half must be recorded");

    let (reader2, _r2) = app_dead_host(&url, &bearer, backup_key.clone()).await;
    let after_rename = reader2
        .enumerate(String::new())
        .await
        .expect("fresh host enumerates after the rename");
    assert!(
        after_rename.iter().any(|i| i.rel == "renamed.txt"),
        "the renamed path enumerates on a fresh host"
    );
    assert!(
        !after_rename.iter().any(|i| i.rel == "new.txt"),
        "the old path's tombstone is applied by the populate fold — a rename must \
         not leave the stale rel behind"
    );

    // ── DELETE: record-first tombstone, then a fresh host sees an empty set. ──
    let ack = writer
        .delete("renamed.txt".to_string())
        .await
        .expect("delete renamed.txt");
    assert!(
        ack.acked,
        "the real nest must record the FP host's delete (recorded-ack gate, \
         same as create/modify/rename)"
    );

    // A retry of the same delete (the OS re-driving a delete whose ack was
    // lost) resolves vacuously — the row is gone, nothing is owed — and MUST
    // still ack, or the OS would spin on it forever.
    let retry = writer
        .delete("renamed.txt".to_string())
        .await
        .expect("retried delete");
    assert!(
        retry.acked,
        "a retried already-recorded delete acks vacuously"
    );

    let (reader3, _r3) = app_dead_host(&url, &bearer, backup_key).await;
    let after_delete = reader3
        .enumerate(String::new())
        .await
        .expect("fresh host enumerates after the delete");
    assert!(
        after_delete.is_empty(),
        "after the delete a fresh host enumerates nothing — the tombstone is the \
         durable record, not a local-row artifact (got: {:?})",
        after_delete
            .iter()
            .map(|i| i.rel.clone())
            .collect::<Vec<_>>()
    );
}

/// The owner `BackupKey` as fresh bytes (each `app_dead_host` call consumes a Vec).
fn backup_key_bytes() -> Vec<u8> {
    BackupKey::derive(&OWNER_SECRET).to_bytes().to_vec()
}

/// The bounded-memory hydrate (`fetch_to_path`) over the real wire: the owner
/// seals + records a multi-chunk file (large enough that the windowed walk runs
/// more than one fetch window), then the app-dead host writes its decrypted
/// plaintext straight to a destination path — the `fetchContents` shape where
/// no file crosses the FFI as one buffer (the iOS appex memory cap) — and the
/// bytes + returned `contentVersion` (the verified whole-file hash) both match.
#[tokio::test]
async fn app_dead_host_fetch_to_path_streams_a_multi_chunk_file() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    // ~12 MB of xorshift bytes: incompressible, deterministic, and with a 2 MB
    // average CDC chunk it yields ~6 chunks — at least two 4-chunk windows.
    let mut big = Vec::with_capacity(12 * 1024 * 1024);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    while big.len() < 12 * 1024 * 1024 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        big.extend_from_slice(&x.to_le_bytes());
    }
    const REL: &str = "big.bin";

    let watch = tempfile::tempdir().unwrap();
    std::fs::write(watch.path().join(REL), &big).unwrap();
    let (engine, nest_client) = owner_write_engine(&url, &bearer, watch.path().to_path_buf());
    nest_client
        .connect()
        .await
        .expect("writer nest_client must reach Connected");
    assert!(
        engine
            .upload_file(REL)
            .await
            .expect("upload big.bin")
            .recorded,
        "the nest records the owner's multi-chunk upload"
    );

    let (reader, _root) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("hydrated.bin");
    let version = tokio::time::timeout(
        Duration::from_secs(60),
        reader.fetch_to_path(REL.to_string(), dest.to_string_lossy().into_owned()),
    )
    .await
    .expect("fetch_to_path must not hang")
    .expect("fetch_to_path");

    let hydrated = std::fs::read(&dest).expect("dest written");
    assert_eq!(
        hydrated, big,
        "the windowed to-path walk lands the exact plaintext"
    );
    assert_eq!(
        version,
        fauna_core::data::ContentHash::of_raw(&big)
            .digest()
            .to_vec(),
        "the returned contentVersion is the verified whole-file hash"
    );
}

/// Lost-ack retry convergence over the real wire: a retried ingest of bytes
/// whose change record already landed (the OS re-drives a write whose ack was
/// lost — an appex crash between record and completion) must ack via the
/// recorded-head-proof skip, WITHOUT re-uploading. Before the skip carried the
/// proof check it always reported `recorded: false`, so such a retry could
/// never converge — the OS would re-drive the same write forever.
#[tokio::test]
async fn app_dead_host_retried_ingest_of_a_recorded_write_still_acks() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let (writer, root) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    const BYTES: &[u8] = b"written once, acked twice\n";
    std::fs::write(root.join("retry.txt"), BYTES).unwrap();

    let first = tokio::time::timeout(
        Duration::from_secs(30),
        writer.ingest("retry.txt".to_string()),
    )
    .await
    .expect("ingest must not hang")
    .expect("first ingest");
    assert!(first.acked, "the first ingest records and acks");

    // The retry: same staged bytes, same rel — the record already landed, so
    // the honest skip must ack (and must not claim a content change).
    let retry = tokio::time::timeout(
        Duration::from_secs(30),
        writer.ingest("retry.txt".to_string()),
    )
    .await
    .expect("retried ingest must not hang")
    .expect("retried ingest");
    assert!(
        retry.acked,
        "a retried ingest of an already-recorded head must ack (recorded-head-proof \
         skip) — an always-false skip would make the OS re-drive this write forever"
    );
    assert!(!retry.content_changed, "a vacuous retry moves no content");
}

/// The **M3 conflict arm** over the real wire: when the OS's `baseVersion` no
/// longer matches the row's current head — a concurrent writer advanced the nest
/// head while the OS edited an older version — the File Provider ingest routes
/// through the engine's shared auto-resolve (`file-sync.md` § Conflicts), NOT a
/// last-writer-wins clobber of the newer head.
///
/// Flow, all against the in-process nest:
/// 1. the owner records `race.bin` = v1; a host that saw v1 yields the
///    `contentVersion` the OS would carry as its `baseVersion`;
/// 2. the owner records `race.bin` = v2 (the concurrent remote edit), stamped
///    "now" — so v2 is the *latest writer*;
/// 3. a fresh host (which populated v2) stages a v1-based local edit (v3) with an
///    intentionally OLD mtime and calls `ingest_with_base` with the stale v1 base.
///
/// The FP host holds no cached merge base and `race.bin` is binary, so resolution
/// is latest-writer-wins; v2 (recorded "now") beats v3 (mtime 1970). A fresh reader
/// therefore hydrates **v2**, proving the stale v1-based local write did NOT clobber
/// the newer head — a blind last-writer ingest would have made v3 the head. The
/// resolved report landing over the real wire is what lets the write `ack`; the
/// losing v3 is retained nest-side by that report's transaction (the no-data-loss
/// invariant — the retention + review-list wire is separately tier_3-proven in
/// `test_text_merge.py`).
#[tokio::test]
async fn app_dead_host_stale_base_write_auto_resolves_over_real_wire() {
    use std::time::{Duration, UNIX_EPOCH};

    const V1: &[u8] = b"race v1 original\n";
    const V2_REMOTE: &[u8] = b"race v2 recorded by the owner's other device\n";
    const V3_LOCAL: &[u8] = b"race v3 edited locally in the FP host from a v1 base\n";
    const REL: &str = "race.bin";

    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    // ── owner records v1 ──
    let owner_watch = tempfile::tempdir().unwrap();
    std::fs::write(owner_watch.path().join(REL), V1).unwrap();
    let (owner_engine, owner_nest) =
        owner_write_engine(&url, &bearer, owner_watch.path().to_path_buf());
    owner_nest
        .connect()
        .await
        .expect("owner nest_client must reach Connected");
    assert!(
        owner_engine
            .upload_file(REL)
            .await
            .expect("upload v1")
            .recorded,
        "the nest records the owner's v1"
    );

    // ── a host that saw v1: capture the base contentVersion the OS would hold ──
    let (host_a, _root_a) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let base_v1 = tokio::time::timeout(Duration::from_secs(30), host_a.item(REL.to_string()))
        .await
        .expect("item must not hang (worker connect + populate is in-process/fast)")
        .expect("item(race.bin)")
        .expect("race.bin resolves on the v1 host")
        .content_version;
    assert!(
        !base_v1.is_empty(),
        "v1 carries a contentVersion to use as the OS's baseVersion"
    );

    // ── owner records v2 (the concurrent remote edit), stamped "now" ⇒ latest writer ──
    std::fs::write(owner_watch.path().join(REL), V2_REMOTE).unwrap();
    assert!(
        owner_engine
            .upload_file(REL)
            .await
            .expect("upload v2")
            .recorded,
        "the nest records the owner's concurrent v2"
    );

    // ── a fresh host that populated v2 stages a v1-based local edit and ingests it ──
    let (writer_b, root_b) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let staged = root_b.join(REL);
    std::fs::write(&staged, V3_LOCAL).unwrap();
    // Force v3's mtime OLD so v2 (recorded "now") is unambiguously the latest writer.
    std::fs::File::open(&staged)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1000))
        .unwrap();

    let ack = tokio::time::timeout(
        Duration::from_secs(30),
        writer_b.ingest_with_base(REL.to_string(), base_v1),
    )
    .await
    .expect("ingest_with_base must not hang")
    .expect("ingest_with_base");
    assert!(
        ack.acked,
        "the stale-base write resolved and its resolved report landed on the real \
         nest (an unresolved fallback would leave it un-acked)"
    );
    assert!(
        ack.content_changed,
        "the incoming v2 won, so the row was re-pointed at content the OS does not \
         hold — the ack must carry content_changed so `modifyItem` returns \
         shouldFetchContent: true (else the OS associates its loser v3 bytes with \
         the winning version and the next edit fast-forwards over the resolve)"
    );

    // ── a fresh reader hydrates the WINNER: v2, not the stale local v3 ──
    let (reader, _root_r) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let hydrated = reader
        .fetch(REL.to_string())
        .await
        .expect("fetch race.bin on a fresh reader");
    assert_eq!(
        hydrated.bytes, V2_REMOTE,
        "the concurrent v2 is the latest writer and wins — the stale v1-based local \
         write did NOT clobber the newer nest head (a blind last-writer ingest would \
         have made v3 the head)"
    );
    assert_ne!(
        hydrated.bytes, V3_LOCAL,
        "the stale local edit is not the head"
    );
}

/// The **live-refresh tick** primitive: the app-dead FP host runs no watcher/loop,
/// so a remote change recorded *after* it was constructed is invisible until
/// `fileproviderd` reconstructs the extension — unless the appex drives an
/// in-session `refresh` (the tick the goal doc names, `file-sync.md` § Apple File
/// Provider binding). This proves it headlessly: a no-op refresh reports nothing to
/// signal; a new remote file appears only after a refresh (never on a bare
/// enumerate); the refresh reports `changed` so the appex knows to signal.
#[tokio::test]
async fn app_dead_host_refresh_picks_up_a_post_construction_remote_change() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let owner_watch = tempfile::tempdir().unwrap();
    std::fs::write(owner_watch.path().join("first.txt"), b"first\n").unwrap();
    let (owner_engine, owner_nest) =
        owner_write_engine(&url, &bearer, owner_watch.path().to_path_buf());
    owner_nest
        .connect()
        .await
        .expect("owner nest_client must reach Connected");
    assert!(
        owner_engine
            .upload_file("first.txt")
            .await
            .expect("upload first")
            .recorded
    );

    // The host populates `first.txt` at construction.
    let (host, _root) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let before = tokio::time::timeout(Duration::from_secs(30), host.enumerate(String::new()))
        .await
        .expect("enumerate must not hang")
        .expect("enumerate root");
    assert!(before.iter().any(|i| i.rel == "first.txt"));
    assert!(!before.iter().any(|i| i.rel == "second.txt"));

    // A refresh with nothing new on the nest reports no change — the appex would
    // not signal the enumerator.
    assert!(
        !host.refresh().await.expect("no-op refresh"),
        "a refresh with no new remote change reports nothing to signal"
    );

    // The owner records a NEW file after the host was constructed.
    std::fs::write(owner_watch.path().join("second.txt"), b"second\n").unwrap();
    assert!(
        owner_engine
            .upload_file("second.txt")
            .await
            .expect("upload second")
            .recorded
    );

    // The app-dead host runs no loop, so the new file is invisible on a bare
    // enumerate until a refresh folds it in.
    let still = host
        .enumerate(String::new())
        .await
        .expect("enumerate pre-refresh");
    assert!(
        !still.iter().any(|i| i.rel == "second.txt"),
        "the app-dead host runs no loop — the new remote file is invisible until refresh"
    );

    assert!(
        host.refresh().await.expect("refresh"),
        "the refresh folded the new remote change and reports it should signal"
    );
    let after = host
        .enumerate(String::new())
        .await
        .expect("enumerate post-refresh");
    assert!(
        after.iter().any(|i| i.rel == "second.txt"),
        "the new remote file appears after an in-session refresh (no domain re-add)"
    );
}

/// The refresh tick must stay quiet for a set containing a 0-byte file. The fold
/// stamps the empty-content `local_hash` once at populate (so a later OS
/// materialization isn't misread as a local edit), but an unchanged head must
/// then rewrite — and count — nothing: before this pin, every pass re-wrote the
/// 0-byte row and reported it in `fold.recorded`, so `refresh` returned `true`
/// every tick forever and the appex signalled + re-enumerated on every interval.
#[tokio::test]
async fn refresh_with_an_unchanged_zero_byte_file_reports_nothing() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let owner_watch = tempfile::tempdir().unwrap();
    std::fs::write(owner_watch.path().join("empty.txt"), b"").unwrap();
    let (owner_engine, owner_nest) =
        owner_write_engine(&url, &bearer, owner_watch.path().to_path_buf());
    owner_nest
        .connect()
        .await
        .expect("owner nest_client must reach Connected");
    assert!(
        owner_engine
            .upload_file("empty.txt")
            .await
            .expect("upload empty")
            .recorded
    );

    // Construction populates the 0-byte placeholder, stamping its empty-content
    // identity; every later refresh with an unchanged head is then a no-op.
    let (host, _root) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let listed = tokio::time::timeout(Duration::from_secs(30), host.enumerate(String::new()))
        .await
        .expect("enumerate must not hang")
        .expect("enumerate root");
    assert!(listed.iter().any(|i| i.rel == "empty.txt"));
    for pass in 1..=2 {
        assert!(
            !host.refresh().await.expect("refresh"),
            "refresh pass {pass}: an unchanged 0-byte row must not re-count as newly folded"
        );
    }
}

/// The refresh tick's harder half: a remote **modify** of a file this host had
/// **hydrated**. The FP host owns no cfapi cache to dehydrate, so `refresh` applies
/// the stale-hydrated row by re-pointing it at the moved head as an un-hydrated
/// placeholder (proof cleared) — its `contentVersion` changes, so the OS re-fetches
/// the current content. Proven headlessly: after the modify, the item's version
/// moves and a re-fetch returns v2, not the stale v1 the host had on disk.
#[tokio::test]
async fn app_dead_host_refresh_repoints_a_remotely_modified_hydrated_file() {
    const V1: &[u8] = b"doc v1 hydrated by the FP host\n";
    const V2: &[u8] = b"doc v2 recorded remotely after hydration\n";
    const REL: &str = "doc.txt";

    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;

    let owner_watch = tempfile::tempdir().unwrap();
    std::fs::write(owner_watch.path().join(REL), V1).unwrap();
    let (owner_engine, owner_nest) =
        owner_write_engine(&url, &bearer, owner_watch.path().to_path_buf());
    owner_nest
        .connect()
        .await
        .expect("owner nest_client must reach Connected");
    assert!(
        owner_engine
            .upload_file(REL)
            .await
            .expect("upload v1")
            .recorded
    );

    // The host populates and HYDRATES v1 (row → Synced, recorded = of_raw(v1)).
    let (host, _root) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let hydrated_v1 = tokio::time::timeout(Duration::from_secs(30), host.fetch(REL.to_string()))
        .await
        .expect("fetch must not hang")
        .expect("hydrate v1");
    assert_eq!(hydrated_v1.bytes, V1);
    let version_v1 = host
        .item(REL.to_string())
        .await
        .expect("item v1")
        .expect("doc.txt resolves")
        .content_version;

    // The owner modifies the file remotely.
    std::fs::write(owner_watch.path().join(REL), V2).unwrap();
    assert!(
        owner_engine
            .upload_file(REL)
            .await
            .expect("upload v2")
            .recorded
    );

    // Refresh detects the moved head over the hydrated row and re-points it.
    assert!(
        host.refresh().await.expect("refresh"),
        "a remote modify of a hydrated file is a change to signal"
    );
    let version_v2 = host
        .item(REL.to_string())
        .await
        .expect("item v2")
        .expect("doc.txt still resolves")
        .content_version;
    assert_ne!(
        version_v1, version_v2,
        "the re-pointed row's contentVersion moved to the new head — the OS will re-fetch"
    );

    // A re-fetch returns v2, not the stale v1 the host had hydrated.
    let hydrated_v2 = host.fetch(REL.to_string()).await.expect("re-fetch");
    assert_eq!(
        hydrated_v2.bytes, V2,
        "after the refresh re-point, a fetch hydrates the current remote content (v2)"
    );
}

// ---------------------------------------------------------------------------
// Office atomic-save-dance — the always-resident watcher path ("attack the needs-a-human claim" — a plain
// process reproduces the save sequence headlessly, no Office install).
// ---------------------------------------------------------------------------

/// Simulates an Office-style resave against a real watched folder: create the
/// `~$` lock file, write the new content to a `.tmp` sibling, delete the
/// original, rename the temp over it, then delete the lock file — all within
/// one debounce window (`always_resident::DEBOUNCE_DELAY`). The
/// always-resident watcher (linux, windows always-resident, and the upload
/// half of windows on-demand all share this one path — `always_resident.rs`
/// module doc) must coalesce the whole burst into exactly one upload of the
/// real target path, and the lock/tmp litter must never reach the nest at all
/// (`file-sync.md` § Built-in default ignores, `libs/fauna-sync-engine/src/ignore.rs`).
#[tokio::test]
async fn office_atomic_save_dance_coalesces_to_one_clean_version_watcher_path() {
    use fauna_sync_engine::always_resident::{LocalWrites, apply_local_write};

    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let watch = tempfile::tempdir().unwrap();

    let (engine, nest_client) = owner_write_engine(&url, &bearer, watch.path().to_path_buf());
    nest_client
        .connect()
        .await
        .expect("writer nest_client must reach Connected (WS auth handshake)");

    const REL: &str = "report.docx";
    const V1: &[u8] = b"draft one\n";
    const V2: &[u8] = b"draft two, after the save dance\n";

    // The document already exists and is synced (v1) before the dance starts —
    // this is a resave, the realistic case for an existing shared document.
    std::fs::write(watch.path().join(REL), V1).unwrap();
    assert!(
        engine.upload_file(REL).await.expect("seed v1").recorded,
        "v1 must land before the dance starts"
    );

    // Start the real watcher only now, so the burst below is the only thing
    // it ever observes.
    let mut local = LocalWrites::start(watch.path()).expect("start real FsWatcher");

    // Readiness barrier (convention 14: a deadline poll on observable state,
    // never a blind sleep): FSEvents (macOS) goes live asynchronously, so a
    // dance performed before the stream is scheduled is silently unobserved —
    // green on inotify (Linux, synchronous registration), red on macOS. Write a
    // sentinel and wait until the watcher surfaces it; only then is the watch
    // provably live. The sentinel is observed but never applied (no upload),
    // and its removal below resolves vacuously in the dance drain.
    const SENTINEL: &str = "watch-ready.sentinel";
    std::fs::write(watch.path().join(SENTINEL), b"x").unwrap();
    let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "watcher never surfaced the readiness sentinel — watch is not live"
        );
        let Ok(write) = tokio::time::timeout(Duration::from_secs(1), local.next(&engine)).await
        else {
            continue;
        };
        if matches!(
            &write,
            fauna_sync_engine::always_resident::LocalWrite::Upload(rels)
                if rels.iter().any(|r| r == SENTINEL)
        ) {
            break;
        }
    }
    std::fs::remove_file(watch.path().join(SENTINEL)).unwrap();

    // ── The dance, as a plain process — no Office, no app, just the file
    // operations an editor's save makes. `rename` is the atomic *replace*
    // (POSIX semantics: the destination is swapped in one step) — there is no
    // separate delete of the old bytes; a real explicit delete-then-create
    // would be a distinct, non-atomic dance and correctly produce two change
    // records, not one. ──
    std::fs::write(watch.path().join("~$report.docx"), b"").unwrap(); // owner-lock
    std::fs::write(watch.path().join("report.docx.tmp"), V2).unwrap(); // atomic-save intermediate
    std::fs::rename(watch.path().join("report.docx.tmp"), watch.path().join(REL)).unwrap(); // atomic replace — no separate delete
    std::fs::remove_file(watch.path().join("~$report.docx")).unwrap(); // lock released

    // Drain the watcher until it reports the coalesced upload — bounded, the
    // harness self-terminates rather than trusting a human to notice a hang
    // (testing.md § point 9).
    let mut recorded = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while recorded.is_empty() && tokio::time::Instant::now() < deadline {
        let Ok(write) = tokio::time::timeout(Duration::from_secs(1), local.next(&engine)).await
        else {
            continue;
        };
        let applied = apply_local_write(&engine, FOLDER, write).await;
        recorded.extend(applied.recorded);
        if !applied.continue_watching {
            break;
        }
    }

    assert_eq!(
        recorded,
        vec![REL.to_string()],
        "the whole save-dance must coalesce into exactly one upload of {REL}, got {recorded:?}"
    );

    // ── No litter: the lock file and the .tmp intermediate never reached the
    // nest, and report.docx now carries exactly two versions (v1 + the
    // dance's one coalesced v2) — no partial/intermediate versions from the
    // dance's individual steps. ──
    let changes = fauna_client_sync::SyncClient::new(nest_client.clone())
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;

    // Raw-wire read, post-S9-flip: no plaintext `path` rides `changes.list`,
    // so the litter/coalesce asserts key on `path_hash` (file-sync.md § Sealed
    // names & paths — hash-addressed state, the ratified rework direction).
    let hash_of = |p: &str| hex::encode(fauna_core::sync::path_hash(p));
    let report_changes: Vec<_> = changes
        .iter()
        .filter(|c| c.path_hash == hash_of(REL))
        .collect();
    assert_eq!(
        report_changes.len(),
        2,
        "report.docx must show exactly v1 + the one coalesced resave — no litter \
         versions from the dance's intermediate steps, got {report_changes:#?}"
    );

    for litter in ["~$report.docx", "report.docx.tmp"] {
        assert!(
            !changes.iter().any(|c| c.path_hash == hash_of(litter)),
            "{litter} must never reach the nest as a change record (built-in \
             default ignores, file-sync.md)"
        );
    }

    // Final content is the dance's new bytes, not the pre-dance v1.
    let bytes = engine
        .download_file_bytes(REL)
        .await
        .expect("download the recorded head");
    assert_eq!(
        bytes, V2,
        "the recorded head must be the dance's final content"
    );
}

/// The **File Provider write-path twin** of the watcher-path dance above: on a
/// macOS/iOS on-demand folder Office's lock file and the `.tmp` intermediate
/// arrive as ordinary `createItem`/`modifyItem` callbacks (`host.ingest`), not
/// filesystem events — the watcher gate never sees them. The ignore gate in the
/// shared `provider_face` write cores must answer `excluded` (the appex maps it
/// to `NSFileProviderError.excludedFromSync`) without sealing, uploading, or
/// recording anything, while the real document ingests normally
/// (`file-sync.md` § Built-in default ignores — Apple FP write path).
#[tokio::test]
async fn office_litter_via_provider_write_path_is_excluded_from_sync() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let (writer, root) = app_dead_host(&url, &bearer, backup_key_bytes()).await;

    const REAL: &[u8] = b"the actual document\n";

    // The litter classes, each as the OS would deliver it: the Office
    // owner-lock, the atomic-save intermediate, a dotfile (the FP path never
    // runs the scanner, so the categorical dotfile exclusion must hold here
    // too), and NESTED litter (the any-depth gitignore semantics — Office
    // writes its lock next to the document, wherever that is).
    std::fs::create_dir_all(root.join("docs")).unwrap();
    for litter in [
        "~$report.docx",
        "report.docx.tmp",
        ".DS_Store",
        "docs/~$nested.docx",
    ] {
        assert!(
            writer
                .is_ignored(litter.to_string())
                .await
                .expect("is_ignored"),
            "{litter} must report ignored over the FFI seam (createItem asks up front)"
        );
        std::fs::write(root.join(litter), b"litter bytes").unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(30), writer.ingest(litter.to_string()))
            .await
            .expect("ingest must not hang")
            .expect("ingest litter");
        assert!(
            ack.excluded && !ack.acked,
            "{litter} must answer excluded (never acked, never ingested)"
        );
    }

    // The real document ingests + records through the same door.
    assert!(
        !writer
            .is_ignored("report.docx".to_string())
            .await
            .expect("is_ignored"),
        "the real document is not ignored"
    );
    std::fs::write(root.join("report.docx"), REAL).unwrap();
    let ack = tokio::time::timeout(
        Duration::from_secs(30),
        writer.ingest("report.docx".to_string()),
    )
    .await
    .expect("ingest must not hang")
    .expect("ingest report.docx");
    assert!(ack.acked && !ack.excluded);

    // A fresh app-dead host (new state dir) populates purely from the nest's
    // change records: only the real document exists — none of the litter ever
    // became a change record.
    let (reader, _r) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let rels: Vec<String> =
        tokio::time::timeout(Duration::from_secs(30), reader.enumerate(String::new()))
            .await
            .expect("enumerate must not hang")
            .expect("fresh host enumerates")
            .into_iter()
            .map(|i| i.rel)
            .collect();
    assert_eq!(
        rels,
        vec!["report.docx".to_string()],
        "the nest must hold exactly the real document — no litter change records"
    );

    // Rename INTO an ignored name takes the file out of the set: the old
    // path's tombstone is real (a fresh host sees an empty set), the new name
    // is never ingested, and the ack says excluded so the OS keeps the local
    // file without retrying.
    std::fs::write(root.join("report.tmp"), REAL).unwrap();
    let ack = writer
        .rename("report.docx".to_string(), "report.tmp".to_string())
        .await
        .expect("rename to ignored name");
    assert!(ack.excluded && !ack.acked);

    let (reader2, _r2) = app_dead_host(&url, &bearer, backup_key_bytes()).await;
    let after = reader2
        .enumerate(String::new())
        .await
        .expect("fresh host enumerates after rename-to-ignored");
    assert!(
        after.is_empty(),
        "renaming into an ignored name tombstones the old path and records \
         nothing for the new one (got: {:?})",
        after.iter().map(|i| i.rel.clone()).collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────────────
// Part (D) — an owner with no resident replica shares a set
// (`mls-group-key-material.md` § M2 → *Pre-bind re-seal migration*;
// `on-demand-files.md` § Shared sets on a capability host, decision 4)
// ─────────────────────────────────────────────────────────────────────

/// An on-demand (placeholder-only) engine over `FOLDER`, bound to `group`
/// with `content_keys`, connected to the real nest, reading under
/// [`SET_NONCE`] as the set's owner. `backup_key` makes it the owner's (which
/// signs its re-seal records with the owner's identity under [`SET_NONCE`]);
/// `None` makes it a member-keyed reader holding only the set's content keys.
fn bound_on_demand_engine(
    url: &str,
    bearer: &str,
    watch_path: std::path::PathBuf,
    device_id: [u8; 32],
    group: Vec<u8>,
    content_keys: fauna_core::folder_keys::FolderContentKeys,
    backup_key: Option<BackupKey>,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine_client, nest_client) =
        fauna_nest::test_support::sync_engine_auth_client(url, bearer, OWNER_SECRET, &device_id);
    let owner_writes = backup_key.is_some();
    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        device_id,
        None, // mls
        None, // epoch_secret
        backup_key.map(Into::into),
        Some(group),
        Some(content_keys),
        ConflictPolicy::Auto,
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(ActorKeypair::from_secret(OWNER_SECRET).actor_id().0),
        ..Default::default()
    });
    if owner_writes {
        engine.set_change_signer(
            Some(common::direct_signer(&ActorKeypair::from_secret(
                OWNER_SECRET,
            ))),
            Some(SET_NONCE),
        );
    }
    (engine, nest_client)
}

/// The day an owner whose only replica is on-demand shares a set holding
/// pre-share files, over the real wire — the mechanism of part (D), whose
/// enabler is the owner's signature (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records*, ruling (5)):
///
/// 1. the owner's on-demand engine, holding placeholders only, re-seals every
///    pre-bind file its account SIGNED under the set's nonce **from the nest**
///    under the set's generation without hydrating any of them, its change
///    records land, and a second pass is a no-op (the per-row stamp terminates
///    it);
/// 2. an unsigned pre-bind record is refused by the nest (`signature_required`),
///    so it never becomes a placeholder, is never offered the owner root and
///    never re-sealed to the members;
/// 3. a reader keyed as a member — the generation only, no `BackupKey`, not
///    the owner — populates from the nest and opens both signed files.
#[tokio::test]
async fn an_on_demand_owner_shares_a_set_and_its_prebind_files_reach_members() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    // `start_test_nest_with_db` stores `SET_NONCE` on the set, as a create does.
    let (url, bearer, db) = start_test_nest_with_db(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    // The attempted plant: a pre-bind record nobody signed. The nest refuses
    // it (`signature_required`), so it never becomes a row at all.
    let planted_watch = tempfile::tempdir().unwrap();
    std::fs::write(planted_watch.path().join("planted.txt"), b"not the owner's").unwrap();
    let (planter, planter_nest) =
        owner_write_engine(&url, &bearer, planted_watch.path().to_path_buf());
    planter.set_change_signer(None, None);
    planter_nest.connect().await.expect("planter connects");
    assert!(
        !planter
            .upload_file("planted.txt")
            .await
            .map(|o| o.recorded)
            .unwrap_or(false),
        "the nest refuses an unsigned record (signature_required)"
    );

    // The share: the set becomes bound on the nest.
    let group = vec![0x77u8; 32];
    assert!(
        db.set_folder_mls_group(FOLDER, &owner, Some(&group))
            .await
            .unwrap(),
        "the owner's set is bound"
    );
    let backup_key = BackupKey::derive(&OWNER_SECRET);

    // ── 1. The owner's on-demand engine re-seals from the nest ──
    let content_keys = fauna_core::folder_keys::FolderContentKeys::genesis([0x78u8; 32], 1_000);
    let od_watch = tempfile::tempdir().unwrap();
    let (od, od_nest) = bound_on_demand_engine(
        &url,
        &bearer,
        od_watch.path().to_path_buf(),
        [0x0Bu8; 32],
        group.clone(),
        content_keys.clone(),
        Some(backup_key.clone()),
    );
    od_nest
        .connect()
        .await
        .expect("owner on-demand engine connects");
    let fold = od
        .populate_placeholders_from_nest()
        .await
        .expect("populate placeholders");
    assert_eq!(
        fold.recorded, 2,
        "every signed pre-bind file is a placeholder here (the refused plant \
         left no row)"
    );
    assert_eq!(
        od.reseal_pending_under_current()
            .await
            .expect("the pass re-seals from the nest and its records land"),
        2
    );
    for rel in ["proof.txt", "docs/nested.txt"] {
        assert!(
            !od_watch.path().join(rel).exists(),
            "the re-seal never hydrates {rel}"
        );
        let entry = od.db().get_entry(rel).unwrap().expect("row kept");
        assert_eq!(
            entry.content_key_version,
            Some(content_keys.current_version()),
            "{rel}'s row is stamped at the set's generation"
        );
    }
    assert!(
        od.db().get_entry("planted.txt").unwrap().is_none(),
        "the refused unsigned plant is never a row, so never re-sealed to the \
         set's members"
    );
    od.download_file_bytes("planted.txt")
        .await
        .expect_err("nor opened under the owner root");
    assert_eq!(
        od.reseal_pending_under_current()
            .await
            .expect("second pass"),
        0,
        "the per-row stamp terminates the pass"
    );

    // ── 2. A member-keyed reader opens both ──
    let reader_watch = tempfile::tempdir().unwrap();
    let (reader, reader_nest) = bound_on_demand_engine(
        &url,
        &bearer,
        reader_watch.path().to_path_buf(),
        [0x0Cu8; 32],
        group,
        content_keys,
        None,
    );
    reader_nest.connect().await.expect("reader connects");
    reader
        .populate_placeholders_from_nest()
        .await
        .expect("reader populates");
    for (rel, expected) in [
        ("proof.txt", PROOF_CONTENT),
        ("docs/nested.txt", NESTED_CONTENT),
    ] {
        let bytes = reader
            .download_file_bytes(rel)
            .await
            .unwrap_or_else(|e| panic!("a member-keyed reader opens re-sealed {rel}: {e:#}"));
        assert_eq!(bytes, expected);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared sets on a capability host (`docs/goal/behavior/on-demand-files.md`
// § Shared sets on a capability host, decisions 1 and 2, *Headless proof*).
//
// A capability host holds the `BackupKey`, a bearer and the machine principal,
// never the seed and never MLS state. Decision 1′: it loads a bound set's
// content keys from the account's plane custody itself, through a throwaway
// fleet replica keyed by the principal's wrap. Decision 2: it re-reads custody at
// every build, at a refresh when the row's binding / access / content-key floor
// moved, and before a seal when the floor is ahead of the generation it holds —
// and when the generation is still missing it fails closed per operation and
// never seals under the older one. The floor binds an owner's host as it binds a
// member's (the nest's own floor check exempts owner devices).
//
// The harness is a real nest serving the whole router over TCP (the host's WS
// control plane and HTTP chunk plane both reach it), an owner A and a WRITER
// member B joined through a real MLS group, the set bound by the production
// `FoldersAuthor::bind_set`, and B's custody-ingest leg modeled by opening the
// real envelope and merging it into B's own custody. Each app's custody lands
// on its account's plane through the production door (`PlaneMachine`), which is
// all a host reads. A rotation is modeled as
// the owner's custody write plus the floor-carrying envelope publish — the two
// nest-visible effects of the rotating device; the host never opens an envelope,
// so what the envelope seals is not what these flows test.
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use fauna_client::NestClient;
use fauna_client_folders::FoldersClient;
use fauna_client_folders::MemoryFolderKeyStore;
use fauna_client_folders::custody;
use fauna_client_folders::orchestration::{CreatedGroup, FolderGroupCrypto, FoldersAuthor};
use fauna_client_sync::SyncClient as CtlSyncClient;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::ChannelId;
use fauna_protocol::conversations::{WelcomeDeliverRequest, WelcomeKind};
use fauna_protocol::folders::{
    ContentKeyGetRequest, ContentKeyPutRequest, FolderShareRequest, MemberEvictRequest,
};

/// The owner of the shared set (distinct from the owner-only fixtures above).
const A_SECRET: [u8; 32] = [0xA1; 32];
/// The writer member the set is shared with.
const B_SECRET: [u8; 32] = [0xB2; 32];
/// A's seed-holding, identity-holding device — the one that writes and rotates.
const A_WRITER_DEVICE: [u8; 32] = [0x1A; 32];
/// A's capability host (the phone's File Provider / provider process).
const A_HOST_DEVICE: [u8; 32] = [0x2A; 32];
/// B's capability host.
const B_HOST_DEVICE: [u8; 32] = [0x2B; 32];
/// A second host B builds after the removal.
const B_LATE_HOST_DEVICE: [u8; 32] = [0x3B; 32];
/// A's machine principal (enrolled in [`shared_set`]) — the writer key A's
/// app provisions its hosts.
const A_PRINCIPAL_SECRET: [u8; 32] = [0xA3; 32];
/// B's machine principal, likewise.
const B_PRINCIPAL_SECRET: [u8; 32] = [0xB3; 32];
const SHARED: &str = "shared";

fn actor_of(secret: [u8; 32]) -> [u8; 32] {
    ActorKeypair::from_secret(secret).actor_id().0
}

fn wire<T: serde::Serialize>(req: &T) -> bytes::Bytes {
    fauna_protocol::encode_canonical(req).unwrap()
}

/// A bound, shared folder on a real served nest: A owns it, B holds a
/// writer grant and gen-1 in B's own custody.
struct SharedSet {
    url: String,
    state: Arc<AppState>,
    folder_ref: FolderRef,
    channel_id: [u8; 32],
    /// The set's nonce (minted into A's custody at create, received by B
    /// inside the envelope) — every record into the set is signed under it.
    nonce: [u8; 32],
    raw_group_id: Vec<u8>,
    a_nest: Arc<NestClient>,
    a_bearer: String,
    b_bearer: String,
    /// Each account app's working custody — what its custody writers hold —
    /// and its machine on the account plane, where that custody rests for the
    /// capability hosts.
    a_custody: Arc<MemoryFolderKeyStore>,
    b_custody: Arc<MemoryFolderKeyStore>,
    a_plane: PlaneMachine,
    b_plane: PlaneMachine,
}

async fn shared_set() -> SharedSet {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());

    let a = actor_of(A_SECRET);
    let b = actor_of(B_SECRET);
    db.create_user(&a, "free", "owner").await.unwrap();
    db.create_user(&b, "free", "member").await.unwrap();

    let token_store = Arc::new(TokenStore::new());
    let a_bearer = token_store
        .insert(ActorKeypair::from_secret(A_SECRET).actor_id(), 3600)
        .await;
    let b_bearer = token_store
        .insert(ActorKeypair::from_secret(B_SECRET).actor_id(), 3600)
        .await;

    let state = Arc::new(AppState {
        nest_identity: Arc::new(fauna_nest::nest_identity::NestIdentity::generate()),
        backup_service: Some(backup_svc),
        rpc_router: Arc::new({
            let mut r = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut r);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut r);
            fauna_nest::sync_handlers::register_sync_handlers(&mut r);
            fauna_nest::folder_handlers::register_folders_handlers(&mut r);
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut r);
            // The generation-escrow doors each account's first plane custody
            // write mints through.
            fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut r);
            r.build()
        }),
        nest_signing_key: Some(deployment_key()),
        auth: fauna_nest::state::AuthState {
            token_store,
            registration: fauna_nest::routes::RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(false)),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let url = format!("http://{authority}");

    let a_nest = common::connected_client(&url, ActorKeypair::from_secret(A_SECRET)).await;
    let b_nest = common::connected_client(&url, ActorKeypair::from_secret(B_SECRET)).await;

    // A creates the set the production way: its nonce minted into A's custody
    // first, then `fauna.folders.create` carrying it.
    let a_custody = Arc::new(MemoryFolderKeyStore::default());
    let b_custody = Arc::new(MemoryFolderKeyStore::default());
    fauna_client_folders::set_lifecycle::create_set(
        &FoldersClient::new(a_nest.clone()),
        a_custody.as_ref(),
        fauna_protocol::folders::FolderCreateRequest {
            name: SHARED.into(),
            ..Default::default()
        },
    )
    .await
    .expect("the set creates, its nonce in custody");
    // Each account's machine principal — what the app provisions its hosts.
    register_principal(
        &a_nest,
        &ActorKeypair::from_secret(A_SECRET),
        &ActorKeypair::from_secret(A_PRINCIPAL_SECRET),
    )
    .await;
    register_principal(
        &b_nest,
        &ActorKeypair::from_secret(B_SECRET),
        &ActorKeypair::from_secret(B_PRINCIPAL_SECRET),
    )
    .await;
    // …and each principal enrolled on its account's plane, where the apps
    // write the custody their capability hosts read.
    let a_plane = PlaneMachine::first(&url, A_SECRET, A_PRINCIPAL_SECRET).await;
    let b_plane = PlaneMachine::first(&url, B_SECRET, B_PRINCIPAL_SECRET).await;

    // A + B form a real MLS group; B joins from the Welcome.
    let a_mls = Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_mls = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let b_kp = b_mls.generate_key_packages_bytes(1).unwrap();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_mls, &b_kp).unwrap();
    let channel_id = created.channel_id;
    assert_eq!(
        b_mls.join_from_welcome_bytes(&created.welcome).unwrap().0,
        channel_id
    );

    // A shares the set with B as a WRITER and binds it (genesis content key into
    // A's custody + the floor-carrying envelope publish).
    FoldersClient::new(a_nest.clone())
        .share(FolderShareRequest {
            name: SHARED.into(),
            group_id: hex::encode(&created.raw_group_id),
            member_actor_id: Some(hex::encode(b)),
            access: Some("writer".into()),
            ..Default::default()
        })
        .await
        .expect("share (writer grant)");
    FoldersAuthor::new(
        FoldersClient::new(a_nest.clone()),
        ActorKeypair::from_secret(A_SECRET),
        a_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_mls.clone(),
    )
    .bind_set(SHARED, channel_id)
    .await
    .expect("bind_set");
    a_plane.write_custody(&a_custody.snapshot()).await;

    state.db.set_inbox_mode(&b, "open").await.unwrap();
    common::dispatch(
        state.rpc_router.as_ref(),
        state.clone(),
        a,
        "fauna.conversations.welcome.deliver",
        wire(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(b),
            channel_id: hex::encode(channel_id),
            welcome_bytes: created.welcome.clone(),
            kind: WelcomeKind::Group {
                group_id: hex::encode(&created.raw_group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver");

    // B's custody-ingest leg: open the live envelope with B's own group state and
    // merge the generations into B's plane custody — the custody B's host reads.
    let reply = FoldersClient::new(b_nest.clone())
        .content_key_get(ContentKeyGetRequest {
            name: SHARED.into(),
            ..Default::default()
        })
        .await
        .expect("content_key.get (member)");
    // The nest stores signature-plus-ciphertext (`folder_envelope_sig`): verify
    // the owner's signature, then open what it signed.
    let blob = hex::decode(reply.sealed.trim()).unwrap();
    let sealed = fauna_protocol::folder_envelope_sig::verify(&blob, &channel_id)
        .expect("the stored envelope is owner-signed")
        .sealed;
    let payload = b_mls
        .open_content_key_envelope_payload(&ChannelId(channel_id), &sealed)
        .expect("B opens the envelope");
    let nonce = payload
        .set_nonce
        .expect("the owner's envelope carries the set's nonce");
    fauna_client_folders::key_reader::update(b_custody.as_ref(), |cfg| {
        custody::merge_received_keys(cfg, channel_id, payload.keys.clone());
        // The rest of the custody-ingest leg: the set's nonce, received inside
        // the envelope, into B's entry — what B's host signs and verifies under.
        custody::record_received_set_nonce(cfg, &channel_id, nonce, 1_000);
    })
    .await
    .expect("B's custody write");
    b_plane.write_custody(&b_custody.snapshot()).await;
    assert_eq!(
        custody::set_nonce_for_channel(&a_custody.snapshot(), &channel_id),
        Some(nonce),
        "the bind kept the create-time nonce as the set's one identity"
    );

    // The set was sealed at its keyed create: read the unrendered wire rows
    // (`list` without label custody drops a sealed set) and match by hash.
    let folders = FoldersClient::new(a_nest.clone())
        .list_wire()
        .await
        .expect("list");
    let row = folders
        .folders
        .iter()
        .find(|f| f.is_named(SHARED))
        .expect("the shared set's row");
    SharedSet {
        url,
        state,
        folder_ref: FolderRef::Local(row.id),
        channel_id,
        nonce,
        raw_group_id: created.raw_group_id,
        a_nest,
        a_bearer,
        b_bearer,
        a_custody,
        b_custody,
        a_plane,
        b_plane,
    }
}

impl SharedSet {
    /// Merge `keys` into B's custody and B's plane (B's identity-holding app
    /// ran its custody-ingest leg).
    async fn member_ingests(&self, keys: FolderContentKeys) {
        let channel_id = self.channel_id;
        let (joined, ()) =
            fauna_client_folders::key_reader::update(self.b_custody.as_ref(), |cfg| {
                custody::merge_received_keys(cfg, channel_id, keys.clone());
            })
            .await
            .expect("B's custody write");
        self.b_plane.write_custody(&joined).await;
    }

    /// The generations A's custody holds.
    async fn owner_keys(&self) -> FolderContentKeys {
        custody::content_keys(&self.a_custody.snapshot(), &self.channel_id)
            .expect("A holds the set's keys")
    }

    /// The owner rotates: a fresh generation into A's custody (and A's plane),
    /// and the envelope re-published carrying it as the floor. Returns A's
    /// custody after it.
    async fn owner_rotates(&self) -> FolderContentKeys {
        let (cfg, ()) = fauna_client_folders::key_reader::update(self.a_custody.as_ref(), |cfg| {
            custody::rotate_set(cfg, &self.channel_id, [0x77; 32], 9_000_000);
        })
        .await
        .expect("A's rotation custody write");
        self.a_plane.write_custody(&cfg).await;
        let keys = custody::content_keys(&cfg, &self.channel_id).expect("rotated custody");
        common::dispatch(
            self.state.rpc_router.as_ref(),
            self.state.clone(),
            actor_of(A_SECRET),
            "fauna.folders.content_key.put",
            wire(&fauna_protocol::folders::addressed(ContentKeyPutRequest {
                name: SHARED.into(),
                epoch: 2,
                sealed: "cd".repeat(48),
                current_version: keys.current_version(),
                ..Default::default()
            })),
        )
        .await
        .expect("the floor-carrying envelope publish");
        keys
    }

    /// A's seed-holding bound engine uploads `bytes` at `rel`, sealed under
    /// `keys`' current generation, and the nest records it.
    async fn owner_uploads(&self, rel: &str, bytes: &[u8], keys: FolderContentKeys) {
        let watch = tempfile::tempdir().unwrap();
        std::fs::write(watch.path().join(rel), bytes).unwrap();
        let (engine_client, nest_client) = fauna_nest::test_support::sync_engine_auth_client(
            &self.url,
            &self.a_bearer,
            A_SECRET,
            &A_WRITER_DEVICE,
        );
        nest_client.connect().await.expect("writer connects");
        let engine = SyncEngine::new(
            watch.path().to_path_buf(),
            SyncDb::open_in_memory().unwrap(),
            engine_client,
            Some(SHARED.to_string()),
            A_WRITER_DEVICE,
            None, // mls
            None, // epoch_secret
            None, // a bound set seals under its content keys, never the owner root
            Some(self.raw_group_id.clone()),
            Some(keys),
            ConflictPolicy::Auto,
            FormatRegistry::new(),
            IgnoreMatcher::default(),
            4,
            TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
            nest_client,
            fauna_sync_engine::config::SyncMode::Sync,
        );
        // The seed-holding device signs directly with A's identity under the
        // set's nonce.
        engine.set_change_signer(
            Some(common::direct_signer(&ActorKeypair::from_secret(A_SECRET))),
            Some(self.nonce),
        );
        engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
            set_nonce: Some(self.nonce),
            owner: Some(actor_of(A_SECRET)),
            ..Default::default()
        });
        let outcome = engine.upload_file(rel).await.expect("owner upload");
        assert!(outcome.recorded, "the nest records the owner's {rel}");
    }

    /// A capability host for `secret`'s account: its `BackupKey` and a bearer,
    /// nothing else. Returns the host and its root (where the OS stages writes).
    fn host(
        &self,
        secret: [u8; 32],
        device: [u8; 32],
    ) -> (Arc<FfiFileProviderHost>, std::path::PathBuf) {
        let (bearer, principal) = if secret == A_SECRET {
            (&self.a_bearer, A_PRINCIPAL_SECRET)
        } else {
            (&self.b_bearer, B_PRINCIPAL_SECRET)
        };
        let state_dir = tempfile::tempdir().unwrap();
        let root_dir = tempfile::tempdir().unwrap();
        let state_path = state_dir.path().to_string_lossy().into_owned();
        let root_path = root_dir.path().to_path_buf();
        std::mem::forget(state_dir);
        std::mem::forget(root_dir);
        let host = FfiFileProviderHost::app_dead(
            self.url.clone(),
            actor_of(secret).to_vec(),
            device.to_vec(),
            "fauna-file-provider".to_string(),
            BackupKey::derive(&secret).to_bytes().to_vec(),
            Arc::new(StaticFfiBearer(bearer.clone())),
            TestSigner::carrying(Some(principal_carriage(
                &ActorKeypair::from_secret(secret),
                &ActorKeypair::from_secret(principal),
                true,
            ))),
            state_path,
            self.folder_ref.to_wire(),
            root_path.to_string_lossy().into_owned(),
            None,
        )
        .expect("app_dead host builds");
        (host, root_path)
    }

    /// The generation the nest recorded `rel` under (`Some(None)` = recorded
    /// unstamped), or `None` when nothing was recorded for it.
    async fn recorded_version(&self, rel: &str) -> Option<Option<u64>> {
        let listed = CtlSyncClient::new(self.a_nest.clone())
            .changes_list(Some(SHARED.into()), None, 0)
            .await
            .expect("owner lists changes");
        let hash = hex::encode(fauna_core::sync::path_hash(rel));
        listed
            .changes
            .iter()
            .rev()
            .find(|c| c.path_hash == hash)
            .map(|c| c.content_key_version)
    }
}

async fn within<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), fut)
        .await
        .unwrap_or_else(|_| panic!("{what} must not hang"))
}

/// Flow 1 — decision 1: a host handed only the `BackupKey` and a bearer loads the
/// bound set's content keys from custody itself, populates, hydrates and decrypts
/// to the exact plaintext — the owner's host from A's custody, the member's from
/// B's own.
#[tokio::test]
async fn capability_host_opens_a_bound_set_from_custody_under_its_backup_key() {
    let s = shared_set().await;
    let gen1 = s.owner_keys().await;
    assert_eq!(gen1.current_version(), 1);
    s.owner_uploads("bound.txt", PROOF_CONTENT, gen1).await;

    for (who, secret, device) in [
        ("owner", A_SECRET, A_HOST_DEVICE),
        ("member", B_SECRET, B_HOST_DEVICE),
    ] {
        let (host, _root) = s.host(secret, device);
        let items = within("enumerate", host.enumerate(String::new()))
            .await
            .unwrap_or_else(|e| panic!("the {who}'s host serves the bound set: {e:?}"));
        assert!(
            items.iter().any(|i| i.rel == "bound.txt"),
            "the {who}'s host enumerates the bound file (its path opens under the custody key)"
        );
        let got = within("fetch", host.fetch("bound.txt".into()))
            .await
            .unwrap_or_else(|e| panic!("the {who}'s host hydrates: {e:?}"));
        assert_eq!(
            got.bytes, PROOF_CONTENT,
            "the {who}'s host decrypts to the exact plaintext"
        );
    }
}

/// Flow 2 — decision 2's fail-closed arm: B's custody holds generation 1 while a
/// file rests stamped generation 2. B's host cannot open it (never a stand-in
/// body) and keeps serving the rest, and a write is refused un-acked rather than
/// sealed under generation 1: nothing is recorded for it. (The nest's own floor
/// would refuse a member's stale record too; flow 4 pins the host's check where
/// nothing else would — the owner's host.)
#[tokio::test]
async fn capability_host_missing_the_stamped_generation_fails_closed_and_holds_writes() {
    let s = shared_set().await;
    s.owner_uploads("early.txt", NESTED_CONTENT, s.owner_keys().await)
        .await;
    let gen2 = s.owner_rotates().await;
    assert_eq!(gen2.current_version(), 2);
    s.owner_uploads("late.txt", PROOF_CONTENT, gen2).await;
    assert_eq!(s.recorded_version("late.txt").await, Some(Some(2)));

    let (host, root) = s.host(B_SECRET, B_HOST_DEVICE);
    let early = within("fetch early", host.fetch("early.txt".into()))
        .await
        .expect("a gen-1 file still opens on a host missing gen 2");
    assert_eq!(early.bytes, NESTED_CONTENT);
    // A path is content: it rests sealed under the generation its body does
    // (`file-sync.md` § Sealed names & paths), so the gen-2 file does not
    // enumerate at all on a host without gen 2 — and an open by its identifier
    // fails rather than serving any body.
    let listed = within("enumerate", host.enumerate(String::new()))
        .await
        .expect("the host keeps serving the set");
    assert!(
        listed.iter().any(|i| i.rel == "early.txt") && !listed.iter().any(|i| i.rel == "late.txt"),
        "the gen-1 file lists, the gen-2 file's sealed path cannot (got {:?})",
        listed.iter().map(|i| i.rel.clone()).collect::<Vec<_>>()
    );
    assert!(
        within("fetch late", host.fetch("late.txt".into()))
            .await
            .is_err(),
        "a body stamped with a missing generation must not open"
    );

    // A write while the floor (2) is ahead of the generation held (1).
    std::fs::write(root.join("mine.txt"), b"member edit").unwrap();
    let ack = within("ingest", host.ingest("mine.txt".into())).await;
    assert!(
        !matches!(ack, Ok(ref a) if a.acked),
        "a write on a host behind the floor is never acknowledged"
    );
    assert_eq!(
        s.recorded_version("mine.txt").await,
        None,
        "nothing is recorded for a write held behind the floor"
    );
}

/// Flow 3 — a removed member keeps nothing: after B is evicted and A rotates, a
/// host B builds afterwards has no row to build from, and a host B built before
/// the removal cannot open what A added after it.
#[tokio::test]
async fn removed_member_host_cannot_open_content_added_after_the_removal() {
    let s = shared_set().await;
    s.owner_uploads("before.txt", NESTED_CONTENT, s.owner_keys().await)
        .await;
    let (early_host, _root) = s.host(B_SECRET, B_HOST_DEVICE);
    let before = within("fetch before", early_host.fetch("before.txt".into()))
        .await
        .expect("B's host opens a file from while B was a member");
    assert_eq!(before.bytes, NESTED_CONTENT);

    common::dispatch(
        s.state.rpc_router.as_ref(),
        s.state.clone(),
        actor_of(A_SECRET),
        "fauna.folders.members.evict",
        wire(&fauna_protocol::folders::addressed(MemberEvictRequest {
            name: SHARED.into(),
            member: hex::encode(actor_of(B_SECRET)),
            ..Default::default()
        })),
    )
    .await
    .expect("evict B");
    let gen2 = s.owner_rotates().await;
    s.owner_uploads("after.txt", PROOF_CONTENT, gen2).await;

    let (late_host, _root) = s.host(B_SECRET, B_LATE_HOST_DEVICE);
    assert!(
        within(
            "enumerate on a post-removal host",
            late_host.enumerate(String::new())
        )
        .await
        .is_err(),
        "a host built after the removal has no row to build from"
    );
    // The early host reaches the change only through a refresh, whose re-read
    // finds the row gone — and even without it, B's custody has no generation 2.
    let _ = within("refresh", early_host.refresh()).await;
    assert!(
        within(
            "fetch on a pre-removal host",
            early_host.fetch("after.txt".into())
        )
        .await
        .is_err(),
        "a host built before the removal cannot open what was added after it"
    );
}

/// Flow 4 — decision 2's edges: hosts built before a rotation. Between the
/// rotation and the next refresh, a write re-reads custody before its seal and
/// is recorded under the NEW generation — for the owner's host (whom the nest's
/// floor exempts) as for the member's; the refresh then rebuilds over the same
/// state DB, and a file sealed under the new generation opens.
#[tokio::test]
async fn host_built_before_a_rotation_seals_under_the_new_generation_and_refresh_opens_it() {
    let s = shared_set().await;
    s.owner_uploads("seed.txt", NESTED_CONTENT, s.owner_keys().await)
        .await;
    let (owner_host, owner_root) = s.host(A_SECRET, A_HOST_DEVICE);
    let (member_host, member_root) = s.host(B_SECRET, B_HOST_DEVICE);
    for h in [&owner_host, &member_host] {
        within("fetch seed", h.fetch("seed.txt".into()))
            .await
            .expect("both hosts built at generation 1 open the seed file");
    }

    let gen2 = s.owner_rotates().await;
    s.member_ingests(gen2.clone()).await;

    for (who, host, root, rel) in [
        ("owner", &owner_host, &owner_root, "owner-edit.txt"),
        ("member", &member_host, &member_root, "member-edit.txt"),
    ] {
        std::fs::write(root.join(rel), format!("{who} edit")).unwrap();
        let ack = within("ingest", host.ingest(rel.into()))
            .await
            .unwrap_or_else(|e| panic!("the {who}'s write: {e:?}"));
        assert!(
            ack.acked,
            "the {who}'s write is recorded once custody holds the floor"
        );
        assert_eq!(
            s.recorded_version(rel).await,
            Some(Some(2)),
            "the {who}'s host sealed under the new generation, never the one it was built on"
        );
    }

    s.owner_uploads("fresh.txt", PROOF_CONTENT, gen2).await;
    for (who, host) in [("owner", &owner_host), ("member", &member_host)] {
        within("refresh", host.refresh())
            .await
            .unwrap_or_else(|e| panic!("the {who}'s refresh: {e:?}"));
        let got = within("fetch fresh", host.fetch("fresh.txt".into()))
            .await
            .unwrap_or_else(|e| {
                panic!("the {who}'s host opens the gen-2 file after refresh: {e:?}")
            });
        assert_eq!(got.bytes, PROOF_CONTENT);
        let seed = within("fetch seed again", host.fetch("seed.txt".into()))
            .await
            .unwrap_or_else(|e| panic!("the {who}'s rebuilt host still opens gen 1: {e:?}"));
        assert_eq!(seed.bytes, NESTED_CONTENT);
    }
}

// ── The owned-tree surface (android's SAF DocumentsProvider binding) ──
//
// `on-demand-files.md` § Android SAF DocumentsProvider binding → *Headless
// proof*: the owned-tree cores are tier-1 tested against the faked engine
// (`provider_face::owned_tree`), and proven here against a real nest through the
// same FFI host a DocumentsProvider holds — open-hydrates, a reclaimed body is
// observed and never recorded as a delete, a closed write uploads, a body left in
// the kept root by a killed process is uploaded by the start sweep, a write closed
// offline is uploaded by the sweep after reconnect, a conflicted close ends with
// the winner's bytes on the next open, a directory is renamed and deleted file by
// file, and a delete that cannot be recorded is refused.

/// One owned-tree host's directories — kept alive for the test, and reusable so
/// a second host can be built over the same state (a process restart).
struct OwnedDirs {
    state: tempfile::TempDir,
    files: tempfile::TempDir,
    cache: tempfile::TempDir,
}

impl OwnedDirs {
    fn new() -> Self {
        Self {
            state: tempfile::tempdir().unwrap(),
            files: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
        }
    }

    /// Build the owned-tree host over these directories against `url`, on the
    /// pre-created owner-only set with the owner's enrolled machine principal.
    fn host(&self, url: &str, bearer: &str) -> Arc<FfiFileProviderHost> {
        self.host_on(
            url,
            bearer,
            FolderRef::Local(1),
            TestSigner::owner_principal(),
        )
    }

    /// Build the owned-tree host over these directories against `url`, for
    /// `set`, signing through `signer`.
    fn host_on(
        &self,
        url: &str,
        bearer: &str,
        set: FolderRef,
        signer: Arc<dyn FfiChangeSignerProvider>,
    ) -> Arc<FfiFileProviderHost> {
        let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
        FfiFileProviderHost::app_dead_owned_tree(
            url.to_string(),
            owner.to_vec(),
            FP_HOST_DEVICE_ID.to_vec(),
            "fauna-documents-provider".to_string(),
            backup_key_bytes(),
            Arc::new(StaticFfiBearer(bearer.to_string())),
            signer,
            self.state.path().to_string_lossy().into_owned(),
            set.to_wire(),
            self.files.path().to_string_lossy().into_owned(),
            self.cache.path().to_string_lossy().into_owned(),
            None,
        )
        .expect("owned-tree host builds")
    }
}

/// Does a fresh, plain app-dead host (its own state) still enumerate `rel` at
/// the root — i.e. did no delete of it ever reach the nest?
async fn nest_still_lists(url: &str, bearer: &str, rel: &str) -> bool {
    let (reader, _root) = app_dead_host(url, bearer, backup_key_bytes()).await;
    within("fresh host enumerate", reader.enumerate(String::new()))
        .await
        .expect("fresh host enumerates")
        .iter()
        .any(|i| i.rel == rel)
}

/// The nest's recorded head of `rel`, read by a fresh plain app-dead host.
async fn nest_head(url: &str, bearer: &str, rel: &str) -> Vec<u8> {
    let (reader, _root) = app_dead_host(url, bearer, backup_key_bytes()).await;
    within("fresh host fetch", reader.fetch(rel.to_string()))
        .await
        .unwrap_or_else(|e| panic!("fresh host fetches {rel}: {e:?}"))
        .bytes
}

#[tokio::test]
async fn owned_tree_open_hydrates_and_a_reclaimed_body_is_observed_never_a_delete() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    let dirs = OwnedDirs::new();
    let host = dirs.host(&url, &bearer);

    // Open-hydrates: the placeholder's body is fetched into the cache root.
    let path = within("open_for_read", host.open_for_read("proof.txt".into()))
        .await
        .expect("open_for_read hydrates the placeholder");
    assert!(
        std::path::Path::new(&path).starts_with(dirs.cache.path()),
        "a recorded body lives in the cache root: {path}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), PROOF_CONTENT);

    // The OS reclaims the cache; the next observation flips the row back.
    std::fs::remove_file(&path).unwrap();
    let evicted = within("observe_evictions", host.observe_evictions())
        .await
        .expect("observe_evictions");
    assert_eq!(evicted, vec!["proof.txt".to_string()]);
    assert_eq!(
        host.lookup_body("proof.txt".into()).await.unwrap(),
        None,
        "a placeholder again"
    );
    assert!(
        nest_still_lists(&url, &bearer, "proof.txt").await,
        "a reclaimed body is never recorded as a delete"
    );

    // And the next open fetches it again.
    let again = within("re-open", host.open_for_read("proof.txt".into()))
        .await
        .expect("re-hydrates");
    assert_eq!(std::fs::read(again).unwrap(), PROOF_CONTENT);
}

#[tokio::test]
async fn owned_tree_closed_write_uploads_and_leaves_the_kept_root() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    let dirs = OwnedDirs::new();
    let host = dirs.host(&url, &bearer);
    const EDIT: &[u8] = b"edited through the owned tree\n";

    let open = within("open_for_write", host.open_for_write("proof.txt".into()))
        .await
        .expect("open_for_write");
    assert!(
        std::path::Path::new(&open.path).starts_with(dirs.files.path()),
        "a body open for write lives in the kept root: {}",
        open.path
    );
    std::fs::write(&open.path, EDIT).unwrap();

    let ack = within(
        "closed_write",
        host.closed_write("proof.txt".into(), open.base_content_version),
    )
    .await
    .expect("closed_write");
    assert!(
        ack.acked && !ack.content_changed,
        "the real nest records the edit"
    );
    assert!(
        !std::path::Path::new(&open.path).exists(),
        "a recorded body leaves the kept root"
    );
    let body = host
        .lookup_body("proof.txt".into())
        .await
        .unwrap()
        .expect("the body is still on the device");
    assert!(std::path::Path::new(&body).starts_with(dirs.cache.path()));
    assert_eq!(std::fs::read(body).unwrap(), EDIT);
    assert_eq!(nest_head(&url, &bearer, "proof.txt").await, EDIT);

    // A document created here uploads too.
    let ack = within("create_document", host.create_document("fresh.txt".into()))
        .await
        .expect("create_document");
    assert!(ack.acked);
    assert!(nest_still_lists(&url, &bearer, "fresh.txt").await);
}

#[tokio::test]
async fn owned_tree_start_sweep_uploads_a_killed_writers_edit() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    let dirs = OwnedDirs::new();
    const EDIT: &[u8] = b"written, then the process died before the close\n";

    let kept = {
        let host = dirs.host(&url, &bearer);
        let open = within("open_for_write", host.open_for_write("proof.txt".into()))
            .await
            .expect("open_for_write");
        std::fs::write(&open.path, EDIT).unwrap();
        open.path
        // The host drops here with the write never closed: the process died.
    };
    assert_eq!(
        nest_head(&url, &bearer, "proof.txt").await,
        PROOF_CONTENT,
        "nothing was uploaded before the kill"
    );

    // A new host over the same directories sweeps the kept root at start.
    let host = dirs.host(&url, &bearer);
    let body = within("lookup after restart", host.lookup_body("proof.txt".into()))
        .await
        .unwrap()
        .expect("the body survived");
    assert!(
        !std::path::Path::new(&kept).exists()
            && std::path::Path::new(&body).starts_with(dirs.cache.path()),
        "the start sweep uploaded the body and demoted it to the cache root ({body})"
    );
    assert_eq!(nest_head(&url, &bearer, "proof.txt").await, EDIT);

    // Idempotent: a re-driven sweep finds nothing left to do.
    let report = within("sweep_kept_root", host.sweep_kept_root())
        .await
        .expect("sweep");
    assert!(report.recorded.is_empty() && report.pending.is_empty());
}

/// A TCP relay in front of the nest that the test can cut — the device going
/// offline mid-session: every relayed connection is dropped and new ones are
/// refused until [`Self::restore`] brings the device back online.
struct CuttableRelay {
    url: String,
    online: Arc<std::sync::atomic::AtomicBool>,
    pipes: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    accept: tokio::task::AbortHandle,
}

impl CuttableRelay {
    async fn start(target: &str) -> Self {
        use std::sync::atomic::Ordering;
        let target = target.trim_start_matches("http://").to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pipes: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();
        let (up, held) = (Arc::clone(&online), Arc::clone(&pipes));
        let accept = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                if !up.load(Ordering::SeqCst) {
                    continue; // offline: the connection is dropped at once
                }
                let target = target.clone();
                let pipe = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(&target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                held.lock().unwrap().push(pipe.abort_handle());
            }
        });
        Self {
            url,
            online,
            pipes,
            accept: accept.abort_handle(),
        }
    }

    fn cut(&self) {
        self.online
            .store(false, std::sync::atomic::Ordering::SeqCst);
        for pipe in self.pipes.lock().unwrap().drain(..) {
            pipe.abort();
        }
    }

    fn restore(&self) {
        self.online.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for CuttableRelay {
    fn drop(&mut self) {
        self.accept.abort();
        self.cut();
    }
}

#[tokio::test]
async fn owned_tree_refuses_a_delete_that_cannot_be_recorded() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    let relay = CuttableRelay::start(&url).await;
    let dirs = OwnedDirs::new();
    let host = dirs.host(&relay.url, &bearer);

    let path = within("open_for_read", host.open_for_read("proof.txt".into()))
        .await
        .expect("hydrates while online");

    relay.cut();
    let refused = tokio::time::timeout(
        Duration::from_secs(120),
        host.delete_document("proof.txt".into()),
    )
    .await
    .expect("an offline delete must answer, not hang");
    assert!(
        refused.is_err(),
        "a delete that cannot be recorded is refused to the caller"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        PROOF_CONTENT,
        "and nothing changed locally"
    );
    assert_eq!(
        host.lookup_body("proof.txt".into()).await.unwrap(),
        Some(path)
    );
    assert!(
        nest_still_lists(&url, &bearer, "proof.txt").await,
        "no delete reached the nest"
    );
}

#[tokio::test]
async fn owned_tree_a_write_closed_offline_uploads_at_the_next_sweep_after_reconnect() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    let relay = CuttableRelay::start(&url).await;
    let dirs = OwnedDirs::new();
    let host = dirs.host(&relay.url, &bearer);
    const EDIT: &[u8] = b"written and closed while the device was offline\n";

    let open = within("open_for_write", host.open_for_write("proof.txt".into()))
        .await
        .expect("opens while online");
    std::fs::write(&open.path, EDIT).unwrap();

    relay.cut();
    let closed = tokio::time::timeout(
        Duration::from_secs(120),
        host.closed_write("proof.txt".into(), open.base_content_version),
    )
    .await
    .expect("an offline close must answer, not hang");
    assert!(
        !matches!(closed, Ok(ref ack) if ack.acked),
        "an offline close cannot be recorded"
    );
    assert_eq!(
        std::fs::read(&open.path).unwrap(),
        EDIT,
        "the un-recorded change stays in the kept root — never the cache root, \
         where the OS could reclaim it"
    );
    assert_eq!(nest_head(&url, &bearer, "proof.txt").await, PROOF_CONTENT);

    // Back online: the provider's next pull re-drives the sweep.
    relay.restore();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let report = within("sweep_kept_root", host.sweep_kept_root())
            .await
            .expect("sweep");
        if report.recorded == vec!["proof.txt".to_string()] {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sweep never recorded the offline write: {:?}",
            report.pending
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        !std::path::Path::new(&open.path).exists(),
        "demoted once recorded"
    );
    assert_eq!(nest_head(&url, &bearer, "proof.txt").await, EDIT);
}

#[tokio::test]
async fn owned_tree_a_conflicted_close_ends_with_the_winners_bytes_on_the_next_open() {
    use std::time::UNIX_EPOCH;
    const REMOTE: &[u8] = b"recorded by the owner's other device meanwhile\n";
    const LOCAL: &[u8] = b"edited here from the older base\n";

    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let watch = seed_owner_files(&url, &bearer).await;
    let dirs = OwnedDirs::new();
    let host = dirs.host(&url, &bearer);

    let open = within("open_for_write", host.open_for_write("proof.txt".into()))
        .await
        .expect("open_for_write");

    // Another device records a newer head while the document is open here.
    std::fs::write(watch.path().join("proof.txt"), REMOTE).unwrap();
    let (other, other_nest) = owner_write_engine(&url, &bearer, watch.path().to_path_buf());
    other_nest
        .connect()
        .await
        .expect("the other device connects");
    assert!(
        other
            .upload_file("proof.txt")
            .await
            .expect("upload")
            .recorded
    );
    // The provider's tick pulls it before this writer closes.
    within("refresh", host.refresh()).await.expect("refresh");

    std::fs::write(&open.path, LOCAL).unwrap();
    std::fs::File::open(&open.path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1000))
        .unwrap();
    let ack = within(
        "closed_write",
        host.closed_write("proof.txt".into(), open.base_content_version),
    )
    .await
    .expect("closed_write");
    assert!(ack.acked, "the conflict resolved and recorded");
    assert!(ack.content_changed, "the newer remote head won");
    assert!(
        !std::path::Path::new(&open.path).exists(),
        "the loser's local body is dropped (the nest retains it)"
    );

    let reopened = within("re-open", host.open_for_read("proof.txt".into()))
        .await
        .expect("the next open fetches the winner");
    assert_eq!(std::fs::read(reopened).unwrap(), REMOTE);
    assert_eq!(nest_head(&url, &bearer, "proof.txt").await, REMOTE);
}

#[tokio::test]
async fn owned_tree_renames_and_deletes_a_directory_file_by_file() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, bearer) = start_test_nest(owner).await;
    let _watch = seed_owner_files(&url, &bearer).await;
    let dirs = OwnedDirs::new();
    let host = dirs.host(&url, &bearer);

    let ack = within(
        "create_document",
        host.create_document("docs/sub/second.txt".into()),
    )
    .await
    .expect("create_document");
    assert!(ack.acked);

    let ack = within(
        "rename_document",
        host.rename_document("docs".into(), "archive".into()),
    )
    .await
    .expect("a directory renames file by file");
    assert!(ack.acked, "every file's create recorded");
    assert!(
        !nest_still_lists(&url, &bearer, "docs").await,
        "the old directory is gone"
    );
    assert!(nest_still_lists(&url, &bearer, "archive").await);
    assert_eq!(
        nest_head(&url, &bearer, "archive/nested.txt").await,
        NESTED_CONTENT
    );
    assert_eq!(
        nest_head(&url, &bearer, "archive/sub/second.txt").await,
        b""
    );

    within("delete_document", host.delete_document("archive".into()))
        .await
        .expect("a directory deletes file by file");
    assert!(!nest_still_lists(&url, &bearer, "archive").await);
    assert!(
        nest_still_lists(&url, &bearer, "proof.txt").await,
        "only the directory's files were deleted"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Writer-signed records from the capability host
// (`docs/goal/architecture/mls-group-key-material.md` § M2 → *Multi-writer* →
// *Writer-signed change records*, ruling (1), *The capability host*).
//
// The host holds no key of its own: the app provisions it the machine
// principal's writer key and `DeviceAuthorization` beside the bearer, and the
// host re-reads that provisioning at every write. Every record it writes is
// signed with it — verifiable by the nest and by every reader from the list's
// `signer_certs` side table alone — and a host that finds no usable signer
// records nothing, saying why. The set is created through the production
// `set_lifecycle::create_set` (its nonce minted into custody first), and the
// principals are enrolled through the real `fauna.sync.register` +
// `fauna.sync.device_grant.register` handlers.
// ─────────────────────────────────────────────────────────────────────────────

const SIGNED_SET: &str = "signed";

/// A real nest with the owner's nonce'd set: `(url, bearer, owner client, the
/// set's ref, its nonce, the owner's first machine on the account plane)`.
async fn signed_set_nest() -> (
    String,
    String,
    Arc<fauna_client::NestClient>,
    FolderRef,
    [u8; 32],
    PlaneMachine,
) {
    let owner = ActorKeypair::from_secret(OWNER_SECRET);
    let (url, bearer, _db, plane) = start_test_nest_with_plane(owner.actor_id().0).await;
    let nest = common::connected_client(&url, ActorKeypair::from_secret(OWNER_SECRET)).await;
    let files = FoldersClient::new(nest.clone());
    let config = MemoryFolderKeyStore::default();
    let created = fauna_client_folders::set_lifecycle::create_set(
        &files,
        &config,
        fauna_protocol::folders::FolderCreateRequest {
            name: SIGNED_SET.into(),
            ..Default::default()
        },
    )
    .await
    .expect("the set creates, its nonce in custody");
    let nonce = fauna_client_folders::set_lifecycle::record_nonce(&files, &config, SIGNED_SET)
        .await
        .expect("custody reads")
        .expect("the set's nonce is in custody");
    plane.write_custody(&config.snapshot()).await;
    (
        url,
        bearer,
        nest,
        FolderRef::Local(created.id),
        nonce,
        plane,
    )
}

/// Enroll a machine principal for the owner the way the ceremony does — its
/// row (device id = the writer key) carrying the root-signed `[RenewBearer,
/// SyncWrite]` grant, and its enrollment in the account's device set, keyed
/// for the current generation by `plane`'s top-up — and return the carriage
/// the app would provision.
async fn enroll_principal(
    url: &str,
    nest: &Arc<fauna_client::NestClient>,
    plane: &PlaneMachine,
) -> (FfiChangeSignerCarriage, [u8; 32]) {
    let owner = ActorKeypair::from_secret(OWNER_SECRET);
    let writer = ActorKeypair::generate();
    let writer_pub = writer.actor_id().0;
    register_principal(nest, &owner, &writer).await;
    plane
        .enroll_sibling(
            url,
            ed25519_dalek::SigningKey::from_bytes(writer.secret_bytes()),
        )
        .await;
    (principal_carriage(&owner, &writer, true), writer_pub)
}

/// The set's head row for `rel`, verified the way any reader verifies it — from
/// the list's side table alone, under the nonce custody holds. Returns the key
/// that signed it.
async fn verified_signer_of(
    nest: &Arc<fauna_client::NestClient>,
    nonce: [u8; 32],
    rel: &str,
) -> [u8; 32] {
    let page = CtlSyncClient::new(nest.clone())
        .changes_list(Some(SIGNED_SET.into()), None, 0)
        .await
        .expect("owner lists changes");
    let hash = hex::encode(fauna_core::sync::path_hash(rel));
    let row = page
        .changes
        .iter()
        .rev()
        .find(|c| c.path_hash == hash)
        .unwrap_or_else(|| panic!("a row for {rel}"));
    let mut certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    certs.ingest_all(&page.signer_certs);
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    fauna_protocol::sync_writer_sig::verify_row(row, nonce, &certs, |a| *a == owner)
        .unwrap_or_else(|e| panic!("the host's record of {rel} verifies: {e}"));
    row.signer_key
        .as_ref()
        .expect("signed")
        .as_slice()
        .try_into()
        .expect("32-byte signer key")
}

/// Whether the set has any row for `rel`.
async fn set_lists(nest: &Arc<fauna_client::NestClient>, rel: &str) -> bool {
    let hash = hex::encode(fauna_core::sync::path_hash(rel));
    CtlSyncClient::new(nest.clone())
        .changes_list(Some(SIGNED_SET.into()), None, 0)
        .await
        .expect("owner lists changes")
        .changes
        .iter()
        .any(|c| c.path_hash == hash)
}

async fn ingest_within(
    host: &FfiFileProviderHost,
    rel: &str,
) -> Result<fauna_ffi::FfiFileProviderAck, fauna_ffi::FfiError> {
    tokio::time::timeout(Duration::from_secs(60), host.ingest(rel.to_string()))
        .await
        .expect("ingest must answer, not hang")
}

/// Every record the host writes is signed by the principal the app
/// provisioned, and verifies from the side table alone; a principal re-minted
/// mid-life is picked up at the next write, without rebuilding the host.
#[tokio::test]
async fn a_capability_host_signs_every_record_with_the_provisioned_principal() {
    let (url, bearer, nest, set, nonce, plane) = signed_set_nest().await;
    let (first, first_pub) = enroll_principal(&url, &nest, &plane).await;
    let signer = TestSigner::carrying(Some(first));
    let backup_key = BackupKey::derive(&OWNER_SECRET).to_bytes().to_vec();
    let (host, root) = app_dead_host_on(&url, &bearer, backup_key, set, signer.clone());

    std::fs::write(root.join("a.txt"), b"signed by the first principal\n").unwrap();
    assert!(ingest_within(&host, "a.txt").await.expect("ingest").acked);
    assert_eq!(
        verified_signer_of(&nest, nonce, "a.txt").await,
        first_pub,
        "signed by the provisioned principal's writer key"
    );

    // The machine's principal is re-minted and re-enrolled; the app provisions
    // the new one. The same host signs its next write with it.
    let (second, second_pub) = enroll_principal(&url, &nest, &plane).await;
    signer.provision(Some(second));
    std::fs::write(root.join("b.txt"), b"signed by the successor\n").unwrap();
    assert!(ingest_within(&host, "b.txt").await.expect("ingest").acked);
    assert_eq!(
        verified_signer_of(&nest, nonce, "b.txt").await,
        second_pub,
        "a re-provisioned principal is read at the next write"
    );

    // A delete records too, and is signed like any record.
    let ack = host.delete("a.txt".into()).await.expect("delete");
    assert!(ack.acked);
    assert_eq!(verified_signer_of(&nest, nonce, "a.txt").await, second_pub);
}

/// A host with no usable signer holds every write with the reason and records
/// nothing — while still serving reads — and writes signed as soon as the app
/// provisions one.
#[tokio::test]
async fn a_capability_host_with_no_signer_records_nothing() {
    let (url, bearer, nest, set, nonce, plane) = signed_set_nest().await;
    let signer = TestSigner::carrying(None);
    let backup_key = BackupKey::derive(&OWNER_SECRET).to_bytes().to_vec();
    let (host, root) = app_dead_host_on(&url, &bearer, backup_key, set, signer.clone());

    tokio::time::timeout(Duration::from_secs(30), host.enumerate(String::new()))
        .await
        .expect("enumerate must not hang")
        .expect("reads need no signer");

    std::fs::write(root.join("held.txt"), b"held until signed\n").unwrap();
    let held = ingest_within(&host, "held.txt").await;
    let why = format!("{:?}", held.err().expect("no signer: the write is held"));
    assert!(why.contains("no change signer"), "says why: {why}");
    assert!(!set_lists(&nest, "held.txt").await, "nothing was recorded");

    // A carriage that cannot sign — a `[RenewBearer]`-only grant — is held too.
    let owner = ActorKeypair::from_secret(OWNER_SECRET);
    signer.provision(Some(principal_carriage(
        &owner,
        &ActorKeypair::generate(),
        false,
    )));
    let held = ingest_within(&host, "held.txt").await;
    let why = format!("{:?}", held.err().expect("an unusable signer: held"));
    assert!(why.contains("unusable"), "says why: {why}");
    assert!(
        !set_lists(&nest, "held.txt").await,
        "still nothing recorded"
    );

    // The app provisions the enrolled principal: the next attempt records,
    // signed — the host built without a signer rebuilt with one.
    let (carriage, writer_pub) = enroll_principal(&url, &nest, &plane).await;
    signer.provision(Some(carriage));
    assert!(
        ingest_within(&host, "held.txt")
            .await
            .expect("ingest")
            .acked
    );
    assert_eq!(
        verified_signer_of(&nest, nonce, "held.txt").await,
        writer_pub
    );
}

/// The app's principal slot as android keeps it — a T10 credential slot (a file
/// backend here, the lent `EncryptedSharedPreferences` on the device) beside the
/// account store root the ceremony serializes its writes under.
struct SlotDirs {
    credentials: tempfile::TempDir,
    store: tempfile::TempDir,
}

impl SlotDirs {
    fn new() -> Self {
        Self {
            credentials: tempfile::tempdir().unwrap(),
            store: tempfile::tempdir().unwrap(),
        }
    }

    /// A fresh handle on the slot (the store is not clonable; the backend is
    /// the shared directory).
    fn credentials(&self) -> fauna_credential_store::CredentialStore {
        fauna_credential_store::CredentialStore::with_file_backend(
            fauna_sync_engine::account_runtime::CRED_NAMESPACE,
            self.credentials.path().to_path_buf(),
        )
    }
}

/// Enroll the owner's machine principal the way android's app does — the account
/// runtime's ceremony halves over the app's own principal slot: the writer key
/// minted into the slot (`resolve_writer_key_serialized`, the pre-assembly
/// resolver), the root-signed `[RenewBearer, SyncWrite]` grant persisted beside
/// it (`PrincipalSlot::store_device_authorization`), and the machine's row +
/// grant registered on the nest. Nothing is handed to any host: an in-process
/// host reads the slot itself. Returns the slot's writer key.
async fn enroll_principal_into_slot(
    url: &str,
    nest: &Arc<fauna_client::NestClient>,
    slot: &SlotDirs,
    plane: &PlaneMachine,
) -> [u8; 32] {
    let owner = ActorKeypair::from_secret(OWNER_SECRET);
    let actor_hex = owner.actor_id_hex();
    let store_root =
        fauna_sync_engine::account_runtime::StoreRoot::at(slot.store.path().to_path_buf());
    let key = fauna_sync_engine::account_runtime::resolve_writer_key_serialized(
        &store_root,
        &actor_hex,
        &slot.credentials(),
    )
    .expect("the writer key mints into the slot");
    let writer_pub = key.verifying_key().to_bytes();
    // The machine's enrollment in the account's device set, keyed for the
    // current generation — what lets the in-process host read the custody.
    plane.enroll_sibling(url, key.clone()).await;
    let grant = fauna_client_sync::build_principal_grant(&owner, &writer_pub).expect("grant");
    let store_dir = store_root
        .store_dir(&actor_hex)
        .expect("the per-actor store dir");
    fauna_sync_engine::principal_bundle::PrincipalSlot::resolve(
        Arc::new(slot.credentials()),
        actor_hex,
        store_dir,
        &writer_pub,
        None,
    )
    .store_device_authorization(grant.clone(), &writer_pub)
    .expect("the grant persists in the slot");

    let sync = CtlSyncClient::new(nest.clone());
    sync.register(hex::encode(writer_pub), "machine", None)
        .await
        .expect("the principal's row registers");
    let reply = sync
        .device_grant_register(hex::encode(writer_pub), grant)
        .await
        .expect("the grant registers");
    assert!(reply.registered);
    writer_pub
}

/// The owner uploads `bytes` at `rel` into the nonce'd signed set through the
/// production engine, signing directly with the identity key; returns the alive
/// watch dir.
async fn seed_signed_set_file(
    url: &str,
    bearer: &str,
    nonce: [u8; 32],
    rel: &str,
    bytes: &[u8],
) -> tempfile::TempDir {
    let watch = tempfile::tempdir().unwrap();
    std::fs::write(watch.path().join(rel), bytes).unwrap();
    let (engine, nest_client) =
        owner_write_engine_on(url, bearer, watch.path().to_path_buf(), SIGNED_SET);
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&ActorKeypair::from_secret(
                OWNER_SECRET,
            )),
        )),
        Some(nonce),
    );
    nest_client
        .connect()
        .await
        .expect("the owner's engine reaches Connected");
    let outcome = engine
        .upload_file(rel)
        .await
        .unwrap_or_else(|e| panic!("upload_file({rel}): {e}"));
    assert!(outcome.recorded, "the owner's seed of {rel} records");
    watch
}

/// The android-shaped provisioning (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records*, *The capability host* — the android arm): the
/// SAF provider runs in the app's process, so its owned-tree host is built over
/// a reader of the app's own principal slot (`SlotChangeSigner`) and nobody
/// provisions it anything. Built before the machine is enrolled, it holds no
/// principal, so it cannot read the account's plane custody (a host reads it
/// as the enrolled device it is — `on-demand-files.md` § Shared sets on a
/// capability host, decision 1′): without the set's nonce no signed row
/// verifies, so it serves an empty tree — fail closed, never an unverified
/// listing — and refuses a create. The ceremony then enrolls the machine into
/// the slot and — with no re-provisioning and nothing handed to the host — its
/// next refresh rebuilds over the custody it can now read: the set lists, and a
/// write and a create record at once, signed by the slot's writer key and
/// verifiable from the side table alone.
#[tokio::test]
async fn an_in_process_host_signs_with_the_principal_slot_it_reads_itself() {
    let (url, bearer, nest, set, nonce, plane) = signed_set_nest().await;
    let owner = ActorKeypair::from_secret(OWNER_SECRET);
    const ORIGINAL: &[u8] = b"the owner's original\n";
    let _watch = seed_signed_set_file(&url, &bearer, nonce, "note.txt", ORIGINAL).await;
    let slot = SlotDirs::new();
    let dirs = OwnedDirs::new();
    let host = dirs.host_on(
        &url,
        &bearer,
        set,
        fauna_ffi::SlotChangeSigner::over(slot.credentials(), owner.actor_id().0),
    );

    // Not enrolled yet: the host serves, but nothing of the signed set — its
    // rows cannot be verified without the nonce custody holds — and a create
    // is refused to the caller.
    let listed = within("enumerate", host.enumerate(String::new()))
        .await
        .expect("an unenrolled host still answers");
    assert!(
        listed.is_empty(),
        "no row verifies before the machine can read custody (got {:?})",
        listed.iter().map(|i| i.rel.clone()).collect::<Vec<_>>()
    );
    let refused = within("create_document", host.create_document("fresh.txt".into())).await;
    let why = format!(
        "{:?}",
        refused
            .err()
            .expect("a create on a machine not enrolled yet is refused")
    );
    assert!(why.contains("no change signer"), "says why: {why}");
    assert!(!set_lists(&nest, "fresh.txt").await, "nothing was created");
    assert_eq!(
        verified_signer_of(&nest, nonce, "note.txt").await,
        owner.actor_id().0,
        "the set's head is still the owner's seed row — nothing was recorded"
    );

    // The app signs in: the ceremony enrolls the machine into the slot and the
    // account's device set. The host is not touched, and nothing is handed to it.
    let writer_pub = enroll_principal_into_slot(&url, &nest, &slot, &plane).await;

    // The next refresh finds the slot's signer and rebuilds over the custody
    // the enrolled machine reads: the set's rows verify and list.
    within("refresh", host.refresh()).await.expect("refresh");
    let listed = within("enumerate", host.enumerate(String::new()))
        .await
        .expect("enumerate");
    assert!(
        listed.iter().any(|i| i.rel == "note.txt"),
        "the enrolled host lists the set (got {:?})",
        listed.iter().map(|i| i.rel.clone()).collect::<Vec<_>>()
    );

    // A write, and the create, record at once — the slot is read at the write.
    let open = within("open_for_write", host.open_for_write("note.txt".into()))
        .await
        .expect("open_for_write");
    std::fs::write(&open.path, b"edited once enrolled\n").unwrap();
    let ack = within(
        "closed_write",
        host.closed_write("note.txt".into(), open.base_content_version),
    )
    .await
    .expect("closed_write");
    assert!(ack.acked, "an enrolled machine's write records at once");
    assert_eq!(
        verified_signer_of(&nest, nonce, "note.txt").await,
        writer_pub
    );
    let ack = within("create_document", host.create_document("fresh.txt".into()))
        .await
        .expect("create_document");
    assert!(ack.acked, "an enrolled machine's create records at once");
    assert_eq!(
        verified_signer_of(&nest, nonce, "fresh.txt").await,
        writer_pub
    );
}

// ---------------------------------------------------------------------------
// Decision 3 — a reader's host is read-only by construction
// (`on-demand-files.md` § Shared sets on a capability host): a set shared with
// the account that it may only read builds read-only. It populates, hydrates
// and decrypts like any host, and every write core refuses before anything is
// sealed or recorded — on the apple-shaped host and on the one that owns its
// tree alike.
// ---------------------------------------------------------------------------

impl SharedSet {
    /// A sets B's grant on the shared set (`fauna.folders.members.set_access`).
    async fn owner_grants(&self, access: &str) {
        FoldersClient::new(self.a_nest.clone())
            .members_set_access(fauna_protocol::folders::MemberSetAccessRequest {
                name: SHARED.into(),
                actor_id: hex::encode(actor_of(B_SECRET)),
                access: access.into(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("A sets B's access to {access}: {e:?}"));
    }

    /// Does A's own (two-way) host still list `rel` — i.e. did no delete or
    /// rename of it ever reach the nest?
    async fn owner_still_lists(&self, rel: &str) -> bool {
        let (owner_host, _root) = self.host(A_SECRET, A_HOST_DEVICE);
        assert!(
            !within("owner is_read_only", owner_host.is_read_only())
                .await
                .expect("the owner's host builds"),
            "an owner's host is the ordinary two-way host"
        );
        within("owner enumerate", owner_host.enumerate(String::new()))
            .await
            .expect("the owner's host lists")
            .iter()
            .any(|i| i.rel == rel)
    }
}

/// The refusal a reader's host gives a write: an error naming the read-only
/// share, never an ack.
fn assert_read_only_refusal<T>(what: &str, result: Result<T, fauna_ffi::FfiError>) {
    match result {
        Ok(_) => panic!("{what} on a reader's host must be refused"),
        Err(e) => assert!(
            format!("{e:?}").contains("read-only"),
            "{what} must be refused as read-only, got {e:?}"
        ),
    }
}

/// Flow 5 — decision 3: B holds the set as a READER. B's host builds
/// read-only — it populates, enumerates, hydrates and decrypts exactly as a
/// writer's does and says it is read-only — and every write core refuses:
/// nothing is sealed, nothing is recorded, the file is still where it was. A
/// `writer` grant reaches the host at its next refresh (it rebuilds two-way
/// over the same state and the same write records), and a demotion reaches it
/// at the next write's own row read, with no refresh in between.
#[tokio::test]
async fn a_readers_host_reads_and_refuses_every_write_until_granted_writer() {
    let s = shared_set().await;
    s.owner_uploads("bound.txt", PROOF_CONTENT, s.owner_keys().await)
        .await;
    s.owner_grants("reader").await;

    let (host, root) = s.host(B_SECRET, B_HOST_DEVICE);
    assert!(
        within("is_read_only", host.is_read_only())
            .await
            .expect("a reader's set builds — it is not refused"),
        "a reader's host is read-only"
    );
    let items = within("enumerate", host.enumerate(String::new()))
        .await
        .expect("a reader's host populates and lists");
    assert!(items.iter().any(|i| i.rel == "bound.txt"));
    let got = within("fetch", host.fetch("bound.txt".into()))
        .await
        .expect("a reader's host hydrates");
    assert_eq!(
        got.bytes, PROOF_CONTENT,
        "a reader's host decrypts to the exact plaintext"
    );

    std::fs::write(root.join("mine.txt"), b"a reader's edit").unwrap();
    assert_read_only_refusal(
        "ingest",
        within("ingest", host.ingest("mine.txt".into())).await,
    );
    assert_read_only_refusal(
        "ingest_with_base",
        within(
            "ingest_with_base",
            host.ingest_with_base("bound.txt".into(), vec![0xEE; 32]),
        )
        .await,
    );
    assert_read_only_refusal(
        "delete",
        within("delete", host.delete("bound.txt".into())).await,
    );
    assert_read_only_refusal(
        "rename",
        within(
            "rename",
            host.rename("bound.txt".into(), "moved.txt".into()),
        )
        .await,
    );
    assert_eq!(
        s.recorded_version("mine.txt").await,
        None,
        "a reader's write records nothing"
    );
    assert_eq!(s.recorded_version("moved.txt").await, None);
    assert!(
        s.owner_still_lists("bound.txt").await,
        "a reader's delete and rename never reached the nest"
    );

    // Granted writer: the next refresh rebuilds the host two-way.
    s.owner_grants("writer").await;
    within("refresh", host.refresh())
        .await
        .expect("the refresh re-reads the row");
    assert!(
        !within("is_read_only", host.is_read_only()).await.unwrap(),
        "a writer grant reaches the host at its next refresh"
    );
    let ack = within("ingest", host.ingest("mine.txt".into()))
        .await
        .expect("a writer's write is served");
    assert!(ack.acked, "a writer's write records");
    assert_eq!(s.recorded_version("mine.txt").await, Some(Some(1)));

    // Demoted again: the next write's own row read rebuilds it read-only.
    s.owner_grants("reader").await;
    std::fs::write(root.join("late.txt"), b"after the demotion").unwrap();
    assert_read_only_refusal(
        "ingest after a demotion",
        within("ingest", host.ingest("late.txt".into())).await,
    );
    assert_eq!(s.recorded_version("late.txt").await, None);
    assert!(within("is_read_only", host.is_read_only()).await.unwrap());
}

/// Whether any regular file rests under `dir`.
fn holds_a_file(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            let path = e.path();
            path.is_file() || (path.is_dir() && holds_a_file(&path))
        })
    })
}

/// Flow 5′ — the same on a host that owns its tree (android's provider): an
/// open hydrates into the cache root, and open-for-write, create, delete and
/// rename are refused with nothing left in the kept root and nothing recorded;
/// the sweep runs and ingests nothing.
#[tokio::test]
async fn a_readers_owned_tree_host_opens_and_refuses_every_write() {
    let s = shared_set().await;
    s.owner_uploads("bound.txt", PROOF_CONTENT, s.owner_keys().await)
        .await;
    s.owner_grants("reader").await;

    let dirs = OwnedDirs::new();
    let host = FfiFileProviderHost::app_dead_owned_tree(
        s.url.clone(),
        actor_of(B_SECRET).to_vec(),
        B_HOST_DEVICE.to_vec(),
        "fauna-documents-provider".to_string(),
        BackupKey::derive(&B_SECRET).to_bytes().to_vec(),
        Arc::new(StaticFfiBearer(s.b_bearer.clone())),
        TestSigner::carrying(Some(principal_carriage(
            &ActorKeypair::from_secret(B_SECRET),
            &ActorKeypair::from_secret(B_PRINCIPAL_SECRET),
            true,
        ))),
        dirs.state.path().to_string_lossy().into_owned(),
        s.folder_ref.to_wire(),
        dirs.files.path().to_string_lossy().into_owned(),
        dirs.cache.path().to_string_lossy().into_owned(),
        None,
    )
    .expect("a reader's owned-tree host builds");
    assert!(
        within("is_read_only", host.is_read_only())
            .await
            .expect("a reader's set builds"),
        "a reader's owned-tree host is read-only"
    );

    let path = within("open_for_read", host.open_for_read("bound.txt".into()))
        .await
        .expect("a reader's open hydrates");
    assert!(std::path::Path::new(&path).starts_with(dirs.cache.path()));
    assert_eq!(std::fs::read(&path).unwrap(), PROOF_CONTENT);

    assert_read_only_refusal(
        "open_for_write",
        within("open_for_write", host.open_for_write("bound.txt".into())).await,
    );
    assert_read_only_refusal(
        "create_document",
        within("create_document", host.create_document("new.txt".into())).await,
    );
    assert_read_only_refusal(
        "delete_document",
        within("delete_document", host.delete_document("bound.txt".into())).await,
    );
    assert_read_only_refusal(
        "rename_document",
        within(
            "rename_document",
            host.rename_document("bound.txt".into(), "moved.txt".into()),
        )
        .await,
    );

    let sweep = within("sweep_kept_root", host.sweep_kept_root())
        .await
        .expect("a reader's sweep runs");
    assert!(
        sweep.recorded.is_empty() && sweep.pending.is_empty(),
        "a reader's sweep ingests nothing"
    );
    assert!(
        !holds_a_file(dirs.files.path()),
        "no write left a body in a reader's kept root"
    );
    assert_eq!(s.recorded_version("new.txt").await, None);
    assert_eq!(s.recorded_version("moved.txt").await, None);
    assert!(s.owner_still_lists("bound.txt").await);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        PROOF_CONTENT,
        "the hydrated body is untouched"
    );
}

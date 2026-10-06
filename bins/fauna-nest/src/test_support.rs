//! In-process test-nest construction, shared by this crate's own integration
//! tests and by `bins/fauna-sync-agent`'s `tier3-nest` harnesses.
//!
//! Four files had hand-copied the same "stand up a real nest on an ephemeral
//! port" idiom — `bins/fauna-nest/tests/conformance_content_key_chunk_route.rs`,
//! `bins/fauna-sync-agent/{src/restore_byteplane_tier3.rs, src/versions_tier3.rs,
//! tests/agent_process_tier3.rs}` — three of them naming another as their mirror
//! in a doc comment. Priority #2 says that belongs in one place; the reason it
//! stayed copied is that the natural home is *this* crate's library and a
//! separate crate can only reach it through a Cargo feature. That question is
//! settled by `docs/goal/architecture/e2e-automation-surface-gating.md` § rule
//! (a): the module is gated
//! `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`, so a plain
//! `cargo test -p fauna-nest` (dev profile ⇒ `debug_assertions`) reaches it with
//! no Cargo wiring at all and no test leaves the default loop, while a release
//! build strips it. Consumers forward the feature from their own test feature —
//! `fauna-sync-agent`'s `tier3-nest` does — and **never name it on a dependency
//! line** (rule (b)).
//!
//! **Why `test-helpers` and not the nest's existing `test-hooks`.** They are
//! different concepts and the codebase keeps them apart: `test-hooks` makes the
//! *running nest* serve the `/api/v1/test/*` automation routes — a runtime
//! surface on the product — while `test-helpers` is the fleet-wide spelling for
//! "this crate lends its in-process construction helpers to another crate's
//! tests" (63 sites in `libs/`). Reusing `test-hooks` here would drag 21 route
//! modules and `fauna-mls/test-helpers` into a harness that wants neither.
//!
//! **What stayed at the call sites, deliberately.** `tempfile` is a dev-only
//! dependency of this crate, so the blob directory is a parameter rather than a
//! `tempdir()` in here; and the handler set is a closure, because the four
//! harnesses genuinely register different families and registering the union
//! would change what each nest answers.

use std::path::PathBuf;
use std::sync::Arc;

use fauna_core::identity::ActorKeypair;

use crate::backup::service::BackupService;
use crate::db::CacheDb;
use crate::routes::AppState;
use crate::rpc_router::RpcRouter;
use crate::token_store::TokenStore;

/// A running in-process test nest: its base URL, the `AppState` its router was
/// built over (for assertions that reach past the wire), and an HTTP bearer for
/// the registered owner.
pub struct TestNest {
    /// `http://127.0.0.1:<ephemeral>` — the nest's base URL.
    pub base_url: String,
    /// The state the router serves, so a test can assert on the nest's own side.
    pub state: Arc<AppState>,
    /// A bearer for `owner_secret`'s actor, valid an hour — what the chunk plane
    /// and any `AuthClient::with_bearer_source` fixture ride.
    pub http_token: String,
}

/// Start a real in-process nest on an ephemeral port: real `build_router`, real
/// handlers, an in-memory `CacheDb`, the owner registered, and an HTTP bearer
/// minted for it.
///
/// `owner_secret` is the owner's **32-byte secret**, not its actor id: the actor
/// id is derived here and used for both the users row and the bearer, which is
/// the pairing every caller actually wants. (One copy of this used to take the
/// actor id and another the secret, and the mismatch was a real bug once — a
/// bearer-only test never noticed the users row was keyed by the raw secret
/// until an identity handshake checked registration and got
/// `fauna.auth.not_registered`.)
///
/// `blob_dir` attaches a `BackupService` over a real `DiskBlobStore` rooted
/// there — the byte plane, with no at-rest encryption or compression, so the
/// store is the opaque passthrough a content-addressed store is to a
/// client-sealed ciphertext. `None` starts a nest with no byte plane at all, for
/// harnesses that prove lifecycle rather than chunk serving. The directory must
/// outlive the nest; the caller owns it (this crate has `tempfile` only as a
/// dev-dependency, so a `tempdir()` here would not compile for every consumer).
///
/// `register` receives the router builder. Each harness registers the handler
/// families it needs and no more: registering the union would silently give a
/// nest kinds its test never meant it to answer.
pub async fn start_test_nest<F>(
    owner_secret: [u8; 32],
    blob_dir: Option<PathBuf>,
    register: F,
) -> TestNest
where
    F: FnOnce(&mut crate::rpc_router::RpcRouterBuilder),
{
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let backup_service = blob_dir
        .map(|path| Arc::new(BackupService::new(db.clone(), None, false, path, None).unwrap()));

    // Register the owner so the WS kinds' caller-class gate passes and the minted
    // bearer resolves to a real actor (the chunk-write route's `ChunkWriteAuth`
    // validates the bearer against the store).
    let actor_id = ActorKeypair::from_secret(owner_secret).actor_id();
    db.create_user(&actor_id.0, "free", "test").await.unwrap();

    let token_store = Arc::new(TokenStore::new());
    let http_token = token_store.insert(actor_id, 3600).await;

    let state = Arc::new(AppState {
        backup_service,
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            register(&mut b);
            b.build()
        }),
        auth: crate::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..AppState::for_test(db)
    });

    let app = crate::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // spawn-ok(test): the axum server for one in-process test nest. The teardown
    // mechanism that reaches it is the test binary exiting — there is no
    // deployment-seed generation here to outlive, and this whole module is
    // `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]` (see the
    // module doc), so the task cannot exist in a release artifact at all.
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    TestNest {
        base_url: format!("http://{addr}"),
        state,
        http_token,
    }
}

/// A bearer-token `SyncClient` + its underlying `NestClient`, both bound to
/// `owner_secret`/`device_id` — the auth/transport half of a `SyncEngine` test
/// fixture, kept separate from the engine builders below because the per-test
/// params (`folder`, `backup_key` vs `content_keys`, the `SyncDb`) belong at the
/// call site.
pub fn sync_engine_auth_client(
    dest_url: &str,
    http_token: &str,
    owner_secret: [u8; 32],
    device_id: &[u8; 32],
) -> (
    fauna_sync_engine::nest_client::SyncClient,
    Arc<fauna_client::NestClient>,
) {
    let owner_kp = || ActorKeypair::from_secret(owner_secret);
    let http_bearer: Arc<dyn fauna_nest_http::BearerSource> =
        Arc::new(fauna_nest_http::StaticBearer(http_token.to_string()));
    let http_auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        dest_url.to_string(),
        owner_kp(),
        http_bearer,
        reqwest::Client::new(),
    ));
    let engine_client = fauna_sync_engine::nest_client::SyncClient::new(http_auth, device_id);
    let nest_client = fauna_client::NestClient::new(dest_url.to_string(), owner_kp());
    (engine_client, nest_client)
}

/// The engine parameters the two shapes below share, so neither builder grows a
/// six-positional-argument signature that a call site can silently transpose.
pub struct EngineFixture<'a> {
    /// Base URL of the nest the engine uploads to ([`TestNest::base_url`]).
    pub dest_url: &'a str,
    /// The bearer the chunk plane rides ([`TestNest::http_token`]).
    pub http_token: &'a str,
    /// The owner secret the auth client signs as — the same one the nest
    /// registered.
    pub owner_secret: [u8; 32],
    /// This replica's device id.
    pub device_id: [u8; 32],
    /// The engine's watch directory. Kept alive by the caller.
    pub watch_path: PathBuf,
}

/// An owner-only `backup_key` [`SyncEngine`](fauna_sync_engine::engine::SyncEngine):
/// unbound, owner-scoped — the ordinary single-user folder / cross-location
/// backup shape (`segment_backup.rs` builds exactly this). No group, no content
/// keys.
pub fn backup_engine(fx: EngineFixture<'_>) -> fauna_sync_engine::engine::SyncEngine {
    let (engine_client, nest_client) =
        sync_engine_auth_client(fx.dest_url, fx.http_token, fx.owner_secret, &fx.device_id);
    fauna_sync_engine::engine::SyncEngine::new(
        fx.watch_path,
        fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
        engine_client,
        None,
        fx.device_id,
        None, // mls
        None, // epoch_secret
        Some(fauna_core::crypto::BackupKey::derive(&fx.owner_secret).into()),
        None, // mls_group_id: owner-scoped backup, never a shared set
        None, // content_keys: owner-scoped backup has no M2 generations
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        nest_client,
        fauna_sync_engine::config::SyncMode::Sync,
    )
}

/// A **bound** (content-key) [`SyncEngine`](fauna_sync_engine::engine::SyncEngine):
/// no `backup_key` (a bound shared set has none — it seals under the content
/// key), `mls_group_id = Some(group_id)` (the bound marker), `content_keys =
/// Some(genesis(content_key))`.
///
/// `folder = None`: the content-key seal path keys off `mls_group_id` +
/// `content_keys` rather than the set name, and the chunk/manifest HTTP uploads
/// are set-independent — so `None` keeps a byte-route test purely about the byte
/// route and skips the best-effort WS `record_change`.
pub fn bound_engine(
    fx: EngineFixture<'_>,
    group_id: &[u8],
    content_key: [u8; 32],
) -> fauna_sync_engine::engine::SyncEngine {
    let (engine_client, nest_client) =
        sync_engine_auth_client(fx.dest_url, fx.http_token, fx.owner_secret, &fx.device_id);
    fauna_sync_engine::engine::SyncEngine::new(
        fx.watch_path,
        fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
        engine_client,
        None,
        fx.device_id,
        None,                    // mls: the content path uses content_keys directly
        None,                    // epoch_secret: unused for M2 content keys
        None,                    // backup_key: None ⇒ the content-key seal path runs
        Some(group_id.to_vec()), // mls_group_id: the bound marker
        Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            content_key,
            1_000,
        )), // M2 generation history (version 1)
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        nest_client,
        fauna_sync_engine::config::SyncMode::Sync,
    )
}

/// A fresh `AppState` over an empty in-memory `CacheDb` — the fixture a
/// dozen-plus handler test modules each hand-copied under their own
/// `fixture_state`/`build_state` name, byte-for-byte, before this lift.
pub fn fixture_state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
    Arc::new(AppState::for_test(db))
}

/// Run the boot step `start_server` runs before a serving generation answers
/// anything — [`crate::nest_kek::mint_at_boot`] — for a test standing a nest up
/// by hand, and fail loudly if any member did not seat.
///
/// The one test-side caller of that step (`nest_kek`'s ratchet names this
/// file), so every hand-built nest that needs its single-row satellites boots
/// the way a real one does rather than minting them some other way.
pub async fn boot_mint(db: &Arc<CacheDb>) {
    let minted = crate::nest_kek::mint_at_boot(db)
        .await
        .expect("the boot mint task")
        .expect("a nest booted in a test holds a deployment keypair");
    let failures: Vec<String> = minted
        .failures()
        .into_iter()
        .map(|(member, e)| format!("{member}: {e:#}"))
        .collect();
    assert!(
        failures.is_empty(),
        "the boot mint could not seat every member: {failures:?}"
    );
}

/// Seat `seed` as the nest's deployment keypair — the `nest_keypair` row a
/// deployment-seed rotation swaps, and the one a minting door reads its seed
/// from ([`crate::nest_kek::deployment_seed`]).
pub async fn seat_deployment_seed(db: &CacheDb, seed: &[u8; 32]) {
    let public = ed25519_dalek::SigningKey::from_bytes(seed)
        .verifying_key()
        .to_bytes();
    db.set_nest_keypair(seed, &public)
        .await
        .expect("seat the deployment keypair");
}

/// Seat `state`'s own identity seed as the nest's deployment keypair.
///
/// `start_server` builds `nest_identity` as a view over the seed `nest_keypair`
/// holds; `AppState::for_test` gives it a stable test identity and no
/// `nest_keypair` row at all. A test that reaches a door minting a
/// key-encryption satellite needs the two to agree: the door seals under the
/// seed the database holds, and the reads that follow open under the
/// generation's copy.
pub async fn seat_own_deployment_seed(state: &AppState) {
    seat_deployment_seed(&state.db, &state.nest_identity.signing_key.to_bytes()).await;
}

/// A serving generation built from `seed`: `nest_signing_key` and the
/// `nest_identity` view over it, both from the one seed, as `start_server`
/// builds them. What a deployment-seed rotation's teardown replaces — and,
/// until it does, a copy of a seed the database may no longer hold.
pub fn serving_generation(db: Arc<CacheDb>, seed: &[u8; 32]) -> AppState {
    AppState {
        nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(seed)),
        nest_identity: Arc::new(crate::nest_identity::NestIdentity::from_seed(seed)),
        ..AppState::for_test(db)
    }
}

/// Whether `table` holds at least one `column` ciphertext and every one opens
/// under the key `context` derives from `seed` — the question the satellite
/// walk (`nest_kek::reencrypt_satellites`) asks of every row when a rotation
/// runs, and refuses the rotation on a row that answers no.
pub async fn every_row_opens_under(
    db: &Arc<CacheDb>,
    table: &'static str,
    column: &'static str,
    context: &'static str,
    seed: &[u8; 32],
) -> bool {
    let (db, key) = (db.clone(), crate::nest_kek::derive(context, seed));
    tokio::task::spawn_blocking(move || {
        let conn = db.conn_blocking();
        let mut stmt = conn
            .prepare(&format!("SELECT {column} FROM {table}"))
            .expect("prepare the satellite read");
        let wrapped = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        !wrapped.is_empty()
            && wrapped
                .iter()
                .all(|w| fauna_core::crypto::decrypt_backup_chunk(&key, w).is_ok())
    })
    .await
    .expect("the satellite read task")
}

/// [`fixture_state`] plus a real `BackupService` rooted in a fresh temp dir —
/// for handler tests that exercise the backup-triggering path. `#[cfg(test)]`
/// rather than this module's usual `debug_assertions`/`test-helpers` gate:
/// `tempfile` is a dev-dependency of this crate (see [`start_test_nest`]'s
/// doc comment), so this fixture — unlike the rest of the module — is only
/// ever reachable from this crate's own `cfg(test)` build, never from an
/// external `test-helpers` consumer.
#[cfg(test)]
pub async fn fixture_state_with_backup() -> (Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
    let tmp = tempfile::tempdir().expect("tempdir");
    let svc = BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None)
        .expect("BackupService::new");
    let mut st = AppState::for_test(db);
    st.backup_service = Some(Arc::new(svc));
    (Arc::new(st), tmp)
}

/// Seed an empty `mail` snapshot row for `actor` — the reserved folder plus a
/// message-kind snapshot over an empty [`fauna_segment_store::Manifest`].
/// Shared by the crate's snapshot tests (`filesync_handlers.rs`). `#[cfg(test)]` like [`fixture_state_with_backup`]: nothing
/// outside this crate's own tests needs it.
#[cfg(test)]
pub async fn seed_mail_snapshot(state: &Arc<AppState>, actor: &[u8; 32]) -> i64 {
    let fs_id = state
        .db
        .get_or_create_reserved_folder(actor, "mail")
        .await
        .expect("get_or_create_reserved_folder");
    let manifest = fauna_segment_store::Manifest::empty("mail");
    let content_blob =
        fauna_core::encoding::canonical_encode(&manifest).expect("serialize empty Manifest");
    state
        .db
        .create_message_kind_snapshot_row(fs_id, "mail", Some(&content_blob), None)
        .await
        .expect("create_message_kind_snapshot_row")
}

/// A small trained plaintext [`fauna_mail::spam::SpamModel`] fixture — alternate
/// `spam_text`/`ham_text` `n` times. `#[cfg(test)]` (see
/// [`fixture_state_with_backup`]'s doc comment): `bridge_blob_handlers` and
/// `bridge_imap_handlers` each hand-copied this exact training loop under their
/// own `small_spam_model`/`seed_spam_model` name; `seed_spam_model` additionally
/// persists the model to `spam_models`, so it now builds on top of this and
/// keeps its own name for that reason.
#[cfg(test)]
pub fn small_spam_model(spam_text: &str, ham_text: &str, n: usize) -> fauna_mail::spam::SpamModel {
    let mut m = fauna_mail::spam::SpamModel::new();
    for _ in 0..n {
        m.train_spam(spam_text);
        m.train_ham(ham_text);
    }
    m
}

/// The MSEK a fixture recipient's seal key derives from when the test never
/// opens what was sealed to it and so has no reason to name its own.
#[cfg(test)]
pub const FIXTURE_MSEK: [u8; 32] = [9u8; 32];

/// Give `actor` a recipient seal key the way the provision door does: both
/// halves, derived from `msek`, in one write through the production writer
/// ([`CacheDb::put_actor_recipient_seal_key`]). Returns the X25519 public half.
///
/// The one fixture writer for `actor_mls_pubkeys` in the unit tests. The key
/// is a real MSEK-derived pair, so a seal to it selects the X-Wing suite as it
/// does for an app user, and [`open_recipient_record`] opens the result.
#[cfg(test)]
pub async fn seed_recipient_seal_key(db: &CacheDb, actor: &[u8; 32], msek: &[u8; 32]) -> [u8; 32] {
    use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, derive_recipient_xwing_keypair};
    let (_secret, pubkey) = derive_recipient_hpke_keypair(msek);
    let xwing = derive_recipient_xwing_keypair(msek);
    db.put_actor_recipient_seal_key(actor, &pubkey, xwing.public.mlkem_encaps_key())
        .await
        .expect("seed recipient seal key");
    pubkey
}

/// Open a record sealed to the recipient key [`seed_recipient_seal_key`] wrote
/// for `msek` — either suite, as the envelope names its own.
#[cfg(test)]
pub fn open_recipient_record(sealed: &[u8], msek: &[u8; 32]) -> Vec<u8> {
    use fauna_mls::wrapped_blob::{
        MailRecordEnvelope, derive_recipient_hpke_keypair, derive_recipient_xwing_keypair,
        unseal_mail_record_hybrid,
    };
    let env = MailRecordEnvelope::from_canonical_bytes(sealed).expect("decode envelope");
    let (secret, _pubkey) = derive_recipient_hpke_keypair(msek);
    let xwing = derive_recipient_xwing_keypair(msek);
    unseal_mail_record_hybrid(&env, &secret, xwing.secret.mlkem_decaps_key())
        .expect("open recipient record")
}

/// Open a `fetch_spam_model` blob with the reader's MSEK-derived keys and
/// parse it.
#[cfg(test)]
pub fn unseal_fetched_model(sealed: &[u8], msek: &[u8; 32]) -> fauna_mail::spam::SpamModel {
    let model_json = open_recipient_record(sealed, msek);
    fauna_mail::spam::SpamModel::from_bytes(&model_json).expect("valid SpamModel")
}

/// What the deployment baseline's two serving paths hand out right now, as the
/// baseline's bytes: `(sealed-model reply field, plaintext cold-start fold)`
/// (`mail-spam.md` § Cold start Path 2). `mda` is an approved MDA the
/// cold-start fold is fetched through; `probe` must be fresh per call — it
/// mints one reader on each path (`[probe; 32]` and `[probe + 1; 32]`).
/// Shared by the departure tests in `bridge_imap_handlers` and the holder-drain
/// tests in `bridge_blob_handlers`, which each assert the same two paths.
#[cfg(test)]
pub async fn baseline_as_served(
    state: &Arc<AppState>,
    mda: &[u8; 32],
    probe: u8,
) -> (
    Option<fauna_mail::spam::SpamModel>,
    Option<fauna_mail::spam::SpamModel>,
) {
    use fauna_protocol::wrapped_blob::{
        FetchSpamModelReply, FetchSpamModelRequest, PutSpamModelRequest,
    };
    fn payload<T: serde::Serialize>(req: &T) -> bytes::Bytes {
        bytes::Bytes::from(fauna_protocol::encode_canonical(req).unwrap().to_vec())
    }
    let fetch = |caller: [u8; 32], actor: [u8; 32]| {
        let state = state.clone();
        async move {
            let bytes = crate::bridge_imap_handlers::fetch_spam_model_handler()(
                state,
                caller,
                payload(&FetchSpamModelRequest {
                    actor_id: actor.to_vec(),
                    extra: Default::default(),
                }),
            )
            .await
            .expect("fetch ok");
            fauna_cbor::decode_strict::<FetchSpamModelReply>(&bytes).unwrap()
        }
    };

    // Path 1 — a client-sealed model: the aggregate rides `baseline`.
    let sealed_reader = [probe; 32];
    state
        .db
        .create_user(&sealed_reader, "free", "test")
        .await
        .unwrap();
    crate::bridge_imap_handlers::put_spam_model_handler()(
        state.clone(),
        sealed_reader,
        payload(&PutSpamModelRequest {
            sealed_model: vec![0xEEu8; 300],
            sample_count: 0,
            ..Default::default()
        }),
    )
    .await
    .expect("put ok");
    let field = fetch(sealed_reader, sealed_reader)
        .await
        .baseline
        .map(|b| fauna_mail::spam::SpamModel::from_bytes(&b).expect("plaintext aggregate"));

    // Path 2 — a fresh actor with no model: the server-side fold hands the
    // full baseline back as the actor's read-time model, or nothing.
    let fresh_reader = [probe.wrapping_add(1); 32];
    let msek = [probe; 32];
    seed_recipient_seal_key(&state.db, &fresh_reader, &msek).await;
    let folded = fetch(*mda, fresh_reader)
        .await
        .blob
        .map(|sealed| unseal_fetched_model(&sealed, &msek));
    (field, folded)
}

/// Both serving paths serve a baseline right now ([`baseline_as_served`]).
#[cfg(test)]
pub async fn assert_baseline_served(state: &Arc<AppState>, mda: &[u8; 32], probe: u8) {
    let (field, folded) = baseline_as_served(state, mda, probe).await;
    assert!(
        field.is_some_and(|m| m.sample_count() > 0),
        "control: the sealed-model path serves the published baseline"
    );
    assert!(
        folded.is_some_and(|m| m.sample_count() > 0),
        "control: the cold-start fold serves the published baseline"
    );
}

/// Neither serving path serves a baseline right now ([`baseline_as_served`]).
#[cfg(test)]
pub async fn assert_baseline_withdrawn(
    state: &Arc<AppState>,
    mda: &[u8; 32],
    probe: u8,
    why: &str,
) {
    let (field, folded) = baseline_as_served(state, mda, probe).await;
    assert_eq!(field, None, "sealed-model path still serves: {why}");
    assert_eq!(folded, None, "cold-start fold still serves: {why}");
}

/// The simulated aggregation holder — an approved off-box content-processor
/// service user with an attested X25519 seal target, the identity
/// `resolve_content_processor_holder` prefers. Every per-user spam model rests
/// sealed, so the deployment baseline is only ever the holder's merged half
/// (`mail-spam.md` § Encrypted-mode interaction); the baseline tests drive this
/// stand-in through the real worklist + submit handlers.
#[cfg(test)]
pub const SIM_HOLDER_ACTOR: [u8; 32] = [0x5Au8; 32];
/// The simulated holder's X25519 seal target (see [`SIM_HOLDER_ACTOR`]).
#[cfg(test)]
pub const SIM_HOLDER_X25519: [u8; 32] = [0x5Bu8; 32];

/// Enroll the simulated holder ([`SIM_HOLDER_ACTOR`]) once per state.
#[cfg(test)]
pub async fn enroll_sim_holder(state: &Arc<AppState>) {
    if state
        .db
        .lookup_bridge_service_user(&SIM_HOLDER_ACTOR)
        .await
        .unwrap()
        .is_some()
    {
        return;
    }
    crate::bridge_approval_test_support::approve_bridge_with_x25519(
        &state.db,
        &SIM_HOLDER_ACTOR,
        crate::db::bridge_service_users::BridgeRole::ContentProcessor,
        &SIM_HOLDER_X25519,
        "sim-holder",
    )
    .await;
}

/// Store `actor`'s trained `model` the way an opted-in contributor's writing
/// agent does: an opaque sealed model row plus a holder copy for the simulated
/// holder, in one `put_spam_model` transaction, and the keyless
/// `content.read{spam-model}` grant to that holder. The copy is the model's
/// plain bytes standing in for what the holder would unseal — the nest never
/// opens it. Every call is a fresh write (a new sealed blob, a later
/// `updated_at`), as a retrain is. The opt-in itself is left to the caller.
#[cfg(test)]
pub async fn seed_sealed_contribution(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    model: &fauna_mail::spam::SpamModel,
) {
    use fauna_mls::wrapped_blob::{GrantBlob, GrantIndex, GrantWindow, ScopeTuple};
    enroll_sim_holder(state).await;
    let mut sealed_model = vec![0xEEu8; 32];
    sealed_model.extend_from_slice(actor);
    sealed_model.extend_from_slice(&model.sample_count().to_le_bytes());
    state
        .db
        .put_spam_model_with_history(
            actor,
            &sealed_model,
            None,
            Some((&SIM_HOLDER_X25519[..], &model.to_bytes()[..])),
        )
        .await
        .unwrap();
    let grant_id = [actor[0]; 16];
    let blob = GrantBlob {
        version: 1,
        kind: GrantBlob::KIND.to_string(),
        index: GrantIndex(actor.to_vec(), grant_id.to_vec()),
        holder: serde_bytes::ByteBuf::from(SIM_HOLDER_X25519.to_vec()),
        window: GrantWindow(0, u64::MAX),
        scope: vec![ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_SPAM_MODEL.into()),
            tier: None,
            set: None,
            factor: None,
        }],
        wrapped_keys: Vec::new(),
    }
    .to_canonical_bytes()
    .expect("encode grant blob");
    state
        .db
        .put_capability_grant(actor, &grant_id, &SIM_HOLDER_X25519, i64::MAX, &blob)
        .await
        .unwrap();
}

/// Run `fauna.bridges.publish_spam_baseline` as `admin` while the simulated
/// holder answers the run ([`run_with_sim_holder`]).
#[cfg(test)]
pub async fn publish_through_sim_holder(
    state: &Arc<AppState>,
    admin: [u8; 32],
) -> Result<fauna_protocol::bridge_routing::PublishSpamBaselineReply, fauna_protocol::RpcError> {
    use fauna_protocol::bridge_routing::{PublishSpamBaselineReply, PublishSpamBaselineRequest};
    let publish_state = state.clone();
    let bytes = run_with_sim_holder(state, async move {
        crate::bridge_imap_handlers::publish_spam_baseline_handler()(
            publish_state,
            admin,
            bytes::Bytes::from(
                fauna_protocol::encode_canonical(&PublishSpamBaselineRequest::default())
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
    })
    .await?;
    Ok(fauna_cbor::decode_strict::<PublishSpamBaselineReply>(&bytes).unwrap())
}

/// Drive `run` — anything that runs a spam-baseline publish: the admin's click
/// or the standing-publish cadence tick — while the simulated holder answers
/// the run the way the real one does: pull the run's grant-gated worklist,
/// "unseal" each copy (decode the stand-in), merge them additively, and
/// submit the half naming every contributor it merged. A run that never
/// registers (no sealed candidate, no resolvable holder, nothing due) or is
/// bound to another holder simply goes unanswered.
#[cfg(test)]
pub async fn run_with_sim_holder<T: Send + 'static>(
    state: &Arc<AppState>,
    run: impl std::future::Future<Output = T> + Send + 'static,
) -> T {
    use fauna_mail::spam::SpamModel;
    use fauna_protocol::wrapped_blob::{
        SpamBaselineWorklistReply, SpamBaselineWorklistRequest, SubmitSpamBaselineRequest,
    };
    fn payload<T: serde::Serialize>(req: &T) -> bytes::Bytes {
        bytes::Bytes::from(fauna_protocol::encode_canonical(req).unwrap().to_vec())
    }
    // spawn-ok(test)
    let task = tokio::spawn(run);
    let mut run_id = None;
    for _ in 0..2000 {
        if let Some(id) = state.spam_baseline_runs.lock().await.keys().next().cloned() {
            run_id = Some(id);
            break;
        }
        if task.is_finished() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    if let Some(run_id) = run_id
        && let Ok(bytes) = crate::bridge_blob_handlers::spam_baseline_worklist_handler()(
            state.clone(),
            SIM_HOLDER_ACTOR,
            payload(&SpamBaselineWorklistRequest {
                run_id: serde_bytes::ByteBuf::from(run_id.clone()),
                extra: Default::default(),
            }),
        )
        .await
    {
        let worklist: SpamBaselineWorklistReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let mut merged = SpamModel::new();
        let mut named = Vec::new();
        for copy in &worklist.copies {
            if let Some(model) = SpamModel::from_bytes(copy.sealed_copy.as_ref()) {
                merged.merge(&model);
                named.push(copy.owner_actor_id.clone());
            }
        }
        let contributors = u32::try_from(named.len()).unwrap();
        crate::bridge_blob_handlers::submit_spam_baseline_handler()(
            state.clone(),
            SIM_HOLDER_ACTOR,
            payload(&SubmitSpamBaselineRequest {
                run_id: serde_bytes::ByteBuf::from(run_id),
                merged_model: if named.is_empty() {
                    Vec::new()
                } else {
                    merged.to_bytes()
                },
                contributors,
                unreadable: 0,
                merged_contributors: named,
                extra: Default::default(),
            }),
        )
        .await
        .expect("submit ok");
    }
    task.await.expect("the publish task")
}

/// Assert NO push arrives within a short bound — a handler test's no-DB-change
/// arms must stay silent. `#[cfg(test)]` (see [`fixture_state_with_backup`]'s
/// doc comment): `bridge_caldav_handlers` and `bridge_carddav_handlers` each
/// hand-copied this exact 200ms-timeout wait/decode/panic shape byte for byte.
#[cfg(test)]
pub async fn expect_no_push(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) {
    match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
        Err(_) => {}
        Ok(Some(bytes)) => {
            let frame = fauna_protocol::decode_frame(&bytes).expect("decode frame");
            panic!("expected no push on a no-DB-change outcome, got {frame:?}");
        }
        Ok(None) => panic!("push channel closed"),
    }
}

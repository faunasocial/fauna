//! Two real in-process nests wired for nest-side segment backup — the source
//! nest's coordinator, its destination, and the owner's enroll end-state.
//!
//! Lifted out of `nest_backup_coordinator.rs` so a second test binary can drive
//! the same pass without a hand copy: `segment_serve_memory.rs` measures the
//! pass's allocations, and a restore's, which needs a binary of its own (it
//! installs a global allocator).

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_nest::backup::service::BackupService;
use fauna_nest::blob_store::BlobStoreBackend;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{backup_handlers, federation_handlers};
use fauna_protocol::backup::{
    DestinationRegisterReply, DestinationRegisterRequest, NestKeyGrantReply, NestKeyGrantRequest,
    WriterGrantRegisterReply, WriterGrantRegisterRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The owner's identity secret: the owner is a real ed25519 key, because the
/// covered-folder re-home is owner-signed (`writer-signed-change-records.md`
/// ruling (7)(a)).
pub const OWNER_SECRET: [u8; 32] = [0x11; 32];
/// `OWNER_SECRET`'s public key — the owner's actor id
/// (`nest_backup_coordinator.rs` pins the pair).
pub const OWNER: [u8; 32] = [
    208, 74, 178, 50, 116, 43, 180, 171, 58, 19, 104, 189, 70, 21, 228, 230, 208, 34, 74, 183, 26,
    1, 107, 175, 133, 32, 163, 50, 201, 119, 135, 55,
];

pub fn owner_key() -> fauna_core::identity::ActorKeypair {
    fauna_core::identity::ActorKeypair::from_secret(OWNER_SECRET)
}

/// The 32 bytes the owner's client granted. Deliberately *not* derived from
/// `OWNER` here — the nest receives raw granted bytes and must never be able to
/// derive them itself.
pub const GRANTED_KEY: [u8; 32] = [0x5A; 32];
pub const DEST_ID: &str = "dest-1";

// ═════════════════════════════════════════════════════════════════════════════
// Harness — two real nests
// ═════════════════════════════════════════════════════════════════════════════

/// A real in-process nest on loopback: own identity, federation router (so a
/// peer can dial it), anonymous discovery (so the pool can resolve its
/// `nest_id`), the client-facing backup kinds, and a real blob store + chunk
/// HTTP routes so the byte plane is not a mock.
///
/// `db_path` points into a fresh tempdir, which is both the coordinator's
/// per-owner state root and the segment store's parent — the same single data
/// dir a deployed nest has.
pub async fn start_nest() -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    // The data dir lives under the state's scratch root, removed when the
    // state drops.
    let base = AppState::for_test(db.clone());
    let data_path = base.test_scratch_path("data");
    std::fs::create_dir_all(&data_path).unwrap();

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    start_nest_with_base(base, data_path, db, secret, generous_limiter()).await
}

/// [`start_nest`] over a given data directory, database and identity seed —
/// what restarting a nest from a copy of its data directory needs: the same
/// identity (the destination's writer grant names it), the copy's database and
/// the copy's segment areas.
pub async fn start_nest_on(
    data_path: std::path::PathBuf,
    db: Arc<CacheDb>,
    secret: [u8; 32],
) -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    start_nest_throttled_on(data_path, db, secret, generous_limiter()).await
}

fn generous_limiter() -> Arc<fauna_nest::bridge_rate_limit::Limiter> {
    // A test drives many coordinator passes within a second, which a
    // deployment spaces minutes apart; the channel default (30 a minute per
    // kind) would refuse the mints. The custody-record kinds keep their own
    // writer budget whatever this says (`federation_router::max_events`); the
    // throttle's own tests below start a nest on the production limiter.
    let generous = fauna_nest::bridge_rate_limit::Limiter::with_config(
        fauna_nest::bridge_rate_limit::LimiterConfig {
            window: std::time::Duration::from_secs(60),
            max_events: 10_000,
        },
    );
    Arc::new(generous)
}

/// [`start_nest`] with the federation channel throttle `limiter` — the
/// production one, or one a test pre-fills to stand for a busy destination.
pub async fn start_nest_throttled(
    limiter: Arc<fauna_nest::bridge_rate_limit::Limiter>,
) -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let base = AppState::for_test(db.clone());
    let data_path = base.test_scratch_path("data");
    std::fs::create_dir_all(&data_path).unwrap();
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    start_nest_with_base(base, data_path, db, secret, limiter).await
}

pub async fn start_nest_throttled_on(
    data_path: std::path::PathBuf,
    db: Arc<CacheDb>,
    secret: [u8; 32],
    federation_rate_limit: Arc<fauna_nest::bridge_rate_limit::Limiter>,
) -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    // The caller owns `data_path`; the state owns only its own scratch root.
    let base = AppState::for_test(db.clone());
    start_nest_with_base(base, data_path, db, secret, federation_rate_limit).await
}

async fn start_nest_with_base(
    base: AppState,
    data_path: std::path::PathBuf,
    db: Arc<CacheDb>,
    secret: [u8; 32],
    federation_rate_limit: Arc<fauna_nest::bridge_rate_limit::Limiter>,
) -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, data_path.join("blobs"), None).unwrap(),
    );
    let blob_store = backup_svc.local_blob_store();

    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        backup_service: Some(backup_svc),
        config: Arc::new(fauna_nest::config::NestConfig {
            nest: fauna_nest::config::NestSection {
                db_path: data_path.join("nest.db").to_string_lossy().into_owned(),
                ..Default::default()
            },
            ..(*base.config).clone()
        }),
        // The nest's own segment store, under the same data dir.
        mail_segments: Arc::new(fauna_segment_store::SegmentManager::new(
            data_path.clone(),
            "mail",
        )),
        federation_rate_limit,
        // The placement journal under the same data dir, so a copy of the
        // data directory carries the journal with the content.
        mail_placement: Arc::new(fauna_nest::segments::MailPlacementSegmentManager::new(
            data_path.clone(),
        )),
        // The calendar and contacts stores and journals likewise, so the
        // lived-in recovery's DAV proofs can regress a nest by its data dir.
        cal_segments: Arc::new(fauna_segment_store::SegmentManager::new(
            data_path.clone(),
            "calendar",
        )),
        card_segments: Arc::new(fauna_segment_store::SegmentManager::new(
            data_path.clone(),
            "card",
        )),
        cal_placement: Arc::new(fauna_nest::segments::CalPlacementSegmentManager::new(
            data_path.clone(),
        )),
        card_placement: Arc::new(fauna_nest::segments::CardPlacementSegmentManager::new(
            data_path.clone(),
        )),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            backup_handlers::register_backup_handlers(&mut b);
            // The owner-authed custody door (`fauna.sync.changes.record`'s
            // reserved-set arm) — what the tests drive to stage custody on the
            // destination directly.
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            // The calendar and contacts doors a DAV client reaches — how the
            // every-kind tests write the corpora they back up.
            fauna_nest::bridge_caldav_handlers::register_bridge_caldav_handlers(&mut b);
            fauna_nest::bridge_carddav_handlers::register_bridge_carddav_handlers(&mut b);
            // `fauna.segments.list` — the recovery leg's skip reads it.
            fauna_nest::segments::register_segments_handlers(&mut b);
            // `fauna.auth.rotation_chain` — what an owner's device reads
            // before it carries a rotated box's writer seat.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        ..base
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state, blob_store)
}

/// One pass of the scheduler's automatic message-kind captures over `nest`,
/// as production arms it (`main.rs`, the run-now hook): it mints the owner's
/// `__<kind>` rail beside whatever the nest holds, so a test that runs it
/// stands where every nest holding mail stands a minute after it lands.
/// Returns how many captures it cut.
pub async fn capture_message_kinds(nest: &Arc<AppState>) -> usize {
    use fauna_nest::backup::message_kind::MessageKindStores;
    use fauna_nest::backup::scheduler::SnapshotScheduler;
    let svc = nest
        .backup_service
        .clone()
        .expect("the nest runs a backup service");
    SnapshotScheduler::new(svc, std::time::Duration::from_secs(60), 0)
        .with_message_kinds(MessageKindStores::of(nest))
        .check_all_message_kinds()
        .await
        .expect("the message-kind capture pass")
}

/// Relabel `OWNER`'s custody copy named `set_name` — a reserved-segment name —
/// to its landing name, and return the name the verb is to be called with.
/// Custody rows hang off the set's row, so the corpus is untouched. A name of
/// any other shape is returned as written. The verb flips only a landing set
/// (`reserved-folders.md` § __mail Sync → *The landing set*), while a nest
/// destination's delivery lands under `__<kind>`; the custody is byte-identical
/// whichever leg wrote it, so this stands the destination's copy in for the
/// one a client leg delivers.
pub async fn land(nest: &Arc<AppState>, set_name: &str) -> String {
    use fauna_sync_engine::segment_backup::{landing_set_name, parse_reserved_backup_set_name};
    let Some((kind, scope)) = parse_reserved_backup_set_name(set_name) else {
        return set_name.to_string();
    };
    let landing = landing_set_name(kind, &scope).expect("every reserved kind lands");
    let conn = nest.db.conn().await;
    conn.execute(
        "UPDATE folders SET name = ?1, name_hash = ?2 \
         WHERE name = ?3 AND actor_id = ?4 AND custody_copy = 1",
        rusqlite::params![
            landing,
            fauna_core::path_crypto::set_name_hash(&landing).as_slice(),
            set_name,
            OWNER.as_slice()
        ],
    )
    .expect("relabel the delivered copy as a landing set");
    landing
}

/// Dispatch a USER-class kind as `actor` — i.e. what the owner's own client
/// does over its own authenticated connection.
pub async fn client_call<Req: Serialize, Rep: DeserializeOwned>(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Rep, RpcError> {
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    let payload = bytes::Bytes::from(encode_canonical(req).unwrap().to_vec());
    let out = (meta.handler)(state.clone(), actor, payload).await?;
    Ok(decode(&out).unwrap())
}

/// The owner grants the same `NestBackupKey` to the nest they are seeding — the
/// enrollment-time grant phase 3 materializes under. Same 32 bytes as the source
/// holds, because it is one seed-derived root of the owner's, not a per-nest
/// secret.
pub async fn grant_key_to(nest: &Arc<AppState>) {
    let grant: NestKeyGrantReply = client_call(
        nest,
        OWNER,
        "fauna.backup.nest_key.grant",
        &NestKeyGrantRequest {
            nest_backup_key: serde_bytes::ByteBuf::from(GRANTED_KEY.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("the owner grants its NestBackupKey to the nest it is seeding");
    assert!(grant.ok);
}

/// Wire the enroll end-state: the owner grants its key + registers the
/// destination on the SOURCE, and registers the source as a writer at the
/// DESTINATION. This is the last moment a client is involved.
pub async fn enroll(source: &Arc<AppState>, dest_state: &Arc<AppState>, dest_url: &str) {
    enroll_source(source, dest_state, dest_url).await;
    let wg = grant_writer(source, dest_state)
        .await
        .expect("owner authorizes its source nest to write custody at the destination");
    assert!(wg.ok);
}

/// [`enroll`]'s source half: the seal grant and the destination registration,
/// both on the SOURCE. Without [`grant_writer`] the box knows where to back up
/// and holds no authority to write there.
pub async fn enroll_source(source: &Arc<AppState>, dest_state: &Arc<AppState>, dest_url: &str) {
    let grant: NestKeyGrantReply = client_call(
        source,
        OWNER,
        "fauna.backup.nest_key.grant",
        &NestKeyGrantRequest {
            nest_backup_key: serde_bytes::ByteBuf::from(GRANTED_KEY.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("owner grants its NestBackupKey to its source nest");
    assert!(grant.ok);

    let reg: DestinationRegisterReply = client_call(
        source,
        OWNER,
        "fauna.backup.destination.register",
        &DestinationRegisterRequest {
            destination_id: DEST_ID.to_string(),
            destination_nest_url: dest_url.to_string(),
            destination_nest_id: hex::encode(dest_state.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
    .expect("owner tells its source nest where to back up");
    assert!(reg.ok);
}

/// [`enroll`]'s destination half: the owner registers `source` as its writer at
/// the DESTINATION, over its own connection there.
pub async fn grant_writer(
    source: &Arc<AppState>,
    dest_state: &Arc<AppState>,
) -> Result<WriterGrantRegisterReply, RpcError> {
    client_call(
        dest_state,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
}

//! **The custodian-NEST serve door, end to end over two real nests** (tier_3;
//! `docs/goal/architecture/account-replica-posture.md` § Replica posture →
//! *The custody grant + ceremony*, the custodian-NEST serve door bullet,
//! rulings 1–4): an owner seals a state row on its own nest; a host's nest
//! pumps it into a custodied store as the NEST's own identity (the
//! `conformance_custody_hosting_pump_client.rs` rig); then an OWNER-FLEET
//! device — a fresh device key holding only a root-signed
//! `DeviceAuthorization`, with no account on the custodian nest — handshakes
//! there over `fauna.auth.custodied_fleet_handshake`, and its production
//! pull-only reading walk converges on the sealed row, bytes verbatim.
//!
//! Then every refusal the rulings name, and the one thing the door does not
//! refuse:
//!
//! - a cert signed by another account, and the host's own device's cert,
//!   refuse at the handshake (ruling 4);
//! - an owner whose only hosting row is expired refuses (ruling 4);
//! - a `conv` scope under an `Account` grant refuses LOUDLY on the feed, and a
//!   stranger's plane refuses on the segment list and the byte route
//!   (ruling 3's account binding + the coverage-enumeration corollary);
//! - a STOPPED row still serves (ruling 4);
//! - a grant naming the device as removed severs its live session at the next
//!   request and refuses its next handshake, while a sibling device keeps
//!   reading (ruling 3's exclusion union);
//! - the host's remove ends the class at the next dispatch and drops the
//!   register (ruling 3's teardown).
//!
//! Every assertion is on latency-independent state (e2e convention 14); the
//! pump is driven by direct `run_once` pokes, never a wall clock.

mod common;
use common::{connected_client, wait_for_connected};

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_client::NestClient;
use fauna_client::ws_custodied_fleet_bearer::custodied_fleet_nest_client;
use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
use fauna_client_capabilities::custody_hosting::{CustodyHostingClient, HostingDeposit};
use fauna_client_capabilities::grant_log::{self, PublishedGrants};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
};
use fauna_core::data::{Capability, DeviceAuthorization, ModerationConfig, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::backup::service::BackupService;
use fauna_nest::custody_hosting_worker::CustodyHostingWorker;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::merge_policy::{KIND_MODERATION, MODERATION_KEY};
use fauna_protocol::segments::{SegmentsListReply, SegmentsListRequest};
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;

const OWNER_SEED: [u8; 32] = [0x6A; 32];
const OTHER_OWNER_SEED: [u8; 32] = [0x6B; 32];
const HOST_SEED: [u8; 32] = [0x6C; 32];
const STRANGER_SEED: [u8; 32] = [0x6D; 32];
/// The custodian NEST's deployment seed — its public key is the witness's
/// `custodian_key`.
const CUSTODIAN_NEST_SEED: [u8; 32] = [0xCF; 32];
const GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x4A; CUSTODY_GRANT_ID_LEN];
/// The re-minted grant that names device D as removed.
const REMOVAL_GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x4B; CUSTODY_GRANT_ID_LEN];
const EXPIRED_GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x4C; CUSTODY_GRANT_ID_LEN];

fn owner() -> ActorKeypair {
    ActorKeypair::from_secret(OWNER_SEED)
}

fn host() -> ActorKeypair {
    ActorKeypair::from_secret(HOST_SEED)
}

fn custodian_nest_pub() -> [u8; 32] {
    SigningKey::from_bytes(&CUSTODIAN_NEST_SEED)
        .verifying_key()
        .to_bytes()
}

/// The OWNER's nest: the full custody serve-door surface the pump pulls from.
async fn start_owner_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    start_owner_nest_signing(None).await
}

/// [`start_owner_nest`], with `deployment` as the deployment identity the
/// generation-escrow doors sign receipts with — the holder a minting owner
/// device trusts.
async fn start_owner_nest_signing(
    deployment: Option<SigningKey>,
) -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
    );
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&secret)),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            fauna_nest::segments::register_segments_handlers(&mut b);
            fauna_nest::custody_receipt_handlers::register_custody_receipt_handlers(&mut b);
            fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        nest_signing_key: deployment,
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

/// The CUSTODIAN nest: hosting doors, the pump's root, and the serve door's
/// two WS kinds (the byte route rides `build_router`).
async fn start_custodian_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&CUSTODIAN_NEST_SEED)),
        custody_hosting_root: Some(tmp.path().join("custody-hosting")),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::custody_hosting_handlers::register_custody_hosting_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::segments::register_segments_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

/// An owner-signed custody witness naming the custodian NEST.
fn witness(
    signer: &ActorKeypair,
    grant_id: [u8; CUSTODY_GRANT_ID_LEN],
    expires_at: Timestamp,
    removed_devices: Vec<[u8; 32]>,
) -> EmbedAsBytes {
    sign_custody_grant(
        signer,
        &CustodyGrant {
            grant_id: grant_id.to_vec(),
            owner: signer.actor_id(),
            custodian_key: custodian_nest_pub(),
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at,
            removed_devices,
        },
    )
    .expect("sign witness")
}

const FAR_FUTURE: Timestamp = Timestamp(9_000_000_000_000_000);

/// A root-signed `DeviceAuthorization` for `device` under `account` — the
/// cert a fleet device carries.
fn device_cert(account: &ActorKeypair, device: [u8; 32]) -> EmbedAsBytes {
    let cert = DeviceAuthorization {
        actor_id: account.actor_id(),
        device_key: device,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(account, &cert).unwrap();
    EmbedAsBytes::from_signed(bytes, env)
}

/// Mint the custody capability row on the OWNER's nest — the pump's
/// handshake there needs it (the pump test's recipe).
async fn mint_custody_row(nest: &Arc<NestClient>) {
    let now = Timestamp::now_secs().max(0) as u64;
    let set = CustodyScopeSet::Account;
    let undeposited = custody_mint_blob(
        &owner().actor_id().0,
        &GRANT_ID,
        &custodian_nest_pub(),
        GrantWindow(now, now + 90 * 24 * 3600),
        &set,
    )
    .expect("keyless custody blob");
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(owner().actor_id());
    grant_log::record_mint(
        &mut log,
        owner().signing_key(),
        GRANT_ID,
        custodian_nest_pub(),
        custody_event_scopes(&set),
        now,
        now + 90 * 24 * 3600,
        now,
    )
    .expect("record the Mint event");
    let blob_bytes = undeposited
        .release(&PublishedGrants::from_published(
            &fauna_client_config::PublishedLedger::acknowledged_for_test(log),
        ))
        .expect("released against the published log");
    let reply = CapabilitiesClient::new(Arc::clone(nest))
        .mint(blob_bytes)
        .await
        .expect("mint RPC");
    assert!(reply.ok, "the custody row deposited");
}

/// The host's register door, one deposit.
async fn deposit(
    hosting: &CustodyHostingClient<Arc<NestClient>>,
    grant_id: [u8; CUSTODY_GRANT_ID_LEN],
    witness: &EmbedAsBytes,
    owner_nest_url: &str,
    stopped: bool,
) {
    let reply = hosting
        .register(&HostingDeposit {
            grant_id: grant_id.to_vec(),
            owner: owner().actor_id().0,
            witness: fauna_core::encoding::canonical_encode(witness)
                .unwrap()
                .to_vec(),
            owner_nest_url: owner_nest_url.to_string(),
            owner_devices: Vec::new(),
            retained_bytes_cap: 8 * 1024 * 1024 * 1024,
            stopped,
        })
        .await
        .expect("hosting register RPC");
    assert!(reply.ok, "the hosting row deposited");
}

/// A fleet device's handshake at the custodian nest, alone — `Err` carries
/// the nest's code.
async fn handshake(
    base: &str,
    claimed_owner: [u8; 32],
    device: &SigningKey,
    cert: &EmbedAsBytes,
) -> Result<String, String> {
    fauna_anon_client::mint_bearer_over_custodied_fleet_handshake(base, claimed_owner, device, cert)
        .await
        .map(|minted| minted.token)
        .map_err(|e| match e {
            fauna_anon_client::AnonClientError::Rpc(rpc) => rpc.code,
            other => format!("{other}"),
        })
}

/// One account-state feed page over a connected client.
async fn state_page(nest: &Arc<NestClient>) -> Result<SyncChangesListReply, String> {
    nest.request::<_, SyncChangesListReply>(
        "fauna.sync.changes.list",
        SyncChangesListRequest {
            item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
            scope: Some(ACCOUNT_STATE_SCOPE.to_string()),
            frontier: Some(Default::default()),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| format!("{e}"))
}

/// The nest's code on a refused request.
fn code(e: &fauna_client::NestClientError) -> &str {
    match e {
        fauna_client::NestClientError::Rpc(e) => e.code.as_str(),
        _ => "<not an RPC refusal>",
    }
}

/// A connected fleet client for `device`.
async fn fleet_client(base: &str, device: &SigningKey) -> Arc<NestClient> {
    let nest = custodied_fleet_nest_client(
        base,
        owner().actor_id().0,
        device.clone(),
        device_cert(&owner(), device.verifying_key().to_bytes()),
    );
    nest.connect().await.expect("fleet bearer connects");
    wait_for_connected(&nest).await;
    nest
}

/// What the setup both tests share leaves standing: two nests, the owner's
/// one sealed row on its own nest, and the custodian nest's pump having
/// pulled it into the custodied store as the NEST's own identity.
struct HeldRow {
    owner_base: String,
    host_base: String,
    host_state: Arc<AppState>,
    host_client: Arc<NestClient>,
    hosting: CustodyHostingClient<Arc<NestClient>>,
    schedule: AccountStateKeySchedule,
    trust: GenerationTrust,
    /// What the custodied store holds for the owner, verbatim.
    held: Vec<fauna_account_store::types::RelayRow>,
    _tmps: Vec<tempfile::TempDir>,
    _owner_state: Arc<AppState>,
}

async fn hold_an_owner_row() -> HeldRow {
    let (owner_base, owner_state, _owner_tmp) = start_owner_nest().await;
    let (host_base, host_state, host_tmp) = start_custodian_nest().await;

    // ── The owner seals one row through the REAL writer door ──────────────
    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&owner_base, owner()).await;
    let owner_store_dir = tempfile::tempdir().unwrap();
    let writer_key = SigningKey::from_bytes(&[0x0C; 32]);
    let writer_pub = writer_key.verifying_key().to_bytes();
    let owner_store = AccountStore::open(
        SqliteBackend::open(owner_store_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(writer_pub),
    )
    .await
    .unwrap();
    let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(owner().secret_bytes()));
    let trust = GenerationTrust {
        root: owner().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    };
    AccountStatePlane::new(
        &owner_store,
        &owner_nest,
        &schedule,
        &writer_key,
        &trust,
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap()
    .put(
        &ItemId {
            kind: KIND_MODERATION.into(),
            key: MODERATION_KEY.into(),
        },
        fauna_core::encoding::canonical_encode(&ModerationConfig {
            muted_keywords: vec!["read-back-from-the-custodian-nest".into()],
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
        Some(
            fauna_protocol::merge_policy::LwwStamp {
                at_ms: 1_000,
                writer: writer_pub,
            }
            .encode()
            .unwrap(),
        ),
    )
    .await
    .expect("the owner's row seals and publishes to its nest");
    mint_custody_row(&owner_nest).await;

    // ── The host deposits; the custodian nest's pump pulls as ITSELF ──────
    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    let hosting = CustodyHostingClient::new(Arc::clone(&host_client));
    let grant_witness = witness(&owner(), GRANT_ID, FAR_FUTURE, Vec::new());
    deposit(&hosting, GRANT_ID, &grant_witness, &owner_base, false).await;
    let worker = CustodyHostingWorker::new(Arc::clone(&host_state), Duration::MAX);
    let report = worker.run_once().await.expect("pump pass");
    assert_eq!(
        report.pulled, 1,
        "the pump pulled the owner's plane: {report:?}"
    );

    let custodied = AccountStore::open(
        SqliteBackend::open(
            host_tmp
                .path()
                .join("custody-hosting")
                .join(hex::encode(host().actor_id().0))
                .join(owner().actor_id_hex()),
        )
        .unwrap(),
        &owner().actor_id_hex(),
        WriterId(custodian_nest_pub()),
    )
    .await
    .unwrap();
    let held = custodied
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap();
    assert!(!held.is_empty(), "the custodian nest holds the owner's row");
    HeldRow {
        owner_base,
        host_base,
        host_state,
        host_client,
        hosting,
        schedule,
        trust,
        held,
        _tmps: vec![_owner_tmp, host_tmp, owner_store_dir],
        _owner_state: owner_state,
    }
}

#[tokio::test]
async fn an_owner_device_reads_back_what_a_custodian_nest_holds_and_every_refusal_holds() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let HeldRow {
        owner_base,
        host_base,
        host_state,
        host_client,
        hosting,
        schedule,
        trust,
        held,
        // Bound, not `..`: unbound fields of the temporary drop at the end of
        // this statement, and the host tempdir holds the custodied store.
        _tmps: _keep_tmps,
        _owner_state: _keep_owner_nest,
        ..
    } = hold_an_owner_row().await;

    // ── Device D: the owner's own fleet, reading back over the door ───────
    let device_d = SigningKey::from_bytes(&[0xD1; 32]);
    let d_pub = device_d.verifying_key().to_bytes();
    let fleet_d = fleet_client(&host_base, &device_d).await;
    assert_eq!(
        host_state
            .db
            .get_custody_hosting_fleet(&d_pub)
            .await
            .unwrap()
            .map(|row| row.owner_actor_id),
        Some(owner().actor_id().0.to_vec()),
        "the handshake wrote the device's register row under its owner"
    );

    let device_store_dir = tempfile::tempdir().unwrap();
    let device_store = AccountStore::open(
        SqliteBackend::open(device_store_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(d_pub),
    )
    .await
    .unwrap();
    let plane = AccountStatePlane::new_pull_only(
        &device_store,
        &fleet_d,
        &schedule,
        &device_d,
        &trust,
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap();
    plane
        .walk()
        .await
        .expect("the production reading walk runs against the custodian nest");
    let read_back = device_store
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap();
    let held_entries: Vec<_> = held.iter().map(|r| r.entry.clone()).collect();
    let read_entries: Vec<_> = read_back.iter().map(|r| r.entry.clone()).collect();
    assert_eq!(
        read_entries, held_entries,
        "the device holds exactly what the custodian nest holds — sealed bytes verbatim"
    );
    assert!(
        plane
            .open_relay_row(&read_back[0])
            .await
            .expect("open")
            .is_some(),
        "the row read back opens under the owner's own key schedule"
    );

    // ── The coverage refusals: loud, never an empty page ──────────────────
    let stranger = ActorKeypair::from_secret(STRANGER_SEED);
    let conv = fauna_protocol::scope::ContentScope::new("conv", stranger.actor_id().0)
        .unwrap()
        .to_string();
    let conv_err = fleet_d
        .request::<_, SyncChangesListReply>(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some(ItemClass::RecordCid.as_wire().to_string()),
                scope: Some(conv),
                ..Default::default()
            },
        )
        .await
        .expect_err("a conv scope under an Account grant refuses loudly");
    assert_eq!(
        code(&conv_err),
        "fauna.sync.permission_denied",
        "{conv_err:?}"
    );
    let own_posts = fleet_d
        .request::<_, SegmentsListReply>(
            "fauna.segments.list",
            SegmentsListRequest {
                kind: "post".into(),
                actor_id: owner().actor_id_hex(),
                extra: Default::default(),
            },
        )
        .await
        .expect("the owner's own post plane lists (nothing held yet)");
    assert!(own_posts.segments.is_empty());
    let stranger_posts = fleet_d
        .request::<_, SegmentsListReply>(
            "fauna.segments.list",
            SegmentsListRequest {
                kind: "post".into(),
                actor_id: stranger.actor_id_hex(),
                extra: Default::default(),
            },
        )
        .await
        .expect_err("a stranger's plane refuses");
    assert_eq!(
        code(&stranger_posts),
        "fauna.segments.not_owner",
        "{stranger_posts:?}"
    );
    let token = handshake(
        &host_base,
        owner().actor_id().0,
        &device_d,
        &device_cert(&owner(), d_pub),
    )
    .await
    .expect("a second handshake mints");
    let http = reqwest::Client::new();
    let byte_status = |actor_hex: String| {
        let http = http.clone();
        let token = token.clone();
        let base = host_base.clone();
        async move {
            http.get(format!("{base}/api/v1/segments/post/{actor_hex}/0"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(
        byte_status(owner().actor_id_hex()).await,
        404,
        "the owner's own plane: admitted, nothing held"
    );
    assert_eq!(
        byte_status(stranger.actor_id_hex()).await,
        403,
        "a stranger's plane: the same 403 a non-owner gets"
    );

    // ── Handshake refusals (ruling 4), one opaque code each ───────────────
    let s_key = SigningKey::from_bytes(&[0x5E; 32]);
    let s_pub = s_key.verifying_key().to_bytes();
    assert_eq!(
        handshake(
            &host_base,
            owner().actor_id().0,
            &s_key,
            &device_cert(&stranger, s_pub)
        )
        .await,
        Err("fauna.auth.not_registered".into()),
        "a cert another account signed does not verify under the owner"
    );
    let h_key = SigningKey::from_bytes(&[0x4E; 32]);
    let h_pub = h_key.verifying_key().to_bytes();
    assert_eq!(
        handshake(
            &host_base,
            owner().actor_id().0,
            &h_key,
            &device_cert(&host(), h_pub)
        )
        .await,
        Err("fauna.auth.not_registered".into()),
        "the host's own device is not the owner's fleet"
    );
    assert_eq!(
        handshake(
            &host_base,
            host().actor_id().0,
            &h_key,
            &device_cert(&host(), h_pub)
        )
        .await,
        Err("fauna.auth.not_registered".into()),
        "nor does it hold a custody of its own here"
    );
    // An owner whose only hosting row is EXPIRED: planted past the register
    // door (which refuses an expired witness), as a row already at rest.
    let other_owner = ActorKeypair::from_secret(OTHER_OWNER_SEED);
    host_state
        .db
        .put_custody_hosting(
            &host().actor_id().0,
            &EXPIRED_GRANT_ID,
            &other_owner.actor_id().0,
            &fauna_core::encoding::canonical_encode(&witness(
                &other_owner,
                EXPIRED_GRANT_ID,
                Timestamp(2_000),
                Vec::new(),
            ))
            .unwrap(),
            &owner_base,
            b"",
            4096,
            false,
        )
        .await
        .unwrap();
    let o_key = SigningKey::from_bytes(&[0x0E; 32]);
    let o_pub = o_key.verifying_key().to_bytes();
    assert_eq!(
        handshake(
            &host_base,
            other_owner.actor_id().0,
            &o_key,
            &device_cert(&other_owner, o_pub)
        )
        .await,
        Err("fauna.auth.not_registered".into()),
        "an expired grant is re-minted, never served past its window"
    );

    // ── A STOPPED row still serves (ruling 4) ─────────────────────────────
    let grant_witness = witness(&owner(), GRANT_ID, FAR_FUTURE, Vec::new());
    deposit(&hosting, GRANT_ID, &grant_witness, &owner_base, true).await;
    assert!(
        !state_page(&fleet_d)
            .await
            .expect("a stopped row still serves its owner")
            .changes
            .is_empty()
    );

    // ── A grant naming D as removed severs D, never its sibling E ─────────
    let device_e = SigningKey::from_bytes(&[0xE1; 32]);
    let fleet_e = fleet_client(&host_base, &device_e).await;
    assert!(
        state_page(&fleet_e).await.is_ok(),
        "E reads before the removal"
    );
    deposit(
        &hosting,
        REMOVAL_GRANT_ID,
        &witness(&owner(), REMOVAL_GRANT_ID, FAR_FUTURE, vec![d_pub]),
        &owner_base,
        false,
    )
    .await;
    assert!(
        state_page(&fleet_d).await.is_err(),
        "the removal severs D's live session at its next request"
    );
    assert_eq!(
        handshake(
            &host_base,
            owner().actor_id().0,
            &device_d,
            &device_cert(&owner(), d_pub)
        )
        .await,
        Err("fauna.auth.not_registered".into()),
        "and refuses its next handshake"
    );
    assert!(
        state_page(&fleet_e).await.is_ok(),
        "the sibling E still reads"
    );

    // ── The host's remove ends the class and drops the register ───────────
    for grant in [GRANT_ID, REMOVAL_GRANT_ID] {
        assert!(
            hosting.remove(&grant).await.expect("remove RPC").removed,
            "the host removes its row"
        );
    }
    assert!(
        state_page(&fleet_e).await.is_err(),
        "the remove ends E's class at its next dispatch"
    );
    let e_pub = device_e.verifying_key().to_bytes();
    assert!(
        host_state
            .db
            .get_custody_hosting_fleet(&e_pub)
            .await
            .unwrap()
            .is_none(),
        "the register falls with the owner's last hosting row"
    );
    host_client.disconnect().await;
}

/// **The owner-fleet restore leg** (ruling 6): an owner device that holds a
/// nest-form `custodian-endpoints` row — and NO sibling row for the device
/// that wrote the data — dials the custodian nest only on a pass whose own
/// nest walk did not answer, and converges on the owner's row through it. With
/// its own nest reachable it makes zero nest-custodian dials (the custodian
/// nest records no handshake for it); a row under a revoked grant is never
/// dialed at all (its URL's listener sees no connection), whatever the pass.
#[tokio::test]
async fn an_owner_device_restores_through_a_custodian_nest_only_when_its_own_nest_is_down() {
    use fauna_sync_engine::custodian_nest_restore::{NestArmTrigger, nest_custodian_arm};
    use fauna_sync_engine::peer_leg::WithdrawalSnapshot;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let HeldRow {
        host_base,
        host_state,
        host_client,
        schedule,
        trust,
        held,
        _tmps: _keep_tmps,
        _owner_state: _keep_owner_nest,
        ..
    } = hold_an_owner_row().await;

    // A URL whose every accepted connection is counted — the revoked grant's
    // custodian. Nothing may ever connect to it.
    let revoked_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let revoked_url = format!(
        "http://127.0.0.1:{}",
        revoked_listener.local_addr().unwrap().port()
    );
    let revoked_dials = Arc::new(AtomicUsize::new(0));
    {
        let revoked_dials = Arc::clone(&revoked_dials);
        tokio::spawn(async move {
            while revoked_listener.accept().await.is_ok() {
                revoked_dials.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    const REVOKED_GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x4D; CUSTODY_GRANT_ID_LEN];

    // ── Device B: a fresh fleet device whose only lead is the registry row ─
    let device_b = SigningKey::from_bytes(&[0xB2; 32]);
    let b_pub = device_b.verifying_key().to_bytes();
    let b_cert = device_cert(&owner(), b_pub);
    let b_dir = tempfile::tempdir().unwrap();
    let b_store = AccountStore::open(
        SqliteBackend::open(b_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(b_pub),
    )
    .await
    .unwrap();
    // The registry rows the custody ceremony left in the owner's fleet plane,
    // staged as merged state (how they arrived is not this test's subject;
    // the peer leg's dial-list test stages them the same way).
    for (grant_id, url) in [
        (GRANT_ID, host_base.clone()),
        (REVOKED_GRANT_ID, revoked_url.clone()),
    ] {
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS;
        b_store
            .put_state(fauna_account_store::types::StateEntry {
                kind: kind.into(),
                key: fauna_core::custody_grant::custody_entry_key(&grant_id),
                scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                    .unwrap()
                    .into(),
                value: fauna_core::encoding::canonical_encode(
                    &fauna_core::custodian_endpoints::CustodianEndpoints {
                        grant_id: grant_id.to_vec(),
                        endpoints: fauna_core::device_endpoints::DeviceEndpoints {
                            node_id: custodian_nest_pub(),
                            ..Default::default()
                        },
                        custodian_nest_url: Some(url),
                        ..Default::default()
                    },
                )
                .unwrap()
                .to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .expect("the custodian-endpoints row");
    }
    // The dialer's revocation view: the second grant is revoked.
    let revoked = WithdrawalSnapshot::<Vec<u8>>::underived();
    revoked.replace([REVOKED_GRANT_ID.to_vec()].into_iter().collect());

    let arm = |trigger| {
        nest_custodian_arm(
            &b_store,
            owner().actor_id().0,
            &schedule,
            &trust,
            &device_b,
            &b_cert,
            None,
            &[],
            &revoked,
            trigger,
        )
    };

    // ── Own nest reachable: the arm does not run ──────────────────────────
    let standing = arm(NestArmTrigger::Pass(fauna_transport::NestPath::Reachable))
        .await
        .expect("a reachable-nest pass");
    assert_eq!(
        (standing.dialed, standing.admitted, standing.withheld),
        (0, 0, 0),
        "no standing dial while the own nest answers: {standing:?}"
    );
    assert!(
        host_state
            .db
            .get_custody_hosting_fleet(&b_pub)
            .await
            .unwrap()
            .is_none(),
        "the custodian nest saw no handshake from B"
    );
    assert!(b_state_rows(&b_store).await.is_empty(), "B learned nothing");

    // ── Own nest unavailable: the restore dial converges ──────────────────
    let restore = arm(NestArmTrigger::Pass(fauna_transport::NestPath::Unavailable))
        .await
        .expect("an unavailable-nest pass");
    assert_eq!(
        (restore.dialed, restore.admitted, restore.withheld),
        (1, 1, 1),
        "the live grant's nest is dialed and admitted; the revoked one is withheld: {restore:?}"
    );
    assert!(restore.applied >= 1, "rows merged: {restore:?}");
    assert_eq!(
        host_state
            .db
            .get_custody_hosting_fleet(&b_pub)
            .await
            .unwrap()
            .map(|row| row.owner_actor_id),
        Some(owner().actor_id().0.to_vec()),
        "B handshook at the custodian nest as the owner's fleet device"
    );
    let read_back = b_state_rows(&b_store).await;
    assert_eq!(
        read_back
            .iter()
            .map(|r| r.entry.clone())
            .collect::<Vec<_>>(),
        held.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
        "B holds exactly what the custodian nest holds — A's sealed row, verbatim"
    );
    assert_eq!(
        revoked_dials.load(Ordering::SeqCst),
        0,
        "the revoked grant's custodian was never dialed"
    );
    host_client.disconnect().await;
}

/// The account-state relay rows `store` holds.
async fn b_state_rows(
    store: &AccountStore<SqliteBackend>,
) -> Vec<fauna_account_store::types::RelayRow> {
    store
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap()
}

/// The owner nest's deployment identity — the escrow holder the owner's
/// device trusts, and the signer of every receipt in its fleet plane.
fn owner_deployment_key() -> SigningKey {
    SigningKey::from_bytes(&[0x6E; 32])
}

/// A device's self-signed `fauna.state.device-set` enrollment under `account`.
fn enrollment_value(device: &SigningKey, account: &ActorKeypair) -> Vec<u8> {
    let authorization = fauna_core::encoding::canonical_encode(&device_cert(
        account,
        device.verifying_key().to_bytes(),
    ))
    .unwrap();
    fauna_core::encoding::canonical_encode(&fauna_core::generation::sign_device_enrollment(
        device,
        authorization,
        5_000,
    ))
    .unwrap()
}

/// **Escrow custody, end to end** (`account-replica-posture.md` § Replica
/// posture → the custodian-NEST serve door bullet → *Escrow custody*, rulings
/// (1)–(2), and bound (e) as amended): the owner's device mints a generation
/// through its nest's real escrow door and seals a `GenerationTip` row under
/// it; the custodian nest pumps the owner's planes; then the owner's nest and
/// device are gone, and a FRESH device holding only the identity seed — no
/// pin naming the holder that signed the receipts, no holder door on the
/// custodian — restores through the custodied-fleet door, keys the generation
/// from the restored `fauna.state.escrow-wrap` row and opens the content row
/// the lost fleet sealed: no fleet relay row stays sealed under a generation
/// it cannot key.
#[tokio::test]
async fn a_seed_only_device_keys_a_custodian_restore_from_the_wrap_row_and_opens_the_content() {
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
    use fauna_protocol::merge_policy::{
        KIND_CUSTODIAN_ENDPOINTS, KIND_DEVICE_ENDPOINTS, KIND_DEVICE_SET, KIND_ESCROW_TARGET,
        KIND_ESCROW_WRAP, LwwStamp, home_scope_for_kind,
    };
    use fauna_sync_engine::cold_replica::MemoryRetainedKeys;
    use fauna_sync_engine::custodian_nest_restore::{NestArmTrigger, nest_custodian_arm};
    use fauna_sync_engine::generation_escrow_recover::{
        EscrowRecoveryMemo, EscrowRecoveryPass, ensure_recovered,
    };
    use fauna_sync_engine::generation_tip::RetainedKeyCustody;
    use fauna_sync_engine::peer_leg::WithdrawalSnapshot;

    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (owner_base, owner_state, owner_tmp) =
        start_owner_nest_signing(Some(owner_deployment_key())).await;
    let (host_base, host_state, host_tmp) = start_custodian_nest().await;

    // ── The owner's device: enroll, publish the escrow target, seal ───────
    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&owner_base, owner()).await;
    let owner_store_dir = tempfile::tempdir().unwrap();
    let device_a = SigningKey::from_bytes(&[0x0C; 32]);
    let a_pub = device_a.verifying_key().to_bytes();
    let owner_store = AccountStore::open(
        SqliteBackend::open(owner_store_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(a_pub),
    )
    .await
    .unwrap();
    let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(owner().secret_bytes()));
    let owner_trust = GenerationTrust {
        root: owner().actor_id(),
        prior: Vec::new(),
        trusted_holders: vec![owner_deployment_key().verifying_key().to_bytes()].into(),
    };
    let fleet_a = AccountStatePlane::new(
        &owner_store,
        &owner_nest,
        &schedule,
        &device_a,
        &owner_trust,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap();
    fleet_a
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(a_pub),
            },
            enrollment_value(&device_a, &owner()),
            None,
        )
        .await
        .expect("A enrolls");
    let target = fauna_sync_engine::generation_mint::escrow_target_entry(&OWNER_SEED).unwrap();
    fleet_a
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: target.key,
            },
            target.value,
            None,
        )
        .await
        .expect("the seed-holding surface publishes the escrow target");
    let endpoints =
        fauna_core::encoding::canonical_encode(&fauna_core::device_endpoints::DeviceEndpoints {
            node_id: a_pub,
            lan_addrs: vec!["192.168.1.12:4433".into()],
            ..Default::default()
        })
        .unwrap()
        .to_vec();
    // The first tip-sealed write mints generation 1 through the owner nest's
    // real escrow door — the mint row, the wrap row and the receipt — and
    // then seals under it.
    fleet_a
        .put(
            &ItemId {
                kind: KIND_DEVICE_ENDPOINTS.into(),
                key: hex::encode(a_pub),
            },
            endpoints.clone(),
            Some(
                LwwStamp {
                    at_ms: 1_000,
                    writer: a_pub,
                }
                .encode()
                .unwrap(),
            ),
        )
        .await
        .expect("the content row mints, seals and publishes");
    let wraps_on_a = owner_store.states_of_kind(KIND_ESCROW_WRAP).await.unwrap();
    assert_eq!(wraps_on_a.len(), 1, "the mint wrote the wrap row");
    mint_custody_row(&owner_nest).await;

    // ── The custodian nest's pump pulls both account-state planes ─────────
    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    let hosting = CustodyHostingClient::new(Arc::clone(&host_client));
    let grant_witness = witness(&owner(), GRANT_ID, FAR_FUTURE, Vec::new());
    deposit(&hosting, GRANT_ID, &grant_witness, &owner_base, false).await;
    let worker = CustodyHostingWorker::new(Arc::clone(&host_state), Duration::MAX);
    let report = worker.run_once().await.expect("pump pass");
    assert_eq!(
        report.pulled, 1,
        "the pump pulled the owner's planes: {report:?}"
    );

    // ── Every device and the nest are lost ────────────────────────────────
    owner_nest.disconnect().await;
    drop(fleet_a);
    drop(owner_store);
    drop(owner_store_dir);

    // ── B: a fresh device, the identity seed, the custodian's address ─────
    let device_b = SigningKey::from_bytes(&[0xB3; 32]);
    let b_pub = device_b.verifying_key().to_bytes();
    let b_cert = device_cert(&owner(), b_pub);
    let b_dir = tempfile::tempdir().unwrap();
    let b_store = AccountStore::open(
        SqliteBackend::open(b_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(b_pub),
    )
    .await
    .unwrap();
    b_store
        .put_state(fauna_account_store::types::StateEntry {
            kind: KIND_CUSTODIAN_ENDPOINTS.into(),
            key: fauna_core::custody_grant::custody_entry_key(&GRANT_ID),
            scope: home_scope_for_kind(KIND_CUSTODIAN_ENDPOINTS)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::custodian_endpoints::CustodianEndpoints {
                    grant_id: GRANT_ID.to_vec(),
                    endpoints: fauna_core::device_endpoints::DeviceEndpoints {
                        node_id: custodian_nest_pub(),
                        ..Default::default()
                    },
                    custodian_nest_url: Some(host_base.clone()),
                    ..Default::default()
                },
            )
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    // A cold device pins no holder: nothing in its trust names the box that
    // signed the account's receipts.
    let b_trust = GenerationTrust {
        root: owner().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    };
    let keys = MemoryRetainedKeys::default();
    let revoked = WithdrawalSnapshot::<Vec<u8>>::underived();
    revoked.replace(Default::default());
    let restore = nest_custodian_arm(
        &b_store,
        owner().actor_id().0,
        &schedule,
        &b_trust,
        &device_b,
        &b_cert,
        Some(&keys),
        &[],
        &revoked,
        NestArmTrigger::RestoreGesture,
    )
    .await
    .expect("the restore gesture");
    assert_eq!(
        (restore.dialed, restore.admitted),
        (1, 1),
        "the custodian is dialed and admitted: {restore:?}"
    );
    assert_eq!(
        b_store.states_of_kind(KIND_ESCROW_WRAP).await.unwrap(),
        wraps_on_a,
        "the custody pull carried the wrap row, verbatim"
    );
    assert!(
        b_store
            .state(KIND_DEVICE_ENDPOINTS, &hex::encode(a_pub))
            .await
            .unwrap()
            .is_none(),
        "before the escrow pass, the content row is sealed to B"
    );

    // ── The escrow-recovery pass: plane first, no holder door ─────────────
    let b_fleet_client = fleet_client(&host_base, &device_b).await;
    let b_fleet = AccountStatePlane::new_pull_only(
        &b_store,
        &b_fleet_client,
        &schedule,
        &device_b,
        &b_trust,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap()
    .with_generation_custody(&keys);
    let recovered = ensure_recovered(
        &b_store,
        &b_fleet,
        &b_trust,
        &device_b,
        &OWNER_SEED,
        &[],
        &EscrowRecoveryMemo::default(),
        &[],
    )
    .await
    .expect("the pass needs no door");
    assert_eq!(recovered, EscrowRecoveryPass::Recovered(1));

    // ── The pass that keys a generation re-presents the scope ─────────────
    b_fleet.reconcile().await.expect("the re-presenting walk");
    let content = b_store
        .state(KIND_DEVICE_ENDPOINTS, &hex::encode(a_pub))
        .await
        .unwrap()
        .expect("B opens the content row the lost fleet sealed");
    assert_eq!(content.value, endpoints);
    let sealed_to_b = b_store
        .relay_rows(
            ACCOUNT_STATE_FLEET_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap()
        .into_iter()
        .filter_map(|row| {
            row.entry
                .as_deref()
                .and_then(fauna_core::account_entry_crypto::peek_generation_id)
        })
        .filter(|g| keys.retained_generation_key(g).is_none())
        .count();
    assert_eq!(
        sealed_to_b, 0,
        "no fleet row stays sealed under an unkeyed generation"
    );

    b_fleet_client.disconnect().await;
    host_client.disconnect().await;
    drop((owner_tmp, host_tmp, owner_state));
}

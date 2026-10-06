//! **The custody-hosting PUMP, end to end over two real nests** (tier_3; the
//! custodian-nest runtime's stage (b) —
//! `docs/goal/architecture/account-data-plane.md` § Replica posture → *The
//! custody grant + ceremony*, the device-or-nest bullet, item 6): an owner
//! seals a state row through the REAL writer door on its own nest and mints
//! the custody capability row; a host on a SECOND nest deposits the hosting
//! row over the production register door; that nest's pump — authenticating
//! as the NEST's own identity, the key the witness names — pulls the owner's
//! sealed plane into a keyless custodied store **with no host device
//! running** (nothing client-side exists for the host after the deposit).
//!
//! The severed arm re-runs the pump after the owner revokes the capability
//! row: the fresh handshake takes its honest refusal at connect, the pass
//! reports the row failed, and the custodied store gains nothing.
//!
//! Every assertion is on latency-independent state (e2e convention 14); the
//! pump is driven by direct `run_once` pokes, never a wall clock.

mod common;
use common::connected_client;

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_client::NestClient;
use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
use fauna_client_capabilities::custody_hosting::{CustodyHostingClient, HostingDeposit};
use fauna_client_capabilities::grant_log::{self, RecordedGrants};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
};
use fauna_core::data::{ModerationConfig, Timestamp};
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
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;

const OWNER_SEED: [u8; 32] = [0x7A; 32];
const HOST_SEED: [u8; 32] = [0x7C; 32];
/// The custodian NEST's deployment seed — its public key is the witness's
/// `custodian_key`, exactly as a bound device principal would be.
const CUSTODIAN_NEST_SEED: [u8; 32] = [0xCE; 32];
const GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x3F; CUSTODY_GRANT_ID_LEN];

fn owner() -> ActorKeypair {
    ActorKeypair::from_secret(OWNER_SEED)
}

fn host() -> ActorKeypair {
    ActorKeypair::from_secret(HOST_SEED)
}

fn custodian_nest_key() -> SigningKey {
    SigningKey::from_bytes(&CUSTODIAN_NEST_SEED)
}

/// The OWNER's nest: the full custody serve-door surface (the W8.6 (account-data-plane.md § Workstreams) rig).
async fn start_owner_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

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

/// The CUSTODIAN nest: hosting doors + the pump's root, under a KNOWN
/// deployment seed so the owner's witness can name its identity.
async fn start_custodian_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

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

/// The owner-signed witness naming the CUSTODIAN NEST's identity.
fn nest_witness() -> fauna_core::encoding::EmbedAsBytes {
    sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id(),
            custodian_key: custodian_nest_key().verifying_key().to_bytes(),
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000_000_000_000_000),
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness")
}

/// Mint the custody capability row on the OWNER's nest over the real verbs
/// (record-then-deposit) — the W8.2 recipe, custodian = the nest identity.
async fn mint_custody_row(nest: &Arc<NestClient>) {
    let now = Timestamp::now_secs().max(0) as u64;
    let custodian = custodian_nest_key().verifying_key().to_bytes();
    let set = CustodyScopeSet::Account;
    let undeposited = custody_mint_blob(
        &owner().actor_id().0,
        &GRANT_ID,
        &custodian,
        GrantWindow(now, now + 90 * 24 * 3600),
        &set,
    )
    .expect("keyless custody blob");
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(owner().actor_id());
    grant_log::record_mint(
        &mut log,
        owner().signing_key(),
        GRANT_ID,
        custodian,
        custody_event_scopes(&set),
        now,
        now + 90 * 24 * 3600,
        now,
    )
    .expect("record the Mint event");
    // The grant log is the succession ledger; the Mint is recorded before
    // the release, exactly as a minting machine records it.
    let stored = log;
    let blob_bytes = undeposited
        .release(&RecordedGrants::from_stored(&stored))
        .expect("released against the stored log");
    let reply = CapabilitiesClient::new(Arc::clone(nest))
        .mint(blob_bytes)
        .await
        .expect("mint RPC");
    assert!(reply.ok, "the custody row deposited");
}

#[tokio::test]
async fn the_custodian_nest_pulls_with_no_host_device_and_revoke_severs() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (owner_base, owner_state, _owner_tmp) = start_owner_nest().await;
    let (host_base, host_state, host_tmp) = start_custodian_nest().await;

    // ── The owner: registered, sealing one row through the REAL writer door ──
    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&owner_base, owner()).await;

    let owner_store_dir = tempfile::tempdir().unwrap();
    let owner_device_key = SigningKey::from_bytes(&[0x0B; 32]);
    let owner_device_pub = owner_device_key.verifying_key().to_bytes();
    let owner_store = AccountStore::open(
        SqliteBackend::open(owner_store_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(owner_device_pub),
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
        &owner_device_key,
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
            muted_keywords: vec!["pulled-by-the-nest-pump".into()],
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
        Some(
            fauna_protocol::merge_policy::LwwStamp {
                at_ms: 1_000,
                writer: owner_device_pub,
            }
            .encode()
            .unwrap(),
        ),
    )
    .await
    .expect("the owner's row seals and publishes to the nest feed");

    // ── The capability row on the owner's nest, custodian = the NEST key ──
    mint_custody_row(&owner_nest).await;

    // ── The host: deposits the hosting row on its OWN nest, then goes away ──
    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    let hosting = CustodyHostingClient::new(Arc::clone(&host_client));
    let reply = hosting
        .register(&HostingDeposit {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id().0,
            witness: fauna_core::encoding::canonical_encode(&nest_witness())
                .unwrap()
                .to_vec(),
            owner_nest_url: owner_base.clone(),
            owner_devices: Vec::new(),
            retained_bytes_cap: 8 * 1024 * 1024 * 1024,
            stopped: false,
        })
        .await
        .expect("hosting register RPC");
    assert!(reply.ok, "the hosting row deposited");
    // No host device runs from here on: the deposit is the host's last act.
    host_client.disconnect().await;

    // ── One pump pass: the NEST pulls the owner's plane as ITSELF ─────────
    let worker = CustodyHostingWorker::new(Arc::clone(&host_state), Duration::MAX);
    let report = worker.run_once().await.expect("pump pass");
    assert_eq!(report.rows, 1, "the pass saw the hosting row");
    assert_eq!(report.pulled, 1, "the pull completed: {report:?}");
    assert_eq!(report.failed, 0, "nothing failed: {report:?}");
    assert!(report.recorded >= 1, "the owner's sealed row was recorded");

    // The custodied store holds the owner's row — keyless, relay-plane only.
    let custodied_dir = host_tmp
        .path()
        .join("custody-hosting")
        .join(hex::encode(host().actor_id().0))
        .join(owner().actor_id_hex());
    let held = AccountStore::open(
        SqliteBackend::open(&custodied_dir).unwrap(),
        &owner().actor_id_hex(),
        WriterId(custodian_nest_key().verifying_key().to_bytes()),
    )
    .await
    .unwrap();
    let rows = held
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap();
    assert!(!rows.is_empty(), "the custodied store holds the pulled row");
    assert_eq!(
        held.max_held_seq(ACCOUNT_STATE_SCOPE, &WriterId(owner_device_pub))
            .await
            .unwrap(),
        None,
        "a custodian never journals — the rows are relay-plane only"
    );

    // ── Stage (c): the same pass minted a receipt under the NEST identity
    //    and deposited it at the owner's nest custody door. ───────────────
    assert_eq!(
        report.receipts_deposited, 1,
        "the first pass owes and deposits a receipt: {report:?}"
    );
    let staged = fauna_client_capabilities::custody_hosting::CustodyReceiptsClient::new(
        Arc::clone(&owner_nest),
    )
    .list()
    .await
    .expect("receipt list RPC")
    .rows;
    assert_eq!(
        staged.len(),
        1,
        "the owner's nest staged exactly one receipt"
    );
    assert_eq!(staged[0].grant_id.as_slice(), GRANT_ID.as_slice());
    let envelope: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&staged[0].receipt).expect("envelope decodes");
    let receipt = fauna_core::custody_receipt::verify_custody_receipt(
        &envelope,
        &custodian_nest_key().verifying_key().to_bytes(),
    )
    .expect("the staged receipt verifies against the NEST identity");
    assert!(receipt.attested_at.0 > 0);
    assert_eq!(receipt.owner, owner().actor_id().0);

    // The metering + receipt bookkeeping reached the hosting row (the host
    // UI's read; bookkeeping advancing is what stops a redrive next pass).
    let listed = host_state
        .db
        .list_custody_hosting(&host().actor_id().0)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].last_receipt_at, receipt.attested_at.0,
        "an acked deposit advances the cadence bookkeeping"
    );

    // ── A LYING deposit is refused AT THE DOOR (the gotcha): a valid
    //    custody bearer carrying a receipt signed by any key but the row's
    //    holder stages nothing. ─────────────────────────────────────────────
    let bearer = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &owner_base,
        owner().actor_id().0,
        custodian_nest_key(),
        nest_witness(),
    );
    bearer.connect().await.expect("custody bearer connects");
    let impostor = ActorKeypair::from_secret([0x77; 32]);
    let forged = {
        let r = fauna_core::custody_receipt::CustodyReceipt {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id().0,
            custodian_key: impostor.actor_id().0,
            attested_at: fauna_core::data::Timestamp(receipt.attested_at.0 + 1),
            ..Default::default()
        };
        let env = fauna_core::custody_receipt::sign_custody_receipt(&impostor, &r).unwrap();
        fauna_core::encoding::canonical_encode(&env)
            .unwrap()
            .to_vec()
    };
    assert!(
        fauna_client_capabilities::custody_hosting::CustodyReceiptsClient::new(Arc::clone(&bearer))
            .deposit(owner().actor_id().0, &GRANT_ID, forged)
            .await
            .is_err(),
        "a receipt signed by a key other than the capability row's holder is refused at the door"
    );
    let still = fauna_client_capabilities::custody_hosting::CustodyReceiptsClient::new(Arc::clone(
        &owner_nest,
    ))
    .list()
    .await
    .expect("receipt list RPC")
    .rows;
    assert_eq!(still.len(), 1, "the forged deposit staged nothing");
    assert_eq!(
        still[0].attested_at, receipt.attested_at.0,
        "the honest receipt is undisturbed"
    );
    bearer.disconnect().await;

    // ── Revoke severs: the next pass's fresh handshake takes its honest
    //    refusal at connect, and the store gains nothing. ──────────────────
    CapabilitiesClient::new(Arc::clone(&owner_nest))
        .revoke(GRANT_ID)
        .await
        .expect("revoke RPC");
    let report = worker.run_once().await.expect("pump pass after revoke");
    assert_eq!(report.pulled, 0, "a revoked custody pulls nothing");
    assert_eq!(
        report.failed, 1,
        "the refused handshake is a failed row, honestly reported: {report:?}"
    );
}

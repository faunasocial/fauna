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
use fauna_client_capabilities::grant_log::{self, PublishedGrants};
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
    // the release, exactly as a minting machine records it. This test drives
    // the nest's custody door, not the grant-mint door, so the log stands in
    // for one the bound nest acknowledged (the door itself is pinned over a
    // real account runtime in `conformance_capability_reconcile_sweep_pass.rs`).
    let stored = log;
    let blob_bytes = undeposited
        .release(&PublishedGrants::from_published(
            &fauna_client_config::PublishedLedger::acknowledged_for_test(stored),
        ))
        .expect("released against the published log");
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

    // ── A successful handshake clears a standing refusal mark (ruling 5):
    //    an old mark planted under a live grant is gone after one pass. ────
    let refused_at = |state: Arc<AppState>| async move {
        state
            .db
            .get_custody_hosting(&host().actor_id().0, &GRANT_ID)
            .await
            .unwrap()
            .expect("the hosting row stands")
            .owner_refused_at
    };
    host_state
        .db
        .set_custody_hosting_owner_refused(&host().actor_id().0, &GRANT_ID, 1_000)
        .await
        .unwrap();
    let report = worker
        .run_once()
        .await
        .expect("pump pass over a marked row");
    assert_eq!(report.owner_refused, 0, "{report:?}");
    assert_eq!(
        refused_at(Arc::clone(&host_state)).await,
        0,
        "the owner's nest admitted the handshake — the mark clears"
    );

    // ── Revoke severs: the next pass's fresh handshake takes its honest
    //    refusal at connect, the row is MARKED (the serve door refuses it
    //    from here), and the store gains nothing. ───────────────────────────
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
    assert_eq!(
        report.owner_refused, 1,
        "the refusal is the owner nest's authenticated not_registered: {report:?}"
    );
    assert!(
        refused_at(Arc::clone(&host_state)).await > 0,
        "the authenticated refusal marks the row"
    );
}

/// A fleet device's handshake at the custodian nest's serve door, alone —
/// `Err` carries the nest's code.
async fn fleet_handshake(base: &str, device: &SigningKey) -> Result<String, String> {
    let cert = {
        let cert = fauna_core::data::DeviceAuthorization {
            actor_id: owner().actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&owner(), &cert).unwrap();
        fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env)
    };
    fauna_anon_client::mint_bearer_over_custodied_fleet_handshake(
        base,
        owner().actor_id().0,
        device,
        &cert,
    )
    .await
    .map(|minted| minted.token)
    .map_err(|e| match e {
        fauna_anon_client::AnonClientError::Rpc(rpc) => rpc.code,
        other => format!("{other}"),
    })
}

/// **A stopped row takes the same handshake, verdict only** (ruling 5's
/// *stopped rows* paragraph): a row the host STOPPED is still dialed each
/// pass for the owner nest's verdict — a success clears a standing mark, an
/// authenticated refusal sets one, so the owner's revoke reaches the serve
/// door within one pump interval — yet nothing is pulled and the pair store is
/// never even created. Red with the stopped skip back ahead of `connect`: the
/// mark neither clears nor sets, and the door keeps admitting the revoked
/// owner's fleet.
#[tokio::test]
async fn a_stopped_row_is_dialed_for_the_owners_verdict_only() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (owner_base, owner_state, _owner_tmp) = start_owner_nest().await;
    let (host_base, host_state, host_tmp) = start_custodian_nest().await;
    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&owner_base, owner()).await;
    mint_custody_row(&owner_nest).await;

    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    let reply = CustodyHostingClient::new(Arc::clone(&host_client))
        .register(&HostingDeposit {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id().0,
            witness: fauna_core::encoding::canonical_encode(&nest_witness())
                .unwrap()
                .to_vec(),
            owner_nest_url: owner_base.clone(),
            owner_devices: Vec::new(),
            retained_bytes_cap: 8 * 1024 * 1024 * 1024,
            stopped: true,
        })
        .await
        .expect("hosting register RPC");
    assert!(reply.ok, "the stopped hosting row deposited");
    host_client.disconnect().await;

    let refused_at = |state: Arc<AppState>| async move {
        state
            .db
            .get_custody_hosting(&host().actor_id().0, &GRANT_ID)
            .await
            .unwrap()
            .expect("the hosting row stands")
            .owner_refused_at
    };
    let custodied_dir = host_tmp
        .path()
        .join("custody-hosting")
        .join(hex::encode(host().actor_id().0))
        .join(owner().actor_id_hex());

    // ── (i) Live grant, a standing mark: the verdict dial clears it, and
    //    nothing is pulled — the pair store is not even created. ───────────
    host_state
        .db
        .set_custody_hosting_owner_refused(&host().actor_id().0, &GRANT_ID, 1_000)
        .await
        .unwrap();
    let worker = CustodyHostingWorker::new(Arc::clone(&host_state), Duration::MAX);
    let report = worker
        .run_once()
        .await
        .expect("pump pass over a stopped row");
    assert_eq!(report.stopped, 1, "{report:?}");
    assert_eq!(
        report.pulled, 0,
        "a stopped row's pull is paused: {report:?}"
    );
    assert_eq!(report.owner_refused, 0, "{report:?}");
    assert_eq!(
        report.failed, 0,
        "a verdict dial is not a failed pull: {report:?}"
    );
    assert_eq!(
        report.receipts_deposited, 0,
        "no receipt for a paused copy: {report:?}"
    );
    assert_eq!(
        refused_at(Arc::clone(&host_state)).await,
        0,
        "the owner's nest admitted the stopped row's handshake — the mark clears"
    );
    assert!(
        !custodied_dir.exists(),
        "a stopped row never opens (and so never creates) a pair store"
    );
    let device = SigningKey::from_bytes(&[0xD5; 32]);
    assert!(
        fleet_handshake(&host_base, &device).await.is_ok(),
        "a stopped, unrefused row serves its owner's fleet (ruling 4)"
    );

    // ── (ii) Revoke: the stopped row's next verdict dial takes the
    //    authenticated refusal and MARKS the row — counted, never failed. ──
    CapabilitiesClient::new(Arc::clone(&owner_nest))
        .revoke(GRANT_ID)
        .await
        .expect("revoke RPC");
    let report = worker.run_once().await.expect("pump pass after revoke");
    assert_eq!(report.stopped, 1, "{report:?}");
    assert_eq!(report.owner_refused, 1, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.pulled, 0, "{report:?}");
    assert!(
        refused_at(Arc::clone(&host_state)).await > 0,
        "the owner's revoke reaches a stopped row within one pass"
    );

    // ── (iii) …and so the serve door refuses the owner's fleet. ────────────
    assert_eq!(
        fleet_handshake(&host_base, &device).await,
        Err("fauna.auth.not_registered".into()),
        "a stopped row the owner revoked is no longer served"
    );
    assert!(!custodied_dir.exists(), "still no pair store");
}

/// **The admission bound on the nest-hosted form**: the
/// hosting row's `retained_bytes_cap` is BELOW one segment pair, so the pump's
/// pull completes (the owner's plane is reachable) and adopts none of the
/// owner's mail segments. Pins what the device-form pump test cannot: the
/// worker hands `pull_from_owner_nest` the row's cap. Mutate: pass `None` there
/// (the 8 GiB default) and the pair adopts.
#[tokio::test]
async fn a_hosted_custody_with_a_cap_below_one_pair_adopts_no_segment() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (owner_base, owner_state, _owner_tmp) = start_owner_nest().await;
    let (host_base, host_state, _host_tmp) = start_custodian_nest().await;

    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&owner_base, owner()).await;

    // One sealed mail record, finalized so the pair is adoptable.
    fauna_nest::segments::mail::append_record(
        &owner_state.mail_segments,
        &owner_state.db,
        &owner().actor_id().0,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed mail the cap leaves no room to hold".to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-index-hint".to_vec(),
        ),
        fauna_mail::segments::MailFloorMetadata {
            format_version: fauna_mail::segments::MAIL_FLOOR_FORMAT_VERSION,
            received_at: 1_715_000_000_000,
            timestamp: 1_715_000_000,
            sender_domain: "example.com".to_string(),
            spam_disposition: "accept".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("append one sealed mail record on the owner's scope");
    owner_state
        .mail_segments
        .finalize_open(&owner().actor_id().0)
        .await
        .expect("finalize the open mail segment");

    mint_custody_row(&owner_nest).await;

    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    let reply = CustodyHostingClient::new(Arc::clone(&host_client))
        .register(&HostingDeposit {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id().0,
            witness: fauna_core::encoding::canonical_encode(&nest_witness())
                .unwrap()
                .to_vec(),
            owner_nest_url: owner_base.clone(),
            owner_devices: Vec::new(),
            // Below any one pair (a `.dat` plus its `.meta`).
            retained_bytes_cap: 64,
            stopped: false,
        })
        .await
        .expect("hosting register RPC");
    assert!(reply.ok, "the hosting row deposited");
    host_client.disconnect().await;

    let worker = CustodyHostingWorker::new(Arc::clone(&host_state), Duration::MAX);
    let report = worker.run_once().await.expect("pump pass");
    assert_eq!(report.pulled, 1, "the pull itself completed: {report:?}");
    assert_eq!(
        report.adopted_segments, 0,
        "the cap leaves no room for a pair: {report:?}"
    );
}

/// The T15 ingest brake on the nest pump: a hosted custody the brake
/// has closed — at the floor, or after [`METERING_FAILURES_BRAKE`] consecutive
/// metering failures — pulls NOTHING on the next pass, yet the pass's budget
/// arm still runs and, metering cleanly, releases the brake so the pass after
/// pulls again. Same mechanism as the device legs (one `IngestBrake`). The
/// failure leg faults the pump's meter (`custody_hosting_meter_fault`), so it
/// is red with the budget arm's `record_metering_failure()` removed.
#[tokio::test]
async fn a_braked_hosted_custody_pulls_nothing_until_the_meter_clears() {
    use fauna_sync_engine::custody_leg::METERING_FAILURES_BRAKE;

    let (owner_base, owner_state, _owner_tmp) = start_owner_nest().await;
    let (host_base, host_state, _host_tmp) = start_custodian_nest().await;
    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&owner_base, owner()).await;
    mint_custody_row(&owner_nest).await;

    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    let reply = CustodyHostingClient::new(Arc::clone(&host_client))
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
    host_client.disconnect().await;

    let worker = CustodyHostingWorker::new(Arc::clone(&host_state), Duration::MAX);
    let brake = host_state
        .custody_hosting_brakes
        .for_custody(&host().actor_id().0, &GRANT_ID);

    // Baseline: an open brake pulls.
    let open = worker.run_once().await.expect("pump pass");
    assert_eq!(open.pulled, 1, "un-braked: the pull runs: {open:?}");
    assert_eq!(open.braked, 0, "{open:?}");

    // At the floor: the next pass pulls nothing; its budget arm meters the
    // (really under-cap) store and releases the brake.
    brake.record_metered(true);
    let floor = worker.run_once().await.expect("pump pass at floor");
    assert_eq!(
        floor.braked, 1,
        "at floor: the custody stops pulling: {floor:?}"
    );
    assert_eq!(floor.pulled, 0, "nothing pulled while braked: {floor:?}");
    assert_eq!(floor.failed, 0, "{floor:?}");
    let released = worker.run_once().await.expect("pump pass after release");
    assert_eq!(
        released.braked, 0,
        "the budget arm released the brake: {released:?}"
    );
    assert_eq!(released.pulled, 1, "pulls resume: {released:?}");

    // Persistent metering failure, through the REAL budget arm: the
    // pump's meter is faulted, so each pass's own `Err` arm writes the brake.
    // One failure is weather — the passes before the threshold still pull;
    // then the next pass is braked.
    let fault = &host_state.custody_hosting_meter_fault;
    fault.store(true, std::sync::atomic::Ordering::SeqCst);
    for i in 1..=METERING_FAILURES_BRAKE {
        let weather = worker.run_once().await.expect("pump pass, meter faulted");
        assert_eq!(
            weather.metering_failed, 1,
            "failure #{i} is reported: {weather:?}"
        );
        assert_eq!(weather.braked, 0, "failure #{i} is weather: {weather:?}");
        assert_eq!(weather.pulled, 1, "still pulling after #{i}: {weather:?}");
    }
    let dark = worker.run_once().await.expect("pump pass, meter dark");
    assert_eq!(dark.braked, 1, "meter dark: ingest is closed: {dark:?}");
    assert_eq!(dark.pulled, 0, "{dark:?}");
    assert_eq!(dark.metering_failed, 1, "still failing: {dark:?}");

    // The meter clears: one pass meters clean (still braked going in) and
    // releases the brake; the pass after pulls.
    fault.store(false, std::sync::atomic::Ordering::SeqCst);
    let clean = worker.run_once().await.expect("pump pass, meter back");
    assert_eq!(clean.metering_failed, 0, "{clean:?}");
    assert_eq!(
        clean.braked, 1,
        "braked going into the clean pass: {clean:?}"
    );
    let resumed = worker
        .run_once()
        .await
        .expect("pump pass after clean meter");
    assert_eq!(
        resumed.braked, 0,
        "a clean meter reopens ingest: {resumed:?}"
    );
    assert_eq!(resumed.pulled, 1, "pulls resume: {resumed:?}");
}

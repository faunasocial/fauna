//! **The custodian copy of the seed escrow, end to end over two real nests**
//! (tier_3; `docs/goal/behavior/identity-succession.md` § Seed escrow → *The
//! custodian copy*, rulings 1–3 and 5).
//!
//! An owner mints a recovery kit on its own nest through the shared ceremony
//! (`fauna_client_recovery::create_kit_with_root` — registration, then the
//! sealed blob); a host's nest custodies the owner under an `Account`-form
//! witness; the custodian nest's pump — the NEST's own identity, the
//! `conformance_custody_hosting_pump_client.rs` rig — pulls the owner's
//! recovery material into the pair store. Then the owner's nest is DROPPED,
//! and the restoring client — a fresh process holding only the kit, over an
//! anonymous connection to the CUSTODIAN — runs the ordinary phrase restore
//! (`fauna_client_recovery::restore_seed`) and recovers the exact identity
//! seed.
//!
//! What else each test attacks:
//!
//! - `escrow.pull` is the custodian's alone: a stranger's session is refused
//!   by class, and a custodian bearer whose grant is the explicit-list form is
//!   refused at admission (the recovery material is of the account);
//! - **the refusal is opaque**: a wrong kit at the custodian, and a kit for
//!   an actor this nest never heard of, read the same `not_registered`;
//! - **the copy follows the chain**: after the owner REPLACES the kit at its
//!   living nest and the pump passes once, the old kit no longer opens the
//!   custodied blob and the new one does; and a pass against an unreachable
//!   owner nest deletes nothing.
//!
//! Every assertion is on latency-independent state (e2e convention 14): the
//! pump is driven by direct `run_once` pokes, never a wall clock.

mod common;
use common::connected_client;

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_anon_client::AnonymousNestClient;
use fauna_client::NestClient;
use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
use fauna_client_capabilities::custody_hosting::{CustodyHostingClient, HostingDeposit};
use fauna_client_capabilities::grant_log::{self, PublishedGrants};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_client_recovery::{RecoveryClient, RecoveryError, create_kit_with_root, parse_kit};
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
};
use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::RecoveryKey;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::backup::service::BackupService;
use fauna_nest::custody_hosting_worker::CustodyHostingWorker;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::recovery::{ESCROW_PULL_KIND, EscrowPullReply, EscrowPullRequest};
use fauna_protocol::{ByteBuf, RpcRequester};

const OWNER_SEED: [u8; 32] = [0x7A; 32];
const HOST_SEED: [u8; 32] = [0x7C; 32];
const STRANGER_SEED: [u8; 32] = [0x7D; 32];
/// The custodian NEST's deployment seed — its public key is the witness's
/// `custodian_key`.
const CUSTODIAN_NEST_SEED: [u8; 32] = [0xC7; 32];
const GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x5A; CUSTODY_GRANT_ID_LEN];
const FAR_FUTURE: Timestamp = Timestamp(9_000_000_000_000_000);

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

fn router(custodian: bool) -> fauna_nest::rpc_router::RpcRouter {
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::auth_handlers::register_auth_handlers(&mut b);
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    fauna_nest::segments::register_segments_handlers(&mut b);
    fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
    if custodian {
        fauna_nest::custody_hosting_handlers::register_custody_hosting_handlers(&mut b);
    } else {
        fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
        fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
        fauna_nest::custody_receipt_handlers::register_custody_receipt_handlers(&mut b);
    }
    b.build()
}

/// One real nest: the OWNER's (`custodian = false`, a fresh deployment key)
/// or the CUSTODIAN (`true`: the pinned key, a hosting root, the hosting
/// doors). Returns its base URL, state, temp dir and serve task.
async fn start_nest(
    custodian: bool,
) -> (
    String,
    Arc<AppState>,
    tempfile::TempDir,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
    );
    let seed = if custodian {
        CUSTODIAN_NEST_SEED
    } else {
        let mut s = [0u8; 32];
        getrandom::fill(&mut s).unwrap();
        s
    };
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&seed)),
        custody_hosting_root: custodian.then(|| tmp.path().join("custody-hosting")),
        rpc_router: Arc::new(router(custodian)),
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
    let serve = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp, serve)
}

/// An owner-signed custody witness naming the custodian NEST.
fn witness(set: CustodyScopeSet) -> EmbedAsBytes {
    sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id(),
            custodian_key: custodian_nest_pub(),
            scopes: set,
            minted_at: Timestamp(1_000),
            expires_at: FAR_FUTURE,
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness")
}

/// Mint the custody capability row on the OWNER's nest, naming `holder`.
async fn mint_custody_row(nest: &Arc<NestClient>, holder: [u8; 32], set: CustodyScopeSet) {
    let now = Timestamp::now_secs().max(0) as u64;
    let undeposited = custody_mint_blob(
        &owner().actor_id().0,
        &GRANT_ID,
        &holder,
        GrantWindow(now, now + 90 * 24 * 3600),
        &set,
    )
    .expect("keyless custody blob");
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(owner().actor_id());
    grant_log::record_mint(
        &mut log,
        owner().signing_key(),
        GRANT_ID,
        holder,
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

/// The host's register door, one deposit of the owner's witness.
async fn deposit(host_client: &Arc<NestClient>, witness: &EmbedAsBytes, owner_nest_url: &str) {
    let reply = CustodyHostingClient::new(Arc::clone(host_client))
        .register(&HostingDeposit {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id().0,
            witness: fauna_core::encoding::canonical_encode(witness)
                .unwrap()
                .to_vec(),
            owner_nest_url: owner_nest_url.to_string(),
            owner_devices: Vec::new(),
            retained_bytes_cap: 8 * 1024 * 1024 * 1024,
            stopped: false,
        })
        .await
        .expect("hosting register RPC");
    assert!(reply.ok, "the hosting row deposited");
}

/// The phrase restore at `base`, as a fresh device holding only `kit`: an
/// anonymous connection and the shared ceremony, unchanged.
async fn restore_at(base: &str, kit: &RecoveryKey, actor: [u8; 32]) -> Result<[u8; 32], String> {
    let anon = AnonymousNestClient::connect(base)
        .await
        .expect("anonymous connection");
    let parsed = parse_kit(&format!(
        "fauna://recovery?secret={}&actor={}",
        kit.to_hex(),
        hex::encode(actor)
    ))
    .expect("a minted kit parses");
    fauna_client_recovery::restore_seed(&RecoveryClient::new(anon), &parsed, None)
        .await
        .map(|restored| *restored.seed)
        .map_err(|e| match e {
            RecoveryError::Superseded { .. } => "superseded".to_string(),
            other => format!("{other:?}"),
        })
}

/// The nest's code on a refused request.
fn code(e: &fauna_client::NestClientError) -> &str {
    match e {
        fauna_client::NestClientError::Rpc(e) => e.code.as_str(),
        _ => "<not an RPC refusal>",
    }
}

/// What the rig leaves standing: the owner's nest (live), the custodian nest
/// with the host's row, and the kit the owner minted.
struct Rig {
    owner_base: String,
    owner_state: Arc<AppState>,
    owner_client: Arc<NestClient>,
    owner_serve: tokio::task::JoinHandle<()>,
    host_base: String,
    host_state: Arc<AppState>,
    host_client: Arc<NestClient>,
    kit: RecoveryKey,
    _tmps: Vec<tempfile::TempDir>,
}

impl Rig {
    async fn pass(&self) -> fauna_nest::custody_hosting_worker::CustodyHostingPassReport {
        CustodyHostingWorker::new(Arc::clone(&self.host_state), Duration::MAX)
            .run_once()
            .await
            .expect("pump pass")
    }
}

async fn rig(set: CustodyScopeSet) -> Rig {
    let (owner_base, owner_state, owner_tmp, owner_serve) = start_nest(false).await;
    let (host_base, host_state, host_tmp, _host_serve) = start_nest(true).await;

    owner_state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_client = connected_client(&owner_base, owner()).await;
    let kit = RecoveryKey::from_bytes([0x11; 32]);
    create_kit_with_root(
        &RecoveryClient::new(Arc::clone(&owner_client)),
        &owner(),
        None,
        RecoveryKey::from_bytes(kit.to_bytes()),
        &[],
    )
    .await
    .expect("the owner mints a kit at home");
    mint_custody_row(&owner_client, custodian_nest_pub(), set.clone()).await;

    host_state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = connected_client(&host_base, host()).await;
    deposit(&host_client, &witness(set), &owner_base).await;

    Rig {
        owner_base,
        owner_state,
        owner_client,
        owner_serve,
        host_base,
        host_state,
        host_client,
        kit,
        _tmps: vec![owner_tmp, host_tmp],
    }
}

/// The capstone: the kit alone recovers the exact seed at the custodian, with
/// the owner's nest gone.
#[tokio::test]
async fn a_kit_alone_restores_at_the_custodian_with_the_home_nest_gone() {
    let rig = rig(CustodyScopeSet::Account).await;

    // Before any pass the custodian holds nothing — the refusal a stranger gets.
    let before = restore_at(&rig.host_base, &rig.kit, owner().actor_id().0).await;
    assert!(
        before.as_ref().is_err_and(|e| e.contains("NotRegistered")),
        "nothing custodied yet: {before:?}"
    );

    let report = rig.pass().await;
    assert_eq!(report.recovery_pulled, 1, "{report:?}");
    assert_eq!(report.escrows_custodied, 1, "{report:?}");

    // The home nest dies — every later pass finds it unreachable.
    rig.owner_serve.abort();
    let report = rig.pass().await;
    assert_eq!(
        (report.failed, report.recovery_pulled),
        (1, 0),
        "the owner's nest is gone: {report:?}"
    );

    // An unreachable owner nest deleted nothing: the restore below opens the
    // copy the first pass took.

    let seed = restore_at(&rig.host_base, &rig.kit, owner().actor_id().0)
        .await
        .expect("the kit restores at the custodian");
    assert_eq!(seed, OWNER_SEED, "the exact identity seed comes back");

    // The registration chain stays the home nest's to answer: the custodian
    // does not serve it for a custodied owner (ruling 3).
    let anon = AnonymousNestClient::connect(&rig.host_base).await.unwrap();
    let chain = RecoveryClient::new(anon)
        .registration_chain(&owner().actor_id())
        .await
        .expect("the chain read answers");
    assert!(
        chain.is_empty(),
        "registration.chain answers residents only"
    );
    let _ = rig.owner_base;
}

/// The refusal is opaque: a wrong kit at the custodian reads exactly as an
/// actor this nest never heard of.
#[tokio::test]
async fn a_wrong_kit_cannot_tell_a_custodied_owner_from_an_unknown_actor() {
    let rig = rig(CustodyScopeSet::Account).await;
    rig.pass().await;

    let wrong = RecoveryKey::from_bytes([0x99; 32]);
    let custodied = restore_at(&rig.host_base, &wrong, owner().actor_id().0)
        .await
        .expect_err("a wrong kit opens nothing");
    let unknown = restore_at(
        &rig.host_base,
        &wrong,
        ActorKeypair::from_secret([0x42; 32]).actor_id().0,
    )
    .await
    .expect_err("an unknown actor opens nothing");
    assert_eq!(
        custodied, unknown,
        "a custodied owner and an unknown actor must refuse identically"
    );
    assert!(custodied.contains("NotRegistered"), "{custodied}");
}

/// `escrow.pull` is the custodian's alone — and only under the `Account` form.
#[tokio::test]
async fn the_pull_door_is_custodian_only_and_account_form_only() {
    // A stranger's own session: refused by class.
    let account = rig(CustodyScopeSet::Account).await;
    account
        .owner_state
        .db
        .create_user_with_handle(
            &ActorKeypair::from_secret(STRANGER_SEED).actor_id().0,
            "free",
            "stranger",
            None,
        )
        .await
        .unwrap();
    let stranger = connected_client(
        &account.owner_base,
        ActorKeypair::from_secret(STRANGER_SEED),
    )
    .await;
    let refused = stranger
        .request::<_, EscrowPullReply>(
            ESCROW_PULL_KIND,
            EscrowPullRequest {
                owner_actor_id: ByteBuf::from(owner().actor_id().0.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect_err("a user session may not pull");
    assert_eq!(
        code(&refused),
        "fauna.recovery.permission_denied",
        "{refused}"
    );
    // The owner itself is a User, never a custodian of itself.
    let refused = account
        .owner_client
        .request::<_, EscrowPullReply>(
            ESCROW_PULL_KIND,
            EscrowPullRequest {
                owner_actor_id: ByteBuf::from(owner().actor_id().0.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect_err("the owner's own session may not pull");
    assert_eq!(
        code(&refused),
        "fauna.recovery.permission_denied",
        "{refused}"
    );

    // An explicit-list custodian pulls its planes but never the material.
    let listed = rig(CustodyScopeSet::Scopes(vec![
        fauna_protocol::account_state::ACCOUNT_STATE_SCOPE.to_string(),
    ]))
    .await;
    let report = listed.pass().await;
    assert_eq!(report.recovery_pulled, 0, "{report:?}");
    assert_eq!(report.escrows_custodied, 0, "{report:?}");
    let _ = (&listed.host_client, &account.host_client);
}

/// The copy follows the chain: a kit replaced at a LIVING home nest stops
/// opening the custodied blob after one pass, and the new kit opens it.
#[tokio::test]
async fn a_replaced_kit_stops_opening_the_custodied_blob_after_one_pass() {
    let rig = rig(CustodyScopeSet::Account).await;
    rig.pass().await;
    assert_eq!(
        restore_at(&rig.host_base, &rig.kit, owner().actor_id().0).await,
        Ok(OWNER_SEED),
        "the first kit opens the copy"
    );

    let replacement = RecoveryKey::from_bytes([0x22; 32]);
    create_kit_with_root(
        &RecoveryClient::new(Arc::clone(&rig.owner_client)),
        &owner(),
        Some(&rig.kit),
        RecoveryKey::from_bytes(replacement.to_bytes()),
        &[],
    )
    .await
    .expect("the owner replaces the kit at home");
    let report = rig.pass().await;
    assert_eq!(report.escrows_custodied, 1, "{report:?}");

    let old = restore_at(&rig.host_base, &rig.kit, owner().actor_id().0)
        .await
        .expect_err("the retired kit opens nothing at the custodian");
    assert!(old.contains("NotRegistered"), "{old}");
    assert_eq!(
        restore_at(&rig.host_base, &replacement, owner().actor_id().0).await,
        Ok(OWNER_SEED),
        "the new kit opens the re-pulled copy"
    );
}

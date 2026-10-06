//! **The road, in the runtime** (`identity-succession.md` § Enforcement on the
//! home nest → *Every nest the identity is linked to*, the paragraphs **The
//! road** and **The stated bounds**) — tier_3, real wire, two nests.
//!
//! An identity holds an account on two nests, H and L, linked both ways by the
//! production both-ends link (`fauna_client_pair`'s `LinkBoth`, which carries
//! the RecoveryKey chain to L before it writes the rows). Its succession lands
//! at H. From then on nothing is driven by hand: the successor's production
//! account runtime (`AccountStoreRuntime`, over a real `NestClient` bound to
//! H) carries the statement to L in its full pass through the production
//! native deliverer (`fauna_client_account_runtime::native_owed_nest_deliverer`
//! — an `AnonymousNestClient` to L, and a `NestClient` signed in as the
//! retired identity when a chain must be replayed), and settles H's owed
//! entry. L then refuses the retired key and signs the successor in; the
//! successor links L again, and the secondary leg completes it — reading L's
//! own owed list on the way, which names H, where the statement already is —
//! and a reader holding only the successor's seed recovers the account plane
//! from L.
//!
//! The second case is the **chain-less arm**: L holds the account and no
//! chain (the pairing predates any kit, so no link carried one), so the
//! statement cannot verify there. A runtime handed no predecessor seed leaves
//! the entry owed; one handed the retired identity's seed signs in at L as
//! that identity, replays H's chain, and the statement lands.
//!
//! The third is **a nest holding no account** for the retired identity,
//! which is settled.
//!
//! Red-verified: with the pass step switched off, all three cases go red.
//!
//! What only this suite catches: the pass step, the runtime's two new inputs
//! (the deliverer and the seeds it signs in with) and the native reach over
//! real sockets. The delivery body's arms are pinned over the real handlers
//! by `conformance_succession.rs`'s `road` module; stated bound 4 is witnessed
//! there.
//!
//! Tier: tier_3 (real nests over real sockets, real clients, real runtimes).
//! Every assertion is on latency-independent state (e2e convention 14): passes
//! are driven with `reconcile_now`, and the only loops are bounded counts of
//! them.

mod common;
use common::connected_client;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_client::NestClient;
use fauna_client_account_runtime::{
    PredecessorSeeds, SeedHolder, native_linked_nest_connector, native_owed_nest_deliverer,
};
use fauna_client_core::succession_delivery::{
    Delivery, OwedReason, fetch_owed_nests, fetch_statement_path, submit_statement,
};
use fauna_client_pair::LinkedNestsAction;
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::RecoveryKey;
use fauna_credential_store::CredentialStore;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE, PumpReport,
    RuntimePrincipal, StoreRoot, fixed_holders,
};
use fauna_sync_engine::deployment_seed_recovery::cold_read_deployment_seeds;
use fauna_sync_engine::linked_leg::LinkedOutcome;

/// The retired identity's seed, and its successor's.
const OLD_SEED: [u8; 32] = [0x11; 32];
const NEW_SEED: [u8; 32] = [0x33; 32];
/// The RecoveryKey the kit registers at H.
const ROOT: [u8; 32] = [0x21; 32];

fn old() -> ActorKeypair {
    ActorKeypair::from_secret(OLD_SEED)
}

fn new() -> ActorKeypair {
    ActorKeypair::from_secret(NEW_SEED)
}

/// One of the identity's nests over a real socket: every handler family the
/// runtime's pass and the both-ends link reach, a deployment key of its own
/// (the identity its escrow receipts are signed with, and the one every
/// connection to it is bound to).
struct Nest {
    base: String,
    state: Arc<AppState>,
    _blob: tempfile::TempDir,
}

impl Nest {
    async fn start(key_seed: u8) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let key = SigningKey::from_bytes(&[key_seed; 32]);
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.set_nest_keypair(key.as_bytes(), &key.verifying_key().to_bytes())
            .await
            .unwrap();
        let blob = tempfile::tempdir().unwrap();
        let backup = Arc::new(
            BackupService::new(db.clone(), None, false, blob.path().into(), None).unwrap(),
        );
        let state = Arc::new(AppState {
            nest_identity: Arc::new(NestIdentity::from_seed(&[key_seed; 32])),
            nest_signing_key: Some(key),
            backup_service: Some(backup),
            rpc_router: Arc::new({
                let mut b = fauna_nest::rpc_router::RpcRouter::builder();
                fauna_nest::auth_handlers::register_auth_handlers(&mut b);
                fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
                fauna_nest::pair_handlers::register_pair_handlers(&mut b);
                fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
                fauna_nest::folder_handlers::register_folders_handlers(&mut b);
                fauna_nest::sync_handlers::register_sync_handlers(&mut b);
                fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
                fauna_nest::account_handlers::register_account_user_handlers(&mut b);
                fauna_nest::account_handlers::register_account_handlers(&mut b);
                fauna_nest::admin_ws_handlers::register_admin_handlers(&mut b);
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
            ..AppState::for_test(db)
        });
        let app = fauna_nest::build_router(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Self {
            base: format!("http://{authority}"),
            state,
            _blob: blob,
        }
    }

    fn id(&self) -> [u8; 32] {
        self.state.bound_identity()
    }

    /// The account the retired identity holds here — what its sign-up made.
    async fn register_old(&self) {
        self.state
            .db
            .create_user(&old().actor_id().0, "free", "alice")
            .await
            .unwrap();
    }
}

/// A recovery kit for the retired identity at the nest behind `client`.
async fn kit_at(client: &Arc<NestClient>) {
    fauna_client_recovery::create_kit_with_root(
        &fauna_client_recovery::RecoveryClient::new(Arc::clone(client)),
        &old(),
        None,
        RecoveryKey::from_bytes(ROOT),
        &[],
    )
    .await
    .expect("the kit ceremony lands");
}

fn link_both(other_nest_url: &str) -> LinkedNestsAction {
    LinkedNestsAction::LinkBoth {
        other_nest_url: other_nest_url.to_string(),
        capabilities: vec![],
        expires_at: None,
        label: None,
    }
}

/// The succession ceremony's submit at `nest`: the statement the kit's key
/// signs, over an anonymous connection (the kind is pre-identity).
async fn succeed_at(nest: &Nest) -> Vec<u8> {
    let statement = common::succession_bytes(
        &RecoveryKey::from_bytes(ROOT),
        old().actor_id().0,
        &SigningKey::from_bytes(&NEW_SEED),
        Some(&SigningKey::from_bytes(&OLD_SEED)),
        2,
    );
    let anon = fauna_anon_client::AnonymousNestClient::connect(&nest.base)
        .await
        .expect("an anonymous connection");
    submit_statement(&anon, &statement)
        .await
        .expect("the succession lands at H");
    statement
}

/// The successor's seed-holding runtime on the machine `base/<name>`, bound to
/// H through `bound`, with the production linked-nest connector and the
/// production owed-nest deliverer — the principal holding the retired seeds in
/// `held`, and the deliverer their copy, as `build_params` assembles them.
async fn successor_runtime(
    base: &Path,
    name: &str,
    bound: Arc<NestClient>,
    pin: [u8; 32],
    held: &[[u8; 32]],
) -> AccountStoreHandle {
    let principal = SeedHolder::new(new())
        .with_predecessors(held.iter().map(|s| ActorKeypair::from_secret(*s)).collect());
    let seeds = PredecessorSeeds::of(&principal);
    AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join(name).join("state")),
        actor_id_hex: new().actor_id_hex(),
        rpc: bound,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(principal),
        credentials: CredentialStore::with_file_backend(
            CRED_NAMESPACE,
            base.join(name).join("creds"),
        ),
        reconnects: None,
        pushes: None,
        // Disarmed: every pass below is driven.
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fixed_holders(vec![pin]),
        attested_predecessors: Default::default(),
        linked_nests: Some(native_linked_nest_connector(new())),
        owed_nests: Some(native_owed_nest_deliverer(seeds)),
        peer_transport: None,
        enrollment_target_device_id: format!("{:0<64}", hex::encode(name.as_bytes())),
    })
    .await
    .expect("the successor's runtime assembles")
}

/// The deliveries one pass made at the keeper `keeper` (`None`: the bound
/// nest), as `(owed nest id, delivery)`.
fn delivered_at(report: &PumpReport, keeper: Option<[u8; 32]>) -> Vec<(Vec<u8>, Delivery)> {
    report
        .owed_nests
        .iter()
        .filter(|p| p.keeper == keeper)
        .flat_map(|p| {
            assert!(p.outcome.is_ok(), "the owed list was read: {:?}", p.outcome);
            p.deliveries()
                .iter()
                .map(|(owed, d)| (owed.nest_id.to_vec(), d.clone()))
        })
        .collect()
}

/// The refusal a sign-in as `keypair` at `nest` meets, or `None` when it
/// signs in.
async fn sign_in_refusal(nest: &Nest, keypair: ActorKeypair) -> Option<String> {
    let client = NestClient::new(nest.base.clone(), keypair);
    match client.connect().await {
        Ok(()) => None,
        Err(e) => Some(
            client
                .auth()
                .last_auth_refusal()
                .map_or_else(|| format!("transport: {e}"), |r| r.code),
        ),
    }
}

/// The owed entries `nest` serves the successor.
async fn owed_at(nest: &Nest) -> usize {
    let client = connected_client(&nest.base, new()).await;
    fetch_owed_nests(&client).await.expect("status").len()
}

const PASS_BUDGET: usize = 12;

/// The main road. See the module docs.
#[tokio::test]
async fn the_successors_runtime_delivers_to_a_linked_nest_and_a_seed_only_reader_recovers_there() {
    let tmp = tempfile::tempdir().unwrap();
    let (h, l) = (Nest::start(0x66).await, Nest::start(0x77).await);
    h.register_old().await;
    l.register_old().await;
    let old_at_h = connected_client(&h.base, old()).await;
    kit_at(&old_at_h).await;
    fauna_client_pair::build_linked_nests_machine(Arc::clone(&old_at_h))
        .dispatch(link_both(&l.base))
        .await
        .expect("the link carries the chain and seeds both ends");
    drop(old_at_h);

    let statement = succeed_at(&h).await;
    assert_eq!(owed_at(&h).await, 1, "precondition: H keeps L as owed");
    assert_eq!(
        sign_in_refusal(&l, old()).await,
        None,
        "precondition: L has not heard yet"
    );

    // The successor's device: bound to H, holding no predecessor seed — L
    // holds the chain, so none is needed.
    let at_h = connected_client(&h.base, new()).await;
    let d = successor_runtime(tmp.path(), "d", Arc::clone(&at_h), h.id(), &[]).await;
    // The runtime's own first pass — the prologue, which runs once it is up
    // — is the one that delivers; `settled()` is its barrier. Its report is
    // not handed out, so what it did is read where it lands.
    d.settled().await;
    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        report.owed_nests.len(),
        1,
        "every full pass reads the bound nest's owed list: {:?}",
        report.owed_nests
    );
    assert!(
        delivered_at(&report, None).is_empty(),
        "and finds nothing left owed there"
    );
    assert_eq!(
        sign_in_refusal(&l, old()).await.as_deref(),
        Some(fauna_protocol::RpcError::CODE_SUPERSEDED),
        "L refuses the retired key"
    );
    assert_eq!(
        sign_in_refusal(&l, new()).await,
        None,
        "and signs the successor in"
    );
    let anon_at_l = fauna_anon_client::AnonymousNestClient::connect(&l.base)
        .await
        .unwrap();
    assert_eq!(
        fetch_statement_path(&anon_at_l, &old().actor_id().0)
            .await
            .unwrap(),
        vec![statement],
        "L serves the very statement"
    );
    assert_eq!(owed_at(&h).await, 0, "H's entry is settled");

    // The burn asks the successor to link L again; the next passes complete it
    // as a replica — and read L's own owed list, which names H (the pairing L
    // burned), where the statement already is.
    fauna_client_pair::build_linked_nests_machine(Arc::clone(&at_h))
        .dispatch(link_both(&l.base))
        .await
        .expect("the successor links L again");
    let entry = DeploymentSeedEntry {
        nest_actor_id: DeploymentSeedEntry::nest_actor_id_for_seed([0x5F; 32]),
        seed: [0x5F; 32].into(),
        domain: Some("box.example".into()),
        ..Default::default()
    };
    let mut merged = false;
    let mut at_l = Vec::new();
    let mut completed = false;
    for _ in 0..PASS_BUDGET {
        let report = d.reconcile_now().await.expect("pass");
        at_l.extend(delivered_at(&report, Some(l.id())));
        if !merged {
            merged = d.merge_deployment_seeds(vec![entry.clone()]).await.is_ok();
            continue;
        }
        completed = report.linked.as_ref().is_some_and(|p| {
            !p.nests.is_empty()
                && p.nests.iter().all(
                    |n| matches!(&n.outcome, LinkedOutcome::Completed(c) if c.errors.is_empty()),
                )
        }) && !l
            .state
            .db
            .get_generation_escrow_wraps(&new().actor_id().0, None)
            .await
            .unwrap()
            .is_empty()
            && d.deployment_seed_published(entry.nest_actor_id)
                .await
                .unwrap();
        if completed {
            // One more whole pass, so the row this pass shipped to the bound
            // nest reaches L by the leg's diff.
            let report = d.reconcile_now().await.expect("pass");
            at_l.extend(delivered_at(&report, Some(l.id())));
            break;
        }
    }
    assert!(merged, "the custody write found a tip to seal under");
    assert!(completed, "the secondary leg completes L");
    assert_eq!(
        at_l.first(),
        Some(&(
            h.id().to_vec(),
            Delivery::Landed {
                landed: 0,
                replayed: 0
            }
        )),
        "L's own owed entry for H is delivered — already there — and settled: {at_l:?}"
    );
    assert_eq!(owed_at(&l).await, 0, "L's owed list is empty too");

    d.shutdown().await;
    drop(at_h);
    // H is lost, and so is the device: a reader holding only the successor's
    // seed recovers from L.
    let reader = connected_client(&l.base, new()).await;
    let read = cold_read_deployment_seeds(&*reader, &NEW_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        read,
        vec![entry],
        "the seed-only reader recovers the account plane from L"
    );
}

/// The chain-less arm. See the module docs.
#[tokio::test]
async fn a_linked_nest_holding_no_chain_is_replayed_it_with_the_held_predecessor_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let (h, l) = (Nest::start(0x66).await, Nest::start(0x77).await);
    h.register_old().await;
    l.register_old().await;
    // Linked before any kit was made: no link carried a chain to L, and the
    // kit made afterwards was made at H alone (no runtime of the retired
    // identity ran a pass to carry it).
    h.state
        .db
        .store_pairing(
            &old().actor_id().0,
            &l.id(),
            &fauna_protocol::pair::default_self_sync(),
            None,
            Some(&l.base),
            None,
        )
        .await
        .unwrap();
    let old_at_h = connected_client(&h.base, old()).await;
    kit_at(&old_at_h).await;
    drop(old_at_h);
    assert!(
        l.state
            .db
            .list_recovery_registrations(&old().actor_id().0)
            .await
            .unwrap()
            .is_empty(),
        "precondition: L holds no chain"
    );
    succeed_at(&h).await;

    // A device of the successor that holds no seed of the retired identity:
    // nothing at L can verify, and the entry stays owed.
    let at_h = connected_client(&h.base, new()).await;
    let seedless = successor_runtime(tmp.path(), "seedless", Arc::clone(&at_h), h.id(), &[]).await;
    let report = seedless.reconcile_now().await.expect("pass");
    assert_eq!(
        delivered_at(&report, None),
        vec![(
            l.id().to_vec(),
            Delivery::Owed(OwedReason::NoSeed {
                code: "fauna.recovery.not_registered".into()
            })
        )],
        "{:?}",
        report.owed_nests
    );
    seedless.shutdown().await;
    assert_eq!(owed_at(&h).await, 1, "owed, never dropped");
    assert_eq!(sign_in_refusal(&l, old()).await, None);

    // The device that ran the ceremony holds the retired seed: its pass signs
    // in at L as that identity, replays H's chain there, and the statement
    // lands.
    let holder =
        successor_runtime(tmp.path(), "holder", Arc::clone(&at_h), h.id(), &[OLD_SEED]).await;
    holder.settled().await;
    assert_eq!(
        l.state
            .db
            .list_recovery_registrations(&old().actor_id().0)
            .await
            .unwrap()
            .len(),
        1,
        "H's chain for the retired identity was replayed at L"
    );
    assert_eq!(
        sign_in_refusal(&l, old()).await.as_deref(),
        Some(fauna_protocol::RpcError::CODE_SUPERSEDED)
    );
    assert_eq!(sign_in_refusal(&l, new()).await, None);
    assert_eq!(owed_at(&h).await, 0);
    holder.shutdown().await;
}

/// A nest the burned pairing named that holds no account for the retired
/// identity is settled: the statement cannot verify there, the native
/// sign-in as the retired identity is refused `not_registered`, and that
/// refusal — typed, though a refused mint flattens to "couldn't obtain a
/// token" — reads as the account being absent. Red-verified: read off the
/// flattened error instead, the sign-in reads as a failure and the entry
/// stays owed.
#[tokio::test]
async fn an_owed_nest_holding_no_account_for_the_retired_identity_is_settled() {
    let tmp = tempfile::tempdir().unwrap();
    let (h, l) = (Nest::start(0x66).await, Nest::start(0x77).await);
    h.register_old().await;
    h.state
        .db
        .store_pairing(
            &old().actor_id().0,
            &l.id(),
            &fauna_protocol::pair::default_self_sync(),
            None,
            Some(&l.base),
            None,
        )
        .await
        .unwrap();
    let old_at_h = connected_client(&h.base, old()).await;
    kit_at(&old_at_h).await;
    drop(old_at_h);
    succeed_at(&h).await;
    assert_eq!(owed_at(&h).await, 1, "precondition: L is owed");

    let at_h = connected_client(&h.base, new()).await;
    let d = successor_runtime(tmp.path(), "d", Arc::clone(&at_h), h.id(), &[OLD_SEED]).await;
    d.settled().await;
    assert_eq!(owed_at(&h).await, 0, "the entry is settled");
    assert_eq!(
        sign_in_refusal(&l, old()).await.as_deref(),
        Some("fauna.auth.not_registered"),
        "and nothing was made at L"
    );
    d.shutdown().await;
}

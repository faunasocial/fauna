//! **The capability reconcile sweep as a step of the full pump pass** (tier_3,
//! real nest handlers + real account runtimes) — `docs/goal/ui/nests.md`
//! § Trust facet — grants → *Reconcile*, the *Exactly one consumer* bullet and
//! the *Automatic and silent, and only where the walk is* bullet (re-ruled
//! 2026-10-06 for the fleet).
//!
//! The sweep judges a nest-held grant id against this replica's copy of the
//! signed grant log, and that copy lags a sibling's fresh `Mint` until the
//! pass's fleet walk pulls it. So the sweep enumerates before the fleet walk,
//! judges after it, and runs nowhere else. The page refresh does not sweep.
//!
//! The flows this file drives:
//!
//! - **(a) the lag pin, PROBE-801-24-A at the pass level:** two runtimes for
//!   one account on one nest. A records a `Mint` through the handle's ledger
//!   door, releases the blob against what the door stored, and the row is
//!   deposited. B still holds a replica without that `Mint`. B's Nests page
//!   hydrates over B's own handle (the page-refresh shape — red while the page
//!   swept), then B runs a full pass: no revoke reaches the nest, and B's log
//!   now holds the `Mint`.
//! - **(b) a true orphan and a resurrected row are still swept** by a pass;
//!   the recognized grant survives and the signed log is unchanged by the sweep.
//! - **(c) order:** the enumerate precedes the fleet walk, and the revoke the
//!   judge fires follows it (the requester's own trace).
//! - **(d) no sweep from a pass whose fleet walk failed, and none from a
//!   non-holder's `reconcile_now`.**
//!
//! ⚠ **The deposits are seeded into `capability_grants`, deliberately** (as the
//! page-level sweep test this file replaces did): the subject is the judgement
//! over nest rows, and an orphan is a state no well-behaved mint chain
//! produces; the mint chain itself is `conformance_capability_trust_client.rs`'s. Pin (a)'s `Mint` itself crosses the
//! production ledger door; its blob is released only against the stored log.
//!
//! Every positive wait is a named-budget deadline poll (convention 14).

mod common;
use common::connected_client;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_client_capabilities::grant_log::{self, RecordedGrants, UndepositedGrant};
use fauna_client_config::SuccessionLedgerStore;
use fauna_core::grant_event::{GrantEvent, GrantEventScope};
use fauna_core::identity::ActorKeypair;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_credential_store::CredentialStore;
use fauna_nest::backup::service::BackupService;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_nest::{db::CacheDb, folder_handlers, rpc_router::RpcRouter, sync_handlers};
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::merge_policy::KIND_DEVICE_ENDPOINTS;
use fauna_protocol::sync::SyncChangesListRequest;
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE, CapabilitySweep,
    RuntimePrincipal, StoreRoot,
};

// ── One account ─────────────────────────────────────────────────────────────

fn account() -> ActorKeypair {
    ActorKeypair::from_secret([0x47; 32])
}

fn actor_bytes() -> [u8; 32] {
    account().actor_id().0
}

/// The deployment identity the nest signs escrow receipts with, pinned by
/// every runtime as its trusted holder — the shape a `GenerationTip` kind
/// (the succession ledger among them) needs to seal at all.
fn deployment_key() -> SigningKey {
    SigningKey::from_bytes(&[0x68; 32])
}

/// The grant holder every event and row names. The holder process never runs.
const HOLDER: [u8; 32] = [0x5Au8; 32];

const RECOGNIZED: [u8; 16] = [0xA1u8; 16];
const ORPHAN: [u8; 16] = [0xB2u8; 16];
const RESURRECTED: [u8; 16] = [0xC3u8; 16];
const FRESH: [u8; 16] = [0xD4u8; 16];

fn label_write_scope() -> GrantEventScope {
    GrantEventScope {
        class: "label.write".into(),
        kind: None,
        tier: None,
    }
}

// ── The transport: the real router, dispatched in-process, traced ──────────

/// One request the runtime sent: its kind, and for a feed listing the scope it
/// listed.
type Traced = (String, Option<String>);

struct RouterRequester {
    state: Arc<AppState>,
    trace: Mutex<Vec<Traced>>,
    /// Refuse every listing of the fleet scope — the pass's fleet walk errs.
    refuse_fleet_list: AtomicBool,
}

#[derive(Debug)]
struct Refused(RpcError);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.0.code, self.0.message)
    }
}

impl RpcErrorClass for Refused {
    fn is_rejection(&self) -> bool {
        true
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        Some(&self.0)
    }
}

impl RouterRequester {
    fn take_trace(&self) -> Vec<Traced> {
        std::mem::take(&mut *self.trace.lock().unwrap())
    }
}

impl RpcRequester for RouterRequester {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        let scope = (kind == "fauna.sync.changes.list")
            .then(|| fauna_protocol::decode_strict::<SyncChangesListRequest>(&bytes).ok())
            .flatten()
            .and_then(|r| r.scope);
        let fleet_list = scope.as_deref() == Some(ACCOUNT_STATE_FLEET_SCOPE);
        self.trace.lock().unwrap().push((kind.to_string(), scope));
        if fleet_list && self.refuse_fleet_list.load(Ordering::SeqCst) {
            return Err(Refused(RpcError::new(
                "unavailable",
                "test.router.fleet_list_refused",
            )));
        }
        let Some(meta) = self.state.rpc_router.kind_meta(kind) else {
            return Err(Refused(RpcError::new(
                "kind_not_served",
                "test.router.kind_not_served",
            )));
        };
        let reply = (meta.handler)(Arc::clone(&self.state), actor_bytes(), bytes)
            .await
            .map_err(Refused)?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }
}

impl fauna_protocol::KeyedRpcRequester for RouterRequester {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

/// A real nest serving both halves: the account runtimes' state, escrow and
/// capability kinds (dispatched in-process through [`RouterRequester`]), and —
/// listening on a socket — what the Nests page's machine drives (auth,
/// discovery, pairings, the capability kinds).
async fn nest() -> (
    Arc<RouterRequester>,
    String,
    Arc<AppState>,
    tempfile::TempDir,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    common::seed_user(&db, &actor_bytes()).await;
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let mut b = RpcRouter::builder();
    fauna_nest::auth_handlers::register_auth_handlers(&mut b);
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    fauna_nest::pair_handlers::register_pair_handlers(&mut b);
    fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
    fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    let state = Arc::new(AppState {
        rpc_router: Arc::new(b.build()),
        backup_service: Some(backup_svc),
        nest_signing_key: Some(deployment_key()),
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
    let app = fauna_nest::build_router(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let rpc = Arc::new(RouterRequester {
        state: Arc::clone(&state),
        trace: Mutex::new(Vec::new()),
        refuse_fleet_list: AtomicBool::new(false),
    });
    (rpc, format!("http://{authority}"), state, tmp)
}

fn device_row(device: &str) -> String {
    format!("{:0<64}", hex::encode(device.as_bytes()))
}

/// One device of the account, trusting the deployment as its escrow holder.
/// The backstop is disarmed and nothing pushes, so every pass in this file is
/// a `reconcile_now` the test asked for.
fn params(
    base: &Path,
    device: &str,
    rpc: &Arc<RouterRequester>,
) -> AccountRuntimeParams<Arc<RouterRequester>> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join(device).join("state")),
        actor_id_hex: account().actor_id_hex(),
        rpc: Arc::clone(rpc),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(account().into()),
        credentials: CredentialStore::with_file_backend(
            CRED_NAMESPACE,
            base.join(device).join("creds"),
        ),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(vec![
            deployment_key().verifying_key().to_bytes(),
        ]),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: device_row(device),
    }
}

const CONVERGENCE_BUDGET: Duration = Duration::from_secs(30);

async fn eventually_or<F, Fut, D, DFut>(what: &str, mut probe: F, diagnose: D)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
    D: FnOnce() -> DFut,
    DFut: Future<Output = String>,
{
    if tokio::time::timeout(CONVERGENCE_BUDGET, async {
        while !probe().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_err()
    {
        let report = diagnose().await;
        panic!("eventually({what}): not reached within budget. Diagnostic: {report}");
    }
}

/// How many device-endpoints rows this replica opens — the evidence that a
/// generation tip every listed device keys has settled.
async fn opened_endpoint_rows(handle: &AccountStoreHandle) -> usize {
    handle
        .states_of_kind(KIND_DEVICE_ENDPOINTS)
        .await
        .expect("states_of_kind")
        .into_iter()
        .filter(|e| !e.tombstone)
        .count()
}

/// Pass every runtime until each opens `devices` endpoint rows: the fleet's
/// tip covers them all, so a tip-sealed row any one writes, every one opens.
async fn settle(handles: &[&AccountStoreHandle], devices: usize) {
    eventually_or(
        "every replica opens every device's row",
        || async {
            let mut all = true;
            for h in handles {
                h.reconcile_now().await.expect("pass");
                all &= opened_endpoint_rows(h).await == devices;
            }
            all
        },
        || async {
            let mut counts = Vec::new();
            for h in handles {
                counts.push(opened_endpoint_rows(h).await);
            }
            format!("opened endpoint rows per replica: {counts:?}")
        },
    )
    .await;
}

/// Record grant events through the runtime's own ledger door: load, append,
/// merge — answering the ledger the door stored.
async fn record(
    handle: &AccountStoreHandle,
    append: impl FnOnce(&mut SuccessionLedger),
) -> SuccessionLedger {
    let mut ledger = SuccessionLedgerStore::load(handle)
        .await
        .expect("load the ledger");
    append(&mut ledger);
    SuccessionLedgerStore::merge(handle, ledger)
        .await
        .expect("the ledger door stores the events")
}

async fn grant_events(handle: &AccountStoreHandle) -> Vec<GrantEvent> {
    SuccessionLedgerStore::load(handle)
        .await
        .expect("load the ledger")
        .grant_events
}

fn holds_event(events: &[GrantEvent], id: [u8; 16]) -> bool {
    events
        .iter()
        .any(|e| e.grant_id.as_slice() == id.as_slice())
}

async fn deposit(state: &AppState, id: [u8; 16], now: u64) {
    state
        .db
        .put_capability_grant(&actor_bytes(), &id, &HOLDER, (now + 86_400) as i64, b"blob")
        .await
        .expect("deposit the row");
}

async fn rows_on_nest(state: &AppState) -> Vec<Vec<u8>> {
    let mut ids = state
        .db
        .fetch_capability_grant_ids_for_owner(&actor_bytes())
        .await
        .expect("read the owner's rows");
    ids.sort();
    ids
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

// ── (a) the lag pin ─────────────────────────────────────────────────────────

/// **PROBE-801-24-A at the pass level.** A sibling whose replica lags a fresh
/// `Mint` must never revoke it: not at its page refresh (which no longer
/// sweeps), and not at its pass (whose fleet walk pulls the `Mint` before the
/// judge reads the log).
///
/// Red on the page-refresh shape: while `LinkedNestsMachine::hydrate()` swept,
/// B's hydrate below judged the fresh row against B's lagging replica and
/// revoked it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sibling_whose_replica_lags_a_fresh_mint_never_revokes_it() {
    let (rpc, base_url, state, tmp) = nest().await;
    let a = AccountStoreRuntime::start(params(tmp.path(), "a", &rpc))
        .await
        .expect("start A");
    let b = AccountStoreRuntime::start(params(tmp.path(), "b", &rpc))
        .await
        .expect("start B");
    settle(&[&a, &b], 2).await;

    // A mints through the record-then-deposit order: the event is recorded by
    // the ledger door, the blob is released only against what that door
    // stored, and its own pass publishes the row before the deposit lands.
    let now = now_secs();
    let stored = record(&a, |ledger| {
        grant_log::record_mint(
            ledger,
            account().signing_key(),
            FRESH,
            HOLDER,
            vec![label_write_scope()],
            now,
            now + 86_400,
            now,
        )
        .expect("record the Mint");
    })
    .await;
    UndepositedGrant::new(FRESH, b"blob".to_vec())
        .release(&RecordedGrants::from_stored(&stored))
        .expect("the stored log records the Mint");
    a.reconcile_now()
        .await
        .expect("A's pass publishes the Mint");
    deposit(&state, FRESH, now).await;

    // Precondition — the lag: B's replica does not hold the Mint yet.
    assert!(
        !holds_event(&grant_events(&b).await, FRESH),
        "precondition: B's replica lags A's fresh Mint"
    );

    // B's Nests page over B's own handle: the page-refresh shape.
    let nest_client = connected_client(&base_url, account()).await;
    let machine = fauna_client_pair::build_linked_nests_machine_with_trust(
        Arc::clone(&nest_client),
        account(),
        Arc::new(b.clone()),
        Arc::new(fauna_client_config::test_helpers::FakeBackupStateStore::empty()),
        Arc::new(fauna_client_pair::NoAccountRuntime),
        fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
    );
    machine.hydrate().await.expect("hydrate B's Nests page");
    assert_eq!(
        rows_on_nest(&state).await,
        vec![FRESH.to_vec()],
        "B's page refresh revoked nothing — the page neither enumerates nor judges"
    );

    // B's full pass: the enumerate names the row, the fleet walk brings the
    // Mint, the judge recognizes it.
    let report = b.reconcile_now().await.expect("B's pass");
    assert_eq!(
        report.capability_sweep,
        Some(CapabilitySweep {
            enumerated: 1,
            revoked: 0,
            skipped: None,
        }),
        "B's pass judged the row and revoked nothing (errors: {:?})",
        report.errors
    );
    assert_eq!(
        rows_on_nest(&state).await,
        vec![FRESH.to_vec()],
        "no revoke reached the nest"
    );
    assert!(
        holds_event(&grant_events(&b).await, FRESH),
        "B's log now holds A's Mint"
    );

    a.shutdown().await;
    b.shutdown().await;
}

// ── (b) a true orphan and a resurrection are still swept ────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pass_sweeps_the_orphan_and_the_resurrection_and_leaves_the_log_alone() {
    let (rpc, _base_url, state, tmp) = nest().await;
    let a = AccountStoreRuntime::start(params(tmp.path(), "a", &rpc))
        .await
        .expect("start A");
    settle(&[&a], 1).await;

    let now = now_secs();
    record(&a, |ledger| {
        let key = account();
        for id in [RECOGNIZED, RESURRECTED] {
            grant_log::record_mint(
                ledger,
                key.signing_key(),
                id,
                HOLDER,
                vec![label_write_scope()],
                now,
                now + 86_400,
                now,
            )
            .expect("record a Mint");
        }
        grant_log::record_revoke(ledger, key.signing_key(), RESURRECTED, HOLDER, now + 1)
            .expect("record the Revoke");
    })
    .await;
    let log_before = grant_events(&a).await;
    assert_eq!(log_before.len(), 3, "two mints and one revoke are recorded");
    for id in [RECOGNIZED, ORPHAN, RESURRECTED] {
        deposit(&state, id, now).await;
    }

    let report = a.reconcile_now().await.expect("A's pass");
    assert_eq!(
        report.capability_sweep,
        Some(CapabilitySweep {
            enumerated: 3,
            revoked: 2,
            skipped: None,
        }),
        "errors: {:?}",
        report.errors
    );
    assert_eq!(
        rows_on_nest(&state).await,
        vec![RECOGNIZED.to_vec()],
        "the orphan and the resurrection are revoked; the recognized grant stays"
    );
    assert_eq!(
        grant_events(&a).await,
        log_before,
        "the sweep appends no GrantEvent — the log is unchanged across it"
    );

    // Converged: the next pass enumerates the survivor and revokes nothing.
    let report = a.reconcile_now().await.expect("A's next pass");
    assert_eq!(
        report.capability_sweep,
        Some(CapabilitySweep {
            enumerated: 1,
            revoked: 0,
            skipped: None,
        })
    );
    a.shutdown().await;
}

// ── (c) order ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_enumerate_precedes_the_fleet_walk_and_the_revoke_follows_it() {
    let (rpc, _base_url, state, tmp) = nest().await;
    let a = AccountStoreRuntime::start(params(tmp.path(), "a", &rpc))
        .await
        .expect("start A");
    settle(&[&a], 1).await;
    deposit(&state, ORPHAN, now_secs()).await;

    rpc.take_trace();
    let report = a.reconcile_now().await.expect("A's pass");
    let trace = rpc.take_trace();
    assert_eq!(
        report.capability_sweep.map(|s| s.revoked),
        Some(1),
        "errors: {:?}",
        report.errors
    );
    let at = |pred: &dyn Fn(&Traced) -> bool, from: usize| {
        trace[from..]
            .iter()
            .position(pred)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("not in the trace after {from}: {trace:?}"))
    };
    let enumerate = at(&|(k, _)| k == "fauna.capabilities.reconcile", 0);
    let fleet_walk = at(
        &|(k, s)| k == "fauna.sync.changes.list" && s.as_deref() == Some(ACCOUNT_STATE_FLEET_SCOPE),
        enumerate,
    );
    let revoke = at(&|(k, _)| k == "fauna.capabilities.revoke", 0);
    assert!(
        enumerate < fleet_walk && fleet_walk < revoke,
        "enumerate ({enumerate}) → fleet walk ({fleet_walk}) → revoke ({revoke}): {trace:?}"
    );
    a.shutdown().await;
}

// ── (d) no sweep where the walk is not ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pass_whose_fleet_walk_failed_revokes_nothing() {
    let (rpc, _base_url, state, tmp) = nest().await;
    let a = AccountStoreRuntime::start(params(tmp.path(), "a", &rpc))
        .await
        .expect("start A");
    settle(&[&a], 1).await;
    deposit(&state, ORPHAN, now_secs()).await;

    rpc.refuse_fleet_list.store(true, Ordering::SeqCst);
    let report = a.reconcile_now().await.expect("A's pass");
    rpc.refuse_fleet_list.store(false, Ordering::SeqCst);
    assert!(report.fleet_walk.is_none(), "the fleet walk erred");
    assert_eq!(
        report.capability_sweep,
        Some(CapabilitySweep {
            enumerated: 1,
            revoked: 0,
            skipped: Some("fleet walk did not complete"),
        })
    );
    assert_eq!(rows_on_nest(&state).await, vec![ORPHAN.to_vec()]);

    // The next pass, whose walk completes, sweeps it.
    let report = a.reconcile_now().await.expect("A's next pass");
    assert_eq!(report.capability_sweep.map(|s| s.revoked), Some(1));
    assert!(rows_on_nest(&state).await.is_empty());
    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_holders_reconcile_now_never_sweeps() {
    let (rpc, _base_url, state, tmp) = nest().await;
    // ONE device area, two runtimes: the multi-instance shape.
    let holder = AccountStoreRuntime::start(params(tmp.path(), "shared", &rpc))
        .await
        .expect("start the holder");
    let other = AccountStoreRuntime::start(params(tmp.path(), "shared", &rpc))
        .await
        .expect("start the second runtime");
    settle(&[&holder], 1).await;
    deposit(&state, ORPHAN, now_secs()).await;

    let report = other.reconcile_now().await.expect("the role answer");
    assert!(
        report.skipped_non_holder,
        "the second runtime holds no role"
    );
    assert_eq!(report.capability_sweep, None, "a non-holder never sweeps");
    assert_eq!(rows_on_nest(&state).await, vec![ORPHAN.to_vec()]);

    holder.shutdown().await;
    other.shutdown().await;
}

//! The participation door reaches the engine holder wherever it lives
//! (`p2p.md` § Per-device participation → *Enforcement*, (c) Promptness): a
//! door over the holding runtime runs the pass itself, and a door over a
//! runtime beside it — the app next to its co-located sync agent — asks the
//! seat's [`EngineHolderNudge`] for that pass, after the row rests.
//!
//! Two runtimes over one store in one process reproduce the desktop's
//! arbitration exactly (`flock` is per open file description): the first
//! holds, the second is a non-holder that runs no pass.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fauna_client_account_runtime::p2p_participation::{
    EngineHolderNudge, EngineHolderNudgeSlot, RuntimeP2pParticipation,
};
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_devices_machine::P2pParticipation;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE,
    RuntimePrincipal, StoreRoot,
};

/// A transport fault the pump absorbs — there is no nest in this test.
#[derive(Debug)]
struct NoNestError(&'static str);

impl std::fmt::Display for NoNestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no nest in this test: {}", self.0)
    }
}

impl RpcErrorClass for NoNestError {
    fn is_rejection(&self) -> bool {
        false
    }
}

#[derive(Clone)]
struct NoNest;

impl RpcRequester for NoNest {
    type Error = NoNestError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        Err(NoNestError(kind))
    }
}

impl fauna_protocol::KeyedRpcRequester for NoNest {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        Err(NoNestError(kind))
    }
}

/// One machine's store: the same root and credential slot for every runtime
/// started on it, the shape a co-located app + agent pair have. The backstop
/// is an hour away, so every pass counted here is one something asked for.
fn params(base: &Path) -> AccountRuntimeParams<NoNest> {
    let root = ActorKeypair::from_secret([0x3cu8; 32]);
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join("state")),
        actor_id_hex: root.actor_id_hex(),
        rpc: NoNest,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(root.into()),
        credentials: CredentialStore::with_file_backend(CRED_NAMESPACE, base.join("creds")),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: "ab".repeat(32),
    }
}

/// The holder in the other process, as the door sees it: a nudge that runs
/// the holder's pass and counts the asks — what the agent's
/// `ReconcileAccountRuntime` does on the far side of the socket.
struct HolderPass {
    holder: AccountStoreHandle,
    asked: AtomicUsize,
}

#[async_trait::async_trait]
impl EngineHolderNudge for HolderPass {
    async fn nudge_engine_holder(&self) {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let _ = self.holder.reconcile_now().await;
    }

    async fn walk_fleet_for_non_holder(&self) -> Result<(), String> {
        self.holder.walk_fleet().await.map_err(|e| format!("{e:#}"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_switch_beside_the_holder_asks_the_holder_for_its_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let holder = AccountStoreRuntime::start(params(tmp.path()))
        .await
        .expect("the holder starts");
    holder
        .reconcile_now()
        .await
        .expect("the holder's first pass");
    assert!(holder.is_engine_holder());
    let app = AccountStoreRuntime::start(params(tmp.path()))
        .await
        .expect("the app's runtime beside it");
    assert!(!app.is_engine_holder(), "the lock is the holder's");

    let pass = Arc::new(HolderPass {
        holder: holder.clone(),
        asked: AtomicUsize::new(0),
    });
    let nudge: Arc<dyn EngineHolderNudge> = pass.clone();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    let source_app = app.clone();
    let door = RuntimeP2pParticipation::new(move || Some(source_app.clone()))
        .with_holder_nudge(slot.clone());

    let (_, before) = holder.pump_cycles();
    door.set_local(false).await.expect("the switch rests");
    assert_eq!(pass.asked.load(Ordering::SeqCst), 1, "the holder was asked");
    let (_, after) = holder.pump_cycles();
    assert!(after > before, "the holder ran its pass within the gesture");
    assert!(
        !holder.p2p_participation().await.unwrap().effective(),
        "the row the holder's pass reads is the one the app rested"
    );

    // The provisioner gone (sign-out): the slot reads absent and the switch
    // still rests — the holder's backstop reads the row.
    drop(nudge);
    drop(pass);
    door.set_local(true)
        .await
        .expect("the switch rests with no nudge");
    assert!(holder.p2p_participation().await.unwrap().effective());

    app.shutdown().await;
    holder.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_switch_in_the_holder_runs_its_own_pass_and_asks_no_one() {
    let tmp = tempfile::tempdir().unwrap();
    let holder = AccountStoreRuntime::start(params(tmp.path()))
        .await
        .expect("the holder starts");
    holder.reconcile_now().await.expect("the first pass");
    assert!(holder.is_engine_holder());

    let pass = Arc::new(HolderPass {
        holder: holder.clone(),
        asked: AtomicUsize::new(0),
    });
    let nudge: Arc<dyn EngineHolderNudge> = pass.clone();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    let source = holder.clone();
    let door = RuntimeP2pParticipation::new(move || Some(source.clone())).with_holder_nudge(slot);

    let (_, before) = holder.pump_cycles();
    door.set_local(false).await.expect("the switch rests");
    let (_, after) = holder.pump_cycles();
    assert!(after > before, "the door ran the holder's pass itself");
    assert_eq!(
        pass.asked.load(Ordering::SeqCst),
        0,
        "no other process is asked"
    );

    holder.shutdown().await;
}

// ── The yield's edge (`account-runtime.md` § Multi-instance concurrency → *The
// agent holds the role when present*, part 4) ─────────────────────────────
//
// An app runtime that hands the engine role to its arriving agent asks the
// agent for its pass at once, through the same seat the switch above uses, so
// the freed role is taken now rather than at the agent's backstop tick. The
// watch every desktop host runs is `holder_yield::spawn_holder_yield_nudge`,
// spawned by `resolve_and_start`; these drive it over a real runtime's role.

/// A nudge that only counts — the agent on the far side of the socket, whose
/// own pass is pinned in the agent's `reconcile_account_runtime_tests`.
struct CountingNudge {
    asked: Arc<AtomicUsize>,
    asked_signal: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl EngineHolderNudge for CountingNudge {
    async fn nudge_engine_holder(&self) {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.asked_signal.notify_waiters();
    }

    async fn walk_fleet_for_non_holder(&self) -> Result<(), String> {
        Err("this test's nudge walks nothing".into())
    }
}

fn counting_nudge() -> (
    Arc<dyn EngineHolderNudge>,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
) {
    let asked = Arc::new(AtomicUsize::new(0));
    let asked_signal = Arc::new(tokio::sync::Notify::new());
    let nudge: Arc<dyn EngineHolderNudge> = Arc::new(CountingNudge {
        asked: Arc::clone(&asked),
        asked_signal: Arc::clone(&asked_signal),
    });
    (nudge, asked, asked_signal)
}

/// The agent's presence lock on this machine's store, as its mount takes it.
fn agent_present(base: &Path) -> fauna_account_store::locks::AgentPresenceLock {
    let p = params(base);
    let store_dir = p
        .store_root
        .store_dir(&p.actor_id_hex)
        .expect("the store dir");
    match fauna_account_store::locks::AgentPresenceLock::acquire(&store_dir) {
        fauna_account_store::locks::AgentPresenceLockOutcome::Held(lock) => lock,
        other => panic!("the agent's lock: {other:?}"),
    }
}

/// Long enough for a spawned watch to have reacted to an edge it saw — the
/// negative cases wait this out before asserting nothing was asked.
const SETTLE: Duration = Duration::from_millis(300);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_yield_beside_a_seat_asks_the_agent_for_its_pass_once() {
    let tmp = tempfile::tempdir().unwrap();
    let app = AccountStoreRuntime::start(params(tmp.path()))
        .await
        .expect("the app's runtime");
    app.reconcile_now().await.expect("the app's first pass");
    assert!(app.is_engine_holder(), "alone, the app pumps");

    let (nudge, asked, asked_signal) = counting_nudge();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    let watch = fauna_client_account_runtime::holder_yield::spawn_holder_yield_nudge(
        app.subscribe_engine_role(),
        slot,
    );

    let presence = agent_present(tmp.path());
    let first_ask = asked_signal.notified();
    app.reconcile_now().await.expect("the yielding command");
    assert!(!app.is_engine_holder(), "the app yielded");
    if asked.load(Ordering::SeqCst) == 0 {
        tokio::time::timeout(Duration::from_secs(5), first_ask)
            .await
            .expect("the yield's edge asks the agent for its pass");
    }
    assert_eq!(asked.load(Ordering::SeqCst), 1, "asked once on the edge");

    // Present, the app stays a non-holder: no further edge, no further ask.
    app.reconcile_now().await.expect("the role answer");
    tokio::time::sleep(SETTLE).await;
    assert_eq!(asked.load(Ordering::SeqCst), 1, "one edge, one ask");

    drop(presence);
    app.shutdown().await;
    drop(app);
    tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .expect("the watch ends with the runtime")
        .expect("the watch task did not panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_yield_with_no_seat_asks_no_one() {
    let tmp = tempfile::tempdir().unwrap();
    let app = AccountStoreRuntime::start(params(tmp.path()))
        .await
        .expect("the app's runtime");
    app.reconcile_now().await.expect("the app's first pass");
    assert!(app.is_engine_holder());

    // Published, then gone (a host whose agent is not provisioned, or a
    // signed-out one): the slot reads absent.
    let (nudge, asked, _) = counting_nudge();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    drop(nudge);
    let _watch = fauna_client_account_runtime::holder_yield::spawn_holder_yield_nudge(
        app.subscribe_engine_role(),
        slot,
    );

    let presence = agent_present(tmp.path());
    app.reconcile_now().await.expect("the yielding command");
    assert!(!app.is_engine_holder(), "the app yielded");
    tokio::time::sleep(SETTLE).await;
    assert_eq!(asked.load(Ordering::SeqCst), 0, "nobody to ask");

    drop(presence);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn taking_the_role_asks_no_one() {
    let tmp = tempfile::tempdir().unwrap();
    // The agent is present at the app's start: the app never contends.
    let presence = agent_present(tmp.path());
    let app = AccountStoreRuntime::start(params(tmp.path()))
        .await
        .expect("the app's runtime beside its agent");
    assert!(!app.is_engine_holder(), "the agent is present");

    let (nudge, asked, _) = counting_nudge();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    let _watch = fauna_client_account_runtime::holder_yield::spawn_holder_yield_nudge(
        app.subscribe_engine_role(),
        slot,
    );

    // The agent exits: the app's next re-read finds it gone and takes the role.
    drop(presence);
    app.reconcile_now().await.expect("the taking command");
    assert!(app.is_engine_holder(), "the app took the freed role");
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "a non-holder→holder edge asks nothing"
    );

    drop(nudge);
    app.shutdown().await;
}

// ── The seed pass's ask (`account-runtime.md` § Multi-instance concurrency →
// *The seed-leg role*, part 4, *Escrow recovery*) ─────────────────────────
//
// A generation key the app's seed pass recovers beside its agent opens rows
// only at the agent's next full reconcile, so the host asks the agent for that
// pass on every move of the runtime's owed-pass watch. The runtime moving the
// watch is pinned in `fauna_sync_engine::account_runtime`
// (`a_machine_signed_out_and_in_beside_its_agent_keys_the_generation_without_a_command`);
// these drive the watch every desktop host runs over the watch alone.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_owed_pass_beside_a_seat_asks_the_agent_once() {
    let (tx, rx) = tokio::sync::watch::channel(0u64);
    let (nudge, asked, asked_signal) = counting_nudge();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    let watch = fauna_client_account_runtime::holder_yield::spawn_holder_pass_owed_nudge(rx, slot);

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the current count asks nothing"
    );

    for n in 1..=2 {
        let ask = asked_signal.notified();
        tx.send_modify(|owed| *owed += 1);
        if asked.load(Ordering::SeqCst) < n {
            tokio::time::timeout(Duration::from_secs(5), ask)
                .await
                .expect("an owed pass asks the agent for it");
        }
        assert_eq!(asked.load(Ordering::SeqCst), n, "one ask per owed pass");
    }

    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .expect("the watch ends with the runtime")
        .expect("the watch task did not panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owed_pass_with_no_seat_asks_no_one() {
    let (tx, rx) = tokio::sync::watch::channel(0u64);
    let (nudge, asked, _) = counting_nudge();
    let slot = EngineHolderNudgeSlot::default();
    slot.publish(&nudge);
    drop(nudge);
    let _watch = fauna_client_account_runtime::holder_yield::spawn_holder_pass_owed_nudge(rx, slot);

    tx.send_modify(|owed| *owed += 1);
    tokio::time::sleep(SETTLE).await;
    assert_eq!(asked.load(Ordering::SeqCst), 0, "nobody to ask");
}

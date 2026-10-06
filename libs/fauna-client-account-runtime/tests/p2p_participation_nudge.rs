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

//! The same-account peer leg against the per-device participation control
//! (`docs/goal/behavior/p2p.md` § Per-device participation): the listener
//! comes up on a device whose switch is on, goes away — accept side and all —
//! the pass after the switch goes off, stays away across a restart because the
//! row rests on the store, and comes back the pass after the switch goes on.
//! Driven entirely through the runtime's pump (`reconcile_now`) over the
//! in-memory transport, the harness `peer_dial_convergence.rs` uses; "no
//! listener" is asserted on the transport's accept side, never on a flag.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_protocol::discovery::NestInfoReply;
use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE, PeerLegPass,
    RuntimePrincipal, StoreRoot, resolve_writer_key_serialized,
};
use fauna_transport::testing::{Listeners, await_listening, listeners};

mod common;
use common::mem_factory;

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([0x1fu8; 32])
}

/// A transport fault the pump absorbs — nothing reached a nest, because
/// there is none.
#[derive(Debug)]
struct NoNest(&'static str);

impl std::fmt::Display for NoNest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no nest in this test: {}", self.0)
    }
}

impl RpcErrorClass for NoNest {
    fn is_rejection(&self) -> bool {
        false
    }
}

/// Answers `fauna.nest.info` (with the always-on `peer-sync` advertisement)
/// and fails everything else — the brake gate's evidence is metadata, and
/// every leg that moves data is dead. The roster read the participation fold
/// makes fails too: an unreachable nest changes nothing about the local
/// verdict, which is exactly the property under test.
#[derive(Clone)]
struct NestInfoOnly;

impl RpcRequester for NestInfoOnly {
    type Error = NoNest;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if kind == "fauna.nest.info" {
            let reply = NestInfoReply {
                capabilities: vec!["peer-sync".to_string()],
                ..Default::default()
            };
            return Ok(decode_strict(&encode_canonical(&reply).expect("encode"))
                .expect("node-info reply decodes"));
        }
        Err(NoNest(kind))
    }
}

impl fauna_protocol::KeyedRpcRequester for NestInfoOnly {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

fn creds_for(base: &Path, device: &str) -> CredentialStore {
    CredentialStore::with_file_backend(CRED_NAMESPACE, base.join(device).join("creds"))
}

fn store_root_for(base: &Path, device: &str) -> StoreRoot {
    StoreRoot::at(base.join(device).join("state"))
}

fn params(base: &Path, device: &str, net: &Listeners) -> AccountRuntimeParams<NestInfoOnly> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root_for(base, device),
        actor_id_hex: root().actor_id_hex(),
        rpc: NestInfoOnly,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(root().into()),
        credentials: creds_for(base, device),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: Some(mem_factory(net)),
        // The machine's named row — never read here (no nest answers enrollment).
        enrollment_target_device_id: "ab".repeat(32),
    }
}

/// Pre-mint the device's writer key (its NodeId on the in-memory network).
fn premint(base: &Path, device: &str) -> [u8; 32] {
    let writer_key = resolve_writer_key_serialized(
        &store_root_for(base, device),
        &root().actor_id_hex(),
        &creds_for(base, device),
    )
    .expect("pre-mint writer key");
    writer_key.verifying_key().to_bytes()
}

/// Whether `node` has a LIVE accept side on the network: registered, and its
/// sender not closed. `PeerNode`'s drop ends the accept task, which drops the
/// stream the sender feeds — so a dropped listener reads closed here even
/// though the double never removes the map entry.
fn accepting(net: &Listeners, node: &[u8; 32]) -> bool {
    net.lock()
        .unwrap()
        .get(node)
        .is_some_and(|tx| !tx.is_closed())
}

/// Wait (bounded) for `node`'s accept side to close. The node's drop ends the
/// accept task, but that task is torn down asynchronously on the runtime, so
/// the sender closes a scheduling beat after the pass that dropped it returns.
async fn await_not_listening(net: &Listeners, node: &[u8; 32]) {
    for _ in 0..200 {
        if !accepting(net, node) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn peer_leg_of(handle: &AccountStoreHandle) -> Option<PeerLegPass> {
    handle.reconcile_now().await.expect("a pass runs").peer_leg
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_participation_off_takes_the_listener_down_and_on_brings_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();
    let node = premint(&base, "a");

    let a = AccountStoreRuntime::start(params(&base, "a", &net))
        .await
        .expect("start");
    // On by default: the prologue binds.
    await_listening(&net, &node).await;
    assert!(
        accepting(&net, &node),
        "the default is on: the accept side is live"
    );
    assert!(
        a.p2p_participation().await.unwrap().effective(),
        "an absent row reads on"
    );

    // Off: the next pass answers ParticipationOff and the accept side is gone.
    a.set_p2p_participation(false)
        .await
        .expect("the switch rests");
    assert_eq!(peer_leg_of(&a).await, Some(PeerLegPass::ParticipationOff));
    await_not_listening(&net, &node).await;
    assert!(
        !accepting(&net, &node),
        "off ⇒ no listener: the accept side ended with the dropped node"
    );
    // And stays off across passes.
    assert_eq!(peer_leg_of(&a).await, Some(PeerLegPass::ParticipationOff));
    assert!(!accepting(&net, &node));

    // On again: the next pass binds afresh.
    a.set_p2p_participation(true)
        .await
        .expect("the switch rests");
    assert_eq!(peer_leg_of(&a).await, Some(PeerLegPass::Bound));
    await_listening(&net, &node).await;
    assert!(accepting(&net, &node), "on ⇒ the listener is back");

    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_switched_off_device_never_binds_after_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();
    let node = premint(&base, "a");

    let a = AccountStoreRuntime::start(params(&base, "a", &net))
        .await
        .expect("start");
    await_listening(&net, &node).await;
    a.set_p2p_participation(false).await.unwrap();
    assert_eq!(peer_leg_of(&a).await, Some(PeerLegPass::ParticipationOff));
    a.shutdown().await;

    // The row rests on the store: a fresh runtime's prologue reads it and
    // never brings the listener up — no registration ever appears.
    let net = listeners();
    let a = AccountStoreRuntime::start(params(&base, "a", &net))
        .await
        .expect("restart");
    assert_eq!(peer_leg_of(&a).await, Some(PeerLegPass::ParticipationOff));
    assert!(
        net.lock().unwrap().get(&node).is_none(),
        "a switched-off device registers no listener at all after a restart"
    );
    assert!(!a.p2p_participation().await.unwrap().effective());
    a.shutdown().await;
}

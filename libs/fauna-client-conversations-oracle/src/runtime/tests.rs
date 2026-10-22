//! tier_1: the runtime account over fakes of its four seams — the roster, the
//! owner's grant log, the account store's meta rows, the nest's record kind —
//! and two apps' runtimes taking the door in turn over one socket
//! (`conversations-oracle.md` § Done definition, items 2–3).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fauna_client_config::test_helpers::FakeSuccessionLedgerStore;
use fauna_conversations::ConversationsManager;
use fauna_conversations::address::Rail;
use fauna_conversations::backends::mock::MockRailBackend;
use fauna_core::grant_event::{GrantEvent, GrantEventKind, GrantEventScope};
use fauna_core::identity::ActorId;
use fauna_core::identity_op::IdentityOpClass;
use fauna_ipc::oracle::{OracleClient, OracleEndpoint, OracleOp, OracleRefusal, OracleReply};
use fauna_ipc::unix_transport::InstanceLock;
use fauna_protocol::ByteBuf;

use super::*;
use crate::{AdmittedPrincipal, ConversationsCustodian, open_challenge, serve};

const ACTOR: [u8; 32] = [0x42; 32];
const PRINCIPAL: &[u8] = b"principal-1";
const GRANT: [u8; 16] = [7; 16];

#[derive(Default)]
struct FakeMeta {
    rows: Mutex<HashMap<String, Vec<u8>>>,
    broken: AtomicBool,
}

impl MetaRows for FakeMeta {
    fn get<'a>(&'a self, key: &'a str) -> DoorFuture<'a, Result<Option<Vec<u8>>, String>> {
        let row = if self.broken.load(Ordering::SeqCst) {
            Err("store gone".to_string())
        } else {
            Ok(self.rows.lock().unwrap().get(key).cloned())
        };
        Box::pin(async move { row })
    }
    fn put<'a>(&'a self, key: &'a str, bytes: Vec<u8>) -> DoorFuture<'a, Result<(), String>> {
        let done = if self.broken.load(Ordering::SeqCst) {
            Err("store gone".to_string())
        } else {
            self.rows.lock().unwrap().insert(key.to_string(), bytes);
            Ok(())
        };
        Box::pin(async move { done })
    }
}

#[derive(Default)]
struct FakeNest {
    roster: Mutex<Vec<PrincipalInfo>>,
    offline: AtomicBool,
    requests: Mutex<Vec<RecordPrincipalOpsRequest>>,
}

impl FakeNest {
    fn with_principal(holder: [u8; 32]) -> Self {
        let nest = Self::default();
        nest.roster.lock().unwrap().push(PrincipalInfo {
            principal_id: PRINCIPAL.to_vec(),
            holder_x25519: Some(ByteBuf::from(holder.to_vec())),
            execution_form: "device".into(),
            ..Default::default()
        });
        nest
    }

    /// Every op id the nest kept, deduplicated as the nest does.
    fn kept(&self) -> Vec<Vec<u8>> {
        let mut ids: Vec<Vec<u8>> = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .flat_map(|r| r.ops.iter().map(|op| op.op_id.clone()))
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

impl NestPrincipals for FakeNest {
    fn list_principals(&self) -> DoorFuture<'_, Result<Vec<PrincipalInfo>, String>> {
        let roster = if self.offline.load(Ordering::SeqCst) {
            Err("offline".to_string())
        } else {
            Ok(self.roster.lock().unwrap().clone())
        };
        Box::pin(async move { roster })
    }
    fn record_ops(
        &self,
        request: RecordPrincipalOpsRequest,
    ) -> DoorFuture<'_, Result<u32, String>> {
        let kept = if self.offline.load(Ordering::SeqCst) {
            Err("offline".to_string())
        } else {
            assert!(request.ops.len() <= PRINCIPAL_OPS_RECORD_BATCH);
            let n = request.ops.len() as u32;
            self.requests.lock().unwrap().push(request);
            Ok(n)
        };
        Box::pin(async move { kept })
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn event(kind: GrantEventKind, holder: [u8; 32], classes: &[IdentityOpClass]) -> GrantEvent {
    let live = kind != GrantEventKind::Revoke;
    GrantEvent {
        grant_id: GRANT.to_vec(),
        holder: holder.to_vec(),
        kind,
        scope: classes
            .iter()
            .map(|c| GrantEventScope {
                class: fauna_core::identity_op::CLASS.to_string(),
                kind: Some(c.name().to_string()),
                tier: None,
            })
            .collect(),
        window_start: if live { now() - 60 } else { 0 },
        window_end: if live { now() + 3600 } else { 0 },
        at: now(),
        sig: vec![],
    }
}

struct World {
    nest: Arc<FakeNest>,
    ledger: Arc<FakeSuccessionLedgerStore>,
    meta: Arc<FakeMeta>,
}

impl World {
    fn granted(holder: [u8; 32], classes: &[IdentityOpClass]) -> Self {
        let ledger = FakeSuccessionLedgerStore::empty(ActorId(ACTOR));
        ledger.mutate(|l| {
            l.grant_events
                .push(event(GrantEventKind::Mint, holder, classes));
        });
        Self {
            nest: Arc::new(FakeNest::with_principal(holder)),
            ledger: Arc::new(ledger),
            meta: Arc::new(FakeMeta::default()),
        }
    }

    /// One app's account over the shared stores — what each of two apps on
    /// one device builds over the one account store they both mount.
    fn account(&self) -> RuntimeAccount {
        RuntimeAccount::new(
            ACTOR,
            self.nest.clone(),
            self.ledger.clone(),
            self.meta.clone(),
            None,
        )
    }

    fn revoke(&self, holder: [u8; 32]) {
        self.ledger.mutate(|l| {
            l.grant_events
                .push(event(GrantEventKind::Revoke, holder, &[]));
        });
    }
}

fn principal(holder: [u8; 32]) -> AdmittedPrincipal {
    AdmittedPrincipal {
        principal_id: PRINCIPAL.to_vec(),
        holder,
    }
}

fn custodian(account: RuntimeAccount) -> ConversationsCustodian<RuntimeAccount> {
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    ConversationsCustodian::new(account, manager)
}

#[tokio::test]
async fn the_principal_is_the_rosters_row_and_an_unreadable_roster_refuses() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    let account = world.account();
    assert_eq!(account.principal_holder(PRINCIPAL).await, Some(holder));
    assert_eq!(account.principal_holder(b"another account's").await, None);
    world.nest.offline.store(true, Ordering::SeqCst);
    assert_eq!(
        account.principal_holder(PRINCIPAL).await,
        None,
        "no cached row stands in for a roster the custodian cannot read"
    );
}

#[tokio::test]
async fn live_tuples_read_the_owners_log_per_call() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (_, other) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(
        holder,
        &[
            IdentityOpClass::ConversationRead,
            IdentityOpClass::ConversationSend,
        ],
    );
    let account = world.account();
    let tuples = account.live_tuples(&holder, now()).await;
    assert_eq!(tuples.len(), 2);
    assert!(account.live_tuples(&other, now()).await.is_empty());
    assert!(
        account.live_tuples(&holder, now() + 7200).await.is_empty(),
        "a closed window grants nothing"
    );
    world.revoke(holder);
    assert!(account.live_tuples(&holder, now()).await.is_empty());
}

#[tokio::test]
async fn the_ceiling_binds_across_the_apps_sharing_the_store_and_fails_closed() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    let first = custodian(world.account());
    for _ in 0..OPS_PER_WINDOW {
        first
            .authorize(&principal(holder), IdentityOpClass::ConversationRead)
            .await
            .unwrap();
    }
    // A second app over the same account store meets the same window.
    let second = custodian(world.account());
    assert!(matches!(
        second
            .authorize(&principal(holder), IdentityOpClass::ConversationRead)
            .await,
        Err(OracleRefusal::CeilingReached { .. })
    ));

    let fresh = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    fresh.meta.broken.store(true, Ordering::SeqCst);
    assert!(
        matches!(
            custodian(fresh.account())
                .authorize(&principal(holder), IdentityOpClass::ConversationRead)
                .await,
            Err(OracleRefusal::CeilingReached { .. })
        ),
        "an unreadable ceiling row refuses"
    );
}

#[tokio::test]
async fn the_record_waits_in_the_outbox_and_reaches_the_nest_in_batches_under_its_op_ids() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    let account = world.account();
    world.nest.offline.store(true, Ordering::SeqCst);
    let rows = PRINCIPAL_OPS_RECORD_BATCH + 6;
    for i in 0..rows {
        account
            .record(
                PRINCIPAL,
                RecordedPrincipalOp {
                    op_id: vec![i as u8; 16],
                    at: now() as i64,
                    class: "conversation.read".into(),
                    detail: "room".into(),
                    extra: Default::default(),
                },
            )
            .await;
    }
    assert_eq!(account.drain_records().await, 0, "offline: nothing leaves");
    assert_eq!(account.pending_records().await[0].ops.len(), rows);

    world.nest.offline.store(false, Ordering::SeqCst);
    // A restart in between: a new account over the same store finds the rows.
    let restarted = world.account();
    assert_eq!(restarted.drain_records().await, rows);
    assert!(restarted.pending_records().await.is_empty());
    assert_eq!(world.nest.kept().len(), rows);
    assert_eq!(world.nest.requests.lock().unwrap().len(), 2, "two batches");
}

/// The ceiling's window lives on the device, so every batch carries it as the
/// batch leaves — the position the ceiling holds then, not when the row was
/// kept — and a ceiling row that cannot be read reports none.
#[tokio::test]
async fn every_batch_carries_the_ceilings_window_as_it_leaves() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    let account = world.account();
    let op = |id: u8| RecordedPrincipalOp {
        op_id: vec![id; 16],
        at: now() as i64,
        class: "conversation.read".into(),
        detail: "room".into(),
        extra: Default::default(),
    };
    let window = |count| RateWindow {
        window_start: now() - 10,
        count,
    };
    account.store_rate_window(PRINCIPAL, window(1)).await;
    account.record(PRINCIPAL, op(1)).await;
    account.store_rate_window(PRINCIPAL, window(2)).await;
    account.record(PRINCIPAL, op(2)).await;
    account.drain_records().await;
    let sent = world.nest.requests.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let carried = sent[0].window.clone().expect("the batch's window");
    assert_eq!(
        (carried.window_start, carried.count),
        (window(2).window_start, 2)
    );

    // A row too short to decode, and one that decodes to no window a ceiling
    // could hold — which the nest would refuse with the whole batch.
    for (id, row) in [(3u8, b"bad".to_vec()), (4, b"not a window".to_vec())] {
        world
            .meta
            .rows
            .lock()
            .unwrap()
            .insert(RuntimeAccount::rate_key(PRINCIPAL), row);
        account.record(PRINCIPAL, op(id)).await;
        account.drain_records().await;
        let last = world.nest.requests.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.ops[0].op_id, vec![id; 16]);
        assert_eq!(last.window, None, "an unusable ceiling row reports nothing");
    }
}

#[tokio::test]
async fn a_stale_or_overflowing_outbox_lets_its_oldest_rows_go() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    let account = world.account();
    world.nest.offline.store(true, Ordering::SeqCst);
    let op = |id: u8, at: i64| RecordedPrincipalOp {
        op_id: vec![id; 16],
        at,
        class: "conversation.read".into(),
        detail: "room".into(),
        extra: Default::default(),
    };
    account
        .record(PRINCIPAL, op(1, now() as i64 - OUTBOX_MAX_AGE_SECS - 10))
        .await;
    account.record(PRINCIPAL, op(2, now() as i64)).await;
    account.drain_records().await;
    let left: Vec<_> = account.pending_records().await[0]
        .ops
        .iter()
        .map(|o| o.op_id[0])
        .collect();
    assert_eq!(left, vec![2], "the week-old row is let go");
}

fn held(path: &std::path::Path) -> bool {
    matches!(
        InstanceLock::acquire(path),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse
    )
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Two signed-in apps over one account: exactly one serves; the other takes
/// the door over within a tick of the first signing out; an operation's record
/// reaches the nest; a revoke from another app refuses the next operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_apps_take_the_door_in_turn_and_a_revoke_bites_at_the_next_operation() {
    let (secret, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let world = World::granted(holder, &[IdentityOpClass::ConversationRead]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oracle-runtime.sock");
    let tick = Duration::from_millis(50);

    let (first_up, first_rx) = tokio::sync::watch::channel(true);
    let first = Arc::new(custodian(world.account()));
    let first_task = tokio::spawn(serve::keep_door_every(
        serve::DoorKeeper::at(path.clone(), first.clone()),
        first_rx,
        tick,
    ));
    until("the first app serves", || path.exists()).await;
    let (second_up, second_rx) = tokio::sync::watch::channel(true);
    let second = Arc::new(custodian(world.account()));
    let second_task = tokio::spawn(serve::keep_door_every(
        serve::DoorKeeper::at(path.clone(), second.clone()),
        second_rx,
        tick,
    ));
    tokio::time::sleep(tick * 3).await;
    assert!(held(&path), "exactly one serves: the endpoint is held");

    let endpoint = OracleEndpoint::Unix(path.clone());
    let list = {
        let endpoint = endpoint.clone();
        move || {
            let mut client = OracleClient::connect(&endpoint, PRINCIPAL, &ACTOR, |enc, ct| {
                open_challenge(&secret, &ACTOR, PRINCIPAL, enc, ct)
            })
            .unwrap();
            client.call(OracleOp::ListRooms).unwrap()
        }
    };
    let listed = tokio::task::spawn_blocking(list.clone()).await.unwrap();
    assert_eq!(listed, Ok(OracleReply::Rooms(vec![])));
    until("the record reaches the nest", || {
        world.nest.kept().len() == 1
    })
    .await;

    // The first app signs out: its sender drops, and the second takes over.
    drop(first_up);
    first_task.await.unwrap();
    until("the second app takes the door over", || {
        path.exists() && held(&path)
    })
    .await;
    let listed = tokio::task::spawn_blocking(list.clone()).await.unwrap();
    assert_eq!(
        listed,
        Ok(OracleReply::Rooms(vec![])),
        "served by the second"
    );

    // A revoke from another app: the next operation is refused.
    world.revoke(holder);
    let refused = tokio::task::spawn_blocking(list).await.unwrap();
    assert_eq!(
        refused,
        Err(OracleRefusal::NotGranted {
            class: "conversation.read".into()
        })
    );
    until("both admitted operations recorded", || {
        world.nest.kept().len() == 2
    })
    .await;

    drop(second_up);
    second_task.await.unwrap();
    assert!(!held(&path), "the door is free once no app may serve");
}

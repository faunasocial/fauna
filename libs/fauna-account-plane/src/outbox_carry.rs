//! The outbox carry — a retired identity's undrained outbox moved into its
//! successor's (`succession-aftermath.md` § Re-key scope → *Adjudicating what
//! the aftermath carries across*, the device-local outbox row, ruled
//! 2026-11-09).
//!
//! **Why.** An undrained intent is the only copy of the user's pending write
//! (`account-offline-mutation.md` § The offline-mutation contract,
//! discriminator 4). After an identity succession the successor is a new
//! account with its own store, and the retired identity's connection never
//! mints again, so an intent queued before the ceremony — or on a device still
//! offline when it ran — would drain from nowhere.
//!
//! **Which device carries.** The read is the departed-fleet read's
//! ([`crate::departed_fleet`]): each predecessor this device still owes
//! ([`AccountRegistry::outbox_carry_sources`] — the attested walk, with the
//! seed) has its store opened at rest under `StoreRoot::store_dir`, with the
//! replica seal key its seed derives. Only a device holding the seed opens
//! it, and only an app holds one: the seedless sync agent never runs this. A
//! thief's device holding the predecessor seed never runs a successor
//! runtime, so nothing of its outbox is ever carried.
//!
//! **Carry = move, verbatim, append-then-delete.** Each row, in
//! `(scope, channel_seq)` order, is appended to the successor's outbox with
//! its intent id, kind, scope, payload and drainer kept
//! ([`CarryTarget::carry_intent`]); a parked row is carried parked with its
//! reason. Only then is the predecessor row deleted. A crash in between leaves
//! the row in both stores; the next pass's append answers `false` (the id is
//! there) and the delete runs: the intent id collapses the duplicate.
//!
//! **The MLS hold.** An MLS intent moves only once the successor's engine
//! holds its channel's group ([`MlsIntentSender::holds_group_of`]); until
//! then it — and every later row of its scope, so the scope's order holds —
//! waits in the predecessor's store, listed as held
//! ([`OutboxCarry::held_changes`], under
//! [`hold_code::CARRY_AWAITING_GROUP`]).
//!
//! **When it runs.** [`crate::account_driver::offline_outbox::AccountOutbox`]
//! runs a pass at the start of every app drain loop — at the lend, the
//! successor's store-ready edge, and on every reconnect — and before each
//! pass's drain, so a group joined since is carried and drained in the same
//! pass. Once a predecessor's store reads back with no undrained intent the
//! registry records the carry ([`AccountRegistry::record_outbox_carried`]) and
//! no later pass opens that store.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use fauna_account_store::root::StoreRoot;
use fauna_account_store::types::{IntentDrainer, IntentStatus, NewOutboxIntent, OutboxIntent};
use fauna_client_accounts::AccountRegistry;
use fauna_core::crypto::ReplicaSealKey;
use fauna_core::offline_intent::{MlsIntentSender, UnsentChange, UnsentState, hold_code};

use crate::account_driver::AccountStoreHandle;
use crate::deployment_seed_recovery::replica_seal_key;

/// Where a carried intent lands — the successor's outbox.
/// [`AccountStoreHandle`] is the production one; a test lands it in a plain
/// store.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait CarryTarget: fauna_core::MaybeSendSync {
    /// [`crate::outbox::carry_intent`] into this outbox.
    async fn carry_intent(&self, intent: NewOutboxIntent, parked: Option<String>) -> Result<bool>;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl CarryTarget for AccountStoreHandle {
    async fn carry_intent(&self, intent: NewOutboxIntent, parked: Option<String>) -> Result<bool> {
        AccountStoreHandle::carry_intent(self, intent, parked).await
    }
}

/// What one carry pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CarryReport {
    /// Rows moved into the successor's outbox this pass.
    pub carried: usize,
    /// Rows left in a predecessor's store: an MLS row whose group the
    /// successor's engine does not hold yet, and every later row of its scope.
    pub waiting: usize,
    /// Predecessors whose outbox read back empty and were recorded carried.
    pub settled: usize,
    /// Why, for every predecessor the pass could not finish.
    pub errors: Vec<String>,
}

/// The carry's seat on one app: the registry that names what is owed and
/// holds the seeds, and the store root the predecessors' stores rest under.
pub struct OutboxCarry {
    registry: AccountRegistry,
    root: StoreRoot,
}

impl OutboxCarry {
    pub fn new(registry: AccountRegistry, root: StoreRoot) -> Self {
        Self { registry, root }
    }

    /// One carry pass for `successor_hex`, landing in `target`. `mls` is the
    /// app's conversations engine when one is up — it answers the MLS hold;
    /// `None` carries no MLS row this pass. Best-effort: a predecessor whose
    /// store does not open is logged and left for the next pass.
    pub async fn run(
        &self,
        successor_hex: &str,
        target: &dyn CarryTarget,
        mls: Option<&dyn MlsIntentSender>,
    ) -> CarryReport {
        let mut report = CarryReport::default();
        for (predecessor, seed) in self.registry.outbox_carry_sources(successor_hex) {
            let seal = replica_seal_key(&seed);
            match carry_one(&self.root, &predecessor, seal, target, mls).await {
                Ok(one) => {
                    report.carried += one.carried;
                    report.waiting += one.waiting;
                    if one.empty {
                        match self.registry.record_outbox_carried(&predecessor) {
                            Ok(()) => report.settled += 1,
                            Err(e) => report
                                .errors
                                .push(format!("record the carry of {predecessor}: {e}")),
                        }
                    }
                }
                Err(e) => report.errors.push(format!("carry {predecessor}: {e:#}")),
            }
        }
        if report != CarryReport::default() {
            tracing::info!(
                carried = report.carried,
                waiting = report.waiting,
                settled = report.settled,
                errors = ?report.errors,
                "outbox carry"
            );
        }
        report
    }

    /// The predecessors' rows not carried yet, as unsent changes — what the
    /// successor's unsent-changes list shows beside its own from the first
    /// sign-in (`succession-aftermath.md`, the device-local outbox row, point
    /// 5). A pending row is [`UnsentState::Held`] with
    /// [`hold_code::CARRY_AWAITING_GROUP`] as its hold; a parked one keeps its
    /// own not-sent state and reason.
    pub async fn held_changes(&self, successor_hex: &str) -> Vec<UnsentChange> {
        let mut out = Vec::new();
        for (predecessor, seed) in self.registry.outbox_carry_sources(successor_hex) {
            match read_rows(&self.root, &predecessor, replica_seal_key(&seed)).await {
                Ok(Some(rows)) => out.extend(held_changes_of(&rows)),
                Ok(None) => {}
                Err(e) => tracing::debug!("outbox carry: {predecessor}'s store unread: {e:#}"),
            }
        }
        out
    }

    /// Whether `intent_id` is a predecessor row not carried yet.
    pub async fn holds(&self, successor_hex: &str, intent_id: [u8; 16]) -> bool {
        self.held_changes(successor_hex)
            .await
            .iter()
            .any(|c| c.intent_id_bytes() == Some(intent_id))
    }

    /// The user's discard of a predecessor row not carried yet — deleted from
    /// the predecessor's store, whatever its status (the list's confirm is the
    /// user's word). `false` = no such row.
    pub async fn discard(&self, successor_hex: &str, intent_id: [u8; 16]) -> Result<bool> {
        for (predecessor, seed) in self.registry.outbox_carry_sources(successor_hex) {
            let seal = replica_seal_key(&seed);
            let Some(rows) = read_rows(&self.root, &predecessor, seal.clone()).await? else {
                continue;
            };
            if let Some(row) = rows.iter().find(|r| r.intent_id == intent_id) {
                delete_rows(
                    &self.root,
                    &predecessor,
                    seal,
                    &[(row.intent_id, row.status)],
                )
                .await?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The unsent-changes rows of a predecessor's undrained `rows`, as the carry
/// lists them: every pending row held by the carry, every parked row as the
/// drain left it.
pub fn held_changes_of(rows: &[OutboxIntent]) -> Vec<UnsentChange> {
    crate::account_driver::unsent_changes_of(rows)
        .into_iter()
        .map(|mut change| {
            if !matches!(change.state, UnsentState::Unsent { .. }) {
                change.state = UnsentState::Held;
                change.hold = Some(hold_code::CARRY_AWAITING_GROUP.to_string());
            }
            change
        })
        .collect()
}

/// What carrying one predecessor did.
struct OneCarry {
    carried: usize,
    waiting: usize,
    /// The store read back with nothing left (or there was none).
    empty: bool,
}

async fn carry_one(
    root: &StoreRoot,
    predecessor: &str,
    seal: ReplicaSealKey,
    target: &dyn CarryTarget,
    mls: Option<&dyn MlsIntentSender>,
) -> Result<OneCarry> {
    let Some(rows) = read_rows(root, predecessor, seal.clone()).await? else {
        tracing::debug!("outbox carry: no store for {predecessor} on this device");
        return Ok(OneCarry {
            carried: 0,
            waiting: 0,
            empty: true,
        });
    };
    let mut waiting_scopes: BTreeSet<&str> = BTreeSet::new();
    let mut waiting = 0usize;
    let mut moved: Vec<([u8; 16], IntentStatus)> = Vec::new();
    for row in &rows {
        let group_waits = row.drainer == IntentDrainer::Mls
            && row.status == IntentStatus::Pending
            && !mls.is_some_and(|m| m.holds_group_of(&row.payload));
        if waiting_scopes.contains(row.scope.as_str()) || group_waits {
            waiting_scopes.insert(&row.scope);
            waiting += 1;
            continue;
        }
        let parked = (row.status == IntentStatus::Failed)
            .then(|| row.failure_reason.clone().unwrap_or_default());
        target
            .carry_intent(
                NewOutboxIntent {
                    intent_id: row.intent_id,
                    kind: row.kind.clone(),
                    scope: row.scope.clone(),
                    payload: row.payload.clone(),
                    drainer: row.drainer,
                },
                parked,
            )
            .await
            .context("append into the successor's outbox")?;
        moved.push((row.intent_id, row.status));
    }
    if !moved.is_empty() {
        delete_rows(root, predecessor, seal.clone(), &moved).await?;
    }
    // Read back, not inferred: only a store that holds nothing more settles.
    let left = read_rows(root, predecessor, seal)
        .await?
        .map_or(0, |rows| rows.len());
    Ok(OneCarry {
        carried: moved.len(),
        waiting,
        empty: left == 0,
    })
}

/// The predecessor's undrained intents, `(scope, channel_seq)` ascending;
/// `None` when no store rests there.
async fn read_rows(
    root: &StoreRoot,
    predecessor: &str,
    seal: ReplicaSealKey,
) -> Result<Option<Vec<OutboxIntent>>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::read_rows(root, predecessor, seal)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = seal;
        use fauna_account_store::backend::StoreBackend as _;
        let name = root.store_name(predecessor)?;
        match fauna_account_store::indexeddb::IndexedDbBackend::open_existing(&name).await? {
            Some(backend) => Ok(Some(backend.outbox_undrained().await?)),
            None => Ok(None),
        }
    }
}

/// Delete `rows` from the predecessor's store: a pending row through the
/// drain's completion door, a parked one through the discard door — between
/// them every status, and never a row not named.
async fn delete_rows(
    root: &StoreRoot,
    predecessor: &str,
    seal: ReplicaSealKey,
    rows: &[([u8; 16], IntentStatus)],
) -> Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::delete_rows(root, predecessor, seal, rows)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = seal;
        use fauna_account_store::backend::StoreBackend as _;
        let name = root.store_name(predecessor)?;
        let Some(backend) =
            fauna_account_store::indexeddb::IndexedDbBackend::open_existing(&name).await?
        else {
            return Ok(());
        };
        for (id, status) in rows {
            match status {
                IntentStatus::Failed => backend.outbox_discard(id).await?,
                _ => backend.outbox_ack(id).await?,
            };
        }
        Ok(())
    }
}

/// Native: SQLite answers synchronously under its async signature and its
/// connection is not `Sync`, so each read or delete opens, runs to completion
/// and drops in a sync helper — no future holds a connection across an
/// await (the departed-fleet read's discipline).
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::future::Future;
    use std::task::{Context as TaskContext, Poll, Waker};

    use anyhow::{Context, Result};
    use fauna_account_store::backend::StoreBackend as _;
    use fauna_account_store::root::StoreRoot;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{IntentStatus, OutboxIntent};
    use fauna_core::crypto::ReplicaSealKey;

    fn now<F: Future>(f: F) -> Result<F::Output> {
        let f = std::pin::pin!(f);
        match f.poll(&mut TaskContext::from_waker(Waker::noop())) {
            Poll::Ready(out) => Ok(out),
            Poll::Pending => anyhow::bail!("the account store did not answer synchronously"),
        }
    }

    fn open(
        root: &StoreRoot,
        predecessor: &str,
        seal: ReplicaSealKey,
    ) -> Result<Option<SqliteBackend>> {
        let dir = root.store_dir(predecessor)?;
        SqliteBackend::open_existing_keyed(&dir, Some(seal))
            .with_context(|| format!("open the account store at {}", dir.display()))
    }

    pub(super) fn read_rows(
        root: &StoreRoot,
        predecessor: &str,
        seal: ReplicaSealKey,
    ) -> Result<Option<Vec<OutboxIntent>>> {
        let Some(backend) = open(root, predecessor, seal)? else {
            return Ok(None);
        };
        Ok(Some(now(backend.outbox_undrained())??))
    }

    pub(super) fn delete_rows(
        root: &StoreRoot,
        predecessor: &str,
        seal: ReplicaSealKey,
        rows: &[([u8; 16], IntentStatus)],
    ) -> Result<()> {
        let Some(backend) = open(root, predecessor, seal)? else {
            return Ok(());
        };
        for (id, status) in rows {
            match status {
                IntentStatus::Failed => now(backend.outbox_discard(id))??,
                _ => now(backend.outbox_ack(id))??,
            };
        }
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use fauna_account_store::memory::MemoryBackend;
    use fauna_account_store::sqlite::{ReplicaPosture, SqliteBackend};
    use fauna_account_store::store::AccountStore;
    use fauna_account_store::types::WriterId;
    use fauna_client_accounts::InMemorySecretStore;
    use fauna_core::identity::ActorKeypair;
    use fauna_core::offline_intent::MlsIntentOutcome;
    use std::sync::{Arc, Mutex};

    /// The successor's outbox, as a plain store.
    struct Successor(AccountStore<MemoryBackend>);

    #[async_trait::async_trait]
    impl CarryTarget for Successor {
        async fn carry_intent(
            &self,
            intent: NewOutboxIntent,
            parked: Option<String>,
        ) -> Result<bool> {
            crate::outbox::carry_intent(&self.0, &intent, parked.as_deref()).await
        }
    }

    /// An engine that holds the groups of the payloads it names.
    struct Engine(Mutex<Vec<Vec<u8>>>);

    #[async_trait::async_trait]
    impl MlsIntentSender for Engine {
        async fn send_mls_intent(&self, _: [u8; 16], _: &[u8]) -> MlsIntentOutcome {
            MlsIntentOutcome::Retry("not in this test".into())
        }
        fn holds_group_of(&self, payload: &[u8]) -> bool {
            self.0.lock().unwrap().iter().any(|p| p == payload)
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: StoreRoot,
        pred: ActorKeypair,
        succ: ActorKeypair,
        registry: AccountRegistry,
        successor: Successor,
    }

    async fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = StoreRoot::at(tmp.path());
        let pred = ActorKeypair::from_secret([0x21; 32]);
        let succ = ActorKeypair::from_secret([0x42; 32]);
        let registry = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));
        for kp in [&pred, &succ] {
            registry
                .add_account(&hex::encode(kp.secret_bytes()), None, None)
                .unwrap();
        }
        registry
            .record_succession(&pred.actor_id_hex(), &succ.actor_id_hex())
            .unwrap();
        let successor = Successor(
            AccountStore::open(
                MemoryBackend::new(),
                &succ.actor_id_hex(),
                WriterId([0x22; 32]),
            )
            .await
            .unwrap(),
        );
        Fixture {
            _tmp: tmp,
            root,
            pred,
            succ,
            registry,
            successor,
        }
    }

    fn intent(id: u8, scope: &str, drainer: IntentDrainer) -> NewOutboxIntent {
        NewOutboxIntent {
            intent_id: [id; 16],
            kind: "fauna.posts.interact".into(),
            scope: scope.into(),
            payload: vec![id],
            drainer,
        }
    }

    /// The predecessor's store as a device leaves it at rest: sealed under
    /// `seed`'s replica key, holding `rows` (each parked when its reason is
    /// `Some`), then closed.
    async fn predecessor_store(
        root: &StoreRoot,
        actor: &ActorKeypair,
        seed: &[u8; 32],
        rows: &[(NewOutboxIntent, Option<&str>)],
    ) {
        let dir = root.store_dir(&actor.actor_id_hex()).unwrap();
        let backend =
            SqliteBackend::open_keyed(&dir, replica_seal_key(seed), ReplicaPosture::Sealed)
                .unwrap();
        let store = AccountStore::open(backend, &actor.actor_id_hex(), WriterId([0x11; 32]))
            .await
            .unwrap();
        for (row, parked) in rows {
            assert!(store.outbox_append(row).await.unwrap());
            if let Some(code) = parked {
                store
                    .outbox_mark_failed(&row.intent_id, code)
                    .await
                    .unwrap();
            }
        }
    }

    fn carry(fx: &Fixture) -> OutboxCarry {
        OutboxCarry::new(fx.registry.clone(), fx.root.clone())
    }

    fn owed(fx: &Fixture) -> bool {
        fx.registry.outbox_carry_owed(&fx.pred.actor_id_hex())
    }

    /// Two scopes of pending rows and a parked one move verbatim, in order,
    /// the parked one parked with its reason; the predecessor's store is left
    /// empty and the registry records the carry, so a second pass opens
    /// nothing.
    #[tokio::test]
    async fn a_predecessors_outbox_moves_verbatim_in_order_and_the_carry_settles() {
        let fx = fixture().await;
        predecessor_store(
            &fx.root,
            &fx.pred,
            fx.pred.secret_bytes(),
            &[
                (intent(1, "posts.interact:a", IntentDrainer::Rpc), None),
                (intent(2, "posts.interact:a", IntentDrainer::Rpc), None),
                (intent(3, "knocks.accept:b", IntentDrainer::Identity), None),
                (
                    intent(4, "posts.interact:c", IntentDrainer::Rpc),
                    Some("fauna.posts.not_found"),
                ),
            ],
        )
        .await;
        assert!(owed(&fx));

        let report = carry(&fx)
            .run(&fx.succ.actor_id_hex(), &fx.successor, None)
            .await;
        assert_eq!((report.carried, report.waiting, report.settled), (4, 0, 1));
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        let rows = fx.successor.0.outbox_undrained().await.unwrap();
        let got: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.intent_id[0],
                    r.scope.as_str(),
                    r.drainer,
                    r.payload.clone(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (3, "knocks.accept:b", IntentDrainer::Identity, vec![3]),
                (1, "posts.interact:a", IntentDrainer::Rpc, vec![1]),
                (2, "posts.interact:a", IntentDrainer::Rpc, vec![2]),
                (4, "posts.interact:c", IntentDrainer::Rpc, vec![4]),
            ]
        );
        let parked = rows.iter().find(|r| r.intent_id == [4; 16]).unwrap();
        assert_eq!(parked.status, IntentStatus::Failed);
        assert_eq!(
            parked.failure_reason.as_deref(),
            Some("fauna.posts.not_found")
        );
        assert_eq!(
            read_rows(
                &fx.root,
                &fx.pred.actor_id_hex(),
                replica_seal_key(fx.pred.secret_bytes())
            )
            .await
            .unwrap()
            .unwrap(),
            vec![],
            "the predecessor's store holds nothing more"
        );
        assert!(!owed(&fx), "the registry records the carry");
        assert!(
            fx.registry
                .outbox_carry_sources(&fx.succ.actor_id_hex())
                .is_empty()
        );

        let again = carry(&fx)
            .run(&fx.succ.actor_id_hex(), &fx.successor, None)
            .await;
        assert_eq!(again, CarryReport::default(), "a second pass opens nothing");
    }

    /// An MLS row moves only once the successor's engine holds its group —
    /// listed as held by the carry meanwhile, with its scope's later rows
    /// behind it — and the carry stays owed until it has moved.
    #[tokio::test]
    async fn an_mls_row_waits_listed_as_held_until_its_group_is_held() {
        let fx = fixture().await;
        let mls = intent(5, "conv:aa", IntentDrainer::Mls);
        let behind = intent(6, "conv:aa", IntentDrainer::Mls);
        predecessor_store(
            &fx.root,
            &fx.pred,
            fx.pred.secret_bytes(),
            &[
                (mls.clone(), None),
                (behind.clone(), None),
                (intent(7, "posts.interact:a", IntentDrainer::Rpc), None),
            ],
        )
        .await;
        let engine = Engine(Mutex::new(Vec::new()));
        let carry = carry(&fx);
        let succ = fx.succ.actor_id_hex();

        // No engine at all: no MLS row moves.
        let report = carry.run(&succ, &fx.successor, None).await;
        assert_eq!((report.carried, report.waiting, report.settled), (1, 2, 0));
        // An engine without the group: still nothing of the conversation.
        let report = carry.run(&succ, &fx.successor, Some(&engine)).await;
        assert_eq!((report.carried, report.waiting, report.settled), (0, 2, 0));
        assert!(owed(&fx));
        let held = carry.held_changes(&succ).await;
        assert_eq!(held.len(), 2);
        for change in &held {
            assert_eq!(change.state, UnsentState::Held);
            assert_eq!(
                change.hold.as_deref(),
                Some(hold_code::CARRY_AWAITING_GROUP)
            );
        }
        assert!(carry.holds(&succ, [5; 16]).await);

        // The group arrives (the sweep's add-successor reached the device).
        engine.0.lock().unwrap().push(mls.payload.clone());
        engine.0.lock().unwrap().push(behind.payload.clone());
        let report = carry.run(&succ, &fx.successor, Some(&engine)).await;
        assert_eq!((report.carried, report.waiting, report.settled), (2, 0, 1));
        let ids: Vec<u8> = fx
            .successor
            .0
            .outbox_undrained()
            .await
            .unwrap()
            .iter()
            .map(|r| r.intent_id[0])
            .collect();
        assert_eq!(ids, vec![5, 6, 7], "the conversation's rows in their order");
        assert!(carry.held_changes(&succ).await.is_empty());
        assert!(!owed(&fx));
    }

    /// A row not carried yet is discarded from the predecessor's store by the
    /// user's word, and the carry then settles.
    #[tokio::test]
    async fn a_held_row_is_discarded_from_the_predecessors_store() {
        let fx = fixture().await;
        predecessor_store(
            &fx.root,
            &fx.pred,
            fx.pred.secret_bytes(),
            &[(intent(8, "conv:bb", IntentDrainer::Mls), None)],
        )
        .await;
        let carry = carry(&fx);
        let succ = fx.succ.actor_id_hex();
        assert!(carry.discard(&succ, [8; 16]).await.unwrap());
        assert!(
            !carry.discard(&succ, [8; 16]).await.unwrap(),
            "already gone"
        );
        let report = carry.run(&succ, &fx.successor, None).await;
        assert_eq!((report.carried, report.settled), (0, 1));
        assert!(fx.successor.0.outbox_undrained().await.unwrap().is_empty());
    }

    /// A store sealed under another seed — a thief's, say — does not open:
    /// nothing is carried, nothing is recorded, the error is reported.
    #[tokio::test]
    async fn a_store_sealed_under_another_seed_carries_nothing() {
        let fx = fixture().await;
        predecessor_store(
            &fx.root,
            &fx.pred,
            &[0x99; 32],
            &[(intent(9, "posts.interact:a", IntentDrainer::Rpc), None)],
        )
        .await;
        let report = carry(&fx)
            .run(&fx.succ.actor_id_hex(), &fx.successor, None)
            .await;
        assert_eq!((report.carried, report.settled), (0, 0));
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert!(fx.successor.0.outbox_undrained().await.unwrap().is_empty());
        assert!(owed(&fx));
    }

    /// A crash between the append and the predecessor's delete leaves the
    /// row in both stores; the next pass's append answers that the id is
    /// there and the delete runs — one row, never two.
    #[tokio::test]
    async fn a_crash_between_append_and_delete_leaves_one_row() {
        let fx = fixture().await;
        let row = intent(10, "posts.interact:a", IntentDrainer::Rpc);
        predecessor_store(
            &fx.root,
            &fx.pred,
            fx.pred.secret_bytes(),
            &[(row.clone(), None)],
        )
        .await;
        // The crash: appended, never deleted.
        assert!(fx.successor.carry_intent(row.clone(), None).await.unwrap());
        let report = carry(&fx)
            .run(&fx.succ.actor_id_hex(), &fx.successor, None)
            .await;
        assert_eq!(report.settled, 1);
        assert_eq!(fx.successor.0.outbox_undrained().await.unwrap().len(), 1);
    }

    /// No predecessor store on this device: nothing to carry, and the carry
    /// settles (a device that never held the retired identity owes nothing).
    #[tokio::test]
    async fn no_store_at_rest_settles_the_carry() {
        let fx = fixture().await;
        let report = carry(&fx)
            .run(&fx.succ.actor_id_hex(), &fx.successor, None)
            .await;
        assert_eq!((report.carried, report.settled), (0, 1));
        assert!(!owed(&fx));
    }
}

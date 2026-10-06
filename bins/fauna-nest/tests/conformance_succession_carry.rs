//! A successor's walk carries its predecessor's generation-0 delegable rows
//! (`docs/goal/behavior/succession-aftermath.md` § Re-key scope → *The
//! account-state plane's generation-0 delegable rows are carried by the
//! successor's walk*), over the real nest handlers and real account stores.
//!
//! A predecessor identity's delegable rows — the preference cluster, the
//! seen-set, the read markers — are sealed under keys derived from ITS
//! `BackupKey`, so a successor's own schedule opens none of them. The
//! successor's plane is handed each attested predecessor's **delegable**
//! schedule; a walked row that opens under one is merged by its kind's
//! ordinary policy and re-authored as the walking device's own row, value and
//! merge metadata verbatim, so it publishes sealed under the successor's
//! schedule and a device holding only the successor's seed reads it.
//!
//! The fleet scope crosses ONE kind the same way, under its own keys and a
//! narrower gate: the mint record of a generation whose key the walking
//! device already holds (`docs/goal/architecture/owner-key-material.md`
//! § Path A-sibling-2 → *Rotation*, the succession rider → *What crosses*).
//! Cases (e) and (g) pin that carry's edges here; its end-to-end proof — the
//! re-escrow, the fresh seed-only device — is
//! `conformance_account_state_walk.rs`'s succession floor.
//!
//! Nothing here touches a blob rail: none is registered, so a
//! value that reaches a successor reached it by the walk.
//!
//! Tier: tier_3 (real nest handlers + real stores). Every assertion is on
//! latency-independent state (e2e convention 14): walks are driven
//! explicitly, no sleeps, and relative stamp order is established by
//! construction where it matters.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use ed25519_dalek::SigningKey;
use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey, DelegableSchedule, GenerationKey};
use fauna_core::data::ModerationConfig;
use fauna_core::encoding::canonical_encode;
use fauna_core::generation::{GenerationMintRecord, MintCore, generation_id, sign_mint_as_minter};
use fauna_core::identity::ActorKeypair;
use fauna_core::seen_set::SeenScopeSet;
use fauna_nest::{
    db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter, sync_handlers,
};
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
use fauna_protocol::merge_policy::{
    KIND_GENERATION_MINT, KIND_MODERATION, KIND_READ_MARKER, KIND_SEEN_SET, LwwStamp,
    PREFERENCE_KEY,
};
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::attested_predecessors::AttestedPredecessors;
use fauna_sync_engine::generation_tip::{GenerationTrust, RetainedKeyCustody};
use fauna_sync_engine::preference_put::put_preference_local;

const ACTOR: [u8; 32] = [0xA7; 32];

// ── The transport: the real router, dispatched in-process ────────────────────

struct RouterRequester {
    router: RpcRouter,
    state: Arc<AppState>,
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

impl RpcRequester for RouterRequester {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        common::seed_dispatch_actor(&self.state.db, &ACTOR).await;
        let meta = self.router.kind_meta(kind).expect("kind registered");
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        let reply = (meta.handler)(Arc::clone(&self.state), ACTOR, bytes)
            .await
            .map_err(Refused)?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }
}

/// The nest, with NO blob-rail handlers: a read or write of one from
/// anything under test panics on "kind registered" rather than passing.
/// One nest actor stands for the rows the ceremony re-pointed at the
/// successor.
fn nest() -> Arc<RouterRequester> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    Arc::new(RouterRequester {
        router: b.build(),
        state,
    })
}

// ── Two identities, per-device writers ──────────────────────────────────────

const PRED_SEED: [u8; 32] = [0x33; 32];
const SUCC_SEED: [u8; 32] = [0x44; 32];

/// The predecessor's device, and the successor's three: `S` and `S2` hold the
/// predecessor's seed (the device that ran the ceremony, and a second one
/// restored out of the successor's escrow container), `F` holds the
/// successor's seed alone.
const PRED_DEVICE: u8 = 0x0A;
const S_DEVICE: u8 = 0x0B;
const S2_DEVICE: u8 = 0x0C;
const F_DEVICE: u8 = 0x0D;

fn backup_key(seed: [u8; 32]) -> BackupKey {
    BackupKey::derive(ActorKeypair::from_secret(seed).secret_bytes())
}

fn schedule(seed: [u8; 32]) -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&backup_key(seed))
}

/// What a successor's runtime is handed for its attested predecessor: the
/// delegable branch, and nothing wider.
fn predecessor_schedules() -> Vec<DelegableSchedule> {
    vec![DelegableSchedule::derive(&backup_key(PRED_SEED))]
}

/// The whole of what a successor's runtime is handed for its attested
/// predecessor, through the constructor every host uses — the fleet plane
/// takes its mint-kind keys from here.
fn attested() -> AttestedPredecessors {
    AttestedPredecessors::from_backup_keys([(
        ActorKeypair::from_secret(PRED_SEED).actor_id(),
        &backup_key(PRED_SEED),
    )])
}

/// A device's retained-key bundle (the runtime's `PrincipalSlot`).
#[derive(Default)]
struct MemCustody(std::sync::Mutex<std::collections::BTreeMap<[u8; 32], [u8; 32]>>);

impl MemCustody {
    fn holding(keys: &[([u8; 32], &GenerationKey)]) -> Self {
        let custody = Self::default();
        for (generation, key) in keys {
            custody.record_generation_key(generation, key);
        }
        custody
    }
}

impl RetainedKeyCustody for MemCustody {
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        self.0
            .lock()
            .unwrap()
            .get(generation)
            .map(|b| GenerationKey::from_bytes(*b))
    }
    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        self.0.lock().unwrap().insert(*generation, *key.as_bytes());
    }
    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.0.lock().unwrap().remove(generation);
    }
}

fn signing_key(writer: u8) -> SigningKey {
    SigningKey::from_bytes(&[writer; 32])
}

fn writer_id(writer: u8) -> WriterId {
    WriterId(signing_key(writer).verifying_key().to_bytes())
}

async fn replica(writer: u8) -> AccountStore<SqliteBackend> {
    AccountStore::open(
        SqliteBackend::open_in_memory().unwrap(),
        &hex::encode(ACTOR),
        writer_id(writer),
    )
    .await
    .unwrap()
}

static PRED_TRUST: std::sync::LazyLock<GenerationTrust> =
    std::sync::LazyLock::new(|| GenerationTrust {
        root: ActorKeypair::from_secret(PRED_SEED).actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    });

/// The successor's writer-door trust: its own root, the predecessor attested.
static SUCC_TRUST: std::sync::LazyLock<GenerationTrust> =
    std::sync::LazyLock::new(|| GenerationTrust {
        root: ActorKeypair::from_secret(SUCC_SEED).actor_id(),
        prior: vec![ActorKeypair::from_secret(PRED_SEED).actor_id()],
        trusted_holders: Default::default(),
    });

type Plane<'a> = AccountStatePlane<'a, SqliteBackend, Arc<RouterRequester>>;

fn pred_plane<'a>(
    store: &'a AccountStore<SqliteBackend>,
    rpc: &'a Arc<RouterRequester>,
    keys: &'a AccountStateKeySchedule,
    device: &'a SigningKey,
    scope: &str,
) -> Plane<'a> {
    AccountStatePlane::new(store, rpc, keys, device, &PRED_TRUST, scope).unwrap()
}

/// A successor device's delegable-scope plane with no predecessor material —
/// a seed-only device, and what every successor was before the carry.
fn succ_plane<'a>(
    store: &'a AccountStore<SqliteBackend>,
    rpc: &'a Arc<RouterRequester>,
    keys: &'a AccountStateKeySchedule,
    device: &'a SigningKey,
) -> Plane<'a> {
    AccountStatePlane::new(store, rpc, keys, device, &SUCC_TRUST, ACCOUNT_STATE_SCOPE).unwrap()
}

fn moderation_bytes(words: &[&str]) -> Vec<u8> {
    canonical_encode(&ModerationConfig {
        muted_keywords: words.iter().map(|w| (*w).into()).collect(),
        ..Default::default()
    })
    .unwrap()
}

fn moderation_item() -> ItemId {
    ItemId {
        kind: KIND_MODERATION.into(),
        key: PREFERENCE_KEY.into(),
    }
}

/// The predecessor writes its muted words the way a preference page does —
/// the plane's local put, then the runtime's publish step — and no blob.
/// Returns the stamp bytes the row carries.
async fn predecessor_writes_moderation(rpc: &Arc<RouterRequester>, words: &[&str]) -> Vec<u8> {
    let keys = schedule(PRED_SEED);
    let store = replica(PRED_DEVICE).await;
    let device = signing_key(PRED_DEVICE);
    let plane = pred_plane(&store, rpc, &keys, &device, ACCOUNT_STATE_SCOPE);
    put_preference_local(&store, &plane, KIND_MODERATION, moderation_bytes(words))
        .await
        .expect("the predecessor's local put");
    assert_eq!(plane.publish_pending().await.expect("publish"), 1);
    store
        .state(KIND_MODERATION, PREFERENCE_KEY)
        .await
        .unwrap()
        .expect("the predecessor holds its own entry")
        .merge_meta
        .expect("a stamped entry")
}

/// How many rows this replica has authored in the delegable scope — the
/// "did this walk write an own row" observable.
async fn own_rows(store: &AccountStore<SqliteBackend>, device: u8) -> usize {
    store
        .scope_rows(ACCOUNT_STATE_SCOPE, &writer_id(device), 0, 1000)
        .await
        .unwrap()
        .len()
}

// ── (a) the carry, and the floor returning to the seed alone ────────────────

/// A successor whose device holds the predecessor's seed carries the
/// predecessor's muted words in one walk, stamp untouched, and re-publishes
/// them sealed under its own schedule; a device holding ONLY the successor's
/// seed then reads them. No blob-rail leg exists in this file to do it.
#[tokio::test]
async fn a_successors_fresh_replica_reads_the_e1_value_its_predecessor_wrote() {
    let rpc = nest();
    let pred_stamp = predecessor_writes_moderation(&rpc, &["witness"]).await;

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);

    let walk = s_plane.walk().await.expect("the successor's walk");
    assert_eq!(
        (walk.inherited, walk.unopened, walk.applied),
        (1, 0, 0),
        "the predecessor's row is carried, not left unopened and not ingested: {walk:?}"
    );
    let entry = s
        .state(KIND_MODERATION, PREFERENCE_KEY)
        .await
        .unwrap()
        .expect("the successor holds the moderation record");
    assert_eq!(entry.value, moderation_bytes(&["witness"]));
    assert_eq!(
        entry.merge_meta.as_deref(),
        Some(pred_stamp.as_slice()),
        "the carried row keeps its predecessor-era stamp, so latest-wins is kept"
    );
    assert_eq!(
        own_rows(&s, S_DEVICE).await,
        1,
        "re-authored as this device's own row"
    );
    assert!(
        s.scope_rows(ACCOUNT_STATE_SCOPE, &writer_id(PRED_DEVICE), 0, 1000)
            .await
            .unwrap()
            .is_empty(),
        "nothing is journaled at the predecessor writer's coordinate"
    );

    // F: the successor's seed and nothing else. The carry's put named the
    // predecessor's own row in `replaces`, so the nest superseded it and the
    // feed serves S's re-authored copy alone, which opens.
    let f = replica(F_DEVICE).await;
    let f_device = signing_key(F_DEVICE);
    let f_plane = succ_plane(&f, &rpc, &succ_keys, &f_device);
    let walk = f_plane.walk().await.expect("the seed-only device's walk");
    assert_eq!(
        (walk.applied, walk.unopened, walk.inherited),
        (1, 0, 0),
        "it adopts the re-authored row, and no predecessor row is served: {walk:?}"
    );
    let entry = f
        .state(KIND_MODERATION, PREFERENCE_KEY)
        .await
        .unwrap()
        .expect("the seed-only device holds the moderation record");
    assert_eq!(entry.value, moderation_bytes(&["witness"]));
}

// ── (a′) the seed-only floor across a succession, the whole cluster ─────────

/// **The seed-only floor for the preference cluster, across a succession**
/// (`config-dissolution.md` § The `__config` dissolution schedule → *The
/// closure order*, step (5): the floor test that lands before the CAS-blob
/// bridge is deleted; its successor leg).
///
/// The predecessor stores a record of every kind in the cluster. One device of
/// the successor holds the predecessor's seed and walks once. A fresh device
/// holding the successor's seed ALONE then reads all four records — with no
/// blob-rail handler on the nest, so nothing but the walk can have carried
/// them. The tip-sealed kinds are not in this leg: their crossing is the
/// succession rider's (step (4)), not built on a successor with its own
/// schedule.
///
/// Red-verified: with `S` built without `with_predecessor_schedules` (and its
/// walk's own assertion lifted), `F`'s walk leaves all four rows unopened and
/// it holds none of the records.
#[tokio::test]
async fn a_device_holding_only_the_successors_seed_reads_the_whole_inherited_preference_cluster() {
    let rpc = nest();
    let cluster = common::preference_cluster();
    {
        let keys = schedule(PRED_SEED);
        let store = replica(PRED_DEVICE).await;
        let device = signing_key(PRED_DEVICE);
        let plane = pred_plane(&store, &rpc, &keys, &device, ACCOUNT_STATE_SCOPE);
        for (kind, value) in &cluster {
            put_preference_local(&store, &plane, kind, value.clone())
                .await
                .expect("the predecessor's local put");
        }
        assert_eq!(
            plane.publish_pending().await.expect("publish"),
            cluster.len()
        );
    }

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);
    let walk = s_plane.walk().await.expect("the successor's one walk");
    assert_eq!(
        (walk.inherited, walk.unopened),
        (cluster.len(), 0),
        "every record of the cluster is carried: {walk:?}"
    );
    drop(s_plane);
    drop(s); // The carrying device is lost; what it re-authored is on the nest.

    let f = replica(F_DEVICE).await;
    let f_device = signing_key(F_DEVICE);
    let f_plane = succ_plane(&f, &rpc, &succ_keys, &f_device);
    let walk = f_plane.walk().await.expect("the seed-only device's walk");
    for (kind, value) in &cluster {
        let held = f
            .state(kind, PREFERENCE_KEY)
            .await
            .unwrap()
            .map(|e| e.value);
        assert_eq!(
            held.as_ref(),
            Some(value),
            "the seed-only successor device reads the inherited {kind} record: {walk:?}"
        );
    }
}

// ── (b) no predecessor material, nothing carried ────────────────────────────

#[tokio::test]
async fn a_successor_handed_no_predecessor_schedule_carries_nothing() {
    let rpc = nest();
    predecessor_writes_moderation(&rpc, &["witness"]).await;

    let succ_keys = schedule(SUCC_SEED);
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane = succ_plane(&s, &rpc, &succ_keys, &s_device);

    let walk = s_plane.walk().await.expect("walk");
    assert_eq!(
        (walk.applied, walk.unopened, walk.inherited),
        (0, 1, 0),
        "the successor opens none of the predecessor's delegable rows: {walk:?}"
    );
    assert!(
        s.state(KIND_MODERATION, PREFERENCE_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(s_plane.publish_pending().await.expect("publish"), 0);
    assert_eq!(own_rows(&s, S_DEVICE).await, 0);
}

// ── (c) latest-wins is kept ─────────────────────────────────────────────────

/// A value the successor wrote since the succession outranks the inherited
/// one: the carried row keeps its predecessor-era stamp, so the ordinary
/// merge keeps the successor's value and the walk writes nothing.
#[tokio::test]
async fn a_value_the_successor_wrote_since_is_not_overwritten() {
    let rpc = nest();
    let pred_stamp = predecessor_writes_moderation(&rpc, &["witness"]).await;
    let pred_stamp = LwwStamp::decode(&pred_stamp).unwrap();

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);

    // The successor's own write, strictly newer by construction.
    let own = LwwStamp {
        at_ms: pred_stamp.at_ms + 1,
        writer: writer_id(S_DEVICE).0,
    };
    s_plane
        .put(
            &moderation_item(),
            moderation_bytes(&["mine"]),
            Some(own.encode().unwrap()),
        )
        .await
        .expect("the successor's own write");
    assert_eq!(own_rows(&s, S_DEVICE).await, 1);

    let walk = s_plane.walk().await.expect("walk");
    assert_eq!(
        (walk.inherited, walk.unopened),
        (1, 0),
        "the predecessor's row opened and lost: {walk:?}"
    );
    let entry = s
        .state(KIND_MODERATION, PREFERENCE_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.value, moderation_bytes(&["mine"]));
    assert_eq!(
        own_rows(&s, S_DEVICE).await,
        1,
        "the walk journaled no own row for the item"
    );
}

// ── (d) idempotent across walks and devices ─────────────────────────────────

#[tokio::test]
async fn a_second_walk_writes_nothing_and_a_second_device_converges() {
    let rpc = nest();
    predecessor_writes_moderation(&rpc, &["witness"]).await;

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);

    s_plane.walk().await.expect("first walk");
    assert_eq!(own_rows(&s, S_DEVICE).await, 1);
    // The full-state reconcile from a zero frontier finds no predecessor row
    // to re-serve: the carry's put superseded it. Nothing opens under the
    // predecessor's schedule and nothing is written.
    let again = s_plane.reconcile().await.expect("reconcile");
    assert_eq!(
        (again.inherited, again.unopened),
        (0, 0),
        "the predecessor's row is superseded, not re-presented: {again:?}"
    );
    assert_eq!(
        own_rows(&s, S_DEVICE).await,
        1,
        "the second walk wrote no own row"
    );
    assert_eq!(s_plane.publish_pending().await.expect("publish"), 0);

    // A second successor device holding the predecessor's seed, walking after
    // S published. The predecessor's row is superseded, so it is served S's
    // re-authored row alone; what is pinned is the value, and that it settles
    // with at most one equal row of its own.
    let s2 = replica(S2_DEVICE).await;
    let s2_device = signing_key(S2_DEVICE);
    let s2_plane =
        succ_plane(&s2, &rpc, &succ_keys, &s2_device).with_predecessor_schedules(&inherited);
    s2_plane.walk().await.expect("the second device's walk");
    let entry = s2
        .state(KIND_MODERATION, PREFERENCE_KEY)
        .await
        .unwrap()
        .expect("the second device holds the record");
    assert_eq!(entry.value, moderation_bytes(&["witness"]));
    let settled = own_rows(&s2, S2_DEVICE).await;
    assert!(settled <= 1, "at most one equal row per device per item");
    s2_plane.reconcile().await.expect("the second device again");
    assert_eq!(own_rows(&s2, S2_DEVICE).await, settled);
    assert_eq!(s2_plane.publish_pending().await.expect("publish"), 0);

    // And S, meeting S2's equal row, writes nothing either.
    s_plane.reconcile().await.expect("S after S2");
    assert_eq!(own_rows(&s, S_DEVICE).await, 1);
}

// ── (e) the carry never widens to the fleet branch ──────────────────────────

/// A predecessor's fleet-only generation-0 row (its escrow-target row) stays
/// unopened on the successor's fleet plane — with no predecessor material,
/// and equally when handed everything a successor's runtime holds for its
/// predecessor: the delegable schedule type holds no fleet branch, the
/// mint-kind keys (which the driver does set on this plane) open the mint
/// kind alone, and the retired machinery keys (which the driver sets on it
/// too) are tried by the reclamation pass's predecessor arm only, never by
/// the walk (`account-data-taxonomy.md` § The generation machinery →
/// *Fleet-scope reclamation*, clause (3)(i)).
#[tokio::test]
async fn a_predecessors_fleet_only_generation_0_row_stays_unopened() {
    let rpc = nest();

    let pred_keys = schedule(PRED_SEED);
    let pred_store = replica(PRED_DEVICE).await;
    let pred_device = signing_key(PRED_DEVICE);
    let pred_fleet = pred_plane(
        &pred_store,
        &rpc,
        &pred_keys,
        &pred_device,
        ACCOUNT_STATE_FLEET_SCOPE,
    );
    let target = fauna_sync_engine::generation_mint::escrow_target_entry(&PRED_SEED).unwrap();
    assert_eq!(target.scope, ACCOUNT_STATE_FLEET_SCOPE);
    pred_fleet
        .put(
            &ItemId {
                kind: target.kind.clone(),
                key: target.key.clone(),
            },
            target.value.clone(),
            target.merge_meta.clone(),
        )
        .await
        .expect("the predecessor's fleet-only row");

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let attested = attested();
    for (device, handed) in [(S_DEVICE, false), (S2_DEVICE, true)] {
        let store = replica(device).await;
        let key = signing_key(device);
        let fleet = AccountStatePlane::new(
            &store,
            &rpc,
            &succ_keys,
            &key,
            &SUCC_TRUST,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        let fleet = if handed {
            fleet
                .with_predecessor_schedules(&inherited)
                .with_predecessor_mint_keys(attested.mint_kind_keys())
                .with_predecessor_machinery_keys(attested.retired_machinery_keys())
        } else {
            fleet
        };
        let walk = fleet.walk().await.expect("the successor's fleet walk");
        assert_eq!(
            (walk.unopened, walk.inherited, walk.applied),
            (1, 0, 0),
            "handed={handed}: {walk:?}"
        );
        assert!(
            store
                .state(&target.kind, &target.key)
                .await
                .unwrap()
                .is_none(),
            "handed={handed}"
        );
        assert_eq!(
            store
                .scope_rows(ACCOUNT_STATE_FLEET_SCOPE, &writer_id(device), 0, 1000)
                .await
                .unwrap()
                .len(),
            0,
            "handed={handed}: nothing re-authored"
        );
    }
}

// ── (f) a CRDT kind crosses too ─────────────────────────────────────────────

/// The carry is the delegable rung, not the preference cluster: a seen-set
/// row — which has no blob-rail road at all — is carried and read by a
/// seed-only device.
#[tokio::test]
async fn a_predecessors_seen_set_row_is_carried_too() {
    let rpc = nest();

    let mut seen = SeenScopeSet::new();
    seen.raise_watermark([0x51; 32], 7);
    seen.insert_ref([0x52; 32], 9);
    let seen = encode_canonical(&seen).unwrap().to_vec();
    let item = ItemId {
        kind: KIND_SEEN_SET.into(),
        key: "scope-x".into(),
    };
    {
        let keys = schedule(PRED_SEED);
        let store = replica(PRED_DEVICE).await;
        let device = signing_key(PRED_DEVICE);
        pred_plane(&store, &rpc, &keys, &device, ACCOUNT_STATE_SCOPE)
            .put(&item, seen.clone(), None)
            .await
            .expect("the predecessor's seen-set row");
    }

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);
    let walk = s_plane.walk().await.expect("the successor's walk");
    assert_eq!((walk.inherited, walk.unopened), (1, 0), "{walk:?}");
    // The carry's put superseded the predecessor's row, so a reconcile from a
    // zero frontier opens nothing under the predecessor's schedule.
    let again = s_plane.reconcile().await.expect("reconcile");
    assert_eq!((again.inherited, again.unopened), (0, 0), "{again:?}");
    assert_eq!(
        own_rows(&s, S_DEVICE).await,
        1,
        "a CRDT row is re-authored once, like any other"
    );

    let f = replica(F_DEVICE).await;
    let f_device = signing_key(F_DEVICE);
    let f_plane = succ_plane(&f, &rpc, &succ_keys, &f_device);
    f_plane.walk().await.expect("the seed-only device's walk");
    let entry = f
        .state(KIND_SEEN_SET, "scope-x")
        .await
        .unwrap()
        .expect("the seed-only device holds the seen-set row");
    assert_eq!(entry.value, seen);
}

// ── (g) the mint record's gate: bound, signed, and its key in hand ──────────

/// One mint record as the predecessor's device would have written it: a core
/// committing to `key`, minted and signed by `minter`.
fn mint_record(minter: &SigningKey, key: &GenerationKey, at_ms: i64) -> ([u8; 32], MintCore) {
    let id = minter.verifying_key().to_bytes();
    let core = MintCore {
        parents: Vec::new(),
        member_ids: vec![id],
        minter: id,
        key_commitment: key.commitment(),
        minted_at_ms: at_ms,
    };
    (generation_id(&core).unwrap(), core)
}

fn minted(core: &MintCore, minter_sig: Vec<u8>) -> Vec<u8> {
    canonical_encode(&GenerationMintRecord::Minted {
        core: core.clone(),
        minter_sig,
        wraps: Vec::new(),
    })
    .unwrap()
}

/// The fleet walk carries a predecessor's mint row only when it is the true
/// record of a generation this device already keys (`owner-key-material.md`
/// § Path A-sibling-2 → *Rotation*, the succession rider → *What crosses*,
/// conditions (a) and (b)). Four predecessor-sealed mint rows are served, the
/// device holds a matching key for the first three, and exactly one crosses:
///
/// - `honest` — id-bound, signed, key held: **carried**, value verbatim;
/// - `misfiled` — an honest signed record re-filed under another logical key
///   (the Key↔id binding fails): left unopened;
/// - `unsigned` — id-bound, key held, a junk minter signature: left unopened;
/// - `unkeyed` — id-bound and signed, no key in the bundle: left unopened.
///
/// A row that is left writes nothing: no entry, no own row.
#[tokio::test]
async fn a_predecessors_mint_row_crosses_only_bound_signed_and_keyed() {
    let rpc = nest();

    let pred_keys = schedule(PRED_SEED);
    let pred_store = replica(PRED_DEVICE).await;
    let pred_device = signing_key(PRED_DEVICE);
    let pred_fleet = pred_plane(
        &pred_store,
        &rpc,
        &pred_keys,
        &pred_device,
        ACCOUNT_STATE_FLEET_SCOPE,
    );
    let keys: Vec<GenerationKey> = (0u8..4)
        .map(|n| GenerationKey::from_bytes([0x50 + n; 32]))
        .collect();
    let records: Vec<([u8; 32], MintCore)> = keys
        .iter()
        .zip(1i64..)
        .map(|(key, at_ms)| mint_record(&pred_device, key, at_ms))
        .collect();
    let [honest, misfiled, unsigned, unkeyed] = &records[..] else {
        unreachable!()
    };
    let signed =
        |(id, core): &([u8; 32], MintCore)| minted(core, sign_mint_as_minter(&pred_device, id));
    let squatted_key = hex::encode([0xEE; 32]);
    let rows: [(String, Vec<u8>); 4] = [
        (hex::encode(honest.0), signed(honest)),
        (squatted_key.clone(), signed(misfiled)),
        (hex::encode(unsigned.0), minted(&unsigned.1, vec![0u8; 64])),
        (hex::encode(unkeyed.0), signed(unkeyed)),
    ];
    for (key, value) in &rows {
        pred_fleet
            .put(
                &ItemId {
                    kind: KIND_GENERATION_MINT.into(),
                    key: key.clone(),
                },
                value.clone(),
                None,
            )
            .await
            .expect("the predecessor's mint row");
    }

    let succ_keys = schedule(SUCC_SEED);
    let attested = attested();
    let store = replica(S_DEVICE).await;
    let device = signing_key(S_DEVICE);
    // The bundle holds a matching key for every row but the last — the
    // misfiled record's at its TRUE id, and (to show a held key at the
    // squatted id buys nothing) the same key there too.
    let custody = MemCustody::holding(&[
        (honest.0, &keys[0]),
        (misfiled.0, &keys[1]),
        ([0xEE; 32], &keys[1]),
        (unsigned.0, &keys[2]),
    ]);
    let fleet = AccountStatePlane::new(
        &store,
        &rpc,
        &succ_keys,
        &device,
        &SUCC_TRUST,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap()
    .with_generation_custody(&custody)
    .with_predecessor_mint_keys(attested.mint_kind_keys());

    let walk = fleet.walk().await.expect("the successor's fleet walk");
    // Which rows crossed, by key, before the counts — so a red run names the
    // condition that let one through.
    let held: Vec<(String, Vec<u8>)> = store
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.value))
        .collect();
    let name = |key: &str| {
        ["honest", "misfiled", "unsigned", "unkeyed"]
            .into_iter()
            .zip(&rows)
            .find(|(_, (k, _))| k == key)
            .map_or("?", |(name, _)| name)
    };
    assert_eq!(
        held.iter().map(|(k, _)| name(k)).collect::<Vec<_>>(),
        vec!["honest"],
        "only the true record of a generation this device keys crosses"
    );
    assert_eq!(held, vec![rows[0].clone()], "and it crosses verbatim");
    assert_eq!(
        (
            walk.inherited,
            walk.unopened,
            walk.applied,
            walk.unmergeable
        ),
        (1, 3, 0, 0),
        "{walk:?}"
    );
    assert_eq!(
        store
            .scope_rows(ACCOUNT_STATE_FLEET_SCOPE, &writer_id(S_DEVICE), 0, 1000)
            .await
            .unwrap()
            .len(),
        1,
        "re-authored once, as this device's own row"
    );

    // Every later presentation is a no-op (each full-state reconcile
    // re-serves all four).
    let again = fleet.reconcile().await.expect("reconcile");
    assert_eq!((again.inherited, again.unopened), (1, 3), "{again:?}");
    assert_eq!(fleet.publish_pending().await.expect("publish"), 0);

    // With no bundle attached, key in hand cannot be shown: nothing crosses.
    let bare_store = replica(S2_DEVICE).await;
    let bare_device = signing_key(S2_DEVICE);
    let bare = AccountStatePlane::new(
        &bare_store,
        &rpc,
        &succ_keys,
        &bare_device,
        &SUCC_TRUST,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap()
    .with_predecessor_mint_keys(attested.mint_kind_keys());
    let walk = bare.walk().await.expect("the bundle-less walk");
    // S's re-authored copy opens under the successor's own schedule and is
    // applied as any sibling's row is; the predecessor's four stay unopened.
    assert_eq!(
        (walk.inherited, walk.unopened, walk.applied),
        (0, 4, 1),
        "{walk:?}"
    );
}

// ── (h) the predecessor's own row is retired behind the carry ───────────────
//
// `succession-aftermath.md` § Re-key scope → *The predecessor's own row is
// retired behind the carry*: a carry's put names the predecessor's row it was
// carried from, so the nest supersedes it in the put's own transaction and the
// carry is count-neutral, even at the cap; a predecessor row the walk opened
// but did not re-author (it lost the merge) is retired by the pass step
// (`fauna_sync_engine::delegable_reclaim`) once a listed member row carries
// the item.

/// The nest's live rows of the delegable scope, counted by origin writer —
/// the census the 2026-10-01 measurement read.
async fn census(rpc: &Arc<RouterRequester>) -> std::collections::BTreeMap<Vec<u8>, usize> {
    let Some(folder) = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_SCOPE)
        .await
        .unwrap()
    else {
        return Default::default();
    };
    let read = rpc
        .state
        .db
        .get_account_state_changes(folder, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap();
    let mut by_writer = std::collections::BTreeMap::new();
    for row in read.rows {
        *by_writer
            .entry(row.origin_writer.expect("a device-writer row"))
            .or_insert(0) += 1;
    }
    by_writer
}

fn live_rows_of(census: &std::collections::BTreeMap<Vec<u8>, usize>, device: u8) -> usize {
    census
        .get(writer_id(device).0.as_slice())
        .copied()
        .unwrap_or(0)
}

/// One successor pass over the delegable scope, as the pump runs it: the
/// journal publish (a `scope_full` refusal is the pass's reported error, not
/// a stop), the full-state reconcile, and the retire behind the carry.
async fn successor_pass(
    store: &AccountStore<SqliteBackend>,
    plane: &Plane<'_>,
    device: &SigningKey,
) -> (
    fauna_sync_engine::account_state_plane::WalkReport,
    fauna_sync_engine::delegable_reclaim::CarryRetire,
) {
    if let Err(e) = plane.publish_pending().await {
        assert!(
            fauna_sync_engine::account_state_plane::is_scope_full(&e),
            "publish_pending: {e:#}"
        );
    }
    let walk = plane.reconcile().await.expect("the successor's reconcile");
    let retire = fauna_sync_engine::delegable_reclaim::retire_behind_carry(
        store,
        plane,
        &SUCC_TRUST,
        device,
    )
    .await
    .expect("the retire behind the carry")
    .expect("a plane holding the predecessor schedule, after a completed reconcile");
    (walk, retire)
}

/// Run [`successor_pass`] until the nest's census stops moving (bounded).
async fn settle(
    rpc: &Arc<RouterRequester>,
    store: &AccountStore<SqliteBackend>,
    plane: &Plane<'_>,
    device: &SigningKey,
) -> std::collections::BTreeMap<Vec<u8>, usize> {
    let mut before = census(rpc).await;
    for _ in 0..8 {
        successor_pass(store, plane, device).await;
        let after = census(rpc).await;
        if after == before && plane.publish_pending().await.ok() == Some(0) {
            return after;
        }
        before = after;
    }
    panic!("the delegable scope did not settle in 8 passes: {before:?}");
}

/// Proof (2): one predecessor device's preference cluster, one seen-set row
/// and one read marker. A successor device holding the predecessor's
/// schedule carries every item, and its passes then retire every
/// predecessor row: zero predecessor rows live at the nest, one own row per
/// item, and a seed-only device reads every item with nothing unopened.
///
/// Red-verified: with the retire step answering `None`/nothing retired, the
/// predecessor's 6 rows stay live at every pass.
#[tokio::test]
async fn a_carried_predecessors_rows_are_retired_and_the_seed_only_device_reads_every_item() {
    let rpc = nest();
    let cluster = common::preference_cluster();
    let mut seen = SeenScopeSet::new();
    seen.raise_watermark([0x51; 32], 7);
    let seen = encode_canonical(&seen).unwrap().to_vec();
    let marker_key = fauna_core::read_marker::channel_key(&"c1".repeat(32));
    let marker = encode_canonical(&fauna_core::read_marker::ReadMarker::new(5))
        .unwrap()
        .to_vec();
    {
        let keys = schedule(PRED_SEED);
        let store = replica(PRED_DEVICE).await;
        let device = signing_key(PRED_DEVICE);
        let plane = pred_plane(&store, &rpc, &keys, &device, ACCOUNT_STATE_SCOPE);
        for (kind, value) in &cluster {
            put_preference_local(&store, &plane, kind, value.clone())
                .await
                .expect("the predecessor's local put");
        }
        for (kind, key, value) in [
            (KIND_SEEN_SET, "scope-x".to_string(), seen.clone()),
            (KIND_READ_MARKER, marker_key.clone(), marker.clone()),
        ] {
            plane
                .put_local(
                    &ItemId {
                        kind: kind.into(),
                        key,
                    },
                    value,
                    None,
                )
                .await
                .expect("the predecessor's local put");
        }
        assert_eq!(plane.publish_pending().await.expect("publish"), 6);
    }
    assert_eq!(live_rows_of(&census(&rpc).await, PRED_DEVICE), 6);

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);
    let settled = settle(&rpc, &s, &s_plane, &s_device).await;
    assert_eq!(
        (
            live_rows_of(&settled, PRED_DEVICE),
            live_rows_of(&settled, S_DEVICE)
        ),
        (0, 6),
        "every predecessor row retired, one own row per item: {settled:?}"
    );
    // Settled: a later pass meets no candidate and retires nothing.
    let (walk, retire) = successor_pass(&s, &s_plane, &s_device).await;
    assert_eq!(
        (walk.inherited, retire.candidates),
        (0, 0),
        "{walk:?} {retire:?}"
    );

    let f = replica(F_DEVICE).await;
    let f_device = signing_key(F_DEVICE);
    let walk = succ_plane(&f, &rpc, &succ_keys, &f_device)
        .walk()
        .await
        .expect("the seed-only device's walk");
    assert_eq!(walk.unopened, 0, "{walk:?}");
    for (kind, key, value) in cluster
        .iter()
        .map(|(kind, value)| (*kind, PREFERENCE_KEY.to_string(), value.clone()))
        .chain([
            (KIND_SEEN_SET, "scope-x".to_string(), seen),
            (KIND_READ_MARKER, marker_key, marker),
        ])
    {
        let held = f.state(kind, &key).await.unwrap().map(|e| e.value);
        assert_eq!(held, Some(value), "the seed-only device reads {kind}");
    }
}

/// Proof (3): latest-wins. A value the successor wrote since outranks the
/// predecessor's row, which lost the merge and was never re-authored; the
/// successor's newer row is the cover, and the predecessor's row is retired
/// with the value still the successor's and nothing re-authored.
#[tokio::test]
async fn a_predecessors_row_that_lost_the_merge_is_retired_behind_the_newer_value() {
    let rpc = nest();
    let pred_stamp = predecessor_writes_moderation(&rpc, &["witness"]).await;
    let pred_stamp = LwwStamp::decode(&pred_stamp).unwrap();

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);
    let own = LwwStamp {
        at_ms: pred_stamp.at_ms + 1,
        writer: writer_id(S_DEVICE).0,
    };
    s_plane
        .put(
            &moderation_item(),
            moderation_bytes(&["mine"]),
            Some(own.encode().unwrap()),
        )
        .await
        .expect("the successor's own write");

    let settled = settle(&rpc, &s, &s_plane, &s_device).await;
    assert_eq!(
        (
            live_rows_of(&settled, PRED_DEVICE),
            live_rows_of(&settled, S_DEVICE)
        ),
        (0, 1),
        "{settled:?}"
    );
    let entry = s
        .state(KIND_MODERATION, PREFERENCE_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.value, moderation_bytes(&["mine"]));
    assert_eq!(
        own_rows(&s, S_DEVICE).await,
        1,
        "the walk re-authored nothing"
    );
}

/// Proof (4): the cap. One predecessor device holds one pair past half the
/// cap. Each carry's put names the predecessor's row it was carried from, so
/// the nest supersedes that row in the put's own transaction and every carry
/// is count-neutral (`delegable-scope-reclamation.md` § Delegable-scope
/// reclamation → *A full scope is owed a put that needs no room*): the
/// successor's first walk returns `Ok` with nothing refused and nothing
/// parked, the scope already holds exactly that many rows, all the
/// successor's, and a seed-only device reads them whole.
///
/// Red-verified: before the build the walk failed at the first refused carry
/// (`fauna.account.state.scope_full`), and with the walk tolerant but no
/// retire step the scope stayed at 4,096 rows with two items unreadable.
#[tokio::test]
async fn at_the_cap_every_carry_is_count_neutral_and_the_scope_converges_to_the_successors_rows() {
    use fauna_protocol::account_state::MAX_STATE_ENTRIES_PER_SCOPE;
    let n = (MAX_STATE_ENTRIES_PER_SCOPE / 2 + 1) as usize;
    let rpc = nest();
    let marker_key = |i: usize| fauna_core::read_marker::channel_key(&format!("{i:064x}"));
    let marker = encode_canonical(&fauna_core::read_marker::ReadMarker::new(3))
        .unwrap()
        .to_vec();
    {
        let keys = schedule(PRED_SEED);
        let store = replica(PRED_DEVICE).await;
        let device = signing_key(PRED_DEVICE);
        let plane = pred_plane(&store, &rpc, &keys, &device, ACCOUNT_STATE_SCOPE);
        for i in 0..n {
            plane
                .put_local(
                    &ItemId {
                        kind: KIND_READ_MARKER.into(),
                        key: marker_key(i),
                    },
                    marker.clone(),
                    None,
                )
                .await
                .expect("the predecessor's marker");
        }
        assert_eq!(plane.publish_pending().await.expect("publish"), n);
    }

    let succ_keys = schedule(SUCC_SEED);
    let inherited = predecessor_schedules();
    let s = replica(S_DEVICE).await;
    let s_device = signing_key(S_DEVICE);
    let s_plane =
        succ_plane(&s, &rpc, &succ_keys, &s_device).with_predecessor_schedules(&inherited);

    // (4) The first pass: every carry supersedes the predecessor's row it
    // was carried from, so none needs room — nothing is refused, nothing is
    // parked, and the scope holds the successor's rows alone after it.
    let (walk, _retire) = successor_pass(&s, &s_plane, &s_device).await;
    assert_eq!(walk.inherited, n, "{walk:?}");
    assert_eq!(walk.carry_scope_full, 0, "{walk:?}");
    assert_eq!(
        s_plane.parked_count().await.unwrap(),
        0,
        "no carry is refused for room, so none is parked"
    );
    let after_first = census(&rpc).await;
    assert_eq!(
        (
            live_rows_of(&after_first, PRED_DEVICE),
            live_rows_of(&after_first, S_DEVICE)
        ),
        (0, n),
        "{after_first:?}"
    );

    let settled = settle(&rpc, &s, &s_plane, &s_device).await;
    assert_eq!(
        (
            live_rows_of(&settled, PRED_DEVICE),
            live_rows_of(&settled, S_DEVICE)
        ),
        (0, n),
        "{settled:?}"
    );

    let f = replica(F_DEVICE).await;
    let f_device = signing_key(F_DEVICE);
    let walk = succ_plane(&f, &rpc, &succ_keys, &f_device)
        .walk()
        .await
        .expect("the seed-only device's walk");
    assert_eq!(walk.unopened, 0, "{walk:?}");
    for i in 0..n {
        assert_eq!(
            f.state(KIND_READ_MARKER, &marker_key(i))
                .await
                .unwrap()
                .map(|e| e.value),
            Some(marker.clone()),
            "the seed-only device reads marker {i}"
        );
    }
}

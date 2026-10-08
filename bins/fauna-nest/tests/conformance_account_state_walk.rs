//! Two reading replicas of one account converging over the real nest feed —
//! the client half of the generalized account-data plane (W2.4 (account-data-plane.md § Workstreams),
//! `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors +
//! § Merge-policy seam).
//!
//! The target flow this file drives end to end: a replica writes a class-2
//! value locally (its store assigns the writer seq) → seals it under the T14
//! entry key → `fauna.account.state.put` → the peer's accounted walk pages it
//! back on the frontier → trial-opens it → dispatches through the kind's merge
//! policy → persists the merged value → the frontier advances. Both replicas
//! are real `fauna-account-store` instances and the nest is the real
//! `AppState` + `CacheDb`; nothing between them is stubbed.
//!
//! Tier: tier_3 (real nest handlers + real stores — no mocks). It lives beside
//! the nest because that is where the real handler dispatch is reachable
//! in-process; nothing here is nest-side behavior.
//!
//! Every assertion is on latency-independent state (e2e convention 14): the
//! walk is driven explicitly, so there is nothing to wait for and no sleep in
//! the file.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use common::{folder_key_custody, held_period_key, senior_rotation_key};
use ed25519_dalek::SigningKey;
use fauna_account_store::{
    sqlite::SqliteBackend,
    store::AccountStore,
    types::{StateEntry, WriterId},
};
use fauna_core::account_entry_crypto::peek_generation_id;
use fauna_core::crypto::{AccountStateKeySchedule, AudienceRung, BackupKey};
use fauna_core::data::ModerationConfig;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::generation::{
    FleetMember, GenerationMintRecord, derive_device_xwing_keypair, escrow_target_identity_key,
    escrow_target_record, sign_escrow_receipt,
};
use fauna_core::seen_set::SeenScopeSet;
use fauna_mls::wrapped_blob::generation_wraps::{build_mint, build_topup_wrap_v2};
use fauna_nest::{
    db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter, sync_handlers,
};
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::generation_escrow::{EscrowPutReply, EscrowPutRequest, KIND_ESCROW_PUT};
use fauna_protocol::merge_policy::{
    KIND_DEVICE_ENDPOINTS, KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_ESCROW_TARGET,
    KIND_GENERATION_MINT, KIND_GENERATION_WRAP, KIND_GROUP_RECEPTION_KEY, KIND_MODERATION,
    KIND_SEEN_SET, LwwStamp, MODERATION_KEY, MergePolicy, audience_rung, merge_policy,
};
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{ByteBuf, RpcError, RpcRequester, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId, WalkReport};

const ACTOR: [u8; 32] = [0xA1; 32];

// ── The transport: the real router, dispatched in-process ────────────────────

/// An [`RpcRequester`] that dispatches straight into the nest's own handler
/// table as `ACTOR`. Not a mock of the nest — it *is* the nest's request path,
/// minus the WebSocket frame.
#[derive(Clone)]
struct RouterRequester {
    router: Arc<RpcRouter>,
    state: Arc<AppState>,
    /// Every request that reached the handler table, by kind — what a pin
    /// on "no request per row" counts.
    calls: Arc<std::sync::Mutex<std::collections::BTreeMap<&'static str, usize>>>,
    /// Every retire's coordinates as `item-hex8/writer-hex8@writer_seq`, in
    /// order — what a red "no request per row" run names.
    retires: Arc<std::sync::Mutex<Vec<String>>>,
}

impl RouterRequester {
    fn calls_of(&self, kind: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .get(kind)
            .copied()
            .unwrap_or_default()
    }
    fn retires_since(&self, n: usize) -> Vec<String> {
        self.retires.lock().unwrap()[n..].to_vec()
    }
}

/// `RpcError` carries no `Display` (it is a wire value, not an error type), so
/// the seam's `Display` bound needs this one-line wrapper.
#[derive(Debug)]
struct Refused(RpcError);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.0.code, self.0.message)
    }
}

/// Every `Refused` reached the handler table and was refused there, wire
/// code intact — so the nest leg's publish classifies a real handler's
/// `stale_writer_seq` exactly as it does over the WebSocket.
impl fauna_protocol::RpcErrorClass for Refused {
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
        let Some(meta) = self.router.kind_meta(kind) else {
            // Production shape: a router that does not serve a kind refuses
            // it — the first-need mint probes the escrow door through here in
            // fixtures whose router deliberately lacks it.
            return Err(Refused(RpcError::new(
                "kind_not_served",
                "test.router.kind_not_served",
            )));
        };
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        *self.calls.lock().unwrap().entry(kind).or_default() += 1;
        if kind == fauna_protocol::account_state::KIND_STATE_RETIRE
            && let Ok(req) = fauna_protocol::decode_strict::<
                fauna_protocol::account_state::AccountStateRetireRequest,
            >(&bytes)
        {
            self.retires.lock().unwrap().push(format!(
                "{}/{}@{}",
                hex::encode(&req.item_key.as_ref()[..4]),
                &req.writer_id[..8],
                req.writer_seq
            ));
        }
        let reply = (meta.handler)(Arc::clone(&self.state), ACTOR, bytes)
            .await
            .map_err(Refused)?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }
}

async fn nest() -> RouterRequester {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    RouterRequester {
        router: Arc::new(b.build()),
        state,
        calls: Default::default(),
        retires: Default::default(),
    }
}

// ── Two replicas of one account ──────────────────────────────────────────────

/// Both replicas derive the same schedule from the same owner `BackupKey` —
/// which is the point: "a seedless enrolled device derives every key of the
/// plane" (`owner-key-material.md` § Path A-sibling-2). Their *writer* ids
/// differ; their keys never do.
fn schedule() -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&BackupKey::derive(&[7u8; 32]))
}

/// A replica's device signing key — the R13 (account-data-plane.md § The ratified decisions) in-seal writer signature's author.
///
/// Derived from the replica byte so `writer_id(b)` and `signing_key(b)` always
/// agree; the plane refuses to be constructed if they ever drift.
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

/// The escrow holder the step-6 tests trust — holder-generic on purpose: any
/// Ed25519 key can sign a verifying receipt; TRUST below is what makes THIS
/// one count (`fauna_core::generation::verify_escrow_receipt`'s
/// integrity/trust split).
fn escrow_holder() -> SigningKey {
    SigningKey::from_bytes(&[0xE5; 32])
}

/// The plane's R14 writer-door trust: certs verify against `fleet_root()`,
/// receipts count when `escrow_holder()` signed them.
static TRUST: std::sync::LazyLock<fauna_sync_engine::generation_tip::GenerationTrust> =
    std::sync::LazyLock::new(|| fauna_sync_engine::generation_tip::GenerationTrust {
        root: fleet_root().actor_id(),
        prior: Vec::new(),
        trusted_holders: vec![escrow_holder().verifying_key().to_bytes()].into(),
    });

fn plane<'a>(
    store: &'a AccountStore<SqliteBackend>,
    rpc: &'a RouterRequester,
    keys: &'a AccountStateKeySchedule,
    device: &'a SigningKey,
) -> AccountStatePlane<'a, SqliteBackend, RouterRequester> {
    AccountStatePlane::new(store, rpc, keys, device, &TRUST, ACCOUNT_STATE_SCOPE).unwrap()
}

/// The fleet-scope plane — machinery kinds and `GenerationTip` kinds seal into
/// `state-fleet` (the A5 partition), so the step-6 flows drive the door there.
fn plane_fleet<'a>(
    store: &'a AccountStore<SqliteBackend>,
    rpc: &'a RouterRequester,
    keys: &'a AccountStateKeySchedule,
    device: &'a SigningKey,
) -> AccountStatePlane<'a, SqliteBackend, RouterRequester> {
    AccountStatePlane::new(store, rpc, keys, device, &TRUST, ACCOUNT_STATE_FLEET_SCOPE).unwrap()
}

// ── The exemplar kinds, and what each is for ─────────────────────────────────
//
// `fauna.state.moderation` (delegable, latest-wins) is the door's LWW
// exemplar: put, frontier accounting, publish_pending.
//
// `fauna.state.seen-set` (delegable, union CRDT — W2.5 item 2) is the door's
// CRDT exemplar: since it registered, the CRDT-merge tests drive the real
// writer door (`put`), discharging item 0's decision (e), which had them
// staging rows door-lessly while the only CRDT kind was the gated one.
//
// `fauna.state.device-endpoints` (fleet-only, latest-wins, `GenerationTip` —
// W2.5 item 3) is the R14 door's exemplar. Since build step 6 the door
// RESOLVES rather than refuses wholesale: a `GenerationTip` origination seals
// form v2 under the current admissible, escrow-acked tip and refuses only
// when none resolves (charter § The generation machinery → *The sealing-epoch
// axis*). The step-6 section at the end of this file drives that flow through
// the door end to end; one test there stages a v1-sealed fleet row by hand to
// pin that it stays unopened (see its doc for the step-7 decision it defers).
//
// 🪦 `fauna.state.user-config` used to be this file's CRDT + door-less
// vehicle; its registration was RETIRED at E0 of the `__config` dissolution
// schedule (2026-08-12 — `fauna_protocol::merge_policy` carries the tombstone;
// the string is retired-never-reuse). The door-less and poisoned-row shapes it
// carried now drive the seen-set, and the gate refusal drives the
// device-endpoints kind.

fn moderation_item() -> ItemId {
    ItemId {
        kind: KIND_MODERATION.into(),
        key: MODERATION_KEY.into(),
    }
}

/// A `ModerationConfig` carrying one muted keyword — the delegable exemplar's
/// value shape.
fn moderation_with(keyword: &str) -> Vec<u8> {
    encode_canonical(&ModerationConfig {
        muted_keywords: vec![keyword.into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec()
}

/// The `LwwStamp` a latest-wins kind must carry — without one the writer door
/// refuses the write, since nothing would rank it at a reader.
fn stamp(at_ms: i64, writer: u8) -> Option<Vec<u8>> {
    Some(
        LwwStamp {
            at_ms,
            writer: writer_id(writer).0,
        }
        .encode()
        .unwrap(),
    )
}

async fn stored_moderation(store: &AccountStore<SqliteBackend>) -> ModerationConfig {
    let entry = store
        .state(KIND_MODERATION, MODERATION_KEY)
        .await
        .unwrap()
        .expect("an entry for the moderation item");
    fauna_protocol::decode_strict(&entry.value).unwrap()
}

/// The seen-set entry recording observations of one referenced scope — the
/// logical key IS that scope's name; the kind is not a singleton
/// (`fauna_protocol::merge_policy::KIND_SEEN_SET`).
fn seen_item(referenced_scope: &str) -> ItemId {
    ItemId {
        kind: KIND_SEEN_SET.into(),
        key: referenced_scope.into(),
    }
}

/// A `SeenScopeSet` built from `(writer_byte, seq)` pairs — the value shape of
/// the union-CRDT exemplar. The writer bytes here are coordinates of items in
/// the *referenced* scope, nothing to do with the two replicas' writer ids.
fn seen_with(watermarks: &[(u8, u64)], refs: &[(u8, u64)]) -> Vec<u8> {
    let mut s = SeenScopeSet::new();
    for (writer, seq) in watermarks {
        s.raise_watermark([*writer; 32], *seq);
    }
    for (writer, seq) in refs {
        s.insert_ref([*writer; 32], *seq);
    }
    encode_canonical(&s).unwrap().to_vec()
}

async fn stored_seen(store: &AccountStore<SqliteBackend>, referenced_scope: &str) -> Vec<u8> {
    store
        .state(KIND_SEEN_SET, referenced_scope)
        .await
        .unwrap()
        .expect("an entry for the seen-set item")
        .value
}

/// A device's endpoint-registry entry — keyed by the *publishing device's*
/// writer id, so each device owns exactly one row
/// (`fauna_protocol::merge_policy::KIND_DEVICE_ENDPOINTS`).
fn device_endpoints_item(writer: &WriterId) -> ItemId {
    ItemId {
        kind: KIND_DEVICE_ENDPOINTS.into(),
        key: hex::encode(writer.0),
    }
}

/// A `DeviceEndpoints` value for the device at `writer` — the W2.5 item-3
/// exemplar's value shape.
fn endpoints_of(writer: u8) -> Vec<u8> {
    encode_canonical(&DeviceEndpoints {
        node_id: writer_id(writer).0,
        lan_addrs: vec![format!("192.168.1.{writer}:4433")],
        public_addrs: vec![format!("203.0.113.{writer}:4433")],
        relay_url: None,
    })
    .unwrap()
    .to_vec()
}

/// Stage a row door-lessly: straight onto the local journal, then sealed and
/// sent by `publish_pending` — exactly the shape a hostile or non-conforming device
/// takes. The door guards what a replica *originates*; this path is how
/// the tests below put on the feed what the door would refuse (a fleet-only
/// row under the R14 gate, a tombstone on a CRDT kind).
///
/// Publishes on the plane serving the **entry's own scope**, so a door-less
/// row still lands on its kind's home scope. That is not cosmetic since step
/// 7: the walk now skips a row riding a scope its kind never seals into (the
/// A5 partition's read side), so staging a fleet-only kind onto `state` would
/// exercise that skip rather than whatever the test means to assert.
async fn stage_doorless(
    store: &AccountStore<SqliteBackend>,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    device: &SigningKey,
    entry: StateEntry,
) {
    let fleet = entry.scope == ACCOUNT_STATE_FLEET_SCOPE;
    store.put_state(entry).await.unwrap();
    let p = if fleet {
        plane_fleet(store, rpc, keys, device)
    } else {
        plane(store, rpc, keys, device)
    };
    p.publish_pending().await.unwrap();
}

// ── The tests ────────────────────────────────────────────────────────────────

/// The whole target flow in one test: A writes, B walks, B holds A's value —
/// and B's frontier now accounts A's writer.
#[tokio::test]
async fn a_write_on_one_replica_reaches_the_other_through_the_feed() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    let seq = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &moderation_item(),
            moderation_with("spoilers"),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    assert_eq!(seq, 1, "the first local row is writer seq 1");

    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report,
        WalkReport {
            pages: 1,
            rows: 1,
            applied: 1,
            ..Default::default()
        },
        "B had nothing for the item, so it adopts A's value verbatim"
    );
    assert_eq!(
        stored_moderation(&b)
            .await
            .muted_keywords
            .iter()
            .map(|k| k.keyword.clone())
            .collect::<Vec<_>>(),
        vec!["spoilers".to_string()]
    );
    assert_eq!(
        b.frontier(ACCOUNT_STATE_SCOPE).await.unwrap(),
        vec![(writer_id(0x0A), 1)],
        "only an accounted walk advances the frontier, and it accounted exactly A's row"
    );
}

/// The R13 signature, end to end over the real feed: B opens A's entry only
/// because A's *device key* signed it. This is the whole reason a delegable
/// grant is safe to hand out — the AEAD key alone confers reading, never
/// authoring (`account-data-plane.md` § The class-2 entry form, R13).
///
/// The negative half is unit-tested where a forgery can actually be built
/// (`fauna_core::account_entry_crypto`'s grant-holder test); here the assertion
/// is that a correctly signed entry survives the whole nest round trip, which
/// is what would break if the signature were dropped anywhere along the wire.
#[tokio::test]
async fn an_entry_crossing_the_feed_carries_its_writer_signature() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &moderation_item(),
            moderation_with("spoilers"),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    assert_eq!(
        plane(&b, &rpc, &keys, &signing_key(0x0B))
            .walk()
            .await
            .unwrap()
            .applied,
        1,
        "a signed entry opens at the peer"
    );

    // A plane whose signing key is not its store's writer is refused at
    // construction — every entry it sealed would fail at every reader, so the
    // mismatch must not be discoverable one write at a time.
    assert!(
        AccountStatePlane::new(
            &a,
            &rpc,
            &keys,
            &signing_key(0x0B),
            &TRUST,
            ACCOUNT_STATE_SCOPE
        )
        .is_err()
    );
}

/// The R14 admission at the writer door, in its step-6 shape: a
/// `GenerationTip` origination is refused **only because no admissible,
/// escrow-acked tip resolves** — that one check is escrow-before-first-seal
/// and the R14 gate in one (charter § The generation machinery → *The
/// sealing-epoch axis, and the gate's real shape*), and it lifts per replica
/// the moment a mint's escrow receipt lands in merged state (the step-6
/// section at the end of this file drives the lifted flow).
///
/// Asserted here rather than only in a unit test because the door is the thing
/// a feature author actually calls — and because the refusal must land *before*
/// any durable local state, or the replica keeps a row it can never publish.
#[tokio::test]
async fn the_writer_door_refuses_a_generation_tip_kind_while_no_tip_resolves() {
    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    // The device-endpoint registry (W2.5 item 3) is the door's exemplar —
    // there the refusal is the *removal severance* the location data wants,
    // not an obstacle (see `KIND_DEVICE_ENDPOINTS`'s registration): a stolen
    // device holding gen-0 keys must not read the fleet's future addresses.
    let err = plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(1, 0x0A),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("no candidate generation tip resolves for this device"),
        "expected the no-tip refusal, got: {err}"
    );

    // Nothing durable, nothing on the feed.
    assert!(
        a.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        a.frontier(ACCOUNT_STATE_FLEET_SCOPE)
            .await
            .unwrap()
            .is_empty()
    );

    // And the delegable exemplars are unaffected — the admission is per
    // epoch, not a freeze of the whole plane.
    assert!(
        plane(&a, &rpc, &keys, &signing_key(0x0A))
            .put(
                &moderation_item(),
                moderation_with("spoilers"),
                stamp(1, 0x0A)
            )
            .await
            .is_ok()
    );
}

/// The A5 partition at the door: every class-2 kind seals into exactly one
/// home scope (delegable → `state`, fleet-only → `state-fleet`), and the door
/// refuses an origination anywhere else — a fleet kind's churn must never
/// ride the scope a delegable grantee subscribes to (charter § The generation
/// machinery, the partition bullet).
#[tokio::test]
async fn the_writer_door_refuses_a_kind_outside_its_home_scope() {
    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    // A fleet-only kind through the delegable-scope plane…
    let err = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(1, 0x0A),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("seals into scope"),
        "expected the home-scope refusal, got: {err}"
    );

    // …and a delegable kind through the fleet-scope plane both refuse.
    let err = plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &moderation_item(),
            moderation_with("spoilers"),
            stamp(1, 0x0A),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("seals into scope"),
        "expected the home-scope refusal, got: {err}"
    );

    // Neither refusal left durable state behind.
    assert!(a.frontier(ACCOUNT_STATE_SCOPE).await.unwrap().is_empty());
    assert!(
        a.frontier(ACCOUNT_STATE_FLEET_SCOPE)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The stamp door, pinned at the writer: a `LatestWins` write carrying no
/// `LwwStamp` is refused before anything lands — no reading replica could
/// rank it, so publishing it would lose the write silently at every peer.
///
/// Owed since the plane's first `LatestWins` kind registered (the refusal at
/// `account_state_plane.rs` was structurally unreachable while the only
/// registered kind was CRDT). Every later `LatestWins` registration — the
/// config-dissolution schedule adds several — inherits this pin for free.
#[tokio::test]
async fn the_writer_door_refuses_a_stampless_latest_wins_write() {
    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    let err = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&moderation_item(), moderation_with("spoilers"), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("carries no merge_meta"),
        "expected the stamp door to refuse, got: {err}"
    );

    // Nothing durable, nothing on the feed — the same all-or-nothing contract
    // the R14 gate's refusal keeps (a row a replica can never publish must not
    // rest locally either).
    assert!(
        a.state(KIND_MODERATION, MODERATION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert!(a.frontier(ACCOUNT_STATE_SCOPE).await.unwrap().is_empty());

    // The stampless *tombstone* meets the same door: a deletion is ordered
    // only by its stamp, so without one it could never rank at a reader.
    let err = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .tombstone(&moderation_item(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("carries no merge_meta"),
        "expected the stamp door to refuse the stampless tombstone, got: {err}"
    );
}

/// The stamped tombstone, end to end: a `LatestWins` deletion crosses the
/// real feed and applies at a peer that had already adopted the value — the
/// round trip a review finding declared owed with the
/// first `LatestWins` registration. This also exercises the wire's
/// cleartext-vs-seal tombstone cross-check (`op` must agree with the sealed
/// marker) on the path a production deletion takes.
#[tokio::test]
async fn a_stamped_tombstone_round_trips_and_deletes_at_the_peer() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &moderation_item(),
            moderation_with("spoilers"),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        stored_moderation(&b)
            .await
            .muted_keywords
            .iter()
            .map(|k| k.keyword.clone())
            .collect::<Vec<_>>(),
        vec!["spoilers".to_string()],
        "B adopted the value before the deletion"
    );

    // A deletes, stamped to outrank the value it supersedes.
    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .tombstone(&moderation_item(), stamp(200, 0x0A))
        .await
        .unwrap();

    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.applied, 1,
        "the tombstone is an applied row, never a skip: {report:?}"
    );
    let entry = b
        .state(KIND_MODERATION, MODERATION_KEY)
        .await
        .unwrap()
        .expect("the tombstone row is HELD — it persists until every frontier passes it");
    assert!(
        entry.tombstone,
        "B's current entry for the item is the deletion, ranked above the value"
    );
}

// The pre-step-6 `device_endpoint_rows_apply_per_device_over_the_feed` staged
// its fleet rows door-lessly under the boolean R14 gate; the step-6 section at
// the end of this file carries its claim forward (per-device keys never
// collide) through the REAL door under a resolved tip — see
// `generation_tip_rows_seal_v2_and_apply_per_device_over_the_feed`.

/// The union-CRDT claim, end to end **through the writer door**: two replicas
/// record *different* observations of the same scope while neither has seen
/// the other, and after one exchange each way both hold the union. A
/// latest-wins fallback would have dropped one side's observations.
///
/// Until the seen-set registered (W2.5 item 2), the only CRDT kind was
/// fleet-only and R14-gated, so this test staged its rows door-lessly; it now
/// drives `put` exactly like a production caller — item 0's decision (e),
/// discharged.
#[tokio::test]
async fn concurrent_observations_merge_to_the_union_through_the_door() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&seen_item("scope-x"), seen_with(&[], &[(1, 1)]), None)
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .put(&seen_item("scope-x"), seen_with(&[], &[(2, 5)]), None)
        .await
        .unwrap();

    // B sees A's row and has its own value to reconcile against: a merge, which
    // B re-publishes as its own authored value.
    let b_report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(b_report.merged, 1, "B merged rather than replacing");
    assert_eq!(
        stored_seen(&b, "scope-x").await,
        seen_with(&[], &[(1, 1), (2, 5)]),
        "B holds the union"
    );

    // A walks: it sees B's original row and B's merged row.
    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        stored_seen(&a, "scope-x").await,
        stored_seen(&b, "scope-x").await,
        "both replicas converged on the same value"
    );
}

/// Convergence has to be a fixed point, not a ping-pong: once both sides hold
/// the merged value, further walks must publish nothing. A merge that reported
/// `Merged` instead of `KeepCurrent` for a no-op would loop here forever.
/// (Driven through the writer door, like the union test above.)
#[tokio::test]
async fn a_converged_pair_stops_publishing() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&seen_item("scope-x"), seen_with(&[], &[(1, 1)]), None)
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .put(&seen_item("scope-x"), seen_with(&[], &[(2, 5)]), None)
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();

    // Now quiesce: every subsequent walk on either side must merge nothing.
    for _ in 0..3 {
        let a_report = plane(&a, &rpc, &keys, &signing_key(0x0A))
            .walk()
            .await
            .unwrap();
        let b_report = plane(&b, &rpc, &keys, &signing_key(0x0B))
            .walk()
            .await
            .unwrap();
        assert_eq!(
            a_report.merged, 0,
            "A re-published a merge after converging"
        );
        assert_eq!(
            b_report.merged, 0,
            "B re-published a merge after converging"
        );
        assert_eq!(a_report.applied, 0);
        assert_eq!(b_report.applied, 0);
    }
    assert_eq!(
        stored_seen(&a, "scope-x").await,
        stored_seen(&b, "scope-x").await
    );
}

/// A4 over the real feed: one replica holds the itemized form, the other
/// compacts the same prefix into a watermark (plus one observation of its
/// own), and after one exchange each way both hold the SAME canonical bytes —
/// smaller than the itemized encoding, with membership intact. This is the
/// property that makes the seen-set "grow-only as a set, compactable as
/// bytes" *convergently*: the elision lives inside the join, so a compacted
/// side never resurrects a peer's itemized copy.
#[tokio::test]
async fn a_watermark_compaction_converges_over_the_feed() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    let itemized = seen_with(&[], &[(1, 1), (1, 2), (1, 3), (1, 4)]);
    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&seen_item("scope-x"), itemized.clone(), None)
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .put(&seen_item("scope-x"), seen_with(&[(1, 4)], &[(2, 9)]), None)
        .await
        .unwrap();

    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();

    let converged = stored_seen(&a, "scope-x").await;
    assert_eq!(
        converged,
        stored_seen(&b, "scope-x").await,
        "identical canonical bytes on both replicas"
    );
    assert_eq!(
        converged,
        seen_with(&[(1, 4)], &[(2, 9)]),
        "the watermark absorbed every itemized ref"
    );
    assert!(
        converged.len() < itemized.len(),
        "compaction shrank the encoding ({} !< {})",
        converged.len(),
        itemized.len()
    );
    // And membership never shrank: the elided refs are still members.
    let got: SeenScopeSet = fauna_protocol::decode_strict(&converged).unwrap();
    for seq in 1..=4 {
        assert!(got.contains(&[1u8; 32], seq));
    }
}

/// Backstop 2 (§ Nudges and backstops): a zero-frontier read is the per-entry
/// full-state reconcile, and re-presenting rows a replica already applied must
/// be a no-op — not a churn of the entry table and not an equivocation refusal
/// (which is what a locally-derived `entry_version` on an ingested row would
/// have produced on the second pass).
#[tokio::test]
async fn a_full_state_reconcile_is_idempotent_over_rows_already_applied() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &moderation_item(),
            moderation_with("spoilers"),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    let after_walk = stored_moderation(&b).await;
    let version = b
        .state(KIND_MODERATION, MODERATION_KEY)
        .await
        .unwrap()
        .unwrap()
        .entry_version;

    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .reconcile()
        .await
        .unwrap();
    assert_eq!(
        report.rows, 1,
        "the live entry re-presents from a zero frontier"
    );
    assert_eq!(report.kept, 1, "and is recognized as already held");
    assert_eq!(report.applied, 0);
    assert_eq!(stored_moderation(&b).await, after_walk);
    assert_eq!(
        b.state(KIND_MODERATION, MODERATION_KEY)
            .await
            .unwrap()
            .unwrap()
            .entry_version,
        version,
        "the entry was not rewritten"
    );
}

/// A replica's own rows coming back off the feed are accounted, never
/// re-ingested — re-ingesting one would collide with the local row it already
/// holds at those coordinates.
#[tokio::test]
async fn a_replica_accounts_its_own_rows_without_re_ingesting_them() {
    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &moderation_item(),
            moderation_with("spoilers"),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    // `put` already accounted our own slot, so a plain walk sees nothing.
    let report = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    assert_eq!(report.rows, 0);

    // A zero-frontier reconcile does re-present it, and that is the self-echo
    // path.
    let report = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .reconcile()
        .await
        .unwrap();
    assert_eq!(
        report,
        WalkReport {
            pages: 1,
            rows: 1,
            self_echo: 1,
            ..Default::default()
        }
    );
}

/// A tombstone on a CRDT kind never reaches the feed: the WRITER door refuses
/// it, because the reader has no cheap answer left (`account-data-plane.md`
/// § Merge-policy seam — the nest collapses per `(item_key, writer)`, so a
/// published row its author never supersedes is permanent, and the writing
/// replica never notices, since it does not merge its own rows).
#[tokio::test]
async fn the_writer_door_refuses_a_tombstone_the_reader_could_not_merge() {
    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;
    let item = seen_item("scope-x");

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&item, seen_with(&[], &[(1, 1)]), None)
        .await
        .unwrap();

    let err = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .tombstone(&item, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("does not admit a tombstone"),
        "expected the writer-door refusal, got: {err}"
    );

    // Refused means refused *everywhere*: no local tombstone, nothing on the
    // feed, and therefore nothing for a peer to choke on.
    assert!(
        !a.state(KIND_SEEN_SET, "scope-x")
            .await
            .unwrap()
            .unwrap()
            .tombstone,
        "the refused write still landed on the local store"
    );
    let b = replica(0x0B).await;
    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(report.unmergeable, 0);
    assert_eq!(report.applied, 1, "the peer should see only the put");

    // And the door's checks come in policy-specific order: a stampless
    // tombstone on a *fleet-only* LWW kind is refused for the missing stamp,
    // NOT with the R14 refusal — the caller learns the reason that will still
    // apply once the generation schedule lands.
    let err = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .tombstone(&device_endpoints_item(&writer_id(0x0A)), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("carries no merge_meta"),
        "expected the stamp refusal ahead of the R14 gate, got: {err}"
    );
}

/// And if one reaches the feed anyway — a hostile or non-conforming device
/// — the walk **skips and counts it** rather than aborting.
///
/// The invariant decides this, not taste: the row is permanent (per-writer
/// collapse), so a fatal reading would starve this replica of every OTHER
/// writer's rows on every walk *and* every reconcile — a client-causable state
/// no client could recover from (`nest/common.md` § Client-state
/// recoverability). The poisoned row is staged by writing it straight to the
/// store and letting `publish_pending` seal and send it, which is exactly the
/// path a build without the door takes.
#[tokio::test]
async fn an_unmergeable_row_on_the_feed_is_skipped_not_fatal() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
    );
    let item = seen_item("scope-x");

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&item, seen_with(&[], &[(1, 1)]), None)
        .await
        .unwrap();

    // Replica C, door-less: the tombstone goes straight onto its journal —
    // the shape a hostile or non-conforming device takes.
    stage_doorless(
        &c,
        &rpc,
        &keys,
        &signing_key(0x0C),
        StateEntry {
            kind: item.kind.clone(),
            key: item.key.clone(),
            scope: ACCOUNT_STATE_SCOPE.into(),
            value: Vec::new(),
            merge_meta: None,
            entry_version: 0,
            tombstone: true,
        },
    )
    .await;

    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(report.unmergeable, 1, "the poisoned row was not counted");
    assert_eq!(
        report.applied, 1,
        "the OTHER writer's row must still land — starving it is the wedge"
    );

    // The frontier deliberately does not advance past a row we did not account,
    // so it re-presents — and keeps not wedging, walk after walk.
    let again = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(again.unmergeable, 1);
    assert_eq!(again.applied, 0, "the accounted row must not replay");

    // And the surviving value is A's, un-deleted.
    let row = b.state(KIND_SEEN_SET, "scope-x").await.unwrap().unwrap();
    assert!(!row.tombstone);
    assert_eq!(row.value, seen_with(&[], &[(1, 1)]));
}

/// The compat contract (§ Compat obligations 1): a row this build cannot open —
/// a newer writer's kind, or a per-kind grant this principal does not hold — is
/// skipped, never guessed at, and never wedges the walk.
#[tokio::test]
async fn a_row_this_build_cannot_open_is_skipped_rather_than_guessed() {
    let rpc = nest().await;
    let keys = schedule();
    let b = replica(0x0B).await;

    // Seal under a kind nobody registers: the reader's trial-open set cannot
    // produce its entry key, so the row is opaque to it — exactly a newer
    // writer's kind from this build's point of view.
    let unknown = "fauna.state.from-a-newer-build";
    assert_eq!(merge_policy(unknown), None);
    // Sealed on the delegable branch: a newer build's kind would be, and it is
    // the branch this reader *can* derive — so the row's opacity comes from the
    // unknown kind alone, not from a branch mismatch that would have hidden it
    // for the wrong reason.
    let sealed = fauna_core::account_entry_crypto::seal_entry(
        &keys.delegable().for_kind(unknown),
        &fauna_core::account_entry_crypto::EntryCoordinates {
            writer_id: writer_id(0x0A).0,
            writer_seq: 1,
            scope: ACCOUNT_STATE_SCOPE,
        },
        &fauna_core::account_entry_crypto::EntryPlaintext {
            kind: unknown.into(),
            key: "whatever".into(),
            merge_meta: None,
            value: b"opaque".to_vec().into(),
            tombstone: false,
        },
        &signing_key(0x0A),
    )
    .unwrap();
    let _: fauna_protocol::account_state::AccountStatePutReply = rpc
        .request(
            fauna_protocol::account_state::KIND_STATE_PUT,
            fauna_protocol::account_state::AccountStatePutRequest {
                scope: ACCOUNT_STATE_SCOPE.into(),
                writer_id: hex::encode(writer_id(0x0A).0),
                writer_seq: 1,
                item_key: sealed.item_key.to_vec().into(),
                op: fauna_protocol::account_state::OP_STATE_PUT.into(),
                entry: sealed.envelope.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report,
        WalkReport {
            pages: 1,
            rows: 1,
            unopened: 1,
            ..Default::default()
        },
        "the row is reported as unopened, not applied and not fatal"
    );
    // Unaccounted on purpose: the durable frontier records only rows this
    // replica holds, so the reconcile can come back to it after an upgrade.
    assert!(b.frontier(ACCOUNT_STATE_SCOPE).await.unwrap().is_empty());
    // And the walk terminated rather than re-serving the same page forever.
    let again = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(again.unopened, 1);
}

/// A local write whose publish never reached the nest is recoverable: the row
/// is durable locally and sits above the published high-water, so
/// `publish_pending` sends it.
#[tokio::test]
async fn a_local_row_that_was_never_published_is_re_sent() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    // Simulate the crash-after-local-write: write straight to the store,
    // bypassing the publish half.
    a.put_state(fauna_account_store::types::StateEntry {
        kind: KIND_SEEN_SET.into(),
        key: "scope-x".into(),
        scope: ACCOUNT_STATE_SCOPE.into(),
        value: seen_with(&[], &[(1, 1)]),
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    })
    .await
    .unwrap();
    assert_eq!(
        plane(&b, &rpc, &keys, &signing_key(0x0B))
            .walk()
            .await
            .unwrap()
            .rows,
        0,
        "nothing was published, so the peer sees nothing"
    );

    assert_eq!(
        plane(&a, &rpc, &keys, &signing_key(0x0A))
            .publish_pending()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        plane(&b, &rpc, &keys, &signing_key(0x0B))
            .walk()
            .await
            .unwrap()
            .applied,
        1
    );
    assert_eq!(stored_seen(&b, "scope-x").await, seen_with(&[], &[(1, 1)]));

    // Idempotent: a second pass has nothing above the high-water.
    assert_eq!(
        plane(&a, &rpc, &keys, &signing_key(0x0A))
            .publish_pending()
            .await
            .unwrap(),
        0
    );
}

/// A put that names the rows it covers (`delegable-scope-reclamation.md`
/// § Delegable-scope reclamation, part (2)), over the real handlers: two
/// writers hold rows of one item, the second re-puts naming the first's row,
/// and the nest supersedes it in the put's own transaction. A replica
/// walking from a zero frontier and one walking from a frontier banked below
/// the replaced row both end on the newer value and are served no replaced
/// row; the putter forgets its relay copy of the row it named, because the
/// reply said the nest read the list.
#[tokio::test]
async fn a_put_naming_the_row_it_covers_takes_it_off_the_feed_for_every_walker() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c, d) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
        replica(0x0D).await,
    );

    // A writes an unrelated row first; C banks its frontier past it, so
    // C's next walk starts below A's moderation row.
    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&seen_item("scope-x"), seen_with(&[], &[(1, 1)]), None)
        .await
        .unwrap();
    assert_eq!(
        plane(&c, &rpc, &keys, &signing_key(0x0C))
            .walk()
            .await
            .unwrap()
            .rows,
        1
    );

    plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(&moderation_item(), moderation_with("one"), stamp(100, 0x0A))
        .await
        .unwrap();

    // B walks both of A's rows, then re-puts the item naming A's row.
    let b_key = signing_key(0x0B);
    let pb = plane(&b, &rpc, &keys, &b_key);
    pb.walk().await.unwrap();
    let item_key = pb
        .gen0_item_key(KIND_MODERATION, MODERATION_KEY)
        .expect("moderation is a gen-0 kind");
    let covered = pb.relay_rows_at(&item_key).await.unwrap();
    assert_eq!(
        covered.iter().map(|r| r.writer).collect::<Vec<_>>(),
        vec![writer_id(0x0A)],
        "B holds A's row of the item, and only it"
    );
    pb.put_replacing(
        &moderation_item(),
        moderation_with("two"),
        stamp(200, 0x0B),
        &covered,
    )
    .await
    .unwrap();
    assert_eq!(
        pb.relay_rows_at(&item_key)
            .await
            .unwrap()
            .iter()
            .map(|r| r.writer)
            .collect::<Vec<_>>(),
        vec![writer_id(0x0B)],
        "the reply carried `replaced`, so B forgot its copy of the row it named"
    );

    // From a zero frontier: A's seen row and B's moderation row, nothing else.
    let fresh = plane(&d, &rpc, &keys, &signing_key(0x0D))
        .walk()
        .await
        .unwrap();
    assert_eq!(fresh.rows, 2, "the replaced row is not served: {fresh:?}");
    assert_eq!(
        stored_moderation(&d)
            .await
            .muted_keywords
            .iter()
            .map(|k| k.keyword.clone())
            .collect::<Vec<_>>(),
        vec!["two".to_string()]
    );

    // From the banked frontier: B's row alone.
    let banked = plane(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    assert_eq!(banked.rows, 1, "the replaced row is not served: {banked:?}");
    assert_eq!(
        stored_moderation(&c)
            .await
            .muted_keywords
            .iter()
            .map(|k| k.keyword.clone())
            .collect::<Vec<_>>(),
        vec!["two".to_string()]
    );
}

/// The seam's registration discipline, asserted where a drift would actually
/// bite: the kind the plane writes must have a policy, or `put` refuses.
#[tokio::test]
async fn writing_an_unregistered_kind_is_refused_before_anything_lands() {
    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    let err = plane(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: "fauna.state.never-registered".into(),
                key: "k".into(),
            },
            b"v".to_vec(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not on the class-2 plane"));
    assert!(a.frontier(ACCOUNT_STATE_SCOPE).await.unwrap().is_empty());
}

// ── The device-set kind: the R14 fleet-membership truth (build step 3) ───────
//
// `fauna.state.device-set` rows stage door-lessly like every fleet-only kind
// (the R14 gate refuses production seals until the schedule's tip-resolution
// step). What these tests pin is the plane-level half of the charter's
// device-set contract (`account-data-plane.md` § The generation machinery):
// the remove-wins lattice holding across the REAL feed + walk + store (the
// join-level laws live in `fauna_core::generation`), and the verified fleet
// view composing with real merged rows.

/// The account's identity line for the device-set tests — the key the
/// enrollment certs verify under. Distinct from the replicas' device signing
/// keys on purpose: a device key must never mint membership.
fn fleet_root() -> fauna_core::identity::ActorKeypair {
    fauna_core::identity::ActorKeypair::from_secret([0x77; 32])
}

/// The fleet root's escrow-target key — the string the target row sits at,
/// every wrap seals under and every receipt names.
fn tk() -> String {
    escrow_target_identity_key(&fleet_root().actor_id())
}

/// A valid enrollment value for `id` (a [`writer_id`]): the cert signed by
/// `signer` in the `EmbedAsBytes` carriage, self-signed by the device behind
/// `id` — the shape production writes ([`signed_enrollment_value`]).
fn enrollment_value(
    id: [u8; 32],
    signer: &fauna_core::identity::ActorKeypair,
    enrolled_at_ms: i64,
) -> Vec<u8> {
    let device = (0..=255u8)
        .map(signing_key)
        .find(|k| k.verifying_key().to_bytes() == id)
        .expect("a fixture id is always a writer_id");
    signed_enrollment_value(&device, signer, enrolled_at_ms)
}

fn enrollment_value_with_pubkey(
    id: [u8; 32],
    signer: &fauna_core::identity::ActorKeypair,
    enrolled_at_ms: i64,
    xwing_pubkey: Vec<u8>,
) -> Vec<u8> {
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    let cert = DeviceAuthorization {
        actor_id: signer.actor_id(),
        device_key: id,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(signer, &cert).unwrap();
    let authorization = fauna_core::encoding::canonical_encode(
        &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
    )
    .unwrap();
    // UNSIGNED: a cert re-filed beside `xwing_pubkey` by someone without
    // the device's key — adoptable, but never a member at the fleet view.
    // The tests that model a device's OWN write use
    // [`signed_enrollment_value`].
    fauna_core::encoding::canonical_encode(&fauna_core::generation::DeviceSetRecord::Enrolled {
        xwing_pubkey,
        authorization,
        enrolled_at_ms,
        device_sig: Vec::new(),
    })
    .unwrap()
}

/// A device's own enrollment as production writes it since the self-signed
/// enrollment ruling: root-signed cert, the KEM half derived from the device
/// secret, self-signed under the device id.
fn signed_enrollment_value(
    device: &SigningKey,
    signer: &fauna_core::identity::ActorKeypair,
    enrolled_at_ms: i64,
) -> Vec<u8> {
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    let cert = DeviceAuthorization {
        actor_id: signer.actor_id(),
        device_key: device.verifying_key().to_bytes(),
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(signer, &cert).unwrap();
    let authorization = fauna_core::encoding::canonical_encode(
        &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
    )
    .unwrap();
    fauna_core::encoding::canonical_encode(&fauna_core::generation::sign_device_enrollment(
        device,
        authorization,
        enrolled_at_ms,
    ))
    .unwrap()
}

fn removal_value(removed_by: [u8; 32]) -> Vec<u8> {
    fauna_core::encoding::canonical_encode(&fauna_core::generation::DeviceSetRecord::Removed {
        removed_at_ms: 9_000,
        removed_by,
    })
    .unwrap()
}

/// A device-set row on its **home** scope (`state-fleet`, the A5 partition —
/// re-staged from the pre-partition `state` shape in step 7, when the walk
/// gained the read-side home-scope skip).
fn device_set_entry(id: &[u8; 32], value: Vec<u8>) -> StateEntry {
    StateEntry {
        kind: fauna_protocol::merge_policy::KIND_DEVICE_SET.into(),
        key: hex::encode(id),
        scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
        value,
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    }
}

async fn stored_device_set(
    store: &AccountStore<SqliteBackend>,
    id: &[u8; 32],
) -> fauna_core::generation::DeviceSetRecord {
    let entry = store
        .state(
            fauna_protocol::merge_policy::KIND_DEVICE_SET,
            &hex::encode(id),
        )
        .await
        .unwrap()
        .expect("a device-set row for the id");
    fauna_core::encoding::canonical_decode(&entry.value).unwrap()
}

/// The contingency check at the plane level: a removal crosses the real
/// feed, absorbs on every replica, and a later re-enrollment of the SAME id —
/// valid cert, fresh bytes, staged the way a hostile or confused fleet-key
/// holder would — resurrects nothing, including on the replica that staged
/// it, once it walks the others' rows.
///
/// Red-verified by mutation: inverting `two_phase_winner`'s absorbing arm in
/// `fauna_core::generation` makes every post-removal assertion here fail.
#[tokio::test]
async fn a_device_set_removal_absorbs_across_the_feed_and_never_resurrects() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
    );
    let root = fleet_root();
    let x = writer_id(0x0A).0; // device A enrolls its own id — the production shape

    // A enrolls X; B walks and holds the enrollment.
    stage_doorless(
        &a,
        &rpc,
        &keys,
        &signing_key(0x0A),
        device_set_entry(&x, enrollment_value(x, &root, 5_000)),
    )
    .await;
    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert!(matches!(
        stored_device_set(&b, &x).await,
        fauna_core::generation::DeviceSetRecord::Enrolled { .. }
    ));

    // B removes X. A walks — its own enrollment is absorbed.
    stage_doorless(
        &b,
        &rpc,
        &keys,
        &signing_key(0x0B),
        device_set_entry(&x, removal_value(writer_id(0x0B).0)),
    )
    .await;
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    assert!(matches!(
        stored_device_set(&a, &x).await,
        fauna_core::generation::DeviceSetRecord::Removed { .. }
    ));

    // The resurrection attempt: C stages a fresh, VALIDLY-CERTIFIED
    // enrollment of the same id (put_state bypasses the merge — exactly what
    // a hostile device's store would hold) and publishes it.
    stage_doorless(
        &c,
        &rpc,
        &keys,
        &signing_key(0x0C),
        device_set_entry(&x, enrollment_value(x, &root, 999_000)),
    )
    .await;

    // A and B walk C's row: Removed absorbs — stamps, certs and arrival
    // order all irrelevant.
    for (store, device) in [(&a, 0x0A), (&b, 0x0B)] {
        plane_fleet(store, &rpc, &keys, &signing_key(device))
            .walk()
            .await
            .unwrap();
        assert!(
            matches!(
                stored_device_set(store, &x).await,
                fauna_core::generation::DeviceSetRecord::Removed { .. }
            ),
            "replica {device:#x} resurrected a removed device"
        );
    }
    // And C, walking the others' rows, converges back to Removed itself.
    plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    assert!(matches!(
        stored_device_set(&c, &x).await,
        fauna_core::generation::DeviceSetRecord::Removed { .. }
    ));
}

/// The verified fleet view over REAL merged rows: membership requires the
/// root-signed cert (a self-signed enrollment verifies as nothing), and a
/// removal excludes — the reader-side authority half of the charter's
/// device-set contract, composed with the actual store.
#[tokio::test]
async fn the_fleet_view_over_merged_rows_admits_only_root_authorized_enrollments() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
    );
    let root = fleet_root();
    let honest = writer_id(0x0A).0;
    let rogue = writer_id(0x0C).0;

    // A enrolls with the root's cert; C enrolls itself with a cert it signed
    // with its own key — "a device cannot authorize a device".
    stage_doorless(
        &a,
        &rpc,
        &keys,
        &signing_key(0x0A),
        device_set_entry(&honest, enrollment_value(honest, &root, 5_000)),
    )
    .await;
    let self_signer = fauna_core::identity::ActorKeypair::from_secret([0x0C; 32]);
    stage_doorless(
        &c,
        &rpc,
        &keys,
        &signing_key(0x0C),
        device_set_entry(&rogue, enrollment_value(rogue, &self_signer, 5_000)),
    )
    .await;

    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    let rows = b
        .states_of_kind(fauna_protocol::merge_policy::KIND_DEVICE_SET)
        .await
        .unwrap();
    let view = fauna_core::generation::FleetView::build(
        &root.actor_id(),
        rows.iter().map(|e| (e.key.as_str(), e.value.as_slice())),
    );
    assert!(view.is_verified_member(&honest));
    assert!(
        !view.is_verified_member(&rogue),
        "a self-signed enrollment must never verify into membership"
    );
    assert_eq!(
        view.invalid().len(),
        1,
        "the rogue row is flagged, not silent"
    );
    assert_eq!(
        view.wrap_targets().map(|m| m.device_id).collect::<Vec<_>>(),
        vec![honest]
    );

    // B removes the honest device; the view over the re-walked rows excludes
    // it — unconditionally, whoever the claimed remover is.
    stage_doorless(
        &b,
        &rpc,
        &keys,
        &signing_key(0x0B),
        device_set_entry(&honest, removal_value(writer_id(0x0B).0)),
    )
    .await;
    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    let rows = b
        .states_of_kind(fauna_protocol::merge_policy::KIND_DEVICE_SET)
        .await
        .unwrap();
    let view = fauna_core::generation::FleetView::build(
        &root.actor_id(),
        rows.iter().map(|e| (e.key.as_str(), e.value.as_slice())),
    );
    assert!(view.is_excluded(&honest));
    assert!(!view.is_verified_member(&honest));
    assert!(view.wrap_targets().next().is_none());
}

/// **The pin against the real plane.** A `BackupKey` holder — C
/// here, sealing fleet-only rows exactly as a removed device with a retained
/// key would — publishes a second `Enrolled` row for A's id under its OWN
/// writer: A's valid root-signed cert copied verbatim, C's own X-Wing key in
/// place of A's, bytes that win the byte-order max. Before the self-signed
/// enrollment ruling that row was B's merged row for A, and every wrap B
/// healed or minted for A sealed the generation key to C. Now A's own signed
/// row is the merged row at every replica — at B, which meets both, and at A,
/// which meets C's as an incoming against its own — and the wrap target the
/// top-up pass reads (`ensure_topped_up` consumes `wrap_targets()` directly)
/// is the KEM A's own secret derives.
#[tokio::test]
async fn a_backup_key_holder_cannot_redirect_a_members_wrap_target() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
    );
    let root = fleet_root();
    let victim = writer_id(0x0A).0;
    let victim_kem = derive_device_xwing_keypair(&[0x0A; 32])
        .public
        .to_bytes()
        .to_vec();
    let attacker_kem = derive_device_xwing_keypair(&[0x0C; 32])
        .public
        .to_bytes()
        .to_vec();

    // A enrolls itself — signed. C files the forgery at A's id: A's cert, C's
    // key, no signature (C holds no A secret to sign with).
    let honest = signed_enrollment_value(&signing_key(0x0A), &root, 5_000);
    let forged = enrollment_value_with_pubkey(victim, &root, 5_000, attacker_kem.clone());
    assert_ne!(honest, forged);
    stage_doorless(
        &a,
        &rpc,
        &keys,
        &signing_key(0x0A),
        device_set_entry(&victim, honest.clone()),
    )
    .await;
    stage_doorless(
        &c,
        &rpc,
        &keys,
        &signing_key(0x0C),
        device_set_entry(&victim, forged.clone()),
    )
    .await;

    // B walks both rows; A walks C's against its own.
    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();

    for (name, replica) in [("B", &b), ("A", &a)] {
        let merged = stored_device_set(replica, &victim).await;
        assert!(
            merged.self_verifies_at(&victim),
            "{name}: the merged row at A's cell is A's own signed enrollment"
        );
        let rows = replica
            .states_of_kind(fauna_protocol::merge_policy::KIND_DEVICE_SET)
            .await
            .unwrap();
        let view = fauna_core::generation::FleetView::build(
            &root.actor_id(),
            rows.iter().map(|e| (e.key.as_str(), e.value.as_slice())),
        );
        assert!(view.invalid().is_empty(), "{name}: {:?}", view.invalid());
        let targets: Vec<_> = view.wrap_targets().collect();
        assert_eq!(targets.len(), 1, "{name}");
        assert_eq!(targets[0].device_id, victim);
        assert_eq!(
            targets[0].xwing_pubkey, victim_kem,
            "{name}: the wrap target every heal and mint seals to is A's own KEM, never C's"
        );
        assert_ne!(targets[0].xwing_pubkey, attacker_kem);
    }
}

/// The first-contact contract at the plane level (`validate_adoptable`): a
/// row whose own content no merge could handle — junk bytes under a CRDT
/// kind, a stampless row under a stamped kind — is skipped at ADOPTION, never
/// adopted-then-kept. Without this the walk's row-content skips make arrival
/// order decide truth (the replica that met the poison first would keep it
/// forever); with it, every replica refuses identically and the walk still
/// lands every other row.
#[tokio::test]
async fn a_poisoned_first_contact_row_is_skipped_at_adoption_not_adopted() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
    );
    let root = fleet_root();
    let victim = [0x5A; 32]; // an id nobody enrolled — the front-run target
    let honest = writer_id(0x0A).0;

    // C front-runs the victim key with junk…
    stage_doorless(
        &c,
        &rpc,
        &keys,
        &signing_key(0x0C),
        device_set_entry(&victim, b"not-a-device-set-record".to_vec()),
    )
    .await;
    // …and puts a stampless row under a stamped fleet-only kind on the feed
    // for good measure — sealed BY HAND exactly as a hostile fleet-key holder
    // would (form v1, gen-0 keys): the plane's own door refuses a stampless
    // `LatestWins` write, but any `BackupKey` holder can seal one directly,
    // which is who this test plays. The kind is the generation-wrap top-up
    // (`LatestWins`, fleet-only, `Gen0`) rather than device-endpoints, because
    // since step 7 a v1 envelope of a `GenerationTip` kind does not open at
    // all — it is not a *row-content* poisoning any more but a form refusal,
    // asserted on its own below.
    {
        use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
        use fauna_protocol::account_state::{
            AccountStatePutReply, AccountStatePutRequest, KIND_STATE_PUT, OP_STATE_PUT,
        };
        let kind_keys =
            fauna_protocol::merge_policy::kind_keys(&keys, KIND_GENERATION_WRAP).unwrap();
        let sealed = seal_entry(
            &kind_keys,
            &EntryCoordinates {
                writer_id: writer_id(0x0C).0,
                writer_seq: 2,
                scope: ACCOUNT_STATE_FLEET_SCOPE,
            },
            &EntryPlaintext {
                kind: KIND_GENERATION_WRAP.into(),
                key: format!("{}/{}", "00".repeat(32), hex::encode(writer_id(0x0C).0)),
                merge_meta: None, // stampless — nothing could ever rank against it
                value: b"whatever".to_vec().into(),
                tombstone: false,
            },
            &signing_key(0x0C),
        )
        .unwrap();
        let _: AccountStatePutReply = rpc
            .request(
                KIND_STATE_PUT,
                AccountStatePutRequest {
                    scope: ACCOUNT_STATE_FLEET_SCOPE.to_string(),
                    writer_id: hex::encode(writer_id(0x0C).0),
                    writer_seq: 2,
                    item_key: sealed.item_key.to_vec().into(),
                    op: OP_STATE_PUT.to_string(),
                    entry: sealed.envelope.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    // A stages a valid enrollment of its own id.
    stage_doorless(
        &a,
        &rpc,
        &keys,
        &signing_key(0x0A),
        device_set_entry(&honest, enrollment_value(honest, &root, 5_000)),
    )
    .await;

    let report = plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.unmergeable, 2,
        "both poisoned rows counted, not fatal"
    );
    assert_eq!(report.applied, 1, "the valid row must still land");
    assert!(
        b.state(
            fauna_protocol::merge_policy::KIND_DEVICE_SET,
            &hex::encode(victim)
        )
        .await
        .unwrap()
        .is_none(),
        "junk was adopted into merged state"
    );
    assert!(matches!(
        stored_device_set(&b, &honest).await,
        fauna_core::generation::DeviceSetRecord::Enrolled { .. }
    ));

    // Walk again: the skips repeat harmlessly, the accounted row does not.
    let again = plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(again.unmergeable, 2);
    assert_eq!(again.applied, 0);
}

/// The registrations every assertion above rests on, pinned in one place — both
/// columns, because the file's whole shape (which kind each test drives, and why
/// three are needed) follows from them.
#[test]
fn the_exemplar_kinds_are_registered_as_this_file_assumes() {
    // The CRDT exemplar: union merge, delegable, sealable through the door —
    // which is why the merge tests drive `put` rather than staging.
    assert_eq!(merge_policy(KIND_SEEN_SET), Some(MergePolicy::CrdtPerField));
    assert_eq!(audience_rung(KIND_SEEN_SET), Some(AudienceRung::Delegable));

    // The delegable LWW exemplar: latest-wins, grant-mintable, sealable today.
    assert_eq!(merge_policy(KIND_MODERATION), Some(MergePolicy::LatestWins));
    assert_eq!(
        audience_rung(KIND_MODERATION),
        Some(AudienceRung::Delegable)
    );

    // The fleet-only exemplar: `GenerationTip` at the writer door — which is
    // why the step-6 tests must resolve a tip before it seals.
    assert_eq!(
        merge_policy(KIND_DEVICE_ENDPOINTS),
        Some(MergePolicy::LatestWins)
    );
    assert_eq!(
        audience_rung(KIND_DEVICE_ENDPOINTS),
        Some(AudienceRung::FleetOnly)
    );
    assert_eq!(
        fauna_protocol::merge_policy::sealing_epoch(KIND_DEVICE_ENDPOINTS),
        Some(fauna_core::crypto::SealingEpoch::GenerationTip)
    );

    // The retired whole-record kind stays retired (E0, the dissolution
    // schedule) — `fauna_protocol::merge_policy` pins this too; asserted here
    // because this file used to drive it and must never quietly go back.
    assert_eq!(merge_policy("fauna.state.user-config"), None);

    // And the stamp encoding the latest-wins kinds use round-trips.
    let s = LwwStamp {
        at_ms: 42,
        writer: writer_id(0x0A).0,
    };
    assert_eq!(LwwStamp::decode(&s.encode().unwrap()).unwrap(), s);
}

/// The W8.3 custody registry pair holds the R14 gate exactly like its
/// tip-sealed sibling (`device-endpoints`): the writer door refuses each
/// while no generation tip resolves — for location data the refusal IS the
/// removal severance — and the refusal lands before any durable local state.
///
/// Deliberately NOT here: a door-less staged convergence leg. A
/// `GenerationTip` kind cannot ride `stage_doorless` (the publish seal
/// itself demands a resolved tip — this test's first version proved that
/// the hard way), and the convergence-under-a-real-generation proof is the
/// step-6 flow below, which for these kinds belongs to the ceremony/
/// custodian tier_3s (W8.4/W8.5) where their production writers land — the
/// LWW walk machinery is kind-generic and already proven on the sibling.
#[tokio::test]
async fn the_custody_registry_kinds_hold_the_r14_gate_like_their_endpoints_sibling() {
    use fauna_core::custodian_endpoints::CustodianEndpoints;
    use fauna_core::custodies_held::CustodyHeld;
    use fauna_core::custody_grant::custody_entry_key;
    use fauna_core::device_endpoints::DeviceEndpoints;
    use fauna_protocol::merge_policy::{KIND_CUSTODIAN_ENDPOINTS, KIND_CUSTODIES_HELD};

    let rpc = nest().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    let grant_id = vec![0x1Du8; 16];
    let key = custody_entry_key(&grant_id);
    let held = encode_canonical(&CustodyHeld {
        grant_id: grant_id.clone(),
        owner: [0xAB; 32],
        witness: vec![0xEE; 40],
        owner_devices: vec![DeviceEndpoints {
            node_id: [7u8; 32],
            lan_addrs: vec!["192.168.1.7:4433".into()],
            public_addrs: Vec::new(),
            relay_url: None,
        }],
        owner_nest_url: None,
        retained_bytes_cap: 42,
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    let custodian = encode_canonical(&CustodianEndpoints {
        grant_id: grant_id.clone(),
        endpoints: DeviceEndpoints {
            node_id: [0xC5; 32],
            lan_addrs: Vec::new(),
            public_addrs: vec!["198.51.100.7:4433".into()],
            relay_url: None,
        },
        // Struct-update, not an exhaustive literal: the next field this wire
        // type grows must not break this fixture, and two branches growing it
        // independently should merge cleanly.
        ..Default::default()
    })
    .unwrap()
    .to_vec();

    // The door: tip-sealed, so origination refuses while no tip resolves —
    // the same R14 gate the endpoints sibling pins — and the refusal
    // precedes any durable state.
    for (kind, value) in [
        (KIND_CUSTODIES_HELD, held),
        (KIND_CUSTODIAN_ENDPOINTS, custodian),
    ] {
        let err = plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
            .put(
                &ItemId {
                    kind: kind.into(),
                    key: key.clone(),
                },
                value,
                stamp(1, 0x0A),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("no candidate generation tip resolves for this device"),
            "{kind}: expected the no-tip refusal, got: {err}"
        );
        assert!(a.state(kind, &key).await.unwrap().is_none());
    }
    assert!(
        a.frontier(ACCOUNT_STATE_FLEET_SCOPE)
            .await
            .unwrap()
            .is_empty()
    );
}

// ── Build step 6: tip resolution at the writer door, form v2 end to end ──────
//
// The production sequence, driven through the REAL door on the fleet scope
// (charter § The generation machinery): devices enroll (device-set, Gen0
// machinery — admitted with no tip in sight, which is the bootstrap
// stratification working), a seed-holding surface publishes the escrow
// target, the minter builds the mint, a trusted holder receipts the escrow
// wrap (holder-GENERIC here on purpose: the receipt is signed by a plain test
// key, never the deployment identity — the tier_3 real-door chain lives in
// `generation_escrow.rs`), the machinery rows land through the door, and only
// then does a `GenerationTip` kind seal — form v2, under the resolved tip.

/// This device's real enrollment: root-signed cert + the X-Wing device-KEM
/// pubkey production derives from its Ed25519 secret (`signing_key(d)`'s
/// secret is `[d; 32]`), written through the door by the device itself.
fn real_enrollment_value(device: u8, enrolled_at_ms: i64) -> Vec<u8> {
    signed_enrollment_value(&signing_key(device), &fleet_root(), enrolled_at_ms)
}

async fn enroll_through_door(
    store: &AccountStore<SqliteBackend>,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    device: u8,
) {
    plane_fleet(store, rpc, keys, &signing_key(device))
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(writer_id(device).0),
            },
            real_enrollment_value(device, 5_000),
            None,
        )
        .await
        .unwrap();
}

/// The member view a minter would read off merged rows — reconstructed
/// directly here since the derivations are deterministic.
fn fleet_member(device: u8) -> FleetMember {
    FleetMember {
        device_id: writer_id(device).0,
        xwing_pubkey: derive_device_xwing_keypair(&[device; 32])
            .public
            .to_bytes()
            .to_vec(),
        enrolled_at_ms: 5_000,
    }
}

/// One completed mint, holder-generically receipted, all three machinery rows
/// (escrow target, mint, receipt) written through the door by `minter` —
/// the production sequence minus the live escrow doors. Returns the
/// generation id and the minted key (the minter's own key source).
async fn mint_through_door(
    store: &AccountStore<SqliteBackend>,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    minter: u8,
    members: &[u8],
) -> ([u8; 32], fauna_core::crypto::GenerationKey) {
    let fleet: Vec<FleetMember> = members.iter().map(|&d| fleet_member(d)).collect();
    let target = escrow_target_record(&[0x77; 32]); // fleet_root()'s seed
    let built = build_mint(
        &fleet,
        &target,
        &tk(),
        Vec::new(),
        &signing_key(minter),
        6_000,
    )
    .unwrap();
    let receipt = sign_escrow_receipt(
        &escrow_holder(),
        built.generation_id,
        blake3::hash(&built.escrow_wrap).into(),
        &tk(),
        6_500,
    );
    let minter_key = signing_key(minter);
    let p = plane_fleet(store, rpc, keys, &minter_key);
    p.put(
        &ItemId {
            kind: KIND_ESCROW_TARGET.into(),
            key: tk(),
        },
        fauna_core::encoding::canonical_encode(&target).unwrap(),
        None,
    )
    .await
    .unwrap();
    p.put(
        &ItemId {
            kind: KIND_GENERATION_MINT.into(),
            key: fauna_core::hex32::encode(&built.generation_id),
        },
        fauna_core::encoding::canonical_encode(&built.record).unwrap(),
        None,
    )
    .await
    .unwrap();
    p.put(
        &ItemId {
            kind: KIND_ESCROW_RECEIPT.into(),
            key: format!(
                "{}/{}",
                fauna_core::hex32::encode(&built.generation_id),
                fauna_core::hex32::encode(&receipt.holder_id)
            ),
        },
        fauna_core::encoding::canonical_encode(&receipt).unwrap(),
        None,
    )
    .await
    .unwrap();
    (built.generation_id, built.gen_key)
}

/// The old `device_endpoint_rows_apply_per_device_over_the_feed` claim —
/// per-device keys never collide, both rows land at both replicas — carried
/// through the REAL door under a resolved tip, plus the two step-6 pins:
/// machinery kinds originate through the door with no tip in sight (`Gen0`
/// admission), and the endpoint rows on the wire are **form v2 naming the
/// resolved generation** (the feed peek — a reader opening them does not pin
/// the form by itself, since gen-0-sealed v1 rows also open).
#[tokio::test]
async fn generation_tip_rows_seal_v2_and_apply_per_device_over_the_feed() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    // Each device enrolls itself; A walks so its merged view holds both.
    enroll_through_door(&a, &rpc, &keys, 0x0A).await;
    enroll_through_door(&b, &rpc, &keys, 0x0B).await;
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();

    // A mints for the two-member fleet — machinery rows through the door,
    // pre-tip, which is the bootstrap stratification working.
    let (generation, _gen_key) = mint_through_door(&a, &rpc, &keys, 0x0A, &[0x0A, 0x0B]).await;

    // A's endpoint row now seals — v2, under the resolved tip, opened from
    // A's own inline member wrap.
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();

    // B walks: A's enrollment + three machinery rows + the endpoint row, all
    // applied (B had nothing for any of them). Opening the endpoint row is
    // B's inline wrap + commitment check + per-generation schedule at work.
    let report = plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        (report.applied, report.merged, report.unopened),
        (5, 0, 0),
        "B applies A's five rows, merges nothing, opens everything: {report:?}"
    );

    // B's own endpoint row seals the same way…
    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .put(
            &device_endpoints_item(&writer_id(0x0B)),
            endpoints_of(0x0B),
            stamp(100, 0x0B),
        )
        .await
        .unwrap();
    // …and A walks it back: per-device keys, applied not merged.
    let report = plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    assert_eq!(report.merged, 0, "per-device keys never merge: {report:?}");

    for store in [&a, &b] {
        for device in [0x0A, 0x0B] {
            let row = store
                .state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(device).0))
                .await
                .unwrap()
                .expect("the device's endpoint row");
            let got: DeviceEndpoints = fauna_protocol::decode_strict(&row.value).unwrap();
            assert_eq!(got.node_id, writer_id(device).0);
            assert_eq!(got.lan_addrs, vec![format!("192.168.1.{device}:4433")]);
        }
    }

    // The form pin, read off the wire itself: every endpoint row on the fleet
    // feed is a v2 envelope naming the resolved generation cleartext.
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                since: 0,
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                frontier: Some(std::collections::BTreeMap::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let endpoint_generations: Vec<_> = reply
        .changes
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .filter_map(|envelope| peek_generation_id(envelope))
        .collect();
    assert_eq!(
        endpoint_generations,
        vec![generation, generation],
        "exactly the two endpoint rows are generation-sealed, under the resolved tip"
    );
}

/// A replica no wrap reaches yet skips v2 rows as unopened — and the
/// full-state reconcile re-presents them once a top-up lands (charter: "a
/// device enrolled moments before a mint that missed its enrollment record …
/// is topped up on first contact"; backstop 2 is the re-presenter).
#[tokio::test]
async fn an_unkeyed_replica_skips_v2_rows_until_a_topup_lands() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, c) = (replica(0x0A).await, replica(0x0C).await);

    enroll_through_door(&a, &rpc, &keys, 0x0A).await;
    let (generation, gen_key) = mint_through_door(&a, &rpc, &keys, 0x0A, &[0x0A]).await;
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();

    // C — enrolled after the mint, in no member set, no top-up yet — walks:
    // the machinery applies, the v2 row is unopened, the walk continues.
    enroll_through_door(&c, &rpc, &keys, 0x0C).await;
    let report = plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        (report.applied, report.unopened),
        (4, 1),
        "machinery lands, the generation-sealed row waits for a wrap: {report:?}"
    );
    assert!(
        c.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
            .await
            .unwrap()
            .is_none()
    );

    // A key-holding device writes C's top-up through the door, into its own
    // per-healer cell — the self-healing path for the mint that raced C's enrollment.
    let topup = build_topup_wrap_v2(
        &gen_key,
        &generation,
        &fleet_member(0x0C),
        &signing_key(0x0A),
        200,
    )
    .unwrap();
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: KIND_GENERATION_WRAP.into(),
                key: fauna_core::generation::wrap_cell_key_per_healer(
                    &generation,
                    &writer_id(0x0C).0,
                    &writer_id(0x0A).0,
                ),
            },
            fauna_core::encoding::canonical_encode(&topup).unwrap(),
            stamp(200, 0x0A),
        )
        .await
        .unwrap();

    // C walks the top-up in, then reconciles: the once-unopened row opens.
    plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    let report = plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .reconcile()
        .await
        .unwrap();
    assert_eq!(
        report.unopened, 0,
        "the top-up unlocked the row: {report:?}"
    );
    let row = c
        .state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
        .await
        .unwrap()
        .expect("A's endpoint row, opened via the top-up wrap");
    let got: DeviceEndpoints = fauna_protocol::decode_strict(&row.value).unwrap();
    assert_eq!(got.node_id, writer_id(0x0A).0);
}

/// A recording custody double for the W5.4a carriage seams — the
/// `RetainedKeyCustody` contract without the credential-store graph (the
/// slot's own persistence is pinned tier_1 in `principal_bundle`).
#[derive(Default)]
struct TestCustody(std::sync::Mutex<std::collections::BTreeMap<[u8; 32], [u8; 32]>>);

impl fauna_sync_engine::generation_tip::RetainedKeyCustody for TestCustody {
    fn retained_generation_key(
        &self,
        generation: &[u8; 32],
    ) -> Option<fauna_core::crypto::GenerationKey> {
        self.0
            .lock()
            .unwrap()
            .get(generation)
            .map(|b| fauna_core::crypto::GenerationKey::from_bytes(*b))
    }

    fn record_generation_key(
        &self,
        generation: &[u8; 32],
        key: &fauna_core::crypto::GenerationKey,
    ) {
        self.0.lock().unwrap().insert(*generation, *key.as_bytes());
    }

    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.0.lock().unwrap().remove(generation);
    }
}

/// W5.4a — the retained bundle (T10 carriage) at every key seam of the walk:
/// a key obtained at seal or unwrap is **recorded**; a replica no wrap
/// reaches opens rows through a bundle-carried key with no top-up in sight
/// (the carriage window the plane cannot bridge); and a shred **drops** the
/// retained key at both the walk and the read — the crypto-shred contract's
/// device-side half ("deleting a generation = devices drop it"), without
/// which every slot that ever carried the key would quietly defeat the
/// user's generation delete.
#[tokio::test]
async fn the_retained_bundle_bridges_the_unkeyed_window_and_drops_on_shred() {
    use fauna_sync_engine::generation_tip::{self, RetainedKeyCustody};
    let rpc = nest().await;
    let keys = schedule();
    let (a, b, c) = (
        replica(0x0A).await,
        replica(0x0B).await,
        replica(0x0C).await,
    );

    enroll_through_door(&a, &rpc, &keys, 0x0A).await;
    enroll_through_door(&b, &rpc, &keys, 0x0B).await;
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    let (generation, gen_key) = mint_through_door(&a, &rpc, &keys, 0x0A, &[0x0A, 0x0B]).await;

    // Seal-side record: A's put resolves the tip's key from its inline wrap
    // and the custody retains it.
    let a_custody = TestCustody::default();
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .with_generation_custody(&a_custody)
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    assert!(
        a_custody.retained_generation_key(&generation).is_some(),
        "the key obtained at seal time rides the bundle"
    );

    // Read-side record: B (a mint member) walks, opens A's row from its own
    // inline wrap, and the custody retains what the unwrap produced.
    let b_custody = TestCustody::default();
    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .with_generation_custody(&b_custody)
        .walk()
        .await
        .unwrap();
    assert_eq!(
        b_custody
            .retained_generation_key(&generation)
            .expect("the key obtained at unwrap time rides the bundle")
            .as_bytes(),
        gen_key.as_bytes(),
    );

    // The carriage window: C — enrolled, in no member set, no top-up — is
    // exactly the unkeyed replica of the sibling test, but its bundle
    // carried the key (a store re-sync on a machine whose slot survived).
    // The plane answers "no wrap"; the bundle answers instead.
    enroll_through_door(&c, &rpc, &keys, 0x0C).await;
    let report = plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.unopened, 1,
        "no custody, no wrap: unopened: {report:?}"
    );
    let c_custody = TestCustody::default();
    c_custody.record_generation_key(&generation, &gen_key);
    let report = plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .with_generation_custody(&c_custody)
        .reconcile()
        .await
        .unwrap();
    assert_eq!(
        report.unopened, 0,
        "the bundle-carried key opens the row with no top-up: {report:?}"
    );
    let row = c
        .state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
        .await
        .unwrap()
        .expect("A's endpoint row, opened via the retained bundle");
    let got: DeviceEndpoints = fauna_protocol::decode_strict(&row.value).unwrap();
    assert_eq!(got.node_id, writer_id(0x0A).0);

    // The shred: A absorbs the mint to `Shredded` through the door (the
    // in-value absorbing state — never a tombstone).
    let mint_row = a
        .state(
            KIND_GENERATION_MINT,
            &fauna_core::hex32::encode(&generation),
        )
        .await
        .unwrap()
        .expect("the live mint row");
    let fauna_core::generation::GenerationMintRecord::Minted { core, .. } =
        fauna_core::encoding::canonical_decode(&mint_row.value).unwrap()
    else {
        panic!("the mint is live before the shred");
    };
    // Authored by A, a verified member — an unauthored shred drops no key
    // (`account-data-taxonomy.md` § *Fleet-scope reclamation* → *the
    // authored shred*; the plane's own tier_1 tests pin that half).
    let shred = fauna_core::generation::sign_shred(&signing_key(0x0A), core, 9_000).unwrap();
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: KIND_GENERATION_MINT.into(),
                key: fauna_core::hex32::encode(&generation),
            },
            fauna_core::encoding::canonical_encode(&shred).unwrap(),
            None,
        )
        .await
        .unwrap();

    // Walk-side drop: B walks the shred in and its custody drops the key the
    // moment the absorbed row lands.
    plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .with_generation_custody(&b_custody)
        .walk()
        .await
        .unwrap();
    assert!(
        b_custody.retained_generation_key(&generation).is_none(),
        "the walk observes the shred and drops the retained key"
    );

    // Read-side drop: C merged the shred without custody attached, so its
    // bundle still holds the key — the read looks at the mint row FIRST
    // (never blind-cache-first), refuses, and drops.
    plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    let view = fauna_sync_engine::fleet_removal::fleet_view(&c, &TRUST)
        .await
        .unwrap();
    let opened = generation_tip::generation_key_for(
        &c,
        &generation,
        &signing_key(0x0C),
        Some(&c_custody),
        Some(&view),
    )
    .await
    .unwrap();
    assert!(
        opened.is_none(),
        "a shredded generation keys nothing, bundle or not"
    );
    assert!(
        c_custody.retained_generation_key(&generation).is_none(),
        "the read observes the shred and drops the retained key"
    );
}

/// The seal-side half of the same gap, under the ST-007 candidacy predicate:
/// a mint whose wraps do not reach this device is **no candidate here**, so
/// the door refuses up front (or first-need-mints where an escrow door is
/// reachable — not in this fixture, whose router serves no escrow kinds), and
/// per the door's contract a refused `GenerationTip` write leaves **no
/// durable local row** — the pre-fix "admit under a tip you cannot key, hold
/// the row, fail at publish" path was exactly the seam ST-007's wedge lived
/// in. The top-up remains the heal: once it lands, the same put seals under
/// the now-keyable tip.
#[tokio::test]
async fn sealing_without_a_wrap_refuses_at_the_door_then_heals_on_topup() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, c) = (replica(0x0A).await, replica(0x0C).await);

    enroll_through_door(&a, &rpc, &keys, 0x0A).await;
    let (generation, gen_key) = mint_through_door(&a, &rpc, &keys, 0x0A, &[0x0A]).await;

    // C learns the fleet + machinery, then tries to seal its own endpoints.
    enroll_through_door(&c, &rpc, &keys, 0x0C).await;
    plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    let err = plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .put(
            &device_endpoints_item(&writer_id(0x0C)),
            endpoints_of(0x0C),
            stamp(100, 0x0C),
        )
        .await
        .unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.contains("no candidate generation tip resolves for this device")
            && text.contains("1 unreachable from this device"),
        "the refusal counts the unreachable mint and names the top-up fix, got: {text}"
    );
    // The door's contract: a refused `GenerationTip` write leaves no durable
    // local row (the pre-fix held-row state is retired with the seam).
    assert!(
        c.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0C).0))
            .await
            .unwrap()
            .is_none()
    );

    // A tops C up; the same put now seals under the now-keyable tip.
    let topup = build_topup_wrap_v2(
        &gen_key,
        &generation,
        &fleet_member(0x0C),
        &signing_key(0x0A),
        200,
    )
    .unwrap();
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: KIND_GENERATION_WRAP.into(),
                key: fauna_core::generation::wrap_cell_key_per_healer(
                    &generation,
                    &writer_id(0x0C).0,
                    &writer_id(0x0A).0,
                ),
            },
            fauna_core::encoding::canonical_encode(&topup).unwrap(),
            stamp(200, 0x0A),
        )
        .await
        .unwrap();
    plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .walk()
        .await
        .unwrap();
    plane_fleet(&c, &rpc, &keys, &signing_key(0x0C))
        .put(
            &device_endpoints_item(&writer_id(0x0C)),
            endpoints_of(0x0C),
            stamp(100, 0x0C),
        )
        .await
        .expect("the top-up made the tip keyable, so the same put seals");

    // And it lands at A — the healed row crossed the real feed.
    plane_fleet(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    assert!(
        a.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0C).0))
            .await
            .unwrap()
            .is_some()
    );
}

/// **Step 7 took the decision step 6 recorded: the v1 trial chain is
/// restricted to `Gen0` kinds, so a v1-sealed `GenerationTip` row is
/// `unopened` at every reader.**
///
/// Step 6 pinned the opposite (a v1 fleet row still opened) as a deliberate
/// compat carve-out, on the worry that flipping it would strand rows a
/// pre-step-6 build had published. It strands none, and the check is not a
/// judgement call: the boolean R14 gate (W2.5 item 0) refused
/// **every** fleet-only origination, and it landed *before* the only
/// `GenerationTip` kind was registered at all (W2.5 item 3) —
/// so the set of v1 `GenerationTip` rows any build of this project could ever
/// have written is empty, and the carve-out protected nothing.
///
/// What it cost is this test's real subject. R14 severs a removed device by
/// **wrap targeting**, and a removed device keeps `BackupKey` forever (the
/// charter's stated exposure) — so while v1 opened for a `GenerationTip`
/// kind, that device could go on sealing endpoint rows under the gen-0 fleet
/// branch and every reader would accept them: severance enforced at the
/// writer's door and nowhere else. Refusing the form is what makes the
/// generation axis load-bearing on the READ side too, and it is exactly what
/// the charter already says — "v1 stays the gen-0 form forever".
#[tokio::test]
async fn a_v1_sealed_generation_tip_row_is_unopened_at_readers() {
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
    use fauna_protocol::account_state::{
        AccountStatePutReply, AccountStatePutRequest, KIND_STATE_PUT, OP_STATE_PUT,
    };

    let rpc = nest().await;
    let keys = schedule();
    let b = replica(0x0B).await;

    // Seal the row exactly as a pre-step-6 build did: form v1, gen-0 fleet
    // branch, then the raw put a `publish_pending` would have sent.
    let kind_keys = fauna_protocol::merge_policy::kind_keys(&keys, KIND_DEVICE_ENDPOINTS).unwrap();
    let plaintext = EntryPlaintext {
        kind: KIND_DEVICE_ENDPOINTS.into(),
        key: hex::encode(writer_id(0x0A).0),
        merge_meta: stamp(100, 0x0A).map(Into::into),
        value: endpoints_of(0x0A).into(),
        tombstone: false,
    };
    let sealed = seal_entry(
        &kind_keys,
        &EntryCoordinates {
            writer_id: writer_id(0x0A).0,
            writer_seq: 1,
            scope: ACCOUNT_STATE_FLEET_SCOPE,
        },
        &plaintext,
        &signing_key(0x0A),
    )
    .unwrap();
    let _: AccountStatePutReply = rpc
        .request(
            KIND_STATE_PUT,
            AccountStatePutRequest {
                scope: ACCOUNT_STATE_FLEET_SCOPE.to_string(),
                writer_id: hex::encode(writer_id(0x0A).0),
                writer_seq: 1,
                item_key: sealed.item_key.to_vec().into(),
                op: OP_STATE_PUT.to_string(),
                entry: sealed.envelope.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let report = plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        (report.applied, report.unopened),
        (0, 1),
        "a v1-sealed `GenerationTip` row must not open — v1 is the gen-0 form: {report:?}"
    );
    assert!(
        b.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
            .await
            .unwrap()
            .is_none(),
        "nothing from a v1 `GenerationTip` row may reach merged state"
    );
}

// ── Build step 7: the first PRODUCTION consumer ───────────────────────────────
//
// Step 6 proved the door and the form with a hand-built mint and a plain test
// key for the holder. What "production consumer" adds is the two halves that
// were still stubbed at the seams:
//
//   * the mint runs through `fauna_sync_engine::generation_mint` — the engine's
//     own deposit-first sequence — against the REAL `fauna.generation.escrow.put`
//     handler, so the receipt that lifts the door is one the nest actually
//     signed with its deployment identity, not one the test minted; and
//   * the trust that accepts it is the deployment identity itself, which is
//     what `account_runtime` now plumbs in production from the app's TOFU pin
//     (`AccountRuntimeParams::trusted_escrow_holders`).
//
// With both real, `fauna.state.device-endpoints` — the schedule's proof kind —
// seals under generation 1 and crosses the feed. That un-gates production peer
// discovery (charter § The generation machinery → *First production consumer*).

/// The nest's deployment identity for the step-7 leg — the receipt signer, and
/// (via `TRUST_DEPLOYMENT`) the only holder this account accepts. Production
/// learns this key by TOFU pin, never from the nest's own claim.
fn deployment_key() -> SigningKey {
    SigningKey::from_bytes(&[0x66; 32])
}

/// The step-7 trust: same account identity line, but the trusted holder is the
/// **deployment identity**, exactly the production shape.
static TRUST_DEPLOYMENT: std::sync::LazyLock<fauna_sync_engine::generation_tip::GenerationTrust> =
    std::sync::LazyLock::new(|| fauna_sync_engine::generation_tip::GenerationTrust {
        root: fleet_root().actor_id(),
        prior: Vec::new(),
        trusted_holders: vec![deployment_key().verifying_key().to_bytes()].into(),
    });

fn plane_prod<'a>(
    store: &'a AccountStore<SqliteBackend>,
    rpc: &'a RouterRequester,
    keys: &'a AccountStateKeySchedule,
    device: &'a SigningKey,
) -> AccountStatePlane<'a, SqliteBackend, RouterRequester> {
    AccountStatePlane::new(
        store,
        rpc,
        keys,
        device,
        &TRUST_DEPLOYMENT,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap()
}

/// The same in-process nest as [`nest`], plus the generation-escrow doors and
/// the deployment identity that signs their receipts.
async fn nest_with_escrow() -> RouterRequester {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    state.nest_signing_key = Some(deployment_key());
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    RouterRequester {
        router: Arc::new(b.build()),
        state: Arc::new(state),
        calls: Default::default(),
        retires: Default::default(),
    }
}

/// Enroll `device` through the production-trust door (the step-6 helper's twin
/// — same row, different trust, since `Gen0` admission consults neither).
async fn enroll_prod(
    store: &AccountStore<SqliteBackend>,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    device: u8,
) {
    plane_prod(store, rpc, keys, &signing_key(device))
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(writer_id(device).0),
            },
            real_enrollment_value(device, 5_000),
            None,
        )
        .await
        .unwrap();
}

/// **The step-7 claim, end to end.** Two devices enroll; a seed-holding surface
/// publishes the escrow target; the engine mints generation 1 and deposits it
/// through the real escrow door, taking back the nest's own deployment-signed
/// receipt; the mint and receipt rows go on the feed through the door; and only
/// then does `fauna.state.device-endpoints` seal — form v2, under that
/// generation — and open at the peer.
///
/// The refusal *before* the receipt lands is asserted in the same flow rather
/// than a separate test, because the whole point is that one door check both
/// refuses and lifts: nothing about the endpoint write changes between the two
/// attempts except what merged state now contains.
#[tokio::test]
async fn device_endpoints_production_seals_under_generation_one_via_the_real_escrow_doors() {
    use fauna_sync_engine::generation_mint::{MintContext, escrow_target_entry, mint_generation};

    /// The identity seed the escrow target derives from — the seed-holding
    /// surface's, matching `fleet_root()`'s account.
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    // ── The fleet: both devices enroll themselves, A merges both.
    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    enroll_prod(&b, &rpc, &keys, 0x0B).await;
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();

    // ── Before any mint: the proof kind is refused, precisely.
    let refused = plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .expect_err("no tip resolves yet");
    assert!(
        format!("{refused:#}").contains("no candidate generation tip resolves for this device"),
        "the refusal must name escrow-before-first-seal, got: {refused:#}"
    );
    // And it lands at the DOOR, before anything durable — not later at seal
    // time, which produces the same message from the same resolver. The
    // difference is the whole of the door's contract: a replica that kept the
    // row would hold one it can never publish under a generation it will
    // never have (the tip it eventually resolves is a *different* one).
    assert!(
        a.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
            .await
            .unwrap()
            .is_none(),
        "the pre-mint refusal must leave no durable local row"
    );

    // ── The seed-holding surface publishes the escrow target through the door.
    let target = escrow_target_entry(&IDENTITY_SEED).unwrap();
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: tk(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();

    // ── The mint: the engine's deposit-first sequence against the REAL door.
    // The receipt comes back signed by the nest's deployment identity — the
    // holder this account trusts — so an unescrowed generation is not
    // representable as this call's output.
    let trusted = [deployment_key().verifying_key().to_bytes()];
    let root_id = fleet_root().actor_id();
    let minted = mint_generation(
        &a,
        &rpc,
        &MintContext {
            root: &root_id,
            minter_key: &signing_key(0x0A),
            trusted_holders: &trusted,
        },
        vec![],
        9_000,
    )
    .await
    .expect("the deposit-first sequence completes against the real escrow door");
    assert_eq!(
        minted.receipt.holder_id,
        deployment_key().verifying_key().to_bytes(),
        "v1 profile: the receipt names the deployment identity the client pins"
    );

    // The mint and its receipt go on the feed through the door — `Gen0`
    // machinery, admitted without resolving anything.
    let minter_key = signing_key(0x0A);
    let p = plane_prod(&a, &rpc, &keys, &minter_key);
    p.put(
        &ItemId {
            kind: KIND_GENERATION_MINT.into(),
            key: fauna_core::hex32::encode(&minted.generation_id),
        },
        minted.mint_entry.value.clone(),
        None,
    )
    .await
    .unwrap();
    p.put(
        &ItemId {
            kind: KIND_ESCROW_RECEIPT.into(),
            key: minted.receipt_entry.key.clone(),
        },
        minted.receipt_entry.value.clone(),
        None,
    )
    .await
    .unwrap();

    // ── The same write, now admitted: generation 1 seals the proof kind.
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .expect("the receipt lifted the door");

    // ── The peer opens it: its inline member wrap keys the generation, the
    // commitment verifies against the mint core, and the row applies.
    let report = plane_prod(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.unopened, 0,
        "every row A published must open at B: {report:?}"
    );
    let row = b
        .state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
        .await
        .unwrap()
        .expect("A's production-sealed endpoint row");
    let got: DeviceEndpoints = fauna_protocol::decode_strict(&row.value).unwrap();
    assert_eq!(got.node_id, writer_id(0x0A).0);

    // ── And the form, read off the wire: the endpoint row names the minted
    // generation in cleartext — the v2 envelope, under generation 1.
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                since: 0,
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                frontier: Some(std::collections::BTreeMap::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sealed_under: Vec<_> = reply
        .changes
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .filter_map(|envelope| peek_generation_id(envelope))
        .collect();
    assert_eq!(
        sealed_under,
        vec![minted.generation_id],
        "exactly the endpoint row is generation-sealed, under the minted generation"
    );
}

// ── Trigger (a): the first-need mint at the door ──────────────────────────────
//
// Step 7 proved the whole chain works when a session runs the mint by hand.
// Trigger (a) is what makes it happen on a real account: "a `GenerationTip`
// origination finds no admissible acked tip and the engine mints instead of
// refusing forever (provided an escrow target exists and a holder is reachable;
// otherwise the refusal stands and says why)" — charter § The generation
// machinery → *The mint protocol*.
//
// The door is the only place that sentence can be honoured, because the write
// that refuses is the write that must end up succeeding: a background pass that
// minted later would leave the user's origination already failed, and the door
// deliberately keeps no durable local row for a refused `GenerationTip` write.

/// The escrow target a seed-holding surface publishes — the one precondition
/// the door cannot supply for itself (it holds no identity seed).
async fn publish_escrow_target(
    store: &AccountStore<SqliteBackend>,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    device: u8,
    seed: &[u8; 32],
) {
    use fauna_sync_engine::generation_mint::escrow_target_entry;
    let target = escrow_target_entry(seed).unwrap();
    plane_prod(store, rpc, keys, &signing_key(device))
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: tk(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();
}

/// The single live `fauna.state.generation-mint` row's generation id — what the
/// door minted, read back the way any replica would.
async fn minted_generation_id(store: &AccountStore<SqliteBackend>) -> [u8; 32] {
    let rows: Vec<_> = store
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .collect();
    assert_eq!(rows.len(), 1, "exactly one generation was minted: {rows:?}");
    fauna_core::hex32::decode(&rows[0].key).expect("the mint row keys itself by generation id")
}

/// **Trigger (a), end to end.** A enrolls, a seed-holding surface publishes the
/// escrow target — and then A simply *writes the proof kind*. There is no
/// hand-run mint in this test: the door resolves no tip, mints one against the
/// real `fauna.generation.escrow.put` handler, publishes the mint and receipt
/// rows itself, and seals the origination under the generation it just minted.
/// B walks the feed and opens the row.
#[tokio::test]
async fn a_first_need_write_mints_its_own_generation_at_the_door() {
    /// `fleet_root()`'s seed — the identity the escrow target derives from.
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);

    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    enroll_prod(&b, &rpc, &keys, 0x0B).await;
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .walk()
        .await
        .unwrap();
    publish_escrow_target(&a, &rpc, &keys, 0x0A, &IDENTITY_SEED).await;

    // The whole claim: ONE put, no orchestration around it.
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .expect("the door mints instead of refusing forever");

    // The mint's own rows are on the plane, published by the door.
    let generation_id = minted_generation_id(&a).await;
    let receipts: Vec<_> = a
        .states_of_kind(KIND_ESCROW_RECEIPT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .collect();
    assert_eq!(
        receipts.len(),
        1,
        "the escrow receipt the door's own mint took back is on the plane: {receipts:?}"
    );

    // Both members were wrapped, so B keys the generation from the mint row
    // alone — the door minted against merged device-set state, not just itself.
    let report = plane_prod(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.unopened, 0,
        "every row the door published must open at B: {report:?}"
    );
    let row = b
        .state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
        .await
        .unwrap()
        .expect("A's endpoint row, sealed under the door-minted generation");
    let got: DeviceEndpoints = fauna_protocol::decode_strict(&row.value).unwrap();
    assert_eq!(got.node_id, writer_id(0x0A).0);

    // And on the wire: exactly the endpoint row is generation-sealed, under the
    // generation the door minted for it.
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                since: 0,
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                frontier: Some(std::collections::BTreeMap::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sealed_under: Vec<_> = reply
        .changes
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .filter_map(|envelope| peek_generation_id(envelope))
        .collect();
    assert_eq!(sealed_under, vec![generation_id]);
}

/// A **second** `GenerationTip` write does not mint again: the first mint's
/// receipt is merged state now, so the tip resolves and trigger (a) does not
/// fire. Without this the door would mint per write — a fork per origination.
#[tokio::test]
async fn the_door_mints_once_and_then_seals_under_the_tip_it_minted() {
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    publish_escrow_target(&a, &rpc, &keys, 0x0A, &IDENTITY_SEED).await;

    for seq in [100, 200] {
        plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
            .put(
                &device_endpoints_item(&writer_id(0x0A)),
                endpoints_of(0x0A),
                stamp(seq, 0x0A),
            )
            .await
            .unwrap();
    }

    let mints: Vec<_> = a
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .collect();
    assert_eq!(
        mints.len(),
        1,
        "the second write seals under the resolved tip; it must not mint a fork: {mints:?}"
    );
}

// ── The bounded mint (charter § The mint protocol → *The bounded mint*) ──────
//
// One inline wrap was ~2.4 KB as encoded then (~1.3 KB since its bytes ride
// as a CBOR byte string) and the plane caps a sealed entry at 64 KiB, so a
// mint wrapping every member stopped fitting past about 27 devices — and such a fleet could never mint again. The bounded mint lists
// every member, wraps `MAX_INLINE_MEMBER_WRAPS` inline (the minter among
// them) and tops the rest up from the minter's own key, in the same sequence,
// ahead of the escrow receipt. The test below is the definition of success:
// a 64-member fleet mints and every member keys the tip.

/// The 64-device fleet: seeds 1..=64 through `signing_key` /
/// `real_enrollment_value` (device `d`'s KEM key derives from `[d; 32]`,
/// exactly as production derives it from the writer secret).
const BIG_FLEET: std::ops::RangeInclusive<u8> = 1..=64;

/// Every wire entry of the fleet scope, paged to the end — the nest refused
/// anything over `MAX_STATE_ENTRY_BYTES` at its door, so re-measuring the
/// whole feed client-side is the belt behind that brace.
async fn fleet_feed(rpc: &RouterRequester) -> Vec<fauna_protocol::sync::SyncChange> {
    // Paged the way the real walk pages: `since` is the nest-writer slot
    // only, the device writers ride the `frontier` (W2.3 ruling (a)), so
    // each page advances every writer it showed and the loop ends on an
    // empty page.
    let mut all = Vec::new();
    let mut frontier: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    loop {
        let reply: SyncChangesListReply = rpc
            .request(
                "fauna.sync.changes.list",
                SyncChangesListRequest {
                    since: 0,
                    item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                    scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                    frontier: Some(frontier.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        if reply.changes.is_empty() {
            return all;
        }
        let mut advanced = false;
        for c in &reply.changes {
            let writer = c
                .origin_writer
                .clone()
                .expect("a state-entry row names its writer");
            let seq = c
                .origin_seq
                .expect("a state-entry row carries its origin seq");
            let slot = frontier.entry(writer).or_insert(0);
            if seq > *slot {
                *slot = seq;
                advanced = true;
            }
        }
        assert!(advanced, "the feed pages forward");
        all.extend(reply.changes);
    }
}

/// The largest-id member of `BIG_FLEET` outside `wraps` — certainly in the
/// spill, since the inline set is the minter plus the SMALLEST other ids.
fn a_spilled_member(wraps: &[fauna_core::generation::MemberWrap]) -> u8 {
    BIG_FLEET
        .filter(|d| !wraps.iter().any(|w| w.device_id == writer_id(*d).0))
        .max_by_key(|d| writer_id(*d).0)
        .expect("a 64-member fleet spills")
}

/// **The bounded mint, end to end.** 64
/// devices enroll, a seed-holding surface publishes the escrow target, and A
/// simply writes the proof kind. Before the bounded mint the door refused
/// here forever — a 64-member mint row could not seal under the cap. Now the
/// door mints a bounded generation against the real escrow door: every
/// member listed, the cap wrapped inline, the spill topped up by the minter
/// BEFORE the receipt on its own log, nothing on the wire over the cap, and
/// every one of the 64 resolves that generation as its tip from the merged
/// rows — inline or through the minter's top-up. A spilled member walks and
/// opens A's generation-sealed row in one pass.
#[tokio::test]
async fn a_first_need_write_over_a_64_member_fleet_mints_a_bounded_generation_every_member_keys() {
    use fauna_core::generation::{GenerationMintRecord, MAX_INLINE_MEMBER_WRAPS};
    use fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES;
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = replica(0x01).await;
    let minter = signing_key(0x01);

    // The fleet, through A's door: the enrollment is merged state whoever
    // wrote the row (Gen0 machinery, admitted with no tip in sight).
    {
        let p = plane_prod(&a, &rpc, &keys, &minter);
        for d in BIG_FLEET {
            p.put(
                &ItemId {
                    kind: KIND_DEVICE_SET.into(),
                    key: hex::encode(writer_id(d).0),
                },
                real_enrollment_value(d, 5_000),
                None,
            )
            .await
            .unwrap();
        }
    }
    publish_escrow_target(&a, &rpc, &keys, 0x01, &IDENTITY_SEED).await;

    // ONE put, no orchestration around it.
    plane_prod(&a, &rpc, &keys, &signing_key(0x01))
        .put(
            &device_endpoints_item(&writer_id(0x01)),
            endpoints_of(0x01),
            stamp(100, 0x01),
        )
        .await
        .expect("the door mints a bounded generation over 64 members instead of refusing forever");
    let generation_id = minted_generation_id(&a).await;

    // (1) The mint row is bounded: everyone listed, the cap inline, the
    // minter among them.
    let row = a
        .state(
            KIND_GENERATION_MINT,
            &fauna_core::hex32::encode(&generation_id),
        )
        .await
        .unwrap()
        .expect("the minted row");
    let GenerationMintRecord::Minted { core, wraps, .. } =
        fauna_core::encoding::canonical_decode(&row.value).unwrap()
    else {
        panic!("a fresh mint is Minted");
    };
    assert_eq!(core.member_ids.len(), 64, "every member is listed");
    assert_eq!(wraps.len(), MAX_INLINE_MEMBER_WRAPS, "the cap is inline");
    assert!(
        wraps.iter().any(|w| w.device_id == writer_id(0x01).0),
        "the minter is inline"
    );

    // (2) Log order: the mint row, then every spill top-up, then the receipt
    // LAST — an acked mint is one whose every wrap precedes it.
    let log = a
        .scope_rows(ACCOUNT_STATE_FLEET_SCOPE, &writer_id(0x01), 0, 10_000)
        .await
        .unwrap();
    let seqs_of = |kind: &str| -> Vec<u64> {
        log.iter()
            .filter_map(|r| match &r.item {
                fauna_account_store::types::ItemRef::StateKey { kind: k, .. } if k == kind => {
                    Some(r.seq)
                }
                _ => None,
            })
            .collect()
    };
    let mint_seqs = seqs_of(KIND_GENERATION_MINT);
    let wrap_seqs = seqs_of(KIND_GENERATION_WRAP);
    let receipt_seqs = seqs_of(KIND_ESCROW_RECEIPT);
    assert_eq!(mint_seqs.len(), 1, "one mint: {mint_seqs:?}");
    assert_eq!(receipt_seqs.len(), 1, "one receipt: {receipt_seqs:?}");
    assert_eq!(
        wrap_seqs.len(),
        64 - MAX_INLINE_MEMBER_WRAPS,
        "one per-healer cell per spilled member"
    );
    assert!(
        wrap_seqs
            .iter()
            .all(|s| *s > mint_seqs[0] && *s < receipt_seqs[0]),
        "every top-up sits between the mint row and the receipt on the minter's log: mint \
         {mint_seqs:?}, wraps {wrap_seqs:?}, receipt {receipt_seqs:?}"
    );

    // (3) Nothing on the wire is over the cap — the nest enforces it at the
    // door; re-measured here over the whole feed.
    let feed = fleet_feed(&rpc).await;
    let over: Vec<usize> = feed
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .map(|e| e.len())
        .filter(|n| *n > MAX_STATE_ENTRY_BYTES)
        .collect();
    assert!(
        over.is_empty(),
        "entries over the cap reached the nest: {over:?}"
    );
    let expected_rows = 64 + 1 + 1 + (64 - MAX_INLINE_MEMBER_WRAPS) + 1 + 1;
    assert!(
        feed.len() >= expected_rows,
        "enrollments + target + mint + top-ups + receipt + the proof row ({expected_rows}): {}",
        feed.len()
    );

    // (4) Every member resolves the bounded mint as its tip from a walker's
    // merged rows — inline for the inline set, by the minter's top-up for
    // the spill. Keyability is plane-native (no bundle): exactly the
    // question a spilled device's own writer door asks.
    let w = replica(0xEE).await;
    plane_prod(&w, &rpc, &keys, &signing_key(0xEE))
        .walk()
        .await
        .unwrap();
    for d in BIG_FLEET {
        let r = fauna_sync_engine::generation_tip::resolve_tip(
            &w,
            &TRUST_DEPLOYMENT,
            &signing_key(d),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            r.tip.as_ref().map(|t| t.generation_id),
            Some(generation_id),
            "device {d:#04x} keys the bounded mint — unkeyable {:?}, invalid {:?}",
            r.unkeyable,
            r.invalid
        );
    }

    // (5) A spilled member walks and opens A's generation-sealed row in ONE
    // pass: its top-up precedes the receipt, which precedes the row.
    let spilled = a_spilled_member(&wraps);
    let b = replica(spilled).await;
    let report = plane_prod(&b, &rpc, &keys, &signing_key(spilled))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.unopened, 0,
        "every row A published opens at a spilled member: {report:?}"
    );
    let got: DeviceEndpoints = fauna_protocol::decode_strict(
        &b.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x01).0))
            .await
            .unwrap()
            .expect("A's endpoint row, sealed under the bounded generation")
            .value,
    )
    .unwrap();
    assert_eq!(got.node_id, writer_id(0x01).0);
}

/// A **removal** unseats the tip — and the next origination heals with a
/// covering mint (the candidate-aware first-need, ratified with the ST-007
/// fix: "no candidate resolves for this observer" IS first-need, whatever
/// rows exist — the old refuse-while-any-mint-row-exists guard was itself a
/// wedge). The heal keeps what the old pin defended: the covering mint names
/// the superseded tip as its parent (the resolver's leaf set), so severance
/// gets its supersession edge rather than a parentless second root — and the
/// removed device is out of the new member set and its wraps.
#[tokio::test]
async fn a_removal_that_unseats_the_tip_heals_with_a_covering_mint_at_the_next_write() {
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = replica(0x0A).await;

    // Generation 1, minted by the door over a two-member fleet.
    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(writer_id(0x0B).0),
            },
            real_enrollment_value(0x0B, 5_000),
            None,
        )
        .await
        .unwrap();
    publish_escrow_target(&a, &rpc, &keys, 0x0A, &IDENTITY_SEED).await;
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .unwrap();
    let first = minted_generation_id(&a).await;

    // B is removed: the tip names it as a member, so the tip goes inadmissible.
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(writer_id(0x0B).0),
            },
            removal_value(writer_id(0x0A).0),
            None,
        )
        .await
        .unwrap();

    // The next write heals: a covering mint, then the seal proceeds.
    plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(200, 0x0A),
        )
        .await
        .expect("the unseated tip heals at the next origination");

    let ids = live_mint_ids(&a).await;
    assert_eq!(ids.len(), 2, "gen 1 + the covering mint: {ids:?}");
    let covering = ids.iter().copied().find(|id| *id != first).unwrap();
    let row = a
        .state(KIND_GENERATION_MINT, &fauna_core::hex32::encode(&covering))
        .await
        .unwrap()
        .unwrap();
    let record: fauna_core::generation::GenerationMintRecord =
        fauna_core::encoding::canonical_decode(&row.value).unwrap();
    let fauna_core::generation::GenerationMintRecord::Minted { core, wraps, .. } = record else {
        panic!("the covering mint is Minted");
    };
    assert_eq!(
        core.parents,
        vec![first],
        "the covering mint names the superseded tip as its parent — the \
         supersession edge severance rides on"
    );
    assert_eq!(
        core.member_ids,
        vec![writer_id(0x0A).0],
        "the removed device is out of the member set"
    );
    assert_eq!(wraps.len(), 1, "and gets no wrap");
    assert_eq!(wraps[0].device_id, writer_id(0x0A).0);
}

/// No escrow target published → the write still refuses, and the refusal
/// **says why**: it names the missing target, not just "no tip resolves".
/// Charter: "an account with no reachable escrow holder cannot complete a first
/// mint — stated, not hidden".
#[tokio::test]
async fn a_first_need_write_without_an_escrow_target_refuses_and_says_why() {
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = replica(0x0A).await;
    enroll_prod(&a, &rpc, &keys, 0x0A).await;

    let refused = plane_prod(&a, &rpc, &keys, &signing_key(0x0A))
        .put(
            &device_endpoints_item(&writer_id(0x0A)),
            endpoints_of(0x0A),
            stamp(100, 0x0A),
        )
        .await
        .expect_err("no escrow target, so the first-need mint cannot complete");
    let text = format!("{refused:#}");
    assert!(
        text.contains(KIND_ESCROW_TARGET),
        "the refusal must name the missing escrow target, got: {text}"
    );

    // The door's contract is unchanged by the mint attempt: nothing durable.
    assert!(
        a.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
            .await
            .unwrap()
            .is_none(),
        "a refused `GenerationTip` write leaves no durable local row"
    );
    assert!(
        a.states_of_kind(KIND_GENERATION_MINT)
            .await
            .unwrap()
            .is_empty(),
        "a failed mint sequence stages nothing on the plane"
    );
}

/// No holder pinned → the deposit's receipt verifies for integrity but is not
/// *trusted*, so the mint refuses and the write refuses with it. This is the
/// honest posture `AccountRuntimeParams::trusted_escrow_holders` documents for
/// an app with no TOFU pin yet: fail-safe, and loud about which half failed.
#[tokio::test]
async fn a_first_need_write_with_no_trusted_holder_refuses_and_says_why() {
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];
    static TRUST_NO_HOLDER: std::sync::LazyLock<
        fauna_sync_engine::generation_tip::GenerationTrust,
    > = std::sync::LazyLock::new(|| fauna_sync_engine::generation_tip::GenerationTrust {
        root: fleet_root().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    });

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = replica(0x0A).await;
    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    publish_escrow_target(&a, &rpc, &keys, 0x0A, &IDENTITY_SEED).await;

    let device = signing_key(0x0A);
    let refused = AccountStatePlane::new(
        &a,
        &rpc,
        &keys,
        &device,
        &TRUST_NO_HOLDER,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap()
    .put(
        &device_endpoints_item(&writer_id(0x0A)),
        endpoints_of(0x0A),
        stamp(100, 0x0A),
    )
    .await
    .expect_err("no holder is trusted, so no receipt can lift the door");
    let text = format!("{refused:#}");
    assert!(
        text.contains("holder"),
        "the refusal must name the holder-trust half, got: {text}"
    );
    assert!(
        a.state(KIND_DEVICE_ENDPOINTS, &hex::encode(writer_id(0x0A).0))
            .await
            .unwrap()
            .is_none(),
        "a refused `GenerationTip` write leaves no durable local row"
    );
}

/// The A5 partition's **read** side (step 7): a fleet-only row riding the
/// delegable `state` scope is skipped, not applied.
///
/// The writer door has refused this origination since step 6, so an honest
/// build cannot produce one — which is exactly why the reader needs its own
/// answer. The author set is the usual one: any `BackupKey` holder, a removed
/// device included, can seal a row and put it on whichever scope it likes, and
/// without this check a fleet-only kind's merged state would be reachable over
/// the very scope the partition promises a delegable grantee can subscribe to
/// without seeing fleet churn.
///
/// Skipped rather than fatal, for the same invariant as every other poisoned
/// row: the nest collapses per `(item_key, writer)`, so aborting here would
/// starve this replica of every other writer's rows forever.
#[tokio::test]
async fn a_fleet_only_row_riding_the_delegable_scope_is_skipped() {
    let rpc = nest().await;
    let keys = schedule();
    let (a, b) = (replica(0x0A).await, replica(0x0B).await);
    let root = fleet_root();
    let x = writer_id(0x0A).0;

    // The same enrollment row the fleet tests use, published onto `state`
    // instead of its `state-fleet` home.
    let mut off_partition = device_set_entry(&x, enrollment_value(x, &root, 5_000));
    off_partition.scope = ACCOUNT_STATE_SCOPE.into();
    stage_doorless(&a, &rpc, &keys, &signing_key(0x0A), off_partition).await;

    let report = plane(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        (report.applied, report.unmergeable),
        (0, 1),
        "a fleet-only kind must not apply off its home scope: {report:?}"
    );
    assert!(
        b.state(KIND_DEVICE_SET, &hex::encode(x))
            .await
            .unwrap()
            .is_none(),
        "an off-partition row reached merged state"
    );

    // …and the same row on its home scope still applies — the check keys on
    // the partition, not on the kind being fleet-only.
    stage_doorless(
        &a,
        &rpc,
        &keys,
        &signing_key(0x0A),
        device_set_entry(&x, enrollment_value(x, &root, 5_000)),
    )
    .await;
    let report = plane_fleet(&b, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    assert_eq!(
        report.applied, 1,
        "the home-scope row must land: {report:?}"
    );
    assert!(matches!(
        stored_device_set(&b, &x).await,
        fauna_core::generation::DeviceSetRecord::Enrolled { .. }
    ));
}

// ── ST-007: the resolver is the trust boundary ────────────────────────────────
//
// `build_mint`'s guards (non-empty member set, minter ∈ members) are
// builder-side; a `BackupKey` holder crafts the `Minted` row directly, and the
// escrow put door acks any caller-named generation (it structurally cannot read
// plane state). So whatever the builder refuses, the RESOLVER must refuse too —
// else a member-set-excluding-the-writer mint becomes the honest writer's
// resolved tip: a generation it can never key, wedging its fleet-only sealing
// permanently (a client-causable unrecoverable state — the invariant ruling 2
// itself cites). These two pins drive the wedge through the REAL doors and
// assert the honest device still seals.

/// A crafted mint published through the real machinery doors: `build_mint`
/// over a chosen (subset) member list, deposited via the REAL escrow put door
/// (whose deployment-signed receipt acks it — "ack is cheap"), both rows
/// published through `minter`'s own plane door (`Gen0` machinery, admitted
/// without resolving). Returns the wedge's generation id.
async fn wedge_mint_via_real_door(
    store: &AccountStore<SqliteBackend>,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    minter: u8,
    members: &[u8],
    parents: Vec<[u8; 32]>,
) -> [u8; 32] {
    let fleet: Vec<FleetMember> = members.iter().map(|&d| fleet_member(d)).collect();
    let target = escrow_target_record(&[0x77; 32]); // fleet_root()'s seed
    let built = build_mint(&fleet, &target, &tk(), parents, &signing_key(minter), 6_000).unwrap();
    let reply: EscrowPutReply = rpc
        .request(
            KIND_ESCROW_PUT,
            EscrowPutRequest {
                generation_id: ByteBuf::from(built.generation_id.to_vec()),
                wrap: ByteBuf::from(built.escrow_wrap.clone()),
                target_key: tk(),
                ..Default::default()
            },
        )
        .await
        .expect("the real door acks any caller-named generation");
    let receipt: fauna_core::generation::EscrowReceiptRecord =
        fauna_core::encoding::canonical_decode(&reply.receipt).unwrap();
    let minter_key = signing_key(minter);
    let p = plane_prod(store, rpc, keys, &minter_key);
    p.put(
        &ItemId {
            kind: KIND_GENERATION_MINT.into(),
            key: fauna_core::hex32::encode(&built.generation_id),
        },
        fauna_core::encoding::canonical_encode(&built.record).unwrap(),
        None,
    )
    .await
    .unwrap();
    p.put(
        &ItemId {
            kind: KIND_ESCROW_RECEIPT.into(),
            key: format!(
                "{}/{}",
                fauna_core::hex32::encode(&built.generation_id),
                fauna_core::hex32::encode(&receipt.holder_id)
            ),
        },
        fauna_core::encoding::canonical_encode(&receipt).unwrap(),
        None,
    )
    .await
    .unwrap();
    built.generation_id
}

/// Every live mint row's generation id at `store`, in row order.
async fn live_mint_ids(store: &AccountStore<SqliteBackend>) -> Vec<[u8; 32]> {
    store
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .map(|e| fauna_core::hex32::decode(&e.key).unwrap())
        .collect()
}

/// **ST-007 pin, Consequence A (first-mint race).** A publishes an acked mint
/// whose member set is `[A]` alone — admissible everywhere (every listed
/// member verifies), acked by the real door, and keyable by nobody but A.
/// H then writes the proof kind for the first time. H must SEAL — a mint H
/// cannot key must never be H's sealing tip, and with no candidate reachable
/// for H the first-need mint fires (the old `any_mint_row_exists` guard read
/// the attacker's row as "a generation exists" and refused forever). The
/// heal-mint keeps the supersession edge: it names the wedge among its
/// parents rather than forking a parentless root.
#[tokio::test]
async fn an_excluding_mint_cannot_wedge_another_devices_first_seal() {
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let (a, h) = (replica(0x0A).await, replica(0x0B).await);

    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    enroll_prod(&h, &rpc, &keys, 0x0B).await;
    publish_escrow_target(&a, &rpc, &keys, 0x0A, &IDENTITY_SEED).await;

    // The wedge: members = [A] only, deposited through the REAL door.
    let wedge = wedge_mint_via_real_door(&a, &rpc, &keys, 0x0A, &[0x0A], vec![]).await;

    // H merges everything: both enrollments, the target, the wedge + its ack.
    plane_prod(&h, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();

    // H's first `GenerationTip` write. The wedge is not keyable by H, so it is
    // not H's candidate; no candidate resolves, and the door mints instead of
    // refusing forever — the charter's trigger (a), not the narrowed guard.
    plane_prod(&h, &rpc, &keys, &signing_key(0x0B))
        .put(
            &device_endpoints_item(&writer_id(0x0B)),
            endpoints_of(0x0B),
            stamp(100, 0x0B),
        )
        .await
        .expect("an excluding mint must not wedge H's fleet-only sealing");

    // H healed with its own mint — and kept the DAG connected: the heal-mint
    // names the wedge among its parents (supersession edge, not a second root).
    let ids = live_mint_ids(&h).await;
    assert_eq!(ids.len(), 2, "wedge + heal-mint: {ids:?}");
    let heal = ids.iter().copied().find(|id| *id != wedge).unwrap();
    let heal_row = h
        .state(KIND_GENERATION_MINT, &fauna_core::hex32::encode(&heal))
        .await
        .unwrap()
        .unwrap();
    let record: fauna_core::generation::GenerationMintRecord =
        fauna_core::encoding::canonical_decode(&heal_row.value).unwrap();
    let fauna_core::generation::GenerationMintRecord::Minted { core, .. } = record else {
        panic!("the heal-mint is Minted");
    };
    assert!(
        core.parents.contains(&wedge),
        "the heal-mint must name the wedge as a parent, keeping the supersession \
         edge — got parents {:?}",
        core.parents
    );

    // And on the wire, H's row is sealed under the heal generation — never the
    // wedge.
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                since: 0,
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                frontier: Some(std::collections::BTreeMap::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sealed_under: Vec<_> = reply
        .changes
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .filter_map(|envelope| peek_generation_id(envelope))
        .collect();
    assert_eq!(
        sealed_under,
        vec![heal],
        "H's endpoint row seals under H's own heal generation, never the wedge"
    );
}

/// **ST-007 pin, Consequence A (retirement).** H seals under generation 1
/// (the door's own first-need mint). A then publishes an acked wedge naming
/// gen 1 as parent, member set `[A]` alone. At H the wedge must be no
/// candidate at all — a mint H cannot key must not RETIRE the tip H can key —
/// so H's next write still seals, still under gen 1.
#[tokio::test]
async fn an_excluding_descendant_cannot_retire_the_tip_an_honest_device_seals_under() {
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let (a, h) = (replica(0x0A).await, replica(0x0B).await);

    enroll_prod(&a, &rpc, &keys, 0x0A).await;
    enroll_prod(&h, &rpc, &keys, 0x0B).await;
    plane_prod(&h, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    publish_escrow_target(&h, &rpc, &keys, 0x0B, &IDENTITY_SEED).await;

    // Generation 1: H's own first-need mint, wrapping both members.
    plane_prod(&h, &rpc, &keys, &signing_key(0x0B))
        .put(
            &device_endpoints_item(&writer_id(0x0B)),
            endpoints_of(0x0B),
            stamp(100, 0x0B),
        )
        .await
        .unwrap();
    let gen1 = minted_generation_id(&h).await;

    // The wedge: names gen 1 as parent, members = [A] alone, real-door ack.
    let wedge = wedge_mint_via_real_door(&a, &rpc, &keys, 0x0A, &[0x0A], vec![gen1]).await;

    // H merges the wedge and its ack, then writes again.
    plane_prod(&h, &rpc, &keys, &signing_key(0x0B))
        .walk()
        .await
        .unwrap();
    plane_prod(&h, &rpc, &keys, &signing_key(0x0B))
        .put(
            &device_endpoints_item(&writer_id(0x0B)),
            endpoints_of(0x0B),
            stamp(200, 0x0B),
        )
        .await
        .expect("a mint H cannot key must not retire the tip H seals under");

    // Still exactly gen 1 — H minted nothing new (its candidate tip stands),
    // and the wire shows the re-sealed row under gen 1, never the wedge.
    let ids = live_mint_ids(&h).await;
    assert_eq!(
        ids.iter().filter(|id| **id != wedge).count(),
        1,
        "no heal-mint here — gen 1 was never retired for H: {ids:?}"
    );
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                since: 0,
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                frontier: Some(std::collections::BTreeMap::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sealed_under: Vec<_> = reply
        .changes
        .iter()
        .filter_map(|c| c.entry.as_ref())
        .filter_map(|envelope| peek_generation_id(envelope))
        .collect();
    assert_eq!(
        sealed_under,
        vec![gen1],
        "H's endpoint row stays sealed under gen 1 (per-device key: one row, \
         re-sealed in place), never the wedge"
    );
}

// ── Fleet-scope reclamation (charter § The generation machinery → *Fleet-scope
// reclamation*) ────────────────────────────

/// A device's retained-key bundle — what the T10 slot carries in production
/// (`principal_bundle`), which the resolver consults once a consumed top-up
/// cell has been reclaimed off the feed.
#[derive(Default)]
struct MemCustody(std::sync::Mutex<std::collections::BTreeMap<[u8; 32], [u8; 32]>>);

impl fauna_sync_engine::generation_tip::RetainedKeyCustody for MemCustody {
    fn retained_generation_key(
        &self,
        generation: &[u8; 32],
    ) -> Option<fauna_core::crypto::GenerationKey> {
        self.0
            .lock()
            .unwrap()
            .get(generation)
            .map(|b| fauna_core::crypto::GenerationKey::from_bytes(*b))
    }
    fn record_generation_key(
        &self,
        generation: &[u8; 32],
        key: &fauna_core::crypto::GenerationKey,
    ) {
        self.0.lock().unwrap().insert(*generation, *key.as_bytes());
    }
    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.0.lock().unwrap().remove(generation);
    }
}

/// One fleet member of the reclamation sweep: its own replica, its own writer
/// key (seeded by a u16 so a 264-device history fits), its own bundle.
struct Seat {
    n: u16,
    store: AccountStore<SqliteBackend>,
    key: SigningKey,
    custody: MemCustody,
    /// `Some` for a seat that signed in with the identity seed — the only
    /// kind that runs the pump's escrow-recovery leg. The sweep's seats stay
    /// `None`, so every older pin keeps measuring the top-up path alone.
    seed_holding: Option<fauna_sync_engine::generation_escrow_recover::EscrowRecoveryMemo>,
    /// `true` for a seat running as the SUCCESSOR identity ([`successor_root`]):
    /// its trust is [`TRUST_SUCCESSOR`] (root = the successor, `prior` = the
    /// fleet root it succeeded), its enrollment is signed by the successor,
    /// and a seed-holding one recovers with [`SUCCESSOR_SEED`].
    successor: bool,
    /// The attested predecessors this seat's runtime was handed — none, but
    /// for a successor seat on a machine whose registry holds the
    /// predecessor's seed ([`Self::attesting_the_predecessor`]). The fleet
    /// plane takes its mint-kind keys and its retired machinery keys from it,
    /// as the driver's does.
    attested: fauna_sync_engine::attested_predecessors::AttestedPredecessors,
    /// The predecessors' keypairs a seed-holding runtime on this machine
    /// holds beside its own seed (`SeedHolder::predecessors`) — what its
    /// escrow recovery opens a kept wrap under. Set with [`Self::attested`].
    predecessor_keypairs: Vec<fauna_core::identity::ActorKeypair>,
}

impl Seat {
    fn seed(n: u16) -> [u8; 32] {
        let mut s = [0xC0u8; 32];
        s[0] = (n & 0xFF) as u8;
        s[1] = (n >> 8) as u8;
        s
    }
    async fn new(n: u16) -> Seat {
        let key = SigningKey::from_bytes(&Self::seed(n));
        let store = AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            &hex::encode(ACTOR),
            WriterId(key.verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        Seat {
            n,
            store,
            key,
            custody: MemCustody::default(),
            seed_holding: None,
            successor: false,
            attested: Default::default(),
            predecessor_keypairs: Vec::new(),
        }
    }
    /// This successor seat's machine holds the predecessor's seed
    /// ([`SEAT_IDENTITY_SEED`]): its runtime is handed that identity as an
    /// attested predecessor, through the constructor every host uses.
    fn attesting_the_predecessor(self) -> Seat {
        Seat {
            attested:
                fauna_sync_engine::attested_predecessors::AttestedPredecessors::from_backup_keys([
                    (
                        fleet_root().actor_id(),
                        &BackupKey::derive(&SEAT_IDENTITY_SEED),
                    ),
                ]),
            predecessor_keypairs: vec![fleet_root()],
            ..self
        }
    }
    /// A seat running as the successor identity; `seed_holding` as for
    /// [`Self::seed_holding`], with the successor's seed.
    async fn successor(n: u16, seed_holding: bool) -> Seat {
        Seat {
            successor: true,
            seed_holding: seed_holding.then(Default::default),
            ..Seat::new(n).await
        }
    }
    fn trust(&self) -> &'static fauna_sync_engine::generation_tip::GenerationTrust {
        if self.successor {
            &TRUST_SUCCESSOR
        } else {
            &TRUST_DEPLOYMENT
        }
    }
    fn identity_seed(&self) -> &'static [u8; 32] {
        if self.successor {
            &SUCCESSOR_SEED
        } else {
            &SEAT_IDENTITY_SEED
        }
    }
    /// A seat whose app holds the identity seed ([`SEAT_IDENTITY_SEED`]).
    async fn seed_holding(n: u16) -> Seat {
        Seat {
            seed_holding: Some(Default::default()),
            ..Seat::new(n).await
        }
    }
    fn id(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
    fn plane<'a>(
        &'a self,
        rpc: &'a RouterRequester,
        keys: &'a AccountStateKeySchedule,
    ) -> AccountStatePlane<'a, SqliteBackend, RouterRequester> {
        AccountStatePlane::new(
            &self.store,
            rpc,
            keys,
            &self.key,
            self.trust(),
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
        .with_generation_custody(&self.custody)
        .with_predecessor_mint_keys(self.attested.mint_kind_keys())
        .with_predecessor_machinery_keys(self.attested.retired_machinery_keys())
    }
    fn enrollment_value(&self) -> Vec<u8> {
        let root = if self.successor {
            successor_root()
        } else {
            fleet_root()
        };
        signed_enrollment_value(&self.key, &root, 5_000 + i64::from(self.n))
    }
    /// Enroll on the plane, and register the writer key as this machine's
    /// principal grant on the nest — the credential the feed's retention gate
    /// counts a walk mark under (the enrollment ceremony's two register legs,
    /// taken at the DB so the sweep's 264 enrollments are not gated by the free
    /// tier's device cap, which is a product rule about the roster and not
    /// about this gate).
    async fn join(&self, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
        self.plane(rpc, keys)
            .put(
                &ItemId {
                    kind: KIND_DEVICE_SET.into(),
                    key: hex::encode(self.id()),
                },
                self.enrollment_value(),
                None,
            )
            .await
            .unwrap();
        rpc.state
            .db
            .register_sync_device(&ACTOR, &self.id(), "seat", None, "")
            .await
            .unwrap();
        rpc.state
            .db
            .set_sync_device_grant(&ACTOR, &self.id(), &self.id(), b"grant")
            .await
            .unwrap();
    }
    /// One full pump pass's fleet legs, in the pump's order: the full-state
    /// reconcile and the publish diff behind it (`account-sync-plane.md`
    /// § The bind leg, ruling 1 — the listing it banks is what reclamation
    /// reads "published" from), escrow recovery (seed-holding seats only),
    /// the device-endpoints re-seal, top-up, unkeyable, reclaim, and the
    /// secondary leg's removed-device arm with no linked replica.
    async fn pass(&self, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
        self.pass_diffed(rpc, keys).await;
    }
    /// [`Self::pass`], answering what its publish diff did.
    async fn pass_diffed(
        &self,
        rpc: &RouterRequester,
        keys: &AccountStateKeySchedule,
    ) -> fauna_sync_engine::publish_diff::DiffPush {
        let p = self.plane(rpc, keys);
        p.reconcile().await.unwrap();
        let diff =
            fauna_sync_engine::publish_diff::publish_diff(&self.store, &p, self.trust(), &self.key)
                .await
                .unwrap();
        if let Some(memo) = &self.seed_holding {
            let recovery = fauna_sync_engine::generation_escrow_recover::ensure_recovered(
                &self.store,
                &p,
                self.trust(),
                &self.key,
                self.identity_seed(),
                &self.predecessor_keypairs,
                memo,
                &[],
            )
            .await
            .unwrap();
            // The pump re-presents the scope behind a recovery that keyed a
            // generation, so the rows it opens merge this same pass.
            if matches!(
                recovery,
                fauna_sync_engine::generation_escrow_recover::EscrowRecoveryPass::Recovered(_)
            ) {
                p.reconcile().await.unwrap();
            }
        }
        fauna_sync_engine::generation_reescrow::ensure_reescrowed(
            &self.store,
            &p,
            self.trust(),
            &self.key,
            None,
        )
        .await
        .unwrap();
        fauna_sync_engine::device_endpoints_writer::ensure_published(
            &self.store,
            &p,
            self.trust(),
            &self.key,
            None,
            None,
        )
        .await
        .unwrap();
        fauna_sync_engine::generation_topup::ensure_topped_up(
            &self.store,
            &p,
            self.trust(),
            &self.key,
        )
        .await
        .unwrap();
        fauna_sync_engine::generation_unkeyable::ensure_signalled(
            &self.store,
            &p,
            self.trust(),
            &self.key,
        )
        .await
        .unwrap();
        fauna_sync_engine::generation_reclaim::ensure_reclaimed(
            &self.store,
            &p,
            self.trust(),
            &self.key,
        )
        .await
        .unwrap();
        // The secondary leg's removed-device arm, as the pump runs it right
        // behind reclamation in a seed holder (`account-sync-plane.md` § The
        // bind leg, ruling 7): the pass retires no removal evidence, and a
        // leg run with no linked replica to carry it to retires it in the
        // same pass — without it the live count grows a row per sign-out.
        let evidence =
            fauna_sync_engine::generation_reclaim::removal_evidence(&self.store, &p, self.trust())
                .await
                .unwrap();
        let arm = fauna_sync_engine::linked_leg::retire_carried_evidence(
            &fauna_sync_engine::linked_leg::LinkedCtx {
                store: &self.store,
                bound_fleet: &p,
                schedule: keys,
                trust: self.trust(),
                writer_key: &self.key,
                custody: p.generation_custody(),
            },
            &evidence,
            &[],
            &std::collections::BTreeMap::new(),
            &[],
        )
        .await;
        assert!(arm.errors.is_empty(), "the removed-device arm: {arm:?}");
        diff
    }
    /// The sign-out: the plane-side severance the runtime's
    /// `shutdown_for_sign_out` runs, then the grant retirement (its tombstone
    /// is what drops this walker's mark).
    async fn sign_out(&self, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
        let p = self.plane(rpc, keys);
        fauna_sync_engine::generation_reclaim::sever_self(&self.store, &p, &self.key)
            .await
            .unwrap();
        rpc.state
            .db
            .revoke_device_grant(&ACTOR, &self.id())
            .await
            .unwrap();
    }
}

/// The nest's live-entry count for the fleet scope — the number the cap reads.
async fn live_fleet_entries(rpc: &RouterRequester) -> usize {
    let fs = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
        .await
        .unwrap()
        .expect("the fleet scope exists");
    rpc.state
        .db
        .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
        .len()
}

/// One replica's fleet-scope relay plane: its row count, and — for the
/// reader of a red run — the rows bucketed by writer (a current seat's, a
/// departed one's), form (v1 gen-0 machinery, v2 generation-sealed) and, for
/// a v2 row, whether this replica's merged state reads its generation
/// `Shredded`.
async fn relay_census(seat: &Seat, current: &[[u8; 32]]) -> (usize, Vec<String>) {
    use fauna_core::generation::GenerationMintRecord;
    let shredded: std::collections::BTreeSet<[u8; 32]> = seat
        .store
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&e.value)
                .is_ok_and(|r| matches!(r, GenerationMintRecord::Shredded { .. }))
        })
        .map(|e| fauna_core::hex32::decode(&e.key).unwrap())
        .collect();
    let rows = seat
        .store
        .relay_rows(
            ACCOUNT_STATE_FLEET_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap();
    let mut buckets: std::collections::BTreeMap<(bool, &str), usize> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let form = match row.entry.as_deref().and_then(peek_generation_id) {
            None => "v1",
            Some(g) if shredded.contains(&g) => "v2 under a shredded generation",
            Some(_) => "v2 under a live generation",
        };
        *buckets
            .entry((current.contains(&row.writer.0), form))
            .or_default() += 1;
    }
    let lines = buckets
        .into_iter()
        .map(|((writer_current, form), n)| {
            format!(
                "{n} relay rows — writer {}, {form}",
                if writer_current {
                    "current"
                } else {
                    "departed"
                }
            )
        })
        .collect();
    (rows.len(), lines)
}

/// **The steady state, measured.** A
/// fleet is driven through sign-out → sign-in cycles against the real nest:
/// each cycle the oldest device signs out (its own `Removed` row, its own rows
/// retired, its grant tombstoned) and a fresh device joins, walks, and writes
/// the proof kind — which mints, since the removal unseated the tip. Every
/// seat then runs the pump's fleet legs `rounds` times. Without reclamation
/// every cycle costs about 2N live entries for good and the cap falls in a
/// few dozen cycles; with it the count after the last cycle sits within
/// O(members) of the count after the first `sample_at`, no put is ever
/// refused `scope_full`, and every seat still keys the current tip.
///
/// Two callers: the ratified 64 × 200 measurement (explicit, `--ignored`
/// — about four and a half hours in a debug build, every cost being real
/// X-Wing and AEAD work over a real feed) and the suite's fast twin, which
/// asserts the same property over a smaller fleet and fewer cycles.
async fn reclamation_sweep(fleet: u16, cycles: u16, rounds: usize, sample_at: u16) {
    use fauna_protocol::account_state::MAX_STATE_ENTRIES_PER_SCOPE;
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let mut seats: Vec<Seat> = Vec::new();
    for n in 0..fleet {
        let seat = Seat::new(n).await;
        seat.join(&rpc, &keys).await;
        seats.push(seat);
    }
    // The escrow target, published once by a seed-holding surface.
    {
        let target =
            fauna_sync_engine::generation_mint::escrow_target_entry(&IDENTITY_SEED).unwrap();
        seats[0]
            .plane(&rpc, &keys)
            .put(
                &ItemId {
                    kind: KIND_ESCROW_TARGET.into(),
                    key: tk(),
                },
                target.value,
                None,
            )
            .await
            .unwrap();
    }
    // Every seat walks the fleet, and the first data write mints generation 1.
    for seat in &seats {
        seat.plane(&rpc, &keys).walk().await.unwrap();
    }
    for _ in 0..rounds {
        for seat in &seats {
            seat.pass(&rpc, &keys).await;
        }
    }
    let baseline = live_fleet_entries(&rpc).await;
    let started = std::time::Instant::now();

    let mut at_sample = 0usize;
    let mut relay_at_sample = 0usize;
    let mut peak = baseline;
    // The bind leg's publish diff against the one nest every seat is bound
    // to (`account-sync-plane.md` § The bind leg, ruling 1): the nest holds
    // every row the fleet wrote, so no push may land.
    let mut diff_puts = 0usize;
    let mut diff_refused = 0usize;
    let mut diff_dead = 0usize;
    for cycle in 1..=cycles {
        // The oldest seat signs out; a fresh one joins. Seat 0 alone stays
        // for the whole sweep: the long-lived replica whose relay plane is
        // measured below.
        let leaving = seats.remove(1);
        leaving.sign_out(&rpc, &keys).await;
        drop(leaving);
        let joining = Seat::new(fleet + cycle - 1).await;
        joining.join(&rpc, &keys).await;
        seats.push(joining);
        // Three rounds of the pump's fleet legs across the fleet: the new
        // seat's first write mints (the removal unseated the tip); the spill
        // tops up; everyone re-seals its endpoints under the new tip and
        // publishes its reach; the previous generation goes dataless and is
        // shredded; the walkers' marks catch up and the retires land.
        for _ in 0..rounds {
            for seat in &seats {
                let diff = seat.pass_diffed(&rpc, &keys).await;
                diff_puts += diff.pushed + diff.refused;
                diff_refused += diff.refused;
                diff_dead += diff.dead;
            }
        }
        let live = live_fleet_entries(&rpc).await;
        peak = peak.max(live);
        if cycle % sample_at == 0 {
            eprintln!(
                "reclamation sweep: cycle {cycle}, {live} live entries, {:.0}s elapsed",
                started.elapsed().as_secs_f64()
            );
        }
        assert!(
            live < usize::try_from(MAX_STATE_ENTRIES_PER_SCOPE).unwrap(),
            "cycle {cycle}: {live} live entries — the cap would refuse the next new row"
        );
        if cycle == sample_at {
            at_sample = live;
            relay_at_sample = relay_census(&seats[0], &[]).await.0;
        }
    }
    let at_end = live_fleet_entries(&rpc).await;
    // Nothing the diff sends ever lands on a nest that holds the fleet's rows.
    // What it does send is the ruled learning path (§ The bind leg, ruling
    // 1(c)): a sibling's row the nest has retired, still in this replica's
    // relay plane, is refused once per replica and its copy retired — only
    // the rows this replica cannot call dead from its own merged state (a
    // shredded generation's mint row, a sibling's superseded row it could not
    // yet call covered), since a dead row is never sent (ruling 1(b)).
    eprintln!(
        "reclamation sweep: the publish diff sent {diff_puts} puts over {cycles} cycles, \
         {diff_refused} refused, {diff_dead} skipped as dead"
    );
    assert_eq!(
        diff_puts,
        diff_refused,
        "the publish diff landed {} rows on a nest that already holds the fleet's rows",
        diff_puts - diff_refused
    );
    let refused_cap = usize::from(cycles) * usize::from(fleet) * 2;
    assert!(
        diff_refused <= refused_cap,
        "the publish diff's learning path is not bounded: {diff_refused} refused puts over \
         {cycles} cycles of a {fleet}-seat fleet (cap {refused_cap}) — is a dead row pushed?"
    );
    let members = usize::from(fleet);
    if at_end > at_sample + members {
        // The leak's shape, for the reader of a red run: which writers hold
        // the surplus rows (a current seat's, a removed seat's), which form
        // (v1 gen-0 machinery, v2 generation-sealed data), how big.
        let fs = rpc
            .state
            .db
            .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
            .await
            .unwrap()
            .unwrap();
        let rows = rpc
            .state
            .db
            .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
            .await
            .unwrap()
            .rows;
        let current: std::collections::BTreeSet<Vec<u8>> =
            seats.iter().map(|s| s.id().to_vec()).collect();
        let mut buckets: std::collections::BTreeMap<(bool, bool, usize), usize> =
            std::collections::BTreeMap::new();
        for row in &rows {
            let writer_current = row
                .origin_writer
                .as_ref()
                .is_some_and(|w| current.contains(w));
            let entry = row.entry_sealed.as_deref().unwrap_or(&[]);
            let v2 = peek_generation_id(entry).is_some();
            let size = entry.len() / 256 * 256;
            *buckets.entry((writer_current, v2, size)).or_default() += 1;
        }
        for ((writer_current, v2, size), n) in &buckets {
            eprintln!(
                "leak: {n} rows — writer {}, form {}, ~{size} B",
                if *writer_current {
                    "current"
                } else {
                    "removed"
                },
                if *v2 { "v2" } else { "v1" }
            );
        }
    }
    assert!(
        at_end <= at_sample + members,
        "not a steady state: {at_sample} live entries after cycle {sample_at}, {at_end} after \
         cycle {cycles} (baseline {baseline}, peak {peak}) — reclamation is not keeping up"
    );
    // The long-lived replica's relay plane stays O(live rows) too: a live
    // sibling's superseded generation-sealed rows, and the dead gen-0 rows it
    // retired itself, are forgotten here once dead everywhere — the relay
    // plane is never told about another writer's retirements, so without the
    // forget every tip change leaves one row per member per sealed item here
    // for good (`account-data-taxonomy.md` § The generation machinery →
    // *Fleet-scope reclamation*).
    let current: Vec<[u8; 32]> = seats.iter().map(Seat::id).collect();
    let (relay_at_end, census) = relay_census(&seats[0], &current).await;
    assert!(
        relay_at_end <= relay_at_sample + members,
        "the long-lived replica's relay plane grows: {relay_at_sample} rows after cycle \
         {sample_at}, {relay_at_end} after cycle {cycles} ({at_end} live entries at the nest) — \
         {census:?}"
    );
    // Every current seat keys the current tip — from the feed or its bundle —
    // and the fleet agrees on one tip.
    let mut tips = std::collections::BTreeSet::new();
    for seat in &seats {
        let r = fauna_sync_engine::generation_tip::resolve_tip(
            &seat.store,
            &TRUST_DEPLOYMENT,
            &seat.key,
            Some(&seat.custody),
        )
        .await
        .unwrap();
        tips.insert(
            r.tip
                .map(|t| t.generation_id)
                .unwrap_or_else(|| panic!("seat {} resolves no tip: {:?}", seat.n, r.unkeyable)),
        );
    }
    assert_eq!(tips.len(), 1, "one tip across the fleet: {tips:?}");
    // The escrow sweep (clause (3e)): the holder's escrow table holds wraps
    // only for generations some current seat still holds a mint row for —
    // the tip's among them, the recovery path — never one per generation
    // ever minted. (A seat never forgets a mint row, so the union over the
    // fleet is every generation whose receipt could still be live.)
    let mut live_generations = std::collections::BTreeSet::new();
    for seat in &seats {
        live_generations.extend(live_mint_ids(&seat.store).await);
    }
    let wraps: std::collections::BTreeSet<[u8; 32]> = rpc
        .state
        .db
        .get_generation_escrow_wraps(&ACTOR, None)
        .await
        .unwrap()
        .into_iter()
        .map(|w| w.generation_id)
        .collect();
    let tip = *tips.iter().next().unwrap();
    assert!(
        wraps.contains(&tip),
        "the tip's escrow wrap is the recovery path and stays with the holder"
    );
    let dead: Vec<String> = wraps
        .difference(&live_generations)
        .map(fauna_core::hex32::encode)
        .collect();
    assert!(
        dead.is_empty(),
        "escrow wraps of generations no current seat holds a mint row for ({} wraps, {} \
         generations held): {dead:?}",
        wraps.len(),
        live_generations.len()
    );
    assert!(
        wraps.len() < usize::from(cycles),
        "{} escrow wraps after {cycles} cycles — one per generation ever minted",
        wraps.len()
    );
    eprintln!(
        "reclamation sweep: {fleet} seats, {cycles} cycles, {rounds} rounds — baseline \
         {baseline}, after cycle {sample_at}: {at_sample}, after cycle {cycles}: {at_end}, \
         peak {peak}, {} escrow wraps, long-lived relay plane {relay_at_sample} → \
         {relay_at_end} rows, {:.0}s",
        wraps.len(),
        started.elapsed().as_secs_f64()
    );
}

/// **What a pass may skip — end to end against the nest's own
/// handler table.** A walker that died without signing out (a live grant, a
/// mark that never moves) holds the retention gate for every row landed
/// after its mark; before this rule a pass asked the nest to retire each of
/// those rows every pass and was refused `not_yet_stable` one RPC at a time.
/// Now the feed serves the gate's watermark, the walk keeps each row's feed
/// coordinate, and the pass withholds the retires the gate would refuse: a
/// full round of passes across the live fleet sends **no** retire request at
/// all while the stranded walker stays, and the moment the user removes it
/// (its grant tombstoned) the next round's walk reads a higher watermark and
/// the withheld retires go out and land.
#[tokio::test]
async fn a_stranded_walker_costs_the_fleet_no_retire_request_per_pass() {
    // `fleet_root()`'s seed: the target row is keyed by the actor it derives.
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let leaving = Seat::new(0).await;
    let live = Seat::new(1).await;
    let stranded = Seat::new(2).await;
    for seat in [&leaving, &live, &stranded] {
        seat.join(&rpc, &keys).await;
    }
    {
        let target =
            fauna_sync_engine::generation_mint::escrow_target_entry(&IDENTITY_SEED).unwrap();
        leaving
            .plane(&rpc, &keys)
            .put(
                &ItemId {
                    kind: KIND_ESCROW_TARGET.into(),
                    key: tk(),
                },
                target.value,
                None,
            )
            .await
            .unwrap();
    }
    for seat in [&leaving, &live, &stranded] {
        seat.plane(&rpc, &keys).walk().await.unwrap();
    }
    for _ in 0..2 {
        for seat in [&leaving, &live, &stranded] {
            seat.pass(&rpc, &keys).await;
        }
    }
    // The stranded seat's last act: it never walks again, and its grant
    // stays live — the dead machine the ruling names.
    drop(stranded);

    // A cycle: the first seat signs out, a fresh one joins, and the live
    // fleet settles — a walker's counted mark is what it held through its
    // PREVIOUS walk, so the retires the gate accepts take a few rounds to
    // land; the round measured below is the steady state, where every one of
    // them has and every other one is held by the stranded mark.
    leaving.sign_out(&rpc, &keys).await;
    drop(leaving);
    let joining = Seat::new(3).await;
    joining.join(&rpc, &keys).await;
    joining.plane(&rpc, &keys).walk().await.unwrap();
    let fleet = [&live, &joining];
    for _ in 0..4 {
        for seat in fleet {
            seat.pass(&rpc, &keys).await;
        }
    }
    let before = rpc.calls_of("fauna.account.state.retire");
    let retires_before = rpc.retires.lock().unwrap().len();
    let mut withheld = 0usize;
    let mut deferred = 0usize;
    let mut trace: Vec<String> = Vec::new();
    for seat in fleet {
        let at = rpc.retires.lock().unwrap().len();
        seat.pass(&rpc, &keys).await;
        trace.push(format!(
            "seat {} ({}) pass: retires {:?}",
            seat.n,
            &hex::encode(seat.id())[..8],
            rpc.retires_since(at)
        ));
        let at = rpc.retires.lock().unwrap().len();
        let p = seat.plane(&rpc, &keys);
        p.walk().await.unwrap();
        let watermark = p.retirable_through_seq();
        let pass = fauna_sync_engine::generation_reclaim::ensure_reclaimed(
            &seat.store,
            &p,
            &TRUST_DEPLOYMENT,
            &seat.key,
        )
        .await
        .unwrap();
        trace.push(format!(
            "seat {} reclaim: watermark {watermark:?}, {pass:?}, retires {:?}",
            seat.n,
            rpc.retires_since(at)
        ));
        assert!(
            watermark.is_some(),
            "the feed serves the gate's watermark: {pass:?}"
        );
        withheld += pass.withheld;
        deferred += pass.deferred;
    }
    let sent = rpc.calls_of("fauna.account.state.retire") - before;
    assert_eq!(
        (sent, deferred),
        (0, 0),
        "a round across the live fleet under a stranded walker sends no retire and is refused \
         none (withheld {withheld}); sent {:?}; departed seat {}, stranded seat {}; {}",
        rpc.retires_since(retires_before),
        &hex::encode(Seat::new(0).await.id())[..8],
        &hex::encode(Seat::new(2).await.id())[..8],
        trace.join("; ")
    );
    assert!(
        withheld > 0,
        "the departed seat's rows above the mark are withheld, not forgotten"
    );
    let held_live = live_fleet_entries(&rpc).await;

    // Removal is the control: the user removes the dead machine, its mark
    // stops counting, and the next round's walk reads a watermark past the
    // withheld rows — which go out and land.
    rpc.state
        .db
        .revoke_device_grant(&ACTOR, &Seat::new(2).await.id())
        .await
        .unwrap();
    let before = rpc.calls_of("fauna.account.state.retire");
    let mut retired = 0usize;
    for seat in fleet {
        let p = seat.plane(&rpc, &keys);
        p.walk().await.unwrap();
        let pass = fauna_sync_engine::generation_reclaim::ensure_reclaimed(
            &seat.store,
            &p,
            &TRUST_DEPLOYMENT,
            &seat.key,
        )
        .await
        .unwrap();
        assert_eq!(pass.withheld, 0, "{pass:?}");
        retired += pass.retired;
    }
    let sent = rpc.calls_of("fauna.account.state.retire") - before;
    assert!(sent > 0 && retired > 0, "sent {sent}, retired {retired}");
    assert!(
        live_fleet_entries(&rpc).await < held_live,
        "the live-entry count drops once the withheld retires land"
    );
}

/// The suite's fast twin of the measurement below: the same steady-state
/// assertion over a 12-seat fleet and 30 cycles, two pump rounds per cycle.
#[tokio::test]
async fn a_fleet_survives_sign_out_cycles_at_a_steady_live_entry_count() {
    reclamation_sweep(12, 30, 2, 10).await;
}

/// **The ratified measurement** — the row's own numbers: a 64-device fleet
/// through 200 sign-out cycles. Explicit (`cargo test … -- --ignored
/// a_64_device_fleet`): about four and a half hours in a debug build
/// (measured 2026-10-01: 15,941 s), every cost being real crypto over a
/// real feed; the fast twin above is the suite's coverage.
#[tokio::test]
#[ignore = "the ratified 64 x 200 measurement, about four and a half hours in a debug build; run explicitly"]
async fn a_64_device_fleet_survives_200_sign_out_cycles_at_a_steady_live_entry_count() {
    reclamation_sweep(64, 200, 3, 20).await;
}

/// **A member's row statement reaches a sibling's removal facts — sealed,
/// through the real nest, and it is what the removal resolves from**
/// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*, clause (4), *The removal target*). Seat 1 states the nest row
/// it enrolled on, on its own generation-sealed device-endpoints entry; seat 0
/// walks. Removing that row then resolves to seat 1 **whatever principal the
/// nest's roster puts on it** — seat 0's own (the hit arm: the caller is not
/// excluded), a stranger's, or none (the spared arm: seat 1 does not stay a
/// wrap target) — and seat 1 cannot be re-paired with another row.
///
/// Red-verified: publish the entry without the statement (`None` in seat 1's
/// `ensure_published`) and the facts carry no binding, so the own-principal
/// claim answers `OwnDevice` instead of seat 1.
#[tokio::test]
async fn a_members_row_statement_reaches_a_siblings_removal_facts() {
    use fauna_core::fleet_removal::{FleetRemovalRefusal, resolve_removal_targets};
    const IDENTITY_SEED: [u8; 32] = [0x77; 32];

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let seats = [Seat::new(0).await, Seat::new(1).await];
    for seat in &seats {
        seat.join(&rpc, &keys).await;
    }
    let target = fauna_sync_engine::generation_mint::escrow_target_entry(&IDENTITY_SEED).unwrap();
    seats[0]
        .plane(&rpc, &keys)
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: tk(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();
    for seat in &seats {
        seat.plane(&rpc, &keys).walk().await.unwrap();
    }

    let laptop_row = "5a".repeat(32);
    // Two rounds, the sweep's own settling shape: a top-up wrap published in
    // one round is what lets the other seat key the tip in the next. Seat 1
    // runs the pump's legs by hand because `Seat::pass` feeds no statement.
    for _ in 0..2 {
        seats[0].pass(&rpc, &keys).await;
        let p = seats[1].plane(&rpc, &keys);
        p.walk().await.unwrap();
        fauna_sync_engine::device_endpoints_writer::ensure_published(
            &seats[1].store,
            &p,
            &TRUST_DEPLOYMENT,
            &seats[1].key,
            None,
            Some(laptop_row.clone()),
        )
        .await
        .unwrap();
        fauna_sync_engine::generation_topup::ensure_topped_up(
            &seats[1].store,
            &p,
            &TRUST_DEPLOYMENT,
            &seats[1].key,
        )
        .await
        .unwrap();
    }
    seats[0].pass(&rpc, &keys).await;

    let facts = fauna_sync_engine::fleet_removal::removal_facts(
        &seats[0].store,
        &TRUST_DEPLOYMENT,
        seats[0].id(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        facts.bindings.get(&seats[1].id()),
        Some(&laptop_row),
        "seat 1's own statement, opened at seat 0"
    );
    for claimed in [Some(seats[0].id()), Some([0x7f; 32]), None] {
        assert_eq!(
            resolve_removal_targets(&facts, &laptop_row, claimed.as_ref()),
            Ok(vec![seats[1].id()]),
            "the member stating the row, whatever the nest claims ({claimed:?})"
        );
    }
    assert_eq!(
        resolve_removal_targets(&facts, &"6b".repeat(32), Some(&seats[1].id())),
        Err(FleetRemovalRefusal::RowMismatch),
        "seat 1 states another row — it cannot be re-paired with this one"
    );
}

// ── A departure retires only the departing device's own rows ────────────────
//
//
// Clause (3)(d) retires a removed device's "reach and device-endpoints rows",
// and clause (4)'s sign-out "its own reach and device-endpoints rows" — the
// device-scoped kinds. An account-level row the departing device happened to
// write (the group-reception keypair every community room wraps to, a held
// group machinery root, the custody registry rows, a share member's endpoints)
// is the account's, and stays: `fauna_protocol::merge_policy::TipSealedKind`
// owns which is which.

/// How the account's first device leaves in [`the_reception_key_after`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Departure {
    /// It stays — the control.
    Stays,
    /// It signs out: its own `Removed` row and retires (`sever_self`), then
    /// the grant revoke.
    SignsOut,
    /// A sibling removes it — the devices page's leg: the sibling's `Removed`
    /// row at its id, then the grant revoke. A forged `Removed` by any
    /// `BackupKey` holder reads the same, since exclusion is unconditional.
    RemovedBySibling,
}

/// The nest seqs of `writer`'s live generation-sealed (form v2) rows in the
/// fleet scope — what the nest serves, however the writer's replica fares.
async fn live_v2_rows_of(rpc: &RouterRequester, writer: &[u8; 32]) -> Vec<i64> {
    let fs = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
        .await
        .unwrap()
        .expect("the fleet scope exists");
    rpc.state
        .db
        .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|r| r.origin_writer.as_deref() == Some(writer.as_slice()))
        .filter(|r| {
            r.entry_sealed
                .as_deref()
                .and_then(peek_generation_id)
                .is_some()
        })
        .map(|r| r.seq)
        .collect()
}

/// Does the escrow holder still hold a wrap of `generation`?
async fn escrowed(rpc: &RouterRequester, generation: &[u8; 32]) -> bool {
    rpc.state
        .db
        .get_generation_escrow_wraps(&ACTOR, None)
        .await
        .unwrap()
        .iter()
        .any(|w| w.generation_id == *generation)
}

/// The group-reception keypairs `seat` holds in merged state.
async fn reception_keys_held(seat: &Seat) -> usize {
    fauna_sync_engine::group_state_plane::reception_keys_from_rows(
        seat.store
            .states_of_kind(fauna_protocol::merge_policy::KIND_GROUP_RECEPTION_KEY)
            .await
            .unwrap(),
    )
    .len()
}

/// `rounds` full pump passes across `seats`, each pass followed by the pump's
/// standing reconcile: a row first walked before the seat could key its
/// generation re-presents once a top-up lands.
async fn settle(
    seats: &[&Seat],
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    rounds: usize,
) {
    for _ in 0..rounds {
        for seat in seats {
            seat.pass(rpc, keys).await;
            seat.plane(rpc, keys).reconcile().await.unwrap();
        }
    }
}

/// The identity seed behind [`publish_identity_escrow_target`]'s target — what
/// a [`Seat::seed_holding`] seat's app holds.
const SEAT_IDENTITY_SEED: [u8; 32] = [0x77; 32];

/// The SUCCESSOR identity's seed — the account after an identity succession
/// from [`fleet_root`] (whose seed is [`SEAT_IDENTITY_SEED`]).
const SUCCESSOR_SEED: [u8; 32] = [0x88; 32];

fn successor_root() -> fauna_core::identity::ActorKeypair {
    fauna_core::identity::ActorKeypair::from_secret(SUCCESSOR_SEED)
}

/// The successor's writer-door trust: its own root, the fleet root it
/// succeeded as the ATTESTED `prior`, and the same deployment-identity holder
/// — what `r14_trust` hands a successor's runtime. `prior` signs nothing in
/// the fleet view, which verifies an enrollment against the account root
/// alone (the device set does not cross a succession; every seat here
/// enrolls under the successor root); it feeds only the group authority view.
static TRUST_SUCCESSOR: std::sync::LazyLock<fauna_sync_engine::generation_tip::GenerationTrust> =
    std::sync::LazyLock::new(|| fauna_sync_engine::generation_tip::GenerationTrust {
        root: successor_root().actor_id(),
        prior: vec![fleet_root().actor_id()],
        trusted_holders: vec![deployment_key().verifying_key().to_bytes()].into(),
    });

/// Publish the escrow target once, from a seed-holding surface.
async fn publish_identity_escrow_target(
    seat: &Seat,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
) {
    let target =
        fauna_sync_engine::generation_mint::escrow_target_entry(&SEAT_IDENTITY_SEED).unwrap();
    seat.plane(rpc, keys)
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: tk(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();
}

/// Write the account's group-reception keypair from `seat` — the ceremony
/// driver's seam (`write_reception_key_row`) — and return the nest seq of the
/// row it landed.
async fn write_reception_key(
    seat: &Seat,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
) -> i64 {
    let before = live_v2_rows_of(rpc, &seat.id()).await;
    let plane = seat.plane(rpc, keys);
    fauna_sync_engine::group_state_plane::write_reception_key_row(
        &plane,
        &fauna_core::group_generation::GroupReceptionKeyRecord::mint(1_700_000_000_000),
    )
    .await
    .unwrap();
    // The seam writes the row LOCALLY and leaves the send to the runtime's
    // publish step (its caller arms `publish_due`), so a co-present ceremony
    // never waits on a nest — take that step here.
    plane.publish_pending().await.unwrap();
    let mut landed: Vec<i64> = live_v2_rows_of(rpc, &seat.id())
        .await
        .into_iter()
        .filter(|s| !before.contains(s))
        .collect();
    assert_eq!(landed.len(), 1, "the reception key is one new v2 row");
    landed.pop().unwrap()
}

/// Write the account's senior rotation key from `seat` through the
/// production door (`atproto_identity_rows::merge_atproto_identity`), then
/// take the runtime's publish step.
async fn write_rotation_key(seat: &Seat, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
    let plane = seat.plane(rpc, keys);
    let (joined, moved) = fauna_sync_engine::atproto_identity_rows::merge_atproto_identity(
        &seat.store,
        &plane,
        &fauna_core::data::AtprotoIdentityConfig {
            rotation_keys: vec![senior_rotation_key()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(moved, "the rotation key is a new row");
    assert_eq!(joined.rotation_keys, vec![senior_rotation_key()]);
    plane.publish_pending().await.unwrap();
}

/// The account's MSEK [`write_mail_custody`] writes — the root of every
/// mail key, irrecoverable if lost (`key-material-hierarchy.md` § Path B),
/// existing nowhere but its `fauna.state.mail` state row.
const ACCOUNT_MSEK: [u8; 32] = [0x6d; 32];

/// Write the account's mail custody from `seat` through the production doors
/// (`mail_rows::{write_mail_state, put_credential}`): the state row carrying
/// the MSEK, and one credential row wrapped under it — then take the
/// runtime's publish step.
async fn write_mail_custody(seat: &Seat, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
    use fauna_core::data::{MailCredential, MailCredentialKind, MsekFingerprint, Timestamp};
    use fauna_core::mail_rows::MailStateRow;
    use fauna_core::secret::SecretArray32;
    let msek = SecretArray32::new(ACCOUNT_MSEK);
    let plane = seat.plane(rpc, keys);
    let now = Timestamp(1_700_000_000_000_000);
    assert!(
        fauna_sync_engine::mail_rows::write_mail_state(
            &seat.store,
            &plane,
            &MailStateRow {
                msek: Some(msek.clone()),
                mail_enabled: Some(true),
                ..MailStateRow::default()
            },
            now,
        )
        .await
        .unwrap(),
        "the MSEK is a new row"
    );
    assert!(
        fauna_sync_engine::mail_rows::put_credential(
            &seat.store,
            &plane,
            &MailCredential {
                credential_id: "default".into(),
                display_name: "Default".into(),
                kind: MailCredentialKind::Plain,
                secret: b"correct horse battery staple".to_vec().into(),
                created_at: 1_700_000_000,
                updated_at: Timestamp::default(),
                wrapped_under: Some(MsekFingerprint::of(&msek)),
                revoked_at_unix: None,
                burned: None,
            },
            now,
        )
        .await
        .unwrap(),
        "the credential is a new row"
    );
    plane.publish_pending().await.unwrap();
}

/// The rotation keys `seat` reads back through the production door's fold.
async fn rotation_keys_held(seat: &Seat) -> Vec<fauna_core::data::AtprotoRotationKey> {
    fauna_sync_engine::atproto_identity_rows::read_atproto_identity(&seat.store)
        .await
        .unwrap()
        .rotation_keys
}

/// Write a tier's period keys from `seat` through the production door
/// (`subscription_rows::merge_subscriptions`), then take the runtime's
/// publish step.
async fn write_period_key(seat: &Seat, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
    let plane = seat.plane(rpc, keys);
    let (joined, moved) = fauna_sync_engine::subscription_rows::merge_subscriptions(
        &seat.store,
        &plane,
        &held_period_key(),
    )
    .await
    .unwrap();
    assert!(moved, "the period keys are new rows");
    assert_eq!(joined, held_period_key());
    plane.publish_pending().await.unwrap();
}

/// The period-key custody `seat` reads back through the production door's
/// fold.
async fn period_keys_held(seat: &Seat) -> fauna_core::data::SubscriptionsConfig {
    fauna_sync_engine::subscription_rows::read_subscriptions(&seat.store)
        .await
        .unwrap()
}

/// Write the account's folder-key custody from `seat` through the production
/// door (`folder_key_rows::merge_folder_keys`), then take the runtime's
/// publish step.
async fn write_folder_keys(seat: &Seat, rpc: &RouterRequester, keys: &AccountStateKeySchedule) {
    let plane = seat.plane(rpc, keys);
    let (joined, moved) = fauna_sync_engine::folder_key_rows::merge_folder_keys(
        &seat.store,
        &plane,
        &folder_key_custody(),
    )
    .await
    .unwrap();
    assert!(moved, "the custody is new rows");
    assert_eq!(joined, folder_key_custody());
    plane.publish_pending().await.unwrap();
}

/// The folder-key custody `seat` reads back through the production door's fold.
async fn folder_keys_held(seat: &Seat) -> fauna_core::data::FoldersConfig {
    fauna_sync_engine::folder_key_rows::read_folder_keys(&seat.store)
        .await
        .unwrap()
}

/// A writes the account's reception key; B, enrolled before, receives it; A
/// then leaves per `departure`; C enrolls after. The nest must still serve the
/// key and C must hold it, whatever became of A; A's device-endpoints row goes
/// with A.
async fn the_reception_key_after(departure: Departure) {
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = Seat::new(0).await;
    let b = Seat::new(1).await;
    a.join(&rpc, &keys).await;
    b.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    // Generation 1 is minted by the first data write (A's endpoints), and
    // both seats key it.
    settle(&[&a, &b], &rpc, &keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    let endpoints = live_v2_rows_of(&rpc, &a.id()).await;
    assert_eq!(endpoints.len(), 1, "A's device-endpoints row, sealed v2");
    let reception = write_reception_key(&a, &rpc, &keys).await;
    settle(&[&a, &b], &rpc, &keys, 2).await;
    assert_eq!(
        reception_keys_held(&b).await,
        1,
        "B receives the account's key"
    );

    match departure {
        Departure::Stays => {}
        Departure::SignsOut => a.sign_out(&rpc, &keys).await,
        Departure::RemovedBySibling => {
            b.plane(&rpc, &keys)
                .put(
                    &ItemId {
                        kind: KIND_DEVICE_SET.into(),
                        key: hex::encode(a.id()),
                    },
                    removal_value(b.id()),
                    None,
                )
                .await
                .unwrap();
            rpc.state
                .db
                .revoke_device_grant(&ACTOR, &a.id())
                .await
                .unwrap();
        }
    }
    let c = Seat::new(2).await;
    c.join(&rpc, &keys).await;
    if departure == Departure::Stays {
        settle(&[&a, &b, &c], &rpc, &keys, 3).await;
    } else {
        settle(&[&b, &c], &rpc, &keys, 5).await;
    }

    let live = live_v2_rows_of(&rpc, &a.id()).await;
    // First, on purpose: the catch-all witness. Every mutation that retires
    // the key with its writer — before any member carries it — reddens here.
    assert_eq!(
        reception_keys_held(&c).await,
        1,
        "{departure:?}: a device enrolled after A left still receives the account's reception key"
    );
    if departure == Departure::Stays {
        assert!(
            live.contains(&reception),
            "the reception key's writer stays: the nest still serves its row (A's live v2 rows: \
             {live:?})"
        );
        return;
    }
    // Clause (3)(g)'s hand-over, then clause (3)(e)'s shred: all read before
    // any assertion, so a red run names the whole chain.
    let b_serves = serves_own_row_of_kind(&rpc, &b, KIND_GROUP_RECEPTION_KEY).await;
    let shredded = is_shredded(&b, &g1).await;
    let wrap_kept = escrowed(&rpc, &g1).await;
    let state = format!(
        "B serves its own reception-key row: {b_serves}, A's reception row served: {}, \
         generation 1 shredded at B: {shredded}, its escrow wrap kept: {wrap_kept} (A's live v2 \
         rows: {live:?})",
        live.contains(&reception)
    );
    assert!(
        b_serves,
        "{departure:?}: B, the hander, re-seals the account's reception key under the new tip \
         — {state}"
    );
    assert!(
        !live.contains(&reception),
        "{departure:?}: A's generation-1 reception row is retired once B's own row carries the \
         key — {state}"
    );
    assert!(
        shredded,
        "{departure:?}: generation 1 is dataless once handed over, and shreds — {state}"
    );
    assert!(
        !wrap_kept,
        "{departure:?}: the shredded generation's escrow wrap is swept — {state}"
    );
    assert!(
        !live.contains(&endpoints[0]),
        "{departure:?}: A's device-endpoints row is device-scoped and goes with A"
    );
}

/// Does the nest serve a live row `seat` authored of logical `kind`? The
/// seat's own journal names the kind at each writer seq (the nest sees only
/// blinded item keys), and the nest's rows are matched by that coordinate.
async fn serves_own_row_of_kind(rpc: &RouterRequester, seat: &Seat, kind: &str) -> bool {
    let seqs: Vec<i64> = seat
        .store
        .scope_rows(ACCOUNT_STATE_FLEET_SCOPE, &WriterId(seat.id()), 0, u32::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter(|row| {
            matches!(
                &row.item,
                fauna_account_store::types::ItemRef::StateKey { kind: k, .. } if k == kind
            )
        })
        .map(|row| row.seq as i64)
        .collect();
    let fs = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
        .await
        .unwrap()
        .expect("the fleet scope exists");
    rpc.state
        .db
        .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
        .iter()
        .any(|r| {
            r.origin_writer.as_deref() == Some(seat.id().as_slice())
                && r.origin_seq.is_some_and(|s| seqs.contains(&s))
        })
}

/// Does `seat` hold `generation`'s key — in its bundle, or by its own wrap?
async fn holds_generation_key(seat: &Seat, generation: &[u8; 32]) -> bool {
    let in_bundle = seat.custody.0.lock().unwrap().contains_key(generation);
    in_bundle
        || fauna_sync_engine::generation_tip::generation_key_for(
            &seat.store,
            generation,
            &seat.key,
            Some(&seat.custody),
            None,
        )
        .await
        .unwrap()
        .is_some()
}

/// Does `seat`'s merged state read `generation` as `Shredded`?
async fn is_shredded(seat: &Seat, generation: &[u8; 32]) -> bool {
    seat.store
        .state(KIND_GENERATION_MINT, &fauna_core::hex32::encode(generation))
        .await
        .unwrap()
        .and_then(|e| {
            fauna_core::encoding::canonical_decode::<fauna_core::generation::GenerationMintRecord>(
                &e.value,
            )
            .ok()
        })
        .is_some_and(|r| {
            matches!(
                r,
                fauna_core::generation::GenerationMintRecord::Shredded { .. }
            )
        })
}

/// **The control.** A stays: its device-endpoints row and the account's
/// reception key are both served, and C, enrolled later, holds the key.
#[tokio::test]
async fn the_reception_key_reaches_a_later_device_while_its_writer_stays() {
    the_reception_key_after(Departure::Stays).await;
}

/// **Sign-out severs the device, not the account** (clause (4)): A's own
/// device-endpoints row is retired; the account's reception key A wrote is
/// handed over — B, the hander (clause (3)(g)), re-seals it under the tip the
/// sign-out's mint produced and retires A's generation-1 row once its own is
/// published — generation 1 then shreds and its escrow wrap is swept, and C,
/// enrolled after A signed out, holds the key from B's row.
///
/// Red-verified, with the removal arm: restore the catch-all v2 filter in
/// `Retirer::retire_device_scoped_rows` (both departure legs) and both pins
/// redden on their first assertion, the catch-all witness — C holds no
/// reception key.
#[tokio::test]
async fn a_sign_out_retires_its_endpoints_and_keeps_the_accounts_reception_key() {
    the_reception_key_after(Departure::SignsOut).await;
}

/// **Removal severs the device, not the account** (clause (3)(d)): B removes
/// A from the devices page; the passes retire A's device-endpoints row, hand
/// the account's reception key A wrote over to B's own row (clause (3)(g)),
/// retire A's row of it, and shred generation 1; C, enrolled after, holds the
/// key.
#[tokio::test]
async fn a_removal_retires_its_endpoints_and_keeps_the_accounts_reception_key() {
    the_reception_key_after(Departure::RemovedBySibling).await;
}

/// **A single-device account signs out and back in, and keeps its keys.** A
/// is the account's only device: it mints generation 1, writes the reception
/// key under it, and signs out; A′ — the next sign-in, a fresh device key —
/// enrolls and runs the pump. A′ cannot key generation 1 (no member is left to
/// top it up; the escrow wrap is the recovery path), so the reception key row
/// must survive at the nest AND generation 1 must stay un-shredded with its
/// escrow wrap at the holder — the one thing that can ever re-open it.
///
/// Red-verified per arm: restore the catch-all v2 filter in `sever_self` (or
/// in `Retirer::retire_removed_device`, which A′ runs over A) and A's rows
/// are retired, generation 1 reads dataless, A′ (the minimum member, the
/// minter removed) shreds it, and the receipt retire's sweep deletes its
/// escrow wrap — the failure message reads all three.
#[tokio::test]
async fn a_single_device_sign_out_and_back_in_keeps_the_reception_key_and_its_escrow_wrap() {
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = Seat::new(0).await;
    let a_id = a.id();
    a.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    let reception = write_reception_key(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 1).await;
    assert!(
        escrowed(&rpc, &g1).await,
        "generation 1's escrow wrap is with the holder before the sign-out"
    );

    a.sign_out(&rpc, &keys).await;
    drop(a);
    let a2 = Seat::new(1).await;
    a2.join(&rpc, &keys).await;
    settle(&[&a2], &rpc, &keys, 3).await;

    // All three read before any assertion, so a red run names the whole
    // chain: the row retired, the generation shredded, the wrap swept.
    let live = live_v2_rows_of(&rpc, &a_id).await;
    let shredded = a2
        .store
        .state(KIND_GENERATION_MINT, &fauna_core::hex32::encode(&g1))
        .await
        .unwrap()
        .and_then(|e| {
            fauna_core::encoding::canonical_decode::<fauna_core::generation::GenerationMintRecord>(
                &e.value,
            )
            .ok()
        })
        .is_some_and(|r| {
            matches!(
                r,
                fauna_core::generation::GenerationMintRecord::Shredded { .. }
            )
        });
    let wrap_kept = escrowed(&rpc, &g1).await;
    let state = format!(
        "reception row served: {}, generation 1 shredded: {shredded}, its escrow wrap kept: \
         {wrap_kept} (A's live v2 rows: {live:?})",
        live.contains(&reception)
    );
    assert!(
        live.contains(&reception),
        "the account's reception key outlives the device that wrote it — {state}"
    );
    assert!(
        !shredded,
        "generation 1 still seals a live row: it must not shred — {state}"
    );
    assert!(
        wrap_kept,
        "generation 1's escrow wrap — the only way back to the reception key — stays — {state}"
    );
}

/// **The let-go — a dead generation's rows are retired by the user's act, and
/// a live sibling's are untouched** (`account-data-taxonomy.md` § The
/// generation machinery → *Fleet-scope reclamation*, clause (3)(j)).
///
/// A, the account's only device, seals the account's reception, rotation,
/// period and folder keys under generation 1 and is gone; A′ signs in seedless
/// and keys nothing of it. While the holder still wraps generation 1 it is NOT
/// dead — a later sign-in with the seed would recover it — and A′ reads no
/// dead generation. Then the holder loses the wrap: the state a loss inside
/// the mint-to-deposit window leaves (no device keys generation 1, no holder
/// wraps it), reached here by taking the wrap at the holder. A′ now reads
/// generation 1 dead, with its live-row count and mint stamp; B, a sibling
/// enrolled beside A′, keeps rows under A′'s own generation. A′ lets
/// generation 1 go: every row resting under it leaves the feed, the live
/// fleet entries drop by at least those rows, no wrap of it is left at the
/// holder, the read answers empty — and every row under any other generation
/// is exactly where it was. B's read answers empty too: its copies of the
/// retired rows are gone from the nest's listing, so it forgets them.
#[tokio::test]
async fn a_dead_generation_is_let_go_by_the_users_act_and_a_live_siblings_rows_stay() {
    use fauna_sync_engine::generation_let_go::let_go;
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = Seat::new(0).await;
    a.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    write_reception_key(&a, &rpc, &keys).await;
    write_rotation_key(&a, &rpc, &keys).await;
    write_period_key(&a, &rpc, &keys).await;
    write_folder_keys(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 1).await;
    let minted_at_ms = a
        .store
        .state(KIND_GENERATION_MINT, &hex::encode(g1))
        .await
        .unwrap()
        .and_then(|e| fauna_sync_engine::generation_escrow_recover::minted_core(&e))
        .map(|(_, core)| core.minted_at_ms);
    a.sign_out(&rpc, &keys).await;
    drop(a);

    let a2 = Seat::new(1).await;
    a2.join(&rpc, &keys).await;
    let b = Seat::new(2).await;
    b.join(&rpc, &keys).await;
    settle(&[&a2, &b], &rpc, &keys, 3).await;
    assert!(
        escrowed(&rpc, &g1).await && !holds_generation_key(&a2, &g1).await,
        "the fixture: A′ keys nothing of generation 1, and the holder still wraps it"
    );
    assert_eq!(
        dead_of(&a2, &rpc, &keys).await,
        Vec::new(),
        "a generation the holder still wraps is not dead — the seed would recover it"
    );

    // The holder loses the wrap: no device keys generation 1, no holder wraps it.
    rpc.state
        .db
        .delete_generation_escrow_wraps(&ACTOR, &g1)
        .await
        .unwrap();
    settle(&[&a2, &b], &rpc, &keys, 1).await;
    let before = live_rows_by_generation(&rpc).await;
    let under_g1 = before.get(&g1).map_or(0, Vec::len);
    let others_before: Vec<_> = before.iter().filter(|(g, _)| **g != g1).collect();
    let live_before = live_fleet_entries(&rpc).await;
    let read = dead_of(&a2, &rpc, &keys).await;
    assert!(under_g1 > 0, "the fixture: rows rest under generation 1");
    assert_eq!(
        read.iter().map(|d| d.generation_id).collect::<Vec<_>>(),
        vec![g1],
        "A′ reads generation 1 dead and nothing else — {read:?}"
    );
    assert_eq!(read[0].live_rows, under_g1 as u64, "its live-row count");
    assert!(
        read[0].minted_at_ms.is_none() || read[0].minted_at_ms == minted_at_ms,
        "its mint stamp, where A′ carries the record — {read:?} vs {minted_at_ms:?}"
    );

    let report = let_go(
        &a2.store,
        &a2.plane(&rpc, &keys),
        a2.trust(),
        &a2.key,
        &[g1].into_iter().collect(),
    )
    .await
    .unwrap();
    settle(&[&a2, &b], &rpc, &keys, 2).await;
    let after = live_rows_by_generation(&rpc).await;
    let others_after: Vec<_> = after.iter().filter(|(g, _)| **g != g1).collect();
    let live_after = live_fleet_entries(&rpc).await;
    let state = format!(
        "{report:?}; rows under g1 {under_g1} -> {}; live fleet entries {live_before} ->          {live_after}; g1 escrowed: {}",
        after.get(&g1).map_or(0, Vec::len),
        escrowed(&rpc, &g1).await,
    );
    assert_eq!(
        report.let_go,
        vec![g1],
        "the act takes generation 1 — {state}"
    );
    assert!(
        !after.contains_key(&g1),
        "no row rests under generation 1 any more — {state}"
    );
    assert!(
        live_after + under_g1 <= live_before,
        "the live fleet entries drop by at least the rows that rested under it — {state}"
    );
    assert!(
        !escrowed(&rpc, &g1).await,
        "no wrap of it is left — {state}"
    );
    assert_eq!(
        others_after, others_before,
        "every row under any other generation — the live siblings' — is untouched — {state}"
    );
    assert_eq!(
        dead_of(&a2, &rpc, &keys).await,
        Vec::new(),
        "the read answers empty — {state}"
    );
    assert_eq!(
        dead_of(&b, &rpc, &keys).await,
        Vec::new(),
        "and so does the sibling's — {state}"
    );
}

/// The let-go's dead read, as `seat`'s runtime answers it between passes: on
/// the plane whose reconcile banked the nest's listing.
async fn dead_of(
    seat: &Seat,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
) -> Vec<fauna_sync_engine::generation_let_go::DeadGeneration> {
    let plane = seat.plane(rpc, keys);
    plane.reconcile().await.unwrap();
    fauna_sync_engine::generation_let_go::dead_generations(
        &seat.store,
        &plane,
        seat.trust(),
        &seat.key,
    )
    .await
    .unwrap()
}

/// **The same sign-out and sign-in, healed** (*Escrow recovery*). A′ signs in
/// with the identity seed, so its pump's recovery leg keys generation 1 from
/// the wrap the holder kept: A′ reads the account's reception key again, and B
/// — enrolled afterwards, holding no seed, never touching the escrow door — is
/// topped up by A′ like any sibling, which it can only be if A′ really holds
/// the key. Recovery reads the wrap, it never deletes it: the wrap goes only
/// with generation 1's shred, once A′ has handed the key over (next pin).
///
/// The same read-back holds the account's senior ATProto rotation key, a
/// subscription tier's period keys and its shared-folder content keys,
/// `GenerationTip` rows whose loss nothing
/// could repair.
///
/// Red-verified: build A′ with `Seat::new` (no recovery leg) and A′ holds no
/// reception key, its bundle is empty, and B is never healed.
#[tokio::test]
async fn a_single_device_sign_in_recovers_its_generation_from_escrow_and_holds_the_reception_key() {
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = Seat::new(0).await;
    a.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    write_reception_key(&a, &rpc, &keys).await;
    write_rotation_key(&a, &rpc, &keys).await;
    write_period_key(&a, &rpc, &keys).await;
    write_folder_keys(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 1).await;
    a.sign_out(&rpc, &keys).await;
    drop(a);

    let a2 = Seat::seed_holding(1).await;
    a2.join(&rpc, &keys).await;
    settle(&[&a2], &rpc, &keys, 3).await;

    // The senior rotation key exists nowhere but its tip-sealed row: a fresh
    // device holding the seed reads it back through generation 1's escrow.
    assert_eq!(
        rotation_keys_held(&a2).await,
        vec![senior_rotation_key()],
        "A′ reads the account's senior rotation key again"
    );
    // So do a tier's period keys, the rotated-out one included — an archival
    // mint for a new subscriber reads every period.
    assert_eq!(
        period_keys_held(&a2).await,
        held_period_key(),
        "A′ reads the account's tier period keys again"
    );
    // So do the shared-folder content keys, every generation of them.
    assert_eq!(
        folder_keys_held(&a2).await,
        folder_key_custody(),
        "A′ reads the account's folder-key custody again"
    );
    let recovered = a2.custody.0.lock().unwrap().contains_key(&g1);
    let held = reception_keys_held(&a2).await;
    let state =
        format!("generation 1 in A′'s bundle: {recovered}, reception keys A′ holds: {held}");
    assert!(
        recovered,
        "A′ keys generation 1 from its escrow wrap and the key rides its retained bundle — {state}"
    );
    assert_eq!(
        held, 1,
        "A′ reads the account's reception key again — {state}"
    );
    // Recovery only reads the wrap. Once A′ keys generation 1 it is its
    // hander, so the hand-over may already have shredded it — and the shred's
    // sweep is the one thing that takes the wrap (the next pin pins that
    // chain).
    assert!(
        escrowed(&rpc, &g1).await || is_shredded(&a2, &g1).await,
        "recovery reads the wrap; only a shredded generation's sweep deletes it"
    );

    let b = Seat::new(2).await;
    b.join(&rpc, &keys).await;
    settle(&[&a2, &b], &rpc, &keys, 3).await;
    assert_eq!(
        reception_keys_held(&b).await,
        1,
        "B, seedless, is topped up by A′ and reads the reception key"
    );
}

/// **The recovered generation is handed over, then shredded** (clause (3)(g)
/// after *Escrow recovery*). The same single-device sign-out and seed-holding
/// sign-in: once A′ keys generation 1 from its escrow wrap it lists it in its
/// reach and is thereby its hander, so it re-seals the account's reception key
/// under the tip its own sign-in minted, retires A's generation-1 row of it,
/// and — generation 1 dataless — shreds it and sweeps its escrow wrap.
#[tokio::test]
async fn a_single_device_sign_in_hands_the_recovered_generation_over_then_shreds_it() {
    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = Seat::new(0).await;
    let a_id = a.id();
    a.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    let reception = write_reception_key(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 1).await;
    a.sign_out(&rpc, &keys).await;
    drop(a);

    let a2 = Seat::seed_holding(1).await;
    a2.join(&rpc, &keys).await;
    settle(&[&a2], &rpc, &keys, 5).await;

    let held = reception_keys_held(&a2).await;
    let a2_serves = serves_own_row_of_kind(&rpc, &a2, KIND_GROUP_RECEPTION_KEY).await;
    let live = live_v2_rows_of(&rpc, &a_id).await;
    let shredded = is_shredded(&a2, &g1).await;
    let wrap_kept = escrowed(&rpc, &g1).await;
    let state = format!(
        "reception keys A′ holds: {held}, A′ serves its own reception-key row: {a2_serves}, A's \
         reception row served: {}, generation 1 shredded: {shredded}, its escrow wrap kept: \
         {wrap_kept} (A's live v2 rows: {live:?})",
        live.contains(&reception)
    );
    assert_eq!(held, 1, "A′ holds the account's reception key — {state}");
    assert!(
        a2_serves,
        "A′, generation 1's hander, re-seals the key under the newer tip — {state}"
    );
    assert!(
        !live.contains(&reception),
        "A's generation-1 row of the key is retired once A′'s own row is published — {state}"
    );
    assert!(shredded, "generation 1 reads Shredded — {state}");
    assert!(
        !wrap_kept,
        "its escrow wrap is swept at the holder — {state}"
    );
}

/// **The succession rider — a successor's floor is its seed alone**
/// (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the succession
/// rider; `config-dissolution.md` § The `__config` dissolution schedule →
/// *The closure order*, step (4); `generation_reescrow` module docs).
///
/// Each identity is on the schedule its own seed derives — the production
/// shape ("generation 0 re-derives under the successor seed's `BackupKey`"),
/// so a successor's replica opens none of the generation-0 rows its
/// predecessor sealed, the fleet-only machinery rows included. What crosses
/// is the generation-1 KEY (the slot carriage) and that generation's MINT
/// RECORD (the walk's carry, under the predecessor's mint-kind keys); the
/// standing passes do the rest. [`the_succession`] runs the flow up to S's
/// re-escrow and pins the carry; here S's machine is then lost, and F — a
/// fresh device holding only the successor seed, never a member of generation
/// 1 and with no sibling left to top it up — reads every pre-succession
/// tip-sealed row from escrow alone.
///
/// What this deliberately does not vary: the nest's authenticated actor (this
/// suite's constant `ACTOR`).
///
/// Red-verified: with the walk's mint-record carry off, S holds no
/// generation-mint row, the re-escrow pass reports `Current`, and F recovers
/// nothing of generation 1.
#[tokio::test]
async fn a_successor_on_its_own_generation_0_schedule_reads_a_pre_succession_tip_sealed_row() {
    let Succession { rpc, s, g1, .. } = the_succession().await;
    drop(s); // S's machine is lost.

    // ── F: a fresh device, the successor seed and nothing else.
    let keys = schedule_of(&SUCCESSOR_SEED);
    let f = Seat::successor(2, true).await;
    f.join(&rpc, &keys).await;
    settle(&[&f], &rpc, &keys, 3).await;
    assert!(
        f.custody.0.lock().unwrap().contains_key(&g1),
        "F keys g1 from the successor-targeted escrow wrap"
    );
    assert_reads_every_pre_succession_row(&f, "F, holding nothing but the seed").await;
}

/// **Forward sealing, the top-up and the reclamation across a succession**
/// (the rider → *Forward sealing: trigger (d) is no mint of its own* and
/// *What the standing passes then do, unchanged*).
///
/// S keeps running after its re-escrow, beside D — a seedless successor
/// device with no seed, no carried key and no predecessor material. S's first
/// tip-sealed origination (its own device-endpoints row, in its next full
/// pass) finds no sealing candidate — the carried generation's member set
/// names the predecessor's device — and mints by trigger (a), exactly once,
/// naming the carried generation as a parent. S's top-up pass hands D the
/// carried generation's key, and D reads the reception key. Over further
/// passes the reclamation pass hands the pre-succession account-level rows
/// over to the successor's generation and lets go of the predecessor device's
/// own endpoints row (clause (3)(g)'s let-go arm), so the carried generation
/// shreds and its escrow wrap is swept. Then every device is lost and a fresh
/// seed-only device still reads every pre-succession row.
///
/// Red-verified: with the carry off, S's first mint is parentless and D is
/// never handed generation 1; with the let-go arm off, the predecessor
/// device's endpoints row stays live under the carried generation, which
/// never shreds.
#[tokio::test]
async fn a_successors_first_mint_supersedes_the_carried_generation_and_a_late_seed_only_device_reads_every_pre_succession_tip_sealed_row()
 {
    let Succession {
        rpc,
        s,
        g1,
        a_id,
        predecessor_ids,
    } = the_succession().await;
    let keys = schedule_of(&SUCCESSOR_SEED);

    let d = Seat::successor(4, false).await;
    d.join(&rpc, &keys).await;
    // D walks the feed before S's next pass, so it relays the predecessor's
    // rows before the predecessor arm retires them: bound (γ) is measured,
    // not dodged.
    d.plane(&rpc, &keys).walk().await.unwrap();
    // D's possession of g1 is read after every round: once nothing rests
    // under g1 the let-go arm lets it shred, and every device drops a
    // shredded generation's key — so the top-up is witnessed while it holds.
    let mut d_was_handed_g1 = false;
    let mut rounds = Vec::new();
    for _ in 0..3 {
        settle(&[&s, &d], &rpc, &keys, 1).await;
        let held = holds_generation_key(&d, &g1).await;
        d_was_handed_g1 |= held;
        rounds.push((held, is_shredded(&d, &g1).await));
    }

    // (c) Exactly one generation minted under the successor, by S, over g1.
    let minted: Vec<(String, GenerationMintRecord)> = s
        .store
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .map(|e| {
            let record = fauna_core::encoding::canonical_decode(&e.value).unwrap();
            (e.key, record)
        })
        .collect();
    let cores: Vec<(&str, &fauna_core::generation::MintCore)> = minted
        .iter()
        .map(|(key, record)| match record {
            GenerationMintRecord::Minted { core, .. }
            | GenerationMintRecord::Shredded { core, .. } => (key.as_str(), core),
        })
        .collect();
    let own: Vec<_> = cores
        .iter()
        .filter(|(key, _)| *key != hex::encode(g1))
        .collect();
    assert_eq!(
        own.len(),
        1,
        "S's first tip-sealed write minted exactly one generation: {cores:?}"
    );
    let (g2_hex, g2_core) = own[0];
    assert_eq!(g2_core.minter, s.id(), "S minted it");
    assert!(
        g2_core.parents.contains(&g1),
        "the successor's first generation names the carried one as a parent: {g2_core:?}"
    );
    let g2 = fauna_core::hex32::decode(g2_hex).unwrap();

    // (d) D — no seed, no carried key, no predecessor keys — was topped up.
    assert!(
        d_was_handed_g1,
        "S's top-up pass handed D the carried generation's key — per round (D holds g1, D \
         reads g1 shredded): {rounds:?}"
    );
    assert_eq!(
        reception_keys_held(&d).await,
        1,
        "D reads the pre-succession reception key"
    );

    // What reclamation does with the superseded carried generation, over
    // further full passes: the hand-over, then the let-go arm's retire of the
    // predecessor device's endpoints row, the shred in the pass after it, and
    // the escrow sweep in the pass after the gate clears the receipt retire.
    settle(&[&s, &d], &rpc, &keys, 4).await;
    for _ in 0..4 {
        if is_shredded(&s, &g1).await && is_shredded(&d, &g1).await && !escrowed(&rpc, &g1).await {
            break;
        }
        settle(&[&s, &d], &rpc, &keys, 1).await;
    }
    let g1_record: GenerationMintRecord = fauna_core::encoding::canonical_decode(
        &s.store
            .state(KIND_GENERATION_MINT, &hex::encode(g1))
            .await
            .unwrap()
            .expect("S still holds g1's mint row")
            .value,
    )
    .unwrap();
    let sealed_under = live_rows_by_generation(&rpc).await;
    let under = |g: &[u8; 32]| sealed_under.get(g).cloned().unwrap_or_default();
    let a_endpoints = s
        .store
        .state(KIND_DEVICE_ENDPOINTS, &hex::encode(a_id))
        .await
        .unwrap();
    let measured = format!(
        "g1 shredded: {}; g1 escrowed: {}; live v2 rows under g1: {} (by the predecessor's \
         device: {}), under g2: {}, under any other: {}; S holds the predecessor device's \
         endpoints entry: {:?}",
        matches!(g1_record, GenerationMintRecord::Shredded { .. }),
        escrowed(&rpc, &g1).await,
        under(&g1).len(),
        under(&g1)
            .iter()
            .filter(|w| w.as_deref() == Some(a_id.as_slice()))
            .count(),
        under(&g2).len(),
        sealed_under
            .iter()
            .filter(|(g, _)| **g != g1 && **g != g2)
            .map(|(_, rows)| rows.len())
            .sum::<usize>(),
        a_endpoints.map(|e| e.tombstone),
    );
    // `account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    // reclamation*, clause (3)(g): the hand-over arm re-seals the
    // account-level rows under the tip, and the let-go arm retires the
    // predecessor DEVICE's own device-endpoints row — its writer is no
    // member under the successor and no removal names it — so nothing rests
    // under the carried generation, which then shreds and is swept from the
    // holder.
    assert!(
        under(&g1).is_empty(),
        "no live row rests under the carried generation — {measured}"
    );
    assert!(
        under(&g2).len() >= 5,
        "the pre-succession account-level rows rest under the successor's generation — \
         {measured}"
    );
    assert!(
        is_shredded(&s, &g1).await && is_shredded(&d, &g1).await,
        "both successor seats read the carried generation shredded — {measured}"
    );
    assert!(
        !escrowed(&rpc, &g1).await,
        "the holder keeps no wrap of the carried generation — {measured}"
    );

    // Clause (3)(i), the predecessor arm: S, which attests the predecessor,
    // opens the predecessor's generation-0 machinery rows under the retired
    // keys and retires them, so with clause (3)(g)'s let-go of the endpoints
    // row no row the predecessor's device wrote is left on the feed. D, which
    // holds no predecessor material, retires nothing and keeps the relay
    // copies it walked before the retires (bound (γ)).
    let left = live_rows_written_by(&rpc, &predecessor_ids).await;
    let d_relay = relay_rows_written_by(&d, &rpc, &keys, &predecessor_ids).await;
    eprintln!(
        "clause (3)(i), one predecessor device: live rows it wrote {left:?}; D's relay copies {d_relay}"
    );
    assert!(
        left.is_empty(),
        "no live row of the predecessor's device stays on the feed (form v2?: {:?}); D still \
         relays {d_relay} — {measured}",
        left.iter().map(|(_, v2)| *v2).collect::<Vec<_>>()
    );

    // The invariant, whatever reclamation did: every device is lost, and a
    // fresh seed-only device joined afterwards reads every pre-succession
    // account-level row.
    drop(s);
    drop(d);
    let f = Seat::successor(2, true).await;
    f.join(&rpc, &keys).await;
    settle(&[&f], &rpc, &keys, 3).await;
    assert_reads_every_pre_succession_row(&f, &format!("a late seed-only device ({measured})"))
        .await;
}

/// **Clause (3)(i) with a predecessor fleet of three devices**
/// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*, clause (3)(i); measured at 12 live rows before the arm and
/// clause (3)(g)'s let-go were built).
///
/// Three predecessor devices on one generation, every grant revoked at the
/// ceremony; S (attesting the predecessor, holding g1) and D (seedless) run
/// the standing passes. Every row a predecessor device wrote — its
/// enrollment, its reach and its endpoints row, and the fleet's escrow
/// target, mint and receipt rows — leaves the feed.
///
/// Red-verified: with the fleet plane handed no retired machinery keys, the
/// 2N + 3 = 9 form-v1 machinery rows stay live.
#[tokio::test]
async fn a_three_device_predecessor_fleet_leaves_no_row_on_the_successors_feed() {
    let Succession {
        rpc,
        s,
        g1,
        predecessor_ids,
        ..
    } = the_succession_of(3).await;
    assert_eq!(predecessor_ids.len(), 3);
    let keys = schedule_of(&SUCCESSOR_SEED);
    let d = Seat::successor(4, false).await;
    d.join(&rpc, &keys).await;
    // As in the one-device flow: D relays the predecessor's rows before the
    // arm retires them (bound (γ)).
    d.plane(&rpc, &keys).walk().await.unwrap();
    settle(&[&s, &d], &rpc, &keys, 7).await;
    for _ in 0..6 {
        if live_rows_written_by(&rpc, &predecessor_ids)
            .await
            .is_empty()
        {
            break;
        }
        settle(&[&s, &d], &rpc, &keys, 1).await;
    }
    let left = live_rows_written_by(&rpc, &predecessor_ids).await;
    let d_relay = relay_rows_written_by(&d, &rpc, &keys, &predecessor_ids).await;
    eprintln!(
        "clause (3)(i), three predecessor devices: live rows they wrote {left:?}; D's relay copies {d_relay}"
    );
    assert!(
        left.is_empty(),
        "no live row of a predecessor device stays: {} left ({} form v1), g1 shredded at S: {}; \
         D still relays {d_relay}",
        left.len(),
        left.iter().filter(|(_, v2)| !v2).count(),
        is_shredded(&s, &g1).await,
    );
}

/// **Clause (3)(i)'s kept mint row** — a predecessor's mint record of a
/// generation no successor device carries is the one machinery row the arm
/// keeps: it is what a device that brings the generation's key later still
/// needs (the rider's residual (iii)).
///
/// E is the `(e)` shape of [`the_succession`]: a successor device that
/// attests the predecessor and holds no generation-1 key. Run alone, its
/// passes retire every other generation-0 machinery row the predecessor's
/// device wrote, and keep the mint row live.
///
/// Red-verified: with E's plane handed no retired machinery keys, the
/// enrollment, reach, escrow-target and receipt rows stay live too.
#[tokio::test]
async fn a_predecessors_mint_row_no_successor_device_carries_is_kept() {
    let rpc = nest_with_escrow().await;
    let keys = &schedule_of(&SEAT_IDENTITY_SEED);
    let a = Seat::new(0).await;
    a.join(&rpc, keys).await;
    publish_identity_escrow_target(&a, &rpc, keys).await;
    settle(&[&a], &rpc, keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    let a_id = a.id();
    drop(a);
    rpc.state
        .db
        .revoke_device_grant(&ACTOR, &a_id)
        .await
        .unwrap();

    let keys = &schedule_of(&SUCCESSOR_SEED);
    let e = Seat::successor(3, false).await.attesting_the_predecessor();
    e.join(&rpc, keys).await;
    let target = fauna_sync_engine::generation_mint::escrow_target_entry(&SUCCESSOR_SEED).unwrap();
    e.plane(&rpc, keys)
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: target.key.clone(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();
    settle(&[&e], &rpc, keys, 4).await;

    let live = live_rows_written_by(&rpc, &[a_id]).await;
    let v1 = live.iter().filter(|(_, v2)| !v2).count();
    let measured = format!(
        "live rows A wrote: {} ({v1} form v1); E holds g1's mint record: {}",
        live.len(),
        e.store
            .state(KIND_GENERATION_MINT, &hex::encode(g1))
            .await
            .unwrap()
            .is_some(),
    );
    eprintln!("clause (3)(i), the kept mint row: {measured}");
    assert!(
        !holds_generation_key(&e, &g1).await,
        "E keys nothing of g1 — {measured}"
    );
    assert_eq!(
        v1, 1,
        "of A's generation-0 machinery only the mint row of the uncarried generation stays \
         live — {measured}"
    );
    // The one form-v1 row left is the mint row: the item key it sits at is
    // the one the predecessor's schedule derives for g1's mint record.
    let mint_item = fauna_protocol::merge_policy::kind_keys(
        &schedule_of(&SEAT_IDENTITY_SEED),
        KIND_GENERATION_MINT,
    )
    .unwrap()
    .item_key(hex::encode(g1).as_bytes());
    assert_eq!(
        e.plane(&rpc, keys)
            .relay_rows_at(&mint_item)
            .await
            .unwrap()
            .iter()
            .map(|r| r.writer.0)
            .collect::<Vec<_>>(),
        vec![a_id],
        "E still relays the predecessor's mint row for g1 — {measured}"
    );
}

/// **Clause (3)(i)'s late carrier** — once the arm has retired the
/// predecessor's mint row for the carried generation (licence 1: S's own row,
/// the walk's carry, is published), a successor device that arrives later
/// with that generation's key and attests the predecessor still ends with the
/// mint record in merged state — from S's re-authored row — and reads every
/// pre-succession row.
///
/// D, a seedless member, keeps the carried generation in use while S runs
/// its pass, so the retire is read before the generation shreds: once it does,
/// the shred retires every mint row of it, S's own included, and a device
/// arriving after that has no record to read and needs none.
///
/// Red-verified: with the fleet plane handed no retired machinery keys, S
/// still relays the predecessor's mint row after its pass.
#[tokio::test]
async fn a_late_carrier_reads_the_mint_record_after_the_predecessors_row_is_retired() {
    let Succession {
        rpc, s, g1, a_id, ..
    } = the_succession().await;
    let keys = schedule_of(&SUCCESSOR_SEED);
    // D, a seedless member whose reach does not hold S's new tip yet, keeps
    // g1 in use, so S's pass reaches the arm before anything shreds.
    let d = Seat::successor(4, false).await;
    d.join(&rpc, &keys).await;
    s.pass(&rpc, &keys).await;
    assert!(
        !is_shredded(&s, &g1).await,
        "g1 is not shredded yet, so licence 1 (S's own published row), not licence 2, is what \
         the arm retired the predecessor's mint row on"
    );
    let mint_item = fauna_protocol::merge_policy::kind_keys(
        &schedule_of(&SEAT_IDENTITY_SEED),
        KIND_GENERATION_MINT,
    )
    .unwrap()
    .item_key(hex::encode(g1).as_bytes());
    let a_live = live_rows_written_by(&rpc, &[a_id]).await;
    let s_relays_a_mint = s
        .plane(&rpc, &keys)
        .relay_rows_at(&mint_item)
        .await
        .unwrap()
        .len();
    assert_eq!(
        s_relays_a_mint, 0,
        "S retired the predecessor's g1 mint row behind its own carried row and forgot its \
         relay copy (A's live rows: {a_live:?})"
    );
    assert!(
        a_live.iter().all(|(_, v2)| *v2),
        "no form-v1 row of A's is live: {a_live:?}"
    );

    // L: the successor on another machine that held g1, arriving now.
    let l = Seat::successor(5, false).await.attesting_the_predecessor();
    let g1_key = *s.custody.0.lock().unwrap().get(&g1).expect("S keys g1");
    l.custody.0.lock().unwrap().insert(g1, g1_key);
    l.join(&rpc, &keys).await;
    l.plane(&rpc, &keys).walk().await.unwrap();
    assert!(
        l.store
            .state(KIND_GENERATION_MINT, &hex::encode(g1))
            .await
            .unwrap()
            .is_some(),
        "L's first walk merges g1's mint record, from S's re-authored row"
    );
    settle(&[&s, &d, &l], &rpc, &keys, 3).await;
    assert_reads_every_pre_succession_row(&l, "L, the late carrier").await;
}

/// What a succession leaves U, a successor device whose machine holds BOTH
/// seeds and no generation key: the predecessor's only device A is gone with
/// its bundle (the stolen only device), and U restored the predecessor's seed
/// from the phrase and ran the ceremony before any pass of its own had keyed
/// generation 1. The ceremony keeps the predecessor-keyed escrow wraps (the
/// kept wrap — `owner-key-material.md` § Path A-sibling-2 → *Rotation*, the
/// succession rider). U then runs six full passes as the successor.
struct UnkeyedSuccessor {
    rpc: RouterRequester,
    u: Seat,
    g1: [u8; 32],
    /// The state after U's passes, for an assertion's message.
    measured: String,
}

async fn an_unkeyed_successor() -> UnkeyedSuccessor {
    let rpc = nest_with_escrow().await;
    let keys = &schedule_of(&SEAT_IDENTITY_SEED);
    let a = Seat::new(0).await;
    a.join(&rpc, keys).await;
    publish_identity_escrow_target(&a, &rpc, keys).await;
    settle(&[&a], &rpc, keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    write_reception_key(&a, &rpc, keys).await;
    write_rotation_key(&a, &rpc, keys).await;
    write_period_key(&a, &rpc, keys).await;
    write_folder_keys(&a, &rpc, keys).await;
    write_mail_custody(&a, &rpc, keys).await;
    settle(&[&a], &rpc, keys, 1).await;
    assert!(
        escrowed(&rpc, &g1).await,
        "g1 is escrowed before the succession"
    );
    let a_id = a.id();
    drop(a); // A's machine is gone, and generation 1's only device key with it.

    // The ceremony: the predecessor's device holds no grant on the successor's
    // account, and the holder keeps every wrap it holds.
    rpc.state
        .db
        .revoke_device_grant(&ACTOR, &a_id)
        .await
        .unwrap();

    let keys = &schedule_of(&SUCCESSOR_SEED);
    let u = Seat::successor(1, true).await.attesting_the_predecessor();
    u.join(&rpc, keys).await;
    let target = fauna_sync_engine::generation_mint::escrow_target_entry(&SUCCESSOR_SEED).unwrap();
    u.plane(&rpc, keys)
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: target.key.clone(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();
    settle(&[&u], &rpc, keys, 6).await;

    let sealed_under = live_rows_by_generation(&rpc).await;
    // Read under the lock, then let the guard go before the awaits below.
    let u_keys_g1 = u.custody.0.lock().unwrap().contains_key(&g1);
    let measured = format!(
        "U keys g1: {}; g1 shredded: {}; g1 escrowed: {}; U holds g1's mint row: {}; reception \
         keys U reads: {}; live fleet entries: {}; live rows sealed under g1: {}",
        u_keys_g1,
        is_shredded(&u, &g1).await,
        escrowed(&rpc, &g1).await,
        u.store
            .state(KIND_GENERATION_MINT, &hex::encode(g1))
            .await
            .unwrap()
            .is_some(),
        reception_keys_held(&u).await,
        live_fleet_entries(&rpc).await,
        sealed_under.get(&g1).map_or(0, Vec::len),
    );
    UnkeyedSuccessor {
        rpc,
        u,
        g1,
        measured,
    }
}

/// **The kept wrap — a successor device that keys nothing recovers a
/// pre-succession generation under the retired seed**
/// (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the succession
/// rider → *The kept wrap*; `account-data-taxonomy.md` § The generation
/// machinery → *Escrow recovery*, the succession clause).
///
/// The stolen only device: the predecessor's device A sealed the account's
/// reception key, senior rotation key, period keys, folder keys and mail
/// custody under generation 1 and is gone with its bundle. U restored the
/// predecessor's seed from the phrase and ran the ceremony at once, so it
/// holds both seeds and no generation key. The holder kept generation 1's
/// wrap, sealed to the PREDECESSOR's escrow target; U's recovery pass opens
/// it under the predecessor's seed, the re-presented walk carries the mint
/// record, the re-escrow pass deposits the generation under the successor,
/// and U reads every pre-succession row — which by the sixth pass rest under
/// U's own generation, g1 reclaimed behind them.
///
/// Red-verified 2026-10-01, the measurement that ruled the kept wrap: with
/// the wraps burned as the first ruling's ceremony burned them, U keyed
/// nothing, held no mint record and read no reception key, with 11 live rows
/// resting under generation 1 for good; with the wraps kept and nothing
/// else changed, the same — the pass opened wraps under the successor's
/// secret alone, and the walk carried no record it could not key.
#[tokio::test]
async fn an_unkeyed_successor_keys_a_pre_succession_generation_from_the_kept_wrap() {
    let UnkeyedSuccessor {
        rpc,
        u,
        g1,
        measured,
    } = an_unkeyed_successor().await;
    assert!(
        u.custody.0.lock().unwrap().contains_key(&g1),
        "U keys g1 from the wrap the ceremony kept — {measured}"
    );
    assert!(
        u.store
            .state(KIND_GENERATION_MINT, &hex::encode(g1))
            .await
            .unwrap()
            .is_some(),
        "U's walk carries g1's mint record once it keys it — {measured}"
    );
    // From there g1 is any carried generation: U's first tip-sealed write
    // mints its own, the hand-over re-seals every pre-succession row under
    // it, and g1 — nothing resting under it — shreds and its wrap is swept,
    // as `a_successor_on_its_own_generation_0_schedule_reads_a_pre_succession_tip_sealed_row`
    // measures for a successor that carried the key.
    assert!(
        is_shredded(&u, &g1).await && !escrowed(&rpc, &g1).await,
        "g1 is reclaimed behind the hand-over — {measured}"
    );
    assert_reads_every_pre_succession_row(&u, &format!("U, keyed from the kept wrap ({measured})"))
        .await;
}

/// The generation-0 schedule the identity holding `seed` derives in
/// production (`AccountStateKeySchedule` under the seed's `BackupKey`).
fn schedule_of(seed: &[u8; 32]) -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&BackupKey::derive(seed))
}

/// The live generation-sealed (form v2) rows the fleet scope's feed holds
/// under each generation, as their origin writers.
async fn live_rows_by_generation(
    rpc: &RouterRequester,
) -> std::collections::BTreeMap<[u8; 32], Vec<Option<Vec<u8>>>> {
    let fs = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
        .await
        .unwrap()
        .expect("the fleet scope exists");
    let mut by_generation = std::collections::BTreeMap::new();
    for row in rpc
        .state
        .db
        .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
    {
        if let Some(generation) = row.entry_sealed.as_deref().and_then(peek_generation_id) {
            by_generation
                .entry(generation)
                .or_insert_with(Vec::new)
                .push(row.origin_writer);
        }
    }
    by_generation
}

/// The live fleet-scope rows the nest serves whose origin writer is one of
/// `writers`, as `(writer, form v2?)` — the census `account-data-taxonomy.md`
/// clause (3)(i) was measured with.
async fn live_rows_written_by(
    rpc: &RouterRequester,
    writers: &[[u8; 32]],
) -> Vec<([u8; 32], bool)> {
    let fs = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
        .await
        .unwrap()
        .expect("the fleet scope exists");
    rpc.state
        .db
        .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| {
            let writer = <[u8; 32]>::try_from(row.origin_writer?.as_slice()).ok()?;
            writers.contains(&writer).then(|| {
                (
                    writer,
                    row.entry_sealed
                        .as_deref()
                        .and_then(peek_generation_id)
                        .is_some(),
                )
            })
        })
        .collect()
}

/// The relay rows `seat`'s fleet plane still holds whose writer is one of
/// `writers` — bound (γ) of clause (3)(i) for a seat that holds no
/// predecessor material.
async fn relay_rows_written_by(
    seat: &Seat,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    writers: &[[u8; 32]],
) -> usize {
    let p = seat.plane(rpc, keys);
    let mut n = 0;
    for writer in writers {
        n += p
            .relay_rows_of_writer(&WriterId(*writer))
            .await
            .unwrap()
            .len();
    }
    n
}

/// What [`the_succession`] leaves behind: the nest, S after its first walk
/// and re-escrow, and the pre-succession generation.
struct Succession {
    rpc: RouterRequester,
    s: Seat,
    g1: [u8; 32],
    /// The predecessor's device A — the writer of every pre-succession
    /// account-level row.
    a_id: [u8; 32],
    /// Every predecessor device, A first ([`the_succession_of`]).
    predecessor_ids: Vec<[u8; 32]>,
}

/// The succession up to the successor's re-escrow, with the carry's own pins.
///
/// A, the predecessor's device, seals the account's reception key, senior
/// ATProto rotation key, a tier's period keys, the folder keys and the mail
/// custody (all `GenerationTip` kinds) under generation 1, escrowed to the
/// PREDECESSOR identity. The predecessor-keyed escrow wraps are then cleared
/// at the holder: this fixture seats both identities on the suite's one nest
/// actor, where the wrap the ceremony keeps (`Succession::Stay` — the kept
/// wrap; `conformance_succession.rs` owns what the ceremony does with it)
/// could not be told from the successor's deposit, so clearing it is what
/// lets the re-escrow below be witnessed. A holds no grant on the
/// successor's account (its grant revoked at the DB), so A's walk mark counts
/// for nothing at the retention gate. S, the successor's runtime on A's machine, is seated as
/// the driver seats it: generation 1's key in its slot (the carriage —
/// `PrincipalSlot::carry_predecessor_generation_keys`, modelled by copying A's
/// bundle) and the predecessor attested, so its fleet plane holds that
/// identity's mint-kind keys. One walk, then the re-escrow pass.
///
/// Pinned here:
///
/// - **(e)** a successor device handed the same mint-kind keys and holding NO
///   generation-1 key carries nothing — key in hand is the gate;
/// - **(a)** S's walk carries generation 1's mint record, value verbatim, as
///   its own row; the re-escrow pass then deposits exactly that generation
///   under the successor's target and mints nothing.
async fn the_succession() -> Succession {
    the_succession_of(1).await
}

/// [`the_succession`] with a predecessor fleet of `devices` devices on one
/// generation: A, then `devices - 1` more seats joined and settled under the
/// predecessor's schedule once A has minted, each holding g1 and writing its
/// own enrollment, reach and device-endpoints rows; every one of their
/// grants is revoked at the ceremony with A's.
async fn the_succession_of(devices: u16) -> Succession {
    let rpc = nest_with_escrow().await;

    // ── Before: the predecessor identity seals the tip-sealed rows under g1.
    let keys = &schedule_of(&SEAT_IDENTITY_SEED);
    let a = Seat::new(0).await;
    a.join(&rpc, keys).await;
    publish_identity_escrow_target(&a, &rpc, keys).await;
    settle(&[&a], &rpc, keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;
    let mut others = Vec::new();
    for n in 1..devices {
        let b = Seat::new(20 + n).await;
        b.join(&rpc, keys).await;
        others.push(b);
    }
    if !others.is_empty() {
        let mut fleet = vec![&a];
        fleet.extend(others.iter());
        settle(&fleet, &rpc, keys, 3).await;
        for b in &others {
            assert!(
                holds_generation_key(b, &g1).await,
                "predecessor device {} keys g1 before the succession",
                b.n
            );
        }
        assert_eq!(
            minted_generation_id(&a.store).await,
            g1,
            "the predecessor fleet stays on one generation"
        );
    }
    write_reception_key(&a, &rpc, keys).await;
    write_rotation_key(&a, &rpc, keys).await;
    write_period_key(&a, &rpc, keys).await;
    write_folder_keys(&a, &rpc, keys).await;
    write_mail_custody(&a, &rpc, keys).await;
    settle(&[&a], &rpc, keys, 1).await;
    assert!(
        escrowed(&rpc, &g1).await,
        "g1 is escrowed before the succession"
    );
    let g1_record = a
        .store
        .state(KIND_GENERATION_MINT, &hex::encode(g1))
        .await
        .unwrap()
        .expect("the predecessor's mint row")
        .value;

    // ── The succession, and the predecessor-keyed wraps cleared at the
    // holder (the fixture's one nest actor — see the doc above).
    for wrap in rpc
        .state
        .db
        .get_generation_escrow_wraps(&ACTOR, None)
        .await
        .unwrap()
    {
        rpc.state
            .db
            .delete_generation_escrow_wraps(&ACTOR, &wrap.generation_id)
            .await
            .unwrap();
    }
    assert!(
        !escrowed(&rpc, &g1).await,
        "the holder holds no wrap of g1 before the successor's deposit"
    );
    // And the predecessor's device holds no grant on the successor's
    // account: its walk mark no longer counts at the retention gate, so the
    // gate's watermark is not held at the predecessor's last walk.
    let a_id = a.id();
    let predecessor_ids: Vec<[u8; 32]> = std::iter::once(a_id)
        .chain(others.iter().map(Seat::id))
        .collect();
    drop(others);
    for id in &predecessor_ids {
        rpc.state.db.revoke_device_grant(&ACTOR, id).await.unwrap();
    }
    let keys = &schedule_of(&SUCCESSOR_SEED);

    // ── (e) A successor device that holds the predecessor's seed (so its
    // plane is handed the mint-kind keys) and NOT generation 1's key: its
    // walk opens the predecessor's mint row and carries nothing.
    {
        let e = Seat::successor(3, false).await.attesting_the_predecessor();
        let p = e.plane(&rpc, keys);
        let walked = p.walk().await.unwrap();
        assert_eq!(
            walked.inherited, 0,
            "no key in hand, no record carried: {walked:?}"
        );
        assert!(
            e.store
                .states_of_kind(KIND_GENERATION_MINT)
                .await
                .unwrap()
                .is_empty(),
            "a device carries no record of a generation whose key it does not hold"
        );
        assert_eq!(
            p.publish_pending().await.unwrap(),
            0,
            "and publishes nothing"
        );
    }

    // ── S: the successor on A's machine, A's retained keys carried over and
    // the predecessor attested.
    let s = Seat::successor(1, true).await.attesting_the_predecessor();
    for (g, k) in a.custody.0.lock().unwrap().iter() {
        s.custody.0.lock().unwrap().insert(*g, *k);
    }
    drop(a);
    s.join(&rpc, keys).await;
    let target = fauna_sync_engine::generation_mint::escrow_target_entry(&SUCCESSOR_SEED).unwrap();
    s.plane(&rpc, keys)
        .put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: target.key.clone(),
            },
            target.value,
            None,
        )
        .await
        .unwrap();
    let p = s.plane(&rpc, keys);
    let walked = p.walk().await.unwrap();
    let rider = fauna_sync_engine::generation_reescrow::ensure_reescrowed(
        &s.store,
        &p,
        s.trust(),
        &s.key,
        None,
    )
    .await
    .unwrap();
    drop(p);

    // (a) The carry, then the deposit. Read before any assertion so a red run
    // names what the rider had to work with.
    let mint_rows = s.store.states_of_kind(KIND_GENERATION_MINT).await.unwrap();
    let state = format!(
        "the rider: {rider:?}; S's walk: {walked:?}; generation-mint rows S holds: {}; g1 in \
         S's bundle: {}",
        mint_rows.len(),
        s.custody.0.lock().unwrap().contains_key(&g1)
    );
    assert_eq!(
        walked.inherited, 1,
        "S's walk carries exactly g1's mint record — {state}"
    );
    assert_eq!(
        mint_rows
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_slice(), e.tombstone))
            .collect::<Vec<_>>(),
        vec![(hex::encode(g1).as_str(), g1_record.as_slice(), false)],
        "S holds one generation-mint row, g1's, the predecessor's record verbatim — {state}"
    );
    assert_eq!(
        rider,
        fauna_sync_engine::generation_reescrow::ReescrowPass::Reescrowed {
            deposited: 1,
            restored: 0,
        },
        "S re-escrows g1 under the successor, and the pass mints nothing — {state}"
    );
    assert!(
        escrowed(&rpc, &g1).await,
        "g1 is escrowed again — under the successor — {state}"
    );
    assert_eq!(
        s.store
            .states_of_kind(KIND_GENERATION_MINT)
            .await
            .unwrap()
            .len(),
        1,
        "the re-escrow pass minted no generation of its own — {state}"
    );
    Succession {
        rpc,
        s,
        g1,
        a_id,
        predecessor_ids,
    }
}

/// Every pre-succession tip-sealed row `seat` must read: the reception key,
/// the senior ATProto rotation key, a tier's period keys, the folder-key
/// custody and the mail custody.
async fn assert_reads_every_pre_succession_row(seat: &Seat, who: &str) {
    assert_eq!(
        reception_keys_held(seat).await,
        1,
        "{who} reads the pre-succession reception key"
    );
    // The irrecoverable case (`config-dissolution.md` — the kinds table's
    // `fauna.state.atproto-identity` row): the senior rotation key, sealed
    // under g1 before the succession, is not stranded by it.
    assert_eq!(
        rotation_keys_held(seat).await,
        vec![senior_rotation_key()],
        "{who} reads the pre-succession senior rotation key"
    );
    // The same for a tier's period keys (the `fauna.state.subscriptions`
    // row): the successor's post-succession rotation leg rotates a tier FROM
    // the predecessor's current period, and an archival mint reads every
    // prior one, so neither may be stranded by the succession.
    assert_eq!(
        period_keys_held(seat).await,
        held_period_key(),
        "{who} reads the pre-succession tier period keys"
    );
    // Nor are the shared-folder content keys (the `fauna.state.folder-keys`
    // row): both generations of the owned set and the foreign record.
    assert_eq!(
        folder_keys_held(seat).await,
        folder_key_custody(),
        "{who} reads the pre-succession folder-key custody"
    );
    // The mail custody (`config-dissolution.md` — the kinds table's
    // `fauna.state.mail` row, the fresh-device + post-succession read-back the
    // consumer cut owes): the MSEK and the credential that unwraps it, sealed
    // under g1 before the succession, are what a device's mail apps open
    // every stored message with.
    let mail = fauna_sync_engine::mail_rows::read_mail(&seat.store)
        .await
        .unwrap();
    assert_eq!(
        mail.msek.as_ref().map(|m| m.to_array()),
        Some(ACCOUNT_MSEK),
        "{who} reads the pre-succession MSEK"
    );
    assert_eq!(
        mail.credentials
            .iter()
            .map(|c| (c.credential_id.as_str(), c.secret.as_slice()))
            .collect::<Vec<_>>(),
        vec![("default", b"correct horse battery staple".as_slice())],
        "{who} reads the credential row beside it"
    );
}

/// **The deployment-seed custody's recovery floor after total device loss**
/// (`config-dissolution.md` § Phases and gates → M, facts (a)–(c); the kinds
/// table's `fauna.state.deployment-seeds` row; `nest/box-recovery.md` § Trust
/// & audience).
///
/// A custodies a box's deployment seed — the box's irrecoverable identity, a
/// `GenerationTip` row sealed under generation 1 — through the production door
/// (`deployment_seed_rows::merge_deployment_seeds`). Then A's machine is lost:
/// no sign-out, no severance, no sibling left. F, a fresh device holding only
/// the identity seed the root ceremony gives back (the phrase recovers it —
/// `conformance_recovery_escrow.rs`), must read the box's seed from the escrow
/// wrap the home nest keeps (fact (a): one availability floor) under a key the
/// seed alone derives (fact (b): one key floor), and re-create the box's
/// identity from it — the floor the blob held, and why the consumer cut may
/// drop the blob's copy.
///
/// Red-verified: build F with `Seat::new` (no escrow-recovery leg) and F reads
/// no custodied box.
#[tokio::test]
async fn a_fresh_device_holding_only_the_seed_reads_the_deployment_seeds_after_total_device_loss() {
    use fauna_core::data::DeploymentSeedEntry;
    use fauna_sync_engine::deployment_seed_rows::{merge_deployment_seeds, read_deployment_seeds};

    let rpc = nest_with_escrow().await;
    let keys = schedule();
    let a = Seat::new(0).await;
    a.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 2).await;
    let g1 = minted_generation_id(&a.store).await;

    let box_seed = [0x5D; 32];
    let box_id = fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(box_seed);
    let entry = DeploymentSeedEntry {
        nest_actor_id: box_id,
        seed: box_seed.into(),
        domain: Some("box.example".into()),
        ..Default::default()
    };
    {
        let plane = a.plane(&rpc, &keys);
        let (joined, moved) =
            merge_deployment_seeds(&a.store, &plane, std::slice::from_ref(&entry))
                .await
                .unwrap();
        assert!(moved, "the custody is a new row");
        assert_eq!(joined, vec![entry.clone()]);
        plane.publish_pending().await.unwrap();
    }
    settle(&[&a], &rpc, &keys, 1).await;
    drop(a); // Total device loss: no sign-out, nothing left but the seed.

    let f = Seat::seed_holding(1).await;
    f.join(&rpc, &keys).await;
    settle(&[&f], &rpc, &keys, 3).await;

    let recovered = f.custody.0.lock().unwrap().contains_key(&g1);
    let custodied = read_deployment_seeds(&f.store).await.unwrap();
    let state = format!(
        "generation 1 in F's bundle: {recovered}, boxes F reads: {}",
        custodied.len()
    );
    assert!(
        recovered,
        "F keys generation 1 from its escrow wrap with nothing but the seed — {state}"
    );
    assert_eq!(
        custodied,
        vec![entry],
        "F reads the box's custody row back — {state}"
    );
    assert_eq!(
        fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(custodied[0].seed.to_array()),
        box_id,
        "the recovered seed re-creates the box's identity"
    );
}

/// **The cold read** — the same floor as the case above, read by a device that
/// never joins the fleet (`nest/box-recovery.md` § The plane-era recovery
/// floor, *(b)*: the cold read writes nothing — no enrollment, no device-set
/// row, no escrow target, no mint, no put).
///
/// A custodies a box through the production door and is lost with no
/// sign-out. A reader holding only the identity seed — no store, no `join`, no
/// device-set row, no sync-device registration — reads the box's seed off the
/// nest with `cold_read_deployment_seeds` and re-derives the box id. The nest
/// is then exactly as it was: not one row written (SQLite's own change count
/// on the nest's connection, so a walk mark or a stray put would show), the
/// same live fleet rows, the same devices.
///
/// The generation's escrow receipt is retired off the feed first: the cold
/// read asks the nest once for every wrap and trusts a wrap for its key
/// commitment, never a receipt (`box-recovery.md` *(b)* — which is what lets it
/// read at a rebuilt or rotated box whatever the receipts say).
///
/// The key schedule is derived from the identity seed here, as production
/// derives it (`BackupKey::derive(seed)`), rather than the file's fixed
/// [`schedule`] — the cold read has the seed and nothing else to derive from.
///
/// Red-verified: with the escrow-recovery step removed from the cold read,
/// the reader opens no generation-sealed row and reads no box.
#[tokio::test]
async fn a_seed_holder_that_never_enrolls_reads_the_deployment_seeds_with_the_cold_read() {
    use fauna_core::data::DeploymentSeedEntry;
    use fauna_sync_engine::deployment_seed_recovery::cold_read_deployment_seeds;
    use fauna_sync_engine::deployment_seed_rows::merge_deployment_seeds;

    let rpc = nest_with_escrow().await;
    let keys = AccountStateKeySchedule::derive(&BackupKey::derive(&SEAT_IDENTITY_SEED));
    let a = Seat::new(0).await;
    a.join(&rpc, &keys).await;
    publish_identity_escrow_target(&a, &rpc, &keys).await;
    settle(&[&a], &rpc, &keys, 2).await;

    let box_seed = [0x5D; 32];
    let box_id = fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(box_seed);
    let entry = DeploymentSeedEntry {
        nest_actor_id: box_id,
        seed: box_seed.into(),
        domain: Some("box.example".into()),
        ..Default::default()
    };
    {
        let plane = a.plane(&rpc, &keys);
        let (_, moved) = merge_deployment_seeds(&a.store, &plane, std::slice::from_ref(&entry))
            .await
            .unwrap();
        assert!(moved, "the custody is a new row");
        plane.publish_pending().await.unwrap();
    }
    settle(&[&a], &rpc, &keys, 1).await;

    // Retire the generation's escrow receipt off the nest's feed.
    let receipt_key = {
        let rows: Vec<_> = a
            .store
            .states_of_kind(KIND_ESCROW_RECEIPT)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| !e.tombstone)
            .collect();
        assert_eq!(rows.len(), 1, "one receipt: {rows:?}");
        rows[0].key.clone()
    };
    let receipt_item = a
        .plane(&rpc, &keys)
        .gen0_item_key(KIND_ESCROW_RECEIPT, &receipt_key)
        .expect("the receipt kind is gen-0 sealed");
    drop(a); // Total device loss: no sign-out, nothing left but the seed.
    let receipt_row = fleet_feed(&rpc)
        .await
        .into_iter()
        .find(|c| fauna_sync_engine::account_state_plane::item_key_of(c).unwrap() == receipt_item)
        .expect("the receipt is on the feed");
    let (receipt_writer, receipt_seq) =
        fauna_sync_engine::account_state_plane::row_coordinates(&receipt_row).unwrap();
    let fs = rpc
        .state
        .db
        .find_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rpc.state
            .db
            .retire_account_state_entry(
                &ACTOR,
                ACCOUNT_STATE_FLEET_SCOPE,
                fs,
                &receipt_item,
                &receipt_writer.0,
                i64::try_from(receipt_seq).unwrap(),
                None,
                false,
            )
            .await
            .unwrap()
            .unwrap(),
        "the receipt row retires"
    );
    assert!(
        !fleet_feed(&rpc).await.iter().any(|c| {
            fauna_sync_engine::account_state_plane::item_key_of(c).unwrap() == receipt_item
        }),
        "no receipt is left in merged state for the reader to find"
    );

    let writes_before = rpc.state.db.conn().await.total_changes();
    let rows_before = live_fleet_entries(&rpc).await;
    let devices_before = rpc.state.db.count_sync_devices(&ACTOR).await.unwrap();

    let read = cold_read_deployment_seeds(&rpc, &SEAT_IDENTITY_SEED)
        .await
        .unwrap();

    assert_eq!(read, vec![entry], "the reader reads the box's custody row");
    assert_eq!(
        fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(read[0].seed.to_array()),
        box_id,
        "the recovered seed re-creates the box's identity"
    );
    assert_eq!(
        rpc.state.db.conn().await.total_changes(),
        writes_before,
        "the cold read wrote nothing on the nest — no row, no walk mark"
    );
    assert_eq!(live_fleet_entries(&rpc).await, rows_before);
    assert_eq!(
        rpc.state.db.count_sync_devices(&ACTOR).await.unwrap(),
        devices_before,
        "the reader registered no device"
    );
}

// ── The capability host's plane read (decision 1′) ──────────────────────────
//
// `on-demand-files.md` § Shared sets on a capability host, decision 1′: a
// process that hosts no account store reads the fleet-only
// `fauna.state.folder-keys` through a throwaway fleet replica keyed as the
// enrolled device it is a process of — the machine principal's inline wraps
// and top-ups, else the custody it holds — and writes nothing on the nest.
// Driven through the production reader (`ColdReplicaFolderKeys`), the host's
// own worker thread and all, over the nest's own request path.

/// A capability host's folder-key reader for the device whose writer key is
/// `device`, holding `custody`.
fn host_folder_keys(
    rpc: &RouterRequester,
    device: &SigningKey,
    custody: Arc<fauna_sync_engine::cold_replica::MemoryRetainedKeys>,
) -> fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys {
    fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys::spawn(
        rpc.clone(),
        fleet_root().actor_id(),
        BackupKey::derive(&SEAT_IDENTITY_SEED),
        fauna_sync_engine::cold_replica::ColdKeySource::Device {
            writer_key: Box::new(device.clone()),
            custody,
        },
    )
    .unwrap()
}

/// A content-key generation of the shared set.
fn set_generation(version: u64) -> fauna_core::folder_keys::ContentKeyGeneration {
    fauna_core::folder_keys::ContentKeyGeneration {
        version,
        key: [0x40 + version as u8; 32].into(),
        rotated_at: version * 1_000,
    }
}

/// The shared set's custody at `generations` (newest current), beside one
/// foreign set shared with the account.
fn shared_custody(generations: &[u64]) -> fauna_core::data::FoldersConfig {
    let mut gens: Vec<_> = generations.iter().map(|v| set_generation(*v)).collect();
    let current = gens.pop().unwrap();
    gens.reverse();
    fauna_core::data::FoldersConfig {
        sets: vec![fauna_core::data::FolderKeyCustody {
            channel_id: Some([0xC1; 32]),
            keys: Some(fauna_core::folder_keys::FolderContentKeys {
                current,
                prior: gens,
            }),
            set_nonce: Some([0x01; 32]),
            name: Some("shared".into()),
            created_at: 7,
            ..Default::default()
        }],
        foreign_sets: vec![fauna_core::data::ForeignFolder {
            channel_id: [0x77; 32],
            mls_group_id: vec![0x78; 16],
            home_nest_url: "https://home.example".into(),
            set_name: Some("photos".into()),
            access: Some("reader".into()),
            content_key_floor: Some(2),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// `seat` writes `custody` through the production folder-keys door, then the
/// runtime's publish step. Returns the seat's fold afterwards.
async fn door_write_folder_keys(
    seat: &Seat,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
    custody: &fauna_core::data::FoldersConfig,
) -> fauna_core::data::FoldersConfig {
    let plane = seat.plane(rpc, keys);
    let (folded, moved) =
        fauna_sync_engine::folder_key_rows::merge_folder_keys(&seat.store, &plane, custody)
            .await
            .unwrap();
    assert!(moved, "the door wrote rows");
    plane.publish_pending().await.unwrap();
    folded
}

/// Seats 0 (A), 1 (B) and 2 (C) enrolled, generation 1 minted over all three,
/// and A's custody of a two-generation shared set plus a foreign set written
/// under it. Returns the nest, the key schedule, the seats and A's fold.
async fn host_read_fleet() -> (
    RouterRequester,
    AccountStateKeySchedule,
    [Seat; 3],
    fauna_core::data::FoldersConfig,
) {
    let rpc = nest_with_escrow().await;
    let keys = AccountStateKeySchedule::derive(&BackupKey::derive(&SEAT_IDENTITY_SEED));
    let seats = [Seat::new(0).await, Seat::new(1).await, Seat::new(2).await];
    for seat in &seats {
        seat.join(&rpc, &keys).await;
    }
    publish_identity_escrow_target(&seats[0], &rpc, &keys).await;
    settle(&[&seats[0], &seats[1], &seats[2]], &rpc, &keys, 2).await;
    let folded = door_write_folder_keys(&seats[0], &rpc, &keys, &shared_custody(&[1, 2])).await;
    assert_eq!(
        (folded.sets.len(), folded.foreign_sets.len()),
        (1, 1),
        "{folded:?}"
    );
    (rpc, keys, seats, folded)
}

/// **(a) The host reads the account's custody as its device, and writes
/// nothing.** A host of A's machine — the machine principal's writer key and
/// an empty custody, no seed, no store — folds exactly the custody A's door
/// wrote (the two-generation set and the foreign record), keyed by the inline
/// wrap generation 1's mint addresses to A. The nest is exactly as it was:
/// not one write on its connection (no walk mark, no row of the replica's
/// ephemeral writer), the same live fleet rows, the same devices.
///
/// Red-verified: with the device arm keying nothing, the fold is empty.
#[tokio::test]
async fn a_capability_host_reads_folder_key_custody_as_its_device_and_writes_nothing() {
    let (rpc, _keys, seats, expected) = host_read_fleet().await;
    let writes_before = rpc.state.db.conn().await.total_changes();
    let rows_before = live_fleet_entries(&rpc).await;
    let devices_before = rpc.state.db.count_sync_devices(&ACTOR).await.unwrap();

    let custody = Arc::new(fauna_sync_engine::cold_replica::MemoryRetainedKeys::default());
    let host = host_folder_keys(&rpc, &seats[0].key, custody.clone());
    let read = fauna_client_folders::FolderKeyReader::load(&host)
        .await
        .unwrap();

    assert_eq!(read, expected, "the host folds the custody A's door wrote");
    assert_eq!(
        custody.generations().into_iter().collect::<Vec<_>>(),
        live_mint_ids(&seats[0].store).await,
        "the generation the host unwrapped is recorded into its custody"
    );
    assert_eq!(
        rpc.state.db.conn().await.total_changes(),
        writes_before,
        "the host wrote nothing on the nest — no row, no walk mark"
    );
    assert_eq!(live_fleet_entries(&rpc).await, rows_before);
    assert_eq!(
        rpc.state.db.count_sync_devices(&ACTOR).await.unwrap(),
        devices_before,
        "the host registered no device"
    );
}

/// **(b) A rotation minted by another device reaches a held host at its next
/// load; (c) a removed device's host fails closed on it.** B removes C, and
/// B's next custody write heal-mints generation 2 over {A, B} — minted by B,
/// after A's host was built and had loaded once. A's host, the same reader,
/// folds B's new content-key generation at its next load through
/// generation 2's inline wrap to A. C's host — a device outside generation
/// 2's member set, its custody empty — folds what generation 1 seals and
/// nothing generation 2 does: the new content key never reaches it.
#[tokio::test]
async fn a_rotation_from_another_device_reaches_a_held_host_and_a_removed_host_fails_closed() {
    let (rpc, keys, seats, first) = host_read_fleet().await;
    let [a, b, c] = &seats;
    let a_host = host_folder_keys(&rpc, &a.key, Default::default());
    assert_eq!(
        fauna_client_folders::FolderKeyReader::load(&a_host)
            .await
            .unwrap(),
        first
    );

    // B removes C; B's next origination heals with a covering mint.
    b.plane(&rpc, &keys)
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(c.id()),
            },
            removal_value(b.id()),
            None,
        )
        .await
        .unwrap();
    b.plane(&rpc, &keys).reconcile().await.unwrap();
    let rotated = door_write_folder_keys(b, &rpc, &keys, &shared_custody(&[1, 2, 3])).await;
    let mints = live_mint_ids(&b.store).await;
    assert_eq!(mints.len(), 2, "B minted a covering generation: {mints:?}");

    let read = fauna_client_folders::FolderKeyReader::load(&a_host)
        .await
        .unwrap();
    assert_eq!(read, rotated, "A's held host folds B's rotation");
    assert_eq!(
        read.sets[0].keys.as_ref().unwrap().current,
        set_generation(3)
    );

    let c_host = host_folder_keys(&rpc, &c.key, Default::default());
    assert_eq!(
        fauna_client_folders::FolderKeyReader::load(&c_host)
            .await
            .unwrap(),
        first,
        "C folds what generation 1 seals and nothing generation 2 does"
    );
}

/// **(d) A generation held only in custody keys the rows no wrap reaches.**
/// A device that never enrolled — no wrap on any mint — folds nothing with an
/// empty custody, and folds A's custody once generation 1's key is recorded
/// in its custody by hand (the escrow-recovered shape).
#[tokio::test]
async fn a_generation_held_only_in_custody_keys_the_rows_no_wrap_reaches() {
    let (rpc, _keys, seats, expected) = host_read_fleet().await;
    let stranger = Seat::new(3).await;

    let bare = host_folder_keys(&rpc, &stranger.key, Default::default());
    assert_eq!(
        fauna_client_folders::FolderKeyReader::load(&bare)
            .await
            .unwrap(),
        Default::default(),
        "no wrap and no custody: every sealed row fails closed"
    );

    let custody = Arc::new(fauna_sync_engine::cold_replica::MemoryRetainedKeys::default());
    for generation in live_mint_ids(&seats[0].store).await {
        let key = fauna_sync_engine::generation_tip::RetainedKeyCustody::retained_generation_key(
            &seats[0].custody,
            &generation,
        )
        .expect("A holds the generation it minted");
        fauna_sync_engine::generation_tip::RetainedKeyCustody::record_generation_key(
            &*custody,
            &generation,
            &key,
        );
    }
    let held = host_folder_keys(&rpc, &stranger.key, custody);
    assert_eq!(
        fauna_client_folders::FolderKeyReader::load(&held)
            .await
            .unwrap(),
        expected
    );
}

/// **(e) Two devices' lists for one box fold on the host, which seals and
/// writes nothing.** A and B each replace the destination list of one source
/// box through the production door — B's later — so a walk from zero meets
/// A's row, then B's under the same key, and the kind's join answers a value
/// other than the one it holds: a merge. A throwaway replica authors nothing
/// (`cold_replica` module docs), so it folds that merge into its own store and
/// never seals it — it trusts no escrow holder, so no tip resolves for it and
/// a seal is refused. The host keeps reading the custody, the replica holds
/// B's list, and the nest is exactly as it was.
///
/// Red-verified: before the replica's plane folded merges, the walk sealed
/// the merged row and failed with `no candidate generation tip resolves`,
/// so the custody load failed.
#[tokio::test]
async fn a_capability_host_folds_two_devices_backup_lists_for_one_box_and_seals_nothing() {
    use fauna_core::data::{BackupConfig, BackupDestination, Timestamp};
    use fauna_protocol::merge_policy::KIND_BACKUP;

    let (rpc, keys, seats, expected) = host_read_fleet().await;
    let [a, b, _] = &seats;
    let source_nest = [0x5B; 32];
    let list = |url: &str| BackupConfig {
        destinations: vec![BackupDestination {
            destination_id: format!("dest-{url}"),
            destination_nest_url: url.into(),
            ..Default::default()
        }],
    };
    for (seat, url, now) in [
        (a, "https://first.example", 1_000),
        (b, "https://second.example", 2_000),
    ] {
        let plane = seat.plane(&rpc, &keys);
        let (_, moved) = fauna_sync_engine::backup_rows::write_backup_destinations(
            &seat.store,
            &plane,
            &source_nest,
            &list(url),
            Timestamp(now),
        )
        .await
        .unwrap();
        assert!(moved, "seat {} wrote its list", seat.n);
        plane.publish_pending().await.unwrap();
    }
    let writes_before = rpc.state.db.conn().await.total_changes();
    let rows_before = live_fleet_entries(&rpc).await;

    let host = host_folder_keys(&rpc, &a.key, Default::default());
    assert_eq!(
        fauna_client_folders::FolderKeyReader::load(&host)
            .await
            .unwrap(),
        expected,
        "the host reads the custody past the merge"
    );

    let replica = fauna_sync_engine::cold_replica::ColdFleetReplica::open(
        rpc.clone(),
        fleet_root().actor_id(),
        &BackupKey::derive(&SEAT_IDENTITY_SEED),
        fauna_sync_engine::cold_replica::ColdKeySource::Device {
            writer_key: Box::new(a.key.clone()),
            custody: Arc::new(fauna_sync_engine::cold_replica::MemoryRetainedKeys::default()),
        },
    )
    .await
    .unwrap();
    replica.walk().await.unwrap();
    let held = fauna_sync_engine::backup_rows::backup_state_of(
        &replica.states_of_kind(KIND_BACKUP).await.unwrap(),
        &source_nest,
    )
    .unwrap();
    assert_eq!(
        held.backup,
        list("https://second.example"),
        "B's later list"
    );

    assert_eq!(
        rpc.state.db.conn().await.total_changes(),
        writes_before,
        "neither the host nor the replica wrote anything on the nest"
    );
    assert_eq!(live_fleet_entries(&rpc).await, rows_before);
}

// ── The bind leg: a bound nest is made complete (row half) ──────────────────
//
// `account-sync-plane.md` § The bind leg, ruling 1: after every full-state
// reconcile a device pushes, verbatim, every relay row the bound nest's
// listing lacks — its own, and a verified sibling's.

/// A seat's delegable-scope plane (the fleet-scope one is [`Seat::plane`]).
fn seat_delegable<'a, R: RpcRequester>(
    seat: &'a Seat,
    rpc: &'a R,
    keys: &'a AccountStateKeySchedule,
) -> AccountStatePlane<'a, SqliteBackend, R>
where
    R::Error: fauna_protocol::RpcErrorClass,
{
    AccountStatePlane::new(
        &seat.store,
        rpc,
        keys,
        &seat.key,
        seat.trust(),
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap()
    .with_generation_custody(&seat.custody)
}

/// A seat's moderation write, stamped as its own.
fn seat_moderation(seat: &Seat, keyword: &str, at_ms: i64) -> (ItemId, Vec<u8>, Option<Vec<u8>>) {
    (
        moderation_item(),
        moderation_with(keyword),
        Some(
            LwwStamp {
                at_ms,
                writer: seat.id(),
            }
            .encode()
            .unwrap(),
        ),
    )
}

/// One pass's reconcile and publish diff on both scopes — the bind leg's
/// row half as the pump runs it.
async fn settle_bound<R: RpcRequester + Clone>(seat: &Seat, rpc: &R, keys: &AccountStateKeySchedule)
where
    R::Error: fauna_protocol::RpcErrorClass,
{
    for p in [
        AccountStatePlane::new(
            &seat.store,
            rpc,
            keys,
            &seat.key,
            seat.trust(),
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
        .with_generation_custody(&seat.custody),
        seat_delegable(seat, rpc, keys),
    ] {
        p.reconcile().await.unwrap();
        fauna_sync_engine::publish_diff::publish_diff(&seat.store, &p, seat.trust(), &seat.key)
            .await
            .unwrap();
    }
}

/// One live row: `(writer, item key, writer seq, sealed entry)`.
type LiveRow = (Vec<u8>, Vec<u8>, i64, Vec<u8>);

/// A nest's live rows of `scope`, as [`LiveRow`]s — the listing a reconcile is
/// answered with, bytes included.
async fn live_rows_of(rpc: &RouterRequester, scope: &str) -> std::collections::BTreeSet<LiveRow> {
    let Some(fs) = rpc.state.db.find_state_scope(&ACTOR, scope).await.unwrap() else {
        return Default::default();
    };
    rpc.state
        .db
        .get_account_state_changes(fs, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
        .into_iter()
        .map(|r| {
            (
                r.origin_writer.unwrap_or_default(),
                r.path_hash,
                r.origin_seq.unwrap_or_default(),
                r.entry_sealed.unwrap_or_default(),
            )
        })
        .collect()
}

/// **A second, empty nest is made whole by one settled pass.** Two seats
/// write on nest A; seat 1 walks everything A holds. Pointed at an empty nest
/// B — the same account present there — seat 1's pass pushes its own rows
/// AND seat 2's: B then lists every live fleet-scope row of both writers,
/// byte-identical to A's, and on the delegable scope A's rows but those below
/// a cover B lists (ruling 1(b)). Before the diff, B held seat 1's rows only if
/// seat 1 wrote after binding, and seat 2's never.
///
/// Red-verified: with the diff step removed from [`settle_bound`], B lists nothing.
#[tokio::test]
async fn a_second_nest_settles_to_every_live_row_byte_for_byte() {
    let (a, b) = (nest().await, nest().await);
    let keys = schedule();
    let (one, two) = (Seat::new(1).await, Seat::new(2).await);
    for seat in [&one, &two] {
        seat.join(&a, &keys).await;
    }
    for (seat, keyword, at) in [(&one, "one", 100), (&two, "two", 200)] {
        let (item, value, stamp) = seat_moderation(seat, keyword, at);
        seat_delegable(seat, &a, &keys)
            .put(&item, value, stamp)
            .await
            .unwrap();
    }
    settle_bound(&one, &a, &keys).await;

    settle_bound(&one, &b, &keys).await;
    for scope in [ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE] {
        let (on_a, on_b) = (live_rows_of(&a, scope).await, live_rows_of(&b, scope).await);
        let writers: std::collections::BTreeSet<&Vec<u8>> = on_a.iter().map(|r| &r.0).collect();
        assert_eq!(writers.len(), 2, "{scope}: both seats wrote on A");
        if scope == ACCOUNT_STATE_FLEET_SCOPE {
            assert_eq!(
                on_b, on_a,
                "{scope}: B holds exactly A's live rows, byte for byte"
            );
        } else {
            // Both seats wrote the one moderation item; seat 2's later stamp
            // won the merge, so seat 1's own row is below that listed cover
            // and the diff never pushes it (`account-sync-plane.md` § The bind
            // leg, ruling 1(b)): B holds the cover alone, byte for byte.
            let two_writer = two.key.verifying_key().to_bytes().to_vec();
            let cover: std::collections::BTreeSet<LiveRow> =
                on_a.iter().filter(|r| r.0 == two_writer).cloned().collect();
            assert_eq!(cover.len(), 1, "{scope}: seat 2's row is on A");
            assert_eq!(
                on_b, cover,
                "{scope}: B holds A's cover and no row below it, byte for byte"
            );
        }
    }

    // Settled: the next pass sends nothing.
    let puts = b.calls_of(fauna_protocol::account_state::KIND_STATE_PUT);
    settle_bound(&one, &b, &keys).await;
    assert_eq!(
        b.calls_of(fauna_protocol::account_state::KIND_STATE_PUT),
        puts,
        "a settled nest is sent nothing"
    );
}

// ── One live row per item: the cover step across seats and nests ───────────
//
// `delegable-scope-reclamation.md` § Delegable-scope reclamation, parts (3)
// and (4), *At a linked nest*.

/// One delegable pass of `seat` at `rpc` as the pump runs it: the reconcile,
/// the publish diff, then the cover step on the same plane (the listing it
/// judges is the one this reconcile banked).
async fn cover_pass(
    seat: &Seat,
    rpc: &RouterRequester,
    keys: &AccountStateKeySchedule,
) -> fauna_sync_engine::delegable_reclaim::CoverReclaim {
    let p = seat_delegable(seat, rpc, keys);
    p.reconcile().await.unwrap();
    fauna_sync_engine::publish_diff::publish_diff(&seat.store, &p, seat.trust(), &seat.key)
        .await
        .unwrap();
    fauna_sync_engine::delegable_reclaim::reclaim_below_cover(
        &seat.store,
        &p,
        seat.trust(),
        &seat.key,
        None,
    )
    .await
    .unwrap()
    .expect("the delegable plane banked a listing")
}

/// **An item only a removed seat carried reaches a second nest** (the
/// 2026-10-01 measurement (d), by the seat flow). Seat two writes the
/// moderation record on nest A; seat one walks it, writes two's `Removed`
/// row and settles — its cover step hands the record over, since no
/// member's row carries it. Bound to an empty nest B, one's passes leave B
/// holding one's own row of the record, and a fresh seat walking B reads it.
/// Before the cover step B held no row of the scope.
#[tokio::test]
async fn an_item_only_a_removed_seat_carried_reaches_a_second_nest() {
    let (a, b) = (nest().await, nest().await);
    let keys = schedule();
    let (one, two) = (Seat::new(1).await, Seat::new(2).await);
    for seat in [&one, &two] {
        seat.join(&a, &keys).await;
    }
    let (item, value, stamp) = seat_moderation(&two, "two", 200);
    seat_delegable(&two, &a, &keys)
        .put(&item, value.clone(), stamp)
        .await
        .unwrap();
    settle_bound(&one, &a, &keys).await;
    one.plane(&a, &keys)
        .put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: hex::encode(two.id()),
            },
            removal_value(one.id()),
            None,
        )
        .await
        .unwrap();
    a.state
        .db
        .revoke_device_grant(&ACTOR, &two.id())
        .await
        .unwrap();
    for _ in 0..3 {
        settle_bound(&one, &a, &keys).await;
        cover_pass(&one, &a, &keys).await;
    }
    let on_a: Vec<Vec<u8>> = live_rows_of(&a, ACCOUNT_STATE_SCOPE)
        .await
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(
        on_a,
        vec![one.id().to_vec()],
        "A: one's hand-over carries the record and two's row is gone"
    );

    for _ in 0..3 {
        settle_bound(&one, &b, &keys).await;
        cover_pass(&one, &b, &keys).await;
    }
    let on_b: Vec<Vec<u8>> = live_rows_of(&b, ACCOUNT_STATE_SCOPE)
        .await
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(on_b, vec![one.id().to_vec()], "B holds one's own row");
    let three = Seat::new(3).await;
    three.join(&b, &keys).await;
    settle_bound(&three, &b, &keys).await;
    assert_eq!(
        three
            .store
            .state(&item.kind, &item.key)
            .await
            .unwrap()
            .map(|e| e.value),
        Some(value),
        "a fresh seat walking B reads the record"
    );
}

/// A read marker's item and value at `through`.
fn read_marker_put(through: u64) -> (ItemId, Vec<u8>) {
    (
        ItemId {
            kind: fauna_protocol::merge_policy::KIND_READ_MARKER.into(),
            key: fauna_core::read_marker::channel_key(&"5e".repeat(32)),
        },
        fauna_core::encoding::canonical_encode(&fauna_core::read_marker::ReadMarker::new(through))
            .unwrap(),
    )
}

/// **No row is lost under a stale view** (`delegable-scope-reclamation.md`
/// § Delegable-scope reclamation, *Why no row is lost*). Three seats hold
/// equal rows of one read marker, and seat three's fleet view lacks seat
/// two's enrollment (it walked the fleet scope before two joined), and two's
/// row is served last — equal, later, and unvouched in three's view, so
/// three hands the item over. Every seat runs the cover step, round after
/// round: the item keeps a live row after every step, one is left in the
/// end, and a fresh seat reads the marker from it.
#[tokio::test]
async fn no_row_is_lost_under_a_stale_fleet_view() {
    let rpc = nest().await;
    let keys = schedule();
    let (one, two, three) = (Seat::new(1).await, Seat::new(2).await, Seat::new(3).await);
    one.join(&rpc, &keys).await;
    three.join(&rpc, &keys).await;
    settle_bound(&one, &rpc, &keys).await;
    settle_bound(&three, &rpc, &keys).await;
    two.join(&rpc, &keys).await;
    settle_bound(&one, &rpc, &keys).await;
    settle_bound(&two, &rpc, &keys).await;
    let (item, value) = read_marker_put(5);
    // Two's row — the one seat three cannot vouch for — is served last.
    for seat in [&one, &three, &two] {
        seat_delegable(seat, &rpc, &keys)
            .put(&item, value.clone(), None)
            .await
            .unwrap();
    }
    assert_eq!(
        live_rows_of(&rpc, ACCOUNT_STATE_SCOPE).await.len(),
        3,
        "three equal rows, none named the others"
    );
    // Seat three's view is stale: two's enrollment is not merged there.
    assert!(
        three
            .store
            .state(KIND_DEVICE_SET, &hex::encode(two.id()))
            .await
            .unwrap()
            .is_none()
    );
    for _ in 0..3 {
        for seat in [&three, &one, &two] {
            cover_pass(seat, &rpc, &keys).await;
            let live = live_rows_of(&rpc, ACCOUNT_STATE_SCOPE).await;
            assert!(!live.is_empty(), "the item kept a live row");
        }
    }
    let live = live_rows_of(&rpc, ACCOUNT_STATE_SCOPE).await;
    assert_eq!(live.len(), 1, "one live row is left of the item");
    let fresh = Seat::new(4).await;
    fresh.join(&rpc, &keys).await;
    settle_bound(&fresh, &rpc, &keys).await;
    assert_eq!(
        fresh
            .store
            .state(&item.kind, &item.key)
            .await
            .unwrap()
            .map(|e| e.value),
        Some(value),
        "every live row opens to the merged entry"
    );
}

/// **At a linked nest the leg retires what that nest lists below a cover it
/// lists** (`delegable-scope-reclamation.md` § Delegable-scope reclamation,
/// "A full scope is owed a put that needs no room", *At a linked nest*;
/// `account-sync-plane.md` § The bind leg, ruling 4). Two nests, each
/// linked at the other, both hold seat one's moderation row and, served
/// after it, seat two's winning one — a stale row and its cover. One leg run
/// each way leaves each linked nest with the cover alone, and no retire
/// record is entered for it.
#[tokio::test]
async fn a_linked_leg_leaves_the_linked_nest_with_the_cover_alone() {
    use fauna_sync_engine::linked_leg::{
        LinkedConnection, LinkedCtx, LinkedNestTarget, LinkedOutcome, complete_linked_nest,
    };
    let (a, b) = (nest().await, nest().await);
    let keys = schedule();
    let (one, two) = (Seat::new(1).await, Seat::new(2).await);
    for seat in [&one, &two] {
        seat.join(&a, &keys).await;
        seat.join(&b, &keys).await;
    }
    // Two writes over one's row unwalked, so its put names nothing: on A the
    // stale row and its cover are both live.
    for (seat, keyword, at) in [(&one, "one", 100), (&two, "two", 200)] {
        let (item, value, stamp) = seat_moderation(seat, keyword, at);
        seat_delegable(seat, &a, &keys)
            .put(&item, value, stamp)
            .await
            .unwrap();
    }
    let rows = live_rows_of(&a, ACCOUNT_STATE_SCOPE).await;
    assert_eq!(rows.len(), 2);
    // B holds the same two rows, in the same serve order.
    let folder = b
        .state
        .db
        .get_or_create_state_scope(&ACTOR, ACCOUNT_STATE_SCOPE)
        .await
        .unwrap();
    let mut ordered: Vec<&LiveRow> = rows.iter().collect();
    ordered.sort_by_key(|r| r.0 != one.id().to_vec());
    for (writer, item_key, seq, sealed) in ordered {
        b.state
            .db
            .record_account_state_entry(
                &ACTOR,
                folder,
                &<[u8; 32]>::try_from(item_key.as_slice()).unwrap(),
                &<[u8; 32]>::try_from(writer.as_slice()).unwrap(),
                *seq,
                "state-put",
                sealed,
                None,
                &[],
            )
            .await
            .unwrap()
            .expect("room");
    }
    // Two walks both nests, so no walker's mark holds a retire back.
    settle_bound(&two, &a, &keys).await;
    settle_bound(&two, &b, &keys).await;
    let cover: std::collections::BTreeSet<LiveRow> = rows
        .iter()
        .filter(|r| r.0 == two.id().to_vec())
        .cloned()
        .collect();

    for (bound, linked, nest_id) in [(&a, &b, [0x0B; 32]), (&b, &a, [0x0A; 32])] {
        // One walks the bound nest: two's row wins its merge.
        settle_bound(&one, bound, &keys).await;
        let bound_fleet = one.plane(bound, &keys);
        let ctx = LinkedCtx {
            store: &one.store,
            bound_fleet: &bound_fleet,
            schedule: &keys,
            trust: one.trust(),
            writer_key: &one.key,
            custody: bound_fleet.generation_custody(),
        };
        let outcome = complete_linked_nest(
            &ctx,
            &LinkedNestTarget {
                nest_id,
                nest_url: "https://linked.test".into(),
                replica: true,
            },
            &LinkedConnection {
                rpc: linked.clone(),
                bound_identity: nest_id,
            },
            &[],
            &[],
        )
        .await;
        let LinkedOutcome::Completed(done) = outcome else {
            panic!("the leg ran: {outcome:?}");
        };
        assert_eq!(
            done.cover_reclaim.as_ref().map(|c| c.retired),
            Some(1),
            "{done:?}"
        );
        assert_eq!(
            live_rows_of(linked, ACCOUNT_STATE_SCOPE).await,
            cover,
            "the linked nest holds the cover alone"
        );
    }
    assert!(
        one.store.issued_retires().await.unwrap().is_empty(),
        "a linked nest's retire enters no retire record"
    );
}

/// The nest is away for every request: a write stays local.
#[derive(Clone)]
struct Away;

impl RpcRequester for Away {
    type Error = Refused;
    async fn request<Req, Reply>(
        &self,
        _kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        Err(Refused(RpcError::new("unavailable", "the nest is away")))
    }
}

/// A peer's feed: `store`'s relay plane served past the requester's frontier,
/// the shape `fauna-peer-sync`'s server answers a state-entry walk with.
struct PeerFeed<'a>(&'a AccountStore<SqliteBackend>);

impl RpcRequester for PeerFeed<'_> {
    type Error = anyhow::Error;
    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(
            kind == "fauna.sync.changes.list",
            "a peer serves the feed only"
        );
        let req: SyncChangesListRequest =
            fauna_protocol::decode_strict(&encode_canonical(&payload)?)?;
        let frontier: Vec<(WriterId, u64)> = req
            .frontier
            .unwrap_or_default()
            .into_iter()
            .map(|(w, seq)| {
                (
                    WriterId(hex::decode(w).unwrap().try_into().unwrap()),
                    u64::try_from(seq).unwrap(),
                )
            })
            .collect();
        let rows = self
            .0
            .relay_rows(
                req.scope.as_deref().unwrap(),
                ItemClass::StateEntry.as_wire(),
                &frontier,
                512,
            )
            .await?;
        let reply = SyncChangesListReply {
            changes: rows
                .into_iter()
                .map(|row| fauna_protocol::sync::SyncChange {
                    seq: row.writer_seq as i64,
                    path_hash: hex::encode(&row.item_key),
                    size_bytes: row.entry.as_ref().map_or(0, |e| e.len() as i64),
                    change_type: row.op,
                    item_class: Some(row.item_class),
                    origin_writer: Some(row.writer.to_hex()),
                    origin_seq: Some(row.writer_seq as i64),
                    entry: row.entry.map(ByteBuf::from),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        Ok(fauna_protocol::decode_strict(&encode_canonical(&reply)?)?)
    }
}

/// **The writer after the relay.** Seat 2 writes while its nest is away; the
/// row reaches seat 1 over the peer leg, and seat 1's pass relays it to the
/// nest verbatim before seat 2 has published it. Seat 2's own publish then
/// re-seals the row at the same coordinate — fresh nonce, other bytes — and
/// the nest refuses it for good; the walk's self-echo settles seat 2's own
/// slot. The row is live at the nest once, under seat 2's coordinate, and
/// nothing was re-authored: no second journal row, no writer rotation.
#[tokio::test]
async fn a_row_relayed_before_its_writer_publishes_settles_by_the_writers_self_echo() {
    let rpc = nest().await;
    let keys = schedule();
    let (one, two) = (Seat::new(1).await, Seat::new(2).await);
    for seat in [&one, &two] {
        seat.join(&rpc, &keys).await;
    }
    settle_bound(&one, &rpc, &keys).await;

    let (item, value, stamp) = seat_moderation(&two, "offline", 300);
    assert!(
        seat_delegable(&two, &Away, &keys)
            .put(&item, value, stamp)
            .await
            .is_err(),
        "the local write lands; its publish leg fails"
    );
    let two_writer = WriterId(two.id());
    let written: Vec<_> = two
        .store
        .scope_rows(ACCOUNT_STATE_SCOPE, &two_writer, 0, u32::MAX)
        .await
        .unwrap();
    assert_eq!(written.len(), 1);
    let seq = written[0].seq;

    // Over the peer leg to seat 1, then to the nest by seat 1's diff.
    AccountStatePlane::new_pull_only(
        &one.store,
        &PeerFeed(&two.store),
        &keys,
        &one.key,
        one.trust(),
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap()
    .walk()
    .await
    .unwrap();
    settle_bound(&one, &rpc, &keys).await;
    let live_of_two = |rows: std::collections::BTreeSet<LiveRow>| {
        rows.into_iter()
            .filter(|r| r.0 == two.id().to_vec())
            .map(|r| r.2)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        live_of_two(live_rows_of(&rpc, ACCOUNT_STATE_SCOPE).await),
        vec![seq as i64],
        "seat 1 relayed seat 2's row"
    );

    // Seat 2's next pass: the ordered own publish, then the walk.
    let p = seat_delegable(&two, &rpc, &keys);
    let _refused = p.publish_pending().await;
    p.reconcile().await.unwrap();
    assert_eq!(
        two.store
            .frontier(ACCOUNT_STATE_SCOPE)
            .await
            .unwrap()
            .into_iter()
            .find(|(w, _)| *w == two_writer)
            .map(|(_, s)| s),
        Some(seq),
        "seat 2's own slot is settled by its self-echo"
    );
    assert_eq!(
        live_of_two(live_rows_of(&rpc, ACCOUNT_STATE_SCOPE).await),
        vec![seq as i64],
        "the row is live at the nest once"
    );
    assert_eq!(two.store.writer(), two_writer, "no writer rotation");
    assert_eq!(
        two.store
            .scope_rows(ACCOUNT_STATE_SCOPE, &two_writer, 0, u32::MAX)
            .await
            .unwrap()
            .len(),
        1,
        "nothing re-authored"
    );
    assert!(
        p.publish_pending().await.is_ok(),
        "and the next publish has nothing left to send"
    );
}

//! W2.6 (account-data-plane.md § Workstreams) — the peer leg proven store↔store over the transport seam
//! (`docs/goal/architecture/account-data-plane.md` § The peer leg).
//!
//! Two real `fauna-account-store` replicas of one account converge with **no
//! nest anywhere**: each runs the peer-sync serve side over the
//! substrate-agnostic `fauna_transport::PeerTransport` seam, each dials the
//! other, admits it via a root-signed `DeviceAuthorization` witness (the
//! admission seam — verdict-consuming core, witness verified against the
//! channel-proven key), and runs the SAME W2.4 walk
//! (`AccountStatePlane`, constructed pull-only) against the peer's relay
//! plane that the nest leg runs against the nest feed — "the same sync
//! contract over a different transport", literally. Blocks travel by
//! want-list pull, content-address-verified on write.
//!
//! The transport here is an in-memory `PeerTransport` impl — the seam's own
//! object-safe surface, exactly what `fauna_iroh::IrohTransport` implements
//! (the engine⇄QUIC composition rides the same seam; the channel-over-iroh
//! composition is pinned in `fauna-iroh`'s own loopback tests).
//!
//! Every assertion is on latency-independent state (e2e convention 14): the
//! walks are driven explicitly, and admission and quotas run on an injected
//! clock. The file's one timed construct is [`await_listening`]'s deadline
//! poll — a causal barrier on the node's listener registration, with a budget
//! a green run pays a single tick of; there is no settle-sleep anywhere.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{RecordIndexEntry, WriterId};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::ContentHash;
use fauna_core::data::{Capability, DeviceAuthorization, ModerationConfig, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_core::read_marker::{ReadMarker, channel_key};
use fauna_core::seen_set::SeenScopeSet;
use fauna_peer_channel::PeerChannel;
use fauna_peer_sync::quota::QuotaConfig;
use fauna_peer_sync::server::{
    NowFn, PeerSyncServer, PeerSyncServerConfig, ServeStoreHandle, start_peer_sync_node,
};
use fauna_peer_sync::{AdmissionViews, PeerRequester, admit_over_as, pull_missing_blocks};
use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
use fauna_protocol::merge_policy::{
    KIND_MODERATION, KIND_READ_MARKER, KIND_SEEN_SET, LwwStamp, MODERATION_KEY,
};
use fauna_protocol::scope::ContentScope;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{Value, decode_strict, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId, WalkReport};
use fauna_sync_engine::content_scope_plane::{ContentScopePlane, ContentWalkReport};
use fauna_transport::testing::{Listeners, MemTransport, OnPath, PathCell, await_listening};
use fauna_transport::{EndpointKey, NestPath, PathCandidates, PathKind, PeerTransport};

// ── The in-memory transport: the shared seam double ─────────────────────────
// Lifted to `fauna_transport::testing` (feature `test-helpers`) when W5.7's
// runtime conformance became its second consumer; this file keeps only the
// fixtures built over it.

// ── Fixtures: one account, two device replicas ───────────────────────────────

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([9u8; 32])
}

fn account() -> [u8; 32] {
    root().actor_id().0
}

/// Both replicas derive the same schedule from the same owner `BackupKey` —
/// their writers differ, their keys never do (Path A-sibling-2).
fn schedule() -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&BackupKey::derive(&[7u8; 32]))
}

fn signing_key(device: u8) -> SigningKey {
    SigningKey::from_bytes(&[device; 32])
}

/// The fixture escrow holder. v1's holder is the nest deployment identity,
/// but trust is holder-generic by contract — "whose receipts do I accept",
/// not "who is a nest" — so a local key is the honest fixture, and it is what
/// lets `the_top_up_pass_un_partitions_a_later_enrolled_replica` seal a
/// `GenerationTip` kind with no nest in the file.
fn holder_key() -> SigningKey {
    SigningKey::from_bytes(&[0x66u8; 32])
}

/// The plane's R14 (account-data-plane.md § The ratified decisions) writer-door trust: the account root, no priors, and the
/// one fixture escrow holder above. Harmless to the tests that seal nothing
/// under a generation — with no mint rows merged, no tip resolves either way.
fn trust() -> fauna_sync_engine::generation_tip::GenerationTrust {
    fauna_sync_engine::generation_tip::GenerationTrust {
        root: root().actor_id(),
        prior: Vec::new(),
        trusted_holders: vec![holder_key().verifying_key().to_bytes()].into(),
    }
}

fn writer_id(device: u8) -> WriterId {
    WriterId(signing_key(device).verifying_key().to_bytes())
}

/// The device's root-signed admission witness; its `device_key` IS the
/// replica's transport identity (NodeId = device principal, R5).
fn witness(device: u8, expires_at: Option<u64>) -> EmbedAsBytes {
    let cert = DeviceAuthorization {
        actor_id: root().actor_id(),
        device_key: writer_id(device).0,
        // Deliberately carrier-narrow: admission never consults capabilities.
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: expires_at.map(Timestamp),
    };
    let (bytes, env) = sign_envelope(&root(), &cert).expect("sign witness");
    EmbedAsBytes::from_signed(bytes, env)
}

/// One replica: its plane-side store, its serve-side node on the shared
/// in-memory network, and its transport (for dialing out).
struct Replica {
    device: u8,
    store: AccountStore<SqliteBackend>,
    transport: Arc<MemTransport>,
    /// The pump-refreshed removed-device snapshot, shared with this
    /// replica's serve config and its dial the way `peer_leg::PeerLegState`
    /// shares its own, and derived by the same
    /// `fleet_removal::removed_device_ids` ([`Replica::refresh_removed_devices`]).
    /// It models a DERIVED snapshot only — empty until the first refresh,
    /// which is why every other test in this file is untouched by it. The
    /// production type (`peer_leg::WithdrawalSnapshot`, behind
    /// `account-runtime`, which this file does not build with) also has an
    /// underived state that refuses; that half is pinned in `peer_leg`'s own
    /// tests over the production ensure step.
    removed_devices: Arc<std::sync::RwLock<std::collections::HashSet<[u8; 32]>>>,
    // Held for their Drop: the listener lives exactly as long as the node
    // (wormability rule 5 — no unconditional bind).
    _node: fauna_peer_channel::PeerNode,
    _dir: tempfile::TempDir,
}

/// The advertised set every start goes through (rule 7's gate).
fn caps() -> Vec<String> {
    vec!["peer-sync".to_string()]
}

async fn replica(
    device: u8,
    listeners: &Listeners,
    now: &Arc<AtomicU64>,
    witness_expires_at: Option<u64>,
    quotas: QuotaConfig,
) -> Replica {
    let dir = tempfile::tempdir().expect("replica dir");
    let actor_hex = hex::encode(account());
    // The plane-side store and the serve-side store are two connections to
    // the SAME WAL store dir — the store's own multi-connection posture.
    let store = AccountStore::open(
        SqliteBackend::open(dir.path()).unwrap(),
        &actor_hex,
        writer_id(device),
    )
    .await
    .unwrap();
    let serve_store = AccountStore::open(
        SqliteBackend::open(dir.path()).unwrap(),
        &actor_hex,
        writer_id(device),
    )
    .await
    .unwrap();

    let now = Arc::clone(now);
    let now_fn: NowFn = Arc::new(move || now.load(Ordering::SeqCst));
    let removed_devices: Arc<std::sync::RwLock<std::collections::HashSet<[u8; 32]>>> =
        Arc::new(std::sync::RwLock::new(std::collections::HashSet::new()));
    let server = Arc::new(PeerSyncServer::new(
        ServeStoreHandle::spawn(serve_store),
        account(),
        PeerSyncServerConfig {
            display_name: format!("device-{device:02x}"),
            own_witness: witness(device, witness_expires_at),
            own_witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.to_string(),
            custody_revoked: None,
            device_removed: Some({
                let removed = Arc::clone(&removed_devices);
                Arc::new(move |acct: &[u8; 32], device_key: &[u8; 32]| {
                    *acct == account() && removed.read().unwrap().contains(device_key)
                })
            }),
            file_chunks: None,
            quotas,
            now: now_fn,
        },
    ));
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(writer_id(device).0),
        listeners: Arc::clone(listeners),
    });
    let node = start_peer_sync_node(transport.clone(), server, &caps())
        .await
        .expect("the capability gate is open in fixtures");
    await_listening(listeners, &writer_id(device).0).await;
    Replica {
        device,
        store,
        transport,
        removed_devices,
        _node: node,
        _dir: dir,
    }
}

impl Replica {
    /// Dial `other`, run the mutual admission exchange, and hand back the
    /// admitted channel.
    async fn admitted_channel(&self, other: &Replica, now: &Arc<AtomicU64>) -> Arc<PeerChannel> {
        self.try_admitted_channel(other, now)
            .await
            .expect("mutual admission")
    }

    /// [`Self::admitted_channel`] with the refusal surfaced — the shape the
    /// severance pins need, since a refused admission is the assertion.
    async fn try_admitted_channel(
        &self,
        other: &Replica,
        now: &Arc<AtomicU64>,
    ) -> anyhow::Result<Arc<PeerChannel>> {
        self.try_admitted_channel_via(self.transport.as_ref(), other, now)
            .await
    }

    /// [`Self::try_admitted_channel`] dialed over `transport` — this
    /// replica's own, wrapped (`fauna_transport::testing::OnPath` puts the
    /// channel on a relayed path).
    async fn try_admitted_channel_via(
        &self,
        transport: &dyn PeerTransport,
        other: &Replica,
        now: &Arc<AtomicU64>,
    ) -> anyhow::Result<Arc<PeerChannel>> {
        let conn = transport
            .dial(
                EndpointKey::from_bytes(writer_id(other.device).0),
                PathCandidates::default(),
            )
            .await
            .expect("dial");
        let channel = PeerChannel::open(conn).await.expect("channel");
        // Client side of the exchange: present ours, independently verify the
        // responder's witness against the channel-proven identity — and
        // against THIS side's own removed-device snapshot, the production
        // dial's views (`peer_leg::dial_one`).
        let removed = Arc::clone(&self.removed_devices);
        let removed_view = move |acct: &[u8; 32], device_key: &[u8; 32]| {
            *acct == account() && removed.read().unwrap().contains(device_key)
        };
        admit_over_as(
            &channel,
            fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION,
            witness(self.device, None),
            &account(),
            now.load(Ordering::SeqCst),
            AdmissionViews {
                custody_revoked: None,
                device_removed: Some(&removed_view),
            },
            None,
        )
        .await?;
        Ok(Arc::new(channel))
    }

    /// Re-derive this replica's removed-device snapshot from its own merged
    /// device-set rows — the pump's `refresh_removed_devices` step, through
    /// the same `fleet_removal::removed_device_ids` derivation production
    /// uses. Returns how many ids the view now excludes.
    async fn refresh_removed_devices(&self) -> usize {
        let next = fauna_sync_engine::fleet_removal::removed_device_ids(&self.store, &trust())
            .await
            .expect("removed-device snapshot");
        let n = next.len();
        *self.removed_devices.write().unwrap() = next;
        n
    }

    /// Walk `other`'s relay plane through the pull-only plane — the same
    /// W2.4 walk the nest leg runs, over the peer channel.
    async fn walk_peer(&self, channel: &Arc<PeerChannel>) -> WalkReport {
        self.walk_peer_scope(channel, ACCOUNT_STATE_SCOPE).await
    }

    /// [`Self::walk_peer_scope`] with the refusal surfaced — a severed
    /// connection's walk is an error, and that error is the assertion.
    async fn try_walk_peer_scope(
        &self,
        channel: &Arc<PeerChannel>,
        scope: &str,
    ) -> anyhow::Result<WalkReport> {
        let requester = PeerRequester::new(Arc::clone(channel));
        let sk = signing_key(self.device);
        let sched = schedule();
        let tr = trust();
        let plane =
            AccountStatePlane::new_pull_only(&self.store, &requester, &sched, &sk, &tr, scope)
                .unwrap();
        plane.walk().await
    }

    /// [`Self::walk_peer`] against a named scope. The fleet scope is the one
    /// that matters for the generation machinery — a same-account witness
    /// admits `AllOfAccount`, so the machinery kinds ride the peer leg exactly
    /// as the charter says ("plane rows over any leg, no nest handshake door").
    async fn walk_peer_scope(&self, channel: &Arc<PeerChannel>, scope: &str) -> WalkReport {
        let requester = PeerRequester::new(Arc::clone(channel));
        let sk = signing_key(self.device);
        let sched = schedule();
        let tr = trust();
        let plane =
            AccountStatePlane::new_pull_only(&self.store, &requester, &sched, &sk, &tr, scope)
                .unwrap();
        plane.walk().await.expect("peer walk")
    }

    /// Re-present rows this replica has already walked past. A row sealed
    /// under a generation the replica could not key is *skipped*, not
    /// consumed — reconcile is the charter's "a later reconcile re-presents
    /// it", and therefore the read half of the top-up story.
    async fn reconcile_peer_scope(&self, channel: &Arc<PeerChannel>, scope: &str) {
        let requester = PeerRequester::new(Arc::clone(channel));
        let sk = signing_key(self.device);
        let sched = schedule();
        let tr = trust();
        let plane =
            AccountStatePlane::new_pull_only(&self.store, &requester, &sched, &sk, &tr, scope)
                .unwrap();
        plane.reconcile().await.expect("peer reconcile");
    }

    /// A local class-2 write through the pull-only plane: lands on this
    /// replica's log + relay plane, publishes nowhere (there is no nest in
    /// this file).
    async fn put_local(&self, item: &ItemId, value: Vec<u8>, merge_meta: Option<Vec<u8>>) {
        self.put_local_scope(ACCOUNT_STATE_SCOPE, item, value, merge_meta)
            .await;
    }

    /// [`Self::put_local`] against a named scope. Machinery rows must go
    /// through the plane, not `put_state`: only a plane put records the relay
    /// row a peer can serve, which is the whole reason a sibling ever sees a
    /// mint or an enrollment.
    async fn put_local_scope(
        &self,
        scope: &str,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
    ) {
        let requester = NoNest;
        let sk = signing_key(self.device);
        let sched = schedule();
        let tr = trust();
        let plane =
            AccountStatePlane::new_pull_only(&self.store, &requester, &sched, &sk, &tr, scope)
                .unwrap();
        plane.put(item, value, merge_meta).await.expect("local put");
    }
}

/// A canned nest feed — the `record-cid` rows a real nest would serve for
/// one content scope, keyed off the request's `since`. This is how replica A
/// "learns" a scope nest-wise before serving it onward peer-wise, without a
/// live nest anywhere in this file. Refuses every other kind and shape.
struct FakeNestFeed {
    scope: String,
    rows: Vec<SyncChange>,
}

impl fauna_protocol::RpcRequester for FakeNestFeed {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(
            kind == "fauna.sync.changes.list",
            "the canned nest serves the feed only, got {kind}"
        );
        let req: SyncChangesListRequest =
            decode_strict(&encode_canonical(&payload)?).expect("feed request shape");
        anyhow::ensure!(req.item_class.as_deref() == Some("record-cid"));
        anyhow::ensure!(req.scope.as_deref() == Some(self.scope.as_str()));
        let reply = SyncChangesListReply {
            changes: self
                .rows
                .iter()
                .filter(|r| r.seq > req.since)
                .cloned()
                .collect(),
            ..Default::default()
        };
        Ok(decode_strict(&encode_canonical(&reply)?)?)
    }
}

/// One canned `record-cid` feed row: the digest in `path_hash`, the op in
/// `change_type` — the wire shape ruling (d) froze.
fn content_row(seq: i64, op: &str, cid: &ContentHash) -> SyncChange {
    SyncChange {
        seq,
        path_hash: hex::encode(cid.digest()),
        change_type: op.into(),
        item_class: Some("record-cid".into()),
        ..Default::default()
    }
}

/// A requester that refuses every call — a pull-only `put` must never touch
/// the network, and this is what proves it.
#[derive(Clone)]
struct NoNest;

impl fauna_protocol::RpcRequester for NoNest {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::bail!("pull-only put reached the wire ({kind}) — it must be local-only")
    }
}

fn moderation_item() -> ItemId {
    ItemId {
        kind: KIND_MODERATION.into(),
        key: MODERATION_KEY.into(),
    }
}

fn moderation_with(keyword: &str) -> Vec<u8> {
    encode_canonical(&ModerationConfig {
        muted_keywords: vec![keyword.into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec()
}

fn stamp(at_ms: i64, device: u8) -> Option<Vec<u8>> {
    Some(
        LwwStamp {
            at_ms,
            writer: writer_id(device).0,
        }
        .encode()
        .unwrap(),
    )
}

async fn stored(store: &AccountStore<SqliteBackend>, kind: &str, key: &str) -> Vec<u8> {
    store
        .state(kind, key)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("an entry for ({kind}, {key})"))
        .value
}

fn seen_item(referenced_scope: &str) -> ItemId {
    ItemId {
        kind: KIND_SEEN_SET.into(),
        key: referenced_scope.into(),
    }
}

fn seen_with(refs: &[(u8, u64)]) -> Vec<u8> {
    let mut s = SeenScopeSet::new();
    for (writer, seq) in refs {
        s.insert_ref([*writer; 32], *seq);
    }
    encode_canonical(&s).unwrap().to_vec()
}

// ── The convergence proofs ───────────────────────────────────────────────────

const DEVICE_A: u8 = 0x0A;
const DEVICE_B: u8 = 0x0B;

/// The headline proof: two replicas, no nest, divergent class-2 state —
/// after each walks the other over the seam, both hold identical bytes.
/// LWW resolves by stamp; the union CRDT converges by join; and the merged
/// row is on the merging replica's own log for the OTHER side to pull (the
/// pull-both-ways contract).
#[tokio::test(flavor = "multi_thread")]
async fn two_replicas_converge_store_to_store_with_no_nest() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    // Divergent local writes on both sides — the nest-outage picture.
    a.put_local(
        &moderation_item(),
        moderation_with("from-a"),
        stamp(1_000, DEVICE_A),
    )
    .await;
    b.put_local(
        &moderation_item(),
        moderation_with("from-b"),
        stamp(2_000, DEVICE_B),
    )
    .await;
    let post_scope = format!("content:post:{}", "1a".repeat(32));
    a.put_local(&seen_item(&post_scope), seen_with(&[(1, 1)]), None)
        .await;
    b.put_local(&seen_item(&post_scope), seen_with(&[(2, 5)]), None)
        .await;

    // A pulls from B, then B pulls from A (each side dials + admits
    // independently — mutual admission per connection).
    let ab = a.admitted_channel(&b, &now).await;
    a.walk_peer(&ab).await;
    let ba = b.admitted_channel(&a, &now).await;
    b.walk_peer(&ba).await;
    // A pulls once more: B's walk of A produced B's merged seen-set row on
    // B's own log; A needs it to hold the same bytes (pull-both-ways
    // converges in one exchange only when one side already dominates).
    a.walk_peer(&ab).await;

    // LWW: the newer stamp (B's) wins on both replicas.
    let a_mod = stored(&a.store, KIND_MODERATION, MODERATION_KEY).await;
    let b_mod = stored(&b.store, KIND_MODERATION, MODERATION_KEY).await;
    assert_eq!(a_mod, b_mod, "moderation must converge to identical bytes");
    let decoded: ModerationConfig = decode_strict(&a_mod).unwrap();
    assert_eq!(
        decoded.muted_keywords,
        vec![fauna_core::data::MutedKeyword::from("from-b")]
    );

    // CRDT: both converge to the union — a value neither replica wrote.
    let a_seen = stored(&a.store, KIND_SEEN_SET, &post_scope).await;
    let b_seen = stored(&b.store, KIND_SEEN_SET, &post_scope).await;
    assert_eq!(a_seen, b_seen, "seen-set must converge to identical bytes");
    let union: SeenScopeSet = decode_strict(&a_seen).unwrap();
    assert!(union.contains(&[1u8; 32], 1) && union.contains(&[2u8; 32], 5));
}

/// A thread read on one device reads on the other
/// (`conversation-read-state.md` § The proofs it ships with): a marker raised
/// on A reaches B, which never held one; and two concurrent raises for one
/// channel converge to the HIGHER position on both — a max-register, so the
/// device that read less can never pull the other backwards.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_marker_raised_on_one_replica_converges_on_the_other() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    let marker_item = |channel_hex: &str| ItemId {
        kind: KIND_READ_MARKER.into(),
        key: channel_key(channel_hex),
    };
    let marker = |through: u64| {
        encode_canonical(&ReadMarker::new(through))
            .unwrap()
            .to_vec()
    };
    let only_on_a = "a1".repeat(32);
    let contended = "c0".repeat(32);

    a.put_local(&marker_item(&only_on_a), marker(12), None)
        .await;
    a.put_local(&marker_item(&contended), marker(4), None).await;
    b.put_local(&marker_item(&contended), marker(9), None).await;

    let ab = a.admitted_channel(&b, &now).await;
    a.walk_peer(&ab).await;
    let ba = b.admitted_channel(&a, &now).await;
    b.walk_peer(&ba).await;
    a.walk_peer(&ab).await;

    for store in [&a.store, &b.store] {
        let read: ReadMarker =
            decode_strict(&stored(store, KIND_READ_MARKER, &channel_key(&only_on_a)).await)
                .unwrap();
        assert_eq!(read.through, 12, "A's read must reach B unchanged");
        let read: ReadMarker =
            decode_strict(&stored(store, KIND_READ_MARKER, &channel_key(&contended)).await)
                .unwrap();
        assert_eq!(read.through, 9, "concurrent raises converge to the higher");
    }
}

/// Two replicas each hold an under-cap seen-set for one scope
/// whose plain union would seal past the per-entry cap. The join bounds the
/// merged value (`fauna_core::seen_set` § The budget), so both converge — to
/// identical bytes, under the cap, through the merge door — instead of one
/// journaling a row no nest accepts and stalling `publish_pending` on it.
#[tokio::test(flavor = "multi_thread")]
async fn under_cap_seen_sets_whose_union_outgrows_the_cap_still_converge() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    let scope = format!("content:conv:{}", "2b".repeat(32));
    // Writer ids whose bytes encode wide, as a real key's do. Each side
    // passes the writer door on its own (600 refs, about 49 KB sealed, under
    // one writer's share); the unbounded union of the two would not fit.
    const WRITER_A: u8 = 0xA1;
    const WRITER_B: u8 = 0xB2;
    let refs = |writer: u8| -> Vec<(u8, u64)> { (1..=600).map(|seq| (writer, seq)).collect() };
    a.put_local(&seen_item(&scope), seen_with(&refs(WRITER_A)), None)
        .await;
    b.put_local(&seen_item(&scope), seen_with(&refs(WRITER_B)), None)
        .await;

    let ab = a.admitted_channel(&b, &now).await;
    let first = a.walk_peer(&ab).await;
    assert_eq!(first.merged, 1, "A merged B's row at the door: {first:?}");
    assert_eq!(first.unmergeable, 0, "{first:?}");
    let ba = b.admitted_channel(&a, &now).await;
    b.walk_peer(&ba).await;
    a.walk_peer(&ab).await;

    let a_seen = stored(&a.store, KIND_SEEN_SET, &scope).await;
    let b_seen = stored(&b.store, KIND_SEEN_SET, &scope).await;
    assert_eq!(a_seen, b_seen, "seen-set must converge to identical bytes");
    assert!(
        a_seen.len() < fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES,
        "the merged value is {} bytes before sealing",
        a_seen.len()
    );
    let merged: SeenScopeSet = decode_strict(&a_seen).unwrap();
    assert!(merged.is_normal_form(), "within budget, normal form");
    for seq in 1..=600 {
        assert!(
            merged.contains(&[WRITER_A; 32], seq) && merged.contains(&[WRITER_B; 32], seq),
            "membership never shrinks: {seq}"
        );
    }
}

/// The backstop behind the join: a merge the fold cannot bound — a writer
/// population past the budget, the one axis it leaves open — is sized at the
/// merge door exactly as a put is at the writer door, and skipped as
/// unmergeable: nothing journaled, the current value kept, the row not
/// accounted (re-presented by the next walk), so `publish_pending` never
/// meets a row no nest accepts.
#[tokio::test(flavor = "multi_thread")]
async fn a_merge_no_nest_accepts_is_skipped_at_the_door_never_journaled() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    let scope = format!("content:conv:{}", "3c".repeat(32));
    // `count` distinct writers with one ref each: past the budget in writers
    // alone, every ref folds into a watermark — one per writer, about 34 KB
    // sealed for 700, under the cap; the union of two such is over it.
    let population = |from: u16, count: u16| -> Vec<u8> {
        let mut s = SeenScopeSet::new();
        for n in from..from + count {
            let mut writer = [0u8; 32];
            writer[..2].copy_from_slice(&n.to_be_bytes());
            s.insert_ref(writer, 1);
        }
        encode_canonical(&s).unwrap().to_vec()
    };
    let b_bytes = population(700, 700);
    a.put_local(&seen_item(&scope), population(0, 700), None)
        .await;
    b.put_local(&seen_item(&scope), b_bytes.clone(), None).await;

    let ba = b.admitted_channel(&a, &now).await;
    let report = b.walk_peer(&ba).await;
    assert_eq!(report.unmergeable, 1, "{report:?}");
    assert_eq!(report.merged, 0, "{report:?}");
    assert_eq!(
        stored(&b.store, KIND_SEEN_SET, &scope).await,
        b_bytes,
        "the current value stays put; nothing was journaled"
    );
    // Not accounted: the next walk re-presents the row and skips it again.
    let again = b.walk_peer(&ba).await;
    assert_eq!(again.unmergeable, 1, "{again:?}");
}

// ── W5.8: the top-up self-heal pass, end to end ─────────────────────────────

const ESCROW_SEED: [u8; 32] = [0x55u8; 32];

fn fleet_scope() -> &'static str {
    fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE
}

fn machinery_item(kind: &str, key: String) -> ItemId {
    ItemId {
        kind: kind.into(),
        key,
    }
}

/// A device's verified fleet-member shape — the production derivation, where
/// the KEM keypair comes from the same secret bytes as the signing key.
fn fleet_member(device: u8) -> fauna_core::generation::FleetMember {
    fauna_core::generation::FleetMember {
        device_id: writer_id(device).0,
        xwing_pubkey: fauna_core::generation::derive_device_xwing_keypair(&[device; 32])
            .public
            .to_bytes()
            .to_vec(),
        enrolled_at_ms: 5_000,
    }
}

fn enrollment_value(device: u8) -> Vec<u8> {
    let cert = DeviceAuthorization {
        actor_id: root().actor_id(),
        device_key: writer_id(device).0,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(&root(), &cert).expect("sign enrollment");
    let authorization =
        fauna_core::encoding::canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
    fauna_core::encoding::canonical_encode(&fauna_core::generation::sign_device_enrollment(
        &SigningKey::from_bytes(&[device; 32]),
        authorization,
        5_000,
    ))
    .unwrap()
}

/// **The W5.8 proof, and the sentence the charter owed since the R14 build:
/// a later-enrolled replica opens a row *another writer* sealed under a
/// generation minted before it existed.**
///
/// Before the top-up pass this was unreachable by construction, not merely
/// slow: A's generation wraps its key for the member set A saw, B enrolls
/// afterwards, and no amount of syncing ever hands B that key — so every row
/// anyone ever sealed under generation 1 stayed dark on B forever. The
/// partition is asserted here *first*, so the green half cannot be vacuous.
///
/// No nest anywhere, per this file's whole premise: the machinery kinds ride
/// the peer leg because a same-account witness admits `AllOfAccount`, and the
/// escrow holder is a local fixture key because receipt trust is
/// holder-generic ("whose receipts do I accept").
#[tokio::test(flavor = "multi_thread")]
async fn the_top_up_pass_un_partitions_a_later_enrolled_replica() {
    use fauna_core::generation::{
        EscrowTargetRecord, escrow_target_identity_key, sign_escrow_receipt,
    };
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;
    use fauna_protocol::merge_policy::{
        KIND_DEVICE_ENDPOINTS, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT,
    };

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;
    let fleet = fleet_scope();

    // 1. A is the founding device: it enrolls itself, and nothing else exists.
    a.put_local_scope(
        fleet,
        &machinery_item(
            fauna_protocol::merge_policy::KIND_DEVICE_SET,
            fauna_core::hex32::encode(&writer_id(DEVICE_A).0),
        ),
        enrollment_value(DEVICE_A),
        None,
    )
    .await;

    // 2. A mints generation 1 over the only member it can see — itself. This
    //    is the subset mint, and it is entirely correct at the time it happens.
    let escrow = EscrowTargetRecord {
        xwing_escrow_pubkey: fauna_core::generation::derive_escrow_xwing_keypair(&ESCROW_SEED)
            .public
            .to_bytes()
            .to_vec(),
    };
    let built = build_mint(
        &[fleet_member(DEVICE_A)],
        &escrow,
        &escrow_target_identity_key(&root().actor_id()),
        Vec::new(),
        &signing_key(DEVICE_A),
        7_000,
    )
    .expect("mint");
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
        ),
        fauna_core::encoding::canonical_encode(&built.record).unwrap(),
        None,
    )
    .await;
    // The ack the writer door's tip resolution requires: a trusted holder's
    // receipt, bound to the deposited wrap.
    let receipt = sign_escrow_receipt(
        &holder_key(),
        built.generation_id,
        blake3::hash(&built.escrow_wrap).into(),
        &escrow_target_identity_key(&root().actor_id()),
        7_100,
    );
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_ESCROW_RECEIPT,
            format!(
                "{}/{}",
                fauna_core::hex32::encode(&built.generation_id),
                fauna_core::hex32::encode(&receipt.holder_id)
            ),
        ),
        fauna_core::encoding::canonical_encode(&receipt).unwrap(),
        None,
    )
    .await;

    // 3. A seals a real `GenerationTip`-epoch row under that generation —
    //    through the writer door, which resolves the tip it just made
    //    resolvable. This is the row B must end up reading.
    let endpoints = fauna_core::device_endpoints::DeviceEndpoints {
        node_id: writer_id(DEVICE_A).0,
        lan_addrs: vec!["10.0.0.1:9000".to_string()],
        public_addrs: Vec::new(),
        relay_url: None,
    };
    let endpoints_bytes = fauna_core::encoding::canonical_encode(&endpoints).unwrap();
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_DEVICE_ENDPOINTS,
            fauna_core::hex32::encode(&writer_id(DEVICE_A).0),
        ),
        endpoints_bytes.clone(),
        stamp(8_000, DEVICE_A),
    )
    .await;

    // 4. Only NOW does B enroll. Generation 1 has no wrap for it, and never
    //    will have one on its own.
    b.put_local_scope(
        fleet,
        &machinery_item(
            fauna_protocol::merge_policy::KIND_DEVICE_SET,
            fauna_core::hex32::encode(&writer_id(DEVICE_B).0),
        ),
        enrollment_value(DEVICE_B),
        None,
    )
    .await;

    // 5. Converge the fleet scope both ways: A learns B exists, B learns the
    //    device set, the mint, the receipt — and meets A's sealed row.
    let ab = a.admitted_channel(&b, &now).await;
    let ba = b.admitted_channel(&a, &now).await;
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    // The partition, asserted before the fix so the green half below cannot be
    // vacuous: B holds the mint row and still cannot key it, and A's sealed
    // row is not in B's state.
    assert!(
        fauna_sync_engine::generation_tip::generation_key_for(
            &b.store,
            &built.generation_id,
            &signing_key(DEVICE_B),
            None,
            None,
        )
        .await
        .unwrap()
        .is_none(),
        "precondition: B must NOT be able to key a generation minted before it enrolled"
    );
    assert!(
        b.store
            .state(
                KIND_DEVICE_ENDPOINTS,
                &fauna_core::hex32::encode(&writer_id(DEVICE_A).0)
            )
            .await
            .unwrap()
            .is_none(),
        "precondition: A's sealed row cannot have merged on B — B cannot open it"
    );

    // 6. A runs the top-up pass. It sees a member of the merged device set
    //    with no wrap for a generation A can key, and writes one.
    {
        let requester = NoNest;
        let sk = signing_key(DEVICE_A);
        let sched = schedule();
        let tr = trust();
        let plane = AccountStatePlane::new_pull_only(&a.store, &requester, &sched, &sk, &tr, fleet)
            .unwrap();
        assert_eq!(
            fauna_sync_engine::generation_topup::ensure_topped_up(&a.store, &plane, &tr, &sk)
                .await
                .expect("top-up pass"),
            fauna_sync_engine::generation_topup::TopupPass::Published(1)
        );
    }

    // 7. B walks again for the wrap, then reconciles — the charter's "a later
    //    reconcile re-presents it" for the row B skipped when it could not
    //    open it.
    b.walk_peer_scope(&ba, fleet).await;
    b.reconcile_peer_scope(&ba, fleet).await;

    // 8. The proof, in both halves: B can key the pre-enrollment generation...
    let keyed = fauna_sync_engine::generation_tip::generation_key_for(
        &b.store,
        &built.generation_id,
        &signing_key(DEVICE_B),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        keyed.is_some(),
        "B must key generation 1 after the top-up — that is the whole pass"
    );
    // ...and therefore reads the row ANOTHER writer sealed under it, which is
    // the half "not just device-endpoints" is about: nothing in the read path
    // is kind-specific, the generation key was the only gate.
    let merged = b
        .store
        .state(
            KIND_DEVICE_ENDPOINTS,
            &fauna_core::hex32::encode(&writer_id(DEVICE_A).0),
        )
        .await
        .unwrap()
        .expect("A's sealed row must merge on B once B can key its generation");
    assert_eq!(
        merged.value, endpoints_bytes,
        "B must read exactly the bytes A sealed"
    );
}

/// **The proof: the in-member corrupted-wrap partition self-heals
/// end-to-end, no nest anywhere.** A `BackupKey` vandal corrupts B's own
/// inline wrap in place and wins the mint join — B is in the signed member
/// set and a wrap is present, so the healer's coverage reads the pair as
/// healthy (that suppression is asserted here FIRST, so the green half cannot
/// be vacuous). B publishes its target-signed "cannot key" testimony; A's
/// top-up pass treats the verifying signal as clearing both suppression
/// grounds and heals exactly once; B keys the generation, reads the row A
/// sealed under it, and retracts with a `Satisfied` — after which BOTH passes
/// are byte-quiet. The once-per-assertion bound (a crashed target extracts at
/// most one wrap per healer) is asserted before the retraction ever merges.
#[tokio::test(flavor = "multi_thread")]
async fn the_cannot_key_signal_un_partitions_an_in_member_corrupted_wrap() {
    use fauna_core::generation::{
        EscrowTargetRecord, GenerationMintRecord, escrow_target_identity_key, sign_escrow_receipt,
    };
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;
    use fauna_protocol::merge_policy::{
        KIND_DEVICE_ENDPOINTS, KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT,
    };

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;
    let fleet = fleet_scope();

    // 1. Both devices enroll, and A learns B's enrollment before minting.
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&writer_id(DEVICE_A).0),
        ),
        enrollment_value(DEVICE_A),
        None,
    )
    .await;
    b.put_local_scope(
        fleet,
        &machinery_item(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&writer_id(DEVICE_B).0),
        ),
        enrollment_value(DEVICE_B),
        None,
    )
    .await;
    let ab = a.admitted_channel(&b, &now).await;
    let ba = b.admitted_channel(&a, &now).await;
    a.walk_peer_scope(&ab, fleet).await;

    // 2. A mints generation 1 over BOTH members — B's wrap is inline; nothing
    //    is missing, and the ordinary top-up pass rightly has nothing to do.
    let escrow = EscrowTargetRecord {
        xwing_escrow_pubkey: fauna_core::generation::derive_escrow_xwing_keypair(&ESCROW_SEED)
            .public
            .to_bytes()
            .to_vec(),
    };
    let built = build_mint(
        &[fleet_member(DEVICE_A), fleet_member(DEVICE_B)],
        &escrow,
        &escrow_target_identity_key(&root().actor_id()),
        Vec::new(),
        &signing_key(DEVICE_A),
        7_000,
    )
    .expect("mint");
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
        ),
        fauna_core::encoding::canonical_encode(&built.record).unwrap(),
        None,
    )
    .await;
    let receipt = sign_escrow_receipt(
        &holder_key(),
        built.generation_id,
        blake3::hash(&built.escrow_wrap).into(),
        &escrow_target_identity_key(&root().actor_id()),
        7_100,
    );
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_ESCROW_RECEIPT,
            format!(
                "{}/{}",
                fauna_core::hex32::encode(&built.generation_id),
                fauna_core::hex32::encode(&receipt.holder_id)
            ),
        ),
        fauna_core::encoding::canonical_encode(&receipt).unwrap(),
        None,
    )
    .await;

    // 3. A seals a real `GenerationTip`-epoch row under generation 1 — the
    //    row B must end up reading.
    let endpoints = fauna_core::device_endpoints::DeviceEndpoints {
        node_id: writer_id(DEVICE_A).0,
        lan_addrs: vec!["10.0.0.1:9000".to_string()],
        public_addrs: Vec::new(),
        relay_url: None,
    };
    let endpoints_bytes = fauna_core::encoding::canonical_encode(&endpoints).unwrap();
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_DEVICE_ENDPOINTS,
            fauna_core::hex32::encode(&writer_id(DEVICE_A).0),
        ),
        endpoints_bytes.clone(),
        stamp(8_000, DEVICE_A),
    )
    .await;

    // 4. THE VANDALISM: B's inline wrap, corrupted in place — B stays in the
    //    signed `member_ids`, a wrap stays present, and the poisoned variant
    //    wins the mint join, so it becomes THE mint row everywhere it merges.
    let honest_bytes = fauna_core::encoding::canonical_encode(&built.record).unwrap();
    let mut poisoned = built.record.clone();
    let corrupted_hash = {
        let GenerationMintRecord::Minted { wraps, .. } = &mut poisoned else {
            panic!("minted")
        };
        let victim = wraps
            .iter_mut()
            .find(|w| w.device_id == writer_id(DEVICE_B).0)
            .expect("B's inline wrap");
        victim.wrap = vec![0xFF; victim.wrap.len()];
        *blake3::hash(&victim.wrap).as_bytes()
    };
    let poisoned_bytes = fauna_core::encoding::canonical_encode(&poisoned).unwrap();
    assert_eq!(
        fauna_core::generation::join_generation_mint(&honest_bytes, &poisoned_bytes).unwrap(),
        poisoned_bytes,
        "the poisoned variant wins the mint join"
    );
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
        ),
        poisoned_bytes,
        None,
    )
    .await;

    // 5. Converge. B holds the poisoned mint, the receipt, and skips A's
    //    sealed row (it cannot key it).
    b.walk_peer_scope(&ba, fleet).await;

    // The partition AND the suppression, asserted before the fix: B cannot
    // key a generation that visibly wraps it, and A's top-up pass — the
    // hardened one — still reads the pair as covered.
    assert!(
        fauna_sync_engine::generation_tip::generation_key_for(
            &b.store,
            &built.generation_id,
            &signing_key(DEVICE_B),
            None,
            None,
        )
        .await
        .unwrap()
        .is_none(),
        "precondition: the corrupted inline wrap keys nothing on B"
    );
    {
        let requester = NoNest;
        let sk = signing_key(DEVICE_A);
        let sched = schedule();
        let tr = trust();
        let plane = AccountStatePlane::new_pull_only(&a.store, &requester, &sched, &sk, &tr, fleet)
            .unwrap();
        assert_eq!(
            fauna_sync_engine::generation_topup::ensure_topped_up(&a.store, &plane, &tr, &sk)
                .await
                .expect("top-up pass"),
            fauna_sync_engine::generation_topup::TopupPass::Current,
            "precondition: without the signal, the healer reads the pair as covered — \
             this is the durable case"
        );
    }

    // 6. B testifies: a target-signed assertion naming the corrupted wrap.
    {
        let requester = NoNest;
        let sk = signing_key(DEVICE_B);
        let sched = schedule();
        let tr = trust();
        let plane = AccountStatePlane::new_pull_only(&b.store, &requester, &sched, &sk, &tr, fleet)
            .unwrap();
        assert_eq!(
            fauna_sync_engine::generation_unkeyable::ensure_signalled(&b.store, &plane, &tr, &sk)
                .await
                .expect("signal pass"),
            fauna_sync_engine::generation_unkeyable::UnkeyablePass::Published(1)
        );
    }
    let signal = b
        .store
        .state(
            fauna_protocol::merge_policy::KIND_GENERATION_UNKEYABLE,
            &fauna_core::generation::unkeyable_cell_key(
                &built.generation_id,
                &writer_id(DEVICE_B).0,
            ),
        )
        .await
        .unwrap()
        .expect("B's signal row");
    let record: fauna_core::generation::GenerationUnkeyableRecord =
        fauna_core::encoding::canonical_decode(&signal.value).unwrap();
    let fauna_core::generation::GenerationUnkeyableRecord::Asserted { tried, .. } = &record else {
        panic!("asserted");
    };
    assert_eq!(
        tried.as_slice(),
        &[corrupted_hash],
        "the testimony names exactly the wrap B tried and failed"
    );

    // 7. A learns the signal; the suppression clears; exactly one heal —
    //    and the same standing assertion is answered exactly once, which is
    //    the crashed-target bound (B has retracted nothing yet).
    a.walk_peer_scope(&ab, fleet).await;
    {
        let requester = NoNest;
        let sk = signing_key(DEVICE_A);
        let sched = schedule();
        let tr = trust();
        let plane = AccountStatePlane::new_pull_only(&a.store, &requester, &sched, &sk, &tr, fleet)
            .unwrap();
        assert_eq!(
            fauna_sync_engine::generation_topup::ensure_topped_up(&a.store, &plane, &tr, &sk)
                .await
                .expect("top-up pass"),
            fauna_sync_engine::generation_topup::TopupPass::Published(1),
            "B's testimony clears the suppression"
        );
        assert_eq!(
            fauna_sync_engine::generation_topup::ensure_topped_up(&a.store, &plane, &tr, &sk)
                .await
                .expect("top-up pass"),
            fauna_sync_engine::generation_topup::TopupPass::Current,
            "one assertion extracts at most one wrap from this healer, retraction or not"
        );
    }

    // 8. B pulls the heal, keys the generation, and reads the row A sealed.
    b.walk_peer_scope(&ba, fleet).await;
    b.reconcile_peer_scope(&ba, fleet).await;
    assert!(
        fauna_sync_engine::generation_tip::generation_key_for(
            &b.store,
            &built.generation_id,
            &signing_key(DEVICE_B),
            None,
            None,
        )
        .await
        .unwrap()
        .is_some(),
        "B must key the generation after the signalled heal"
    );
    let merged = b
        .store
        .state(
            KIND_DEVICE_ENDPOINTS,
            &fauna_core::hex32::encode(&writer_id(DEVICE_A).0),
        )
        .await
        .unwrap()
        .expect("A's sealed row must merge on B once the heal lands");
    assert_eq!(
        merged.value, endpoints_bytes,
        "B must read exactly the bytes A sealed"
    );

    // 9. The retraction, then byte-quiet on both sides: B's pass publishes
    //    the `Satisfied`, A's pass finds ordinary coverage restored, and a
    //    further round on either replica produces nothing.
    {
        let requester = NoNest;
        let sk = signing_key(DEVICE_B);
        let sched = schedule();
        let tr = trust();
        let plane = AccountStatePlane::new_pull_only(&b.store, &requester, &sched, &sk, &tr, fleet)
            .unwrap();
        assert_eq!(
            fauna_sync_engine::generation_unkeyable::ensure_signalled(&b.store, &plane, &tr, &sk)
                .await
                .expect("signal pass"),
            fauna_sync_engine::generation_unkeyable::UnkeyablePass::Published(1),
            "the standing assertion is retracted"
        );
        assert_eq!(
            fauna_sync_engine::generation_unkeyable::ensure_signalled(&b.store, &plane, &tr, &sk)
                .await
                .expect("signal pass"),
            fauna_sync_engine::generation_unkeyable::UnkeyablePass::Current,
            "a satisfied target goes byte-quiet"
        );
    }
    a.walk_peer_scope(&ab, fleet).await;
    {
        let requester = NoNest;
        let sk = signing_key(DEVICE_A);
        let sched = schedule();
        let tr = trust();
        let plane = AccountStatePlane::new_pull_only(&a.store, &requester, &sched, &sk, &tr, fleet)
            .unwrap();
        assert_eq!(
            fauna_sync_engine::generation_topup::ensure_topped_up(&a.store, &plane, &tr, &sk)
                .await
                .expect("top-up pass"),
            fauna_sync_engine::generation_topup::TopupPass::Current,
            "a converged pair goes byte-quiet on the healer side too"
        );
    }
}

/// The class-1/3 half: a block staged on one replica reaches the other by
/// want-list pull, content-address-verified on write.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_block_travels_by_want_list_pull() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    let post_scope = format!("content:post:{}", "1a".repeat(32));
    let bytes = b"canonical sealed post block bytes";
    let (cid, _seq) = b
        .store
        .stage_local_record(&post_scope, "post", bytes)
        .await
        .unwrap();

    // A knows OF the record (index row) but lacks its bytes — the dehydrated
    // shape a feed row leaves behind.
    a.store
        .note_record(&RecordIndexEntry {
            cid,
            scope: post_scope.clone(),
            kind: "post".into(),
            size: Some(bytes.len() as u64),
        })
        .await
        .unwrap();
    assert!(!a.store.is_present(&cid).await.unwrap());

    let ab = a.admitted_channel(&b, &now).await;
    let report = pull_missing_blocks(&a.store, &ab, &post_scope, NestPath::Reachable)
        .await
        .unwrap();
    assert_eq!((report.fetched, report.missing), (1, 0));
    assert_eq!(
        a.store.block(&cid).await.unwrap().as_deref(),
        Some(&bytes[..]),
        "the pulled block must be held, byte-identical, after its CID check"
    );
}

/// A block staged on B that A indexes but lacks, and A's admitted channel to
/// B over a connection whose path [`Self::path`] sets — relayed to begin
/// with. The ruling 4 pins below share it.
struct RelayedPull {
    a: Replica,
    _b: Replica,
    ab: Arc<PeerChannel>,
    path: PathCell,
    scope: String,
    cid: ContentHash,
    bytes: &'static [u8],
}

async fn a_missing_block_over_a_relayed_channel() -> RelayedPull {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;
    let post_scope = format!("content:post:{}", "1a".repeat(32));
    let bytes: &'static [u8] = b"a block the nest path would carry";
    let (cid, _seq) = b
        .store
        .stage_local_record(&post_scope, "post", bytes)
        .await
        .unwrap();
    a.store
        .note_record(&RecordIndexEntry {
            cid,
            scope: post_scope.clone(),
            kind: "post".into(),
            size: Some(bytes.len() as u64),
        })
        .await
        .unwrap();
    let path = PathCell::new(PathKind::Relay);
    let relayed = OnPath {
        inner: a.transport.clone(),
        path: path.clone(),
    };
    let ab = a
        .try_admitted_channel_via(&relayed, &b, &now)
        .await
        .expect("mutual admission over the relayed path");
    RelayedPull {
        a,
        _b: b,
        ab,
        path,
        scope: post_scope,
        cid,
        bytes,
    }
}

/// `p2p.md` § The relay, ruling 4: over a relayed connection, with the nest
/// path reachable, the want-list pull moves no byte — the block is counted
/// deferred, never failed — and the path is re-read before each pull, so the
/// same channel turned direct pulls it.
#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_channel_leaves_the_bytes_to_a_reachable_nest() {
    let RelayedPull {
        a,
        _b,
        ab,
        path,
        scope: post_scope,
        cid,
        bytes,
    } = a_missing_block_over_a_relayed_channel().await;

    let report = pull_missing_blocks(&a.store, &ab, &post_scope, NestPath::Reachable)
        .await
        .unwrap();
    assert_eq!(
        (report.fetched, report.missing, report.relay_deferred),
        (0, 0, 1),
        "a relayed path with the nest reachable pulls nothing: {report:?}"
    );
    assert!(
        !a.store.is_present(&cid).await.unwrap(),
        "no byte crossed the relayed channel"
    );

    path.set(PathKind::WanDirect);
    let report = pull_missing_blocks(&a.store, &ab, &post_scope, NestPath::Reachable)
        .await
        .unwrap();
    assert_eq!(
        (report.fetched, report.relay_deferred),
        (1, 0),
        "the channel the substrate turned direct carries the bytes: {report:?}"
    );
    assert_eq!(a.store.block(&cid).await.unwrap().as_deref(), Some(bytes));
}

/// Ruling 4's one exception: the nest path unavailable to this device, an
/// already-standing relayed connection carries the bytes too.
#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_channel_carries_the_bytes_when_the_nest_path_is_unavailable() {
    let RelayedPull {
        a,
        _b,
        ab,
        scope: post_scope,
        cid,
        bytes,
        ..
    } = a_missing_block_over_a_relayed_channel().await;

    let report = pull_missing_blocks(&a.store, &ab, &post_scope, NestPath::Unavailable)
        .await
        .unwrap();
    assert_eq!(
        (report.fetched, report.missing, report.relay_deferred),
        (1, 0, 0),
        "{report:?}"
    );
    assert_eq!(a.store.block(&cid).await.unwrap().as_deref(), Some(bytes));
}

/// The W3 relay: a brand-new record's **coordinates** travel peer-wise — the
/// gap W2.6 stated ("a brand-new record on one device reaches a sibling only
/// nest-mediated"). A learns a content scope's feed nest-wise (a canned nest
/// here — no live nest anywhere in this file); B then converges its index off
/// A's relay plane alone, running the SAME `ContentScopePlane` walk over the
/// peer channel that the nest leg runs over WS-RPC, and pulls the new
/// record's bytes by want-list. Coordinates and bytes both peer-wise: the
/// record reaches B with no nest on any leg.
#[tokio::test(flavor = "multi_thread")]
async fn a_brand_new_records_coordinates_travel_peer_wise() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    let scope = ContentScope::new("post", [0x1A; 32]).unwrap();
    let scope_str = scope.to_string();
    let live_bytes = b"a brand-new post record, dag-cbor-canonical for its CID";
    let live = ContentHash::of_dag_cbor(live_bytes);
    let dead = ContentHash::of_dag_cbor(b"a record the nest deleted again");

    // A walks the nest's feed: the new record arrives, another comes and
    // goes — the collapse A's relay plane must mirror (one live row per
    // record: the add for `live`, the tombstone for `dead`).
    let nest = FakeNestFeed {
        scope: scope_str.clone(),
        rows: vec![
            content_row(1, "record-added", &live),
            content_row(2, "record-added", &dead),
            content_row(3, "tombstone", &dead),
        ],
    };
    let report = ContentScopePlane::new(&a.store, &nest, scope.clone())
        .walk()
        .await
        .expect("A's nest-leg walk");
    assert_eq!((report.indexed, report.tombstoned), (2, 1));
    a.store
        .hydrate(&live, live_bytes)
        .await
        .expect("A holds the new record's bytes");

    // B converges off A's relay plane alone — the identical walk, over the
    // peer channel.
    let ba = b.admitted_channel(&a, &now).await;
    let requester = PeerRequester::new(Arc::clone(&ba));
    let plane = ContentScopePlane::new(&b.store, &requester, scope.clone());
    let report = plane.walk().await.expect("B's peer-leg walk");
    assert_eq!(
        (report.pages, report.rows, report.indexed, report.tombstoned),
        (1, 2, 1, 1),
        "the relay serves the collapse: one add, one tombstone"
    );
    assert!(
        b.store.record(&live).await.unwrap().is_some(),
        "the new record is indexed on B"
    );
    assert!(
        b.store.record(&dead).await.unwrap().is_none(),
        "the deleted record never survives on B"
    );
    // The frontier slot is the nest's own coordinate, learned peer-wise — a
    // later nest-leg walk resumes past everything B already holds.
    let frontier = b.store.frontier(&scope_str).await.unwrap();
    assert!(
        frontier.contains(&(WriterId::NEST_SEQUENCER, 3)),
        "B accounts the nest-sequencer slot at 3, got {frontier:?}"
    );
    // Chaining: B's own ingest re-fed B's relay plane, so a third sibling
    // could now converge off B the same way.
    let relayed = b
        .store
        .relay_rows(&scope_str, "record-cid", &[], 100)
        .await
        .unwrap();
    let ops: Vec<(u64, &str)> = relayed
        .iter()
        .map(|r| (r.writer_seq, r.op.as_str()))
        .collect();
    assert_eq!(ops, vec![(1, "record-added"), (3, "tombstone")]);

    // A walk that asks again finds nothing new (the cursor holds). The report
    // still names its scope — that field says what was walked, not what moved.
    let report = plane.walk().await.expect("B's second walk");
    assert_eq!(
        report,
        ContentWalkReport {
            scope: scope_str.clone(),
            ..Default::default()
        }
    );

    // The bytes ride the shipped want-list pull beside the coordinates.
    let report = pull_missing_blocks(&b.store, &ba, &scope_str, NestPath::Reachable)
        .await
        .unwrap();
    assert_eq!((report.fetched, report.missing), (1, 0));
    assert_eq!(
        b.store.block(&live).await.unwrap().as_deref(),
        Some(&live_bytes[..])
    );

    // The loud-refusal rule extends to the new arm: a record-cid request
    // with no scope is refused, never answered from an implied plane (an
    // empty page would read as "converged" to a walk).
    let scopeless: Value = decode_strict(
        &encode_canonical(&SyncChangesListRequest {
            item_class: Some("record-cid".into()),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let err = ba
        .request("fauna.sync.changes.list", scopeless)
        .await
        .expect_err("a scope-less record-cid request must be refused");
    assert!(
        err.to_string().contains("fauna.peer.sync.unsupported"),
        "{err}"
    );
}

/// The admission seam holds at the serve door: an un-admitted connection is
/// refused on every transfer kind (never served, never hung), and a kind
/// outside the allowlist answers `unknown_kind` (wormability rule 3).
#[tokio::test(flavor = "multi_thread")]
async fn transfer_kinds_refuse_before_admission_and_the_allowlist_holds() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    // A raw channel with NO admission exchange.
    let conn = a
        .transport
        .dial(
            EndpointKey::from_bytes(writer_id(b.device).0),
            PathCandidates::default(),
        )
        .await
        .unwrap();
    let channel = PeerChannel::open(conn).await.unwrap();

    let list_req: Value = decode_strict(
        &encode_canonical(&SyncChangesListRequest {
            item_class: Some("state-entry".into()),
            scope: Some(ACCOUNT_STATE_SCOPE.into()),
            frontier: Some(Default::default()),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let err = channel
        .request("fauna.sync.changes.list", list_req)
        .await
        .expect_err("un-admitted changes.list must be refused");
    assert!(
        err.to_string().contains("fauna.peer.sync.not_admitted"),
        "{err}"
    );

    // Rule 3: a kind outside the allowlist does not exist on this dispatcher.
    let err = channel
        .request("fauna.admin.audit_log.list", Value::Null)
        .await
        .expect_err("an off-allowlist kind must be unknown");
    assert!(
        err.to_string().contains("fauna.protocol.unknown_kind"),
        "{err}"
    );
}

/// The verdict's validity bound (the seam: lifetime ≤ witness expiry): a
/// connection outliving its witness is refused until it re-presents — driven
/// on the injected clock, no wall time.
#[tokio::test(flavor = "multi_thread")]
async fn an_expired_witness_must_re_present_before_continuing() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    // B's server is fine with A's witness expiring at 6_000 — the client
    // presents it below.
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;

    let conn = a
        .transport
        .dial(
            EndpointKey::from_bytes(writer_id(b.device).0),
            PathCandidates::default(),
        )
        .await
        .unwrap();
    let channel = PeerChannel::open(conn).await.unwrap();
    // Admit with a witness that expires at 6_000.
    let admit_req: Value = decode_strict(
        &encode_canonical(&fauna_protocol::peer_sync::PeerSyncAdmitRequest {
            witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.into(),
            witness: witness(DEVICE_A, Some(6_000)),
            endpoints: None,
            extra: Default::default(),
        })
        .unwrap(),
    )
    .unwrap();
    channel
        .request(fauna_protocol::peer_sync::KIND_PEER_SYNC_ADMIT, admit_req)
        .await
        .expect("admission within validity");

    let list_req = || -> Value {
        decode_strict(
            &encode_canonical(&SyncChangesListRequest {
                item_class: Some("state-entry".into()),
                scope: Some(ACCOUNT_STATE_SCOPE.into()),
                frontier: Some(Default::default()),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap()
    };
    channel
        .request("fauna.sync.changes.list", list_req())
        .await
        .expect("serving within the validity bound");

    // The clock passes the witness expiry — the verdict lapses.
    now.store(7_000, Ordering::SeqCst);
    let err = channel
        .request("fauna.sync.changes.list", list_req())
        .await
        .expect_err("a lapsed verdict must refuse until re-presented");
    assert!(
        err.to_string().contains("fauna.peer.sync.not_admitted"),
        "{err}"
    );
}

/// Rule 7, the fleet brake: without the nest's `peer-sync` capability the
/// leg refuses to come up at its one bind site.
#[tokio::test(flavor = "multi_thread")]
async fn the_fleet_brake_refuses_to_start_the_leg() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now: Arc<AtomicU64> = Arc::new(AtomicU64::new(5_000));
    let now_fn: NowFn = {
        let now = Arc::clone(&now);
        Arc::new(move || now.load(Ordering::SeqCst))
    };
    let dir = tempfile::tempdir().unwrap();
    let serve_store = AccountStore::open(
        SqliteBackend::open(dir.path()).unwrap(),
        &hex::encode(account()),
        writer_id(DEVICE_A),
    )
    .await
    .unwrap();
    let server = Arc::new(PeerSyncServer::new(
        ServeStoreHandle::spawn(serve_store),
        account(),
        PeerSyncServerConfig {
            display_name: "braked".into(),
            own_witness: witness(DEVICE_A, None),
            own_witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.to_string(),
            custody_revoked: None,
            device_removed: None,
            file_chunks: None,
            quotas: QuotaConfig::default(),
            now: now_fn,
        },
    ));
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(writer_id(DEVICE_A).0),
        listeners,
    });

    let err = match start_peer_sync_node(transport, server, &[]).await {
        Ok(_) => panic!("no capability must mean no listener"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("peer-sync"), "{err}");
}

/// Rule 8 at the wire: per-peer request quotas refuse — the admission
/// exchange itself included (a refused witness spends budget too).
#[tokio::test(flavor = "multi_thread")]
async fn request_quotas_refuse_over_the_wire() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(
        DEVICE_B,
        &listeners,
        &now,
        None,
        QuotaConfig {
            conns_per_window: 32,
            requests_per_window: 1,
            window_secs: 60,
            ..QuotaConfig::default()
        },
    )
    .await;

    let conn = a
        .transport
        .dial(
            EndpointKey::from_bytes(writer_id(b.device).0),
            PathCandidates::default(),
        )
        .await
        .unwrap();
    let channel = PeerChannel::open(conn).await.unwrap();
    let admit_req = || -> Value {
        decode_strict(
            &encode_canonical(&fauna_protocol::peer_sync::PeerSyncAdmitRequest {
                witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.into(),
                witness: witness(DEVICE_A, None),
                endpoints: None,
                extra: Default::default(),
            })
            .unwrap(),
        )
        .unwrap()
    };
    channel
        .request(fauna_protocol::peer_sync::KIND_PEER_SYNC_ADMIT, admit_req())
        .await
        .expect("first request is within budget");
    let err = channel
        .request(fauna_protocol::peer_sync::KIND_PEER_SYNC_ADMIT, admit_req())
        .await
        .expect_err("the second request must be over quota");
    assert!(
        err.to_string().contains("fauna.peer.sync.over_quota"),
        "{err}"
    );
}

// ── Removal severs the peer leg ──────
//
// The `DeviceAuthorization` the fleet mints carries `expires_at: None` on
// purpose — "removal is the control" — so if nothing on this leg consults
// device removal, a removed device keeps an `AllOfAccount` verdict forever at
// every sibling that answers its dial. These two pins are the inverse of the
// probe that found it: the same two replicas, the same `Removed` row, the
// opposite outcome.
//
// Authority: `docs/goal/architecture/account-sync-plane.md` § The peer leg →
// *Authorization* ("revoking a device … severs its peer-plane admission at the
// next handshake") and *Validity and severance* ("Revocation severs at the
// next admission evaluation"); Wormability walk rule 6 ("Admission severance
// is the seam's next-evaluation bound — that half stands"). The removal record
// itself is `docs/goal/behavior/devices.md` § Removing a Device.

/// A raw `fauna.sync.changes.list` over an open channel, expected to be
/// refused — the wire form, because a plane walk wraps the wire error's own
/// kind away and the refusing door is exactly what these pins assert.
async fn refused_changes_list(channel: &PeerChannel) -> String {
    let req: Value = decode_strict(
        &encode_canonical(&SyncChangesListRequest {
            since: 0,
            item_class: Some("state-entry".into()),
            scope: Some(ACCOUNT_STATE_SCOPE.into()),
            frontier: Some(std::collections::BTreeMap::new()),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    match channel.request("fauna.sync.changes.list", req).await {
        Ok(_) => panic!("the peer feed must refuse this request"),
        Err(e) => e.to_string(),
    }
}

/// The fleet id whose `Removed` row the pins below write.
fn removed_row(removed_by: u8) -> Vec<u8> {
    fauna_core::encoding::canonical_encode(&fauna_core::generation::DeviceSetRecord::Removed {
        removed_at_ms: 9_000,
        removed_by: writer_id(removed_by).0,
    })
    .unwrap()
    .to_vec()
}

fn device_set_item(device: u8) -> ItemId {
    machinery_item(
        fauna_protocol::merge_policy::KIND_DEVICE_SET,
        fauna_core::hex32::encode(&writer_id(device).0),
    )
}

/// **A device the fleet removed is refused at the next handshake — in both
/// directions**, and therefore holds none of the rows written after it.
///
/// Both halves matter and neither implies the other: the serve side refuses
/// the removed device's witness (so B never walks A), and the dial side
/// refuses the removed device's *reply* witness (so A never walks B either —
/// "sides admit independently" cuts both ways, and a removed device serving a
/// stale relay plane is not a peer this fleet walks).
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_device_is_refused_at_the_next_handshake_in_both_directions() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;
    let fleet = fleet_scope();

    // 1. A two-device fleet, as the plane holds it.
    a.put_local_scope(
        fleet,
        &device_set_item(DEVICE_A),
        enrollment_value(DEVICE_A),
        None,
    )
    .await;
    a.put_local_scope(
        fleet,
        &device_set_item(DEVICE_B),
        enrollment_value(DEVICE_B),
        None,
    )
    .await;
    assert_eq!(
        a.refresh_removed_devices().await,
        0,
        "an enrolled fleet excludes nobody — the check must be about the Removed row, \
         never about mere presence"
    );

    // 2. A removes B — the plane-side leg of the devices-page gesture.
    a.put_local_scope(
        fleet,
        &device_set_item(DEVICE_B),
        removed_row(DEVICE_A),
        None,
    )
    .await;
    assert_eq!(
        a.refresh_removed_devices().await,
        1,
        "A's merged device-set state now excludes exactly one id"
    );

    // 3. A writes a row only a still-admitted peer may ever see.
    a.put_local(
        &moderation_item(),
        moderation_with("written-after-b-was-removed"),
        stamp(2_000, DEVICE_A),
    )
    .await;

    // 4. Serve side: B dials A and is refused. The probe got
    //    `AdmissionVerdict { scopes: AllOfAccount, expires_at: None }` here.
    let refusal = match b.try_admitted_channel(&a, &now).await {
        Ok(_) => panic!("a removed device must not be admitted"),
        Err(e) => e,
    };
    assert!(
        format!("{refusal:#}").contains("witness_refused"),
        "the refusal is the admission door's, by name: {refusal:#}"
    );

    // 5. …and an unadmitted connection serves it nothing, so the row cannot
    //    arrive by any other door on the same channel.
    let conn = b
        .transport
        .dial(
            EndpointKey::from_bytes(writer_id(DEVICE_A).0),
            PathCandidates::default(),
        )
        .await
        .expect("dial");
    let channel = Arc::new(PeerChannel::open(conn).await.expect("channel"));
    let refused = refused_changes_list(&channel).await;
    assert!(
        refused.contains("fauna.peer.sync.not_admitted"),
        "the pull is closed at the verdict preflight: {refused}"
    );
    assert!(
        b.try_walk_peer_scope(&channel, ACCOUNT_STATE_SCOPE)
            .await
            .is_err(),
        "and the plane walk that rides it fails with it"
    );
    assert!(
        b.store
            .state(KIND_MODERATION, MODERATION_KEY)
            .await
            .unwrap()
            .is_none(),
        "B must hold none of A's post-removal rows"
    );

    // 6. Dial side: A dials B, whose reply witness is B's own fleet cert —
    //    A's view excludes it, so A never walks a removed device's plane.
    let mirror = match a.try_admitted_channel(&b, &now).await {
        Ok(_) => panic!("a removed device's reply witness must not be accepted either"),
        Err(e) => e,
    };
    assert!(
        format!("{mirror:#}").contains("removed from this account's fleet"),
        "the dialer refuses on its own view, not on the peer's: {mirror:#}"
    );
}

/// **Severance is per REQUEST, not per connection.** A fleet cert never
/// expires, so a connection admitted before the removal would otherwise keep
/// pulling for as long as the peer held it open — the removal would bound only
/// the *next* handshake, which no one controls. The serve side re-checks the
/// view on every request, exactly as it re-checks served-set membership.
#[tokio::test(flavor = "multi_thread")]
async fn removal_severs_a_live_connection_at_its_next_request() {
    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;
    let fleet = fleet_scope();

    a.put_local_scope(
        fleet,
        &device_set_item(DEVICE_A),
        enrollment_value(DEVICE_A),
        None,
    )
    .await;
    a.put_local_scope(
        fleet,
        &device_set_item(DEVICE_B),
        enrollment_value(DEVICE_B),
        None,
    )
    .await;
    a.put_local(
        &moderation_item(),
        moderation_with("before-removal"),
        stamp(1_000, DEVICE_A),
    )
    .await;

    // 1. B is admitted while still enrolled, and walks.
    let channel = b.admitted_channel(&a, &now).await;
    b.try_walk_peer_scope(&channel, ACCOUNT_STATE_SCOPE)
        .await
        .expect("the walk of an admitted sibling");
    assert_eq!(
        stored(&b.store, KIND_MODERATION, MODERATION_KEY).await,
        moderation_with("before-removal")
    );

    // 2. A learns of the removal — the pump's snapshot refresh — and writes.
    a.put_local_scope(
        fleet,
        &device_set_item(DEVICE_B),
        removed_row(DEVICE_A),
        None,
    )
    .await;
    assert_eq!(a.refresh_removed_devices().await, 1);
    a.put_local(
        &moderation_item(),
        moderation_with("after-removal"),
        stamp(2_000, DEVICE_A),
    )
    .await;

    // 3. The SAME channel, already holding a verdict, is severed at its next
    //    request — no re-handshake anywhere in between.
    let severed = refused_changes_list(&channel).await;
    assert!(
        severed.contains("fauna.peer.sync.not_admitted"),
        "severance is the verdict preflight's refusal: {severed}"
    );
    assert!(
        b.try_walk_peer_scope(&channel, ACCOUNT_STATE_SCOPE)
            .await
            .is_err(),
        "so the walk riding that connection stops too"
    );
    assert_eq!(
        stored(&b.store, KIND_MODERATION, MODERATION_KEY).await,
        moderation_with("before-removal"),
        "nothing written after the removal may reach B"
    );
}

// ── The private contact overlay (`contacts.md` § The private overlay) ───────

/// A Save on `replica` through the production door
/// (`contact_overlay_rows::write_contact_overlay`) over its fleet plane, then
/// the runtime's publish step (`publish_pending` — on the peer leg it records
/// the relay row a sibling is served).
async fn save_overlay(
    replica: &Replica,
    actor: &str,
    write: fauna_sync_engine::contact_overlay_rows::OverlayWrite,
    now_ms: i64,
) -> fauna_sync_engine::contact_overlay_rows::OverlayWriteOutcome {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let outcome = fauna_sync_engine::contact_overlay_rows::write_contact_overlay(
        &replica.store,
        &plane,
        writer_id(replica.device).0,
        actor,
        &write,
        now_ms,
    )
    .await
    .expect("overlay save");
    plane.publish_pending().await.expect("publish step");
    outcome
}

/// The changes a form edit produces — through the shared diff, as the
/// private section's Save does.
fn edit(
    baseline: &fauna_core::contact_overlay::OverlayForm,
    staged: fauna_core::contact_overlay::OverlayForm,
) -> fauna_sync_engine::contact_overlay_rows::OverlayWrite {
    fauna_sync_engine::contact_overlay_rows::OverlayWrite::Changes(
        fauna_core::contact_overlay::changed_registers(baseline, &staged).expect("valid form"),
    )
}

/// Two devices of one account over one fleet, one generation minted over
/// both and acked by the escrow holder — so both overlay writer doors resolve
/// a tip — joined by admitted channels both ways.
struct TippedPair {
    _listeners: Listeners,
    _now: Arc<AtomicU64>,
    a: Replica,
    b: Replica,
    ab: Arc<PeerChannel>,
    ba: Arc<PeerChannel>,
}

async fn tipped_pair() -> TippedPair {
    use fauna_core::generation::{
        EscrowTargetRecord, escrow_target_identity_key, sign_escrow_receipt,
    };
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;
    use fauna_protocol::merge_policy::{
        KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT,
    };

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let b = replica(DEVICE_B, &listeners, &now, None, QuotaConfig::default()).await;
    let fleet = fleet_scope();

    // One fleet, one generation over both devices, acked by the escrow
    // holder — so both writer doors resolve a tip.
    for r in [&a, &b] {
        r.put_local_scope(
            fleet,
            &machinery_item(
                KIND_DEVICE_SET,
                fauna_core::hex32::encode(&writer_id(r.device).0),
            ),
            enrollment_value(r.device),
            None,
        )
        .await;
    }
    let ab = a.admitted_channel(&b, &now).await;
    let ba = b.admitted_channel(&a, &now).await;
    a.walk_peer_scope(&ab, fleet).await;
    let escrow = EscrowTargetRecord {
        xwing_escrow_pubkey: fauna_core::generation::derive_escrow_xwing_keypair(&ESCROW_SEED)
            .public
            .to_bytes()
            .to_vec(),
    };
    let built = build_mint(
        &[fleet_member(DEVICE_A), fleet_member(DEVICE_B)],
        &escrow,
        &escrow_target_identity_key(&root().actor_id()),
        Vec::new(),
        &signing_key(DEVICE_A),
        7_000,
    )
    .expect("mint");
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
        ),
        fauna_core::encoding::canonical_encode(&built.record).unwrap(),
        None,
    )
    .await;
    let receipt = sign_escrow_receipt(
        &holder_key(),
        built.generation_id,
        blake3::hash(&built.escrow_wrap).into(),
        &escrow_target_identity_key(&root().actor_id()),
        7_100,
    );
    a.put_local_scope(
        fleet,
        &machinery_item(
            KIND_ESCROW_RECEIPT,
            format!(
                "{}/{}",
                fauna_core::hex32::encode(&built.generation_id),
                fauna_core::hex32::encode(&receipt.holder_id)
            ),
        ),
        fauna_core::encoding::canonical_encode(&receipt).unwrap(),
        None,
    )
    .await;
    b.walk_peer_scope(&ba, fleet).await;

    TippedPair {
        _listeners: listeners,
        _now: now,
        a,
        b,
        ab,
        ba,
    }
}

/// **The overlay's convergence proof (step 3): two devices of one
/// account, a nickname set on one and notes on the other — both survive; a
/// label removed on one is not resurrected by the other's stale copy.** Real
/// `GenerationTip` seals through the production door on both replicas (one
/// generation minted over both), real peer walks both ways, the kind's own
/// `CrdtPerField` arm merging.
#[tokio::test(flavor = "multi_thread")]
async fn a_contact_overlay_converges_per_field_and_a_removed_label_stays_removed() {
    use fauna_core::contact_overlay::OverlayForm;
    use fauna_protocol::merge_policy::KIND_CONTACT_OVERLAY;
    use fauna_sync_engine::contact_overlay_rows::{OverlayWriteOutcome, read_contact_overlay};

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    let person = "cd".repeat(32);
    let empty = OverlayForm::default();

    // A: a nickname and two labels. B learns them.
    let a_first = OverlayForm {
        nickname: "Mum".into(),
        labels: vec!["Family".into(), "Book club".into()],
        ..Default::default()
    };
    assert!(matches!(
        save_overlay(&a, &person, edit(&empty, a_first.clone()), 10_000).await,
        OverlayWriteOutcome::Written(_)
    ));
    b.walk_peer_scope(&ba, fleet).await;
    let b_view = read_contact_overlay(&b.store, &person).await.unwrap();
    assert_eq!(
        b_view.nickname(),
        Some("Mum"),
        "B opened A's tip-sealed row"
    );

    // Concurrently, offline from each other: B writes notes (from the form
    // it opened), A removes "Book club".
    let b_form = b_view.form();
    let b_staged = OverlayForm {
        notes: "likes tea".into(),
        ..b_form.clone()
    };
    save_overlay(&b, &person, edit(&b_form, b_staged), 20_000).await;
    let a_staged = OverlayForm {
        labels: vec!["Family".into()],
        ..a_first.clone()
    };
    save_overlay(&a, &person, edit(&a_first, a_staged), 21_000).await;

    // Converge both ways.
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let on_a = read_contact_overlay(&a.store, &person).await.unwrap();
    let on_b = read_contact_overlay(&b.store, &person).await.unwrap();
    for (side, view) in [("A", &on_a), ("B", &on_b)] {
        assert_eq!(
            view.nickname(),
            Some("Mum"),
            "{side}: A's nickname survives"
        );
        assert_eq!(view.notes(), Some("likes tea"), "{side}: B's notes survive");
        assert_eq!(
            view.live_labels(),
            vec!["Family"],
            "{side}: the removed label is not resurrected by B's stale copy"
        );
    }
    assert_eq!(
        stored(&a.store, KIND_CONTACT_OVERLAY, &person).await,
        stored(&b.store, KIND_CONTACT_OVERLAY, &person).await,
        "both replicas hold identical bytes"
    );
}

/// Fold `pred`'s overlay onto `succ` on `replica` through the production door
/// (`contact_overlay_rows::fold_contact_overlay`), then the publish step.
async fn fold_overlay(replica: &Replica, pred: &str, succ: &str) -> bool {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let wrote = fauna_sync_engine::contact_overlay_rows::fold_contact_overlay(
        &replica.store,
        &plane,
        pred,
        succ,
    )
    .await
    .expect("overlay fold");
    plane.publish_pending().await.expect("publish step");
    wrote
}

/// **The succession fold at the store (`contacts.md` § The private overlay →
/// *When a person's identity succeeds*):** A folds a person's overlay onto
/// their verified successor through the production door; B, which never
/// folded, converges on A's result by the walk alone — the successor carries
/// the nickname and the label, the predecessor keeps none of it. A straggler
/// edit B made to the OLD identity meanwhile survives the fold's clear, and
/// B's own reconcile fold carries it without undoing the first.
#[tokio::test(flavor = "multi_thread")]
async fn a_succession_fold_moves_the_overlay_and_a_straggler_edit_folds_after_it() {
    use fauna_core::contact_overlay::OverlayForm;
    use fauna_sync_engine::contact_overlay_rows::read_contact_overlay;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let old = "cd".repeat(32);
    let new = "ef".repeat(32);

    let named = OverlayForm {
        nickname: "Mum".into(),
        labels: vec!["Family".into()],
        ..Default::default()
    };
    save_overlay(&a, &old, edit(&OverlayForm::default(), named), 10_000).await;
    b.walk_peer_scope(&ba, fleet).await;

    // A consumes the verified statement and folds.
    assert!(fold_overlay(&a, &old, &new).await);
    assert!(
        !fold_overlay(&a, &old, &new).await,
        "a second fold of the same pair writes nothing"
    );

    // B, offline from A meanwhile, edits the OLD identity's notes.
    let b_form = read_contact_overlay(&b.store, &old).await.unwrap().form();
    let b_staged = OverlayForm {
        notes: "moved house".into(),
        ..b_form.clone()
    };
    save_overlay(&b, &old, edit(&b_form, b_staged), 30_000).await;

    b.walk_peer_scope(&ba, fleet).await;
    let on_b_new = read_contact_overlay(&b.store, &new).await.unwrap();
    assert_eq!(on_b_new.nickname(), Some("Mum"), "B converges on A's fold");
    assert_eq!(on_b_new.live_labels(), vec!["Family"]);
    let on_b_old = read_contact_overlay(&b.store, &old).await.unwrap();
    assert_eq!(on_b_old.nickname(), None, "what was carried stays carried");
    assert_eq!(
        on_b_old.notes(),
        Some("moved house"),
        "the straggler's later edit survives the fold's clear"
    );

    // B's reconcile folds the straggler edit; A converges on it.
    assert!(fold_overlay(&b, &old, &new).await);
    a.walk_peer_scope(&ab, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let succ = read_contact_overlay(store, &new).await.unwrap();
        assert_eq!(succ.nickname(), Some("Mum"), "{side}");
        assert_eq!(succ.notes(), Some("moved house"), "{side}");
        assert!(
            read_contact_overlay(store, &old).await.unwrap().is_empty(),
            "{side}: the predecessor says nothing"
        );
    }
}

// ── The group-share ceremony record (`p2p.md` § Offline share initiation) ───

/// Flush `record` on `replica` through the production door
/// (`group_share_rows::merge_group_shares`) over its fleet plane, then the
/// runtime's publish step.
async fn flush_group_shares(
    replica: &Replica,
    record: &fauna_core::group_ceremony::GroupShareConfig,
) -> anyhow::Result<fauna_core::group_ceremony::GroupShareConfig> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let (joined, _moved) =
        fauna_sync_engine::group_share_rows::merge_group_shares(&replica.store, &plane, record)
            .await?;
    plane.publish_pending().await.expect("publish step");
    Ok(joined)
}

/// **The ceremony records' convergence proof (the E3 slice's conformance
/// step): two devices of one account, each recording ceremony progress the
/// other has not seen, over several ceremonies — one row each
/// (`config-dissolution.md` → *Bounded rows*) — every record and every
/// monotone marker survives on both, the fold identical and every row in
/// identical bytes.** Real `GenerationTip` seals through the
/// production door on both replicas (generation 1 minted over both), real
/// peer walks both ways, the kind's own `CrdtPerField` arm merging.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_share_ceremony_record_converges_through_the_production_door() {
    use fauna_core::group_ceremony::{GroupShareConfig, InitiatedGroupShare, InvitedGroupShare};
    use fauna_protocol::merge_policy::KIND_GROUP_SHARE_CEREMONY;
    use fauna_sync_engine::group_share_rows::read_group_shares;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let shared = [0x51; 32];
    let recipient = ActorKeypair::from_secret([0x31; 32]).actor_id();

    // A begins two shares: the offers are posted, the roots held — two
    // ceremonies, two rows. B learns them.
    let begun = GroupShareConfig {
        initiated: vec![
            InitiatedGroupShare {
                scope_id: shared,
                recipient,
                root: vec![0x11; 32].into(),
                offer: vec![1, 2, 3],
                offer_posted: true,
                offered_at: Timestamp(100),
                updated_at: Timestamp(100),
                ..Default::default()
            },
            InitiatedGroupShare {
                scope_id: [0x53; 32],
                recipient,
                root: vec![0x12; 32].into(),
                offer: vec![5, 6],
                offer_posted: true,
                offered_at: Timestamp(110),
                updated_at: Timestamp(110),
                ..Default::default()
            },
        ],
        invited: Vec::new(),
    };
    flush_group_shares(&a, &begun)
        .await
        .expect("A's door writes");
    b.walk_peer_scope(&ba, fleet).await;
    let on_b = read_group_shares(&b.store).await.unwrap();
    assert_eq!(on_b, begun, "B opened A's tip-sealed rows");

    // Concurrently, unaware of each other: B records the deliver it built
    // and an invitation of its own; A, from its stale replica, marks the
    // plane rows written.
    let mut b_progress = on_b.clone();
    b_progress.initiated[0].deliver = vec![9];
    b_progress.initiated[0].delivered = true;
    b_progress.initiated[0].updated_at = Timestamp(200);
    b_progress.invited.push(InvitedGroupShare {
        scope_id: [0x52; 32],
        initiator: recipient,
        offer: vec![4],
        updated_at: Timestamp(150),
        ..Default::default()
    });
    flush_group_shares(&b, &b_progress)
        .await
        .expect("B's door writes");
    let mut a_progress = begun.clone();
    a_progress.initiated[0].plane_rows_written = true;
    flush_group_shares(&a, &a_progress)
        .await
        .expect("A's door writes");

    // Converge both ways.
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let view = read_group_shares(store).await.unwrap();
        assert_eq!(view.initiated.len(), 2, "{side}: both ceremonies survive");
        let rec = &view.initiated[0];
        assert!(rec.delivered, "{side}: B's deliver marker survives");
        assert_eq!(
            rec.deliver,
            vec![9],
            "{side}: B's deliver envelope survives"
        );
        assert!(
            rec.plane_rows_written,
            "{side}: A's marker survives B's concurrent write"
        );
        assert_eq!(view.invited.len(), 1, "{side}: B's invitation survives");
    }
    let on_a = read_group_shares(&a.store).await.unwrap();
    assert_eq!(
        on_a,
        read_group_shares(&b.store).await.unwrap(),
        "the fold converges"
    );
    let rows = on_a.rows();
    assert_eq!(rows.len(), 3, "one row per ceremony side-record");
    for (key, _) in &rows {
        assert_eq!(
            stored(&a.store, KIND_GROUP_SHARE_CEREMONY, key).await,
            stored(&b.store, KIND_GROUP_SHARE_CEREMONY, key).await,
            "both replicas hold identical bytes at {key}"
        );
    }
    // The echo-stop at the door: re-flushing what the row already covers
    // puts nothing new.
    let before = a.store.frontier(fleet).await.unwrap();
    flush_group_shares(&a, &begun)
        .await
        .expect("a covered flush");
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a flush the row already covers writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal the record — the refusal lands before any
/// durable local state, so the seat's flush reports it and the replica keeps
/// no row it could never publish.
#[tokio::test(flavor = "multi_thread")]
async fn the_group_share_ceremony_door_refuses_while_no_tip_resolves() {
    use fauna_core::group_ceremony::{GroupShareConfig, InvitedGroupShare};
    use fauna_protocol::merge_policy::KIND_GROUP_SHARE_CEREMONY;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let record = GroupShareConfig {
        initiated: Vec::new(),
        invited: vec![InvitedGroupShare {
            scope_id: [0x52; 32],
            offer: vec![4],
            ..Default::default()
        }],
    };
    let err = flush_group_shares(&a, &record)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_GROUP_SHARE_CEREMONY)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// The source box the backup tests list for, unless a test names another.
const BACKUP_BOX: [u8; 32] = [0xA1; 32];

/// What a backup-state door call writes on `replica`: a new destination list
/// for one source box stamped at `now`, or a batch of marks.
enum BackupWrite<'a> {
    Destinations([u8; 32], &'a [&'a str], u64),
    Marks(Vec<fauna_core::data::DestinationUnattestedMark>),
}

/// Run one `fauna.state.backup` door call on `replica` through the production
/// door (`backup_rows`) over its fleet plane, then the runtime's publish step.
/// Answers whether the door put anything.
async fn flush_backup(replica: &Replica, write: BackupWrite<'_>) -> anyhow::Result<bool> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let moved = match write {
        BackupWrite::Destinations(source_nest, ids, now) => {
            fauna_sync_engine::backup_rows::write_backup_destinations(
                &replica.store,
                &plane,
                &source_nest,
                &backup_list(ids),
                Timestamp(now),
            )
            .await?
            .1
        }
        BackupWrite::Marks(marks) => {
            fauna_sync_engine::backup_rows::merge_destination_marks(&replica.store, &plane, &marks)
                .await?
                .1
        }
    };
    plane.publish_pending().await.expect("publish step");
    Ok(moved)
}

fn backup_list(ids: &[&str]) -> fauna_core::data::BackupConfig {
    fauna_core::data::BackupConfig {
        destinations: ids
            .iter()
            .map(|id| fauna_core::data::BackupDestination {
                destination_id: id.to_string(),
                destination_nest_url: format!("https://{id}.example"),
                folder_name: "__mail".to_string(),
                ..Default::default()
            })
            .collect(),
    }
}

/// **The backup-destination state's convergence proof (the E3 slice's
/// conformance step): two devices of one account, one editing the list from
/// a stale replica while the other removes a carried-across destination —
/// the removal holds against the newer list, every mark survives on both,
/// the fold identical and every row in identical bytes.** Real
/// `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the kind's
/// own `CrdtPerField` arm merging row by row and the read fold pruning.
#[tokio::test(flavor = "multi_thread")]
async fn the_backup_state_converges_through_the_production_door() {
    use fauna_core::data::{DestinationUnattestedMark, UnattestedVerdict};
    use fauna_protocol::merge_policy::KIND_BACKUP;
    use fauna_sync_engine::backup_rows::read_backup;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let predecessor = ActorKeypair::from_secret([0x41; 32]).actor_id();
    let mark = |id: &str, verdict| DestinationUnattestedMark {
        destination_id: id.to_string(),
        predecessor,
        verdict,
    };
    let ids = |state: &fauna_core::backup_state::BackupState| -> Vec<String> {
        state
            .backup
            .destinations
            .iter()
            .map(|d| d.destination_id.clone())
            .collect()
    };

    // A enrolls two destinations; B learns them.
    assert!(
        flush_backup(
            &a,
            BackupWrite::Destinations(BACKUP_BOX, &["d1", "d2"], 100)
        )
        .await
        .expect("A's door writes")
    );
    b.walk_peer_scope(&ba, fleet).await;
    assert_eq!(
        ids(&read_backup(&b.store, &BACKUP_BOX).await.unwrap()),
        ["d1", "d2"],
        "B opened A's tip-sealed row"
    );

    // Concurrently, unaware of each other: B's aftermath raises marks on
    // both carried-across destinations and the owner removes d2 and keeps
    // d1; A, from its stale replica, enrolls a third destination — a NEWER
    // list that still carries d2.
    flush_backup(
        &b,
        BackupWrite::Marks(vec![
            mark("d1", UnattestedVerdict::Open),
            mark("d2", UnattestedVerdict::Open),
        ]),
    )
    .await
    .expect("B raises");
    flush_backup(
        &b,
        BackupWrite::Marks(vec![
            mark("d1", UnattestedVerdict::Kept),
            mark("d2", UnattestedVerdict::Removed),
        ]),
    )
    .await
    .expect("B adjudicates");
    flush_backup(
        &a,
        BackupWrite::Destinations(BACKUP_BOX, &["d1", "d2", "d3"], 300),
    )
    .await
    .expect("A's door writes");

    // Converge both ways.
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let on_a = read_backup(&a.store, &BACKUP_BOX).await.unwrap();
    assert_eq!(
        on_a,
        read_backup(&b.store, &BACKUP_BOX).await.unwrap(),
        "the fold converges"
    );
    assert_eq!(
        ids(&on_a),
        ["d1", "d3"],
        "A's newer list wins, and the removed destination stays removed"
    );
    assert_eq!(
        on_a.marks,
        vec![
            mark("d1", UnattestedVerdict::Kept),
            mark("d2", UnattestedVerdict::Removed)
        ],
        "both verdicts survive"
    );
    let mut keys = vec![fauna_core::backup_state::destinations_key(&BACKUP_BOX)];
    keys.extend(on_a.marks.iter().map(DestinationUnattestedMark::plane_key));
    for key in &keys {
        // `stored` panics on a missing entry, so this also proves every row
        // rests on both replicas.
        assert_eq!(
            stored(&a.store, KIND_BACKUP, key).await,
            stored(&b.store, KIND_BACKUP, key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // The echo-stop at the door, and the bounds: re-writing the list the row
    // already holds, re-raising a decided mark as open, and a list over the
    // row's ceiling each put nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(
        !flush_backup(
            &a,
            BackupWrite::Destinations(BACKUP_BOX, &["d1", "d2", "d3"], 400)
        )
        .await
        .expect("a covered write")
    );
    assert!(
        !flush_backup(
            &a,
            BackupWrite::Marks(vec![mark("d2", UnattestedVerdict::Open)])
        )
        .await
        .expect("a covered raise")
    );
    // Real-sized entries, far more than any box lists, until the row's byte
    // ceiling binds.
    let too_many: Vec<String> = (0..1_000).map(|i| format!("d{i}")).collect();
    let too_many: Vec<&str> = too_many.iter().map(String::as_str).collect();
    let err = flush_backup(&a, BackupWrite::Destinations(BACKUP_BOX, &too_many, 500))
        .await
        .expect_err("over the ceiling");
    assert!(
        format!("{err:#}").contains("ceiling"),
        "expected the bounds refusal, got: {err:#}"
    );
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a covered or refused write puts nothing"
    );
}

/// **Two boxes' lists through the production door never read into each
/// other (`backup-destinations.md` § State & data shape → *Destination data
/// model*), while a `Removed` mark prunes whichever box lists the
/// destination.** Device A keeps box X's list, device B box Y's; after both
/// walks each replica holds both rows, each box's fold reads only its own
/// list, a box with no row reads empty, and the all-boxes read sees both.
#[tokio::test(flavor = "multi_thread")]
async fn two_boxes_backup_lists_stay_apart_through_the_production_door() {
    use fauna_core::data::{DestinationUnattestedMark, UnattestedVerdict};
    use fauna_sync_engine::backup_rows::{destination_lists_of, read_backup};

    const BOX_X: [u8; 32] = [0x0A; 32];
    const BOX_Y: [u8; 32] = [0x0B; 32];
    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let ids = |state: &fauna_core::backup_state::BackupState| -> Vec<String> {
        state
            .backup
            .destinations
            .iter()
            .map(|d| d.destination_id.clone())
            .collect()
    };

    flush_backup(&a, BackupWrite::Destinations(BOX_X, &["x1", "x2"], 100))
        .await
        .expect("A writes box X's list");
    flush_backup(&b, BackupWrite::Destinations(BOX_Y, &["y1"], 900))
        .await
        .expect("B writes box Y's list");
    flush_backup(
        &b,
        BackupWrite::Marks(vec![DestinationUnattestedMark {
            destination_id: "x2".into(),
            predecessor: ActorKeypair::from_secret([0x41; 32]).actor_id(),
            verdict: UnattestedVerdict::Removed,
        }]),
    )
    .await
    .expect("B removes a destination box X lists");
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for replica in [&a, &b] {
        assert_eq!(
            ids(&read_backup(&replica.store, &BOX_X).await.unwrap()),
            ["x1"],
            "box X reads its own list, the removed destination pruned"
        );
        assert_eq!(
            ids(&read_backup(&replica.store, &BOX_Y).await.unwrap()),
            ["y1"],
            "box Y reads its own list and never X's newer-stamped one"
        );
        assert!(
            read_backup(&replica.store, &[0x0C; 32])
                .await
                .unwrap()
                .backup
                .destinations
                .is_empty(),
            "a box with no row reads empty, never another box's"
        );
        let entries = replica
            .store
            .states_of_kind(fauna_protocol::merge_policy::KIND_BACKUP)
            .await
            .unwrap();
        let mut boxes: Vec<[u8; 32]> = destination_lists_of(&entries)
            .unwrap()
            .into_iter()
            .map(|row| row.source_nest)
            .collect();
        boxes.sort_unstable();
        assert_eq!(boxes, [BOX_X, BOX_Y], "the all-boxes read sees both");
    }
}

/// **The backup-state door refuses while no generation tip resolves, and
/// keeps nothing** — both of its writes.
#[tokio::test(flavor = "multi_thread")]
async fn the_backup_state_door_refuses_while_no_tip_resolves() {
    use fauna_core::data::{DestinationUnattestedMark, UnattestedVerdict};
    use fauna_protocol::merge_policy::KIND_BACKUP;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    for write in [
        BackupWrite::Destinations(BACKUP_BOX, &["d1"], 100),
        BackupWrite::Marks(vec![DestinationUnattestedMark {
            destination_id: "d1".into(),
            predecessor: ActorKeypair::from_secret([0x41; 32]).actor_id(),
            verdict: UnattestedVerdict::Open,
        }]),
    ] {
        let err = flush_backup(&a, write)
            .await
            .expect_err("no tip resolves, so the door must refuse");
        assert!(
            format!("{err:#}").contains("no candidate generation tip resolves"),
            "expected the no-tip refusal, got: {err:#}"
        );
    }
    assert!(
        a.store
            .states_of_kind(KIND_BACKUP)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

// ── The DNS management record (`dns-management.md`; `fauna.state.dns`) ─────

/// Write `next` on `replica` through the production door
/// (`dns_rows::write_dns`) over its fleet plane, then the runtime's publish
/// step. Whether the door wrote.
async fn put_dns(replica: &Replica, next: &fauna_core::data::DnsConfig) -> anyhow::Result<bool> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let wrote = fauna_sync_engine::dns_rows::write_dns(
        &replica.store,
        &plane,
        writer_id(replica.device).0,
        next,
    )
    .await?;
    plane.publish_pending().await.expect("publish step");
    Ok(wrote)
}

fn dns_with(domain: &str, token: &str) -> fauna_core::data::DnsConfig {
    let mut cfg = fauna_core::data::DnsConfig {
        credentials: vec![fauna_core::data::DnsProviderCredential {
            provider_id: "hetzner".into(),
            fields: vec![("api-token".into(), token.to_string().into())],
            zones: vec![fauna_core::data::DnsZoneRef {
                id: "z1".into(),
                name: domain.into(),
            }],
            label: format!("Hetzner ({domain})"),
            created_at: 1,
        }],
        acme_account: Some(vec![0xac; 64]),
        ..Default::default()
    };
    cfg.managed_domains.insert(domain.into());
    cfg
}

/// **The DNS record's convergence proof (the E3 slice's conformance step):
/// two devices of one account, a credential entered on one and a later
/// whole-record replacement on the other — both replicas converge on the
/// later write, in identical bytes.** Real `GenerationTip` seals through the
/// production door on both replicas (generation 1 minted over both), real
/// peer walks both ways, the generic `LatestWins` path ordering by the
/// entry's `LwwStamp` (the shipped `theirs_wins` rule, stamp and all).
#[tokio::test(flavor = "multi_thread")]
async fn a_dns_record_converges_latest_wins_through_the_production_door() {
    use fauna_protocol::merge_policy::{DNS_ROW_KEY, KIND_DNS};
    use fauna_sync_engine::dns_rows::read_dns;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    // A enters a credential. B learns it.
    let first = dns_with("example.com", "token-a");
    assert!(put_dns(&a, &first).await.expect("A's door writes"));
    b.walk_peer_scope(&ba, fleet).await;
    assert_eq!(
        read_dns(&b.store).await.unwrap(),
        first,
        "B opened A's tip-sealed row"
    );

    // Then B, later, replaces the record (a rotated token, another domain);
    // A has not yet heard. The later stamp must win on both.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let mut later = dns_with("example.com", "token-b");
    later.managed_domains.insert("example.org".into());
    assert!(put_dns(&b, &later).await.expect("B's door writes"));

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_dns(store).await.unwrap(),
            later,
            "{side}: the later whole-record write wins"
        );
    }
    assert_eq!(
        stored(&a.store, KIND_DNS, DNS_ROW_KEY).await,
        stored(&b.store, KIND_DNS, DNS_ROW_KEY).await,
        "both replicas hold identical bytes"
    );

    // The echo-stop at the door: re-writing the record as it stands puts
    // nothing new.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(!put_dns(&a, &later).await.expect("an unchanged write"));
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a write of the record as it stands writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal the provider credentials — the refusal lands
/// before any durable local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_dns_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_DNS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = put_dns(&a, &dns_with("example.com", "token-a"))
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store.states_of_kind(KIND_DNS).await.unwrap().is_empty(),
        "nothing durable is left behind"
    );
}

// ── The ATProto app credentials (`atproto-pds-full.md`; `fauna.state.atproto`) ──

/// Which app-credential door a test drives.
enum AtprotoWrite<'a> {
    Mint(&'a fauna_core::data::AtprotoAppCredential),
    Revoke(&'a str),
}

/// Drive one app-credential door (`atproto_rows::{put,revoke}_app_credential`)
/// on `replica` over its fleet plane, then the runtime's publish step.
/// Whether the door wrote.
async fn atproto_door(replica: &Replica, write: AtprotoWrite<'_>) -> anyhow::Result<bool> {
    use fauna_sync_engine::atproto_rows::{put_app_credential, revoke_app_credential};
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let me = writer_id(replica.device).0;
    let wrote = match write {
        AtprotoWrite::Mint(credential) => {
            put_app_credential(&replica.store, &plane, me, credential).await?
        }
        AtprotoWrite::Revoke(id) => revoke_app_credential(&replica.store, &plane, me, id).await?,
    };
    plane.publish_pending().await.expect("publish step");
    Ok(wrote)
}

async fn mint(
    replica: &Replica,
    credential: &fauna_core::data::AtprotoAppCredential,
) -> anyhow::Result<bool> {
    atproto_door(replica, AtprotoWrite::Mint(credential)).await
}

async fn revoke(replica: &Replica, credential_id: &str) -> anyhow::Result<bool> {
    atproto_door(replica, AtprotoWrite::Revoke(credential_id)).await
}

fn app_credential(
    id: &str,
    secret: &str,
    created_at: u64,
) -> fauna_core::data::AtprotoAppCredential {
    fauna_core::data::AtprotoAppCredential {
        credential_id: id.into(),
        label: id.to_uppercase(),
        secret: fauna_core::secret::SecretByteBuf::new(secret.as_bytes().to_vec()),
        dm_allowed: false,
        created_at,
    }
}

/// **The app credentials' convergence proof (the E3 slice's conformance step):
/// two devices of one account mint concurrently — both credentials survive on
/// both replicas (one row per credential, never one record whose later write
/// drops the other's secret) — and a revoke on one device reaches the other.**
/// Real `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the generic
/// `LatestWins` path ordering each row by the entry's `LwwStamp`.
#[tokio::test(flavor = "multi_thread")]
async fn app_credentials_converge_per_credential_through_the_production_door() {
    use fauna_protocol::merge_policy::KIND_ATPROTO;
    use fauna_sync_engine::atproto_rows::read_atproto;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    // A mints Ivory and B mints Graysky, neither having heard of the other.
    let ivory = app_credential("ivory", "aaaa-bbbb-cccc-dddd", 1);
    let graysky = app_credential("graysky", "eeee-ffff-gggg-hhhh", 2);
    assert!(mint(&a, &ivory).await.expect("A's door writes"));
    assert!(mint(&b, &graysky).await.expect("B's door writes"));

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_atproto(store).await.unwrap().app_credentials,
            vec![ivory.clone(), graysky.clone()],
            "{side}: both concurrent mints survive, oldest first"
        );
    }
    for id in ["ivory", "graysky"] {
        assert_eq!(
            stored(&a.store, KIND_ATPROTO, id).await,
            stored(&b.store, KIND_ATPROTO, id).await,
            "both replicas hold identical bytes for {id}"
        );
    }

    // The echo-stop at the door: re-minting the row as it stands writes
    // nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(!mint(&a, &ivory).await.expect("an unchanged put"));
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a put of the row as it stands writes nothing"
    );

    // B, later, revokes Ivory — the credential A minted. The stamped
    // tombstone reaches A.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert!(revoke(&b, "ivory").await.expect("B's revoke writes"));
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_atproto(store).await.unwrap().app_credentials,
            vec![graysky.clone()],
            "{side}: the revoke propagated"
        );
        assert!(
            fauna_sync_engine::atproto_rows::read_app_credential(store, "ivory")
                .await
                .unwrap()
                .is_none(),
            "{side}: the revoked secret is no longer readable"
        );
    }
    assert!(
        !revoke(&a, "ivory").await.expect("a revoke of nothing"),
        "revoking a row already tombstoned writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal an app-credential secret — the refusal lands
/// before any durable local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_app_credential_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_ATPROTO;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = mint(&a, &app_credential("ivory", "aaaa-bbbb-cccc-dddd", 1))
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_ATPROTO)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

// ── The followed public folders (`folders.md` § Publicly-synced follow; `fauna.state.follows`) ──

/// Which follow door a test drives.
enum FollowWrite<'a> {
    Follow(&'a fauna_core::data::FollowedFolder),
    Unfollow(&'a str, i64),
}

/// Drive one follow door (`follows_rows::{put_follow, unfollow}`) on `replica`
/// over its fleet plane, then the runtime's publish step. Whether the door
/// wrote.
async fn follows_door(replica: &Replica, write: FollowWrite<'_>) -> anyhow::Result<bool> {
    use fauna_sync_engine::follows_rows::{put_follow, unfollow};
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let me = writer_id(replica.device).0;
    let wrote = match write {
        FollowWrite::Follow(follow) => put_follow(&replica.store, &plane, me, follow).await?,
        FollowWrite::Unfollow(home, id) => unfollow(&replica.store, &plane, me, home, id).await?,
    };
    plane.publish_pending().await.expect("publish step");
    Ok(wrote)
}

fn followed_folder(home: &str, folder_id: i64, name: &str) -> fauna_core::data::FollowedFolder {
    fauna_core::data::FollowedFolder {
        home_nest_url: home.into(),
        home_nest_actor_id: Some("ab".repeat(32)),
        owner_actor_id: "cd".repeat(32),
        owner_handle: Some("alice".into()),
        folder_id,
        display_name: name.into(),
    }
}

/// **The followed folders' convergence proof (the E3 slice's conformance
/// step): two devices of one account follow different folders concurrently —
/// both follows survive on both replicas (one row per folder, never one record
/// whose later write drops the other's follow, the blob's declared cost) — a
/// refresh of one follow replaces it in place, and an unfollow on one device
/// reaches the other (the property the blob's latest-wins arm existed for).**
/// Real `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the generic
/// `LatestWins` path ordering each row by the entry's `LwwStamp`.
#[tokio::test(flavor = "multi_thread")]
async fn follows_converge_per_folder_through_the_production_door() {
    use fauna_core::data::FollowedFolder;
    use fauna_protocol::merge_policy::KIND_FOLLOWS;
    use fauna_sync_engine::follows_rows::{read_follow, read_follows};

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    // A follows a peer nest's folder and B a same-nest one, neither having
    // heard of the other.
    let site = followed_folder("https://peer.example", 7, "site");
    let local = followed_folder("", 9, "photos");
    assert!(
        follows_door(&a, FollowWrite::Follow(&site))
            .await
            .expect("A's door writes")
    );
    assert!(
        follows_door(&b, FollowWrite::Follow(&local))
            .await
            .expect("B's door writes")
    );

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_follows(store).await.unwrap().followed,
            vec![local.clone(), site.clone()],
            "{side}: both concurrent follows survive, in canonical order"
        );
    }
    for key in [site.plane_key(), local.plane_key()] {
        assert_eq!(
            stored(&a.store, KIND_FOLLOWS, &key).await,
            stored(&b.store, KIND_FOLLOWS, &key).await,
            "both replicas hold identical bytes for {key}"
        );
    }

    // The echo-stop at the door: re-following the row as it stands writes
    // nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(
        !follows_door(&a, FollowWrite::Follow(&site))
            .await
            .expect("an unchanged put")
    );
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a put of the row as it stands writes nothing"
    );

    // B, later, refreshes the peer follow (the folder was renamed at home):
    // the same row, replaced in place on both replicas.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let renamed = FollowedFolder {
        display_name: "site, renamed".into(),
        ..site.clone()
    };
    assert!(
        follows_door(&b, FollowWrite::Follow(&renamed))
            .await
            .expect("B's refresh writes")
    );
    a.walk_peer_scope(&ab, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_follow(store, "https://peer.example", 7).await.unwrap(),
            Some(renamed.clone()),
            "{side}: the refresh replaced the follow in place"
        );
    }

    // A, later still, unfollows the folder B followed. The stamped tombstone
    // reaches B — no device that still listed it resurrects it.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert!(
        follows_door(&a, FollowWrite::Unfollow("", 9))
            .await
            .expect("A's unfollow writes")
    );
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_follows(store).await.unwrap().followed,
            vec![renamed.clone()],
            "{side}: the unfollow propagated"
        );
    }
    assert!(
        !follows_door(&b, FollowWrite::Unfollow("", 9))
            .await
            .expect("an unfollow of nothing"),
        "unfollowing a row already tombstoned writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal a follow — the refusal lands before any durable
/// local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_follow_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_FOLLOWS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = follows_door(
        &a,
        FollowWrite::Follow(&followed_folder("https://peer.example", 7, "site")),
    )
    .await
    .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_FOLLOWS)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

// ── The succession ledger (`config-dissolution.md` → *The ledger*) ─────────

/// Which production door a [`ledger_door`] call drives.
enum LedgerWrite<'a> {
    Merge {
        self_actor: fauna_core::identity::ActorId,
        replica: &'a fauna_core::succession_ledger::SuccessionLedger,
        attested: &'a [fauna_core::identity::ActorId],
    },
    Repoint {
        retired: fauna_core::identity::ActorId,
        successor: fauna_core::identity::ActorId,
    },
    RaiseGrantMarks {
        self_actor: fauna_core::identity::ActorId,
        predecessor: fauna_core::identity::ActorId,
    },
}

/// Drive one succession-ledger door (`succession_ledger_rows`) on `replica`
/// over its fleet plane, then the runtime's publish step. Answers whether
/// the door put anything.
async fn ledger_door(replica: &Replica, write: LedgerWrite<'_>) -> anyhow::Result<bool> {
    use fauna_sync_engine::succession_ledger_rows as rows;
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let moved = match write {
        LedgerWrite::Merge {
            self_actor,
            replica: ledger,
            attested,
        } => {
            rows::merge_succession_ledger(&replica.store, &plane, self_actor, ledger, attested)
                .await?
                .1
        }
        LedgerWrite::Repoint { retired, successor } => {
            rows::repoint_succession_ledger(&replica.store, &plane, retired, successor).await?
        }
        LedgerWrite::RaiseGrantMarks {
            self_actor,
            predecessor,
        } => rows::raise_grant_marks(&replica.store, &plane, self_actor, predecessor).await?,
    };
    plane.publish_pending().await.expect("publish step");
    Ok(moved)
}

/// A signed grant event of `kind` for grant `grant` at `at`.
fn ledger_event(
    signer: &ActorKeypair,
    grant: u8,
    kind: fauna_core::grant_event::GrantEventKind,
    at: u64,
) -> fauna_core::grant_event::GrantEvent {
    fauna_core::grant_event::GrantEvent {
        grant_id: vec![grant; 16],
        holder: vec![0xAA; 32],
        kind,
        scope: vec![],
        window_start: at,
        window_end: at + 1_000,
        at,
        sig: vec![0u8; fauna_core::grant_event::GRANT_EVENT_SIGNATURE_LEN],
    }
    .sign(signer.signing_key())
    .unwrap()
}

/// **The ledger's convergence proof (the E3 slice's conformance step): two
/// devices of one account, each writing ledger rows the other has not seen —
/// a mint on A, a revoke of another grant on B, a mark KEPT on A while B
/// still holds it open — converge through the production door: one row per
/// entity, the fold identical on both ends, every row in identical bytes,
/// and the decided mark beats the open one.** Real `GenerationTip` seals
/// (generation 1 over both), real peer walks both ways, the kind's own
/// `CrdtPerField` arm merging. A stranger's event is refused at the door
/// and leaves nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_succession_ledger_converges_through_the_production_door() {
    use fauna_core::data::{GrantUnattestedMark, UnattestedVerdict};
    use fauna_core::grant_event::GrantEventKind::{Mint, Revoke};
    use fauna_core::succession_ledger::SuccessionLedger;
    use fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER;
    use fauna_sync_engine::succession_ledger_rows::read_succession_ledger;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let me = ActorKeypair::from_secret([0x61; 32]);
    let pred = ActorKeypair::from_secret([0x62; 32]).actor_id();
    let id = me.actor_id();

    // A: grant 2 minted, and its carried-across mark raised open. B learns it.
    let mut first = SuccessionLedger::empty(id);
    first.grant_events.push(ledger_event(&me, 2, Mint, 100));
    first.unattested_grant_marks.push(GrantUnattestedMark {
        grant_id: vec![2; 16],
        predecessor: pred,
        verdict: UnattestedVerdict::Open,
    });
    assert!(
        ledger_door(
            &a,
            LedgerWrite::Merge {
                self_actor: id,
                replica: &first,
                attested: &[]
            }
        )
        .await
        .expect("A's door writes")
    );
    b.walk_peer_scope(&ba, fleet).await;
    let on_b = read_succession_ledger(&b.store, id).await.unwrap();
    assert_eq!(on_b, first, "B opened A's tip-sealed rows");

    // Concurrently, unaware of each other: A mints grant 1 and KEEPS the
    // mark; B revokes grant 2 from its replica, where the mark is open.
    let mut a_side = first.clone();
    a_side.grant_events.push(ledger_event(&me, 1, Mint, 200));
    a_side.unattested_grant_marks[0].verdict = UnattestedVerdict::Kept;
    ledger_door(
        &a,
        LedgerWrite::Merge {
            self_actor: id,
            replica: &a_side,
            attested: &[],
        },
    )
    .await
    .expect("A's door writes");
    let mut b_side = on_b.clone();
    b_side.grant_events.push(ledger_event(&me, 2, Revoke, 210));
    ledger_door(
        &b,
        LedgerWrite::Merge {
            self_actor: id,
            replica: &b_side,
            attested: &[],
        },
    )
    .await
    .expect("B's door writes");

    // Converge both ways.
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let on_a = read_succession_ledger(&a.store, id).await.unwrap();
    assert_eq!(
        on_a,
        read_succession_ledger(&b.store, id).await.unwrap(),
        "the fold converges"
    );
    assert_eq!(on_a.grant_events.len(), 3, "both sides' events survive");
    let live: Vec<_> = fauna_core::grant_event::current_grants_of(&on_a.grant_events)
        .into_iter()
        .map(|g| g.grant_id)
        .collect();
    assert_eq!(
        live,
        vec![vec![1; 16]],
        "A's mint lives, B's revoke is terminal"
    );
    assert_eq!(
        on_a.unattested_grant_marks[0].verdict,
        UnattestedVerdict::Kept,
        "the decided verdict beats B's open copy"
    );
    let rows = on_a.rows().unwrap();
    assert_eq!(rows.len(), 5, "chain + three events + one mark");
    for (key, _) in &rows {
        assert_eq!(
            stored(&a.store, KIND_SUCCESSION_LEDGER, key).await,
            stored(&b.store, KIND_SUCCESSION_LEDGER, key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // The echo-stop at the door: a merge the rows already cover puts nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(
        !ledger_door(
            &a,
            LedgerWrite::Merge {
                self_actor: id,
                replica: &first,
                attested: &[]
            }
        )
        .await
        .expect("a covered merge"),
        "a merge the rows already cover writes nothing"
    );
    assert_eq!(a.store.frontier(fleet).await.unwrap(), before);

    // The attested-signer refusal: an event no attested identity signed is
    // refused whole, before any put — the rest of that replica included.
    let stranger = ActorKeypair::from_secret([0x63; 32]);
    let mut forged = on_a.clone();
    forged
        .grant_events
        .push(ledger_event(&stranger, 9, Mint, 300));
    forged.grant_events.push(ledger_event(&me, 8, Mint, 300));
    let err = ledger_door(
        &a,
        LedgerWrite::Merge {
            self_actor: id,
            replica: &forged,
            attested: &[pred],
        },
    )
    .await
    .expect_err("an unattested event must be refused");
    assert!(
        format!("{err:#}").contains("verifies against no attested identity"),
        "expected the attested-signer refusal, got: {err:#}"
    );
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a refused merge leaves nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing** —
/// the kind is `GenerationTip`-sealed, so the aftermath's leg stays owed and
/// retries.
#[tokio::test(flavor = "multi_thread")]
async fn the_succession_ledger_door_refuses_while_no_tip_resolves() {
    use fauna_core::succession_ledger::SuccessionLedger;
    use fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let id = ActorKeypair::from_secret([0x61; 32]).actor_id();
    let err = ledger_door(
        &a,
        LedgerWrite::Merge {
            self_actor: id,
            replica: &SuccessionLedger::empty(id),
            attested: &[],
        },
    )
    .await
    .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    let err = ledger_door(
        &a,
        LedgerWrite::Repoint {
            retired: ActorKeypair::from_secret([0x62; 32]).actor_id(),
            successor: id,
        },
    )
    .await
    .expect_err("the succession write is refused alike");
    assert!(format!("{err:#}").contains("no candidate generation tip resolves"));
    assert!(
        a.store
            .states_of_kind(KIND_SUCCESSION_LEDGER)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// **The succession write through the production door.** Device A still runs
/// the predecessor identity: it writes the `chain` row and one
/// predecessor-signed mint. Device B runs the successor: before its re-point
/// the predecessor's event is stored and invisible to it (the chain it reads
/// is its own alone); after `repoint_succession_ledger` the successor's fold
/// shows the event and the chain re-pointed, the grant-mark raise puts one
/// `Open` mark — on the predecessor's grant only, never on the one the
/// successor minted itself, since the raise re-runs at every store-ready —
/// and both writes are idempotent. A, walking B's rows, reads the successor's
/// chain too — the gate's third arm.
///
/// (One generation serves both identities here: the tip is the fleet's, and
/// a fleet crossing a succession keeps its pre-succession generations — the
/// walk suite's `a_successor_on_its_own_generation_0_schedule_reads_a_pre_succession_tip_sealed_row`
/// owns the cross-generation read.)
#[tokio::test(flavor = "multi_thread")]
async fn a_succession_ledger_repoint_carries_the_predecessor_signed_rows_to_the_successor() {
    use fauna_core::data::UnattestedVerdict;
    use fauna_core::grant_event::GrantEventKind::Mint;
    use fauna_core::succession_ledger::SuccessionLedger;
    use fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER;
    use fauna_sync_engine::succession_ledger_rows::read_succession_ledger;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let old = ActorKeypair::from_secret([0x71; 32]);
    let new = ActorKeypair::from_secret([0x72; 32]);
    let (p, s) = (old.actor_id(), new.actor_id());

    let mut pre = SuccessionLedger::empty(p);
    pre.grant_events.push(ledger_event(&old, 5, Mint, 100));
    ledger_door(
        &a,
        LedgerWrite::Merge {
            self_actor: p,
            replica: &pre,
            attested: &[],
        },
    )
    .await
    .expect("the predecessor's door writes");
    b.walk_peer_scope(&ba, fleet).await;

    let before = read_succession_ledger(&b.store, s).await.unwrap();
    assert_eq!(before.actor_id, s);
    assert!(
        before.grant_events.is_empty(),
        "before the re-point, the predecessor's event is stored and invisible"
    );

    assert!(
        ledger_door(
            &b,
            LedgerWrite::Repoint {
                retired: p,
                successor: s
            }
        )
        .await
        .expect("the successor's re-point")
    );
    assert!(
        !ledger_door(
            &b,
            LedgerWrite::Repoint {
                retired: p,
                successor: s
            }
        )
        .await
        .expect("a second re-point"),
        "a second re-point writes nothing"
    );
    // The successor's own grant, minted after the re-point: signed by `s`, it
    // is no carried-across row, and a raise that re-runs at every store-ready
    // must never ask the owner to adjudicate their own act.
    let mut own = SuccessionLedger::empty(s);
    own.grant_events.push(ledger_event(&new, 6, Mint, 300));
    ledger_door(
        &b,
        LedgerWrite::Merge {
            self_actor: s,
            replica: &own,
            attested: &[p],
        },
    )
    .await
    .expect("the successor's own mint");
    assert!(
        ledger_door(
            &b,
            LedgerWrite::RaiseGrantMarks {
                self_actor: s,
                predecessor: p
            }
        )
        .await
        .expect("the raise")
    );
    assert!(
        !ledger_door(
            &b,
            LedgerWrite::RaiseGrantMarks {
                self_actor: s,
                predecessor: p
            }
        )
        .await
        .expect("a second raise"),
        "the raise is idempotent by the key"
    );

    let after = read_succession_ledger(&b.store, s).await.unwrap();
    assert_eq!(after.actor_id, s);
    assert_eq!(after.prior_actor_ids, vec![p], "the chain is re-pointed");
    assert!(
        pre.grant_events
            .iter()
            .all(|e| after.grant_events.contains(e)),
        "the predecessor's event shows"
    );
    assert_eq!(
        after.unattested_grant_marks.len(),
        1,
        "only the predecessor-signed grant is marked, never the successor's own"
    );
    assert_eq!(after.unattested_grant_marks[0].grant_id, vec![5; 16]);
    assert_eq!(after.unattested_grant_marks[0].predecessor, p);
    assert_eq!(
        after.unattested_grant_marks[0].verdict,
        UnattestedVerdict::Open
    );

    // The predecessor-era device converges on the successor's chain.
    a.walk_peer_scope(&ab, fleet).await;
    let on_a = read_succession_ledger(&a.store, p).await.unwrap();
    assert_eq!(
        on_a.actor_id, s,
        "the successor's id survives on the straggler"
    );
    assert_eq!(on_a, after);
    assert_eq!(
        stored(&a.store, KIND_SUCCESSION_LEDGER, "chain").await,
        stored(&b.store, KIND_SUCCESSION_LEDGER, "chain").await,
        "both replicas hold identical chain bytes"
    );
}

// ── The subscription period keys (`key-material-hierarchy.md`; `fauna.state.subscriptions`) ──

/// Which door a subscriptions write goes through.
enum SubscriptionsWrite<'a> {
    Merge(&'a fauna_core::data::SubscriptionsConfig),
    Settle(&'a fauna_core::data::PendingRemoval),
}

/// Drive one subscriptions door (`subscription_rows::{merge_subscriptions,
/// settle_pending_removal}`) on `replica` over its fleet plane, then the
/// runtime's publish step. The fold as it now stands.
async fn subscriptions_door(
    replica: &Replica,
    write: SubscriptionsWrite<'_>,
) -> anyhow::Result<fauna_core::data::SubscriptionsConfig> {
    use fauna_sync_engine::subscription_rows::{merge_subscriptions, settle_pending_removal};
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let (folded, _moved) = match write {
        SubscriptionsWrite::Merge(replica_value) => {
            merge_subscriptions(&replica.store, &plane, replica_value).await?
        }
        SubscriptionsWrite::Settle(removal) => {
            settle_pending_removal(&replica.store, &plane, removal).await?
        }
    };
    plane.publish_pending().await.expect("publish step");
    Ok(folded)
}

fn tier_period(version: u64, key: u8) -> fauna_core::data::TierPeriod {
    fauna_core::data::TierPeriod {
        version,
        key: [key; 32].into(),
        rotated_at: version * 1_000 + u64::from(key),
        minted_by: Some(root().actor_id()),
    }
}

fn one_tier(
    name: &str,
    current: fauna_core::data::TierPeriod,
    prior: Vec<fauna_core::data::TierPeriod>,
) -> fauna_core::data::TierPeriodKeys {
    fauna_core::data::TierPeriodKeys {
        tier_name: name.into(),
        current,
        prior,
    }
}

/// **The period keys' convergence proof (the E3 slice's conformance step):
/// two devices of one account rotate the same tier concurrently and each
/// stages a removal — one row per period and per removal
/// (`config-dissolution.md` → *Bounded rows*) — and every key survives on
/// both: the concurrent loser in `prior`, never dropped; the fold identical,
/// every row in identical bytes.** Then a settle on one device retires the
/// sentinel on both, and a stale replica's unsettled copy cannot bring it
/// back. Real `GenerationTip` seals through the production door on both
/// replicas (generation 1 minted over both), real peer walks both ways, the
/// kind's own `CrdtPerField` arm merging. B starts empty and reads A's keys
/// back — the fresh-device read-back half of the key-material proof.
#[tokio::test(flavor = "multi_thread")]
async fn subscription_period_keys_converge_through_the_production_door() {
    use fauna_core::data::{PendingRemoval, SubscriptionsConfig};
    use fauna_protocol::merge_policy::KIND_SUBSCRIPTIONS;
    use fauna_sync_engine::subscription_rows::read_subscriptions;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    // A creates a tier and rotates it once. B, fresh, learns both keys.
    let created = SubscriptionsConfig {
        tiers: vec![one_tier(
            "gold",
            tier_period(2, 0x22),
            vec![tier_period(1, 0x11)],
        )],
        pending_removals: Vec::new(),
    };
    subscriptions_door(&a, SubscriptionsWrite::Merge(&created))
        .await
        .expect("A's door writes");
    b.walk_peer_scope(&ba, fleet).await;
    let on_b = read_subscriptions(&b.store).await.unwrap();
    assert_eq!(on_b, created, "B opened A's tip-sealed rows");

    // Concurrently, unaware of each other: both rotate gold to version 3
    // with different keys, and each stages a subscriber removal.
    let removal_a = PendingRemoval {
        tier_name: "gold".into(),
        subscriber_id: ActorKeypair::from_secret([0x31; 32]).actor_id(),
        new_period: tier_period(4, 0x4A),
    };
    let removal_b = PendingRemoval {
        tier_name: "gold".into(),
        subscriber_id: ActorKeypair::from_secret([0x32; 32]).actor_id(),
        new_period: tier_period(4, 0x4B),
    };
    let rotated_a = SubscriptionsConfig {
        tiers: vec![one_tier(
            "gold",
            tier_period(3, 0x3A),
            vec![tier_period(2, 0x22), tier_period(1, 0x11)],
        )],
        pending_removals: vec![removal_a.clone()],
    };
    let rotated_b = SubscriptionsConfig {
        tiers: vec![
            one_tier(
                "gold",
                tier_period(3, 0x3B),
                vec![tier_period(2, 0x22), tier_period(1, 0x11)],
            ),
            one_tier("silver", tier_period(1, 0x51), Vec::new()),
        ],
        pending_removals: vec![removal_b.clone()],
    };
    subscriptions_door(&a, SubscriptionsWrite::Merge(&rotated_a))
        .await
        .expect("A's door writes");
    subscriptions_door(&b, SubscriptionsWrite::Merge(&rotated_b))
        .await
        .expect("B's door writes");

    // Converge both ways.
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let expected = rotated_a.merge(&rotated_b);
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let view = read_subscriptions(store).await.unwrap();
        assert_eq!(view, expected, "{side}: the fold is the shipped merge");
        let gold = &view.tiers[0];
        assert_eq!(
            gold.current,
            tier_period(3, 0x3B),
            "{side}: the higher key wins"
        );
        assert!(
            gold.prior.contains(&tier_period(3, 0x3A)),
            "{side}: the concurrent loser's key survives in prior"
        );
        assert_eq!(
            view.pending_removals.len(),
            2,
            "{side}: both sentinels survive"
        );
    }
    let rows = expected.rows();
    assert_eq!(rows.len(), 7, "one row per period (5) and per removal (2)");
    for (key, _) in &rows {
        assert_eq!(
            stored(&a.store, KIND_SUBSCRIPTIONS, key).await,
            stored(&b.store, KIND_SUBSCRIPTIONS, key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // The echo-stop at the door: re-merging a replica the rows already
    // cover — a stale one included — puts nothing new.
    let before = a.store.frontier(fleet).await.unwrap();
    subscriptions_door(&a, SubscriptionsWrite::Merge(&created))
        .await
        .expect("a covered merge");
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a merge the rows already cover writes nothing"
    );

    // A settles its removal: the fresh key lands as a period row, the
    // sentinel leaves the fold on both, and B's stale re-merge of the
    // unsettled sentinel cannot resurrect it.
    let settled = subscriptions_door(&a, SubscriptionsWrite::Settle(&removal_a))
        .await
        .expect("A settles");
    assert_eq!(settled.pending_removals, vec![removal_b.clone()]);
    assert_eq!(
        settled.tiers[0].current,
        tier_period(4, 0x4A),
        "the settled removal's fresh key is the tier's current"
    );
    b.walk_peer_scope(&ba, fleet).await;
    subscriptions_door(&b, SubscriptionsWrite::Merge(&rotated_b.merge(&rotated_a)))
        .await
        .expect("B's stale merge");
    a.walk_peer_scope(&ab, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let view = read_subscriptions(store).await.unwrap();
        assert_eq!(
            view.pending_removals,
            vec![removal_b.clone()],
            "{side}: the settled sentinel stays settled"
        );
        assert_eq!(view.tiers[0].current, tier_period(4, 0x4A));
    }
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal a period key — the refusal lands before any
/// durable local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_subscriptions_door_refuses_while_no_tip_resolves() {
    use fauna_core::data::SubscriptionsConfig;
    use fauna_protocol::merge_policy::KIND_SUBSCRIPTIONS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let record = SubscriptionsConfig {
        tiers: vec![one_tier("gold", tier_period(1, 0x11), Vec::new())],
        pending_removals: Vec::new(),
    };
    let err = subscriptions_door(&a, SubscriptionsWrite::Merge(&record))
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_SUBSCRIPTIONS)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

// ── Shared-folder content keys (`config-dissolution.md` → *Bounded rows*) ──

/// Drive the folder-keys door (`folder_key_rows::merge_folder_keys`) on
/// `replica` over its fleet plane, then the runtime's publish step. Answers
/// whether the door put anything.
async fn folder_keys_door(
    replica: &Replica,
    custody: &fauna_core::data::FoldersConfig,
) -> anyhow::Result<bool> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let moved =
        fauna_sync_engine::folder_key_rows::merge_folder_keys(&replica.store, &plane, custody)
            .await?
            .1;
    plane.publish_pending().await.expect("publish step");
    Ok(moved)
}

/// Drive the folder-keys settle door on `replica`, then the publish step.
async fn settle_folder_removal(
    replica: &Replica,
    removal: &fauna_core::data::FolderPendingRemoval,
) -> anyhow::Result<bool> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let moved =
        fauna_sync_engine::folder_key_rows::settle_pending_removal(&replica.store, &plane, removal)
            .await?
            .1;
    plane.publish_pending().await.expect("publish step");
    Ok(moved)
}

fn content_key(version: u64, key: u8) -> fauna_core::folder_keys::ContentKeyGeneration {
    fauna_core::folder_keys::ContentKeyGeneration {
        version,
        key: [key; 32].into(),
        rotated_at: version * 1_000,
    }
}

/// **The folder-keys convergence proof (the E3 slice's conformance step):
/// two devices of one account, each writing custody the other has not seen —
/// A rotates the set on a member removal (staging it first) while B
/// concurrently rotates it to the same version under another key and records
/// a foreign set — converge through the production door: one row per entity,
/// the fold identical on both ends and equal to the blob arm's merge of the
/// two replicas, BOTH same-version keys kept, every row in identical bytes.**
/// Real `GenerationTip` seals (generation 1 over both), real peer walks both
/// ways, the kind's own `CrdtPerField` arm merging. A replica the door must
/// refuse leaves nothing.
#[tokio::test(flavor = "multi_thread")]
async fn folder_key_custody_converges_through_the_production_door() {
    use fauna_core::data::{FolderKeyCustody, FolderPendingRemoval, FoldersConfig, ForeignFolder};
    use fauna_core::folder_keys::FolderContentKeys;
    use fauna_protocol::merge_policy::KIND_FOLDER_KEYS;
    use fauna_sync_engine::folder_key_rows::read_folder_keys;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let channel = [0xC1; 32];

    // A creates, binds and keys the set; B learns it.
    let first = FoldersConfig {
        sets: vec![FolderKeyCustody {
            channel_id: Some(channel),
            keys: Some(FolderContentKeys::genesis([0x11; 32], 1_000)),
            set_nonce: Some([0x01; 32]),
            name: Some("photos".into()),
            created_at: 900,
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(folder_keys_door(&a, &first).await.expect("A's door writes"));
    b.walk_peer_scope(&ba, fleet).await;
    let on_b = read_folder_keys(&b.store).await.unwrap();
    assert_eq!(on_b, first, "B opened A's tip-sealed rows");

    // Concurrently, unaware of each other: A stages a removal and commits it
    // (generation 2 under 0x22); B rotates to generation 2 under 0x2B and
    // records a foreign set.
    let mut a_side = first.clone();
    a_side.pending_removals.push(FolderPendingRemoval {
        channel_id: channel,
        name: "photos".into(),
        removed_member: ActorKeypair::from_secret([0x55; 32]).actor_id(),
        new_generation: content_key(2, 0x22),
        commit: Some(vec![0xC0; 64]),
        gated_attempted: false,
    });
    folder_keys_door(&a, &a_side).await.expect("A stages");
    a_side.sets[0].keys = Some(
        a_side.sets[0]
            .keys
            .as_ref()
            .unwrap()
            .merge(&FolderContentKeys {
                current: content_key(2, 0x22),
                prior: vec![],
            }),
    );
    folder_keys_door(&a, &a_side).await.expect("A commits");
    let mut b_side = on_b.clone();
    b_side.sets[0]
        .keys
        .as_mut()
        .unwrap()
        .rotate([0x2B; 32], 2_000);
    b_side.foreign_sets.push(ForeignFolder {
        channel_id: [0x77; 32],
        mls_group_id: vec![0x78; 16],
        home_nest_url: "https://home.example".into(),
        home_nest_actor_id: None,
        set_name: Some("shared".into()),
        access: Some("reader".into()),
        content_key_floor: Some(1),
        ..Default::default()
    });
    folder_keys_door(&b, &b_side)
        .await
        .expect("B's door writes");

    // Converge both ways.
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let on_a = read_folder_keys(&a.store).await.unwrap();
    assert_eq!(
        on_a,
        read_folder_keys(&b.store).await.unwrap(),
        "the fold converges"
    );
    assert_eq!(
        on_a,
        a_side.merge(&b_side),
        "the fold is the blob arm's merge"
    );
    let keys = on_a.sets[0].keys.as_ref().unwrap();
    assert_eq!(
        keys.keys_for(2).count(),
        2,
        "both same-version keys survive"
    );
    assert_eq!(keys.keys_for(1).count(), 1);
    let rows = on_a.rows().unwrap();
    assert_eq!(
        rows.len(),
        1 + 3 + 1 + 1,
        "set + three generations + staging + foreign"
    );
    for (key, _) in &rows {
        assert_eq!(
            stored(&a.store, KIND_FOLDER_KEYS, key).await,
            stored(&b.store, KIND_FOLDER_KEYS, key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // The echo-stop at the door: a merge the rows already cover puts nothing
    // — and a replica that dropped a staging drops no row.
    let before = a.store.frontier(fleet).await.unwrap();
    let mut dropped = on_a.clone();
    dropped.pending_removals.clear();
    assert!(
        !folder_keys_door(&a, &dropped)
            .await
            .expect("a covered merge"),
        "a merge the rows already cover writes nothing"
    );
    assert_eq!(a.store.frontier(fleet).await.unwrap(), before);
    assert_eq!(read_folder_keys(&a.store).await.unwrap(), on_a);

    // The door's refusal: an entry with neither nonce nor channel has no row,
    // and the whole replica is refused before any put.
    let mut keyless = on_a.clone();
    keyless.sets.push(FolderKeyCustody {
        name: Some("orphan".into()),
        ..Default::default()
    });
    keyless.foreign_sets[0].content_key_floor = Some(9);
    folder_keys_door(&a, &keyless)
        .await
        .expect_err("an identity-less entry must be refused");
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a refused merge leaves nothing"
    );

    // The settle: A is done with the staging. It leaves the fold on both
    // replicas once B walks, its fresh key stays, and a stale replica's
    // unsettled copy cannot bring it back.
    let staged = on_a.pending_removals[0].clone();
    assert!(settle_folder_removal(&a, &staged).await.expect("A settles"));
    assert!(
        !settle_folder_removal(&a, &staged)
            .await
            .expect("a second settle"),
        "settling is idempotent"
    );
    b.walk_peer_scope(&ba, fleet).await;
    for store in [&a.store, &b.store] {
        let now = read_folder_keys(store).await.unwrap();
        assert!(
            now.pending_removals.is_empty(),
            "the settled staging left the fold"
        );
        assert_eq!(now.sets, on_a.sets, "no key moved or went missing");
    }
    assert!(
        !folder_keys_door(&b, &on_a)
            .await
            .expect("a stale replica's merge"),
        "the unsettled copy joins into the settled row and writes nothing"
    );
    assert!(
        read_folder_keys(&b.store)
            .await
            .unwrap()
            .pending_removals
            .is_empty()
    );
}

/// **The folder-keys door refuses while no generation tip resolves, and keeps
/// nothing** — the kind is `GenerationTip`-sealed, so the caller's leg stays
/// owed and retries.
#[tokio::test(flavor = "multi_thread")]
async fn the_folder_keys_door_refuses_while_no_tip_resolves() {
    use fauna_core::data::{FolderKeyCustody, FoldersConfig};
    use fauna_core::folder_keys::FolderContentKeys;
    use fauna_protocol::merge_policy::KIND_FOLDER_KEYS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let custody = FoldersConfig {
        sets: vec![FolderKeyCustody {
            channel_id: Some([0xC1; 32]),
            keys: Some(FolderContentKeys::genesis([0x11; 32], 1_000)),
            set_nonce: Some([0x01; 32]),
            ..Default::default()
        }],
        ..Default::default()
    };
    let err = folder_keys_door(&a, &custody)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_FOLDER_KEYS)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// Drive the deployment-seed door (`deployment_seed_rows::merge_deployment_seeds`)
/// on `replica` over its fleet plane, then the runtime's publish step. The
/// fold as it now stands.
async fn deployment_seeds_door(
    replica: &Replica,
    map: &[fauna_core::data::DeploymentSeedEntry],
) -> anyhow::Result<Vec<fauna_core::data::DeploymentSeedEntry>> {
    use fauna_sync_engine::deployment_seed_rows::merge_deployment_seeds;
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let (folded, _moved) = merge_deployment_seeds(&replica.store, &plane, map).await?;
    plane.publish_pending().await.expect("publish step");
    Ok(folded)
}

/// One box's custody entry: the seed `[seed; 32]`, its derived id, a label,
/// and (when rotated) the successor box's id.
fn seed_entry(
    seed: u8,
    domain: Option<&str>,
    superseded_by: Option<u8>,
) -> fauna_core::data::DeploymentSeedEntry {
    fauna_core::data::DeploymentSeedEntry {
        nest_actor_id: fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed([seed; 32]),
        seed: [seed; 32].into(),
        domain: domain.map(str::to_string),
        superseded_by: superseded_by
            .map(|s| fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed([s; 32])),
        ..Default::default()
    }
}

/// **The deployment seeds' convergence proof (the E3 slice's conformance
/// step): one row per custodied box (`config-dissolution.md` → *Bounded
/// rows*), real `GenerationTip` seals through the production door on both
/// replicas (generation 1 minted over both), real peer walks both ways, the
/// kind's own `CrdtPerField` arm merging.** B starts empty and reads A's box
/// back — the seed its recovery re-provisions the box from, re-derived to the
/// box's id: the fresh-device box-recovery read. Then two devices capture
/// different boxes concurrently and both survive on both, every row in
/// identical bytes; a rotation's supersession mark lands on both, and a
/// stale replica's unmarked copy cannot un-mark it.
#[tokio::test(flavor = "multi_thread")]
async fn deployment_seeds_converge_through_the_production_door() {
    use fauna_core::data::DeploymentSeedEntry;
    use fauna_core::deployment_seed_rows::deployment_seed_rows;
    use fauna_protocol::merge_policy::KIND_DEPLOYMENT_SEEDS;
    use fauna_sync_engine::deployment_seed_rows::read_deployment_seeds;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    // A claims box X. B, a fresh device, reads its seed back.
    let box_x = seed_entry(0x11, Some("x.example"), None);
    deployment_seeds_door(&a, std::slice::from_ref(&box_x))
        .await
        .expect("A's door writes");
    b.walk_peer_scope(&ba, fleet).await;
    let on_b = read_deployment_seeds(&b.store).await.unwrap();
    assert_eq!(on_b, vec![box_x.clone()], "B opened A's tip-sealed row");
    let recovered = on_b[0].seed.to_array();
    assert_eq!(
        fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(recovered),
        box_x.nest_actor_id,
        "the recovered seed re-creates box X's identity"
    );

    // Concurrently, unaware of each other: A claims box Y, B claims box Z.
    let box_y = seed_entry(0x22, Some("y.example"), None);
    let box_z = seed_entry(0x33, None, None);
    let on_a = vec![box_x.clone(), box_y.clone()];
    let on_b = vec![box_x.clone(), box_z.clone()];
    deployment_seeds_door(&a, &on_a)
        .await
        .expect("A's door writes");
    deployment_seeds_door(&b, &on_b)
        .await
        .expect("B's door writes");
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let expected = DeploymentSeedEntry::merge_seed_map(&on_a, &on_b);
    assert_eq!(expected.len(), 3, "every box survives");
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_deployment_seeds(store).await.unwrap(),
            expected,
            "{side}: the fold is the shipped union"
        );
    }
    for (key, _) in deployment_seed_rows(&expected) {
        assert_eq!(
            stored(&a.store, KIND_DEPLOYMENT_SEEDS, &key).await,
            stored(&b.store, KIND_DEPLOYMENT_SEEDS, &key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // The echo-stop at the door: re-merging a replica the rows already
    // cover — a stale one included — puts nothing new.
    let before = a.store.frontier(fleet).await.unwrap();
    deployment_seeds_door(&a, std::slice::from_ref(&box_x))
        .await
        .expect("a covered merge");
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a merge the rows already cover writes nothing"
    );

    // A rotates box X to box Y: the mark lands on both, and B's stale
    // re-merge of the unmarked entry cannot un-mark it.
    let rotated = seed_entry(0x11, Some("x.example"), Some(0x22));
    deployment_seeds_door(&a, std::slice::from_ref(&rotated))
        .await
        .expect("A marks the rotation");
    b.walk_peer_scope(&ba, fleet).await;
    deployment_seeds_door(&b, &on_b)
        .await
        .expect("B's stale merge");
    a.walk_peer_scope(&ab, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let view = read_deployment_seeds(store).await.unwrap();
        let x = view
            .iter()
            .find(|e| e.nest_actor_id == box_x.nest_actor_id)
            .expect("box X stays custodied");
        assert_eq!(
            x.superseded_by, rotated.superseded_by,
            "{side}: the supersession mark is never undone"
        );
        assert_eq!(x.seed, box_x.seed, "{side}: the seed bytes stay");
    }
}

/// The [`fauna_client_config::DeploymentSeedStore`] seam over one replica's
/// production door — the same `merge_deployment_seeds` join and the same
/// fold the account-store handle's impl reaches, minus the handle's command
/// channel this file never assembles.
///
/// That channel is what makes the handle's futures `Send` over a `!Sync`
/// SQLite store, and the seam demands `Send` futures natively. Here the
/// bound is type-level only: the one test that uses this awaits every seam
/// call inline on its own task and never spawns or shares the adapter, so
/// nothing crosses a thread while a borrow of the store is live.
struct ReplicaSeedStore<'a>(&'a Replica);

// SAFETY: see the type's doc — used by one test, awaited inline, never
// shared or moved across threads while in use.
unsafe impl Send for ReplicaSeedStore<'_> {}
unsafe impl Sync for ReplicaSeedStore<'_> {}

/// A future asserted `Send` — [`ReplicaSeedStore`]'s store calls, under the
/// same inline-await discipline.
struct AssertSend<F>(F);

// SAFETY: as for `ReplicaSeedStore` — polled inline by the awaiting test only.
unsafe impl<F> Send for AssertSend<F> {}

impl<F: std::future::Future> std::future::Future for AssertSend<F> {
    type Output = F::Output;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<F::Output> {
        // SAFETY: structural pinning of the only field; it is never moved out.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

#[async_trait::async_trait]
impl fauna_client_config::DeploymentSeedStore for ReplicaSeedStore<'_> {
    async fn seeds(
        &self,
    ) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>, fauna_client_config::StoreError> {
        AssertSend(fauna_sync_engine::deployment_seed_rows::read_deployment_seeds(&self.0.store))
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn merge_seeds(
        &self,
        replica: Vec<fauna_core::data::DeploymentSeedEntry>,
    ) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>, fauna_client_config::StoreError> {
        AssertSend(deployment_seeds_door(self.0, &replica))
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn seed_published(
        &self,
        _nest_actor_id: [u8; 32],
    ) -> Result<bool, fauna_client_config::StoreError> {
        unreachable!("the custody leg never asks; only the rotation drive does")
    }
}

/// **The custody leg through the production door: a claiming admin with an
/// empty fold lands the box's row under generation 1.** The leg runs over a
/// connection answering as the box does for its claiming admin
/// (`am_i_admin` yes, `deployment_seed.get` the box's own seed, its domain),
/// and its one write is the real `GenerationTip`-sealed put — so the row a
/// fresh device reads back at recovery is the one this lands, sealed under
/// the generation the fleet minted, and the sibling opens it.
#[tokio::test(flavor = "multi_thread")]
async fn the_custody_leg_lands_the_claimed_box_under_generation_one() {
    use fauna_client_config::{
        DeploymentSeedCustody, SupersessionMarks, run_deployment_seed_custody_leg,
    };
    use fauna_client_testkit::RejectingRequester;
    use fauna_core::identity::ActorId;
    use fauna_protocol::merge_policy::{KIND_DEPLOYMENT_SEEDS, KIND_GENERATION_MINT};
    use fauna_sync_engine::deployment_seed_rows::read_deployment_seeds;

    let TippedPair { a, b, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let the_box = seed_entry(0x44, Some("claimed.example"), None);
    let bound = ActorId(the_box.nest_actor_id);
    let nest = RejectingRequester::new()
        .reply(
            "fauna.account.am_i_admin",
            &fauna_protocol::account::AmIAdminReply {
                admin: true,
                extra: Default::default(),
            },
        )
        .reply(
            "fauna.admin.deployment_seed.get",
            &fauna_protocol::admin::AdminDeploymentSeedGetReply {
                deployment_seed: Some(fauna_core::hex32::encode(&[0x44; 32]).into()),
                extra: Default::default(),
            },
        )
        .reply(
            "fauna.nest.info",
            &fauna_protocol::discovery::NestInfoReply {
                domain: "claimed.example".into(),
                ..Default::default()
            },
        );

    let report = run_deployment_seed_custody_leg(&nest, bound, &ReplicaSeedStore(&a)).await;

    assert_eq!(report.custody, DeploymentSeedCustody::Captured);
    assert_eq!(report.marks, SupersessionMarks::NothingToCheck);
    assert_eq!(
        read_deployment_seeds(&a.store).await.unwrap(),
        vec![the_box.clone()]
    );

    // Sealed under generation 1 — the one mint the pair holds.
    let mints = a.store.states_of_kind(KIND_GENERATION_MINT).await.unwrap();
    assert_eq!(mints.len(), 1, "the pair minted exactly one generation");
    let generation_1 = fauna_core::hex32::decode(&mints[0].key).unwrap();
    let requester = NoNest;
    let (sk, sched, tr) = (signing_key(a.device), schedule(), trust());
    let plane =
        AccountStatePlane::new_pull_only(&a.store, &requester, &sched, &sk, &tr, fleet).unwrap();
    let mut sealed_under = Vec::new();
    let mut row_seq = 0;
    for row in plane
        .relay_rows_of_writer(&writer_id(a.device))
        .await
        .unwrap()
    {
        if plane
            .own_journal_item(row.writer_seq)
            .await
            .unwrap()
            .is_some_and(|item| item.kind == KIND_DEPLOYMENT_SEEDS)
        {
            row_seq = row.writer_seq;
            sealed_under.push(
                row.entry
                    .as_deref()
                    .and_then(fauna_core::account_entry_crypto::peek_generation_id),
            );
        }
    }
    assert_eq!(
        sealed_under,
        vec![Some(generation_1)],
        "one custody row, sealed under generation 1"
    );

    // The rotation drive's publication gate reads this row as owed until
    // this device's own frontier slot covers it — which is what a put the
    // nest accepted leaves behind (the publish step's own accounting).
    let published = || async {
        fauna_sync_engine::deployment_seed_rows::deployment_seed_published(
            &a.store, &plane, &bound.0,
        )
        .await
        .unwrap()
    };
    assert!(!published().await, "a local-only row is not published");
    a.store
        .advance_frontier(fleet, &writer_id(a.device), row_seq)
        .await
        .unwrap();
    assert!(published().await, "the row at the own slot is published");

    // The sibling opens it.
    b.walk_peer_scope(&ba, fleet).await;
    assert_eq!(
        read_deployment_seeds(&b.store).await.unwrap(),
        vec![the_box]
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing —
/// and refuses, before any put, an entry the plane would refuse.** The kind
/// is `GenerationTip`-sealed: a device that has minted and learned no
/// generation cannot seal a seed. And a replica carrying a forged box (a seed
/// that is not its id's preimage) writes none of its boxes.
#[tokio::test(flavor = "multi_thread")]
async fn the_deployment_seeds_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_DEPLOYMENT_SEEDS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = deployment_seeds_door(&a, &[seed_entry(0x11, None, None)])
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    let forged = fauna_core::data::DeploymentSeedEntry {
        seed: [0x77; 32].into(),
        ..seed_entry(0x22, None, None)
    };
    let err = deployment_seeds_door(&a, &[seed_entry(0x11, None, None), forged])
        .await
        .expect_err("a forged box is refused at the door");
    assert!(
        format!("{err:#}").contains("preimage"),
        "expected the self-consistency refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_DEPLOYMENT_SEEDS)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// Join `replica`-side `custody` into the account's ATProto identity rows
/// through the production door (`atproto_identity_rows::merge_atproto_identity`)
/// and take the runtime's publish step. Answers the joined fold.
async fn identity_door(
    replica: &Replica,
    custody: &fauna_core::data::AtprotoIdentityConfig,
) -> anyhow::Result<(fauna_core::data::AtprotoIdentityConfig, bool)> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let out = fauna_sync_engine::atproto_identity_rows::merge_atproto_identity(
        &replica.store,
        &plane,
        custody,
    )
    .await?;
    plane.publish_pending().await.expect("publish step");
    Ok(out)
}

fn identity_key(seed: u8, created_at: u64, dids: &[&str]) -> fauna_core::data::AtprotoRotationKey {
    fauna_core::data::AtprotoRotationKey {
        secret_scalar: fauna_core::secret::SecretArray32::new([seed; 32]),
        pubkey_did_key: format!("did:key:zDnae{seed:02x}"),
        created_at,
        published_for_dids: dids.iter().map(|d| (*d).to_string()).collect(),
    }
}

/// **The ATProto identity custody's convergence proof (the E3 slice's
/// conformance step): two devices of one account each mint a rotation key,
/// bind the SAME key to different DIDs, and record different consents and
/// contest intents, concurrently — every element survives on both replicas
/// (one row per element, joined by the per-element half of
/// `AtprotoIdentityConfig::merge`), the two replicas hold identical bytes per
/// row, and the fold equals the blob rail's composite merge of the two
/// sides.** Real `GenerationTip` seals through the production door on both
/// replicas (generation 1 minted over both), real peer walks both ways, the
/// registered `CrdtPerField` arm.
#[tokio::test(flavor = "multi_thread")]
async fn atproto_identity_converges_per_element_through_the_production_door() {
    use fauna_core::data::{AtprotoContestIntent, AtprotoIdentityConfig};
    use fauna_protocol::merge_policy::KIND_ATPROTO_IDENTITY;
    use fauna_sync_engine::atproto_identity_rows::read_atproto_identity;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    let shared_a = identity_key(1, 10, &["did:plc:aaa"]);
    let a_side = AtprotoIdentityConfig {
        rotation_keys: vec![shared_a.clone(), identity_key(2, 20, &[])],
        tombstone_consents: vec!["did:plc:aaa".into()],
        contest_intents: vec![AtprotoContestIntent {
            did: "did:plc:bbb".into(),
            contested_op_cid: "bafyop1".into(),
            requested_at: 40,
        }],
        nest_named_dids: vec!["did:plc:aaa".into()],
    };
    let b_side = AtprotoIdentityConfig {
        // The same key, converged from the log on B with a different binding.
        rotation_keys: vec![
            identity_key(1, 10, &["did:plc:bbb"]),
            identity_key(3, 30, &[]),
        ],
        tombstone_consents: vec![],
        contest_intents: vec![AtprotoContestIntent {
            did: "did:plc:bbb".into(),
            contested_op_cid: "bafyop1".into(),
            requested_at: 35,
        }],
        nest_named_dids: vec!["did:plc:bbb".into()],
    };
    assert!(identity_door(&a, &a_side).await.expect("A's door writes").1);
    assert!(identity_door(&b, &b_side).await.expect("B's door writes").1);

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    let want = a_side.merge(&b_side);
    assert_eq!(want.rotation_keys.len(), 3);
    assert_eq!(
        want.rotation_keys[0].published_for_dids,
        ["did:plc:aaa", "did:plc:bbb"]
    );
    assert_eq!(want.contest_intents[0].requested_at, 35);
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_atproto_identity(store).await.unwrap(),
            want,
            "{side}: every element survives, joined as the composite merge joins it"
        );
    }
    for (key, _) in want.rows().unwrap() {
        assert_eq!(
            stored(&a.store, KIND_ATPROTO_IDENTITY, &key).await,
            stored(&b.store, KIND_ATPROTO_IDENTITY, &key).await,
            "both replicas hold identical bytes for {key}"
        );
    }

    // The echo-stop at the door, and no shrinking: a replica missing
    // elements the store holds writes nothing and loses nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    let (joined, moved) = identity_door(&a, &a_side).await.expect("a covered join");
    assert!(!moved, "a replica the rows already cover writes nothing");
    assert_eq!(joined, want, "the door answers the whole fold");
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a covered join writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal a rotation key — the refusal lands before any
/// durable local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_atproto_identity_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_ATPROTO_IDENTITY;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let custody = fauna_core::data::AtprotoIdentityConfig {
        rotation_keys: vec![identity_key(1, 10, &[])],
        ..Default::default()
    };
    let err = identity_door(&a, &custody)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_ATPROTO_IDENTITY)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

// ── The mail custody (`mail-credentials.md`; `fauna.state.mail`) ──

/// Which mail door a test drives, and the instant it stamps with.
enum MailWrite<'a> {
    State(&'a fauna_core::mail_rows::MailStateRow),
    Put(&'a fauna_core::data::MailCredential),
    Mark(&'a str, fauna_core::data::MsekFingerprint),
    Revoke(&'a str),
    Generation(&'a fauna_core::data::PriorMsekRetirement),
}

/// Drive one mail door (`mail_rows::{write_mail_state, put_credential,
/// mark_credential_wrapped, revoke_credential, put_generation}`) on `replica` over its fleet
/// plane at the instant `at`, then the runtime's publish step. Whether the
/// door wrote.
async fn mail_door(replica: &Replica, write: MailWrite<'_>, at: u64) -> anyhow::Result<bool> {
    use fauna_sync_engine::mail_rows::{
        mark_credential_wrapped, put_credential, put_generation, revoke_credential,
        write_mail_state,
    };
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let now = fauna_core::data::Timestamp(at);
    let store = &replica.store;
    let wrote = match write {
        MailWrite::State(state) => write_mail_state(store, &plane, state, now).await?,
        MailWrite::Put(credential) => put_credential(store, &plane, credential, now).await?,
        MailWrite::Mark(id, fp) => mark_credential_wrapped(store, &plane, id, fp, now).await?,
        MailWrite::Revoke(id) => revoke_credential(store, &plane, id, now).await?,
        MailWrite::Generation(g) => put_generation(store, &plane, g).await?,
    };
    plane.publish_pending().await.expect("publish step");
    Ok(wrote)
}

fn a_mail_credential(id: &str, secret: &str, created_at: u64) -> fauna_core::data::MailCredential {
    fauna_core::data::MailCredential {
        credential_id: id.into(),
        display_name: id.to_uppercase(),
        kind: fauna_core::data::MailCredentialKind::Plain,
        secret: secret.as_bytes().to_vec().into(),
        created_at,
        updated_at: fauna_core::data::Timestamp::default(),
        wrapped_under: None,
        revoked_at_unix: None,
        burned: None,
    }
}

/// Both replicas hold identical bytes for every mail row either holds.
async fn assert_mail_rows_identical(a: &Replica, b: &Replica) {
    use fauna_protocol::merge_policy::KIND_MAIL;
    let keys = |rows: Vec<fauna_account_store::types::StateEntry>| -> Vec<String> {
        rows.into_iter().map(|e| e.key).collect()
    };
    let on_a = keys(a.store.states_of_kind(KIND_MAIL).await.unwrap());
    assert_eq!(
        on_a,
        keys(b.store.states_of_kind(KIND_MAIL).await.unwrap()),
        "both replicas hold the same mail rows"
    );
    for key in on_a {
        assert_eq!(
            stored(&a.store, KIND_MAIL, &key).await,
            stored(&b.store, KIND_MAIL, &key).await,
            "both replicas hold identical bytes for {key}"
        );
    }
}

/// **The mail custody's convergence proof (the E3 slice's conformance step):
/// two devices of one account add a credential each and write the state row
/// concurrently — both credentials survive on both replicas (one row per
/// credential: the blob's whole-record pick would drop one list), and the
/// state row joins the MSEK present-wins with the newer device's flags.**
/// Real `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the per-field
/// CRDT arm joining each row.
#[tokio::test(flavor = "multi_thread")]
async fn mail_rows_converge_per_credential_through_the_production_door() {
    use fauna_core::mail_rows::MailStateRow;
    use fauna_core::secret::SecretArray32;
    use fauna_sync_engine::mail_rows::read_mail;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let msek = SecretArray32::new([0x5e; 32]);

    // A enables mail (the MSEK) and adds iPhone Mail; B — never having heard
    // of A's MSEK — turns CalDAV on later and adds Thunderbird.
    let enabled = MailStateRow {
        msek: Some(msek.clone()),
        mail_enabled: Some(true),
        ..MailStateRow::default()
    };
    assert!(
        mail_door(&a, MailWrite::State(&enabled), 100)
            .await
            .unwrap()
    );
    let iphone = a_mail_credential("iphone-mail", "correct horse", 1);
    let thunderbird = a_mail_credential("thunderbird", "battery staple", 2);
    assert!(mail_door(&a, MailWrite::Put(&iphone), 101).await.unwrap());
    let caldav = MailStateRow {
        caldav_enabled: true,
        ..MailStateRow::default()
    };
    assert!(mail_door(&b, MailWrite::State(&caldav), 200).await.unwrap());
    assert!(
        mail_door(&b, MailWrite::Put(&thunderbird), 201)
            .await
            .unwrap()
    );

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let mail = read_mail(store).await.unwrap();
        assert_eq!(
            mail.msek,
            Some(msek.clone()),
            "{side}: the MSEK is never blanked"
        );
        assert_eq!(
            mail.mail_enabled,
            Some(true),
            "{side}: present-wins beside the MSEK"
        );
        assert!(mail.caldav_enabled, "{side}: the newer device's flags win");
        let ids: Vec<&str> = mail
            .credentials
            .iter()
            .map(|c| c.credential_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["iphone-mail", "thunderbird"],
            "{side}: both adds survive"
        );
        assert_eq!(mail.credentials[0].secret.as_slice(), b"correct horse");
    }
    assert_mail_rows_identical(&a, &b).await;

    // The echo-stop at the door: re-putting a row as it stands, or writing
    // the state row's own content, writes nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(!mail_door(&a, MailWrite::Put(&iphone), 300).await.unwrap());
    let joined = MailStateRow::from_config(
        &read_mail(&a.store).await.unwrap(),
        fauna_core::data::Timestamp::default(),
    );
    assert!(!mail_door(&a, MailWrite::State(&joined), 300).await.unwrap());
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "an unchanged write writes nothing"
    );
}

/// **The fresh-device read-back of the mail custody** — the irrecoverable
/// half of the kind (`key-material-hierarchy.md` § Path B: losing the MSEK
/// loses stored mail). A holds the MSEK with a grace generation and one
/// credential; B, a device that has written nothing, walks once and reads
/// back exactly the key material its mail apps open stored mail with: the
/// MSEK, the retained prior with its retirement instant, and the credential's
/// secret and generation marker.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_device_reads_the_mail_custody_back() {
    use fauna_core::data::{MsekFingerprint, PriorMsekRetirement};
    use fauna_core::mail_rows::MailStateRow;
    use fauna_core::secret::SecretArray32;
    use fauna_sync_engine::mail_rows::read_mail;

    let TippedPair { a, b, ba, .. } = tipped_pair().await;
    let msek = SecretArray32::new([0x71; 32]);
    let prior = SecretArray32::new([0x70; 32]);
    let state = MailStateRow {
        msek: Some(msek.clone()),
        mail_enabled: Some(true),
        ..MailStateRow::default()
    };
    assert!(mail_door(&a, MailWrite::State(&state), 100).await.unwrap());
    let generation = PriorMsekRetirement {
        msek: prior.clone(),
        retired_at_unix: 1_700_000_000,
    };
    assert!(
        mail_door(&a, MailWrite::Generation(&generation), 100)
            .await
            .unwrap()
    );
    let credential = fauna_core::data::MailCredential {
        wrapped_under: Some(MsekFingerprint::of(&msek)),
        ..a_mail_credential("default", "correct horse", 1)
    };
    assert!(
        mail_door(&a, MailWrite::Put(&credential), 101)
            .await
            .unwrap()
    );
    assert_eq!(
        read_mail(&b.store).await.unwrap(),
        fauna_core::data::MailConfig::default(),
        "B holds nothing before its walk"
    );

    b.walk_peer_scope(&ba, fleet_scope()).await;

    let mail = read_mail(&b.store).await.unwrap();
    assert_eq!(mail.msek, Some(msek.clone()), "B reads the MSEK back");
    assert_eq!(
        mail.prior_mseks,
        vec![prior.clone()],
        "and the grace generation"
    );
    assert_eq!(mail.prior_msek_retired_at(&prior), Some(1_700_000_000));
    assert_eq!(mail.credentials.len(), 1);
    assert_eq!(mail.credentials[0].secret.as_slice(), b"correct horse");
    assert_eq!(
        mail.credentials[0].wrapped_under,
        Some(MsekFingerprint::of(&msek)),
        "the generation marker rides the row"
    );
    assert_mail_rows_identical(&a, &b).await;
}

/// **The rotation finalize's revert, on the plane rows** (`mail-credentials.md`
/// § Cross-device finalize race). A swaps its MSEK to MSEK′; B, a sibling that
/// never saw the swap, finalizes its OWN concurrent rotation to MSEK″ under a
/// later stamp — the one write that can revert a swap, since every other state
/// write restates no key material and a restatement of the key a replica
/// already holds joins to no change (the door writes nothing). The state row's
/// join is a deliberate-rotation latest-wins on the stamp, so after the walks
/// both replicas hold MSEK″ — and MSEK′ is in neither `msek` nor a
/// generation row (a generation row is written only for the key a finalize
/// retires): the residual the finalize's read back exists to catch. A's
/// re-drive — the same swap, stamped strictly above what it now reads — lands
/// MSEK′ on both, the old key a prior generation.
#[tokio::test(flavor = "multi_thread")]
async fn a_reverted_mail_key_swap_is_caught_on_read_back_and_re_driven() {
    use fauna_core::data::PriorMsekRetirement;
    use fauna_core::mail_rows::{MailRotationSentinel, MailStateRow};
    use fauna_core::secret::SecretArray32;
    use fauna_sync_engine::mail_rows::read_mail;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let old = SecretArray32::new([0x01; 32]);
    let new = SecretArray32::new([0x02; 32]);
    let enabled = MailStateRow {
        msek: Some(old.clone()),
        mail_enabled: Some(true),
        ..MailStateRow::default()
    };
    assert!(
        mail_door(&a, MailWrite::State(&enabled), 100)
            .await
            .unwrap()
    );
    b.walk_peer_scope(&ba, fleet).await;

    // A's finalize: the old key's generation row FIRST, then the swap —
    // MSEK′ in, the sentinel kept until the read back confirms it.
    let retired = PriorMsekRetirement {
        msek: old.clone(),
        retired_at_unix: 1_700_000_300,
    };
    assert!(
        mail_door(&a, MailWrite::Generation(&retired), 300)
            .await
            .unwrap()
    );
    let swap = MailStateRow {
        msek: Some(new.clone()),
        pending_rotation: Some(MailRotationSentinel {
            new_msek: new.clone(),
        }),
        mail_enabled: Some(true),
        ..MailStateRow::default()
    };
    assert!(mail_door(&a, MailWrite::State(&swap), 300).await.unwrap());
    // B, stale, finalizes its own concurrent rotation under a later stamp.
    let other = SecretArray32::new([0x03; 32]);
    let concurrent = MailStateRow {
        msek: Some(other.clone()),
        mail_enabled: Some(true),
        ..MailStateRow::default()
    };
    let b_retired = PriorMsekRetirement {
        msek: old.clone(),
        retired_at_unix: 1_700_000_400,
    };
    assert!(
        mail_door(&b, MailWrite::Generation(&b_retired), 400)
            .await
            .unwrap()
    );
    assert!(
        mail_door(&b, MailWrite::State(&concurrent), 400)
            .await
            .unwrap()
    );
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    let read_back = read_mail(&a.store).await.unwrap();
    assert_eq!(read_back.msek, Some(other.clone()), "the swap was reverted");
    assert!(
        !read_back.prior_mseks.contains(&new) && read_back.pending_rotation.is_none(),
        "MSEK′ survives nowhere on the joined row — what the read back must catch"
    );

    // The re-drive: the same swap, stamped above what A now reads.
    assert!(mail_door(&a, MailWrite::State(&swap), 500).await.unwrap());
    b.walk_peer_scope(&ba, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let mail = read_mail(store).await.unwrap();
        assert_eq!(
            mail.msek,
            Some(new.clone()),
            "{side}: the re-driven swap holds"
        );
        assert_eq!(
            mail.prior_mseks,
            vec![old.clone()],
            "{side}: the old key is a prior generation"
        );
        assert_eq!(
            mail.prior_msek_retired_at(&old),
            Some(1_700_000_400),
            "{side}: two records of one retirement join on the later instant"
        );
    }
    assert_mail_rows_identical(&a, &b).await;
}

/// **A revoke and a burn each survive a concurrent re-wrap** (`config-dissolution.md`
/// § *The mail plane*: the markers are monotone). A revokes one credential and
/// holds another burned; B, not yet having heard of either marker, re-wraps
/// both under a new generation with a LATER stamp. After the walks both
/// replicas hold each row marked, with no secret — the re-wrap's generation
/// is kept, which is exactly what leaves the marked row owed a delete — the
/// fold hides the revoked row and shows the burned one, and the revoked id
/// stays spent.
#[tokio::test(flavor = "multi_thread")]
async fn a_mail_revoke_and_burn_survive_a_concurrent_rewrap() {
    use fauna_core::data::{MailSuccessionBurn, MsekFingerprint};
    use fauna_core::identity::ActorId;
    use fauna_core::secret::SecretArray32;
    use fauna_sync_engine::mail_rows::{mail_rows_of, read_mail};

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    let iphone = a_mail_credential("iphone-mail", "correct horse", 1);
    let laptop = a_mail_credential("laptop-mail", "battery staple", 2);
    assert!(mail_door(&a, MailWrite::Put(&iphone), 100).await.unwrap());
    assert!(mail_door(&a, MailWrite::Put(&laptop), 101).await.unwrap());
    b.walk_peer_scope(&ba, fleet).await;
    assert_eq!(read_mail(&b.store).await.unwrap().credentials.len(), 2);

    // A: revoke iPhone Mail; burn Laptop Mail (the succession leg's marker,
    // put through the door's read-join-put).
    assert!(
        mail_door(&a, MailWrite::Revoke("iphone-mail"), 200)
            .await
            .unwrap()
    );
    let burned = fauna_core::data::MailCredential {
        burned: Some(MailSuccessionBurn {
            predecessor: ActorId([0x77; 32]),
            at_unix: 1_800_000_000,
        }),
        secret: Default::default(),
        ..laptop.clone()
    };
    assert!(mail_door(&a, MailWrite::Put(&burned), 201).await.unwrap());
    // B, concurrently and later: re-wrap both under MSEK'.
    let fp = MsekFingerprint::of(&SecretArray32::new([0x6f; 32]));
    assert!(
        mail_door(&b, MailWrite::Mark("iphone-mail", fp), 300)
            .await
            .unwrap()
    );
    assert!(
        mail_door(&b, MailWrite::Mark("laptop-mail", fp), 301)
            .await
            .unwrap()
    );

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let rows = mail_rows_of(
            &store
                .states_of_kind(fauna_protocol::merge_policy::KIND_MAIL)
                .await
                .unwrap(),
        )
        .unwrap();
        let revoked = &rows.credentials["iphone-mail"];
        assert!(
            revoked.revoked_at_unix.is_some(),
            "{side}: the revoke survives"
        );
        assert!(
            revoked.secret.is_empty(),
            "{side}: a revoked row carries no secret"
        );
        assert_eq!(revoked.wrapped_under, Some(fp), "{side}: owed a delete");
        let burnt = &rows.credentials["laptop-mail"];
        assert_eq!(burnt.burned, burned.burned, "{side}: the burn survives");
        assert!(
            burnt.secret.is_empty(),
            "{side}: a burned row carries no secret"
        );
        assert_eq!(
            rows.spent_credential_ids(),
            ["iphone-mail", "laptop-mail"],
            "{side}: the revoked id stays spent"
        );
        let ids: Vec<String> = read_mail(store)
            .await
            .unwrap()
            .credentials
            .into_iter()
            .map(|c| c.credential_id)
            .collect();
        assert_eq!(ids, ["laptop-mail"], "{side}: revoked hidden, burned shown");
    }
    assert_mail_rows_identical(&a, &b).await;

    // A marked row is never re-wrapped, and a second revoke writes nothing.
    assert!(
        !mail_door(&a, MailWrite::Mark("laptop-mail", fp), 400)
            .await
            .unwrap()
    );
    assert!(
        !mail_door(&a, MailWrite::Revoke("iphone-mail"), 400)
            .await
            .unwrap()
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal the MSEK or a credential secret — the refusal
/// lands before any durable local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_mail_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_MAIL;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let state = fauna_core::mail_rows::MailStateRow {
        msek: Some(fauna_core::secret::SecretArray32::new([0x5e; 32])),
        ..Default::default()
    };
    for write in [
        MailWrite::State(&state),
        MailWrite::Put(&a_mail_credential("iphone-mail", "correct horse", 1)),
    ] {
        let err = mail_door(&a, write, 100)
            .await
            .expect_err("no tip resolves, so the door must refuse");
        assert!(
            format!("{err:#}").contains("no candidate generation tip resolves"),
            "expected the no-tip refusal, got: {err:#}"
        );
    }
    assert!(
        a.store.states_of_kind(KIND_MAIL).await.unwrap().is_empty(),
        "nothing durable is left behind"
    );
}

/// Join `custody` into the account's custody-ceremony rows through the
/// production door (`custody_ceremony_rows::merge_custody`) and take the
/// runtime's publish step. Answers the joined fold.
async fn custody_door(
    replica: &Replica,
    custody: &fauna_core::custody_ceremony::CustodyConfig,
) -> anyhow::Result<(fauna_core::custody_ceremony::CustodyConfig, bool)> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let out =
        fauna_sync_engine::custody_ceremony_rows::merge_custody(&replica.store, &plane, custody)
            .await?;
    plane.publish_pending().await.expect("publish step");
    Ok(out)
}

fn granted_custody(
    id: u8,
    accept: &[u8],
    minted: bool,
    updated_at: u64,
) -> fauna_core::custody_ceremony::GrantedCustody {
    fauna_core::custody_ceremony::GrantedCustody {
        grant_id: vec![id; 16],
        host: [2u8; 32],
        channel_hex: "aa".repeat(32),
        offer: vec![0xF0, id],
        offer_posted: true,
        accept: accept.to_vec(),
        minted,
        offered_at: fauna_core::data::Timestamp(1_000),
        duration_secs: 3_600,
        updated_at: fauna_core::data::Timestamp(updated_at),
        ..Default::default()
    }
}

/// **The custody ceremony state's convergence proof (the E3 slice's
/// conformance step): two devices of one account record the same ceremony
/// concurrently — one captured the host's accept, the other ran the mint —
/// and each also holds a ceremony the other never saw, on both sides
/// (granted and held). Every record survives on both replicas (one row per
/// ceremony side-record, joined by the per-record half of
/// `CustodyConfig::merge`), the two replicas hold identical bytes per row,
/// and the fold equals the blob rail's composite merge of the two sides.**
/// Real `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the registered
/// `CrdtPerField` arm.
#[tokio::test(flavor = "multi_thread")]
async fn custody_ceremony_converges_per_record_through_the_production_door() {
    use fauna_core::custody_ceremony::{CustodyConfig, HeldCustody};
    use fauna_protocol::merge_policy::KIND_CUSTODY_CEREMONY;
    use fauna_sync_engine::custody_ceremony_rows::read_custody;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    let a_side = CustodyConfig {
        granted: vec![granted_custody(0x1D, &[0xAC, 0x01], false, 2_000)],
        held: vec![HeldCustody {
            grant_id: vec![0x3F; 16],
            owner: [9u8; 32],
            channel_hex: "bb".repeat(32),
            offer: vec![0x0F, 0x3F],
            declined: true,
            updated_at: fauna_core::data::Timestamp(1_200),
            ..Default::default()
        }],
    };
    let b_side = CustodyConfig {
        granted: vec![
            granted_custody(0x1D, &[], true, 1_500),
            granted_custody(0x2E, &[], false, 1_000),
        ],
        held: Vec::new(),
    };
    assert!(custody_door(&a, &a_side).await.expect("A's door writes").1);
    assert!(custody_door(&b, &b_side).await.expect("B's door writes").1);

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    let want = a_side.merge(&b_side);
    assert_eq!(want.granted.len(), 2);
    assert_eq!(
        want.granted[0].accept,
        vec![0xAC, 0x01],
        "A's capture survives"
    );
    assert!(want.granted[0].minted, "B's progress survives");
    assert!(want.held[0].declined);
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_custody(store).await.unwrap(),
            want,
            "{side}: every record survives, joined as the composite merge joins it"
        );
    }
    for (key, _) in want.rows().unwrap() {
        assert_eq!(
            stored(&a.store, KIND_CUSTODY_CEREMONY, &key).await,
            stored(&b.store, KIND_CUSTODY_CEREMONY, &key).await,
            "both replicas hold identical bytes for {key}"
        );
    }

    // The echo-stop at the door, and no shrinking: a replica missing
    // records the store holds writes nothing and loses nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    let (joined, moved) = custody_door(&a, &a_side).await.expect("a covered join");
    assert!(!moved, "a replica the rows already cover writes nothing");
    assert_eq!(joined, want, "the door answers the whole fold");
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a covered join writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal a ceremony record — the refusal lands before any
/// durable local state, and the ceremony's act stays owed.
#[tokio::test(flavor = "multi_thread")]
async fn the_custody_ceremony_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_CUSTODY_CEREMONY;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let custody = fauna_core::custody_ceremony::CustodyConfig {
        granted: vec![granted_custody(0x1D, &[], false, 1_000)],
        held: Vec::new(),
    };
    let err = custody_door(&a, &custody)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_CUSTODY_CEREMONY)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// Drive the peer-anchor door (`peer_anchor_rows::merge_peer_anchors`) on
/// `replica` over its fleet plane, then the runtime's publish step. The fold
/// as it now stands.
async fn peer_anchors_door(
    replica: &Replica,
    anchors: &fauna_core::data::PeerAnchors,
) -> anyhow::Result<fauna_core::data::PeerAnchors> {
    use fauna_sync_engine::peer_anchor_rows::merge_peer_anchors;
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let (folded, _moved) = merge_peer_anchors(&replica.store, &plane, anchors).await?;
    plane.publish_pending().await.expect("publish step");
    Ok(folded)
}

/// One anchored actor's chain head: RecoveryKey `[key; 32]` at `seq`, first
/// sighted at `first_seen`.
fn anchor_head(
    actor: [u8; 32],
    key: u8,
    seq: u64,
    first_seen: u64,
    outrun: bool,
) -> fauna_core::data::PeerChainHead {
    fauna_core::data::PeerChainHead {
        actor: fauna_core::identity::ActorId(actor),
        recovery_pubkey: vec![key; 32],
        seq,
        first_seen: fauna_core::data::Timestamp(first_seen),
        outrun,
    }
}

/// One anchored actor's harvested home domain.
fn anchor_domain(
    actor: [u8; 32],
    host: &str,
    first_seen: u64,
) -> fauna_core::data::PeerAnchorDomain {
    fauna_core::data::PeerAnchorDomain {
        actor: fauna_core::identity::ActorId(actor),
        domain: host.to_string(),
        first_seen: fauna_core::data::Timestamp(first_seen),
    }
}

/// **The peer anchors' convergence proof (the E3 slice's conformance step):
/// one row per anchored actor per vector (`config-dissolution.md` → *Bounded
/// rows*), real `GenerationTip` seals through the production door on both
/// replicas (generation 1 minted over both), real peer walks both ways, the
/// kind's own `CrdtPerField` arm merging.** B starts empty and reads A's
/// harvest back. Then, concurrently, A harvests a second peer and marks the
/// first peer's head outrun while B's verified walk advances that head: both
/// converge on the shipped merge in identical bytes — the advance holds and
/// the stale head's mark does not follow it. A behind replica's re-merge
/// rewinds nothing, and a covered merge writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn peer_anchors_converge_through_the_production_door() {
    use fauna_core::data::PeerAnchors;
    use fauna_protocol::merge_policy::KIND_PEER_ANCHORS;
    use fauna_sync_engine::peer_anchor_rows::read_peer_anchors;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let (x, y) = ([0x11; 32], [0x22; 32]);

    // A harvests peer X. B, a fresh device, reads the anchors back.
    let harvested = PeerAnchors {
        chain_heads: vec![anchor_head(x, 0x10, 3, 100, false)],
        anchor_domains: vec![anchor_domain(x, "x.example", 100)],
    };
    peer_anchors_door(&a, &harvested)
        .await
        .expect("A's door writes");
    b.walk_peer_scope(&ba, fleet).await;
    assert_eq!(
        read_peer_anchors(&b.store).await.unwrap(),
        harvested,
        "B opened A's tip-sealed rows"
    );

    // Concurrently, unaware of each other: A harvests peer Y and marks X's
    // held head outrun; B's verified walk advances X's head.
    let on_a = PeerAnchors {
        chain_heads: vec![
            anchor_head(x, 0x10, 3, 100, true),
            anchor_head(y, 0x20, 1, 200, false),
        ],
        anchor_domains: vec![
            anchor_domain(x, "x.example", 100),
            anchor_domain(y, "y.example", 200),
        ],
    };
    let on_b = PeerAnchors {
        chain_heads: vec![anchor_head(x, 0x30, 5, 100, false)],
        anchor_domains: vec![anchor_domain(x, "x.example", 100)],
    };
    peer_anchors_door(&a, &on_a).await.expect("A's door writes");
    peer_anchors_door(&b, &on_b).await.expect("B's door writes");
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let expected = on_a.merge(&on_b);
    let x_head = expected
        .chain_heads
        .iter()
        .find(|h| h.actor.0 == x)
        .unwrap();
    assert_eq!(x_head.seq, 5, "the walk's advance holds");
    assert!(!x_head.outrun, "the stale head's mark does not follow it");
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_peer_anchors(store).await.unwrap(),
            expected,
            "{side}: the fold is the shipped merge"
        );
    }
    for (key, _) in expected.rows() {
        assert_eq!(
            stored(&a.store, KIND_PEER_ANCHORS, &key).await,
            stored(&b.store, KIND_PEER_ANCHORS, &key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // A behind replica — B still holding the harvest's seq-3 head — rewinds
    // nothing, and a merge the rows already cover writes nothing.
    let before = b.store.frontier(fleet).await.unwrap();
    let after_stale = peer_anchors_door(&b, &harvested)
        .await
        .expect("a covered merge");
    assert_eq!(after_stale, expected, "a behind replica rewinds no anchor");
    assert_eq!(
        b.store.frontier(fleet).await.unwrap(),
        before,
        "a merge the rows already cover writes nothing"
    );
}

/// **The ceiling holds at the door and at the fold (P2: one ceiling, one
/// order).** A holds a full vector; a new, later-sighted peer's head is cut
/// by the fold, so the door puts nothing for it — the writers' refusal,
/// never eviction. B, holding a disjoint full vector, converges with A to
/// the shipped merge's `MAX_PEER_ANCHOR_ENTRIES` oldest on both replicas,
/// though each store now holds both replicas' rows.
#[tokio::test(flavor = "multi_thread")]
async fn the_peer_anchor_ceiling_holds_at_the_door_and_the_fold() {
    use fauna_core::data::{MAX_PEER_ANCHOR_ENTRIES, PeerAnchors};
    use fauna_sync_engine::peer_anchor_rows::read_peer_anchors;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let full = |tag: u8, stamp: u64| PeerAnchors {
        chain_heads: (0..MAX_PEER_ANCHOR_ENTRIES as u64)
            .map(|i| {
                let mut actor = [tag; 32];
                actor[24..].copy_from_slice(&i.to_be_bytes());
                anchor_head(actor, 0x42, 1, stamp + i * 2, false)
            })
            .collect(),
        anchor_domains: Vec::new(),
    };
    let on_a = full(0xa0, 1_000);
    let on_b = full(0xb0, 1_001);
    assert_eq!(
        peer_anchors_door(&a, &on_a).await.expect("A fills"),
        on_a.merge(&PeerAnchors::default())
    );

    // A later-sighted newcomer is cut by the fold, so nothing is put.
    let before = a.store.frontier(fleet).await.unwrap();
    let newcomer = PeerAnchors {
        chain_heads: vec![anchor_head([0xcc; 32], 0x43, 1, 1_000_000, false)],
        anchor_domains: Vec::new(),
    };
    let folded = peer_anchors_door(&a, &newcomer)
        .await
        .expect("a full store refuses by not writing");
    assert!(
        !folded.chain_heads.iter().any(|h| h.actor.0 == [0xcc; 32]),
        "the newcomer is cut"
    );
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "the door puts no row the fold would cut"
    );

    peer_anchors_door(&b, &on_b).await.expect("B fills");
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    let expected = on_a.merge(&on_b);
    assert_eq!(expected.chain_heads.len(), MAX_PEER_ANCHOR_ENTRIES);
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_peer_anchors(store).await.unwrap(),
            expected,
            "{side}: the fold keeps the fleet's oldest anchors"
        );
    }
}

/// **The door refuses while no generation tip resolves, and keeps nothing —
/// and refuses, before any put, an entry the plane would refuse.** The kind
/// is `GenerationTip`-sealed: a device that has minted and learned no
/// generation cannot seal an anchor. And a replica carrying a misshapen head
/// (a RecoveryKey that is not 32 bytes) writes none of its anchors.
#[tokio::test(flavor = "multi_thread")]
async fn the_peer_anchors_door_refuses_while_no_tip_resolves() {
    use fauna_core::data::PeerAnchors;
    use fauna_protocol::merge_policy::KIND_PEER_ANCHORS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let good = PeerAnchors {
        chain_heads: vec![anchor_head([0x11; 32], 0x10, 3, 100, false)],
        anchor_domains: vec![anchor_domain([0x11; 32], "x.example", 100)],
    };
    let err = peer_anchors_door(&a, &good)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    let mut misshapen = good.clone();
    misshapen.chain_heads.push(fauna_core::data::PeerChainHead {
        recovery_pubkey: vec![0x20; 31],
        ..anchor_head([0x22; 32], 0x20, 1, 200, false)
    });
    let err = peer_anchors_door(&a, &misshapen)
        .await
        .expect_err("a misshapen head is refused at the door");
    assert!(
        format!("{err:#}").contains("RecoveryKey"),
        "expected the shape refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_PEER_ANCHORS)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

// ── The npub confirmation stamp (`nostr.md` § Key succession and rotation;
// `fauna.state.nostr-confirmation`) ──────────────────────────────────────────

/// Confirm at `now` on `replica` through the production door
/// (`nostr_confirmation_rows::confirm_nostr_npub`) over its fleet plane, then
/// the runtime's publish step. Whether the door wrote.
async fn confirm_npub(replica: &Replica, now: i64) -> anyhow::Result<bool> {
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let wrote =
        fauna_sync_engine::nostr_confirmation_rows::confirm_nostr_npub(&replica.store, &plane, now)
            .await?;
    plane.publish_pending().await.expect("publish step");
    Ok(wrote)
}

/// **The npub confirmation's convergence proof (the E3 slice's conformance
/// step): two devices of one account confirm concurrently, one later than the
/// other — both replicas converge on the later stamp, in identical bytes.**
/// Real `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the plane arm
/// joining by `NostrConfirmation::merge` (the max).
#[tokio::test(flavor = "multi_thread")]
async fn an_npub_confirmation_converges_on_the_later_stamp_through_the_production_door() {
    use fauna_protocol::merge_policy::{KIND_NOSTR_CONFIRMATION, NOSTR_CONFIRMATION_ROW_KEY};
    use fauna_sync_engine::nostr_confirmation_rows::read_nostr_confirmation;

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();

    // A confirms later than B, and neither has heard the other.
    assert!(confirm_npub(&a, 2_000).await.expect("A's door writes"));
    assert!(confirm_npub(&b, 1_000).await.expect("B's door writes"));

    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;

    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_nostr_confirmation(store).await.unwrap().confirmed_at,
            Some(2_000),
            "{side}: the later confirmation wins"
        );
    }
    assert_eq!(
        stored(
            &a.store,
            KIND_NOSTR_CONFIRMATION,
            NOSTR_CONFIRMATION_ROW_KEY
        )
        .await,
        stored(
            &b.store,
            KIND_NOSTR_CONFIRMATION,
            NOSTR_CONFIRMATION_ROW_KEY
        )
        .await,
        "both replicas hold identical bytes"
    );

    // The echo-stop at the door: an older confirmation never lowers the
    // stamp and puts nothing new.
    let before = b.store.frontier(fleet).await.unwrap();
    assert!(!confirm_npub(&b, 1_500).await.expect("an older confirm"));
    assert_eq!(
        b.store.frontier(fleet).await.unwrap(),
        before,
        "a confirmation at or before the stored stamp writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal the stamp — the refusal lands before any durable
/// local state.
#[tokio::test(flavor = "multi_thread")]
async fn the_npub_confirmation_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_NOSTR_CONFIRMATION;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = confirm_npub(&a, 2_000)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_NOSTR_CONFIRMATION)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// Drive the blessed-nests door (`blessed_nest_rows::set_nest_blessed`) on
/// `replica` over its fleet plane, then the runtime's publish step. Whether a
/// put happened.
async fn blessed_nest_door(
    replica: &Replica,
    nest_id: [u8; 32],
    blessed: bool,
    now: u64,
) -> anyhow::Result<bool> {
    use fauna_sync_engine::blessed_nest_rows::set_nest_blessed;
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let moved = set_nest_blessed(&replica.store, &plane, &nest_id, blessed, now).await?;
    plane.publish_pending().await.expect("publish step");
    Ok(moved)
}

/// **The blessed nests' convergence proof (the E3 slice's conformance step):
/// one row per nest (`config-dissolution.md` → *Bounded rows*), real
/// `GenerationTip` seals through the production door on both replicas
/// (generation 1 minted over both), real peer walks both ways, the kind's own
/// `CrdtPerField` arm merging.** A blesses nest X; B, a fresh device, reads
/// the blessing back. Then, concurrently, A un-blesses X while B withdraws
/// and re-blesses X at A's very stamp and blesses Y: both converge on the
/// shipped rule in identical bytes — the tie on X un-blesses, Y stays
/// blessed. A re-assert of the current verdict writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn blessed_nests_converge_through_the_production_door() {
    use fauna_core::data::BlessedNest;
    use fauna_protocol::merge_policy::KIND_BLESSED_NESTS;
    use fauna_sync_engine::blessed_nest_rows::{read_blessed_nests, read_nest_blessed};

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let (x, y) = ([0x11; 32], [0x22; 32]);

    // A blesses X. B, a fresh device, reads the blessing back.
    assert!(
        blessed_nest_door(&a, x, true, 100)
            .await
            .expect("A's door writes")
    );
    b.walk_peer_scope(&ba, fleet).await;
    assert_eq!(
        read_blessed_nests(&b.store).await.unwrap(),
        vec![BlessedNest {
            nest_id: x.to_vec(),
            blessed: true,
            at: 100
        }],
        "B opened A's tip-sealed row"
    );
    assert!(read_nest_blessed(&b.store, &x).await.unwrap());

    // Concurrently, unaware of each other: A un-blesses X at 200; B
    // withdraws X at 150, re-blesses it at 200 — a real tie with A's verdict
    // — and blesses Y.
    assert!(
        blessed_nest_door(&a, x, false, 200)
            .await
            .expect("A writes")
    );
    assert!(
        blessed_nest_door(&b, x, false, 150)
            .await
            .expect("B writes")
    );
    assert!(blessed_nest_door(&b, x, true, 200).await.expect("B writes"));
    assert!(blessed_nest_door(&b, y, true, 200).await.expect("B writes"));
    assert!(
        read_nest_blessed(&b.store, &x).await.unwrap(),
        "B holds its own blessing at the tied stamp"
    );
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;

    let expected = vec![
        BlessedNest {
            nest_id: x.to_vec(),
            blessed: false,
            at: 200,
        },
        BlessedNest {
            nest_id: y.to_vec(),
            blessed: true,
            at: 200,
        },
    ];
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_blessed_nests(store).await.unwrap(),
            expected,
            "{side}: the tie un-blesses X, Y stays blessed"
        );
        assert!(!read_nest_blessed(store, &x).await.unwrap());
        assert!(read_nest_blessed(store, &y).await.unwrap());
    }
    for key in expected.iter().map(BlessedNest::plane_key) {
        assert_eq!(
            stored(&a.store, KIND_BLESSED_NESTS, &key).await,
            stored(&b.store, KIND_BLESSED_NESTS, &key).await,
            "both replicas hold identical bytes at {key}"
        );
    }

    // A re-assert of the current verdict writes nothing.
    let before = a.store.frontier(fleet).await.unwrap();
    assert!(
        !blessed_nest_door(&a, x, false, 300)
            .await
            .expect("a re-assert")
    );
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a re-assert of the current verdict writes nothing"
    );
}

/// **The door refuses while no generation tip resolves, and keeps
/// nothing.** The kind is `GenerationTip`-sealed: a device that has minted
/// and learned no generation cannot seal a blessing.
#[tokio::test(flavor = "multi_thread")]
async fn the_blessed_nests_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_BLESSED_NESTS;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = blessed_nest_door(&a, [0x11; 32], true, 100)
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_BLESSED_NESTS)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

/// Drive the refused-change door
/// (`refused_change_rows::write_refused_scheduling_changes`) on `replica` over
/// its fleet plane, then the runtime's publish step. Whether it changed
/// anything.
async fn refused_changes_door(
    replica: &Replica,
    write: fauna_sync_engine::refused_change_rows::RefusedChangeWrite,
) -> anyhow::Result<bool> {
    use fauna_sync_engine::refused_change_rows::write_refused_scheduling_changes;
    let requester = NoNest;
    let sk = signing_key(replica.device);
    let sched = schedule();
    let tr = trust();
    let plane = AccountStatePlane::new_pull_only(
        &replica.store,
        &requester,
        &sched,
        &sk,
        &tr,
        fleet_scope(),
    )
    .unwrap();
    let changed = write_refused_scheduling_changes(&replica.store, &plane, write).await?;
    plane.publish_pending().await.expect("publish step");
    Ok(changed)
}

/// One refusal as the inbound drain reports it: a co-attendee's `CANCEL` on
/// event `uid`, refused at `at`.
fn a_refusal(uid: u8, at: i64) -> fauna_core::data::RefusedSchedulingChange {
    fauna_core::data::RefusedSchedulingChange {
        uid_hash: fauna_core::hex32::encode(&[uid; 32]),
        author: Some(fauna_core::hex32::encode(&[0xaa; 32])),
        author_home_nest_url: String::new(),
        sender_address: String::new(),
        method: "CANCEL".into(),
        reason: "not_the_organizer".into(),
        summary: "Kickoff".into(),
        first_refused_at: at,
        last_refused_at: at,
        occurrences: 0,
        dismissed_through: 0,
        extra: Default::default(),
    }
}

/// **The refused inbound scheduling changes' convergence proof (the E3
/// slice's conformance step): one `self` row, real `GenerationTip` seals
/// through the production door on both replicas (generation 1 minted over
/// both), real peer walks both ways, the kind's own `CrdtPerField` arm
/// merging.** The device that drained a refused message is the only one that
/// ever sees it, so B — which drained nothing — must list A's notice. Then the
/// two record different refusals concurrently and both survive on both in
/// identical bytes; a dismissal on B lands on A, and a later attempt recorded
/// on A re-opens the row on both.
#[tokio::test(flavor = "multi_thread")]
async fn refused_scheduling_changes_converge_through_the_production_door() {
    use fauna_core::refused_change_rows::REFUSED_CHANGES_ROW_KEY;
    use fauna_protocol::merge_policy::KIND_REFUSED_SCHEDULING_CHANGES;
    use fauna_sync_engine::refused_change_rows::{
        RefusedChangeWrite, read_refused_scheduling_changes,
    };

    let TippedPair { a, b, ab, ba, .. } = tipped_pair().await;
    let fleet = fleet_scope();
    let record = |uid, at| RefusedChangeWrite::Record(Box::new(a_refusal(uid, at)));

    // A drains a refused CANCEL. B reads the notice back.
    assert!(
        refused_changes_door(&a, record(1, 100))
            .await
            .expect("A's door writes")
    );
    b.walk_peer_scope(&ba, fleet).await;
    let on_b = read_refused_scheduling_changes(&b.store).await.unwrap();
    let [row]: [_; 1] = on_b.open().try_into().expect("one open notice on B");
    assert_eq!(
        row.uid_hash,
        a_refusal(1, 100).uid_hash,
        "B opened A's tip-sealed row"
    );
    assert_eq!(row.occurrences, 1);

    // Concurrently, unaware of each other: A and B each refuse a different
    // change.
    refused_changes_door(&a, record(2, 200))
        .await
        .expect("A's door writes");
    refused_changes_door(&b, record(3, 210))
        .await
        .expect("B's door writes");
    a.walk_peer_scope(&ab, fleet).await;
    b.walk_peer_scope(&ba, fleet).await;
    a.walk_peer_scope(&ab, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        assert_eq!(
            read_refused_scheduling_changes(store)
                .await
                .unwrap()
                .open()
                .len(),
            3,
            "{side}: both concurrent notices survive"
        );
    }
    assert_eq!(
        stored(
            &a.store,
            KIND_REFUSED_SCHEDULING_CHANGES,
            REFUSED_CHANGES_ROW_KEY
        )
        .await,
        stored(
            &b.store,
            KIND_REFUSED_SCHEDULING_CHANGES,
            REFUSED_CHANGES_ROW_KEY
        )
        .await,
        "both replicas hold identical bytes"
    );

    // The echo-stop at the door: a covered merge puts nothing new.
    let before = a.store.frontier(fleet).await.unwrap();
    let covered = read_refused_scheduling_changes(&b.store).await.unwrap();
    assert!(
        !refused_changes_door(&a, RefusedChangeWrite::Merge(Box::new(covered)))
            .await
            .expect("a covered merge")
    );
    assert_eq!(
        a.store.frontier(fleet).await.unwrap(),
        before,
        "a merge the row already covers writes nothing"
    );

    // B dismisses event 1's notice; A, which never dismissed, reads it closed.
    let key = row.key();
    assert!(
        refused_changes_door(&b, RefusedChangeWrite::Dismiss(key.clone()))
            .await
            .expect("B dismisses")
    );
    assert!(
        !refused_changes_door(&b, RefusedChangeWrite::Dismiss(key.clone()))
            .await
            .expect("a repeat dismissal"),
        "a repeat dismissal changes nothing"
    );
    a.walk_peer_scope(&ab, fleet).await;
    let on_a = read_refused_scheduling_changes(&a.store).await.unwrap();
    assert!(
        on_a.open().iter().all(|r| r.key() != key),
        "the dismissal made on B holds on A"
    );

    // A later attempt on event 1, drained on A, re-opens it on both.
    refused_changes_door(&a, record(1, 300))
        .await
        .expect("A records again");
    b.walk_peer_scope(&ba, fleet).await;
    for (side, store) in [("A", &a.store), ("B", &b.store)] {
        let list = read_refused_scheduling_changes(store).await.unwrap();
        let reopened = list
            .open()
            .into_iter()
            .find(|r| r.key() == key)
            .unwrap_or_else(|| panic!("{side}: the later attempt re-opens the notice"));
        assert_eq!(reopened.occurrences, 2, "{side}: the attempts are counted");
    }
}

/// **The door refuses while no generation tip resolves, and keeps nothing.**
/// The kind is `GenerationTip`-sealed: a device that has minted and learned
/// no generation cannot seal a notice.
#[tokio::test(flavor = "multi_thread")]
async fn the_refused_scheduling_changes_door_refuses_while_no_tip_resolves() {
    use fauna_protocol::merge_policy::KIND_REFUSED_SCHEDULING_CHANGES;
    use fauna_sync_engine::refused_change_rows::RefusedChangeWrite;

    let listeners: Listeners = fauna_transport::testing::listeners();
    let now = Arc::new(AtomicU64::new(5_000));
    let a = replica(DEVICE_A, &listeners, &now, None, QuotaConfig::default()).await;
    let err = refused_changes_door(&a, RefusedChangeWrite::Record(Box::new(a_refusal(1, 100))))
        .await
        .expect_err("no tip resolves, so the door must refuse");
    assert!(
        format!("{err:#}").contains("no candidate generation tip resolves"),
        "expected the no-tip refusal, got: {err:#}"
    );
    assert!(
        a.store
            .states_of_kind(KIND_REFUSED_SCHEDULING_CHANGES)
            .await
            .unwrap()
            .is_empty(),
        "nothing durable is left behind"
    );
}

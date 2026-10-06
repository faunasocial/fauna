//! **An engine converges a third-party kind it did not compile in**
//! (`docs/goal/architecture/third-party-kinds.md` § The kinds vocabulary →
//! *The registry overlay*; finding A6's whole claim).
//!
//! The kind `ext.example.com.notes` appears nowhere in this binary's compiled
//! registry. A replica that holds the account's admitted-kinds overlay — the
//! set a consenting device admitted from a verified manifest — opens, merges
//! and stores a row of it walked from its `ext:<kind>` scope; a replica
//! without the overlay leaves the same row unopened (the compat answer, "not
//! on the plane here"), and the writer door refuses to originate the kind
//! anywhere but its own scope.
//!
//! Every assertion is on latency-independent state: the stub answers
//! synchronously and the walks are driven explicitly.

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass, OP_STATE_PUT};
use fauna_protocol::ext_kind::ExtKind;
use fauna_protocol::merge_policy::{AdmittedKinds, LwwStamp, MergePolicy, merge_policy};
use fauna_protocol::scope::ext_scope;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{ByteBuf, decode_strict, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;

const KIND: &str = "ext.example.com.notes";
const KEY: &str = "note-1";

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([9u8; 32])
}

fn schedule() -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&BackupKey::derive(&[7u8; 32]))
}

/// The walk opens v1 rows only, and v1 opening consults no trust.
fn trust() -> GenerationTrust {
    GenerationTrust {
        root: root().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    }
}

/// The walking replica's own device key.
const DEVICE: [u8; 32] = [0xD0; 32];

/// The replica that wrote the row — another of the account's writers.
fn other_writer() -> SigningKey {
    SigningKey::from_bytes(&[0x5E; 32])
}

fn overlay() -> AdmittedKinds {
    let mut admitted = AdmittedKinds::new();
    admitted
        .admit(KIND.parse::<ExtKind>().unwrap(), MergePolicy::LatestWins)
        .unwrap();
    admitted
}

fn scope() -> String {
    ext_scope(&KIND.parse().unwrap())
}

/// One `ext.example.com.notes` row sealed by `writer` under the kind's
/// delegable pair — exactly what a consenting device (or a granted principal
/// holding the wrapped pair) seals.
fn sealed_row(writer: &SigningKey, value: &[u8], nest_seq: i64) -> SyncChange {
    let writer_id = writer.verifying_key().to_bytes();
    let scope = scope();
    let plaintext = EntryPlaintext {
        kind: KIND.into(),
        key: KEY.into(),
        merge_meta: Some(ByteBuf::from(
            LwwStamp {
                at_ms: 1_000 + nest_seq,
                writer: writer_id,
            }
            .encode()
            .unwrap(),
        )),
        value: ByteBuf::from(value.to_vec()),
        tombstone: false,
    };
    let keys = overlay().kind_keys(&schedule(), KIND).expect("admitted");
    let sealed = seal_entry(
        &keys,
        &EntryCoordinates {
            writer_id,
            writer_seq: 1,
            scope: &scope,
        },
        &plaintext,
        writer,
    )
    .expect("seal");
    SyncChange {
        seq: nest_seq,
        path_hash: hex::encode(sealed.item_key),
        size_bytes: sealed.envelope.len() as i64,
        change_type: OP_STATE_PUT.into(),
        item_class: Some(ItemClass::StateEntry.as_wire().into()),
        origin_writer: Some(hex::encode(writer_id)),
        origin_seq: Some(1),
        entry: Some(ByteBuf::from(sealed.envelope)),
        ..Default::default()
    }
}

/// A canned one-page feed of the `ext` scope.
struct StubFeed {
    rows: Vec<SyncChange>,
}

#[derive(Debug)]
struct StubError(anyhow::Error);

impl std::fmt::Display for StubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl fauna_protocol::RpcErrorClass for StubError {
    fn is_rejection(&self) -> bool {
        false
    }
}

impl fauna_protocol::RpcRequester for StubFeed {
    type Error = StubError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, StubError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.serve(kind, payload).map_err(StubError)
    }
}

impl StubFeed {
    fn serve<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(kind == "fauna.sync.changes.list", "feed only, got {kind}");
        let req: SyncChangesListRequest = decode_strict(&encode_canonical(&payload)?)?;
        anyhow::ensure!(req.scope.as_deref() == Some(scope().as_str()));
        let frontier = req.frontier.unwrap_or_default();
        let changes: Vec<SyncChange> = self
            .rows
            .iter()
            .filter(|row| {
                let slot = frontier.get(row.origin_writer.as_deref().unwrap()).copied();
                row.origin_seq.unwrap() > slot.unwrap_or(0)
            })
            .cloned()
            .collect();
        let reply = SyncChangesListReply {
            changes,
            ..Default::default()
        };
        Ok(decode_strict(&encode_canonical(&reply)?)?)
    }
}

async fn replica() -> AccountStore<SqliteBackend> {
    AccountStore::open(
        SqliteBackend::open_in_memory().unwrap(),
        &hex::encode(root().actor_id().0),
        WriterId(SigningKey::from_bytes(&DEVICE).verifying_key().to_bytes()),
    )
    .await
    .unwrap()
}

/// The owner's grant log authorizing `writer` over [`KIND`]: one root-signed
/// `Mint` carrying a `content.write` tuple confined to the writer's key,
/// filed as the `fauna.state.succession-ledger` event row it rides in
/// production (`third-party-kinds.md` § Principal write authority).
async fn grant_write(store: &AccountStore<SqliteBackend>, writer: &SigningKey) {
    use fauna_core::grant_event::{
        CLASS_CONTENT_WRITE, GrantEvent, GrantEventKind, GrantEventScope, writer_factor,
    };
    use fauna_core::succession_ledger::SuccessionLedgerRecord;
    let event = GrantEvent {
        grant_id: vec![1; 16],
        holder: vec![2; 32],
        kind: GrantEventKind::Mint,
        scope: vec![
            GrantEventScope {
                class: CLASS_CONTENT_WRITE.into(),
                kind: Some(KIND.into()),
                tier: None,
            }
            .with_factor(&writer_factor(&writer.verifying_key().to_bytes())),
        ],
        window_start: 0,
        window_end: 1,
        at: 1,
        sig: Vec::new(),
    }
    .sign(root().signing_key())
    .unwrap();
    let record = SuccessionLedgerRecord::Event(event);
    store
        .put_state(fauna_account_store::types::StateEntry {
            kind: fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER.into(),
            key: record.key().unwrap().render(),
            scope: fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE.into(),
            value: record.encode().unwrap(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_replica_holding_the_overlay_converges_a_kind_it_did_not_compile_in() {
    assert_eq!(
        merge_policy(KIND),
        None,
        "the fixture kind must not be compiled in"
    );
    let feed = StubFeed {
        rows: vec![sealed_row(&other_writer(), b"hello", 1)],
    };
    let store = replica().await;
    grant_write(&store, &other_writer()).await;
    let (sk, sched, tr, admitted) = (
        SigningKey::from_bytes(&DEVICE),
        schedule(),
        trust(),
        overlay(),
    );
    let report = AccountStatePlane::new(&store, &feed, &sched, &sk, &tr, scope())
        .unwrap()
        .with_admitted_kinds(&admitted)
        .walk()
        .await
        .unwrap();
    assert_eq!(report.unopened, 0, "{report:?}");
    assert_eq!(report.unmergeable, 0, "{report:?}");
    let entry = store.state(KIND, KEY).await.unwrap().expect("converged");
    assert_eq!(entry.value, b"hello");
    assert_eq!(entry.scope, scope());
}

/// **Read capability never conveys write authority** (`third-party-kinds.md`
/// § Principal write authority, position (2)): a row of an admitted kind,
/// sealed under the kind's pair and signed by a writer that is neither one of
/// the account's devices nor granted `content.write` over the kind, is skipped
/// and counted — and re-presented, so the first walk after the owner's grant
/// event lands admits it. A grant over the kind to ANOTHER writer is no
/// authority for this one.
#[tokio::test]
async fn a_writer_without_a_content_write_grant_is_refused_until_one_lands() {
    let feed = StubFeed {
        rows: vec![sealed_row(&other_writer(), b"hello", 1)],
    };
    let store = replica().await;
    let (sk, sched, tr, admitted) = (
        SigningKey::from_bytes(&DEVICE),
        schedule(),
        trust(),
        overlay(),
    );
    let plane = AccountStatePlane::new(&store, &feed, &sched, &sk, &tr, scope())
        .unwrap()
        .with_admitted_kinds(&admitted);

    let report = plane.walk().await.unwrap();
    assert_eq!(report.unopened, 0, "the pair opens it: {report:?}");
    assert_eq!(report.unmergeable, 1, "{report:?}");
    assert!(store.state(KIND, KEY).await.unwrap().is_none());

    // A grant over the kind to another writer key is no authority for this one.
    grant_write(&store, &SigningKey::from_bytes(&[0x77; 32])).await;
    assert_eq!(plane.walk().await.unwrap().unmergeable, 1);

    grant_write(&store, &other_writer()).await;
    let report = plane.walk().await.unwrap();
    assert_eq!(report.unmergeable, 0, "{report:?}");
    assert_eq!(
        store
            .state(KIND, KEY)
            .await
            .unwrap()
            .expect("admitted")
            .value,
        b"hello"
    );
}

#[tokio::test]
async fn a_replica_without_the_overlay_leaves_the_row_unopened() {
    let feed = StubFeed {
        rows: vec![sealed_row(&other_writer(), b"hello", 1)],
    };
    let store = replica().await;
    let (sk, sched, tr) = (SigningKey::from_bytes(&DEVICE), schedule(), trust());
    let report = AccountStatePlane::new(&store, &feed, &sched, &sk, &tr, scope())
        .unwrap()
        .walk()
        .await
        .unwrap();
    assert_eq!(report.unopened, 1, "{report:?}");
    assert!(store.state(KIND, KEY).await.unwrap().is_none());
}

#[tokio::test]
async fn the_writer_door_originates_an_admitted_kind_only_into_its_own_scope() {
    let feed = StubFeed { rows: Vec::new() };
    let store = replica().await;
    let (sk, sched, tr, admitted) = (
        SigningKey::from_bytes(&DEVICE),
        schedule(),
        trust(),
        overlay(),
    );
    let item = ItemId {
        kind: KIND.into(),
        key: KEY.into(),
    };
    let stamp = || {
        Some(
            LwwStamp {
                at_ms: 5,
                writer: sk.verifying_key().to_bytes(),
            }
            .encode()
            .unwrap(),
        )
    };

    // Without the overlay the kind is not on the plane here.
    let bare = AccountStatePlane::new(&store, &feed, &sched, &sk, &tr, scope()).unwrap();
    assert!(bare.put_local(&item, b"v".to_vec(), stamp()).await.is_err());

    // Into `state` — the A5 partition routes it to `ext:<kind>` alone.
    let wrong = AccountStatePlane::new(&store, &feed, &sched, &sk, &tr, ACCOUNT_STATE_SCOPE)
        .unwrap()
        .with_admitted_kinds(&admitted);
    let err = wrong
        .put_local(&item, b"v".to_vec(), stamp())
        .await
        .expect_err("off-partition origination");
    assert!(
        format!("{err:#}").contains("ext:ext.example.com.notes"),
        "{err:#}"
    );

    // Its own scope, with the overlay: admitted, and LWW's stamp is required.
    let home = AccountStatePlane::new(&store, &feed, &sched, &sk, &tr, scope())
        .unwrap()
        .with_admitted_kinds(&admitted);
    assert!(home.put_local(&item, b"v".to_vec(), None).await.is_err());
    home.put_local(&item, b"v".to_vec(), stamp()).await.unwrap();
    let entry = store.state(KIND, KEY).await.unwrap().expect("written");
    assert_eq!(entry.value, b"v");
}

/// **The account driver's route** (`third-party-kinds.md` § The `ext`
/// sub-scope; the pump's `ext.*` step): the overlay is read off the merged
/// `fauna.state.kind-manifest` row — re-verified against its `client_id`'s
/// host — and the `ext:<kind>` plane is derived from the bound fleet plane
/// for every kind it admits. A replica whose only knowledge of the kind is
/// that row converges a row of it; a row whose manifest fails to verify
/// admits nothing, so no plane is derived for its kinds.
#[tokio::test]
async fn the_fleet_plane_derives_an_ext_plane_for_every_kind_the_manifest_rows_admit() {
    use fauna_account_plane::kind_manifest_rows::read_admitted_kinds;
    use fauna_protocol::kind_manifest::{KindManifestRecord, ed25519_did_key, sign_manifest};

    let publisher = SigningKey::from_bytes(&[0x42; 32]);
    let jws = sign_manifest(
        &publisher,
        &serde_json::json!({
            "version": 1,
            "publisher": {
                "domain": "example.com",
                "key": ed25519_did_key(&publisher.verifying_key().to_bytes()),
            },
            "kinds": [{
                "kind": KIND, "class": "state", "merge": "latest-wins", "floor": "none"
            }],
        }),
        None,
    );
    let manifest_row = |client_id: &str| fauna_account_store::types::StateEntry {
        kind: fauna_protocol::merge_policy::KIND_KIND_MANIFEST.into(),
        key: client_id.into(),
        scope: fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE.into(),
        value: fauna_core::encoding::canonical_encode(&KindManifestRecord {
            jws: jws.clone(),
            admitted_at_ms: 1,
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    };

    let feed = StubFeed {
        rows: vec![sealed_row(&other_writer(), b"hello", 1)],
    };
    let store = replica().await;
    grant_write(&store, &other_writer()).await;
    let (sk, sched, tr) = (SigningKey::from_bytes(&DEVICE), schedule(), trust());
    let fleet = AccountStatePlane::new(
        &store,
        &feed,
        &sched,
        &sk,
        &tr,
        fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap();

    // A manifest filed under another host verifies against nothing.
    store
        .put_state(manifest_row("https://other.org/client-metadata.json"))
        .await
        .unwrap();
    let overlay = read_admitted_kinds(&store).await.unwrap();
    assert_eq!(overlay.kinds.kinds().count(), 0);
    assert_eq!(overlay.refused.len(), 1, "{:?}", overlay.refused);

    store
        .put_state(manifest_row("https://example.com/client-metadata.json"))
        .await
        .unwrap();
    let overlay = read_admitted_kinds(&store).await.unwrap();
    let kinds: Vec<_> = overlay.kinds.kinds().cloned().collect();
    assert_eq!(kinds, vec![KIND.parse::<ExtKind>().unwrap()]);

    let plane = fleet.ext_plane(&kinds[0], &overlay.kinds);
    assert_eq!(plane.scope(), scope());
    let report = plane.reconcile().await.unwrap();
    assert_eq!(report.unopened, 0, "{report:?}");
    assert_eq!(report.unmergeable, 0, "{report:?}");
    assert_eq!(
        store
            .state(KIND, KEY)
            .await
            .unwrap()
            .expect("converged")
            .value,
        b"hello"
    );
}

//! **A sibling's fence must not make a still-open handle disown its own
//! store's rows** (`docs/goal/architecture/account-sync-plane.md` § The peer
//! leg, § Feeds and cursors → the frontier vector; `account-replica-posture.md`
//! § The store device principal → succession decision 4).
//!
//! `AccountStore::open` snapshots the store's `writer` and `retired` sets and
//! never reassigns them — `rotate_writer_identity` takes the backend, not the
//! store, so **no open handle ever learns of a fence**. The peer leg's
//! self-echo exception used to decide "is this row mine?" from exactly that
//! pair, and ingest is deliberately unguarded (succession decision 4 fences
//! *appends*, and `StaleWriter`'s own doc says "ingest of another writer's
//! rows is never guarded"), so nothing stopped a stale handle from walking.
//!
//! After a co-located sibling fenced `A -> B`, an `A`-handle therefore
//! answered "not mine" for a row authored by **`B` — the store's own current
//! writer** — and its frozen `retired` set did not contain `B` either. The
//! exception did not fire and the row fell through to ingest, with two
//! consequences on opposite branches:
//!
//! 1. **The published-to-the-nest watermark moves on a peer's word.** The
//!    ingest tail advances the row's frontier slot unconditionally, and the
//!    own slot doubles as `publish_pending`'s high-water. A peer echoing the
//!    row proves only that the *peer* holds it — so the nest leg pages
//!    `after = published` past rows the nest never received, and the frontier
//!    is MAX-merge, so it never regresses. Un-pushed rows are lost to the nest
//!    for good: the **No user-data loss** invariant (`principles.md`) as
//!    non-delivery.
//! 2. **The walk aborts.** Where the store already holds that `(writer, seq)`
//!    in locally-authored form — the normal case, since the successor authored
//!    through the same store dir — the wire form re-derives a different
//!    `ItemRef` (the V13 provenance rule) and `ingest_state` bails "journal
//!    equivocation", which `apply` propagates. The walk keeps aborting until
//!    the handle reassembles.
//!
//! Which of the two a given row takes is an accident of arithmetic: the local
//! entry counter is per `(kind, key)` while the ingest re-derivation uses the
//! origin seq, so a store's FIRST row collides at `1 == 1` and ingests
//! idempotently (consequence 1), and the next row under a different key does
//! not (consequence 2). Both rows are exercised below, in that order.
//!
//! The fix is one live read (`AccountStore::writer_relation`), so consequence
//! 2 needs no separate remedy: a row correctly recognized as our own never
//! reaches ingest at all.

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::{AccountStore, rotate_writer_identity, stamped_writer};
use fauna_account_store::types::WriterId;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::ModerationConfig;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::merge_policy::{KIND_MODERATION, KIND_SEEN_SET, LwwStamp, MODERATION_KEY};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{decode_strict, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([9u8; 32])
}

fn schedule() -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&BackupKey::derive(&[7u8; 32]))
}

fn trust() -> GenerationTrust {
    GenerationTrust {
        root: root().actor_id(),
        prior: Vec::new(),
        trusted_holders: vec![
            SigningKey::from_bytes(&[0x66u8; 32])
                .verifying_key()
                .to_bytes(),
        ]
        .into(),
    }
}

fn signing_key(device: u8) -> SigningKey {
    SigningKey::from_bytes(&[device; 32])
}

fn writer_id(device: u8) -> WriterId {
    WriterId(signing_key(device).verifying_key().to_bytes())
}

async fn open_store(dir: &std::path::Path, device: u8) -> AccountStore<SqliteBackend> {
    AccountStore::open(
        SqliteBackend::open(dir).unwrap(),
        &hex::encode(root().actor_id().0),
        writer_id(device),
    )
    .await
    .unwrap()
}

/// The requester a local, pull-only put must never reach.
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

/// **The peer, echoing our own rows back at us.** Serves the replica's own
/// relay rows verbatim — which is exactly what a peer holding them would
/// serve — honouring the request frontier so a caught-up walk terminates.
struct EchoingPeer {
    scope: String,
    rows: Vec<SyncChange>,
}

impl fauna_protocol::RpcRequester for EchoingPeer {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(
            kind == "fauna.sync.changes.list",
            "the echoing peer serves the feed only, got {kind}"
        );
        let req: SyncChangesListRequest =
            decode_strict(&encode_canonical(&payload)?).expect("feed request shape");
        anyhow::ensure!(req.scope.as_deref() == Some(self.scope.as_str()));
        let frontier = req.frontier.unwrap_or_default();
        let reply = SyncChangesListReply {
            changes: self
                .rows
                .iter()
                .filter(|r| {
                    let writer = r.origin_writer.as_deref().unwrap_or_default();
                    r.origin_seq.unwrap_or(0) > frontier.get(writer).copied().unwrap_or(0)
                })
                .cloned()
                .collect(),
            ..Default::default()
        };
        Ok(decode_strict(&encode_canonical(&reply)?)?)
    }
}

/// One replica's sealed relay rows, as the feed a peer holding them serves.
async fn echo_of(store: &AccountStore<SqliteBackend>, scope: &str) -> EchoingPeer {
    let relay = store
        .relay_rows(scope, ItemClass::StateEntry.as_wire(), &[], 100)
        .await
        .unwrap();
    assert!(
        !relay.is_empty(),
        "fixture: the successor's rows must reach the relay plane, or the walk \
         below would prove nothing"
    );
    EchoingPeer {
        scope: scope.to_string(),
        rows: relay
            .iter()
            .enumerate()
            .map(|(i, r)| SyncChange {
                seq: (i + 1) as i64,
                path_hash: hex::encode(r.item_key.as_slice()),
                change_type: r.op.clone(),
                origin_writer: Some(r.writer.to_hex()),
                origin_seq: Some(r.writer_seq as i64),
                entry: r.entry.clone().map(Into::into),
                ..Default::default()
            })
            .collect(),
    }
}

async fn slot_of(store: &AccountStore<SqliteBackend>, scope: &str, writer: &WriterId) -> u64 {
    store
        .frontier(scope)
        .await
        .unwrap()
        .into_iter()
        .find(|(w, _)| w == writer)
        .map_or(0, |(_, seq)| seq)
}

// ── The staged replica ───────────────────────────────────────────────────────

const A: u8 = 0xA1;
const B: u8 = 0xB2;

/// The scenario both tests below run on: a handle opened BEFORE a co-located
/// sibling fences `A -> B` and never reopened (the agent's assembly-time store
/// handle — the live production caller is `peer_leg.rs`'s `new_pull_only`),
/// and the successor's own un-pushed rows, authored through the same store dir
/// as succession's in-place re-stamp means they are.
///
/// Returns `(stale, successor)`.
async fn fenced_replica(
    dir: &std::path::Path,
    rows: &[(i64, &str, &str)],
) -> (AccountStore<SqliteBackend>, AccountStore<SqliteBackend>) {
    let stale = open_store(dir, A).await;

    // The lost-slot heal's own flagship scenario. Our handle is not told.
    rotate_writer_identity(stale.backend(), &writer_id(A), &writer_id(B))
        .await
        .unwrap();

    // The probe's own premises, asserted before anything is concluded from
    // them: the two writers really differ, the fence really took, and the
    // handle really is stale.
    assert_ne!(
        writer_id(A),
        writer_id(B),
        "vacuous otherwise: A and B must be different writers"
    );
    assert_eq!(
        stamped_writer(stale.backend()).await.unwrap(),
        Some(writer_id(B)),
        "the fence must have landed, or there is no staleness to test"
    );
    assert_eq!(
        stale.writer(),
        writer_id(A),
        "the open-time cache must still answer A, or the handle is not stale"
    );

    let successor = open_store(dir, B).await;
    {
        let requester = NoNest;
        let sk = signing_key(B);
        let sched = schedule();
        let tr = trust();
        // Pull-only, like every put in this file's sibling tests: there is no
        // nest here, and a local put is local-first by the plane's own law.
        let plane = AccountStatePlane::new_pull_only(
            &successor,
            &requester,
            &sched,
            &sk,
            &tr,
            ACCOUNT_STATE_SCOPE,
        )
        .unwrap();
        for (at_ms, kind, key) in rows {
            plane
                .put(
                    &ItemId {
                        kind: (*kind).into(),
                        key: (*key).into(),
                    },
                    encode_canonical(&ModerationConfig {
                        muted_keywords: vec!["authored-by-the-successor".into()],
                        ..Default::default()
                    })
                    .unwrap()
                    .to_vec(),
                    Some(
                        LwwStamp {
                            at_ms: *at_ms,
                            writer: writer_id(B).0,
                        }
                        .encode()
                        .unwrap(),
                    ),
                )
                .await
                .expect("the successor's local put");
        }
    }

    // Nothing has published these rows anywhere: B's slot is the un-pushed
    // tail's own high-water, and it starts at zero.
    assert_eq!(
        slot_of(&successor, ACCOUNT_STATE_SCOPE, &writer_id(B)).await,
        0,
        "fixture: a local put must not advance the published high-water"
    );

    (stale, successor)
}

// ── The two consequences ─────────────────────────────────────────────────────

/// **Consequence 1 — the silent one.** One row, and one only: the store's
/// first, whose local `entry_version` and its ingest re-derivation collide at
/// `1 == 1`, so ingest is idempotent and nothing complains. Before the fix the
/// walk therefore ran clean to the end and the ingest tail advanced `B`'s
/// frontier slot on a peer's word alone — and that slot is `publish_pending`'s
/// published-to-the-nest high-water.
#[tokio::test]
async fn a_peers_echo_must_not_move_the_successors_publish_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let (stale, successor) =
        fenced_replica(dir.path(), &[(1_000, KIND_MODERATION, MODERATION_KEY)]).await;

    let peer = echo_of(&successor, ACCOUNT_STATE_SCOPE).await;
    let sk = signing_key(A);
    let sched = schedule();
    let tr = trust();
    let plane =
        AccountStatePlane::new_pull_only(&stale, &peer, &sched, &sk, &tr, ACCOUNT_STATE_SCOPE)
            .unwrap();
    let report = plane.walk().await.expect("the stale handle's walk");

    assert_eq!(
        slot_of(&stale, ACCOUNT_STATE_SCOPE, &writer_id(B)).await,
        0,
        "a PEER echoing the successor's row back moved the successor's \
         published-to-the-nest watermark: `publish_pending` now pages past an \
         un-pushed tail the nest never received, and the frontier is MAX-merge, \
         so this never self-heals ({report:?})"
    );
    assert_eq!(
        report.self_echo, 1,
        "the row is this store's own history and must be accounted as the echo \
         it is, not ingested as a foreign writer's ({report:?})"
    );
}

/// **Consequence 2 — the loud one, and the ruling on it.** A second row under a
/// different `(kind, key)` carries local `entry_version` 1 at origin seq 2, so
/// the ingest re-derivation does NOT collide: `ingest_state` bails "journal
/// equivocation" against the store's own history and `apply` propagates it, so
/// the whole peer-leg walk aborts — and keeps aborting until the handle
/// reassembles.
///
/// It needs no remedy of its own. The bail is a correct detector of genuine
/// equivocation; what was wrong is only ever reaching it, and a row correctly
/// recognized as this store's own never does.
#[tokio::test]
async fn a_stale_handles_walk_does_not_abort_on_its_own_stores_history() {
    let dir = tempfile::tempdir().unwrap();
    let (stale, successor) = fenced_replica(
        dir.path(),
        &[
            (1_000, KIND_MODERATION, MODERATION_KEY),
            (2_000, KIND_SEEN_SET, "inbox"),
        ],
    )
    .await;

    let peer = echo_of(&successor, ACCOUNT_STATE_SCOPE).await;
    let sk = signing_key(A);
    let sched = schedule();
    let tr = trust();
    let plane =
        AccountStatePlane::new_pull_only(&stale, &peer, &sched, &sk, &tr, ACCOUNT_STATE_SCOPE)
            .unwrap();
    let report = plane
        .walk()
        .await
        .expect("the stale handle's walk must not abort on its own store's history");

    assert_eq!(
        report.self_echo, 2,
        "both rows are this store's own history ({report:?})"
    );
    assert_eq!(
        slot_of(&stale, ACCOUNT_STATE_SCOPE, &writer_id(B)).await,
        0,
        "the successor's publish watermark moved on a peer's echo ({report:?})"
    );
}

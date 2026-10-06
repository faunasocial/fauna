//! **Frontier compaction on the nest leg** — a class-2 scope whose live rows
//! name more writers than a frontier may carry still converges against a nest
//! that honours the serve-order watermark
//! (`docs/goal/architecture/account-sync-plane.md` § Feeds and cursors →
//! *Compaction is a serve-order watermark*).
//!
//! The honest way to reach `MAX_FRONTIER_WRITERS` is a lifetime of principal
//! successions on one scope, each leaving one retired writer on the feed
//! forever. This file compresses that lifetime into a stub nest serving one
//! sealed row per writer, for more writers than the ceiling allows, and walks
//! the REAL plane against it:
//!
//! - against a **non-honouring nest** — `held_through_seq` ignored, nothing
//!   echoed (a store-served leg answers the same) — the walk keeps its contract: it refuses above the ceiling, and the
//!   next walk from the stored frontier re-grows and refuses again;
//! - against a **watermark-honouring nest** it converges, every request's
//!   frontier stays under the ceiling, and the stored `frontiers` table still
//!   holds every writer — the projection is what a walk *sends*, never what
//!   the store forgets;
//! - on the **peer leg** — a store-served leg, where the ruling defers the
//!   watermark — nothing changes, even when the counterpart echoes one.
//!
//! Every assertion is on latency-independent state: walks are driven
//! explicitly and the stub answers synchronously.

use std::sync::Mutex;

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::ModerationConfig;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass, OP_STATE_PUT};
use fauna_protocol::merge_policy::{KIND_MODERATION, LwwStamp, MODERATION_KEY, kind_keys};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{ByteBuf, decode_strict, encode_canonical};
use fauna_sync_engine::MAX_FRONTIER_WRITERS;
use fauna_sync_engine::account_state_plane::{AccountStatePlane, WalkReport};
use fauna_sync_engine::generation_tip::GenerationTrust;

// ── Fixtures: one account, one replica, many writers ─────────────────────────

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

/// The `i`th device writer on the feed — a retired identity, as far as the
/// walk can tell. Seeds never collide with [`DEVICE`].
fn fleet_writer(i: u32) -> SigningKey {
    let mut seed = [0x5E; 32];
    seed[..4].copy_from_slice(&i.to_be_bytes());
    SigningKey::from_bytes(&seed)
}

/// Rows per stub page — the frame budget's stand-in. Just over half the
/// ceiling, so a feed of `2 * PAGE_ROWS + 1` writers crosses the ceiling on
/// its SECOND page and leaves one row past the page that crosses it.
const PAGE_ROWS: u32 = MAX_FRONTIER_WRITERS as u32 / 2 + 1;

/// One `fauna.state.moderation` entry sealed by `writer` exactly as its own
/// plane would seal it, served at nest-log position `nest_seq`.
fn sealed_row(
    sched: &AccountStateKeySchedule,
    writer: &SigningKey,
    writer_seq: u64,
    nest_seq: i64,
) -> SyncChange {
    let writer_id = writer.verifying_key().to_bytes();
    let plaintext = EntryPlaintext {
        kind: KIND_MODERATION.into(),
        key: MODERATION_KEY.into(),
        merge_meta: Some(ByteBuf::from(
            LwwStamp {
                at_ms: nest_seq,
                writer: writer_id,
            }
            .encode()
            .unwrap(),
        )),
        value: ByteBuf::from(
            encode_canonical(&ModerationConfig {
                muted_keywords: vec![format!("row-{nest_seq}").as_str().into()],
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        ),
        tombstone: false,
    };
    let keys = kind_keys(sched, KIND_MODERATION).expect("moderation is registered");
    let sealed = seal_entry(
        &keys,
        &EntryCoordinates {
            writer_id,
            writer_seq,
            scope: ACCOUNT_STATE_SCOPE,
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
        origin_seq: Some(writer_seq as i64),
        entry: Some(ByteBuf::from(sealed.envelope)),
        ..Default::default()
    }
}

/// One sealed row per writer, writers `0..writers`, at nest seqs `1..=writers`.
fn one_row_per_writer(sched: &AccountStateKeySchedule, writers: u32) -> Vec<SyncChange> {
    (0..writers)
        .map(|i| sealed_row(sched, &fleet_writer(i), 1, i64::from(i) + 1))
        .collect()
}

// ── The stub nest ────────────────────────────────────────────────────────────

/// Which counterpart the stub plays.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Serves {
    /// A nest that does not honour the watermark: `held_through_seq` ignored,
    /// no echo.
    NoEchoNest,
    /// A nest honouring it: the third gate, and an echo on every page.
    HonouringNest,
    /// A store-served leg that echoes anyway — a counterpart whose echo has no
    /// serve order behind it, so a requester must never take it.
    EchoingPeer,
}

/// A canned class-2 feed with the nest's serve semantics — `seq` order, one
/// live row per `(item, writer)`, ordered-prefix paging — and a fixed page
/// size standing in for the frame budget.
struct StubFeed {
    serves: Serves,
    rows: Mutex<Vec<SyncChange>>,
    /// Every request's `(named writers, held_through_seq)`, in order.
    requests: Mutex<Vec<(usize, Option<i64>)>>,
    /// The replica id every reply names (`account-sync-plane.md` § The bind
    /// leg, ruling 2) — `None`: a nest that names none.
    replica: Option<[u8; 16]>,
}

impl StubFeed {
    fn new(serves: Serves, rows: Vec<SyncChange>) -> Self {
        Self {
            serves,
            rows: Mutex::new(rows),
            requests: Mutex::new(Vec::new()),
            replica: None,
        }
    }

    /// The same nest, naming `replica` on every reply.
    fn of_replica(mut self, replica: [u8; 16]) -> Self {
        self.replica = Some(replica);
        self
    }

    fn requests(&self) -> Vec<(usize, Option<i64>)> {
        self.requests.lock().unwrap().clone()
    }

    fn next_seq(&self) -> i64 {
        self.rows.lock().unwrap().last().map_or(1, |r| r.seq + 1)
    }

    /// Land `row` the way the nest does: it supersedes its own writer's
    /// predecessor for the same item, and takes the log's next position.
    fn land(&self, row: SyncChange) {
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|r| !(r.origin_writer == row.origin_writer && r.path_hash == row.path_hash));
        rows.push(row);
    }
}

/// The stub's transport error: a plain fault, never a typed nest refusal
/// (`RpcErrorClass` answers `None`), which is what the nest leg's constructor
/// asks of its requester's error so a real refusal can be classified.
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
        self.serve(kind, payload).await.map_err(StubError)
    }
}

impl StubFeed {
    async fn serve<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(
            kind == "fauna.sync.changes.list",
            "the stub serves the feed only, got {kind}"
        );
        let req: SyncChangesListRequest = decode_strict(&encode_canonical(&payload)?)?;
        anyhow::ensure!(req.item_class.as_deref() == Some(ItemClass::StateEntry.as_wire()));
        anyhow::ensure!(req.scope.as_deref() == Some(ACCOUNT_STATE_SCOPE));
        let frontier = req.frontier.unwrap_or_default();
        self.requests
            .lock()
            .unwrap()
            .push((frontier.len(), req.held_through_seq));

        let held = match self.serves {
            Serves::HonouringNest => req.held_through_seq,
            Serves::NoEchoNest | Serves::EchoingPeer => None,
        };
        let rows = self.rows.lock().unwrap();
        let owed: Vec<&SyncChange> = rows
            .iter()
            .filter(|row| {
                let slot = frontier.get(row.origin_writer.as_deref().unwrap()).copied();
                let gated = row.origin_seq.unwrap() <= slot.unwrap_or(0)
                    || (slot.is_none() && held.is_some_and(|held| row.seq <= held));
                !gated
            })
            .collect();
        let page: Vec<SyncChange> = owed
            .iter()
            .take(PAGE_ROWS as usize)
            .map(|row| (*row).clone())
            .collect();
        let tip = rows.iter().map(|r| r.seq).max();
        let complete_through_seq = match self.serves {
            Serves::NoEchoNest => None,
            Serves::HonouringNest if owed.len() > page.len() => page.last().map(|r| r.seq),
            Serves::HonouringNest | Serves::EchoingPeer => tip,
        };
        let reply = SyncChangesListReply {
            changes: page,
            complete_through_seq,
            replica_id: self.replica.map(|id| ByteBuf::from(id.to_vec())),
            ..Default::default()
        };
        Ok(decode_strict(&encode_canonical(&reply)?)?)
    }
}

// ── The replica ──────────────────────────────────────────────────────────────

async fn replica() -> AccountStore<SqliteBackend> {
    AccountStore::open(
        SqliteBackend::open_in_memory().unwrap(),
        &hex::encode(root().actor_id().0),
        WriterId(SigningKey::from_bytes(&DEVICE).verifying_key().to_bytes()),
    )
    .await
    .unwrap()
}

/// The nest leg's walk — the plane every nudge drives.
async fn nest_walk(
    store: &AccountStore<SqliteBackend>,
    feed: &StubFeed,
) -> anyhow::Result<WalkReport> {
    let (sk, sched, tr) = (SigningKey::from_bytes(&DEVICE), schedule(), trust());
    AccountStatePlane::new(store, feed, &sched, &sk, &tr, ACCOUNT_STATE_SCOPE)?
        .walk()
        .await
}

/// The peer leg's walk — the same plane, constructed pull-only.
async fn peer_walk(
    store: &AccountStore<SqliteBackend>,
    feed: &StubFeed,
) -> anyhow::Result<WalkReport> {
    let (sk, sched, tr) = (SigningKey::from_bytes(&DEVICE), schedule(), trust());
    AccountStatePlane::new_pull_only(store, feed, &sched, &sk, &tr, ACCOUNT_STATE_SCOPE)?
        .walk()
        .await
}

fn writer_id(writer: &SigningKey) -> WriterId {
    WriterId(writer.verifying_key().to_bytes())
}

/// Every relay row's serve coordinate (`RelayRow::feed_seq`), by writer.
async fn coordinates(
    store: &AccountStore<SqliteBackend>,
) -> std::collections::BTreeMap<WriterId, Option<u64>> {
    store
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.writer, row.feed_seq))
        .collect()
}

async fn stored_writers(store: &AccountStore<SqliteBackend>) -> usize {
    store.frontier(ACCOUNT_STATE_SCOPE).await.unwrap().len()
}

async fn slot_of(store: &AccountStore<SqliteBackend>, writer: &SigningKey) -> u64 {
    let writer = WriterId(writer.verifying_key().to_bytes());
    store
        .frontier(ACCOUNT_STATE_SCOPE)
        .await
        .unwrap()
        .into_iter()
        .find(|(w, _)| *w == writer)
        .map_or(0, |(_, seq)| seq)
}

// ── The pin: a non-echoing nest keeps the walk's contract ────────────────────────────

/// **Refuse, re-grow, refuse — unchanged against a nest that never echoes.**
/// Page one fits under the ceiling; page two pushes the frontier past it and
/// the walk refuses — above the checkpoint, but after the page's applies
/// already raised the stored frontier for every row they took. One row sits
/// past that refused page, so the NEXT walk sends the stored frontier (itself
/// past the ceiling), is served that row, re-grows, and refuses again: the
/// state § Feeds and cursors names. The fix never touches it, because a nest
/// that has never echoed is never projected against.
#[tokio::test]
async fn a_non_echoing_nest_keeps_the_refuse_re_grow_refuse_contract() {
    let writers = 2 * PAGE_ROWS + 1;
    let feed = StubFeed::new(Serves::NoEchoNest, one_row_per_writer(&schedule(), writers));
    let store = replica().await;

    let first = nest_walk(&store, &feed)
        .await
        .expect_err("a non-echoing nest cannot page past the ceiling");
    assert!(first.to_string().contains("past the"), "{first:#}");
    assert_eq!(
        stored_writers(&store).await,
        2 * PAGE_ROWS as usize,
        "the refused page's applies were banked; the row past it was never served"
    );

    let second = nest_walk(&store, &feed)
        .await
        .expect_err("the stored frontier re-grows into the same refusal");
    assert!(second.to_string().contains("past the"), "{second:#}");
    assert_eq!(stored_writers(&store).await, writers as usize);

    assert!(
        feed.requests().iter().all(|(_, held)| held.is_none()),
        "a nest that never echoed is never sent a watermark"
    );
    assert_eq!(
        store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
        None,
        "and nothing is banked from a nest that never echoed"
    );
}

/// The no-echo reading, at a size that converges: against a non-echoing nest
/// (or a store-served leg) every walk sends the stored frontier whole, and converges.
#[tokio::test]
async fn a_non_echoing_nest_is_walked_with_the_whole_frontier_and_converges() {
    let sched = schedule();
    let feed = StubFeed::new(Serves::NoEchoNest, one_row_per_writer(&sched, 3));
    let store = replica().await;

    nest_walk(&store, &feed).await.expect("converges");
    feed.land(sealed_row(&sched, &fleet_writer(1), 2, feed.next_seq()));
    let before = feed.requests().len();
    let second = nest_walk(&store, &feed).await.expect("converges again");

    assert_eq!(second.rows, 1);
    assert_eq!(
        feed.requests()[before..],
        [(3, None), (3, None)],
        "the whole stored frontier on both pages, and never a watermark"
    );
}

// ── The fix: a watermark-honouring nest ──────────────────────────────────────

/// **The definition of success.** The same over-ceiling feed converges in ONE
/// walk: each page's echo lets the walk stop naming the writers it covers, so
/// no request frontier ever approaches the ceiling — and the stored
/// `frontiers` table still holds every writer. A second walk, after a fresh
/// writer and an old writer's newer row land, resumes from the banked
/// watermark naming nobody, and takes exactly the two new rows.
#[tokio::test]
async fn a_watermark_honouring_nest_converges_a_frontier_past_the_ceiling() {
    let sched = schedule();
    let writers = 2 * PAGE_ROWS + 1;
    let feed = StubFeed::new(Serves::HonouringNest, one_row_per_writer(&sched, writers));
    let store = replica().await;

    let first = nest_walk(&store, &feed)
        .await
        .expect("the watermark pages what the whole frontier could not");
    assert_eq!(first.rows, writers as usize);
    assert_eq!(first.unopened, 0);
    assert_eq!(
        stored_writers(&store).await,
        writers as usize,
        "the stored frontier is complete — a projection is what a request sends, never \
         what the store forgets"
    );
    assert_eq!(
        store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
        Some(u64::from(writers)),
        "the converged walk banked the whole log"
    );

    feed.land(sealed_row(&sched, &fleet_writer(7), 2, feed.next_seq()));
    feed.land(sealed_row(
        &sched,
        &fleet_writer(writers),
        1,
        feed.next_seq(),
    ));
    let before = feed.requests().len();
    let second = nest_walk(&store, &feed)
        .await
        .expect("the second walk resumes from the banked watermark");

    assert_eq!(
        second.rows, 2,
        "exactly the two rows that landed between walks"
    );
    assert!(
        feed.requests()[before..]
            .iter()
            .all(|(named, held)| *named == 0 && held.is_some()),
        "every writer is covered by the watermark, so none is named: {:?}",
        &feed.requests()[before..]
    );
    assert_eq!(stored_writers(&store).await, writers as usize + 1);
    assert_eq!(slot_of(&store, &fleet_writer(7)).await, 2);
    assert_eq!(
        store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
        Some(u64::from(writers) + 2),
        "banked through the new tip"
    );
    assert!(
        feed.requests()
            .iter()
            .all(|(named, _)| *named <= MAX_FRONTIER_WRITERS),
        "no request frontier ever crossed the ceiling"
    );
}

// ── The fix: a rolled-back nest is un-banked ────

/// **A nest that stops honouring a banked watermark is un-banked after one
/// full re-serve.** Bank a watermark against a honouring nest, then roll it
/// back — same rows, `held_through_seq` ignored, nothing echoed, exactly what
/// a nest that does not honour it answers. The first post-rollback walk still opens narrow
/// (the store had no way to know before asking) and so pays one full
/// re-serve, but the reply's missing echo falls the in-walk cursor back to
/// the whole frontier for the rest of that walk AND clears the persisted
/// watermark — so the SECOND post-rollback walk reopens with the whole
/// stored frontier, exactly as a non-echoing nest is always walked with, and
/// takes zero rows.
#[tokio::test]
async fn a_rolled_back_nest_is_unbanked_after_one_full_re_serve() {
    let sched = schedule();
    let writers = 3;
    let store = replica().await;

    let honouring = StubFeed::new(Serves::HonouringNest, one_row_per_writer(&sched, writers));
    nest_walk(&store, &honouring)
        .await
        .expect("banks a watermark");
    assert_eq!(
        store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
        Some(u64::from(writers))
    );

    let rolled_back = StubFeed::new(Serves::NoEchoNest, one_row_per_writer(&sched, writers));

    let first = nest_walk(&store, &rolled_back)
        .await
        .expect("the first post-rollback walk still converges");
    assert_eq!(
        first.rows, writers as usize,
        "the stale bank starts the walk narrow, so the rolled-back nest re-serves everyone once"
    );
    assert_eq!(
        store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
        None,
        "the unechoed bank is cleared, not left stale for the next walk"
    );
    assert_eq!(
        rolled_back.requests(),
        [(0, Some(i64::from(writers))), (writers as usize, None)],
        "the opening request is still narrow (the stale bank), and the request after the \
         first non-echoed reply names the whole frontier with no watermark: {:?}",
        rolled_back.requests()
    );

    let before = rolled_back.requests().len();
    let second = nest_walk(&store, &rolled_back)
        .await
        .expect("the second post-rollback walk converges too");
    assert_eq!(
        second.rows, 0,
        "no re-serve: the second walk reopens with the whole stored frontier, matching what \
         every writer already holds"
    );
    assert_eq!(
        rolled_back.requests()[before..],
        [(writers as usize, None)],
        "one request, the whole frontier, never a watermark"
    );
}

/// **A banked watermark never claims a row the store did not take.** A row
/// this replica cannot open is seen but not accounted — its writer's stored
/// slot stays put — so every later walk keeps naming that writer at its
/// stored slot, and the nest keeps re-presenting the row exactly as a
/// whole-frontier walk would; every writer whose rows were all accounted is
/// covered by the watermark instead.
#[tokio::test]
async fn an_unopened_row_keeps_its_writer_named_across_banked_watermarks() {
    let sched = schedule();
    let another_account = AccountStateKeySchedule::derive(&BackupKey::derive(&[8u8; 32]));
    let mut rows = one_row_per_writer(&sched, 3);
    rows.push(sealed_row(&another_account, &fleet_writer(3), 1, 4));
    let feed = StubFeed::new(Serves::HonouringNest, rows);
    let store = replica().await;

    let first = nest_walk(&store, &feed).await.expect("converges");
    assert_eq!(first.unopened, 1);

    let before = feed.requests().len();
    let second = nest_walk(&store, &feed).await.expect("converges again");
    assert_eq!(
        second.unopened, 1,
        "the row the store could not take is re-presented, watermark or not"
    );
    assert_eq!(
        feed.requests()[before],
        (1, Some(4)),
        "the watermark covers the three accounted writers; the unaccounted one stays named"
    );
}

// ── The deferred legs: nothing changes on the peer leg ───────────────────────

/// **A store-served leg never takes or sends a watermark** — correction to a
/// shape a build could reach for: the per-scope watermark lives in the same
/// store the peer leg walks, and a peer's echo has no serve order behind it.
/// After a nest walk has banked a watermark, a pull-only walk against a peer
/// that echoes anyway still sends the whole stored frontier on every page.
#[tokio::test]
async fn the_peer_leg_sends_the_whole_frontier_even_with_a_watermark_banked() {
    let sched = schedule();
    let nest = StubFeed::new(Serves::HonouringNest, one_row_per_writer(&sched, 3));
    let store = replica().await;
    nest_walk(&store, &nest)
        .await
        .expect("the nest leg banks a watermark");

    let mut peer_rows = one_row_per_writer(&sched, 3);
    peer_rows.push(sealed_row(&sched, &fleet_writer(3), 1, 99));
    let peer = StubFeed::new(Serves::EchoingPeer, peer_rows);
    let report = peer_walk(&store, &peer)
        .await
        .expect("the peer walk converges");

    assert_eq!(report.rows, 1);
    assert_eq!(
        peer.requests(),
        [(3, None), (4, None)],
        "the whole frontier, grown by the page, never projected and never a watermark"
    );
    assert_eq!(
        store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
        Some(3),
        "the peer's echo was never banked over the nest's"
    );
}

// ── The bind leg: a watermark is valid only for its replica ─────────────────

/// **A banked watermark never hides a row on another replica**
/// (`account-sync-plane.md` § The bind leg, ruling 2). Nest A — replica X —
/// serves three writers through seq 3, and the walk banks 3 under X. The same
/// store then walks a second nest B holding a row this replica has never seen,
/// by an unnamed writer, BELOW the bank (seq 1), and one above it. B's echo is
/// its own tip — above the bank — so before the fix the walk took it as
/// confirmation and the seq-1 row was held back for good. Now the bank is void
/// when B names another replica, when it names none, and when a reply from
/// the SAME replica id echoes below it (B holding only the seq-1 row): each
/// walk re-sends the whole frontier and the hidden row is walked.
///
/// **Every serve coordinate is void with the bank**, too: a relay row's
/// `feed_seq` is a position in A's log, so after the walk of B no row keeps
/// A's numbering — and the void lands BEFORE B's first page is recorded, so
/// the coordinate B's opening page stamps (the seq-5 row, served above the
/// stale bank on the very request that finds it void) survives.
#[tokio::test]
async fn a_watermark_banked_on_one_replica_never_hides_a_row_on_another() {
    const X: [u8; 16] = [0xAA; 16];
    const Y: [u8; 16] = [0xBB; 16];
    let sched = schedule();
    let hidden = fleet_writer(9);
    for (second, why) in [
        (
            StubFeed::new(
                Serves::HonouringNest,
                vec![
                    sealed_row(&sched, &hidden, 1, 1),
                    sealed_row(&sched, &fleet_writer(10), 1, 5),
                ],
            )
            .of_replica(Y),
            "another replica id",
        ),
        (
            StubFeed::new(
                Serves::HonouringNest,
                vec![
                    sealed_row(&sched, &hidden, 1, 1),
                    sealed_row(&sched, &fleet_writer(10), 1, 5),
                ],
            ),
            "a nest naming no replica",
        ),
        (
            StubFeed::new(
                Serves::HonouringNest,
                vec![sealed_row(&sched, &hidden, 1, 1)],
            )
            .of_replica(X),
            "an echo below the bank",
        ),
    ] {
        let first =
            StubFeed::new(Serves::HonouringNest, one_row_per_writer(&sched, 3)).of_replica(X);
        let store = replica().await;
        nest_walk(&store, &first).await.expect("nest A converges");
        assert_eq!(
            store.nest_watermark(ACCOUNT_STATE_SCOPE).await.unwrap(),
            Some(3)
        );
        assert_eq!(
            store
                .nest_watermark_replica(ACCOUNT_STATE_SCOPE)
                .await
                .unwrap()
                .as_deref(),
            Some(X.as_slice()),
            "the bank is keyed by the replica that echoed it"
        );

        let mut stamped: Vec<Option<u64>> = coordinates(&store).await.into_values().collect();
        stamped.sort_unstable();
        assert_eq!(
            stamped,
            vec![Some(1), Some(2), Some(3)],
            "nest A's walk stamped its own coordinates"
        );

        nest_walk(&store, &second).await.expect("nest B converges");
        assert_eq!(
            slot_of(&store, &hidden).await,
            1,
            "{why}: the row below the stale bank is walked"
        );
        let coordinates = coordinates(&store).await;
        for i in 0..3 {
            assert_eq!(
                coordinates[&writer_id(&fleet_writer(i))],
                None,
                "{why}: nest A's coordinate is void with its bank"
            );
        }
        assert_eq!(coordinates[&writer_id(&hidden)], Some(1), "{why}");
        if let Some(stamped) = coordinates.get(&writer_id(&fleet_writer(10))) {
            assert_eq!(
                *stamped,
                Some(5),
                "{why}: the stamp of B's opening page survives the void"
            );
        }
        assert_eq!(
            second.requests().first().copied(),
            Some((0, Some(3))),
            "{why}: the walk opened on the inherited bank"
        );
        assert!(
            second
                .requests()
                .iter()
                .skip(1)
                .any(|(named, held)| *named > 0 && held.is_none()),
            "{why}: the void bank fell back to the whole frontier: {:?}",
            second.requests()
        );
    }
}

//! A fresh replica bootstrapping one content scope off a real nest — the bulk
//! class-1 half of the generalized account-data plane (W2.2 (account-data-plane.md § Workstreams),
//! `docs/goal/architecture/account-data-plane.md` § Store logical schema →
//! *How nest CARv2 segments map onto the local log (the bootstrap contract)*).
//!
//! The target flow this file drives end to end: an empty store enumerates the
//! scope's segments over the real `fauna.segments.list` control plane → pulls
//! each `(dat, meta)` pair over the real HTTP byte plane → `adopt_segment`
//! verifies the container and files it verbatim → the records come back out
//! through the store's **ordinary** read path, which knows nothing about
//! segments.
//!
//! Tier: tier_3. Both planes are the nest's real ones — the WS-RPC handler
//! table dispatched in-process (as `conformance_account_state_walk.rs` does),
//! and a real `axum` server on a real socket, fetched by the shipped
//! `SyncClient` over real HTTP with a real bearer. The only fixture is the
//! post records themselves, written by the nest's own append path.
//!
//! **Why `post` and not `mail`.** Adoption re-hashes every block against the
//! CID it is filed under (`fauna_account_store::segments::admit`). Post files
//! records under `Cid::of_dag_cbor(body)` (`segments::post::append_body`), so
//! its segments satisfy that by construction. Mail files them under a
//! *sequenced* record id (`Cid::from_digest_dag_cbor(rid)`), which is why the
//! kind that reaches this plane first is post, not the kind the backup arm
//! uses.
//!
//! Every assertion is on latency-independent state (e2e convention 14): the
//! bootstrap is driven explicitly, so there is nothing to wait for and no
//! sleep in the file.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;

use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_cbor::Cid;
use fauna_core::identity::ActorKeypair;
use fauna_mail::segments::{MAIL_FLOOR_FORMAT_VERSION, MailFloorMetadata};
use fauna_mls::wrapped_blob::SealedRecordBytes;
use fauna_nest::{
    db::CacheDb, routes::AppState, rpc_router::RpcRouter, segments::register_segments_handlers,
    token_store::TokenStore,
};
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::{RpcError, RpcRequester, encode_canonical};
use fauna_sync_engine::bootstrap_source::{NestBootstrapSource, ScopeBinding};
use fauna_sync_engine::content_scope_plane::ContentScopePlane;
use fauna_sync_engine::nest_client::{SegmentMetaUnavailable, SyncClient};

/// The owner. Its keypair's actor id is what both planes authorize against, so
/// it is derived once from a seed rather than picked as a bare 32 bytes.
const OWNER_SEED: [u8; 32] = [0xC3; 32];
const DEVICE_ID: [u8; 32] = [0x0D; 32];

/// The canonical scope string for the owner's posts —
/// `content:post:<owner-hex>` per the ratified encoding
/// (`account-sync-plane.md` § Feeds and cursors → *The scope string*),
/// built through the constructor so this fixture cannot drift from it.
/// Still opaque to the source: the `(kind, scope_id)` mapping travels in
/// the [`ScopeBinding`] (`bootstrap_source.rs` module docs).
fn post_scope() -> String {
    fauna_protocol::scope::ContentScope::new("post", owner())
        .unwrap()
        .to_string()
}

fn owner() -> [u8; 32] {
    ActorKeypair::from_secret(OWNER_SEED).actor_id().0
}

// ── The two planes, both real ────────────────────────────────────────────────

/// An [`RpcRequester`] that dispatches straight into the nest's own handler
/// table as the owner. Not a mock of the nest — it *is* the nest's request
/// path, minus the WebSocket frame.
struct RouterRequester {
    router: RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
}

#[derive(Debug)]
struct Refused(RpcError);

impl std::fmt::Display for Refused {
    /// Code **and detail**. The `message` is a localization key, so a refusal
    /// rendered without its `details` says only "invalid_request" — which is
    /// how a failing test here would report "the nest refused" while hiding
    /// *why* (e2e convention 6: a failure must diagnose itself).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.code)?;
        if let Some(details) = &self.0.details {
            write!(f, ": {details:?}")?;
        }
        Ok(())
    }
}

impl RpcRequester for RouterRequester {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        common::seed_dispatch_actor(&self.state.db, &self.actor).await;
        let meta = self.router.kind_meta(kind).expect("kind registered");
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        let reply = (meta.handler)(Arc::clone(&self.state), self.actor, bytes)
            .await
            .map_err(Refused)?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }
}

/// A real nest holding `bodies` as the owner's post records: real segment
/// files on disk, the real handler table, and the real HTTP router on a real
/// socket. Returns the two planes plus the CIDs the nest filed the records
/// under.
async fn nest_with_posts(bodies: &[&[u8]]) -> (RouterRequester, SyncClient, Vec<Cid>) {
    let owner = owner();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&owner, "free", "test").await.unwrap();

    // Real on-disk segment area. Leaked deliberately: the axum server outlives
    // the test body and would otherwise read a deleted directory.
    let dir = tempfile::tempdir().unwrap();
    let seg_root = dir.path().to_path_buf();
    std::mem::forget(dir);

    let token_store = Arc::new(TokenStore::new());
    let token = token_store
        .insert(ActorKeypair::from_secret(OWNER_SEED).actor_id(), 3600)
        .await;

    let state = Arc::new(AppState {
        post_segments: Arc::new(fauna_segment_store::SegmentManager::new(seg_root, "post")),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..AppState::for_test(db)
    });

    // The records themselves, written by the nest's own append path — a
    // hand-rolled CARv2 would only prove this reader self-consistent.
    let mut cids = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let outcome = fauna_nest::segments::post::append_body(
            &state.post_segments,
            &state.db,
            &owner,
            body,
            1_715_000_000 + i as i64,
        )
        .await
        .expect("append post body");
        cids.push(outcome.record_cid);
    }

    let mut b = RpcRouter::builder();
    register_segments_handlers(&mut b);
    // The feed door the walk half rides — the same handler table, not a second
    // one: bulk adoption and the walk are two arms of one contract.
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    let rpc = RouterRequester {
        router: b.build(),
        state: Arc::clone(&state),
        actor: owner,
    };

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer(token));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        format!("http://{addr}"),
        ActorKeypair::from_secret(OWNER_SEED),
        bearer,
        reqwest::Client::new(),
    ));

    (rpc, SyncClient::new(auth, &DEVICE_ID), cids)
}

/// A fresh, empty replica of the owner's account.
///
/// Directory-backed, not in-memory: adoption files whole segment pairs into
/// the store's **segment area**, which only a store on a directory has.
async fn empty_replica() -> AccountStore<SqliteBackend> {
    replica_for(owner()).await
}

/// A fresh, empty replica of `actor`'s account — the conv tests walk as the
/// channel *member* and the probing *stranger*, whose replicas are their own
/// accounts' (a conv scope's member replicas belong to different accounts —
/// charter § Feeds and cursors → *The scope string* ruling 1).
async fn replica_for(actor: [u8; 32]) -> AccountStore<SqliteBackend> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    std::mem::forget(dir); // outlive the store; never deleted under test
    AccountStore::open(
        SqliteBackend::open(path).unwrap(),
        &hex::encode(actor),
        WriterId([0x0B; 32]),
    )
    .await
    .unwrap()
}

fn binding() -> ScopeBinding {
    ScopeBinding {
        scope: post_scope(),
        kinds: vec!["post".into()],
        actor_hex: hex::encode(owner()),
    }
}

// ── The tests ────────────────────────────────────────────────────────────────

/// The whole target flow in one test: an empty replica pulls a real nest's
/// segments and then serves their records back through the ordinary read path.
#[tokio::test]
async fn a_fresh_replica_bootstraps_a_scope_off_a_real_nest() {
    let bodies: [&[u8]; 3] = [b"first post", b"second post", b"third post"];
    let (rpc, bytes, cids) = nest_with_posts(&bodies).await;
    let store = empty_replica().await;
    let source = NestBootstrapSource::new(&rpc, &bytes, binding());

    let report = store
        .bootstrap_scope_segments(&post_scope(), &source)
        .await
        .expect("bootstrap");

    assert_eq!(report.adopted, 1, "the nest holds one live post segment");
    assert_eq!(report.records_indexed, bodies.len());
    assert_eq!(report.already_held, 0);
    assert_eq!(report.skipped_dehydrating, 0);

    // The half that matters: the records come back out through the store's
    // *ordinary* read path. Nothing above the store observes that these bytes
    // live inside an adopted segment rather than as loose blocks.
    for (cid, body) in cids.iter().zip(bodies.iter()) {
        let got = store
            .block(cid)
            .await
            .expect("block read")
            .expect("an adopted record is readable");
        assert_eq!(&got, body, "the adopted block is the byte-identical record");
    }

    let indexed = store
        .records_in_scope(&post_scope(), None, 100)
        .await
        .expect("records_in_scope");
    assert_eq!(
        indexed.len(),
        bodies.len(),
        "every adopted record has an index row — one without is reachable through no store API"
    );
}

/// Re-running a bootstrap adopts nothing twice. The nest is still serving the
/// same segment; the store recognizes it as held, and no record is indexed a
/// second time.
#[tokio::test]
async fn re_bootstrapping_the_same_scope_adopts_nothing_twice() {
    let bodies: [&[u8]; 2] = [b"alpha", b"beta"];
    let (rpc, bytes, _) = nest_with_posts(&bodies).await;
    let store = empty_replica().await;
    let source = NestBootstrapSource::new(&rpc, &bytes, binding());

    let first = store
        .bootstrap_scope_segments(&post_scope(), &source)
        .await
        .unwrap();
    let second = store
        .bootstrap_scope_segments(&post_scope(), &source)
        .await
        .unwrap();

    assert_eq!((first.adopted, first.already_held), (1, 0));
    assert_eq!(
        (second.adopted, second.already_held, second.records_indexed),
        (0, 1, 0),
        "the second pass recognizes the held segment and writes nothing"
    );
}

/// A replica whose policy dehydrates the kind pulls **no bytes at all** — the
/// skip happens *before* the fetch, not after.
///
/// Asserted on the source's own fetch count rather than on the store's end
/// state, because the end state cannot tell the two apart: a policy that skips
/// after paying for the download leaves exactly the same empty index as one
/// that never asks. The counter wraps the real source and delegates to it —
/// the nest, both planes and the adoption path are unchanged.
#[tokio::test]
async fn a_dehydrating_replica_never_fetches_the_bytes() {
    let bodies: [&[u8]; 1] = [b"never pulled"];
    let (rpc, bytes, _) = nest_with_posts(&bodies).await;
    let store = empty_replica().await;
    store
        .set_hydration_policy(&fauna_account_store::types::HydrationPolicy {
            default: fauna_account_store::types::Hydration::OnDemand,
            per_kind: Default::default(),
        })
        .await
        .unwrap();

    let source = CountingSource {
        inner: NestBootstrapSource::new(&rpc, &bytes, binding()),
        fetches: AtomicUsize::new(0),
    };
    let report = store
        .bootstrap_scope_segments(&post_scope(), &source)
        .await
        .unwrap();

    assert_eq!(
        (report.adopted, report.skipped_dehydrating),
        (0, 1),
        "the offer is enumerated, then passed over on the policy"
    );
    assert_eq!(
        source.fetches.load(Ordering::SeqCst),
        0,
        "a dehydrated kind is never fetched — the download is not paid for and discarded"
    );
    assert!(
        store
            .records_in_scope(&post_scope(), None, 100)
            .await
            .unwrap()
            .is_empty(),
        "a dehydrating replica's index comes from the feed walk, not from segments"
    );
}

// ── The walk half: what the nest did after the segments were pulled ──────────

/// Append one more post to the running nest, as its own production path does.
async fn append_post(rpc: &RouterRequester, body: &[u8]) -> Cid {
    fauna_nest::segments::post::append_body(
        &rpc.state.post_segments,
        &rpc.state.db,
        &owner(),
        body,
        1_715_900_000,
    )
    .await
    .expect("append post body")
    .record_cid
}

/// Delete one post on the nest through the segment-record tombstone step of
/// the real delete pipeline (`routes.rs::delete_post_core` step 2 — the step
/// that makes the record gone to the segment plane; steps 1 and 3 are the
/// nest's own projection and counter bookkeeping, which no replica reads).
async fn tombstone_post(rpc: &RouterRequester, cid: &Cid) {
    let (scope, seg_id) =
        fauna_nest::segments::post::lookup_scope_by_post_id(&rpc.state.db, &cid.digest())
            .await
            .expect("lookup")
            .expect("the record is on this nest");
    let n = fauna_nest::segments::post::tombstone_by_cid(&rpc.state.db, &scope, seg_id, cid)
        .await
        .expect("tombstone");
    assert_eq!(n, 1, "the delete tombstoned exactly its own record");
}

fn plane<'a>(
    store: &'a AccountStore<SqliteBackend>,
    rpc: &'a RouterRequester,
) -> ContentScopePlane<'a, SqliteBackend, RouterRequester> {
    ContentScopePlane::new(
        store,
        rpc,
        fauna_protocol::scope::ContentScope::new("post", owner()).unwrap(),
    )
}

async fn indexed_cids(store: &AccountStore<SqliteBackend>) -> Vec<Cid> {
    store
        .records_in_scope(&post_scope(), None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.cid)
        .collect()
}

/// **The bootstrap contract, both halves.** A replica adopts the scope's
/// segments; the nest then deletes one record and adds another; the replica's
/// feed walk converges on the nest's current state.
///
/// This is the sentence the charter's § Store logical schema writes and W2.2
/// built only the first half of: adopt verbatim, rebuild the index from
/// `record_order`, "then walk the scope's feed from a zero frontier to
/// materialize state entries and tombstones".
///
/// Nothing here waits on a clock (e2e convention 14): every step is driven
/// explicitly and every assertion is on state.
#[tokio::test]
async fn a_bootstrapped_scope_walks_its_feed_onto_the_nests_current_state() {
    let bodies: [&[u8]; 3] = [b"kept one", b"deleted one", b"kept two"];
    let (rpc, bytes, cids) = nest_with_posts(&bodies).await;
    let store = empty_replica().await;
    let source = NestBootstrapSource::new(&rpc, &bytes, binding());

    store
        .bootstrap_scope_segments(&post_scope(), &source)
        .await
        .expect("bootstrap");
    assert_eq!(indexed_cids(&store).await.len(), 3, "the bulk half landed");

    // The nest moves on: one record deleted, one added. Neither reaches the
    // replica through segments — the pulled files are a snapshot of the past.
    tombstone_post(&rpc, &cids[1]).await;
    let late = append_post(&rpc, b"posted after the pull").await;

    let report = plane(&store, &rpc).walk().await.expect("walk");
    assert_eq!(report.tombstoned, 1, "the delete arrived as a tombstone");
    assert!(
        report.indexed >= 1,
        "the late record arrived as record-added (with the re-presented survivors)"
    );

    let mut have = indexed_cids(&store).await;
    have.sort_by_key(|c| *c.as_bytes());
    let mut want = vec![cids[0], cids[2], late];
    want.sort_by_key(|c| *c.as_bytes());
    assert_eq!(
        have, want,
        "the replica's index is the nest's live set: survivors + the late record, \
         and the deleted record is gone"
    );

    // The late record's bytes have not arrived at all — the walk carries
    // identity, never payload ("the always-present layer is the index, not the
    // blocks"), and fetching them is hydration's separate decision.
    assert!(store.block(&late).await.unwrap().is_none());
    assert!(
        store.block(&cids[0]).await.unwrap().is_some(),
        "a survivor's adopted bytes are untouched"
    );
    // The deleted record's bytes, by contrast, are still on disk — it was
    // segment-resident, and a CARv2 file is immutable (§ Store logical schema:
    // reclaiming one is segment eviction, a whole-file operation). That is not
    // a leak of deleted content: with the index row gone the record is
    // reachable through no store API, which is the same order the nest's own
    // delete uses — projection first, segment body left to compaction.
    assert!(store.record(&cids[1]).await.unwrap().is_none());
    assert!(store.block(&cids[1]).await.unwrap().is_some());
}

/// **A delete reaches a replica that had already walked past the record.**
///
/// This is the case the tombstone's *coordinate* exists for, and the only one
/// that can detect its absence: at a zero cursor the feed returns the scope's
/// whole state, so a tombstone arrives whether or not the delete moved the
/// record's coordinate. Past a cursor it does not — a delete that left the
/// coordinate where it was would sit below the replica's frontier forever, and
/// the replica would go on serving a post its author deleted.
///
/// Red-verified by reverting the bump (`records_db::mark_tombstoned`'s
/// `changed_seq = ?5` → `changed_seq = changed_seq`): this test fails, and —
/// the reason it is worth its own test — every other test in this file still
/// passes.
#[tokio::test]
async fn a_delete_after_the_replica_caught_up_still_reaches_it() {
    let (rpc, _bytes, cids) = nest_with_posts(&[b"stays", b"goes"]).await;
    let store = empty_replica().await;

    // Caught up: the replica's frontier now sits past both records.
    let first = plane(&store, &rpc).walk().await.unwrap();
    assert_eq!(first.indexed, 2);
    assert_eq!(indexed_cids(&store).await.len(), 2);

    tombstone_post(&rpc, &cids[1]).await;

    let second = plane(&store, &rpc).walk().await.unwrap();
    assert_eq!(
        (second.rows, second.tombstoned, second.indexed),
        (1, 1, 0),
        "exactly the delete came through — not a rescan of the whole scope"
    );
    assert_eq!(
        indexed_cids(&store).await,
        vec![cids[0]],
        "the deleted record is gone from the index; the survivor is untouched"
    );
}

/// The walk is a cursor, not a rescan: with nothing changed on the nest, a
/// second walk fetches an empty page and applies nothing.
#[tokio::test]
async fn a_second_walk_over_an_unchanged_scope_applies_nothing() {
    let (rpc, _bytes, _cids) = nest_with_posts(&[b"one", b"two"]).await;
    let store = empty_replica().await;

    let first = plane(&store, &rpc).walk().await.unwrap();
    assert_eq!((first.rows, first.indexed), (2, 2));

    let second = plane(&store, &rpc).walk().await.unwrap();
    assert_eq!(
        second,
        fauna_sync_engine::content_scope_plane::ContentWalkReport {
            // The report names its subject unconditionally (the pump's
            // per-pass vector reads it off the report); every counter zero is
            // what "applied nothing" means.
            scope: post_scope(),
            ..Default::default()
        },
        "the frontier held: nothing fetched, nothing applied"
    );
}

/// A replica that never adopted a segment still materializes the scope's index
/// from the walk alone — the dehydrating replica's path, and the reason the
/// feed carries every live record rather than only the deltas since some
/// bootstrap.
#[tokio::test]
async fn a_replica_with_no_segments_materializes_the_index_from_the_walk_alone() {
    let (rpc, _bytes, cids) = nest_with_posts(&[b"alpha", b"beta"]).await;
    let store = empty_replica().await;

    let report = plane(&store, &rpc).walk().await.unwrap();
    assert_eq!(report.indexed, 2);

    let mut have = indexed_cids(&store).await;
    have.sort_by_key(|c| *c.as_bytes());
    let mut want = cids.clone();
    want.sort_by_key(|c| *c.as_bytes());
    assert_eq!(have, want);
    assert!(
        store.block(&cids[0]).await.unwrap().is_none(),
        "index without bytes is the whole point — hydration is a separate decision"
    );
}

/// **A refusal is not an empty page** (charter § Feeds and cursors → *The scope
/// string* ruling 4). A kind this nest does not serve on the feed comes back as
/// an error the caller must handle, never as a zero-row success a replica would
/// record as converged-empty.
///
/// The kind is fictional-but-well-formed on purpose: the parser checks shape,
/// not kind knowledge (charter ruling 3), so this is exactly the version-skew
/// case ruling 4 describes — a newer kind's scope in an older nest's hands.
/// (`conv` used to play this role here; it is a served kind now.)
#[tokio::test]
async fn a_kind_this_nest_does_not_serve_is_a_refusal_not_an_empty_walk() {
    let (rpc, _bytes, _) = nest_with_posts(&[b"a post"]).await;
    let store = empty_replica().await;
    let unserved = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("beacon", owner()).unwrap(),
    );

    let err = unserved
        .walk()
        .await
        .expect_err("an unserved kind is refused, not answered empty");
    let s = format!("{err:#}");
    // The ratified refusal shape: coded `invalid_request` (charter ruling 4
    // names exactly this for the class-2 doors "and the same on the generalized
    // feed when content scopes join it"), naming the kind it will not serve.
    assert!(
        s.contains("fauna.sync.invalid_request") && s.contains("does not serve kind"),
        "want the door's own refusal, got: {s}"
    );
}

/// Another actor's content scope is refused. The scope string is absolute —
/// the same string names the same scope in every store — so the door cannot
/// lean on "it's the caller's own" being implied by the name; it checks.
#[tokio::test]
async fn another_actors_content_scope_is_refused() {
    let (rpc, _bytes, _) = nest_with_posts(&[b"a post"]).await;
    let store = empty_replica().await;
    let stranger = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("post", [0x99; 32]).unwrap(),
    );

    let err = stranger.walk().await.expect_err("not the caller's scope");
    let s = format!("{err:#}");
    assert!(
        s.contains("fauna.sync.forbidden") && s.contains("belongs to"),
        "want the ownership refusal, got: {s}"
    );
}

/// The real source, plus a count of the byte-plane fetches it was asked for.
/// Not a mock: every call lands on the wrapped production source.
struct CountingSource<'a> {
    inner: NestBootstrapSource<'a, RouterRequester>,
    fetches: AtomicUsize,
}

impl fauna_account_store::store::BootstrapSource for CountingSource<'_> {
    async fn list_segments(
        &self,
        scope: &str,
    ) -> anyhow::Result<Vec<fauna_account_store::store::SegmentOffer>> {
        self.inner.list_segments(scope).await
    }

    async fn fetch_segment(
        &self,
        scope: &str,
        offer: &fauna_account_store::store::SegmentOffer,
        max_bytes: u64,
        into: &mut impl fauna_account_store::segments::SegmentSink,
    ) -> anyhow::Result<()> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.inner
            .fetch_segment(scope, offer, max_bytes, into)
            .await
    }
}

/// A source built for one scope refuses another. The `(kind, scope_id)`
/// mapping is knowledge the caller supplies; a source that guessed would file
/// one scope's records under its neighbour's name.
#[tokio::test]
async fn a_source_refuses_a_scope_it_was_not_built_for() {
    let (rpc, bytes, _) = nest_with_posts(&[b"only post"]).await;
    let store = empty_replica().await;
    let source = NestBootstrapSource::new(&rpc, &bytes, binding());

    let err = store
        .bootstrap_scope_segments("content:mail", &source)
        .await
        .expect_err("a mismatched scope is refused");
    assert!(
        err.to_string().contains("this source serves scope"),
        "unexpected error: {err}"
    );
}

/// The sidecar door's compat shape: a kind this nest does not serve answers
/// 404, and the client renders that as the typed availability statement — not
/// as "the segment is corrupt" and not as a bare HTTP status.
#[tokio::test]
async fn an_unserved_kind_surfaces_as_the_typed_sidecar_refusal() {
    let (_rpc, bytes, _) = nest_with_posts(&[b"a post"]).await;

    let err = bytes
        .get_segment_meta_bytes("conv", &hex::encode(owner()), 0, u64::MAX)
        .await
        .expect_err("this nest serves no conv segments");
    assert!(
        err.downcast_ref::<SegmentMetaUnavailable>().is_some(),
        "want the typed refusal, got: {err:?}"
    );
}

// ── mail: the relay-ack purge now carries feed coordinates (row 4) ──────────

/// The canonical scope string for the owner's mail —
/// `content:mail:<owner-hex>`, sibling of [`post_scope`].
fn mail_scope() -> String {
    fauna_protocol::scope::ContentScope::new("mail", owner())
        .unwrap()
        .to_string()
}

/// Minimal `MailFloorMetadata` with only `received_at` meaningful. Mirrors
/// `conformance_cross_nest_mail_relay.rs::floor` (duplicated rather than
/// shared across integration-test binaries: each compiles standalone, and the
/// in-crate `segments::test_helpers::floor` is `pub(crate)`, unreachable from
/// here). `append_record` overwrites `seq`.
fn mail_floor(received_at: i64) -> MailFloorMetadata {
    MailFloorMetadata {
        format_version: MAIL_FLOOR_FORMAT_VERSION,
        received_at,
        timestamp: received_at / 1000,
        ciphertext_size: 0,
        sender_domain: "example.com".to_string(),
        spam_disposition: "accept".to_string(),
        is_own_submission: false,
        spf: "pass".into(),
        dkim: "pass".into(),
        dmarc: "pass".into(),
        dmarc_policy: "reject".into(),
        arc: "pass".into(),
        spam_score: 0,
        seq: 0,
        continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        // Struct-update so the next additive floor field doesn't break this
        // fixture (the reason MailFloorMetadata carries a Default at all).
        ..Default::default()
    }
}

/// A real nest holding `bodies` as the owner's mail records, on its real
/// `__mail` segment area. No HTTP byte plane and no `post_segments`: unlike
/// [`nest_with_posts`], this fixture drives only the feed walk (RPC), never
/// bulk segment adoption — mail's sequenced record id cannot satisfy
/// adoption's re-hash check (this file's module docs), so a mail bootstrap
/// source has no shape to test here. Returns the RPC plane plus the CIDs the
/// nest filed the records under, oldest (lowest `seq`) first.
async fn nest_with_mail(bodies: &[&[u8]]) -> (RouterRequester, Vec<Cid>) {
    let owner = owner();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&owner, "free", "test").await.unwrap();

    let state = Arc::new(AppState::for_test(db));

    let mut cids = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let outcome = fauna_nest::segments::mail::append_record(
            &state.mail_segments,
            &state.db,
            &owner,
            &SealedRecordBytes::carried_at_rest_unchecked(body.to_vec()),
            &SealedRecordBytes::carried_at_rest_unchecked(b"hint".to_vec()),
            mail_floor(1_715_900_000_000 + i as i64),
        )
        .await
        .expect("append mail record");
        cids.push(outcome.cid);
    }

    let mut b = RpcRouter::builder();
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    let rpc = RouterRequester {
        router: b.build(),
        state: Arc::clone(&state),
        actor: owner,
    };

    (rpc, cids)
}

async fn mail_indexed_cids(store: &AccountStore<SqliteBackend>) -> Vec<Cid> {
    store
        .records_in_scope(&mail_scope(), None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.cid)
        .collect()
}

/// **Row 4: the relay-ack purge now carries per-record feed coordinates.**
/// Before this fix, `segments::mail::tombstone_up_to_seq` (the relay-ack
/// purge step) flipped `tombstoned` in one bulk UPDATE without touching
/// `changed_seq`, so a purged record's coordinate never moved — a replica
/// already past it would never learn of the delete, which is why `mail` sat
/// off `FEED_SERVED_KINDS`. This is `a_delete_after_the_replica_caught_up_
/// still_reaches_it`'s exact shape, replayed for mail's own purge path and
/// its own kind on the feed.
///
/// Red-verified by reverting `records_db::tombstone_up_to_seq_with_
/// coordinates`'s `changed_seq = (SELECT newseq …)` bump back to a bare
/// `tombstoned = 1`: this test fails (`(0, 0, 0)` vs. `(1, 1, 0)` — the purge
/// never arrives), and every other test in this file still passes.
#[tokio::test]
async fn a_mail_relay_ack_purge_reaches_a_replica_that_had_already_caught_up() {
    let (rpc, cids) = nest_with_mail(&[b"goes", b"stays"]).await;
    let store = empty_replica().await;
    let plane = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("mail", owner()).unwrap(),
    );

    // Caught up: the replica's frontier now sits past both records.
    let first = plane.walk().await.unwrap();
    assert_eq!(first.indexed, 2);
    assert_eq!(mail_indexed_cids(&store).await.len(), 2);

    // The relay-ack purge: acking seq 1 alone (the first-appended, lowest-seq
    // record) tombstones only that record's live row — the exact bulk path
    // row 4 gave a fresh coordinate.
    let purged = fauna_nest::segments::mail::tombstone_up_to_seq(&rpc.state.db, &owner(), 1)
        .await
        .expect("relay-ack purge");
    assert_eq!(purged, 1, "only the acked record purges");

    let second = plane.walk().await.unwrap();
    assert_eq!(
        (second.rows, second.tombstoned, second.indexed),
        (1, 1, 0),
        "exactly the purge came through — not a rescan of the whole scope"
    );
    assert_eq!(
        mail_indexed_cids(&store).await,
        vec![cids[1]],
        "the purged record is gone from the index; the survivor is untouched"
    );
}

/// `mail` on the feed door itself: a kind that used to be refused now walks
/// like any other served kind, since [`FEED_SERVED_KINDS`] gained it in the
/// same fix. Sibling of
/// `a_replica_with_no_segments_materializes_the_index_from_the_walk_alone`.
///
/// [`FEED_SERVED_KINDS`]: fauna_nest::segments::records_db::FEED_SERVED_KINDS
#[tokio::test]
async fn a_mail_scope_is_no_longer_refused_on_the_content_feed() {
    let (rpc, cids) = nest_with_mail(&[b"alpha", b"beta"]).await;
    let store = empty_replica().await;
    let plane = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("mail", owner()).unwrap(),
    );

    let report = plane
        .walk()
        .await
        .expect("mail scope is served, not refused");
    assert_eq!(report.indexed, 2);

    let mut have = mail_indexed_cids(&store).await;
    have.sort_by_key(|c| *c.as_bytes());
    let mut want = cids.clone();
    want.sort_by_key(|c| *c.as_bytes());
    assert_eq!(have, want);
}

// ── conv: channel members walk the channel scope's feed ────────────

const MEMBER_SEED: [u8; 32] = [0xC5; 32];
const STRANGER_SEED: [u8; 32] = [0xC6; 32];
/// The MLS channel under test. Opaque 32 bytes — the nest keys on it and never
/// interprets it (a conv scope id is the channel, not any actor).
const CHANNEL: [u8; 32] = [0x7C; 32];

fn member() -> [u8; 32] {
    ActorKeypair::from_secret(MEMBER_SEED).actor_id().0
}

fn stranger() -> [u8; 32] {
    ActorKeypair::from_secret(STRANGER_SEED).actor_id().0
}

fn conv_scope() -> String {
    fauna_protocol::scope::ContentScope::new("conv", CHANNEL)
        .unwrap()
        .to_string()
}

/// A requester dispatching as `actor` over the shared nest state. The conv
/// tests need several callers over ONE nest — the group creator delivering the
/// Welcome, the member walking, the stranger probing — so unlike the
/// single-owner fixtures the handler table is built per caller.
fn requester_as(state: &Arc<AppState>, actor: [u8; 32]) -> RouterRequester {
    let mut b = RpcRouter::builder();
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
    RouterRequester {
        router: b.build(),
        state: Arc::clone(state),
        actor,
    }
}

/// A real nest holding `bodies` as one channel's conv records (the production
/// `segments::conv::append` path), with [`member`] admitted through the REAL
/// Welcome-delivery kind — `fauna.conversations.welcome.deliver`, dispatched
/// through the handler table by the group creator — and [`stranger`] a local
/// user the channel never admitted. That Welcome is what writes the
/// `actor_channels` roster row the feed door's admission checks, so the test
/// covers the production flow end to end: Welcome delivered → roster row →
/// feed served. Feed-walk only, like [`nest_with_mail`]: conv's sequenced
/// record id cannot satisfy bulk adoption's re-hash check.
async fn nest_with_conv(bodies: &[&[u8]]) -> (Arc<AppState>, Vec<Cid>) {
    let creator = owner();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    for actor in [creator, member(), stranger()] {
        db.create_user(&actor, "free", "test").await.unwrap();
    }
    let state = Arc::new(AppState::for_test(db));

    // An accepted contact lets the creator's DM Welcome flow the reach floor
    // (established contacts flow under every inbox mode).
    state
        .db
        .upsert_contact(&member(), &creator, "accepted")
        .await
        .unwrap();
    let deliver = requester_as(&state, creator);
    let _: fauna_protocol::conversations::WelcomeDeliverReply = deliver
        .request(
            "fauna.conversations.welcome.deliver",
            fauna_protocol::conversations::WelcomeDeliverRequest {
                recipient_actor_id: hex::encode(member()),
                channel_id: hex::encode(CHANNEL),
                welcome_bytes: vec![],
                kind: fauna_protocol::conversations::WelcomeKind::Dm,
                nest_url: None,
                extra: Default::default(),
            },
        )
        .await
        .expect("welcome delivery");
    assert!(
        state
            .db
            .is_actor_in_channel(&member(), &CHANNEL)
            .await
            .unwrap(),
        "fixture: the Welcome must have registered the member on the roster"
    );

    let mut cids = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let outcome = fauna_nest::segments::conv::append(
            &state.conv_segments,
            &state.db,
            &CHANNEL,
            body,
            1_715_900_000_000 + i as i64,
        )
        .await
        .expect("append conv record");
        let _ = outcome;
        cids.push(fauna_mls::segments::derive_record_cid(body).expect("derive conv record cid"));
    }
    (state, cids)
}

async fn conv_indexed_cids(store: &AccountStore<SqliteBackend>) -> Vec<Cid> {
    store
        .records_in_scope(&conv_scope(), None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.cid)
        .collect()
}

/// **Row 100: a channel member's replica walks the conv scope's feed onto the
/// channel's current state** — coordinates and tombstones converge through the
/// ordinary `record-cid` arm, exactly as post does above. The admission fact
/// is the channel roster the Welcome delivery wrote (`actor_channels`, the
/// same fact `channel.actors` gates on) — never owner-equality, which has no
/// meaning for a scope id that names an MLS channel.
///
/// Nothing here waits on a clock (e2e convention 14): every step is driven
/// explicitly and every assertion is on state.
#[tokio::test]
async fn a_channel_members_replica_walks_the_conv_scope_feed() {
    let (state, cids) = nest_with_conv(&[b"purged later", b"kept"]).await;
    let rpc = requester_as(&state, member());
    let store = replica_for(member()).await;
    let plane = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("conv", CHANNEL).unwrap(),
    );

    // Caught up: the member's frontier now sits past both records.
    let first = plane.walk().await.expect("a member's conv walk is served");
    assert_eq!(first.indexed, 2);
    assert_eq!(conv_indexed_cids(&store).await.len(), 2);

    // The nest moves on: the relay-ack purge tombstones the first record
    // (seq 1) — the same `tombstone_up_to_seq_with_coordinates` bulk path the
    // mail test above pins — and one more record lands.
    let purged = fauna_nest::segments::conv::tombstone_up_to_seq(&state.db, &[CHANNEL], 1)
        .await
        .expect("relay-ack purge");
    assert_eq!(purged, 1, "only the acked record purges");
    let late_body: &[u8] = b"landed after the member caught up";
    let late_seq = fauna_nest::segments::conv::append(
        &state.conv_segments,
        &state.db,
        &CHANNEL,
        late_body,
        1_715_900_000_100,
    )
    .await
    .expect("late append")
    .seq;
    let _ = late_seq;
    let late = fauna_mls::segments::derive_record_cid(late_body).expect("derive conv record cid");

    let second = plane.walk().await.expect("still a member: still served");
    assert_eq!(
        (second.tombstoned, second.indexed),
        (1, 1),
        "exactly the purge and the late record came through"
    );

    let mut have = conv_indexed_cids(&store).await;
    have.sort_by_key(|c| *c.as_bytes());
    let mut want = vec![cids[1], late];
    want.sort_by_key(|c| *c.as_bytes());
    assert_eq!(
        have, want,
        "the member's index is the channel's live set: the survivor + the late \
         record, and the purged record is gone"
    );
}

/// **A non-member is refused loudly — and the refusal writes nothing.** The
/// door checks the roster; it must never auto-register the caller it is
/// checking (the `channel.actors` rule: a roster read must not write the very
/// row it reports), or any authenticated actor could self-admit to any
/// unclaimed channel's feed by asking.
#[tokio::test]
async fn a_non_members_conv_walk_is_refused_and_registers_nothing() {
    let (state, _cids) = nest_with_conv(&[b"a private coordinate"]).await;
    let rpc = requester_as(&state, stranger());
    let store = replica_for(stranger()).await;
    let plane = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("conv", CHANNEL).unwrap(),
    );

    let err = plane
        .walk()
        .await
        .expect_err("a non-member's conv walk is refused, never served and never answered empty");
    let s = format!("{err:#}");
    assert!(
        s.contains("fauna.sync.forbidden") && s.contains("member"),
        "want the membership refusal, got: {s}"
    );
    assert!(
        !state
            .db
            .is_actor_in_channel(&stranger(), &CHANNEL)
            .await
            .unwrap(),
        "the refused probe must not have registered its caller on the roster"
    );
    assert!(
        conv_indexed_cids(&store).await.is_empty(),
        "nothing reached the stranger's replica"
    );
}

/// **A lapsed membership serves a refusal, never an empty page.** Eviction is
/// the roster lapse the nest can observe (the F1 rotate-on-removal primitive,
/// `db::channels::evict_actor_from_channel` — the only `DELETE` on the
/// roster); after it the same walk that was served refuses. The replica
/// records the scope unserved and retries — dropping its local data stays the
/// departure seam's decision, taken only from the client's own affirmative
/// membership answer (charter § The replica boundary, T2 transition 3), never
/// from this refusal.
#[tokio::test]
async fn an_evicted_members_conv_walk_is_refused() {
    let (state, _cids) = nest_with_conv(&[b"was readable while a member"]).await;
    let rpc = requester_as(&state, member());
    let store = replica_for(member()).await;
    let plane = ContentScopePlane::new(
        &store,
        &rpc,
        fauna_protocol::scope::ContentScope::new("conv", CHANNEL).unwrap(),
    );

    assert_eq!(
        plane.walk().await.expect("while a member: served").indexed,
        1
    );

    assert!(
        state
            .db
            .evict_actor_from_channel(&member(), &CHANNEL)
            .await
            .unwrap(),
        "fixture: the eviction removed the roster row"
    );

    let err = plane
        .walk()
        .await
        .expect_err("after eviction: refused, not answered empty");
    let s = format!("{err:#}");
    assert!(
        s.contains("fauna.sync.forbidden"),
        "want the membership refusal, got: {s}"
    );
}

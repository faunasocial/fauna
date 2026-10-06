//! The `__index` rail end to end, driven by the **real client publisher**.
//!
//! `conformance_content_index_rail.rs` proves the nest half in isolation, with a
//! helper that pokes bytes straight into the blob store. This file proves the
//! half that helper stands in for: a real
//! [`fauna_client_index::IndexRailPublisher`] over a real
//! [`fauna_client::NestClient`], talking to a real `fauna-nest` router over real
//! HTTP and a real WebSocket — the path a logged-in app actually takes
//! (`docs/goal/behavior/content-index.md` § Ingest triggers, v1).
//!
//! It exists because every way this seam can fail is nest-side, and the seam has
//! already produced compiling, documented, zero-caller code that the nest
//! *refused* at runtime (`SyncEngine::publish_reserved_bytes`, deleted
//! 2026-08-02). A publisher that compiles proves nothing here.
//!
//! What is real: the blob route (`PUT /api/v1/blob/{cid}`, bearer-authed, CID
//! verified), the WS-RPC plane and its handshake-minted bearer, the
//! `fauna.index.{record,list}` handlers, `CacheDb`, and `DiskBlobStore`. What is
//! not the nest's business — and is asserted to stay that way — is the segment
//! *content*: it crosses the wire AEAD-sealed under a key no nest holds.
//!
//! Tier: tier_3 (real router + real `CacheDb` + real `DiskBlobStore` + a real
//! client crate over a real socket — no mocks).

mod common;

use std::sync::Arc;
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client::auth_client::AuthClient;
use fauna_client_index::{
    IndexBuilder, IndexRailPublisher, LocatedDraft, LocatedMessage, MailContentLookup,
    MailLocalSearch, MailcalKeyRing, MasterLocalSearch, SealedSegment, SegmentRail,
};
use fauna_client_search::{LocalSearchIndex, SearchKindClass, SearchNav};
// The contacts section drives the production launcher, so it needs the trait its
// two triggers live on (`launch` at attach, `ensure_arm` per sweep) and the raw
// requester the address-book fixtures write through.
use fauna_conversations::backend::IndexBuilderLauncher;
use fauna_conversations::index_sink::{
    IndexableDraft, IndexableKind, IndexableMessage, MessageIndexObserver,
};
use fauna_conversations::message::MessageId;
use fauna_conversations::thread::ThreadId;
use fauna_core::identity::ActorKeypair;
use fauna_index::{
    ContentKind, Index, IndexManifest, IndexMasterKey, IndexSegmentKey, mailcal_manifest_path,
    segment_path,
};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::RpcRequester;

/// The MSEK both halves derive the mail/calendar index-segment key from — the
/// client to seal, the test to re-open. The nest never sees it.
const MSEK: [u8; 32] = [0x5Au8; 32];

struct Harness {
    base: String,
    db: Arc<CacheDb>,
    /// Shared so `restart` can serve a second time over the *same* blob store —
    /// dropping it between the two would delete the at-rest bytes the restart
    /// leg exists to prove survive.
    dir: Arc<tempfile::TempDir>,
}

/// A real nest router on a real loopback socket, plain HTTP so the client needs
/// no TLS floor material (the escape the sibling serve-loop tests use).
async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory nest.db"));
    let dir = Arc::new(tempfile::tempdir().expect("tempdir"));
    serve_over(db, dir).await
}

/// Bring a nest up over an existing `nest.db` + blob store. `start` is this over
/// fresh ones; `restart` is this over the ones a previous serve left behind.
async fn serve_over(db: Arc<CacheDb>, dir: Arc<tempfile::TempDir>) -> Harness {
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None)
            .expect("backup service"),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            // The handshake the client mints its bearer over, plus the rail.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::content_index_handlers::register_content_index_handlers(&mut b);
            // Backend 1. The Success-clause tests at the bottom drive the whole
            // Search page through a real `SearchManager`, which fires BOTH arms
            // — without this the nest arm would fail and the page would render
            // its local rows beside an error, which is a different (also
            // ratified) state from the one they mean to assert.
            fauna_nest::search_handlers::register_search_handlers(&mut b);
            fauna_nest::bridge_carddav_handlers::register_bridge_carddav_handlers(&mut b);
            // The posts arm's whole wire surface: create (the fixtures + the
            // trickle's confirmation), list (the reconcile walk), get (the
            // per-hit resolver), delete (the display-healed deletion).
            fauna_nest::posts_handlers::register_posts_handlers(&mut b);
            // The File arm's whole wire surface, and it is two kinds rather than
            // one: `fauna.media.list` is the cross-set enumeration the walk
            // drains and the resolver re-reads, and `fauna.folders.list` is the
            // name→stable-id join that gives each row a durable identity
            // (`content-index.md` § Ingest triggers, v1 → *The files/media arms
            // are SCOPED*). Omitting the second is silent: the walk reads an
            // empty set list and stages nothing at all.
            fauna_nest::media_handlers::register_media_handlers(&mut b);
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            b.build()
        }),
        nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])),
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    Harness {
        base: format!("http://{addr}"),
        db,
        dir,
    }
}

/// The nest process goes away and comes back: a fresh `AppState`, router, socket
/// and (at the call site) client connection, over the **same** `nest.db` rows and
/// blob store. Nothing is carried across in memory.
async fn restart(h: &Harness) -> Harness {
    serve_over(h.db.clone(), h.dir.clone()).await
}

/// A connected client for `kp`, admitted to the nest the way registration would.
async fn connect(h: &Harness, kp: ActorKeypair) -> Arc<NestClient> {
    h.db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
    dial(h, kp).await
}

/// A second (or post-restart) connection for an actor the nest already knows —
/// admission is a one-time event, so this skips it and only dials.
async fn reconnect(h: &Harness, kp: ActorKeypair) -> Arc<NestClient> {
    dial(h, kp).await
}

async fn dial(h: &Harness, kp: ActorKeypair) -> Arc<NestClient> {
    let nest = NestClient::with_auth(Arc::new(AuthClient::new(h.base.clone(), kp)));
    nest.connect().await.expect("connect spawns the supervisor");

    // A generous ceiling on a loopback dial, polled — not a settle-sleep
    // (testing.md convention 14). A green run pays only the poll interval.
    let mut state = nest.connection_state();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if *state.borrow() == fauna_client::types::ConnectionState::Connected {
                return;
            }
            state
                .changed()
                .await
                .expect("connection-state channel open");
        }
    })
    .await
    .expect("the client reached Connected against the test nest");
    nest
}

fn observe(builder: &IndexBuilder, id: &str, subject: &str, body: &str) {
    let thread = ThreadId("t-1".into());
    let message = MessageId(id.into());
    builder.observe_indexable_message(IndexableMessage {
        kind: IndexableKind::Mail,
        thread_id: &thread,
        message_id: &message,
        subject: Some(subject),
        body,
        sender_actor_id: None,
        nest_message_id: None,
        timestamp_ms: 1_700_000_000_000,
        is_own: false,
    });
}

fn mailcal_key() -> IndexSegmentKey {
    IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK))
}

/// Flush and take the single published segment. Mirrors the crate-internal
/// `flush_one` (`fauna-client-index`'s own `index_builder` tests) for the same
/// reason: since one builder holds a kind *set*, `flush` reports one entry per
/// kind it touched, so "exactly one segment" is an assertion these single-kind
/// (mail-only) tests make rather than a fact the type still guarantees — and a
/// multi-kind flush leaking in should fail loudly here instead of silently
/// taking `[0]`.
async fn flush_one(builder: &IndexBuilder) -> SealedSegment {
    let mut segments = builder.flush().await.expect("flush");
    assert_eq!(
        segments.len(),
        1,
        "expected exactly one published segment, got {segments:?}"
    );
    segments.remove(0)
}

/// Fetch a published blob back over the public byte route, by the hex hash
/// `fauna.index.list` handed out — the read half a replica refresh performs.
async fn fetch(h: &Harness, hash_hex: &str) -> Vec<u8> {
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/blob/{hash_hex}", h.base))
        .send()
        .await
        .expect("GET the published blob");
    assert!(
        resp.status().is_success(),
        "blob GET {hash_hex} failed: {}",
        resp.status()
    );
    resp.bytes().await.expect("blob body").to_vec()
}

/// **The S3 success clause.** A `IndexBuilder::flush` against a real nest
/// lands sealed blobs, and `fauna.index.list` returns exactly the segment and
/// manifest it published — byte-identical on readback, and queryable once
/// unsealed with the key the nest does not have.
#[tokio::test]
async fn a_flush_lands_sealed_segments_a_second_device_can_list_and_open() {
    let h = start().await;
    let nest = connect(&h, ActorKeypair::generate()).await;

    let publisher = Arc::new(IndexRailPublisher::new(nest));
    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(
        &builder,
        "<a@x>",
        "Lunch plans",
        "shall we meet at the harbour",
    );
    observe(&builder, "<b@x>", "Invoice 42", "payment is due next week");

    let sealed = flush_one(&builder).await;
    assert_eq!(sealed.doc_count, 2);

    // ---- The rail's own view: exactly the two paths, nothing else.
    let listed = publisher.list().await.expect("fauna.index.list");
    let mut paths: Vec<&str> = listed.entries.iter().map(|e| e.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        // Sorted: `mail/` sorts before `manifest-` ('i' < 'n').
        vec![
            segment_path(ContentKind::Mail, 1).as_str(),
            mailcal_manifest_path().as_str(),
        ],
        "the rail lists exactly the segment and the manifest the builder published"
    );

    // ---- The bytes: byte-identical on readback, and still sealed.
    let seg_entry = listed
        .entries
        .iter()
        .find(|e| e.path == segment_path(ContentKind::Mail, 1))
        .expect("segment entry");
    let seg_bytes = fetch(&h, &seg_entry.blob_hash).await;
    assert_eq!(
        seg_bytes.len() as i64,
        seg_entry.size_bytes,
        "the recorded size must meter the bytes that actually landed"
    );
    assert_eq!(
        seg_bytes.len(),
        sealed.byte_len,
        "the blob read back is the blob the builder sealed"
    );
    assert!(
        !seg_bytes.windows(7).any(|w| w == b"harbour"),
        "a searchable term is readable in the blob the NEST holds — the segment \
         reached the rail unsealed, which is the exact defect the nest-side writer \
         was deleted for (content-index.md § Encryption posture)"
    );

    // ---- The second device: open the manifest, open the segment it lists, query.
    let man_entry = listed
        .entries
        .iter()
        .find(|e| e.path == mailcal_manifest_path())
        .expect("manifest entry");
    let man_bytes = fetch(&h, &man_entry.blob_hash).await;
    let manifest = IndexManifest::from_sealed_bytes_mailcal(&man_bytes, &mailcal_key())
        .expect("open manifest");
    assert_eq!(
        manifest
            .kind(ContentKind::Mail)
            .expect("mail kind present")
            .live_segments,
        vec![1],
        "the synced manifest points a second device at the segment that landed"
    );

    let index = Index::open_encrypted_mailcal(&seg_bytes, &mailcal_key()).expect("open segment");
    let hits = index
        .query("harbour", &[ContentKind::Mail], None, 10)
        .expect("query");
    assert_eq!(
        hits.len(),
        1,
        "the whole point: content published to the nest is searchable from the \
         replica, with a key the nest never held"
    );
}

/// The replica refresh: a second run opens the published manifest off the nest
/// and **appends to the same segment chain**, rather than restarting segment ids
/// and orphaning everything the first run built. The advisory ingest cursor
/// rides along, which is what stops a restart re-walking the whole mailbox.
#[tokio::test]
async fn a_resumed_builder_appends_to_the_published_chain_instead_of_restarting() {
    let h = start().await;
    // Same identity across both runs — a device coming back, not a new one.
    let secret = [0x3Cu8; 32];
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret(secret)).await,
    ));

    // ---- Run 1: build a segment and note how far the backfill got.
    let first = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&first, "<a@x>", "Lunch plans", "meet at the harbour");
    first
        .note_ingest_cursor(ContentKind::Mail, 4_242)
        .expect("cursor");
    let one = flush_one(&first).await;
    assert_eq!(one.path, segment_path(ContentKind::Mail, 1));
    drop(first);

    // ---- Run 2: a cold builder that knows only the MSEK and the nest.
    let second = fauna_client_index::resume_mail_builder(
        &MSEK,
        &MailcalKeyRing::from_msek(&MSEK),
        publisher.clone(),
    )
    .await
    .expect("resume off the synced replica");
    assert_eq!(
        second.ingest_cursor(ContentKind::Mail),
        Some(4_242),
        "the advisory cursor survives the restart — otherwise the backfill \
         re-walks the whole mailbox every launch"
    );

    observe(&second, "<b@x>", "Invoice 42", "payment is due next week");
    let two = flush_one(&second).await;
    assert_eq!(
        two.path,
        segment_path(ContentKind::Mail, 2),
        "a resumed builder must not restart segment ids — id 1 is already taken \
         and rewriting it would orphan the first run's docs"
    );

    // ---- Both segments are live in the manifest, and both still open.
    let listed = publisher.list().await.expect("fauna.index.list");
    let man_bytes = fetch(
        &h,
        &listed
            .entries
            .iter()
            .find(|e| e.path == mailcal_manifest_path())
            .expect("manifest entry")
            .blob_hash,
    )
    .await;
    let manifest = IndexManifest::from_sealed_bytes_mailcal(&man_bytes, &mailcal_key())
        .expect("open manifest");
    assert_eq!(
        manifest
            .kind(ContentKind::Mail)
            .expect("mail kind present")
            .live_segments,
        vec![1, 2],
        "the chain grew; neither run's segment was dropped"
    );

    for (seg_id, term) in [(1u32, "harbour"), (2, "invoice")] {
        let hash = &listed
            .entries
            .iter()
            .find(|e| e.path == segment_path(ContentKind::Mail, seg_id))
            .unwrap_or_else(|| panic!("segment {seg_id} listed"))
            .blob_hash;
        let index = Index::open_encrypted_mailcal(&fetch(&h, hash).await, &mailcal_key())
            .expect("open segment");
        assert_eq!(
            index
                .query(term, &[ContentKind::Mail], None, 10)
                .expect("query")
                .len(),
            1,
            "segment {seg_id} lost its docs across the resume"
        );
    }
}

/// The local query handle — the seam the Search page's local arm calls. A
/// device that built nothing itself opens the synced replica and searches
/// **across the whole segment chain**, which is the user-visible point of the
/// entire slice.
#[tokio::test]
async fn a_reader_queries_across_every_live_segment_of_the_synced_replica() {
    let h = start().await;
    let secret = [0x77u8; 32];
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret(secret)).await,
    ));

    // Two flushes → two segments, so the reader must span them, not just open
    // the newest.
    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<a@x>", "Lunch plans", "meet at the harbour");
    builder.flush().await.expect("flush 1");
    observe(&builder, "<b@x>", "Invoice 42", "payment is due next week");
    builder.flush().await.expect("flush 2");

    // A cold reader: it knows the MSEK and the nest, and built nothing.
    let reader =
        fauna_client_index::open_mail_reader(&MailcalKeyRing::from_msek(&MSEK), publisher.as_ref())
            .await
            .expect("open the synced replica");

    for (term, expected_id) in [("harbour", "<a@x>"), ("payment", "<b@x>")] {
        let hits = reader
            .query(term, fauna_client_index::MAILCAL_KINDS, None, 10)
            .expect("query");
        assert_eq!(
            hits.len(),
            1,
            "`{term}` should hit exactly once across the chain"
        );
        assert_eq!(
            hits[0].content_id,
            fauna_index::ContentId(expected_id.as_bytes().to_vec()),
            "`{term}` resolved to the wrong message — a hit's content_id is the \
             navigation target the Search page renders"
        );
        assert_eq!(hits[0].kind, ContentKind::Mail);
    }

    assert!(
        reader
            .query(
                "nothingmatchesthis",
                fauna_client_index::MAILCAL_KINDS,
                None,
                10
            )
            .expect("query")
            .is_empty(),
        "a miss is an empty result, never an error"
    );
}

/// A device that has never published anything must render *no local results*,
/// not a failure — the state every fresh device is in before its first flush.
#[tokio::test]
async fn a_reader_on_an_actor_with_no_index_is_empty_rather_than_an_error() {
    let h = start().await;
    let publisher = IndexRailPublisher::new(connect(&h, ActorKeypair::generate()).await);

    let reader =
        fauna_client_index::open_mail_reader(&MailcalKeyRing::from_msek(&MSEK), &publisher)
            .await
            .expect("a fresh actor's reader opens cleanly");
    assert!(
        reader
            .query("anything", fauna_client_index::MAILCAL_KINDS, None, 10)
            .expect("query")
            .is_empty(),
        "no index yet is a normal state, not an error"
    );
}

/// A real mailbox's segments are megabytes, and the blob PUT route silently kept
/// axum's 2 MB `Bytes` default until the `DefaultBodyLimit` layer landed beside
/// it — so a perfectly correct publisher would have 413'd on real data while
/// every small-fixture test stayed green. This pins the limit.
#[tokio::test]
async fn a_segment_larger_than_axums_default_body_limit_still_lands() {
    let h = start().await;
    let nest = connect(&h, ActorKeypair::generate()).await;
    let publisher = IndexRailPublisher::new(nest);

    // Comfortably past axum's 2 MiB `Bytes` default, comfortably under both the
    // nest's 10 MiB `BLOB_BODY_LIMIT` and the builder's 8 MiB segment ceiling.
    let bytes = vec![0xABu8; 3 * 1024 * 1024];
    let path = segment_path(ContentKind::Mail, 7);
    publisher
        .publish(&path, &bytes)
        .await
        .expect("a multi-megabyte segment must reach the blob route");

    let listed = publisher.list().await.expect("fauna.index.list");
    let entry = listed
        .entries
        .iter()
        .find(|e| e.path == path)
        .expect("the big segment is listed");
    assert_eq!(entry.size_bytes, bytes.len() as i64);
    assert_eq!(
        fetch(&h, &entry.blob_hash).await,
        bytes,
        "byte-identical across a >2 MiB round trip"
    );
}

/// The wire code behind a refusal — the stable, contractual half of the wire
/// error. See the sibling helper in `conformance_custody_nest_door_client.rs`
/// for why a rendered `to_string()` is the wrong thing to pin.
fn refusal_code(err: &fauna_client::NestClientError) -> &str {
    match err {
        fauna_client::NestClientError::Rpc(e) => e.code.as_str(),
        other => panic!("expected a wire-level refusal, got a transport fault: {other:?}"),
    }
}

/// The ordering contract, from the client side. The nest verifies it holds the
/// blob before writing the row, so a publisher that recorded first would be
/// refused — this proves the refusal is real and typed, not a convention the
/// client is trusted to keep.
#[tokio::test]
async fn recording_a_path_whose_bytes_were_never_uploaded_is_refused() {
    use fauna_protocol::content_index::{
        KIND_RECORD, RecordIndexBlobReply, RecordIndexBlobRequest,
    };

    let h = start().await;
    let nest = connect(&h, ActorKeypair::generate()).await;

    let err = nest
        .request::<_, RecordIndexBlobReply>(
            KIND_RECORD,
            RecordIndexBlobRequest {
                path: segment_path(ContentKind::Mail, 1),
                // Well-formed, and the nest holds nothing under it.
                blob_hash: "11".repeat(32),
                size_bytes: 42,
                extra: Default::default(),
            },
        )
        .await
        .expect_err("the nest must refuse a row pointing at bytes it does not hold");
    // Assert the CODE, not the rendered sentence: since
    // 2026-08-25 `NestClientError::Display` renders the localized string,
    // which deliberately carries no wire vocabulary (`version-compatibility.md`
    // § Dimension 4). This mirrors the same refusal's nest-side assertion in
    // `conformance_content_index_rail.rs`.
    assert_eq!(
        refusal_code(&err),
        "fauna.index.bytes_not_held",
        "expected the typed retryable refusal"
    );
}

// ── The Search page's local arm, over the same real rail ──────────────────────

/// The conversations store, as the projection sees it — the device-local content
/// a snippet and a navigation target are resolved from.
#[derive(Default)]
struct StubLookup(
    std::sync::Mutex<std::collections::HashMap<String, LocatedMessage>>,
    std::sync::Mutex<std::collections::HashMap<String, LocatedDraft>>,
);

impl StubLookup {
    fn holds(&self, message_id: &str, thread_id: &str, body: &str) {
        self.0.lock().unwrap().insert(
            message_id.to_string(),
            LocatedMessage {
                thread_id: thread_id.to_string(),
                body: body.to_string(),
            },
        );
    }

    /// The device's live draft store, as the projection sees it. `forgets` is
    /// what a discard looks like from the query side.
    fn holds_draft(&self, content_id: &str, thread_id: Option<&str>, body: &str) {
        self.1.lock().unwrap().insert(
            content_id.to_string(),
            LocatedDraft {
                thread_id: thread_id.map(|t| t.to_string()),
                body: body.to_string(),
            },
        );
    }

    fn forgets_draft(&self, content_id: &str) {
        self.1.lock().unwrap().remove(content_id);
    }
}

impl MailContentLookup for StubLookup {
    fn locate(&self, message_id: &str) -> Option<LocatedMessage> {
        self.0.lock().unwrap().get(message_id).cloned()
    }

    fn locate_draft(&self, content_id: &str) -> Option<LocatedDraft> {
        self.1.lock().unwrap().get(content_id).cloned()
    }
}

/// **The whole point of rollout S3: backend 2 answers the Search page.** A mail
/// that was indexed and published comes back through the shared
/// `LocalSearchIndex` seam as a display-ready row — a real snippet rendered from
/// locally-held content and a navigation target the conversations page can open
/// (`ui/search.md` § State & data shape).
#[tokio::test]
async fn the_local_arm_answers_a_query_with_navigable_rows_and_real_snippets() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x21u8; 32])).await,
    ));

    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<a@x>", "Lunch plans", "meet at the harbour");
    builder.flush().await.expect("flush");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds("<a@x>", "thread-42", "meet at the harbour");
    let arm = MailLocalSearch::new(
        MailcalKeyRing::from_msek(&MSEK),
        publisher.clone(),
        lookup.clone(),
    );

    let rows = arm
        .query("harbour", &[SearchKindClass::Mail], 10)
        .await
        .expect("the local arm answers");

    assert_eq!(
        rows.len(),
        1,
        "expected exactly the indexed mail, got {rows:?}"
    );
    assert_eq!(rows[0].content_id, "<a@x>");
    assert_eq!(rows[0].content_type, "mail");
    assert_eq!(
        rows[0].snippet, "meet at the harbour",
        "the snippet must render from locally-held content — the sealed index \
         stores postings only (content-index.md § Don't do these)"
    );
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::Mail {
            thread_id: "thread-42".into(),
            message_id: "<a@x>".into(),
        }),
        "search.md § State & data shape: local rows always carry Some(navigation)"
    );
}

/// **Mail that arrives during the session is findable.** The arm holds a reader
/// open between queries, so a segment published after the first query is exactly
/// the case a cache gets wrong — and the one the Success clause names ("a mail
/// that arrived this session is found by the Search page's local arm").
///
/// Latency-independent per convention 14: the second query is ordered *after*
/// the second flush by awaiting it, not by waiting out a window.
#[tokio::test]
async fn a_segment_published_after_the_first_query_is_found_by_the_next() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x22u8; 32])).await,
    ));

    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<first@x>", "Lunch plans", "meet at the harbour");
    builder.flush().await.expect("flush 1");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds("<first@x>", "thread-1", "meet at the harbour");
    lookup.holds("<second@x>", "thread-2", "payment is due next week");
    let arm = MailLocalSearch::new(
        MailcalKeyRing::from_msek(&MSEK),
        publisher.clone(),
        lookup.clone(),
    );

    // First query opens (and caches) a reader over the one published segment.
    let before = arm
        .query("payment", &[SearchKindClass::Mail], 10)
        .await
        .expect("the local arm answers");
    assert!(
        before.is_empty(),
        "nothing matching `payment` is published yet, got {before:?}"
    );

    // The mail arrives and its segment lands — the manifest moves.
    observe(
        &builder,
        "<second@x>",
        "Invoice 42",
        "payment is due next week",
    );
    builder.flush().await.expect("flush 2");

    let after = arm
        .query("payment", &[SearchKindClass::Mail], 10)
        .await
        .expect("the local arm answers");
    assert_eq!(
        after.len(),
        1,
        "a segment published after the first query must be visible to the next \
         one — a reader cached past a manifest change hides freshly arrived mail"
    );
    assert_eq!(after[0].content_id, "<second@x>");
}

/// The type filter reaches the engine: a page filtered to calendar must not
/// return the mail rows the same key opens.
#[tokio::test]
async fn a_filter_that_excludes_mail_returns_no_mail_rows() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x23u8; 32])).await,
    ));

    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<a@x>", "Lunch plans", "meet at the harbour");
    builder.flush().await.expect("flush");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds("<a@x>", "thread-42", "meet at the harbour");
    let arm = MailLocalSearch::new(
        MailcalKeyRing::from_msek(&MSEK),
        publisher.clone(),
        lookup.clone(),
    );

    let rows = arm
        .query("harbour", &[SearchKindClass::Calendar], 10)
        .await
        .expect("the local arm answers");
    assert!(
        rows.is_empty(),
        "a calendar-only filter must not return the mail row, got {rows:?}"
    );
}

/// A device whose store cannot resolve the hit renders **no row** rather than an
/// empty, unclickable one — the rule that keeps search.md's "local rows always
/// carry Some(navigation)" true against an index that outlives the store.
#[tokio::test]
async fn a_published_hit_this_device_cannot_resolve_yields_no_row() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x24u8; 32])).await,
    ));

    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<a@x>", "Lunch plans", "meet at the harbour");
    builder.flush().await.expect("flush");

    // The store holds nothing — the state a device is in before its mailbox
    // re-walk repopulates it.
    let arm = MailLocalSearch::new(
        MailcalKeyRing::from_msek(&MSEK),
        publisher.clone(),
        Arc::new(StubLookup::default()),
    );

    let rows = arm
        .query("harbour", &[SearchKindClass::Mail], 10)
        .await
        .expect("an unresolvable hit is not an error");
    assert!(
        rows.is_empty(),
        "an unresolvable hit must be dropped, not rendered inert, got {rows:?}"
    );
}

// ── The S3 Success clause, at the level the user actually sees ────────────────
//
// The tests above pin the rail and the `LocalSearchIndex` arm. These pin the
// **page**: a real `SearchManager` firing both backends over the real nest, and
// the `SearchSnapshot` it commits — the object every one of the seven apps
// paints (`ui/search.md` § State & data shape). That level is where the S3
// Success clause is written ("a user's private content is searchable from the
// app Search page"), and it is the one an arm-level test cannot reach: the arm
// can answer perfectly while the manager drops the rows, merges them wrong, or
// never registers the local index at all — which is exactly the state every app
// was in before piece 5b (`has_local_index()` answered `false` everywhere).

/// Stand up the page as an app does: a manager over the real authed transport,
/// with backend 2 registered.
fn page(
    nest: Arc<NestClient>,
    arm: Arc<MailLocalSearch>,
) -> fauna_client_search::SearchManager<Arc<NestClient>> {
    let manager = fauna_client_search::SearchManager::new(nest);
    manager.set_local_index(arm);
    manager
}

/// The rows a query commits to the snapshot, with the nest arm asserted healthy
/// — a local-only assertion would pass just as well against a broken backend 1,
/// and "partial results shown honestly" is a *different* ratified state that
/// must not be mistaken for this one.
async fn search_page(
    manager: &fauna_client_search::SearchManager<Arc<NestClient>>,
    query: &str,
) -> Vec<fauna_client_search::SearchResultRow> {
    manager
        .run_query(query, fauna_client_search::TYPE_FILTER_ALL)
        .await;
    let snap = manager.snapshot();
    assert!(
        !snap.in_flight,
        "run_query returns only once both arms have settled"
    );
    assert_eq!(
        snap.error, None,
        "both arms must be healthy here — an error means this test is proving \
         the partial-results path by accident, not the merge"
    );
    snap.results
}

/// **Leg 1 of the Success clause: mail arrives → the Search page finds it
/// locally.** Not "a segment was published" and not "the arm answers" — a row
/// on the page, attributed to backend 2, carrying the snippet and the
/// navigation target the conversations page opens.
#[tokio::test]
async fn mail_that_arrived_is_a_local_row_on_the_search_page() {
    let h = start().await;
    let nest = connect(&h, ActorKeypair::from_secret([0x31u8; 32])).await;
    let publisher = Arc::new(IndexRailPublisher::new(nest.clone()));

    // Mail arrives and the builder publishes it, the way the receive loop does.
    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<lunch@x>", "Lunch plans", "meet at the harbour");
    builder.flush().await.expect("flush");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds("<lunch@x>", "thread-42", "meet at the harbour");
    let manager = page(
        nest,
        Arc::new(MailLocalSearch::new(
            MailcalKeyRing::from_msek(&MSEK),
            publisher,
            lookup,
        )),
    );

    let rows = search_page(&manager, "harbour").await;

    assert_eq!(rows.len(), 1, "expected the arrived mail, got {rows:?}");
    assert_eq!(
        rows[0].source,
        fauna_client_search::SearchSource::Local,
        "the row must be attributed to backend 2 — the nest cannot see this \
         mail at all, it is sealed under a key no nest holds"
    );
    assert_eq!(rows[0].snippet, "meet at the harbour");
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::Mail {
            thread_id: "thread-42".into(),
            message_id: "<lunch@x>".into(),
        }),
    );
}

/// **Leg 2: the nest restarts and the mail is still findable.**
///
/// `index_survives_nest_restart.rs` is the standing pin that a boot does not
/// *delete* `__index` content, driven against the whole real serve loop. This
/// asserts the consequence that matters to the user and that blob survival does
/// not by itself prove: after the nest comes back, **the query still answers**.
/// A rail that survives the boot but whose manifest the returning client can no
/// longer resolve is a silently empty Search page.
///
/// The restart modelled here is a fresh `AppState` + router + socket + client
/// connection over the *same* at-rest state (`nest.db` rows and the blob store),
/// which is what this in-process harness can reach; the cold-boot half is the
/// sibling test's job. The client is rebuilt too, so nothing is carried in
/// memory across the boundary.
#[tokio::test]
async fn mail_is_still_found_on_the_page_after_the_nest_restarts() {
    let h = start().await;
    let secret = [0x32u8; 32];
    let nest = connect(&h, ActorKeypair::from_secret(secret)).await;
    let publisher = Arc::new(IndexRailPublisher::new(nest));

    let builder = IndexBuilder::mail(&MSEK, publisher.clone());
    observe(&builder, "<invoice@x>", "Invoice", "the quarterly invoice");
    builder.flush().await.expect("flush");

    // The nest goes away and comes back over the same at-rest state.
    let h2 = restart(&h).await;
    let nest2 = reconnect(&h2, ActorKeypair::from_secret(secret)).await;
    let publisher2 = Arc::new(IndexRailPublisher::new(nest2.clone()));

    let lookup = Arc::new(StubLookup::default());
    lookup.holds("<invoice@x>", "thread-7", "the quarterly invoice");
    let manager = page(
        nest2,
        Arc::new(MailLocalSearch::new(
            MailcalKeyRing::from_msek(&MSEK),
            publisher2,
            lookup,
        )),
    );

    let rows = search_page(&manager, "quarterly").await;

    assert_eq!(
        rows.len(),
        1,
        "mail indexed before the restart must still be findable after it, \
         got {rows:?}"
    );
    assert_eq!(rows[0].source, fauna_client_search::SearchSource::Local);
    assert_eq!(rows[0].snippet, "the quarterly invoice");
}

/// **Leg 3: a second app instance finds the mail without ever building.**
///
/// The other seat runs no builder and never saw the message arrive — it only
/// syncs the sealed segments off the rail and opens them with the key derived
/// from the same MSEK. This is what makes the index a *replica* rather than
/// per-device state, and it is the leg that proves the seal is the only thing
/// standing between the nest and the content: the nest served these bytes to a
/// device it cannot distinguish from the first.
#[tokio::test]
async fn a_second_app_instance_finds_the_mail_without_building_it() {
    let h = start().await;
    let secret = [0x33u8; 32];

    // Seat 1 receives the mail and publishes.
    let seat1 = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret(secret)).await,
    ));
    let builder = IndexBuilder::mail(&MSEK, seat1.clone());
    observe(&builder, "<tickets@x>", "Tickets", "two tickets for friday");
    builder.flush().await.expect("flush");

    // Seat 2: same account, its own connection, NO builder — a pure querier.
    let nest2 = reconnect(&h, ActorKeypair::from_secret(secret)).await;
    let seat2 = Arc::new(IndexRailPublisher::new(nest2.clone()));
    let lookup = Arc::new(StubLookup::default());
    lookup.holds("<tickets@x>", "thread-9", "two tickets for friday");
    let manager = page(
        nest2,
        Arc::new(MailLocalSearch::new(
            MailcalKeyRing::from_msek(&MSEK),
            seat2,
            lookup,
        )),
    );

    let rows = search_page(&manager, "friday").await;

    assert_eq!(
        rows.len(),
        1,
        "a seat that never built the index must still find the mail from the \
         synced segments, got {rows:?}"
    );
    assert_eq!(rows[0].source, fauna_client_search::SearchSource::Local);
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::Mail {
            thread_id: "thread-9".into(),
            message_id: "<tickets@x>".into(),
        }),
    );
}

/// **Compaction, against the real rail** (`content-index.md` § Where the index
/// is built — fold-at-flush, ruled 2026-08-02, built 2026-08-03).
///
/// The unit tests beside the builder prove the fold's *policy* and its manifest
/// bookkeeping against an in-memory rail. This proves the half only a real nest
/// can: that the fold's inputs come back correctly over `fauna.index.list` +
/// the blob route (real sizes, real hex hashes, real HTTP), that the merged
/// segment publishes through the same record path, and that a **cold reader**
/// — one that never saw the pre-fold chain — answers exactly what the pre-fold
/// index answered. A fold that silently dropped a folded segment's docs would
/// pass every arm-level check and fail precisely here.
#[tokio::test]
async fn a_fold_over_the_real_rail_shrinks_the_chain_and_keeps_every_result() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x5Du8; 32])).await,
    ));
    let builder = IndexBuilder::mail(&MSEK, publisher.clone());

    // One segment per flush, past the ratified 16-live-segment threshold.
    let count = fauna_client_index::COMPACTION_LIVE_SEGMENT_THRESHOLD + 1;
    for i in 0..count {
        observe(
            &builder,
            &format!("<m{i}@x>"),
            "quarterly",
            "meet at the harbour",
        );
        flush_one(&builder).await;
    }

    let live_before = live_mail_segments(&h, &publisher).await;
    assert_eq!(
        live_before.len(),
        count,
        "one segment per flush, so the kind is now past the threshold"
    );

    // The flush that folds.
    observe(&builder, "<new@x>", "quarterly", "meet at the harbour");
    flush_one(&builder).await;

    let live_after = live_mail_segments(&h, &publisher).await;
    assert!(
        live_after.len() < live_before.len(),
        "the fold must shrink the live chain over the real rail: {} -> {}",
        live_before.len(),
        live_after.len()
    );

    // A cold reader, resuming from nothing but the MSEK and the nest, sees
    // every document — the folded ones included.
    let reader =
        fauna_client_index::open_mail_reader(&MailcalKeyRing::from_msek(&MSEK), publisher.as_ref())
            .await
            .expect("open a reader off the synced replica");
    let hits = reader
        .query("harbour", &[ContentKind::Mail], None, 1000)
        .expect("query");
    let mut ids: Vec<Vec<u8>> = hits.into_iter().map(|hit| hit.content_id.0).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        count + 1,
        "every doc must survive the fold — {} staged, {} found",
        count + 1,
        ids.len()
    );

    // Tombstone-only: every folded segment's blob is still on the rail.
    let manifest = published_manifest(&h, &publisher).await;
    let km = manifest.kind(ContentKind::Mail).expect("a mail kind");
    assert!(
        !km.tombstoned_segments.is_empty(),
        "no fold was taken, so this test proves nothing about folding"
    );
    let listed = publisher.list_entries().await.expect("list");
    for folded in &km.tombstoned_segments {
        let path = segment_path(ContentKind::Mail, *folded);
        assert!(
            listed.iter().any(|e| e.path == path),
            "folded {path} left the rail — a fold never deletes a segment blob"
        );
        assert!(
            !km.live_segments.contains(folded),
            "segment {folded} is both live and tombstoned"
        );
    }
}

/// The mail kind's live segment ids, read back off the published manifest.
async fn live_mail_segments(h: &Harness, publisher: &Arc<IndexRailPublisher>) -> Vec<u32> {
    published_manifest(h, publisher)
        .await
        .kind(ContentKind::Mail)
        .expect("a mail kind")
        .live_segments
        .clone()
}

async fn published_manifest(h: &Harness, publisher: &Arc<IndexRailPublisher>) -> IndexManifest {
    let listed = publisher.list_entries().await.expect("list");
    let entry = listed
        .iter()
        .find(|e| e.path == mailcal_manifest_path())
        .expect("a published mail/calendar manifest");
    IndexManifest::from_sealed_bytes_mailcal(&fetch(h, &entry.blob_hash).await, &mailcal_key())
        .expect("open the manifest")
}

// ── The drafts arm, over the real rail ────────────────────────────────────────

/// The master-class key these drafts cases seal under. Distinct bytes from
/// [`MSEK`] so a cross-class open failure cannot be a coincidence.
const DRAFT_MASTER: [u8; 32] = [0x5du8; 32];

fn draft_corpus(entries: &[(&str, &str)]) -> Vec<IndexableDraft> {
    entries
        .iter()
        .map(|(id, body)| IndexableDraft {
            content_id: (*id).to_string(),
            thread_id: Some(fauna_conversations::thread::ThreadId((*id).to_string())),
            subject: None,
            body: (*body).to_string(),
        })
        .collect()
}

/// **The drafts arm's success clause, over a real nest, a real socket and the
/// real rail**: a phrase that exists only in an unsent draft comes back as a
/// display-ready local row that navigates to the composer holding it.
#[tokio::test]
async fn an_unsent_draft_is_a_navigable_local_row() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x31u8; 32])).await,
    ));

    let builder = IndexBuilder::master(
        IndexMasterKey::from_bytes(DRAFT_MASTER),
        [ContentKind::Draft],
        publisher.clone(),
    );
    builder.observe_draft_corpus(&draft_corpus(&[("thread-9", "pemmican for the crossing")]));
    builder.flush().await.expect("flush");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds_draft("thread-9", Some("thread-9"), "pemmican for the crossing");
    let arm = MasterLocalSearch::new(
        IndexMasterKey::from_bytes(DRAFT_MASTER),
        publisher.clone(),
        lookup.clone(),
    );

    let rows = arm
        .query("pemmican", &[SearchKindClass::Draft], 10)
        .await
        .expect("the master arm answers");

    assert_eq!(rows.len(), 1, "expected exactly the draft, got {rows:?}");
    assert_eq!(rows[0].content_type, "draft");
    assert_eq!(
        rows[0].snippet, "pemmican for the crossing",
        "the snippet renders from the live draft store, not from the index"
    );
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::Draft {
            thread_id: Some("thread-9".into())
        }),
        "a draft row opens the composer holding it"
    );
}

/// **The clause that separates a mutable kind from mail, proven at rest**: after
/// an edit, a query for the deleted phrase finds nothing — because the segment
/// that held it is no longer live on the real rail, not merely because some
/// in-memory copy moved on.
#[tokio::test]
async fn an_edited_draft_stops_matching_its_old_text_over_the_rail() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x32u8; 32])).await,
    ));

    let builder = IndexBuilder::master(
        IndexMasterKey::from_bytes(DRAFT_MASTER),
        [ContentKind::Draft],
        publisher.clone(),
    );
    builder.observe_draft_corpus(&draft_corpus(&[("thread-9", "pemmican for the crossing")]));
    builder.flush().await.expect("flush 1");

    // The user rewrites the draft.
    builder.observe_draft_corpus(&draft_corpus(&[("thread-9", "biltong for the crossing")]));
    builder.flush().await.expect("flush 2");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds_draft("thread-9", Some("thread-9"), "biltong for the crossing");
    let arm = MasterLocalSearch::new(
        IndexMasterKey::from_bytes(DRAFT_MASTER),
        publisher.clone(),
        lookup.clone(),
    );

    let new_text = arm
        .query("biltong", &[SearchKindClass::Draft], 10)
        .await
        .expect("query");
    assert_eq!(new_text.len(), 1, "the new text is findable");

    let old_text = arm
        .query("pemmican", &[SearchKindClass::Draft], 10)
        .await
        .expect("query");
    assert!(
        old_text.is_empty(),
        "the text the user deleted must stop matching — got {old_text:?}"
    );
}

/// A draft discarded after its segment was sealed is **dropped**, not rendered
/// as an empty row with a dead click (`ui/search.md` § State & data shape).
///
/// Both halves are exercised deliberately: the index still holds the doc (this
/// query runs before any re-flush), and the live store no longer resolves it —
/// which is exactly the transient state the drop rule exists for.
#[tokio::test]
async fn a_draft_discarded_since_the_flush_is_dropped_not_rendered_inert() {
    let h = start().await;
    let publisher = Arc::new(IndexRailPublisher::new(
        connect(&h, ActorKeypair::from_secret([0x33u8; 32])).await,
    ));

    let builder = IndexBuilder::master(
        IndexMasterKey::from_bytes(DRAFT_MASTER),
        [ContentKind::Draft],
        publisher.clone(),
    );
    builder.observe_draft_corpus(&draft_corpus(&[
        ("thread-9", "pemmican for the crossing"),
        ("thread-10", "pemmican inventory"),
    ]));
    builder.flush().await.expect("flush");

    let lookup = Arc::new(StubLookup::default());
    lookup.holds_draft("thread-9", Some("thread-9"), "pemmican for the crossing");
    lookup.holds_draft("thread-10", Some("thread-10"), "pemmican inventory");
    let arm = MasterLocalSearch::new(
        IndexMasterKey::from_bytes(DRAFT_MASTER),
        publisher.clone(),
        lookup.clone(),
    );
    assert_eq!(
        arm.query("pemmican", &[SearchKindClass::Draft], 10)
            .await
            .expect("query")
            .len(),
        2,
        "both drafts are findable while the store holds them"
    );

    // The user discards one. The index still holds its doc until the next flush.
    lookup.forgets_draft("thread-10");

    let rows = arm
        .query("pemmican", &[SearchKindClass::Draft], 10)
        .await
        .expect("query");
    assert_eq!(rows.len(), 1, "the discarded draft's row is dropped");
    assert_eq!(rows[0].content_id, "thread-9");
}

// ── The contacts arm, driven by the REAL LAUNCHER ─────────────────────────────
//
// Everything above drives an `IndexBuilder` (or a reader) the test constructed
// itself. **These drive `NestMailIndexLauncher`** — the object app glue actually
// holds — and that is the point of the section rather than a stylistic
// preference: the contacts producer is not a seam callback but a *walk the
// launcher runs itself* (`content-index.md` § Ingest triggers, v1 — the third
// ingest class), so a builder-level test asserts nothing about whether the walk
// ever runs, whether it runs again on the next sweep, or whether what it staged
// reaches a flush. All three can be broken with every test above still green —
// which is the shape that shipped the drafts arm dark for a day.
//
// The fixtures write through the wire (`fauna.bridges.*` + `fauna.config`) as
// setup, which convention 8 permits; every *assertion* comes back through the
// Search page's `SearchManager`.

/// The MSEK this section's seats provision mail with. Distinct from [`MSEK`] and
/// [`DRAFT_MASTER`] so a cross-fixture open failure cannot be a coincidence.
const CONTACT_MSEK: [u8; 32] = [0x5cu8; 32];
/// A stable client-assigned address-book id (the MKCOL path's own choice).
const BOOK_ID: [u8; 32] = [0xabu8; 32];
/// A second book, for the per-KIND corpus rule: staging one book's cards must
/// not retire the other's.
const BOOK_ID_2: [u8; 32] = [0xacu8; 32];

/// Everything the arm's preconditions need, in the order a real seat gets them:
/// an admitted actor, a connected client, and mail provisioned through a real
/// `fauna.config` CAS write.
///
/// ⚠ **The mail step is not optional colour.** Contacts are gated on the MSEK
/// rather than on the identity seed the rest of the master class rides
/// (`content-index.md` § Ingest triggers, v1 → the contacts ruling's BUILT
/// sub-bullet), so a test that skips it drives a walk that returns at its first
/// `let ... else` and every later assertion passes vacuously against an empty
/// index.
/// Takes the seed rather than the keypair because `ActorKeypair` is
/// deliberately not `Clone` (key material is minted per use, never duplicated);
/// returns the connected client and the actor id the `fauna.bridges.*` fixtures
/// address.
/// The actors [`seat_with_mail`] provisioned — each one's mail custody holds
/// [`CONTACT_MSEK`]; every other seat's is empty.
static MAIL_SEATS: std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// The mail custody (`fauna.state.mail`) of the seat `nest` is signed in as —
/// the account runtime's store in production.
fn mail_custody_of(nest: &Arc<NestClient>) -> Arc<dyn fauna_client_config::MailStore> {
    let actor = nest
        .auth()
        .keypair()
        .expect("a signed-in seat")
        .actor_id()
        .0;
    let store = if MAIL_SEATS.lock().unwrap().contains(&actor) {
        fauna_client_config::test_helpers::FakeMailStore::with(&fauna_core::data::MailConfig {
            msek: Some(CONTACT_MSEK.into()),
            mail_enabled: Some(true),
            ..Default::default()
        })
    } else {
        fauna_client_config::test_helpers::FakeMailStore::empty()
    };
    Arc::new(store)
}

async fn seat_with_mail(h: &Harness, secret: [u8; 32]) -> (Arc<NestClient>, [u8; 32]) {
    let actor = ActorKeypair::from_secret(secret).actor_id().0;
    let nest = connect(h, ActorKeypair::from_secret(secret)).await;
    // Mail provisioned — the MSEK rests in the actor's mail custody, which the
    // launcher reads it from (`mail_custody_of`).
    MAIL_SEATS.lock().unwrap().insert(actor);
    (nest, actor)
}

/// A minimal but genuine vCard. The searchable text the walk stages is the
/// *parsed* fields (`FN` / `NOTE` / …), never the raw vCard scaffolding, so a
/// term asserted on must be a value rather than a property name.
fn vcard(uid: &str, full_name: &str, note: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{full_name}\r\nNOTE:{note}\r\nEND:VCARD\r\n"
    )
}

/// MKCOL an address book, sealing its metadata exactly as the MDA does.
async fn provision_book(nest: &Arc<NestClient>, actor: [u8; 32], book: [u8; 32], name: &str) {
    let meta = fauna_client_carddav::seal_addressbook_metadata(
        &fauna_client_carddav::AddressbookMetadata {
            displayname: name.to_string(),
            description: String::new(),
        },
        &CONTACT_MSEK,
    )
    .expect("seal address-book metadata");
    let reply: fauna_protocol::bridge_routing::ProvisionAddressbookReply = nest
        .request(
            "fauna.bridges.provision_addressbook",
            fauna_protocol::bridge_routing::ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book.to_vec(),
                encrypted_metadata: meta,
                update_metadata: false,
            },
        )
        .await
        .expect("provision_addressbook");
    assert!(
        matches!(
            reply,
            fauna_protocol::bridge_routing::ProvisionAddressbookReply::Created
        ),
        "expected a fresh address book, got {reply:?}"
    );
}

/// PUT a card the way a CardDAV MUA's write lands through the MDA: the body
/// sealed to the actor's own MSEK-derived recipient key, keyed by `blake3(UID)`.
/// Returns that `uid_hash` — the doc identity the arm indexes under.
async fn put_card(
    nest: &Arc<NestClient>,
    actor: [u8; 32],
    book: [u8; 32],
    uid: &str,
    card: &str,
) -> [u8; 32] {
    let uid_hash = fauna_client_carddav::uid_hash(uid);
    // `ciphertext_size` meters the SEALED bytes, not the vCard — the nest refuses
    // the request outright when the two disagree, so the seal happens once and
    // both fields read off it.
    let body = fauna_client_carddav::seal_card_body(card.as_bytes(), &CONTACT_MSEK)
        .expect("seal card body");
    let reply: fauna_protocol::bridge_routing::PutCardCiphertextReply = nest
        .request(
            "fauna.bridges.put_card_ciphertext",
            fauna_protocol::bridge_routing::PutCardCiphertextRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book.to_vec(),
                uid_hash: uid_hash.to_vec(),
                ciphertext_size: body.len() as u32,
                encrypted_body: body,
                encrypted_index_hint: fauna_client_carddav::seal_card_body(
                    b"sealed-index-hint",
                    &CONTACT_MSEK,
                )
                .expect("seal index hint"),
                timestamp: 1_700_000_000,
                if_match: None,
                encrypted_fauna_ext: None,
            },
        )
        .await
        .expect("put_card_ciphertext");
    assert!(
        matches!(
            reply,
            fauna_protocol::bridge_routing::PutCardCiphertextReply::Created { .. }
                | fauna_protocol::bridge_routing::PutCardCiphertextReply::Updated { .. }
        ),
        "expected the card to land, got {reply:?}"
    );
    uid_hash
}

/// A MUA deletes the card outright — the state the DROPPED rule exists for.
async fn delete_card(nest: &Arc<NestClient>, actor: [u8; 32], book: [u8; 32], uid_hash: [u8; 32]) {
    let reply: fauna_protocol::bridge_routing::DeleteCardReply = nest
        .request(
            "fauna.bridges.delete_card",
            fauna_protocol::bridge_routing::DeleteCardRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book.to_vec(),
                uid_hash: uid_hash.to_vec(),
                if_match: None,
            },
        )
        .await
        .expect("delete_card");
    assert!(
        matches!(
            reply,
            fauna_protocol::bridge_routing::DeleteCardReply::Deleted { .. }
        ),
        "expected the card to be deleted, got {reply:?}"
    );
}

/// The launcher as app glue builds it — a `NestClient` in, an opaque index out,
/// no key material crossing the boundary (`conversations.md` § Architectural
/// rules #2). `None` for the lease seat is the supported uncoordinated shape.
///
/// It carries the shared-set resolver glue installs at every seat
/// ([`SetNonceResolver`]): the File arm judges each listed row before it reads
/// it, under the nonces that resolver answers, and with none it admits nothing.
fn launcher(nest: &Arc<NestClient>) -> Arc<fauna_client_conversations::NestMailIndexLauncher> {
    let launcher = fauna_client_conversations::NestMailIndexLauncher::new(
        nest.clone(),
        fauna_client_conversations::MailKeyCache::new(nest.clone(), mail_custody_of(nest)),
        None,
    );
    launcher.set_folder_key_resolver(Arc::new(SetNonceResolver));
    launcher
}

/// The shared-set resolver standing in for `fauna_client_folders::
/// NestFolderKeyResolver`: every set is positively owner-only and bound to
/// [`common::SET_NONCE`] — the nonce [`seed_folder`] stores and every seeded
/// row is signed under, so the reader seat's judge verifies them
/// (`writer-signed-change-records.md` ruling (3)).
struct SetNonceResolver;

#[async_trait::async_trait]
impl fauna_core::folder_keys::FolderKeyResolver for SetNonceResolver {
    async fn resolve(
        &self,
        _name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
        Ok(fauna_core::folder_keys::ResolvedCustody::owner_only())
    }

    async fn set_nonce(&self, _folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        Ok(Some(common::SET_NONCE))
    }
}

/// Stand the Search page up over a launcher-minted local arm — the query-side
/// half app glue wires, so a kind the launcher's own reader declines to claim
/// shows up here as no row rather than as a passing arm-level assert.
async fn contacts_page(
    nest: Arc<NestClient>,
    launcher: &Arc<fauna_client_conversations::NestMailIndexLauncher>,
) -> fauna_client_search::SearchManager<Arc<NestClient>> {
    page_over(nest, launcher, Arc::new(StubLookup::default())).await
}

/// The same, with a caller-supplied content lookup — needed wherever a *non*-
/// contact kind is on the page, since those kinds resolve through the lookup and
/// an empty one drops every one of their hits (the DROPPED rule doing its job).
async fn page_over(
    nest: Arc<NestClient>,
    launcher: &Arc<fauna_client_conversations::NestMailIndexLauncher>,
    lookup: Arc<StubLookup>,
) -> fauna_client_search::SearchManager<Arc<NestClient>> {
    let arm = launcher
        .local_search_index(lookup)
        .await
        .expect("the launcher mints a local arm for a seat with an identity");
    let manager = fauna_client_search::SearchManager::new(nest);
    manager.set_local_index(arm);
    manager
}

/// A named generous budget, not a guess about machine speed: the walk stages
/// synchronously inside `launch`, but the *publish* rides the flush debounce, so
/// what is waited on is a driver tick. A green run pays one poll interval; the
/// ceiling only decides how long a genuinely broken arm takes to say so
/// (convention 14).
const PAGE_BUDGET: Duration = Duration::from_secs(120);

/// Poll the page until `query` commits exactly `expected` local rows.
async fn local_rows_settle(
    manager: &fauna_client_search::SearchManager<Arc<NestClient>>,
    query: &str,
    expected: usize,
) -> Vec<fauna_client_search::SearchResultRow> {
    let deadline = tokio::time::Instant::now() + PAGE_BUDGET;
    loop {
        let last: Vec<_> = search_page(manager, query)
            .await
            .into_iter()
            .filter(|r| r.source == fauna_client_search::SearchSource::Local)
            .collect();
        if last.len() == expected {
            return last;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the page never settled on {expected} local row(s) for {query:?} within \
             {PAGE_BUDGET:?} — last saw {last:?}"
        );
        // sleep-ok: the poll interval of a deadline poll, not a settle wait.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// **The contacts arm's success clause, through the object app glue holds.** A
/// card that exists only in the nest-resident address book — put there by a
/// CardDAV MUA, never by any app — becomes a navigable local row on the Search
/// page, because the launcher walked the book at attach.
///
/// Nothing here touches an `IndexBuilder`: the producer under test is the walk
/// inside `launch()`, and the resolver under test is the one the launcher
/// attaches to its own reader.
#[tokio::test]
async fn a_card_in_the_address_book_is_a_navigable_local_row_on_the_search_page() {
    let h = start().await;
    let (nest, actor) = seat_with_mail(&h, [0x41u8; 32]).await;

    provision_book(&nest, actor, BOOK_ID, "Personal").await;
    let uid_hash = put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:ingrid",
        &vcard("urn:uuid:ingrid", "Ingrid Saltmarsh", "met at the harbour"),
    )
    .await;

    let launcher = launcher(&nest);
    launcher
        .launch()
        .await
        .expect("the launcher's arm container");
    let manager = contacts_page(nest, &launcher).await;

    let rows = local_rows_settle(&manager, "saltmarsh", 1).await;
    assert_eq!(
        rows[0].content_type, "contact",
        "the row must be attributed to the contact kind, got {rows:?}"
    );
    assert_eq!(
        rows[0].content_id,
        fauna_core::hex32::encode(&uid_hash),
        "the doc identity is the card's uid_hash, hex-LOWERCASE — the spelling \
         the nest's content_id conformance pin fixed for every id surface"
    );
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::Contact {
            uid_hash: fauna_core::hex32::encode(&uid_hash),
        }),
        "search.md § State & data shape: local rows always carry Some(navigation)"
    );
    assert!(
        rows[0].snippet.contains("Ingrid Saltmarsh"),
        "the snippet renders from the card's CURRENT fields, read at query time \
         — got {:?}",
        rows[0].snippet
    );
}

/// **Every book, one corpus.** A snapshot kind's staging retires every live
/// segment of the kind, so a walk that staged book-by-book would leave only the
/// last book searchable. Asserted on two books written before the walk ever
/// runs, which is the arrangement that distinguishes the two.
#[tokio::test]
async fn a_walk_stages_every_address_book_as_one_corpus() {
    let h = start().await;
    let (nest, actor) = seat_with_mail(&h, [0x42u8; 32]).await;

    provision_book(&nest, actor, BOOK_ID, "Personal").await;
    provision_book(&nest, actor, BOOK_ID_2, "Work").await;
    put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:ingrid",
        &vcard("urn:uuid:ingrid", "Ingrid Saltmarsh", "pemmican supplier"),
    )
    .await;
    put_card(
        &nest,
        actor,
        BOOK_ID_2,
        "urn:uuid:bjorn",
        &vcard("urn:uuid:bjorn", "Bjorn Kelp", "pemmican supplier"),
    )
    .await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest, &launcher).await;

    let rows = local_rows_settle(&manager, "pemmican", 2).await;
    let mut names: Vec<&str> = rows.iter().map(|r| r.snippet.as_str()).collect();
    names.sort_unstable();
    assert!(
        names[0].contains("Bjorn Kelp") && names[1].contains("Ingrid Saltmarsh"),
        "both books' cards must survive one corpus staging — got {names:?}"
    );
}

/// **An edit through a MUA reaches the page on the next sweep.** The card is
/// rewritten off-client (no app can write one), so the only thing that can carry
/// the change is the walk re-running — which `ensure_arm` does on every sweep,
/// change-suppressed by the ctag until there is something to carry.
///
/// Both halves are asserted: the new text is found **and** the old text stops
/// matching. The second is what separates a snapshot kind from an append-shaped
/// one — the segment holding the old text is no longer live, rather than an
/// upsert having quietly failed to cross it.
#[tokio::test]
async fn a_card_edited_through_a_mua_replaces_its_old_text_on_the_next_sweep() {
    let h = start().await;
    let (nest, actor) = seat_with_mail(&h, [0x43u8; 32]).await;

    provision_book(&nest, actor, BOOK_ID, "Personal").await;
    put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:ingrid",
        &vcard(
            "urn:uuid:ingrid",
            "Ingrid Saltmarsh",
            "pemmican for the crossing",
        ),
    )
    .await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;
    local_rows_settle(&manager, "pemmican", 1).await;

    // The MUA rewrites the card under the same UID — the in-place edit the
    // `uid_hash` identity exists for.
    put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:ingrid",
        &vcard(
            "urn:uuid:ingrid",
            "Ingrid Saltmarsh",
            "biltong for the crossing",
        ),
    )
    .await;
    // A sweep of any master kind re-runs the walk — the production trigger, not
    // a test-only door.
    launcher.ensure_arm(IndexableKind::Conversation).await;

    local_rows_settle(&manager, "biltong", 1).await;
    local_rows_settle(&manager, "pemmican", 0).await;
}

/// **A card deleted since the flush is DROPPED, not rendered inert.** No walk
/// runs here on purpose: the index still holds the doc, and the resolver — one
/// `query_cards` sweep at query time — no longer sees the card. That transient
/// state is what the drop rule is for (`ui/search.md` § State & data shape → *An
/// unresolvable local hit is DROPPED*), and it is also the arm's whole answer to
/// deletion, which has no other hook.
#[tokio::test]
async fn a_card_deleted_since_the_flush_is_dropped_from_the_page() {
    let h = start().await;
    let (nest, actor) = seat_with_mail(&h, [0x44u8; 32]).await;

    provision_book(&nest, actor, BOOK_ID, "Personal").await;
    put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:ingrid",
        &vcard(
            "urn:uuid:ingrid",
            "Ingrid Saltmarsh",
            "pemmican for the crossing",
        ),
    )
    .await;
    let doomed = put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:bjorn",
        &vcard("urn:uuid:bjorn", "Bjorn Kelp", "pemmican inventory"),
    )
    .await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;
    local_rows_settle(&manager, "pemmican", 2).await;

    delete_card(&nest, actor, BOOK_ID, doomed).await;

    // No sweep, no re-walk: the index is deliberately stale here.
    let rows = local_rows_settle(&manager, "pemmican", 1).await;
    assert!(
        rows[0].snippet.contains("Ingrid Saltmarsh"),
        "the surviving card is the one the book still holds — got {rows:?}"
    );
}

/// Every live segment of `kind` the master manifest points at, read the way a
/// second device would: fetch the published manifest off the rail and open it
/// with the key derived from this actor's own identity seed.
async fn live_master_segments(
    h: &Harness,
    publisher: &IndexRailPublisher,
    secret: &[u8; 32],
    kind: ContentKind,
) -> Vec<u32> {
    let listed = publisher.list().await.expect("fauna.index.list");
    let Some(entry) = listed
        .entries
        .iter()
        .find(|e| e.path == fauna_index::manifest_path())
    else {
        return Vec::new();
    };
    let bytes = fetch(h, &entry.blob_hash).await;
    let key = IndexMasterKey::from_bytes(*fauna_core::crypto::derive_index_master_key(
        ActorKeypair::from_secret(*secret).secret_bytes(),
    ));
    let manifest = IndexManifest::from_sealed_bytes(&bytes, &key).expect("open master manifest");
    manifest
        .kind(kind)
        .map(|k| k.live_segments.clone())
        .unwrap_or_default()
}

/// The contacts flavor of [`live_master_segments`].
async fn live_contact_segments(
    h: &Harness,
    publisher: &IndexRailPublisher,
    secret: &[u8; 32],
) -> Vec<u32> {
    live_master_segments(h, publisher, secret, ContentKind::Contact).await
}

/// **The ctag precheck, across two launches — the one clause with no unit-level
/// twin.** A relaunch over an unchanged address book must publish no new
/// segment: the snapshot path keeps no cross-launch re-index guard, so without
/// the precheck every launch would re-stage an identical corpus, tombstone the
/// live segment and append a fresh one, forever, on a rail whose only
/// reclamation is tombstoning.
///
/// The negative assert is anchored to a **causal barrier**, not a settle-sleep
/// (convention 14): the second launcher's walk runs to completion inside
/// `launch()`, and the draft corpus staged afterwards pulses the same flush
/// driver — so once the draft's own segment is published, any contacts corpus
/// that walk had staged would have ridden the same flush. Waiting on the draft
/// is therefore proof that a missing contacts publish is missing, not merely
/// late.
#[tokio::test]
async fn a_relaunch_over_an_unchanged_address_book_publishes_no_new_segment() {
    let h = start().await;
    const SEED: [u8; 32] = [0x45u8; 32];
    let (nest, actor) = seat_with_mail(&h, SEED).await;

    provision_book(&nest, actor, BOOK_ID, "Personal").await;
    put_card(
        &nest,
        actor,
        BOOK_ID,
        "urn:uuid:ingrid",
        &vcard(
            "urn:uuid:ingrid",
            "Ingrid Saltmarsh",
            "pemmican for the crossing",
        ),
    )
    .await;

    let first = launcher(&nest);
    first.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &first).await;
    local_rows_settle(&manager, "pemmican", 1).await;

    let publisher = IndexRailPublisher::new(nest.clone());
    let before = live_contact_segments(&h, &publisher, &SEED).await;
    assert_eq!(
        before.len(),
        1,
        "the first launch publishes exactly one contacts segment, got {before:?}"
    );

    // The app restarts: a fresh launcher over the same actor, same nest, same
    // unchanged book.
    let second = launcher(&nest);
    let arms = second.launch().await.expect("arm container");
    // The barrier: stage a draft through the observer the launcher just handed
    // back and wait for it on the page. It pulses the same driver the walk does,
    // so its publish cannot precede a contacts publish the walk had queued.
    //
    // The lookup must hold the draft — a draft resolves through it, and an empty
    // one would drop the row and turn this barrier into a 120-second timeout.
    arms.observe_draft_corpus(&draft_corpus(&[("thread-barrier", "kelp for the barrier")]));
    let lookup = Arc::new(StubLookup::default());
    lookup.holds_draft(
        "thread-barrier",
        Some("thread-barrier"),
        "kelp for the barrier",
    );
    let barrier_page = page_over(nest.clone(), &second, lookup).await;
    local_rows_settle(&barrier_page, "kelp", 1).await;

    let after = live_contact_segments(&h, &publisher, &SEED).await;
    assert_eq!(
        after, before,
        "an unchanged address book must publish nothing on relaunch — the ctag \
         precheck is the only thing standing between a snapshot kind and a \
         republish on every single launch"
    );
}

// ── The posts arm, driven by the REAL LAUNCHER ────────────────────────────────
//
// The append-shaped third-class kind (`content-index.md` § Ingest triggers, v1
// — the posts ruling): the producer is the launcher's own `fauna.posts.list`
// reconcile walk plus the create-time trickle, and the resolver is a per-hit
// `fauna.posts.get`. Same section discipline as the contacts tests above —
// fixtures write through the wire (the *same* `posts_create` every composer
// submits through), every assertion comes back through the Search page.
//
// No mail is provisioned anywhere here, deliberately: posts are the first
// third-class kind on the identity seed alone, so a test that needed an MSEK
// would be proving the wrong gate.

/// Create a post over the real wire the way every app's composer does — built +
/// signed by the shared `build_post`, submitted over `fauna.posts.create` —
/// and hand back the nest-echoed hex id the index will carry.
async fn create_post(nest: &Arc<NestClient>, secret: [u8; 32], text: &str) -> String {
    let kp = ActorKeypair::from_secret(secret);
    let bytes = fauna_client_core::post::build_post(&kp, text, &[], None).expect("build post");
    fauna_client_posts::PostsClient::new(nest.clone())
        .posts_create(bytes)
        .await
        .expect("fauna.posts.create")
        .post_id
}

/// Delete the caller's own post — the signed `Tombstone` shape
/// `FeedManager::delete_post` builds.
async fn wire_delete_post(nest: &Arc<NestClient>, secret: [u8; 32], post_id: &str) {
    let digest = fauna_core::hex32::decode(post_id).expect("hex post id");
    let kp = ActorKeypair::from_secret(secret);
    let tombstone = fauna_core::data::Tombstone {
        author: kp.actor_id(),
        post_id: fauna_core::data::PostId::from_digest_dag_cbor(digest),
        created_at: fauna_core::data::Timestamp::now(),
    };
    let bytes = fauna_core::encoding::sign_and_pack(&kp, &tombstone).expect("sign tombstone");
    let reply = fauna_client_posts::PostsClient::new(nest.clone())
        .posts_delete(bytes)
        .await
        .expect("fauna.posts.delete");
    assert!(reply.deleted, "the fixture post should have newly deleted");
}

/// **The posts arm's catch-up clause: a post that predates the arm is found.**
/// The corpus lives nest-side and the seat never saw the create — only the
/// attach-time walk over `fauna.posts.list` can index it, which is the whole
/// reason the enumeration kind exists (the refuted from-now-on corpus).
///
/// The same rows also assert the **twin dedup end to end**: `posts_create` put
/// this post into backend 1 (`content_fts`) too, so the page holds a local row
/// and a nest row for one post — and must show ONE row, local winning, because
/// both spell the id identically (`ui/search.md` § The page's wire surface).
#[tokio::test]
async fn a_pre_existing_post_is_a_navigable_local_row_after_the_attach_walk() {
    let h = start().await;
    const SEED: [u8; 32] = [0x46u8; 32];
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    let post_id = create_post(&nest, SEED, "the albatross crossed the meridian").await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest, &launcher).await;

    let rows = local_rows_settle(&manager, "albatross", 1).await;
    assert_eq!(rows[0].content_type, "post");
    assert_eq!(
        rows[0].content_id, post_id,
        "the doc identity is the nest-echoed post id, hex-lowercase"
    );
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::Post {
            post_id: post_id.clone(),
        }),
        "search.md § State & data shape: local rows always carry Some(navigation)"
    );
    assert!(
        rows[0].snippet.contains("albatross"),
        "the snippet renders from the post's CURRENT text, read at query time — \
         got {:?}",
        rows[0].snippet
    );

    // The dedup half: the whole page — both arms — shows exactly one row for
    // this post, and it is the local one.
    let all = search_page(&manager, "albatross").await;
    assert_eq!(
        all.len(),
        1,
        "backend 1 holds this post too; the (kind class, id) dedup must fold \
         the twins into one — got {all:?}"
    );
    assert_eq!(all[0].source, fauna_client_search::SearchSource::Local);
}

/// **The trickle clause: a just-composed post is searchable without any walk.**
/// The launcher attached over an *empty* corpus, so the walk indexed nothing
/// and no sweep runs afterwards — the only path from the create to the page is
/// `own_post_observer`, the object glue hands `FeedManager` (whose
/// confirmed-create → observer call is pinned in `fauna-feed`'s own tests).
#[tokio::test]
async fn a_composed_post_reaches_the_page_through_the_trickle_without_a_walk() {
    let h = start().await;
    const SEED: [u8; 32] = [0x47u8; 32];
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;

    let post_id = create_post(&nest, SEED, "provisions for the long haul").await;
    launcher
        .own_post_observer()
        .own_post_created(&post_id, "provisions for the long haul");

    let rows = local_rows_settle(&manager, "provisions", 1).await;
    assert_eq!(rows[0].content_type, "post");
    assert_eq!(rows[0].content_id, post_id);
}

/// **Deletion is display-healed, at wire distance.** The index still holds the
/// doc — an append arm cannot remove it — and no walk runs between the delete
/// and the query, so the drop can only come from the resolver's
/// `fauna.posts.get` answering `not_found` (`content-index.md` § Ingest
/// triggers, v1: do not invent a delete hook).
#[tokio::test]
async fn a_deleted_post_is_dropped_from_the_page_at_resolve() {
    let h = start().await;
    const SEED: [u8; 32] = [0x48u8; 32];
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    create_post(&nest, SEED, "pemmican inventory for the crossing").await;
    let doomed = create_post(&nest, SEED, "pemmican rations, second sledge").await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;
    local_rows_settle(&manager, "pemmican", 2).await;

    wire_delete_post(&nest, SEED, &doomed).await;

    // No sweep, no re-walk: the index is deliberately stale here.
    let rows = local_rows_settle(&manager, "pemmican", 1).await;
    assert!(
        rows[0].snippet.contains("inventory"),
        "the surviving row is the post the nest still serves — got {rows:?}"
    );
}

/// **The marker precheck, across two launches.** An append kind's guard would
/// stop a *republish* of known docs, but only the marker stops the walk
/// re-paging the whole enumeration and — on a corpus whose tail the guard has
/// not seen — publishing an identical segment chain again. Asserted exactly
/// like the contacts twin: the second launcher's walk completes inside
/// `launch()`, the draft corpus staged afterwards is the causal barrier, and
/// once the draft's segment is on the page any posts publish that walk had
/// staged would have ridden the same flush.
#[tokio::test]
async fn a_relaunch_over_an_unchanged_posts_corpus_publishes_no_new_segment() {
    let h = start().await;
    const SEED: [u8; 32] = [0x49u8; 32];
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    create_post(&nest, SEED, "the meridian log, day one").await;
    create_post(&nest, SEED, "the meridian log, day two").await;

    let first = launcher(&nest);
    first.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &first).await;
    local_rows_settle(&manager, "meridian", 2).await;

    let publisher = IndexRailPublisher::new(nest.clone());
    let before = live_master_segments(&h, &publisher, &SEED, ContentKind::Post).await;
    assert!(
        !before.is_empty(),
        "the first launch publishes the posts segment(s), got {before:?}"
    );

    // The app restarts: a fresh launcher, same actor, unchanged corpus.
    let second = launcher(&nest);
    let arms = second.launch().await.expect("arm container");
    arms.observe_draft_corpus(&draft_corpus(&[("thread-barrier", "kelp for the barrier")]));
    let lookup = Arc::new(StubLookup::default());
    lookup.holds_draft(
        "thread-barrier",
        Some("thread-barrier"),
        "kelp for the barrier",
    );
    let barrier_page = page_over(nest.clone(), &second, lookup).await;
    local_rows_settle(&barrier_page, "kelp", 1).await;

    let after = live_master_segments(&h, &publisher, &SEED, ContentKind::Post).await;
    assert_eq!(
        after, before,
        "an unchanged posts corpus must publish nothing on relaunch — the \
         marker precheck is what keeps the per-sweep walk cadence to one page \
         RPC and the rail free of re-published chains"
    );
}

// ── the File arm, end to end ───────────────────────────────────────────────
//
// The third ingest class's second append-shaped instance (`content-index.md`
// § Ingest triggers, v1 → *The files/media arms are SCOPED*). Like posts and
// unlike contacts it needs no MSEK — a file *name* seals to its set's download
// keys, whose owner root derives from the identity seed every logged-in actor
// has — so these seats connect with `connect`, not `seat_with_mail`.
//
// Files are seeded through the DB the way `conformance_media_list.rs` does
// rather than over a client upload path: what is under test is the walk, the
// resolver and the identity join reading `fauna.media.list` over the **real
// wire**, and the sync-agent record path that puts a row there is another
// subsystem's contract with its own tests.

/// Create a folder for `actor` bound to [`common::SET_NONCE`] and hand back its
/// stable row id — the durable half of a File doc's identity. The nonce is what
/// a signed row's statement is verified under: the owner's create sends it, and
/// this fixture creates the row directly, so it stores it the way the owner's
/// update would.
async fn seed_folder(h: &Harness, actor: &[u8; 32], name: &str) -> i64 {
    let id =
        h.db.create_folder(name, actor)
            .await
            .expect("create folder");
    h.db.update_folder_for_user(
        name,
        actor,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&common::SET_NONCE),
            ..Default::default()
        },
    )
    .await
    .expect("store the set nonce");
    id
}

/// Seed one non-deleted file into `folder_id`, mirroring a recorded `create`
/// change, and hand back the `path_hash` hex the index will key on.
async fn seed_file(h: &Harness, writer: &ActorKeypair, folder_id: i64, path: &str) -> String {
    let manifest: [u8; 32] = *blake3::hash(format!("manifest:{path}").as_bytes()).as_bytes();
    let path_hash = seed_change(h, writer, folder_id, path, Some(&manifest), 1_024, "create").await;
    fauna_core::hex32::encode(&path_hash)
}

/// Tombstone a seeded file — the `delete` change a sync agent records when the
/// file leaves the set, which is also what a **rename** produces for the old
/// name (the engine's watcher resolves renames by on-disk existence).
async fn seed_file_delete(h: &Harness, writer: &ActorKeypair, folder_id: i64, path: &str) {
    seed_change(h, writer, folder_id, path, None, 0, "delete").await;
}

/// Store one change row signed directly by `writer` under [`common::SET_NONCE`]
/// — the statement a writer engine signs (`common::sign_record`). The File arm
/// judges every listed row before it reads it (`writer-signed-change-records.md`
/// ruling (3)), so an unsigned row would be held and never staged.
async fn seed_change(
    h: &Harness,
    writer: &ActorKeypair,
    folder_id: i64,
    path: &str,
    manifest: Option<&[u8; 32]>,
    size_bytes: i64,
    change_type: &str,
) -> [u8; 32] {
    const DEVICE: [u8; 32] = [0xd1u8; 32];
    let mut req = fauna_protocol::sync::SyncChangeRecordRequest {
        device_id: fauna_core::hex32::encode(&DEVICE),
        path: path.into(),
        manifest_hash: manifest.map(fauna_core::hex32::encode),
        size_bytes,
        change_type: change_type.into(),
        ..Default::default()
    };
    common::sign_record(&mut req, writer, common::SET_NONCE);
    let actor = writer.actor_id().0;
    let path_hash = fauna_core::sync::path_hash(path);
    h.db.record_sync_change_metered_signed(
        &actor,
        &actor,
        None,
        &path_hash,
        manifest,
        size_bytes,
        change_type,
        folder_id,
        &DEVICE,
        Some(path),
        None,
        None,
        None,
        None,
        None,
        i64::MAX,
        Some(fauna_nest::db::RowSignature {
            signature: req.signature.as_deref().expect("signed"),
            signer_key: req.signer_key.as_deref().expect("signed"),
        }),
    )
    .await
    .expect("record the change");
    path_hash
}

/// **The File arm's success clause, through the object app glue holds.** A file
/// that exists only in the nest-resident listing — put there by a sync agent,
/// another device, or another member of a shared set, never by this app —
/// becomes a navigable local row on the Search page, because the launcher
/// drained `fauna.media.list` at attach.
///
/// The navigation assertion is the one with teeth: it must carry the set's
/// **stable row id**, not its renameable name, and the `path_hash` the wire
/// row carried.
#[tokio::test]
async fn a_file_in_a_set_is_a_navigable_local_row_on_the_search_page() {
    let h = start().await;
    const SEED: [u8; 32] = [0x4au8; 32];
    let kp = ActorKeypair::from_secret(SEED);
    let actor = kp.actor_id().0;
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    let set = seed_folder(&h, &actor, "photos").await;
    let path_hash = seed_file(&h, &kp, set, "holidays/albatross.jpg").await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;

    // The bare stem — what a user actually types. The path alone would not
    // match it, which is why `file_doc_for` derives the stem (pinned in
    // `fauna-client-index`).
    let rows = local_rows_settle(&manager, "albatross", 1).await;
    assert_eq!(rows[0].content_type, "file");
    assert_eq!(
        rows[0].content_id,
        format!("{set}:{path_hash}"),
        "the doc identity is the (stable set id, path_hash) pair"
    );
    assert_eq!(
        rows[0].navigation,
        Some(SearchNav::File {
            folder_id: set,
            path_hash: path_hash.clone(),
        }),
        "search.md § State & data shape: local rows always carry Some(navigation) \
         — and it must be the DURABLE pair, never the renameable set name"
    );
    assert_eq!(
        rows[0].snippet, "holidays/albatross.jpg",
        "the snippet is the path the resolver read at query time — got {:?}",
        rows[0].snippet
    );
}

/// The corpus is **cross-set**, which is what makes `fauna.media.list` the door
/// rather than the per-set `fauna.sync.files`: one drain covers every readable
/// set, and two sets holding the same relative path are two documents.
#[tokio::test]
async fn one_walk_covers_every_set_and_keeps_same_named_files_distinct() {
    let h = start().await;
    const SEED: [u8; 32] = [0x4bu8; 32];
    let kp = ActorKeypair::from_secret(SEED);
    let actor = kp.actor_id().0;
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    let photos = seed_folder(&h, &actor, "photos").await;
    let backups = seed_folder(&h, &actor, "backups").await;
    seed_file(&h, &kp, photos, "meridian.txt").await;
    seed_file(&h, &kp, backups, "meridian.txt").await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;

    let rows = local_rows_settle(&manager, "meridian", 2).await;
    let mut sets: Vec<i64> = rows
        .iter()
        .filter_map(|r| match &r.navigation {
            Some(SearchNav::File { folder_id, .. }) => Some(*folder_id),
            _ => None,
        })
        .collect();
    sets.sort();
    let mut expected = vec![photos, backups];
    expected.sort();
    assert_eq!(
        sets, expected,
        "one file per set — keying identity on path_hash alone would guard the \
         second set's file away as already-indexed"
    );
}

/// **Deletion is display-healed, at wire distance — and so is a rename**, which
/// is structurally delete + create for the old name. The index still holds the
/// doc (an append arm cannot remove it) and no walk runs between the delete and
/// the query, so the drop can only come from the resolver's drain no longer
/// listing the row.
#[tokio::test]
async fn a_deleted_file_is_dropped_from_the_page_at_resolve() {
    let h = start().await;
    const SEED: [u8; 32] = [0x4cu8; 32];
    let kp = ActorKeypair::from_secret(SEED);
    let actor = kp.actor_id().0;
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    let set = seed_folder(&h, &actor, "photos").await;
    seed_file(&h, &kp, set, "pemmican-inventory.txt").await;
    seed_file(&h, &kp, set, "pemmican-rations.txt").await;

    let launcher = launcher(&nest);
    launcher.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &launcher).await;
    local_rows_settle(&manager, "pemmican", 2).await;

    seed_file_delete(&h, &kp, set, "pemmican-rations.txt").await;

    // No sweep, no re-walk: the index is deliberately stale here.
    let rows = local_rows_settle(&manager, "pemmican", 1).await;
    assert!(
        rows[0].snippet.contains("inventory"),
        "the surviving row is the file the nest still lists — got {rows:?}"
    );
}

/// **The no-marker suppression, across two launches.** File keeps no corpus
/// marker, so this is what proves the ruled claim that the stage-time guard
/// alone is enough: a relaunch re-drains the whole listing and must publish
/// nothing, because every id it re-encounters is already indexed.
///
/// Asserted exactly like its posts and contacts twins — the second launcher's
/// walk completes inside `launch()`, and the draft corpus staged afterwards is
/// the causal barrier: once the draft's segment is on the page, any files
/// publish that walk had staged would have ridden the same flush.
#[tokio::test]
async fn a_relaunch_over_an_unchanged_file_corpus_publishes_no_new_segment() {
    let h = start().await;
    const SEED: [u8; 32] = [0x4du8; 32];
    let kp = ActorKeypair::from_secret(SEED);
    let actor = kp.actor_id().0;
    let nest = connect(&h, ActorKeypair::from_secret(SEED)).await;

    let set = seed_folder(&h, &actor, "logs").await;
    seed_file(&h, &kp, set, "meridian-day-one.txt").await;
    seed_file(&h, &kp, set, "meridian-day-two.txt").await;

    let first = launcher(&nest);
    first.launch().await.expect("arm container");
    let manager = contacts_page(nest.clone(), &first).await;
    local_rows_settle(&manager, "meridian", 2).await;

    let publisher = IndexRailPublisher::new(nest.clone());
    let before = live_master_segments(&h, &publisher, &SEED, ContentKind::File).await;
    assert!(
        !before.is_empty(),
        "the first launch publishes the files segment(s), got {before:?}"
    );

    // The app restarts: a fresh launcher, same actor, unchanged corpus.
    let second = launcher(&nest);
    let arms = second.launch().await.expect("arm container");
    arms.observe_draft_corpus(&draft_corpus(&[("thread-barrier", "kelp for the barrier")]));
    let lookup = Arc::new(StubLookup::default());
    lookup.holds_draft(
        "thread-barrier",
        Some("thread-barrier"),
        "kelp for the barrier",
    );
    let barrier_page = page_over(nest.clone(), &second, lookup).await;
    local_rows_settle(&barrier_page, "kelp", 1).await;

    let after = live_master_segments(&h, &publisher, &SEED, ContentKind::File).await;
    assert_eq!(
        after, before,
        "an unchanged file corpus must publish nothing on relaunch — for an \
         append kind the stage-time guard IS the ruled suppression, which is \
         why this arm carries no corpus marker"
    );
}

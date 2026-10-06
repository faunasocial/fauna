//! **Mailbox export — client seals → real wire → nest disk → download → client
//! opens** (tier_3). The end-to-end proof that the shared-Rust export
//! serializers, the frame codec, the nest's twelve `fauna.bridges.*export*`
//! handlers and the `GET /api/v1/export/<id>` route actually compose.
//!
//! Production data flow asserted end-to-end (real WS-RPC over a real socket,
//! real SQLite, real files, real HTTP download):
//!
//!   the user's client lists its own mailboxes (`fauna.bridges.list_own_mailboxes`)
//!   → serializes a corpus through the *same* `fauna_mail::export` functions the
//!   wizard will (`serialize_all` → `build_blob`, a deterministic zip inside one
//!   zstd stream) → opens a session (`start_export_session`, carrying the
//!   client-wrapped per-session key) → seals successive byte slices of that one
//!   stream with `ExportBlobSealer` and pushes them up (`upload_export_chunk`)
//!   → finalizes (`finalize_export_session`) → GETs the returned
//!   `download_url` with its actor bearer → opens the downloaded bytes with
//!   `ExportBlobOpener` and recovers **byte-for-byte the `.zip.zst` it sealed**.
//!
//! What this catches that nothing else does:
//!
//! - Over `bridge_export_handlers`' in-process handler tests (which dispatch
//!   handler closures directly): the **router, the caller-class allowlist gate
//!   and the CBOR round-trip of every export type**. A field the wire drops, a
//!   kind missing from the live router, a reply shape the client cannot decode
//!   — each passes an in-process dispatch and fails here.
//! - Over `export::seal`'s unit tests (which seal and open in one process
//!   without a nest between): that the nest's **opaque append preserves the
//!   frame boundaries** the opener needs. The nest parses no frame, so nothing
//!   nest-side can notice if it reordered, padded or coalesced them — only a
//!   round trip through real storage and a real download can.
//! - Over `mail_export_reclaim.rs` (which plants fixture blob files by hand,
//!   because no blob writer existed when it was written): the blob is now
//!   written by the production path.
//!
//! The one thing it deliberately does **not** claim: that the *wizard* drives
//! this. The shared drive loop (`MailExportMachine::run_export`) has its own
//! unit tests over an in-memory nest; this test plays the client by hand — but
//! through the production functions, never a test-local re-derivation, which is
//! what makes the composition claim real rather than circular.
//!
//! The second test is § Resume's **cold resume** over the same real stack: a
//! stream abandoned mid-upload, `restart_export_session` from a second
//! connection, the abandoned driver shut out by its stale generation, and a
//! download that opens — under the restart's key only — to exactly the archive,
//! with not one frame of the abandoned stream in it.
//!
//! The third test is § Resume's **client-callable fail kind**: a driver reports
//! a session-fatal condition, the partial blob goes the way a cancel's does,
//! and the *record* of why is read back by a second client of the same user on
//! its own connection — which is the whole thing the cancel it replaced could
//! not do.
//!
//! Tier: tier_3 (real in-process nest + real CacheDb + a real tempdir data dir
//! — no mocks anywhere in the path).

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_mail::export::{
    ExportBlobOpener, ExportBlobSealer, ExportFormat, ExportMessage, ExportOptions, build_blob,
    serialize_all,
};
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::bridge_routing::{
    DiscardExportBlobReply, EXPORT_STREAM_SUPERSEDED, ExportSessionActionReply,
    ExportSessionActionRequest, FailExportSessionRequest, FetchExportChunkCiphertextReply,
    FetchExportChunkCiphertextRequest, ListExportSessionsReply, ListExportSessionsRequest,
    ListOwnMailboxesReply, ListOwnMailboxesRequest, RestartExportSessionRequest,
    StartExportSessionReply, StartExportSessionRequest, UploadExportChunkReply,
    UploadExportChunkRequest,
};

/// The exporting user's identity seed.
const USER_SEED: [u8; 32] = [0xE7; 32];
/// A second actor, to prove § Cross-actor isolation reaches the download route
/// and not only the RPC surface.
const OTHER_SEED: [u8; 32] = [0x0B; 32];

/// The per-session key § Key material has the client mint. Fixed here so the
/// test is deterministic; in production it is random per session and the nest
/// only ever sees the wrapped form.
const SESSION_KEY: [u8; 32] = [0x5A; 32];

/// The corpus the client serializes, **already in § Container shape's total
/// order**: mailbox name ascending by raw bytes, then INTERNALDATE, then UID.
/// `Archive` therefore precedes `INBOX` — the serializer refuses an out-of-
/// order push outright (`ExportError::OutOfOrder`), which is what makes the
/// archive byte-identical for the same input, and is why the drive loop must
/// walk the mailboxes in the order `list_own_mailboxes` returns them.
///
/// Two mailboxes and three flag shapes, so the ordering rule and the mbox
/// `Status:` line both have something to carry.
fn corpus() -> Vec<ExportMessage> {
    vec![
        ExportMessage {
            mailbox: "Archive".into(),
            flags: vec!["\\Flagged".into()],
            body: b"From: c@example.com\r\nSubject: third\r\n\r\nkept\r\n".to_vec(),
            internal_date_epoch: 1_700_000_200,
            uid: 7,
            uid_validity: 9,
        },
        ExportMessage {
            mailbox: "INBOX".into(),
            flags: vec!["\\Seen".into()],
            body: b"From: a@example.com\r\nSubject: first\r\n\r\nhello\r\n".to_vec(),
            internal_date_epoch: 1_700_000_000,
            uid: 1,
            uid_validity: 9,
        },
        ExportMessage {
            mailbox: "INBOX".into(),
            flags: vec![],
            body: b"From: b@example.com\r\nSubject: second\r\n\r\nworld\r\n".to_vec(),
            internal_date_epoch: 1_700_000_100,
            uid: 2,
            uid_validity: 9,
        },
    ]
}

// ─────────────────────────────── the nest ────────────────────────────────────

/// A real in-process nest serving what an export drives: auth bootstrap,
/// discovery, the export kinds, and the HTTP download route that
/// `build_router` wires.
///
/// `db_path` points into a tempdir, so the data dir the blob writer derives is
/// one this test owns — the bare `AppState::for_test` has no data dir at all,
/// which is precisely the state in which an upload has nowhere to write.
async fn start_export_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("nest.db").to_string_lossy().into_owned();

    let base = AppState::for_test(db);
    let config = Arc::new(fauna_nest::config::NestConfig {
        nest: fauna_nest::config::NestSection {
            db_path,
            ..base.config.nest.clone()
        },
        ..(*base.config).clone()
    });

    let state = Arc::new(AppState {
        config,
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::bridge_export_handlers::register_bridge_export_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        ..base
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, dir)
}

// ───────────────────────────────── the test ──────────────────────────────────

#[tokio::test]
async fn a_client_sealed_export_survives_the_wire_the_disk_and_the_download() {
    let (base, state, _dir) = start_export_nest().await;

    // ── Fixture precondition: two registered mail-serving users. In production
    // "enable mail" provisions this.
    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "exporter")
        .await
        .unwrap();
    common::seed_recipient_seal_key(&state.db, &user_id, &common::FIXTURE_MSEK).await;
    let other_id = ActorKeypair::from_secret(OTHER_SEED).actor_id().0;
    state
        .db
        .create_user(&other_id, "free", "bystander")
        .await
        .unwrap();
    common::seed_recipient_seal_key(&state.db, &other_id, &common::FIXTURE_MSEK).await;

    let nest = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;

    // ── Step 2 of the wizard: the caller's own mailboxes, over the real wire.
    let mailboxes: ListOwnMailboxesReply = nest
        .request(
            "fauna.bridges.list_own_mailboxes",
            ListOwnMailboxesRequest {},
        )
        .await
        .expect("list_own_mailboxes RPC");
    assert!(
        mailboxes.mailboxes.iter().any(|m| m.name == "INBOX"),
        "a mail-serving actor always has INBOX, got {:?}",
        mailboxes.mailboxes
    );
    let names: Vec<&str> = mailboxes
        .mailboxes
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(names, sorted, "the scope step's options come byte-ordered");

    // ── The client half: serialize the corpus and compress it, through the
    // production functions. This is the artifact the user ends up holding.
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("exporter"),
        &corpus(),
    )
    .expect("serialize the corpus");
    let archive = build_blob(&entries).expect("zip inside zstd");
    assert!(!archive.is_empty());

    // ── Step 3: open the session. The wrapped key is opaque to the nest — it
    // stores it verbatim so any of the user's clients can open the download.
    let wrapped_key = b"wrapped-under-the-actor-key".to_vec();
    let started: StartExportSessionReply = nest
        .request(
            "fauna.bridges.start_export_session",
            StartExportSessionRequest {
                format: "mbox".into(),
                scope_descriptor: b"cbor-scope-descriptor".to_vec(),
                wrapped_session_key: Some(serde_bytes::ByteBuf::from(wrapped_key.clone())),
                total_count: corpus().len() as u64,
                ..Default::default()
            },
        )
        .await
        .expect("start_export_session RPC");
    let sid = started.session_id;

    // The down-leg answers over the real wire even with nothing to serve — the
    // walk's shape is what matters here; the message-bearing path is covered by
    // the handler tests, and the drive loop that consumes it is unbuilt.
    let page: FetchExportChunkCiphertextReply = nest
        .request(
            "fauna.bridges.fetch_export_chunk_ciphertext",
            FetchExportChunkCiphertextRequest {
                session_id: sid.clone(),
                mailbox: "INBOX".into(),
                after_uid: 0,
                max_messages: 0,
                max_bytes: 0,
                ..Default::default()
            },
        )
        .await
        .expect("fetch_export_chunk_ciphertext RPC");
    assert!(page.messages.is_empty());
    assert!(page.mailbox_done);

    // ── Step 4: seal the archive into frames and push them up. The split is
    // deliberately uneven and mid-stream — a chunk is a byte SLICE of the one
    // zstd stream (§ Container shape), never an independently-compressed unit,
    // so the boundary has to be able to fall anywhere.
    let mut sealer = ExportBlobSealer::new(&SESSION_KEY, &sid, "mbox").expect("sealer");
    let cut = archive.len() / 3;
    let slices: Vec<&[u8]> = vec![&archive[..cut], &archive[cut..]];

    let mut uploaded = 0u64;
    for (i, slice) in slices.iter().enumerate() {
        let mut frame = Vec::new();
        if i == 0 {
            // The preamble rides chunk 0's bytes: the nest appends what it is
            // given and knows nothing about a header.
            frame.extend_from_slice(&sealer.preamble());
        }
        frame.extend_from_slice(&sealer.seal_chunk(slice).expect("seal a chunk"));
        let reply: UploadExportChunkReply = nest
            .request(
                "fauna.bridges.upload_export_chunk",
                UploadExportChunkRequest {
                    session_id: sid.clone(),
                    chunk_idx: i as u64,
                    sealed_chunk: frame.clone(),
                    exported_delta: 1,
                    skipped_delta: 0,
                    errored_delta: 0,
                    last_processed_message_id: format!("m{i}"),
                    ..Default::default()
                },
            )
            .await
            .expect("upload_export_chunk RPC");
        uploaded += frame.len() as u64;
        assert_eq!(reply.blob_bytes, uploaded);
        assert_eq!(reply.next_chunk_idx, i as u64 + 1);
    }

    // The terminator frame — § Blob shape on disk's commitment to the blob's
    // length, uploaded as an ordinary chunk because the nest cannot tell one
    // from a body frame.
    let terminator = sealer.finish().expect("terminator frame");
    let final_upload: UploadExportChunkReply = nest
        .request(
            "fauna.bridges.upload_export_chunk",
            UploadExportChunkRequest {
                session_id: sid.clone(),
                chunk_idx: slices.len() as u64,
                sealed_chunk: terminator.clone(),
                ..Default::default()
            },
        )
        .await
        .expect("upload the terminator");
    let blob_bytes = final_upload.blob_bytes;

    // ── Before finalize: § Architectural rules' no-partial-blob-download rule,
    // enforced at the route and not merely implied by the reply omitting a URL.
    let bearer = state.auth.token_store.insert(user.actor_id(), 3600).await;
    let http = reqwest::Client::new();
    let early = http
        .get(format!("{base}/api/v1/export/{sid}"))
        .bearer_auth(&bearer)
        .send()
        .await
        .expect("GET a running session's blob");
    assert_eq!(
        early.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a running session must not serve its partial blob"
    );

    // ── Step 5: finalize.
    let done: ExportSessionActionReply = nest
        .request(
            "fauna.bridges.finalize_export_session",
            ExportSessionActionRequest {
                session_id: sid.clone(),
                ..Default::default()
            },
        )
        .await
        .expect("finalize_export_session RPC");
    assert_eq!(done.session.state, "completed");
    assert_eq!(done.session.download_url, format!("/api/v1/export/{sid}"));
    assert_eq!(done.session.blob_bytes, blob_bytes);
    assert_eq!(
        done.session.wrapped_session_key.as_ref().map(|b| &b[..]),
        Some(&wrapped_key[..]),
        "any of the user's clients must be able to fetch the wrapped key"
    );

    // ── § Cross-actor isolation reaches the download route: a second actor's
    // bearer is told not-found, never that the session exists.
    let other_bearer = state
        .auth
        .token_store
        .insert(ActorKeypair::from_secret(OTHER_SEED).actor_id(), 3600)
        .await;
    let foreign = http
        .get(format!("{base}{}", done.session.download_url))
        .bearer_auth(&other_bearer)
        .send()
        .await
        .expect("GET as a foreign actor");
    assert_eq!(foreign.status(), reqwest::StatusCode::NOT_FOUND);

    // ── The download, and the claim the whole test exists for.
    let resp = http
        .get(format!("{base}{}", done.session.download_url))
        .bearer_auth(&bearer)
        .send()
        .await
        .expect("GET the finished blob");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let downloaded = resp.bytes().await.expect("download body").to_vec();
    assert_eq!(
        downloaded.len() as u64,
        blob_bytes,
        "the route must serve every byte the session accounted for"
    );

    // The nest stored ciphertext and nothing else: no fragment of the archive
    // the client sealed survives into what came back sealed.
    assert!(
        !downloaded
            .windows(32)
            .any(|w| archive.windows(32).any(|a| a == w)),
        "the sealed blob must share no 32-byte run with the plaintext archive"
    );

    let mut opener = ExportBlobOpener::new(&SESSION_KEY, &sid, "mbox").expect("opener");
    let mut recovered = Vec::new();
    for frame in opener
        .push(&downloaded)
        .expect("open the downloaded frames")
    {
        recovered.extend_from_slice(&frame);
    }
    opener.finish().expect("the archive carries its terminator");
    assert_eq!(
        recovered, archive,
        "the bytes the client sealed are the bytes it gets back"
    );

    // ── The download is never silent (`mail-export.md` § Download flow →
    // *Standing and the owner's notice*): the owner is rung
    // with a `MailboxExportDownloaded` security notice. Spawned off the response
    // path, so a positive wait — a named generous budget + deadline poll
    // (convention 14), never a settle sleep-then-assert.
    const RING_BUDGET_SECS: u64 = 30;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(RING_BUDGET_SECS);
    loop {
        let rows = state
            .db
            .list_notifications(&user_id, None, 10)
            .await
            .unwrap();
        if rows.iter().any(|r| {
            r.notif_type.as_wire() == "security.notice"
                && r.summary.contains("mailbox export was downloaded")
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no MailboxExportDownloaded security notice within {RING_BUDGET_SECS}s; rows: {rows:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // ── § Expiry's "Discard now": the blob goes, the row goes, and the stale
    // URL answers 404 rather than a zombie blob.
    let discarded: DiscardExportBlobReply = nest
        .request(
            "fauna.bridges.discard_export_blob",
            ExportSessionActionRequest {
                session_id: sid.clone(),
                ..Default::default()
            },
        )
        .await
        .expect("discard_export_blob RPC");
    assert!(discarded.existed);

    let after: ListExportSessionsReply = nest
        .request(
            "fauna.bridges.list_export_sessions",
            ListExportSessionsRequest {},
        )
        .await
        .expect("list_export_sessions RPC");
    assert!(
        !after.sessions.iter().any(|s| s.session_id == sid),
        "a discarded session leaves no row"
    );
    let stale = http
        .get(format!("{base}/api/v1/export/{sid}"))
        .bearer_auth(&bearer)
        .send()
        .await
        .expect("GET a discarded session's blob");
    assert_eq!(stale.status(), reqwest::StatusCode::NOT_FOUND);
}

// ─────────────────────────── § Resume — the cold resume ──────────────────────

/// The key the restarting client mints for the new stream generation — § Resume
/// gives every generation a key of its own.
const RESTART_KEY: [u8; 32] = [0xC3; 32];

/// Seal `archive` into `pieces` frames plus the terminator, exactly as the
/// drive loop does: the preamble rides frame 0, the terminator is the last.
fn seal_into_frames(key: &[u8; 32], sid: &str, archive: &[u8], pieces: usize) -> Vec<Vec<u8>> {
    let mut sealer = ExportBlobSealer::new(key, sid, "mbox").expect("sealer");
    let step = archive.len().div_ceil(pieces);
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for (i, slice) in archive.chunks(step).enumerate() {
        let mut frame = Vec::new();
        if i == 0 {
            frame.extend_from_slice(&sealer.preamble());
        }
        frame.extend_from_slice(&sealer.seal_chunk(slice).expect("seal a chunk"));
        frames.push(frame);
    }
    frames.push(sealer.finish().expect("terminator frame"));
    frames
}

fn upload_request(
    sid: &str,
    generation: u64,
    idx: usize,
    frame: &[u8],
) -> UploadExportChunkRequest {
    UploadExportChunkRequest {
        session_id: sid.into(),
        chunk_idx: idx as u64,
        sealed_chunk: frame.to_vec(),
        exported_delta: 1,
        stream_generation: generation,
        ..Default::default()
    }
}

/// The wire code of a refused call, or a panic naming what came back instead.
fn refusal_code<E: fauna_protocol::RpcErrorClass + std::fmt::Debug>(e: &E) -> String {
    e.as_rpc_error()
        .unwrap_or_else(|| panic!("expected a server rejection, got {e:?}"))
        .code
        .clone()
}

#[tokio::test]
async fn a_cold_resume_restarts_the_stream_and_the_download_holds_no_stale_frame() {
    let (base, state, dir) = start_export_nest().await;
    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "exporter")
        .await
        .unwrap();
    common::seed_recipient_seal_key(&state.db, &user_id, &common::FIXTURE_MSEK).await;

    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("exporter"),
        &corpus(),
    )
    .expect("serialize the corpus");
    let archive = build_blob(&entries).expect("zip inside zstd");

    // ── The first app: starts the export, uploads two frames of its stream,
    // and is closed. Nobody is left to pause the session, so it rests `running`.
    let first = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let started: StartExportSessionReply = first
        .request(
            "fauna.bridges.start_export_session",
            StartExportSessionRequest {
                format: "mbox".into(),
                scope_descriptor: b"cbor-scope-descriptor".to_vec(),
                wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"wrapped-first".to_vec())),
                total_count: 3,
                ..Default::default()
            },
        )
        .await
        .expect("start_export_session RPC");
    let sid = started.session_id;
    let abandoned = seal_into_frames(&SESSION_KEY, &sid, &archive, 3);
    for (i, frame) in abandoned.iter().take(2).enumerate() {
        let _: UploadExportChunkReply = first
            .request(
                "fauna.bridges.upload_export_chunk",
                upload_request(&sid, 0, i, frame),
            )
            .await
            .expect("the first stream's frames land");
    }
    let exports = dir.path().join("exports");
    let files_on_disk = |when: &str| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&exports)
            .unwrap_or_else(|e| panic!("{when}: list exports/: {e}"))
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        files_on_disk("before the restart"),
        vec![format!("{sid}.zip.zst.sealed")]
    );

    // ── The second app (another connection — another device) finds the session
    // and restarts it. `running` is accepted: there was nobody to pause it.
    let second = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let restarted: ExportSessionActionReply = second
        .request(
            "fauna.bridges.restart_export_session",
            RestartExportSessionRequest {
                session_id: sid.clone(),
                wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"wrapped-second".to_vec())),
                total_count: 3,
                ..Default::default()
            },
        )
        .await
        .expect("restart_export_session RPC");
    let info = restarted.session;
    assert_eq!(info.session_id, sid, "the SAME session, not a new one");
    assert_eq!(info.state, "running");
    assert_eq!(info.stream_generation, 1);
    assert_eq!(
        (info.next_chunk_idx, info.blob_bytes, info.exported_count),
        (0, 0, 0)
    );
    assert_eq!(
        info.wrapped_session_key.as_ref().map(|b| &b[..]),
        Some(&b"wrapped-second"[..]),
        "the row now hands out the restart's key"
    );
    assert!(
        files_on_disk("after the restart").is_empty(),
        "the abandoned generation's blob is gone, and the new one is not created \
         until its frame 0 arrives"
    );

    // ── The first app turns out to be alive. Its next frame — and its failure
    // path's cancel — are told it is no longer the driver, and the restarted
    // stream is untouched by both.
    let stale: Result<UploadExportChunkReply, _> = first
        .request(
            "fauna.bridges.upload_export_chunk",
            upload_request(&sid, 0, 2, &abandoned[2]),
        )
        .await;
    let stale = stale.expect_err("a stale generation's upload is refused");
    assert_eq!(refusal_code(&stale), EXPORT_STREAM_SUPERSEDED);
    let stale_cancel: Result<ExportSessionActionReply, _> = first
        .request(
            "fauna.bridges.cancel_export_session",
            ExportSessionActionRequest {
                session_id: sid.clone(),
                stream_generation: Some(0),
                ..Default::default()
            },
        )
        .await;
    let stale_cancel = stale_cancel.expect_err("a superseded driver cannot cancel its successor");
    assert_eq!(refusal_code(&stale_cancel), EXPORT_STREAM_SUPERSEDED);

    // ── The second app drives the whole archive again, under its own key and
    // its own generation, and finalizes as the driver.
    let frames = seal_into_frames(&RESTART_KEY, &sid, &archive, 2);
    let mut uploaded = 0u64;
    for (i, frame) in frames.iter().enumerate() {
        let reply: UploadExportChunkReply = second
            .request(
                "fauna.bridges.upload_export_chunk",
                upload_request(&sid, 1, i, frame),
            )
            .await
            .expect("the restarted stream's frames land");
        uploaded += frame.len() as u64;
        assert_eq!(
            reply.blob_bytes, uploaded,
            "the byte total counts the new stream only"
        );
    }
    assert_eq!(
        files_on_disk("after the new stream's uploads"),
        vec![format!("{sid}.1.zip.zst.sealed")]
    );
    let done: ExportSessionActionReply = second
        .request(
            "fauna.bridges.finalize_export_session",
            ExportSessionActionRequest {
                session_id: sid.clone(),
                stream_generation: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("finalize as the driver");
    assert_eq!(done.session.state, "completed");
    assert_eq!(done.session.blob_bytes, uploaded);

    // ── The download is the restarted stream, whole, and nothing else.
    let bearer = state.auth.token_store.insert(user.actor_id(), 3600).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}{}", done.session.download_url))
        .bearer_auth(&bearer)
        .send()
        .await
        .expect("GET the finished blob");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let downloaded = resp.bytes().await.expect("download body").to_vec();
    assert_eq!(downloaded.len() as u64, uploaded);

    let mut opener = ExportBlobOpener::new(&RESTART_KEY, &sid, "mbox").expect("opener");
    let mut recovered = Vec::new();
    for frame in opener
        .push(&downloaded)
        .expect("every frame opens under the restart's key, in order")
    {
        recovered.extend_from_slice(&frame);
    }
    opener.finish().expect("the archive carries its terminator");
    assert_eq!(recovered, archive, "byte-for-byte the archive, restarted");

    // One key per generation: the abandoned stream's key opens nothing here.
    let mut old_key = ExportBlobOpener::new(&SESSION_KEY, &sid, "mbox").expect("opener");
    assert!(
        old_key.push(&downloaded).is_err(),
        "no frame of the download may open under the abandoned generation's key"
    );
}

/// § Resume's client-callable fail kind over the same real stack: the driver
/// reports a condition fatal to the export, and what survives is a row that
/// says so — readable by the user's *other* client, on its own connection,
/// which is the whole difference from the cancel this replaced.
///
/// Also pins the two refusals the kind carries: a superseded driver cannot fail
/// the stream that replaced its own, and an empty reason moves nothing.
#[tokio::test]
async fn a_failed_export_records_its_reason_for_the_users_other_client() {
    let (base, state, dir) = start_export_nest().await;
    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "exporter")
        .await
        .unwrap();
    common::seed_recipient_seal_key(&state.db, &user_id, &common::FIXTURE_MSEK).await;

    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("exporter"),
        &corpus(),
    )
    .expect("serialize the corpus");
    let archive = build_blob(&entries).expect("zip inside zstd");

    // ── The driving app opens a session and gets one frame up, then meets a
    // record its keys cannot open.
    let driver = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let started: StartExportSessionReply = driver
        .request(
            "fauna.bridges.start_export_session",
            StartExportSessionRequest {
                format: "mbox".into(),
                scope_descriptor: b"cbor-scope-descriptor".to_vec(),
                wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"wrapped-key".to_vec())),
                total_count: 3,
                ..Default::default()
            },
        )
        .await
        .expect("start_export_session RPC");
    let sid = started.session_id;
    let frames = seal_into_frames(&SESSION_KEY, &sid, &archive, 3);
    let _: UploadExportChunkReply = driver
        .request(
            "fauna.bridges.upload_export_chunk",
            upload_request(&sid, 0, 0, &frames[0]),
        )
        .await
        .expect("the first frame lands");
    let exports = dir.path().join("exports");
    let blob = exports.join(format!("{sid}.zip.zst.sealed"));
    assert!(
        blob.exists(),
        "the partial blob is on disk before the failure"
    );

    // An empty reason is refused before anything moves: the record is the point.
    let empty: Result<ExportSessionActionReply, _> = driver
        .request(
            "fauna.bridges.fail_export_session",
            FailExportSessionRequest {
                session_id: sid.clone(),
                reason: String::new(),
                stream_generation: Some(0),
                ..Default::default()
            },
        )
        .await;
    empty.expect_err("an empty reason is refused");
    assert!(blob.exists(), "and nothing was unlinked");

    const WHY: &str = "INBOX uid 2: sealed to a key this account never held";
    let failed: ExportSessionActionReply = driver
        .request(
            "fauna.bridges.fail_export_session",
            FailExportSessionRequest {
                session_id: sid.clone(),
                reason: WHY.into(),
                stream_generation: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("fail_export_session RPC");
    assert_eq!(failed.session.state, "errored");
    assert_eq!(failed.session.error_reason, WHY);
    assert!(
        !blob.exists(),
        "an archive without its terminator frame is disposed of, exactly as a cancel disposes of one"
    );

    // ── The user's OTHER client, on its own connection, reads the record. This
    // is what the cancel could not do, and the only reason the kind exists.
    let other_device = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let listed: ListExportSessionsReply = other_device
        .request(
            "fauna.bridges.list_export_sessions",
            ListExportSessionsRequest::default(),
        )
        .await
        .expect("list_export_sessions RPC");
    let row = listed
        .sessions
        .iter()
        .find(|s| s.session_id == sid)
        .expect("the failed session is still listed");
    assert_eq!(row.state, "errored");
    assert_eq!(row.error_reason, WHY);
    assert!(
        row.download_url.is_empty(),
        "a failed export offers no download"
    );

    // ── A driver superseded by another device cannot fail the stream that
    // replaced its own (§ Resume), so a dropped fetch on one device is not a
    // failure report about a healthy export on another.
    let second = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let fresh: StartExportSessionReply = second
        .request(
            "fauna.bridges.start_export_session",
            StartExportSessionRequest {
                format: "mbox".into(),
                scope_descriptor: b"cbor-scope-descriptor".to_vec(),
                wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"wrapped-a".to_vec())),
                total_count: 3,
                ..Default::default()
            },
        )
        .await
        .expect("start a second session");
    let sid2 = fresh.session_id;
    let _: ExportSessionActionReply = second
        .request(
            "fauna.bridges.restart_export_session",
            RestartExportSessionRequest {
                session_id: sid2.clone(),
                wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"wrapped-b".to_vec())),
                total_count: 3,
                ..Default::default()
            },
        )
        .await
        .expect("restart_export_session RPC");
    let stale: Result<ExportSessionActionReply, _> = second
        .request(
            "fauna.bridges.fail_export_session",
            FailExportSessionRequest {
                session_id: sid2.clone(),
                reason: "a dropped fetch on the abandoned device".into(),
                stream_generation: Some(0),
                ..Default::default()
            },
        )
        .await;
    let stale = stale.expect_err("a superseded driver cannot fail its successor");
    assert_eq!(refusal_code(&stale), EXPORT_STREAM_SUPERSEDED);
    let after: ListExportSessionsReply = second
        .request(
            "fauna.bridges.list_export_sessions",
            ListExportSessionsRequest::default(),
        )
        .await
        .expect("list_export_sessions RPC");
    let live = after
        .sessions
        .iter()
        .find(|s| s.session_id == sid2)
        .expect("the restarted session is still listed");
    assert_eq!(
        (live.state.as_str(), live.stream_generation),
        ("running", 1)
    );
    assert!(
        live.error_reason.is_empty(),
        "and it is not painted errored"
    );
}

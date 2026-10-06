//! **S9.4 — the client-import staged-envelope leg** (tier_3, real-wire).
//!
//! A raw RFC 5322 body **too large for the 2 MiB WS-RPC frame** cannot ride
//! `import_message` inline. Above the inline ceiling the client stages a **staged
//! envelope** on the bulk-byte plane and names it by reference; the nest resolves
//! it, opens it, and takes the identical seal-at-ingest path an inline body takes.
//! This is the nest-side end-to-end proof, driving the real surfaces:
//!
//!   the client AEAD-encrypts the plaintext under a one-shot key
//!   (`fauna_mail::staged_envelope::seal_and_split_staged_body`)
//!     → real `POST /api/v1/chunks` over real HTTP, authenticated with the client's
//!       own **session bearer** (proving `ChunkWriteAuth` accepts it with no mint —
//!       the property that distinguishes this leg from the MTA's minted bulk token)
//!     → real `fauna.bridges.import_message` carrying a `staged_body`, no inline body
//!     → the nest's `resolve_staged_body` (join fail-closed on the total, AEAD-open)
//!       → the *same* `import_one` path: `body_size` check, seal-at-ingest, quota,
//!         dedup, placement
//!     → real segment store (rests sealed, identical at-rest shape to an inline import)
//!     → open with the owner's secret → byte-for-byte the RFC 5322 the client staged.
//!
//! Owner docs: `smtp-server.md` § Message size limits (the staged-envelope rule) +
//! `mailbox-migration.md` § RPC surface. Complements the in-process handler pins
//! (`bridge_import_handlers.rs::a_staged_body_import_resolves_and_seals_the_plaintext_at_rest`),
//! which stage into the blob store directly and seed a dummy pubkey — here the whole
//! path is real, from an HTTP bearer-authenticated upload to an owner-secret open.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mail::staged_envelope::seal_and_split_staged_body;
use fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES;
use fauna_mls::wrapped_blob::is_sealed_mail_record;
use fauna_nest::backup::service::BackupService;
use fauna_nest::bridge_import_handlers::register_bridge_import_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    ImportMessageItem, ImportMessageOutcome, ImportMessageReply, ImportMessageRequest,
    StagedBodyRef, StartImportSessionReply, StartImportSessionRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};
use serde_bytes::ByteBuf;

/// Comfortably over the inline budget (`INLINE_MAIL_REQUEST_BUDGET_BYTES` ≈ 2.03 MB)
/// so the staged path is the *only* way this body can arrive, over the 4 MiB
/// `MAIL_BODY_CHUNK_BYTES` so the split/join legs genuinely engage (>1 chunk), and
/// under the ~8.06 MB at-rest admission ceiling so the import is admitted.
const BODY_BYTES: usize = 6 * 1024 * 1024;

struct Harness {
    router: RpcRouter,
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let mut b = RpcRouter::builder();
    register_bridge_import_handlers(&mut b);
    let router = b.build();

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    Harness {
        router,
        state,
        addr,
        _dir: dir,
    }
}

async fn dispatch(
    h: &Harness,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    common::seed_dispatch_actor(&h.state.db, &actor).await; // the User-class `users` row
    let meta = h.router.kind_meta(kind).expect("kind registered");
    (meta.handler)(h.state.clone(), actor, payload).await
}

/// A realistic RFC 5322 message with a multi-megabyte body — a photo attachment
/// imported from a foreign mailbox. Deterministic, non-compressible-ish filler so
/// a truncated or reordered chunk cannot coincidentally still compare equal.
fn big_message() -> Vec<u8> {
    let mut m = Vec::with_capacity(BODY_BYTES + 256);
    m.extend_from_slice(
        b"Message-ID: <big-import@source.test>\r\n\
          From: Old Account <me@source.test>\r\n\
          Subject: a multi-megabyte attachment being imported\r\n\
          \r\n",
    );
    let mut i: u64 = 0;
    while m.len() < BODY_BYTES {
        m.extend_from_slice(&i.to_le_bytes());
        i = i.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    m
}

#[tokio::test]
async fn an_over_frame_import_body_stages_with_the_session_bearer_and_rests_sealed() {
    let h = harness().await;
    let http = reqwest::Client::new();

    // The importing user: a real recipient keypair (the nest sees only the public
    // half), and a full session bearer — the client's own, minted at login.
    let user_id: [u8; 32] = [0x42; 32];
    let owner_msek = [0x6fu8; 32];
    common::seed_recipient_seal_key(&h.state.db, &user_id, &owner_msek).await;
    common::seed_dispatch_actor(&h.state.db, &user_id).await; // User-class `users` row
    let bearer = h
        .state
        .auth
        .token_store
        .insert(fauna_core::identity::ActorId(user_id), 3600)
        .await;

    // ── Client: seal the plaintext under a one-shot key and split the CIPHERTEXT ──
    let body = big_message();
    let (key, chunks, total_bytes) = seal_and_split_staged_body(&body);
    assert!(
        chunks.len() > 1,
        "a 6 MB body must span several ciphertext chunks (4 MiB MAIL_BODY_CHUNK_BYTES)"
    );

    // ── Client: stage every ciphertext chunk over the REAL byte-plane write route,
    // authenticated by the SESSION BEARER — no mint, the leg's defining property ──
    let mut chunk_hashes = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let resp = http
            .post(format!("http://{}/api/v1/chunks", h.addr))
            .header("Authorization", format!("Bearer {bearer}"))
            .header("X-Content-Hash", hex::encode(&c.hash))
            .body(c.bytes.clone())
            .send()
            .await
            .expect("chunk upload");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::CREATED,
            "the byte plane must accept a chunk authorized by a full session bearer"
        );
        chunk_hashes.push(ByteBuf::from(c.hash.clone()));
    }

    // ── Client: start a session, then import by reference — no inline body ──
    let start_reply: StartImportSessionReply = decode(
        &dispatch(
            &h,
            user_id,
            "fauna.bridges.start_import_session",
            Bytes::from(
                encode_canonical(&StartImportSessionRequest {
                    source_descriptor: "generic:imap.source.test:me".into(),
                    total_count: 1,
                    scope: vec!["INBOX".into()],
                    // Not the subject here (the staged-body ceiling is); a
                    // sealless session is the ratified keyless degrade.
                    source_sealed: None,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("start the import session"),
    )
    .unwrap();

    let import_req = ImportMessageRequest {
        session_id: start_reply.session_id.clone(),
        message: ImportMessageItem {
            mailbox: "INBOX".into(),
            flags: vec!["\\Seen".into()],
            body: Vec::new(), // empty: the bytes rode the byte plane
            timestamp: 1_715_000_000,
            body_size: body.len() as u32, // the PLAINTEXT length
            sender_domain: "source.test".into(),
            source_uid: 1,
            source_uid_validity: 9,
            dedup_key: "msgid:v1:big-import@source.test".into(),
            envelope_key: "env:v1:staged".into(),
            staged_body: Some(StagedBodyRef {
                chunk_hashes,
                total_bytes,
                key: fauna_core::secret::SecretByteBuf::from(key),
            }),
        },
        skip_dedup: false,
    };
    let encoded = encode_canonical(&import_req).expect("encode import");
    // The reference is what keeps the import request inside the permanent 2 MiB cap
    // that the inline body could never fit.
    assert!(
        encoded.len() < INLINE_MAIL_REQUEST_BUDGET_BYTES as usize,
        "the staged import request must fit the WS-RPC frame ({} bytes) — a {BODY_BYTES}-byte \
         inline body never could",
        encoded.len()
    );

    let reply: ImportMessageReply = decode(
        &dispatch(
            &h,
            user_id,
            "fauna.bridges.import_message",
            Bytes::from(encoded.to_vec()),
        )
        .await
        .expect("import by reference must succeed"),
    )
    .expect("decode import reply");
    let ImportMessageOutcome::Imported { message_id, .. } = reply.outcome else {
        panic!("expected Imported, got {:?}", reply.outcome);
    };
    assert_eq!(reply.imported_count, 1);

    // ── At rest: sealed, and openable by the owner alone — byte-for-byte the body
    // the client staged (the proof the dummy-pubkey handler pin cannot make). A
    // 6 MB body rests as CONTINUATION RECORDS post-write-flip (it splits at the
    // 1 MiB part cap), so the serve-join `read_sealed_body_with_floor` reassembles
    // it — the same path IMAP `fetch_message_ciphertext` drives. ──
    let (joined_body, _hint, _floor) = fauna_nest::segments::mail::read_sealed_body_with_floor(
        &h.state.mail_segments,
        &h.state.db,
        &user_id,
        &message_id,
    )
    .await
    .unwrap()
    .expect("the imported record must be readable at rest");
    assert!(
        is_sealed_mail_record(&joined_body),
        "a staged import must rest sealed, identical to an inline import"
    );
    let opened = common::open_recipient_record(&joined_body, &owner_msek);
    assert_eq!(
        opened, body,
        "the plaintext recovered from the sealed record must be byte-for-byte the RFC 5322 \
         the client staged over the byte plane"
    );
}

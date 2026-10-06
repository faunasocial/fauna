//! A sealed mail body **too large for the RPC frame** crosses on the bulk-byte
//! plane, rests sealed, and comes back byte-for-byte.
//!
//! Owner doc: `docs/goal/behavior/smtp-server.md` § Message size limits (the rule:
//! `docs/goal/architecture/transport.md` § Max frame).
//!
//! The contradiction this closes: mail advertises `max_message_bytes` = 50 MB while
//! every WS-RPC frame is permanently capped at 2 MiB. Until now the perimeter
//! resolved that by *refusing* anything over the ~1.5 MB inline ceiling with a
//! permanent `552 5.3.4` — honest, but it meant the 50 MB product ceiling was
//! fiction. The reference legs make it real: a body over the inline budget is staged
//! on the byte plane as content-addressed chunks and the RPC carries only the hashes.
//!
//! This is the **nest-side** end-to-end proof, and it drives the real surfaces —
//! nothing is simulated but the Go bridge processes themselves:
//!
//!   seal (`seal_to_recipient` — the exact Rust core the Go MTA's
//!   `EncryptToRecipient` wraps over FFI)
//!     → real `fauna.bridges.mint_bulk_byte_token` (purpose `MailBody`, as the MTA calls it)
//!     → real `POST /api/v1/chunks` over real HTTP, authenticated with that minted
//!       bulk token (proving `ChunkWriteAuth` accepts a mail-purpose token — the
//!       auth decision this track took)
//!     → real `fauna.bridges.ingest_inbound_mail` carrying a `body_ref`, no inline body
//!     → real segment store (the bytes rest sealed, in the identical at-rest shape an
//!       inline body would have taken)
//!     → real `fauna.bridges.fetch_message_ciphertext`, whose reply carries a `body_ref`
//!     → real `GET /api/v1/chunks/{hash}` (the **open** download route — ciphertext by
//!       hash, so the MDA needs no token on this leg)
//!     → rejoin → `unseal_mail_record` → byte-equal with the body that was sent.
//!
//! What this does NOT prove, and the tier_3 e2e still must: that the *Go* MTA takes
//! this path off a real SMTP `DATA`, and that the *Go* MDA serves it back over a real
//! IMAP `FETCH BODY[]`.

mod common;
use common::approve_bridge;

/// A real-HTTP [`BlobFetcher`](fauna_core::file_download::BlobFetcher) over the
/// nest's **open** chunk download route — the test's stand-in for the shipping
/// per-target bindings (`fauna_client::NestPublicChunkFetcher` natively, the
/// gloo-net one on web), which need a full authenticated client this in-process
/// test has no reason to build. Everything below the binding — the hash→key
/// derivation, the fetch, the fail-closed join — is the shared code those
/// bindings run, reached through `resolve_referenced_mail_body`.
///
/// Carries no bearer, because the route takes none.
struct OpenRouteChunkFetcher {
    base: String,
    http: reqwest::Client,
}

#[async_trait::async_trait]
impl fauna_core::file_download::BlobFetcher for OpenRouteChunkFetcher {
    async fn fetch_manifest(
        &self,
        _hash: &fauna_core::data::ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        // A mail body reference names no manifest (`fauna_mail::body_ref` docs).
        anyhow::bail!("a mail body reference never names a manifest")
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[fauna_core::data::ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let url = format!("{}/api/v1/chunks/{}", self.base, hex::encode(key.digest()));
            let resp = self.http.get(&url).send().await?;
            if resp.status() != reqwest::StatusCode::OK {
                anyhow::bail!("GET {url} ({}) for {relative_path}", resp.status());
            }
            out.push(resp.bytes().await?.to_vec());
        }
        Ok(out)
    }
}

use std::sync::Arc;

use bytes::Bytes;

use fauna_mail::body_ref::{join_sealed_mail_body_checked, split_sealed_mail_body};
use fauna_mail::transport_limits::SEAL_ENVELOPE_ALLOWANCE_BYTES;
use fauna_mls::wrapped_blob::{
    MailRecordEnvelope, derive_recipient_hpke_keypair, seal_to_recipient, unseal_mail_record,
};
use fauna_nest::backup::service::BackupService;
use fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers;
use fauna_nest::bridge_imap_handlers::register_bridge_imap_handlers;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    AppendMessageReply, AppendMessageRequest, AuthVerdicts, DkimVerdict, DmarcVerdict,
    FetchMessageCiphertextReply, FetchMessageCiphertextRequest, IngestInboundMailReply,
    IngestInboundMailRequest, PublicMailMetadata, SpfVerdict,
};
use fauna_protocol::email::{InboxFetchReply, InboxFetchRequest};
use fauna_protocol::wrapped_blob::{
    BulkByteAccess, BulkByteMintPurpose, MintBulkByteTokenReply, MintBulkByteTokenRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// A body comfortably over the 2 MiB frame — and over the ~1.5 MB inline ceiling the
/// interim perimeter refused — so the reference path is the only way it can arrive.
///
/// Deliberately **6 MB, not 50 MB** — historically because *resting* was the binding
/// constraint: a sealed body had to fit one CARv2 record (16 MiB on read) and the v1
/// at-rest envelope inflated ciphertext ~1.91×, capping a readable body at ~8.06 MB.
///
/// **Both of those ended with the 2026-07-18 flip** (v2 payloads are byte strings, and
/// an over-cap body splits into continuation records), so 6 MB is no longer near any
/// ceiling — this body now exercises the **continuation** path rather than a single
/// oversized record, which is strictly better coverage for the same cost. The value
/// stays 6 MB because what it must prove is the reference legs against the real at-rest
/// path, and the perimeter still admits ~8.06 MB until the ceiling-retirement slice
/// lands (`smtp-server.md` § Message size limits).
const BODY_BYTES: usize = 6 * 1024 * 1024;

struct Harness {
    router: RpcRouter,
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
    /// Dropping this unlinks the on-disk blob store the server still holds.
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
    register_bridge_routing_handlers(&mut b); // ingest_inbound_mail
    register_bridge_imap_handlers(&mut b); // fetch_message_ciphertext
    register_bridge_blob_handlers(&mut b); // mint_bulk_byte_token
    register_email_handlers(&mut b); // inbox.fetch (the first-party feed)
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
    common::seed_dispatch_actor(&h.state.db, &actor).await;
    let meta = h.router.kind_meta(kind).expect("kind registered");
    (meta.handler)(h.state.clone(), actor, payload).await
}

/// An `IngestInboundMailRequest` for `recipient`, carrying either an inline body or
/// a reference (never both — callers pick one and pass `Vec::new()`/`None` for the
/// other) plus the sealed hint every test in this file derives the same way.
///
/// Verdicts are fixed at dkim/spf/dmarc = `Pass`: nest only records auth verdicts
/// (`bridge_routing_handlers.rs` flattens them for storage/display), it never gates
/// ingest admission on them, and no test in this file asserts a verdict value back —
/// so one canonical "clean" triple serves every call site that cares to set one.
fn ingest_request(
    recipient: &[u8; 32],
    encrypted_body: Vec<u8>,
    body_ref: Option<fauna_protocol::bridge_routing::MailBodyRef>,
    sealed_hint: Vec<u8>,
    timestamp: i64,
    ciphertext_size: u32,
) -> IngestInboundMailRequest {
    IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: recipient.to_vec(),
        encrypted_body,
        encrypted_index_hint: sealed_hint,
        body_ref,
        public_metadata: PublicMailMetadata {
            timestamp,
            ciphertext_size,
            sender_domain: "external.test".into(),
        },
        verdicts: AuthVerdicts {
            dkim: DkimVerdict::Pass,
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Pass,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A realistic RFC 5322 message with a large body — what a photo attachment looks
/// like once it reaches the perimeter.
fn big_message() -> Vec<u8> {
    let mut m = Vec::with_capacity(BODY_BYTES + 256);
    m.extend_from_slice(
        b"From: External Sender <sender@external.test>\r\n\
          To: alice@local.test\r\n\
          Subject: a multi-megabyte attachment\r\n\
          \r\n",
    );
    // Deterministic, non-compressible-ish filler so a truncation or a reordered
    // chunk cannot coincidentally still compare equal.
    let mut i: u64 = 0;
    while m.len() < BODY_BYTES {
        m.extend_from_slice(&i.to_le_bytes());
        i = i.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    m
}

#[tokio::test]
async fn an_over_frame_mail_body_crosses_by_reference_and_returns_byte_for_byte() {
    let h = harness().await;
    let http = reqwest::Client::new();

    // Recipient: the production recipient-mail keypair, derived from a client-held
    // MSEK exactly as `enable_mail` does. The nest sees only the public half.
    let recipient: [u8; 32] = [0x42; 32];
    let msek: [u8; 32] = [0x5e; 32];
    let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);
    common::seed_recipient_seal_key(&h.state.db, &recipient, &msek).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    let mda_actor: [u8; 32] = [0x22; 32];
    approve_bridge(&h.state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;
    approve_bridge(&h.state.db, &mda_actor, BridgeRole::Mda, &[0x92; 32]).await;

    // ── The perimeter seal, exactly as the Go MTA performs it ──
    let body = big_message();
    let sealed_body = seal_to_recipient(&body, &recipient_pubkey)
        .expect("seal body")
        .to_canonical_bytes()
        .expect("encode sealed body envelope");
    let sealed_hint = seal_to_recipient(b"index-hint", &recipient_pubkey)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint envelope");

    // The premise of the whole track: this body cannot ride the frame.
    assert!(
        fauna_mail::body_ref::mail_body_needs_reference(
            sealed_body.len() as u64,
            sealed_hint.len() as u64
        ),
        "a 10 MB sealed body must be over the inline budget, else this test proves nothing"
    );

    // ── MTA: mint a mail-body bulk token ──
    let mint_reply: MintBulkByteTokenReply = decode(
        &dispatch(
            &h,
            mta_actor,
            "fauna.bridges.mint_bulk_byte_token",
            Bytes::from(
                encode_canonical(&MintBulkByteTokenRequest {
                    actor_id: recipient.to_vec(),
                    folder: String::new(), // mail belongs to no set
                    access: BulkByteAccess::Write,
                    purpose: BulkByteMintPurpose::MailBody,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("the MTA mints a mail-body token"),
    )
    .unwrap();

    // ── MTA: stage the sealed body over the REAL byte-plane write route ──
    // The bulk token is what makes this reachable: the MTA holds no session bearer.
    let chunks = split_sealed_mail_body(&sealed_body);
    assert!(chunks.len() > 1, "a 10 MB body must span several chunks");
    let mut chunk_hashes = Vec::new();
    for c in &chunks {
        let resp = http
            .post(format!("http://{}/api/v1/chunks", h.addr))
            .header("Authorization", format!("Bearer {}", mint_reply.token))
            .header("X-Content-Hash", hex::encode(&c.hash))
            .body(c.bytes.clone())
            .send()
            .await
            .expect("chunk upload");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::CREATED,
            "the byte plane must accept a chunk authorized by a mail-purpose bulk token"
        );
        chunk_hashes.push(serde_bytes::ByteBuf::from(c.hash.clone()));
    }

    // ── MTA: ingest by reference — no inline body on the wire at all ──
    let ingest_req = ingest_request(
        &recipient,
        Vec::new(),
        Some(fauna_protocol::bridge_routing::MailBodyRef {
            chunk_hashes,
            total_bytes: sealed_body.len() as u64,
        }),
        sealed_hint,
        1_715_000_000,
        sealed_body.len() as u32,
    );
    let encoded = encode_canonical(&ingest_req).expect("encode ingest");
    // The reference is what keeps the request inside the permanent 2 MiB cap.
    assert!(
        encoded.len() < 2 * 1024 * 1024,
        "the ingest request must fit the WS-RPC frame ({} bytes)",
        encoded.len()
    );

    let ingest_reply: IngestInboundMailReply = decode(
        &dispatch(
            &h,
            mta_actor,
            "fauna.bridges.ingest_inbound_mail",
            Bytes::from(encoded.to_vec()),
        )
        .await
        .expect("ingest by reference must succeed"),
    )
    .expect("decode ingest reply");
    let message_id = ingest_reply.message_id;
    assert_eq!(message_id.len(), 32);

    // ── MDA: fetch — the reply must carry a reference, not 10 MB of inline bytes ──
    let fetch_reply: FetchMessageCiphertextReply = decode(
        &dispatch(
            &h,
            mda_actor,
            "fauna.bridges.fetch_message_ciphertext",
            Bytes::from(
                encode_canonical(&FetchMessageCiphertextRequest {
                    actor_id: recipient.to_vec(),
                    message_id: message_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("MDA fetch must succeed"),
    )
    .expect("decode fetch reply");

    let (body_ref, ciphertext_size) = match fetch_reply {
        FetchMessageCiphertextReply::Found {
            encrypted_body,
            ciphertext_size,
            body_ref,
            ..
        } => {
            assert!(
                encrypted_body.is_empty(),
                "an over-frame body must not be handed back inline — it cannot cross"
            );
            (
                body_ref.expect("reply must carry a body reference"),
                ciphertext_size,
            )
        }
        FetchMessageCiphertextReply::NotFound => panic!("stored message must be fetchable"),
    };
    assert_eq!(ciphertext_size as usize, sealed_body.len());
    assert_eq!(body_ref.total_bytes, sealed_body.len() as u64);

    // ── MDA: GET the chunks back over the OPEN download route (no token) ──
    let mut fetched: Vec<Vec<u8>> = Vec::new();
    for hash in &body_ref.chunk_hashes {
        let resp = http
            .get(format!(
                "http://{}/api/v1/chunks/{}",
                h.addr,
                hex::encode(hash)
            ))
            .send()
            .await
            .expect("chunk download");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        fetched.push(resp.bytes().await.unwrap().to_vec());
    }
    let rejoined = join_sealed_mail_body_checked(&fetched, body_ref.total_bytes)
        .expect("the chunks the reply named must rejoin to the declared total");

    // The nest re-wraps the bridge envelope in a segment envelope on store; what comes
    // back must be the *inner* bridge envelope, verbatim — exactly what the MTA sealed.
    assert_eq!(
        rejoined, sealed_body,
        "the round-tripped ciphertext is the verbatim sealed envelope the MTA produced"
    );

    // ── The assertion that proves large mail actually works ──
    let envelope =
        MailRecordEnvelope::from_canonical_bytes(&rejoined).expect("parse fetched envelope");
    let opened = unseal_mail_record(&envelope, &recipient_secret)
        .expect("recipient opens the stored ciphertext with the MSEK-derived secret");
    assert_eq!(
        opened.as_slice(),
        body.as_slice(),
        "a 10 MB message decrypts byte-for-byte back to what was sent"
    );

    // Negative control — gives the open above teeth.
    let (wrong_secret, _) = derive_recipient_hpke_keypair(&[0xAB; 32]);
    assert!(
        unseal_mail_record(&envelope, &wrong_secret).is_err(),
        "a non-recipient secret must fail to open the stored ciphertext"
    );
}

/// **A body over the single-record cap RESTS AS CONTINUATION RECORDS and reads
/// back** — the inverted successor of `a_body_too_large_to_rest_is_refused_...`.
///
/// Until the 2026-07-18 write flip this same 12 MB body was *refused*: at v1's
/// ~1.91× at-rest expansion it would have encoded to ~23 MB, over the 16 MiB CARv2
/// `MAX_RECORD_LEN` that is enforced **on read**, so storing it would have meant a
/// `250` followed by permanently unreadable mail. The refusal was the honest
/// interim answer while a body had to fit one record.
///
/// Continuation writes ended that: `append_record` now splits any sealed body over
/// `MAIL_BODY_PART_CAP_BYTES` into frame-sized parts + a v3 head, so there is no
/// such thing as "too large to rest" any more. This test pins the *replacement*
/// property, which is strictly stronger than the refusal it supersedes — the body
/// is accepted, actually rests as a continuation family, and comes back
/// byte-for-byte.
///
/// **Why the head assertion carries the test.** Post-flip the write format is v2
/// (no array-of-integers expansion), so a 12 MB body would *also* fit a single
/// 16 MiB record — success alone would therefore prove nothing about the split.
/// Asserting the live continuation head is what distinguishes "it split" from "it
/// happened to fit".
#[tokio::test]
async fn a_body_over_the_single_record_cap_rests_as_continuation_records_and_reads_back() {
    let h = harness().await;
    let http = reqwest::Client::new();

    let recipient: [u8; 32] = [0x42; 32];
    let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&h.state.db, &recipient, &[0x5e; 32]).await;
    let mta_actor: [u8; 32] = [0x11; 32];
    let mda_actor: [u8; 32] = [0x22; 32];
    approve_bridge(&h.state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;
    approve_bridge(&h.state.db, &mda_actor, BridgeRole::Mda, &[0x92; 32]).await;

    // Far over `MAIL_BODY_PART_CAP_BYTES` (1 MiB) so the continuation split is the
    // route this takes, and over the old ~8.1 MB at-rest ceiling (now retired) so
    // it also proves that ceiling is gone. At v1's ~1.91× expansion this record
    // would have exceeded the 16 MiB CARv2 cap and been refused.
    let mut body = vec![0u8; 12 * 1024 * 1024];
    let mut i: u64 = 0;
    for b in body.iter_mut() {
        // High-entropy filler — every byte ≥ 24, i.e. the worst-case 2-byte encoding.
        i = i.wrapping_mul(6364136223846793005).wrapping_add(1);
        *b = 24 + (i >> 56) as u8 % 232;
    }
    let sealed_body = seal_to_recipient(&body, &recipient_pubkey)
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
    let sealed_hint = seal_to_recipient(b"index-hint", &recipient_pubkey)
        .unwrap()
        .to_canonical_bytes()
        .unwrap();

    let mint_reply: MintBulkByteTokenReply = decode(
        &dispatch(
            &h,
            mta_actor,
            "fauna.bridges.mint_bulk_byte_token",
            Bytes::from(
                encode_canonical(&MintBulkByteTokenRequest {
                    actor_id: recipient.to_vec(),
                    folder: String::new(),
                    access: BulkByteAccess::Write,
                    purpose: BulkByteMintPurpose::MailBody,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();

    let mut chunk_hashes = Vec::new();
    for c in &split_sealed_mail_body(&sealed_body) {
        http.post(format!("http://{}/api/v1/chunks", h.addr))
            .header("Authorization", format!("Bearer {}", mint_reply.token))
            .header("X-Content-Hash", hex::encode(&c.hash))
            .body(c.bytes.clone())
            .send()
            .await
            .expect("chunk upload");
        chunk_hashes.push(serde_bytes::ByteBuf::from(c.hash.clone()));
    }

    let ingest_reply: IngestInboundMailReply = decode(
        &dispatch(
            &h,
            mta_actor,
            "fauna.bridges.ingest_inbound_mail",
            Bytes::from(
                encode_canonical(&ingest_request(
                    &recipient,
                    Vec::new(),
                    Some(fauna_protocol::bridge_routing::MailBodyRef {
                        chunk_hashes,
                        total_bytes: sealed_body.len() as u64,
                    }),
                    sealed_hint,
                    1_715_000_000,
                    sealed_body.len() as u32,
                ))
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("post-flip an over-cap body rests as continuation records, never refused"),
    )
    .expect("decode ingest reply");
    assert_eq!(ingest_reply.message_id.len(), 32);

    // ── It actually SPLIT — the discriminator this test turns on ──
    let heads = {
        let conn = h.state.db.conn().await;
        fauna_nest::segments::records_db::list_live_continuation_heads(&conn, &recipient)
            .expect("list continuation heads")
    };
    assert_eq!(
        heads.len(),
        1,
        "a 12 MB body must rest as exactly one continuation family (parts + one v3 \
         head); {} live heads means the split did not happen and it fell back to a \
         single record",
        heads.len()
    );

    // ── And it reads back: the serve-join rejoins the parts transparently ──
    let fetch_reply: FetchMessageCiphertextReply = decode(
        &dispatch(
            &h,
            mda_actor,
            "fauna.bridges.fetch_message_ciphertext",
            Bytes::from(
                encode_canonical(&FetchMessageCiphertextRequest {
                    actor_id: recipient.to_vec(),
                    message_id: ingest_reply.message_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("MDA fetch of a continuation family must succeed"),
    )
    .expect("decode fetch reply");

    let body_ref = match fetch_reply {
        FetchMessageCiphertextReply::Found { body_ref, .. } => {
            body_ref.expect("a 12 MB body cannot ride inline — it must come back by reference")
        }
        FetchMessageCiphertextReply::NotFound => {
            panic!("the stored continuation family must be fetchable")
        }
    };

    let mut fetched: Vec<Vec<u8>> = Vec::new();
    for hash in &body_ref.chunk_hashes {
        let resp = http
            .get(format!(
                "http://{}/api/v1/chunks/{}",
                h.addr,
                hex::encode(hash)
            ))
            .send()
            .await
            .expect("chunk download");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        fetched.push(resp.bytes().await.unwrap().to_vec());
    }
    let rejoined = join_sealed_mail_body_checked(&fetched, body_ref.total_bytes)
        .expect("the chunks the reply named must rejoin to the declared total");
    assert_eq!(
        rejoined, sealed_body,
        "the parts must rejoin to the verbatim sealed envelope the MTA produced"
    );

    // The property the whole flip exists to deliver: it decrypts back to the message.
    let envelope =
        MailRecordEnvelope::from_canonical_bytes(&rejoined).expect("parse fetched envelope");
    let opened = unseal_mail_record(&envelope, &recipient_secret)
        .expect("recipient opens a continuation-stored body with the MSEK-derived secret");
    assert_eq!(
        opened.as_slice(),
        body.as_slice(),
        "a 12 MB message stored as continuation records decrypts byte-for-byte"
    );
}

/// An ingest that carries neither an inline body nor a reference is refused.
///
/// This is the guard against silent mail loss. A producer that sends neither an
/// inline body nor a `body_ref` would leave an empty `encrypted_body` — and an empty
/// body that stored cleanly would be silent mail loss under a `250`. The nest never
/// does that: it fails closed, and the sender is told.
#[tokio::test]
async fn an_ingest_with_neither_an_inline_body_nor_a_reference_is_refused() {
    let h = harness().await;
    let recipient: [u8; 32] = [0x42; 32];
    let (_, recipient_pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&h.state.db, &recipient, &[0x5e; 32]).await;
    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&h.state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    let sealed_hint = seal_to_recipient(b"index-hint", &recipient_pubkey)
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
    let err = dispatch(
        &h,
        mta_actor,
        "fauna.bridges.ingest_inbound_mail",
        Bytes::from(
            encode_canonical(&IngestInboundMailRequest {
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                actor_id: recipient.to_vec(),
                encrypted_body: Vec::new(),
                encrypted_index_hint: sealed_hint,
                body_ref: None,
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect_err("an empty message must never be stored");
    assert!(
        err.code.contains("malformed") || err.code.contains("invalid"),
        "expected a typed refusal, got {}",
        err.code
    );
}

/// Nest's APPEND admission (`bridge_imap_handlers::append_message_handler`) sees
/// only the sealed `ciphertext_size`, but the product ceiling `max_message_bytes`
/// is on RAW bytes. It bridges the two with `SEAL_ENVELOPE_ALLOWANCE_BYTES`,
/// admitting up to `ceiling + allowance` sealed bytes — so the allowance must
/// cover the seal's real growth, or a raw message legitimately within the ceiling
/// would be refused. This is the test that makes that constant falsifiable rather
/// than a guess.
///
/// The seal cost is a small CONSTANT, independent of body size (the wire seal's
/// `enc`/`ct` are CBOR byte strings — no array-of-integers expansion), so a
/// modest body measures it as faithfully as a ceiling-sized one and keeps the
/// test cheap. With the at-rest ceiling retired the erring directions inverted:
/// erring high now only admits a sliver over the raw ceiling (harmless — any size
/// rests), while erring low would refuse a body legitimately within it.
#[test]
fn the_seal_allowance_covers_what_the_seal_actually_adds() {
    let (_sk, recipient_pubkey) = derive_recipient_hpke_keypair(&[7u8; 32]);

    // Any size measures the (constant) seal overhead; 1 MiB is representative and
    // cheap.
    let raw = vec![b'x'; 1024 * 1024];
    let sealed = seal_to_recipient(&raw, &recipient_pubkey)
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");

    let growth = sealed.len() - raw.len();
    assert!(
        growth < SEAL_ENVELOPE_ALLOWANCE_BYTES as usize,
        "the seal grew a body by {growth} bytes, which does not fit the \
         {SEAL_ENVELOPE_ALLOWANCE_BYTES}-byte allowance — nest's APPEND admission would \
         then refuse a raw message legitimately within the product ceiling",
    );
}

/// **The first-party mailbox feed never builds an unsendable page.**
///
/// **The first-party mailbox feed serves an over-frame message by REFERENCE (the
/// client-feed leg).**
///
/// `fauna.email.inbox.fetch` ships each message's verbatim outer segment envelope,
/// and the perimeter admits messages whose stored envelope alone exceeds the
/// permanent 2 MiB WS-RPC frame. That envelope cannot be serialized into one frame,
/// so it crosses by reference: the message carries a small `body_ref`, its bytes
/// staged on the byte plane, and the client GETs + rejoins the chunks over the open
/// download route to the exact stored envelope, which opens to the plaintext body
/// byte-for-byte. (The deployed-reader gate that once skipped it instead left with
/// the compat-remnant sweep.)
#[tokio::test]
async fn the_mailbox_feed_serves_an_over_frame_envelope_by_reference() {
    let h = harness().await;
    let http = reqwest::Client::new();

    let recipient: [u8; 32] = [0x42; 32];
    let msek: [u8; 32] = [0x5e; 32];
    let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);
    common::seed_recipient_seal_key(&h.state.db, &recipient, &msek).await;
    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&h.state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    let seal = |body: &[u8]| {
        seal_to_recipient(body, &recipient_pubkey)
            .unwrap()
            .to_canonical_bytes()
            .unwrap()
    };
    let sealed_hint = seal(b"index-hint");

    // The closures below capture a Copy reference so they stay callable
    // repeatedly (an `async move` block would otherwise move `h` itself).
    let hr = &h;

    // Ingest one INLINE message; returns its server-assigned message id.
    let ingest_inline = |body: Vec<u8>, timestamp: i64| {
        let sealed_body = seal(&body);
        let req = ingest_request(
            &recipient,
            sealed_body.clone(),
            None,
            sealed_hint.clone(),
            timestamp,
            sealed_body.len() as u32,
        );
        async move {
            let reply: IngestInboundMailReply = decode(
                &dispatch(
                    hr,
                    mta_actor,
                    "fauna.bridges.ingest_inbound_mail",
                    Bytes::from(encode_canonical(&req).unwrap().to_vec()),
                )
                .await
                .expect("inline ingest"),
            )
            .unwrap();
            reply.message_id
        }
    };

    // uid 1 — a normal small message.
    let small_1 = ingest_inline(b"a normal message".to_vec(), 1_715_000_001).await;

    // uid 2 — the 6 MB message, ingested by reference exactly as the MTA stages it.
    // Its stored body is far over the 2 MiB frame budget either way (post-flip it rests
    // as continuation parts and the serve-join materializes it): the message the guard
    // exists for.
    let big_sealed = seal(&big_message());
    let mint_reply: MintBulkByteTokenReply = decode(
        &dispatch(
            &h,
            mta_actor,
            "fauna.bridges.mint_bulk_byte_token",
            Bytes::from(
                encode_canonical(&MintBulkByteTokenRequest {
                    actor_id: recipient.to_vec(),
                    folder: String::new(),
                    access: BulkByteAccess::Write,
                    purpose: BulkByteMintPurpose::MailBody,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let mut chunk_hashes = Vec::new();
    for c in &split_sealed_mail_body(&big_sealed) {
        let resp = http
            .post(format!("http://{}/api/v1/chunks", h.addr))
            .header("Authorization", format!("Bearer {}", mint_reply.token))
            .header("X-Content-Hash", hex::encode(&c.hash))
            .body(c.bytes.clone())
            .send()
            .await
            .expect("chunk upload");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        chunk_hashes.push(serde_bytes::ByteBuf::from(c.hash.clone()));
    }
    let big_id: Vec<u8> = {
        let reply: IngestInboundMailReply = decode(
            &dispatch(
                &h,
                mta_actor,
                "fauna.bridges.ingest_inbound_mail",
                Bytes::from(
                    encode_canonical(&ingest_request(
                        &recipient,
                        Vec::new(),
                        Some(fauna_protocol::bridge_routing::MailBodyRef {
                            chunk_hashes,
                            total_bytes: big_sealed.len() as u64,
                        }),
                        sealed_hint.clone(),
                        1_715_000_002,
                        big_sealed.len() as u32,
                    ))
                    .unwrap()
                    .to_vec(),
                ),
            )
            .await
            .expect("reference ingest"),
        )
        .unwrap();
        reply.message_id
    };

    // uids 3–5 — three ~700 KB messages that individually fit the page budget but do
    // not all fit one page together, so the budget guard's early-close is exercised.
    //
    // ⚠ Recalibrated for v2 (2026-07-18 write flip). These used to be *two* messages,
    // relying on v1's ~1.91× array expansion to make each stored envelope ~1.3 MB so
    // two overflowed the ~2,031,616-byte budget. v2 kills that expansion — an envelope
    // now costs its payload plus a small constant — so two ~700 KB messages fit
    // comfortably and the early-close never fired. Three do overflow (3 × 716,800 =
    // 2,150,400), restoring the property under the real post-flip sizes. Deliberately
    // kept **under** `MAIL_BODY_PART_CAP_BYTES` (1 MiB) so these stay plain inline
    // records: the continuation path is the big message's job in this test, not theirs.
    let mid_body = vec![b'm'; 700 * 1024];
    let small_3 = ingest_inline(mid_body.clone(), 1_715_000_003).await;
    let small_4 = ingest_inline(mid_body.clone(), 1_715_000_004).await;
    let small_5 = ingest_inline(mid_body, 1_715_000_005).await;

    let fetch = |after_uid: u32| async move {
        let reply: InboxFetchReply = decode(
            &dispatch(
                hr,
                recipient,
                "fauna.email.inbox.fetch",
                Bytes::from(
                    encode_canonical(&InboxFetchRequest {
                        after_uid,
                        limit: 0,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec(),
                ),
            )
            .await
            .expect("inbox fetch must succeed even with an over-frame message stored"),
        )
        .unwrap();
        reply
    };

    // The over-frame message crosses by REFERENCE.
    let page1 = fetch(0).await;
    let ids1: Vec<&[u8]> = page1
        .messages
        .iter()
        .map(|m| m.message_id.as_slice())
        .collect();
    assert_eq!(
        ids1,
        vec![
            small_1.as_slice(),
            big_id.as_slice(),
            small_3.as_slice(),
            small_4.as_slice()
        ],
        "the over-frame message now appears (by reference) between the inline ones"
    );
    // The whole reply must fit one frame — the reference is what keeps the big
    // message's presence cheap (a few hundred bytes, not a few MB).
    let encoded_len = encode_canonical(&page1).unwrap().len();
    assert!(
        encoded_len < 2 * 1024 * 1024,
        "the reply must fit one 2 MiB WS-RPC frame ({encoded_len} bytes)"
    );
    assert!(
        page1.more,
        "the page still closes early before the fourth message"
    );

    // The referenced message carries no inline envelope — only a `body_ref`.
    let big_msg = page1
        .messages
        .iter()
        .find(|m| m.message_id.as_slice() == big_id.as_slice())
        .expect("the big message is in the page");
    assert!(
        big_msg.sealed_envelope.is_empty(),
        "an over-frame message carries no inline envelope — it crossed by reference"
    );
    let body_ref = big_msg
        .body_ref
        .clone()
        .expect("an over-frame message carries a body reference");

    // ── Resolve the reference exactly as a client does ──────────────────────
    // Not a hand-rolled fetch+join: this is the REAL shared resolver the native
    // (`NestMailInboundSource::fetch`) and web (`resolveMailBodyRef`) receive
    // paths both call, driven over the real OPEN download route (no token)
    // against this nest's real serve. If the client's hash→key derivation, its
    // fetch, or its fail-closed join ever drifts from what the feed serves, this
    // is what catches it.
    let joined = fauna_mail::body_ref::resolve_referenced_mail_body(
        &OpenRouteChunkFetcher {
            base: format!("http://{}", h.addr),
            http: http.clone(),
        },
        &body_ref.chunk_hashes,
        body_ref.total_bytes,
    )
    .await
    .expect("the client resolver rejoins the referenced chunks to the declared total");

    // The rejoin is the exact OUTER segment envelope an inline serve would have
    // shipped — the client opens it identically to an inline `sealed_envelope`.
    let expected_outer =
        fauna_mail::segments::MailRecordEnvelope::new(big_sealed.clone(), sealed_hint.clone())
            .encode()
            .expect("re-encode the stored outer envelope");
    assert_eq!(
        joined, expected_outer,
        "the reference resolves to the exact stored outer envelope, byte-for-byte"
    );

    // …and it opens (outer decode → inner HPKE unseal) to the plaintext body.
    let outer = fauna_mail::segments::MailRecordEnvelope::decode(&joined)
        .expect("decode the rejoined outer envelope");
    let inner = MailRecordEnvelope::from_canonical_bytes(&outer.encrypted_body)
        .expect("the outer envelope's body is the inner sealed record");
    let opened = unseal_mail_record(&inner, &recipient_secret)
        .expect("the recipient opens the referenced body with the MSEK-derived secret");
    assert_eq!(
        opened.as_slice(),
        big_message().as_slice(),
        "an over-frame message read back over the feed's reference leg decrypts byte-for-byte"
    );

    // Paging is unchanged: resume past the page's last uid → the fourth message.
    let last_uid = page1.messages.last().unwrap().uid;
    let page2 = fetch(last_uid).await;
    let ids2: Vec<&[u8]> = page2
        .messages
        .iter()
        .map(|m| m.message_id.as_slice())
        .collect();
    assert_eq!(
        ids2,
        vec![small_5.as_slice()],
        "page 2 resumes past the referenced message and serves the remaining one"
    );
}

/// **The MDA-APPEND reference leg, end to end** (S9): a user filing a
/// too-large-for-the-frame message into their own mailbox via IMAP APPEND stages
/// the *already-sealed* body on the bulk-byte plane and the `fauna.bridges.append`
/// RPC carries only a `MailBodyRef`. This is the nest-side proof that the append
/// handler resolves that reference into the identical at-rest bytes an inline
/// APPEND would have stored, and serves them back byte-for-byte — the sealed-bytes
/// sibling of `an_over_frame_mail_body_crosses_by_reference...` above, differing
/// only in that the MDA both mints the token (D2: MailBody is BridgeMta|BridgeMda)
/// and seals to the actor's own key (seal-to-self, as `imap/append.go` does).
///
/// Owner doc: `smtp-server.md` § Message size limits (the MDA-APPEND upward leg).
/// What this does NOT prove, and the Go `append_test.go` / tier_3 e2e must: that
/// the *Go* MDA takes this path off a real IMAP `APPEND` literal.
#[tokio::test]
async fn an_over_frame_append_crosses_by_reference_and_returns_byte_for_byte() {
    let h = harness().await;
    let http = reqwest::Client::new();

    // The actor whose own MUA files mail into its own mailbox. seal-to-self, so
    // the recipient IS the sealer — one keypair, derived from the client-held MSEK.
    let recipient: [u8; 32] = [0x43; 32];
    let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&[0x5f; 32]);
    common::seed_recipient_seal_key(&h.state.db, &recipient, &[0x5f; 32]).await;

    // APPEND is BridgeMda-served, and the MDA also mints the MailBody token.
    let mda_actor: [u8; 32] = [0x23; 32];
    approve_bridge(&h.state.db, &mda_actor, BridgeRole::Mda, &[0x93; 32]).await;

    // ── The MDA seals to the actor's own key, exactly as imap/append.go does ──
    let body = big_message();
    let sealed_body = seal_to_recipient(&body, &recipient_pubkey)
        .expect("seal body")
        .to_canonical_bytes()
        .expect("encode sealed body envelope");
    let sealed_hint = seal_to_recipient(b"append-index-hint", &recipient_pubkey)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint envelope");
    assert!(
        fauna_mail::body_ref::mail_body_needs_reference(
            sealed_body.len() as u64,
            sealed_hint.len() as u64
        ),
        "the sealed APPEND body must be over the inline budget, else this proves nothing"
    );

    // ── MDA: mint a mail-body bulk token (D2 admits BridgeMda to MailBody) ──
    let mint_reply: MintBulkByteTokenReply = decode(
        &dispatch(
            &h,
            mda_actor,
            "fauna.bridges.mint_bulk_byte_token",
            Bytes::from(
                encode_canonical(&MintBulkByteTokenRequest {
                    actor_id: recipient.to_vec(),
                    folder: String::new(),
                    access: BulkByteAccess::Write,
                    purpose: BulkByteMintPurpose::MailBody,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("the MDA mints a mail-body token"),
    )
    .unwrap();

    // ── MDA: stage the sealed body over the REAL byte-plane write route ──
    let chunks = split_sealed_mail_body(&sealed_body);
    assert!(chunks.len() > 1, "a 6 MB body must span several chunks");
    let mut chunk_hashes = Vec::new();
    for c in &chunks {
        let resp = http
            .post(format!("http://{}/api/v1/chunks", h.addr))
            .header("Authorization", format!("Bearer {}", mint_reply.token))
            .header("X-Content-Hash", hex::encode(&c.hash))
            .body(c.bytes.clone())
            .send()
            .await
            .expect("chunk upload");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        chunk_hashes.push(serde_bytes::ByteBuf::from(c.hash.clone()));
    }

    // ── MDA: APPEND by reference — no inline body on the wire ──
    let append_req = AppendMessageRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: recipient.to_vec(),
        mailbox: "INBOX".into(),
        flags: vec![],
        encrypted_body: Vec::new(),
        encrypted_index_hint: sealed_hint,
        timestamp: 1_715_500_000,
        ciphertext_size: sealed_body.len() as u32,
        sender_domain: String::new(),
        body_ref: Some(fauna_protocol::bridge_routing::MailBodyRef {
            chunk_hashes,
            total_bytes: sealed_body.len() as u64,
        }),
    };
    let encoded = encode_canonical(&append_req).expect("encode append");
    assert!(
        encoded.len() < 2 * 1024 * 1024,
        "the append request must fit the WS-RPC frame ({} bytes)",
        encoded.len()
    );
    let append_reply: AppendMessageReply = decode(
        &dispatch(
            &h,
            mda_actor,
            "fauna.bridges.append",
            Bytes::from(encoded.to_vec()),
        )
        .await
        .expect("append by reference must succeed"),
    )
    .expect("decode append reply");
    let message_id = append_reply.message_id;
    assert_eq!(message_id.len(), 32);

    // ── MDA: fetch back — the reply carries a reference, not the inline bytes ──
    let fetch_reply: FetchMessageCiphertextReply = decode(
        &dispatch(
            &h,
            mda_actor,
            "fauna.bridges.fetch_message_ciphertext",
            Bytes::from(
                encode_canonical(&FetchMessageCiphertextRequest {
                    actor_id: recipient.to_vec(),
                    message_id: message_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("MDA fetch must succeed"),
    )
    .expect("decode fetch reply");
    let body_ref = match fetch_reply {
        FetchMessageCiphertextReply::Found {
            encrypted_body,
            ciphertext_size,
            body_ref,
            ..
        } => {
            assert!(
                encrypted_body.is_empty(),
                "an over-frame appended body must come back by reference"
            );
            assert_eq!(ciphertext_size as usize, sealed_body.len());
            body_ref.expect("reply must carry a body reference")
        }
        FetchMessageCiphertextReply::NotFound => panic!("the appended message must be fetchable"),
    };
    assert_eq!(body_ref.total_bytes, sealed_body.len() as u64);

    // ── GET the chunks back over the OPEN download route, rejoin, unseal ──
    let mut fetched: Vec<Vec<u8>> = Vec::new();
    for hash in &body_ref.chunk_hashes {
        let resp = http
            .get(format!(
                "http://{}/api/v1/chunks/{}",
                h.addr,
                hex::encode(hash)
            ))
            .send()
            .await
            .expect("chunk download");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        fetched.push(resp.bytes().await.unwrap().to_vec());
    }
    let rejoined = join_sealed_mail_body_checked(&fetched, body_ref.total_bytes)
        .expect("the chunks the reply named must rejoin to the declared total");
    assert_eq!(
        rejoined, sealed_body,
        "the appended body round-trips byte-for-byte through the reference legs"
    );

    let envelope =
        MailRecordEnvelope::from_canonical_bytes(&rejoined).expect("parse fetched envelope");
    let opened = unseal_mail_record(&envelope, &recipient_secret)
        .expect("the actor opens its own APPENDed ciphertext with the MSEK-derived secret");
    assert_eq!(
        opened.as_slice(),
        body.as_slice(),
        "a 6 MB APPENDed message decrypts byte-for-byte back to the literal that was filed"
    );
}

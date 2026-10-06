//! Moving a sealed mail body across the bulk-byte plane — nest's two halves.
//!
//! Owner doc: `docs/goal/behavior/mail-message-size.md` § Message size limits (the rule:
//! `docs/goal/architecture/transport.md` § Max frame). The 2 MiB WS-RPC cap is
//! permanent for every caller class, so a sealed body over
//! [`fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES`] crosses as a
//! **reference** — the ordered blake3 chunk hashes — and the bytes themselves ride
//! the content-addressed chunk store (`/api/v1/chunks`).
//!
//! Two directions, one store:
//!
//! * [`resolve_body_ref`] (**upward**) — a producer (the Go MTA on inbound
//!   delivery) staged the chunks with `POST /api/v1/chunks` and sent the reference;
//!   nest reads them back out of its *own local* blob store (a disk read, not a
//!   network fetch) and rejoins them. The rejoined bytes then take the identical
//!   `append_record` path an inline body takes, so the at-rest shape, the seal, and
//!   the quota charge are bit-for-bit the same either way. Only the transport moved.
//! * [`stage_sealed_body`] (**downward**) — the stored body is too large to hand
//!   back in a reply frame, so nest writes it to the same chunk store and returns a
//!   reference; the MDA GETs the chunks over the *open* download route (ciphertext
//!   by hash — no key, hence no token on this leg) and rejoins them.
//!
//! **Staging needs no lifecycle of its own.** A staged chunk is an orphan blob (no
//! manifest references it), and the existing blob GC already reaps orphans older
//! than its grace window while explicitly skipping blobs newer than the cutoff
//! (`backup/gc.rs`). That gives the goal doc's "unconsumed staging is TTL-GC'd" for
//! free, and it collects the *consumed* chunks too — once ingest has copied the
//! bytes into the segment, the staged copy is redundant and simply ages out. There
//! is no staging table, no TTL sweeper, and nothing to leak.
//!
//! **The plaintext legs — the staged envelope.** The two pairs above move
//! already-*sealed* bytes ([`MailBodyRef`]). The outbound queue pair and client
//! import move *plaintext*-derived bytes the nest must read, which the open
//! ciphertext-only store cannot hold directly (`smtp-server.md` § Message size
//! limits, *the staged-envelope rule*). [`stage_staged_body`] /
//! [`resolve_staged_body`] are their siblings: they AEAD-seal the plaintext under
//! a **fresh one-shot key** (`fauna_mail::staged_envelope`) before splitting, so
//! the store still holds only ciphertext (and a *unique* ciphertext per staging —
//! no `blake3(plaintext)` correlation), and the reference carries the key
//! ([`StagedBodyRef`]) inside the confidential WS-RPC. The GC story is identical
//! (fresh-key chunks are just more disposable orphans).
//!
//! **Every resolver is bounded before it reads (2026-09-28).** A reference's chunk
//! list comes off the request and is not self-verifying: it may name one staged
//! hash tens of thousands of times (34 bytes a repeat, well inside the 2 MiB
//! frame), and every repeat is a full chunk read. Reading the list first and
//! comparing the join to `total_bytes` after would buffer hundreds of GB for one
//! request — and `import_message` is `User`-class, so any user on the box could
//! OOM the nest for every tenant. So [`resolve_body_ref`] and
//! [`resolve_staged_body`] apply the shared bound (`fauna_mail::body_ref`):
//! `check_mail_body_ref_shape` refuses a declared total over the carrying leg's
//! ceiling, or a list longer than that total can split into, before the first
//! `store.get`; `MailBodyJoin` then refuses the chunk that carries the join past
//! the declared total. A resolver holds at most the declared total plus one
//! chunk, and the declared total is capped at the leg's ceiling, which each
//! caller passes in.

use std::sync::Arc;

use fauna_cbor::Value;
use fauna_core::data::ContentHash;
use fauna_core::secret::SecretByteBuf;
use fauna_mail::body_ref::{MailBodyJoin, check_mail_body_ref_shape, split_sealed_mail_body};
use fauna_mail::staged_envelope::{open_staged_body, seal_and_split_staged_body};
use fauna_protocol::bridge_routing::{MailBodyRef, StagedBodyRef};
use serde_bytes::ByteBuf;

use fauna_protocol::error::RpcError;

use crate::AppState;

/// The byte plane is not configured on this nest, so a body reference can be
/// neither staged nor resolved.
fn byte_plane_unavailable() -> RpcError {
    RpcError::new(
        "fauna.bridges.byte_plane_unavailable",
        "error.bridges.byte_plane_unavailable",
    )
}

/// Wire code for a body reference the producer got wrong — a mis-named,
/// reordered, or dropped chunk; a wrong key; or a lying total. Distinct from
/// [`byte_plane_unavailable`] (a nest-wide infra condition): a bad reference is
/// the *caller's* fault, so a batched consumer (client import) can fail just that
/// one item and let its siblings through, while an infra failure aborts the call.
pub const INVALID_BODY_REF_CODE: &str = "fauna.bridges.invalid_body_ref";

fn invalid_body_ref(reason: &str) -> RpcError {
    let mut e = RpcError::new(INVALID_BODY_REF_CODE, "error.bridges.invalid_body_ref");
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

/// Resolve an upward body reference back to the sealed bytes the producer staged.
///
/// Fails closed on every way a reference can lie: a chunk that is not in the store,
/// or a chunk list whose join does not match the declared total (wrong chunks,
/// wrong order, or a dropped one). A body that stored cleanly while being the wrong
/// message is precisely the outcome this must not have.
///
/// **A chunk under a legal-takedown withhold is refused before it is read.**
/// The digests here come off the REQUEST, and this join is authenticated only
/// by the declared total — unlike [`resolve_staged_body`], whose AEAD tag over
/// the join is computed under the caller's own key and so cannot be satisfied
/// by a blob they merely named. So without this an authenticated bridge
/// session could name a withheld digest as a body chunk, pad to the declared
/// total, and read the compelled bytes straight back out as its own message:
/// the same path-substitution bypass the four routes onto the store close,
/// wearing a mail reference instead of a URL (`moderation.md` § Legal takedown
/// → *The blob-serve door*; found by
/// `backup::service`'s `every_blob_store_serve_path_is_partitioned`).
///
/// **Bounded before and during the read** (module docs, *Every resolver is
/// bounded before it reads*).
/// `max_total_bytes` is the sealed-bytes ceiling of the carrying leg.
pub async fn resolve_body_ref(
    state: &Arc<AppState>,
    body_ref: &MailBodyRef,
    max_total_bytes: u64,
) -> Result<Vec<u8>, RpcError> {
    let backup_svc = state
        .backup_service
        .as_ref()
        .ok_or_else(byte_plane_unavailable)?;
    let store = backup_svc.local_blob_store();

    check_mail_body_ref_shape(
        body_ref.chunk_hashes.len(),
        body_ref.total_bytes,
        max_total_bytes,
    )
    .map_err(|e| invalid_body_ref(&e.to_string()))?;
    let mut join = MailBodyJoin::new(body_ref.total_bytes);
    for (i, h) in body_ref.chunk_hashes.iter().enumerate() {
        let digest: [u8; 32] = h
            .as_slice()
            .try_into()
            .map_err(|_| invalid_body_ref("chunk hash must be 32 bytes"))?;
        let hash = ContentHash::from_digest_raw(digest);
        if crate::blob_routes::is_legally_withheld(&state.db, &digest).await {
            tracing::info!(
                chunk = i,
                hash = hex::encode(digest),
                "body_ref chunk withheld — legal takedown"
            );
            // Disclosed, not hidden: the detail names the reason, the way the
            // routes onto the store answer 451 rather than 404.
            return Err(invalid_body_ref(
                "body reference names a blob withheld under a legal takedown",
            ));
        }
        let raw = store
            .get(&hash)
            .await
            .map_err(|e| {
                tracing::error!("body_ref chunk read error: {e}");
                byte_plane_unavailable()
            })?
            .ok_or_else(|| {
                // The staging window is the blob GC's grace period, which is orders
                // of magnitude longer than an upload→RPC round trip; a miss here
                // means the producer never staged this chunk, not that it expired.
                tracing::warn!(
                    chunk = i,
                    hash = hex::encode(digest),
                    "body_ref names a chunk that is not staged"
                );
                invalid_body_ref("body reference names a chunk that is not staged")
            })?;
        // Symmetric with `upload_chunk`'s encode — `decode_blob ∘ encode_blob` is the
        // identity, so this yields exactly the bytes the producer uploaded.
        let decoded =
            crate::backup::decode_blob(&raw, backup_svc.encryption_key()).map_err(|e| {
                tracing::error!("body_ref chunk decode error: {e}");
                byte_plane_unavailable()
            })?;
        join.push(&decoded)
            .map_err(|e| invalid_body_ref(&e.to_string()))?;
    }

    join.finish().map_err(|e| invalid_body_ref(&e.to_string()))
}

/// Stage a stored sealed body on the byte plane for a consumer to GET back, and
/// return the reference naming its chunks.
///
/// Content-addressed, so this is idempotent: re-fetching the same message re-derives
/// the same hashes and the `put`s land on bytes already present.
pub async fn stage_sealed_body(
    state: &Arc<AppState>,
    body: &[u8],
) -> Result<MailBodyRef, RpcError> {
    let backup_svc = state
        .backup_service
        .as_ref()
        .ok_or_else(byte_plane_unavailable)?;
    let store = backup_svc.local_blob_store();

    let chunks = split_sealed_mail_body(body);
    let mut chunk_hashes = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let digest: [u8; 32] = c
            .hash
            .as_slice()
            .try_into()
            .expect("body_ref chunk hashes are blake3 digests");
        let hash = ContentHash::from_digest_raw(digest);

        let encoded = crate::backup::encode_blob(
            &c.bytes,
            backup_svc.encryption_key(),
            backup_svc.compression(),
        )
        .map_err(|e| {
            tracing::error!("body_ref chunk encode error: {e}");
            byte_plane_unavailable()
        })?;
        store.put(&hash, &encoded).await.map_err(|e| {
            tracing::error!("body_ref chunk write error: {e}");
            byte_plane_unavailable()
        })?;
        // Same metadata row `upload_chunk` writes, so the blob GC sees these exactly
        // as it sees a staged upload: an orphan chunk, protected by the grace window,
        // reaped once it ages out. A write failure fails the request (not warn-and-
        // proceed): the row is the blob's only durable trace until the mail record
        // references it, and the retry re-puts idempotently.
        state
            .db
            .put_blob_metadata(&digest, c.bytes.len() as i64, "chunk", None, None)
            .await
            .map_err(|e| {
                tracing::error!("failed to record staged mail-body chunk metadata: {e}");
                byte_plane_unavailable()
            })?;
        chunk_hashes.push(ByteBuf::from(digest.to_vec()));
    }

    Ok(MailBodyRef {
        chunk_hashes,
        total_bytes: body.len() as u64,
    })
}

/// Seal a **plaintext** body under a fresh one-shot AEAD key, stage the resulting
/// ciphertext on the byte plane, and return the [`StagedBodyRef`] naming its
/// chunks **and** carrying the key (the staged-envelope rule — the plaintext
/// sibling of [`stage_sealed_body`]).
///
/// The store still holds only ciphertext, and a fresh key per call means the
/// ciphertext (hence the chunk hashes) is unique per staging — no
/// `blake3(plaintext)` correlation, and no cross-staging dedup to lose. Not
/// idempotent by design: two calls on identical plaintext produce disjoint
/// chunks (both age out as GC orphans).
pub async fn stage_staged_body(
    state: &Arc<AppState>,
    plaintext: &[u8],
) -> Result<StagedBodyRef, RpcError> {
    let backup_svc = state
        .backup_service
        .as_ref()
        .ok_or_else(byte_plane_unavailable)?;
    let store = backup_svc.local_blob_store();

    // Seal first, then split the CIPHERTEXT — never the plaintext (the open
    // download route is safe only because the store holds ciphertext).
    let (key, chunks, total_bytes) = seal_and_split_staged_body(plaintext);

    let mut chunk_hashes = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let digest: [u8; 32] = c
            .hash
            .as_slice()
            .try_into()
            .expect("body_ref chunk hashes are blake3 digests");
        let hash = ContentHash::from_digest_raw(digest);

        let encoded = crate::backup::encode_blob(
            &c.bytes,
            backup_svc.encryption_key(),
            backup_svc.compression(),
        )
        .map_err(|e| {
            tracing::error!("staged body chunk encode error: {e}");
            byte_plane_unavailable()
        })?;
        store.put(&hash, &encoded).await.map_err(|e| {
            tracing::error!("staged body chunk write error: {e}");
            byte_plane_unavailable()
        })?;
        // Same orphan-chunk metadata row as `stage_sealed_body`, so the blob GC
        // reaps these on the same grace window. A write failure fails the
        // request (the content-addressed retry re-puts idempotently — though the
        // key differs, so a retry restages under a fresh reference).
        state
            .db
            .put_blob_metadata(&digest, c.bytes.len() as i64, "chunk", None, None)
            .await
            .map_err(|e| {
                tracing::error!("failed to record staged mail-body chunk metadata: {e}");
                byte_plane_unavailable()
            })?;
        chunk_hashes.push(ByteBuf::from(digest.to_vec()));
    }

    Ok(StagedBodyRef {
        chunk_hashes,
        total_bytes,
        key: SecretByteBuf::from(key),
    })
}

/// Resolve a [`StagedBodyRef`] back to the plaintext its producer sealed — the
/// plaintext sibling of [`resolve_body_ref`].
///
/// Fails closed twice over: the join is pinned against the declared (sealed)
/// total, and the AEAD open then authenticates the entire rejoin, so a body that
/// stored cleanly while being the wrong message — a mis-named, reordered, or
/// dropped chunk — never survives to be enqueued.
///
/// **Bounded before and during the read** (module docs, *Every resolver is
/// bounded before it reads*).
/// `max_total_bytes` is the carrying leg's ceiling on the **sealed** total: its
/// plaintext ceiling plus
/// [`fauna_mail::staged_envelope::STAGED_ENVELOPE_OVERHEAD_BYTES`].
pub async fn resolve_staged_body(
    state: &Arc<AppState>,
    staged: &StagedBodyRef,
    max_total_bytes: u64,
) -> Result<Vec<u8>, RpcError> {
    let backup_svc = state
        .backup_service
        .as_ref()
        .ok_or_else(byte_plane_unavailable)?;
    let store = backup_svc.local_blob_store();

    check_mail_body_ref_shape(
        staged.chunk_hashes.len(),
        staged.total_bytes,
        max_total_bytes,
    )
    .map_err(|e| invalid_body_ref(&e.to_string()))?;
    let mut join = MailBodyJoin::new(staged.total_bytes);
    for (i, h) in staged.chunk_hashes.iter().enumerate() {
        let digest: [u8; 32] = h
            .as_slice()
            .try_into()
            .map_err(|_| invalid_body_ref("chunk hash must be 32 bytes"))?;
        let hash = ContentHash::from_digest_raw(digest);
        let raw = store
            .get(&hash)
            .await
            .map_err(|e| {
                tracing::error!("staged body chunk read error: {e}");
                byte_plane_unavailable()
            })?
            .ok_or_else(|| {
                tracing::warn!(
                    chunk = i,
                    hash = hex::encode(digest),
                    "staged body reference names a chunk that is not staged"
                );
                invalid_body_ref("staged body reference names a chunk that is not staged")
            })?;
        let decoded =
            crate::backup::decode_blob(&raw, backup_svc.encryption_key()).map_err(|e| {
                tracing::error!("staged body chunk decode error: {e}");
                byte_plane_unavailable()
            })?;
        join.push(&decoded)
            .map_err(|e| invalid_body_ref(&e.to_string()))?;
    }

    let sealed = join
        .finish()
        .map_err(|e| invalid_body_ref(&e.to_string()))?;
    open_staged_body(&sealed, staged.key.as_slice()).map_err(|e| invalid_body_ref(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No-false-ACK contract, staging arm (the fix; pinned so the
    /// arm can't silently revert to warn-and-proceed): when the metadata row —
    /// a staged chunk's only durable trace until the mail record references
    /// it — cannot be written, staging must fail rather than return a
    /// reference; the content-addressed retry re-puts idempotently.
    #[tokio::test]
    async fn stage_sealed_body_fails_when_the_metadata_write_fails() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().to_path_buf();
        std::mem::forget(blob_dir); // outlive the call; never deleted under test
        let backup_svc = Arc::new(
            crate::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
                .unwrap(),
        );
        // Fault injection: rename the table out from under `put_blob_metadata`,
        // the closest in-process stand-in for a mid-request sqlite write error.
        db.execute_batch("ALTER TABLE blob_metadata RENAME TO blob_metadata_fault;")
            .await
            .unwrap();
        let state = Arc::new(AppState {
            backup_service: Some(backup_svc),
            ..AppState::for_test(db)
        });
        let staged = stage_sealed_body(&state, b"sealed mail body bytes for staging").await;
        assert!(
            staged.is_err(),
            "a failed metadata write must fail the staging, not return a body reference"
        );
    }

    /// A state whose byte plane is real (temp dir), for the staged round trips.
    fn state_with_blob_plane() -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().to_path_buf();
        std::mem::forget(blob_dir); // outlive the call; never deleted under test
        let backup_svc = Arc::new(
            crate::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
                .unwrap(),
        );
        Arc::new(AppState {
            backup_service: Some(backup_svc),
            ..AppState::for_test(db)
        })
    }

    #[tokio::test]
    async fn staged_body_round_trips_through_stage_and_resolve() {
        // Plaintext → one-shot-sealed ciphertext chunks → resolve back
        // byte-for-byte. Multi-chunk so the split/join legs genuinely engage.
        let state = state_with_blob_plane();
        let plaintext: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let sref = stage_staged_body(&state, &plaintext).await.unwrap();
        assert!(
            sref.chunk_hashes.len() > 1,
            "a 5 MiB body must span multiple chunks"
        );
        assert_eq!(sref.key.len(), 32, "the one-shot key rides the reference");
        // The sealed total is larger than the plaintext (nonce + AEAD tag).
        assert!(sref.total_bytes > plaintext.len() as u64);
        let recovered = resolve_staged_body(&state, &sref, u64::MAX).await.unwrap();
        assert_eq!(recovered, plaintext);
    }

    /// **A body reference naming a withheld blob is refused** — the mail byte
    /// plane is a route onto the blob store like any other (`moderation.md`
    /// § Legal takedown → *The blob-serve door*), found by
    /// `backup::service`'s partition rather than by a review.
    ///
    /// The digests come off the request, and this join is checked only against
    /// the declared total, so an authenticated bridge session could otherwise
    /// name a taken-down post's photo as its own message body and read the
    /// compelled bytes straight back out. The staged twin needs no gate — its
    /// AEAD tag is computed under the caller's own key — and the last half
    /// here asserts that asymmetry adversarially: a staged reference naming
    /// the withheld digest itself, its declared total matched so the join
    /// passes, still resolves to nothing.
    #[tokio::test]
    async fn a_body_ref_naming_a_withheld_blob_is_refused() {
        let state = state_with_blob_plane();
        let body = b"the compelled bytes, re-read as somebody's own mail".to_vec();
        let bref = stage_sealed_body(&state, &body).await.unwrap();
        assert_eq!(
            resolve_body_ref(&state, &bref, u64::MAX).await.unwrap(),
            body,
            "baseline: an unwithheld reference resolves byte for byte"
        );

        let digest: [u8; 32] = bref.chunk_hashes[0].as_slice().try_into().unwrap();
        state
            .db
            .replace_blob_legal_withhold(&[digest])
            .await
            .unwrap();
        let err = resolve_body_ref(&state, &bref, u64::MAX)
            .await
            .expect_err("a withheld chunk must refuse, not resolve");
        assert_eq!(
            err.code, INVALID_BODY_REF_CODE,
            "the refusal is the caller's-fault class — they named the digest — so a \
             batched consumer fails just this item"
        );

        // The staged plane's own reference over the same store needs no gate:
        // its join is AEAD-authenticated under the key the caller's reference
        // carries, so a blob merely NAMED cannot satisfy it.
        let sref = stage_staged_body(&state, b"an ordinary outbound body")
            .await
            .unwrap();
        assert_eq!(
            resolve_staged_body(&state, &sref, u64::MAX).await.unwrap(),
            b"an ordinary outbound body".to_vec(),
            "the staged plane is unaffected by an unrelated withheld digest"
        );
        // …and naming the withheld digest in a staged reference is exactly
        // that. The caller holds a key (any key — here one of their own) and
        // the blob's own length, so the rejoin matches its declared total; the
        // open still fails, because no key the caller holds made that tag.
        let forged = StagedBodyRef {
            chunk_hashes: vec![ByteBuf::from(digest.to_vec())],
            total_bytes: body.len() as u64,
            key: SecretByteBuf::from(vec![0x5au8; 32]),
        };
        let err = resolve_staged_body(&state, &forged, u64::MAX)
            .await
            .expect_err("a staged reference naming a withheld blob must not resolve");
        assert_eq!(
            err.code, INVALID_BODY_REF_CODE,
            "the refusal is the caller's-fault class, the one any wrong key gets"
        );

        // Restore re-resolves the very same bytes: nothing was deleted.
        state.db.replace_blob_legal_withhold(&[]).await.unwrap();
        assert_eq!(
            resolve_body_ref(&state, &bref, u64::MAX).await.unwrap(),
            body,
            "restore=true resolves the same body, byte for byte"
        );
    }

    /// A reference repeating `hash` `n` times — the repeat-walk shape.
    fn repeated(hash: &ByteBuf, n: usize) -> Vec<ByteBuf> {
        std::iter::repeat_n(hash.clone(), n).collect()
    }

    /// The refusal's free-form detail (what distinguishes a shape refusal,
    /// which reads nothing, from a join that read every chunk first).
    fn detail(e: &RpcError) -> String {
        match e.details.as_deref() {
            Some(Value::String(s)) => s.clone(),
            other => panic!("expected a string detail, got {other:?}"),
        }
    }

    /// **The repeat walk** — a staged reference that repeats one staged hash far past
    /// what its `total_bytes` allows is refused on its shape, before a single
    /// chunk is read: any user can send one through `import_message`, and a
    /// resolver that read every repeat before the total check buffered
    /// hundreds of GB for one request. The shape refusal fires even when the
    /// named chunks are not staged at all — proof no read happened.
    #[tokio::test]
    async fn a_staged_ref_repeating_one_hash_is_refused_before_any_read() {
        let state = state_with_blob_plane();
        let sref = stage_staged_body(&state, b"one small staged body")
            .await
            .unwrap();
        let forged = StagedBodyRef {
            chunk_hashes: repeated(&sref.chunk_hashes[0], 5_000),
            total_bytes: sref.total_bytes,
            key: sref.key.clone(),
        };
        let err = resolve_staged_body(&state, &forged, u64::MAX)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_BODY_REF_CODE);
        assert!(
            detail(&err).contains("chunks"),
            "a shape refusal, got: {}",
            detail(&err)
        );

        let unstaged = StagedBodyRef {
            chunk_hashes: repeated(&ByteBuf::from(vec![0xabu8; 32]), 5_000),
            total_bytes: 100,
            key: sref.key.clone(),
        };
        let err = resolve_staged_body(&state, &unstaged, u64::MAX)
            .await
            .unwrap_err();
        assert!(
            !detail(&err).contains("not staged"),
            "the shape check must precede every read, got: {}",
            detail(&err)
        );
    }

    /// The running total: a single-hash reference within the count bound whose
    /// chunk is larger than the declared total is refused at that chunk (an
    /// overrun), not joined whole and compared after.
    #[tokio::test]
    async fn a_staged_ref_whose_chunk_overruns_its_total_is_refused_at_that_chunk() {
        let state = state_with_blob_plane();
        let sref = stage_staged_body(&state, &[9u8; 4096]).await.unwrap();
        let forged = StagedBodyRef {
            chunk_hashes: sref.chunk_hashes.clone(),
            total_bytes: 100,
            key: sref.key.clone(),
        };
        let err = resolve_staged_body(&state, &forged, u64::MAX)
            .await
            .unwrap_err();
        assert!(
            detail(&err).contains("passed"),
            "an overrun refusal, got: {}",
            detail(&err)
        );
    }

    /// The repeat walk, the sealed sibling: the MTA-ingest and IMAP-APPEND legs'
    /// [`resolve_body_ref`] applies the same shape bound before any read.
    #[tokio::test]
    async fn a_body_ref_repeating_one_hash_is_refused_before_any_read() {
        let state = state_with_blob_plane();
        let bref = stage_sealed_body(&state, b"a sealed body").await.unwrap();
        let forged = MailBodyRef {
            chunk_hashes: repeated(&bref.chunk_hashes[0], 5_000),
            total_bytes: bref.total_bytes,
        };
        let err = resolve_body_ref(&state, &forged, u64::MAX)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_BODY_REF_CODE);
        assert!(
            detail(&err).contains("chunks"),
            "a shape refusal, got: {}",
            detail(&err)
        );

        let unstaged = MailBodyRef {
            chunk_hashes: repeated(&ByteBuf::from(vec![0xabu8; 32]), 5_000),
            total_bytes: 100,
        };
        let err = resolve_body_ref(&state, &unstaged, u64::MAX)
            .await
            .unwrap_err();
        assert!(
            !detail(&err).contains("not staged"),
            "the shape check must precede every read, got: {}",
            detail(&err)
        );
    }

    /// A declared total over the carrying leg's ceiling is refused before any
    /// read — both resolvers, unstaged hashes as the no-read witness.
    #[tokio::test]
    async fn a_reference_over_its_legs_ceiling_is_refused_before_any_read() {
        let state = state_with_blob_plane();
        let unstaged = vec![ByteBuf::from(vec![0xabu8; 32])];
        let err = resolve_body_ref(
            &state,
            &MailBodyRef {
                chunk_hashes: unstaged.clone(),
                total_bytes: 1001,
            },
            1000,
        )
        .await
        .unwrap_err();
        assert!(detail(&err).contains("ceiling"), "got: {}", detail(&err));
        let err = resolve_staged_body(
            &state,
            &StagedBodyRef {
                chunk_hashes: unstaged,
                total_bytes: 1001,
                key: SecretByteBuf::from(vec![0u8; 32]),
            },
            1000,
        )
        .await
        .unwrap_err();
        assert!(detail(&err).contains("ceiling"), "got: {}", detail(&err));
    }

    #[tokio::test]
    async fn resolve_staged_body_fails_closed_on_a_wrong_key() {
        // The reference carries the key, but a corrupted key must fail the AEAD
        // open rather than yield the wrong plaintext (or empty bytes).
        let state = state_with_blob_plane();
        let plaintext = b"a secret outbound message body".to_vec();
        let mut sref = stage_staged_body(&state, &plaintext).await.unwrap();
        let mut bad_key = sref.key.to_vec();
        bad_key[0] ^= 0x01;
        sref.key = SecretByteBuf::from(bad_key);
        assert!(
            resolve_staged_body(&state, &sref, u64::MAX).await.is_err(),
            "a wrong key must fail closed, never recover partial or empty bytes"
        );
    }
}

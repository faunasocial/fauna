//! Splitting a sealed mail body across the bulk-byte plane, and joining it back.
//!
//! Owner doc: `docs/goal/behavior/mail-message-size.md` § Message size limits (the
//! rule: `docs/goal/architecture/transport.md` § Max frame; the mechanism
//! mirrored: `docs/goal/behavior/webdav-server.md` § Bulk-byte plane).
//!
//! A sealed body at or below [`transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES`]
//! rides the RPC inline exactly as it always has. Above it, the producer stages
//! the *sealed* bytes on the byte plane (`POST /api/v1/chunks`, one request per
//! chunk) and the RPC carries a **reference** — the ordered chunk hashes — in
//! place of the bytes. Nest resolves the reference from its own local blob store
//! and hands the rejoined bytes to the same `append_record` path an inline body
//! takes, so the at-rest shape, the seal, and the quota charge are bit-for-bit
//! identical either way. Only the transport moves.
//!
//! Three properties this module exists to keep true:
//!
//! 1. **One implementation of the split.** The Go MTA stages, nest rejoins, and
//!    the Go MDA re-fetches — all three call these functions over UniFFI rather
//!    than re-deriving a chunking rule per language (priority #2). A disagreement
//!    about where chunk boundaries fall, or about which hash keys a chunk, would
//!    corrupt mail silently.
//! 2. **The hash IS the store key.** [`MAIL_BODY_CHUNK_BYTES`] chunks are keyed by
//!    `blake3(chunk)`, which is precisely what `POST /api/v1/chunks` verifies
//!    against its `X-Content-Hash` header and stores under
//!    (`chunk_routes.rs::resolve_verified_chunk_hash`). There is no separate
//!    manifest object: a mail reference is just the ordered hash list, so this
//!    path never touches the `ChunkManifest` type or its fail-closed decode.
//! 3. **Chunk size is bounded by the route, not by taste.** The byte plane caps a
//!    request body at 10 MiB (`lib.rs::CHUNK_BLOB_BODY_LIMIT`); 4 MiB leaves ample
//!    headroom for the encode/compression envelope the nest wraps a blob in.

use crate::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES;

/// Bytes per staged chunk of a sealed mail body.
///
/// Sized against the byte plane's 10 MiB per-request body limit
/// (`bins/fauna-nest/src/lib.rs::CHUNK_BLOB_BODY_LIMIT`), not against a
/// preference: a chunk plus the nest's at-rest encode envelope must fit one
/// `POST /api/v1/chunks`. At 4 MiB a 50 MB `max_message_bytes` body is 13
/// chunks — 416 bytes of hashes on the RPC, against the 2 MiB frame.
pub const MAIL_BODY_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// One staged chunk: the bytes, and the blake3 digest that keys them in the
/// content-addressed store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailBodyChunk {
    /// `blake3(bytes)` — the 32-byte store key. Sent as hex in `X-Content-Hash`
    /// on upload, and carried (raw) in the RPC's body reference.
    pub hash: Vec<u8>,
    /// The chunk's sealed bytes.
    pub bytes: Vec<u8>,
}

/// Does a sealed body + sealed index hint have to cross by reference?
///
/// This is the single switchover predicate. Both halves count: the index hint is
/// input-dependent (a unique-word-dense body grows it toward the body's own
/// size), so a body comfortably under the budget can still assemble a request
/// over the frame — which is exactly the bug the old post-seal guard existed to
/// turn into a permanent `552` and which the reference leg now turns into a
/// delivery.
pub fn mail_body_needs_reference(sealed_body_len: u64, sealed_hint_len: u64) -> bool {
    sealed_body_len.saturating_add(sealed_hint_len) > u64::from(INLINE_MAIL_REQUEST_BUDGET_BYTES)
}

/// Split a sealed body into content-addressed chunks, in order.
///
/// An empty body yields no chunks (a reference to nothing is not a reference).
/// The final chunk is short unless the body divides evenly.
pub fn split_sealed_mail_body(body: &[u8]) -> Vec<MailBodyChunk> {
    body.chunks(MAIL_BODY_CHUNK_BYTES)
        .map(|c| MailBodyChunk {
            hash: blake3::hash(c).as_bytes().to_vec(),
            bytes: c.to_vec(),
        })
        .collect()
}

/// Rejoin staged chunks, in the order the reference listed them.
///
/// Integrity is not re-checked here: each chunk was fetched *by its own hash*
/// from a content-addressed store, so the bytes cannot be other than the bytes
/// that hash names. What the caller must still check is the **total** — see
/// [`join_sealed_mail_body_checked`], which is what nest uses, because the
/// number and order of chunks come from the (bridge-supplied) reference rather
/// than from the store.
pub fn join_sealed_mail_body(chunks: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = chunks.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for c in chunks {
        out.extend_from_slice(c);
    }
    out
}

/// Rejoin staged chunks and pin the result against the length the reference
/// promised.
///
/// The chunk *contents* are self-verifying (content-addressed), but the chunk
/// *list* is not: a producer that named the wrong chunks, named them in the
/// wrong order, or dropped one would otherwise hand nest a body that seals and
/// stores cleanly while being the wrong message. The declared total is the cheap
/// end-to-end check that catches all three, and it fails closed.
pub fn join_sealed_mail_body_checked(
    chunks: &[Vec<u8>],
    declared_total_bytes: u64,
) -> Result<Vec<u8>, MailBodyRefError> {
    let mut join = MailBodyJoin::new(declared_total_bytes);
    for c in chunks {
        join.push(c)?;
    }
    join.finish()
}

/// The most chunks [`split_sealed_mail_body`] yields for a body of
/// `total_bytes` — and so the most an honest reference can name.
///
/// A bound, not an exact count: a future, *larger* chunk size would name fewer
/// chunks for the same total and still pass, while a list naming more than the
/// split could have produced is refused before any chunk is read.
pub fn max_mail_body_chunk_count(total_bytes: u64) -> u64 {
    total_bytes.div_ceil(MAIL_BODY_CHUNK_BYTES as u64)
}

/// Check a body reference's **shape** before a single chunk is read — the
/// first two of the three bounds every resolver applies (the third is
/// [`MailBodyJoin`]'s running total).
///
/// The chunk list comes off the request, and nothing about it is
/// self-verifying: a reference may name the same staged hash tens of thousands
/// of times at 34 bytes a repeat, and a resolver that read every entry before
/// comparing the join to the declared total would buffer hundreds of GB for one
/// request. So, before any read:
///
/// 1. the declared total may not exceed `max_total_bytes`, the ceiling the
///    carrying leg admits (the caller's product or wire ceiling, plus whatever
///    envelope the leg's bytes are wrapped in);
/// 2. the list may not name more chunks than [`max_mail_body_chunk_count`]
///    allows for that total.
///
/// Together with the running total they cap what a resolver ever holds at the
/// declared total plus one chunk, and the declared total at the leg's ceiling.
pub fn check_mail_body_ref_shape(
    chunk_count: usize,
    declared_total_bytes: u64,
    max_total_bytes: u64,
) -> Result<(), MailBodyRefError> {
    if declared_total_bytes > max_total_bytes {
        return Err(MailBodyRefError::OverCeiling {
            declared: declared_total_bytes,
            ceiling: max_total_bytes,
        });
    }
    let max = max_mail_body_chunk_count(declared_total_bytes);
    if chunk_count as u64 > max {
        return Err(MailBodyRefError::TooManyChunks {
            count: chunk_count as u64,
            declared: declared_total_bytes,
            max,
        });
    }
    Ok(())
}

/// Rejoin a body reference's chunks one at a time against a **running total**:
/// the moment the bytes read pass the declared total, the join refuses, so a
/// resolver never holds more than the declared total plus the one chunk that
/// overran it. [`Self::finish`] then pins the exact length.
///
/// The buffer grows as chunks arrive rather than being sized from the declared
/// total, so a lying total costs nothing up front either.
#[derive(Debug)]
pub struct MailBodyJoin {
    declared: u64,
    body: Vec<u8>,
}

impl MailBodyJoin {
    pub fn new(declared_total_bytes: u64) -> Self {
        Self {
            declared: declared_total_bytes,
            body: Vec::with_capacity(
                declared_total_bytes.min(MAIL_BODY_CHUNK_BYTES as u64) as usize
            ),
        }
    }

    /// Append the next chunk, refusing once the join passes the declared total.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), MailBodyRefError> {
        let after = self.body.len() as u64 + chunk.len() as u64;
        if after > self.declared {
            return Err(MailBodyRefError::Overrun {
                declared: self.declared,
                read: after,
            });
        }
        self.body.extend_from_slice(chunk);
        Ok(())
    }

    /// The joined body, pinned against the declared total.
    pub fn finish(self) -> Result<Vec<u8>, MailBodyRefError> {
        if self.body.len() as u64 != self.declared {
            return Err(MailBodyRefError::TotalBytesMismatch {
                declared: self.declared,
                actual: self.body.len() as u64,
            });
        }
        Ok(self.body)
    }
}

/// Why a body reference could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailBodyRefError {
    #[error("body reference declared {declared} bytes but its chunks joined to {actual}")]
    TotalBytesMismatch { declared: u64, actual: u64 },
    #[error("body reference declares {declared} bytes, over this leg's {ceiling}-byte ceiling")]
    OverCeiling { declared: u64, ceiling: u64 },
    #[error(
        "body reference names {count} chunks, but a {declared}-byte body splits into at most {max}"
    )]
    TooManyChunks { count: u64, declared: u64, max: u64 },
    #[error("body reference declared {declared} bytes but its chunks passed that at {read}")]
    Overrun { declared: u64, read: u64 },
}

/// Fetch the chunks a body reference names and rejoin them to the sealed bytes
/// it stands for.
///
/// The **reader half** of the client-feed reference leg (`smtp-server.md`
/// § Message size limits): a first-party inbox/sent message whose stored *outer*
/// envelope exceeds the 2 MiB WS-RPC frame arrives with an empty
/// `sealed_envelope` and an `InboxMessage.body_ref`; the receive path calls this
/// to get the exact envelope bytes back, then opens them through
/// `open_inbound_record_hybrid` exactly as if they had arrived inline.
///
/// Shared across every app on purpose (priority #2): the native receive path
/// (`NestMailInboundSource::fetch`, behind all six native apps) and the web
/// one both call this, so no target can drift on the hash→key derivation or on
/// the fail-closed total. It is the client twin of nest's
/// `mail_body_plane::resolve_body_ref`, and deliberately *not* the same code:
/// nest reads its own local blob store and must strip at-rest framing, while a
/// client GETs the open download route, which already returns decoded bytes.
///
/// `fetcher` is [`fauna_core::file_download::BlobFetcher`] — the established
/// cross-target seam — so the native (pinned reqwest + shared bearer) and web
/// (gloo-net) bindings, and a fake in tests, all drive this same body.
///
/// Fails closed on both axes a reference can lie about: a hash that is not 32
/// bytes is rejected before any fetch, and the rejoined length must equal the
/// declared total ([`join_sealed_mail_body_checked`]). Chunk *contents* need no
/// check — each was fetched by its own content address.
#[cfg(feature = "body-ref-resolve")]
pub async fn resolve_referenced_mail_body(
    fetcher: &dyn fauna_core::file_download::BlobFetcher,
    chunk_hashes: &[impl AsRef<[u8]>],
    total_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    // A served reference names no more chunks than its total can split into,
    // checked before any fetch (the same shape bound nest applies on the way
    // in; a client has no leg ceiling of its own, so only the count applies).
    check_mail_body_ref_shape(chunk_hashes.len(), total_bytes, u64::MAX)?;
    // Width-check every hash *before* spending a request: the wire type is
    // `Vec<ByteBuf>`, so nothing but this pins the 32 bytes a store key is.
    let mut store_keys = Vec::with_capacity(chunk_hashes.len());
    for (i, h) in chunk_hashes.iter().enumerate() {
        let raw = h.as_ref();
        let digest: [u8; 32] = raw.try_into().map_err(|_| {
            anyhow::anyhow!(
                "mail body reference chunk {i}: hash must be 32 bytes, got {}",
                raw.len()
            )
        })?;
        store_keys.push(fauna_core::data::ContentHash::from_digest_raw(digest));
    }

    // `relative_path` is error context only, never a lookup key
    // (`fauna_core::file_download::BlobFetcher::fetch_chunks`).
    let chunks = fetcher.fetch_chunks(&store_keys, "mail body").await?;

    Ok(join_sealed_mail_body_checked(&chunks, total_bytes)?)
}

#[cfg(feature = "uniffi")]
mod ffi {
    use super::*;

    // NOTE (2026-07-15 dark-rail audit): the `mail_body_chunk_bytes` export
    // was deleted — the chunk size is consumed only by the Rust-side
    // `split_sealed_mail_body`; the Go bridge splits/joins through the export
    // pair below and never needed the constant.

    /// UniFFI: does this sealed pair need to cross by reference?
    #[uniffi::export]
    pub fn mail_body_needs_reference(sealed_body_len: u64, sealed_hint_len: u64) -> bool {
        super::mail_body_needs_reference(sealed_body_len, sealed_hint_len)
    }

    /// UniFFI: split a sealed body into content-addressed chunks (the Go MTA
    /// uploads each, then sends the hashes as the reference).
    #[uniffi::export]
    pub fn split_sealed_mail_body(body: Vec<u8>) -> Vec<MailBodyChunk> {
        super::split_sealed_mail_body(&body)
    }

    /// UniFFI: rejoin chunks fetched back off the byte plane (the Go MDA, on a
    /// `FETCH` whose reply carried a reference).
    #[uniffi::export]
    pub fn join_sealed_mail_body(chunks: Vec<Vec<u8>>) -> Vec<u8> {
        super::join_sealed_mail_body(&chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn an_empty_body_has_no_chunks() {
        assert!(split_sealed_mail_body(&[]).is_empty());
        assert!(join_sealed_mail_body(&[]).is_empty());
    }

    #[test]
    fn a_chunk_is_keyed_by_the_blake3_of_its_own_bytes() {
        // The store key contract: `POST /api/v1/chunks` recomputes blake3 over the
        // body it receives and rejects a mismatched `X-Content-Hash`. If this ever
        // drifts, every staged upload 400s.
        let b = body(10_000);
        let chunks = split_sealed_mail_body(&b);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].hash, blake3::hash(&b).as_bytes().to_vec());
        assert_eq!(chunks[0].bytes, b);
    }

    #[test]
    fn split_then_join_round_trips_across_the_chunk_boundary() {
        for n in [
            1,
            MAIL_BODY_CHUNK_BYTES - 1,
            MAIL_BODY_CHUNK_BYTES,
            MAIL_BODY_CHUNK_BYTES + 1,
            MAIL_BODY_CHUNK_BYTES * 2,
            MAIL_BODY_CHUNK_BYTES * 2 + 7,
        ] {
            let b = body(n);
            let chunks = split_sealed_mail_body(&b);
            let expected_chunks = n.div_ceil(MAIL_BODY_CHUNK_BYTES);
            assert_eq!(chunks.len(), expected_chunks, "chunk count for {n} bytes");
            let bytes: Vec<Vec<u8>> = chunks.into_iter().map(|c| c.bytes).collect();
            assert_eq!(join_sealed_mail_body(&bytes), b, "round trip for {n} bytes");
        }
    }

    #[test]
    fn a_fifty_megabyte_body_fits_a_reference_that_is_tiny_on_the_wire() {
        // The whole point: `max_message_bytes` = 50 MB must cross a 2 MiB frame.
        let chunks = split_sealed_mail_body(&body(50_000_000));
        assert_eq!(
            chunks.len(),
            50_000_000usize.div_ceil(MAIL_BODY_CHUNK_BYTES)
        );
        let wire_bytes = chunks.len() * 32;
        assert!(
            wire_bytes < 1024,
            "a 50 MB body's reference is {wire_bytes} bytes of hashes"
        );
    }

    #[test]
    fn joining_fails_closed_when_the_reference_lied_about_the_total() {
        // A wrong/reordered/short chunk list must not seal and store as if it were
        // the message. The declared total is what catches it.
        let b = body(MAIL_BODY_CHUNK_BYTES + 100);
        let mut bytes: Vec<Vec<u8>> = split_sealed_mail_body(&b)
            .into_iter()
            .map(|c| c.bytes)
            .collect();
        assert_eq!(
            join_sealed_mail_body_checked(&bytes, b.len() as u64).unwrap(),
            b
        );

        bytes.pop(); // a dropped chunk
        assert!(matches!(
            join_sealed_mail_body_checked(&bytes, b.len() as u64),
            Err(MailBodyRefError::TotalBytesMismatch { .. })
        ));
    }

    #[test]
    fn a_reference_repeating_one_hash_past_its_total_is_refused_by_shape() {
        // The repeat walk: 60,000 repeats of one staged hash under a small total. The
        // shape check refuses it before any chunk is read.
        let err = check_mail_body_ref_shape(60_000, 1000, u64::MAX).unwrap_err();
        assert!(matches!(
            err,
            MailBodyRefError::TooManyChunks {
                count: 60_000,
                max: 1,
                ..
            }
        ));
        // An honest split is exactly at the bound, at every size.
        for n in [
            0usize,
            1,
            MAIL_BODY_CHUNK_BYTES,
            MAIL_BODY_CHUNK_BYTES * 3 + 5,
        ] {
            let chunks = split_sealed_mail_body(&body(n));
            check_mail_body_ref_shape(chunks.len(), n as u64, u64::MAX)
                .unwrap_or_else(|e| panic!("honest {n}-byte split refused: {e}"));
        }
    }

    #[test]
    fn a_total_over_the_legs_ceiling_is_refused_by_shape() {
        assert!(matches!(
            check_mail_body_ref_shape(1, 1001, 1000),
            Err(MailBodyRefError::OverCeiling {
                declared: 1001,
                ceiling: 1000
            })
        ));
        check_mail_body_ref_shape(1, 1000, 1000).unwrap();
    }

    #[test]
    fn the_running_join_refuses_the_chunk_that_passes_the_total() {
        let mut join = MailBodyJoin::new(10);
        join.push(&[0u8; 6]).unwrap();
        assert!(matches!(
            join.push(&[0u8; 6]),
            Err(MailBodyRefError::Overrun {
                declared: 10,
                read: 12
            })
        ));
        // Short of the total → the exact-length pin still fires at finish.
        let mut join = MailBodyJoin::new(10);
        join.push(&[0u8; 6]).unwrap();
        assert!(matches!(
            join.finish(),
            Err(MailBodyRefError::TotalBytesMismatch {
                declared: 10,
                actual: 6
            })
        ));
    }

    // ── The reader half: resolving a served reference ───────────────────────
    //
    // `fauna_core::file_download` has a `FakeFetcher`, but it is a private item
    // inside that crate's own `#[cfg(test)] mod tests` — invisible here. Ours is
    // the same shape, and the precedent for standing one up per consumer is
    // `fauna-media-machine/tests/media_lifecycle.rs::FakeDownloadFetcher`.
    #[cfg(feature = "body-ref-resolve")]
    mod resolve {
        use super::*;
        use fauna_core::data::ContentHash;
        use fauna_core::file_download::BlobFetcher;
        use std::collections::HashMap;

        /// Serves chunks by content address, exactly like the open download
        /// route. `fetch_manifest` is unreachable by construction: a mail
        /// reference is an ordered hash list with no manifest object
        /// (`body_ref.rs` module docs, property 2).
        #[derive(Default)]
        struct FakeChunkFetcher {
            blobs: HashMap<Vec<u8>, Vec<u8>>,
            /// Every key `fetch_chunks` was asked for, in order — lets a test
            /// assert the *order* the reference declared is the order fetched.
            asked: std::sync::Mutex<Vec<Vec<u8>>>,
        }

        impl FakeChunkFetcher {
            fn with(chunks: &[MailBodyChunk]) -> Self {
                Self {
                    blobs: chunks
                        .iter()
                        .map(|c| (c.hash.clone(), c.bytes.clone()))
                        .collect(),
                    asked: Default::default(),
                }
            }
        }

        #[async_trait::async_trait]
        impl BlobFetcher for FakeChunkFetcher {
            async fn fetch_manifest(&self, _hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
                anyhow::bail!("a mail body reference never names a manifest")
            }

            async fn fetch_chunks(
                &self,
                store_keys: &[ContentHash],
                _relative_path: &str,
            ) -> anyhow::Result<Vec<Vec<u8>>> {
                let mut out = Vec::with_capacity(store_keys.len());
                for key in store_keys {
                    let k = key.digest().to_vec();
                    self.asked.lock().unwrap().push(k.clone());
                    match self.blobs.get(&k) {
                        Some(b) => out.push(b.clone()),
                        None => anyhow::bail!("no chunk stored under {}", hex::encode(&k)),
                    }
                }
                Ok(out)
            }
        }

        fn hashes(chunks: &[MailBodyChunk]) -> Vec<Vec<u8>> {
            chunks.iter().map(|c| c.hash.clone()).collect()
        }

        #[tokio::test]
        async fn a_reference_resolves_to_the_exact_bytes_it_stands_for() {
            // The whole contract: what the producer split is what the reader
            // rejoins, byte-for-byte, across a real chunk boundary.
            let body = body(MAIL_BODY_CHUNK_BYTES * 2 + 4242);
            let chunks = split_sealed_mail_body(&body);
            assert_eq!(chunks.len(), 3, "fixture must cross the chunk boundary");
            let fetcher = FakeChunkFetcher::with(&chunks);

            let got = resolve_referenced_mail_body(&fetcher, &hashes(&chunks), body.len() as u64)
                .await
                .unwrap();

            assert_eq!(got, body);
            // In the order the reference declared, not the store's order.
            assert_eq!(*fetcher.asked.lock().unwrap(), hashes(&chunks));
        }

        #[tokio::test]
        async fn a_chunk_hash_that_is_not_thirty_two_bytes_is_refused_before_any_fetch() {
            // The wire carries `Vec<ByteBuf>` — nothing in the type system pins
            // the width, so a peer/corrupt reference can name a short hash. Nest
            // fails closed here (`mail_body_plane::resolve_body_ref`); so must a
            // client, and *before* spending a request on it.
            let body = body(1000);
            let chunks = split_sealed_mail_body(&body);
            let fetcher = FakeChunkFetcher::with(&chunks);

            let err = resolve_referenced_mail_body(&fetcher, &[vec![0u8; 31]], body.len() as u64)
                .await
                .unwrap_err();

            assert!(
                err.to_string().contains("32 bytes"),
                "expected a width complaint, got: {err}"
            );
            assert!(
                fetcher.asked.lock().unwrap().is_empty(),
                "must not fetch on a malformed reference"
            );
        }

        #[tokio::test]
        async fn a_reference_that_lies_about_its_total_fails_closed() {
            // A dropped/reordered/wrong chunk list would otherwise open as a
            // different message. The declared total is what catches it — the
            // same end-to-end check nest applies on the way in.
            let body = body(MAIL_BODY_CHUNK_BYTES + 100);
            let chunks = split_sealed_mail_body(&body);
            let fetcher = FakeChunkFetcher::with(&chunks);

            let mut short = hashes(&chunks);
            short.pop();

            let err = resolve_referenced_mail_body(&fetcher, &short, body.len() as u64)
                .await
                .unwrap_err();

            assert!(
                err.to_string().contains("declared"),
                "expected a total-mismatch, got: {err}"
            );
        }

        #[tokio::test]
        async fn a_missing_chunk_surfaces_rather_than_joining_short() {
            // The nest re-stages chunks on every serve, so a miss is transient —
            // it must surface as an error the caller retries, never as a body.
            let body = body(1000);
            let chunks = split_sealed_mail_body(&body);
            let fetcher = FakeChunkFetcher::default(); // stores nothing

            assert!(
                resolve_referenced_mail_body(&fetcher, &hashes(&chunks), body.len() as u64)
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn the_switchover_counts_the_hint_too() {
        let budget = u64::from(INLINE_MAIL_REQUEST_BUDGET_BYTES);
        assert!(!mail_body_needs_reference(budget, 0));
        assert!(mail_body_needs_reference(budget, 1));
        // The input-dependent hint is why a body alone under budget is not enough.
        assert!(mail_body_needs_reference(budget - 10, 11));
        assert!(!mail_body_needs_reference(0, 0));
    }
}

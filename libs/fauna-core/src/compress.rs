//! Chunk compression module.
//!
//! # Pipeline order
//!
//! Encode: `chunk -> hash (on raw data) -> compress -> encrypt -> store`
//! Decode: `load -> decrypt -> decompress -> verify hash -> use`
//!
//! The content hash is ALWAYS computed on the original uncompressed, unencrypted data.
//!
//! # Stored format
//!
//! Each compressed blob starts with a single prefix byte:
//! - `0x00` = uncompressed; payload is the raw chunk data
//! - `0x01` = zstd compressed; payload is the zstd-compressed chunk data
//!
//! If compressing a chunk does not reduce its size, the uncompressed form is stored
//! (common for already-compressed media such as JPEG or MP4).

use anyhow::{Result, anyhow};

/// Prefix byte indicating the payload is stored uncompressed.
const PREFIX_UNCOMPRESSED: u8 = 0x00;

/// Prefix byte indicating the payload is zstd-compressed.
const PREFIX_ZSTD: u8 = 0x01;

/// zstd compression level – level 3 is a good speed/ratio tradeoff.
const ZSTD_LEVEL: i32 = 3;

/// Frame `data` as an uncompressed blob — `[0x00][data]` — *without* attempting
/// compression.
///
/// Use this when a config disables compression but the blob must still be
/// self-describing so [`decompress_chunk`] is its exact inverse. Contrast
/// [`compress_chunk`], which *attempts* zstd and falls back to this same framing
/// when compression does not help. Returning the data un-prefixed instead would
/// make [`decompress_chunk`] mis-strip any payload whose first byte is
/// `0x00`/`0x01` — the latent at-rest corruption the self-describing prefix
/// exists to prevent.
pub fn frame_uncompressed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + data.len());
    out.push(PREFIX_UNCOMPRESSED);
    out.extend_from_slice(data);
    out
}

/// Which of the two **shipped** write-side framings a caller seals with.
///
/// # Why this is a parameter and not a cleanup
///
/// Two framings of this same prefix scheme shipped independently, and each has a
/// live at-rest corpus written with it. Both decode identically — the prefix is
/// self-describing, and [`decompress_chunk`] is the single inverse of both — so
/// this is a *write-policy* difference only, never a compatibility one.
///
/// The reason it cannot simply be collapsed to one policy: whichever framing
/// lost would silently change the bytes of every **newly** sealed chunk in its
/// corpus. Old artifacts still open, so nothing breaks loudly; what changes is
/// that re-sealing identical plaintext yields different ciphertext, hence a
/// different store key, hence a re-upload and a dedup miss against everything
/// already stored (`message-segment-store.md` § Client-device custodian →
/// *Seal reproduction* also wants the custodian's bytes to reproduce a nest
/// destination's exactly). Making the policy an explicit named constant is what
/// turns "two forked implementations that drift" into one implementation whose
/// per-corpus choice is deliberate and greppable — and what lets a future
/// session retire one corpus's framing as a *chosen* migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkFraming {
    /// Chunks smaller than this are stored raw without attempting zstd.
    min_compress_size: usize,
    /// Whether the "did it help" test charges the compressed side for the
    /// one-byte prefix.
    count_prefix_in_gain: bool,
}

impl ChunkFraming {
    /// The framing of the reserved rails (`__drafts`, `__mls`),
    /// the personalization model and the nest backup store: no small-chunk
    /// floor, and gain measured framed-against-framed.
    ///
    /// This is the **correct** "did it help" test of the two: both candidate
    /// outputs carry the prefix, so compressed wins iff `compressed < raw`.
    pub const RESERVED_RAIL: Self = Self {
        min_compress_size: 0,
        count_prefix_in_gain: false,
    };

    /// The framing of the file-sync chunk corpus (`fauna-sync-engine`'s upload
    /// and seal paths).
    ///
    /// ⚠ Two deliberate differences from [`Self::RESERVED_RAIL`], both kept
    /// **only** because this corpus is already sealed with them — neither is
    /// worth reproducing in new code:
    ///
    /// * the 4096-byte floor, which skips zstd on small chunks;
    /// * `count_prefix_in_gain`, which compares the *framed* compressed length
    ///   against the *unframed* raw length. That is an
    ///   apples-to-oranges test — it stores raw in the razor-thin case where
    ///   compression saved exactly one byte and the framed compressed blob
    ///   would still have been strictly smaller. Harmless (a one-byte size
    ///   pessimisation, never a decode difference), and load-bearing because it
    ///   is what the corpus was written with.
    pub const FILE_SYNC: Self = Self {
        min_compress_size: 4096,
        count_prefix_in_gain: true,
    };
}

/// Compress `data` under the reserved-rail framing
/// ([`ChunkFraming::RESERVED_RAIL`]).
///
/// Returns `[0x01][zstd(data)]` when compression reduces the size, or
/// `[0x00][data]` when the compressed form is not smaller than the original.
pub fn compress_chunk(data: &[u8]) -> Vec<u8> {
    compress_chunk_framed(data, ChunkFraming::RESERVED_RAIL)
}

/// Compress `data` under an explicit [`ChunkFraming`].
///
/// Infallible by design: a zstd failure is not an error condition for a
/// self-describing format, it just means this chunk rests raw.
pub fn compress_chunk_framed(data: &[u8], framing: ChunkFraming) -> Vec<u8> {
    if data.len() < framing.min_compress_size {
        return frame_uncompressed(data);
    }
    // Attempt compression.
    match zstd::encode_all(data, ZSTD_LEVEL) {
        Ok(compressed)
            if compressed.len() + usize::from(framing.count_prefix_in_gain) < data.len() =>
        {
            let mut out = Vec::with_capacity(1 + compressed.len());
            out.push(PREFIX_ZSTD);
            out.extend_from_slice(&compressed);
            out
        }
        // Either compression failed or did not reduce size – store as-is.
        _ => frame_uncompressed(data),
    }
}

/// [`compress_chunk_framed`] over a slice of chunks.
pub fn compress_chunks_framed(chunks: &[Vec<u8>], framing: ChunkFraming) -> Vec<Vec<u8>> {
    chunks
        .iter()
        .map(|c| compress_chunk_framed(c, framing))
        .collect()
}

/// Decompress a prefixed blob produced by [`compress_chunk`].
///
/// Returns the original uncompressed data. Unknown prefix bytes are passed
/// through — the entire blob is returned as-is. This arm is LIVE, not a
/// compatibility remnant: the nest's shared blob store also holds raw blobs
/// (link-preview images, video segments, dag-cbor rail bodies, spilled
/// payloads) that its `decode_blob` readers serve by hash
/// (`version-compatibility.md` § Dimension 2, program 4). The file-sync walk
/// reads through [`unframe_verified_chunk`] instead, where the manifest's hash
/// picks the framed or the raw reading.
///
/// **Unbounded** — for a caller decoding bytes it sealed itself. Those raw
/// blobs let a client choose the prefix byte, so the nest's blob-store reads go
/// through [`decompress_chunk_bounded`] (`backup-restore.md` § 10).
pub fn decompress_chunk(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    match data[0] {
        PREFIX_UNCOMPRESSED => Ok(data[1..].to_vec()),
        PREFIX_ZSTD => {
            let decompressed = zstd::decode_all(&data[1..])
                .map_err(|e| anyhow!("zstd decompression failed: {e}"))?;
            Ok(decompressed)
        }
        _ => Ok(data.to_vec()), // Raw blob without a compression prefix (live raw writes) — return as-is.
    }
}

/// The bomb-guard bound every CAS chunk read passes to
/// [`decompress_chunk_bounded`].
///
/// One owner for a bound that six call sites each used to re-declare: three
/// private `MAX_DECOMPRESSED_CHUNK` copies (the shared reader, the
/// since-removed sync daemon, the nest web-content reader), a differently-named
/// `MAX_PLAINTEXT_CHUNK` in the nest chunk-upload route, and two bare literals
/// in tests that describe themselves as replicas of those paths. It is the
/// same bound in every one of them because they all read the same population:
/// chunks the shared chunker produced.
///
/// **Why the invariant below is a compile-time pin and not a comment.** The
/// guard must stay at or above `chunker::MAX_CHUNK`. Below it, the
/// failure is not a rejected attack but a *legitimate maximum-size chunk that
/// silently stops opening* — a read failure on the user's own stored bytes,
/// reached only by whoever happens to store an 8 MB chunk. The headroom above
/// `MAX_CHUNK` is deliberate, so the chunk bound can be raised without this
/// becoming the thing that breaks.
pub const MAX_DECOMPRESSED_CHUNK: usize = 16 * 1024 * 1024;

const _: () = assert!(
    MAX_DECOMPRESSED_CHUNK >= crate::chunker::MAX_CHUNK as usize,
    "the bomb guard must admit a maximum-size chunk, or legitimate reads fail"
);

/// Like [`decompress_chunk`] but caps the decompressed output at `max_len`
/// bytes, returning an error if the payload would exceed it.
///
/// **Bomb-safe** — unlike [`decompress_chunk`] (which uses `zstd::decode_all`
/// and allocates whatever the stream expands to), this decodes into a buffer of
/// at most `max_len` and fails rather than expanding further. Use it on any
/// **untrusted** input — e.g. nest-side verification of a client-uploaded chunk
/// against its claimed plaintext hash — where a small malicious blob must not
/// be able to expand to exhaust memory.
pub fn decompress_chunk_bounded(data: &[u8], max_len: usize) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let too_big = || anyhow!("decompressed chunk exceeds maximum size ({max_len})");
    match data[0] {
        PREFIX_UNCOMPRESSED => {
            let payload = &data[1..];
            if payload.len() > max_len {
                return Err(too_big());
            }
            Ok(payload.to_vec())
        }
        PREFIX_ZSTD => {
            // `zstd::bulk::decompress` allocates exactly `max_len` and errors if
            // the content would overflow it — never materialising the full bomb.
            zstd::bulk::decompress(&data[1..], max_len)
                .map_err(|e| anyhow!("zstd decompression failed (or exceeds {max_len}): {e}"))
        }
        _ => {
            // Raw blob without a compression prefix (live raw writes) — return as-is, bounded.
            if data.len() > max_len {
                return Err(too_big());
            }
            Ok(data.to_vec())
        }
    }
}

/// Read a stored chunk's bytes against `want`, the manifest's recorded
/// plaintext hash: the framed reading first ([`unframe_strict_bounded`]), then
/// the raw body itself, returning whichever addresses `want` and `None` when
/// neither does (a substituted or corrupted chunk). The frame is not decidable
/// by inspection (an unframed body can begin with a frame byte), so the hash
/// decides — and because a raw body is returned only when its own `blake3` IS
/// `want`, the fallback costs no integrity.
///
/// The raw arm is a LIVE read, not a compatibility remnant:
/// every client chunk writer frames through the one seal door
/// (`crate::chunk_seal`), but the nest's blob store is first-writer-wins and
/// also takes raw bodies under `blake3(body)` (a headerless chunk upload, and
/// the store's other raw writers) — exactly the key a public (unsealed) chunk
/// rests under. Without this arm a registered user who pre-seeds a public
/// chunk's plaintext raw makes that file unreadable on every reader of the
/// walk (`version-compatibility.md` § Dimension 2, program 4, tranche B3).
/// Lifted out of hand-copies (`bins/fauna-nest/src/web_content/file_bytes.rs`
/// among them) that each wrap this in their own error type — this returns
/// `Option` so each caller keeps its own error, and none carries another's
/// message shape.
pub fn unframe_verified_chunk(body: Vec<u8>, want: &crate::data::ContentHash) -> Option<Vec<u8>> {
    if let Ok(unframed) = unframe_strict_bounded(&body, MAX_DECOMPRESSED_CHUNK)
        && crate::data::ContentHash::of_raw(&unframed) == *want
    {
        return Some(unframed);
    }
    (crate::data::ContentHash::of_raw(&body) == *want).then_some(body)
}

/// [`decompress_chunk_bounded`] without the unknown-prefix passthrough: the
/// body MUST carry a frame prefix. The framed half of
/// [`unframe_verified_chunk`], and the chunk upload's framed-plaintext proof,
/// where an unframed body cannot be the framed reading.
pub fn unframe_strict_bounded(data: &[u8], max_len: usize) -> Result<Vec<u8>> {
    match data.first() {
        Some(&PREFIX_UNCOMPRESSED) | Some(&PREFIX_ZSTD) => decompress_chunk_bounded(data, max_len),
        Some(other) => Err(anyhow!(
            "unframed body: no frame prefix (first byte {other:#04x})"
        )),
        None => Err(anyhow!("unframed body: empty (no frame prefix)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip for typical text data (should compress well).
    #[test]
    fn roundtrip_text() {
        let input = b"Hello, world! ".repeat(200);
        let compressed = compress_chunk(&input);
        // Verify it was actually compressed (prefix 0x01).
        assert_eq!(
            compressed[0], PREFIX_ZSTD,
            "expected zstd prefix for compressible text"
        );
        assert!(
            compressed.len() < input.len(),
            "compressed blob should be smaller than original"
        );
        let recovered = decompress_chunk(&compressed).expect("decompression failed");
        assert_eq!(recovered, input.to_vec());
    }

    /// Round-trip for typical binary data (repeating pattern compresses well).
    #[test]
    fn roundtrip_binary() {
        let input: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let compressed = compress_chunk(&input);
        let recovered = decompress_chunk(&compressed).expect("decompression failed");
        assert_eq!(recovered, input);
    }

    /// Incompressible data (random-looking) must be stored uncompressed.
    #[test]
    fn incompressible_stored_uncompressed() {
        // A pre-built "random-looking" byte sequence that zstd cannot shrink.
        // We use a pseudo-random pattern with no structure.
        let input: Vec<u8> = (0u8..=255)
            .flat_map(|b| [b, b.wrapping_mul(17).wrapping_add(91)])
            .cycle()
            .take(128)
            .collect();

        // Manually check: if zstd cannot compress it, prefix should be 0x00.
        let blob = compress_chunk(&input);
        // Whether prefix is 0x00 or 0x01, round-trip must be lossless.
        let recovered = decompress_chunk(&blob).expect("decompression failed");
        assert_eq!(
            recovered, input,
            "round-trip must be lossless for incompressible data"
        );

        // For a clearly incompressible input (e.g. 1 byte), the blob is uncompressed.
        let single = b"X";
        let blob_single = compress_chunk(single);
        assert_eq!(
            blob_single[0], PREFIX_UNCOMPRESSED,
            "single byte cannot be compressed"
        );
        let recovered_single = decompress_chunk(&blob_single).expect("decompression failed");
        assert_eq!(recovered_single, single);
    }

    /// The hash decides the reading, whatever the first byte: an unframed body
    /// whose own bytes address `want` is served as-is (a frame-byte-led one
    /// only after its framed reading misses), a framed one is unframed, and a
    /// body addressing `want` in neither reading is refused.
    #[test]
    fn unframe_verified_chunk_lets_the_hash_decide_whatever_the_first_byte() {
        for first in [0x00u8, 0x01, 0x42, 0xff] {
            let raw = vec![first, 9, 8, 7, 6];
            let want = crate::data::ContentHash::of_raw(&raw);
            assert_eq!(
                unframe_verified_chunk(raw.clone(), &want),
                Some(raw.clone()),
                "an unframed body starting {first:#04x} that addresses want is served"
            );
            let other = crate::data::ContentHash::of_raw(b"some other chunk");
            assert_eq!(
                unframe_verified_chunk(raw, &other),
                None,
                "a body starting {first:#04x} addressing neither reading is refused"
            );
        }
        let plain = b"framed plaintext".to_vec();
        let want = crate::data::ContentHash::of_raw(&plain);
        assert_eq!(
            unframe_verified_chunk(frame_uncompressed(&plain), &want),
            Some(plain.clone())
        );
        assert_eq!(
            unframe_verified_chunk(compress_chunk(&plain), &want),
            Some(plain)
        );
        assert!(unframe_strict_bounded(&[], 16).is_err());
    }

    /// Unknown prefix byte is passed through by the BLOB-STORE decoder (returned
    /// as-is, no error): the nest's store also holds raw blobs — link previews,
    /// video segments, dag-cbor rail bodies — that its readers serve, so this
    /// arm is live (`version-compatibility.md` § Dimension 2, program 4).
    #[test]
    fn unknown_prefix_is_raw_passthrough() {
        let raw = vec![0x42u8, 1, 2, 3];
        let result = decompress_chunk(&raw);
        assert!(
            result.is_ok(),
            "unknown prefix should be treated as a raw blob"
        );
        assert_eq!(
            result.unwrap(),
            raw,
            "a raw blob must be returned unchanged"
        );
    }

    /// Empty input returns empty output (no error).
    #[test]
    fn empty_input_returns_empty() {
        let result = decompress_chunk(&[]);
        assert!(result.is_ok(), "empty input should return Ok");
        assert!(
            result.unwrap().is_empty(),
            "empty input should return empty output"
        );
    }

    /// A compressible chunk **below** the file-sync floor is the one input that
    /// distinguishes the two shipped framings — and it is exactly the input a
    /// careless unification would silently re-frame, changing the store key of
    /// every such chunk in a live corpus.
    #[test]
    fn the_two_shipped_framings_differ_only_below_the_floor() {
        // Highly compressible, and deliberately under FILE_SYNC's 4096 floor.
        let small = b"aaaaaaaaaaaaaaaa".repeat(64); // 1024 bytes
        assert!(small.len() < 4096);

        assert_eq!(
            compress_chunk_framed(&small, ChunkFraming::RESERVED_RAIL)[0],
            PREFIX_ZSTD,
            "the reserved-rail framing has no floor — it compresses this",
        );
        assert_eq!(
            compress_chunk_framed(&small, ChunkFraming::FILE_SYNC)[0],
            PREFIX_UNCOMPRESSED,
            "the file-sync framing stores it raw: BELOW the 4096 floor. \
             Changing this re-keys every small chunk in the file-sync corpus.",
        );

        // Above the floor the two agree, so most of the corpus is unaffected.
        let large = b"aaaaaaaaaaaaaaaa".repeat(1024); // 16 KiB
        assert!(large.len() > 4096);
        assert_eq!(
            compress_chunk_framed(&large, ChunkFraming::RESERVED_RAIL),
            compress_chunk_framed(&large, ChunkFraming::FILE_SYNC),
            "above the floor the framings must agree byte for byte",
        );
    }

    /// `decompress_chunk` is the single inverse of **both** framings — which is
    /// what makes the policy a write-side choice and never a compat break.
    #[test]
    fn one_decompressor_inverts_both_framings_on_both_sides_of_the_floor() {
        let cases: Vec<Vec<u8>> = vec![
            b"x".to_vec(),                                      // tiny
            b"abcabcabc".repeat(64),                            // compressible, under the floor
            (0..3000u32).map(|i| (i * 7 + 13) as u8).collect(), // noisy, under the floor
            b"abcabcabc".repeat(2048),                          // compressible, over the floor
            (0..9000u32).map(|i| (i * 7 + 13) as u8).collect(), // noisy, over the floor
            Vec::new(),                                         // empty
        ];
        for framing in [ChunkFraming::RESERVED_RAIL, ChunkFraming::FILE_SYNC] {
            for plain in &cases {
                let blob = compress_chunk_framed(plain, framing);
                assert_eq!(
                    &decompress_chunk(&blob).expect("decompress"),
                    plain,
                    "round-trip failed for {framing:?} on a {}-byte chunk",
                    plain.len(),
                );
                assert!(
                    blob[0] == PREFIX_UNCOMPRESSED || blob[0] == PREFIX_ZSTD,
                    "every framed blob is self-describing",
                );
            }
        }
    }

    /// The default entry point is the reserved-rail framing — the rails
    /// (`__drafts`, `__mls`), personalization and nest backup all
    /// call it, so a change here re-seals those blobs.
    #[test]
    fn compress_chunk_is_the_reserved_rail_framing() {
        for plain in [b"tiny".to_vec(), b"abcabc".repeat(1000)] {
            assert_eq!(
                compress_chunk(&plain),
                compress_chunk_framed(&plain, ChunkFraming::RESERVED_RAIL),
            );
        }
    }

    /// Bounded decompress round-trips within the cap and rejects a zstd bomb
    /// *without* materialising it (the cap is the allocation ceiling).
    #[test]
    fn bounded_decompress_roundtrips_and_caps() {
        // Round-trip: well within the cap.
        let input = b"Hello, world! ".repeat(200);
        let compressed = compress_chunk(&input);
        let recovered =
            decompress_chunk_bounded(&compressed, 16 * 1024 * 1024).expect("within-cap decompress");
        assert_eq!(recovered, input.to_vec());

        // Bomb: ~1 MB of zeros compresses tiny but expands past a small cap.
        let bomb_plain = vec![0u8; 1024 * 1024];
        let bomb = compress_chunk(&bomb_plain);
        assert_eq!(bomb[0], PREFIX_ZSTD, "zeros must zstd-compress");
        assert!(
            bomb.len() < 4096,
            "bomb compresses tiny (got {} bytes)",
            bomb.len()
        );
        assert!(
            decompress_chunk_bounded(&bomb, 64 * 1024).is_err(),
            "decompressing past the cap must error, not expand"
        );
        // The same blob is fine under a generous cap.
        assert_eq!(
            decompress_chunk_bounded(&bomb, 2 * 1024 * 1024).unwrap(),
            bomb_plain
        );

        // Uncompressed + raw payloads are also capped.
        assert!(decompress_chunk_bounded(&frame_uncompressed(&[0u8; 100]), 10).is_err());
        assert!(decompress_chunk_bounded(&[0x42, 1, 2, 3, 4, 5], 3).is_err());
    }
}

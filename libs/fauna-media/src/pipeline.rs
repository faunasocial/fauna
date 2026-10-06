//! UploadPayload + process_and_seal — convenience composer for per-app wire-up.

use crate::audience::Audience;
use crate::process::{ProcessedMedia, process_media};
use crate::seal::{SealedBlob, seal_for_audience};
use crate::sidecar::UploadSidecar;

/// What the client uploads: primary blob + sidecar, optionally a thumbnail
/// blob + sidecar (sealed under the same audience).
#[derive(Debug, Clone)]
pub struct UploadPayload {
    pub primary: SealedBlob,
    pub primary_sidecar: UploadSidecar,
    /// Present when `process_media` produced a thumbnail. The thumbnail is
    /// sealed under the same audience as the parent and uploaded as a
    /// separate blob; the parent's `UploadSidecar.thumbnail_hash` carries
    /// the BLAKE3 hash of the sealed thumbnail bytes.
    pub thumbnail: Option<(SealedBlob, UploadSidecar)>,
}

/// One blob's two `multipart/form-data` parts, exactly as the nest's
/// `POST /api/v1/blob` expects them: the DAG-CBOR `UploadSidecar` (the
/// `sidecar` part) and the sealed-or-plaintext blob bytes (the `bytes` part).
#[derive(Debug, Clone)]
pub struct MultipartBlob {
    /// DAG-CBOR-encoded [`UploadSidecar`] — the `sidecar` multipart part.
    pub sidecar_cbor: Vec<u8>,
    /// Sealed (AEAD classes) or plaintext (`PublicPost`) bytes — the `bytes` part.
    pub bytes: Vec<u8>,
}

impl UploadPayload {
    /// Flatten into the parts a client POSTs: the primary blob, plus the
    /// thumbnail blob when `process_media` derived one.
    ///
    /// Every app shares this mapping so all 7 ship byte-identical multipart
    /// parts for the same input (priority #1/#2) — it is the one place the
    /// `(SealedBlob, UploadSidecar)` pair is flattened to wire bytes. Callers:
    /// native `fauna_client::upload_public_post_blob`, the UniFFI
    /// `fauna_ffi::process_and_seal_upload` binding, and the web
    /// `fauna_wasm::WasmUploadPayload`.
    ///
    /// **Ordering — thumbnail first on the `?thumb=1` routing model.** The
    /// primary's sidecar carries the thumbnail's `blake3` as its
    /// `thumbnail_hash`, and the nest bakes that pointer into `blob_metadata`
    /// when it ingests the primary. Any producer whose thumbnail is reached
    /// *through* the primary (`?thumb=1` — the feed-attachment path) must
    /// therefore POST the thumbnail first, so the recorded pointer never names
    /// a blob that is not yet in the store; the nest degrades that case by
    /// silently serving the full-size original, which is invisible to a status
    /// check. Either way the thumbnail POST is best-effort — it must never fail
    /// the upload.
    ///
    /// A producer that instead records the thumbnail hash on its *own* row and
    /// fetches direct-by-hash (the Media-library path,
    /// `MediaMachine::do_upload`) has no such window and deliberately posts the
    /// primary first, so a thumbnail failure cannot orphan a stored primary.
    /// Both orders are correct for their routing model — don't "unify" them.
    pub fn into_multipart_parts(self) -> (MultipartBlob, Option<MultipartBlob>) {
        let primary = MultipartBlob {
            sidecar_cbor: self.primary_sidecar.to_dag_cbor(),
            bytes: self.primary.bytes,
        };
        let thumbnail = self.thumbnail.map(|(sealed, sidecar)| MultipartBlob {
            sidecar_cbor: sidecar.to_dag_cbor(),
            bytes: sealed.bytes,
        });
        (primary, thumbnail)
    }
}

/// A sealed thumbnail blob ready to upload as a standalone blob, for a producer
/// that stores the primary by another path and needs only the thumbnail.
///
/// The device-sync engine is the canonical caller: it stores the file itself as
/// encrypted chunks (no `/api/v1/blob` primary, so `?thumb=1` can't resolve a
/// thumbnail), then uploads this sealed thumbnail as a standalone blob and
/// records [`hash`](Self::hash) on the folder member. A client later fetches
/// the thumbnail direct-by-hash (`GET /api/v1/blob/<hash>`) and decrypts it.
#[derive(Debug, Clone)]
pub struct SealedThumbnail {
    /// Sealed thumbnail bytes — the blob `bytes` part to POST.
    pub bytes: Vec<u8>,
    /// The thumbnail's own sidecar (`thumbnail_hash: None` — no recursion).
    pub sidecar: UploadSidecar,
    /// `blake3(bytes)` — the content hash to record on the member and the
    /// identifier a client fetches by (`GET /api/v1/blob/<hash>`). Equals the
    /// hash the nest assigns to the stored blob (an AEAD-sealed blob is stored
    /// verbatim, so `blake3(stored) == blake3(sealed)`).
    pub hash: [u8; 32],
}

/// Seal a thumbnail under `audience`, returning the sealed blob + its sidecar.
///
/// Shared by [`process_and_seal`] and [`seal_thumbnail_only`] so the two
/// producers agree on the thumbnail's sidecar shape exactly. For an AEAD-sealed
/// class the on-wire bytes are ciphertext, so the sidecar MUST declare
/// `application/octet-stream` + `has_c2pa = false` (the real MIME / C2PA flag
/// ride *inside* the sealed bytes, and the nest's encrypted-mode verifier
/// rejects any other sidecar MIME for a sealed class —
/// `bins/fauna-nest/src/storage/encrypted.rs::classify_per_class_envelope` →
/// `mime_class_mismatch`). The thumbnailer always emits JPEG, so a *plaintext*
/// thumbnail's MIME is `image/jpeg`.
fn seal_thumbnail(audience: &Audience, thumbnail_bytes: &[u8]) -> (SealedBlob, UploadSidecar) {
    let sealed = seal_for_audience(audience, thumbnail_bytes);
    let class = sealed.class;
    let mime = if class.is_aead_sealed() {
        "application/octet-stream".to_string()
    } else {
        "image/jpeg".to_string()
    };
    let sidecar = UploadSidecar {
        class,
        mime,
        has_c2pa: false,
        thumbnail_hash: None, // thumbnails don't have thumbnails
    };
    (sealed, sidecar)
}

/// Process raw uploader bytes, then seal under the audience.
///
/// **Contract is final.** When the deferred image-processing work lands,
/// `process_media` swaps from a
/// stub to the real EXIF/IPTC/MIME/thumbnail/C2PA pipeline without touching
/// this signature or `UploadPayload`'s shape. Per-app wire-up sessions
/// code against this entrypoint.
pub fn process_and_seal(raw: &[u8], audience: &Audience) -> UploadPayload {
    process_and_seal_with_mime(raw, audience).0
}

/// [`process_and_seal`], plus the plaintext's **real sniffed MIME**.
///
/// An AEAD-sealed class's sidecar cannot carry the real MIME — it is pinned to
/// `application/octet-stream` (the sidecar rule inside) — but a reader that
/// opens the seal still needs to know how to interpret the plaintext once
/// decrypted. The caller shape is a sealed `MediaItem`: its `media_type` must
/// name the real type, and neither the sidecar nor an OS filename guess can
/// supply it. A plaintext (`PublicPost`) caller gets the same string its
/// sidecar already carries.
///
/// This is [`process_and_seal`]'s own body — that function delegates here — so
/// the two producers can never drift. They already had: a byte-for-byte copy of
/// this body lived in `fauna_client::media_upload` for the archive import's
/// sealed media, and the `FeedManager`'s gated-compose seal would have needed a
/// third.
pub fn process_and_seal_with_mime(raw: &[u8], audience: &Audience) -> (UploadPayload, String) {
    let ProcessedMedia {
        stripped_bytes,
        mime,
        thumbnail_bytes,
        has_c2pa,
    } = process_media(raw);

    let primary = seal_for_audience(audience, &stripped_bytes);
    let class = primary.class;

    // Audience-aware sidecar MIME / C2PA flag. For an AEAD-sealed class the
    // on-wire bytes are ciphertext, so the sidecar MUST declare
    // `application/octet-stream` + `has_c2pa = false`. Only a `PublicPost` blob
    // is plaintext, so its sidecar carries the real sniffed MIME + C2PA flag
    // (the Content-Type the nest serves on download). The thumbnail's own
    // sidecar shape follows the same rule, applied in `seal_thumbnail`.
    let (primary_mime, primary_has_c2pa) = if class.is_aead_sealed() {
        ("application/octet-stream".to_string(), false)
    } else {
        (mime.clone(), has_c2pa)
    };

    // Thumbnail seals under the same audience as the parent.
    let thumbnail = thumbnail_bytes.map(|tb| seal_thumbnail(audience, &tb));

    // Primary's sidecar carries the thumbnail's BLAKE3 hash (computed once,
    // over the sealed bytes — that's the routing identifier the nest stores).
    let primary_thumbnail_hash = thumbnail
        .as_ref()
        .map(|(sealed, _)| *blake3::hash(&sealed.bytes).as_bytes());

    let primary_sidecar = UploadSidecar {
        class,
        mime: primary_mime,
        has_c2pa: primary_has_c2pa,
        thumbnail_hash: primary_thumbnail_hash,
    };

    (
        UploadPayload {
            primary,
            primary_sidecar,
            thumbnail,
        },
        mime,
    )
}

/// Generate + seal ONLY the thumbnail for `raw` under `audience`.
///
/// `None` when `process_media` produces no thumbnail (non-image, ≤300px, or the
/// curated `process_media` feature is off → stub). For producers that upload the
/// primary by another path (the device-sync chunk pipeline) and need just the
/// thumbnail sidecar blob plus its content hash to record on the member. The
/// sealed bytes and sidecar are identical to the thumbnail [`process_and_seal`]
/// produces for the same `raw` + `audience`, so a client fetches + decrypts it
/// uniformly.
pub fn seal_thumbnail_only(raw: &[u8], audience: &Audience) -> Option<SealedThumbnail> {
    let thumbnail_bytes = process_media(raw).thumbnail_bytes?;
    Some(seal_rendered_thumbnail(&thumbnail_bytes, audience))
}

/// Seal an **already-rendered** thumbnail under `audience`.
///
/// The re-seal entry point, and the one thumbnail producer that is **infallible
/// and feature-free**: it renders nothing, so it does not go through
/// `process_media` and does not care whether the curated `process_media` feature
/// is compiled in. That is the whole reason it exists. [`seal_thumbnail_only`]
/// and [`process_and_seal`] both return no thumbnail on a build without the
/// thumbnailer, so a re-seal on such a build would record `thumbnail_hash: None`
/// over a file that *has* a thumbnail — dropping the pointer instead of moving
/// the seal. Opening the existing sealed thumbnail and handing its plaintext
/// here moves it under the new key with no thumbnailer in sight
/// (`identity-succession.md` § Implementation status today, the raw-AEAD Library
/// plane bullet; `SyncEngine::move_recorded_thumbnail`).
///
/// Shares [`seal_thumbnail`]'s body, so a moved thumbnail's sidecar is
/// byte-shaped exactly like a freshly-rendered one — a consumer cannot tell
/// which producer made it, which is what keeps the move invisible to every
/// reader.
pub fn seal_rendered_thumbnail(thumbnail_bytes: &[u8], audience: &Audience) -> SealedThumbnail {
    let (sealed, sidecar) = seal_thumbnail(audience, thumbnail_bytes);
    let hash = *blake3::hash(&sealed.bytes).as_bytes();
    SealedThumbnail {
        bytes: sealed.bytes,
        sidecar,
        hash,
    }
}

/// Generate + seal ONLY the thumbnail for the image at `path`, reading the
/// source **incrementally from disk** (`image::ImageReader::open`) instead of
/// loading the whole file into memory.
///
/// The from-path twin of [`seal_thumbnail_only`], for the device-sync
/// **streaming** upload path (files ≥ 64 MiB), which is O(MAX_CHUNK) memory by
/// design and must not re-read the whole file to thumbnail it. The sealed bytes
/// and sidecar are identical in shape to what [`seal_thumbnail_only`] produces
/// for the same source image + audience (the thumbnail pixels are the same; only
/// the per-call AEAD nonce differs), so a client fetches + decrypts a
/// streamed-file thumbnail exactly as it does a small-file one.
///
/// `None` when no thumbnail is warranted: non-image, ≤300px, a missing /
/// unreadable / undecodable file, or the curated `process_media` feature is off
/// (stub). Skips the EXIF/IPTC strip [`seal_thumbnail_only`] runs on its input —
/// the thumbnail is a freshly re-encoded JPEG that carries no source metadata,
/// so the pixels (hence the thumbnail) are identical to stripping first.
pub fn seal_thumbnail_only_from_path(
    path: &std::path::Path,
    audience: &Audience,
) -> Option<SealedThumbnail> {
    let thumbnail_bytes = crate::process::render_thumbnail_from_path(path)?;
    let (sealed, sidecar) = seal_thumbnail(audience, &thumbnail_bytes);
    let hash = *blake3::hash(&sealed.bytes).as_bytes();
    Some(SealedThumbnail {
        bytes: sealed.bytes,
        sidecar,
        hash,
    })
}

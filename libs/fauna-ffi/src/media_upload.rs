//! UniFFI binding for `fauna_media::process_and_seal` — the encrypted-mode
//! upload-sidecar seal + sidecar-CBOR composer the UniFFI apps (Android,
//! iOS, macOS, Windows) call before POSTing `multipart/form-data` to
//! `POST /api/v1/blob`.
//!
//! Design tracked internally.
//! Goal: `docs/goal/architecture/encryption-at-rest.md` § Per-content-kind
//! conformance → Media row.
//!
//! **Audience subset.** Only the two *client-key* audiences are expressible as
//! FFI inputs: `Library` (the owner's `BackupKey`, which the client derives
//! from the identity seed and legitimately holds) and `PublicPost` (no key —
//! the bytes are signed plaintext). The MLS-keyed audiences (`Conversation`,
//! `RestrictedPost`) are deliberately absent: their key material lives in the
//! shared-Rust MLS / subscription state and must NOT be extracted to the
//! client across the FFI boundary. Sealing under those audiences will get its
//! own shared-Rust seal helper that reaches into the engine *by id* and never
//! hands the raw epoch secret out (a tracked follow-on).
//!
//! **Real `process_media` (on-device).** `fauna-media` is pulled here with
//! `features = ["process_media", "c2pa-detect"]` (`Cargo.toml`), so every
//! native UniFFI app runs the real pipeline on-device before sealing — MIME
//! sniff + EXIF/IPTC strip + JPEG thumbnail + C2PA detect. `image` /
//! `img-parts` / `c2pa` all cross-compile to the four Android ABIs + the Apple
//! targets (`c2pa` via `rust_native_crypto`, pure-Rust); the flip landed
//! 2026-06-30. So a real image
//! attached to a `PublicPost` now yields a sniffed `image/*` MIME + a thumbnail
//! blob in [`FfiUploadPayload::thumbnail`] (the client POSTs it as a second
//! blob; `?thumb=1` resolves it off the primary's stored sidecar hash). The
//! web/wasm leaf runs the **curated** pipeline (`process_media` without
//! `c2pa-detect`, so `has_c2pa` stays `false` there); it reached the same
//! producer parity 2026-07-22 via `fauna_wasm::WasmUploadPayload`.
//!
//! **Not only upload.** [`strip_media_metadata`] and [`detect_media_c2pa`] are
//! plain `fauna_media::process` passthroughs with no seal in sight — the photo
//! backup ingress and the viewer's `c2pa-badge` correction respectively. This
//! module is the UniFFI face of that crate's byte-level pipeline, of which the
//! sidecar seal is the largest but not the only user.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_media::audience::Audience;
use fauna_media::pipeline::process_and_seal;
use fauna_media::sidecar::UploadSidecar;

use crate::FfiError;

/// The audience for a single client-side blob upload — the subset of
/// [`fauna_media::audience::Audience`] whose key material the client
/// legitimately holds (see module docs for why the MLS-keyed variants are
/// excluded).
#[derive(uniffi::Enum)]
pub enum FfiUploadAudience {
    /// Owner-only library media, sealed under the owner's `BackupKey` (exactly
    /// 32 bytes; the client derives it from the identity seed via
    /// `BackupKey::derive`).
    Library { backup_key: Vec<u8> },
    /// Public-post-attached media — no seal; the bytes pass through as signed
    /// plaintext (the post's signature attests the blob hash). The post
    /// binding lives on the post's reference list, not in the sidecar, so the
    /// upload carries a zero `post_id` placeholder (the `PublicPost` seal
    /// derives no key from it).
    PublicPost,
}

/// One sealed blob ready for a `multipart/form-data` part pair: `bytes` rides
/// in the `bytes` part (`application/octet-stream`), `sidecar_cbor` (canonical
/// DAG-CBOR `UploadSidecar`) in the `sidecar` part (`application/cbor`).
#[derive(uniffi::Record)]
pub struct FfiSealedUpload {
    pub bytes: Vec<u8>,
    pub sidecar_cbor: Vec<u8>,
}

/// The result of sealing one upload: the primary blob, plus a thumbnail blob
/// when `process_media` derived one (a separate blob sealed under the same
/// audience; the primary sidecar's `thumbnail_hash` already points at the
/// sealed thumbnail bytes). With the real on-device `process_media` enabled
/// here, an image larger than 300×300 yields a `Some(thumbnail)`; a non-image
/// (or already-small) upload yields `None`.
#[derive(uniffi::Record)]
pub struct FfiUploadPayload {
    pub primary: FfiSealedUpload,
    pub thumbnail: Option<FfiSealedUpload>,
}

/// Process + seal raw uploader bytes under `audience`, returning the multipart
/// parts the client POSTs to `/api/v1/blob`. See module docs for the audience
/// subset and the on-device `process_media` note (a real image yields a second
/// `thumbnail` part).
#[uniffi::export]
pub fn process_and_seal_upload(
    raw: Vec<u8>,
    audience: FfiUploadAudience,
) -> Result<FfiUploadPayload, FfiError> {
    let audience = match audience {
        FfiUploadAudience::Library { backup_key } => {
            let key: [u8; 32] =
                backup_key
                    .as_slice()
                    .try_into()
                    .map_err(|_| FfiError::General {
                        msg: format!("backup_key must be 32 bytes, got {}", backup_key.len()),
                    })?;
            Audience::Library {
                backup_key: BackupKey::from_bytes(key),
            }
        }
        // `post_id` is unused by the `PublicPost` seal (it derives no key), and
        // the blob uploads before the post that references it exists, so a zero
        // placeholder is the honest value — mirrors `apps/fauna-linux`'s
        // `public_post_upload_payload`.
        FfiUploadAudience::PublicPost => Audience::PublicPost {
            post_id: ContentHash::from_digest_raw([0u8; 32]),
        },
    };

    // The shared flattening, so every app POSTs byte-identical multipart
    // parts for the same input (priority #1/#2).
    let (primary, thumbnail) = process_and_seal(&raw, &audience).into_multipart_parts();
    Ok(FfiUploadPayload {
        primary: FfiSealedUpload {
            bytes: primary.bytes,
            sidecar_cbor: primary.sidecar_cbor,
        },
        thumbnail: thumbnail.map(|t| FfiSealedUpload {
            bytes: t.bytes,
            sidecar_cbor: t.sidecar_cbor,
        }),
    })
}

/// Lossless privacy-metadata strip (EXIF/IPTC removal, C2PA preserved) with
/// **no** seal, no thumbnail, no C2PA probe — the on-device face for a
/// photo-library backup ingress, where re-encoding (what `process_and_seal_upload`'s
/// `process_media` does internally) would permanently degrade the only copy a
/// restore returns. See `fauna_media::process::strip_metadata` for the
/// lossless-container-segment-removal guarantee this wraps verbatim.
#[uniffi::export]
pub fn strip_media_metadata(raw: Vec<u8>) -> Vec<u8> {
    fauna_media::process::strip_metadata(&raw)
}

/// The **viewer-side** provenance verdict over blob bytes the app already
/// holds — `fauna_media::process::detect_c2pa_in_bytes` verbatim, the one
/// ground truth every app paints `c2pa-badge` from (`docs/goal/ui/media.md`
/// § C2PA provenance).
///
/// The only reason it crosses the FFI: android (Kotlin), windows (C#) and
/// apple (Swift) fetch the bytes in platform code, not in Rust, so unlike tui
/// and linux they cannot call the shared function directly. The Rust side is
/// identical for all five — one detector, one sniff, one verdict (priority
/// #2).
///
/// **It takes bytes, and deliberately no MIME.** The `x-c2pa` header and the
/// sidecar MIME are both the uploader's own assertion, which the nest stores
/// without ever inspecting the bytes; a badge painted from either is a badge
/// a modified client can forge. Callers keep the header as a *pre-filter*
/// only — `false` means skip the parse, `true` means ask this function.
#[uniffi::export]
pub fn detect_media_c2pa(raw: Vec<u8>) -> bool {
    fauna_media::process::detect_c2pa_in_bytes(&raw)
}

/// The DAG-CBOR `UploadSidecar` bytes for a gated post's **already-sealed**
/// full-body blob — the native-client (Apple / Windows / Android) twin of
/// `fauna_wasm::gated_post_sidecar`, both routed through the one shared
/// [`UploadSidecar::gated_post`] so every app ships a byte-identical
/// `PeriodRestrictedPost` sidecar (priority #1/#2). The sealed bytes come from
/// [`crate::feed_manager::FfiFeedManager::prepare_gated_blob`]; this is transport
/// glue only (no `process_and_seal` — the real MIME rides inside the seal, so the
/// sidecar mime is `application/octet-stream` and there is no thumbnail). The
/// client POSTs these bytes as the `multipart` `sidecar` part alongside the
/// sealed blob as `bytes` (`ui/feed.md` § Encryption at rest).
#[uniffi::export]
pub fn gated_post_sidecar() -> Vec<u8> {
    UploadSidecar::gated_post().to_dag_cbor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_media::audience::AudienceClass;

    /// Re-encode an expected sidecar through the same `to_dag_cbor` the binding
    /// uses — asserting on exact wire bytes without needing a CBOR decoder dep.
    fn sidecar_cbor(class: AudienceClass, mime: &str) -> Vec<u8> {
        UploadSidecar {
            class,
            mime: mime.to_string(),
            has_c2pa: false,
            thumbnail_hash: None,
        }
        .to_dag_cbor()
    }

    #[test]
    fn public_post_passes_bytes_through_with_public_post_sidecar() {
        let raw = b"not an image, just attachment bytes".to_vec();
        let payload = process_and_seal_upload(raw.clone(), FfiUploadAudience::PublicPost).unwrap();

        // PublicPost: bytes are signed plaintext, byte-identical (no seal).
        assert_eq!(payload.primary.bytes, raw);
        assert!(payload.thumbnail.is_none());
        // Non-image input → octet-stream MIME / no thumbnail / no C2PA (the real
        // on-device `process_media` only sniffs + thumbnails actual images; the
        // `public_post_image_yields_thumbnail_part` sibling covers the image path).
        assert_eq!(
            payload.primary.sidecar_cbor,
            sidecar_cbor(AudienceClass::PublicPost, "application/octet-stream")
        );
    }

    // A real PNG larger than the 300×300 thumbnail threshold, so the real
    // on-device `process_media` (always enabled for `fauna-ffi`) sniffs
    // `image/png` AND renders a JPEG thumbnail.
    use fauna_media::test_fixtures::build_png;

    #[test]
    fn public_post_image_yields_thumbnail_part() {
        // The feed-image producer: a real >300px image attached to a PublicPost
        // is sniffed (image/png), thumbnailed, and the binding threads the
        // (plaintext) thumbnail through as a second multipart part the client
        // POSTs. Locks the binding seam activated by the 2026-06-30
        // `process_media` flip — the
        // non-image sibling above asserts the `None` path.
        let png = build_png(800, 600);
        let payload = process_and_seal_upload(png.clone(), FfiUploadAudience::PublicPost).unwrap();

        // PublicPost: signed plaintext, bytes pass through unsealed.
        assert_eq!(payload.primary.bytes, png);
        let primary_sidecar =
            UploadSidecar::from_dag_cbor(&payload.primary.sidecar_cbor).expect("primary decodes");
        assert_eq!(primary_sidecar.class, AudienceClass::PublicPost);
        assert_eq!(primary_sidecar.mime, "image/png");
        assert!(
            primary_sidecar.thumbnail_hash.is_some(),
            "the primary sidecar routes `?thumb=1` to the thumbnail blob"
        );

        // The thumbnail part the client POSTs: a plaintext JPEG, its own sidecar
        // thumbnail-less (thumbnails don't have thumbnails).
        let thumb = payload
            .thumbnail
            .expect("a >300px PublicPost image yields a thumbnail");
        let thumb_sidecar =
            UploadSidecar::from_dag_cbor(&thumb.sidecar_cbor).expect("thumbnail decodes");
        assert_eq!(thumb_sidecar.class, AudienceClass::PublicPost);
        assert_eq!(thumb_sidecar.mime, "image/jpeg");
        assert_eq!(thumb_sidecar.thumbnail_hash, None);
    }

    #[test]
    fn library_seals_bytes_under_backup_key() {
        let raw = b"owner-only library media bytes".to_vec();
        let payload = process_and_seal_upload(
            raw.clone(),
            FfiUploadAudience::Library {
                backup_key: vec![7u8; 32],
            },
        )
        .unwrap();

        // Sealed: an AEAD envelope, never byte-identical to the plaintext, and
        // carrying at least the 12-byte nonce + 16-byte tag floor over it.
        assert_ne!(payload.primary.bytes, raw);
        assert!(payload.primary.bytes.len() >= raw.len() + 28);
        assert_eq!(
            payload.primary.sidecar_cbor,
            sidecar_cbor(AudienceClass::Library, "application/octet-stream")
        );
    }

    #[test]
    fn library_rejects_wrong_length_backup_key() {
        match process_and_seal_upload(
            b"x".to_vec(),
            FfiUploadAudience::Library {
                backup_key: vec![0u8; 16],
            },
        ) {
            Err(FfiError::General { msg }) => assert!(msg.contains("32 bytes"), "got: {msg}"),
            // A local seal never talks to a nest, so every typed nest verdict is
            // impossible here — one arm, so a new verdict variant doesn't have to
            // touch this test.
            Err(other) => panic!("local seal can only fail in the general bucket, got {other:?}"),
            Ok(_) => panic!("expected wrong-length backup_key to be rejected"),
        }
    }

    #[test]
    fn strip_media_metadata_is_lossless_and_no_seal() {
        // Non-image bytes pass through byte-identical (no seal, unlike
        // process_and_seal_upload) — this face never encrypts, it only strips.
        let raw = b"not an image, just backup bytes".to_vec();
        assert_eq!(strip_media_metadata(raw.clone()), raw);

        // A metadata-free real image round-trips byte-identical too.
        let png = build_png(10, 10);
        assert_eq!(strip_media_metadata(png.clone()), png);
    }
}

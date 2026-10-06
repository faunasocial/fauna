//! Uploading a composed post's blob attachment — the client-glue half of the
//! feed composer.
//!
//! `feed.md` § Where logic lives draws the line: the post *build* is shared
//! (`FeedManager::submit_post` validates, builds, signs and creates), but the
//! file picker and the blob **upload** are client glue, because they ride the
//! platform's HTTP/bulk plane rather than WS-RPC. `submit_post` therefore takes
//! an attachment whose `blob_hash` is **already resolved** — resolving it is
//! this module's job.
//!
//! It lives here, in `fauna-client`, rather than in `fauna-feed`: the upload
//! needs `NestContentApi` (reqwest multipart), and `fauna-feed` is deliberately
//! wasm-clean — it backs the web SPA through `fauna-wasm`, and its Cargo.toml
//! goes out of its way to keep native-only deps out of that graph. `fauna-client`
//! is native-only and already owns the content-API edge, so both direct-Rust
//! clients (linux, cli) reach it here instead of hand-rolling it twice.

use fauna_media::audience::{Audience, RestrictedPostAudience};
use fauna_media::pipeline::{
    MultipartBlob, UploadPayload, process_and_seal, process_and_seal_with_mime,
};
use fauna_nest_http::{NestContentApi, paths};

/// A blob that has been sealed, uploaded, and had its content hash resolved —
/// everything `fauna_feed::AttachedFile` needs, without this crate having to
/// depend on `fauna-feed` (which would invert the layering: a transport crate
/// reaching up into a feature manager). Each app maps it across in one
/// struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedBlob {
    pub name: String,
    pub size: u64,
    /// The primary blob's content hash, hex — the value `submit_post` requires.
    pub blob_hash: String,
    /// The plaintext's real sniffed MIME. For a public blob this is also the
    /// sidecar's MIME (the sidecar carries the plaintext, so both are the same
    /// string); for a sealed blob the sidecar is pinned to
    /// `application/octet-stream` and this is instead what the sealed body's
    /// `MediaItem` names, once its reader decrypts it.
    pub media_type: String,
}

/// Upload `data` (already in memory — e.g. a zip member read by the archive
/// importer) as a `PublicPost` blob (sealed + sidecar multipart, the same wire
/// path `upload_blob` takes) and resolve its content hash.
///
/// The bytes are processed and sealed on the way out; the thumbnail upload is
/// **best-effort** (the nest does not gate the primary on it, and the
/// `MediaItem` the manager builds carries only the primary hash today), so a
/// thumbnail failure warns rather than failing the post.
pub async fn upload_public_post_blob_bytes(
    nest: &dyn NestContentApi,
    name: &str,
    data: &[u8],
) -> Result<UploadedBlob, String> {
    let size = data.len() as u64;

    let upload = public_post_upload_payload(data);
    let media_type = upload.primary_sidecar.mime.clone();
    // The shared flattening, so every app POSTs byte-identical multipart
    // parts for the same input (priority #1/#2).
    let (primary, thumbnail) = upload.into_multipart_parts();
    let blob_hash = upload_prepared_blob(nest, primary, thumbnail).await?;

    Ok(UploadedBlob {
        name: name.to_string(),
        size,
        blob_hash,
        media_type,
    })
}

/// POST an **already processed and sealed** blob's multipart parts and return
/// the nest's hex content hash.
///
/// The one transport body every blob producer in this module shares, and the
/// door for a producer that sealed its bytes *elsewhere* — notably
/// `FeedManager::seal_compose_attachment`, which must own the seal because a
/// tier's period key may not cross the FFI boundary (`ui/media.md` § Encryption
/// at rest), leaving this crate with nothing to do but carry the bytes.
///
/// **Thumbnail first, and best-effort.** The primary's sidecar already names
/// the thumbnail's hash, so the nest must not record a pointer to a blob that
/// is not in the store yet; and a thumbnail failure must never fail the post
/// (`fauna_media::pipeline::UploadPayload::into_multipart_parts` owns the
/// ordering rule and its rationale).
pub async fn upload_prepared_blob(
    nest: &dyn NestContentApi,
    primary: MultipartBlob,
    thumbnail: Option<MultipartBlob>,
) -> Result<String, String> {
    if let Some(thumb) = thumbnail
        && let Err(e) = nest
            .post_multipart_blob(paths::blob::UPLOAD, thumb.sidecar_cbor, thumb.bytes)
            .await
    {
        tracing::warn!("blob upload: thumbnail failed (non-fatal): {e}");
    }

    let resp = nest
        .post_multipart_blob(paths::blob::UPLOAD, primary.sidecar_cbor, primary.bytes)
        .await
        .map_err(|e| format!("blob upload: {e}"))?;
    serde_json::from_slice::<serde_json::Value>(&resp)
        .ok()
        .and_then(|v| v.get("hash").and_then(|h| h.as_str()).map(String::from))
        .ok_or_else(|| "blob upload response missing hash".to_string())
}

/// Upload `file_path` as a `PublicPost` blob — reads the file, derives `name`
/// from its filename, and delegates to [`upload_public_post_blob_bytes`].
pub async fn upload_public_post_blob(
    nest: &dyn NestContentApi,
    file_path: &str,
) -> Result<UploadedBlob, String> {
    let data = std::fs::read(file_path).map_err(|e| format!("read {file_path}: {e}"))?;
    let name = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    upload_public_post_blob_bytes(nest, &name, &data).await
}

/// Build the seal+sidecar upload payload for a public-post blob attachment.
///
/// Feed posts are always public (`build_feed_post_payload` sets
/// `Post { gated: None }`), so every blob attachment is the `PublicPost`
/// audience. `process_and_seal` runs the native `process_media` (EXIF/IPTC
/// strip, MIME sniff, thumbnail derivation, C2PA detect) and then the audience
/// seal — which for `PublicPost` passes the bytes through unsealed
/// (`encryption-at-rest.md` Media row: public-post media is signed plaintext,
/// no key, byte-identical in both storage modes).
///
/// `post_id` is unused by the `PublicPost` seal (it derives no key), and the
/// blob is uploaded before the post that references it exists, so there is no
/// real id yet — a zero placeholder is the honest, correct value for this
/// audience. The binding (which post references this blob) lives on the post's
/// reference list, not in the sidecar.
fn public_post_upload_payload(raw: &[u8]) -> UploadPayload {
    let audience = Audience::PublicPost {
        post_id: fauna_core::data::ContentHash::from_digest_raw([0u8; 32]),
    };
    process_and_seal(raw, &audience)
}

/// Upload a gated post's **already-sealed** full-body blob (the
/// `fauna_client_core::post::build_gated_post` output: `encrypt_content` under
/// `derive_post_key(period_key, seal_id)`) and return the nest's hex hash —
/// which the caller passes to `FeedManager::submit_gated_post` (it must echo
/// the staged post's `encrypted_ref`).
///
/// The sidecar is the sealed-class shape the strict blob verifier expects
/// (`ui/feed.md` § Encryption at rest): class `PeriodRestrictedPost`, mime
/// `application/octet-stream` (the real MIME rides inside the seal), no
/// thumbnail. No `process_and_seal` here — the bytes are sealed by the post
/// builder; this is transport glue only, shared so all apps ship
/// byte-identical sidecars (priority #1).
pub async fn upload_gated_post_blob(
    nest: &dyn NestContentApi,
    sealed: Vec<u8>,
) -> Result<String, String> {
    upload_sealed_post_blob(
        nest,
        fauna_media::sidecar::UploadSidecar::gated_post(),
        sealed,
    )
    .await
}

/// [`upload_gated_post_blob`] with the sidecar named by the caller — the one
/// transport for every restricted post's sealed body, whichever audience
/// sealed it. A caller holding a staged post passes
/// `FeedManager::gated_upload_sidecar`, which answers the room arm's
/// `GroupRestrictedPost` for a room post and `PeriodRestrictedPost` otherwise,
/// so no app chooses a class itself.
pub async fn upload_sealed_post_blob(
    nest: &dyn NestContentApi,
    sidecar: fauna_media::sidecar::UploadSidecar,
    sealed: Vec<u8>,
) -> Result<String, String> {
    let sidecar = sidecar.to_dag_cbor();
    let resp = nest
        .post_multipart_blob(paths::blob::UPLOAD, sidecar, sealed)
        .await
        .map_err(|e| format!("gated blob upload: {e}"))?;
    serde_json::from_slice::<serde_json::Value>(&resp)
        .ok()
        .and_then(|v| v.get("hash").and_then(|h| h.as_str()).map(String::from))
        .ok_or_else(|| "gated blob upload response missing hash".to_string())
}

/// The period-restricted seal's inputs for one gated post's media attachment
/// — an archive-import zip member whose original post was gated to a
/// subscription tier's period.
///
/// `period_key` is caller-supplied secret material (the same period key a
/// gated post's body was sealed under); this type deliberately does **not**
/// derive `Debug` so it can never be logged by accident.
pub struct PeriodMediaSeal {
    /// The gated post's own seal id — sealed media derives its per-blob key
    /// from `derive_post_key(period_key, seal_id)`, the same id the post
    /// body itself opens under, so one reader opens both.
    pub seal_id: [u8; 32],
    pub tier: String,
    pub period_version: u64,
    pub period_key: zeroize::Zeroizing<[u8; 32]>,
}

/// The `Audience::RestrictedPost { .. Period }` [`seal_for_audience`]
/// dispatches on, built from a [`PeriodMediaSeal`].
fn period_sealed_audience(seal: &PeriodMediaSeal) -> Audience {
    Audience::RestrictedPost {
        post_id: fauna_core::data::ContentHash::from_digest_raw(seal.seal_id),
        audience: RestrictedPostAudience::Period {
            tier: seal.tier.clone(),
            period_epoch: seal.period_version,
            period_key: seal.period_key.clone(),
        },
    }
}

/// Upload `data` sealed for an audience-restricted (subscription-period-gated)
/// post: `process_media` then `Audience::RestrictedPost { post_id: seal_id,
/// audience: Period { .. } }` — the media path for an archive-import zip
/// member whose original post is gated behind a subscription tier's period.
///
/// The sidecar is the sealed-class shape (`application/octet-stream`, no C2PA
/// flag) the strict verifier expects for an AEAD-sealed class; the real MIME
/// rides inside the seal — opened under the same
/// `derive_post_key(period_key, seal_id)` the gated post body itself opens
/// under — and is returned here, so the `MediaItem` the reader opens by
/// carries the correct type.
pub async fn upload_period_sealed_media(
    nest: &dyn NestContentApi,
    name: &str,
    data: &[u8],
    seal: &PeriodMediaSeal,
) -> Result<UploadedBlob, String> {
    let size = data.len() as u64;
    let (upload, media_type) = process_and_seal_with_mime(data, &period_sealed_audience(seal));
    let (primary, thumbnail) = upload.into_multipart_parts();
    let blob_hash = upload_prepared_blob(nest, primary, thumbnail).await?;

    Ok(UploadedBlob {
        name: name.to_string(),
        size,
        blob_hash,
        media_type,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        PeriodMediaSeal, period_sealed_audience, process_and_seal_with_mime,
        public_post_upload_payload,
    };
    use fauna_media::audience::AudienceClass;

    /// Exercises the actual production path,
    /// `fauna_media::pipeline::process_and_seal_with_mime` — not a test-only
    /// stand-in — because that is where the highest-risk logic lives:
    /// recovering the plaintext's real MIME while the sidecar is forced to
    /// `application/octet-stream`. The pin stays here, on the archive import's
    /// own caller, even though the function itself moved into `fauna-media`
    /// (the byte-for-byte copy that used to live in this module was lifted
    /// there once the gated-compose seal became a third caller).
    ///
    /// The input is a **real** PNG signature (the exact 8 bytes
    /// `sniff_known_prefix` keys on) followed by bytes no PNG decoder can
    /// parse — so `process_media` sniffs `image/png` (a signature check, no
    /// decode) but `img_parts::png::Png::from_bytes` fails, so `strip_exif_iptc`
    /// bails and passes the bytes through unchanged, and `render_thumbnail`'s
    /// `image::ImageReader::decode()` fails too, yielding no thumbnail — an
    /// upload the pipeline still completes successfully. This makes the pin
    /// meaningful: `mime` comes back `"image/png"` (the real sniff) while
    /// `primary_sidecar.mime` stays `"application/octet-stream"` (the sealed
    /// contract) — the two **must differ**, which is the whole point of the
    /// ruling that added this test. The payload still opens under the same
    /// `derive_post_key(period_key, seal_id)` the gated post body uses, so the
    /// reader that opens the body opens its photos; and `payload.thumbnail`
    /// stays `None` on this input exactly as `process_and_seal` would report
    /// for the same bytes, so a future divergence of the hand-copied thumbnail
    /// arm would show here.
    #[test]
    fn period_sealed_media_payload_returns_the_real_mime_and_opens_under_the_post_key() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
        let mut raw = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        raw.extend_from_slice(b"not a decodable PNG body, just the signature");
        let seal = PeriodMediaSeal {
            seal_id: [4u8; 32],
            tier: "only-me".into(),
            period_version: 1,
            period_key: zeroize::Zeroizing::new([3u8; 32]),
        };
        let (payload, mime) = process_and_seal_with_mime(&raw, &period_sealed_audience(&seal));

        // (a) the sealed class/sidecar shape.
        assert_eq!(
            payload.primary_sidecar.class,
            AudienceClass::PeriodRestrictedPost
        );
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);

        // (c) the returned MIME is the real sniff, and it differs from the
        // sidecar's forced-sealed-class MIME — the point of this ruling.
        assert_eq!(mime, "image/png");
        assert_ne!(mime, payload.primary_sidecar.mime);

        // (b) the body opens under the same key the gated post body uses.
        let plain = decrypt_content(
            &derive_post_key(
                &seal.period_key,
                &ContentHash::from_digest_raw(seal.seal_id),
            ),
            &payload.primary.bytes,
        )
        .expect("opens under the post key");
        assert_eq!(plain, raw);

        // An undecodable body yields no thumbnail — the same answer
        // `process_and_seal` gives for these bytes, which is now true by
        // construction (it delegates here).
        assert!(payload.thumbnail.is_none());
    }

    #[test]
    fn public_post_attachment_is_passthrough_with_public_post_class() {
        // Non-image bytes: process_media is a no-op strip (no thumbnail, no
        // C2PA detected), and the PublicPost seal passes the bytes through
        // unchanged. This pins the audience the composer attaches under.
        let raw = b"not an image, just some attachment bytes";
        let payload = public_post_upload_payload(raw);

        assert_eq!(payload.primary_sidecar.class, AudienceClass::PublicPost);
        assert_eq!(payload.primary.class, AudienceClass::PublicPost);
        assert_eq!(
            payload.primary.bytes,
            raw.to_vec(),
            "PublicPost bytes pass through unsealed"
        );
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);
        assert_eq!(payload.primary_sidecar.thumbnail_hash, None);
        assert!(payload.thumbnail.is_none());
    }
}

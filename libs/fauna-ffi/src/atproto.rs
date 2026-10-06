//! ATProto PDS-bridge projection FFI: deterministic TID minting + Fauna →
//! `app.bsky.*` record translation.
//!
//! Consumer: the Go atproto.pds bridge via the tracked uniffi-bindgen-go
//! binding — the bridge pulls raw stored records from nest
//! (`fauna.bridges.atproto.fetch_public_posts` / `fetch_profile`) and calls
//! these pure translators to re-derive each user's repo deterministically.
//! Backed by `fauna-bridge-atproto` with `default-features = false` (the
//! pure core only — no reqwest/tokio/atrium).
//!
//! D8 (the authorization module) is NOT wrapped here: uniffi-bindgen-go emits
//! one Go package per namespace and cannot resolve a type across two, so
//! `authorize` is exported from `fauna_bridge_atproto` itself, beside the
//! `AuthzInput`/`AuthzVerdict` types it takes. fauna-ffi still enables that
//! crate's `uniffi` feature, which is what links its scaffolding into
//! libfauna_ffi.

use fauna_bridge_atproto::outbound::{
    QuoteRef, ReplyRefs, ResolvedImage, ResolvedVideo, deterministic_tid,
    extract_profile_media_from_bytes, extract_projection_media_from_bytes,
    extract_projection_refs_from_bytes, extract_projection_video_from_bytes,
    translate_post_bytes_for_projection, translate_profile_bytes_for_projection,
};

use crate::FfiError;

/// Deterministic record key for a projected post (D-s3-2): the 13-char
/// base32-sortable TID string for
/// `(top bit 0 | 53-bit created_at µs | 10-bit first-10-bits-of-BLAKE3(post_id))`.
///
/// `created_at_micros` is `Post.created_at` (microseconds since Unix epoch);
/// out-of-range values clamp (saturating) into the 53-bit range. `post_id` is
/// the PostId wire bytes (the 36-byte dag-cbor CID). The profile record's
/// rkey is the literal `"self"` — no TID involved.
#[uniffi::export]
pub fn atproto_deterministic_tid(created_at_micros: i64, post_id: Vec<u8>) -> String {
    deterministic_tid(created_at_micros, &post_id)
}

/// Translate stored Fauna post bytes (the `fetch_public_posts` payload —
/// signed embed-as-bytes wire or bare canonical `Post`) into the
/// deterministic `app.bsky.feed.post` record JSON.
///
/// `reply_json`, when present, is a JSON object
/// `{"parent_uri":…,"parent_cid":…,"root_uri":…,"root_cid":…}` (AT URI + CID
/// of the direct parent and thread root, resolved by the Go caller from its
/// PostId→record map). `quote_json` is `{"uri":…,"cid":…}` of the quoted
/// post. `self_labels` is the D-s3-3 mechanism — v1 callers pass empty.
///
/// `images` are the attachments the caller has already fetched, re-hashed to
/// their ATProto sha256 CIDs and **stored** — passing one the PDS cannot serve
/// would publish a dangling ref. Pass an empty list to project with no media
/// embed, which is also the right call when every attachment was unfetchable:
/// the post publishes, minus the image. `video` is the same contract for the
/// single mp4 blob the caller assembled from one rendition
/// ([`atproto_extract_post_video`]); it and `images` are mutually exclusive by
/// construction.
///
/// Returns `Ok(None)` when the post must **not** be projected at all, because
/// the record would carry neither text nor an embed — a media post whose every
/// attachment was dropped, or a video whose renditions all exceeded the
/// caller's ceiling. Committing that record would publish a blank post
/// (`atproto-pds-bridge.md` § Projection & backfill). The caller skips it and
/// lets the watermark advance; the post does not enter `post_map`.
#[uniffi::export]
pub fn atproto_translate_post_record(
    post_bytes: Vec<u8>,
    reply_json: Option<String>,
    quote_json: Option<String>,
    images: Vec<AtprotoResolvedImage>,
    video: Option<AtprotoResolvedVideo>,
    self_labels: Vec<String>,
) -> Result<Option<String>, FfiError> {
    let reply: Option<ReplyRefs> = reply_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| FfiError::General {
            msg: format!("parse atproto reply refs: {e}"),
        })?;
    let quote: Option<QuoteRef> = quote_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| FfiError::General {
            msg: format!("parse atproto quote ref: {e}"),
        })?;
    let images: Vec<ResolvedImage> = images.into_iter().map(Into::into).collect();
    let video: Option<ResolvedVideo> = video.map(Into::into);
    translate_post_bytes_for_projection(
        &post_bytes,
        reply.as_ref(),
        quote.as_ref(),
        &images,
        video.as_ref(),
        &self_labels,
    )
    .map_err(|e| FfiError::General {
        msg: format!("translate atproto post record: {e}"),
    })
}

/// The single mp4 blob the projection attaches to a video record — the FFI face
/// of [`fauna_bridge_atproto::outbound::ResolvedVideo`], on the same
/// caller-has-already-stored-it contract as [`AtprotoResolvedImage`].
///
/// Unlike an image blob, these bytes are not a Fauna blob republished: the
/// caller concatenated one rendition's segments and remuxed them to mp4, so the
/// blob exists only here. `size_bytes` is signed for the same reason as on
/// [`AtprotoResolvedImage`]; negative values clamp to 0.
#[derive(Debug, uniffi::Record)]
pub struct AtprotoResolvedVideo {
    pub blob_cid: String,
    /// Always `video/mp4` — carried rather than assumed so the blob ref and the
    /// stored blob's recorded type cannot drift apart.
    pub mime: String,
    pub size_bytes: i64,
    pub aspect_width: u32,
    pub aspect_height: u32,
}

impl From<AtprotoResolvedVideo> for ResolvedVideo {
    fn from(v: AtprotoResolvedVideo) -> Self {
        ResolvedVideo {
            blob_cid: v.blob_cid,
            mime: v.mime,
            size_bytes: v.size_bytes.max(0) as u64,
            aspect_width: v.aspect_width,
            aspect_height: v.aspect_height,
        }
    }
}

/// One resolution's worth of a Fauna video — the FFI face of
/// [`fauna_bridge_atproto::outbound::ProjectionVideoRendition`].
#[derive(Debug, uniffi::Record)]
pub struct AtprotoVideoRendition {
    /// Height in pixels — 360, 720, 1080, 2160.
    pub height: u32,
    /// The **Fauna** content CIDs of this rendition's segments, base32, in
    /// playback order — each a path segment of `GET /api/v1/blob/{cid_b32}`.
    /// Concatenating the fetched bytes in this order yields a playable MPEG-TS
    /// stream, because they came from one ffmpeg HLS run.
    pub segment_cids: Vec<String>,
    /// The sum of the segments' declared sizes — what the caller weighs against
    /// its ceiling before fetching. An upper bound on the assembled mp4, never
    /// an underestimate: the remux strips MPEG-TS packet headers.
    pub declared_bytes: i64,
}

/// A Fauna video's publishable shape — the FFI face of
/// [`fauna_bridge_atproto::outbound::ProjectionVideo`].
#[derive(Debug, uniffi::Record)]
pub struct AtprotoVideo {
    /// The video's manifest CID, base32 — the stable per-video identity to
    /// dedup on. **Not** the identity of the published bytes, which the caller
    /// assembles and which exist nowhere in Fauna.
    pub manifest_cid: String,
    /// Every rendition, highest resolution first: take the first that fits the
    /// ceiling and you have the best copy this PDS can serve.
    pub renditions: Vec<AtprotoVideoRendition>,
    pub aspect_width: u32,
    pub aspect_height: u32,
}

/// Extract the publishable video of stored Fauna post bytes, so the Go
/// projection can assemble one rendition and feed the result back in as
/// [`atproto_translate_post_record`]'s `video`.
///
/// `None` = the post carries no publishable video: it is not a video post, it
/// has no segments, or its aspect ratio is degenerate (a required `aspectRatio`
/// with no honest source drops the media rather than inventing one).
///
/// The *choice* between renditions is deliberately left to the caller: it is a
/// byte-ceiling decision, and the ceiling lives with whoever does the fetching
/// (`atproto-pds-bridge.md` § Where logic lives).
#[uniffi::export]
pub fn atproto_extract_post_video(post_bytes: Vec<u8>) -> Result<Option<AtprotoVideo>, FfiError> {
    let video =
        extract_projection_video_from_bytes(&post_bytes).map_err(|e| FfiError::General {
            msg: format!("extract atproto post video: {e}"),
        })?;
    Ok(video.map(|v| AtprotoVideo {
        manifest_cid: v.manifest_cid,
        renditions: v
            .renditions
            .into_iter()
            .map(|r| AtprotoVideoRendition {
                height: u32::from(r.height),
                segment_cids: r.segment_cids,
                declared_bytes: i64::try_from(r.declared_bytes).unwrap_or(i64::MAX),
            })
            .collect(),
        aspect_width: v.aspect_width,
        aspect_height: v.aspect_height,
    }))
}

/// One already-fetched, already-stored image the projection attaches to a
/// record — the FFI face of [`fauna_bridge_atproto::outbound::ResolvedImage`].
///
/// `blob_cid` is the **ATProto** blob CID (CIDv1, raw codec, sha2-256 of the
/// bytes the caller stored), never the Fauna BLAKE3 CID that
/// [`atproto_extract_post_media`] handed out; the re-hash is the caller's job
/// because it needs the bytes, and bytes never cross this boundary.
///
/// `size_bytes` is an `i64` rather than a `u64` on purpose: uniffi maps `u64`
/// onto Go's `uint64` fine, but every other size on this surface is already
/// signed (`created_at_micros`), and a blob larger than 2^63 bytes is not a
/// representable failure mode. Negative values are clamped to 0.
#[derive(Debug, uniffi::Record)]
pub struct AtprotoResolvedImage {
    pub blob_cid: String,
    pub mime: String,
    pub size_bytes: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: String,
}

impl From<AtprotoResolvedImage> for ResolvedImage {
    fn from(img: AtprotoResolvedImage) -> Self {
        ResolvedImage {
            blob_cid: img.blob_cid,
            mime: img.mime,
            size_bytes: img.size_bytes.max(0) as u64,
            width: img.width,
            height: img.height,
            alt: img.alt,
        }
    }
}

/// One image attachment the projection should try to publish — the FFI face of
/// [`fauna_bridge_atproto::outbound::ProjectionMediaItem`].
///
/// `blob_cid` is the **Fauna** content CID, base32 — exactly the path segment
/// of `GET /api/v1/blob/{cid_b32}`, the sanctioned CID-addressed byte surface
/// (`api-layers.md` § The four API protocols), so the Go caller fetches it
/// without deriving anything.
#[derive(Debug, uniffi::Record)]
pub struct AtprotoMediaItem {
    pub blob_cid: String,
    pub mime: String,
    pub size_bytes: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: String,
}

/// Extract the image attachments of stored Fauna post bytes, so the Go
/// projection can fetch, re-hash and store each one and feed the result back in
/// as [`atproto_translate_post_record`]'s `images`.
///
/// Already filtered and capped by the shared translator: non-image
/// attachments (anything outside the `image/` MIME prefix) are dropped
/// (ATProto has no generic file embed), a video post yields nothing **here**
/// (it is assembled rather than republished — see
/// [`atproto_extract_post_video`]), and the list is capped at the lexicon's
/// four. So Go fetches what it is given and interprets nothing.
#[uniffi::export]
pub fn atproto_extract_post_media(post_bytes: Vec<u8>) -> Result<Vec<AtprotoMediaItem>, FfiError> {
    let items =
        extract_projection_media_from_bytes(&post_bytes).map_err(|e| FfiError::General {
            msg: format!("extract atproto post media: {e}"),
        })?;
    Ok(items
        .into_iter()
        .map(|item| AtprotoMediaItem {
            blob_cid: item.blob_cid,
            mime: item.mime,
            size_bytes: i64::try_from(item.size_bytes).unwrap_or(i64::MAX),
            width: item.width,
            height: item.height,
            alt: item.alt,
        })
        .collect())
}

/// The Fauna posts a stored post refers to, as lowercase-hex post ids — the FFI
/// face of [`fauna_bridge_atproto::outbound::ProjectionRefs`].
///
/// Each id is directly a key of the bridge's `post_map` (and of the
/// `fetch_public_posts` wire), so the Go caller looks it up without deriving
/// anything. `None` = the post carries no reference of that kind.
#[derive(Debug, uniffi::Record)]
pub struct AtprotoPostRefs {
    /// The post this one replies to.
    pub reply_parent_post_id: Option<String>,
    /// The post this one quotes.
    pub quote_post_id: Option<String>,
}

/// Extract the reply/quote targets of stored Fauna post bytes, so the Go
/// projection can resolve them against its PostId→AT-URI map and feed the
/// result back in as [`atproto_translate_post_record`]'s `reply_json` /
/// `quote_json`.
///
/// Returned as a typed record rather than JSON: these values are produced *by*
/// Rust (unlike the ref params, which the Go caller composes), so there is no
/// reason to make Go parse a string. Repost/react/vote references are ignored —
/// they have no `app.bsky.feed.post` field.
#[uniffi::export]
pub fn atproto_extract_post_refs(post_bytes: Vec<u8>) -> Result<AtprotoPostRefs, FfiError> {
    let refs = extract_projection_refs_from_bytes(&post_bytes).map_err(|e| FfiError::General {
        msg: format!("extract atproto post refs: {e}"),
    })?;
    Ok(AtprotoPostRefs {
        reply_parent_post_id: refs.reply_parent,
        quote_post_id: refs.quote,
    })
}

/// Extract the AT-URIs an **external app's** record refers to — its reply
/// parent and the post it quotes — so the Go bridge can resolve them against
/// `post_map` and send the resolutions to the nest as
/// `ExternalWrite.resolved_targets` (F2.2 slice 4b).
///
/// The exact inverse direction of [`atproto_extract_post_refs`]: that one reads
/// a *Fauna* post and yields *Fauna* post ids; this one reads an *ATProto*
/// record and yields *AT-URIs*.
///
/// Deliberately **infallible and untyped by reference kind.** A record this
/// cannot read yields no references, and the nest then journals — the ratified
/// fallback (`atproto-pds-full.md` § F2 detail) — rather than failing the
/// user's write. And the list says nothing about which URI is the reply parent
/// versus the quote, because the bridge's whole job here is "what Fauna post,
/// if any, sits at this URI"; the nest parses the record anyway and is the only
/// side that needs the reference's meaning.
#[uniffi::export]
pub fn atproto_extract_record_refs(record_cbor: Vec<u8>) -> Vec<String> {
    fauna_bridge_atproto::record_refs::external_record_refs(&record_cbor)
}

/// Extract every blob CID a record references — the generic blob-shape walk
/// (`record_refs::external_record_blob_refs`), F2.4 slice 2.
///
/// The bridge fills the `#commit` frame's `blobs` field from this, and the
/// nest stamps `atproto_blobs.referenced_at` from the same Rust walk — one
/// walker, two consumers, so the frame's announcement and the GC guard cannot
/// disagree about which blobs a record names. Infallible like the refs
/// extraction above: unreadable bytes announce nothing.
#[uniffi::export]
pub fn atproto_extract_blob_refs(record_cbor: Vec<u8>) -> Vec<String> {
    fauna_bridge_atproto::record_refs::external_record_blob_refs(&record_cbor)
}

/// Translate stored Fauna profile bytes (the signed embed-as-bytes wire
/// `fauna.profile.set` stores, decoded via the shared signed-only verify
/// `decode_profile`) into the `app.bsky.actor.profile` record JSON, served
/// at rkey `"self"`.
///
/// `avatar` and `banner` are the pictures the caller has already fetched,
/// re-hashed to their ATProto sha256 CIDs and **stored** — the same contract
/// [`atproto_translate_post_record`]'s `images` carries. Passing `None` omits
/// that field from the record entirely, which is both how a *cleared* picture
/// stops being referenced and the right answer when the bytes could not be
/// published: the profile projects without it rather than not at all.
#[uniffi::export]
pub fn atproto_translate_profile_record(
    profile_bytes: Vec<u8>,
    avatar: Option<AtprotoResolvedImage>,
    banner: Option<AtprotoResolvedImage>,
) -> Result<String, FfiError> {
    let avatar: Option<ResolvedImage> = avatar.map(Into::into);
    let banner: Option<ResolvedImage> = banner.map(Into::into);
    translate_profile_bytes_for_projection(&profile_bytes, avatar.as_ref(), banner.as_ref())
        .map_err(|e| FfiError::General {
            msg: format!("translate atproto profile record: {e}"),
        })
}

/// The avatar and banner of a stored Fauna profile — the FFI face of
/// [`fauna_bridge_atproto::outbound::ProfileMedia`].
///
/// Each is `None` when the profile carries no such picture. Both descriptors
/// name a **Fauna** content CID (the `GET /api/v1/blob/{cid_b32}` path
/// segment) and, unlike a post attachment, declare **no MIME type**: a profile
/// stores a bare `ContentHash`, so the caller that fetches the bytes is the one
/// that determines what they are.
#[derive(Debug, uniffi::Record)]
pub struct AtprotoProfileMedia {
    pub avatar: Option<AtprotoMediaItem>,
    pub banner: Option<AtprotoMediaItem>,
}

/// Extract the avatar/banner of stored Fauna profile bytes, so the Go
/// projection can fetch, re-hash and store each one and feed the result back
/// in as [`atproto_translate_profile_record`]'s `avatar` / `banner`.
///
/// The profile analogue of [`atproto_extract_post_media`], and the reason the
/// projection can reference a picture at all: § Projection & backfill's
/// "fetch, re-hash, store, THEN reference" applies to profile pictures exactly
/// as it does to post attachments.
#[uniffi::export]
pub fn atproto_extract_profile_media(
    profile_bytes: Vec<u8>,
) -> Result<AtprotoProfileMedia, FfiError> {
    let media =
        extract_profile_media_from_bytes(&profile_bytes).map_err(|e| FfiError::General {
            msg: format!("extract atproto profile media: {e}"),
        })?;
    let item = |i: fauna_bridge_atproto::outbound::ProjectionMediaItem| AtprotoMediaItem {
        blob_cid: i.blob_cid,
        mime: i.mime,
        size_bytes: i64::try_from(i.size_bytes).unwrap_or(i64::MAX),
        width: i.width,
        height: i.height,
        alt: i.alt,
    };
    Ok(AtprotoProfileMedia {
        avatar: media.avatar.map(item),
        banner: media.banner.map(item),
    })
}

/// The canonical DAG-CBOR `UploadSidecar` bytes for an INBOUND blob — the
/// `sidecar` part of the multipart `POST /api/v1/blob` the atproto bridge sends
/// when serving `com.atproto.repo.uploadBlob` (F2.4 slice 1).
///
/// **Exported rather than hand-rolled in Go for the `appview_service_did`
/// reason.** These bytes are strict-decoded nest-side
/// (`UploadSidecar::from_dag_cbor` → `fauna_cbor::decode_strict`) and then
/// cross-checked against the body by the per-class envelope verifier, so a Go
/// copy that drifted on canonical map ordering, the `AudienceClass` variant
/// spelling, or how an absent `thumbnail_hash` encodes would fail at the nest
/// with an opaque 400 that says nothing about which of the four fields was
/// wrong. One encoder, no copy that can drift.
///
/// The class is always [`AudienceClass::PublicPost`] — the one class Fauna does
/// not AEAD-seal. That is both what makes the leg possible (the bridge holds no
/// user key material and could seal for no other class) and correct by content:
/// a blob an `app.bsky.*` record references is public by construction. It is
/// also why `mime` is the real sniffed type rather than
/// `application/octet-stream`: for a sealed class the true MIME rides *inside*
/// the ciphertext and the verifier demands the octet-stream placeholder, while
/// `PublicPost` bytes are plaintext and this value is what the nest serves as
/// `Content-Type` on download.
///
/// `has_c2pa` is false and `thumbnail_hash` absent: the bridge does no media
/// processing, and asserting either would be a claim about bytes nobody
/// inspected.
#[uniffi::export]
pub fn atproto_public_post_upload_sidecar(mime: String) -> Vec<u8> {
    fauna_media::sidecar::UploadSidecar {
        class: fauna_media::audience::AudienceClass::PublicPost,
        mime,
        has_c2pa: false,
        thumbnail_hash: None,
    }
    .to_dag_cbor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tid_matches_pure_core_vector() {
        assert_eq!(atproto_deterministic_tid(0, vec![]), "22222222222pw");
    }

    /// The exported bytes must survive the nest's own strict decode and carry
    /// exactly the four field values the `PublicPost` verifier arm requires
    /// (`bins/fauna-nest/src/storage/sealed.rs::classify_per_class_envelope`):
    /// the plaintext class, the real sniffed MIME as a non-empty `type/subtype`,
    /// no C2PA claim, no thumbnail. If this ever goes red, an inbound
    /// `uploadBlob` is failing at the nest with an opaque 400.
    #[test]
    fn the_public_post_sidecar_decodes_strictly_and_carries_the_sniffed_mime() {
        use fauna_media::audience::AudienceClass;
        use fauna_media::sidecar::UploadSidecar;

        let bytes = atproto_public_post_upload_sidecar("image/png".into());
        let back = UploadSidecar::from_dag_cbor(&bytes).expect("the nest's strict decode");
        assert_eq!(back.class, AudienceClass::PublicPost);
        // Plaintext class ⇒ the real type travels in the clear. A sealed class
        // would require the octet-stream placeholder instead, and getting this
        // backwards is what makes the nest serve a wrong Content-Type forever.
        assert_eq!(back.mime, "image/png");
        assert!(back.mime.contains('/') && !back.mime.is_empty());
        assert!(!back.has_c2pa);
        assert!(back.thumbnail_hash.is_none());
        // Not AEAD-sealed is the property the whole leg rests on: the bridge
        // holds no user key material and could seal for no other class.
        assert!(!back.class.is_aead_sealed());
    }

    #[test]
    fn reply_and_quote_json_parse_and_flow_through() {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::encoding::sign_and_pack;
        use fauna_core::identity::ActorKeypair;
        let kp = ActorKeypair::generate();
        let post = Post {
            author: kp.actor_id(),
            created_at: Timestamp(1_774_008_000_000_000),
            body: PostBody::Text {
                content: "over the boundary".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let stored = sign_and_pack(&kp, &post).expect("sign+pack");
        let reply = r#"{"parent_uri":"at://p/app.bsky.feed.post/1","parent_cid":"cp","root_uri":"at://r/app.bsky.feed.post/2","root_cid":"cr"}"#;
        let quote = r#"{"uri":"at://q/app.bsky.feed.post/3","cid":"cq"}"#;
        let out = atproto_translate_post_record(
            stored,
            Some(reply.to_string()),
            Some(quote.to_string()),
            vec![],
            None,
            vec![],
        )
        .expect("translate")
        .expect("a text post with reply and quote must project");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["reply"]["parent"]["cid"], "cp");
        assert_eq!(v["embed"]["record"]["cid"], "cq");

        let err = atproto_translate_post_record(
            vec![1, 2, 3],
            Some("{bad".into()),
            None,
            vec![],
            None,
            vec![],
        )
        .expect_err("bad reply json must error");
        assert!(matches!(err, FfiError::General { .. }));
    }

    /// The extracted ids must be the post_map keys the Go side looks up — the
    /// CID *digest* hex, matching nest's `hex::encode(row.id)` on the wire.
    #[test]
    fn extracted_refs_are_post_map_keys_over_the_boundary() {
        use fauna_core::data::{Post, PostBody, PostId, Reference, Timestamp};
        use fauna_core::encoding::sign_and_pack;
        use fauna_core::identity::ActorKeypair;
        let kp = ActorKeypair::generate();
        let parent = PostId::of_dag_cbor(b"parent");
        let quoted = PostId::of_dag_cbor(b"quoted");
        let post = Post {
            author: kp.actor_id(),
            created_at: Timestamp(1_774_008_000_000_000),
            body: PostBody::Text {
                content: "a reply that also quotes".into(),
                facets: vec![],
            },
            references: vec![
                Reference::Reply { post_id: parent },
                Reference::Quote { post_id: quoted },
            ],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let stored = sign_and_pack(&kp, &post).expect("sign+pack");
        let refs = atproto_extract_post_refs(stored).expect("extract");
        assert_eq!(
            refs.reply_parent_post_id,
            Some(hex::encode(parent.digest()))
        );
        assert_eq!(refs.quote_post_id, Some(hex::encode(quoted.digest())));

        let err = atproto_extract_post_refs(vec![1, 2, 3]).expect_err("non-post must error");
        assert!(matches!(err, FfiError::General { .. }));
    }
}

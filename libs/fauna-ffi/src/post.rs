use crate::{FfiError, bytes_to_post_id, keypair_from_bytes};

#[derive(uniffi::Record)]
pub struct DecodedPost {
    pub post_id: String,
    pub author: String,
    pub body: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub valid: bool,
    /// The D10 audit answer (`atproto-pds-full.md` § D10 → *Audit*): whether an
    /// external app authored this post as the account, via a delegated
    /// authoring sub-key. `Delegated` paints the `delegated-origin-badge`.
    /// Strictly richer than [`Self::valid`], which cannot carry the distinction:
    /// `valid == false` and `authoring_origin == Unknown` always coincide.
    pub authoring_origin: fauna_core::render::AuthoringOriginStatus,
    pub items: Vec<DecodedMediaItem>,
    pub references: Vec<DecodedReference>,
    pub facets: Vec<DecodedFacet>,
    pub content_warning: Option<String>,
}

#[derive(uniffi::Record)]
pub struct DecodedMediaItem {
    pub blob_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
}

#[derive(uniffi::Record)]
pub struct DecodedReference {
    pub ref_type: String,
    pub post_id: String,
    pub emoji: Option<String>,
}

#[derive(uniffi::Record)]
pub struct DecodedFacet {
    pub byte_start: u32,
    pub byte_end: u32,
    pub feature_type: String,
    pub value: String,
}

/// Build and sign a bare Post for the feed (PostBody::Text).
/// Returns dag-cbor-encoded bytes ready to POST to `/api/v1/posts`.
#[uniffi::export]
pub fn build_post(secret: Vec<u8>, body: String) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    fauna_client_core::post::build_post(&kp, &body, &[], None)
        .map_err(|e| FfiError::General { msg: e.0 })
}

/// Build and sign a Post with tags.
/// `tags` is a list of tag strings (without the `#` prefix).
#[uniffi::export]
pub fn build_post_tagged(
    secret: Vec<u8>,
    body: String,
    tags: Vec<String>,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    fauna_client_core::post::build_post(&kp, &body, &tags, None)
        .map_err(|e| FfiError::General { msg: e.0 })
}

/// Build and sign a reply post for the feed.
///
/// `reply_to` is the 36-byte CID (`Cid::as_bytes`) of the post being replied
/// to. Layer 2 of the CBOR-DAG-everywhere migration moved post IDs from
/// bare 32-byte hashes to full 36-byte IPLD CIDs.
#[uniffi::export]
pub fn build_post_reply(
    secret: Vec<u8>,
    body: String,
    reply_to: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let cid_bytes = bytes_to_post_id(&reply_to)?;
    fauna_client_core::post::build_post(&kp, &body, &[], Some(cid_bytes))
        .map_err(|e| FfiError::General { msg: e.0 })
}

/// Build and sign a Post carrying a single media item (`PostBody::TextWithMedia`).
///
/// The media blob is uploaded out-of-band (the HTTP `/api/v1/blob` sidecar);
/// `blob_hash_hex` is its 32-byte raw content hash (hex). `tags` are display-only
/// tag facets (without the `#`). Multi-image posts are out of scope here — the
/// native apps render one image per post (phase 1). Returns the embed-as-bytes
/// wire ready for `fauna.posts.create`. Mirrors the text builders above; shared by
/// Windows/Apple/Android via UniFFI.
#[uniffi::export]
pub fn build_post_with_media(
    secret: Vec<u8>,
    body: String,
    tags: Vec<String>,
    blob_hash_hex: String,
    media_type: String,
    size_bytes: u64,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let digest: [u8; 32] =
        fauna_core::hex32::decode(&blob_hash_hex).map_err(|_| FfiError::General {
            msg: "blob_hash_hex must be 32-byte hex".to_string(),
        })?;
    let media_item = fauna_core::data::MediaItem {
        blob_hash: fauna_core::data::ContentHash::from_digest_raw(digest),
        media_type,
        size_bytes,
        dimensions: None,
        thumbnail: None,
        remote_url: None,
        alt: None,
    };
    fauna_client_core::post::build_post_with_media(&kp, &body, vec![media_item], &tags, None)
        .map_err(|e| FfiError::General { msg: e.0 })
}

/// Whether a dag-cbor-encoded Post's signature verifies.
///
/// The bare validity bool only — [`decode_post_full`] is the richer face, and
/// the one to reach for when a caller needs the D10 authoring origin as well
/// (`atproto-pds-full.md` § D10 → *Audit*). This stays a `bool` deliberately: it
/// is the narrow "did it verify" question, and widening it would make every
/// caller destructure an answer most of them do not use.
#[uniffi::export]
pub fn decode_post(data: Vec<u8>) -> Result<bool, FfiError> {
    let (_post, origin) =
        fauna_client_core::post::decode_post(&data).map_err(|e| FfiError::General { msg: e.0 })?;
    Ok(origin.is_some())
}

/// Decode a dag-cbor-encoded Post into a full structured record.
/// Mirrors the WASM `decode_post_inner` in `libs/fauna-wasm/src/lib.rs` — both
/// marshal `fauna_client_core::post::decode_post_shaped`'s platform-neutral
/// shape into their own binding's record type.
#[uniffi::export]
pub fn decode_post_full(data: Vec<u8>) -> Result<DecodedPost, FfiError> {
    let shaped = fauna_client_core::post::decode_post_shaped(&data)
        .map_err(|e| FfiError::General { msg: e.0 })?;
    Ok(DecodedPost {
        post_id: shaped.post_id,
        author: shaped.author,
        body: shaped.body,
        created_at: shaped.created_at,
        tags: shaped.tags,
        valid: shaped.valid,
        authoring_origin: shaped.authoring_origin,
        items: shaped
            .items
            .into_iter()
            .map(|i| DecodedMediaItem {
                blob_hash: i.blob_hash,
                media_type: i.media_type,
                size_bytes: i.size_bytes,
            })
            .collect(),
        references: shaped
            .references
            .into_iter()
            .map(|r| DecodedReference {
                ref_type: r.ref_type.into(),
                post_id: r.post_id,
                emoji: r.emoji,
            })
            .collect(),
        facets: shaped
            .facets
            .into_iter()
            .map(|f| DecodedFacet {
                byte_start: f.byte_start,
                byte_end: f.byte_end,
                feature_type: f.feature_type.into(),
                value: f.value,
            })
            .collect(),
        content_warning: shaped.content_warning,
    })
}

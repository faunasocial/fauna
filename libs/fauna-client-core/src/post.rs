//! Feed post building, signing, and decoding.
//!
//! Posts now travel as the embed-as-bytes wire shape per
//! `docs/goal/architecture/transport.md` § Embed-as-bytes for signed
//! payloads: `canonical_encode(EmbedAsBytes { envelope, bytes })`, where
//! `envelope` is 100 bytes (36-byte CID || 64-byte sig) and `bytes` is
//! the canonical-dag-cbor of the inner `Post`. Verification is
//! `BLAKE3(bytes) == cid.multihash` + Ed25519 over the CID — no encoder
//! in the security path.

use fauna_core::data::{
    Capability, Facet, FacetFeature, MediaItem, Post, PostBody, PostOrigin, Reference, Timestamp,
};
use fauna_core::encoding::{
    AuthoringOrigin, EmbedAsBytes, canonical_decode, canonical_encode, compute_post_id,
    decode_signed_bytes, sign_envelope, verify_authoring_envelope,
};
use fauna_core::identity::ActorKeypair;
pub use fauna_core::room_post::RoomPostSeal;

use crate::ClientError;

/// The two things an archive import supplies that a live compose never does
/// (`docs/goal/behavior/archive-import.md` § Implementation status today —
/// the authoring entry point slice 3 adds beside the builders taking an
/// explicit `created_at` + origin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostAuthoring {
    pub created_at: Timestamp,
    pub origin: Option<PostOrigin>,
}

impl PostAuthoring {
    /// A live compose: stamped now, no origin.
    pub fn now() -> Self {
        Self {
            created_at: Timestamp::now(),
            origin: None,
        }
    }

    /// An archive re-authoring at its original instant.
    pub fn imported(created_at: Timestamp, platform: &str) -> Self {
        Self {
            created_at,
            origin: Some(PostOrigin {
                platform: platform.to_string(),
                url: None,
            }),
        }
    }
}

/// Build and sign a bare Post (PostBody::Text) and dag-cbor-encode it as
/// `EmbedAsBytes { envelope, bytes }`.
///
/// - `tags`: display-only tag facets (byte range set to 0)
/// - `reply_to`: optional 36-byte CID of the post being replied to
///   (Cid = v1 + dag-cbor + blake3-256 + 32-byte digest)
///
/// Returns dag-cbor-encoded bytes ready to POST to `/api/v1/feeds/local/posts`.
pub fn build_post(
    kp: &ActorKeypair,
    body: &str,
    tags: &[String],
    reply_to: Option<[u8; 36]>,
) -> Result<Vec<u8>, ClientError> {
    build_post_at(kp, body, tags, reply_to, &PostAuthoring::now())
}

/// [`build_post`], but with a caller-supplied `created_at` + `origin` — the
/// authoring entry point slice 3 adds beside the builders
/// (`docs/goal/behavior/archive-import.md` § Implementation status today).
pub fn build_post_at(
    kp: &ActorKeypair,
    body: &str,
    tags: &[String],
    reply_to: Option<[u8; 36]>,
    authoring: &PostAuthoring,
) -> Result<Vec<u8>, ClientError> {
    let facets = tags_to_facets(tags);
    let references = reply_to_references(reply_to);

    let post = Post {
        author: kp.actor_id(),
        created_at: authoring.created_at,
        body: PostBody::Text {
            content: body.to_string(),
            facets,
        },
        references,
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: authoring.origin.clone(),
    };

    encode_signed_post(kp, &post)
}

/// Build and sign a video Post (`PostBody::Video`) and dag-cbor-encode it as
/// `EmbedAsBytes`.
///
/// - `manifest`: the HLS manifest's content hash — the video's stable identity
/// - `segments`: the transcoded HLS segments, in playback order per resolution
///   (callers upload the bytes and resolve hashes before calling)
/// - `thumbnail`: the poster frame's content hash
///
/// A video post carries no text of its own, matching `PostBody::Video`'s shape.
pub fn build_video_post(
    kp: &ActorKeypair,
    manifest: fauna_core::data::ContentHash,
    segments: Vec<fauna_core::data::VideoSegment>,
    thumbnail: fauna_core::data::ContentHash,
    duration_ms: u64,
    aspect_ratio: (u16, u16),
) -> Result<Vec<u8>, ClientError> {
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Video {
            manifest,
            segments,
            thumbnail,
            duration_ms,
            aspect_ratio,
            anchors: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    encode_signed_post(kp, &post)
}

/// Build and sign a Post with attached media (PostBody::TextWithMedia)
/// and dag-cbor-encode it as `EmbedAsBytes`.
///
/// - `media_items`: pre-built `MediaItem` values (callers resolve hashes before calling)
/// - `tags`: display-only tag facets
/// - `reply_to`: optional 36-byte CID of the post being replied to
pub fn build_post_with_media(
    kp: &ActorKeypair,
    body: &str,
    media_items: Vec<MediaItem>,
    tags: &[String],
    reply_to: Option<[u8; 36]>,
) -> Result<Vec<u8>, ClientError> {
    build_post_with_media_at(kp, body, media_items, tags, reply_to, &PostAuthoring::now())
}

/// [`build_post_with_media`], but with a caller-supplied `created_at` +
/// `origin` — the authoring entry point slice 3 adds beside the builders
/// (`docs/goal/behavior/archive-import.md` § Implementation status today).
pub fn build_post_with_media_at(
    kp: &ActorKeypair,
    body: &str,
    media_items: Vec<MediaItem>,
    tags: &[String],
    reply_to: Option<[u8; 36]>,
    authoring: &PostAuthoring,
) -> Result<Vec<u8>, ClientError> {
    let facets = tags_to_facets(tags);
    let references = reply_to_references(reply_to);

    let post = Post {
        author: kp.actor_id(),
        created_at: authoring.created_at,
        body: PostBody::TextWithMedia {
            content: body.to_string(),
            facets,
            items: media_items,
        },
        references,
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: authoring.origin.clone(),
    };

    encode_signed_post(kp, &post)
}

/// The output of [`build_gated_post`]: the signed post wire bytes plus the
/// sealed full-body blob the caller uploads to the nest blob store (the
/// post's `GatedInfo.encrypted_ref` is its BLAKE3).
pub struct GatedPostBuild {
    /// `canonical_encode(EmbedAsBytes)` — the `fauna.posts.create` body.
    pub post_bytes: Vec<u8>,
    /// `encrypt_content(derive_post_key(period_key, seal_id), full PostBody)` —
    /// upload verbatim; its BLAKE3 is the post's `encrypted_ref`.
    pub encrypted_blob: Vec<u8>,
    /// BLAKE3 of `encrypted_blob` — the `GatedInfo.encrypted_ref` stamped in
    /// the signed post; the blob-upload reply must echo it (content-addressed
    /// store).
    pub encrypted_ref: [u8; 32],
    /// The random per-post seal id carried in `GatedInfo.seal_id`.
    pub seal_id: [u8; 32],
}

/// Mint a fresh per-post `seal_id` — the creator-minted random 32-byte derive
/// input `GatedInfo.seal_id` carries (`ui/feed.md` § Encryption at rest: the
/// record's own CID is circular at creation and a plaintext-body hash would be
/// a content-confirmation oracle).
///
/// Public because a compose that **attaches media** must know the id *before*
/// the body is built: each attachment seals under
/// `derive_post_key(period_key, seal_id)` and must be uploaded (so its hash is
/// known) before it can go into the body this id then seals. That ordering is
/// why `build_gated_post_at` takes the id rather than minting one
/// (`ui/media.md` § Encryption at rest — one per-post key seals body and
/// attachments alike).
pub fn mint_seal_id() -> Result<[u8; 32], ClientError> {
    let mut seal_id = [0u8; 32];
    getrandom::fill(&mut seal_id).map_err(|e| ClientError(format!("seal_id entropy: {e}")))?;
    Ok(seal_id)
}

/// Build and sign a tier-**gated** Post (`ui/feed.md` § Encryption at rest —
/// the shared-Rust seal helper the client gate-to-tier compose flow rides;
/// the fauna-ffi cabi wraps it for the e2e harness).
///
/// `preview` becomes the public `body` (plaintext-by-design teaser); the full
/// `PostBody::Text { content: full_text }` is canonical-dag-cbor-encoded and
/// sealed with `encrypt_content` under `derive_post_key(period_key, seal_id)`
/// where `seal_id` is a fresh random 32-byte per-post derive input (the record
/// CID is circular at creation — see `GatedInfo::seal_id`). `key_blob_ref` is
/// the BLAKE3 of the tier's live `KeyBlob` (Pillar-1 machinery), telling
/// subscribers where the period key is wrapped for them.
pub fn build_gated_post(
    kp: &ActorKeypair,
    preview: &str,
    full_text: &str,
    tier: &str,
    tier_rank: u32,
    key_blob_ref: [u8; 32],
    period_key: &[u8; 32],
) -> Result<GatedPostBuild, ClientError> {
    let seal_id = mint_seal_id()?;

    let full_body = PostBody::Text {
        content: full_text.to_string(),
        facets: vec![],
    };
    build_gated_post_at(
        kp,
        preview,
        full_body,
        tier,
        tier_rank,
        key_blob_ref,
        period_key,
        seal_id,
        &PostAuthoring::now(),
    )
}

/// [`build_gated_post`], but taking the FULL [`PostBody`] (so a gated import
/// can carry media items) and a caller-supplied `seal_id` and `created_at` +
/// `origin` — the authoring entry point slice 3 adds beside the builders
/// (`docs/goal/behavior/archive-import.md` § Implementation status today).
/// The media the caller sealed beforehand under
/// `derive_post_key(period_key, seal_id)` must open with `full_body`.
#[allow(clippy::too_many_arguments)]
pub fn build_gated_post_at(
    kp: &ActorKeypair,
    preview: &str,
    full_body: PostBody,
    tier: &str,
    tier_rank: u32,
    key_blob_ref: [u8; 32],
    period_key: &[u8; 32],
    seal_id: [u8; 32],
    authoring: &PostAuthoring,
) -> Result<GatedPostBuild, ClientError> {
    use fauna_core::data::ContentHash;
    use fauna_core::subscription::types::KeyAccess;

    seal_and_sign_gated_post(
        kp,
        preview,
        full_body,
        vec![],
        KeyAccess::Broadcast {
            key_blob_ref: ContentHash::from_digest_raw(key_blob_ref),
        },
        tier,
        tier_rank,
        period_key,
        seal_id,
        authoring,
    )
}

/// Build and sign a **room-restricted** Post: an ordinary post of its author
/// whose body only the floor members of room `room` (its 32-byte channel id)
/// open (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*).
///
/// The twin of [`build_gated_post_at`], differing only where the ruling says:
/// the arm is [`KeyAccess::Room`](fauna_core::subscription::types::KeyAccess::Room)
/// naming the room (and, for a community room, the generation), and the tier
/// is the reserved constant [`ROOM_POST_TIER`](fauna_core::subscription::ROOM_POST_TIER)
/// at rank [`ROOM_POST_TIER_RANK`](fauna_core::subscription::ROOM_POST_TIER_RANK)
/// — never a tier the author owns. Everything else is the Posts row's own
/// seal: the full body under `derive_post_key(base_key, seal_id)` +
/// `encrypt_content`, the same `seal_id` convention, the same plaintext
/// `attachment_refs` so a nest pins the media.
///
/// `base_key` is the class's base, which the caller resolves because only its
/// conversations plane holds the room's keys: the MLS epoch secret for
/// [`RoomPostSeal::EndToEnd`], or `room_post_base_key(gen_key)` for
/// [`RoomPostSeal::Community`]. Media the caller attaches must already be
/// sealed under `derive_post_key(base_key, seal_id)` (as
/// `RestrictedPostAudience::Group`, `AudienceClass::GroupRestrictedPost`) and
/// named by `full_body`.
///
/// Nothing here travels to the room: the post goes to its author's own nest
/// like every post, and no post ever rides `channel.send`.
#[allow(clippy::too_many_arguments)]
pub fn build_room_post_at(
    kp: &ActorKeypair,
    preview: &str,
    full_body: PostBody,
    room: [u8; 32],
    seal: RoomPostSeal,
    base_key: &[u8; 32],
    seal_id: [u8; 32],
    authoring: &PostAuthoring,
) -> Result<GatedPostBuild, ClientError> {
    use fauna_core::subscription::{ROOM_POST_TIER, ROOM_POST_TIER_RANK};

    seal_and_sign_gated_post(
        kp,
        preview,
        full_body,
        vec![],
        room_arm(room, seal),
        ROOM_POST_TIER,
        ROOM_POST_TIER_RANK,
        base_key,
        seal_id,
        authoring,
    )
}

/// The room arm naming `room` under `seal` — one home, so a room post and a
/// sealed reply to one can never spell the arm differently.
fn room_arm(room: [u8; 32], seal: RoomPostSeal) -> fauna_core::subscription::types::KeyAccess {
    use fauna_core::subscription::types::{KeyAccess, MlsGroupId};

    let (epoch, generation) = match seal {
        RoomPostSeal::EndToEnd { epoch } => (epoch, None),
        // A community room has no epoch; the generation says which key.
        RoomPostSeal::Community { generation } => (0, Some(generation)),
    };
    KeyAccess::Room {
        group_id: MlsGroupId(room.to_vec()),
        epoch,
        generation,
    }
}

/// The audience a **sealed** reply or quote is authored under — the arm of the
/// restricted post it answers, which this device can author under
/// (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*, (d)).
/// The key material is the caller's to resolve, exactly as for
/// [`build_gated_post_at`] and [`build_room_post_at`].
pub enum SealedAudience<'a> {
    /// The author's own tier, under its current period key.
    Tier {
        tier: &'a str,
        tier_rank: u32,
        key_blob_ref: [u8; 32],
        period_key: &'a [u8; 32],
    },
    /// A room this device holds a seat on, under the class's base key.
    Room {
        room: [u8; 32],
        seal: RoomPostSeal,
        base_key: &'a [u8; 32],
    },
}

/// Build and sign a reply to, or quote of, a restricted post **sealed to that
/// post's own audience** — the only way the user's words go out under a
/// restricted target without an explicit public answer
/// ([`build_referencing_post`] is the public builder and never seals).
///
/// The words are the sealed full body. The public `body` is **empty**: a reply
/// announces nothing, and any text there would be the replier's words in the
/// clear. The reference rides the plaintext envelope — `references` is on the
/// plaintext floor — so the post says what it answers and nothing of what it
/// says. Strict on a malformed target, as [`build_referencing_post`] is.
pub fn build_sealed_referencing_post(
    kp: &ActorKeypair,
    body: &str,
    target: [u8; 36],
    kind: ReferenceKind,
    audience: SealedAudience<'_>,
    seal_id: [u8; 32],
) -> Result<GatedPostBuild, ClientError> {
    use fauna_core::data::ContentHash;
    use fauna_core::subscription::types::KeyAccess;
    use fauna_core::subscription::{ROOM_POST_TIER, ROOM_POST_TIER_RANK};

    let cid = fauna_cbor::Cid::from_bytes(target)
        .map_err(|e| ClientError(format!("malformed target post id: {e}")))?;
    let (key_access, tier, tier_rank, base_key) = match audience {
        SealedAudience::Tier {
            tier,
            tier_rank,
            key_blob_ref,
            period_key,
        } => (
            KeyAccess::Broadcast {
                key_blob_ref: ContentHash::from_digest_raw(key_blob_ref),
            },
            tier,
            tier_rank,
            period_key,
        ),
        SealedAudience::Room {
            room,
            seal,
            base_key,
        } => (
            room_arm(room, seal),
            ROOM_POST_TIER,
            ROOM_POST_TIER_RANK,
            base_key,
        ),
    };
    seal_and_sign_gated_post(
        kp,
        "",
        PostBody::Text {
            content: body.to_string(),
            facets: vec![],
        },
        vec![kind.reference(cid)],
        key_access,
        tier,
        tier_rank,
        base_key,
        seal_id,
        &PostAuthoring::now(),
    )
}

/// The one seal-and-sign path every gated post takes, whatever reaches its
/// readers the base key: `full_body` sealed under
/// `derive_post_key(base_key, seal_id)` + `encrypt_content`, its media named in
/// plaintext for the nest's pin, the preview as the public body, and
/// `references` on the plaintext envelope (empty for every composer post; a
/// sealed reply's one reference).
#[allow(clippy::too_many_arguments)]
fn seal_and_sign_gated_post(
    kp: &ActorKeypair,
    preview: &str,
    full_body: PostBody,
    references: Vec<Reference>,
    key_access: fauna_core::subscription::types::KeyAccess,
    tier: &str,
    tier_rank: u32,
    base_key: &[u8; 32],
    seal_id: [u8; 32],
    authoring: &PostAuthoring,
) -> Result<GatedPostBuild, ClientError> {
    use fauna_core::data::ContentHash;
    use fauna_core::subscription::crypto::{derive_post_key, encrypt_content};
    use fauna_core::subscription::types::GatedInfo;

    let plain =
        canonical_encode(&full_body).map_err(|e| ClientError(format!("encode full body: {e}")))?;
    let key = derive_post_key(base_key, &ContentHash::from_digest_raw(seal_id));
    let encrypted_blob = encrypt_content(&key, &plain);
    let encrypted_ref = ContentHash::from_digest_raw(*blake3::hash(&encrypted_blob).as_bytes());

    let post = Post {
        author: kp.actor_id(),
        created_at: authoring.created_at,
        body: PostBody::Text {
            content: preview.to_string(),
            facets: vec![],
        },
        references,
        expires_at: None,
        gated: Some(GatedInfo {
            encrypted_ref,
            key_access,
            tier: tier.to_string(),
            tier_rank,
            seal_id: ContentHash::from_digest_raw(seal_id),
            // The sealed body's own media list, in plaintext, so the nest can
            // pin blobs it cannot otherwise see (`GatedInfo::attachment_refs`;
            // floor ruling 2026-09-08). Derived here rather than passed in:
            // the body IS the authority on what was sealed with it.
            attachment_refs: full_body.blob_refs(),
        }),
        content_warning: None,
        origin: authoring.origin.clone(),
    };
    let post_bytes = encode_signed_post(kp, &post)?;
    Ok(GatedPostBuild {
        post_bytes,
        encrypted_blob,
        encrypted_ref: encrypted_ref.digest(),
        seal_id,
    })
}

fn encode_signed_post(kp: &ActorKeypair, post: &Post) -> Result<Vec<u8>, ClientError> {
    let (bytes, env) =
        sign_envelope(kp, post).map_err(|e| ClientError(format!("sign post: {e}")))?;
    let wire = EmbedAsBytes::from_signed(bytes, env);
    canonical_encode(&wire).map_err(|e| ClientError(format!("encode post: {e}")))
}

/// Decode an `EmbedAsBytes`-wrapped `Post` and verify its signature.
///
/// Returns `(Post, origin)`, where `origin` is `Some(_)` exactly when
/// verification **passed** — so `origin.is_some()` is the old `is_valid` bool,
/// and the variant additionally says *how* it passed. Decoding errors are
/// propagated; signature failure is `None` rather than an error.
///
/// A post authored by an external app through the full-PDS bridge is signed by
/// the account's server-held authoring sub-key, not its identity key, with the
/// identity-signed delegation cert riding in the wire's `signer_auth`
/// (`atproto-pds-full.md` D10). Verification therefore accepts *either* the
/// author's own signature ([`AuthoringOrigin::Direct`]) or a valid delegated one
/// ([`AuthoringOrigin::Delegated`]) via [`verify_authoring_envelope`]; an
/// absent-or-invalid cert yields `None`, so no client ever renders a delegated
/// post as verified without the chain (fail-closed by construction).
///
/// **Returning the origin rather than a bool is the D10 audit surface**
/// (`atproto-pds-full.md` § D10 → *Audit*, ratified 2026-07-29): the signed
/// bytes *are* the log, and this is the one shared read face through which every
/// app learns that an external app authored a post as the account. A `(Post,
/// bool)` return structurally could not carry that distinction.
pub fn decode_post(data: &[u8]) -> Result<(Post, Option<AuthoringOrigin>), ClientError> {
    let wire: EmbedAsBytes =
        canonical_decode(data).map_err(|e| ClientError(format!("decode embed-as-bytes: {e}")))?;
    let signer_auth = wire.signer_auth.clone();
    let (inner_bytes, env) = wire
        .into_signed()
        .map_err(|e| ClientError(format!("split envelope: {e}")))?;
    // Inner bytes are canonical dag-cbor (sign-over-CID). Verification is
    // hash + signature only; structural decode runs only after verify.
    let post: Post =
        decode_signed_bytes(&inner_bytes).map_err(|e| ClientError(format!("decode post: {e}")))?;
    let origin = verify_authoring_envelope(
        &post,
        &inner_bytes,
        &env,
        signer_auth.as_deref(),
        &Capability::Post,
        post.created_at,
    )
    .ok();
    Ok((post, origin))
}

/// The id a nest files these wire bytes under: hex `blake3` over the exact
/// signed-envelope bytes it was handed (`ingest_post_core`, and every other
/// nest-side `store_post` writer) — NOT [`compute_post_id`] over the decoded
/// value.
pub fn wire_post_id(data: &[u8]) -> String {
    hex::encode(fauna_core::encoding::content_hash(data).digest())
}

/// [`decode_post`] for a body fetched **by id**: verification additionally
/// requires the bytes to be the post that was asked for,
/// `wire_post_id(data) == requested_post_id`.
///
/// A signature proves only *who* wrote a body, not *which* post it is, so a
/// hostile or lured nest answering `posts.get(X)` with some other genuinely
/// signed post `Y` would otherwise render `Y` as `X`, verified — a context
/// substitution no signature check catches. A mismatch folds into the same
/// `None` a signature failure yields: the post still decodes and renders,
/// badged unverified (`security.md` § App display of unverified content), and no
/// caller can derive an authoring-origin claim from it.
pub fn decode_post_fetched_as(
    requested_post_id: &str,
    data: &[u8],
) -> Result<(Post, Option<AuthoringOrigin>), ClientError> {
    let (post, origin) = decode_post(data)?;
    let bound = wire_post_id(data).eq_ignore_ascii_case(requested_post_id);
    Ok((post, origin.filter(|_| bound)))
}

/// Decode a body fetched **by id** that is a **bare** canonical `Post` — the shape a
/// nest stores a bridge-translated post in (Bluesky, ActivityPub and nostr ingest
/// file `canonical_encode(&post)` under `blake3` of those bytes), as against the
/// signed envelope [`decode_post_fetched_as`] reads. `None` unless the bytes are a
/// bare `Post` **and** hash to the requested id — the same binding the signed path
/// makes, so a nest cannot substitute another body under this id.
///
/// There is no envelope, so there is nothing for this client to verify: a caller
/// renders the post `Unchecked` (no badge — the protocol badge carries its trust
/// class), never `Failed` (`security.md` § App display of unverified content). The
/// accepted shapes mirror [`Post::decode_resolved_bytes`], the nest's own reader.
pub fn decode_bare_post_fetched_as(requested_post_id: &str, data: &[u8]) -> Option<Post> {
    if !wire_post_id(data).eq_ignore_ascii_case(requested_post_id) {
        return None;
    }
    fauna_core::encoding::canonical_decode::<Post>(data).ok()
}

/// Compute the hex-encoded post ID for a decoded post.
///
/// Returns the 72-character lowercase hex of the 36-byte CID (full prefix
/// + digest). Returns an empty string on failure.
///
/// Note: `Cid::to_base32` is the spec-canonical user-facing form (Layer 2
/// onwards). `post_id_hex` is kept for FFI consumers that pre-existed the
/// base32 migration; new callers should prefer `compute_post_id(...)`
/// + `Cid::to_base32` or the raw 36 bytes via `Cid::as_bytes`.
pub fn post_id_hex(post: &Post) -> String {
    compute_post_id(post)
        .map(|id| fauna_core::format::hex_full(id.as_bytes()))
        .unwrap_or_default()
}

/// A decoded media item, platform-neutral (`fauna-ffi`'s `DecodedMediaItem`
/// and `fauna-wasm`'s inline JSON object are both last-mile marshalings of
/// this shape).
pub struct DecodedMediaItemShape {
    pub blob_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
}

/// A decoded post reference (reply/repost/quote/react/upvote/downvote),
/// platform-neutral.
pub struct DecodedReferenceShape {
    pub ref_type: &'static str,
    pub post_id: String,
    pub emoji: Option<String>,
}

/// A decoded text facet (mention/link/tag span), platform-neutral.
pub struct DecodedFacetShape {
    pub byte_start: u32,
    pub byte_end: u32,
    pub feature_type: &'static str,
    pub value: String,
}

/// A fully-shaped decoded post — the platform-neutral half of
/// `fauna-ffi::decode_post_full`/`fauna-wasm::decode_post_inner`, which
/// independently re-derived this exact shaping until `decode_post_full`'s own
/// doc comment ("Mirrors the WASM `decode_post_inner`") became the only thing
/// keeping the two in sync. Each binding crate keeps its own last-mile
/// marshaling (a `uniffi::Record` vs. a `serde_json::Value`) and decides which
/// fields to expose — `fauna-wasm`'s JSON has never included `facets`/
/// `content_warning`; moving the shaping here doesn't change that.
pub struct DecodedPostShape {
    pub post_id: String,
    pub author: String,
    pub body: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub valid: bool,
    pub authoring_origin: fauna_core::render::AuthoringOriginStatus,
    pub items: Vec<DecodedMediaItemShape>,
    pub references: Vec<DecodedReferenceShape>,
    pub facets: Vec<DecodedFacetShape>,
    pub content_warning: Option<String>,
}

/// Decode a dag-cbor-encoded Post into [`DecodedPostShape`] — the shared body
/// + tags + facets + items + references extraction both binding crates need.
pub fn decode_post_shaped(data: &[u8]) -> Result<DecodedPostShape, ClientError> {
    let (post, origin) = decode_post(data)?;
    let valid = origin.is_some();
    let authoring_origin = fauna_core::render::AuthoringOriginStatus::from_origin(origin.as_ref());
    let post_id = post_id_hex(&post);

    let (body_text, tags, facets) = match &post.body {
        PostBody::Text { content, facets }
        | PostBody::TextWithMedia {
            content, facets, ..
        } => {
            let tags: Vec<String> = facets
                .iter()
                .filter_map(|f| {
                    if let FacetFeature::Tag { name } = &f.feature {
                        Some(name.clone())
                    } else {
                        None
                    }
                })
                .collect();
            // A feature a newer build wrote gets no decoded facet: its range
            // renders as plain text.
            let decoded_facets: Vec<DecodedFacetShape> = facets
                .iter()
                .filter_map(|f| {
                    let (feature_type, value) = match &f.feature {
                        FacetFeature::Mention { actor_id } => ("mention", hex::encode(actor_id.0)),
                        FacetFeature::Link { uri } => ("link", uri.clone()),
                        FacetFeature::Tag { name } => ("tag", name.clone()),
                        FacetFeature::Unknown(_) => return None,
                    };
                    Some(DecodedFacetShape {
                        byte_start: f.byte_start,
                        byte_end: f.byte_end,
                        feature_type,
                        value,
                    })
                })
                .collect();
            (content.clone(), tags, decoded_facets)
        }
        _ => (String::new(), vec![], vec![]),
    };

    let items: Vec<DecodedMediaItemShape> = match &post.body {
        PostBody::TextWithMedia { items, .. } | PostBody::Media { items, .. } => items
            .iter()
            .map(|item| DecodedMediaItemShape {
                blob_hash: hex::encode(item.blob_hash.digest()),
                media_type: item.media_type.clone(),
                size_bytes: item.size_bytes,
            })
            .collect(),
        _ => vec![],
    };

    // A reference a newer build wrote is ignored: no decoded reference.
    let references: Vec<DecodedReferenceShape> = post
        .references
        .iter()
        .filter_map(|r| {
            Some(match r {
                Reference::Reply { post_id } => DecodedReferenceShape {
                    ref_type: "reply",
                    post_id: hex::encode(post_id.as_bytes()),
                    emoji: None,
                },
                Reference::Repost { post_id } => DecodedReferenceShape {
                    ref_type: "repost",
                    post_id: hex::encode(post_id.as_bytes()),
                    emoji: None,
                },
                Reference::Quote { post_id } => DecodedReferenceShape {
                    ref_type: "quote",
                    post_id: hex::encode(post_id.as_bytes()),
                    emoji: None,
                },
                Reference::React { post_id, emoji } => DecodedReferenceShape {
                    ref_type: "react",
                    post_id: hex::encode(post_id.as_bytes()),
                    emoji: Some(emoji.clone()),
                },
                Reference::Upvote { post_id } => DecodedReferenceShape {
                    ref_type: "upvote",
                    post_id: hex::encode(post_id.as_bytes()),
                    emoji: None,
                },
                Reference::Downvote { post_id } => DecodedReferenceShape {
                    ref_type: "downvote",
                    post_id: hex::encode(post_id.as_bytes()),
                    emoji: None,
                },
                Reference::Unknown(_) => return None,
            })
        })
        .collect();

    Ok(DecodedPostShape {
        post_id,
        author: hex::encode(post.author.0),
        body: body_text,
        created_at: post.created_at.0 as i64,
        tags,
        valid,
        authoring_origin,
        items,
        references,
        facets,
        content_warning: post.content_warning.clone(),
    })
}

// ── Private helpers ───────────────────────────────────────────

fn tags_to_facets(tags: &[String]) -> Vec<Facet> {
    tags.iter()
        .map(|t| Facet {
            byte_start: 0,
            byte_end: 0,
            feature: FacetFeature::Tag { name: t.clone() },
        })
        .collect()
}

fn reply_to_references(reply_to: Option<[u8; 36]>) -> Vec<Reference> {
    match reply_to {
        Some(arr) => match fauna_cbor::Cid::from_bytes(arr) {
            Ok(cid) => vec![Reference::Reply { post_id: cid }],
            // Caller passed a malformed CID prefix; treat as no reply rather
            // than failing the whole build (matches the previous "skip
            // unknown reply hash" semantics).
            Err(_) => vec![],
        },
        None => vec![],
    }
}

/// Which interaction-bar reference a composed post carries — the three
/// `fauna.posts.interact` actions that are *composed*, not recorded.
///
/// `like`/`unlike` are absent by design: the nest records those against the
/// target directly, so they compose nothing. These three each mean "create a
/// new post that references the target", which is what moves the target's
/// `reply_count` / `repost_count` / `quote_count`
/// (`bins/fauna-nest/src/db/posts.rs::record_reference_engagements`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Reply,
    Repost,
    Quote,
}

impl ReferenceKind {
    /// The `fauna.posts.interact` action name this reference answers to —
    /// the string the nest's `interact_routes` validates and the apps' buttons
    /// already pass.
    pub fn action(&self) -> &'static str {
        match self {
            ReferenceKind::Reply => "reply",
            ReferenceKind::Repost => "repost",
            ReferenceKind::Quote => "quote",
        }
    }

    fn reference(&self, post_id: fauna_cbor::Cid) -> Reference {
        match self {
            ReferenceKind::Reply => Reference::Reply { post_id },
            ReferenceKind::Repost => Reference::Repost { post_id },
            ReferenceKind::Quote => Reference::Quote { post_id },
        }
    }
}

/// The audience class of the post a reference names, as the caller's own read
/// model reports it (`ui/feed.md` § Encryption at rest → *Three audience
/// classes*). [`build_referencing_post`] takes it as an argument — never a
/// default — so no composer can reference a post without saying what it is
/// referencing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferencedAudience {
    /// `gated: None` — the target is readable by anyone.
    Public,
    /// Audience-restricted (a subscriber tier) or room-restricted: the
    /// target's body is sealed to a smaller set of readers. Words under it are
    /// refused — the user was not told they would be public.
    Restricted,
    /// Restricted, **and the user confirmed, in the act, that this reply goes
    /// public** (`ui/feed.md` § Encryption at rest → *A reply, quote or repost
    /// of a restricted post*, ruling 5's confirmation; the dialog's
    /// `feed-reply-public-confirm`). The only way words pass the public
    /// builder under a restricted target. A caller states it only on a
    /// per-reply answer it just took — never remembered, never a default.
    RestrictedPublicConfirmed,
}

/// What [`build_referencing_post`] answers when asked to publish the user's
/// words, public, under a restricted post. A stable string, because it crosses
/// the FFI/wasm faces as the error text and an app leg maps it to its own
/// localized copy (`feed.reference_restricted`).
pub const REFERENCE_REFUSED_RESTRICTED: &str = "this post is restricted to a smaller audience, and a \
     reply or quote with text would be public — it was not sent";

/// Build and sign a post that **references** another post — the composer half
/// of the interaction bar's reply / repost / quote (`ui/feed.md` § Interaction
/// bar).
///
/// This is the piece whose absence made all three buttons inert on all seven
/// apps: `fauna.posts.interact`'s native arm only *returns target info* "so the
/// client can compose a post" (`bins/fauna-nest/src/interact_routes.rs`), and
/// nothing composed it. The target's counter moves when the post built here
/// lands, never on the interact call itself.
///
/// - `body`: the commentary. Empty is legitimate — a bare repost and a
///   commentary-less quote-repost both carry no text of their own.
/// - `target`: the 36-byte CID of the referenced post
///   (`Cid::from_digest_dag_cbor(digest).as_bytes()`).
///
/// **Strict on a malformed CID, unlike [`build_post`]'s lenient `reply_to`.**
/// That asymmetry is deliberate: `build_post` keeps its shipped
/// skip-unknown-reply-hash semantics for existing callers, but a *composer*
/// that silently dropped the reference would publish the user's reply as a
/// standalone top-level post — worse than failing, because the user cannot see
/// that it happened.
///
/// **The user's words never go public under a restricted post unless they
/// said so** (`ui/feed.md` § Encryption at rest → *A reply, quote or repost of
/// a restricted post*). The post built here is always public — `gated: None`;
/// the sealed arm is [`build_sealed_referencing_post`] — so a non-empty
/// `body` referencing a [`ReferencedAudience::Restricted`] target is refused
/// with [`REFERENCE_REFUSED_RESTRICTED`]: the user composed it in a room's or
/// a tier's context and it would reach every follower. Under
/// [`ReferencedAudience::RestrictedPublicConfirmed`] the words build public:
/// the caller took the user's explicit per-reply answer (ruling 5) and this
/// builder trusts that one value alone. A **wordless** reference (a repost, a
/// commentary-less quote) publishes nothing of the user's and names only what
/// the target's plaintext envelope already shows every follower, so it builds
/// for every class.
pub fn build_referencing_post(
    kp: &ActorKeypair,
    body: &str,
    tags: &[String],
    target: [u8; 36],
    target_audience: ReferencedAudience,
    kind: ReferenceKind,
) -> Result<Vec<u8>, ClientError> {
    let cid = fauna_cbor::Cid::from_bytes(target)
        .map_err(|e| ClientError(format!("malformed target post id: {e}")))?;
    if target_audience == ReferencedAudience::Restricted && !body.trim().is_empty() {
        return Err(ClientError(REFERENCE_REFUSED_RESTRICTED.to_string()));
    }

    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: body.to_string(),
            facets: tags_to_facets(tags),
        },
        references: vec![kind.reference(cid)],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    encode_signed_post(kp, &post)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{PostBody, Reference};

    fn test_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([7u8; 32])
    }

    fn target_cid() -> [u8; 36] {
        *fauna_cbor::Cid::from_digest_dag_cbor([9u8; 32]).as_bytes()
    }

    /// Each kind emits its own `Reference` variant over the same target — the
    /// one thing that distinguishes a reply from a repost from a quote on the
    /// wire, and what `record_reference_engagements` switches on to pick which
    /// counter to bump.
    #[test]
    fn a_referencing_post_carries_the_kind_it_was_asked_for() {
        let kp = test_keypair();
        let target = target_cid();
        for (kind, want_action) in [
            (ReferenceKind::Reply, "reply"),
            (ReferenceKind::Repost, "repost"),
            (ReferenceKind::Quote, "quote"),
        ] {
            assert_eq!(kind.action(), want_action);
            let bytes = build_referencing_post(
                &kp,
                "commentary",
                &[],
                target,
                ReferencedAudience::Public,
                kind,
            )
            .expect("build_referencing_post");
            let (post, _) = decode_post(&bytes).expect("decode_post");
            assert_eq!(post.references.len(), 1, "{kind:?}");
            let got = match (&post.references[0], kind) {
                (Reference::Reply { post_id }, ReferenceKind::Reply) => post_id,
                (Reference::Repost { post_id }, ReferenceKind::Repost) => post_id,
                (Reference::Quote { post_id }, ReferenceKind::Quote) => post_id,
                (other, _) => panic!("{kind:?} built the wrong reference: {other:?}"),
            };
            assert_eq!(got.as_bytes(), &target, "{kind:?} lost the target");
            assert!(post.is_reply() == matches!(kind, ReferenceKind::Reply));
        }
    }

    /// A bare repost and a commentary-less quote both carry no text — the
    /// composer must not treat that as an error the way the *composer box*
    /// does (`FeedManager::submit_post` rejects an empty top-level post).
    #[test]
    fn an_empty_body_is_legitimate_for_a_referencing_post() {
        let kp = test_keypair();
        let bytes = build_referencing_post(
            &kp,
            "",
            &[],
            target_cid(),
            ReferencedAudience::Public,
            ReferenceKind::Repost,
        )
        .expect("an empty repost body builds");
        let (post, _) = decode_post(&bytes).expect("decode_post");
        match post.body {
            PostBody::Text { content, .. } => assert_eq!(content, ""),
            other => panic!("expected Text body, got {other:?}"),
        }
        assert!(matches!(post.references[0], Reference::Repost { .. }));
    }

    /// The user's words never go public under a restricted post (`ui/feed.md`
    /// § Encryption at rest → *A reply, quote or repost of a restricted post*):
    /// the post built here is always `gated: None`, so text referencing a
    /// restricted target is refused with the stable reason — for a reply and
    /// for a quote alike.
    #[test]
    fn words_under_a_restricted_post_are_refused_never_built_public() {
        let kp = test_keypair();
        for kind in [ReferenceKind::Reply, ReferenceKind::Quote] {
            let err = build_referencing_post(
                &kp,
                "said in the room",
                &[],
                target_cid(),
                ReferencedAudience::Restricted,
                kind,
            )
            .expect_err("text under a restricted post must not build public");
            assert_eq!(err.0, REFERENCE_REFUSED_RESTRICTED, "{kind:?}");
        }
    }

    /// **Confirmed, the words build public** (`ui/feed.md` § Encryption at
    /// rest → ruling 5's confirmation): the one audience value under which
    /// text passes this builder for a restricted target — and the built post
    /// is the same public reference an unrestricted target gets, `gated:
    /// None`, the words in the clear, one reference. Paired with the refusal
    /// pin above: the gate is the audience value, nothing else.
    #[test]
    fn confirmed_words_under_a_restricted_post_build_public() {
        let kp = test_keypair();
        for kind in [ReferenceKind::Reply, ReferenceKind::Quote] {
            let bytes = build_referencing_post(
                &kp,
                "said, knowing it is public",
                &[],
                target_cid(),
                ReferencedAudience::RestrictedPublicConfirmed,
                kind,
            )
            .expect("confirmed words build public");
            let (post, _) = decode_post(&bytes).expect("decode_post");
            assert!(
                post.gated.is_none(),
                "{kind:?}: public, by the user's answer"
            );
            match post.body {
                PostBody::Text { ref content, .. } => {
                    assert_eq!(content, "said, knowing it is public", "{kind:?}");
                }
                ref other => panic!("{kind:?}: expected Text body, got {other:?}"),
            }
            assert_eq!(post.references.len(), 1, "{kind:?}");
        }
    }

    /// A wordless reference publishes nothing of the user's — a repost and a
    /// commentary-less quote of a restricted post stay public, naming only
    /// what the target's plaintext envelope already shows every follower.
    /// Whitespace is not words.
    #[test]
    fn a_wordless_reference_to_a_restricted_post_still_builds_public() {
        let kp = test_keypair();
        for (kind, body) in [
            (ReferenceKind::Repost, ""),
            (ReferenceKind::Quote, ""),
            (ReferenceKind::Quote, "  \n"),
        ] {
            let bytes = build_referencing_post(
                &kp,
                body,
                &[],
                target_cid(),
                ReferencedAudience::Restricted,
                kind,
            )
            .expect("a wordless reference builds");
            let (post, _) = decode_post(&bytes).expect("decode_post");
            assert!(post.gated.is_none(), "{kind:?}");
            assert_eq!(post.references.len(), 1, "{kind:?}");
        }
    }

    /// The strictness that separates this from [`build_post`]'s lenient
    /// `reply_to`: a malformed target must FAIL, never silently produce a
    /// reference-less post — that would publish the user's reply as a
    /// standalone top-level post, invisibly.
    #[test]
    fn a_malformed_target_fails_instead_of_dropping_the_reference() {
        let kp = test_keypair();
        let err = build_referencing_post(
            &kp,
            "hi",
            &[],
            [0u8; 36],
            ReferencedAudience::Public,
            ReferenceKind::Reply,
        )
        .expect_err("a malformed CID must not build");
        assert!(
            err.0.contains("malformed target post id"),
            "unexpected error: {}",
            err.0
        );

        // The lenient shipped path is deliberately unchanged: `build_post`
        // still skips an unparseable reply target rather than failing.
        let lenient =
            build_post(&kp, "hi", &[], Some([0u8; 36])).expect("build_post stays lenient");
        let (post, _) = decode_post(&lenient).expect("decode_post");
        assert!(post.references.is_empty());
    }

    #[test]
    fn roundtrip_build_and_decode() {
        let kp = test_keypair();
        let bytes = build_post(&kp, "hello world", &[], None).expect("build_post");
        let (post, origin) = decode_post(&bytes).expect("decode_post");

        assert_eq!(
            origin,
            Some(AuthoringOrigin::Direct),
            "a self-signed post is DIRECTLY authored, not delegated"
        );
        assert_eq!(post.author, kp.actor_id());
        match &post.body {
            PostBody::Text { content, .. } => assert_eq!(content, "hello world"),
            other => panic!("unexpected body variant: {other:?}"),
        }
    }

    #[test]
    fn build_with_reply_to_sets_reference() {
        let kp = test_keypair();
        let reply_cid = fauna_cbor::Cid::of_dag_cbor(b"reply-target-post");
        let reply_bytes = *reply_cid.as_bytes();
        let bytes = build_post(&kp, "reply text", &[], Some(reply_bytes)).expect("build_post");
        let (post, origin) = decode_post(&bytes).expect("decode_post");

        assert_eq!(origin, Some(AuthoringOrigin::Direct));
        assert_eq!(post.references.len(), 1);
        match &post.references[0] {
            Reference::Reply { post_id } => assert_eq!(post_id, &reply_cid),
            other => panic!("expected Reply reference, got {other:?}"),
        }
    }

    #[test]
    fn build_with_tags_creates_facets() {
        let kp = test_keypair();
        let tags = vec!["rust".to_string(), "fauna".to_string()];
        let bytes = build_post(&kp, "tagged post", &tags, None).expect("build_post");
        let (post, origin) = decode_post(&bytes).expect("decode_post");

        assert_eq!(origin, Some(AuthoringOrigin::Direct));
        match &post.body {
            PostBody::Text { facets, .. } => {
                assert_eq!(facets.len(), 2);
                match &facets[0].feature {
                    FacetFeature::Tag { name } => assert_eq!(name, "rust"),
                    other => panic!("expected Tag facet, got {other:?}"),
                }
                match &facets[1].feature {
                    FacetFeature::Tag { name } => assert_eq!(name, "fauna"),
                    other => panic!("expected Tag facet, got {other:?}"),
                }
            }
            other => panic!("unexpected body: {other:?}"),
        }
    }

    #[test]
    fn build_with_media_roundtrips() {
        let kp = test_keypair();
        let items = vec![MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([99u8; 32]),
            media_type: "image/png".to_string(),
            size_bytes: 1024,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        }];
        let bytes =
            build_post_with_media(&kp, "photo post", items.clone(), &[], None).expect("build");
        let (post, origin) = decode_post(&bytes).expect("decode");

        assert_eq!(origin, Some(AuthoringOrigin::Direct));
        match &post.body {
            PostBody::TextWithMedia {
                content,
                items: decoded_items,
                ..
            } => {
                assert_eq!(content, "photo post");
                assert_eq!(decoded_items.len(), 1);
                assert_eq!(decoded_items[0].blob_hash, items[0].blob_hash);
                assert_eq!(decoded_items[0].media_type, "image/png");
            }
            other => panic!("unexpected body: {other:?}"),
        }
    }

    /// Pins the shared shaping `decode_post_full` (fauna-ffi) and
    /// `decode_post_inner` (fauna-wasm) both delegate to — one round-trip
    /// through tags/facets/media/references exercises every field both
    /// binding crates marshal, so a regression here catches both at once.
    #[test]
    fn decode_post_shaped_carries_tags_facets_items_and_a_reference() {
        let kp = test_keypair();
        let items = vec![MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([42u8; 32]),
            media_type: "image/jpeg".to_string(),
            size_bytes: 2048,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        }];
        let tags = vec!["rust".to_string()];
        let bytes = build_post_with_media(&kp, "shaped post", items, &tags, None).expect("build");
        let shaped = decode_post_shaped(&bytes).expect("decode_post_shaped");

        assert!(shaped.valid);
        assert_eq!(shaped.author, hex::encode(kp.actor_id().0));
        assert!(!shaped.post_id.is_empty());
        assert_eq!(shaped.body, "shaped post");
        assert_eq!(shaped.tags, vec!["rust".to_string()]);
        assert_eq!(shaped.facets.len(), 1);
        assert_eq!(shaped.facets[0].feature_type, "tag");
        assert_eq!(shaped.facets[0].value, "rust");
        assert_eq!(shaped.items.len(), 1);
        assert_eq!(
            shaped.items[0].blob_hash,
            hex::encode([42u8; 32]),
            "blob_hash must be hex, not the raw digest"
        );
        assert_eq!(shaped.items[0].media_type, "image/jpeg");
        assert_eq!(shaped.items[0].size_bytes, 2048);
        assert_eq!(shaped.content_warning, None);

        let target = target_cid();
        let reply_bytes = build_referencing_post(
            &kp,
            "re: shaped",
            &[],
            target,
            ReferencedAudience::Public,
            ReferenceKind::Reply,
        )
        .expect("build_referencing_post");
        let reply_shaped = decode_post_shaped(&reply_bytes).expect("decode_post_shaped");
        assert_eq!(reply_shaped.references.len(), 1);
        assert_eq!(reply_shaped.references[0].ref_type, "reply");
        assert_eq!(reply_shaped.references[0].post_id, hex::encode(target));
        assert_eq!(reply_shaped.references[0].emoji, None);
    }

    /// A body fetched by id verifies only when it IS that id: a genuinely
    /// signed post served under another post's id decodes (it still renders)
    /// but carries no origin — the same fail-closed answer as a bad signature.
    #[test]
    fn decode_post_fetched_as_binds_the_body_to_the_requested_id() {
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let x = build_post(&kp, "post x", &[], None).unwrap();
        let y = build_post(&kp, "post y", &[], None).unwrap();
        let x_id = wire_post_id(&x);
        let text = |post: &Post| match &post.body {
            PostBody::Text { content, .. } => content.clone(),
            other => panic!("unexpected body {other:?}"),
        };

        let (post, origin) = decode_post_fetched_as(&x_id, &x).expect("decode x");
        assert_eq!(text(&post), "post x");
        assert_eq!(origin, Some(AuthoringOrigin::Direct));
        // The nest parses ids as hex, so either case names the same post.
        assert!(
            decode_post_fetched_as(&x_id.to_uppercase(), &x)
                .unwrap()
                .1
                .is_some()
        );

        let (post, origin) = decode_post_fetched_as(&x_id, &y).expect("y still decodes");
        assert_eq!(text(&post), "post y");
        assert_eq!(origin, None, "a substituted body must not verify");
    }

    /// A bridge-translated post rests as a bare canonical `Post` (no envelope):
    /// it decodes by id only when its bytes hash to the id asked for, and a signed
    /// envelope is never mistaken for one.
    #[test]
    fn decode_bare_post_fetched_as_binds_a_bridged_body_to_its_id() {
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let signed = build_post(&kp, "signed", &[], None).unwrap();
        let (bare_post, _) = decode_post(&signed).unwrap();
        let bare = fauna_core::encoding::canonical_encode(&bare_post).unwrap();
        let id = wire_post_id(&bare);

        assert!(
            decode_post(&bare).is_err(),
            "the signed reader refuses a bare body"
        );
        let post = decode_bare_post_fetched_as(&id, &bare).expect("a bare body decodes");
        assert_eq!(post, bare_post);
        assert!(decode_bare_post_fetched_as(&id.to_uppercase(), &bare).is_some());
        assert!(
            decode_bare_post_fetched_as(&wire_post_id(&signed), &bare).is_none(),
            "another id's body is refused"
        );
        assert!(
            decode_bare_post_fetched_as(&wire_post_id(&signed), &signed).is_none(),
            "a signed envelope is not a bare Post"
        );
    }

    #[test]
    fn decode_post_accepts_delegated_authoring_and_stays_fail_closed() {
        use fauna_core::data::DeviceAuthorization;

        // A post authored by `identity` but signed by a delegated sub-key,
        // with the identity-signed cert riding in `signer_auth` (D10). The
        // shared client read path must render it as verified.
        let identity = ActorKeypair::from_secret([7u8; 32]);
        let sub = ActorKeypair::from_secret([8u8; 32]);
        let (post, _) = decode_post(&build_post(&identity, "delegated hello", &[], None).unwrap())
            .expect("decode identity post");
        assert_eq!(post.author, identity.actor_id());

        let cert = {
            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key: sub.actor_id().0,
                capabilities: vec![Capability::Post],
                created_at: Timestamp(1),
                expires_at: None,
            };
            let (bytes, env) = sign_envelope(&identity, &da).expect("sign cert");
            EmbedAsBytes::from_signed(bytes, env)
        };

        let (bytes, env) = sign_envelope(&sub, &post).expect("sign post with sub-key");
        let wire = EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert);
        let body = canonical_encode(&wire).expect("encode delegated wire");
        let (decoded, origin) = decode_post(&body).expect("decode delegated post");
        // THE D10 AUDIT SURFACE'S ROOT ASSERTION (`atproto-pds-full.md` § D10 →
        // *Audit*): it is not enough that a delegated post reads as *verified* —
        // the read face must say it was DELEGATED, and name the sub-key that
        // signed it. A `(Post, bool)` return could carry the first half only,
        // which is precisely why the signature widened.
        assert_eq!(
            origin,
            Some(AuthoringOrigin::Delegated {
                device_key: sub.actor_id().0
            }),
            "a validly-delegated post must read as DELEGATED, naming the sub-key"
        );
        assert_eq!(decoded.author, identity.actor_id());

        // Fail-closed: the same sub-key signature with NO cert must read invalid.
        let (bytes, env) = sign_envelope(&sub, &post).expect("re-sign");
        let uncertified = EmbedAsBytes::from_signed(bytes, env);
        let body = canonical_encode(&uncertified).expect("encode uncertified");
        let (_, origin) = decode_post(&body).expect("decode uncertified");
        assert_eq!(
            origin, None,
            "a delegated post without its cert must read as invalid — and must \
             NOT fall back to reporting Direct, which would launder a delegated \
             post into an identity-authored one"
        );
    }

    /// An imported post is signed at its ORIGINAL instant with its origin on the
    /// envelope — `created_at` is client-supplied and stored verbatim (the
    /// backdating rule, `archive-import.md` § Implementation status today).
    #[test]
    fn an_imported_post_carries_its_original_instant_and_origin() {
        let kp = test_keypair();
        let authoring = PostAuthoring::imported(
            Timestamp(1_600_000_000_000_000),
            fauna_core::source::FACEBOOK,
        );
        let bytes = build_post_at(&kp, "Dobrý den", &[], None, &authoring).expect("build");
        let (post, origin) = decode_post(&bytes).expect("decode");
        assert_eq!(
            origin,
            Some(AuthoringOrigin::Direct),
            "signed by the owner, no delegation"
        );
        assert_eq!(post.created_at, Timestamp(1_600_000_000_000_000));
        assert_eq!(
            post.origin,
            Some(PostOrigin {
                platform: fauna_core::source::FACEBOOK.into(),
                url: None
            })
        );
        assert_eq!(post.source_token(), fauna_core::source::FACEBOOK);
    }

    /// A gated import seals a MEDIA body under the caller's seal id, so the
    /// media the caller sealed under `derive_post_key(period_key, seal_id)`
    /// beforehand opens with the body.
    #[test]
    fn a_gated_import_seals_the_full_body_under_the_supplied_seal_id() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
        let kp = test_keypair();
        let period_key = [3u8; 32];
        let seal_id = [4u8; 32];
        let full = PostBody::TextWithMedia {
            content: "At the park".into(),
            facets: vec![],
            items: vec![MediaItem {
                blob_hash: ContentHash::from_digest_raw([9u8; 32]),
                media_type: "image/jpeg".into(),
                size_bytes: 15,
                dimensions: None,
                thumbnail: None,
                ..Default::default()
            }],
        };
        let authoring = PostAuthoring::imported(
            Timestamp(1_609_999_000_000_000),
            fauna_core::source::FACEBOOK,
        );
        let build = build_gated_post_at(
            &kp,
            "At the park",
            full.clone(),
            "only-me",
            u32::MAX,
            [7u8; 32],
            &period_key,
            seal_id,
            &authoring,
        )
        .expect("build");
        let (post, _) = decode_post(&build.post_bytes).expect("decode");
        let gated = post.gated.expect("gated");
        assert_eq!(gated.seal_id, ContentHash::from_digest_raw(seal_id));
        assert_eq!(gated.tier, "only-me");
        assert_eq!(post.created_at, Timestamp(1_609_999_000_000_000));
        assert!(post.origin.is_some());
        let plain = decrypt_content(
            &derive_post_key(&period_key, &ContentHash::from_digest_raw(seal_id)),
            &build.encrypted_blob,
        )
        .expect("opens under the derived key");
        let body: PostBody = canonical_decode(&plain).unwrap();
        assert_eq!(body, full);
    }

    /// A room-restricted post addressed to a **community** room: the arm names
    /// the room and the generation, the tier is the reserved constant rather
    /// than any of the author's, and the body opens under the Posts row's own
    /// per-post key derived from the base the caller holds — never under the
    /// generation key itself (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the ruling*, rulings 2–4).
    #[test]
    fn a_community_room_post_names_its_room_and_generation_and_seals_under_the_base() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
        use fauna_core::subscription::types::{KeyAccess, MlsGroupId};
        let kp = test_keypair();
        let room = [0xC7u8; 32];
        let generation = [0x9Au8; 32];
        let base = [6u8; 32];
        let seal_id = [4u8; 32];
        let full = PostBody::TextWithMedia {
            content: "for the room".into(),
            facets: vec![],
            items: vec![MediaItem {
                blob_hash: ContentHash::from_digest_raw([9u8; 32]),
                media_type: "image/jpeg".into(),
                size_bytes: 15,
                dimensions: None,
                thumbnail: None,
                ..Default::default()
            }],
        };
        let build = build_room_post_at(
            &kp,
            "a post for the room",
            full.clone(),
            room,
            RoomPostSeal::Community { generation },
            &base,
            seal_id,
            &PostAuthoring::now(),
        )
        .expect("build");

        let (post, origin) = decode_post(&build.post_bytes).expect("decode");
        assert!(origin.is_some(), "the author's signature verifies");
        // The public body is the preview, never the sealed text.
        assert_eq!(post.body_text(), "a post for the room");
        let gated = post.gated.expect("a room post is gated");
        assert_eq!(
            gated.key_access,
            KeyAccess::Room {
                group_id: MlsGroupId(room.to_vec()),
                epoch: 0,
                generation: Some(generation),
            }
        );
        assert_eq!(gated.tier, fauna_core::subscription::ROOM_POST_TIER);
        assert_eq!(
            gated.tier_rank,
            fauna_core::subscription::ROOM_POST_TIER_RANK
        );
        assert_eq!(gated.seal_id, ContentHash::from_digest_raw(seal_id));
        assert_eq!(gated.attachment_refs, full.blob_refs());
        assert_eq!(
            gated.encrypted_ref,
            ContentHash::from_digest_raw(*blake3::hash(&build.encrypted_blob).as_bytes())
        );

        let per_post = derive_post_key(&base, &ContentHash::from_digest_raw(seal_id));
        let plain = decrypt_content(&per_post, &build.encrypted_blob).expect("opens");
        assert_eq!(canonical_decode::<PostBody>(&plain).unwrap(), full);
        assert!(
            decrypt_content(&base, &build.encrypted_blob).is_err(),
            "the base key alone must not open the body — the seal is per post"
        );
    }

    /// A **sealed reply** (`ui/feed.md` § Encryption at rest → *Ruling 5's
    /// build — the shape*, (a) + (b)): the words are in the sealed body and
    /// nowhere else, the public body is empty, the reference rides the
    /// plaintext envelope, and the arm is the target's own — a room's and a
    /// tier's alike.
    #[test]
    fn a_sealed_reply_keeps_its_words_sealed_and_its_reference_in_the_clear() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
        use fauna_core::subscription::types::KeyAccess;
        let kp = test_keypair();
        let base = [6u8; 32];
        let seal_id = [4u8; 32];
        let room = [0xC7u8; 32];
        for (class, audience) in [
            (
                "room",
                SealedAudience::Room {
                    room,
                    seal: RoomPostSeal::EndToEnd { epoch: 3 },
                    base_key: &base,
                },
            ),
            (
                "tier",
                SealedAudience::Tier {
                    tier: "patrons",
                    tier_rank: 2,
                    key_blob_ref: [8u8; 32],
                    period_key: &base,
                },
            ),
        ] {
            let build = build_sealed_referencing_post(
                &kp,
                "said in confidence",
                target_cid(),
                ReferenceKind::Reply,
                audience,
                seal_id,
            )
            .expect("build");

            let (post, _) = decode_post(&build.post_bytes).expect("decode");
            assert_eq!(post.body_text(), "", "{class}: no words in the clear");
            assert!(
                !build
                    .post_bytes
                    .windows(b"confidence".len())
                    .any(|w| w == b"confidence"),
                "{class}: the envelope must not carry the words anywhere"
            );
            match &post.references[..] {
                [Reference::Reply { post_id }] => assert_eq!(post_id.as_bytes(), &target_cid()),
                other => panic!("{class}: expected one Reply reference, got {other:?}"),
            }
            let gated = post.gated.expect("a sealed reply is gated");
            match (class, &gated.key_access) {
                ("room", KeyAccess::Room { .. }) => {
                    assert_eq!(gated.tier, fauna_core::subscription::ROOM_POST_TIER);
                }
                ("tier", KeyAccess::Broadcast { .. }) => assert_eq!(gated.tier, "patrons"),
                (_, other) => panic!("{class}: wrong arm {other:?}"),
            }

            let per_post = derive_post_key(&base, &ContentHash::from_digest_raw(seal_id));
            let plain = decrypt_content(&per_post, &build.encrypted_blob).expect("opens");
            match canonical_decode::<PostBody>(&plain).unwrap() {
                PostBody::Text { content, .. } => assert_eq!(content, "said in confidence"),
                other => panic!("{class}: unexpected sealed body {other:?}"),
            }
        }

        assert!(
            build_sealed_referencing_post(
                &kp,
                "hi",
                [0u8; 36],
                ReferenceKind::Reply,
                SealedAudience::Room {
                    room,
                    seal: RoomPostSeal::EndToEnd { epoch: 3 },
                    base_key: &base,
                },
                seal_id,
            )
            .is_err(),
            "a malformed target must fail, never drop the reference"
        );
    }

    /// An **end-to-end** room's arm is the arm as first designed: the epoch the
    /// base secret was exported at, and no generation — byte-identical to the
    /// shape every shipped app already decodes.
    #[test]
    fn an_end_to_end_room_post_names_its_epoch_and_carries_no_generation() {
        use fauna_core::subscription::types::{KeyAccess, MlsGroupId};
        let kp = test_keypair();
        let room = [0x11u8; 32];
        let build = build_room_post_at(
            &kp,
            "preview",
            PostBody::Text {
                content: "full".into(),
                facets: vec![],
            },
            room,
            RoomPostSeal::EndToEnd { epoch: 12 },
            &[2u8; 32],
            [3u8; 32],
            &PostAuthoring::now(),
        )
        .expect("build");
        let (post, _) = decode_post(&build.post_bytes).expect("decode");
        assert_eq!(
            post.gated.expect("gated").key_access,
            KeyAccess::Room {
                group_id: MlsGroupId(room.to_vec()),
                epoch: 12,
                generation: None,
            }
        );
    }

    /// The live builders are unchanged in shape: no origin, stamped now.
    #[test]
    fn the_live_builders_stamp_now_and_carry_no_origin() {
        let kp = test_keypair();
        let before = Timestamp::now();
        let (post, _) = decode_post(&build_post(&kp, "hi", &[], None).unwrap()).unwrap();
        assert!(post.created_at >= before);
        assert_eq!(post.origin, None);
    }
}

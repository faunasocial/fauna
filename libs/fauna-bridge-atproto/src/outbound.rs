//! Write-through outbound translation: Fauna Post -> ATProto record JSON.
//!
//! This module provides pure, synchronous translation from Fauna post content
//! into the JSON record format expected by `com.atproto.repo.createRecord` for
//! the `app.bsky.feed.post` collection.
//!
//! Media **bytes** never enter this module: fetching, re-hashing and storing a
//! blob is I/O, so the caller does it and hands back [`ResolvedImage`]s.
//! [`extract_projection_media`] is the other half of that contract — it says
//! which bytes matter and what they are, so the bridge still never has to
//! decode a Fauna post (`atproto-pds-bridge.md` § Where logic lives, "media
//! descriptor prep").

use fauna_core::data::{Facet, Post, PostBody, Profile, Reference};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;

/// The result of translating a Fauna post to an ATProto record.
pub struct OutboundRecord {
    /// The ATProto collection, always `"app.bsky.feed.post"` for feed posts.
    pub collection: String,
    /// The full record JSON suitable for `com.atproto.repo.createRecord`.
    pub record_json: Value,
}

/// Translate a Fauna post into an ATProto feed post record.
///
/// Returns `None` if the post should not be cross-posted (e.g. video, encrypted, DM).
///
/// # Arguments
///
/// * `text` — The plain-text content of the post.
/// * `facets` — Fauna rich-text facets (byte-range indexed).
/// * `reply` — the direct parent and thread root of a `Reference::Reply`,
///   resolved by the caller from its PostId→record map (the thread root is the
///   parent record's own `reply.root`, else the parent — `bridges.md`
///   § Cross-posting → *A post that references a Bluesky record*).
/// * `quote` — the record a `Reference::Quote` names, resolved the same way;
///   emitted as `app.bsky.embed.record`.
///
/// Typed inputs rather than loose URI/CID strings, so a half-resolved
/// reference is unrepresentable: a reply ref is whole or absent.
pub fn fauna_post_to_bsky_record(
    text: &str,
    facets: &[Facet],
    reply: Option<&ReplyRefs>,
    quote: Option<&QuoteRef>,
) -> Option<OutboundRecord> {
    let record = build_post_record(text, facets, &now_iso8601(), reply, quote, &[], None, &[]);

    Some(OutboundRecord {
        collection: "app.bsky.feed.post".to_string(),
        record_json: record,
    })
}

/// Build the `app.bsky.feed.post` record JSON — the single shared builder
/// behind both the write-through path ([`fauna_post_to_bsky_record`], which
/// stamps "now") and the deterministic projection path
/// ([`translate_post_for_projection`], which stamps the post's own
/// `created_at`).
///
/// Facet byte offsets index into the UTF-8 **bytes** of the text on both
/// sides — Fauna's `Facet.byte_start`/`byte_end` and Bluesky's
/// `index.byteStart`/`byteEnd` share the same semantics, so ranges cross
/// unchanged; facets pointing past the truncated text are dropped.
#[allow(clippy::too_many_arguments)]
fn build_post_record(
    text: &str,
    facets: &[Facet],
    created_at_iso: &str,
    reply: Option<&ReplyRefs>,
    quote: Option<&QuoteRef>,
    images: &[ResolvedImage],
    video: Option<&ResolvedVideo>,
    self_labels: &[String],
) -> Value {
    let final_text = truncate_for_lexicon(text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);

    // Determine the byte length of the final text for facet filtering
    let text_byte_len = final_text.len() as u32;

    // Translate facets, dropping any that reference byte positions beyond the truncated text
    let bsky_facets: Vec<Value> = facets
        .iter()
        .filter(|f| f.byte_start < text_byte_len && f.byte_end <= text_byte_len)
        .filter_map(translate_facet)
        .collect();

    let mut record = json!({
        "$type": "app.bsky.feed.post",
        "text": final_text,
        "createdAt": created_at_iso,
    });

    // Add facets if any
    if !bsky_facets.is_empty() {
        record["facets"] = Value::Array(bsky_facets);
    }

    // Add reply reference if provided
    if let Some(r) = reply {
        record["reply"] = json!({
            "parent": { "uri": r.parent_uri, "cid": r.parent_cid },
            "root": { "uri": r.root_uri, "cid": r.root_cid },
        });
    }

    // Quote and/or media → the record's single `embed` field. A post can be
    // both a quote and a media post in Fauna, but `app.bsky.feed.post.embed`
    // holds exactly one value, so that combination takes the lexicon's own
    // answer: `app.bsky.embed.recordWithMedia`, which nests the two.
    //
    // Images and video never collide: `PostBody::Video` carries no `items` and
    // the image-bearing variants carry no segments, so the two are mutually
    // exclusive at the type level. The `or_else` is therefore a total function
    // over inputs that cannot both be populated, not a precedence rule someone
    // should read a ranking into.
    let media_embed = build_video_embed(video).or_else(|| build_images_embed(images));
    let record_embed = quote.map(|q| {
        json!({
            "$type": "app.bsky.embed.record",
            "record": { "uri": q.uri, "cid": q.cid },
        })
    });
    let embed = match (record_embed, media_embed) {
        (Some(rec), Some(media)) => Some(json!({
            "$type": "app.bsky.embed.recordWithMedia",
            "record": rec,
            "media": media,
        })),
        (Some(rec), None) => Some(rec),
        (None, Some(media)) => Some(media),
        (None, None) => None,
    };
    // Absent, never null: dag-cbor would carry an explicit null as a present
    // field, and the lexicon's `embed` is optional.
    if let Some(embed) = embed {
        record["embed"] = embed;
    }

    // Self-labels: the mechanism is wired from day one but v1 callers pass
    // empty — no author-asserted structured category exists on `Post`, and
    // classifier verdicts must not be published as self-labels.
    if !self_labels.is_empty() {
        let values: Vec<Value> = self_labels.iter().map(|v| json!({ "val": v })).collect();
        record["labels"] = json!({
            "$type": "com.atproto.label.defs#selfLabels",
            "values": values,
        });
    }

    record
}

/// Truncate `text` to fit a lexicon string field's cap, appending " [...]" when
/// anything was dropped.
///
/// **A lexicon string cap is a PAIR, and both halves bind**: `maxGraphemes`
/// counts user-perceived characters, `maxLength` counts UTF-8 **bytes**, and
/// they are independent limits — a record violating either is refused. The two
/// coincide for ASCII, which is why capping graphemes alone looked sufficient
/// for two years and was not: one family-emoji cluster (👨‍👩‍👧‍👦) is a single
/// grapheme of 25 bytes, so 300 of them are 7500 bytes of `text` — inside the
/// 300-grapheme cap and 2.5× outside the 3000-byte one. That rendering reached
/// the repo and the firehose with nothing on the projection path checking it
/// (found 2026-08-03 by `projection_lexicon_ffi_test.go`, the first test to
/// validate the projection's *own* output rather than a caller's record).
///
/// Truncation never splits a grapheme cluster: clusters are accumulated whole
/// while both budgets hold. The " [...]" marker rides *inside* the budget — a
/// marker that pushed the result back over the cap would defeat the point — and
/// is dropped entirely when the cap is too small to hold even it.
pub fn truncate_for_lexicon(text: &str, max_graphemes: usize, max_bytes: usize) -> String {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    if graphemes.len() <= max_graphemes && text.len() <= max_bytes {
        return text.to_string();
    }

    const SUFFIX: &str = " [...]";
    let suffix_graphemes = SUFFIX.graphemes(true).count();
    let suffix_bytes = SUFFIX.len();
    let (with_suffix, grapheme_budget, byte_budget) =
        if max_graphemes >= suffix_graphemes && max_bytes >= suffix_bytes {
            (
                true,
                max_graphemes - suffix_graphemes,
                max_bytes - suffix_bytes,
            )
        } else {
            (false, max_graphemes, max_bytes)
        };

    let mut out = String::new();
    for grapheme in graphemes.into_iter().take(grapheme_budget) {
        // Whole clusters only: a byte-indexed cut here would split a family
        // emoji into a lone ZWJ tail, or worse, mid-code-point into bytes that
        // are not UTF-8 at all.
        if out.len() + grapheme.len() > byte_budget {
            break;
        }
        out.push_str(grapheme);
    }
    if with_suffix {
        out.push_str(SUFFIX);
    }
    out
}

/// Translate a single Fauna facet to an ATProto facet JSON value.
fn translate_facet(facet: &Facet) -> Option<Value> {
    use fauna_core::data::FacetFeature;

    let feature = match &facet.feature {
        FacetFeature::Mention { .. } => {
            // A mention is deliberately NOT emitted (drop the facet, keep the
            // display text). Encoding the mentioned user's raw 32-byte actor
            // pubkey as the `did` published a non-consenting user's signing key
            // to the permanent, world-readable atproto firehose — a cross-network
            // correlation the mentioned party never opted into (they need not have
            // linked Bluesky at all) — and the raw hex is a malformed DID no
            // Bluesky client can resolve, so it was not even a functional mention.
            // A real `#mention` may only be emitted once an identity-mapping layer
            // can resolve the mentioned user's *own, consented* Bluesky account DID.
            // (network-exposure.md § Rulings F2.)
            return None;
        }
        FacetFeature::Link { uri } => {
            json!({
                "$type": "app.bsky.richtext.facet#link",
                "uri": uri,
            })
        }
        FacetFeature::Tag { name } => {
            json!({
                "$type": "app.bsky.richtext.facet#tag",
                "tag": name,
            })
        }
        // A feature a newer build wrote: its range stays plain text.
        FacetFeature::Unknown(_) => return None,
    };

    Some(json!({
        "index": {
            "byteStart": facet.byte_start,
            "byteEnd": facet.byte_end,
        },
        "features": [feature],
    }))
}

/// Produce an ISO 8601 timestamp string for "now" in UTC.
///
/// Format: `YYYY-MM-DDTHH:MM:SS.sssZ`
fn now_iso8601() -> String {
    let micros = fauna_core::data::Timestamp::now().0;
    let total_secs = micros / 1_000_000;
    let millis = (micros / 1_000) % 1_000;

    // Break total_secs into calendar components (no leap second handling).
    let (year, month, day, hour, min, sec) = secs_to_datetime(total_secs);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, min, sec, millis
    )
}

/// Convert seconds since Unix epoch to (year, month, day, hour, minute, second).
///
/// This is a simplified civil-time conversion (no leap seconds) which is
/// sufficient for ATProto `createdAt` timestamps.
fn secs_to_datetime(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let day_secs = secs % 86400;
    let (year, month, day) = fauna_core::caltime::civil_from_days((secs / 86400) as i64);
    (
        year as u64,
        month as u64,
        day as u64,
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60,
    )
}

// ─── Deterministic host-direction projection (S3) ───────────────────────────
//
// The PDS bridge re-derives every user's atproto repo from nest state
// (`atproto-pds-bridge.md` § Where logic lives), so record keys and record
// bodies must be pure functions of the stored Fauna content. Everything below
// is synchronous and dependency-light — it compiles with
// `--no-default-features` and is what fauna-ffi exposes to the Go bridge.

/// The base32-sortable TID alphabet (atproto spec ordering).
pub const TID_ALPHABET: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// The literal rkey of the `app.bsky.actor.profile` record (atproto spec:
/// exactly one profile record per repo, keyed `self`).
pub const PROFILE_SELF_RKEY: &str = "self";

/// Deterministic TID for a projected post record.
///
/// 64-bit value = top bit forced 0 | 53-bit microseconds since Unix epoch
/// (from `Post.created_at`) | 10-bit disambiguator = the first 10 bits
/// (big-endian) of BLAKE3(PostId wire bytes). Encoded as the standard
/// 13-char base32-sortable TID string, MSB first.
///
/// Out-of-range instants are clamped (saturating) into the representable
/// 53-bit range: pre-epoch (negative) inputs become 0, instants beyond
/// 2^53−1 µs (~year 2255) become 2^53−1. `Timestamp` is unsigned micros so
/// negatives only arrive via foreign i64 inputs (the FFI boundary); the
/// clamp keeps the function total and the top bit provably 0.
pub fn deterministic_tid(created_at_micros: i64, post_id: &[u8]) -> String {
    const MAX_53: i64 = (1i64 << 53) - 1;
    let micros = created_at_micros.clamp(0, MAX_53) as u64;
    let hash = blake3::hash(post_id);
    let hb = hash.as_bytes();
    // First 10 bits of the digest, big-endian: all 8 bits of byte 0, then the
    // top 2 bits of byte 1.
    let disambiguator = ((hb[0] as u64) << 2) | ((hb[1] as u64) >> 6);
    let value = (micros << 10) | disambiguator; // < 2^63: top bit is 0
    encode_tid(value)
}

/// Encode a 64-bit value as the 13-char base32-sortable TID string, MSB
/// first (13 × 5 = 65 bit capacity; the leading character carries the top 4
/// bits).
fn encode_tid(value: u64) -> String {
    (0..13)
        .map(|i| TID_ALPHABET[((value >> (5 * (12 - i))) & 0x1F) as usize] as char)
        .collect()
}

/// AT URI + CID of a reply's direct parent and thread root, resolved by the
/// caller from its PostId→record mapping. All four fields are required — a
/// reply ref is only emitted when the whole chain is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRefs {
    pub parent_uri: String,
    pub parent_cid: String,
    pub root_uri: String,
    pub root_cid: String,
}

/// AT URI + CID of a quoted post, resolved by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRef {
    pub uri: String,
    pub cid: String,
}

/// Build the `app.bsky.embed.images` value for `images`, or `None` when there
/// are none.
///
/// Each entry is the data-model **blob** shape — `{$type, ref: {$link}, mimeType,
/// size}` — whose `ref` the dag-cbor encoder turns into a real CID link (tag 42),
/// not a string. `aspectRatio` is emitted only when the descriptor carried
/// dimensions; the lexicon makes it optional, and inventing one would assert a
/// shape we do not know.
/// The data-model **blob** value for an already-stored image — `{$type, ref:
/// {$link}, mimeType, size}`, whose `ref` the dag-cbor encoder turns into a
/// real CID link (tag 42), not a string.
///
/// Shared by every blob reference the projection emits: a post's
/// `app.bsky.embed.images` entries and the profile's `avatar`/`banner`. They
/// are the same lexicon type (`blob`), so they must stay the same bytes —
/// letting the two drift is how one of them ends up unfetchable.
fn build_blob_ref(img: &ResolvedImage) -> Value {
    json!({
        "$type": "blob",
        "ref": { "$link": img.blob_cid },
        "mimeType": img.mime,
        "size": img.size_bytes,
    })
}

fn build_images_embed(images: &[ResolvedImage]) -> Option<Value> {
    if images.is_empty() {
        return None;
    }
    let entries: Vec<Value> = images
        .iter()
        .take(MAX_EMBED_IMAGES)
        .map(|img| {
            let mut entry = json!({
                "alt": img.alt,
                "image": build_blob_ref(img),
            });
            if let (Some(width), Some(height)) = (img.width, img.height) {
                entry["aspectRatio"] = json!({ "width": width, "height": height });
            }
            entry
        })
        .collect();
    Some(json!({
        "$type": "app.bsky.embed.images",
        "images": entries,
    }))
}

/// One video the projection may attach to a record: the single mp4 blob the
/// caller assembled from one rendition's segments, already re-hashed and
/// stored.
///
/// The shape mirrors [`ResolvedImage`] deliberately — same blob-ref bytes, same
/// caller-does-the-I/O contract — but the *provenance* differs in a way worth
/// stating: an image blob is a Fauna blob republished verbatim, whereas this
/// one is bytes the bridge computed (concatenate one rendition's MPEG-TS
/// segments, remux to mp4 with stream copy). Nothing in Fauna is addressed by
/// its hash, which is why the dedup key rides on [`ProjectionVideo`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedVideo {
    /// The stored blob's ATProto CID, base32 (`bafkrei…`).
    pub blob_cid: String,
    /// Always `video/mp4` — the only type `app.bsky.embed.video` accepts, and
    /// what the remux produces. Carried rather than assumed so the blob ref and
    /// the stored blob's recorded type cannot drift apart.
    pub mime: String,
    /// The assembled blob's byte length, as stored.
    pub size_bytes: u64,
    /// The video's display aspect ratio, from `PostBody::Video.aspect_ratio`.
    /// Carried on the *post*, not measured from the bytes, so a rendition
    /// choice can never change it.
    pub aspect_width: u32,
    pub aspect_height: u32,
}

/// Build the `app.bsky.embed.video` embed.
///
/// `alt` and `captions` are deliberately absent rather than empty: Fauna's
/// video body carries no alt text and no caption tracks, and the lexicon marks
/// both optional, so emitting an empty string would assert the author supplied
/// a blank description. The thumbnail is absent because the lexicon has no
/// field for one — the AppView derives its own.
fn build_video_embed(video: Option<&ResolvedVideo>) -> Option<Value> {
    let video = video?;
    Some(json!({
        "$type": "app.bsky.embed.video",
        "video": {
            "$type": "blob",
            "ref": { "$link": video.blob_cid },
            "mimeType": video.mime,
            "size": video.size_bytes,
        },
        "aspectRatio": {
            "width": video.aspect_width,
            "height": video.aspect_height,
        },
    }))
}

/// One image the projection may attach to a record, already re-hashed and
/// stored by the caller.
///
/// `blob_cid` is the **ATProto** blob CID (CIDv1, raw codec, sha2-256
/// multihash of the bytes) — never the Fauna BLAKE3 CID the descriptor named.
/// The re-hash is the ratified cost in `atproto-pds-bridge.md` § Goal
/// ("blob re-hashing (BLAKE3/FastCDC → sha256-CID)"), and doing it caller-side
/// is what keeps this translator pure: the bytes never enter shared Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedImage {
    /// The stored blob's ATProto CID, base32 (`bafkrei…`).
    pub blob_cid: String,
    /// The blob's MIME type, as the `image/*` the descriptor declared.
    pub mime: String,
    /// The blob's byte length, as stored.
    pub size_bytes: u64,
    /// Pixel dimensions, when the descriptor carried them — `aspectRatio`.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Alt text; empty string when the post carried none (the lexicon's `alt`
    /// is required, so "no alt" is the empty string, never an absent field).
    pub alt: String,
}

/// The lexicon ceiling on `app.bsky.embed.images.images`.
pub const MAX_EMBED_IMAGES: usize = 4;

// The lexicon ceilings on the three string fields the projection renders. Each
// is a PAIR — `maxGraphemes` and the UTF-8 `maxLength` — and both bind
// independently; see [`truncate_for_lexicon`] for why capping graphemes alone
// is not enough and what it cost.
//
// The byte halves were established EMPIRICALLY against the vendored catalog
// (binary search over rendered records, 2026-08-03) rather than transcribed, and
// they are pinned by the boundary arms of
// `cmd/fauna-atproto-bridge/projection_lexicon_ffi_test.go`: a rendering at the
// cap must validate, and one built from input that overruns it must still
// validate after truncation. So a catalog refresh that moves a ceiling turns
// those arms red instead of silently invalidating every projected record.

/// `app.bsky.feed.post.text`.
pub const POST_TEXT_MAX_GRAPHEMES: usize = 300;
/// `app.bsky.feed.post.text`, in UTF-8 bytes.
pub const POST_TEXT_MAX_BYTES: usize = 3000;
/// `app.bsky.actor.profile.displayName`.
pub const PROFILE_DISPLAY_NAME_MAX_GRAPHEMES: usize = 64;
/// `app.bsky.actor.profile.displayName`, in UTF-8 bytes.
pub const PROFILE_DISPLAY_NAME_MAX_BYTES: usize = 640;
/// `app.bsky.actor.profile.description`.
pub const PROFILE_DESCRIPTION_MAX_GRAPHEMES: usize = 256;
/// `app.bsky.actor.profile.description`, in UTF-8 bytes.
pub const PROFILE_DESCRIPTION_MAX_BYTES: usize = 2560;

/// A media attachment the projection should try to publish — the descriptor
/// half, before the caller has fetched or re-hashed anything.
///
/// This is the media analogue of [`ProjectionRefs`]: shared Rust reads the
/// Fauna post and says *which* bytes matter and what they are; the caller does
/// the I/O (fetch, re-hash, store) and hands back [`ResolvedImage`]s. Priority
/// #2 and `atproto-pds-bridge.md` § Where logic lives ("media descriptor prep")
/// put this side in Rust; the bridge still never decodes a Fauna post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionMediaItem {
    /// The Fauna content CID of the bytes, base32 — the identifier the
    /// caller feeds to `GET /api/v1/blob/{cid_b32}`.
    pub blob_cid: String,
    /// The declared MIME type; always `image/*` (non-image items are filtered
    /// out by the extractor — ATProto has no generic file embed).
    pub mime: String,
    /// The declared byte length, from the post. Advisory only: the caller
    /// re-measures what it actually fetched.
    pub size_bytes: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Alt text, or the empty string.
    pub alt: String,
}

/// Extract the image attachments of a Fauna [`Post`] for the projection, in
/// post order, capped at the lexicon's [`MAX_EMBED_IMAGES`].
///
/// Three deliberate filters, each a translation edge worth stating:
///
/// - **Only `image/*` maps.** ATProto has no generic file embed, so an audio
///   or document attachment is dropped rather than mangled into an image.
/// - **`PostBody::Video` yields nothing *here*.** A Fauna video is an HLS
///   manifest plus per-resolution segments, so it is assembled rather than
///   republished — a different mapping, not a variation of this one. It has its
///   own extractor, [`extract_projection_video`], and its own embed.
/// - **`alt_text` lands on the FIRST image only.** `PostBody::Media` carries
///   one alt for the whole set, and repeating it on every image would assert
///   something false about images 2..n; the lexicon's per-image `alt` has no
///   honest way to carry a set-level description.
///
/// Dropping an unmappable attachment — rather than skipping the post — is the
/// same reading § Projection & backfill's translation-edge row already ratified
/// for an unbridged reply parent: withholding a post the user consented to
/// publish is the worse failure.
pub fn extract_projection_media(post: &Post) -> Vec<ProjectionMediaItem> {
    let (items, alt_text) = match &post.body {
        PostBody::Media { items, alt_text } => (items.as_slice(), alt_text.as_deref()),
        PostBody::TextWithMedia { items, .. } | PostBody::Structured { items, .. } => {
            (items.as_slice(), None)
        }
        PostBody::Text { .. } | PostBody::Video { .. } => (&[][..], None),
    };

    items
        .iter()
        .filter(|item| item.media_type.starts_with("image/"))
        .take(MAX_EMBED_IMAGES)
        .enumerate()
        .map(|(idx, item)| ProjectionMediaItem {
            blob_cid: item.blob_hash.to_base32(),
            mime: item.media_type.clone(),
            size_bytes: item.size_bytes,
            width: item.dimensions.as_ref().map(|d| d.width),
            height: item.dimensions.as_ref().map(|d| d.height),
            alt: match (idx, alt_text) {
                (0, Some(alt)) => alt.to_string(),
                _ => String::new(),
            },
        })
        .collect()
}

/// One resolution's worth of a Fauna video — the unit the projection publishes
/// at most one of.
///
/// `segment_cids` is in **playback order**, which is the order the segments sit
/// in `PostBody::Video.segments` filtered to this height. That is the same
/// source the nest's own variant playlist is generated from
/// (`bins/fauna-nest/src/video_routes.rs` § `serve_variant_manifest`), so the
/// bytes the bridge concatenates are the bytes a Fauna player would stream, in
/// the same sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionVideoRendition {
    /// Height in pixels — 360, 720, 1080, 2160.
    pub height: u16,
    /// The Fauna content CIDs of this rendition's segments, base32, in playback
    /// order. Each is a path segment of `GET /api/v1/blob/{cid_b32}`.
    pub segment_cids: Vec<String>,
    /// The sum of the segments' declared byte sizes — what the caller weighs
    /// against its ceiling *before* fetching anything.
    ///
    /// An upper bound on the assembled mp4, not an estimate of it: remuxing
    /// MPEG-TS to mp4 strips the 4-byte header every 188-byte packet carries,
    /// so the result is reliably a little smaller. Choosing on this number is
    /// therefore conservative — it can reject a rendition that would just fit,
    /// never accept one that would not.
    pub declared_bytes: u64,
}

/// A Fauna video's publishable shape: which renditions exist, best first, and
/// the aspect ratio the record needs.
///
/// The *choice* between renditions is deliberately not made here. Shared Rust
/// says which bytes matter and what they are; the ceiling that decides between
/// them is a byte/I-O limit and lives with the caller that does the fetching —
/// the same split that puts the lexicon's [`MAX_EMBED_IMAGES`] in Rust and the
/// per-blob byte ceiling in Go (`atproto-pds-bridge.md` § Where logic lives).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionVideo {
    /// The video's HLS manifest CID, base32 — the stable per-video identity the
    /// caller dedups on.
    ///
    /// It is **not** the identity of the bytes that get published: the caller
    /// assembles those from segments, so they exist nowhere in Fauna and have
    /// no Fauna CID. Without a per-video key, answering "have I already
    /// published this video?" would cost re-fetching and re-assembling every
    /// segment — the same argument that made the blob row remember its Fauna
    /// CID for images, one level up.
    pub manifest_cid: String,
    /// Every rendition the post carries, **highest resolution first**, so a
    /// caller applying a ceiling takes the first that fits and gets the best
    /// copy it can serve.
    pub renditions: Vec<ProjectionVideoRendition>,
    /// `aspectRatio`, from the post's own `aspect_ratio` — never derived from a
    /// chosen rendition, so which rendition fits cannot change how the video is
    /// laid out.
    pub aspect_width: u32,
    pub aspect_height: u32,
}

/// Extract the publishable video of a Fauna [`Post`], or `None` when the post
/// carries none.
///
/// A video with no segments at all yields `None` rather than an empty
/// rendition list: there is nothing to assemble, and the two would otherwise be
/// distinguishable only by a length check every caller would have to remember.
///
/// A zero aspect ratio also yields `None`. `app.bsky.embed.video`'s
/// `aspectRatio` is `{width, height}` with both positive, and a zero would
/// either be refused downstream or render as a collapsed player — the same
/// reading as an undeclared MIME type on a profile picture: a required field
/// with no honest source drops the media rather than inventing a value.
pub fn extract_projection_video(post: &Post) -> Option<ProjectionVideo> {
    let PostBody::Video {
        manifest,
        segments,
        aspect_ratio,
        ..
    } = &post.body
    else {
        return None;
    };
    if segments.is_empty() || aspect_ratio.0 == 0 || aspect_ratio.1 == 0 {
        return None;
    }

    // Group by height, preserving each rendition's segment order. Heights are
    // collected in first-appearance order and sorted after, so the grouping
    // never depends on the segment vec being sorted by resolution.
    let mut heights: Vec<u16> = Vec::new();
    for seg in segments {
        if !heights.contains(&seg.resolution) {
            heights.push(seg.resolution);
        }
    }
    heights.sort_unstable_by(|a, b| b.cmp(a));

    let renditions: Vec<ProjectionVideoRendition> = heights
        .into_iter()
        .map(|height| {
            let mine = segments.iter().filter(|s| s.resolution == height);
            ProjectionVideoRendition {
                height,
                segment_cids: mine.clone().map(|s| s.hash.to_base32()).collect(),
                // Saturating, not `sum()`: the sizes come off a user-signed
                // post, and a wrapping total would understate a huge rendition
                // straight past the caller's ceiling check. The caller also
                // guards the accumulation itself, but a descriptor that lies
                // should not need a second line of defence to be caught.
                declared_bytes: mine.fold(0u64, |acc, s| acc.saturating_add(s.byte_size)),
            }
        })
        .collect();

    Some(ProjectionVideo {
        manifest_cid: manifest.to_base32(),
        renditions,
        aspect_width: u32::from(aspect_ratio.0),
        aspect_height: u32::from(aspect_ratio.1),
    })
}

/// [`extract_projection_video`] over the raw stored content-row bytes, decoded
/// via the same shared [`Post::decode_resolved_bytes`] every other reader of
/// stored post bytes uses.
pub fn extract_projection_video_from_bytes(
    post_bytes: &[u8],
) -> Result<Option<ProjectionVideo>, TranslateError> {
    let post = Post::decode_resolved_bytes(post_bytes).ok_or_else(|| {
        TranslateError::Decode(
            "post bytes are neither the signed embed-as-bytes wire nor a bare canonical Post"
                .to_string(),
        )
    })?;
    Ok(extract_projection_video(&post))
}

/// [`extract_projection_media`] over the raw stored content-row bytes, decoded
/// via the same shared [`Post::decode_resolved_bytes`] every other reader of
/// stored post bytes uses.
pub fn extract_projection_media_from_bytes(
    post_bytes: &[u8],
) -> Result<Vec<ProjectionMediaItem>, TranslateError> {
    let post = Post::decode_resolved_bytes(post_bytes).ok_or_else(|| {
        TranslateError::Decode(
            "post bytes are neither the signed embed-as-bytes wire nor a bare canonical Post"
                .to_string(),
        )
    })?;
    Ok(extract_projection_media(&post))
}

/// The Fauna posts a post refers to, as lowercase-hex post ids — the input the
/// projection needs to resolve [`ReplyRefs`] / [`QuoteRef`] against its
/// PostId→AT-URI map.
///
/// The hex is `Cid::digest()`, not the 36-byte CID: a post's content-row id is
/// `blake3(body)` = its record CID's digest (`bins/fauna-nest/src/segments/post.rs`
/// § "post_id == record_cid.digest()"), and that hex is exactly what the
/// `fetch_public_posts` wire and the bridge's `post_map` key on. So a caller can
/// feed these straight into its map without a second derivation.
///
/// A post may carry several references; the projection honours the **first** of
/// each kind, matching `app.bsky.feed.post`'s single `reply`/`embed.record`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionRefs {
    /// The post this one replies to (`Reference::Reply`).
    pub reply_parent: Option<String>,
    /// The post this one quotes (`Reference::Quote`).
    pub quote: Option<String>,
}

/// Extract the reply/quote targets of a Fauna [`Post`] for the projection.
///
/// Repost/react/upvote/downvote references are deliberately ignored: they have
/// no `app.bsky.feed.post` field, and the interaction lexicons they *would* map
/// to are out of the projection's scope (`atproto-pds-bridge.md` § Projection &
/// backfill — "Defer follow/like/repost *graph* backfill").
pub fn extract_projection_refs(post: &Post) -> ProjectionRefs {
    let mut refs = ProjectionRefs::default();
    for reference in &post.references {
        match reference {
            Reference::Reply { post_id } if refs.reply_parent.is_none() => {
                refs.reply_parent = Some(hex::encode(post_id.digest()));
            }
            Reference::Quote { post_id } if refs.quote.is_none() => {
                refs.quote = Some(hex::encode(post_id.digest()));
            }
            _ => {}
        }
        if refs.reply_parent.is_some() && refs.quote.is_some() {
            break;
        }
    }
    refs
}

/// [`extract_projection_refs`] over the raw stored content-row bytes, decoded
/// via the same shared [`Post::decode_resolved_bytes`] the translators use — so
/// extraction and translation can never disagree on which shapes decode.
pub fn extract_projection_refs_from_bytes(
    post_bytes: &[u8],
) -> Result<ProjectionRefs, TranslateError> {
    let post = Post::decode_resolved_bytes(post_bytes).ok_or_else(|| {
        TranslateError::Decode(
            "post bytes are neither the signed embed-as-bytes wire nor a bare canonical Post"
                .to_string(),
        )
    })?;
    Ok(extract_projection_refs(&post))
}

/// Error from the deterministic projection translators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// Stored bytes did not decode as the expected Fauna wire shape.
    Decode(String),
    /// Record JSON serialization failed (unreachable for these shapes in
    /// practice; kept explicit rather than panicking).
    Json(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::Decode(msg) => write!(f, "decode: {msg}"),
            TranslateError::Json(msg) => write!(f, "json: {msg}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// Translate a Fauna [`Post`] into the deterministic `app.bsky.feed.post`
/// record JSON for the projection path (repo re-derivation).
///
/// Unlike [`fauna_post_to_bsky_record`] (write-through, stamps "now"), the
/// record's `createdAt` is the post's own `created_at` — byte-identical
/// re-derivation requires it.
///
/// `images` are the attachments the caller has already fetched, re-hashed and
/// **stored** — passing one whose bytes the PDS cannot serve would publish a
/// dangling ref, which § Projection & backfill forbids. An empty slice projects
/// the post with no media embed, which is also what a caller does when every
/// attachment was unfetchable: the post still publishes, minus the image.
/// `video` is the single mp4 blob the caller assembled from one rendition, on
/// the same already-stored contract as `images`; the two are mutually exclusive
/// by construction. `self_labels` is the D-s3-3 mechanism; v1 callers pass
/// `&[]`.
///
/// Returns `Ok(None)` when the record would carry **neither text nor an
/// embed** — a media post whose every attachment was dropped, or a video whose
/// renditions all exceeded the caller's ceiling. Committing that record would
/// publish a blank post, which is strictly worse than publishing nothing: it
/// asserts the user said nothing, where absence asserts nothing at all
/// (`atproto-pds-bridge.md` § Projection & backfill). Skipping does not cascade
/// the way skipping an unbridged reply parent would — by that same section's
/// ruling a post whose parent is unmapped projects *standalone*, so a skipped
/// post costs its replies their ref, never their publication.
pub fn translate_post_for_projection(
    post: &Post,
    reply: Option<&ReplyRefs>,
    quote: Option<&QuoteRef>,
    images: &[ResolvedImage],
    video: Option<&ResolvedVideo>,
    self_labels: &[String],
) -> Result<Option<String>, TranslateError> {
    const NO_FACETS: &[Facet] = &[];
    let (text, facets): (&str, &[Facet]) = match &post.body {
        PostBody::Text { content, facets } => (content, facets),
        PostBody::TextWithMedia {
            content, facets, ..
        } => (content, facets),
        PostBody::Structured {
            content, facets, ..
        } => (content.as_deref().unwrap_or(""), facets),
        // Neither variant has text of its own: a media post's images and a
        // video post's assembled blob ride the embed instead.
        PostBody::Media { .. } | PostBody::Video { .. } => ("", NO_FACETS),
    };

    // Whether an embed will exist is decided from the same inputs the builder
    // gets, rather than by inspecting the JSON it produced — the record's shape
    // is the builder's business, and re-reading it here would couple this rule
    // to field names it does not own.
    let has_embed = quote.is_some() || !images.is_empty() || video.is_some();
    if text.is_empty() && !has_embed {
        return Ok(None);
    }

    let created_at = micros_to_iso8601(post.created_at.0);
    let record = build_post_record(
        text,
        facets,
        &created_at,
        reply,
        quote,
        images,
        video,
        self_labels,
    );
    serde_json::to_string(&record)
        .map(Some)
        .map_err(|e| TranslateError::Json(e.to_string()))
}

/// [`translate_post_for_projection`] over the raw stored content-row bytes
/// (the payload `fauna.bridges.atproto.fetch_public_posts` serves), decoded
/// via the shared [`Post::decode_resolved_bytes`] — the single decode path
/// every reader of stored post bytes uses.
pub fn translate_post_bytes_for_projection(
    post_bytes: &[u8],
    reply: Option<&ReplyRefs>,
    quote: Option<&QuoteRef>,
    images: &[ResolvedImage],
    video: Option<&ResolvedVideo>,
    self_labels: &[String],
) -> Result<Option<String>, TranslateError> {
    let post = Post::decode_resolved_bytes(post_bytes).ok_or_else(|| {
        TranslateError::Decode(
            "post bytes are neither the signed embed-as-bytes wire nor a bare canonical Post"
                .to_string(),
        )
    })?;
    translate_post_for_projection(&post, reply, quote, images, video, self_labels)
}

/// The profile pictures a projection should try to publish — the profile
/// analogue of [`extract_projection_media`], and the input the caller feeds
/// through the same fetch/re-hash/store path before referencing either.
///
/// Both fields are independent: a profile may carry an avatar, a banner,
/// both, or neither, and one being unpublishable never withholds the other.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMedia {
    pub avatar: Option<ProjectionMediaItem>,
    pub banner: Option<ProjectionMediaItem>,
}

/// Extract the avatar and banner of a Fauna [`Profile`] for the projection.
///
/// Unlike a post attachment, a profile picture is stored as a bare
/// [`fauna_core::data::ContentHash`] — the profile records *which* bytes, and
/// nothing else. So the descriptors here carry **no declared MIME type**
/// (empty string), no size and no dimensions, and the caller that fetches the
/// bytes is the one that determines what they are. `size_bytes` on a post
/// descriptor was already advisory ("the caller re-measures what it actually
/// fetched"); here there is simply nothing to advise.
///
/// `alt` is empty and stays empty: `app.bsky.actor.profile`'s `avatar` and
/// `banner` are bare blob refs with no alt text in the lexicon, and Fauna's
/// profile carries none either.
pub fn extract_profile_media(profile: &Profile) -> ProfileMedia {
    let descriptor = |hash: &fauna_core::data::ContentHash| ProjectionMediaItem {
        blob_cid: hash.to_base32(),
        // Undeclared: the profile stores a hash, not a media type. The caller
        // determines the real type from the bytes it fetched.
        mime: String::new(),
        size_bytes: 0,
        width: None,
        height: None,
        alt: String::new(),
    };
    ProfileMedia {
        avatar: profile.avatar.as_ref().map(descriptor),
        banner: profile.banner.as_ref().map(descriptor),
    }
}

/// [`extract_profile_media`] over the raw stored profile content-row bytes,
/// decoded via the same shared [`fauna_core::encoding::decode_profile`] every
/// other reader of stored profile bytes uses.
pub fn extract_profile_media_from_bytes(
    profile_bytes: &[u8],
) -> Result<ProfileMedia, TranslateError> {
    let (profile, _origin) = fauna_core::encoding::decode_profile(profile_bytes)
        .map_err(|e| TranslateError::Decode(format!("profile bytes: {e}")))?;
    Ok(extract_profile_media(&profile))
}

/// Translate a Fauna [`Profile`] into the `app.bsky.actor.profile` record
/// JSON (rkey [`PROFILE_SELF_RKEY`]).
///
/// Maps `display_name` → `displayName` (truncated to the lexicon's 64
/// graphemes) and `bio` → `description` (256 graphemes).
///
/// `avatar` and `banner` are the pictures the caller has already fetched,
/// re-hashed and **stored** — the same contract [`translate_post_for_projection`]'s
/// `images` carries, for the same reason: referencing a blob the PDS cannot
/// serve would publish a dangling ref, which § Projection & backfill forbids.
///
/// Passing `None` for either **omits that field from the record**, which is
/// what makes clearing a picture work: the record is rebuilt from the profile
/// on every projection pass and replaces the stored one wholesale, so a
/// cleared avatar stops being referenced the moment the next pass runs. `None`
/// is equally the right value when the picture existed but could not be
/// published — the profile still projects, minus that field, exactly as a post
/// publishes minus an unfetchable attachment.
pub fn translate_profile_for_projection(
    profile: &Profile,
    avatar: Option<&ResolvedImage>,
    banner: Option<&ResolvedImage>,
) -> Result<String, TranslateError> {
    let mut record = json!({ "$type": "app.bsky.actor.profile" });
    if let Some(dn) = &profile.display_name {
        record["displayName"] = Value::String(truncate_for_lexicon(
            dn,
            PROFILE_DISPLAY_NAME_MAX_GRAPHEMES,
            PROFILE_DISPLAY_NAME_MAX_BYTES,
        ));
    }
    if let Some(bio) = &profile.bio {
        record["description"] = Value::String(truncate_for_lexicon(
            bio,
            PROFILE_DESCRIPTION_MAX_GRAPHEMES,
            PROFILE_DESCRIPTION_MAX_BYTES,
        ));
    }
    if let Some(img) = avatar {
        record["avatar"] = build_blob_ref(img);
    }
    if let Some(img) = banner {
        record["banner"] = build_blob_ref(img);
    }
    serde_json::to_string(&record).map_err(|e| TranslateError::Json(e.to_string()))
}

/// [`translate_profile_for_projection`] over the raw stored profile
/// content-row bytes, decoded via the shared
/// [`fauna_core::encoding::decode_profile`] (signed-only verify — the same
/// helper `fauna.profile.get` and the federation serve use).
pub fn translate_profile_bytes_for_projection(
    profile_bytes: &[u8],
    avatar: Option<&ResolvedImage>,
    banner: Option<&ResolvedImage>,
) -> Result<String, TranslateError> {
    let (profile, _origin) = fauna_core::encoding::decode_profile(profile_bytes)
        .map_err(|e| TranslateError::Decode(format!("profile bytes: {e}")))?;
    translate_profile_for_projection(&profile, avatar, banner)
}

/// Render microseconds since Unix epoch as an RFC 3339 UTC timestamp with
/// microsecond precision (`YYYY-MM-DDTHH:MM:SS.ssssssZ`) — the deterministic
/// `createdAt` of projected records.
pub fn micros_to_iso8601(micros: u64) -> String {
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    let (year, month, day, hour, min, sec) = secs_to_datetime(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        year, month, day, hour, min, sec, frac
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::ContentHash;

    #[test]
    fn now_iso8601_format() {
        let ts = now_iso8601();
        // Should match YYYY-MM-DDTHH:MM:SS.mmmZ
        assert!(ts.ends_with('Z'), "timestamp should end with Z: {ts}");
        assert_eq!(ts.len(), 24, "timestamp length: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], ".");
    }

    #[test]
    fn secs_to_datetime_epoch() {
        let (y, m, d, h, mi, s) = secs_to_datetime(0);
        assert_eq!((y, m, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn secs_to_datetime_known_date() {
        // 2026-03-20T12:00:00Z = 1774008000
        let (y, m, d, h, mi, s) = secs_to_datetime(1_774_008_000);
        assert_eq!(y, 2026);
        assert_eq!(m, 3);
        assert_eq!(d, 20);
        assert_eq!(h, 12);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    // network-exposure.md § Rulings F2: a mention of a user who did not opt
    // into Bluesky must not publish their raw actor pubkey to the firehose.
    #[test]
    fn mention_facet_is_omitted_never_leaks_actor_pubkey() {
        use fauna_core::data::{Facet, FacetFeature};
        use fauna_core::identity::ActorId;

        let actor = ActorId([0xAB; 32]);
        let facets = vec![Facet {
            byte_start: 3,
            byte_end: 9,
            feature: FacetFeature::Mention { actor_id: actor },
        }];
        let rec = fauna_post_to_bsky_record("hi @alice", &facets, None, None).expect("record");
        let wire = serde_json::to_string(&rec.record_json).unwrap();

        // The display text is preserved …
        assert_eq!(rec.record_json["text"], "hi @alice");
        // … but nothing about the mentioned user crosses to the firehose:
        // no `#mention` facet and, above all, no raw actor pubkey hex.
        assert!(
            !wire.contains("#mention"),
            "mention facet must be omitted: {wire}"
        );
        assert!(
            !wire.contains(&hex::encode(actor.0)),
            "actor pubkey must never reach the firehose: {wire}"
        );
        // A mention-only post carries no `facets` array at all.
        assert!(
            rec.record_json.get("facets").is_none(),
            "no surviving facets expected: {wire}"
        );
    }

    // Regression guard: dropping mentions must not drop link/tag facets.
    #[test]
    fn link_and_tag_facets_survive() {
        use fauna_core::data::{Facet, FacetFeature};

        let facets = vec![
            Facet {
                byte_start: 0,
                byte_end: 4,
                feature: FacetFeature::Link {
                    uri: "https://example.com".into(),
                },
            },
            Facet {
                byte_start: 5,
                byte_end: 9,
                feature: FacetFeature::Tag {
                    name: "rust".into(),
                },
            },
        ];
        let rec = fauna_post_to_bsky_record("link #tag", &facets, None, None).expect("record");
        let out = rec.record_json["facets"].as_array().expect("facets array");
        assert_eq!(out.len(), 2, "both link and tag facets should survive");
        let wire = serde_json::to_string(&rec.record_json).unwrap();
        assert!(wire.contains("#link"), "link facet present: {wire}");
        assert!(wire.contains("#tag"), "tag facet present: {wire}");
    }

    // ── Deterministic projection (S3) ──────────────────────────────────────

    /// Pinned D-s3-2 vectors, computed with an independent implementation of
    /// the spec (BLAKE3 via the python `blake3` package). value =
    /// (clamp(micros, 0, 2^53-1) << 10) | first-10-bits-of-BLAKE3(post_id),
    /// base32-sortable, 13 chars, MSB first.
    #[test]
    fn deterministic_tid_pinned_vectors() {
        // BLAKE3("") = af1349b9… → disambiguator (0xaf<<2)|(0x13>>6) = 700.
        assert_eq!(deterministic_tid(0, b""), "22222222222pw");
        // BLAKE3([1,2,3,4]) starts 0x63 0x78.
        assert_eq!(deterministic_tid(0, &[1, 2, 3, 4]), "22222222222gh");
        // 2026-03-20T12:00:00Z = 1_774_008_000_000_000 µs; a 36-byte
        // CID-shaped post id (0x00..0x23); BLAKE3 starts 0x02 0xb3.
        let cid_shaped: Vec<u8> = (0u8..36).collect();
        assert_eq!(
            deterministic_tid(1_774_008_000_000_000, &cid_shaped),
            "3mhihqeqg222e"
        );
    }

    /// Out-of-range instants clamp (saturating) into the 53-bit range.
    #[test]
    fn deterministic_tid_clamps_out_of_range_micros() {
        // Pre-epoch → 0.
        assert_eq!(deterministic_tid(-5, b"neg"), "22222222222im");
        assert_eq!(
            deterministic_tid(i64::MIN, b"neg"),
            deterministic_tid(0, b"neg")
        );
        // Beyond 2^53-1 → 2^53-1 (top bit of the 64-bit value stays 0).
        let max53 = (1i64 << 53) - 1;
        assert_eq!(deterministic_tid(1i64 << 60, b"big"), "bzzzzzzzzzzyz");
        assert_eq!(
            deterministic_tid(i64::MAX, b"big"),
            deterministic_tid(max53, b"big")
        );
    }

    /// TID strings must sort lexicographically in (created_at, disambiguator)
    /// order — that is the whole point of the base32-sortable alphabet.
    #[test]
    fn deterministic_tid_sorts_by_creation_instant() {
        let a = deterministic_tid(1_000, b"x");
        let b = deterministic_tid(2_000, b"x");
        let c = deterministic_tid(1_774_008_000_000_000, b"x");
        assert!(a < b, "{a} < {b}");
        assert!(b < c, "{b} < {c}");
    }

    fn text_post(content: &str, facets: Vec<Facet>) -> Post {
        use fauna_core::data::Timestamp;
        use fauna_core::identity::ActorId;
        Post {
            author: ActorId([7; 32]),
            created_at: Timestamp(1_774_008_000_000_000), // 2026-03-20T12:00:00Z
            body: PostBody::Text {
                content: content.to_string(),
                facets,
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    /// Translate a post that is expected to project, and hand back the record
    /// as JSON. A `None` here is a skipped post, which every caller of this
    /// helper would treat as a failure — the skip arm has its own tests.
    fn translate_ok(
        post: &Post,
        reply: Option<&ReplyRefs>,
        quote: Option<&QuoteRef>,
        images: &[ResolvedImage],
        video: Option<&ResolvedVideo>,
    ) -> Value {
        let out = translate_post_for_projection(post, reply, quote, images, video, &[])
            .expect("translate must not fail")
            .expect("this post must project, not be skipped");
        serde_json::from_str(&out).expect("record is valid JSON")
    }

    #[test]
    fn projection_post_record_minimal_text() {
        let post = text_post("hello bluesky", vec![]);
        let v = translate_ok(&post, None, None, &[], None);
        assert_eq!(v["$type"], "app.bsky.feed.post");
        assert_eq!(v["text"], "hello bluesky");
        // createdAt is the post's own instant, microsecond precision — NOT "now".
        assert_eq!(v["createdAt"], "2026-03-20T12:00:00.000000Z");
        assert!(v.get("facets").is_none());
        assert!(v.get("reply").is_none());
        assert!(v.get("embed").is_none());
        assert!(v.get("labels").is_none());
    }

    #[test]
    fn projection_post_record_facets_reply_quote_labels() {
        use fauna_core::data::FacetFeature;
        let facets = vec![Facet {
            byte_start: 0,
            byte_end: 4,
            feature: FacetFeature::Link {
                uri: "https://example.com".into(),
            },
        }];
        let post = text_post("link and more", facets);
        let reply = ReplyRefs {
            parent_uri: "at://did:plc:parent/app.bsky.feed.post/3aaaaaaaaaaaa".into(),
            parent_cid: "bafyparent".into(),
            root_uri: "at://did:plc:root/app.bsky.feed.post/3bbbbbbbbbbbb".into(),
            root_cid: "bafyroot".into(),
        };
        let quote = QuoteRef {
            uri: "at://did:plc:quoted/app.bsky.feed.post/3cccccccccccc".into(),
            cid: "bafyquoted".into(),
        };
        let labels = vec!["sexual".to_string()];
        let out =
            translate_post_for_projection(&post, Some(&reply), Some(&quote), &[], None, &labels)
                .expect("translate must not fail")
                .expect("this post must project");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["facets"][0]["index"]["byteStart"], 0);
        assert_eq!(v["facets"][0]["index"]["byteEnd"], 4);
        assert_eq!(
            v["facets"][0]["features"][0]["$type"],
            "app.bsky.richtext.facet#link"
        );
        assert_eq!(v["reply"]["parent"]["uri"], reply.parent_uri.as_str());
        assert_eq!(v["reply"]["parent"]["cid"], "bafyparent");
        assert_eq!(v["reply"]["root"]["uri"], reply.root_uri.as_str());
        assert_eq!(v["reply"]["root"]["cid"], "bafyroot");
        assert_eq!(v["embed"]["$type"], "app.bsky.embed.record");
        assert_eq!(v["embed"]["record"]["uri"], quote.uri.as_str());
        assert_eq!(v["embed"]["record"]["cid"], "bafyquoted");
        assert_eq!(v["labels"]["$type"], "com.atproto.label.defs#selfLabels");
        assert_eq!(v["labels"]["values"][0]["val"], "sexual");
    }

    fn media_item(mime: &str, bytes: &[u8]) -> fauna_core::data::MediaItem {
        fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::of_raw(bytes),
            media_type: mime.into(),
            size_bytes: bytes.len() as u64,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        }
    }

    fn media_post(items: Vec<fauna_core::data::MediaItem>, alt_text: Option<&str>) -> Post {
        use fauna_core::data::Timestamp;
        use fauna_core::identity::ActorId;
        Post {
            author: ActorId([7; 32]),
            created_at: Timestamp(1_774_008_000_000_000),
            body: PostBody::Media {
                items,
                alt_text: alt_text.map(str::to_string),
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    fn resolved(cid: &str, alt: &str) -> ResolvedImage {
        ResolvedImage {
            blob_cid: cid.into(),
            mime: "image/png".into(),
            size_bytes: 3,
            width: Some(800),
            height: Some(600),
            alt: alt.into(),
        }
    }

    /// A media post whose every attachment was dropped carries neither text nor
    /// an embed, so it is **not projected at all** rather than committed as a
    /// blank record (§ Projection & backfill). Publishing it would assert the
    /// user said nothing; skipping asserts nothing.
    ///
    /// This inverts the pre-2026-07-29 behaviour, which committed the blank
    /// record deliberately so replies could anchor to it. That reason did not
    /// survive slice 3's ruling: an unmapped parent makes a reply project
    /// *standalone*, so the anchor was never what kept the subtree publishable.
    #[test]
    fn projection_media_post_without_resolved_images_is_skipped() {
        let post = media_post(vec![media_item("image/png", b"img")], Some("a picture"));
        let out = translate_post_for_projection(&post, None, None, &[], None, &[])
            .expect("translate must not fail");
        assert!(
            out.is_none(),
            "a record with neither text nor embed must be skipped, not committed blank"
        );
    }

    /// The same post *with* a quote is not blank — the quote is an embed — so
    /// it still projects. The skip rule turns on the record carrying nothing,
    /// never on the images alone being absent.
    #[test]
    fn projection_media_post_without_images_but_with_quote_still_projects() {
        let post = media_post(vec![media_item("image/png", b"img")], Some("a picture"));
        let quote = QuoteRef {
            uri: "at://did:plc:x/app.bsky.feed.post/3l".into(),
            cid: "bafyquoted".into(),
        };
        let v = translate_ok(&post, None, Some(&quote), &[], None);
        assert_eq!(v["text"], "");
        assert_eq!(v["embed"]["$type"], "app.bsky.embed.record");
    }

    /// The descriptor half: which bytes matter, and what they are.
    #[test]
    fn extract_media_reads_items_and_first_item_alt() {
        let post = media_post(
            vec![
                media_item("image/png", b"one"),
                media_item("image/jpeg", b"two"),
            ],
            Some("the set"),
        );
        let items = extract_projection_media(&post);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blob_cid, ContentHash::of_raw(b"one").to_base32());
        assert_eq!(items[0].mime, "image/png");
        assert_eq!(items[0].size_bytes, 3);
        // One post-level alt describes the set; asserting it of image 2 as
        // well would be a claim the post never made.
        assert_eq!(items[0].alt, "the set");
        assert_eq!(items[1].alt, "");
    }

    /// Non-image attachments are dropped (no generic ATProto file embed), and
    /// the list is capped at the lexicon's four.
    #[test]
    fn extract_media_filters_non_images_and_caps_at_four() {
        let post = media_post(
            vec![
                media_item("audio/mpeg", b"a"),
                media_item("image/png", b"b"),
                media_item("application/pdf", b"c"),
                media_item("image/png", b"d"),
                media_item("image/png", b"e"),
                media_item("image/png", b"f"),
                media_item("image/png", b"g"),
            ],
            None,
        );
        let items = extract_projection_media(&post);
        assert_eq!(items.len(), MAX_EMBED_IMAGES);
        assert!(items.iter().all(|i| i.mime.starts_with("image/")));
    }

    /// `TextWithMedia` carries images beside real text; `Video` carries none
    /// (an HLS manifest plus segments is not a single blob).
    #[test]
    fn extract_media_covers_text_with_media_and_skips_video() {
        use fauna_core::data::{Timestamp, VideoSegment};
        use fauna_core::identity::ActorId;
        let mut post = media_post(vec![], None);
        post.body = PostBody::TextWithMedia {
            content: "look".into(),
            facets: vec![],
            items: vec![media_item("image/webp", b"pic")],
        };
        assert_eq!(extract_projection_media(&post).len(), 1);

        let video = Post {
            author: ActorId([7; 32]),
            created_at: Timestamp(1_774_008_000_000_000),
            body: PostBody::Video {
                manifest: ContentHash::of_raw(b"m"),
                segments: vec![VideoSegment {
                    hash: ContentHash::of_raw(b"s"),
                    resolution: 720,
                    codec: "h264".into(),
                    bitrate: 2500,
                    byte_size: 100,
                }],
                thumbnail: ContentHash::of_raw(b"t"),
                duration_ms: 1000,
                aspect_ratio: (16, 9),
                anchors: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        assert!(
            extract_projection_media(&video).is_empty(),
            "video has its own extractor and embed; it must not leak into the image path"
        );
    }

    // ── Video ────────────────────────────────────────────────────────────

    fn video_segment(res: u16, bytes: &[u8], byte_size: u64) -> fauna_core::data::VideoSegment {
        fauna_core::data::VideoSegment {
            hash: ContentHash::of_raw(bytes),
            resolution: res,
            codec: "h264".into(),
            bitrate: 2500,
            byte_size,
        }
    }

    fn video_post(segments: Vec<fauna_core::data::VideoSegment>, aspect_ratio: (u16, u16)) -> Post {
        use fauna_core::data::Timestamp;
        use fauna_core::identity::ActorId;
        Post {
            author: ActorId([7; 32]),
            created_at: Timestamp(1_774_008_000_000_000),
            body: PostBody::Video {
                manifest: ContentHash::of_raw(b"manifest"),
                segments,
                thumbnail: ContentHash::of_raw(b"thumb"),
                duration_ms: 12_000,
                aspect_ratio,
                anchors: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    fn resolved_video(cid: &str) -> ResolvedVideo {
        ResolvedVideo {
            blob_cid: cid.into(),
            mime: "video/mp4".into(),
            size_bytes: 4_242_424,
            aspect_width: 16,
            aspect_height: 9,
        }
    }

    /// The descriptor half: renditions are grouped by height, **best first**,
    /// each carrying its segments in playback order and its declared total —
    /// so a caller applying a byte ceiling takes the first that fits.
    #[test]
    fn extract_video_groups_renditions_best_first() {
        // Deliberately interleaved and ascending, so a grouping that assumed
        // the vec was already sorted or contiguous would fail here.
        let post = video_post(
            vec![
                video_segment(360, b"a360", 10),
                video_segment(720, b"a720", 100),
                video_segment(360, b"b360", 11),
                video_segment(1080, b"a1080", 1000),
                video_segment(720, b"b720", 101),
            ],
            (16, 9),
        );
        let v = extract_projection_video(&post).expect("a video post yields a descriptor");

        assert_eq!(
            v.renditions.iter().map(|r| r.height).collect::<Vec<_>>(),
            vec![1080, 720, 360],
            "highest resolution first"
        );
        assert_eq!(v.renditions[0].declared_bytes, 1000);
        assert_eq!(v.renditions[1].declared_bytes, 201);
        assert_eq!(v.renditions[2].declared_bytes, 21);

        // Segment order within a rendition is playback order, and the CIDs are
        // exactly what `GET /api/v1/blob/{cid_b32}` takes.
        assert_eq!(
            v.renditions[1].segment_cids,
            vec![
                ContentHash::of_raw(b"a720").to_base32(),
                ContentHash::of_raw(b"b720").to_base32(),
            ]
        );
        assert_eq!(v.manifest_cid, ContentHash::of_raw(b"manifest").to_base32());
        assert_eq!((v.aspect_width, v.aspect_height), (16, 9));
    }

    /// A non-video post has no video descriptor — the two extractors partition
    /// the body variants rather than overlapping.
    #[test]
    fn extract_video_is_none_for_a_non_video_post() {
        assert!(extract_projection_video(&text_post("hi", vec![])).is_none());
        assert!(
            extract_projection_video(&media_post(vec![media_item("image/png", b"i")], None))
                .is_none()
        );
    }

    /// A required field with no honest source drops the media rather than
    /// inventing a value — the same reading as an undeclared profile-picture
    /// MIME type. A zero aspect ratio cannot be published as `aspectRatio`,
    /// and a segment-less video has nothing to assemble.
    #[test]
    fn extract_video_is_none_without_segments_or_aspect_ratio() {
        assert!(
            extract_projection_video(&video_post(vec![], (16, 9))).is_none(),
            "no segments means nothing to assemble"
        );
        assert!(
            extract_projection_video(&video_post(vec![video_segment(720, b"s", 100)], (0, 9)))
                .is_none(),
            "a zero aspect ratio has no publishable aspectRatio"
        );
        assert!(
            extract_projection_video(&video_post(vec![video_segment(720, b"s", 100)], (16, 0)))
                .is_none()
        );
    }

    /// The record half: a resolved video becomes `app.bsky.embed.video`, with
    /// the blob in the same `$link` shape every other blob ref uses.
    #[test]
    fn projection_video_post_emits_video_embed() {
        let post = video_post(vec![video_segment(720, b"s", 100)], (16, 9));
        let video = resolved_video("bafkreivideo");
        let v = translate_ok(&post, None, None, &[], Some(&video));

        assert_eq!(
            v["text"], "",
            "a Fauna video post carries no text of its own"
        );
        assert_eq!(v["embed"]["$type"], "app.bsky.embed.video");
        assert_eq!(v["embed"]["video"]["$type"], "blob");
        assert_eq!(v["embed"]["video"]["ref"]["$link"], "bafkreivideo");
        assert_eq!(v["embed"]["video"]["mimeType"], "video/mp4");
        assert_eq!(v["embed"]["video"]["size"], 4_242_424u64);
        assert_eq!(v["embed"]["aspectRatio"]["width"], 16);
        assert_eq!(v["embed"]["aspectRatio"]["height"], 9);
        // Fauna carries neither, and the lexicon marks both optional: absent,
        // never an empty string that would assert a blank description.
        assert!(v["embed"].get("alt").is_none());
        assert!(v["embed"].get("captions").is_none());
    }

    /// A quote plus a video nests the same way a quote plus images does — the
    /// lexicon's single `embed` field takes `recordWithMedia` either way.
    #[test]
    fn projection_quote_plus_video_nests_as_record_with_media() {
        let post = video_post(vec![video_segment(720, b"s", 100)], (16, 9));
        let quote = QuoteRef {
            uri: "at://did:plc:x/app.bsky.feed.post/3l".into(),
            cid: "bafyquoted".into(),
        };
        let video = resolved_video("bafkreivideo");
        let v = translate_ok(&post, None, Some(&quote), &[], Some(&video));

        assert_eq!(v["embed"]["$type"], "app.bsky.embed.recordWithMedia");
        assert_eq!(v["embed"]["record"]["record"]["cid"], "bafyquoted");
        assert_eq!(v["embed"]["media"]["$type"], "app.bsky.embed.video");
        assert_eq!(v["embed"]["media"]["video"]["ref"]["$link"], "bafkreivideo");
    }

    /// A video the caller could not assemble — every rendition over its ceiling,
    /// or a failed fetch — leaves a record with neither text nor embed, so the
    /// post is skipped rather than committed blank. This is the arm that used to
    /// publish an empty post for every video a user posted.
    #[test]
    fn projection_video_post_without_resolved_video_is_skipped() {
        let post = video_post(vec![video_segment(720, b"s", 100)], (16, 9));
        let out = translate_post_for_projection(&post, None, None, &[], None, &[])
            .expect("translate must not fail");
        assert!(
            out.is_none(),
            "an unpublishable video must skip the post, not commit a blank record"
        );
    }

    /// The bytes path agrees with the typed one, over the real stored wire.
    #[test]
    fn extract_video_from_signed_wire_bytes() {
        use fauna_core::encoding::sign_and_pack;
        use fauna_core::identity::ActorKeypair;
        let kp = ActorKeypair::generate();
        let mut post = video_post(
            vec![
                video_segment(720, b"a720", 100),
                video_segment(360, b"a360", 10),
            ],
            (9, 16),
        );
        post.author = kp.actor_id();
        let stored = sign_and_pack(&kp, &post).expect("sign+pack");

        let v = extract_projection_video_from_bytes(&stored)
            .expect("decode")
            .expect("a video post yields a descriptor");
        assert_eq!(
            v.renditions.iter().map(|r| r.height).collect::<Vec<_>>(),
            vec![720, 360]
        );
        assert_eq!((v.aspect_width, v.aspect_height), (9, 16));
        assert_eq!(extract_projection_video(&post), Some(v));
    }

    /// The record half: a resolved image becomes `app.bsky.embed.images`, with
    /// the blob's `ref` in the data-model `$link` shape the dag-cbor encoder
    /// turns into a real CID link.
    #[test]
    fn projection_media_post_emits_images_embed() {
        let post = media_post(vec![media_item("image/png", b"img")], Some("a picture"));
        let images = vec![resolved("bafkreiimage", "a picture")];
        let v = translate_ok(&post, None, None, &images, None);
        assert_eq!(v["embed"]["$type"], "app.bsky.embed.images");
        let img = &v["embed"]["images"][0];
        assert_eq!(img["alt"], "a picture");
        assert_eq!(img["image"]["$type"], "blob");
        assert_eq!(img["image"]["ref"]["$link"], "bafkreiimage");
        assert_eq!(img["image"]["mimeType"], "image/png");
        assert_eq!(img["image"]["size"], 3);
        assert_eq!(img["aspectRatio"]["width"], 800);
        assert_eq!(img["aspectRatio"]["height"], 600);
    }

    /// Dimensions are optional on the descriptor, and `aspectRatio` is optional
    /// in the lexicon — an unknown ratio is absent, never invented.
    #[test]
    fn projection_image_without_dimensions_omits_aspect_ratio() {
        let post = media_post(vec![media_item("image/png", b"img")], None);
        let images = vec![ResolvedImage {
            width: None,
            height: None,
            ..resolved("bafkreiimage", "")
        }];
        let v = translate_ok(&post, None, None, &images, None);
        assert!(v["embed"]["images"][0].get("aspectRatio").is_none());
    }

    /// A post that both quotes and carries media takes the lexicon's own answer
    /// for a single `embed` field: `recordWithMedia`, nesting the two.
    #[test]
    fn projection_quote_plus_media_nests_as_record_with_media() {
        let post = media_post(vec![media_item("image/png", b"img")], None);
        let quote = QuoteRef {
            uri: "at://did:plc:x/app.bsky.feed.post/3l".into(),
            cid: "bafyquoted".into(),
        };
        let images = vec![resolved("bafkreiimage", "")];
        let v = translate_ok(&post, None, Some(&quote), &images, None);
        assert_eq!(v["embed"]["$type"], "app.bsky.embed.recordWithMedia");
        assert_eq!(v["embed"]["record"]["$type"], "app.bsky.embed.record");
        assert_eq!(v["embed"]["record"]["record"]["cid"], "bafyquoted");
        assert_eq!(v["embed"]["media"]["$type"], "app.bsky.embed.images");
        assert_eq!(
            v["embed"]["media"]["images"][0]["image"]["ref"]["$link"],
            "bafkreiimage"
        );
    }

    /// The descriptor CID is exactly the `GET /api/v1/blob/{cid_b32}` path
    /// segment the caller fetches — no second derivation on the Go side.
    #[test]
    fn extract_media_cid_is_the_blob_fetch_path_segment() {
        let post = media_post(vec![media_item("image/png", b"bytes")], None);
        let items = extract_projection_media(&post);
        let expected = ContentHash::of_raw(b"bytes").to_base32();
        assert_eq!(items[0].blob_cid, expected);
        assert!(
            ContentHash::from_base32(&items[0].blob_cid).is_ok(),
            "the emitted CID must parse back as a CID: {expected}"
        );
    }

    /// A profile carrying the given pictures and nothing else of interest.
    fn profile_with_pictures(avatar: Option<ContentHash>, banner: Option<ContentHash>) -> Profile {
        use fauna_core::data::{InboxMode, Timestamp};
        use fauna_core::identity::ActorId;
        Profile {
            actor_id: ActorId([9; 32]),
            display_name: Some("Alice".into()),
            bio: None,
            avatar,
            banner,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        }
    }

    #[test]
    fn projection_profile_record() {
        use fauna_core::data::{InboxMode, Timestamp};
        use fauna_core::identity::ActorId;
        let profile = Profile {
            actor_id: ActorId([9; 32]),
            display_name: Some("Alice".into()),
            bio: Some("building fauna".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        };
        let out = translate_profile_for_projection(&profile, None, None).expect("translate");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["$type"], "app.bsky.actor.profile");
        assert_eq!(v["displayName"], "Alice");
        assert_eq!(v["description"], "building fauna");

        // None fields are omitted, not null.
        let bare = Profile {
            display_name: None,
            bio: None,
            ..profile
        };
        let out = translate_profile_for_projection(&bare, None, None).expect("translate");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("displayName").is_none());
        assert!(v.get("description").is_none());
    }

    /// A profile with pictures: both refs land as the data-model blob shape,
    /// byte-identical to the one a post attachment emits (same lexicon type).
    #[test]
    fn projection_profile_record_carries_avatar_and_banner() {
        let profile = profile_with_pictures(
            Some(ContentHash::of_raw(b"avatar bytes")),
            Some(ContentHash::of_raw(b"banner bytes")),
        );
        let avatar = ResolvedImage {
            blob_cid: "bafkreiavatar".into(),
            mime: "image/png".into(),
            size_bytes: 1234,
            width: None,
            height: None,
            alt: String::new(),
        };
        let banner = ResolvedImage {
            blob_cid: "bafkreibanner".into(),
            mime: "image/jpeg".into(),
            size_bytes: 5678,
            width: None,
            height: None,
            alt: String::new(),
        };
        let out = translate_profile_for_projection(&profile, Some(&avatar), Some(&banner))
            .expect("translate");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["avatar"]["$type"], "blob");
        assert_eq!(v["avatar"]["ref"]["$link"], "bafkreiavatar");
        assert_eq!(v["avatar"]["mimeType"], "image/png");
        assert_eq!(v["avatar"]["size"], 1234);
        assert_eq!(v["banner"]["ref"]["$link"], "bafkreibanner");
        assert_eq!(v["banner"]["mimeType"], "image/jpeg");
        assert_eq!(v["banner"]["size"], 5678);
        // No alt on a profile picture — the lexicon has no such field.
        assert!(v["avatar"].get("alt").is_none());
    }

    /// Clearing is the arm a create-only path gets wrong: a profile that still
    /// *has* an avatar stored, projected with `None` (because it was cleared,
    /// or because the bytes were unfetchable), must emit NO ref at all rather
    /// than leaving the old one standing.
    #[test]
    fn projection_profile_omits_cleared_or_unpublishable_pictures() {
        let with_both = profile_with_pictures(
            Some(ContentHash::of_raw(b"avatar bytes")),
            Some(ContentHash::of_raw(b"banner bytes")),
        );
        let banner = ResolvedImage {
            blob_cid: "bafkreibanner".into(),
            mime: "image/jpeg".into(),
            size_bytes: 10,
            width: None,
            height: None,
            alt: String::new(),
        };
        // Avatar unresolved, banner resolved: one absent, the other present.
        let out =
            translate_profile_for_projection(&with_both, None, Some(&banner)).expect("translate");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(
            v.get("avatar").is_none(),
            "an unresolved avatar must be absent, not null: {out}"
        );
        assert_eq!(v["banner"]["ref"]["$link"], "bafkreibanner");
        // A profile whose pictures were cleared projects with neither.
        let cleared = profile_with_pictures(None, None);
        let out = translate_profile_for_projection(&cleared, None, None).expect("translate");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("avatar").is_none());
        assert!(v.get("banner").is_none());
    }

    /// The descriptors name the two hashes the profile stores, with no
    /// declared MIME — the profile records which bytes and nothing else.
    #[test]
    fn extract_profile_media_names_both_pictures_without_a_mime() {
        let avatar_hash = ContentHash::of_raw(b"avatar bytes");
        let banner_hash = ContentHash::of_raw(b"banner bytes");
        let profile = profile_with_pictures(Some(avatar_hash), Some(banner_hash));
        let media = extract_profile_media(&profile);
        let avatar = media.avatar.expect("avatar descriptor");
        assert_eq!(avatar.blob_cid, avatar_hash.to_base32());
        assert_eq!(
            avatar.mime, "",
            "a profile picture has no declared media type; the fetcher decides"
        );
        assert_eq!(
            media.banner.expect("banner").blob_cid,
            banner_hash.to_base32()
        );

        // Each field is independent.
        let avatar_only = profile_with_pictures(Some(avatar_hash), None);
        let media = extract_profile_media(&avatar_only);
        assert!(media.avatar.is_some());
        assert!(media.banner.is_none());

        let neither = profile_with_pictures(None, None);
        assert_eq!(extract_profile_media(&neither), ProfileMedia::default());
    }

    /// End-to-end over the stored wire shape: sign_and_pack (exactly what
    /// `fauna.posts.create` stores) → translate_post_bytes_for_projection.
    #[test]
    fn projection_post_bytes_roundtrip_signed_wire() {
        use fauna_core::encoding::sign_and_pack;
        use fauna_core::identity::ActorKeypair;
        let kp = ActorKeypair::generate();
        let mut post = text_post("stored and projected", vec![]);
        post.author = kp.actor_id();
        let stored = sign_and_pack(&kp, &post).expect("sign+pack");
        let out = translate_post_bytes_for_projection(&stored, None, None, &[], None, &[])
            .expect("translate must not fail")
            .expect("this post must project");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["text"], "stored and projected");
        assert_eq!(v["createdAt"], "2026-03-20T12:00:00.000000Z");

        // Garbage bytes fail as Decode, not panic.
        let err = translate_post_bytes_for_projection(b"not a post", None, None, &[], None, &[])
            .expect_err("garbage must not decode");
        assert!(matches!(err, TranslateError::Decode(_)));
    }

    /// Same for the profile: sign_and_pack (what `fauna.profile.set` stores)
    /// → translate_profile_bytes_for_projection via the shared
    /// signed-only verify `decode_profile`.
    #[test]
    fn projection_profile_bytes_roundtrip_signed_wire() {
        use fauna_core::data::{InboxMode, Timestamp};
        use fauna_core::encoding::sign_and_pack;
        use fauna_core::identity::ActorKeypair;
        let kp = ActorKeypair::generate();
        let profile = Profile {
            actor_id: kp.actor_id(),
            display_name: Some("Bob".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        };
        let stored = sign_and_pack(&kp, &profile).expect("sign+pack");
        let out = translate_profile_bytes_for_projection(&stored, None, None).expect("translate");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["displayName"], "Bob");
        assert!(v.get("description").is_none());

        let err = translate_profile_bytes_for_projection(b"junk", None, None)
            .expect_err("garbage must not decode");
        assert!(matches!(err, TranslateError::Decode(_)));
    }

    #[test]
    fn micros_to_iso8601_full_precision() {
        assert_eq!(micros_to_iso8601(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(
            micros_to_iso8601(1_774_008_000_123_456),
            "2026-03-20T12:00:00.123456Z"
        );
    }

    // ── Reference extraction (S5 slice 3) ──────────────────────────────────

    /// A post id whose digest is a recognisable byte fill.
    fn post_id_of(fill: u8) -> fauna_core::data::PostId {
        fauna_core::data::PostId::of_dag_cbor(&[fill])
    }

    fn with_refs(refs: Vec<Reference>) -> Post {
        let mut post = text_post("body", vec![]);
        post.references = refs;
        post
    }

    #[test]
    fn extracts_reply_and_quote_as_post_map_keys() {
        let parent = post_id_of(1);
        let quoted = post_id_of(2);
        let post = with_refs(vec![
            Reference::Reply { post_id: parent },
            Reference::Quote { post_id: quoted },
        ]);
        let refs = extract_projection_refs(&post);
        // The hex is the CID *digest*, which is what post_map keys on — not
        // the 36-byte CID.
        assert_eq!(
            refs.reply_parent.as_deref(),
            Some(hex::encode(parent.digest()).as_str())
        );
        assert_eq!(
            refs.quote.as_deref(),
            Some(hex::encode(quoted.digest()).as_str())
        );
        assert_eq!(
            refs.reply_parent.as_ref().unwrap().len(),
            64,
            "32-byte digest hex"
        );
    }

    #[test]
    fn a_post_with_no_references_extracts_nothing() {
        assert_eq!(
            extract_projection_refs(&with_refs(vec![])),
            ProjectionRefs::default()
        );
    }

    /// Repost/react/upvote/downvote have no `app.bsky.feed.post` field and must
    /// never be mistaken for a reply or a quote.
    #[test]
    fn non_reply_non_quote_references_are_ignored() {
        let post = with_refs(vec![
            Reference::Repost {
                post_id: post_id_of(3),
            },
            Reference::Upvote {
                post_id: post_id_of(4),
            },
            Reference::Downvote {
                post_id: post_id_of(5),
            },
            Reference::React {
                post_id: post_id_of(6),
                emoji: "🎉".into(),
            },
        ]);
        assert_eq!(extract_projection_refs(&post), ProjectionRefs::default());
    }

    /// The lexicon has one `reply` and one `embed.record`, so the first of each
    /// kind wins and later duplicates are dropped rather than overwriting.
    #[test]
    fn first_reference_of_each_kind_wins() {
        let first_parent = post_id_of(1);
        let first_quote = post_id_of(2);
        let post = with_refs(vec![
            Reference::Reply {
                post_id: first_parent,
            },
            Reference::Quote {
                post_id: first_quote,
            },
            Reference::Reply {
                post_id: post_id_of(8),
            },
            Reference::Quote {
                post_id: post_id_of(9),
            },
        ]);
        let refs = extract_projection_refs(&post);
        assert_eq!(refs.reply_parent, Some(hex::encode(first_parent.digest())));
        assert_eq!(refs.quote, Some(hex::encode(first_quote.digest())));
    }

    /// Extraction shares `Post::decode_resolved_bytes` with the translators, so
    /// a bare canonical post round-trips through the bytes entry point.
    #[test]
    fn extracts_from_bare_canonical_post_bytes() {
        let parent = post_id_of(1);
        let post = with_refs(vec![Reference::Reply { post_id: parent }]);
        let bytes = fauna_core::encoding::canonical_encode(&post).expect("encode");
        let refs = extract_projection_refs_from_bytes(&bytes).expect("extract");
        assert_eq!(refs.reply_parent, Some(hex::encode(parent.digest())));
        assert_eq!(refs.quote, None);
    }

    #[test]
    fn extraction_rejects_bytes_that_are_not_a_post() {
        let err = extract_projection_refs_from_bytes(b"not a post").expect_err("must not decode");
        assert!(matches!(err, TranslateError::Decode(_)), "got {err:?}");
    }
}

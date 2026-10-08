//! Reverse translation: an external app's ATProto record → the intermediate
//! Fauna shape (`docs/goal/behavior/atproto-pds-full.md` § F2 detail).
//!
//! This is the **inbound authoring** direction, and it is the mirror of
//! [`crate::outbound`], not of [`crate::translate`]:
//!
//! | Module | Direction | Purpose |
//! |---|---|---|
//! | [`crate::outbound`] | Fauna `Post`/`Profile` → ATProto **record** | the one-way projection (repo re-derivation) |
//! | **this module** | ATProto **record** → intermediate Fauna shape | external writes coming back in (F2) |
//! | [`crate::translate`] | ATProto **view** → `Bluesky*` display types | the consume side's read path |
//!
//! # Scope: parsing only, no signing, no clock
//!
//! Everything here is pure translation. The intermediate shapes this produces
//! are *not* Fauna posts yet — the F2.2 slice-3 ingest arm builds and signs
//! the real `Post` with the D10 delegated authoring sub-key. Keeping the two
//! apart is what lets this module be exhaustively fixture-tested without any
//! key material in play.
//!
//! # Round-trip fidelity is bounded by what each side can express
//!
//! Two deliberate asymmetries, both matching the outbound direction so a
//! record that leaves Fauna and comes back does not accumulate junk:
//!
//! - **Mentions are dropped, display text kept.** Outbound refuses to emit a
//!   `#mention` because it would publish a non-consenting user's raw actor
//!   pubkey as a DID (`outbound.rs`, `network-exposure.md` § Rulings F2).
//!   Inbound is the same story from the other end: an ATProto mention carries
//!   a `did`, and there is no resolver from a DID to the Fauna actor pubkey
//!   `FacetFeature::Mention` requires. Inventing one would attribute a
//!   mention to the wrong account, so the facet is dropped and the text it
//!   spanned survives unchanged.
//! - **Image embeds parse into [`IntermediateImage`]s (F2.4 slice 2); other
//!   media stays unparsed.** `app.bsky.embed.images` and the media half of
//!   `app.bsky.embed.recordWithMedia` yield the blob CIDs, per-image alt and
//!   declared aspect ratio the ingest arm resolves against the `uploadBlob`
//!   ledger. A video or external (link-card) embed still parses to a post with
//!   text and facets intact — whether the *record* may commit at all is the
//!   nest's blob-existence pre-flight, not this module's (parsing stays
//!   infallible; the arm taxonomy lives in `atproto-pds-full.md` § F2 detail).
//!
//! # Alt text crosses lossily, symmetric with outbound
//!
//! `PostBody::Media` carries ONE `alt_text` for the whole set, so outbound
//! puts it on the first image only (`outbound::extract_projection_media`).
//! Inbound is the exact inverse: the FIRST image's `alt` becomes `alt_text`,
//! and alts on images 2..n drop. `TextWithMedia` has no alt field at all, so a
//! captioned text-plus-images post drops every alt — the same asymmetry
//! outbound already has, read backwards.

use atrium_api::app::bsky::actor::profile::Record as ProfileRecord;
use atrium_api::app::bsky::feed::post::Record as PostRecord;
use atrium_api::app::bsky::richtext::facet::MainFeaturesItem;
use atrium_api::types::Union;
use fauna_core::data::{Facet, FacetFeature};

use crate::outbound::{QuoteRef, ReplyRefs};

/// Why a record could not be reverse-translated.
///
/// Deliberately small: a record that is *structurally* fine but carries
/// something this direction cannot express (a mention, a media embed) is not
/// an error — it translates with that part dropped, per the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReverseTranslateError {
    /// The bytes are not dag-cbor, or not the expected record lexicon.
    Decode(String),
    /// A required field was present but unusable (e.g. an unparseable
    /// `createdAt`).
    Field(String),
}

impl std::fmt::Display for ReverseTranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(msg) => write!(f, "decode: {msg}"),
            Self::Field(msg) => write!(f, "field: {msg}"),
        }
    }
}

impl std::error::Error for ReverseTranslateError {}

/// An `app.bsky.feed.post` record, reduced to what a Fauna post needs.
///
/// Mirrors the inputs [`crate::outbound::translate_post_for_projection`]
/// consumes, so the two directions stay legibly inverse.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IntermediatePost {
    /// The post text, verbatim (no truncation — the outbound 300-grapheme
    /// cap is a *lexicon* limit, and this text is already inside it).
    pub text: String,
    /// Rich-text facets whose feature Fauna can express. Byte offsets index
    /// UTF-8 bytes on both sides, so ranges cross unchanged.
    pub facets: Vec<Facet>,
    /// The thread parent + root, when the record is a reply.
    pub reply: Option<ReplyRefs>,
    /// The quoted post, when the record embeds one.
    pub quote: Option<QuoteRef>,
    /// The record's self-asserted `createdAt`, in microseconds since the
    /// Unix epoch.
    ///
    /// **Parsed, not trusted.** This is what the external client claimed; the
    /// slice-3 ingest arm decides whether the Fauna post adopts it or stamps
    /// its own creation instant. That decision matters because the projected
    /// rkey is derived from the Fauna post's `created_at` (D1), so honouring
    /// a client-supplied value lets an external app choose where its post
    /// sorts. Parsing it here keeps the choice at the layer that can weigh
    /// it, rather than silently discarding information.
    pub created_at_micros: u64,
    /// The image attachments, from either embed shape (a bare
    /// `app.bsky.embed.images` or the media half of
    /// `app.bsky.embed.recordWithMedia`), in record order.
    pub images: Vec<IntermediateImage>,
}

/// One image of a record's embed, reduced to what the ingest arm needs.
///
/// The inverse of [`crate::outbound::ResolvedImage`], minus the fields the
/// nest re-derives from its own store rather than trusting: the record's
/// declared `mimeType` and `size` are deliberately NOT carried, because the
/// `MediaItem` the arm builds takes both from the `blob_metadata` row the
/// upload's sniff wrote — the record declaring `image/jpeg` for a PNG must not
/// become what 7 apps render it as (`atproto-pds-full.md` § F2 detail, the
/// sniffed-not-declared ruling). The aspect ratio IS carried: nothing stored
/// knows it, and a native app declares its own dimensions too.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntermediateImage {
    /// The blob's CID as the record spelled it — the key the `uploadBlob`
    /// ledger (`atproto_blobs`) is resolved by.
    pub cid: String,
    /// The per-image alt text; empty when the record carried none (the
    /// lexicon's `alt` is required, so empty is its own "none").
    pub alt: String,
    /// The declared `aspectRatio`, when present and within `u32`.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// An `app.bsky.actor.profile` record, reduced to what a Fauna profile needs.
///
/// The inverse of [`crate::outbound::translate_profile_for_projection`],
/// field for field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntermediateProfile {
    /// `displayName` → `Profile::display_name`.
    pub display_name: Option<String>,
    /// `description` → `Profile::bio`.
    pub bio: Option<String>,
    /// `avatar` → `Profile::avatar`, as the ref's **ATProto** blob CID.
    ///
    /// Deliberately a CID string and not a `ContentHash`: this crate cannot
    /// resolve one to the other. The two content-address spaces are only
    /// bridged by holding the bytes, so the caller resolves the string through
    /// the two ratified resolvers — the nest's `atproto_blobs` upload ledger and
    /// the bridge blob store's Fauna-CID index (`atproto-pds-full.md` § F2
    /// detail, *a picture crosses INBOUND by resolution, never by trust*).
    ///
    /// `None` is authoritative, exactly like `display_name`: a record without an
    /// `avatar` clears the user's picture, because otherwise a picture set
    /// anywhere would be unremovable from bsky.app. No bytes are destroyed —
    /// the ref simply stops being referenced.
    pub avatar: Option<String>,
    /// `banner` → `Profile::banner`, same rules as [`Self::avatar`] and
    /// independent of it (one field's absence never speaks for the other).
    pub banner: Option<String>,
}

/// Parse the dag-cbor bytes of an `app.bsky.feed.post` record.
///
/// The bytes are the record verbatim as the bridge validated and encoded it
/// (indigo's `JSONRecordToDagCBOR`) — the same bytes the journal path stores,
/// so one wire field feeds both dispositions.
pub fn parse_post_record(dag_cbor: &[u8]) -> Result<IntermediatePost, ReverseTranslateError> {
    let record: PostRecord = serde_ipld_dagcbor::from_slice(dag_cbor)
        .map_err(|e| ReverseTranslateError::Decode(format!("app.bsky.feed.post: {e}")))?;
    intermediate_from_record(&record)
}

/// The record half of the translation, over an already-decoded
/// `app.bsky.feed.post` — what [`parse_post_record`] runs after decoding the
/// dag-cbor, and what [`crate::ingest`] runs over the `record` a `PostView`
/// carries verbatim. One function, so the two inbound directions cannot drift
/// on mentions, facets, refs or the timestamp.
pub fn intermediate_from_record(
    record: &PostRecord,
) -> Result<IntermediatePost, ReverseTranslateError> {
    let created_at_micros = parse_rfc3339_micros(record.created_at.as_str())?;

    Ok(IntermediatePost {
        text: record.text.clone(),
        facets: parse_facets(record.facets.as_deref()),
        reply: record.reply.as_ref().map(|r| ReplyRefs {
            parent_uri: r.parent.uri.clone(),
            parent_cid: r.parent.cid.as_ref().to_string(),
            root_uri: r.root.uri.clone(),
            root_cid: r.root.cid.as_ref().to_string(),
        }),
        quote: parse_quote(record),
        created_at_micros,
        images: parse_images(record),
    })
}

/// Parse the dag-cbor bytes of an `app.bsky.actor.profile` record.
pub fn parse_profile_record(dag_cbor: &[u8]) -> Result<IntermediateProfile, ReverseTranslateError> {
    let record: ProfileRecord = serde_ipld_dagcbor::from_slice(dag_cbor)
        .map_err(|e| ReverseTranslateError::Decode(format!("app.bsky.actor.profile: {e}")))?;

    Ok(IntermediateProfile {
        display_name: record.display_name.clone(),
        bio: record.description.clone(),
        avatar: record.avatar.as_ref().map(blob_ref_cid),
        banner: record.banner.as_ref().map(blob_ref_cid),
    })
}

/// Translate ATProto facets into Fauna [`Facet`]s, dropping features Fauna
/// cannot express (see the module docs on mentions).
fn parse_facets(facets: Option<&[atrium_api::app::bsky::richtext::facet::Main]>) -> Vec<Facet> {
    let Some(facets) = facets else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for facet in facets {
        for feature in &facet.features {
            let parsed = match feature {
                Union::Refs(MainFeaturesItem::Link(l)) => FacetFeature::Link { uri: l.uri.clone() },
                Union::Refs(MainFeaturesItem::Tag(t)) => FacetFeature::Tag {
                    name: t.tag.clone(),
                },
                // A mention cannot be resolved to a Fauna actor; an unknown
                // feature is an open-world lexicon extension. Both drop the
                // facet and keep the text.
                Union::Refs(MainFeaturesItem::Mention(_)) | Union::Unknown(_) => continue,
            };
            out.push(Facet {
                byte_start: facet.index.byte_start as u32,
                byte_end: facet.index.byte_end as u32,
                feature: parsed,
            });
        }
    }
    out
}

/// Extract a quoted post from the record's embed, if it is a record embed.
///
/// Handles both `app.bsky.embed.record` (a bare quote) and
/// `app.bsky.embed.recordWithMedia` (quote + media — the media half is
/// [`parse_images`]' job, the quote half is kept here).
fn parse_quote(record: &PostRecord) -> Option<QuoteRef> {
    use atrium_api::app::bsky::feed::post::RecordEmbedRefs;

    match record.embed.as_ref()? {
        Union::Refs(RecordEmbedRefs::AppBskyEmbedRecordMain(r)) => Some(QuoteRef {
            uri: r.record.uri.clone(),
            cid: r.record.cid.as_ref().to_string(),
        }),
        Union::Refs(RecordEmbedRefs::AppBskyEmbedRecordWithMediaMain(r)) => Some(QuoteRef {
            uri: r.record.record.uri.clone(),
            cid: r.record.record.cid.as_ref().to_string(),
        }),
        _ => None,
    }
}

/// Extract the image attachments from whichever of the two embed shapes
/// carries them — the same two-shape dispatch as [`parse_quote`], because a
/// walker that learned only the bare `app.bsky.embed.images` would silently
/// drop the media half of every quote-with-media post (the exact trap
/// `record_refs` was written against, one field over).
fn parse_images(record: &PostRecord) -> Vec<IntermediateImage> {
    use atrium_api::app::bsky::embed::record_with_media::MainMediaRefs;
    use atrium_api::app::bsky::feed::post::RecordEmbedRefs;

    let images = match record.embed.as_ref() {
        Some(Union::Refs(RecordEmbedRefs::AppBskyEmbedImagesMain(m))) => &m.images,
        Some(Union::Refs(RecordEmbedRefs::AppBskyEmbedRecordWithMediaMain(m))) => {
            match &m.media {
                Union::Refs(MainMediaRefs::AppBskyEmbedImagesMain(im)) => &im.images,
                // A video or unknown media half carries no images; whether the
                // record itself may commit is the nest's blob pre-flight.
                _ => return Vec::new(),
            }
        }
        _ => return Vec::new(),
    };

    images
        .iter()
        .map(|img| {
            // An aspect ratio outside u32 is a nonsense declaration; drop the
            // pair rather than half of it (a width without a height would
            // render as a divide-by-guess downstream).
            let (width, height) = img
                .aspect_ratio
                .as_ref()
                .and_then(|ar| {
                    Some((
                        u32::try_from(u64::from(ar.width)).ok()?,
                        u32::try_from(u64::from(ar.height)).ok()?,
                    ))
                })
                .map_or((None, None), |(w, h)| (Some(w), Some(h)));
            IntermediateImage {
                cid: blob_ref_cid(&img.image),
                alt: img.alt.clone(),
                width,
                height,
            }
        })
        .collect()
}

/// The CID a blob ref names, as a string, for either wire form: the typed
/// `blob` shape (a real CID link) or the legacy untyped `{cid, mimeType}`
/// form old records carry.
fn blob_ref_cid(blob: &atrium_api::types::BlobRef) -> String {
    match blob {
        atrium_api::types::BlobRef::Typed(atrium_api::types::TypedBlobRef::Blob(b)) => {
            b.r#ref.0.to_string()
        }
        atrium_api::types::BlobRef::Untyped(u) => u.cid.clone(),
    }
}

/// Parse an RFC 3339 timestamp into microseconds since the Unix epoch.
///
/// Same call and same purpose as the sibling bridge's
/// `fauna_bridge_activitypub::translate::parse_iso8601_to_timestamp`. Not
/// lifted into `fauna-core` on purpose: that crate keeps `chrono` off its
/// default surface deliberately (it is wasm-safe by construction), and this
/// module is `client`-gated where `chrono` is already free.
fn parse_rfc3339_micros(s: &str) -> Result<u64, ReverseTranslateError> {
    let dt = chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| ReverseTranslateError::Field(format!("createdAt {s:?}: {e}")))?;
    let secs = dt.timestamp();
    if secs < 0 {
        return Err(ReverseTranslateError::Field(format!(
            "createdAt {s:?} predates the Unix epoch"
        )));
    }
    Ok(secs as u64 * 1_000_000 + dt.timestamp_subsec_micros() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dag_cbor, test_cid};
    use serde_json::json;

    fn post_json(extra: serde_json::Value) -> serde_json::Value {
        crate::test_support::post_json(extra)
    }

    #[test]
    fn a_plain_post_round_trips_text_and_created_at() {
        let parsed = parse_post_record(&dag_cbor(post_json(json!({})))).unwrap();
        assert_eq!(parsed.text, "hello from an external app");
        assert!(parsed.facets.is_empty());
        assert_eq!(parsed.reply, None);
        assert_eq!(parsed.quote, None);
        // 2026-07-24T10:30:00Z
        assert_eq!(parsed.created_at_micros, 1_784_889_000_000_000);
    }

    #[test]
    fn link_and_tag_facets_cross_with_their_byte_ranges_intact() {
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "facets": [
                {
                    "index": { "byteStart": 0, "byteEnd": 5 },
                    "features": [{ "$type": "app.bsky.richtext.facet#link", "uri": "https://example.com" }],
                },
                {
                    "index": { "byteStart": 6, "byteEnd": 11 },
                    "features": [{ "$type": "app.bsky.richtext.facet#tag", "tag": "fauna" }],
                },
            ],
        }))))
        .unwrap();

        assert_eq!(parsed.facets.len(), 2);
        assert_eq!(
            (parsed.facets[0].byte_start, parsed.facets[0].byte_end),
            (0, 5)
        );
        assert_eq!(
            parsed.facets[0].feature,
            FacetFeature::Link {
                uri: "https://example.com".to_string()
            }
        );
        assert_eq!(
            (parsed.facets[1].byte_start, parsed.facets[1].byte_end),
            (6, 11)
        );
        assert_eq!(
            parsed.facets[1].feature,
            FacetFeature::Tag {
                name: "fauna".to_string()
            }
        );
    }

    /// The asymmetry the module docs promise: a mention drops, and it drops
    /// *without* taking the text or its sibling facets with it.
    #[test]
    fn a_mention_facet_is_dropped_but_the_text_and_other_facets_survive() {
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "text": "hi @someone and https://example.com",
            "facets": [
                {
                    "index": { "byteStart": 3, "byteEnd": 11 },
                    "features": [{ "$type": "app.bsky.richtext.facet#mention", "did": "did:plc:abc123" }],
                },
                {
                    "index": { "byteStart": 16, "byteEnd": 35 },
                    "features": [{ "$type": "app.bsky.richtext.facet#link", "uri": "https://example.com" }],
                },
            ],
        }))))
        .unwrap();

        assert_eq!(parsed.text, "hi @someone and https://example.com");
        assert_eq!(
            parsed.facets.len(),
            1,
            "only the mention drops, got {:?}",
            parsed.facets
        );
        assert!(matches!(
            parsed.facets[0].feature,
            FacetFeature::Link { .. }
        ));
    }

    #[test]
    fn a_reply_carries_both_parent_and_root_refs() {
        let parent_cid = test_cid(1);
        let root_cid = test_cid(2);
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "reply": {
                "parent": { "uri": "at://did:plc:p/app.bsky.feed.post/3parent", "cid": parent_cid },
                "root":   { "uri": "at://did:plc:r/app.bsky.feed.post/3root",   "cid": root_cid },
            },
        }))))
        .unwrap();

        let reply = parsed.reply.expect("reply parsed");
        assert_eq!(
            reply.parent_uri,
            "at://did:plc:p/app.bsky.feed.post/3parent"
        );
        assert_eq!(reply.parent_cid, parent_cid);
        assert_eq!(reply.root_uri, "at://did:plc:r/app.bsky.feed.post/3root");
        assert_eq!(reply.root_cid, root_cid);
    }

    #[test]
    fn a_record_embed_parses_as_a_quote() {
        let cid = test_cid(3);
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "embed": {
                "$type": "app.bsky.embed.record",
                "record": { "uri": "at://did:plc:q/app.bsky.feed.post/3quoted", "cid": cid },
            },
        }))))
        .unwrap();

        let quote = parsed.quote.expect("quote parsed");
        assert_eq!(quote.uri, "at://did:plc:q/app.bsky.feed.post/3quoted");
        assert_eq!(quote.cid, cid);
    }

    /// F2.4 slice 2: a bare `app.bsky.embed.images` yields the images — CID,
    /// per-image alt, declared aspect ratio — and is not a quote.
    #[test]
    fn an_images_embed_parses_cid_alt_and_aspect_ratio() {
        let blob_cid = test_cid(4);
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "embed": {
                "$type": "app.bsky.embed.images",
                "images": [{
                    "alt": "a picture",
                    "image": {
                        "$type": "blob",
                        "ref": { "/": blob_cid },
                        "mimeType": "image/jpeg",
                        "size": 1234,
                    },
                    "aspectRatio": { "width": 640, "height": 480 },
                }],
            },
        }))))
        .unwrap();

        assert_eq!(parsed.text, "hello from an external app");
        assert_eq!(parsed.quote, None, "an image embed is not a quote");
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].cid, blob_cid);
        assert_eq!(parsed.images[0].alt, "a picture");
        assert_eq!(
            (parsed.images[0].width, parsed.images[0].height),
            (Some(640), Some(480))
        );
    }

    /// The two-shape trap `record_refs` documents, on the media axis: a
    /// quote-with-media record must yield BOTH the quote and the images, or
    /// every quote-with-media post silently loses its pictures.
    #[test]
    fn a_record_with_media_embed_yields_both_the_quote_and_the_images() {
        let quote_cid = test_cid(5);
        let blob_cid = test_cid(6);
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "embed": {
                "$type": "app.bsky.embed.recordWithMedia",
                "record": {
                    "$type": "app.bsky.embed.record",
                    "record": { "uri": "at://did:plc:q/app.bsky.feed.post/3quoted", "cid": quote_cid },
                },
                "media": {
                    "$type": "app.bsky.embed.images",
                    "images": [{
                        "alt": "",
                        "image": {
                            "$type": "blob",
                            "ref": { "/": blob_cid },
                            "mimeType": "image/png",
                            "size": 99,
                        },
                    }],
                },
            },
        }))))
        .unwrap();

        let quote = parsed.quote.expect("quote survives alongside media");
        assert_eq!(quote.uri, "at://did:plc:q/app.bsky.feed.post/3quoted");
        assert_eq!(parsed.images.len(), 1, "the media half must parse too");
        assert_eq!(parsed.images[0].cid, blob_cid);
        assert_eq!(parsed.images[0].alt, "");
        assert_eq!(
            (parsed.images[0].width, parsed.images[0].height),
            (None, None)
        );
    }

    /// A missing aspectRatio is `None`, never an invented shape — the same
    /// stance outbound takes emitting one only when dimensions are known.
    #[test]
    fn a_video_embed_yields_no_images_and_still_parses() {
        let blob_cid = test_cid(7);
        let parsed = parse_post_record(&dag_cbor(post_json(json!({
            "embed": {
                "$type": "app.bsky.embed.video",
                "video": {
                    "$type": "blob",
                    "ref": { "/": blob_cid },
                    "mimeType": "video/mp4",
                    "size": 5000,
                },
            },
        }))))
        .unwrap();

        assert_eq!(parsed.text, "hello from an external app");
        assert!(parsed.images.is_empty(), "a video is not an image");
    }

    #[test]
    fn a_profile_record_maps_display_name_and_description() {
        let parsed = parse_profile_record(&dag_cbor(json!({
            "$type": "app.bsky.actor.profile",
            "displayName": "Ada",
            "description": "builds things",
        })))
        .unwrap();

        assert_eq!(parsed.display_name.as_deref(), Some("Ada"));
        assert_eq!(parsed.bio.as_deref(), Some("builds things"));
        assert_eq!(parsed.avatar, None, "no avatar ref means no avatar");
        assert_eq!(parsed.banner, None);
    }

    /// The two picture fields are read out as their **ATProto** blob CIDs, and
    /// they are independent: a record may carry either, both or neither.
    #[test]
    fn a_profile_record_maps_avatar_and_banner_to_their_atproto_cids() {
        let avatar_cid = test_cid(11);
        let banner_cid = test_cid(12);
        let parsed = parse_profile_record(&dag_cbor(json!({
            "$type": "app.bsky.actor.profile",
            "displayName": "Ada",
            "avatar": {
                "$type": "blob",
                "ref": { "/": avatar_cid },
                "mimeType": "image/jpeg",
                "size": 1234,
            },
            "banner": {
                "$type": "blob",
                "ref": { "/": banner_cid },
                "mimeType": "image/png",
                "size": 5678,
            },
        })))
        .unwrap();

        assert_eq!(parsed.avatar.as_deref(), Some(avatar_cid.as_str()));
        assert_eq!(parsed.banner.as_deref(), Some(banner_cid.as_str()));
    }

    /// Only-a-banner is not only-an-avatar: absence of one field never speaks
    /// for the other (`atproto-pds-bridge.md:112`'s per-field independence,
    /// read inbound).
    #[test]
    fn a_profile_record_with_only_a_banner_leaves_the_avatar_none() {
        let banner_cid = test_cid(13);
        let parsed = parse_profile_record(&dag_cbor(json!({
            "$type": "app.bsky.actor.profile",
            "banner": {
                "$type": "blob",
                "ref": { "/": banner_cid },
                "mimeType": "image/png",
                "size": 10,
            },
        })))
        .unwrap();

        assert_eq!(parsed.avatar, None);
        assert_eq!(parsed.banner.as_deref(), Some(banner_cid.as_str()));
    }

    /// The generic blob walk the nest stamps and gates from MUST see every
    /// picture this parse reads — the profile twin of
    /// `record_refs::the_blob_walk_covers_every_image_reverse_translate_parses`.
    /// A miss here is not cosmetic: an unstamped `atproto_blobs` row reads as
    /// unreferenced to the F2.4 GC, which is deleted user media.
    #[test]
    fn the_blob_walk_covers_every_picture_the_profile_parse_reads() {
        let avatar_cid = test_cid(14);
        let banner_cid = test_cid(15);
        let bytes = dag_cbor(json!({
            "$type": "app.bsky.actor.profile",
            "avatar": {
                "$type": "blob",
                "ref": { "/": avatar_cid },
                "mimeType": "image/jpeg",
                "size": 1,
            },
            "banner": {
                "$type": "blob",
                "ref": { "/": banner_cid },
                "mimeType": "image/png",
                "size": 2,
            },
        }));

        let parsed = parse_profile_record(&bytes).unwrap();
        let walked = crate::record_refs::external_record_blob_refs(&bytes);
        for cid in [parsed.avatar.unwrap(), parsed.banner.unwrap()] {
            assert!(
                walked.contains(&cid),
                "the parse reads {cid} but the walk missed it: {walked:?}"
            );
        }
    }

    /// **The shape production actually carries.** The bridge encodes a caller's
    /// record with indigo's `atdata` codec, which stores a blob's `ref` as the
    /// literal map `{"$link": "<cid>"}` rather than a dag-cbor tag-42 link. The
    /// parse and the walk must BOTH read it — they disagreed until 2026-07-30,
    /// and the walk's half of that disagreement is the finding-42 data-loss
    /// class (see `record_refs::link_cid`).
    ///
    /// Every fixture above builds tag-42 links because that is what
    /// `json_to_ipld` makes convenient; this one deliberately does not.
    #[test]
    fn a_dollar_link_blob_ref_is_read_by_both_the_parse_and_the_walk() {
        let cid = test_cid(21);
        let bytes = dag_cbor(json!({
            "$type": "app.bsky.actor.profile",
            "avatar": {
                "$type": "blob",
                // NOT `{"/": …}` — the map form indigo really writes.
                "ref": { "$link": cid.clone() },
                "mimeType": "image/png",
                "size": 1,
            },
        }));

        assert_eq!(
            parse_profile_record(&bytes).unwrap().avatar.as_deref(),
            Some(cid.as_str()),
        );
        assert_eq!(
            crate::record_refs::external_record_blob_refs(&bytes),
            vec![cid],
            "the walk must see the ref the parse sees — an unstamped \
             `atproto_blobs` row is deleted user media",
        );
    }

    #[test]
    fn an_empty_profile_record_parses_to_all_none() {
        let parsed = parse_profile_record(&dag_cbor(json!({
            "$type": "app.bsky.actor.profile",
        })))
        .unwrap();

        assert_eq!(parsed, IntermediateProfile::default());
    }

    #[test]
    fn non_dag_cbor_bytes_are_a_decode_error_not_a_panic() {
        let err = parse_post_record(b"not cbor at all").unwrap_err();
        assert!(
            matches!(err, ReverseTranslateError::Decode(_)),
            "got {err:?}"
        );
    }

    /// A malformed `createdAt` must never reach the ingest arm as a silently
    /// zeroed timestamp. It is rejected at *decode*, not by
    /// [`parse_rfc3339_micros`]: atrium's `Datetime` newtype validates the
    /// RFC 3339 format while deserializing, so by the time this module reads
    /// the field it is already well-formed. That makes the `Field` arm
    /// defense-in-depth rather than the live path — which is exactly why this
    /// asserts "rejected", not "rejected by a particular layer".
    #[test]
    fn a_malformed_created_at_is_rejected_rather_than_defaulted() {
        assert!(
            parse_post_record(&dag_cbor(json!({
                "$type": "app.bsky.feed.post",
                "text": "hi",
                "createdAt": "last tuesday",
            })))
            .is_err(),
            "a garbage createdAt must not parse"
        );
    }

    /// The forward direction stamps `createdAt` with microsecond precision
    /// (`outbound::micros_to_iso8601`); parsing must not silently round it
    /// away, or a Fauna post that leaves and returns would shift in time.
    #[test]
    fn microsecond_precision_survives_the_round_trip_from_outbound() {
        let micros = 1_784_896_200_123_456u64;
        let iso = crate::outbound::micros_to_iso8601(micros);
        let parsed = parse_post_record(&dag_cbor(json!({
            "$type": "app.bsky.feed.post",
            "text": "precise",
            "createdAt": iso,
        })))
        .unwrap();
        assert_eq!(parsed.created_at_micros, micros);
    }

    /// A non-UTC offset is a legal RFC 3339 timestamp and real clients emit
    /// them; the parsed instant must be the same absolute moment.
    #[test]
    fn a_non_utc_offset_normalizes_to_the_same_instant() {
        let utc = parse_post_record(&dag_cbor(json!({
            "$type": "app.bsky.feed.post", "text": "t",
            "createdAt": "2026-07-24T10:30:00Z",
        })))
        .unwrap();
        let offset = parse_post_record(&dag_cbor(json!({
            "$type": "app.bsky.feed.post", "text": "t",
            "createdAt": "2026-07-24T12:30:00+02:00",
        })))
        .unwrap();
        assert_eq!(utc.created_at_micros, offset.created_at_micros);
    }
}

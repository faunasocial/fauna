//! Which already-existing records does an external write point at?
//!
//! An external app's `app.bsky.feed.post` names its reply parent and its
//! quoted post by **AT-URI**. A Fauna [`Reference`] names them by **Fauna post
//! id**, and nothing resolves one to the other nest-side: the projected rkey
//! derivation is one-way, and the reverse index is the bridge's own `post_map`
//! (`atproto-pds-full.md` § F2 detail). So the bridge resolves the URIs before
//! the nest call and sends the resolutions alongside the record
//! (`ExternalWrite.resolved_targets`).
//!
//! [`Reference`]: fauna_core::data::Reference
//!
//! # Why the extraction lives here, in Rust, in the always-on core
//!
//! The bridge needs to know *which* URIs to resolve, so something must read
//! them out of the record. Three properties decided the placement (F2.2 slice
//! 4b):
//!
//! - **Not Go.** A quote reaches the record through **two** shapes —
//!   `app.bsky.embed.record` and `app.bsky.embed.recordWithMedia`, the latter
//!   nesting one level deeper — and [`crate::reverse_translate::parse_post_record`]
//!   consumes both. A hand-rolled Go walker that learned only the first would
//!   silently fail to resolve every quote-with-media, and the nest would
//!   journal a post that should have round-tripped. Silent degradation, not a
//!   loud failure, which is the worst kind.
//! - **Not [`crate::reverse_translate`].** That module is `client`-gated and
//!   deliberately has no FFI export (F2.1). Exporting it would drag
//!   `atrium-api` into the FFI cdylib, and `atrium-api` carries a 184-crate
//!   subtree including tokio, regex and http — exactly the async client stack
//!   `fauna-ffi`'s `default-features = false` exists to keep out.
//! - **So: here.** A pure, always-on, wasm-clean module that walks the raw
//!   dag-cbor with the *same decoder* `reverse_translate` uses, and nothing
//!   else. The two therefore cannot disagree about the **bytes**; they could
//!   in principle disagree about the **paths**, which is what
//!   `extraction_agrees_with_reverse_translate` pins on a shared fixture
//!   corpus.
//!
//! # The bridge is a resolver, not an interpreter
//!
//! This returns a flat list of AT-URIs with no indication of which is the
//! reply parent and which is the quote. That is deliberate: the bridge's job
//! is to answer "what Fauna post, if any, is at this URI", and the nest —
//! which parses the record anyway — is the only side that needs to know what
//! each reference *means*. Keeping reference *kinds* out of the bridge is why
//! the wire carries a map rather than two named fields, and why adding a third
//! reference kind later touches neither the bridge nor the wire.

use ipld_core::ipld::Ipld;

/// The most references one record may carry.
///
/// A post has at most one reply parent and at most one quote, so two is the
/// real ceiling; the cap is the defensive bound on a hostile record that
/// nests embeds, not a design limit. The nest re-checks it on
/// `resolved_targets` — a bridge is trusted but a cap the receiver does not
/// enforce is not a cap.
pub const MAX_RECORD_REFS: usize = 4;

/// `$type` of the bare record embed (a quote).
const EMBED_RECORD: &str = "app.bsky.embed.record";
/// `$type` of the record-plus-media embed (a quote alongside images/video).
const EMBED_RECORD_WITH_MEDIA: &str = "app.bsky.embed.recordWithMedia";

/// Extract every AT-URI this record refers to, in a stable order: the reply
/// parent first, then the quoted post.
///
/// Never errors. Bytes that do not decode, or a record shaped in a way this
/// does not recognize, yield no references — and no references means the nest
/// resolves nothing and journals, which is the ratified fallback
/// (`atproto-pds-full.md` § F2 detail: *A reply or quote whose target is not a
/// Fauna post JOURNALS*). Refusing here instead would turn an unreadable
/// embed into a failed user write.
pub fn external_record_refs(dag_cbor: &[u8]) -> Vec<String> {
    let Ok(record) = serde_ipld_dagcbor::from_slice::<Ipld>(dag_cbor) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    if let Some(uri) = reply_parent_uri(&record) {
        push_ref(&mut out, uri);
    }
    if let Some(uri) = quoted_uri(&record) {
        push_ref(&mut out, uri);
    }
    out
}

/// `reply.parent.uri` — the post this record answers.
///
/// The thread `root` is deliberately **not** extracted. Fauna's [`Reference`]
/// vocabulary has no thread-root concept (`Reply { post_id }` names the parent
/// only), so resolving the root would cost a lookup whose answer nothing
/// consumes. The root still matters bridge-side, where the funnel records it
/// in `post_map` so a later reply resolves its own root in one lookup — but
/// that is the *projection* direction and reads the Fauna post, not this
/// record.
///
/// [`Reference`]: fauna_core::data::Reference
fn reply_parent_uri(record: &Ipld) -> Option<&str> {
    string_at(field(field(field(record, "reply")?, "parent")?, "uri")?)
}

/// The quoted post's URI, from whichever of the two embed shapes carries one.
///
/// Dispatching on `$type` mirrors how [`crate::reverse_translate`] resolves
/// the same union (atrium deserializes it by `$type`), so the two agree by
/// construction on which shape is which. An unrecognized `$type` — a media-only
/// embed, or an open-world lexicon extension — yields no quote.
fn quoted_uri(record: &Ipld) -> Option<&str> {
    let embed = field(record, "embed")?;
    match string_at(field(embed, "$type")?)? {
        // { embed: { $type: …record, record: { uri, cid } } }
        EMBED_RECORD => string_at(field(field(embed, "record")?, "uri")?),
        // { embed: { $type: …recordWithMedia, record: { record: { uri, cid } } } }
        EMBED_RECORD_WITH_MEDIA => {
            string_at(field(field(field(embed, "record")?, "record")?, "uri")?)
        }
        _ => None,
    }
}

/// The most blob refs one record may carry.
///
/// An honest record sits far below this (four embed images plus a link-card
/// thumb); the cap is a defensive bound on a hostile record that nests blob
/// shapes, not a design limit. Past it the walk stops collecting — see
/// [`external_record_blob_refs`] for why a miss here only hurts the record's
/// own author.
pub const MAX_RECORD_BLOB_REFS: usize = 16;

/// Extract every blob CID this record references, in walk order, deduplicated.
///
/// A blob ref is the data-model `blob` shape — a map with `$type: "blob"`
/// whose `ref` is a CID link — or the legacy untyped `{cid, mimeType}` form
/// old records carry. The walk is **generic over the whole record** rather
/// than shaped per-embed on purpose: `app.bsky.embed.images`, the media half
/// of `recordWithMedia`, a video blob, a link card's `thumb`, and any
/// journal-collection lexicon all carry the same shape, and a walker that
/// learned only the embeds it knows would silently miss the rest. A missed
/// ref is not cosmetic: the nest stamps `atproto_blobs.referenced_at` from
/// this walk, and an unstamped row reads as unreferenced to the F2.4 GC — a
/// miss becomes deleted user media.
///
/// Never errors (the [`external_record_refs`] contract): bytes that do not
/// decode yield no refs. That best-effort stance is safe because every
/// consequence of a miss lands on the record's own author — an unannounced
/// `#commit` blob, an unstamped row, an existence check not run — never on
/// another account.
pub fn external_record_blob_refs(dag_cbor: &[u8]) -> Vec<String> {
    let Ok(record) = serde_ipld_dagcbor::from_slice::<Ipld>(dag_cbor) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_blob_refs(&record, &mut out);
    out
}

fn walk_blob_refs(value: &Ipld, out: &mut Vec<String>) {
    match value {
        Ipld::Map(map) => {
            if let Some(cid) = blob_shape_cid(value)
                && !cid.is_empty()
                && out.len() < MAX_RECORD_BLOB_REFS
                && !out.iter().any(|c| c == &cid)
            {
                out.push(cid);
            }
            for v in map.values() {
                walk_blob_refs(v, out);
            }
        }
        Ipld::List(items) => {
            for v in items {
                walk_blob_refs(v, out);
            }
        }
        _ => {}
    }
}

/// The CID a `blob`-shaped map names, in either wire form.
fn blob_shape_cid(value: &Ipld) -> Option<String> {
    match field(value, "$type") {
        // The modern typed form: { $type: "blob", ref: <link>, mimeType, size }.
        Some(Ipld::String(t)) if t == "blob" => link_cid(field(value, "ref")?),
        Some(_) => None,
        // The legacy untyped form: { cid: "<string>", mimeType: "<string>" } —
        // matched only when BOTH keys hold strings, so an ordinary map with a
        // stray `cid` field does not read as a blob.
        None => match (field(value, "cid"), field(value, "mimeType")) {
            (Some(Ipld::String(cid)), Some(Ipld::String(_))) => Some(cid.clone()),
            _ => None,
        },
    }
}

/// The CID a blob ref's `ref` position names, in **both** dag-cbor spellings.
///
/// A dag-cbor CID link is a tag-42 [`Ipld::Link`], and that is the only form
/// this understood until 2026-07-30 — which made the whole walk a silent no-op
/// on every record an external app actually wrote. The bridge encodes a caller's
/// record with indigo's `atdata` codec, and that codec round-trips a blob's
/// `ref` as the **ATProto JSON link form kept as a literal map** —
/// `{"$link": "<cid string>"}` — not as a tag-42 link. Verified by decoding what
/// `JSONRecordToDagCBOR` emits; the cross-binary pin below keeps it verified.
///
/// The cost of the miss was not cosmetic and not hypothetical: the nest stamps
/// `atproto_blobs.referenced_at` from this walk, an unstamped row is swept after
/// the 7-day window, and the box-wide GC then reclaims the bytes — so **every
/// image an external app uploaded was scheduled for deletion**, the second
/// sighting of the finding-42 class. The `#commit` frame's `blobs` field was
/// likewise always empty. Every unit test passed throughout, because the
/// fixtures built tag-42 links: a shape production never produces.
///
/// So: accept both, and never assume the in-memory IPLD shape a fixture is
/// convenient to build is the shape the wire carries.
fn link_cid(value: &Ipld) -> Option<String> {
    match value {
        Ipld::Link(cid) => Some(cid.to_string()),
        // `{"$link": "<cid>"}` — the form indigo's atdata codec stores.
        // `{"$link": "<cid>"}` — the form indigo's atdata codec stores.
        Ipld::Map(_) => match field(value, "$link")? {
            Ipld::String(cid) if !cid.is_empty() => Some(cid.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Read a map field, or `None` when the value is not a map / the key is absent.
fn field<'a>(value: &'a Ipld, key: &str) -> Option<&'a Ipld> {
    match value {
        Ipld::Map(map) => map.get(key),
        _ => None,
    }
}

/// Read a string value, or `None` when it is any other IPLD kind.
fn string_at(value: &Ipld) -> Option<&str> {
    match value {
        Ipld::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Append a reference, skipping empties and duplicates and honouring the cap.
fn push_ref(out: &mut Vec<String>, uri: &str) {
    if uri.is_empty() || out.len() >= MAX_RECORD_REFS || out.iter().any(|u| u == uri) {
        return;
    }
    out.push(uri.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dag_cbor, test_cid};
    use serde_json::json;

    fn strong_ref(uri: &str, seed: u8) -> serde_json::Value {
        json!({ "uri": uri, "cid": test_cid(seed) })
    }

    fn post_json(extra: serde_json::Value) -> serde_json::Value {
        crate::test_support::post_json(extra)
    }

    const PARENT: &str = "at://did:plc:parent/app.bsky.feed.post/3kparent";
    const ROOT: &str = "at://did:plc:root/app.bsky.feed.post/3kroot";
    const QUOTED: &str = "at://did:plc:quoted/app.bsky.feed.post/3kquoted";

    fn reply_post() -> serde_json::Value {
        post_json(json!({
            "reply": { "parent": strong_ref(PARENT, 1), "root": strong_ref(ROOT, 2) },
        }))
    }

    fn quote_post() -> serde_json::Value {
        post_json(json!({
            "embed": { "$type": EMBED_RECORD, "record": strong_ref(QUOTED, 3) },
        }))
    }

    fn quote_with_media_post() -> serde_json::Value {
        post_json(json!({
            "embed": {
                "$type": EMBED_RECORD_WITH_MEDIA,
                "record": { "$type": EMBED_RECORD, "record": strong_ref(QUOTED, 3) },
                "media": { "$type": "app.bsky.embed.images", "images": [] },
            },
        }))
    }

    #[test]
    fn a_plain_post_refers_to_nothing() {
        assert!(external_record_refs(&dag_cbor(post_json(json!({})))).is_empty());
    }

    #[test]
    fn a_reply_yields_its_parent_and_never_its_root() {
        // Fauna's Reference vocabulary has no thread root, so resolving ROOT
        // would buy a lookup nothing consumes.
        assert_eq!(external_record_refs(&dag_cbor(reply_post())), vec![PARENT]);
    }

    #[test]
    fn a_quote_yields_the_quoted_post() {
        assert_eq!(external_record_refs(&dag_cbor(quote_post())), vec![QUOTED]);
    }

    /// The shape a Go-side walker would miss: the quote sits one level deeper.
    #[test]
    fn a_quote_with_media_yields_the_quoted_post_too() {
        assert_eq!(
            external_record_refs(&dag_cbor(quote_with_media_post())),
            vec![QUOTED]
        );
    }

    #[test]
    fn a_reply_that_also_quotes_yields_both_parent_first() {
        let record = post_json(json!({
            "reply": { "parent": strong_ref(PARENT, 1), "root": strong_ref(ROOT, 2) },
            "embed": { "$type": EMBED_RECORD, "record": strong_ref(QUOTED, 3) },
        }));
        assert_eq!(
            external_record_refs(&dag_cbor(record)),
            vec![PARENT, QUOTED]
        );
    }

    /// A post may reply to and quote the SAME record; the wire map is keyed by
    /// URI, so emitting it twice would be a duplicate key.
    #[test]
    fn a_repeated_uri_is_emitted_once() {
        let record = post_json(json!({
            "reply": { "parent": strong_ref(PARENT, 1), "root": strong_ref(ROOT, 2) },
            "embed": { "$type": EMBED_RECORD, "record": strong_ref(PARENT, 1) },
        }));
        assert_eq!(external_record_refs(&dag_cbor(record)), vec![PARENT]);
    }

    #[test]
    fn a_media_only_embed_yields_nothing() {
        let record = post_json(json!({
            "embed": { "$type": "app.bsky.embed.images", "images": [] },
        }));
        assert!(external_record_refs(&dag_cbor(record)).is_empty());
    }

    /// Undecodable bytes and wrong-typed fields resolve to nothing rather than
    /// erroring — the nest then journals, which is the ratified fallback.
    #[test]
    fn malformed_input_yields_nothing_rather_than_failing() {
        assert!(external_record_refs(b"not dag-cbor at all").is_empty());
        assert!(external_record_refs(&[]).is_empty());

        let wrong_types = post_json(json!({ "reply": { "parent": { "uri": 42 } } }));
        assert!(external_record_refs(&dag_cbor(wrong_types)).is_empty());

        let reply_is_a_string = post_json(json!({ "reply": "not an object" }));
        assert!(external_record_refs(&dag_cbor(reply_is_a_string)).is_empty());
    }

    /// **The drift pin.** This module and [`crate::reverse_translate`] read the
    /// same bytes with the same decoder but through different value shapes
    /// (raw IPLD here, atrium's typed record there). Nothing in the type system
    /// forces them to look in the same *places* — and if they diverge, the
    /// bridge resolves a URI the nest never asks about (or misses one it does),
    /// which shows up as a reply silently journaling instead of round-tripping.
    ///
    /// So: over one corpus, what this extracts must be exactly the set of URIs
    /// `parse_post_record` puts in `reply.parent_uri` + `quote.uri`.
    #[cfg(feature = "client")]
    #[test]
    fn extraction_agrees_with_reverse_translate() {
        use crate::reverse_translate::parse_post_record;

        let corpus = [
            ("plain", post_json(json!({}))),
            ("reply", reply_post()),
            ("quote", quote_post()),
            ("quote_with_media", quote_with_media_post()),
            (
                "reply_and_quote",
                post_json(json!({
                    "reply": { "parent": strong_ref(PARENT, 1), "root": strong_ref(ROOT, 2) },
                    "embed": { "$type": EMBED_RECORD, "record": strong_ref(QUOTED, 3) },
                })),
            ),
            (
                "media_only",
                post_json(json!({
                    "embed": { "$type": "app.bsky.embed.images", "images": [] },
                })),
            ),
        ];

        for (name, record) in corpus {
            let bytes = dag_cbor(record);
            let parsed = parse_post_record(&bytes)
                .unwrap_or_else(|e| panic!("fixture {name} reverse-translates: {e}"));

            let mut expected = Vec::new();
            if let Some(reply) = parsed.reply.as_ref() {
                push_ref(&mut expected, &reply.parent_uri);
            }
            if let Some(quote) = parsed.quote.as_ref() {
                push_ref(&mut expected, &quote.uri);
            }

            assert_eq!(
                external_record_refs(&bytes),
                expected,
                "fixture {name}: the extractor and reverse_translate disagree about \
                 which records this post references"
            );
        }
    }

    // ── external_record_blob_refs (F2.4 slice 2) ─────────────────────────────

    fn blob_value(seed: u8) -> serde_json::Value {
        json!({
            "$type": "blob",
            "ref": { "/": test_cid(seed) },
            "mimeType": "image/png",
            "size": 1234,
        })
    }

    #[test]
    fn a_plain_post_references_no_blobs() {
        assert!(external_record_blob_refs(&dag_cbor(post_json(json!({})))).is_empty());
    }

    #[test]
    fn an_images_embed_yields_each_blob_cid_in_order() {
        let record = post_json(json!({
            "embed": {
                "$type": "app.bsky.embed.images",
                "images": [
                    { "alt": "", "image": blob_value(10) },
                    { "alt": "", "image": blob_value(11) },
                ],
            },
        }));
        assert_eq!(
            external_record_blob_refs(&dag_cbor(record)),
            vec![test_cid(10), test_cid(11)]
        );
    }

    /// The generic walk is the point: a link card's `thumb`, a video blob and
    /// an arbitrary journal-collection record all carry the same `blob` shape,
    /// and none of them is an image embed.
    #[test]
    fn the_walk_is_generic_over_embed_shapes_and_unknown_lexicons() {
        let external = post_json(json!({
            "embed": {
                "$type": "app.bsky.embed.external",
                "external": {
                    "uri": "https://example.com",
                    "title": "t",
                    "description": "d",
                    "thumb": blob_value(12),
                },
            },
        }));
        assert_eq!(
            external_record_blob_refs(&dag_cbor(external)),
            vec![test_cid(12)]
        );

        let video = post_json(json!({
            "embed": { "$type": "app.bsky.embed.video", "video": blob_value(13) },
        }));
        assert_eq!(
            external_record_blob_refs(&dag_cbor(video)),
            vec![test_cid(13)]
        );

        let unknown_lexicon = json!({
            "$type": "com.example.recipe",
            "title": "soup",
            "steps": [ { "photo": blob_value(14) } ],
        });
        assert_eq!(
            external_record_blob_refs(&dag_cbor(unknown_lexicon)),
            vec![test_cid(14)]
        );
    }

    /// A quote-with-media's images sit two maps deep; missing them is the same
    /// one-shape-learned trap as the quote extraction's.
    #[test]
    fn a_quote_with_media_yields_the_media_blobs() {
        let record = post_json(json!({
            "embed": {
                "$type": EMBED_RECORD_WITH_MEDIA,
                "record": { "$type": EMBED_RECORD, "record": strong_ref(QUOTED, 3) },
                "media": {
                    "$type": "app.bsky.embed.images",
                    "images": [ { "alt": "", "image": blob_value(15) } ],
                },
            },
        }));
        assert_eq!(
            external_record_blob_refs(&dag_cbor(record)),
            vec![test_cid(15)]
        );
    }

    /// The legacy untyped form still names its blob; an ordinary map that
    /// merely HAS a `cid` string does not read as one.
    #[test]
    fn the_legacy_untyped_blob_form_is_recognized_but_a_stray_cid_is_not() {
        let legacy = json!({
            "$type": "com.example.old",
            "picture": { "cid": test_cid(16), "mimeType": "image/jpeg" },
        });
        assert_eq!(
            external_record_blob_refs(&dag_cbor(legacy)),
            vec![test_cid(16)]
        );

        let stray = json!({
            "$type": "com.example.other",
            "pin": { "cid": test_cid(17) },
        });
        assert!(external_record_blob_refs(&dag_cbor(stray)).is_empty());
    }

    #[test]
    fn duplicate_blob_refs_are_emitted_once_and_the_cap_holds() {
        let record = json!({
            "$type": "com.example.dup",
            "a": blob_value(18),
            "b": blob_value(18),
        });
        assert_eq!(
            external_record_blob_refs(&dag_cbor(record)),
            vec![test_cid(18)]
        );

        let hostile_entries: Vec<serde_json::Value> =
            (0..40u8).map(|i| blob_value(100 + i)).collect();
        let hostile = json!({ "$type": "com.example.many", "items": hostile_entries });
        assert_eq!(
            external_record_blob_refs(&dag_cbor(hostile)).len(),
            MAX_RECORD_BLOB_REFS
        );
    }

    #[test]
    fn malformed_blob_input_yields_nothing_rather_than_failing() {
        assert!(external_record_blob_refs(b"not dag-cbor at all").is_empty());
        // A `$type: "blob"` whose ref is a string rather than a link is not a
        // well-formed typed blob.
        let bad_ref = json!({
            "$type": "com.example.x",
            "b": { "$type": "blob", "ref": "not-a-link", "mimeType": "image/png" },
        });
        assert!(external_record_blob_refs(&dag_cbor(bad_ref)).is_empty());
    }

    /// **The drift pin, media axis.** The nest resolves the images
    /// `reverse_translate` parses and refuses/stamps from what THIS walk finds;
    /// if the walk missed an image the parse sees, a post could round-trip
    /// carrying media whose row was never stamped — GC bait. So over the image
    /// corpus, the walk must find at least every CID the parse yields.
    #[cfg(feature = "client")]
    #[test]
    fn the_blob_walk_covers_every_image_reverse_translate_parses() {
        use crate::reverse_translate::parse_post_record;

        let corpus = [
            (
                "images",
                post_json(json!({
                    "embed": {
                        "$type": "app.bsky.embed.images",
                        "images": [
                            { "alt": "a", "image": blob_value(20) },
                            { "alt": "b", "image": blob_value(21) },
                        ],
                    },
                })),
            ),
            (
                "quote_with_images",
                post_json(json!({
                    "embed": {
                        "$type": EMBED_RECORD_WITH_MEDIA,
                        "record": { "$type": EMBED_RECORD, "record": strong_ref(QUOTED, 3) },
                        "media": {
                            "$type": "app.bsky.embed.images",
                            "images": [ { "alt": "", "image": blob_value(22) } ],
                        },
                    },
                })),
            ),
            ("no_media", post_json(json!({}))),
        ];

        for (name, record) in corpus {
            let bytes = dag_cbor(record);
            let parsed = parse_post_record(&bytes)
                .unwrap_or_else(|e| panic!("fixture {name} reverse-translates: {e}"));
            let walked = external_record_blob_refs(&bytes);
            for img in &parsed.images {
                assert!(
                    walked.contains(&img.cid),
                    "fixture {name}: the blob walk missed image {}, which the \
                     media arm would resolve but nothing would stamp",
                    img.cid
                );
            }
        }
    }
}

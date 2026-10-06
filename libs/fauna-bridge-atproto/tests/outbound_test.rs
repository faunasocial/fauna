//! Integration tests for the outbound (Fauna -> ATProto) translation module.

use fauna_bridge_atproto::outbound::{
    POST_TEXT_MAX_BYTES, POST_TEXT_MAX_GRAPHEMES, PROFILE_DESCRIPTION_MAX_BYTES,
    PROFILE_DESCRIPTION_MAX_GRAPHEMES, PROFILE_DISPLAY_NAME_MAX_BYTES,
    PROFILE_DISPLAY_NAME_MAX_GRAPHEMES, QuoteRef, ReplyRefs, fauna_post_to_bsky_record,
    truncate_for_lexicon,
};
use fauna_core::data::{Facet, FacetFeature};
use fauna_core::identity::ActorId;
use unicode_segmentation::UnicodeSegmentation;

// ---------------------------------------------------------------------------
// 1. Basic text post -> valid record JSON
// ---------------------------------------------------------------------------

#[test]
fn basic_text_post_produces_valid_record() {
    let result = fauna_post_to_bsky_record("Hello, Bluesky!", &[], None, None);
    let rec = result.expect("should produce a record");

    assert_eq!(rec.collection, "app.bsky.feed.post");

    let json = &rec.record_json;
    assert_eq!(json["$type"], "app.bsky.feed.post");
    assert_eq!(json["text"], "Hello, Bluesky!");

    // createdAt should be present and look like an ISO timestamp
    let created_at = json["createdAt"]
        .as_str()
        .expect("createdAt should be a string");
    assert!(created_at.ends_with('Z'), "should end with Z");
    assert!(created_at.contains('T'), "should contain T separator");

    // No facets, no reply
    assert!(
        json.get("facets").is_none(),
        "no facets for plain text post"
    );
    assert!(json.get("reply").is_none(), "no reply for non-reply post");
}

// ---------------------------------------------------------------------------
// 2. Post with facets -> facets array in record
// ---------------------------------------------------------------------------

#[test]
fn post_with_facets_translates_correctly() {
    // "Hello @alice check https://example.com #rust"
    //  0     6     12    18                   40
    let text = "Hello @alice check https://example.com #rust";

    let facets = vec![
        Facet {
            byte_start: 6,
            byte_end: 12,
            feature: FacetFeature::Mention {
                actor_id: ActorId([0xAA; 32]),
            },
        },
        Facet {
            byte_start: 19,
            byte_end: 38,
            feature: FacetFeature::Link {
                uri: "https://example.com".to_string(),
            },
        },
        Facet {
            byte_start: 39,
            byte_end: 44,
            feature: FacetFeature::Tag {
                name: "rust".to_string(),
            },
        },
    ];

    let result = fauna_post_to_bsky_record(text, &facets, None, None);
    let rec = result.expect("should produce a record");
    let json = &rec.record_json;

    assert_eq!(json["text"], text);

    let facets_arr = json["facets"]
        .as_array()
        .expect("facets should be an array");
    // The mention facet is intentionally omitted (network-exposure.md § F2):
    // a mentioned user's raw actor pubkey must never reach the firehose. The
    // display text ("@alice") is preserved above; only the link and tag facets
    // carry through.
    assert_eq!(facets_arr.len(), 2);
    let wire = serde_json::to_string(json).unwrap();
    assert!(
        !wire.contains("#mention"),
        "mention facet must be omitted: {wire}"
    );
    assert!(
        !wire.contains(&hex::encode([0xAA; 32])),
        "actor pubkey must never reach the firehose: {wire}"
    );

    // Link (now first surviving facet)
    let f0 = &facets_arr[0];
    assert_eq!(f0["features"][0]["$type"], "app.bsky.richtext.facet#link");
    assert_eq!(f0["features"][0]["uri"], "https://example.com");

    // Tag
    let f1 = &facets_arr[1];
    assert_eq!(f1["features"][0]["$type"], "app.bsky.richtext.facet#tag");
    assert_eq!(f1["features"][0]["tag"], "rust");
}

// ---------------------------------------------------------------------------
// 3. Post exceeding 300 graphemes -> truncated with " [...]"
// ---------------------------------------------------------------------------

#[test]
fn long_post_truncated_with_suffix() {
    // Build a string of exactly 350 'a' characters
    let long_text: String = "a".repeat(350);

    let result = fauna_post_to_bsky_record(&long_text, &[], None, None);
    let rec = result.expect("should produce a record");
    let json = &rec.record_json;

    let text = json["text"].as_str().unwrap();
    assert!(
        text.ends_with(" [...]"),
        "truncated text should end with ' [...]'"
    );

    // Count graphemes: should be <= 300
    use unicode_segmentation::UnicodeSegmentation;
    let count = text.graphemes(true).count();
    assert!(count <= 300, "grapheme count {count} should be <= 300");
}

// ---------------------------------------------------------------------------
// 3b. Facets beyond truncated text are dropped
// ---------------------------------------------------------------------------

#[test]
fn facets_beyond_truncated_text_are_dropped() {
    // 310 characters, will be truncated
    let long_text: String = "a".repeat(310);

    let facets = vec![
        // This facet is within the kept range
        Facet {
            byte_start: 0,
            byte_end: 5,
            feature: FacetFeature::Tag {
                name: "ok".to_string(),
            },
        },
        // This facet extends beyond the truncated byte length
        Facet {
            byte_start: 295,
            byte_end: 310,
            feature: FacetFeature::Tag {
                name: "dropped".to_string(),
            },
        },
    ];

    let result = fauna_post_to_bsky_record(&long_text, &facets, None, None);
    let rec = result.expect("should produce a record");
    let json = &rec.record_json;

    let facets_arr = json["facets"].as_array().expect("facets array");
    // Only the first facet should survive (byte range 0..5 is within truncated text)
    assert_eq!(facets_arr.len(), 1);
    assert_eq!(facets_arr[0]["features"][0]["tag"], "ok");
}

// ---------------------------------------------------------------------------
// 4. Reply post -> includes reply object with parent + root
// ---------------------------------------------------------------------------

fn reply_refs() -> ReplyRefs {
    ReplyRefs {
        parent_uri: "at://did:plc:parent/app.bsky.feed.post/abc".into(),
        parent_cid: "bafyparentcid".into(),
        root_uri: "at://did:plc:root/app.bsky.feed.post/xyz".into(),
        root_cid: "bafyrootcid".into(),
    }
}

fn quote_ref() -> QuoteRef {
    QuoteRef {
        uri: "at://did:plc:quoted/app.bsky.feed.post/q1".into(),
        cid: "bafyquotedcid".into(),
    }
}

#[test]
fn reply_post_includes_reply_refs() {
    let result = fauna_post_to_bsky_record("This is a reply", &[], Some(&reply_refs()), None);
    let rec = result.expect("should produce a record");
    let json = &rec.record_json;

    let reply = &json["reply"];
    assert_eq!(
        reply["parent"]["uri"],
        "at://did:plc:parent/app.bsky.feed.post/abc"
    );
    assert_eq!(reply["parent"]["cid"], "bafyparentcid");
    assert_eq!(
        reply["root"]["uri"],
        "at://did:plc:root/app.bsky.feed.post/xyz"
    );
    assert_eq!(reply["root"]["cid"], "bafyrootcid");
    assert!(
        json.get("embed").is_none(),
        "a plain reply carries no embed"
    );
}

// ---------------------------------------------------------------------------
// 4b. Quote post -> `app.bsky.embed.record` naming the quoted record
// (the write-through's derivation of `Reference::Quote`, bridges.md
// § Cross-posting → *A post that references a Bluesky record*)
// ---------------------------------------------------------------------------

#[test]
fn quote_post_embeds_the_quoted_record() {
    let result = fauna_post_to_bsky_record("look at this", &[], None, Some(&quote_ref()));
    let rec = result.expect("should produce a record");
    let json = &rec.record_json;

    assert_eq!(json["embed"]["$type"], "app.bsky.embed.record");
    assert_eq!(
        json["embed"]["record"]["uri"],
        "at://did:plc:quoted/app.bsky.feed.post/q1"
    );
    assert_eq!(json["embed"]["record"]["cid"], "bafyquotedcid");
    assert!(json.get("reply").is_none(), "a quote is not a reply");
}

// ---------------------------------------------------------------------------
// 4c. Neither input -> byte-identical to the plain record (no `reply`, no
// `embed` key at all — absent, never null)
// ---------------------------------------------------------------------------

#[test]
fn no_reference_inputs_leave_the_record_plain() {
    let rec = fauna_post_to_bsky_record("plain", &[], None, None).expect("record");
    assert!(rec.record_json.get("reply").is_none());
    assert!(rec.record_json.get("embed").is_none());
}

// ---------------------------------------------------------------------------
// 5. truncate_for_lexicon edge cases
// ---------------------------------------------------------------------------

#[test]
fn truncate_exact_limit_no_change() {
    let text: String = "a".repeat(300);
    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert_eq!(result, text, "exactly at limit should not truncate");
}

#[test]
fn truncate_under_limit_no_change() {
    let text = "short text";
    let result = truncate_for_lexicon(text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert_eq!(result, text);
}

#[test]
fn truncate_one_over_limit() {
    let text: String = "a".repeat(301);
    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert!(result.ends_with(" [...]"));

    use unicode_segmentation::UnicodeSegmentation;
    let count = result.graphemes(true).count();
    assert!(count <= 300, "grapheme count {count} should be <= 300");
}

#[test]
fn truncate_emoji_graphemes() {
    // Each flag emoji is 2 code points but 1 grapheme cluster
    // U+1F1FA U+1F1F8 = flag
    let flag = "\u{1F1FA}\u{1F1F8}"; // one grapheme
    let text: String = flag.repeat(301); // 301 graphemes, each multi-byte

    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert!(result.ends_with(" [...]"));

    use unicode_segmentation::UnicodeSegmentation;
    let count = result.graphemes(true).count();
    assert!(count <= 300, "grapheme count {count} should be <= 300");
}

#[test]
fn truncate_multi_byte_chars() {
    // Chinese characters: each is 3 bytes, 1 grapheme
    let text: String = "\u{4e16}".repeat(305); // 305 graphemes of 'shi' (world)
    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert!(result.ends_with(" [...]"));

    use unicode_segmentation::UnicodeSegmentation;
    let count = result.graphemes(true).count();
    assert!(count <= 300);
}

#[test]
fn truncate_zero_max() {
    // A cap too small to hold even the marker drops the marker rather than
    // emitting one that itself violates the cap — which is what the old
    // saturating-subtract did, answering a 6-grapheme " [...]" for max 0.
    let result = truncate_for_lexicon("hello", 0, 0);
    assert_eq!(result, "");
}

#[test]
fn truncate_empty_string() {
    let result = truncate_for_lexicon("", POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert_eq!(result, "");
}

// ---------------------------------------------------------------------------
// 5b. The BYTE axis — the half that was missing until 2026-08-03
// ---------------------------------------------------------------------------

/// One family-emoji cluster: 1 grapheme, 25 UTF-8 bytes. The whole point of the
/// pair is that these two numbers are not the same question.
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";

#[test]
fn the_byte_cap_binds_even_when_the_grapheme_cap_does_not() {
    // Exactly at the grapheme cap, 25× over the byte cap. Before the fix this
    // returned all 7500 bytes unchanged, and the repo carried a post no PDS on
    // the network would accept.
    let text = FAMILY.repeat(POST_TEXT_MAX_GRAPHEMES);
    assert_eq!(text.graphemes(true).count(), POST_TEXT_MAX_GRAPHEMES);
    assert!(text.len() > POST_TEXT_MAX_BYTES);

    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert!(
        result.len() <= POST_TEXT_MAX_BYTES,
        "byte length {} exceeds the lexicon's {POST_TEXT_MAX_BYTES}",
        result.len()
    );
    assert!(result.graphemes(true).count() <= POST_TEXT_MAX_GRAPHEMES);
    assert!(result.ends_with(" [...]"));
}

#[test]
fn truncation_never_splits_a_grapheme_cluster() {
    // 3000 / 25 does not divide evenly once the marker is reserved, so the byte
    // budget runs out mid-cluster — the case a naive `&text[..max_bytes]` would
    // cut in half, producing a lone ZWJ tail or, worse, invalid UTF-8.
    let text = FAMILY.repeat(POST_TEXT_MAX_GRAPHEMES);
    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    let body = result.strip_suffix(" [...]").expect("marker present");
    assert_eq!(
        body.len() % FAMILY.len(),
        0,
        "kept {} bytes, which is not a whole number of {}-byte clusters",
        body.len(),
        FAMILY.len()
    );
    assert!(body.graphemes(true).all(|g| g == FAMILY));
}

#[test]
fn both_profile_fields_respect_both_axes() {
    for (max_graphemes, max_bytes) in [
        (
            PROFILE_DISPLAY_NAME_MAX_GRAPHEMES,
            PROFILE_DISPLAY_NAME_MAX_BYTES,
        ),
        (
            PROFILE_DESCRIPTION_MAX_GRAPHEMES,
            PROFILE_DESCRIPTION_MAX_BYTES,
        ),
    ] {
        let text = FAMILY.repeat(max_graphemes);
        let result = truncate_for_lexicon(&text, max_graphemes, max_bytes);
        assert!(
            result.len() <= max_bytes && result.graphemes(true).count() <= max_graphemes,
            "({max_graphemes}, {max_bytes}) produced {} bytes / {} graphemes",
            result.len(),
            result.graphemes(true).count()
        );
    }
}

#[test]
fn text_inside_both_caps_is_returned_verbatim() {
    // The regression that matters in the other direction: a fix that clamped
    // bytes unconditionally would start truncating ordinary multi-byte posts
    // that were always fine. 100 clusters = 2500 bytes, inside both caps.
    let text = FAMILY.repeat(100);
    assert!(text.len() < POST_TEXT_MAX_BYTES);
    let result = truncate_for_lexicon(&text, POST_TEXT_MAX_GRAPHEMES, POST_TEXT_MAX_BYTES);
    assert_eq!(result, text);
}

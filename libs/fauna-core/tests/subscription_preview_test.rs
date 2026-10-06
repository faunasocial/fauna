use fauna_core::data::{ContentHash, Dimensions, MediaItem, PostBody};
use fauna_core::subscription::preview::auto_preview;

#[test]
fn preview_short_text_unchanged() {
    let body = PostBody::Text {
        content: "Short post.".to_string(),
        facets: vec![],
    };

    let preview = auto_preview(&body);

    match preview {
        PostBody::Text { content, facets } => {
            assert_eq!(content, "Short post.");
            assert!(facets.is_empty());
        }
        _ => panic!("expected Text"),
    }
}

#[test]
fn preview_long_text_truncated_at_280() {
    let long_text = "a".repeat(500);
    let body = PostBody::Text {
        content: long_text,
        facets: vec![],
    };

    let preview = auto_preview(&body);

    match preview {
        PostBody::Text { content, .. } => {
            assert!(content.len() <= 283); // 280 + "..."
            assert!(content.ends_with("..."));
        }
        _ => panic!("expected Text"),
    }
}

#[test]
fn preview_text_truncation_respects_char_boundary() {
    let body = PostBody::Text {
        content: "é".repeat(300), // each é is 2 bytes, 300 chars > 280 limit
        facets: vec![],
    };

    let preview = auto_preview(&body);

    match preview {
        PostBody::Text { content, .. } => {
            assert!(content.is_char_boundary(content.len()));
            assert!(content.ends_with("..."));
        }
        _ => panic!("expected Text"),
    }
}

#[test]
fn preview_media_replaces_with_blurred_flag() {
    let body = PostBody::Media {
        items: vec![MediaItem {
            blob_hash: ContentHash::from_digest_raw([1u8; 32]),
            media_type: "image/jpeg".to_string(),
            size_bytes: 100_000,
            dimensions: Some(Dimensions {
                width: 1920,
                height: 1080,
            }),
            thumbnail: Some(ContentHash::from_digest_raw([2u8; 32])),
            ..Default::default()
        }],
        alt_text: Some("A photo".to_string()),
    };

    let preview = auto_preview(&body);

    match preview {
        PostBody::Media { items, alt_text } => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].blob_hash, ContentHash::from_digest_raw([0u8; 32]));
            assert!(items[0].thumbnail.is_some());
            assert_eq!(alt_text, Some("A photo".to_string()));
        }
        _ => panic!("expected Media"),
    }
}

#[test]
fn preview_text_with_media_truncates_and_blurs() {
    let body = PostBody::TextWithMedia {
        content: "a".repeat(500),
        facets: vec![],
        items: vec![MediaItem {
            blob_hash: ContentHash::from_digest_raw([1u8; 32]),
            media_type: "image/png".to_string(),
            size_bytes: 50_000,
            dimensions: None,
            thumbnail: Some(ContentHash::from_digest_raw([3u8; 32])),
            ..Default::default()
        }],
    };

    let preview = auto_preview(&body);

    match preview {
        PostBody::TextWithMedia { content, items, .. } => {
            assert!(content.len() <= 283);
            assert!(content.ends_with("..."));
            assert_eq!(items[0].blob_hash, ContentHash::from_digest_raw([0u8; 32]));
        }
        _ => panic!("expected TextWithMedia"),
    }
}

#[test]
fn preview_facets_within_range_kept() {
    use fauna_core::data::{Facet, FacetFeature};

    let body = PostBody::Text {
        content: "a".repeat(500),
        facets: vec![
            Facet {
                byte_start: 0,
                byte_end: 10,
                feature: FacetFeature::Tag {
                    name: "test".to_string(),
                },
            },
            Facet {
                byte_start: 290,
                byte_end: 300,
                feature: FacetFeature::Tag {
                    name: "cut".to_string(),
                },
            },
        ],
    };

    let preview = auto_preview(&body);

    match preview {
        PostBody::Text { facets, .. } => {
            assert_eq!(facets.len(), 1);
            assert_eq!(facets[0].byte_start, 0);
        }
        _ => panic!("expected Text"),
    }
}

//! Tests for extract_post_searchable_text — the indexer's mail-text extractor.

#![cfg(feature = "post-text")]

use fauna_core::data::{ContentHash, Post, PostBody, StructuredField, Timestamp};
use fauna_core::identity::ActorId;
use fauna_mail::{SearchableText, extract_post_searchable_text};

fn post_with_body(body: PostBody) -> Post {
    Post {
        author: ActorId([0u8; 32]),
        created_at: Timestamp(0),
        expires_at: None,
        references: vec![],
        body,
        gated: None,
        content_warning: None,
        origin: None,
    }
}

#[test]
fn extracts_subject_and_body_from_email_v1_structured() {
    let body = PostBody::Structured {
        schema: "email/v1".into(),
        fields: vec![
            StructuredField {
                key: "subject".into(),
                value: "Hello, world".into(),
            },
            StructuredField {
                key: "in_reply_to".into(),
                value: "<x@y>".into(),
            },
        ],
        content: Some("This is the body text.".into()),
        facets: vec![],
        items: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: Some("Hello, world".into()),
            body: "This is the body text.".into(),
        },
    );
}

#[test]
fn handles_email_v1_with_missing_subject() {
    let body = PostBody::Structured {
        schema: "email/v1".into(),
        fields: vec![],
        content: Some("Body only".into()),
        facets: vec![],
        items: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: "Body only".into()
        }
    );
}

#[test]
fn handles_email_v1_with_missing_body() {
    let body = PostBody::Structured {
        schema: "email/v1".into(),
        fields: vec![StructuredField {
            key: "subject".into(),
            value: "Just a subject".into(),
        }],
        content: None,
        facets: vec![],
        items: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: Some("Just a subject".into()),
            body: String::new()
        },
    );
}

#[test]
fn handles_email_v1_with_empty_subject_string() {
    let body = PostBody::Structured {
        schema: "email/v1".into(),
        fields: vec![StructuredField {
            key: "subject".into(),
            value: String::new(),
        }],
        content: Some("Body present".into()),
        facets: vec![],
        items: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: "Body present".into(),
        },
        "empty subject string should produce title: None, same as missing subject"
    );
}

#[test]
fn extracts_text_post_body() {
    let body = PostBody::Text {
        content: "A plain text post".into(),
        facets: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: "A plain text post".into()
        },
    );
}

#[test]
fn extracts_text_with_media_body() {
    let body = PostBody::TextWithMedia {
        content: "Photo dump".into(),
        facets: vec![],
        items: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: "Photo dump".into()
        },
    );
}

#[test]
fn unknown_structured_schema_returns_empty_body() {
    let body = PostBody::Structured {
        schema: "weather/v1".into(),
        fields: vec![],
        content: Some("ignored".into()),
        facets: vec![],
        items: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    // Indexer can still index this kind, but the indexer for the mail kind
    // shouldn't pick up unknown schemas — return empty so the caller can
    // skip it.
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: String::new()
        }
    );
}

#[test]
fn media_only_body_returns_empty() {
    let body = PostBody::Media {
        items: vec![],
        alt_text: Some("alt".into()),
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: String::new()
        }
    );
}

#[test]
fn video_body_returns_empty() {
    let body = PostBody::Video {
        manifest: ContentHash::from_digest_raw([0u8; 32]),
        segments: vec![],
        thumbnail: ContentHash::from_digest_raw([0u8; 32]),
        duration_ms: 0,
        aspect_ratio: (16, 9),
        anchors: vec![],
    };
    let result = extract_post_searchable_text(&post_with_body(body));
    assert_eq!(
        result,
        SearchableText {
            title: None,
            body: String::new()
        }
    );
}

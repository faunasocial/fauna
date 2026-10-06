//! `extract_post_searchable_text` — pull a `(title, body)` view out of a
//! `fauna_core::data::Post` for the search index.
//!
//! The fauna `Post` shape is uniform across all content kinds, but per-kind
//! conventions (which `PostBody` variant, which `StructuredField` keys map
//! to subject/title) differ. This helper handles the mail kind. Other kinds
//! land their extractors here as they wire up their own indexer paths
//! (Plan 9 et seq.).

use fauna_core::data::{Post, PostBody};

/// Title + body view of a post, suitable for tokenization through
/// `fauna_index`'s `IndexedDoc` schema.
///
/// `title` is `None` when the post has no separate title concept (a plain
/// `PostBody::Text` post) — the indexer treats this as a body-only field set.
/// `body` is `String::new()` when the post has no searchable text at all
/// (media-only, video). Callers (e.g. the indexer) typically skip empty-body
/// posts entirely rather than write an empty segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchableText {
    pub title: Option<String>,
    pub body: String,
}

/// Extract `(title, body)` from a `Post`. See `SearchableText` for the shape.
///
/// Currently handles the mail kind explicitly (`Structured { schema: "email/v1" }`),
/// `Text`, `TextWithMedia`. Other shapes (Media, Video, unknown Structured
/// schemas) return an empty body — the caller's responsibility is to decide
/// whether to index the post at all.
pub fn extract_post_searchable_text(post: &Post) -> SearchableText {
    match &post.body {
        PostBody::Structured {
            schema,
            fields,
            content,
            ..
        } if schema == "email/v1" => {
            let title = fields
                .iter()
                .find(|f| f.key == "subject")
                .map(|f| f.value.clone())
                // Empty subject strings produce title: None — same as a missing subject
                // field. (Bridge's extract_email_fields uses unwrap_or_default() and
                // keeps the empty string; for indexing there's no point in writing an
                // empty title field, so we filter it out here.)
                .filter(|s| !s.is_empty());
            let body = content.clone().unwrap_or_default();
            SearchableText { title, body }
        }
        PostBody::Text { content, .. } => SearchableText {
            title: None,
            body: content.clone(),
        },
        PostBody::TextWithMedia { content, .. } => SearchableText {
            title: None,
            body: content.clone(),
        },
        // Media-only / Video / unknown Structured schemas: no searchable text
        // for the mail-kind indexer. Other content kinds will get their own
        // helpers in this module as they ship.
        _ => SearchableText {
            title: None,
            body: String::new(),
        },
    }
}

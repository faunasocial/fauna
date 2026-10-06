//! The shared quoted-post projection (`docs/goal/ui/feed.md` § The read model:
//! "The embedded quoted-post card is projected by a shared `libs/fauna-feed`
//! helper from the already-loaded post set"). This is the single shared
//! projection that retires the per-app quoted-post divergence — before it,
//! iOS rendered the embed in `post_detail`, web/macOS in the list card only,
//! and linux/windows/android nowhere, each fetching+projecting the quote
//! itself. Now every app + web renders it identically from this helper.
//!
//! The common case is in-page: the quoted post is already in the loaded list,
//! so [`project_from_loaded`] finds it with no fetch. For a quote *outside* the
//! loaded page, the manager falls back to a batched `fauna.posts.get`
//! ([`crate::FeedManager::resolve_quoted_post`]) and projects via
//! [`project_decoded`].

use fauna_core::data::Post;
#[cfg(test)]
use fauna_core::data::PostBody;
use fauna_core::render::{AuthoringOriginStatus, VerificationStatus};

use crate::snapshot::{PostSummary, QuotedPostView};

/// The 280-char cap on the embedded quote body (`feed.md` § Post content
/// types: "a truncated body (280 character cap)").
pub const QUOTE_BODY_CAP: usize = 280;

/// Truncate `body` to the [`QUOTE_BODY_CAP`] on a `char` boundary (never
/// splitting a multi-byte char), appending an ellipsis when it was cut.
pub(crate) fn truncate_quote_body(body: &str) -> String {
    let mut chars = body.chars();
    let head: String = chars.by_ref().take(QUOTE_BODY_CAP).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Project the embedded quoted-post card from the already-loaded post set.
/// Returns `None` when the quoted post isn't in `loaded` (the caller then
/// falls back to a `fauna.posts.get` fetch). Matching is by hex `post_id`.
pub fn project_from_loaded(loaded: &[PostSummary], quoted_post_id: &str) -> Option<QuotedPostView> {
    loaded
        .iter()
        .find(|p| p.post_id == quoted_post_id)
        .map(|p| QuotedPostView {
            post_id: p.post_id.clone(),
            author: p.author.clone(),
            body: truncate_quote_body(&p.body),
            // Inherit the loaded list card's status (usually `Unchecked` — the
            // card wasn't independently decoded). The in-page path does no
            // decode, so it asserts no fresh verification of its own.
            verification: p.verification,
            // Same inheritance for the D10 audit answer: the in-page path does no
            // decode, so it reports whatever the loaded card already knew.
            authoring_origin: p.authoring_origin,
            // A quote resolved from the loaded page is never a taken-down post —
            // the feed excludes taken-down posts, so they never appear in the
            // loaded set (the tombstone is set only on the fetch fallback below).
            legal_takedown_ref: None,
            // It is in the loaded page, so it is there.
            not_found: false,
        })
}

/// Project the embedded quoted-post card for a quote whose post has been **taken
/// down under a legal obligation** (`moderation.md` § Categories & enforcement
/// item 1). The nest withholds the body ([`PostGetReply::legal_takedown`] `= Some`,
/// `body` empty), so there is no envelope to decode: the author is unknown and the
/// card carries only the takedown `reference`, from which the client renders the
/// shared tombstone (`fauna_core::obligation::legal_takedown_tombstone`) in place
/// of the quoted content. Reached only on the [`crate::FeedManager::resolve_quoted_post`]
/// fetch fallback (a taken-down post is feed-excluded, never in the loaded page).
///
/// [`PostGetReply::legal_takedown`]: fauna_protocol::posts::PostGetReply
pub fn project_taken_down(post_id: &str, reference: String) -> QuotedPostView {
    QuotedPostView {
        post_id: post_id.to_string(),
        // The body is withheld, so the signed envelope can't be decoded — the
        // author is unknown. The tombstone stands in for the whole quoted card.
        author: String::new(),
        body: String::new(),
        // No envelope was verified (there was none to verify), so neither the
        // verification nor the origin question has a trustworthy answer here.
        verification: VerificationStatus::Unchecked,
        authoring_origin: AuthoringOriginStatus::Unknown,
        legal_takedown_ref: Some(reference),
        // Withheld is not gone: the post exists, the nest may not serve it.
        not_found: false,
    }
}

/// Project the embedded quoted-post card for a quote whose post is **gone** — its
/// author deleted it (`ui/feed.md` § Post deletion: a reference to a deleted post
/// dangles by design and renders the not-found state). There is nothing to show,
/// not even an author, so the card carries only the id and `not_found`, and the
/// client paints `feed.post.post_not_found` in the embed's place — never a blank
/// embed, and never no embed at all, which would read as a quote of nothing.
///
/// Reached from [`crate::FeedManager::resolve_quoted_post`]'s fetch fallback when
/// the nest answers `fauna.posts.not_found`, and from
/// [`crate::FeedManager::delete_post`] for the post the user just deleted (whose
/// embed that device had resolved from its own loaded page).
pub fn project_not_found(post_id: &str) -> QuotedPostView {
    QuotedPostView {
        post_id: post_id.to_string(),
        author: String::new(),
        body: String::new(),
        // Nothing was decoded, so neither question has an answer.
        verification: VerificationStatus::Unchecked,
        authoring_origin: AuthoringOriginStatus::Unknown,
        legal_takedown_ref: None,
        not_found: true,
    }
}

/// Project the embedded quoted-post card from a freshly-decoded [`Post`] (the
/// `fauna.posts.get` fallback path). `post_id` is the hex id the card links to
/// (the caller already holds it from the `quoted_post_id` reference, so we
/// don't recompute the CID here). `verification` is the status from the caller's
/// `decode_post` of the fetched body ([`VerificationStatus::from_valid`]) — this
/// path *does* decode a raw signed envelope, so it carries a real
/// `Verified`/`Failed` (F-CL3: don't discard the validity flag).
pub fn project_decoded(
    post_id: &str,
    post: &Post,
    verification: VerificationStatus,
    authoring_origin: AuthoringOriginStatus,
) -> QuotedPostView {
    QuotedPostView {
        post_id: post_id.to_string(),
        author: hex::encode(post.author.0),
        body: truncate_quote_body(&post.body_text()),
        verification,
        authoring_origin,
        // A decoded post is a live post (it decoded); a taken-down post has a
        // withheld/empty body and never reaches this decode path — its tombstone
        // is projected by [`project_taken_down`] instead.
        legal_takedown_ref: None,
        // It decoded, so it is there.
        not_found: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, author: &str, body: &str) -> PostSummary {
        PostSummary {
            post_id: id.into(),
            author: author.into(),
            body: body.into(),
            document: fauna_core::render::markdown_to_document(body),
            source: "fauna".into(),
            ..Default::default()
        }
    }

    #[test]
    fn projects_quote_from_the_loaded_set() {
        let loaded = vec![
            summary("aa", "11", "first"),
            summary("bb", "22", "the quoted body"),
        ];
        let view = project_from_loaded(&loaded, "bb").expect("bb is loaded");
        assert_eq!(view.post_id, "bb");
        assert_eq!(view.author, "22");
        assert_eq!(view.body, "the quoted body");
        // The in-page path does no decode, so it inherits the card's status
        // (here `Unchecked`) rather than asserting a fresh verification.
        assert_eq!(view.verification, VerificationStatus::Unchecked);
    }

    #[test]
    fn loaded_quote_inherits_the_source_cards_verification() {
        let mut quoted = summary("bb", "22", "the quoted body");
        quoted.verification = VerificationStatus::Failed;
        let loaded = vec![summary("aa", "11", "first"), quoted];
        let view = project_from_loaded(&loaded, "bb").expect("bb is loaded");
        assert_eq!(view.verification, VerificationStatus::Failed);
    }

    #[test]
    fn decoded_quote_carries_the_callers_verification() {
        let post = Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(0),
            body: PostBody::Text {
                content: "decoded quote body".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let ok = project_decoded(
            "cc",
            &post,
            VerificationStatus::from_valid(true),
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(ok.verification, VerificationStatus::Verified);
        let bad = project_decoded(
            "cc",
            &post,
            VerificationStatus::from_valid(false),
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(bad.verification, VerificationStatus::Failed);
        assert_eq!(bad.body, "decoded quote body");
    }

    #[test]
    fn returns_none_when_quote_not_loaded() {
        let loaded = vec![summary("aa", "11", "first")];
        assert!(project_from_loaded(&loaded, "zz").is_none());
    }

    #[test]
    fn truncates_long_quote_body_to_the_cap_on_a_char_boundary() {
        // A multi-byte char straddling the cap boundary must not be split.
        let body = "é".repeat(QUOTE_BODY_CAP + 10);
        let out = truncate_quote_body(&body);
        // QUOTE_BODY_CAP chars + the ellipsis.
        assert_eq!(out.chars().count(), QUOTE_BODY_CAP + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn short_body_is_not_truncated_or_ellipsised() {
        assert_eq!(truncate_quote_body("hi"), "hi");
    }

    fn post_with_body(body: PostBody) -> Post {
        Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(0),
            body,
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    #[test]
    fn decoded_text_with_media_body_yields_its_content() {
        let post = post_with_body(PostBody::TextWithMedia {
            content: "caption text".into(),
            facets: vec![],
            items: vec![],
        });
        let view = project_decoded(
            "cc",
            &post,
            VerificationStatus::Unchecked,
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(view.body, "caption text");
    }

    #[test]
    fn decoded_structured_body_yields_its_content_when_present() {
        let post = post_with_body(PostBody::Structured {
            schema: "poll".into(),
            fields: vec![],
            content: Some("structured content".into()),
            facets: vec![],
            items: vec![],
        });
        let view = project_decoded(
            "cc",
            &post,
            VerificationStatus::Unchecked,
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(view.body, "structured content");
    }

    #[test]
    fn decoded_structured_body_with_no_content_yields_empty_string() {
        let post = post_with_body(PostBody::Structured {
            schema: "poll".into(),
            fields: vec![],
            content: None,
            facets: vec![],
            items: vec![],
        });
        let view = project_decoded(
            "cc",
            &post,
            VerificationStatus::Unchecked,
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(view.body, "");
    }

    #[test]
    fn decoded_media_body_yields_its_alt_text_when_present() {
        let post = post_with_body(PostBody::Media {
            items: vec![],
            alt_text: Some("a cat photo".into()),
        });
        let view = project_decoded(
            "cc",
            &post,
            VerificationStatus::Unchecked,
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(view.body, "a cat photo");
    }

    #[test]
    fn decoded_media_body_with_no_alt_text_yields_empty_string() {
        let post = post_with_body(PostBody::Media {
            items: vec![],
            alt_text: None,
        });
        let view = project_decoded(
            "cc",
            &post,
            VerificationStatus::Unchecked,
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(view.body, "");
    }

    #[test]
    fn decoded_video_body_has_no_inline_text() {
        let post = post_with_body(PostBody::Video {
            manifest: fauna_core::data::ContentHash::of_raw(b"manifest"),
            segments: vec![],
            thumbnail: fauna_core::data::ContentHash::of_raw(b"thumb"),
            duration_ms: 1000,
            aspect_ratio: (16, 9),
            anchors: vec![],
        });
        let view = project_decoded(
            "cc",
            &post,
            VerificationStatus::Unchecked,
            AuthoringOriginStatus::Unknown,
        );
        assert_eq!(view.body, "");
    }

    #[test]
    fn taken_down_quote_has_no_author_and_no_body() {
        let view = project_taken_down("dd", "legal-ref-123".into());
        assert_eq!(view.post_id, "dd");
        assert_eq!(
            view.author, "",
            "the withheld body means the author is unknown"
        );
        assert_eq!(view.body, "");
        assert_eq!(view.legal_takedown_ref, Some("legal-ref-123".to_string()));
        assert_eq!(view.verification, VerificationStatus::Unchecked);
    }
}

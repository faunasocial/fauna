//! The Bluesky-native thread page — the shared read model behind
//! `feed-thread-page` (`docs/goal/behavior/bridges.md` § Bluesky-native thread
//! view). A post detail whose source is `bluesky` offers *View thread*; the
//! page shows the conversation around it as one flat list — ancestors
//! oldest-first, the opened post, then its direct replies — with the opened
//! post marked.
//!
//! Which post is the opened one is the nest's answer
//! ([`BlueskyThreadReply::focal_index`]), never an app's guess. Every app reads
//! the same [`ThreadPage`] off [`crate::FeedSnapshot::thread`], so loading /
//! loaded / failed is decided once here (priority #2).

use fauna_core::localized::LocalizedText;
use fauna_protocol::bluesky::BlueskyThreadReply;
use serde::{Deserialize, Serialize};

use crate::{PostSummary, SourceKind};

/// Where the thread fetch stands — drives the page's loading / list / error
/// rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ThreadStatus {
    /// `bluesky.feed.thread` is in flight; `posts` is empty.
    #[default]
    Loading,
    /// `posts` is the thread the nest returned.
    Loaded,
    /// The fetch failed; [`ThreadPage::error`] carries the reason, which the
    /// page paints on its `error-message` like any failed fetch.
    Failed,
}

/// One post of the thread — one `feed-thread-post[i]`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ThreadPostView {
    /// The post's AT-URI — its identity on Bluesky.
    pub at_uri: String,
    /// `@handle`, without the `@`.
    pub author_handle: String,
    /// The author's chosen display name, when their profile carries one.
    pub author_display_name: Option<String>,
    /// The post's text.
    pub text: String,
    /// RFC 3339, as the origin stamped it.
    pub created_at: String,
    /// The engagement counts Bluesky reported for the post — shown as a
    /// static row (a thread post is a Bluesky AT-URI, not a Fauna post, so it
    /// carries no interaction buttons).
    pub reply_count: u64,
    pub repost_count: u64,
    pub like_count: u64,
    /// The post the reader opened the thread from — the selected state of its
    /// `feed-thread-post`. Exactly one post of a loaded thread carries it.
    pub is_focal: bool,
}

impl ThreadPostView {
    /// The author as the page names them: the display name when there is one,
    /// else `@handle`.
    pub fn author_label(&self) -> String {
        match self.author_display_name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => format!("@{}", self.author_handle),
        }
    }
}

/// The thread page — present on [`crate::FeedSnapshot::thread`] from
/// [`crate::FeedManager::open_thread`] until
/// [`crate::FeedManager::close_thread`].
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ThreadPage {
    /// Hex id of the Fauna post the thread was opened from.
    pub post_id: String,
    pub status: ThreadStatus,
    /// Ancestors oldest-first, the opened post, then its direct replies.
    pub posts: Vec<ThreadPostView>,
    /// Why the fetch failed — present only when `status` is `Failed`.
    pub error: Option<LocalizedText>,
}

impl ThreadPage {
    /// A page whose fetch is in flight.
    pub(crate) fn loading(post_id: String) -> Self {
        ThreadPage {
            post_id,
            ..Default::default()
        }
    }

    /// The loaded page for `reply`: each post projected, the one at the
    /// nest's `focal_index` marked. A `focal_index` past the end marks none
    /// rather than guessing.
    pub(crate) fn loaded(post_id: String, reply: BlueskyThreadReply) -> Self {
        let focal = reply.focal_index as usize;
        let posts = reply
            .posts
            .into_iter()
            .enumerate()
            .map(|(i, p)| ThreadPostView {
                at_uri: p.at_uri,
                author_handle: p.author_handle,
                author_display_name: p.author_display_name,
                text: p.text,
                created_at: p.created_at,
                reply_count: p.reply_count,
                repost_count: p.repost_count,
                like_count: p.like_count,
                is_focal: i == focal,
            })
            .collect();
        ThreadPage {
            post_id,
            status: ThreadStatus::Loaded,
            posts,
            error: None,
        }
    }

    /// A failed page carrying the reason.
    pub(crate) fn failed(post_id: String, error: LocalizedText) -> Self {
        ThreadPage {
            post_id,
            status: ThreadStatus::Failed,
            posts: Vec::new(),
            error: Some(error),
        }
    }
}

impl PostSummary {
    /// Whether this post's detail offers *View thread*
    /// (`feed-post-view-thread-button`): only a post whose source is
    /// `bluesky` has a Bluesky thread to open.
    pub fn offers_thread_view(&self) -> bool {
        SourceKind::classify(&self.source) == SourceKind::Bluesky
    }
}

/// [`PostSummary::offers_thread_view`] for native (UniFFI) apps — a
/// `uniffi::Record` can't carry exported methods (the [`crate::feed_empty_state`]
/// posture).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn post_offers_thread_view(post: PostSummary) -> bool {
    post.offers_thread_view()
}

/// [`ThreadPostView::author_label`] for native (UniFFI) apps — the same
/// record-carries-no-methods posture as [`post_offers_thread_view`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn thread_post_author_label(post: ThreadPostView) -> String {
    post.author_label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::bluesky::BlueskyPost;

    fn bsky(uri: &str, handle: &str, text: &str) -> BlueskyPost {
        BlueskyPost {
            at_uri: uri.into(),
            author_handle: handle.into(),
            text: text.into(),
            source: "bluesky".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_loaded_page_marks_exactly_the_nest_named_post() {
        let reply = BlueskyThreadReply {
            posts: vec![
                bsky("at://a/1", "root.test", "the root"),
                bsky("at://a/2", "opened.test", "the opened post"),
                bsky("at://a/3", "stranger.test", "a reply"),
            ],
            focal_index: 1,
            extra: Default::default(),
        };
        let page = ThreadPage::loaded("ab".into(), reply);
        assert_eq!(page.status, ThreadStatus::Loaded);
        let marked: Vec<bool> = page.posts.iter().map(|p| p.is_focal).collect();
        assert_eq!(marked, [false, true, false]);
        assert_eq!(page.posts[1].text, "the opened post");
        assert_eq!(page.error, None);
    }

    #[test]
    fn the_loaded_page_carries_each_posts_engagement_counts() {
        let reply = BlueskyThreadReply {
            posts: vec![BlueskyPost {
                reply_count: 2,
                repost_count: 0,
                like_count: 7,
                ..bsky("at://a/1", "a.test", "counted")
            }],
            focal_index: 0,
            extra: Default::default(),
        };
        let page = ThreadPage::loaded("ab".into(), reply);
        let p = &page.posts[0];
        assert_eq!((p.reply_count, p.repost_count, p.like_count), (2, 0, 7));
    }

    #[test]
    fn a_focal_index_past_the_end_marks_nothing() {
        let reply = BlueskyThreadReply {
            posts: vec![bsky("at://a/1", "a.test", "only")],
            focal_index: 4,
            extra: Default::default(),
        };
        let page = ThreadPage::loaded("ab".into(), reply);
        assert!(page.posts.iter().all(|p| !p.is_focal));
    }

    #[test]
    fn the_author_label_prefers_the_display_name() {
        let mut p = ThreadPostView {
            author_handle: "bob.test".into(),
            ..Default::default()
        };
        assert_eq!(p.author_label(), "@bob.test");
        p.author_display_name = Some("  ".into());
        assert_eq!(p.author_label(), "@bob.test");
        p.author_display_name = Some("Bob".into());
        assert_eq!(p.author_label(), "Bob");
        // The native apps' face answers the same label.
        assert_eq!(thread_post_author_label(p), "Bob");
    }

    #[test]
    fn only_a_bluesky_post_offers_the_thread_view() {
        let post = |source: &str| PostSummary {
            source: source.into(),
            ..Default::default()
        };
        assert!(post("bluesky").offers_thread_view());
        assert!(post_offers_thread_view(post("Bluesky")));
        for other in ["fauna", "nostr", "activitypub", ""] {
            assert!(!post(other).offers_thread_view(), "{other}");
        }
    }
}

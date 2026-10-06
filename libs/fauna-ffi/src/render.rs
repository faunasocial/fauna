//! UniFFI façade for the shared render-model plaintext flattener
//! ([`fauna_core::render::RenderDocument::to_plaintext`]).
//!
//! The Rust-native Linux app calls `RenderDocument::to_plaintext` directly;
//! this export gives Apple the same document→plaintext projection so its e2e
//! body-text automation reads (`dm-message-text` / `feed-post-text` /
//! `feed-post-detail-body`) expose the *painted document* text — markdown
//! markers stripped, format-aware — exactly like linux/web/android/windows,
//! instead of the raw `body` source (the lone-outlier read drift documented in
//! `docs/goal/architecture/render-model.md` § Implementation status, the P1
//! read-uniformity residual). The raw `body` snapshot field stays put; only the
//! automation read changes.
//!
//! Gated default-on (dropped by the Go mail-bridge `--no-default-features`
//! build) for the same cross-namespace reason as `value-format` / `search`: the
//! export takes a `fauna_core` `RenderDocument`, so uniffi-bindgen-go would emit
//! an uncompilable bare `fauna_core` import. The feature forwards
//! `fauna-core/uniffi` so `RenderDocument` gets its `uniffi::Record`
//! registration cross-crate (render-model.md § Implementation status — "direct
//! embed … forwarding `fauna-core/uniffi`"). The bridge has no render surface,
//! so dropping it is harmless.

use fauna_core::render::{
    Inline, LinkPreviewStateOwned, ProxiedImageRefOwned, ProxiedVideoRefOwned,
    QuotedPostEmbedOwned, RemoteImageRefOwned, RenderDocument, ResolvedLinkPreviewOwned,
};

/// UniFFI face of [`fauna_core::render::RenderDocument::to_plaintext`] — flatten
/// a render document to a single-line, format-aware plaintext preview (markdown
/// markers stripped, link labels kept, image alt text included), the document
/// twin of `markdown_to_plaintext`. Clients read the painted body text through
/// this instead of re-deriving it from the raw `body` source, so every app's
/// body automation read agrees by construction.
#[uniffi::export]
pub fn render_document_to_plaintext(document: RenderDocument) -> String {
    document.to_plaintext()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::has_blocked_remote_images`]
/// — whether the document carries at least one **un-revealed** remote image (a body
/// [`RenderBlock::RemoteImage`], or a `Resolved` link-preview og:image at any nesting
/// depth), i.e. the post/message `load-remote-content-button` should show
/// (render-model.md § D3, § D4 the D4 og:image twin). Clients gate the button on this
/// single shared predicate instead of each re-walking the block tree — a per-app
/// omission vector, since every new blocked-content arm (the D4 og:image, the D7a
/// task-list recursion) otherwise has to be added to each hand-rolled twin separately,
/// and a top-level-only walk silently misses a nested embed.
#[uniffi::export]
pub fn render_document_has_blocked_remote_images(document: RenderDocument) -> bool {
    document.has_blocked_remote_images()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::first_image_hash`] — the
/// content hash of the first trusted embedded image, so a client can paint the
/// `post-image`/media bytes through its own blob loader instead of re-walking the
/// block tree to find the hash itself.
#[uniffi::export]
pub fn render_document_first_image_hash(document: RenderDocument) -> Option<String> {
    document.first_image_hash().map(str::to_string)
}

/// UniFFI face of [`fauna_core::render::RenderDocument::first_video_hash`] — the content hash
/// of the first trusted embedded **video**, the twin of [`render_document_first_image_hash`]
/// and the accessor a client paints its `video-thumbnail` element from.
///
/// Kept separate from the image face on purpose: the two drive different ui.yaml elements, and
/// a client must never paint a video blob into `post-image` (render-model.md § Implementation
/// status today — the gap this closed).
#[uniffi::export]
pub fn render_document_first_video_hash(document: RenderDocument) -> Option<String> {
    document.first_video_hash().map(str::to_string)
}

/// UniFFI face of [`fauna_core::render::RenderDocument::media_blocks`] — every trusted media
/// block (`Image` / `Video`) in body order, for a client that paints them ALL.
///
/// The single-element painters use the `first_*_hash` faces above instead; this is the door for
/// the multi-item render (`ui.yaml` `image-grid`, specced for 1/2/4 images) that web already
/// ships and the native apps trickle down to.
#[uniffi::export]
pub fn render_document_media_blocks(
    document: RenderDocument,
) -> Vec<fauna_core::render::RenderBlock> {
    document.media_blocks()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::quoted_post`] — the folded
/// feed quote-post embed, if the manager has resolved one. Returns the owned
/// [`QuotedPostEmbedOwned`] mirror since the borrowed [`fauna_core::render::QuotedPostEmbed`]
/// carries a lifetime that can't cross UniFFI.
#[uniffi::export]
pub fn render_document_quoted_post(document: RenderDocument) -> Option<QuotedPostEmbedOwned> {
    document.quoted_post().map(Into::into)
}

/// UniFFI face of [`fauna_core::render::RenderDocument::resolving_link_preview_urls`] —
/// the urls of link previews still resolving, in body order, so a client can fire the
/// resolve for each without re-walking the block tree itself.
#[uniffi::export]
pub fn render_document_resolving_link_preview_urls(document: RenderDocument) -> Vec<String> {
    document
        .resolving_link_preview_urls()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::resolved_link_previews`] —
/// the resolved link previews, in body order, as owned
/// [`ResolvedLinkPreviewOwned`] mirrors (see [`render_document_quoted_post`] for why).
#[uniffi::export]
pub fn render_document_resolved_link_previews(
    document: RenderDocument,
) -> Vec<ResolvedLinkPreviewOwned> {
    document
        .resolved_link_previews()
        .into_iter()
        .map(Into::into)
        .collect()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::link_previews`] — every link
/// preview in body order with its state name (`resolving` / `resolved` / `failed`),
/// whatever the state. The app's e2e state dump publishes it as
/// `data.feed.posts[].link_previews` so a test can tell a FAILED preview from one
/// still resolving (render-model.md § D4).
#[uniffi::export]
pub fn render_document_link_previews(document: RenderDocument) -> Vec<LinkPreviewStateOwned> {
    document
        .link_previews()
        .into_iter()
        .map(Into::into)
        .collect()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::remote_images`] — every
/// [`RenderBlock::RemoteImage`] in body order, each with its own manager-projected
/// `revealed` flag. Returns the owned [`RemoteImageRefOwned`] mirror since the borrowed
/// [`fauna_core::render::RemoteImageRef`] carries a lifetime that can't cross UniFFI (see
/// [`render_document_quoted_post`] for why).
#[uniffi::export]
pub fn render_document_remote_images(document: RenderDocument) -> Vec<RemoteImageRefOwned> {
    document
        .remote_images()
        .into_iter()
        .map(Into::into)
        .collect()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::proxied_images`] — every
/// [`RenderBlock::ProxiedImage`] in body order: a bridged post's own pictures, each a
/// nest-relative path the app fetches from its own nest with the session bearer and paints in
/// the `post-image` slot, immediately (render-model.md § D6c). Returns the owned
/// [`ProxiedImageRefOwned`] mirror (see [`render_document_quoted_post`] for why).
#[uniffi::export]
pub fn render_document_proxied_images(document: RenderDocument) -> Vec<ProxiedImageRefOwned> {
    document
        .proxied_images()
        .into_iter()
        .map(Into::into)
        .collect()
}

/// UniFFI face of [`fauna_core::render::RenderDocument::proxied_videos`] — every
/// [`RenderBlock::ProxiedVideo`] in body order: a bridged post's own videos, each a
/// nest-relative path the app paints in its `video-thumbnail` slot without byte-loading it
/// (render-model.md § D6c → *Proxied video*). Returns the owned [`ProxiedVideoRefOwned`]
/// mirror (see [`render_document_quoted_post`] for why).
#[uniffi::export]
pub fn render_document_proxied_videos(document: RenderDocument) -> Vec<ProxiedVideoRefOwned> {
    document
        .proxied_videos()
        .into_iter()
        .map(Into::into)
        .collect()
}

/// UniFFI face of [`fauna_core::render::inline_line_runs`] — a paragraph's inline run
/// split at its hard line breaks into runs of at most
/// [`fauna_core::render::MAX_LINES_PER_TEXT_RUN`] lines, so a native shell hands no
/// single text widget a pathological line count (render-model.md § Where logic lives).
/// A run within the budget comes back as one run, equal to the input.
#[uniffi::export]
pub fn render_inline_line_runs(inlines: Vec<Inline>) -> Vec<Vec<Inline>> {
    fauna_core::render::inline_line_runs(&inlines)
}

/// UniFFI face of [`fauna_core::render::text_line_runs`] — the same split for a code
/// block's unstyled text.
#[uniffi::export]
pub fn render_text_line_runs(text: String) -> Vec<String> {
    fauna_core::render::text_line_runs(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::render::{Inline, PreviewState, RenderBlock};

    #[test]
    fn delegates_and_strips_markdown_markers() {
        // A bold run + trailing text flattens to marker-free plaintext — the
        // exact property the e2e markdown reads assert (`"**" not in text`).
        let doc = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![
                    Inline::Bold {
                        inlines: vec![Inline::Text {
                            text: "bold".into(),
                        }],
                    },
                    Inline::Text {
                        text: " body".into(),
                    },
                ],
            }],
        };
        assert_eq!(render_document_to_plaintext(doc), "bold body");
    }

    #[test]
    fn detects_a_blocked_remote_image_at_any_depth() {
        // A `Resolved` link-preview og:image that is not yet revealed is blocked
        // remote content — and the shared walk finds it even nested inside a block
        // quote, the any-depth recursion a top-level-only hand-roll would miss.
        let blocked_nested_preview = RenderDocument {
            blocks: vec![RenderBlock::BlockQuote {
                blocks: vec![RenderBlock::LinkPreview {
                    url: "https://example.com".into(),
                    state: PreviewState::Resolved {
                        title: "t".into(),
                        description: String::new(),
                        image_hash: Some("abcd".into()),
                        revealed: false,
                    },
                }],
            }],
        };
        assert!(render_document_has_blocked_remote_images(
            blocked_nested_preview
        ));

        // Revealed → not blocked; marker-only doc → not blocked.
        let revealed = RenderDocument {
            blocks: vec![RenderBlock::LinkPreview {
                url: "https://example.com".into(),
                state: PreviewState::Resolved {
                    title: "t".into(),
                    description: String::new(),
                    image_hash: Some("abcd".into()),
                    revealed: true,
                },
            }],
        };
        assert!(!render_document_has_blocked_remote_images(revealed));

        let plain = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text { text: "hi".into() }],
            }],
        };
        assert!(!render_document_has_blocked_remote_images(plain));
    }

    #[test]
    fn first_image_hash_finds_a_nested_image() {
        let doc = RenderDocument {
            blocks: vec![RenderBlock::BlockQuote {
                blocks: vec![RenderBlock::Image {
                    hash: "abcd".into(),
                    alt: String::new(),
                }],
            }],
        };
        assert_eq!(
            render_document_first_image_hash(doc),
            Some("abcd".to_string())
        );

        let none = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text { text: "hi".into() }],
            }],
        };
        assert_eq!(render_document_first_image_hash(none), None);
    }

    #[test]
    fn first_video_hash_finds_a_nested_video() {
        // The exact twin of first_image_hash_finds_a_nested_image above.
        let doc = RenderDocument {
            blocks: vec![RenderBlock::BlockQuote {
                blocks: vec![RenderBlock::Video {
                    hash: "feedbeef".into(),
                    alt: String::new(),
                }],
            }],
        };
        assert_eq!(
            render_document_first_video_hash(doc),
            Some("feedbeef".to_string())
        );

        let none = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text { text: "hi".into() }],
            }],
        };
        assert_eq!(render_document_first_video_hash(none), None);
    }

    #[test]
    fn quoted_post_crosses_as_an_owned_mirror() {
        use fauna_core::render::{AuthoringOriginStatus, VerificationStatus};

        let doc = RenderDocument {
            blocks: vec![RenderBlock::QuotedPost {
                post_id: "post-1".into(),
                author: "alice".into(),
                body: "hello".into(),
                verification: VerificationStatus::Verified,
                authoring_origin: AuthoringOriginStatus::Delegated,
                legal_takedown_ref: None,
                not_found: false,
            }],
        };
        let owned = render_document_quoted_post(doc).expect("a QuotedPost block folds");
        assert_eq!(owned.post_id, "post-1");
        assert_eq!(owned.author, "alice");
        // The D10 origin must survive the owned-mirror crossing too — an FFI
        // face that dropped it would leave the native apps unable to paint the
        // quoted embed's delegated-origin badge.
        assert_eq!(owned.authoring_origin, AuthoringOriginStatus::Delegated);
        assert_eq!(owned.body, "hello");
        assert_eq!(owned.verification, VerificationStatus::Verified);
        assert_eq!(owned.legal_takedown_ref, None);

        let none = RenderDocument {
            blocks: vec![RenderBlock::Paragraph { inlines: vec![] }],
        };
        assert!(render_document_quoted_post(none).is_none());
    }

    #[test]
    fn resolving_link_preview_urls_collects_only_the_resolving_ones() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::LinkPreview {
                    url: "https://resolving.example".into(),
                    state: PreviewState::Resolving,
                },
                RenderBlock::LinkPreview {
                    url: "https://resolved.example".into(),
                    state: PreviewState::Resolved {
                        title: "t".into(),
                        description: String::new(),
                        image_hash: None,
                        revealed: false,
                    },
                },
            ],
        };
        assert_eq!(
            render_document_resolving_link_preview_urls(doc),
            vec!["https://resolving.example".to_string()]
        );
    }

    #[test]
    fn link_previews_cross_every_state_by_name_in_body_order() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::LinkPreview {
                    url: "https://failed.example".into(),
                    state: PreviewState::Failed,
                },
                RenderBlock::LinkPreview {
                    url: "https://resolving.example".into(),
                    state: PreviewState::Resolving,
                },
                RenderBlock::LinkPreview {
                    url: "https://resolved.example".into(),
                    state: PreviewState::Resolved {
                        title: "t".into(),
                        description: String::new(),
                        image_hash: None,
                        revealed: false,
                    },
                },
            ],
        };
        let states: Vec<(String, String)> = render_document_link_previews(doc)
            .into_iter()
            .map(|p| (p.url, p.state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("https://failed.example".into(), "failed".into()),
                ("https://resolving.example".into(), "resolving".into()),
                ("https://resolved.example".into(), "resolved".into()),
            ]
        );
    }

    #[test]
    fn resolved_link_previews_cross_as_owned_mirrors() {
        let doc = RenderDocument {
            blocks: vec![RenderBlock::LinkPreview {
                url: "https://example.com".into(),
                state: PreviewState::Resolved {
                    title: "Title".into(),
                    description: "Desc".into(),
                    image_hash: Some("abcd".into()),
                    revealed: true,
                },
            }],
        };
        let previews = render_document_resolved_link_previews(doc);
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].url, "https://example.com");
        assert_eq!(previews[0].title, "Title");
        assert_eq!(previews[0].description, "Desc");
        assert_eq!(previews[0].image_hash, Some("abcd".to_string()));
        assert!(previews[0].revealed);
    }

    #[test]
    fn remote_images_finds_one_nested_in_a_quote_a_list_item_and_a_task_item() {
        use fauna_core::render::TaskItem;

        let remote = |url: &str, alt: &str, revealed: bool| RenderBlock::RemoteImage {
            url: url.into(),
            alt: alt.into(),
            revealed,
        };
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::BlockQuote {
                    blocks: vec![remote("https://quoted.example/img.png", "quoted", false)],
                },
                RenderBlock::ListBlock {
                    ordered: false,
                    items: vec![RenderDocument {
                        blocks: vec![remote("https://listed.example/img.png", "listed", true)],
                    }],
                },
                RenderBlock::TaskList {
                    items: vec![TaskItem {
                        checked: false,
                        blocks: vec![remote("https://tasked.example/img.png", "tasked", false)],
                    }],
                },
            ],
        };

        let images = render_document_remote_images(doc);
        assert_eq!(
            images.iter().map(|i| i.url.as_str()).collect::<Vec<_>>(),
            vec![
                "https://quoted.example/img.png",
                "https://listed.example/img.png",
                "https://tasked.example/img.png",
            ],
            "a remote image nested inside a block quote, a list item AND a task item must \
             all be found — a top-level-only twin would silently miss all three"
        );
        assert_eq!(
            images.iter().map(|i| i.revealed).collect::<Vec<_>>(),
            vec![false, true, false],
        );

        let none = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text { text: "hi".into() }],
            }],
        };
        assert!(render_document_remote_images(none).is_empty());
    }
}

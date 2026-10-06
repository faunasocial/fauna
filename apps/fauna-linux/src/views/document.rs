//! GTK per-block widget renderer for a shared [`RenderDocument`] (render-model.md § D1/D6).
//!
//! **The body is produced once, in shared Rust** (`fauna_core::render`): the conversations
//! manager builds `MessageSnapshot.document` from `(body, body_format)` via
//! `document_for_body`, and the feed manager builds `PostSummary.document` from the post body
//! via `markdown_to_document` (D6). This module is the thin GTK *render* layer **shared by
//! both pages**: it walks the typed `RenderBlock`/`Inline` tree into a body widget — a
//! vertical `gtk::Box` of per-block `gtk::Label`s (inline styling via Pango markup; block
//! styling via CSS `.md-*` classes in `style.css`), with remote `![]()` images painted from
//! the block's authoritative `revealed` flag — blocked placeholder when `false`, fetched
//! picture when `true` (the manager owns the reveal state, render-model.md § D3). No client
//! re-parses or re-formats the body — it only paints the document the manager already built
//! (render-model.md § The boundary). It lives under `views/` (not `views/conversations/`)
//! because the Feed page paints its post bodies through this same walker — the priority
//! #1/#4 consolidation D6 exists for (retiring the bespoke per-page flat-text body
//! renderers, e.g. the feed's hand-rolled `> ` blockquote splitter).
//!
//! This replaced the former `markdown::render_to_widget`, which re-parsed `msg.body` at
//! render time. The shared producer (`markdown_to_document`) already **promotes** every
//! remote image out of its paragraph into a sibling `RenderBlock::RemoteImage` in body
//! order, so — unlike the old `MdSpan`-splitting renderer — a `Paragraph`/`Heading` here
//! carries text inlines only and image handling is one flat block arm.
//!
//! Why labels, not a single `GtkTextView`: a `GtkTextView` reports its *minimum* height at
//! its *minimum* width (longest word → many wrapped lines), and on an in-place message-list
//! rebuild GTK uses that min-width height before the real width is pinned, so bubbles
//! flashed/stuck too-tall (whitespace below the text) or, under overflow, clipped to a few
//! px. `GtkLabel` computes its height-for-width correctly on the first measure.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;

use fauna_core::render::{
    AuthoringOriginStatus, Inline, RenderBlock, RenderDocument, ResolvedLinkPreview,
    VerificationStatus,
};
use gtk::prelude::*;

use crate::client::FaunaClient;
use crate::media_loads::{self, Ask, DocImage, MediaScope};
use crate::nest_content_api::ApiError;
use fauna_core::load_cache::Finished;

/// Render a [`RenderDocument`] into a vertical `gtk::Box` of per-block `gtk::Label`s — the
/// body widget for a conversation message bubble **or** a feed post card. Each remote `![]()`
/// image is painted from its own authoritative `revealed` flag — a blocked placeholder when
/// `false`, a fetched picture when `true` — so the manager-owned reveal state is the single
/// source of truth (render-model.md § D3); `rt` drives the on-demand fetch for a revealed
/// image, and `scope` names whose per-url cache it reads, so a rebuild of an already-painted
/// body contacts no image host again ([`crate::media_loads`]). Covers every block kind the producers emit — paragraph, heading, (un)ordered list,
/// fenced code block, block quote, remote image, and the `Attachment` embed (D2; appended
/// after the body by the conversations manager); the trusted inline `Image` block arrives
/// with a later phase. (A feed `PostSummary.document` from `markdown_to_document` emits only
/// the text + `RemoteImage` arms — never `Attachment` / `Image`, which are
/// conversations-manager-resolved — so the feed walk never touches the
/// `crate::conversations::manager()` paths below.)
///
/// A text block over the shared line budget paints as lazily laid-out line runs
/// ([`crate::views::line_runs`]), whose off-screen labels hold no text yet. A body with one
/// therefore declares its automation read from the document — the same seam as windows'
/// `DocumentBodyView` (render-model.md § Implementation status today, read-path uniformity) —
/// on this box, which is the widget every caller puts its test id on. A body with none is
/// untouched: same widgets, same inferred read as before.
pub fn render_to_widget(
    doc: &RenderDocument,
    rt: &tokio::runtime::Handle,
    scope: &MediaScope,
) -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let split = Cell::new(false);
    let walk = Walk {
        rt,
        scope,
        split: &split,
    };
    for block in &doc.blocks {
        render_block_into(&container, block, "", &[], &walk);
    }
    if split.get() {
        crate::testid::set_test_text(&container, &doc.to_plaintext());
    }
    container
}

/// What every block of one [`render_to_widget`] walk shares: `rt` drives the on-demand fetch
/// for a revealed remote image, `scope` names the cache it reads, and `split` is set when any
/// block painted as line runs.
struct Walk<'a> {
    rt: &'a tokio::runtime::Handle,
    scope: &'a MediaScope,
    split: &'a Cell<bool>,
}

/// Render one [`RenderBlock`] into `container`. `prefix` is literal text prepended to the
/// block's label (a list bullet/number — only set by the `List` arm); `classes` are CSS
/// classes carried by the block's label(s) (e.g. `md-blockquote`, `md-list-indent`).
fn render_block_into(
    container: &gtk::Box,
    block: &RenderBlock,
    prefix: &str,
    classes: &[&str],
    walk: &Walk<'_>,
) {
    let split = walk.split;
    match block {
        RenderBlock::Paragraph { inlines } => {
            container.append(&inline_block(prefix, inlines, classes, split));
        }
        RenderBlock::Heading { level, inlines } => {
            let class = match level {
                1 => "md-heading1",
                2 => "md-heading2",
                _ => "md-heading3",
            };
            container.append(&inline_block(prefix, inlines, &[class], split));
        }
        // Code-block text is literal: escape it so no markup is interpreted; monospace +
        // background come from the CSS class.
        RenderBlock::CodeBlock { text, .. } => {
            let runs = fauna_core::render::text_line_runs(text);
            let markups = runs
                .iter()
                .map(|run| glib::markup_escape_text(run).to_string())
                .collect();
            container.append(&text_block(markups, &["md-code-block"], split));
        }
        RenderBlock::BlockQuote { blocks } => {
            for b in blocks {
                render_block_into(container, b, "", &["md-blockquote"], walk);
            }
        }
        RenderBlock::ListBlock { ordered, items } => {
            for (i, item) in items.iter().enumerate() {
                // The shared model carries no item numbers — an ordered list renumbers
                // from 1. Each item is a sub-document; the bullet/number prefixes its first
                // block, and every block is indented.
                let item_prefix = if *ordered {
                    format!("{}.\u{00a0}", i + 1)
                } else {
                    "\u{2022}\u{00a0}".to_string()
                };
                for (j, b) in item.blocks.iter().enumerate() {
                    let p = if j == 0 { item_prefix.as_str() } else { "" };
                    render_block_into(container, b, p, &["md-list-indent"], walk);
                }
            }
        }
        // A GFM task list (render-model.md § D7a): like a bullet list, but each item's first
        // block is prefixed with a static checked/unchecked box glyph (read-side render — the
        // editable checkbox is the Notes editor's job). Sibling of `ListBlock`; same indent.
        RenderBlock::TaskList { items } => {
            for item in items {
                let item_prefix = if item.checked {
                    "\u{2611}\u{00a0}"
                } else {
                    "\u{2610}\u{00a0}"
                };
                for (j, b) in item.blocks.iter().enumerate() {
                    let p = if j == 0 { item_prefix } else { "" };
                    render_block_into(container, b, p, &["md-list-indent"], walk);
                }
            }
        }
        // A remote `![alt](url)` image (render-model.md § D3): the block's `revealed` flag —
        // owned by the manager and projected onto the document — is the single source of
        // truth. When `false` paint the blocked placeholder (the only way a tracking pixel
        // ever loads is the per-message/per-post reveal button — html-mail Slice 3); when
        // `true` paint the fetched picture directly. No client-side reveal state, no
        // swappable slot: the reveal button dispatches to the manager, whose re-emit drives
        // the observer rebuild that re-walks this document with `revealed: true`.
        RenderBlock::RemoteImage { url, alt, revealed } => {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.set_halign(gtk::Align::Start);
            if *revealed {
                row.append(&build_remote_image(walk.rt, url, walk.scope));
            } else {
                row.append(&build_blocked_image_placeholder(alt));
            }
            container.append(&row);
        }
        // Trusted, already-fetched media addressed by content hash. The conversations
        // markdown/plaintext producers never emit it; it arrives when a later phase projects
        // inline media into the document (the bytes loader is wired there). Until then
        // show the alt as a caption so the match stays exhaustive without dead widget code.
        RenderBlock::Image { alt, .. } => {
            if !alt.is_empty() {
                container.append(&block_label(
                    glib::markup_escape_text(alt).as_str(),
                    &["caption", "dim-label"],
                ));
            }
        }
        // The typed video sibling (render-model.md § Implementation status today). Inert here
        // for the same reason `Image` is: the feed *page* paints the `video-thumbnail` element
        // from `first_video_hash()`, and painting it here as well would double-render it.
        //
        // A bridged post's nest-served picture (render-model.md § D6c) is the same: the feed
        // page owns the `post-image` slot and paints it (`build_post_proxied_image`). A
        // bridged video (`ProxiedVideo`, D6c → *Proxied video*) likewise: the page paints its
        // `video-thumbnail`.
        RenderBlock::Video { alt, .. }
        | RenderBlock::ProxiedImage { alt, .. }
        | RenderBlock::ProxiedVideo { alt, .. } => {
            if !alt.is_empty() {
                container.append(&block_label(
                    glib::markup_escape_text(alt).as_str(),
                    &["caption", "dim-label"],
                ));
            }
        }
        // A first-class attachment (render-model.md § D2). The conversations manager appends
        // one `Attachment` block per `msg.attachments` entry after the body, so attachments
        // render in body order as part of this one document walk — no separate bubble-level
        // loop. `is_image` picks the picture-vs-file element; the bytes resolve through the
        // same shared loader (`attachment_bytes(blob_hash)`).
        RenderBlock::Attachment {
            blob_hash,
            filename,
            size_bytes,
            is_image,
            c2pa,
            ..
        } => {
            container.append(&build_attachment(
                blob_hash,
                filename,
                *size_bytes,
                *is_image,
            ));
            // The receiver's own per-attachment verdict — shared Rust probed the
            // bytes (`AttachmentSnapshot.c2pa`), so the badge sits on the attachment
            // it vouches for, never on the whole message (`conversations.md`
            // § Attachments "C2PA on-device").
            if *c2pa {
                container.append(&c2pa_attachment_badge());
            }
        }
        // A feed quoted-post embed (render-model.md § D6). The feed manager folds
        // this in after the body once `resolve_quoted_post` resolves the quote, so
        // the card paints from the block's own `author`/`body` — no manager call
        // and no async resolve here (the feed page triggers the resolution
        // fire-once and re-renders on the manager re-emit). Replaces the former
        // feed-page `build_quoted_post_embed` sibling widget. The conversations
        // producers never emit it, so the conversations bubble never hits this arm.
        RenderBlock::QuotedPost {
            author,
            body,
            verification,
            authoring_origin,
            legal_takedown_ref,
            not_found,
            ..
        } => {
            // A quote with nothing to show paints one line in place of author +
            // body: the legal-takedown tombstone, or — its author deleted it
            // (`ui/feed.md` § Post deletion: references dangle by design) — the
            // not-found state.
            let placeholder = match legal_takedown_ref {
                Some(reference) => Some(
                    crate::i18n::strings::moderation::legal_takedown::tombstone(reference),
                ),
                None if *not_found => {
                    Some(crate::i18n::strings::feed::post::POST_NOT_FOUND.to_string())
                }
                None => None,
            };
            container.append(&build_quoted_post_card(
                author,
                body,
                *verification,
                *authoring_origin,
                placeholder.as_deref(),
            ));
        }
        // An in-bubble reply-quote (render-model.md § D2 QuotedMessage). The
        // conversations manager folds this in at read time (`thread_detail`) when a
        // message replies to a parent loaded in the same thread, PREPENDED — so the
        // in-order walk paints it above the body. Hidden (no block) when the parent
        // isn't loaded. The feed producers never emit it.
        RenderBlock::QuotedMessage {
            author_display,
            snippet,
        } => {
            container.append(&build_reply_quote_card(author_display, snippet));
        }
        // A link-preview embed (render-model.md § D4). The shared producer emits one in
        // `Resolving` for a standalone bare-url paragraph, leaving the inline link in the
        // paragraph above — so the link is ALREADY painted and this block degrades to a
        // no-op until the per-app card + manager resolve-call land. Painting a skeleton here would show a perpetual
        // loading state (no client wires `fauna.linkpreview.resolve` yet), strictly worse
        // than the already-visible link; the real card paints `Resolving`→skeleton,
        // `Resolved`→full card, `Failed`→plain link once the manager re-emits.
        RenderBlock::LinkPreview { .. } => {}
    }
}

/// Build the in-bubble reply-quote card for a [`RenderBlock::QuotedMessage`]
/// (render-model.md § D2): the parent author (caption-heading) over a ≤ 2-line,
/// ellipsized snippet of the parent body. The content is carried in the block —
/// the conversations manager projected it in `thread_detail` — so this is a pure
/// paint. Mirrors [`build_quoted_post_card`]; identical shape on every app
/// (priority #1). Tagged `dm-message-quote` for the cross-app e2e.
fn build_reply_quote_card(author: &str, snippet: &str) -> gtk::Box {
    let embed = gtk::Box::new(gtk::Orientation::Vertical, 2);
    embed.add_css_class("card");
    embed.set_margin_top(4);
    embed.set_margin_bottom(4);
    crate::testid::set_test_id(&embed, ids::DM_MESSAGE_QUOTE);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
    inner.set_margin_top(6);
    inner.set_margin_bottom(6);
    inner.set_margin_start(8);
    inner.set_margin_end(8);

    let author_label = gtk::Label::new(Some(author));
    author_label.set_halign(gtk::Align::Start);
    author_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    author_label.add_css_class("caption-heading");
    inner.append(&author_label);

    let snippet_label = gtk::Label::new(Some(snippet));
    snippet_label.set_halign(gtk::Align::Start);
    snippet_label.set_wrap(true);
    snippet_label.set_xalign(0.0);
    // Clamp the parent snippet to ≤ 2 lines with a trailing ellipsis (the
    // user-approved shape); `set_lines` needs wrap + ellipsize to take effect.
    snippet_label.set_lines(2);
    snippet_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    snippet_label.add_css_class("dim-label");
    inner.append(&snippet_label);

    embed.append(&inner);
    embed
}

/// Build the `quoted-post` embed card for a feed [`RenderBlock::QuotedPost`]: the
/// quoted author (caption-heading) over the (already-truncated) quoted body. The
/// content is carried in the block — the feed manager resolved it via
/// `resolve_quoted_post` and folded the block into the post `document` — so this
/// is a pure paint with no manager/runtime, unlike the former async
/// `build_quoted_post_embed`. Identical structure on every app (priority #1).
///
/// When the *quoted* post's signed envelope could not be verified by this client
/// (`verification == VerificationStatus::Failed`; security.md § Client display of
/// unverified content) the author row also paints the muted "unverified source"
/// badge — the same `unverified-source-badge`, scoped under this card's
/// `quoted-post` id so the cross-app e2e disambiguates it from the focal
/// post's badge. Slice 2b of the unverified-source indicator.
///
/// The same row paints the `delegated-origin-badge` when the *quoted* post was
/// authored by an external app through the D10 delegated sub-key
/// (`authoring_origin == AuthoringOriginStatus::Delegated`;
/// atproto-pds-full.md § D10 → *Audit*) — keyed off the **quoted** post's own
/// origin, independent of the focal post's, and scoped under this card's
/// `quoted-post` id exactly as the unverified badge is.
///
/// When the quoted post has nothing to show — **taken down under a legal
/// obligation** (moderation.md § Categories & enforcement item 1: the shared
/// tombstone "Removed under legal obligation ({reference})") or **deleted by its
/// author** (`ui/feed.md` § Post deletion: "Post not found") — the caller passes
/// that one line as `placeholder`, and the card renders it in place of the
/// (empty) body and omits the author/verification row — never a blank/broken
/// embed.
fn build_quoted_post_card(
    author: &str,
    body: &str,
    verification: VerificationStatus,
    authoring_origin: AuthoringOriginStatus,
    placeholder: Option<&str>,
) -> gtk::Box {
    let embed = gtk::Box::new(gtk::Orientation::Vertical, 2);
    embed.add_css_class("card");
    embed.set_margin_top(4);
    embed.set_margin_bottom(4);
    crate::testid::set_test_id(&embed, ids::QUOTED_POST);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
    inner.set_margin_top(6);
    inner.set_margin_bottom(6);
    inner.set_margin_start(8);
    inner.set_margin_end(8);

    // A taken-down or deleted quote: there is no body to paint, so paint only the
    // one line (no author/verification — there was no envelope to decode).
    if let Some(placeholder) = placeholder {
        let tombstone = gtk::Label::new(Some(placeholder));
        tombstone.set_halign(gtk::Align::Start);
        tombstone.set_wrap(true);
        tombstone.set_xalign(0.0);
        tombstone.add_css_class("dim-label");
        // No dedicated test ID — the tombstone renders inside the existing
        // `quoted-post` card scope (the NEXT: presentation like ContentLabelBadge;
        // a new e2e id would need ui.yaml approval first, § UI Consistency A).
        inner.append(&tombstone);
        embed.append(&inner);
        return embed;
    }

    // Author row: the quoted author over on its own line, with the unverified
    // badge trailing it (the post-card top_line order) when the quote failed
    // verification.
    let author_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let author_label = gtk::Label::new(Some(author));
    author_label.set_halign(gtk::Align::Start);
    author_label.set_hexpand(true);
    author_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    author_label.add_css_class("caption-heading");
    author_row.append(&author_label);
    if let Some(badge) = crate::views::feed::build_unverified_badge(verification) {
        author_row.append(&badge);
    }
    if let Some(badge) = crate::views::feed::build_delegated_origin_badge(authoring_origin) {
        author_row.append(&badge);
    }
    inner.append(&author_row);

    let body_label = gtk::Label::new(Some(body));
    body_label.set_halign(gtk::Align::Start);
    body_label.set_wrap(true);
    body_label.set_xalign(0.0);
    body_label.add_css_class("dim-label");
    inner.append(&body_label);

    embed.append(&inner);
    embed
}

// `has_quoted_post`, `first_image_hash`, `resolved_link_previews`, and
// `resolving_link_preview_urls` — plus their `ResolvedLinkPreview` return type — used to be
// hand-rolled twins here. They're now shared methods/types on `fauna_core::render`
// (`RenderDocument::has_quoted_post`/`first_image_hash`/`resolved_link_previews`/
// `resolving_link_preview_urls`, `fauna_core::render::ResolvedLinkPreview`) — call sites use
// `doc.first_image_hash()` etc. directly. One behavior difference from the old top-level-only
// scan: the shared versions recurse into block quotes / list items / task items, so they can
// find strictly more (never less). The "why extract here instead of painting in the shared
// walker" reasoning (no blob loader in the shared walker; the client's `&Rc<FaunaClient>` blob
// loader lives at the call site) is preserved in the doc comments at each call site (feed
// `post_list.rs`/`post_detail.rs`, conversations `message_bubble.rs`/`detail.rs`) rather than
// here.

/// Async-load the og:image blob for a link-preview card into a `GtkPicture`
/// (tagged `link-preview-image`). The bytes resolve through the client's blob loader
/// (`fetch_blob_bytes` — the og:image is a content-addressed nest blob, render-model.md
/// § D4), the same async-byte-load-stays-client idiom as the feed media image. Only ever
/// built for a `revealed` preview (the D3 reveal gate is decided by the caller).
///
/// The decoded texture comes from `scope`'s per-hash cache ([`crate::media_loads`]): the
/// card holding it is rebuilt far more often than the og:image changes (every feed
/// notification; every change to a conversations message and every visit to its thread), so
/// only the first build to ask for a hash fetches and decodes it.
pub(crate) fn build_preview_image(
    hash: &str,
    client: &Rc<FaunaClient>,
    scope: &MediaScope,
) -> gtk::Picture {
    build_preview_image_from(hash, client.as_ref(), scope)
}

/// Where a document's own-nest image bytes come from — `FaunaClient` in the app, a counting
/// fake in the tests that pin how many a rebuild issues.
pub(crate) trait BlobSource {
    /// `GET /api/v1/blob/<hash>` — the blob's bytes, as fetched.
    fn fetch_blob_bytes(&self, hash: &str) -> async_channel::Receiver<Result<Vec<u8>, ApiError>>;
}

impl BlobSource for FaunaClient {
    fn fetch_blob_bytes(&self, hash: &str) -> async_channel::Receiver<Result<Vec<u8>, ApiError>> {
        FaunaClient::fetch_blob_bytes(self, hash)
    }
}

fn build_preview_image_from(
    hash: &str,
    source: &dyn BlobSource,
    scope: &MediaScope,
) -> gtk::Picture {
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_size_request(-1, 180);
    picture.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&picture, ids::LINK_PREVIEW_IMAGE);

    match media_loads::ask_doc_image(scope, DocImage::Preview, hash, &picture) {
        Ask::Ready(texture) => picture.set_paintable(Some(&texture)),
        Ask::Start => {
            let rx = source.fetch_blob_bytes(hash);
            let scope = scope.clone();
            let hash = hash.to_string();
            glib::spawn_future_local(async move {
                // A refused GET or undecodable bytes are settled; a nest that could not serve
                // it right now (or a runtime torn down under the fetch) is forgotten, so the
                // next build asks again (`fauna_core::load_cache`'s module doc).
                let outcome = match rx.recv().await {
                    Ok(Ok(bytes)) => decode_texture(bytes),
                    Ok(Err(e)) if e.is_transient() => Finished::Transient,
                    Ok(Err(_)) => Finished::Failed,
                    Err(_) => Finished::Transient,
                };
                media_loads::finish_doc_image(&scope, DocImage::Preview, &hash, outcome);
            });
        }
        Ask::Waiting | Ask::Failed => {}
    }

    picture
}

/// Decode fetched image bytes into a texture on the GTK thread — `Failed` when they are
/// not an image GTK can read.
fn decode_texture(bytes: Vec<u8>) -> Finished<gtk::gdk::Texture> {
    gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes))
        .ok()
        .into()
}

/// Build a D4 link-preview card (render-model.md § D4) from a Resolved `LinkPreview`
/// block: og:image (blocked-by-default — painted only when `revealed`, the D3 twin),
/// title, description, and domain (the shared `fauna_core::format::url_host` — host
/// without scheme/port, identical to the native `FaunaFfiMethods.UrlHost` peers and the
/// web `new URL().hostname`). The whole card is a `GtkLinkButton` opening `url`. The
/// og:image blob is loaded through `client` (the shared walker has no blob loader).
///
/// Shared by **both** pages (priority #1/#4): the Feed post card
/// ([`crate::views::feed::post_list`]) and the Conversations message bubble
/// ([`crate::views::conversations::message_bubble`]) paint identical cards, one per Resolved
/// `LinkPreview` block in their respective document.
pub(crate) fn build_link_preview_card(
    lp: &ResolvedLinkPreview<'_>,
    client: &Rc<FaunaClient>,
    scope: &MediaScope,
) -> gtk::Widget {
    let card = gtk::LinkButton::new(lp.url);
    card.set_margin_top(4);
    card.set_margin_bottom(4);
    card.set_halign(gtk::Align::Start);
    card.add_css_class("card");
    crate::testid::set_test_id(&card, ids::LINK_PREVIEW_CARD);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
    inner.set_margin_top(8);
    inner.set_margin_bottom(8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);

    // og:image — blocked-by-default (render-model.md § D4): paint only when the message's
    // remote content is revealed; otherwise the `load-remote-content-button` (driven by the
    // og:image via `has_blocked_remote_images`) reveals it and the card re-renders.
    if lp.revealed
        && let Some(hash) = lp.image_hash
    {
        inner.append(&build_preview_image(hash, client, scope));
    }
    if !lp.title.is_empty() {
        let title = gtk::Label::new(Some(lp.title));
        title.set_halign(gtk::Align::Start);
        title.set_wrap(true);
        title.set_xalign(0.0);
        title.add_css_class("heading");
        crate::testid::set_test_id(&title, ids::LINK_PREVIEW_TITLE);
        inner.append(&title);
    }
    if !lp.description.is_empty() {
        let desc = gtk::Label::new(Some(lp.description));
        desc.set_halign(gtk::Align::Start);
        desc.set_wrap(true);
        desc.set_xalign(0.0);
        desc.set_lines(2);
        desc.set_ellipsize(gtk::pango::EllipsizeMode::End);
        desc.add_css_class("dim-label");
        crate::testid::set_test_id(&desc, ids::LINK_PREVIEW_DESCRIPTION);
        inner.append(&desc);
    }
    let domain = gtk::Label::new(Some(&fauna_core::format::url_host(lp.url)));
    domain.set_halign(gtk::Align::Start);
    domain.add_css_class("caption");
    domain.add_css_class("dim-label");
    crate::testid::set_test_id(&domain, ids::LINK_PREVIEW_DOMAIN);
    inner.append(&domain);

    card.set_child(Some(&inner));
    card.upcast()
}

/// Build the widget for a [`RenderBlock::Attachment`]: a `GtkPicture` painted from the
/// resolved bytes for an image, else a file button. Bytes resolve through the shared loader
/// (`attachment_bytes(blob_hash)`, populated by the inbound parse / send echo), the same
/// paint-from-bytes idiom as the revealed-remote-image path; a miss is what asks the receive
/// loop to fetch them again. A picture without decodable bytes — not yet fetched, or evicted
/// with nowhere to refill from — degrades to its DECLARED placeholder under the same id:
/// filename and size, never a bare icon (`conversations.md` § Attachments → *Retention*).
/// The file button carries the same name and size. `dm-attachment-image` answers
/// `get_attr(.., "state")` with `painted` (the picture's paintable) or `placeholder`.
fn build_attachment(
    blob_hash: &str,
    filename: &str,
    size_bytes: u64,
    is_image: bool,
) -> gtk::Widget {
    let declared = format!("{filename} ({})", crate::i18n::byte_size(size_bytes));
    if is_image {
        if let Some(texture) = crate::conversations::manager()
            .attachment_bytes(blob_hash.to_string())
            .and_then(|bytes| {
                gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(bytes)).ok()
            })
        {
            let picture = gtk::Picture::new();
            picture.set_paintable(Some(&texture));
            picture.set_can_shrink(true);
            picture.set_size_request(-1, 200);
            picture.set_halign(gtk::Align::Start);
            picture.set_tooltip_text(Some(&declared));
            crate::testid::set_test_id(&picture, ids::DM_ATTACHMENT_IMAGE);
            return picture.upcast();
        }
        let placeholder = gtk::Label::new(Some(&declared));
        placeholder.set_halign(gtk::Align::Start);
        placeholder.add_css_class("dim-label");
        placeholder.add_css_class("test-attr-state-placeholder");
        crate::testid::set_test_id(&placeholder, ids::DM_ATTACHMENT_IMAGE);
        placeholder.upcast()
    } else {
        let btn = gtk::Button::with_label(&declared);
        btn.add_css_class("flat");
        crate::testid::set_test_id(&btn, ids::DM_ATTACHMENT_FILE);
        btn.upcast()
    }
}

/// The `c2pa-badge` beside an attachment whose bytes carry content credentials — the
/// feed card's label and tooltip, painted off the block's own verdict.
fn c2pa_attachment_badge() -> gtk::Widget {
    let badge = gtk::Label::new(Some(crate::i18n::strings::c2pa::BADGE_LABEL));
    badge.add_css_class("dim-label");
    badge.set_halign(gtk::Align::Start);
    badge.set_tooltip_text(Some(
        crate::i18n::strings::conversations::detail::BADGE_C2PA,
    ));
    crate::testid::set_test_id(&badge, ids::C2PA_BADGE);
    badge.upcast()
}

/// Build the widget for an inline text block: `prefix` (a list marker — escaped) then the
/// Pango markup of `inlines`, carrying `classes`. The shared
/// `fauna_core::render::inline_line_runs` decides whether the block is one label or several
/// — the budget and the styling-preserving split live once, in Rust.
fn inline_block(
    prefix: &str,
    inlines: &[Inline],
    classes: &[&str],
    split: &Cell<bool>,
) -> gtk::Widget {
    let mut markups: Vec<String> = fauna_core::render::inline_line_runs(inlines)
        .iter()
        .map(|run| inlines_to_markup(run))
        .collect();
    if let Some(first) = markups.first_mut() {
        first.insert_str(0, glib::markup_escape_text(prefix).as_str());
    }
    text_block(markups, classes, split)
}

/// One text block from its line runs' markup: a single run — every block within the budget
/// — is the one [`block_label`] it always was; several are the lazily laid-out
/// [`crate::views::line_runs`] box, and `split` records that the body now holds one.
fn text_block(mut markups: Vec<String>, classes: &[&str], split: &Cell<bool>) -> gtk::Widget {
    if markups.len() <= 1 {
        let markup = markups.pop().unwrap_or_default();
        return block_label(&markup, classes).upcast();
    }
    split.set(true);
    crate::views::line_runs::build(markups, classes, |markup| block_label(markup, &[])).upcast()
}

/// A blocked-remote-image placeholder: a "broken image" icon + the alt text and a dim
/// "Remote image blocked" caption. Holds the alt only — never the image bytes, never
/// fetches. Painted while the block's `revealed` flag is `false`; once the user reveals
/// (a manager dispatch flips `revealed`), the observer rebuild re-walks the document and
/// this block paints as a fetched picture instead (html-mail Slice 3 — never auto-fetch
/// untrusted remote refs).
fn build_blocked_image_placeholder(alt: &str) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("blocked-remote-image");
    crate::testid::set_test_id(&row, ids::DOC_REMOTE_IMAGE);

    let icon = gtk::Image::from_icon_name("image-x-generic-symbolic");
    icon.add_css_class("dim-label");
    row.append(&icon);

    if !alt.is_empty() {
        let alt_label = gtk::Label::new(Some(alt));
        alt_label.add_css_class("caption");
        alt_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&alt_label);
    }

    let blocked = gtk::Label::new(Some(
        crate::i18n::strings::conversations::detail::REMOTE_IMAGE_BLOCKED,
    ));
    blocked.add_css_class("caption");
    blocked.add_css_class("dim-label");
    row.append(&blocked);

    row.upcast()
}

/// A wrapping, left-aligned `gtk::Label` carrying Pango `markup` plus the CSS `classes`.
/// `<a href>` links open via `activate-link`.
fn block_label(markup: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_markup(markup);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Fill);
    label.set_selectable(true);
    for c in classes {
        label.add_css_class(c);
    }
    label.connect_activate_link(|_label, uri| {
        let launcher = gtk::UriLauncher::new(uri);
        launcher.launch(gtk::Window::NONE, gio::Cancellable::NONE, |_| {});
        glib::Propagation::Stop
    });
    label
}

/// Convert an inline run (the typed [`Inline`] tree) to a Pango-markup string, every text
/// value escaped. Recurses into nested emphasis/links — a `bold && italic` span the producer
/// nests as `Bold(Italic(Text))` renders `<b><i>…</i></b>`. Style mirrors the shared HTML
/// renderer (`fauna_core::markdown`) and the former `markdown::span_to_markup`: code, link,
/// then bold/italic.
fn inlines_to_markup(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text } => out.push_str(glib::markup_escape_text(text).as_str()),
            Inline::Code { text } => out.push_str(&format!(
                "<span font_family=\"monospace\" background=\"#262626\">{}</span>",
                glib::markup_escape_text(text)
            )),
            Inline::Link { href, inlines } => out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                glib::markup_escape_text(href),
                inlines_to_markup(inlines)
            )),
            Inline::Bold { inlines } => {
                out.push_str("<b>");
                out.push_str(&inlines_to_markup(inlines));
                out.push_str("</b>");
            }
            Inline::Italic { inlines } => {
                out.push_str("<i>");
                out.push_str(&inlines_to_markup(inlines));
                out.push_str("</i>");
            }
        }
    }
    out
}

/// Append the per-message/per-post `load-remote-content-button` below the body, wired to
/// DISPATCH `reveal` (the caller hands a closure that flips the manager's reveal state —
/// `ConversationsManager::reveal_remote_images` for a bubble,
/// `FeedManager::reveal_remote_images` for a post — render-model.md § D3). The button does
/// **not** swap any widget client-side: the manager re-emits, the page's observer rebuild
/// re-walks the document with `revealed: true`, and this button's gate
/// ([`RenderDocument::has_blocked_remote_images`]) then goes false, so it's simply absent
/// from the rebuilt body — no one-shot hide needed.
///
/// The caller gates the call on `doc.has_blocked_remote_images()` so the button appears iff
/// there is at least one still-blocked remote image (the manager's authoritative state),
/// with no second parse and no risk of a button that has nothing to reveal.
pub fn attach_reveal_button(container: &gtk::Box, reveal: impl Fn() + 'static) {
    let button =
        gtk::Button::with_label(crate::i18n::strings::conversations::detail::LOAD_REMOTE_CONTENT);
    button.add_css_class("flat");
    button.set_halign(gtk::Align::Start);
    button.set_margin_top(2);
    crate::testid::set_test_id(&button, ids::LOAD_REMOTE_CONTENT_BUTTON);
    button.connect_clicked(move |_| reveal());
    container.append(&button);
}

/// A `GtkPicture` that lazily fetches a revealed remote image (mirrors the feed's
/// `build_post_image` paint-on-arrival pattern). Uses a fresh `reqwest` GET — NOT
/// the nest-authenticated client — so no Fauna credentials leak to a third-party
/// image host. A failed/blocked fetch simply leaves the picture empty.
///
/// The texture comes from `scope`'s per-url cache ([`crate::media_loads`]), so a rebuilt body
/// never contacts the image host a second time — the host learns of a reveal once per
/// session, not once per repaint. Every failure is settled: the shared fetch cannot tell a
/// transient one apart, and a hostile host must not be able to turn the reader into a repeat
/// caller (`fauna_core::load_cache`'s module doc).
fn build_remote_image(rt: &tokio::runtime::Handle, url: &str, scope: &MediaScope) -> gtk::Widget {
    let rt = rt.clone();
    build_remote_image_from(url, scope, move |url| fetch_remote_image_bytes(&rt, url))
}

fn build_remote_image_from(
    url: &str,
    scope: &MediaScope,
    fetch: impl FnOnce(&str) -> async_channel::Receiver<Result<Vec<u8>, String>>,
) -> gtk::Widget {
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_size_request(-1, 200);
    picture.set_halign(gtk::Align::Start);
    picture.add_css_class("remote-image");
    crate::testid::set_test_id(&picture, ids::DOC_REMOTE_IMAGE);

    match media_loads::ask_doc_image(scope, DocImage::Remote, url, &picture) {
        Ask::Ready(texture) => picture.set_paintable(Some(&texture)),
        Ask::Start => {
            let rx = fetch(url);
            let scope = scope.clone();
            let url = url.to_string();
            gtk::glib::spawn_future_local(async move {
                let outcome = match rx.recv().await {
                    Ok(Ok(bytes)) => decode_texture(bytes),
                    _ => Finished::Failed,
                };
                media_loads::finish_doc_image(&scope, DocImage::Remote, &url, outcome);
            });
        }
        Ask::Waiting | Ask::Failed => {}
    }

    picture.upcast()
}

/// Fetch raw image bytes for a revealed remote image on the tokio runtime,
/// delivering the result to the GTK thread over an `async_channel`.
///
/// The request is `fauna_client::remote_image` — the SHARED one every Rust-native
/// app makes (tui makes the identical call and differs only in painting the bytes
/// as half-block art). Deliberately a bare client with no auth, no cookies and no
/// fauna headers, since the image host is untrusted; and bounded by a request
/// timeout plus an incrementally-checked response cap, so a hostile or broken
/// server cannot hang the fetch or balloon the decode buffer. Those bounds used to
/// be absent here and present only on tui; sharing the call is what keeps them from
/// drifting apart again.
///
/// `Option` collapses to the one `Err` string the GTK side already renders as "no
/// picture": every failure mode means the same thing to the caller — show nothing
/// rather than a broken frame.
fn fetch_remote_image_bytes(
    rt: &tokio::runtime::Handle,
    url: &str,
) -> async_channel::Receiver<Result<Vec<u8>, String>> {
    let (tx, rx) = async_channel::bounded(1);
    let url = url.to_string();
    rt.spawn(async move {
        let result = fauna_client::remote_image::fetch_bytes(&url)
            .await
            .ok_or_else(|| format!("could not load remote image {url}"));
        let _ = tx.send(result).await;
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::widget_names;
    use fauna_core::render::markdown_to_document;

    /// `inlines_to_markup` is pure (Pango-markup string only, no GTK widgets), so it is
    /// unit-testable without a display. The block-level `render_to_widget` needs a live GTK
    /// and is exercised by the conversations e2e instead. We drive it through the shared
    /// producer so the test asserts the end-to-end "manager document → linux markup" path.
    fn markup(md: &str) -> String {
        let doc = markdown_to_document(md);
        match doc.blocks.first() {
            Some(RenderBlock::Paragraph { inlines })
            | Some(RenderBlock::Heading { inlines, .. }) => inlines_to_markup(inlines),
            _ => String::new(),
        }
    }

    #[test]
    fn inline_styles_map_to_pango_markup() {
        assert_eq!(markup("**bold**"), "<b>bold</b>");
        assert_eq!(markup("*italic*"), "<i>italic</i>");
        assert_eq!(markup("***both***"), "<b><i>both</i></b>");
        assert_eq!(
            markup("`code`"),
            "<span font_family=\"monospace\" background=\"#262626\">code</span>"
        );
        assert_eq!(
            markup("[label](https://x.io)"),
            "<a href=\"https://x.io\">label</a>"
        );
    }

    #[test]
    fn markup_escapes_pango_special_chars() {
        // Plain text and link hrefs are escaped so a body can't inject Pango markup.
        assert_eq!(markup("a < b & c"), "a &lt; b &amp; c");
    }

    #[test]
    fn mixed_run_concatenates_styled_spans() {
        assert_eq!(
            markup("Hello **world** and *you*"),
            "Hello <b>world</b> and <i>you</i>"
        );
    }

    #[test]
    fn remote_image_is_not_an_inline() {
        // The producer promotes a remote image to its OWN RemoteImage block, so the first
        // block here is the lone image — no paragraph, no inline markup.
        let doc = markdown_to_document("![alt](https://img.test/c.png)");
        assert!(matches!(
            doc.blocks.first(),
            Some(RenderBlock::RemoteImage { .. })
        ));
    }

    fn numbered_lines(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn paragraph_of(lines: usize) -> RenderDocument {
        RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text {
                    text: numbered_lines(lines),
                }],
            }],
        }
    }

    /// Every `gtk::Label` under `root`, document order.
    fn labels_under(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Label> {
        fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Label>) {
            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                out.push(l.clone());
            }
            let mut c = w.first_child();
            while let Some(child) = c {
                walk(&child, out);
                c = child.next_sibling();
            }
        }
        let mut out = Vec::new();
        walk(root.as_ref(), &mut out);
        out
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// A block within the shared line budget — nearly every real message — paints exactly
    /// as it did before the line-run projection was adopted: its one label, directly in the
    /// body box, the read still inferred from that label.
    #[test]
    fn a_block_within_the_line_budget_paints_as_its_one_label() {
        crate::testid::run_on_gtk_thread(|| {
            let rt = test_runtime();
            let doc = paragraph_of(fauna_core::render::MAX_LINES_PER_TEXT_RUN);
            let body = render_to_widget(&doc, rt.handle(), &MediaScope::conversations());
            let only = body.first_child().expect("the paragraph's label");
            assert!(only.is::<gtk::Label>(), "painted straight into the body");
            assert!(only.next_sibling().is_none());
            assert_eq!(crate::testid::test_text(&body), None);
        });
    }

    /// An over-budget block paints one label per shared line run, and — because a run off
    /// screen is not painted yet — the body's automation read is the DOCUMENT, not a join of
    /// whatever labels happen to be painted (render-model.md § Implementation status today,
    /// the read-path uniformity bullet).
    #[test]
    fn an_over_budget_block_paints_a_label_per_line_run_and_reads_as_the_document() {
        crate::testid::run_on_gtk_thread(|| {
            let rt = test_runtime();
            let budget = fauna_core::render::MAX_LINES_PER_TEXT_RUN;

            let doc = paragraph_of(100);
            let body = render_to_widget(&doc, rt.handle(), &MediaScope::conversations());
            let labels = labels_under(&body);
            assert_eq!(labels.len(), 100usize.div_ceil(budget));
            assert!(labels[0].text().starts_with("line 1\n"));
            // Never mapped, so nothing past the measuring first run has been laid out.
            assert_eq!(labels.last().unwrap().text(), "");
            let read = crate::automation::find::text_of(body.upcast_ref());
            assert_eq!(read, doc.to_plaintext());
            assert!(read.contains("line 100"));

            let code = RenderDocument {
                blocks: vec![RenderBlock::CodeBlock {
                    lang: None,
                    text: numbered_lines(40),
                }],
            };
            let body = render_to_widget(&code, rt.handle(), &MediaScope::conversations());
            assert_eq!(labels_under(&body).len(), 40usize.div_ceil(budget));
        });
    }

    /// The point of the projection on linux: only the runs near the viewport are laid out,
    /// and scrolling lays out the ones it reaches. 200 runs in a 300 px window.
    #[test]
    fn only_the_runs_near_the_viewport_are_painted_and_scrolling_paints_the_rest() {
        crate::testid::run_on_gtk_thread(|| {
            let rt = test_runtime();
            let budget = fauna_core::render::MAX_LINES_PER_TEXT_RUN;
            let body = render_to_widget(
                &paragraph_of(200 * budget),
                rt.handle(),
                &MediaScope::conversations(),
            );
            let labels = labels_under(&body);
            assert_eq!(labels.len(), 200);

            let scroller = gtk::ScrolledWindow::new();
            scroller.set_child(Some(&body));
            let window = gtk::Window::new();
            window.set_default_size(400, 300);
            window.set_child(Some(&scroller));
            window.present();

            // Run 1 is on screen and is not the eagerly-painted measuring run, so its text
            // proves a viewport pass ran — which is what makes the two blanks below mean
            // "not reached" rather than "no pass yet".
            assert!(
                pump_until(|| !labels[1].text().is_empty()),
                "the run under the viewport was never painted"
            );
            assert_eq!(labels[100].text(), "");
            assert_eq!(labels[199].text(), "");

            let adj = scroller.vadjustment();
            adj.set_value(adj.upper() - adj.page_size());
            assert!(
                pump_until(|| !labels[199].text().is_empty()),
                "scrolling to the end never painted the last run"
            );
            assert!(
                labels[199]
                    .text()
                    .ends_with(&format!("line {}", 200 * budget))
            );
            assert_eq!(
                labels[100].text(),
                "",
                "a run never scrolled to stays unpainted"
            );

            window.destroy();
        });
    }

    /// Pump to a deadline on observable state, never a fixed amount of work
    /// (`e2e-conventions.md` point 14) — the `month_grid` tests' shape.
    fn pump_until(cond: impl Fn() -> bool) -> bool {
        let ctx = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if cond() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// `doc-remote-image` (render-model.md § D3) must read on **both** arms of the
    /// `RemoteImage` block — the blocked placeholder is the one a reveal test asserts on
    /// first, so tagging only the revealed picture would re-create the same hole linux had.
    /// Runs on the shared GTK test thread (`run_on_gtk_thread`) so it actually executes
    /// instead of silently no-opping.
    #[test]
    fn remote_image_block_tags_doc_remote_image_in_both_states() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = gtk::init();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            for revealed in [false, true] {
                let doc = RenderDocument {
                    blocks: vec![RenderBlock::RemoteImage {
                        url: "https://img.test/c.png".to_string(),
                        alt: "a cat".to_string(),
                        revealed,
                    }],
                };
                let container = render_to_widget(&doc, rt.handle(), &MediaScope::conversations());
                let names = widget_names(&container);
                assert!(
                    names.iter().any(|n| n == "doc-remote-image"),
                    "revealed={revealed}: doc-remote-image missing; have {names:?}"
                );
            }
        });
    }
}

/// The document images' rebuild cost (`crate::media_loads`): a body or link-preview card
/// rebuilt from the same document must not fetch, or decode, what it already painted.
/// Counts, never durations (convention 14).
#[cfg(test)]
mod image_cache_tests {
    use super::*;
    use std::cell::RefCell;

    type BlobReply = async_channel::Sender<Result<Vec<u8>, ApiError>>;
    type RemoteReply = async_channel::Sender<Result<Vec<u8>, String>>;

    /// A fake byte source: records every blob GET and every remote-image fetch a build
    /// issues and holds each reply until the test sends it.
    #[derive(Default)]
    struct CountingSource {
        gets: RefCell<Vec<BlobReply>>,
        remote: RefCell<Vec<RemoteReply>>,
    }

    impl BlobSource for CountingSource {
        fn fetch_blob_bytes(
            &self,
            _hash: &str,
        ) -> async_channel::Receiver<Result<Vec<u8>, ApiError>> {
            let (tx, rx) = async_channel::bounded(1);
            self.gets.borrow_mut().push(tx);
            rx
        }
    }

    impl CountingSource {
        fn remote_fetch(&self, _url: &str) -> async_channel::Receiver<Result<Vec<u8>, String>> {
            let (tx, rx) = async_channel::bounded(1);
            self.remote.borrow_mut().push(tx);
            rx
        }

        fn gets(&self) -> usize {
            self.gets.borrow().len()
        }

        fn remote_fetches(&self) -> usize {
            self.remote.borrow().len()
        }
    }

    fn png() -> Vec<u8> {
        fauna_media::test_fixtures::build_png(2, 2)
    }

    /// Build a revealed remote image through `source`'s counting fetch.
    fn remote_image(url: &str, scope: &MediaScope, source: &CountingSource) -> gtk::Picture {
        build_remote_image_from(url, scope, |u| source.remote_fetch(u))
            .downcast::<gtk::Picture>()
            .expect("a remote image is a picture")
    }

    /// Pump `ctx` until `done` holds — bounded by iterations, never by a clock.
    fn pump_until(ctx: &glib::MainContext, done: impl Fn() -> bool) -> bool {
        for _ in 0..10_000 {
            if done() {
                return true;
            }
            ctx.iteration(false);
        }
        done()
    }

    /// Run `body` on the GTK test thread under a private main context, so the loads a build
    /// spawns run only when the test pumps them.
    fn on_private_context(body: impl FnOnce(&glib::MainContext) + Send + 'static) {
        crate::testid::run_on_gtk_thread(move || {
            let ctx = glib::MainContext::new();
            ctx.with_thread_default(|| body(&ctx))
                .expect("acquire a private main context");
        });
    }

    /// The og:image repaint pin: a card rebuilt while its og:image loads, and again after it
    /// painted, issues no second blob GET; the rebuild after the paint shows the texture at
    /// once. Before the cache every build of a feed card (per notification) or a bubble (per
    /// message change, per thread visit) re-fetched and re-decoded it.
    #[test]
    fn a_repaint_of_a_painted_og_image_fetches_nothing() {
        on_private_context(|ctx| {
            let scope = MediaScope::conversations();
            let nest = CountingSource::default();
            let hash = hex::encode([0x31u8; 32]);

            let _first = build_preview_image_from(&hash, &nest, &scope);
            let rebuilt_while_loading = build_preview_image_from(&hash, &nest, &scope);
            assert_eq!(
                nest.gets(),
                1,
                "a rebuild while loading must not fetch again"
            );

            let _ = nest.gets.borrow()[0].try_send(Ok(png()));
            assert!(
                pump_until(ctx, || rebuilt_while_loading.paintable().is_some()),
                "the card rebuilt while loading must be painted when the one load lands"
            );

            let repainted = build_preview_image_from(&hash, &nest, &scope);
            assert_eq!(
                nest.gets(),
                1,
                "a repaint of a painted og:image must not fetch"
            );
            assert!(
                repainted.paintable().is_some(),
                "painted from the cache at once"
            );
        });
    }

    /// A nest that could not serve the og:image right now does not blank it for the session:
    /// the next build asks again, and its success paints.
    #[test]
    fn a_transient_og_image_failure_is_retried_by_the_next_build() {
        on_private_context(|ctx| {
            let scope = MediaScope::conversations();
            let nest = CountingSource::default();
            let hash = hex::encode([0x32u8; 32]);

            let _ = build_preview_image_from(&hash, &nest, &scope);
            let _ = nest.gets.borrow()[0].try_send(Err(ApiError::Status {
                code: 503,
                message: "overloaded".into(),
            }));
            while ctx.iteration(false) {}

            let retried = build_preview_image_from(&hash, &nest, &scope);
            assert_eq!(nest.gets(), 2, "the next build must ask again");
            let _ = nest.gets.borrow()[1].try_send(Ok(png()));
            assert!(pump_until(ctx, || retried.paintable().is_some()));
        });
    }

    /// The nest's own answer about the og:image (a `404`) is settled: no rebuild asks again.
    #[test]
    fn a_refused_og_image_is_never_refetched() {
        on_private_context(|ctx| {
            let scope = MediaScope::conversations();
            let nest = CountingSource::default();
            let hash = hex::encode([0x33u8; 32]);

            let _ = build_preview_image_from(&hash, &nest, &scope);
            let _ = nest.gets.borrow()[0].try_send(Err(ApiError::Status {
                code: 404,
                message: "no such blob".into(),
            }));
            while ctx.iteration(false) {}

            let _ = build_preview_image_from(&hash, &nest, &scope);
            assert_eq!(nest.gets(), 1);
        });
    }

    /// The revealed remote-image repaint pin: the third-party host is contacted once per
    /// session, however often the body holding the image is rebuilt.
    #[test]
    fn a_repaint_of_a_revealed_remote_image_contacts_the_host_once() {
        on_private_context(|ctx| {
            let scope = MediaScope::conversations();
            let host = CountingSource::default();
            let url = "https://img.test/cached.png";

            let _first = remote_image(url, &scope, &host);
            let rebuilt_while_loading = remote_image(url, &scope, &host);
            assert_eq!(host.remote_fetches(), 1);

            let _ = host.remote.borrow()[0].try_send(Ok(png()));
            assert!(pump_until(ctx, || rebuilt_while_loading
                .paintable()
                .is_some()));

            let repainted = remote_image(url, &scope, &host);
            assert_eq!(
                host.remote_fetches(),
                1,
                "a repaint must not contact the host"
            );
            assert!(repainted.paintable().is_some());
        });
    }

    /// A remote image that failed is settled, whatever the failure: the host cannot make the
    /// reader call it again by failing.
    #[test]
    fn a_failed_remote_image_is_never_refetched() {
        on_private_context(|ctx| {
            let scope = MediaScope::conversations();
            let host = CountingSource::default();
            let url = "https://img.test/refused.png";

            let _ = remote_image(url, &scope, &host);
            let _ = host.remote.borrow()[0].try_send(Err("timed out".into()));
            while ctx.iteration(false) {}

            let _ = remote_image(url, &scope, &host);
            assert_eq!(host.remote_fetches(), 1);
        });
    }

    /// The og:image cache (by hash) and the remote-image cache (by url) are apart: a url
    /// that happens to spell a hash is not answered from the og:image cache.
    #[test]
    fn the_hash_and_url_caches_never_answer_for_each_other() {
        on_private_context(|ctx| {
            let scope = MediaScope::conversations();
            let source = CountingSource::default();
            let key = hex::encode([0x34u8; 32]);

            let _ = build_preview_image_from(&key, &source, &scope);
            let _ = source.gets.borrow()[0].try_send(Ok(png()));
            while ctx.iteration(false) {}

            let _ = remote_image(&key, &scope, &source);
            assert_eq!(source.remote_fetches(), 1);
        });
    }

    /// The conversations cache is one reader's: the actor-change teardown empties it, and a
    /// load that lands after the teardown is discarded rather than painting for the next
    /// reader — the conversations page's manager outlives the reader, so the teardown epoch
    /// is what scopes it.
    #[test]
    fn the_identity_change_retires_the_conversations_cache() {
        on_private_context(|ctx| {
            let host = CountingSource::default();
            let url = "https://img.test/departed.png";

            let before = MediaScope::conversations();
            let _ = remote_image(url, &before, &host);
            crate::media_loads::clear_for_identity_change();
            let _ = host.remote.borrow()[0].try_send(Ok(png()));
            while ctx.iteration(false) {}

            let after = MediaScope::conversations();
            let card = remote_image(url, &after, &host);
            assert_eq!(
                host.remote_fetches(),
                2,
                "the next reader fetches for itself"
            );
            assert!(
                card.paintable().is_none(),
                "the departed reader's image must not paint for the next one"
            );
        });
    }
}

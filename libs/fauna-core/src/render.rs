//! The shared semantic render model — `RenderDocument`, the one structured body
//! representation every app paints (`docs/goal/architecture/render-model.md`).
//!
//! `RenderDocument` is the typed generalization of the stringly-typed
//! [`crate::markdown::MdBlock`] token model: an ordered list of semantic **blocks**,
//! where text blocks carry inline **spans** and embeds (images, and — in later phases —
//! attachments, quoted messages/posts, link previews) are **first-class blocks**, not
//! sibling snapshot fields. A page manager produces it once (in Rust); each native shell
//! walks the blocks and constructs native widgets. No client re-parses, re-formats, or
//! holds body-render state of its own.
//!
//! **Layered, not a replacement (P1 design call).** This module is built *on top of*
//! [`crate::markdown::parse_markdown`] — it does **not** retype `MdBlock.kind: String`.
//! That string kind is mirrored into `FfiMdBlock` (`fauna-ffi`) and consumed by every
//! app's shipped html-mail render path; retyping it would ripple into all seven apps.
//! So `markdown_to_document` *calls* `parse_markdown` and maps its flat tokens into the
//! typed tree here, leaving the shipped text path byte-identical (render-model.md
//! § "What's already shared (the floor — do not rebuild)").
//!
//! **WASM-safe.** Like [`crate::markdown`], this module pulls no `tokio`/`redb`; both
//! `fauna-conversations` and `fauna-feed` take `fauna-core` with
//! `default-features = false`, and the web app serialises `RenderDocument` over
//! `serde` via `fauna-wasm`.
//!
//! **Exposure** mirrors every other snapshot type: `#[cfg_attr(feature = "uniffi", …)]`
//! `uniffi::Record`/`Enum` for the native apps (registered cross-crate from
//! `fauna-core`, exactly like [`crate::localized::LocalizedText`]) and `serde` for web.
//! The type surfaces in the generated Apple/Android/Windows bindings once a snapshot
//! embeds it (P2: `MessageSnapshot.document`); P1 only proves it compiles through both
//! faces.
//!
//! ## Producers (P1)
//! - [`markdown_to_document`] — markdown source → document (reuses `parse_markdown`).
//! - [`plaintext_to_document`] — a plain-text body → one paragraph per blank-line block,
//!   no inline parsing.
//! - The **inbound-HTML** path is *not* here: html-mail.md already converts HTML→markdown
//!   in `fauna_conversations::html_markdown::html_to_markdown` (which depends on
//!   `fauna-core`, so it can't live here), and the document for an inbound HTML body is
//!   `markdown_to_document(&html_to_markdown(html))`, assembled by the conversations
//!   manager in P2. We deliberately do **not** duplicate that converter (priority #2/#4).
//!
//! ## Not yet modelled (added by their producing phase)
//! The embed nodes whose payloads are defined by downstream snapshot shapes —
//! `Attachment` (⊇ `fauna_conversations::AttachmentSnapshot`), `QuotedMessage`,
//! `QuotedPost` (⊇ feed's `QuotedPostView`), and `LinkPreview` (a P4 feature) — are added
//! when their producer lands (P2/P3/P4), where the field shapes live and the
//! strict-superset rule can be checked. The `Inline::Mention` node likewise waits for a
//! producer (markdown has no mention syntax today). [`PreviewState`] is defined now (it
//! is a leaf the model names and P4's `LinkPreview` will carry it).

use serde::{Deserialize, Serialize};

use crate::markdown::{self, MdBlock, MdLine, MdSpan};

/// An ordered list of semantic [`RenderBlock`]s — one rendered body. The named type
/// (rather than a bare `Vec<RenderBlock>` alias) gives snapshot fields a stable FFI
/// record (`MessageSnapshot.document: RenderDocument`) and lets a list item be "a
/// sub-document" without a second alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RenderDocument {
    pub blocks: Vec<RenderBlock>,
}

/// A block-level node. The text variants are the typed form of today's
/// [`MdBlock`] kinds; `Image`/`RemoteImage` are the embeds markdown produces (every
/// `![alt](url)` is a **remote** http(s) image, so the markdown producer only ever emits
/// `RemoteImage` — `Image` carries already-fetched, trusted media from a snapshot
/// producer in a later phase). Embeds appear **in body order**, never appended after the
/// text by client glue (render-model.md § D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RenderBlock {
    /// A paragraph of inline content.
    Paragraph { inlines: Vec<Inline> },
    /// An ATX heading, `level` 1–4 (mirrors `MdBlock`'s heading levels).
    Heading { level: u8, inlines: Vec<Inline> },
    /// An (un)ordered list. Each item is a sub-[`RenderDocument`] so an item can hold
    /// block-level content (the richest existing pattern — priority #4); a markdown item
    /// is one `Paragraph`. An ordered list renumbers from 1 at render (it carries no
    /// item numbers), matching `<ol>` and `markdown::parse_markdown`.
    ///
    /// Named `ListBlock`, not `List`: a `uniffi::Enum` variant named `List` generates a
    /// nested Kotlin class `RenderBlock.List` that **shadows `kotlin.collections.List`**,
    /// so every `List<…>` field in the generated sealed class fails to compile (caught when
    /// android first walked the document — render-model.md § The model).
    ListBlock {
        ordered: bool,
        items: Vec<RenderDocument>,
    },
    /// A GFM task list (`- [ ] …` / `- [x] …`) — a **sibling** of [`ListBlock`](Self::ListBlock)
    /// so a plain bullet list is untouched (render-model.md § D7a). Each [`TaskItem`] is a
    /// sub-document exactly like a `ListBlock` item, so nested and mixed bullet/checkbox lists
    /// compose: the markdown producer folds a run of task items into one `TaskList` and an
    /// adjacent run of plain bullets into a sibling `ListBlock`. Named `TaskList` (collision-free
    /// in all four bindings, unlike `List` → `ListBlock`). The read-side walk paints a **static**
    /// checked/unchecked box per item; the *interactive* checkbox is the Notes editor's job
    /// (tracked internally, § D7), not this display
    /// render. Not produced by any snapshot projection — only the markdown body producer emits it.
    TaskList { items: Vec<TaskItem> },
    /// A fenced code block. `lang` is the info-string language (always `None` from
    /// markdown today — `parse_markdown` does not capture it yet); `text` is the raw,
    /// un-rendered code.
    CodeBlock { lang: Option<String>, text: String },
    /// A block quote. Markdown emits one quoted line per block; the nested `blocks` shape
    /// admits richer quotes from future producers.
    BlockQuote { blocks: Vec<RenderBlock> },
    /// Trusted, already-fetched media addressed by content hash. **Not** produced by the
    /// markdown path (markdown images are remote); emitted by the attachment/media
    /// producers in P2/P3.
    Image { hash: String, alt: String },
    /// Trusted, already-fetched **video** addressed by content hash — the typed sibling of
    /// [`Image`](Self::Image) (render-model.md § Implementation status today → *the D6 media
    /// fold is UNTYPED*, closed by the D7a new-variant recipe). Before this variant existed
    /// the feed's media fold reduced every `MediaItem` to its blob hash alone, so no
    /// document-painting app could tell an mp4 from a png and `video-thumbnail` was
    /// unbuildable on all 7 apps; web painted it only by calling `decode_post` a *second*
    /// time in app code and branching on `media_type` itself.
    ///
    /// A **sibling variant, not a field on `Image`**, exactly as D7a prescribes: being new it
    /// breaks the exhaustive app walkers at compile time (the safety net), while leaving every
    /// existing `Image` construction and paint untouched.
    ///
    /// Carries the same two fields as [`Image`](Self::Image) and no more, because § The boundary
    /// admits structure/role/content/state and never a player: an app paints its
    /// `video-thumbnail` from `hash` through its own blob loader, the same async byte-load as
    /// `post-image`. Deliberately **no** poster/dimensions field — `MediaItem::thumbnail` and
    /// `::dimensions` are `None` from every writer we have (see `fauna_feed`'s
    /// `media_item_from_staged`), so such a field would be dead on arrival; it is additive
    /// later if a writer ever populates one. Deliberately no mime either: the image-vs-video
    /// branch is made *once* in the shared fold, which is the whole point of the variant.
    Video { hash: String, alt: String },
    /// A bridged post's own image attachment, served by the reader's OWN nest at a
    /// nest-relative, already-proxied `path` (`/api/v1/media/proxy?url=…` for ActivityPub
    /// and nostr, `/api/v1/bluesky/media?url=…` for Bluesky) — never an absolute URL
    /// (render-model.md § D6c; the path form is bridges.md § Unified feed ingestion
    /// ruling 4's). The nest-served sibling of [`Image`](Self::Image), a new variant per the
    /// D7a recipe so every exhaustive walker breaks at compile time.
    ///
    /// **Not [`RemoteImage`](Self::RemoteImage):** that url is a third-party origin the device
    /// dials itself, only after the D3 reveal; this path is fetched from the reader's own nest
    /// with the session bearer, exactly like `/api/v1/blob/<hash>`. **Not `Image`:** a path is
    /// not a content hash. It paints in the `post-image` slot immediately (the user-ruled D6c
    /// posture, 2026-09-30): no `revealed` flag, never counted by
    /// [`has_blocked_remote_images`](RenderDocument::has_blocked_remote_images).
    ProxiedImage { path: String, alt: String },
    /// A bridged post's own **video** attachment, served by the reader's own nest at the same
    /// nest-relative, already-proxied `path` form as [`ProxiedImage`](Self::ProxiedImage)
    /// (render-model.md § D6c → *Proxied video*). It completes the 2×2: [`Image`](Self::Image)
    /// / [`Video`](Self::Video) by content hash, `ProxiedImage` / `ProxiedVideo` by path. A
    /// sibling variant per the D7a recipe, never a flag on `ProxiedImage` (an app must never
    /// paint a video into an image element) and never a `Video` with an empty hash (a path is
    /// not a content address).
    ///
    /// Same posture as `ProxiedImage`: paints immediately in the `video-thumbnail` slot, no
    /// `revealed` flag, never counted by
    /// [`has_blocked_remote_images`](RenderDocument::has_blocked_remote_images). No app
    /// byte-loads it for a thumbnail — no writer supplies a poster frame, and the proxied twin
    /// of a first-frame grab would be a full bearer fetch per card.
    ProxiedVideo { path: String, alt: String },
    /// A remote `![alt](url)` image (`http(s)` only). `revealed` is **false** by default —
    /// the privacy posture for untrusted inbound content (html-mail.md § Rendering /
    /// § Security). In P2 the reveal flag is projected from a manager-owned in-memory set
    /// (render-model.md § D3); the parser never fetches the url.
    RemoteImage {
        url: String,
        alt: String,
        revealed: bool,
    },
    /// A first-class attachment node — the typed, in-body-order projection of
    /// `fauna_conversations::AttachmentSnapshot` (render-model.md § D2: embeds are
    /// blocks, not sibling snapshot fields). A **strict superset** of that snapshot's
    /// fields (priority #4) so no render data is lost. `blob_hash` is the content
    /// handle the client resolves to bytes through its existing
    /// `attachment_bytes(blob_hash)` loader — the *resolution* path is unchanged; only
    /// the *placement* moves into the document. Not produced by the markdown path; the
    /// conversations manager appends one per attachment after the text body (the
    /// `[body] → [attachments]` order all 7 apps already render). The variant name
    /// is `Attachment` (no stdlib collision, unlike `List` → `ListBlock`).
    Attachment {
        blob_hash: String,
        filename: String,
        mime_type: String,
        size_bytes: u64,
        is_image: bool,
        c2pa: bool,
    },
    /// A first-class link-preview embed — a new **async-resolved** node
    /// (render-model.md § D4). The body producer emits one in
    /// [`PreviewState::Resolving`] for a **standalone bare-URL paragraph** (a
    /// paragraph that is a single [`Inline::Link`] whose visible text equals its
    /// `href`); the inline link itself **stays** in the paragraph and this preview
    /// renders as an *additional* block below it, so a client that doesn't paint the
    /// card still shows the link. The page manager then resolves the metadata
    /// off-thread via the authenticated `fauna.linkpreview.resolve` WS-RPC kind and
    /// re-emits the block [`Resolved`](PreviewState::Resolved) / [`Failed`](PreviewState::Failed)
    /// — the **same lazy-resolve→rebuild-document pattern** feed already uses for
    /// `media_hash` and the quoted-post fallback (new capability, established
    /// mechanism). Not produced by any P2/P3 snapshot projection; only the markdown
    /// body producer emits it. The preview image
    /// ([`PreviewState::Resolved`]`::image_hash`) is **blocked-by-default** exactly
    /// like any [`RemoteImage`](Self::RemoteImage) (render-model.md § D3 posture,
    /// user-ratified 2026-06-27): its [`revealed`](PreviewState::Resolved::revealed)
    /// flag is projected from the post's reveal set by the manager (the D3 twin) and
    /// drives [`has_blocked_remote_images`](RenderDocument::has_blocked_remote_images),
    /// so the post's `load-remote-content-button` covers the og:image too; the
    /// per-app card paints the image only when `revealed`.
    LinkPreview { url: String, state: PreviewState },
    /// A first-class quoted-post node — the typed, in-body-order projection of
    /// `fauna_feed::QuotedPostView` (render-model.md § D2/D6: a feed quote-post is
    /// a **block**, not a sibling snapshot field). A **strict superset** of that
    /// view's fields (priority #4) so no render data is lost: `post_id` is the hex
    /// id the card links to, `author` the hex author of the quoted post, `body` the
    /// already-truncated (280-char cap) quoted body, and `verification` whether
    /// **this client** cryptographically verified the *quoted* post's signed
    /// envelope (carried from [`QuotedPostView::verification`](fauna_feed::QuotedPostView)
    /// so the quoted-embed card paints the "unverified source" badge **iff**
    /// [`Failed`](VerificationStatus::Failed) — `security.md` § Client display of
    /// unverified content). Not produced by the markdown path; the feed manager
    /// folds one in **after** the body when `resolve_quoted_post` resolves the
    /// quote (lazy-resolve→rebuild-document), so every app walks it instead of
    /// reading the sibling `quoted_post_id` field and re-projecting the quote itself.
    ///
    /// `legal_takedown_ref` is `Some(reference)` when the quoted post has been
    /// **taken down under a legal obligation** (`moderation.md` § Categories &
    /// enforcement item 1): the nest withholds its body, so `body`/`author` are
    /// empty and the client renders the shared tombstone
    /// (`fauna_core::obligation::legal_takedown_tombstone(reference)` — "Removed
    /// under legal obligation ({reference})") in place of the quoted content,
    /// never a blank/broken embed. Additive `Option` (`None` for every normal
    /// quote), carried from [`QuotedPostView::legal_takedown_ref`](fauna_feed::QuotedPostView)
    /// through [`build_post_document`](fauna_feed::build_post_document).
    QuotedPost {
        post_id: String,
        author: String,
        body: String,
        verification: VerificationStatus,
        /// Whether an external app authored the *quoted* post as its account —
        /// the D10 audit answer (`atproto-pds-full.md` § D10 → *Audit*), carried
        /// from [`QuotedPostView::authoring_origin`](fauna_feed::QuotedPostView)
        /// so the quoted-embed card paints the `delegated-origin-badge` **iff**
        /// [`Delegated`](AuthoringOriginStatus::Delegated). The
        /// `verification` twin, one field over.
        authoring_origin: AuthoringOriginStatus,
        legal_takedown_ref: Option<String>,
        /// `true` when the quoted post is **no longer there** — its author deleted
        /// it, so the nest answers `fauna.posts.not_found` (`ui/feed.md` § Post
        /// deletion: a reference to a deleted post dangles by design and renders
        /// the not-found state). `author`/`body` are empty; the client paints
        /// `feed.post.post_not_found` in place of the quoted content, never a
        /// blank embed. Additive (`false` for every live quote, and for a block
        /// serialized before the field existed), carried from
        /// [`QuotedPostView::not_found`](fauna_feed::QuotedPostView). Last in the
        /// variant so the positional bindings of the older fields keep their
        /// order.
        #[serde(default)]
        not_found: bool,
    },
    /// A first-class in-bubble reply-quote node — the conversations analogue of
    /// [`QuotedPost`](Self::QuotedPost) (render-model.md § D2: a reply-quote is a
    /// **block**, not a sibling `reply_to` field every bubble re-projects). The
    /// conversations manager folds one in at read time
    /// (`ConversationsManager::thread_detail`, beside the D3 reveal projection)
    /// when a message replies to a parent loaded in the **same thread**,
    /// **prepended** as the first block so the in-order client walkers paint it
    /// above the body; hidden (no block) when the parent isn't loaded. Carries the
    /// parent's `author_display` and a plaintext `snippet` of the parent body
    /// (clients clamp the snippet to ≤ 2 lines — value lives in `fauna_conversations`).
    /// Not produced by the markdown path.
    QuotedMessage {
        author_display: String,
        snippet: String,
    },
}

/// One item of a [`RenderBlock::TaskList`] — a GFM task-list entry (render-model.md § D7a).
/// `checked` is the `[x]`/`[ ]` state; `blocks` is the item's sub-document — one `Paragraph`
/// from markdown today, but block-level (like [`RenderBlock::BlockQuote`]'s `blocks`) so a
/// nested list/quote inside an item composes. Sibling to a `ListBlock` item (which is a
/// [`RenderDocument`]); both carry block content, so the client walkers paint them the same way
/// (`item.blocks`) save the per-item checkbox marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TaskItem {
    pub checked: bool,
    pub blocks: Vec<RenderBlock>,
}

/// One inline run, as a tree (`Bold`/`Italic`/`Link` carry nested inlines). This is a
/// strict generalisation of the flat [`MdSpan`] (which carries `bold`/`italic`/`code` as
/// bools on a single run): the markdown producer maps a flat span to at most one level of
/// nesting, but the tree shape lets a future producer (e.g. full HTML) nest emphasis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum Inline {
    /// Plain text.
    Text { text: String },
    /// Bold (`**`/`__`) wrapping nested inlines.
    Bold { inlines: Vec<Inline> },
    /// Italic (`*`/`_`) wrapping nested inlines.
    Italic { inlines: Vec<Inline> },
    /// Inline `` `code` `` — raw, never re-parsed.
    Code { text: String },
    /// A `[label](href)` link (`href` is `http(s)` only, mirroring `parse_link`).
    Link { href: String, inlines: Vec<Inline> },
}

/// Resolution state of a link-preview embed. Defined now as the leaf the model names;
/// the `LinkPreview` block that carries it is a P4 feature (render-model.md § D4), where
/// the manager resolves metadata off-thread and re-emits with `Resolved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PreviewState {
    /// Metadata fetch in flight.
    Resolving,
    /// Resolved metadata. `image_hash` is the (optional) fetched preview image. Even though
    /// the og:image is a **content-addressed blob served by this client's OWN nest** (the
    /// nest fetched it server-side — `fauna_protocol::linkpreview`), so painting it never
    /// phones home to a third party, it carries the **same blocked-by-default reveal posture
    /// as any [`RenderBlock::RemoteImage`]** (render-model.md § D4 — user-ratified
    /// 2026-06-27: the og:image hides behind the post's existing remote-content reveal). The
    /// card's title / description / domain always show; only the image is gated.
    Resolved {
        title: String,
        description: String,
        image_hash: Option<String>,
        /// Projected from the manager's per-post reveal set (the D3 twin) — **not** on the
        /// wire, exactly like [`RenderBlock::RemoteImage`]'s `revealed`. `false` at
        /// resolution; the manager flips it `true` once the user reveals the post's remote
        /// content, and the per-app card paints the og:image only when `true`.
        revealed: bool,
    },
    /// The fetch failed (a generic, non-retried failure for render purposes).
    Failed,
}

impl PreviewState {
    /// The state's stable lowercase name — `"resolving"`, `"resolved"` or `"failed"` —
    /// the one spelling every app's e2e state dump publishes beside a preview's url
    /// ([`RenderDocument::link_previews`]).
    pub fn name(&self) -> &'static str {
        match self {
            PreviewState::Resolving => "resolving",
            PreviewState::Resolved { .. } => "resolved",
            PreviewState::Failed => "failed",
        }
    }
}

/// Shared cache-check/insert/notify orchestration behind `resolve_link_preview`
/// (render-model.md § D4) — identical in `fauna_feed::FeedManager` and
/// `fauna_conversations::ConversationsManager` before this lift, differing only in how
/// each reaches the nest (a concrete `LinkPreviewClient<R: RpcRequester>` for feed, an
/// injectable `LinkPreviewRpc` trait object — possibly unwired in receive-only/SMTP-only
/// mode — for conversations, by design: `fauna-conversations` carries no `fauna-protocol`
/// dependency). `fetch` is awaited only on a cache miss and resolves to `None` to skip
/// entirely (no state change, no notify — the "no RPC wired" case) or `Some(state)` to
/// cache it; a transport error or explicit `Failed` reply both collapse to
/// [`PreviewState::Failed`] inside the caller's `fetch`, never here. Returns whether the
/// caller should notify. Idempotent by construction: a repeat call for an
/// already-resolved URL never re-fetches or re-notifies.
pub async fn resolve_link_preview_cached<F>(
    resolved_previews: &std::sync::RwLock<std::collections::HashMap<String, PreviewState>>,
    url: String,
    fetch: F,
) -> bool
where
    F: std::future::Future<Output = Option<PreviewState>>,
{
    if resolved_previews.read().unwrap().contains_key(&url) {
        return false;
    }
    let Some(state) = fetch.await else {
        return false;
    };
    resolved_previews.write().unwrap().insert(url, state);
    true
}

/// Whether a rendered post/quote's signed envelope was cryptographically
/// verified by *this client* (`security.md` § Client display of unverified
/// content; review findings F-CL2/F-CL3). Three honest states, **not** a bool —
/// a bool conflates "we never checked" with "we checked and it's authentic",
/// which is exactly the dangerous conflation the unverified-source indicator
/// exists to surface (an unverified projection must never *look* verified).
///
/// The indicator (a muted "unverified source" badge) renders **iff** [`Failed`]
/// — the DKIM-fail analogue. [`Unchecked`] is the normal feed-list card and
/// carries **no** badge: a `FeedPostItem` is the home nest's trusted index
/// projection (it has no envelope to verify — see
/// `fauna_feed::PostSummary`), and the home-nest trust model
/// (`security.md` § Transport trust) accepts it. Verification can only run where
/// the client decodes a **raw signed envelope** (`fauna.posts.get` + `decode_post`:
/// post-detail, the quoted-post fallback, media-resolve), so most list cards
/// stay [`Unchecked`] until the post is opened/decoded. A [`Failed`] post still
/// renders in full — a transient key-rotation-lag false-negative must not make a
/// legitimate post silently vanish (`security.md` § Client display).
///
/// [`Unchecked`]: VerificationStatus::Unchecked
/// [`Failed`]: VerificationStatus::Failed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum VerificationStatus {
    /// Not independently verified by this client — the default for a nest-index
    /// projection (`FeedPostItem` carries no envelope). **No badge.** This is the
    /// trusted-home-nest case, not a failure.
    #[default]
    Unchecked,
    /// The client decoded the raw signed envelope and the BLAKE3-CID + Ed25519
    /// signature check **passed** (`decode_post` → `valid == true`). **No badge.**
    Verified,
    /// The client decoded the raw signed envelope and verification **failed**
    /// (`decode_post` → `valid == false`) — content whose source could not be
    /// authenticated. **Renders the "unverified source" badge** while still
    /// showing the content.
    Failed,
}

impl VerificationStatus {
    /// Map the `bool` validity flag from
    /// [`fauna_client_core::post::decode_post`] (or `fauna_ffi`/WASM
    /// `decode_post*`'s `valid`) to a checked status — `true` → [`Verified`],
    /// `false` → [`Failed`]. Use this at every site that *actually decodes* a
    /// raw envelope; leave [`Unchecked`] where no decode happened.
    ///
    /// [`Verified`]: VerificationStatus::Verified
    /// [`Failed`]: VerificationStatus::Failed
    /// [`Unchecked`]: VerificationStatus::Unchecked
    pub fn from_valid(valid: bool) -> Self {
        if valid { Self::Verified } else { Self::Failed }
    }
}

/// Whether a verified payload was authored by the account's **own identity key**
/// or by a **delegated authoring sub-key** — the render-layer face of
/// [`crate::encoding::AuthoringOrigin`], and the D10 audit surface
/// (`atproto-pds-full.md` § Problem 1 → D10 → *Audit*, ratified 2026-07-29).
///
/// The audit surface D10 ratified is *the delegated content itself, verified
/// client-side* — never a nest-read log. `signer_auth` rides **outside** the
/// signed bytes, so stripping it from a delegated value makes verification FAIL
/// rather than read as [`Direct`]; masquerading delegated content as direct
/// needs the identity key. Rendering this enum is therefore client-authoritative
/// audit in `docs/goal/ui/nests.md`'s "never a nest read" sense.
///
/// The marker (a `delegated-origin-badge`) renders **iff** [`Delegated`].
/// [`Unknown`] is both the nest-index projection default (no envelope to verify
/// — the [`VerificationStatus::Unchecked`] twin) *and* the verification-failed
/// case: neither says anything trustworthy about origin, and collapsing them is
/// deliberate, since a failed envelope's cert claim is exactly what must not be
/// believed.
///
/// **The delegated sub-key's `device_key` is deliberately NOT carried here.**
/// There is exactly one authoring sub-key per account, so it can never name
/// *which* external app wrote a post — no surface can render it, and exporting
/// it to the apps would be the dark-capability class the Audit bullet warns of.
///
/// [`Direct`]: AuthoringOriginStatus::Direct
/// [`Delegated`]: AuthoringOriginStatus::Delegated
/// [`Unknown`]: AuthoringOriginStatus::Unknown
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AuthoringOriginStatus {
    /// No trustworthy origin answer — a nest-index projection this client never
    /// decoded, or an envelope whose verification **failed**. **No badge.**
    #[default]
    Unknown,
    /// The envelope verified under the author's **own identity key**. **No badge.**
    Direct,
    /// The envelope verified under a **delegated authoring sub-key** carrying a
    /// valid identity-signed `DeviceAuthorization` — an external app authored
    /// this as the account. **Renders the `delegated-origin-badge`.**
    Delegated,
}

impl AuthoringOriginStatus {
    /// Project the shared verify answer onto the render layer: `None` (the
    /// verification-failed case, or a bridge-translated bare Post) → [`Unknown`], and the two
    /// [`crate::encoding::AuthoringOrigin`] variants onto their twins. The
    /// `device_key` is dropped here on purpose — see the type docs.
    ///
    /// [`Unknown`]: AuthoringOriginStatus::Unknown
    pub fn from_origin(origin: Option<&crate::encoding::AuthoringOrigin>) -> Self {
        match origin {
            None => Self::Unknown,
            Some(crate::encoding::AuthoringOrigin::Direct) => Self::Direct,
            Some(crate::encoding::AuthoringOrigin::Delegated { .. }) => Self::Delegated,
        }
    }
}

impl RenderDocument {
    /// Flatten the document to a single-line plaintext preview — the document-level twin
    /// of [`crate::markdown::markdown_to_plaintext`], walking the same content in the same
    /// order so the two agree by construction. Used as the P1 golden parity guard against
    /// html-mail text regression, and useful for snippets/notifications later.
    ///
    /// Block-level structure and runs of whitespace fold into one space-separated line
    /// (a preview is one line, not a rendered document). Link spans contribute their
    /// label (not the href); inline/code-block text is included raw; an image contributes
    /// its alt text (matching `markdown_to_plaintext`, where an image span's `text` is the
    /// alt).
    pub fn to_plaintext(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        collect_plaintext(&self.blocks, &mut parts);
        parts
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Set `revealed` on **every** [`RenderBlock::RemoteImage`] in the document,
    /// recursing into list items and block quotes (markdown promotes images out of
    /// paragraphs, but a quote/list item can still carry one). This is the projection
    /// step for render-model.md § D3: the manager holds the per-message/per-post reveal
    /// set off the document and applies it here when emitting the snapshot, so the
    /// document the client walks carries the authoritative flag and no client keeps a
    /// `revealedRemote`/`remoteLoaded` dictionary of its own. The no-persistence posture
    /// is unchanged (html-mail.md § Rendering) — the set lives only in the manager's
    /// memory; this only stamps the projection onto the emitted document.
    pub fn set_remote_images_revealed(&mut self, revealed: bool) {
        set_remote_images_revealed(&mut self.blocks, revealed);
    }

    /// Whether the document carries at least one **un-revealed** remote image — i.e. the
    /// `load-remote-content-button` should show. The manager projects `revealed` via
    /// [`Self::set_remote_images_revealed`], so a client gates the button on this single
    /// document-derived predicate (render-model.md § D3) instead of OR-ing a local flag.
    pub fn has_blocked_remote_images(&self) -> bool {
        has_blocked_remote_images(&self.blocks)
    }

    /// The content hash of the first trusted [`RenderBlock::Image`] — the feed's
    /// lazily-resolved media, folded into the document by `resolve_media`
    /// (render-model.md § D6).
    ///
    /// A client extracts the hash here and paints the bytes through its own blob
    /// loader, because the *shared* model deliberately carries no loader (the
    /// async-byte-load-stays-client idiom) — so the walker's `Image` arm is a no-op
    /// and the page paints `post-image` itself.
    pub fn first_image_hash(&self) -> Option<&str> {
        first_image_hash(&self.blocks)
    }

    /// The content hash of the first trusted [`RenderBlock::Video`] — the exact twin of
    /// [`first_image_hash`](Self::first_image_hash), and the accessor a page paints its
    /// `video-thumbnail` element from (render-model.md § Implementation status today).
    ///
    /// Separate from `first_image_hash` rather than one "first media" accessor, because the
    /// two drive *different ui.yaml elements* (`post-image` vs `video-thumbnail`) and an app
    /// must never paint a video blob into an image element — that is the bug this whole
    /// variant exists to make unrepresentable.
    pub fn first_video_hash(&self) -> Option<&str> {
        first_video_hash(&self.blocks)
    }

    /// Every trusted media block the feed folded in, in body order — the
    /// [`Image`](RenderBlock::Image), [`Video`](RenderBlock::Video),
    /// [`ProxiedImage`](RenderBlock::ProxiedImage) and
    /// [`ProxiedVideo`](RenderBlock::ProxiedVideo) blocks, cloned.
    ///
    /// This is the document's answer to *"what media does this post carry"*, and it is the
    /// **authority** for it: the manager re-folds a post's document whenever another embed
    /// resolves (a quote, a gated body), and it recovers the already-folded media from here
    /// rather than from a sibling snapshot field. That is what keeps multi-item media from
    /// collapsing back to one item on the next rebuild without adding a second, drift-prone
    /// copy of the list to `PostSummary` — `media_hash` there stays exactly what it has always
    /// been, the fire-once resolve guard (and the e2e state field several apps publish).
    ///
    /// Top-level only: the feed appends media after the body, never inside a list item or a
    /// quote (unlike [`RemoteImage`](RenderBlock::RemoteImage), which markdown can nest).
    pub fn media_blocks(&self) -> Vec<RenderBlock> {
        self.blocks
            .iter()
            .filter(|b| {
                matches!(
                    b,
                    RenderBlock::Image { .. }
                        | RenderBlock::Video { .. }
                        | RenderBlock::ProxiedImage { .. }
                        | RenderBlock::ProxiedVideo { .. }
                )
            })
            .cloned()
            .collect()
    }

    /// Every [`RenderBlock::ProxiedImage`] in the body, in body order (render-model.md
    /// § D6c) — a bridged post's own pictures, each a nest-relative path the client fetches
    /// from its own nest with the session bearer and paints in the `post-image` slot.
    ///
    /// The nest-served counterpart of [`Self::remote_images`], but with no reveal state: a
    /// proxied image paints immediately (the user-ruled D6c posture). Recurses like its
    /// siblings.
    pub fn proxied_images(&self) -> Vec<ProxiedImageRef<'_>> {
        let mut out = Vec::new();
        proxied_images(&self.blocks, &mut out);
        out
    }

    /// Every [`RenderBlock::ProxiedVideo`] in the body, in body order (render-model.md § D6c →
    /// *Proxied video*) — a bridged post's own videos, each a nest-relative path an app paints
    /// in its `video-thumbnail` slot. The exact shape of [`Self::proxied_images`]; recurses
    /// like its siblings.
    pub fn proxied_videos(&self) -> Vec<ProxiedVideoRef<'_>> {
        let mut out = Vec::new();
        proxied_videos(&self.blocks, &mut out);
        out
    }

    /// The nest-relative path a post's `post-image` slot paints when it is a bridged
    /// picture: the first [`RenderBlock::ProxiedImage`] of a post with no blob image
    /// (render-model.md § D6c) — the slot takes a blob image first, so this is `None`
    /// whenever [`Self::first_image_hash`] is `Some`.
    pub fn proxied_post_image(&self) -> Option<&str> {
        if self.first_image_hash().is_some() {
            return None;
        }
        self.proxied_images().into_iter().next().map(|img| img.path)
    }

    /// The nest-relative path a post's `video-thumbnail` slot paints when it is a bridged
    /// video: the first [`RenderBlock::ProxiedVideo`] of a post with no blob video
    /// (render-model.md § D6c → *Proxied video*) — [`Self::proxied_post_image`]'s
    /// precedence, applied to the video slot.
    pub fn proxied_post_video(&self) -> Option<&str> {
        if self.first_video_hash().is_some() {
            return None;
        }
        self.proxied_videos()
            .into_iter()
            .next()
            .map(|video| video.path)
    }

    /// The folded feed quote-post embed ([`RenderBlock::QuotedPost`]), if the manager
    /// has resolved one (render-model.md § D6).
    ///
    /// This is also the **fire-once guard** for `resolve_quoted_post`: a client fires the
    /// resolve only while `quoted_post_id.is_some()` and this is `None`, so the manager's
    /// idempotent re-emit settles instead of driving a render loop.
    pub fn quoted_post(&self) -> Option<QuotedPostEmbed<'_>> {
        quoted_post(&self.blocks)
    }

    /// Whether a folded [`RenderBlock::QuotedPost`] is present — the boolean face of
    /// [`Self::quoted_post`].
    pub fn has_quoted_post(&self) -> bool {
        self.quoted_post().is_some()
    }

    /// The urls of link previews still in [`PreviewState::Resolving`], in body order —
    /// a client fires `resolve_link_preview` for each (render-model.md § D4). Fire-once
    /// by construction: a resolved block no longer yields its url.
    pub fn resolving_link_preview_urls(&self) -> Vec<&str> {
        let mut out = Vec::new();
        resolving_link_preview_urls(&self.blocks, &mut out);
        out
    }

    /// The [`PreviewState::Resolved`] link previews, in body order — one `link-preview-card`
    /// per entry (render-model.md § D4). The og:image is reveal-gated: paint
    /// [`ResolvedLinkPreview::image_hash`] only when [`ResolvedLinkPreview::revealed`]
    /// (the D3 twin — `set_remote_images_revealed` flips it, and an unrevealed one counts
    /// toward [`Self::has_blocked_remote_images`]).
    pub fn resolved_link_previews(&self) -> Vec<ResolvedLinkPreview<'_>> {
        let mut out = Vec::new();
        resolved_link_previews(&self.blocks, &mut out);
        out
    }

    /// Every link preview in the body, in body order, with its state — whatever the
    /// state. An app's e2e state dump publishes these (`data.feed.posts[].link_previews`,
    /// each as `{url, state: PreviewState::name}`) because a card is absent while a
    /// preview is still resolving too: "no card" says a preview FAILED only once its
    /// state says so, and a test has to be able to wait for that (render-model.md § D4).
    pub fn link_previews(&self) -> Vec<(&str, &PreviewState)> {
        let mut out = Vec::new();
        link_previews(&self.blocks, &mut out);
        out
    }

    /// Every [`RenderBlock::RemoteImage`] in the body, in body order, each with its
    /// own manager-projected `revealed` flag (render-model.md § D3).
    ///
    /// The read a client paints its reveal-gated remote-image element from, and the
    /// itemised counterpart of [`Self::has_blocked_remote_images`] — which answers
    /// only *whether* to show the one `load-remote-content-button`. A blocked entry
    /// paints a placeholder and its `url` is **not** fetched; the url is loaded only
    /// once `revealed` is true, which is the whole of the "when does untrusted
    /// content phone home" gate this projection exists to keep in one place.
    ///
    /// Deliberately excludes [`RenderBlock::Image`] (trusted, addressed by content
    /// hash — that is [`Self::first_image_hash`]) and the link-preview og:image
    /// (an own-nest blob — that is [`Self::resolved_link_previews`]).
    pub fn remote_images(&self) -> Vec<RemoteImageRef<'_>> {
        let mut out = Vec::new();
        remote_images(&self.blocks, &mut out);
        out
    }
}

/// The folded feed quote-post embed, borrowed from a [`RenderBlock::QuotedPost`] —
/// what a client needs to paint the `quoted-post` card without re-matching the block
/// itself. A `legal_takedown_ref` of `Some(_)` means the nest withheld the quoted body
/// and the client paints the shared tombstone instead
/// ([`crate::obligation::legal_takedown_tombstone`]); `not_found` means the quoted post
/// is gone and the client paints `feed.post.post_not_found` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotedPostEmbed<'a> {
    pub post_id: &'a str,
    pub author: &'a str,
    pub body: &'a str,
    pub verification: VerificationStatus,
    pub authoring_origin: AuthoringOriginStatus,
    pub legal_takedown_ref: Option<&'a str>,
    pub not_found: bool,
}

/// A [`PreviewState::Resolved`] link preview, borrowed from a [`RenderBlock::LinkPreview`] —
/// what a client needs to paint the `link-preview-card` (title / description / domain via
/// [`crate::format::url_host`], and the reveal-gated og:image).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLinkPreview<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    /// The og:image blob hash, if the nest kept one. Reveal-gated: paint it only when
    /// `revealed` is true (render-model.md § D4 — the D3 twin).
    pub image_hash: Option<&'a str>,
    pub revealed: bool,
}

/// Owned mirror of [`QuotedPostEmbed`] — the borrowed type carries a lifetime tied to
/// the source [`RenderDocument`], which can't cross UniFFI/wasm, so the FFI/wasm face
/// of [`RenderDocument::quoted_post`] (`fauna-ffi`/`fauna-wasm`) returns this instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QuotedPostEmbedOwned {
    pub post_id: String,
    pub author: String,
    pub body: String,
    pub verification: VerificationStatus,
    pub authoring_origin: AuthoringOriginStatus,
    pub legal_takedown_ref: Option<String>,
    /// See [`RenderBlock::QuotedPost`]'s `not_found`. Defaulted on every face, so
    /// a binding or a payload from before the field keeps reading `false`.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub not_found: bool,
}

impl From<QuotedPostEmbed<'_>> for QuotedPostEmbedOwned {
    fn from(e: QuotedPostEmbed<'_>) -> Self {
        QuotedPostEmbedOwned {
            post_id: e.post_id.to_string(),
            author: e.author.to_string(),
            body: e.body.to_string(),
            verification: e.verification,
            authoring_origin: e.authoring_origin,
            legal_takedown_ref: e.legal_takedown_ref.map(str::to_string),
            not_found: e.not_found,
        }
    }
}

/// A [`RenderBlock::RemoteImage`], borrowed from the document — what a client needs to
/// paint its reveal-gated remote-image element (render-model.md § D3).
///
/// `revealed` is the manager's projection, never a flag the client keeps; `url` is
/// fetched **only** when it is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteImageRef<'a> {
    pub url: &'a str,
    pub alt: &'a str,
    pub revealed: bool,
}

/// Owned mirror of [`RemoteImageRef`] — same cross-boundary reason as
/// [`QuotedPostEmbedOwned`], for the FFI/wasm face of [`RenderDocument::remote_images`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RemoteImageRefOwned {
    pub url: String,
    pub alt: String,
    pub revealed: bool,
}

impl From<RemoteImageRef<'_>> for RemoteImageRefOwned {
    fn from(r: RemoteImageRef<'_>) -> Self {
        RemoteImageRefOwned {
            url: r.url.to_string(),
            alt: r.alt.to_string(),
            revealed: r.revealed,
        }
    }
}

/// A [`RenderBlock::ProxiedImage`], borrowed from the document — the nest-relative `path` a
/// client fetches from its own nest (with the session bearer) and the `alt` it labels the
/// `post-image` slot with (render-model.md § D6c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxiedImageRef<'a> {
    pub path: &'a str,
    pub alt: &'a str,
}

/// Owned mirror of [`ProxiedImageRef`] — same cross-boundary reason as
/// [`QuotedPostEmbedOwned`], for the FFI/wasm face of [`RenderDocument::proxied_images`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProxiedImageRefOwned {
    pub path: String,
    pub alt: String,
}

impl From<ProxiedImageRef<'_>> for ProxiedImageRefOwned {
    fn from(r: ProxiedImageRef<'_>) -> Self {
        ProxiedImageRefOwned {
            path: r.path.to_string(),
            alt: r.alt.to_string(),
        }
    }
}

/// A [`RenderBlock::ProxiedVideo`], borrowed from the document — the nest-relative `path` an
/// app paints in its `video-thumbnail` slot (render-model.md § D6c → *Proxied video*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxiedVideoRef<'a> {
    pub path: &'a str,
    pub alt: &'a str,
}

/// Owned mirror of [`ProxiedVideoRef`] — same cross-boundary reason as
/// [`QuotedPostEmbedOwned`], for the FFI/wasm face of [`RenderDocument::proxied_videos`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProxiedVideoRefOwned {
    pub path: String,
    pub alt: String,
}

impl From<ProxiedVideoRef<'_>> for ProxiedVideoRefOwned {
    fn from(r: ProxiedVideoRef<'_>) -> Self {
        ProxiedVideoRefOwned {
            path: r.path.to_string(),
            alt: r.alt.to_string(),
        }
    }
}

/// Owned mirror of [`ResolvedLinkPreview`] — same cross-boundary reason as
/// [`QuotedPostEmbedOwned`], for the FFI/wasm face of
/// [`RenderDocument::resolved_link_previews`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ResolvedLinkPreviewOwned {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_hash: Option<String>,
    pub revealed: bool,
}

impl From<ResolvedLinkPreview<'_>> for ResolvedLinkPreviewOwned {
    fn from(p: ResolvedLinkPreview<'_>) -> Self {
        ResolvedLinkPreviewOwned {
            url: p.url.to_string(),
            title: p.title.to_string(),
            description: p.description.to_string(),
            image_hash: p.image_hash.map(str::to_string),
            revealed: p.revealed,
        }
    }
}

/// Owned `(url, state name)` pair from [`RenderDocument::link_previews`], for the
/// FFI/wasm face every non-Rust app's e2e state dump publishes as
/// `data.feed.posts[].link_previews` (`{url, state}`, `state` being
/// [`PreviewState::name`]). A plain `{url, state}` record so each app serializes it
/// as-is — the same shape the Rust apps build from the borrowed pairs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LinkPreviewStateOwned {
    pub url: String,
    pub state: String,
}

impl From<(&str, &PreviewState)> for LinkPreviewStateOwned {
    fn from((url, state): (&str, &PreviewState)) -> Self {
        LinkPreviewStateOwned {
            url: url.to_string(),
            state: state.name().to_string(),
        }
    }
}

/// Recursively find the first trusted [`RenderBlock::Image`]'s hash.
///
/// Recurses into list items / task items / block quotes for the same reason
/// [`has_blocked_remote_images`] does: a hand-rolled top-level-only twin silently misses
/// a nested arm, and the per-app twins this replaced were exactly that shape
/// (render-model.md § Implementation status — the single-sourcing rationale). The
/// markdown producer never emits `Image` (markdown images are *remote*), so today only
/// the manager's top-level media fold produces one — the recursion costs nothing and
/// forecloses nothing.
fn first_image_hash(blocks: &[RenderBlock]) -> Option<&str> {
    blocks.iter().find_map(|b| match b {
        RenderBlock::Image { hash, .. } => Some(hash.as_str()),
        RenderBlock::ListBlock { items, .. } => {
            items.iter().find_map(|item| first_image_hash(&item.blocks))
        }
        RenderBlock::TaskList { items } => {
            items.iter().find_map(|item| first_image_hash(&item.blocks))
        }
        RenderBlock::BlockQuote { blocks } => first_image_hash(blocks),
        // A `Video` is media but not an *image* hash: this feeds `post-image`, and painting a
        // video blob into it would show a broken image on every app. `media_blocks()` is the
        // accessor that sees both.
        RenderBlock::Video { .. }
        | RenderBlock::ProxiedImage { .. }
        | RenderBlock::ProxiedVideo { .. }
        | RenderBlock::Paragraph { .. }
        | RenderBlock::Heading { .. }
        | RenderBlock::CodeBlock { .. }
        | RenderBlock::RemoteImage { .. }
        | RenderBlock::Attachment { .. }
        | RenderBlock::LinkPreview { .. }
        | RenderBlock::QuotedPost { .. }
        | RenderBlock::QuotedMessage { .. } => None,
    })
}

/// Recursively find the first trusted [`RenderBlock::Video`]'s hash — the exact twin of
/// [`first_image_hash`] (same recursion, same rationale: a top-level-only twin silently
/// misses a nested arm).
fn first_video_hash(blocks: &[RenderBlock]) -> Option<&str> {
    blocks.iter().find_map(|b| match b {
        RenderBlock::Video { hash, .. } => Some(hash.as_str()),
        RenderBlock::ListBlock { items, .. } => {
            items.iter().find_map(|item| first_video_hash(&item.blocks))
        }
        RenderBlock::TaskList { items } => {
            items.iter().find_map(|item| first_video_hash(&item.blocks))
        }
        RenderBlock::BlockQuote { blocks } => first_video_hash(blocks),
        // An `Image` is media but not a *video* hash — the exact twin exclusion
        // `first_image_hash` makes for `Video`.
        RenderBlock::Image { .. }
        | RenderBlock::ProxiedImage { .. }
        | RenderBlock::ProxiedVideo { .. }
        | RenderBlock::Paragraph { .. }
        | RenderBlock::Heading { .. }
        | RenderBlock::CodeBlock { .. }
        | RenderBlock::RemoteImage { .. }
        | RenderBlock::Attachment { .. }
        | RenderBlock::LinkPreview { .. }
        | RenderBlock::QuotedPost { .. }
        | RenderBlock::QuotedMessage { .. } => None,
    })
}

/// Recursively find the folded [`RenderBlock::QuotedPost`] (see [`first_image_hash`] on why
/// this recurses).
fn quoted_post(blocks: &[RenderBlock]) -> Option<QuotedPostEmbed<'_>> {
    blocks.iter().find_map(|b| match b {
        RenderBlock::QuotedPost {
            post_id,
            author,
            body,
            verification,
            authoring_origin,
            legal_takedown_ref,
            not_found,
        } => Some(QuotedPostEmbed {
            post_id,
            author,
            body,
            verification: *verification,
            authoring_origin: *authoring_origin,
            legal_takedown_ref: legal_takedown_ref.as_deref(),
            not_found: *not_found,
        }),
        RenderBlock::ListBlock { items, .. } => {
            items.iter().find_map(|item| quoted_post(&item.blocks))
        }
        RenderBlock::TaskList { items } => items.iter().find_map(|item| quoted_post(&item.blocks)),
        RenderBlock::BlockQuote { blocks } => quoted_post(blocks),
        RenderBlock::Paragraph { .. }
        | RenderBlock::Heading { .. }
        | RenderBlock::CodeBlock { .. }
        | RenderBlock::Image { .. }
        | RenderBlock::Video { .. }
        | RenderBlock::ProxiedImage { .. }
        | RenderBlock::ProxiedVideo { .. }
        | RenderBlock::RemoteImage { .. }
        | RenderBlock::Attachment { .. }
        | RenderBlock::LinkPreview { .. }
        | RenderBlock::QuotedMessage { .. } => None,
    })
}

/// Recursively collect the urls of `Resolving` link previews, in body order.
fn resolving_link_preview_urls<'a>(blocks: &'a [RenderBlock], out: &mut Vec<&'a str>) {
    for b in blocks {
        match b {
            RenderBlock::LinkPreview {
                url,
                state: PreviewState::Resolving,
            } => out.push(url.as_str()),
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    resolving_link_preview_urls(&item.blocks, out);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    resolving_link_preview_urls(&item.blocks, out);
                }
            }
            RenderBlock::BlockQuote { blocks } => resolving_link_preview_urls(blocks, out),
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedImage { .. }
            | RenderBlock::ProxiedVideo { .. }
            | RenderBlock::RemoteImage { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::LinkPreview { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively collect every link-preview block with its state, in body order.
fn link_previews<'a>(blocks: &'a [RenderBlock], out: &mut Vec<(&'a str, &'a PreviewState)>) {
    for b in blocks {
        match b {
            RenderBlock::LinkPreview { url, state } => out.push((url.as_str(), state)),
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    link_previews(&item.blocks, out);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    link_previews(&item.blocks, out);
                }
            }
            RenderBlock::BlockQuote { blocks } => link_previews(blocks, out),
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedImage { .. }
            | RenderBlock::ProxiedVideo { .. }
            | RenderBlock::RemoteImage { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively collect the remote-image blocks, in body order.
fn remote_images<'a>(blocks: &'a [RenderBlock], out: &mut Vec<RemoteImageRef<'a>>) {
    for b in blocks {
        match b {
            RenderBlock::RemoteImage { url, alt, revealed } => out.push(RemoteImageRef {
                url,
                alt,
                revealed: *revealed,
            }),
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    remote_images(&item.blocks, out);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    remote_images(&item.blocks, out);
                }
            }
            RenderBlock::BlockQuote { blocks } => remote_images(blocks, out),
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedImage { .. }
            | RenderBlock::ProxiedVideo { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::LinkPreview { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively collect the proxied-image blocks, in body order (render-model.md § D6c).
fn proxied_images<'a>(blocks: &'a [RenderBlock], out: &mut Vec<ProxiedImageRef<'a>>) {
    for b in blocks {
        match b {
            RenderBlock::ProxiedImage { path, alt } => out.push(ProxiedImageRef { path, alt }),
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    proxied_images(&item.blocks, out);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    proxied_images(&item.blocks, out);
                }
            }
            RenderBlock::BlockQuote { blocks } => proxied_images(blocks, out),
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedVideo { .. }
            | RenderBlock::RemoteImage { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::LinkPreview { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively collect the proxied-video blocks, in body order (render-model.md § D6c →
/// *Proxied video*) — the twin of [`proxied_images`].
fn proxied_videos<'a>(blocks: &'a [RenderBlock], out: &mut Vec<ProxiedVideoRef<'a>>) {
    for b in blocks {
        match b {
            RenderBlock::ProxiedVideo { path, alt } => out.push(ProxiedVideoRef { path, alt }),
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    proxied_videos(&item.blocks, out);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    proxied_videos(&item.blocks, out);
                }
            }
            RenderBlock::BlockQuote { blocks } => proxied_videos(blocks, out),
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedImage { .. }
            | RenderBlock::RemoteImage { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::LinkPreview { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively collect the `Resolved` link previews, in body order.
fn resolved_link_previews<'a>(blocks: &'a [RenderBlock], out: &mut Vec<ResolvedLinkPreview<'a>>) {
    for b in blocks {
        match b {
            RenderBlock::LinkPreview {
                url,
                state:
                    PreviewState::Resolved {
                        title,
                        description,
                        image_hash,
                        revealed,
                    },
            } => out.push(ResolvedLinkPreview {
                url,
                title,
                description,
                image_hash: image_hash.as_deref(),
                revealed: *revealed,
            }),
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    resolved_link_previews(&item.blocks, out);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    resolved_link_previews(&item.blocks, out);
                }
            }
            RenderBlock::BlockQuote { blocks } => resolved_link_previews(blocks, out),
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedImage { .. }
            | RenderBlock::ProxiedVideo { .. }
            | RenderBlock::RemoteImage { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::LinkPreview { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively flip `revealed` on every [`RenderBlock::RemoteImage`] (mirrors
/// [`collect_plaintext`]'s recursion into list items + block quotes).
fn set_remote_images_revealed(blocks: &mut [RenderBlock], revealed: bool) {
    for b in blocks {
        match b {
            RenderBlock::RemoteImage { revealed: r, .. } => *r = revealed,
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    set_remote_images_revealed(&mut item.blocks, revealed);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    set_remote_images_revealed(&mut item.blocks, revealed);
                }
            }
            RenderBlock::BlockQuote { blocks } => set_remote_images_revealed(blocks, revealed),
            // A link preview's og:image (`PreviewState::Resolved.image_hash`) is content-addressed
            // (served by this client's own nest), but carries the SAME blocked-by-default reveal
            // posture as a `RemoteImage` (render-model.md § D4, user-ratified 2026-06-27): its
            // `revealed` flag is projected from the post's reveal set here — the D3 twin. A
            // `Resolving` / `Failed` (or image-less `Resolved`) preview has no image to gate and
            // falls to the catch-all below.
            RenderBlock::LinkPreview {
                state: PreviewState::Resolved { revealed: r, .. },
                ..
            } => *r = revealed,
            RenderBlock::Paragraph { .. }
            | RenderBlock::Heading { .. }
            | RenderBlock::CodeBlock { .. }
            | RenderBlock::Image { .. }
            | RenderBlock::Video { .. }
            | RenderBlock::ProxiedImage { .. }
            | RenderBlock::ProxiedVideo { .. }
            | RenderBlock::Attachment { .. }
            | RenderBlock::LinkPreview { .. }
            | RenderBlock::QuotedPost { .. }
            | RenderBlock::QuotedMessage { .. } => {}
        }
    }
}

/// Recursively test for any un-revealed [`RenderBlock::RemoteImage`].
fn has_blocked_remote_images(blocks: &[RenderBlock]) -> bool {
    blocks.iter().any(|b| match b {
        RenderBlock::RemoteImage { revealed, .. } => !revealed,
        RenderBlock::ListBlock { items, .. } => items
            .iter()
            .any(|item| has_blocked_remote_images(&item.blocks)),
        RenderBlock::TaskList { items } => items
            .iter()
            .any(|item| has_blocked_remote_images(&item.blocks)),
        RenderBlock::BlockQuote { blocks } => has_blocked_remote_images(blocks),
        // A Resolved link preview WITH an og:image that's not yet revealed is blocked remote
        // content too (render-model.md § D4, the D3 twin) — so a post whose ONLY remote content
        // is the preview image still surfaces the post's `load-remote-content-button`. A
        // `Resolving` / `Failed` / image-less `Resolved` preview has nothing to gate.
        RenderBlock::LinkPreview {
            state:
                PreviewState::Resolved {
                    image_hash: Some(_),
                    revealed,
                    ..
                },
            ..
        } => !revealed,
        RenderBlock::Paragraph { .. }
        | RenderBlock::Heading { .. }
        | RenderBlock::CodeBlock { .. }
        | RenderBlock::Image { .. }
        | RenderBlock::Video { .. }
        | RenderBlock::ProxiedImage { .. }
        | RenderBlock::ProxiedVideo { .. }
        | RenderBlock::Attachment { .. }
        | RenderBlock::LinkPreview { .. }
        | RenderBlock::QuotedPost { .. }
        | RenderBlock::QuotedMessage { .. } => false,
    })
}

/// Append each block's plaintext to `parts` (one part per text line / code block / image
/// alt), matching `markdown_to_plaintext`'s part granularity so the final
/// whitespace-collapse yields an identical string.
fn collect_plaintext(blocks: &[RenderBlock], parts: &mut Vec<String>) {
    for b in blocks {
        match b {
            RenderBlock::Paragraph { inlines } | RenderBlock::Heading { inlines, .. } => {
                let t = inlines_text(inlines);
                if !t.is_empty() {
                    parts.push(t);
                }
            }
            RenderBlock::ListBlock { items, .. } => {
                for item in items {
                    collect_plaintext(&item.blocks, parts);
                }
            }
            RenderBlock::TaskList { items } => {
                for item in items {
                    collect_plaintext(&item.blocks, parts);
                }
            }
            RenderBlock::CodeBlock { text, .. } => {
                if !text.is_empty() {
                    parts.push(text.clone());
                }
            }
            RenderBlock::BlockQuote { blocks } => collect_plaintext(blocks, parts),
            RenderBlock::Image { alt, .. }
            | RenderBlock::Video { alt, .. }
            | RenderBlock::ProxiedImage { alt, .. }
            | RenderBlock::ProxiedVideo { alt, .. }
            | RenderBlock::RemoteImage { alt, .. } => {
                if !alt.is_empty() {
                    parts.push(alt.clone());
                }
            }
            // An attachment contributes its filename to a one-line preview (the closest
            // body-text analogue, like an image's alt). markdown_to_document never emits
            // this variant, so the golden parity guard is unaffected.
            RenderBlock::Attachment { filename, .. } => {
                if !filename.is_empty() {
                    parts.push(filename.clone());
                }
            }
            // A quoted post contributes its (already-truncated) body to a one-line
            // preview, like an attachment's filename. markdown_to_document never
            // emits this variant, so the golden parity guard is unaffected.
            RenderBlock::QuotedPost { body, .. } => {
                if !body.is_empty() {
                    parts.push(body.clone());
                }
            }
            // A reply-quote's snippet is the *parent's* text, not this message's
            // content (unlike QuotedPost, whose body is intrinsic feed-card
            // content) — it is EXCLUDED from this message's plaintext so the
            // `dm-message-text` automation read stays the body alone; the quote
            // carries its own `dm-message-quote` id. markdown_to_document never
            // emits this variant, so the golden parity guard is unaffected.
            RenderBlock::QuotedMessage { .. } => {}
            // A link preview's url is ALREADY present once in the plaintext via the
            // kept inline link in the paragraph above it (the producer leaves that link
            // in place — render-model.md § D4); the preview block is an *additional*
            // node, so counting it here would double the url and break golden parity.
            // Its resolved title/description are fetched metadata, not body text. So it
            // contributes nothing — keeping `markdown_to_document(bare_url).to_plaintext()`
            // identical to the shipped `markdown_to_plaintext`.
            RenderBlock::LinkPreview { .. } => {}
        }
    }
}

/// Concatenate an inline run's text (link label, not href; recursing into emphasis).
fn inlines_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text { text } | Inline::Code { text } => out.push_str(text),
            Inline::Bold { inlines } | Inline::Italic { inlines } => {
                out.push_str(&inlines_text(inlines))
            }
            Inline::Link { inlines, .. } => out.push_str(&inlines_text(inlines)),
        }
    }
    out
}

/// Parse a Markdown body into a [`RenderDocument`]. Reuses
/// [`crate::markdown::parse_markdown`] (one parser, no fork) and maps its flat block/line/
/// span model into the typed tree: remote `![alt](url)` images are **promoted out of the
/// paragraph** into sibling [`RenderBlock::RemoteImage`] nodes in body order (D2), each
/// `revealed: false` (D3 posture). The shipped `MdBlock`/HTML path is untouched.
pub fn markdown_to_document(md: &str) -> RenderDocument {
    let blocks = markdown::parse_markdown(md)
        .into_iter()
        .flat_map(|b| block_from_md(&b))
        .collect();
    RenderDocument {
        blocks: inject_link_previews(blocks),
    }
}

/// Serialize a [`RenderDocument`] back to Markdown (render-model.md § D7b) — the inverse of
/// [`markdown_to_document`] for the markdown-producible block/inline kinds. This is the
/// serialization half of the lossless `blocks ⇄ markdown` round-trip (Proof 1 of the Fork-2
/// validation spec / § D7b go/no-go) and the export path the Notes editor reuses. Blocks the
/// markdown body producer never emits (`Image`/`Attachment`/`QuotedPost`/`QuotedMessage`/
/// `LinkPreview`) serialize to nothing: they are folded in by downstream snapshot producers,
/// not parsed from a body, and a bare-url `LinkPreview` is re-derived from the kept inline link
/// on reparse — so a `markdown_to_document(document_to_markdown(doc))` round-trip is idempotent.
pub fn document_to_markdown(doc: &RenderDocument) -> String {
    let mut out = String::new();
    for b in &doc.blocks {
        write_block(b, &mut out);
    }
    out.trim_end_matches('\n').to_string()
}

/// Write one top-level block + its trailing blank line.
fn write_block(b: &RenderBlock, out: &mut String) {
    match b {
        RenderBlock::Paragraph { inlines } => {
            out.push_str(&write_inlines(inlines));
            out.push_str("\n\n");
        }
        RenderBlock::Heading { level, inlines } => {
            for _ in 0..(*level).max(1) {
                out.push('#');
            }
            out.push(' ');
            out.push_str(&write_inlines(inlines));
            out.push_str("\n\n");
        }
        RenderBlock::CodeBlock { text, .. } => {
            out.push_str("```\n");
            out.push_str(text);
            out.push_str("\n```\n\n");
        }
        RenderBlock::BlockQuote { blocks } => {
            // The parser emits one quoted line per block; mirror that on the way out.
            for inner in blocks {
                out.push_str("> ");
                out.push_str(&block_inline(inner));
                out.push('\n');
            }
            out.push('\n');
        }
        RenderBlock::ListBlock { ordered, items } => {
            write_list(*ordered, items, 0, out);
            out.push('\n');
        }
        RenderBlock::TaskList { items } => {
            write_tasks(items, 0, out);
            out.push('\n');
        }
        RenderBlock::RemoteImage { url, alt, .. } => {
            out.push_str(&format!("![{alt}]({url})\n\n"));
        }
        // Not produced by the markdown body path (see [`document_to_markdown`]).
        RenderBlock::Image { .. }
        | RenderBlock::Video { .. }
        | RenderBlock::ProxiedImage { .. }
        | RenderBlock::ProxiedVideo { .. }
        | RenderBlock::Attachment { .. }
        | RenderBlock::QuotedPost { .. }
        | RenderBlock::QuotedMessage { .. }
        | RenderBlock::LinkPreview { .. } => {}
    }
}

/// Write an (un)ordered list's items at indent `depth` (two spaces per level).
fn write_list(ordered: bool, items: &[RenderDocument], depth: usize, out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        let marker = if ordered {
            format!("{}. ", i + 1)
        } else {
            "- ".to_string()
        };
        write_item(&item.blocks, &marker, depth, out);
    }
}

/// Write a task list's items at indent `depth` (`- [ ] ` / `- [x] `).
fn write_tasks(items: &[TaskItem], depth: usize, out: &mut String) {
    for item in items {
        let marker = if item.checked { "- [x] " } else { "- [ ] " };
        write_item(&item.blocks, marker, depth, out);
    }
}

/// Write one list/task item: `<indent><marker><first-block>` then any nested list blocks one
/// level deeper. A markdown item is one paragraph followed (optionally) by nested lists.
fn write_item(blocks: &[RenderBlock], marker: &str, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let mut wrote_marker = false;
    for b in blocks {
        match b {
            RenderBlock::ListBlock { ordered, items } => {
                write_list(*ordered, items, depth + 1, out)
            }
            RenderBlock::TaskList { items } => write_tasks(items, depth + 1, out),
            _ => {
                let prefix = if wrote_marker {
                    "  ".repeat(depth + 1)
                } else {
                    wrote_marker = true;
                    format!("{indent}{marker}")
                };
                out.push_str(&prefix);
                out.push_str(&block_inline(b));
                out.push('\n');
            }
        }
    }
    if !wrote_marker {
        // An item whose only content is a nested list still needs a marker line to reparse.
        out.push_str(&indent);
        out.push_str(marker.trim_end());
        out.push('\n');
    }
}

/// The single-line markdown form of an item/quote's inline content.
fn block_inline(b: &RenderBlock) -> String {
    match b {
        RenderBlock::Paragraph { inlines } | RenderBlock::Heading { inlines, .. } => {
            write_inlines(inlines)
        }
        RenderBlock::RemoteImage { url, alt, .. } => format!("![{alt}]({url})"),
        RenderBlock::CodeBlock { text, .. } => text.clone(),
        _ => String::new(),
    }
}

/// Serialize an inline run back to markdown (the inverse of [`inline_from_span`] / the inline
/// scanner). Emphasis always serializes to the `*`/`**` forms; an underscore source normalizes
/// to it, but the *document* is identical, so the round-trip holds.
fn write_inlines(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text { text } => s.push_str(text),
            Inline::Bold { inlines } => {
                s.push_str("**");
                s.push_str(&write_inlines(inlines));
                s.push_str("**");
            }
            Inline::Italic { inlines } => {
                s.push('*');
                s.push_str(&write_inlines(inlines));
                s.push('*');
            }
            Inline::Code { text } => {
                s.push('`');
                s.push_str(text);
                s.push('`');
            }
            Inline::Link { href, inlines } => {
                s.push('[');
                s.push_str(&write_inlines(inlines));
                s.push_str("](");
                s.push_str(href);
                s.push(')');
            }
        }
    }
    s
}

/// Append a [`RenderBlock::LinkPreview`] in [`PreviewState::Resolving`] after each
/// **standalone bare-url paragraph** — a top-level [`RenderBlock::Paragraph`] whose only
/// inline is an [`Inline::Link`] whose visible text equals its `href` (render-model.md
/// § D4). The paragraph (with its inline link) is left **in place**; the preview is an
/// *additional* sibling block right below it, so a client that doesn't paint the preview
/// card still shows the link. The page manager later resolves each `Resolving` block via
/// the authenticated `fauna.linkpreview.resolve` WS-RPC kind and re-emits it
/// `Resolved`/`Failed` (the lazy-resolve→rebuild-document pattern).
///
/// Only **top-level** paragraphs are considered — a url inside a list item or block quote
/// is not "standalone". Plaintext bodies never reach a link here (the plaintext producer
/// emits no [`Inline::Link`]), so in practice this fires only on markdown bodies.
fn inject_link_previews(blocks: Vec<RenderBlock>) -> Vec<RenderBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        let preview = bare_url_of(&b).map(|url| RenderBlock::LinkPreview {
            url: url.to_string(),
            state: PreviewState::Resolving,
        });
        out.push(b);
        if let Some(p) = preview {
            out.push(p);
        }
    }
    out
}

/// If `block` is a standalone bare-url paragraph (a single [`Inline::Link`] whose visible
/// text equals its `href`), return that url; otherwise `None`. The visible text is the
/// link label's concatenated inline text (so `[https://x](https://x)` qualifies but
/// `[docs](https://x)` does not).
fn bare_url_of(block: &RenderBlock) -> Option<&str> {
    let RenderBlock::Paragraph { inlines } = block else {
        return None;
    };
    let [
        Inline::Link {
            href,
            inlines: label,
        },
    ] = inlines.as_slice()
    else {
        return None;
    };
    (inlines_text(label) == *href).then_some(href.as_str())
}

/// Map one [`MdBlock`] to one-or-more [`RenderBlock`]s (more than one when a text block
/// contains an inline image, which is promoted to its own block).
fn block_from_md(b: &MdBlock) -> Vec<RenderBlock> {
    // Block-kind strings are `markdown`'s private constants; match the same literals the
    // shipped `render_html_with` matches (markdown.rs § KIND_*).
    match b.kind.as_str() {
        "code_block" => vec![RenderBlock::CodeBlock {
            lang: None,
            text: b.code.clone(),
        }],
        "heading" => line_blocks(first_line(b), |inlines| RenderBlock::Heading {
            level: b.level,
            inlines,
        }),
        "blockquote" => {
            // One quoted line → a block quote wrapping its paragraph(s)/image(s).
            let inner = line_blocks(first_line(b), |inlines| RenderBlock::Paragraph { inlines });
            vec![RenderBlock::BlockQuote { blocks: inner }]
        }
        "list" | "ordered_list" => fold_list(&b.lines, b.kind == "ordered_list"),
        // "paragraph" and any unknown kind: treat as a paragraph of its first line.
        _ => line_blocks(first_line(b), |inlines| RenderBlock::Paragraph { inlines }),
    }
}

/// Fold a list [`MdBlock`]'s flat, depth-annotated `lines` (render-model.md § D7b) into nested
/// [`RenderBlock::ListBlock`] / [`RenderBlock::TaskList`] blocks. Entries at the current
/// shallowest depth are this level's items; each item's more-deeply-indented followers are its
/// sub-document (recursively). A maximal run of same-depth entries of the same *kind* (plain
/// bullet vs. task) groups into one container, so a list mixing bullets and checkboxes emits
/// adjacent `ListBlock` + `TaskList` blocks (GFM renders both in one `<ul>`; we render adjacent
/// blocks, visually equivalent — § D7a). `ordered` is the block's marker kind (a task run is
/// never ordered). Same-kind nesting; a cross-kind nested run degrades to a sibling.
fn fold_list(lines: &[MdLine], ordered: bool) -> Vec<RenderBlock> {
    let mut out: Vec<RenderBlock> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let level = lines[i].depth;
        let is_task = lines[i].task.is_some();
        let mut list_items: Vec<RenderDocument> = Vec::new();
        let mut task_items: Vec<TaskItem> = Vec::new();
        // A maximal run of entries at this depth and kind → one container; each entry's
        // deeper-indented followers nest under it. `i` always advances past `lines[i]` here, so
        // the loop terminates even for a malformed (deeper-first) depth sequence.
        while i < lines.len() && lines[i].depth == level && lines[i].task.is_some() == is_task {
            let line = &lines[i];
            i += 1;
            let child_start = i;
            while i < lines.len() && lines[i].depth > level {
                i += 1;
            }
            let mut blocks = item_blocks(line);
            if child_start < i {
                blocks.extend(fold_list(&lines[child_start..i], ordered));
            }
            match line.task {
                Some(checked) => task_items.push(TaskItem { checked, blocks }),
                None => list_items.push(RenderDocument { blocks }),
            }
        }
        if is_task {
            out.push(RenderBlock::TaskList { items: task_items });
        } else {
            out.push(RenderBlock::ListBlock {
                ordered,
                items: list_items,
            });
        }
    }
    out
}

/// One list/task item's own content as blocks (one `Paragraph` from a markdown item, possibly
/// split by an inline image). For a task item the `MdLine` keeps the `[ ]`/`[x]` marker verbatim
/// (so the shipped html-mail path is byte-identical — render.rs floor); strip it here so the
/// typed `TaskItem` carries only the text (`checked` is the marker).
fn item_blocks(line: &MdLine) -> Vec<RenderBlock> {
    let stripped;
    let line = if line.task.is_some() {
        stripped = strip_task_marker(line);
        &stripped
    } else {
        line
    };
    line_blocks(Some(line), |inlines| RenderBlock::Paragraph { inlines })
}

/// Remove a leading GFM task marker (`[ ] ` / `[x] ` / `[X] `, then any extra whitespace) from
/// a task item's first inline span. The marker is the verbatim prefix the parser kept on the
/// `MdLine` content; only the first plain-text span can carry it (markdown's `[` opens a link
/// only with a following `](url)`, so a real task marker is always plain text).
fn strip_task_marker(line: &MdLine) -> MdLine {
    let mut out = line.clone();
    if let Some(first) = out.spans.first_mut()
        && !first.is_image()
        && !first.code
    {
        // `[`, one of ` `/`x`/`X`, then `]` — all ASCII, so the rest starts at byte 3.
        if matches!(first.text.as_bytes(), [b'[', b' ' | b'x' | b'X', b']', ..]) {
            first.text = first.text[3..].trim_start_matches([' ', '\t']).to_string();
        }
    }
    out
}

fn first_line(b: &MdBlock) -> Option<&MdLine> {
    b.lines.first()
}

/// Convert one line's spans into blocks, wrapping each maximal run of non-image inlines
/// with `wrap` and promoting each image span to its own [`RenderBlock::RemoteImage`] in
/// body order. A run that is empty or all-whitespace is dropped (no blank paragraphs
/// around a lone image); a line with no content at all yields nothing.
fn line_blocks(
    line: Option<&MdLine>,
    wrap: impl Fn(Vec<Inline>) -> RenderBlock,
) -> Vec<RenderBlock> {
    let Some(line) = line else {
        return Vec::new();
    };
    let mut out: Vec<RenderBlock> = Vec::new();
    let mut run: Vec<Inline> = Vec::new();
    for s in &line.spans {
        if s.is_image() {
            flush_run(&mut run, &wrap, &mut out);
            out.push(image_block(s));
        } else {
            run.push(inline_from_span(s));
        }
    }
    flush_run(&mut run, &wrap, &mut out);
    out
}

/// Flush an accumulated inline run as one wrapped block, unless it is empty or carries
/// only whitespace text.
fn flush_run(
    run: &mut Vec<Inline>,
    wrap: &impl Fn(Vec<Inline>) -> RenderBlock,
    out: &mut Vec<RenderBlock>,
) {
    if run.is_empty() {
        return;
    }
    let taken = std::mem::take(run);
    if inlines_text(&taken).trim().is_empty() {
        return;
    }
    out.push(wrap(taken));
}

/// Project a remote `![alt](url)` image span to a blocked-by-default [`RenderBlock`].
/// Markdown only ever yields remote http(s) images (`parse_link`), so this is always
/// `RemoteImage`, never `Image`.
fn image_block(s: &MdSpan) -> RenderBlock {
    RenderBlock::RemoteImage {
        url: s.image_url.clone(),
        alt: s.text.clone(),
        revealed: false,
    }
}

/// Map one flat [`MdSpan`] (never an image — callers route images to [`image_block`]) to
/// a nested [`Inline`]. `code`/`link` are mutually exclusive with emphasis (mirroring
/// `MdSpan`); a `bold && italic` span nests `Bold(Italic(Text))`.
fn inline_from_span(s: &MdSpan) -> Inline {
    if s.code {
        return Inline::Code {
            text: s.text.clone(),
        };
    }
    if !s.link.is_empty() {
        return Inline::Link {
            href: s.link.clone(),
            inlines: vec![Inline::Text {
                text: s.text.clone(),
            }],
        };
    }
    let text = Inline::Text {
        text: s.text.clone(),
    };
    match (s.bold, s.italic) {
        (true, true) => Inline::Bold {
            inlines: vec![Inline::Italic {
                inlines: vec![text],
            }],
        },
        (true, false) => Inline::Bold {
            inlines: vec![text],
        },
        (false, true) => Inline::Italic {
            inlines: vec![text],
        },
        (false, false) => text,
    }
}

/// Build a [`RenderDocument`] from a **plain-text** body — no markdown, no inline parsing,
/// no image classification. Blank-line-separated chunks become paragraphs, each a single
/// [`Inline::Text`]; an empty body yields an empty document. (The conversations manager
/// chooses this vs. [`markdown_to_document`] from the message's body format in P2.)
pub fn plaintext_to_document(text: &str) -> RenderDocument {
    let normalized = text.replace("\r\n", "\n");
    let mut blocks: Vec<RenderBlock> = Vec::new();
    for chunk in normalized.split("\n\n") {
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }
        blocks.push(RenderBlock::Paragraph {
            inlines: vec![Inline::Text {
                text: trimmed.to_string(),
            }],
        });
    }
    RenderDocument { blocks }
}

// ── Paint projection: native text-widget line runs ──────────────────────────

/// The most hard-broken lines a native shell hands ONE text widget — the budget
/// [`inline_line_runs`] / [`text_line_runs`] split a text block to
/// (render-model.md § Where logic lives).
///
/// A native text widget can lay out a string super-linearly in its hard line breaks.
/// Measured on apple (2026-09-11, `mail-message-size.md` § Implementation status
/// today): SwiftUI sizes a `Text` through `NSStringDrawing`, whose CoreText typesetter
/// re-shapes from each line break onward, so one `Text` of 4 000 short lines took 21 s
/// and the ~3 MiB, ~40 000-line plain-text mail extrapolated to ~36 min — a hung app.
/// The same bytes as ONE line lay out in 0.06 s: the cost is line COUNT per widget.
///
/// 16, because the apple shell paints a split block's runs in a lazy stack (only the
/// runs on screen are laid out) and a run's cost grows with the square of its line
/// count: at 16 that mail opened in 0.08 s and survived end/middle/end scroll jumps in
/// 0.2 s, against 1.4 s at 32 and 5.4 s at 128. A block of 16 lines or fewer — nearly
/// all real prose — stays one run, painted exactly as before.
pub const MAX_LINES_PER_TEXT_RUN: usize = 16;

/// Split a text block's inline run at its hard line breaks (`\n` inside
/// [`Inline::Text`] / [`Inline::Code`]) into consecutive runs of at most
/// [`MAX_LINES_PER_TEXT_RUN`] lines, for a native shell to paint as one text widget
/// per run.
///
/// A **paint projection**, like the embed extractors on [`RenderDocument`]: it reads
/// the model and never changes it (render-model.md § The boundary), so the document
/// keeps its one paragraph and a shell whose text layout is linear (web, tui) simply
/// never calls it.
///
/// Lossless: each cut consumes exactly the one `\n` it falls on (the shell's stacking
/// supplies that break), so joining the runs' text with `\n` gives back the input's.
/// Styling survives a cut — an [`Inline::Bold`] / [`Inline::Italic`] / [`Inline::Link`]
/// spanning it is re-opened on the far side, the link with the same `href`. A run
/// already within the budget comes back as exactly one run, equal to the input.
pub fn inline_line_runs(inlines: &[Inline]) -> Vec<Vec<Inline>> {
    if inline_breaks(inlines) < MAX_LINES_PER_TEXT_RUN {
        return vec![inlines.to_vec()];
    }
    LineRunSplitter::new(MAX_LINES_PER_TEXT_RUN).split_seq(inlines)
}

/// [`inline_line_runs`] for an unstyled string — a [`RenderBlock::CodeBlock`]'s text,
/// the other text block that carries hard line breaks.
pub fn text_line_runs(text: &str) -> Vec<String> {
    LineRunSplitter::new(MAX_LINES_PER_TEXT_RUN).split_text(text)
}

/// Hard line breaks in an inline run, at any nesting depth.
fn inline_breaks(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text { text } | Inline::Code { text } => text.matches('\n').count(),
            Inline::Bold { inlines } | Inline::Italic { inlines } => inline_breaks(inlines),
            Inline::Link { inlines, .. } => inline_breaks(inlines),
        })
        .sum()
}

/// The walk behind [`inline_line_runs`]: a depth-first pass carrying one counter — the
/// `\n`s kept in the run being built — across every leaf, so a run's budget spans
/// sibling and nested inlines alike.
struct LineRunSplitter {
    max_lines: usize,
    /// `\n`s kept in the current run (a run holding `k` of them paints `k + 1` lines).
    breaks_in_run: usize,
}

impl LineRunSplitter {
    fn new(max_lines: usize) -> Self {
        Self {
            max_lines: max_lines.max(1),
            breaks_in_run: 0,
        }
    }

    /// Split a sequence. Each inline yields one fragment per run it touches; the last
    /// fragment of one inline and the first of the next share a run, so fragments are
    /// stitched as they arrive.
    fn split_seq(&mut self, inlines: &[Inline]) -> Vec<Vec<Inline>> {
        let mut runs = vec![Vec::new()];
        for inline in inlines {
            for (i, fragment) in self.split_one(inline).into_iter().enumerate() {
                if i > 0 {
                    runs.push(Vec::new());
                }
                if let Some(fragment) = fragment {
                    runs.last_mut().expect("starts non-empty").push(fragment);
                }
            }
        }
        runs
    }

    /// One inline's fragments, one per run it touches — `None` for a fragment with
    /// nothing in it (an empty wrapper paints nothing, so it is not re-opened).
    fn split_one(&mut self, inline: &Inline) -> Vec<Option<Inline>> {
        fn wrap(
            runs: Vec<Vec<Inline>>,
            make: impl Fn(Vec<Inline>) -> Inline,
        ) -> Vec<Option<Inline>> {
            runs.into_iter()
                .map(|inlines| (!inlines.is_empty()).then(|| make(inlines)))
                .collect()
        }
        match inline {
            Inline::Text { text } => self
                .split_text(text)
                .into_iter()
                .map(|text| (!text.is_empty()).then_some(Inline::Text { text }))
                .collect(),
            Inline::Code { text } => self
                .split_text(text)
                .into_iter()
                .map(|text| (!text.is_empty()).then_some(Inline::Code { text }))
                .collect(),
            Inline::Bold { inlines } => {
                wrap(self.split_seq(inlines), |inlines| Inline::Bold { inlines })
            }
            Inline::Italic { inlines } => wrap(self.split_seq(inlines), |inlines| Inline::Italic {
                inlines,
            }),
            Inline::Link { href, inlines } => {
                wrap(self.split_seq(inlines), |inlines| Inline::Link {
                    href: href.clone(),
                    inlines,
                })
            }
        }
    }

    /// Split one leaf's text, cutting at the break that would open line
    /// `max_lines + 1` of the current run and dropping that one `\n`.
    fn split_text(&mut self, text: &str) -> Vec<String> {
        let mut pieces = vec![String::new()];
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                if self.breaks_in_run + 1 >= self.max_lines {
                    pieces.push(String::new());
                    self.breaks_in_run = 0;
                } else {
                    pieces.last_mut().expect("starts non-empty").push('\n');
                    self.breaks_in_run += 1;
                }
            }
            pieces.last_mut().expect("starts non-empty").push_str(line);
        }
        pieces
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{markdown_to_plaintext, parse_markdown};

    fn text(t: &str) -> Inline {
        Inline::Text { text: t.into() }
    }

    // ── block kinds map through ──────────────────────────────────────────────

    #[test]
    fn paragraph_with_emphasis_nests() {
        let doc = markdown_to_document("Hello **world** and *you* and `x`");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![
                    text("Hello "),
                    Inline::Bold {
                        inlines: vec![text("world")]
                    },
                    text(" and "),
                    Inline::Italic {
                        inlines: vec![text("you")]
                    },
                    text(" and "),
                    Inline::Code { text: "x".into() },
                ]
            }]
        );
    }

    #[test]
    fn bold_italic_span_nests_bold_outer() {
        let doc = markdown_to_document("***x***");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Bold {
                    inlines: vec![Inline::Italic {
                        inlines: vec![text("x")]
                    }]
                }]
            }]
        );
    }

    #[test]
    fn heading_carries_level() {
        let doc = markdown_to_document("## Hi **bold**");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Heading {
                level: 2,
                inlines: vec![
                    text("Hi "),
                    Inline::Bold {
                        inlines: vec![text("bold")]
                    }
                ]
            }]
        );
    }

    #[test]
    fn link_span_carries_href_and_label() {
        let doc = markdown_to_document("see [docs](https://example.com/a) now");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![
                    text("see "),
                    Inline::Link {
                        href: "https://example.com/a".into(),
                        inlines: vec![text("docs")]
                    },
                    text(" now"),
                ]
            }]
        );
    }

    #[test]
    fn unordered_and_ordered_lists() {
        let ul = markdown_to_document("- a\n- b");
        assert_eq!(
            ul.blocks,
            vec![RenderBlock::ListBlock {
                ordered: false,
                items: vec![
                    RenderDocument {
                        blocks: vec![RenderBlock::Paragraph {
                            inlines: vec![text("a")]
                        }]
                    },
                    RenderDocument {
                        blocks: vec![RenderBlock::Paragraph {
                            inlines: vec![text("b")]
                        }]
                    },
                ]
            }]
        );

        let ol = markdown_to_document("1. one\n2. two");
        match &ol.blocks[0] {
            RenderBlock::ListBlock { ordered, items } => {
                assert!(*ordered);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected ordered list, got {other:?}"),
        }
    }

    #[test]
    fn blockquote_wraps_paragraph() {
        let doc = markdown_to_document("> quoted");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::BlockQuote {
                blocks: vec![RenderBlock::Paragraph {
                    inlines: vec![text("quoted")]
                }]
            }]
        );
    }

    #[test]
    fn code_block_is_raw() {
        let doc = markdown_to_document("```\na<b\n```");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::CodeBlock {
                lang: None,
                text: "a<b".into()
            }]
        );
    }

    // ── remote-image classification & promotion ──────────────────────────────

    #[test]
    fn remote_image_is_promoted_to_a_blocked_block() {
        // The P1 flow-trace: the image becomes a SIBLING RemoteImage block after the
        // paragraph, revealed:false, never an inline.
        let doc = markdown_to_document("**hi** ![a](http://x)");
        assert_eq!(
            doc.blocks,
            vec![
                RenderBlock::Paragraph {
                    inlines: vec![
                        Inline::Bold {
                            inlines: vec![text("hi")]
                        },
                        text(" "),
                    ]
                },
                RenderBlock::RemoteImage {
                    url: "http://x".into(),
                    alt: "a".into(),
                    revealed: false,
                },
            ]
        );
    }

    #[test]
    fn lone_image_yields_no_blank_paragraph() {
        let doc = markdown_to_document("![cat](https://img.test/c.png)");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::RemoteImage {
                url: "https://img.test/c.png".into(),
                alt: "cat".into(),
                revealed: false,
            }]
        );
    }

    #[test]
    fn image_between_text_splits_into_three_blocks() {
        let doc = markdown_to_document("before ![m](http://i) after");
        assert_eq!(
            doc.blocks,
            vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("before ")]
                },
                RenderBlock::RemoteImage {
                    url: "http://i".into(),
                    alt: "m".into(),
                    revealed: false,
                },
                RenderBlock::Paragraph {
                    inlines: vec![text(" after")]
                },
            ]
        );
    }

    // ── plaintext producer ───────────────────────────────────────────────────

    #[test]
    fn plaintext_splits_on_blank_lines_without_inline_parsing() {
        let doc = plaintext_to_document("one **not bold**\n\ntwo");
        assert_eq!(
            doc.blocks,
            vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("one **not bold**")]
                },
                RenderBlock::Paragraph {
                    inlines: vec![text("two")]
                },
            ]
        );
        assert_eq!(plaintext_to_document("").blocks, vec![]);
    }

    // ── attachment embed block ───────────────────────────────────────────────

    #[test]
    fn attachment_block_contributes_filename_to_plaintext() {
        // An Attachment node is a metadata block (no inline content); its filename is
        // the one-line-preview contribution, after the body text in body order.
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("see this")],
                },
                RenderBlock::Attachment {
                    blob_hash: "aa01".into(),
                    filename: "pic.png".into(),
                    mime_type: "image/png".into(),
                    size_bytes: 10,
                    is_image: true,
                    c2pa: false,
                },
            ],
        };
        assert_eq!(doc.to_plaintext(), "see this pic.png");
    }

    // ── quoted-post embed block ──────────────────────────────────────────────

    #[test]
    fn quoted_post_block_contributes_body_to_plaintext() {
        // A QuotedPost node is a metadata block (no inline content); its
        // already-truncated body is the one-line-preview contribution, after the
        // body text in body order. markdown_to_document never emits it, so the
        // golden parity guard stays intact.
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("look at this")],
                },
                RenderBlock::QuotedPost {
                    post_id: "bb01".into(),
                    author: "2222".into(),
                    body: "the quoted body".into(),
                    verification: VerificationStatus::Unchecked,
                    authoring_origin: AuthoringOriginStatus::Unknown,
                    legal_takedown_ref: None,
                    not_found: false,
                },
            ],
        };
        assert_eq!(doc.to_plaintext(), "look at this the quoted body");
    }

    // ── quoted-message (reply-quote) embed block ─────────────────────────────

    #[test]
    fn quoted_message_block_excluded_from_plaintext() {
        // Unlike QuotedPost, a reply-quote's snippet is the PARENT's text, not
        // this message's content, so it does NOT contribute to this message's
        // plaintext — `dm-message-text` stays the body alone (the quote has its
        // own `dm-message-quote` id). The quote is prepended (above the body).
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::QuotedMessage {
                    author_display: "Alice".into(),
                    snippet: "the parent body".into(),
                },
                RenderBlock::Paragraph {
                    inlines: vec![text("my reply")],
                },
            ],
        };
        assert_eq!(doc.to_plaintext(), "my reply");
    }

    // ── D4: link-preview producer (bare-URL detection) ───────────────────────

    #[test]
    fn standalone_bare_url_paragraph_gets_a_resolving_preview() {
        // A paragraph that is exactly ONE link whose visible text == its href is a
        // "bare URL" (render-model.md § D4). The producer KEEPS the inline link in the
        // paragraph and appends a `LinkPreview { Resolving }` block right below it (so a
        // client that never paints the card still shows the link).
        let doc = markdown_to_document("[https://example.com/a](https://example.com/a)");
        assert_eq!(
            doc.blocks,
            vec![
                RenderBlock::Paragraph {
                    inlines: vec![Inline::Link {
                        href: "https://example.com/a".into(),
                        inlines: vec![text("https://example.com/a")],
                    }]
                },
                RenderBlock::LinkPreview {
                    url: "https://example.com/a".into(),
                    state: PreviewState::Resolving,
                },
            ]
        );
    }

    #[test]
    fn link_with_a_distinct_label_is_not_a_bare_url() {
        // `[docs](url)` — visible text "docs" != href, so it is an ordinary link and
        // gets NO preview (only a *bare* url, where text == href, previews).
        let doc = markdown_to_document("[docs](https://example.com/a)");
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Link {
                    href: "https://example.com/a".into(),
                    inlines: vec![text("docs")],
                }]
            }]
        );
    }

    #[test]
    fn bare_url_with_surrounding_text_is_not_standalone() {
        // The link must be ALONE in its paragraph — "see <url>" is not a standalone
        // bare-url paragraph, so no preview is appended.
        let doc = markdown_to_document("see [https://example.com](https://example.com)");
        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(doc.blocks[0], RenderBlock::Paragraph { .. }));
    }

    #[test]
    fn two_bare_url_paragraphs_each_get_their_own_preview() {
        // Each standalone bare-url paragraph is detected independently, the preview
        // appended directly after its paragraph (body order preserved).
        let doc = markdown_to_document(
            "[https://a.test/x](https://a.test/x)\n\n[https://b.test/y](https://b.test/y)",
        );
        assert_eq!(
            doc.blocks,
            vec![
                RenderBlock::Paragraph {
                    inlines: vec![Inline::Link {
                        href: "https://a.test/x".into(),
                        inlines: vec![text("https://a.test/x")],
                    }]
                },
                RenderBlock::LinkPreview {
                    url: "https://a.test/x".into(),
                    state: PreviewState::Resolving,
                },
                RenderBlock::Paragraph {
                    inlines: vec![Inline::Link {
                        href: "https://b.test/y".into(),
                        inlines: vec![text("https://b.test/y")],
                    }]
                },
                RenderBlock::LinkPreview {
                    url: "https://b.test/y".into(),
                    state: PreviewState::Resolving,
                },
            ]
        );
    }

    #[test]
    fn bare_url_inside_a_list_item_is_not_promoted() {
        // "standalone" means a top-level paragraph; a bare url inside a list item is
        // not standalone, so the producer does not inject a preview for it (only the
        // list block is emitted).
        let doc = markdown_to_document("- [https://x.test/a](https://x.test/a)");
        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(doc.blocks[0], RenderBlock::ListBlock { .. }));
    }

    #[test]
    fn bare_url_preview_preserves_plaintext_parity() {
        // The appended `LinkPreview` contributes nothing to the one-line plaintext (the
        // url is already present once via the kept inline link), so document plaintext
        // still equals the shipped `markdown_to_plaintext` for a bare-url body — while
        // the preview block IS present (not optimized away).
        let md = "[https://example.com/a](https://example.com/a)";
        assert_eq!(
            markdown_to_document(md).to_plaintext(),
            markdown_to_plaintext(md),
        );
        assert!(
            markdown_to_document(md)
                .blocks
                .iter()
                .any(|b| matches!(b, RenderBlock::LinkPreview { .. }))
        );
    }

    // ── golden parity: document plaintext == shipped markdown_to_plaintext ─────

    #[test]
    fn golden_plaintext_parity_with_shipped_path() {
        // The new model must not lose or reorder body text relative to the shipped
        // html-mail path. `to_plaintext` walks the same content `markdown_to_plaintext`
        // does, so they agree for EVERY input — including remote images (whose alt text
        // both include) — guarding against a mapping regression.
        let corpus = [
            "",
            "just a paragraph",
            "Hello **world** and *you* and `x`",
            "# Title\n\nBody para with a [link](https://example.com).\n\n- item one\n- item two",
            "1. first\n2. second",
            "> a quoted line",
            "```\nlet x = a < b && c;\n```",
            "text ![cat](https://img.test/c.png) more text",
            "a __bold__ and _ital_ and ___both___ word",
            "line one\nline two\n\nsecond para",
        ];
        for md in corpus {
            assert_eq!(
                markdown_to_document(md).to_plaintext(),
                markdown_to_plaintext(md),
                "plaintext parity diverged for input {md:?}"
            );
        }
    }

    #[test]
    fn block_count_tracks_parse_markdown_for_text_only_bodies() {
        // For image-free bodies, every MdBlock maps to exactly one RenderBlock (no
        // promotion splits), so the structures stay 1:1.
        let md = "# H\n\npara\n\n- a\n- b\n\n> q\n\n```\ncode\n```";
        assert_eq!(
            markdown_to_document(md).blocks.len(),
            parse_markdown(md).len()
        );
    }

    // ── D7: task lists + nested-list emission ─────────────────────────────────

    fn para(t: &str) -> RenderBlock {
        RenderBlock::Paragraph {
            inlines: vec![text(t)],
        }
    }

    #[test]
    fn task_list_folds_to_task_block_with_marker_stripped() {
        // `- [ ]`/`- [x]` → a `TaskList` whose items carry `checked` and the marker-stripped
        // content (the `[ ]` stays in the shipped MdBlock path; only the typed projection drops
        // it). render-model.md § D7a/b.
        let doc = markdown_to_document("- [ ] todo\n- [x] done");
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            RenderBlock::TaskList { items } => {
                assert_eq!(items.len(), 2);
                assert!(!items[0].checked);
                assert!(items[1].checked);
                assert_eq!(items[0].blocks, vec![para("todo")]);
                assert_eq!(items[1].blocks, vec![para("done")]);
            }
            other => panic!("expected TaskList, got {other:?}"),
        }
    }

    #[test]
    fn nested_bullets_build_a_sub_list_under_the_parent_item() {
        // `- a` with indented `  - b` → one top-level `ListBlock` whose first item carries its
        // own paragraph PLUS a nested `ListBlock`; the de-indented `- c` is a sibling item.
        let doc = markdown_to_document("- a\n  - b\n- c");
        match &doc.blocks[0] {
            RenderBlock::ListBlock {
                ordered: false,
                items,
            } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].blocks.len(), 2);
                assert_eq!(items[0].blocks[0], para("a"));
                assert!(matches!(
                    items[0].blocks[1],
                    RenderBlock::ListBlock { ordered: false, .. }
                ));
                assert_eq!(items[1].blocks, vec![para("c")]);
            }
            other => panic!("expected ListBlock, got {other:?}"),
        }
        assert_eq!(doc.blocks.len(), 1);
    }

    #[test]
    fn mixed_bullet_and_task_runs_split_into_adjacent_blocks() {
        // A list mixing plain bullets and checkboxes folds a run of bullets into a `ListBlock`
        // and a run of tasks into a sibling `TaskList` (render-model.md § D7a).
        let doc = markdown_to_document("- plain\n- [ ] task\n- [x] done\n- plain2");
        assert_eq!(doc.blocks.len(), 3);
        assert!(matches!(doc.blocks[0], RenderBlock::ListBlock { .. }));
        assert!(matches!(doc.blocks[1], RenderBlock::TaskList { .. }));
        assert!(matches!(doc.blocks[2], RenderBlock::ListBlock { .. }));
    }

    #[test]
    fn d7_round_trip_blocks_markdown_blocks() {
        // Proof 1 of the Fork-2 validation spec / render-model.md § D7b go/no-go: a document
        // projected from markdown survives a `blocks → markdown → blocks` round-trip unchanged,
        // over nested bullets, nested ordered lists, GFM task lists, mixed bullet/checkbox,
        // headings, code, quotes, paragraphs, and inline emphasis/code/links.
        let corpus = [
            "",
            "just a paragraph",
            "A **bold** *italic* `code` and a [link](https://x.io).",
            "# Title\n\nbody para",
            "- a\n  - b\n  - c\n- d",
            "1. one\n   1. inner\n   2. inner two\n2. two",
            "- [ ] todo\n- [x] done",
            "- plain\n- [ ] task\n- [x] done\n- plain2",
            "- top\n  - [ ] nested task\n  - [x] nested done",
            "```\nlet x = a < b && c;\n```",
            "> quoted line",
            "para one\n\npara two\n\n- l1\n- l2",
        ];
        for md in corpus {
            let doc1 = markdown_to_document(md);
            let md2 = document_to_markdown(&doc1);
            let doc2 = markdown_to_document(&md2);
            assert_eq!(
                doc1, doc2,
                "round-trip diverged for {md:?}\n--- serialized ---\n{md2}"
            );
        }
    }

    // ── D3: remote-image reveal projection (set + predicate) ──────────────────

    #[test]
    fn set_remote_images_revealed_flips_all_including_nested() {
        // A top-level promoted image, plus one inside a list item and one inside a
        // block quote — the projection must reach every RemoteImage (the manager
        // applies this for a message/post in its reveal set), and nothing else.
        let mut doc = RenderDocument {
            blocks: vec![
                RenderBlock::RemoteImage {
                    url: "http://top".into(),
                    alt: "t".into(),
                    revealed: false,
                },
                RenderBlock::ListBlock {
                    ordered: false,
                    items: vec![RenderDocument {
                        blocks: vec![RenderBlock::RemoteImage {
                            url: "http://list".into(),
                            alt: "l".into(),
                            revealed: false,
                        }],
                    }],
                },
                RenderBlock::BlockQuote {
                    blocks: vec![RenderBlock::RemoteImage {
                        url: "http://quote".into(),
                        alt: "q".into(),
                        revealed: false,
                    }],
                },
                // A trusted Image is NOT a remote image and stays untouched.
                RenderBlock::Image {
                    hash: "aa".into(),
                    alt: "trusted".into(),
                },
            ],
        };
        assert!(doc.has_blocked_remote_images());

        doc.set_remote_images_revealed(true);
        assert!(!doc.has_blocked_remote_images());
        // Every RemoteImage flipped, the trusted Image untouched.
        let revealed_flags: Vec<bool> = collect_revealed(&doc.blocks);
        assert_eq!(revealed_flags, vec![true, true, true]);

        // Idempotent the other way too.
        doc.set_remote_images_revealed(false);
        assert!(doc.has_blocked_remote_images());
        assert_eq!(collect_revealed(&doc.blocks), vec![false, false, false]);
    }

    #[test]
    fn has_blocked_remote_images_false_without_remote_images() {
        let doc = markdown_to_document("# Title\n\njust text, no images");
        assert!(!doc.has_blocked_remote_images());
        // A trusted (already-fetched) Image is never "blocked".
        let doc = RenderDocument {
            blocks: vec![RenderBlock::Image {
                hash: "aa".into(),
                alt: "x".into(),
            }],
        };
        assert!(!doc.has_blocked_remote_images());
    }

    #[test]
    fn link_preview_og_image_obeys_the_d3_reveal_posture() {
        // render-model.md § D4 (user-ratified 2026-06-27): a Resolved link-preview's og:image
        // is blocked-by-default exactly like a `RemoteImage`. Its `revealed` flag drives the
        // post's `load-remote-content-button` and flips with `set_remote_images_revealed`.
        let resolved = |image_hash: Option<&str>, revealed: bool| RenderBlock::LinkPreview {
            url: "https://example.com".into(),
            state: PreviewState::Resolved {
                title: "T".into(),
                description: "D".into(),
                image_hash: image_hash.map(Into::into),
                revealed,
            },
        };

        // A Resolved preview WITH an image, not yet revealed → blocked; revealing flips it.
        let mut doc = RenderDocument {
            blocks: vec![resolved(Some("ab"), false)],
        };
        assert!(
            doc.has_blocked_remote_images(),
            "an un-revealed og:image is blocked remote content",
        );
        doc.set_remote_images_revealed(true);
        assert!(
            !doc.has_blocked_remote_images(),
            "revealing the post un-gates the og:image",
        );
        // Idempotent the other way.
        doc.set_remote_images_revealed(false);
        assert!(doc.has_blocked_remote_images());

        // A Resolved preview with NO image, and the non-terminal/failed states, are never
        // "blocked" — there is nothing to gate (and they must not spawn a phantom button).
        for block in [
            resolved(None, false),
            RenderBlock::LinkPreview {
                url: "https://example.com".into(),
                state: PreviewState::Resolving,
            },
            RenderBlock::LinkPreview {
                url: "https://example.com".into(),
                state: PreviewState::Failed,
            },
        ] {
            let mut doc = RenderDocument {
                blocks: vec![block],
            };
            assert!(!doc.has_blocked_remote_images());
            // The reveal walk is a harmless no-op on these (no image flag to flip).
            doc.set_remote_images_revealed(true);
            assert!(!doc.has_blocked_remote_images());
        }
    }

    /// Test helper: gather every `RemoteImage.revealed` flag in document order,
    /// recursing into list items + block quotes.
    fn collect_revealed(blocks: &[RenderBlock]) -> Vec<bool> {
        let mut out = Vec::new();
        for b in blocks {
            match b {
                RenderBlock::RemoteImage { revealed, .. } => out.push(*revealed),
                RenderBlock::ListBlock { items, .. } => {
                    for item in items {
                        out.extend(collect_revealed(&item.blocks));
                    }
                }
                RenderBlock::BlockQuote { blocks } => out.extend(collect_revealed(blocks)),
                _ => {}
            }
        }
        out
    }

    // ── embed projections (the single-sourced twins of the per-app walkers) ──

    fn quoted(post_id: &str, body: &str, verification: VerificationStatus) -> RenderBlock {
        RenderBlock::QuotedPost {
            post_id: post_id.into(),
            author: "aa".into(),
            body: body.into(),
            verification,
            authoring_origin: AuthoringOriginStatus::Unknown,
            legal_takedown_ref: None,
            not_found: false,
        }
    }

    fn preview(url: &str, state: PreviewState) -> RenderBlock {
        RenderBlock::LinkPreview {
            url: url.into(),
            state,
        }
    }

    fn resolved(title: &str, image_hash: Option<&str>, revealed: bool) -> PreviewState {
        PreviewState::Resolved {
            title: title.into(),
            description: "d".into(),
            image_hash: image_hash.map(str::to_string),
            revealed,
        }
    }

    #[test]
    fn first_image_hash_takes_the_first_trusted_image_in_body_order() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("body")],
                },
                RenderBlock::Image {
                    hash: "aaa".into(),
                    alt: String::new(),
                },
                RenderBlock::Image {
                    hash: "bbb".into(),
                    alt: String::new(),
                },
            ],
        };
        assert_eq!(doc.first_image_hash(), Some("aaa"));
        // A *remote* image is not trusted media and never answers this.
        let remote = RenderDocument {
            blocks: vec![RenderBlock::RemoteImage {
                url: "https://x/i.png".into(),
                alt: String::new(),
                revealed: true,
            }],
        };
        assert_eq!(remote.first_image_hash(), None);
        assert_eq!(RenderDocument::default().first_image_hash(), None);
    }

    #[test]
    fn first_video_hash_takes_the_first_trusted_video_in_body_order() {
        // The exact twin of first_image_hash_takes_the_first_trusted_image_in_body_order.
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("body")],
                },
                RenderBlock::Video {
                    hash: "aaa".into(),
                    alt: String::new(),
                },
                RenderBlock::Video {
                    hash: "bbb".into(),
                    alt: String::new(),
                },
            ],
        };
        assert_eq!(doc.first_video_hash(), Some("aaa"));
        assert_eq!(RenderDocument::default().first_video_hash(), None);
    }

    #[test]
    fn first_video_hash_finds_a_nested_video() {
        // The exact twin of first_image_hash_finds_a_nested_image (fauna-ffi's own
        // render.rs) — recurses into a block quote instead of missing it top-level-only.
        let doc = RenderDocument {
            blocks: vec![RenderBlock::BlockQuote {
                blocks: vec![RenderBlock::Video {
                    hash: "feedbeef".into(),
                    alt: String::new(),
                }],
            }],
        };
        assert_eq!(doc.first_video_hash(), Some("feedbeef"));
    }

    #[test]
    fn quoted_post_projects_the_folded_embed_and_gates_the_fire_once_resolve() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Paragraph {
                    inlines: vec![text("look at this")],
                },
                quoted("p1", "quoted body", VerificationStatus::Failed),
            ],
        };
        let embed = doc.quoted_post().expect("folded quote projects");
        assert_eq!(embed.post_id, "p1");
        assert_eq!(embed.body, "quoted body");
        assert_eq!(embed.verification, VerificationStatus::Failed);
        assert_eq!(embed.legal_takedown_ref, None);
        assert!(doc.has_quoted_post());

        // Before the manager folds the quote in, the guard is open — this is what makes
        // the client's resolve fire exactly once.
        let unresolved = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![text("look at this")],
            }],
        };
        assert!(!unresolved.has_quoted_post());
        assert!(unresolved.quoted_post().is_none());
    }

    #[test]
    fn link_previews_split_by_resolution_state_in_body_order() {
        let doc = RenderDocument {
            blocks: vec![
                preview("https://a.example/1", PreviewState::Resolving),
                preview("https://b.example/2", resolved("B", Some("hb"), false)),
                preview("https://c.example/3", PreviewState::Failed),
                preview("https://d.example/4", resolved("D", None, true)),
            ],
        };

        // Resolving → the resolve-trigger list (fire-once: a resolved block drops out).
        assert_eq!(
            doc.resolving_link_preview_urls(),
            vec!["https://a.example/1"]
        );

        // Resolved → the cards, in body order; Failed/Resolving paint none.
        let cards = doc.resolved_link_previews();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].url, "https://b.example/2");
        assert_eq!(cards[0].title, "B");
        assert_eq!(cards[0].image_hash, Some("hb"));
        assert!(
            !cards[0].revealed,
            "og:image is blocked-by-default (D3 twin)"
        );
        assert_eq!(cards[1].url, "https://d.example/4");
        assert_eq!(cards[1].image_hash, None);
        assert!(cards[1].revealed);

        // Every preview, whatever its state, in body order — the state dump's read,
        // which is what lets a test tell "failed" from "still resolving" when both
        // paint no card.
        let all: Vec<(&str, &str)> = doc
            .link_previews()
            .into_iter()
            .map(|(url, state)| (url, state.name()))
            .collect();
        assert_eq!(
            all,
            vec![
                ("https://a.example/1", "resolving"),
                ("https://b.example/2", "resolved"),
                ("https://c.example/3", "failed"),
                ("https://d.example/4", "resolved"),
            ]
        );
    }

    /// The whole reason these projections are single-sourced: a hand-rolled per-app
    /// twin walks only the top level and silently misses an embed nested inside a quote
    /// or a list item — the same class of miss `has_blocked_remote_images` was lifted to
    /// prevent (render-model.md § Implementation status).
    #[test]
    fn every_projection_recurses_into_quotes_and_list_items() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::BlockQuote {
                    blocks: vec![
                        RenderBlock::Image {
                            hash: "nested".into(),
                            alt: String::new(),
                        },
                        RenderBlock::Video {
                            hash: "nested-video".into(),
                            alt: String::new(),
                        },
                        preview("https://nested.example/r", PreviewState::Resolving),
                    ],
                },
                RenderBlock::ListBlock {
                    ordered: false,
                    items: vec![RenderDocument {
                        blocks: vec![
                            quoted("pq", "in a list", VerificationStatus::Verified),
                            preview("https://nested.example/ok", resolved("N", None, true)),
                        ],
                    }],
                },
                RenderBlock::TaskList {
                    items: vec![TaskItem {
                        checked: true,
                        blocks: vec![
                            preview("https://nested.example/t", PreviewState::Resolving),
                            RenderBlock::RemoteImage {
                                url: "https://nested.example/t.png".into(),
                                alt: "in a task".into(),
                                revealed: true,
                            },
                        ],
                    }],
                },
            ],
        };

        assert_eq!(doc.first_image_hash(), Some("nested"));
        assert_eq!(doc.first_video_hash(), Some("nested-video"));
        assert_eq!(doc.quoted_post().map(|q| q.post_id), Some("pq"));
        assert!(doc.has_quoted_post());
        assert_eq!(
            doc.resolving_link_preview_urls(),
            vec!["https://nested.example/r", "https://nested.example/t"]
        );
        assert_eq!(doc.resolved_link_previews().len(), 1);
        assert_eq!(doc.resolved_link_previews()[0].title, "N");
        assert_eq!(
            doc.link_previews()
                .iter()
                .map(|(url, _)| *url)
                .collect::<Vec<_>>(),
            vec![
                "https://nested.example/r",
                "https://nested.example/ok",
                "https://nested.example/t"
            ],
        );
        assert_eq!(
            doc.remote_images()
                .iter()
                .map(|i| i.url)
                .collect::<Vec<_>>(),
            vec!["https://nested.example/t.png"],
            "the projection recurses into task items like every sibling"
        );
    }

    /// The `RemoteImage` projection — the 5th member of the embed-projection
    /// family, the read a client paints its own reveal-gated image element from
    /// (render-model.md § D3). Body order, both reveal states, and full
    /// recursion, exactly like `resolved_link_previews`.
    #[test]
    fn remote_images_projects_every_block_in_body_order_with_its_reveal_state() {
        let remote = |url: &str, alt: &str, revealed: bool| RenderBlock::RemoteImage {
            url: url.into(),
            alt: alt.into(),
            revealed,
        };
        let doc = RenderDocument {
            blocks: vec![
                remote("https://a.example/1.png", "first", false),
                RenderBlock::BlockQuote {
                    blocks: vec![remote("https://a.example/2.png", "quoted", true)],
                },
                RenderBlock::ListBlock {
                    ordered: false,
                    items: vec![RenderDocument {
                        blocks: vec![remote("https://a.example/3.png", "listed", false)],
                    }],
                },
                // A trusted by-hash Image is a DIFFERENT block with its own
                // element (`post-image`) — it must never leak into this read.
                RenderBlock::Image {
                    hash: "deadbeef".into(),
                    alt: String::new(),
                },
            ],
        };

        let images = doc.remote_images();
        assert_eq!(
            images.iter().map(|i| i.url).collect::<Vec<_>>(),
            vec![
                "https://a.example/1.png",
                "https://a.example/2.png",
                "https://a.example/3.png",
            ],
        );
        assert_eq!(
            images.iter().map(|i| i.alt).collect::<Vec<_>>(),
            vec!["first", "quoted", "listed"],
        );
        assert_eq!(
            images.iter().map(|i| i.revealed).collect::<Vec<_>>(),
            vec![false, true, false],
            "each block carries its OWN manager-projected reveal flag",
        );

        // The blocked ones are exactly what `has_blocked_remote_images` counts,
        // so one read can never disagree with the other about the reveal button.
        assert!(doc.has_blocked_remote_images());
        assert!(
            !RenderDocument {
                blocks: vec![remote("https://a.example/x.png", "", true)],
            }
            .has_blocked_remote_images()
        );
    }

    /// render-model.md § D6c: the sixth accessor recurses like its siblings, and a proxied
    /// image is never reveal-gated (the user-ruled `post-image` posture) — it never counts
    /// toward `has_blocked_remote_images`, never leaks into `remote_images()`, and rides
    /// `media_blocks()` so a quote/gated rebuild carries it across.
    #[test]
    fn proxied_images_recurse_paint_ungated_and_ride_media_blocks() {
        let proxied = |path: &str, alt: &str| RenderBlock::ProxiedImage {
            path: path.into(),
            alt: alt.into(),
        };
        let doc = RenderDocument {
            blocks: vec![
                proxied("/api/v1/bluesky/media?url=a", "first"),
                RenderBlock::BlockQuote {
                    blocks: vec![proxied("/api/v1/media/proxy?url=b", "quoted")],
                },
                RenderBlock::Image {
                    hash: "deadbeef".into(),
                    alt: String::new(),
                },
            ],
        };
        let got = doc.proxied_images();
        assert_eq!(
            got.iter().map(|i| (i.path, i.alt)).collect::<Vec<_>>(),
            vec![
                ("/api/v1/bluesky/media?url=a", "first"),
                ("/api/v1/media/proxy?url=b", "quoted"),
            ],
        );
        assert!(!doc.has_blocked_remote_images());
        assert!(doc.remote_images().is_empty());
        assert_eq!(doc.first_image_hash(), Some("deadbeef"));
        assert_eq!(
            doc.media_blocks(),
            vec![
                proxied("/api/v1/bluesky/media?url=a", "first"),
                RenderBlock::Image {
                    hash: "deadbeef".into(),
                    alt: String::new(),
                },
            ],
            "top-level media only, body order",
        );
        let mut revealed = doc.clone();
        revealed.set_remote_images_revealed(true);
        assert_eq!(
            revealed, doc,
            "the reveal walk never touches a proxied image"
        );
    }

    /// render-model.md § D6c → *Proxied video*: the seventh accessor, the exact twin of
    /// `proxied_images()` — recurses, never reveal-gated, never an image or video hash, and
    /// rides `media_blocks()`.
    #[test]
    fn proxied_videos_recurse_paint_ungated_and_ride_media_blocks() {
        let proxied = |path: &str, alt: &str| RenderBlock::ProxiedVideo {
            path: path.into(),
            alt: alt.into(),
        };
        let doc = RenderDocument {
            blocks: vec![
                proxied("/api/v1/media/proxy?url=a", "first"),
                RenderBlock::ListBlock {
                    ordered: false,
                    items: vec![RenderDocument {
                        blocks: vec![proxied("/api/v1/media/proxy?url=b", "listed")],
                    }],
                },
                RenderBlock::ProxiedImage {
                    path: "/api/v1/media/proxy?url=c".into(),
                    alt: String::new(),
                },
            ],
        };
        assert_eq!(
            doc.proxied_videos()
                .iter()
                .map(|v| (v.path, v.alt))
                .collect::<Vec<_>>(),
            vec![
                ("/api/v1/media/proxy?url=a", "first"),
                ("/api/v1/media/proxy?url=b", "listed"),
            ],
        );
        assert_eq!(
            doc.proxied_images().len(),
            1,
            "a proxied video never leaks into the image accessor"
        );
        assert!(!doc.has_blocked_remote_images());
        assert_eq!(doc.first_video_hash(), None);
        assert_eq!(doc.first_image_hash(), None);
        assert_eq!(
            doc.media_blocks(),
            vec![
                proxied("/api/v1/media/proxy?url=a", "first"),
                RenderBlock::ProxiedImage {
                    path: "/api/v1/media/proxy?url=c".into(),
                    alt: String::new(),
                },
            ],
            "top-level media only, body order",
        );
        let mut revealed = doc.clone();
        revealed.set_remote_images_revealed(true);
        assert_eq!(
            revealed, doc,
            "the reveal walk never touches a proxied video"
        );
    }

    /// The `post-image` / `video-thumbnail` slot precedence (render-model.md § D6c): the
    /// first proxied path paints only when the post has no blob image / video.
    #[test]
    fn a_proxied_post_slot_yields_to_a_blob_and_takes_the_first_path() {
        let image = |p: &str| RenderBlock::ProxiedImage {
            path: p.into(),
            alt: String::new(),
        };
        let video = |p: &str| RenderBlock::ProxiedVideo {
            path: p.into(),
            alt: String::new(),
        };
        let bridged = RenderDocument {
            blocks: vec![image("/i/a"), image("/i/b"), video("/v/a"), video("/v/b")],
        };
        assert_eq!(bridged.proxied_post_image(), Some("/i/a"));
        assert_eq!(bridged.proxied_post_video(), Some("/v/a"));

        let mut mixed = bridged.clone();
        mixed.blocks.push(RenderBlock::Image {
            hash: "ab".repeat(32),
            alt: String::new(),
        });
        mixed.blocks.push(RenderBlock::Video {
            hash: "cd".repeat(32),
            alt: String::new(),
        });
        assert_eq!(
            mixed.proxied_post_image(),
            None,
            "a blob image takes the slot"
        );
        assert_eq!(
            mixed.proxied_post_video(),
            None,
            "a blob video takes the slot"
        );
        assert_eq!(RenderDocument::default().proxied_post_image(), None);
    }

    #[test]
    fn to_plaintext_is_fast_for_a_multi_megabyte_body() {
        // Diagnostic for the macos+ios full-app hang (`mail-message-size.md` §
        // Implementation status today): isolates
        // `to_plaintext`'s own cost from the SwiftUI/UniFFI call sites that were
        // re-invoking it on every body pass. Shape matches the e2e over-frame
        // test's body (`test_mail_client_receive_over_frame_reference.py`): ~3 MiB
        // of 76-char lines, single-newline separated (one giant paragraph, since
        // `plaintext_to_document`'s blank-line split finds no blank line here).
        let filler_line = "z".repeat(76) + "\n";
        let filler = filler_line.repeat((3 * 1024 * 1024) / filler_line.len());
        let body = format!("HEAD\n{filler}TAIL\n");
        let doc = plaintext_to_document(&body);

        let started = std::time::Instant::now();
        let text = doc.to_plaintext();
        let elapsed = started.elapsed();

        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
        // Measured on macOS, 2026-09-11: 61.5ms — `to_plaintext` alone never
        // explained the apple hang; its profiled cause was SwiftUI's text layout of
        // the paragraph's ~40k hard line breaks (`MAX_LINES_PER_TEXT_RUN`). Printed,
        // not asserted: a wall-clock bound on a shared build box is no verdict.
        eprintln!("to_plaintext_is_fast_for_a_multi_megabyte_body: {elapsed:?} for a ~3 MiB body");
    }

    // ── line runs (the native text-widget budget) ────────────────────────────

    /// The text a run of inlines paints, hard breaks included — the losslessness
    /// oracle for the line-run tests (`to_plaintext` collapses whitespace, so it
    /// cannot see a dropped or doubled `\n`).
    fn painted(inlines: &[Inline]) -> String {
        inlines
            .iter()
            .map(|inline| match inline {
                Inline::Text { text } | Inline::Code { text } => text.clone(),
                Inline::Bold { inlines } | Inline::Italic { inlines } => painted(inlines),
                Inline::Link { inlines, .. } => painted(inlines),
            })
            .collect()
    }

    fn numbered_lines(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_paragraph_within_the_budget_is_one_run_equal_to_its_input() {
        // Nearly all real prose: the shell must paint it exactly as before.
        let inlines = vec![
            text(&numbered_lines(MAX_LINES_PER_TEXT_RUN - 1)),
            Inline::Bold {
                inlines: vec![text("\nbold last line")],
            },
            Inline::Link {
                href: "https://a.example".into(),
                inlines: vec![text("")],
            },
        ];
        assert_eq!(inline_line_runs(&inlines), vec![inlines.clone()]);
        assert_eq!(inline_line_runs(&[]), vec![Vec::<Inline>::new()]);
    }

    #[test]
    fn the_budget_is_exactly_max_lines_per_run() {
        let at_budget = vec![text(&numbered_lines(MAX_LINES_PER_TEXT_RUN))];
        assert_eq!(inline_line_runs(&at_budget).len(), 1);

        let one_over = vec![text(&numbered_lines(MAX_LINES_PER_TEXT_RUN + 1))];
        let runs = inline_line_runs(&one_over);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[1],
            vec![text(&format!("line {}", MAX_LINES_PER_TEXT_RUN + 1))]
        );
    }

    #[test]
    fn line_runs_are_lossless_and_capped() {
        let source = numbered_lines(100);
        let runs = inline_line_runs(&[text(&source)]);

        assert_eq!(runs.len(), 100usize.div_ceil(MAX_LINES_PER_TEXT_RUN));
        for run in &runs {
            let lines = painted(run).split('\n').count();
            assert!(lines <= MAX_LINES_PER_TEXT_RUN, "a run of {lines} lines");
        }
        // Each cut consumes exactly the one `\n` it falls on.
        let rejoined = runs
            .iter()
            .map(|r| painted(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rejoined, source);
    }

    #[test]
    fn styling_survives_a_cut_on_both_sides() {
        // Two lines per run, so every break in `bold` and in the link's italic is
        // a cut: each fragment must keep its wrappers (and the link its href).
        let inlines = vec![
            text("plain\n"),
            Inline::Bold {
                inlines: vec![text("b1\nb2")],
            },
            Inline::Link {
                href: "https://a.example/x".into(),
                inlines: vec![Inline::Italic {
                    inlines: vec![text("\ni1\ni2")],
                }],
            },
        ];
        let runs = LineRunSplitter::new(2).split_seq(&inlines);
        let link = |inner: &str| Inline::Link {
            href: "https://a.example/x".into(),
            inlines: vec![Inline::Italic {
                inlines: vec![text(inner)],
            }],
        };
        assert_eq!(
            runs,
            vec![
                vec![
                    text("plain\n"),
                    Inline::Bold {
                        inlines: vec![text("b1")],
                    },
                ],
                vec![
                    Inline::Bold {
                        inlines: vec![text("b2")],
                    },
                    link("\ni1"),
                ],
                vec![link("i2")],
            ]
        );
    }

    #[test]
    fn a_blank_line_at_a_cut_survives_as_the_next_runs_leading_break() {
        let runs = LineRunSplitter::new(2).split_seq(&[text("a\nb\n\nc")]);
        assert_eq!(runs, vec![vec![text("a\nb")], vec![text("\nc")]]);
    }

    #[test]
    fn a_code_block_splits_into_line_runs_too() {
        let source = numbered_lines(40);
        let runs = text_line_runs(&source);
        assert_eq!(runs.len(), 40usize.div_ceil(MAX_LINES_PER_TEXT_RUN));
        assert!(
            runs.iter()
                .all(|r| r.split('\n').count() <= MAX_LINES_PER_TEXT_RUN)
        );
        assert_eq!(runs.join("\n"), source);
        assert_eq!(text_line_runs("one\ntwo"), vec!["one\ntwo".to_string()]);
    }

    #[test]
    fn the_over_frame_mail_body_splits_into_budget_sized_runs() {
        // The shape that hung apple (`mail-message-size.md` § Implementation status
        // today): `test_mail_client_receive_over_frame_reference.py`'s ~3 MiB of
        // 76-char lines with no blank line, i.e. ONE paragraph of ~40k lines.
        let filler_line = "z".repeat(76) + "\r\n";
        let filler = filler_line.repeat((3 * 1024 * 1024) / filler_line.len());
        let doc = plaintext_to_document(&format!("HEAD\r\n{filler}TAIL\r\n"));
        let [RenderBlock::Paragraph { inlines }] = doc.blocks.as_slice() else {
            panic!("expected one paragraph, got {:?} blocks", doc.blocks.len());
        };
        let lines = painted(inlines).split('\n').count();

        let runs = inline_line_runs(inlines);
        assert_eq!(runs.len(), lines.div_ceil(MAX_LINES_PER_TEXT_RUN));
        assert!(painted(&runs[0]).starts_with("HEAD\n"));
        assert!(painted(runs.last().unwrap()).ends_with("TAIL"));
    }

    #[test]
    fn a_legally_withheld_quote_projects_its_reference() {
        let doc = RenderDocument {
            blocks: vec![RenderBlock::QuotedPost {
                post_id: "p".into(),
                author: String::new(),
                body: String::new(),
                verification: VerificationStatus::Unchecked,
                authoring_origin: AuthoringOriginStatus::Unknown,
                legal_takedown_ref: Some("DMCA-1".into()),
                not_found: false,
            }],
        };
        let embed = doc
            .quoted_post()
            .expect("a withheld quote is still an embed");
        assert_eq!(embed.legal_takedown_ref, Some("DMCA-1"));
        assert!(embed.body.is_empty(), "the nest withholds the body");
        assert!(!embed.not_found, "withheld is not gone");
    }

    /// A quote of a deleted post is still an embed — one that says the post is
    /// not there (`ui/feed.md` § Post deletion) — and the flag survives the
    /// owned mirror every FFI/wasm face hands to an app.
    #[test]
    fn a_quote_of_a_post_that_is_gone_projects_not_found() {
        let doc = RenderDocument {
            blocks: vec![RenderBlock::QuotedPost {
                post_id: "p".into(),
                author: String::new(),
                body: String::new(),
                verification: VerificationStatus::Unchecked,
                authoring_origin: AuthoringOriginStatus::Unknown,
                legal_takedown_ref: None,
                not_found: true,
            }],
        };
        let embed = doc.quoted_post().expect("a gone quote is still an embed");
        assert!(embed.not_found);
        assert_eq!(embed.legal_takedown_ref, None);
        assert!(QuotedPostEmbedOwned::from(embed).not_found);
    }

    /// A block serialized before `not_found` existed reads as a live quote.
    #[test]
    fn a_quoted_post_block_from_before_not_found_reads_as_live() {
        let mut value = serde_json::to_value(RenderBlock::QuotedPost {
            post_id: "p".into(),
            author: "a".into(),
            body: "b".into(),
            verification: VerificationStatus::Unchecked,
            authoring_origin: AuthoringOriginStatus::Unknown,
            legal_takedown_ref: None,
            not_found: true,
        })
        .unwrap();
        let fields = value
            .as_object_mut()
            .and_then(|o| o.values_mut().next())
            .and_then(|v| v.as_object_mut())
            .expect("an externally tagged struct variant");
        assert!(fields.remove("not_found").is_some());
        let block: RenderBlock = serde_json::from_value(value).unwrap();
        assert!(matches!(
            block,
            RenderBlock::QuotedPost {
                not_found: false,
                ..
            }
        ));
    }
}

/**
 * Web painter for the shared semantic `RenderDocument` (render-model.md § D1).
 *
 * The conversations manager (shared Rust) builds `MessageSnapshot.document` once — choosing
 * the markdown / plaintext / inbound-HTML producer — and the snapshot carries the typed
 * block/inline/embed tree across `serde` (`thread_detail` serialises the whole `ThreadDetail`,
 * externally-tagged enums). This module is the **web** painter: it walks that document into
 * the message-bubble body HTML, the DOM twin of linux `views/conversations/document.rs`. No
 * body is re-parsed at render time — the bubble paints the document the manager already built
 * (render-model.md § The boundary: "the shell's whole job is `match block { … }` → construct
 * the native widget"; for web the native widget is DOM).
 *
 * The HTML matches the shared `fauna_core::markdown::render_html_with` tag output, so the
 * shipped CSS and the conversations e2e are unchanged — with one intentional difference the
 * document model makes uniform across every app: a remote `![]()` image is a **sibling
 * block** (the producer promotes it out of its paragraph in body order — render-model.md
 * § D2), not inline inside the `<p>`. linux already rendered remote images on their own line;
 * this brings web into line.
 *
 * The remote-image reveal state is the **manager's** (render-model.md § D3): a manager owns the
 * per-message / per-post reveal set and projects it onto each `RemoteImage.revealed` field of the
 * document it emits. So every `RemoteImage` block carries its OWN authoritative `revealed` bool,
 * and the painter reads it per-block — `revealed` chooses the posture: blocked-by-default
 * `<img data-remote-src … class="blocked-remote-image">` (no `src` → no fetch) vs. the
 * `<img src …>` the user opted into via `load-remote-content-button` (which now dispatches to the
 * manager). The web app keeps no reveal dictionary of its own.
 */

import {
  renderDocumentHasBlockedRemoteImages,
  renderDocumentQuotedPost,
  renderDocumentFirstImageHash,
  renderDocumentMediaBlocks,
  renderDocumentProxiedImages,
  renderDocumentResolvingLinkPreviewUrls,
  renderDocumentResolvedLinkPreviews,
} from '$lib/wasm';
import { IDS } from '$lib/generated/uiIds';

// The serde **externally-tagged** JS shapes of `fauna_core::render` — each enum value is an
// object with a single key naming the variant. Kept in lockstep with `libs/fauna-core/src/render.rs`.
export type Inline =
  | { Text: { text: string } }
  | { Bold: { inlines: Inline[] } }
  | { Italic: { inlines: Inline[] } }
  | { Code: { text: string } }
  | { Link: { href: string; inlines: Inline[] } };

export type RenderBlock =
  | { Paragraph: { inlines: Inline[] } }
  | { Heading: { level: number; inlines: Inline[] } }
  | { ListBlock: { ordered: boolean; items: RenderDocument[] } }
  // A GFM task list (render-model.md § D7a) — sibling of ListBlock; each item carries `checked`
  // and a sub-document (`blocks`). Painted as disabled checkboxes (the web-native idiom; native
  // apps render a ☐/☑ glyph — the concept is shared, the visual stays per-platform).
  | { TaskList: { items: { checked: boolean; blocks: RenderBlock[] }[] } }
  | { CodeBlock: { lang: string | null; text: string } }
  | { BlockQuote: { blocks: RenderBlock[] } }
  | { Image: { hash: string; alt: string } }
  // The typed video sibling of `Image` (render-model.md § Implementation status today). The
  // shared feed fold branches image-vs-video off `MediaItem.media_type` and emits this, which
  // is what let web stop calling `decodePost` a second time to find its videos.
  | { Video: { hash: string; alt: string } }
  // A bridged post's own picture, served by the reader's own nest at a nest-relative proxied
  // `path` (render-model.md § D6c). Paints in the `post-image` slot immediately (no reveal), via
  // an authenticated `fetch` → object URL (the page's `proxiedMediaUrl`), never a bare
  // `<img src>` to the path and never the old zero-hash blob fetch.
  | { ProxiedImage: { path: string; alt: string } }
  // A bridged post's own video at the same nest-relative proxied `path` form (render-model.md
  // § D6c → *Proxied video*). Paints in the `video-thumbnail` slot as the play glyph over a
  // poster-less frame carrying the path, never byte-loaded.
  | { ProxiedVideo: { path: string; alt: string } }
  | { RemoteImage: { url: string; alt: string; revealed: boolean } }
  | { Attachment: AttachmentPayload }
  | { QuotedPost: QuotedPostPayload }
  | { QuotedMessage: QuotedMessagePayload }
  | { LinkPreview: LinkPreviewPayload };

/** The resolution state of a `RenderBlock::LinkPreview` (render-model.md § D4). The serde
 *  shape of `fauna_core::render::PreviewState`: the unit variants serialize as the plain
 *  string (`'Resolving'` / `'Failed'`), the `Resolved` struct variant as the single-key
 *  object. `Resolving` is the state the producer emits; the manager resolves it to
 *  `Resolved`/`Failed` via `fauna.linkpreview.resolve` (a per-app leg, not yet wired). */
export type PreviewState =
  | 'Resolving'
  // `revealed` is the D3-twin reveal flag the manager projects (render-model.md § D4,
  // user-ratified 2026-06-27): the og:image is blocked-by-default like any `RemoteImage`,
  // so the card paints `image_hash` only when `revealed`. title/description/domain always show.
  | { Resolved: { title: string; description: string; image_hash: string | null; revealed: boolean } }
  | 'Failed';

/** The payload of a `RenderBlock::LinkPreview` — the serde shape of the render-model § D4
 *  link-preview embed. The producer emits one in `Resolving` for a standalone bare-url
 *  paragraph, leaving the inline link in place; the preview card (Resolving→skeleton,
 *  Resolved→full card, Failed→plain link) + the manager resolve-call are the web client-adoption
 *  leg (the `link-preview-card` ui.yaml element added there), blocked on
 *  the nest `fauna.linkpreview.resolve` handler. Carried in the type so the document is fully
 *  typed (lockstep with render.rs); a NO-OP in `blockToHtml` until that leg lands. */
export interface LinkPreviewPayload {
  url: string;
  state: PreviewState;
}

/** One `PreviewState::Resolved` link preview as the shared `RenderDocument::resolved_link_previews`
 *  projection hands it over — the TS twin of `fauna_core::render::ResolvedLinkPreviewOwned` (same
 *  owned-mirror reason as `QuotedPostEmbed`). The state match lives in shared Rust, so a client
 *  gets the flat card payload and never re-derives `Resolved` from `PreviewState` itself. */
export interface ResolvedLinkPreview {
  url: string;
  title: string;
  description: string;
  image_hash: string | null;
  revealed: boolean;
}

/** The payload of a `RenderBlock::QuotedPost` — the serde (snake_case) shape of
 *  `fauna_feed::QuotedPostView` (the feed quoted-post embed, render-model.md § D6). The feed
 *  manager folds one in after the body once `resolve_quoted_post` resolves the quote. Carried
 *  in the type so the document is fully typed; the feed page's client-adoption leg renders the
 *  card from this block (and drops the sibling `QuotedPost.svelte` prop render). */
export interface QuotedPostPayload {
  post_id: string;
  author: string;
  body: string;
  /** `Some(reference)` when the quoted post has been **taken down under a legal
   *  obligation** (`moderation.md` § Categories & enforcement item 1): the nest
   *  withheld its body, so `author`/`body` are empty and the card renders the
   *  shared tombstone (`legalTakedownTombstone(reference)` → "Removed under legal
   *  obligation ({reference})") in place of the quoted content — never a
   *  blank/broken embed. Carried from `QuotedPostView::legal_takedown_ref` through
   *  the folded `RenderBlock::QuotedPost`; absent/`null` for every normal quote. */
  legal_takedown_ref?: string | null;
  /** `true` when the quoted post is **no longer there** — its author deleted it, so
   *  the nest answers `fauna.posts.not_found` (feed.md § Post deletion: references
   *  to a deleted post dangle by design and render the not-found state). `author`/
   *  `body` are empty; the card paints `feed.post.post_not_found` in their place.
   *  Absent/`false` for every live quote. */
  not_found?: boolean;
  /** Whether THIS client verified the *quoted* post's signed envelope, carried
   *  from `QuotedPostView::verification` (security.md § Client display of unverified
   *  content). The shared `VerificationStatus` enum serializes as the plain variant
   *  string (`'Unchecked'` / `'Verified'` / `'Failed'`); the quoted-post card paints
   *  the `unverified-source-badge` iff `'Failed'` (Slice 2b). */
  verification: string;
}

/** What the shared `RenderDocument::quoted_post` projection hands a client to paint the
 *  `quoted-post` card — the TS twin of `fauna_core::render::QuotedPostEmbedOwned` (the owned
 *  mirror the wasm/UniFFI faces return, since the borrowed `QuotedPostEmbed` carries a Rust
 *  lifetime that can't cross the boundary).
 *
 *  A superset of `QuotedPostPayload`: it also carries `authoring_origin`, the D10 audit surface
 *  (`AuthoringOriginStatus` — `'Unknown' | 'Direct' | 'Delegated'`) that gates the embed's
 *  `delegated-origin-badge`. tui led that badge (ui.yaml `delegated-origin-badge`, 2026-07-31);
 *  web joined the trickle-down 2026-08-15 and `QuotedPost.svelte` now paints it from this field
 *  via `DelegatedOriginBadge`. */
export interface QuotedPostEmbed {
  post_id: string;
  author: string;
  body: string;
  verification: string;
  authoring_origin: string;
  legal_takedown_ref: string | null;
}

/** The payload of a `RenderBlock::QuotedMessage` — the serde (snake_case) shape of the
 *  in-bubble reply-quote (render-model.md § D2). The conversations manager folds one in at
 *  read time (prepended, above the body) when a message replies to a parent loaded in the
 *  same thread. The bubble template paints the card from `quotedMessageBlock(msg.document)`
 *  (it is a NO-OP in `blockToHtml`, like `QuotedPost`). */
export interface QuotedMessagePayload {
  author_display: string;
  snippet: string;
}

/** The payload of a `RenderBlock::Attachment` — the serde (snake_case) shape of
 *  `fauna_conversations::AttachmentSnapshot`, so the bubble template renders it with the same
 *  `attachmentUrl(att)` / `formatBytes(att.size_bytes)` helpers it used for `msg.attachments`. */
export interface AttachmentPayload {
  blob_hash: string;
  filename: string;
  mime_type: string;
  size_bytes: number;
  is_image: boolean;
  c2pa: boolean;
}

export interface RenderDocument {
  blocks: RenderBlock[];
}

/** HTML-escape, matching shared `fauna_core::markdown::escape_html` (`&` first, then `<`, `>`, `"`). */
function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/**
 * Concatenate an inline run (the typed `Inline` tree) to an HTML string, every text value
 * escaped. Recurses into nested emphasis/links — a `bold && italic` span the producer nests
 * as `Bold(Italic(Text))` renders `<strong><em>…</em></strong>`. Style mirrors the shared
 * `render_spans`: code, link, then bold/italic.
 */
function inlinesToHtml(inlines: Inline[]): string {
  let out = '';
  for (const i of inlines) {
    if ('Text' in i) out += escapeHtml(i.Text.text);
    else if ('Code' in i) out += `<code>${escapeHtml(i.Code.text)}</code>`;
    else if ('Link' in i)
      out += `<a href="${escapeHtml(i.Link.href)}" rel="noopener noreferrer" target="_blank">${inlinesToHtml(i.Link.inlines)}</a>`;
    else if ('Bold' in i) out += `<strong>${inlinesToHtml(i.Bold.inlines)}</strong>`;
    else if ('Italic' in i) out += `<em>${inlinesToHtml(i.Italic.inlines)}</em>`;
  }
  return out;
}

/** Render one remote `![alt](url)` image, blocked-by-default unless `revealed` (the shared `render_image`
 *  posture). Carries `data-testid="doc-remote-image"` in BOTH branches — the element registers in every
 *  state under one id (ui.yaml `doc-remote-image`), so a blocked-vs-painted assertion is about the
 *  element's attributes rather than its existence. */
function renderImage(url: string, alt: string, revealed: boolean): string {
  const a = escapeHtml(alt);
  const u = escapeHtml(url);
  // No `src` attribute → the browser performs no request until reveal copies
  // `data-remote-src` into `src` (html-mail.md § Security & privacy).
  return revealed
    ? `<img data-testid="${IDS.DOC_REMOTE_IMAGE}" src="${u}" alt="${a}">`
    : `<img data-testid="${IDS.DOC_REMOTE_IMAGE}" alt="${a}" data-remote-src="${u}" class="blocked-remote-image">`;
}

/**
 * A list item's inner HTML. A markdown list item is one `Paragraph` → render its inlines
 * directly inside `<li>` (matching `render_html_with`'s `<li>{inline}</li>`, no nested `<p>`);
 * a richer item (future producers) falls back to block rendering.
 *
 * `forceReveal` is the optional client-built-document override (see `documentToHtml`); `undefined`
 * for the normal manager-projected path, where each `RemoteImage` block carries its own `revealed`.
 */
function itemInner(item: RenderDocument, forceReveal: boolean | undefined): string {
  const blocks = item.blocks ?? [];
  if (blocks.length === 1 && 'Paragraph' in blocks[0]) {
    return inlinesToHtml(blocks[0].Paragraph.inlines);
  }
  return blocks.map((b) => blockToHtml(b, forceReveal)).join('\n');
}

/**
 * Render one `RenderBlock` to HTML, mirroring shared `render_html_with`. A `RemoteImage` is
 * rendered from its OWN `block.RemoteImage.revealed` field (the manager projects the reveal set
 * onto it — render-model.md § D3), unless `forceReveal === true` forces every remote image
 * revealed (the client-built-document override — see `documentToHtml`).
 */
function blockToHtml(block: RenderBlock, forceReveal: boolean | undefined): string {
  if ('Paragraph' in block) return `<p>${inlinesToHtml(block.Paragraph.inlines)}</p>`;
  if ('Heading' in block) {
    const l = block.Heading.level;
    return `<h${l}>${inlinesToHtml(block.Heading.inlines)}</h${l}>`;
  }
  if ('ListBlock' in block) {
    const tag = block.ListBlock.ordered ? 'ol' : 'ul';
    const items = block.ListBlock.items.map((it) => `<li>${itemInner(it, forceReveal)}</li>`).join('');
    return `<${tag}>${items}</${tag}>`;
  }
  // A GFM task list (render-model.md § D7a): each item is a disabled checkbox + its content
  // (the `<input>` reflects `checked`; read-side render only — the editable checkbox is Notes).
  if ('TaskList' in block) {
    const items = block.TaskList.items
      .map((it) => {
        const box = `<input type="checkbox" disabled${it.checked ? ' checked' : ''}>`;
        return `<li>${box} ${itemInner({ blocks: it.blocks }, forceReveal)}</li>`;
      })
      .join('');
    return `<ul class="task-list">${items}</ul>`;
  }
  if ('CodeBlock' in block) return `<pre><code>${escapeHtml(block.CodeBlock.text)}</code></pre>`;
  if ('BlockQuote' in block)
    return `<blockquote>${block.BlockQuote.blocks.map((b) => blockToHtml(b, forceReveal)).join('\n')}</blockquote>`;
  if ('RemoteImage' in block)
    return renderImage(block.RemoteImage.url, block.RemoteImage.alt, forceReveal === true || block.RemoteImage.revealed);
  // `Attachment` embeds (render-model.md § D2) are painted by the bubble's reactive Svelte
  // template (`attachmentBlocks`), NOT here: an image attachment needs an async blob URL that
  // `{@html}` can't produce. Skipped in the body HTML.
  if ('Attachment' in block) return '';
  // `QuotedPost` (the feed quoted-post embed, render-model.md § D6) is a NO-OP here for now:
  // the feed page still paints the quote via its own `QuotedPost.svelte`, so rendering it in
  // the body HTML too would double-render. The feed client-adoption leg renders the card from
  // this block (+ drops the sibling component) once that leg lands.
  if ('QuotedPost' in block) return '';
  // `QuotedMessage` (the in-bubble reply-quote, render-model.md § D2) is a NO-OP in the body
  // HTML: the bubble template paints it as a card above the body from `quotedMessageBlock`, so
  // emitting it here too would double-render (and pollute the `dm-message-text` read).
  if ('QuotedMessage' in block) return '';
  // `LinkPreview` (render-model.md § D4) is a NO-OP here for now: the shared producer emits it in
  // `Resolving` for a standalone bare-url paragraph, leaving the inline link in the paragraph — so
  // the link is already shown, and the body HTML emits nothing until the web link-preview-card leg
  // wires the card + the manager `fauna.linkpreview.resolve` call (blocked on the nest handler).
  // Painting a skeleton now would show a perpetual loading state.
  if ('LinkPreview' in block) return '';
  // `Image` / `Video` (trusted media addressed by content hash) and `ProxiedImage` /
  // `ProxiedVideo` (bridged media at a nest path) are painted by the PAGE as their own `post-image` /
  // `video-thumbnail` elements through the client blob loader — the body
  // `{@html}` cannot produce an async blob URL. Emitting them here would double-render, so this
  // walk deliberately ends in nothing (keeps the body byte-clean).
  return '';
}

/**
 * Walk a shared `RenderDocument` into the message-bubble body HTML (`{@html …}` it inside
 * `dm-message-text`). Blocks join with `\n`, matching shared `render_html_with`.
 *
 * Each `RemoteImage` block is normally rendered from its OWN `revealed` field — the manager owns
 * the reveal set and projects it onto the document it emits (render-model.md § D3). The optional
 * `opts.revealed === true` is a per-call override that forces ALL remote images revealed; it
 * exists for the ONE client-built-document case the manager can't project onto: the feed detail
 * builds its own full-body document client-side via `markdownToDocument` (it is not a manager
 * snapshot doc), so the feed-detail page keeps a local render toggle and passes it here (see
 * `apps/fauna-web/src/routes/feed/+page.svelte`). Everywhere else, omit `opts` (or leave
 * `opts.revealed` unset) and the per-block `RemoteImage.revealed` decides.
 */
export function documentToHtml(doc: RenderDocument | undefined, opts?: { revealed?: boolean }): string {
  if (!doc || !doc.blocks) return '';
  const forceReveal = opts?.revealed === true ? true : undefined;
  return doc.blocks.map((b) => blockToHtml(b, forceReveal)).join('\n');
}

/**
 * Whether the document carries any remote image still BLOCKED (`revealed === false`) — the gate
 * for `load-remote-content-button`. Delegates to the shared `RenderDocument::has_blocked_remote_images`
 * over the `renderDocumentHasBlockedRemoteImages` wasm boundary (the browser twin of the native
 * UniFFI face; render-model.md § D3/D4): a body `![]()` `RemoteImage`, OR a Resolved `LinkPreview`
 * whose og:image (`image_hash`) is not yet revealed, counts — recursing into `ListBlock`/`TaskList`
 * items + `BlockQuote`. Single-sourcing the walk across all 7 apps means a new blocked-content arm
 * (the D4 og:image, a D7a task-list) can't drift per-app. Once the manager has projected
 * `revealed: true` onto every remote image of a message/post, this returns false and the button hides.
 */
export function documentHasBlockedRemoteImages(doc: RenderDocument | undefined): boolean {
  if (!doc || !doc.blocks) return false;
  return renderDocumentHasBlockedRemoteImages(doc);
}

/**
 * The `Attachment` embed blocks, in body order — the conversations manager appends one per
 * attachment after the text body (render-model.md § D2). The bubble iterates these instead of
 * the sibling `msg.attachments` field, so attachments are sourced from the one document like
 * every other block; an image attachment is then painted by the reactive Svelte template
 * (it needs an async blob URL `documentToHtml`'s `{@html}` string can't produce).
 */
export function attachmentBlocks(doc: RenderDocument | undefined): AttachmentPayload[] {
  if (!doc || !doc.blocks) return [];
  return doc.blocks.flatMap((b) => ('Attachment' in b ? [b.Attachment] : []));
}

/**
 * The folded `QuotedPost` embed (the feed quoted-post, render-model.md § D6), or null. The feed
 * manager folds one in after the body once `resolve_quoted_post` resolves the quote; the feed page
 * renders the card from this block (sourcing the embed from the one document like every other
 * block) instead of the former resolve→`quotedPosts` map→prop path. Painted by the reactive Svelte
 * template (the card is its own `quoted-post` element), not the `{@html}` body string.
 *
 * Delegates to the shared `RenderDocument::quoted_post` over the `renderDocumentQuotedPost` wasm
 * boundary (the browser twin of the native UniFFI face) — the same single-sourcing as
 * `documentHasBlockedRemoteImages`: the walk recurses into `ListBlock`/`TaskList` items and
 * `BlockQuote`, so a client can't drift to a top-level-only scan that silently misses a nested
 * embed if a future producer folds one there.
 */
export function quotedPostBlock(doc: RenderDocument | undefined): QuotedPostEmbed | null {
  if (!doc || !doc.blocks) return null;
  return renderDocumentQuotedPost(doc);
}

/**
 * The in-bubble reply-quote block (render-model.md § D2 `QuotedMessage`), or null. The
 * conversations manager folds one in at read time (prepended, above the body) when a message
 * replies to a parent loaded in the same thread. The bubble template paints it as a card above
 * the body (its own `dm-message-quote` element), not the `{@html}` body string.
 */
export function quotedMessageBlock(doc: RenderDocument | undefined): QuotedMessagePayload | null {
  if (!doc || !doc.blocks) return null;
  for (const b of doc.blocks) if ('QuotedMessage' in b) return b.QuotedMessage;
  return null;
}

/**
 * The content hash of the first trusted `Image` block (the feed's resolved media,
 * render-model.md § D6), or null. `resolve_media` folds it in; the feed page paints it through the
 * client blob loader (the body `{@html}` can't produce an async blob URL — the same reason an
 * image `Attachment` paints reactively). This is the snapshot-media fallback BELOW web's richer
 * decoded multi-image path.
 *
 * Delegates to the shared `RenderDocument::first_image_hash` over the `renderDocumentFirstImageHash`
 * wasm boundary (the browser twin of the native UniFFI face linux/tui already consume) — hash only,
 * matching the shared projection: the block's `alt` is not part of it. That loses nothing today —
 * the feed fold sets `alt` to the empty string (`fauna-feed` `manager.rs`: "the alt isn't on the
 * snapshot") and both call sites label the image themselves. Should the media alt ever become
 * paintable, it belongs on the SHARED face so all 7 apps get it at once, not in a local walker.
 */
export function mediaImageHash(doc: RenderDocument | undefined): string | null {
  if (!doc || !doc.blocks) return null;
  return renderDocumentFirstImageHash(doc);
}

/**
 * The nest-relative path a post's single `post-image` slot paints when it is a bridged picture —
 * the first `ProxiedImage` of a post with no blob image (render-model.md § D6c; the precedence of
 * `RenderDocument::proxied_post_image`), or null. The post-detail pane's one image reads it; the
 * list card paints every media block.
 */
export function proxiedPostImage(doc: RenderDocument | undefined): { path: string; alt: string } | null {
  if (!doc || !doc.blocks || mediaImageHash(doc)) return null;
  return renderDocumentProxiedImages(doc)[0] ?? null;
}

/**
 * Every trusted media block (`Image` / `Video` / `ProxiedImage` / `ProxiedVideo`) the shared fold emitted, in body
 * order.
 *
 * This is the accessor that RETIRED web's second decode: `PostCard` used to call `decodePost`
 * itself and branch on `decoded.items[].media_type` in app code, because the shared fold
 * discarded the media type and no document block said "video" (render-model.md § Implementation
 * status today). The branch is now made once, in shared Rust, for all 7 apps — and web keeps
 * rendering EVERY attachment, which is the richest existing pattern (priority #4) and why the
 * shared fold is multi-item rather than first-only.
 */
export function documentMediaBlocks(doc: RenderDocument | undefined): RenderBlock[] {
  if (!doc || !doc.blocks) return [];
  return renderDocumentMediaBlocks(doc);
}

/**
 * The urls of this document's link previews still `Resolving` (render-model.md § D4), in body
 * order — the shared producer appends one below each standalone bare-url paragraph, and the page
 * fires `manager.resolveLinkPreview(url)` for each (re-emitting it `Resolved`/`Failed`).
 *
 * Delegates to the shared `RenderDocument::resolving_link_preview_urls` over the
 * `renderDocumentResolvingLinkPreviewUrls` wasm boundary. Fire-once by construction: a resolved
 * block no longer yields its url, so the caller's dedup set is belt-and-braces, not the guard.
 */
export function resolvingLinkPreviewUrls(doc: RenderDocument | undefined): string[] {
  if (!doc || !doc.blocks) return [];
  return renderDocumentResolvingLinkPreviewUrls(doc);
}

/**
 * This document's `Resolved` link previews (render-model.md § D4), in body order — one
 * `link-preview-card` per entry. `Resolving`/`Failed` yield nothing (the producer left the inline
 * body link in place, so the URL already shows); the card image is an async blob URL the `{@html}`
 * body can't produce, so the reactive Svelte template paints it.
 *
 * Delegates to the shared `RenderDocument::resolved_link_previews` over the
 * `renderDocumentResolvedLinkPreviews` wasm boundary, which returns the flat card payload — so no
 * client re-derives `Resolved` out of `PreviewState` in its own language (web had that match
 * written twice, once `any`-typed).
 */
export function resolvedLinkPreviews(doc: RenderDocument | undefined): ResolvedLinkPreview[] {
  if (!doc || !doc.blocks) return [];
  return renderDocumentResolvedLinkPreviews(doc);
}

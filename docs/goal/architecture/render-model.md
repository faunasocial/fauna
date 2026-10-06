# Render model — the shared semantic document every app paints

Owns: render-model
Status: ratified
Authority: cross-page body render model (`RenderDocument` + deltas D1–D7) and the shared-Rust/app-glue render boundary; page-level UX → conversations.md / feed.md / html-mail.md; element IDs → ui.yaml.

**Delta index:** D1 conversations body → `document` · D2 embeds as blocks (+ D2b
reply-quote) · D3 manager-owned remote-image reveal · D4 link previews · D5
`SourceGlyph` rail/badge token · D6 feed body + embed-fold (+ D6b typed/multi-item
media fold, IN PROGRESS — one app leg still owed: windows; + D6c bridged media as
`ProxiedImage`, BUILT on tui 2026-10-02, macos + ios 2026-10-03 and linux + web + android 2026-10-05, windows trickles down; a bridged video as
`ProxiedVideo` RULED 2026-10-02, BUILT on tui 2026-10-03 and linux + web + android 2026-10-05, three apps trickle down; inline playback of `Video` +
`ProxiedVideo` RULED 2026-10-02 — § D6c → *Inline playback* — both nest routes BUILT 2026-10-02, the projection's `Video` arms + web's player 2026-10-03, the rest pending) · D7 task lists +
nested-list emission. **All seven original deltas (D1–D7) are BUILT on all 7 apps,
on both the feed and the conversations surface** (per-delta detail in
§ Implementation status today).
**The embed-projection consolidation is COMPLETE on all 7 apps as of 2026-08-02** —
the four UniFFI/wasm faces landed 2026-07-21; web + android adopted them 2026-07-31,
apple 2026-08-02, windows 2026-08-02 (last). The text floor (the
`fauna_core::markdown` block model + HTML/plaintext producers + remote-image
blocking) shipped 2026-06-13 via [html-mail.md](../behavior/html-mail.md).

**Authority (scope detail).** This doc owns the *cross-page body render model*: how a message body
(`docs/goal/ui/conversations.md`) or a post body (`docs/goal/ui/feed.md`) becomes the
structured, semantic node tree a native shell renders, and where the line falls
between shared Rust (the model + decisions) and app glue (native widget
construction). It is **more specific than, and reconciles**, the body-rendering prose
in `html-mail.md`, `conversations.md`, and `feed.md` — those docs keep their
page-level concerns and defer the render-model mechanism here.

## Goal

One shared, semantic **`RenderDocument`** — produced in shared Rust, carried in the
page snapshot, painted by each app into native widgets — is the single way a body
is rendered on every app and every page. No app re-parses, re-formats, or
re-derives body structure; no app holds body-render *state* (remote-image reveal,
link-preview resolution) of its own. This is priorities #1/#2/#3 applied to the one
surface still partly per-app: rich body content.

## Why (the pre-D1/D6 state this model replaced)

Before D1/D6 landed, the body seam was inconsistent, and the inconsistency was where
per-app logic re-accreted:

- **Conversations** handed the shell `MessageSnapshot.body: String` + `body_format`,
  and each app called `fauna_core::markdown::parse_markdown` (or the `FfiMd*` /
  `markdownToHtml*` face) **itself, at render time**, then painted the blocks. The
  parse was shared; the *invocation, the remote-image reveal state, and the
  embed-vs-text interleaving* were per-app. (The sibling `body_format` field is
  gone — the discriminant now lives inside the producer, D1.)
- **Feed** was a step behind: `PostSummary.body` was a **raw `String`** (the nest's
  500-char FTS preview), with quoted posts (`quoted_post_id`) and media
  (`media_hash`) as **sibling snapshot fields**, not nodes in the body. The body text
  itself had no structure at all.

So the same five "how do I turn a body into something renderable" decisions were made
in five-to-six places, and new rich content (link previews, inline attachments,
quoted messages) had no shared home to land in. Naming one model fixed the drift and
gives future rich-content work a single place to grow (priority #4: resolve drift,
don't add a seventh surface); the deltas below are what closed the gaps.

## The model

`RenderDocument` is an ordered list of semantic **blocks**; text blocks carry inline
**spans**; embeds are **first-class blocks**, not sibling fields. It is the typed
generalization of today's stringly-typed `fauna_core::markdown::MdBlock { kind:
String, … }` — the same block→line→span granularity that already ships, widened to
cover embeds and made a snapshot citizen.

```text
RenderDocument = Vec<RenderBlock>

RenderBlock =
  // —— text (today's MdBlock kinds, typed) ——
  | Paragraph(Vec<Inline>)
  | Heading { level: u8, inlines: Vec<Inline> }
  | ListBlock { ordered: bool, items: Vec<Vec<RenderBlock>> } // not `List` — a `List` enum variant shadows kotlin.collections.List in the UniFFI Kotlin binding
  | CodeBlock { lang: Option<String>, text: String }
  | BlockQuote(Vec<RenderBlock>)
  | TaskList { items: Vec<TaskItem> }  // GFM task list (D7a); TaskItem { checked: bool, blocks: Vec<RenderBlock> }
  // —— embeds (the new, first-class part) ——
  | Image       { hash: String, alt: String }              // trusted, already-fetched media
  | Video       { hash: String, alt: String }               // sibling of Image (D6b, built 2026-08-15)
  | ProxiedImage { path: String, alt: String }             // a bridged post's attachment served by the reader's OWN nest at a nest-relative proxied path (D6c, per-app build state in the status table; paints immediately, no reveal — user-ruled 2026-09-30)
  | ProxiedVideo { path: String, alt: String }             // its video twin, sibling of Video (D6c → Proxied video, per-app build state in the status table; the video-thumbnail slot, never byte-loaded)
  | RemoteImage { url: String, alt: String, revealed: bool } // ← reveal flag from the MANAGER
  | Attachment  { blob_hash, filename, mime_type, size_bytes, is_image, c2pa }
  | LinkPreview { url: String, state: PreviewState }        // async-resolved by the manager
  | QuotedMessage { author_display: String, snippet: String, … }  // reply-quote (conversations)
  | QuotedPost    { post_id, author, body, …, legal_takedown_ref, not_found } // quote-post (feed); the two tombstone states, ui/feed.md § Post deletion

Inline =
  | Text(String) | Bold(Vec<Inline>) | Italic(Vec<Inline>) | Code(String)
  | Link { href: String, inlines: Vec<Inline> }
  | Mention { actor: String, display: String } // NOT YET BUILT — no producer emits it; markdown has no mention syntax today (render.rs's "Not yet modelled")

PreviewState = Resolving
             | Resolved { title, description, image_hash: Option<String>, revealed: bool } // revealed = the D4 og:image reveal gate, the D3 twin
             | Failed
```

The exact field set is the implementing slice's to finalize against the current
`MdSpan`/`AttachmentSnapshot`/`QuotedPostView` shapes (it must be a strict superset of
what those already carry — priority #4: pick the richest existing pattern). What is
load-bearing is the **shape discipline**: typed block variants, embeds as blocks,
stateful nodes whose state is owned by the manager.

## Where it lives

**`fauna-core`**, alongside the prior art it generalizes — `fauna_core::markdown`
(the block model + `parse_markdown` + `render_html_with` + `markdown_to_plaintext` +
`RemoteImageMode`) and `fauna_core::structured` (the structured-post `structured_view`
projection). Both `fauna-conversations` and `fauna-feed` already depend on
`fauna-core` with `default-features = false`, and `fauna-core/uniffi` already exists
(it registers `LocalizedText` cross-crate for the feed/devices snapshots). **No new
crate** — adding one would fragment the presentation model the codebase has already
decided lives in `fauna-core` (priorities #3/#4). `markdown` becomes one **producer**
of a `RenderDocument` (markdown source → document); plaintext and the inbound
HTML→markdown path (html-mail.md) are the others.

Exposure is unchanged from every other snapshot type: `uniffi::Record` for
Apple/Android/Windows/Linux, `serde` for web via `fauna-wasm`.

## What's already shared (the floor — do not rebuild)

Per html-mail.md (shipped, all 7 apps) and feed.md (ratified), a large part of the
render model already exists and **must be reused, not reimplemented**:

- **The text block model** — `fauna_core::markdown::{MdBlock, MdLine, MdSpan}` +
  `parse_markdown`, the `FfiMd*` UniFFI twin, and the `markdownToHtml*` /
  `decorationMap` / `wrapMarkdownSelection` wasm faces. Headings/bold/italic/lists/
  links/code/blockquote/remote-image are all already modeled and painted on every
  app.
- **Remote-image *classification* and *blocking posture*** — `MdSpan.image_url`,
  `RemoteImageMode::{Blocked, Fetch}`, `count_remote_images`, the
  `load-remote-content-button` (html-mail.md § Rendering / § Security & privacy).
  Untrusted inbound bodies render `Blocked`; user-authored/followed content renders
  `Fetch`. **This posture is unchanged by this doc.**
- **Badge / source classification** — `fauna_feed::classify_sources() → SourceKind`,
  where "Apps keep only their genuinely platform-specific `SourceGlyph → asset` map
  (emoji on every app … the concept stays in Rust)" keyed off the shared `glyph`
  concept (feed.md § Where logic lives). This is the **template** the rail-icon delta copies.
- **Structured-post field projection** — `fauna_core::structured::structured_view`
  (`Article`/`Community`/`Classified`/`LiveActivity`).
- **Sender/participant display** — `TypedAddress::display` / `typed_address_display`
  (conversations.md § Where logic lives), already shared across all 7 apps.

## Deltas (what this doc newly ratifies)

Each delta closes a place where body-render logic or state is still per-app.

### D1 — Body becomes a structured `RenderDocument` in the snapshot

`MessageSnapshot.body: String` (+ `body_format`) → `MessageSnapshot.document:
RenderDocument`, produced once by the manager. Apps walk the document and paint;
no app calls `parse_markdown` at render time. The `body_format` discriminant moves
inside the producer (markdown vs plaintext vs already-converted inbound HTML).

### D2 — Embeds are first-class blocks, not sibling fields

Attachments, quoted messages (conversations reply-quote), quoted posts (feed
quote-post), and inline images render as `RenderBlock` nodes **in body order**,
instead of being appended by per-app glue after the text. The existing
`AttachmentSnapshot` / `QuotedPostView` / `QuotedMessage` data is projected into these
nodes; their resolution paths (the `attachment_bytes` loader, the shared quoted-post
projection, `resolve_media`) are unchanged — only their *placement* moves into the
document.

### D3 — Remote-image reveal state moves into the manager *(refines html-mail.md)*

Today each app keeps its own per-message `revealedRemote` / `remoteLoaded`
dictionary; html-mail.md § Rendering specified the reveal as **"render-time only (no
persistence, no new storage)."** This doc keeps the **no-persistence** invariant but
moves *where the state lives*: the manager holds an in-memory per-message reveal set,
`RemoteImage.revealed` is projected from it, and `manager.reveal_remote_images(message_id)`
flips it and re-emits the snapshot (the `load-remote-content-button` becomes a manager
dispatch like every other gesture). This deletes the 6 per-app dictionaries and puts
the **privacy-sensitive "when does untrusted content phone home" decision in one
audited place** (priorities #1/#2; the security-review surface html-mail.md § Security
already flags). It **supersedes** html-mail.md's *location* choice only — the posture
(blocked-by-default, per-message opt-in, no persistence, `Fetch` only for
user-authored/followed content) is unchanged. html-mail.md § Rendering is updated to
point here. ✅ **User-approved 2026-06-22** (it refines a user-ratified shipped
decision) and **DONE on all 7 apps' feed surface** (windows landed last of the
original six; tui built it into feed at parity), and **symmetric
across conversations bubbles AND feed post cards/detail on all 7 apps** since
2026-07-30, when tui's `conversation_detail` closed the last gap (see
§ Implementation status today).
A bridged post's own attachment served through the reader's nest (`ProxiedImage`, § D6c)
is **not** a `RemoteImage` — it never phones home from the device — and does NOT obey
this reveal: it paints immediately like a native `post-image` (D6c's posture, user-ruled 2026-09-30).

### D4 — Link previews — a new async-resolved embed node

`LinkPreview { url, state }` is a new `RenderBlock`. The manager resolves preview
metadata off-thread and re-emits with `state: Resolved { … }` — the **same
lazy-resolve-then-re-emit pattern feed already uses** for `media_hash` and the
quoted-post fallback (feed.md § The read model). New capability, established
mechanism. Blocked-by-default applies to the preview image exactly as for any
`RemoteImage` (D3 posture).

**Design ratified 2026-06-26 (user-approved — the gated decisions are settled):**

- **Producer location = nest-side.** The preview metadata (`og:title` / `og:description`
  / `og:image`) is fetched **by the nest**, not the client — a new authenticated WS-RPC
  kind **`fauna.linkpreview.resolve { url } -> ` a reply that maps to
  `PreviewState::Resolved { title, description, image_hash } | Failed`** (the nest fetches
  the URL's OpenGraph/meta, caches by URL, and — if it keeps the og:image — stores it as a
  content-addressed blob whose hash becomes `image_hash`, so the client resolves the image
  through its existing `RemoteImage`/media path). *Why nest-side:* a client-side fetch leaks
  every user's IP to every linked site **and** is CORS-blocked on web (so web could not
  render previews at all — a priority-#1 per-app divergence). The nest owns the
  SSRF/abuse guards (http(s)-only, block private/link-local IPs, size + time caps), mirroring
  the existing `media_proxy_routes` Bluesky-media proxy posture
  (that proxy is the *unauthenticated* media-byte sibling; this
  resolve kind is **authenticated**). **Owned by dedicated nest-side work** — the canonical
  "nest adds the kind before the client consumes it" hand-off.
- **Producer (bare-URL detection, shared Rust).** The body producer (`fauna_core::render`)
  emits a `LinkPreview { url, state: Resolving }` block for a **standalone paragraph that is a
  single bare URL** (an `Inline::Link` whose visible text equals its `href`, alone in its
  paragraph). The inline link itself **stays** in the paragraph; the preview is an *additional*
  block rendered below it (so an app that doesn't paint the card still shows the link).
- **Manager resolution (app glue, per-platform).** For each `LinkPreview` in `Resolving`,
  the client's render manager calls `fauna.linkpreview.resolve(url)` off-thread and re-emits
  the block `Resolved`/`Failed` (lazy-resolve→rebuild-document — the `media_hash` / quoted-post
  pattern, where the async byte load stays client-side).
- **Card (`ui.yaml`, user-approved 2026-06-26 — forward-pointer, added with the implementing
  commit).** Component **`link-preview-card`** (clickable → opens `url`) with children
  **`link-preview-image`** (the og:image — **blocked-by-default per D3**; see the reveal note
  below), **`link-preview-title`**, **`link-preview-description`** (truncated),
  **`link-preview-domain`** (the host, via the shared `fauna_core::format::url_host`).
  **States:** `Resolved` → the full card; `Resolving` → **no card** (the inline body link
  always stays in the paragraph and already shows the URL — a skeleton would be a
  perpetual-loading state if a resolve never completes, strictly worse than the visible link;
  this supersedes the original "skeleton placeholder" shape, matching the shipped web+linux
  reference `LinkPreviewCard.svelte` + `PostCard.svelte`); `Failed` → fall back to the plain
  inline link (no card). Same IDs on all 7 apps. Because `Resolving` and `Failed` paint the
  same thing, the page alone cannot say which one it is showing: `RenderDocument::link_previews`
  answers every preview with its state (`PreviewState::name` — `resolving`/`resolved`/`failed`),
  and an app's e2e state dump publishes it as `data.feed.posts[].link_previews` so a test can
  wait for `failed` before it reads "no card" (tui, linux, web, macos, ios and windows publish it — web over
  the wasm `renderDocumentLinkPreviews` face, macos, ios and windows over the FFI
  `render_document_link_previews` face; android adds it with its witness of the failed
  state, over the same FFI face).
- **og:image reveal gate (user-ratified 2026-06-27).** The og:image is a content-addressed
  blob the **nest** fetched and stored, so the client only ever loads it from its **own** nest
  (`/api/v1/blob/<hash>`) — painting it never phones home to a third party, the concern D3
  exists for. The user nonetheless ratified that the og:image **obeys the post's D3
  remote-content reveal** (blocked-by-default, same posture as any `RemoteImage`): the card
  paints title/description/domain immediately and the og:image **only after** the post's
  existing `load-remote-content-button` is tapped. **Mechanism (shared Rust, not per-app
  glue):** `PreviewState::Resolved` carries a `revealed` flag the manager projects from the
  per-post reveal set (the exact twin of `RemoteImage.revealed`); it drives
  `RenderDocument::has_blocked_remote_images` (so a post whose only remote content is the
  og:image still surfaces the one reveal button) and flips on `reveal_remote_images`. One
  button reveals all of a post's remote content (body images + og:image) together. **NOT on the
  wire** — a client-side render-model projection, like D3's reveal flag.

Rollout (across multiple work tracks, captured internally — the shape is greenfield-on-top-of-the-landed-model
but the new `RenderBlock` variant breaks the exhaustive app walkers, so it lands with all
walkers updated in the same change): **(1)** shared Rust — the
`RenderBlock::LinkPreview` variant (`PreviewState` already exists) + the producer bare-URL
rule + the `fauna.linkpreview.resolve` wire type; **(2)** nest —
the resolve handler + OG-fetcher + cache + SSRF guards (nest-side work); **(3)** per-app —
the card rendering + the manager resolve-call (each app), with the `ui.yaml` element added
on first app implementation.

### D5 — Rail-icon semantic token (`SourceGlyph`)

The 6 duplicated `rail_glyph` / `protocolSymbol` / `ProtocolGlyphFor` switches
(`+page.svelte`, linux `list.rs` **and** `thread_header.rs`, windows
`ConversationsPage.xaml.cs`, android `ConversationListScreen.kt`, apple
`ConversationsHelpers.swift`) collapse to the feed model: shared Rust owns the
canonical **concept** each source-protocol icon depicts, and — since every app
renders that concept as the same emoji — the `concept → emoji` map too (the map
was originally left per-app; see *All seven apps are emoji apps* below for why
that premise did not survive). Forcing the concept into Rust is a
priority-#1 fix — the semantics had silently diverged (apple mapped
FaunaMls→lock / Bluesky→cloud while the others used a fox / butterfly emoji, and
web even split fox in the rail vs leaf in the feed badge for the *same* source).

The token is **`fauna_core::source_glyph::SourceGlyph`** — `{ Fox, Envelope,
Butterfly, Bolt, Globe, Unknown, Archive, Bridge }`, **named for the concept, not the source**, so
the brand decision lives in one Rust mapping and a re-brand is a deliberate
change that ripples to every app's exhaustive match. Both
`fauna_conversations::Rail::glyph()` and `fauna_feed::SourceKind::glyph()` resolve
to it, so the conversations rail and the feed badge share **one** per-app
asset map (which also kills the within-app rail-vs-badge split). The
conversations snapshot (`ThreadSummary` / `ThreadDetail`) carries a precomputed
`glyph: SourceGlyph` — web reads the lowercase serde string off the snapshot;
native apps match the enum. On a `Rail::Bridged` thread that field is the glyph
the thread's bridge declared, one of this set (`ui/conversations.md` § Where
logic lives → *The `Bridged` adapter*, ruling 2 (a)).

**Canonical concept per source (ratified by the user 2026-06-22):** Fauna → Fox
(the project's animal brand), Bluesky → Butterfly (Bluesky's brand mark),
Fediverse (ActivityPub / Mastodon / …) → Globe (the *generic* fediverse, not the
Mastodon elephant, since the source is broader than Mastodon), Nostr → Bolt,
Email → Envelope, an archive import (a Facebook or Instagram export re-authored
by its owner — behavior/archive-import.md § What each category becomes) →
Archive (a box; both platforms share the concept and the badge label names the
platform; added 2026-09-06 with the ratified archive-import design, appended
after Unknown so existing FFI discriminants stay put), a bridge whose identity
is not to hand → Bridge (a bridge; added 2026-10-02 with the `Bridged` adapter's
first slice, appended after Archive for the same reason, and a glyph a bridge's
manifest may declare), anything else → Unknown (a generic broadcast/antenna
concept).
**All seven apps are emoji apps, so the MAP is shared too, not just the
concept** (2026-08-22). Every app independently arrived at the same six
glyphs — neither SF Symbols nor Segoe Fluent has a fox or a butterfly, and one
emoji beside five native symbols in a single rail column reads inconsistently, so
apple and windows both chose emoji-for-all rather than bundling brand art. What
D5 originally left per-app was therefore not a *native asset* map at all but
seven copies of one table, and they had begun to disagree: apple and windows had
each independently fixed their envelope to the emoji-presentation form while
linux / tui / web / android still shipped the bare codepoint (and tui carried two
copies of its own, one per surface). The map now lives with the concept, in
**`fauna_core::source_glyph::SourceGlyph::emoji()`** — the same lift
`notification_glyph::NotificationGlyph::emoji()` already made for the
Notifications row icon. A future move to bundled brand art stays a deliberate
change, now to one function rather than seven copies.

**Canonical bytes:** 🦊 / ✉️ / 🦋 / ⚡ / 🌐 / 📡 / 📦 / 🌉. **The envelope carries VS16**
(U+2709 + U+FE0F): U+2709 is `Emoji_Presentation=No`, so the bare codepoint
defaults to *text* presentation — monochrome, and single-width in a terminal cell
grid beside the other five glyphs' double width. The selector is what renders it
as the color, full-size emoji the other five already are by default; it is a
property of the codepoint, not of any one platform, which is why apple and
windows both needed it and the four bare copies were drift rather than a
deliberate per-toolkit choice. `fauna_core::source_glyph` pins the value for
every app at once.

Each app keeps only the **call**: linux `source_glyph_emoji`, tui `.emoji()` at
its rail and badge sites, web over wasm (`sourceGlyphEmoji`), android over UniFFI
(`com.fauna.ffi.sourceGlyphEmoji`), apple `ConversationsUI.glyphEmoji` and
windows `SourceGlyphAsset.Emoji`. Within each app the ONE map still serves both
the conversations rail and the feed badge, so a within-client rail-vs-badge split
stays impossible.

### D6 — Feed body adopts the same `RenderDocument`

Feed's raw-string body (list-card preview + `fauna.posts.get` full body) is projected
into a `RenderDocument`, with quoted-post and media folded into `QuotedPost` / `Image`
nodes (D2). Feed's nest-computed order/search/dedup discipline (feed.md § The read
model, § Anti-patterns — search is a re-query, the app never re-sorts/re-filters)
is **untouched**: this delta is about body *structure*, not list assembly.

### D7 — Task lists + nested-list emission *(designed 2026-06-27; built 2026-06-28)*

Two additive, **substrate-agnostic** render-side changes that close the last gaps for rich list rendering on
*every* markdown-painting surface (conversations, feed, mail) — and are the render foundation the **Notes**
editor consumes (design ratified 2026-06-27, tracked internally, which ratifies this delta
in the same commit; `docs/goal/ui/spaces-documents.md` § the document editor).

- **D7a — a task-list / checkbox `RenderBlock` variant.** Pre-D7, no checkbox variant existed (render.rs:73-185).
  Add a **new sibling variant** (not a mutation of `ListBlock`), recommended shape (field set finalized at build
  per § The model): `TaskList { items: Vec<TaskItem> }`, `TaskItem { checked: bool, blocks: Vec<RenderBlock> }`
  — items are sub-documents exactly like `ListBlock`, so nesting and mixed bullet/checkbox lists compose. Named
  `TaskList` (collision-free in all four bindings, unlike `List` → `ListBlock`, render.rs:83-86). Being a *new
  variant* it breaks the **exhaustive** app walkers (linux/android/apple) at **compile time** — the safety
  net — while leaving every existing `ListBlock` construction + paint **untouched** (far smaller blast radius
  than retyping `ListBlock.items`). Land the variant + all 6 app walkers in one coordinated commit (android
  needs host-`.so` bindgen regen first; windows C# switch has no `default`; web TS switch needs the case added;
  `just wasm` after the fauna-core change; Go binding regen if a mail-render path walks it).
- **D7b — `parse_markdown` emits nested lists + task items.** Pre-D7, `parse_markdown` (markdown.rs:182-347) was
  **flat** (list lines accumulated into one block via `flush_items`; `parse_list_item` captured no indent and no
  `- [ ]` syntax). Upgrade: capture per-item indent **depth** and recognize `- [ ]`/`- [x]`, then build the
  nested `ListBlock`/`TaskList` tree in the typed `markdown_to_document` mapping (an indentation-stack fold) —
  **not** by retyping `MdBlock.kind: String` (render.rs:11-18 — that ripples into all seven apps' shipped
  html-mail path). **Additive: default depth 0 keeps every non-nested input byte-identical** in the shipped
  `MdBlock`/html-mail path (§ What's already shared — the floor). Go/no-go: property-tested `blocks → markdown →
  blocks` round-trip identity (Proof 1 of the Fork-2 validation spec).

## The boundary (guardrail — a model, never a widget tree)

`RenderDocument` carries **structure + semantic role + content + state**. It MUST NOT
carry layout, colors, fonts, sizes, spacing, or a box/view tree. The membership test
for any field: *would all 7 apps compute the same value, and is it about **what** to
show rather than **how** it looks?* "How" stays native. This is the line that keeps
native accessibility, automation IDs (`tests/e2e-unified/ui.yaml`), and platform feel
intact — the reason the project chose native shells over a cross-platform UI toolkit.
The shell's whole job is `match block { … }` → construct the native widget.

**"Admits no player" means exactly this (ruled 2026-10-02, § D6c → *Inline playback*):** no player node and no playback state (position, paused, volume) in the model — the platform's native player and its transient state are app glue; what is shared is the *decision of what plays* (the `FeedManager::playback_source` projection) and the `state` attribute the `video-thumbnail` element publishes for the drivers.

## Where logic lives

| Concern | Shared Rust (`fauna-core` + the page manager) | App glue |
|---|---|---|
| Parse/convert body → `RenderDocument` | ✅ producer (markdown / plaintext / inbound-HTML) | — |
| Block/inline/embed structure & order | ✅ the document | — |
| Remote-image reveal state (D3) | ✅ manager-owned, in-memory | fire the dispatch on button tap |
| Link-preview resolution (D4) | ✅ manager async-resolve + re-emit | — |
| Rail/source → icon concept (D5) | ✅ `SourceGlyph` (`Rail::glyph` / `SourceKind::glyph`) **and its emoji** (`SourceGlyph::emoji`) | the call site only — every app renders the same emoji |
| Painting nodes into widgets | — | ✅ GTK / WinUI / SwiftUI / Compose / Svelte |
| Splitting a text block into text-widget-sized line runs | ✅ `inline_line_runs` / `text_line_runs` (≤ `MAX_LINES_PER_TEXT_RUN` lines per run) — a projection; the document is untouched | paint one text widget per run, lazily when there are several; a shell whose text layout is linear never calls it |
| Which source a video plays from — public blob URL / ticketed proxy URL / a sealed blob to open client-side (§ D6c → *Inline playback*) | ✅ `FeedManager::playback_source` (+ the `fauna.media.playback_ticket` mint) | hand it to the platform's native player in the `video-thumbnail` slot; own the player's transient state; publish `state` on the element |
| Layout, styling, lightbox, file picker | — | ✅ native |

## Implementation status today

**Every delta (D1–D7) is BUILT on all 7 apps, on BOTH the feed and the
conversations surface, as of 2026-07-30** — tui's `conversation_detail` bubble was
the last gap on three deltas and closed them in order: D2 (`dm-attachment-*`) and
then D3 (`load-remote-content-button`) + D4 (`link-preview-card`) together, the
latter pair because one reveal button gates a body remote image and a resolved
preview's og:image alike. **The embed-projection consolidation leg is now CLOSED on all
7 apps** (the four UniFFI/wasm faces landed 2026-07-21 — web + android adopted them
2026-07-31, apple 2026-08-02, windows 2026-08-02 as the last app, below). The many
session narratives this section used to carry are compressed to the matrix + notes
+ dated provenance below;
the delta prose above is authoritative for behavior, the work-commits and TODOs for
build history.

| Delta | State | Apps | Key proof / anchor |
|---|---|---|---|
| Floor — text block model (html-mail) | shipped 2026-06-13 | all 7 | [html-mail.md](../behavior/html-mail.md) |
| P1 — model + producers (`fauna_core::render`) | landed | shared | uniffi + wasm faces; `to_plaintext` golden parity vs `markdown_to_plaintext` |
| D1 — conversations body → `document` | DONE | all 7 | sibling `body_format` removed; apple read-path e2e green macos+ios 2026-06-23 |
| D2 — `Attachment` embed blocks | DONE | all 7 (feed + conversations) | sibling `attachments` removed; read projection `attachment_blocks(&document)`; tui's `dm-attachment-image`/`dm-attachment-file` landed 2026-07-30, the last app |
| D2b — `QuotedMessage` reply-quote | DONE | all 7 | `dm-message-quote` (ui.yaml); tier_3 `test_conversations_reply_quote.py` green web/linux/windows; apple card in `DmMessageBubble.swift`; tui paints it in `conversation_detail` too |
| D3 — manager-owned remote-image reveal | DONE | all 7 (feed + conversations) | shared `set_remote_images_revealed`/`has_blocked_remote_images`; windows last of the original six; tui's `conversation_detail` `load-remote-content-button` landed 2026-07-30, the last app — tier_3 `test_conversations_remote_image.py` (web/windows/tui) |
| D4 — link previews (feed + conversations) | COMPLETE | all 7 (feed + conversations) | nest `fauna.linkpreview.resolve` handler; tier_2 `test_feed_link_preview.py` (web/linux/windows) + `test_conversations_link_preview.py` (web/linux/tui); tui's `conversation_detail` `link-preview-card` landed 2026-07-30, the last app |
| D5 — `SourceGlyph` rail + feed badge | DONE | all 7 | ONE shared `SourceGlyph::emoji` map (2026-08-22; was seven per-app copies that had drifted on the envelope's VS16); `fauna-core` `source_glyph::tests`; `ConversationsGlyphTests` / `SourceGlyphAssetTests` |
| D6 — feed body + embed-fold | DONE | all 7 | bespoke flat feed renderers deleted; fire-once triggers + idempotent re-emit |
| D7 — task lists + nested-list emission | BUILT 2026-06-28 | all 7 walkers compile-confirmed | `blocks → markdown → blocks` round-trip property test (Proof 1) |
| D6b — typed + multi-item media fold (`Video` sibling) | shared BUILT 2026-08-15; walkers all 7; **paints on all 7 apps** | `RenderBlock::Video`; `resolve_media_folds_a_video_block_for_a_video_attachment` + `…_folds_every_attachment_in_body_order` + `a_later_quote_resolve_does_not_collapse_multi_item_media` (tier_1); tier_3 `test_feed_video_thumbnail.py` green tui + web + linux + windows + macos + ios (real simulator), compile-verified android |
| D6c — bridged media as `ProxiedImage` (nest-served sibling of `Image`) | BUILT 2026-10-02: the variant + `proxied_images()` (UniFFI / wasm faces), the `fauna-feed` fold (an absolute `https://` `remote_url` rewritten defensively; `media_hash` `Some("")` for an all-remote post), `shared_media_proxy_url` in `fauna_core::data`, ActivityPub ingest storing the proxied path; reveal posture user-ruled 2026-09-30 (paints immediately) | tui (`post-image` in list card + post detail, bearer-fetched through `Op::FetchImage`'s `paths`); macos + ios (FaunaKit `FeedVM.documentPostImage` → `proxiedPostImage`: the bearer-carrying `APIClient.get` into a decoded image, the placeholder's automation text the path; the list card and both post details; no C2PA badge); linux (`build_post_proxied_image`: the bearer-carrying content `get` of the path, the texture cached by path, the automation text the path; list card + post detail), web (`ProxiedImage.svelte`: an authenticated `fetchNestPath` → object URL, the path as the placeholder's text until the bytes land, the block's `alt` on the image; list card + post detail) and android (`FeedPostImage` → `ApiClient.fetchNestPathBytes`, the path as the placeholder's text; compile-verified) since 2026-10-05, the slot precedence the shared `RenderDocument::proxied_post_image`; windows carries a stub walker arm and paints nothing for it until its lift | § D6c below; bridges.md § Unified feed ingestion ruling 4 (the URL form) |
| D6c-video — a bridged video as `ProxiedVideo` (nest-served sibling of `Video`) | RULED 2026-10-02; BUILT on tui 2026-10-03: the variant + `proxied_videos()` (UniFFI / wasm faces), the `fauna-feed` fold (a zero-hash `video/*` item → `ProxiedVideo` through the same `proxied_media_path` rewrite; audio still folds to nothing), tui's `video-thumbnail` paint (`▶ {path}`, list card + post detail, no byte load) | tier_1 `a_bridged_video_folds_to_a_proxied_video_never_the_origin` + `proxied_videos_recurse_paint_ungated_and_ride_media_blocks`; tui unit `a_bridged_proxied_video_paints_its_path_in_video_thumbnail_and_fetches_nothing`; linux / web / android paint `▶` + the path in `video-thumbnail` since 2026-10-05 (web: `VideoThumbnail`'s poster-less frame, no `src`), the slot precedence the shared `RenderDocument::proxied_post_video`; windows / macos / ios carry a stub walker arm and paint nothing for it until their lift | § D6c below, *Proxied video* |
| D6c-play — inline playback of `Video` + `ProxiedVideo` (the platform's native player off the shared `playback_source`; `Range` + streaming + the 100 MiB cap + the playback ticket on the routes) | RULED 2026-10-02; proxy streaming + `Range` + cap + ticket BUILT 2026-10-02 (`media_proxy_routes.rs`, `media_ticket.rs`, the `fauna.media.playback_ticket` kind); blob-route `Range` BUILT 2026-10-02 (`http_range.rs`, the one parser the segment route also calls; `BlobStoreBackend::get_range`); the projection's `Video` arms + web's player BUILT 2026-10-03 (`FeedManager::playback_source` → `Url` / `Sealed` / `Unplayable`, the UniFFI + wasm `playbackSource` faces, web's `VideoThumbnail.svelte` publishing `data-state`); macos + ios' player BUILT 2026-10-03 for the `Video` arms (one FaunaKit view: `VideoThumbnailView` hosting `AVPlayer` in a `VideoPlayer`, driven by `InlineVideoPlayer`, the sealed arm through `FeedVM.videoPlaybackURL` to an owner-only temp file; publishes `state` + `position` + `source`) — FaunaKit unit tests and marked tier_3 legs exist for it; the projection's ticket arm pending (the `ProxiedVideo` variant it needed BUILT 2026-10-03, D6c-video); every other player UNBUILT | both nest routes (the blob route's hex branch ranged, its CID branch whole-body by design); the projection's `Video` arms; web's player (`test_feed_video_playback.py`); macos + ios' player (`InlineVideoPlaybackTests`) | macos + ios' tier_3 run; the ticket arm (`ProxiedVideo` folds since 2026-10-03; until the arm lands `playback_source` answers `Unplayable` for one), beside tui's OS-handoff leg, then linux + android, windows | § D6c below, *Inline playback* |

**D6b — the D6 media fold is TYPED and multi-item (gap root-caused 2026-07-30, closed
2026-08-15).** `Image { hash, alt }` carried no media type and the one place the shared
code *had* the type threw it away, so `video-thumbnail` was unbuildable from the document
on all 7 apps and web painted it only via a *second*, app-side `decode_post`. Now:

- **`RenderBlock::Video { hash, alt }` is a sibling of `Image`**, per the D7a recipe (a
  new variant, never a field on `Image`, so the exhaustive walkers break at compile time).
  It carries the same two fields and no more: § The boundary admits no player, and
  `MediaItem`'s `thumbnail`/`dimensions` are `None` from every writer we have
  (`media_item_from_staged`), so a poster field would be dead on arrival — additive later
  if a writer appears.
- **`media_blocks(&PostBody)` is the one place the image-vs-video branch is made**, and it
  folds **every** item in body order, not just `items.first()` (priority #4 — web's
  render-them-all was the richest existing pattern, and `ui.yaml`'s `image-grid` is
  specced for 1/2/4). This is what let web **retire its second decode**: `PostCard` now
  paints from `documentMediaBlocks(post.document)`.
- **The document is the authority for folded media, not a sibling field.** The rebuild
  paths (a quote or gated body resolving later) recover it via
  `RenderDocument::media_blocks()`; recovering it from the single-hash `PostSummary`
  `media_hash` would collapse a multi-item post back to one image on the next rebuild
  (pinned by `a_later_quote_resolve_does_not_collapse_multi_item_media`). `media_hash`
  itself is unchanged and stays exactly what it was — the fire-once resolve guard, and the
  e2e state field tui/linux/windows/apple publish.
- **Accessors:** `first_image_hash` / `first_video_hash` for the single-element painters
  (`post-image` / `video-thumbnail`, neither `indexed` in ui.yaml), `media_blocks` for the
  multi-item painters. They are deliberately separate: an app must never paint a video blob
  into an image element, which is the bug the typed variant makes unrepresentable.
  `first_video_hash` **recurses** into block quotes/list items/task items exactly like
  `first_image_hash` (§ above, "they recurse") — its initial 2026-08-15 landing was a
  top-level-only twin that missed a nested embed, the exact class of bug that whole
  recursing-accessor family exists to prevent; fixed 2026-08-26, found by a windows unit test.
- **Built on all 7 apps — D6b is closed.** tui led (lead-app rule), web adopted in the same
  commit because retiring its second decode *was* the fold's payoff; linux + android landed
  2026-08-16; macos + ios landed 2026-08-25 — new shared FaunaKit `VideoThumbnailView` + `documentMediaVideoHash` wrapper, wired
  into both targets' list card and post detail (4 call sites); windows landed 2026-08-26 — `DocumentRenderer.MediaVideoHash` twinning its
  existing `MediaImageHash` wrapper, painted at the feed list card + post-detail dialog (2
  call sites). None of the app legs has a poster frame to decode, so all mirror tui's
  play-glyph + hash text rather than inventing one.

**D6c — a bridged post's remote media is a `ProxiedImage`, the nest-served sibling of `Image` (ruled 2026-09-28, a design pass; BUILT on tui 2026-10-02, macos + ios 2026-10-03 and linux + web + android 2026-10-05, windows trickles down — the status table above; its reveal posture is user-ruled, below).** `fauna_core::data::MediaItem.remote_url` is written by two ingest paths (`ap_note_to_fauna_post`, `fauna_bridge_atproto::ingest::build_fauna_post`) and was read by no client; `media_blocks` / `first_media_hash` hex-encoded the zero `blob_hash` of such an item into `Image { hash: "00…0" }` and `media_hash = "00…0"`, so a bridged post asked every app for `/api/v1/blob/000…` — broken, not invisible. Four rulings close it; the URL form itself is [bridges.md](../behavior/bridges.md) § Unified feed ingestion → *Bridge ingestion* ruling 4's (a `remote_url` at rest and on the wire is always a nest-relative, already-proxied path; the shared rewrite lives in `fauna_core`).

- **The block is a new sibling variant, `RenderBlock::ProxiedImage { path: String, alt: String }`**, per the D7a/D6b recipe (a new variant, never a field on `Image`, so the exhaustive walkers break at compile time and the variant lands with all 7 walkers in one commit — the six trickle-down apps take a stub arm that paints the `post-image` placeholder until their lift; tui paints it first). `path` is nest-relative (`/api/v1/media/proxy?url=…` for ActivityPub and nostr, `/api/v1/bluesky/media?url=…` for Bluesky), never an absolute URL. **Not `RemoteImage`:** that variant's `url` is a third-party origin the reader's device dials itself, bearer-less, only after the D3 reveal, and its walkers, `remote_images()` and `has_blocked_remote_images` exist for exactly that phone-home class — a nest-served image folded into it would either need a per-app "is this url relative?" branch in seven fetchers or make every bridged post grow a reveal button. **Not `Image`:** its `hash` is a content address every app turns into `/api/v1/blob/<hash>`; a path is not a hash. **`media_blocks` stays the one place the branch is made:** a non-zero `blob_hash` folds as today; a zero `blob_hash` with a `remote_url` and an `image/*` type folds to `ProxiedImage` (an absolute `https://` `remote_url` it still meets — a row written before the ingest rewrite, a writer that forgets — goes through the same shared rewrite first, so the snapshot never carries a cross-origin URL; one that is neither nest-relative nor rewritable folds to nothing); a zero-hash `video/*` item folds to `ProxiedVideo` (RULED 2026-10-02 — the *Proxied video* bullet below; until that build lands it folds to nothing; bridges.md ruling 4 carries no Bluesky video, so an ActivityPub video attachment is the only writer today). The accessor family gains its sixth member, `proxied_images()` (+ the UniFFI / wasm faces and a `ProxiedImageRefOwned` mirror, the exact shape `remote_images()` took), and it recurses like its siblings. It paints in the `post-image` slot (and an app's `image-grid`), never `doc-remote-image` — no new ui.yaml ID.
- **Apps fetch a nest-relative path exactly as they fetch `/api/v1/blob/<hash>`: from their own nest, on the bulk plane, with the session bearer.** Native apps already do — `NestContentApi::get(path)` attaches the bearer, and every `post-image` loader (tui's `Op::FetchImage` and its six siblings) is that call with a `paths::blob::by_hash` argument; the proxied path is the same call with a different argument. Web, whose `post-image` is a plain `<img src>` (a subresource load cannot carry a bearer), fetches the bytes with its authenticated `fetch` and paints an object URL — its existing shape for a sealed attachment (`media-src.ts`'s `blob:` arm). Never `fauna_client::remote_image`'s bare third-party client. The same loader serves the bridged author's `avatar_url` (bridges.md → *Bridged authors*, ruling 1), which rides this lane and has no renderer yet. **Consequences:** `/api/v1/media/proxy` keeps its bearer requirement — the security-review control stands (only a registered actor drives an outbound fetch through the nest, and a minted URL is not a replayable fetch capability for a stranger); the `bluesky/media` route's premise that all 7 apps load it as a bearer-less subresource is retired by this ruling (its module comment is corrected in the build), and requiring the bearer there too — its CDN allowlist stays regardless — is a follow-on once every app's loader carries it. **Refuted alternatives:** HMAC-signed unauthenticated URLs (camo) turn every minted URL into an unexpiring bearer-less fetch capability for anyone who holds it, weakening a reviewed control for the sake of web's `<img src>`, which web does not need; fetch-and-store at ingest as a blob makes the nest a permanent mirror of every bridged image (storage, takedown exposure, fetches nobody views) where the proxy fetches on view only — a proxy-side cache is an optimization behind the same URL, never a different URL form; a `fauna.media.*` WS-RPC kind carrying bytes — bulk bytes never ride DAG-CBOR frames ([api-layers.md](api-layers.md) § HTTP residue).
- **`media_hash` stays the fire-once resolve guard, so it must never rest `None` after a resolve** — an app kicks `resolve_media` on `has_media && media_hash.is_none()` (tui `feed/mod.rs`, the manager's own `needs` guard), and a post whose media are all remote would otherwise re-fire a `fauna.posts.get` on every render pass. After a resolve `media_hash` is `Some(first blob hash)`, or `Some("")` for a post whose attachments carry no blob at all — the `tips` convention ([feed.md](../ui/feed.md) § State & data shape: a resolver writes `Some` on every outcome, and a truthful "resolved, nothing" is a default, never `None`). An app that still paints `post-image` from `media_hash` rather than the document treats the empty string as none; the document is the authority for folded media (D6b), so tui and web never read it for painting.
- **Reveal posture — RULED by the user 2026-09-30 (answer A): `ProxiedImage` takes the `post-image` posture — it paints immediately, carries no `revealed` flag and never counts in `has_blocked_remote_images`.** It is the author's own attachment, the bridged twin of a native post's `post-image` (which paints immediately), the reader's device never phones home (the nest fetches it, unattributed — the proxy sends no `User-Agent`), and the D4 og:image gate governs content the *linked site* chose, not the author's. The D3 blocked-by-default rule is therefore NOT generalised to "anything the nest fetched from beyond itself": a Bluesky or fediverse timeline never shows its pictures behind the `load-remote-content-button`, and `ProxiedImage` never joins `set_remote_images_revealed`, `remote_images()`'s reveal walk or `reveal_remote_images`.
- **TDD in `libs/fauna-feed` before any app code, three cases:** a bridged post whose item is a Bluesky relative `remote_url` folds to `ProxiedImage { path }` with that path; an ActivityPub absolute `https://` `remote_url` folds to the proxied `media/proxy` path and never the origin; a native blob post is unchanged (`Image`/`Video`, `media_hash` the hash); plus the guard: a resolved all-remote post reads `media_hash == Some("")` and a second `resolve_media` is a no-op.
- **Proxied video — `RenderBlock::ProxiedVideo { path: String, alt: String }`, the nest-served sibling of `Video` (RULED 2026-10-02, a design pass; BUILT on tui 2026-10-03 — shared Rust + tui in one commit; the six other apps trickle down, linux / web / android / windows batched with their `ProxiedImage` lift, macos + ios on a lift of their own).** A bridged post's video attachment reaches the fold today as a zero-`blob_hash` `video/*` `MediaItem` whose `remote_url` is the proxied path (`ap_note_to_fauna_post` stores it with the attachment's own `media_type`; Bluesky carries no video — bridges.md ruling 4), and the fold drops it. Three answers. **(1) Shape — a fourth variant completing the 2×2 (`Image` / `Video` by content hash, `ProxiedImage` / `ProxiedVideo` by nest-relative path), the D7a/D6b recipe applied a third time, with the same two fields as its three siblings and no more.** Not a field on `ProxiedImage` (an `is_video` flag or a `kind`): D6b's reason stands — an app must never paint a video into an image element, and the typed variant is what makes that unrepresentable; a flag would put the image-vs-video branch back into seven walkers. Not `Video` with an empty `hash`: a path is not a content address, and `hash` is what every app turns into `/api/v1/blob/<hash>`. Not a nested source enum (`Image { src: Hash | Path }`): it would rewrite every existing construction and paint site on 7 apps for one new arm, beside two sibling-variant precedents — drift, not design. No poster field (`MediaItem::thumbnail` is `None` from every writer, and a Mastodon `Document` attachment carries no preview — a PeerTube `icon` or a Bluesky HLS `thumbnail` is a writer that does not exist today, additive later exactly as `Video` ruled), no mime (the branch is made once, in the fold), no `revealed` (the `ProxiedImage` posture: the author's own attachment, fetched by the nest, paints immediately, never counted by `has_blocked_remote_images`). `media_blocks` stays the one branch point: its zero-hash arm folds `image/*` → `ProxiedImage` and `video/*` → `ProxiedVideo`, both through the same `proxied_media_path` rewrite; anything else (audio) still folds to nothing, as it does for a blob item — no app renders audio. `media_hash` and `has_media` are untouched (`Some("")` for an all-remote post, as above). The accessor family gains its seventh member, `proxied_videos()` (+ `ProxiedVideoRefOwned` and the UniFFI / wasm faces), the exact shape `proxied_images()` took, recursing like its siblings; `RenderDocument::media_blocks()` carries the variant for the multi-item painters. **(2) Paint — the `video-thumbnail` slot, no new ui.yaml ID, in each app's `Video` posture, and NO app byte-loads a proxied video for its thumbnail.** The six text-only legs paint the play glyph + the `path` where they paint the hash for a `Video` — the same addressable observable `ProxiedImage`'s placeholder label is (the bridged-post e2e reads the path out of `post-image`); the path is opaque to the app, never parsed for the remote host. tui takes the first `ProxiedVideo` when `first_video_hash()` is `None`, the precedence `proxied_post_image` uses, on the list card and post detail alike, not clickable (inline playback is tui's declared absence, [apps/tui.md](apps/tui.md) § Declared platform absences). The thumbnail's only legitimate byte need is a poster frame, which no writer supplies; web's native `<video preload="metadata">` first-frame grab is a free byte-range read from the blob route, and the proxied twin of it would be a full bearer fetch of up to the proxy cap per card — so web's `ProxiedVideo` thumbnail is the glyph over a poster-less frame until a poster writer or a player exists. **`alt` — on all four media variants — is the item's own `MediaItem.alt` (BUILT 2026-10-03):** `media_blocks` folds `item.alt` into the block, empty when the item has none; the field, its two bridge writers and its relation to the post-level `alt_text` are [bridges.md](../behavior/bridges.md) § Unified feed ingestion → *Bridge ingestion* ruling 4's. Every app's walker already carries the block's `alt`; an app that paints a fixed description on a media element paints the block's `alt` when it is non-empty and its fixed text otherwise (a leaf-component change, no new element — web's `Image` / `Video` arms do since the same build, its proxied arms since their `ProxiedImage` lift 2026-10-05). **(3) The proxy serves the block as-is, and the block lands with no nest change.** `media_proxy_routes`' content-type allowlist already passes `video/mp4`, `webm`, `ogg`, `quicktime`, `mp2t` and the HLS playlist types, which is all a thumbnail-less block needs. Its 50 MiB cap, its whole-body buffered read, its missing `Range` passthrough and its bearer requirement (a `<video src>` cannot carry one) are PLAYBACK concerns, and no app plays a video from any route today (§ The boundary admits no player; tui's absence; web's `<video>` is a thumbnail). They are owned by the inline-playback ruling, which rules `Video` (the blob route) and `ProxiedVideo` (the proxy) together — never a proxy-only patch. **TDD in `libs/fauna-feed` before any app code:** an absolute `https://` `video/mp4` `remote_url` folds to `ProxiedVideo` with the proxied path, never the origin; a nest-relative `video/*` path folds as-is; a zero-hash `audio/*` still folds to nothing; a native `Video` is unchanged; plus the tui paint pin — the label carries the path and kicks no byte fetch.
- **Inline playback — `Video` and `ProxiedVideo` play in the platform's NATIVE player, as app glue, from a source the shared `FeedManager::playback_source` projection hands the app (RULED 2026-10-02, a design pass; the proxy's streaming + `Range` + cap + ticket and the blob route's `Range` BUILT 2026-10-02; the projection's `Video` arms + web's player BUILT 2026-10-03 — web LEADS because inline AV is tui's declared absence; the projection's ticket arm is unbuilt (the `ProxiedVideo` variant it needed folds since 2026-10-03, and `playback_source` answers `Unplayable` for one until the arm lands); macos + ios' player BUILT 2026-10-03 for the `Video` arms, its tier_3 run still owed; the rest UNBUILT — beside tui's OS-handoff leg, then linux + android and windows).** Before this ruling no app played a video from any route: five apps painted `video-thumbnail` as an inert glyph + text, web's `<video preload="metadata">` had an unwired click; the blob route (`blob_routes.rs::download_blob`) answered whole bodies with no `Range` until its half of answer (2) landed; the proxy (`media_proxy_routes.rs`) buffered the upstream into one `Vec` under a 50 MiB cap, forwarded no `Range` and required a bearer until its half of answers (2)–(4) landed. Five answers. **(1) The player is app glue around the platform's own player; the model and § The boundary are untouched.** `<video controls>` (web), `AVPlayer` in SwiftUI's `VideoPlayer` (macos + ios, one FaunaKit view), the framework `VideoView`/`MediaPlayer` (android), `gtk::Video` (linux, GTK4's built-in GStreamer-backed widget), `MediaPlayerElement` (windows) — platform frameworks only, **zero new third-party dependencies** (a player library would need the user's per-install approval, and none is needed). Not a `playing` state in the model: the boundary's membership test fails — position, paused, volume are one view's transient interaction, not a value all 7 apps compute alike — and D3's reveal lives in the manager because it is a *privacy decision*, which playback is not. What IS shared is the decision of what plays: `FeedManager::playback_source(block) -> PlaybackSource` (async; UniFFI + wasm faces): `Url("/api/v1/blob/<hash>")` for a `Video` whose media is not sealed (the blob route is unauthenticated by design — answer 4); `Sealed { hash }` for a `Video` on a gated post (`is_sealed_media`, the arm web's `mediaUrl` already takes for a sealed image: the app opens the blob through the existing sealed-media path to an object URL or a hardened temp file and plays THAT — whole blob, no `Range`, the tui handoff's "whole file in memory, acceptable for a first cut"); `Url(ticketed proxied path)` for a `ProxiedVideo` (answer 4); `Unplayable` otherwise. **Activation: the `video-thumbnail` element IS the player host** — tap/Enter swaps the glyph for the native player in the same slot (no new ui.yaml ID; rule A untouched), and the element publishes `state` ∈ `idle` / `loading` / `playing` / `error` (the `post-image` `painted`/`placeholder` attribute precedent, read by the drivers' `get_attr`; the vocabulary is documented in `tests/e2e-unified/drivers/base.py`) — the headless observable every tier_3 player test asserts, with the position advancing; the native player's own error surface is the error UI, no new element. **Never autoplay, on any app:** the tap is what spends the bytes — which is also why no data-budget knob exists (answer 3). tui's leg is the OS external handoff, owned by [apps/tui.md](apps/tui.md) § External media handoff → *Feed video*: the same mode gate and hardened temp file as the Media page, fed by three byte paths (public blob GET, sealed open, bearer GET of the proxied path — tui needs no ticket, it is not a `src=` fetch); its `ask` confirm reusing the `media-external-open-*` IDs on the feed page is the ONE ui.yaml scope question this ruling leaves to the user. **(2) Byte-range and streaming on both routes.** Blob route, hex branch only: `Accept-Ranges: bytes`; a single `bytes=a-b` / `a-` / `-n` → 206 + `Content-Range`, unsatisfiable → 416, multi-range → the whole body (browsers never send one); the parser is lifted from `segment_route.rs`'s private `parse_range` into one shared nest module both routes call (priority #4), and `BlobStoreBackend` gains `get_range(hash, start, end)` with a default over `get` (`DiskBlobStore` seeks, `S3BlobStore` passes the `Range` header) so a 99 MB video is never read whole per request; the CID branch keeps whole-body verified reads — a range of unverified bytes defeats what it is for; a sealed blob's range is served like any other (harmless, useless, never asked for). WebKit refuses a `<video>` whose server answers no byte-range request and seeking needs it everywhere, which is why the blob row gates the web row. Proxy: the body STREAMS (`CappedBody::next_chunk` → `Body::from_stream`) for every type, never a `Vec` — the one memory question gone; the client's `Range` is forwarded upstream verbatim and the upstream's `206`/`Content-Range`/`Content-Length`/`Accept-Ranges` relayed; the SSRF guard is untouched (one dial, redirects off, pinned addresses); the 30 s timeout becomes connect + per-chunk, never whole-body. **HLS playback is unsupported** through the proxy — a playlist's segment URLs would be dialed by the player directly, bypassing it (the playlist types stay allowlisted for the inert-bytes reason only); playlist rewriting is a follow-on the day a writer exists (Bluesky video — bridges.md ruling 4 carries none). **(3) The cap is a Rust constant, 100 MiB, enforced on the DECLARED total before the first body byte.** Mastodon's default video upload is 99 MB (its posting guide, read 2026-10-02: "Videos … up to 99MB", transcoded to H.264 at ≤1300 kbps), so 50 MiB refused half of what the commonest peer accepts. Nobody chooses the cap — it bounds this nest's egress per request against an abusive upstream, a resource bound and not a preference, so it is bucket 1 (`principles.md` § One configuration surface: a user would never want to pick it, and there is no operator); the phone's data budget is answered by never autoplaying (the tap spends the bytes) and the native player's own cellular behaviour, so no knob exists there either. Enforcement: a 200 whose `Content-Length`, or a 206 whose `Content-Range` total, exceeds the cap → `502 upstream content too large` before streaming; absent length → the capped stream cuts at the cap (today's behaviour, streamed). One constant for all types; images are unaffected in practice. Not a per-type cap, not a per-user quota: both would be knobs nobody wants. **(4) Auth for a `src=` fetch — a nest-minted PLAYBACK TICKET on the proxy URL, the same on all 7 apps; the blob route needs none.** The blob route is unauthenticated by design (the hash is the capability; the inert-type allowlist + `nosniff` close the navigable-XSS vector — `build_blob_response`'s comment), so a public `Video` is just `<video src="/api/v1/blob/<hash>">`. The proxy keeps its bearer (images; tui's fetches) and gains a second credential: `&exp=<unix>&sig=<base64url HMAC-SHA256>` over `(url, exp)` under a purpose-bound nest secret — `media_ticket_secret`, minted at boot like the OAuth refresh-token secret, sealed under the deployment seed, satellite-registered; [key-material-hierarchy.md](key-material-hierarchy.md) § Audience: deployment infrastructure → *Media playback-ticket secret* owns its custody — with a 1 h TTL (a constant: it covers a feature-length pause; a leaked ticket fetches one public fediverse video through one nest for an hour, nothing else; expiry mid-play surfaces as the player's error, and the next tap re-mints). The ticket is minted by the new WS-RPC kind `fauna.media.playback_ticket` (Read class, User allowlist — the client's one authenticated channel), consumed inside `playback_source`, so no app ever builds the URL by hand. Why not the alternatives: a bearer header — `<video src>` cannot carry one, and native players taking one would diverge from web (priority #1); a cookie — web-only (#1); a bearer-less "known-reference" form (serve only URLs an ingested post references) — an unauthenticated oracle over which posts this nest ingested, i.e. its users' follow set, plus open egress amplification; caching the remote video into the blob store — third-party content at rest inside the user's quota, when the proxy exists for IP privacy only. The ticket never widens the unauthenticated sink: a forged, expired or absent credential is the same 401 a missing bearer gets, and the bluesky twin's bearer-less state stays its own documented follow-on. **(5) Poster frames — none; the thumbnail stays glyph + text.** No writer exists; the nest never derives one (it cannot for a proxied video, and for a public blob it would mean a video-decoder dependency on the nest for a cosmetic); the uploader's own platform is the natural writer when one appears (additive `MediaItem::thumbnail`, which `?thumb=1` already serves — not minted: no user has asked, and the player makes the first frame visible on tap). With `Range` on the blob route, web's `<video preload="metadata">` first-frame grab becomes the cheap partial read it pretends to be today; a `ProxiedVideo` thumbnail still byte-loads nothing before the tap (the *Proxied video* bullet's answer 2). **TDD before any app code:** tier_1 nest — the blob `Range` round-trip (206/416/whole body/the CID branch ignores `Range`/451 holds) and the proxy's ticket mint → GET 200, expired/forged → 401, bearer still accepted, `Range` relayed as 206, declared size over cap → 502 before a body byte, headers before the last chunk (streaming); tier_1 shared — `playback_source` on each arm; tier_3 per app — activation reaches `state=playing` and the position advances, on a VP9 `.webm` for web and linux (Playwright's Chromium ships no H.264) and the H.264 `.mp4` for apple and windows (AVFoundation plays no WebM), a per-app fixture map beside the test.

**Load-bearing mechanics (the facts a consuming session needs):**

- **Body-source discipline (D1/D6):** conversations `body` and feed `PostSummary.body`
  are **retained as the canonical text sources** — the manager builds `document` from
  them; the thread-list snippet (a bounded preview — [conversations.md](../ui/conversations.md)
  § State & data shape owns its derivation) and feed `quote::project_from_loaded` read them. The removed siblings are conversations
  `body_format` (consumed inside the producer) and `attachments` only. Feed
  `media_hash` / `quoted_post_id` remain as the lazy-resolve **fold inputs**
  (`resolve_media` / `resolve_quoted_post`) — resolution state, not dead render
  siblings.
- **Reveal-gate predicate single-sourced via a UniFFI face (2026-07-02):**
  `RenderDocument::has_blocked_remote_images` is exported as
  `render_document_has_blocked_remote_images` (`libs/fauna-ffi/src/render.rs`, gated
  default-on `render`, forwarding `fauna-core/uniffi`, dropped from the Go
  `--no-default-features` build — the sibling of `render_document_to_plaintext`), so a
  app gates the `load-remote-content-button` on the ONE shared predicate instead of
  re-walking the block tree per app (a hand-rolled twin silently misses new
  blocked-content arms — the D4 og:image, the D7a task-list recursion — and nested
  embeds). **windows** consumes it (`DocumentRenderer.HasBlockedRemoteImage` → FFI;
  its local top-level-only fold is gone), **web** consumes it via the
  `renderDocumentHasBlockedRemoteImages` wasm export (hand-rolled
  `blockHasBlockedRemoteImage` TS recursion deleted), **android** consumes it
  (2026-07-02, tracked internally — `DocumentText.kt` delegates to
  `com.fauna.ffi.renderDocumentHasBlockedRemoteImages`; the
  `detects_a_blocked_remote_image_at_any_depth` fauna-ffi test owns the contract);
  **linux** and **tui** call `RenderDocument::has_blocked_remote_images` directly
  (Rust-native, no FFI). **apple consumes it too (2026-07-16)** — `hasBlockedRemoteImages` /
  `blockHasBlockedRemoteImage` (`FaunaKit/Sources/FaunaKit/Views/DocumentBodyView.swift`)
  deleted; the public `hasBlockedRemoteImages(_:)` wrapper (unchanged signature, so no
  call site moved) now forwards straight to `renderDocumentHasBlockedRemoteImages`. **All
  7 apps now consume the shared predicate.**
- **The four embed projections are single-sourced too (2026-07-12) — the same fix, same
  rationale, applied to the rest of the family.** Beside the reveal predicate, every app
  had *also* hand-rolled the four pure projections a page needs to paint the folded embeds
  the walker deliberately leaves inert (§ D6 — the walker has no blob loader, so the *page*
  extracts): `first_image_hash` (the `post-image` media hash), `quoted_post` /
  `has_quoted_post` (the `quoted-post` card + the fire-once `resolve_quoted_post` guard),
  `resolving_link_preview_urls` (the fire-once `resolve_link_preview` trigger) and
  `resolved_link_previews` (the `link-preview-card` data). Four twins, four chances to miss
  an arm. They are now methods on `RenderDocument` (`libs/fauna-core/src/render.rs`),
  returning the borrowed views `QuotedPostEmbed<'_>` / `ResolvedLinkPreview<'_>`, and — like
  `has_blocked_remote_images` and unlike every twin they replace — **they recurse** into
  block quotes, list items and task items, so an embed nested inside a quote or list is
  found rather than silently skipped. **linux and tui consume them** (both Rust-native, no
  FFI); linux's local `views/document.rs` twins + its private `ResolvedLinkPreview` struct
  are deleted. **The UniFFI/wasm faces landed 2026-07-21** (`render_document_first_image_hash`
  / `render_document_quoted_post` / `render_document_resolving_link_preview_urls` /
  `render_document_resolved_link_previews`, `libs/fauna-ffi/src/render.rs` + the matching
  `libs/fauna-wasm/src/lib.rs` faces — the exact shape `has_blocked_remote_images` took in
  2026-07-02). **Web adopted them 2026-07-31** — `$lib/document`'s `quotedPostBlock` /
  `mediaImageHash` (was `mediaImageBlock`) / `resolvingLinkPreviewUrls` + `resolvedLinkPreviews`
  (were one `linkPreviewBlocks`, whose `PreviewState` match each of its 4 call sites re-derived in
  TS) now delegate over the wasm faces, and web's `QuotedPostPayload`-shaped local walkers are
  gone. Web therefore also *receives* `authoring_origin` on the quoted embed, and now paints it too
  — `delegated-origin-badge` (tui-led 2026-07-31) landed on web 2026-08-15, one leg of the six-app
  trickle-down owned by [atproto-pds-full.md](../behavior/atproto-pds-full.md) § Problem 1 → D10 →
  Audit. In the same pass `fauna-feed`'s private
  `document_has_quoted_post` was deduped onto the shared `RenderDocument::has_quoted_post`.
  **Android adopted them the same day** — `DocumentText.kt`'s `documentQuotedPost` /
  `documentMediaImageHash` (was `documentMediaImage`) / `documentResolvedLinkPreviews` /
  `documentResolvingLinkPreviewUrls` delegate to the UniFFI faces, and its local
  `ResolvedLinkPreview` mirror is deleted in favour of the generated
  `uniffi.fauna_core.ResolvedLinkPreviewOwned`. Android likewise receives `authoringOrigin`
  and paints it too — `delegated-origin-badge` landed on android 2026-08-15, alongside web.
  **Apple adopted them 2026-08-02** — `DocumentBodyView.swift`'s `documentQuotedPost` /
  `documentMediaImageHash` (was `documentMediaImage`) / `documentResolvedLinkPreviews` (was
  `documentLinkPreviews`) / `resolvingLinkPreviewUrls` now delegate to the UniFFI faces, joining the
  `hasBlockedRemoteImages` half that landed in apple. Apple's local mirrors are gone: the
  quoted-post extractor returns the generated `QuotedPostEmbedOwned` instead of a hand-rolled Swift
  tuple, and the preview extractor returns `[ResolvedLinkPreviewOwned]` instead of every block with
  its raw `PreviewState` — which had made each of its five call sites (macOS card + detail, iOS card
  + detail, the conversation bubble) re-derive the same `case .resolved` match, the identical
  per-call-site re-derivation web deleted. Apple likewise *receives* `authoringOrigin` on the quoted
  embed; it paints `delegated-origin-badge` from it too (2026-07-31, same day as tui) — linux and
  android joined 2026-08-15 and **windows 2026-08-24, the last of the seven**, so the badge
  (focal card, detail pane and quoted embed) is now painted everywhere it is owed.
  **windows adopted the four faces 2026-08-02 as the LAST app, closing this consolidation.**
  `DocumentRenderer`'s twins are gone: `QuotedPost` returns the generated `QuotedPostEmbedOwned`,
  `MediaImage` became `MediaImageHash` over `first_image_hash` (the web/apple `…Hash` naming), and
  the single `LinkPreview` extractor split into `ResolvedLinkPreviews` +
  `ResolvingLinkPreviewUrls` — which deleted the `PreviewState.Resolved` match from
  `LinkPreviewCardModel`, the same per-call-site re-derivation web and apple deleted.
  ⚠ **The real gap this swap was required to fix rather than preserve is fixed:**
  `DocumentRenderer.LinkPreview` used to return only the FIRST preview via `FirstOrDefault`,
  where `resolved_link_previews` returns the full list. windows now paints **one card per
  resolved preview** on both surfaces — the feed post card through an `ItemsControl` over
  `FeedPostItem.LinkPreviewCards` (which replaced eight flat per-post card properties), the DM
  bubble by clear-and-rebuild into a `LinkPreviewCards` panel like `AttachmentsList` — and fires
  the resolve trigger for **every** `Resolving` url, where the old single-preview twin stranded
  the second one in `Resolving` forever. Pinned by tests that fail against the deleted twins:
  `LinkPreviewCardModelTests.TwoBareUrls_YieldACardEach_InBodyOrder` +
  `PreviewNestedInABlockQuote_IsStillFound`, `DocumentRenderTests`' two
  `…_IsFoundWhenNestedInABlockQuote` cases, and
  `FeedPostItemTests.TwoBareUrls_ProjectACardEach_AndContentEqualsSeesTheSecond` (which also
  pins that the refresh diff notices a change confined to the second card — the flat
  comparisons could not).
  ✅ **The fleet-wide follow-on this surfaced is RULED (2026-08-13, ui.yaml's domain):**
  `link-preview-card` is **`indexed: true`** — N cards under the one bare id, in body order,
  addressed positionally (`post-card[i]/link-preview-card[n]`), with the four children resolving
  *within* their own card. Scope + rationale live in ui.yaml's `link_preview_card` component
  block; the measurement behind it is that all 7 apps already paint N off the shared
  `resolved_link_previews()` projection. The one app that had to conform was **tui**, which
  painted the children as flat siblings of the card rather than inside it (the other six nest);
  it now scopes them `.within("link-preview-card", n)` on both surfaces. Pinned end-to-end by
  `test_feed_link_preview.py::test_two_bare_urls_paint_a_card_each`.
  **A fifth joined the family 2026-08-01: `remote_images()`** — every `RenderBlock::RemoteImage`
  in body order, each with its own manager-projected `revealed` flag (borrowed `RemoteImageRef<'_>`),
  recursing exactly like its four siblings. It is the
  *itemised* counterpart of `has_blocked_remote_images`, which answers only whether to show the
  one reveal button: an app painting the images themselves needs the list, and until now had to
  re-walk the block tree to get it — the same twin-per-app shape this bullet exists to stop.
  **tui consumes it** (Rust-native, no FFI) for `doc-remote-image`. **The UniFFI/wasm faces landed
  2026-08-01** — `render_document_remote_images` (`libs/fauna-ffi/src/render.rs` + the matching
  `libs/fauna-wasm/src/lib.rs` face, the exact shape `render_document_resolved_link_previews` took), returning the owned mirror `RemoteImageRefOwned` that landed with it
  (`libs/fauna-core/src/render.rs`, beside `QuotedPostEmbedOwned` / `ResolvedLinkPreviewOwned`), the
  tracked Go binding regenerated in the same commit. **web and android adopted it the same day
  (2026-08-01).** web needed no separate extraction call: its `documentToHtml` walker was already
  the ONE unified pass (unlike quoted_post/resolved_link_previews, which had to be pulled out into
  discrete Svelte components), so `renderImage()` (`apps/fauna-web/src/lib/document.ts`) just grew
  the `doc-remote-image` test id on both the blocked and revealed `<img>` templates it already
  emits — no wasm-face call needed for rendering, though the face exists for any future consumer
  that needs the itemised list. android's `documentRemoteImages` (`DocumentText.kt`) now delegates
  to `com.fauna.ffi.renderDocumentRemoteImages` exactly like its four siblings, replacing the
  hand-rolled recursive Kotlin twin (`collectRemoteImages`) — the same per-app omission vector
  this whole family exists to close, now closed for the fifth member on the two apps buildable from
  the primary dev machine. ~~**OPEN — windows and apple**, which need their own machines~~
  ("adopt `doc-remote-image` on the other 6 apps").
  **CORRECTED 2026-08-10:** windows and apple have both since landed it
  (`DocumentPainter.cs:98` sets the `doc-remote-image` AutomationId; `DocumentBodyView.swift:171`
  the accessibilityIdentifier), so that OPEN is spent. **linux landed it the same day** —
  `views/document.rs`'s `RenderBlock::RemoteImage` arm now tags both `build_remote_image`
  (the revealed picture) and `build_blocked_image_placeholder` (the blocked state) with
  `crate::testid::set_test_id(..., "doc-remote-image")`, the same per-arm idiom `dm-attachment-image`
  already uses in this file. All 7 apps now paint it. Found by `lint-ui-elements.py` the day it
  learned to walk `sub_pages` — `doc-remote-image`'s only page scope is `feed.post_detail`, so no
  gate had ever checked it on any app.
- **D4 og:image reveal gate — user-ratified SETTLED 2026-06-27:** the prior
  gate-vs-direct cross-app #1 divergence (web rendered the og:image directly,
  windows deferred to gate it) is resolved — the og:image **obeys the post's D3
  remote-content reveal on all 7** (blocked-by-default, even though it is a
  nest-served content-addressed blob with no third-party fetch; tui inherits the
  gate for free via the shared Rust mechanism, built at parity; § D4 og:image reveal
  note). The mechanism is **shared, not per-app glue:** `PreviewState::Resolved`
  carries a `revealed` flag the manager projects from the per-post / per-message
  reveal set (the D3 twin); `has_blocked_remote_images` + `set_remote_images_revealed`
  count/flip the Resolved-with-image preview, so the post's one
  `load-remote-content-button` reveals body images + og:image together; `snapshot()`
  folds the preview state **before** the reveal walk. **web FLIPPED to gated** with
  the settlement.
- **Resolve-trigger loop safety:** `FeedManager::resolve_link_preview` /
  `ConversationsManager::resolve_link_preview` (and `resolve_media` /
  `resolve_quoted_post`) re-emit **idempotently** — fold + notify only when the block
  isn't already carried — so every app trigger discipline settles without loops:
  linux/android fire-once guards, windows' un-guarded observer-driven resolves, web's
  observer-driven `refreshFeed` (subscribed through `WasmFeedManager::subscribe` since 2026-09-24).
- **Test-inject seams:** feed `feed_inject_posts` `link_preview` spec;
  `ConversationsManager::seed_resolved_link_preview_for_test` (wasm
  `seedResolvedLinkPreviewForTest`, linux `conversations_seed_resolved_link_preview`
  test-agent command, python `conversations.seed_resolved_link_preview`). windows
  (card landed), android, and apple add their conversations e2e mark once
  their inject-command leg lands.
- **apple read-path uniformity (2026-06-22/23; windows `dm-message-text` seam 2026-09-15):**
  the `dm-message-text` / `feed-post-text` / `feed-post-detail-body` automation reads derive
  the painted *document* text — on **windows** `dm-message-text` sits on
  `Controls/DocumentBodyView`, whose custom `AutomationPeer` answers the UIA Value pattern
  with `render_document_to_plaintext` over the painted document (the bridge's `GetText` tries
  Value before scraping descendant `TextBlock`s), so the read is the document however many
  widgets the body is painted across; windows' two feed reads still scrape the one painted
  widget, which yields the same text only because each paints its whole document into it —
  a feed change that splits a body owes the same seam (`DocumentBodyView`'s peer, or
  `MarkdownRichEditBox`'s, is the in-app prior art) — elsewhere via `render_document_to_plaintext`
  (apple closures
  swapped; the DM test-inject `bodyFormat` rail-derived, Leg 1; the feed state push serializes `renderDocumentToPlaintext`, Leg 2);
  `test_feed.py::test_post_body_renders_markdown` GREEN on macos AND ios (2026-06-23,
  tracked internally).
- **Text-block line runs — a paint projection, not model state (2026-09-11).** A native
  text widget can lay out a string super-linearly in its hard line breaks, and the
  plaintext producer legitimately emits a paragraph with tens of thousands of them (a
  plain-text mail with no blank line). Profiled on apple: SwiftUI sizes a `Text` through
  `NSStringDrawing`, whose CoreText typesetter re-shapes from every break onward —
  quadratic in the break count, so one ~40k-line paragraph froze the whole app
  ([mail-message-size.md](../behavior/mail-message-size.md) § Implementation status
  today owns the diagnosis). The shared `fauna_core::render::inline_line_runs` (and
  `text_line_runs` for a `CodeBlock`) splits a text block at its hard breaks into runs of
  at most `MAX_LINES_PER_TEXT_RUN` (16) lines — lossless, each cut consuming the one
  `\n` it falls on, styling re-opened across a cut, and a block within the budget
  returned as exactly one run — so the budget and the tree split live once, in Rust.
  UniFFI faces `render_inline_line_runs` / `render_text_line_runs`; no wasm face (web's
  text layout is linear). Consumers: **apple** — `DocumentBodyView`'s `LineRunsView`
  paints one run as its single `Text`, several as a `Text` per run in a `LazyVStack`
  (only on-screen runs lay out; a drag-selection spans one run). **windows**: ADOPTED AS
  VIRTUALIZATION (2026-09-15) — not apple's class (at a fixed ~1.54 MB, cutting the hard
  breaks from 20 000 to 20 made the open *slower*, and the run→`Inline` mapping costs 0–1 ms;
  measured 2026-09-11), but a single fresh layout of a multi-megabyte `TextBlock` in the live
  tree measured tens of seconds (2026-09-13): WinUI's cost is the text handed to ONE widget.
  So the projection's value on windows is that only the on-screen runs lay out.
  `Controls/DocumentBodyView` paints a body with no block over the budget as one `TextBlock`
  — byte-identical to the old whole-document paint, so every ordinary message paints as
  before — and a body with a split block as one `TextBlock` per run in an `ItemsRepeater`
  (`StackLayout`), realized inside the enclosing `ScrollViewer`'s effective viewport;
  `FaunaApp.Core`'s `DocumentRenderer.Segments` is the walk (one segment per top-level body
  block, the shared UniFFI faces splitting a `Paragraph`/`CodeBlock` over budget), and the
  `dm-message-text` read moved onto the control's own Value pattern first (the *apple
  read-path uniformity* bullet above). Measured through the over-frame test: the ~3 MiB
  mail's bubble is there when the thread-open step returns (as on macos), the tick that
  paints it costing ~0.7 s — half of it marshalling the 3 MB document across UniFFI — where
  the whole-document paint stalled the UI thread for tens of seconds
  (mail-message-size.md § Implementation status today owns the numbers). **A shell's messages
  list is keyed by message id** — an observer tick re-binds each bubble in place and
  reconciles order, never clears and rebuilds, so a body's layout is paid once, not per tick:
  web (`{#each … (msg.message_id)}`), android (`key = { it.messageId }`), windows
  (`ConversationsPage.RefreshDetailView` + `ReconcileChildren`, 2026-09-15), linux
  (`ConversationDetail::render`'s `bubbles_by_id`/`reconcile_children`, mirroring windows'
  shape, 2026-09-15); apple (`ThreadDetailView.swift`'s messages `ForEach`, keyed by
  `\.messageId`, 2026-09-16).
  **linux**: ADOPTED AS LAZY LABELS (2026-09-21) — the 2026-09-13 exclusion rested on a
  masked measurement: one `gtk::Label` over the same ~40,332-line body stalls the UI
  thread for over a minute inside Pango's layout, apple's super-linear class
  (mail-message-size.md § Implementation status today owns both numbers and why the
  first one was wrong). `views/document.rs` paints a text block within the budget as the
  one label it always was, and a block the shared projection splits as one label per run
  in `views/line_runs.rs`: GTK has no virtualizing container that nests inside another
  scroller, so every run's label exists up front, empty and reserved at the measured
  first run's height, and takes its markup only once it comes within a page of the
  enclosing `ScrolledWindow`'s viewport (a painted run stays painted; a drag-selection
  spans one run). Because an off-screen run holds no text, a body with a split block
  declares its `dm-message-text` / `feed-post-text` / `feed-post-detail-body` read from
  the document (`to_plaintext`), windows' seam in the *apple read-path uniformity* bullet
  above; every other body keeps its label-inferred read.
  **android**: unmeasured. **web / tui**: not needed. A shell that proves
  super-linear adopts this projection, never its own split — and one that measures
  linear says so here rather than adopting it on the strength of painting one widget.

**Dated provenance (compressed; the work-commits + TODOs carry the narratives):**
P1 model + producers (`RenderDocument`/`RenderBlock`/`Inline`/`PreviewState`, layered
over `MdBlock` — `MdBlock.kind` stays a `String`, shipped html-mail path
byte-identical; remote `![]()` promoted to blocked `RemoteImage` blocks). D1/D2 all 6
(apple legs last; DM inject + feed state-push gaps fixed Legs 1+2; e2e confirmed
2026-06-23 — the iOS feed state-read extension flipped 4 iOS feed reds green). D2b
web/linux/android (user-approved 2026-06-23) → windows 2026-06-24 (tracked internally;
`test_conversations_reply_quote.py --client windows` 3/3; snippet = parent body via
`markdown_to_plaintext`, prepended block, hidden when parent unloaded, excluded from
`to_plaintext`) → apple (shared FaunaKit card). D3 shared managers +
web/linux/android/apple 2026-06-22 → windows; in-memory only, per-message
opt-in, no persistence (html-mail.md posture unchanged). D4: design ratified
2026-06-26 → shared variant + producer bare-URL rule + `fauna.linkpreview.resolve`
wire type → nest handler (SSRF guards + OG parse,
og:image as public content-addressed blob via `GET /api/v1/blob/<hash>`, 1 h URL
cache, per-actor rate limit; tier_1 ×24 + tier_3 `test_link_preview.py`) → shared `FeedManager::resolve_link_preview` +
`fauna-client-linkpreview` + web card 2026-06-27 → windows text card / trigger /
og:image legs (tracked internally, 2026-06-27; card states: Resolved → full
card, Resolving → **no card**, Failed → plain link) → linux + android cards + shared
`revealed` flag 2026-06-27/28 → apple feed + bubble cards 2026-06-27 (shared FaunaKit
`LinkPreviewCard`, `documentLinkPreviews` extractor) → android bubble card 2026-06-28
→ conversations tier_2 e2e web+linux 2026-06-27. D5 rail + feed badge all 6
(emoji-for-all incl. windows `SourceGlyphAsset.Emoji` + apple
`ConversationsUI.glyphEmoji`; VS16 envelope U+2709+U+FE0F; `FfiSourceBadge`/
`fauna_wasm::SourceBadge` carry precomputed `glyph`, gated default-on `feed-badge`).
D6 body: linux/web/android → windows (`DocumentPainter` extraction + `FeedPostBodyBind`)
→ apple; embed-fold mechanism (`build_post_document`; body → quoted post → media) →
the linux/web/android paints (linux shared walker + `build_post_image`; web extractors +
`QuotedPost.svelte` reuse; android extractors + new `ApiClient.fetchBlobBytes` blob
loader) → windows (extractors + `Flatten` exclusions; `dotnet test` 504/504) → apple
last. Bubble timestamp unified 2026-06-23 via
`fauna_core::format::conversation_timestamp_display` behind the new
`dm-message-timestamp` element (linux naive-UTC tz bug fixed; web/android added it;
apple carries the id — `DmMessageBubble.swift:305`; windows landed the full leg —
`DmMessageBubble.xaml:162`; tui direct-calls it too, `conversations/mod.rs:2452`) —
**DONE on all 7**; owner:
[value-formatting.md](../behavior/value-formatting.md) § Conversation timestamp. D7
built 2026-06-28 (D7a `TaskList`/`TaskItem` sibling variant + all internal recursions;
D7b additive `depth`/`task` on `MdLine`, indentation-stack fold, `[ ]` stripped into
`TaskItem.checked`, non-nested input byte-identical; `document_to_markdown` +
round-trip property test; apple walker compile confirmed 2026-06-28 `just swift-test`
138/138, windows confirmed 2026-06-29 `dotnet test` 765/765).

**Reconciliation record:** html-mail.md § Rendering (D3 location supersession —
reconciled 2026-06-22), feed.md § State & data shape (D6), conversations.md § State &
data shape + § Where logic lives (D1/D2/D3 — reconciled 2026-06-22).

## Phasing

Shared-Rust-first (priority #2 ordering), conversations-before-feed (conversations is
furthest along and carries the reveal-state pain). The phased breakdown (tracked internally) is:
P1 model + producers in `fauna-core` (no app touched); P2 conversations adopts
(D1/D2/D3) + drift mop-up (D5, bubble timestamp, search); P3 feed adopts (D6);
P4 link previews (D4, a feature on top).

## Cross-references / reading list

- [html-mail.md](../behavior/html-mail.md) — markdown-as-interchange, the shipped text
  render foundation, remote-image blocking posture (the floor D1–D3 build on).
- [conversations.md](../ui/conversations.md) — § State & data shape, § Where logic
  lives (the conversations consumer; D1/D2/D3/D5).
- [feed.md](../ui/feed.md) — § The read model, § Post content types, § Where logic
  lives (`SourceKind` prior art for D5; D6 consumer).
- `libs/fauna-core/src/markdown.rs`, `libs/fauna-core/src/structured.rs` — the model's
  home and prior art.
- (design ratified 2026-05-10; tracked internally) — full conversations Rust
  signatures.

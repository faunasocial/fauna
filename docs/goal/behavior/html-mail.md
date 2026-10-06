# HTML email — markdown as the interchange format

Owns: html-mail
Status: ratified
Authority: HTML mail as markdown-interchange — the HTML↔markdown converter contract (sanitization, scheme-gating, no-auto-fetch), inbound Markdown stamping + best-part selection, outbound multipart/alternative serialization, the supports_markdown capability flip, and the blocked-by-default remote-image posture for inbound bodies; defers the reveal-state model (D3) + RenderDocument to architecture/render-model.md § Deltas, compose-field inline styling to ui/conversations.md, MIME/relay context to behavior/smtp-server.md, element IDs to tests/e2e-unified/ui.yaml.

Ratified 2026-06-08 (user). Email is treated as rich content, but **markdown is
the single interchange format in both directions**: inbound HTML is converted to
markdown and rendered through each app's existing markdown path; outbound mail
is composed in markdown and serialized to `multipart/alternative` (markdown→HTML +
a plain-text part). **The feature was complete on the original 6 apps by
2026-06-13**; tui built the same render + compose during its own conversations
work (2026-07-13 onward), reaching parity on text formatting and the compose
toolbar, and its remote-image reveal control landed on `conversation_detail`
2026-07-30, closing the last per-app gap. See § Implementation status today
for the exact per-app state.

## Why

Email is HTML in the real world. Before this design landed (pre-2026-06), an
inbound HTML email showed its raw markup (`<p>…</p>`, tags and all) because every
app rendered `BodyFormat::Html` as raw text — the code hard-coded
`BodyFormat::PlainText` inbound, built `text/plain`-only outbound, and gated the
markdown compose toolbar off for the SMTP rail. Both were wrong (user, 2026-06-08);
all three sites have since been superseded (§ Implementation status today).

**The chosen design is deliberately the small, uniform one (priorities #1/#2):**
markdown is the lingua franca. Every app *already* renders markdown
(`BodyFormat::Markdown`) and *already* has a markdown compose toolbar (previously
gated off for mail). So "HTML shown + editable" reduces to: convert HTML↔markdown in
shared Rust, stamp inbound mail `Markdown`, and **un-gate the existing markdown
toolbar for the SMTP rail**. Almost no new per-app UI; the heavy lifting is two
shared converters.

## Scope & invariants

- **Inbound.** A received email's best body part is converted **HTML → markdown** in
  shared Rust and stamped `BodyFormat::Markdown`; apps render it through the
  markdown path they already have. (Deferred — the original HTML is **not yet
  retained** as a blob; a "view original" affordance and faithful reply-quoting from
  the original would need that retention first. § Implementation status today.)
- **Outbound.** The mail compose bar is the **existing markdown editor**; Send
  converts **markdown → HTML** and builds `multipart/alternative` (a `text/plain`
  part — the markdown source / its plain rendering — **and** a `text/html` part), so
  plain-text clients and non-Fauna recipients still get a readable copy and
  deliverability is unchanged.
- **Privacy by default (product invariant — "user always controls their data").**
  Conversion **strips scripts and active content unconditionally** (markdown cannot
  carry them). Remote content (tracking pixels, external images) is **blocked by
  default**: remote image refs survive conversion as markdown image links that the
  renderer leaves un-fetched until a per-message "load remote content" opt-in. No
  remote fetch without a user action.
- **Uniform across all 7 apps (priority #1).** The HTML↔markdown converters and
  the capability flip live in shared Rust; apps reuse their existing markdown
  render + compose surfaces. No per-app HTML renderer, no per-app rich-text
  editor.

## Data model (shared Rust — `fauna-conversations` / `fauna-mail` / `fauna-core`)

- **Inbound stamping (landed).** The SMTP backend picks the best alternative part of
  a received message: a `text/html` part → run the shared **HTML→markdown**
  converter, store the result in `MessageSnapshot.body`, stamp
  `BodyFormat::Markdown`; a `text/plain`-only message stays `PlainText`
  (`html_markdown::inbound_mail_body`, called from `backends/smtp.rs` — superseded
  the hard-coded `BodyFormat::PlainText`).
- **Outbound serialization (landed).** A markdown compose body → run the shared
  **markdown→HTML** converter → build `multipart/alternative` (`text/plain` derived
  from the markdown + the generated `text/html`) — superseded the `text/plain`-only
  builder in `libs/fauna-conversations/src/rfc5322.rs`.
- **Capability — reuse `supports_markdown`, no new axis (landed).**
  `derive_capabilities(Smtp, _)` `supports_markdown` is **`true`**. That one change
  un-gates the existing markdown compose toolbar (`markdown-bold-button` etc.) for
  mail on **every** app at once (the toolbar is shared-capability-gated in each
  app's compose bar). No `supports_rich_text`/`supports_html` is needed.
- **Shared converters (landed).** Two WASM-safe functions in shared Rust, each
  unit-tested:
  - **HTML → markdown** (`html_markdown.rs`, `mail_parser` + `htmd`): strips
    `script`/`style`/active content; turns headings/bold/italic/lists/links/blockquotes
    into markdown; remote images become markdown image refs (left un-fetched, gated).
  - **markdown → HTML** (`fauna_core::markdown::markdown_to_html`): the inverse for
    the outbound `text/html` part; emits a conservative, sanitized subset (no script,
    no remote includes the user didn't author).
  Both used by every app (web via WASM, native via UniFFI).

## Rendering

**Text formatting — no new app work.** Because inbound mail is stamped
`BodyFormat::Markdown`, the shared producer consumes that discriminant once —
`document_for_body` in `message.rs` — and builds the message's `RenderDocument`
through the markdown path; every app then walks that document with the
render surface it already has (linux `views/document.rs`, windows
`DocumentPainter.cs`, apple `DocumentBodyView.swift`, android `DocumentText.kt`,
web `document.ts`, tui `document.rs`) — no app branches on `body_format`
itself (render-model.md § D1 owns the mechanism).

**Inline images — a uniform render capability (Slice 3, landed all 6).** The shared
token model carries `![alt](https://…)` remote images (`MdSpan.image_url`,
`fauna_core::markdown::parse_markdown`/`FfiMdSpan` → native; `markdownToHtmlBlocked` →
web). Each app's markdown render site renders a remote image **blocked by default**:
a placeholder (the alt text + a "remote image blocked" affordance), **never an
auto-fetched image**. A per-message `load-remote-content-button` — shown only when the
body has ≥1 remote image (`count_remote_images`) — reveals the images for that message
by **re-rendering it in fetch mode** (native: re-render the image spans; web: re-render
the bubble via `markdownToHtml` instead of `markdownToHtmlBlocked`, so the blocked
`data-remote-src` images become `<img src>`). The per-message "remote loaded" state is
**render-time only** (no persistence, no new storage). An optional "view original HTML"
affordance (with HTML-blob retention) remains deferred — it is independent of this
slice.

> **Refinement (BUILT 2026-06-22; DONE on all 6 apps, windows last) —
> [render-model.md](../architecture/render-model.md) § Deltas → D3 owns the
> mechanism.** The reveal *state location* moved from each app's render layer into
> the `ConversationsManager` (and symmetrically the `FeedManager` for feed cards) —
> in-memory, **still no persistence**; the manager projects a reveal set onto
> `RenderBlock::RemoteImage`'s `revealed` flag at the read boundary, and the
> `load-remote-content-button` is a manager dispatch (`reveal_remote_images(id)`).
> The blocked-by-default posture above is unchanged; only where the reveal flag lives
> moved. See render-model.md § Deltas → D3 + § Implementation status today for the
> projection/dispatch detail.

## Composition (reuse the existing markdown toolbar)

The mail compose bar already has the markdown toolbar and `dm-text-field`; it was
gated off only while `supports_markdown` was `false` for SMTP. Flipping that
capability to `true` (above) un-gated it. Send routes the markdown body through the
shared markdown→HTML + `multipart/alternative` serializer. No new compose UI.

The toolbar buttons insert markdown **source** markers — the buffer literally
holds `**bold**`, which is what is stored and sent. The compose field then
**inline-styles** that source (the formatting also shows *in the editor* as you
type; inline emphasis markers are **hidden by default**, revealed at the caret edge,
with a per-editor toggle back to dimmed — see
[conversations.md](../ui/conversations.md) § Compose-field inline markdown
styling). This is decoration over a plain-text markdown buffer, **not** WYSIWYG:
there is no rich-text tree and no serialize-on-send boundary, so the wire format
stays markdown source. All seven apps insert the **asterisk** family — `**`
bold, `*` italic — for uniformity (priority #1); the shared renderer
additionally accepts the underscore forms (`__`/`_`) so inbound mail emphasis
(which `htmd` emits as `_…_`) renders too.

## Element IDs / ui.yaml

- **Reuse** the existing compose toolbar ids (`markdown-bold-button` etc.) — they
  light up on the mail rail. No new compose ids.
- `load-remote-content-button` — per-message, inside `dm-message-bubble` (and the
  feed-card twin), visible only when the body has ≥1 blocked remote image
  (`count_remote_images > 0`) and they have not yet been revealed. Clicking it
  reveals that message's remote images. Approved by the user 2026-06-09 (Slice 4)
  and **allocated in ui.yaml** (conversations + feed); `ui-actual-<app>.yaml`
  entries landed with each app's leg. (A "view original HTML" control is
  deferred — see § Rendering.)

## Security & privacy

- Conversion is the perimeter: HTML→markdown inherently drops `script`/`style`/event
  handlers/`iframe` (markdown can't represent them); the converter additionally
  neutralizes `javascript:`/`data:` links and image sources (only `http(s)` survive)
  and leaves remote `http(s)` image refs as `![alt](https://…)` with the url **un-fetched**.
  The outbound markdown→HTML emits a sanitized, conservative HTML subset.
- **The remote-image fetch gate lives in the shared renderer.** `fauna_core::markdown`
  models a remote image as an `MdSpan` with `image_url`; rendering takes a
  `RemoteImageMode`. Untrusted inbound bodies (mail, DMs) render with
  `RemoteImageMode::Blocked` — native apps show a placeholder for each image span,
  web uses `markdownToHtmlBlocked` (emits `<img data-remote-src=… class="blocked-remote-image">`
  with **no `src`** → the browser issues no request). Only a user action (the
  `load-remote-content-button`) flips that one message to fetch mode. Content the user
  authored / follows / is sending — outbound email `text/html`, feed articles, nest web
  pages — uses `RemoteImageMode::Fetch` (`markdown_to_html`).
- **Never execute; never auto-fetch.** Remote content blocked by default aligns with
  the privacy-respecting-default and user-controls-their-data product invariants
  (`principles.md` § The user always controls their data). Treat the converters **and the render-mode split** as a security-review
  surface: a regression that renders an inbound body in `Fetch` mode silently re-enables
  tracking-pixel fetches.

## Implementation status today

> **Emphasis-rendering fix (2026-06-13).** Slices 1–2 were marked done, but
> *italics* rendered as literal underscores everywhere — in the sender's Sent
> copy, in received HTML mail, and in the outbound `text/html` part — a
> user-reported bug. Root cause: the shared `fauna_core::markdown` parser only
> understood **asterisk** emphasis (`*`/`**`/`***`), yet (a) the inbound
> HTML→markdown converter `htmd` emits the **underscore** form `_…_` for
> `<em>`/`<i>` (it uses `**` for `<strong>`, so *bold* always worked) and (b) the
> macOS and windows compose toolbars inserted `_` for italic. The tier_3 e2e
> missed it because it only ever exercised `**bold**`. Fixed: `parse_inline` now
> parses `_italic_` / `__bold__` / `___both___` with CommonMark's intraword guard
> (so `snake_case` stays literal) — one shared change fixes received-mail italics
> on all 7 apps, the macOS/windows toolbars, and the outbound `text/html`. The
> macOS + windows toolbars were also unified onto `*` (matching linux/web/android
> and the asterisk-family bold buttons; the renderer accepts both). The e2e now
> exercises `_italic_` outbound and inbound `<em>`. *bold* was never broken; a
> report of bold not rendering points to a pre-fix app binary.

**Status matrix (all slices closed 2026-06-13 on the original 6 apps; D3
refinement closed 2026-06-22 on those same 6, then on tui too 2026-07-30 —
see the row below):**

| Leg | Status |
|---|---|
| Slice 1 — shared converters + inbound stamping + outbound `multipart/alternative` + capability flip | DONE |
| Slice 2 — per-app text render + compose toolbar, all 6 | DONE (web's bubble previously ignored `body_format` — fixed 2026-06-09, branching on `Markdown` through the shared `markdownToHtml`; android's toolbar mounted 2026-06-13, capability-gated) |
| Slice 3 — blocked inline images + per-message reveal, all 6 | DONE 2026-06-13 (linux lead set the placeholder shape; every app renders through the shared parser — see below) |
| Slice 4 — `load-remote-content-button` in ui.yaml | DONE (user-approved 2026-06-09; allocated in conversations + feed) |
| D3 reveal-state refinement (manager-owned reveal set) | DONE on all 7 (windows last of the original six; tui's `conversation_detail` `load-remote-content-button` landed 2026-07-30, the last app) — render-model.md owns |
| "View original HTML" + HTML-blob retention | DEFERRED — the original HTML is **not** retained as a blob today |

**Landed in shared Rust** (the mechanism facts a consumer needs):

- **Inbound** (`backends/smtp.rs` → `html_markdown::inbound_mail_body`):
  `mail_parser` selects the best body part; a genuine `text/html` part → shared
  HTML→markdown converter → stamped `BodyFormat::Markdown`; `text/plain`-only stays
  `PlainText`.
- **Outbound** (`rfc5322.rs` `build_message`): markdown compose body →
  `multipart/alternative` (`text/plain` = markdown source + `text/html` =
  `fauna_core::markdown::markdown_to_html`). The nest relays the raw bytes
  transparently (`email_handlers.rs send_handler` parses headers only).
- **Capability flip** (`capabilities.rs`): `derive_capabilities(Smtp, _)`
  `supports_markdown` = `true`; also flags the mail "Sent" copy `Markdown`
  (`manager.rs`).
- **Converters** (`html_markdown.rs`, WASM-safe, unit-tested): HTML→markdown via
  `htmd`/`html5ever` (script/style/embedding tags dropped; link/image destinations
  scheme-gated to `http(s)`/`mailto`; remote images survive as **un-fetched** refs).
  Outbound markdown→HTML reuses the sanitized `markdown_to_html` — no second
  converter. `markdown_to_html` emits real `<img src>` for `![]()` in Fetch mode;
  inbound mail/DM bodies use `markdown_to_html_blocked` + `count_remote_images`.
- **One shared parser, enriched to a true superset (tracked internally; historical —
  the per-app *consumption* path below has since moved, see the supersession
  note).** When this landed, all 6 apps (pre-tui) rendered images and text blocks
  by calling the SHARED `fauna_core::markdown` parser themselves, at render time —
  linux's bespoke `Block`/`Span` parser was deleted in its favor. To avoid regressing
  bubble features the shared parser lacked, it gained **`> ` blockquotes, `N.`
  ordered lists (`kind: "ordered_list"`, renumber-from-1 like `<ol>`), and
  `***bold-italic***`**, with the three `MdBlock` consumers of the day — web
  (`markdownToHtml`), windows (`MarkdownHelper.cs`), android (`MarkdownText.kt`) —
  updated to render the new kinds in the same change. **Superseded by
  [render-model.md](../architecture/render-model.md) § D1:** no app re-parses
  `body` at render time any more — the manager builds a typed `RenderDocument` once
  (still produced from this same enriched parser) and every app walks *that*
  instead. The three files above are gone: linux's `markdown.rs` was later deleted
  outright (`views/document.rs` walks the document), windows' `MarkdownHelper.cs`
  was deleted for `DocumentRenderer.cs`/`DocumentPainter.cs`, and android's
  `MarkdownText.kt` was deleted for `DocumentText.kt`. The parser enrichment
  described above is still true of `fauna_core::markdown` as the producer feeding
  `RenderDocument` — only the per-app consumption path changed. The per-app
  image legs each render a blocked placeholder (platform glyph + alt + the shared
  `conversations/detail/remote_image_blocked` caption, **no image source/request**)
  and wire the reveal button off the shared `count_remote_images > 0` gate (now
  `RenderDocument::has_blocked_remote_images` for the built document — render-model.md
  § Implementation status today); the reveal click is the **sole** inbound fetch.
  **No app may ever auto-fetch an inbound image** — the blocked render mode is
  the perimeter.

**Verification:** `tests/e2e-unified/tests/test_mail_html_roundtrip.py` (tier_3,
linux lead — markdown compose → `multipart/alternative` relay assert, real
`text/html` delivery → formatted render assert, and
`test_html_mail_remote_image_blocked_until_reveal` for the blocked→reveal flow;
macos + ios joined 2026-09-24 once the apple dial-seam gap it was blocked
on was fixed);
`test_conversations_remote_image.py` (tier_3, web); `test_conversations_markdown.py`
(web marker; the wasm inject seam stamps `Markdown` per-rail like linux);
Robolectric `DocumentTextTest` (android — placeholder + gate, driving
`DocumentBlocks` with constructed `RenderDocument` records since the `MarkdownText.kt`
→ `DocumentText.kt` port; the `--client android` reveal e2e stays host-emulator-gated
fleet-wide); shared-Rust unit tests on both converters and the parser superset.

## Cross-references

- `docs/goal/ui/conversations.md` § Detail — `dm-message-text` renders the
  manager-built `document` (`RenderDocument`); `body_format` was **removed** from
  `MessageSnapshot` and is now consumed only inside the producer
  ([render-model.md](../architecture/render-model.md) § D1) — mail inbound is still
  stamped `Markdown` internally so the producer picks the HTML→markdown result. The
  compose markdown toolbar lights up on `supports_markdown` rails (now incl. SMTP).
- [render-model.md](../architecture/render-model.md) — owns the current cross-app
  render *mechanism* (the `RenderDocument`/`RenderBlock` model, D1–D7, all BUILT). This
  doc's scope stays the HTML↔markdown conversion + capability flip + blocked-by-default
  posture; how a converted body reaches an app's widgets today is render-model.md's
  to describe, not this doc's.
- `docs/goal/behavior/smtp-server.md` — the SMTP relay layer these raw RFC 5322
  bytes travel over (size limits, the inbound arrival push); part selection and
  `multipart/alternative` serialization are this doc's own mechanism, above.
- **Superseded prior art:** an earlier design (tracked internally;
  frozen, skylink-era) prototyped exactly this HTML↔markdown approach via
  `mail_parser`/`htmd`; its `fauna-core-daemon`/IMAP-bridge plumbing is dead post-I6,
  but the conversion design is directly reusable. Mine it for ideas; do not implement
  against its architecture.

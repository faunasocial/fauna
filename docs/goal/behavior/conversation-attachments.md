# Conversation attachments — target state

Owns: conversation-attachments
Status: ratified — split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) on 2026-09-28; the shape, both rails and every app's receive render are built, outbound staging on all 7 apps, the store's retention rule ratified and built 2026-09-13, C2PA on-device built 2026-08-17 (android's badge still owed).
Authority: a conversation attachment below the page — the content-addressed `blob_hash` shape and the one door into the attachment store, the store's retention rule (a bounded cache, a pinned staged draft, the refill of an evicted attachment), the SMTP and FaunaMls rails' outbound and inbound mechanics, the staged-attachment preview, the privacy-metadata strip as a seam property, and C2PA at receive time. Defers the `attachment-button` and chip element IDs, the per-app frontier table and every other page rule to [`../ui/conversations.md`](../ui/conversations.md); a restored draft's attachment to [`conversation-drafts.md`](conversation-drafts.md); an attachment's at-rest reachability to [`conversations-at-rest.md`](conversations-at-rest.md); the provenance badge's UX to [`../ui/media.md`](../ui/media.md); the community class's attachment content kind to [`community-rooms.md`](community-rooms.md).

> **Audience:** shared-Rust work on `libs/fauna-conversations`' attachment store and both rails, and each app's staging and render glue.
> **Purpose:** how a file travels with a conversation message, and what each device keeps of it.

*Split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) § User actions on 2026-09-28, when that page doc was 56 bytes under the whole-file read ceiling; the page's Authority line already named attachments as a concept of its own. The subsection keeps its original parent heading, so a `§ Attachments` citation resolves here by changing only the file. A routing stub remains at the original location; prior history: `git log --follow docs/goal/ui/conversations.md`.*

*Reading this doc. Its text was carried verbatim, so an unqualified `§ <name>` citation may name a section that is not a heading here. § Status + residuals, § Outbound, § Attachments and *C2PA on-device* mean this doc's text. `§ Implementation status today` — and "the § Implementation status table above" — mean the per-app frontier table in [`../ui/conversations.md`](../ui/conversations.md); § Persistence resolves in [`conversation-drafts.md`](conversation-drafts.md) for a draft; § Encryption at rest in [`conversations-at-rest.md`](conversations-at-rest.md). Every other unqualified name — § State & data shape, § User actions — resolves in [`../ui/conversations.md`](../ui/conversations.md).*

## Section map

- **§ User actions → Attachments — implementation status today** — the whole concept under its original heading: the ratified shape, *Retention*, the SMTP rail, *Staged-attachment preview*, the FaunaMls rail, *Status + residuals*, *Privacy metadata*, and *C2PA on-device*.

## User actions

### Attachments — implementation status today

**Shape RATIFIED + SMTP rail AND FaunaMls rail built end-to-end (shared Rust, TDD'd); per-app
render (receive) lift DONE on all apps — linux / android / apple / windows / web (C2PA-on-device
detection at receive time is now real on native, BUILT 2026-08-17 — § Attachments "C2PA on-device"
below; the FaunaMls-rail web blob glue landed 2026-06-15 — see
§ Status + residuals below, which once listed it as pending).** **Outbound attachment staging (the
compose-side file-picker → `add_attachment` wiring) is a separate, larger gap, unbuilt on 5 of the 6
shipped apps as of 2026-07-18** (dark-rail-audit rerun; § Implementation status today has the
per-app table + file:line evidence). The two
design questions the prior inert state flagged are settled by prior art (the existing `fauna-media` +
nest blob-store + `BlobImageLoader` feed-image pipeline) and priorities #2/#3/#4 (reuse the richest
shared pattern):

**The unified shape (settled, no user-facing-impact code-shape decision):** attachments are
content-addressed by **`blob_hash`** (lowercase-hex BLAKE3 of the plaintext bytes) — the single
handle every app renders off (§ State & data shape). **That is an enforced invariant, not a
convention (2026-09-09):** `ConversationsManager::cache_attachment_bytes` — the one door into
the store, and a `uniffi::export`ed surface — rehashes the bytes and *refuses* a pair whose key
is not their BLAKE3, warning and skipping exactly as an undecryptable attachment is skipped.
Until then the key was taken on trust, and on the FaunaMls rail it is **sender-authored** (it
rides inside the sealed `ChannelAttachment`, so opening the blob proves the sender is a member,
never that the handle names those bytes). Two things followed, both closed by this one check:
any co-member who knew a hash could overwrite its bytes in the **single store shared across
every channel and room**, so a poisoned attachment rendered in conversations the attacker was
not even in; and because `send` re-resolves bytes from that same map by hash (above), a
victim's *staged outgoing* file could be swapped between staging and send, leaving them to seal
and sign the attacker's bytes under their own filename. Substituting bytes under a fixed key
now costs a BLAKE3 preimage. The refusal is silent rather than fallible on purpose: a `Result`
would change the FFI signature for all 7 apps to report a condition none of them can act on. `AttachmentSnapshot` gained `blob_hash` +
per-attachment `c2pa: bool` and dropped the per-app `uri`; `AttachmentDraft` is light
(`{blob_hash, filename, mime_type, size_bytes, is_image}`, **no bytes**). The bytes live in the
manager's in-memory **attachment store**, loaded back through `attachment_bytes(blob_hash)`. So the
*handle* is uniform and *resolution* is the only per-rail concern (SMTP: local cache, populated from
the inbound MIME; FaunaMls: nest content-addressed blob GET + `decrypt_blob`). This makes the
structured field the **rich** pattern — windows migrates *up* onto it (real image + C2PA badge),
never down to a stub.

**Retention — the store is a bounded cache, never the home of the bytes (ratified 2026-09-13;
BUILT the same day).** The per-record reader bound
([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § The home nest → *The
reader bounds what it fetches*) caps what one message costs a member — at most 64 entries of at
most 10 MiB — but not what the app **holds**: until 2026-09-13 the store kept every opened
attachment for the manager's lifetime, so any co-member of an end-to-end or community room could
grow every other member's process by 640 MiB per message until the OS killed it, and honest
traffic did the same more slowly. The bytes rest elsewhere — on the room's home nest
(`conversation-rooms.md` § The home nest → *Attachment bytes*, pinned past the blob GC by the
plaintext refs, § Encryption at rest → *Attachment reachability*) or in the INBOX segment record
(SMTP) — so the device holds a *cache*, and a cache has a budget. Three rules, one shared-Rust home
(`libs/fauna-conversations/src/store/attachments.rs`, the manager's `attachments` field):

- **The store holds at most `ATTACHMENT_STORE_BUDGET_BYTES` (128 MiB), the same on all 7 apps,
  evicting the least recently *read* entry first** — `attachment_bytes` is a read, so what is on
  screen stays resident. A hard-coded Rust constant, never a knob
  ([`../principles.md`](../principles.md) § One configuration surface): it follows from device
  memory and the wire's per-attachment ceiling, not from anything a deployment would choose; sized
  for the tightest platform (a dozen door-ceiling attachments, or some forty phone photographs).
- **A staged outgoing draft is pinned.** `send` re-resolves a draft's bytes from the store by hash,
  so evicting them would silently drop the user's own attachment from the wire; eviction skips every
  hash any compose draft still references (`DraftStore::staged_attachment_hashes`). The store may
  then exceed the budget by the user's own picks plus the newest entry — bounded by what the user
  chose, never by a remote member. **The pin has no window (2026-09-14):** staging registers the
  draft on its compose *before* its bytes enter the store (`ConversationsManager::stage_attachment`),
  and the store reads the pin set at eviction time, under its own lock, after the new entry is
  resident (`AttachmentStore::insert` takes the pin set as a closure) — so no insert can see a
  just-staged file's bytes without also seeing its pin. Until then the pin set was read before the
  store lock, and staging inserted before registering, so a receive-path insert on a store at budget
  could evict the bytes the user had just picked. What `send` does when a draft's bytes are missing
  anyway — a restored draft — is § Persistence's.
- **An evicted attachment is fetched again where it can be.** On the FaunaMls rail the receive loop
  remembers, per handle, where the bytes rest and which key opens them — channel, sealed content
  address, declared size, and the MLS epoch or the room generation (`AttachmentCoordinates`,
  manager-internal: nothing new crosses the FFI, `AttachmentSnapshot` is unchanged) — and **the send
  remembers the same for the sender's own attachments** (`SendOutcome::attachment_coordinates`,
  Rust-only), because a sender never walks its own record back: the end-to-end class cannot open
  it, and the community class's poll skips a record whose id the Sent copy already holds. A render-time
  miss on a remembered handle marks it *wanted* and pokes the receive loop; the next receive cycle
  (`refill_evicted_attachments`, after the walk, on native and web alike) repeats exactly the first
  receive's bounded fetch — the same size ceilings, the same open, the same content-address check
  at the store's one door — and notifies observers once, so the render that missed asks again and
  hits. A handle whose bytes are gone for good (absent on the nest, or no longer openable — on web,
  where no historical epoch keys are kept, an end-to-end attachment sealed before a membership
  change) is forgotten and renders **declared** from then on: filename and size, no bytes, the
  placeholder every app already paints for a not-yet-fetched attachment. **An app that reuses a bubble across renders keys its rebuild on residency as well as on the message** (`ConversationsManager::attachment_resident`, a read-only peek): evicting bytes or fetching them again changes no message, so until 2026-09-24 linux's identity-keyed bubbles and web's memoized blob URLs kept painting a dropped picture and never repainted one fetched again. **On the SMTP rail the
  ingest remembers the record** — its mailbox *and* UID, since INBOX and Sent number their UIDs
  independently — and a miss is repaired by re-reading exactly that record
  (`InboundMailSource::fetch_one`: one record after `uid − 1` on the existing feed kind, so no new
  wire kind; web drives the same re-read from its JS poll, `body_ref` resolution included), re-parsing
  its MIME through the first ingest's extractor, and caching the wanted parts through the same door.
  A record the mailbox no longer holds (moved to Junk, expunged) forgets its handles. **Each rail's
  sweep takes only its own wants** — the mail sweep needs the mail sources and a mail push wakes
  only it. **The coordinates ride the `history/<ch>` slice** (`ChannelHistorySlice::attachment_coordinates`,
  keyed by handle, additive at rest; stamped by the manager's snapshot so both slice writers carry
  them, unioned by the slice merge), so a device restored from the replica — whose poll never
  re-walks the records that named its restored attachments — fetches each on its first render
  instead of rendering it declared. A slice re-sealed to a room newcomer carries none
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § History for
  joiners).

Weighed and rejected: a disk-backed store (a new plaintext-at-rest surface on the device for bytes
that already rest sealed on the nest, and a second at-rest key question `encryption-at-rest.md`
would have to own); carrying fetch coordinates on the FFI record (7 app changes for a field no app
renders); and a per-platform budget (the same behavior everywhere is priority #1, and the tightest
platform's number costs desktop only a re-fetch).

**Built (SMTP rail, shared Rust — `libs/fauna-conversations`):**

- `add_attachment` / `remove_attachment` (+ `add_new_thread_attachment` /
  `remove_new_thread_attachment`) mutators on `ConversationsManager`; the attachment store +
  `attachment_bytes` / `cache_attachment_bytes` accessors. `send` / `send_new_thread` resolve the
  staged drafts to bytes and pass them to the backend; the sender's echo carries exactly the
  attachments that were resolved, and a draft whose bytes the store does not hold refuses the send
  (§ Persistence).

**Staged-attachment preview (compose-side render of `ComposeState.attachments`).** A staged
attachment renders as one indexed chip per `AttachmentDraft`, in **both** compose surfaces
(`dm-compose-bar` inline reply and `dm-compose-form` new-thread) — the same two the
`attachment-button` sits in:

| Element | Renders | Action |
|---|---|---|
| `dm-compose-attachment-chip` (indexed) | The draft's `filename` + its `size_bytes` through the shared [`byte_size`](../behavior/value-formatting.md) formatter; an icon keyed off `is_image`. | Informational. |
| `dm-compose-attachment-remove` (indexed, parallel) | `×` on the chip. | `remove_attachment(thread_id, index)` (inline reply) / `remove_new_thread_attachment(index)` (new thread). |

The index is positional over `ComposeState.attachments`, which is what the app's chip loop
enumerates, so chip and mutator index cannot drift. `AttachmentDraft` is light by design (no bytes),
so everything the chip shows is already in the observed snapshot — no attachment-store round-trip,
and a multi-MB image costs nothing to display. **Apps MUST route the size label through the
shared `byte_size`**, never a native byte formatter or a hand-rolled threshold table
(priority #2; `value-formatting.md` § Byte sizes owns the decision). Rendering a *bare filename*
with no test ID (windows' pre-2026-07-20 shape) does not satisfy this — it is the poorest existing
rendering and migrates *up*.

Without this preview a user attaches a file and gets **zero** feedback that anything staged until
the message sends, and the remove mutators — built since the SMTP rail landed — have no caller at
all. Apple built it first (2026-07-20); per-app status is the § Implementation status table above.
- **Outbound:** `rfc5322::build_message` emits `multipart/mixed` with one base64
  `Content-Disposition: attachment` part per attachment (the standard email shape a non-Fauna MUA
  renders) when any are staged; flat `multipart/alternative` otherwise (unchanged).
- **Inbound:** `html_markdown::extract_attachments` walks the MIME parts (`mail-parser` decodes the
  transfer-encoding), carries them on the inbound message (the backend folds them into the
  document's `Attachment` blocks via `document_for_message`), and the receive path caches each
  part's bytes under its `blob_hash`. Covered by a Rust round-trip test (compose → send → parse
  back → same `blob_hash`, bytes loadable both sides) + tier_1 extraction/build unit tests.

**Built (FaunaMls rail, shared Rust — `libs/fauna-conversations` + `libs/fauna-mls`):** unlike SMTP
(bytes inlined in the MIME), the MLS rail seals each attachment as a **separate** content-addressed
blob and references it from the channel message:

- **Wire shape:** `ChannelMessageBody::Attachments { body, attachments: Vec<ChannelAttachment> }`
  (the dead single-blob `Media` variant was replaced). Each `ChannelAttachment` carries the uniform
  plaintext `blob_hash` (render handle), the `sealed_cid` (the nest fetch key), `filename` /
  `mime_type` / `size_bytes` / `is_image`, and the `epoch` it was sealed at. The references ride
  *inside* the sealed MLS envelope; the bytes ride separately.
- **Outbound (`FaunaMlsBackend::send` → `encode_body`):** per attachment, the MLS engine seals the
  bytes under the channel's current-epoch blob key (`MlsEngine::seal_conversation_blob` → the same
  `derive_blob_key(epoch_secret)` family; the raw epoch secret never leaves the engine), uploads the
  sealed blob to the nest's **content-addressed byte-source surface** — bearer-`POST /api/v1/blob`,
  keyed by `blake3(sealed)` (`docs/goal/architecture/api-layers.md` § HTTP residue, the single
  owner of the HTTP-residue inventory) — via
  the `ConversationsRpc::blob_put` seam, and references it by `sealed_cid` + `blob_hash` + `epoch`.
  (Not a bespoke `__conv` blob store: `__conv/<channel>` is the ordered *message-segment* store, not
  a content-addressed blob store — attachments ride the global content-addressed store, opaque, with
  audience-scoping implicit per `docs/goal/architecture/encryption-at-rest.md` Media row.) **`blob_put`
  itself was broken on every app** — both the native `NestConversationsRpc::blob_put` (until
  2026-07-20) and the wasm `WsConversationsRpc::blob_put` (until 2026-07-31)
  (`libs/fauna-client-conversations/src/lib.rs`) posted a
  raw, non-multipart body, but the nest's strict blob verifier
  (`bins/fauna-nest/src/blob_routes.rs::upload_blob`) has required `multipart/form-data` with `sidecar`
  + `bytes` parts since the blob strict flip — every real (non-mock, non-injected) FaunaMls attachment
  send failed nest-side with a 400. No prior test caught it (`create_mls_group`'s synthetic peer can't
  even reach `blob_put` on the native real backend — see the linux row's own note; the passing tests
  all used the mock backend or the inbound-inject seam). Found while landing linux's outbound-attach UI
  (a genuine real-UI send-with-attachment test finally exercised the path). **Both arms are fixed, and
  neither open-codes the multipart shape any more** — each target has exactly ONE helper that owns it
  and every blob uploader on that target calls it: native
  `fauna_nest_http::ReqwestNestContentApi::post_multipart_blob`, wasm
  `fauna_rpc_wasm::post_multipart_blob` (added 2026-07-31 as that API's browser twin; the media
  `upload` gesture's wasm arm was folded onto it in the same change, so the SPA no longer had two
  hand-written copies of a shape whose only failure mode is a 400 from a real nest). Both pass
  `fauna_media::sidecar::UploadSidecar::conversation_attachment()` as the sidecar.
- **Inbound (`poll_inbound_conv`):** an `Attachments` message GETs each blob by `sealed_cid`
  (`blob_get`), opens it under the message's epoch blob key (`MlsEngine::open_conversation_blob`,
  grace-decrypt aware via the per-epoch blob-key cache), caches the plaintext under `blob_hash`, and
  yields a `MessageSnapshot` whose `document` carries the rendered attachments as `Attachment` blocks.
  Covered by a Rust round-trip
  test (alice send w/ attachment → bob `poll_inbound_conv` → same `blob_hash`, bytes loadable) +
  `fauna-mls` seal/open unit tests (incl. cross-epoch grace-decrypt).

**Status + residuals.** The per-app render lift is **DONE on all 7 apps**
(§ Implementation status today): every app sources attachments from
`MessageSnapshot.document` (the manager folds one `Attachment` block per
attachment via `document_for_message`; the sibling `attachments` snapshot field
is removed — the Rust read projection is `attachment_blocks(&document)`), and
resolves bytes via the shared `attachment_bytes(blob_hash)` loader. The
FaunaMls-rail web blob client (`WsConversationsRpc::blob_put`/`blob_get` over a
browser `fetch`) is landed for **both** halves. `blob_get`:
`test_fauna_mls_web_receives_from_linux_sender.py` proves a real cross-engine send
(linux's native `blob_put`, fixed 2026-07-20 — see § Outbound above) received +
rendered over web's `blob_get`. `blob_put`: fixed 2026-07-31 and proven by
`test_conversations_attachments_outbound.py::test_web_real_faunamls_send_with_attachment_renders_in_sender_echo`
— the first test anywhere to drive a real, non-mock FaunaMls send *from* web
(web's e2e default is the mock rail, which uploads no blob; that is precisely why
the bug survived). It is a genuine gate rather than a render check: the sender's
own echo is appended only on `Ok` from `backend.send`, and a failed `blob_put`
propagates with `?`, so a 400 upload yields no echo at all. The cross-app render e2e
is `test_conversations_attachments.py` (android host-emulator-gated). **The
upload-conformance gap this section once flagged is closed:** `Storage::ingest_blob`
has had exactly one implementation since the Phase-4 demolition (`SealedStorage`;
`storage/plaintext.rs` is deleted) and it never runs `process_media` or any
server-side decode — `docs/goal/ui/media.md` § Today's reality is the authority
(the conversation rail inherits this conformance, not a gap). **This status
covers the render (receive) lift only** — the compose-side outbound staging
(`attachment-button` → file picker → `add_attachment`) is a distinct gap
*owned by this page*; windows, web, linux, android, macos and ios all had it
by 2026-07-20 and tui landed its leg 2026-07-30, so **all seven apps have it**
(corrected 2026-09-21);
see Known residuals above and the per-app table in § Implementation status
today. Two further residuals remain:

**Privacy metadata (settled 2026-07-20 — the strip is a *seam* property, not a
app duty).** `ConversationsManager::stage_attachment` — the single function
both `add_attachment` and `add_new_thread_attachment` funnel through — runs
`fauna_media::process::strip_metadata` before hashing, so metadata is removed
from every staged attachment on every app — **to the extent that function covers
the format**; the authoritative per-container coverage table (a shrinking set
of declared residuals) is owned by `../behavior/sync-engine-deployments.md` § Ingress
metadata-strip convergence. It was per-app glue until then,
and that shape failed the moment it was tested: windows and android hand-rolled
a stripper, **linux and web landed their pickers on 2026-07-20 with none and
shipped location coordinates**. A privacy step each app must remember is the
wrong seam. `strip_metadata` is lossless (container segment/chunk removal, never
a decode/re-encode — a file with nothing to strip returns byte-identical),
**preserves C2PA** (JUMBF/APP11 kept, so the `c2pa` residual below is
unaffected), and passes any format it does not cover through untouched, so it
is safe for the arbitrary files an attachment may carry and idempotent for the
two apps that still strip client-side. Hashing happens *after* the strip
deliberately: `blob_hash` is both the render handle and the wire reference, so
hashing raw bytes would leave every receiver's content-address disagreeing with
the payload. **Web flipped 2026-07-20 — no gap remains on any app.**
`fauna-conversations`' wasm32 target now takes `fauna-media` with
`process_media` enabled (previously the crate's identity-passthrough stub, so
the web bundle grew by nothing and web staged unstripped); the change was a
one-line `Cargo.toml` feature flip with no source edit, exactly as anticipated.
**Measured cost, accepted:** the core wasm chunk (`fauna_wasm_bg.wasm`) grew
from 13,532,275 to 14,341,036 bytes (+808,761 bytes, ≈ +6%) — `image` +
`img-parts` are not new wasm-bundle risk (`fauna-wasm-media` already ships the
same pair for the media artifact), and the increase is a one-time core-chunk
cost, not per-page/per-render growth. Accepted in exchange for closing a real
EXIF/GPS leak on every web conversation attachment sent to date.

- **C2PA on-device — BUILT 2026-08-17.** `attachments_to_inbound`
  (`libs/fauna-conversations/src/backends/fauna_mls.rs:3864-3943`) now probes the decrypted
  attachment bytes via the new `fauna_media::process::detect_c2pa(mime, bytes)` (the probe-only
  half of the upload-side `process_media` pipeline, extracted so a receiver can call it without
  re-running strip/thumbnail) instead of hard-coding `c2pa: false`. Real on every native app —
  `fauna-conversations`'s native-target `fauna-media` dependency now takes the `c2pa-detect`
  feature (`Cargo.toml`), the same reader that has shipped on every native UniFFI app since
  2026-06-30 (`media.md` § State & data shape), and this closes the actual wiring gap: tui and
  the other five FFI-native apps get a real per-attachment verdict, not just the capability.
  **Web remains a genuine stub** (wasm target keeps `process_media` only, no `c2pa-detect` — the
  heavy `c2pa` tree stays out of the bundle, unchanged design). Proven by
  `fauna_mls_backend_tests.rs::attachment_c2pa_detected_on_inbound_for_signed_image` (a real
  C2PA-signed fixture round-tripped seal → upload → fetch → open → detect, landing `c2pa == true`
  on the receiver) plus `fauna-media`'s own `detect_c2pa_*` unit tests
  (`libs/fauna-media/tests/process_test.rs`). **No app-side paint change was needed on windows** —
  `DmMessageBubble.xaml.cs` already gates `BuildC2paBadge()` on `att.c2pa`
  (`AttachmentSnapshot.c2pa`, this exact field); that badge could structurally never paint before
  this fix, whatever the attachment carried. **The bubble's `c2pa-badge` is per attachment, off
  `AttachmentSnapshot.c2pa` — a message carries no C2PA verdict of its own** (`MessageBadges`
  once had a `c2pa` flag that no producer set; it was removed 2026-10-02). Since 2026-09-24 the test inject seam (`ConversationsManager::make_attachment_for_test`)
  runs the same probe over the injected bytes, so an injected signed picture carries the real
  verdict; **tui** (`attachment_bubble_elements`) and **linux** (the document walk's attachment
  arm, `views/document.rs`; its dead message-level badge removed) paint the badge beside the
  attachment it vouches for, witnessed by
  `test_conversations_attachment_c2pa.py::test_a_received_signed_picture_shows_a_provenance_badge`.
  **macos and ios joined 2026-09-24** — the shared FaunaKit
  `DmMessageBubble.swift` paints `c2pa-badge` beside each `.attachment` block whose
  `AttachmentSnapshot.c2pa` is true (its `attachment(_:)` view function), and the dead
  whole-message badge gated on `message.badges.c2pa` is removed. **windows joined 2026-09-26** —
  its bubble already painted the badge off `att.c2pa`, but as a bare `Border` carrying only an
  `AutomationId`, which UIA prunes (`count("c2pa-badge")` read 0 though the badge was painted);
  it now carries the shared `c2pa/badge_label` string as its `AutomationProperties.Name` and the
  `conversations/detail/badge_c2pa` tooltip, as the feed's list-card badge does. **android joined 2026-10-02** — its
  bubble's dead header badge on the whole-message flag is gone, and the attachment loop
  (`ConversationDetailScreen.kt`) paints the feed's shared `C2paBadge` leaf beside each attachment
  whose `c2pa` is true, pinned by the Robolectric `c2paBadgeRendersPerSignedAttachment`. The same
  test now runs `[tui, linux, macos, ios, windows, android]`; web's bubble verdict is the declared
  `false` stub above.

# Spaces — shared documents — target state

Owns: spaces-documents
Status: ratified — UI design + IDs user-approved (2026-06-24 two-level model; 2026-06-27 Notes WYSIWYG fidelity + net-new IDs; 2026-06-28 Fork-2 block-document substrate validated); apps + `libs/fauna-spaces` + the nest handlers are unbuilt while the wire kinds + Track-1 spike landed — the gap is declared in § Implementation status today
Authority: ui.yaml (`spaces` page, once landed) owns element IDs + per-page element scope — this doc stages the approved ID set until the first app implementation transcribes it (§ Element IDs: IDs land in ui.yaml WITH an implementation, and this section MUST then agree with ui.yaml); this doc owns the page UX (three-level layout, editor behavior, snapshot shape, error/edge rules); the architecture + trust property → architecture/encrypted-spaces.md; integration mechanics (crate/wire/handler shapes) → an internal design spec (2026-06-24; tracked internally); the full editor design → an internal design spec (2026-06-27; tracked internally); render folding → architecture/render-model.md § D7; the inline-reveal engine's compose-side use → ui/conversations.md § Compose-field. On conflict in another doc's domain, raise it.

> **Track-1 spike: DONE** (ran against `encrypted-spaces/prototype@4cda0ae8`; results in integration spec §8 +
> spike findings tracked internally, 2026-06-24): the ECS schema, the 15-method `Transport`, the
> wire shapes, and the no-zkVM + WASM-feasible posture are all pinned. The § State & data shape
> **snapshot-projection** field set is the one remaining open piece — it finalizes with the `SpacesEngine`
> projection as `libs/fauna-spaces` is built (the remaining gate is the **crate + nest handlers**, not the
> spike). The **UI layout/IDs are settled** and are safe to implement against.

## Goal
"Spaces" is Fauna's surface for **verifiable, end-to-end-encrypted collaborative shared state**. The **first
content type shipped is shared documents** (collaborative rich-text). A **Space is a container** (a team space)
that holds documents — and, in later releases, tables and calendars — with cryptographically-enforced
membership and the *verifiable-untrusted-server* property: the client cryptographically verifies the nest
applied every operation correctly. This page owns the user-facing two-level surface: **a list of Spaces → a
Space's documents → a document editor.**

## Layout & flow
Three levels, modeled on the `conversations` list+detail pattern (priority #3):

1. **`spaces` page (list of Spaces).** Top-level nav destination (`spaces-tab`). A list of Space rows
   (`space-list-item` component), a create-Space affordance, and the per-Space verified indicator.
2. **`space_detail` sub-page (inside a Space).** Reached by opening a Space. Lists the Space's documents
   (`space-document-list-item` component), a create-document affordance, and a members button that opens the
   Space-level access picker (`space-access-picker` component, modeled on `recipient-picker`).
3. **`document_detail` sub-page (the editor).** Reached by opening a document. A rich-text editor reusing the
   existing **`markdown-toolbar`** component (priority #3 — do not fork a new toolbar), with a save/sync
   status line and a delete affordance.

Mobile (ios/android) collapses the two-pane desktop layout to a `NavigationStack` push per level (same IDs),
exactly as conversations does (ui.yaml conversations § `notes.ios`).

## Element IDs
Canonical ui.yaml IDs this surface uses. **This section is the implementation contract** — the first app to
implement transcribes these into ui.yaml (page `elements`/`components` + the registry) as part of its
red-green TDD cycle, so the IDs land in ui.yaml *with* an implementation
rather than as an all-7-`nav_exceptions` skeleton. This section MUST agree with ui.yaml once landed.

**Navigation:** `spaces-tab` (top-level destination; mobile `more`, desktop `sidebar`, web `navbar`).

**Page `spaces` (list of Spaces):**
- `new-space-button` (button) — create a new Space.
- `space-item` (view, indexed) — a Space row in the list.
- component **`space-list-item`** → `space-name` (text, indexed), `space-member-count` (text, indexed),
  `space-verified-badge` (text, indexed — "verified": client cryptographically verified the nest applied every
  op correctly; the headline trust property, surfaced).
- `error-message`, `page-heading` (global).

**Sub-page `create_space`** (trigger `new-space-button`):
- `space-name-input` (text_input), `space-create-button` (button).

**Sub-page `space_detail`** (trigger `space-item`):
- `new-space-document-button` (button) — create a document in this Space.
- `space-document` (view, indexed) — a document row in the Space.
- `space-members-button` (button) — open the Space's member/access management.
- component **`space-document-list-item`** → `space-document-title` (text, indexed),
  `space-document-modified` (text, indexed).
- component **`space-access-picker`** (Space-level membership; mirrors `recipient-picker`) →
  `space-access-input` (text_input), `space-access-chip` (view, indexed), `space-access-suggestion`
  (view, indexed), `space-access-level-select` (select — viewer / editor).

**Sub-page `document_detail`** (trigger `space-document`):
- `space-document-title-input` (text_input) — document title.
- `space-document-body` (text_input — rich-text editor body).
- `space-document-save-status` (text — "All changes saved" / "Syncing…" / "Offline").
- `space-document-delete-button` (button — cryptographic deletion / retention).
- `space-presence` (view, indexed — live collaborator presence).
- component **`markdown-toolbar`** (reused — bold/italic/code/link).

### The document editor's target fidelity — Notes (bullets-first WYSIWYG)

The `document_detail` editor is **also Fauna's "Notes" surface** — an Apple-Notes-like, bullets-first editor
**stored in markdown**. Rather than a separate surface, Notes is **this editor leveled up** (priorities #1/#3 —
one document editor, not two; user lean 2026-06-27). Its target fidelity, **decided (user, 2026-06-27),
is (c) full WYSIWYG — structural and inline markdown markers are *never shown***; the editor draws real bullet
glyphs / tappable checkboxes / heading sizes and styles inline emphasis, while the document round-trips
losslessly to markdown. The full editor design — the block model, the additive task-list render variant
([render-model.md](../architecture/render-model.md) § D7), the nested-list/task parser, the structural-gesture
API, the per-platform marker-hiding strategy, and the web feasibility probe — is the **cold-start contract**
(design ratified 2026-06-27; tracked internally). It is **Path-2 only** (the platform text
control is a render + input-capture surface over the shared block model — never a platform rich-text-editor
model, no markdown round-trip at an editor boundary), which is exactly what § Where logic lives already mandates.

**Net-new IDs Notes adds — ✅ user-approved 2026-06-27 (priority #1):** `document-checkbox` (view, indexed; the
tappable task checkbox), plus optional `markdown-toolbar` extensions `markdown-checklist-button` /
`markdown-indent-button` / `markdown-outdent-button` (touch nesting — no `Tab` key on mobile). All four are
optional/indexed extensions of the **shared** `markdown-toolbar` (no new toolbar — see § Don't do these), so
conversations is unaffected. They **land in ui.yaml with the first app implementation** (per § Element IDs —
IDs land *with* an implementation, not as a skeleton), now that the set is approved (spec § ui.yaml surface).

> **Shared inline-reveal engine (2026-06-28).** The *inline* half of this editor's marker-hiding (emphasis markers
> hidden, revealed at the caret edge, over `fauna_core::markdown::inline_reveal_ranges`) is now **also the compose
> field's default** — conversations compose flips from dimmed to hidden-by-default with a per-editor toggle
> (`markdown-marker-toggle-button`), reusing this same engine ([conversations.md](conversations.md) § Compose-field;
> rationale tracked internally, 2026-06-28).
> "One editor, not two" now holds at the *engine* level too. The **structural** chrome + gesture half stays
> Notes-only (compose keeps Enter=send) — so the inline reveal unifies, the document gestures don't.

> **Document substrate = Fork-2 block-document model — VALIDATED 2026-06-28 (Proof 2 PASS).** The document is a
> **list of typed blocks, each block its own collaborative-text CRDT** (not a single text body — the abandoned
> Fork-1 shape). The ECS concurrent-convergence spike proved all four hard concurrent-edit scenarios
> (reorder / split / merge / indent ∥ edit) converge to identical, sensible state on both peers, with the
> merged-away-block edit *failing closed* rather than corrupting state
> (spike findings 2026-06-28 + validation design spec 2026-06-27, § Proof 2; tracked internally). § State & data shape /
> § Persistence / § User actions / § Errors & edge cases below are the **block-document model** this validated. The
> **editor design above is substrate-agnostic** — it lowers onto this Fork-2 substrate (and would have onto Fork-1
> unchanged).

## State & data shape
The exact field set finalizes with the `SpacesEngine` projection as `libs/fauna-spaces` is built; the spike has
pinned the underlying ECS schema (integration spec §8 + the appended documents-schema section). Preferred shape —
typed snapshot getters on the shared spaces backend (mirroring how conversations exposes a snapshot), so no app
makes per-platform shape decisions:

- `spaces_snapshot() -> SpacesSnapshot { spaces: Vec<SpaceSummary> }`,
  `SpaceSummary { space_id, name, member_count, verified: bool }`.
- `space_detail_snapshot(space_id) -> SpaceDetail { documents: Vec<DocumentSummary>, members: Vec<MemberSummary> }`,
  `DocumentSummary { doc_id, title, modified_at }`.
- `document_snapshot(space_id, doc_id) -> DocumentSnapshot { title, blocks: Vec<DocumentBlock>, save_status, presence: Vec<Presence> }`.

**A document is a list of typed blocks, not a single text body (Fork-2; validated 2026-06-28).** Each block is the
editor's `NoteDocument` block (design spec § The block model, 2026-06-27; tracked internally):

- `DocumentBlock { block_id, kind: BlockKind, depth: u32, checked: bool, text: String }`, where
  `BlockKind = paragraph | bullet | ordered | todo | heading | quote | code | raw`. The structural prefix
  (`- `, `1. `, `- [ ] `, `# `, `> `), the indent, and the `[ ]`/`[x]` are **derived** from `kind`/`depth`/`checked`,
  never stored in `text` — and `text` holds inline markdown verbatim (`**bold**`, `code`). The document round-trips
  losslessly to markdown by `kind`-prefix + `depth`-indent + `text`; any construct the schema does not model
  (GFM tables, footnotes, embedded HTML) is carried verbatim in a `raw` block.
- **Underlying ECS substrate (Fork-2):** a `blocks` table keyed by plaintext `block_id` PK + a singleton `doc` row
  carrying a `block_order: List<i64>` order-statistic list. **Each block's `text` is its own ECS collaborative-text
  CRDT** (the piece-table CRDT of `encrypted-spaces.md` § What an Encrypted Space is) — concurrent edits to the same
  block char-merge; structural ops (reorder / split / merge / indent) are `block_order` list moves + `depth`/`checked`
  cell updates + row insert/delete. Proof-2 evidence tracked internally (2026-06-28).

The **editable** `blocks` snapshot above is what the app editor binds keystrokes to. The **painted** view folds a
run of adjacent blocks into Fauna's existing `fauna_core::render` block model (priority #2/#3) — consecutive
`bullet`/`ordered` blocks fold into `ListBlock`, a run of `todo` blocks into the render-model **D7** `TaskList`
variant ([render-model.md](../architecture/render-model.md) § D7) — so the render path is shared with feed/mail.
These project from `SpacesEngine`'s open-Space state (integration spec §1.3); confirm the exact getter shape at
crate-build time.

## Where logic lives
**Shared Rust by default** (priority #2). `libs/fauna-spaces` (the `SpacesEngine` wrapping `encrypted_spaces_sdk`)
owns: Space/document/membership state, CRDT op application, and **verify-on-ingest** (the tracer-proof check
that drives `verified`). The spaces consumption backend (WASM via `fauna-wasm`, UniFFI via `fauna-ffi`,
mirroring `FaunaMlsBackend`) owns the WS-RPC seam + the inbound poll/verify/apply driver. **App glue is only
the rich-text editor widget binding** — the document model + CRDT ops are shared Rust; the platform renders the
shared render blocks and forwards keystrokes as ops. The verified badge is pure rendering of the
shared-computed `verified` flag — **never** a client-side trust decision.

## User actions
Each action calls a named `SpacesEngine`/backend method and re-renders the returned snapshot (no per-app
decisions):
- `new-space-button` → create Space → `SpacesEngine::create_space`.
- `new-space-document-button` → create document → `apply_op` (genesis document op: one empty `paragraph` block +
  its `block_order` entry).
- type within a block in `space-document-body` → per-block collaborative-text CRDT edit (insert/delete on that
  block's `text` cell) → `apply_op`; concurrent edits to the same block char-merge. `space-document-save-status`
  reflects the send/ack/verify state.
- structural gesture in `space-document-body` (Enter / Tab+Shift-Tab / tap `document-checkbox` / toolbar
  `markdown-checklist-button`/`markdown-indent-button`/`markdown-outdent-button`) → the editor's
  `apply_structural_gesture` (editor spec § The structural-gesture API) lowers it to block ops —
  `SplitBlock` / `MergeBlock` / `SetDepth` / `SetKind` / `SetChecked` / a `block_order` reorder move — **never** a
  text-prefix edit (the prefix is derived from `kind`/`depth`/`checked`, never stored).
- ⭐ **concurrent merge/delete of a block being edited:** an edit targeting a block a peer concurrently merged or
  deleted **fails closed** (the substrate rejects the orphaned edit with a typed error rather than corrupting
  state); the editor catches it and **re-targets** the edit (re-apply to the merge destination, or surface "that
  block was merged away") — see § Errors & edge cases. Proven in Proof 2's hard case.
- `space-members-button` + `space-access-picker` → `invite_member` / `remove_member` / set access level
  (viewer/editor) → `rotate_keys` as ECS requires.
- `space-document-delete-button` → `cryptographic_delete` (retention key-tree; new members never see deleted
  history).
- **Export (v1 invariant — ratified 2026-07-20, user-approved; see
  `../architecture/encrypted-spaces.md` § Role in Fauna's storage model).** A document exports to a
  markdown file (into a folder, or plain download) from the surface's **first release** — without it
  the data is structurally unreachable outside Fauna (no file bytes exist until a member device
  materializes them), the lock-in shape the product invariants forbid. The concrete element ID(s) are
  net-new and land with the export slice's TDD cycle under the standard user-approval gate — the
  *requirement* is ratified; the *IDs* are not yet.

## Persistence
Space state is the ECS **verifiable-DB**, **sealed beyond the routing floor**
(`encrypted-spaces.md` § Encryption at rest for Spaces — stronger than the conversations floor,
`encryption-at-rest.md`). The nest is the ECS **verifiable-DB engine**: it applies each sealed `Change` to Merkle search
trees and serves verified queries (Merk inclusion proofs), routing/indexing on the plaintext tree structure
(`space_id`, `actor_id`) only and **never decrypting Space values** (integration spec §8 — the post-spike
re-scope of the §3.1 "append-only relay" sketch; authority `encrypted-spaces.md` § Where the integration lives).
Clients cache the last-verified snapshot locally (spec §1.1 `storage.rs`). No plaintext document store exists
anywhere.

**Documents-schema column posture (Fork-2; the concrete instance of `encrypted-spaces.md` § Encryption at rest for
Spaces — "index entries on columns explicitly marked plaintext").** In the `blocks` table, **`text` (the
per-block textarea) is the encrypted content column** — the nest never decrypts it. `kind` / `depth` / `checked`
and the `block_order` list are **structural/index columns the schema marks `.plaintext()`** so the nest can route,
order, and serve verified queries on document structure without reading content; `block_id` is the **plaintext
PK**. This is the documents content-kind's plaintext floor; it feeds Slice 2's documents schema
(tracked internally) and the `encryption-at-rest.md` Spaces conformance row when the full Spaces
byte shapes are fixed.

## Errors & edge cases
- **Empty states:** no Spaces → an empty-Spaces prompt with `new-space-button`; empty Space → an
  empty-documents prompt with `new-space-document-button`.
- **Loading:** snapshot not yet hydrated → skeleton; `connection-status` (global) reflects WS state.
- **Verification failure:** if `ingest_op` fails tracer-proof verification (the nest tampered/forked), the
  op is **rejected** and `error-message` surfaces a tamper warning; `space-verified-badge` is withheld. This
  is the trust property made visible — a failed verify is never silently accepted.
- **Concurrent merge/delete of an edited block (Proof-2 hard case):** when one collaborator merges or deletes a
  block another is editing, the substrate **fails closed** — it rejects the orphaned block-edit op with a typed
  error rather than losing the text, resurrecting an orphan, or duplicating. The editor and the nest documents
  handler **must catch and re-target** this, never surface a hard error: re-apply the edit to the merge destination
  block, or (if the block is gone) surface a soft "that block was merged away" and keep the user's text recoverable.
  Editor side = `apply_structural_gesture` / edit-application (tracked internally); handler side = the
  Slice-2 error mapping (tracked internally). Both are flagged from the
  design spike findings, § Key findings (2026-06-28; tracked internally).
- **Offline:** edits queue locally; `space-document-save-status` shows "Offline"; catch-up **fast-forwards**
  (replays the `Change`s since the last seen change) on reconnect (no FF proofs — spec §5/§8).

## Architectural rules
- The nest is **untrusted for Space content** — never gate any Space affordance on the
  deployment storage mode.
- All 7 apps render the **same** IDs/layout (priority #1); the editor body is the only platform-native
  widget, bound to the shared render/op model.
- New net-new IDs beyond this approved set need fresh user approval (priority #1).

## Don't do these
- **Don't** build a separate plaintext document store or let the nest decrypt Space content.
- **Don't** fork a per-app document/CRDT model — it lives in shared Rust (`libs/fauna-spaces`).
- **Don't** reuse the file-sync `sync` page or its IDs — a Space is a multi-user collaborative concept, not
  per-user device folder sync (`encrypted-spaces.md` § Coexistence — "Space ≠ folder").
- **Don't** pull ECS's bundled `WebSocketTransport` into the client — route through Fauna's one-WS substrate
  via `FaunaSpacesTransport` (spec §1.4/§1.5).
- **Don't** add a new rich-text toolbar — reuse `markdown-toolbar`.

## Implementation status today
**No app has a reachable Spaces surface.** The UI design is settled + user-approved (2026-06-24, two-level
model, ID-level approval); the IDs are **not yet in ui.yaml** (they land with the first app implementation per
§ Element IDs); no `spaces`/`space_detail`/`document_detail` page exists on any app, and no app renders
these IDs.

**The `document_detail` editor's *engine* has landed ahead of the page it will serve (2026-06-28/29) — pure
groundwork, not user-reachable.** The block model, the structural-gesture API, and the caret-edge inline reveal
(§ target fidelity above) are implemented and tested in shared Rust (`fauna_core::notes`,
`libs/fauna-core/src/notes.rs` — `parse_note`/`serialize_note`/`apply_structural_gesture`/`note_line_map`/
`caret_to_block_caret`/`block_caret_to_byte`) and consumed by web as a `notes` mode on the shared
`MarkdownEditor.svelte`, over the WASM faces in `apps/fauna-web/src/lib/notes-editor.ts` /
`notes-editor-cm.ts` (`notesEditorExtensions()`). This mode is never invoked from any route today — it exists
only as tested infrastructure, gated on the same `libs/fauna-spaces` + nest-handler blocker below before it can
host a real document. The remaining apps get the equivalent once each platform's `document_detail` host
exists.

No `fauna.spaces.*` **wire kinds exist** (the Slice 1 set, registered with no handler, was removed
2026-10-01 before the 2026-10 baseline — `../architecture/encrypted-spaces.md` § Implementation status today);
the wire kinds, `libs/fauna-spaces`, the spaces consumption backend,
and the nest handlers **do not exist yet** (this is the gate for the app work). The Track-1 spike is **done**
(integration spec §8); the § State & data shape snapshot-projection field set finalizes with the crate. **Hard
gate:** the production ECS dependency is blocked until ECS reaches audited, non-research-preview maturity
(`encrypted-spaces.md` § Implementation status today). Build order (spec §7): spike (done) → `libs/fauna-spaces`
→ nest handlers → 7 apps (macos leading ios).

## Done definition (per app)
- [ ] `spaces-tab` reaches the `spaces` page; Spaces list renders from the shared snapshot.
- [ ] Create Space; open a Space; create/open/edit a document (shared CRDT ops); delete a document.
- [ ] Invite/remove members + set access level via `space-access-picker`.
- [ ] `space-verified-badge` reflects the shared-computed verified flag; a verification failure surfaces via
      `error-message` and withholds the badge.
- [ ] Offline edit → reconnect catch-up works; `space-document-save-status` accurate.
- [ ] ui.yaml IDs landed + `ui-actual-<app>.yaml` updated; `just ui-lint` clean for this page.

## Reading list
1. `principles.md` (priorities #1–#3, § Engineering principles).
2. `docs/goal/architecture/encrypted-spaces.md` — the architecture + trust property (authority).
3. Internal design spec (2026-06-24; tracked internally) — crate/wire/handler mechanics + the spike.
4. ui.yaml `conversations` page + `markdown-toolbar` / `recipient-picker` components — the patterns this mirrors.
5. `libs/fauna-spaces` (once it exists) + the spaces backend; `tests/e2e-unified/ui.yaml` `spaces` block.

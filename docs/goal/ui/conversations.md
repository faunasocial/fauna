# Conversations — target state

Owns: conversations, bridged-adapter
Status: ratified — the one-page-all-rails model, compose styling (2026-06-13/28), participants-vs-reply-recipients (2026-06-07), attachments, reactions & delete (2026-06-23), and drafts v2 are all user-ratified and built; the per-app frontier lives in § Implementation status today
Authority: ui.yaml (`conversations` page) owns element IDs + per-page element scope; this doc owns the unified-conversations page UX/behavior — the one-page-all-rails model, the RailBackend/ConversationsManager split, capability gating, compose-field inline markdown styling, participants-vs-reply-recipients, reactions & message delete, and the attachment and draft *surfaces* (their element IDs, user actions and per-app frontier rows); defers attachments below the page (the store, its retention, both rails, the metadata strip, C2PA at receive) to [`../behavior/conversation-attachments.md`](../behavior/conversation-attachments.md), what a compose draft keeps and how it is restored (drafts v1/v2) to [`../behavior/conversation-drafts.md`](../behavior/conversation-drafts.md), the new-message banner's when/for-whom decision and its per-app build to [`../behavior/message-banners.md`](../behavior/message-banners.md), the page's at-rest conformance section (and how inbound reaches the page per rail) to [`../behavior/conversations-at-rest.md`](../behavior/conversations-at-rest.md) — all four split out 2026-09-28 —, the MLS channel protocol + platform bindings to [`../behavior/direct-messages.md`](../behavior/direct-messages.md), the room model (membership, the three confidentiality classes, roles, join rules, the home nest, history for joiners) to [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md), the retired group plane's pointer stub to [`../behavior/groups.md`](../behavior/groups.md), per-content-kind at-rest rows to [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md), the render tree (D1–D5) to [`../architecture/render-model.md`](../architecture/render-model.md), drafts/MLS-replica reserved-set shapes to [`../behavior/file-sync.md`](../behavior/file-sync.md), timestamps + thread-label display to [`../behavior/value-formatting.md`](../behavior/value-formatting.md), muted-keyword semantics to [`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md), and mark-as-spam training to [`../behavior/mail-spam.md`](../behavior/mail-spam.md). On conflict in the other doc's domain, raise it.

Last updated: 2026-09-29 (§ Reactions & message delete → *Rendering / picker glue*, status: windows ships the free-entry field over the shared `MORE_GRID_EMOJIS` grid). 2026-09-28 (§ Reactions & message delete → *Rendering / picker glue* ruled: the fuller picker is a native chooser or a free-entry emoji field carrying `dm-reaction-more-button` in entry mode, never a fixed grid alone; web and windows gain the field and keep a shortcut grid whose list moves to shared Rust). 2026-09-24 (§ State & data shape → *When a thread is read*, status: the mail rail's read state is the mailbox's `\Seen` flag — read at launch, set on open, synced across devices — and no longer uses the floor). 2026-09-22 (§ Where logic lives → the OS-toast decision's rule 2 gates on the run's launch floor, published on every snapshot as `launch_floor_ms`, so a slower rail's launch re-drain never banners for old mail; the banner and the unread indicator read one floor; § Reactions & message delete → *Durability across a relaunch*: a slice merge no longer costs a device its own reactions — rule owned by `devices.md`). 2026-09-21 (§ State & data shape → *When a thread is read* added: read on open, own never unread, account-scoped read state as target, the in-memory floor as today's status). 2026-09-20 (§ Reactions & message delete → *At rest* corrected: the aggregate and the tombstone are derived but NOT re-derivable, so they ride the `history/<ch>` slice — mechanism owned by `devices.md`; § Durability across a relaunch added). 2026-09-15. Design spec (frozen provenance; design ratified 2026-05-10; tracked internally).

## Goal

The Conversations page is the **single user-facing surface for every
DM** the user sends or receives, regardless of underlying rail. One page
covers:

- Fauna native MLS DMs (E2E-encrypted, 1:1 + groups).
- SMTP email (subject-keyed and participant-keyed threads).
- Bluesky chat DMs.
- Nostr DMs (NIP-04 / NIP-17).
- Fediverse (ActivityPub) direct messages.

Group threads (MLS groups) and email-style subject-keyed threads live
on the same page as 1:1 chats — there is no separate "Groups" page. Per
the spec section 1, the UI uses **chat grammar everywhere** (C1):
subject is per-message metadata, not a structural mode switch. Adding
a participant to a FaunaMls 1:1 forks a new MLS group thread (Signal
semantics); other rails add in place. Adding to a group or
subject-keyed thread also stays in place.

Per-protocol divergence collapses behind a `RailBackend` trait in
`libs/fauna-conversations`. Apps render off one snapshot and never
branch on rail — only on `capabilities.*`.

## Implementation status today

**Unread tracking (ratified + shared Rust built 2026-09-21; § State & data shape → *When a thread is read*).** `ThreadSummary.unread_count` is real and a thread is read by opening it, both decided in `fauna_conversations` — so all 7 apps, which already paint `dm-unread-indicator` from the count and already select through the shared `select_thread`, show and clear it with no app change. **Read state is the account's (2026-09-24):** a mail thread's is the mailbox's `\Seen` flag on every app, a native thread's the synced read marker on every app that hosts an account runtime (one carrier per rail — owner [`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md)). **Declared gap:** native threads on web stay **in memory for one app run** (a native message that arrived while the tab was closed shows no indicator — the floor rule, same section). **android is the one app where it is wrong rather than merely owed a witness:** a single-pane shell must close the thread on the way back to the list (same section) and android does not, so a thread it has opened once swallows later arrivals unflagged. Residue: none — every app's redundant select-time `mark_read` call (web's went with the build, macos's and ios's with the apple trickle-down, windows' last) is retired; the manager's read-on-select is the only caller left.

**Partly built (ratified 2026-09-05): the `Bridged` adapter and this chain's half of the room model** (§ Where logic lives → *The `Bridged` adapter*). **The shared-Rust slice landed 2026-10-02:** the unit `Rail::Bridged`, `TypedAddress::Bridged` with its `unresolved_bridged` twin, the additive `bridge: Option<BridgeIdentitySnapshot>` on `ThreadSummary` / `ThreadDetail` (projected from the rail's registry, the thread's `glyph` then the declared one), one `BridgedBackend` over the `BridgedSink` / `BridgedSource` seams with the declared-vector overlay in `capabilities(thread)` and the `poll_inbound_bridged` driver, `derive_capabilities(Bridged, _)` as the most-restrictive vector, `SourceGlyph::Bridge`, and the test agents' inject grammar's `bridge_id`; the Bluesky and ActivityPub stubs and variants are deleted, and `ThreadEncryption::None` with them. **The nest's Phase G kinds landed 2026-10-03** ([`../architecture/apps/bridges.md`](../architecture/apps/bridges.md) § Implementation status today), with the user-side wrapper `fauna_client_bridges::conversations::BridgedConversationClient` the glue implements the seams over, and `Rail::Bridged.send_wire_kind()` now answers `fauna.bridges.conversation.send`. **The receive half and the first render landed 2026-10-03, on tui (the lead app):** `ConversationsSession::register_bridged` registers the one backend over the glue's two seams and the receive loop sweeps it on its ticker and on `fauna.bridges.push.conversation_changed`, under one row cursor shared with the manual `poll_bridged`; `BridgedSource` reads `conversation.rooms.list` too, and `BridgedBackend::set_rooms` loads each pass's identities (connected rows only) and the family gate's marker per room, which the manager projects as the additive `guardian_state: Option<GuardianState>` on `ThreadSummary` / `ThreadDetail`; the snapshot's additive `bridges` lists the registered bridges for the recipient picker; a bridged room is its **far** participants — the account's own far address is dropped at bucketing, so a deposit, the user's Sent copy and a thread opened from the picker are one thread; the address probe runs the rails in a fixed order with the bridged rail last, because its resolve is the only one with an effect (`rooms.open`). The native glue is `fauna_client_conversations::NestBridgedGlue` — both seams over `BridgedConversationClient`, sealing to `bridge_x25519` and to the account's own recipient key and opening the inbox under the session's shared mail-key cache (the account needs its mail key: with none, rooms still list and paint, the inbox read is a quiet no-op and a send is refused by name). tui registers it at login and paints the declared glyph with the declared label, `thread-room-class`, `conversation-guardian-state` on the row and in the detail, and the bridges' labels under the recipient picker. **Web followed the same day:** the glue is one implementation on both targets — its logic generic over the transport, its keys behind a `BridgedKeySource` (natively the `MailKeyCache`; in the browser the standing key set, mail-epoch roots and the account's own recipient public key the JS receive loop already hands the wasm manager) — so web registers the same `NestBridgedGlue` over `WsRpcClient` in `WasmConversationsManager::with_conversations`, drives the shared `poll_inbound_bridged` through `pollBridged()` on its mail rail's pump slot (the ticker, the reconnect sweep and `fauna.bridges.push.conversation_changed`), and paints the same row, detail and picker as tui. **The tier_3 journey covers all three shapes on both apps since 2026-10-03** (`tests/e2e-unified/tests/test_bridged_conversation.py`): a consented bridge's room with the reply opening under the bridge's key only, a supervised ward's cold-peer room marked `held` and still readable, and a real Nostr gift wrap arriving as a `nostr`-bridged room. **The native apps followed on 2026-10-04:** linux registers the same `NestBridgedGlue` at login as tui does, and the `fauna-ffi` `conversations_session` factory registers it over the login's one mail-key cache, so android, macOS, iOS and Windows receive bridged rooms with no per-app glue; the picker's bridged-chip text is the shared `TypedAddress::display_with_bridges` (UniFFI `typed_address_display_with_bridges`), and the marker's text and token are `guardian_state_label` / `guardian_state_attr_token` over the FFI. linux and android paint what tui paints — the declared glyph with the declared label (`bridge` attribute = the bridge id), `conversation-guardian-state` on the row and in the detail, the bridges' labels under the recipient picker — and the journey runs on linux too. **What is not built yet:** macOS, iOS and Windows receive bridged rooms but paint only the declared glyph — not the declared label, the marker or the picker's bridges line; android's journey run waits on its emulator venue like every android e2e; a user-opened room is 1:1 (`rooms.open` takes one far address), so a send naming several far addresses is refused in the shared backend rather than minting one room per peer; a disconnected room and an undelivered Sent row carry no label of their own yet (the thread falls to the generic glyph and the most-restrictive vector; the shape proposed for both, awaiting the user's rule-A answer since 2026-10-03, is in § Element IDs → *A bridged room no bridge serves*, and neither flag is read past the glue until it is answered). The Nostr leg's rows and kinds moved onto the family on 2026-10-03 ([`nostr.md`](nostr.md) § Implementation status today → DMs), and `NostrBackend`, its `NostrDmSink` / `NostrDmSource` seams, `Rail::Nostr` and `TypedAddress::Nostr` — with the last per-rail `encryption` constant — were deleted the same day: `Rail` is `FaunaMls | Smtp | Bridged`, and a Nostr DM thread is a bridged one. **The other two first-party legs followed on 2026-10-03, so all three are migrated (ruling 3):** the nest's first-party leg seam (`bins/fauna-nest/src/bridge_legs.rs`) serves each account the legs whose network it has linked, and every leg's inbound goes through its one `deposit_gated` (the family gate's verdict, the seal to the recipient key, the deposit). The **Bluesky** leg `{ bluesky, Bluesky, butterfly }` is a worker that polls each consume-side linked account's `chat.bsky` conversations (`bluesky::dm_worker`, with D7's hosted-backing gate at its start site — [`../behavior/atproto-pds-full.md`](../behavior/atproto-pds-full.md) § D7) and drains the leg's outbox through `chat.bsky.convo.sendMessage` under the account's session; the **ActivityPub** leg `{ activitypub, Fediverse, globe }` takes the inbox's direct `Create{Note}` ([`../behavior/activitypub.md`](../behavior/activitypub.md) § Architecture → *The inbound audience gate*) and drains its outbox as a `Create{Note}` addressed to the peer alone on the signed delivery queue. No app changed: both legs' rooms arrive on `conversation.rooms.list` with their declared identity, like any bridge's. **Not built on the two new legs:** a room is one-to-one and keyed on the far network's stable id, so `rooms.open` takes a DID on Bluesky and an actor URI on ActivityPub — a typed handle or `user@host` is not resolved yet; a Bluesky conversation with more than two members is skipped; an inbound `Update` or `Delete` of a direct Note changes nothing; and neither leg has a tier_3 journey yet (their deposits, gates and far-call shapes are pinned in nest tests). **The room object itself is built for the native rail (2026-09-08):** `ThreadDetail.room` carries the derived class, per-member roles and the policy, `ThreadCapabilities` carries the role-gated four, and the class derivation (`derive_room_class`) answers all three classes — mail and the bridged rail produce the transport-only arm through `room::transport_room` (2026-10-02), so every rail derives its `encryption`. Owner of every room-model gap: [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Implementation status today. Sequencing: [`../architecture/third-party.md`](../architecture/third-party.md) § Implementation status today. **Designed 2026-10-02 (the third-party chain's adapter design pass):** the four gaps the 2026-09-05 ruling left open — how the per-bridge identity rides a unit `Rail::Bridged`, the user-side kinds, the Nostr leg's at-rest and wire migration, and the bridge glyph — are ruled in § Where logic lives → *The `Bridged` adapter* rulings 2–3, [`../architecture/apps/bridges.md`](../architecture/apps/bridges.md) § Bridge-kind catalogue → Phase G, [`nostr.md`](nostr.md) § Implementation status today → DMs and [`../architecture/third-party.md`](../architecture/third-party.md) § The manifest; the build is enumerated as five slices — shared Rust, the nest's Phase G, the Nostr leg, tui's render with the tier_3 journey, web's move onto the unified list — of which the first two have landed.

**The attachment store's retention rule (§ Attachments → *Retention*, ratified + BUILT
2026-09-13)** is built for both rails on all 7 apps in shared Rust — budget, LRU eviction, pinned
drafts, and the refill on the next receive cycle: FaunaMls re-fetches the sealed blob (native poke +
conversation sweep step; web sweep step), SMTP re-reads the one mail record by mailbox and UID
(native mail sweep step; web's JS-driven twin in its mail poll, type-checked but without a test of
its own), and the coordinates ride the `history/<ch>` slice so a device-synced client's restored
bubbles fetch their attachments too. Pinned by `manager_integration_tests.rs`'s store tests, the
`evicted` and restored-slice cases in `fauna_mls_backend_tests.rs` (end-to-end, community class,
restored from a slice), the refill cases in `smtp_backend_tests.rs` (refill, a record gone from
the mailbox, per-mailbox UIDs) and `store/history.rs`'s at-rest and merge tests. The sender's own
FaunaMls attachments are remembered by the send itself (built 2026-09-13), pinned by the
sender-side cases in `fauna_mls_backend_tests.rs`: evicted on the sending device (end-to-end and
community class — the latter after its own poll has skipped the record) and restored from a slice
on the sender's other device.

The page is built and shipping on all seven apps over the shared
`ConversationsManager` snapshot (list / detail / compose / recipient-picker /
rename + add-participant overlays; no separate groups page). Per-surface
frontier (2026-07-10; per-leg landing dates live in git history):

| Surface | web | linux | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| Dual-rail `ConversationsSession` wiring (FaunaMls + SMTP send/receive loop) | ✅ (wasm wrapper) | ✅ (in-process `from_manager`) | ✅ (fauna-ffi factory) | ✅ | ✅ | ✅ (`ConversationsManagerHost.startConversationsSession`) | ✅ (in-process `from_manager` + `register_smtp`/`register_mail_receive`, mirrors linux — `conv_backend.rs`) |
| Inline markdown styling applier (`decoration_map`) | ✅ | ✅ (lead) | ✅ (`MarkdownRichEditBox`) | ✅ | ✅ | ✅ | n/a (terminal shows markers literally, by design — decoration is GUI-only; § Compose-field inline markdown styling) |
| Hide-markers default + `markdown-marker-toggle-button` | ✅ (lead) | ✅ | ✅ | ✅ (2026-08-06) | ✅ (2026-08-06) | ✅ (2026-07-17) | **n/a — declared absence** (2026-07-30): the toggle flips between the hidden- and dimmed-marker *decoration* modes, and § Compose-field inline markdown styling ratifies that a terminal composer shows markers literally (the decoration engine is GUI-only), so there is nothing to toggle. Recorded as a `declared_absence` gate on `test_conversations_marker_toggle.py` (citation required by `helpers/app_surface.py`) + a ui.yaml `markdown-toolbar` note, so it reports itself as a cited skip instead of vanishing into the deselect delta. |
| Shared show-markers dim rule (`compose_show_markers_dim_ranges`) | n/a (hide default) | ✅ | ✅ | ✅ (2026-08-06) | ✅ (2026-08-06) | ✅ (2026-07-17) | n/a (no decoration engine) |
| Compose-affordance capability gating readable over automation (`get_attr(id, "disabled")`; § Capability gating) | ✅ | ✅ | ✅ (reference) | ✅ | ✅ | ✅ | ✅ 2026-07-30 — `automation.rs` now **derives** `disabled` from `Element::enabled` (inline attrs still win). ⚠ Before that it answered `Null` for *every* element, so `test_capability_gating.py`'s `disabled in ("false", None)` assertions passed **vacuously** on tui and its `== "true"` ones could never pass — the surface's real state was unobservable. Same fix exposed that tui had never painted `topic-toggle-button`/`subject-input` in the **reply** bar (ui.yaml declares both on `dm-compose-bar`), an absence hidden for the same reason |
| Reply recipients (editable To line + `dm-reply-all-button`) | ✅ | ✅ (lead) | ✅ | ✅ | ✅ | ✅ | ✅ (`dm-reply-all-button` + `dm-reply-recipient-chip`, `mod.rs`) |
| The selected message (§ The selected message) — mark it **and** bring it into view | **n/a — structurally unreachable** (2026-08-15): `SearchNav::Mail` is exclusively produced by the local sealed-content index (backend 2 — `ui/search.md` § State & data shape: "Local rows always carry `Some`... nest rows carry `Some(Post)` or `None`, never `Some(Mail)`"), and web's `SearchManager` runs nest-only, structurally (no Tantivy on wasm — same section). No code path can ever hand web a `Mail` target to select, so the paint has nothing to drive; the `nav.Mail` arm in `routes/search/+page.svelte` exists only for TS discriminated-union exhaustiveness | ✅ 2026-08-15 — `views/search.rs`'s `Mail` arm now calls `select_thread_and_message`; `message_bubble.rs`'s `message_timestamp_label` paints the `selected` attribute on **every** render arm (deleted/legal-takedown/blocked/muted/collapsed/normal — mirrors tui's `message_timestamp_element`, closing a gap linux had relative to tui: the timestamp used to be absent on those five tombstone arms), `.message-selected` tints the bubble (`style.css`), and `detail.rs` scrolls the selected bubble into view via `compute_bounds`+`vadjustment` (mirrors `automation::agent::scroll_into_view`'s math, reimplemented since that module is compiled out of release builds). `test_search_local_index.py` green `--app linux` | ✅ 2026-08-26 — landed as a side-discovery of the search-result navigation trickle-down closing (`ui/search.md` § Implementation status today): `ConversationsViewModel.OpenThreadAndMessage` calls `SelectThreadAndMessage`, `DmMessageBubble.xaml.cs` paints the shared `selected` HelpText attribute on every render arm plus an amber `SelectionRing` border (cross-app-consistent with linux/android/macos), and `ConversationsPage.xaml.cs` scrolls the marked bubble into view via `StartBringIntoView()`. **Stays e2e-unverified end to end**, not because the paint is unproven but because windows has no local search arm registered yet, so no `Mail`-class row can reach its result list in a test run until that separate prerequisite closes; the paint itself fires by construction on every bubble render | ✅ 2026-08-25 — `SearchResultsView.swift`'s `Mail` arm now calls `selectThreadAndMessage` (upgrading the earlier plain thread-jump); `DmMessageBubble.swift`'s new `timestampLabel` paints the `selected` attribute on **every** render arm (deleted/legal-takedown/blocked/collapsed/muted/normal — mirrors tui/linux/android, closing the same tombstone-arm timestamp gap they each had — apple's `/element/attr` has no per-attribute-name map, so `selected` rides `automationValue`'s `value` closure, not a named `.attr()`), an amber `#F59E0B` stroke overlay tints the bubble (matches linux's box-shadow / android's ring), and `ThreadDetailView`'s new `ScrollViewReader` + explicit `.id(msg.messageId)` scrolls the selected bubble into view. `test_search_local_index.py` green `--app macos` (336s: real MTA delivery → client receive → local index build → search → select → scroll) | ✅ 2026-08-27 real-simulator-verified (upgrading the 2026-08-25 paint) — same shared `DmMessageBubble`/`ThreadDetailView`/`SearchResultsView` FaunaKit code macOS e2e-proved, plus the same one-line iOS routing edit. `test_search_local_index.py` still structurally cannot run on iOS (`CLIENT_BUILDS_INDEX` is `false` there, `libs/fauna-ffi/src/index_launch.rs` — a single-seat run seeds no local index to search regardless of the paint), so this is graded through the new `conversations_select_message` test-only command (`FaunaApp.swift`/`FaunaMacApp.swift`, calling the identical `ConversationsVM.selectThreadAndMessage` a real `SearchResultsView` row tap does) instead: `test_conversations_selected_message.py` seeds a thread past the fold and asserts the marked bubble is both correctly-identified and scrolled into view — green `--app ios` on a real booted simulator, and `--app macos` as a same-path cross-check. That grading pass also found and fixed a 2-day-old regression that had silently broken `data.conversation_threads` for every apple conversations e2e test on both targets (the e2e login path never activates a real `ConversationsSession`, so reading threads off `conversationsVM.session` — the earlier refactor — always returned empty; fixed by adding `ConversationsManager::conversation_threads_json`, a manager-level twin that works whether or not a session exists) | ✅ 2026-08-15 (Kotlin paint compile+Robolectric-verified; real e2e blocked fleet-wide on host emulator setup, same standing gap every android e2e track carries — not specific to this row) — `SearchNav::Mail` already routed to `selectThreadAndMessage`; `MessageBubble` gained an `isSelected` param, `MessageTimestamp` paints Compose's own `selected` semantics boolean on **every** render arm (mirrors linux/tui — closes the same tombstone-arm timestamp gap), `Modifier.selectedMessageMark` rings the bubble amber (`#F59E0B`, matches linux's `.message-selected`), and a `LaunchedEffect` drives `LazyListState.animateScrollToItem` to the selected message. `ConversationDetailContentTest.kt` — 6 new cases incl. the deleted-message arm — 75/75 green. Also fixed 2026-08-27, same bug class as the apple grading pass found in the ios cell above: `TestAgent.serializeState`'s `data.conversation_threads` read off `ConversationsManagerHost.session`, which a plain e2e run never activates, while `conversations_inject_inbound` injects into `.manager` — reading `[]` unconditionally under every ordinary android e2e test. Reads off `.manager` now, via the same manager-level `ConversationsManager::conversation_threads_json` the apple fix added; compile-verified only, same standing host-emulator gap | ✅ 2026-08-10 **(lead)** — the shared half (`select_thread_and_message`, `ThreadDetail.selected_message_id`, the read-time resolve) is in `libs/fauna-conversations` and is **done for all seven**; what the remaining apps owe is only the paint + the `selected` attribute on `dm-message-timestamp` + their own scroll-into-view. tui marks with an id-less `Element::chrome` line and lands the focus ring on the message (the viewport follows the ring). Follow-on captured in the per-app trickle-down NEXTs. **e2e-witnessed 2026-09-19:** `test_conversations_selected_message.py` green `--app tui` — its `conversations_select_message` test command dispatches the real `SearchNav::Mail` gesture (`search::open_result`), and the in-view read is the agent's `in-viewport` attribute, measured on the frame the terminal painted (`ui::PaintedPage`) — tui's registry, like GTK's, lists every row, so it cannot say "on screen" by omission |
| Toolbar wrap rule (`wrap_selection`) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (`markdown-toolbar` component over shared `wrap_selection`; **all six buttons since 2026-07-30** — heading/list joined as prefix-only marker pairs, so tui matches linux's full variant and web. `test_conversations_markdown_toolbar_wrap.py` green `--app tui` 5/5, all three arms mutation-verified) |
| Attachments **render** (receive) off `document` (D2) + `attachment_bytes` | ✅ (incl. FaunaMls blob fetch) | ✅ | ✅ | ✅ | ✅ | ✅ (e2e host-gated) | ✅ 2026-07-30 — `attachment_bubble_elements` reads the same `RenderBlock::Attachment` blocks and resolves each `blob_hash` through the shared `attachment_bytes`; an image leaf rasterizes via the `thumbnail::rasterize` path feed's `post-image` uses and degrades to a filename placeholder under the same id when bytes are absent/undecodable. The inject seam + `make_attachment_for_test` call were already wired — only the render was missing. `test_conversations_attachments.py` green `--app tui` |
| Attachments **outbound** (`attachment-button` → file picker → `manager.add_attachment`/`add_new_thread_attachment`) | ✅ 2026-07-20 (real `<input type="file">`, `+page.svelte` `stageAttachments`; mirrors the feed `compose-file` shape — Playwright drives it directly via `set_input_files`, no OS dialog involved) | ✅ 2026-07-20 (native `gtk::FileDialog` via `views/conversations/detail.rs` `.on_attach`; shared `fauna_conversations::compose::guess_mime_type` fills the mime the OS dialog doesn't hand back; UI-driven e2e proof via a `set_input_files` `target`-disambiguation seam — see § Attachments below) | ✅ (reference impl — real file picker → `AddAttachment`, `DmComposeBar.xaml.cs`; 2026-07-24: gained its own `set_input_files` `target`-disambiguation seam in `App.xaml.cs` (mirrors linux's), routing `attachment-button` to `ConversationsPage.Current.StageAttachment` instead of always falling through to feed's `compose-file` handler — so the outbound path is UI-driven-e2e-drivable at all, not just reference-quality; live e2e run still owed) | ✅ 2026-07-20 (shared `DmComposeBar` `.fileImporter` `[.item]` → `ConversationsVM.attachFile` → `manager.addAttachment`; **no automation `activate` registered**, matching feed's `ComposeAttachButton` — an OS panel can't be driven in-process, so the e2e path is the `compose.file` injection carrying `target: "attachment-button"`, mirroring linux's disambiguation seam; **UI-driven tier_3 e2e GREEN** — `test_conversations_attachments_outbound.py::test_macos_...`, with an anti-mock key-package guard that was falsified before being trusted) | ✅ 2026-07-20 (same shared `DmComposeBar` leaf + `attachNewThreadFile`; **UI-driven tier_3 e2e GREEN 2026-07-23** — `test_conversations_attachments_outbound.py::test_ios_...`, real-MLS-gated) | ✅ app-wiring landed 2026-07-20 (`ConversationDetailScreen.kt`/`NewThreadComposeScreen.kt`, `ActivityResultContracts.GetContent()` → `ExifStripper.strip` → `manager.addAttachment`/`addNewThreadAttachment`, mirroring the feed `compose-file` picker + the windows reference leg's EXIF-strip step; compile+Robolectric-verified); 2026-09-22: the test-agent seam landed — `TestAgent.kt`'s `compose.file[attachment-button]` arm reads the open composer off the shared snapshot (an active `new_thread_compose`, else `selected_thread_id` — linux's and apple's precedence), EXIF-strips, and calls the same `addAttachment`/`addNewThreadAttachment`, types from the shared `content_type_for_filename`; Robolectric-pinned against a real manager, and the outbound e2e legs are written. **A recorded e2e run is still open** — android has no run venue yet ([`../architecture/testing.md`](../architecture/testing.md) § Default app and nest mode → *Android's run venue*), its driver has no app-log reader for the real-rail control, and the file transport — built 2026-10-03: `drivers/android.py`'s `set_input_files` carries the picked file's bytes over the bridge (`POST /input-file`, written under the app's own cache dir by the same-uid instrumentation, basename kept) and hands the agent that device path, the `seed_credentials` crossing's shape — has not yet run on a device | ✅ 2026-07-30 — `attachment-button` is an `input_commit` **typed-path** control, not a picker: `apps/tui.md` § Declared platform absences 4 had already ratified "path entry with completion" as tui's replacement for an OS file picker, and feed's `compose-file` + `profile-edit-avatar`/`-banner` had shipped that shape twice. ⚠ **The "needs its own design pass" gate this cell carried from 2026-07-20 was STALE** — the three options previously left open (typed-path prompt? tab-completion? file-browser widget?) are verbatim what absence 4 already answers; it blocked the leg for ten days. Commits through the shared `add_attachment`/`add_new_thread_attachment` with `guess_mime_type`, on both composers, routed by `Mode`; an unreadable path or a closed composer reports on `error-message` rather than dropping (point 11) |
| Staged-attachment preview (`dm-compose-attachment-chip`/`-remove`, compose-side render of `ComposeState.attachments`) | ✅ 2026-07-21 (`+page.svelte` chip row beside the reply-recipient chips; `byteSize` value-format; tier_3 `test_conversations_compose_attachments.py --client web` GREEN — the first run hit a 30s locator timeout under severe host load (28-core box, load avg 26+), re-ran in isolation once load eased and it passed clean, confirming environmental flake not regression) | ✅ 2026-07-20 (`compose_bar.rs` chip row, mirrors `reply_chips_box`; `crate::i18n::byte_size`; tier_3 `test_conversations_compose_attachments.py --client linux` GREEN) | ✅ 2026-07-24 (the bare `AttachmentPreview` `TextBlock` is gone — `DmComposeBar.SetAttachments` chip row mirrors `SetReplyRecipients`'s imperative-`StackPanel` idiom exactly; staging moved from deferred-to-Send to immediate `add_attachment`/`add_new_thread_attachment` on pick so `remove_attachment`/`remove_new_thread_attachment` have something real to unstage — first windows caller of either mutator; also built windows' own `set_input_files` `target`-disambiguation seam (`App.xaml.cs`, mirrors linux's) so `attachment-button` is e2e-drivable at all; `@pytest.mark.windows` added to the e2e file. **Verified structurally only** (`FaunaApp.csproj` ARM64 Debug clean, `FaunaApp.Tests` 1169/1169 unaffected) — **not run live**, needs a cold windows-debug build + a real nest instance under FlaUI; if later found flaky, check the seam first, not this migration) | ✅ 2026-07-20 (apple lead — `DmComposeBar.swift` `attachmentChips`, first caller of `remove_attachment`/`remove_new_thread_attachment` anywhere; `swift-test` 292/292, e2e falsified-then-green) | ✅ 2026-07-20 (same shared `DmComposeBar` leaf; **e2e-proven 2026-07-23** — `test_conversations_compose_attachments.py --client ios` 2/2 GREEN) | ✅ 2026-07-21 (`ConversationsComposeBar.kt` chip row via FFI-free-injected `byteSize` lambda, mirrors `claimStatusLabel`/`providerStatusLabel` in `ProfileTiersTab.kt`; `ConversationDetailScreen.kt`/`NewThreadComposeScreen.kt` wire `onRemoveAttachment`; Robolectric `ConversationDetailContentTest` 67/67 incl. 3 new attachment-chip tests, JUnit-XML-verified; `@pytest.mark.android` added to the e2e file, real run still host-emulator-gated fleet-wide) | ✅ 2026-07-30 — one `attachment_elements` builder serves both composers (ui.yaml declares the ids on each); the chip names the file and its size through the shared `fauna_core::format::byte_size`, and its sibling × calls `remove_attachment`/`remove_new_thread_attachment` by `Mode`. `test_conversations_compose_attachments.py` green `--app tui` |
| Reactions + message delete render legs | ✅ | ✅ | ✅ (reference) | ✅ | ✅ | ✅ (e2e host-gated) | ✅ (`dm-reaction-pill`, `dm-message-delete-button` → `-confirm`, `mod.rs`) |
| Drafts v2 (shared `DraftsSync` restore + debounced save) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ 2026-08-04 — tui last of the 7, closing the row everywhere. `conversations/drafts.rs` over the same shared `DraftsSync` and the same `"conversations"` rail, wired from `session::establish` **outside** `start_conversations_session`'s MLS gate (drafts seal under `BackupKey`, so a failed engine init must not also cost the user their unsent compose — linux's `app.rs` ordering). tui's debounce needs no generation counter: the whole app is one tokio runtime, so a `select!` between the 1.5 s sleep and the next observer tick *is* the generation. ⚠ The prior `open` state was a tui-parity-flip miss — that commit bumped the claim from "All 6" to "All 7" mechanically, without checking tui |
| New-thread cancel + deactivate-preserves-draft | ✅ | ✅ | ✅ (canonical `new-conversation-cancel`) | ✅ | ✅ | ✅ | ✅ (`new-conversation-cancel`, `mod.rs`) |
| D2b `QuotedMessage` in-bubble quote | ✅ | ✅ | ✅ (2026-06-24 — doc was stale) | ✅ (2026-06-23 — doc was stale) | ✅ (shared FaunaKit — doc was stale) | ✅ | ✅ (`dm-message-quote`, `mod.rs:2078`) |
| D3 remote-image reveal via manager | ✅ | ✅ | ✅ (2026-06-09 — doc was stale) | ✅ | ✅ | ✅ | ✅ 2026-07-30 — `load-remote-content-button` gated on the shared `has_blocked_remote_images()`, dispatching `Action::RevealRemoteImages { msg_id }` → `manager.reveal_remote_images`, landed on `conversation_detail` alongside D4 link-previews. `test_conversations_remote_image.py` green `--app tui` |
| DM OS-toast firing (shared `MessageNotificationTracker`) | moved 2026-09-28 — the per-app row is [`../behavior/message-banners.md`](../behavior/message-banners.md) § Implementation status today | ↗ | ↗ | ↗ | ↗ | ↗ | ↗ |
| List-row last-activity time `conversation-item-timestamp` (approved 2026-09-25; shared `conversation_timestamp_display`, scoped within `conversation-item`) | ⏳ paints it untagged (`conversationTimestamp`, `+page.svelte`) — trickle-down owes the id | ✅ 2026-09-26 (tags the existing `list.rs` label) | ⏳ id owed | ✅ 2026-09-26 (shared FaunaKit `ConversationListRow`, tagged with `automationText`; rendered only while `last_activity_ms > 0`, so absent — not an empty label — otherwise) | ✅ 2026-09-26 (the same shared row) | ⏳ id owed | ✅ 2026-09-26 (lead, `mod.rs` list rows) |
| List filter (`set_search_query` shared filter) | ✅ | ✅ | ✅ | ✅ (doc was stale) | ✅ (doc was stale) | ✅ | ✅ (automatic — reads the manager's already-filtered `snapshot().threads`, same as every app) |
| Sort cycle (`next_sort_order` shared 3-way) | ✅ | ✅ (lead) | ✅ | ✅ (apple was last, 2026-07-18) | ✅ (apple was last, 2026-07-18) | ✅ (already matched the ratified cycle) | ✅ (2026-07-22 — `Action::Sort` now calls shared `next_sort_order`, `mod.rs`; the hand-rolled match is gone) |
| Membership/label failure on `error-message` (`ConversationsSnapshot.error` read; § Errors & edge cases) | ✅ 2026-08-02 (`membershipError` in `+page.svelte`, same precedence-first read; new `injectPageError` wasm seam on `WasmConversationsManager` — `libs/fauna-wasm/src/conversations.rs` — since none existed for any transport before this) | ✅ 2026-07-29 render (`page_error_text` in `detail.rs`) — **e2e-pinned 2026-08-02**: the render existed but no test-agent command did, so `test_conversations_compose_error.py` had no way to drive it; added `handle_conversations_inject_page_error` (`main.rs`, mirrors `handle_conversations_inject_send_failure`) | ✅ **read** 2026-08-03 (`ConversationsViewModel.ActiveSendErrorReason` reads `Snapshot.error` first, falls back to `send_state` — same precedence, pinned by a real-FFI VM test in `ConversationsViewModelTests.cs`, 1265/1265 green) and the previously-missing test-agent command was added (`conversations_inject_page_error`, `ConversationsCommands.cs` + `TestAgent.cs`, mirrors `conversations_inject_send_failure`) — **e2e GREEN 2026-08-04**: `test_failed_membership_op_surfaces_on_error_message` PASSED `--app windows` (marker widened), once the blocker found on 2026-08-03 was fixed. That blocker was worth its own fix rather than a workaround: **three** surfaces wore the `error-message` id at once (`MainPage.GlobalErrorText`, 24 per-page 1×1 `ErrorTextMirror` TextBlocks, the page's own InfoBar) and the first two were unconditionally on-screen, so `is_visible("error-message")` was structurally true on a clean page and the opening negative assertion could never fail. Shims now leave the UIA tree unless they carry a message (`MessageShim.ShouldShow`); `ConversationsPage`'s bar was also the lone `ConversationsError` outlier (renamed `ErrorBar`) and never published `App.CurrentErrorMessage`, so `error_text()`/`has_error()` had read empty for a real conversations error | ✅ 2026-08-02 (`ConversationsVM.pageError` — shared FaunaKit — reads `snapshot.error` first, falls back to send state; wired via `ErrorBanner` in `MacConversationsView.swift`; pinned by `ConversationsPageErrorTests.swift`, 326/326 swift-test green) — e2e proof still owed, the marker set below doesn't cover macos yet | ✅ 2026-08-02 (same shared `ConversationsVM.pageError`, wired via `ErrorBanner` in `ConversationsListView.swift`) — e2e proof still owed | ✅ render (`ConversationDetailScreen.kt`: `pageError = snapshot?.error` feeds the `error-message` precedence chain — receive-stopped, then the page error, then a failed send; this cell read `open` until a code check 2026-09-19) — e2e proof still owed | ✅ 2026-07-29 (lead — `sync_page_error` reads the snapshot error and resolves it through `wizard::localized`). **e2e covers tui, linux, web and windows** — `test_conversations_compose_error.py::test_failed_membership_op_surfaces_on_error_message` widened 2026-08-02 (tui/linux/web) then 2026-08-04 (windows); all four confirmed green (web's run was initially blocked by an unrelated SPA boot-time effect loop; fixed and landed since) |
| Send failure on `error-message` **localized** (`SendState::Failed { reason: LocalizedText }` resolved, not painted; § Errors & edge cases) | ✅ 2026-07-29 (`resolveLocalized`) | ✅ 2026-07-29 (`lt.resolve(i18n::strings::lookup)`) | ✅ **the resolve itself** — `ConversationsViewModelTests.cs`'s real-FFI VM test proves `Strings.Resolve` correctly renders the backend detail, not the raw key (1265/1265 green). **and the live e2e proof, GREEN 2026-08-04**: `test_failed_send_surfaces_on_error_message` PASSED `--app windows`. It had been CONFIRMED RED 2026-08-03 (never actually run before — the 2026-07-29 landing was "verified by review only, no MSBuild there"), failing at its very first assertion (`error-message` must start hidden) for a reason unrelated to this row: the `error-message` AutomationId collision, now fixed — see the row above | ✅ 2026-07-29 (`renderLocalizedText`) — **edit verified by review only** (no Xcode on the box it landed from) | ✅ 2026-07-29 (same shared FaunaKit sites) — **edit verified by review only** | ✅ 2026-07-29 (`localized(…)`; `compileDebugKotlin` + both Robolectric classes green, 80 cases) | ✅ 2026-07-29 (`wizard::localized`) |
| Live self-address (shared cell + `set_self_address`; § State & data shape → *Self-address: live, never baked*) | ✅ 2026-08-02 (identity-store subscription → `setSelfAddress`; the race-window `''` build now heals when the handle lands) | ✅ 2026-08-02 (`IdentityRefreshed` handler → `set_self_address`; `mail_sink.rs::ensure_smtp_backend` retired — the cell heals BOTH rails, incl. the MLS routing domain the old glue never touched) | ✅ 2026-08-05 — domain source fixed 2026-08-03 (`BuildConversationsSessionAsync`'s `identityDomain` param, sourced from `LaunchMachine.Snapshot().identity?.domain`, `?? ""` when unresolved); **heal closed 2026-08-05**: all four `App.xaml.cs` `LaunchMachine` construction sites now wire a real `SelfAddressHealObserver` (was `NullLaunchObserver()`) instead of a private refresh RPC, mirroring linux's `IdentityRefreshed` / tui's `SelfAddressRefreshed` / android's `AppLaunchVM.applyIdentity` — the observer is created before its machine (settable `Machine` property, since the machine's constructor takes the observer), and `StartMainAppAsync` attaches the freshly-built `ConversationsSession` via `AttachSession`, which also applies whatever identity already resolved in the gap between machine construction and session build. `OnChanged` re-reads `Machine.Snapshot().identity` and pushes `SetSelfAddress` on every resolved/changed value (deduped against the last-applied address), so a later rename or late-resolve reaches the live session with no re-navigate. `FaunaApp.Tests` unit-covers the observer's decision logic (apply / skip-while-unresolved / dedup / apply-on-late-attach) against fakes — no cross-app e2e mechanism exists yet for a live server-side handle rename (none of the other 6 apps built one for this row either) | ✅ 2026-08-03 — `completeAuthenticatedLaunch` already sourced `handle`/`domain` from `FaunaAccounts.registry(keychain:).sessionMaterial(actorId:)` (the freshly-verified per-account row, never the nest URL host or a stale mirror), and every re-dispatch (`trustNestIdentity()`) already rebuilds the whole session with it — so the only gap was the heal itself: `conversationsVM.session?.setSelfAddress(selfAddress:)` now fires first, healing an already-live session without a full rebuild | ✅ 2026-08-03 — same fix as macos, plus the domain source itself: `completeAuthenticatedGlue` read the legacy single-slot mirror (`keychain.load(.cachedDomain)`, refreshed only once at boot), so a first login or a mid-session rename could still build (or heal) off a previous run's domain. Switched to the same `FaunaAccounts.registry(keychain:).sessionMaterial(actorId:)` read macOS already used (priority #4 — the richest existing pattern, lifted), plus the same `setSelfAddress` heal | ✅ 2026-08-03 — the design call was taken as **shared Rust** (priority #2): `LaunchSnapshot` now carries the nest-confirmed `identity` (handle/domain/tier), so the live channel android lacked exists for **every** native app. `AppLaunchVM` collects it, refreshes the `cachedHandle`/`cachedDomain` caches nothing used to write, and pushes `<handle>@<domain>` into the live session | ✅ 2026-08-02 (lead with linux — `SelfAddressRefreshed` message → `set_self_address`; `conv_backend::ensure_smtp_backend` retired) |
| Dead receive rail on `error-message` (`ConversationsManager::receive_stopped`, web's stalled pump; § Errors & edge cases) | ✅ 2026-09-13 — `$lib/receive-pump` bounds each rail's pass and reports a stall; `receiveRailStalled` ranks directly under the role refusal in `+page.svelte`; pinned by `receive-pump.test.ts` | ✅ 2026-09-14 — `page_error_text` (`detail.rs`) gained a `receive_stopped: bool` param, ranked directly under `served_elsewhere` and above `snapshot.error`/a failed send; threaded from `ConversationsManager::receive_stopped()` at the render call (`refresh` in `mod.rs`, beside `engine_served_elsewhere()`). No FFI seam needed — `begin_receive_loop`/`mark_receive_stopped` are already crate-`pub` outside the uniffi-export impl, so the unit test drives the manager directly, same as tui. Pinned by `receive_stopped_outranks_snapshot_error_and_a_failed_send_but_not_served_elsewhere` + `a_newer_receive_loop_retires_a_stopped_rails_notice` (`detail.rs::page_error_tests`, mirrors tui's `sync_page_error_surfaces_a_stopped_receive_rail_and_no_success_fold_clears_it`), 8/8 green. `conv_receive_cycles` already carries `exit` (`main.rs` via the shared `state_json::conv_receive_cycles_json`), so the checker's `receive-loop-alive` invariant needed no change | ✅ 2026-09-14 — `ConversationsViewModel.ReceiveStopped` (new bool property, same FFI-fault-degrades-to-false posture as `EngineServedElsewhere`) reads `_manager.ReceiveStopped()` and `ActiveSendErrorReason` now ranks it directly under `EngineServedElsewhere`, above `Snapshot.error`/`send_state`. Bindings regenerated via `just windows-ffi-test` (the `test-helpers` flavor now also carries `MarkReceiveStoppedForTest`, mirroring apple's `mark_receive_stopped_for_test` seam). The `conv_receive_cycles` publisher's two no-session/decode-failure fallbacks in `App.xaml.cs` now include `"exit": null` alongside `started`/`completed`, so the checker's `receive-loop-alive` invariant stops reading `n/a` on windows once the native DLL carries the shared `exit` field — no shim change needed, `ConvReceiveCyclesJson()` already passes it through. Pinned by `ConversationsViewModelTests.cs`'s `ActiveSendErrorReason_receive_stopped_outranks_a_standing_page_error_but_not_served_elsewhere` + `ActiveSendErrorReason_receive_stopped_is_not_cleared_by_an_unrelated_success` (mirrors apple's `ConversationsPageErrorTests.swift`), 1704/1704 green | ✅ 2026-09-13 — `ConversationsVM.pageError` (shared FaunaKit) now reads `manager.receiveStopped()` under `engineServedElsewhere`, ranking it above the snapshot-error/send-state truths; no gesture clears it (only a newer receive-loop generation does). Bindings regenerated via `just apple-ffi`'s test-helpers flavor (a new `mark_receive_stopped_for_test` UniFFI seam, mirroring `inject_page_error_for_test`, since `mark_receive_stopped` is deliberately outside the always-exported impl). Pinned by `swift-test`'s `pageErrorRanksReceiveStoppedAboveTheSnapshotErrorAndBelowServedElsewhere` + `pageErrorReceiveStoppedIsNotClearedByASuccess` (`ConversationsPageErrorTests.swift`, 584/584 green) | ✅ 2026-09-13 (same shared FaunaKit leg as macOS — one Swift change, both targets) | ✅ 2026-09-14 — `ConversationDetailScreen.kt`'s new `receiveStopped: Boolean` param on `ConversationDetailContent` reads `manager.receiveStopped()` and ranks it TOP inside `pageErrorReason` — android has no `engineServedElsewhere` arm (by design), so this is the highest-precedence truth here, above `pageError` and a failed compose-send, unlike windows/apple which read served-elsewhere first. No binding regeneration needed (`receiveStopped()` already present in both debug and storeSafe generated Kotlin). `conv_receive_cycles`'s pre-session/decode-failure fallback in `TestAgent.kt` now includes `"exit": JSONObject.NULL` (mirroring windows's fix) — the live path already passed `exit` through via the shared UniFFI twin. Pinned by `ConversationDetailContentTest.kt`'s `receiveStoppedOutranksAPageErrorAndAFailedComposeSend` + `receiveStoppedIsShownWithNoOtherErrorPresent` (mirrors apple's `ConversationsPageErrorTests.swift`); `compileDebugKotlin` green. Real e2e stays gated on the host emulator setup, the standing android gap every other row here carries | ✅ 2026-09-13 **(lead)** — `sync_page_error` ranks it under `served_elsewhere`; set by the shared `supervise_receive_loop`, pinned by `receive_cycle_poke_tests.rs` and tui's `sync_page_error_surfaces_a_stopped_receive_rail_and_no_success_fold_clears_it` |
| Skipped unopenable mail on `error-message` (`ConversationsManager::unopenable_mail_count`, ranked last; § Errors & edge cases) | ✅ 2026-09-16 — `displayError`'s chain now falls through an empty `pageError` to `unopenableMail` via an extracted, unit-testable helper (`conversationsDisplayError`, `$lib/conversations-display-error.ts` — kept import-free so `deno test` can load it without pulling in `$lib/conversations`'s svelte-store/wasm graph; `''` is not nullish, so a bare `??` chain can never reach the floor arm past it, only `||` can); `unopenableMailCount` reads on the same refresh the snapshot rides (`refreshConversations`). Pinned by `conversations-display-error.test.ts` (7 precedence cases) | ✅ 2026-09-16 — `page_error_text` gained an `unopenable_mail: u32` param, ranked last, threaded from `manager.unopenable_mail_count()` in `mod.rs::refresh`; substitutes through the generated `conversations::errors::mail_unopenable`. Pinned by `page_error_tests::unopenable_mail_shows_only_when_nothing_else_present` + 2 outranking tests | ✅ 2026-09-21 — `ConversationsViewModel.ActiveSendErrorReason` gains a last-ranked arm on a new `UnopenableMailCount` read (`_manager.UnopenableMailCount()`; an FFI fault degrades to 0, the posture of the `EngineServedElsewhere`/`ReceiveStopped` reads above it), substituted through `Strings.Format("conversations/errors/mail_unopenable", n)`. One arm covers both panes — windows' `error-message` is page-level (`ConversationsPage.Refresh` publishes `ActiveSendErrorReason` once to `App.CurrentErrorMessage`), so unlike android there is no separate list-screen surface. Pinned by six `ConversationsViewModelTests.ActiveSendErrorReason_…unopenable_mail…` cases (count substituted through a `FakeLocalizer` carrying the verbatim resw string; a failed send, a standing page error, a dead rail and the role refusal each outrank it; no gesture clears it), driven through the existing `note_unopenable_mail_for_test` seam. `test_mail_client_receive_after_reenable.py --app windows` real-MTA-verified green (687s incl. cold build) | ✅ 2026-09-16 — `ConversationsVM.pageError`'s floor arm reads `manager.unopenableMailCount()`, substituted through `L.conversations.errors.mailUnopenable(count:)`; a new test-helpers seam `note_unopenable_mail_for_test` (`ConversationsManager`, `manager.rs`) drives it without a real unopenable record, since `MailFeed` has no `uniffi` derive and cannot cross FFI. Pinned by `pageErrorSurfacesUnopenableMailWhenNothingElseIsStanding` + `pageErrorRanksUnopenableMailBelowEveryOtherTruth` (`ConversationsPageErrorTests.swift`, shared by both apple targets). `test_mail_client_receive_after_reenable.py --app macos` real-MTA-verified green (335s) | ✅ 2026-09-16 — same shared FaunaKit change as macOS (one VM, one pinning test file, both targets close together). `test_mail_client_receive_after_reenable.py --app ios` real-simulator-verified green (104s), after a full `just apple-ffi-test` (3-slice release, test-helpers) rebuild for the iOS-simulator slice | ✅ 2026-09-16 — `ConversationDetailScreen.kt`'s `pageErrorReason` gains a last-ranked arm on `manager.unopenableMailCount()`; the mobile-collapsed **list** screen (`ConversationListScreen.kt`) surfaces the same notice directly too (its own, simpler read — the list has no other page-error producer to rank against), since a user who never opens a thread only ever sees the list and the e2e drives exactly that path. Pinned by `ConversationDetailContentTest.kt` (3 new cases) + `ConversationsListContentTest.kt` (2 new cases); host-target `testDebugUnitTest` green | ✅ 2026-09-15 **(lead)** — `sync_page_error`'s floor arm reads `unopenable_mail_count()` and substitutes `{count}`; the shared page opener (`NestMailInboundSource::fetch_page`) carries a skip per unopenable record and `poll_inbound_mail` counts it. Pinned by `sync_page_error_surfaces_skipped_unopenable_mail_below_every_other_error` (tui), `poll_skips_an_unopenable_record_and_keeps_receiving` (`smtp_backend_tests.rs`) and `keyset_opener_reads_pre_rotation_standing_mail_within_the_grace_window` (`fauna-mail`); e2e `test_mail_client_receive_after_reenable.py` proves the mailbox keeps receiving past a record a re-enable made unopenable |

Known residuals: **outbound attachment staging is built on all SEVEN apps** —
windows (reference), web + linux + android (2026-07-20), macos + ios (2026-07-20),
tui last (2026-07-30). ⚠ **tui's cell carried "needs its own design pass" for ten
days and that gate was stale**: the question it posed (how does a terminal choose a
file, with no OS picker?) was already answered by `apps/tui.md` § Declared platform
absences 4 — "replaced by path entry with completion and a file-browser widget" —
and shipped twice, on feed's `compose-file` and `profile-edit-avatar`/`-banner`. The
lesson generalises past this row: **a "needs a design pass" claim about an app is
worth checking against that app's own architecture doc before believing it**, which
is the same rule the project applies to "this needs a human". **Privacy metadata is stripped at the shared staging seam since
2026-07-20** — `ConversationsManager::stage_attachment` runs
`fauna_media::process::strip_metadata` before hashing, so every app gets it
without remembering (see § Attachments → *Privacy metadata* below for why the
per-app shape was retired); **web, linux, macos and ios have a UI-driven e2e proof of the
send-with-attachment path** (iOS's leg proven 2026-07-23);
android's app wiring is compile+Robolectric-verified
only (host-emulator-gated, same as every android e2e track); linux (2026-07-20)
and windows (2026-07-24) solved the harder "a native OS file-chooser dialog
isn't driver-automatable" gap via a `set_input_files` `target`-disambiguation
seam (extends feed's existing `compose.file` → `upload_blob()` bypass so the
agent can tell conversations' `attachment-button` apart from feed's
`compose-file` — see § Attachments below); **tui needed no such seam** — its
`attachment-button` is a real typed-path input, so the driver patch drives the
production control directly rather than bypassing an undrivable dialog, and the
`target` disambiguation it reuses was already in `apply_compose` for profile's
avatar/banner;
**The coverage was injection-only until 2026-07-31 — web/linux/tui now
also have a genuine non-injected proof.** Every existing send-failure e2e
(`test_conversations_compose_error.py` and friends) drives
`ConversationsManager::inject_send_failure_for_test`, since a mail-OFF nest
has no on-demand real send failure to trigger — so none of them could rule out
`error_text()`'s test-helper short-circuit (`actions/__init__.py` returns `""`
without reading the element when `messages.error` is `null`) masking an app
that stays silent on a REAL backend rejection.
`test_real_faunamls_send_with_oversized_attachment_surfaces_on_error_message`
(`test_conversations_attachments_outbound.py`) closes that gap for FaunaMls
attachment sends on web/linux/tui: an attachment over the nest's blob-upload
body cap is a genuine 413/400 a real nest issues on a real multipart POST (no
mutation, no seam), and the element is read directly
(`driver.is_visible`/`get_text`), proving the render path is not merely
correct-when-injected. windows/macos/ios remain injection-only proven, same
residual as every other per-app follow-on in this doc;
**staged-attachment preview/remove UI landed on apple (macOS + iOS) 2026-07-20 —
the first app anywhere to render `ComposeState.attachments`** (shared
`FaunaKit/Views/DmComposeBar.swift`, so both composers and both platforms inherit
one chip row; `dm-compose-attachment-chip` + `dm-compose-attachment-remove`, IDs
user-approved the same day, mirroring the `dm-reply-recipient-chip`/`-remove`
prior art), which also makes it the first caller anywhere of the long-built
`remove_attachment`/`remove_new_thread_attachment` mutators; linux (2026-07-20),
web + android (2026-07-21) and windows (2026-07-24) landed their own chip rows
since, and tui carries both outbound staging and the chip (the row above; the
chip-row builder in `apps/fauna-tui/src/conversations/mod.rs`) — every app with
outbound staging now paints the chip —
see § Attachments → *Staged-attachment preview* below; attachment `c2pa` at receive time is now
a real per-attachment verdict on every native app — BUILT 2026-08-17, § Attachments "C2PA
on-device" below — and stays a genuine `false` stub only on web (wasm never enables
`c2pa-detect`); the
keypackage replenish
model is now **fully unified** on the session's top-up-to-20 — the former
per-app android/apple 5→10 page-load floors are deleted (android
2026-07-14, apple 2026-07-16; both Settings pages now
show a read-only count, never an auto-mint trigger) — corrects this doc's own
stale "declared drift" framing
([`../behavior/direct-messages.md`](../behavior/direct-messages.md)
§ Key Package Management).

**Windows RichEditBox applier constraints (load-bearing; history in git):** the
decoration pass runs **deferred + debounced** (~150 ms `DispatcherQueue` timer)
— never re-entrantly inside the native keystroke insert (hang/`E_BOUNDS` class);
`CharacterFormat` must be **set back** onto the range; font sizes convert
**DIP→points**; decoration **defaults on** (the new-thread composer never
receives `SetCapabilities`); built-in rich-formatting affordances are disabled
(`DisabledFormattingAccelerators.All` + `SelectionFlyout = null`) so the shared
toolbar is the only formatter; the control carries a custom `IValueProvider`
peer (`MarkdownRichEditBoxAutomationPeer`) for the e2e ValuePattern; the
per-keystroke hang has **no headless CI gate** (validated manually — the
harness can't inject real key input).

## Layout & flow

Per spec section 3.

**List pane (320px on desktop, full-width on mobile).**

- Top strip: page heading, `new-conversation-button` (`+`), `conversation-sort` (`↕`).
- Search box.
- Conversation rows (component `conversation-list-item`, indexed):
  per-row `dm-unread-indicator`, label, snippet, timestamp (`conversation-item-timestamp`),
  `protocol-icon` (rail glyph). Mixed across rails, sorted by activity.
  The row timestamp is the shared contextual last-activity formatter
  (today → local clock, Yesterday, a weekday, else a date) — see
  [value-formatting.md](../behavior/value-formatting.md) § Conversation timestamp;
  apps must not hand-roll it.

**Detail pane.**

- `thread-header` strip (component): label, `protocol-icon`,
  `thread-member-chip` (indexed; **one chip per participant — the full set,
  never truncated**: ratified 2026-08-28 — the
  "first 3 + +N expand" this line carried from its 2026-05-10 authoring was
  built as a cap-without-expand on android alone and never as an expand
  anywhere, and it hid a flagged member's review pair behind the overflow; the
  pair below is why every member must be visible in place, and any future
  overflow affordance is a new ui.yaml element needing approval, not a
  re-reading of this line) — a member carrying an
  open post-succession review item also shows
  `thread-member-unattested-mark` + `thread-member-keep-button` **scoped inside
  that chip**, the load-bearing rendering of one flag whose whole ruling
  (including why *Remove* is the chip itself and no second affordance) is owned
  by [`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md)
  § Propagation → *MLS groups*,
  `thread-add-participant-button` (capability-gated),
  `thread-rename-button` (MLS groups only), overflow menu with
  "Show full headers" for email-shaped threads.
- `MessagesList`: scrolling stream of `dm-message-bubble` (component).
  Bubble carries `dm-sender`, badges (`encrypted-badge` / `signed-badge` /
  `verified-badge` are page-level text elements; `c2pa-badge` and
  `content-label-badge` are standalone components used in
  `conversation_detail` — indexing/scope per ui.yaml, which owns it),
  `dm-message-text` (indexed, rendered by walking `MessageSnapshot.document` —
  the producer routes inbound mail HTML through `Markdown`, with a per-message
  `load-remote-content-button`; see [html-mail.md](../behavior/html-mail.md)),
  `dm-attachment-image` (indexed) and `dm-attachment-file` (indexed),
  `dm-message-timestamp` (indexed — the per-message contextual time via the
  shared `fauna_core::format::conversation_timestamp_display`, the same bucketer
  the conversation-list row uses; [value-formatting.md](../behavior/value-formatting.md)
  § Conversation timestamp), `dm-reply-button` (indexed).
- `subject-divider` (indexed) renders **between** bubbles when a
  message's `subject_line` differs from the previous message's
  effective subject. Keeps merged threads readable when senders change
  subject mid-flight.
- `dm-compose-bar` (component) at the bottom: optional reply preview
  (`dm-reply-preview` + `dm-reply-cancel`); optional `subject-input`
  (visible iff `compose.subject_draft: Some(_)`); `dm-text-field`
  (multiline body); toolbar row with `topic-toggle-button` (left),
  markdown toolbar (`markdown-bold-button` etc. — now also enabled on the
  mail rail; markdown→HTML on send, see
  [html-mail.md](../behavior/html-mail.md)), `attachment-button`,
  `dm-send-button` (right).

**New-thread compose lives in the detail pane, not a modal.** Clicking
`new-conversation-button` swaps the detail pane for a `recipient-picker`
component (`recipient-picker-input`, `recipient-picker-chip` indexed,
`recipient-picker-suggestion` indexed, `recipient-resolve-status`)
above the same compose bar. No `NewConversationDialog` ContentDialog or
equivalent.

**Mobile collapse.** On Android and iOS, list pane and detail pane are
separate screens. Tap `conversation-item` → push detail screen with
back button. Element IDs are identical across form factors;
`driver.is_mobile()` branching lives in the action layer
(`architecture/e2e-conventions.md` § Cross-app e2e conventions, point 3), never in
test files or app code.

**Empty states.** No thread selected → detail pane is hint text. New
compose active → detail pane shows recipient picker. Thread with zero
messages → header + compose bar visible, bubble area blank.

### Compose-field inline markdown styling (ratified 2026-06-13; marker default → hidden + per-editor toggle, 2026-06-28, user)

The body field (`dm-text-field`) renders **inline markdown styling** as the
user types — the Obsidian / Typora / iA-Writer "live preview" model,
**not** WYSIWYG. `**bold**`, `*italic*` / `_italic_`, `***both***`,
`` `code` ``, `# `…`#### ` headings, `> ` blockquotes, and `- `/`1. ` list
items appear **visually styled** in the editor (bold/italic font, monospace,
larger heading text, quote indent), while the markdown **markers stay in the
buffer**. Uniform across the six GUI apps (priority #1); the tui's terminal
composer shows the markers literally (a terminal cell grid has no live-preview
type styling — the toolbar wrap buttons are shared, the decoration engine is
GUI-only). **Corollary, made explicit 2026-07-30: `markdown-marker-toggle-button`
is therefore a declared absence on tui, not a pending rollout leg** — it flips
*between* the two decoration modes below (hidden vs. dimmed markers), and an app
with neither mode has nothing to flip. The six wrap buttons are unaffected and all
six ship on tui; § Implementation status today carries the per-app cells.

> **Update (2026-06-28, user — flips the marker default + adds a toggle).** Inline
> emphasis markers (`*`/`**`/`` ` ``/link-bracket syntax) now **default to HIDDEN** — concealed
> and revealed only at the caret *edge* (the Notes editor's inline hide engine over
> `fauna_core::markdown::inline_reveal_ranges`), not dimmed. A **per-editor toolbar
> toggle** (`markdown-marker-toggle-button`) flips *this* editor back to the dimmed
> live-preview described below (markers shown, whole-line reveal) — Obsidian
> source/preview style; **default hidden**, no global setting / no nest persistence.
> The detailed description below is now the **"show markers" (dimmed) mode**; the
> hidden default reuses the **inline half** of the Notes hide engine — **structural**
> markers (`- `, `# `, `> `) stay dimmed in compose (no chrome, no widgets, no
> gestures) and **Enter still sends** (a one-shot message is not a structured
> document; the structural gestures stay Notes-only). The styling *carries* the "this
> is markdown" signal, so the asterisks are redundant by default; the toggle exists
> for the cases the glyph doesn't show (link URLs, accidental `*literal*`). Rationale,
> the three-axis (experience / keyboard / storage) analysis, and the cross-app
> rollout are tracked internally (2026-06-28).
> The **"show markers" (dim) mode**'s caret-line reveal rule is shared:
> `fauna_core::markdown::compose_show_markers_dim_ranges(src, caret)` returns the marker byte-ranges
> to dim (every marker whose source line differs from the caret's line; markers *on* the caret's line
> reveal at full opacity) — the whole-line counterpart of `compose_decoration_plan`'s per-run hide
> reveal, so native appliers never re-derive "is this marker on the caret's line".
> Per-app adoption of hide-by-default + the toggle + the dim rule: § Implementation status today.

This is **decoration over a plain-text document**, which is what keeps it
distinct from WYSIWYG and preserves every invariant:

- **The buffer literally holds markdown source.** `hello *world*` is exactly
  what's stored, drafted, copied, and sent — caret, selection, and undo all
  operate on that string. There is **no rich-text tree and no
  serialize-on-send boundary**, so the WYSIWYG round-trip-fidelity bug class
  does not exist. (`markdown→HTML on send` is unchanged — see
  [html-mail.md](../behavior/html-mail.md) § Composition.)
- **The toolbar still inserts markers.** `markdown-bold-button` etc. still wrap the
  selection — identical on all seven apps, unchanged by this feature. Styling is
  *additive*; the action is still "insert `**`/`_`". (The wrap *rule* itself now lives
  in shared Rust — `fauna_core::markdown::wrap_selection` via `wrapMarkdownSelection` —
  so edge whitespace stays outside the markers; see the App glue list below. On
  `dm-text-field` specifically, apple's wrap is **selection-aware** (`MarkdownCompose.wrap`
  over the `NSTextView`/`UITextView` representable's `selectedRange`) — the empty-selection
  case (`MarkdownCompose.insertion`, a stale doc claim once made here too) only remains on the
  feed composer's separate `MarkdownToolbar`, a different page; see *Implementation status*
  below.)
- **A decoration layer styles source ranges.** On each edit, the app
  tokenizes the buffer and applies *visual* styles to byte ranges: content
  ranges get the style (italic/bold/code/heading/…); the marker characters
  (`*`, `` ` ``, `#`, `>`, `-`/`1.`, and the link-bracket syntax) are **dimmed/concealed**.
- **Markers reveal near the caret.** Markers on the caret's **current line**
  (the line being edited) are shown un-dimmed so they can be edited; markers on
  every other line are dimmed/concealed so the user sees just the styled text —
  the "source on the active line" model of Obsidian Live Preview. Start with
  **dim**, not true-hide, to avoid caret-offset surprises. (Per-line is the
  linux-lead baseline; an app may refine to per-styled-span reveal — the
  shared `decoration_map` returns every marker range, so the reveal granularity
  is a pure app-glue choice.)

**Known limitation:** decoration is computed **per source line**, so an
emphasis run that spans a hard line break (`**bold\nmore**`) is not styled in
the editor even though the renderer (which joins paragraph lines) would bold
it. Single-line emphasis — the overwhelming common case — is exact. Multi-byte
input (emoji, accents) is handled: the shared API returns **byte** ranges and
each app converts to its native offset unit.

**Why inline-styling, not WYSIWYG** (the *architecture* — decoration over a
plain-text markdown buffer, no rich-text tree — settled with the user 2026-06-13
and **not** reopened; only the marker-visibility *default* flipped to hidden +
gained a toggle on 2026-06-28, see the Update banner above): WYSIWYG would mean
six divergent rich-text edit layers
(RichEditBox/TextKit/Compose/ProseMirror), a perpetual round-trip test burden,
a new paste/XSS sink on web's `contenteditable`, and a heavy web rich-text
dependency in a no-Node/Deno, supply-chain-cautious codebase — to buy only
"zero visible markers ever", a nicety the user did not require. Inline-styling
keeps the markdown wire format, the shared toolbar, near-zero new security
surface, and puts the hard part (tokenizing styled vs. marker source ranges) in
**shared Rust** where every app consumes one definition.

**Implementation status.** Shared Rust is landed: `fauna_core::markdown::decoration_map`
behind the ungated `FfiMdDecoration` / `decorationMap` UniFFI face (native) + the `decorationMap`
wasm face (web), and the `wrap_selection` toolbar rule behind `wrapMarkdownSelection`. **Decoration
appliers are built on all six GUI apps** (the tui deliberately shows markers literally —
a terminal composer has no live-preview styling; its toolbar wrap buttons consume the same
shared `wrap_selection` rule) — linux (lead, `gtk::TextTag`s in `compose_decoration.rs`),
android (Compose `VisualTransformation`, `MarkdownCompose.kt`), macOS + iOS (one shared FaunaKit
`MarkdownTextEditor.swift` `NSTextView`/`UITextView` attributer, byte→UTF-16 conversion; its
representable also makes the apple toolbar wrap **selection-aware**, clearing the old iOS-17 caveat),
windows (`MarkdownRichEditBox` — the load-bearing constraints live in § Implementation status today),
and web (`markdown-decorations.ts` + `MarkdownEditor.svelte`, `compose-hide.test.ts`). The
toolbar-wrap migration is complete on all six. The one dedup follow-on: the macOS-only feed-composer
`MarkdownToolbar` still uses the empty-selection `MarkdownCompose.insertion`. e2e:
`tests/e2e-unified/tests/test_compose_markdown_styling.py` (tier_3). Per-app hide-default/dim-rule
adoption: § Implementation status today.

### Participants vs. reply recipients (ratified 2026-06-07, user)

Two distinct concepts that earlier implementations conflated into the header
`thread-member-chip`s (which were click-to-remove — an incoherent, unmarked
destructive action on a mail thread, since you can't un-send who an email went
to):

- **Thread participants** — the *historical* From/To/Cc set of the thread.
  **Informational, not mutable** on mail. Header chips are non-removable;
  clicking a chip reveals the full address / contact (never removes). For mail
  the rail capability `supports_membership_change` is **false** (so the
  add-participant button is hidden and chips are non-removable); for FaunaMls it
  stays true (group membership is real). One source of truth:
  `fauna_conversations::derive_capabilities`.
- **Reply recipients** — the To/Cc of the message you are *about to send*. A
  per-reply **draft**, editable, defaulting to reply-all-minus-self. Rendered as
  an **always-visible "To" line** in `dm-compose-bar` (removable recipient chips
  + an add affordance) when the rail has the new capability
  `supports_recipient_selection` (mail: true; FaunaMls: false — recipients are
  the group). Removing a reply chip drops that recipient from **this reply
  only** — thread history is untouched. `dm-reply-button` seeds reply
  (sender-only); `dm-reply-all-button` seeds reply-all; either is then editable.

#### Implementation status
**DONE on all seven apps (web landed last — `dm-reply-all-button`
`+page.svelte:611`, the editable To line + chips over the shared manager).**
The shared model
lives in `fauna-conversations`: `ThreadCapabilities.supports_recipient_selection`
(mail true; FaunaMls/social false — `capabilities.rs`), `ComposeState.reply_recipients`,
the manager actions `start_reply(id, msg_id, reply_all)` (reply = the replied
message's sender only; reply-all = every participant but self, dropped via
`RailBackend::self_address()`) + `add_reply_recipient` / `remove_reply_recipient`,
and the SMTP backend sends to `reply_recipients` when set (falling back to the
historical participants when empty, so a plain send still addresses the thread).
The **linux** app (and **macOS + iOS**, via one shared `FaunaKit`
change — `DmComposeBar` renders the To line, `DmMessageBubble` the
`dm-reply-all-button`, `ThreadHeader` the tap-to-reveal-address chips) renders
the always-visible editable To line
(`dm-reply-recipient-chip` + `-remove` + `-add`, from `compose.reply_recipients`)
and the `dm-reply-all-button`, both gated on `supports_recipient_selection`; mail
header chips are now click-to-reveal-address (non-removable since the membership
flip). **windows** is lifted too (2026-06-08), through the same shared manager
flow (`start_reply` / `add_reply_recipient` / `remove_reply_recipient`). The
shared `try_parse_typed_address` is FFI-exported and consumed by the apple,
windows, and android reply-recipient add-inputs + recipient pickers (each
app's local duplicate parser deleted; linux calls it directly as a Rust crate
dep; web parses via the wasm face). The earlier capability flip
(`Smtp.supports_membership_change = false`, 2026-06-07) stands.

**Adding a participant to an MLS group is reach-gated, and UI-proven since
2026-08-03.** A membership add delivers an MLS Welcome, so it is subject to the
recipient's inbox mode exactly as a new DM is — the policy and its verdicts are
owned by [`../behavior/direct-messages.md`](../behavior/direct-messages.md)
§ Reach policy. The user-visible consequence: **a stranger cannot be added to a
group** until they are reachable (a contact edge, or an `open` inbox mode); the
attempt is refused, `confirm_add_participant` rolls the optimistic add back, and
the failure surfaces on `error-message`. The in-place add is proven through the
app UI — picker resolve → chip → `add-participant-confirm`, against a real bound
group, with the nest-side Welcome/Commit effects asserted — by
`test_thread_membership_real.py::test_in_place_mls_add_through_the_ui`
(GREEN `--app linux` + `--app tui`). That test is also what licenses the
command-driven membership ops in `test_fauna_mls_real_roundtrip.py` under
[`../architecture/e2e-conventions.md`](../architecture/e2e-conventions.md)
convention 8. Note a real group's `participant_count` counts **peers only** —
self is not a participant row.

## Element IDs

**The element inventory (IDs, indexing, page-vs-component scope, sub-page
membership) is owned by ui.yaml's `conversations:` block — read it there; this
doc does not mirror the list** (the mirror drifted twice; ui.yaml wins in its
domain). Orientation: the page composes the components `dm-compose-form`,
`dm-compose-bar`, `dm-message-bubble`, `thread-header`, `recipient-picker`, and
`conversation-list-item`, plus the sub-pages `compose` and
`conversation_detail`. Per-element *behavior* notes that belong to this doc:

- `conversation-search-box` is a **page-level** element (moved out of
  `conversation-list-item` 2026-07-10, user-approved) — a local list filter,
  not a nest search (§ Where logic lives).
- `thread-add-participant-button` is capability-gated; on a FaunaMls 1:1 it
  forks a new MlsGroup thread, on every other rail/flavor it adds in place.
- `thread-rename-button` renders iff `capabilities.supports_rename` (MLS
  groups only).
- `recipient-resolve-status` is a single text element with a state attribute
  (§ Errors & edge cases).
- `dm-message-actions-menu` also carries `dm-message-mark-as-spam-button` —
  trains the sealed per-user spam model; behavior owner
  [`../behavior/mail-spam.md`](../behavior/mail-spam.md) § Wire shapes (this
  page renders the menu item only).
- `dm-message-actions-menu` also carries `dm-message-report-button` (IDs
  user-approved 2026-09-25; unbuilt on every app) — opens the shared report
  sheet with subject `message { channel, record_cid }`; behavior owner
  [`../behavior/moderation.md`](../behavior/moderation.md) § User-initiated
  reporting (this page renders the menu item only).
- `dm-message-report-button` (PROPOSED 2026-08-27, not yet in ui.yaml) — the
  report verb beside mark-as-spam, gated `!is_own`; owner
  [`../behavior/moderation.md`](../behavior/moderation.md) § User-initiated
  reporting (this page will render the menu item only).
- **The room model's elements (user-approved 2026-09-09 under rule A, asked
  2026-09-08; behavior owner
  [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)).**
  Built on tui first; the six apps followed in the batched trickle-down,
  which closed with android on 2026-09-11 — all 7 apps paint these. Per-app
  build record and every remaining gap (android's tokens are painted but not
  yet driver-readable, and android is not yet witnessed at tier_3): the behavior
  owner's § Implementation status today.
  - `thread-room-class` on `thread-header` — a single text element stating
    the room's class (`ThreadDetail.room.class`), with a `class` attribute
    `end-to-end | community | transport-only` for drivers, the
    `recipient-resolve-status` state-attribute shape. Not `encrypted-badge`,
    which is a per-message badge. Absent where the rail models no room.
  - `thread-member-chip[i]` carries a `role` attribute (`owner | admin |
    member`; absent on a policy-less room) — a state of the existing element, no
    new id; the chip's text carries the localized owner/admin mark.
  - `thread-room-settings-button` on `thread-header` — opens the policy
    editor; painted **and live whenever the thread is a room** (widened
    2026-09-20, user-approved). It was greyed unless
    `capabilities.can_set_policy`, and the walk-out below made that wrong:
    leaving is a plain member's verb and the editor is where it lives, so the
    door gated the one control a member needs behind the one capability a
    member never has — caught by the leave journey, which got a 409 on a
    member's click. The greying moved INSIDE, per control (§ Architectural
    rules 5, which is what that rule asks for anyway): a member opening this
    reads the room's settings with both selects, both per-participant
    controls and Save greyed, and only `room-leave-button` live.
  - The editor, the sub-page `room_settings` of `conversations` (trigger:
    click `thread-room-settings-button`): `room-join-rule-select`
    (tokens `invite | member-invite`) and `room-history-policy-select`
    (`none | full`) — token round-trips on `select`/`get_text`, the
    `event-detail-reminder-select` contract; `room-admin-toggle[i]`
    (indexed like the chips, `checked` attribute `true | false`; greyed
    unless `capabilities.can_appoint_admins`, and never live on the owner's
    own chip); `room-owner-transfer-button[i]` (user-approved 2026-09-09;
    indexed like the chips, `checked` attribute `true | false` with at most
    one row staged — staging a second un-stages the first; greyed unless
    `capabilities.can_transfer_ownership`, never live on the owner's own
    chip; behavior owner
    [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
    § Roles and authorization → *Ownership transfer*);
    `room-settings-save-button`, which commits every staged change as its
    own policy commit, the hand-over last, and closes only when all landed —
    for the hand-over "landed" means the offer is on the channel: the roles
    flip on every seat once the new owner's device has committed it; the
    page's own `error-message` for a refusal
    (`conversations.unified.error_set_room_policy`). Esc cancels, like the
    rename overlay (no cancel id).
  - **The walk-out (user-approved 2026-09-20 under rule A, asked the same
    day; behavior owner
    [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
    § Roles and authorization → *Leaving — the mechanism*).** Built on tui
    first; the six others follow in the batched trickle-down.
    `room-leave-button` sits in the `room_settings` editor **below** Save and
    outside its staged set, greyed unless `capabilities.can_leave_room` (§
    Architectural rules 5: greyed, never hidden — the owner sees it dead and
    hands the room over first, which is the remedy the copy names). It opens
    `room-leave-confirm`, the `thread-rename-confirm` /
    `add-participant-confirm` shape this page already uses; confirming acts
    **immediately** rather than staging, because leaving is not a policy edit.
    One gesture for both classes: the room's class picks the door in shared
    Rust (`manager.leave_room`), so no app branches on it. The refusal — the
    owner's, or a report the floor did not take — lands on the page's own
    `error-message` (`conversations.unified.error_leave_room`). The thread
    **stays in the leaver's own list** afterwards: the user keeps their copy
    of the conversation.
  - **Pending invitations (user-approved 2026-09-25 under rule A, proposed
    2026-09-21; in ui.yaml; behavior owner
    [`../behavior/room-invitations.md`](../behavior/room-invitations.md)
    § Join rules and invites → *Pending invitations are visible to whoever
    may withdraw them*).** Two ids in the `room_settings` editor, which every
    member can open, in a section between the per-participant controls and
    Save: `room-pending-invite[i]` — one row per entry of
    `ThreadDetail.room.pending_invites`, its text naming the invitee, who
    invited them and the rank on offer, with a `lapsed` attribute
    `true | false` for drivers (the `room-admin-toggle` `checked` shape; a
    lapsed row also says so in words — an invitation that would no longer be
    accepted, listed so it can be cleared) — and
    `room-pending-invite-withdraw-button[i]`, indexed like the rows. The
    section is **absent** while `pending_invites` is `None` or empty. The
    button is live on every painted row and **acts immediately**, outside
    Save's staged set like `room-leave-button`: a withdrawal is not a policy
    edit, and it needs no confirm because inviting again undoes it. A refusal
    lands on the page's own `error-message`
    (`conversations.unified.error_withdraw_room_invite`). Not an overlay on
    `thread-member-chip[i]`: an invitee is not a participant, and a chip for
    somebody who may never join would misstate the room. The row's sentence
    is shared (`RoomPendingInviteSnapshot::text`, FFI twin
    `room_pending_invite_text`; `conversations.unified.room_pending_invite_*`),
    as is the section's heading (`room_pending_invites_label`) and the button's
    word (`room_pending_invite_withdraw`). Built on tui 2026-09-25; the six
    follow in the batched trickle-down.
  - **The community room's three controls (user-approved 2026-09-25 under
    rule A; in ui.yaml; behavior owner
    [`../behavior/community-rooms.md`](../behavior/community-rooms.md) § The
    three classes and § Implementation status today).** Five ids; built on
    tui first (2026-09-25), the six follow in a batched trickle-down. Everything beneath the paint is shared Rust.
    `recipient-picker-home-nest-toggle` in the `compose` sub-page — the
    composer's home-nest choice, painted whether or not a chip is committed,
    with a `checked` attribute `true | false` (the `room-admin-toggle`
    shape); ON seats the home nest in the room about to be created, so the
    first send **founds** a community room (`set_new_thread_home_nest`, then
    the `room.create` ceremony) and `recipient-picker-class` states
    `community` before it. A bridge-ridden chip keeps the room transport-only
    whatever the toggle says. `room-invitation[i]` with
    `room-invitation-accept-button[i]` and
    `room-invitation-decline-button[i]` — flat and indexed alike, **atop the
    conversation list** below the list's own controls: one row per
    `ConversationsSnapshot.room_invitations` entry, its text the shared
    `RoomInvitationSnapshot::text` (who invited, and the rank when it is
    admin) with a `role` attribute `admin | member`; absent while none
    stands. Accept seats the account on the room's floor and the page follows
    the manager into the room's thread; decline tells the room nothing. A
    refusal of either lands on `error-message`
    (`conversations.unified.error_room_invitation`).
    `room-nest-read-toggle` in the `room_settings` editor, before Save — the
    home nest's read, painted only when the draft carries one
    (`RoomSettingsDraft::nest_read` is `Some`: a community room whose answer
    this device has read), with a `checked` attribute, greyed unless
    `capabilities.can_set_policy`; staged, and Save commits it as a rotation
    with the nest in or out, before any hand-over.
  - **The room's labeler set (user-approved 2026-09-25 under rule A;
    behavior owner
    [`../behavior/community-rooms.md`](../behavior/community-rooms.md)
    § The three classes → *What the home nest does with its read*, purpose
    2).** A section of the `room_settings` editor, between the pending
    invitations and Save, painted only while
    `RoomSettingsDraft.labelers` is `Some` (a community room whose set
    verified — `None` on an end-to-end room). `room-labeler-toggle[i]` —
    one row per labeler the nest's catalog (`fauna.labelers.list`, the
    labeler catalog machine the Community labelers page reads) publishes
    as a kind a room may name (`room_may_name_labeler_kind`: `wasm` or
    `text-model`), in catalog order, its text naming the kind and the id,
    `checked` attribute `true | false` (`RoomSettingsDraft::labeler_staged`);
    greyed unless `capabilities.can_set_policy`, and an unchecked row
    greyed once the staged set is full (`labeler_toggle_live`, bounded at
    `MAX_ROOM_LABELERS`) so un-naming always stays open. Staged like the
    other policy controls: Save sends the whole set as one signed record,
    before any hand-over. `room-labeler-inspect-button[i]` beside it opens
    that labeler's **catalog inspect view in place** — the Community
    labelers page's own `labeler-inspect-panel` / `-metadata` /
    `-close-button` and the kind's model section, over the same machine —
    so the owner inspects before choosing without leaving the editor or
    losing the staged draft; close returns to the editor. The catalog is
    re-read when the editor opens, so a labeler published since sign-in
    appears. tui first; the six follow in a batched trickle-down.
  - `recipient-picker-class` on `recipient-picker` — the class statement of
    the room about to be created, painted once a recipient chip is committed
    and before the first message is sent, with the same `class` attribute;
    derived in shared Rust (`fauna_conversations::room::prospective_room_class`)
    from the committed chips — end-to-end when every chip is a Fauna
    address, transport-only when any chip rides a bridge. **Owns the copy**
    `thread-room-class` repeats (`RoomClass::label`, `i18n/strings/en.yaml`'s
    `room_class_*` keys): on the community class the sentence is also the
    read's consent surface
    ([`../behavior/community-rooms.md`](../behavior/community-rooms.md) §
    What the read covers) and must name the purposes in words, not only the
    class — today "Community — searched and labelled by the home nest".
  - `thread-room-notice` on `thread-header` (rule A, user-approved 2026-09-25;
    ui.yaml `optional_elements`, since it is absent while no notice holds) — a
    single text element for the room's standing notice, painted only while
    one holds, with a `state` attribute for drivers, the
    `recipient-resolve-status` shape: `moderation-unverified`
    (`ThreadDetail.room.moderation_unverified`; behavior owner
    [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
    § Roles and authorization → *Delete any message — the mechanism* →
    *Members verify what they paint*) and `awaiting-key`
    (`ThreadDetail.room.awaiting_key`; owner
    [`../behavior/community-rooms.md`](../behavior/community-rooms.md)
    § Implementation status today → *A newcomer's walk waits for its key-in*).
    `awaiting-key` wins when both hold: nothing sealed opens, so there is no
    moderation in view to speak of. Copy, user voice, one sentence per state:
    *"Some moderation in this room couldn't be verified on this device, so
    the affected messages are still shown."* and *"Waiting for a room key —
    messages will appear once an owner or admin keys you in."* Never a mark
    on a message a parked record targets (the behavior owner's rule). Which
    state speaks, its sentence and its token are the shared
    `RoomSnapshot::notice` / `RoomNotice` (FFI twins `room_notice_for`,
    `room_notice_label`, `room_notice_attr_token`) — no app decides them.
    Painted on tui (2026-09-26); the six follow in the room trickle-down.
- **A bridged room no bridge serves, and a Sent message the bridge never
  took (PROPOSED 2026-10-03 under rule A — asked, not yet answered; nothing
  below is in ui.yaml and nothing is built; behavior owner
  [`../architecture/apps/bridges.md`](../architecture/apps/bridges.md)
  § Bridge-kind catalogue → Phase G → *When the bridge stops serving*).** The
  nest reports both facts (`rooms.list`'s `disconnected`, `inbox.fetch`'s
  `undelivered`) and no app says either in words. Recommended shape, each
  after the page's own prior art:
  - **The room: a third state of `thread-room-notice`, no new id.**
    `bridge-disconnected` joins `moderation-unverified | awaiting-key` on the
    `state` attribute — the header's one standing-notice slot is exactly "this
    room is in a condition you should know about", and it already speaks one
    shared sentence per state. Copy, user voice: *"The app that carried this
    conversation is no longer connected. You can still read it, but you can't
    send."* The fact rides an additive `RoomSnapshot.bridge_disconnected`, and
    `RoomNotice` gains the arm, so no app decides it; the other two states
    cannot hold on a transport-only room, so no precedence question arises.
    The list row gains nothing: it already falls to the bridge's id and the
    generic glyph, and the composer is already closed by the most-restrictive
    vector. ui.yaml delta: the `thread-room-notice` description names the
    third state. *Not* an attribute on `protocol-icon` (a `view` that states
    which network, and can carry no sentence), and not a new element (the
    notice slot exists for this).
  - **The message: one new id, `dm-message-undelivered`.** An optional,
    indexed text element of `dm-message-bubble`, painted under the body of an
    own message the nest stamped `undelivered`, the `dm-message-deleted` /
    `dm-message-muted` shape — except that the body stays in view, since the
    user needs it to send again. Copy: *"Not delivered — the app that carried
    this conversation was disconnected before it went out."* The fact rides
    an additive `MessageSnapshot.undelivered`. No resend button: the user
    sends again from the composer once a bridge serves the room, which is the
    behavior owner's rule. ui.yaml delta: the element, and its line in
    `dm-message-bubble`'s `optional_elements`. *Not* an attribute on
    `dm-message-timestamp` (the `selected` precedent): that shape suits a
    state each app shows by its own visual treatment, and this one has to be
    said in words — a tinted bubble does not tell the user the message never
    left, and the timestamp's text is a format contract with no room for a
    sentence.
  - **If declined:** the generic rendering is the ruled one — a disconnected
    room is recognised by its closed composer and generic glyph alone, and an
    undelivered Sent message looks like any other.
- The bubble carries `dm-message-muted` / `dm-message-muted-reveal-button` —
  the muted-keyword collapse-with-reveal verb; behavior owner
  [`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md)
  (this page renders the collapse only).

## State & data shape

See the messaging-UI design § 2 Data model (ratified 2026-05-10; tracked internally)
for full Rust signatures. Owned by `libs/fauna-conversations`.

Snapshot summary:

```text
ConversationsSnapshot {
    threads: Vec<ThreadSummary>,        // pre-sorted by activity
    sort: SortOrder,
    search_query: Option<String>,
    selected_thread_id: Option<ThreadId>,
    new_thread_compose: Option<ComposeState>,
    add_participant: Option<AddParticipantState>,  // Some while the add-participant overlay is open
                                                   // (carries the offline-gate discriminant — see below)
    error: Option<LocalizedText>,       // page-level error → error-message (§ Errors & edge cases)
    room_invitations: Vec<RoomInvitationSnapshot>, // verified community-room invitations standing
                                                   // for this account (§ Key types)
}

ThreadDetail {
    thread_id, rail, flavor, label,
    participants: Vec<TypedAddress>,
    participant_displays: Vec<String>,
    capabilities: ThreadCapabilities,
    messages: Vec<MessageSnapshot>,
    compose: ComposeState,
    selected_message_id: Option<MessageId>,  // § The selected message
}
```

Key types:

- `Rail` — `FaunaMls | Smtp | Bridged` in the target state; today
  `FaunaMls | Smtp | Nostr | Bridged`, `Nostr` retiring with the Nostr leg's
  migration (§ Where logic lives → *The `Bridged` adapter*, ruling 3's
  deletion timing).
- `ThreadFlavor` — `OneToOne | MlsGroup | SubjectKeyed`.
- **Room class** — end-to-end / community / transport-only, **derived from
  the member set** in shared Rust and rendered as
  `ThreadCapabilities.encryption`; owner
  [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § The three classes. Built for the native rail 2026-09-08
  (`libs/fauna-conversations/src/room.rs`), and for mail and the bridged
  rail 2026-10-02 (`room::transport_room`); the last per-rail constant,
  the Nostr leg's, went with `Rail::Nostr` on 2026-10-03
  (§ Implementation status today).
- **`ThreadDetail.room`** — `Option<RoomSnapshot>`: the room the thread is
  (`RoomClass`, one `RoomMemberSnapshot { kind, role }` per participant,
  **index-parallel with `participants`**, the policy's rendered fields
  `RoomPolicySnapshot { version, name, join_rule, history_policy }`, and
  `my_role`, and `nest_read` — whether a community room's home nest reads
  it, `None` for every other class and until the floor has answered). A
  read-time projection the thread's rail supplies on every
  emit (`RailBackend::room_state`), `None` for a rail that models no room
  yet. `role` and `my_role` are `None` on a **policy-less** room (one carrying no
  policy) — there are no roles to mark, not "everyone is a member". Apps
  paint the class on the header, the role mark on each chip and the policy
  in the editor from this alone; owner
  [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § The room.
- `TypedAddress` — typed-per-rail `(protocol, identifier)` (Fauna carries
  two fields; the `Bridged { bridge_id, address }` arm carries a far
  network's own spelling under the bridge that reaches it — ruling 2 (c) of
  *The `Bridged` adapter*; others one). Display names live on the snapshot,
  never on the address.
- **`ThreadSummary.bridge` / `ThreadDetail.bridge`** —
  `Option<BridgeIdentitySnapshot { id, label, glyph }>`, the bridge a
  `Bridged` thread rides, projected from the bridged rail's registry on every
  emit and `None` on every other rail; when `Some`, the thread's `glyph` is
  the bridge's declared one (ruling 2 (a) of *The `Bridged` adapter*).
- `ThreadCapabilities` — `supports_attachments`, `supports_markdown`,
  `supports_reactions`, `supports_message_delete`, `supports_per_message_reply`,
  `supports_membership_change`, `supports_recipient_selection`, `supports_rename`,
  `supports_subject`, `delivery_mode`, `encryption`, and the **role-gated
  five** `can_invite`, `can_remove_members`, `can_set_policy`,
  `can_appoint_admins`, `can_transfer_ownership`. The rail/flavor half is
  derived from `(rail, flavor)`;
  the role-gated half (and `supports_rename` on a governed room) is overlaid
  per emit from the viewer's effective role by `RoomSnapshot::gate`
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § Roles and authorization) and stays **open** wherever no policy governs.
  Apps grey `thread-add-participant-button` on `!can_invite` and the chip's
  remove on `!can_remove_members`, and never branch on a role themselves
  (§ Architectural rules 5). `encryption` is `E2E | TransportOnly | None |
  NestReadable` — the room class's render; `NestReadable` is the community
  arm, produced by no roster until the nest plane lands.
  `supports_reactions` + `supports_message_delete` are FaunaMls-only today (every
  other rail `false`) — see § Reactions & message delete.
- `ComposeState` — per-thread; carries body / subject drafts,
  attachments, `reply_to`, `recipient_picker` (`Some` only on
  `new_thread_compose`), `send_state`. The picker's `include_home_nest`
  seats the user's home nest in the room about to be created — the one
  member that is never a chip — which makes it a community room the first
  send founds ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § Implementation status today, *The class reaches the app layer*).
- `RoomInvitationSnapshot` — `{ id, inviter_display, role }`, one standing
  community-room invitation, already verified against its signer; the
  manager's `accept_room_invitation(id)` / `decline_room_invitation(id)`
  settle it.
- `RoomPendingInviteSnapshot` — `{ invitee_actor_hex, invitee_display,
  inviter_display, role, invited_at_ms, lapsed }`, the **room-side** twin: one
  invitation pending on the room, on `ThreadDetail.room.pending_invites`
  (`Option<Vec<_>>`, oldest first). `None` until the home nest has served a
  list — and for every class but community — so an app paints nothing rather
  than an empty section; `Some(empty)` is a real answer. The nest scopes the
  list to what the viewer may withdraw, so **every row carries the withdraw
  gesture and no capability gates it**: the manager's
  `withdraw_room_invite(thread_id, invitee_actor_hex)`. Owner
  [`../behavior/room-invitations.md`](../behavior/room-invitations.md)
  § Join rules and invites → *Pending invitations are visible to whoever may
  withdraw them*.
- `AttachmentDraft` (staged compose attachment) — `{ blob_hash, filename,
  mime_type, size_bytes, is_image }`. **Light by design — no bytes.**
  `add_attachment` hashes the picked file (BLAKE3 → `blob_hash`), caches the
  bytes in the manager's in-memory **attachment store**, and stages only this
  metadata, so an observed `ComposeState` stays cheap to diff over UniFFI even
  with a multi-MB image staged.
- `AttachmentSnapshot` (rendered attachment on a message) — `{ blob_hash,
  filename, mime_type, size_bytes, is_image, c2pa }`. `blob_hash` is the unified
  content handle every app renders off; the app resolves it to real bytes
  through the shared loader `ConversationsManager.attachment_bytes(blob_hash)`
  (the cache the inbound parse / send echo populated for nest-backed rails) — and,
  for FaunaMls, the receive path fetches the sealed blob from the nest's
  content-addressed byte-source (`GET /api/v1/blob/{sealed_cid}`), `decrypt_blob`s
  it, and caches it under `blob_hash` (§ Attachments → *Built (FaunaMls rail)*).
  This replaces the old per-app `uri` so the *handle* is uniform and
  *resolution* is the only per-rail concern. `c2pa` is the receiver's per-attachment
  content-credentials verdict; which platforms compute it and which apps paint it is
  owned by § Attachments "C2PA on-device" below (corrected 2026-09-21).
- `AddParticipantState` — `{ target_thread_id: ThreadId, picker: RecipientPickerState, in_place_mls_group: bool }`. Drives the add-participant overlay; the recipient picker routes through the manager exactly like `new_thread_compose.recipient_picker`.
  `in_place_mls_group` (2026-08-17) is the overlay's **offline-gate
  discriminant**, stamped by `open_add_participant` from the thread it already
  looked up: `true` exactly on a bound FaunaMls **group**, the one
  `(rail, flavor)` whose confirm reaches the wire (the add commit opens by
  fetching the newcomer's key package). A 1:1 fork and every non-FaunaMls rail
  issue nothing, so a client gating `add-participant-confirm` on
  `fauna.conversations.keypackage.fetch` unconditionally would grey a gesture
  that works offline — the over-claim
  [`../architecture/account-offline-mutation.md`](../architecture/account-offline-mutation.md)
  § The offline-mutation contract forbids. Every app reads this one field
  rather than re-deriving the `(rail, flavor)` test (priority #2); the
  predicate is `fauna_conversations::capabilities::is_in_place_mls_group`.
  ⚠ It is **not** the authority for the wire op — `confirm_add_participant`
  re-derives from the live thread — and it is **not** the fork-vs-mutate test:
  `add_participant_inner` forks only on a FaunaMls 1:1, so an SMTP 1:1 adds
  "in place" there while answering `false` here.
- `ConversationsSnapshot.error: Option<LocalizedText>` (2026-07-29) — the
  page-level error, same shape and role as `FeedSnapshot.error`. Scope is the
  **membership/label wire ops** (`confirm_add_participant`,
  `remove_participant`, `rename_thread`), never the compose send path — see
  § Errors & edge cases for the split and why each gesture has exactly one
  truth.
- `MessageSnapshot.subject_line: Option<String>` — `Some(_)` only when
  the message's subject differs from the previous message's effective
  subject in the same thread (drives `subject-divider` rendering).
- `MessageSnapshot.reactions: Vec<ReactionGroup>` + `deleted: bool` + `is_own: bool`
  — the per-message reaction aggregate (`ReactionGroup { emoji, count, reacted_by_me }`,
  ordered by first-added time), the tombstone flag (renders the deleted placeholder when
  `true`), and the own-authorship flag that gates the delete affordance (derived per-rail —
  mailbox provenance for mail, never the `From:` header; § Encryption at rest). All three are
  **derived** by the manager from the channel message stream, not separately stored
  (§ Reactions & message delete).
- `MessageSnapshot.labels: Vec<ContentLabelEntry>` (ratified 2026-07-16) — per-message
  category labels populated client-side by the manager's post-decrypt classify pass
  (`ConversationsManager::observe_local_detection`, called from `ingest_inbound_to_thread`;
  never round-tripped through the nest); drives the `content-label-badge` child of
  `dm-message-bubble`. **Built on all 7 apps** — tui's DM-bubble consumer
  (`crate::moderation::content_label_badge`) landed 2026-07-29, closing what this bullet
  once carried as a "tui not yet" gap. Shape, per-app status, and badge semantics owner:
  [`../behavior/moderation.md`](../behavior/moderation.md) § Per-row badge data path.

The crate's `ConversationsManager` is the single observable surface
(UniFFI for Apple/Android/Windows/Linux, WASM for web). Apps call
mutators through it and observe snapshot diffs back.

> **[render-model.md](../architecture/render-model.md) § Deltas → D1/D2/D2b/D3 DONE on all 7 apps
> (tui's `conversation_detail` closed the last D2/D3 gap 2026-07-30 —
> render-model.md § Implementation status today is the authoritative per-app table, incl.
> D4 link-previews which this section doesn't otherwise cover; this blockquote only restates the
> conversations-specific D1/D2/D2b/D3 rationale).**
> `MessageSnapshot` carries `document: RenderDocument`, the complete render tree (text body +
> `Attachment` blocks) produced once by the manager via `document_for_message`. The sibling
> render fields `body_format` and `attachments` are **removed** — the `body_format`
> discriminant is consumed *inside* the producer (D1), and attachments are first-class
> `Attachment` blocks (D2). `body` is **retained as the canonical text source** (not a
> deletable sibling): the manager builds `document` from it, and the thread-list snippet
> reads it. The snippet is a **bounded preview**, never the whole body: `summarize` runs
> `markdown_to_plaintext` over a byte-bounded prefix of `body` and caps the result
> (`SNIPPET_PARSE_MAX_BYTES` / `SNIPPET_MAX_BYTES`, `store/threads.rs`), because every
> app paints one line of it and every `snapshot()` re-derives it — an unbounded one made a
> multi-megabyte mail's list row the bulk of its open
> ([mail-message-size.md](../behavior/mail-message-size.md) § Implementation status today).
> The retained `body` is the exact analogue of feed's
> retained `PostSummary.body`. No app re-parses `body` at render time. The former apple
> `dm-message-text` raw-`body` read (a priority-#1 e2e drift) was closed 2026-06-22/23
> — every app, apple and tui included, now reads the painted
> *document* plaintext via `render_document_to_plaintext` / `RenderDocument::to_plaintext`.
> - **D1 (body → document): DONE on all 7 apps** — every shell walks `document` instead of
>   re-parsing `body` at render time; `body_format` deleted from the snapshot.
> - **D2 (embeds as in-body blocks): the `Attachment` embed is DONE on all 7 apps** (tui closed
>   the gap 2026-07-30) — the manager folds one `Attachment` block per attachment after the text
>   body (at construction, via `document_for_message`), every shell renders attachments from
>   `document`, and the sibling `attachments` field is **deleted**; `attachment_bytes(blob_hash)`
>   resolution is unchanged (only placement moved). The Rust read projection is
>   `attachment_blocks(&document)` (§ Implementation status today).
>   **D2b — `QuotedMessage` (in-bubble reply-quote): DONE on all 7 apps** (user-approved
>   2026-06-23; windows 2026-06-24, apple 2026-06-23, tui `mod.rs:2078`
>   — this doc's own table wrongly carried windows/macos/ios as "tracked internally" for a
>   month; fixed in § Implementation status today). The manager folds a
>   `RenderBlock::QuotedMessage{author_display, snippet}` into a reply's document at read time
>   (`thread_detail`, beside the D3 reveal projection), resolving the parent from the same
>   thread's loaded messages and **prepending** the block so it paints above the body; hidden
>   when the parent isn't loaded (we hold only the bare `reply_to` id). Snippet =
>   `markdown_to_plaintext(parent.body)` (char-bounded; apps clamp to ≤ 2 lines), excluded
>   from `to_plaintext` so `dm-message-text` stays the body alone. New `ui.yaml` element
>   `dm-message-quote` on `dm-message-bubble`; tier_3 `test_conversations_reply_quote.py` green
>   web/linux/windows. See [render-model.md](../architecture/render-model.md) § D2 / § Implementation
>   status. (Tap-to-jump to the parent is a deferred follow-on.)
> - **D3 (remote-image reveal state → manager): DONE (user-approved 2026-06-22) on all 7 apps** —
>   web/linux/android/apple/windows landed first (windows 2026-06-09; this doc's
>   own table wrongly carried it as "tracked internally" for a month, fixed in § Implementation
>   status today), tui last (2026-07-30) via `load-remote-content-button` on `conversation_detail`
>   (mirroring its own feed post-cards). The per-app `revealedRemote`/`remoteLoaded`
>   dictionary is gone: `ConversationsManager` owns an in-memory reveal set (keyed by message id),
>   projects it onto `RemoteImage.revealed` in `thread_detail`, and `reveal_remote_images(message_id)`
>   (the `load-remote-content-button` dispatch) flips it + re-emits. Posture unchanged (blocked by
>   default, in-memory only — no persistence); refines [html-mail.md](../behavior/html-mail.md)
>   § Rendering. Symmetric on the feed side (`FeedManager.reveal_remote_images(post_id)`).

### When a thread is read (ratified 2026-09-21; shared Rust built the same day)

**An unread message is one somebody else sent that arrived while the user did not have its thread open.** `ThreadSummary.unread_count` counts them, per thread; `dm-unread-indicator` paints exactly when it is above zero, and `SortOrder::Unread` groups on it. Every app renders the count and none of them decides it — the whole rule is `fauna_conversations`' (`store::threads` holds the state, `ConversationsManager::notify` applies the read).

- **A thread is read by opening it — whole, at once — and whatever arrives while it stays open is read on arrival.** "Open" means it is the selected thread *and* the detail pane is showing it: the new-thread composer covers the selected thread without deselecting it (§ Persistence — the draft switch is reversible), and a covered thread is not being read. This is deliberately the same notion of attention as the OS-toast decision's focus suppression (§ Where logic lives): a message that raises no banner because the user is already looking at its thread is not unread either, and the two may never disagree.
- **A shell that shows the list *or* the thread, never both, closes the thread when the user goes back to the list** — it calls `clear_selection()`. This is the one thing about reading an app has to do, and only the single-pane ones: a two-pane app's selected thread really is on screen. Skipping it is silent and total — the thread stays selected behind the list, so everything that arrives in it afterwards is read on arrival and never flagged (and, by the same attention rule, never raises a banner). iOS does it on the detail view's disappear; tui does it in `conversations::show_list`, the one door its `conversations-tab` and its Esc both leave a thread through (2026-09-21 — until then the tab was no way back to the list at all, and Esc left the thread selected); **android does not yet**.
- **Why "opened", not "its newest message reached the viewport".** Viewport precision exists in the system and means something else: the body-rendered observation ([`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md) § The replica boundary → T1) records what an account has *observed*, is grow-only, and is reported per shell because only a shell knows what it painted. Unread is a product affordance that must clear by one gesture on every app the day it ships, and the moment a thread opens is the manager's own (the reveal-state division of [`../architecture/render-model.md`](../architecture/render-model.md) § D3) — so the rule needs no app leg at all. It also ratifies what was already practised: four apps called `mark_read` on select against the stub for months.
- **The user's own messages are never unread**, on any rail, including a server-side `Sent` copy that arrives as inbound and is recognised as own afterwards. A deleted message stops counting.
- **`mark_read(thread_id)` reads a thread without opening it.** Selection needs no call — an app that also calls it on select is redundant, never wrong — so the method stays for a gesture that reads without opening.
- **Read state belongs to the account, not to a device.** A thread read on one of the account's devices is read on all of them, and a message that arrived while every app was closed is unread at the next launch. How — one carrier per rail, and how each fills the unread set this section describes — is owned by [`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md); nothing in it changes the rule above, and no app takes part in it beyond registering one seam.

**Implementation status today — mail's read state is the mailbox's `\Seen` flag on every app, and the native rail's is the account's on every app that hosts an account runtime; web's native rail lives in memory, for one app run.** On every app but web a fauna-native thread's unread messages are those above its synced read marker once the account store is ready, and a read raises the marker for every device ([`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md) § Implementation status today). A mail message is unread exactly when it is not own and lacks IMAP `\Seen`, read off the inbox feed at every launch, so a mail that arrived while the app was closed is unread at launch and none of the floor below applies to it; opening a mail thread sets the flag on the nest, and a flag changed elsewhere — another device, another mail app — reaches a running app without a relaunch (same doc, same section). Native threads on web, and on any app before its store is ready, still cannot tell a message the user read last week from one that arrived while the app was closed, and the store is refilled from the nest at every launch. So each run has a **floor** — the moment the store was created, or wiped for an identity change — and a message stamped before it is history, never unread. The consequence is declared rather than hidden: **a message that arrived while the app was closed shows no indicator**; the indicator means "arrived during this run, thread not opened since". Under-reporting was chosen over the alternative on purpose — without the floor every mail the account has ever received would be unread again after every restart, and a false indicator on every row is worse than a missing one on some. The floor compares a message's own timestamp with this device's clock, so a device whose clock runs ahead under-reports for that long after launch; and on every rail but mail that timestamp is the sender's own claim, so a live message its sender backdated below the floor under-reports too (declared, and bounded to the floor's own lifetime, in [`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md) § How the carriers meet the in-memory set). **The same floor gates the new-message banner** (§ Where logic lives, rule 2, 2026-09-22): it rides every snapshot as `launch_floor_ms`, and a message stamped before it is history to both decisions — neither unread nor bannered — however late its rail delivers it. The state is a per-thread set of message ids rather than a position because no rail-neutral position key exists to hold; the carriers fill that set, and the same doc says where the floor survives them (a host with no account runtime) and when it is deleted.

### The selected message (built 2026-08-10, tui lead)

The thread detail can distinguish **one** message inside itself. Its only
producer today is a mail search result: `SearchNav::Mail`'s contract is *"open
the thread **and** select this message in it"*
([search.md](search.md) § State & data shape owns the target type; this section
owns what the page then does with it).

- **`ConversationsManager::select_thread_and_message(thread_id, message_id)`**
  is the only way a message becomes selected. It sets thread + message under one
  `notify`, so an observer never sees the thread flipped with the message not yet
  set — on an app that scrolls to the selection, that intermediate frame is a
  visible jump to the wrong place. Every other selection path (`select_thread`,
  `clear_selection`, the identity-change wipe) clears it: a marker must never
  survive onto a thread the user picked by hand.
- **`ThreadDetail.selected_message_id` is a read-time RESOLVE, not stored view
  state.** `thread_detail` re-derives it on every emit and yields `None` unless
  the id is present in `messages`. Two properties follow, and both are the
  reason it is done this way: a selection naming a message this thread does not
  hold (the stale id left behind when `send_new_thread` or the 1:1 participant
  fork selects a *different* thread) can never reach an app as a marker with
  nowhere to go; and a hit on a message whose history has not streamed in yet
  lights up by itself on the emit that fetches it, with no retry plumbing in any
  of the 7 apps.
- **It is view state on the detail, not a flag on `MessageSnapshot`** — the same
  split that puts `selected_thread_id` on the page snapshot rather than on
  `ThreadSummary`. "Selected" is not a fact about a message, and
  `MessageSnapshot` crosses FFI into the Go mail bridge, which has no view.
- **Apps paint from the field and never re-derive it.** The visual treatment
  is per-platform (a background tint where there is fill to vary; a marker line
  in a terminal, which has none — the same reasoning as the reaction pill's own
  `reacted_by_me` marker). The **automation observable is uniform**: a `selected`
  (`"true"`/`"false"`) attribute on `dm-message-timestamp`, always present, never
  a new element id — see `ui.yaml` `dm-message-bubble` for why the timestamp
  carries it (it is the one child painted by *every* render arm, including the
  deleted / muted / content-collapsed ones that paint no `dm-message-text`).
- **Bringing it into view is part of the affordance, not a nicety** — a hit deep
  in a long thread that is marked but off-screen is as hard to find as it was
  before search pointed at it. Apps with a scrolling viewport scroll to it;
  on tui the viewport follows the focus ring, so the ring lands on the selected
  message's first control and the scroll follows (`apps/tui.md` § Rendering —
  the same mechanism the week/day time axis uses).

### Self-address: live, never baked (ratified 2026-08-02; both scope rulings are the user's, 2026-07-23)

- **The canonical self-address is the account's `<handle>@<domain>`** — the
  handle's own domain from the account/identity state, **never the nest URL
  host** (a nest serves handles on domains that need not be its hostname —
  [mail-multidomain.md](../behavior/mail-multidomain.md)). It is three things at
  once: the SMTP `From:` (nest-side verification:
  [mail-app-surface.md](../behavior/mail-app-surface.md) § First-party client send), the
  FaunaMls self-handle (outbound sender attribution, and the address
  `RailBackend::self_address()` drops from reply-all recipient seeds — §
  Participants vs. reply recipients), and the domain the FaunaMls data plane
  compares a peer's domain against to route same-nest vs. cross-nest
  (`backends/fauna_mls.rs`; [federation.md](../architecture/federation.md)). A
  stale or empty domain therefore doesn't just mislabel a `From:` — it silently
  mis-routes cross-nest key-package fetch / Welcome delivery.
- **Identity resolution must never delay conversation delivery** (user ruling
  1, 2026-07-23: split construction, not gate-the-whole-build). The
  manager/session is built as soon as the actor secret exists — MLS/DM delivery
  needs no address — and the self-address is supplied *when it resolves*, never
  demanded as a build precondition. Gating the whole build on handle+domain
  costs a live MLS-delivery delay on every login for no MLS-side reason.
- **The address is one live value with one setter** (user ruling 2, 2026-07-23:
  uniform self-heal on every app, not accepted per-app debt).
  `ConversationsSession::set_self_address` (native/FFI) and the wasm face's
  `setSelfAddress` update a single shared cell that both rails read **at use
  time** — send-time `From:`, routing-time domain comparison, reply-all
  self-drop. An app calls the setter from the one place identity state lands
  (login-time resolution, the background identity refresh, a server-side handle
  rename) and never rebuilds a backend for it. This supersedes the linux/tui
  `ensure_smtp_backend` re-register-on-compose pattern, which healed only the
  SMTP rail while the FaunaMls rail stayed stale-at-build — the self-heal now
  lives once in shared Rust (priority #2/#4; richest pattern, lifted).
- **"The one place identity state lands" is itself shared on the native apps**
  (2026-08-03): `LaunchSnapshot.identity` (`fauna_launch_machine`) carries the
  `handle`/`domain`/`tier` the nest confirmed on the most recent silent
  challenge — `None` until one resolves, replaced on every later resolution.
  The machine always knew all three (they arrive on `VerifyReply` and are
  written to the long-term store via `LaunchPersistence::save_authenticated`);
  what was missing is that **a store write is not an event**, so an app
  observing only `on_changed()` could not see the identity resolve or change.
  A native app therefore needs no hand-rolled refresh RPC of its own: it reacts
  to the snapshot, and gets the ratified `domain` source for free (the handle's
  domain as the nest reports it — never the dialed URL host, the bug the first
  bullet names). android is the reference consumer; **windows and apple should
  prefer this channel** to a third and fourth private copy. Not a wasm/web
  surface: web has its own identity-store subscription and does not run this
  machine.
- **While unresolved, the address is empty and a send refuses locally** — the
  § Errors & edge cases bullet *"A send the app cannot legitimately address is
  refused locally"* owns that floor (`error.email.no_handle`). The refusal
  treats an address with an empty local part or an empty domain
  (`"@nest.example"` — the shape an app synthesizes from an unresolved handle
  plus a URL host) exactly like a missing one: both are the forbidden
  "handle the nest does not back" substitute, not a sendable `From:`.

## Where logic lives

Per the messaging-UI design § 5 Per-rail strategy (ratified 2026-05-10; tracked internally).

**Shared Rust (`libs/fauna-conversations`):**

- Address parsing / resolution (rail probe, MLS key lookup, WebFinger,
  Fauna→Email disambiguation; a bridged address is resolved nest-side against
  each bridge's grammar, § *The `Bridged` adapter* ruling 2 (d)). A **cross-nest** resolve
  names the peer by the domain the client dialed, never by the domain the peer
  echoes back — rule and rationale at
  [`../architecture/foreign-handle-resolution.md`](../architecture/foreign-handle-resolution.md)
  § Peer-auth model → *Discovery-failure semantics* (*The dial names the peer*).
- **`TypedAddress` → display string** — the per-rail variant→display switch
  (Fauna→handle, Email→address, Nostr→npub, Bridged→the far spelling)
  lives once as `TypedAddress::display` + the FFI twin `typed_address_display`
  (`address.rs`), consumed wherever a raw address must be rendered with no
  pre-computed display at hand (sender bubble when `MessageSnapshot.sender_display`
  is empty — which is *always*, until contact-name resolution lands — recipient/
  participant chips the user is still typing). Pre-resolved displays still travel
  in the snapshot (`ThreadDetail.participant_displays`). **All 7 apps share the
  one switch** (linux as a Rust dep, natives over the FFI export, web over the
  wasm face); no per-app duplicate remains.
- **The reaction quick-set** — the six emoji `dm-message-actions-menu` offers, and their
  order, live once as `fauna_conversations::QUICKSET_EMOJIS` (`reactions.rs`), read by
  linux and tui as a crate dep, by apple/android/windows over the UniFFI face
  `quickset_emojis`, and by web over its wasm twin `quicksetEmojis`. Shared because the
  **order** is a cross-app contract, not a style choice: `dm-reaction-option` is an indexed
  ui.yaml element, so an e2e tapping index 0 asserts 👍 on every app. Four apps used to keep
  re-typed copies, which a reorder here would have silently turned into a 4-app divergence
  with nothing to catch it. The fuller "more" picker stays per-platform
  (§ Reactions & message delete → *Rendering / picker glue*), but the twenty-emoji shortcut grid
  web and windows show beside it is the same kind of list and lives beside this one
  (`MORE_GRID_EMOJIS` / `more_grid_emojis`). *Status:* all 7 apps read the
  shared quick-set face; no per-app copy remains (apple 2026-08-02; android + web 2026-08-11; windows
  2026-08-11). The grid list is in shared Rust (2026-09-29, with its wasm twin `moreGridEmojis`);
  windows reads it over UniFFI, web still hand-copies it until its field lands.
- Subject normalization, hybrid thread keying, inbound bucketing
  (`route_inbound`).
- **Reply preview (`dm-reply-preview`)** — `ConversationsManager::reply_preview(thread_id)` owns what the compose bar says about the reply in progress: the answered message's sender and a plain-text excerpt of it (markdown stripped, the list-row snippet's bound), `None` when no reply is armed or the answered message is outside the fetched window (an empty preview beats a stale or wrong one). Apps render the record, never derive it. *Status:* tui and linux render it (2026-09-21), web through the wasm `replyPreview` face and macos + ios and windows through the UniFFI `replyPreview` face (all 2026-09-24), android through the same face (2026-09-25) — so all 7 apps; until then the apps showed three different things — the body (tui, apple), the sender's name (windows), the bare message id (web, android, linux).
- **Thread-list sort cycle (`conversation-sort`)** — `fauna_conversations::snapshot::next_sort_order`
  owns the one decision an app makes when the sort button is tapped: which order comes next.
  The cycle is **latest activity → oldest first → unread → latest activity** — three taps from any
  order return home, so every order stays reachable everywhere. Apps pass the order the snapshot
  handed them and feed the result straight back to `set_sort`; they never enumerate the orders.
  *Ratified 2026-07-16* (the doc previously said only "toggle sort order", specifying no arity, and
  the apps had drifted: android cycled all three while web and apple reached only two, leaving
  `SortOrder::Unread` — fully implemented in `sort_summaries` — unreachable for those users, and
  linux's button was an explicit no-op). Unknown/absent order restarts the cycle at the default
  (a tap always moves). UniFFI face `next_sort_order`, wasm twin `nextSortOrder` (takes/returns the
  serde variant name `setSort` already speaks). *Status:* shared cycle consumed on **all 6 GUI
  apps** — web (2026-07-16) and windows (2026-07-16) both landed before
  this paragraph's prior revision was even written (2026-07-18), which had stale-flagged them as
  "follow"; linux (lead); macOS and iOS (apple was the last of these three, 2026-07-18 — the prior stub `Menu`
  with two inert items is now a single button calling `manager.set_sort(next_sort_order(current))`,
  same shape as linux/web); android already matched the ratified cycle before the shared fn existed;
  tui (an earlier gap, closed 2026-07-22 — `Action::Sort` now calls
  `manager.set_sort(next_sort_order(current))`, same shape as linux/web).
- **Thread-list sort + search filtering** — `snapshot()` owns both, so every
  app renders the already-sorted, already-filtered `threads` and never
  re-sorts/re-filters (priority #3/#4). The active `SortOrder` reorders the list
  (`sort_summaries`) and the `conversation-search-box` query filters it
  (`filter_summaries` — a case-insensitive substring over each thread's `label`
  + its latest message's full plaintext — deliberately not the bounded `snippet`, so a
  term past what the row shows still finds the thread; a blank/whitespace query shows
  every thread). *Status:* shared
  filter consumed on all seven apps (windows' client-side `.Where()` twin is
  deleted; apple landed — this doc's *Implementation status today*
  table had gone stale claiming "leg follows").
  - **Deliberately a local filter, NOT a nest re-query** (contrast
    [feed.md](feed.md) § Where logic lives, where feed search *is* a nest
    re-query folding `BodyContains` over post *content*). A conversation-list row
    is a *thread the manager already holds* (label = name, snippet = latest
    preview), so "Filter list" is a substring filter over that loaded set —
    instant, offline, no nest round-trip. A future "search the *bodies* of every
    message across all (incl. unloaded) threads" would be a separate, larger
    nest-backed feature mirroring feed's re-query (a `fauna.conversations.search`
    kind); it is out of scope here and must not be conflated with this list
    filter.
- **Thread label display (empty-label fallback)** — `fauna_core::format::thread_label_display(label)
  -> LocalizedText` owns the one decision an app makes when rendering a thread's name: a
  blank/whitespace-only `label` carries the canonical `conversations.detail.no_subject` key
  (`"(no subject)"`), a non-empty label rides verbatim (resolves to itself on an i18n miss, the same
  passthrough `contact_status_label` uses for an unknown status). Consumed wherever a thread name is
  painted — the list-row label **and** the thread-header title. The raw `label` stays the value used
  for filter/sort (above) and the rename field; only the *display* derivation lives here. *Status:*
  **all seven apps single-source the thread label** (UniFFI face
  `value_format::thread_label_display`, wasm twin `threadLabelDisplay`; list row
  + thread-header title on every app) — resolving the prior drift where
  windows/apple hardcoded an untranslated literal, linux/web applied no
  fallback, and android's `conversations.detail.no_subject` key was the richest
  pattern this converged on.
- Per-rail wire format encode/decode (one `RailBackend` impl per rail).
- Capability derivation (`(rail, flavor)` → `ThreadCapabilities`).
- All MLS / encryption ops (extends `libs/fauna-mls`).
- Compose draft persistence (`DraftStore`).
- Send queue, retry, offline handling.
- Inbound decryption.
- **Markdown source-range decoration map** for inline compose styling —
  `fauna_core::markdown::decoration_map(src) -> Vec<MdDecoration>` (byte ranges
  into the *raw* source: styled-content ranges + marker ranges). The **same
  inline scanner** feeds both `parse_markdown` (render) and `decoration_map`
  (compose), so the editor's preview and the sent message can never disagree on
  what is styled. Native apps consume it directly (linux) or over UniFFI
  (`FfiMdDecoration`); web over `fauna-wasm` (`decorationMap`). *(This lives in
  `fauna-core`, not `libs/fauna-conversations` — it is shared with feed/article
  rendering.)*
- **New-message OS-toast decision** — moved 2026-09-28 to
  [`../behavior/message-banners.md`](../behavior/message-banners.md) § Where logic lives:
  the shared `MessageNotificationTracker` when/for-whom decision, its three rules, the
  launch floor and the unread-count key, and the fired-banner witness.

**App glue:**

- Rendering (XAML / SwiftUI / Compose / Svelte / GTK).
- Capability-based styling — the `disabled` styling itself; the flag
  comes from shared Rust.
- Mobile-vs-desktop layout decisions.
- Markdown toolbar text-wrap *splice* only: read the `TextBox`/textarea selection,
  write the new text + selection. The wrap *rule* — where the `*`/`**`/`` ` `` markers
  go, keeping edge whitespace OUTSIDE them so a word-selection's trailing space can't
  produce `*italic *` (which collides into `*italic ***bold**` and isn't valid
  CommonMark) — lives once in shared Rust (`fauna_core::markdown::wrap_selection`,
  reached via the `wrapMarkdownSelection` wasm/UniFFI face), so every app wraps
  identically (priority #1/#2/#4).
- **Apply the inline-styling decoration map** to the compose editor — take
  `decoration_map`'s byte ranges, convert to the platform's offset unit
  (GTK chars, JS UTF-16, Swift `String.Index`, Compose offsets), and apply
  native styled/dimmed runs (`gtk::TextTag`, CodeMirror `Decoration`,
  `NSAttributedString` attrs, Compose `SpanStyle`); reveal marker runs near
  the caret. (§ Compose-field inline markdown styling.)
- Image lightbox UI.
- OS notifications — the *firing* only; moved 2026-09-28 to
  [`../behavior/message-banners.md`](../behavior/message-banners.md) § Where logic lives → *App glue*.

**Per-rail backends.** `FaunaMlsBackend` is complete — send + lazy group
bootstrap, the `poll_inbound_conv` / `ingest_welcome` receive drivers,
add/remove/rename membership, and the keypackage lifecycle — reached natively
via the `ConversationsSession` seam (`libs/fauna-conversations::session`) — over
UniFFI (`libs/fauna-ffi`) for Windows/macOS/iOS, and in-process for linux — which
the native factory wires **dual-rail**: `from_parts`/`from_manager` registers
FaunaMls, `register_smtp` adds the SMTP send rail, and `register_mail_receive`
wires the `INBOX` + `Sent` mail read-feeds the loop polls (the native twin of the
wasm wrapper's dual-rail `with_conversations`); on web via the wasm wrapper.
Per-app app-glue wiring: web wired end-to-end via the wasm wrapper; the four
native apps (**linux / Windows / macOS / iOS**) wired over the
`ConversationsSession` seam incl. the push-driven `start_receive_loop` driving both
conv and mail receive — linux in-process via `from_manager` over its
`host::manager()` singleton, the others via the `fauna-ffi` factory; Android
pending). `SmtpBackend` ships its send path and `poll_inbound_mail` its receive
driver in this slice.
The deferred `BlueskyBackend` and `ActivityPubBackend` stubs were deleted
2026-10-02 in favour of the one `BridgedBackend` (§ *The `Bridged` adapter*,
ruling 3), and `NostrBackend` with its seams followed on 2026-10-03, once
the Nostr leg's rows and kinds had moved onto the family.

**The `Bridged` adapter and this chain's half of the room model (TP8 + TP10, ratified 2026-09-05 — the third-party integration chain; the shared-Rust slice built 2026-10-02, § Implementation status today).** Three rulings, in the order they depend on each other.

1. **Transport adapters are separate from membership; principals with keys may be members; a room's confidentiality class derives from its member set.** All members user devices ⇒ end-to-end (MLS); the user's home nest a member ⇒ nest-readable community room; a bridge principal or a mail transfer agent a member ⇒ perimeter class, transport-only, labeled honestly. `ThreadEncryption` therefore becomes a **derived** field computed in shared Rust from the roster — derived on every rail but the Nostr leg since 2026-10-02 (`room::transport_room` seats the bridge or mail transfer agent), and a per-rail constant in `derive_capabilities` (`libs/fauna-conversations/src/capabilities.rs`) only there, until the leg migrates. Under this rule "never presume a bridge is an MLS member" (TP12) is a theorem: a bridge can only be a member of a room whose class already says the bridge reads it, and an end-to-end room is by definition one with no such member. **The room model itself — nest-readable community rooms, a single home nest per room transferable by succession, per-room history-for-joiners policy, roles enforced cryptographically in end-to-end rooms and at the floor in community rooms, and the duplicate `fauna.conversations.group.*` plane's retirement as the last step (executed 2026-09-26) — is ratified in [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) (2026-09-08), not here**; this chain contributed exactly one constraint to it: the bridged family never depends on the group plane in either form ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Bridged rooms).
2. **One variant, one backend.** `Rail::Bridged` is the only enum change (`libs/fauna-conversations/src/address.rs` — the closed, exhaustively-matched `Rail`), and one shared-Rust `BridgedBackend` serves every bridge. Per-bridge identity — id, glyph, address grammar, capability vector — comes from the principal's metadata document at registration ([`../architecture/third-party.md`](../architecture/third-party.md) § The manifest's `bridge` block, whose `capabilities` is the `ThreadCapabilities` record verbatim); no per-bridge Rust, no per-bridge app change. **How the identity rides (ruled 2026-10-02; refutable until the first bridged room renders):** `Rail::Bridged` is a **unit** variant — `Rail` stays `Copy`, backend registration stays keyed per rail, and a unit variant cannot read a declared value, so the 2026-09-05 sentence "the `Rail::glyph` match and `derive_capabilities` gain one arm each that read the declared values" is corrected to what the code can do. (a) **The thread carries the identity:** `ThreadSummary` and `ThreadDetail` gain an additive `bridge: Option<BridgeIdentitySnapshot { id, label, glyph }>`, filled by the manager from the backend's registry for a `Bridged` thread and `None` on every other rail; the snapshot's existing `glyph` field — what every app already paints — is the declared glyph on a bridged thread, and `Rail::glyph` answers the generic `SourceGlyph::Bridge` only as the fallback for an identity-less read. (b) **The capability vector is overlaid where the trait already takes the thread:** `derive_capabilities(Bridged, _)` answers the most restrictive vector (every affordance withheld, `delivery_mode: Async`), and `BridgedBackend::capabilities(thread)` replaces it with the vector declared for `thread.bridge.id` — the same read-time overlay `RoomSnapshot::gate` performs for roles; `encryption` is never declared, it is the room's derived class (ruling 1), and the backend's `room_state` seats the bridge principal, so the class is transport-only by construction. (c) **The address is `TypedAddress::Bridged { bridge_id, address }`** — the far network's own spelling, `rail()` → `Bridged`; the test agents' inject commands (`conversations_inject_*`, convention 11) name the bridge id beside the rail through a `TypedAddress::unresolved_bridged(bridge_id, raw)` twin of `unresolved_for_rail`, which cannot answer `Bridged` from a bare string. (d) **The address grammar is enforced nest-side only** — compiled at manifest resolution and matched at `conversation.rooms.open` (bounds: [`../architecture/third-party.md`](../architecture/third-party.md) § The manifest), so no app ever compiles a third party's pattern: the recipient picker lists each registered bridge by its declared label, and a typed address resolves through the backend's `resolve_address`, which asks the nest (the Fauna rail's own shape) and surfaces its typed refusal. (e) **The first-party legs are bridges of the same shape:** the in-process Nostr leg registers identity `{ id: "nostr", label: "Nostr", glyph: "bolt" }` with its declared vector (today's `(Nostr, _)` constants minus `encryption`), so a Nostr DM thread renders ⚡ as before. The wire is `fauna.bridges.conversation.{deposit, outbox.fetch, outbox.ack, room.upsert, room.members, receipt}` for the bridge and `fauna.bridges.conversation.{rooms.list, rooms.open, inbox.fetch, send}` for the user's app, plus the push `fauna.bridges.push.conversation_changed` — mail's shape generalized; the kinds' existence and caller classes are owned by [`../architecture/apps/bridges.md`](../architecture/apps/bridges.md) § Bridge-kind catalogue → Phase G. **The user-side contract (ruled 2026-10-02, refutable as above):** `rooms.list` is the caller's bridged rooms across every bridge serving the account, each row carrying the bridge identity, the far room id and participants, the declared vector, `bridge_x25519` (the key an outbound item is sealed to — carried on the row so a first-party leg with no roster row needs no join against `fauna.principals.list`), `last_at`, the family gate's `guardian_state`, and — for a room no live principal serves — `disconnected`, which the glue honours by registering identities from connected rows only, so the thread falls to the rail's most-restrictive vector under (b) (the fate itself: [`../architecture/apps/bridges.md`](../architecture/apps/bridges.md) § Bridge-kind catalogue → Phase G → *When the bridge stops serving*); `rooms.open` finds or mints the room for `(bridge_id, far_address)` — idempotent, grammar-checked, the floor roster seated with the user and the bridge principal ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Bridged rooms); `inbox.fetch` is one room's rows in **both** directions, sealed to the user — inbound deposits and the caller's own Sent copies — ordered and paginated on the nest's `received_at`, never the far side's claimed timestamp (the Nostr lesson, [`nostr.md`](nostr.md) § Implementation status today), with the reply-level `guardian_state`; `send` takes `{ room_id, sealed_for_bridge, sealed_for_self }` — the item sealed to `bridge_x25519` goes to the outbox the bridge drains, the copy sealed to the user's own recipient key is the Sent row — and refuses a guardian-`block`ed peer (the gate's outbound rule) while seeding the `allow` row; `conversation_changed { room_id }` is the nudge, and carries no content. Read state is the native thread's carrier, keyed by room id ([`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md)). **The nest is blind in both directions:** inbound, the bridge fetches the user's registered recipient key (`fetch_recipient_mls_pubkey`, widened to `ThirdParty`), HPKE-seals each message to it, deposits ciphertext; outbound, the user's app seals the message to the **bridge principal's** X25519 key and the nest queues ciphertext only the bridge opens. The one honest exception is a first-party in-process leg whose key the nest itself holds (the custodial Nostr signer, [`nostr.md`](nostr.md) § Architecture): there the nest opens the outbound to wrap and relay it, which is exactly what the room's transport-only label already says. A bridged room is a room whose transport is the bridge and whose member set includes the bridge principal — hence transport-only by derivation; membership ops reach the bridge only where its vector says so. No first-party bridge does better today; TP11 migrates them onto the same shape.
3. **The three first-party legs migrate onto the family as internal callers, and the deferred per-rail backends are superseded.** The Nostr DM ingest ([`nostr.md`](nostr.md) § Implementation status today → DMs), the Bluesky DM poll (a consume-side linked account's `chat.bsky` conversations, read under its OAuth session — never a nest-hosted-backed account's, whose chat traffic rides the proxy path: [`../behavior/atproto-pds-full.md`](../behavior/atproto-pds-full.md) § D7) and the ActivityPub direct-message ingest (the direct Notes the audience gate keeps out of the public projection, [`../behavior/activitypub.md`](../behavior/activitypub.md)) all become internal callers of the deposit family, and `BlueskyBackend` / `NostrBackend` / `ActivityPubBackend` — the three deferred stubs the paragraph above names — are **deleted in favour of the one backend** rather than registered per app. First-party use proves the seam before any third party arrives. Feed legs go generic the same way ([`feed.md`](feed.md) § Implementation status today, `SourceKind::Bridged`); identity legs are third-party-implementable in the target state through the oracle and nest-terminated ingress, their build order step 4's. **Deletion timing (ruled 2026-10-02):** `BlueskyBackend`, `ActivityPubBackend`, `Rail::{Bluesky, ActivityPub}` and `TypedAddress::{Bluesky, ActivityPub}` go with the adapter's first slice — no app registers the backends and nothing produces their addresses (the one e2e injection of a Bluesky thread becomes a bridged one); `NostrBackend`, its `NostrDmSink` / `NostrDmSource` seams, `Rail::Nostr` and `TypedAddress::Nostr` go with the Nostr leg's migration ([`nostr.md`](nostr.md) § Implementation status today → DMs). Each is a code-only alpha-carve-out deletion — no user data, no approval gate, said so in the commit. A variant a pre-deletion build wrote into a drafts blob would read back as the carried `Unknown` arm; under the 2026-09-24 baseline reset no such blob exists. Every exhaustive `Rail` / `TypedAddress` / `SourceGlyph` switch in the six non-Rust apps gains and loses its arms in the same change (cross-area cleanup rule 1).

## User actions

All action handlers route through `ConversationsManager` (the
shared-Rust singleton). Apps observe snapshot updates back.

| Element | Action | Where it runs |
|---|---|---|
| `new-conversation-button` | Open in-pane new-thread compose. | `manager.start_new_conversation()`. |
| `conversation-item[i]` | Select thread. | `manager.select_thread(id)`. |
| `conversation-sort` | Advance the sort order one step round the cycle (latest activity → oldest first → unread → latest activity). | `manager.set_sort(next_sort_order(current))` — the cycle is `fauna_conversations::snapshot::next_sort_order` (§ Where logic lives). |
| `recipient-picker-input` | Type recipient. | `manager.set_recipient_input(text)`; on Enter → `manager.resolve_recipient()` then `manager.accept_recipient_chip()`. |
| `recipient-picker-chip[i]` (× button) | Remove chip. | `manager.remove_recipient_chip(i)`. |
| `recipient-picker-suggestion[i]` | Click to accept suggestion. | `manager.accept_suggestion(i)`. |
| `topic-toggle-button` | Reveal/hide subject input. | `manager.toggle_topic(thread_id)`. |
| `subject-input` | Edit subject draft. | `manager.set_compose_subject(thread_id, text)`. |
| `dm-text-field` | Edit body draft. | `manager.set_compose_body(thread_id, text)`. |
| `attachment-button` | Open file picker, stage attachment. | App glue (native file picker) reads the picked file's bytes → `manager.add_attachment(thread_id, filename, mime_type, bytes)` (new-thread compose: `add_new_thread_attachment(filename, mime_type, bytes)`). The manager hashes the bytes (BLAKE3 → `blob_hash`), caches them in its attachment store, and stages a light `AttachmentDraft` on the compose; `send` re-resolves the bytes from the store. Remove via `remove_attachment(thread_id, index)`. |
| `dm-send-button` | Send. | `manager.send(thread_id)` for existing threads; `manager.send_new_thread()` for new compose. For new compose, Send first **flushes a typed-but-uncommitted recipient** — if the picker holds raw input but no committed chip (the user typed an address and clicked Send without pressing Enter / clicking a suggestion), `send_new_thread` runs the same `resolve_recipient` → `accept_current_recipient_chip` the Enter handler does, so Send never silently drops a pending recipient. |
| `dm-reply-button[i]` | Pre-fill reply context. | `manager.set_reply_to(thread_id, msg_id)`. |
| `dm-reply-cancel` | Clear reply context. | `manager.clear_reply_to(thread_id)`. |
| `thread-add-participant-button` | Open the add-participant overlay. | `manager.open_add_participant(thread_id)`; the overlay's picker drives `manager.set_add_participant_recipient_input` / `accept_add_participant_chip`; confirm → `manager.confirm_add_participant()`. On a `FaunaMls` 1:1 this forks a new `MlsGroup` thread (then selects it); on every other `(rail, flavor)` it adds in place. Cancel → `manager.cancel_add_participant()`. Adding someone already in the group is an idempotent no-op, and a retry after a lost Welcome heals the half-added member; the mechanism and its same-nest-only limit are owned by `../architecture/mls-group-key-material.md` § M2 *Admitting a member*. **A failure rolls the optimistic list mutation back and surfaces on `error-message`** (§ Errors & edge cases). |
| `recipient-picker-input` Enter / `recipient-picker-suggestion[i]` | Commit current text as a chip on the active picker. | `manager.accept_current_recipient_chip()` — add-participant overlay takes priority over new-thread compose. |
| `thread-rename-button` | Rename thread. | `manager.rename_thread(thread_id, label)`. Visible iff `capabilities.supports_rename`. A failed wire op surfaces on `error-message` (§ Errors & edge cases). |
| `room-leave-button` → `room-leave-confirm` | Walk out of the room. | `manager.leave_room(thread_id)` — one verb over the one self-scoped `room.leave` door on every class (`../behavior/conversation-rooms.md` § Roles and authorization → *Leaving — the mechanism*), so no app branches on class. Gated on `capabilities.can_leave_room`, which the roles table closes for the owner. Acts immediately — never staged through `room-settings-save-button`. A refusal surfaces on `error-message`; the thread stays in the leaver's list. |
| `recipient-picker-home-nest-toggle` | Seat (or unseat) the home nest in the room about to be created. | `manager.set_new_thread_home_nest(include)`; `send_new_thread` then **founds** a community room through the `room.create` ceremony when the prospective class is community (`../behavior/community-rooms.md` § The three classes). |
| `room-invitation-accept-button[i]` | Accept a standing community-room invitation. | `manager.accept_room_invitation(id)` — seats the account on the floor and selects the room's thread, which the page opens; it reads nothing until an owner's or admin's device keys it in. A refusal surfaces on `error-message` and the list is re-read. |
| `room-invitation-decline-button[i]` | Decline it. | `manager.decline_room_invitation(id)` — the invitation stops standing; the room is not told. |
| `room-nest-read-toggle` | Stage the home nest's read of a community room. | `RoomSettingsDraft::toggle_nest_read()`; `room-settings-save-button` commits it as a rotation with the nest in or out (`set_room_nest_read`), before any hand-over. Withdrawing it deletes every view the nest built. |
| `room-labeler-toggle[i]` / `room-labeler-inspect-button[i]` | Name or un-name a labeler the home nest runs on the room; inspect it first. | Toggle → `RoomSettingsDraft::toggle_labeler(catalog_entry.labeler_id)` (staged; `room_settings_toggle_labeler` over FFI), committed by `room-settings-save-button` as one `RoomSettingsEdit::Labelers` → `manager.set_room_labelers`, before any hand-over. Inspect → `LabelerCatalogMachine::inspect(catalog_index)`, painted in place; `labeler-inspect-close-button` → `close_inspect()`. Opening the editor re-reads the catalog (`refresh()`). A refusal lands on `error-message`. |
| `thread-member-chip[i]` | Open participant detail / remove. | `manager.remove_participant(thread_id, addr)` (when supported). A failure puts the member back on the list and surfaces on `error-message` (§ Errors & edge cases). |
| `thread-member-keep-button[i]` | Close the open post-succession review items for that member. | `fauna_client_config::decide_member_review(store, person, Kept)`. Rendered only beside a raised `thread-member-unattested-mark`; the *Remove* half is the chip above, deliberately not re-rendered. Ruling owned by [`../behavior/succession-propagation.md`](../behavior/succession-propagation.md) § Propagation → *MLS groups*. |
| `markdown-bold-button` (and rest of markdown toolbar) | Wrap selected text with `**` / `*` / `` ` `` etc. | Wrap *rule* shared (`fauna_core::markdown::wrap_selection` via `wrapMarkdownSelection` — edge whitespace stays outside the markers); app glue does only the selection splice. Not a snapshot mutator. |
| `conversation-search-box` | Filter list. | `manager.set_search_query(text)`. |
| `dm-message-actions-button[i]` | Open the per-message actions flyout (`dm-message-actions-menu`: react / delete). | App glue (open flyout). Shown iff ≥1 action available for that message. Not a snapshot mutator. |
| `dm-reaction-option[i]` / `dm-reaction-pill[i]` | Toggle a reaction on a message. | `manager.toggle_reaction(thread_id, msg_id, emoji)` — resolves Add/Remove vs self's current state for that emoji. Gated `capabilities.supports_reactions`. |
| `dm-reaction-more-button` | Open the fuller emoji picker (any emoji); the picked or typed emoji toggles. Where the picker is a free-entry field (tui, apple, web, windows) the open field carries this same id — *entry mode*, § *Rendering / picker glue*. | App glue (native chooser or free-entry field) → `manager.toggle_reaction(thread_id, msg_id, emoji)`. Gated `capabilities.supports_reactions`. |
| `dm-message-delete-button` → `dm-message-delete-confirm-button` | Delete a message (cooperative tombstone) — your own; in a governed room an owner or admin may delete anyone's, by the role table [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization owns (build state: that doc's § Implementation status today). | `manager.delete_message(thread_id, msg_id)` — the manager rejects a target whose snapshot is not `can_delete`; the affordance is gated on `message.can_delete` (tui; the other six still gate `capabilities.supports_message_delete && message.is_own` until their lift). |

### Attachments — implementation status today

Moved 2026-09-28 to [`../behavior/conversation-attachments.md`](../behavior/conversation-attachments.md)
§ User actions → *Attachments — implementation status today*, under this same heading: the
unified `blob_hash` shape, *Retention*, the SMTP rail, *Staged-attachment preview*, the FaunaMls
rail, *Status + residuals*, *Privacy metadata*, and *C2PA on-device*. The `attachment-button`
row above and the per-app frontier rows in § Implementation status today stay here.

## Reactions & message delete

> **Spec:** (tracked internally, ratified with the user 2026-06-23). **FaunaMls-only today** via capability flags; mail/social light
> up when their backends + each rail's native semantics land.

Per-message **reactions** and **message delete** are the two per-bubble affordances on top of
send/reply/attachments. Both are **shared-Rust logic + thin app render glue with zero nest change**:
they ride the existing channel fan-out and the append-only `__conv` segment store as new
`ChannelMessageBody` variants sealed inside the MLS envelope — or, in a community room, inside the room's `RoomSealed` envelope, the sender its signed author ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization → *Delete any message — the mechanism* → *Community rooms* owns that class's two acts) — and the nest stays transparent (it stores
opaque envelopes and never opens them; § Encryption at rest). Capability-gated and observer-driven like
every other affordance (§ Architectural rules) — apps never branch on rail.

**Wire (`libs/fauna-mls`).** `ChannelMessageBody` gains `Reaction { target_seq, emoji, op }`
(`ReactionOp = Add | Remove`) and `Delete { target_seq }`. `target_seq` is the target message's nest
segment-store `seq` (the cross-member-stable identity behind `MessageId("conv:{channel}:{seq}")`). A
Reaction/Delete is not a user-visible message — the manager consumes it and renders no bubble (like
`GroupMeta`).

**Capability.** `supports_reactions` (already modelled, FaunaMls-only) gates reactions;
`supports_message_delete` (new, FaunaMls-only) gates delete. Other rails are `false` until their backends
implement the rail-native semantics.

**Manager + snapshot (`libs/fauna-conversations`).** `MessageSnapshot` carries the derived
`reactions: Vec<ReactionGroup>` (`{ emoji, count, reacted_by_me }`, ordered by first-added time),
`deleted: bool`, `is_own: bool`, and `can_delete: bool` — whether this viewer may delete the message, derived on every projection (§ State & data shape). Two actions route through
`ConversationsManager`:

- `toggle_reaction(thread_id, msg_id, emoji)` — resolves Add vs Remove against self's current state for
  that emoji (one emoji per user per message; multiple distinct emojis allowed), optimistically updates
  the aggregate, posts a `Reaction` channel message.
- `delete_message(thread_id, msg_id)` — the manager rejects a target whose snapshot does not say
  `can_delete` — posts a `Delete`, optimistically marks the target `deleted`. `can_delete` is the
  sender's own message everywhere; who else may delete in a governed room is the role table's
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization →
  *Delete any message — the mechanism*, which owns the rule and its build state).

**Ingest (`poll_inbound_conv`)** folds the variants into the derived state: a `Reaction` updates the
target's aggregate (any member may react); a `Delete` sets `deleted` **only if
`delete.sender == target.sender`** (a forged cross-sender delete is dropped — the security floor is
enforced on ingest, not just at the action). A `Reaction`/`Delete` arriving before its `target_seq` is
buffered and applied on the target's arrival. **Which reaction op wins a `(reactor, emoji)` pair is
the author's own signed stamp, not the log's order** — a community room's record can be re-appended
at a fresh `seq` by any member, so the fold reads the set of signed ops rather than the sequence;
rule and reasoning owned by [`../behavior/community-rooms.md`](../behavior/community-rooms.md)
§ The three classes → *Community* → *Who wrote it*. Display order is unaffected (below). The sender match is the as-built rule and stays the floor:
the owner/admin delete the room model promises
([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization) is a
second, role-checked admission beside it — owned there, with its build state — never a loosening of it.

**Affordances (ui.yaml).** A single `dm-message-actions-button[i]` ("⋯") per bubble opens
`dm-message-actions-menu` — reaction quick-set (`dm-reaction-option[i]`, fixed shared order
👍 ❤️ 😂 😮 😢 🙏) + `dm-reaction-more-button` (the fuller picker), `dm-message-delete-button` →
`dm-message-delete-confirm-button` (on a bubble whose snapshot says `can_delete` — *Manager +
snapshot* above; tui reads it, the other six still read `is_own` until their lift), and `dm-message-mark-as-spam-button`
(spam-model training — behavior owner [`../behavior/mail-spam.md`](../behavior/mail-spam.md)
§ Wire shapes), and `dm-message-report-button` (the report verb — behavior owner
[`../behavior/moderation.md`](../behavior/moderation.md) § User-initiated reporting; IDs
user-approved 2026-09-25, unbuilt on every app). The ⋯ button is shown iff ≥1 action is available for that message
(keeps mail bubbles clean). Aggregated reactions render under the bubble as `dm-reaction-pill[i]`
(emoji + count, own highlighted), tap-to-toggle. A deleted message renders a localized "This message was
deleted" placeholder (body/attachments/reactions stripped). The inline `dm-reply-button` is untouched.

**Rendering / picker glue.** Uniform on all 7 apps (quick-set, pills, placeholder, ⋯ flyout). The
**only** sanctioned divergence is the "more" picker *widget* — the same class of app glue as the
attachment file-picker — and it comes in exactly two families, both of which reach **any** emoji
(the outcome the picker exists for): a **native chooser** where the platform has one a button can
raise (GTK `EmojiChooser` on linux, emoji2 `EmojiPickerView` on android), and a **free-entry emoji
field** everywhere else (tui, apple, web, windows) — a single-emoji input that commits on Enter and,
on a GUI app, also as soon as one complete emoji is in it, so an OS emoji panel (the macOS character
palette, the iOS emoji keyboard, Win+. on windows, a browser's own) picks and commits in one gesture,
and typing or pasting works with no panel at all. While the field is open it carries the
`dm-reaction-more-button` id itself (*entry mode* — the shape `attachment-button` already has on tui,
a button whose free-entry form is a committing input under the same id; ui.yaml's description
declares it, user-approved 2026-09-28). Web and windows keep their twenty-emoji **shortcut grid**
beside the field; its list lives once in shared Rust (`fauna_conversations::MORE_GRID_EMOJIS`, the
`quickset_emojis` shape over UniFFI and wasm) and its cells carry no id. A fixed grid *alone* is not
a fuller picker — it cannot reach an emoji outside itself, which is what web and windows shipped
before this ruling. The resulting "emoji string → `toggle_reaction`" is shared. The e2e seam is one
action for all seven (`react_with_custom_emoji`): click the button, type the emoji at the id (a
native chooser's driver emits the chooser's own pick; a field receives the text), and on tui click
once more for Enter; on apple the field commits by itself, so there is no second click. Ruled
2026-09-28; build state per app: outcome 5 of
`docs/features/reactions-and-message-delete.md` § Status (linux and tui green at the ruling, macos
and ios since, windows 2026-09-29; android's chooser-pick driver arm is built and unit-pinned, its
recorded run waiting on an android run venue; web owes the field itself).

**At rest.** Reaction/Delete are normal sealed `__conv` envelopes — the same `Conversation messages`
encryption-at-rest row, **no new representation**. The aggregate and the deleted flag are derived —
but **not re-derivable**, so they ride the `history/<ch>` replica slice rather than being replayed: an
own `Reaction`/`Delete` is MLS-opaque to its own author off the log, and a restored device resumes past
every record it already folded, so a relaunch that replayed would lose every tombstone and pill it made
itself. What the slice carries, and why the fold alone is not enough, is owned by
[`../behavior/devices.md`](../behavior/devices.md) § Cross-device MLS group-state sync. The tombstone
**never hard-deletes** the target
envelope (No-user-data-loss is iron-clad; MLS can't recall a delivered message) — "delete for everyone"
is the standard cooperative marker compliant clients honor. The same holds for the target's attachment
blobs: a `Delete` is sealed and never a store tombstone, so the nest keeps pinning them for the record's
life (§ Encryption at rest → *Attachment reachability*).

**Implementation status.** Shared Rust + **all seven app render legs are
landed** (2026-06-24; § Implementation status today). Shared Rust:
`ChannelMessageBody::{Reaction,Delete}` + `ReactionOp` (`fauna-mls`);
`supports_message_delete`; `ReactionGroup` + `fold_reactions` +
`MessageSnapshot.{reactions,deleted,is_own}`; manager `toggle_reaction`
(optimistic) + sender-only `delete_message`; ingest via `poll_inbound_conv` →
`apply_inbound_{reaction,delete}` (forged-delete drop + out-of-order buffering,
validated at projection time); UniFFI + wasm exports. Every app renders the
same shape (⋯ menu, quick-set, pills, tombstone) capability-gated off the
snapshot; the only sanctioned divergence is the "more" picker widget
(§ Rendering / picker glue). tier_3 `test_conversations_reactions.py`
(incl. the cross-engine `test_react_cross_member_peer_sees_pill`) +
`test_conversations_message_delete.py` are green per app (android
host-emulator-gated; Robolectric covers its render). One e2e gotcha worth
keeping: own-message seeding is per-app — every mock-backend app uses the
compose-send echo, but **linux** (real FaunaMls backend at login) seeds via
`ConversationsManager::inject_own_for_test`.

**Durability across a relaunch (2026-09-20).** Both derived states now ride the `history/<ch>` replica
slice, closing the gap this doc's *At rest* paragraph described the wrong way round for three months
(it claimed stream replay rebuilds them; nothing could). `ConversationsManager::snapshot_channel_slice`
— the one door both slice writers take — projects the same fold `thread_detail` serves the apps
(`project_reactions_and_deletes`, one function, two callers) onto the slice's messages and records the
raw state beside it: `deleted_messages` and the per-message reaction log (`reaction_log`, stamped), both additive and
omitted when empty, so a slice with neither is byte-identical to one written before the fields and an
older client still renders the folded form. `restore_channel_slice` re-seeds the manager's `deleted`
set (union — a tombstone is monotone) and its reaction log (fill-never-overwrite — what this device
logged itself outranks another device's copy). `merge_history_slices` unions the tombstone rather than
taking the higher-watermark side's copy, since an own delete is opaque to its author and the deleting
device need not be the furthest-polled one. Pinned by
`a_tombstone_and_its_reaction_pills_survive_a_history_slice_restore`
(`manager_integration_tests.rs` — own delete, an admin's cross-sender delete, the pills, and a reaction
toggled *after* the restore, which is the leg the folded aggregate alone cannot pass) and the
`store::history` merge tests. Contents authority: [`../behavior/devices.md`](../behavior/devices.md)
§ Cross-device MLS group-state sync.

**A merge no longer costs a device its own reactions (2026-09-22).** Two devices that each reacted on
a message while their replicas were forked now BOTH keep their pill through the slice merge, where
before the further-polled side's log won whole — an own reaction is MLS-opaque to its author, so the
reacting device need not be the further-polled one. The rule, and the one case it still cannot cover,
are owned by [`../behavior/devices.md`](../behavior/devices.md) § Cross-device MLS group-state sync.

**Sender authentication (2026-06-27, MLS-1).** The delete floor above ("a `Delete` sets `deleted` **only if
`delete.sender == target.sender`**") and every sender-attributed bubble/reaction rest on
`ChannelMessage.sender` being trustworthy. That anchor is enforced in shared Rust at
`fauna-mls::engine::decrypt`: after MLS `process_message` authenticates the sending leaf, `decrypt`
**overwrites** the returned `sender` with that authenticated leaf's `ActorId`, so the self-asserted value
inside the encrypted payload is never trusted. The rule is simply **the authenticated leaf is the sender**; the decrypt-side bind holds it for all seven
apps at once (no wire/struct change). The CalDAV scheduling rail is unaffected — its receive path re-authenticates via the iMIP `ORGANIZER` and never reads `cm.sender`.

**Leaf-credential binding (2026-06-27, MLS-2).** The MLS-1 bind trusts the *authenticated leaf credential*
as the sender. MLS authenticates only the leaf *signature*; the leaf `BasicCredential` (the 32-byte
`ActorId`) is opaque to MLS, so the protocol itself does not tie the identity a leaf *names* to the key it
*signs with* — that bind is ours to make, one layer below MLS-1, and every consumer of the credential (the
sender bind above, the roster) rests on it. Because a Fauna `ActorId` *is* the
Ed25519 public key (`fauna-core::identity`) and the MLS leaf signer *is* that key
(`build_credential_and_signer`), an honest leaf always satisfies `credential == leaf signature key` (the
same 32 bytes). Shared Rust now **enforces that equality for every leaf admitted to a group**, with a typed
rejection on a mismatch: at `create_group`/`add_member` (invited KeyPackages), in `process_commit` (every
leaf a commit introduces or updates — Add/Update proposals + the committer's update-path leaf, validated
before the merge), and on `join_from_welcome` (the whole learned roster, catching a malicious inviter whose
own leaf is forged). The nest adds a defense-in-depth check at the `keypackage.upload` choke point
(`fauna-mls::engine::verify_uploaded_key_package`): an uploaded KeyPackage is rejected unless its inner
credential == its leaf signature key == the authenticated uploader, so the nest never stores or serves a
forged-identity KeyPackage. One 32-byte equality protects every credential consumer at once — the sender
bind, `group_members()`/roster, and `find_leaf_by_identity` — for all seven apps with no wire/struct change.
One consumer up, the private contact overlay's paint gate rests on the same roster: a nickname paints on a
member chip only once the participant's actor id is a verified leaf of the thread's group
([`contacts.md`](contacts.md) § The private overlay → *The paint gate* owns that rule).
Reference: tracked internally (design review, Part 2).

**Deferred (captured in the spec):** delete-for-me (local hide, needs a synced per-user hidden-set);
reactions/delete on the mail + social rails (each rail's native semantics behind the same capability
gates). **No longer deferred (2026-09-19):** an owner's or admin's delete of another member's message —
listed here as deferred on 2026-06-23, then promised by the room model ratified 2026-09-08; it is
[`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization's to
own, and that doc's § Implementation status today declares what is built.

## Persistence

- **Thread + message store** is **nest-resident**, synced over the
  protocol per
  [`../behavior/direct-messages.md`](../behavior/direct-messages.md)
  and per-rail bridges. At-rest property for fauna-native MLS
  conversations: see § Encryption at rest below +
  [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md)
  § Per-content-kind conformance rows `Conversation messages` and
  `Group memberships`.
- **Client-local cache** holds MLS group state and decryption keys
  (per `direct-messages.md` §6) plus profile-cache display names.
- **Drafts — moved 2026-09-28 to [`../behavior/conversation-drafts.md`](../behavior/conversation-drafts.md)
  § Persistence**, under this same heading: *Per-thread `ComposeState`* (drafts v2 and its
  implementation status), *Only user-authored input rests; transient UI state does not*,
  *A restore FILLS*, *A restore that FILLS a recipient picker owes it a probe*, *A restore the
  outgoing account started fills nothing after an identity change*, *A restored draft's
  attachment is a handle, not a file*, and `new_thread_compose` (the single-slot new-thread
  draft that persists across switching).
- **Backup of fauna-native conversation history** flows through the
  per-channel `__conv` reserved folder (the conversation kind on the
  message segment store); see
  [`../behavior/backup-restore.md`](../behavior/backup-restore.md)
  §§ 5–6 (per-kind snapshot create / restore) and § Choosing a
  destination's capability, plus
  [`../architecture/message-segment-store.md`](../architecture/message-segment-store.md).
  Conv ingest/reads landed Plan 7; conv compaction + conv snapshot
  create/restore (channel-scoped, membership-authorized, no placement
  layer) landed Plan 8; conv cross-location backup + the `__conv/<channel>`
  backup destination land Plan 9.

## Encryption at rest

Moved 2026-09-28 to [`../behavior/conversations-at-rest.md`](../behavior/conversations-at-rest.md)
§ Encryption at rest, under this same heading: the three at-rest representations of a
fauna-native conversation (*Conversation message envelopes*, *Attachment reachability*,
*MLS-channel routing roster*, *MLS-cryptographic group state*), *Per-rail bridge DMs carve-out*,
*Receiving into the conversations view (per-rail)*, *Community rooms*, *Anti-spam metadata
carve-out*, *MLS Welcome at-rest*, and *Conformance* — this page's conformance section against
[`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Per-content-kind
conformance.

## Errors & edge cases

- `error-message` — page-level surface for unrecoverable errors, fed by
  `ConversationsSnapshot.error: Option<LocalizedText>` (§ State & data shape).
  **Two manager-owned truths reach this one element, and they do not overlap:**
  the snapshot's `error` carries the **membership/label wire ops**
  (`confirm_add_participant`, `remove_participant`, `rename_thread`); the
  *active* compose's `ComposeState.send_state` carries **sends** (below). One
  gesture, one truth — an app renders the snapshot error when set and falls
  back to `send_state`, which is safe because the page error is always the more
  recent of the two by construction: **every producer clears it on entry,
  `send`/`send_new_thread` included**, so a stale membership failure can never
  mask a fresh send failure, and a success never leaves the page accusing the
  user of a failure they already recovered from.
  **One producer is not a gesture (2026-09-09):** a room's outgoing owner is
  told at its own commit fold that the hand-over it offered was overtaken and
  must be offered again
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § Roles and authorization → *Ownership transfer*). It writes this same slot
  rather than standing beside it, because a superseded offer is a **one-time
  event, not a standing refusal**: clear-on-entry is what should retire the
  notice at the owner's next gesture, which is exactly the property that made
  the slot wrong for the standing refusal below.
  **A third, higher-precedence truth landed 2026-08-15 (W5.6 (account-data-plane.md § Workstreams)) beside — not
  inside — the two above: a standing "served in another instance" refusal.**
  On a *retired* app (`ServingMode::Concurrent`; tui and linux so far), a
  same-account instance that lost the conversations-engine role lock
  (`MlsError::ServedElsewhere` at engine construction) calls
  `ConversationsManager::set_engine_served_elsewhere(true)`; the flag is read
  back via `engine_served_elsewhere()` with **top precedence** over both the
  snapshot error and `send_state`, deliberately **not** folded into
  `ConversationsSnapshot.error` — that slot's clear-on-entry contract (the
  paragraph above) would mask a standing refusal the moment an unrelated
  gesture ran. Role-lock mechanism owner:
  [`../architecture/account-runtime.md`](../architecture/account-runtime.md)
  § Multi-instance concurrency; which apps can even reach this state (their
  `ServingMode`) owner:
  [`../architecture/apps/account-scoping.md`](../architecture/apps/account-scoping.md)
  § Concurrent instances — android/iOS never do, one instance per app by
  construction.
  **A fourth truth, standing like that refusal and ranked directly under it
  (2026-09-13): the receive rail stopped.** When a receive pass panics, the
  shared receive loop's supervisor records the exit
  (`ReceiveLoopExit::Panicked`) and sets
  `ConversationsManager::receive_stopped()`; the app shows
  `conversations.errors.receive_stopped` — new messages stopped arriving
  because of an internal error, restart the app (or reload the page) — above
  the snapshot error and `send_state`, and no gesture clears it. **The loop is
  not restarted in place, and the session does not rebuild itself.** The
  panic's broken invariant is unknowable — locks the pass held are poisoned,
  and the MLS engine's in-memory group state may be half-applied — so a
  re-armed loop could fold over that state and let the replica autosave persist
  it, trading a dead rail for lost group state; a factory rebuild would first
  retire the poisoned predecessor through locks the same panic may have
  poisoned. Restarting the app is recoverable by construction: a fresh process
  restores from what was durably written. A designed exit (the session dropped,
  the engine handed over) never sets it, and a later loop over the same manager
  retires it. **On web** a panic aborts its wasm task and the pass's promise
  never settles, so the SPA's receive pump bounds each rail's pass with a named
  ceiling and shows the same notice while a pass has outrun it
  (`$lib/receive-pump`); a stalled pump is never re-armed over the possibly
  poisoned wasm side, and a pass that settles after all — slow, not dead —
  clears the notice. Before this a dead rail was silent on every app: the
  2026-09-09 web outage
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § Implementation status today) denied every web session's receive rail for
  three days with no user told.
  **A fifth truth, the floor of the stack (2026-09-15): received mail this
  run could not open.** A mail record the receive path cannot open under the
  account's complete standing key set is skipped, not blocking
  ([`../behavior/mail-app-surface.md`](../behavior/mail-app-surface.md)
  § Inbound client receive → *Unopenable records* owns the rule and the
  reasons a record ends up so), and the manager counts every such skip
  (`ConversationsManager::unopenable_mail_count`); the app shows
  `conversations.errors.mail_unopenable` with the count — some received
  messages could not be opened on this device and were skipped — **ranked
  below every other truth**, so a fresh failure of any gesture, a dead rail
  or the role refusal all outrank it (it is a floor, never a mask), and no
  gesture clears it. It clears only as the records open after all: the ledger
  is per app run, a relaunch re-drains every feed with the account's current
  keys, and each record that opens retires its own entry. The count, not the
  records: the rail cannot say who the mail was from, since that is inside the
  seal.
  **The membership ops also roll their optimistic snapshot mutation back on
  failure** — each mutates first and fires the wire op second, so without the
  rollback a failed add renders someone as a participant of a group they are not
  in, and a failed remove hides someone who is still in it; both are the inverse
  of the "rendered truthfully as not-yet-shared" property
  [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
  § M2 *Admitting a member* requires. Each rollback undoes only what *its own*
  gesture changed (`add_participant_to`/`remove_participant_from` are
  idempotent, so a duplicate gesture must not evict a real member), and both are
  sound because an `Err` from either wire op provably leaves group membership
  unchanged — both routes merge only on an accepted send. Surfacing the reason
  is what lets the heal's refuse-to-guess arm (a cross-nest member whose roster
  the owner's nest cannot read — same §) reach the user as the *specific,
  actionable* error that bullet requires.
  **Both producers stamp `BackendError::user_detail()`, never `Display`** — the
  send-slot taxonomy below governs this element, not one of its two producers
  (the same reason `send_state`'s `reason` is a `LocalizedText`).
  **The add's phantom-heal annotation is a localized `Refusal` (2026-08-06).**
  When the heal evicted a ghost leaf and the re-invitation then failed, the
  group is internally consistent and the person is simply not a member; that
  sentence is a product statement, so it rides
  `conversations.unified.error_add_participant_after_heal` with the underlying
  `user_detail()` as `{reason}`. It previously composed **hardcoded English**
  (§ Architectural rules 3) around `{e}` — `Display` — and re-wrapped
  `Transport`, which is now seam-only.
- `recipient-resolve-status` — single text element with state
  attribute (resolving / resolved / error / not-found). (The former
  split `resolve-status-resolving` / `resolved` / `error` IDs were
  removed from ui.yaml.) The state→(token, label) render viewmodel is
  shared Rust: `fauna_conversations::compose::recipient_resolve_status`
  (`ResolveStatusView { token, label }`; the `_from_variant` twin /
  wasm `recipientResolveStatus` takes the snapshot's serde variant
  name; idle carries no label). Apps render the returned view —
  no per-app state→text map; the element's styling stays
  per-app. Linux swapped 2026-07-16 (same commit as the lift);
  windows swapped the same day (both its conversations recipient
  picker and its folder share-sheet picker, which shared the
  control); web swapped 2026-07-16 (both its new-thread and
  add-participant pickers, which had carried twin `resolveStateAttr`
  / `resolveStatusText` switches); android swapped 2026-07-17
  (`ConversationsComposeBar.kt`); apple swapped 2026-07-18 (both its
  conversations `RecipientPicker` and its folder share-sheet
  picker — the latter was the drift fix: its 3-arm local copy
  blanked resolved/not-found, now covered via the shared 5-arm view).
  **All six pre-tui GUI apps now consume the shared view; tui does not** —
  `apps/fauna-tui/src/conversations/mod.rs`'s `resolve_status_text` /
  `resolve_state_name` are their own hand-rolled `ResolveState` switches
  (verified 2026-07-23), never migrated onto `recipient_resolve_status` /
  `ResolveStatusView`; output matches (same five states, same i18n keys) so
  this is a priority-#2 duplication gap, not a behavior bug.
- **The picker tells the truth (ratified 2026-08-29).** The `recipient-resolve-status`
  state is a statement about what a rail *confirmed*, never about what the
  typed text *looks like*, and a chip is only ever an address a rail confirmed.
  Three rules, all shared Rust (`ConversationsManager`), zero app edits:
  1. **Typing owes a probe.** `set_new_thread_recipient_input` /
     `set_add_participant_recipient_input` stamp `resolving` for any non-empty
     input (empty → `idle`); only the async `resolve_recipient` moves the state to
     `resolved` / `not-found` / `error`. The former synchronous shape-derived
     `resolved` — "it contains an `@`, call it resolved" — is retired: it said
     *Resolved* for a Fauna peer whose lookup had not even started. Every app
     already calls `resolve_recipient` on input change (and web/linux resolve
     again on Enter); an app that stops doing so now shows a picker parked on
     *Resolving…* — a loud failure, not a silent downgrade.
  2. **A rail's `error` is the answer.** `probe_address` stops at the first rail
     that answers `Error` (rails are probed FaunaMls-first); a later rail's
     syntactic claim on the same string — the SMTP rail resolves any
     `user@host` by shape — never overrides it. The status reads
     `conversations.unified.recipient_resolve_error` ("Lookup failed — try
     again"), `resolved` is empty, and no chip can be committed. When the
     FaunaMls rail answers `Error` for a foreign domain is ruled in
     [`../architecture/foreign-handle-resolution.md`](../architecture/foreign-handle-resolution.md) § Peer-auth
     model → *Discovery-failure semantics* (a domain this account's own evidence
     vouches for — or has not yet been able to rule out — whose nest does not
     answer; a domain *established* as never-seen falls through to email by
     design).
  3. **Enter commits only a probed address.** `accept_current_recipient_chip`
     commits the picker's `resolved` address and nothing else — no shape parse
     from `resolving` / `error` / `not-found` / `idle`. `send_new_thread`'s
     flush of a typed-but-uncommitted recipient resolves first and then commits
     by the same rule, so *Send* without Enter is equally honest (and a send
     with nothing committed fails loudly, as before).
  The e2e twin: `test_fauna_mls_cross_nest_roundtrip.py::test_known_peer_unreachable_never_downgrades_to_email`
  (status `error`, no chip, no thread on any rail) and
  `::test_first_contact_with_unreachable_authority_is_email`.
- Empty list, empty thread, send-while-disconnected — snapshot
  variants, not per-app checks. `ComposeState.send_state` is
  `Idle | Sending | Failed { reason }`, and `reason` is a
  **`LocalizedText`** — the same carrier the snapshot's `error` above uses,
  because architectural rule 3 governs the element, not one of its two
  producers. Sends have exactly one producer (`ConversationsManager::send`,
  which `send_new_thread` tail-calls), so it is always the single key
  `conversations.unified.error_send` with the backend's own rejection as
  `{message}` — no per-gesture key to choose, which is why the shared
  `SendState::failed` constructor owns the key and the e2e injection seam takes
  only the detail.
  **The send-slot taxonomy (ratified 2026-08-02): `{message}` carries
  `BackendError::user_detail()`, never `Display`, and only product statements
  ever reach it.** Every `BackendError` is one of three things, expressed at the
  type level so a producer cannot be vague about which:
  - **Product statements** — the payload IS the user-facing sentence, localized
    at the producer: `NeedsUpdate { message }` (classified upstream by
    `RpcError::localized`), `Refusal(String)` (i18n-table text — e.g. the
    inline-ceiling refusal's `error.email.too_large`, the no-handle refusal,
    the no-recipients refusal's `error.send.no_recipients`, the missing-attachment
    refusal's `error.send.attachment_missing` (§ Persistence); the seam's
    `ConvRpcError::Rejected` maps here), and `Transport(SeamMessage)` (the
    seam's best-effort user string by `ConvRpcError`'s own contract — localized
    for a recognised wire code, the raw transport sentence otherwise — **but
    never a foreign nest's own words**: see below). These pass through
    `user_detail()` verbatim.
    **Who answered decides whether their words may reach the user (2026-09-10).**
    A transport sentence from **our own** nest is ours to show. A **foreign**
    nest's response text is not: a conversation's home nest is chosen by
    whoever created the room — on an invitation, a stranger — so its error body
    is text a stranger wrote, and on this slot it would read as the product
    speaking (spoofing, phishing). A producer that holds a foreign responder's
    text therefore classifies it into a localized product sentence and sends the
    text to the log. The first such producer is the cross-nest attachment
    upload's no-verdict fallback
    (`fauna_client_conversations::classify_blob_upload_error`, native and web
    through the one function; `error.send.attachment_upload_foreign_failed`),
    and every non-2xx body read on the byte plane is bounded
    (`MAX_ERROR_BODY_BYTES`), so the responder does not choose the length
    either. Witnessed through the real native call site, not only the pure
    classifier, by `fauna_client_conversations::blob_put_foreign_upload_tests::a_foreign_home_nests_upload_failure_never_reaches_blob_put_callers`.
    **`Transport` is seam-only at the type level (2026-08-06).** Its payload is
    a `SeamMessage` newtype with a private field, so the variant is
    constructible only inside `backend.rs` — via `From<ConvRpcError>` or the
    named `BackendError::transport_from_seam` (the mail/nostr sinks, whose
    `Err(String)` is likewise a seam's user sentence). The accidental shape
    `Transport(e.to_string())` no longer compiles anywhere else. This is a
    *producer*-side guarantee: the map-side pins below check `user_detail()`
    arm by arm and are structurally blind to a producer that picked the wrong
    variant, which is how the leak in the next paragraph survived a sweep whose
    grep was the only thing standing at those construction sites.
  - **Conditions** — variants with no payload for the user: `AuthRequired` and
    `NotSupported` map to their i18n sentences (`error.send.auth_required`,
    `error.send.not_supported`).
  - **Diagnostics** — `Internal(String)`: "no key package available for …",
    "serialize welcome: …", dropped-handle and commit-gate faults. The payload
    is **never rendered**; `user_detail()` maps it to `error.send.generic` and
    the raw text goes to the log at the catch site
    (`ConversationsManager::send` logs `{e:?}` before stamping the slot).
  The retired catch-all `Other` carried product refusals and diagnostics in one
  variant, so ~30 raw internals reached `error-message` verbatim on all 7 apps
  (its `"other: {0}"` Display tag was the first half of the bug, fixed
  2026-07-30; the taxonomy is the second). Constructing a `Refusal` from raw
  English, or routing a diagnostic anywhere but `Internal`, is the bug the
  variant names exist to make visible — and rendering `Display` (which keeps
  tags + raw payloads for logs) on ANY user surface is forbidden: render paths
  go through `user_detail()` or their own surface's map. Pinned by
  `fauna_mls_backend_tests.rs::user_detail_maps_every_variant_to_user_renderable_text`
  (every arm, mutation-graded on the diagnostic-leak arm),
  `manager_integration_tests.rs::failed_send_stamps_send_state_failed_for_surfacing`
  (the slot carries no `"transport failure"` Display tag — mutation-graded
  against an `e.to_string()` regression),
  `manager_integration_tests.rs::a_refusal_carrying_user_facing_text_rides_message_with_no_variant_tag`
  (a refusal payload rides `{message}` as itself), and end-to-end by
  `test_mail_client_send.py::test_client_send_over_inline_ceiling_refused_with_localized_display_text`.
  **The 2026-08-02 sweep left both halves of this element leaking; closed
  2026-08-06.** (a) Seven `backends::fauna_mls` sites wrapped **MLS-engine**
  errors in `Transport` — `create_group`, `build_scheduling_delivery`,
  `encrypt`, `add_member_staged_from_bytes`, `merge_pending_commit` ×2,
  `remove_member_staged` — so a raw openmls diagnostic rendered verbatim on all
  7 apps; they route `Internal` now, and `SeamMessage` makes the shape
  unrepresentable rather than merely swept. (b) The **membership producers**
  (`confirm_add_participant`, `remove_participant`, `rename_thread`) stamped the
  page error with `e.to_string()` — `Display`, forbidden above — so every
  diagnostic reached `error-message` intact through the element's *other*
  producer; all three stamp `user_detail()` and log `{e:?}`, matching
  `send`. Pinned by
  `fauna_mls_backend_tests.rs::an_engine_failure_reaches_the_send_slot_as_the_generic_sentence`
  (a real `MlsEngine::encrypt` failure driven through the manager) and the
  page-error half of
  `fauna_mls_backend_tests.rs::manager_confirm_add_participant_rolls_back_the_snapshot_when_the_wire_op_fails`
  — both mutation-graded, each reverting to a raw payload on `error-message`.
  Corollary for readers of `page_error_diagnostic()`: a **diagnostic** now reads
  there as the generic sentence, and its detail is in the log line beside the
  stamp.
  **A third site of the same family — the cross-nest attachment mint — closed
  2026-09-09.** `SeamMessage` makes the *shape* unrepresentable outside
  `backend.rs`, but `ConvRpcError::transient(…)` → `From<ConvRpcError>` →
  `transport_from_seam` is a legitimate door through it, so a producer that
  classifies wrongly still reaches `error-message` with whatever string it built.
  `fauna_client_conversations`'s foreign-attachment upload did both halves at
  once: its write-token minter mapped **every** `NestClientError::Rpc` to a hard
  `403` (matching the *variant*, not `RpcError::action()`), and one frame up
  `blob_put` flattened that result — refusal included — back to `Transient`. So a
  permanent membership refusal (`fauna.federation.forbidden` from the home nest's
  federation gate) was reported as retryable, an unreachable home nest — which
  our own nest reports as `internal("federation mint failed")`, a wire `RpcError`
  and the canonical *retryable* fault — was reported as permanent, and the
  sentence the user read was `blob_put: HTTP 403: blob.write_token.get refused:
  forbidden (sealed_cid <64 hex>)`: hand-written English, a wire code and a
  content id. Both arms now classify through the shared `conv_rpc_error`
  (`action()` + `localized()`), so a refusal arrives as `Rejected` → `Refusal`
  with the shared `error.authorization` sentence and no retry, a transient mint
  stays `Transient`, and the cid rides the log. **The reusable lesson is that the
  classification and the render are one decision**: the byte plane needs an
  `ApiError` and the send slot needs a `ConvRpcError`, and deriving both from one
  `action()` call is what keeps them from disagreeing — matching on the transport
  error's *variant* collapsed two opposite faults into one bucket. **`transient`'s contract stays prose — decided 2026-09-09, do not re-open without
  new evidence.** Its precondition ("no wire `RpcError` to classify") is not
  type-enforced, and the closing of the mint bug asked whether it should be. The
  census says no. All **13** production call sites were enumerated and **none**
  holds a wire error: two are *inside* `conv_rpc_error` itself, reached only
  after `as_rpc_error()` returned `None`; the upload arms carry
  `fauna_nest_http::ApiError` / `fauna_rpc_wasm::BlobHttpError`, neither of which
  implements `RpcErrorClass`; the `blob_get` arms are reqwest/gloo HTTP faults; one
  is a `Weak::upgrade` failure with no error value at all; and the only two whose
  *type* could carry `Rpc(..)` are bare `connect` calls, where a wire refusal
  cannot arise. Three further reasons the guard is the wrong instrument: Rust has
  no negative trait bounds, so the proposed "cannot be built from a classifiable
  error" signature is **not expressible** — and `impl Into<String>` is reachable
  from any `Display` via `.to_string()` regardless; `ConvRpcError::Transient {
  message }` is a public variant constructed directly elsewhere, so the
  constructor was never the only door; and — the decisive one — the 2026-09-09
  mint defect it was proposed to prevent **would not have been caught by it**,
  because that caller held an already-flattened `ApiError`, not a classifiable
  error. The real lesson of that bug is upstream and is stated above: classify
  once, at the frame that still holds the wire error. A count ratchet over
  `transient(` was also weighed and declined: every present site is correct, so it
  could only force a re-review on new ones — and that bug survived exactly such a
  grep-plus-review.
- **A send the app cannot legitimately address is refused locally, not attempted
  (2026-07-31).** The SMTP rail's `From:` is the user's own `<handle>@<domain>`,
  and the nest verifies it ([`../behavior/mail-app-surface.md`](../behavior/mail-app-surface.md)
  § First-party client send). When an app cannot resolve that address — the account
  has no handle on its nest, which is a representable state
  ([`../architecture/nest/public-mode.md`](../architecture/nest/public-mode.md)
  § *A handle-less account*) — the shared `SmtpBackend::send` fails the compose
  with `error.email.no_handle` before the wire, so all 7 apps surface one honest
  sentence on `error-message` through the ordinary `SendState::Failed` carrier
  above. Two tempting substitutes are specifically forbidden, because both make
  the failure *look* like success: a session handle the nest does not back (an
  address the account does not own — the nest refuses it, and the user sees an
  RPC code), and an **empty** `From:`, which relays a malformed message *and*
  slips past the nest's gate rather than being caught by it. The
  nest-side refusal now resolves too: `fauna.email.permission_denied` maps to
  `error.email.permission_denied` in `RpcError::localized()`, where it
  previously fell through to `error::UNEXPECTED` while the nest's own human
  reason sat unread in `details`.
- Reply threading is `In-Reply-To` first, **subject second**: an inbound
  reply keys by its referenced parent (`ByMessageReference`), but when that
  parent isn't in any thread (unknown / mismatched `Message-ID`, e.g. the
  original was sent from a different MUA, or it arrives after the reply) the
  receiver falls back to the **normalized-subject** key — the same key a
  non-reply gets — so the reply still merges with its conversation instead of
  stranding the original in a separate thread (`fauna_conversations::manager`
  `ingest_inbound`; fixed 2026-06-08 from linux manual testing where a reply
  showed without its original). Only a reply with **both** an unresolvable
  `In-Reply-To` **and** a divergent subject still forks — the residual
  "Cross-side thread continuity" degraded case.

## Architectural rules

1. **Observer-driven rendering** against the
   `ConversationsManager` snapshot. No client-side state machine.
2. **No client-side MLS state.** All MLS group operations and message
   decryption run in shared Rust (`libs/fauna-mls` and
   `libs/fauna-conversations::backends::fauna_mls`).
3. **Localized strings** via i18n table; never hardcode English.
   `i18n/strings/en.yaml` is the source.
4. **Indexed list IDs** (`conversation-item[i]`, `dm-attachment-image[i]`,
   `dm-sender[i]`, `subject-divider[i]`, `thread-member-chip[i]`,
   `dm-message-text[i]`, `dm-reply-button[i]`, `recipient-picker-chip[i]`,
   `recipient-picker-suggestion[i]`, `protocol-icon[i]`) — scoped
   queries only, never global count + slice.
5. **Capability gating.** Apps render every affordance unconditionally
   and gray out / disable based on `capabilities.*`. They never branch
   on `rail` directly. This rule extends to the action layer (Python
   e2e) and the viewmodel — neither layer may branch on rail. Adding a
   sixth rail must be a backend change with zero app edits.

## Don't do these

- Don't decrypt messages on the client. Shared Rust decrypts; the
  snapshot delivers plaintext.
- Don't branch on `rail` anywhere — view, viewmodel, action layer.
  Always branch on `capabilities.*`.
- Don't reintroduce a `groups` page. Group threads live on this page
  with `flavor: MlsGroup`.
- Don't reintroduce a `transport-toggle-button` or modal new-thread
  compose dialog. Address resolution decides rail once at thread
  creation; new-thread compose is in the detail pane.
- Don't skip tests for apps lacking a rail today; the action layer
  falls through and the test fails informatively.
- Don't introduce a per-app `ConversationsViewModel` owning state.
  ViewModels are thin observers over the shared snapshot.

## Done definition

- [ ] All ui.yaml `conversations` IDs render with their canonical IDs on every app.
- [ ] Capability gating works on every app (no rail branches in app code).
- [ ] Subject-divider rendered between bubbles when subject changes (per-app).
- [ ] Add-participant on FaunaMls 1:1 forks a new group thread; on every other rail/flavor, adds in place (per-app).
- [ ] Rename only enabled on MLS groups (per-app capability gate).
- [ ] e2e tests GREEN on `--client X` for: `test_recipient_picker`,
      `test_keying`, `test_subject_divider`, `test_capability_gating`,
      `test_thread_membership`, `test_thread_rename` — for X ∈
      {`windows`, `web`, `linux`, `macos`, `ios`, `android`, `tui`} (pre-flip
      "six apps" phrasing corrected — tui is the 7th app since
      2026-07-19; `test_capability_gating` no longer skips tui — the
      `attachment-button` buildout it was gated on landed 2026-07-30).
- [ ] `libs/fauna-conversations` covers protocol logic; per-rail
      backends complete per
      the conversations-rails track (tracked internally).
- [ ] `tests/e2e-unified/ui.yaml` `conversations` block reflects spec section 8.
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml` refreshed for each app.
- [ ] Reactions (per app): quick-set toggle adds/removes a `dm-reaction-pill` with the right count;
      cross-member receive renders the pill; `supports_reactions` gate honored (no react on mail).
- [ ] Message delete (per app): own-message delete renders the "deleted" placeholder on sender +
      receiver; delete affordance absent on others' bubbles (the sender-only slice — the owner/admin
      delete is `../behavior/conversation-rooms.md` § Roles and authorization's, with its own done-state);
      `supports_message_delete` gate honored.

## Reading list

1. `principles.md` — product invariants + engineering priorities.
2. (design ratified 2026-05-10; tracked internally) — design spec (the why).
3. (implementation plan tracked internally) — implementation plan (Windows-led).
4. `tests/e2e-unified/ui.yaml` — `conversations:` page block + components.
5. [`../behavior/direct-messages.md`](../behavior/direct-messages.md) — protocol-side counterpart for Fauna native MLS DMs.
5a. [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) — the room model: membership, classes, roles, home nest, history policy.
6. `libs/fauna-conversations/` — shared crate (snapshot, manager, RailBackend, per-rail backends).
7. `libs/fauna-mls/` — current MLS crate (`FaunaMlsBackend` builds on it).
8. `apps/fauna-windows/FaunaApp/FaunaApp/Views/ConversationsPage.xaml` — Windows reference impl (lead app for this slice).
9. `tests/e2e-unified/ui-actual-<app>.yaml`.

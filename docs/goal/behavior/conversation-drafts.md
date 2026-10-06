# Conversation drafts — target state

Owns: conversation-drafts
Status: ratified — split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) on 2026-09-28; drafts v1 (the single-slot new-thread draft) and v2 (persistent across restarts, synced across devices) are built on all 7 apps, and each restore rule below carries its own ruling date.
Authority: what a conversation compose draft keeps and how it survives — the per-thread `ComposeState` and the single-slot `new_thread_compose` draft (v1), their persistence through `DraftStore` and `DraftsSync` (v2), the rule that only user-authored input rests, and the restore rules (a restore fills and never clears, a filled picker is probed, a restore the outgoing account started fills nothing, a restored attachment is a handle that refuses its send). Defers the `__drafts` reserved folder and its wire kinds to [`reserved-folders.md`](reserved-folders.md) § Drafts Sync; the posts rail's drafts to [`../ui/feed.md`](../ui/feed.md) § Persistence; the compose surface, its element IDs and every other page rule to [`../ui/conversations.md`](../ui/conversations.md); the attachment store a restored draft's bytes live in to [`conversation-attachments.md`](conversation-attachments.md).

> **Audience:** shared-Rust work on `fauna_conversations::store::drafts` and `fauna_client_drafts`, and each app's restore-and-autosave trigger.
> **Purpose:** what a half-written conversation message keeps, across a thread switch, a relaunch and another device.

*Split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) § Persistence on 2026-09-28, when that page doc was 56 bytes under the whole-file read ceiling; the page's Authority line already named drafts as a concept of its own, and the restore rules were a steady share of its growth. The bullets keep their original parent heading, so a `§ Persistence` citation resolves here by changing only the file. A routing stub remains at the original location; prior history: `git log --follow docs/goal/ui/conversations.md`.*

*Reading this doc. Its text was carried verbatim, so an unqualified `§ <name>` citation may name a section that is not a heading here. `§ Persistence` means this doc's section for the drafts bullets and the page's own for its other bullets. `§ Attachments` (and `→ *Retention*`) resolves in [`conversation-attachments.md`](conversation-attachments.md). Every other unqualified name — § Implementation status today, § User actions, § Errors & edge cases → *The picker tells the truth* — resolves in [`../ui/conversations.md`](../ui/conversations.md).*

## Section map

- **§ Persistence** — the drafts bullets, in their original order: *Per-thread `ComposeState`* (v2 and its implementation status), *Only user-authored input rests*, *A restore FILLS*, *A restore that FILLS a recipient picker owes it a probe*, *A restore the outgoing account started fills nothing after an identity change*, *A restored draft's attachment is a handle, not a file*, and `new_thread_compose` (v1).

## Persistence

- **Per-thread `ComposeState`** is persisted by `DraftStore` in shared
  Rust. **v2 (persistent across app restarts, synced across the user's
  devices) — shared-Rust + nest core is implemented;** the 7 app legs
  wire the trigger (see *Implementation status today* below).
  `DraftStore::snapshot_bytes` / `restore_from_bytes`
  (`libs/fauna-conversations`) serialise the whole draft set to canonical
  bytes; the client seals them under the owner's `BackupKey` and
  round-trips them through the `__drafts` reserved folder via the
  `fauna.drafts.{put,get}` WS-RPC kinds, per
  [`../behavior/reserved-folders.md`](../behavior/reserved-folders.md) § Drafts Sync
  — sealed under the owner's `BackupKey` (the owner-only key defined in
  [`../architecture/owner-key-material.md`](../architecture/owner-key-material.md)
  § Audience: owner only Path A), regardless of nest mode. Mail drafts
  saved by a third-party IMAP MUA into the server `Drafts` folder are
  mail messages and follow the `Mail body` row in
  [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md)
  § Per-content-kind conformance, not this path.
- **Only user-authored input rests; transient UI state does not**
  (ratified 2026-08-16). This section owns the rule for this rail;
  [`../ui/feed.md`](../ui/feed.md) § Persistence owns it for the posts rail, where it
  was ratified first. What rests is what the user wrote: `body_draft`,
  `subject_draft`, `attachments` (by content address), `reply_to`,
  `reply_recipients`, and — for the new-thread slot — the recipient
  picker's `raw_input` and committed `chips`. What does **not** rest is
  one app run's own state: `send_state`, and the picker's `suggestions`
  / `resolve_state` / `resolved` probe output.

  Restoring `send_state` was a **user-stuck defect**, not a cosmetic one:
  every shell builds `dm-send-button` as `enabled = !sending`, and
  `send_state` is stamped only by `ConversationsManager::send`, so a
  restored `Sending` disabled the send button with no gesture that
  cleared it. The window was not a narrow crash race — `send` stamps
  `Sending` *through the draft store* before awaiting the network, which
  changes the snapshot bytes and re-triggers the app's debounced
  autosave mid-send; and a failed send stamps `Failed` **without**
  clearing the draft, so it rested indefinitely.

  **The bytes are frozen, so the enforcement is a constructor, not a new
  record.** Unlike the posts rail — which enumerates its own
  `fauna_feed::drafts::PostDrafts` because it opened with no shipped
  blobs — this rail's blob is shipped, and `ComposeState` carries no
  `#[serde(default)]`: a blob missing a key fails to decode on an app
  that has not been updated, and the shared launch gate stays closed
  after a failed restore, which would show that app *no drafts at all*.
  So evolution stays additive-everywhere
  ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)):
  every field is still written, the transient ones at their idle
  default, normalised by one chokepoint in
  `libs/fauna-conversations/src/store/drafts.rs`. That chokepoint is an
  explicit struct literal with no `..Default::default()`, so a field
  added to `ComposeState` later fails to compile until its author
  classifies it — the same "cannot silently become at-rest data"
  property the posts rail gets from enumerating a record. It runs on
  write; the read-side heal of blobs resting with `Sending`/`Failed`
  from before the chokepoint was retired 2026-09-24 by the compat-remnant
  sweep ([`../architecture/compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md)
  § Program 4).

  **Implementation status (drafts v2).** Landed end-to-end (2026-06-21):
  the shared-Rust serialize/restore (`DraftStore::snapshot_bytes` /
  `restore_from_bytes`, byte-stable + `BackupKey` seal round-trip,
  tier_1-tested), the manager pass-throughs (`drafts_snapshot_bytes` /
  `restore_drafts`), the `fauna.drafts.{get,put}` protocol + nest plane
  (`__drafts` reserved set, modelled on the `__config` rail
  retired 2026-10-02), and the shared client
  wrapper `fauna_client_drafts::DraftsSync` (launch gate + last-saved-baseline
  dedup — the canonical client shape, so each leg is pure trigger glue).
  **All 7 app legs ride `DraftsSync`** (restore-on-launch + a debounced
  autosave; § Implementation status today), tui last on 2026-08-04; proven by
  the tier_3 restart round-trip `test_conversations_draft_persistence.py`
  (windows leg green incl. `+real_conversations`; linux + tui markers added with
  the tui leg — linux's wiring had been live since 2026-06-21 and was only ever
  a *test* gap, tui's was a real one). Because the shared `DraftStore`'s
  `DraftsSnapshot` already carries the single-slot `new_thread` compose, no app
  needed an interim in-memory draft store retired to get there. Two more
  load-bearing gotchas, both GUI-app-specific: the windows
  `MarkdownRichEditBox` render-echo (unchanged-text `TextChanged` each render
  tick perpetually reset the save debounce) is fixed by a value-idempotent
  `BodyChanged` (see `reserved-folders.md` § Drafts Sync); the apple in-process e2e
  path attaches the handle via `ConversationsVM.attachDraftsSync` because the
  e2e path keeps mock backends and never runs `activate` (SAVE-side gap,
  closed 2026-06-22).
- **A restore FILLS: it never clears a slot, and never overwrites a draft the
  user is composing into (ruled 2026-09-21).** The `__drafts` fetch is fired at
  login and awaited off the UI thread, so it lands whenever the network says —
  routinely *after* the user has opened `+` and started typing. Replacing the
  in-memory set wholesale therefore made the restore a third way to lose a
  draft, which the `new_thread_compose` bullet below does not allow: a blob
  resting with no new-thread slot cleared the live one. The loss was silent in
  a particular way, and that is why it is worth stating here rather than
  leaving to the implementation — the composer's *visibility* is a separate
  flag (`new_thread_active`) the restore never touches, while every mutator
  behind it (`set_new_thread_recipient_input`, `accept_new_thread_chip`) is a
  no-op once the slot is gone. So the user goes on typing into a picker that
  can no longer record a keystroke or start a resolve, and `recipient-resolve-status`
  sits at `idle` with no probe ever issued. Three consequences: an absent slot
  in the blob is not an instruction to close a live composer; a thread the blob
  omits keeps its local draft; and on a collision the live compose wins, the
  blob being at best as old as the last autosave. **Nothing is given up** — a
  slot the user has not authored into is still adopted, which is what carries a
  draft written on another device onto this one, an open-but-untouched composer
  included. The posts rail arrives at the same place from the other side, by
  declining to apply an all-empty record at all (`PostDrafts::is_empty`).
  Pinned by `libs/fauna-conversations/src/store/drafts.rs`:
  `a_restore_never_clears_a_live_new_thread_draft`,
  `a_restore_never_overwrites_a_live_per_thread_draft`, and
  `a_restore_still_fills_an_open_but_untouched_composer` for the other half.
- **A restore that FILLS a recipient picker owes it a probe (ruled 2026-09-21).**
  What rests of the picker is the user-authored half — `raw_input` and the
  committed `chips` — while `resolve_state` rests `Idle`, because a probe cannot
  survive a relaunch and resting `Resolving` would paint a spinner nothing ever
  resolves (`store::drafts::persistable_picker` owns that reasoning). A restored
  picker therefore arrives holding an address at `idle`, and that was a **dead
  state**: § Errors & edge cases → *The picker tells the truth* rule 1 says only
  the async probe leaves `idle` for a non-empty input, nothing re-probed a
  restored one, and no chip can be committed from an unprobed address
  ([`../architecture/foreign-handle-resolution.md`](../architecture/foreign-handle-resolution.md) § Peer-auth
  model → *Discovery-failure semantics*). The user could not even re-arm it by
  retyping the same address, because every app shell suppresses the echo of an
  unchanged field — correctly, since an echo would un-resolve a confirmed probe.
  So **the restore issues the probe itself**, with no user gesture: a non-empty
  `raw_input` resting at `Idle` on the restored new-thread slot is resolved
  before the restore returns. Both halves of that guard are load-bearing — an
  untouched picker is the one legitimate `idle` (rule 1) and is left alone, and a
  picker already past `idle` belongs to a live compose the restore declined to
  overwrite, whose own probe has already run or is in flight. The probe is aimed
  at the new-thread picker *specifically*, never "whichever picker is active":
  the blob carries only that slot, so an add-participant overlay the user happens
  to have open when the late fetch lands is not a target. Landing this made
  `ConversationsManager::restore_drafts` **async** — one shared seam rather than
  the same rule written into seven shells (priorities #1/#2) — and every app leg
  gets it through the call it already makes. Pinned by
  `libs/fauna-conversations/tests/manager_integration_tests.rs`:
  `a_restored_recipient_is_probed_with_no_user_gesture`,
  `a_restore_with_no_typed_recipient_probes_nothing`, and
  `a_restore_does_not_probe_an_open_add_participant_overlay`.
- **A restore the outgoing account started fills nothing after an identity
  change (2026-09-23).** Because a restore fills, and fills the probe's input,
  a launch fetch the outgoing account started that answers after the switch
  would shadow the incoming account's own draft, be re-sealed into its plane by
  its next autosave, and ask its rails to resolve the outgoing account's
  recipient. The rule is [account-scoping.md](../architecture/apps/account-scoping.md)
  § The scoping taxonomy's (the writers of account-scoped state retire with the
  drop); the mechanism is the manager's identity epoch:
  `clear_for_identity_change` advances it, each app reads it before the fetch,
  and `ConversationsManager::restore_drafts_at` refuses a moved epoch **before
  the fill**, so no probe runs. Pinned by
  `a_restore_the_outgoing_account_started_writes_nothing_after_the_identity_change`
  (zero `resolve_address` calls) and its positive control, plus the shell
  pins for each app's own epoch read (which the library test cannot see): tui
  and linux `a_load_the_outgoing_account_started_fills_nothing_after_the_switch`,
  windows `RestoreOnLaunch_ReadsTheIdentityEpochBeforeTheLoad_NotAfter`, apple
  `ConversationsDraftsRestoreEpochTests`.
  Adopted by tui, linux, android, web, apple and windows — every app leg now calls `restore_drafts_at`.
- **A restored draft's attachment is a handle, not a file — its send refuses
  rather than going without it (ruled 2026-09-14, on the posts rail's
  precedent).** A draft's `attachments` rest by content address (above), but
  their bytes live only in the in-memory attachment store of the device that
  staged them (§ Attachments → *Retention*, where a disk-backed store was
  weighed and rejected). So a draft restored after a relaunch, or synced to the
  user's other device, can list a file no store on that device holds — and
  unlike a received attachment it has no coordinates to fetch it from, because
  a staged file is uploaded only by the send itself. On that device the compose
  still shows the attachment chip, and **`send` refuses**: every staged
  attachment resolves to bytes or the whole send fails closed, before anything
  reaches the backend, with a `BackendError::Refusal` naming the file
  (`error.send.attachment_missing`, on `error-message` through `send_state` like
  every send failure). The draft is kept, so the user removes the chip, attaches
  the file again and sends. It never sends without it — the harm the store's pin
  exists to prevent (§ Attachments → *Retention*), and the posture of Send never
  silently dropping a pending recipient (§ User actions). The sender's own echo
  is built from the set that was resolved and handed to the backend, not from
  the compose, so the Sent bubble cannot list a file the recipients did not get.
  Weighed and rejected for now: marking a restored draft's attachments "attach
  again" in the compose bar at restore time — a new snapshot field and seven app
  renders, which would still need this send-time refusal as its backstop, since
  whether the device holds the bytes can change after the restore (the user
  picks the file again, a receive caches the same bytes). Refusal is also the
  posts rail's existing answer for a staged attachment that can no longer go as
  prepared (`feed.compose_attachment_stale`, [`../ui/feed.md`](../ui/feed.md)) — and, ruled
  the same day, that rail's answer to this exact case: a restored posts draft's
  attachment refuses its submit by name too (`feed.compose_attachment_missing`,
  [`../ui/feed.md`](../ui/feed.md) § Persistence owns it). Pinned by
  `manager_integration_tests.rs::a_restored_drafts_attachment_either_reaches_the_backend_or_refuses_the_send`.
- `new_thread_compose` is a single-slot `Option<ComposeState>` draft that
  **persists across switching** — clicking an existing conversation (or
  toggling back to `+`) keeps the half-written new message intact, so in
  effect there is one preserved draft per existing conversation (the
  per-thread `ComposeState` above) **plus** one for the new conversation, and
  switching between any of them never alters another's draft. An internal
  active-view flag (`ConversationsManager.new_thread_active`) governs whether
  the composer is the *shown* detail pane — distinct from whether the draft
  *exists* — so the detail pane shows the most-recently-chosen of {a selected
  thread, the new-thread composer} while every draft is kept. Only an explicit
  **cancel/discard** (`cancel_new_conversation`, surfaced as the composer's
  Cancel affordance) or **calling send** (`send_new_thread`) clears the
  new-thread draft — `send_new_thread` materializes the thread and moves the
  draft onto its own per-thread `ComposeState` **before** attempting the send
  (`manager.rs::send_new_thread`), so the slot clears and the thread is
  selected regardless of whether the send itself succeeds or fails; a failed
  send's retry state lives on the materialized thread's compose, not back in
  `new_thread_compose`. No separate drafts folder in v1 (in-memory; the
  across-restart `__drafts` persistence is v2, above).
  - *Implementation status:* the shared-Rust core (the `new_thread_active`
    active-view flag, `select_thread` preserving the draft,
    `start_new_conversation` restoring it, `deactivate_new_conversation` — the
    nav-back-without-clearing path) and **all seven app legs are landed**
    (2026-06-21; § Implementation status today): every app ships the
    explicit-discard `new-conversation-cancel` (windows owns the canonical
    element), and the mobile/macOS reconciliation holds — a plain back/swipe
    PRESERVES the draft (android `BackHandler` and iOS `.onDisappear` call
    `deactivate_new_conversation`; macOS has no plain-back path — thread
    switching already preserves via `select_thread`), only the explicit Cancel
    discards. e2e: `test_conversations_new_thread_cancel.py` (android marker
    host-emulator-gated; macos/ios confirm delegated to the harness (tracked internally)).

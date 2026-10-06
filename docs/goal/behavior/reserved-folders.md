# Reserved folders — target state

Owns: reserved-folders
Status: ratified — per-rail statuses live in each rail's own status note below
Authority: the reserved `__*` folder convention — implicit per-actor creation + list exclusion, the management-surface namespace refusal + the rail / custody-copy discriminator and its collision rule, the shared `high_cadence` flag, and each rail's at-rest persistence channel (`__drafts`, `__mls`, `__mail` + siblings, `__mail-placement`, `__calendar-placement`, `__card-placement`; the retired `__config` rail keeps a history stub, its owner [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)); defers each rail's *mechanism* to its named owner (`mail-credentials.md`, [`devices.md`](devices.md), `../architecture/message-segment-store.md`, per-page compose docs, …) and the chunk pipeline + `sync_changes` catch-up to [`file-sync.md`](file-sync.md)

Split verbatim out of `file-sync.md` § Reserved folders on 2026-08-02 (a stub there redirects;
prior history: `git log --follow docs/goal/behavior/file-sync.md`).


Reserved folders are implicit per-actor collections used by nest features — created at first use, every device implicitly a member, not listed by `fauna.folders.list`. This section groups all reserved folders and documents their shared flag infrastructure.

**The management surface refuses the namespace — whole (ruled 2026-09-28, the folders mode contraction design pass; the built 2026-07-30 shape is recorded after it).** The `__` namespace belongs to the nest, so every user-facing folder write kind treats a reserved *resolved* name as not the caller's to mutate, via the shared `fauna_core::sync::is_reserved_folder_name` predicate (never a literal): `create` refuses it **unconditionally** (as `fauna.admin.folders.create` always did — the former `mode="backup"` carve-out closes, because no production client provisions a custody copy any more; the two nest-side provisioners named in the next paragraph do); `update` refuses it **whole** (the former per-field refusals — a mode change, a WebDAV serve-on — generalize: no field of a reserved set is a client's to set, and a reserved rail is never served over WebDAV, `webdav-server.md` § What the namespace is); `delete` refuses any reserved set that is **not a custody copy**, because a live rail's `sync_changes` rows are the only reach to its sealed irrecoverable material (`../architecture/nest/common.md` § Client-state recoverability), while deleting a custody copy stays allowed — it *is* the destination-removal handshake (`backup-destinations.md` § State & data shape → Destination-removal / supersede handshake); the DB delete helper backstops the same rule for future callers. **Built 2026-09-28** — the 2026-07-30 shape it replaced (the discriminator `folders.mode`; a `create` that admitted a reserved name with `mode="backup"`; per-field `update` refusals; a `delete` keyed on `mode != "backup"`) is retired, and `create_refuses_every_reserved_name` / `update_refuses_a_reserved_name_whole` / `delete_still_allows_a_custody_copy` (`bins/fauna-nest/tests/conformance_folders.rs`) pin the whole-namespace rule.

**A rail and a custody copy are told apart by `folders.custody_copy`, a nest-internal flag no client can set (ruled 2026-09-28; built 2026-09-28, schema 94 — a one-shot step, since folded into the genesis, carried each `mode = 'backup'` reserved row onto the flag before the column dropped).** Two kinds of row share the reserved namespace on one nest: a **rail** — the actor's own live `__mail`, `__drafts`, … on this nest, minted lazily by the nest at first feature use (`get_or_create_reserved_folder`, the only writer of a rail) — and a **custody copy** — the blind sealed mirror of *another* location's rail or folder, provisioned by the nest itself on the first custody write (the federation handler's resolve-or-create for a nest-driven backup, `federation_handlers::resolve_backup_custody_set`; the sync door's twin for a client-device custodian re-seeding a nest, `sync_handlers::writable_or_provisioned_backup_set`). The target shape: `folders.custody_copy INTEGER NOT NULL DEFAULT 0` with `CHECK (custody_copy = 0 OR substr(name, 1, 2) = '__')`, so a custody copy on a non-reserved name is unrepresentable; written `1` only by those two provisioners; **on no wire kind** — not on create, update or any projection: the discriminator derives from *who wrote the row*, the one thing a client cannot set; and read through ONE seam, `fauna_nest::db::snapshots::is_reserved_custody_copy(row)` (the successor of `is_reserved_backup_set(mode, name)`, keeping the name conjunct as defence in depth), which every custody-vs-head routing or gating decision calls — custody routing on both record rails, `fauna.sync.changes.mark`, the device-pull exclusion, the GC manifest-class walk, the `delete` carve-out above, `is_pure_backup_destination` (the four destination gates, [`../architecture/message-segment-store.md`](../architecture/message-segment-store.md) § Destination capability), the snapshot refusals and scheduled compaction. **The heal collapses to a refusal:** the rail mint's `INSERT OR IGNORE` may adopt an existing row, and the only row it can meet under a rail's name is a custody copy — it **refuses** one, never re-classes it (the 2026-07-30 re-class arm, backup→sync on a row holding no live custody state, existed only because a client could pre-create the name through the carve-out, and closes with it). One rule across every collision site — the rail mint meeting a custody copy, either provisioner meeting a rail — is unchanged: **the role holding live state wins and the newcomer is refused** (re-classing would drop a functioning custody copy out of the GC's manifest-class walk and reclaim a live offsite backup); the collision is unreachable through the enroll flow, which never points an owner's backup destination at their own home nest, and the recovery if it is ever reached is the ordinary owner `fauna.folders.delete` of the custody copy, then retry. *History:* the 2026-07-30 heal made the discriminator nest-written for every row that is a rail — correcting a client-chosen `mode` on adoption — because five nest behaviors keyed on it (the `delete` carve-out, `fauna.sync.changes.mark`, scheduled compaction, post/conv snapshot create); the flag keeps that property by construction.

## High-cadence flush

Every reserved folder has an optional `high_cadence` flag in its metadata. When set, the chunk-forward coordinator (per the mail-segment backup protocol, design ratified 2026-05-15, tracked internally) flushes the folder's chunks to fauna-sync destinations on a fast cadence — every 5 seconds of activity or every 100 events appended, whichever fires first — instead of waiting for the segment-lifecycle wake-ups (`Finalized` / `Compacted*` / `Tombstoned`) that standard reserved folders ride.

Currently set on: `__mail-placement`, `__calendar-placement`, `__card-placement`.

**When to set the flag.** A reserved folder is a candidate for the `high_cadence` flag when both: (a) its records are small (kilobyte-scale, not megabyte-scale) so frequent chunk forwards don't flood the network, and (b) the cost of losing recent writes in a disaster-recovery scenario is materially higher than the cost of the extra network traffic. Mail / calendar / card placement journals fit both criteria; content segments (`__mail`, `__calendar`, `__card`, …) do not — content records are larger, less frequent, and snapshot-cadence-grained loss is acceptable. (`__card-placement` carries the flag as `__calendar-placement`'s structural twin: the two DAV bridge stores must not diverge in DR posture.)

**Snapshot-row pinning still runs at standard cadence.** The high-cadence flag affects chunk forward, not snapshot-row creation. A point-in-time snapshot row (`snapshots` table) pins both the content manifest and the paired placement manifest at the scheduler's standard cadence (per `backup-restore.md` § Background Tasks); the placement journal is up-to-date at the backup destination ahead of when the snapshot row pins it.

## UserConfig Sync

**History — the `__config` rail is retired (2026-10-02).** Until then the account's settings rested here as one whole-record `UserConfig` blob (`user.faunaconfig`), sealed under the owner's `BackupKey` in the per-actor, per-nest `__config` reserved folder and loaded and stored whole over `fauna.config.{get,put}`; every field has since left for its own account-state plane kind, and the rail, its wire kinds, its nest store and the whole-record struct are gone ([`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (6), which owns this history; each former field's kind, audience rung and merge rule → the same section's *The kinds*; the plane's at-rest seal → [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Per-content-kind conformance, the row "User settings — the account-state plane kinds (formerly `UserConfig`)"). The `__config` name stays reserved, and no reader opens the rows a nest may still hold under it. This section is no longer the at-rest authority for any live data; its former text: `git log -p -- docs/goal/behavior/reserved-folders.md`.

## Drafts Sync

Fauna-native compose drafts — the in-progress posts (Feed page, `compose-text-field` + tags + attachments), conversation messages (Conversations page, per-thread `ComposeState`: body, subject, attachments, `reply_to`), and calendar events (Events page, `EventComposeState`) the user has authored but not yet sent / published / discarded — are persisted as a single reserved folder named `__drafts`. Authority for the per-page compose-state shape stays with the respective per-page goal docs (`docs/goal/ui/feed.md`, `docs/goal/ui/conversations.md`, `docs/goal/ui/events.md`); this section is the at-rest authority for how the resulting bytes are sealed and synced.

The sync path is identical to ordinary folder content:

1. A shared-Rust per-rail store serialises that rail's draft set to its on-disk form as **one canonical blob, byte-stable so an unchanged set re-uploads identically** — the property the client-side dedup rests on. Each rail owns its own record shape (per the § intro, the per-page goal doc is the authority for what a compose state contains), and there are two implementations today: the **conversations** rail is `libs/fauna-conversations::DraftStore::snapshot_bytes` / `restore_from_bytes` (body / subject / attachments-by-blob-hash / `reply_to` / reply-recipients, per-thread plus a new-thread slot — transient send state deliberately **not** among them since 2026-08-16, per [`conversation-drafts.md`](conversation-drafts.md) § Persistence, which owns this rail's field-level shape), and the **posts** rail is `libs/fauna-feed::drafts::PostDrafts` (one slot, user-authored compose input only — shape ratified in [`../ui/feed.md`](../ui/feed.md) § Persistence). Internal layout under `__drafts` is addressed by a rail `path` carried on the `fauna.drafts.{get,put}` wire, and that key is a **closed set of exactly three rail constants** — `"conversations"`, `"posts"`, `"events"` — owned by `fauna_protocol::drafts::DRAFT_RAILS` and **enforced nest-side on both kinds**. Each rail stores one combined blob at `path = "<rail>"`.

**The enumeration is closed on purpose, and it is half of how this rail is bounded** (ruled 2026-08-05; refutable-advisory — a later session may reopen it by supplying the delete verb and metering below). A reserved rail is bounded by *construction* — a fixed path count times a per-blob cap — rather than by the storage quota: `__drafts` is three rails times `MAX_DRAFTS_BLOB_BYTES` (1 MiB), so an actor's **live** `__drafts` footprint is bounded at 3 MiB. It writes through the *unmetered* recording helper, and that is correct for this class rather than an oversight: it does not charge `users.storage_bytes_used`, because it cannot grow without bound. ⚠ **`__index` is the rail whose path count is deliberately NOT fixed** — its segment paths grow with the account, capped per blob (the byte route's 10 MiB body limit) but not in number — so the "fixed path count" half of this bound is *stated and open* there rather than closed; what closes the version axis for it, why the count axis cannot be closed nest-side, and what would close it are [content-index.md](content-index.md) § Where the index is built's. ⚠ **The live bound is not the resting bound, and the caps alone never gave one (corrected 2026-08-05).** Every `put` registers a fresh blob and appends a row, and each such row *is* a version — so the resting footprint is the live 3 MiB **plus whatever the rail's superseded history still pins**. That history is bounded only because reserved rails collapse it at write time; the rule, the three properties a rail must satisfy to be allowed to, and why this is the axis metering could never have bounded are owned by [`file-versions.md`](file-versions.md) § Retention. State the ratio when quoting a number here: the resting cost is (writes per GC cycle) × 1 MiB per rail above the live 3 MiB, not a constant. A **client-chosen** key would break exactly that property — every distinct value is its own latest-per-path live row, which GC pins forever — which is why the earlier freedom to store "a blob per `__drafts/<rail>/<id>`" at `DraftStore`'s sole discretion is **withdrawn**: a per-id layout is still reachable, but it is now a wire change that must ratify its key grammar here and be admitted nest-side (additive and nest-gated — a client sending a rail an older nest does not know is refused, so a new rail ships once nests admit it), and an unbounded key space additionally needs metering, which in turn needs a delete verb this plane does not yet have (`fauna.drafts.{get,put}` only). The closed enumeration is also what lets the `path` column rest as plaintext honestly: three frozen constants identical on every deployment and every account are the machine-authored class [`path-sealing.md`](path-sealing.md) § Deliberate non-seals exempts, which a free-form client-chosen string was not.
2. The serialised bytes are chunked, hashed, and uploaded via `encode_blob()` (per `file-sync.md` § Content-Addressed Storage): zstd-compressed, then sealed under the owner's `BackupKey` (seal shape + store keying owned by `docs/goal/architecture/owner-key-material.md` § Path A — chunks riding the chunk route use the convergent `convergent_chunk_root()` seal keyed by the ciphertext hash; whole-blob writes keep the random-nonce `[version=0x01][nonce(12)][ciphertext+tag]` frame), then stored in the blob store by its store key.
3. A `sync_changes` row is written for the new head — **at the DB layer, by the rail's own handler**, not over the device-sync wire (see § How a rail actually reaches the user's other devices, below).

### How a rail actually reaches the user's other devices

**A rail does not ride the file-sync catch-up, and this is structural rather than incidental** (corrected 2026-08-10 — the previous text here said rail changes are "picked up using the same catch-up mechanism as regular files", which describes neither the write path nor the read path). Three facts, each enforced in code:

- **The row is written beneath the wire, not through it.** `drafts_put_handler` calls `CacheDb::record_drafts_blob_change`, which calls `record_sync_change` directly and then `collapse_reserved_rail_history` (`bins/fauna-nest/src/db/drafts.rs`); `__mls` and `__index` do the same. The device-sync kind `fauna.sync.changes.record` **refuses** a reserved set outright (`bins/fauna-nest/src/sync_handlers.rs`, `is_reserved_folder_name` guard) — so the wire path a file-sync client uses is closed to rails by design.
- **Nothing enumerates the rail to sync it.** `fauna.folders.list` filters reserved sets out (`bins/fauna-nest/src/folder_handlers.rs::list_core`), so a syncing client never sees `__drafts` and never asks for its changes.
- **No push fires.** No rail handler emits a *push event*; a rail write is silent on the push channel. (The one exception, the `__config` rail's `fauna.sync.changed` nudge of 2026-08-11, retired with that rail on 2026-10-02 — [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (6).)

So what is the `sync_changes` row *for*? **GC reachability and versioning** — it is what pins the head blob against collection (and what the write-time collapse in § Drafts Sync step 3 above prunes down to one). It is not a device fan-out mechanism.

**Propagation is therefore load-on-launch, not push-on-write:** a second device picks up rail changes the next time it loads the rail directly (`fauna.drafts.get`) — which is exactly what the shared `fauna_client_drafts::DraftsSync` launch gate does. The practical consequence is worth stating plainly for anyone specifying against this rail: **a rail edit on device A is not visible on an already-running device B until B re-loads**. Full live propagation is the account data plane's sync plane (`../architecture/account-sync-plane.md` § The sync plane), which every former `__config` setting now rides as its own plane kind ([`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (6)).

`__drafts` is a reserved folder like every rail: it is created at first use rather than via the user-facing folder creation UI, every one of the user's devices is implicitly a member (no per-device opt-in), and the user-facing folder management surface (`fauna.folders.list`) does not list it.

**Conformance.** This section is the at-rest authority for fauna-native compose drafts; the `encryption-at-rest.md` § Per-content-kind conformance row "Drafts" points here. `__drafts` blobs are encrypted under the owner's `BackupKey` (the owner-only symmetric key defined in `docs/goal/architecture/owner-key-material.md` § Audience: owner only Path A) on the data owner's client before upload. No nest holds `BackupKey` (`nest/storage-modes.md` — the storage-mode axis is retired); the nest's single `SealedStorage` implementation treats `__drafts` blobs as opaque, and no nest path reads drafts today. Drafts have no plaintext-floor routing mirror (unlike inbox mode's `inbox_modes` table) because the nest never makes a routing decision based on draft contents — SEND transitions a draft into a `Post`, conversation message, or calendar event, each of which is routed via its own already-specified path with its own seal.

**Mail drafts saved by a third-party MUA into the IMAP `Drafts` folder are not stored here.** They are mail messages on the wire and at rest — APPEND-ed via IMAP to the user's mailbox `Drafts` folder, stored in the mail-storage table with the standard mail body sealed at the MTA-bridge perimeter under the recipient's MLS read key (recipient and author are the same actor for a self-addressed draft) — and they follow the existing `Mail body` conformance row, not the `Drafts` row. The fauna-native composer's `DraftStore` and the IMAP server's `Drafts` folder are two separate concepts with two separate at-rest stories; cross-surface unification (presenting an IMAP-MUA's draft inside the fauna-native composer, or vice versa) is a future sync-layer feature, not a single-store merge.

**Implementation status today (2026-07-10; rail bound added 2026-08-05).** Core landed: `DraftStore::snapshot_bytes` / `restore_from_bytes` (`libs/fauna-conversations`, byte-stable, tier_1 seal round-trip), the `fauna.drafts.{get,put}` kinds (`libs/fauna-protocol/src/drafts.rs`), and the nest `__drafts` raw-opaque store per `(actor_id, path)` + `sync_changes` row + reserved-set list exclusion (`bins/fauna-nest/src/{drafts_handlers,db/drafts}.rs`); cross-device propagation tier_3-proven (`tests/e2e-unified/tests/api/test_drafts_sync.py`). The bound described above is **live**: `fauna_protocol::drafts::{DRAFT_RAILS, MAX_DRAFTS_BLOB_BYTES}` are enforced by `drafts_handlers::validate_path` (both kinds) and the put handler's cap, pinned by `bins/fauna-nest/src/drafts_handlers.rs` tests + `test_drafts_sync.py`. **The write-time history collapse is live too (2026-08-05; `__index` joined 2026-09-20):** `CacheDb::collapse_reserved_rail_history` runs on every `put` for the raw-opaque rails — `__drafts`, `__mls`, `__index` (that rail's own bound, and the segment-path axis it leaves open, are [content-index.md](content-index.md) § Where the index is built's), as for `__config` until it retired on 2026-10-02 — so the resting term above is one GC cycle rather than permanent; pinned per rail by `a_rewritten_rail_pins_only_its_head_against_gc` in `drafts_handlers.rs` / `mls_replica_handlers.rs`, plus `the_collapse_is_per_rail_not_per_set`. **The `"posts"` rail opened 2026-08-15** — its at-rest record is `fauna_feed::drafts::PostDrafts` (shape ratified in [`../ui/feed.md`](../ui/feed.md) § Persistence), reached through `FeedManager::{drafts_snapshot_bytes,restore_drafts}` and the same two faces the conversations rail uses (`FfiFeedManager` native, `WasmFeedManager.{restoreDrafts,saveDrafts}` web); **all seven apps have landed their leg** (tui, linux, android, web, macos, ios — apple's macOS+iOS leg is one `FeedVM` change, 2026-08-25 — and windows last, 2026-08-26, `FeedDraftsService`). The nest needed no change for it, which is exactly what the closed-enumeration design promised. **The `"events"` rail's at-rest shape is ratified as of 2026-08-17** — `fauna_client_caldav::drafts::EventDrafts` (the five user-authored `event-form` inputs; shape + the calendar-is-not-at-rest and raw-datetime rules ratified in [`../ui/events.md`](../ui/events.md) § Persistence), living in the client-side calendar crate all seven apps already reach (tui + linux natively, the other five via `fauna-ffi` / `fauna-wasm`). The nest again needed no change, closing the enumeration's third and last constant. **All seven app legs now landed** — tui first (2026-08-17, `apps/fauna-tui/src/events/drafts.rs`, the first app to write this rail at all), then linux and web together (2026-08-31, `apps/fauna-linux/src/views/events/drafts.rs`; `apps/fauna-web/src/lib/event-drafts.ts` over the new `fauna_wasm::event_drafts::WasmEventDrafts` face), then android (2026-09-01, `EventDraftsHost.kt`, which also built the `fauna-ffi::event_drafts` typed native face — `FfiEventDraftsSync`/`FfiEventDrafts` — the windows/apple arms then consume), then macOS+iOS together (2026-09-02, one shared FaunaKit `EventsVM` leg over that same face), then windows last (2026-09-07, `EventDraftsService` over the same typed face — no manager to hang a snapshot off, so `EventsPage`'s own five `TextBox`es are the store) — with `tests/e2e-unified/tests/test_event_draft_persistence.py` as their cross-app restart proof, the twin of the two files named above. **A modal compose adds a constraint the always-present composers do not have:** the launch load can land either side of the user opening the form, so a leg that only reads the rail *at open* makes the restore latency-dependent — invisible on a slow nest, and unassertable without a sleep (e2e convention 14). Every leg since therefore also applies a restore that arrives while the form is already up (linux `drafts::attach_form`; web `resumeDraftIfAny` on the opener; android's reactive `State<FfiEventDrafts?>`; apple's `.onChange(of: vm.resumableDraft)`; windows applies the restore straight into its boxes the instant it lands, with no separate open-time step needed since there is no separate rail-held value). **A second rule fell out of the same test and is just as load-bearing: the resume must be NON-DESTRUCTIVE, and the rail — not the compose surface — must hold the live draft.** A compose surface that can be torn down and rebuilt within one session (a Svelte route unmounting on navigation; a transient dialog) takes its own state with it, so a leg that hands the restored draft to the first opener and nothing to the next shows an empty form for a draft the nest still holds — measured on web 2026-08-31, where visiting Settings and returning to Events was enough. tui and linux were already correct by construction (the rail object owns the draft and the opener reads it); that is the shape to copy. This rail's glue is deliberately **not** the shape of the other two: the Events page has no shared manager to observe, so the autosave tick carries the draft rather than pointing at one. **All 7 app legs now ride the shared `fauna_client_drafts::DraftsSync`** — the first 6 on 2026-06-21 (the roster was 6 apps at that date), tui last on 2026-08-04 (`apps/fauna-tui/src/conversations/drafts.rs`, wired from `session::establish` outside the conversations session's MLS gate); [conversations.md](../ui/conversations.md) § Implementation status today carries the current per-app table for this row (priority #1/#2: the launch gate — `save_if_changed` no-ops until `load` runs, the never-PUT-an-empty-snapshot no-data-loss property — plus last-saved-baseline dedup, written once + tier_1-tested, so per-app legs are pure trigger glue): linux native (`conversations/drafts.rs`, ~1.5 s debounce); android / windows / macos+ios over the UniFFI `FfiDraftsSync` wrapper (`ConversationsManagerHost`; `ConversationDraftsService` with restore-on-launch + ~600 ms debounced save; apple `ConversationsVM.activate` + the e2e-path `attachDraftsSync` after `applySessionPatch` — the SAVE-side e2e gap closed 2026-06-22); web over the wasm manager (converged onto the shared gate 2026-06-21 off its early raw-client + local seal latch — the shared gate is strictly safer, staying closed after any failed restore). Windows' full UI→nest→restore round-trip is bridge-e2e-verified (2026-06-28, `test_conversations_draft_persistence.py` `+windows +real_conversations` over the shared `WireConversationDraftsAsync` helper production login also uses) — landing it fixed a windows-only render-echo (`MarkdownRichEditBox` re-fired `TextChanged` on unchanged text, perpetually resetting the save debounce; `BodyChanged` is now value-idempotent) and refuted the earlier "BackupKey gap" framing. Apple's e2e RUN of the restart round-trip is tracked internally. The Feed/Events rails reuse the same plane with `path = "posts"` / `"events"` — the Feed one as of 2026-08-15 (above), with `tests/e2e-unified/tests/test_feed_draft_persistence.py` as its cross-app restart proof, the twin of the conversations file named here.

**Ruled 2026-08-17 (tui): the three trigger-glue shapes stay THREE files, not collapsed into one
generic.** tui's `{conversations,feed}/drafts.rs` are near-byte-identical (a `Weak<Manager>` handle
+ an observer trait that ticks on any manager change + `save_if_changed` re-reading a fresh snapshot
from the manager at save time — no channel payload beyond the tick itself). `events/drafts.rs` is
genuinely different, not cosmetically: the Events page has no manager, so its channel carries the
`EventDrafts` **value** directly, and its debounce loop keeps the newest value seen rather than
re-fetching one. A generic spanning both shapes would need to hide "tick + re-fetch from shared
state" and "carry the value, no shared state to re-fetch from" behind one abstraction — two
fundamentally different concurrency patterns, not one shape with two thin call-sites — and the
result would very likely read *worse* than three self-contained ~250–350-line files. Two copies of
the same shape (conversations/feed) earning a shared trigger module remains open to a future
session if a REAL third copy of that exact shape ever lands; the events rail is not that copy.

**Correction, same pass — and reconciled 2026-08-30:** this section previously (wrongly) credited
windows' `ConversationDraftsService` with a "quit flush." Checked against current code
(`apps/fauna-windows/.../ConversationDraftsService.cs`): `Dispose()` only cancels the pending
debounce timer, it does not fire one more save — still true of that class. **But the windows quit
flush exists at a different door**: `TrayIconService.QuitApplication()` flushes both rails —
`ConvDrafts.SaveNowAsync()` + `FeedDrafts.FlushIfPendingAsync()`
(`apps/fauna-windows/FaunaApp/FaunaApp/Services/TrayIconService.cs`), with the end-session twin in
`RestartManagerService` — proven nest-side by
`tests/e2e-unified/tests/test_windows_window_close_flush.py` (a draft typed inside the debounce
window reaches `__drafts` after a real `WM_CLOSE` quit). **linux, tui and web gained their own
leave-door flush 2026-09-01** (§ The leave-flush promise, Implementation status today, below) —
linux's `blocking_flush` backs `flush_now_blocking` on all three rails, wired into
`connect_close_request`'s sign-out and no-tray-quit branches; tui's `drafts_autosave::flush_now`
(plus the events rail's own) is awaited in `main.rs` right before the process exits; web's
`flushDraftsNow()`/`flushEventDraftNow()` force an immediate save on `visibilitychange`
(hidden)/`pagehide`. **Apple's conversations/feed rails joined 2026-09-21**
(`ConversationsVM.flushDraftsNow`/`FeedVM.flushDraftsNow` over the shared
`flushDraftsAutosave`, reached from each target's leave door through
`AppState.conversationsVM`/`feedVM`). **A door is only as good as its reachability, and that
reachability must not depend on which launch path ran** — the rule the iOS leg's own witness found
the hard way on 2026-09-21: its handoff sat inside optimistic entry, which is a *branch* (withheld
for a pending-factory-reset slot, re-entered only when no client exists), so any launch that
already held a client left the delegate with no `AppState`, and the door's guard took a silent
early exit with the promise off for that whole session. Publish the handoff once per launch, where
macOS always did (the scene's `.onAppear`). This is the class of defect a leave-flush witness
cannot catch while it is *also* able to pass on the debounce — which is why the iOS leg waited for
the window seam rather than shipping sooner. That leaves **android's `ConversationsManagerHost`** as the one
app that still flushes nothing on teardown for those two rails: the very last edit inside the
debounce window (~1.5 s, now uniform across all seven apps — see below) can still be lost there on
a fast quit. The events rail alone is flush-safe by construction
(it carries the value on the channel, so a closed channel can still yield its last one) on the
native apps whose rail IS a channel (tui, linux); android's/web's module-/host-held-state shape does
not get this for free and needs its own explicit flush — android's events rail gained one
(`EventDraftsHost.onStop`, a `ProcessLifecycleOwner` observer) landing its leg 2026-09-01; its
conversations/feed rails and web's own explicit `flushEventDraftNow()` predate this note. What this
paragraph used to file as a fleet-wide papercut "NOT captured as an urgent row" is now a per-app
gap against a ratified promise — § The leave-flush promise, below.

**The debounce window has a single owner: `fauna_client_drafts::AUTOSAVE_DEBOUNCE` (1500 ms).**
The *timer* remains per-app trigger glue — that is the whole point of the split above — but the
*window* is one product decision, and it had been written five times in Rust alone (linux ×2, tui
×3, one per rail, each under a comment promising it matched the others) before converging on the
shared constant 2026-08-22, and independently six more times across the door-crossing apps (android
×2, apple ×1, web ×2, windows ×1) before those converged too: 1500 ms on
android/apple, **1200 ms on web** (with a deliberate 150 ms e2e override, unchanged), **~600 ms on
windows** — a spread nobody had chosen. **All seven apps now read the one constant** through
`fauna-ffi::autosave_debounce_ms()` / `fauna-wasm`'s `autosaveDebounceMs()`; no app holds a debounce
literal of its own beyond web's e2e override, which is a deliberate test-mode variation, not drift.

**Since 2026-09-21 the single owner is an accessor, `fauna_client_drafts::autosave_debounce()`, and
the constant is its production answer.** Read the accessor, not the constant: it is what the two
door-crossing faces above now answer from, so the harness can re-time the window for one run without
any app-side plumbing. That seam — `FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS`, under
[e2e-automation-surface-gating.md](../architecture/e2e-automation-surface-gating.md) § The drafts
autosave-window seam's compile-time gate — exists to **lengthen** the window, never to shorten it:
it is how a leave-flush witness on an app that keeps running after its leave door (iOS) recovers the
property process death gives every other leg, namely that the debounce provably cannot be what
saved the draft. web is the one app the seam cannot reach, because `wasm32-unknown-unknown` has no
process environment; its 150 ms override remains its own substitute, and it shortens where the seam
lengthens, so web's leave-flush leg still races the debounce rather than ruling it out.

### The leave-flush promise

**Ratified 2026-08-30: leaving the app never loses
the compose input the debounced autosave has not yet caught.** Every app flushes its in-progress
draft rails at its own leave door — closing the window or quitting on desktop, moving to the
background on a phone (the OS may kill a backgrounded app at any moment, so the flush rides the
background transition, never the kill), quitting the terminal app, and on web the tab-leave events
the browser actually delivers (`pagehide`/visibility change — a browser grants no reliable async
work after that, so web's door is best-effort by platform fact, stated here rather than hidden).
The door differs per platform; the promise does not, and no app adds a second choice or knob around
it. **Windows is the worked example, e2e-proven**: `TrayIconService.QuitApplication()` flushes both
door-crossing rails — `ConvDrafts.SaveNowAsync()` + `FeedDrafts.FlushIfPendingAsync()` — with the
`RestartManagerService` end-session twin, witnessed nest-side by
`tests/e2e-unified/tests/test_windows_window_close_flush.py`. The events rail is flush-safe by
construction on every app (the autosave channel carries the value itself, per the three-shapes
ruling above).

**Implementation status today (2026-09-01):** windows has a full leave-door flush (all rails).
**linux, tui and web joined the same day** — linux (`conversations`/`feed`/`events`, via the shared
`blocking_flush` worker wired into `connect_close_request`'s sign-out and no-tray-quit branches),
tui (`conversations`/`feed` via `drafts_autosave::flush_now`, plus `events`' own, both awaited
before process exit), and web (`conversations`/`feed`/`events` via `flushDraftsNow()`/
`flushEventDraftNow()`, wired to `visibilitychange`/`pagehide`) — each with its own real-quit/
exit-tab/pagehide e2e witness. Android's events rail gained its own leave-flush 2026-09-01
(`EventDraftsHost.onStop`), and apple's events rail gained its own 2026-09-02 —
macOS folds `EventsVM.flushDraftsNow()` into the existing `applicationShouldTerminate`
bounded quit-flush gate (the engagement-cue rollup's own `.terminateLater` pattern);
iOS fires it from `applicationDidEnterBackground` inside a `beginBackgroundTask`
extension. **Apple's conversations and feed rails joined 2026-09-21**, riding those same two
doors: `ConversationsVM.flushDraftsNow()` and `FeedVM.flushDraftsNow()` (the shared
`flushDraftsAutosave` body, the leave-flush twin of `scheduleDraftsAutosave`) are awaited beside
`EventsVM`'s in macOS's bounded quit gate and inside iOS's background-task extension, reaching both
VMs through the new `AppState.conversationsVM`/`feedVM` slots — the same reachability shape
`eventsVM` already used. **macOS's door is a real quit, never a window close**: closing a window is
deliberately not a quit on that platform (`architecture/apps/macos.md` § Sync), so the witness
drives `NSApp.terminate`. **Android's conversations/feed rails are now the only remaining
gap** (the census in the correction paragraph above is the current per-app truth). The per-column witness is
[`docs/features/drafts-survive.md`](../../features/drafts-survive.md)'s leave-flush outcome, and
the remaining legs are read off `just features-parity drafts-survive` rather than tracked as
rows.

## Contacts Sync

**`__contacts` is RETIRED UNBUILT (ruled 2026-09-19): the folder is never minted, and the private contact overlay rides the account-state plane instead.** The name stays reserved — it sits inside the `__` namespace the management surface already refuses (§ intro) — and must not be reused for anything else. What this section used to route there — the user's own nickname, notes and labels on a person, keyed by that person's actor id — is now one account-state plane item per person under the kind `fauna.state.contact-overlay`: record, merge, succession fold and paint rule are owned by [`../ui/contacts.md`](../ui/contacts.md) § The private overlay, and the kind's audience rung and sealing epoch by [`../architecture/account-data-taxonomy.md`](../architecture/account-data-taxonomy.md) § The audience ladder → *The contact-overlay rung*.

**Why a rail that was ratified on paper is withdrawn rather than built.** Only its *seal* was ever specified. The three-step sync path this section carried (serialise → `encode_blob` under `BackupKey` → a `sync_changes` row) described no transport: the protocol has no `fauna.contacts` get/put pair for it, and it had none of the three things that bound a real rail — a closed path set, a per-blob size cap, and the write-time history collapse (§ Drafts Sync → *The enumeration is closed on purpose*). Building it would have meant growing all of those for a rail whose ratified future is to dissolve onto the plane anyway ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § Substrate settlements), and then carrying a bridge and an at-rest migration for data users cannot recreate. The overlay had **no legacy data** on any box, which is the one moment a substrate can be chosen for free. The standard *address book* (full vCards, the cross-rail mappings that unify one person across `actor_id` + mail address + XMPP JID) was never this section's either: it is the MLS-sealed CardDAV bridge store `bridge_carddav_*` (`docs/goal/behavior/carddav-server.md` § Storage model).

What remains owned here is the split between the routing floor and the sealed contents, because that is a persistence-channel fact that did not move.

**Floor / contents split.** The overlay carries the *user-private contact-record contents* only. The *contact-relationship existence* — the per-edge row `(actor_id, peer_id, status, accepted_at, created_at)` with `status` in `{pending, accepted, confirmed, blocked}` that the knock/DM-arrival hot path reads on every Fauna inbox delivery (not real email — `docs/goal/ui/contacts.md` § Persistence) — lives in the plaintext routing floor (the live `contacts` SQLite table on the nest, `bins/fauna-nest/src/db/contacts.rs`) and stays there. The relationship-state subset is the floor's authoritative representation, not a derived mirror of a sealed overlay source: status changes flow through the WS-RPC kinds `fauna.contacts.confirm`, `fauna.knocks.{accept,block,dismiss,unblock}`, and the knock-arrival `upsert_contact(..., "pending")` write directly to the floor row — paths the nest must be able to mutate without `BackupKey`. The two write paths are operationally distinct (routing state mutates per inbound knock/DM; contents mutate when the user edits them), and the floor representation is the authority for the routing-state subset by construction. If a *new* routing-relevant per-contact field is added later (e.g., a per-contact "always allow regardless of inbox mode" flag — author-mutated, not auto-mutated), it follows the inbox-mode mirror pattern: the flag lives in the sealed overlay item (authoritative) AND mirrors into a plaintext-floor table at update time, written by the client that holds the key. No such field exists today, so no such mirror is required today.

**Out of scope for this section.** Two adjacent concepts have separate at-rest stories:

- **Knock storage** (`bins/fauna-nest/src/db/contacts.rs` § Knocks). The pending-knock table `(actor_id, sender_id, sender_node, summary, payload, created_at)` is a *routing/contact-request artifact*, not user-private contact-record content. Sender identity and timestamps are floor items per `encryption-at-rest.md` § Plaintext floor. The `summary` plaintext field carries a Fauna contact-request summary (`push_knock` from `bins/fauna-nest/src/routes.rs`); it no longer carries an inbound mail-subject fragment — the in-core SMTP path that did (`format!("Email from {from}: {subject}")` in the now-removed `fauna-bridge-smtp`) was excised at the I6 cutover, so the `knocks.summary` plaintext-leak resolution noted in `encryption-at-rest.md` § Plaintext floor (`Mail subject` row) is complete.
- **P2P peer state** (`libs/fauna-peer::PeerContact`). The per-(actor, device) last-known endpoint, success rate, backoff level, LAN endpoints, STUN-discovered endpoints, and feed-sync flags live device-local in each of the user's apps. This is operational state of one device's P2P engine, not a user-content-kind synced between the user's locations; it does not fall under this row. Future cross-device unification of any subset (e.g., a "met in person" flag) would join the contact overlay as an additive field ([`../ui/contacts.md`](../ui/contacts.md) § The private overlay), not a separate at-rest concept.

**Conformance.** The `encryption-at-rest.md` § Per-content-kind conformance row "Contacts" points here for the floor / contents split and to [`../ui/contacts.md`](../ui/contacts.md) § The private overlay for the sealed contents. No nest path reads contact-overlay contents: the items are plane entries sealed on the owner's client under a fleet-only key no grant can reach, and every nest relays them unopened.

## MLS state replica (`__mls`)

The cross-device replica of the user's MLS conversation state is persisted as a single reserved
folder named `__mls`. Authority for the replica *mechanism* — what the replica carries, the
device-owned-epoch write invariant, commit CAS/rebase, bootstrap — is
[`devices.md`](devices.md) § Cross-device MLS group-state sync (design
ratified 2026-07-05, tracked internally); this section is the
at-rest authority for how the bytes are sealed and synced.

**The transport is WS-RPC, not the ordinary file-sync plane** (same posture as
`__drafts`): a client loads/stores sealed blobs via the `fauna.mls.{get,put}` kinds, keyed by an
opaque `path` within the one `__mls` reserved folder — `provider` (the openMLS provider
snapshot, **plus the per-channel ingest cursor** that snapshot was captured at) and
`history/<channel_hex>` (the per-channel thread-store slice, including own-message plaintext and
its attachments' fetch coordinates — contents owned by [`devices.md`](devices.md) § Cross-device MLS
group-state sync).
Each blob is sealed under the owner's `BackupKey` on the client before upload; the nest stores
the bytes raw-opaque per `(actor_id, path_hash)` and records a
`sync_changes` row on put (mirroring `__drafts`). No nest holds `BackupKey`; no nest path reads
replica contents.

**The ingest cursor lives in `provider`, not in `history/*`.** It is the read position of the
crypto state, so one CAS must persist both — a torn `{provider, cursor}` pair is what strands a
device at a dead epoch forever. `history/<ch>` still carries a `watermark` field, now
informational (how much the slice folded) and read as the fallback cursor only for a channel a
device holds a slice of before its first poll has written that channel's cursor. Mechanism + the two ordering
rules: [`devices.md`](devices.md) § Cross-device MLS group-state sync → *Durability rules*.

**Write order within a tick: every `history/<ch>` first, then `provider`, and `provider` is
skipped entirely if any history upload failed** — own-message plaintext is user-irrecoverable, so
a `provider` whose cursor claims a record whose history did not land would durably destroy it.

**Concurrency: `fauna.mls.put` carries a CAS precondition** (`base` =
`Absent` | the loaded blob's content hash; mismatch → `fauna.mls.conflict`), and the client
merges client-side and retries — per-key three-way union for `provider`, per-message union +
watermark-max for `history/*` (commutative + convergent; own-message history is
user-irrecoverable, so the merge is no-data-loss by construction). The one field that is **not** union/theirs-wins is `provider`'s per-channel cursor,
which merges by **`min`**: the merged KV is a mixture of both sides, and a cursor behind the true
read position costs only an idempotent re-walk while one ahead of it strands the device
permanently. This is **not** the blind-overwrite shape the other WS-RPC reserved sets use —
the replica carries irrecoverable data, so CAS is required from day one.

`__mls` is a reserved folder on the same footing as `__drafts`:
created at first use, every one of the user's devices is implicitly a member, and
`fauna.folders.list` does not list it. **Status (2026-07-05): design ratified; slice 1
(the shared-Rust replica core — `fauna-mls::state_replica`,
`fauna-conversations::store::history`), slice 2a (the `fauna.mls.{get,put}` plane itself:
`libs/fauna-protocol/src/mls_replica.rs`, `bins/fauna-nest/src/{mls_replica_handlers,db/
mls_replica}.rs`, CAS enforced, tier_3-proven vs a real nest), and slice 2b (the
`fauna.conversations.channel.send` `expect_no_commit_since` commit gate — nest-side
`fauna.conversations.channel.stale` rejection backed by the per-channel commit high-water
mark `channel_commit_watermark`, enforced under the per-channel seq lock in
`segments::conv::append_gated`, tier_3-proven) are LANDED — **plus slice 3 (the `fauna-mls`
commit-rebase primitives + the gated `channel.send` client seam) and slice 4a: the
`fauna-client-mls-sync` crate — the first + only client consumer of this plane — with the
`MlsStateSync` wrapper (launch gate + baselines, `DraftsSync`-style), the `MlsReplicaClient`
CAS merge-retry (`save_provider_cas` three-way / `save_history_cas` commutative, on
`fauna.mls.conflict`), the per-channel processed-seq cursor, `Zeroizing` at the unseal
boundary, and the `MAX_MLS_REPLICA_BYTES` cap (a shared `fauna-protocol` const; the nest
rejects an over-cap blob `fauna.mls.too_large`, the client checks it before the put). The
client-side commit-rebase LOOP + takeover-before-send (slice 4b), the own-leaf resync (4c),
and the 7 app legs (5) are not yet built** (tracked internally).

## __mail Sync

The mail at-rest authority is the per-actor `__mail/<actor_id_hex>/` reserved folder. Authority for the segment-store *mechanism* — segment file shape, manifest, the SQLite mirror, the at-rest vs transport tiers — is owned by [`docs/goal/architecture/message-segment-store.md`](../architecture/message-segment-store.md); this section is the file-sync-side authority for what rides the chunk pipeline and how it seals at the destination.

`__mail` is a reserved folder on the same footing as `__drafts` and `__index`: created at first use rather than via the user-facing folder creation UI, every one of the user's devices is implicitly a member, and the user-facing folder management surface (`fauna.folders.list`) does not list it.

**The at-rest shape diverges from `__drafts`.** That reserved folder is chunk-encrypted at rest at every location because the nest never reads them. `__mail` is **not** chunk-encrypted at rest on the source nest: the nest must read floor metadata (received_at, sender_dom, spam_disposition, record CID, record block length from the CARv2 index) on every IMAP `SELECT` and every inbound-mail routing decision. So `__mail` is stored as **two storage tiers** (per `docs/goal/architecture/message-segment-store.md` § At-rest vs transport):

1. **At rest on a nest** — plaintext-framed local segment file pairs in `<data_dir>/__mail/<actor_id_hex>/seg-NNNNNNNN.{dat,meta}` (`SegmentManager::scope_dir` — `__<kind>/<scope_hex>`, no `segments/` parent directory). The `.dat` is standard CARv2 (pragma + header + CARv1 data section + MultihashIndexSorted index); the `.meta` is a canonical dag-cbor `SegmentSidecar` carrying per-segment header info + per-record opaque floor metadata. The record block bytes *inside* each CARv2 block are sealed by the kind's inner seal **unconditionally** (per the `Mail body` / `Mail subject` / `Mail attachments` rows in `encryption-at-rest.md` — new ingest always seals since Phase-3, 2026-07-08; pre-Phase-3 raw residue converges via the boot-time `content_seal_backfill.rs` backfill, `nest/storage-modes.md` § Legacy artifacts): the nest reads the framing but never the body. The framing + index + sidecar floor stay plaintext. No `BackupKey` is involved at rest.

2. **At transport time and at a pure-backup destination** — segments ride the standard fauna-sync chunk pipeline (per `file-sync.md` § Content-Addressed Storage): zstd-compressed, then sealed under the data owner's `BackupKey` (the convergent `convergent_chunk_root()` chunk seal — FS-BIND FOLLOW-ON A), stored at the destination keyed by `BLAKE3(ciphertext)` (the manifest's `stored_hashes`, what the destination's F9 chunk route verifies). An *active-replica* destination decrypts arriving chunks and reconstitutes the plaintext-framed segment file locally; a *pure-backup destination* keeps encrypted chunks only and never reconstitutes a segment.

**Who runs the chunk-encrypt step.** The same as for `__drafts`: the data owner's *client* mediates, uniformly across every nest. The client fetches the plaintext-framed segment file from the source nest via `GET /api/v1/segments/{kind}/{actor}/{segment_id}` (an HTTP byte-source endpoint per `docs/goal/architecture/transport.md` § HTTP residue), chunks it with the client's `BackupKey`, and uploads chunks to the destination via the standard `POST /api/v1/chunks` + `POST /api/v1/manifests` pipeline. Source-segment enumeration uses the WS-RPC kind `fauna.segments.list { kind, actor_id }`; the source emits `fauna.segments.changed { kind, actor_id, segment_id, change }` pushes when a segment is finalized, compacted, or tombstoned (per-record arrivals continue to ride the existing `fauna.mail.received` push). The protocol's full semantics — control plane + auth + iOS background-execution constraints — are tracked internally (design ratified 2026-05-15).

**Destination capability is the custody-copy flag — set by the nest that provisions the copy, never declared by the data owner (ruled 2026-09-28; built 2026-09-28).** A `__mail` (or any reserved) set on a nest is one of exactly two things, and the row's `folders.custody_copy` says which (the discriminator's owner is the paragraph *A rail and a custody copy are told apart* above):

- `custody_copy = 1` — a **pure-backup destination**. Holds encrypted chunks, opaque. Never reconstitutes a local segment file. The destination nest refuses IMAP serving for the folder's actor (`fauna.bridges.*` WS-RPC handlers return a typed `pure_backup_destination` error), refuses the `fauna.segments.compact` WS-RPC kind for the actor (`fauna.segments.pure_backup_destination` error), refuses the `fauna.filesync.snapshot.create_message_kind` WS-RPC kind for `kind = "mail"` (`fauna.filesync.snapshot.pure_backup_destination` error), and the scheduled compaction worker skips the actor on its 6-hour sweep — the four gates [`../architecture/message-segment-store.md`](../architecture/message-segment-store.md) § Destination capability owns, all keyed on `is_pure_backup_destination`, which reads the flag. Useful for friend-as-backup and the owner's own offsite-but-rarely-touched node.
- `custody_copy = 0` — the actor's own **live rail** on this nest, exactly the `__mail` their mail feature writes here; it is not a destination of anything.

**The "active replica" destination — a `__mail` copy created as an active replica so the destination decrypts arriving chunks and reconstitutes segment files — is RETIRED as a row state (ruled 2026-09-28).** It was never built: with no chunk-decrypt path on a destination it was "functionally the same as backup", and the re-model has no third row kind to carry it. What a box may read is the set of user-minted capability grants (`../principles.md` § The user always controls their data), so if a readable replica is ever wanted it is a **grant** the owner mints on a custody copy (the owner-provisioned read capability the message-segment-store design of 2026-05-14, tracked internally, deferred under § Open Items), never a mode the copy is created in. `BackupDestination.mode_hint` and its wire and at-rest twins retire with it ([`backup-destinations.md`](backup-destinations.md) § State & data shape → *Capability*).

**Over-cap bodies ride the same pipeline with no file-sync change.** A sealed body larger than one CARv2 record is stored as continuation records — parts + head, ordinary records inside the same `__mail` segment files (`docs/goal/architecture/message-segment-store.md` § Continuation records; ratified 2026-07-12, built and live since the 2026-07-18 write flip) — so backup, restore, and every other chunk-pipeline consumer carry them by construction; nothing mail-sized ever rests in the staging blob store.

**Conformance.** This section is the at-rest authority for the `__mail` reserved folder; the `encryption-at-rest.md` § Per-content-kind conformance rows `Mail body`, `Mail subject`, and `Mail attachments` cite it for the at-rest shape. The seal *of the record payload* stays exactly what those rows commit to (recipient's MLS read key); this section does not change that. The seal *of the chunks at transport / at a pure-backup destination* is the data-owner's `BackupKey` per `file-sync.md` § Content-Addressed Storage.

The same authority extends to the `__conv` (landed on the segment store in Plan 7; cross-location-backup four-gate capability landed in Plan 9), `__post` (landed 2026-06-15), `__calendar`, and `__card` (both live 2026-07-09) reserved folders, which inherit identical at-rest tiering and chunk-pipeline-on-transport semantics — each kind's records remain sealed under that kind's own authority (MLS group epoch key for conversations, MLS read key for calendar events and contact cards, `derive_post_key` for posts); the authoritative kind table (dirs, scopes, per-kind statuses) is `docs/goal/architecture/message-segment-store.md` § Layout. Conversation records' scope is the `channel_id` (the MLS group), not an actor_id — so `__conv/<channel_id_hex>/`, per `docs/goal/architecture/message-segment-store.md` § Layout. Conv's backup destination is the per-channel reserved set `__conv/<channel_hex>` (`folders.actor_id = channel_id`); membership derives from `actor_channels`, and each member backs up the shared channel bytes under their own `BackupKey` (the cross-location-backup coordinator treats conv segment bytes as opaque — the same client-side per-scope driver as `__mail`, scope = `channel_id`).

## __mail-placement Sync

The IMAP placement-journal at-rest authority is the per-actor `__mail-placement/<actor_id_hex>/` reserved folder. Authority for the placement-journal shape and its restore semantics is owned by the IMAP/CalDAV restore design (2026-05-14, tracked internally; becoming a goal doc when the design ratifies into `docs/goal/architecture/`); this section is the file-sync-side authority for what rides the chunk pipeline.

`__mail-placement` is a reserved folder on the same footing as `__mail`: created at first use, every device implicitly a member, not listed by `fauna.folders.list`.

**At-rest shape — same two-tier model as `__mail`.** Segment files (`<data_dir>/__mail-placement/<actor_id_hex>/seg-NNNNNNNN.dat`) are plaintext-framed on the source nest because the placement events they carry are themselves floor metadata per `encryption-at-rest.md § Plaintext floor` (mailbox names, UIDs, flags, modseq, tombstones are nest-readable — they're how IMAP `SELECT` works). At transport time and at pure-backup destinations, segments ride the standard fauna-sync chunk pipeline (zstd + ChaCha20-Poly1305 under `BackupKey`). No new key material is involved.

**Inner-seal-of-payload property.** Unlike `__mail`'s HPKE-sealed payload, the records inside `__mail-placement` segments are **fully plaintext** (no inner seal) — placement events carry no message bodies, only IMAP placement state (routing floor), so they are never sealed (whereas `__mail` payloads are sealed unconditionally under the recipient's mail HPKE keypair, per `encryption-at-rest.md` § Readable classes and the Mail body conformance row). The records remain inside the chunk-pipeline outer seal at the destination; nothing in the placement journal is content-addressed to message bytes.

**Record schema.** Placement events appended on every IMAP state-changing RPC: `Create` / `Delete` / `Rename` (mailbox lifecycle), `Append` / `StoreFlags` / `Move` / `Copy` / `Expunge` (message placement / flags), `Subscribe` / `Unsubscribe` (LSUB). Each event is keyed by `(mailbox, modseq)` — modseq is monotonic per-mailbox per `imap-server.md` § CONDSTORE.

**High-cadence flush flag.** `__mail-placement` carries the `high_cadence` reserved-folder flag (see § High-cadence flush). The chunk-forward coordinator flushes the folder every 5 seconds of activity or every 100 events appended (whichever fires first), instead of waiting for segment-lifecycle events. Rationale: shrinks the "MUA write on the nest but not at the backup destination" window from ~30s to ~5s, keeping the post-DR-restore divergence rare in practice.

**Conformance.** The placement journal is a floor-only data stream — no message bodies, no per-record seal. The cross-location at-rest seal is exclusively the chunk pipeline's outer seal under `BackupKey`. An active-replica destination reconstitutes a plaintext-framed local file (and can serve IMAP itself once the bridge is enrolled); a pure-backup destination keeps encrypted chunks only.

## __calendar-placement Sync

The CalDAV placement-journal at-rest authority is the per-actor `__calendar-placement/<actor_id_hex>/` reserved folder. Mirror of `__mail-placement` (same two-tier model; same plaintext records; same chunk-pipeline outer seal; same `high_cadence` flag); the record schema is calendar-specific.

**Record schema.** Placement events appended on every CalDAV state-changing RPC: `ProvisionCalendar` / `UpdateCalendarMetadata` / `DeleteCalendar` (calendar lifecycle), `PutEvent` / `DeleteEvent` (event placement). Each event is keyed by `(calendar_id, modseq)`.

The encrypted `calendar_metadata` blob shipped inside `ProvisionCalendar` / `UpdateCalendarMetadata` records is the same sealed blob `bridge_caldav_calendars.encrypted_metadata` holds — sealed under the calendar-owner's MLS read key per `encryption-at-rest.md`. The placement journal carries it verbatim; the seal stays exactly the kind's existing seal.

**Smaller than `__mail-placement`.** CalDAV has no flag analogue (no `\Seen` / `\Flagged`); no cross-collection move (MOVE is delete+put per `caldav-server.md` § Write surface). The placement journal's per-event byte cost is correspondingly lower.

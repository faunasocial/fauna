# File sync — target state

Owns: file-sync, folder-exclusive-editing
Status: ratified — the § On-Demand Phase-2 per-set key is a declared-unbuilt target; § Relay serving (ratified 2026-10-01 — the content path of a metadata-only folder on app-only devices) is a declared-unbuilt target; the § Apple apps — convergence design (ratified 2026-07-12) is BUILT (B1–B5 closed 2026-07-13/14; the in-process resident-engine deployment shape was superseded 2026-07-19 by the per-user `fauna-sync-agent` cutover — milestone A4, owned by [`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md)); everything else matches code per the per-section status notes. **Split 2026-08-02:** conflicts, file versions, sealed names & paths, and the reserved `__*` sets each moved to their own owner doc (stub headings below redirect in place). **Split 2026-08-11:** delete propagation — the tombstone record, the apply conditions, the echo guards, the mass-delete floor and the daemon sweep — moved to [`delete-propagation.md`](delete-propagation.md); § Files Appear Automatically keeps the arrival UX and redirects. **Split 2026-09-06:** the engine-vs-deployment model, the Control Plane Principle, and the apple + android convergence records — with the ingress metadata strip carried inside the apple narrative — moved to [`sync-engine-deployments.md`](sync-engine-deployments.md); § Status keeps the wire/channel split and the known-gaps list, and a stub redirects at each original location
Authority: the sync protocol + content-addressed storage — device registration/timing, the WS-RPC control plane + the bulk byte routes + the chunk relay, the chunk/manifest pipeline + the chunk-size/transport contract + `path_hash` derivation, membership projections, and the six-state sync-display vocabulary; defers **where the engine runs and who controls it** — the engine-vs-deployment model, the Control Plane Principle, the per-set nest-row config authority (mode/cadence/selective-sync), the remote-change nudge, the built-in default ignores, the apple + android convergence records and the uploader-side ingress metadata strip — to [`sync-engine-deployments.md`](sync-engine-deployments.md) (**NOT owned here either — split 2026-09-06**; what stays here is the protocol those deployments speak), on-demand placeholders + the hydration-host capability shape to [`on-demand-files.md`](on-demand-files.md), delete propagation — the tombstone record, the row-plus-disk apply conditions, the batch-latest fold, the echo-suppression channels and re-record guards, the mass-delete floor, and the removed daemon's delete-sweep record — to [`delete-propagation.md`](delete-propagation.md) (**NOT owned here either — split 2026-08-11**; what stays here is the change-record substrate a delete rides on: `sync_changes`, catch-up, the remote-change nudge), conflicts to [`conflicts.md`](conflicts.md), file versions to [`file-versions.md`](file-versions.md), sealed names & paths to [`path-sealing.md`](path-sealing.md), the reserved `__*` sets to [`reserved-folders.md`](reserved-folders.md), the folder concept map to [`folders.md`](folders.md), page UX to `../ui/folders.md` + `../ui/media.md`, segment-store mechanics to [`../architecture/message-segment-store.md`](../architecture/message-segment-store.md), snapshot/retention/GC behavior to [`backup-restore.md`](backup-restore.md), key material to [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md), the MLS-replica mechanism to [`devices.md`](devices.md), WebDAV serving to [`webdav-server.md`](webdav-server.md), and the device model + page UX to [`devices.md`](devices.md) + `../ui/devices.md`.

Last verified: 2026-08-20 | Sources: `bins/fauna-nest/src/sync_handlers.rs`, `bins/fauna-nest/src/chunk_relay.rs`, `bins/fauna-nest/src/chunk_routes.rs`, `bins/fauna-nest/src/web_files_projection.rs`, `libs/fauna-sync-engine/src/engine.rs`

How files are kept in sync across devices. **New here?** Read
[`folders.md`](folders.md) first for the folder concept big
picture (the substrate + how Media / Settings →
Folders / Backups / Web hosting relate); this doc owns the sync
*protocol*. See [`../architecture/api-layers.md`](../architecture/api-layers.md)
for the kind ↔ twin map.

## Section map

Four concepts split into their own owner docs on 2026-08-02 — [`conflicts.md`](conflicts.md)
(auto-resolve model), [`file-versions.md`](file-versions.md) (versions projection + restore),
[`path-sealing.md`](path-sealing.md) (sealed names & paths mechanism), and
[`reserved-folders.md`](reserved-folders.md) (the `__*` rails); a fifth followed on
2026-08-03 — [`on-demand-files.md`](on-demand-files.md) (placeholders: the direction ruling,
the present-but-unreadable invariant, the File Provider / cfapi bindings), which alone was 78K
and had this doc within 2K of the Read-tool ceiling. A sixth followed on 2026-08-11 —
[`delete-propagation.md`](delete-propagation.md) (how a delete travels and the guards that
stop one destroying data), 31.5K carrying more than half this doc's weekly growth. A seventh
followed on 2026-09-06 — [`sync-engine-deployments.md`](sync-engine-deployments.md) (where the
engine runs and who controls it, plus each app's convergence onto it), 71K that had accumulated
*inside* § Status under headings that had stopped describing themselves. Stub
headings below redirect at each original location. What remains here:

- **§ Status** — the wire/channel split, known gaps, closed-drift one-liners.
- **§ Control Plane Principle / § Apple apps — convergence design / § Implementation status
  today (android byte-sync)** — all three moved 2026-09-06 to
  [`sync-engine-deployments.md`](sync-engine-deployments.md) (the engine vs its three
  deployments, the nest-row config authority + cadence model, the remote-change nudge, built-in
  default ignores, the apple + android convergence records, and the ingress metadata strip with
  its per-container coverage table). A stub redirects at each original location.
- **§ User Experience** — second device, selecting folders, how a change arrives (the
  remote-change nudge vs. orchestrator forwarding), per-file sync-status display
  (on-demand placeholders + the Apple File Provider binding are now
  [`on-demand-files.md`](on-demand-files.md); how a *delete* travels and the guards on
  applying one are now [`delete-propagation.md`](delete-propagation.md); a pointer
  redirects at each original location).
- **§ Technical Flow** — registration → WebSocket → upload → orchestrator forwarding → offline
  catch-up (steps 1–5, incl. the merge-base clauses).
- **§ Folders / § Multi-writer shared sets** — membership; content residency; content
  reachability (the one owner of what `source_online` means); relay serving (how a
  metadata-only folder's bytes move between app-only devices — unbuilt); the shared-set
  write plane.
- **§ Content-Addressed Storage** — chunk pipeline, chunk-size/transport contract, path hashing.
- **§ Implementing Sync on a New Platform / § FAQ.**

## Status

File sync rides **no sync-specific socket**. The device-sync **control
plane** — device registration, change recording/polling, per-set status,
file/device listing, backup status, relay serving — runs on the bearer WS-RPC
connection as the `fauna.sync.{register,changes.{list,record},status,files,
backup_status,devices.{list,delete},serve.announce}` kinds (Track B13;
`fauna.sync.conflicts.*` under B14), with the remote-change nudge and the
relay's ask as its pushes. Only the bulk byte routes (`/chunks/*`,
`/manifests/*`, and the relay's answer route `/chunks/relay/{request_id}`) stay
HTTP — see `docs/goal/architecture/core-client-kind-catalog.md` § File Sync for
the kind ↔ twin map. (The server-side reassembly route `/sync/file/{*path}` was
deleted 2026-07-14 — `backup-restore.md` § 3. The legacy `/api/v1/sync/ws` data
plane — the headless daemon's real-time forwarding socket, DAG-CBOR
`DeviceMessage`/`NodeMessage` frames — was removed 2026-10-02 with its only
dialer: § Relay serving → *The `/sync/ws` data plane leaves with the daemon*.)

Known gaps that survive into goal/:

- **~~Data-plane socket gaps~~ — MOOT since 2026-10-02.** The `/sync/ws` data plane's three recorded gaps — no per-message HMAC after its bearer-authenticated upgrade, the daemon's TLS dial (closed 2026-08-02 by the shared `fauna_anon_client::tls_dial`), and its 401-blind token refresh — left with the route (§ Relay serving → *The `/sync/ws` data plane leaves with the daemon*); git history keeps their record. Every remaining sync connection is the bearer WS-RPC connection, whose transport trust `security.md` § Transport trust owns.
- **Conflict notification API**: Conflicts are recorded and reported to nest, but no dedicated push notification on conflict creation today; clients poll the `fauna.sync.conflicts.list` WS-RPC kind (≡ the deleted `GET /api/v1/sync/conflicts` twin).
- **A skipped catch-up change reaches the review list — shared half built (found 2026-10-01; RULED 2026-10-02; engine + fold built 2026-10-02).** § 5's promise that a permanently un-appliable change's `catchup_failed` row "surfaces in the conflict review list" now holds on the device side: the engine reports the row to the nest as a candidate-free conflict, re-sends it until acknowledged, and cures it when a later change to the path applies, and the shared fold renders it on every app. The nest's dedupe and device-delete cascade remain open — [`conflicts.md`](conflicts.md) § Skipped catch-up changes reach the review list, the status paragraph. The app-seat witness `tests/e2e-unified/tests/test_filesync_seat_catchup.py` reads the row off the seat's state DB until the tui row lands.
- **Conflict resolution:** the auto-resolve model (text three-way merge when clean, else latest-wins, losers retained as versions, per-set review list) is **built end-to-end** — decision core, nest + wire, both engine paths, and the review-list renders on linux/windows/web/apple; the blocking chooser UX is retired. Remaining legs are per-app parity (android review list; web/android policy selects) — see `conflicts.md` for the authoritative slice status. The candidate/choose-winner wire protocol stays the substrate for old clients within the major version.
- **~~Apple apps run a bespoke Swift sync engine~~ — RESOLVED 2026-07-13/14 (B1–B5).** Apple now runs the shared `fauna-sync-engine` in-process over `FfiSyncEngineHost`; every upload is sealed through the convergent `chunk_crypto` path; the hand-written Swift `SyncEngine`, its FSEvents watcher, and the `SyncDaemonManager` LaunchAgent glue are deleted. B3 closed the photo set name (the wizard-preset Backup set + the legacy-set adoption rule — set-model authority `../ui/folders.md` § Photo backup); B4 the iOS scene-phase lifecycle (a host-level resident pause/resume, retired with the host's resident half 2026-09-25); B5 closed as N/A (user-ruled 2026-07-14) — neither apple write path ever produced a real manifest, so no legacy plaintext back-catalogue existed to migrate, protect, or delete. Full narrative: § Apple apps — convergence design + this section's git history.
- **~~Android runs a bespoke Kotlin byte-sync path~~ — RESOLVED 2026-07-16.** The last non-conforming deployment (§ Control Plane Principle): unsealed chunks, the whole-file hash recorded where the manifest-blob key belongs (so multi-chunk backups were GC-swept — `backup-restore.md` § 9), and a hardcoded `"photos"` set that never existed. Android now runs the shared `FfiSyncEngineHost` over both ingresses (MediaStore + SAF, both library-ingress-shaped); `core/ChunkedSyncEngine.kt` and the bespoke `/api/v1/chunks` + `/manifests` HTTP routes are deleted. The watched-directory legacy-migration question is user-ruled N/A (2026-07-16 — no deployment had ever run a watched-directory scan). Full statement: § Implementation status today (android byte-sync).
- **Semantic chunking**: Format registry wired for 3-way merge but raw `chunk_file()` still used for chunking.
- **~~Selective sync (`include_paths`/`exclude_paths`) filters nothing on the deployments that matter~~ — RESOLVED 2026-08-27** (found in sweep, closed the same week). The nest folder row stores per-set include/exclude patterns (`FolderSummary.include_paths`/`.exclude_paths`), every app's Folders UI sets them, and for a while the **only** production consumer was the LEGACY `bins/fauna-sync` daemon's `apply_folder_row`: both live desktop deployments built their `IgnoreMatcher` with a bare `IgnoreMatcher::load(...)` at construction and never applied the row, so a folder's selective-sync setting round-tripped through the UI with **zero effect** on what synced. The fix is not a second construction-site read — neither builder holds a control plane, which is *why* they could not carry the row — but the shared engine's existing per-row **posture refresh**: the lists now ride the same `fauna.folders.list` read as the mode, the audience, the residency and the accepts gate (`config::resolve_device_mode_from_nest` → `SeatResolution::selective_sync`, rendered sealed-first with the reader's own key material) and install into the RUNNING engine's matcher in `SyncEngine::refresh_sync_mode`, at loop entry and on every rescan tick. One shared-library change therefore covers linux, apple in-process, iOS, android **and** the per-user agent — a flip in any app reaches a running seat within a cadence, in both directions. Per-list failure posture is `SeatResolution`'s throughout: a failed read, an absent row, or a seal this reader cannot open **keeps the armed filter** rather than blanking it (blanking would sync exactly what the user excluded, and the row is the list's only home — `path-sealing.md` § `folders.include_paths`/`exclude_paths`); a row carrying no seal at all installs the empty list, which is what lets a user's *clear the exclusions* gesture un-filter a running seat. Pinned by tier_3 `conformance_selective_sync_install.rs` (arms, un-arms, renders sealed-first against a disagreeing plaintext column, and keeps the armed filter on an unopenable seal). The legacy daemon keeps its own `apply_folder_row` copy of the read, unchanged.
- **On-demand files (placeholders)** (§ On-Demand Files): on **Windows** the full hydration path has now landed in Rust — the shared `SyncEngine::download_file_bytes` primitive, the bearer-only user-session engine host, `changes.list`→`SyncState::Placeholder` population, and the cfapi OS shim (sync-root register/connect plus the `extern "system"` FETCH_DATA / FETCH_PLACEHOLDERS callbacks that hydrate a file on open and populate a directory lazily on first browse) — **and the C# app-side sender now drives it end-to-end**: the WinUI app mints the owner `BackupKey` via the `backup_key_derive` FFI export and pushes `ProvisionCapability` + `RefreshBearer` over `\\.\pipe\fauna-sync` from a **session-scoped provisioning convergence loop** (not a one-shot login push — ratified 2026-07-17; the loop re-provisions a restarted agent within one tick, and its mechanics are owned by `../architecture/apps/windows.md` § On-demand hydration host), so the host starts and the cfapi callbacks are live (no longer dormant). The seed never leaves the app — only the derived `BackupKey` + public `actor_id` + a renewable bearer are sent. No other app surfaces placeholders yet. Remaining refinements: a placeholder's mtime is the change's record time (`changes.list` carries no per-file mtime), and the Phase-2 per-folder key is still a follow-on — both **demand-driven, gated on unscoped upstream**, so neither lives in a NEXT (the watch is retired — goal reached 2026-06-14). The durable record + route-3 entrust path is § On-Demand Files (impl-status, items 2 & 5) below; their upstream triggers are the `changes.list` per-file-`mtime` field (§ Technical Flow step 5) and `chunk_crypto`-keyed folders (`mls-group-key-material.md` § Audience: an MLS group).
- **The exclusive lease: nest half + wire + engine + two-seat witness BUILT; app surface on tui only** (first measured 2026-09-21 while writing the outcome's witness; corrected in place as each half lands, per this bullet's own standing instruction). § Folders promises that *“while a lease is held, other devices treat the folder as read-only”*. **Nest half — built and witnessed:** `fauna.folders.lease.{acquire,release}` (`bins/fauna-nest/src/folder_handlers.rs`) admits one device at a time — the holder may renew, a second device is refused typed `fauna.folders.conflict`, the lease expires on a 300 s TTL, and the lease is bound to the ACCOUNT that took it (`upload_leases.actor_id`, schema 107), so acquire, renewal and release are all scoped to `(actor, device)` and a writer member naming the holder's client-asserted device id can neither drop, renew nor take another account's lease — pinned end-to-end by `tests/e2e-unified/tests/api/test_folder_exclusive_lease.py`. **Property + wire — built:** the `folders.exclusive_editing` column, `FolderUpdateRequest.exclusive_editing`, `FolderSummary.exclusive_editing` and `FolderSummary.lease` (`FolderLeaseState`), all additive and all on both projection arms, pinned by `bins/fauna-nest/tests/conformance_folder_exclusive_editing.rs`. **Engine half — built:** `libs/fauna-sync-engine`'s `folder_lease` module — the governance flag and the holder reading install off the same `fauna.folders.list` read as the mode, the audience, the residency and the selective-sync lists (`SyncEngine::refresh_sync_mode`); a lease-governed folder takes ONE lease per upload pass (`open_lease_window` → `upload_pending` / `apply_local_write` → `close_lease_window`), renews at half the TTL, releases when the pass drains, and defers — never drops — every local edit it could not get the lease for; `upload_file_inner` carries the per-caller choke point so a host reaching past the passes cannot write through another device's lease. **Two-seat witness — built:** tier_3 `bins/fauna-nest/tests/conformance_folder_lease_two_seats.rs` drives two real engines against a real in-process nest through hold → refusal → release → takeover. **App surface — tui built 2026-09-27; the other six owed** (a batched trickle-down): the toggle and the status line (`../ui/folders.md` § Exclusive editing, IDs approved 2026-09-25). Mechanism: § Exclusive editing. Correct this bullet in place as each half lands rather than deleting it, so the app trickle-down keeps its named gap.

### Control Plane Principle

Moved 2026-09-06 to
[`sync-engine-deployments.md`](sync-engine-deployments.md) § Control Plane
Principle — the engine vs. its three deployments, the nest folder row's per-set
config authority and the de-knobbed reconcile cadence, the remote-change nudge,
and the built-in default ignores with the two write doors that enforce them.

### Apple apps — convergence design (ratified 2026-07-12)

Moved 2026-09-06 to
[`sync-engine-deployments.md`](sync-engine-deployments.md) § Apple apps —
convergence design — `FfiSyncEngineHost`, library ingress, the B1–B5 close-out,
and the ingress metadata-strip convergence with its per-container coverage
table.

### Implementation status today (android byte-sync)

Moved 2026-09-06 to
[`sync-engine-deployments.md`](sync-engine-deployments.md) § Implementation
status today (android byte-sync) — the closed-out record of android, the last
non-conforming byte-sync deployment, converging onto the shared engine.

## User Experience

### Installing on a Second Device

A user who already has fauna on one device installs it on another. After signing in with their secret key, the new device registers itself with the nest and opens a persistent WebSocket connection. No manual pairing is required — the nest already knows the user's folders from the first device.

### Selecting Folders to Sync (Folders)

The user selects one or more local folders to sync. Each folder maps to a named **folder** — a logical collection of files that the nest tracks together. Folders can be shared across devices selectively: the user chooses which devices participate in each set.

### Local binding at rest

A device's bindings — which local directory holds which set, the *location* vocabulary (a bound local directory is the set's *location on this device*) — persist device-locally in exactly one place, never on the nest: **`locations`** in the per-user agent's `config.toml` (`bins/fauna-sync-agent/src/config.rs`, `LocationConfig`), the source of truth wherever a location is bound (`../architecture/apps/sync-agent.md` § Control plane split). Each app renders the agent's rows; none keeps a binding record of its own.

**No second binding record exists.** The in-process engine host's `location-map.json` (spelled `folder-map.json` before the phase-1a rename) and the config key's pre-rename `sync_locations` alias were removed by the compat-remnant sweep ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md) § Dimension 2, the fourth exception): the aliases 2026-09-24, and the file 2026-09-25 with the in-process host's resident half — no app binds a location in-process any more, so nothing wrote it. Nothing is migrated.

### Files Appear Automatically

Once a device joins a folder, any file added or changed on any member device appears on every other online member device within seconds. Files are transferred as encrypted, compressed chunks; the receiving device reassembles them on disk.

**The carrier is the remote-change nudge — and since 2026-08-18 it is the ONLY carrier.** On each recorded change the nest fires `PushEvent::SyncChanged` at every connected participant of the set (§ Remote-change nudge, owned in § Config's status text), and each device pulls what it is missing. The former "orchestrator destination forwarding" served admin-registered `folder_destinations` rows that no app or daemon ever created; that phantom rail — table, admin writer kind, forward — was deleted outright (schema v42), so the nudge + pull is the whole delivery path for every device, as it always effectively was. *Corrected 2026-08-01 — this section previously said changes are "forwarded to all other online member devices", contradicting § 4's "propagates it to all destination devices" in the same doc; the code implemented § 4, and the gap was total for the headless daemon (see § Config's status text).*

**How a delete travels — and the guards that stop one destroying data — is now [`delete-propagation.md`](delete-propagation.md)** (split out 2026-08-11): the tombstone record, the row-plus-disk condition for applying one, the batch-latest fold, the echo-suppression channels and re-record guards, the mass-delete floor, and the daemon's delete sweep.

### Per-file sync-status display

Each synced file shows a **sync-status badge** (`sync-state-badge`, the `media` page, inside **`media-item`**) telling the user where that file currently lives. The shared user-facing vocabulary is the **six presence states**:

> **Badge home, corrected 2026-07-13 (user-approved).** This section used to place the badge "inside `file-list-item`" — the per-set row the 2026-06-28 sync/folder UI unification **retired** in favour of the cross-set `media-item` row. The badge had therefore never landed on *any* app: it was specced onto a row the media page no longer renders. It now hangs off `media-item` (ui.yaml `components.media-item`), which is the row that exists. The `sync-state-badge` id stays *additionally* reused on the **backups** page inside `file-list-item` for per-destination status — a different concept, still deliberately not unified (see below).

| Display state | Meaning |
|---|---|
| **Synced** | Present and up to date both locally and on the nest. |
| **LocalOnly** | Present on this device, not yet uploaded. |
| **RemoteOnly** | On the nest only — a placeholder on this device (on-demand), not yet hydrated. |
| **Uploading** | Local changes are being pushed. |
| **Downloading** | Remote bytes are being pulled / hydrated. |
| **Conflict** | Local and remote diverged; resolve per-set on the Settings → Folders page (§ Conflicts Detected and Reported). |

**The state is client-computed, never wire-delivered** (no wire change). The `fauna.sync.files` reply (`SyncFile`, `libs/fauna-protocol/src/sync.rs`) carries **no** per-file status field, and `SyncStatusReply` is per-file-*set* (source device + destinations), not per-file. Each app derives the badge from its **own local sync engine**, so there are two app classes:

- **Control-plane apps for the `media-item` badge (web, the Linux *media page*, android's Media page, and the windows in-app Media page)** have no local-filesystem sync engine behind that page — they list `fauna.sync.files` and treat every listed file as available, so they render only **Synced**. The placeholder / resident / uploading / downloading / conflict lifecycle is **desktop-engine-only**, on the one app that actually renders it. (Web documents this inline at `apps/fauna-web/src/routes/media/+page.svelte`; Linux stamps `synced` in `client.rs::fetch_sync_files`; android's `MediaScreen.kt` reads the same control-plane `MediaMachine` snapshot linux does, not its local Room sync-engine ledger — corrected sweep, this bullet previously listed android under Desktop-engine apps below, which the § *Engine→display map* section already contradicted; windows' in-app Media page is control-plane too, per that section.)
- **Desktop-engine apps (apple's in-process shared engine)** track real per-file state locally and render the full vocabulary. The state comes from the shared engine's own per-set `SyncDb`, read through `FfiSyncEngineHost::file_states` (already collapsed by `SyncState::to_display()`) — there is no Swift-side per-file store: the SwiftData `SyncFile` mirror retired with the B2 cutover. A file in a set this device doesn't sync locally (every set on iOS, any unbound set on macOS) has no engine row and renders **RemoteOnly** — on the nest only — which is what the vocabulary says. (The Windows **cfapi host** also tracks real per-file state, but maps it to the OS-imposed `FileStatus` shell-overlay vocabulary, not `SyncDisplayState` — see the *Windows OS shell-overlay carve-out* paragraph below; it is a third, non-unified case, not a second member of this bullet.)

**Where the logic lives (shared Rust, priority #2/#4).** The vocabulary + its labels are a shared **`fauna_core::format::SyncDisplayState`** enum (six variants, `uniffi::Enum`) + **`sync_display_state_label(SyncDisplayState) -> LocalizedText`** returning the `media.status_label.*` keys — the same render-vs-label split as `offer_status_label` / `contact_status_label`. The badge **visual (icon / color) stays an idiomatic per-app render**; only the **text label** is shared. (android renders a color dot with no text — a conformant compact choice; macOS colors the six states; linux renders icon + label.)

**Engine→display map (8→6).** The daemon-internal engine enum `fauna_sync_engine::SyncState` (eight variants: `Synced`, `LocallyModified`, `RemotelyModified`, `Conflicted`, `Uploading`, `Downloading`, `Placeholder`, `Deleted`) collapses to the six display states via **`SyncState::to_display() -> fauna_core::format::SyncDisplayState`**: `Synced→Synced`, `Uploading→Uploading`, `Downloading→Downloading`, `Conflicted→Conflict`, `Placeholder→RemoteOnly`, `LocallyModified→Uploading`, `RemotelyModified→Downloading`, `Deleted→`(hidden — the file is gone, so no row). Only an app that renders the in-app `SyncDisplayState` badge **from real per-file engine state** needs this map: the windows in-app media page is control-plane (renders `Synced`, like web/linux — its WS-RPC `fauna.sync.files` list carries no per-file status), and the windows `fauna-sync-agent` maps engine state to the **OS shell-overlay** vocabulary (`file_status_from_state` — the carve-out below), *not* `SyncDisplayState`. **`SyncState::to_display()` is built** (`libs/fauna-sync-engine/src/db.rs`; `Deleted` maps to `None` — a deleted file renders no row, and `LocalOnly` is unreachable from an engine row by construction, since a tracked file is already known to the nest). Its **ratified first consumer is the converged apple engine** (§ Apple apps — convergence design): in-process, so no per-file-status pipe IPC is needed — the FFI engine host surfaces the display state directly through `FfiSyncEngineHost::file_states`. **Apple renders it as of the B2 cutover (2026-07-13)** — the first app to render the badge, and the only one rendering the *full* six-state vocabulary from real engine state. **Linux and windows render it too, as of 2026-07-13** (linux — `views/media/item.rs`, shared label + `client.rs::fetch_sync_files` stamping `synced`; windows `MediaPage.xaml` binding `MediaItem.SyncStateLabel`), both as control-plane `Synced`-only legs per the split above. **tui renders it too, as of 2026-07-16** (`apps/fauna-tui/src/media/mod.rs` — the last scoped child of every `media-item` row, same control-plane `Synced` leg, shared label; `../architecture/apps/tui.md` § Implementation status today). **android landed it 2026-07-19** and **web landed it 2026-07-20** (the wasm `syncedStateBadgeLabel` twin in `libs/fauna-wasm-media`), both the same control-plane `Synced`-only leg — all 7 apps now render the badge. (Verified against code 2026-07-16, updated 2026-07-20. A stale denial in `ui/media.md` § Source status vs. sync state — since corrected — had this section's linux/windows status read as authoritative for a "linux is drifting" verdict; linux was conformant all along.)

**Windows OS shell-overlay carve-out (genuine platform divergence, priority #1).** Windows *additionally* maps the engine state to the **OS Cloud Files API overlay** `FileStatus` (`Synced` / `Syncing` / `CloudOnly` / `Error` / `NotTracked`, `bins/fauna-sync-agent/src/path_map.rs::file_status_from_state`) for the File-Explorer pin-state badges. That is an OS-imposed vocabulary, **separate** from the in-app `SyncDisplayState` badge — not a free-choice label, so it is *not* unified onto the six display states.

**OS-initiated dehydration is observed, so the row stays honest.** When the user frees a file's local bytes through Explorer's native *"Free up space"* (or Storage Sense dehydrates under disk pressure), Windows fires the cfapi `NOTIFY_DEHYDRATE_COMPLETION` callback; the on-demand host flips that file's engine row `Synced` → `Placeholder` and pushes `FileStatusChanged{CloudOnly}` — the exact symmetric inverse of the hydrate-on-open flip (`Placeholder` → `Synced` + `FileStatusChanged{Synced}`). Without this the row would keep claiming `Synced` while the disk is empty: *harmless* on its own (the placeholder guard stops reconcile mistaking the absent bytes for a delete — § On-Demand Files → *A placeholder is present-but-unreadable*), but a **dishonest badge**. Registering the callback makes the row *honest*, not merely *harmless*. This is the OS/user-initiated dehydrate; it is distinct from the **provider-initiated** one — the Fauna shell menu's own *"Free up space"* verb (`pipe_server::handle_free_space`) calls `CfDehydratePlaceholder` itself and records the row + pushes the same event directly, because cfapi fires **no** callback for the provider's own I/O. The `_COMPLETION` variant is registered deliberately (not the vetoable pre-dehydrate `NOTIFY_DEHYDRATE`): this is a post-hoc *observation* that needs no acknowledgment and cannot block the user reclaiming space — registering it can only make the row honest, never break "Free up space". Shared-engine hook: `SyncEngine::mark_placeholder` (the inverse of `mark_hydrated`), so a macOS File Provider host inherits the same honest-row behavior (priority #2).

**Implementation status (2026-07-16).** The row-flip **mechanism** is landed and headlessly tested end-to-end: `SyncEngine::mark_placeholder` (engine test) ← `HydrationCommand::Dehydrate` + the `run_hydration_loop` arm (`loop_marks_placeholder_and_emits_cloud_only_on_dehydrate` — asserts the row flips and `FileStatusChanged{CloudOnly}` is pushed) ← the `NOTIFY_DEHYDRATE_COMPLETION` trampoline that feeds that command. The **suppression asymmetry** the whole design rests on is pinned too (`an_in_process_dehydrate_fires_no_callback`): the provider's own dehydrate frees the bytes and fires nothing, which is why the Fauna shell verb records the row itself and why the callback concerns only the OS-initiated dehydrate.

**The pin-state reaction loop is BUILT (2026-07-16, same day the contract was measured).** The provider now does the byte work Explorer's verbs delegate (answered question 2 above): the host reacts **live** off the shared watcher's debounced events (a pin flip is an ordinary attribute-change `Modified` event — measured) and **heals flips made while the service was down** via a stat-only sweep at startup and on every rescan tick, after `converge` so a dirty file uploads before its dehydrate is attempted. Three further OS facts were measured first (`diag_pin_reaction_mechanics`, the re-runnable probe) and the design leans on each:

- **cldflt auto-hydrates a newly-PINNED placeholder itself** through the connected provider's own FETCH_DATA — the live hydrate direction needs no provider reaction at all; only the while-down case does, and the sweep serves it with an in-process `CfHydratePlaceholder` (measured to round-trip through the provider's **own** FETCH_DATA — the own-I/O suppression does *not* apply to explicit hydration requests; the call is parked off the loop thread, which must stay free to serve it).
- **A pin flip does not trip `CF_INSYNC_POLICY_TRACK_ALL`** — the common never-edited "Free up space" dehydrates bare, preserving cfapi's own dirty-file refusal as the first-line edit guard.
- **A tripped not-in-sync bit holds until the provider asserts otherwise** — and nothing ever called `CfSetInSyncState` before this landed, so one local edit made "Free up space" refuse *forever*, even after the edit uploaded. The reaction arm repairs a refusal **only** when the engine's own record proves the disk content is the **recorded head** (`SyncEngine::is_dehydration_safe`: row `Synced` + disk hash == `recorded_content_hash` — the content the recorded head `manifest_hash` reassembles to, stamped only where that head is *proven* to match local content: record success, hydrate-on-open, download apply — shared on the engine so a macOS File Provider evict path inherits the identical gate), which also closes the debounce-window race: an edit the engine hasn't seen yet fails the gate and the platform's refusal stands. Sabotage-verified: removing the gate turns the dirty-file loop test red. **Not merely `local_hash`, and here is why (the 2026-07-18 hardening):** `local_hash` is advanced *optimistically before* a change record (the upload path writes the `Synced` row, then records), so a record that FAILED leaves a `Synced` row whose `local_hash == disk` while `manifest_hash` still points at the OLD merge base — and a re-hydration fetches `manifest_hash`, so dehydrating it would re-anchor the placeholder on the OLD content and the next open would download the OLD bytes, destroying the un-recorded edit. Gating on `recorded_content_hash` (NULL and therefore fail-closed on any unproven head) refuses exactly that row; because the comparison is against the current *disk*, it also refuses an edit landing between record-success and the in-sync flip — and since 2026-09-28 the post-record flip itself answers this gate, atomically (before that the flip was unconditional, so that window was open; the condition is owned by [`on-demand-files.md`](on-demand-files.md) § *Recorded is necessary, not sufficient*). Pinned by `a_record_failed_edit_is_not_dehydration_safe` (the record-failed row stays un-free-able) plus the three proven-head allow-tests (`commit_recorded_head_makes_…`, `mark_hydrated_makes_…`, `a_downloaded_file_is_dehydration_safe`) and the connected-nest `a_successful_record_leaves_the_file_dehydration_safe`.
- **A replacing editor destroys placeholder-ness entirely** — a save-by-replace/truncate (how most editors write) leaves an ordinary file with **no reparse point**: every placeholder op on it fails `0x80070178` "not a cloud file", so neither the bare dehydrate nor the in-sync repair can ever free it. The proven-uploaded repair therefore branches on cloud-file-ness: a still-anchored dirty placeholder gets the in-sync assertion + a bare-dehydrate retry; a replaced ordinary file is first re-anchored **not** in-sync (`CfConvertToPlaceholder` with no flags, the folder-relative path as its `FileIdentity`; needs a plain Win32 handle, the oplock-protected one is refused `0x80070006`) and then takes the same assertion + retry. The assertion is the one atomic prove-then-assert road the post-record flip uses (owned by [`on-demand-files.md`](on-demand-files.md) § *Recorded is necessary, not sufficient*). *Until 2026-09-28 the replaced file took one `CfConvertToPlaceholder(MARK_IN_SYNC | DEHYDRATE)` (measured to land on the cloud-only `0x401620` signature) — retired because a convert takes no USN condition, so a save landing after the proof was vouched for and freed in the same op.*
- **A graceful provider teardown (`disconnect` + unregister) removes un-hydrated cloud-only placeholders from disk** (measured: the file stops existing, `0x80070002`) — so "pinned while the provider was down" is a state only a *crash* leaves behind, and the sweep's while-down heal is tested against a survived filter registration.

Pinned live against a real cfapi root, all four paths (`cfapi_live_integration.rs`): `an_explorer_unpin_dehydrates_and_flips_the_badge` (watcher arm), `an_explorer_pin_on_a_placeholder_hydrates_through_our_fetch_arm` (the OS-auto-hydration contract — goes red if Windows ever stops), `an_edit_plus_unpin_uploads_the_edit_but_does_not_free_it_until_recorded` (the iron rule under the verb: an edit uploads but is **not** freed while its record is un-landed — freeing an unrecorded edit is the data loss the recorded-head gate refuses), `pin_flips_made_while_the_service_was_down_are_healed_at_startup` (the sweep, both directions); and the in-sync assertion's own atomicity by `a_save_during_an_upload_survives_the_in_sync_flip_and_a_free_up_space` and `a_save_between_the_proof_and_the_in_sync_assertion_is_refused`.

What is **not** verified is the callback *firing* on a real OS dehydrate, because no headless probe reproduces one. Measured on real Windows against a live connected root holding a hydrated file — **`diag_cross_process_dehydrate` re-runs all four in ~20 s; prefer it to this prose**: an ordinary `CreateFile`, `CfOpenFileWithOplock`, and `CfSetPinState(UNPINNED)` all **succeed** cross-process but none dehydrate (unpin leaves the bytes until Storage Sense feels disk pressure — the same reason `attrib +U` doesn't); a cross-process `CfDehydratePlaceholder` **hangs indefinitely** (no return in 120 s — so not cfapi's 60 s recall timeout, and not loop starvation: the driving loop is polled throughout).

**Two claims recorded on 2026-07-16 were retracted the same day — do not re-derive them.** (1) The hang was recorded as being in `CfOpenFileWithOplock`; it measurably is not — that call returns. (2) From that it was inferred that the OS gates third-party dehydration on the provider ACK-ing the vetoable pre-`NOTIFY_DEHYDRATE`. It never followed: `dehydrate_placeholder` *opens before it dehydrates*, so a hang at the open precedes any dehydrate request — there is no request for a handler to ACK, which is exactly why adding one changed nothing. The `_COMPLETION`-only choice above therefore **stands unamended**, and on its own merits: a vetoable callback the provider must answer can block a user reclaiming disk, which is the one thing an observation must never do.

**The live-box questions are ANSWERED (measured 2026-07-16 against a real Explorer, Windows 11 ARM64, via the `hold_a_live_root_for_explorer` harness + the dev-fleet UIA menu-reading script):**

1. **Appear?** **Yes — the product host shell-registers as of 2026-07-16.** Explorer's cloud verbs (*"Free up space"*, *"Always keep on this device"*), the sync-status column, and the provider grouping are keyed off the **shell** registration (`StorageProviderSyncRootManager`, the `SyncRootManager` registry state) — the bare `CfRegisterSyncRoot` filter registration writes none of it (measured both ways: no verbs on a filter-only root; both verbs render once the same root is shell-registered with `AllowPinning=true`). The product path (`cfapi_host::register_and_connect_shell`, called by every on-demand engine) shell-registers with a per-folder display name. A folder that is filter-registered but not shell-registered needs no special handling and is never unregistered: the WinRT `Register` registers over the filter registration in place (measured 2026-09-25, connected or not — a 2026-07-16 measurement saw a `0x8007018B` refusal there that no longer reproduces, so a shell `Register` succeeding proves nothing about the filter side; `fauna_cfapi::is_filter_registered` observes it). The product itself never leaves a filter-only root, since the ended-binding teardown takes the filter side down first. The reverse order, `CF_REGISTER_FLAG_UPDATE` over a shell-registered root, is tolerated and is the normal reconnect path). **The registration lifecycle is persistent — it belongs to the folder↔folder *binding*, not the serve-session** (see the lifecycle block below).
2. **Complete?** **Yes, instantly — and it never dehydrates.** Explorer's *"Free up space"* is `CfSetPinState(UNPINNED)` (the `FILE_ATTRIBUTE_UNPINNED` flip was observed; bytes intact 180 s later; Explorer fully responsive throughout). It is **not** a cross-process `CfDehydratePlaceholder`, so the measured indefinite hang in that call is **not on any Explorer user path** — the hang worry dissolves for this verb (it remains unprobed for Storage Sense, which is OS-internal). Symmetrically, *"Always keep on this device"* is `CfSetPinState(PINNED)`. Both verbs are pure pin-state writes: **the provider is expected to observe the pin-state transition and do the byte work itself** — dehydrate on UNPINNED (the provider's own in-process `CfDehydratePlaceholder` works and frees the bytes — pinned green by `an_in_process_dehydrate_fires_no_callback`), hydrate on PINNED. **The pin-state reaction loop is BUILT (2026-07-16)** — see the implementation-status block below for its shape and the further-measured OS facts it rests on.
3. **Badge flip?** No — **correctly**: nothing dehydrated, the bytes are still local, so the row staying `Synced` is honest, and `NOTIFY_DEHYDRATE_COMPLETION` never fired because no dehydration ever happened. The flip mechanism is untriggered, not broken. The `_COMPLETION` registration stands for genuinely OS-initiated dehydration (Storage Sense under disk pressure); Explorer's verb is covered by the provider-side pin-reaction path, which records the row itself (`handle_free_space` mechanics — no callback fires for the provider's own dehydrate).

Both follow-ons the live pass left are BUILT: the **pin-state reaction loop** (implementation-status block above) and **shell registration in the product host** (2026-07-16, this block).

**A synced local edit flips the file to the platform's ✅ (in-sync) — the outbound symmetric of the OS-dehydrate flip (LANDED 2026-07-17).** When a local edit's change *record* reaches the nest, the on-demand host marks the file a placeholder IN_SYNC so Explorer's sync-status column drops the pending-upload arrows and shows the check; without it a perfectly-synced edit shows the arrows **forever** (measured live 2026-07-17). The repair branches on cloud-file-ness exactly as the pin-reaction one does — a still-anchored placeholder gets the in-sync assertion; an editor's replace-save that destroyed the placeholder is first re-anchored not-in-sync by a `CfConvertToPlaceholder` carrying the folder-relative `FileIdentity` (never the identity-less convert — the `FileIdentity` bug class — never a convert that marks a file in-sync, and never the byte-freeing `DEHYDRATE` variant: the user's bytes stay local) and then takes the same assertion, and a clean ancestor directory whose whole known subtree is synced flips too, deepest-first (an empty or not-fully-synced dir stays honestly pending). It fires on **every** rel whose upload recorded — the live watcher path (`apply_local_write`) and the `LocalWriteHost::converge` backstop alike (startup catch-up, offline edits, and the rescan tick), so an edit uploaded by `converge` rather than the live watcher is **not** left showing arrows until the next edit (the startup-converge race, measured live 2026-07-17). Keyed on `UploadOutcome::recorded` (the change record reached the nest and `commit_recorded_head` stamped the row), so a failed record never flips a file that is not actually synced — and **conditioned on the disk still being the recorded content**, proven and asserted atomically, so a save that landed after the upload read the file is never vouched for (2026-09-28; the condition and its mechanism are owned by [`on-demand-files.md`](on-demand-files.md) § *Recorded is necessary, not sufficient*). Pinned by `flip_recorded_in_sync_picks_the_repair_by_cloud_file_ness`, `the_flip_never_vouches_for_a_save_newer_than_the_recorded_content`, `the_startup_converge_flips_its_recorded_uploads_in_sync`, `the_rescan_tick_flips_its_recorded_uploads_in_sync` (bridge-loop tests) + the recorded-head / folder-flip engine tests; the live Explorer ✅ render is the one human-eye inch on top.

**The registration lifecycle is persistent (ratified with the shell-registration landing, 2026-07-16).** A sync-root registration (shell + filter) belongs to the folder↔folder **binding**, and comes down only when the binding ends — unbind, folder removal, re-bind to a different folder, or a mode flip to always-resident. A service stop, restart, crash, or capability loss only **disconnects**; the registration (and therefore every placeholder Explorer shows) survives, and the next start re-connects over it (`CF_REGISTER_FLAG_UPDATE`, the same measured path the crash-heal test drives). Why this and not per-serve-session register/unregister (what shipped before): a graceful unregister was **measured to remove un-hydrated cloud-only placeholders from disk**, so per-session registration emptied and repopulated every on-demand folder at each service restart — churn plus a transient "my files are gone". Persistence also collapses the crash path and the graceful path into one state, already healed by the startup sweep. Mechanics (the teardown-intent mark consumed in the connection guard's drop, the ghost janitor sweeping registrations whose folder no longer exists at service startup, the recomputable-never-persisted `Fauna!SID!account` id) live in `cfapi_host.rs`; the platform-binding notes in `../architecture/apps/windows.md` § Shell Extension. Pinned live by four tests in `cfapi_live_integration.rs`: `a_shell_registered_root_keeps_placeholders_across_a_service_stop`, `a_filter_registered_root_shell_registers_in_place` (a leftover filter registration is shell-registered over in place, hydrated bytes untouched), `an_ended_binding_fully_unregisters_filter_and_shell`, `the_startup_sweep_removes_ghosts_and_spares_live_roots`. ⚠ One measured trap for any future query code: for an unpackaged Win32 process the WinRT **read** surface is dead — `GetCurrentSyncRoots` returns an empty list and `GetSyncRootInformationForFolder` fails `0x80070490` while the registration demonstrably exists — so enumeration reads the `SyncRootManager` registry state (`fauna_cfapi::list_shell_sync_roots`; probe `libs/fauna-cfapi/tests/shell_enumeration_probe.rs` re-measures in ~0.2 s).

**Not the same as backup-destination status.** The `sync-state-badge` reused on the *backups* page row is a different concept — per-**destination** upload status (`backup-destinations.md` § State & data shape → Per-destination status read, `BackupDestinationStatus`), not per-file presence. The two are intentionally not unified.

**Implementation status today (updated 2026-07-13).** The shared `fauna_core::format::SyncDisplayState` (six-variant `uniffi::Enum`) + `sync_display_state_label` are **BUILT** (tier_1 `value_format_tests::sync_display_state_label_maps_every_variant`); all six `media.status_label.*` i18n keys exist. `SyncState::to_display()` (the 8→6 engine collapse) is **BUILT** (`libs/fauna-sync-engine/src/db.rs`, landed with B1) and now has its first consumer.

**The badge first rendered on apple.** The B2 cutover (2026-07-13) put `sync-state-badge` on the `media-item` row on macOS + iOS, sourced from the in-process engine's per-set `SyncDb` through `FfiSyncEngineHost::file_states`, with the label from the shared `sync_display_state_label` and only icon/color rendered per-app. (Before B2 **no** app rendered it: the badge had been specced onto `file-list-item`, a row the 2026-06-28 unification retired — see the callout at the top of this section.) **⚠ "the other four apps still owe their leg" / android's detail-screen color-dot claim, both formerly here, are stale — see the paragraph above for the current per-app status** (linux, windows and tui landed the control-plane `Synced` leg; **android landed it too, 2026-07-19** — `MediaScreen.kt`'s `media-item` row renders the constant `Synced` state via the shared `sync_display_state_label`, mirroring linux's `build_state_badge` rather than apple's per-file `SyncStatesStore`, since android's unified Media page reads the same control-plane `MediaMachine` snapshot linux does, not its local `Room` sync-engine ledger — **web landed 2026-07-20**, closing the last gap: the wasm `syncedStateBadgeLabel` twin (`libs/fauna-wasm-media`) returns the shared `Synced` label, rendered on each `media-item` row in `routes/media/+page.svelte`, mirroring linux's `build_state_badge`). All 7 apps now render `sync-state-badge`. The per-app raw-string / borrowed-key drift this unified is history (fixed 2026-06-28/29; details in git).

### On-Demand Files (Placeholders)

**Moved to [`on-demand-files.md`](on-demand-files.md)** (2026-08-03 split) — the direction
ruling (on-demand is a *storage* choice, never a *direction* choice), the
present-but-unreadable placeholder invariant, hydration/dehydration, the recorded-upload
head flip, and the Apple File Provider + Windows cfapi bindings. It had grown to 78K under
this § *User Experience* heading, which describes almost none of it.

### Conflicts Detected and Reported

If two devices modify the same file while both are online — or if an offline device made changes that conflict with changes recorded during its absence — the nest flags a conflict. **Built end-to-end (ratified 2026-07-10, landed 2026-07-11 — [`conflicts.md`](conflicts.md)):** the conflict auto-resolves per the set's policy (text three-way merge when clean, else latest-writer-wins), the losing version is retained in version history, and the per-set review list on Settings → Folders lets the user re-point to the other version later — nothing blocks on the user. No app renders a blocking chooser; the candidate/choose-winner wire stays only as the substrate for clients built before 2026-07-11.

### Offline Changes Sync on Reconnect

Changes made while a device is offline are recorded locally. On reconnect, the device polls the nest for any changes it missed (using its last-seen sequence number) and uploads its own queued changes. The nest applies them in sequence-number order and forwards them to any currently online devices.

## Technical Flow

### 1. Device Registration

Before a device can sync, it must register with the nest.

**WS-RPC kind:** `fauna.sync.register` (≡ the deleted `POST /api/v1/sync/register` twin).

**Request:**

```json
{
  "device_id": "<uuid>",
  "label": "Alice's Laptop",
  "capabilities": ["read", "write"]
}
```

- `device_id` — random UUID generated locally, per-device, not tied to the actor keypair.
- `label` — human-readable name shown in device management UI.
- `capabilities` — `read` (receive changes), `write` (send changes), or both. Backup-only nodes use `read` only.

The nest stores the device record (`INSERT OR REPLACE`, so re-registering the same `device_id` is idempotent and the label is authoritative) and associates it with the authenticated actor.

**When a device registers (registration timing).** A device registers when it first needs to act, not merely on login:

- A **foreground sync client** registers when it maps a sync folder (per folder — `apps/fauna-linux/src/sync.rs` `engine_lifecycle`); the headless `bins/fauna-sync` daemon (removed 2026-10-02) registered on every start, **before its first data-plane dial**. Until one register was accepted it dialed nothing: it retried on its redial curve ([`../architecture/transport-connection.md`](../architecture/transport-connection.md) § Connection lifecycle) and logged each failure by its cause, so a typed refusal such as the tier's device cap ([`devices.md`](devices.md) § Step 4) was named with its code and remedy, never read as an offline nest; a changed nest identity was the one failure it did not retry ([`../architecture/security.md`](../architecture/security.md) § Pin custody across processes). An app or agent register carries the client's authoritative `label`.
- A **Media write** (upload create / delete tombstone), an ordinary sync-engine upload's change record, or a version restore from a device that is **not yet registered** self-registers it write-capable: the nest rejects an unregistered recording device with the dedicated `fauna.sync.device_unregistered` error (distinct from the overloaded `permission_denied` used for an authz denial or a *read-only* device), and `fauna_client_sync::SyncClient::changes_record` — the **one** public record entry point, self-healing by construction (2026-07-17; there is no longer a non-healing variant to call by mistake) — responds by calling the idempotent `fauna.sync.register` (write-capable, a placeholder label) and retrying once. Media's write seam (`libs/fauna-media-machine` `nest_api::ws_rpc::record_self_healing`) and `SyncEngine::record_change` both call this one entry, so the heal is uniform across every writer rather than an opt-in a caller could skip — closing a live gap where the engine's own upload path called a non-healing variant and left a never-registered device's uploads permanently unrecorded (sync-pending forever, empty version history). This makes **folder-less media upload, and any first write from a freshly-provisioned device, work uniformly on every app** (one shared change, no per-app wiring; priorities #1/#2). It fires **only** for a never-registered device — an already-registered one records on the first try — so it never clobbers a device's authoritative label; a later location-binding registration (`INSERT OR REPLACE`) supersedes the placeholder. The read-only case is **not** auto-upgraded (a backup-only node stays read-only).
- Consequence for the **roster** (`devices.md`): a device appears in Settings → Devices once it has registered via any of the above — a location binding or a Media write — **not** on login alone. Registering *every* logged-in client on connect (so the roster reflects presence immediately, with each app's real label) is a possible future refinement; until then a media-only client that never maps a folder shows the generic placeholder label.

### 2. Connection

After registration the device's engine works over the account's one bearer
WS-RPC connection — the `fauna.sync.*` kinds, the remote-change nudge as a push
(§ Config) and, for a seat that serves the relay, the announce and the ask
(§ Relay serving) — plus the bulk byte routes. There is no sync-specific socket:
the legacy `GET /api/v1/sync/ws` data plane, with its `Hello` handshake and its
table of connected seats, was removed 2026-10-02 (§ Relay serving → *The
`/sync/ws` data plane leaves with the daemon*).

### 3. File Upload (Source Device)

When a file changes on a device:

1. **File watcher detects the change** — platform file-system event (inotify on linux, FSEvents on macOS, ReadDirectoryChangesW on windows). ⚠ **android is the exception and has no watcher at all**: `core/WatchedDirectoryManager.kt` is a *manual, one-shot SAF scan* the user triggers (`WatchedDirectoryVM.scanAll()`), not an observer — the app registers no `FileObserver`/`ContentObserver` anywhere, and its bindings are SAF `content://` tree URIs, which expose no path for a watcher to open. (Corrected 2026-07-16 — this line and § Implementing Sync on a New Platform previously described android as "observer-based", which no android build has ever been.) So a live android edit is **never** picked up until the next manual scan, and a modified file is never re-uploaded at all (the scan dedups on row-existence, not content). Both are tracked in § Implementation status today (android byte-sync).
2. **Chunk splitting and hashing** — the file is split into variable-size chunks. Each chunk is identified by its BLAKE3 hash (content-addressed).
3. **Chunk upload** — each chunk is encrypted, compressed, and uploaded:

   ```
   POST /api/v1/chunks
   Body: binary (encoded blob)
   Response: { "chunk_hash": "<blake3-hex>" }
   ```

   Chunks that already exist on the server (same hash) are skipped — the server returns the existing hash without re-storing.

4. **Manifest upload** — a manifest (ordered list of chunk hashes that reconstruct the file) is uploaded:

   ```
   POST /api/v1/manifests
   Body: { "chunks": ["<hash1>", "<hash2>", ...] }
   Response: { "manifest_hash": "<blake3-hex>" }
   ```

5. **Change recorded** — the device records the file change with the
   `fauna.sync.changes.record` WS-RPC kind (≡ the deleted
   `POST /api/v1/sync/changes` twin).

   `change_type` is the lowercase verb `create`, `modify`, or `delete` — the
   value the engine emits, stored and surfaced by
   `fauna.sync.changes.list` verbatim (the deleted REST twin used the same).

   > **No per-file modification time today.** A change record (and the
   > `SyncChange` the nest replays via `fauna.sync.changes.list`) carries only
   > `created_at` — the nest's record time — and **no per-file `mtime`**. The
   > on-demand placeholder host (§ On-Demand Files) therefore records each
   > placeholder's mtime as `created_at`, not the file's real modification time.
   > Adding a per-file `mtime` to this wire shape is the **upstream half** of
   > that deferred follow-on (§ On-Demand Files impl-status, item 2): whoever
   > adds the field here should also schedule the windows/linux/macOS hydration
   > host's consumption half so placeholders surface the real mtime.

6. **Sequence number assigned** — the nest assigns a monotonically increasing sequence number to the change for this folder. This number is used by offline devices to catch up.

### 4. Delivery (Nest → the other devices)

The nest forwards no file. After recording a change it sends the remote-change
nudge to the actor's other connections (§ Config), and each seat catches up by
pulling `fauna.sync.changes.list` since its anchor (§ 5) and fetching the
change's manifest and chunks from the bulk byte routes. A chunk the blob store
does not hold — always, for a metadata-only folder (§ Content residency) — is
relayed from a seat that announced the folder (§ Relay serving). The blob store
is `encode_blob`-format (compress → encrypt, § Content-Addressed Storage), and
every reader — the byte routes and the relay alike — decodes back to the raw
chunk before serving it, so a reader's `BLAKE3(chunk) == hash` check runs on raw
bytes. (Until 2026-08-18 an orchestrator forwarded `StoreManifest` /
`StoreChunk` / `ApplyChange` frames to destinations on the legacy `/sync/ws` data
plane; the destination rail went that day and the socket on 2026-10-02.)

**Archive-seat delete suppression binds EVERY rail (corrected 2026-08-02).** Until 2026-08-02 it was enforced only in the nest's destination forward — which protected almost nobody, because that guard keyed on an admin-registered `folder_destinations` row and **no app or daemon ever created one** (the whole phantom rail, forward included, was deleted outright 2026-08-18) (the same fact that hid the total device-to-device nudge failure — § Config's status text). Both client-side rails applied the delete anyway, so a backup destination lost the files its source deleted: a § *No user-data loss* violation (`../principles.md`), caught by the tier_3 `test_ws_backup_delete_suppression` and by nothing else.

The rule is therefore stated on the **device's resolved mode**, not on the forward: **a backup seat never applies a peer's delete to its own disk, whatever rail delivered it.** The shared predicate is `fauna_sync_engine::config::SyncMode::applies_remote_deletes`, and every rail that can write a peer's delete consults it — the shared engine's delete arm (`libs/fauna-sync-engine/src/engine.rs`, so every desktop's per-user agent is covered too), and, until it was removed 2026-10-02, the headless daemon's single apply choke point (`handle_apply_change`). (The nest-side destination forward this list used to open with was the phantom `folder_destinations` rail — deleted whole 2026-08-18; production delivery is nudge + pull, so every delete-writing rail is client-side.)

**The place's `accepts` flag gates delivery WHOLE, upstream of the delete guard (folders re-model phase 2 slice c, 2026-08-19).** `PlaceFlags::accepts` is now real: a seat whose place does not accept remote changes skips its delivery rails entirely — the engine's `pull_remote_changes` and `populate_placeholders_from_nest` return before fetching (the removed daemon's `handle_apply_change` choke point declined every inbound change type before any side effect, as `DeclinedHold`). The posture rules, each deliberate: **hold, never advance** — the engine's anchor stays put, because delivery is switched *off*, not the changes disclaimed, so a later flag flip to accepting resumes from exactly where delivery stopped, nothing skipped; **default accept** — every seat's behavior before the flag became real, kept by an unreadable seat or a failed read (`SeatResolution::accepts` is `None` there and the installs skip it — the audience flag's failure discipline), and safe because *both* wrong directions are recoverable, unlike the delete guard's; **live** — the flag rides the same authoritative roster read the mode does (`config::accepts_from_seat`, pure and pinned) and re-installs on every rescan tick, so a place-flag edit reaches a running seat within one cadence. A seat with **no roster row** (`SeatRead::Absent` — a member of someone else's set, whose single-owner roster cannot carry one) positively accepts: accepting is what such a seat is *for*. Making `accepts` real is also what finally makes a source place's `applies_deletes: false` non-vacuous — no delete arrives to decline — **without** simplifying `SyncMode::from_place_flags` to a single flag: the slice-c plan expected that simplification, and the consumer audit refuted it (`SyncMode` also drove the since-removed daemon's upload-side `push_essential` accounting, so remapping `source` from `Sync` to `Backup` would change behavior for no benefit — the projection's own rustdoc records it).

**What resolves a seat's behavior is THIS device's place flags — never a folder-level knob (second correction, 2026-08-02, caught by the tier_3 confirmation run the first correction shipped without; reshaped by the mode and `role` contractions, 2026-09-28/29).** A folder has no type, and the recording projection (`sync_changes` / `backup_custody` / `web_files` — § Membership) is one value for every seat, while what one seat does with what arrives is its own place. Keying delete-suppression on a folder-level value was vacuous where it held and wrong where it mattered: an archive seat of an ordinary folder — the wizard's "this laptop syncs, the NAS archives" shape, reachable from every app's UI — resolved to plain sync and erased the very files it existed to keep. Resolution is through the shared `config::SeatRead` + `resolve_device_mode`: a roster row's **place flags** decide (`originates`/`accepts`/`applies_deletes` — owner [`folders.md`](folders.md) § Target re-model; carried on the wire as the required `FolderMember.flags` and at rest on `folder_members`). `accepts && !applies_deletes` ⇒ the archive seat ⇒ suppress; anything else converges — a *mirror* being an exact replica that tracks deletions by definition. **The mode-free rule:** when the roster says nothing about *this* device (`SeatRead::Absent` — no row for it) the seat is the default point, and its mode is runtime state the resolver installs, never a stored folder property. Read from the authoritative nest rows — never invented client-side, and never taken from a device's own config file.

**What arms the guard is LIVE, cached, and single-implementation (third correction, 2026-08-02 — the arming finding; the first two corrections fixed the guard, this one fixes what arms it).** Three sentences the previous prose did not say, each closing a leg that let a correctly-written guard protect nobody:

1. **The resolution is not once-per-process, and here is what re-reads it.** Every production reader routes the two authoritative reads (`fauna.folders.list` + `fauna.folders.members.list`) through one shared implementation — the pure `fauna_sync_engine::config::resolve_device_mode` under `resolve_device_mode_from_nest` — and re-runs it on a live seat: the resident engines at `always_resident::run_watch_loop` entry **and every rescan tick** (`SyncEngine::refresh_sync_mode`), and the one-shot pass at its entry (the headless daemon, removed 2026-10-02, did the same at every session start and rescan tick). A role the user changes in the wizard's picker therefore reaches a *running* seat within one cadence — never "at the next process restart". (An engine-restart-based design was considered and rejected: `reconcile_engines` only runs on config/capability edges, and a nest-side role write raises none of them.) **The `members.list` half of that read is skipped for a roster-member's own set (fixed 2026-08-11):** `folder_members` (the per-device role table `members.list` projects) is a **single-owner multi-device roster** — the nest's `add_folder_member_for_user` requires the device and the set to share one `actor_id` — so a genuine cross-user roster member's device can *never* hold a row there, and the read was a guaranteed, log-spamming `not_found` every reconcile tick for a member's own bound shared set (previously mis-diagnosed as the same owner-scoped-roster class as the sibling sites in point 2 below, which are client-side *set-selection* bugs, not this nest-side *per-device-role* read — widening the nest-side authorization was considered and rejected: it would hand a set's device labels to every roster member for no behavioral gain, since the answer for a member is always "no role row"). `should_read_member_role` (`config.rs`) recognizes a `role: Some("member")` row from the member-visible `fauna.folders.list` projection and resolves the known-empty answer locally — same match arm as a real empty roster reply, never the failed-read fallback.
2. **An *unresolved* seat declines rather than converges, and the anchor tells the two declines apart.** When the reads fail and no cached answer exists, the seat's mode is `Unresolved` (`config::ModeResolution`): it declines remote deletes **and holds its anchor** at the first declined tombstone (the engine's transient-class cap, the same as the sealed-path degrade), so the tombstone re-delivers once the role is readable. A **resolved `backup`** decline still advances the anchor — that suppression is deliberate and permanent, and re-fetching a forever-declined tombstone is waste. (This clause is the backup role's deliberate *exception* to the anchor rule owned by [`delete-propagation.md`](delete-propagation.md) — *a skipped delete never comes back* — which is why the hold above exists for the unresolved case.) This *replaces* the former "an unreadable row keeps converging deletes" direction: the two error directions are not symmetric in reversibility (§ *No user-data loss*, `../principles.md`) — a delete wrongly withheld is recoverable (the file is still on disk and the held tombstone re-delivers), while a delete wrongly applied on a backup seat destroys the one copy meant to outlive the source's deletion. The opt-in clause survives for the *readable* case: an answered roster with no backup marker anywhere (`web` mode, absent row, no role) keeps converging — suppression still requires a row that positively says `backup`. A failed read also falls back to the **persisted last authoritative answer** first (per-seat, in the engine's `SyncDb` meta — written only when the nest actually answered), so a transient `members.list` failure re-arms from what the nest last said; `Unresolved` is only ever a seat the nest has *never* answered for. A deferred (capped) pull is an **unfinished** pass: it must not stamp the device clean/caught-up (`mark_clean_pass_if_drained`).
3. **`engine_lifecycle::build_engine` was a third reader, and is one no longer.** It resolved by the retired set-mode-only key (`from_row_mode` on the set row), probe-proven to disagree with the role-aware readers about the same seat; it now installs only the cached-or-default starting position, and every drive path (`run_watch_loop` entry/tick, `run_one_shot_pass` entry) resolves through the one shared implementation before its first pull. Pinned by `config::resolve_device_mode`'s unit matrix and the tier_3 `conformance_sync_mode_role_flip.rs` (a live engine's `applies_remote_deletes()` follows a wizard role flip, both directions, no restart).

**The set-level `mode` fallback retires with the column — a seat is its place and nothing else (ruled 2026-09-28, the folders mode contraction design pass; built 2026-09-28).** `resolve_device_mode` loses its `row_mode` input: `SeatRead::Flags` resolves from the flags as today; `SeatRead::Absent` — both reads answered and the roster holds no row for this device — resolves to the **default point** `{originates, accepts, applies_deletes}` all true (`SyncMode::Sync`, cached like every answered read), which is exactly what the opinion-less arm resolves today and what the gesture that gave this device a local presence writes (below), so no seat changes behavior; a failed read keeps clause 2's cache-or-`Unresolved` rule. **`SeatRead::Unreadable` retired with the role contraction (2026-09-29):** `FolderMember.flags` is required, so a row that exists always resolves from its flags — there is no role string left to fail to parse, and a newer nest's future vocabulary rides the flags' rule-4 `extra`, which the reader ignores; `SeatRead` is `Absent | Flags`, and clause 2's "hold the anchor" serves `Unresolved` (a failed read with nothing cached) alone. `SyncMode` stays the engine's two-point *projection* of a place (`from_place_flags`, the both-flags rule above); `from_row_mode` and `from_member_role` go, and the cache keeps a two-value spelling of the engine's own — it is device-local, and "the vocabulary the nest row speaks" no longer exists. The daemon's TOML `SyncConfig` that once carried a `mode` key was deleted with the daemon (2026-10-02); a seat's mode is runtime state fed only by this resolution and its cache (a config file is not a configuration surface, `../principles.md` § One configuration surface). **A local presence writes the place it needs (ruled with it; its own build):** binding a location on any desktop, and turning on apple's or android's on-demand toggle for a folder this device holds no place in, ENROLS the device at the default point through `fauna.folders.places.set` — one shared implementation on the bind path, so the roster never again describes fewer seats than are syncing; the engine's `Absent` default covers the gap between the gesture and the write. **Built for every desktop bind (2026-09-28):** the agent's `SetLocationFolder` handling (`bins/fauna-sync-agent/src/pipe_server.rs`, `enrol_bound_place`) calls the shared `FoldersClient::ensure_place` (`libs/fauna-client-folders`) — a roster read, then `places.set` at `PlaceFlags::default_place()` iff this device holds no place; an existing place is never rewritten — best-effort, bounded, own-nest (`local:`) sets only, before the bind's engine reconcile; linux, tui and windows all bind through the agent, so none carries its own call, and tui re-reads the expanded row's roster when the bind confirms so the new seat paints. Built for apple (2026-09-29): FaunaKit's `FolderOnDemandToggle` turned ON calls the FFI face (`FfiFoldersClient::ensure_place`) with this device's sync device id before the domain reconcile re-plans, then re-reads the roster, and the toggle is offered on every owner row. **Unbuilt: the android on-demand-toggle leg** — the FFI face exists and android's toggle does not call it yet, so until it does a place-less android device syncs exactly as today, invisible in the roster.

*Implementation status today (what actually heals an unresolved seat, 2026-08-28): clause 2's closing invariant — ***`Unresolved` is only ever a seat the nest has never answered for*** — did not hold, in two independent ways, one of which was a live delete-propagation failure. Found by `test_filesync_seats.py::test_seats_converge`, red on all four `native`/`tui` cells and green on every headless `engine` cell; legs 1-4 pass throughout, because an unresolved seat suppresses **deletes only**, which is exactly why the red read as a delete-propagation bug for a whole session. **(1) The trigger gap — the live cause.** `always_resident::run_watch_loop` resolved the mode at loop entry and then only on the rescan tick. An in-process host reaches entry while its own control plane is still connecting, so both authoritative reads fail against it and a seat with nothing cached lands on `Unresolved` — `old=Resolved(Sync) new=Unresolved` logged a millisecond after `authenticated WS connect`, then `sync mode is unresolved; declining a peer's delete` per tombstone. The headless daemon resolves inside `run_ws_session`, *after* its session is up, which is the whole of the asymmetry — not two diverged implementations. In production the 300 s tick heals it; any window shorter than the tick never does. The loop now re-resolves **and pulls** on the control plane's `Connected` transition, on the arm that already existed for the share roster and already carried this rationale (*"the one the rescan tick can only catch by luck"*) — its `conn_rx` was subscribed only under `p2p-share`, parking the arm entirely on every other build, and is now unconditional. The pull is not optional: an unresolved seat **held** its anchor at the first declined tombstone precisely so it re-delivers once the role is readable, and the nudge that would have carried it has already been consumed and declined. **(2) The memory gap — real, but not this failure.** `config::resolve_device_mode`'s readable-but-opinion-less arm (`web` mode / absent row, roster answered `Absent`) resolved `Sync` correctly but persisted nothing, on the reading that a reply carrying no backup marker "is not an authoritative mode". It is one: both reads succeeded, and what the nest said was *no backup marker anywhere* — so a later degraded read fell to `Unresolved` for a seat the nest had answered for. It now caches that answer like every other answered read, which cannot disarm a backup seat (it is written only on a reply that positively carried no backup marker, and that same reply has already resolved this pass to `Sync` regardless of the cache). **Refined 2026-09-01:** that persist decision splits by seat-read shape — `SeatRead::Absent` (no row for this device at all) keeps caching per the mechanism above, but `SeatRead::Unreadable` (a row exists that this binary cannot parse) resolves `Sync` for the current pass and no longer persists it, since the unparsed row could be hiding a future-vocabulary `Backup` marker that would otherwise be permanently overwritten by the guess. This did **not** fix the cells: the demoting read there is the engine's *first*, so `old=Resolved(Sync)` is the constructor default and no successful read had ever run to write a cache — measured, by re-running the cells with (2) alone and watching them stay red. **Coverage.** tier_1 `config::resolve_device_mode_tests::a_no_opinion_answer_is_remembered_so_a_later_failed_read_never_unresolves` pins (2) over the production sequence, cache-fed, verified red first; `pull_remote_changes_test::a_re_resolved_seat_applies_the_tombstone_its_unresolved_self_held` pins the contract (1) depends on and that nothing pinned before — that the hold is *provisional*, so a re-resolved seat actually applies what its unresolved self declined (the sibling test only proved the anchor stays put). The `Connected`-arm **wiring** is pinned at tier_1 too, by `fauna_sync_engine::connected_arm_heal_test` (2026-08-28): a real `NestClient` over `fauna_client::testing`'s mocked socket reaches loop entry with nothing authoritative to answer it, so the seat lands `Unresolved` for the production reason, and the transition then has to re-resolve it — both authoritative reads back on the wire, `Resolved(Backup)` installed, distinguishable from both the constructor's `Sync` and the failure posture — **and** pull, with the rescan tick set an hour out so nothing but the arm can have done it. Each of the three production lines was verified red on its own mutation: the arm's `refresh_sync_mode`, its `pull_remote_changes`, and the `conn_rx` subscription (mutated to no subscription — the shape the retired gate produced on a non-`p2p-share` build; re-gating it on `cfg!` is *not* a testable mutation in this crate, whose own test build always unifies `p2p-share` on through its dev-dependencies, measured 2026-08-28). The two obstacles that had kept this at tier_3 were removed in the same change: the crate took a `fauna-client/test-util` dev-dependency (the mode reads go through `NestClient`, so its `wiremock` dev-dep could never serve them, and the runtime-free `fauna-client-testkit` has no connection state to transition), and `run_watch_loop` now takes an `Arc<SyncEngine>`, so a handle survives to read the healed mode back. Being shared, both fixes reach every app running `fauna-sync-engine` and the headless daemon alike.*

This is **only** about deletes arriving *from* the nest: a backup seat still records its own local deletes upward as tombstones. Upload-only constrains what it applies downward, not what it reports. The cross-nest foreign-set carve-out is unchanged: a foreign set has no row in this member's own projection, and keeps the delete-applying default.

### 5. Offline Catch-Up

When a device reconnects after being offline:

1. **Poll for missed changes** over the `fauna.sync.changes.list` WS-RPC kind
   (`folder`, `since=<last_seq>`) — the control-plane twin of the deleted
   `GET /api/v1/sync/changes`. Returns a list of changes since the device's
   last-seen sequence number, in order.

2. **Download missing chunks and manifests** over the kept bulk-binary byte
   routes:

   ```
   GET /api/v1/chunks/{hash}
   GET /api/v1/manifests/{hash}
   ```

3. **Apply changes in order** — the device applies each change in sequence-number order, then uploads any locally queued changes it made while offline.

**The anchor is the ordering mechanism, and it is load-bearing — there is no
second line of defence (stated 2026-07-31).** A device never re-sees a change it
has already applied, and that is enforced at the *source*, not by the applying
client: every `changes.list` arm answers `seq > since` in `seq` order (including
the cross-nest relay, which serves an ordered prefix page — it closes a page
early, never skips), and the client's anchor advances to each applied batch's
maximum `seq` and never regresses. The apply path deliberately carries **no
cross-batch sequence guard**: within one batch the highest-`seq` change per path
wins ([`delete-propagation.md`](delete-propagation.md) § Batch-latest, not
per-change), but across batches the engine trusts the anchor, and a row
records no `seq` to compare against.

**The accounting law (ruled 2026-08-06 — the ack-advance livelock):** the
anchor asserts *"every row ≤ it is applied or deliberately skipped"*, so **only
the accounted catch-up walk — own-echo tail included — may advance it;
single-row acknowledgments never do.** Until 2026-08-06 the `fauna-sync`
daemon's receive loop anchored on the data-plane `Ack` of its own `FileChanged`
— a leftover from the pre-clause-4 era when the pull rail deliberately excluded
own rows and the ack was the only anchor mechanism. Once the pull rail became
the echo channel ([conflicts.md](conflicts.md) clause 4), that advance
DE-LISTED the just-acked own row before any of its echo bookkeeping ran
(edit-frontier fold, in-flight retire, base advance), and every concurrent peer
row recorded just above it judged fast-forward and hit the causal defer on
every retry — a livelock only the rescan tick could break, reaching production
as a seat that silently ignores its peers for up to the rescan interval
(300 s) whenever its own record lands contiguously before theirs. Measured
live 2026-08-06 (`test_seats_converge[3seat-engine+engine+engine]`, 1-PASS/
2-FAIL on one tree — record-order roulette); refutation record:
`merge_convergence_test::daemon_ack_anchor_advance_past_own_row_wedges_refuted`.
The daemon's receive loop now takes no `SyncDb` at all, making a recv-path
anchor write structurally impossible; an `Ack`'d own row and a
real-time-applied peer row both re-list on the next pull, where the accounted
walk retires them (pre-pass + echo tail; rung-1 duplicate skip) and advances
the anchor.

**A transient failure must not be terminal (added 2026-08-01).** The anchor rule
above says a transient failure stops the pass with the anchor unmoved, so a later
pull retries from there — but *something must actually drive that later pull*.
For the headless daemon nothing did: its rescan tick ran no catch-up, a healthy
connection never reconnects, and the nudge only fires when a peer writes, so one
racing 404 left the device permanently behind on an otherwise healthy set. A
client whose catch-up can stop early therefore **owes a retry driver**: the
shared engine pulls on every rescan tick, and the daemon (removed 2026-10-02)
re-drove a pull that reported itself unfinished on its next rescan tick
(`PullPhase::Retry`), conditionally — still no standing nest poll. Re-pulling from an unmoved
anchor is safe for exactly the reason the nudge-vs-reconnect overlap is, below.

Two consequences a future change must preserve:

- **Any caller that hands the apply path a batch not drawn from the current
  anchor breaks a real invariant.** A change delivered out of order — in
  particular a delete whose `seq` predates a conflict resolution that already
  committed a local winner as head — would be applied, destroying content the
  resolution had just declared the head. Ordering is what makes that
  unrepresentable; *serializing* pulls is a weaker property and is not
  sufficient on its own.
- **Concurrency between the real-time nudge and the reconnect catch-up is
  safe by construction**, because both draw their batch from the anchor and the
  nest decides its contents: either both batches predate the advance and carry
  the superseded change together with its successor (where the within-batch rule
  applies), or the later fetch begins above the advance and excludes it.

> **Implementation status (2026-06-08).** The shared `fauna-sync-engine`
> implements this — `SyncEngine::pull_remote_changes` polls
> `fauna.sync.changes.list` since the anchor and applies in order — and the
> **Linux** in-process client drives it each rescan tick. The **headless
> `bins/fauna-sync` daemon** does too, as of **tracks A3 + B3(0) (2026-06-08)**
> — but only after a three-step fix. The Track-C HTTP control-plane rip first
> removed the legacy `run_http_mode` (which had driven catch-up through the
> engine), leaving the daemon's sole run mode forwarding real-time changes over
> `/sync/ws` with no replay of changes missed while disconnected. Until track A2 (also
> 2026-06-08) the daemon *masked* this gap: its `reconcile_ws` re-sent a
> `FileChanged` for **every** watched file on each rescan tick, so a destination
> that reconnected received the changes it missed via the source's re-blast
> (which the orchestrator re-forwarded) rather than a real catch-up — at the cost
> of a duplicate `sync_changes` row per file per tick. A2 made the daemon record
> each change **once** (one change = one record — § Technical Flow), removing the
> re-blast; `test_ws_reconnection_replay` pinned the real gap. **Tracks A3 + B3(0)
> (2026-06-08) closed it for the daemon.** A3 made the daemon push chunks to the
> nest blob store in **both** modes (push-always) with the `ChunkResolver`
> decoding blob-store bytes before forwarding, so the nest is a resilient chunk
> source a reconnecting device can `GET` from regardless of whether the original
> source is still online. B3(0) then wired the catch-up itself: on every
> `run_ws_session` (re)connect the daemon pulls `fauna.sync.changes.list` since
> its local anchor (excluding its own device), downloads each missed change's
> manifest + chunks from the kept bulk-binary byte routes, and applies them via
> the shared apply path — run **inline before the receive task**, so the
> `tokio::select!` cancellation trap fixed for `test_file_sync` cannot drop a
> mid-apply. `test_ws_reconnection_replay` is green. The **concurrent offline-edit**
> case (task B3 parts 1/2, 2026-06-08) is resolved **as a conflict, not a silent
> merge** ([`conflicts.md`](conflicts.md)): when a reconnecting daemon's catch-up finds a missed
> change whose content *and* the device's own local file have both diverged from
> the cached merge base, it records a conflict with both candidate versions for
> the user to choose between, rather than a silent 3-way merge it cannot do safely
> (it keeps only a single per-path base, not per-version history, so a silent merge
> can drop a side's edit). Real-time forwards (both peers online) keep their
> best-effort merge. See [`conflicts.md`](conflicts.md) and `test_text_merge.py`.
>
> **A failed change must not strand the device — the two failure classes do
> OPPOSITE things (ratified 2026-07-31).** Catch-up applies in sequence order
> behind a single scalar anchor, so what a failure does to that anchor decides
> whether one bad change can block every later one forever:
>
> - **Transient** — the nest is unreachable, or a change's manifest/chunks are
>   not fetchable yet. Every later change would fail identically, so the pass
>   **stops** with the anchor unmoved and the next reconnect retries from there.
>   Advancing past a network outage would skip the whole tail.
> - **Permanent for that change** — a manifest too new to read
>   (`check_min_reader`), a chunk sealed to a key this device does not hold,
>   content that does not address its recorded hash, a `path_hash`-only legacy
>   record whose relative path cannot be reconstructed. Later changes are
>   unaffected, so the change is **recorded** (a `catchup_failed` conflict row)
>   and **skipped**, and the anchor moves past it.
>
> The earlier rule — *any* failed apply leaves the anchor unmoved and is retried
> next reconnect — is **retired**: it made a permanently un-appliable change a
> permanent block. Measured live 2026-07-31 against `example.com`: one change
> recorded 2026-07-18 could not apply, so the anchor never passed it and the
> device pulled **zero** of the changes after it, on three consecutive runs with
> three fresh device ids. That also breaks the version-compatibility rule that a
> client may be older than its peers (`../architecture/version-compatibility.md`):
> one file written by a newer client would otherwise freeze an older device's
> catch-up for good. Skipping is not silent — the recorded row surfaces in the
> conflict review list (how it gets there: [`conflicts.md`](conflicts.md) § Skipped
> catch-up changes), and a later change to the same path applies normally.
>
> **TRANSIENT IS THE DEFAULT, and permanent is marked at the site (2026-09-20).** The two classes above are the rule; *which class an arbitrary failure falls in* is the part the appliers got wrong. The shared engine's apply loop treated **every** failed download as neither: it logged, `continue`d, and let the end-of-pass `set_anchor` advance past the row — so one blip mid-fetch dropped a peer's change on that device for good, and (compounding it) every later local edit stamped `derived_through` at an anchor that now claimed the unread row, licensing peers to fast-forward over their own unread work ([conflicts.md](conflicts.md) § the causal watermark). The classification is therefore explicit and asymmetric: an unclassified failure is **transient**, and only a site that knows its failure recurs identically on every later pull marks it permanent (`fauna_core::apply_failure::PermanentApplyFailure`, carried on the error and read by both appliers). Guessing transient for a permanent failure costs a retry loop that the list above's incident shows is loud and recoverable; guessing permanent for a transient one silently destroys a change. The marked set is exactly the permanent list above: a manifest past `check_min_reader`, content that does not address its recorded hash, a path outside the sync root, and sealed content reaching a reader holding **no** key material at all. "Does not address its recorded hash" covers two chunk-stage failures as well as the whole-file mismatch: a manifest whose chunk list disagrees in count with the chunks it stores (a property of the manifest, not the fetch), and — on the headless daemon — a sealed chunk that fails its AEAD tag under the daemon's one convergent root, which is derived from the identity seed so no later pull brings another; the chunk key is a function of that root and the recorded plaintext hash, so a tag failure means the stored bytes are not this chunk. Left unmarked, a writer co-member's row of random chunk bytes (content-addressed, so the nest stores them) held the daemon's catch-up below it forever. That premise is true of the nest's bytes and false of a local cache, so **a local cache entry is checked against its content name before any content failure is classed; a mismatching entry is a miss (evict, refetch), never a permanent mark** — and bytes that do not address their name are never cached, and a cache write is atomic, so a torn write cannot rest under the full name. The one content mismatch that reaches the mark is therefore the nest's own bytes on a fresh fetch: a fetched chunk that does not address its store key is marked, while a fetched manifest that does not address its hash stays transient, as the shared download classes it (`fauna-sync` `ws_client.rs::{cache_put,cache_get,fetch_change_into_cache}`). The engine's `BlobFetcher` holds no cache, so this exposure is the daemon's alone. A *path label* that no held root opens is a different class and stays transient ([`path-sealing.md`](path-sealing.md) § The transient arm is NARROWED): a sealed-name row reaches the chunk stage only after its label opened under that same single root, so the writer demonstrably held it, and this is not "a generation this holder will never hold". "Outside the sync root" is a property of the row against the tree — its path is lexically unsafe, or a symlinked intermediate directory redirects it — and never of the root being **absent**: a root that does not resolve (an unmounted drive, a directory renamed away) refuses the write but stays transient, since the same change applies once the root is back and marking it permanent would skip every change a pull delivers meanwhile. Both appliers resolve every write door's target through one shared function (`fauna_core::path_guard::contained_apply_target`) so the two cases cannot fold together on either host.
>
> **"A chunk sealed to a key this device does not hold" splits (same ruling).** The permanent list's fourth entry predates the M2 generations, and read whole it now contradicts [`path-sealing.md`](path-sealing.md) § Apply-path degrade ruling, which classes an unopenable *label* transient because key material lags its changes. Both are right about different holders: a reader with **no `BackupKey` and no content keys** never acquires an owner key by syncing, so waiting for it is waiting for nothing — permanent, as listed. A reader that simply **lacks generation *v*** may be granted it on the next pull — transient, so the anchor waits, and it stays transient even for a removed member, because a failed open cannot tell "removed" from "not synced yet". The code splits at exactly that seam (`FileDownloadKeys::content_open_roots` vs. the fail-closed no-key-material arm below it). Two marks belong to the writer-signed reader bounds, whose rulings own them ([`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)): an unstamped row signed as a retired identity that opens under none of that identity's roots (`SIGNER_BOUND`, ruling (8)(c)), and a STAMPED row reaching an **owner-only** reader that holds none of its generation (`STAMP_BOUND`, ruling (10)(c) — a stamped record is never offered an owner root). The second is not the transient "lacks generation *v*" above: an owner-only reader's generations are the retired ones carried by the very custody that made its set owner-only, so no pull brings the missing one, and a hold would let one forged stamp stall every later row.
>
> **Catch-up runs on every rescan tick.** The shared engine's resident loop
> pulls `fauna.sync.changes.list` each tick (`always_resident::run_watch_loop`),
> as well as on the remote-change nudge. *(The removed daemon instead relied on
> its data-plane socket's reconnect to trigger catch-up; both left the tree on
> 2026-10-02.)*

## Folders

> A `public`-audience folder is additionally readable by **followers** — any Fauna user,
> browse-on-demand, no location binding; the whole contract lives in
> [`folders.md`](folders.md) § Publicly-synced follow (kinds: `../architecture/federation.md`
> § The public folder read plane).

A **folder** is a named collection of files synced together as a unit. Folders are owned by an actor and can span multiple devices.

| Operation                              | WS-RPC kind                      | Deleted HTTP twin (≡)                     |
|----------------------------------------|----------------------------------|-------------------------------------------|
| Create a folder                      | `fauna.folders.create`         | `POST /api/v1/file-sets`                  |
| List folders                         | `fauna.folders.list`           | `GET /api/v1/file-sets`                   |
| Add a device to a folder (or change its place) | `fauna.folders.places.set` | — (WS-RPC only; `members.add` retired with the role contraction) |
| Remove a device from a folder        | `fauna.folders.members.remove` | `DELETE /api/v1/file-sets/{name}/members` |
| Acquire an exclusive edit lease        | `fauna.folders.lease.acquire`  | `POST /api/v1/file-sets/{name}/lease`     |

**Exclusive leases** allow one device at a time to hold write access to a folder. While a lease is held, other devices treat the folder as read-only. This is useful for files that cannot safely be merged (e.g. SQLite databases, binary assets). Leases expire automatically after a configured TTL. ⚠ The second sentence holds for a folder whose owner turned **exclusive editing** on, and only there: the mechanism is § Exclusive editing immediately below (the per-folder opt-in, the lease window an upload pass writes inside, the read a seat learns the state from). A folder that never opted in takes no lease and is never read-only — which is what keeps the ordinary case free of a nest round-trip per write. What is built is § Status, *The exclusive lease: nest half + wire + engine + two-seat witness BUILT; app surface on tui only*.

### Exclusive editing — the lease's client half (design ratified 2026-09-21; nest + wire + engine built, app surface on tui, trickle-down owed)

The paragraph above is the promise; this is the mechanism that keeps it, settled 2026-09-21 while closing the outcome that witnessed the nest half. The mechanism below is built from the nest down to the sync engine; only the app surface is still target state — see this section's closing implementation note and § Status.

**Lease governance is a per-folder choice, not a per-write reflex.** A folder carries an **exclusive editing** property, chosen by its owner in the app UI and persisted in nest state: off (today's behaviour, and the default), or on — *one device at a time may write to this folder*. Only a folder whose owner turned it on takes a lease at all. The alternative shape — acquiring on every upload everywhere — is **refused**: it puts a nest round-trip in front of every write on every folder, to buy nothing for the ordinary case the auto-resolve model already handles well ([conflicts.md](conflicts.md)).

**It is its own property and NOT a third conflict-policy value**, though both live on the folder row and a future reader will be tempted to fold them. They are different axes: `folder-conflict-policy-select` (`Auto` / `Latest-wins-always`) decides what happens **after** a divergence, and exclusive editing tries to stop one **before** it. Folding them would also strand the cases a lease cannot cover — an offline edit, a lease that expired mid-write, a member who never took one — which still need a resolution policy of their own. A lease-governed folder therefore has BOTH properties, and both are meaningful.

**The acquire is per upload pass, never per file.** On a lease-governed folder the engine acquires once before the first upload of a flush, renews while the flush runs (the nest treats a re-acquire by the same device as a renewal, so renewing costs one kind and no new state), and releases when the pass drains. Whatever a pass covers is one lease window. A per-file acquire is the shape this section's *Never* list forbids.

**A seat learns a folder is leased by READING, not by asking.** `fauna.folders.lease.acquire` is not a probe and must never be used as one, for two independent reasons: it **takes** a free lease as a side effect of asking (its DAO insert is conditional on no other holder, not on a question), and it is gated on the writable-folder resolver, so a **reader** member — the seat that most needs to know a folder is locked — cannot ask at all. Instead the lease's live state rides the folder read projection every client already polls: `FolderSummary.lease`, additive, `None` when the folder is unleased or the nest is older than the field, otherwise the holding device and the lease's expiry. It rides **both** projection arms, owner and member, for the same reason residency does — a member seat writes too, so a member must see it. **The holding device is named only to the account that holds the lease**: a device id is client-asserted and means nothing to another account, so every other reader gets the lease and its expiry with an empty `device_id`, which a client renders as *another device is editing*.

**The refusal is typed; the holder comes from the projection.** A second device's acquire is refused `fauna.folders.conflict`, which carries no payload — the detail string beside it is free-form prose for a log, and **no client may parse it for the holder's identity**. A client that needs to name the holder re-reads the projection, which owns that fact. Keeping one owner for it is what stops the holder's device id becoming a string format two code paths disagree about.

**An offline seat edits freely and uploads later.** `fauna.folders.lease.acquire` is classed online-only ([transport.md](../architecture/transport.md) § offline classification), so an offline seat on a lease-governed folder cannot hold one — and is **not** thereby made read-only. It records the user's edit locally exactly as on any other folder; the upload waits for the reconnect that can acquire. If the acquire then fails because another device holds the lease, the local edit still stands: it stays on disk, unsent, and the folder renders read-only-with-pending-changes until the lease frees. **No path here may drop, overwrite or silently defer-to-oblivion a local edit** — a lease is a coordination convenience, and user data outranks it.

**The flag fails OPEN, to un-governed.** An absent or unparseable `exclusive_editing` reads as *off*, and the reading has one owner so no consumer re-derives it. Note this is the opposite direction from § Content residency's fail-closed-to-full, and deliberately so: there, failing the wrong way would rest bytes the owner asked us not to rest; here, failing the wrong way would **freeze a user's own folder** against their own writes because a field did not parse. Neither is a security boundary against a determined writer — a lease coordinates cooperating devices and stops nothing a client holding the folder's keys could not do anyway — so the tie breaks toward the behaviour that keeps the user working. **On a multi-writer set the holder may be another account's device**, and a member who keeps renewing defers the owner's upload passes for as long as they do (nothing is lost; a deferred pass leaves every entry locally modified); the lease is therefore bound to the acting account so a member cannot impersonate or evict the owner's device, and the owner's remedy for a member holding the folder is turning exclusive editing off.

**What the apps render** is owned by [`../ui/folders.md`](../ui/folders.md) § Exclusive editing: the per-folder toggle, and a lease-governed folder's read-only state naming the device that holds it.

*Implementation status (2026-09-22).* **Built:** the nest half (`fauna.folders.lease.{acquire,release}`, one holder at a time, same-device renewal, a 300 s TTL takeover and holder-scoped release — `device_id` required since 2026-09-24, when the compat-remnant sweep retired the device-id-less owner clear (`../architecture/compat-remnant-sweep.md` § The borderline items, item 6) — pinned by `tests/e2e-unified/tests/api/test_folder_exclusive_lease.py`); the `exclusive_editing` property at rest and on both wire directions plus `FolderSummary.lease`, on both projection arms, pinned by `bins/fauna-nest/tests/conformance_folder_exclusive_editing.rs`; and the engine half — `libs/fauna-sync-engine`'s `folder_lease` module installs the governance flag and the holder reading off the same folder-list read as every other per-folder posture, takes one lease per upload pass, renews at half the TTL, releases when the pass drains, defers (never drops) a pass it could not get the lease for, and keeps the offline arm distinct from the refused one so an unreachable nest never renders a folder read-only. The two-seat tier_3 witness through hold → refusal → release → takeover is built too (`bins/fauna-nest/tests/conformance_folder_lease_two_seats.rs`). **Owed:** the app surface on the six apps after tui, which rendered it first on 2026-09-27 (`../ui/folders.md` § Exclusive editing). § Status carries the same reading for the whole feature. **The one limitation it once named is gone:** the legacy headless daemon (`bins/fauna-sync`) ran its own uploader rather than the shared engine, so it neither took nor honoured a lease; it was removed 2026-10-02 ([`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Headless deployment — the headless story is fauna-tui plus the per-user agent), so every folder seat now uploads through the shared engine and the lease reaches all of them.

### Membership — which table records a file, and where it surfaces

A recorded file change (`fauna.sync.changes.record`, or a data-plane `FileChanged`) lands in a projection chosen by the folder's kind of custody, and that choice decides which read surfaces see it. A folder has no type, so there are only three cases:

| Folder | Membership projection | Plaintext `path` stored? | Surfaces in Media (`fauna.media.list`)? |
|---|---|---|---|
| **Every ordinary folder** | `sync_changes` (append-only device-sync feed; `record_sync_change_metered`) — the **head unification** (folders re-model phase 3, 2026-08-17): records are device-attributed, get real seqs, are pullable by accepting seats, and supersede via `fauna.sync.changes.supersede` | yes | **yes** — the canonical media-library case (`get_files_for_folder`) |
| **A reserved (`__*`) destination-custody folder** | `backup_custody` (latest-per-path; `upsert_backup_custody`/`tombstone_backup_custody`, `seq: 0`, never the feed) — the destination-side custody machine, not a recording mode. The seam is the nest-internal `folders.custody_copy` flag, read through `is_reserved_custody_copy`, the ONE named predicate every routing site calls | yes | no — reserved `__*` sets are never Media |
| **A website-enabled folder** (any folder whose `website_enabled` toggle is on) | the head feed, exactly like every folder, **plus** a `web_files` projection (`route_web_file_change`, keyed on `folder.website_enabled` — owner [`web-content-hosting.md`](web-content-hosting.md) § Content model; never *instead of* the head row) | yes | **yes** — a website-published folder browses like any other |

(Authority for *which sets are media* is `../ui/media.md` § State & data shape; this table is the sync-side companion — the recording-table mapping it depends on.)

**Head unification — BUILT 2026-08-17** (folders re-model phase 3; concept owner [`folders.md`](folders.md) § Target re-model, which states the *one write plane* claim — the table above already reads unified). An ordinary (non-reserved) Backup folder records to the head feed like every other folder, so Media, snapshots and the snapshot watermark (`max_change_seq` alone — `snapshots.max_custody_updated_at` is dropped, schema v39) read one plane. Reserved destination custody (the `__*` rails) is untouched — a different machine on the destination side, not a recording mode. Historical routing for the record: ordinary Backup sets recorded to `backup_custody` from 2026-06-15 to the v39 flip; their legacy custody rows were **deleted, not re-seeded** (approved alpha deletion — the ⚠ paragraph below is why).

⚠ **The re-seed constraint found 2026-08-17 — RESOLVED the same day by user ruling: there is no re-seed at all.** A custody→head re-seed cannot attribute its synthetic rows to a device (`backup_custody` records an `uploader_actor` and **no device column**), and a NULL-device head row passes **both** echo filters — the nest serves `(device_id IS NULL OR device_id != ?3)` (`bins/fauna-nest/src/db/sync_storage.rs:1983`) and the client's `is_self_echo` needs an exact match (`libs/fauna-sync-engine/src/engine.rs:4002`) — while every seat's anchor for a Backup folder is 0 (the custody branch returns `seq: 0` without assigning one, `bins/fauna-nest/src/sync_handlers.rs:1567`). A re-seed would therefore have delivered every Backup folder's entire back-catalogue to every seat, the uploading device included. The ruling (2026-08-17, phase 3's opening decision) was a one-time, pre-user reset: **every remaining data-preserving migration step in the re-model plan is deleted** — phase 3's schema step deletes the ordinary-set `backup_custody` rows instead of re-seeding them (approved alpha deletion; scope recorded at [`folders.md`](folders.md) § Implementation status today). The mechanism facts stand for anyone who ever synthesizes head rows again: **a head record without device attribution is delivered to and applied by every seat** — synthetic rows must carry a device or be excluded from delivery.

### Content residency — nest-dehydrated sets (direction ratified 2026-08-10, user directive; v1 design ratified 2026-08-19, folders re-model phase 5; built 2026-08-20 — status paragraph at this section's end)

A folder gains a per-set **content residency** choice, made by the set owner in the app UI and persisted in nest state (`account-data-plane.md` R10 (account-data-plane.md § The ratified decisions) owns the per-replica-hydration decision this realizes):

- **Full (default — today's behavior):** the nest holds metadata *and* content chunks; offline time-shifting of content works (a destination can catch up from the nest with every source offline).
- **Metadata-only ("content stays on my devices"):** the change feed, manifests, and snapshots sync app→nest→app exactly as today, but **chunk bytes never rest on the nest** — the `ChunkResolver` relays a chunk from a holding seat to a requester transiently (§ Relay serving — the ask goes to a seat that announced the folder) **without writing it to the blob store**, and seats additionally fetch content directly seat↔seat over the account plane's peer leg once it lands (`account-sync-plane.md` § The peer leg — the same want-list chunk pull; until then, nest relay is the only content path). It is the **nest place's own content property** — orthogonal to member roles, to audience, and (while the column survives) to `mode`: the nest place stays *live* (it always holds the head — which is why the place model deliberately has no `live` field), and residency varies only what the nest's own replica physically retains.

**The v1 model (phase 5 design pass, 2026-08-19):**

- **At rest:** additive `folders.nest_content_residency` TEXT column — `NULL` = full (the correct resting value for every existing row; no backfill), `'metadata_only'` the one non-default value. `MIN_READER_SCHEMA_VERSION` unmoved. Reserved `__*` rails refuse it (the destination custody machine has its own contract), as does any folder with a serving toggle on (below).
- **Wire, all additive:** `FolderUpdateRequest.residency: Option<String>` — `None` = unchanged; deliberately its **own field**, never folded into the sent-whole `nest_place` policy record, where an older writer's policy edit would silently clear it. `FolderSummary.residency` rides **both** projection arms (member seats upload bytes too, so members must see it), absent = full; the fail-closed reading is *full* — nothing unparseable may ever stop bytes resting; only an explicit, parsed `metadata_only` may. `fauna_protocol::folders::FolderSummary::is_metadata_only` owns that reading, so no consumer re-derives the fail-closed direction.
- **Enforcement — bytes never rest, three gates:** (1) a residency-aware seat **skips uploading chunk bytes** for a metadata-only folder — the flag rides the same `SeatResolution` refresh the audience posture rides, live within one tick; (2) the nest's chunk-write rails **accept-and-discard** for a metadata-only folder, so an older seat that never heard of residency keeps functioning (its metadata records normally; its byte upload is simply not stored) — never an error a released client would surface as sync failure; (3) the relay path serves a pulled chunk **without caching it** (the `ChunkResolver` store-write is skipped). Reads need no new plane: every consumer already fetches by hash, and a store miss falls back to the device relay — metadata bootstraps always; bytes need a live holding seat, stated in the opting UI. **Whom the fallback serves (corrected 2026-10-01 — this bullet named a cross-nest member and a follower among them, and it serves neither):** a caller the home nest can name as the folder's owner or a member on its roster, which today means a session bearer on that nest. A member whose account lives on another nest holds none; its path is § Relay serving → *A member on another nest*. A follower of a `public` folder is no member, and no path is made for one: a `public` folder is never metadata-only (the fifth refusal of the *v1 scope cuts* bullet below, ruled 2026-10-01).
- **The flip is consent-gated — v1 has NO custody-inferred eviction.** Flipping full → metadata-only arms an explicit owner confirm that names exactly what happens: *the nest's copy of this folder's content is deleted now; your devices become the only holders; content moves between devices only while one holding it is online; if your devices lose it, the nest cannot restore it.* On confirm the nest drops its chunk bytes for the folder — R10's **consent arm**, applied at every seat count. (The 2026-08-10 text made the confirmed-holder predicate the multi-seat default with the confirm as single-seat fallback; that ordering is **superseded by A7** — `../architecture/message-segment-store.md` § Nest dehydration, ruled 2026-08-11: "confirmed custody elsewhere" requires the signed custody-receipts contract, and pre-receipts custody/anchor state is exactly the un-attested evidence A7 rules out. So v1 trusts only the owner's explicit consent; the receipts-verified graceful flip — no alarming confirm once N-of-M receipts prove live holders — arrives with T18 and *softens the confirm* rather than changing the model.)
- **v1 scope cuts (pairwise refusals, both directions, each refutable):** metadata-only ⊕ `website_enabled`, ⊕ `webdav_enabled`, ⊕ paywall — each serving surface reads bytes off the nest store, and a site or DAV mount that is up only while the owner's laptop is on is a broken serving promise; refuse whichever side moves second (the phase-4 cross-toggle pattern). **Snapshots stay allowed** (the ratified claim stands): they are manifest-level pointer rows, complete in metadata; restoring *bytes* from one needs a live holder, stated in the opting UI. Ordinary-folder destination places don't exist yet (their design pass is owned elsewhere) — no interaction in v1. **A fourth refusal, temporary (ruled 2026-10-01, built 2026-10-04):** metadata-only ⊕ a member whose account lives on another nest, both directions, until the cross-nest leg of relay serving is built — § Relay serving → *A member on another nest* owns it and its lifting. **A fifth refusal (ruled 2026-10-01, unbuilt): metadata-only ⊕ `public` audience, both directions** — flipping a `public` folder to metadata-only, and making a metadata-only folder `public`, each read off the effective state like the serving pairs, so the two cannot ride one request in either order. A folder can be born `public` but not born metadata-only, so create needs no check. A `public` folder is a serving surface in all but name: anyone may follow it, and a follower reads its bytes off the nest store by hash, with no credential ([`folders.md`](folders.md) § Publicly-synced follow owns that read). Bytes that are up only while a device of the owner's is online are the same broken promise the three serving pairs refuse. Giving a follower the relay instead is ruled out on its own grounds, not deferred: the public read names no caller, so the home nest has nobody to authorize an ask for; an anonymous request would make the nest ask the owner's devices for a hash — on every read, since gate 3 never caches — which turns a folder's popularity into load on the owner's phone and tells any stranger when the owner's devices are online; and the privacy this residency buys has no object there, since the owner's own confirm has already published the content. The refusal sits in the one writer of both fields, typed like the serving pairs, its sentence naming the repair; the public read gate stays audience-only ([`../architecture/federation.md`](../architecture/federation.md) § The public folder read plane owns it). A pair that already exists is left as it is — nothing is flipped back — and can only be left: re-asserting either side is refused with the same sentence.
- **GC:** a metadata-only folder's head/snapshot rows reference hashes the store never held — the chunk-GC oracle treats that as expected absence, never an error; the accept-and-discard rail must not leak partial writes.

Three consequences, stated in the opting UI rather than discovered:

1. **Content availability needs a live holder.** Metadata (a rename, a delete, a new file's existence) time-shifts through the nest as always; the *bytes* of a metadata-only set transfer only while some seat holding them is online. A second device syncing a metadata-only set materializes placeholders first and hydrates opportunistically ([on-demand-files.md](on-demand-files.md) placeholders are exactly this shape).
2. **Flipping full → metadata-only deletes the nest copy under explicit consent** (the v1 bullet above) — the confirm is the gate at every seat count until custody receipts land; the nest copy *was* the redundancy, and the user says so, not the code.
3. **Flipping back re-hydrates opportunistically** from whichever holding seats come online — the nest resumes storing (seats' uploads resume; relayed chunks may re-cache); content addressing re-converges, nothing re-records.

The privacy payoff is real and is the point: a metadata-only set's content never touches the nest's disk in any form — stronger than sealed custody, in exchange for the availability cost above. UI surface: `folder-nest-residency-select` on the nest-place editor block plus `folder-residency-confirm` (arm-then-confirm — the audience-confirm pattern: the select keeps painting the current value until the confirm is answered); the rule-A ask was granted 2026-08-20.

*Implementation status (phase 5; nest half landed 2026-08-20, relay read path + UI landed 2026-08-20).* Built: the additive `folders.nest_content_residency` column (v45), `FolderUpdateRequest.residency` / `FolderSummary.residency` on both projection arms (fail-closed to full), the reserved-rail + pairwise serving refusals both directions and same-request orders (conformance-pinned), the **flip-time chunk drop** (`backup/gc.rs::drop_folder_chunk_bytes` — spawned on the consent commit; walks the folder's own manifests through the single reachability oracle, so dedup-shared chunks and every manifest survive), and the **GC residency arm** (a metadata-only folder's manifests stay pinned but are never chunk-walked, so an old seat's uploaded bytes reclaim on the ordinary GC cadence). The **seat-side byte-upload skip** (`SeatResolution.metadata_only_residency` off the same list read as the audience, fail-closed to full; armed at engine build + per refresh tick + the daemon's install; gates at `upload_sealed`, the streaming path, the drain, and the daemon's `push_cached_file_to_nest`). The **transient relay read path**: `ChunkResolver::relay_for_actor_folder(hash, owner, folder, RelayCache::{Store,Transient})` — the store-miss arm of `GET /api/v1/chunks/{hash}` takes an additive `?folder=` hint (`fauna_nest_http::paths::chunk_store::FOLDER_HINT_PARAM`), resolves the caller's bearer → actor and the hint → an owner-or-member folder row (`folder_authz::resolve_readable_folder`, so it can never probe a stranger's seats), and relays the chunk from one of that owner's live seats, serving `Transient` (no store write) for a metadata-only folder and `Store` (the historical re-hydration cache) for a full one; the engine's `SyncClient` carries the hint (`set_folder_hint`, set from `SyncEngine::new`'s folder and the daemon's config). ⚠ **Which seat the relay asks was a coin flip until 2026-09-02, and for a metadata-only folder the relay is the only content path there is.** The resolver took the FIRST match from an unordered `HashMap` iteration over the owner's seats in the folder and stopped. But the seat *asking* for a chunk is registered exactly like the seat holding it — same owner, same folder — so that set contains the requester, and picking the requester cannot work twice over: it does not hold the bytes, and it is blocked on its own in-flight download, so it answers nothing until the fetch deadline. Measured with two real daemons: the seat that had never held the file was handed its own `FetchChunk`, logged `not found in cache` only 30 s later, and the second attempt 404'd — while the same test had passed minutes earlier, the two runs differing in nothing but iteration order. The seat lookup then returned them all and the relay tries each until one answers with bytes; a seat that answers nothing, or bytes that do not hash to the requested address, is passed over rather than taken as the answer, so one bad seat cannot deny a file. The cache decision is still taken against the seat that actually answered — attribution is that seat's assertion about its own cache and does not travel between seats. Order stays arbitrary — the caller tries until one answers, so order is latency, not correctness — and excluding the requester by identity would need the device on the by-hash rail, a wire question (`TokenStore::validate` yields an actor and nothing else; the `minted_by_device` it holds is the renewal key, not the sync `device_id` the registry is keyed by). ⚠ **The residue was recorded as "one fetch deadline" until 2026-09-02, and that undercounted it twice over**. The walk was serial, so the cost was one deadline per *silent seat*, not one per read — and `N` is bounded by nothing: the registry is a plain map with no cap, fed by an unthrottled `fauna.sync.register`, behind a route carrying no rate limit. It was also paid **per chunk**: the engine issues one GET each with no single-flight, and the map's iteration order is fixed for the process's life, so the same silent seats were asked ahead of the holder on every chunk of the file and the penalty never averaged out. What bounds it now is a **bounded-concurrency race**: the relay asks `RELAY_FETCH_CONCURRENCY` seats at a time and takes the first answer that survives `verify_fetched`, capped at `RELAY_MAX_CANDIDATE_SEATS` candidates in total, so a read costs one deadline per window rather than one per seat. A window and not a fan-out on purpose — the engine already pulls chunks K-parallel, so asking every seat at once would multiply *seat-side* `FetchChunk` load by K × N. The cache decision is unchanged by the race: `attributed` is still read off the seat that actually answered. Pinned by `chunk_relay.rs::{a_relay_passes_over_a_seat_that_holds_nothing_and_asks_the_next,a_relay_read_is_bounded_by_the_window_not_the_seat_count,a_relay_read_asks_no_more_seats_than_the_candidate_cap,a_losing_racer_leaves_no_pending_entry_behind}` — each red-verified against its own mutation (serial walk, cap not applied, sweep removed). ⚠ The concurrency pin's threshold is a literal, **not** `RELAY_FETCH_CONCURRENCY`: its first draft asserted `asked >= RELAY_FETCH_CONCURRENCY`, making the constant both the subject and the bar, so setting it to `1` moved the bar down with it and the pin passed against the serial walk — the same vacuity this section records for the sync package's rescan pins, one layer down. A `const _: () = assert!(RELAY_FETCH_CONCURRENCY > 1)` now guards the constant itself, because that guard cannot live in the test. The **UI** (tui lead): `folder-nest-residency-select` (Full / Metadata-only) + the consent-gated `folder-residency-confirm` on the nest-place editor block, over `FoldersClient::set_residency`. Pinned by `bins/fauna-nest/tests/sync_relay_serving.rs::the_route_relays_only_a_hinted_miss_of_a_readable_folder` (route arms), `libs/fauna-sync-engine` `nest_client` (the hint on the wire), and the tier_3 `bins/fauna-nest/tests/conformance_relay_serving_two_engines.rs` (two real engines: the writing seat skips the upload → the store holds no chunk of the file, named by its store keys → the second seat hydrates by relay, still no chunk resting). ⚠ **The pin it replaced — two legacy daemons, deleted with the daemon's tests — asserted nothing about residency until 2026-09-02**, and the reason generalizes to any future one: it weighed the WHOLE blob root, which is not the folder plane's — the nest rests its own SQLite hot-copy and logical dump in the same `hh/hh/<hex>` fanout, both on the scheduler's immediate first tick ([`backup-restore.md`](backup-restore.md) § 11). So its "the store became non-empty, therefore the manifest arrived" arm was already true before any seat connected, and its byte ceiling was exceeded ~13x by a fresh nest with nothing in it. Corrected the same day by NAMING both sides instead of summing: the manifest is identified by the hash `fauna.sync.changes.list` carries (the daemon pushes it before it records the change, so the feed is also the causal barrier to wait on), and the store reading subtracts every hash the nest named in its own `backups/*.manifest.json` sidecars. A new residency assertion copies that shape rather than totalling the root. ⚠ One honest deviation, refutable: **the "accept-and-discard" rail gate is realized as accept-and-*reclaim*** — the by-hash chunk rails carry no folder attribution, so an old (pre-residency) seat's bytes rest transiently until the flip-drop/GC pass rather than being discarded at the rail. ⚠ **A second honest deviation, same root cause — the relay read path's cache gate (fixed 2026-08-21).** The route authorizes on a folder and fetches on a hash, and nothing between attributes the chunk to the folder: `NodeMessage::FetchChunk` carries no folder, and the seat answers from a chunk cache that carries no folder attribution of its own — `bins/fauna-sync`'s `chunk_cache_dir` is a flat, content-addressed directory, defaulting to `{config_dir}/chunk_cache`, while one daemon syncs one folder. (Measured 2026-08-21, correcting an earlier “per-user” reading of this sentence: the directory was per-**config-dir**, so it was shared exactly when two daemons' config files resolved to one directory — a hazard `bins/fauna-sync/src/main.rs` had documented and forbidden for months with nothing enforcing it. **It is per-`(device, folder)` since the fix described below** — the sharing is now unrepresentable rather than forbidden.) Cache policy therefore could not be read from the *hinted* folder at all: a hint names a folder the caller may read, never the folder a content-addressed chunk belongs to, so under a `full` folder's hint the `Store` arm could rest a chunk of that same owner's `metadata_only` folder — against the promise the owner was given a UI switch for. **The gate now refuses to cache what it cannot attribute**: a relayed chunk is stored only when the owner has *no* metadata-only folder (`CacheDb::actor_has_metadata_only_folder`, one indexed query), and a failed read declines the cache too. Serving is untouched — the bytes are sealed and the caller may read a folder of that owner's; **resting them is what breaks the promise the owner was given a UI switch for** (`principles.md` § The user always controls their data). Note the direction: the fail-closed-to-FULL rule above is right about an unparseable residency *value* (never stop bytes resting) and inverted for an unattributable *chunk* (never rest bytes the owner asked us not to). **The stated trade was:** the gate is per-owner, so an owner who has opted in anywhere loses the relay re-hydration cache on their full folders too. **Closed on the read rail 2026-08-21**, by the second half of the same ruling, which moved the attribution to where it can actually be established — the **seat**, not the request. Three pieces: (a) the daemon claims its chunk cache directory once, `bins/fauna-sync`'s `claim_cache_for_folder` writing a digest-only `.folder-owner` marker of `(device id, folder)` — absent claims it, matching keeps it, differing never re-claims it and is never evicted. ⚠ **That marker alone records who claimed the directory FIRST, not who owns it** (2026-08-21): its `None` result gated only the *serve* side, so a daemon that lost the claim kept **writing** its chunks into the winner's directory and the winner went on stamping every hash there with its own folder — which, with (c) below, made the nest *rest* a metadata-only folder's bytes where it had previously refused them. A regression, not a residual. What makes the assertion true is the **construction**: each daemon's cache is a per-`(device, folder)` subdirectory of the configured root (`scoped_cache_dir`), applied to an explicitly configured root as well as the default, so two daemons cannot reach one directory and the marker is reduced to the cheap check it reads as. Nothing is carried in from the flat pre-scoping root — it predates the compat-remnant sweep, so every scope starts empty (`conflicts.md` § Implementation status today owns the scoping); (b) an owning seat serves `FetchChunk` and stamps the folder on its answer (additive `DeviceMessage::ChunkResponse.folder`), while a seat that cannot prove sole ownership serves **nothing** — and a seat answers and stores only a well-formed content name (exactly 64 lowercase hex, the digest's own spelling), refusing any other name on all four frames (`FetchChunk`/`FetchManifest` answer `ChunkNotFound`, `StoreChunk`/`StoreManifest` are dropped): the name is the nest's own text, and a path in it would reach files outside the cache, the daemon's identity seed included (the typed `ws_client.rs::CacheKey` is the only name the cache helpers accept; pinned by `ws_client.rs::tests::{a_fetch_for_a_name_outside_the_cache_answers_chunk_not_found,a_store_for_a_name_outside_the_cache_writes_nothing}`); (c) the route's `RelayCache` gains `StoreIfAttributed`, taken when the hinted folder is `full` but its owner holds a `metadata_only` folder somewhere — it rests the bytes only when the seat attributed them to *this* folder. So an upgraded seat gets the re-hydration cache back and the per-owner downgrade survives only as the compat arm for a seat that asserts nothing: **absence of attribution falls to *do not cache*, never to `Store`.** Pinned by `chunk_relay.rs::each_relay_cache_arm_rests_on_exactly_its_rule` — the truth table is the ruling, and it fails if the arm is folded onto `Store` or onto `Transient` — plus `sync_relay_serving.rs::an_announced_answer_is_attributed_so_a_full_folder_still_caches` (an announced answer is attributed by construction, so it rests). The daemon-era pins of the unattributed arm left with the `/sync/ws` data plane (2026-10-02): no remaining seat answers unattributed. What remains, deliberately: the **write** rail's accept-and-*reclaim* deviation above (a different rail, unchanged by this), and any seat older than the `folder` field, which the compat arm covers. **The UI trickle-down's tracking was mis-cited** (a different, unrelated feature — folders slice-e's nest-place-editor lift); corrected 2026-08-21. tui shipped the reference build 2026-08-20; **linux, web, and android BUILT 2026-08-21** — `DevicesMachine::set_folder_residency` landed once on the shared `fauna-devices-machine` crate (the `nest_api` trait + both real-backend specializations + the fake), so linux (direct), web (`fauna-wasm-folders`'s `InnerDevices` alias), and android (the crate's UniFFI-exported impl block) all reach the identical method. **macOS + iOS BUILT 2026-08-25** — one shared FaunaKit `FolderResidencySelect` reaches the same machine-level method via a new `DevicesMachineVM.setFolderResidency` wrapper, rendered as a sibling right after the nest-place editor's save button; apply-on-change with the confirm-arms-rather-than-commits shape this section's own paragraph above names (the Picker's binding reads off the live snapshot, so it naturally keeps painting the current value while armed — no manual revert needed, unlike an immediately-committing native picker). Only windows remains. ⚠ **linux's leg did not actually keep the promise this section's own arm-then-confirm sentence makes, and the fix is the shape every immediately-committing picker needs** (found + fixed 2026-08-27 while its sibling `folder-audience-public-confirm` landed on the same rows): a `gtk::DropDown` commits its pick the instant it is made, and the leg reset it only on **cancel**, so for as long as the confirm sat unanswered the row painted `metadata_only` — reporting a residency the folder did not have, which is exactly what arming exists to prevent. The arm branch now snaps the selection back to the current residency *before* presenting the dialog (a no-op guard on an already-current value terminates the re-entry), so a cancel has nothing left to undo. apple's leg was already correct for the structural reason its own note gives — its Picker reads off the live snapshot — and the windows leg needed exactly linux's shape, not apple's, confirmed by its own run. **windows BUILT 2026-08-27** — a WinUI `ComboBox` commits its pick immediately like GTK's `DropDown`, so the leg needed the same snap-back-before-arm fix linux's own leg needed; anchored beside the existing `folder-conflict-policy-select` rather than a `folder-nest-save-button` neighborhood, which does not exist on windows (that slice-e nest-place-editor arm is a separate, still-unbuilt row). `test_folder_residency_control.py` both cases GREEN on `--app windows` first run, including the arm-before-evict assertion that would have caught linux's bug. Remaining: the seat↔seat peer-leg direct pull (`account-sync-plane.md` § The peer leg — until it lands the nest relay is the only content path), and the seats of **relay serving** that are not built yet (§ Relay serving's status paragraph) — until 2026-10-01 the relay asked only `/sync/ws` seats, which no app, agent or engine holds, so on a deployment whose devices all run the apps a metadata-only folder's bytes could not move at all (measured 2026-09-30); the per-user agent serves since that day. **The `public` pair (the fifth refusal, ruled 2026-10-01) is unbuilt — read from the code:** the audience arm of the folder update handler reads no residency and the residency arm reads no audience, so a folder can be both today, and it then lists for its followers and opens for none (the follower's byte read is the open by-hash GET, whose miss arm serves an owner or roster member only).

### Content reachability — what a folder's `source_online` means (ratified 2026-09-22; built 2026-09-24)

`fauna.media.list`'s per-item `source_online` (the `media-source-status` dot, `../ui/media.md` § Source status vs. sync state) and `fauna.sync.status`'s `source_online` are **one per-folder verdict, defined here and nowhere else: the folder's content is reachable right now — some holder of its bytes can serve them.** Two kinds of holder exist. The **nest place itself**, whenever it holds the folder's content (residency *full*, the default — § Content residency). And a **connected seat that can serve reads** — exactly the seats the relay read path would ask: every connection that announced the folder's row for relay serving (§ Relay serving). So `reachable = nest_holds_content || some live connection announced the row`. It is **not** any device's liveness, and it is **not** the liveness of one designated "source" seat.

**Which seats count.** Plural, and whether or not they *originate*: the relay does not ask a seat for its role, only whether it can answer the relay's ask, and neither does this verdict. The legacy `folders.source_device_id` is **never read** — it names one device by the resting role spelling `source`, so under it a `custom`-role originating point, a places-model folder with no `source` seat, and every seat but the named one read as "no source"; the column is on the drop path ([`folders.md`](folders.md) § Implementation status today) and this verdict is one reader fewer. A seat that cannot serve reads does not count, and honestly so: the verdict is about bytes. The serving path of a seat that runs the shared engine is **relay serving** (§ Relay serving, ratified 2026-10-01): a resident engine announces the folders it holds on the connection it already has and joins the set the relay asks, so the verdict follows with no change to this rule. **Built for the per-user `fauna-sync-agent` (2026-10-01):** its engines' folders are announced and count. A seat outside the agent — an on-demand replica hosted elsewhere, a phone's ingress engine — still answers nothing until its row of *Which seats hold* is built; the legacy daemon's `/sync/ws` seat counted until the daemon was removed (2026-10-02), and the route itself left the same day. Two seats never count, whatever is built: web, which keeps no bodies, and a phone's run-and-drop ingress engine (§ Relay serving → *Which seats hold*).

**Why the nest is a holder — the ruling that rejects both of the original candidates.** For a full-residency folder the nest holds every listed file's bytes by construction (§ 3 File Upload: chunks and manifest precede the change record), so the content is reachable while the nest is, whatever any seat does — a laptop-sourced folder reads *reachable*, which is the common deployment the drift had reversed. Candidate (a), *"the seat's device holds a live WS-RPC session"*, answers "is the device up", which for a full folder changes nothing about whether a file can be opened, and it costs a device↔session binding the session registry did not have at the time (`RpcConnection` carried `actor_id` + `token_id` only; it carries the minting device key since 2026-09-22) — that binding is the **Devices** page's question, [`devices.md`](devices.md) § Listing Devices → *The binding*, kept separate on purpose. Candidate (b) as first framed, *"a seat can serve relay reads"*, was the right axis and the wrong scope: without the nest as a holder it paints every app-sourced full folder red for as long as it exists, under copy claiming files may be unavailable when every one of them is a nest fetch away.

**When the dot is red, honestly.** A metadata-only folder with no holding seat connected — exactly the case § Content residency's consequence 1 states in the opting UI ("content moves between devices only while one holding it is online"). One declared imprecision: a folder flipped *back* to full re-hydrates opportunistically (consequence 3), and until it has, "full" overstates what the nest holds; v1 reads `residency == full` as "the nest holds the content", and a per-folder re-hydration mark, when residency grows one, refines this rule rather than changing it. A store miss on a full folder outside that window is a bug, not a status.

**Copy and wire.** The dot's copy names the **files**, never a "source device" — there is no source device in the places model: `media.source_online` / `media.source_offline` / `media.source_offline_notice` keep their keys (all seven apps share the strings) and say *files reachable* / *files unreachable* / *files unreachable — no device holding them is connected*. The dot stays distinct from the per-file `sync-state-badge` (§ Per-file sync-status display): the dot is the folder's reachability, the badge is the file's own state. **No wire change**: `source_online: bool` keeps its name and type on both replies; only the nest's computation moves, so an older app renders the new verdict under its old copy — truer than what it rendered before — and a newer app against an older nest sees the old verdict, both within the additive-everywhere rule (`../architecture/version-compatibility.md`).

*Implementation status (built 2026-09-24).* One nest function, `chunk_relay::folder_content_reachable(&WsState, &FolderRow)`, computes the verdict — a connection that announced the row (`WsState::has_announced_for_folder`, since 2026-10-01) counts as a holder; an owner `/sync/ws` seat counted too until that data plane was removed 2026-10-02 — and both consumers call it — `fauna.media.list` (`bins/fauna-nest/src/media_handlers.rs`) and `fauna.sync.status` (`bins/fauna-nest/src/sync_handlers.rs`, which still echoes `source_device_id` on the wire and no longer reads it for liveness) — so the two replies agree on every folder by construction. The truth table is unit-pinned beside the function (full with no seat → reachable; metadata-only with no announced connection, or only one that announced another row, or a revoked one → unreachable; metadata-only with a connection that announced the row → reachable; the legacy column never consulted). The witness, `tests/e2e-unified/tests/test_media_source_status.py` (tui first), pins the flip both ways on a **metadata-only** folder held by a second device of the account — a real app and its sync agent, whose announce is what makes the folder reachable and whose stop (the harness's own teardown) is what ends it — read from an app that holds no copy, with the badge-independence assertion, and pins a **full** folder whose file was recorded with no seat ever connected as reachable; both read the verdict off both wire replies as well as the dot. Before this build both consumers read `folders.source_device_id` → the legacy data plane's seat registry, so every app-sourced folder read offline for ever. The declared imprecision above (a folder flipped back to full reads reachable before re-hydration completes) stands. The device-level `online` on the Devices page had the same root cause and its own fix (`devices.md` § Listing Devices).

### Relay serving — how a metadata-only folder's bytes move between app-only devices (ratified 2026-10-01; BUILT 2026-10-01 for the per-user agent's seat, other clauses UNBUILT — status paragraph at this section's end)

**The gap this closes.** § Content residency makes the nest relay the only content path of a metadata-only folder, and until 2026-10-01 the relay asked only seats on the legacy `/sync/ws` data plane. The one program that dialed it was the legacy headless daemon ([`sync-engine-deployments.md`](sync-engine-deployments.md) § Control Plane Principle, the removed third deployment), which no app started. So on a deployment whose devices all run the apps no seat can hand over a byte: a second device lists the file and can never open it, and the folder reads unreachable for ever. **Ruling: a seat that runs the shared engine serves the relay over the connection it already holds. No app, agent or engine gains a `/sync/ws` dialer** — that socket was the legacy daemon's, and it went with the daemon (*The `/sync/ws` data plane leaves with the daemon*, below).

**The flow, four steps.** *(1) Announce.* A process hosting resident engines sends `fauna.sync.serve.announce { device_id, folders }` on its WS-RPC connection, naming by `FolderRef` each folder it runs an engine for and holds bodies of. The nest admits a folder only when the device is one of the bearer actor's registered devices and the actor may read the folder the ref names (its owner, or a member on its roster — the row-level reader's gate, `folder_authz::can_read_folder`, by its owner and member arms alone; a ref is a row, so the by-name resolver, which finds a member's own same-named folder first, is not the gate here), and keeps the result **on the connection**: in memory, replaced whole by the next announce, bounded in count, gone when the connection closes or is revoked. An announced connection is a seat of that folder exactly where the relay looks — the seat walk and the reachability verdict (§ Content reachability) count it. *(2) Ask.* On a store miss the relay walks its candidates as it does today — same window, same cap, same verification — and asks an announced seat with a push on that one connection, `fauna.sync.chunk.wanted { request_id, folder, store_key }`. *(3) Answer.* The bytes return on the bulk rail, never in a WS-RPC frame: `POST /api/v1/chunks/relay/{request_id}` under the seat's bearer, the stored chunk as the body. A seat that holds no such chunk says so with `DELETE` on the same path, so the window refills at once instead of waiting out the fetch deadline. The nest takes an answer only for a request it has pending and only from the actor it asked, checks the bytes against the store key before serving them, and applies the same `RelayCache` arm as for any seat — a metadata-only folder's bytes still never touch its disk. The ask names the folder and an engine answers only from that folder's own state, so the answer is attributed by construction and the `StoreIfAttributed` arm needs no assertion from the seat. *(4) Read.* A reader fetches by hash, under its bearer, with the folder hint (§ Content residency, gate 3) — every reader, web and both phones included, whichever path fetches: an engine's pull, or the by-manifest download behind the Media page, the web app and the one-shot calls. Without the hint, or without a bearer the nest can name, a store miss is a plain 404 and no seat is asked. All of it is additive on the wire — a kind, a push and a route an older peer never sees ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)).

**Why this shape.** The WS-RPC connection carries the client's requests and the nest's pushes; the nest never sends a request on it, and its 2 MiB frame bound is a security bound that sits below the 8 MiB chunk ceiling (§ Chunk size and transport contract). So the ask is a push and the bytes ride HTTP like every other bulk transfer. A push is lossy, and a lost one costs what a silent seat already costs: the relay passes it over. The announce is explicit rather than read off the device's places, because a place says a device *should* hold the folder while only the process running the engine knows that it does — and that process is the one the ask must reach.

**The seat serves from the file, through one serve core.** The engine keeps no chunk cache. It answers from the bound file: a local index, written when the seat seals an upload and when it applies a download, maps a store key to the path, range and plaintext hash of a body the seat holds; the seat reads that range, checks it, seals it, and checks the result against the key it was asked for. That is the body-from-disk core the cross-user share leg already runs ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the serve-side byte half), and it becomes the engine's **one** serve path with three consumers: the share leg, this relay, and later the same-account peer leg. So it leaves the `p2p-share` feature — relay serving is not an excisable feature — and it covers an owner-sealed folder as well as a content-keyed one. The rules it keeps: a body that changed since it was recorded, a placeholder, and a key the index does not name each answer *none*; the seat **never hydrates on an asker's behalf**; it takes only a well-formed store key, and only for a folder it announced; and a constant bounds its concurrent serves.

**Which seats hold.**

| Seat | Serves the relay |
|---|---|
| linux, macOS, windows — the per-user agent's engine over a bound location; headless tui with its agent | **Yes**: every recorded body, while the agent runs — the app may be closed. The first build. |
| An on-demand root the agent hosts (windows today) | Hydrated bodies; a placeholder answers none. |
| An on-demand replica hosted elsewhere — the macOS and iOS File Provider extension, android's provider | Not in the first build. It rides the replica access and the body source of [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Phone peers — design (decision 1); a phone serves only while the app is in front. |
| iOS and android library ingress (photo backup) | Not yet. The ingress engine runs and drops, and the body stays in the OS library alone; serving it is the same design's descriptor body source (decision 4). Until then such a file is listed on the other devices and unreachable from them — nothing is lost, the phone holds the original. |
| web | **Never** — it keeps no bodies. A declared platform absence as a holder; as a reader it is a full citizen. |
| the legacy daemon | Removed 2026-10-02; it served over `/sync/ws`, which left with it (next paragraph). |

**The `/sync/ws` data plane leaves with the daemon (ruled 2026-10-01; BUILT 2026-10-02 — the daemon, then the route).** The legacy daemon was removed 2026-10-02 ([`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Headless deployment owns that ruling and what the daemon's deployment handed to the agent), and its socket goes with it: no consumer is named for it, so nothing of it reaches the public tree. **What leaves the nest:** the route `GET /api/v1/sync/ws` and its `Hello`; the message set it carried in both directions (`DeviceMessage`, `NodeMessage`, and the frame bound that was theirs alone); the data-plane seat registry with its register, its per-device close and its revocation sweeps; the upgrade's own failed-credential throttle surface; and the `FileChanged` record path, already a logged refusal. **What stays, unchanged for every reader:** the store-miss walk with its window, cap, verification and `RelayCache` arms, over announced connections alone; the reachability verdict (§ Content reachability), counting announced seats alone; a device's `online`, which already reads the WS-RPC binding ([`devices.md`](devices.md) § Listing Devices); and the web-files projection's two maintenance functions (`route_web_file_change`, `reconcile_web_files_projection`), which shared a file with the socket's handler and nothing else. **Order:** the route is removed after this section's nest half and serving seat are built and after the daemon itself is gone — both true (the agent's seat announces since 2026-10-01; the daemon went 2026-10-02), and the route went the same day. *Implementation status (2026-10-02): BUILT.* The relay and the verdict moved to `bins/fauna-nest/src/chunk_relay.rs`, the projection's two functions to `bins/fauna-nest/src/web_files_projection.rs`; the route, `sync_ws/`, `DeviceMessage`/`NodeMessage` with `MAX_SYNC_WS_MESSAGE_SIZE`, `Surface::SyncWsUpgrade` and `touch_device_last_seen` are gone, and the revocation helpers close WS-RPC connections alone. The relay's rules are re-pinned through announced seats (`chunk_relay.rs`'s unit tests and `sync_relay_serving.rs`, whose `a_revoked_connection_stops_being_a_candidate` holds the four teardowns).

A member's seat counts on the owner's terms: it is the only holder of what that member wrote. A member whose account lives on the folder's home nest announces there like any seat. A member whose account lives on another nest is the next three paragraphs.

**A member on another nest: the ask goes through its own nest, the bytes go straight to the home nest (ruled 2026-10-01; refutable; step (5) BUILT 2026-10-03 on the home nest, step (1) BUILT 2026-10-04 but for the Welcome's seed and the upload door, steps (2)–(4) and (6) BUILT 2026-10-04 on both nests but for the seat that announces).** Such a member holds no session on the folder's home nest: its engine relays the control plane through its own nest and dials the home nest's byte plane itself (§ Multi-writer shared sets, the cross-nest write plane). Relay serving keeps that split and opens no connection to the home nest. The kinds, gates and stamps named below are [`../architecture/federation.md`](../architecture/federation.md)'s (§ Cross-nest shared folders + channel append → *Relay serving across nests*); the flow is this section's. *(1) Residency reaches the seat.* The home nest stamps the folder's residency on the carriers the member's access rides, the member's custody record keeps it, and the member's seat arms from that record what a same-nest seat arms from its row: the upload skip, the holder-keeps gate and the upload door's refusal. A record no home nest has stamped reads *unknown*, and each consumer takes its own safe side — the seat uploads (nothing unparseable may stop bytes resting) and keeps what it wrote. *(2) Announce.* The seat announces the folder on the connection it already holds — to its own nest, by the folder's foreign ref and home nest. Its nest forwards that to the home nest, naming the member and the device. The home nest admits it only for a member on the folder's roster who holds the `writer` grant, and keeps it as a seat of the folder: in memory, bounded in count, on a lease the member's nest renews while the connection still announces and that lapses by itself when it stops. *(3) Ask.* A foreign seat is one more candidate in the same walk, window and cap. The home nest asks it through the member's nest, which pushes `fauna.sync.chunk.wanted` on that one connection and says at once when the connection is gone, so the window refills. *(4) Answer.* The seat answers on its own byte plane: the same answer route, on the home nest, under the write token its uploads already carry. The home nest takes the answer only from the actor it asked, checks the bytes against the store key and rests nothing. *(5) Read.* The store-miss arm admits a cross-nest member on a short-lived byte-plane token its own nest obtains for it through the home nest's member gate — a writer's write token, or a read-scoped twin for a reader — and resolves the folder against the roster at every use, so a removal ends that member's reads at once, whatever life the token has left. *(6) Verdict.* A foreign seat with a live lease counts in § Content reachability exactly as an announced connection does.

**Why this shape, and what was weighed.** It is the cross-nest design's own split, applied to one more message: control through the member's nest, bytes direct. The member's nest carries a store key and never a byte, so *content stays on my devices* holds on both nests — the home nest relays in passing, as for any seat, and the member's nest sees no content at all. That nest already speaks for its member on every federated call, so the forwarded announce adds no trust; a nest that announces a seat that is not there costs one slot of the window, bounded by the candidate cap, and can ask a seat only about a folder that seat itself announced with that nest as its home. A **reader's** seat does not serve: it wrote nothing that only it holds, and it has no credential on the home nest's byte plane to answer with. Every piece is additive ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)): a nest on either side that predates it refuses the kind as unknown, and that seat is simply no candidate. *Rejected:* a connection from the seat to the home nest (a connection class for an actor with no account and no standing there, one more socket per home nest, and the one exception to control-through-the-own-nest); the member's nest relaying the bytes (a second nest in the byte path of content the owner asked to keep off nests); the cross-user share leg as the only path (an excisable feature that web lacks and a phone cannot host — the reason the peer leg is not the floor, below); refusing the pair for good (it divides one folder's members by where their accounts live, which no member chose).

**Until that leg is built, the pair is refused.** A metadata-only folder with a member on another nest fails in both directions today (the status paragraph below), and no opting copy says so. So the home nest refuses whichever side moves second: flipping a folder to metadata-only while its roster holds a member on another nest, and adding such a member to a metadata-only folder — a typed refusal, its reason shown where the serving refusals of § Content residency are shown. A pair that already exists is left as it is; nothing is evicted or flipped back. The refusal is removed in the change that lands the cross-nest witness.

**A write door that keeps no body refuses a metadata-only folder.** A record into such a folder is a promise that the recording seat holds the bytes. The Media page's upload door posts the file to the nest and keeps nothing, on every app, and both of its arms would go wrong here (read from the code 2026-10-01; the refusal below is built, so neither arm is reached): a content-keyed folder's chunks are accepted, reclaimed, and rest nowhere while the file stays listed; an owner-sealed folder's file goes up as one blob that the residency gates take for the file's manifest, so it rests on the nest against the folder's promise. That door refuses a metadata-only folder before anything is sealed or sent — one shared-Rust refusal, with its reason shown ([`../ui/media.md`](../ui/media.md) § User actions owns the surface). The way into such a folder is to put the file in it on a device that syncs it.

**A holder keeps what it wrote.** Freeing a body is lossless only if its head can be fetched again ([`on-demand-files.md`](on-demand-files.md) owns that gate), and in a metadata-only folder a record proves nothing of the kind: the nest took no bytes. So there a seat never frees the body of **its own recorded write** — not by *free up space*, not by letting the OS evict it, not by demoting it to a cache the OS may reclaim. That waits for the custody receipts § Content residency's v1 bullet defers to. A body the seat fetched from another holder may be freed as before, which returns the device to where it stood. The gate reads the folder's residency **when the body is freed**, not when its proof was earned, so a folder flipped to metadata-only stops its seats freeing what they wrote while it was full; a row that does not say how its proof was earned counts as the seat's own write.

**A superseded own-record body is replaced, never freed (ruled 2026-10-04; BUILT 2026-10-04 in the engine and on the linux root, the other bindings UNBUILT — status paragraph).** On an on-demand root a hydrated copy the head has moved past is freed and its row re-pointed, to be fetched at the next open ([`on-demand-files.md`](on-demand-files.md) owns that road). The rule above refuses that free for the seat's own record, and must: the new head rests on its writer alone, so a seat that freed its body would hold no version of the file, and the file would open nowhere while that writer is away. So such a seat follows the head as a resident root does — **fetch first, replace second, move the row last.** It fetches the new head from a holder and writes it over the old body through the resident root's apply door; that door reads the disk again after the fetch and settles anything but the recorded body as a divergence ([`conflicts.md`](conflicts.md)), and the seat does not start at all over a body edited since its record — that edit is the upload rail's. Only then does the row stand at the new head, the file still on the disk, its proof a *fetched* one. The seat was a holder of the file and stays one; the body may be freed afterwards like any fetched body. *The fetch is eager, and a wait when no holder answers:* it is tried on the pass that finds the head moved and on every pull after it. Until one succeeds nothing changes — the old body, the row at the old head, the seat still answering the relay for that body — and the file is not vouched in-sync, as an own record there never is. No state is added for the wait: the row says what the disk holds at every instant, so a crash lands on one whole version. *The old version is owed to nobody.* It is overwritten as on a resident root: a version in a metadata-only folder can be restored only while some seat still holds its body (§ Content residency, the snapshot clause), and no seat is asked to keep superseded ones. *One rule, in the engine.* The fetch, the judgement and the proof are shared Rust; a binding supplies only when it is asked and where the body is written — the off-disk root when its own gate refuses the free, cfapi when the OS's refusal lands on such a body (the written file then re-anchored as the recorded-upload flip does it), the File Provider by holding its row at the old head until the fetched body is in hand.

**The self-echo reads the local body.** A seat's own change comes back in its pull, and the engine fetches that content again to advance its merge base — here, from a nest that never had it. The manifest carries the whole-file hash, so the echo first compares it with the local file: when they are equal the local body *is* the echoed content and nothing is fetched, in every residency. Only a body that has since moved on is fetched, as today. What an echo does when those bytes rest on no seat at all — a superseded own write in a metadata-only folder — is [`conflicts.md`](conflicts.md)'s question and is not ruled here.

**The peer leg stays the direct path, and nothing here waits on it.** § Content residency's seat↔seat pull over the account plane's peer leg takes the nest out of the byte path and works while the nest is unreachable; it is a further consumer of the same serve core, admitted by the same-account witness. It cannot be the whole answer — web has no peer leg and a phone hosts none — so relay serving is the floor every deployment stands on, and the peer leg improves on it.

**The opting copy stands.** The residency hint and confirm (`FOLDER_RESIDENCY_METADATA_ONLY_HINT`, `RESIDENCY_CONFIRM_BODY`: content *"moves between your devices only while one of them holding it is online"*) describe exactly this path, and are honoured rather than reworded. A desktop is online while its agent runs; a phone, while the app is in front.

*Implementation status (2026-10-01): BUILT — the nest half, the engine's serve core and the serving seat of the per-user agent; the clauses named unbuilt below are UNBUILT.* **Built, nest-side:** `fauna.sync.serve.announce` (`sync_handlers::serve_announce_handler` — a device of the caller's account, each `FolderRef` row admitted on `can_read_folder`'s owner and member arms, at most `SERVE_ANNOUNCE_MAX_FOLDERS`, kept on `RpcConnection` and replaced whole); the ask push `fauna.sync.chunk.wanted` (`PushEvent::SyncChunkWanted`, sent with `WsState::push_to_connection`); the answer route `POST`/`DELETE /api/v1/chunks/relay/{request_id}` (`chunk_routes::{answer_relay_chunk, decline_relay_chunk}`, `BulkWriteAuth`, taken only for a pending ask whose asked actor equals the answering actor — each pending ask records the actor it went to, so no other actor's answer can complete it); announced connections as the candidates of `ChunkResolver::relay_for_folder` (`bins/fauna-nest/src/chunk_relay.rs`; the `/sync/ws` seats raced beside them until that data plane's removal, 2026-10-02), asked in the relay's window under its cap and `verify_fetched`, attributed by construction, each non-owner actor re-checked against the row at every walk; and announced connections in `folder_content_reachable`. Pinned by `bins/fauna-nest/tests/sync_relay_serving.rs` (a test client announces and answers; the reader is served and the store keeps nothing; a decline settles at once; another actor's answer and an unissued id are refused; a member's seat is asked; each of the four WS-RPC teardowns — session revoke, revoke-all-others, device removal, actor-wide — and a replacing announce end the candidacy; every `RelayCache` arm holds through an announced seat) and the announced-seat rows of the truth table beside the function. **Built — the engine half of *The seat serves from the file*:** `SyncEngine::serve_chunk` answers a store key from the bound file through the one serve core (`fauna-sync-engine`'s `serve_core`, out of `p2p-share`; the share leg's `chunk_body_from_hit` calls the same core), resolving the key through the per-folder `held_chunks` index that the upload seal (before the metadata-only skip) and the download apply write, and sealing under the root the upload would use — owner-sealed, content-keyed and public folders alike; an unknown key, a placeholder, a changed body and a reseal that misses the key answer none (tier 1, `relay_serve_test`). **Built — the serving seat (2026-10-01):** `fauna-sync-engine`'s `relay_seat` is the shared half every host of resident engines wires in. `RelaySeat` holds the running engines' folders, keyed by `FolderRef`; `RelaySeat::run` announces that set whole on the host's connection — at start, on every reconnect, whenever an engine registers or leaves, and again after a refused announce — and hands each `fauna.sync.chunk.wanted` to the engine of the folder it names. An ask for a folder the process did not announce, or carrying anything but a 64-character lowercase-hex store key, goes nowhere; a folder homed on another nest is never registered. `ServeInbox::serve` answers through `SyncEngine::answer_relay_ask` (`serve_chunk`, then the `POST` or the `DELETE`), at most `SERVE_CONCURRENCY` (4) at a time per folder, with a bounded queue behind it whose overflow is dropped like a lost push. It runs **beside** the engine's loop, never as an arm of it: the seat that asks for a chunk is itself an announced seat of the folder and is asked for the very chunk it is fetching, and only a serve that is not waiting on that fetch can decline at once. The per-user agent is the one host: its engine driver registers each engine for as long as it runs — both roots, so an on-demand root serves its hydrated bodies — and the engine host's background task runs the seat over a connection of its own (`bins/fauna-sync-agent`'s `engine_driver`). Pinned by `relay_seat`'s unit tests (routing, the store-key shape, withdrawal on drop, and the restart order the agent first showed — a cancelled run that registers after its replacement and then drops leaves the folder announced, because each run registers and removes its own entry, the concurrency bound against a literal), by the tier_3 `bins/fauna-nest/tests/conformance_relay_serving_two_engines.rs` (two real engines against a real nest: seat A writes and uploads no chunk, both seats announce, seat B's ordinary pull reassembles the file from relayed chunks, the store holds none of the file's named store keys before or after, a key no seat holds is declined by both at once, and B serves the file once it has applied it), and through the shipped processes by `tests/e2e-unified/tests/test_filesync_metadata_only_relay.py` (two apps, each with its agent: the second device receives the first one's file) and `test_media_source_status.py` (the agent's announce makes the folder reachable; its stop ends it). **Unbuilt on the read side:** step (4)'s by-manifest readers. An engine's pull carries the hint and its bearer; the download path behind the Media page, the web app and the one-shot calls sends neither the hint nor, on web, a bearer (`fauna_client::NestPublicChunkFetcher`, `fauna_core::file_download::WasmPublicChunkFetcher`), so a device that syncs the folder receives its files and a device that only browses it cannot open them. **Unbuilt seats:** every row of *Which seats hold* but the agent's. `apply_self_echo` always fetches. **Built (2026-10-04): the holder keeps what it wrote, in shared Rust** — a row's proof says how it was earned (`sync_entries.recorded_proof_origin`: this device's own record, or *fetched* — hydrate-on-open, a download apply, a peer body the nest's row confirmed; a row that does not say reads as own), the engine persists its residency reading in the folder's state DB at build and on every `refresh_sync_mode` (`SyncDb::residency_reading`), and `SyncEngine::is_dehydration_safe_in` keeps an own-record body unless that reading is *full* — a reading never written keeps it too. Every caller inherits the one gate: the off-disk dehydrate, the windows shell verb, the platform's in-sync assertion and the owned tree's demotion to its cache root. Pinned by `pull_remote_changes_test` (own record kept, fetched body freed, the flip, the absent reading), `the_free_space_gate_frees_only_the_recorded_content` and `fuse_live_integration::freeing_an_own_record_in_a_metadata_only_folder_is_refused`. **Still unarmed:** the File Provider's own eviction; on windows, an in-sync bit asserted while the folder was full outlives a flip, so the pin reaction's bare dehydrate and the OS's own eviction are not held, and an own record made in a metadata-only folder keeps its sync-pending arrows — read from the code. **Built (2026-10-04): a superseded own-record body is replaced, in the engine and on the linux root** — `SyncEngine::replace_superseded_own_record` takes the row the on-demand fold reported stale, and only a `Synced` own record the gate keeps whose disk still holds the recorded body; it runs `download_and_write_file` with no listing context and answers whether the row now stands at the new head. The agent's `apply_stale_hydrated` calls it when the free is refused on an off-disk root, counts the row as followed and reports the file `Synced`; a failed fetch is logged and left for the next pull. Pinned by `superseded_own_record_test` (the body replaced and then freeable; no holder answering leaves body and row as they were; a full folder's body and an edited one are not taken) and by `bridge`'s three `apply_stale_hydrated` tests beside the older ones; not yet driven through a mounted root or two real seats. **Unbuilt bindings of that rule:** cfapi, where the refusal is the OS's and the fallback is not wired (the seat keeps its old head, no byte lost); and the File Provider, whose refresh (`SyncEngine::apply_refresh_fold`) re-points every stale row without asking the gate, so the row moves to a head whose body this seat has not fetched — the order the rule forbids; read from the code, with no measurement recorded of what the OS then does with the old body. **Built (2026-10-02): the Media upload door's refusal** — `MediaMachine::upload` reads `MediaFolder::metadata_only` (carried from the folder list read, only an explicit `metadata_only` arms it) and refuses before anything is sealed or sent, with `media.error_metadata_only_folder` on `error-message`; pinned by `upload_into_a_metadata_only_folder_refuses_and_sends_nothing` and `test_upload_into_a_metadata_only_folder_is_refused_with_its_reason`. Whether a single-blob Media upload already resting in a metadata-only folder is dropped at the flip, and what becomes of one that rests there now, is unmeasured and not ruled here. **The cross-nest member leg (ruled 2026-10-01) is built nest-side in every step — residency reaching the seat (step (1)), the nest half of the announce, the ask, the answer and the verdict (steps (2)–(4) and (6)), and the read door (step (5)), each below; the pair still fails on the serving side, because no engine announces a foreign folder yet (`relay_seat` never registers a folder homed on another nest), so a cross-nest writer's file in a metadata-only folder opens for no other member.** **The nest half of steps (2)–(4) and (6) (BUILT 2026-10-04):** `fauna.sync.serve.announce` takes an additive `foreign` list beside `folders` — `{ folder: foreign:<channel>, nest_url }` — and the member's nest forwards each entry to its home nest as `fauna.federation.folder.serve.announce` (`sync_handlers::forward_foreign_announces`), checking only that the device is the caller's; what the home nest admits goes into the reply's `admitted` and onto the connection with the home nest's verified `nest_id` (`ws::ForeignServing`), and a refused or unknown-kind forward is left out. The connection's renewal task (`sync_handlers::keep_foreign_seats`) renews at half the lease the home nest states and says *no longer serving* when the connection closes or is revoked; a replacing announce withdraws what it drops. The home nest gates the forward on the write gate and leases a seat in `chunk_relay::ForeignSeats` — memory only, keyed by folder row, member and device, `FOREIGN_SEAT_LEASE` (120 s), at most 16 per folder and 4096 in all. `ChunkResolver::relay_for_folder` takes such a seat as a third candidate (`chunk_relay::SeatVia::Forwarded`), re-checked on the write gate at every walk and dropped when it fails (`chunk_routes::foreign_seats_for`); the ask is `fauna.federation.folder.chunk.wanted` to the leasing nest alone (`federation_pool::originate_folder_chunk_wanted`), and anything but `pushed: true` passes the seat over at once. The member's nest pushes `fauna.sync.chunk.wanted` naming the folder by its foreign ref, only on the connection that announced that device and folder with the calling nest as its home (`ws::WsState::foreign_serving_connection`). The answer is the existing route under the member's write token, taken only from the asked actor. `folder_content_reachable` counts a live lease. Pinned by the tier_3 `bins/fauna-nest/tests/conformance_relay_serving_cross_nest.rs` (two real nests, the pair seeded beneath the refusing doors: the flow end to end with neither store holding the chunk; a reader's seat refused by omission and a demoted seat dropped at the next ask; a seat with no connection settling at once; a third nest's ask pushing nothing; another actor's answer refused; a closed connection and a replacing announce ending the lease) and by `chunk_relay`'s unit tests (the lease's renewal, lapse and bounds, an unpushed forwarded ask, and the truth table's foreign-seat rows). **What remains of the leg:** the seat — an engine of a folder homed elsewhere registering with `relay_seat` and answering under its write token — and the two-nest witness through real engines, with the interim refusal's removal; the Welcome's seed of the residency stamp. **Residency reaches the seat (BUILT 2026-10-04):** the home nest stamps the folder's residency, always stated (`metadata_only` or `full`, off its claimed row — `federation_handlers::residency_stamp`), on the three federated folder read replies and on the Welcome relay; the member's nest threads it through its relays; the member's commit poll writes it into the `ForeignFolder` record (`custody::refresh_foreign_set_from_reply`; `ForeignFolder::residency` folds on its own stamp, a tie landing on metadata-only); the foreign binding builds from that record (`ResolvedBinding::metadata_only_residency` is an `Option<bool>`: an unstamped record is *unknown*, arms no skip, persists no reading and clears a stale one), and a flip reaches a resident foreign engine as a basis change at the custody edge (`BindingBasis::of_foreign` carries the reading — the per-tick sync-mode read never does for a foreign engine). So a cross-nest writer's seat in a metadata-only folder records its manifest and uploads no chunk, and the holder-keeps gate keeps its own-record bodies. Pinned by `custody`'s refresh and fold tests, `build_engine_retired_custody_test` (a record stamped metadata-only arms the skip, an unstamped one uploads and persists nothing, a stamp change is an edge), `pull_remote_changes_test::an_unknown_residency_at_build_clears_a_stale_full_reading`, `conformance_federation_channel::every_federated_folder_read_reply_stamps_the_folders_residency`, and the tier_3 `conformance_cross_nest_conversations_client::cross_nest_writer_into_a_metadata_only_folder_records_and_uploads_no_chunk` (two real nests, the pair seeded beneath the handlers: one commit poll carries the flip into the record, and the writer's chunks are absent from the home store by name while its manifest is there). **Unbuilt in step (1):** the accept does not seed the record from the Welcome's stamp (`join_folder_welcome` takes no residency), so a fresh member's record reads *unknown* until its first commit poll; and the upload door's refusal reads the Media page's folder list, from the own nest's `fauna.folders.list` — whether it is reachable for a foreign set at all is not checked. **The read door is built on the home nest (2026-10-03):** the store-miss arm takes, beside a session bearer, a byte-plane token minted for a cross-nest member — its write token, or the read-scoped twin (`fauna.folders.read_token.get` → `fauna.federation.folder.read_token.mint`) — and resolves the hint for such a token through the cross-nest roster alone, read at every request, so a removed member's live token reads nothing (`chunk_routes::relay_chunk_for_folder`, `folder_authz::resolve_foreign_readable_folder`; pinned in `bins/fauna-nest/tests/sync_relay_serving.rs` and, for the mint's gate, `conformance_federation_channel.rs`). A cross-nest writer's engine already sends its write token and its folder hint on every chunk read, which is what the door now admits — pinned at the nest with a test client, not yet driven by a real engine across two nests; a cross-nest reader's engine takes the read token (2026-10-05, [`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host, decision 3). The pair still fails on the other side: the nest asks a cross-nest seat now, but no engine announces one, so the door relays only from a seat on the home nest. **The interim refusal is BUILT (2026-10-04), nest-side, both directions:** the folder update handler's residency arm refuses the flip while the folder's channel holds a `channel_foreign_members` row (`folder_handlers.rs`, pinned by `conformance_folder_residency.rs`), and `welcome_deliver_core` refuses a Welcome for a recipient on another nest onto a channel claimed by a metadata-only folder before it relays anything (`conversations_handlers.rs`, pinned by `conformance_cross_nest_conversations_client.rs`); a folder already metadata-only and a recipient already on the channel's foreign roster are existing pairs and pass. **Its reason is not shown yet:** every app renders an `invalid_request` as the generic unexpected-error string — the sentence rides `details`, which is log-only — and the three serving refusals of § Content residency share the gap.

## Multi-writer shared sets (ratified 2026-07-18; same-nest write plane BUILT 2026-07-19 — see the status note at this section's end)

Design + rationale: the 2026-07-18 read-write/cross-nest design (tracked internally);
access model + UI: `../ui/folders.md` § Sharing; crypto: `../architecture/key-material-hierarchy.md`
§ M2 (Multi-writer bullet); cross-nest kinds: `../architecture/federation.md`. This section owns the
**sync-protocol mechanics** of a set with owner-granted `writer` members:

- **The gate family widens at exactly three kinds.** A new `writable_folder` resolver
  (owner OR roster member with `access == 'writer'`) replaces `owned_folder` at
  `fauna.sync.changes.record`, `fauna.folders.lease.{acquire,release}`, and
  `fauna.sync.conflicts.report`. `changes.supersede` stays **owner-only** (the re-seal /
  verified-reclaim pass is owner-run and head-bound); every config/roster/key kind is
  untouched. The `changes.record` device write-capability gate is kept — it keys on the
  connection actor, so a writer's own registered devices satisfy it.
  Two writer-engine consequences (decided 2026-07-19): the engine's drain re-seal path
  calls the owner-only `changes.supersede` after re-recording, so a **writer's** engine
  treats that typed refusal as **non-fatal** (the re-sealed record already landed;
  reclaiming the stale pre-re-seal record is the owner's verified-reclaim pass); and a
  typed `stale_content_key` refusal (KMH § M2 version floor) heals through the existing
  drain machinery — the path is requeued-as-prior-generation and re-seals after the next
  rotation-commit custody ingest advances the current generation, never via a blind retry.
  That heal is what scopes the refusal: a **declassified** record on a `public`-audience
  folder has no generation to advance to and must not be re-sealed while public, so it is
  exempt at the floor rather than refused (KMH § M2 owns the exemption and its bounds).
- **Owner-pays metering + per-member caps.** The per-path size delta charges the **set
  owner's** `storage_bytes_used` regardless of who records (delete-reclaim already
  credits the owner — one payer keeps the arithmetic coherent, and it is the only shape
  enforceable cross-nest, where the writer has no account on the storing nest). When the
  recorder is a member, the same transaction bumps `bytes_used` on their
  `folder_member_access` row (floored at 0) and refuses with a typed
  `member_cap_exceeded` when the owner-set `byte_cap` would be exceeded. `bytes_used` is
  an **abuse counter, not exact attribution** — writers supersede each other's paths, and
  the delta charges the author of the superseding record. Owner devices bypass the cap.
  **A negative declared `size_bytes` is refused before anything is read or written**
  (`fauna.sync.invalid_size`; added 2026-08-18). The declaration *is* the meter on this
  plane, so a value below the bottom of its range unmakes the accounting twice over: under
  retained accounting the charge simply *is* the declared size, so a negative one is not
  examined by the `charge > 0` ceiling check at all, and it then *credits* the owner
  instead of charging them (the member `bytes_used` counter counts the same way). The
  refusal lives in the one metering core (`record_sync_change_metered`), not at each door,
  because **four** record doors funnel there — `fauna.sync.changes.record`, the two
  `fauna.federation.{folder,backup}.changes.record` relays, and
  `fauna.bridges.webdav_record_change` — so one refusal covers them all
  and a fifth DOOR cannot be added past it. **But a door is not a core (added
  2026-08-19):** `upsert_backup_custody` — this function's own doc-named custody twin, reached by
  its own branch of `record_change_core` — is a *second* metering core, not a fifth door, and
  for a destination-custody set its `size_bytes` is likewise the owner client's **declared**
  logical size. It went unguarded for a day — the same defect, in a parallel core — and now
  carries the identical refusal at its own top. The rule that generalises:
  a chokepoint argument covers the callers that *funnel through* it and says nothing about a
  parallel implementation of the same job — when placing a refusal "in the one core", grep for
  the other cores, not just the other doors. Deletes are refused on the same rule even though
  that arm ignores the value: "negative is fine as long as you also say delete" is a
  carve-out every future reader would have to re-derive, and no honest engine sends one.
  **Not a wire
  break within the major** ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)),
  by the same argument the sweep-167 admin refusals landed under: every producer derives the
  number from bytes it holds (`.len() as i64` throughout the engine and the custody leg), so
  no honest client of any age sends a negative — the refusal narrows the accepted set only
  where nothing legitimate lives.
  **And the same obligation at the TOP of the range (added 2026-08-19):** bounding a declared
  quantity from below is only half the door's job — the *arithmetic that consumes it* must be
  bounded too. The owner ceiling summed `used + charge` unchecked, so a declared size near the
  top of the range could overflow the sum — and an overflowed total is neither refused by the
  ceiling nor representable in the counter column it lands in — a state no client must be able
  to reach ([`../architecture/nest/common.md`](../architecture/nest/common.md)
  § Client-state recoverability). Both quota comparisons therefore use `checked_add` and treat
  an unrepresentable total as over the ceiling (`Exceeded` / `MemberCapExceeded` — no new wire
  code, since a sum that cannot be represented is unambiguously over), and the shipped
  **release** profile has enabled `overflow-checks` workspace-wide since the 2026-08-19 ruling
  ([`../architecture/build-system.md`](../architecture/build-system.md)
  § Shipped-profile overflow checks). Cross-principal: the
  ceiling keys on the **owner's** row whoever records, so the check has to hold for a granted
  member recording into a shared folder, not only for the owner. **The general rule, and the one to carry to the
  next sweep: a declared quantity is a door obligation at BOTH ends of its range, and "the door
  refuses a bad value" is not finished until the sums that consume the good values are checked
  too.**
- **Attribution is nest-stamped, additive.** `SyncChange` gains `author_actor_id:
  Option<String>` — stamped by the nest from the authenticated connection actor (or the
  membership-gated federated requester), **never client-asserted**; the version-history
  projection carries it so versions render "who wrote this". Cryptographic provenance is
  ruled (2026-09-27) and built (2026-09-29): writer-signed change records —
  [`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
  § Writer-signed change records owns the design; the record then
  carries the writer's signature under the device-signed authoring chain, and the nest-stamped
  `author_actor_id` must agree with the signed author.
- **The fold and the conflict machinery are already writer-agnostic** (verified 2026-07-18):
  `apply_remote_changes` partitions peer-vs-self by `device_id` alone, and the resolver
  sees only base/local/incoming bytes + timestamps — another actor's change flows through
  download/merge/auto-resolve unchanged, and the losing version is always retained. The
  one honest caveat: latest-writer-wins compares wall-clocks across actors, so
  cross-actor clock skew can pick the winner — never lose data (the loser is a version).
- **Readers never bind locations** (`../ui/folders.md` § Sharing): a bound location whose
  edits cannot upload would breach this doc's iron rule that a tracked file's local
  modification MUST be uploaded. Writer members bind + sync with the same engine, keyed
  from their own custody copy.

**Implementation status today (2026-07-19, Phase 1 same-nest).** The nest +
wire + shared-client write plane above is **built**: schema v28
(`folder_member_access` + `folder_content_keys.current_version`), the
`writable_folder` gate at exactly the three kinds, `fauna.folders.members.
set_access` + the D5 share-time grant (`FolderShareRequest.{member_actor_id,
access}`), owner-pays metering with the same-step member `bytes_used` bump +
typed `member_cap_exceeded`, the monotonic version floor + typed
`stale_content_key` (owner-exempt), and nest-stamped `author_actor_id` on the
changes feed + the file-versions projection (`author_handle` resolved
nest-side; apps fold display once via `account_display_label` at the
`FileVersionSummary.author_display` transcribe). The role grant dies with the
membership (evict/leave delete the role row — a re-add starts from the reader
default). Shared picker catalog: `fauna_folders_machine::
member_access_options()` (+ FFI/wasm twins). Linux is the LEAD app leg
(share-flow role select, per-row role/cap editors, the uncapped-writer
warning).

**Cross-nest write plane BUILT (2026-07-20, Phase 3; kinds/gates owner:
`../architecture/federation.md` § Cross-nest).** A cross-nest `writer`
records through the federated `folder.changes.record` relay (its own nest
→ the set's home nest) under the same owner-pays metering + per-member cap +
version floor as a same-nest record, nest-stamped to the writer and
**content-idempotent** (a re-relayed record charges once — the durable
exactly-once check now guards the same-nest record path too), and uploads
sealed chunks direct to the home nest under a short-lived write-only bulk
token (`write_token.mint`/`.get`; engine `set_foreign_routing` +
`WriteTokenBearer`). Pinned two-nest tier_3:
`cross_nest_writer_records_and_uploads_owner_reads_back_through_client_stack`.
**v1 scope:** no cross-nest conflict-report relay (named gap — the losing
version is still retained) and no cross-nest upload lease (concurrent
cross-nest writes auto-resolve). The same-nest `lease.release` is now
holder-scoped (a release names its device — the field is required — and frees only that device's lease).

**Design RATIFIED 2026-07-20, build tracked. Superseded by the landings below — discovery,
the bind itself, and revocation are all BUILT on linux as of 2026-07-22 (tasks
2–4, PROVEN end-to-end by task 6); the remaining gap is the other six apps'
app leg (§ *Still open* at the end of this section) and the linux Media
resolver.**
The paragraph above describes the nest+wire+engine plane, proven by driving a
`SyncEngine` directly in the pinned tier_3 test. **The ratified closing design
(decisions of record):** the grant reaches the recipient's client as
**advisory-for-UI data, never an authz input** (the cross-nest mirror of the D5
share-wire invariant — the sole enforcement is the home nest's
`folder_member_access` row at `require_foreign_writer`, state the home nest
itself wrote): seeded at
Welcome time through the exact `set_name` additive relay chain
(nest-authoritative at `welcome_deliver_core`), refreshed by an additive
`caller_access` stamp on the federated read replies — poll-parity
with same-nest access changes, which have no push either; no new federation
kind. A bind is verified authoritatively by **one eager `write_token.get` at
the bind gesture** (typed refusal fails the bind loudly), and a mid-life
demotion is **fail-closed and loud**: the engine parks the set in a terminal
`access-revoked` state surfaced through the agent status plumbing and the
binding row, with the mapping pruned and local files/pending edits untouched
and visible — the same shape same-nest demotion adopts (retiring the "stale
mapping survives demotion" residual). Engine plumbing rides additive
`FolderEngineKeys.{home_nest_url, channel_id_hex}`; the agent points the
byte plane at the home nest under a `WriteTokenBearer` and calls
`set_foreign_routing`. Linux is the LEAD app leg.

***What of that is BUILT, as of 2026-07-22 (the discovery
wire).*** A cross-nest member's client now **discovers** its grant end-to-end:
`ForeignFolder` carries an additive `access` field, seeded from the Welcome
relay (`FedWelcomeDeliverRequest.access` → `WelcomeInbox`/`WelcomePayload` →
`join_folder_welcome` → the member's own folder-key custody) and refreshed by the
`caller_access` stamp every federated folder read reply carries (kinds +
absent-stamp semantics owned by `../architecture/federation.md` § Cross-nest →
*Recipient-side access discovery*); the custody-ingest commit poll CAS-updates
the stored value, so a promotion or demotion lands on the ordinary poll cadence
with no push kind. `foreign_rows` maps the real value onto the synthetic
`FolderSummary`, so a foreign writer now takes the **same** row split every
same-nest member row takes — one rule across both planes, rather than the old
unconditional `access: None`. Unknown (`None`) stays **reader ⇒ unbindable**,
the fail-safe direction, and the custody CRDT folds two devices' disagreeing
grants to the *lesser* privilege. **Still unbuilt:** the
linux Media resolver (`NestFolderKeyResolver`).
The `access-revoked` park + its surfacing landed 2026-07-22 (task 4) — see the
*Revocation* block below.

***The bind itself — BUILT 2026-07-22, linux, and PROVEN
end-to-end 2026-07-22 (task 6).*** A cross-nest writer's folder now binds, on
three legs:

- **Key material + routing reach the agent.** A foreign set is in no
  `fauna.folders.list` projection at all — it exists only in the member's own
  folder-key custody (`fauna.state.folder-keys`) — so the agent's content-key resolution (the app-pushed
  `content_key_bindings` blob until 2026-09-27; the agent's own custody read
  since, `on-demand-files.md` § Shared sets on a capability host → *One
  mechanism*) unions that second source (`resolve_foreign_engine_key_bindings`), each foreign entry carrying
  additive `FolderEngineKeys.{home_nest_url, channel_id_hex}`. Without this a
  bound foreign folder ran an **unbound** engine and sealed the user's edits
  under their own `BackupKey`, which the owner cannot decrypt — silent and
  wrong-keyed, the same defect class as the member-roster bug below, and the
  same breach of this doc's iron rule that binding a *reader* would be.
- **The agent builds a genuinely cross-nest engine.** Byte plane at the set's
  **home** nest under a `WriteTokenBearer` (its chunks live nowhere else, and
  the member holds no session there); control plane at the member's **own** nest,
  relaying — `set_foreign_routing` now carries `nest_url`+`channel_id` on
  `changes.list` as well as `changes.record`, because a foreign set has no rows
  in the member's own change log and the read half would otherwise poll an empty
  log forever.
- **The byte-plane HTTPS dial is authenticated even to a self-signed home**
  (the member holds no account there to graduate a pin the ordinary way). Before
  the agent dials it, `graduate_home_nest_pin` runs the pre-identity
  `fauna.auth.nest_handshake` against the home nest with the grant-delivered
  `home_nest_actor_id` (`IdentityRoot::PreResolved`) and pins the SPKI the
  byte-plane reqwest client then accepts — trust mechanics + the no-TOFU/hard-fail
  rules owned by `../architecture/security.md` § Transport trust (the
  federation-granted Axis-2 row). `home_nest_actor_id` rides the same two carriers
  as `access` (Welcome relay + `caller_access` read reply) into
  `ForeignFolder`/`FolderEngineKeys`; absent (a relay-unaware home) keeps the
  `RequireWebPki` floor, never weaker.
- **The bind gesture is verified, not optimistic** (D3). Since the rendered
  `access` is advisory and poll-refreshed, the add first spends one eager
  `fauna.folders.write_token.get`; the home nest answers from the roster row it
  wrote itself, and a typed refusal fails the bind loudly with nothing rendered
  or pushed. A stale-writer *row* is survivable; a stale-writer *binding* is a
  folder the user believes is syncing whose every edit is refused.

***Revocation — BUILT 2026-07-22, same-nest and cross-nest
alike, linux the rendering app; PROVEN end-to-end 2026-07-22 (task 6).*** A
mid-life demotion now parks the set instead of retrying forever:

- **One classifier, both planes.** A demotion folds differently depending on
  where it lands — the cross-nest relay passes the home nest's
  `fauna.federation.forbidden` through untouched, while same-nest
  `resolve_writable_folder` folds it into `fauna.sync.not_found` (ST-RES-1: a
  reader probing the write plane learns nothing a stranger wouldn't). Exactly
  those two codes park (`fauna_client_sync::is_access_revoked`); a
  `peer_nest_outdated`, a self-healing `device_unregistered`, and every transport
  fault stay retryable, since none of them asserts the grant is gone. Because one
  shape spans both planes, this is also the **same-nest** demotion behavior — the
  standing "nothing prunes the stale mapping on demotion" residual is retired,
  not solved cross-nest only.
- **The park is terminal and shared.** A one-way `AccessGate` is held by `Arc`
  between the engine and its byte-plane write-token bearer, because the bearer is
  built *before* the engine and a demoted writer meets the mint refusal on its
  first upload byte — before it has anything to record. Once flipped, the engine
  leaves its watch loop and refuses further records locally rather than re-asking.
  Previously **neither** refusal stopped anything: the bearer flattened the typed
  refusal into a transport error (so the byte plane read a revocation as a network
  blip and re-minted forever) and a refused record only logged a warning — both
  the silent un-sync this doc's iron rule forbids.
- **Durable, and visible.** The agent persists the park on the binding
  (`access_revoked`), drops it from the running engine plan, and reports it to
  apps on the folder row; linux renders `folder-access-revoked-warning` on the
  set (ID user-approved 2026-07-22), worded to say both halves — sync stopped, and
  **local files and pending local edits are untouched**. Persisting is what stops
  an agent restart silently resuming a binding the owning nest already refused.
  **The app watches the park; it does not wait to be told.** The park is a
  level the agent derives on its own, so the app folds it off the agent's
  location list on its status poll (`LocationBindingsModel::fold_parks`, beside
  the mass-delete hold's `fold_engine_holds`) — the binding reconcile, which
  mirrors it too, runs only on a mutation or a reachability edge, and no gesture
  of the demoted member's produces either. **The warning and the parked binding
  stay on the member's row whatever the row's access now reads**: a demotion is
  exactly the change that turns it to `reader`, and a row that asked the access
  alone painted neither on the first folder-list refresh after the park — the
  moment the user looks. Which rows carry the binding section, and when the
  warning heads it, is one shared decision,
  `fauna_folders_machine::binding_section` (owner or writer ⇒ bound; any parked
  binding ⇒ shown and warned).
- **Recovery is a re-bind**, which clears the park and re-runs the eager
  bind-time mint verify (D3) — so a re-granted writer is one gesture away, and a
  still-revoked one is refused loudly at that gesture rather than parked again in
  silence.

***PROVEN end-to-end 2026-07-22 — the two-nest client-stack
capstone.*** `bins/fauna-sync-agent/tests/cross_nest_agent_capstone.rs` (tier_3,
unix, `--features tier3-nest`) drives the whole chain against two real nests and
the **real `fauna-sync-agent` process**: owner shares `writer` cross-nest → the
grant survives the federation relay into the member's sealed custody → the
agent resolves the foreign set from the member's own custody (foreign union
included; the member's app computed and pushed the blob until 2026-09-27) → the agent, provisioned over its real IPC socket, builds a *serving*
foreign-routed engine → the owner's file materializes in the member's bound
folder → the member's edit relays back and the owner opens it byte-for-byte
under the shared M2 key, nest-stamped to the writer → a demotion parks the set,
surfaced as `LocationInfo::access_revoked` with local files untouched. What it
deliberately does not cover, and why, is in that file's module docs (the linux
GUI render of the revoked row, unit-pinned by task 4; TLS, see the
cross-nest byte-plane trust gap below; and post-bind live delivery to the
member — a cross-nest member has no nudge and rides the rescan cadence, so the
promise the capstone asserts is the eager first pull of a pre-bind seed).
*Re-proven and gated 2026-08-20:* the suite sat red-and-unwatched from the S9
flip (its owner-side matcher keyed on resting plaintext `path`, which sealed
planes no longer serve) plus an upload-after-bind ordering race — both
test-side, fixed per its module docs § *Order is load-bearing* — and it now
runs in the nightly `sync-agent-tier3-nest-check`
(`merge-gate-catalog.md` § The heavy gate catalog owns the gate record). Per-app fan-out beyond linux inherits
all three legs, and a client that adopts the writer-binding row without them must
keep withholding the widget for a row carrying a `home_nest_url` — offering a
bind the engine cannot honour is the reader's situation exactly.

**Two cross-nest gaps the capstone surfaced (one since closed, one open;
neither a correctness break):**

- **A foreign set's scan cadence is not readable at all** — CLOSED: the home
  nest now stamps the cadence on the Welcome/`caller_access` carriers and the
  agent applies it (§ Config's *"A cross-nest member reads the cadence off the
  federated carrier"* paragraph owns the mechanism; and the phase-5 de-knob
  target retires the choice entirely, leaving the constant).
- **A self-signed home nest's byte plane cannot be trusted by a cross-nest
  member.** The member's byte plane dials the home nest *directly*, but the whole
  cross-nest design relays every control-plane call through the member's own
  nest — so no handshake ever graduates an SPKI pin for the home authority, and
  `store_pinned_reqwest_tls`'s `RequireWebPki` policy refuses a non-WebPKI cert.
  Cross-nest sync therefore works today only where the home nest has a
  publicly-chaining cert. Both gaps are tracked.

**Linux writer location-binding BUILT (2026-07-20, Phase 1 completion); the
empty-content-key-blob agent regression fixed 2026-07-21 and CONFIRMED
end-to-end 2026-07-22 (see the block at the end of this subsection — closing it
took a second fix on the member side).** A shared-with-me set
the caller holds an `access == "writer"` grant on is now bindable in the linux
app. The nest projects `access` onto the member's `FolderSummary` (from
`folder_member_access`); `decide_engine_content_binding` resolves the content
key from the **writer's own custody** (a writer seals under `current` exactly as
the owner does — no new crypto); a **reader** is refused there (fail-closed — no
read-only mirror in v1; the one host that builds a reader's set, read-only, is
the control-inverted on-demand host, which has no directory a user could edit —
[`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host,
decision 3). The linux row splits on the grant: a writer member
renders the `folder-location-*` binding UI (an `ExpanderRow`), a reader stays
read-only. **The owner-side regression (found 2026-07-20, fixed 2026-07-21):**
`decide_engine_content_binding` is driven only by the apple in-process engine
hosts; linux's live engine host is the external **sync agent**, and the app had
provisioned it with an empty `content_key_bindings` blob, so through the
production agent path a bound M2 set — the owner's own shared set included — ran
an **unbound** engine sealing under `BackupKey` (regression live since the A3
agent cutover, 2026-07-19). **Fix:** the linux app now resolves the per-set
content-key blob from its `__config` custody
(`fauna_client_folders::compute_engine_key_bindings_blob` — the single producer
windows drives over the `folder_engine_key_bindings` FFI, priority #2) and
pushes it into the `SyncCapability` at provision **and on every custody-changing
event** (folder bind, content-key rotation) via
`sync_agent::refresh_content_keys` → `SyncAgentProvisioner::set_content_key_bindings`;
the shared convergence loop re-provisions the agent on the resulting
`needs_reprovision`, so the agent re-keys the engine (its engine-stamp machinery
restarts an engine whose key material changed). Mechanism authority:
`../architecture/mls-group-key-material.md` § M2 / § Implementation status (the
5d(c) push, now consumed by linux like windows). *(Superseded 2026-09-27: the
agent resolves every set's keys from its holder's custody itself, re-reading at
its own edges, and the app-side push is retired on every app —
`on-demand-files.md` § Shared sets on a capability host → *One mechanism*.)*

**Confirmed end-to-end 2026-07-22 — the client-path pin closed it, after
surfacing a second, distinct wrong-key bug.**
`tests/e2e-unified/tests/test_folder_agent_content_sync.py::
test_writer_member_decrypts_owner_upload` drives the production path with two
concurrent linux GUI apps and their two real sync agents, and is **green**:
the owner's agent uploads under the set's M2 `current` key and the writer
member's agent materializes the plaintext into their own folder. Reaching green
needed one more fix, and it was the **mirror image** of the regression above —
member-side rather than owner-side. The shared blob producer
(`compute_engine_key_bindings_blob`) resolved its roster from the **owner-scoped**
`fauna.folders.list`, which by construction omits shared-with-me sets, so a
*member* had no entry in the pushed blob at all; the agent read `(None, None)` =
unbound and the member's engine tried its **own** `BackupKey` on the owner's
M2-sealed chunks. Like the owner-side regression this was **silent, not
fail-closed**: an unbound engine holds key material, so it reaches the AEAD and
fails there instead of refusing. The producer now uses `list_owned_and_shared`,
which is what the apple in-process host
(`fauna-sync-engine::engine_lifecycle::fetch_folders_retry`) had always done —
the agent path was the drift, and windows got the fix for free through the same
producer behind the `folder_engine_key_bindings` FFI (since 2026-09-27 the agent
runs that producer itself, `fauna_client_folders::resolve_engine_keys`). **Two sibling sites
carried the identical owner-scoped roster and are fixed with it:** the agent's
`rescan_interval` (a member's row was never found, so every member polled at the
`DEFAULT_RESCAN_INTERVAL` of 300 s regardless of the cadence the owner chose —
measured exactly, and previously mis-triaged as design-consistent; it was this
bug, and the fall-through is invisible because it is also the legitimate
nest-unreachable path) and the standalone `fauna-sync` daemon's folder policy
read. A same-nest member's inbound pull is now push-nudged **plus** timer-backed:
since the nest fires `PushEvent::SyncChanged` on every record so a connected
member pulls within seconds (§ *Remote-change nudge*), with the owner's chosen
cadence as the correctness backstop. A **cross-nest** member's inbound pull
remains timer-only (the cross-nest nudge is a named candidate, not built).

**Same-nest two-engine round-trip capstone BUILT 2026-07-20.** A tier_3 test
(`conformance_shared_folders.rs::writer_member_and_owner_round_trip_edits_both_ways_two_engines`)
drives a real owner engine + a real writer engine over the wiremock chunk plane: a
writer's edit round-trips into the owner's replica (record lands under the
`writable_folder` gate, charges the owner, and the owner opens the writer's chunks
under the shared content key from the writer's own custody) **and** an owner edit
round-trips back, with the writer's version nest-stamped to the writer.

**Writer-engine live pins BUILT 2026-07-20.** The D3/D4 writer-engine consequences
are now pinned end-to-end over a WS-served nest (the writer's *own* connected engine
drives `changes.record` + `changes.supersede` against the genuine gate), in
`conformance_shared_folders.rs`:
`writer_drain_across_rotation_tolerates_owner_only_supersede_refusal_two_plane`
(D3 — a writer crosses a rotation with a pending upload: the drain requeues under the
current generation, the re-record lands, and the owner-only `changes.supersede`
refusal is tolerated so the engine never wedges) and
`writer_below_floor_record_refused_then_heals_after_reingest_two_plane` (D4 — a
below-floor writer record is refused typed `stale_content_key`, then heals: after the
re-ingest advances the writer's current generation, the ordinary drain re-records past
the floor). The eviction corollary of Success (e) — an evicted writer's role row is
deleted so their next `changes.record` is refused (reader default) — is pinned by
`evicted_writer_member_cannot_record_role_row_deleted_flips_to_reader_real_router`.

**Still open (per-app UI fan-out is owned by `../ui/folders.md` § Sharing,
don't restate its table here):** the SAME-NEST per-app writer-binding fan-out
closed 2026-08-26 — every app with any location binding to gate (linux, macOS,
tui, windows) renders it; android/web/iOS are N/A by construction, ruled
2026-08-15 in the owner doc. Still open: the CROSS-NEST writer app leg on the
six apps other than linux — linux's own leg (discovery, bind, revoked-park) is
BUILT per the § status above, leaving only its Media resolver open, per
`../ui/folders.md` § Sharing's own status line.

**The revoked-park row after the demotion (2026-09-22).** The first app-driven
witness of *Revocation* (`test_folder_writer_revocation.py`) found the warning
unreachable on both apps it ran on, for two reasons stacked: the app learned the
park only on a binding reconcile, which a demotion never triggers; and the
member's row keyed on the current access alone, so even a learned park vanished
on the refresh after the demotion. tui and linux now watch the park on their
status poll (`fold_parks`) and ask `binding_section`; macOS does both since
2026-09-25 (its 10 s agent-status tick folds the park through the UniFFI
`FfiLocationBindingsModel::fold_parks`, and the shared `FoldersContent` asks
`bindingSection`), and windows too since 2026-09-25 (its ~10 s agent-status
tick folds the park through the same UniFFI `fold_parks`, and
`FoldersPage.xaml.cs` asks `BindingSection` for every member row, rebuilding
the rows when a park comes or goes).

## Conflicts

**Moved to [`conflicts.md`](conflicts.md)** (2026-08-02 split) — the auto-resolve +
version-retention model (text three-way merge when clean, else latest-wins, losers retained,
review-list-not-chooser), per-set conflict policy, delete-vs-edit, concurrent resolution &
ancestor freshness, and the legacy candidate/choose-winner wire substrate.

## File Versions

**Moved to [`file-versions.md`](file-versions.md)** (2026-08-02 split) — every recorded change
IS a version: the projection over `sync_changes`, the `fauna.files.versions.*` wire surface,
restore-as-re-point (+ the recording device's local apply and its three orderings), retention
posture, and the per-surface implementation status.

### Sealed names & paths

**Moved to [`path-sealing.md`](path-sealing.md)** (2026-08-02 split) — the `SealedLabel`
envelope + sealed sibling columns, the `fauna.path.v1` derivation + nonce modes, hash
companions, and the expand→migrate→contract staging (ruling + exemptions stay in
`../architecture/encryption-at-rest.md` § Carve-outs).

## Content-Addressed Storage

All file data is stored and transferred as **chunks** identified by BLAKE3 hashes of their content.

```
encode_blob(chunk_bytes)
  → compress (zstd)
  → encrypt (ChaCha20-Poly1305 — see Confidentiality below for which key)
  → stored in blob store keyed by BLAKE3(plaintext_chunk)
```

Key properties:

- **Deduplication** — identical content across different files or versions is stored exactly once. Only the manifest differs.
- **Integrity** — any corruption in transit or at rest is detected when the client verifies the BLAKE3 hash of the reassembled chunk.
- **Confidentiality** — chunks are encrypted before leaving the device; the nest holds ciphertext only. **The one deliberate exception is a `public`-audience folder** (folders re-model phase 4, 2026-08-17): its owner explicitly declassified it, so its uploads rest as **plaintext chunks** (`stored_hashes = None`, the self-describing shape every reader passes through) with plaintext labels — world-readable by ratified design (`../architecture/key-material-hierarchy.md` § Audience: the public; the invariant exception is owned by `principles.md` § The user always controls their data). The arm is selected by the armed `SyncEngine::with_public_audience` flag off the folder's projected `audience` — never by a missing key, **and (ratified 2026-09-21) never off the projection alone: the flag arms only on the owner's verified audience attestation**, whose mechanism, trusted-owner rule, replay floor and build status are owned by [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Readable classes → *The declassification is owner-ATTESTED*, so wherever this bullet says a seat acts "off the projection" read "off the projection, once verified"; the keyless fail-closed bails below stand for every non-public engine, and the owner-only re-seal pass no-ops on a public engine rather than sealing the site dark. **An audience flip moves the back-catalogue too** (`SyncEngine::converge_corpus_to_audience`, run at every catch-up **and on every rescan tick**, directly after the `refresh_sync_mode` that installs the audience it reads — the same pairing its `converge_corpus_to_website` sibling has, so a flip reaches a running seat's back-catalogue within one tick instead of at the next process start; corrected 2026-08-21): each device converges its corpus when the device-local `corpus_audience` marker disagrees with the projected audience — declassify re-records sealed entries plaintext (clearing their `owner_sealed` markers, whose stale survival would otherwise strand paths plaintext after a flip-back), the flip-back drops all markers and re-seals — dispatching on bound-ness: a bound folder re-seals under its current content key via the version-mismatch pass (`reseal_pending_under_current`), an unbound one under `BackupKey` via the owner-only pass, both off the projection alone so every seat, members' included, converges (2026-08-20 — no custody sentinel; the sentinel-staging audience door is deleted). Absent marker = the legacy sealed shape, so never-flipped folders pay one meta-row read. (The `bins/fauna-sync` daemon took the same arm until its removal, 2026-10-02.) The flag is **live on a running seat**: it rides the same per-tick `fauna.folders.list` read as the sync mode (`config::SeatResolution`), so a flip reaches the write path within one tick in both directions. For everything else the encryption key is selected per-engine-configuration from the two paths defined across [`owner-key-material.md`](../architecture/owner-key-material.md) § Path A and [`mls-group-key-material.md`](../architecture/mls-group-key-material.md) § Audience: an MLS group: **Path A — `BackupKey`** (identity-seed-derived; the live path for the owner-only fauna-sync content kinds — backups, fauna-native compose drafts, user-private contact-record contents, and ordinary single-user file chunks), or the **`chunk_crypto`** per-chunk key rooted in a bound set's M2 content key (live end-to-end for cross-user-shared folders; per `mls-group-key-material.md` § Audience: an MLS group). A **WebDAV-served** set (ratified 2026-07-06; built — `webdav-server.md` owns status) is a third selector case: it is M2-content-key-sealed like a shared set — that is what lets the MDA bridge serve it per AUTH'd DAV session without ever holding `BackupKey` — so flipping an existing `BackupKey`-sealed set to served runs a one-time client-side re-seal migration; authority `docs/goal/behavior/webdav-server.md` § Key model. Both paths seal chunks through the same **convergent** `chunk_crypto` primitive (ChaCha20-Poly1305; deterministic per-chunk key+nonce off the chunk's **content hash**) — they differ only in the root (a bound set's M2 content key vs. the owner's domain-separated `BackupKey::convergent_chunk_root()` — FS-BIND FOLLOW-ON A, 2026-07-07) and hence in which audience can read the result; the keying mechanism + the ratified equality-visibility property are owned by `owner-key-material.md` § Path A / `mls-group-key-material.md` § M2 *At-rest blob keying*. **The owner-only seal is unconditional — never a knob** (until 2026-07-13 every production embedder passed `backup_key = None` and owner-only chunks rested in plaintext, on the retired claim-time-trust premise): the engine fails a keyless owner-only upload closed, linux derives `BackupKey` from the identity seed and runs the one-time owner-only re-seal pass (`SyncEngine::reseal_owner_only_plaintext` — the same trio as the § Apple *Migration for existing plaintext records*, generalized), and the read path stays manifest-driven so the legacy plaintext back-catalogue keeps opening while it converges. The `bins/fauna-sync` headless daemon seals its own cache/push pipeline too (2026-07-13): `run_ws_session` derives the owner `convergent_chunk_root()` and threads it into the WS client, so `chunk_and_cache` → `push_cached_file_to_nest` seal every chunk (ciphertext store keys + `stored_hashes`) and its read/apply + `restore` paths open manifest-driven; the first post-upgrade rescan re-seals every head (the natural migration — the daemon's ws pipeline records no per-path state, so the legacy plaintext heads it re-records land sealed). No owner-only folder writer rests plaintext.
- **Efficient delta** — unchanged chunks in a modified file are not re-uploaded; only new or changed chunks are sent.
- **Store key** — a chunk's content-address is `BLAKE3(plaintext_chunk)` for plaintext chunks, but `BLAKE3(ciphertext)` for **every sealed** chunk — content-key (bound-set) AND owner `BackupKey` chunks alike (recorded in the manifest's parallel `stored_hashes`) — so the AEAD body satisfies the nest chunk route's F9 anti-poisoning check with no route change (proven end-to-end against the **real** `/api/v1/chunks` route by `bins/fauna-nest/tests/conformance_content_key_chunk_route.rs`: a bound-set content-key upload and an owner backup `upload_bytes` pass are both accepted, served back, decrypt, and the superseded plaintext-hash pairing is rejected `400`; a manifest with no `stored_hashes` is **plaintext, unconditionally** — the once-hypothesized legacy framed-seal corpus provably never rested anywhere, its uploads were the F9-rejected breakage — so a keyed reader passes a plaintext manifest through and fails closed, loudly, on a sealed manifest it has no key for). **Every** store-addressing site addresses the store by this key — upload, dedup `check_chunks`, the nest forward + server-reassembly paths, **and the client-side sync-engine drain/resume worker** (an interrupted content-key upload re-uploads its queued chunks under the ciphertext store key rather than silently dropping them). Full mechanism: [`mls-group-key-material.md`](../architecture/mls-group-key-material.md) § M2.

- **Manifest privacy** — a sealed-chunk manifest carries its plaintext `file_hash`/`chunk_hashes` only sealed, beside the plaintext `stored_hashes` (no destination-side confirmation oracle); which fields seal, under which root, every writer and the reader's refusal of any other shape are owned by [`mls-group-key-material.md`](../architecture/mls-group-key-material.md) § M2 *Sealed manifest hashes*.

A **manifest** is an ordered list of chunk hashes. To reconstruct a file:

```
for hash in manifest.chunks:
    chunk = blob_store.get(hash)
    verify BLAKE3(decompress(decrypt(chunk))) == hash
    append to output
```

**Read is the exact inverse of write, and both stages are mandatory.** Encode is
`compress -> encrypt`, so decode is `decrypt -> decompress` — in that order
(`libs/fauna-core/src/compress.rs` § Pipeline order owns the statement; this
line read `decrypt(decompress(…))` until 2026-07-31, inverted). Dropping the
decompress stage does **not** fail loudly at the AEAD: it yields
`0x00 ‖ plaintext` and fails the *hash* check one layer later, so the symptom
points at content rather than at the reader. That is precisely how the
`bins/fauna-sync` daemon shipped a reader missing the stage — and a writer
missing the matching `compress`, which kept its own write→read loop green and
hid the asymmetry from every daemon-only test. Its cost while it stood: the
daemon could not open **any** file an app had uploaded (every catch-up apply
failed the whole-file verify), it produced different ciphertext from the apps
for identical content (so convergent dedup broke and one file had two manifest
hashes across an owner's devices), and `restore` — which verified nothing —
wrote the framed bytes to the user's disk. Fixed 2026-07-31; the daemon now
verifies **per chunk** against `manifest.chunk_hashes[i]`, which is also what
told a framed chunk from its own pre-fix unframed back-catalogue, since a
leading `0x00` is otherwise ambiguous by inspection (that raw fallback,
retired 2026-09-25 with the compat-remnant sweep, was restored 2026-09-27 as a
live-writer read, not a remnant: the nest's first-writer-wins store also takes
raw bodies under their plaintext hash, so a raw pre-seed of a public chunk must
stay readable — `compress::unframe_verified_chunk` reads framed first, then raw,
and serves a body only when it addresses the recorded hash;
`compat-remnant-sweep.md` § Program 4, tranche B3 (i)). **The same divergence is
not only a dedup/interop break — it is a keystream reuse** (read 2026-09-03,
when the Go WebDAV MDA turned out to be a second raw writer against the
engine's framed one): the chunk plane's (key, nonce) derives from the
*unframed* hash while the AEAD encrypts the *framed* body, so two writers
framing one chunk differently rest two plaintexts under one nonce, and the
XOR of the pair recovers the user's bytes with no key. The writer side is now
one door — `fauna_core::chunk_seal`, the only path to the AEAD, used by the
engine, the MDA and the e2e agent alike (and by this daemon until it was
removed, 2026-10-02) — and the apps' shared walk
(`file_download::open_chunk_window`) takes the same framed-first,
raw-fallback, hash-decided read; `mls-group-key-material.md`
§ Per-chunk file-sync key owns the invariant.

**The whole-file verify belongs to the reassembly point, never to its callers.**
It is not subsumed by the per-chunk check: that proves each chunk addresses
`chunk_hashes[i]`, never that the manifest's own chunk *list* reassembles to the
`file_hash` it advertises (wrong order, a dropped or duplicated entry, a
truncated `chunk_hashes`). Siting the obligation at the callers was tried and
failed silently — the daemon's `open_cached_chunks` documented that "the
whole-file hash verify in every caller anchors integrity" while **two of its
three callers** did no such check: only the `ApplyChange` arm verified, while
both *conflict* arms (whose bytes are three-way merged, written to disk **and**
uploaded) did not. An obligation most callers skip is not an invariant. From then until its
removal (2026-10-02) `bins/fauna-sync` verified at its one reassembly point
(`reassemble_verified`, covering all three callers) and `cmd_restore` verified
per file (`reassembly_is_intact`), where a bad file is skipped and named rather
than aborting the whole restore — so one corrupt entry cannot deny the user
every good file after it. General rule: **a mandatory pipeline stage belongs where it
cannot be forgotten.**

**At-rest framing is self-describing and symmetric on read.** The nest's
`encode_blob` (`bins/fauna-nest/src/backup/mod.rs`) always frames a stored blob
with a one-byte compression prefix (`0x00` uncompressed / `0x01` zstd, from
`fauna_core::compress`) — *including* the `compress=false` path — so `decode_blob`
is its exact inverse for every `(key, compression)` config and a chunk whose first
plaintext byte is `0x00`/`0x01` never gets mis-stripped. The blob store is
therefore **uniformly `encode_blob`-format, and every reader decodes** — the
chunk relay's `ChunkResolver` (§ 4), the snapshot/restore path, *and* the bulk
download routes `GET /api/v1/chunks/{hash}` + `GET /api/v1/manifests/{hash}`, which
strip the at-rest framing before returning. Because `decode_blob ∘ encode_blob` is
the identity, a client gets back byte-for-byte what it uploaded and applies its own
`decrypt(decompress(…))` (above) on top — the nest's at-rest framing is transparent
to the client. (A download route returning the stored bytes *verbatim* would leak
the nest prefix into the client's decode; do not reintroduce that.)

### Chunk size and transport contract

The chunker (`libs/fauna-core/src/chunker.rs`, shared by every app, the sync
agent and backup) guarantees a **maximum chunk size** of `MAX_CHUNK = 8 MB`. No chunk it
emits — by either the FastCDC path (`MIN_CHUNK = 512 KB`, `AVG_CHUNK = 2 MB`,
`MAX_CHUNK = 8 MB`) or the single-blob path for files below
`SINGLE_CHUNK_THRESHOLD` (held **equal to** `MAX_CHUNK`) — ever exceeds 8 MB. Two
load-bearing reasons:

1. **Tier coherence.** Each tier defines a "max blob" limit (Free 10 MB / Personal
   100 MB / Community 500 MB; see the FAQ below and `backup-restore.md`) that
   applies to individual chunks/manifests. The chunk size is **tier-independent**
   (a tier-dependent max would fork the same file's chunk boundaries per tier and
   break content-addressed dedup), so it is held ≤ the *smallest* tier limit — every
   chunk is therefore a storable blob on every tier.
2. **Single, bounded transport.** Because chunks are bounded, the nest transport is
   sized once for them:
   - **`POST /api/v1/chunks` + `/api/v1/manifests`** receive the raw chunk/manifest
     body and carry a `DefaultBodyLimit` of 10 MB (above axum's 2 MB `Bytes` default,
     matching the smallest tier limit, with headroom over an 8 MB chunk plus
     compression framing).

   No WebSocket carries a chunk: the bearer WS-RPC connection keeps the nest's
   2 MiB `MAX_WS_MESSAGE_SIZE` security bound (2026-06-01 review § D3), and the
   relay's answer returns on the `/chunks/relay/{request_id}` byte route under the
   same body limit. (The legacy `/sync/ws` data plane carried whole chunks in
   DAG-CBOR frames under a 10 MB bound of its own until its removal 2026-10-02;
   reusing the 2 MiB cap there once broke every file over ~2 MB.)

### Path hashing

`path_hash` is the BLAKE3 hash of the normalized file path, used as the stable path key on the hash-keyed surfaces (`fauna.files.versions.*` replies carry no plaintext path) and as the convergent salt for the `path` field's `SealedLabel` envelope. It is **not** itself a privacy seal — the hash companions are deliberately unkeyed, by design (`path-sealing.md` § Hash companions join the floor); the plaintext `path` column's at-rest exposure, the 2026-07-29 paths-are-content sealing ruling, and its current status (the S9 flip scrubbed the nest-side plaintext once a sealed sibling exists) are owned by [`path-sealing.md`](path-sealing.md) + `../architecture/encryption-at-rest.md` § Per-content-kind conformance + § Carve-outs. **Normalized** means the folder-relative path with **forward-slash** (`/`) separators — never the OS-native separator. This is mandatory for cross-app agreement: the same file must hash identically on every platform, so a Windows app must convert `\` to `/` when deriving relative paths. The derivation lives in shared Rust — never per-app.

Both halves have exactly one owner, in `fauna_core::sync`: `normalize_rel_path` (OS separator → `/`) and `path_hash` (BLAKE3 over the normalized bytes, pinned by a known-answer test — changing that digest breaks every stored `path_hash`, so it is a data migration, not a refactor). Every derivation site calls them: the nest's record paths (`sync_handlers`, `bridge_blob_handlers`, `sync_storage`), the client half (`fauna-media-machine`'s `nest_api::ws_rpc`), and `fauna-sync-engine`'s watcher `normalize_rel`, which delegates to the shared normalizer so its scan and event paths cannot diverge. `fauna-core` reaches the nest, the sync engine, the Windows sync-service, wasm (web) and UniFFI (native), so no platform ever needs to re-derive. Never inline `blake3::hash(path.as_bytes())`.

### Write-path containment (a materialized row never escapes the sync root)

Every door that materializes a **remotely-authored** change row — the nest-pull download, the peer-share ingest, and (for uniformity) the own-row self-echo, plus the snapshot restore walk, now ONE implementation (`fauna_sync_engine::engine::restore_snapshot_walk`, driven by the FFI/macOS surface via `SyncEngine::restore_snapshot_files_to_dir`; the removed daemon's `cmd_restore` was its second host until 2026-10-02) — writes to `root.join(relative_path)`. The relative path is attacker-influenced on the two remote doors, so the write must stay under the sync root. **Two guards, in order:** the lexical `fauna_core::path_guard::is_safe_relative_path` rejects `..` / absolute / empty by string shape (cheap, pre-fetch), and `fauna_core::path_guard::resolved_target_within_root` resolves the target against the filesystem and refuses it when a **symlinked intermediate directory** in the victim's tree would redirect the write outside the root — the case a lexical check cannot see, because it never touches the disk. The resolved guard walks the deepest *existing* ancestor and runs **before** `create_dir_all`, so an escapee never even gets a directory created beyond the root; a refusal skips (and accounts for) the one row, never aborts the page. There is **one** containment implementation, `resolved_target_within_root`, shared by all five write doors — the per-door inline copies that preceded it have been deleted, and a door reaching containment any other way is the shape to reject in review. The final path component is deliberately *not* canonicalized — the target legitimately may not exist yet, and a **dangling** symlink planted at that name defeats `canonicalize` regardless (it fails, so the ancestor walk falls back to the root and permits the write; a *resolvable* final symlink, by contrast, is caught). That last hop is contained by the doors instead: **all five** end in a `rename` — `atomic_write_file`'s temp + fsync + rename, or the streaming path's equivalent `persist` — which replaces a symlink rather than following it. A door ending in a direct `std::fs::write` does **not** get that containment and would have to canonicalize the final component itself; the two restore doors were the only ones still writing directly, and both moved onto `atomic_write_file` (which also makes restore crash-safe, which it was not). **This is the invariant to preserve when adding a door: guard, then rename — never a direct write.** Owner: `libs/fauna-core/src/path_guard.rs`.


## Third-party deposit ingress (ratified 2026-09-05; nest half built 2026-10-03, adoption built 2026-10-05)

*The third-party integration chain's folder half — the owner of the **deposit** and its adoption. The bearer door on the DAV server and the read grant twin are [`webdav-server.md`](webdav-server.md) § Independent enablement → *Third-party read + deposit*; the page facet [`../ui/folders.md`](../ui/folders.md) § Audience and website serving → *Third-party deposit and read*; the keyless `deposit` class [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Capability tiering → *Third-party holders*.*

**Deposit is write-only and carries no key.** A third-party principal holding `fauna:folder:deposit:<id>` posts a file to `/api/v1/folders/{id}/deposit` (remote servers — classified as HTTP residue by [`../architecture/api-layers.md`](../architecture/api-layers.md); a hosted principal deposits over WS-RPC without the HTTP door). The nest seals it **on the user's behalf** to the user's registered recipient key through the one resolver the mail ingest sites already share (the D2 seal-key resolver, [`smtp-server.md`](smtp-server.md) § Inbound pipeline; [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § The sealed posture) and parks it in the folder's **inbox segment** — a per-folder holding area that is not yet a folder entry. The principal holds no key, sees no folder content, and learns nothing but "accepted".

**The door, as built (2026-10-03).** Both forms carry one file — a name, a declared media type, at most `MAX_DEPOSIT_BYTES` (1 MiB, under the WS-RPC message cap so the two doors accept the same deposits; `fauna_protocol::folders`) — and run one gate (`bins/fauna-nest/src/folder_deposit.rs`): the HTTP form admits the DPoP-bound token exactly as the principal session's upgrade does and dispatches the same kind, `fauna.folders.deposit`. After the `ThirdParty` ceiling and the arm-wide scope check, the handler requires, in order: a name that is one plain file-name component (`is_deposit_name`, shared with adoption); one of the session's scopes naming **this** folder; a LIVE keyless `deposit` grant over this folder, owned by the account and held by the key the principal attested — the audit + revocation record, re-resolved at every deposit, so revoking the grant ends deposits as revoking the principal does; a folder the account owns; and a residency that is not metadata-only. Every refusal but the last answers the same `fauna.folders.permission_denied`, so a depositor never learns whether a folder it does not hold exists; the metadata-only refusal names its reason (`ui/folders.md` § Audience and website serving → *Third-party deposit and read* owns the refusal pairs — a `public` folder is **not** one of them for deposit). The sealed payload is a `DepositEnvelope` (name, media type, bytes — all content, so all inside the seal), sealed through the recipient-blob seal the nest's other seal sites use, and the inbox segment is the `folder_deposit_inbox` table: per item the folder, the depositor's principal id (kept past a revoke — an accepted item is the user's), the sealed blob, its size and the arrival time. A deleted folder takes its unadopted items with it, as it takes its files. The reply is `{accepted: true}` (HTTP `202`) and nothing else.

**Adoption is the user's next client sync.** An owner's seat of the folder opens the parked item with the owner's recipient key, re-seals it under the folder's own convergent scheme (§ Content-Addressed Storage — `BackupKey`-rooted for an unbound folder, the M2 content key for a bound one), and records it as an ordinary change row through the same remotely-authored-change door every other ingest uses — guard, then rename, never a direct write (the invariant § Content-Addressed Storage's last paragraph states). From then on it is a file like any other: versioned, synced, served. This is the mail pattern applied to files: sealed at the perimeter to the user, adopted by the user's own device.

**Adoption, as built (2026-10-05).** Two owner-only kinds beside the door, `fauna.folders.deposits.list` (a page of the folder's parked items, sealed as they rest, oldest first) and `fauna.folders.deposits.retire` (drop one by its id; `false` when already gone), both refusing a folder the caller does not own as not found. The engine pass is `SyncEngine::adopt_deposits` (`libs/fauna-sync-engine/src/deposit_adoption.rs`), armed at build only on a set the account owns on its own nest (never a member's, a reader's or a cross-nest set's engine: only the owner's recipient secret opens the item), and run by the resident root on entry, on every rescan tick and on the sync nudge the door sends the owner's devices alone. Per item: open it with the recipient secret derived from every MSEK the mail custody holds (`fauna.state.mail`, current then the retained priors), through the X-Wing opener, which opens the classical degrade too; land it under its own name, or under `<stem> (deposit <id>)<ext>` when another file holds that name, so every seat derives the same target; write it through `contained_apply_target` and `atomic_write_file`; record it with the ordinary own-change upload, which seals it under the folder's scheme; retire it once the path's recorded head is exactly the item's bytes. A seat that finds the target already holding those bytes (another seat adopted it, or a crash fell before the retire) writes nothing and only records if owed, then retires, so adoption is idempotent on the deposit id. A host whose custody source holds no mail custody (a capability host's cold replica) adopts nothing; another of the owner's seats does.

**Status:** the nest half is built (2026-10-03): the inbox segment, both doors, the gate and the seal above, proven by `folder_deposit`'s unit tests (the parked item opens under the owner's recipient key; every refusal) and the tier_3 `tests/e2e-unified/tests/api/test_folder_deposit.py` (both doors over a real consent, nothing of the file in clear anywhere on the nest's disk, not listed by `fauna.sync.files`). The `deposit` class string is minted ([`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Implementation status today). **Adoption is built (2026-10-05)** as above, proven by `deposit_adoption_test` (lands, retires only once durable, a landed item only retired, a taken name, the classical degrade and a prior MSEK, an unopenable item stays parked, an unarmed engine never lists), the nest's `folder_deposit::tests` for the two kinds, and the tier_3 `tests/e2e-unified/tests/test_folder_deposit_adoption.py` (a real consent and deposit, two tui seats of the owner, the file on both disks once, the inbox drained, nothing in clear on the nest's disk). **Unbuilt:** adoption on an on-demand root and in the one-shot (iOS) pass, which run no `adopt_deposits` yet; an end-to-end proof of adoption into a bound folder (the seal path is the ordinary upload's); the approving app's consent-time mint of the `deposit` grant (today only the e2e harness mints it); a deposit larger than one message. Sequencing: [`../architecture/third-party.md`](../architecture/third-party.md) § Implementation status today.

## Reserved folders

**Moved to [`reserved-folders.md`](reserved-folders.md)** (2026-08-02 split) — the `__*`
convention (implicit creation, list exclusion, namespace refusal, the rail-mode heal), the
`high_cadence` flag, and the per-rail at-rest channels (`__config`, retired 2026-10-02 with
the name kept reserved / `__drafts` / `__mls` / `__mail` + siblings / the placement journals).

## Implementing Sync on a New Platform

A native app that runs the engine **in-process** (Linux today) should link the shared
`fauna-sync-engine` crate (`libs/fauna-sync-engine`) rather than reimplement the steps below — the
crate already provides the watcher, chunk pipeline, conflict-aware apply, and state DB. The checklist
documents what that engine does end-to-end (and what a non-Rust app, e.g. a future C#/Swift
in-process port, must replicate). Management (steps 1, 9 — registration, folders, conflicts) always
routes through the nest per the § Control Plane Principle.

Checklist for adding file sync to a new app:

1. **Device registration** — call the `fauna.sync.register` WS-RPC kind after authentication. Generate a random UUID as `device_id` and persist it locally.
2. **Connection** — sync rides the account's bearer WS-RPC connection (§ 2. Connection); there is no sync-specific socket.
3. **File watcher** — use a platform-appropriate watcher (inotify, FSEvents, ReadDirectoryChangesW) to detect changes. Emit folder-relative paths with **forward-slash** separators (see § Content-Addressed Storage on `path_hash` normalization) — on Windows, convert `\` to `/`. **Where the platform gives you no watchable path, do not invent a bespoke upload path** — that is what a *library ingress* is for (§ Apple apps — convergence design, library-ingress bullet): stage the bytes you exported to a real file and hand them to `FfiSyncEngineHost::ingest_file`, which runs the ordinary sealed pipeline with no watch dir. This covers an OS photo library (PhotoKit, MediaStore) and android's SAF `content://` tree bindings alike. (Corrected 2026-07-16 — this line previously cited "android's observer-based `WatchedDirectoryManager`" as a watcher example; android has no watcher, and its bespoke scan is the § Control Plane Principle drift, not a model to copy.)
4. **Chunk pipeline** — split changed files into chunks, compute BLAKE3 hashes, skip already-uploaded chunks, upload new ones via `POST /api/v1/chunks`.
5. **Manifest upload** — assemble and upload the manifest via `POST /api/v1/manifests`.
6. **Change notification** — record the change with the `fauna.sync.changes.record` WS-RPC kind.
7. **Incoming changes** — on the remote-change nudge and each rescan, pull the `fauna.sync.changes.list` WS-RPC kind since the persisted anchor (`since=N`), fetch each change's manifest and chunks from the bulk byte routes, and write the files to disk.
8. **Offline catch-up** — the same pull on reconnect, before uploading local queued changes.
9. **Conflict surface** — poll the `fauna.sync.conflicts.list` WS-RPC kind. Built model ([`conflicts.md`](conflicts.md)): conflicts arrive auto-resolved — render the per-set review list (re-point = the [`file-versions.md`](file-versions.md) restore) via `include_resolved`; never render a blocking chooser. The candidate/choose-winner path (`fauna.sync.conflicts.resolve`) is legacy substrate only, for interop with clients built before 2026-07-11.
10. **Lease support** — for binary/non-mergeable folders, acquire an exclusive lease via `fauna.folders.lease.acquire` before writing.

## FAQ

**Q: What's the maximum file size for sync?**
A: A file's size is bounded by the tier's **total storage** quota, not by a single
"max file size". Files are split into content-addressed chunks each ≤ 8 MB
(`MAX_CHUNK`, see § Content-Addressed Storage), and the tier's **max blob** limit
(Free 10 MB / Personal 100 MB / Community 500 MB) applies to those individual
chunks/manifests — which the 8 MB chunk bound keeps comfortably under for every
tier. So a Free user can sync a file far larger than 10 MB as long as it fits their
total storage quota; "max blob" is a per-chunk ceiling, not a per-file one.

**Q: What happens when two devices edit the same file?**
A: The nest records a conflict with both candidate versions and **auto-resolves it immediately** (built end-to-end, ratified 2026-07-10) — a clean three-way merge for text-like files, otherwise latest-writer-wins — with the losing version retained in version history and the resolution reviewable per-set (re-point = version restore) via `fauna.sync.conflicts.list`. No app presents a blocking chooser; `fauna.sync.conflicts.resolve` (the candidate-chooser path) survives only as the wire substrate for pre-2026-07-11 clients. Exclusive leases (`fauna.folders.lease.acquire`) remain available to prevent concurrent edits on non-mergeable sets.

# Sync engine deployments — where the engine runs, and how every app converged — target state

Owns: sync-engine-deployments, metadata-strip
Status: ratified — split verbatim out of `file-sync.md` on 2026-09-06; the engine-vs-deployment model is BUILT (all three desktops run deployment 2's per-user agent since 2026-07-19/22, iOS keeps deployment 1 in-process, and deployment 3 — the seed-holding headless daemon — was removed 2026-10-02), the apple B-track (B1–B5) closed 2026-07-13/14 and android's byte-sync convergence 2026-07-16, and the ingress metadata strip is converged across every app with its remaining residuals named in the coverage table
Authority: **where the shared `fauna-sync-engine` runs, and who controls it** — the engine-vs-deployment model and its deployment shapes (two live, a third removed); the Control Plane Principle (apps manage sync through the nest, never by driving a daemon over a side channel); the nest folder row as the single authority for per-set policy (mode, retention, selective sync), the de-knobbed reconcile cadence and the cross-nest cadence carriers; the remote-change nudge; the built-in default ignores and the two write doors that enforce them — **plus the per-app convergence records** (apple's `FfiSyncEngineHost` design and its B1–B5 close-out; android's byte-sync convergence) **and the uploader-side ingress metadata strip**: which app runs it, the exact per-container coverage table with its declared residuals, the lossless-never-re-encode rule and the `StripCoverage` contract. **NOT owned here** — the sync protocol itself (device registration, the chunk/manifest pipeline, offline catch-up, folders and membership, multi-writer shared sets, the six-state sync-display vocabulary) → [`file-sync.md`](file-sync.md); the cross-platform per-user agent's own scope, packaging, IPC seam and credential model → [`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md); on-demand placeholders + the hydration-host capability shape → [`on-demand-files.md`](on-demand-files.md); key material → [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md); the folder concept map → [`folders.md`](folders.md); the photo-backup set model → [`../ui/folders.md`](../ui/folders.md). On conflict in those domains, raise it.

Last verified: 2026-08-20 | Sources: `libs/fauna-sync-engine/src/{engine,config,watcher}.rs`, `libs/fauna-media/src/process.rs`

Split verbatim out of [`file-sync.md`](file-sync.md) on 2026-09-06 — that doc had
reached **242,738 B**, ~17 days from the 262,144 B whole-file read ceiling, and
its `## Status` section had stopped describing itself: 71,006 of its 82,181 bytes
were three contiguous child sections that are one concept — *where the engine
runs and how each app got there* — while only the remaining 11,175 B were
actually status. What stays there is the sync protocol itself. A routing stub
remains at each original location; prior history:
`git log --follow docs/goal/behavior/file-sync.md`.

> **Reading this doc.** Its text was carried **verbatim** out of
> [`file-sync.md`](file-sync.md) on 2026-09-06, so an unqualified `§ <name>`
> citation inside it — and any "above" / "below" pointing outside the text you
> see here — may name a section that is no longer a sibling on the page.
> § Status, § User Experience (with § Per-file sync-status display and
> § Local binding at rest), § Technical Flow, § Folders and § Membership,
> § Multi-writer shared sets, § Content-Addressed Storage,
> § On-Demand Files and § Implementing Sync on a New Platform all stayed in
> [`file-sync.md`](file-sync.md); resolve any other unqualified name there
> first.

## Section map

| Section | What it holds |
|---|---|
| § Control Plane Principle | The engine vs. its deployments — in-process in the app and the per-user session agent (a third, the legacy headless daemon, was removed 2026-10-02) — and the rule that both route management through the nest. Then the nest folder row as the single authority for per-set policy, what "scan frequency" actually means and the phase-5 de-knob to a hard-coded constant, the cross-nest cadence carriers, the remote-change nudge, and the built-in default ignores with the two write doors that enforce them. |
| § Apple apps — convergence design | The `FfiSyncEngineHost` design that moved apple onto the shared engine and the B1–B5 close-out record — and, carried in the same narrative, the **ingress metadata-strip convergence**: which app runs the strip, the exact per-container coverage table and its declared residuals, the lossless-never-re-encode rule, the known limit of the HEIC pins, and the ISO-BMFF major-brand sniff. |
| § Implementation status today (android byte-sync) | The closed-out record of android — the last non-conforming byte-sync deployment — converging onto the shared `FfiSyncEngineHost` over both its ingresses, and the one migration question ruled N/A. |

## Control Plane Principle

Apps control sync through the **nest HTTP API**, not by driving a *separate* sync daemon over a side channel. This is a statement about the **control/management plane**, not about where the byte-syncing runs.

**The engine vs. its deployment.** The sync *engine* — watcher → chunk pipeline → conflict-aware apply → state DB — is the shared `fauna-sync-engine` crate (`libs/fauna-sync-engine`). It runs in one of two deployments, both of which route management through the nest (a third, the autonomous headless daemon, was removed 2026-10-02 — the last bullet below keeps its record):

- **In-process** inside a desktop app — Linux links `fauna-sync-engine` directly and runs it on its own tokio runtime (no sidecar, no IPC; see `apps/linux.md` § File Sync). "Implementing Sync on a New Platform" below describes exactly this in-process path. **(Ratified 2026-07-18; REACHED on all three desktops — linux + macOS cut over 2026-07-19, windows completed its swap 2026-07-22: this deployment has narrowed to the app-side one-shots — photo ingress, restore walks — and the *always-resident* sync engines now run in deployment 2's per-user agent. The one residency that deliberately stayed in-app on all three was the segment-backup upload driver; it is now **gone from linux (2026-07-29) and macOS (2026-08-15)** and remains only on windows, retired not by D6's agent-hosting plan but by the nest becoming the writer at the segment-backup slice-5 flip (`backup-restore.md` § Background Tasks → *Flip status (slice 5)*). iOS keeps the in-process shape entirely. Owner: `../architecture/apps/sync-agent.md` § Implementation status today.)**
- **User-session helper process — the per-user sync+backup agent, LIVE on ALL three desktops (ratified 2026-07-18; linux + macOS cut over 2026-07-19, windows completed its swap to the generalized agent 2026-07-22).** Each desktop runs the engine (and, on windows, the cfapi placeholder host) in a helper process **in the user's logon session** — per-user, **not** a LocalSystem service (the same security context as OneDrive's `OneDrive.exe`, and the only context a per-user cfapi sync root can be served from). The identity-holding app provisions the helper with a **least-privilege capability** — the owner's `BackupKey` plus a renewable nest bearer, never the identity seed — exactly the "client mints a narrow capability for a content-serving helper" pattern the MDA mail bridge uses (`docs/goal/architecture/owner-key-material.md` § Audience: owner only Path B; rule #7 — `key-material-hierarchy.md` § Architectural rules). The generalized cross-platform agent (`bins/fauna-sync-agent` — packaging, IPC seam, persisted-capability model, lifecycle) is owned by `../architecture/apps/sync-agent.md`; see `apps/windows.md` § Shell Extension and § On-Demand Files below for the windows halves.
- **Autonomous headless daemon — REMOVED 2026-10-02 (legacy from 2026-07-18; removal ruled 2026-10-01; `../architecture/apps/sync-agent.md` § Headless deployment owns the ruling and its build status).** `bins/fauna-sync` ran on a NAS/server with no GUI. Unlike the helper agent it held the identity seed in its hand-edited config — a standing tension with the one-configuration-surface invariant its seed-custody carve-out never covered. The headless story is now **fauna-tui + the per-user agent** (onboard over SSH in a full app, provision the agent, enable linger — `../architecture/apps/sync-agent.md` § Headless deployment), which collapsed this deployment into deployment 2.

(✅ The apple apps **reached deployment shape 1 on 2026-07-13** — the bespoke Swift engine that made them a fourth, non-conforming deployment is deleted; see the § Status known-gaps bullet *~~Apple apps run a bespoke Swift sync engine~~ — RESOLVED* and § Apple apps — convergence design.)

(✅ **The android app reached deployment shape 1 on 2026-07-16** — the bespoke Kotlin upload path
that made it a fifth, non-conforming deployment (`core/ChunkedSyncEngine.kt`, driven by
`PhotoBackupEngine` + `WatchedDirectoryManager`) is deleted; android now runs the already-built
`FfiSyncEngineHost` over the ingress-staging path, same as apple (§ Apple apps — convergence design,
library-ingress bullet — *apple defines the shape; android mirrors it*). See § Implementation status
today (android byte-sync) below for the closed-out record.)

The principle, restated against those deployments:

- A *separate* sync process — the per-user agent — is autonomous once provisioned: it syncs with the nest over its own connection, according to the nest's folder rows, and does not require an app window to be running.
- Any app (phone, desktop, web) can manage folders, devices, and sync status through the nest's `fauna.sync.*` / `fauna.folders.*` WS-RPC kinds regardless of which deployment runs the engine.
- Desktop apps never reach a *separate* sync process via pipes, CLI, or local IPC **for sync management**. The nest may be on `localhost`, on the LAN, or remote. An app that runs the engine **in-process** owns it directly and is not "talking to a daemon" — it still drives all management through the nest. The per-user agent's local `fauna-ipc` seam is **not** an exception: it carries only the structurally-local (capability provisioning, the device-local location↔set binding, local status/shell integration, device-global pause) — never per-set policy, which stays in the nest rows (`../architecture/apps/sync-agent.md` § Control plane split).
- This enables **headless sync**: on a NAS or server with no GUI, fauna-tui provisions the per-user agent (with linger enabled) and the agent syncs autonomously (`../architecture/apps/sync-agent.md` § Headless deployment). Users manage it from any app through the nest.

**⚠ Phase 5 — the scan-frequency knob retired (folders re-model open call #2; design pass ratified 2026-08-19; NON-UI HALF LANDED 2026-08-20 — every reader below takes the constant; the rule-A-granted UI RETIREMENT LANDED 2026-08-20 too — no app renders a cadence control).** `folders.md` § Target re-model owns the concept claim (*the reconcile backstop becomes a hard-coded constant; the user-facing time control is snapshot policy*); this block owns the behavior + compat contract:

- **The backstop becomes a bucket-1 constant**: `fauna_client_folders::cadence::DEFAULT_RESCAN_INTERVAL` (300 s) stops being a *default* and becomes the **only** value — `rescan_interval_for` and every row/carrier read retire, and all five readers (the linux in-process engines, the headless daemon — removed since, 2026-10-02 — windows' hydration host, macOS's File Provider tick, android's `PhotoBackupWorker`) take the constant directly. Android's 15-minute WorkManager floor already absorbs 300 s today, so no platform's *default* behavior changes; what retires is the ability to choose otherwise (a user's 1 m or Daily choice stops being honored by new seats — the ratified knob-retirement, not a slip).
- **The wire, at-rest and carrier vestiges are gone (the compat-remnant contraction, 2026-10-01; `../architecture/version-compatibility.md` § Dimension 2, fourth exception).** `rescan_interval_secs` left `FolderSummary`, `FolderCreateRequest`/`Reply`, the `folders` table, and every cross-nest carrier; `fauna.folders.schedule.set` is retired. Until then they were a downgrade mirror kept for released apps — every app stamped 300 on create and no seat read the value.
- **The UI retirement — rule A granted and LANDED 2026-08-20** — `folder-frequency-select`, `wizard-frequency-option`, the wizard's Frequency step (the wizard is name → devices → review), the `devices.wizard.frequency_*` + `folders.scan_frequency*` strings, and the shared `frequency_options` / `frequency_label` / `set_folder_frequency` faces (FFI + wasm) retired together, on every app (the windows + apple call sites were swept without a compiler on the Linux dev VM; their merge-gate scripts are the detector). This does **not** touch the nest place's snapshot policy (`folder-nest-*`) — that IS the surviving user-facing time control, already built. **Test seam:** a tier_3 run that needs a different cadence sets the compile-gated `FAUNA_E2E_RESCAN_MS` (`fauna_sync_engine::always_resident::rescan_interval`, the `FAUNA_E2E_DEBOUNCE_MS` twin — every resident host reads it; the e2e drivers default every app launch to 30 s, the multiseat harness sets it per seat, and the `tests/platform/sync/` daemon package set it per spawn until those daemon tests left with the daemon, 2026-10-02); it is never a production knob. **A test that spawns a resident host itself and does not set the seam gets the 300 s constant** — which is correct, and is why a test asserting anything *across a rescan tick* must both set the seam and assert the cadence the host reports adopting. Two regression pins in that daemon package spent weeks passing vacuously on exactly this: they slept for "3 ticks" of a cadence they had only ever written into the retired TOML field, so zero ticks fired and their negative assertions held for the wrong reason (fixed 2026-09-02).

**Config: the nest folder row is authoritative (per-set control).** A folder's operating policy — `mode`, retention, and selective-sync (`include_paths` / `exclude_paths`) — is the **nest folder row's** to own. (The scan cadence left this list with phase 5's de-knob, landed 2026-08-20: it is a hard-coded constant, not a choice — the block above owns the contract.) The row is the **single authoritative source**, and **every device — foreground app *and* per-user agent — reads the row and applies it.** (✅ True of selective-sync's `include_paths`/`exclude_paths` too as of 2026-08-27: the lists install into the running engine's `IgnoreMatcher` off the same authoritative row read as the mode — `SyncEngine::refresh_sync_mode`, per the § Status entry *~~Selective sync filters nothing on the deployments that matter~~ — RESOLVED*, which owns the mechanism and the per-list failure posture. `mode` is resolved everywhere via the same `config::resolve_device_mode`, per § *What arms the guard* below.) No device pushes a divergent local value; in particular the autonomous `bins/fauna-sync` daemon, while it existed (removed 2026-10-02), **pulled** its mode / retention *from the row* (the same `fauna.folders.list` / folder-row read every app already does) instead of pushing a local TOML value and re-asserting it on every register. The old rule — *the daemon's local TOML is authoritative, it pushes config at registration and re-asserts on restart, and a nest→device pull-override is "deliberately not done"* — is **retired** (it was the `file-sync.md` half of the `devices.md ↔ file-sync.md` scan-frequency contradiction; `devices.md` already presents frequency as real control).

**The cross-nest cadence carriers are gone (2026-10-01).** The Welcome relay, the `caller_access` reply, the member's `__config` `ForeignFolder` and the pushed engine-key blob no longer carry a cadence: every seat ticks at the constant (the phase-5 block above).

**Why this is the correct direction — it removes a latent invariant violation, not just a preference.** The retired reflection model treated a daemon's *local TOML* as authoritative and called the user's wizard choice "transient" — i.e. it assumed an **operator hand-edits a daemon config file** to set the scan cadence. That is exactly the banned **"operator" / configuration-file-theatre** anti-pattern the iron-clad product invariant forbids (`principles.md` § One configuration surface: *the only configuration surface is the apps; there is no operator; anything a user/admin chooses lives in app UI + nest state and nothing else hand-edits config*). Scan frequency, while it was a choice at all, belonged in app UI + nest state, honored by all devices (phase 5 then settled that it is not a choice — a bucket-1 constant — which removes the question entirely). The headless daemon's **seed-in-config exception** (above; the daemon was removed 2026-10-02) sanctioned holding the *identity seed* locally — that is how the box authenticates *as* the user, identity material / artifact wiring, not a behavior preference — and it does **not** extend to a user-chosen knob like the scan cadence / mode / retention; the reflection model over-extended it. Pulling the row therefore *removes* the violation rather than introducing one, and the old "the DB default `rescan_interval_secs = 300` would clobber the operator's local value" objection dissolves: there is no authoritative local value to clobber — the row *is* the value, set by the user.

**What "scan frequency" actually means — it is not the live-edit latency.** Live changes are **already real-time**: an inotify / FSEvents / ReadDirectoryChangesW watcher with a ~2 s debounce (`libs/fauna-sync-engine/src/watcher.rs`; `always_resident::DEBOUNCE_DELAY`) forwards an edit within seconds. Scan frequency is **not** that trigger. The rescan interval (`fauna_client_folders::DEFAULT_RESCAN_INTERVAL`, read through `always_resident::rescan_interval`) is the **periodic full-reconcile / catch-up cadence** — a backstop that re-lists the folder to catch watcher-misses and offline changes and to pull remote changes. Its role differs by mode, but the **same constant governs all three**:

*Implementation status today (watcher backend, 2026-08-05): **the "FSEvents on macOS" above is true only as of today** — every macOS build before it ran notify's **kqueue** backend instead, and lost most watcher events.* The workspace pinned `notify`'s `macos_kqueue` feature in the very first scaffold commit that introduced the dependency (2026-03-08) with no rationale recorded, which takes macOS **off** FSEvents. kqueue learns of a new directory entry only by re-scanning the directory it was told changed, and drops entries under concurrent writes: measured on macOS, of ten files written into a watch root the watcher reported **three**. The real-time plane described in this section therefore did not work on macOS at all — a file could appear in a synced folder and be reported nowhere, its only recovery the periodic rescan (minutes), and for a *delete* there was no recovery at any cadence until the sweep below. Cost before it was found: four sessions diagnosing it as a wedged daemon, a full watcher channel, and "FSEvents coalescing" — a backend that was never running. Now `macos_fsevent`, pinned by tier_1 `fauna_sync_engine::watcher::tests::every_file_created_in_the_watch_root_is_reported` (7/10 lost on kqueue, green on FSEvents). **A second defect the flip exposed, and fixed with it:** FSEvents reports fully-resolved paths, so a watch root spelled through a symlink (every macOS `TMPDIR`, and any user folder reached via one) failed `strip_prefix` for **every** event and attributed none of them to a file — a healthy watcher, events flowing, nothing syncing. `watcher::event_rel_path` — the one funnel every event consumer routes through — now falls back to the canonicalized root, pinned by `::event_rel_path_resolves_a_symlinked_root`. iOS is unaffected in kind (it never had FSEvents) and still compiles; linux/windows never consulted this feature.

*Implementation status today (watcher event delivery, 2026-08-11): **a dropped watcher event heals at the rescan; a wedged watcher thread does not, so the queue now drops rather than blocks.*** The question this settles is what happens when the watcher misses an event mid-session, with the daemon still up. **Creates and modifies heal** at the periodic reconcile — `rescan_interval_secs`, 300 s in production (`libs/fauna-sync-engine/src/watcher.rs`; cadence resolved per § Cadence above) — and the OS's own overflow signal is consumed and logged rather than swallowed, so "events were lost" is always on the record. **Deletes heal too**, which was the open question until 2026-08-05: the engine's reconcile diffs the `Synced` set against the scan and records each missing row through `handle_delete` (`engine.rs::reconcile` → the delete-detection pass); the headless daemon's rescan tick ran a twin, `sweep_deletes_ws`, until the daemon was removed (2026-10-02). The reconcile carries the **mass-delete floor** — every synced path missing at once reads as infrastructure failure, never intent, and is held rather than propagated. (The delete leg and the floor are owned by [`delete-propagation.md`](delete-propagation.md); named here only because they are what makes a *dropped* watcher event recoverable.) **What changed 2026-08-11:** the watcher's OS callback thread previously handed events on with `blocking_send`, which does not drop on a full queue — it parks the callback thread until capacity frees. One stalled consumer therefore wedged the watcher permanently, losing every subsequent event with *no drop and no log*, recoverable only by restarting the process. It is now a bounded `try_send` with a drop counter (`FsWatcher::dropped_events`, logged on the first drop of a burst and then on powers of two): a full queue costs the tail, loudly and countably, and delivery resumes the moment the consumer catches up. The trade is favourable **because of the paragraph above** — a dropped event costs at most one rescan interval of latency on all three event classes, where a wedged thread costs everything until restart. Pinned by tier_1 `watcher::tests::a_full_queue_drops_and_counts_instead_of_wedging_the_callback_thread` (state, never timing; red-verified by reverting to `blocking_send` — though note it goes red by tokio panicking on a blocking call inside its runtime, where the real callback thread has no runtime to object and would simply park in silence) and `::a_closed_receiver_is_not_counted_as_a_full_queue_drop`, keeping queue pressure and a shut-down consumer distinguishable — they have different remedies. Note the wedge was never witnessed in production: it was filed as the leading explanation for the 2026-08-05 three-daemon blackout, which was separately root-caused to the mispinned macOS backend above and fixed. This is robustness, not a defect fix.

- **Sync** — how often a full catch-up reconcile runs as a backstop behind real-time forwarding.
- **Backup** — the snapshot cadence (`config.rs:14`).
- **Web** — how often the published site re-reconciles / re-publishes.

The value is the constant `DEFAULT_RESCAN_INTERVAL` = **300 s**, identical across Sync / Backup / Web (the phase-5 block above). Until 2026-08-20 a per-set picker offered a 7-option catalog `{60, 300, 900, 1800, 3600, 21600, 86400}` s with a helptext that drew exactly this live-vs-backstop distinction; the picker and catalog are gone (`docs/goal/ui/folders.md` § Scan frequency).

**Remote-change nudge (ratified target 2026-07-20; BUILT same-nest
2026-07-23; PROVEN end-to-end — `same_nest_push_nudge` tier_3 test).** The upload half of "real-time" was always built (watcher +
debounce); the *download* half was cadence-only — a device learned of another
device's/member's record no sooner than its next reconcile pull, so shared-set
collaboration latency was bounded by the rescan interval (minutes), not the
watcher (seconds). Now a best-effort **push nudge** fires on new sync records:
the additive `PushEvent::SyncChanged` (wire kind `fauna.sync.changed`, carrying
the set name — `transport.md` § Push events owns the wire surface) fires from
the nest's record path (`record_change_core`, so both the same-nest
`fauna.sync.changes.record` handler and the federation relay into a set homed
here) to every connected **same-nest** participant of the set (owner + roster
members, via `list_channel_actors`); a receiving engine reacts by scheduling an
immediate off-cadence pull for that set (the resident watch loop's wake arm,
routed on linux/the agent as `PushEvent::SyncChanged` → the `PullFolderNow` IPC
→ the per-engine wake channel). Exactly the `fauna.mail.received` pattern: push
is the latency path, the periodic reconcile stays the correctness backstop, and
a missed push costs only latency (offline devices catch up on cadence — no new
correctness dependency, so it fires unconditionally after a durable insert; a
rare byte-identical replay's redundant nudge is a harmless no-op pull). **Same-nest
only**; a **cross-nest** nudge would grow the deliberately-closed federation
kind inventory (`federation.md` § Cross-nest) and therefore needs its own
dedicated federation design + security pass before ratification — a named
candidate, not a ratified target. Rationale: shrinking propagation latency
directly shrinks the concurrent-edit divergence window, which is the dominant UX
factor for shared-set collaboration through OS folders.

**Built-in default ignores (ratified target 2026-07-20; BUILT 2026-07-24,
extended to the Apple FP write path + any-depth matching 2026-07-24,
).** The scanner already excludes dotfiles
categorically (they never enter a folder — `watcher::scan_recursive_filtered`;
this is what keeps `.DS_Store` and `.faunaignore` itself out), and
`.faunaignore` provides per-folder gitignore-style overrides. **Built:** a
hard-coded Rust constant, `fauna_sync_engine::ignore::DEFAULT_IGNORE_PATTERNS`
(`~$*`, `*.tmp`, `Thumbs.db`, `desktop.ini`), applied unconditionally by
`IgnoreMatcher::load` — unioned with any `.faunaignore` content, never a
replacement for it, and never a config surface (no-operator invariant;
`.faunaignore` stays the per-folder user override, data inside the folder, not
operator config). Patterns without a `/` match at **any depth** (gitignore
semantics — Office writes its lock file next to the document, wherever that
is), and `ignore::has_hidden_component` states the scanner's categorical
dotfile rule as a pure rel predicate for write paths that never scan.
Decision of record: Office
`~$` lock files are **filtered** (OneDrive parity) — syncing them would give a
crude "locked by …" presence signal in Office, but at reconcile latency it is
mostly stale-lock annoyance after crashes; version retention makes either
choice lossless, so parity wins.

**Enforcement covers BOTH write doors into a `SyncEngine`:** (1) the
**watcher path** (linux, windows always-resident, and the upload half of
windows on-demand) gates every filesystem event in
`always_resident::is_user_write` — dotfile-component check + `is_ignored` —
before it reaches the debouncer or `upload_file`; (2) the **Apple File
Provider write path**, where litter arrives as ordinary
`createItem`/`modifyItem` callbacks rather than filesystem events: the shared
`provider_face` write cores (`serve_ingest`, `serve_ingest_with_base`,
`serve_rename`'s ingest half) consult `ProviderEngine::is_ignored` and answer
`WriteAck::excluded` — no seal, no upload, no change record — which the appex
maps to **`NSFileProviderError.excludedFromSync`** (macOS 13+/iOS 16+, Apple's
purpose-built signal: the file stays on the user's disk, the system stops
syncing it, issues a provider-side `deleteItem` — vacuous for an untracked
rel, a genuine tombstone for legacy pre-gate litter rows, self-healing them —
and re-evaluates via a fresh `createItem` on later changes). `createItem`
additionally asks `host.is_ignored` up front, before staging bytes; an ignored
*folder* is excluded whole, so the OS never fans out `createItem` for its
children (`.git/`). Renaming **into** an ignored name tombstones the old path
(every other device drops it) and excludes the new one — the same net shape
the watcher path produces. `serve_delete` stays un-gated so legacy litter
stays deletable. Known gap: an FP-bound set does not honor per-folder
`.faunaignore` **user overrides** (the staging root never materializes
excluded files, so the matcher only ever sees built-ins + dotfile exclusion
there); acceptable until a deliberate design routes the override file to the
extension. Proof: tier-1 `provider_face` gate tests + the tier-3 twins
`office_atomic_save_dance_coalesces_to_one_clean_version_watcher_path` /
`office_litter_via_provider_write_path_is_excluded_from_sync`
(`bins/fauna-nest/tests/conformance_file_provider_client.rs`).

**Implementation status today (Config: cadence + nudge; compressed 2026-08-02 — the discovery narratives live in this section's git history).**

- **Cadence — the constant, every reader (phase 5's de-knob, non-UI half landed 2026-08-20).** All five former row-readers — the linux in-process engines (`engine_lifecycle`), the headless daemon (`apply_folder_row` stopped overlaying the cadence; the daemon itself was removed 2026-10-02), the agent's hydration hosts (owner, writer-member, and cross-nest foreign-routed alike — `HydrationHost::rescan_interval` is a constant, kept as a trait method only as a test seam), macOS's File Provider re-pull tick (`FfiFileProviderHost::rescan_interval_secs` deleted; the appex takes the constant — apple leg with the trickle-down batch), and android's photo-backup `WorkManager` period (`PhotoBackupWorker` runs the flat 15-minute floor; `FfiFoldersClient::rescan_interval_secs_for` deleted) — take `fauna_client_folders::cadence::DEFAULT_RESCAN_INTERVAL` (300 s) directly; `rescan_interval_for` / `rescan_interval_from_secs` are deleted. The wire/at-rest vestiges the phase-5 block once listed are gone (2026-10-01). The `folder-frequency-select` control and the wizard's frequency step retired on every app the same day (phase 5 step 3).
- **The cadence governs the catch-up rhythm, never the first pull** (since 2026-07-22): every resident engine pulls remote changes once, eagerly, before settling into its tick (`always_resident::run_watch_loop`; the one-shot iOS shape and the headless daemon already behaved this way) — a freshly-bound location converges in ~0.5 s instead of one full interval (`cross_nest_agent_capstone`). A member binds a shared folder precisely to receive what is already in the set: correctness-of-experience, not tuning.
- **An edit saved while a resident engine starts is not stranded for a cadence** (since 2026-09-27): every caller converges the local half before `run_watch_loop` exists, so a file written between that scan and the watcher's start reached neither and waited out the whole interval. The loop now converges once more right after its watcher is up — anything earlier is on disk for that pass, anything later raises a watcher event. The window opens on every engine restart, and the sync agent restarts an engine on every content-key re-key ([`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host → *One mechanism*), which is how `agent_custody_rekey` found it.
- **Remote-change nudge — BUILT same-nest; reaction arms landed on all 7 apps + both engine hosts (last: windows, 2026-08-02).** `PushEvent::SyncChanged` (`fauna.sync.changed`, carrying the set name and its `folder_hash` — matched through `SyncChangedPayload::names_set`, `path-sealing.md` § the set-name plane) fires from the one nest ingest rail — `record_change_core` (the WS-RPC `changes.record` path; the `/sync/ws` data-plane twin is removed) — and from the resolved conflict report (`report_conflict`). **Standing rule: every path that records a `sync_changes` row owes the nudge** (pinned both ways by `a_resolved_conflict_report_nudges_the_other_devices` / `an_unresolved_conflict_report_does_not_nudge`, `bins/fauna-nest/tests/conformance_folders.rs`). Receivers: linux / tui / the windows app relay via the `PullFolderNow` IPC (windows' per-kind push routing is unit-pinned — `NestRpcPushDispatchTests` — because the agent holds no WS connection of its own, so the app's relay is the only delivery path a test can observe); apple via `FaunaClient.swift` `.syncChanged`, which signals **two** consumers unconditionally because a macOS set may be served by either — the **File Provider extension** for a domain-bound set (`FileProviderCoordinator.signalChanged(folder:folderHash:)` — the push matched to a held set by its own name through `sync_changed_names_set`, that set's domain signalled by its actor-scoped identifier, never by its display name — → the extension's `enumerateChanges` → `host.refresh()` + self-terminating re-signal; Apple's sanctioned cross-process wake, tier_1 `apply_refresh_fold_*` pins + a signing-gated appex e2e leg on the read-path harness), and, since 2026-08-06, the **resident `fauna-sync-agent`** for a folder-bound set, relayed as `pullFolderNow` on the session's `FfiSyncAgentProvisioner` (registered at FaunaKit's one construction point, held weakly so a retired provisioner is never nudged; nothing registers on iOS, where the extension is the only mechanism). ⚠ **The agent arm was missing until then, and the doc's own "all 7 apps" wording hid it**: the File-Provider half was genuinely built, so apple read as complete while a *folder-bound* macOS app — the shape the `native+native` seat pair drives — uploaded correctly and never applied a peer's change, its only delivery path being the rescan tick. Found by convention 16's macos seat pair; the identical hole on windows was closed. Per-app coverage of a nudge means *per binding mode*, not per app; android + web as blanket machine-snapshot refetches (`folderChangedTick` collectors; `FoldersSection.svelte`); and the headless daemon, which subscribed on the same WS-RPC handle its catch-up used and pulled off-cadence (removed 2026-10-02). Proven end-to-end tier_3: `same_nest_push_nudge` (a second device's save materializes in 0.3 s, RED-guarded without the nudge) and `test_ws_two_devices_converge_with_no_destination_rows` (~2 s both directions — deliberately WITHOUT the admin `folder_destinations` rows every earlier orchestrator test hand-created and no product code reaches). A **cross-nest** nudge would grow the deliberately-closed federation kind inventory and stays a named candidate needing its own dedicated federation + security pass — NOT ratified.
- **Two ordering rules the nudge exposed (both fixed + pinned 2026-08-01):** (1) a writer pushes chunks + manifest **before** it notifies — § 3 File Upload's numbered order; the daemon once inverted it, so a nudged peer `GET /manifests/<hash>`-404'd 12.8 ms before the writer's own post-push log line, and the 404 was **terminal** (catch-up classes it transient and stops; nothing re-drove the pull). (2) A pull that reports itself unfinished is re-driven by the rescan tick (the engine pulls on every tick; the removed daemon used a conditional `PullPhase::Retry`) — there is still no standing nest poll. The nudge did not create that race; it exposed it — sequence the upload, never back the nudge out.
- **⚠ A new `PushEvent` variant is compile-blocking on apple BY DESIGN:** `FaunaClient.swift`'s push switch is exhaustive — one arm per new variant, even a documented ignore, bought in exchange for a compile-time prompt to decide rather than a silent drop — and no fleet merge gate compiles Swift, so land the apple arm in the same change. Unknown *wire* kinds still arrive as `Other` everywhere.
- **Sync activity is now observable on web (2026-08-11) — CLOSES the gap this bullet used to describe.** `FolderSummary` (what web/android's blanket refetch reads) still carries no field that changes on an ordinary sync `changes.record` (`cached_snapshot_count`/`cached_total_bytes` are backup-mode-only), but the nest's per-device `last_change_at`/`change_count` (`fauna.folders.devices` → `get_folder_devices`) is now threaded through, on web, as a THIN direct read (`WsRpcClient.foldersDevices` → `FoldersClient::devices`, mirroring `foldersActorMembers` — deliberately bypassing `DevicesMachine`/`DevicesNestApi`, the same division `foldersActorMembers` already uses for a read-only per-set fetch that needs no custody rendering) rather than folded into the cached snapshot. Rendered as `folder-device-activity-item`/`-label`/`-count` in the expanded row (lazy-loaded on expand, re-fetched on every `fauna.sync.changed` push — `FoldersSection.svelte`), and proven end-to-end: `test_folder_device_activity_reflects_recorded_changes` (tier_3, `tests/e2e-unified/tests/test_folders.py`) records a change over the real `fauna.sync.changes.record` RPC and asserts the mounted, expanded row's count updates with no manual reload — the concrete e2e pin this bullet used to say didn't exist yet. **tui landed the identical render 2026-08-11** (the lead app's turn per priority #1) — the same `fauna.folders.devices` read, consumed IN-PROCESS through `fauna_client_folders::FoldersClient::devices` (no wasm/FFI hop; tui already links the shared crate directly, mirroring its existing `load_roster` roster-read shape) rather than web's wasm binding. The expand-time read rides the SAME awaited round trip as the "Shared with" roster (`Op::LoadFolderRoster`, extended to fetch both — tui's one-awaited-op-per-gesture contract means the agent's very next query sees both sections settled), and the push-driven live update is a narrower, dedicated round trip (`Op::LoadFolderDeviceActivity`, spawned by `crate::settings::device_activity_resync_op`) that re-fires ONLY when `fauna.sync.changed` names the set the ONE currently-expanded row shows and the Folders page is the one on screen — never a blanket page-wide resync, and never touching (or blanking) the roster the push has nothing to do with. `test_folder_device_activity_reflects_recorded_changes[tui]` passes end-to-end (same test, widened). **linux landed the identical render 2026-08-11** — the same `fauna.folders.devices` read, consumed IN-PROCESS via `fauna_client_folders::FoldersClient::devices` (no FFI/wasm hop; `FaunaClient::fetch_folder_devices`, `client.rs`, mirroring the existing `fetch_folder_members`/`fetch_folder_actors` row-detail-read shape). Rendered as `folder-device-activity-item`/`-label`/`-count` on owner rows only (`build_folder_row`, `views/devices_folders/folders.rs`), lazy-loaded on first expand alongside the member/actor rosters. The push-driven live update — the whole point of the feature — re-fetches on every `fauna.sync.changed` push while the row stays expanded (`app.rs`'s `PushEvent::SyncChanged` arm), gated on a new `folder_row_is_expanded` helper (reads the `ExpanderRow`'s own `is_expanded()` directly, no per-row guard state needed) so a collapsed row's roster is never wastefully re-fetched — mirrors web's `if (expandedFs) void loadDeviceActivity(...)`. `test_folder_device_activity_reflects_recorded_changes[linux]` PASSED end-to-end 2026-08-11 (same test, widened again). **android landed the identical render 2026-08-12** (the fourth of six) — the same `fauna.folders.devices` read, consumed over the existing UniFFI `FfiFoldersClient::devices` (`ApiClient.folderDevices`, `ApiClient.kt`, mirroring `folderActorMembers`'s deliberate bypass of `DevicesMachine`/`DevicesNestApi`). Rendered as `folder-device-activity-item`/`-label`/`-count` on owner rows only (`DeviceActivitySection`, `FoldersScreen.kt`), loaded on every transition into the row's local `expanded` state (`DevicesVM.setFolderExpanded` → `loadFolderDeviceActivity`, mirroring the existing `folderActors` on-expand-load shape — expanding always re-reads, matching web's `loadDeviceActivity` firing on every toggle-to-expand rather than only the first). The push-driven live update rides the SAME blanket, unfiltered `folderChangedTick` `DevicesVM` already collected for `refresh()` (`event.folder` still isn't threaded through android's `FfiPushEvent.SyncChanged` arm, `ApiClient.kt` — deliberately: android's push handling is VM-owned, not Composable-owned, unlike web's component-local `expandedFs`), narrowed downstream instead: the VM keeps its own shadow `Set<String>` of which rows the Compose screen currently has expanded (`FolderRow`'s `expanded` boolean is Compose-local, invisible to the VM otherwise — a `LaunchedEffect(expanded, folder.name)` reports every transition), and re-reads device activity only for those rows on every tick — never a blanket refetch of every set's activity, and never touching a collapsed row's stale data (linux's `folder_row_is_expanded` gate / web's `if (expandedFs) void loadDeviceActivity(expandedFs)`, adapted to Android's per-row independent Compose state rather than a single page-level variable). Verified by `DevicesVMTest` (the push-driven re-fetch, the expanded-only gating across multiple sets, the degrade-to-empty-on-failure shape) and `FoldersContentTest` (the rendering + the `onFolderExpandedChanged` wiring) — both host-JVM Robolectric/JUnit, no emulator; the android e2e leg (`test_folder_device_activity_reflects_recorded_changes[android]`) is captured but **UNVERIFIED from this session**, since android e2e needs the Android emulator, which only the `host` dev machine runs (this session's machine cannot reach it). **windows landed the identical render 2026-08-13** (the fifth of six) — the same `fauna.folders.devices` read, consumed over the existing UniFFI `FfiFoldersClient::devices` (`INestRpcClient.FoldersDevicesAsync` → `NestRpcClient.cs`, closer to web's thin-FFI-consumer shape than tui/linux's in-process one, per the earlier framing). Rendered as `folder-device-activity-item`/`-label`/`-count` in the expanded row (`FoldersPage.xaml.cs`'s `BuildDeviceActivitySection`, lazy-loaded on first expand). The push-driven live update wires `FfiPushEvent.SyncChanged` to a new `INestRpcClient.FolderChangedPushed` event, marshaled onto the UI thread and gated on `_expandedFolder == folder` (`OnFolderChangedPushed`) — the same "only if this exact set is currently expanded" guard tui/linux/android independently arrived at — fired alongside the pre-existing agent `PullFolderNow` nudge the same push already drove. `test_folder_device_activity_reflects_recorded_changes[windows]` passes end-to-end (same test, widened again). **apple landed the identical render 2026-08-23** (the sixth and last of six) — the same `fauna.folders.devices` read, consumed over the existing UniFFI wrapper (`FfiFoldersClient::devices` → Swift `APIClient.listFolderDevices(name:)`, mirroring `folderActorMembers`'s deliberate bypass of `DevicesMachine`). Rendered as `folder-device-activity-item`/`-label`/`-count` on owner rows only (`FolderDeviceActivitySection`, `FoldersContent.swift` — shared by macOS + iOS, one FaunaKit surface per priority #2), lazy-loaded on row expand (the section only exists while its row is expanded). The push-driven live update posts a new `.faunaFolderDeviceActivityChanged` notification from `FaunaClient`'s `.syncChanged` push arm — carrying the folder name, unlike this file's other push signals which are payload-less broad re-pull hints — observed via the `onFolderDeviceActivityChanged` modifier and gated on the pushed name matching the currently-expanded set (the same "only if this exact set is currently expanded" guard tui/linux/windows independently arrived at). `test_folder_device_activity_reflects_recorded_changes` now runs unconditionally on all 7 apps — the skip gate is deleted, not widened, since apple was the last residual. **The whole per-app rollout is now COMPLETE.**
- **The live-nest proof of the 2026-08-01/02 convergence fixes LANDED 2026-08-03**: all 4 seat-mode cells this machine collects converged against example.com post-redeploy, full fan-out + concurrent merge + delete propagation (`e2e-live-sync-convergence.md` § convention 16 implementation status).

See `app-guidelines.md` rule 9 for the full client-facing control-plane rule.

## Apple apps — convergence design (ratified 2026-07-12)

How macOS and iOS converge onto the shared `fauna-sync-engine`, retiring the bespoke Swift
engine (§ Status known-gaps bullet). Key-material mechanics are owned by
[`../architecture/owner-key-material.md`](../architecture/owner-key-material.md) (§ Path A) +
[`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
(§ M2) — referenced, not restated; the photo-backup *set model* is owned by
[`../ui/folders.md`](../ui/folders.md) § Photo backup. This section owns the apple
deployment + migration shape.

- **Deployment shape: in-process on both targets (deployment 1, the Linux shape) — reached
  over a UniFFI engine host.** A new `libs/fauna-ffi` host object (working name
  `FfiSyncEngineHost`) wraps N `SyncEngine`s multiplexed over the **same shared
  `fauna_sync_engine::engine_host::EngineHost` driver** Linux and the Windows hydration host
  use (§ On-Demand Files: one shared multi-engine driver, never a per-app driver), on a
  dedicated OS thread with a current-thread runtime — the worker-thread precedent for a
  `Send`+`!Sync` engine behind UniFFI (established by the since-deleted `FfiBackupCoordinator`;
  today's exemplar is `libs/fauna-ffi/src/sync_engine_host.rs` itself).
  Swift keeps only the platform shells: location-binding UI, PhotoKit ingress, lifecycle glue,
  and rendering. No chunk pipeline, no `/api/v1/chunks` calls, and no per-file sync-state
  store remain in Swift — the engine's per-set `SyncDb` is the state store, and the
  SwiftData `SyncFile`/`SyncAnchor` models retire.
- **Sealed chunks from day one.** The host resolves per-set key bindings exactly as Linux
  does (`fauna_client_folders::resolve_engine_key_binding` over the owner's folder-key
  custody, the account-state plane kind `fauna.state.folder-keys` —
  [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
  § The `__config` dissolution schedule → *The kinds*; a bound-but-keyless set
  **fails closed**, never plaintext) — and, unlike
  Linux today, passes `backup_key = Some(BackupKey::derive(seed))` for **unbound owner-only
  sets**, so every apple upload seals through the convergent `chunk_crypto` path and is
  ciphertext-hash store-keyed (§ Content-Addressed Storage; `encryption-at-rest.md`
  § Per-content-kind conformance, *Folder files* row). ⚠ Fleet note (updated 2026-07-13): Linux now matches this — the foreground engine derives
  `backup_key = Some(BackupKey::derive(seed))` for owner-only sets (`apps/fauna-linux/src/sync.rs`;
  the retired *"box is trusted for the owner's own data"* rationale the no-modes ruling
  retired, `../architecture/nest/storage-modes.md`, is gone from the comment). **The
  `bins/fauna-sync` headless daemon now seals too (2026-07-13)** — `run_ws_session` derives the
  owner `convergent_chunk_root()` and threads it into the WS client, so its own
  `chunk_and_cache` → `push_cached_file_to_nest` pipeline seals every chunk (ciphertext store
  keys + `stored_hashes`), and its read/apply + `restore` paths open manifest-driven (both the
  sealed and the legacy plaintext back-catalogue). No owner-only folder writer rests plaintext.
- **macOS: one byte-sync path.** The in-process engines replace **both** legacy paths — the
  Swift `SyncEngine` + FSEvents `DirectoryWatcher` (the engine's own `notify` watcher uses
  FSEvents on macOS), **and** the launchctl-managed LaunchAgent path: `SyncDaemonManager`
  retires outright. Rationale: the bundled `fauna-sync` binary was never actually shipped by
  the SwiftPM build (`Bundle.main.path(forAuxiliaryExecutable:)` returns nil → the path is
  inert in practice), its launchctl shell-outs are the declared control-plane gap
  (`apps/macos.md`), and its seed-in-TOML custody is sanctioned only for the headless
  deployment-3 daemon — never for a GUI machine that holds the seed in the Keychain.
  `bins/fauna-sync` then remained available as the standalone headless daemon (deployment 3),
  self-installed on a GUI-less box, simply no longer app-managed (true when this was
  written; the daemon was since removed, 2026-10-02 — `../architecture/apps/sync-agent.md`
  § Headless deployment). Folder↔set bindings
  unify on the existing device-local location map (`MacFolderBindingSection`,
  § On-Demand Files binding rule) — the engine host reads and writes that same file, so the
  bindings a user already made carry over untouched. The legacy `UserDefaults
  "fauna.syncFolders"` store is **deleted, not migrated**: this doc previously called for a
  one-time migration out of it, but that key had **no writer** — its only one was
  `DirectoryWatcher.saveLocationConfig` (renamed from `saveFolderConfig` in the phase 1a
  location-vocabulary sweep), reachable solely from `addFolder`/`removeFolder`,
  which had zero call sites — so it has never held a binding to migrate (verified at
  file:line during the B2 cutover, 2026-07-13).
- **iOS: no in-process location binding.** iOS binds no folder in the app (no always-on
  daemon exists on iOS by construction): remote files are read on demand via Media, and the
  Files-app File Provider is ratified as the shared apple binding (§ On-Demand Files → Apple
  File Provider binding — the same extension as macOS). The app's host serves only the
  construct-run-drop calls below — photo ingress from the upload `BGProcessingTask` and the
  foreground PhotoKit observer (`apps/ios.md` § BackgroundScheduler) — so nothing needs
  pausing across scene phases and there is no file-sync background pass.
- **Photo-library ingress — the "library-ingress" use of the in-process deployment (apple
  defines the shape; android mirrors it).** An OS-owned photo library (PhotoKit; MediaStore
  on android) is not a folder, so it does not get a watch-dir engine. Ingress is the
  engine's **sealed per-file ingest**: export the asset to a temp file → ingest into the
  set through the ordinary sealed chunk pipeline + `changes.record` (backup-type custody
  upsert, § Membership) → delete the temp. No `watch_dir`, no reconcile pass, no
  tombstone risk from deleting staged files, and no duplicate photo library on disk. The
  target set is the **wizard-created "Photo Library" folder** (one-tap preset;
  set-model authority `../ui/folders.md` § Photo backup, including the legacy-`"photos"`
  adoption rule). The PhotoKit-specific dedup ledger (`PhotoBackupRecord`,
  `localIdentifier`-keyed) stays in Swift — it maps OS asset identity, not sync state.
- **Migration for existing plaintext records — the § M2 re-seal trio shape, no new wire.**
  (Retired as a standalone once-per-start migration by the compat-remnant sweep,
  `../architecture/version-compatibility.md` § Dimension 2; the trio survives as the
  audience convergence's public → private flip-back.)
  For each live head the legacy engine recorded (a legacy plaintext manifest — no
  `stored_hashes`): re-upload through the sealed path (idempotent — the convergent seal
  yields byte-identical ciphertext on re-run), whole-file verify, then **best-effort**
  `fauna.sync.changes.supersede` of the superseded plaintext rows (rows are marked, never
  deleted; the ordinary GC grace sweep reclaims — `mls-group-key-material.md` § M2
  *Pre-bind re-seal migration*, pieces A/B/C, all landed 2026-07-07). Photos re-ingest from
  PhotoKit (the device library is the source of truth). The pass cursor is device-local and
  the pass is additive + idempotent, so a lost cursor merely re-runs it. In
  expand→migrate→contract terms (`version-compatibility.md` § 2.1): expand = sealed records
  land beside plaintext ones (both readable — the manifest's `stored_hashes` presence
  selects the open path); migrate = the re-seal pass; contract = GC's existing grace sweep.
- **Version-skew consequence (I2, stated honestly).** A pre-convergence apple app cannot
  fetch a head once it is re-sealed (it downloads by plaintext chunk hash; sealed chunks
  are store-keyed by ciphertext hash) — the same within-major degrade the ratified M2
  pre-bind re-seal already accepts for old readers of a newly-bound set. No data is
  destroyed: the old device's local copies stay on its disk, superseded plaintext rows
  persist through the GC grace window, and every current client reads both shapes.
- **Per-file display state.** The converged apple UI becomes the **first consumer of
  `SyncState::to_display()`** (§ Per-file sync-status display): in-process means no
  status-pipe IPC is needed — the FFI host surfaces per-file display state directly, and
  apple keeps routing the label through the shared `sync_display_state_label`.

**Implementation status today (updated 2026-07-13).** **B1 (the engine host) and B2 (the macOS
cutover) are BUILT.** macOS byte-sync now runs on the shared engine in-process: binding a location
on the Folders page starts a resident engine for that set immediately, and every upload is
sealed. The bespoke Swift `SyncEngine`, the FSEvents `DirectoryWatcher`, the launchctl
`SyncDaemonManager`/`SyncDaemonConfig`, and the SwiftData `SyncFile`/`SyncAnchor` models are
**deleted**. Photo-library ingress runs through the host's sealed per-file `ingest_file`
(metadata is stripped on the *ingress*, where the copy is ours — never inside the engine, which
must keep a synced folder byte-exact; the strip itself is now shared across every app, and covers
**six container formats, not all of them** — WebM and PDF remain declared residuals; see the
coverage table in the metadata-strip note below before relying on it), and the iOS
`BGProcessingTask` pull leg called a one-shot pass over the bindings (retired with the resident
half, below). **B3 landed 2026-07-13:** the photo ingress no longer names a hardcoded
set — it resolves one through the shared `fauna_folders_machine::photo_library` resolver (adopt an
existing `"photos"` set, else create the wizard-preset "Photo Library" folder through the
ordinary `FolderWizardMachine`), behind the single UniFFI face `photo_library_set(...)` that android
consumes too; the device-local binding lives in `<state_dir>/photo-ingress.json`, deliberately **not**
in the location map (a watch-dir entry there would spin a resident engine + reconcile over a photo
library). Set-model authority: `../ui/folders.md` § Photo backup → *Target set model*, § Where logic
lives. **B4 landed 2026-07-14:** iOS scene-phase start/stop — `suspend()` stopped every resident
engine through a host-level pause, `resume()` restarted them (the host itself was kept across the
transition — the BG one-shot pass needed it). ⚠ Deliberately NOT `stop_engine`/`start_engine`: those persist the location-map removal,
so using them for a temporary backgrounding pause would silently lose a binding if iOS killed the
process mid-background instead of resuming it — the same silent-feature-death shape as B2's dead
`UserDefaults` key and B3's rejected change records. **The resident half B4 paused was retired
2026-09-25:** once the agent cutover moved every location binding out of the app, nothing wrote the
location map, so no resident engine ever started and the iOS pull leg's pass iterated nothing; the host
now carries only the construct-run-drop calls, and the B4 pause/resume, the one-shot pass, the
`social.fauna.sync.pull` background task and the per-path `SyncStateObserver` push went with it. **B5 CLOSED AS N/A (2026-07-14, user-ruled):**
no legacy plaintext manifest exists to migrate — B2's recon found the old Swift *folder* upload
path had never executed even once in production (zero writers of the `UserDefaults` key its
watcher read, at any point the current UI shipped), and B3 found photo backup's `changes.record`
was rejected `not_found` on every attempt — so neither apple write path ever produced a real
manifest. No migration was built; there is nothing to migrate or drop. What landed:

**Ingress metadata-strip convergence — CONVERGED across every app (windows landed 2026-07-23).**
⚠ **Convergence is about *which app runs the strip*, not about which formats it covers — read the
coverage table below before treating "the metadata is stripped" as true of an arbitrary file.**
The *blob-upload*
path is already shared and correct on every app: it runs `fauna_media::pipeline::process_and_seal`
→ `process_media` → the lossless `strip_exif_iptc`. The **photo-backup ingress did not** — it reaches the sync
plane via `ingest_file`, which never calls `process_media`, so each *native* app hand-rolled its
own stripper that decodes and re-encodes the pixels, permanently degrading the backed-up copy — which
is the copy a restore returns — a fidelity bug, not just divergence (priorities #1/#2/#4).
`libs/fauna-media` exposes **`strip_metadata(raw) -> Vec<u8>`**, the strip-only half of
`process_media` (same shared `strip_exif_iptc`, no thumbnail/C2PA cost), proven byte-identical on
metadata-free input and byte-exact round-trip after injection (`libs/fauna-media/tests/process_test.rs`),
exposed over UniFFI as `strip_media_metadata` (`libs/fauna-ffi/src/media_upload.rs`).
**android CONVERGED 2026-07-20**: `ExifStripper.kt` (Bitmap → JPEG quality-95 recompress) now
delegates to the shared face — both android ingresses (`PhotoBackupEngine.kt` MediaStore,
`WatchedDirectoryManager.kt` SAF) and its two conversations/feed attachment-picker call sites
converge in one change (`mimeType` kept in the signature only to avoid touching every call site; the
shared face sniffs MIME itself). **apple CONVERGED 2026-07-22**: `ImageMetadataStripper.swift`
(ImageIO decode/re-encode, which also dropped C2PA) deleted; `PhotoBackupEngine.swift`'s
`stripImageMetadata(at:)` now calls the shared `stripMediaMetadata` UniFFI face directly (no type
gate needed — bytes the strip does not cover pass through unchanged; that set is **not** just
"non-image/unparseable", see the coverage table below). **windows CONVERGED
2026-07-23**: `ExifStripper.cs`'s `BitmapEncoder` decode/re-encode is gone — `ExifStripper.Strip`
now delegates to `FaunaFfiMethods.StripMediaMetadata` directly (moved from the `FaunaApp` WinUI
project to `FaunaApp.Core.Helpers` so it's unit-testable; `ExifStripperTests.cs` pins the
byte-identical round-trip). **web has no photo-backup ingress at all** (a browser has no persistent
OS photo-library access the way PhotoKit/MediaStore do) — `exif.ts`'s `stripExif` was leftover dead
code from a pre-`processAndSealPublicPost` feed-compose path, superseded and orphaned (zero callers,
confirmed via `git log -S`), **deleted 2026-07-20**, not converged (there was nothing to converge:
web's actual upload path already runs the real shared `process_and_seal` → `process_media`). The
fifth duplicate, linux's `apps/fauna-linux/src/exif.rs` (an `exiftool` shell-out that silently no-op'd
when the binary was absent), was **deleted 2026-07-20**: its one caller pasted a GDK-re-encoded
clipboard PNG that carries no EXIF, and that attachment's upload already strips losslessly via the
blob path.

**Strip coverage — the exact set (corrected 2026-08-01; mp4/mov added the same day).** Owner of this claim: this section; the
executable source of truth is `fauna_media::process::strip_coverage(bytes) -> StripCoverage`, pinned
format-by-format in `libs/fauna-media/tests/process_test.rs`. Every strip is **lossless** — metadata
is removed from the container without decoding or re-compressing the media payload, so a file with
nothing to strip returns byte-identical. **Convergence (which app runs the strip) and coverage (which
formats it understands) are different axes, and conflating them is what let this drift for a year:**
the prose above said "EXIF/GPS is stripped on the ingress" while the code stripped **two** formats of
the eight it could recognise, with the rest falling through a `_ => body` catch-all
(found 2026-07-23; closed
2026-08-01).

| Container | Coverage | What is removed / why not |
|---|---|---|
| JPEG | ✅ stripped | APP1 (Exif) + APP13 (IPTC); APP11 (JUMBF/C2PA) preserved. A still the JPEG parser rejects comes back untouched and the outcome says `Residual`, like every other arm's bail (fixed 2026-09-29; it had reported `Stripped`) |
| PNG | ✅ stripped | `eXIf` / `tEXt` / `iTXt` / `zTXt` chunks; `caBX` (C2PA) preserved |
| WebP | ✅ stripped **2026-08-01** | RIFF `EXIF` + `XMP ` chunks. Recognised by the sniff since the first lift but never stripped — the original silent gap |
| GIF | ✅ stripped **2026-08-01** | Comment (`0x21 0xFE`) + Application (`0x21 0xFF`, where XMP rides) extension blocks; Graphic Control kept (render state, not metadata) |
| HEIC / HEIF / AVIF | ✅ stripped **2026-08-01** | The `Exif` item and the XMP item (type `mime`, content_type `application/rdf+xml`), **located via `iinf` → `iloc` and zeroed extent-by-extent**. Item-level surgery, deliberately *not* the box-level neutralisation the mp4 row uses: a HEIC's top-level `meta` box is **structural** — it holds the `iinf`/`iloc` that locate the picture itself — so `free`-ing it would destroy the image. `Container::Heif` and `Container::IsoBmffVideo` are separate variants routing to separate code for exactly this reason; do not merge them. Nothing is inserted, removed or moved, so every `iloc` offset stays valid by construction rather than by fixup, and the zeroed item keeps its declared length (a reader following it finds no valid TIFF header and moves on). Only `Exif` and XMP are touched — every other item is content (primary image, thumbnail, alpha plane, derived image) and zeroing one would punch a hole in the photo. Fail-safe on any structural surprise (missing `meta`/`iinf`/`iloc`, a field width the spec does not permit, an extent past EOF, or `construction_method` 2 — `item_offset`): the user's original bytes are returned untouched. **And fail-safe on one surprise that is not structural at all (fixed 2026-08-15): a file read *correctly* that declares a metadata extent overlapping another item's bytes.** Every other guard defends against misreading the layout; this file is flawless in every field — valid lengths, offsets in range, `construction_method` 0 — and the whitelist cannot object because the item really is `Exif`. Zeroing it would destroy the picture, so an overlapping layout is refused rather than acted on (metadata-on-metadata too, since no real writer emits one; content-on-content is ignored — the strip never writes there). The realistic author is a buggy camera app or transcoder, not an attacker, and what raises it above a curiosity is where the output goes: the backup stores the zeroed copy. **The same guard is stated positively too (fixed 2026-09-29): a metadata extent must lie wholly inside an item-data payload — a top-level `mdat` box's, or `meta/idat`'s for a `construction_method` 1 item.** Comparing only against other items' extents let an `Exif` extent declared over the file's own boxes (`ftyp`, the head of `meta`) through, and zeroing it left a file no reader can open; naming the payloads metadata may live in, rather than listing the boxes it must avoid, leaves no unlisted box to slip past. |
| mp4 / mov / m4a / 3gp | ✅ stripped **2026-08-01** | `moov/udta` (the `©xyz` and `loci` location atoms), `moov/meta`, and the Adobe XMP `uuid` box — **neutralised in place, never removed**: the box's 4-byte type is overwritten with `free` (ISO/IEC 14496-12 ignorable padding) and its payload zeroed, leaving every size byte untouched. Removing the box instead would shift `mdat` and silently invalidate every `stco`/`co64` chunk offset; in-place neutralisation deletes that entire failure class rather than attempting the fixup. Only `moov`/`trak` are descended into — `mdat` is never walked or written. A non-XMP `uuid` box is left alone (it is the generic vendor-extension box, and some carry data a player needs). |
| WebM / Matroska | **declared residual** | `Tags` / `Attachments` use EBML variable-length integers; no EBML parser in the dependency graph |
| PDF | **declared residual** | XMP metadata stream + Info dictionary; no PDF parser in the dependency graph |
| unrecognised bytes | **declared residual** (was "— no carrier" until 2026-08-15) | The container could not be identified, so **we cannot say what it carries**. Passing through untouched is still deliberate — mangling bytes we do not understand is how a backup ingress destroys a file — but the *claim* changed: failing to parse bytes is not evidence about their contents, and the family this was most wrong about is the one that defines Exif. The sniff has no TIFF branch, so **every DNG — Apple ProRAW, Android RAW — landed here and was affirmatively reported as carrying no metadata while keeping its full Exif block** (fixed 2026-08-15). There is now no third "nothing to worry about" coverage value at all: `StripCoverage::NoCarrier` is **removed**, so the taxonomy is exactly *we stripped it* or *here is why we did not*. |
| JPEG with an appended container (Google/Samsung **Motion Photo**) | ✅ stripped **2026-08-15** — residual if the appended half is one | A Motion Photo is a single `.jpg` with a **complete mp4 appended after `EOI`**, and MediaStore hands it to the ingress as `image/jpeg` — so the outer container alone cannot decide the dispatch, and a composite handled as one container is only half stripped while coverage reports `Stripped` (fixed 2026-08-15). The strip now finds the JPEG's true end by walking its marker structure (never by scanning for a bare `FF D9`, which occurs freely inside entropy-coded data — and stepping `0xFF` fill bytes one at a time, since any number may precede the `EOI`; stepping them in pairs walked past a padded `FF FF D9` into the video, fixed 2026-09-29), splits still from trailer **before** the JPEG parser sees anything, and strips each half through the same dispatch. A composite is only as stripped as its least-stripped half: if the appended container is a declared residual, so is the whole file. |

A **declared residual** means the file passes through **carrying its metadata** — byte-identical,
never mangled — and `strip_coverage` reports `Residual(reason)` rather than success. That
distinction is the point: a caller can tell "we stripped it" from "this format keeps its metadata",
instead of inferring completeness from a successful return. Structurally, the container taxonomy is
now an enum whose coverage `match` is exhaustive, so **a newly-sniffed format cannot silently join
the residual set** — it fails to compile until someone decides. The residuals are **not** yet
surfaced in any app UI.

**Known limit of the HEIC pins, recorded rather than carried as debt.** Every HEIC/HEIF fixture in `process_test.rs` is **hand-assembled**;
no real iPhone photo is in the suite, and none can be — there is no HEIF encoder anywhere in the
dependency graph (`image` is pinned to `["jpeg","png","webp","gif"]`, `Cargo.toml:220`), so a real
file would mean committing a binary fixture, which this suite has deliberately avoided from the
start. That is an acceptable limit *for this walker specifically*: the structure it parses is
`meta`/`iinf`/`iloc` — box headers and integer field widths — and it never touches HEVC pixel data,
so a hand-built file exercises exactly the code under test. The residual risk is the one every
hand-built fixture carries (fixture and walker sharing one misunderstanding), which is why
`heic_strip_preserves_the_primary_image_and_every_byte_offset` and the overlap pin both
locate the primary image **by content** rather than by re-running the walker's own `iloc` parse.
Do not "fix" this by weakening that technique.

⚠ **`strip_coverage` is a *capability* claim, not a statement about your bytes — read
`strip_metadata_with_coverage` instead** (fixed 2026-08-15). `strip_coverage(raw)`
sniffs the container and reads the table above; it never runs the strip. Every arm of the strip is
deliberately fail-safe, so for exactly the files the walker *declined to touch* — a malformed mp4, a
GIF with an unrecognised block introducer — it answered `Stripped` while nothing had been removed.
The outcome-carrying face `strip_metadata_with_coverage(raw) -> (Vec<u8>, StripCoverage)` runs the
strip and reports what actually happened, reporting `Residual` on a bail and on a composite whose
appended half was not stripped. **The app that eventually warns "this file may still carry its
location" must call that one**; the same weakening also covers the two honest over-claims the
2026-08-01 review graded (timed-metadata tracks such as GoPro GPMF and DJI, whose samples live in
`mdat` and are never walked; and non-XMP vendor `uuid` boxes, deliberately left alone). Neither face
is on the UniFFI/wasm faces yet — additive when a consumer appears.

**Related correction, same change:** ISO-BMFF sniffing now splits on the 4-byte major brand, so
HEIC/HEIF/AVIF report `image/heic` instead of `video/mp4`. Every `ftyp` container was previously
called a video, which mislabelled iPhone photos on the wire and excluded them from every
`mime.starts_with("image/")` branch (C2PA probe, thumbnail render). The nest already allow-lists
`image/heic`/`image/heif` in `SAFE_MEDIA_TYPES`, so the served content-type is unaffected.

- **`libs/fauna-ffi/src/sync_engine_host.rs` — `FfiSyncEngineHost`** (default-on
  `sync-engine-host` feature): construct-run-drop engine work on one worker thread — the sealed
  per-file `ingest_file` (staging-dir engine → `upload_file` → delete temp; no watcher, no
  reconcile), `file_states` (per-file display state), `transfer_backlog`, and the restore walk.
  **No resident engine** (retired 2026-09-25 — every location binding is the agent's). Built
  through `FfiNestClient::sync_engine_host(...)` — one factory for macOS, iOS and android —
  which reuses the app's live WS-RPC connection and takes the conversations rail's shared
  per-actor `MlsEngine`.
- **`libs/fauna-sync-engine/src/engine_lifecycle.rs`** — the *shared* engine **build**
  lifecycle behind it: device registration, the **fail-closed** content-key binding
  (`decide_engine_content_binding` — lifted out of `apps/fauna-linux/src/sync.rs` so the
  security-critical decision has ONE implementation, priority #2/#4), the owner-`BackupKey`
  seal root, and the stable per-device sync id (the in-process drive — the pre-bind re-seal
  catch-up, the one-shot pass, the resident loop — and the location-map binding shape were
  retired 2026-09-25 with the host's resident half). **The one builder since 2026-09-28** —
  every engine the desktop sync agent runs is built here too, a cross-nest set from its
  holder's custody record (`on-demand-files.md` § Shared sets on a capability host → *One
  mechanism*, question 2, owns the arm). Gated by the default-off `engine-lifecycle` feature,
  which takes only the base, fauna-mls-free custody resolvers, so the lean deployments (the Go
  mail-bridge) compile none of it and the bearer-only agent links no conversations/MLS graph.

  **Three shared modules, three jobs** (they landed the same day from two parallel development
  sessions and are deliberately layered, not overlapping): `engine_host` **multiplexes** N engines on one
  worker thread; `always_resident` **drives** an already-built engine (initial convergence,
  owner-only re-seal, watch/debounce/rescan) and is shared with the bearer-only
  `fauna-sync-agent`; `engine_lifecycle` **builds** an engine from nest state for the
  in-process `fauna-ffi` hosts and the agent alike (since 2026-09-25 it drives none — the
  resident drive is the agent's).
- **`SyncState::to_display()`** — the 8→6 engine→display map (below) now exists, with
  `FfiSyncEngineHost::file_states` as its first consumer.
- Verified: `fauna-ffi` **compiles for `aarch64-apple-ios`** (rusqlite + notify/kqueue +
  walkdir all build for the iOS target — the open question B1 carried is closed).

Tracks, in dependency order (tracked internally): ~~**B3** the wizard-preset photo set~~
(**DONE 2026-07-13**) → ~~**B4** iOS scene-phase lifecycle~~ (**DONE 2026-07-14** — see above) →
~~**B5** the plaintext-record re-seal migration~~ (**CLOSED AS N/A 2026-07-14** — no legacy manifest
ever existed to migrate, see above). **The apple B-track is now fully closed.** **Linux still runs
its own copy of the *build* half** of the lifecycle it donated (its drive loop already delegates to
`always_resident`): migrating `apps/fauna-linux/src/sync.rs` onto `engine_lifecycle` is a
mechanical follow-on, entrusted rather than done alongside B1 because it must be compiled +
tested on a Linux machine.

## Implementation status today (android byte-sync)

**RESOLVED 2026-07-16.** Android was the **last non-conforming byte-sync
deployment** (§ Control Plane Principle) and the only app that had never run the shared engine. Its
bespoke path — `core/ChunkedSyncEngine.kt` (66 lines), driven by `core/PhotoBackupEngine.kt` (MediaStore
ingress) and `core/WatchedDirectoryManager.kt` (SAF ingress) — orchestrated the shared Rust chunker over
the raw `/api/v1/chunks` + `/manifests` routes rather than running the engine, so it re-derived — and
got wrong — three things the engine owns. All three are now fixed *by construction* by the convergence,
not individually:

1. **Chunks rested in PLAINTEXT** (`uploadChunked` uploaded `extract_chunks` output with no seal and no
   `BackupKey` — the § Content-Addressed Storage invariant was **violated on android**, precisely
   macOS's pre-B2 condition). **Fixed:** every chunk now rides the shared engine's sealed pipeline.
2. **The recorded `manifest_hash` was the WHOLE-FILE hash, not the manifest-blob key**
   (`ChunkedSyncEngine.kt` discarded the hash `POST /api/v1/manifests` returned and recorded
   `manifest.file_hash` — the live bug [`backup-restore.md`](backup-restore.md) § 9 names as gating
   record-time `manifest_hash` validation). **Fixed:** the engine records the correct manifest-blob key
   by construction; there is no longer a second, wrong code path that could re-derive it.
3. **`PhotoBackupEngine` hardcoded `FOLDER = "photos"`** — a set no production code had ever created,
   so every photo's `changes.record` was rejected `not_found` and android photo backup had never
   persisted a photo. **Fixed:** the ingress resolves its set via the shared `photo_library_set`
   resolver (adopt-existing-`"photos"`-else-create-`"Photo Library"`; set-model authority
   [`../ui/folders.md`](../ui/folders.md) § Photo backup) before every pass.

**The shape landed.** Android now runs the already-built `FfiSyncEngineHost` (default-on
`sync-engine-host`) over both ingresses via `ApiClient.syncEngineHost()` (built once per session, held,
closed on `clearAuth`). Neither android ingress is a watchable folder — MediaStore is an OS library, and
a SAF binding is a `content://` tree with no path — so **both** route through the **library-ingress**
shape (*apple defines the shape; android mirrors it*): stage the exported/read bytes to a temp file,
`ingest_file`, delete the temp. A location binding over a *user-picked* tree is **not** android's
shape and no VFS seam was added — but that sentence was over-read 2026-09-25 as "android keeps no
replica", and the user refuted that reading 2026-09-26 (peer transfer: [`p2p.md`](p2p.md) § Cross-user
shared-set transfer → Implementation status today, the android paragraph): android is **owed an
on-demand replica over an app-owned root** — the SAF `DocumentsProvider` binding over `provider_face`,
designed in [`on-demand-files.md`](on-demand-files.md) § Android SAF DocumentsProvider binding
and hosted in-process by the on-demand host, never by this ingress host. EXIF stripping stays on the ingress, never in the engine (the apple B2 lesson: the engine
must keep a user's files byte-exact). `core/ChunkedSyncEngine.kt` and the bespoke `/api/v1/chunks` +
`/manifests` HTTP routes are deleted.

**The one open migration question is resolved N/A.** Unlike photo backup (defect 3 meant
`changes.record` was rejected on every attempt, so nothing was ever written — no legacy to migrate,
inheriting apple's B5-as-N/A reasoning), the watched-directory path was **live and wired**: it recorded
against a real user-chosen set over WS-RPC and succeeded, so B5's N/A ruling could not be assumed the
way it was for apple. **User-answered live (2026-07-16): no deployment had ever run a watched-directory
scan.** So there is no plaintext back-catalogue to re-seal, and the § M2 re-seal trio (re-upload sealed
→ verify → best-effort `fauna.sync.changes.supersede`) is not needed. If a pre-cutover user is later
found, re-open this question — the candidate repair is unchanged: clear the derived `sync_files` cache
(reconstructible; `watched_directories`, the persisted SAF grants, is the one genuinely irrecoverable
table and was not touched) so the next scan re-uploads sealed and correctly-keyed.

**Verification.** Compile-verified via host-`.so` bindgen: `:app:compileDebugKotlin` and
`:app:compileDebugUnitTestKotlin` both `BUILD SUCCESSFUL`. The **mechanism** (chunk → seal → correct
manifest key → record) is shared Rust already tested end-to-end as linux/windows/apple's production
path. What is **not** proven headlessly: the android wiring itself — Robolectric over FFI-touching code
needs host JNA (the `android-test-harness` track owns that gap) and the emulator e2e is emulator-host-gated. No
test coverage existed for the retired path either (zero unit/Robolectric/e2e over all three files), so
this is a net-new-coverage change, not a regression risk against an existing suite.


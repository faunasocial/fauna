# File versions — target state

Owns: file-versions
Status: ratified — projection + wire + restore BUILT; retention MODEL ratified 2026-08-17 and both halves now BUILT — reclamation nest-side 2026-08-17 (app legs: tui/linux/web/android/macos/ios/windows all landed, windows verified + closed 2026-09-09) and metering 2026-08-18, the alpha no-user-data-loss gate having CLEARED in-session 2026-08-17 (§ Retention (5); per-surface detail in § Implementation status today)
Authority: version history as a projection over `sync_changes` (every recorded change IS a version), the `fauna.files.versions.{list,get}` wire surface + audience scoping, restore-as-re-point (+ the recording device's local apply and its three load-bearing orderings), retention posture; defers the page surface to `../ui/media.md` (`file-version-history`), conflicts to [`conflicts.md`](conflicts.md), the chunk/manifest pipeline + `path_hash` derivation to [`file-sync.md`](file-sync.md) § Content-Addressed Storage, sealed names/paths to [`path-sealing.md`](path-sealing.md), and snapshot/retention/GC behavior to [`backup-restore.md`](backup-restore.md)

Split verbatim out of `file-sync.md` § File Versions on 2026-08-02 (a stub there redirects;
prior history: `git log --follow docs/goal/behavior/file-sync.md`).

**Every recorded change IS a version** — the section below is the projection contract; § Restore
is the write half; § Implementation status today is the per-surface build state.


**Every recorded change IS a version** (ratified 2026-07-09): the version history of a
synced file is a **projection over the append-only `sync_changes` table**, not a second
bookkeeping table. `sync_changes` already carries everything a version needs — `seq`,
`path_hash`, `folder_id`, `manifest_hash`, `size_bytes`, `created_at`,
`content_key_version`, `superseded_at` — is written by the production record path
(WS-RPC `fauna.sync.changes.record`), and its
non-superseded manifests are GC-pinned (`bins/fauna-nest/src/backup/gc.rs` step 2b), so
every historical version's chunks stay durable and cost only their changed chunks
(content-addressing). Because the projection reads history that already exists, version
history is **retroactive**: every change ever recorded is a listable version.

## Wire surface

- `fauna.files.versions.list` — the version history of one file, oldest→newest.
- `fauna.files.versions.get` — one version's metadata; missing → `fauna.files.not_found`.

Both are metadata-only: a version's bytes are reconstructed client-side from its
`manifest_hash` via `GET /api/v1/manifests/{hash}` + `GET /api/v1/chunks/{hash}`
(`file-sync.md` § Content-Addressed Storage), with the standard fail-closed sealed-manifest unseal at
`fetch_manifest` (`file-sync.md` § Manifest privacy) applying unchanged.

Projection semantics (each rule is load-bearing):

- **A version row** = a `sync_changes` row for the `(folder, path_hash)` with
  `manifest_hash IS NOT NULL` (a `delete` records a tombstone in the history, not a
  restorable version) and `superseded_at IS NULL` — superseded rows are excluded exactly
  like the `changes.list` feed excludes them (their chunks may be GC-reclaimed; the M2
  re-seal that supersedes a row records a live re-sealed twin, so no content leaves the
  history).
- **`version_num` = the recording row's `seq`** — stable and unique, never renumbered.
  A dense 1..N ordinal would silently shift under a later supersede of a middle row;
  `seq` is already client-visible via `changes.list`. Display ordinals are derived
  client-side from list position. (The pre-projection dense-`version_num` semantics
  carried no compat constraint: the `file_versions` table had no production writer, so
  the surface returned empty everywhere.)
- **Scope + access control:** requests carry the folder name (`folder`,
  wire-optional for compat); the handler resolves it against the caller's readable sets
  (`folder_authz::enumerate_readable_folders`) **filtered to label-audience grants
  (owner + roster member — `is_label_audience()`; ruled 2026-07-30)** and reads only
  those sets' rows. The Q5 admin-discovery grant does **not** reach version history:
  path existence + manifest + author + timestamps are content metadata, not the
  discovery metadata that grant is scoped to, and an admin-reachable reply would be an
  online existence oracle for a guessed `path_hash` (the audience rule is owned by
  `../architecture/encryption-at-rest.md` § Carve-outs). With `folder` absent, the
  projection unions across all the caller's audience sets containing the `path_hash`.
  This closes two pre-projection gaps: the surface carried no per-set authz, and a bare
  `path_hash` is ambiguous across sets (the hash is set-relative, so two sets can share
  it).
- **Reply items carry `content_key_version`** (nullable, additive) — the M2 content-key
  generation the version's chunks were sealed under, echoed from the `sync_changes`
  column so the reader selects `key_for(version)` (see Restore below).
- **Every ordinary folder has version history.** Since the head
  unification (2026-08-17, `file-sync.md` § Membership) an ordinary backup-type
  folder records to `sync_changes` like any other, so its files list versions too (the handler has no
  type gate). Only a **reserved** (`__*`) backup destination set stays on the latest-per-path
  `backup_custody` projection and lists none; its recovery surface is `backup-restore.md`.

## Restore

**Restore = re-point, never re-upload** (ratified 2026-07-09): restoring version N is the
client recording an **ordinary `modify` change** via `fauna.sync.changes.record`, carrying
the historical version's `manifest_hash`, `size_bytes`, and — the sealed-set edge — its
historical **`content_key_version`**. Properties, each by construction:

- **No byte movement.** The historical manifest and chunks are already in the store and
  GC-pinned; the record is metadata-only, so restore works identically from control-plane
  apps (web) and full sync engines. (A never-registered device self-heals registration
  on the record — the `record_self_healing` precedent in `libs/fauna-media-machine`.)
- **Propagates as a normal remote change.** Member devices apply it like any other
  `modify` (download the manifest's chunks, most already local); no new wire kind, no new
  engine path.
- **Sealed sets open historical generations.** A bound (MLS-keyed) set's historical
  manifest may be sealed under an older retained content-key generation; the restore
  record carries that generation verbatim and readers select `key_for(content_key_version)`
  from the retained set (the drain-after-rotation re-seal precedent). Restore never
  re-seals under the current generation — the bytes are untouched. (Reader-side hazard to
  honor: the `FolderContentKeys::merge`/`key_for` same-version-merge ambiguity,
  `mls-group-key-material.md` § M2 *Same-version candidates*.)
- **The one version that is not re-pointed: one recorded under a previous identity of the
  account, whose bytes rest under an owner root.** After an identity succession such a version
  is restored by opening and re-sealing it, never by re-signing its manifest — the rule and its
  reason are [`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
  § Writer-signed change records, ruling (8)(d); which surfaces perform the re-seal and
  which refuse with the reason is that doc's § Implementation status today.
- **Every restore door takes one verified version and one decision.** The Media restore, the
  sync agent's verb and the conflict review list's *use the other version* all restore a
  version the verified listing admits, by the same branch (re-point, re-seal, or refuse with
  the reason) — same doc, ruling (10), which also rules what makes a stamped version safe to
  re-point and how a conflict's retained losing version comes to be listed at all (built
  2026-10-03 — that doc's § Implementation status today).
- **Restore is reversible.** The restore record becomes the new head AND a new version
  entry (append-only history); the pre-restore head remains listed and restorable. This is
  why the app UI needs only a lightweight confirm (media.md § User actions), not the
  irreversible-action ceremony.
- **Quota-neutral in the common case.** The metered record path computes the supersede
  delta against the prior head (`record_sync_change_metered`); restoring an older, smaller
  version credits quota back like any shrink.
- **The recording device must re-point its own local copy** (ratified 2026-07-10). "Propagates
  as a normal remote change" covers every device *except the one that restored*: catch-up
  deliberately skips a device's own changes — nest-side for the always-on daemon (it passes its
  `device_id` as `changes.list`'s exclude param) and client-side for `SyncEngine`, whose
  `apply_self_echo` only refreshes the merge base and never rewrites the file. So a restore
  issued *from* a device that also holds the file locally leaves that device — the very one the
  user is looking at — showing the pre-restore content, forever. A client that both records a
  restore **and** owns a local copy therefore owns the local apply as a second, explicit step:
  re-point the local sync-state row at the historical `manifest_hash` / `size_bytes` /
  `content_key_version`, then invalidate the cached bytes. Apps whose restore surface has no
  local file (the Media library on all 7 apps — a nest-side view fetched by hash) have
  nothing to apply and are unaffected.

  On an on-demand (cfapi) client this is the invalidation path: re-point the row,
  `supersede_placeholder` (free the cached bytes and leave the placeholder describing the restored
  version — its size and mtime, since cfapi asks for exactly the placeholder's size on the next
  open, and a bare dehydrate keeps the current one), emit `FileStatusChanged{CloudOnly}` — the
  next open re-hydrates the historical bytes, because the hydration `FETCH_DATA` resolves its
  manifest from that same local row.

  **Three orderings are load-bearing.**

  1. **Record on the nest first, apply locally second.** The nest is the source of truth; the
     reverse order would leave a device serving content the nest never agreed to. The cost is a
     crash window: a crash (or timeout) between the two leaves the nest correct and *this* device's
     row stale. Both a **placeholder** row (no local bytes) and a **hydrated** row heal at the next
     service start, by the same start-up `changes.list` fold, but by *opposite* mechanisms because
     the risk differs. A placeholder the fold **re-points** in place (its manifest no longer matches
     the nest's head; no local bytes to lose), and a cfapi host then re-describes its on-disk
     placeholder — size and mtime — as the new version. A hydrated row's bytes are on disk and may carry an
     unsynced local edit, so the fold **never rewrites it** — it *reports* the row, and the on-demand
     host invalidates it via ordering 3 below. If that invalidation's dehydrate refuses (a locally
     edited file), the row stays hydrated and the divergence becomes an ordinary conflict; otherwise
     the file becomes a placeholder at the new head and the next open re-hydrates the restored bytes.
  2. **Within a *restore's* local apply: durable row before volatile cache.** Re-point the sync-state
     row, *then* drop the cached bytes. A failed free then leaves a correct row and a stale
     cache — never a truncated file: an open of a placeholder that describes another size than its
     row's version is refused, not served (`on-demand-files.md` § On-Demand Files). The reverse would
     leave a freed placeholder still pointing at the **old** manifest, so the next open actively
     re-materializes the pre-restore bytes — strictly worse than a stale cache. This holds because a
     restore records content the user *chose*: there is no local edit to protect, so the row is
     authoritative and leads.
  3. **Within a stale-hydrated *invalidation*: free the bytes before the row — the inverse of 2.**
     When the trigger is not the user's own restore but the fold discovering that a **remote** change
     moved the head under a hydrated file, the on-disk copy may be racing a **local edit that has not
     yet been uploaded** (the uploader is asynchronous — a write is observed, queued, and pushed, so
     there is always a window in which the newest bytes exist only on this disk). Here the free is
     the **gate, not the follow-up**: run it *first*, and only re-point on success. On cfapi the free
     is one `CfUpdatePlaceholder` that also re-describes the placeholder as the new version (size and
     mtime: cfapi asks for exactly the placeholder's size on the next open, so a placeholder left at
     the old size served a grown file truncated — which was then uploaded over the real edit) and is
     refused unless the file is in sync. A dirty file makes
     the free refuse, so the edit survives and the row stays `Synced` — and because a tracked file's
     change **must** be uploaded (`on-demand-files.md` § On-Demand Files → *Sync direction*), what happens next is an
     ordinary two-sided divergence, resolved per [`conflicts.md`](conflicts.md) like any other. Re-pointing first would
     strand the edit under a `Placeholder` row claiming no local bytes. A crash between the freed
     bytes and the re-point is safe and idempotent — the row still resolves the old manifest, so an
     open before the retry gets the old version, or is refused when the placeholder already
     describes a different size (never a truncated file); and the next fold, which at a restart
     runs before any open is served, retries.

     ⚠ **The pre-2026-07-14 wording justified this gate by asserting "an on-demand host is
     download-only, so nothing else guards it."** That premise is retired (`on-demand-files.md` § On-Demand Files →
     *Sync direction*): an on-demand host is two-way, so the edit *does* have another guardian. The
     **ordering rule is unchanged** — the free still gates the re-point, because an
     upload-in-flight window exists regardless — but it no longer rests on the host being
     one-directional.


## Retention

**The retention model — RATIFIED 2026-08-17 (the retention-model pass; refutable at build time). Reclamation first, metering second — both halves are
now BUILT:** reclamation (rulings 1–3) nest-side 2026-08-17 (slice 2, same day; app-side
knobs landed on tui/linux/web/android/macos/ios — windows still owed, see
§ Implementation status today), and metering (ruling 4) 2026-08-18 (slice 3). Before the
metering build landed, retention was unmetered: retained bytes counted against no quota —
neither the owner's nor the recording member's (the *escape*). **(4) below closes
that gap** — the escape it describes no longer exists.

**(1) Where the bound rests: a per-folder SIBLING policy, never a re-map.**
`folders.version_retention` — a new column beside (never inside) `folders.retention_policy`
— holding the bounds pair `VersionRetention { max_versions_per_path, max_age_days }` in the
same JSON discipline as its snapshot sibling. Semantics mirror § 8's armed bounds engine
exactly: the two bounds intersect (a version survives only if it is among the newest
`max_versions_per_path` listable versions of its path **and** younger than `max_age_days`),
a `0` in a bound means *that bound is unset*, an absent/`NULL` column means **keep
everything** (§ 8b's three-state rule: `NULL` is the honest resting value, where every
folder rests until its owner chooses), and an unparseable value refuses and logs, never
guesses. Two grounds for the sibling column: **(a)** § 8 forbids silently re-mapping one
policy family onto another, and quietly widening the *armed* `retention_policy` column to a
second plane is that violation in time rather than in vocabulary — every existing configured
`max_snapshots` would start binding version history its owner never asked to bound;
**(b)** the two knobs are genuinely different products (7 snapshots ≠ 7 versions per path)
and § 8b's per-place model already expects sibling knobs, each with its own column. The
owner sets it via `fauna.folders.update` (carried whole, like `nest_place`); both
projections carry it back on `FolderSummary` with the same audience grading as
`retention_policy`.

**(2) What the evaluator may mark, and the floor.** Evaluation is per `(folder,
path_hash)` over the **listable** population only (`manifest_hash IS NOT NULL`,
`superseded_at IS NULL`, not already in the prune pipeline). The **head is structurally
never a candidate** — the same only-below-head invariant the M2 supersede and the rail
collapse rest on — and the automatic path additionally enforces `VERSION_HARD_FLOOR = 2`:
the head plus the newest prior version always survive, clamped onto the engine's output
exactly as `SNAPSHOT_HARD_FLOOR` is, so a configured policy can thin history but can never
silently turn the undo affordance off. The floor binds the *automatic* path only: the
owner-driven M2 `fauna.sync.changes.supersede` RPC remains the explicit, floorless,
per-path instrument for "this old content must go" (privacy edits), unchanged by this
model.

**(3) The prune pipeline is § 7's three layers, not a bare supersede.** `superseded_at` is
the *terminal* reclaim mark: GC pins a superseded row's chunks only within the ~30-minute
orphan grace (`gc.rs` step 2b), so marking at evaluation time would destroy listable,
restorable content with no recovery window — the data-loss direction. Instead the automatic
prune mirrors the snapshot pipeline state-for-state (same concepts, same windows,
priority #3): the evaluator creates one `VersionBulkPrune` pending action per folder prune
with a **7-day** cancellable `execute_after`, marking its target rows prune-pending
(idempotence + Layer 2); pending versions stay listable and restorable for the whole
window; the executor then **soft-prunes** — targets leave the default `versions.list`
projection but remain recoverable for **30 days** (`purge_after`) via
`fauna.files.versions.undelete`, with `versions.list { include_pruned: true }` as the
recovery browse (additive wire, within-major); only past `purge_after` does the purge step
stamp `superseded_at`, after which the existing pin predicate releases the chunks one GC
grace later. Two safety properties to pin at build: **restoring a pending or soft-pruned
version is always safe** (restore-as-re-point makes its manifest the new head, and GC
reachability is row-set-wide, so a later purge of the old row never touches chunks any live
row references); and **cancel/undelete fully restore the row to the listable population**,
Layer 1-style, never stranding it.

**(4) Metering, second — the retained-accounting model (built 2026-08-18, slice 3).**
Retained bytes are charged, not just pinned: every **charged** version's `size_bytes` —
the listable population (heads included) — counts against the **owner's**
`storage_bytes_used` *and* against the **recording member's**
`folder_member_access.bytes_used` / `byte_cap` — the escape is the member half
being non-optional (the pin is the member create→delete probe: the cap REFUSES while
`bytes_used` carries the retained laps, verified with its owner-quota twin). The
operational rules, each deliberate:

- **A metered content record charges its full `size_bytes`** (the former head stays
  listable as a retained version, so nothing is released at record time) — **unless it
  records the manifest the path's head already holds**, in which case it adds no bytes and
  takes the charge the (path, manifest) pair already carries instead of adding one
  (`succession-cut.md` ruling (11)(g), the owner of that rule; built
  2026-10-03): the row that held the charge is marked `charged = 0` and its member half is
  released, the new row is charged, and the refusals are checked on the *net* charge — zero
  for the owner when the declared sizes agree, so such a record is never refused at a full
  quota. Consequence: a *modify* to new content — shrink included — can refuse at the
  quota; only a delete, an idempotent re-record and a same-manifest record of no greater
  size always succeed.
- **Every client-reachable door that mints a charged row meters it — not only the
  record door.** The pre-resolved conflict report (`fauna.sync.conflicts.report`) mints a
  loser retention row and a winner head row, and the legacy chooser
  (`fauna.sync.conflicts.resolve` with a winner) mints a head row at the winning
  candidate's report-time size. Each row is charged like a metered content record: a
  negative declared size is refused (`invalid_size`; the report refuses a negative size on
  every candidate, since the chooser later mints from it), a charge past the owner's
  ceiling is refused (`storage_quota_exceeded`), and a member reporter's charge past their
  `byte_cap` is refused (`member_cap_exceeded`). All of it is all-or-nothing with the
  conflict row. Without the charge, a writer could rest rows at a size they declared, and
  the release credits below would floor the owner's meter.
  **"Like a metered content record" includes that record's same-manifest transfer (ruled
  and built 2026-10-03; ruling (11)(g) of
  `writer-signed-change-records.md` owns the pair rule).** A row a conflict door mints
  over the manifest the path's head already holds takes the charge the (path, manifest)
  pair carries instead of adding a second, checked on the net charge, through the one
  vet-and-settle pair all three doors share. That is the common shape, not a corner: the
  chooser's winner is normally the version the path already heads, and a latest-wins
  report whose remote side won mints its winner over the remote head — while the report's
  retained loser is bytes the path never listed and is charged in full. A full charge
  there was refused for three reasons: the owner paid twice for one set of bytes until
  retention released a row; an owner at a full quota was *refused* a resolution that adds
  no bytes, leaving the conflict open with no affordance but freeing space; and the
  release side (the flag hand-over, `undelete`) already treats the pair door-blind, so a
  door that double-charges is the only place the pair could hold two charges for
  head-adjacent rows. "The head" is the record door's — the newest row of the path, not a
  delete — read before the door mints anything: a report's loser retention row lands ahead
  of its winner and is no head, so both of its rows are asked against the head the path
  held when the report arrived.
- **A delete is net zero**: the tombstone adds nothing, and the former head keeps
  charging as a retained version — "the delete credit becomes partial" resolves to
  charge-continuity, not a refund.
- **Release = leaving the listable population.** The pipeline's soft-prune credits each
  released row to both counters; the owner-driven M2 supersede credits exactly the rows
  it newly terminates (a row the pipeline already released is marked terminal but never
  credited twice); the GC purge of an already-soft-pruned row moves the meter not at all.
  **The charge follows the manifest, not the row** (ruling (11)(g)): a listable row
  marked `charged = 0` credits nothing when it leaves, and a charged row that leaves while
  an uncharged listable row of its path and manifest remains hands the flag to the newest
  such row instead of crediting — the bytes are credited only when the last listable row
  of the pair goes.
- **Undelete re-charges and never refuses** — recovery must not strand behind a full
  quota; an over-quota resting state is expressible, and only positive-charge records
  refuse. It charges the row only when no listable row of its path and manifest is
  already charged (ruling (11)(g)); otherwise the row rejoins uncharged. Cancel of a
  pending prune moves nothing (pending rows never left the listable population or the
  meter).
- **Folder deletion reclaims the folder's whole charged population** (every
  `manifest_hash`-bearing row not yet superseded or soft-pruned, **and marked
  `charged = 1`** — an uncharged row of a same-manifest pair was never charged, so it is
  not credited) — the head-only sum would leak exactly the retained charges.
- **The counters are floored at zero and approximate in the generous direction** around
  nest-internal writers: the unmetered `record_sync_change` (index writer, snapshots,
  restore bookkeeping, GC) can insert rows that were never charged, and a release credit
  never drives a counter negative.

Sequencing after reclamation was the works-out-of-the-box gate (charging without a
release path would ratchet every owner to the ceiling and wedge there) — satisfied: the
pipeline (3) and the tui affordance both shipped first. This also collapses the
custody-plane asymmetry into uniformity: **the two planes agree** — both charge what they
retain and reclaim on a schedule (custody: T=30 d, `backup-restore.md` § 9 step 0; the
version plane: (3)'s 7-day window + 30-day `purge_after`).

**(5) The gates — CLEARED (user approval, 2026-08-17, in-session).** The alpha
no-user-data-loss ask was put to the user the day the model was ratified and **answered
yes**, with three riders that bind the slice-2 build: **(a) green-field — build no
migrations.** The ruling is a one-time, pre-user reset — the plane holds **no data in any
folder** — so the build assumes an empty plane: no expand→migrate→contract, no legacy
at-rest readers, no compat shims or options for pre-model rows — and any such code found
in the way is deleted rather than preserved. (Scope boundary this license does NOT cross:
client↔nest **wire** compatibility — the alpha carve-out is at-rest only,
`version-compatibility.md`. Satisfied for free here: every wire piece of this design is a
new kind or an additive field.) **(b) Element IDs approved:**
`folder-version-retention-count` / `folder-version-retention-days` on § 8b's folder
editor, plus a recovery-browse/undelete affordance whose IDs are proposed at build —
these enter ui.yaml with the build, citing this approval. **(c) The default-bound
question is CLOSED, not parked:** no nest-wide default ships; `NULL` keeps everything.
With no existing folder data the question carried no data-loss stakes, and the user
directed no further time on it — re-raise only if a future product reason (not a
compat/data reason) demands a default.

**Reserved rails are the one exception, and they collapse their history at write time (ruled
2026-08-05).** The unbounded posture above is a deliberate
trade for sets a user can *reach*: history costs disk, but every version is listable and
restorable, so the bytes buy something. A reserved rail (`__drafts`, `__mls`, `__index`; `__config` was one until it
retired 2026-10-02, the name staying reserved) buys
nothing with them, because its history is reachable by **no wire surface at all** —
`folder_authz::enumerate_readable_folders` skips reserved names in both passes, so
`fauna.files.versions.{list,get}` scope them out, and each rail's own reader takes the newest
row only. Left alone, a rail's every historical blob would stay GC-pinned **permanently**, not
for one GC cycle: `superseded_at` has exactly one other writer in production, the owner-driven
M2 `fauna.sync.changes.supersede` RPC (§ *A version row*), which no rail invokes. So each rail's
recorder marks its own strictly-earlier rows superseded as it writes the new head, and GC
reclaims them one grace window later (cadence: `backup-restore.md` § 9).

Three properties make that lossless rather than a retention policy in disguise, and a fourth
rail must satisfy all three before joining them. **(1) The history is unreachable** — the
audience scoping above, not merely "no UI ships yet". **(2) The rails store whole-state blobs,
never deltas**, so a device catching up on the head alone reconstructs the current state by
construction; a delta rail would need its intermediate rows. **(3) Only rows strictly below the
head are marked**, so the rail's live state is structurally unmarkable — the same invariant the
M2 supersede rests on — and two racing writers stay safe in either interleaving. This is the
axis metering was never the instrument for: the ratchet is the *count* of retained versions, and
a charge with no delete verb could not have released them (`reserved-folders.md` § Drafts Sync).

## Implementation status today (File Versions)

- **Retention metering BUILT (2026-08-18, row 165 slice 3 — ruling 4 live, retained
  accounting):** `record_sync_change_metered` charges every content record's full
  `size_bytes` (a modify — shrink included — can refuse at the quota; a delete is net
  zero); release credits move exactly at the § Retention (3) release points via
  `db/sync_storage.rs::adjust_version_accounting_in_conn` (soft-prune and the owner M2
  supersede credit owner + recording member; undelete re-charges without refusal; the GC
  purge moves nothing); folder deletion reclaims the whole charged population. Pinned by
  the member create→delete probe + its owner twin + the release/no-double-credit
  tests (`db::sync_storage`), all red-verified against the pre-change delta accounting,
  and the rewritten `tests/storage_quota.rs` flow (shrink returns no headroom; release
  does).
- **Retention reclamation BUILT nest-side (2026-08-17, row 165 slice 2 — rulings 1–3
  live; schema v40, green-field per § Retention (5)):** the `folders.version_retention`
  column rides `fauna.folders.update` sent-whole/applied-whole (binds-nothing rests as
  `NULL`) and both `FolderSummary` projections; the evaluator
  (`backup::version_prune::evaluate_version_retention_at`, `VERSION_HARD_FLOOR = 2`, head
  structurally excluded) schedules one 7-day cancellable `VersionBulkPrune` per folder on
  the GC cycle, cancel/succession-disarm release the `prune_pending` marks, the executor
  soft-prunes (30-day `purge_after`), `fauna.files.versions.undelete` +
  `versions.list { include_pruned }` serve the recovery window, and the GC purge phase
  stamps `superseded_at` behind a newer-row guard (over-retain, never over-delete).
  Pinned by `backup::version_prune::tests` (floor/head red-verified),
  `db::sync_storage::tests` (lifecycle, purge guard, restore-during-window pin,
  schedule/cancel), and `conformance_{files_versions,folders}.rs` (wire).
  **The tui leg LANDED 2026-08-18 (tui leads):** the § 8b editor
  paints `folder-version-retention-count`/`-days` (prefill + save through
  `fauna_folders_machine::version_retention_*`, riding the same `fauna.folders.update`),
  and media's `file-version-history` carries the recovery browse —
  `file-version-show-pruned-toggle` re-lists with `include_pruned`, soft-pruned rows
  render `file-version-pruned-badge` + `file-version-undelete-button`
  (`MediaMachine::{file_versions(include_pruned), undelete_version}`, shared). Pinned by
  `test_folder_nest_place.py` (knobs → column, both directions) +
  `test_version_prune_recovery.py` (the whole pipeline: bound → evaluate → soft-prune →
  browse → undelete, via the `version_prune/evaluate` test hook) + the tui in-process
  render/action tests (badge/undelete only on pruned rows, red-verified).
  **linux LANDED 2026-08-18** — same shape, both
  `test_folder_nest_place.py` and `test_version_prune_recovery.py` now marked linux.
  **web LANDED 2026-08-18** — the wasm face's `fileVersions`/`undeleteVersion`
  were already exported; only `setFolderNestPlace` needed widening (5→7 args) plus a new
  `versionRetentionEditFromBounds` export for the editor's seed. Both tests now marked web.
  **android LANDED 2026-08-18** — the UniFFI boundary already carried
  `DevicesMachine.setFolderNestPlace`'s `versionRetention` param and `MediaMachine.
  fileVersions`/`undeleteVersion`; pure Kotlin/Compose call-site work. No android e2e path
  on the Linux dev machine (host-emulator-gated, like every android track); verified via
  `just android-host-test`'s real JNA-wired Robolectric suite, 1095/1095 green.
  **macOS + iOS LANDED 2026-08-25** — same shape: shared FaunaKit
  `FolderNestPlaceEditor` (`FoldersContent.swift`) gained the version-retention pair,
  `DevicesMachineVM.setFolderNestPlace` widened to take `versionRetention: VersionRetentionWrite?`
  (always non-`nil` on this call); `MediaItemDetailView` gained the show-pruned toggle +
  pruned-badge/undelete on `MediaMachineVM.fileVersions(.., includePruned:)`/`undeleteVersion`.
  Build-verified only (`just apple-swift-build-check` + `just swift-test` 463/463 green);
  `test_folder_nest_place.py` already carries `pytest.mark.macos` from a prior leg on the same
  editor, `test_version_prune_recovery.py` does not yet carry macos/ios — a tier_3 run is owed.
  **windows LANDED 2026-09-09** — both
  halves. **Scope correction, verified before implementing:** windows' § 8b editor (folders
  re-model slice-e) was actually already BUILT 2026-08-21 — authored blind on
  the Linux dev machine without a windows compiler (the phase-1b precedent) and never
  compiled or run since, so both owning rows still read as open. This session
  ran the owed verification (`test_folder_nest_place.py --app windows`) and it PASSED first
  try, no defect found (unlike apple's own leg, which hit a save-without-collapse trap on the
  same editor) — `pytest.mark.windows` added. That closes the § 8b half in
  the same pass. This session's own new work is the media half: `MediaPage`'s
  `file-version-history` gained the recovery browse — `file-version-show-pruned-toggle`
  re-lists via `MediaMachine.FileVersions(.., includePruned)` (widened from a hardcoded
  `false`), a soft-pruned row renders `file-version-pruned-badge` + `file-version-undelete-button`
  → `MediaMachine.UndeleteVersion`. With both halves in place, `test_version_prune_recovery.py`
  (the whole pipeline: bound → evaluate → soft-prune → browse → undelete) was run
  `--app windows` and PASSED first try too; `pytest.mark.windows` added there as well —
  windows is the fourth app on that ledger (tui/linux/web/windows), matching macOS/iOS's own
  still-owed tier_3 run.
  Slice 3 (metering, ruling 4) has since landed — *Retention metering BUILT (2026-08-18)* above
  (`adjust_version_accounting_in_conn`); this bullet previously called it open (corrected 2026-09-19).
- **Reserved-rail history collapse LANDED (2026-08-05; `__index` joined 2026-09-20):** the
  § Retention exception is live for all four raw-opaque rails.
  `CacheDb::collapse_reserved_rail_history{,_in_conn}`
  (`bins/fauna-nest/src/db/sync_storage.rs`) marks a rail's strictly-earlier rows superseded
  as the new head is written, called from each rail's own recorder — `db/drafts.rs`,
  `db/user_config.rs`, `db/mls_replica.rs` (the latter two from inside their existing CAS
  lock, so head and collapse share one critical section), and `db/content_index_rail.rs`.
  Pinned per rail by
  `a_rewritten_rail_pins_only_its_head_against_gc` in `drafts_handlers.rs` /
  `config_handlers.rs` / `mls_replica_handlers.rs`, and scoped by
  `the_collapse_is_per_rail_not_per_set`; all verified red first (4 blobs pinned where 1
  belongs). The `__index` arm is pinned by
  `a_superseded_manifest_generation_stops_pinning_its_blob` +
  `collapsing_one_paths_history_leaves_every_other_path_live`
  (`bins/fauna-nest/tests/conformance_content_index_rail.rs`), red first the same way — it had
  repeated the axis on a third rail for every flush of an honest builder's manifests
  see [content-index.md](content-index.md) § Where the index is built for
  that rail's own bound, including the segment-path axis it leaves deliberately open.
  Ordinary folders are untouched — their history stays unbounded per the
  paragraph above.
- **Nest projection LANDED (2026-07-09, slice 1;
  tracked internally):** `fauna.files.versions.{list,get}`
  (`bins/fauna-nest/src/files_handlers.rs`) project over `sync_changes`
  (`CacheDb::{list_file_versions_in_sets, get_file_version_by_seq}`), scoped by
  `enumerate_readable_folders` filtered to `is_label_audience()` grants (since
  2026-07-30 — the Q5 admin arm is dropped; pinned by the
  `versions_are_withheld_from_a_q5_admin_who_is_not_the_audience` /
  `versions_ship_to_the_owner_and_to_a_roster_member` twins), `version_num = seq`, with the additive
  `folder`/`content_key_version` wire fields; the dead `file_versions` table is
  dropped (empty in production — no writer ever existed). Tier_3-proven through the
  **production** flow (`bins/fauna-nest/tests/conformance_files_versions.rs`): record
  over the real wire → listed; restore-record → head AND a new version, generation
  echoed; non-member → empty/`not_found`.
- **Shared-Rust client half LANDED (2026-07-09, slice 2):** typed calls
  `SyncClient::{versions_list, versions_get}` (`libs/fauna-client-sync`);
  `MediaNestApi::{file_versions, restore_member}` +
  `MediaMachine::{file_versions, restore_version}` (`libs/fauna-media-machine` —
  path→`path_hash` BLAKE3 derivation and the restore re-point both in shared Rust,
  self-healing device registration included, `FileVersionSummary` FFI record), exposed
  via UniFFI (`fauna-ffi` re-export) + wasm (`fauna-wasm-media`
  `fileVersions`/`restoreVersion`).
- **App UI: landed on all 7 apps** (`media-item-detail` / `file-version-history`
  surface, media.md § Element IDs, approved 2026-07-09; slices 3–4; android
  landed 2026-07-19, closing the last gap). Per-app status is owned
  by `../ui/media.md` § Implementation status today — don't restate the
  per-app landing dates here.
  The Windows Explorer shell-extension verb consumes these same semantics
  (tracked internally) — it must not invent its own.
- **Recording-device local apply (§ Restore): LANDED for the Windows shell surface (2026-07-10,
  TRACK V slice D)**, its first and so far only consumer. The shared cross-platform agent's
  (`bins/fauna-sync-agent`, the crate the windows `fauna-sync-agent.exe` binary now ships from —
  `../architecture/apps/sync-agent.md` § Implementation status, A1b/A5) `RestoreFileVersion` pipe
  verb records the restore, then re-points its own row through the
  all-columns `SyncDb::upsert_entry` (there is still no narrow setter; the `ON CONFLICT` clause
  leaves `pinned` intact), `supersede_placeholder`s the stale bytes (the placeholder left
  describing the restored version — the `dehydrate_placeholder` it called until 2026-10-08 kept the
  current size) and emits
  `FileStatusChanged{CloudOnly}`. Media still needs none of this on any of the 7 apps — it is
  a nest-side library fetched by hash, so `MediaMachine::restore_version` correctly stops at
  `refresh()`.
- **Placeholder re-point on a moved head: LANDED (2026-07-10).**
  `SyncEngine::record_placeholders_from_changes` used to skip *every* already-tracked path, so on
  the Windows on-demand host (which folds `changes.list` once at `prepare()` and never calls
  `pull_remote_changes`) a cloud-only placeholder stayed anchored to a superseded manifest forever
  — an ordinary remote modify served stale bytes on open, and the § Restore crash window never
  healed. The fold now owns `Placeholder` rows and only those, mirroring the rule its delete arm
  already followed: a bytes-free placeholder is re-pointed when the folded head's
  `(manifest_hash, size_bytes, content_key_version)` differs; an unchanged head writes nothing.
  **The re-point reaches the disk too (2026-10-08):** the fold names its re-pointed rows
  (`PlaceholderFold::repointed`, with the new version's size and mtime), and the cfapi host
  re-describes each placeholder already on the disk (`bridge::redescribe_repointed` →
  `fauna_cfapi::supersede_placeholder`) — until then a cloud-only file whose size a remote edit
  changed kept its old size on disk, and its next open was served truncated or short.
- **Stale hydrated copies invalidated at service start: LANDED (2026-07-10).** A `Synced` row is
  still never *rewritten* by the fold — its bytes are on disk and may carry an unsynced local edit.
  But the fold no longer stays silent about one whose head moved: it now **reports** each such row
  (`SyncEngine::record_placeholders_from_changes` returns a `PlaceholderFold` carrying the
  `StaleHydratedRow`s alongside the recorded count), and the Windows on-demand host acts on the
  report at `prepare()` — freeing the file first, the placeholder left describing the new version
  (`fauna_cfapi::supersede_placeholder`: `CfUpdatePlaceholder` with the new size and mtime,
  `DEHYDRATE`, and `VERIFY_IN_SYNC`, so cfapi's `CF_INSYNC_POLICY_TRACK_ALL` root
  refuses a locally-edited file and the edit is never clobbered; a bare `CfDehydratePlaceholder`,
  used until 2026-10-08, kept the old size, and a grown file's next open was served a prefix that
  the engine then uploaded over the real edit), then re-pointing the row at the
  head via `SyncEngine::repoint_hydrated_to_placeholder`, then emitting `FileStatusChanged{CloudOnly}`
  (§ Restore ordering 3 — dehydrate-before-row, the inverse of a restore's ordering). This heals both
  a remote modify to a hydrated file *and* a restore interrupted on a hydrated file.
  ⚠ **Until 2026-07-17 this same comparison also misfired on the device's own successful uploads**
  (not just genuine remote modifies): the upload path left `manifest_hash` at the pre-upload merge
  base even after a change record succeeded, so the very next fold read the device's own echoed
  record as a stale hydrated copy and queued the user's freshly-synced edit for dehydration (only
  the dirty-file refusal protected the bytes — live 2026-07-17, `stale_hydrated=2`). Fixed by
  `SyncEngine::commit_recorded_head` (`on-demand-files.md` § On-Demand Files → *A recorded upload flips the file to the
  platform's in-sync state*), which stamps the row with the just-recorded head so the fold sees its
  own echo as current.
- **No restart needed — the on-demand host re-pulls on a timer: LANDED (2026-07-11).** The
  invalidation above used to fire only at service *start* (the host folded `changes.list` once, at
  `prepare()`), so a remote change arriving while the service was up went unseen until the next start.
  `run_hydration_loop` now also re-pulls in steady state: a `rescan_tick` re-calls
  `populate_placeholders_from_nest` and feeds the resulting `PlaceholderFold` back through the *same*
  `apply_fold` → `apply_stale_hydrated` path the startup fold uses — so a change picked up on a timer
  lands in exactly the state a restart would have produced (re-pointed `Placeholder`; dehydrate-gated
  invalidation for a `Synced` copy, § Restore ordering 3, which still refuses on a dirty file). The
  cadence is the shared reconcile constant `fauna_client_folders::cadence::DEFAULT_RESCAN_INTERVAL`
  — the same one Linux's `rescan_tick` uses (`sync-engine-deployments.md` § Control Plane Principle,
  the phase-5 block). A **reconnect** (`NestClient::subscribe_reconnects`) re-pulls immediately
  on top of the tick, so changes recorded while the socket was down don't wait out a full interval.
  Every failure is best-effort like `prepare` — an unreachable nest just means that pass saw nothing,
  and the next tick retries, which is also how a host that started against a *down* nest eventually
  populates at all. **The timer (± reconnect) re-pull was the host's only inbound-change signal until
  the push nudge landed (windows leg BUILT 2026-08-02).** Since the nest pushes folder changes on
  the WS-RPC plane — `PushEvent::SyncChanged` (`fauna.sync.changed`), fired from `record_change_core` to
  same-nest participants (`file-sync.md` § *Remote-change nudge*) — and linux consumes it directly off its in-process
  engine. **Windows' bearer-only app WS now consumes it too**: `NestRpcClient`'s generic push pump
  (`FaunaApp.Core/Services/NestRpcClient.cs::RunPushPumpAsync`) routes a `SyncChanged` event to the
  resident agent's engine over the shared `PullFolderNow` verb
  (`fauna-ipc::sync::RequestMethod::PullFolderNow`), reached through the shared FFI agent surface
  (`IAgentSyncNudge.PullFolderNowAsync`) since the named-pipe retirement — fire-and-forget, the
  same best-effort contract as the timer tick, just off-cadence. The
  timer + reconnect stays the correctness backstop for a missed/dropped push; no e2e proof yet (the
  windows app driver is session-singleton, so a two-real-agent same-nest proof — the linux
  `test_folder_agent_content_sync.py` shape — needs its own harness leg, not just this wiring). (The raw `/api/v1/sync/ws` push path was
  daemon-only and is removed with the daemon; the nudge
  rides the authenticated WS-RPC socket the host already holds, not a second socket.)
- **Nest-backed listing + submenu + restore on the Windows shell surface: LANDED (2026-07-10,
  TRACK V slices B, C, D).** `ListFileVersions` answers from `fauna.files.versions.list` via the
  shared `SyncClient`, keyed by `fauna_core::sync::path_hash` over the folder-relative path and
  scoped by `folder`; `FaunaInfoVersions` renders it as an `ECF_HASSUBCOMMANDS` nested submenu
  of per-version children (returned objects, never CLSID-activated); invoking an older child
  calls `RestoreFileVersion`.
- **Restore exercised end-to-end against a real nest: LANDED (2026-07-15, TRACK V slice F).** The
  service-side version-history + restore **control-plane mechanism** now runs headlessly against a
  real `fauna-nest` (tier_3, `bins/fauna-sync-agent/src/versions_tier3.rs`, opt-in
  `--features tier3-nest`; an in-process `RpcRequester` dispatches into the nest's own
  `AppState`/`RpcRouter` handlers, the same arrangement `conformance_files_versions.rs` uses).
  Record two versions over the real `fauna.sync.changes.record` handler → the service's own
  `list_file_versions` returns both → `restore_file_version` records the restore carrying the
  historical `manifest_hash`/`size_bytes`/`content_key_version` **verbatim** and a fresh
  `versions.list`/`versions.get` reads the restored head back → the local
  `pipe_server::repoint_entry` re-points **this** device's own `SyncDb` row at the restored version
  (`Placeholder` at the new head). This closes the "never once run end-to-end" gap Track V-D
  carried — the earlier coverage was an in-memory `FakeRestoreNest` that could not prove the
  `SyncClient` wire shapes against the real handlers. **Still manual:** only the live-Explorer submenu
  **render** (pixels only) — the cfapi **byte-plane** half is now exercised too (next bullet).
- **Cfapi byte-plane restore round-trip exercised: LANDED (2026-07-15, TRACK V slice G).** Slice 1
  (V-F, above) proved the restore *control* plane; this closes the *byte* plane. Two versions of a
  file are uploaded through the **real** `/api/v1/chunks` + `/api/v1/manifests` routes into a real
  `DiskBlobStore` — a real in-process `fauna-nest` byte plane (`fauna_nest::build_router` bound on an
  ephemeral `127.0.0.1:0` port over a real `BackupService`, the `conformance_content_key_chunk_route.rs`
  idiom) — the `SyncDb` row is re-pointed at the **older** version's manifest (what
  `pipe_server::repoint_entry` writes on an on-demand restore), and a **real cfapi `FETCH_DATA`** (a
  placeholder opened from another process) re-hydrates the **older version's bytes**: `serve_fetch` →
  `SyncEngine::download_file_bytes` resolves the re-pointed manifest from the row → fetches that
  version's chunks from the real store → the OS receives them. This proves the manifest→chunks
  resolution the cfapi live harness's `DbBackedHost.blobs` path→bytes fake structurally could not
  (`bins/fauna-sync-agent/src/restore_byteplane_tier3.rs`, opt-in `--features
  tier3-nest`; the cross-platform engine-level guard over a stateful wiremock store is
  `fauna-sync-engine`'s `download_file_bytes_test::download_file_bytes_serves_historical_version_after_repoint`).
  So § Restore's *"the next open re-hydrates the historical bytes, because the hydration FETCH_DATA
  resolves its manifest from that same local row"* is now headlessly proven end-to-end.

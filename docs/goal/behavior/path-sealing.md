# Sealed names & paths — target state

Owns: path-sealing
Status: ratified 2026-07-29 (mechanism); S1–S9 landed — the S9 flip (write-plaintext scrub) EXECUTED 2026-08-02 (v32) and per-app enablement is complete on all 7 apps (web's D1/D3 backfill legs, the last gap, landed 2026-08-11 on the now-transport-generic resolver); two aux fields DISPOSITIONED BLOCKED-DARK (2026-07-31, no production writer); see the status notes at this doc's end
Authority: the sealed names & paths MECHANISM — the `SealedLabel` envelope + additive sealed sibling columns, the `fauna.path.v1` two-step key derivation + the two nonce modes, hash companions, and the expand→migrate→contract staging; the paths-are-content ruling + tightening set + exemptions stay owned by `../architecture/encryption-at-rest.md` § Carve-outs, the key entry by `../architecture/mls-group-key-material.md` § M2 → Sealed names & paths, the MDA session unseal by [`webdav-server.md`](webdav-server.md) § Key model, and the sync protocol + `path_hash` derivation by [`file-sync.md`](file-sync.md)

Split verbatim out of `file-sync.md` § File Versions → Sealed names & paths on 2026-08-02 (a
stub there redirects; prior history: `git log --follow docs/goal/behavior/file-sync.md`).


Implements the *paths-are-content* ruling (`../architecture/encryption-at-rest.md` § Carve-outs →
*Seal file-sync name/path metadata* — the ruling, the tightening set, and the two exemptions live
there; this section owns the **mechanism**). Design record + slice plan tracked internally
(2026-07-29).

**The carrier.** User-chosen labels rest in additive sealed sibling columns (`path_sealed`,
`name_sealed`, `label_sealed`, `details_sealed`, `filename_sealed`, `source_sealed`,
`tags_sealed`, `include_paths_sealed`/`exclude_paths_sealed`), each a self-describing
`SealedLabel` envelope (canonical dag-cbor): `{ v, gen: Option<u64>, nonce: Option<[u8;12]>, ct }`.
`gen` names the M2 content-key generation the label sealed under (`None` = the owner root), so a
server-side row copy (snapshot creation copying the membership projection) moves sealed labels
verbatim with no key and no per-table generation column.

**The key (owner: `../architecture/mls-group-key-material.md` § M2 → *Sealed names & paths*).**
The root that already seals the set's chunks — a bound/served set's M2 content-key generation
(opened `keys_for(version)` all-candidates) or the owner's `BackupKey::convergent_chunk_root()` —
through a two-step BLAKE3 derivation, context `"fauna.path.v1"`, AAD-bound to the salt + a field
domain tag. Anyone who can open the set's bytes can render its names; nobody weaker can. The MDA
renders served sets per AUTH'd session via the `WebdavKeysBlob` keys it already holds (the mail
pattern; `webdav-server.md` § Key model). **Two nonce modes:** convergent (nonce derived from the
salt) *only* where the salt determines the plaintext — `path` (salt = `path_hash`), set `name`
(salt = `name_hash`), `source_descriptor` (salt = `source_hash`), per-tag seals; explicit random
nonce for every mutable-under-salt field (device label, include/exclude, conflict `details`, the
tag-list display copy) — a derived nonce there would reuse a (key, nonce) pair across differing
plaintexts.

**Hash companions join the floor** (equality-only routing/uniqueness keys, the `path_hash`
class), each a `blake3::derive_key` under its own context so the hash spaces stay disjoint, each
KAT-pinned in `fauna_core::path_crypto` exactly as `sync::path_hash` is — changing one orphans
every stored digest, so it is a data migration, not a refactor:
`name_hash = derive_key("fauna.set-name.v1", name)` (set addressing +
`UNIQUE(name_hash, actor_id)`), `source_hash = derive_key("fauna.import-source.v1",
source_descriptor)` (the import mutual-exclusion lock), and per-tag
`derive_key("fauna.snapshot-tag.v1", tag)` collected into `tag_hashes` (server-side retention
`keep_tags` matching — the policy's tags hash the same way, so the compare is hash-to-hash on
both sides). All three are nest-computable from the plaintext that still rests during expand, so
the nest backfills them for legacy rows; the *sealed* halves are client-keyed and never can be.
The companions are deliberately **unkeyed**, and that bound is declared, not incidental: a
wordlist recovers a guessable string from its resting digest — the residual, the rejected
keyed/actor-scoped alternatives, and the S9-pinned uniform re-key question are owned by
`../architecture/encryption-at-rest.md` § Carve-outs (settled 2026-07-30). Server-side logic
stays hash-keyed everywhere: the latest-per-path fold, the snapshot diff join, the WebDAV ETag
head lookup, and conflict-winner propagation all re-key from the plaintext column to `path_hash`
(1:1, already stored) with no wire change. Reserved `__` set names are routing constants and
never seal — every `is_reserved_folder_name` decision site (incl. the GC direct-blob
classifier) is untouched by construction.

**Migration is expand→migrate→contract** (`../architecture/version-compatibility.md` § 2.1):
expand = additive columns/fields, nest-backfilled *hash* companions, clients dual-write
plaintext + sealed through the one engine funnel (`SyncEngine::record_change`), sealed-first
read enablement on all 7 apps + the MDA; migrate = a client-driven seal-names backfill pass
(the `reseal_owner_only_plaintext` precedent — no server-side backfill is possible for
client-keyed seals), with sealed-only rows meeting an old or keyless reader following the
ratified degrade contract (**omit from the listing, re-enter on re-record** — the
`backup_custody` path-less precedent); contract = writers stop emitting plaintext and the nest
scrubs plaintext where a sealed sibling exists — **a compatibility break (a pre-expand engine
applies a change by writing at the plaintext path), so the flip is major-gated by default; an
earlier alpha flip needs explicit per-case user approval, never assumed. That approval was GRANTED
2026-08-01 — clear, don't migrate; the approval, its deletion inventory, the execution
prerequisites, and the execution record are owned by `../architecture/encryption-at-rest.md`
§ Carve-outs. **The contract step EXECUTED 2026-08-02 (v32): a one-way boot reconcile
(`reconcile_path_sealing_flip`) — inventory → scrub → `VACUUM` → marker → immediate
hot-copy — with the scrub UPDATEs re-run idempotently on every later boot, and the write
path flipped (a sealless record on a sealed plane is refused `path_seal_required`; web-mode
and reserved rails keep their ratified plaintext).** The reconcile, its marker and its inventory
retired with the history the 2026-09-24 genesis of the nest schema replaced; the every-boot scrub
stays, as `run_scrub_plaintext` in `run_migrations` (`../architecture/nest/common.md` § Database), and exempts the `sync_changes.path` and `sync_conflicts.path` planes of a folder whose paths rest plaintext by ratified design — a `public` audience or the legacy `web` mode, the class `FolderRow::rests_plaintext_paths` names and both write rails already exempt — even where an over-sealing writer rested a seal beside the plaintext: a public follower's projection withholds that seal, so scrubbing would hand the follower a row with neither label, refused as `NoSeal`. The exemption reads the folder's current class, so a folder flipped back to private scrubs on its next boot. The
`fauna.media.list` plaintext-ordered v1 cursor order and the `snapshot_files`
`(snapshot_id, path)` PK retired with it (a v1 request paged deterministically — hex-hash order — for shipped older
clients until the 2026-09-24 compat-remnant sweep removed the v1 order from the wire; the PK is `(snapshot_id, path_hash)` and the
plaintext column is gone).

**Deliberate non-seals:** Website-folder paths **once published** (public URLs — their conformance
row; the exemption's ground is publication, so a web-mode path *refused before it enters
`web_files`* — e.g. a rejected server-side extension — is NOT covered and redacts like any
other path);
coordinator-synthetic destination custody names; **both machine-authored device labels** —
the nest-authored `"WebDAV"` pseudo-device and the `"fauna"` self-heal placeholder a
never-registered device writes before its real registration supersedes it (frozen constants,
identical on every deployment and every account, so sealing them protects nothing and costs every
reader a key; refused centrally in `fauna_core::label_custody::is_synthetic_device_label`, never
at the five call sites — one of which is nest-side and reaches no client helper at all);
IMAP mailbox-name import cursors (mailbox structure is floor). The share filename seals
at rest for the author's list only — the recipient-serving path keeps reading the URL-presented
signed token transiently (`Content-Disposition`/MIME at serve time; the read-position purpose
test's serve-time transient read), the fragment-key variant having been set aside in the design
record — and **reopened and ratified 2026-09-25 by the user** for links to private files that
keep the nest blind: the key rides the URL fragment, the nest serves ciphertext plus a sealed
key envelope, and the filename travels inside that envelope rather than the token (owner:
`share-links.md` § The private-file extension; this paragraph defers to it). Exact sizes on file-sync surfaces are **re-ratified exact** (not rounded) — the
manifest's plaintext `total_size`/`chunk_sizes` and per-chunk ciphertext blobs already expose
them (accepted-forever, `mls-group-key-material.md` § M2 *Sealed manifest hashes*), and DAV
`getcontentlength`/quota/restore need exactness.

**Implementation status today (Sealed names & paths).** The **carrier, the expand-phase schema,
and the write half are BUILT** (2026-07-29): `fauna_core::path_crypto` holds the `SealedLabel`
envelope, both nonce modes, the all-candidates `open`, and the three KAT-pinned hash companions;
nest schema v29 adds every sealed sibling column and hash companion listed above as additive
nullables, with `reconcile_path_sealing_companions` backfilling the nest-computable hashes and
creating the `name_hash` / `source_hash` / `snapshot_files.path_hash` indexes. **Clients now seal
and the nest now carries it:** the wire gained additive `path_sealed` siblings on the record /
changes-list / files / media / snapshot-browse / snapshot-diff / conflict planes, the `/sync/ws`
`FileChanged` frame (since removed with that data plane), and the cross-nest federated record relay (`path_hash` rides the snapshot and
conflict planes with them); `SyncEngine::record_change` seals **every** recorded path through one
funnel (`seal_recorded_path` — M2 content-key generation for a bound set, the owner's
`convergent_chunk_root()` otherwise, convergent nonce salted by `path_hash`, and FS-BIND-5
fail-closed inherited from `content_seal_root`), joined by the `fauna-sync` daemon's data-plane
notify and the Media upload gesture, which seals where it already holds the `BackupKey`. A tier_3
conformance test drives a real client seal through `fauna.sync.changes.record` into
`sync_changes.path_sealed` and back out of `fauna.sync.changes.list` byte-for-byte, opening under
the client root and failing closed under any other.

⚠ **Gap CLOSED (was open 2026-08-02, closed 2026-08-03) — the Media *gesture* seam is now wired on
all 7 apps; a missing one would be a broken gesture, not a degraded one.** The write-side rule
above (*Third silent surface*) depends on each app injecting the owner key via
`MediaMachine::set_owner_backup_key`, because `delete` / `restore_version` take **no per-call key**
— unlike `upload`, which seals from the key handed to it (`machine.rs`, `do_upload`), which is why
an app can look healthy on uploads while every delete and restore fails. Without the injection
`seal_gesture_path` resolves no seal root, the record carries `path_sealed: None`, and since the S9
flip the nest **refuses** it outright (`fauna.sync.path_seal_required`). Injecting: **tui**
(`apps/fauna-tui/src/media/mod.rs`), **linux** (`apps/fauna-linux/src/views/media/mod.rs`),
**android** (`ui/viewmodel/MediaVM.kt::ensureMachine`, landed 2026-08-02), **web**
(the `setOwnerBackupKey` wasm export + injection inside `$lib/wasm-media::createMediaMachine`,
landed 2026-08-02), **apple** (`MediaMachineVM.configure()`, landed 2026-08-02 — detailed below),
and **windows** (`MediaPage.xaml.cs::Page_Loaded`, landed 2026-08-03 — detailed below), closing
the cross-app injection track. The client half is pinned by
`an_unkeyed_machines_restore_seals_nothing_so_the_post_flip_nest_refuses_it`
(`libs/fauna-media-machine/tests/media_lifecycle.rs`), the nest half by
`conformance_path_sealing.rs::a_sealless_record_is_refused_loudly`.

**The shared read half is BUILT too (2026-07-29).** `fauna_core::path_crypto::render_sealed_label`
is the one seam every read surface shares: seal first, legacy plaintext as fallback, and
`SealedLabelRender::Omit` when the reader can open neither — the ratified degrade, expressed as a
type so no surface can render an empty name or fail a page instead. Its root selection is
`FileDownloadKeys::label_open_roots`, deliberately the reader's ordinary **byte-download** custody
rather than a second resolver (whoever opens a set's chunks opens its names), keyed off the
*envelope's* `gen` and — unlike the chunk path — not suppressing the owner key for a bound set, so
a set's owner still renders the names it sealed before the set was bound. **Wired end to end on
the media surface:** `fauna.media.list` items gained `path_hash` beside `path_sealed` (the
convergent salt — without it a scrubbed row has nothing to open under, which is why it rides this
plane and not only the snapshot/conflict planes), and `MediaMachine::refresh` takes the owner key
per gesture and renders at ingest, resolving custody once per set. Tests blank the plaintext and
still render the right name and basename, show a keyless and a wrong-key reader omitting the row
while the page stays healthy, and render a bound set's label under its content-key custody.

**⚠ `Omit`'s degrade is PLANE-SPECIFIC — read this before generalising either half.** On the
**path/media** and **conflict** planes it **DROPS the row** (the rest of this paragraph); on the
**device** plane it deliberately **keeps a nameless row** — `DevicesMachine::render_devices` maps
`SealedLabelRender::Omit => String::new()` and retains the row
(`libs/fauna-devices-machine/src/machine.rs`), because a device is actionable by `device_id` alone
(revoke it, see it online, read its folder roles) and hiding one the user may need to **revoke**
is the outcome worse than an unnamed one. Same reasoning as conflict `details`, which degrades to
`None` rather than dropping the conflict. **The triage consequence inverts with the plane:** on the
path/media plane a seal failure takes *count-based* assertions to zero, so a `0` is a custody
question; on the device plane counts stay right and only *name* assertions break. Measured both
ways (2026-08-02: `device-card` count correct at 1 and 2 with `device-name` text `''`). Do not
carry a conclusion from one plane to the other.

**The device plane's WRITE half mirrors that choice: `fauna.sync.register` ACCEPTS a sealless
register, where the record path refuses (`path_seal_required`).** Registration is deliberately
best-effort — a device that cannot seal its name must still appear in the device list, where it
can be revoked (the same revocability reasoning as the read half) — so a non-synthetic label with
no `label_sealed` rests **nameless** (`label = ''`, no seal) rather than refusing or resting
plaintext; and because the pair moves together on the upsert (deliberately no `COALESCE` — a
stale seal would present the *previous* name with nothing failing), a keyless **re**-register also
drops an existing seal. The row stays nameless until the device's next **keyed** register
re-stamps it. Sealless registers from current clients are only the seal-failure degrade and the
bearer-only-connection allowance (a bearer-only host holds no keypair to seal with); a device
whose only writer is bearer-only rests nameless until the user re-labels it from a keyed app.
**The older-client permanence question is RULED (user directive, 2026-08-02): a pre-expand client
that never sends a keyed register rests a nameless device row permanently, and that is ACCEPTED —
no re-stamp path exists or is owed, because no pre-expand alpha clients are assumed to remain in
the field.** This is a one-time older-client write-off in the spirit of the alpha carve-out
(development speed over a compat shim for clients nobody runs); it retires the S6-b "rests
plaintext-only for S8 to re-stamp" justification, which the flip already voided (post-flip no
plaintext rests to re-stamp from). A permanently nameless row is not data loss: the device stays
actionable by `device_id`, and the user re-labels it from their app — that re-label *is* the next
keyed register. Compat status recorded in `../architecture/version-compatibility.md` § 2.2 status.

**On the path/media plane, then: `Omit` DROPS the row — it does not render it nameless, and after
the flip that is the only outcome an unopenable seal has.** `render_sealed_label` falls back to `Plaintext` only while the
plaintext is *non-empty*; once the flip scrubbed that column the fallback is gone, so
`MediaMachine::refresh` skips the item entirely (`libs/fauna-media-machine/src/machine.rs`) and it
never reaches a snapshot. **The consequence that bites is on the seeding side, and it is silent:** a
fixture that records a *synthetic* `path_sealed` — a blob shaped like an envelope but sealed under
no root the reader holds — seeds rows **no app can ever see**. Every assertion over them fails,
*including count-based ones*, for a reason that looks nothing like its cause; nothing logs an error,
because omitting is the correct behavior. That is not hypothetical: it took out the entire
`seeded_media_app` cluster (13 tests across 5 files) the day the flip landed, and the first triage
predicted "rows render nameless, counts survive" — the opposite of what happens. **So the rule for
any seeding seam is: seal through the real funnel or seed nothing renderable.** The e2e seams do
this through `fauna_path_seal` (`libs/fauna-ffi/src/cabi.rs` → `fauna_ffi.seal_path`), which hands
Python `label_custody::seal_path` itself rather than a look-alike — a fourth writer of this plane
would drift, and a drifted seal fails as `Omit`, never as an error. Read-side twin, same silence:
a test that asserts on the plaintext `path` of a `fauna.sync.changes.list` row is reading a column
the flip emptied; it must key on the convergent `path_sealed` (byte-identical for a given
(root, path), so sealing the expected path is a total match) or on `path_hash`.

**Third silent surface, on the WRITE side: a gesture's own post-write refresh must re-render under
the custody it just sealed with.** A delete/restore seals its record with the injected owner key
([`MediaMachine::set_owner_backup_key`]) and then refreshes the page; if that refresh runs *keyless*
the reader degrades to `Omit` against labels it demonstrably holds the key for — and because the
set-*name* axis omits per set, not per row, **deleting one of two files empties the entire set from
the library**. The user sees their whole photo set disappear on a single delete; nothing errors,
because omitting is the correct behavior for a reader that cannot open the label. This shipped as
`self.refresh(None)` in `MediaMachine::delete`/`restore_version` and was caught by
`test_media_delete_removes_the_item` reading **0** items where 1 was owed (its sibling
`test_file_version_history_and_restore` read an empty `media-item-size` — the restored row was not
missing a size, it was missing from the snapshot). **The rule: every gesture that both seals and
refreshes routes its refresh through the same key its seal used** — in the machine,
`refresh_with_owner(self.owner_key())`, never `refresh(None)`. Pinned by
`a_keyed_machines_{delete,restore}_refresh_still_renders_*` in
`libs/fauna-media-machine/tests/media_lifecycle.rs`.

**Generalised — THE CONSUMER-WIRING RULE: every reader of a sealed plane must be handed custody at
its construction seam, and "this surface doesn't need names" is the trap.** Two instances found the
same day, both silent, both in code that *held the key already*:

1. `MediaMachine::delete`/`restore_version` re-rendered keyless (above).
2. **`fauna-sync restore` restored NOTHING and exited `0`.** `cmd_restore` built
   `SnapshotsClient::new(nest)` with no `with_label_custody`, while deriving the very same owner
   root ~60 lines later for the chunks. `SnapshotsClient::get` renders each file row sealed-first
   and **drops** what it cannot render (`render_paths`' `Omit => false`); post-flip `snapshot_files`
   carries no plaintext path, so a keyless client omitted **every** row — `snap.files` came back
   empty, the restore loop was a no-op, and the command reported success. `with_label_custody`'s own
   doc listed "restore history" among the sites that never need custody; **restore is not one of
   them.** Fixed by wiring `LabelCustody::owner_only(config.backup_key())` at construction.

**The review question this owes every future reader:** for each surface that reads a sealed plane,
*where is its custody wired, and what happens if it isn't?* If the answer to the second half is
"rows silently vanish", the wiring is load-bearing and needs a pin. A consumer that derives a seal
root for one purpose (chunks) and not another (labels) is the specific shape to look for — the key
is present, so nothing looks unauthorised; only the output is empty. Caught by the disk assertion of
the daemon's restore e2e (`test_backup_restore.py`, deleted 2026-10-01 ahead of the daemon itself —
`backup-restore.md` § 4); the shared walk's end-to-end pin is the tier-3 one described below.

**The rule's own sweep — run 2026-08-03, and it found three more.** Writing
the rule above did not fix the code it describes: a sweep of every `SnapshotsClient::new` filtered
to file-row readers turned up the native **full restore** (`fauna-ffi/src/sync_engine_host.rs` — the
macOS `MacRestoreView` consumer, which reported `files_restored: 0` with `Ok` while its own doc
comment claimed sealed snapshots restore correctly), the native **per-file download**
(`fauna-ffi/src/snapshot_download.rs` — took `owner_secret`, spent it on the chunk walk, read the
rows keyless), and **web's `snapshotGet`** (a sealed snapshot rendered as an empty file table). All
three now build through one seam per platform: `fauna-ffi`'s `owner_read_snapshots_client` and, as
of 2026-08-13, the wasm **`BackupsMachine`** constructor (the page machine `snapshotGet`'s bindings
were folded into, 2026-08-05) — both now build the same resolver-backed `LabelCustody` through
`seal_backfill::resolver_backed_custody`, the identical constructor web's own seal-backfill sweep
(below) already used, so a bound-but-unresolvable set fails **closed** on web exactly as it does on
native. (The gap's stated cause — `NestFolderKeyResolver` not yet being transport-generic — expired
2026-08-11; the wasm side had simply not adopted the seam yet, closed here. No new pin was needed:
`seal_backfill::resolver_backed_custody`'s own shape — both the resolver and owner-key arms present
— is already pinned natively by `the_sweeps_one_custody_constructor_carries_both_arms`, and every
caller through that one constructor inherits it by construction.)

**A sixth instance found 2026-08-10 (live e2e sweep) — the
sink above doesn't cover it, because this is a DIFFERENT plane.** `DevicesMachine` (`libs/fauna-
devices-machine`) has carried `set_label_custody` since before the S9 flip, but **no caller on 6 of
the 7 apps ever called it** — only tui wires it (`apps/fauna-tui/src/settings/devices.rs`). This was
completely latent until 2026-08-02 (the S9 flip made a real daemon's `fauna.sync.register` seal its
label by default), at which point every device on windows/linux/web/android/apple started opening
`Omit` — a blank name, per `DevicesMachine::render_devices`'s deliberate "keep the row, not the
restore plane's drop" degrade. **On windows specifically, an empty `AutomationProperties.Name`
additionally prunes the WHOLE `device-card` row from the UIA tree** (the exact bare-panel trap the
XAML's own comment already warns about, now retriggered by its own fix's blind spot), so `--app
windows` AND `--app web` both showed **zero** device-cards for two nest-confirmed devices —
`test_device_card_shows_after_register` and `test_family_device_marker_badge_and_ward_delete_
refusal` red on both, cascade-ruled-out, 2/2 reproducible. **Fixed on windows, 2026-08-10, via a
now-retired dedicated call** (`#[uniffi::export] wire_devices_label_custody(devices, owner_secret)`,
`LabelCustody::owner_only`, called from `NestRpcClient.BuildDevicesMachineAsync` right after
construction) — the SAME session's later `build_devices_machine` auto-wire commit (below) made it
redundant without removing it, and because `set_label_custody` is a plain `Mutex` replace, the
redundant call silently downgraded the conflict list's render custody from full (resolver + owner)
back to owner-only for every windows session, breaking a shared/bound file set's conflict rows.
Found and fixed 2026-08-13: the call and the now-dead-code
`wire_devices_label_custody` free fn were both removed; windows joins linux/android/apple in relying
solely on the shared auto-wire. **linux/android/apple needed no separate call in the end:**
`label_custody` is the single `DevicesMachine` field both `render_devices` and the conflict render
below read, so the S3 conflict-custody wiring (below) closed this same device-label gap for
android/apple as a side effect of its own fix — `build_devices_machine`'s internal auto-wire
(2026-08-10) — and for linux the same way (`FaunaClient::label_custody()`, 2026-08-11). **Web
closed the sixth instance 2026-08-13**, via a new wasm export shaped exactly like `snapshotGet`'s
two instances above: `DevicesMachine::setLabelCustody(secretHex)` (`libs/fauna-wasm-folders`)
builds and wires **both** arms — the resolver and the owner key, never owner-only, because the
same page's conflict list reads the identical `label_custody` field for a shared/bound set's
paths — called from `DevicesSection.svelte`/`FoldersSection.svelte` right before their first
`refresh()`, mirroring the existing `setMlsQuery`/`setForeignSetsSource` calls.

**And the class now has a SINK, so it cannot regrow silently:**
`SnapshotsClient::get_for_restore` cross-checks the rendered row count against the reply's own
`file_count` and refuses rather than restoring a subset. `file_count` is a trustworthy witness
because the nest stamps it as `files.len()` at capture and serves the rows unpaginated, so any
shortfall was introduced client-side by the omission. Every restore entry point routes through it
(`fauna-sync cmd_restore`, the FFI full restore, the FFI per-file download) — **a future seam that
forgets its custody gets a loud error naming the missing wiring, instead of an empty directory and
exit 0.** This is the shape to prefer whenever a degrade is correct for one audience and data loss
for another: keep the degrade on the shared read, and put the strictness on the call site that knows
it is about to write to disk. Pinned by `get_for_restore_refuses_a_restore_that_would_silently_write_nothing`
(+ two positive controls, so the check cannot be "fixed" by deletion) and by the call-site custody
pins in `fauna-ffi`'s `snapshots_client.rs`, which exist because a custody-shape pin in `fauna-core`
**cannot** observe a consumer downgrading its own wiring.

**The native full restore now has its own end-to-end pin, because seam pins provably could not catch
this bug** (2026-08-05): all three of the constructor pin, the sink pin and the walk-and-write pin were
green *while the bug shipped* — it lived in the composition, and nothing drove the listing read, the
chunk walk and the filesystem write as one call.
`bins/fauna-nest/tests/conformance_ffi_snapshot_restore_client.rs` does, against a real in-process
nest: the owner captures through the production `SyncEngine::upload_file`, and the restore runs
through the shipped macOS construction (`FfiNestClient::new` → `connect` → `start_account_runtime` →
`sync_engine_host` → `restore_snapshot_to_dir`), asserting the bytes on disk. Its
negative arm is the A/B on the exact line the fix changed — a keyless client renders **zero** rows
while the reply still says `file_count: 2`, and the same read through `get_for_restore` refuses
loudly. Mutation-graded three ways: reverting the call site to the pre-fix keyless `get` reproduces
`files_restored: 0` with `Ok` verbatim, keyless-but-sinked turns it into the loud refusal, and
neutering the sink's own comparison fails the negative arm. **It needs no Mac** —
`FfiSyncEngineHost` is plain async Rust, so the mechanism is a merge-gate-able barrier on every dev
machine; what is genuinely mac-only above it is `MacRestoreView`'s three lines of SwiftUI glue and an
`NSOpenPanel` folder choice. `fauna-sync cmd_restore` has had no end-to-end pin of its own since
2026-10-01 (its e2e left ahead of the daemon); this file's multi-chunk arm took over the reassembly witness.

**`fauna.media.list` also gained the additive negotiated v2 cursor** — keyset ordering that outlives
the plaintext scrub, requested by `cursor_version` and echoed in the reply; the plaintext
v1 order stayed until the flip (and left the wire entirely with the 2026-09-24 compat-remnant sweep), with a cross-order cursor replay refused as
`invalid_cursor` rather than silently mis-paging. `MediaClient::media_snapshot` asks for v2, so the
order the flip depends on is the one running in production. (v2 shipped keyed by
`(set_name_hash, path_hash)`; S5c-1 re-keyed its set component to the set's nest-local row id — see
that slice's note below for why, and for the refusal an earlier-shape v2 cursor gets.)

**The per-app swap LANDED on 7 of 7 (2026-07-29, S3; windows same day; apple 2026-08-04).** tui,
linux, web, android, windows and apple now pass the owner backup key to `MediaMachine::refresh`
(web's wasm binding derives it from the identity seed hex, mirroring `uploadSelected`/
`fetchThumbnail`, so the raw key still never crosses into JS), so the sealed-first render is live in
production on every app, not merely tested.

**⚠ The apple leg was never the cosmetic one-liner it was filed as — a keyless `refresh` EMPTIES
the page (found 2026-08-04).** `MediaMachine::render_sealed_paths` *omits* every row whose label the
reader cannot open — the ratified non-audience degrade — so a caller passing `backup_key = None`
drops **its own** sealed rows: apple's `MediaMachineVM.refresh()` left the macOS/iOS Media page
rendering **zero** items, with no `error-message` and a perfectly healthy `fauna.media.list` reply
(3 items in, 0 stored). Two lessons worth keeping: (1) a "pass the key" per-app swap is a
**correctness** item once its plane is sealed, not a rendering nicety — the last app to swap is the
one whose users see an empty page; (2) the drop was silent, which is what made it read as an empty
RPC and cost several sessions across the live tri-machine round. `render_sealed_paths` now
`tracing::warn!`s (target `fauna_media`, with `dropped`/`total`/`keyed`) whenever it omits rows, so
the next keyless caller is one grep away. Caught by the existing `test_all_media_cross_set`, which
was red on `--app macos` the whole time — the macOS media suite is outside the default `[tui]` app
set, so nothing ran it (`e2e-conventions.md` § point 7's blind spot, in its coverage form).

**The engine's FUNCTIONAL read half reconstitutes at fetch (2026-08-02, the post-flip sweep).**
The S3 render wiring below covers the *display* surfaces; the sync engine's own consumers of
`changes.list` — `apply_remote_changes`, `record_placeholders_from_changes` and the batch-latest
folds — key on the change's `path` and skip a `None`, which post-flip is every change on a sealed
plane. `SyncEngine::fetch_changes` therefore opens each fetched change's `path_sealed` through the
one shared funnel (`label_custody::render_path`, salt = the wire's `path_hash`, keys = the
engine's ordinary download custody) and fills the in-memory `path` before any consumer folds —
found by the post-flip verification sweep, where every `conformance_file_provider_client`
enumeration came back empty ("not tracked") because the flip landed with the display renders
converted but this functional twin still plaintext-keyed, structurally breaking device-to-device
catch-up convergence. The `fauna-sync` daemon's own catch-up (`apply_caught_up_changes`, a
separate `changes.list` consumer — found independently by the two-seat local test) reconstitutes
the same way via `SyncWsClient::open_recorded_path`, the read mirror of its owner-root seal
sites. A wrong-key hydration host now enumerates nothing (the Omit degrade), where during expand
it could still list from plaintext.

**Apply-path degrade ruling (2026-08-02).** The listing degrade — *omit, re-enter on
re-record* — is a DISPLAY contract and must not govern apply: an applier that skips a change and
advances its anchor loses that file on that device permanently. The apply contract is therefore
two-classed: **a seal the holder cannot open is the TRANSIENT class** (key material — an M2
generation — can lag its changes), so the pass **stops with the anchor unmoved** and the existing
retry machinery re-drives it (the daemon's `PullPhase::Retry` tick; a resident engine's next
cadence/nudge pull — the engine caps the batch at the first unopenable seq and applies everything
below it); **a record with neither plaintext nor seal is the `NoSeal` REFUSAL** — permanent like
the other locally-decidable refusals (recorded, then advanced past), never a silent skip: no
current writer lands the shape on a plane whose plaintext scrubs (the nest refuses it as
`path_seal_required`) and a plaintext-resting plane serves its `path`, so the shape names a row
no applier anywhere can use (its skip-and-advance arm was the pre-expand hash-only remnant the
compat-remnant sweep removed, 2026-09-25). **Remediation for anchors already advanced:**
devices that ran a post-flip binary before this fix skipped sealed-only changes while advancing
their anchor; `SyncDb::migrate` rewinds the `seq` anchor to 0 exactly once (meta-marker-gated),
re-listing the set — apply is convergent, so the cost is one catch-up pass and no data can be
destroyed by it.

**The transient arm is NARROWED to "no root here yet" (2026-09-20).** The ruling above classes *every* seal that does not open as transient, and that overshoots in the one direction the ruling itself warns about. "Cannot open **yet**" is the key material lagging its changes — genuinely transient, genuinely worth freezing the batch for. But a `path_sealed` blob that is not a decodable envelope, a `path_hash` that is not 32 hex bytes, and plaintext that is not UTF-8 are each decidable **locally, from the row's own bytes**, and no key that ever arrives changes the answer. Holding the anchor below one of those held it there **forever** — and because the nest cannot check a seal (it rests none of this plaintext), ONE member's malformed record froze **every other member's** catch-up on a shared set. So the apply path is three-classed, not two: a seal no root here opens **yet** keeps the transient stop-with-the-anchor-unmoved arm; a seal that can never open **anywhere** joins the permanent arm — recorded as a `catchup_failed` conflict row and skipped, with the anchor advancing past it, exactly as [`file-sync.md`](file-sync.md) § *A failed change must not strand the device* already rules for a permanently un-appliable change (and reported to the nest under the row's own label pair, rendering name-less — [`conflicts.md`](conflicts.md) § Skipped catch-up changes reach the review list); and a record with neither plaintext nor seal keeps its own permanent skip, unchanged. Only a locally-decidable criterion may enter the permanent arm: "a generation this holder will never hold" is **not** one — it is indistinguishable from "not synced yet" at a failed open — so it stays transient.

**An opened path is bound to its own row (2026-09-20).** The AEAD binds the **salt** — the wire's `path_hash` — and says nothing about what plaintext the sealer put inside it, so a seal that opens proves only *"someone holding this set's label key sealed something under this hash"*. A writer holding that key could therefore seal path P under path Q's hash: every device would write P while the nest's per-path heads, conflicts and history all recorded Q. The apply path therefore requires `path_hash(opened) == path_hash` before a row's `path` is filled at all, and a mismatch is **refused** — permanent, recorded, never applied. **The render path binds too (2026-09-27).** The earlier exemption — *a read surface displays a label, it does not act on one* — was false for one surface: the WebDAV MDA lists through the same renderer and then **acts** on the rendered name (GET serves the first row matching it, DELETE and bulk delete tombstone it, COPY/MOVE resolve their source by it), so an unbound P would have it serve Q's bytes or tombstone the owner's real P. The binding therefore lives in the one render funnel, `fauna_core::label_custody::render_path`, sharing `path_bound_to_salt` with `open_change_path`: a seal that opens to a plaintext not hashing to the row's `path_hash` is treated exactly as a seal that did not open — the row falls to its resting plaintext (which the nest hashed itself on ingest, so it is bound by construction) or takes the ordinary `Omit` degrade. It is not a refusal: a render has no anchor to account for. **Surface audit** — every production caller of `render_path`: the WebDAV MDA listing (`webdav_render_paths`) *acts*; snapshot browse/diff (`fauna-client-snapshots`), the conflict list (`fauna-devices-machine`), the media list (`fauna-media-machine`) and the file-corpus enumeration (`fauna-client-conversations`) show the rendered name to a user, and any action a user then takes from one (restore a snapshot entry, choose a conflict's winner) starts from a name the same funnel produced — so every one of them, acting or not, now receives only bound names, and no surface keeps a private exemption to re-audit when a new action is added.

**Both hosts opened through ONE function** (one host since the `fauna-sync` daemon's removal, 2026-10-02; the rule stands for any future host). `fauna_core::label_custody::open_change_path` decodes the envelope, decodes the salt, opens under caller-supplied roots, and applies the binding check, returning the three-way verdict (`Opened` / `Refused(reason)` / `NoRoot`). The shared engine hands it its generation-aware fail-closed custody (`FileDownloadKeys::label_open_roots`); the single-owner `fauna-sync` daemon handed it its one convergent chunk root. Root selection is genuinely per-host; **everything the verdict turns on is not**, and a private copy of it on either host is the drift that freezes one host's sets while the other's converge — the same one-funnel constraint the seal side has held since S6-a.

**All four read surfaces now resolve custody (2026-07-29, S3).** Custody resolution itself is one
shared object, `fauna_core::label_custody::LabelCustody` — a `(shared-set resolver, owner
BackupKey)` pair that answers "what may this reader open for this folder" — joined by
`label_custody::render_path`, which owns the **salt selection** (the wire's `path_hash` is the
salt — or there is none: every carrier ships `path_sealed` and `path_hash` together or withholds
both, so a row with no wire hash has no seal to attempt and renders from its resting plaintext or
omits; the plaintext-derived salt was the expand-era fallback, removed by the compat-remnant
sweep 2026-09-25 — the one derivation left is the fail-soft for a *malformed* wire hash, shared
with the set-name and import-source salts). Media, snapshot browse, snapshot diff
and the conflict list all go through both, so none can drift on which root is tried or which salt
opens it — a drift that would be *silent*, since a wrong root and a wrong salt both degrade to
`Omit` rather than erroring. The shared-set resolver seam (`FolderKeyResolver` /
`ResolvedFolderKeys`) moved from `fauna-media-machine` into `fauna_core::folder_keys` for the
same reason: a snapshot browse must not depend on the Media *page machine* to render a file name.

- **Snapshot browse + diff** render in `fauna_client_snapshots::SnapshotsClient` — the one client
  every app's snapshot surface routes through (linux and tui directly, apple/android/windows via
  the `fauna-ffi` mirror, web via the wasm mirror), so the render lands once rather than three
  times. Custody is opt-in per construction (`with_label_custody`); without it the client behaves
  exactly as it did before sealing. `SnapshotDiffReply` gained a wire-additive `folder`, because
  a diff is requested by two snapshot ids alone and the reply is the only place the set name could
  come from (required since the compat-remnant sweep — the nest always names the set — with the
  client's rendered, `Omit`-able view of the name on its own `SnapshotDiff` output).
- **Conflicts** render in `DevicesMachine::refresh`, at ingest and **before** the transcribe: the
  summary's `file_info` line is precomputed from `path`, so a later render would leave that line
  built from an unrendered — post-flip, empty — name. `DevicesNestApi::list_conflicts` therefore
  hands the machine the **wire** rows, the same division `MediaNestApi` already used. Custody is
  injected post-construction (`set_label_custody`, the `set_mls_query` pattern) so `refresh()`
  keeps its arity across all seven apps and its UniFFI export.
- **Wired live on linux (snapshot browse) and tui (conflicts)** — the apps that have those
  surfaces.
- **Conflict custody now also wired live on apple, windows and android (2026-08-10).** Rather than
  three per-app `set_label_custody` calls, `libs/fauna-ffi/src/devices.rs::build_devices_machine` —
  the one free fn all three FFI apps funnel through — derives the resolver + owner `BackupKey` from
  the connection's own keypair and wires it internally, the same `client()` pattern
  `FfiSnapshotsClient`/`FfiFoldersClient` already use for snapshot browse. One shared-Rust change
  closes all three FFI apps at once; a keyless (bearer-only) connection stays keyless, unchanged.
  **Conflict custody now also wired live on linux (2026-08-11)** —
  `build_devices_and_folders_pages` calls `machine.set_label_custody(fauna_client.label_custody())`
  right after building the machine, reusing the same resolver + owner key the Media page's byte
  download already uses (`apps/fauna-linux/src/client.rs::FaunaClient::label_custody`).
  **Conflict custody now also wired live on web (2026-08-13)** — the same `setLabelCustody(secretHex)`
  export (§ A sixth instance, above) builds both the resolver and the owner key so
  `DevicesMachine::refresh()` renders a shared/bound set's conflict rows exactly as linux does,
  not just its own device labels.

- **The WebDAV/MDA leg both seals and renders (S4).** The MDA is the one reader that is not a Rust
  client, so the seal/open crosses the `fauna-ffi` UniFFI boundary: `webdav_seal_path` seals a
  DAV-recorded path under the served set's **current** content-key generation, and
  `webdav_render_paths` renders a listing sealed-first, returning the `Omit` degrade as an absent
  name so the Go side omits the entry. Both delegate to `fauna_core::label_custody` — no second
  resolver, no second salt rule, no Go reimplementation. `WebdavFile` gained `path_sealed` +
  `path_hash` (the salt, without which a scrubbed row is unrenderable) and
  `WebdavRecordChangeRequest` gained `path_sealed`; the bridge renders once per listing at the
  single `Backend.listFiles` seam, so the DAV hierarchy synthesis keeps operating on session-memory
  plaintext. **All four DAV writers seal** — PUT, the delete tombstone, and both COPY/MOVE legs —
  so the DAV leg is no longer a keyless writer seam. A label whose `gen` names a generation the
  session lacks (most importantly `gen: None`, the owner root the MDA structurally never holds)
  omits post-flip and still lists from plaintext during expand.

**The set-name plane's seal, render and admin addressing are BUILT (2026-07-29, S5).** A set
**name** is sealed by a **keyed writer, not by the create gesture** — the shared create gesture
(`fauna-folders-machine`) holds no key material on any app, so the set-name seal is stamped by the
sync engine's bind/serve catch-up pass (`SyncEngine::stamp_sealed_set_name`, whose pure half
`sealed_set_name` takes byte-for-byte `seal_recorded_path`'s root selection, so a set's name and its
paths always render for the same audience). Consequence worth stating: **a set's name seals when an
engine first binds it, not at create time.** The stamp is convergent and therefore free to repeat,
and its nest call is best-effort — a name is a display label and must never fail the bind it rides
on. **Since 2026-10-02 a set is also sealed from birth:** the one shared create helper
(`fauna_client_folders::create_set`, which every production create routes through) sends the new
set's `name_hash` and — under the owner's `convergent_chunk_root()`, a brand-new set being
owner-only — its `name_sealed` and `retention_policy_sealed` on `fauna.folders.create`
(`set_lifecycle::seal_at_create`); the nest refuses a `name_hash` that is not the hash of the
request's `name`. Every app's create seals — the folder wizard's seam takes the owner key
explicitly (`create_set_with_owner_root`, never as label custody on a client that could later
update a bound set): a native build derives it from its authenticated connection, the web build
from the actor identity the SPA hands the devices machine (the browser connection carries no
keypair); `FfiFoldersClient::create` holds label custody. The engine's catch-up stamp and the update-time backfill stay, as the repair for
a custody-less client and for the re-seal a bind's audience change needs. **A share's bind re-seals the name itself (2026-10-03):** `FoldersAuthor::bind_set` re-seals the name under the genesis content key and pushes it by hash, best-effort — a member holds the set's content keys and never the owner's root, and a set no owner device has bound to a local folder has no engine to stamp it, so without this the set is omitted from every member's list. The engine's stamp runs at every engine start, last of the start passes (`always_resident::converge_corpus_at_start`), and goes out by hash; it had no caller from 2026-09-25 to 2026-10-03, which the resting plaintext name hid until schema 114. **A create carries no
path list at all:** the include/exclude lists seal under the `folders.id` the create mints, so the
plaintext pair left `fauna.folders.create` (2026-10-01 survey A25) and the lists arrive on the
first keyed `fauna.folders.update`. `fauna.folders.update` gained an additive `name_sealed` as the stamp's push, and
`FolderSummary` gained `name_sealed` **plus its `name_hash` salt** (the pair that survives the
scrub — a seal without the salt is unrenderable, the hole found on `fauna.media.list` and
`WebdavFile`); both the owner and the *member* projection carry them, because a roster member is
exactly the audience the name is sealed to. The render seam is
`fauna_core::label_custody::render_set_name` (+ `set_name_label_salt`), the set-name twin of
`render_path`, and the seal funnel is `label_custody::seal_set_name`, which **refuses a reserved
`__` name by construction** — the nest refuses one again at `fauna.folders.update`, since ~28 nest
decision sites route on those literal names. `fauna_core::sync::is_reserved_folder_name` is now
the shared owner of the `__` convention for exactly that reason. **Admin addressing switched:**
`fauna.admin.folders.{get,add_member,add_destination}` accept an additive `name_hash`, resolved by
the same 0/1/many ambiguity contract as the plaintext name (`(name_hash, actor_id)` is exact by
`UNIQUE(name_hash, actor_id)`; a bare hash is ambiguous exactly as a bare name is) — this is how an
admin, who is not a set's key audience, addresses a row once the flip scrubs the name they cannot
read.

**The folders list renders the name client-side (2026-10-02).** Both `fauna.folders.list`
projections, read through `FoldersClient::{list, list_owned_and_shared}`, render each row's `name`
sealed-first from `name_sealed` under its `name_hash` (the same pass that renders the retention
policy), so every app keeps reading `FolderSummary::name` once the nest stops resting the plaintext.
A row whose seal this reader cannot open is omitted rather than shown blank, and an unsealed row (a
reserved `__` or public-audience set) keeps its plaintext. The render needs the reader's label
custody, so a consumer without it never reads the rendered list — it would omit every sealed set:
it reads `FoldersClient::{list_wire, list_owned_and_shared_wire}` (the rows as the nest sent them,
a sealed set's `name` the empty sentinel) and finds a set it holds a name for by hash
(`FolderSummary::is_named`), or renders with custody it holds itself (the devices machine renders
the name and retention policy by hash before transcribing; the photo-library seam opens the names
with the owner key it seals under). **An engine host names the row it binds (2026-10-03):** an engine takes its set's name from that row, so the shared engine build, the sync agent's content-key resolution and the share host's (`resolve_engine_keys`) read the wire rows and name each sealed one from the custody they already hold, `engine_binding::named_for_engine_host` — the holder's own row from custody by hash, a member's row by opening `name_sealed` under the set's content keys, the binding the engine is handed for the set's bytes; a sealed row neither names gets no engine until an edge re-reads. Pure and in the base crate, because the bearer-only agent builds none of the `mls` graph that `NestFolderKeyResolver` lives in. A wire row's blank name built an engine that addressed the nest by the hash of the empty string. A consumer that holds folder-key custody but no label custody names its own sets the same way, `custody::named_from_custody` (the custody reconcile, the WebDAV keys blob, the served-set and paywall launch resumes). **Custody resolves by `name_hash`
(2026-10-02):** `FolderKeyResolver::resolve` takes the set's `name_hash`, and
`NestFolderKeyResolver` matches roster rows (`custody::find_roster_row`) and foreign records
(`custody::find_foreign_set_by_hash`) by hash, a row's address being its projected `name_hash` or
the hash of its plaintext (`label_custody::set_name_label_salt`). `LabelCustody::keys_for(name)`
hashes the name and delegates to `keys_for_hash`, which the list render calls with the row's own
hash, because the keys are needed before the name can be opened. `fauna-client-sync`'s row judge
matches its sets the same way. **Every render over a nest reply resolves custody by the row's hash
(2026-10-02):** `LabelCustody::keys_for_row(plaintext, wire_hash)` (the row's `folder_hash` /
`name_hash`, else the hash of its plaintext) is the one call a render site makes for a wire row's
set — snapshot browse and diff, the Media page, the devices conflict list, the content-index file
walk, the folders seal backfill — and per-set caches key by that hash, never by the plaintext,
which is the same empty sentinel for every scrubbed set. `keys_for(name)` stays for a name the user
chose or a render already opened (gesture targets, create, retention seals). The seal backfill
skips the name and retention stamps of a scrubbed row: a blank name is neither plaintext nor salt.

**The user-facing `name_hash` addressing fan-out landed (S5b, 2026-07-30):** all 23 (not the
earlier ~16 estimate — the true count once every `folder_authz`-mediated call site is
enumerated) `fauna.folders.*` / `fauna.filesync.*` / `fauna.sync.*` by-name request types accept
an additive `name_hash`, resolved hash-first with the same convention the admin plane uses (a
malformed hash refuses, never silently downgrades to the name arm). Two classes of site were
found and left unchanged because they only ever address a **reserved** (`__`) name, which never
seals: `federation_handlers.rs`'s cross-nest backup-custody resolver (`resolve_backup_custody_set`,
always one of `__mail`/`__post`/`__calendar`/`__card`/`__conv/<hex>`) and `folders.create`'s own
reserved-name collision check. A handful of handlers had also been separately re-deriving the
mutation from the plaintext wire `name` even after an owner-scoped hash resolve (`update`,
`set_web_paywall`, `delete`, `share`) — fixed to key the mutation off the resolved row's name, not
the (possibly empty, hash-addressed) wire field.

**The media plane's set name now seals, renders and projects per reader (S5c-1, 2026-07-30).**
`MediaItem` gained the `folder_sealed` + `folder_hash` pair, and `MediaMachine::refresh` renders
the set name through `label_custody::render_set_name` beside the path it already rendered — same
custody, same degrade, so a set whose name a reader cannot open omits its rows rather than listing
them under an empty set. **The pair is projected to the seal's audience only.** The nest classifies
*why* a caller may read each set (`folder_authz::FolderReadGrant` — owner / roster member /
Q5-admin-discovery, an audience distinction, not a new permission) and sends both halves to the
first two and **neither** to an admin who holds no key for the set: the `name_hash` salt is an
unkeyed digest of a user-chosen name, so shipping it to a reader who cannot open the seal would hand
back by dictionary exactly what the seal withholds. The `fauna.media.list` **v2 cursor** re-keyed
from `set_name_hash(name)` to the set's nest-local **row id** for the same reason — the cursor is
nest-minted and nest-consumed, so it needs stability, not convergence — and a cursor of the earlier
v2 shape is refused `invalid_cursor` rather than mis-paged.

**The path axis was untouched by S5c-1 and is now closed too (S5d + S5e, 2026-07-30).** Gating
`path_hash` on the reader's audience was a change to the then-ratified floor exemption rather than
an implementation choice, so it was ruled first (`../architecture/encryption-at-rest.md`
§ Carve-outs: the floor exemption does not survive per-reader) and then executed — **S5d** gated
`MediaItem.path_hash`/`path_sealed` on the same `is_label_audience()`, and **S5e** brought the two
sibling planes under the same rule (below).

**The other three set-name-carrying read planes now carry the pair too (S5c-2, 2026-07-30):**
`SyncConflict`, `SnapshotDiffReply`, `WelcomeInbox`/the push `WelcomePayload` each gained
`folder_sealed`/`folder_hash` (or `set_name_sealed`/`set_name_hash` on the Welcome pair),
shipped **unconditionally at the time** — the reasoning being that, unlike `media.list`'s aggregate,
none of these three mixes audience and non-audience *rows*. ⚠ **That reasoning was wrong for
`SnapshotDiffReply`, and S5e fixed it:** "the reply is not a multi-set aggregate" and "every reader of
this reply holds the key" are different claims, and `authorize_snapshot` admits a Q5 `AdminDiscovery`
reader to a group-bound set's `diff`. A single-set reply can still have a non-audience *reader*. The
other two planes' unconditional shipping stands (`SyncConflict` is participant-audience;
`WelcomeInbox`/`WelcomePayload` go to the invitee). Render
seam: `label_custody::render_set_name`, same shared function `media.list` uses; an `Omit` drops
the whole conflict row (owner/participant-audience surface, so an unrenderable row has no
non-audience reader to still show it to) or nulls the single top-level `folder` of the client's
rendered `SnapshotDiff` (the wire `SnapshotDiffReply.folder` itself is required; not a list row,
so there is nothing to drop). The Welcome pair also crosses the federation
relay: `FedWelcomeDeliverRequest` (the origin→peer internal wire, distinct from the client-facing
`WelcomeInbox`) carries the same pair, resolved from the ORIGIN nest's claimed `folders` row and
forwarded verbatim by the receiving nest into its own recipient's local envelope — sound because a
shared set's seal is under the M2 content key every roster member holds, cross-nest included.

**Two residuals stated plainly, not solved by S5c-2 — read before assuming either render path is
complete:**
- **Custody by plaintext `folder` — closed (2026-10-02).** Render sites resolve by the wire row's
  hash through `LabelCustody::keys_for_row` (the set-name plane paragraph above), so a shared set's
  names and paths keep rendering once `folder` blanks. What stays is the wire shape:
  `MediaItem.folder` and `SyncConflict.folder` are still non-`Option` `String`s carrying the empty
  sentinel after the scrub — a shape, not a resting plaintext: every reader renders the name from
  the sealed pair (the Media page overwrites `item.folder` with it).
- **The Welcome carries the pair alone for a sealed set (2026-10-02).** `welcome.deliver` stamps
  `set_name` from the claimed row, so a set whose row rests no name ships `set_name: None` beside
  `set_name_sealed` + `set_name_hash`, and the resting inbox envelope holds no plaintext (pin:
  `conformance_conversations_welcome::deliver_folder_kind_stamps_the_set_name_seal_and_salt_pair`).
- **The pending-share list (`list_folder_pending_shares`) renders the Welcome pair keyless by
  construction, and — once the flip lands — that means `set_name: None` for essentially every
  pending share.** A pending share is by definition a group this reader has not yet joined, and
  there is no "derive keys from an unjoined Welcome" capability in `fauna-mls` — so the seal is
  genuinely unopenable until accept, at which point the client already falls back to its
  unknown-set i18n label. This is the correct fail-closed shape, not a bug, but it is a real UX
  regression at the flip worth knowing about. **The join opens the pair (2026-10-04).** Every
  folder-Welcome join — the accept and the auto-join contact-gate path alike — carries the pair as
  `FolderWelcomeContext.set_name_seal` into `join_folder_welcome`, which records a cross-nest
  member's foreign set nameless, ingests the owner's content-key envelope (published before the
  Welcome is delivered), then opens the seal under the set's M2 content keys and names the record
  (`custody::name_foreign_set_from_seal`, gain-only). A nameless foreign set has no engine binding
  and no Folders-page label, so without this a cross-nest share never synced (pin:
  `cross_nest_agent_capstone`'s L2 leg).

**The two sibling planes carry the same rule (S5e, 2026-07-30).** The ruled property — a reader who
cannot open a seal is not sent its salt — holds on every producer of the pair, not on `MediaItem`
alone:
- **The snapshot planes project on audience.** The gate lives where the grant is computed —
  `authorize_snapshot` **returns** the `FolderReadGrant` — and
  `fauna.filesync.snapshot.get`/`.diff` project both pairs on `is_label_audience()`; `diff` is a
  producer of the **set-name** pair too, so it gates on the same call. Write callers
  (`delete`/`undelete`) bind the grant as `_`.
- **The v2 pagination cursor is opaque to every holder.** The position key *must* stay the true hash
  (audience-gating it mis-pages the listing), so **the cursor is sealed** under a domain-separated
  subkey of the nest's durable deployment key (`bins/fauna-nest/src/cursor_seal.rs`) rather than
  handed back in the open. No new key material: the deployment signing key already persists and
  restores. The earlier "this needs a persistent nest secret the nest does not have" note — recorded twice, in
  S5c-1's cursor re-key and S5d's declared residual — was **false**; see the module doc.
Both are pinned by conformance tests with audience-positive twins, a two-page `limit = 1` cursor
assertion, and a mutation check.

**The conflict plane now has a write half (S6-a, 2026-07-30) — and the path plane has one funnel.**
`SyncConflict` has carried `path_hash`/`path_sealed`/`details_sealed` since S2, and since S6-a the
writer matches the reply type, so conflict rows rest sealed: `ConflictReportRequest` carries all
three, both client builders mint them, and the nest stores them opaquely.
Two things worth carrying forward:
- **Every folder-relative path in every table seals under the single field tag
  `LabelField::SyncChangePath`** — never a per-table variant. This is forced, not stylistic: the nest
  copies `path_sealed` *verbatim* between tables and holds no key to re-seal with (`sync_changes` →
  `snapshot_files`), which is exactly what the self-describing `gen` field exists to make safe, and
  the tag is mixed into both the key derivation and the AAD, so a per-table tag would make every
  copied blob fail to open — silently, as `Omit` rather than an error. The unused per-table variants
  (`SnapshotFilePath`, `BackupCustodyPath`, `BackupCustodyGenerationPath`, `ConflictPath`) were removed
  (no blob sealed under them survives the baseline reset), so a per-table tag cannot be written by accident. Conflict `details` keeps its own tag: it is never copied between tables, so it
  carries none of that constraint, and it shares the row's `path_hash` as its salt — sound precisely
  because the tag separates the two derivations, which a test pins on stored rows.
- **The three hand-rolled path-seal sites are gone.** `SyncEngine::seal_recorded_path`, the
  `fauna-sync` daemon's data-plane notify (until the daemon's removal, 2026-10-02) and the Media
  upload gesture now all call one shared funnel
  (`fauna_core::label_custody::seal_path`), which is what stops this slice from adding a fourth. Root
  selection stays with each caller — the one thing that legitimately varies. `details` seals through
  the sibling `seal_conflict_details` (**random** nonce: mutable prose the salt does not determine).

**The hash-only request shape now works on every kind that accepts a `name_hash` (2026-07-30).**
S5b's fan-out gave 23 by-name kinds an additive `name_hash`, but two of them read it only *inside*
their `folder.is_some()` arm — so a `folder: None, name_hash: Some(h)` request (the shape every
client sends once the flip stops sending cleartext names) neither refused a malformed hash nor
resolved to the addressed set: `fauna.sync.changes.list` fell through to the caller's changes across
**every** set they own, and `fauna.filesync.snapshot.list` to the bearer's message-kind rows. Both
now parse the hash unconditionally, like the other 21, and treat `name_hash` as a set selector in
its own right. Bearer scoping meant no cross-actor row was ever reachable; the defect was scope and
skipped input validation on exactly the shape the flip makes normal. Pinned per kind by a
hash-only scope assertion **and** a malformed-hash-with-no-plaintext-name refusal — the two halves
are independently load-bearing, which a mutation check confirms (reverting only the scope half
leaves the refusal green). The hash arm of both `folder_authz` resolvers also gained its first
cross-actor negatives: a hash is an address, never a grant, so the owner scope, the roster check on
the deliberately global candidate query, and the writer-role gate all still apply when a caller
addresses by hash.

**The two set-scoped kinds that once had no `name_hash` now carry it (2026-10-02).**
`fauna.stats.get` and `fauna.files.versions.list` scoped by the plaintext set name only and
resolve outside `folder_authz`, which is why S5b's sweep of resolver-mediated call sites did not
reach them; a scrubbed `name` column would have dropped `stats.get`'s per-set branch and silently
widened `versions.list` to the union across all the caller's audience sets. Both requests now take
an optional `name_hash` that resolves first (`stats.get` through the owner-scoped by-hash lookup,
`versions.list` by narrowing its audience-set scope on the row's `name_hash`), and the client sends
both through the funnel below. `fauna.files.versions.{get,undelete}` address a version by
`path_hash` alone and never named a set.

**The log + error-string scrub is BUILT (S7, 2026-07-31).** Every nest-side and daemon-side
(`bins/fauna-nest`, `bins/fauna-sync`) log line and error string that interpolated a user-chosen
path, folder name, or import-source descriptor now carries only a short hash-prefix redaction
(`fauna_core::log_redact::{log_path,log_folder_name,log_hash_prefix}`) — a reserved (`__`) set
name stays literal (a routing constant, not user data), every other name/path hashes. Three
surfaces named in the design record got specific treatment: `manifest_reference_sources` (the GC
debug diagnostic) now selects only the existing `path_hash`/`name_hash` columns, never plaintext;
`fauna.filesync.snapshot.check`'s `SnapshotCheckReply` gained an additive `structured_errors` field
(`SnapshotCheckError { kind, snapshot_id, path_hash, manifest_hash, chunk_hash }`) alongside the
now-redacted `errors: Vec<String>` (which left the wire with the 2026-09-24 compat-remnant sweep), so a client can still render a friendly message by joining on
the hash against its own decrypted listing; and the import-lock conflict error drops the source
descriptor from its message entirely (neither plaintext nor hashed), per the design record's "drops
the descriptor from its message" line. Proven end to end, not just at the source level: a
conformance test drives a real `web_files` rejection (a website set's `fauna.sync.changes.record` naming a server-side-executable path; the earlier `sync_ws` case left with that data plane) through the real `tracing` ring and the real
`fauna.admin.logs` RPC handler, and asserts the admin's reply carries the redacted form only
(`bins/fauna-nest/tests/conformance_log_redaction.rs`). **Discovered, not fixed:** the daemon's
local merge-base cache (`bins/fauna-sync`'s `cache_get`/`cache_put` under `base:<relative_path>`
keys) still names the plaintext path in a local cache **filename** on disk — not a log line or an
error string, so out of S7's scope, but worth a future slice if that cache directory is ever
admin-readable on a co-located deployment. **`libs/fauna-sync-engine` — the daemon's own engine
crate, shared by both `bins/fauna-nest` and `bins/fauna-sync` — sat outside S7's original
inventory** (its own T6 slice scrubbed only `bins/fauna-nest/src` and `bins/fauna-sync/src`) until
finding brought its `tracing` field/format-string call sites into line the same way; its
`anyhow!`/`.context()` error-string construction sites are a further, still-open residual of the
same gap (tracked, not yet fixed).

**The keyless writer seams are CLOSED (S8 D2, 2026-08-01)** — all four, not the two the earlier
list named (the caller audit found two more, the devices-machine review-list re-point and the
windows agent's restore verb, both riding the same shared record): the Media *delete* and
*restore* gestures seal through injected write-side custody (`MediaMachine::set_owner_backup_key`
+ the set resolver, resolver-first so a bound set seals under the M2 generation its roster holds
and **fails closed** when those keys are unresolvable — never the owner-root fallback; the
resolver is wired on all 7 apps: linux + tui build it themselves, web since 2026-09-25, and
android / apple / windows since 2026-09-29 through `fauna-ffi`'s one `build_media_machine`,
pinned at that call site — before that the UniFFI builder was resolver-less, so
those three apps sealed a bound set's gesture paths under the owner root); the
devices-machine re-point seals from the machine's `LabelCustody` (live wherever
`set_label_custody` is already wired); the windows agent's restore verb seals from the
capability's app-pushed per-set engine keys + `BackupKey` through the pinned
`FileDownloadKeys::label_seal_root`; and the `fauna-ffi` record export mints from the
connection's own custody assembly inside `FfiSyncClient::changes_record`, UniFFI
signature unchanged. Every arm stays best-effort — a keyless holder records plaintext-only, an
honest S8 backfill row. ⚠ Rows those writers recorded **before** this close never converge by
re-record (`sync_changes` is append-only and its content-idempotence compare deliberately
excludes `path_sealed`), so pre-D2 keyless heads — delete tombstones above all — are a named
population for the S9 scrub ask, not a backfill target. So are the **owner-root-sealed heads**
android / apple / windows recorded on a bound set before 2026-09-29: sealed, but under a root
no roster member can open, so members render them only while the plaintext column rides.
**The client-driven backfill pass is BUILT (S8 D1 + D3, 2026-08-01).**
`FoldersClient::backfill_sealed_fields` walks the owner's raw list and stamps every missing
folder-plane sealed sibling from the still-resting dual-write plaintext through the nest's
seal-only update arm — predicates keyed on the nest-observed NULL (idempotent, no local marker;
random-nonce fields only-if-missing), reserved rails skipped, and a bound row whose M2 keys the
custody cannot resolve skips its audience-root fields **fail-closed** (the bound-but-unresolvable
guard, derived from the row's own `mls_group_id` because a resolver-less root selection would
silently fall to the owner arm). **That fail-closed contract became true in code on 2026-08-01**
(before then it held only at D1's own row-level check, because the shared resolver collapsed three
distinct answers into one). `FolderKeyResolver::resolve` is now
**three-valued** — `Err` = could-not-determine (roster/config failure, corrupt group hex) and
yields *no* keys, never the owner fallback; `Ok(None)` = positively unbound, where the owner root
is correct; `Ok(Some)` with `content_keys: None` = bound-but-unresolvable, which keeps its
`mls_group_id` so `label_seal_root()`'s bail fires at every seal site with no per-site logic. A
matching-named foreign set is no longer mistaken for an owned one, and D1 additionally checks row
identity before stamping. The accepted trade is on the read path: a *transient* resolve failure
now errors an unbound set's download instead of succeeding via the owner fallback — an availability
blip that retries, taken over sealing under a root the audience cannot open, which is durable loss.
The immutable snapshot plane — which has no later mutation to ride — gets the
**one new wire kind the S8 design licenses**: `fauna.filesync.snapshot.stamp_labels`
(label-audience only via the S5e grant, stamp-only — a tag-less snapshot refuses, so a seal can
never conjure tags — and overwrite-allowed **along one axis only**, which is what makes the
owner-root-axis re-seal possible), driven by `SnapshotsClient::backfill_tag_seals`,
whose only re-stamp trigger is the root AXIS (a seal under an older generation still opens via
the chained key history and is left alone). **Since 2026-08-01 the nest
enforces that axis rule itself rather than trusting the client to hold it**: a resting envelope
that already names a key generation is roster-rooted — its whole audience opens it, so nothing
is left to converge — and the stamp freezes with `invalid_request`; an owner-root `gen: None`
seal, an empty column, or bytes that parse for nobody stay stampable, and the write is a
compare-and-swap on the bytes the predicate decided against so a concurrent stamp is refused
rather than clobbered. The nest reads the **resting** envelope's header only — never any
ciphertext (it holds no key), and never the incoming bytes at all, because format-validating
those would make an older nest refuse a newer client's envelope revision. `SnapshotSummaryRow` carries the `tags`/`tags_sealed` pair for the pass —
ungated, because both list arms are label-audience by construction (`resolve_readable_folder`
has no admin arm; the message-kind arm is bearer-self-scoped). The headless daemon ran both
passes once per start until its removal (2026-10-02); device labels converge with **no new code** (both connect paths
re-register keyed on every boot — the daemon and `build_engine`).
**android enablement landed 2026-08-02** — all three calls,
matching linux/tui: `MediaMachine::set_owner_backup_key` wired at `MediaVM.ensureMachine()`
(the existing `ApiClient.ownerBackupKey()` derivation, already used by `upload()`); the S8 D1 +
D3 backfill passes wired at the same universal post-auth hook as the critical-alert sweep
(`MailEnableGlueVM.runSealBackfill`), through two new UniFFI exports —
`FfiFoldersClient::backfill_sealed_fields` / `FfiSnapshotsClient::backfill_tag_seals` — that
reuse each façade's own already-resolver-wired `client()` (no second derivation). Exposed a gap
in the process: the `FfiFolder` UniFFI mirror had silently dropped the wire's `role` field
entirely (needed to skip `role == "member"` rows, the same daemon rule), added as an additive
`Option<String>`.

**apple enablement landed 2026-08-02** — all three calls, both macOS and iOS (one shared FaunaKit
edit covers both targets): `MediaMachine::set_owner_backup_key` wired at
`MediaMachineVM.configure()` right after the machine is built, using the existing
`APIClient.ownerBackupKeyBytes()` derivation (already used by `upload()`/`fetchThumbnail`); the
S8 D1 + D3 backfill passes wired at `FaunaClient.start()` (the same session-start hook
`refreshMailEpochSchedule` uses), through `APIClient.backfillSealedFields()` /
`APIClient.backfillTagSeals()`, which call the already-resolver-wired `foldersClient()` /
`snapshotsClient()` façades — no second custody derivation. `swift-test` green, 326/326.

**web's `set_owner_backup_key` leg landed the same day** — a new `MediaMachine.setOwnerBackupKey`
wasm export (`libs/fauna-wasm-media`), called once after construction in
`routes/media/+page.svelte`, deriving the key from `secretHex` exactly as `refresh`/
`uploadSelected` already do (the raw key never crosses into JS).

**web's D1/D3 backfill legs landed 2026-08-11 — the per-app batch is now complete on all 7 apps.**
They were unwired until then for a stated architectural reason, not a missed one-liner: the
resolver both passes need (`fauna_client_folders::NestFolderKeyResolver`) was native-only, so no
wasm build could construct a resolver-backed `LabelCustody`, and wiring an owner-only custody
instead would have risked exactly the class this section warns against (a bound set's fields
sealing under the owner root instead of failing closed) — so per the S8 rule ("a
keyless/bound-unresolvable custody must stay INERT") the legs stayed unwired rather than unsafe.
The resolver became transport-generic over `RpcRequester` in the cross-nest foreign-set work
(`custody_ingest.rs` — one `?Send` impl per transport, native `Arc<NestClient>` + wasm
`WsRpcClient`), which is what unblocked them.

**What web wired is the whole sweep, not two calls** (`fauna_client_folders::seal_backfill`): D1,
then the roster, then D3 per **owned** set (`role == "member"` skipped), every step
independently best-effort. That sequencing had been hand-written once per client — `bins/fauna-sync`,
windows' `SealBackfillSweep.cs`, apple's `FaunaClient.runSealBackfill`, android's
`MailEnableGlueVM.runSealBackfill` — so rather than adding a fifth copy in TypeScript it moved into
shared Rust, where `run_sweep(transport, keypair)` builds both clients over the one
`resolver_backed_custody` constructor. Web calls it at the same post-auth gate as the critical-alert
sweep (`runSealBackfillSweep`, `+layout.svelte`); `bins/fauna-sync` was converted onto it in the same
change, which is what proves the sweep against both transports. **A UniFFI face landed 2026-08-13**
(`FfiFoldersClient::run_seal_backfill_sweep`, wired through the same `resolver_backed_custody`/
`run_sweep` constructor, degrading to keyless `sweep_with` where the crate's `folders-author`
feature is off) — **android adopted it the same day**, `MailEnableGlueVM.runSealBackfill` now calling
the one sweep instead of hand-rolling D1-then-D3-skip-member. **windows' `SealBackfillSweep.cs` and
apple's `FaunaClient.runSealBackfill` still run their own loops** — same policy, not yet lifted
; they are correct today, and the new FFI face is what their lift
adopts.

**windows enablement landed 2026-08-03** — all three calls, closing the whole per-app batch (only
web's D1/D3 residual above remains open): `MediaMachine::set_owner_backup_key` wired at
`MediaPage.xaml.cs::Page_Loaded` right after the machine is built, reusing the existing
`FaunaFfiMethods.BackupKeyDerive(_crypto.SecretBytes)` derivation the read path (thumbnail loader
+ `Refresh`) already computes — no second derivation; the S8 D1 + D3 backfill passes wired at the
same universal post-auth hook as `CriticalAlertsSweep` (`Core.Helpers.SealBackfillSweep.RunAsync`,
called from `App.xaml.cs::StartMainAppAsync`), through two new `INestRpcClient` methods —
`BackfillSealedFieldsAsync()` / `BackfillTagSealsAsync(folder)` — that reuse the already-wired
`nest.Folders()`/`nest.Snapshots()` façades. Unit-pinned in `SealBackfillSweepTests.cs` (D1-then-D3
ordering, the owner-only `role == "member"` skip, never-throws), `FaunaApp.Tests` 1265/1265 green.
The `folders.name` cutover leg (gated on the per-app
hash-sender batch) remains separate. **The S9 flip itself LANDED
2026-08-02 (v32):** the scrub graduated to production as the boot reconcile's every-boot pass
(`migrations.rs::SCRUB_PLANES` / `run_scrub_plaintext`; the `test-hooks` method wraps the same
table), minus `folders.retention_policy` — withdrawn from the planes at the flip per the
ARMED auto-prune ruling (`encryption-at-rest.md` § Carve-outs: nest-parsed knobs, not a
label) — and plus `import_sessions.source_descriptor`, whose plaintext lock index rebuilt onto
`source_hash`. The at-rest proof is now the pair in
`bins/fauna-nest/tests/conformance_at_rest_byte_scan.rs`: the never-rests scan through the next
boot's scrub + `VACUUM` (the pre-flip-image reconcile boot retired with the flip at the
2026-09-24 genesis of the nest schema), and the self-backup rotation proof; and
`bins/fauna-sync/tests/at_rest_cache_artifacts.rs` pins the daemon's cache-dir artifact state
after the real apply flow — every resting entry flat and hash-shaped (at artifact
level).
**The one surface the scrub cannot reach is RULED (2026-08-01): the
self-backup window is ratified and bounded, not invalidated.** The nest's hourly unencrypted
hot-copies of `nest.db` (`backup-restore.md` § 11 owns the mechanism and its retention) keep
pre-scrub plaintext for up to 24 post-flip backup cycles (≈ 24 h of nest uptime) until
count-based rotation deletes them; the flip does **not** invalidate the store — those images are
the destructive migration's only rollback. The ruling, its rejected alternatives, and the three
flip-contract requirements it adds (an immediate post-scrub hot-copy; an idempotent,
boot-reconciled scrub so a window-time restore cannot silently reopen the planes; the D5 byte
scan extended over the self-backup store on the rotation clock) are owned by
`../architecture/encryption-at-rest.md` § Carve-outs. Scope, deliberately narrow **and now
pinned**: the *daily logical dump* is NOT affected — `DUMP_TABLES`
(`bins/fauna-nest/src/export/logical.rs`) omits `folders`, `sync_changes`, `snapshots` and
`snapshot_files` entirely, so it carries no name plane at all. That was a point-in-time manual
check until 2026-08-15; the standing witness is now
`export::logical::tests::the_dump_carries_no_sealed_plane`, which **walks** the live schema for
`sealed`/`*_sealed` columns rather than re-listing those four names — so it also reds on the two
breakages the names alone would miss (a new sealed table added to `DUMP_TABLES`, and a new sealed
column grown on a table already dumped). ⚠ The list's narrowness is therefore a **confidentiality
boundary, not drift to close**: `account-data-plane.md` § Nest-side requirements item 1 names the
dump's hand list as W1 (account-data-plane.md § Workstreams) follow-on, and converging it on `ACTOR_TABLES` (or on a `sqlite_master`
walk) would pull all four of these tables in and break this ruling. The gate reds if attempted;
the `DUMP_TABLES` doc comment carries the same warning at the code site.
**Of the aux fields (S6), the conflict plane above and the device label are built.**
`sync_devices.label` now seals under the **registering owner's** root (salt = the `device_id`
already on every row, so this plane needs no hash companion; random nonce) through
`fauna_core::label_custody::seal_device_label`, at all four user-facing writers — the engine's
lifecycle registration, the `fauna-sync` daemon (removed 2026-10-02), `setup_renewal_grant` (shared by the unix agent
provisioner and the windows app), and `FfiSyncClient::register`, which derives the root from its
own connection keypair so the UniFFI surface stays byte-identical. `fauna.sync.devices.list`
carries `label_sealed` **ungated** — that reply is `WHERE actor_id = ?1`, so its reader is by SQL
the registering owner and therefore always the seal's audience — and `DevicesMachine::render_devices`
renders it sealed-first under owner-only custody at ingest. An unopenable label degrades to an
**empty label on a kept row**, deliberately weaker than the path/set-name degrade: a device is
actionable by `device_id` alone (revoke it, see it online), and dropping the row would hide a device
the user may need to revoke.
**Two known gaps on the device plane, both flip-completeness rather than confidentiality — plus a
third cross-actor surface, RULED rather than open.**
(a) `fauna.folders.devices` and `fauna.folders.members.list` join `sync_devices` on `device_id`
**and the reader's `actor_id`** (2026-09-24; pinned by `db/mod.rs`'s
`folder_device_label_joins_are_scoped_to_the_reader`): one row per seat whoever else registered
that device id, and a label only from the reader's own registration. Before that the join carried
no actor predicate — a latent shape (duplicate rows), **not** a live disclosure, and what remains
open here is the foreign row's identity, not a leak. The reply's label is empty for every
user-named device, so **the place editor names its seats client-side**:
`fauna_devices_machine::place_rows` joins `members.list` to the app's own unsealed device roster
(`DevicesSnapshot::devices`) on `device_id`, the one join all seven apps call, and a seat the
roster does not hold keeps the reply's label. Post-flip there is no
plaintext left to hand over: every user-chosen label rests `''` in `sync_devices.label` (the
register upsert writes `String::new()` for any non-synthetic label, `db/sync_storage.rs`; the S8
scrub and flip-inventory (b) cleared the back-catalogue, `db/migrations.rs`), and the invariant is
pinned at-rest by `conformance_at_rest_byte_scan.rs`'s
`after_the_write_flip_no_sealed_plane_plaintext_ever_rests`, which drives the production writer and
byte-scans the resting DB. Only the two machine-authored synthetic labels
(`is_synthetic_device_label`) rest readable, and those are constants, not user data. So a foreign
device row renders with an **empty** label, identified by `device_id` alone — flip-completeness,
exactly as this section's lead-in says, and the reason the ratified direction below is about giving
that row an identity rather than plugging a leak. Two narrower facts worth keeping: `folder_members`
cannot hold a foreign device through any *user* door (both writers verify the device's owner —
`add_folder_member_for_user`, `set_folder_place_flags`), so only the Admin door
`fauna.admin.folders.add_member` can seat one, which makes the cross-actor case reachable in
practice only on `fauna.folders.devices` (via a shared set's multi-writer `sync_changes` rows); and
neither reply carries a sealed sibling, because the seal is under the *registering* owner's root and
a foreign reader could not open one. The ratified direction for those two is *"cross-actor member
listings render actor identity, not the device label"* — which needs an actor-identity carrier
neither `FolderDevice` nor `FolderMember` has today, so it rides the flip; until then a foreign
seat renders nameless. ⚠ **Keep the reader predicate on both joins and that byte scan green** —
together they are what stops any future re-population of plaintext `label` from turning back into
a cross-actor leak.
(b) A keyless re-register (the ffi bearer-only arm) clears `label_sealed` alongside the label it
replaces — the pair moves together on purpose, since a retained seal would open to the *previous*
name — leaving an S8 backfill row rather than a silently stale one.
(c) **The guardian's ward-device projection — RULED 2026-08-02, not open.** `fauna.family.status`'s
`FamilyWardDeviceInfo` is the one read surface that hands *another actor* a device row list where
every row belongs to the **same** registering owner (the ward) — so gap (a)'s actor-identity answer
is degenerate (all rows are one actor), yet the reader-holds-no-key premise is identical: the
guardian holds no key for the ward's root, and a guardian-openable second seal is forbidden outright
(`family-safety.md` § Don't do these — no guardian key escrow). No sealed sibling rides the
projection, deliberately and permanently. The ruling: the nest populates the projection's `label`
with the **device display identity** — the device code, with the machine-authored plaintext label
beside it when one rests, chosen over the ward's whole device list at once
(`fauna_core::format::device_display_identities`, `value-formatting.md` § Device display identity,
which owns the set-relative widening and why a per-device chooser could not keep the distinctness
guarantee) — the device plane's own "actionable by `device_id` alone" degrade turned into a
positive render. The guardian-facing behavior and its rationale are owned by `family-safety.md`
§ Full visibility for young children; this entry owns only the plane mechanics: the seal stays
sealed, no key moves, and the scrubbed plaintext column is never re-plaintexted for any reader.
**`folders.include_paths`/`exclude_paths` — the ruling's self-declared sharpest field — also
seals now**, and it is the one member of the tightening set whose audience is **owner-only rather
than label-audience**. The nest already withholds the plaintext from a `role == "member"` row (the
owner's absolute local filesystem layout is not a member's business), so the sealed pair is withheld
from that projection too, and both lists seal under the **owner's** `convergent_chunk_root()` —
never a bound set's M2 generation, which every roster member holds. The funnel
(`fauna_core::label_custody::seal_include_paths`/`seal_exclude_paths`) therefore takes a
`BackupKey` rather than a `LabelRoot`, so the member-openable root is unrepresentable at a call site
instead of merely warned against; each list carries its own field domain tag, and both take a random
nonce (mutable under their salt). **The salt is `folders.id`** — nest-local, disclosing nothing,
and a non-`Option` field on every `FolderSummary` row, so this is the one sealed plane that needs
**no hash companion** on the wire to stay openable after the scrub.
The **writer is the user's selective-sync save** (`DevicesMachine::set_folder_paths` →
`fauna.folders.update`), not an engine catch-up stamp, and the reason binds any future
re-scoping: the salt is the row id the nest mints at INSERT, so a create request cannot carry the
seal even from a keyed client; and the plaintext lives *only* on the authoritative nest row (`file-sync.md` § Config
— every device pulls it, none holds a divergent local copy), so once the flip scrubs it there is
nothing for a stamp to re-derive from. The gesture that holds the lists at write time is the only
writer that survives the flip. Seals and plaintext **move together** nest-side: a write of either
list writes its seal, `None` included, so a keyless app's save *clears* the stale seal rather than
leaving one that opens to the list it replaced; a seal-only update stamps in place, which is the
shape the S8 backfill uses. The read seam (`render_include_paths`/`render_exclude_paths`, returning
`None` for the ratified omit degrade) was wired at **three** consumers (two since the daemon's removal,
2026-10-02). The `fauna-sync` daemon's
`apply_folder_row` was first — the consumer whose *behaviour* the flip would otherwise break,
where a silently empty filter list would make the daemon sync exactly what the user excluded. The
**app-side selective-sync editors** followed (2026-08-02) at `DevicesMachine::render_folders`,
which renders each row's pair at ingest before transcribing to the snapshot — so this is **one
shared-Rust seam serving all 7 apps**, not the per-app trickle-down it was first scoped as. That
required `DevicesNestApi::list_folders` to hand over **wire** rows, the same division
`list_devices`/`list_conflicts` already used: transcribing in the adapter dropped
`include_paths_sealed`/`exclude_paths_sealed` before any custody could open them.
The third is `libs/fauna-sync-engine`'s `resolve_device_mode_from_nest`, feeding every
always-resident per-app `SyncEngine`'s `install_selective_sync` — unlike the other two, this
consumer cannot rely on `sealed_present` to tell "never configured" apart from "a nest-side edit
just deleted the seal": both render `None` identically. It resolves the ambiguity by substituting
`include_paths`/`exclude_paths` **independently**, reapplying whatever the running `IgnoreMatcher`
is already armed with on a `None` for that one field rather than blanking it (2026-09-01, closing a
seal-deletion downgrade where a row edit that deleted only the seal
read identically to a row that never had selective-sync configured, and un-filtered every seat that
already had one armed).
⚠ **The editor half is not the cosmetic one, despite reading that way.** Because the save gesture is
the plane's only durable writer (paragraph above), a blank editor is a *destructive* render: a user
who opens selective-sync settings on a scrubbed row sees two empty lists and a save from that state
overwrites their real lists with nothing — and no engine pass can re-derive them. A consumer that
holds the owner key and never hands it to the render is the recurring failure of this whole flip
(the media-refresh and `fauna-sync restore` bugs of the same day); on this plane it costs data, not
just a name.
**`snapshots.tags` seals now too, as a display copy — and it is the field where the include/exclude
precedent one line above must NOT be copied.** Its audience is the **label audience**, graded from
where the field rests today rather than lifted: the nest ships the plaintext `tags` on
`fauna.filesync.snapshot.get` to the owner, to every roster member, *and* to a Q5 admin, and it
ships `retention_policy` to a member unmodified — so tags are not in the owner-only least-disclosure
class `include_paths` sits in, and sealing them owner-only would take them from a reader who has
them today. The ruling removes exactly one reader, the admin, and that is a **projection** question,
not a root one: `tags_sealed` and the set-name pair project on
`folder_authz::FolderReadGrant::is_label_audience()`, the same gate and the same value the
reply's per-file path pair already uses. The funnel
(`fauna_core::label_custody::seal_snapshot_tags`) therefore takes a `LabelRoot` — a bound set's M2
generation is a *legal* root here, which is precisely what `seal_include_paths`'s `BackupKey`
argument forbids for that field. Random nonce (the list is mutable and its salt does not determine
it), one `LabelField::SnapshotTags` domain tag; a per-tag `LabelField::SnapshotTag` variant was
**removed** — retention matching never needed it, being already hash-to-hash on
`tag_hashes`, and a seal is the wrong primitive for an equality test the nest must run without a key.
**The salt is the set's `name_hash`**, so no fifth digest shape is minted: it is already this set's
wire address (23 request types) and already rests in `folders.name_hash`. The reply carries it as
`folder_hash`, added to `SnapshotGetReply` alongside `folder_sealed` in the same change — the
`SnapshotDiffReply` twin, and without it this reply's set name had no carrier at all once the flip
scrubs `folder`.
The **writer is the create gesture** (`SnapshotsClient::create_folder`, now keyed at its
construction sites), and the reason binds any future re-scoping: `snapshots.tags` has exactly one
production writer (`create_snapshot_v2`, from the request's plaintext), the nest holds no key, and a
snapshot row — unlike a folder row — has no later bind/serve pass that revisits it, so there is no
catch-up stamp behind this and a create that does not seal loses its tags at the flip. The retention
pruner is deliberately untouched: it matches on `tag_hashes` and must keep doing so.

**The two tag planes, and which one is authoritative for what** (ruled 2026-08-01). A snapshot's
tags rest in two shapes that nothing can reconcile after the fact:
`tag_hashes`, nest-computed at INSERT and **authoritative for retention** — the pruner's whole
decision — and `tags_sealed`, the client-keyed **display copy**, which is what a user reads once
the flip scrubs the plaintext. The nest cannot verify one against the other, by construction: it
holds no key, and the seal carries a random nonce, so there is no equality test available to it.
The ruling is therefore **not** "the seal is advisory" — a plane a user reads to decide whether a
snapshot is protected must not be one the system disclaims — but **agreement by narrowing the
writers**, so no reachable state can diverge them dishonestly:

- Both planes derive from **one** tag list, at **one** moment: `create_snapshot_v2` hashes the
  request's tags into `tag_hashes` while the creating client seals that same list. The honest path
  agrees by construction, not by a check.
- Snapshots are immutable and neither plane has an editing gesture; `tags_sealed`'s only other
  writer is the S8 D3 stamp, which the nest now allows exactly once per row plus the one licensed
  owner-root → roster-root axis upgrade (above), as a compare-and-swap.
- **The first-stamp residual was REFUTED and RULED (2026-08-01)** — the
  earlier "accepted residual" paragraph here understated it on three counts, each proven by
  execution: the plant needs **no keys** (`SealedLabel.gen` is unauthenticated header metadata,
  and garbage ciphertext under `gen: Some(n)` reads as roster-rooted to the freeze predicate);
  the stamp freeze then binds the **owner permanently** (no owner arm existed, and snapshots
  are immutable — the only escape was deleting the snapshot); and the S9 scrub is
  provenance-blind, so a member's plant would license destroying the owner's resting tag
  plaintext — user-irrecoverable loss, not a display nuisance. Nor is the population shrinking:
  bidirectional compat admits sealless creates for the life of the major, so every old-client
  snapshot is a fresh target. **The ruling (fix (1)+(2) of the review's menu; full 7-app render
  provenance — its fix (3) — stays rejected as disproportionate):** **(1) the set owner may
  re-stamp unconditionally** — the stamp's freeze gains an owner arm; a member still cannot
  replace a generation-rooted seal, and the owner-root → roster-root axis upgrade stays
  member-reachable; **(2) tag seals gain nest-side stamp provenance** — additive
  `snapshots.tags_sealed_by` (the authed actor, recorded at a sealed create and at every stamp;
  never a wire field), and **at the S9 flip a resting tag seal survives only if attributable to
  the set owner or the snapshot's creator** — unattributable seals (non-creator member stamps,
  pre-provenance stamps, plants) clear together with the plaintext, so a plant achieves nothing
  an unsealed row doesn't already get, and the honest legacy stamps this disowns fall into the
  user-approved alpha clearing class. The D3 backfill pass narrows to match: a client stamps only
  rows its own actor owns or created. Remaining residual, stated plainly: a member can still be
  *first* to stamp garbage; during expand the render simply falls back to the resting plaintext,
  the owner can overwrite at any time, and the flip clears the plant — a nuisance with no durable
  effect. It still cannot change what retention obeys (`tag_hashes` has no writer after INSERT),
  cannot conjure tags onto a tag-less snapshot (the stamp refuses), and cannot reach a
  non-audience reader. **Implementation status: the live legs are CLOSED (S9 Phase 0,
  2026-08-01)** — schema v31 adds `snapshots.tags_sealed_by` (recorded at the sealed create and
  at every stamp), the stamp's freeze carries the owner arm, the `test-hooks` scrub mechanism is
  already provenance-aware on the tags plane (mutation-verified: reverting the owner arm alone
  reds `stamp_labels_owner_overwrites_a_member_plant` at its own assertion), and the daemon's D3
  pass skips `role == "member"` rows; the unattributable-seal *clearing* leg LANDED in the S9 flip's
  inventory transaction (v32, 2026-08-02): at the flip an unattributable resting seal clears
  seal + plaintext + provenance together, `tag_hashes` untouched — pinned by
  `a_pre_flip_image_boots_into_the_full_flip_reconcile`. **Both columns retired 2026-09-30 (schema
  101, `retire_snapshot_plaintext_tags`, the compat-remnant sweep):** with the flip's inventory
  gone at the genesis and every create resting `tags = NULL`, the plaintext `snapshots.tags`
  column had no writer, and the tags scrub it fed was `tags_sealed_by`'s one reader — both are
  dropped, with the scrub plane, the succession leg that re-pointed the provenance, and the
  pruner's and stamp's plaintext fallbacks. A snapshot's tags now rest as `tag_hashes` (the
  pruner's key) and `tags_sealed` (the display copy) alone; the owner arm of the stamp's freeze
  is unchanged, and the wire's plaintext `tags` fields stay (empty but for the create reply's
  echo) for compatibility.

Consequently no render surface needs to name its plane or caveat what it shows — the invariant the
UI relies on is that the tags a user reads are the tags the creator wrote.
**`folders.retention_policy` seals as of S6-e (2026-07-31), and its audience is the same
label-audience answer for the same reason `snapshots.tags` was** — the nest ships its plaintext to a
roster member unmodified (`member_summary`), unlike the owner-only `include_paths`/`exclude_paths`
three fields away on that struct, so the funnel takes a `LabelRoot` and the sealed sibling ships on
**both** projection arms ungated. The load-bearing fact behind "no admin arm" (corrected on
review): `fauna.admin.folders.get` **does** exist and is admin-only — what holds is that
`AdminFolderGetReply` carries no `retention_policy` field. **Forward-watch: if that reply ever
grows `retention_policy`, the ungated-both-arms decision must be re-taken.** Two mechanism facts bind any re-scoping. **The salt is the set's
`name_hash`, forced rather than chosen:** retention is settable at *create*, and `folders.id` is
minted at INSERT, so an id salt — the selective-sync pair's — could never be sealed by the create
gesture; `name_hash` is derivable before the row exists and already rides both arms beside the seal,
so the pair is never separated. **The writer is the post-create retention editor**, through the
shared `FoldersClient::update`, which mints the seal itself from the connection's custody so every
caller of that one method is a keyed writer; the create wizard stays keyless exactly as it is for
`name_sealed`, and the pair-moves-together rule (a plaintext save always writes the seal slot, so a
keyless save *clears* rather than strands a stale seal) applies unchanged. This is the slice that
also added the **one** schema bump the S6 aux fields needed (v30) — every other field in the
tightening set got its column in S1. The **UserConfig `backup` section** that the same carve-out
once called the real retention exposure was ruled a **dark rail** in the same pass and deliberately
not sealed; `../architecture/encryption-at-rest.md` § Carve-outs carries the corrected statement and
`backup-restore.md` § 8 the consequence for auto-pruning.
**Superseded for the share half (2026-10-01):** the passage below, written 2026-07-31, predates the share-link build — `share_tokens.filename` is no longer a dark rail with no producer or a declared (vacuous) gap: the seal writer was built 2026-09-28 and the seal made required 2026-10-01, the plaintext column left at schema 103, and the sealed name is the only form (owner: [`share-links.md`](share-links.md) § The filename rests sealed). Read every "share" clause below as history; the `import_sessions.source_descriptor` half stands as written.
The remaining two aux fields — `share_tokens.filename` and `import_sessions.source_descriptor` —
are **DISPOSITIONED BLOCKED-DARK (2026-07-31): deliberately not built, and the S6 sealing-writer
slice set is complete without them.** Their sealed columns exist from S1; both *surfaces* are dark
rails, verified at code level rather than assumed: `fauna.share.*` has no producer anywhere outside
the nest handler, kind registry and the nest's own conformance test, and the whole
mailbox-migration import surface (`fauna.bridges.*_import_session`, `import_message{,_batch}`) has
**no production caller at all** — no app, and not the Go MDA bridge either (its single reference is
an error-code doc comment), which sharpens the earlier "the MDA is the plausible keyless producer"
note: there is no producer, keyless or otherwise. Building either writer now would mint a carrier
no producer fills — the dark-rail class already ruled against on `FolderCreateRequest`'s sealed
pair and the UserConfig `backup` section. The binding rule instead: **whichever session lands the
first production caller of either surface owns landing it born-sealed**, against the already
ratified seal specs (share: salt = `token_id`, author root, random nonce; the recipient-serving
path stays URL-token-transient and out of scope. import: `source_hash` stays the nest-computed
lock/companion; `source_sealed` needs its producer to hold key material, else the row is an
S8-named keyless-writer gap like the Media delete/restore gestures were before S8 D2 closed
them). Until such a producer lands,
the paths-are-content ruling holds **vacuously** on both planes — no production row exists on any
deployment — and the S9 flip needed nothing from either.
Every built plaintext column now rests scrubbed by the S9 flip (EXECUTED 2026-08-02 as a
boot reconcile, retired at the 2026-09-24 genesis), per the `encryption-at-rest.md` per-kind
conformance rows (approval, deletion inventory, and prerequisites:
`../architecture/encryption-at-rest.md` § Carve-outs); the two dark-rail surfaces above are the
only planes still holding a declared (vacuous) gap, since no producer exists to flip. The
`folders.name` cutover leg, the last of them, landed 2026-10-02 (2026-10-01 survey A25): the name
rests sealed at schema 114 and travels by hash alone (§ the set-name plane, below). Its client half is one funnel, `fauna_protocol::folders::addressed` over the
protocol's `SetAddressed` trait: since 2026-10-02 every set-addressed request that has a
`name_hash` field — every `fauna.folders.*` request `FoldersClient` sends, and the
`fauna.sync.changes.{record,supersede,list}`, `fauna.sync.{status,files}` and
`fauna.filesync.snapshot.{create_folder,list,prune,prune_set_policy,check}` requests
`fauna-client-sync`, `fauna-client-snapshots` and the sync engine send, plus `fauna.stats.get`,
`fauna.files.versions.list` and `fauna.web.files.prune_sealed` — leaves the app with
`set_name_hash(name)` beside the name (a reserved `__` set, and an optional-set read naming no
set, stay unhashed). The mail bridge's WebDAV kinds
(`fauna.bridges.{webdav_list_files,webdav_record_change,mint_bulk_byte_token}`, built in Go) carry
it too: the MDA stamps `set_name_hash` through the `fauna-ffi` export `webdav_set_name_hash`,
never a Go re-derivation, and the nest resolves the served set hash-first. The batch's one
remainder is the admin `fauna.admin.folders.create`, whose set has no owner seal. **The nest's
storage half (2026-10-02):** every per-set mutator writes by the row id its handler resolved
(`update_folder_by_id`, `delete_folder_by_id`, `set_folder_mls_group_by_id`,
`set_folder_web_paywall_tier_by_id`), never by `(name, actor_id)`, and an empty name resolves
no set, so a blanked name can never address, or be written through, another blanked row.
**Outbound carriers (2026-10-02, first of them):** the remote-change nudge
(`fauna.sync.changed`) carries the set's `folder_hash` beside its name, every receiver matches
it through the one predicate `SyncChangedPayload::names_set` (the hash when present, else the
name; FFI `sync_changed_names_set`), and the apps relay the hash on the agent's `PullFolderNow`,
which resolves the binding whose name hashes to it — so a sealed set's nudge still reaches its
engine once its name is blank. linux and tui gate their expanded-row refresh through the
predicate; the apple and windows row gates (and apple's File Provider domain signal) still
compare the name. The chunk relay's folder hint carries the address too: the engine sends
`?folder_hash=<hex>` (`FOLDER_HASH_HINT_PARAM`) beside `?folder=`, the route resolves the hash
first (a malformed one is the unreadable-folder `404`, never a fall back to the name), and the
relay attributes a seat's answer to the hinted row's id, not its name. A bulk-byte token's
attribution key names a set as `folder:<row id>`, never by name, and `FileVersionInfo` gains
the set's `folder_hash` beside `folder`. The replies that show a set's name to its owner carry
the pair — `name_hash` and `name_sealed` — beside the plaintext: `BackupStatusEntry`
(`fauna.sync.backup_status`, rendered by the FFI `backup_status` through the connection's
custody), `DeviceFolderRole` (each place on a `fauna.sync.devices.list` row, rendered by the
devices machine with per-set custody found by the hash; a place the reader cannot open drops,
the device row stays) and the account export's folder entries (hex). `AdminFolderGetReply` and
`FolderShareReply` carry `name_hash` alone: an admin cannot open the seal, and the share reply
echoes a set its caller already named. A covered folder's coverage listing (`CoveredFolder`)
carries the pair beside its name; a client-device custodian records it per set beside the name
(`CustodianStore::put_folder_label`), the re-seed hands it on the delivered set
(`DeliveredSet::folder_label`), and the folder materialize names its target by the hash alone
(`CustodyMaterializeRequest::folder_name_hash`; a display name rides only for a set the store
holds no label for, and one riding beside a hash that is not its own is refused
`invalid_params`) and writes by the row id it resolved — so a restore finds a target whose
plaintext name is blank. Where the custodian's display name comes from once the listing
carries none is `../architecture/segment-backup-protocol.md` § *Where a restored folder's name
comes from*. The seal is salted by the name, never a row id, so it opens on whichever
nest the same owner restores to. The MDA's served-set listing (`WebdavServedSet`) carries each
set's `name_hash`: the MDA serves a set whose hash matches the requested name's, and names a
blank-named set in the root collection from its `WebdavKeysBlob` (a set the blob does not name
drops). A web-paywall grant names its set by hash too — the `content.read{folder}` tuple's `set`
qualifier is the hex of `set_name_hash` (owner `../architecture/mls-group-key-material.md` § M2,
the third distribution channel) — and the serve path matches it against the row's `name_hash`.

**At rest (schema 114, 2026-10-02):** `folders.name` is nullable, and a set whose `name_sealed` and `name_hash` both rest holds its name NULL — never a reserved `__` routing constant, and never a `public` folder, whose name is its URL segment (`../architecture/encryption-at-rest.md` § Carve-outs). One SQL guard decides the class (`migrations::folder_name_rests_sealed_sql!`) for all three writers of it: the create rests no plaintext beside a seal, every `update_folder_by_id` blanks the row in its own transaction (a seal stamped, a flip back from public), and the `folders.name` scrub plane catches what a crash left at the next boot. A table `CHECK` keeps a NULL-named row addressable (its hash and seal rest), and the custody-copy `CHECK` reads a NULL name as no reserved name. The nest reads a NULL name as the empty sentinel, which resolves no set. A →`public` flip of a set resting no plaintext must carry its `name`, which must hash to the row's `name_hash`: the nest restores it as the URL segment, and refuses the flip otherwise. **The admin create rests its plaintext until the owner seals it:** `fauna.admin.folders.create` has no owner key, so its set is born unsealed and keeps its name until the owner's app stamps `name_sealed` (the seal-backfill sweep every app runs at session start does so), and that write blanks it — refusing the admin create was the alternative, rejected because the admin names the folder for the user and the backfill closes the window without a second flow. **On the wire (2026-10-02):** the app funnel (`fauna_protocol::folders::addressed`) takes the plaintext name off every set-addressed request once it has stamped the hash — the request's `name`/`folder` is omitted on the wire, and the nest resolves by `name_hash` alone (`fauna.stats.get` included, whose hash alone selects the per-folder arm). A reserved `__` set still travels by its literal name, and a name rides only where the nest must rest it (`SetAddressed::keeps_set_name`): an unsealed create (a custody-less client), a `public` create, and a `fauna.folders.update` that turns the set `public`, whose name is restored as the URL segment. **A sealed create travels by hash alone:** `fauna.folders.create` with no `name` mints the row from `name_hash` + `name_sealed`, resting NULL from its first write; the nest refuses it without a seal, with a malformed hash, or for a `public` set, and a duplicate is the usual `UNIQUE(name_hash, actor_id)` conflict (`CacheDb::create_sealed_folder_by_hash`). The WebDAV MDA's three kinds (`webdav_list_files`, `webdav_record_change`, the bulk-byte mint) carry `name_hash` and no `folder` at all. **A path list rests only sealed:** `fauna.folders.update` refuses a non-empty `include_paths`/`exclude_paths` without its seal (an empty list names no path and still clears). **Two by-name arms stay, deliberately:** each hash-first handler still resolves a hash-less request by name, and `CustodyMaterializeRequest.folder_display_name` still rides for a label-less set — both can only reach a set whose name already rests plaintext (an unsealed, reserved or `public` one), since an empty or NULL name names no set, so refusing them would seal nothing more.

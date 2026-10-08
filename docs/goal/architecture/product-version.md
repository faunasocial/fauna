# Product version — one fleet-wide string

Owns: product-version
Status: ratified
Authority: owns what the product version IS (one fleet-wide `major.minor.patch` string), its source of truth (`Cargo.toml [workspace.package] version`), the lockstep-surface contract and its `version-lockstep-check` gate, the bump and reship rules, and the never-reuse invariant. What major/minor/patch *mean* for compatibility → [`version-compatibility.md`](version-compatibility.md) (I2/I3); each store's version-field *format* → that store's [`installers/`](installers/) doc; nest image tags & channels → [`build-system.md`](build-system.md) § Image tags & channels.

Last verified: 2026-10-07 (reship scope ruled: a refusal re-uploads only the refusing store) | Source: `Cargo.toml`, the version-lockstep checker, the per-store packaging files it enumerates

## The model (ratified 2026-08-23)

**There is exactly ONE product version — a `major.minor.patch` string naming a
source snapshot of the whole tree — and it is the same string on every surface:
all 7 apps' About rows, every store listing, the nest's advertised version, the
production image tag.** It is never per-app and never per-store. A user asking
"what version are you on?" gets one answer regardless of platform (priority #1),
and no two stores can ever disagree about what a given string means.

### Source of truth: `Cargo.toml [workspace.package] version`

Every workspace crate inherits it (`version.workspace = true`), which is why it
was already the de facto source with four live consumer classes before this doc
existed:

- **In-app**: `env!("CARGO_PKG_VERSION")` renders the About rows (linux, tui),
  seeds the update-checker baseline (`libs/fauna-core/src/version.rs`), and is
  exported to the non-Rust apps as `fauna_ffi_build_version()`
  (`libs/fauna-ffi/src/version.rs` — windows' C# assembly version is
  MSBuild-derived and deliberately NOT a product-version surface; C# compares
  through this export instead).
- **On the wire**: the nest advertises it (`libs/fauna-protocol/src/discovery.rs`;
  unauthenticated callers get MAJOR.MINOR only — a deliberate
  information-disclosure trim, owner `nest/network-exposure.md`).
- **Release pipeline**: `build-nest-image.yml` / `restage-nest.yml` grep it;
  the production image's `:<version>` is the public repository's package,
  tagged by its own pipeline since the 2026-10-02 channel map (tag semantics
  → [`build-system.md`](build-system.md) § Image tags & channels).
- **Installers**: the macOS `.pkg` build extracts it
  (`installer/macos/build.sh`).

The version grammar is plain `MAJOR.MINOR.PATCH` digits — no pre-release or
build-metadata suffixes (they would break the Android `versionCode` arithmetic
and several store fields; the gate refuses them).

### Lockstep surfaces — committed copies, gate-enforced

Formats cargo cannot reach hold hand-edited committed copies that MUST equal the
source. The cheap merge gate **`version-lockstep-check`**
(a dedicated dev-fleet checker) refuses any merge where a surface drifts —
its check functions are the machine-readable enumeration of the surfaces. Today:
Android `versionName`/`versionCode` (arithmetic owned by
[`installers/android.md`](installers/android.md) § Versioning), the two Apple
`Info.plist` files plus every pbxproj `MARKETING_VERSION`
([`installers/macos.md`](installers/macos.md) § Versioning), windows
`Package.wxs` `Version` plus the `ShellDllName` coupling
([`installers/windows.md`](installers/windows.md) § Shell Extension), the snap
`version:`, and web's `package.json`.

**Committed-not-derived is deliberate**: [`installers/android.md`](installers/android.md)
§ Versioning already ratified the reasoning (F-Droid builds from source in its
own environment; a build-time derivation is a second mechanism), and a gate that
refuses the merge makes hand-editing safe rather than hopeful. Converting any
one surface to build-time derivation *from the same source file* later is a
compatible refactor, not a model change.

### Release-history records are a different class

The AppStream metainfo `<releases>` list records *shipped* releases: entries
are appended at release time, and the newest entry may **trail** the tree
version but never lead it (gate-checked). They are histories, not lockstep
copies.

### Bumping — one commit, all surfaces; a release is a train

A version bump is a single commit editing the source and every lockstep surface
(the gate refuses anything less). Patch = fixes **and store-mechanical
reships**; minor = features; major = a compatibility break under
[`version-compatibility.md`](version-compatibility.md) I3 — that doc owns what
the components promise (minor/patch always compatible, major may break, with a
transition window and no data loss); the version its invariants speak of is
this product version, which every binary advertises. Pre-1.0 alpha reads the
same contract at `0.x`.

**A release is a train: the nest and all seven apps ship simultaneously at one
version (ratified 2026-08-26, user-confirmed).** A bump allocates the train's
number; cutting the release ships that number **everywhere** — the nest image
promoted to `:<version>`, every app's channel that exists — so a version
string names one snapshot *and* one shipping event, and a user on any platform
who reads "0.1.3" is on the same release as a user on any other. The one
exception is the reship below — a store's re-upload, of unchanged code or of a
fix that store demanded: it ships **one** app at the next patch, every other
store keeps the upload it already holds, and the feature catalog's release
table records the per-app shipped version so that gap is visible rather than
inferred
([`feature-catalog.md`](feature-catalog.md) § Tag gate and release table).
Pre-1.0, "every app's channel that exists" is the operative phrase — a channel
that has never shipped (§ Current values) is not held back by the rule and does
not hold the train back; the catalog is the honest label on what shipped, not
a green-everywhere precondition. *This replaces the 2026-08-23 "a bump does
not oblige any app to ship it" rule: sparse per-store histories are no longer
expected — a store skipping a number now means a reship carve-out, and the
table says so.*

### Reships: a store re-upload mints a NEW product patch

When a store demands a fresh version for a re-upload of unchanged product code
(Play rejects `versionCode` reuse; MSI major-upgrade logic wants a higher
`Package/@Version`; App Store Connect wants a new build), the answer is always
the same, on every store: **bump the product patch version fleet-wide (one
commit), rebuild the affected app from that commit, ship it from that store
only.** This generalizes the rule android.md first ratified for Play and what
windows' `0.1.1` did in practice.

**The same holds when the refusal demands a code fix (user-ruled 2026-10-07)** —
a store floor such as Play's API-36 target, a certification failure, a review
rejection: the fix and the patch bump land on `main`, the next public commit
carries both, and **only the refusing store ships the new number** from it.
**A store that already holds an accepted upload of the old number keeps it and
uploads nothing** — not as a courtesy, not to keep the stores level; its
upload is a valid snapshot under its own string, and the release table shows
the gap. A refusal therefore costs exactly one extra upload, the refusing
store's, never a round of re-uploads across the stores that were fine. (A
store that has not uploaded the train at all is not re-uploading: it ships the
newest number, from that same new commit — § Version strings are never reused
or re-meant, the train corollary.) Consequences:

- Each product version has exactly **one** Android `versionCode` — the
  arithmetic needs no reship digit.
- Apple's `CFBundleVersion` / `CURRENT_PROJECT_VERSION` are **pinned at `1`**
  forever: each version train gets exactly one upload, so the build number
  never moves.
- The windows MSIX/MSI version is the product version verbatim (4th MSIX digit
  `0`), never an encoding.

### Version strings are never reused or re-meant

Once any artifact has shipped under a version string, that string names that
snapshot **forever** — it is never reassigned to different source, and the tree
version never moves backwards past a shipped number. This is the invariant that
makes "one string everywhere" safe: the failure this doc exists to prevent is
two stores silently disagreeing about what `0.1.1` means.

**An upload spends the string (ruled 2026-10-07; it supersedes a same-day ruling that a never-served Play draft could be withdrawn to free the string).** A version string is spent the moment any store has ACCEPTED an upload carrying it — rolled out or not: a Play draft release and a TestFlight build that never reached a tester count. The project never relies on taking an upload back. Google documents only that a used `versionCode` cannot be uploaded again; whether deleting a never-rolled-out bundle from the App bundle explorer frees it is undocumented; App Store Connect refuses a repeated build number outright (`CFBundleVersion` is pinned at `1`, § Reships); and a rule whose effect shows only at the next real upload is no rule. So a refused rollout, a withdrawn upload or a store floor hit after the upload is an ordinary reship (§ Reships): bump the product patch fleet-wide in one commit and ship the new number from the refusing store, and leave the spent upload at the store, untouched, as the record of what was uploaded. Precedent: `0.1.2`/`102`, built from the public first-push commit and uploaded to a Play internal-testing draft on 2026-10-07, was refused rollout under Play's API-36 target floor ([`installers/android.md`](installers/android.md) § Versioning); `0.1.2` is spent unshipped — the draft stays in the Console, nobody deletes it, no store ever uploads `0.1.2` — and the fleet bumped to `0.1.3`/`103` the same day.

**Corollary for the train: the commit a version string names is fixed by the first upload any store accepts.** `0.1.2` names the public first-push commit for good although nothing shipped it, and every later store that uploads `0.1.3` builds the one commit `0.1.3` names. **When a store refuses, one rule decides who uploads what, and it never re-uploads a store that was fine:** the fleet bumps the patch in one commit (§ Reships), the next public commit carries the bump and whatever the refusal demanded, and from it ship (a) the refusing store and (b) every store that has **not yet uploaded** the train — each of those uploads once, at the new number. **A store that already holds an accepted upload of the refused number keeps it and uploads nothing.** So when the refused upload was the train's first (Play's `0.1.2` on 2026-10-07), every store moves to the next number, but only because none of them had uploaded `0.1.2` — not because a refusal resets the stores that were fine. Worst case, three stores refusing one after another, is three uploads plus one per refusal ([`release-integrity.md`](release-integrity.md) § Release signing → *A store upload is built from a recorded public commit*).

## The decision record (2026-08-23)

The fleet had drifted into four hand-maintained values and no owner
. Options
considered:

- **(B) shared `major.minor`, per-store patch** — REJECTED: one string acquires
  N per-store meanings; the About row (a user-visible, cross-app surface)
  diverges per platform, which is precisely the divergence priority #1 forbids;
  "what version are you on?" needs a per-store answer in support contexts.
- **(C) per-store reship counters encoded into store-native slots**
  (Android reship digit in `versionCode`, Apple build numbers, a windows
  `patch*100+reship` build field) — REJECTED: three new per-store mechanisms
  and arithmetic encodings, a Microsoft Store listing that permanently displays
  an encoding different from the product version, and a far more complex gate —
  all purchased to avoid an occasional one-commit patch bump.
- **(A) one string everywhere** — RATIFIED, with the refinement that defuses
  its textbook cost: a reship bumps the fleet *number*, not the fleet's
  *artifacts* — nobody re-uploads anything except the store that demanded it.

## Current values

All lockstep surfaces: **`0.1.3`** (bumped 2026-10-07 under § Version
strings are never reused or re-meant → *An upload spends the string*: the
ratifying commit's `0.1.2` was uploaded to a Play draft from the public
first-push commit on 2026-10-07 and refused rollout under Play's API-36
target floor, so it is spent unshipped). Why `0.1.2` and not
`0.1.0`/`0.1.1` before that: windows shipped `0.1.1` on 2026-07-17 (a
store-mechanical reship of the snapshot, pre-model), so `0.1.1` is burned —
reusing it would give one string two meanings, and moving windows back down
to `0.1.0` would break MSI upgrade monotonicity. `0.1.3` is the first string
no store has accepted an upload of. Shipped state per store: Microsoft
Store `0.1.1`; Play holds **no release at all** — the one draft upload,
`0.1.2`/`102`, is left in the Console as the record, and the next Play
upload is `0.1.3`/`103` from the public commit carrying the bump and the
targetSdk-36 change (see [`installers/android.md`](installers/android.md)
§ Implementation status today; an earlier revision of this section claimed a
pre-model `1.0.0`/`versionCode 1` upload, corrected 2026-08-23); Apple and the
linux channels have shipped nothing.

## Implementation status today

- **Fully implemented as of the ratifying commit**: source and every lockstep
  surface agree at `0.1.2`; `version-lockstep-check` (+ its `-test` companion)
  runs in the cheap tier on both merge scripts.
- The metainfo's `<releases>` list is empty — nothing has shipped; the first
  entry is appended when the linux channels first ship (an empty history
  passes the trail-never-lead gate).
- **The release train (2026-08-26) is a release-process rule with no code
  surface of its own yet**: its record — the release table's per-app shipped
  version — lands with the feature catalog's release recipes
  ([`feature-catalog.md`](feature-catalog.md) § Implementation status today,
  step 4). Until then the rule binds the maintainer cutting a release, not a
  gate.
- **The About-row surface is not on all 7 apps yet (checked 2026-09-23)**:
  only tui (`apps/fauna-tui/src/settings/about.rs`, the Settings root's
  `settings-app-version` — painted 2026-09-26; the 2026-09-23 check had named
  a tui `general.rs` that never existed) and linux
  (`apps/fauna-linux/src/settings/general.rs`) render the product version,
  from `env!("CARGO_PKG_VERSION")`, and macOS and windows (Settings → General's
  About block, both 2026-10-08) from `fauna_ffi_build_version()`. Web's Settings "Build" section shows a
  git SHA (`VITE_GIT_SHA`), not the product version, and nothing displays its
  lockstep `package.json` copy. iOS and android show no app version
  anywhere, although each could read `fauna_ffi_build_version()` or its own
  lockstep copy. The dashboard "Version" stat on web and Apple is the nest's
  version, not the app's. The row's ui.yaml id is `settings-app-version`,
  painted on tui, on linux's General page (2026-10-05) and on macOS's and
  windows' (2026-10-08).

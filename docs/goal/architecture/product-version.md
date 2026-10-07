# Product version — one fleet-wide string

Owns: product-version
Status: ratified
Authority: owns what the product version IS (one fleet-wide `major.minor.patch` string), its source of truth (`Cargo.toml [workspace.package] version`), the lockstep-surface contract and its `version-lockstep-check` gate, the bump and reship rules, and the never-reuse invariant. What major/minor/patch *mean* for compatibility → [`version-compatibility.md`](version-compatibility.md) (I2/I3); each store's version-field *format* → that store's [`installers/`](installers/) doc; nest image tags & channels → [`build-system.md`](build-system.md) § Image tags & channels.

Last verified: 2026-08-26 (release train ratified) | Source: `Cargo.toml`, the version-lockstep checker, the per-store packaging files it enumerates

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

The AppStream metainfo `<releases>` list (and the Sparkle appcast, once live —
[`installers/macos.md`](installers/macos.md)) record *shipped* releases: entries
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
exception is the store-mechanical reship below: it ships **one** app at the
next patch, and the feature catalog's release table records the per-app
shipped version so that gap is visible rather than inferred
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
windows' `0.1.1` did in practice. Consequences:

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

**"Shipped" means SERVED, not uploaded (ruled 2026-10-07).** A version string is spent for a store once that store has served an artifact carrying it to anyone — a rollout to any track, Play internal testing or a TestFlight group included — or once the store refuses to take the string again, whichever comes first. An upload the store never served and lets the project take back (Play: discard the draft release, then delete the bundle from the App bundle explorer — a *draft* in Play's own release states is "not served to users yet") is withdrawn at the store and leaves the string and its derived `versionCode` **unspent**: the next upload carries the same version from a newer public commit. The reship rule (§ Reships) fires only if the store still demands a fresh number after the withdrawal — Play refusing the `versionCode` on the re-upload is that demand — and the answer is then the ordinary one-commit patch bump, never a second draft. Apple is the opposite case by construction: `CFBundleVersion` is pinned at `1` (§ Reships), and App Store Connect refuses a second build with the same version and build number whether or not the first was ever served, so an upload there spends the string at upload. Microsoft Store is unruled until a submission is first withdrawn. Precedent: Play's draft of `0.1.2`/`102` from the public first-push commit, uploaded 2026-10-07 and refused rollout under Play's API-36 target floor ([`installers/android.md`](installers/android.md) § Versioning), is withdrawn, and the next Play upload is `0.1.2`/`102` again, from the public commit carrying the targetSdk bump.

**Corollary for the train: the commit a version string names is fixed by the first artifact a store SERVES.** Until then every public commit at that version is the same unshipped train: each store builds the newest public commit at the version when it builds, and the first store to serve fixes that commit for the rest — a store that built an earlier unserved commit rebuilds from the fixed one ([`release-integrity.md`](release-integrity.md) § Release signing → *A store upload is built from a recorded public commit*).

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

All lockstep surfaces: **`0.1.2`** (set in the ratifying commit). Why `0.1.2`
and not `0.1.0`/`0.1.1`: windows shipped `0.1.1` on 2026-07-17 (a
store-mechanical reship of the snapshot, pre-model), so `0.1.1` is burned —
reusing it for today's tree would give one string two meanings, and moving
windows back down to `0.1.0` would break MSI upgrade monotonicity. `0.1.2` is
the first string no store has ever seen. Shipped state per store: Microsoft
Store `0.1.1`; Play holds **no release at all** — one draft upload of
`0.1.2`/`102` from the public first-push commit on 2026-10-07 was refused
rollout under Play's API-36 target floor and is withdrawn under § Version
strings are never reused or re-meant → *"Shipped" means served*, so
`0.1.2`/`102` is unspent and is the next Play upload, from the public commit
carrying the targetSdk bump (see [`installers/android.md`](installers/android.md)
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
- The Sparkle appcast is not live (`installers/macos.md` § Implementation
  status today); when it goes live its entries fall under the release-history
  class here.
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
  from `env!("CARGO_PKG_VERSION")`. Web's Settings "Build" section shows a
  git SHA (`VITE_GIT_SHA`), not the product version, and nothing displays its
  lockstep `package.json` copy. Windows' About section shows a name and a
  description but no version, and macOS, iOS and android show no app version
  anywhere, although each could read `fauna_ffi_build_version()` or its own
  lockstep copy. The dashboard "Version" stat on web and Apple is the nest's
  version, not the app's. The row's ui.yaml id is `settings-app-version`,
  painted on tui and on linux's General page (2026-10-05).

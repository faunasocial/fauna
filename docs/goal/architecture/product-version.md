# Product version — one fleet-wide string

Owns: product-version
Status: ratified
Authority: owns what the product version IS (one fleet-wide `major.minor.patch` string), its source of truth (`Cargo.toml [workspace.package] version`), the lockstep-surface contract and its `version-lockstep-check` gate, the bump and reship rules (the weekly cadence among them), and the never-reuse invariant. What major/minor/patch *mean* for compatibility → [`version-compatibility.md`](version-compatibility.md) (I2/I3); each store's version-field *format* → that store's [`installers/`](installers/) doc; nest image tags & channels → [`image-pipeline.md`](image-pipeline.md) § Image tags & channels.

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
  → [`image-pipeline.md`](image-pipeline.md) § Image tags & channels).
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
[`installers/android.md`](installers/android.md) § Versioning), the three Apple
`Info.plist` files (macOS, iOS, and the Fauna Kids iOS app's
`Info-Kids.plist`) plus every pbxproj `MARKETING_VERSION`
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
the public pipeline promotes to `:<version>`, every app's channel that exists — so a version
string names one snapshot *and* one shipping event, and a user on any platform
who reads "0.1.3" is on the same release as a user on any other. The one
exception is the reship below — a store's re-upload, of unchanged code or of a
fix that store demanded: it ships **one** app at the next patch, every other
store keeps the upload it already holds, and the feature catalog's release
table records the per-app shipped version so that gap is visible rather than
inferred
([`release-table.md`](release-table.md) § Tag gate and release table).
Pre-1.0, "every app's channel that exists" is the operative phrase — a channel
that has never shipped (§ Current values) is not held back by the rule and does
not hold the train back; the catalog is the honest label on what shipped, not
a green-everywhere precondition. *This replaces the 2026-08-23 "a bump does
not oblige any app to ship it" rule: sparse per-store histories are no longer
expected — a store skipping a number now means a reship carve-out, and the
table says so.*

**A train leaves every week and carries what has landed (owner-ruled 2026-11-07: "make sure there is a release each week … incremental by default, so 0.1.7 next").** Each train is the next PATCH of the last published version, cut from `origin/main`'s tip when its attempt opens, and its content is whatever has landed by then: a feature catches the train that leaves after it lands, and no theme holds a cut. The themed trains planned on 2026-10-08 are re-read under this rule — 0.1.6, the everyday train, finishes the attempt already in flight as planned; 0.1.7 is the first weekly train, and its themed scope (the admin, mail and accounts features) moves off the version task onto an objective of its own (a `rows` set or the feature-gap driver's goal, [`fleet-scheduler.md`](fleet-scheduler.md) § Objectives), still pursued and never a prerequisite of a version. A version task carries only what the cut itself waits for: `AFTER` the previous version task and the attempt's own mechanics tasks (the rehearsals, the retips). A MINOR bump is the owner's word, never the cadence's; a reship stays a patch as above, and spends the next number as the never-reuse rule says — the weekly train after a reship is the patch after it. What keeps a version task open, rolls to the next and reads the week's `due` is the cadence objective `release every 7d` ([`fleet-scheduler.md`](fleet-scheduler.md) § Objectives); what the owner still does per train — the public text's go, the Environment approvals — is [`release-attempt.md`](release-attempt.md) § The release attempt. The owner's measure at the ruling: a release costs 5–10 days of fleet work, so trains overlap, and the first weekly train is a test of whether the attempt's cost comes down to the week.

**Every weekly train opens on Monday at 05:00 Europe/Oslo and is out within 24 hours (owner-set 2026-11-12, for 0.1.7 and every train after it).** The clock starts at the attempt's `opened` stamp and stops when all four hold: the version's GitHub Release is flipped from the pre-release, `:latest` serves its nest image, fauna.social is deployed at the version with its read-back passed, and every direct download of the release resolves. The store apps are submitted inside the same 24 hours; when each becomes downloadable is the store reviewers' time, measured on its own line and never left out. There is no "if nothing goes red" (the owner: "Nothing goes red here, we make things red … We do everything, and are accountable. Nobody to blame."): a red is the fleet's own work, fixed inside the version with the clock running, and the owner's own actions are on the clock too. The attempt opens on one commit already proven green and stays on it; whatever mechanism the deadline rests on is built before the open, off the clock. The owner's part is two sittings: the public text, presented verbatim at the open (the present-then-stop rule of the public commit message is unchanged, and the steps that publish nothing go on meanwhile); and ONE message at the tag asking the three Environment approvals together (`release`, `release-macos`, `promote-nest`), possible because the acceptance set is green before the tag. The owner accepted what the one message costs for now: `:latest` moves about 4.5 hours before the new apps are downloadable, and inside 0.1.x a nest that updates in that window may be ahead of its apps; with no outside installation this is accepted, it is decided again before any outside user exists, and it is never carried past 0.2.0 without the owner's word. Every version task carries the deadline, and its close owes a review against it, written in the task and told to the owner in one screen: the open stamp, the stamp of each of the four conditions, the total against 24 hours, each store's submit and available stamps, every red with what we did to cause it and the hours it cost, and each wait on the owner with its length. The figure rests on 0.1.6's measured stages on a commit that did not move: about 17 to 18 hours of work, the rest for fixing what we break. The weekday is the owner's word of the same day ("0.1.7 and later releases start on Mondays 05:00"), which moved a first Thursday ruling: 0.1.7 opens on Monday 2026-11-16 at 05:00; until then the first priority is 0.1.6 downloadable on every platform, the stores being what is missing.

**A release reaches the three stores by itself (owner-ruled 2026-11-12: "The release should be to both iOS, Play and Microsoft stores automatically").** Every release uploads its build to the App Store, Google Play and the Microsoft Store as steps of the release itself: no hand-run recipe, no console visit for an upload, no session remembering to do it. Each store leg builds the store's upload from the version's recorded public commit, uploads it with the store's own tool or documented API, puts it on the store's testing track, submits it to the store's review, and records each state it reaches (`built`, `live-test`, `review`, `live`; [`fleet-versions.md`](fleet-versions.md) § The apps of a release) from the store's own reading. A store leg's red is a red of the release, fixed inside the version. What a store's own terms keep for a human (a legal attestation, a questionnaire answered by the organisation's representative) is named exactly and asked once, never left as a standing visit. The design (where each leg runs, each credential and where it rests, what the owner's one approval message covers) is owned by [`installers/store-review.md`](installers/store-review.md) § The store legs (designed 2026-11-12); until the legs are built § Implementation status today says what is done by hand.

**A release is never given up (owner-ruled 2026-11-12: "A release is never given up. We simply don't quit.").** An attempt that has opened ends one way only: its version promoted to `:latest` and its GitHub Release flipped to the release. A red at any step, the promote's acceptance set included, is fixed and the attempt goes on at the same version: no session closes an attempt as "not promoted", rolls its fixes into the next number as a way out, or records a give-up of its own ([`fleet-versions.md`](fleet-versions.md) § The apps of a release: a give-up is the owner's word alone). The next version's attempt does not open while the one before it is unpromoted. Two consequences for the sequence. First, the acceptance set is read green on the commit BEFORE any step that spends the string (the public tag, a store's accepted upload; § Version strings are never reused or re-meant), so a fix moves the tip inside the version and never needs a new number. Second, a defect found after the string is spent goes to the owner at once, with the ways left to finish that version (promoting the commit as built, the defect listed as a known issue of the release, is one); it is never settled by a session's own decision to stop. The 0.1.5 ruling (2026-10-23) stands as that version's history and is no longer a rule a session may apply: 0.1.6, closed "not promoted" on 2026-11-11 under it, was reopened under this one and promoted and released on 2026-11-12 with the owner's accepted known issues (§ Current values; § Implementation status today for what the tooling does not yet do).

**The public website is a surface of the train: fauna.social deploys at every version's sync, and the version's public tag refuses without it (ruled by the owner 2026-10-16).** The deploy builds the private commit the sync's `Sync-Point:` names — the commit the public tree was made from — so the site's "what works where" page and the public repository's catalog are one commit's rendering, and it runs once that sync's squash-merge lands on public `main`, before the tag and the promote (which follow hours or days later and would leave the site behind the repository it describes). It stays an act of the version's attempt, taken by the release session after the version's one go, never a push trigger: a merge to private `main` never publishes the site by itself. The tag gate reads a green deploy at the internal tag's commit live, as it reads the release Environments ([`release-table.md`](release-table.md) § Tag gate and release table); a deploy that would publish a site text still in draft is refused ([`public-texts.md`](public-texts.md) § Classes). **Amended by the owner 2026-10-24: the deploy moves to the end of the release.** fauna.social/apps links each released file at its version, so a deploy before the release files exist links files that are not there yet. The deploy therefore runs once the version's release workflows have published their files at the tag; it does not wait for the promote or the release flip, which can follow days later. The tag gate reads a green site *build* at the internal tag's commit instead of a deploy, which still refuses a site that does not build before anything is tagged. For the hours between the release's files and the deploy, the site still describes the previous version; the owner prefers that to dead links. The draft refusal is unchanged.

**Iron-clad: https://fauna.social never has a dead link, and a deploy that leaves one is fixed at once (owner-ruled 2026-11-11).** The owner's words, after 0.1.6's deploy put four 404 download links live: "https://fauna.social must never have dead links. If you deploy it with broken links, you fix it immediately." and "make this iron-clad (auto-approve fixes in the future too)". So: every deploy is read back live — every page the sitemap lists and every link the pages print, release downloads included, must answer — and a deploy is not done until that read-back passes. A failing read-back is fixed in the same session, at once, by whatever deploys a site with no dead link (a fix landed on `main` and deployed with `deploy-site <version> --from <that commit>`, even when that renders `main`'s pages ahead of the public tree for a while; a redeploy of an earlier good commit; a cell taken out of `installer/downloads.json`), and that fix needs no go: the owner's ruling above is its standing approval. The one-commit rule earlier in this section yields to it for as long as the fix needs. The read-back is a mechanism, not a hand step: the deploy workflow's `verify` job runs the live-site verifier after every deploy and reds the run when it fails — every page reachable from the root, every page the sitemap lists and every live site text the public-texts inventory records answers; every link the pages print answers (an external redirect is an answer); `/apps` links no release but this version's and carries every file `installer/downloads.json` names; and the build marker the deploy writes (`/build.json`, the commit it was built from) names the deployed commit, polled while the CDN catches up. So a green deploy run is a verified live site. **A non-critical issue the read-back finds** — anything short of a dead link: a page missing from the sitemap, a redirect hop, a slow or oversized page, a broken image alt, stale copy — **is not fixed in the release session: it is filed as work for the website's own queue to consider** (owner-directed 2026-11-11: "If you find non-critical issues for the website then please mint rows for … to consider fixing").

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

All lockstep surfaces: **`0.1.7`** (bumped 2026-11-12: the first weekly
train, carrying whatever had landed on `main` when its attempt opened that
evening on the owner's word; the first version whose tag runs the three store
legs). Before that,
**`0.1.6`** (bumped 2026-11-05: the everyday
train — feed, posts, conversations, contacts, notifications, folders and
sharing at parity on every app; Android push moved to `0.1.7`; no store has accepted an upload of it yet; its public tag
`v0.1.6` was pushed on 2026-11-11, and on 2026-11-12 it became the first version promoted and released: `:latest` serves its image and its GitHub Release is flipped from the pre-release. Its acceptance set read red on two pieces (two tui regression cells; three live mail tests after a factory reset, one cause), each fixed on `main` after the tag; the owner accepted them as known issues of the release under § Bumping, *A release is never given up*, undoing the 2026-11-11 close as not promoted, [`release-table.md`](release-table.md)). Before that,
**`0.1.5`** (bumped 2026-10-18: the first tagged
train, cut from a `main` tip on the release-attempt shape and carrying
everything landed since `0.1.4`; Android push is deferred to `0.1.6`; no store has accepted an upload of it
yet; its public tag `v0.1.5` was pushed on 2026-10-22 and its GitHub Release
stays a pre-release — owner-ruled 2026-10-23, 0.1.5 is not promoted). Before that, **`0.1.4`** (bumped 2026-10-11: the parity train,
which catches the apps up on most of what `0.1.3` shipped on only some; no store has accepted an upload of it
yet. **Its public commit is the 2026-10-17 squash-merge of its sync, and the public image `:test` + `:0.1.4` is staged and signed, not promoted. Owner-ruled 2026-10-17, taking the recommendation: 0.1.4 takes NO `v0.1.4` tag and no installer release — it stays published-and-staged, because its source is not on `main` (the retired base-pin-plus-cherry-picks shape, [`release-attempt.md`](release-attempt.md) § The release attempt), it carries RUSTSEC-2026-0367..0369 against `zip`, and nothing can promote it while the organisation's nests are firewalled; `0.1.5` is the first tagged train, cut from `main` on the attempt shape.**) Before that, **`0.1.3`** (bumped 2026-10-07 under § Version
strings are never reused or re-meant → *An upload spends the string*: the
ratifying commit's `0.1.2` was uploaded to a Play draft from the public
first-push commit on 2026-10-07 and refused rollout under Play's API-36
target floor, so it is spent unshipped). Why `0.1.2` and not
`0.1.0`/`0.1.1` before that: windows shipped `0.1.1` on 2026-07-17 (a
store-mechanical reship of the snapshot, pre-model), so `0.1.1` is burned —
reusing it would give one string two meanings, and moving windows back down
to `0.1.0` would break MSI upgrade monotonicity. `0.1.3` is the first string
no store had accepted an upload of. Shipped state per store (the live record is
[`installers/store-review.md`](installers/store-review.md) § The record): the
Microsoft Store has no submission — the `0.1.1` above was spent by the
2026-07-17 Windows build and reached no store
([`installers/windows.md`](installers/windows.md) § Implementation status
(Store channel)); Play serves **`0.1.3`/`103` on internal testing since
2026-10-09**, built from the public commit that published `0.1.3` — the `0.1.3` train's commit — while the earlier draft upload `0.1.2`/`102` stays in the Console
as the record (see [`installers/android.md`](installers/android.md)
§ Implementation status today; an earlier revision of this section claimed a
pre-model `1.0.0`/`versionCode 1` upload, corrected 2026-08-23); Apple and the
linux channels have shipped nothing.

## Implementation status today

- **The Monday 05:00 open and the 24-hour clock (set 2026-11-12) are kept by tooling as of 2026-11-12, with one gap.** Built: the version task the janitor's objectives sweep mints carries a typed `not before` of the first Monday 05:00 Europe/Oslo after the previous train's close and after the sweep's own reading, and its body names this paragraph, the open, the deadline, the four conditions, the store line and the review; `fauna-publish attempt review <version>` prints the review from the attempt record and the release table; and `task-close` refuses a landed close of a version task from 0.1.7 on whose block lacks the review's head line. The site, the downloads and the stores are stamped to the minute on the attempt record as of 2026-11-12 (release-attempt.md's cost record, items (k) and (l)): `fauna-publish deploy-site` and `release` stamp the attempt's `site` step from the green deploy run's own start and end and `attempt read` ends a running one at the run's end; `just store-review` stamps each store's submit and available (`--at` the minute stated, else the minute read); the review prefers these over the release table's day stamps, which stay its fallback for a record written before them. Not built: the cadence objective's `due` is still the last published close plus N days, not the Monday open. 0.1.7's task, minted before the mechanism, carries its clock by hand.

- **The store legs are built and have never run; until each runs green its store is reached by hand (2026-11-12).** All three legs are built in the private tree and reach the public repository with the next version sync ([`installers/store-review.md`](installers/store-review.md) § Implementation status today owns the detail): the App Store's credential-free build job and its signing, upload, TestFlight-group and Beta App Review steps in `release-macos.yml`, and the Play and Microsoft Store steps in `release.yml`. Until then iOS's one upload, 0.1.6 build 1, was made by a session running `just ios-store-upload <version> upload` on macOS (2026-11-11); Google Play holds one upload, 0.1.3, uploaded in the Play Console by the owner; and the Microsoft Store submission is the owner's Partner Center visit. No store write credential is entered in the release Environments yet.

- **A release is never given up (ruled 2026-11-12) — the refusals and the stage BUILT 2026-11-12.** Built: `fauna-publish tag` refuses unless the acceptance record holds every piece green, or red and accepted by the owner, at the candidate digest; the same read stands alone for a store upload, and `just ios-store-upload <version> upload` asks it before it builds; `fauna-publish promote-ready` before the public tag answers `ACCEPTANCE GREEN before the tag` once the set is green, so the reading is taken first; a give-up by the release session is refused (the owner's word alone); the attempt's open refuses while the version before it (from 0.1.6 on; 0.1.5 is history) has no promoted release-table row. Each refusal is pinned by a test. The stage is built, not yet run by a release: it spends nothing (it moves `:test` to a signed candidate, a fix before the public tag is staged again at the same version, and a new build of a tagged version is refused), and `:<version>` is written once, by the promote ([`image-pipeline.md`](image-pipeline.md) § Image tags & channels, the UAT bullet's decision (3), and its § Implementation status today). **Built 2026-11-12:** the release-readiness lane reads the `regression` piece ahead at the releasable marker, from the background tester's records, and routes a red one to the `release` janitor section; the other four pieces need the staged candidate and stay the attempt's ([`release-readiness.md`](release-readiness.md) § The readings, *The acceptance pre-reading*). Play and Partner Center uploads are console steps the release procedure gates on the same read by hand. With it, the owner's two step-order rulings of the same day: the public text is presented at the attempt's open (the runbook's step order), and `promote-ready`'s READY sentence asks the `release`, `release-macos` and `promote-nest` approvals in one message while the release builds do not yet exist. **Built 2026-11-12:** the owner's acceptance of a known issue is recorded by `fauna-publish promote-ready <version> --accept <piece> --why …` (an `[[accepted]]` table of the version's acceptance record, covering exactly the red reading it was given for), and READY then says so in its first sentence.

- **Fully implemented as of the ratifying commit**: source and every lockstep
  surface agree at `0.1.7`; `version-lockstep-check` (+ its `-test` companion)
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
- **The 2026-10-24 amendment (the deploy at the end of the release) is built, not yet exercised on a release (first: 0.1.6):** `fauna-publish build-site <version>` dispatches `deploy-site.yml` with its `build_only` input at the sync point (a run titled `Build fauna.social at <sha>`, uploading nothing), and `fauna-publish tag` refuses without a green build there, a deploy counting as one; `fauna-publish deploy-site <version>` refuses until the public Release `v<version>` exists, is not a draft, and carries every file `installer/downloads.json` at the sync point links, whichever workflow publishes it. Separately, two site deploys of 2026-10-24 ran from private `main` past the sync point, on the owner's word, to publish the `/apps` page and three site fixes before the 0.1.6 sync.
- **The site surface of the train (2026-10-17) is built:** `fauna-publish
  deploy-site <version>` dispatches `deploy-site.yml` at the sync point (the
  workflow's `commit` input, named in the run's title); the tag's check of it
  became a build check under the amendment above. The live site was last deployed 2026-09-26; the 0.1.4 sync's
  deploy is refused by its draft check — its tree wraps the code-signing policy
  page while that text was still a draft. The page and `/terms` were approved in
  public-texts round 1 (landed 2026-10-19, after 0.1.4's sync point), and the
  draft check counts a round's approval as ending a draft even while the
  inventory still says `drafted` (a first-time page's status moves to `live`
  only on its first readback), so the first sync cut after round 1 deploys
  both pages.
- **The About-row surface is not on all 7 apps yet (checked 2026-09-23)**:
  only tui (`apps/fauna-tui/src/settings/about.rs`, the Settings root's
  `settings-app-version` — painted 2026-09-26; the 2026-09-23 check had named
  a tui `general.rs` that never existed) and linux
  (`apps/fauna-linux/src/settings/general.rs`) render the product version,
  from `env!("CARGO_PKG_VERSION")`, and macOS and windows (Settings → General's
  About block, both 2026-10-08) from `fauna_ffi_build_version()`, and web
  (Settings → General's About block, 2026-10-19) from `fauna-wasm`'s
  `buildVersion` export — its Status "Build" section's git SHA
  (`VITE_GIT_SHA`) stays a separate row, and nothing displays its lockstep
  `package.json` copy. iOS and android show no app version
  anywhere, although each could read `fauna_ffi_build_version()` or its own
  lockstep copy (iOS has shown it since 2026-10-22 — the Settings root's About
  section, from `fauna_ffi_build_version()`, painted with the terms link; android
  since 2026-10-20). The dashboard "Version" stat on web and Apple is the nest's
  version, not the app's. The row's ui.yaml id is `settings-app-version`,
  painted on tui, on linux's General page (2026-10-05), on macOS's and
  windows' (2026-10-08) and on web's (2026-10-19). The organisation's terms link, `settings-terms-link`, joins this About
  row on every app beside the version — owner
  [`../behavior/terms-and-content-guidelines.md`](../behavior/terms-and-content-guidelines.md)
  § The decisions, item 4 (ratified 2026-10-20; painted on tui 2026-10-20 as a
  show-and-copy button, on web, linux and android 2026-10-20 as a
  browser-opening link — android's Settings root gained an About section for
  it, carrying `settings-app-version` from `fauna_ffi_build_version()` — and on
  macOS (Settings → General's About block) and iOS (a new Settings-root About
  section, like android's) 2026-10-22 as a browser-opening link, and on
  windows (Settings → General's About block, through `UrlOpener`) 2026-10-28).

# Release integrity — supply-chain & shipped-code trust — target state

Owns: release-integrity, container-image-trust, ci-action-pinning, official-apps-list
Status: ratified — threat model + priority ordering user-ratified 2026-06-28/29; § Third-party container images ruled 2026-08-27 (refutable) — pins + gate BUILT the same day, the mirror pipeline BUILT 2026-08-29 (bundle flip pending one dispatch); § Third-party GitHub Actions ruled 2026-08-31 (refutable) — pins + gate BUILT the same day
Authority: owns release / supply-chain / build-time trust — the integrity of the code Fauna ships to users (dependency verification, trust tiers, dossiers, release signing, client blast radius) and the controls that protect it. Neighbours that do **not** own this: [`security.md`](security.md) owns **runtime** crypto discipline + transport trust (sign-over-CID, verification recipe, decode-strictness, key-management invariants); [`build-system.md`](build-system.md) owns build **mechanics** (recipes, the self-hosted CI runner, ghcr image channels, Watchtower deploy, `deploy-verify`) — not the trust model over them.

> **Audience:** everyone working on the ship pipeline — CI workflows
> (`.github/workflows/`), the merge script, the nest Docker image, the
> client installers/updaters, dependency manifests, `build.rs`, proc-macros, or
> any code that decides *what reaches a user*.
>
> **Design record + current-state map + verified infra facts:**
> (tracked internally; ratified 2026-06-28).
> **Threat model + priority ordering ratified by the user 2026-06-28:** same
> design doc § *Threat model* (transcribes the user's verbatim reasoning).
> **ECS-specific instance** of the same dev-machine-compromise threat:
> (tracked internally; ratified 2026-06-28).

## Goal

Protect **the integrity of the code Fauna ships to users — client code foremost**
(the client is the plaintext endpoint; a malicious client update defeats E2E
entirely, which no server-side guarantee touches). The asset is **user data**;
shipped-code integrity is the proxy.

The deliverable is an **automated, human-free** posture such that **no single
dev-tier machine compromise** — a rogue / prompt-injected AI session, a popped dev
VM, or a malicious dependency pulled by a session's build — **can ship code to
users**, across all ship pipelines (nest + 6 apps), **with fleet merge velocity
preserved.** Controls gate **releases** (what users run), **never merges** (dev
velocity is non-negotiable — thousands of autonomous sessions), and there is **no
human in the loop** of a release (USER-RATIFIED 2026-06-28: *strict — no human
ever*, even on the rarest high-risk surface).

## The only configuration surface still applies

Nothing here introduces an "operator." Trust tiers, the controls, and any signing
keys are **deployment/build wiring + hard-coded Rust constants**, never a
user/admin choice and never a hand-edited knob (`principles.md` § One
configuration surface). A user/admin never sees these controls;
they only ever receive an artifact that has already passed them.

## Defense priority — what actually protects user data (USER-RATIFIED 2026-06-28)

The terminal harm is *compromised code shipped to users*. The cheapest road to it
is the **in-source** route: a dev-tier attacker poisons `origin/main` (gateless
ff-push, no review) and the pipeline faithfully builds and ships it. **No
signature, attestation, or reproducible build helps against in-source malice — it
reproduces and gets *blessed*** (user: *signing "does nothing more than bless that
malware"*). So the defense is ordered by what bites the in-source route, not by
what is cryptographically satisfying:

**PRIMARY — source-side prevention + detection + containment:**

1. **Dependency verification — minimize + pin + vet the *shipped* surface.** The
   only genuine *prevention* lever against dependency malware is not pulling it in:
   a small, pinned, actually-reviewed shipped dependency closure. This is the
   **first deliverable** (§ *Dependency verification*).
2. **Detection speed + transparency.** A dev-machine compromise that introduces
   *stealthy* malice cannot be prevented human-free (accepted limit, below); the
   defense degrades to finding it fast. Audit/anomaly signal on the rare high-risk
   surface (new dep, lockfile, `build.rs`, workflows, `Dockerfile`, crypto,
   updater), an append-only transparency trail, and — decisively —
   **open-sourcing** (planned shortly), which lets anyone read and reproduce the
   source: a *stronger* transparency guarantee than a signature, and the one
   legitimate "users can verify we don't lie about the source" value.
3. **Client blast-radius limits.** Because the client is the E2E endpoint, even a
   shipped malicious update must not be able to *silently* exfiltrate plaintext or
   keys: constrained / observable client egress, key export gated on explicit user
   action. Containment for the case prevention and detection both miss.

**SECONDARY — release signing + consumer verification (kept, built last).**
Signing + a consumer that verifies it closes **only** the gap *between* approved
source and the shipped artifact — **build-time injection** (tampering on the self-hosted CI runner)
and **distribution-channel tampering** (poisoning ghcr, swapping a binary, MITMing
an update). That gap is real and wide open today (Watchtower verifies nothing),
and closing it once is cheap insurance worth having. But within the *stated* threat
model — a dev-tier machine compromise — the attacker holds the push key and takes
the in-source route signing cannot stop, so **signing is secondary, not the
linchpin, and must never be sold as the protection.** Sequence it last (§ *Release
signing*). Reproducible builds stay a *lower-tier* control (malicious maintainer /
build-time injection), never the primary defense.

**Accepted limit (USER-RATIFIED):** a *stealthy in-source malicious dependency*
that passes vet/audit/policy reproduces faithfully and ships. The posture stops the
single-machine-compromise class and makes everything detectable + contained; it
does not achieve perfect prevention. *"Stealthy dep malware that fooled reputable
users will fool us too."*

**Shipped runtime semantics (2026-08-19):** release artifacts build with
`overflow-checks = true`, so a debug test's overflow red-verify is evidence about the
shipped binary and a missed arithmetic guard fails one contained request loudly instead
of silently corrupting cross-principal state. Ruling, reasoning, and escape hatches:
[`build-system.md`](build-system.md) § Shipped-profile overflow checks.

## Trust tiers (target state)

Every machine in the ship pipeline sits in exactly one tier. The invariant: **the
authority to run untrusted code and the authority to ship to users MUST NOT live
on the same machine.**

- **Tier 2 — dev fleet** (Linux/Windows/macOS dev VMs, **all AI sessions + their
  dependency trees**). Runs untrusted code. **Holds NO unilateral ship
  authority** — *proposes* source (writes to `main`), cannot *release*.
  **Proposing source to the public repository is confined to ONE Tier-2
  machine (user-directed 2026-08-28): the Linux dev machine's standing mirror
  is the repository's only push origin** — still a proposal (the public CI and
  branch protection gate the merge), narrowed to one box so the publish
  tooling, the per-machine personal-terms file and the clone are kept in
  lockstep in exactly one place; the mechanism, the mirror's incremental
  contract and the mac/win build-fixture role → [`merge-gates.md`](merge-gates.md)
  § Local-merge gates → *The standing mirror and the push origin*.
- **Tier 1 — build infra** (the self-hosted CI runner, GitHub-hosted runners). Builds **only
  gate-approved source.** Scoped, short-lived credentials; no standing dev-fleet
  credentials; no untrusted code beyond the source admitted.
- **Tier 0 — the gate + release authority.** A minimal, single-purpose, hardened
  machine **off the dev-fleet attack surface** (different host — preferably **not**
  the shared host the dev VMs run on; different software; different creds; no AI sessions). Its
  primary job is the automated dependency-verification + detection gate; it also
  holds any release-signing key (the secondary control). *Provisioning is a
  user-gated open decision — until it exists the gate runs in advisory mode.*

## Dependency verification (the first deliverable)

> ⚠️ **READ FIRST — until 2026-07-17, NOTHING in this section ran. Present-tense prose
> below describes CODE, not behaviour.** Verified 2026-07-17: all **200** runs
> `supply-chain.yml` has ever had (2026-03-30 → 2026-07-17) concluded `failure`, every job
> with `steps: []` after 3-4s, on GitHub's billing error — *"The job was not started because
> recent account payments have failed…"*. **Not one of `cargo-vet` / `cargo-audit` /
> `cargo-deny` / `govulncheck` / `vendored-forks` / `dep-prescan` had ever executed.** The
> 2026-06-01 § S1 review restored this workflow's triggers *specifically so vuln scanning
> would run*; that fix was **inert on arrival** — the missing thing was a runner, not a
> trigger. Billing stays unfixed **by choice** (user decision 2026-07-16 —
> `merge-gates.md` § CI enforcement is the authority on the posture).
>
> **Repair (2026-07-17):** the workflow's jobs were pointed at the billing-exempt **self-hosted
> runner**. ✅ **OBSERVED the same day — scanning RUNS, and this block's "pending" caveat is
> CLEARED.** Runs `29580191806` + `29580271394` executed there (push-triggered, no dispatch
> needed); every job recorded 7–10 real steps, against `steps: []` for all 200 prior runs. So the
> present-tense prose below now describes **behaviour**, not merely code — with two exceptions
> called out inline: `govulncheck` **scanned a Go toolchain the shipped image did not use** (a
> gate-fidelity defect — the scan pinned `go1.26.4` while the image floated to the fixed 1.26.5;
> **RESOLVED 2026-07-18** by pinning the Go toolchain exactly and driving the scan + image from one
> source — `build-system.md` § Go toolchain pin), and `cargo-audit` is RED. **Authority on the
> posture, the per-job results, the measured contention, and the gate-fidelity resolution:
> `merge-gates.md` § CI enforcement + `build-system.md` § Go toolchain pin** — do not restate those numbers here.

> **The dated history now lives in its own doc (split 2026-10-05).** Every dated `Correction` / `Update` entry this section accumulated — from the first real scan (2026-07-17) through the pre-scan ledger's npm leg (2026-10-05): the advisory triage and each disposition, the `cargo vet` drifts and re-greenings, the wasmtime tree's pre-scan and venue review, the ledger's seeding — was carried **verbatim** to [`dependency-verification-log.md`](dependency-verification-log.md), and a citation of the form *§ Dependency verification, the `<date>` entry* resolves there. New dated entries are appended to the log, never here. This section keeps the current-state target below; the status of each scanner today is § *Implementation status today*. The one lesson the log keeps teaching stays here: **a scanner result is valid only for the lockfile state it ran against — re-run the scanners after the last dependency edit, never before, and measure a remediation's effect rather than predicting it.**

Verification today is **hollow** — and, until repaired on 2026-06-28, was not even
passing. `cargo vet` rests on a large **exemption backlog** (770 grandfathered
crates) + imported third-party audits, with only **22 Fauna audits** (as of
2026-07-07; the first landed 2026-06-28) — *"passing"* means *"grandfathered or
covered by someone else's audit,"* not *"we reviewed it."* Worse, the gate had **drifted RED**: the fleet
adds/bumps deps via the merge gate without running vet, so `Cargo.lock` outgrew the
`[exemptions]` snapshot and `cargo vet --locked` failed on **165 now-uncovered
crates** (plus a stale `[policy.fauna-bridge-smtp]` for an excised crate and a
missing `audit-as-crates-io` for the vendored `ksni` fork) — breaking `release.yml`
and the new merge-path trigger. A perpetually-red gate gives **zero** signal (a new
bad dep is indistinguishable from the 165 stale ones), so step 0 of the burn-down is
keeping it green: refresh imports + `cargo vet regenerate exemptions` (`just
vet-regen-exemptions`) re-grandfathers the genuine backlog. And the audit had never
run on the path the fleet uses (the merge gate's ff-push has no audit gate; `cargo
vet`'s PR trigger is skipped by direct merges — now closed by the merge-path trigger;
`build-nest-image.yml` has none, and at the time did not even build `--locked` —
the `--locked` half is **closed** (S6, 2026-07-05, validated by a real production
build 2026-07-07; see § *Implementation status today*), the missing audit gate on
that path is not). Target state:

- **Define the *shipped* dependency closure** — the crates that reach a user (nest
  image + each app binary), separated from dev/test/bench-only deps. Verification
  effort concentrates here; the build-time-executing subset (`build.rs` +
  proc-macros) is highest-priority within it (it runs arbitrary code at build time).
- **Pin everything** the shipped closure resolves — `Cargo.lock` is authoritative
  and **`--locked` is enforced in every shipped build** (including the nest image),
  git deps pinned by `rev`, web deps by SHA-512 integrity.
- **Replace exemptions with real review for the shipped closure, driven by a
  dependency dossier + scoring system.** For *every* dependency — direct or
  transitive — maintain a dossier (authors/publishers, GitHub stars, OpenSSF
  Scorecard, downloads, usage in other projects / reverse-dependents,
  documentation, crate age/cadence, advisories, `unsafe` surface, build-time-exec
  flag, source size) and a **score** that triages **how much of the code we read**
  (read-all / read-the-risky-parts / reputation-trust). Reputation normally only
  lowers the read depth to "read the risky parts"; **skipping the source read
  entirely is a rare exception reserved for major-company / institutionally-backed
  crates** (organizational accountability, not mere popularity — a popular
  single-maintainer crate is still read; USER-RATIFIED 2026-06-28). Drive shipped-
  closure crates from `[exemptions]` to Fauna `audits.toml` entries at the
  score-mandated depth — so a shipped dep is one Fauna *reviewed*, not one the list
  grandfathered. **Apply retroactively to all existing deps and as a gate before any
  new dep** (USER-RATIFIED 2026-06-28). The score is a triage prioritizer, not a
  verdict; the review labor is an AI-fleet fan-out (one agent per crate, by score);
  the result is recorded in cargo-vet (delta audits cover version history).
  (Design ratified 2026-06-28; tracked internally.)
- **Every review effort is logged with full provenance** (USER-RATIFIED
  2026-06-28): the **exact Claude model version** that did the reading (so a later
  model can re-review what an older one passed), the **exact dependency version**
  read, and **where the bytes were fetched from + their content hash** — bound to
  the `Cargo.lock` `checksum` (crates.io) or git `rev`, so a review applies only to
  the exact bytes the build pulls, and a hash mismatch reverts the crate to
  unreviewed. Append-only; it is the evidence trail.
- **Surface today** (`dep-inventory`, the source of truth; refreshed
  2026-06-28 after the baseline repair): **1042 third-party crates** (1142 total −
  100 Fauna-local), **833 shipped** (685 in the nest closure), **214
  build-time-executing** (proc-macro/`build.rs`), **770 exemptions**, and a
  **129-crate priority-1 read-first list** (shipped + build-time-exec + still
  exempted) at that baseline. As of 2026-07-07 (`supply-chain/audits.toml`):
  **22 Fauna audits** (the `async-trait`/`futures-macro` pilot + Batch 1a), the
  priority-1 backlog at **109 crates** (per the dependency-review track's log, tracked internally).
  *(The pre-repair
  "124" undercounted: the 165 unvetted-and-unexempted crates fell outside the
  inventory's "still exempted" filter, so re-grandfathering them made the backlog
  honest — the count is only meaningful once the exemption set covers the full
  unvetted closure, i.e. the gate is green.)* **Re-verified 2026-07-23** (`just
  dep-inventory`, live run): **1197 third-party crates** (1328 total − 131
  Fauna-local — Fauna-local grew with the windows-workspace-unification merge,
  which folded `apps/fauna-windows`'s six crates into the root workspace with
  zero third-party dep churn), **916 shipped** (673 in the nest closure), **251
  build-time-executing**, **856 exemptions**, and the priority-1 backlog now at
  **131 crates** — up from 109, against a **still-22** Fauna-audit count (no new
  audits landed since Batch 1a): the unvetted shipped surface is growing faster
  than it is being reviewed, the opposite of the "Minimize" goal below.
  **Re-verified 2026-08-13** (`dep-inventory`, live run, 3 weeks after the
  2026-07-23 figures above): **1182 third-party crates** (1325 total − 143
  Fauna-local — the growth is continued feature landings, not a single merge),
  **905 shipped** (694 in the nest closure), **247 build-time-executing**, **852
  exemptions**, and the priority-1 backlog now at **132 crates** — against a
  **still-22** Fauna-audit count (`supply-chain/audits.toml`, 22 `[[audits.*]]`
  entries, unchanged since Batch 1a): the trend called out on 2026-07-23 continues
  — unvetted shipped surface roughly flat-to-growing while reviewed crates stay
  at 22. **Re-verified 2026-08-26** (`dep-inventory`, live run, 13 days after
  the 2026-08-13 figures above): **1161 third-party crates** (1311 total − 150
  Fauna-local), **895 shipped** (693 in the nest closure), **241
  build-time-executing**, **849 exemptions**, and the priority-1 backlog now at
  **130 crates** — down from 132, against a **still-22** Fauna-audit count: the
  first reversal of the growing-backlog trend noted above, driven by dependency
  removal (the WireGuard-stack deletion) rather than review throughput — the
  "reviewed crates stay flat" half of the trend still holds.
  **Re-verified 2026-09-16** (`dep-inventory`, live run, 21 days after the
  2026-08-26 figures above): **1161 third-party crates** (1316 total − 155
  Fauna-local), **882 shipped** (682 in the nest closure), **241
  build-time-executing**, **836 exemptions**, and the priority-1 backlog now at
  **129 crates** — roughly flat (130 → 129), continuing the 2026-08-26 reversal
  (shipped/nest-closure/exemption counts all fell further, consistent with more
  dependency removal rather than net feature growth pulling in new deps),
  against a **still-22** Fauna-audit count (`supply-chain/audits.toml`, 22
  `[[audits.*]]` entries) — the "reviewed crates stay flat" half of the trend
  still holds, now three consecutive re-verifications (2026-08-13 →
  2026-08-26 → 2026-09-16) with zero new Fauna audits landed.
- **A dependency / lockfile change is a high-signal, audited event on the real
  merge path** — `cargo vet` + `cargo audit` (+ a `cargo-deny` source/ban/license
  policy) run on every `main` push that touches dependency files (not only PRs and
  the scheduled cron — daily since 2026-10-04), so the gateless ff-push no longer outruns the audit. *Caveat:*
  any check enforced at merge time runs on Tier 2 and is bypassable by a
  malicious session — so the **authoritative** check is the post-push re-validation
  on clean infra (CI today; Tier-0/Tier-1 in target state), which an attacker would
  have to compromise *separately*. This is a **detection** guarantee, not pure
  prevention (consistent with the accepted limit).
- **Minimize** — track the shipped-closure crate count and reduce it; a new shipped
  dependency is a deliberate, reviewed addition, not an incidental transitive pull.
- **Publisher trust (`cargo vet trust`) — ruled 2026-10-02 (refutable): Fauna records NO `[[trusted]]` entries; an exemption stays until a Fauna audit replaces it.** `cargo vet suggest` proposes trusting the crates.io publishers that the imported `mozilla` and `bytecode-alliance` audit sets trust (thirteen accounts on 2026-10-02 — `dtolnay`, `cuviper`, `rust-lang-owner`, `epage`, `kennykerr`, `BurntSushi`, `alexcrichton`, `seanmonstar`, `JohnTitor`, `Amanieu`, `Darksonn`, `Thomasdezeeuw`, `sunfishcode`; the set moves with the imports), which would clear exemptions wholesale. Declined, as policy, for four reasons. (1) A `trusted` entry is the "skip the source read entirely" tier applied to every future version the account publishes, and the 2026-06-28 ruling above reserves that tier for institutionally-backed crates decided per crate — reputation lowers the read depth, it never waives the read; every suggested publisher but `rust-lang-owner` is an individual maintainer account, exactly the "popular single-maintainer crate is still read" case. (2) An exemption is pinned to one version, so every bump surfaces as a vet-red that a session re-grandfathers with its eyes open (the 2026-09-08 and 2026-09-10 entries are that mechanism working); a trust entry is open-ended and silent — the next version lands reviewed by nobody. (3) Trust binds to a crates.io account, the compromised-maintainer vector itself; the imported wildcard audits that cover the wasmtime family are a different claim — bound to trusted publishing from a named repository's release workflow and vouched for by an organization that reads code — and stay import-side. (4) `dep-inventory`'s priority-1 backlog is "shipped + build-time-executing + still exempted" (the `dep-inventory` script): a trusted crate drops out of the backlog without having been read, which makes the count dishonest in the way the pre-2026-06-28 "124" was. What may change it: a per-crate decision at dossier time for a publisher that is an organization's release account (`rust-lang-owner` is the one candidate in the list above), recorded as a `[[trusted]]` entry whose `notes` carry the dossier's organizational-accountability finding. Importing a further audit set (`cargo vet suggest` names `zcash`) is a separate question — an import is an audit someone performed, not a trust grant — and welcome whenever a set covers shipped crates.

### Vendored forks (`[patch.crates-io]` / path sources)

A **vendored fork** — published crate source copied into the tree and consumed via a
path dep or a `[patch.crates-io]` override — **leaves cargo-vet's model entirely**,
and cargo-deny does not compensate (its `sources` check is crates.io-only with
`unknown-git = "deny"`, yet passes with a fork present: a path/patch source is not an
"unknown source"). Live instance: `libs/ksni` (published ksni 0.2.2 + a 3-file
xdg-activation patch, with its build-time code generator replaced by the committed output).

- **Every fork carries an explicit `[policy.<crate>] audit-as-crates-io` entry, and the
  answer is `false`.** A fork must **not** inherit audits written against the real
  published bytes: ours differ by the patch, so a Mozilla/Google audit of
  `ksni@0.2.2` would not describe what Fauna ships. `true` would launder someone
  else's audit of *different bytes* into coverage of ours — the failure mode is worse
  than no coverage, because it reports as green. (Ratified for `ksni` 2026-06-28.)
- **The consequence is deliberate, and must never be mistaken for coverage:** `false`
  means cargo-vet requires *nothing* for the fork. A fork's trust rests on three
  compensating controls, not on the gate — (a) **provenance at vendor time** (cargo
  verified the registry checksum before the source was copied in); (b) a **PATCH.md**
  naming the upstream version and rationale, plus `// FORK (...):` marker comments on
  divergent code sites, so a reader can tell patch from upstream; (c) the **re-vendor
  drift check** — *the tree equals the pristine published source MINUS exactly the
  recorded divergence* — which is the only control that survives a future re-vendor.
- **(b) is documentation, not a control — never let it gate anything.** A marker comment
  proves nothing: whoever tampers with the tree can also type the marker, so a check that
  accepted "this hunk is labelled, therefore it is fine" would report **green on a
  backdoor** — the same failure mode this section rejects for `audit-as-crates-io =
  true`. Labels are also *incomplete* by nature: `libs/ksni` carries no markers at all,
  describing its patch in PATCH.md prose instead.
  So the labels help a human read the diff; only (c) decides.
- **(c) is the gate, and it compares bytes.** `supply-chain/vendored-forks/manifest.json`
  records, per fork, the pristine published `.crate` + its **crates.io-published checksum**
  and the exhaustive set of paths allowed to differ, each pinned to its expected content
  hash. `dep-vendor-drift` (CI: the `vendored-forks` job) asserts every other file is
  byte-identical to the published source and fails closed on anything unrecorded — an
  unlisted edit, an unlisted new file, a resurrected deletion, or a listed file whose
  content moved. This collapses the reviewable surface of a re-vendor from an unreadable
  whole-crate diff to a ~12-line manifest delta, and it is what makes `--update` safe to
  offer: the diff *is* the review. The check is **hermetic** — the pristine `.crate` is
  committed, so the gate needs no network (the same reasoning that keeps cargo-vet on
  `--locked`). Its anchor still has external authority: `pristine_sha256` is the value
  crates.io publishes, so tampering cannot stay invisible — it must also rewrite that
  constant into a public, permanent disagreement with the registry, which
  `dep-vendor-drift --verify-upstream` (network, deliberately not in the gate)
  re-checks on demand.
- **⚠ The gate's fail-closed guarantee only holds when it *runs* — until 2026-07-22, a
  direct edit to a fork's own tree never triggered it.** `supply-chain.yml`'s trigger
  `paths` filter covered dependency manifests (`Cargo.toml`/`.lock`, `supply-chain/**`)
  but not the fork trees themselves, so a commit editing a fork's source directly
  (`libs/ksni/`; e.g. a clippy fix) fired nothing — the drift sat
  undetected until the next scheduled cron. Found when a commit drifted `libs/ksni`
  (5 files) silently for days. Fixed by adding every fork tree (`libs/ksni/**`, …)
  to the trigger's `paths` filter; the check itself (below) was never the gap.
- **⚠ `cargo vet --locked` cannot enforce the first bullet — the drift check does
  instead.** Deciding whether a local crate matches a published version needs crates.io
  metadata, which `--locked` will not fetch — so `cargo vet --locked` (the gate in
  `supply-chain.yml` + `release.yml`) **silently skips a fork with no policy entry**,
  while plain `cargo vet` (`just vet`) hard-errors demanding one. A new fork therefore
  lands **green in CI** and reds only the next session to run `just vet` locally.
  `dep-vendor-drift` closes this **without** asking the unanswerable-offline question: it
  never checks whether a name shadows a published crate, only that a fork's three
  declarations agree, which is pure local TOML/JSON — **(A)** every `[patch.crates-io]`
  path override has a policy entry; **(B)** policy-entries-carrying-the-flag ==
  manifest forks, a bijection (a crate declared a fork to cargo-vet but absent from the
  manifest is an *unchecked* fork, since the flag means vet requires nothing); **(C)** the
  flag is `false`. Adding the entry remains a **manual step of vendoring** — the gate now
  fails closed when it is skipped.
- **Residual gap: a fully *undeclared* fork.** Published source copied to a plain path dep
  with no `[patch.crates-io]`, no policy entry and no manifest row cannot be detected
  offline — recognising that the name shadows a published crate is exactly the crates.io
  lookup `--locked` refuses. Plain `just vet` remains the backstop (it hard-errors on
  "non-crates.io-fetched packages match published crates.io versions"), which is one more
  reason to run it when touching this area. `libs/ksni` is that shape, declared.
- `supply-chain/config.toml` is **cargo-vet-managed** — any non-`--locked` run rewrites
  it and **strips comments** — so a fork's rationale cannot live there. It lives here.

### Reviewing untrusted source without being subverted (USER-RATIFIED 2026-06-29)

The dependency review is **itself an attack surface**: a malicious crate author
controls every byte the reviewer reads and can target the reviewer. Beyond stealth
(threat 1, the accepted limit), two prompt-injection failure modes: **(2a) verdict
subversion** — the reviewer is induced to *accept* the malware, which ships to users
undetected; **(2b) machine compromise** — injection during review (or a malicious
`build.rs` during a verifying build) compromises a dev machine, and the attacker then
ships at will. The earlier soft-rollout's *"reading executes nothing, so it is safe on
a credentialed machine"* answers **neither** for low-reputation source:
reading-executes-nothing addresses build-time **code** execution, but an LLM reviewer
is an **execution engine for the text it reads** (2b), and the review **venue is
irrelevant to whether the verdict is subverted** (2a). So untrusted / low-reputation
review splits into **two orthogonal layers** (high-reputation, already-shipping crates
stay reviewable on a credentialed machine — the prior soft-rollout holds *there*):

- **Containment (defeats 2b) — venue.** The *reading* of untrusted source and the
  *building* of it (`build.rs`/proc-macros) run in a **credential-free isolated
  environment** (no SSH/`gh` token/push/deploy creds; egress default-deny + captured),
  so an injected reviewer or malicious build script is trapped — it cannot pivot to
  the dev fleet or the ship pipeline. **The venue buys containment only — it does
  nothing for the verdict.** (Build containment: the ECS isolated-build plan. Read
  containment: a credential-free local container or the cloud review surface.)
  **Read venue PINNED 2026-09-06, refined 2026-09-09 (user-directed): Claude Code on
  the web, against a private, single-purpose sandbox repository, connected to
  claude.ai through a DEDICATED GitHub account that is a member of no organization
  and a collaborator on that one repository only — never the project owner's
  personal account, never the project's own repositories — in a cloud environment
  whose network access level is `None`, verified black-box before the first review.**
  Why the dedicated account: the vendor's own documentation states that a cloud
  session "can access any repository the connecting GitHub account can see, not just
  the repositories the Claude GitHub App is installed on" and that the App install
  "is not a session-level access control" — so scoping the App to one repository
  fences nothing, and the 2026-09-06 wording ("the venue holds exactly one
  credential, the repository-scoped app grant") was wrong. What fences a session is
  (i) the connected account's own GitHub visibility — hence the dedicated account,
  since the owner's personal account administers the project organization; (ii) the
  vendor's GitHub proxy, which keeps the real token outside the VM (the session sees
  only a `proxy-injected` placeholder), restricts `git push` to the session's own
  branch, and limits API and release-asset requests to the session's attached
  repositories; and (iii) the environment's `None` level — the onboarding-created
  Default environment is `Trusted`, whose allowlist reaches crates.io and raw GitHub
  content, so a review never runs there. The App is nonetheless installed — the web
  UI lists only repositories it covers — on the sandbox's *owning* account, scoped to
  the sandbox alone; it fences nothing, and its declared permissions widen the
  surface (write on workflows, actions and repository hooks), so GitHub Actions stay
  disabled on the sandbox (the App holds only read on administration and cannot
  re-enable them) and the sandbox's webhooks are audited before and after every
  review run, any webhook being a stop. Under `None` the session still reaches the
  vendor API and the attached repository through the proxy, which is what the
  black-box check probes: registry and raw-content fetches must fail, the project's
  own repositories must be unreachable by `git ls-remote` and by API, the sandbox
  must be reachable, and no real credential may appear in the environment (credential
  variables hold the proxy's placeholder; the session's own vendor messaging token is
  the one real value, and reaches only that session). A session can ask to attach
  further repositories, bounded by the connected account's visibility — the reason the
  dedicated account, not the App, is the fence. The crate
  source is pushed *into* the sandbox from a dev machine (checksum-verified against
  the registry index first), and findings leave it only as JSON files the aggregator
  parses as data — no session on a credentialed machine reads them raw. First
  exercised on the kache build-cache review, which also serves as the venue's
  shakedown. **Launching a venue session from
  a dev machine is allowed only through a path that pins repository, environment and
  model and hands back nothing the session wrote (verified 2026-10-05: one routine per
  model, fired by token, returning a session id and URL only; the token is held in files
  no session reads), and a venue session is never pulled onto a dev machine** — no
  attach, teleport or transcript fetch, each of which loads reviewer-authored text into a
  credentialed session — **and the venue's account runs no cloud session but venue
  sessions**, since a venue session's tooling can reach other cloud sessions of its
  account with a scope the vendor does not document. The operating manual carries the
  mechanics and the launch-path check.
- **Verdict integrity (defeats 2a) — methodology + enforcement, venue-independent.**
  The review must not be trickable into accepting malware, and the gate must be
  un-bypassable by a compromised reviewer **or** a malicious contributor: (i) a
  **mechanical, injection-immune pre-scan** (deterministic — Trojan-Source/bidi/
  zero-width unicode, capability census, typosquat; the one layer injection cannot
  social-engineer — in what it *detects*; a RED it raises is lifted only as *Clearing a
  pre-scan RED* below rules; reviewer-addressing text in the source is itself a red flag,
  never an instruction); (ii) the **verdict is un-authorable by the crate** — the AI reviewer
  emits *findings*, never the accept decision, and `cargo vet certify` is issued from
  `Cargo.lock` by a trusted aggregator outside the reading context; (iii) **adversarial
  N-of-M, model-diverse review** — independent reviewers each prompted to *refute*,
  quorum required, so an injection must fool all N independently — and each reviewer's
  **read depth is checked, never taken on its word**: the pre-scan lists every
  net / process / unsafe / codegen line in shipping source (`tests/`, `benches/`,
  `examples/` excluded) as a review site, and the aggregator rejects any reviewer that
  does not disposition every site with a verbatim quote of that source line, verified
  against the scanned source (the site list carries only file:line, so the quote proves
  the file was opened there; ruled 2026-10-03 after a depth-B review of a 129k-line crate
  came back in about five minutes); (iv) **enforcement on
  clean infra, off Tier 2** (§ *The enforcement point*) — a fooled/subverted Tier-2
  review, or a contributor's self-certification, is never authoritative.
- **Clearing a pre-scan RED (owner-ruled 2026-10-03; it NARROWS (i) above).** A RED rejects the crate and stays RED until the venue's reviewers clear it — the "a RED is the venue's to clear" of § *Dependency verification*. The scan matches patterns, so it also fires on a test asserting that a hostile input is refused, on a code comment, on a help string; with no way to clear such a hit the crate could never be reviewed at all. **The rule:** each reviewer, in the same session as its review, rules every RED finding of the scan — keyed by file:line *and* category, the three RED categories (`unicode-hazard`, `injection`, `build-rs-net`) alike — `false-positive` or `confirmed`, quoting the whole source line. The aggregator lifts the RED only when **every** valid reviewer rules **every** RED finding `false-positive` with a quote equal to the line it re-reads from the scanned tree (tree hash checked; an invisible character must appear in the quote, raw or rendered, never omitted). Any `confirmed`, any missing ruling or any non-matching quote rejects; the reviewer and model minimums of (iii) apply unchanged, and a relaxed clearing quorum never relaxes this unanimity. Lifting the RED waives nothing else: the crate still needs the full quorum, depth and site checks. **What is given up:** before this ruling, text the scan caught could never reach a verdict. Now the reviewers — the injectable layer — read the suspected line and decide, so an injection that fools all of them into `false-positive` passes where it used to fail closed. **What bounds it:** (a) the attack must fool every reviewer, across at least two models, on that exact line — the bar every un-flagged line of the crate already faces, and here each reviewer is told the line is suspect and that any doubt is `confirmed`; (b) "cleared" is un-authorable — no findings field says it, the aggregator computes it per finding; (c) the reviewer-facing dossier carries no excerpt of a RED line (`dep-scan --no-excerpts`; the aggregator clears no RED whose scan holds one, and the prompt builder refuses such a dossier), so the whole-line quote shows the file was opened at the finding; (d) the scan's detection is untouched — the RED verdict stays in the dossier and every cleared finding is named in the certify's provenance notes, so a later reader or a later model can re-open it. There is no owner-attested clearance path. The owner ruled the mechanism; (c), the whole-line quote and the per-category key are the implementing session's details under it, refutable.
- **Ruling a pre-scan RED outside a review (owner-directed 2026-10-04 — "send the astro 7 set to the venue"; the shape below is the implementing session's, refutable).** The bump report (`dependency-bump-report.md`) marks a new or changed package RED when the pre-scan trips on one of its lines, and nobody is reviewing that package: its coverage is the report's crude checks, the minimum release age and the advisory scanners — the same as every GREEN row's. Such a RED is lifted by the venue's **RED ruling alone**: the standing quorum (two reviewers, two models) rules every RED finding `false-positive` with a whole-line quote, exactly as *Clearing a pre-scan RED* above demands, and the aggregator's ruling mode (`dep-aggregate --ruling-only`) computes the answer — `RED-CLEARED`, or `RED-STANDS` on any `confirmed`, any missing ruling, any non-matching quote, any `blocking` finding a reviewer records on the way, a quorum or model-diversity shortfall, an identity mismatch or a tree-hash mismatch. **It is not a review and certifies nothing:** no site list, no depth, no disposition on the package (the reviewers file `inconclusive`), no `cargo vet` certify, no review-ledger entry; its record is the pre-scan ledger (*The pre-scan ledger*, next bullet; `dep-aggregate --ruling-only --record` writes it), bound to the package's lockfile hash (a `deno.lock` `integrity`, a `Cargo.lock` checksum) and the scanned tree's hash. **What is given up:** the flagged lines are read and ruled; the rest of the package is read by nobody — a package whose only RED was a coincidence reaches the build machine as unread as every GREEN package already does. **What bounds it:** the ruling is the same unanimous, quote-verified act as a review's RED clearance; a line that addresses the reviewer is ruled `confirmed` by instruction; the quorum's `blocking` findings still stop it; and the clearance names exact bytes, so a later bump to other bytes is RED again. Tooling: the scanner names a package from `package.json`, the aggregator binds to a `deno.lock` entry, `dep-npm-stage` lays checksum-verified tarballs into the venue clone, and a ruling is one mode of the venue run's prompt (`dep-review-prompt-build --rule <name@version>`), whose other mode reviews a crate or an npm package in full or by delta — an npm package in full is read at depth `A`, because the scanner takes no census of JavaScript and so no site list could check a shallower read (the implementing session's detail, refutable). First use: the six REDs of the site's astro-7 bump (`undici` 8.11.2, `fast-string-width` 3.0.2, `get-tsconfig` 5.0.0-beta.4, `jsonc-parser` 3.3.1, `vite` 8.3.1, `zod` 4.6.5). When a bump's REDs come here — once per round of bumps, at its end, together with everything else the round left — is `dependency-bump-report.md` § A round of bumps (owner-ruled 2026-10-04); that first use, a pass for one bump alone, is what that ruling retires. Whatever that pass holds — reviews and rulings, Rust crates and npm packages — it is one prompt per reviewer model, so one session per model.
- **The pre-scan ledger — a RED stays RED until it is ruled (built 2026-10-05; closes the 2026-10-02 entry's gate-fidelity finding in [`dependency-verification-log.md`](dependency-verification-log.md)).** The CI pre-scan's baseline is a committed scan ledger, `supply-chain/prescan-ledger.toml`, not the previous push. It holds one entry per artifact the scanner has seen, per ecosystem — a crate keyed by name, version and `Cargo.lock` checksum, an npm package by name, version and `deno.lock` integrity — with the verdict (`clean`, `flagged`, `red`), the scanner's rule-set identity (`dep-scan.py`'s `RULESET`, pinned by a test to a digest of its rule tables, so a rule change re-scans everything), the scan date, the scanned tree's hash and, for a RED, its findings as (category, file:line) keys. The `dep-prescan` job scans every registry crate in `Cargo.lock` and every npm package in a tracked `deno.lock` (the web app's and the site's) with no entry for its exact bytes, or one under an older rule set — independent of push history, so a cancelled run loses nothing — and fails while any RED package in a lock, freshly scanned or ledgered, lacks a disposition; it runs on the daily schedule too, so an unruled RED is re-asserted even when nothing changes. **A disposition covers exact bytes only:** the same lockfile hash, the same tree hash, and every RED key of the scan among the keys the venue cleared; other bytes, another version or a new RED line is RED again. **Written by two tools, never by hand:** the ledger update (scans what is unledgered, writes the entries, prunes entries for bytes gone from their lock set — crates against `Cargo.lock`, npm packages against the union of the `deno.lock` files; a disposition stays, a ruling being a fact about bytes — and stops; the session landing a dependency runs it beside `just vet-regen-exemptions`) and `dep-aggregate --record` (a cleared RED: `--ruling-only`'s `RED-CLEARED`, or a certify that cleared one). The CI runner commits nothing: an unledgered package is a warning carrying the entry lines to land. The bump report reads the same ledger, so a RED the venue cleared on the exact artifact shows as cleared there (`dependency-bump-report.md`). It is kept apart from the review ledger (`supply-chain/review-ledger.toml`, reviews only): most entries here are crates nobody reviewed, and a clearance is not a review. The tree hash ignores cargo's own `.cargo-ok` unpack marker, so the scanner's fetch cache and the venue's staged trees hash alike. An npm package's tarball comes from the npm registry only, through the bump report's registry client, and is verified against its `deno.lock` integrity before it is extracted under the bump report's guards and scanned without excerpts — the same fetch and checks `dep-npm-stage` uses for the venue.
- **A pre-scan hit outside the shipped tree is a FLAG, not a RED (owner-ruled 2026-10-05, option 1 of three put to the owner: narrow the rule then rule once, rule everything as it stands, or accept the pre-existing tree).** A `unicode-hazard` or `injection` hit in a crate's top-level `tests/`, `benches/` or `examples/` tree — code and data that never ship in the artifact — is reported as a FLAG (`unicode-hazard-test`, `injection-test`) and no longer fails the gate; the same text anywhere else — shipping source, a build script, prose such as a README or CHANGELOG, or a `tests` directory nested inside `src/` — stays RED. Why: the whole-lockfile seed found that most RED lines sat in test data written to exercise Unicode and parser edge cases (one crate's emoji test file alone was 2,506 of 2,687 lines), which no ruling can read meaningfully line by line, while the real reading is the shipping source. **What is given up:** reviewer-addressing text planted in a non-shipping tree no longer stops the gate on its own; it stays visible as a FLAG in every dossier and bump report, and a full review still reads it. **What bounds it:** none of those trees is compiled into a consumer's artifact; a build script, the manifest and every shipping file keep the RED rule. The scanner's `RULESET` moved to `2` with this ruling, so the ledger re-scanned every crate under it the same day (1215 crates, 41 minutes): **63 RED, 2 of them the ruled wasmtime crates (both rulings carried over, same bytes and lines), 61 open with 163 RED lines (82 `injection`, 81 `unicode-hazard`)** — down from 66 crates and 2,687 lines. 45 of the 61 hold 141 lines in shipping source; the other 16 hold 22 lines in prose files only, which stay RED by this ruling.
- **A Go module's `*_test.go` files and `testdata/` directories are its non-shipping tree (owner-ruled 2026-10-05, option (a) of two put to the owner: extend the bullet above to Go, or keep Go test hits RED for the venue to rule).** The owner's answer, verbatim: "(a)". A `unicode-hazard` or `injection` hit in a `*_test.go` file or under any `testdata/` directory, at any depth, is a FLAG (`injection-test`, `unicode-hazard-test`) in the bump report's Go scan; the same text in Go source, assembly, `go.mod` or prose stays RED. Why (the ask's reasoning, accepted by the owner's choice): the go tool compiles neither into any artifact and our builds and tests never run them. **What is given up:** reviewer-addressing text planted in a Go test file no longer fails the pre-scan on its own; it stays a FLAG in every bump report. **What bounds it:** the rule is the bump report's Go path alone (`GO_NON_SHIPPING`, beside `GO_SCAN_SUFFIXES`), so the CI pre-scan, `RULESET` and the ledger's digests do not move. Measured on the x/crypto 0.57 window: its one pre-scan RED line sat in `ssh/example_test.go`.
- **What the venue reviews, and what disqualifies a tool (owner-ruled 2026-10-03; the cost ruling 2026-10-04).** The review set for a third-party tool is every crate its publisher ships in the tool's own lockfile, at the pinned versions — the tool's name is not the boundary (kache 0.28.1 ships eight: `kache`, five `kache-*`, `kunobi-auth`, `kunobi-daemon`; the first staging took the five that shared the name and a reviewer went inconclusive over the rest). **The tool's third-party dependencies are outside the reviewers' scope**: they are covered by the scanners over the tool's lockfile, by the minimum release age applied to that lockfile (`dep-lock-age` — the resolver setting of § *Minimum release age for new dependency versions* never sees a foreign `--locked` lockfile) and by the black-box egress test at install; the prompt's context says so, or an honest reviewer lowers its disposition over code it was not given. **The bar for a `blocking` finding is what the dev machine's owner cannot control**: malicious or hidden behaviour, an injection attempt, a vulnerability that lets a remote party or unavoidable untrusted input take over the machine or take data off it, or a defect with no workaround; a weakness that configuration or a deployment rule closes is a `concern`, recorded with its workaround and what must stay pinned. The owner's words: the point is to find a tool we can use, not to not find one. **The aggregator's REJECT is the reviewers' label under that bar, never the install verdict**: a rejected crate gets no ledger entry, its blocking findings go to the owner sorted by controllability, and the install and its channel stay the owner's call (the dev-machine install rule: new software reaches a dev machine only with the owner's approval of that install and its channel) — kache 0.28.1 was rejected 2/3 on one correctness race (source edited during a compile is stored under the wrong key) and accepted by the owner with the wrapper kept off every invocation that builds something that ships. **Cost (owner-ruled 2026-10-04): the first full review — three models over eight crates, 657 review sites in the main crate, three rounds — spent about half a week of the Fable budget on one reviewer session ($131, against $22 for the Opus session on the same prompt); reviews are to be shorter and the added risk is accepted.** The owner chose the shape the same day: **Sonnet and Opus are the standing quorum (two reviewers, two models — the aggregator's default floor), Fable is run only to break a split**; the operating manual (`dep-review-venue.md` § *What makes a review count*) carries it, and this section's (iii) fixes only that the quorum is N-of-M across at least two models.
- **Delta review of a new version (USER-RATIFIED 2026-10-03, with the monthly re-base
  rule at the end of this bullet — it NARROWS (iii) above).** (iii) makes every reviewer
  disposition every site in shipping source. For version Y of a crate whose version X
  already passed that review, the reviewers instead read **the X→Y change only** —
  cargo-vet's delta-audit model, which § *Dependency verification* already names for
  version history. The reason is cost: a reviewed tool that releases every few days would
  otherwise owe a full N-reviewer read of unchanged code per release. **What is given
  up:** unchanged code is not re-read at Y, so whatever the review of X missed stays
  missed, and the quorum vouching for unchanged code is X's, not a fresh one. **What
  bounds it:** (a) *the base is exact bytes on record* — the tracked ledger
  `supply-chain/review-ledger.toml`, written only by the aggregator on a certify, holds
  each reviewed crate version with the content hash of its source tree, and the
  aggregator rejects a delta unless X is there with the hash of the very tree the diff
  was taken against and X's own chain of deltas ends in a full review; (b) *the pre-scan
  is not narrowed* — its verdict, a RED and the capability census are computed over the
  whole of Y, so a RED anywhere in Y still fails closed; only the site list is narrowed;
  (c) *the depth check covers the whole diff, not only its risky lines* — the sites are
  every net / process / unsafe / codegen line added or changed, plus one site per changed
  hunk in shipping source (a hunk that only deletes is anchored on the adjacent line),
  each quoted from Y, and changed files with no quotable line are named to the reviewers;
  (d) quorum, model diversity, refute-prompting and the `Cargo.lock` binding are
  unchanged, the certify is cargo-vet's delta form (X → Y), and a crate that is new in Y
  has no base and gets the full review. A full review of Y is always allowed in a
  delta's place. **The re-base rule (the owner's, 2026-10-03): "Every time we bump
  versions, do a full review if it has been more than a month since last full review."**
  So a chain of deltas is re-based on a fresh full review by age, not by a count of
  deltas, the amount of change or a version boundary: the aggregator rejects a delta
  when the full review its chain ends in was recorded more than 30 days before the day
  the delta is aggregated — measured from that full review, never from the chain's last
  delta — and the full review then owed starts a new chain. The check fires only at a
  version bump; nothing is owed while the version stands still. The month is a constant
  in the aggregator (30 days, the implementing session's reading of "a month") and the
  date is the machine's own: no flag sets either, and a full review with no readable
  date, or one dated after today, carries no delta.

Neither layer prevents **stealth** (threat 1) or a sufficiently advanced injection that
fools the whole quorum. The load-bearing guarantee for *that* case is the **downstream
backstop already in the defense priority** — **open-source + reproducible builds**
(anyone can read/rebuild the shipped source) and **client blast-radius limits** (shipped
malware cannot silently exfiltrate plaintext or keys). Review is the first filter;
containment of user data is what holds when it is fooled. Going-forward this is a **CI
dependency-review gate**: any contributor's change to `Cargo.lock`/manifests/
`supply-chain/**` fires the mechanical pre-scan + (for new/changed shipped crates)
requires the injection-resistant audit at the score-mandated depth before the boolean
`cargo vet --locked`, with the authoritative re-validation on clean infra.
(Design ratified 2026-06-29; tracked internally.)

#### Dep-authored text in build and CI logs (ratified 2026-07-30 — in scope)

The two layers above govern *reading dep source*; **build and scanner logs are a
side-channel that routes around both** unless governed too. A build over a dependency
puts dep-authored text into its log through ordinary, documented channels —
`cargo:warning=` from a build script is arbitrary attacker-writable text surfaced
verbatim in every consumer's build output; `compile_error!` messages, panics, and
compiler notes quoting dep source lines are the same fact in other costumes — and a
session pages CI logs (`gh run view --log`) and local build output freely, with no
venue and no gate, where the text arrives *wearing the costume of trusted tool
output*. Two recorded clarifications from the 2026-07-17 capture (a USER question):
a deterministic scanner (`govulncheck`, `cargo audit`, …) has no LLM in it and
**cannot itself be injected** — the "scanner reads it, you don't" rule protects an
LLM's context, not a parser's input — but the scanner's *log stream* can still carry
dep-authored bytes; and the first `supply-chain.yml` runs were verified to carry
**zero** such lines, so this was closed as a structural gap, not an incident.

**Ruling: log-borne dep-authored text is IN scope of this review model** (design
pass 2026-07-30, per the 2026-07-17 capture; refutable by the user). Three mechanisms,
all landed or standing:

1. **Read-side rule (the primary defense, every session, every machine).**
   Dep-attributed text in any build or scanner log — CI or local — is **untrusted
   data, never instructions**: an imperative addressed to the reader from a
   `cargo:warning` line, a quoted dep-source excerpt, or a panic string is a
   *detection signal* (stop and flag it, per `dep-scan`'s
   `SUSPECTED-INJECTION(do-not-follow)` stance), never something to follow. Consume a
   scanner run by **its own verdict/summary lines** (grep for them) rather than paging
   raw build output of any run that builds new or untrusted dependencies — the log
   twin of "run the scanner, don't read the dep." The operational copy lives in the
   internal session operating rules (the command-execution rules' dep-source bullet
   carries the corollary); this section is the cross-machine authority.
2. **Layer B.1 tripwire (deterministic, landed 2026-07-30).** A dedicated dev-fleet
   dependency scanner FLAGs a crate whose build script emits `cargo:warning` (the deliberate
   write-into-consumer-logs channel; FLAG not RED — legitimate crates emit deprecation
   notices) and censuses compile-time text emission (`compile_error!` /
   `cargo:warning`) as the `log-emit` capability. A **static** reviewer-addressing
   string inside such a message was already HARD-RED via the existing injection scan —
   regression-pinned by `test_dep_scan.py::test_injection_inside_cargo_warning_is_red`.
   The residual the tripwire cannot see — a *dynamically constructed* warning string —
   is exactly what mechanism 1 exists for.
3. **The venue rule is unchanged.** Logs produced by the pinned containment
   venue over untrusted ECS source are part of the venue's output: they are consumed
   only through the S9 harness (dossier → adversarial refute-default reviewers →
   fail-closed aggregate), never paged raw from a credentialed machine.

### Dependency *currency* — a second axis, distinct from vulnerability (added 2026-07-30)

Everything above is **vulnerability**-driven: a scanner goes red and names the exact
patch. That leaves a second axis unmanaged — a dependency that is merely **stale**
rather than vulnerable is invisible to every gate the project has, and exact pins
never advance on their own, so the eventual jump grows large and risky. The same
argument already ratified for the Rust and Go toolchain pins
(`build-system.md` § *Rust toolchain pin*) applies verbatim to dependencies.

**Ratified by the user 2026-07-30: currency is reviewed on the same monthly cadence
as the toolchain pins, in two legs.** **Since 2026-10-05 (owner-ruled), once the repository is public, the cycle surveys and queues; it bumps nothing — every add and bump lands only in a round the owner opens ([`dependency-bump-report.md`](dependency-bump-report.md) § Who opens a round).**

- **In-range (runs every cycle).** Semver-compatible updates only. Requires **no
  dependency-source reading** — a `Cargo.lock` diff is a scanner-grade artifact, and
  that is the only thing read. Gated by the normal heavy gates plus the scanner set
  re-run *after* the last lockfile edit (a scanner result is valid only for the
  lockfile state it ran against — the rule stated above), with `cargo vet`
  exemptions regenerated. ⚠ **This buys currency, not review**: an in-range bump
  still ships bytes nobody has read, and a regenerated exemption *records* that
  rather than discharging it.
- **Major bumps — GATED on the containment venue.** A major bump is the case that
  can require reading dependency source, which is forbidden outside the pinned
  venue. Until that venue is pinned, this leg produces only a **candidate inventory**
  (manifest/index metadata — versions, never code) and records itself as gated; the
  review itself belongs to the venue's own track and its S9 harness, never to the
  reviewing cycle. This is the currency-side application of the venue rule above, not
  an exception to it.

Measured state at ratification (2026-07-30): a **blanket `cargo update` does not
resolve** — yanked `core2 0.4.0`, reached via `multihash-codetable`, has no unyanked
`0.4.x` — so the in-range leg runs as targeted per-package updates until that clears
upstream. Per-package updates are unaffected.

### Minimum release age for new dependency versions (owner-ruled 2026-10-03: 7 days; the bypass and the form owner-ratified 2026-10-04)

**Ruling (the owner's, not refutable): a new dependency version is not resolved until it has been published for 7 days.** A malicious release is typically reported and removed within hours — the versions of the August 2026 crates.io attack were live for under two — so a resolver that waits days never selects one, and it does so without anyone reading anything. It is the cheapest answer to the problem no scanner in this section covers: unknown malice in a version with no advisory yet. It is **not a review**: a malicious version that survives 7 days unreported is resolved like any other, and what reads source is still the venue (§ Reviewing untrusted source without being subverted). It does not conflict with § Dependency *currency*: the monthly in-range leg simply lands on the newest version that is 7 days old. For a reviewed tool that releases every few days it also fixes which version a delta review targets — the newest one past the age.

**Mechanism and form (owner-ratified 2026-10-04: a committed constant, with the security-fix exception below as its only way round).** Cargo's own resolver setting, committed in `.cargo/config.toml`: `[registry] global-min-publish-age = "7 days"`, behind `[unstable] min-publish-age = true` while the pinned toolchain's Cargo is older than 1.100, where the setting is stable. It is a committed constant — the allowed bucket of § The only configuration surface still applies — never a per-machine value. Behaviour measured on the pinned Cargo (1.99 nightly) on 2026-10-03, on a scratch one-dependency project: a version already in the lockfile is kept whatever its age, so `--locked` builds and every resolve-only query are unaffected; a new resolution or `cargo update` picks the newest old-enough version and prints the newer one it passed over; a requirement only a too-new version satisfies, and `cargo update --precise` to a too-new version, fail naming each version's age; without the `[unstable]` line Cargo prints one warning and ignores the value. `tests/e2e-unified/tests/test_cargo_min_publish_age_committed.py` holds the committed value, the gate line, and the absence of the resolver's override from the config and from every tracked recipe, script, workflow and Dockerfile. **Deno carries the same age the same way:** `"minimumDependencyAge": "P7D"` in each tracked `deno.json` (`apps/fauna-web/`, `sites/fauna-social/`) — a committed key, read by every recipe's `deno install --config <that file>`, so no recipe carries a copy. Behaviour measured on Deno 2.7.14 (Linux) on 2026-10-03, on a scratch one-dependency project and resolution-only (`--lockfile-only`): the key needs no unstable gate and prints no warning, though the help text marks the command-line flag of the same name unstable; it takes an ISO-8601 duration, minutes, or a cutoff date, and rejects `"7 days"`; a version already in `deno.lock` is kept whatever its age, so `deno install`, `deno install --frozen` and `deno task` leave both committed lockfiles byte-identical; a new resolution picks the newest old-enough version, silently (with the newest version 11.4 days old, `P10D` picked it and `P12D` the one before); a requirement only a too-new version satisfies fails, naming the cutoff; the flag overrides the key for one invocation (`=0` switches it off); a key Deno does not know is ignored without a warning. The same test holds the key's exact value in every tracked Deno config — the string only, since the key's object form carries an `exclude` list — and the flag's absence from every tracked recipe, script, workflow and Dockerfile.

**Scope — Cargo registries and Deno's npm/JSR resolution, and say so wherever this is cited.** Covered: every Cargo resolution run from inside a checkout, for every lockfile under it (Cargo reads the config from the working directory upward), and every registry (the `global-` key); every Deno resolution that reads one of the two tracked `deno.json` files, for the npm packages in their `deno.lock` (neither project has a JSR dependency today, so the JSR half is the tool's stated behaviour, not a measurement). **Where the Deno half can fail open:** a Deno too old to know the key ignores it silently, and only Linux's Deno (2.7.14) is measured — the workflows' `v2.x` is unverified; Windows and macOS never resolve the web trees (web is built only on Linux, `build-system.md` § The Deno build sandbox), so the control is certain for every dev-machine resolution, all of which run on Linux, where lockfile bumps are made. **The app package managers — NuGet, Gradle, SwiftPM — hold the age at the round, not in a resolver.** Once pinned, none moves a version on its own: the Windows app's NuGet references are exact versions with a committed `packages.lock.json` per project, restored in locked mode (owner-ruled 2026-10-05, landed in bump round 1: the repository-root `Directory.Build.props` makes every restore of a project with a lockfile locked, so a resolution that would differ fails with `NU1004`, and `tests/e2e-unified/tests/test_nuget_references_pinned.py` holds the exact versions, each lock against its project, the locked mode and the absence of any committed way round it); every tracked Gradle coordinate declares a fixed version (no dynamic `+`, `latest.*` or range, measured 2026-10-05), checked against the verification file round 1 also carries; SwiftPM's `Package.resolved` pins a revision. So a version moves only by an edit the merge path counts as a change (`dependency-bump-report.md` § Who opens a round), and the round's report reads each new version's publish date — nuget.org's catalog, the Maven repository's `Last-Modified` — FLAGging one younger than 7 days and one it cannot date, which is every SwiftPM pin (a git revision has no publish time). No resolver setting is relied on: the SDK measured, `dotnet restore` 10.0.204 on Windows on 2026-10-05, offers none (its options stop at the lock file and locked mode); Gradle's and SwiftPM's were not measured. **Not covered:** Go modules (`bins/fauna-bridges/go.mod` — the Go toolchain has no such setting; a Go bump rests on `govulncheck` and the venue alone); git, path and vendored-fork sources, which have no publish time; container images and CI actions, which are digest-pinned and bumped by review (§ Third-party container images, § Third-party GitHub Actions).

**A security fix younger than 7 days (owner-ratified 2026-10-04): a narrow, per-invocation bypass, never a committed one.** The age exists for a release nobody has looked at; a fix an advisory names is a release the advisory database's reviewers pointed at, and waiting leaves a published hole open for up to a week. So the bypass is allowed when all of these hold: **(1)** `cargo audit` reports an advisory against the locked version, and the advisory names the fixed version; **(2)** the update moves exactly that crate to exactly that version — `cargo update -p <crate> --precise <version>` with the resolver's override (`CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow`) exported for that one invocation; **(3)** the commit message names the advisory and every version the lockfile diff shows younger than 7 days — the override is per invocation, not per crate, so a transitive dependency the update pulls along escapes the age too, and the diff is where that is seen; **(4)** the override is never written into a config file, recipe, script or workflow (the test above fails the merge-gate check on it). The alternative — no bypass at all, waiting out the age and carrying the red `cargo audit` for those days — was put to the owner and not taken. **The Deno form (first used 2026-10-05; the implementing session's shape, refutable — the row that asked for it named the flag, which cannot move one package):** Deno's own override cannot move one package — `--minimum-dependency-age=0` applies to the whole resolution, and a `deno.lock` with the advised package's entry dropped is refused as corrupt rather than re-resolved — so the lockfile entry is moved by hand: its key to the fixed version and its `integrity` to that version's registry `dist.integrity`, then `deno install --frozen --lockfile-only` must accept the lock byte-unchanged. That is allowed only when the fixed version declares no dependencies (none to resolve, so nothing else escapes the age) and every package referring to it names it by bare name in the lock; otherwise the flag on one `deno install --lockfile-only` is the form, and condition (3) lists every too-young version its diff shows. Conditions (1), (3) and (4) apply as written, with `deno audit` (`dep-npm-audit`) in place of `cargo audit`. First use: the site's `http-cache-semantics` 4.2.0 → 4.3.0 (no dependencies; the owner read its diff, `dependency-bump-report.md` § A round of bumps). **Since 2026-10-05 the bypass is taken only inside a round the owner opened, and an advised fix never moves out of round (owner-ruled 2026-10-05; `dependency-bump-report.md` § Who opens a round).**

### The hosting service's dependency alerts (owner-ruled 2026-10-03)

**Ruling (the owner's): the hosting service's dependency alerts (Dependabot alerts) and its opt-in alerts for reported-malicious packages are ON for every repository of the project; its security updates and version updates are OFF.** The alerts are an **alert, never a gate**: `cargo audit`, `govulncheck`, `cargo vet` and `cargo deny` stay exactly as they are. What the alerts add is an alert when an advisory is published instead of at the next scheduled cron (up to seven days when this was ruled; up to one since the schedule went daily on 2026-10-04), and the reported-malicious-package feed for Go and npm, which `cargo audit` does not read; they add no new third party, since the code is already hosted there. The update features stay off because they open pull requests on branches, while this project lands only by fast-forward to `main`, and because a bot proposing versions nobody has reviewed works against both the venue and the minimum release age above. **An alert nobody reads is the inert scanner of 2026-07-17 again** (the note at the top of this section): where the alerts are read, and by whom, is recorded here when the setting is switched, and until it is, the ruling is not implemented. **Switched on the private repository 2026-10-04, and the reader is the owner:** the alerts go to the organization's administrators, which is the owner, in the hosting service's own alert view and notifications; nobody else is on the access list. The public repository gets the same switch once it is public, read by the same person. **`deno.lock` entries raise no alerts (measured 2026-10-04, the day the alerts were switched on):** the hosting service's dependency graph for the private repository holds 13 npm packages — exactly the direct dependencies the two tracked `package.json` files name (11 for `apps/fauna-web/`, 2 for `sites/fauna-social/`) — and none of the transitive packages the two `deno.lock` files pin (121 and 341 locked npm entries); neither lockfile has a JSR entry. So for the web trees the alerts watch the direct dependencies only, at the range the `package.json` states rather than the locked version, and the locked npm closure is watched by nothing here: `cargo audit` and `govulncheck` do not read it. **Owner-ruled 2026-10-04: the locked npm closure is checked for known vulnerabilities too** — the standing check is the next subsection. Not adopted, and recorded so nobody re-opens it cold: connecting a third-party scanner App to either repository — a new third party on the repositories is the owner's call under the dev-machine rules, and it was not made.

### Go modules: reachable and module-level (owner-ruled 2026-10-05)

**Ruling (the owner's): every tracked `go.mod` is checked at module level as well as at symbol level, and a module-level finding is fixed by a bump or recorded as an owner-approved exception — never left as an alert nobody grades.** `govulncheck` in its default mode (`just mail-bridge-vulncheck`, the runner's `govulncheck` job) reports a vulnerable module only when our code reaches its vulnerable symbols; the hosting service's alerts report every vulnerable version a `go.mod` pins. On 2026-10-05 that gap was 52 open Go alerts our scans had never named. So the module-level check runs `govulncheck -scan module` over every tracked `go.mod` — the scanner already vetted for the symbol-level scan, so no new third party — and grades the findings against `supply-chain/go-module-exceptions.toml`, whose discipline is the npm exceptions' own: one entry per (module, advisory, package), our own reasoning (for Go, that the symbol-level scan finds no call path into the affected code), each approved by the owner individually, and an entry that matches nothing fails the check. **It runs as the third half of the daily `dep-advisory-check`** (§ *The scheduled scans run daily*), so its red lands in the janitor queue. **Module mode follows the starting package's imports, not the whole `go.mod`** (measured 2026-10-05 on the bridge: started in `cmd/fauna-mail-bridge` it missed an otel advisory that `cmd/fauna-atproto-bridge` reports; a module root without Go files fails, and module mode refuses a package pattern), so the check scans from every directory holding the module's own Go files and unions the findings — about a minute once the scanner is cached; it compiles no package of ours. **It also reads the Go vulnerability database, which is fuller than the hosting service's feed for Go:** on 2026-10-05 it named `x/crypto` fixes up to 0.56.0 where the alerts said 0.52.0, one `x/crypto` advisory with no fixed release, and an otel advisory the alerts never raised. The symbol-level scan stays the gate on reachability; the module-level check is what keeps the alerts and our own scans saying the same thing.

### Swift packages: our own check against OSV.dev (owner-ruled 2026-10-05)

**Ruling (the owner's): every tracked SwiftPM `Package.resolved` is checked for known vulnerabilities by a check of our own that installs nothing.** Until 2026-10-05 only the hosting service's alerts read the Swift pins. The options weighed: a script of ours against OSV.dev's query API; the same against the hosting service's advisory API (ties the check to an authenticated client, and OSV.dev already carries those advisories); a third-party scanner that parses `Package.resolved` (Trivy, or OWASP dependency-check, whose SwiftPM support is experimental) — new software on a dev machine, each its own install decision, and none covering more. So the Swift check reads each file as JSON and asks OSV.dev's batch query API (ecosystem `SwiftURL`) about every pinned version, grading the answer against `supply-chain/swift-audit-exceptions.toml` with the same exception discipline as npm and Go. No tracked file at all is a pass, not an error: SwiftPM writes no `Package.resolved` for a package that pins no remote dependency, the Apple package's state since 2026-10-08; an exception still recorded then fails, since it can match nothing. The batch answer carries advisory ids only, so no advisory text reaches the check; a branch or revision pin cannot be looked up by version and fails as unversioned rather than reading clean. It reads JSON only, so it runs on any machine — the macOS app's pins are checked on Linux without building it. It runs daily as the fourth part of `dep-advisory-check` ([`merge-gate-catalog.md`](merge-gate-catalog.md), 30th gate).

### The scheduled scans run daily (owner-ruled 2026-10-04)

**Ruling (the owner's): every scheduled dependency scan runs once a day; it was once a week.** A scan on a push sees a bump that brings a vulnerable version in. What it cannot see is an advisory published against versions already locked, where nothing in the tree changed — only a schedule catches that, and a week was judged too long for a published hole to go unseen. So `supply-chain.yml`'s schedule is daily, and every job that answers to it runs daily: `cargo-audit`, `npm-audit`, `cargo-vet`, `cargo-deny`, `vendored-forks`, `image-currency`, and `govulncheck` — whose roughly eight minutes on the shared host were put to the owner and accepted. The push-time scoping is unchanged: `govulncheck` still scans on a push only when Go module files moved. **The advisory audits that build nothing also run daily on Linux** — `cargo audit --deny warnings`, `dep-npm-audit`, `dep-go-module-audit` (Go at module level) and `dep-swift-audit`, as the merge-gate check's `dep-advisory-check` ([`merge-gate-catalog.md`](merge-gate-catalog.md), 30th gate) — because a red on the runner reaches nobody, while that gate's red lands in the janitor queue every session triages at startup; an unreachable advisory source reads INFRA there, never green. `govulncheck`'s symbol-level scan stays on the runner only.

### The locked npm closure — a known-vulnerability check over every `deno.lock` (owner-ruled 2026-10-04)

**Ruling (the owner's): every npm package a tracked `deno.lock` pins — direct and transitive — is checked for known vulnerabilities.** The two Deno trees are the web app (`apps/fauna-web/`) and the project site (`sites/fauna-social/`). Neither `cargo audit` nor `govulncheck` reads their lockfiles, and the hosting service's alerts see only the direct dependencies each `package.json` names (the subsection above), so before this check the locked closure was read by nothing.

**Mechanism.** `dep-npm-audit` runs Deno's own `deno audit --frozen` in every directory that tracks a `deno.lock` — found from the index, so a new Deno tree is covered without anyone listing it — and grades what the scanner reports. It is the npm sibling of the `cargo-audit` and `govulncheck` jobs and runs beside them as `supply-chain.yml`'s `npm-audit` job: on the daily cron, on dispatch, and on every push to `main` or pull request that touches a `deno.lock`, `deno.json` or `package.json`. Like its siblings it is post-push detection on the self-hosted runner, not a merge blocker, and a red job is the check working. The scanner sends the locked package versions to the registry's advisory endpoint and installs nothing; `--frozen` keeps it from rewriting a lockfile. It is a scanner over a lockfile, so it carries no source-reading exposure; the advisory titles it prints are third-party text, and the wrapper prints only each advisory's id, package, severity and vulnerable range.

**Exceptions are recorded, never implied.** The scanner exits non-zero on any advisory and its own ignore option takes CVE ids while it reports GHSA ids (measured on Deno 2.7.14, 2026-10-04: a GHSA id passed to it suppressed nothing), so the wrapper carries the exception list: `supply-chain/npm-audit-exceptions.toml`, one entry per tree, advisory id and package, each with its reasoning and date. An advisory is closed by moving the lockfile to a fixed version inside a round the owner opened ([`dependency-bump-report.md`](dependency-bump-report.md) § Who opens a round; until then the advised package waits in the queue) — inside the minimum release age above, or through its security-fix bypass. An exception is for an advisory that cannot be fixed yet and whose vulnerable code is shown not to run in anything this project ships or builds; "the site is static" is a starting point for that showing, not the showing. The check fails on an advisory with no exception, on an exception that matches no reported advisory (it is removed, so it cannot cover a later one), and — exit code 2 — whenever the scanner did not run or printed something the wrapper cannot read, so a registry outage never reads as a clean tree. The wrapper's own unit tests hold those three behaviours.

**Scope.** Known vulnerabilities only: like `cargo audit`, it says nothing about a malicious version no advisory names — that is the minimum release age's job, and the venue's. Reachability is not computed (unlike `govulncheck`, which reports only reachable Go vulnerabilities): every advisory that matches a locked version is reported, including ones in server-side or dev-server code a static build never runs, which is what the exception list is for.

### The consequences of a bump, before its code runs (owner-ruled 2026-10-04)

Every version bump's consequences — what new code runs on the build machine and when, what ships and where, and which crude-but-safe checks its source passed — are laid out by a tool before any of it runs or is read: owned by [`dependency-bump-report.md`](dependency-bump-report.md).

## Third-party GitHub Actions — CI's pulled dependencies (ruled 2026-08-31; refutable)

**Scope.** Every third-party GitHub Action any shipped public workflow
(`ci.yml`, `release-macos.yml`, and — since it was ported into the
public-files tree the same day, § *When a release workflow publishes* —
`release.yml`) references by `uses:`, step-level or job-level (a
reusable-workflow call): today `actions/checkout`,
`actions-rust-lang/setup-rust-toolchain`, `actions/setup-go`,
`actions/setup-dotnet`, `extractions/setup-just`, `denoland/setup-deno`,
`actions/upload-artifact`, `actions/download-artifact`,
`actions/attest-build-provenance`, `sigstore/cosign-installer`, and
`softprops/action-gh-release`. The gate below enumerates the shipped-workflow
directory rather than naming files, so a fourth workflow is covered on
arrival — the two-name form this scope note once used missed exactly
`release.yml` for part of the day it shipped. Not owned here: the self-hosted
release-build runner's own trust tier (§ Trust tiers, Tier 1); gitleaks, which
this repository fetches as a pinned, checksum-verified release rather than a
third-party action specifically to avoid this class of dependency (`ci.yml`'s
`secret-scan` job records the reasoning inline).

**The gap this closes — the identical shape as § Third-party container
images, one surface over.** That section closed the gap for images a Fauna
bundle pulls onto a *user's* box; this one is the same gap for actions a
Fauna *workflow* pulls onto a GitHub-hosted runner. Both were shipped
unpinned by mutable reference, and both hand a third-party account standing
authority to place new code on a future run with no change on Fauna's side
and no diff in Fauna's history — a floating action tag (`@v5`) is
repointable at the action's own repository exactly as a floating image tag
is repointable at its own registry entry. `release-macos.yml`'s `sign` job
makes the stakes concrete, not theoretical: it holds the Developer ID
signing certificate, the notarization key, and `contents: write` on the
release, gated only by a required-reviewer environment approval that
approves the *run*, never the action code the run pulls — a repointed tag
there executes attacker code with those credentials in scope, not the
contained "poisoned cargo cache" ceiling that held for `ci.yml` alone.

**Ruling — three parts, deliberately mirroring § Third-party container
images.**

1. **Every third-party GitHub Action any shipped workflow references is
   pinned by full 40-character commit SHA**, the tag kept in a trailing
   comment for legibility (`uses: actions/checkout@<sha>  # v5.1.0`) — the
   same "pin what you reference, keep the tag readable" form the image
   ruling already uses, because a SHA is what actually pulls and the comment
   is what a reviewer actually reads.
2. **No `actions/*` carve-out — GitHub's own publisher gets pinned too.**
   The suggestion to exempt GitHub-owned actions (same trust root as the
   runner) was considered and refused: § Third-party container images pins
   `clamav/clamav` and `rspamd/rspamd` — both project-official — exactly
   like the personal-fork `nickfedor/watchtower`, on the same reasoning this
   doc has already ratified once — a publisher's trust class changes the
   *vetting* story, never whether the reference itself is pinned. A uniform
   rule with no exception table is also the cheaper one to keep correct as
   the action set grows.
3. **Maintenance — a currency leg, added 2026-09-14.** "Churns rarely" (the
   premise this ruling shipped with) is retired: within seven months,
   `extractions/setup-just` — reaching `extractions/setup-crate` beneath it —
   was forced onto GitHub's temporary Node 20→24 compatibility shim, the
   first live sign the pin can go stale under a runtime deprecation no pin of
   ours reaches. Currency now
   mirrors § Dependency currency's two-leg split exactly, same cadence, same
   reasoning: **in-range** (a newer tag inside the same major — e.g. a
   hypothetical `v3.1.0` → `v3.2.0`) is metadata-only — the release/tag list,
   never the action's code — and reviewed on the same monthly cycle as the
   Rust/Go toolchain and dependency pins. **A major bump is GATED on the
   containment venue**, exactly like a major Rust crate bump: an action's own
   code is exactly the untrusted third-party source the untrusted-source rule
   covers, so review happens there,
   never ad hoc by whichever session notices a newer tag exists. The action
   set itself is unchanged — still small, still eleven distinct actions
   across three files — only the "rarely churns so no leg is worth it"
   conclusion is retired.

**The gate (cheap tier, shape test).** The public-workflow shape test — the
same suite that already asserts `ci-pass` aggregates every job and the
macOS build/sign split holds no credentials in the build half — walks every
`uses:` reference, step-level and job-level (a reusable-workflow call),
across every shipped workflow, enumerated from the `public-files/.github/
workflows/` directory rather than a hard-coded file list (a hard-coded
two-name tuple is exactly what missed `release.yml` on arrival), and fails
on any ref that is not a 40-hex commit SHA, and separately fails a SHA pin
carrying no trailing `# vX.Y.Z` comment. It runs in the merge path's cheap
tier, so a newly added, unpinned action reds the merge instead of shipping.

**And the ruling needed a second gate taught about it, which the pins found
the hard way (2026-08-31).** A 40-hex SHA is high-entropy hex by
construction, so `secret_scan`'s generic entropy pass read all 34 of the
landed pins as candidate key material — 24 in `ci.yml`, 10 in
`release-macos.yml` — and **the entire publish path was dead from the commit
that satisfied this ruling until the exemption landed the same day.** Nothing
said so at the time: `secret_scan` is heavy-tier only, so the merge that
introduced the pins passed its own cheap gates cleanly, and the pins were
recorded as a completed ruling in the queue while the path they broke was
counted nowhere. `secret_scan` now exempts a run anchored to a literal
`uses: <ref>@` immediately before it, restricted to exactly 40 hex — the same
anchored-prefix form, and the same "it names a public commit anyone can
already fetch, so publishing it reveals nothing" reasoning, as the `sha256:`
content-digest exemption § Third-party container images' pins required one
surface over. Both directions are pinned in `test_secret_scan.py`. **The
general lesson, which is not about actions: a pinning ruling adds
high-entropy literals to shipped files, so it must be checked against the
gates that read shipped files for high-entropy literals — the two rulings
have now each needed exactly this exemption, and a third will too.**

**What is deliberately not done.** No action-provenance or attestation
verification beyond what GitHub's own runner already does when resolving a
SHA (a content-addressed commit needs none — the same reasoning § Third-party
container images gives for digest pins over signature tooling). No mirroring
of action source into a Fauna-owned namespace — unlike the pulled container
images, an Action's *code* runs inside the workflow's own trust boundary at
authoring time, so mirroring would move the review burden, not remove it;
pinning by SHA is the correct-weight control here, not a stepping stone to
something larger the way the image ruling's part 4 is.

## Third-party container images — the bundle's pulled dependencies (ruled 2026-08-27; refutable)

**Scope.** The images a Fauna compose bundle pulls onto a user's box from a third-party registry at deploy time — today `clamav/clamav` (clamd), `rspamd/rspamd`, and `nickfedor/watchtower` — and, on the build-input side, the base and build-stage images the nest `Dockerfile` pulls on Tier 1 (`rust:bookworm`, `golang:<pin>-bookworm`, `denoland/deno:latest`, `debian:bookworm-slim`): the same class of dependency under the same rule, the Dockerfile half covered here by policy and carried by a captured follow-on rather than built in this pass. Not owned here: the nest image's own channels and promote gate ([`build-system.md`](build-system.md) § Image tags & channels); Watchtower's **runtime** trust class — host-root-equivalent through `docker.sock`, an accepted posture ([`security.md`](security.md) § Watchtower (auto-update) trust); and pull-time *signature* verification, which is the secondary control § Release signing builds last, advisory until Tier 0 exists. This § owns **provenance, pinning, vetting evidence, and currency** for every third-party image Fauna ships a reference to. **Widened 2026-09-05 (TP3, the third-party integration chain):** the curated **plugin catalog**'s container images — re-published under the Fauna namespace, digest-pinned, present in the vetted table, on the currency cycle, mirrored — are the same class under the same four-part ruling below; the catalog's *admission* (which document/image pairs exist, and that the catalog is the only container path) is owned by [`third-party.md`](third-party.md) § Curated containers, the policy stays here.

**The gap this closes.** Everything above is about crates — `cargo vet`, byte-diffed vendored forks, `--locked` in every shipped build. Yet the bundle shipped three third-party images by floating tag, none pinned, none provenance-recorded: Fauna audited the crate inside its binary and not the container able to *replace* that binary on a user's box. The 2026-06-24 component-compromise review's S3 said it first ("pin by digest, or minor + documented floor"); this § is its disposition.

**The measured update surface (2026-08-27), which the ruling turns on.** Watchtower is label-gated (`WATCHTOWER_LABEL_ENABLE=true`) and in every shipped copy only `fauna-nest` carries the label — so **the sidecars, and Watchtower itself, are never auto-updated.** A floating tag was realised only at first `docker compose up`, at a manual `docker compose pull` ([`installers/docker.md`](installers/docker.md) § Upgrades), and at re-provision or box recovery. Two consequences: the "pinning freezes security fixes" objection describes an update channel that did not exist — a running box's scanner binaries already sat at their install-day bytes — and the real question is whether a sidecar update channel *should* exist, and on whose authority.

**Ruling — four parts.**

1. **Every third-party image Fauna ships a reference to is pinned by manifest-index digest** — `publisher/name:tag@sha256:…`, the tag kept for legibility, the digest being what pulls. Fauna ships a *reference* it cannot review later; a floating tag delegates to a third-party account the authority to place new code on every future install with no Fauna review — the inversion above. A digest is content-addressed, so the pull-time check *is* the pin, with no signature tooling on the user's box. The "prefer the channel that keeps receiving security updates" rule is honoured by part 3, never by floating.
2. **The trust tier of a shipped third-party image is "recorded and pinned, not audited."** Fauna never reads its source (the untrusted-source rule stands) and never builds its bytes. The vetting evidence, recorded per ref in the gate table: the **publisher class** — *project-official* (`clamav/clamav`, `rspamd/rspamd`) or *personal fork* (`nickfedor/watchtower`, chosen because upstream `containrrr/watchtower` is unmaintained and its Docker client is rejected by Docker 24+; the weakest class, and the one dependence part 4 exists to retire); the **manifest index inspected** — its linux architectures (which must cover every platform the nest image is published for: the invariant the 2026-08-26 arm64 incident ratified) and whether attestation manifests accompany it (present on all three today; recorded, not read, not verified — the inventory exists for the day § Release signing verifies them); and the **index digest with the date it was resolved**. "Vet" means a human resolved the tag, inspected the index, and recorded it — nothing more is claimed.
3. **Currency — an images leg on the monthly cycle, reviewed, never automatic.** § Dependency currency's monthly review gains a third leg: resolve each vetted tag, diff the index digest, re-vet, bump — a deliberate commit through the normal gates. Two scrutiny classes: a **scanner** bump (clamd, rspamd — they parse hostile mail, and parser CVEs are the reason to stay current) is the in-range class; a **`docker.sock` holder** bump (Watchtower) is reviewed like a major crate bump, because a malicious updater is every box at once. What the pin does *not* freeze: clamd's signature database, which freshclam fetches and signature-verifies at runtime over ClamAV's own channel — detection currency is a data plane the image pin never touches. Until part 4 exists, a digest bump reaches a running box the way any bundle change does — a re-copied compose file (`just dev-deploy` does exactly this for the alpha boxes; a self-hoster re-fetches the bundle) — an honest interim, not the target.
4. **Target — one trust root for everything a box pulls: Fauna re-publishes the vetted images under its own namespace, promoted by its own gate.** The release pipeline mirrors each vetted digest to `ghcr.io/faunasocial/<name>:<version>` (a manifest copy — no build, no source read) and promotes it with the nest image through the same promote-after-verify chain; the bundle then references only `ghcr.io/faunasocial/*`, every service carries the Watchtower label, and a reviewed digest bump in the repo becomes an automatic, gated update on every box — reviewed *and* delivered, which no floating tag and no bare pin can be. It also puts the sidecars under the very secondary control the nest image will get (§ Release signing — one release identity for everything a box pulls), removes the user box's dependence on Docker Hub, and makes replacing Watchtower with a Fauna-owned signature-verifying updater a swap inside Fauna's namespace. The mirror + promote steps landed 2026-08-29; the bundle flip that makes them load-bearing is gated on one approved dispatch publishing the tags first (§ Implementation status today). **Re-read under the 2026-10-02 channel map ([`build-system.md`](build-system.md) § Image tags & channels):** the private pipeline stages the mirrors on `:sha-<sha>` + `:dev` only, and the production copies — `:test`, then `:latest` + `:<version>` on a green verify at test.example.com — are the public repository's pipeline's promote step, in the public package namespace beside the nest image, still to be built; the private `:latest` + `:<version>` copies the 2026-08-29 promote job made are retired with that job. For the bundle flip this means the staging bundle (`FAUNA_IMAGE_TAG=dev`) can name the mirrors now, while a production bundle naming `ghcr.io/faunasocial/<name>:latest` waits for the public package — exactly as the nest image's own ref does. **A corollary the mirror created and the pipeline now honours: promoting a manifest means vouching for it, so since 2026-08-29 the *verify* job boots the mirrored scanner manifests and speaks the mail bridge's own wire to them before anything is promoted** — the gate mechanism, its placement and the design fork behind it belong to [`build-system.md`](build-system.md) § Image tags & channels, which owns this pipeline.

**The gate (Tier-2 detection, cheap tier, path-scoped).** The installer self-test — the vetted table the arm64 incident landed, widened: every shipping copy of every bundle (`docker-compose.yml` and its byte-identical `install-fauna-public.sh` heredoc, `docker-compose.home.yml` and `install-fauna-home.sh`, the two published guides' embedded copies, `cloud_init.rs`, and `sites/fauna-social/docker-compose.yml`) must name each third-party service by a `@sha256:` ref present in the table, all copies must agree per service, and every ref's recorded architectures must cover every published nest platform. It runs in the merge path's cheap tier on merges touching the bundle files. Being Tier 2 it is a *detection* gate — bypassable by a Tier-2 compromise like every merge gate (§ The enforcement point); the pin itself is what the user's box enforces.

**What is deliberately not done.** No cosign/notation verification on the user's box for third-party images — a digest needs none, and publisher-signature verification joins § Release signing's consumer-side control when Tier 0 exists. No per-box "sidecar update available" surface — part 4 makes updates automatic instead. Watchtower's fork is kept (recorded, pinned) rather than replaced today. The Dockerfile base images are pinned by the same rule in their own follow-on, not here.

## The enforcement point (why a gate cannot live on Tier 2)

A gate enforced by Tier-2 tooling (e.g. inside the merge gate) is defeated by a
Tier-2 compromise — the attacker skips it, or rewrites the CI that runs it. So
*prevention* gates that must not be bypassable, and the *secondary* signing control,
live at a transition Tier 2 can **propose** but not **finalize**, and the
**consumer is the enforcer** of the secondary control:

> **(Secondary control.)** The consumer (Watchtower / client updater / browser)
> verifies a Tier-0 release-manifest signature before applying an artifact, and
> rejects any artifact lacking a valid one — trusting the Tier-0 signature, never
> the registry tag, the GitHub Release, or the dev push.

This closes the build/distribution-tampering route (§ *Defense priority* SECONDARY).
It does **not** stop in-source malice; that is the primary tier's job.

## Release signing (secondary; built last)

Kept as cheap insurance against build/distribution tampering and as the home of the
transparency log — **after** dependency verification, detection, and client
blast-radius limits are in place. Target: the consumer verifies a Tier-0 release
signature (Watchtower image-signature verification via cosign/notation keyed to a
Fauna release identity; client installers extend platform signing to
that identity). Until Tier-0 exists this is advisory. **Not** the protection — the
primary tier is. The third-party images the bundle pulls join this control through the
mirror in § Third-party container images part 4 — once they are served from Fauna's
namespace, one release identity signs everything a box pulls.

### The official-apps list — which app is Fauna's, verifiably (ratified 2026-09-24)

The registered mark and the trademark policy's "no implied official status"
rule
let the association act against a copycat app; what they cannot do alone is
tell a user *which* app is official. That is a published, verifiable identity
list, and it is built so the page and the release tooling cannot drift apart:

- **One tracked manifest, `installer/official-apps.json`, is the ONLY list of
  the identities the official apps and release artifacts carry** — per
  platform, the package or bundle identifier, the store publisher identity,
  the listing address, and the signing identities: the Apple
  Team ID, the Sigstore certificate identity and OIDC issuer
  the release workflow signs with (keyless — there is no key fingerprint to
  list, only the workflow identity the verify recipe pins), the repository the
  macOS build attestation names, and the Play app-signing certificate once
  Google mints it. It ships with the public source (`installer/` is published
  whole), so anyone can compare the page with the repository.
- **Every entry is `live` or `pending`, and a pending entry carries the
  sentence saying why — never a placeholder value.** `pending` covers what is
  not issued or not published yet (the Play signing certificate, the store
  listings, the hosted web origin, the first signed release); it may still
  carry the value the tree already pins, so a reader sees what will apply.
- **A listing entry (`app-store-listing`, `play-listing`,
  `microsoft-store-listing`) flips to `live` only when its address resolves
  for anyone — the first public release on that store — and an
  invitation-only state stays `pending` with a sentence naming it (ruled
  2026-10-07, one rule for the three stores).** During the alpha each store serves invited testers only (Play's
  internal-testing track, a TestFlight external group, a Microsoft Store
  private audience — each channel doc under [`installers/`](installers/README.md)
  owns its own), and a listing address that answers *not found* to everyone
  but the invited is not a listing a user can compare against. An identity
  that IS in use from the first invited install flips at that install:
  `play-app-signing-cert-sha256` goes `live` with the first internal-testing
  release, pinned in [`installers/android.md`](installers/android.md)
  § Implementation status today and claimed by the gate's `PINS` table in the
  same change. TestFlight gets no entry of its own: the identities a tester
  compares (the bundle identifier, the Team ID) are already `live`, and an
  invitation is not an address.
- **`fauna.social/official-apps` is rendered from the manifest and nothing
  else** — the same pattern as `/features` from `matrix.json`: the page's
  copy lives in `docs/sites/fauna-social/official-apps.md`, and
  `sites/fauna-social/src/pages/official-apps.astro` only places it; no
  identity is typed in the site tree. The apex vhost serves it
  ([`front-door.md`](front-door.md) § Public-vhost topology).
- **The `official-apps-check` cheap gate,
  on both merge scripts, holds the manifest equal to the tree in both
  directions**: an entry whose value differs from the tracked input it is
  pinned in, an entry `live` with no tracked pin, and a tracked signing input
  the manifest omits each fail by name. The pins are the script's `PINS`
  table: the shipped `release.yml` and `release-macos.yml`, the macOS
  `Info.plist`, `fauna_core::platform_ids`, the Xcode project, the Android
  build file, the Linux metainfo, the Windows Store manifest template, the
  front door's app origin (cross-checked against the nest's default CORS
  origin) — and, for the values only Partner Center minted and nothing else in
  the tree holds, [`installers/windows.md`](installers/windows.md) § Identity &
  coexistence's table. **Identities the workflows take from secrets** (the
  Developer ID certificate, the notary key) have no tracked input and are
  deliberately not pinned; their entries stay `pending` until one exists.
  Gate paragraph: [`convention-gates.md`](convention-gates.md).
- **Scope, honestly.** This answers a copycat *listing* — a user who can see
  a package name, a publisher, a signing identity and compare. It does nothing
  for a user sent to a lookalike origin (§ Web-app verifiability's accepted
  limit) and nothing against malice in the public source (§ Defense
  priority). The user-facing check is written in `docs/guides/install.md`.

### A store upload is built from a recorded public commit (ruled 2026-10-07)

**Every store upload — to Google Play, to App Store Connect, to Partner Center —
is built from a clean checkout of the PUBLIC repository (`faunasocial/fauna`)
at a recorded commit carrying the product version, and the channel doc's
`## Implementation status today` records that commit, the artifact's hash and
its identity before the upload is called done.** This generalizes the Play
ruling (user, 2026-09-30: nothing is uploaded before the public release, and
the upload is built from the public first-push commit) to the three stores as one rule,
for the same reason the `.dmg` is built by the public repository's own
workflow: what ships is what anyone can build from the published source, and
the record is what lets anyone try. Mechanics: the build tree is a clone of
the public repository with its HEAD asserted equal to the recorded commit and
its working tree clean — on the primary dev VM a scratch clone, on the macOS and
Windows VMs the public clone beside the development repo's root (`<dev-parent>/fauna`,
[`publish-gates.md`](publish-gates.md) § Local-merge gates → *The standing
mirror and the push origin*) fetched from the public repository and checked out
at that commit — never the private tree, and never a mirror the transform has
just rewritten from it, which is a different tree with the same version
string. **One train, one commit:** every store that uploads a version builds
the one commit that version names, fixed by the first upload any store
accepted ([`product-version.md`](product-version.md) § Version strings are
never reused or re-meant, the train corollary: the public repository's first
commit is `0.1.2` for good — uploaded to a Play draft and
refused rollout on 2026-10-07, so `0.1.2` is spent unshipped and no store
uploads it; `0.1.3`'s commit is the first public commit carrying the bump and
the targetSdk-36 change). A store that needs anything newer than that commit —
a refusal's fix — never builds a tree of its own under the old number: it
ships the next patch from the next public commit, **alone**, and every store
already holding an accepted upload of the old number keeps it and re-uploads
nothing ([`product-version.md`](product-version.md) § Reships). The three channel docs
point here and own only their own mechanics
([`installers/android.md`](installers/android.md), [`installers/ios.md`](installers/ios.md),
[`installers/windows.md`](installers/windows.md) § Store distribution).

### Web-app verifiability — what the central origin serves (ratified 2026-09-24)

The hosted web app (`app.fauna.social` — [`front-door.md`](front-door.md)
§ Which origin a user loads the app from owns why it exists) is only as
trustworthy as its box and deploy key: the same dev-tier compromise class
§ Defense priority accepts, and a web user has no store, no installer
signature and no update channel to lean on. What makes the origin *checkable*
rather than merely central is that anyone can compare what it serves against
the public source. Three pieces, ranked exactly where § Defense priority
ranks reproducible builds — a lower-tier control that closes build-time and
distribution tampering and does nothing against in-source malice — and
still the only way a web user ever verifies anything:

1. **A reproducible SPA build.** `just web` from a tagged public commit
   yields byte-identical content-hashed assets, so the served tree is a pure
   function of the source. Measured 2026-09-24 (the primary dev VM, one
   checkout, one toolchain, two consecutive builds of an unchanged tree): the Deno/Vite
   layer was NOT deterministic — SvelteKit's `kit.version.name` defaults to
   the build's wall-clock milliseconds, which lands in `_app/version.json`
   and inside one content-hashed chunk, and the hashed filenames of the
   chunk graph cascade from it (122 of 170 output entries differed, every
   one traceable to that string). Fixed the same day: the build identity is
   the commit (`apps/fauna-web/build-id.js` — `VITE_GIT_SHA` when the build
   passes it, else the checkout's HEAD; a build that knows no commit keeps
   the timestamp default so a running client still notices a redeploy), and
   the re-measurement was byte-identical. The nest-image build passes the
   commit too: its `web-builder` stage turns the image's existing
   `FAUNA_BUILD_COMMIT` build argument into `VITE_GIT_SHA`. The wasm-pack layer (rustc →
   wasm-bindgen → wasm-opt) was sampled the same day on one chunk
   (`fauna-wasm-launch`) built twice into fresh cargo targets, the `pkg/`
   trees diffed: byte-identical, all six output files — the glue, the
   `.wasm`, both `.d.ts`, `package.json` — so nothing in that layer is
   time- or run-dependent within one environment. Cross-environment
   reproducibility
   additionally needs source paths remapped — a registry crate compiles with
   its absolute `CARGO_HOME` path in panic locations — so every wasm32 build
   (the justfile's, through one per-target `--remap-path-prefix` export, and
   the image's `rust-wasm` stage) maps the checkout and `CARGO_HOME` roots to
   the fixed placeholders `/fauna` and `/cargo`. Not cargo's `trim-paths`:
   its `cargo-features` line makes the manifest unloadable by stable cargo.
   Measured 2026-09-27 on the same chunk, built in two checkouts at
   different-length paths with two different `CARGO_HOME`s: unremapped, the
   `.wasm` differed and embedded both real roots; remapped, all five `pkg/`
   files were byte-identical with no real path left. The same day's first
   full rebuild-and-compare from a clean clone reproduced eight of the nine
   chunks but NOT the core one (`fauna_wasm_bg.wasm`); the SPA's hashed
   JavaScript differed too (attributed to that chunk at the time — it was a
   second, unrelated cause, found 2026-10-06 below). That chunk was not
   deterministic even
   within one environment: two builds with the same checkout, `CARGO_HOME`,
   target dir and byte-identical build-script output differ by about 1 KB
   of code bytes, and still do at one codegen unit, so the cause is a crate
   that compiles differently run to run, not LLVM's partitioning and not a
   path. Localized the same day by comparing all 714 wasm32 artifacts of two
   such builds: the one differing crate we do not own is the registry
   dependency `mail-parser`, and the 17 differing fauna crates are exactly
   its reverse dependencies in that graph (their metadata embeds its hash) —
   which is also why the eight chunks that do not link it reproduce.
   Narrowed further the same evening without reading a line of the
   dependency: `scripts/crate-determinism-probe.sh mail-parser` builds that
   one crate alone, three times in twenty seconds (the earlier probe needed a
   full core-chunk build per sample), and compares by counts only. Its
   wasm32 rlib differs every time (two of its codegen-unit objects; its
   metadata by sixteen bytes), its native aarch64 rlib differs too, and so
   does the crate's *macro-expanded* form (`-Zunpretty=expanded`: same size,
   about a hundred of eleven thousand lines, in two regions) — so the
   crate's token stream itself changes from one compile to the next, before
   rustc or LLVM see it. `mail-parser` 0.11.2 has no build script, no
   default features and exactly one procedural-macro dependency, `hashify`
   0.2.7 (compile-time perfect hashing), which nothing else in the graph
   uses: that macro's output is what varies. No feature of either crate
   avoids it. **Resolved 2026-10-06:** the owner's bump round 1 landed
   `hashify` 0.2.9 (which adds `indexmap` where 0.2.7 had none — the shape
   of an ordering fix, read off the registry metadata alone) together with
   `mail-parser` 0.11.9 (`dependency-bump-report.md` § Who opens a round
   decides such moves; compiling a new procedural macro runs new
   third-party code on a dev machine, so neither moved before the round).
   On that lock the probe exits 0 — three runs, every view identical: the
   wasm32 rlib, its archive members, the native rlib and the expanded form,
   zero differing bytes — and the same day's `web-verify` round trip (this
   checkout against a clean clone) found all nine wasm chunks
   byte-identical, the core one included. The native rlib varying had meant
   every native app artifact that links `mail-parser` was non-reproducible
   for the same reason; the same bump restores those too. Neither an
   upstream report nor a vendored patch was needed. **What that round trip
   still reported, and why (2026-10-06):** every hashed JavaScript and CSS
   name under `_app/immutable/` differed, and `index.html` with them — and
   no wasm file is a hashed asset (the chunks are served by name from
   `static/`), so the JavaScript never depended on the chunk's bytes: the
   2026-09-27 attribution was wrong. The cause is the SPA toolchain's own
   order dependence: SvelteKit numbers its route nodes (`nodes/<N>.*.js`,
   `assets/<N>.*.css`) in the order the filesystem lists `src/routes`, and
   the whole rollup graph re-hashes from that numbering. Measured
   black-box: two fresh clones of one commit on ZFS — same path length,
   same bytes, one with its `src/routes` directory re-created in a
   different insertion order — build different trees whose CSS content
   hashes are identical under permuted numbers; ZFS (salted per-directory
   hashing) and ext4 (a per-filesystem hash seed) list a directory in an
   order no two checkouts share, so no checkout reproduces another's tree.
   Two checkouts on Linux tmpfs, whose listing follows creation order,
   built a byte-identical tree (`MATCH — 178 files`). **Ruling:** the
   served tree's hashed names must not depend on the checkout's directory
   iteration order, and the BUILD removes the dependence — a sorted
   directory listing inside the build process (the Deno sandbox that runs
   `vite build`, `build-system.md` § The Deno build sandbox, is the natural
   place for a preload that sorts `readdir`) — never an environment rule
   about which filesystem to build on (tmpfs's own order changed between
   kernel versions, and a verifier may build anywhere). Proof shape: the
   clone pair above building byte-identical trees. An upstream report to
   SvelteKit built from this evidence is the parallel route (outward-facing
   — the owner sends it); SvelteKit 3 is queued for bump round 2 and may
   already sort — re-measure when it lands. **Built 2026-10-06:** the
   sandbox launcher preloads `scripts/deno-sandbox-preload.mjs`, which
   sorts `node:fs` directory listings for every tool it runs
   (`build-system.md` § The Deno build sandbox says what it reaches); the
   clone pair, re-run on two fresh clones with the second's `src/routes`
   re-created in reverse order, built byte-identical trees with it
   (`MATCH — 178 files`) and 215 differences without it. A second, smaller
   finding from the same round trip: a long-lived checkout's `static/` can
   hold git-ignored files no fresh checkout has — a retired chunk's
   (`fauna_wasm_onnx*`), a test recipe's (`*_panic_witness*`) — which the
   build copies into the served tree. `just web-manifest` now refuses
   them (`web-asset-manifest.py write --static`: any git-ignored `static/`
   file outside the chunks `just wasm` produces), and it always re-runs
   the SPA build rather than trusting the `web` freshness gate, which does
   not see a deleted file and so would keep a removed stray in a reused
   `build/`.
   **The wasm32 C compiler — ruled 2026-10-07 (advisory; the owner's round ratifies it by landing the pin).** The chunks carry ONE C-compiled crate: `zstd-sys` (zstd 1.5.7 — `fauna-core`'s chunk compression and `fauna-mail`'s export wrapper reach it), and every chunk's `producers` section names the clang that compiled it. The 2026-10-06 attribution to `ring` and `secp256k1-sys` was read off a `cargo tree` run without the build's own `CARGO_RESOLVER_FEATURE_UNIFICATION=selected`, which unifies the whole workspace's features onto the wasm32 view; under the build's resolution neither crate is in any chunk's graph (measured 2026-10-07 on all ten chunks, and the built core chunk carries zstd's version string and no `ring`/`secp256k1` symbol). **Every builder of the served tree — the image's `rust-wasm` stage, `just wasm` on the primary dev VM, a verifier's `just web-verify` — compiles that C with ONE checksum-pinned upstream LLVM release (clang and `llvm-ar` of the same release), never a distribution's package:** no two distributions ship the same clang build, and an unpinned `apt` clang was the one tool in the chain no repo file named. **The build SELECTS it, never finds it:** a repo pin file names the release, its exact `clang --version` line and the SHA-256 of its Linux tarball per host architecture (x86_64 and aarch64 — one source, one version); the justfile's wasm32 export block and the `rust-wasm` stage set `CC_wasm32_unknown_unknown` and `AR_wasm32_unknown_unknown` to the pinned install (the stage fetches the tarball by that checksum; the dev VM reads the install at one fixed root the dev-setup doc names, with no override knob — a build-machine path is wiring, not a choice); and a wasm32 build whose compiler is not the pinned one REFUSES, printing the install step, rather than falling back to the `clang` on `PATH` — a dev build and the manifest build are one code path, so there is no second way to produce a chunk. The parse-time parity gate that already holds the two builders' chunk sets equal extends to the pin: the stage's URL and checksum must equal the pin file's. The manifest keeps recording the compiler (`wasm32-cc`, which must equal the pin), and `producers` STAYS in the chunks: stripping it (`wasm-opt --strip-producers`) would have made seven of the ten chunks match while hiding the evidence that the other three differ in code, and under one compiler the section is identical anyway. The compiler is a dependency: the toolchain need is queued for the owner's round (`dependency-bump-report.md` § Who opens a round — the round chooses the release, the newest past the minimum release age, and lands the pin), and no builder obtains it ad hoc. Host architecture: the image is built for both platforms, and wasm32 output is host-independent by LLVM's design, so the stage runs on the build platform (`--platform=$BUILDPLATFORM`, as the bridges stage already does) — one compile per image build, on the same aarch64 the primary dev VM and the runner VM run; a verifier on x86_64 uses the same release's x86_64 build, and its first `MATCH` is the measurement that the host build does not leak into the output (if it ever does, the pin narrows to one host architecture and the stage keeps its platform). **Alternatives weighed.** *Building inside the pinned image* — rejected: a dev VM never builds the image, bookworm's `clang` is unpinned, and a verifier that must trust the image build is not an independent verifier. *Removing the C from the wasm32 graph* (a pure-Rust zstd) — the shape that would retire the compiler from the trust base altogether and let a macOS or Windows verifier build the chunks at all (Apple clang has no WebAssembly backend — `build-machine-resources.md`), rejected for now on a measured obstacle: `mail-export.md` § Container shape's determinism contract makes the export's bytes a function of the fixed zstd level, so a second encoder on one app alone breaks the cross-app byte-identical bar, and a pure-Rust encoder on every app trades the reference encoder's ratio at the chosen levels for its own — a product cost the owner weighs, not a build fix. Re-open it, through the round as a dependency add, once a pure-Rust zstd encoder's ratio is measured within reach of the reference on our chunk and export corpora.
2. **A published manifest of the served asset hashes.** Every release
   publishes a manifest naming each served file and its SHA-256 (the
   2026-03-13 design's hash-manifest idea, kept; its GitHub-Pages hosting is superseded — front-door.md,
   above), produced by the same release workflow that builds the artifact:
   `asset-manifest.json` (every file's SHA-256, the commit, the toolchain
   pins), written into the served tree by `just web-manifest` — the recipe
   the hosted app's deploy builds with — so it is published and served with
   the files it names. A verifier runs `just web-verify <manifest>`, which
   rebuilds the named commit from a clean clone, hashes the tree and compares; a
   fetch of `app.fauna.social` can be checked against the manifest by
   anyone, including a Code-Verify-style browser extension later.
3. **An entry in the transparency log.** The manifest is appended to the
   log [`../behavior/region-blocking.md`](../behavior/region-blocking.md)
   § The transparency log defines (witnessed, append-only), so a served tree
   matching no logged manifest is detectable even by a user who saw it
   alone — the CT property, applied to app bytes.

Honest scope: none of this helps a user who was sent to a lookalike origin,
and none of it detects malice that is in the public source (§ Defense
priority's accepted limit). Nothing here is a runtime check the SPA makes on
itself — an origin check in the shipped code is a UX warning an attacker who
serves the code simply removes (the 2026-03-13 design said so of its own
§ 1.5).

### When a release workflow publishes (ruled 2026-08-31; refutable)

**A release workflow publishes on a `v*` tag ref and on nothing else; every other
ref is a dry run.** One rule, two workflows, no second pattern:

- **`release-macos.yml`** (public repository, the shipping example) — `push: tags:
  ["v*"]` plus `workflow_dispatch`, with the credentialed `sign` job inside the
  `release-macos` Environment, whose protection rules are *required reviewer +
  `v*` tags only*. Owned by [`installers/macos.md`](installers/macos.md) § Build
  Pipeline → *The `.dmg` release pipeline*; that doc's "on every `v*` tag (and on
  demand for a dry run)" is the sentence this rule generalizes.
- **`release.yml`** (public repository since 2026-08-31 — ported beside
  `release-macos.yml` into the publish tooling's injected public-files tree;
  the internal copy, which had never run, is deleted) — `push: tags: ["v*"]`
  plus `workflow_dispatch`, its `release` job guarded by `if:
  startsWith(github.ref, 'refs/tags/v')` **and** inside the `release`
  Environment. The guard is the same rule expressed against the ref alone, and
  it is **reachable**: a `workflow_dispatch` may name a tag (`gh workflow run
  release.yml --ref v1.2.3`), which makes `github.ref` `refs/tags/v1.2.3`. So
  a branch dispatch builds all four Rust targets and both Windows arches and
  publishes nothing — the dry-run shape a first run should take — and a tag
  ref publishes. Shape pinned by the publish tooling's own workflow-shape
  tests (tags-only trigger, no-credential build jobs, the Environment on the
  publisher, SHA-pinned actions, the anchored verify regex).

**Why a ref and not a `dry_run` input.** An input would be a second way to say
what the ref already says, and the two could disagree — a `v*` tag dispatched with
`dry_run: true`, or a branch dispatched with `publish: true`, are states the ref
form cannot represent. The ref is also what the eventual `push: tags` trigger
supplies, so the same guard serves both triggers and the restore needs no edit.

**The two gaps the 2026-08-31 ruling left open were settled the same day, by
porting `release.yml` into the public-files tree:**

1. **The publish job now sits inside the `release` Environment** (required
   reviewer + `v*` tags only — the same per-run human approval
   `release-macos.yml`'s `sign` job carries, and the workflow-file statement of
   the per-run approval rule the nest image's dispatch carried in prose
   until the user lifted it 2026-10-05; [`build-system.md`](build-system.md) § Image tags & channels). The
   repository-side protection rules are a settings act GitHub refuses on a
   private free-plan repository — the same class as branch protection — so
   they are a flip-day item on the visibility-flip track's settings sheet, to
   be applied **before or in the same act as** the file landing on the public
   repository: the first run referencing a not-yet-configured environment
   creates an *unprotected* one, which reads as a gate and is not one.
2. **Which repository and which runner: the public repository, on
   GitHub-hosted runners** — `installers/macos.md` § Build Pipeline's 2026-08-25
   posture, applied to the artifact class it was ratified for. The 2026-07-16
   self-hosted posture ([`merge-gates.md`](merge-gates.md) § CI enforcement)
   governs *enforcement gates*, which stay on the merge path and the self-hosted
   fleet; a release artifact's attestation from a self-hosted runner would be
   the machine vouching for itself, an objection that binds at least as hard
   for server binaries as for the `.dmg`. Three consequences, each deliberate:
   the internal copy is deleted (its dependents — `front-door.md`,
   `feature-catalog.md`, `testing.md`, `version-compatibility.md`, and two
   tier_1 pins in `tests/e2e-unified/tests/` — now reference the shipped file
   at `.github/workflows/release.yml`); the
   verification recipe's certificate-identity regex is **anchored**
   (`^https://github\.com/faunasocial/fauna/` — the unanchored form also
   matched sibling repository names, so a reader could not tell which
   repository signed what); and a binaries release is cut by tagging the
   **public** repository — `just release-tag` still mints the internal tag
   that names a release ([`feature-catalog.md`](feature-catalog.md) § Tag gate
   and release table), and the corresponding `v*` tag on the public repository
   is the separate deliberate act the workflow fires on, consistent with "what
   ships is what anyone can build from the published source".

### What a release ships

**`release.yml`'s asset set, every one `cosign`-signed and listed in `SHA256SUMS`:** per Rust target (`x86_64`/`aarch64` × linux/darwin) the server binaries `fauna-nest`, `fauna-push-relay`, `fauna-cors-proxy` and `fauna-front-door`, each as `<package>-<arch>-<os>`; on `x86_64` linux alone the linux desktop app as `fauna-linux-x86_64-linux` (package `fauna-linux`, binary `fauna-desktop`); on both linux targets the terminal app's archive `fauna-tui-<arch>-linux.tar.gz` ([`installers/tui.md`](installers/tui.md) § The ratified channel); and per Windows arch `Fauna-Setup-<arch>.msi` plus `fauna-tui-<arch>-windows.zip`. The macOS downloads are `release-macos.yml`'s ([`installers/macos.md`](installers/macos.md) § Build Pipeline). **The released nest is the image's flavor** — built with exactly the features the `Dockerfile`'s native nest build enables (`bluesky`, `nostr`, `activitypub` over the defaults), so a downloaded server and the image are one server. **A named asset the build did not produce fails the job**; nothing is skipped quietly. Both properties, and that every collected binary is one a workspace package builds, are pinned by the publish tooling's own workflow-shape tests. `fauna-sync` left the set with its daemon on 2026-10-02 ([`apps/sync-agent.md`](apps/sync-agent.md) § Headless deployment). `fauna-router` is not in the set: with its HTTP surface removed it cannot onboard an actor, so it is not a release artifact until a WS-RPC frontend exists ([`nest/worker.md`](nest/worker.md) § `fauna-router`). Unproven until the first run: the bridge features have never been compiled for the darwin targets — the workflow has never executed on any runner, so its branch-dispatch dry run (§ *When a release workflow publishes*) is where that is first measured.

## Honest residual (the limit of full automation)

Full automation ⇒ some machine (Tier 0) is trusted ⇒ it is the single point of
failure; if compromised it ships. Budget the remainder on: making Tier 0 hard to
take via the dev-fleet vector (separate host/software/creds, no untrusted code,
minimal inbound); detection speed (transparency + monitors + anomaly alerts on
high-risk releases — limits the *duration/stealth* harm of any stolen key or
undetected compromise); and the client blast-radius limits above. The accepted
limit (stealthy in-source dep malware) stands.

## Implementation status today

**§ Third-party container images part 4, re-read 2026-10-02:** the mirror's production copies (`:test` → `:latest` + `:<version>`) are the public repository's pipeline's, not yet built; the private pipeline's promote-side mirror copy is cut with the promote job, and the private pipeline stages the mirrors on `:sha-<sha>` + `:dev` only.

**Almost nothing in the target state is implemented.** As of 2026-06-28 (verified
against code — re-verify before acting, design doc § 2):

- **A store upload is built from a recorded public commit — RULED 2026-10-07, first applied by the Play upload (§ Release signing → *A store upload is built from a recorded public commit*).** The Play candidate is built from the public repository's first commit and recorded in [`installers/android.md`](installers/android.md) § Implementation status today when uploaded; iOS and Windows have no upload yet ([`installers/ios.md`](installers/ios.md), [`installers/windows.md`](installers/windows.md) § Implementation status (Store channel)). The listing-flip rule of the same date is applied by each store's first invited release; no entry has flipped.
- **The official-apps list — BUILT 2026-09-24 (§ Release signing → *The official-apps list*).** `installer/official-apps.json` (24 entries over eight sections since 2026-10-08, when the macOS update-channel key and update-feed address left with the update framework), the `/official-apps` page rendered from it, the `official-apps-check` gate on both merge scripts with its own test suite (`official-apps-test`, red-verified in both drift directions), the install guide's check, the takedown runbook and the IP doc's pointer all landed together. **14 of the 24 entries are `pending`**, each with its reason; what flips them is other tracks' work, not this list's: the first `v*` release on the public repository (the Sigstore identity, the attestation, the Linux and terminal binaries), the first Play upload (the app-signing certificate and the listing), the first Store submission (the reserved listing address), the front door going live (the hosted web origin), image signing (the nest image). The page is in the site build, not yet served: the apex site's deployment is front-door.md's open item.
- **Web-app verifiability — RATIFIED 2026-09-24; the mechanism BUILT 2026-09-27; the served tree REPRODUCIBLE across checkouts since 2026-10-06 (§ Release signing → *Web-app verifiability*).** Built: the SPA keys its identity on the commit (`apps/fauna-web/build-id.js`), in the nest image too (its `web-builder` stage turns `FAUNA_BUILD_COMMIT` into `VITE_GIT_SHA`); every wasm32 build remaps its checkout, `CARGO_HOME` and target roots (the justfile export, the image's `rust-wasm` stage), proven across checkouts on one chunk; `scripts/web-asset-manifest.py` writes and checks `asset-manifest.json`, `just web-manifest` builds the tree with it, `just web-verify <manifest>` rebuilds from a clean clone and compares, and the front door's deploy draft builds with `just web-manifest`. **(a)** RETIRED 2026-10-06 — the core chunk `fauna_wasm_bg.wasm` reproduces: its nondeterminism was `mail-parser`'s one procedural-macro dependency `hashify` 0.2.7, and bump round 1's `hashify` 0.2.9 + `mail-parser` 0.11.9 fixed it (`scripts/crate-determinism-probe.sh mail-parser` exits 0 in every view; a `web-verify` round trip finds all nine wasm chunks byte-identical — piece 1's resolution). **(a′)** RETIRED 2026-10-06 — the SPA's hashed names no longer follow the checkout's directory order: the Deno build sandbox preloads a sorted `readdir` (piece 1), proven by two differently-ordered clones building byte-identical trees, and `just web-manifest` refuses git-ignored `static/` files `just wasm` does not produce and always rebuilds the SPA; a `just web-manifest` in a long-lived dev checkout then `just web-verify` from a clean clone answered `MATCH — 177 files` the same day. Open: **(b)** the image leg is HALF proven — the Dockerfile's `web-tree` stage is exactly the tree the final stage serves from `/usr/share/fauna-web/`, and the dispatch-only `web-tree-reproducibility.yml` builds it twice on the self-hosted CI runner for one ref (the second with the `rust-wasm` and `web-builder` layers uncached), compares and pushes nothing: its first run (2026-10-06, the probe's own commit) answered `MATCH — 165 files`, so the image reproduces ITSELF. It does NOT yet reproduce across environments: `just web-verify` on Linux against that run's manifest (a cold clean-clone build of the same commit) answered `MISMATCH — 35`: every `_bg.wasm` chunk changed while every wasm-bindgen `.js` matched (so the exported surface agrees and the bytes underneath do not), the share chunk's hashed name and `share-viewer.html` followed it, and the image shipped none of the 20 `.d.ts` files `just wasm` copies into `static/`. Two causes are found. **The wasm32 C compiler is part of every chunk:** the chunks' one C dependency (`zstd-sys`; the 2026-10-06 reading `ring`, `secp256k1-sys` came from a `cargo tree` without the build's `selected` feature resolution — piece 1's ruling) is compiled by `clang`, and each chunk's `producers` section records which one — `Ubuntu clang 21.1.8 (6ubuntu1)` on Linux (`scripts/wasm-sections.py` prints it), while the image's `rust-base` stage installs bookworm's `clang` package (`Debian clang version 14.0.6`). **Measured 2026-10-06 (the probe run on the commit that added the fingerprint, against a `just wasm` of the same commit on Linux):** the image already ran the same `wasm-pack` 0.15.0 as Linux (pinned to it in bump round 2 the same day), so that suspect is cleared; seven of the ten chunks differ ONLY in the `producers` section that names the C compiler (code and data byte-identical), and the other three (`fauna_wasm`, `media`, `share`) differ in their code section too — clang 14 and clang 21 compile the C differently. So cross-environment reproducibility needs ONE pinned wasm32 C compiler for every builder, the verifier's included — RULED 2026-10-07 (piece 1's *The wasm32 C compiler* paragraph): one checksum-pinned upstream LLVM release, selected by both builders through `CC_wasm32_unknown_unknown`/`AR_wasm32_unknown_unknown`, an unpinned compiler refused, `producers` kept; the toolchain need is queued for the owner's round (`supply-chain/bump-queue.toml`, `toolchain:llvm`). **NOT BUILT:** the pin file, the two builders' selection, the refusal, the parity-gate extension and the manifest's pin check all wait on the round; the manifest records the compiler as `wasm32-cc`. The probe prints the image's wasm toolchain and fingerprints every chunk's sections (`scripts/wasm-sections.py`); the image still reproduces itself (`MATCH — 185 files`). **The `.d.ts` shape is settled:** the image copies all four files wasm-pack emits per chunk, as `just wasm` does (a merge gate pins the set); **(c)** the log entry waits on the log itself; the origin is not live (front-door.md § Implementation status today). All of them are captured.
- **The plugin catalog's images (§ Third-party container images, widened 2026-09-05): no catalog exists yet** — the first entry lands with `third-party.md`'s catalog + supervisor slice and joins the vetted table then.
- **Third-party container images — PINNED + GATED 2026-08-27 (§ Third-party container images).** All three refs the bundles pull (`clamav/clamav:latest-debian`, `rspamd/rspamd:latest`, `nickfedor/watchtower:latest`) are digest-pinned in every shipping copy and recorded in the installer self-test's vetted table with publisher class, architectures, attestation presence and vet date; the widened gate is green. **Part 4 is half-built as of 2026-08-29: the PRODUCER side ships, the CONSUMER side does not.** The release pipeline now mirrors every vetted digest into Fauna's namespace — `build-nest-image.yml`'s stage step copies each to `ghcr.io/faunasocial/<name>:sha-<sha>` + `:dev`, and — until the private promote's 2026-10-02 retirement — its promote job (and `promote-nest.yml`) moved the same manifests to `:latest` + `:<version>` on the same green verify; the production copies are the public pipeline's now (part 4, the re-read), both legs driven off the vetted table itself — the installer self-test prints the mirror worklist and a single mirror script consumes it, so no second list of images can exist. The argv that script builds, the digest-pinned stage source, and the skip-a-pre-mirror-sha rollback path are covered by tests in the installer self-test, red-verified against all three defect shapes. **Since the same day the mirrored manifests are also BOOT-GATED before promote**: the pipeline's *verify* job starts the bundle's scanner services from those manifests and speaks the mail bridge's own clamd/rspamd wire to them, red-verified against an unbootable manifest, a scanner that runs without serving, and a bundle carrying no scanners at all (mechanism owned by [`build-system.md`](build-system.md) § Image tags & channels). Until then nothing anywhere in the pipeline had ever started a sidecar, so a mirrored manifest missing the runner's architecture, or a scanner that boots without serving, would have been promoted to `:latest` unobserved. **The bundles still name the third-party refs**, so nothing a box pulls has changed yet: the flip is gated on one approved `build-nest-image.yml` dispatch actually publishing the mirror tags, because a bundle pointing at `ghcr.io/faunasocial/clamav` before that image exists is a dead fresh install. **The Dockerfile base-image pins and the currency leg's digest-drift check both landed 2026-08-29:** the Dockerfile's four `FROM` images (`rust:bookworm`, `golang:X.Y.Z-bookworm`, `denoland/deno:latest`, `debian:bookworm-slim`) are digest-pinned and recorded in the same vetted table with `mirror=None` (a build input the pipeline consumes on the runner, never ships a reference to), gated by the same widened installer self-test, now also path-scoped to the Dockerfile itself; and the installer self-test's new currency-check mode (wired into `supply-chain.yml`'s new `image-currency` job, scheduled cron + dispatch — daily since 2026-10-04 — self-hosted runner) resolves every vetted ref's live index digest and prints a `::warning::` annotation on drift — always green by design, since currency stays reviewed monthly, never auto-bumped.

- **The hosting service's dependency alerts — RULED ON 2026-10-03 (§ Dependency verification → *The hosting service's dependency alerts*).** The owner ruled the vulnerability alerts and the opt-in alerts for reported-malicious packages ON for both repositories, and the security and version updates OFF. Measured 2026-10-04: `deno.lock` entries raise no alerts — the dependency graph holds only the direct npm dependencies the `package.json` files name, so the alerts do not watch the locked npm closure of the two web trees; the check in the next bullet does. No service surveyed offers the depth-checked multi-reader review, its ledger or an enforcement point.
- **The locked npm closure — CHECK BUILT 2026-10-04, GREEN from 2026-10-05: `dep-npm-audit` exits 0 (web 1 advisory, excepted; site 0) (§ Dependency verification → *The locked npm closure*).** `dep-npm-audit` and `supply-chain.yml`'s `npm-audit` job exist and run on the daily cron, on dispatch and on every Deno-manifest push; the job has not yet run on the self-hosted runner, so the runner's Deno accepting `deno audit` is unverified. Measured on Linux (Deno 2.7.14) the day it was built: 32 advisories against the web app's lockfile (12 high) and 29 against the site's (1 critical, 14 high), no exception recorded. **The web app's lockfile was re-resolved inside its existing version ranges the same day** (owner-approved after its `dep-bump-report` consequence table: 52 package versions changed, none RED; verified by `just web-check` and `just web`), leaving 2: `cookie` <0.7.0, which only `@sveltejs/kit` 3 moves, and `ts-deepmerge` <8.0.0, which `@contentauth/c2pa-web` 0.15 drops — each a major migration of the web app, to be taken through its own consequence table rather than excepted (the owner prefers the newer versions). **`@contentauth/c2pa-web` 0.6.1 → 0.15.2 landed the same evening** (owner-approved table: 5 changed, 0 RED, 1 FLAG — the new `@contentauth/c2pa-utilities`, signed provenance from the SDK's own repository), with the tracked `apps/fauna-web/static/c2pa.wasm` re-synced to the package's binary and `just web-check` now failing if the two ever differ; verified by `just web-check`, `just web` and the web content-credentials e2e (a signed upload paints the badge, an unsigned one does not). The web app is down to 1: `cookie`, which waits on Kit 3 (past the minimum release age from 2026-10-08) and opens the next round of bumps. **The site moved to astro 7 on 2026-10-05: 29 → 0.** The lock resolves astro 7.3.5 (`"astro": "^7.2.8"`), its six pre-scan REDs byte-identical to the six the venue cleared (lock integrity = the cleared integrity), and `http-cache-semantics` 4.2.0 → 4.3.0 through the security-fix bypass on the owner's own reading of its diff (§ *Minimum release age*, the Deno form); the consequence table re-run on that lock differed from the one the owner approved by exactly that row (176 changed, 6 RED — the cleared six — 89 FLAG). Its four in-process native addons run inside the whole-build bubblewrap; the build and its output check: `build-system.md` § The Deno build sandbox, implementation status. **The web app's `cookie` advisory (GHSA-pxg6-pf52-xh8x, low) is a temporary exception (owner-approved 2026-10-05)**: SvelteKit 2 uses `cookie` only server-side to serialize `Set-Cookie`, and the static SPA ships no server and never calls Kit's cookie API, so the serializer only runs at build time on our own values; it is deleted in the SvelteKit 3 bump, resolvable from 2026-10-08 17:22 UTC.
- **The daily Linux advisory gate — BUILT AND ARMED 2026-10-05 (§ Dependency verification → *The scheduled scans run daily*).** `dep-advisory-check` runs the audits once a day in the Linux merge-gate check and publishes a red into the janitor queue; green on its first run. Since 2026-10-05 it runs three: npm, cargo and the module-level Go check below.
- **Go modules at module level — BUILT 2026-10-05 (§ Dependency verification → *Go modules: reachable and module-level*).** `dep-go-module-audit` covers the three tracked `go.mod` files (`bins/fauna-bridges`, its vendored `third_party/go-imap`, `libs/fauna-mail-go`). Measured 2026-10-05 before the fix: the hosting service showed 52 open Go alerts — 18 on the bridge, 21 on `tools/atproto-s0-probe` (removed that day, a throwaway whose gate was waived) and 13 on `bins/fauna-dns/go.mod`, a manifest deleted on 2026-05-25 that the hosting service's dependency graph still listed. The bridge moved `x/crypto` 0.41.0 → 0.57.0, `x/sys` 0.48.0, `go-ipld-prime` 0.23.0, `go-retryablehttp` 0.7.7 and `go-ipld-prime`'s own four dependencies, and — on the check's own first finding, one the alerts never raised — `otel` 1.41.0 → 1.42.0, all owner-approved; the push closed the bridge's and the probe's alerts on the hosting service within minutes. One exception is recorded, `GO-2026-5932` on `x/crypto` (no fixed release; not reachable from the bridge per the symbol-level scan). The `fauna-dns` alerts stay open: their manifest was deleted before the alerts were switched on, so no push ever shows the hosting service its removal. The runner's `govulncheck` job still runs the symbol-level scan only.
- **Cargo advisories only the hosting service saw — FIXED 2026-10-05.** Two of the four `Cargo.lock` alerts carried no RustSec id (GHSA-only), so `cargo audit` could not report them: `tar` (GHSA-3pv8-6f4r-ffg2, fixed 0.4.46, in range) and `aws-smithy-json` (GHSA-8ffr-xgwf-xj56, fixed 0.62.7, reachable only by moving `aws-sdk-s3` 1.129 → 1.152). Both landed on the owner's approval of their consequence table (19 changed, 0 RED); `aws-sdk-s3` 1.152.0, `aws-smithy-runtime` 1.16.0 and `aws-smithy-runtime-api` 1.19.0 were 4–5 days old, taken on the owner's explicit in-chat exception to the 7-day age — beyond the written bypass, whose condition (1) wants a `cargo audit` report. `aws-sdk-s3` moved onto `lru` 0.18 with it. The other two alerts are the `libcrux-chacha20poly1305` and `lru` 0.12.5 `cargo audit` exceptions: the first closes with the openmls 0.9 move, which lands before the public flip, and the second with the tantivy bump. The same commit's vet-exemption regeneration also covered the ratatui 0.30 crates, which had left `cargo vet --locked` failing on 45 entries.
- **Swift packages — BUILT 2026-10-05, IN THE DAILY GATE (§ Dependency verification → *Swift packages*).** `dep-swift-audit` read the one tracked `Package.resolved` (`apps/fauna-apple/`) until 2026-10-08, when the update framework — its only pin — left the tree and the file with it; no tracked file is now a pass (an exception left recorded still fails). Its first run reported exactly the hosting service's two open Sparkle advisories against 2.9.0 (GHSA-hg88-v3cw-3qrh, GHSA-g3hp-f6mg-559v, both covering ≤ 2.9.1). Sparkle moved 2.9.0 → 2.9.6 on the owner's approval of its consequence table (the newest 2.9.x, patch releases only, 49 days old; 2.10.0 was the alternative), verified by `swift-test` and `mac-debug`, and the check now exits 0 with no exception recorded. In the same commit it became the fourth part of `dep-advisory-check`, whose suspect scope gained `Package.resolved` and `supply-chain/swift-audit-exceptions.toml`.
- **`cargo audit` — GREEN from 2026-10-05: `cargo audit --deny warnings` exits 0 against `.cargo/audit.toml` (1 vulnerability and 4 warnings excepted since bump round 1, measured 2026-10-05 with no ignore list: `rsa`; unmaintained `proc-macro-error`, `paste`, `proc-macro-error2`; unsound `lru` 0.16 — 6 after the round's scraper and nostr moves and before its tantivy move, 9 after the openmls 0.9 move, 7 and 10 before it; `core2`, `ansi_term` and `atty` fixed, `rustls-pemfile` removed), from 10 / 25 before the 2026-10-04 bump (§ Dependency verification).** In-range updates moved `event-listener`, `lru` 0.18, `rand` 0.9 and 0.10, `chacha20`, `der`, `spin` 0.9 and 0.10 and `async-utility`, and `wasmtime` 49.0.1 → 49.0.2 came in through the security-fix bypass for RUSTSEC-2026-0325/0326/0327 (the bypass's first use; its 33 wasmtime/cranelift/pulley/wasm-tools crates rest on the Bytecode Alliance import, the other nine on moved exemptions). **The seven vulnerabilities are recorded exceptions in `.cargo/audit.toml` (2026-10-05), each approved by the owner individually**, every one on the 2026-07-30 disposition above with its premise re-verified against the tree that day: `libcrux-secrets` RUSTSEC-2026-0212 (the owner's 2026-07-30 acceptance), `libcrux-sha3` 0207/0208 (outside ML-KEM's use; MLS pins a classical suite), `libcrux-chacha20poly1305` 0124 and `libcrux-aesgcm` 0211/0209 (no workspace crate reaches them under any feature or target), `rsa` RUSTSEC-2023-0071 (no fixed release; no RSA decryption in the codebase — key generation, parsing, PKCS#1 v1.5 signing and verification only). **Of the 15 warnings, 10 are excepted and 5 fixed (the 2 `core2` ones, `ansi_term` and `atty` ×2); most excepted ones have a fix**, measured from registry metadata the same day: in-range updates (`cid` 0.11.3 / `multihash` 0.19.5 drop `core2`, both its unmaintained and its yanked warning — and cargo-audit's ignore list takes advisory ids, so a yanked warning cannot be excepted at all) and major bumps of their dependents (`ratatui` 0.30, `tantivy` 0.26, `scraper`/`selectors`, `multihash-codetable` 0.2, `nostr` 0.45 for the dev-only `nostr-relay-pool`). A sixteenth, `rustls-pemfile` RUSTSEC-2025-0134 (unmaintained; a direct dependency of five workspace crates), is gone from the tree since 2026-10-05: certificate and key PEM is read through `rustls-pki-types`' PEM API, already in the graph through rustls. The three with no fix are excepted, each owner-approved individually on 2026-10-05: `lru` RUSTSEC-2026-0253 (fixed only in 0.18.2, which the newest `tantivy` and `atrium-common` do not accept; shipped builds unwind, so build settings do not rule it out), `proc-macro-error` RUSTSEC-2024-0370 via `iref-core` and `proc-macro-error2` RUSTSEC-2026-0173 via the libcrux stack (compile-time macro helpers that ship nothing). `libcrux-aesgcm`'s rename notice RUSTSEC-2026-0210 is excepted with 0211/0209 (the same unreachable lock entry). **The fixable six are temporary exceptions (owner's choice and individual approvals, 2026-10-05)**, each naming the row that removes it and deleted in that fix's commit — `fxhash`, `instant`, `nostr-relay-pool`, `paste`, `lru` RUSTSEC-2026-0002; `paste` also comes through iroh's network crates (`netwatch`, `netdev`), so it may outlive our own fixes (`instant`'s third path, `openmls` 0.8's `fluvio-wasm-timer`, left with openmls 0.9). **`lru` 0.12 is off three of its five parents (2026-10-05, owner-approved consequence table: 7 changed, 0 RED):** the workspace's own `lru` requirement moved 0.12 → 0.18, and `aws-sdk-s3` 1.119 → 1.129 and `atrium-common` 0.1.3 → 0.1.4 moved in range, both onto `lru` 0.16.4 (`aws-sdk-s3` 1.152, which takes 0.18, was under the minimum release age), bringing `aws-smithy-checksums` 0.64, `aws-smithy-json` 0.62, `crc-fast` 1.9, `md-5` 0.11 and `crc` 3.3 along and dropping `aws-smithy-http` 0.62; vet exemptions moved with them, `lru` 0.16.4's rising to `safe-to-deploy`. RUSTSEC-2026-0002 stays for `ratatui` 0.29 and `tantivy` 0.22, RUSTSEC-2026-0253 for the 0.16 parents; the counts are unchanged. **`ansi_term` and `atty` are fixed without a version bump (2026-10-05): the vendored `libs/ksni` fork commits the output of its build-time code generator** (`dbus-codegen` 0.9.1, byte-identical to what the build produced) and drops the generator, which was the only path to `clap` 2.34; the lock shed nine packages (`ansi_term`, `atty`, `clap` 2.34, `dbus-codegen`, `hermit-abi` 0.1, `strsim` 0.8, `textwrap` 0.11, `vec_map`, `xml-rs`) and added none, so no consequence table and no new source read were needed — moving to `ksni` 0.3 would have meant re-porting the xdg-activation patch onto a rewritten crate (`libs/ksni/PATCH.md` § Generated D-Bus code). Their three exceptions are deleted. **`core2` is fixed (2026-10-05, owner-approved consequence table: 6 changed, 0 RED):** `cid` 0.11.1 → 0.11.3 and `multihash` 0.19.3 → 0.19.5 in range, which drop it, and `multihash-codetable` 0.1.4 → 0.2.2, moved to `fauna-cbor`'s dev-dependencies — production computes CIDs with `blake3` directly and the crate only cross-checks them in tests, so it and its hash crates no longer ship (their vet exemptions drop to `safe-to-run`). A yanked warning has no advisory id and cannot be excepted, which is why this one was fixed first. **The vendored C# bindgen is on clap 4 (2026-10-05; consequence table: 0 changed, the lock only shed clap 3 and its private dependencies)** and no longer uses `paste`; no exception went stale that day, because `atty` still came through dbus-codegen's clap 2 (ksni 0.2, fixed since as above), `paste` through ratatui 0.29 and iroh's netlink crates, and `proc-macro-error` through `iref-core`. The regenerated C# bindings are byte-identical. Verified: `fauna-cbor` and `fauna-carv2` tests (the cross-language golden CID vector included), the native workspace check, `just wasm`, `cargo vet --locked`. **`fauna-tui` is on ratatui 0.30.2 with crossterm 0.29 (2026-10-05; owner-approved consequence table: 46 changed lock entries of which 13 are compiled, including `bitflags` 2.13.2 across the workspace, forced by `ratatui-core`)**, with no source change. The tui no longer pulls `paste` and moves to `lru` 0.18, but no exception went stale: `paste` still comes through iroh's netlink crates, and `lru` 0.12.5 through tantivy 0.22, now its only parent. `cargo audit --deny warnings` still exits 0. Verified: `cargo test -p fauna-tui --bins` and clippy, plus the tui e2e tier. **The openmls/libcrux stack is on openmls 0.9.0, `openmls_rust_crypto` 0.6, `hpke-rs` 0.7 and `libcrux-ml-kem` 0.0.10 (2026-10-05; owner-approved consequence table: 52 changed, 1 RED, the RED `crabgrind` accepted by the owner as lock-only — it sits behind `cfg(valgrind_ct_test)` under `libcrux-secrets` 0.0.6 and no build compiles it)**, landed before the public flip with no compatibility step (the flip removes every nest, app and datum — `version-compatibility.md` § Dimension 2). Seven exceptions went stale and are deleted: `libcrux-secrets` 0212, `libcrux-sha3` 0207/0208, `libcrux-chacha20poly1305` 0124 and `libcrux-aesgcm` 0211/0209/0210 (the optional libcrux HPKE subtree stays locked, at fixed versions); `proc-macro-error2` 0173 stays, `hax-lib-macros` being still 0.3.7. `fauna-pq-kem`'s pinned X-Wing vectors are byte-identical on 0.0.10. Verified: `just test-compile-check`, `fauna-mls`, `fauna-pq-kem` and `fauna-client-mls-sync` tests, `just wasm`, `cargo vet --locked`, `cargo audit --deny warnings`. **Bump round 1 — the first owner-opened round, decided as one table (2026-10-05, `supply-chain/rounds/round-1.md`) — landed `scraper` 0.27 and the dev-only `nostr-sdk`/`nostr-connect` 0.45:** `fxhash` and `nostr-relay-pool` left the lock and their two exceptions are deleted; **the round's tantivy 0.22 → 0.26.2 move (index format v4, `content-index.md` § Engine and shape) then took `instant` and `lru` 0.12.5 out of the tree**, after the round's single venue pass cleared `typetag` 0.2.23's pre-scan RED: RUSTSEC-2024-0384 and RUSTSEC-2026-0002 deleted, the latter closing the repository's last open Dependabot alert; tantivy 0.26's stop-word filter needs its `stemmer` feature, so `fauna-index` takes it too (`rust-stemmers` at the version 0.22 already used). Verified: the four nostr test binaries with `--features nostr` (27 tests, migrated from nostr 0.45's changelog and a signatures-only API listing, both owner-checked before they were read), the link-preview tests, `fauna-mail` and `fauna-conversations`, `cargo audit --deny warnings`. Once the repository is public the remaining fixes are taken together in an owner-opened bump round, never one table at a time; until then each lands on the owner's approval of its own table (`dependency-bump-report.md` § Who opens a round); each remaining exception is still proposed individually. **`libcrux-kem` 0.0.9's RUSTSEC-2026-0330/0331 are excepted since bump round 4 (2026-10-07, owner-approved; 3 vulnerabilities and 4 warnings excepted from then):** the entry is lock-only — the optional libcrux HPKE subtree above, compiled by no workspace crate under any feature or target — and 0.0.10 is unresolvable from the registry, needing `hpke-rs` 0.8, which no `openmls_rust_crypto` or `openmls_libcrux_crypto` release accepts yet; both entries are deleted when one does and the queued need moves `libcrux-kem`.
- **Minimum release age — 7 days ADOPTED and BUILT for Cargo and for Deno 2026-10-03; the security-fix bypass and the committed-constant form RATIFIED 2026-10-04; Go not covered (§ Dependency verification → *Minimum release age for new dependency versions*).** The value and its `[unstable]` gate are committed in `.cargo/config.toml` and held by `test_cargo_min_publish_age_committed.py` (red-verified against a changed age, a removed value, a removed gate and a committed override). The owner ratified the bypass rule on 2026-10-04. The owner ratified the committed-constant form the same day, with the security-fix bypass as its one exception. Open: re-verifying the `[unstable]` line at the pin bump to Cargo 1.100. The Deno value is the `minimumDependencyAge` key of both tracked `deno.json` files, held by the same test (red-verified against the key's absence); open there: the workflows' Deno is not yet confirmed to know the key, and one that does not ignores it silently (Windows and macOS build no web, so resolve nothing). No recipe wraps the bypass yet — it is a hand-run invocation under the four conditions, and for Deno a hand edit of one lock entry (the Deno form, first used 2026-10-05).

- **Untrusted-source read venue — SET UP and containment-verified 2026-10-02; the
  FIRST FULL REVIEW COMPLETED 2026-10-04 (kache 0.28.1; § Reviewing untrusted source
  without being subverted).** The dedicated no-organization account, its collaborator
  grant on the sandbox, the App on the sandbox's owning account (sandbox only), the
  `None`-network environment, Actions disabled and a zero-webhook baseline are all in
  place, and the black-box containment check passed: registry, raw-content and
  cloud-credential endpoints refused by the proxy, the project's private and public
  repositories unreachable by git and by API, the sandbox reachable, credential
  variables holding the proxy placeholder; webhooks were 0 and Actions off at every
  audit of the run. The first reviewer sessions (kache 0.16.0, 2026-10-02) exposed that
  read depth was self-reported — a depth-B review of a 129k-line crate returned in about
  five minutes — and the depth check of § *Verdict integrity* was built 2026-10-03 in
  response. **kache 0.28.1, eight crates, three rounds (2026-10-03/04):** round 1 was
  three same-publisher crates short (the review-set rule of the section's *What the
  venue reviews* bullet); in round 2 every reviewer refuted `kache` over configuration
  gaps (the bar of the same bullet); in round 3, the bar stated, Sonnet and Fable
  certified `kache` with all 657 sites quoted and Opus kept one blocking finding — the
  aggregate is REJECT 2/3 and the owner accepted the tool. The seven other crates are
  CERTIFY 3/3 with every site quoted — the review ledger's first entries; `kache` itself
  is not on the ledger, so its next version is a full review again unless the quorum
  certifies it. **Clearing a RED has cleared real findings:** both `kache` REDs
  (`injection`, `src/cli.rs:7833`, `src/scheduler.rs:991`) were ruled false-positive
  with matching whole-line quotes by every reviewer in every round; the wasmtime REDs
  above are still RED. **Delta review** is BUILT and now has seven recorded bases to
  rest on; no real delta has run — the next kache version is the first. The delta ruling is ratified by the owner and its
  monthly re-base rule is enforced by the aggregator. Evidence. **One prompt per model per venue run, every
  ecosystem and mode — BUILT 2026-10-05:** one builder invocation renders one prompt per
  model over every item a run holds (Rust and npm reviews, full or delta, and RED rulings),
  the aggregator certifies an npm review into the review ledger (no `cargo vet` certify),
  and the npm review half has no real run yet. Build containment (the ECS isolated-build
  venue) is a separate, still-open item.

- **The wasmtime tree (landed 2026-10-02) — REVIEWED 2026-10-04 to the owner's scope; both pre-scan REDs cleared ([`dependency-verification-log.md`](dependency-verification-log.md), the 2026-10-02 and 2026-10-04 evening entries).** Of the 46 introduced crates, the twelve Fauna itself vouched for (the eleven former exemptions and the two RED crates) went through the venue: eleven Fauna `safe-to-deploy` audits with their exemptions removed, both REDs cleared, `wasmtime-internal-cranelift` cleared by ruling alone; the 33 other Bytecode Alliance crates rest, by owner ruling, on the import's trusted-publishing wildcard audits plus the 7-day minimum release age, with no Fauna review. Both clearances are in the pre-scan ledger, whose baseline since 2026-10-05 holds any RED until it is ruled (§ Reviewing untrusted source → *The pre-scan ledger*); the previous-push baseline that forgot the 2026-10-02 landing's REDs after one run is retired. Publisher trust is ruled (no `[[trusted]]` entries; the *Publisher trust* bullet).

- **Dependency verification is hollow, and the gate had drifted RED (repaired
  2026-06-28).** `cargo vet` rests on **770 exemptions** with **22 Fauna audits**
  as of 2026-07-07 (`supply-chain/{config,audits}.toml`; the first landed
  2026-06-28 — the `async-trait`/`futures-macro` pilot; Batch 1a followed). Before the repair, `cargo vet --locked`
  was **failing**: (a) **165 deps** were unvetted-and-unexempted (`Cargo.lock`
  outgrew the `[exemptions]` snapshot as the fleet added/bumped deps without running
  vet); (b) a stale `[policy.fauna-bridge-smtp]` referenced an excised crate; (c) the
  vendored `ksni` fork (`libs/ksni`, path dep, version-collides with crates.io 0.2.2)
  lacked an `audit-as-crates-io = false` policy. All three are fixed; green restored
  by refreshing imports + `cargo vet regenerate exemptions` (now `just
  vet-regen-exemptions`). The gate (a **hard** `cargo vet --locked` job) runs on
  `pull_request` (skipped by the merge gate's ff-push), daily (weekly until 2026-10-04), on `main`-push
  touching dep files (the merge-path trigger, S5a), and in `release.yml`.
  **`--locked` is now enforced in every shipped build (S6, DONE 2026-07-05):** the nest-image `Dockerfile`'s
  native cargo builds + wasm-pack builds, `release.yml`'s root-workspace
  builds, and the app justfile FFI recipes (android/apple/windows/linux/
  mail-bridge) all pass `--locked`. **⚠ Follow-up (2026-07-07):
  the deferred `build-nest-image.yml` validation
  finally ran and FAILED** — a change added `--locked` to the nest Dockerfile
  but was only "locally verified against the in-sync root `Cargo.lock`" (the FULL
  workspace), never in the Dockerfile's own context, which STRIPS the
  apps/services/tools workspace members (their source is `.dockerignore`d). That
  strip is fundamentally incompatible with `--locked`: pruning app-only crates
  collapses cross-boundary duplicate versions (quick-xml / textwrap /
  unicode-width, pulled at different versions by kept runtime crates via c2pa/clap3
  vs stripped apps via clap2/ksni) to a single unqualified ref in a *kept*
  package's lock entry, so the committed full-workspace lock is not canonical for
  the stripped subset → `cargo build --locked` fails "cannot update the lock file".
  **Fix (same change): keep `--locked` (policy preserved) and stop stripping —
  the Dockerfile now COPYs the member trees (un-ignored in `.dockerignore`) so
  cargo resolves the full workspace; the members are build-graph leaves, resolved
  but never compiled, and never reach the runtime image.** Real-build GREEN
  CONFIRMED: `build-nest-image.yml` run 28851510054 succeeded
  2026-07-07 (34m, all steps incl. amd64 smoke-test + multi-arch manifest push),
  cutting a fresh `:latest` — so `--locked` in the shipped nest image is now
  validated by a real production build, not just locally. The app justfile FFI
  recipes + `release.yml` were unaffected (no member strip). The
  injection-resistant **mechanical pre-scan gate landed 2026-06-30 (S5b); like the rest of this
  workflow it went from CODE ONLY to actually executing once the 2026-07-17 self-hosted-runner
  repair landed (see the ⚠ block at the top of this section) — stale text corrected 2026-08-21:
  re-verified live via `gh run view` on several recent `main`-push runs, `dep-prescan` and
  `cargo-deny` both pass green consistently** (the workflow's overall `failure` conclusion on
  most of those runs is `cargo-audit`, which is designed to stay red while any accepted-but-
  unfixed vulnerability sits in the dispositioned set — not this gate's doing; `cargo-vet` drifted
  red too between 2026-09-02 and 2026-09-10, see [`dependency-verification-log.md`](dependency-verification-log.md) → Update (2026-09-10)
  above for the fix):
  a `dep-prescan` job
  (Trojan-Source/injection/build-script-network scan of every registry crate the committed
  pre-scan ledger has not seen — of the crates a push introduced until 2026-10-05 —
  fail-closed while any RED in the lock lacks a disposition, daily as well as on the
  dep-path triggers, + an advisory `::warning::` for any new crate lacking a Fauna audit;
  § Reviewing untrusted source → *The pre-scan ledger*) and a **`cargo-deny`** source/ban/license job (`deny.toml`:
  crates.io-only sources, permissive-license allowlist, multiple-versions=warn), both on
  the dep-path triggers as post-push DETECTION (not merge-time blockers); a `dep-tools-checks`
  job in `ci.yml` was meant to guard the gate's own tooling but `ci.yml` is still
  `workflow_dispatch:`-only, so it guards nothing on any automatic path. ⚠ The **boolean** `cargo vet --locked` gate
  still **drifts red whenever a dep is added/bumped without re-running vet** (the fleet does
  this) — `just vet-regen-exemptions` is the manual re-green, and `just
  dep-prescan-ledger-update` the pre-scan ledger's (the session landing a dependency runs
  both); the ledger grandfathers nothing — a RED stays RED until the venue rules it.
  (1142 locked packages, 1042 third-party; `dep-inventory` is the inventory.)
- **Control (c) — the re-vendor drift check — is BUILT and green (2026-07-16).**
  A dedicated dev-fleet drift checker + `supply-chain/vendored-forks/manifest.json` +
  `dep-vendor-drift`, gated by the `vendored-forks` job in `supply-chain.yml` and
  self-tested by its own test suite (13 tests) in `ci.yml`'s
  `dep-tools-checks`. It covers every live fork against its committed pristine
  `.crate` — `libs/ksni` @ ksni 0.2.2 (9 of 24 byte-identical; 15 pinned, plus 2 added files — `PATCH.md` and, since 2026-10-05, the committed code-generator output).
  Authoring it corrected a premise worth keeping: the divergence is **not** fully
  self-labelled — ksni has no markers at all
  — so the check pins bytes rather than trusting labels (§ *Vendored forks*).
- **The CI blind spot is CLOSED for declared forks (2026-07-16).** The same
  `vendored-forks` job now runs `check_declarations`: `[patch.crates-io]` path overrides ⊆
  policy entries, policy-entries-with-the-flag == manifest forks (a bijection), flag ==
  `false`. Had it existed on 2026-07-15, a fork landing without its
  `[policy.<crate>]` entry would have failed CI immediately instead of sitting outside
  cargo-vet and cargo-deny for a day. It sidesteps `--locked`'s limit by never asking
  whether a name shadows a published crate — only that the local declarations agree.
  **Still open (accepted, not deferred work):** a *fully undeclared* fork — no `[patch]`,
  no policy entry, no manifest row — remains undetectable offline for the reason
  `--locked` exists; plain `just vet` is the backstop. See § *Vendored forks*.
- **The exemption backlog drifts red continuously.** Re-greened 2026-07-16:
  23 crates were unvetted (the `fauna-tui` ratatui stack, the QR path, `imap-proto`,
  `serde_bare`, `rustix`, `linux-raw-sys`, `lru`) — 20 re-grandfathered as exemptions, and
  `strum`/`strum_macros`/`unicode-width` covered by real imported Google/Mozilla audits that
  a refreshed `imports.lock` pulled in. Gate: RED → GREEN (196 fully audited, 9 partially,
  975 exempted). This recurs on every dep-adding merge; `just vet-regen-exemptions` is the
  re-green and adds **backlog, not review**.
- **No trust tiering.** The credential a developer machine holds is a
  general-purpose one, not a scoped release credential: the machine that writes
  the code is the machine that can push it, and a machine that edits CI
  workflow files necessarily holds the permission to rewrite them. There is no
  separate release identity and no step-up between the two. *(User-ratified
  2026-06-28: leave it as-is for now — the credential is instrumental and
  recoverable, not the asset. Revisit when a release identity exists to move
  to.)* The exposure is also wider than any credential: a developer machine
  holds standing interactive access to the machine that builds, verifies, and
  promotes release images (re-verified open 2026-08-30), so there is no
  separation between the machine that writes the code and the machine that
  produces the artifact — a credential's scope bounds what a compromised
  developer machine could push; access to the builder bounds nothing.
- **No enforcement point.** The merge gate fast-forward-pushes `HEAD:main` with
  no human review, so nothing between a developer's commit and `main` is
  review-gated. Branch protection on this repository — squash-only merges, a
  required green `ci-pass`, enforced for administrators — is the target posture
  (user-ratified 2026-06-28).
- **Consumers verify nothing.** Watchtower polls ghcr and recreates the container
  with no image-signature check; the web SPA is TLS-only; no app verifies an update signature of its
  own, because none downloads an update (`installers/README.md` § Knowing a newer
  version is out).
- **Provenance ≠ source integrity.** cosign keyless on release binaries
  (`release.yml`), `attest-build-provenance` on web, `cargo vet`/`audit`/
  `govulncheck` (`supply-chain.yml`) attest *"built by our CI from our repo,"* not
  *"the source is trustworthy."*
- **The highest-value surface is the least-gated.** ios / android / macos are built
  on dev machines with local signing keys, no CI, no attestation; only app-store
  review (Apple/Google) gates them externally. Direct-download apps
  (linux/windows/macos) have only Fauna's own pipeline. **Exception in flight —
  the macOS `.dmg` (user-decided 2026-08-25):** it is built and attested by this
  repository's own `Release (macOS)` workflow on GitHub-hosted runners
  (`installers/macos.md` § Build Pipeline → *The `.dmg` release pipeline*).
- **Interim release-signing custody for the macOS download — GitHub Environment
  secrets, not a Tier-0 machine (USER-APPROVED 2026-08-25).** § Trust tiers puts
  the release-signing key on Tier 0, which has not been provisioned. Rather than
  wait, the Developer ID Application certificate (`.p12`), the App Store Connect
  notary API key live as
  secrets of the public repository's `release-macos` Environment, whose rules are
  **required reviewer = a maintainer, `v*` tags only**. That exposes them only to a
  run a human approved, on an ephemeral GitHub-hosted runner, and never to the dev
  fleet (Tier 2) or to a pull request — Tier-1 custody with a human approval
  standing in for the Tier-0 machine. Honest limits: GitHub holds the key material
  at rest; a compromised maintainer account plus an approved run can sign; the
  Developer ID certificate is revocable and every notarization is on Apple's record,
  which is the detection trail. The Tier-0 move stays the target; this bullet is the
  interim, chosen so the download ships now. A self-hosted "trusted" runner holding
  the same credentials was rejected: it weakens the provenance (the machine attests
  itself) without moving the key off the dev-fleet host. **All three halves of the
  `build`/`sign` privilege separation this design rests on are machine-enforced
  : no credential in the `build` job's
  scope, `sign` gated behind the Environment's required reviewer, and — closing
  a gap a security review found the same day the other two shipped — no job
  outside `sign` may carry its own `permissions:` block, and `sign`'s is pinned
  to an exact value (`test_only_the_reviewed_jobs_carry_their_own_permissions_block`),
  so a job-level `GITHUB_TOKEN` grant (which replaces the workflow-level one
  for that job) can no longer walk around the secrets/environment checks unseen.**

**Sequencing toward the target** (design doc § 3.5, re-ordered after the 2026-06-28
decisions): **(1) dependency verification first** — enforce `--locked` in shipped
builds; run vet/audit/deny on the real merge path; define + start vetting the
shipped closure (build-time-exec crates first); add `cargo-deny`. **Soft rollout
(USER-RATIFIED): the reading is done by Claude in interactive dev-machine sessions
(reading source executes nothing — safe on a credentialed machine), and CI's only
job is the boolean gate `cargo vet --locked` (already present), which tightens
automatically as the exemption backlog is burned down to real audits — no automated
signal/CI pipeline required.** ⚠ **Refined 2026-06-29** (§ *Reviewing untrusted source
without being subverted*): credentialed-machine reading holds for **high-reputation,
already-shipping** crates only; **low-reputation / new source — the ECS surface first —
requires the containment + verdict-integrity process** (an LLM reviewer is injectable
even though reading runs no code), and going-forward the review is a **CI gate**, not a
manual session. (Design ratified 2026-06-29; tracked internally.) **(2)** detection
+ transparency (rides on open-sourcing). **(3)** client blast-radius limits. **(4,
last)** Tier-0 + release signing + consumer verification (needs the user to
provision Tier-0). The token-narrowing and paid-plan levers are **declined for now**
per the decisions above.

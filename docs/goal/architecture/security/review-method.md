# Adversarial security review — the standing method — target state

Owns: security-review-method
Status: ratified
Authority: owns *how* a reviewer conducts an adversarial security review of landed code — the standing charter rules, the evidence standard, the finding/verify-back contract, and the failure modes that have repeatedly produced false CLEAN verdicts. Consumes without redefining: what the runtime crypto discipline *is* → [`../security.md`](../security.md); the four-tier test taxonomy + the e2e conventions → [`../testing.md`](../testing.md); who may ship code at all (supply chain, dep-source reading) → [`../release-integrity.md`](../release-integrity.md); the PQ overlay → [`post-quantum.md`](post-quantum.md). This doc states only the *method*; it never states what the system's security properties are.

> **Audience:** anyone reviewing security-relevant landed code, and any contributor
> whose change will be graded that way. These rules are general — anyone grading a
> security-relevant delta should read them.
> **Purpose:** these rules were each earned by a specific miss. A review that skips
> them does not merely review less — it reliably produces a **false CLEAN**, which is
> worse than no review, because later reviewers inherit the verdict and
> stop looking.

## Why this is a goal doc and not a working note

For a long time this charter lived in one review track's working notes, readable only by
people working in that track. Every rule below is general: they apply whenever anyone grades
a security-relevant change, and several were *learned* while working in other areas. A
method only one track can read is a method most reviewers will re-derive from scratch or miss
entirely. Graduated 2026-08-02.

## The charter rules

Each rule was earned by a specific incident while grading landed code.

### ⭐ Verify a production caller exists

For any exported crypto / recovery / GC / reconcile / authz capability, **verify a production
caller exists** — not merely that the entry point is correct and its tests are green.
**A dark rail is indistinguishable from a working one in every test written for that rail.**

Three instances in eight days, two of them severe: `resume_pending_removals` (dark on all apps — a
verdict of MINE rested on it); `seal_segment_bytes`/`get_index_manifest` (dark on every nest);
`SyncConfig::backup_key()` (dark on every embedder). Each was an exported capability whose own
tests were green and whose production caller did not exist.

**Name-based greps are a floor, not a ceiling**, and an FFI/wasm wrapper is **not** a caller.

**Corollary — before prescribing a guard on a write path, trace who legitimately writes it.**
Learned twice on the prescribing side: a one-liner guard I prescribed had an untraced live
consumer and would have broken production.

### ⭐ No catch-all on the destructive side

**A classifier whose two answers have asymmetric blast radius must never reach the destructive
answer through a catch-all.** `_ =>`, `unwrap_or(false)`, `.ok()` landing on the
delete/skip-seal/skip-authz branch is a bug **even when every enumerated arm is right**.

Instances: `_ => false` swallowing a blob-store read `Err`; `GatedRemoval::NoGate` conflating
no-gate-by-design with not-engaged; `stored_hashes = None` conflating unencrypted with unknown.
**Grep the destructive side directly.**

**Extension (the class went five-for-five):** a `match` on a foreign crate's error enum makes its
carve-out at exactly *one* nesting depth — enumerate **every** nested enum's variants. And **when a
fix's whole point is "enumerate exhaustively, no catch-all", grep that same fix for its last `(_)`
or `..` arm before ratifying**: the discipline stops at the depth the author was looking at.

### ⭐ A second consumer inherits the oracle, not the caller's other guards

When prescribing "reuse X's oracle", **enumerate everything X does between the oracle and the
destructive act.** GC's creation-grace and must-have-metadata guards were silently not adopted by
the purge that reused its oracle.

### ⭐ Name both halves of a composing finding

**A finding with two composing halves gets fixed on the half that is easiest to name.** Give each
half its own name and its own verify-the-fix line, or the unnamed half silently ships as fixed.

### ⭐ A fix that adds a door orphans tests that manufactured the pathology

When a fix adds a door that refuses a pathological input, **grep the test tree for pre-existing
seeds of that exact input.** A red-first pin proves the new door, but a test that *manufactured*
the pathology through the same door is now orphaned — and the cheap merge gates will not catch it.

### ⭐ A fence around a FIELD is not a fence around the PROPERTY

Enumerate every field that reaches the same sink. Fencing one field leaves its siblings in the same
`format!` armed.

### ⭐ A fix's blast radius is the SINK's, not the field's — and a sink is a *renderer*, not a screen

When a fix lands at a composition boundary, **ask which other compositions feed the same
renderer** — grep the painter for its callers, not the finding for its field. A defense placed
*after* a structural split can never cover the splitting character. And **one column with two
writers has two threat models**: a per-column safety claim is only as strong as its weakest
writer, and when a writer's constraint is an accident of an unrelated protocol, say *unproven*,
not *safe*.

### ⭐ A reachability question must name the ARM, not the field

**The arm that validates least is the one that resolves fastest.** A classifier with N arms owes N
reachability answers; read the classifier's *dispatch order* first. A correct answer about the
wrong arm produces a wrong bound.

- **The arm that skips the expensive step also skips the validation that rode on it.** When a
  defense is a by-product of an unrelated operation (a URL fetch screening control characters),
  enumerate who doesn't perform that operation.
- **A documented carve-out's security argument covers the property its author was defending, and
  stops there.** A thorough security comment is not a thorough security review of the same code.
- **When a bounding clause is refuted, re-derive the severity — don't just raise it.** A refuted
  bound is an invitation to re-run the severity argument from the start, not a ratchet.
- **A path that exists in `src/` is not a path that exists in the SHIPPED ARTIFACT.** Two things
  stand between a source-visible arm and a release build, and the read that *finds* the arm sees
  neither: **cfg-gating** — a `#[cfg(feature = "…")]` arm is compiled out of release artifacts
  (e2e convention 15) — and **wiring order**, where an earlier return hands back a different value
  on every path the arm claimed. Check both before crediting a window, and say which you checked:
  *"the code is there"* is evidence for neither. Measured 2026-08-26: a CONDITIONAL severity
  rested on three named windows, a source read had found all three, and **two failed exactly this
  check** — the loopback nest always synthesizes a floor cert before the certless resolver is
  consulted, and the production pre-ACME case returns the floor resolver first under an explicit
  ordering the comment above it states outright, leaving the certless branch reachable only if the
  floor *write* failed. Only the third window was real, and it was `#[cfg(feature = "test-hooks")]`.
- **The converse is a finding, not a clearance: "absent from release artifacts" ≠ "unreachable".**
  A `test-hooks` arm is live on every tier_3 and dev nest, and a degraded branch — a disk error, a
  failed write — is a real branch with a real reachability story. What this check retires is the
  *severity condition the finding named*, never the window itself; say which one you are retiring.
  This is § Evidence standard's negative-half rule applied to reachability: *un-cleared* and
  *clean* are different words.

### ⭐ A guard whose failure is silent lives inside the consumer, not the callers

A guard the caller must remember is a guard that can be unwired — and **mutation testing is what
tells you which kind you wrote**: delete the guard's call and see what reds. When the guard's
failure mode is silent (a vacuous verdict rather than an error), place it at the head of the
function that consumes what it guards, ahead of that function's own early returns — then every
caller reaches it by construction and none has to know it exists. Proven in the bound-(3) build:
the drive-loop placement survived its deletion mutation (no tier_1 test constructs the caller);
the in-pass placement cannot be removed without a red.

**When the caller must pass the door earlier than the write, carry the verdict as a ticket.** The
head-of-the-consumer placement assumes the guard can run at the moment of the write. Sometimes it
cannot: something between the check and the write must not happen for a value the guard would
refuse — an irreversible side effect, or a second refusal that would steal the first one's voice.
Checking at the write reorders those; checking at each caller is the forgettable copy again. The
shape that keeps both is a **newtype the consumer demands and only the guard can mint**: the caller
passes the door where it must, holds the verdict as a value, and spends it at the write, while the
compiler refuses any arm that never passed. Borrow the guarded value inside the ticket so it cannot
be spent on a different one. Built out for the account plane's per-entry cap
([`account-sync-plane.md`](../account-sync-plane.md) § Implementation status today → *the door is
a ticket, not a step*), where the write sits behind a step that can mint a generation.

**And where neither placement is reachable by a test, pin the mechanism.** "No other arm bypasses
the seam" is not a behaviour: a bypassing arm is green everywhere. Assert the source shape instead
— `include_str!` the module, count the consumer's call sites, fail on the second. It is a cheap,
honest witness for exactly the property the deletion mutation says nothing about.

### ⭐ Pinning a pure predicate does not pin its use

The argument-position rule one layer out: N green pins on a predicate coexist happily with a
production caller that ignores it entirely. **Ask of every new predicate: is there a test that
fails if the production caller stops consulting it?** If the caller is unreachable from tests,
split the decision out as a reachable seam and pin the seam. Proven in the bound-(3) build:
`predecessor_keys_may_be_dropped` had seven green pins while the delegate calling it could have
answered `true` without reading the roster; the fix was the `license_from_engines_answer` seam.

### ⭐ A union pin must give EACH source something the other lacks

A pin asserting "the union of A and B survives" whose fixtures make B a subset of A never routes
through B at all — deleting the merge leaves it green, because the assertion was about dedup and
its B half was fixture. Seed each source with an element the other lacks and assert the count that
only the true union produces. Third sighting of one family — the argument-position rule, the
pure-predicate rule above, and this — all "the assertion never routed through the thing the
sentence is about"; the mutation run is the only cheap detector for all three. Proven when that
gap was closed: the first union pin seeded `[shared]` vs `[shared, registry_only]` and survived
deletion of the carry-forward; re-seeded `[shared, blob_only]` vs `[shared, registry_only]`
asserting three, it reds exactly.

### ⭐ An agent-only path is a worse witness than no path at all

A missing feature is visibly missing; a mechanism whose *only* caller is the test harness reports
green every e2e run, honours the command, posts the real Commit — and is unreachable by any user.
The detector is not "does this symbol exist" (it does, twice) but **"which non-test call sites
reach it, and is any of them a gesture?"** Same family as the app-gate skip-class rule: the thing
that hides is the one that looks like coverage. Proven when that hole was closed: tui's
`thread-member-chip` renders as a plain label while the removal op is fully wired behind the e2e
agent seam (`conversations_real_remove`), so the remedy the succession copy names was exercised
every run and reachable by nobody — on the lead app.

### ⭐ A throttle pin whose template payload is invalid cannot witness the honest caller

The existing throttle tests' template (register / invite-submit) reaches the gate with a
deliberately bogus signature and asserts only "the reply is not `rate_limited`". Copied for a new
throttled kind, that shape proves the flood is refused and says nothing about the claim a throttle
change actually owes — that the limit does not cost the legitimate caller their first call. Sign a
genuinely valid payload so the owner's success and the flood's refusal are one two-sided test;
where the template's invalid payload would error for its own reasons, the template cannot be
copied at all, which is the tell. Adopted from the lockout throttle's verify-back:
`lockout_is_rate_limited_per_source` signs the real 40 bytes and asserts `reply.ok` first.

### ⭐ A comment can be a correct quotation of a wrong ratification

When a code comment carries a wrong security rationale, grep the owner doc before concluding the
comment is the bug: the boundary-file comment that justified one such hole was a faithful
transcription of a ratified `transport.md` sentence — the general principle, stated three rows
below a table that already contradicted it. Fixing only the comment leaves the doc teaching the
next kind the wrong rule; the fix must land in the owner doc and every quoting site in one change.
Mirror of the one-owner rule: restatements drift, but they also *propagate*, and the copy in the
code is the one a reviewer reflexively blames.

### ⭐ A pin against a CALLER obligation cannot assemble the call itself

When the defect class is "the caller forgot X" — a superset policy slice, a required flag, a
context argument with a legitimate empty meaning — any test that supplies X correctly is the
compliant caller, and it tests the callee, which was never in doubt. The first draft of the
subset-edge pin loaded `superset_authored` and passed it itself; it survived the
mutation that deletes the production load, while the twin that drives the real resolver
(`resolve_effective_policy`) reddened. The pin must enter through the same door production does,
and the mutation run — not prediction — is what exposes a pin that quietly took the wrong door.
Found by the implementing session running its own mutation matrix; the structural complement
is collapsing the obligation to one production chokepoint so the pin has exactly one door to test.

### ⭐ A ruling axis named by PURPOSE misses the members that share the CAPABILITY

A security ruling reached by asking what a table's rows are *for* finds the members that share
the **purpose** and misses the members that share the **capability**. The `email_filters`
succession ruling's axis was *"where does a copy of this account's mail go next"* — which is
exactly what hid the autoreply: an autoreply sends no copy anywhere, yet it carries
the same capability as the forward it was ruled alongside (standing authority to make the nest
emit content outward under the account's identity), and unlike the forward it was already live. When ruling a family, name the **capability class** the dangerous members
share, then enumerate by that class — and prefer a predicate the compiler owns (the fix's
exhaustive `match` on the action enum) over a hand-written list, so the next member cannot join
the family un-ruled. First stated by the implementing session that closed the underlying finding;
ratified here at that finding's verify-back.

## Evidence standard

- **Prove findings with a probe, not by reading.** Write a throwaway test **whose assertion is the
  claim you expect to be FALSE**, so a passing run refutes you and a failing run hands you the
  finding's own evidence string. A whole turn's five findings were each produced this way.
- **Probe hygiene is non-negotiable.** Create the probe as its own `tests/zz_review_probe.rs` —
  never edit the crate's real test file — and before committing run `git status --short` and
  confirm it is **empty**. A probe needing a dev-dependency also dirties `Cargo.toml` *and*
  `Cargo.lock`.
- **Grade a fix by re-running its own named mutation**, not by reading the diff. Confirm both the
  count and *which* test reddens — an exact contract reddens exactly the named test, and collateral
  reddening is itself a finding.
- **Never inherit another reviewer's conclusion on a finding** — re-verify every link by hand
  before filing.
- **State the negative half as loudly as the positive.** What the attack does *not* reach, what
  remains untested rather than cleared, and what you did not trace at all are part of the verdict.
  "Un-cleared" and "clean" are different words.
- **An exact-string grep over formatting-sensitive source gives false negatives that read exactly
  like a closed finding.** Anchor on the identifier, never on spacing.
- **A goal doc's "live" status column means "the capability exists", NOT "an embedder calls it."**
  Both dark rails were marked *live* while nothing called them. Treat a conformance-row status
  as a claim to verify.

## Scope and process

- **Reviews are READ-ONLY.** Entrust each fix to the track that owns the code (append-only, and
  read the current state before writing); never fix in-flight files a concurrently-running track
  owns.
- **The pathspec is `bins/ libs/ scripts/publish/ services/`, and it only ever WIDENS.** It began
  `bins/ libs/` when it was first written down (2026-08-26 — never *"at the charter's start"*: the
  standing review is older than any date this page once gave for it, see the *no span of history*
  bullet below), widened permanently to add `scripts/publish/` — the publish transform is release
  infrastructure, and whatever it can rewrite ships to every user. **`services/` was added
  2026-09-19 on exactly that reasoning:**
  `services/fauna-front-door` and `services/fauna-cors-proxy` are attacker-exposed Rust binaries
  serving the public web door, and the apex hosts the install guides and the official-apps page that
  tell users which download is genuine — so *"whoever controls the door influences what software users run"*
  ([`front-door.md`](../front-door.md) § Goal). An attacker-facing binary on the software-download
  channel that the standing review structurally never enumerates is the exact silent-coverage
  failure this charter exists to prevent; leaving it out had never been a reasoned ruling, only an
  artefact of the door being lifted into `services/` (2026-08-21) after the pathspec was first
  written down (2026-08-26). **Do not re-narrow.** A widening edits the probe's default path list,
  its code-path matcher and its landing-path list together, with a self-test case, or the probe
  stays blind to the new tree.
- **Enumerate the fresh window against the graded ledgers at OPEN, not at close** — and re-grep
  owed-back statuses **after the last rebase before halt**, because a sibling's fix and its
  verify-back both arrive mid-session.
- **A commit leaves the fresh window only by a recorded DISPOSITION — never because the base
  advanced past it.** The window is a scheduler, not a ledger: `<base>..<head>` is re-cut at
  every OPEN, and whatever the new base excludes is excluded from every later turn too. So a base
  may advance only over code-bearing commits (`bins/`, `libs/`, the Go bridge) that each carry one
  of three dispositions **by SHA** in a dated verdict record: **graded** (the turn that graded it),
  **ranked out** (one line naming why it is not a security surface — a lift, a test pin, a
  rename), or **out of scope** (the rule that excludes it). A summary count — *"~30 code-bearing
  commits, most claims/docs"* — is not a disposition, and a carried ranking that names only the
  top three is not one for the rest. Everything the ledger does not name by SHA is still *in* the
  window, and the base stays behind it. The check is a grep, not a mechanism: a code-bearing SHA
  older than the base that no verdict record names — and whose own commit **body** cites no
  finding or verdict (the track's own fixes cite theirs in the
  body, not the subject; read `%B`, never `--oneline` — see the `%B`-arm bullet below)
  — has fallen through. Measured 2026-08-26: a nest + Go-bridge change of the inbound MTA's
  bounce-vs-defer decision landed inside one turn's window, that turn's record summarised the
  window as a count, the next turn's base was the previous turn's close, and the commit sat
  fourteen days ungraded — surfacing only because an unrelated row happened to be refused.
- **No span of history is outside the charter: the per-SHA standard above runs from the
  repository's first commit, and a commit's AGE is never a disposition.** A cutoff date is a
  measurement convenience. For a while this page dated *"the charter's start"* to 2026-08-01;
  that was one drain's `--since`, copied into prose, and nothing began that day. Date the review
  from what its records *say*, never from their filenames: the standing track's first record and
  its queue are 2026-06-23, its turns are numbered by 2026-06-27 (a record of that day cites an
  earlier turn by number), this page
  graduated 2026-08-02, and the per-SHA rule and its `DISPOSED:` grammar are 2026-08-26 and
  2026-08-29. None of those dates ends an obligation, because the rule they lead to is about
  what a base may advance over, and every base ever cut sits in front of all of that history.

  Ruled 2026-09-20, against three
  candidate shapes. An exclusion is the unrecoverable direction, so a span may leave the
  obligation only by a stated reason a later reader can check — and the two reasons on offer
  were both measured false:
  - ***"Old code has been superseded."*** Measured 2026-09-20, **1,159,179 of the
    pathspec tree's 1,993,085 lines — 58 %, in 2,665 of its 3,964 files, 931 of them wholly —
    were last written before 2026-08-01**
    (`git blame <last commit before the date>..<head>`, counting boundary lines).
  - ***"It predates deployment."*** The first production image predates the alpha phase
    ([`version-compatibility.md`](../version-compatibility.md) § 0 *The trigger*, and the
    refused-removal precedent under its Dimension 2), so no date inside the repository's life cleanly separates code
    that never met real data from code that did.

  **A tree-surface review is not a substitute, and the dated surface reviews dispose of
  nothing.** Reading today's surfaces instead of the commits that built them is the larger job,
  not the smaller one (the 1.16 M lines above, against a commit set of thousands), and it is
  blind by construction to the classes history shows and the tree hides: a guard a commit
  *removed*, at-rest data written by code since replaced, a secret committed and later deleted.
  The un-numbered dated reviews (back to 2026-03-12) and every surface review since stay what
  the marks bullet below already makes them — evidence a grader may cite on the `DISPOSED:` line
  it writes, inert until one does.

  **What the ruling leaves open is ORDER and PACE, never the standard.** Order is exposure
  first, then newest first, in tranches a probe command can bound: a tree that entered the
  pathspec late and so sat in no window at all; then the span from the track's first record
  (2026-06-23) to the cutoff, whose code is the likeliest to be alive and deployed; then the
  rest, newest first. *Exposure* is mechanical, never a pass's own reading: the probe lists
  the residual in drain order from one tier table (3 attacker-facing ingress, 2 trust and crypto, 1 the rest, 0
  test-only; a commit takes the maximum over its files; a renamed tree keeps its old path in
  the table, because history is ranked by the paths a commit touched). A drain's success measure is the probe's **RESIDUAL**, never its
  never-named count — *naming is not marking*, and a drain that closed on the second left over
  a thousand commits in the window. Pace is a cost that is funded explicitly, tranche by
  tranche. An unfunded tranche stays in the residual, reported by every probe run as owed, and
  that is an honest state; what is never honest is a coverage claim that reaches across it.
  § Implementation status today → *Corpus coverage* says which spans are drained.
- **A window bounded by a DATE must quote an absolute timestamp — a bare `--since=<date>` DRIFTS.**
  When a backlog sweep bounds the excluded-commit set by date rather than by base, git parses a
  bare `--since=2026-08-27` with **approxidate**, which resolves relative to *now*, so the same
  command over unchanged history returns a smaller window every hour. Measured 2026-08-31 on one tree, minutes apart and with nothing
  in history changing: `--since=2026-08-27` gave 277 commits, then 275, while
  `--since='2026-08-27 00:00:00'` gave **367** — the recipe as written was 92 commits short of its
  own window, and that alone explained a 294-vs-277 discrepancy an earlier turn had recorded as
  unreconciled. The failure mode is the one a disposition ledger cannot tolerate: the drift moves
  commits **out** of the measured set, so a sweep reports *"0 fallen through"* while commits the
  base permanently excludes have never been read by anyone. Always quote the time
  (`--since='<date> 00:00:00'`), and treat a residual count that shrinks without marks being
  written as evidence of a moving cutoff rather than of progress.
- **A measurement's recorded ANCHOR must be a SHA that resolves on the shared integration
  branch, or an absolute timestamp — a reviewer's pre-merge local HEAD is orphaned once its
  commits are rebased, and a number anchored to it describes a tree nobody can reach.** A later
  pass differences its counts against the recorded ones, so that difference is only as good as
  the anchor beside it.
- **The disposition corpus is the dated verdict records, the close archive, and commit bodies —
  NEVER the live work queue**, which is where a track writes its carried ranking of next
  candidates. Include it, and naming a commit as the next pass's top candidate becomes what
  removes it from the next pass's candidate list: the higher a commit ranks, the more certainly
  it is erased.
- **A disposition is a POSITIVELY MARKED claim, not a mention — the probe must grep marks, not
  SHAs.** The bullet above dropped the live queue from the corpus because that is where the carried
  ranking is written. That fix was right and insufficient: the ranking is written into the corpus
  it *keeps* as well — every verdict record ends with a "Carried TOP" section for the next turn, and those
  roll into the close archive verbatim — so the identical bias returns one file up, and naming a
  commit as next turn's top candidate is still what removes it from next turn's candidate list.
  Measured 2026-08-29, on the corpus the bullet
  above had just corrected: **all nine of that turn's carried candidates read as disposed, none graded.**
  ⚠ **And stripping the ranking sections does not fix it either — the mention need not be a ranking
  at all.** Re-measured with every `Carried TOP` section removed, all nine still read as disposed,
  now by prose no rule would think to strip:
  - One commit — matched in the previous turn's **window enumeration**, inside the sentence
    *"Both remain **undisposed and un-graded**"*. The grep read the very sentence declaring
    non-disposition as a disposition.
  - One commit and its seven siblings — matched inside that same record's **measurement table**, the
    evidence block of the finding *about this leak*. The proof erased its own subjects.

  So the mechanism cannot be subtractive (drop a file, strip a section): prose that merely *names* a
  SHA is unbounded, and every subtraction is a guess about where naming happens next. **Invert it —
  a disposition is claimed explicitly or not at all.** A verdict record disposes of a SHA on a line
  carrying the literal marker `DISPOSED:`, one per SHA, naming which of the three dispositions it is
  and why:

  ```
  DISPOSED: <sha> graded — foreign-resolve fail direction, one finding filed
  DISPOSED: <sha> out-of-scope — openMLS fork prune, per the standing ruling
  DISPOSED: <sha> ranked-out — wraps a row's citations, no security surface
  ```

  The probe greps `^DISPOSED: <sha>` and each candidate's own `%B`, and **nothing else** — rankings,
  enumerations, measurement tables, and narrative prose are all inert by construction, in any file,
  forever.

  ⚠ **RUN THE PROBE SCRIPT — do not re-derive this bullet by hand (ratified 2026-09-01).** The
  probe is executable: it enumerates the set, applies both arms, refuses rather than guessing, and
  prints residual MEMBERSHIP plus a two-way diff against a claimed list. Everything below this line
  is the rationale it encodes, and it is worth reading — but it is no longer the executable. Four
  consecutive turns applied the arms by hand and got four different answers to the same mechanical
  question; the fourth was the turn that had just written the rule.


  Three properties are load-bearing, and each exists because a hand-application lost it: **only a
  positive, recognised self-disposition claim leaves the residual** — an unreadable body is
  reported `UNDECIDED` and *stays in*, because a wrong exclusion is unrecoverable and a wrong
  inclusion costs five minutes; **an arm-1 mark that prefixes two commits in the set is a refusal,
  not a coin-flip**; and **a bare `--since=<date>` is refused outright** as approxidate. The
  script's self-test is the red-verify: it feeds the probe one body of each excluded class and one
  genuine verdict commit, and fails if the classifier stops rejecting. ⚠ **And the `%B` arm is satisfied only by a citation OF THIS TRACK — the track name or a
  finding key. A bare `row N` or `tNNN` is a mention, and mentions are inert here too.** The arm
  exists because this track's own fixes cite the finding they close in their body; it is not a
  general "was anything cited?" test: reading the arm loosely disposes of
  the backlog on a token that claims nothing about a SHA's security disposition. Measured 2026-09-01 over the 367-commit set a backlog sweep had
  closed as *"0 fallen through"*: 223 carried a real mark and 24 the track's own citation, while
  **120 — a third of the set — were disposed by nothing but a row or turn number**, among them a
  leak-gate fix and a data-plane re-seed, all of them already excluded by an advanced base. The
  sweep's marks were sound; it was its *residual* that the loose arm erased, and no later turn would
  have re-enumerated them. ⚠ The generalizable shape, and why this bullet replaces rather than extends its
  predecessor: **both earlier fixes asked "which text should the probe not read?", and that question
  has no stable answer — the leak is the grep's inability to read intent.** The 2026-08-29 record saw its ranking
  sitting inside its own corpus and reasoned it "safe (this is a verdict record, which *is* a
  disposition corpus)": corpus membership was the premise, and it was the wrong half of the
  question. Ask what a *sentence claims* about the SHA — and make it say so in a form a grep can
  check — never what file or section it sits in.

  ⚠ **Two corollaries measured 2026-09-01, both of them the same rule applied to
  the probe's own two arms.** They are recorded here because each was, in its turn, a *literal*
  reading of the bullet above that still got the answer wrong.

  **(1) The `%B` arm is a claim test, not a string match — and the phrase is spelled two ways.**
  Searching a commit body for the track's name finds, among others, commits whose entire subject is
  the track's own vocabulary: one draining dated/severity review citations out of `libs/`, another
  scrubbing a `for ratification` process line out of the tree. Those commits *mention* the track
  because they are **deleting its name**, which is the purest possible mention and the opposite of a
  disposition. Conversely a matcher written for the phrase with a space misses this track's own
  verdict commits. So:
  match either spelling, and then ask the sentence test — **does this sentence claim the commit was
  security-reviewed, or does it merely contain the words?** A citation-maintenance commit does not.

  ⚠ **Nor does a fix closing a finding — corrected 2026-09-01, reversing the
  half-sentence that stood here.** This bullet used to answer *"a fix closing a finding says so"*,
  and that is wrong in the direction this whole rule exists to prevent. A commit from another track
  citing the finding key it closes is **new code that no one has reviewed** — the citation says
  something true about a *different*, already-graded commit, and nothing at all about this one. The
  event such a commit triggers is a **verify-back**, and the verify-back is where its grading
  happens. Crediting it disposes of an ungraded commit on the strength of its own fix message.
  Measured the day the probe was scripted: eleven of them sat in one behind-base set, and the
  hand-application that had just tightened this rule credited ten.

  ⚠ **And the vocabulary is not a SUBJECT test — the third layer of the same bug, corrected
  2026-09-02.** The rule above asks what a
  sentence claims; a pattern match answers only whether the words appear, and a body can use every
  one of the charter's grading words while reporting *somebody else's* grading. Three commits were
  credited — and so excluded from the residual permanently — on sentences about an earlier review
  slice, about a different SHA's mechanism, and about a fix that *had already been* graded; none of
  the three had been read by anyone, and all three were in the class that is supposed to STAY in the
  residual. ⚠ The instructive part is what does **not** fix it: a rule keyed on sentence shape —
  *"reject the match if its sentence names another SHA, or negates the verb"* — is wrong in **both**
  directions at once. It misses the plainest case (*"Slice 1 graded CLEAN, but named one
  precondition"* names no SHA and negates nothing), and it demotes genuine verdict commits, because
  the charter's own self-crediting shape is a **listing** — *"- `<sha>` … graded CLEAN: the guard
  covers all six reads"* — in which naming the graded SHA on the line is precisely the claim. Prose
  cannot separate them. **The artifact can, and the charter already says which one:** *verdicts are
  dated records*, so a commit claiming to have graded something is credited
  on that vocabulary only when it also **wrote the verdict record** the claim implies. That is a fact
  about the commit rather than a reading of its prose, and it is the only form of the test that
  keeps every genuine verdict commit while rejecting every borrowed one. The self-referential
  openers stay exempt: they can only be said by this
  track about the commit saying them.

  ⚠ **And that exclusion is sound only while the verify-back it defers to EXISTS — which is why
  the roster is a second mechanical check.** A finding whose fix leaves the window must have its
  grading scheduled, and it counts as scheduled only on a positive mark, exactly as a SHA is
  disposed only by one: a bare prose mention schedules nothing.

  ⚠ **And a record naming a finding reads it only if it is FOR reading it — a fix is not a reader
  of its own fix.** A finding witnessed only by the fix that closes it is reported, not counted as
  scheduled, and every fix is filed with a finding id and its own scheduled verify-back.

  ⚠ **A close record names findings in two ROLES — the one it discharged and any it newly minted —
  and only one of them is a discharge.** A creation verb must never clear the finding it just
  created: credit an id only where an independent record corroborates it as the subject, and let
  an uncorroborated id fall through to a loud gap. An id quoted inside a fenced code block is an
  illustration, not a mint.


  **(2) A disposition roll-up is commentary, exactly like a ranking.** Several turns closed with an
  end-of-turn summary line — `**DISPOSED this turn:** <sha>, <sha>, …`. `^DISPOSED:` cannot match a
  line beginning `**`, so a SHA disposed *only* that way is invisible to the ledger and will be
  re-graded later, or — worse — will make a future session doubt the probe. Roll-ups are useful
  prose and may be written freely; **they do not count, and the canonical one-per-SHA lines must
  exist beside them.** Measured blast radius when this was found: 23 SHAs named across seven turns'
  roll-ups, 22 of which also carried canonical marks, one that did not.
- **Resolving whether a verify-back's target has landed is THREE questions, and a pointer to the
  target answers none of them on its own:** (a) is what the pointer now resolves to actually
  ABOUT this finding? (b) has this verify-back ALREADY been performed and recorded? (c) does the
  landing actually DISCHARGE the trigger — a closed target is not a discharged wait, and a trigger
  that fired may already have been discharged by someone else. (a) is a comparison, not a
  retrieval: having the target's title in hand is the state in which the error happens. Default
  to leaving the verify-back open — a mislabelled wait costs a reader a moment, while a landed but
  ungraded fix hidden behind a wait is never graded at all.
- **A hand-off is captured only when the receiving queue shows it** — a concurrent editor can drop
  an appended entry, so confirm it landed before trusting the record that says it was handed off.
- **An ordering constraint on someone else's work is written where that work's picker will read
  it, never only in the fix's own record.** A constraint the constrained side never reads binds
  nobody: write the tripwire where the machinery will read it, never as prose hoping a mind passes
  by.
- **A delta to a universal chokepoint can invalidate a distant test** — run the crate's other
  transport/protocol suites, not just the delta's own pins.
- **When two remedies compose, a probe pins the remedy that repairs its scenario, not the one it was
  authored against.** Attribute each pin by re-running the whole mutation matrix after every fix in
  the set lands — never by the probe's authorship — and record the corrected attribution beside the
  pins, or a later session reads the probe as guarding the wrong item and reverts the unguarded one.
- **The filed items enumerate the doors the finder saw; the mutation matrix over the landed fix
  enumerates the doors the code has.** Run the matrix to completion before closing a finding — a
  mutation that *survives* every filed pin is a missing door, not a passing grade (one such
  fourth mutation survived all four filed pins and exposed the half-rewrapped segment
  door, which then got its own pin). Prefer pinning the found door as a general invariant over
  whatever the artifact actually lists, with a vacuity guard, rather than a hand-picked id.
- **When a fix re-derives a value, ask whether the ceremony already holds the authoritative copy —
  and whether the source it chose is as complete as the property it must satisfy.** Name the source
  and the property separately, then check they have the same domain: a doc comment licensing a
  fallback *"while \<source\> holds X"* has keyed itself on the source rather than the property, and
  that phrasing is the tell. A destructive write authenticated to the thing it destroys can nearly
  always read it first — replace/overwrite ceremonies hide this because the authorization to write
  and the ability to read are one credential (a kit replacement rebuilt the escrow's
  predecessor section from the local registry while holding the prior key that opens the blob it
  was overwriting). Related: grade an *"empty is a legitimate answer"* justification against the
  case the mechanism exists for — such a justification travels between call sites unchanged and is
  routinely sound at one and false at the next.
- **A pin's prose states the property; only its fixtures state the coverage — read them against each
  other.** A pin asserting *"the resting blob's section survives"* whose body never builds a blob is
  proving something narrower than its own sentence, and the sentence is what the next session reads.
- **"This needs a human" is a load-bearing claim — attack it before believing it.** Name the exact
  observable that needs an eye; ask what else observes it (another process, a database row, an OS
  attribute, a log line, the wire); test the mechanism headlessly and leave a human only the last
  inch of *does it look right*. Be suspicious in proportion to convenience: if believing the claim
  means you get to stop working, attack it hardest.

## Implementation status today

The rules above are **method, not mechanism** — they are read by a reviewer and applied by
judgement, and no code enforces them. **Four exceptions, and they are the charter's mechanical
questions** — the rules here that have a right
answer not needing a mind:

1. **The disposition probe, ratified 2026-09-01** — *has this SHA been disposed of?* See § Scope
   and process's probe bullet. It also lists every mark written in a near-miss grammar — without
   the colon, or not at the start of its line — so a reader re-anchors it; such a line never
   counts as a mark.
2. **The roster check, ratified 2026-09-02** — *does every finding a verdict record mints have a
   scheduled verify-back, by a positive mark?* It also reports a finding witnessed only by its own
   fix, and every open fix whose mandate names code under the pathspec but carries no finding id.
   See the exclusion bullet it protects in § Scope and process.
3. **The fired-watch check, ratified 2026-09-03** — *has every deferred verify-back whose trigger
   has already fired become selectable again?* A deferred item stays invisible until someone
   notices its trigger fired, so the check reports every fired, partly fired and uncorroborated
   trigger — including triggers recorded as gates or refusals — generously, each with its reason.
   ⚠ **It detects, it does not decide:** § Scope and process's three questions still govern each
   retag, and a trigger written in prose no check can read is listed for a reader to walk.
4. **The ownership check, ratified 2026-09-15** — *is every commit the probe keeps in the
   residual still owned by an open verify-back?* A closed verify-back owns nothing further, so a
   member is owned only by a live record that positively points at it. The check reports, never
   disposes, and prints the owner beside every owned member.

The asymmetry between the four is deliberate and runs three different ways. For the probe, a
wrong **exclusion** is unrecoverable, so only a positive claim leaves the residual. For the
roster, a wrong **clear** is unrecoverable, so only a positive mark clears an id. For the
fired-watch and ownership checks, a wrong **silence** is unrecoverable, so every uncertain row
or member is surfaced with its reason rather than held back. All four refuse to guess, and all
four are red-verified against the real history that produced them.

Everything else on this page stays method. The other mechanical supports are the
finding/verify-back contract the review track carries in its working notes (finding identifiers,
named mutations, per-finding verify contracts) and the durable dated verdict records.

**Corpus coverage — read this before trusting a probe run's silence.** The `DISPOSED:` rule in
§ Scope and process is ratified and correct; its *practice* has drifted once, measurably, and the
drift is worth more than the rule. Marks were written for a run of verdict records after the
rule's ratification and then simply stopped, and across the span that followed the window's
exclusion mechanism silently reverted — first to a hex-token mention scan that still called
itself the `DISPOSED:` corpus and had the live work queue back inside it, then to plain base
advancement, which this doc forbids by name. So over a span where the marks were not being
written, a probe's *silence* is ambiguous: "no mark" does not mean "never read" there, though it
does wherever the marks were kept. A reviewer inheriting such a span must re-establish the
dispositions before treating the base as honest. **Re-establishing them has two stages, and only
the first is done.** Since 2026-09-19 every code-bearing commit from 2026-08-01 to the base is
*named* by at least one record. But **naming is not marking**. Over a thousand commits in the
2026-08-01 → 08-27 span are named only by rankings or enumerations and still carry no
`DISPOSED:` line, so they are still in the window. **The span before 2026-08-01 is ruled owed**
(2026-09-20; the ruling is § Scope and process's *no span of history* bullet). **Its tranche from
2026-06-23 to 2026-08-01 is drained in the marking sense (2026-10-08): RESIDUAL 0, all 2130
commits marked; the span before 2026-06-23 is owed and unfunded.**
Measured that day, default pathspec, `--until '2026-08-01 00:00:00'`:

| from (`--since … 00:00:00`) | set | arm 1 | arm 2 | RESIDUAL | never-named | fix-closing, all UNOWNED |
|---|---|---|---|---|---|---|
| 2026-02-27, the first commit | 5397 | 2 | 1 | **5394** | 4334 | 257 |
| 2026-06-23, the track's first record | 2130 | 2 | 1 | **2127** | 1143 | 246 |
| 2026-06-27, the first numbered turn record | 1963 | 2 | 1 | 1960 | 1012 | 225 |

That table is a **dated** measurement, and the drain has already moved it: the eleven commits it
counts as never having been in any window at all — the public CORS forwarder under `services/` —
were marked per-SHA later the same day. Re-measured over the
first row's span on 2026-09-20 after that landing: set 5397, **arm 1 13**, arm 2 1, RESIDUAL
5383, **never-named 4326**. Thirteen marks over 5397 commits is still nothing in the marking
sense. It drains in the ruled order, and only as far as
it is funded; until a tranche's RESIDUAL reads 0 no coverage claim reaches across it. So the base
is honest *from 2026-08-01 forward in the naming sense, and from 2026-06-23 to 2026-08-01 in the
marking sense*; nothing before 2026-06-23 is covered.

That episode is also the sharpest evidence available on the "method, not mechanism" stance
above, and it points at *which* half of a rule decays. Every reviewer in that span read the rule
— its read side is forty lines and impossible to miss — while its write side is one clause, and
not one of them performed it. What it settles is that **a rule whose output nobody produces will
read as satisfied for as long as its name keeps appearing**, because the only thing left to
inspect is the name.

**That live question is now answered, in one direction only** (2026-09-01, closing this doc's own
open item). The probe's *read* side became a
script — the decay episode above was the third of four measurements of
the same failure, and the fourth is what settled it: the turn that tightened the arm-2 rule and
landed it here **still over-credited ten commits when it applied its own new rule by hand**, and
those ten were, by this doc's own standard, excluded from a backlog forever. A rule re-derived in
a reader's head every time is not one rule. The probe's *write* side — a reviewer producing its own
`DISPOSED:` marks — stays method, deliberately: it is a judgement about what a turn actually read,
and no script can produce it. So the split is not "method versus mechanism" but **which half has a
right answer**: mechanise the half that does, and leave the half that does not to a mind.

Two things this doc deliberately does **not** own, because they change per development machine and
per track: build/test invocation gotchas (toolchain environment variables on a given machine,
feature-gated suites that report "0 tests" as a false green, crates that do not build natively)
and the list of code areas a concurrently-running track owns. Those stay in the reviewing track's
own working notes, which is where they can be kept current.

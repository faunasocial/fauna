# File-sync conflicts — target state

Owns: conflicts
Status: ratified — (the second host named throughout, the `fauna-sync` daemon, was removed 2026-10-02 — `../architecture/apps/sync-agent.md` § Headless deployment; "both hosts" below is the record of when it stood, and the shared engine is now the one host) — the auto-resolve model (2026-07-10) is BUILT end-to-end (slices 1–4; review list + policy fan-out complete on all 7 apps); delete-vs-edit (2026-07-29) and the concurrent-resolution & ancestor-freshness clauses (2026-08-02) are BUILT per their status notes. The former N≥3 lost-edit defect is RULED and FIXED by the causal watermark — clause 5 below, ruled 2026-08-02, which corrected clauses 2 and 4; BOTH hosts (the `fauna-sync` daemon and the shared engine) are built and e2e-verified as of 2026-08-03 per the status note. The schedule fuzzer's two clause-5 gaps (reissue destruction; order-dependent-fold stranding) are RULED CLOSED 2026-08-03 — the widened `is_resolution` meaning, the content rung, the edit-frontier + author-blind covering-resolution adoption, and the nest's honest winner-stamp upgrade, all BUILT in both hosts + the nest the same day (tier_1: both pins flipped, the fuzzer green with the reissue steps restored to its standing alphabet). The THIRD clause-5 gap (same-anchor divergence once a reissue enters the schedule; found 2026-08-04 by e2e leg 4a) is RULED CLOSED 2026-08-04 and BUILT in both hosts the same day: novelty is a property of BYTES, not of rows — the content licence on every edit-frontier advance, the ledger-widened proven-reissue stamp, the receiver-side class upgrade, the sharpened published-bytes conjunct, and the re-assert on a stale-declined own tail (tier_1: the pin FLIPPED to assert convergence at 2–4 seats under both delivery modes, the walk's shape axis widened; live: the armed leg's re-run now AGREES across seats — the divergence half is fixed end to end). The report-plane chain that followed (duplication → the loser-row ruling; the leg-4 live refutation → the leg-4 ruling; agreement-with-loss → the SAME-ANCHOR RULING, 2026-08-05: union merges, honest edit-class winners, daemon parity, per-report pending windows) is RULED and model-built per § Concurrent resolution's decision records — production build of the same-anchor conjuncts in flight on the held branch; leg 4a stays the acceptance gate until it runs green armed
Authority: the file-sync conflict model — the auto-resolve + version-retention target (text three-way merge when clean, else latest-wins, losers retained, review-list-not-chooser), the per-set conflict policy + format-driver dispositions, delete-vs-edit forced resolution, concurrent resolution & ancestor freshness, and the legacy candidate/choose-winner wire substrate; defers the review-surface UX to `../ui/folders.md` § Conflicts, version retention/restore mechanics to [`file-versions.md`](file-versions.md), and the sync protocol (change records, catch-up, merge-base storage, orchestrator) to [`file-sync.md`](file-sync.md)

Split verbatim out of `file-sync.md` § Conflicts on 2026-08-02 (a stub there redirects; prior
history: `git log --follow docs/goal/behavior/file-sync.md`).


A conflict arises when two devices produce changes to the same path with
diverging history — typically caused by concurrent offline edits. The nest
records the conflict together with its **candidate versions** (the diverging
manifests).

**Target model — automatic resolution with version retention (ratified
2026-07-10).** Conflicts never block on the user:

- **Every conflict auto-resolves immediately.** The losing version is always
  retained in version history ([`file-versions.md`](file-versions.md) — restore is re-point, nothing is
  destroyed), so auto-resolution is risk-free by construction: the no-data-loss
  invariant holds trivially.
- **Default action:** for **text-like** files, attempt a **three-way merge**
  against the cached merge base (the engine already tracks per-path bases) —
  merge only when hunks don't overlap, and never write conflict markers into a
  user's file; the merged result is a new version with both parents retained. On
  overlapping hunks, binary files, or no base: **latest-writer-wins**.
- **The per-set surface is a review list, not a blocking chooser:** the
  Folders page shows auto-resolved conflicts with a one-tap "use the other
  version" — which is exactly the [`file-versions.md`](file-versions.md) restore re-point. The user can
  review and override later; nothing waits on them.
- **Policy:** one per-set **conflict policy** (`Auto` — merge text, else
  latest-wins — the default | `Latest-wins-always`), with a global default on a
  sync-settings surface. The text-like vs binary classification is a **built-in
  Rust constant** (extension classes), never user-managed per-extension config —
  works-out-of-the-box wants correct defaults with zero configuration.
- **Format-aware merge drivers (disposition of record, 2026-07-20).** The
  extension-class constant is the sanctioned slot for future per-format merge
  drivers, each behind a hard **validation gate** (merge → verify the output
  round-trips/parses → any doubt falls back to latest-wins; retention makes the
  fallback lossless). Dispositions: **xlsx cell-level three-way merge** is a
  plausible demand-driven future driver (cells are addressable merge units;
  bounded, honest fallback on row/col-shift, formula-graph, or non-cell
  conflicts). **docx merge is decided NEVER**: OOXML run-renormalization +
  `rsid` churn on every Word save defeats base alignment, and cross-part
  package invariants make naive merges produce documents Word refuses to open —
  a corrupt head is strictly worse than latest-wins, and the only robust
  merger in existence (Word's own Combine) lives inside Word. Word-document
  collaboration belongs to app-level coordination or a future live co-editing
  surface, not this engine. Chunk-level merging is categorically out for every
  format: chunk boundaries are storage arithmetic, not semantic units — a
  spliced manifest is a file state no author ever had.

**Delete-vs-edit (ratified 2026-07-29).** A deletion is a change, so a remote
delete racing local content falls under the conflict definition above — but its
resolution is **forced, not policy**: local content the nest lacks always wins
over a tombstone, because the bytes exist nowhere else and applying the delete
would destroy their only copy (`principles.md` § No user-data loss).
`Latest-wins-always` does not extend to deletions — a tombstone can never "win"
over content that was never uploaded. The ratified product semantic:

- **A declined delete surfaces as a resolved conflict row on the review list**
  (`conflict_type = delete_declined`, `resolution = latest_wins`, winner = the
  surviving content, uploaded retention-first before the report — the same
  order the concurrent-edit auto-resolve uses). Silence here reads as a sync
  bug: the survivor's re-upload makes the file "reappear" on the deleting
  device with no explanation.
- **The losing version is the deletion itself, and it is already retained:**
  the tombstone is a recorded change, and every recorded change IS a version
  ([`file-versions.md`](file-versions.md)). No separate retention step exists or is needed.
- **The row is informational-only.** It carries one candidate (the survivor),
  so there is no non-winning candidate to re-point to and the one-tap "use the
  other version" does not apply (the shared `use_other_version` declines the
  gesture gracefully). A user who still wants the file gone deletes it again —
  the same action, available everywhere. A dedicated "apply the delete anyway"
  gesture is deliberately NOT built (it would race edits made since the
  decline); revisit only on demand. Its badge is
  `devices.conflicts.delete_declined` ("Delete declined") via the shared
  `conflict_badge_label`, kept through any resolution stamp — "Latest kept"
  is technically true but hides exactly the half the user needs.
- **Propagation rides the resolved report:** the nest's propagate step writes
  the winner as the new head row, which is what re-materializes the file on
  the deleting device — explained by the review row instead of resurrecting
  as an anonymous reconcile fresh-create.
- **Scope — only a lost conflict reports.** The row is recorded exactly when
  the engine keeps a **tracked** row's bytes the nest lacks: the in-flight
  local states (`Uploading`/`LocallyModified`/`Conflicted`), a settled row
  whose disk diverged from the recorded local identity (the mid-debounce
  edit), or a materialized row with no local identity (nothing proves it
  unedited). Declined **silently**, not conflicts: an untracked file (no
  shared lineage — every fresh bind replaying history meets this), a
  recreation after its tombstone already applied (`Deleted` row — the delete
  was honored), a headless lineage (never reached the nest), the mid-apply
  remote states (the local bytes are the recoverable synced version), and
  infra-failure fail-closed keeps.
- **Best-effort at every rung, bytes kept regardless:** an upload or report
  failure degrades to the local conflict record + a legacy unresolved report
  (the same ladder the concurrent-edit auto-resolve uses), and reconcile's
  fresh-create upload remains the propagation.

**Concurrent resolution & ancestor freshness (ratified 2026-08-02; the causal
watermark — clause 5 — ruled later the same day, correcting clauses 2 and 4).**
Auto-resolution is decentralized: any device that finds itself diverged from
the head resolves, so two devices resolving the *same* divergence — each from
its own cached ancestor — is expected operation, not a race to prevent. The
contract has five clauses:

- **A resolution is an ordinary change, not a final verdict.** Each resolver
  lands its winner as the new head row via the resolved report; a later,
  better-ancestored resolution may supersede it as head. Every intermediate
  winner remains a retained version ([`file-versions.md`](file-versions.md)), so supersession is
  lossless by construction. A review row whose recorded winner is no longer
  head is a true record of that device's resolution, not an inconsistency;
  `use_other_version` still re-points among retained candidates.
- **Convergence — not winner-permanence — is the invariant.** A device whose
  own divergence is resolved has `base == local`, so a later head that
  **causally dominates its frontier** (clause 5) fast-forwards onto it; each
  resolution round shrinks the set of diverged devices until all settle on one
  head. ⚠ `base == local` ALONE is not a fast-forward licence — it proves only
  "no unpublished local work", never "the incoming head descends from my
  state": a concurrent SIBLING head satisfies it while containing none of this
  device's work, and applying it verbatim destroys that work with a log line
  claiming a clean merge. This is exactly the measured N≥3 leg-4 loss and the
  N=2 two-batch loss; clause 5's watermark is what closes it.
- **Equal ancestors make the first winner durable.** The resolver is
  deterministic (the shared `resolve_conflict`) and manifests are
  content-addressed, so two devices resolving the same divergence from the
  same ancestor compute the *same* winning manifest — their reports collapse
  to one head, and the first reporter's recorded winner IS the final head.
  This is the designed common case, and it is what the next clause protects.
- **Ancestor freshness is a device obligation: a device's cached merge base
  must never lag its own last acknowledged publication of the path.** A device
  that merges from an ancestor older than content it *itself published* uses a
  base it provably knows is superseded — still safe (a genuine-but-stale
  ancestor can duplicate or misplace lines, never silently drop a side, and
  retention keeps every parent) but needlessly low-quality, and it forfeits
  the equal-ancestor determinism above (measured: the authoring seat's stale
  base turned a clean `hello/cat/there/dog` merge into a duplicated-line
  head that its peer then had to supersede). Mechanism: a device advances its
  base on its own **echo** — the nest-attributed copy of its own change coming
  back on the pull rail — iff the local file still matches the published bytes
  (`SyncEngine::apply_self_echo`; peer changes in the same batch are processed
  *before* the batch's own echoes, so a genuinely concurrent peer edit still
  merges against the pre-publication ancestor rather than fast-forwarding over
  local work). First publication (`SyncWsClient::record_merge_base_if_absent`)
  remains the bootstrap for the window before the first echo. Genuinely
  unequal ancestors stay legitimate: a device offline since an older common
  version resolves from the freshest ancestor it can causally justify, and its
  resolution may transiently head until superseded per clause 1. (Correction,
  same day: the parenthetical above once claimed a genuine-but-stale ancestor
  "can duplicate or misplace lines, never silently drop a side" — TRUE of one
  merge, FALSE of a sequence: the model showed re-merging already-incorporated
  content from a stale ancestor produces overlapping-identical hunks that read
  as unmergeable and fall to latest-wins, which does drop a side. Clause 5's
  idempotence guard is what forbids that re-merge.) The same ruling retired
  this clause's original closing claim that `SyncChange` rows "deliberately
  carry no parent pointer" leaving the cached base as a device's only
  ancestry: causality is now ON the wire, and the base slot is the fallback
  rung, not the whole obligation.
- **The causal watermark (ruled 2026-08-02) — causality is wire state,
  and the licence reads it.** Every recorded change MAY carry two additive
  fields (`SyncChange.derived_through` / `.is_resolution`, mirrored on the
  record requests, the data-plane `FileChanged`, the real-time `ApplyChange`
  forward, the federation record relay, and the resolved report's
  `winning_derived_through`/`losing_derived_through`):
  `derived_through` = the set seq through which the WRITER had incorporated
  every row when the content was produced — **a lower bound by law: stamp the
  persisted catch-up anchor, never more; over-claiming recreates the lost-edit
  defect, under-claiming costs one extra idempotent merge round.** The law
  is the writer's duty; the receiver's enforcement is the bound
  `min(w, seq − 1)` — no receiver reads a claim at or above the row's own
  nest-assigned seq (the watermark-bound decision record, 2026-09-21, below).
  `is_resolution` = true for a row that **carries no novel content beyond
  `derived_through`** (meaning widened by the 2026-08-03 gap ruling below): a
  pure resolution (auto-resolve merge result / propagated winner — the nest
  stamps propagated winners by construction) and a **proven reissue** — a
  re-upload of bytes whose change record provably landed: the re-seal
  migration holding its `recorded_content_hash` proof, **or bytes the
  writer's own causal ledger holds at seq ≤ its frontier (the gap-3
  widening — the reconcile-after-restart re-send and the consumed-echo retry
  are provable at send time; a genuine revert to held bytes thereby also
  stamps no-novel-content, so a stale-raced revert is skipped and re-reverted
  once caught up — the content rung's recorded trade, moved to the stamp)**.
  A re-upload WITHOUT either proof (the lost-ack retry) keeps the edit stamp:
  its record may never have landed, making its bytes the only carrier of a
  real edit. Each device
  keeps, per path: a **frontier**
  (the newest seq its local content reflects, authored or applied — advanced
  by the self-echo, a verbatim apply, a merge, and an equal-content arrival;
  the echo advances frontier and base in one event so the licence's conjuncts
  cannot desynchronize), a **content frontier** (the newest seq whose
  content local reflects — the frontier minus retention rows: advanced
  wherever the frontier is, except by the accounting of a retention row;
  untracked reads as equal to the frontier — *Retention rows are
  transparent to the licence*, below), an **edit-frontier** (the newest seq carrying NOVEL
  content local reflects — advanced only by non-resolution rows applied,
  merged, or authored **whose bytes are not already held at a lower seq**
  (the gap-3 content licence: **novelty is a property of BYTES, not of
  rows** — a reissue's novelty was counted at its earliest carrier, so it
  advances the full frontier only, whoever authored it and whatever stamp it
  wears; this licence binds EVERY advance site uniformly — the receiver
  arms, the fold arm, the author's own echo sites, and the engine's
  record-ack), by such edits folded under a licensed batch carrier, and
  never by resolutions or skipped reissues; **upper-bound law, the
  watermark's mirror: never UNDER-state it** — an under-count licences
  adopting a resolution that misses content the device holds, a regression,
  while an over-count only strands a covering resolution, and the gap-3
  re-assert rule (below) bounds that strand to one extra log row where it
  used to be permanent divergence on the same-anchor shape; untracked reads
  as equal to the frontier) and a small **ledger**
  of held row versions (capped; bytes from the content-addressed cache). The
  receiver rules, in order, for an incoming row at seq `s` with watermark
  `w`:
  1. `s ≤ frontier` → **skip** (idempotence guard — a duplicate delivery is
     never re-applied and never re-merged);
  2. `is_resolution` and `w < edit-frontier` → **skip, account** (a stale
     resolution misses novel content this device reflects; the frontier
     advances, the edit-frontier does not);
  3. `is_resolution` and `w ≥ edit-frontier` — a **covering** resolution
     (every novel edit incorporated; anything it misses between the two
     frontiers is other resolutions, i.e. order-choices): `local == base` →
     **adopt verbatim** — the nest log's total order arbitrates which
     order-choice wins, per clause 1's "a later resolution may supersede",
     and adoption is **author-blind**: a device's OWN covering resolution
     whose echo lands after local moved to an earlier-seq resolution
     re-adopts by the same licence (else the publisher of the log-tail
     permutation is the one device that never converges to it). `local !=
     base` → merge per rule 6;
  4. an edit with `local == base` AND `w ≥ content frontier` → **fast-forward**
     (the writer provably incorporated everything this device reflects — the
     content frontier, not the full frontier, since a retention row is
     reflected by no one: *Retention rows are transparent to the licence*,
     below; the edit-frontier conjunct beside it is unchanged);
  5. `w < frontier` and the row's exact content hash is held in the ledger at
     seq ≤ frontier → **skip, account** (the content-keyed idempotence rung:
     a reissue — lost-ack retry whose record landed, a pre-ruling writer's
     re-seal — must never re-merge from a stale ancestor, which reads as
     overlapping-identical hunks and latest-wins over a peer's edit; the
     frontier advances, the edit-frontier does not, even when the row wears
     an edit stamp). This rung sits AFTER the adopt/fast-forward licences —
     it guards the merge arm only, because a ledger hold proves bytes were
     SEEN, not incorporated (rule 2 holds skipped rows too), and skipping an
     adoptable covering resolution re-strands the fold. Recorded trade: a
     STALE-watermarked genuine revert to held bytes is skipped and must be
     redone once caught up (a caught-up reverter has `w ≥ frontier` and never
     hits the rung) — the alternative destroyed a peer's newer concurrent
     edit via latest-wins;
  6. otherwise **diverged** → three-way merge whose ancestor is the newest
     held version with seq ≤ `w` (a version the writer provably had — hence
     genuinely common); ladder on absence: the live base slot, then no-base
     latest-wins. An absent watermark (old writer, old-nest relay) degrades
     every rule to the pre-ruling behaviour — additive-everywhere, never worse
     than before; an absent edit-frontier (state written before the gap
     ruling) degrades rules 2–3 to the pre-gap behaviour the same way.
  **Three gap-3 refinements (ruled 2026-08-04) bind rules 2–6 uniformly.**
  **(i) The class UPGRADE:** a row whose exact bytes the receiver already
  holds at seq ≤ its frontier is PROVEN novel-content-free — the
  `is_resolution` definition — so it is judged by the resolution licences
  whatever stamp it wears (the lost-ack retry's writer cannot stamp it; the
  receiver holding the bytes has the proof the writer lacked). Without the
  upgrade a reissue's adoption was ORDER-dependent — fast-forwarded where it
  arrived ahead of the receiver's frontier, rung-skipped behind it — and on
  the same-anchor shape that order-dependence IS divergence. **(ii)
  `local == base` means NO UNPUBLISHED WORK, and the proxy is sharpened:**
  local bytes equal to the base OR to bytes this device has published (the
  daemon's last-sent manifest, the engine's `recorded_content_hash`) are
  recoverable nest-side, so the verbatim licences treat them as published —
  the base advances only on the echo, and a covering resolution judged in
  the publish→echo window used to fall to a pre-publication-ancestor merge
  that DUPLICATED the published edit (the live leg-4a signature). **(iii)
  RE-ASSERT on a stale-declined own tail:** when a device's OWN
  novel-content-free row echoes back, would re-adopt by log order (later
  than the row local came from, no unpublished work) and is refused ONLY by
  rule 2's staleness test, the device republishes its CURRENT bytes as a
  resolution at its current anchor — that comparison is genuinely
  undecidable (the edit-frontier honestly counts an unprovable reissue's
  seq; the tail's stamp honestly under-claims its own unechoed novelty;
  neither side can be fixed), and clause 1 applied reflexively converts it
  into a fresh, decidable log-tail row the whole fleet converges onto.
  The batch-latest fold is **causal and all-or-nothing**: same-path rows fold
  only under the path's CARRIER — **the latest *superseding* row (every
  delete, every peer create/modify), not the raw batch-latest row** (ruled
  2026-08-04, measured live): a fresh bind's reconcile
  uploads the local file *before* the first pull, so the first batch's tail
  is the device's own just-recorded echo, and a raw-tail carrier rule
  refused the fold outright while a fully-covering peer delete sat right
  below — the stale create then downloaded against the user's newer file.
  Any row above the superseding max is by construction a self create/modify
  echo (anything else would itself be the superseding max), writes nothing,
  and neither carries nor blocks the fold; the fold covers exactly the rows
  BELOW the carrier. The carrier must be a PEER row that provably supersedes
  every same-path row below it — by `w` covering the row's seq, **or, under
  a DELETE carrier only, by same-author supersession** (same ruling): a
  device's later row on a path causally descends from its own earlier rows
  there by single-writer linearity, a fact the receiver proves from the batch
  rows alone. No watermark can substitute for that rung — the stamp is a
  set-wide lower bound, so a device that records create → delete before its
  own echoes return honestly stamps an anchor below its own create (bumping
  the stamp to own-authored seqs would over-claim unseen peer rows below
  them), and refusing the fold there let the stale create download against
  a fresh bind's newer local file and manufacture a spurious conflict
  review row for a file nobody edited concurrently — the 2026-07-24 live
  shape, in the shape it presented then
  (pinned tier_1 by
  `a_delete_supersedes_its_own_authors_earlier_create_in_one_batch` and
  `an_own_echo_above_the_carrier_does_not_disable_the_fold`). **That live
  shape's tier_3 observable has MOVED — from conflict NOISE to PROGRESS
  (re-measured 2026-09-20 on linux/tui).** A fresh bind records its own upload before the first pull (each
  caller's startup path converges the local half above
  `always_resident::run_watch_loop`'s eager pull), so the stale create
  meets an UNTRACKED path frontier plus the recorded-bytes witness (gap-3)
  and judges FAST-FORWARD — the verdict the leg-4 DEFER cap refuses while
  the live base still holds nothing. So with the fold neutered the stale
  create is no longer downloaded-and-fought-off by auto-resolve (the
  2026-07-29 reading — the cap now intercepts one rung above the merge
  arm, which is why no conflict row is recorded any more); it is DEFERRED,
  and since the cap holds the anchor BELOW it, every row above it stops
  applying on that device — the set's remaining history included.
  `test_filesync_bind_history.py` is still the fold's tier_3 pin, now
  through its replay barrier rather than its conflict count; here the fold
  is not defense-in-depth but the FIRST defense, because the rung below it
  refuses this row rather than resolving it.
  **Delete carriers only — the content-carrier variant of the rung is
  REFUTED** (schedule-fuzzer counterexample, 2026-08-04): a delete is never
  judged by the receiver rules and carries the whole folded information
  ("the path ends deleted"), while folding under a content carrier starves
  the ledger — the folded rows' skip-holds are what let the content rung
  (rule 5) absorb a later stale-watermarked lost-ack reissue, and without
  them the reissue reaches the merge arm and latest-wins destroys a peer's
  edit. Per-row folding stays refuted — an intermediate fast-forward can
  strand the designated carrier as a stale resolution, and the folded rows'
  content is never delivered. And the fold fires **never under a carrier
  that would itself be stale-skipped** (`w` below the receiver's effective
  edit-frontier): a skipped carrier consumes the folded rows' content
  undelivered — a lost edit, not a strand (fuzzer-measured 2026-08-03; a
  carrier below the full frontier needs no such conjunct — the folded seqs
  are then ≤ `w` < frontier, already reflected; a delete carrier is outside
  this conjunct's territory — it is never judged, so it cannot be
  stale-skipped). Shared decision core:
  `fauna_sync_engine::causal` (`CausalStamp`, `judge_incoming`); executable
  model: `merge_convergence_test.rs`'s causal section — union + convergence
  at 2–4 seats, batched and per-row, with and without duplicate-redelivery
  churn, plus the two pre-ruling loss records kept as refutations.

**The anchor stopped being the lower bound the day the apply path grew a permanent skip (2026-09-20).** *Stamp the persisted catch-up anchor, never more* was exact while the anchor meant *"every row at or below me was applied"*. It no longer does: [`file-sync.md`](file-sync.md) § *A failed change must not strand the device* deliberately lets the anchor move past a change that can never apply here, precisely so one bad row cannot strand the device — and the anchor's own accounting law says *applied **or deliberately skipped***. So an edit stamped at the raw anchor claims rows this device skipped, which is the lost-edit defect the law exists to prevent: a peer whose file still matches its base reads the claim, judges the edit fast-forward, and replaces its own unread work with no conflict row. An **edit** stamp is therefore `min(anchor, the earliest permanently-skipped seq − 1)`, and every edit-stamp site on both hosts goes through the one reduction (`CausalStore::honest_anchor`) rather than reading the anchor itself.

**The reduction is per path and self-releasing (same ruling).** A skip on path P says nothing about path Q, and `derived_through` is consumed per path — [`judge_incoming`]'s `w >= f` compares it against the *receiver's frontier for the row's own path* — so only P's claims are reduced. A set-wide ceiling would be honest too, and far worse: one bad row would tax every edit on the device for ever, and the "one extra idempotent merge round" the law prices in would become a standing retained-loser conflict on every binary edit. The release needs no clearing pass: once P's own frontier reaches the skipped seq, this device's content for P reflects a row at or past the skip in nest-log order, so a peer fast-forwarded to these bytes loses nothing it should have kept — the comparison IS the release. **So "the earliest permanently-skipped seq" means the earliest LIVE one (2026-09-28):** P's floor holds every skip recorded above P's frontier, the reduction takes the lowest of them, and a skip the frontier has reached is released and pruned at the next write. A floor holding one seq kept the lowest skip and dropped any later one as already covered, so once the frontier released the lowest, a skip recorded after it — or one still live above a release that passed only a lower skip — was forgotten, and every claim on P crossed it (reachable wherever a path floored at the resolved-path mint site keeps receiving readable rows: a mixed-version fleet's unreadable row twice on one file). Pinned in `anchor_accounting_test.rs` (a skip recorded after a release, and a release passing only the lower of two live skips, on both `honest_anchor` and `honest_winner_claim`) and mirrored by the daemon's own multi-skip cell. The one row shape with no usable per-path key, a `path_hash` that is not 32 hex bytes, takes an absolute set-wide floor; no correct nest serves one, and nothing about such a record can be trusted to name a path. That judgement belongs to the floor's one sink (`CausalStore::note_permanent_skip`), never to a caller: the floor's key is the only causal-store key taken from the wire rather than derived locally, so it names a file only after it decodes to exactly 32 bytes and is re-encoded — a nest-served string never reaches a filename, so it can never name one outside the store — and the shared opener judges the hash before anything else about the row, so the `Salt` refusal is always the verdict for the shape that takes the set-wide floor.

**A resolution IS reduced — the lower-bound law binds `derived_through` on every stamp, and the resolution bit changes nothing about what it may claim (ruled 2026-09-20; supersedes the same-day "edits only" narrowing).** That narrowing feared that under-claiming a resolution gets a **merge result** stale-skipped by rule 2 rather than re-merged — but no raw-anchor resolution site carries a merge result. The auto-resolve winner travels as the propagated winner row whose claim is `honest_w`: the incoming seq only when it is contiguous with (or own-gapped from) the path's pre-merge frontier, else the frontier itself (the leg-4 ruling) — and a live **per-path** skip at S on P sits **above** P's frontier by construction, since that floor's release is exactly `frontier ≥ S`; a frontier-bounded claim never crosses it, so *that* branch of the winner claim needs no per-path reduction. ⚠ The argument reaches two of the claim's three branches and neither of its two floors — see the next paragraph, which retires the exemption. The sites that DID read the raw anchor — the gap-1 proven reissue, the re-seal migration's re-upload, the gap-3 ledger/base-witness reissue, and the (iii) re-assert — carry bytes this device already holds or has published, so the `is_resolution` half of the stamp (*no novel content beyond `w`*) is honest whatever `w` says; it is the `derived_through` half, which rule 3 reads as *the writer incorporated every row ≤ w*, that lies once `w ≥ S`. The per-reader permanent classes (a manifest past `check_min_reader`, a seal under key material this device lacks) are exactly rows OTHER readers apply, so a peer whose edit-frontier reached S holds novel content there, judges the over-claimed row covering and, with no unpublished work, adopts old bytes **verbatim** — S's content replaced with no conflict row, the lost-edit defect wearing a resolution stamp. The gap-3 class upgrade is no defence: it judges the row's class from held bytes and never its claim, and the adopt licence runs ahead of the content rung. With the honest `min(anchor, S − 1)` that peer stale-skips a row that is information-free to it (its own content descends from S; this device's bytes do not) — rule 2 doing its job — while every receiver whose edit-frontier is below S still reads the stamp as covering (gap-2: the rows between the two frontiers are order-choices) and adopts or merges it: no legitimate covering resolution is stranded. A device that permanently cannot read S therefore cannot converge P with a peer that did until a later readable row on P releases the floor — the truth of its state, not a defect of the stamp — and its stale-anchored revert to held bytes on P loses to that peer's newer content instead of destroying it, the content rung's recorded trade unchanged. Mechanism: the one per-path, self-releasing reduction above, `CausalStore::honest_anchor`, consumed by every edit AND resolution stamp site on both hosts (`SyncEngine::{edit_stamp, resolution_stamp}`; the daemon's `SyncWsClient::{edit_stamp, resolution_stamp}`), pinned in `anchor_accounting_test.rs` (the shared floor on both stamps; the over-claim's rule-3 regression as the refutation; the reduced stamp still covering below the skip; the frontier-bounded winner claim untouched by a live per-path skip) and mirrored by the daemon's own stamp-floor cell.

**The winner claim IS reduced by BOTH floors, the set-wide AND the per-path — the exemption above was argued for one floor and taken for both, and failed inside the per-path floor too (ruled 2026-09-22).** The per-path floor releases, and that release is the whole of what makes a frontier-bounded claim safe: a live skip at S on P holds P's frontier below S, so `honest_w` — bounded by that frontier — cannot reach it. The set-wide floor has neither property. It is minted for the one row shape with no usable per-path key, a `path_hash` that is not 32 hex bytes, it has no release at all, and a skip attributable to no path holds NO path's frontier down — so P's frontier climbs past S freely, `honest_w` follows it, and the propagated winner row crosses a row this device never read. That is the same lost-edit defect in the same words: a peer whose edit-frontier reached S judges the over-claimed winner **covering** and, with no unpublished work, adopts old bytes **verbatim** over S's content with no conflict row (the gap-3 class upgrade is no defence — it judges class from held bytes, never from the claim). The device also disagreed with itself about one skipped row: with a set-wide floor at 100 and P's frontier at 150, an edit or resolution stamp claimed 99 while the winner claimed 150 — honest or dishonest depending only on which stamp was being minted. Mechanism: one derivation, `CausalStore::honest_winner_claim(path, incoming_seq, gap_all_own)`, carrying the leg-4 frontier bound AND both skip-floor reductions (per-path, then set-wide), consumed by both hosts' winner arms (`SyncEngine::resolve_and_report`; the daemon's `SyncWsClient::resolve_divergence` through its own delegating twin) exactly as the stamp sites already share `honest_anchor`. ⚠ **And the same argument fails a second time inside the per-path floor**, which is why the exemption is retired rather than narrowed: the own-gap-widened branch (`claim_gap_all_own`, the same-anchor ruling's conjunct 3) is not frontier-bounded at all — it claims the incoming seq outright. A row skipped under P's own `path_hash` can be invisible to the caller's gap proof: at the seal-refusal mint sites the row's path never resolved, so it matches no same-path filter (the resolved-path mint site — a change that can never apply on this device — lacks that property, and soundness does not rest on it, since the reduction is unconditional); and both hosts mint per-path floors earlier in the same batch they later claim in, so the floor is still live when the claim is made. `honest_winner_claim` therefore applies BOTH reductions, in the same order `honest_anchor` does. That costs the sound branches nothing, which is why it is unconditional rather than branched: a live floor sits above the frontier (`floor > f`) and is a different row from the incoming one (`floor ≠ s`), so the contiguous branch has `floor ≥ f + 2 ⇒ floor − 1 ≥ s` and the frontier-fallback branch has `floor − 1 ≥ f` — both untouched. It bites only where the claim outran the frontier, and it releases exactly as the anchor's does once P's frontier reaches the skip. Pinned in `anchor_accounting_test.rs`: the set-wide skip cutting a contiguous, a frontier-fallback and an own-gap-widened winner claim alike, with the per-path control asserting the exemption's sound arm through `honest_winner_claim` itself rather than `honest_anchor` standing in for it — a proxy cannot tell the two arms apart, which is how the gap survived its own pin — and mirrored by the daemon's own winner-claim cell.

**A floor that cannot be READ is not the absence of a skip (same ruling).** `CausalStore::read_floor` folded every read error and every parse failure into `None` — no floor — silently restoring the raw over-claiming anchor for every stamp and every winner claim on that key. The asymmetry was the tell: the floor WRITE had always documented its fail-open toward over-claiming and logged it loudly, while the read did neither. A floor file exists only because a skip was recorded under that key, so unreadable means *a skip happened here and its seq is lost*, and each floor now answers with the strongest bound it can prove. An unreadable **per-path** floor claims no further than the path's own frontier: whatever the lost seq was, that floor had either released (`frontier ≥ floor`, claim unreduced) or was live (`floor > frontier`, so `floor − 1 ≥ frontier`), and the frontier is honest under both — and it climbs as the path does, so this degrades rather than strands. An unreadable **set-wide** floor has no release to reason from and so admits no bound above 0: it pins every claim on the device until the file reads again — the honest answer to a record nothing about which can be trusted, and pathological in exactly the way the row shape that mints it is. Both log at the write side's volume. Recording a new skip over an unreadable floor follows the same split: a **per-path** floor is never replaced by a readable value that forgets the loss — its degradation is no strand, so the file keeps a *lost* marker beside the new seq and keeps reading as unreadable (every seq it held is dominated by the frontier bound anyway); the **set-wide** floor, whose unreadable state does strand the device, is replaced by the new seq, logged loudly because an earlier lost seq makes that an over-claim. The floor write is temp-plus-rename for the same reason: `std::fs::write` truncates before it writes, so a process death in between left a zero-length file, which is now read as *unreadable* rather than *absent* — and our own crashes must not be able to reach a state that pins the device. The frontier writes above need no such care: a lost frontier reads as 0, which only ever over-merges.

**Retention rows are transparent to the licence — and the nest's winner-stamp upgrade is RETIRED (ruled 2026-09-27).** The 2026-08-03 gap-2 ruling had the nest raise the propagated winner's `derived_through` from the reporter's claim to the just-minted loser row's seq when no same-path row intervened, because the loser row — then an edit-class row — bumped every receiver's edit-frontier past the winner's claim and rule 3 never fired on the report path. Two later rulings changed what that upgrade rests on. The loser-row ruling (2026-08-05) made the loser row a retention row that advances the frontier only, never the edit-frontier, so for a **resolution-class** winner the upgrade has bought nothing since: rule 3 reads the edit-frontier. And writer-signed change records ([`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md) § Writer-signed change records, ruling (1)(ii)) sign `derived_through`, and a reporter cannot sign a seq the nest assigns after it signs — an upgraded winner row can never verify. What the upgrade still bought was rule 4 for an **edit-class** winner (one carrying the reporter's unpublished novelty — `winning_carries_novelty`, the common conflict): the retention row advanced the receiver's full frontier to its own seq, the unupgraded claim sat one below it, `w ≥ frontier` failed, and a caught-up receiver with no unpublished work took a merge round — and filed a report of its own — for a row it could have applied verbatim. That is a defect in the licence, not in the claim: the frontier conjunct exists so an edit never overwrites content this device reflects, and a retention row is reflected by NO receiver — the retention rung never fetches, applies, merges or ledgers it. So the licence reads the frontier through the rows that carry content: each device keeps, beside the frontier and the edit-frontier, a per-path **content frontier** — the newest seq whose content its local reflects, advanced at every site that advances the frontier EXCEPT the accounting of a retention row (the shared `RetentionRow` rung's callers and both hosts' self-echo retention arms, through one funnel) — and rule 4's conjunct becomes `w ≥ content frontier`; the edit-frontier conjunct beside it, rule 1's duplicate guard, rule 5's `w < frontier` and `honest_winner_claim`'s contiguity keep the full frontier, so a retention row still counts as seen and as contiguous. Untracked reads as equal to the frontier — today's behaviour, the edit-frontier's migration story. The rule is the nest's upgrade generalised and moved to the one party that can judge it: it covers any retention row on the path, not only the report's own adjacent one, needs no listing look-behind, and holds against a nest that plants retention rows — they lower no bar below the content the receiver holds, and the nest could withhold the same rows outright. With it the nest mints `winning_derived_through` exactly as sent — the report transaction (`sync_storage.rs::report_conflict`) and the tier_1 model's nest mirror (`merge_convergence_test.rs`, `CPublish::Report`) both drop the upgrade — and the reporter's signed statement is the minted row. Rejected shapes are the owner's (mls-group-key-material.md, the clause above). Cost of the interim, should the nest's retirement land before the receiver rule: one extra idempotent merge round per caught-up receiver of an edit-class winner — the under-claim price the lower-bound law already names, never a loss.

**The union is the positional merge's FALLBACK, never its replacement
(narrowed 2026-09-02).** The same-anchor
ruling's conjunct 1 (line-multiset unions, below) was built as a *pre-check*
running ahead of the positional three-way merge, so it also answered every case
the positional merge already handled cleanly — and because the union keeps
ours' order and appends theirs' uncovered lines at the END, it did so by
relocating the peer's inserted line to the bottom of the file. That contradicts
two ratified claims at once: the 2026-07-10 **user ruling** that non-overlapping
text hunks three-way merge from the cached ancestor (§ Target model), and
clause 3 above — the union is order-dependent, so two seats resolving one
divergence from the *same* ancestor computed *different* winning manifests
(base `hello/there`, one seat appending `dog` while the other inserts `cat`,
yields `hello/cat/there/dog` from the inserting seat's vantage and
`hello/there/dog/cat` from the appending seat's). Neither was intended: the
ruling's own rationale is that a positional diff of two **permuted supersets**
misreads shared lines as conflicting replacements and falls to latest-wins,
destroying novelty — a defect about LOSS, not about placement. So the order is
now: containment short-circuits first (a subset side adds nothing), then an
**affordability gate** — above a fixed line-count ceiling on any of base,
ours or theirs (`fauna-core::format_text::POSITIONAL_MERGE_LINE_CEILING`), the
positional attempt is not made at all, because the same low-cardinality shape
that defeats containment also degrades the positional diff's Patience pass to
its quadratic Myers fallback — then the positional
merge, which stands **iff it is both clean and lossless** — lossless
meaning it already carries the union's own guarantee of `max(ours, theirs)`
copies of every line, nothing dropped, nothing duplicated, nothing invented
(`fauna-core::format_text::is_lossless_union`) — and otherwise the union,
unchanged, for exactly the permuted-superset case it was ruled for. Where the
positional merge stands, clause 3's determinism is restored for free. That
narrowing moved conjunct 1's *placement*, not its content: no wire change, no
policy change, and no case that previously merged losslessly fell to
latest-wins **as a result of it** — the widening below is where a new
latest-wins case is deliberately introduced, and it is introduced on cost
grounds, not placement ones.

**The affordability gate governs the positional CALL, not one arm (widened
2026-09-02).** As first built, the ceiling
was consulted only inside the both-extending branch, so it bounded exactly the
shape a peer never has to send. `both_extend_base` means *both heads are pure
appends*; one deleted or modified line on either side drops an occurrence of a
base line, containment fails, and the divergence took the **general** path,
where the same quadratic `diff3_merge` ran with nothing consulting the ceiling
— and the incoming side is content a peer with write access uploaded, so the
peer alone decides which path a divergence takes. An ordinary edit is already
that shape. The bound is therefore stated on the **call**: a single gate sits
immediately before `diff3_merge`, which has exactly one call site, so no input
reaches the quadratic pass ungated. Above the ceiling it dispatches on the arm:

- **Both heads extend the base** → the line-multiset union, exactly as ratified
  above. Unchanged: the union is that arm's lossless answer at any size.
- **Anything else** → the adapter **declines**: `TextAdapter::merge` returns
  `Err`, and `resolve_conflict` folds a declining adapter into
  **latest-writer-wins**, whose loser is retained (§ Target model). The union
  is *not* available here and must not be reached for: with a deletion on
  either side it would resurrect a line its author deleted — a file state no
  author ever had, which is the same objection that rules out chunk-level
  merging above. Declining is the honest answer, and it is the one the target
  model already gives for overlapping hunks, binary files and a missing base.

So yes: an oversized general-path divergence that used to merge now resolves
latest-wins. That is the deliberate trade, and it is the format-driver
**validation gate** discipline of § Target model (*"any doubt falls back to
latest-wins; retention makes the fallback lossless"*) applied to **cost** as
well as to parse validity — nothing is destroyed, because the loser is a
retained version and the user's one-tap "use the other version" still re-points
to it. The constant is named for what it now bounds
(`fauna-core::format_text::POSITIONAL_MERGE_LINE_CEILING`); its value is
unchanged, and it stays a hard-coded Rust constant (`principles.md` § One
configuration surface). Consequence for the hosts: the synchronous stall inside
`resolve_conflict` is bounded on **every** path through the text adapter, which
is what the declined `spawn_blocking` at both call sites
(`SyncEngine::auto_resolve_conflict`, `WsClient::resolve_divergence`) rests on.

> **Implementation status (concurrent resolution, 2026-08-02).** Clauses 1–3
> are how the built system already behaves (resolved reports land winner head
> rows; the resolver is shared and deterministic). Clause 4 is BUILT in both
> hosts: the shared engine (`apply_self_echo`, peer-first batch partition) and
> — since 2026-08-02 — the `fauna-sync` daemon, which now pulls WITHOUT the
> own-device exclusion (`pull_remote_changes`) and partitions its own rows off
> in `apply_caught_up_changes`: peers apply first, echoes advance the base
> only (`SyncWsClient::apply_self_echo`), the anchor advances monotonically,
> and the nudge's per-actor fan-out (author included) is the timely echo
> channel. **Clause 5 status:** the wire + nest pass-through (both record
> planes, list echo, real-time forward, federation relay, report stamping,
> `sync_changes.derived_through`/`is_resolution` columns) and the **daemon
> receiver** (frontier + ledger, `judge_incoming` on both rails, causal
> ancestor + stamped reports, anchor-mirror edit stamps) are BUILT and landed
> 2026-08-02. **The shared engine's receiver leg is BUILT — first conjunct
> 2026-08-02, the rest 2026-08-03.** **The 2026-08-03 gap ruling is BUILT the
> same day, in the shared core (`causal.rs`: `PathFrontiers` with the
> edit-frontier, the content rung behind the adopt licences,
> `ReissueOfHeldContent`), both hosts (engine `download_and_write_file` +
> fold + `apply_self_echo`'s author-blind arm + the proven-reissue upload
> stamps; daemon both rails — including the catch-up conflict arm, which
> previously never consulted the licence — + `apply_self_echo` + the
> `LocalWins` accounting the engine twin already had), and the nest (the
> resolved-report winner-stamp upgrade). Model and production share the one
> decision core, so the tier_1 fuzzer exercises shipped rules directly.**
> **Live pin (2026-08-04):** the same-anchor shape the gap ruling was made for
> is exercised end to end by `tests/test_filesync_seats.py`'s **leg 4a**
> (`helpers/convergence_legs.py`) at every seat count and in every nest mode —
> every seat appends a distinct line at ONE anchor of a common base and the run
> polls until all seats hold one body carrying every append exactly once. It is
> the model's production-path confirmation, covering what tier_1 cannot: the
> nest's winner-stamp upgrade feeding real receivers' adopt rule over the real
> report plane. The winning permutation is arbitrated by the nest log's total
> order and is deliberately not asserted; agreement, no-loss and no-duplication
> are. Leg 4 is a different shape and does not bear on it — its edits sit at
> unique anchors and merge cleanly by construction. Its first run RED'd on the
> third gap (the leg doing its job); the second run — armed against the built
> gap-3 ruling — AGREED across seats but converged onto a duplicated body
> (the report-plane remainder); the loser-row ruling closed that remainder,
> and the armed verification runs then RED'd **leg 4** (the unique-anchor
> leg) — the LEG-4 RULING (2026-08-05, the ruling block's ⚠ decision
> record) closed that in turn: pending-window defer-as-cap, honest winner
> claims, carrier-never-deferred. The leg remains the production acceptance
> gate.
> **Implementation status (the gap-3 ruling, 2026-08-04; the loser-row +
> leg-4 rulings, 2026-08-05).** ALL FIVE gap-3 conjuncts BUILT 2026-08-04
> and LANDED; the loser-row ruling's two conjuncts and the leg-4 ruling's
> three conjuncts are BUILT and land together with this ruling's commit
> (leg-4 build detail: engine `download_and_write_file` returns
> `DownloadOutcome` and the pull loop turns a defer into a dynamic anchor
> cap; the widened defer condition + honest claim in both hosts'
> report paths; the fold's pending conjunct in both hosts) —
> loser-row build detail: shared core: the
> `RetentionRow` rung in both judgement passes; wire/nest: the additive
> `SyncChange.is_retention` field + change-log column, minted on the report
> transaction's loser row, echoed on list and the real-time forward; engine:
> the catch-up interception + the pre-fetch belt + the BASE witness at both
> upload-path stamps; daemon: the pre-fetch arms on both rails, the
> `apply_self_echo` retention arm, the self-echo partition exclusion, and
> the BASE witness (`local_matches_merge_base`) in `handle_local_change`'s
> stamp; both hosts' fold carriers exclude retention rows. Earlier gap-3
> build detail — shared core (`causal.rs`): the class upgrade inside
> `judge_incoming` (both hosts inherit it), plus the `remember_manifest`
> memory for the byte-free judgement sites. Engine: the ledger-widened
> `proven_reissue` at both upload paths (which also licences the record-ack
> advance), the content licence at the batch pre-pass / fold arm (manifest
> memory) / resolver arms / `apply_self_echo` (bytes), the sharpened
> recorded-content conjunct at the manifest-step rung and the divergence
> judgement (which now carries the real held witness, with an explicit
> reissue-skip belt), and the re-assert via `upload_file` (the widened proof
> stamps it a resolution by construction). Daemon: the ledger-proof stamp in
> `handle_local_change`, the content licence at the catch-up pre-pass
> (batch-local manifest dedupe + cache-reassembled ledger check) /
> `apply_self_echo` / all three resolver arms / the verbatim apply, the
> `local_matches_last_sent` sharpening, and the re-assert republish in
> `apply_self_echo`. Both hosts' would-be-covering DEFER arms widened to
> upgraded rows.

> **The third clause-5 gap (found 2026-08-04 by e2e leg 4a; RULED CLOSED the
> same day — this block is the decision record).** **The pre-ruling
> rules did not converge for the SAME-ANCHOR shape once any reissue entered
> the schedule.** Measured twice, from both ends: leg 4a on three real
> daemons against a real nest — every seat duplicated two of three appends
> *and* the seats disagreed — and, in 0.01 s, the tier_1 model, whose shrunk
> counterexample is two steps at two seats:
> `[Reupload { seat: 1 }, Deliver { seat: 1, n: 3 }]`. **Mechanism:** a
> lost-ack retry honestly keeps its EDIT stamp (gap 1's ruling — its record
> may never have landed), and applying it advanced the receiver's
> edit-frontier; every covering resolution published afterwards then read as
> stale against that inflated edit-frontier and rule 2 skipped it,
> permanently. The recorded trade — "an over-count only strands a covering
> resolution" — holds for the own-line shape, where the bytes converge
> regardless; when the fold is order-dependent, order-choices are the only
> thing left to converge on, so a stranded covering resolution IS the
> divergence.
> **The ruling: NOVELTY IS A PROPERTY OF BYTES, NOT OF ROWS — the gap-1
> content rung, which already keyed the receiver's merge-arm skip on content
> rather than the stamp, becomes the licence for ALL novelty accounting.**
> Five conjuncts, each fuzzer-validated (flipping any one re-refutes the
> model): the content-licensed edit-frontier (every advance site, author
> echo and ack included — the frontiers definition above), the widened
> writer-side proven-reissue stamp (the `is_resolution` definition above),
> and the three receiver refinements in the rules — the class upgrade, the
> sharpened published-bytes conjunct, and the re-assert on a stale-declined
> own tail. Convergence is proven WITH duplicate rows in the log, which is
> load-bearing for compat: the nest's content-identical replay dedupe
> (`record_sync_change_metered`, exactly-once by content) keys on the path's
> HEAD row — the only safe key, since deduping against non-head rows would
> eat genuine reverts — so a retry landing after any peer row moved the head
> still mints a duplicate, and old logs / old writers carry them already.
> Zero wire change: everything rides the existing stamp fields and
> client-local state.
> **Recorded residual (accepted):** the engine advances its stored
> edit-frontier at record-ack (load-bearing: waiting for the echo leaves the
> window where a same-batch covering resolution adopts over the un-counted
> edit, measured live 2026-08-03), and a retry acked BEFORE the original's
> echo was ever consumed cannot be content-checked there — ack-time
> inflation persists for that narrow stack (ack lost ∧ retry before the
> original's echo ∧ ledger miss), bounded by the re-assert rule's one-extra-
> row repair and by the nest head-dedupe eating the immediate-retry shape.
> If it ever bites, the additive path is an ack field naming the deduped
> original's seq — do not build it speculatively.
> **Why it hid:** the schedule fuzzer's random walk fuzzed
> `EditShape::OwnLine` only, and the hand-written same-anchor test runs the
> EMPTY schedule — so the one shape whose fold is order-dependent met none
> of the churn alphabet. The walk now fuzzes both shapes, and the flipped
> pin (`same_anchor_appends_converge_even_when_a_reissue_enters_the_schedule`)
> asserts convergence at 2–4 seats under both delivery modes. **Scope note
> (unchanged):** this is about the *receiver rules*, not the resolver —
> resolver-level deterministic same-anchor ordering stays REFUTED
> (non-confluent across multi-round folds) and must not be re-proposed.
> **The REPORT-PLANE remainder (found 2026-08-04 by the armed leg's re-run;
> RULED CLOSED 2026-08-05 — this block is the decision record).**
> With all five gap-3 conjuncts built, the live 3-seat re-run **agreed**
> across seats — the divergence half was fixed end to end — but converged
> onto a body with two of three appends **duplicated**. Root cause: the
> RESOLVED-REPORT PLANE was absent from the tier_1 model, which is why the
> fuzzer validated the gap-3 ruling while the live run refuted its
> completeness. A daemon merge does not publish one resolution row: it files
> a resolved report, and the nest's transaction mints a **loser-retention
> row** — an ordinary `sync_changes` row, edit-class, stamped at the
> reporter's ANCESTOR seq (`losing_derived_through`), carrying the
> reporter's pre-merge candidate — plus the winner row (`sync_storage.rs`,
> the resolved-conflict insert; the winner-stamp upgrade covers its loser
> only when no same-path row intervenes, rare in a cascade — the upgrade is
> RETIRED 2026-09-27, § *Retention rows are transparent to the licence*).
> At three seats
> the loser candidates are never-published INTERMEDIATE merge results, so at
> peers the loser row was a stale edit-stamped row whose bytes no ledger
> holds — a stale-resolution skip is byte-free, so a peer that stale-skipped
> the winner never held the intermediate either — and the same-anchor merge
> of a derivative candidate from its deep ancestor was the duplication
> engine (the resolver concatenates same-anchor insertions), each such merge
> filing the next report. **Model-reproduced 2026-08-05** with the report
> plane in the tier_1 alphabet (and the model's stale-resolution skip made
> byte-free to match production — its old ledger-hold there was the mask),
> which also WIDENED the evidence: the append shape duplicated at EVERY seat
> count once reissue churn entered the schedule, and own-line LOST an edit —
> a loser row's stale content won a latest-wins (its `created_at` is the
> report's mint time, so it reads newest) at 3 seats under batch-boundary
> schedules, at 4 even on the plain drain.
> **THE RULING — two conjuncts, each fuzzer-validated (flipping either
> re-refutes the model):**
> **(1) A loser-retention row is a RETENTION VEHICLE, not a change to apply
> — retention rows are INVISIBLE TO CONTENT.** The nest marks the resolved
> report's loser row with the additive `is_retention` flag
> (`SyncChange.is_retention` on the wire; an additive nullable column on the
> change log; minted ONLY by the nest's report transaction, never
> client-recorded). Every receiver accounts the row's seq into the path
> FRONTIER and does nothing else: never fetches, applies, merges, or adopts
> it; never advances the edit-frontier (its bytes are a pre-merge candidate
> — the reporter-side echo advancing the stored edit-frontier was the
> stranding's other half); never holds it in the ledger (it was no one's
> reflected content, so it can never be a correct common ancestor); and it
> can never carry a fold (folding rows under a skipped carrier would consume
> their content undelivered). The rung lives in the ONE shared decision core
> (`causal::IncomingVerdict::RetentionRow` — first after the duplicate
> guard, deliberately outside the tracked-frontier gate so a fresh receiver
> skips it too), with explicit retention arms at both hosts' self-echo
> paths. ⚠ One interplay is load-bearing and cost the armed cell a red run
> to find (2026-08-05): both hosts' resolver arms hold the in-flight
> defer-guard "until the report's loser row echoes" — so the retention
> arms at the self-echo paths and the catch-up partitions must still CLEAR
> that guard when the own retention row appears (its seq is then known),
> while skipping the edit-frontier advance; excluding retention rows from
> the partition wholesale takes the clear with it and defers every
> covering winner forever. Nest-side retention is UNTOUCHED: the row still
> GC-pins and lists
> the losing candidate, and clause 1's `use_other_version` re-points among
> retained candidates via a NEW head row — peers never apply retention rows.
> Old receivers ignore the marker and degrade to the pre-ruling behaviour
> (additive-everywhere; the wire-compatibility corollary holds — the
> residual is that a not-yet-updated receiver still merges retention rows
> until it updates).
> **(2) The BASE witness widens the writer-side proven-reissue stamp** (no
> wire change): a re-upload of bytes EQUAL TO THE LIVE MERGE BASE is a
> proven reissue — the base only ever advances on an echo (row in the log),
> an applied remote (row in the log), or a fail-closed merge write whose
> resolved report landed transactionally (winner row in the log) — so it
> stamps `is_resolution`; only a retry of base-diverged content (potential
> unpublished user novelty, gap 1's shape) keeps the edit stamp. Without
> this conjunct the model refuted conjunct (1) alone in two steps
> (`[Deliver{b,1}, Reupload{b}]`, own-line, 3 seats): a reconcile re-upload
> of a merge RESULT — no row's bytes, so never ledger-held — wore the edit
> stamp, receivers' edit-frontiers inflated to its seq, every
> union-carrying winner read stale forever, and the stale-ancestor
> latest-wins destroyed the third seat's edit. Writer sites: the engine's
> two upload paths (`proven_reissue`), the daemon's `handle_local_change`
> stamp (`local_matches_merge_base`).
> **Validation + the flipped pin:** the pin
> (`report_plane_retention_rows_are_accounted_and_never_applied`) asserts
> the ruled behaviour and keeps the defect narrative; the full-strength
> model suite (2–4 seats, both shapes, both delivery modes, the
> deterministic searches and the random walk) is green with both conjuncts
> built and red with either flipped. Leg 4a remains the production
> acceptance gate.
> ⚠ **LIVE-REFUTED (2026-08-05, the armed verification runs) → RULED
> CLOSED the same day (the LEG-4 RULING — this block is the decision
> record).** Two armed runs of the 3-seat engine cell RED'd **leg 4** (the
> unique-anchor leg, green pre-ruling), losing a different seat's line each
> run, while the tier_1 model stayed green: the model's own-novelty
> machinery (a seq-exact in-flight fold into every judgement) was STRICTLY
> SAFER than production's (the partition pre-pass + the unknown-seq report
> flag + defer-and-drop). With the machinery production-shaped — cited line
> by line from `engine.rs` — the schedule fuzzer reproduced the loss in its
> first pass: FIVE plain deliveries, no churn steps at all. **The defect
> chain (all three links needed):** (1) a fast-forward licensed by the
> RECORDED witness alone (`recorded_content_hash` matching local while
> local ≠ base) adopted over published-unechoed own work; the dropped row's
> echo then FALSELY advanced the frontiers — "local reflects it" was no
> longer true; (2) the reports' `winning_derived_through = incoming_seq`
> stamp OVER-CLAIMED coverage of the dropped row (the lower-bound law
> violated), and every covering test downstream trusted it — at the author
> arm this is the live red's `own covering resolution re-adopted` line;
> (3) the engine's covering-adopt defer consumed-and-DROPPED the deferred
> row (the pull anchor advances whatever the arms do, and nothing re-lists
> a consumed row), permanently discarding the covering winners that carried
> the repair.
> **THE LEG-4 RULING — three conjuncts, each fuzzer-validated (flipping
> any one re-refutes the model):**
> **(1) A verbatim adopt is NEVER taken while the receiver's own pending
> rows are unlisted.** Both windows defer: the unknown-seq report window
> (the in-flight flag) and the ordinary ack→echo window, where the licence
> rides the recorded witness alone (local ≠ live base). The defer is
> TRANSIENT-CLASS, never a drop: the daemon holds the row (no write, no
> anchor — its rails redeliver); the engine turns it into a dynamic batch
> CAP (the seal-cap family) — rows at and past the deferred seq wait, the
> anchor holds below it, and the next pull re-lists them together with the
> pending own rows (log contiguity), so the retried judgement is exact.
> The gap-3 anti-duplication adopt is thereby DELAYED, never weakened —
> and the poisoned-frontier state (an own echo advancing frontiers for
> content an adopt dropped) becomes unreachable.
> **(2) The winner claim is honest under the lower-bound law.** A report's
> `winning_derived_through` is the incoming row's seq only when the merge
> provably incorporates every row ≤ it: the seq is contiguous with the
> reporter's pre-merge frontier, or — on the engine host, with the listing
> in hand — every same-path gap row below it is the reporter's own (their
> content is reflected in local by conjunct (1)'s invariant). Otherwise
> the claim falls back to the frontier: an under-claim only strands
> (repaired by the re-assert); the over-claim was the loss. The nest's
> winner-stamp upgrade composes unchanged — its intervening-row window
> contains the incoming row whenever the claim fell back, so the upgrade
> correctly declines (the upgrade is RETIRED 2026-09-27 — the receiver's
> content frontier carries this now, § *Retention rows are transparent to
> the licence*; the claim's honesty is unchanged).
> **(3) The fold's licence gains carrier-never-DEFERRED** beside
> carrier-never-stale: a content carrier that would defer under conjunct
> (1) must not carry — its folded rows' content would be consumed under a
> row that never applied. The test is the defer's exact reachability (the
> report window, or local ≠ base while the recorded witness matches it);
> an unrecorded divergence merges instead — a merging carrier still
> applies — so the fold behaviour is untouched; DELETE
> carriers are exempt (never judged, cannot defer). While a window is
> open, the listing processes per-row.
> **Validation:** the flipped pin
> (`leg4_unique_anchor_plain_deliveries_converge`) and the full-strength
> model suite (2–4 seats, both shapes, both delivery modes, the
> deterministic searches and the random walk) are green with all three
> conjuncts built and red with any flipped; engine lib 370-green; daemon
> suites green. **The armed run (2026-08-05, second): LEG 4 GREEN — the
> ruling is LIVE-VERIFIED on the leg that refuted its predecessor.**
> ⚠ **Conjunct (1) premise defect found + closed 2026-08-06 (the ack-advance
> livelock — leg 4 intermittent on the primary Linux dev VM, 1-PASS/2-FAIL on
> one tree).** The
> defer's transient-class promise — *"the next pull re-lists them together
> with the pending own rows (log contiguity), so the retried judgement is
> exact"* — assumed the pending own row IS in the next listing. The daemon's
> receive loop broke that assumption from outside the ruling: it anchored on
> the data-plane `Ack` of the device's own `FileChanged` (a pre-clause-4
> leftover), DE-LISTING the own row before its echo bookkeeping ran, so
> every retry re-judged fast-forward on an under-counted edit-frontier and
> re-deferred, identically, forever. No conjunct changed — the rules judge
> exactly once the row lists; the fix is the **accounting law** (owner:
> [file-sync.md](file-sync.md) § 5 Offline Catch-Up): only the accounted
> catch-up walk advances the anchor, single-row acknowledgments never do.
> Refutation record: `merge_convergence_test::
> daemon_ack_anchor_advance_past_own_row_wedges_refuted` (the model gained
> the rail, watched it wedge, and pins its absence).
> ⚠ **Conjunct (1) premise defect, second — the FRESH-BIND PARK (found
> 2026-09-20 at tier_1 and live; RULED CLOSED 2026-09-21 — this block is the decision record).** The
> transient promise assumed once more that the retry DIFFERS: that the
> pending own row, once listed, is COUNTED by the judgement that deferred.
> On an UNTRACKED path it was not. `judge_incoming`'s tracked arm grants a
> fast-forward only to a watermark dominating the effective edit-frontier —
> the count the record ack (engine) and the listing pre-pass (both hosts)
> stamp for this device's own just-recorded row, ahead of the frontier its
> echo alone advances — but its two untracked arms (a watermarked row on a
> path with no frontier, and the no-watermark degrade) read the recorded
> witness alone. So a fresh bind — every caller's startup path records the
> local file before the first pull; no frontier, no live base — judged a
> peer's EARLIER create at the same path fast-forward, the cap held the
> anchor below it, and the cap skipped everything above it: the own echo
> included, the one row whose processing changes the retry. Every pull
> re-judged identically and the folder silently stopped receiving anything
> (measured: `causal: verbatim adopt deferred … seq=11` / `pulled remote
> changes changes=0 anchor=10`, unchanged on every pull; the daemon's
> `DeclinedHold` → `break` is the same park, own echoes above the held seq
> never reaching its echo tail). The model never saw it because every seat
> it ran was SEEDED — tracked at 0, a ledger holding the seed row.
> **The ruling — the untracked arms honour the counted edit-frontier, the
> same conjunct the tracked arm demands:** a watermarked row fast-forwards
> only if `w ≥ ef`; an unwatermarked row only if `seq > ef` (the log-order
> floor: a row at or below the count was recorded before the own row and
> cannot have incorporated it). Both are STRICTLY FEWER fast-forwards, never
> more, so conjunct (1) is not re-opened: a would-be-deferred adopt becomes
> the sibling MERGE the tracked arm already gave the same row. Neither
> option the track named was taken: letting own echoes past the cap
> over-claims the frontier for a row never merged (the poisoned state in a
> new coat), and counting the recorded head as the live base deletes
> conjunct (1)'s ack→echo window. **Corollary — cap-above-own:** a capped
> seq now always sits ABOVE every own row this device has counted (a
> fast-forward needs `w ≥ ef`, and `w < seq` — a fact the RECEIVER enforces,
> not the writer's word: the watermark-bound record below), so the
> releasing own echoes
> are BELOW the cap and process in the same pass: a cap lasts exactly one
> pull. **Residue, not ruled here:** an own RESOLUTION-class row (a revert
> to bytes the ledger or base already holds — a proven reissue) is counted
> by neither frontier by design, so a peer edit ABOVE the count that sits
> BELOW that row in the log can still cap under it; the honest verdict
> there is a ruling of its own — the record after this one's validation
> (RULED the same day).
> **Validation:** the shared judge's unit
> `causal::tests::a_row_below_the_counted_own_novelty_is_a_sibling_on_an_untracked_path`;
> the engine's production pins
> `create_arm_local_conflict_test::a_fresh_binds_counted_own_row_releases_the_cap_for_a_watermarked_create`
> and `…_for_an_unwatermarked_create` (red-first against the pre-ruling
> judge: anchor 0, the barrier row never written) beside the withheld-count
> pin, which still asserts the hold; the daemon's
> `ws_client::tests::a_counted_own_row_releases_the_causal_hold_on_a_fresh_bind`
> (the hold with nothing counted, the sibling judgement once the pre-pass
> has counted); the oracle's
> `merge_convergence_test::a_fresh_binds_counted_own_row_releases_the_defer_cap`
> — the model gained a FRESH-BIND seat, untracked until its first advance —
> with the full model suite green; and the carrier-less tier_3 shape
> `test_filesync_bind_history.py::test_fresh_bind_onto_a_same_named_never_deleted_file_keeps_syncing`
> (the peer's file replayed, the user's bytes kept, a later local file
> still uploading).
> ⚠ **The resolution-class residue above — the HELD-BYTES RELEASE (RULED
> CLOSED 2026-09-21 — this block is
> the decision record).** The cap's ack→echo window exists to protect own
> NOVEL bytes: content the log would lack if a verbatim adopt dropped it
> before its carrier listed, and whose echo then falsely advances the
> frontiers. An own pending row whose bytes the ledger ALREADY HOLDS at or
> below the frontier — a revert to a held version, which `upload_file`'s
> widened proven-reissue proof stamps resolution-class and neither frontier
> counts — protects nothing the log lacks, and holding for it parked the
> anchor below the row's own echo for ever (measured at the oracle: two
> seats drained to the log's tail on the union, b appends an edit stamped at
> the tail, a reverts to the seed and re-uploads, a's next listing never
> quiesces). **The ruling — the
> cap's firing rule lives ONCE, in `causal::verbatim_adopt_deferred`, called
> by both hosts and the model:** hold while own novelty is in flight (the
> report window), or while local ≠ live base AND local is not ledger-held at
> or below the frontier; a held-bytes pending row never holds. The covering
> peer edit is then adopted, and the revert's later echo — a stale-declined
> own tail, its watermark below the edit-frontier the adopt advanced —
> RE-ASSERTS the current bytes (the gap-3 arm both hosts already carry), so
> every seat converges on the edit: the reverter loses its stale revert
> exactly as peers skip it under rule 5's ratified trade, which stays
> intact. The row's merge alternative was NOT taken: it would keep the
> revert's intent at the reverter while peers skip the revert — a divergence
> until the merge report lands — and it changes what a revert's report may
> claim (the same-anchor conjunct 2). Scope: reachable by an editor
> undo-then-save or a snapshot restore (`restore_snapshot_walk`) writing
> held bytes into a live watch dir; the Media/shell restore-version verb
> records an unstamped edit-class modify and is not a trigger — no tier_3
> shape. **Validation:** the oracle's
> `merge_convergence_test::a_reverts_own_held_bytes_do_not_park_a_covering_peer_edit`
> (the alphabet gained `Step::Revert`, deterministic-pin only; red-first: no
> quiescence), the engine's
> `create_arm_local_conflict_test::a_reverts_own_held_bytes_do_not_park_a_covering_peer_edit`
> and the daemon's
> `ws_client::tests::a_reverts_own_held_bytes_do_not_hold_a_covering_peer_edit`
> (both red-first: the cap/hold with the anchor unmoved), the predicate's
> unit test, and the full model suite green.
> ⚠ **THE WATERMARK BOUND — the receiver's half of the lower-bound law
> (found 2026-09-21 grading the two cap releases above; RULED CLOSED the
> same day —
> this block is the decision record).** "Stamp the persisted catch-up
> anchor, never more" is the WRITER's duty, and nothing held a writer to it:
> the nest stores `derived_through` as sent and assigns the row's seq
> afterwards, and both watermarked arms of `judge_incoming` judged the row
> by its claim alone. So the cap-above-own corollary's `w < seq` was an
> assumption about honest writers, and one forged field re-opened the
> fresh-bind park: a stale row claiming `w ≥ ef` read fast-forward over this
> device's counted own row, the leg-4 cap held the anchor below the own echo
> that releases it, and every pull re-judged identically — the victim's
> whole folder set silently stops receiving, for ever, at the cost of one
> lying field per row to any writer member (the nest can stall sync anyway;
> the member is the new exposure). No bytes are adopted while the cap
> holds, so it is an availability defect, never a content loss. **The
> ruling — every receiver reads a watermark as `min(w, seq − 1)`
> (`causal::bounded_watermark`):** the log itself refutes a claim at or
> above the row's own seq (a row cannot have incorporated itself, nor
> anything recorded after it), an honest anchor is always below the seq the
> nest assigns later so **no honest verdict moves**, and a forged row is
> judged exactly as its most-caught-up honest sibling. The bound sits in
> BOTH public judges (`judge_incoming_before_fetch` too — both hosts act on
> the byte-free verdicts before the fetch, and a bound in the full judge
> alone would MERGE a forged resolution its honest sibling stale-skips) and
> where a row enters each host (the listing; the real-time forward's apply
> door), so the reads outside the judge — the merge-ancestor lookup, the
> fold licence, the held-content rung — never see the raw claim either: an
> unbounded ancestor lookup would hand a forged sibling the receiver's own
> newest held version as "common ancestor", turning the merge into an adopt.
> **Not taken — a nest-side clamp or refusal at record time:** a refusal is
> a wire-visible change, and a silent clamp is redundant — a receiver can
> never rely on it, because the nest is itself a party that may forge the
> field, so the receiver bound is necessary either way and sufficient alone.
> **The resolved report's two claims, traced:** `losing_derived_through`
> lands on a retention row, which the retention rung accounts without
> reading a watermark; `winning_derived_through` lands on the winner row,
> minted AFTER the claim (and after the loser row, whose seq the nest's
> honest-claim upgrade — RETIRED 2026-09-27, § *Retention rows are
> transparent to the licence* — substituted when no same-path row
> intervenes), so the same receiver bound covers it. **What the bound does
> not close, by construction:** a claim inside `[honest anchor, seq − 1]` is
> unfalsifiable from the log. It buys the forger a latest-wins over
> versions that stay retained and listable — a power a writer member
> already holds by writing — and never a park: a fast-forward needs
> `w ≥ ef`, the bound makes `w < seq` a fact, so the cap-above-own
> corollary now holds for a dishonest writer too. **Validation:**
> `causal::tests::a_forged_watermark_is_bounded_by_the_rows_own_seq` (each
> forged row paired with its honest sibling, both judges); the engine's
> `create_arm_local_conflict_test::a_forged_watermark_does_not_repark_a_fresh_binds_counted_own_row`
> and the daemon's
> `ws_client::tests::a_forged_watermark_does_not_rehold_a_fresh_bind` (the
> fresh-bind pins above with the peer's claim forged to `i64::MAX`;
> red-first: the cap/hold with the anchor unmoved).
> ⚠ **THE SAME-ANCHOR RULING (2026-08-05 — the leg-4a remainder, RULED; this
> block is the decision record).** The armed run's agreement-with-loss (all
> seats on a body missing one seat's append) was model-reproduced the same
> day: the tier_1 model gained the DAEMON host's rails (the e2e "engine"
> seats are headless `fauna-sync` daemons) plus the two live WATCHER shapes
> it lacked — a mid-run edit stamped at the seat's current anchor, and the
> PENDING window (an append on disk whose upload hasn't fired; a conflict
> merge that overwrites the file first consumes it, so it never becomes a
> row and BOTH frontiers are structurally blind to it). The seq-less-rail
> suspicion was REFUTED (every rail carries the seq; the real-time forward
> reaches only admin-registered destination rows no app creates — delivery
> is nudge + catch-up). **FOUR conjuncts, each pinned by the model
> (`merge_convergence_test.rs` — the pending-append pin, both-host
> plain-schedule searches, deterministic searches at 2–4 seats):**
> **(1) Insert-overlap merges are LINE-MULTISET UNIONS, never latest-wins**
> (`fauna-core::format_text`): a subset side adds nothing (the superset
> stands byte-exact — the live kill where a late watcher upload of a
> subset destroyed the converged union fleet-wide); two sides extending
> the base with mutual novelty merge as ours-order + theirs' uncovered
> lines (positional diff of permuted supersets misreads shared lines as
> conflicting replacements → latest-wins → the losing side's novelty
> died); per/cross-anchor insert unions dedupe shared lines (the
> duplication signature). No canonical order is chosen — sequences
> converge via nest-log supersession, as ratified.
> **(2) A winner that consumed UNPUBLISHED local novelty is EDIT-class** —
> the ratified `is_resolution` semantics ("carries no novel content")
> applied honestly: such a winner IS the novelty's only carrier, so the
> resolution stamp on it licensed byte-free stale-skips that killed the
> last off-disk copy while skippers' frontiers kept feeding claims that
> adopted the on-disk copy away. Edit-class, receivers MERGE it (lossless
> under conjunct 1), their edit-frontiers count it, the reporter's echo
> binds it through the standing non-resolution accounting. Wire: the
> resolved report carries the winner class (additive; old nests stamp
> resolution as today and degrade to the pre-ruling behaviour).
> **(3) Daemon parity:** the published-bytes witness joins the catch-up
> conflict test (`detect_catchup_conflict`'s base-slot-only routing sent
> published-but-unechoed content to the resolver's latest-wins with no
> defer — the "no defer line" evidence); the daemon's winner claim gains
> the engine's own-gap widening (contiguity-only under-claims made every
> merge winner stale at every peer — the model's 2-seat mutual-strand
> divergence); and a catch-up defer STOP still processes own echoes BELOW
> the deferred seq (the `if !progress.stopped` gate starved the very
> base-advance that closes the defer's ack→echo window — a livelock the
> model measured).
> **(4) The pending-report window is PER REPORT, keyed by winner content**
> (production: winner manifest): a single in-flight bit let an older
> report's winner listing clear the window while a newer report's
> novelty-carrying winner was still unlisted, releasing the covering
> adopt over it. Armed at the resolver arms, retired one occurrence per
> own winner listed/echoed; the RETENTION row never closes the window
> (the winner — transactionally co-minted — is what closes it).
> **Status:** ruled + model-built (382-green with all pins flipped;
> conjunct 1 is production code already — `format_text.rs`). Production
> build of conjuncts 2–4 in both hosts + the nest stamp is in flight on
> the held branch; leg 4a remains the acceptance gate and the build stays
> branch-side until it runs green armed.
> **First conjunct (2026-08-02):** the fold's "batch-latest **PEER** row"
> requirement, enforced on its own (`content_superseding_seq_by_path` — every
> delete plus every peer create/modify; a self-echoed create/modify writes
> nothing, so it supersedes nothing). That alone closed a measured silent loss:
> a batch holding a peer's concurrent edit *and* this device's echo at a higher
> seq skipped the peer's edit, and since the anchor advances past a skipped
> change it was never redelivered — no download, hence no merge and no conflict
> row. Pinned tier_1 by
> `a_self_echo_does_not_suppress_a_peers_concurrent_change_in_the_same_batch`
> plus its control.
> **The rest (2026-08-03):** `download_and_write_file` takes the byte-free
> verdicts before the fetch (`judge_incoming_before_fetch` — duplicate / stale
> resolution), judges sibling-vs-descendant at the divergence decision,
> overrides the merge ancestor from the ledger bound, stamps its own edits and
> reports, and the fold gained its watermark conjunct and became all-or-nothing
> (`fold_licensed`). **The two fold rules are ANDed and neither subsumes the
> other:** the first decides *which* row may supersede (excluding self-echoes),
> the second decides *whether* folding is licensed at all — and with two
> concurrent PEERS in one batch the first names the higher-seq peer as
> superseding, which is exactly the N≥3 lost edit the second refuses. Verified
> by `test_filesync_threeseat.py [engine+engine+engine]` (605s, the former
> red; both seat modules since folded into `test_filesync_seats.py`,
> 2026-08-03 — the cell is now `[3seat-engine+engine+engine]`), the two-seat
> module at two and three seats, and
> `tests/platform/sync/test_text_merge.py` — the non-convergence instrument
> that refuted two earlier candidates. Native app seats therefore inherit the
> fix through the shared engine; the windows `[native+native]` leg-4 proof
> landed 2026-08-03 (CLOSED — `[native+native]`
> green on all five legs), and convention 16's native-seat arm across every
> platform has since completed (`e2e-live-sync-convergence.md` convention 16). ⚠ Own rows must NEVER be fed to the apply path: the
> resolved report's transactional loser-retention row is attributed to the
> reporting device, and applied as if from a peer it fast-forwards the
> just-merged file back to the losing candidate. Pinned tier_1 by
> `a_self_echo_advances_the_base_only_while_local_matches` (ws_client) and
> tier_3 by `tests/platform/sync/test_text_merge.py::
> test_text_merge_concurrent_edits`, whose final assertion (head row == the
> reported winner) is exactly the clause-3 outcome — do not weaken it to
> tolerate stale-ancestor artifacts; it is the freshness rule's regression
> guard.

> **The two clause-5 gaps (measured 2026-08-03 by the schedule fuzzer's first
> run; RULED CLOSED the same day — this block is the decision
> record).** Both were gaps in the *ruling*, not in the code implementing it;
> both were the N≥3 / shared-anchor class the hand-written schedules never
> reached. Zero wire-schema change: both fixes ride the existing
> `derived_through`/`is_resolution` fields, additive-everywhere.
>
> 1. **`is_resolution` was not a sound proxy for "carries no novel
>    content".** A reconcile re-upload of *already-incorporated* content was
>    stamped `CausalStamp::edit(anchor)` at a possibly stale anchor and a NEW
>    seq, so it escaped rule 1 and rule 2, reached the merge arm, and merged
>    from the old ancestor against a peer that already folded that content in
>    — overlapping-identical hunks → latest-wins destroyed the peer's edit.
>    **Ruled closed twice over, split by what the writer can prove:** the
>    re-seal migration holding its recorded-content proof now stamps
>    `is_resolution = true` (the bit's meaning widened to "carries no novel
>    content"; old receivers then skip it via rule 2 for free), and the
>    receiver-side **content rung** (rule 5) absorbs what no writer stamp can
>    fix — the lost-ack retry, whose record may never have landed, keeps its
>    honest edit stamp and is skipped only when the receiver PROVES the bytes
>    already incorporated. Pin (flipped to assert the fix):
>    `a_reconcile_reupload_of_incorporated_content_is_absorbed_as_a_reissue`.
> 2. **Rule 2 stranded seats when the fold is order-dependent.** Three seats
>    APPENDING at a shared anchor — zero churn, plain in-order delivery —
>    quiesced holding three different permutations: each folded its peers'
>    rows in its own order (a three-way merge of same-anchor insertions is
>    non-commutative), published its permutation as a resolution, and every
>    peer skipped that row as a stale resolution. Rule 2's rationale — a stale
>    resolution "adds zero information" — held for the row SET it incorporated
>    and failed for the BYTES it produced. **Ruled closed by the
>    edit-frontier + covering-resolution adoption (rules 2–3):** staleness now
>    means "misses novel content", and a covering resolution is adopted,
>    author-blind, with the nest log's total order as the arbiter every seat
>    already shares — all seats converge on the log-tail covering resolution,
>    and adoption publishes nothing, so the fold terminates. **A
>    resolver-level deterministic ordering was REJECTED:** block-level
>    ordering is not confluent across multi-round folds (the result depends on
>    how earlier merges grouped the insertion blocks), and line-level sorting
>    scrambles multi-line user blocks — supersession, not fold confluence, is
>    what converges. This still bounds *Equal ancestors make the first winner
>    durable* above: `resolve_conflict` stays deterministic per CALL only.
>    Pin (flipped): `concurrent_same_anchor_appends_converge_at_three_and_four_seats`.
>
> **The same-day hardening (the first build was refuted LIVE by the 3-seat
> e2e cell within the hour, then three more times by the fuzzer once the
> model mirrored production's clause-4 partition — own echoes processed
> last).** The root of all four refutations: the edit-frontier's upper-bound
> law binds for the device's OWN work too, and echo-time accounting
> under-counts it. The ruled closes, all landed the same day: **own novelty
> is accounted the moment its seq is knowable** — at record-ack (the engine's
> `record_change` returns the seq), at listing time (both hosts fold own
> non-resolution rows' seqs into the stored edit-frontier at batch partition,
> BEFORE any peer row is judged), and at consumption (an own row advances the
> edit-frontier whichever arm takes it — a rule-1'd own echo must not drop
> its novelty); **an edit's fast-forward watermark must dominate the
> effective edit-frontier as well as the frontier** (a reupload with `w ==
> frontier` otherwise fast-forwards over the receiver's own in-flight edit);
> **own rows are judged by the author arm BEFORE rules 1–6** (as production's
> partition already routes them), and the author-blind re-adopt works on
> late echoes too — the ordering guard is a ledger lookup (newest held row
> whose bytes equal local), because seq-vs-frontier cannot say which row
> local came from; and **for the residual unknown-seq windows** (a
> publication sent but not yet listed; a resolved report's rows, whose seqs
> the reporter never learns) each host keeps a per-path in-flight flag and
> **DEFERS a would-be covering adopt** (decline-hold, redelivered) rather
> than merging it — a blocking guard was refuted twice (merge-and-consume
> strands the tail permanently; a latch livelocks into a 400-row runaway).
> The tier_1 model mirrors the partition, so these paths stay fuzzed.
>
> Two more consequences a future session should not re-derive: **(a)** ⚠ RETIRED
> 2026-09-27 and BUILT (§ *Retention rows are transparent to the licence*: the
> nest mints the claim as sent — `sync_storage.rs::report_conflict` — and the
> receiver's content frontier replaces the upgrade: `CausalStore::account_retention_row`,
> the one funnel every retention accounting site on both hosts calls, and
> `judge_incoming`'s edit arm reading `PathFrontiers::effective_content_frontier`;
> pinned by `causal::tests`, the tier_1 model's
> `an_edit_class_winner_behind_its_retention_row_fast_forwards_at_a_caught_up_seat`,
> the daemon's retention cell and `conformance_folders.rs`). What the code
> used to do: the nest's
> resolved-report transaction stamps the propagated winner's
> `derived_through` up to the just-inserted loser-retention row's seq when no
> other same-path row intervenes (`sync_storage.rs`, the resolved-conflict
> insert) — without that honest upgrade the loser row (an edit) bumps every
> receiver's edit-frontier past the winner's watermark and the adopt of rule
> 3 never fires on the report path, which is where production resolutions
> actually propagate; the upgrade is licensed by construction (the resolution
> incorporated the loser candidate) and refused when an intervening foreign
> row would be over-claimed. **(b)** The rung deliberately loses one case: a
> stale-watermarked genuine revert to previously-held bytes is skipped at
> peers (rule 5's recorded trade) — re-revert once caught up; the pre-ruling
> alternative silently destroyed the peer's newer concurrent edit instead.

> **Implementation status (the ruling's two halves, 2026-08-04).**
> BOTH BUILT in the shared engine's fold licence (`engine.rs`,
> `fold_licensed`), each tier_1-pinned red-first in
> `create_arm_local_conflict_test`: **the same-author delete rung**
> (`a_delete_supersedes_its_own_authors_earlier_create_in_one_batch` — red
> showed the fold refused on an honestly under-covering delete stamp, stale
> create downloaded, spurious conflict recorded) and **carrier = latest
> superseding row, not the raw batch tail**
> (`an_own_echo_above_the_carrier_does_not_disable_the_fold` — red
> reproduced the live agent-log shape: reconcile-first upload puts the own
> echo at the batch tail, and the raw-tail rule refused a fully-covering
> peer delete's fold). The daemon needs no twin — it applies per-row and
> never folds. The tier_1 model mirrors the carrier selection at both fold
> sites but deliberately does NOT carry the same-author rung (no deletes in
> its alphabet), and the schedule fuzzer is the rung's boundary pin: its
> first run REFUTED the content-carrier variant (the counterexample: folded
> same-author rows' skip-holds are what the content rung needs to absorb a
> later stale-watermarked reissue), which is why the rung is delete-gated.
> The prior "does not reproduce" close of the create-arm residue
> (2026-07-31) is NOT contradicted: its pin covers a batch
> with nothing to fold and stays; the e2e red it was blamed for was a
> regression from the 2026-08-03 watermark conjunct + raw-tail carrier rule
> meeting the real bind flow — both tests were right, the licence was
> incomplete.
> ✅ **The "never under a stale-skippable carrier" conjunct is BUILT in the
> engine as of 2026-08-04, closing the 2026-08-04 correction that
> recorded it MODEL-ONLY.** `fold_licensed` now vetoes a carrier the receiver
> rules would skip, and it asks `judge_incoming_before_fetch` itself rather
> than re-deriving the comparison, so the licence and the skip cannot
> disagree. That indirection is load-bearing, not tidiness: **rule 2 only
> skips when a FULL frontier is tracked**, so the bare
> `w < effective_edit_frontier` comparison — which is what the model uses,
> soundly, because its seats always track both — over-refuses on an untracked
> path, downloading the stale row against the user's file and manufacturing
> exactly the spurious review row the same day's carrier ruling had just
> stopped. Reachable, not hypothetical: the record-ack site advances the
> EDIT-frontier alone, so a device holds `edit_frontier = Some(seq)` with the
> full frontier still `None` until its echo returns. Only `StaleResolution`
> vetoes (a `Duplicate` carrier sits at or below the frontier, so every row it
> folds is already reflected), and a DELETE carrier is outside the conjunct's
> territory — never judged, so never stale-skippable. Three mutation-graded
> tier_1 pins in `create_arm_local_conflict_test`: the loss itself
> (`a_carrier_that_would_be_stale_skipped_does_not_license_the_fold` — red
> showed the folded edit never fetched, no merge, no conflict row, anchor
> advanced), the blast-radius control
> (`a_covering_resolution_carrier_still_licenses_the_fold`), and the boundary
> (`an_untracked_frontier_does_not_make_the_carrier_stale`, which is what
> caught the bare-comparison shape). The daemon still needs no twin — it
> applies per-row and never folds.
> engine: the delete arm's keep verdict distinguishes conflict-keeps from
> silent-keeps, and `SyncEngine::report_declined_delete` runs the
> upload-first resolved report + `KeepLocal`-style row commit (tier_1 pins in
> `pull_remote_changes_test.rs`; badge pin in `fauna-folders-machine`).
> Zero wire or nest changes: `conflict_type` is free-form pass-through,
> `latest_wins` is an already-validated resolution, and a single-candidate
> resolved report propagates by the existing choose-winner precedent. Clients
> shipped before this render the row as "Latest kept" — graceful, additive.
> **The tier_3 end-to-end proof landed 2026-07-29** —
> `test_declined_delete_of_a_mid_debounce_edit_keeps_the_file` (tier_3,
> `--client tui`, `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py`):
> a real app + external `fauna-sync-agent` + local nest sync a file to
> Synced, edit it on disk, and delete it through the Media UI while the edit
> sits mid-debounce — deterministic via the new `FAUNA_E2E_DEBOUNCE_MS`
> compile-gated override (`always_resident.rs::debounce_delay`, e2e-conventions.md
> convention 15) holding the live watcher's debounce open indefinitely, with
> the set's own rescan cadence pushed past the test's budget too (both of the
> engine's upload paths — the debounced watcher and the periodic `converge`
> catch-up — must clear the window, not just one). The `delete_declined`
> conflict review row is the causal barrier the survival assert anchors to.
> Red-verified by reverting the `KeepConflict` arm in `engine.rs` (every
> settled-row delete applies unconditionally) and confirming the file is
> destroyed instead of declined. **Landed alongside it:** tui gained the
> same-nest remote-change push-nudge relay linux already had
> (`PushEvent::SyncChanged` → `SyncAgentState::pull_set_now` → the
> `PullFolderNow` IPC) — closing fan-out leg 5 of the
> `fauna.sync.changed` reaction-arm track — because it turned out to be a
> genuine prerequisite: on tui the periodic rescan tick's own `converge` call
> always precedes its `pull_remote_changes`, so without an off-cadence
> pull-only path the tick can never observe a delete without first re-uploading
> whatever edit is pending, structurally precluding the KeepConflict arm from
> ever firing.

## Skipped catch-up changes reach the review list (ratified 2026-10-02; shared half BUILT — status paragraph at this section's end)

**The question.** [`file-sync.md`](file-sync.md) § 5 rules that a permanently un-appliable change is recorded as a `catchup_failed` conflict row and skipped, and that "skipping is not silent — the recorded row surfaces in the conflict review list". The engine recorded that row only in the device's own `sync_conflicts` table and never reported it, while every app's review list reads the nest's `fauna.sync.conflicts.list` — so the promise held on no app. Two shapes were weighed: the device reports the skip to the nest, or each app folds its own device's local rows into the list through the agent.

**Ruling: the device REPORTS the skip to the nest as an unresolved, candidate-free `catchup_failed` conflict over `fauna.sync.conflicts.report` — no new kind, no new table, no new element ID.** The candidate-free ("mark-only") report is a live wire shape (§ Candidates): the nest stores it with an empty candidate set, `conflicts.list` serves it to every app through the one shared fold, and the review list already renders an unresolved row informationally with no button ([`../ui/folders.md`](../ui/folders.md) § Conflicts). The device-local fold was refused for one decisive reason: a seat with no UI — the per-user agent's relay seat, a headless backup seat — would never surface its skips to anyone, and the list exists so the user learns what a device could NOT do from whichever device they are looking at. The row is per DEVICE, and the existing `device_id` column carries exactly that: on this row it names the device that skipped, not a writer.

**What the report carries, site by site.** The apply-stage refusal (a manifest too new, content that does not address its hash, a path outside the root, sealed content with no key material) knows the plaintext relative path and reports through the ordinary funnel (`report_conflict_ws`): `path` sealed under the engine's label root, `path_hash`, `details` = the content-free reason class (`PermanentApplyFailure::reason`) sealed under the row's own salt, empty `candidates`, no `resolution` — so the report is unsigned, as every unresolved report is (it mints no row to sign). The sealed-path refusal ([`path-sealing.md`](path-sealing.md) § The transient arm is NARROWED) has no plaintext — that IS the failure — so it forwards the change row's own label pair verbatim: `path_sealed` as the nest served it (the nest copies the blob it cannot check), `path_hash` the row's own when it is 32 hex bytes, else the BLAKE3 of its raw string, `path` empty, `details` the refusal reason sealed under that same hash. A record carrying neither plaintext nor a seal (`NoSeal`) cannot be filed anywhere a sealed plane accepts (the S9 flip refuses a sealless report) and stays local-only; no current writer produces one. The report rides the writable-folder gate like every report: a seat without write access keeps its row local (readers bind no location, so none runs catch-up today), and a cross-nest writer's skip stays local until the cross-nest conflict-report relay exists ([`../ui/folders.md`](../ui/folders.md) § Sharing a folder, the named v1 gap).

**Idempotent at both ends.** The engine already records a `catchup_failed` row once per path; the report fires exactly when the local record fires, and the local row gains a `nest_id` (NULL until the nest's reply lands) so an unacknowledged row is re-sent on the next catch-up pass and never forgotten — reporting is best-effort per attempt and guaranteed over time. The nest dedupes: an unresolved `catchup_failed` row already on record for the same (set, device, `path_hash`) is answered with the existing id, never duplicated, so a retry after a lost reply, or a re-judge pass meeting the same change, lands nothing new. Scoped to this type on purpose: it is the one report whose reporter re-sends by design.

**Lifecycle — the row clears when the condition is cured, never by hand.** The row says "this device's copy of this path is behind the set's head", and it is true until this device applies a later change to that path — which is also what [`file-sync.md`](file-sync.md) § 5 promises ("a later change to the same path applies normally"). When such a change applies, the device resolves its local row and sends the candidate-free `conflicts.resolve` for its nest row by the stored `nest_id` (the only resolve a candidate-free conflict admits). Removing the device (`fauna.sync.devices.delete`) resolves its unresolved `catchup_failed` rows in the same nest transaction — a device that no longer exists has no stale copy. Until cured, the row stays: it is a true statement about a stale copy, not noise. There is no dismiss gesture in v1; the candidate-free resolve is on the wire for one if a device the user keeps but ignores ever warrants it. The user's remedy is any new version of the file — an edit from any device, or a restore from version history ([`file-versions.md`](file-versions.md)) — which delivers it again; a device that could not read a manifest too new for it applies that new version once upgraded.

**The verbatim-winner shortcut must not arm on this row (a latent data-loss path, found while ruling).** The engine's apply path takes an incoming change verbatim, skipping conflict detection, whenever the path has ANY unresolved local conflict row (`has_unresolved_conflict_for_path`), because such a row used to mean "the next change on this path is the propagated winner the user chose". A `catchup_failed` row is not that: with one on record, a later remote change overwrote unpublished local edits on that path with no conflict row and no retained loser. The shortcut keys on the conflict kinds a propagated winner exists for and never on `catchup_failed`; a later change on a skipped path goes through ordinary detection and auto-resolve, and clears the skip row only after it lands (the cure above).

**Rendering — one shared fold, all 7 apps, zero new IDs.** `conflict-type-badge` gets a dedicated label, `devices.conflicts.type_catchup_failed` ("Not applied"); the generic `type_other` fallback stays for genuinely unknown types. `conflict-file-info` appends the skipping device's label (the devices snapshot already names it), so the user sees WHICH device is behind. **A `catchup_failed` row whose sealed path does not open is rendered name-less, never omitted:** the shared render drops any row this reader cannot name, which is right for a conflict the user acts on by path and wrong here, where the unreadable name IS the finding — the row renders the placeholder `devices.conflicts.unreadable_path` with its details. The audience is unchanged: this surface already ships to owner/participant readers only. Unresolved → informational, no button, as every unresolved row.

**Compatibility.** Wire-additive on every edge: an older app within the major renders the row with the `type_other` badge and the path; an older nest stores and lists the report like any candidate-free one (without the dedupe — the engine's once-per-path local guard bounds what a lost reply can duplicate). The local `sync_conflicts.nest_id` column is additive at rest.

> **Status (2026-10-02): shared half BUILT.** The engine reports both sites (the apply-stage refusal through `report_conflict_ws`, the sealed-path refusal through `report_sealed_skip_ws`, details sealed by `label_custody::seal_conflict_details_by_hash`), keeps the reply in `sync_conflicts.nest_id` and re-sends an unacknowledged report or cure resolve on every catch-up pass (`flush_skip_reports`); the verbatim-winner apply keys on `CONFLICT_KINDS_WITH_PROPAGATED_WINNER` only; a landed later change cures the row (`cure_skipped_path`); the shared fold renders the badge, the name-less row and the skipping device's label. Still open — until the nest dedupes, a retry after a lost reply can land a second nest row, bounded by the engine's once-per-path guard. The seat witness `tests/e2e-unified/tests/test_filesync_seat_catchup.py` reads the row off the state DB until the tui row lands.

## Implementation status today (2026-07-11)

**A skipped catch-up change reaches the review list — shared half built (2026-10-02).** The engine reports and cures the row and the shared fold renders it; the nest's dedupe and device-delete cascade and the e2e witness flip are still open — § Skipped catch-up changes reach the review list, the status paragraph.

**The review list's gesture is verified (declared a gap 2026-10-01; the listing gap closed 2026-10-02, the gesture built 2026-10-03).** The target model says the losing version is always retained in version history and that *use the other version* re-points to it; the gesture and the choose-winner now sign a candidate only once it is found as a verified version of this file in this set — the rule and its build are owned by [`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md) § Writer-signed change records, ruling (10) and § Implementation status today. The reporter's own losing version is listed again: its reporter signs the retention row (ruling (10)(d), built 2026-10-02), so every judged version history admits it as that reporter's version; a retention row minted before that build stays unsigned and unlisted, its bytes retained and pinned on the nest. The same ruling limited which resolved reports the legacy daemon could sign, until its removal (2026-10-02).

**The merge base is stored per DAEMON, not per config directory.** *(Record: the daemon was removed 2026-10-02; the engine's own roots follow the same construction — see "The ENGINE's two state roots are scoped the same way", below.)* The three-way merge's ancestor, both causal
frontiers and the per-path version ledger lived in the `fauna-sync` daemon's chunk cache, keyed by
`hex(path_hash(relative_path))` alone — **no folder or actor component**. That directory used to
default to `{config_dir}/chunk_cache`, so two daemons whose config files resolved to one directory
overwrote each other's ancestors for every same-named relative path (`notes.txt` in two folders is
one key). `bins/fauna-sync/src/main.rs` documented the rule ("each daemon instance must use its own
chunk_cache_dir") and nothing enforced it.

⚠ **That is a data-loss shape, not a tidiness note**, which is why it is stated here rather than
left to the storage layer. `detect_catchup_conflict` reads the base to decide whether an incoming
change is a genuine concurrent edit, and all three outcomes of a shared directory are wrong: with
the base **absent** it defers to a plain apply, which overwrites the local version with **no
conflict row and no merge**; with a **foreign** base it declares a conflict where there may be none;
and with a foreign base that happens to equal the local bytes it reads "no local change" and
overwrites a genuinely diverged edit silently. Only the middle one is recoverable.

**The fix is a construction, not a rule:** each daemon's cache is a per-`(device, folder)`
subdirectory of the configured root (`ws_client::scoped_cache_dir`), applied to an explicitly
configured root as well as the default. Nothing is carried in from the root: the flat pre-scoping
cache predates the compat-remnant sweep (`../architecture/compat-remnant-sweep.md` § Program
4), so every scope starts empty. Pinned by
`ws_client::tests::the_scoped_cache_dir_is_stable_per_daemon_and_distinct_between_daemons` and
`::the_startup_sequence_carries_and_sweeps_nothing_from_the_root`.

**One implementation of the retention policy serves both hosts.** The daemon's frontier pair and per-path ledger are stored as above, but the *policy* over
that storage — the size guard, the seq dedupe, the prune to the newest `LEDGER_KEEP`, and the three
readers (ancestor-at-or-below, holds-content-at-or-below, newest-seq-matching) — is
`fauna_sync_engine::causal::CausalStore` alone; `SyncWsClient`'s `path_*`/`ledger_*` methods are
one-line delegations over `SyncWsClient::causal()`. Until then the two hosts hand-copied it, and the
constants alone had been converged. **Stated here because the divergence it prevents is invisible:**
two copies of the retention rule means a future tightening can land on one host and not the other,
and the symptom is not a crash but a three-way merge that silently degrades to latest-wins on one
host only. **No at-rest change** — `CausalStore` derives byte-identical filenames over the same flat
directory, which is what keeps the irrecoverable path-keyed entries above out of the question.
Pinned by `ws_client::tests::the_daemon_and_the_engine_are_one_ledger`, which reads each host's
writes through the other across the prune boundary, and by the daemon's own unshared statement of
the on-disk key names (`ws_client::ledger_entry_key`, test-only since the collapse) — so a rename on
the shared side cannot silently orphan a daemon's ancestors.

**The ENGINE's two state roots are scoped the same way (2026-09-20).** The 2026-08-21 construction above scoped the *daemon's* cache and left the in-process engine's own roots — `watch_dir/.fauna-bases` and `watch_dir/.fauna-causal` — flat. Being inside the watch directory made them look per-set; they are not, because **the watch directory is what gets re-bound**. `SyncServiceState`'s `set_location_folder` re-points an already-bound path at a different set in place, the per-set `fsid-<ref>.db` is replaced while the watch directory's dotfiles are not, and nothing anywhere clears them — and the hazard is *concurrent* as well as sequential, since the FFI host dedups persisted bindings by SET, so one path can be bound to two sets at once and two live engines share one root. The causal half is the wrong-answer shape: `seq` is per set, the frontier only grows, and receiver rule 1 skips every incoming `seq ≤ frontier` **byte-free**, so the second set's rows are never applied while the anchor advances past them. The merge-base half is this section's own data-loss shape, already stated above. Both roots now resolve, at engine construction, to a per-`(device, set)` subdirectory — `causal::scoped_store_dir`, over `causal::store_owner_digest`, which the daemon's `ws_client::cache_owner_digest` is now a one-line delegation to (one derivation, or the two hosts' at-rest layouts are free to drift apart). An engine with **no** set has nothing to scope by and keeps the flat root unchanged.

**Nothing is carried in from a flat root (the compat-remnant sweep, 2026-09-25).** Neither host adopts the pre-scoping flat layout any more: the engine opens its scope (`causal::open_scoped_store_dir`) and the daemon its own (`ws_client::prepare_chunk_cache`), both empty on first use, and a flat root's own entries are neither carried nor swept — no installation holding one exists (`../architecture/compat-remnant-sweep.md` § Program 4). The daemon's `.folder-owner` claim inside its scope stays: it is what lets a seat refuse to attribute a directory some other binding reaches (`ws_client::claim_cache_for_folder`, read through `ws_client::read_store_claim`; an unreadable marker is treated as claimed, so the seat serves no chunks). Pinned in `anchor_accounting_test` (sequential re-bind, concurrent double-bind, both roots scoped, a flat-root state is never carried, a nested flat-root base is never carried) and `ws_client::tests::the_startup_sequence_carries_and_sweeps_nothing_from_the_root` + `::the_daemon_and_the_engine_derive_one_scope`.

**"Self-echo" is a per-(actor, device) question, not a per-device one.** Every arm above that distinguishes a peer row
from this seat's own echo — the `apply_remote_changes` partition, the superseding fold,
the fold-licence carrier — resolves it through one predicate,
`fauna_sync_engine::SyncEngine::row_is_own`: the row's device must be ours **and** its
`author_actor_id` must be absent or ours. Device alone is not sufficient, because
`sync_devices` is keyed `(actor_id, device_id)` — a device id is unique only *within*
an actor, so two actors in one shared set may carry the same one. Reading a peer's row
as an echo is silent permanent data loss, not a cosmetic misread: the row applies
nothing and `set_anchor(max_seq)` moves past it regardless, so no later pull re-offers
it. Measured: a writer member never received the owner's file at all. An **absent**
author keeps the device-only answer, because the nest strips authorship on the public
plane (`bins/fauna-nest/src/folder_public.rs`).

> One consequence worth not re-deriving: the conflict-resolution propagation row below
> is inserted with `actor_id` = the **set owner** but `device_id` = the *winning
> candidate's* device (`sync_storage.rs`), so when a **member's** device wins, that
> member no longer self-echo-skips the row — it applies the winning manifest verbatim,
> which is idempotent (those are its own bytes) and additionally clears its local
> conflict row. Same-actor wins, the common case, are unchanged.

The auto-resolve + text-merge
model above is ratified target and **built end-to-end** (slices 1–4, tracked
internally; slices 1–3 landed 2026-07-10, slice 4 2026-07-11):

- **Decision core (slice 1):** `fauna_core::format::ConflictPolicy`
  (`auto`/`latest_wins_always` wire strings) +
  `fauna_sync_engine::conflict_resolver::resolve_conflict` — clean-merge /
  latest-writer-wins / symmetric hash tiebreak, markers never used. Pure, not
  yet consumed by any production path.
- **Nest + wire (slices 2 + 3a):** `folders.conflict_policy` column (default
  `auto`) round-tripping via `fauna.folders.update`/`list`;
  `ConflictReportRequest.{resolution, winning_manifest_hash,
  winning_size_bytes, winning_content_key_version}` — a pre-resolved report
  lands with `resolved_at` set AND the nest **transactionally retains the
  reporter's losing candidate as an ordinary `sync_changes` row and propagates
  the winner as the new head row** (the choose-winner precedent; both versions
  listable + GC-pinned, devices converge via normal catch-up);
  `ConflictsListRequest.include_resolved` (the review-list read);
  `ConflictCandidate.content_key_version` (echoed into the retained rows —
  the choose-winner propagate row now carries it too). All wire-additive; old
  daemons/clients keep the unresolved-report + chooser contract unchanged.
- **In-process engine (slice 3b):** `SyncEngine::download_and_write_file`
  detects divergence pre-write (streaming reassembly goes to a temp promoted by
  rename — a divergent local file is never clobbered pre-resolution),
  auto-resolves per the row's policy, uploads the local version FIRST, sends
  one resolved report, then applies the outcome; markers and conflict-copy
  files are gone from the engine. Fail-closed: if the resolved report cannot
  land, the local file survives untouched and the divergence falls back to the
  unresolved (chooser) flow. Linux threads the row's `conflict_policy` into
  its in-process engines.
- **`fauna-sync` daemon (slice 3c):** both daemon paths (real-time
  `ApplyChange` + reconnect catch-up) route through one
  `SyncWsClient::resolve_divergence` — retention-first local upload, shared
  resolver, one resolved report, per-outcome apply; markers, conflict-copy
  files, and the silent real-time merge are gone. The daemon reads the row's
  `conflict_policy` at session start (runtime state, never a TOML knob).
  Tier_3-proven end-to-end (`tests/platform/sync/test_text_merge.py`):
  concurrent offline edits auto-merge, the conflict lands resolved on the
  review list, both parents stay listable change rows, devices converge.
  **⚠ That proof had a hole the shape of its own fixture, closed 2026-08-01: it
  only ever exercised a device merging content it had RECEIVED.** A device
  could not merge a peer's concurrent edit of a file *it had authored*, because
  a locally-authored version never became a merge base — `handle_local_change`
  kept writing the plaintext `base:<path>` key that
  retired when it hashed every reader's key, so those entries were read by
  nobody and swept at the next startup. With no ancestor, the daemon does not
  merely lose the merge: `detect_catchup_conflict` reads a missing base as "no
  recorded common ancestor → defer to apply", and the peer's version overwrites
  the local one with no conflict row at all. Fixed by recording the base at
  first publication — **if-absent, never on every local write**, since a base
  equal to the local file is how both callers spell "I have not diverged"
  (the shared engine reaches the same rule from the other side: it advances a
  self-authored base only on the nest's echo, and only if the local file still
  matches what it uploaded — `SyncEngine::apply_self_echo`; since 2026-08-02
  the daemon receives and honours its own echoes the same way — see
  *Concurrent resolution & ancestor freshness* above — so first-publication is
  the bootstrap, not the ceiling). Now pinned where a same-shaped fixture cannot hide it:
  `tests/test_filesync_seats.py` leg 4, whose two seats each author one
  side, plus the tier_1 `bins/fauna-sync/tests/merge_base_of_a_local_write.rs`.
- **App review list + policy surfaces (slice 4, landed 2026-07-11).** The
  shared `DevicesMachine` reads conflicts with `include_resolved` and carries
  the resolution fields; its `use_other_version` gesture re-points the file at
  the latest retained non-winning candidate via the ONE shared restore record
  (`SyncClient::restore_version` — the same record Media and the windows shell
  verb use), and `set_folder_conflict_policy` is the per-set
  `folder-conflict-policy-select` edit. **linux, windows, web, apple
  (macOS + iOS — shared `FoldersContent` `ConflictRow`, landed),
  and android (`ConflictCard`, `FoldersScreen.kt`, wired onto the
  review re-point 2026-07-16)** render the
  review list (resolution badge + path → winner + the one-tap
  re-point; unresolved legacy rows are informational) — **no app renders a
  blocking chooser**. The **global default** for new sets lives in
  `default_conflict_policy` field of the account-state plane kind
  `fauna.state.sync-prefs` (the E1 cluster —
  [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
  § The `__config` dissolution schedule → *The kinds*), surfaced as
  `sync-default-conflict-policy-select` in the Folders page's Sync defaults
  section (fan-out across apps owned by `../ui/folders.md` § Conflicts,
  don't restate the per-app list here) and stamped onto creates
  (`FolderCreateRequest.conflict_policy`, wire-additive; the wizard's
  `set_default_conflict_policy` injection). Pinned by the rewritten
  `test_devices_conflicts.py` (review row + re-point round-trip + the
  informational legacy row) and the tier_3 latest-wins fallback e2e
  (`test_latest_wins_fallback_overlapping_and_binary`: overlapping text hunks
  + a binary file both resolve latest-wins, losers retained, never markers).
- **Per-app policy-select fan-out: complete on all 7 apps** (superseded —
  android and web both landed since; per-app dates + wiring detail owned by
  `../ui/folders.md` § Conflicts, don't restate here). The review re-point
  itself is UI-pinned via `test_devices_conflicts.py` (the underlying restore
  record was already tier_3-proven in `conformance_files_versions`).

The candidate/choose-winner **wire protocol stays the substrate**: every app
still reports unresolved conflicts and resolves them via
`fauna.sync.conflicts.resolve`, and the candidate-free resolve beside the
winner-naming one is kept as a wire shape, not as an older-client arm (§
Resolution's 2026-09-24 keep, first grounded on the headless daemon's degraded
report and re-grounded 2026-10-02 when the daemon
left on the shared engine's own degrades,
§ Candidates; the "old clients
within the major version" justification was reworded 2026-10-02 under the
compat-remnant sweep, since no app of this major is older than the shape). The legacy
`conflict-resolution-panel` component and the per-candidate chooser UX are
retired from ui.yaml and every app (2026-07-11).

| Operation | WS-RPC kind | Deleted HTTP twin (≡) |
|---|---|---|
| List unresolved conflicts (with candidates) | `fauna.sync.conflicts.list`    | `GET /api/v1/sync/conflicts` |
| Report a conflict (with candidates)          | `fauna.sync.conflicts.report`  | `POST /api/v1/sync/conflicts` |
| Resolve a conflict (choose winning manifest) | `fauna.sync.conflicts.resolve` | `POST /api/v1/sync/conflicts/{id}/resolve` |

**Candidates.** A seat that detects a conflict on a path where it knows
the diverging manifests — its own local version and the incoming version that
conflicted — reports both as `candidates` (the shape was first built for the
fauna-sync daemon, removed 2026-10-02), each a `{manifest_hash,
device_id, size_bytes, created_at}`. The nest stores them alongside the conflict
row, so `conflicts.list` returns every conflict with its full candidate set; no
join against `file_versions` is needed (candidates are self-contained). A
conflict with an empty candidate set is a candidate-free ("mark-only") conflict
and resolves by `id` alone — a live wire shape, not a pre-candidate-flow leftover.
**Its writer today is the shared sync engine every folder-syncing app embeds**
(`libs/fauna-sync-engine/src/engine.rs`): reporting is best-effort, and the
candidate upload that should precede a report can fail, so two sites degrade to
`report_conflict_ws` with an empty candidate set — `auto_resolve_conflict` when
the local version's upload fails (the conflict is kept local and reported
`concurrent_edit`, unresolved) and `report_declined_delete` when the surviving
version's upload fails (reported `delete_declined`); every other report site
carries its candidates. The deleted headless daemon (removed 2026-10-02) sent the
same degrade and was the writer the 2026-09-24 keep first named; the engine's
sites were verified as the shape's ground the day the daemon
left. The nest's report arm stores the empty set
and its resolve arm takes the `None`-winner path
(`bins/fauna-nest/src/folder_handlers.rs`).

**Resolution.** The client sends the chosen `winning_manifest_hash` (one of the
conflict's candidates) to `conflicts.resolve`. The nest validates the choice
against the recorded candidates, records the winner, marks the conflict
resolved, and **propagates the winner by writing an ordinary change record** — a
`sync_changes` row for `(folder, path, manifest_hash = winner)`. Every device
converges on the winner through the normal catch-up path (`changes.list`'s
`since` cursor, `file-sync.md` § Offline Changes Sync on Reconnect): on seeing that change for a
path it has an unresolved local conflict on, a device applies the winning
manifest and clears its local conflict. No new push channel is introduced —
resolution rides the existing change-record + catch-up machinery. A resolve with
no `winning_manifest_hash` is the candidate-free ("mark-only") path (flips
`resolved_at`, propagates nothing) — the only resolve a candidate-free conflict
admits, and what every app sends for one. **Ruled kept 2026-09-24** under the
compat-remnant sweep (`../architecture/version-compatibility.md` § Dimension 2,
the fourth exception): a current writer produces candidate-free conflicts — the
shared engine's two upload-failure degrades, § Candidates (re-verified
2026-10-02 after the headless daemon, the writer first named, was deleted) — so
`None` is a live reading of an absent value, not an older-client arm; only the
word "legacy" was pre-sweep, and it left the code comments the same day.

> **Implementation status (2026-06-01).** Target: the candidate + choose-winner
> + propagate flow above. **Landed:** the wire types carry it —
> `SyncConflict.candidates`, `ConflictReportRequest.candidates`,
> `ConflictResolveRequest.winning_manifest_hash`, the `ConflictCandidate` record
> (`libs/fauna-protocol/src/folders.rs`), plus the client adapter surface
> `SyncClient::{conflicts_list,conflicts_resolve}` (`libs/fauna-client-sync`) —
> Slice 1 (tracked internally). **Nest (Slice 2):** the
> `sync_conflict_candidates` table + `sync_conflicts.winning_manifest_hash`
> column store candidates; `fauna.sync.conflicts.report` persists them,
> `conflicts.list` returns them, and `conflicts.resolve` validates the chosen
> `winning_manifest_hash` against the recorded candidates (`fauna.sync.bad_candidate`
> otherwise), records the winner, and **propagates it by writing a `sync_changes`
> row** (`change_type = "modify"`, attributed to the winning candidate's device
> so that device self-echo-skips while every other downloads). The legacy
> mark-only path (`winning_manifest_hash == None`) is preserved. **Daemon
> apply half (Slice 3a, 2026-06-01):** the fauna-sync daemon now applies the
> propagated winner on catch-up — when a `changes.list` `modify` lands for a
> path with an unresolved local conflict (`SyncDb::has_unresolved_conflict_for_path`),
> the engine takes the incoming winning manifest verbatim (skipping the 3-way
> merge, which would re-conflict) and clears the local conflict row
> (`SyncDb::resolve_conflict_for_path`) after the write lands
> (the shared `SyncEngine::download_and_write_file`, `libs/fauna-sync-engine` —
> the daemon-local `engine.rs` that first carried this was deleted by the
> Track-C rip). **Daemon report
> half (Slice 3b, 2026-06-01):** the daemon *reports* candidates at detection
> over WS-RPC, building a `[local, incoming]` candidate set — the LOCAL version is
> idempotently chunked + uploaded first (so "keep local" propagates a manifest
> every peer can fetch), the INCOMING version reuses the manifest the conflicting
> change carried — and reports via `fauna.sync.conflicts.report`. The
> **in-process engine** (Linux) reports at `write_conflict_copy` /
> `maybe_merge_http` (`libs/fauna-sync-engine/src/engine.rs`
> `report_conflict_with_candidates`). **Headless daemon, WS path (2026-06-08):**
> the Track-C HTTP rip deleted the daemon's `bins/fauna-sync/src/engine.rs`
> (`run_http_mode`), which had carried the report path, so the headless daemon
> briefly stopped reporting conflicts. It is **restored on the reconnect catch-up
> path**: `SyncWsClient::detect_catchup_conflict` flags a missed change whose
> content *and* the device's own local file both diverged from the cached merge
> base (concurrent offline edits), and `report_catchup_conflict`
> (`bins/fauna-sync/src/main.rs`) uploads the local candidate + reports via
> `fauna_client_sync::SyncClient::conflicts_report`. Reporting is best-effort: a
> failed local upload degrades to a candidate-free (legacy mark-only) report, and
> a failed WS report leaves the conflict recorded in the local `SyncDb`.
> **Superseded by the auto-resolve Slice 4 (2026-07-11):** linux + windows did
> briefly render this candidate chooser, but it — and every other app's
> equivalent (`conflict-resolution-panel`, windows' `ConflictsViewModel`,
> apple's `ConflictListView`) — was retired 2026-07-10/12 in favor of the
> auto-resolved review list (see the implementation status
> above); no app ships a chooser today. Still open at the time: the
> daemon's *real-time* (both-online) forward path keeps its best-effort
> silent merge rather than reporting a conflict — subsumed by the
> auto-resolve target above.

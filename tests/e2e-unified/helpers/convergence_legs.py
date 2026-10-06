"""The convergence legs for an N-seat, one-machine sync run.

One owner for the assertions, one caller since the seat-count fold
(2026-08-03): ``tests/test_filesync_seats.py`` — every mode-uniform seat set at
both ratified counts, over the whole nest-mode axis (tier_3 against a
locally-built ``fauna-nest`` under ``--nest standalone``, elevated to tier_4
against the nest image under s6 with ``--nest docker`` and against the deployed
box with ``--nest live``). Three seats is what turns the pairwise properties
into fan-out ones (see :func:`hello_fanout_plan`).

The legs live *here* rather than in the test module for a specific reason: they
are the *contract* an N-seat convergence claim rests on, kept beside their
tier_1 pins rather than inside a tier_3 module. (The pre-fold reason is
recorded because it still constrains the module: the retired live twins set
``FAUNA_LIVE_NEST_URL`` at import time — one of the two things that engage
conftest's ``_serialize_live_box`` flock — so a tier_3 module importing one
would silently serialize on the shared live box. Under ``--nest live`` the flock
engages from the MODE instead, which no module import can forge; the module
must never reintroduce that module-scope write.)

What differs between the cells is only the environment and the seat count; what
must NOT differ is the assertions. A copy-paste second version would drift, and
a local proxy that has quietly stopped asserting what the live run asserts is
worse than no proxy at all — it reports confidence it no longer earns
(priorities #1/#4).

Two seats or three
------------------
Every leg below is written over ``seats`` as a whole, never over a hardcoded
``a, b`` pair, and the *plans* it runs are pure functions pinned by tier_1 tests
(:func:`hello_fanout_plan`, :func:`peers`). That is deliberate: the cheapest way
to break a three-seat run is to reuse a pairwise leg body and quietly assert two
pairwise facts where the fan-out fact was wanted — a → b and b → c both green
while a → c was never checked at all. Enumerating the peers from the seat list
makes that unrepresentable, and the tier_1 gates pin the enumeration itself.

With two seats the plans reduce to exactly the legs this module has always run,
byte for byte.
"""

from __future__ import annotations

import time

from helpers import sync_seats
from helpers.waiting import wait_until

# The run-file protocol (paths, byte-exact bodies, the temp+rename write) has
# ONE owner and is imported, never re-declared, so a mixed run stays
# byte-compatible.
import tests.test_filesync_multiseat_live as ms


# ── the leg plans (pure; tier_1 pins them) ───────────────────────────────────


def peers(names, of):
    """Every seat except ``of``, in order.

    A one-liner with a name, because it is the single place the fan-out is
    decided: a leg that reaches for ``seats[1]`` instead of calling this is how
    a three-seat assertion silently narrows back to a pairwise one.
    """
    return [n for n in names if n != of]


def hello_fanout_plan(names):
    """``[(writer, [observers…]), …]`` — every seat writes, every OTHER reads.

    The property two seats cannot express. With ``("a", "b")`` this is exactly
    the historical legs 1 and 2 (a → b, then b → a). With ``("a", "b", "c")`` it
    is six observations rather than two, and crucially it includes ``a → c``:
    a chain that only ever checked neighbours would pass while one seat received
    from nobody.
    """
    return [(w, peers(names, w)) for w in names]


def await_count(n: int) -> int:
    """How many peer observations an ``n``-seat run performs.

    Exposed so each test module can DERIVE its ``pytest.mark.timeout`` ceiling
    from the plan instead of hardcoding a leg count that silently stops matching
    it (convention 9 — bounded always, and bounded by something true).

    Fan-out ``n(n-1)`` + merge base ``n-1`` + the concurrent merge ``n`` +
    the same-anchor base ``n-1`` + the same-anchor fold ``n`` + delete
    propagation ``n-1``.

    Leg 4a's share is counted even while :data:`SAME_ANCHOR_FOLD_GAP_OPEN`
    holds it: this is a CEILING, a held leg simply never spends it, and a count
    that tracked the flag would have to be re-derived — silently under-bounding
    every module — on the day the leg is armed.
    """
    return n * (n - 1) + (n - 1) + n + (n - 1) + n + (n - 1)


# ⚠ Leg 4a is HELD — the same-anchor gap's REPORT-PLANE remainder is open.
#
# Two measured runs, two distinct defects:
# - First run (2026-08-04, pre-ruling): every seat duplicated appends AND the
#   seats DISAGREED — clause 5's third gap, RULED CLOSED the same day (the
#   gap-3 decision record in `conflicts.md` § Concurrent resolution: novelty
#   is a property of bytes, not rows; five conjuncts, built in both hosts;
#   tier_1 pin flipped to `merge_convergence_test.rs::
#   same_anchor_appends_converge_even_when_a_reissue_enters_the_schedule`).
# - Second run (same day, armed against the built ruling): the seats now
#   AGREE — the divergence half is fixed live — but they converged onto a
#   body with two appends DUPLICATED. Root cause per the goal doc's gap
#   block: the resolved-report plane is ABSENT from the tier_1 model (which
#   is why the fuzzer passed), and its loser-retention rows — stale
#   edit-stamped carriers of derivative pre-merge candidates, forwarded
#   per-row on the real-time rail — are the remaining duplication engine.
# - 2026-08-05: the model grew the report plane, reproduced the duplication
#   deterministically, and the LOSER-ROW RULING closed it MODEL-SIDE
#   (`conflicts.md` § Concurrent resolution's decision record: the additive
#   `is_retention` marker, accounted-never-applied; the BASE witness on the
#   writer stamp; tier_1 pin
#   `report_plane_retention_rows_are_accounted_and_never_applied`). **The
#   armed runs then RED'd LEG 4 — the unique-anchor leg** — two runs, two
#   different lost lines. The model's own-novelty machinery then went
#   production-shaped, reproduced the loss in the fuzzer's first pass (five
#   plain deliveries), and the LEG-4 RULING closed it in both hosts
#   (`conflicts.md` § Concurrent resolution's ⚠ decision record, 2026-08-05):
#   a verbatim adopt is never taken while own pending rows are unlisted
#   (defer-as-cap, transient-class — never the engine's old
#   consume-and-drop); the winner claim is honest under the lower-bound law
#   (the shipped unconditional incoming_seq stamp was the covering lie); and
#   the fold's licence gains carrier-never-deferred. Tier_1 pin:
#   `leg4_unique_anchor_plain_deliveries_converge`.
# - 2026-08-05, the ruling's armed run: **LEG 4 GREEN — the leg-4 ruling is
#   LIVE-VERIFIED** — and leg 4a advanced from duplication to a NEW, narrower
#   red: all three seats AGREE on a body missing exactly one seat's append
#   (b's), the empty no-churn schedule the tier_1 model asserts exactly-once
#   for. Prime suspect (from the seat logs): the covering winner was adopted
#   over pending-append local content WITH NO DEFER LINE at seat c — the
#   real-time apply rail judging seq-less (`local_matches_last_sent`
#   degrade), bypassing the causal machinery; the defer DID fire on the
#   catch-up rail (seat a, seq=14). Owned upstream; the
#   ruling build stays on its branch until this leg is green.
#
# The held and armed branches are the same fully-asserting code (arming is
# this one constant). ARMED 2026-08-05: row 8's same-anchor ruling (union
# merges, honest edit-class winners, daemon parity, per-carrier pending
# windows) is model-closed and built in both hosts + the nest — this leg is
# the production acceptance gate it just passed.
SAME_ANCHOR_FOLD_GAP_OPEN = False


# ── leg 4a's file protocol (leg-local; the I/O primitives stay ms's) ─────────
#
# The hello and shared files belong to the run-file protocol `ms` owns, because
# the tri-machine round writes those same bytes on three machines and a
# re-declaration here would let a mixed run drift. The anchor file is different
# in kind: it is written ONLY by this module, by seats in one process, so there
# is nothing to stay byte-compatible with — and putting it in the tri-machine
# module would grow that module with a leg it never runs. What is NOT
# re-declared is the I/O: `ms._write`'s temp+rename and `ms._read`'s tolerance
# of a mid-write body are the shared primitives, and this leg calls them.


def anchor_path(folder, run_token: str):
    """This run's same-anchor file in ``folder``.

    Inside the ``<token>-*`` namespace by construction — leg 5's cleanup globs
    exactly that prefix, and so does the live box's residue reaping
    (`sync_seats.finalize_live_residue`). A name outside it would leak onto the
    shared set permanently.
    """
    return folder / f"{run_token}-anchor.txt"


def anchor_base(run_token: str) -> str:
    """The common ancestor every seat appends to — ONE line, so every seat's
    hunk necessarily shares the end-of-file anchor.

    Deliberately not `ms._shared`'s shape: that one separates each seat's line
    with unique spacer lines *so that* pairwise hunks never overlap. Here the
    overlap is the subject.
    """
    return f"same-anchor file, run {run_token}\n"


def anchor_marker(seat_name: str, run_token: str) -> str:
    """The line ``seat_name`` appends — distinct per seat (else a lost append
    is unobservable) and token-bearing (else the residue sweep misses it)."""
    return f"{seat_name}: appended in run {run_token}"


def anchor_edit(seat_name: str, run_token: str) -> str:
    """``seat_name``'s whole file after its append: the base verbatim plus its
    own line. The base is a strict prefix — an edit that rewrote it would be a
    different-anchor edit wearing this leg's name."""
    return anchor_base(run_token) + anchor_marker(seat_name, run_token) + "\n"


def _fold_diagnosis(bodies, markers, note) -> str:
    """Why the fold never converged, in the vocabulary of the ruling it pins.

    Four distinct findings, deliberately not interchangeable: a seat the file
    never reached, a permanent permutation DISAGREEMENT (finding B's exact
    shape), a DUPLICATED append (the runaway signature), and a LOST append.
    """
    lines = []
    absent = sorted(name for name, body in bodies.items() if body is None)
    if absent:
        lines.append(
            f"  the anchor file never reached: {', '.join(absent)} — that is a "
            f"delivery failure, not a fold failure; leg 3 already proved the "
            f"shared file reaches every seat, so suspect this leg's own base "
            f"write before the merge rules."
        )
    present = {name: body for name, body in bodies.items() if body is not None}
    if len(set(present.values())) > 1:
        lines.append(
            f"  the seats DISAGREE — {len(set(present.values()))} distinct "
            f"bodies across {len(present)} seats, every one of them a "
            f"permutation of the same appends. This is FINDING B's exact "
            f"shape: each seat folds its peers' rows into its own order, "
            f"publishes it as a resolution, and every peer then skips it as "
            f"stale — a permanent disagreement with nothing lost or "
            f"duplicated. Convergence, not winner-permanence, is the invariant "
            f"(`conflicts.md` § Concurrent resolution, clause 2). Suspect the "
            f"covering-resolution adopt arm (clause 5, receiver rule 3) and "
            f"its author-blind conjunct first."
        )
    for name, body in sorted(present.items()):
        for owner, marker in sorted(markers.items()):
            count = body.count(marker)
            if count > 1:
                lines.append(
                    f"  {name} DUPLICATED {owner}'s append ({count}×) — the "
                    f"runaway signature: re-merging already-incorporated "
                    f"content from a stale ancestor, whose duplicates then "
                    f"feed the next round. Suspect the content rung (receiver "
                    f"rule 5) and the idempotence guard (rule 1)."
                )
            elif count == 0:
                lines.append(
                    f"  {name} LOST {owner}'s append. Every seat wrote once "
                    f"and this leg induces no re-delivery churn, so this is "
                    f"the empty schedule the tier_1 model asserts "
                    f"exactly-once for "
                    f"(`merge_convergence_test.rs::"
                    f"concurrent_same_anchor_appends_converge_at_three_and_"
                    f"four_seats`) — a finding, not the sanctioned "
                    f"latest-wins degrade."
                )
    for name in sorted(bodies):
        lines.append(f"  {name} holds {bodies[name]!r}")
    return "the same-anchor fold never converged:\n" + "\n".join(lines) + f"\n{note()}"


def await_same_anchor_fold(
    seats, run_token: str, budget: float, note, *, interval: float = 1.0
) -> str:
    """Poll until every seat holds ONE body carrying every append exactly once.

    Three conjuncts, and each catches something the other two pass:

    * **agreement** — all seats byte-identical. Alone, satisfiable by a body
      that dropped a side.
    * **every marker present** — no append lost. Alone, satisfiable by three
      seats holding three different permutations (finding B).
    * **every marker exactly once** — no duplication, the runaway signature.
      Alone, satisfiable by agreement on an empty file.

    Returns the converged body, so the caller can log which permutation the
    log-tail resolution actually produced — the thing no assertion may predict.

    **Latency-independent (convention 14).** The permutation is decided by the
    nest log's total order, so there is nothing to wait a fixed time for and
    nothing to predict; the poll returns the instant the state holds and a
    green run pays none of ``budget``. Polling a state that may still be
    mid-fold is sound because every state satisfying all three conjuncts IS a
    converged state: a later supersession can only move every seat to another
    body that satisfies them too.
    """
    markers = {seat.name: anchor_marker(seat.name, run_token) for seat in seats}
    seen: dict = {}

    def _observe():
        bodies = {
            seat.name: ms._read(anchor_path(seat.path, run_token)) for seat in seats
        }
        seen.clear()
        seen.update(bodies)
        if any(body is None for body in bodies.values()):
            return None
        distinct = set(bodies.values())
        if len(distinct) != 1:
            return None
        body = next(iter(distinct))
        if any(body.count(marker) != 1 for marker in markers.values()):
            return None
        return body

    return wait_until(
        _observe,
        budget,
        interval=interval,
        diagnose=lambda: _fold_diagnosis(seen, markers, note),
    )


# ── shared waiters ───────────────────────────────────────────────────────────


def await_own_base_line(seat, run_token: str, budget: float, note) -> str:
    """This seat's copy of the shared file, once it presents this seat's own line.

    POLL the read rather than reading once: a peer's concurrent apply can
    delete→recreate the file, so a single read races a transient absence or a
    half-written body (the discipline
    the retired engine-only tri-machine round's phase 3 recorded).
    """
    path = ms._shared_path(seat.path, run_id=run_token)
    marker = f"{seat.name}: base"
    deadline = time.monotonic() + budget
    current = ms._read(path)
    while (current is None or marker not in current) and time.monotonic() < deadline:
        time.sleep(0.5)
        current = ms._read(path)
    assert current is not None and marker in current, (
        f"[{seat.name}] the shared file never presented this seat's base line "
        f"within {budget:.0f}s (last read {current!r}) — either a peer's apply "
        f"keeps it absent/half-written here, or the merge base never synced to "
        f"this seat.\n{note()}"
    )
    return current


def prime_delete_witness(watchers, basenames) -> None:
    """Record the PRESENT half of every watcher's delete witness — call this
    BEFORE the deleter unlinks anything.

    An app seat's :meth:`~helpers.sync_seats.AppSeatDriver.observed_delete` is a
    witnessed present→absent transition, and the presence is recorded only when
    that seat is actually asked. The cleanup leg awaits its watchers
    sequentially, so the second watcher's polling window opens after the first
    watcher's whole await — by which time it has usually already applied the
    deletes, its folder is empty, and the transition is unwitnessable: a false
    RED on a healthy product, hit on ``[3seat-tui+tui+tui]``'s first-ever run
    (2026-08-03; a single watcher's window opens at unlink time, which is why
    the two-seat cells never showed it).

    Priming closes it as a causal barrier rather than a poll-cadence bet
    (convention 14): presence is recorded at a moment the legs have already
    proven every file is on every seat, so a window that opens arbitrarily late
    still proves the transition. Fail-toward-RED is preserved — a file absent
    at priming time was never here, is not recorded, and its delete can never
    be reported as applied. On an engine seat the call is a harmless no-op:
    its witness is the daemon's own ``applied remote delete`` log line, which
    no polling window can miss.
    """
    for watcher in watchers:
        for name in basenames:
            # The return value is deliberately ignored: pre-delete it can only
            # be False (present = held, not deleted); the call's job is the
            # recording side effect its docstring guarantees.
            watcher.observed_delete(name)


def await_applied_deletes(
    seat, basenames, budget: float, note, *, interval: float = 1.0
) -> None:
    """Poll until ``seat`` has APPLIED the deleter's removal of every basename.

    A positive observation, deliberately — the causal barrier the absence
    assertion that follows hangs off (convention 14). Absence alone would be
    satisfiable by a file that never arrived in the first place.

    The barrier anchors on the OBSERVING seat's applied delete rather than on the
    deleter's own notify because the deleter's notify logged no success line
    when this leg was written (the since-removed legacy daemon only logged
    "WS delete notify failed").
    That makes this the stronger observable anyway: the tombstone provably
    reached the nest and came back out again.

    Built on the shared ``wait_until`` rather than a hand-rolled loop: the
    predicate retires each name as it is observed, so ``pending`` holds exactly
    what is genuinely outstanding when ``diagnose`` runs — a message that never
    names a file it did not examine. ``interval`` is a poll cadence, not a
    timing assumption; a green run returns on the first poll and pays none of
    the budget.
    """
    pending = list(basenames)

    def _observe() -> bool:
        for name in list(pending):
            if seat.observed_delete(name):
                pending.remove(name)
                print(
                    f"[seats] {seat.name}: applied the peer's delete of {name}",
                    flush=True,
                )
        return not pending

    wait_until(
        _observe,
        budget,
        interval=interval,
        diagnose=lambda: (
            f"[{seat.name}] never applied the peer's delete of "
            f"{', '.join(sorted(pending))}. Note this leg is NEW coverage (this "
            f"was the FIRST delete phase anywhere in the suite; the tri-machine "
            f"round grew its own — phase 5 — only on 2026-08-06), so a red here "
            f"is as likely a genuine delete-propagation defect as a harness "
            f"problem. "
            f"Check whether the deleting seat notified at all before concluding "
            f"either.\n{note()}"
        ),
    )


# Leg 4b's own budget, deliberately far smaller than the convergence `window`.
# By the time it runs, leg 4 has already observed the merged bytes on EVERY
# seat — and those bytes only reach a peer through the nest, so the resolved
# report has necessarily already landed. The only thing left to wait on is the
# page re-reading its snapshot, which is a UI round-trip, not a sync round-trip.
# Each poll costs two real navigations, so the interval is seconds, not
# milliseconds; a green run returns on the first poll and pays neither.
REVIEW_BUDGET_S = 30.0
REVIEW_POLL_S = 3.0


def assert_review_list_agrees(
    seats, run_token: str, budget: float, note, *, interval: float = REVIEW_POLL_S
) -> None:
    """Every review row this run produced reads as AUTO-RESOLVED, on a UI seat.

    Why this is worth a leg of its own. The engine's auto-resolve is
    **fail-closed**: if the local version's upload or the resolved report fails,
    `auto_resolve_conflict` records a local unresolved row, sends a legacy
    unresolved report, and keeps the local file — and the seats still converge
    on identical bytes by the next pass. So leg 4's byte assertion passes
    *either way*. The only place a degraded resolve is visible is the review
    row's badge, and it is visible there to the USER — which is exactly the
    class of bug worth an e2e assertion.

    This is genuinely new coverage rather than a second copy of
    ``test_devices_conflicts.py``: that test SEEDS conflicts through
    ``fauna.sync.conflicts.report`` and proves the page renders what the nest
    serves. Nothing until now drove a real multi-device divergence and then read
    the list — the seam between "the daemon resolved it" and "the user sees a
    resolved row" was untested end to end.

    **Latency-independent (convention 14), deliberately.** It does NOT assert
    that a row must exist. Whether concurrent edits collide into a *conflict* or
    land as one clean sequential update depends on which write reaches a peer
    first — real behaviour, not a defect, and the merged bytes are correct under
    both. Demanding a row would be a wall-clock bet, i.e. a defunct test by this
    project's own rule. So the shape is: poll only for a POSITIVE observation
    (returning the instant a row appears, paying nothing on a green run), then
    assert a property of whatever rows are there. A run that produced no
    conflict says so out loud rather than reporting silent green.
    """
    readable = [(seat, seat.review_rows()) for seat in seats]
    ui_seats = [(seat, rows) for seat, rows in readable if rows is not None]
    if not ui_seats:
        # Declared structural absence, stated in the log rather than skipped:
        # a disk-only run has no Folders page on any seat.
        print(
            "[seats] review list: N/A — no seat has a UI to read it "
            "from (disk-only run). The conflict is still detected, resolved "
            "and reported by every engine; only the READ is absent here.",
            flush=True,
        )
        return

    started = time.monotonic()
    deadline = started + budget
    while True:
        found = [
            (seat, rows, sync_seats.review_rows_for_run(rows, run_token))
            for seat, rows in ui_seats
        ]
        if any(mine for _seat, _rows, mine in found):
            break
        if time.monotonic() >= deadline:
            break
        time.sleep(interval)
        ui_seats = [(seat, seat.review_rows()) for seat, _rows in ui_seats]

    # The ACTUAL wait, not the budget: the loop exits as soon as ANY seat has a
    # row, so a seat with none may have been polled once. Reporting the budget
    # here would claim a wait that never happened — the same "a diagnostic must
    # not state evidence it does not have" rule the ANSI-log incident taught.
    waited = time.monotonic() - started

    for seat, rows, mine in found:
        if not mine:
            print(
                f"[seats] {seat.name}: review list holds no row for "
                f"{run_token} after {waited:.0f}s — the concurrent edits did not "
                f"collide into a conflict on this seat (they converged as a "
                f"clean sequential update). Nothing is asserted about "
                f"resolution here; the merge itself was already proven by leg "
                f"4. Rows visible on the page: {len(rows)}.",
                flush=True,
            )
            continue
        unresolved = sync_seats.unresolved_review_rows(rows, run_token)
        assert not unresolved, (
            f"[{seat.name}] this run's concurrent edit produced "
            f"{len(unresolved)} review row(s) that did NOT auto-resolve: "
            f"{unresolved!r}.\nThe bytes converged (leg 4 passed), so this is "
            f"the fail-closed degrade in `auto_resolve_conflict` — the local "
            f"version's upload or the resolved report failed and it fell back "
            f"to a legacy unresolved report. The user sees an unresolved "
            f"conflict for a file the system actually merged cleanly. Expected "
            f"one of {sorted(sync_seats.RESOLVED_BADGES)} in the badge.\n"
            f"{note()}"
        )
        print(
            f"[seats] {seat.name}: review list agrees — "
            f"{len(mine)} row(s) for {run_token}, all auto-resolved: "
            f"{[badge for badge, _info in mine]}",
            flush=True,
        )


def run_convergence_legs(
    seats, run_token: str, *, window: float, label: str
) -> None:
    """The convergence legs against N already-ready seats (N ≥ 2).

    (1) the hello **fan-out** — every seat writes its own file and every OTHER
    seat observes it; (3) the merge base from the first seat reaches all the
    rest; (4) every seat edits its own line of the shared file concurrently and
    all of them converge on the identical merged bytes; (4a) every seat appends
    a distinct line at the SAME anchor of a second shared file and all of them
    converge on one permutation holding every append exactly once; (4b) any
    review row those merges produced reads as auto-resolved on a UI seat; (5)
    the first seat deletes everything this run created and every other seat is
    watched applying the deletions.

    (Leg numbering is historical and kept stable across seat counts: with two
    seats the fan-out is the familiar a → b and b → a.)

    There is no own-ack barrier as a pass gate. Seats with their own dbs, device
    ids and watch dirs share nothing local, so the ONLY channel between the
    folders is the nest: an observation on a peer IS proof that the writer's
    upload reached it. Each seat's own upload log survives as failure
    *diagnostics*, which is where it belongs — it names the broken direction
    rather than gating the pass.
    """
    seats = list(seats)
    assert len(seats) >= 2, (
        f"a convergence run needs at least two seats to have anything to "
        f"converge; got {len(seats)}"
    )
    by_name = {seat.name: seat for seat in seats}
    names = tuple(by_name)
    assert len(names) == len(seats), f"seat names must be unique, got {names}"

    def _note() -> str:
        return "\n".join(s.diagnostics() for s in seats)

    def _await(observer, expect, phase, budget=None) -> None:
        ms._await_files(
            observer.name,
            None,  # no client on an engine seat; the hooks below are better
            expect,
            phase,
            budget if budget is not None else window,
            self_note=lambda _folder: observer.self_note(),
            extra_note=_note,
        )

    # ── legs 1..n: the hello fan-out ─────────────────────────────────────────
    # Every seat writes one file; every OTHER seat must see it. With two seats
    # this is the historical a → b / b → a pair; with three it is the property
    # those two legs cannot express, because a chain of neighbours would leave
    # a → c unchecked.
    for writer_name, observer_names in hello_fanout_plan(names):
        writer = by_name[writer_name]
        body = ms._hello(writer.name, run_id=run_token)
        ms._write(ms._hello_path(writer.path, writer.name, run_id=run_token), body)
        print(
            f"[seats] {writer.name} -> {', '.join(observer_names)}: wrote hello, "
            f"awaiting it on every peer",
            flush=True,
        )
        for observer_name in observer_names:
            observer = by_name[observer_name]
            _await(
                observer,
                {ms._hello_path(observer.path, writer.name, run_id=run_token): body},
                f"leg {writer.name} -> {observer.name}",
            )

    # ── leg 3: the merge base reaches every peer ─────────────────────────────
    creator = seats[0]
    base = ms._shared({s: "base" for s in names}, run_id=run_token, seats=names)
    ms._write(ms._shared_path(creator.path, run_id=run_token), base)
    print(
        f"[seats] {creator.name}: wrote the merge base, awaiting it on "
        f"{', '.join(peers(names, creator.name))}",
        flush=True,
    )
    for observer_name in peers(names, creator.name):
        observer = by_name[observer_name]
        _await(
            observer,
            {ms._shared_path(observer.path, run_id=run_token): base},
            "leg 3 (merge base)",
        )

    # ── leg 4: concurrent edit of one shared file, by EVERY seat ─────────────
    # No barrier file: the process itself sequences the overlap. Read every
    # copy, then issue all the writes back-to-back — and note that convergence
    # does not actually depend on the overlap being perfect. If one edit lands on
    # a peer first, the other seats' writes are simply based on a newer version
    # and the three-way merge from the common base reaches the same bytes.
    #
    # With three seats this is the genuinely new merge property: a seat must fold
    # in TWO remote edits, not one, and every seat must land on the same bytes.
    edits = []
    for seat in seats:
        current = await_own_base_line(seat, run_token, window, _note)
        edits.append(
            (
                seat,
                current.replace(
                    f"{seat.name}: base",
                    f"{seat.name}: edited in run {run_token}",
                ),
            )
        )
    for seat, body in edits:
        ms._write(ms._shared_path(seat.path, run_id=run_token), body)
    print(
        f"[seats] all {len(seats)} seats edited their own line, awaiting the merge",
        flush=True,
    )

    final = ms._shared(
        {s: f"edited in run {run_token}" for s in names},
        run_id=run_token,
        seats=names,
    )
    for seat in seats:
        _await(
            seat,
            {ms._shared_path(seat.path, run_id=run_token): final},
            f"leg 4 (concurrent {len(seats)}-seat merge, observed on {seat.name})",
        )

    # ── leg 4a: the same-anchor append fold ──────────────────────────────────
    # Leg 4 cannot express this shape, by its own construction: `ms._shared`
    # separates each seat's line with unique spacers SO THAT every pairwise
    # hunk merges cleanly, which is what lets it assert byte-exact expected
    # output. Here every seat's hunk shares one anchor (end of a one-line
    # base), so each seat's fold of its peers' rows is ORDER-DEPENDENT and
    # yields its own permutation. That is the shape `conflicts.md` clause 5's
    # 2026-08-03 gap ruling was made for — before it, the seats published their
    # permutations as resolutions and every peer skipped them as stale, so they
    # disagreed permanently with nothing lost and nothing duplicated (finding
    # B). The ruling adopts a COVERING resolution verbatim and lets the nest
    # log's total order arbitrate, so all seats land on the log-tail
    # permutation.
    #
    # Which permutation that is, is not predictable and is not asserted — the
    # leg asserts the three properties the ruling guarantees (agreement, every
    # append present, none duplicated). Runs at every seat count: the defect
    # was N ≥ 3, and the tier_1 model pins 2..=4, so the pair cell is the
    # cheap guard that the rules did not regress for two.
    # (`creator` is leg 3's — the same seat authors both bases, so a red here
    # is never "a different seat could not create a file".)
    if SAME_ANCHOR_FOLD_GAP_OPEN:
        # Declared out loud on every run, never a silent skip: what is held,
        # what holds it, and how to arm it. (The flag's history and the
        # re-hold discipline — a ruled defect, a pinned counterexample, a
        # printed pointer — live on its definition; the 2026-08-04 hold was
        # armed when clause 5's third gap was ruled closed the same day.)
        print(
            "[seats] leg 4a (same-anchor append fold): HELD on an open ruled "
            "defect — see convergence_legs.SAME_ANCHOR_FOLD_GAP_OPEN for the "
            "defect, its tier_1 pin, and the arming instruction "
            "(`convergence_legs.SAME_ANCHOR_FOLD_GAP_OPEN = False`).",
            flush=True,
        )
    else:
        _run_same_anchor_fold_leg(
            seats, by_name, names, creator, run_token, window, _await, _note
        )

    # ── leg 4b: the review list agrees with what just converged ──────────────
    # Deliberately BEFORE the cleanup: with no delete issued yet, a
    # `delete_declined` row cannot exist, so any non-resolved badge here is
    # unambiguously the auto-resolve degrade rather than delete-vs-edit noise.
    assert_review_list_agrees(seats, run_token, REVIEW_BUDGET_S, _note)

    # ── leg 5: the assertive cleanup, which is also new coverage ─────────────
    # Here one process sequences the DELETE strictly after convergence, so the
    # tombstone-before-write hazard cannot arise — and on the live box the
    # non-destructive carve-out requires the leg regardless. The witness has its
    # own race though (a fast apply beats a late-opening poll window), which is
    # what the priming below closes. (The tri-machine round was delete-less for
    # exactly that hazard until 2026-08-06, when its phase 5 bought the same
    # ordering across machines with an explicit barrier — no single process to
    # sequence it there; `test_filesync_multiseat_live.py::_run_delete_phase`.)
    #
    # Every peer is watched applying every deletion. With three seats that is
    # the second fan-out property: a tombstone that reached one peer and not the
    # other is a real defect the two-seat shape cannot see.
    deleter = seats[0]
    watchers = [by_name[n] for n in peers(names, deleter.name)]
    created = sorted(p.name for p in deleter.path.glob(f"{run_token}-*"))
    assert created, (
        f"[{deleter.name}] nothing matching {run_token}-* is in this seat's "
        f"folder at cleanup time, so the run cannot verify it removed what it "
        f"created.\n{_note()}"
    )
    # BEFORE the unlink: record the present half of every watcher's witness,
    # so the sequential awaits below stay sound however late each window opens.
    prime_delete_witness(watchers, created)
    for name in created:
        (deleter.path / name).unlink()
    print(
        f"[seats] {deleter.name}: deleted {', '.join(created)}, awaiting "
        f"{', '.join(w.name for w in watchers)} to apply the deletions",
        flush=True,
    )
    for watcher in watchers:
        await_applied_deletes(watcher, created, window, _note)
    # Only now is absence meaningful — the applied-delete observations above are
    # the causal barrier that makes this negative assert sound.
    for watcher in watchers:
        still_there = sorted(n for n in created if (watcher.path / n).exists())
        assert not still_there, (
            f"[{watcher.name}] applied the peer's deletes but still holds "
            f"{', '.join(still_there)} on disk — the tombstone was processed "
            f"without the file being removed.\n{_note()}"
        )
    print(
        f"[seats] {len(seats)} seats converged and cleaned up "
        f"({label}, token {run_token}) — PASS",
        flush=True,
    )


def _run_same_anchor_fold_leg(
    seats, by_name, names, creator, run_token: str, window: float, _await, _note
) -> None:
    """Leg 4a's body — see :data:`SAME_ANCHOR_FOLD_GAP_OPEN` for why it is a
    function rather than inline: held and armed must be the SAME code, so that
    arming is a constant flip and can never mean "rewrite the leg"."""
    ms._write(anchor_path(creator.path, run_token), anchor_base(run_token))
    print(
        f"[seats] {creator.name}: wrote the same-anchor base, awaiting it on "
        f"{', '.join(peers(names, creator.name))}",
        flush=True,
    )
    for observer_name in peers(names, creator.name):
        observer = by_name[observer_name]
        _await(
            observer,
            {anchor_path(observer.path, run_token): anchor_base(run_token)},
            "leg 4a (same-anchor base)",
        )
    # Every seat now provably holds the base as its ancestor, so the appends
    # below are concurrent edits from ONE common ancestor at ONE anchor — the
    # tier_1 model's `EditShape::Append` seed, in production shape.
    for seat in seats:
        ms._write(
            anchor_path(seat.path, run_token), anchor_edit(seat.name, run_token)
        )
    print(
        f"[seats] all {len(seats)} seats appended at the same anchor, awaiting "
        f"the fold",
        flush=True,
    )
    folded = await_same_anchor_fold(seats, run_token, window, _note)
    print(
        f"[seats] the same-anchor fold converged on every seat: "
        f"{folded.splitlines()[1:]!r} (the log-tail permutation — which one it "
        f"is is arbitrated by the nest log and is deliberately not asserted)",
        flush=True,
    )


__all__ = [
    "REVIEW_BUDGET_S",
    "REVIEW_POLL_S",
    "anchor_base",
    "anchor_edit",
    "anchor_marker",
    "anchor_path",
    "assert_review_list_agrees",
    "await_applied_deletes",
    "await_count",
    "await_own_base_line",
    "await_same_anchor_fold",
    "hello_fanout_plan",
    "peers",
    "prime_delete_witness",
    "run_convergence_legs",
]

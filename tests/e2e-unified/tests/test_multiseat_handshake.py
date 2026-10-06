"""tier_1 gates for the multiseat HANDSHAKE mechanics — the two properties that
decide whether an operator-coordinated round survives a loaded machine.

Both exist because of what the 2026-07-29 FIFO ticket queue did *not* fix. That
queue (`build-system.md` § *Slot fairness*) ended indefinite `e2e`-slot
starvation — the 780 s and 1140 s deaths that killed run `20260729-02` — by
granting slots in arrival order. But it bounds a waiter's **position**, never its
**duration**: width 2 means "you can be passed at most once", and an `--app`
sweep legitimately holds a slot 15+ minutes. Two consequences the queue cannot
reach, one per section below:

  1. A round entered the queue TWICE — announce, operator gate, then the seat as
     a second invocation queueing from the back. The gap straddles human latency,
     and the slot must not be held across it (that idle-holds 1 of 2 machine-wide
     slots and starves siblings — considered and rejected). So the fix is to make
     ONE invocation able to announce *and* seat, which needs the seat's run id to
     be resolvable at RUNTIME rather than frozen at module import.
  2. The three machines' waits are independent and UNEQUAL. Phase 1 is the only
     asymmetric wait — `CREATOR = SEATS[0]` lays the merge base and every peer's
     phase 1 is a pure wait on that output — so a peer that won its slot instantly
     burns its whole window while the creator is still queued, and fails a
     perfectly healthy cohort. Phase 1 therefore needs a budget that absorbs
     another machine's queue + cold build, while phases 2-4 keep the tight
     symmetric window they need for edits to genuinely overlap.

Both are convention-14 shaped (testing.md point 14): named generous budgets and
deadline polls, never wall-clock timing assertions. A green run pays nothing for
either — the polls return the instant the state is observed.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import tests.test_filesync_multiseat_live as ms

pytestmark = [pytest.mark.tier_1]


# ── 1. one invocation, one slot: the announce hands the id to the seat ─────────


def test_announce_publishes_run_id_for_a_seat_in_the_same_session(monkeypatch):
    """The seat must be able to read an id the announce computed IN-PROCESS.

    This is the whole enabling condition for one-invocation/one-slot rounds. It
    is a real behavioural claim, not a symbol check: after publishing, every
    run-file path helper must carry the new id, because those helpers are what
    the rendezvous actually compares.
    """
    monkeypatch.setattr(ms, "RUN_ID", "")
    monkeypatch.delenv("FAUNA_MULTISEAT_RUN_ID", raising=False)

    ms._publish_run_id("20260730-07")

    assert ms.RUN_ID == "20260730-07"
    folder = Path("/tmp/does-not-need-to-exist")
    assert ms._hello_path(folder, "linux").name == "20260730-07-hello-linux.txt"
    assert ms._ready_path(folder, "macos").name == "20260730-07-ready-macos.txt"
    assert ms._done_path(folder, "windows").name == "20260730-07-done-windows.txt"
    assert ms._shared_path(folder).name == "20260730-07-shared.txt"


def test_published_run_id_reaches_a_child_process(monkeypatch):
    """The id must also land in the environment.

    The app and the sync agent are SPAWNED (the app spawns the agent — see the
    module's `isolated_sync_agent` note), so an id that lives only as a module
    global would be invisible to anything downstream that reads the env. Cheap to
    guarantee, expensive to discover missing during an operator-held cohort.
    """
    monkeypatch.setattr(ms, "RUN_ID", "")
    monkeypatch.delenv("FAUNA_MULTISEAT_RUN_ID", raising=False)

    ms._publish_run_id("20260730-02")

    import os

    assert os.environ["FAUNA_MULTISEAT_RUN_ID"] == "20260730-02"


def test_an_explicit_env_run_id_still_wins_for_a_two_invocation_round(monkeypatch):
    """The existing operator-compare flow must keep working byte-for-byte.

    A machine that announces in one invocation and seats in another exports the
    id; combined mode is purely additive. If this ever regresses, the documented
    handshake silently stops honouring the operator's chosen id — which is how a
    cohort ends up split across two ids with no failure until phase 1.
    """
    monkeypatch.setenv("FAUNA_MULTISEAT_RUN_ID", "20260730-11")
    assert ms._resolve_run_id() == "20260730-11"
    # ...and it wins even over a computed candidate.
    assert ms._resolve_run_id(computed="20260730-99") == "20260730-11"


def test_resolve_falls_back_to_the_computed_id_when_no_override(monkeypatch):
    monkeypatch.delenv("FAUNA_MULTISEAT_RUN_ID", raising=False)
    assert ms._resolve_run_id(computed="20260730-04") == "20260730-04"


# ── 2. phase 1 absorbs another machine's slot queue ───────────────────────────


@pytest.fixture
def _no_real_sleeping(monkeypatch):
    """Deadline polls without wall-clock cost. The loop's cadence is not what is
    under test here — which budget it honours is."""
    monkeypatch.setattr(ms.time, "sleep", lambda _s: None)


def test_phase_one_honours_a_budget_larger_than_the_symmetric_window(
    monkeypatch, _no_real_sleeping
):
    """A file that appears after WINDOW but inside the assembly budget is observed.

    Red before the fix: `_await_files` computed `deadline = monotonic() + WINDOW`
    unconditionally, so with WINDOW exhausted it never polled again and a healthy
    cohort failed `phase 1 (create + rendezvous)` because a PEER was still in the
    slot queue. The creator's merge base is exactly the output a peer cannot
    hurry.
    """
    monkeypatch.setattr(ms, "WINDOW", 0.0)

    target = Path("/tmp/multiseat-assembly-probe.txt")
    reads = {"n": 0}

    def _fake_read(path):
        reads["n"] += 1
        # Arrives on the third poll — impossible to reach on a WINDOW=0 budget.
        return "base" if reads["n"] >= 3 else None

    monkeypatch.setattr(ms, "_read", _fake_read)

    ms._await_files(
        "macos", None, {target: "base"}, "phase 1 (create + rendezvous)", budget=60.0
    )
    assert reads["n"] >= 3


def test_symmetric_phases_still_use_the_tight_window(monkeypatch, _no_real_sleeping):
    """Phases 2-4 must NOT inherit the assembly budget.

    Their whole point is that the edits genuinely overlap within one sync latency;
    a peer allowed to stroll in 20 minutes late would still "pass" while proving
    nothing about concurrency. So the default budget stays WINDOW, and a wait that
    exceeds it fails with the self-diagnosing message.
    """
    monkeypatch.setattr(ms, "WINDOW", 0.0)
    monkeypatch.setattr(ms, "_read", lambda _p: None)
    monkeypatch.setattr(ms, "_nest_listing", lambda _app: (set(), True))

    # `_await_files` reports via pytest.fail, so the outcome exception is
    # `pytest.fail.Exception`, not AssertionError.
    with pytest.raises(pytest.fail.Exception, match=r"phase 2 \(ready\)"):
        ms._await_files(
            "macos", None, {Path("/tmp/never.txt"): "x"}, "phase 2 (ready)"
        )


def test_assembly_window_is_derived_from_the_window_never_a_bare_constant():
    """A raised window is exactly what a loaded box needs, so the assembly budget
    must grow with it — the same argument the module's derived pytest timeout
    makes. A fixed constant would silently truncate a deliberately-widened run."""
    assert ms.ASSEMBLY_WINDOW >= ms.WINDOW
    assert ms.ASSEMBLY_WINDOW >= 900.0


def test_the_module_timeout_still_bounds_the_widened_worst_case():
    """testing.md point 9: bounded always, unbounded never. The ceiling must cover
    an assembly-budgeted phase 1 PLUS the three symmetric phases plus the
    build/launch/sign-in prologue, or a healthy-but-slow round is hard-killed
    mid-phase — which reads as a crash rather than a timeout."""
    ceiling = next(
        m.args[0] for m in ms.pytestmark if getattr(m, "name", "") == "timeout"
    )
    assert ceiling >= 2 * ms.ASSEMBLY_WINDOW + 3 * ms.WINDOW + 300


# ── 3. adopting the shared folder is the SAME asymmetric wait, one step
#      earlier — a peer waiting on the CREATOR to make the set, before phase 1 ──


def test_adopt_or_create_set_honours_the_assembly_budget_not_the_symmetric_window(
    monkeypatch, _no_real_sleeping
):
    """Red before the fix: `_adopt_or_create_set` computed `deadline =
    monotonic() + WINDOW` for a peer waiting on the CREATOR to make the shared
    folder — the identical asymmetric-wait shape phase 1 has (a peer cannot
    hurry the creator's output), just one step earlier, before the folder is
    even bound. A creator still in its own e2e-slot queue or cold build past
    WINDOW seconds failed a peer here with "was the {CREATOR} seat started?"
    even though the creator was merely slow, never broken.

    Live proof, run `20260730-01`: the macos seat failed exactly this wait at
    600s ("folder 'e2e-multiseat' never appeared within 600s — was the linux
    seat started?") while linux's own seat build was still running — the same
    failure mode phase 1's ASSEMBLY_WINDOW fix exists to prevent, just at this
    earlier gate.
    """
    monkeypatch.setattr(ms, "WINDOW", 0.0)
    monkeypatch.setattr(ms, "CREATOR", "linux")

    class _FakeDriver:
        def wait_for(self, *_a, **_k):
            pass

    class _FakeApp:
        driver = _FakeDriver()

        def error_text(self):
            return ""

    calls = {"n": 0}

    class _FakeBackups:
        def navigate_folders(self):
            pass

        def navigate_devices(self):
            pass

        def folder_count(self):
            calls["n"] += 1
            # Arrives on the third poll — impossible to reach on a WINDOW=0 budget.
            return 1 if calls["n"] >= 3 else 0

        def folder_title(self, _i):
            return ms.SET_NAME

    ms._adopt_or_create_set(_FakeApp(), _FakeBackups(), "macos")
    assert calls["n"] >= 3


# ── 4. a wait NARRATES while it runs — partial convergence is a live signal ────


def test_the_waiter_names_each_file_as_it_arrives(monkeypatch, _no_real_sleeping, capsys):
    """A file landing mid-wait must be printed the poll it is observed.

    Red before this gate: `_await_files` printed nothing between its caller's
    one entry line and its return-or-fail. So the whole assembly budget — up to
    1200 s on phase 1, spanning another machine's slot queue and cold build —
    was textually indistinguishable from a hang, and the loop's own knowledge of
    WHICH peer was outstanding was thrown away on every poll and only rebuilt at
    failure time. An operator holding three machines open had to spend the full
    budget to learn a fact the waiter had after one second.

    Live proof, run `20260730-02` (the windows seat): `hello-linux` and the
    shared merge base had both landed and only the macos hello was missing — a
    partially converged cohort, which is exactly the state worth acting on early
    — but the log showed only "awaiting peers" and the arrivals had to be
    recovered by listing the bound folder from outside the run.

    Observability only: convention 14 still governs what is ASSERTED (the
    deadline poll is unchanged, and a green run's timing is untouched).
    """
    monkeypatch.setattr(ms, "WINDOW", 60.0)
    linux = Path("/tmp/ms-hello-linux.txt")
    macos = Path("/tmp/ms-hello-macos.txt")
    polls = {"n": 0}

    def _fake_read(path):
        if path == linux:
            return "linux-hello"  # already there on the first poll
        polls["n"] += 1
        return "macos-hello" if polls["n"] >= 4 else None  # the straggler

    monkeypatch.setattr(ms, "_read", _fake_read)

    ms._await_files(
        "windows",
        None,
        {linux: "linux-hello", macos: "macos-hello"},
        "phase 1 (create + rendezvous)",
    )

    out = capsys.readouterr().out
    assert "ms-hello-linux.txt" in out, "the early arrival was never announced"
    assert "ms-hello-macos.txt" in out, "the straggler's arrival was never announced"

    # The load-bearing half: while one file is outstanding, the waiter must name
    # THAT FILE. "still waiting" alone would not have told the -02 operator that
    # macos specifically was the missing seat.
    outstanding = [ln for ln in out.splitlines() if "still waiting on" in ln]
    assert outstanding, "the waiter never said what was still outstanding"
    assert any("ms-hello-macos.txt" in ln for ln in outstanding)
    assert not any("ms-hello-linux.txt" in ln for ln in outstanding), (
        "a file already observed must not be reported as still outstanding"
    )


# ── 5. a missing set row is diagnosed against the NEST, never against a peer ───


class _DiagBackups:
    """A Folders row list that renders `titles` and nothing else."""

    def __init__(self, titles):
        self._titles = list(titles)

    def navigate_folders(self):
        pass

    def navigate_devices(self):
        pass

    def folder_count(self):
        return len(self._titles)

    def folder_title(self, i):
        return self._titles[i]


class _DiagApp:
    class _Driver:
        def wait_for(self, *_a, **_k):
            pass

    driver = _Driver()

    def error_text(self):
        return ""


def test_one_client_reading_media_but_no_rows_is_blamed_locally_not_on_the_creator(
    monkeypatch,
):
    """Media reads fine and the row list is empty ⇒ the fault is LOCAL, and the
    message must say so without accusing a peer.

    Red before this gate: the message asked "was the {CREATOR} seat started?" —
    naming the one party this seat holds no evidence about. On run
    `20260730-02` that accusation was provably FALSE (the creator had created
    the set; the other two machines had already exchanged files through it) and
    it sent the diagnosis to the wrong machine for a full session. Same class as
    the "NEVER ARRIVED" misdiagnosis this module already closed once.

    The sound inference is the SPLIT, not the file count: two surfaces of one
    client over one nest disagreeing cannot be explained by a dead connection
    or by a peer.
    """
    monkeypatch.setattr(ms, "CREATOR", "linux")
    monkeypatch.setattr(ms, "_nest_listing", lambda _app: ({"a.txt", "b.txt"}, True))

    msg = ms._diagnose_missing_set_row(
        _DiagApp(), _DiagBackups(["Photo Library", "some-other-set"]), "macos", 1200.0
    )

    assert "FAULT IS LOCAL TO THIS CLIENT" in msg
    # The exact false accusation that cost run -02 its diagnosis.
    assert "was the linux seat started?" not in msg
    # The rows actually rendered are the smoking gun — never thrown away again.
    assert "Photo Library" in msg and "some-other-set" in msg


def test_the_media_witness_never_claims_the_set_itself_exists(monkeypatch):
    """A non-empty Media read must NOT be reported as proof the set exists.

    `set_filter(SET_NAME)` is best-effort: it silently falls back to the
    ALL-MEDIA view when the set offers no filter option — and an empty folder
    list is exactly what removes that option, so the fallback is *correlated*
    with the very failure being diagnosed. Reading "40 files" as "the set has 40
    files" would therefore be unsound in precisely the case that matters. This
    module already retired one inference of this shape (`settle_listing`'s note:
    "the filter proved the set is non-empty" — tried 2026-07-24 and reverted);
    this gate stops it being reintroduced through the failure text instead.
    """
    monkeypatch.setattr(ms, "CREATOR", "linux")
    monkeypatch.setattr(ms, "_nest_listing", lambda _app: ({"a.txt"} , True))

    msg = ms._diagnose_missing_set_row(_DiagApp(), _DiagBackups([]), "macos", None)

    assert "NOT proof the SET exists" in msg
    assert "ALL-MEDIA" in msg
    # The claim the live -02 repro would have licensed, and must not.
    assert "EXISTS ON THE NEST" not in msg


def test_an_empty_nest_read_refuses_to_accuse_the_creator(monkeypatch):
    """No files visible is NOT proof a peer failed.

    `set_filter` succeeds against a genuinely empty set, so "no files" is
    consistent with both a creator that never ran and a real empty set. A
    message that resolved this to "the creator never started" would just
    relocate the false accusation this gate exists to remove.
    """
    monkeypatch.setattr(ms, "CREATOR", "linux")
    monkeypatch.setattr(ms, "_nest_listing", lambda _app: (set(), True))

    msg = ms._diagnose_missing_set_row(_DiagApp(), _DiagBackups([]), "macos", None)

    assert "Not proof either way" in msg
    assert "EXISTS ON THE NEST" not in msg


def test_an_incomplete_nest_read_says_it_cannot_tell(monkeypatch):
    """The Media read did not complete ⇒ this seat has established nothing about
    any peer, and must say that rather than defaulting to blame."""
    monkeypatch.setattr(ms, "CREATOR", "linux")
    monkeypatch.setattr(ms, "_nest_listing", lambda _app: (None, False))

    msg = ms._diagnose_missing_set_row(_DiagApp(), _DiagBackups([]), "macos", None)

    assert "CANNOT TELL" in msg
    assert "EXISTS ON THE NEST" not in msg


def test_zero_rendered_rows_is_reported_as_the_registration_smoking_gun(monkeypatch):
    """An EMPTY row list must be visible in the message as a count, not implied.

    `folder_count()` counts *registered* automation slots, so a client that
    realizes rows lazily loses the row AND the count together — `_folder_index`
    never visits the missing index and no LookupError is ever raised. That makes
    "0 rows rendered" the distinguishing evidence for a realization fault, and
    it is precisely what the old message omitted.
    """
    monkeypatch.setattr(ms, "_nest_listing", lambda _app: ({"a.txt"}, True))

    msg = ms._diagnose_missing_set_row(_DiagApp(), _DiagBackups([]), "macos", None)

    assert "rows rendered here: 0 -> []" in msg


def test_an_unreadable_row_never_raises_out_of_the_diagnosis(monkeypatch):
    """Diagnosis must survive the failure it is diagnosing.

    A row counted but unreadable (the LookupError shape) has to be REPORTED,
    not raised — otherwise the failure path throws away the failure message and
    the session sees a traceback where the evidence should be.
    """

    class _Broken(_DiagBackups):
        def folder_title(self, i):
            raise LookupError(f"row {i} is not registered")

    monkeypatch.setattr(ms, "_nest_listing", lambda _app: ({"a.txt"}, True))

    msg = ms._diagnose_missing_set_row(_DiagApp(), _Broken(["x", "y"]), "macos", None)

    assert "rows rendered here: 2 ->" in msg
    assert "unreadable" in msg


# ── 6. the row list is read AFTER it loads, never when the page merely mounts ──


def test_the_row_read_waits_for_the_list_not_for_the_add_button(_no_real_sleeping):
    """Red before the fix: the Folders row list was read the instant
    `folder-add-button` appeared.

    That button lives in the section HEADER and renders synchronously with the
    page; the rows arrive from the machine's first async `refresh()`. Measured
    on macOS against the live account (2026-07-31, three consecutive mounts):
    the add button is up at 0.00 s with 0 rows, and the rows register 0.21 s
    later. So the read was a read of the pre-refresh page — latency-dependent
    in exactly the way convention 14 forbids.

    Cost of that one-line assumption: the macOS seat looked incapable of seeing
    the shared set at all, which killed two operator-coordinated tri-machine
    rounds and sent three sessions after the nest, the MLS join-filter and
    apple's `list_folders` — none of which were ever at fault (the
    instrumented run recorded `folders=5` in the machine and `vm sees
    folders=5` in the view model while the harness counted 0).

    tui passed throughout because it builds its `DevicesMachine` at session
    attach, so its rows precede the paint. Both client shapes are correct; the
    HARNESS must not encode either one's timing.
    """
    polls = {"n": 0}

    class _LateRows:
        def folder_count(self):
            polls["n"] += 1
            # The macOS shape: nothing on the first reads, the full list after.
            return 5 if polls["n"] > 3 else 0

    ms._await_folder_rows(_LateRows(), budget=10.0)

    assert polls["n"] > 3, "returned before the row list had loaded"


def test_a_settled_page_pays_nothing_to_be_read(_no_real_sleeping):
    """The green-run half of convention 14: a page whose rows are already
    registered must return on the FIRST poll. A budget that is always paid is a
    fixed delay wearing a deadline's clothes — the `sleep(2.0)` this replaced."""
    polls = {"n": 0}

    class _AlreadyLoaded:
        def folder_count(self):
            polls["n"] += 1
            return 5

    ms._await_folder_rows(_AlreadyLoaded(), budget=10.0)

    assert polls["n"] == 1


def test_a_genuinely_empty_list_returns_and_lets_the_caller_assert(_no_real_sleeping):
    """An account that really owns no sets must not hang past the budget, and
    must not raise: the caller's own assertion (and `_diagnose_missing_set_row`)
    is what reports the empty list, with its nest witness attached."""

    class _Empty:
        def folder_count(self):
            return 0

    ms._await_folder_rows(_Empty(), budget=0.5)  # returns, does not raise


def test_a_read_failure_is_treated_as_not_ready_never_as_an_answer(_no_real_sleeping):
    """A driver read that throws mid-load is 'not ready yet', not 'zero rows' —
    swallowing it as an answer would reintroduce the same false negative."""
    polls = {"n": 0}

    class _FlakyThenGood:
        def folder_count(self):
            polls["n"] += 1
            if polls["n"] <= 2:
                raise RuntimeError("bridge not ready")
            return 3

    ms._await_folder_rows(_FlakyThenGood(), budget=10.0)

    assert polls["n"] == 3


# ── 7. a seat with no client swaps its witness, and the evidence with it ───────


def test_a_seat_with_no_client_still_gets_a_diagnosing_failure(
    monkeypatch, _no_real_sleeping, tmp_path
):
    """``_await_files(app=None)`` must fail with the seat's OWN witness attached.

    Red before the hooks: the failure ended in ``app.error_text()`` and the
    Media-listing self-check, so a client-free seat's report was two lines of
    "undetermined" and nothing from the witness that actually knows. The point of
    point 6 is that the failure diagnoses itself; a seat that swapped its witness
    must be able to swap the evidence too.
    """
    monkeypatch.setattr(ms, "WINDOW", 0.0)
    monkeypatch.setattr(ms, "_read", lambda _p: None)

    # `pytest.fail` raises `Failed`, which derives from BaseException — a bare
    # `pytest.raises(Exception)` would sail past it and the assertions below
    # would never run.
    with pytest.raises(pytest.fail.Exception) as excinfo:
        ms._await_files(
            "linux",
            None,
            {tmp_path / "20260730-02-hello-macos.txt": "hello"},
            "phase 1 (create + rendezvous)",
            self_note=lambda _f: "  self-check: the seat notified everything",
            extra_note=lambda: "  seat log: /tmp/seat-linux.log",
        )
    message = str(excinfo.value)
    assert "the seat notified everything" in message
    assert "/tmp/seat-linux.log" in message
    assert "no client on this seat" in message, (
        "a seat with no client must say so, never report an unreadable app"
    )


def test_the_client_seats_failure_message_is_unchanged(
    monkeypatch, _no_real_sleeping, tmp_path
):
    """The hooks are additive: with neither supplied, nothing about the client
    round's diagnosis moves."""
    monkeypatch.setattr(ms, "WINDOW", 0.0)
    monkeypatch.setattr(ms, "_read", lambda _p: None)
    monkeypatch.setattr(ms, "_nest_listing", lambda _a: (set(), True))

    class _App:
        def error_text(self):
            return "boom"

    # `pytest.fail` raises `Failed`, which derives from BaseException — a bare
    # `pytest.raises(Exception)` would sail past it and the assertions below
    # would never run.
    with pytest.raises(pytest.fail.Exception) as excinfo:
        ms._await_files(
            "linux",
            _App(),
            {tmp_path / "20260730-02-hello-macos.txt": "hello"},
            "phase 1 (create + rendezvous)",
        )
    message = str(excinfo.value)
    assert "app error element: 'boom'" in message
    assert "self-check" in message


# ── 8. the announce's freshness claim ──────────────────────────────────────────


def test_an_announce_marks_its_id_as_nest_computed(monkeypatch):
    """Only an ANNOUNCE may claim "this id came out of a nest read".

    That claim is what lets a seat whose own view of the set is unavailable still
    know the id is fresh: `next_run_id_from_names` returns 1 + the highest NN
    already present today, so no file can carry it yet. `_publish_run_id` must
    NOT set it — a seat calls that too, and a seat assigning itself an id proves
    nothing about the set.
    """
    monkeypatch.setattr(ms, "_ANNOUNCED_IN_SESSION", False)
    ms._publish_run_id("20260731-09")
    assert ms.announced_in_session() is False, (
        "publishing an id must not, by itself, claim a nest read happened"
    )
    ms._note_announced_in_session()
    assert ms.announced_in_session() is True

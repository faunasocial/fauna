"""A redirected pipe must be drained for as long as the child lives (testing.md
§ Cross-app e2e conventions, point 13).

This pins the convention headlessly, in seconds, with no client and no bridge —
the mechanism is a property of `subprocess.PIPE`, not of any app. It is worth a
dedicated test because the failure it prevents is invisible at the point of
breakage: an undrained pipe does not raise, it *blocks the child's writing
thread*, and the symptom surfaces arbitrarily far away (on windows, as the app's
UI thread wedging inside a log call, which read as a product bug for seven
sessions).
"""

import shutil
import subprocess
import sys
import threading
import time

import pytest

from drivers import port_util
from drivers.port_util import (
    drain_pipes,
    popen_group_kwargs,
    reap_descendants_of,
    wait_pipes_drained,
)

pytestmark = pytest.mark.tier_1

# Comfortably beyond the ~64 KB OS pipe buffer, so an undrained pipe is
# GUARANTEED to block rather than merely likely to.
_LINES = 20_000
_LINE_BYTES = 80


def _flooding_child() -> subprocess.Popen:
    # The final marker rides the SAME stream as the flood. The two reader threads
    # append to one shared tail with no ordering between streams, so a marker on
    # stdout raced the stderr reader's backlog (up to ~800 buffered lines at the
    # child's exit): a reader the scheduler had starved appended 500+ lines after
    # it and evicted it from a 500-line tail. One stream is one reader, and a
    # reader appends in the child's own order.
    code = (
        "import sys\n"
        f"for _ in range({_LINES}):\n"
        f"    sys.stderr.write('x' * {_LINE_BYTES} + '\\n')\n"
        "sys.stderr.write('CHILD_DONE\\n')\n"
        "sys.stderr.flush()\n"
    )
    proc = subprocess.Popen(
        [sys.executable, "-c", code],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — no-op off Windows.
    reap_descendants_of(proc.pid)
    return proc


def test_drained_child_completes_despite_flooding_its_pipe():
    """The whole point: a child that writes far past the buffer still exits."""
    proc = _flooding_child()
    drain_pipes(proc)
    # Without draining this wait never returns; the bound is what makes the
    # regression a failure rather than a hang.
    assert proc.wait(timeout=60) == 0


def test_undrained_child_blocks__proving_the_test_above_is_not_vacuous():
    """The guard is worthless unless the undrained case genuinely deadlocks.

    Asserting the *absence* of a hang needs a positive demonstration beside it
    that the hang is real — otherwise a future refactor could make `drain_pipes`
    a no-op and both tests would still pass.
    """
    proc = _flooding_child()
    try:
        with pytest.raises(subprocess.TimeoutExpired):
            proc.wait(timeout=5)  # blocked on a full, unread stderr buffer
    finally:
        # Drain to unblock, then reap — never leave a stuck child behind.
        drain_pipes(proc)
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def test_drain_keeps_a_bounded_tail_for_diagnostics():
    """Draining must not mean discarding: the output is still the diagnostic."""
    proc = _flooding_child()
    recent = drain_pipes(proc, maxlen=500)
    assert proc.wait(timeout=60) == 0
    # The reader threads are daemons racing the exit: join them at EOF (the
    # completion signal, convention 14) rather than polling a stopwatch.
    assert wait_pipes_drained(proc, timeout=60), "both reader threads reach EOF"
    assert len(recent) <= 500, "tail must stay bounded, not grow without limit"
    assert any("CHILD_DONE" in line for line in recent), (
        "the child's final line should survive in the retained tail"
    )


def test_both_streams_are_drained_into_the_tail_tagged_by_stream():
    """The flood rides stderr, so stdout's drain is pinned on its own: a few
    lines on each stream, a tail big enough that nothing is evicted, so the
    assertion reads membership rather than the order two reader threads win."""
    code = "import sys\nprint('OUT_LINE')\nsys.stderr.write('ERR_LINE\\n')\n"
    proc = subprocess.Popen(
        [sys.executable, "-c", code],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    recent = drain_pipes(proc, maxlen=500)
    assert proc.wait(timeout=60) == 0
    assert wait_pipes_drained(proc, timeout=60), "both reader threads reach EOF"
    assert "[out] OUT_LINE" in recent
    assert "[err] ERR_LINE" in recent


# ── teeing: a drain that is ALSO the live view ─────────────────────────────────
#
# A long build is the case where "drain it" and "show it live" pull apart. The
# multiseat announce builds this machine's seat client inside the announce step,
# and the operator is holding two other machines open against an already-printed
# run id. Two failure modes bracket the choice, and only a tee escapes both:
# capture-then-report makes a healthy multi-minute build look HUNG (nothing is
# emitted until it exits), while stream-and-forget loses the output entirely on
# the paths where the child's inherited fd never reaches the pytest log — which
# is how an announce-time build break arrived as a bare CalledProcessError with
# no evidence at all (observed 2026-07-29).


def test_echo_tees_lines_live_while_still_retaining_the_tail():
    """`echo=` must not turn the drain into a capture: lines land on the echo
    stream AND in the bounded tail."""
    import io

    sink = io.StringIO()
    proc = _flooding_child()
    recent = drain_pipes(proc, maxlen=500, echo=sink)
    assert proc.wait(timeout=60) == 0
    assert wait_pipes_drained(proc, timeout=60), "both reader threads reach EOF"

    assert "CHILD_DONE" in sink.getvalue(), "the echo stream must see the output"
    assert any("CHILD_DONE" in line for line in recent), (
        "teeing must not cost the retained tail — both sinks, not either/or"
    )


def test_echo_filter_narrows_the_tee_without_narrowing_the_retained_tail():
    """`echo_filter=` is what makes an always-on tee affordable.

    The windows bridge tees its own `[bridge]` narration live, because the
    retained tail is only ever read by a formatted FAILURES section and the
    pytest-timeout watchdog kills the process without formatting one — so the
    hangs worth explaining are exactly the ones whose explanation is dropped.
    That tee may not drag the child's bulk log volume onto the critical path
    with it, and it may not cost the tail either.
    """
    import io

    sink = io.StringIO()
    proc = _flooding_child()
    recent = drain_pipes(
        proc, maxlen=500, echo=sink, echo_filter=lambda line: "CHILD_DONE" in line
    )
    assert proc.wait(timeout=60) == 0
    assert wait_pipes_drained(proc, timeout=30), "reader threads never hit EOF"

    echoed = sink.getvalue()
    assert "CHILD_DONE" in echoed, "a line the filter admits must reach the echo"
    assert "x" * _LINE_BYTES not in echoed, (
        "a line the filter rejects must NOT reach the echo — the whole point is "
        "keeping the child's log volume off the critical path"
    )
    assert any("x" * _LINE_BYTES in line for line in recent), (
        "filtering the TEE must not filter the retained tail — the deque is the "
        "post-mortem record and stays complete"
    )


def test_a_raising_echo_filter_still_echoes_rather_than_going_silent():
    """A broken predicate must degrade to noise, never to silence.

    The drain is the one thing here that may not grow a new way to fail: it
    exists because an unread pipe wedges the child. A filter that throws is a
    bug in the caller, and the safe reading of an unanswerable "should this line
    be shown?" is yes — a silent drain would hide the very output someone added
    the filter to surface.
    """
    import io

    def _explode(_line: str) -> bool:
        raise RuntimeError("filter is broken")

    sink = io.StringIO()
    proc = _flooding_child()
    drain_pipes(proc, maxlen=500, echo=sink, echo_filter=_explode)
    assert proc.wait(timeout=60) == 0
    assert wait_pipes_drained(proc, timeout=30), "reader threads never hit EOF"

    assert "CHILD_DONE" in sink.getvalue(), (
        "a raising filter must fall back to echoing, not silence the tee"
    )


def test_echo_filter_defaults_off_so_existing_tees_are_unchanged():
    """No filter means tee everything — the pre-existing `echo=` contract."""
    import inspect

    sig = inspect.signature(drain_pipes)
    assert sig.parameters["echo_filter"].default is None


def test_echo_defaults_off_so_existing_drains_are_unchanged():
    """The windows-bridge drain must keep its exact behaviour: draining silently.

    Echoing by default would double every driver's log volume onto the pytest
    stream — and log volume is precisely the axis the drain contract exists to
    keep off the critical path.
    """
    import inspect

    sig = inspect.signature(drain_pipes)
    assert sig.parameters["echo"].default is None


# ── wait_pipes_drained: "recent is non-empty" is not "drained" ─────────────────
#
# `proc.wait()` returning only means the child exited; the reader threads
# still need to be scheduled to read whatever is left in the pipe. A caller
# that treats `recent` going non-empty as "drained" can genuinely observe a
# PARTIAL tail: the first line lands, the check fires before the reader's
# next `readline()` returns, and a real error line further down never makes
# it into what gets quoted. This is exactly what let
# `test_a_failed_client_build_quotes_the_build_output` fail once under a
# batched tier_1 sweep — the quoted tail was one `Compiling...` line, not the
# `error[E0425]` line the assertion looks for. wait_pipes_drained() replaces
# the "recent is truthy" guess with a real completion signal: the reader
# thread's own EOF, joined with a bound.


def test_wait_pipes_drained_returns_true_only_once_the_reader_hits_eof():
    """Once it returns True, `recent` must hold the reader's FULL output —
    not just whatever had landed by the time `recent` first went non-empty."""
    proc = _flooding_child()
    recent = drain_pipes(proc)
    assert proc.wait(timeout=60) == 0
    assert wait_pipes_drained(proc, timeout=30.0) is True
    assert any("CHILD_DONE" in line for line in recent), (
        "wait_pipes_drained() returning True must mean the reader drained "
        "through the child's final line, not just its first"
    )


def test_binary_pipes_reach_eof_when_the_child_exits():
    """A binary pipe's `readline()` returns `b""` at EOF, never `""`: a loop that
    ends only on `""` spins at full speed for the life of the pytest process, and
    a few dozen such threads starve the test thread of the GIL (a tier-1 run took
    7 hours). The reader must end on ANY empty read, text and binary alike."""
    proc = subprocess.Popen(
        [sys.executable, "-c", "import sys\nprint('BIN_OUT')\nsys.stderr.write('BIN_ERR\\n')\n"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,  # no text=True: the pipes are binary
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    recent = drain_pipes(proc, maxlen=500)
    assert proc.wait(timeout=60) == 0
    assert wait_pipes_drained(proc, timeout=10), "binary reader threads never hit EOF"
    assert "[out] BIN_OUT" in recent
    assert "[err] BIN_ERR" in recent
    assert not [t for t in port_util._drain_threads[proc] if t.is_alive()]


def test_wait_pipes_drained_times_out_on_a_reader_that_never_reaches_eof():
    """Bounded, per convention 14 — a genuinely stuck reader must not hang the
    caller forever; the caller can still quote a partial `recent` on timeout."""
    proc = _flooding_child()
    drain_pipes(proc)
    assert proc.wait(timeout=60) == 0
    # Splice in a reader thread that deliberately never reaches EOF, standing
    # in for a stuck pipe without needing to construct a real one.
    stuck = threading.Event()
    never_finishes = threading.Thread(target=stuck.wait, daemon=True)
    never_finishes.start()
    port_util._drain_threads[proc] = [never_finishes]
    try:
        started = time.monotonic()
        finished = wait_pipes_drained(proc, timeout=0.3)
        elapsed = time.monotonic() - started
        assert finished is False
        assert elapsed < 5.0, "must respect the timeout bound, not the thread's lifetime"
    finally:
        stuck.set()
        never_finishes.join(timeout=5)


# ── the client build re-emits its own output on failure ────────────────────────


def _fake_just(tmp_path, body: str) -> None:
    """Write a fake `just` on PATH that runs the POSIX shell script `body`.

    `_build_via_just` resolves the `just` binary via `shutil.which`, which —
    unlike a bare-name PATH lookup through `Popen`/`CreateProcess` — DOES
    honour Windows' PATHEXT, so a `.cmd` fake is found there. But every
    caller's `body` is POSIX shell (`$$`, `[ -e ... ]`, `: > file`), so on
    Windows the `.cmd` hands `body` to Git Bash's `sh.exe` instead of running
    it itself. `sh` is resolved to an ABSOLUTE PATH *before* the fixture
    replaces PATH wholesale (`monkeypatch.setenv("PATH", ..., prepend=False)`
    strips the real `sh.exe` off PATH too, same as it does `just`).
    """
    if sys.platform == "win32":
        sh = shutil.which("sh")
        assert sh, "no POSIX sh on PATH to back the fake `just` shim"
        script = tmp_path / "just.sh"
        script.write_text("#!/bin/sh\n" + body)
        fake_just = tmp_path / "just.cmd"
        fake_just.write_text(
            "@echo off\n" f'"{sh}" "{script}" %*\n' "exit /b %ERRORLEVEL%\n"
        )
    else:
        fake_just = tmp_path / "just"
        fake_just.write_text("#!/bin/sh\n" + body)
        fake_just.chmod(0o755)


def test_a_failed_client_build_quotes_the_build_output(monkeypatch, tmp_path):
    """A broken recipe must diagnose itself (convention 6).

    Before this, a failing `just <recipe>` on the multiseat announce path raised a
    bare CalledProcessError with NO build output — the operator was holding two
    other machines open against an already-printed run id, with nothing to look
    at. The message must now carry the recipe's own last lines AND the repro
    command; a stack trace pointing at subprocess is not a diagnosis.
    """
    import conftest

    _fake_just(
        tmp_path,
        "echo 'Compiling fauna-tui v0.1.0'\n"
        "echo 'error[E0425]: cannot find value `oops` in this scope' >&2\n"
        "exit 101\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    with pytest.raises(RuntimeError) as excinfo:
        conftest._build_app_via_just("tui")

    msg = str(excinfo.value)
    assert "exited 101" in msg
    assert "cannot find value" in msg, (
        "the recipe's own error line must survive into the failure message — "
        "that line IS the diagnosis"
    )
    assert "just tui-debug" in msg, "the repro command must still be named"


def test_a_successful_client_build_raises_nothing(monkeypatch, tmp_path):
    """The tee must not invent failures: rc 0 is a pass, output or no output."""
    import conftest

    _fake_just(tmp_path, "echo 'Finished dev profile'\nexit 0\n")
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    conftest._build_app_via_just("tui")  # must not raise


# ── the client build retries ONCE on a transient failure (follow-up (b) of ──
# the announce-build trap, filed 2026-07-29, closed 2026-08-06)
#
# Observed live 2026-07-29: a seat build broke with a bare non-zero exit
# while another concurrent build held the other build slot compiling
# `fauna-ffi`; re-running by hand exited 0. On the multiseat announce path a
# false red like that strands an operator-coordinated cross-machine cohort,
# so a bounded single retry is cheap insurance against exactly that class of
# failure.


def test_a_seat_build_retries_once_on_a_transient_failure(monkeypatch, tmp_path):
    """A failure that does NOT repeat on the very next attempt must not raise
    — that is the transient-contention case this retry exists for.

    The recipe decides fail-vs-succeed from a sentinel FILE'S EXISTENCE
    (created by the failing first run, checked by the second), not from a
    count it reads back — a subprocess reading CONTENT a sibling subprocess
    wrote is unreliable in this harness (a prior version of this test hung
    on that exact trap: two supposedly-sequential invocations both saw the
    counter file as absent). `[ -e ... ]` existence checks and unconditional
    writes ARE reliable; only "read a sibling's content" is not.
    """
    import conftest

    invocations = tmp_path / "invocations"
    invocations.mkdir()
    sentinel = tmp_path / "first_attempt_happened"
    _fake_just(
        tmp_path,
        f': > "{invocations}/$$"\n'  # unconditional: proves this run happened
        f'if [ -e "{sentinel}" ]; then\n'
        "  echo 'Finished dev profile'\n"
        "  exit 0\n"
        "fi\n"
        f': > "{sentinel}"\n'
        "echo 'error: could not compile, extern crate deadlock' >&2\n"
        "exit 1\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    conftest._build_app_via_just("tui")  # must not raise — retry succeeded

    assert len(list(invocations.iterdir())) == 2, (
        "the recipe must have run exactly twice: the original failure and "
        "the one retry — no more, no fewer"
    )


def test_a_seat_build_that_fails_twice_quotes_both_attempts(monkeypatch, tmp_path):
    """Failing identically twice means a real break, not contention: the
    retry must be bounded to one, and the raised message must carry BOTH
    attempts' output (structurally labelled by `_build_via_just` itself, in
    "--- attempt N: ... ---" sections) so a reader can compare them."""
    import conftest

    invocations = tmp_path / "invocations"
    invocations.mkdir()
    _fake_just(
        tmp_path,
        f': > "{invocations}/$$"\n'  # unconditional: proves this run happened
        "echo 'error[E0425]: cannot find value `oops` in this scope' >&2\n"
        "exit 101\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    with pytest.raises(RuntimeError) as excinfo:
        conftest._build_app_via_just("tui")

    msg = str(excinfo.value)
    assert len(list(invocations.iterdir())) == 2, "must retry exactly once, not loop"
    assert "--- attempt 1:" in msg and "--- attempt 2:" in msg, (
        "both attempts must get their own labelled section in the message"
    )
    assert msg.count("cannot find value") == 2, (
        "the recipe's error line must survive into BOTH attempts' sections"
    )
    assert "just tui-debug" in msg


def test_a_build_slot_wait_timeout_is_not_retried(monkeypatch, tmp_path):
    """A build-slot WAIT timeout must NOT be retried — retrying
    it means queueing another up-to-90-minute wait, turning one bounded
    failure into two. This is a genuinely distinct failure class from the
    fast compile-time contention the retry exists for (2026-07-29's incident:
    a build that had ALREADY acquired its own slot broke while a sibling
    build ran in the other slot — nothing here waited for a slot at all)."""
    import conftest

    invocations = tmp_path / "invocations"
    invocations.mkdir()
    _fake_just(
        tmp_path,
        f': > "{invocations}/$$"\n'  # unconditional: proves this run happened
        "echo \"no 'build' slot freed within 5400s — queue position 1 of 1, "
        "holders:\" >&2\n"
        "exit 1\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    with pytest.raises(RuntimeError) as excinfo:
        conftest._build_app_via_just("tui")

    msg = str(excinfo.value)
    assert len(list(invocations.iterdir())) == 1, (
        "a slot-wait timeout must be raised on the FIRST failure — retrying "
        "it is exactly the outcome this test exists to prevent"
    )
    assert "NOT retried" in msg
    assert "WAIT timeout" in msg
    assert "just tui-debug" in msg


def test_a_disk_quota_failure_is_classified_as_infra_not_a_real_break(monkeypatch, tmp_path):
    """A `debug/incremental` cache eating
    a dev checkout's ZFS quota fails a build with a bare
    `building the 'tui' app FAILED (exit 101)` — the retry fails IDENTICALLY
    because the cause is persistent, and the generic advice
    ("suspect a real break") is exactly backwards for it. The real
    `Disk quota exceeded (os error 122)` line must be recognized (via the
    `INFRA_RE` pattern ported from the fleet's own merge-gate infra-failure
    classifier) and named at the TOP of the raised message, ahead of the
    generic retry advice — not left for a reader to find several hundred
    characters into a log tail."""
    import conftest

    invocations = tmp_path / "invocations"
    invocations.mkdir()
    _fake_just(
        tmp_path,
        f': > "{invocations}/$$"\n'  # unconditional: proves this run happened
        "echo 'error: could not compile `fauna-tui`: Disk quota exceeded "
        "(os error 122)' >&2\n"
        "exit 101\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    with pytest.raises(RuntimeError) as excinfo:
        conftest._build_app_via_just("tui")

    msg = str(excinfo.value)
    assert len(list(invocations.iterdir())) == 2, "still retries once, same as any other failure"
    assert "INFRA" in msg.split("\n", 1)[0], (
        "the INFRA classification must lead the message, ahead of the "
        "generic 'suspect a real break' advice"
    )
    assert "Disk quota exceeded" in msg
    assert "debug/incremental" in msg, "the cheap safe reclaim must be named"
    assert "--- attempt 1:" in msg and "--- attempt 2:" in msg, (
        "the classification is a preamble, not a replacement — both attempts "
        "still get their own labelled section"
    )
    assert "just tui-debug" in msg


def test_a_generic_compile_failure_is_not_misclassified_as_infra(monkeypatch, tmp_path):
    """Regression guard for the INFRA classification above: an ordinary
    compile error that never mentions disk/build-slot exhaustion must NOT be
    relabelled — misclassifying a real break as INFRA would hide it exactly
    the way the merge-gate check's anchored `INFRA_RE` was designed to avoid
    (an unanchored match once scored a genuine red as INFRA)."""
    import conftest

    invocations = tmp_path / "invocations"
    invocations.mkdir()
    _fake_just(
        tmp_path,
        f': > "{invocations}/$$"\n'
        "echo 'error[E0425]: cannot find value `oops` in this scope' >&2\n"
        "exit 101\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    with pytest.raises(RuntimeError) as excinfo:
        conftest._build_app_via_just("tui")

    msg = str(excinfo.value)
    assert "INFRA" not in msg

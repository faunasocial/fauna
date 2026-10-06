"""Self-terminating-harness regression proofs (testing.md § conventions, point 9).

The 2026-07-13→14 wedge incident (one hung web test held the machine-wide
browser slot for ~10 h, killing pytest left the bridge/node/Chromium tree as
orphans, and the orphan then wedged the NEXT run) ratified three mechanisms;
each gets a red-on-regression proof here rather than folklore:

1. **No singleton, no wedge** — the exact incident shape (a SECOND
   ``create_driver("web")`` + ``launch`` in one process) now simply works:
   Playwright's bundled Chromium has no machine-wide ProcessSingleton, and the
   serialization flock is deleted.
2. **Bounded tests** — pytest-timeout turns an unbounded hang into a bounded,
   diagnosed end (proven via a child pytest run). How that end is *reported* is
   platform-split: POSIX's `signal` method leaves a normal failure and an intact
   summary, windows' `thread` method `os._exit`s and leaves neither — so
   convention 7's live-reason line is what carries an earlier failure's
   traceback across the kill there. Both halves asserted, neither skipped.
3. **No orphan survivors** — every harness child is a die-with-parent process-
   group leader (``drivers/port_util.popen_group_kwargs``): ``kill -9`` of the
   pytest that spawned it reaps the whole subtree with no cleanup code running
   (kernel PDEATHSIG → the bridge's group-forwarding signal handler). Proven
   at three fidelities: a unit ``terminate_tree`` proof, a kernel PDEATHSIG
   proof, and the full drill (SIGKILL a child pytest mid-web-test, assert the
   bridge's entire process group is gone).

Section 5 pins the fifth mechanism (2026-07-29): a binary build — and the
machine-wide `build`-slot wait in front of it — is never charged to a test's
own pytest-timeout clock. Session fixtures that build binaries delegate to
memoized module-level ensurers, and ``pytest_collection_finish`` runs them at
collection time (``_prebuild_binaries``, beside ``_prebuild_web_spa`` and the
`e2e` slot, which were always correctly placed there). Before this, a fully
warm tree still died as ``Timeout (>900.0s)`` in fixture setup whenever
concurrent builds elsewhere on the machine held both build slots — a bound
inversion (a 5400 s slot wait nested inside the 900 s per-test bound) that
reads exactly like a product bug (build-system.md § Build/e2e slot locks;
observed twice on 2026-07-29).

Process safety: every kill in this file targets a pid/process-group THIS test
run created — never a discovered one, so concurrent sessions are untouchable.

Client-independent (listed in conftest ``_CLIENT_INDEPENDENT_FILES``): run with
``--include-independent``, e.g.
``pytest tests/e2e-unified/tests/test_harness_self_termination.py --include-independent``.
"""

from __future__ import annotations

import os
import pathlib
import signal
import subprocess
import sys
import textwrap
import time
from pathlib import Path

import pytest

_E2E_DIR = str(Path(__file__).resolve().parent.parent)

# tier_2 file-wide: the drill tests drive a REAL web driver (bridge + browser)
# with no backend at all, and the harness-of-the-harness tests (child pytest
# runs, process-tree units) have no product stack to mock — tier_2 is the
# closest honest depth, pinned file-level per the one-tier-per-file convention.
#
# The POSIX skip used to be file-wide, back when every mechanism here was a
# process group or PDEATHSIG. It is now PER-TEST: Windows has its own
# die-with-the-run mechanism (the kill-on-close job object, § 3b-win), and a
# file-wide skip would have hidden it — the one platform where an orphan is
# hardest to see is the one that would never have run its own proof.
_posix_only = pytest.mark.skipif(
    os.name != "posix", reason="process-group/PDEATHSIG mechanisms are POSIX-side"
)

pytestmark = [pytest.mark.tier_2]


def _pids_in_group(pgid: int) -> list[int]:
    """Live pids whose process group is `pgid`; zombies count as live until
    reaped, which is what we want — a zombie browser is still an unreaped child.

    Linux reads /proc directly. macOS has no /proc, so it shells out to `ps`
    (which also reports un-reaped zombies) — without this the whole orphan-proof
    suite errored on darwin, leaving mac, the platform with no PDEATHSIG and
    therefore the weakest reaping story, as the one that never ran its own
    proofs.
    """
    if sys.platform == "linux":
        pids = []
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            try:
                with open(f"/proc/{entry}/stat") as f:
                    stat = f.read()
            except OSError:
                continue
            fields = stat.rsplit(")", 1)[-1].split()
            if len(fields) > 2 and fields[2] == str(pgid):
                pids.append(int(entry))
        return pids
    out = subprocess.run(
        ["ps", "-axo", "pid=,pgid="], capture_output=True, text=True, check=False
    ).stdout
    pids = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1] == str(pgid):
            pids.append(int(parts[0]))
    return pids


def _wait_gone(predicate, timeout_s: float) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.25)
    return False


# ── 1. the incident shape is now harmless ───────────────────────────────────

@pytest.mark.web
@pytest.mark.timeout(300)
def test_second_web_driver_in_one_process_is_safe():
    """The 2026-07-13 wedge: a second same-process web driver blocked forever
    on the snap-Chromium singleton flock. With bundled Chromium + no flock,
    two full drivers (two bridges, two browsers) coexist; the bounded mark
    makes a reintroduced block fail crisply instead of hanging the lane.

    Runs on Windows too (the `_posix_only` mark was dropped 2026-08-02): the
    subject is a *browser* singleton, not a process group — and the constraint
    it replaced (snap Chromium) never existed off Linux, so the skip was stale
    drift, not a platform truth. Windows runs a web lane, so skipping here was
    exactly the "a skip is not coverage" shape (conventions point 7). It is also
    the only test in this file that exercises the web bridge's spawn path, which
    is where `drivers/web.py` arms the Windows die-with-the-run job — so on Windows
    this is the proof that binding the bridge does not disturb Chromium's own
    sandbox job objects (nested jobs, allowed since Windows 8)."""
    from drivers import create_driver

    d1 = create_driver("web")
    d2 = None
    try:
        d1.launch({"url": "about:blank"})
        d2 = create_driver("web")
        d2.launch({"url": "about:blank"})  # pre-fix: wedged here, forever
        assert d1._get("/health").get("ready") is True
        assert d2._get("/health").get("ready") is True
    finally:
        for d in (d2, d1):
            if d is not None:
                try:
                    d.teardown()
                except Exception:
                    pass


# ── 2. a wedge is bounded and says why — and an earlier failure's reason
#       outlives the kill that ends the run ───────────────────────────────────

@pytest.mark.timeout(120)
def test_wedged_test_is_bounded_and_says_why(tmp_path):
    """pytest-timeout converts an unbounded hang (the incident's
    hrtimer_nanosleep shape) into a bounded, diagnosed end.

    **How that end is reported is platform-split, and the split is the point.**
    On POSIX the `signal` method raises inside the hung test, so it dies as an
    ordinary failure and the session — summary and counts included — carries on.
    Windows has no `SIGALRM`, so pytest-timeout falls to its `thread` method,
    which dumps every thread's stack and then `os._exit`s the process out from
    under the terminal reporter: the run ends at `+++ Timeout +++` with no
    summary, no counts, and no traceback for any test that had already failed.

    This test ran POSIX-only until 2026-09-21 behind a `_posix_only` mark whose
    stated reason (process-group/PDEATHSIG) never described it — so convention
    9's own proof skipped the one platform where its "dies as a normal failure"
    half is false, and the gap surfaced instead as two permanently unattributable
    failures in an 11-file windows run. Asserting
    the platform truth beats skipping it (convention 7).
    """
    test_file = tmp_path / "test_wedge.py"
    test_file.write_text(
        "import time\n\ndef test_hang():\n    time.sleep(300)\n"
    )
    result = subprocess.run(
        [sys.executable, "-m", "pytest", str(test_file),
         "--timeout=3", "-p", "no:cacheprovider"],
        capture_output=True, text=True, timeout=90,
    )
    # Bounded and diagnosed, on every platform: the hang ends far inside the
    # 90 s ceiling above, and the output names the timeout as the cause.
    assert result.returncode != 0
    assert "Timeout" in result.stdout, result.stdout[-2000:]

    if os.name == "posix":
        assert "1 failed" in result.stdout, result.stdout[-2000:]
    else:
        assert "1 failed" not in result.stdout, (
            "windows now reports a summary after a pytest-timeout kill. That is "
            "good news, not a regression: re-read e2e-conventions.md convention 9 "
            "and convention 7's live-line rider, which both document the absence "
            "as the reason a FAILED test prints its reason live.\n"
            + result.stdout[-2000:]
        )


@pytest.mark.timeout(180)
def test_an_earlier_failures_reason_survives_a_timeout_kill(tmp_path):
    """The live-reason line is what carries a FAILED test's diagnosis across a
    run that never reaches its own summary (conventions 6 + 7's rider).

    The shape measured on Windows 2026-09-21: an 11-file run printed `F` twice, then
    wedged in a third file and was killed at 900 s. `pytest_sessionfinish` and
    the terminal reporter's failure summary never ran, so both tracebacks died
    with the process — and neither failure reproduced in three isolated re-runs,
    making their causes permanently unrecoverable.

    The child here re-exports the SHIPPED hook rather than a copy, so a
    regression in the real `conftest.pytest_runtest_logreport` reds this. It is
    loaded under an explicit distinct module name: pytest imports a rootdir
    `conftest.py` as the module `conftest`, so a plain `import conftest` inside
    one resolves to the partially-initialised child file, not the e2e parent.

    tier_2 file-wide (child pytest runs have no product stack to mock), and
    platform-uniform: on POSIX the summary survives the kill, so the assertion
    that discriminates on BOTH platforms is not "the reason is somewhere in the
    output" but "the reason was printed on the run's OWN clock" — before the
    timeout that ends it, never in a summary after it.
    """
    (tmp_path / "conftest.py").write_text(textwrap.dedent(f"""
        import importlib.util, sys
        sys.path.insert(0, {_E2E_DIR!r})
        _spec = importlib.util.spec_from_file_location(
            "fauna_e2e_parent_conftest", {str(Path(_E2E_DIR) / "conftest.py")!r}
        )
        _mod = importlib.util.module_from_spec(_spec)
        sys.modules["fauna_e2e_parent_conftest"] = _mod
        _spec.loader.exec_module(_mod)
        pytest_runtest_logreport = _mod.pytest_runtest_logreport
    """))
    (tmp_path / "test_wedge_after_a_failure.py").write_text(textwrap.dedent("""
        import time

        def test_fails_first():
            assert 1 == 2, "the reason that must outlive the kill"

        def test_hangs_second():
            time.sleep(300)  # sleep-ok: the wedge under test; --timeout ends it
    """))
    result = subprocess.run(
        [sys.executable, "-m", "pytest", str(tmp_path),
         "--timeout=3", "-p", "no:cacheprovider"],
        capture_output=True, text=True, timeout=150,
    )
    out = result.stdout
    assert "[e2e] FAILED" in out, out[-3000:]
    assert "test_fails_first" in out, out[-3000:]
    assert "the reason that must outlive the kill" in out, out[-3000:]

    # Capital-T `Timeout` is the banner (`+++ Timeout +++` / `Failed: Timeout
    # >3.0s`), never the lowercase `timeout: 3.0s` header line — so this ordering
    # compares the live line against the END of the run, not its start.
    assert out.index("[e2e] FAILED") < out.index("Timeout"), (
        "the failure's reason was printed only AFTER the timeout that ended the "
        "run — i.e. it came from the end-of-run summary, which a thread-method "
        "kill never reaches. It must be printed when the failure is recorded.\n"
        + out[-3000:]
    )


# ── 3a. terminate_tree reaps grandchildren ──────────────────────────────────

@pytest.mark.timeout(60)
@_posix_only
def test_terminate_tree_kills_grandchildren():
    from drivers.port_util import popen_group_kwargs, terminate_tree

    child_src = (
        "import subprocess, sys, time\n"
        "p = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(300)'])\n"
        "print(p.pid, flush=True)\n"
        "time.sleep(300)\n"
    )
    proc = subprocess.Popen(
        [sys.executable, "-c", child_src],
        stdout=subprocess.PIPE, text=True, **popen_group_kwargs(),
    )
    try:
        grandchild_pid = int(proc.stdout.readline())
        terminate_tree(proc)
        assert proc.poll() is not None, "child survived terminate_tree"
        assert _wait_gone(lambda: not _pids_in_group(proc.pid), 10.0), (
            f"process group {proc.pid} still has members "
            f"{_pids_in_group(proc.pid)} after terminate_tree"
        )
        # The grandchild specifically (reparented + reaped by init once killed).
        def grandchild_gone():
            try:
                os.kill(grandchild_pid, 0)
                return False
            except ProcessLookupError:
                return True
            except PermissionError:
                return False
        assert _wait_gone(grandchild_gone, 10.0), "grandchild survived terminate_tree"
    finally:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            pass


# ── 3b. PDEATHSIG: children die when their spawner is SIGKILLed ─────────────

@pytest.mark.timeout(60)
@pytest.mark.skipif(sys.platform != "linux", reason="PR_SET_PDEATHSIG is Linux-only")
@_posix_only
def test_pdeathsig_reaps_child_of_sigkilled_parent():
    """The kernel-level guarantee behind 'no cleanup code ran, still no
    orphans': an intermediate python (standing in for pytest) spawns a child
    via popen_group_kwargs, is SIGKILLed, and the child dies via PDEATHSIG."""
    wrapper_src = textwrap.dedent(f"""
        import subprocess, sys, time
        sys.path.insert(0, {_E2E_DIR!r})
        from drivers.port_util import popen_group_kwargs
        p = subprocess.Popen(
            [sys.executable, '-c', 'import time; time.sleep(300)'],
            **popen_group_kwargs(),
        )
        print(p.pid, flush=True)
        time.sleep(300)
    """)
    wrapper = subprocess.Popen(
        [sys.executable, "-c", wrapper_src], stdout=subprocess.PIPE, text=True,
    )
    child_pid = None
    try:
        child_pid = int(wrapper.stdout.readline())
        os.kill(wrapper.pid, signal.SIGKILL)  # no cleanup code ever runs
        wrapper.wait(timeout=10)

        def child_gone():
            try:
                os.kill(child_pid, 0)
                return False
            except ProcessLookupError:
                return True
            except PermissionError:
                return False
        assert _wait_gone(child_gone, 10.0), (
            f"child {child_pid} outlived its SIGKILLed spawner (PDEATHSIG not armed?)"
        )
    finally:
        for pid in (child_pid, wrapper.pid):
            if pid:
                try:
                    os.killpg(pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError, OSError):
                    pass


# ── 3b-posix. the run-lifetime reaper: the same guarantee where there is no
#              PDEATHSIG at all (darwin), pinned on every POSIX platform ─────

# A grandchild that IGNORES SIGTERM is what makes the next two tests
# discriminating rather than incidental. PDEATHSIG only ever signals the direct
# child, so on Linux the grandchild is merely orphaned and lives on; on darwin
# nothing signals anything. Only a group-wide TERM → wait → KILL sweep reaches
# it — which is precisely the real shape: `fauna-sync-agent` is the *app's*
# child, and the mail bridge's workers are the bridge's.
_TERM_IGNORING_GRANDCHILD = (
    "import signal, time;"
    "signal.signal(signal.SIGTERM, signal.SIG_IGN);"
    "signal.signal(signal.SIGHUP, signal.SIG_IGN);"
    "time.sleep(300)"  # sleep-ok: a toy child that hangs until the drill kills it
)
_SPAWNS_A_GRANDCHILD = (
    "import subprocess, sys, time;"
    f"subprocess.Popen([sys.executable, '-c', {_TERM_IGNORING_GRANDCHILD!r}]);"
    "time.sleep(300)"  # sleep-ok: same — the drill, not the clock, ends it
)


@pytest.mark.timeout(120)
@_posix_only
def test_parent_reaper_kills_every_registered_group_when_the_spawner_is_sigkilled():
    """The darwin half of point 9, and the fix for the 2026-08-22 macOS incident.

    macOS has neither PDEATHSIG nor job objects, so for months every darwin
    harness child was held only by `terminate_tree`/atexit — cleanup code, which
    a SIGKILLed pytest by definition never runs. Measured cost: an e2e trio (app
    + sync agent + nest) plus two mail bridges outlived their run by 4–7 h, all
    reparented to PID 1, holding two ports and ~375 MB while a live session kept
    accumulating more.

    Two armed groups, because the incident was never one stray: the reaper must
    reap EVERY group this run registered, not just the last. Each group holds a
    TERM-ignoring grandchild, so passing requires the full group-wide sweep —
    PDEATHSIG alone leaves those alive on Linux too, which is why this proof
    runs on every POSIX platform instead of only the one that needs it.
    """
    wrapper_src = textwrap.dedent(f"""
        import subprocess, sys, time
        sys.path.insert(0, {_E2E_DIR!r})
        from drivers.port_util import popen_group_kwargs
        pids = []
        for _ in range(2):
            p = subprocess.Popen(
                [sys.executable, '-c', {_SPAWNS_A_GRANDCHILD!r}],
                **popen_group_kwargs(),
            )
            pids.append(p.pid)
        print(' '.join(str(p) for p in pids), flush=True)
        time.sleep(300)  # sleep-ok: the run stays alive until the test kills it
    """)
    wrapper = subprocess.Popen(
        [sys.executable, "-c", wrapper_src], stdout=subprocess.PIPE, text=True,
    )
    pgids: list[int] = []
    try:
        pgids = [int(tok) for tok in wrapper.stdout.readline().split()]
        assert len(pgids) == 2, f"wrapper did not report two groups: {pgids!r}"

        # Causal barrier, not a settle-sleep: wait until each group actually
        # holds its child AND its grandchild, so the kill below cannot race the
        # grandchild's own spawn and pass for the wrong reason.
        assert _wait_gone(
            lambda: all(len(_pids_in_group(g)) >= 2 for g in pgids), 30.0
        ), f"grandchildren never appeared in {pgids!r} — nothing to prove yet"

        os.kill(wrapper.pid, signal.SIGKILL)  # no cleanup code ever runs
        wrapper.wait(timeout=10)

        # Generous ceiling: the reaper's own TERM → KILL escalation is 2 s.
        assert _wait_gone(
            lambda: not any(_pids_in_group(g) for g in pgids), 30.0
        ), (
            "process groups outlived the run that spawned them: "
            + repr({g: _pids_in_group(g) for g in pgids})
            + " — popen_group_kwargs must register every child's group with "
            "this run's reaper (drivers/port_util._start_reaper), the pipe-EOF "
            "mechanism that carries the die-with-the-run guarantee on a "
            "platform with no PDEATHSIG"
        )
    finally:
        for pgid in pgids:
            try:
                os.killpg(pgid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError, OSError):
                pass


@pytest.mark.timeout(90)
@_posix_only
def test_parent_reaper_signals_nothing_after_a_clean_exit():
    """Process safety: a run that tore itself down never signals a pgid again.

    The reaper holds bare process-group NUMBERS, and a pid the OS has recycled
    is indistinguishable from the one this run registered — so firing after a
    normal exit could take down a *concurrent, unrelated* run on the same box
    (point 9's scoping rule: only ever groups this run created). The clean path therefore sends `BYE` before closing the pipe and
    the reaper exits without signalling; only an EOF with no `BYE` — i.e. a run
    the kernel tore down — reaps.

    Asserted against a causal barrier rather than a settle-sleep (convention
    14): the reaper's own EXIT is the proof no signal can ever follow, so the
    test waits for that and only then reads the child's liveness.
    """
    wrapper_src = textwrap.dedent(f"""
        import subprocess, sys, time
        sys.path.insert(0, {_E2E_DIR!r})
        from drivers import port_util
        # Deliberately NOT track_process()'d: the atexit sweep must not be what
        # makes this pass or fail — the question is only what the reaper does.
        p = subprocess.Popen(
            [sys.executable, '-c', 'import time; time.sleep(300)'],  # sleep-ok: toy child; the test kills it
            **port_util.popen_group_kwargs(),
        )
        print(p.pid, port_util._reaper_proc.pid, flush=True)
        sys.exit(0)  # a CLEAN exit: atexit runs, so `BYE` is sent
    """)
    wrapper = subprocess.Popen(
        [sys.executable, "-c", wrapper_src], stdout=subprocess.PIPE, text=True,
    )
    child_pid = reaper_pid = None
    try:
        child_pid, reaper_pid = (int(t) for t in wrapper.stdout.readline().split())
        assert wrapper.wait(timeout=30) == 0, "wrapper did not exit cleanly"

        def reaper_gone():
            try:
                os.kill(reaper_pid, 0)
                return False
            except ProcessLookupError:
                return True
            except PermissionError:
                return False

        assert _wait_gone(reaper_gone, 30.0), (
            f"reaper {reaper_pid} outlived a clean exit — it must see `BYE`, "
            "stop, and never signal a pgid this run can no longer prove it owns"
        )
        # Barrier passed: the reaper is dead, so nothing can signal from here.
        assert _pids_in_group(child_pid), (
            f"the reaper signalled group {child_pid} on a CLEAN exit — that is "
            "the recycled-pgid hazard, and it can reach another session's run"
        )
    finally:
        for pid in (child_pid, reaper_pid):
            if pid:
                try:
                    os.killpg(pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError, OSError):
                    pass
                try:
                    os.kill(pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError, OSError):
                    pass


# ── 3b-win. the kill-on-close job reaps a grandchild of a killed spawner ────

def _win_pid_alive(pid: int) -> bool:
    """Liveness without signalling. NEVER `os.kill(pid, 0)` on Windows: signal 0
    maps to CTRL_C_EVENT there and takes down the whole console, this run
    included."""
    import ctypes
    from ctypes import wintypes

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    STILL_ACTIVE = 259
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenProcess.restype = wintypes.HANDLE
    handle = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return False
    code = wintypes.DWORD()
    ok = k32.GetExitCodeProcess(handle, ctypes.byref(code))
    k32.CloseHandle(handle)
    return bool(ok) and code.value == STILL_ACTIVE


@pytest.mark.timeout(60)
@pytest.mark.skipif(os.name != "nt", reason="job objects are the Windows mechanism")
def test_job_object_reaps_a_grandchild_of_a_killed_spawner():
    """The Windows twin of 3a **and** 3b at once — `reap_descendants_of`.

    A wrapper python (standing in for pytest) binds its child to a kill-on-close
    job; the child then spawns a **grandchild**; the wrapper is killed with NO
    cleanup code running. Both must die, because the OS closes the job handle
    with the wrapper.

    This is precisely the guarantee the fauna-tui leg rests on: it spawns
    `fauna-sync-agent`, a process *built* to outlive the app that started it
    (`sync-agent.md` § Packaging + lifecycle). Right in production, an orphan in
    a test — and one that then answers the next launch's pipe and holds its own
    .exe against the next build (observed 2026-07-24).
    """
    # Every `sleep(300)` below lives inside the CHILD or GRANDCHILD program
    # source: it is how those processes stay alive long enough to be killed —
    # the subject under test, not a wait for a condition. The only waiting this
    # test does is the deadline poll at the end.
    grandchild_src = (
        "import subprocess, sys, time\n"
        "p = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(300)'])\n"  # sleep-ok: the grandchild staying alive until the job kills it
        "print(p.pid, flush=True)\n"
        "time.sleep(300)\n"  # sleep-ok: the child staying alive until the job kills it
    )
    wrapper_src = textwrap.dedent(f"""
        import subprocess, sys, time
        sys.path.insert(0, {_E2E_DIR!r})
        from drivers.port_util import reap_descendants_of
        child = subprocess.Popen(
            [sys.executable, '-c', {grandchild_src!r}],
            stdout=subprocess.PIPE, text=True,
        )
        assert reap_descendants_of(child.pid), 'job assignment refused'
        # The grandchild is spawned AFTER the assignment — descendants join the
        # job automatically, which is the property under test.
        print(child.stdout.readline().strip(), flush=True)
        print(child.pid, flush=True)
        time.sleep(300)  # sleep-ok: the wrapper staying alive until the test kills it
    """)
    wrapper = subprocess.Popen(
        [sys.executable, "-c", wrapper_src], stdout=subprocess.PIPE, text=True,
    )
    grandchild_pid = child_pid = None
    try:
        grandchild_pid = int(wrapper.stdout.readline())
        child_pid = int(wrapper.stdout.readline())
        assert _win_pid_alive(grandchild_pid), "grandchild never started"

        wrapper.kill()  # no atexit, no fixture teardown — only the OS acts
        wrapper.wait(timeout=10)

        assert _wait_gone(lambda: not _win_pid_alive(child_pid), 15.0), (
            f"child {child_pid} outlived its killed spawner — the job handle "
            "did not close, or the process was never assigned"
        )
        assert _wait_gone(lambda: not _win_pid_alive(grandchild_pid), 15.0), (
            f"grandchild {grandchild_pid} outlived the job — a descendant "
            "spawned after assignment did not inherit it"
        )
    finally:
        for pid in (grandchild_pid, child_pid):
            if pid and _win_pid_alive(pid):
                subprocess.run(
                    ["taskkill", "/F", "/PID", str(pid)],
                    capture_output=True, check=False,
                )


# ── 3c. the full reap drill: SIGKILL a pytest mid-web-test, no orphans ──────

@pytest.mark.web
@pytest.mark.timeout(300)
@_posix_only
def test_reap_drill_sigkilled_pytest_leaves_no_orphans(tmp_path):
    """The incident's cleanup half, end-to-end: a child pytest run launches a
    real web driver (bridge + playwright node + Chromium), gets SIGKILLed
    mid-test (so NO fixture teardown / atexit runs), and the bridge's entire
    process group must be gone shortly after — PDEATHSIG delivers SIGTERM to
    the bridge, whose handler forwards it group-wide."""
    pidfile = tmp_path / "bridge.pid"
    child_test = tmp_path / "test_drill_child.py"
    child_test.write_text(textwrap.dedent(f"""
        import sys, time
        sys.path.insert(0, {_E2E_DIR!r})

        def test_launch_web_driver_and_hang():
            from drivers import create_driver
            d = create_driver("web")
            d.launch({{"url": "about:blank"}})
            with open({str(pidfile)!r}, "w") as f:
                f.write(str(d._bridge_proc.pid))
            time.sleep(600)  # hang so the parent can SIGKILL us mid-test
    """))
    child = subprocess.Popen(
        [sys.executable, "-m", "pytest", str(child_test),
         "-p", "no:cacheprovider", "--timeout=300"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    bridge_pid = None
    try:
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            if pidfile.exists() and pidfile.read_text().strip():
                bridge_pid = int(pidfile.read_text())
                break
            if child.poll() is not None:
                pytest.fail(
                    "drill child pytest exited before launching the driver:\n"
                    + child.stdout.read()[-3000:]
                )
            time.sleep(0.5)
        assert bridge_pid, "drill child never reported the bridge pid"
        assert _pids_in_group(bridge_pid), "bridge group empty before the kill?"

        os.kill(child.pid, signal.SIGKILL)  # the incident: pytest dies, no cleanup
        child.wait(timeout=10)

        assert _wait_gone(lambda: not _pids_in_group(bridge_pid), 20.0), (
            f"orphan survivors in bridge process group {bridge_pid}: "
            f"{_pids_in_group(bridge_pid)} — the web bridge subtree outlived "
            "its SIGKILLed pytest (PDEATHSIG or the bridge's group-forwarding "
            "signal handler regressed)"
        )
    finally:
        try:
            os.kill(child.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        if bridge_pid:
            try:
                os.killpg(bridge_pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError, OSError):
                pass


# ── 3c-win. the same drill on Windows: the job reaps the browser subtree ────

def _win_children_of(pid: int) -> list[int]:
    """Direct children of `pid` via a Toolhelp snapshot (no psutil in this env)."""
    import ctypes
    from ctypes import wintypes

    TH32CS_SNAPPROCESS = 0x00000002

    class PROCESSENTRY32(ctypes.Structure):
        _fields_ = [
            ("dwSize", wintypes.DWORD),
            ("cntUsage", wintypes.DWORD),
            ("th32ProcessID", wintypes.DWORD),
            ("th32DefaultHeapID", ctypes.POINTER(ctypes.c_ulong)),
            ("th32ModuleID", wintypes.DWORD),
            ("cntThreads", wintypes.DWORD),
            ("th32ParentProcessID", wintypes.DWORD),
            ("pcPriClassBase", ctypes.c_long),
            ("dwFlags", wintypes.DWORD),
            ("szExeFile", ctypes.c_char * 260),
        ]

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    snap = k32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if snap == wintypes.HANDLE(-1).value:
        return []
    kids: list[int] = []
    try:
        entry = PROCESSENTRY32()
        entry.dwSize = ctypes.sizeof(PROCESSENTRY32)
        if k32.Process32First(snap, ctypes.byref(entry)):
            while True:
                if entry.th32ParentProcessID == pid:
                    kids.append(entry.th32ProcessID)
                if not k32.Process32Next(snap, ctypes.byref(entry)):
                    break
    finally:
        k32.CloseHandle(snap)
    return kids


def _win_subtree_of(pid: int) -> list[int]:
    out, frontier = [], [pid]
    while frontier:
        cur = frontier.pop()
        for kid in _win_children_of(cur):
            if kid not in out:
                out.append(kid)
                frontier.append(kid)
    return out


@pytest.mark.web
@pytest.mark.timeout(300)
@pytest.mark.skipif(os.name != "nt", reason="the job object is the Windows mechanism")
def test_reap_drill_killed_pytest_leaves_no_orphans_win(tmp_path):
    """The Windows twin of 3c, and the end-to-end proof for `drivers/web.py`.

    3c's assertion is a *process group* emptying via PDEATHSIG, which has no
    Windows analogue — so before this, the platform whose orphans are hardest to
    see had no end-to-end web-bridge drill at all. Here a child pytest launches a
    real web driver (bridge + playwright node + Chromium), is TerminateProcess'd
    mid-test so NO fixture teardown or atexit runs, and the kill-on-close job the
    bridge spawn arms must take the bridge AND its whole browser subtree with it
    when the kernel closes the dead run's handles.
    """
    pidfile = tmp_path / "bridge.pid"
    child_test = tmp_path / "test_drill_child_win.py"
    child_test.write_text(textwrap.dedent(f"""
        import sys, time
        sys.path.insert(0, {_E2E_DIR!r})

        def test_launch_web_driver_and_hang():
            from drivers import create_driver
            d = create_driver("web")
            d.launch({{"url": "about:blank"}})
            with open({str(pidfile)!r}, "w") as f:
                f.write(str(d._bridge_proc.pid))
            time.sleep(600)  # sleep-ok: hang so the parent can kill us mid-test
    """))
    child = subprocess.Popen(
        [sys.executable, "-m", "pytest", str(child_test),
         "-p", "no:cacheprovider", "--timeout=300"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    bridge_pid, subtree = None, []
    try:
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            if pidfile.exists() and pidfile.read_text().strip():
                bridge_pid = int(pidfile.read_text())
                break
            if child.poll() is not None:
                pytest.fail(
                    "the child pytest exited before launching the web driver:\n"
                    + (child.stdout.read() or "")[-2000:]
                )
            time.sleep(0.5)
        assert bridge_pid, "the child never reported a bridge pid"

        subtree = _win_subtree_of(bridge_pid)
        assert subtree, (
            f"bridge {bridge_pid} reported no descendants — the browser subtree "
            "never materialised, so this drill would assert nothing"
        )

        child.kill()  # TerminateProcess: no teardown, no atexit, only the OS
        child.wait(timeout=15)

        assert _wait_gone(lambda: not _win_pid_alive(bridge_pid), 30.0), (
            f"web bridge {bridge_pid} outlived its killed run — `drivers/web.py` "
            "must arm reap_descendants_of on the bridge spawn"
        )
        assert _wait_gone(
            lambda: not any(_win_pid_alive(p) for p in subtree), 30.0
        ), (
            f"browser subtree {[p for p in subtree if _win_pid_alive(p)]} outlived "
            "the job — descendants of the bridge did not inherit it"
        )
    finally:
        for pid in [bridge_pid, *subtree]:
            if pid and _win_pid_alive(pid):
                subprocess.run(["taskkill", "/F", "/PID", str(pid)],
                               capture_output=True, check=False)


# ── 3d. a client driver's own teardown reaps the agent it spawned ───────────
#
# 3a proves `terminate_tree` works. These prove the client drivers actually USE
# it, which is a separate claim and the one that regressed: `drivers/macos.py`
# hand-rolled its teardown and leaked `fauna-sync-agent` on every native
# `--client macos` run (observed 2026-07-24: pid 29443, PPID 1, 52 minutes,
# launch dir `/private/tmp/fauna-e2e-macos-agent-*`). The orphan then mass-
# deleted the user's live files when tmp GC emptied its bound folder — 34
# deletes propagated to example.com.
#
# Two independent holes, one test each. Both are about the TEARDOWN contract,
# so they drive the real driver's `teardown()` with a stand-in child; no app
# binary, no nest, nothing to build.


def _spawn_fake_app(marker: str, *, agent_ignores_term: bool, app_exits: bool):
    """A stand-in for a launched client: a session/group leader that spawns an
    'agent' grandchild (announcing its pid via `marker`) exactly the way the
    real drivers do — `start_new_session=True`, agent inheriting the group.

    Spawned with the same kwargs `drivers/macos.py::launch` uses, so what the
    teardown faces here is what it faces in production.
    """
    trap = 'trap "" TERM; ' if agent_ignores_term else ""
    tail = "exit 0" if app_exits else "wait"
    app = (
        f"sh -c '{trap}echo $$ > {marker}; while true; do sleep 1; done' &\n"
        f"{tail}\n"
    )
    proc = subprocess.Popen(
        ["sh", "-c", app],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    # Causal barrier, not a settle-sleep: proceed only once the agent has
    # announced its pid, so the teardown below always faces a live grandchild.
    assert _wait_gone(lambda: os.path.exists(marker) and open(marker).read().strip(), 30.0), (
        "the stand-in agent never announced its pid"
    )
    return proc, int(open(marker).read().strip())


def _macos_driver_with(proc):
    from drivers.macos import MacosInProcessDriver

    driver = MacosInProcessDriver()
    driver._app_proc = proc
    return driver


def _pid_gone(pid: int):
    def check():
        try:
            os.kill(pid, 0)
            return False
        except ProcessLookupError:
            return True
        except PermissionError:
            return False
    return check


def _reap(pid: int, proc):
    for target in (lambda: os.kill(pid, signal.SIGKILL),
                   lambda: os.killpg(proc.pid, signal.SIGKILL)):
        try:
            target()
        except (ProcessLookupError, PermissionError, OSError):
            pass


@pytest.mark.timeout(120)
@_posix_only
def test_macos_teardown_reaps_the_agent_when_the_app_already_exited():
    """Hole 1 — the one that produced the real orphan.

    The teardown used to compute `os.getpgid(proc.pid)` INSIDE the try that
    swallows `ProcessLookupError`. Once the app has exited and been reaped (the
    driver's own health loop calls `poll()`, and a finished seat run ends with
    the app gone), `getpgid` raises and the whole `killpg` is skipped — so
    NOTHING is ever signalled and the agent survives at PPID 1.

    Note the agent here dies on SIGTERM, which is what the real
    `fauna-sync-agent` does (it registers only `tokio::signal::ctrl_c`, i.e.
    SIGINT, so SIGTERM keeps its default disposition — measured 2026-07-29).
    The orphan never *ignored* a signal; it never received one.
    """
    marker = f"/tmp/fauna-reap-proof-exited-{os.getpid()}.pid"
    proc, agent_pid = _spawn_fake_app(marker, agent_ignores_term=False, app_exits=True)
    try:
        assert _wait_gone(lambda: proc.poll() is not None, 30.0), "stand-in app never exited"
        _macos_driver_with(proc).teardown()
        assert _wait_gone(_pid_gone(agent_pid), 30.0), (
            f"agent {agent_pid} survived MacosInProcessDriver.teardown() after the app "
            "had already exited — the group signal was skipped, which is exactly how "
            "the native macOS seat orphaned fauna-sync-agent"
        )
    finally:
        _reap(agent_pid, proc)
        os.path.exists(marker) and os.unlink(marker)


@pytest.mark.timeout(120)
@_posix_only
def test_macos_teardown_reaps_a_term_ignoring_agent():
    """Hole 2 — no fix may assume a polite shutdown.

    Even when the group SIGTERM does go out, the old teardown escalated with
    `proc.kill()` (the APP only, single-process) and only on a wait timeout, so
    it never swept the GROUP with SIGKILL. A grandchild that ignores SIGTERM
    therefore outlived the run.
    """
    marker = f"/tmp/fauna-reap-proof-stubborn-{os.getpid()}.pid"
    proc, agent_pid = _spawn_fake_app(marker, agent_ignores_term=True, app_exits=False)
    try:
        _macos_driver_with(proc).teardown()
        assert _wait_gone(_pid_gone(agent_pid), 30.0), (
            f"TERM-ignoring agent {agent_pid} survived MacosInProcessDriver.teardown() — "
            "the teardown must finish with a group SIGKILL sweep"
        )
    finally:
        _reap(agent_pid, proc)
        os.path.exists(marker) and os.unlink(marker)


@pytest.mark.timeout(120)
@_posix_only
def test_tui_teardown_reaps_a_term_ignoring_agent():
    """The same contract for the tui driver — the client the mac seat runs.

    `drivers/tui.py` hand-rolled the identical shape (backend TERM, wait, then a
    single-process `kill()` only on timeout). It survived where macOS did not
    for a reason worth recording: the tui child owns a **pty**, so when the
    session leader exits the kernel SIGHUPs the foreground group, and that — not
    the teardown — was reaping the agent. Measured 2026-07-29: a grandchild
    trapping TERM alone dies, one trapping TERM *and* HUP survives. So the
    agent is one `nohup`-style disposition away from orphaning here too, which
    is unremarkable for a daemon built to outlive its parent.

    Hence TERM+HUP below: it isolates the teardown's own guarantee from the pty
    backstop. `PtyBackend`'s contract already says the backend may be handed
    straight to `terminate_tree`.
    """
    from drivers.pty_backend import spawn_pty
    from drivers.tui import TuiDriver

    marker = f"/tmp/fauna-reap-proof-tui-{os.getpid()}.pid"
    app = (
        f"sh -c 'trap \"\" TERM HUP; echo $$ > {marker}; while true; do sleep 1; done' &\n"
        "wait\n"
    )
    backend = spawn_pty(["sh", "-c", app], dict(os.environ), 24, 80, 8, 16)
    assert _wait_gone(lambda: os.path.exists(marker) and open(marker).read().strip(), 30.0), (
        "the stand-in agent never announced its pid"
    )
    agent_pid = int(open(marker).read().strip())
    driver = TuiDriver()
    driver._pty = backend
    try:
        driver.teardown()
        assert _wait_gone(_pid_gone(agent_pid), 30.0), (
            f"TERM-ignoring agent {agent_pid} survived TuiDriver.teardown() — the tui "
            "teardown must finish with a group SIGKILL sweep (terminate_tree)"
        )
    finally:
        _reap(agent_pid, backend)
        os.path.exists(marker) and os.unlink(marker)


# ── 3e. the NEST SPAWN adopts the die-with-the-run mechanism ────────────────

_TESTS_DIR = str(Path(__file__).resolve().parents[2])

# A stand-in nest: answers the one endpoint `wait_for_node` polls
# (`/api/v1/health` → 200), then stays alive. Driving the REAL
# `common.nest._spawn_and_wait` against this stub — rather than a real
# fauna-nest — keeps the proof a seconds-long process-lifetime question with no
# build in it, while still exercising the exact production spawn path whose
# reaping is the subject. What it does NOT prove: anything about fauna-nest's
# own shutdown behaviour, which is not what leaked.
_STUB_NEST_SRC = (
    "import sys, threading, time\n"
    "from http.server import BaseHTTPRequestHandler, HTTPServer\n"
    "class H(BaseHTTPRequestHandler):\n"
    "    def do_GET(self):\n"
    "        self.send_response(200)\n"
    "        self.end_headers()\n"
    "        self.wfile.write(b'ok')\n"
    "    def log_message(self, *a):\n"
    "        pass\n"
    "srv = HTTPServer(('127.0.0.1', int(sys.argv[1])), H)\n"
    "threading.Thread(target=srv.serve_forever, daemon=True).start()\n"
    "time.sleep(300)\n"  # sleep-ok: the stub nest staying alive until the run reaps it
)


def _spawned_nest_gone(pid: int) -> bool:
    if os.name == "nt":
        return not _win_pid_alive(pid)
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True
    except PermissionError:
        return False


@pytest.mark.timeout(120)
def test_nest_spawn_dies_with_a_killed_run(tmp_path):
    """A nest spawned by `_spawn_and_wait` dies with the run that spawned it,
    with NO cleanup code running — the point-9 guarantee applied to the spawn
    site that names the nest explicitly ("web bridge, nest, mail bridge, xvfb +
    desktop app").

    Regression: on Windows this spawn site adopted only `popen_group_kwargs()`,
    which is `{}` there by construction — so the nest's sole protection was the
    cooperative atexit sweep and it outlived any run that ended without one. The
    orphan then held its own pinned `fauna-nest-e2e-<hash>.exe` open, and because
    that name is deterministic per (features, profile) the NEXT run's
    `shutil.copy2` onto it failed with `[WinError 32] … used by another process`
    — attributed to `nest_binary`, i.e. presenting as a build break in a tree
    that builds fine (found on Windows 2026-08-02, cost a full ~13-minute run).
    """
    from drivers.port_util import find_free_port

    port = find_free_port()
    log_path = str(tmp_path / f"stub-nest-{port}.log")
    wrapper_src = textwrap.dedent(f"""
        import sys, time
        sys.path.insert(0, {_E2E_DIR!r})
        sys.path.insert(0, {_TESTS_DIR!r})
        from common.nest import _spawn_and_wait
        proc = _spawn_and_wait(
            [sys.executable, '-c', {_STUB_NEST_SRC!r}, {str(port)!r}],
            {port!r}, {log_path!r},
        )
        print(proc.pid, flush=True)
        time.sleep(300)  # sleep-ok: the run staying alive until the test kills it
    """)
    wrapper = subprocess.Popen(
        [sys.executable, "-c", wrapper_src], stdout=subprocess.PIPE, text=True,
    )
    nest_pid = None
    try:
        line = wrapper.stdout.readline().strip()
        assert line.isdigit(), (
            f"the stub nest never came up healthy (wrapper said {line!r}); "
            f"see {log_path}"
        )
        nest_pid = int(line)

        # No atexit, no fixture teardown, no signal handler — only the OS acts.
        if os.name == "nt":
            wrapper.kill()
        else:
            os.kill(wrapper.pid, signal.SIGKILL)
        wrapper.wait(timeout=10)

        assert _wait_gone(lambda: _spawned_nest_gone(nest_pid), 20.0), (
            f"nest {nest_pid} outlived the run that spawned it — "
            "`_spawn_and_wait` must arm this platform's die-with-the-run "
            "mechanism (linux: popen_group_kwargs/PDEATHSIG; darwin: the same "
            "call's reaper registration; windows: port_util.reap_descendants_of"
            "), not rely on the atexit sweep, which a killed run never reaches"
        )
    finally:
        if nest_pid and not _spawned_nest_gone(nest_pid):
            if os.name == "nt":
                subprocess.run(["taskkill", "/F", "/PID", str(nest_pid)],
                               capture_output=True, check=False)
            else:
                try:
                    os.kill(nest_pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError, OSError):
                    pass


# ── 3f. no spawn site arms only the POSIX half of the guarantee ─────────────

# Spawners that legitimately arm ONLY `popen_group_kwargs()`, with the reason.
# Exemptions are POSIX-only *by construction* — the spawned thing cannot exist
# on Windows — never "we haven't got to it yet": the whole failure mode this
# pins is that the Windows half is a SEPARATE call and therefore forgettable,
# and it stays invisible until a run on Windows pays for it.
_POSIX_ONLY_SPAWNERS = {
    "drivers/linux.py": "the linux app driver; the app it spawns is Linux-only",
    "drivers/session_bus.py": "spawns dbus-daemon — D-Bus is a Linux-only dep here",
    "drivers/secret_service.py": "spawns gnome-keyring-daemon on that bus — Linux-only",
    "helpers/tray_bus.py": "spawns the fake StatusNotifier watcher over D-Bus",
    # Both became VISIBLE to this pin on 2026-08-14, when they were upgraded from
    # a bare `start_new_session=True` to the shared `popen_group_kwargs()` spelling
    # (same setsid, plus PR_SET_PDEATHSIG). They spawn nothing that can exist on
    # Windows, so they are exempt by construction — not "not got to yet".
    "drivers/macos.py": "the macOS app driver; the .app bundle it spawns is macOS-only",
    "drivers/posix_pty.py": "spawns through a POSIX pty (pty/termios/fcntl) — no Windows path",
    "tests/test_x_display_lock_sweep.py": (
        "spawns `true` for a certainly-dead pid; the Xvfb lock sweep it drills is Linux-only"
    ),
}
# Not spawners: the mechanism's own definition, this file's toy wrappers, which
# deliberately arm one half at a time to prove each half separately, and a file
# that only NAMES the helper in a comment (the scan is textual).
_NOT_SPAWN_SITES = {
    "drivers/port_util.py",
    "tests/test_harness_self_termination.py",
    "tests/test_nest_mode_axis.py",
}


@pytest.mark.timeout(60)
def test_every_spawn_site_arms_the_windows_half_too():
    """Structural twin of 3e, covering the sites a live-process proof cannot
    cheaply reach (the mail bridge needs a built Go binary; the web bridge needs
    a browser).

    The bug class: on POSIX `popen_group_kwargs()` arms the kernel mechanism
    *inside* the `Popen` call, so it is impossible to spawn and forget. On
    Windows it returns `{}` and the mechanism is a SECOND call after the spawn
    (`reap_descendants_of`) — so every new spawn site silently defaults to
    POSIX-only, and the gap only ever surfaces as an orphan on Windows. That is what
    happened to the nest spawn site (`common/nest.py`, fixed 2026-08-02) after
    the same gap was fixed once for `drivers/tui.py` on 2026-07-24.
    """
    roots = {
        "": Path(_E2E_DIR),
        "common/": Path(_TESTS_DIR) / "common",
    }
    offenders, stale = [], []
    seen_exempt = set()
    for prefix, root in roots.items():
        for path in sorted(root.rglob("*.py")):
            rel = prefix + path.relative_to(root).as_posix()
            if rel in _NOT_SPAWN_SITES:
                continue
            src = path.read_text(encoding="utf-8", errors="replace")
            if "popen_group_kwargs(" not in src:
                continue
            if rel in _POSIX_ONLY_SPAWNERS:
                seen_exempt.add(rel)
                continue
            if "reap_descendants_of(" not in src:
                offenders.append(rel)

    stale = sorted(set(_POSIX_ONLY_SPAWNERS) - seen_exempt)
    assert not offenders, (
        "spawn site(s) arm only the POSIX half of point 9's die-with-the-run "
        f"guarantee: {offenders}. On Windows `popen_group_kwargs()` is `{{}}` — "
        "add `reap_descendants_of(proc.pid)` after the Popen (it is a no-op off "
        "Windows), or add the file to _POSIX_ONLY_SPAWNERS with the reason it "
        "can never run there."
    )
    assert not stale, (
        f"stale _POSIX_ONLY_SPAWNERS entries (no longer spawn anything): {stale}. "
        "Drop them — a rotted exemption list is how the next real gap hides."
    )


# ── 4. the remaining machine-wide lock is loud + bounded ────────────────────

@pytest.mark.timeout(60)
@_posix_only
def test_live_box_lock_bounded_wait_names_the_holder(tmp_path, monkeypatch, capsys):
    """`helpers/live_box_lock.acquire` waits loudly (holder pid + cwd) and
    fails with that diagnosis after the bound — never an infinite sleep.
    (flock treats two fds in one process as contending, so the second acquire
    here contends exactly like a sibling session's would.)"""
    monkeypatch.setenv("FAUNA_E2E_LIVE_BOX_LOCK_DIR", str(tmp_path))
    from helpers import live_box_lock

    url = "https://drill.example.invalid"
    fd = live_box_lock.acquire(url)
    assert fd is not None
    try:
        with pytest.raises(TimeoutError) as exc:
            live_box_lock.acquire(url, max_wait_s=3.0)
        msg = str(exc.value)
        assert str(os.getpid()) in msg, msg      # names the holder pid
        assert os.getcwd() in msg, msg           # names the holder's checkout (cwd)
        err = capsys.readouterr().err
        assert "waiting" in err and str(os.getpid()) in err  # loud while waiting
    finally:
        live_box_lock.release(fd)

    # Released → immediately acquirable again.
    fd2 = live_box_lock.acquire(url, max_wait_s=3.0)
    assert fd2 is not None
    live_box_lock.release(fd2)


# ── 5. binary builds are never charged to a test's own clock ─────────────────
#
# The unit half drives the LIVE parent-conftest module (the same object pytest
# loaded for this run — precedent: test_contacts.py imports it for
# MAIL_PRIMARY_DOMAIN); every mutation is snapshot/restored so the surrounding
# session keeps its real memo state. The end-to-end half runs a child pytest
# against the real conftest hooks with a stub `just` on PATH, so it needs no
# real toolchain, takes no machine-wide slot (FAUNA_SLOT_HELD_E2E reentrancy),
# and gives the same verdict at any fleet load.


def _e2e_conftest():
    import conftest as mod

    # `import conftest` must resolve to tests/e2e-unified/conftest.py (the
    # parent), not tests/e2e-unified/tests/conftest.py. Fail loudly if the
    # import machinery ever hands back the wrong one.
    assert hasattr(mod, "_prebuild_binaries"), (
        f"'import conftest' resolved to {mod.__file__!r}, which has no "
        "_prebuild_binaries — expected tests/e2e-unified/conftest.py"
    )
    return mod


@pytest.fixture
def prebuild_memo_isolated():
    """The live e2e conftest module with its build memo snapshotted + cleared."""
    mod = _e2e_conftest()
    saved = dict(mod._BINARY_BUILD_MEMO)
    mod._BINARY_BUILD_MEMO.clear()
    # Two hoists fire on fake sessions no test here means to build for: the
    # fauna-ffi load (any nest-touching session) and the windows sync-agent
    # build (any session whose apps include windows, since the 2026-09-21
    # isolated-agent default). Both run real subprocesses — `build-if-stale`
    # gates, cargo — and a unit test of the selection logic must never do that:
    # measured 2026-09-25, the sync-agent one pushed the installed-product and
    # seat-pair tests past their 60 s bound whenever the generated-file gates
    # were stale. A test pinning a hoist itself patches over its stub. The
    # cdylib's compile half (`_ensure_fauna_ffi_built`, the warm-pass job split
    # out of the load on 2026-09-29) is the same class of real subprocess.
    stubbed = ("_ensure_fauna_ffi_loaded", "_ensure_fauna_ffi_built", "_ensure_sync_agent_built")
    saved_fns = {name: getattr(mod, name) for name in stubbed}
    for name in stubbed:
        setattr(mod, name, lambda: None)
    try:
        yield mod
    finally:
        mod._BINARY_BUILD_MEMO.clear()
        mod._BINARY_BUILD_MEMO.update(saved)
        for name, fn in saved_fns.items():
            setattr(mod, name, fn)


class _FakeItem:
    def __init__(self, fixturenames=(), params=None, markers=()):
        self.fixturenames = list(fixturenames)
        if params is None:
            self.callspec = None
        else:
            import types

            self.callspec = types.SimpleNamespace(params=dict(params))
        self._markers = set(markers)

    def get_closest_marker(self, name):
        return object() if name in self._markers else None


def _fake_session(*items):
    import types

    return types.SimpleNamespace(items=list(items))


@pytest.mark.timeout(60)
def test_ensure_build_runs_once_and_memoizes(monkeypatch, prebuild_memo_isolated):
    """A second request for the same binary must not re-invoke `just` — the
    recipe's {{slot_build}} wrapper queues on the machine-wide build pool
    unconditionally, so an un-memoized re-invocation re-acquires the slot even
    on a fully warm tree."""
    mod = prebuild_memo_isolated
    calls = []

    def record(cmd, *a, **k):
        calls.append(list(cmd))

    monkeypatch.setattr(mod.subprocess, "run", record)
    path_one = mod._ensure_seal_helper_built()
    path_two = mod._ensure_seal_helper_built()
    assert path_one == path_two
    seal_calls = [c for c in calls if any("seal-helper-build" in part for part in c)]
    assert len(seal_calls) == 1, calls


@pytest.mark.timeout(60)
def test_ensure_build_failure_replays_without_requeueing(
    monkeypatch, prebuild_memo_isolated
):
    """A failed collection-time build must surface in the requesting fixture
    as the ORIGINAL failure — never a silent rebuild, which would re-queue the
    build slot inside the test's own 900 s budget (the exact inversion this
    mechanism removes)."""
    mod = prebuild_memo_isolated
    calls = []

    def run(cmd, *a, **k):
        calls.append(list(cmd))
        if any("seal-helper-build" in part for part in cmd):
            raise subprocess.CalledProcessError(2, cmd)

    monkeypatch.setattr(mod.subprocess, "run", run)
    with pytest.raises(subprocess.CalledProcessError):
        mod._ensure_seal_helper_built()
    with pytest.raises(RuntimeError, match="already failed"):
        mod._ensure_seal_helper_built()
    seal_calls = [c for c in calls if any("seal-helper-build" in part for part in c)]
    assert len(seal_calls) == 1, calls


@pytest.mark.timeout(60)
def test_prebuild_selection_is_lazy_and_exact(monkeypatch, prebuild_memo_isolated):
    """_prebuild_binaries builds exactly what the collected items' fixture
    closures + app parametrization will use — a run selecting no mail test
    never builds the bridge (laziness is part of the contract)."""
    mod = prebuild_memo_isolated
    built = []
    for fn in (
        "_ensure_nest_built",
        "_ensure_bluesky_nest_built",
        "_ensure_mail_bridge_built",
        "_ensure_seal_helper_built",
    ):
        monkeypatch.setattr(mod, fn, (lambda name: lambda: built.append(name))(fn))
    monkeypatch.setattr(
        mod, "_ensure_app_built", lambda name: built.append(f"app:{name}")
    )
    monkeypatch.setattr(
        mod, "get_available_apps", lambda: ["web", "linux", "tui", "windows"]
    )

    # No binary-building fixtures selected -> nothing built.
    mod._prebuild_binaries(_fake_session(_FakeItem(fixturenames=["tmp_path"])))
    assert built == []

    # A mail test on tui -> its exact closure builds, nothing else does.
    mod._prebuild_binaries(
        _fake_session(
            _FakeItem(
                fixturenames=["nest_binary", "seal_helper_binary", "nest_instance"],
                params={"app": "tui"},
            )
        )
    )
    assert "_ensure_nest_built" in built
    assert "_ensure_seal_helper_built" in built
    assert "app:tui" in built
    assert "_ensure_mail_bridge_built" not in built
    assert "_ensure_bluesky_nest_built" not in built
    # A param value that isn't an available app is not an app to build.
    assert not any(b.startswith("app:") and b != "app:tui" for b in built)


@pytest.mark.timeout(60)
def test_prebuild_hoists_the_mail_venue_binaries_dedicated_mail_nest_hides(
    monkeypatch, prebuild_memo_isolated
):
    """`dedicated_mail_nest` (and its three `_start_mail_venue` siblings) fetch
    `mail_bridge_binary`/`seal_helper_binary` via `request.getfixturevalue`
    inside the standalone provider, not as a declared parameter — invisible to
    `item.fixturenames`, so a test whose ONLY mail-venue fixture is one of
    these four must still trigger both builds at collection time. Reproduces
    the 2026-09-09 `test_addressbook.py` failure: a bare `Timeout (>900.0s)`
    on the mail-bridge `just` build, running inside the test's own budget
    because nothing in its closure named `mail_bridge_binary`."""
    mod = prebuild_memo_isolated
    built = []
    for fn in ("_ensure_mail_bridge_built", "_ensure_seal_helper_built", "_ensure_nest_built"):
        monkeypatch.setattr(mod, fn, (lambda name: lambda: built.append(name))(fn))
    monkeypatch.setattr(mod, "get_available_apps", lambda: ["web"])

    # Exactly what a real `dedicated_mail_nest` consumer's closure looks like:
    # the fixture itself, plus the ordinary `nest_mode`/`tmp_path_factory`
    # params it declares — never the binaries it fetches dynamically.
    mod._prebuild_binaries(
        _fake_session(
            _FakeItem(
                fixturenames=["dedicated_mail_nest", "nest_mode", "tmp_path_factory"],
                params={"app": "web"},
            )
        )
    )
    assert "_ensure_mail_bridge_built" in built
    assert "_ensure_seal_helper_built" in built

    # A run touching neither `dedicated_mail_nest` nor its three siblings must
    # still build neither binary (laziness is unaffected by the correction).
    built.clear()
    mod._prebuild_binaries(
        _fake_session(_FakeItem(fixturenames=["tmp_path"], params={"app": "web"}))
    )
    assert built == []


@pytest.mark.timeout(60)
def test_prebuild_windows_app_respects_installed_product(
    monkeypatch, prebuild_memo_isolated
):
    """The installer full-journey suite drives the MSI-installed app; its runs
    must not prebuild the dev app it will never launch (mirrors
    _build_app_config's carve-out)."""
    mod = prebuild_memo_isolated
    built = []
    monkeypatch.setattr(
        mod, "_ensure_app_built", lambda name: built.append(f"app:{name}")
    )
    monkeypatch.setattr(
        mod, "_ensure_nest_built", lambda: built.append("_ensure_nest_built")
    )
    monkeypatch.setattr(mod, "get_available_apps", lambda: ["windows"])

    marked = _FakeItem(
        fixturenames=["app"], params={"app": "windows"}, markers={"installed_product"}
    )
    mod._prebuild_binaries(_fake_session(marked))
    assert not any(b.startswith("app:") for b in built)

    built.clear()
    unmarked = _FakeItem(fixturenames=["app"], params={"app": "windows"})
    mod._prebuild_binaries(_fake_session(unmarked))
    assert built == ["_ensure_nest_built", "app:windows"]


@pytest.mark.timeout(60)
def test_prebuild_loads_fauna_ffi_for_a_run_that_touches_a_nest(
    monkeypatch, prebuild_memo_isolated
):
    """`fauna_ffi` builds its cdylib when it is first IMPORTED, and the
    headless seeding helpers import it lazily — `ApiActor.subscription_create_tier`
    does it inside the test body — so on a tree without the library the cargo
    build and its machine-wide `build`-slot wait ran inside that test's 900 s
    budget. Observed 2026-09-25 on the primary Linux dev VM: both
    `test_profile.py` offers tests died as bare `Timeout (>900.0s)` after
    `[fauna-ffi] libfauna_ffi.so not found — building` queued 14+ minutes,
    while the rest of the module passed. Any run
    touching a nest may seed through those helpers, so the load is hoisted to
    collection; a run touching no nest still loads nothing."""
    mod = prebuild_memo_isolated
    loaded = []
    monkeypatch.setattr(
        mod, "_ensure_fauna_ffi_loaded", lambda: loaded.append("ffi"), raising=False
    )
    for fn in ("_ensure_nest_built", "_ensure_app_built"):
        monkeypatch.setattr(mod, fn, lambda *a: None)
    monkeypatch.setattr(mod, "get_available_apps", lambda: ["tui"])

    mod._prebuild_binaries(_fake_session(_FakeItem(fixturenames=["tmp_path"])))
    assert loaded == []

    mod._prebuild_binaries(
        _fake_session(_FakeItem(fixturenames=["app"], params={"app": "tui"}))
    )
    assert loaded == ["ffi"]


@pytest.mark.timeout(60)
def test_prebuild_resolves_seat_pair_params_to_apps(
    monkeypatch, prebuild_memo_isolated
):
    """A convention-16 seat pair names its apps by seat MODE, not by app name,
    so the app-discovery scan has to resolve them.

    Regression pin for the exact bound inversion this whole layer exists to
    remove. The pair param is a TUPLE (``("native", "native")``), so the
    `isinstance(value, str)` app scan skipped it entirely, no app was hoisted,
    and `_make_seat` ran `just windows-debug` — MSBuild plus a machine-wide
    build-slot acquisition — inside `timeout = 900`. Observed 2026-08-02 on
    Windows: `[native+native]` died as a bare `Timeout` pointing at
    `subprocess.wait`, which reads exactly like a product bug while
    `[engine+engine]` passed in the same run (testing.md § convention 16).
    """
    mod = prebuild_memo_isolated
    built = []
    monkeypatch.setattr(
        mod, "_ensure_app_built", lambda name: built.append(f"app:{name}")
    )
    monkeypatch.setattr(
        mod, "_ensure_nest_built", lambda: built.append("_ensure_nest_built")
    )
    monkeypatch.setattr(
        mod,
        "get_available_apps",
        lambda: ["web", "linux", "tui", "windows", "macos"],
    )
    # This test lies about `sys.platform` (below), and `sweep_apps()` scans the
    # REAL box — a combination the stdlib stopped tolerating on Python 3.14:
    # `shutil.which` now branches on `sys.platform == "win32"` into `_winapi`,
    # which is None off Windows, so the macOS branch's `which("xcodebuild")`
    # raised `AttributeError` and this test failed on macOS alone. Stub the scan
    # out: it is bounded machine discovery, not this test's subject, and with
    # `fixturenames=()` no `_CROSS_APP_FIXTURE_APPS` hoist consults it anyway.
    monkeypatch.setattr(
        mod, "sweep_apps", lambda: ["web", "linux", "tui", "windows", "macos"]
    )

    # `native` resolves per platform, through sync_seats' single resolution
    # point — the same one `_make_seat` uses, so they cannot drift apart.
    monkeypatch.setattr(sys, "platform", "win32")
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"pair": ("native", "native")}))
    )
    assert built == ["app:windows"]

    built.clear()
    monkeypatch.setattr(sys, "platform", "darwin")
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"pair": ("native", "native")}))
    )
    assert built == ["app:macos"]

    # linux joined 2026-08-10, and this arm is the one that keeps the bound
    # inversion from re-entering on the box it was never observed on: the
    # primary Linux dev VM is where the default sweep runs, so an unhoisted
    # linux app seat would put `just linux-debug` — cargo plus a machine-wide
    # build-slot acquisition — back inside `timeout = 900` on the busiest
    # machine in the fleet.
    built.clear()
    monkeypatch.setattr(sys, "platform", "linux")
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"pair": ("native", "native")}))
    )
    assert built == ["app:linux"]

    # A tui pair builds the tui app; a mixed pair builds both halves.
    built.clear()
    monkeypatch.setattr(sys, "platform", "win32")
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"pair": ("tui", "tui")}))
    )
    assert built == ["app:tui"]

    built.clear()
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"pair": ("native", "tui")}))
    )
    assert sorted(built) == ["app:tui", "app:windows"]

    # A tuple naming a mode that is not a seat mode (the retired disk-only
    # `engine`) is not a seat pair at all, so it launches no app — laziness is
    # still exact.
    built.clear()
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"pair": ("engine", "engine")}))
    )
    assert built == []

    # A tuple holding an unhashable element (a parametrize case carrying a set)
    # is not a seat pair either, and must not crash collection: measured
    # 2026-10-03 on macOS, `TypeError: cannot use 'set' as a set element` in the
    # seat-mode subset check aborted a whole `--app tui` run as an INTERNALERROR.
    built.clear()
    mod._prebuild_binaries(
        _fake_session(_FakeItem(params={"case": ({"tui"}, "native")}))
    )
    assert built == []


def test_prebuild_hoists_apps_a_cross_app_fixture_launches(
    monkeypatch, prebuild_memo_isolated
):
    """A fixture launching a SECOND app is invisible to the parametrization scan.

    The third instance of this layer's one bound inversion, and the first where
    neither scan could ever have seen the app: `real_faunamls_linux_sender`
    launches a real-engine **linux** GUI to send at a **web** receiver, so the
    item is parametrized `[web]` and nothing names linux. Observed 2026-08-15:
    `test_conv_rail_push_wakes_web[web]` died in setup as a bare
    `Timeout (>900.0s)` on `['just', 'linux-debug']` after 1220 s queued at
    position 3 of 3 — a ~90-minute run spent to learn that the second app was
    never prebuilt. `test_fauna_mls_web_receives_from_linux_sender` had carried
    the same latent bug since it was written.
    """
    mod = prebuild_memo_isolated
    built = []
    monkeypatch.setattr(
        mod, "_ensure_app_built", lambda name: built.append(f"app:{name}")
    )
    # ⚠ `get_available_apps` is the run's `--app` SELECTION, and it is stubbed to
    # `["web"]` here on purpose — that is the real shape of the only run that
    # reaches this fixture (`--app web`), and the first cut of both the fix and
    # this pin got it wrong in the same direction: the fix guarded on
    # `implied in get_available_apps()`, the pin stubbed that list to include
    # linux, so the pin passed against a fix that hoists nothing in production.
    # A stub more generous than reality is a pin that agrees with the bug.
    monkeypatch.setattr(mod, "get_available_apps", lambda: ["web"])
    # The machine's drivable set is the correct bound, and the only one.
    monkeypatch.setattr(mod, "sweep_apps", lambda: ["web", "linux", "tui"])

    # The web item names only `web`; the fixture closure is what carries linux.
    mod._prebuild_binaries(
        _fake_session(
            _FakeItem(
                fixturenames=("real_faunamls_app", "real_faunamls_linux_sender"),
                params={"app": "web"},
            )
        )
    )
    assert built == ["app:linux", "app:web"], (
        "the cross-app sender's linux build must be hoisted to collection time "
        "alongside the parametrized app"
    )

    # Laziness is exact: no such fixture in the closure, no second app.
    built.clear()
    mod._prebuild_binaries(
        _fake_session(_FakeItem(fixturenames=("real_faunamls_app",), params={"app": "web"}))
    )
    assert built == ["app:web"]

    # And the machine bound still holds: a box that cannot drive linux (mac —
    # web/ios/macos/tui) must never be handed `just linux-debug`.
    built.clear()
    monkeypatch.setattr(mod, "sweep_apps", lambda: ["web", "ios", "macos", "tui"])
    mod._prebuild_binaries(
        _fake_session(
            _FakeItem(
                fixturenames=("real_faunamls_app", "real_faunamls_linux_sender"),
                params={"app": "web"},
            )
        )
    )
    assert built == ["app:web"]

    # Every declared fixture must exist, or the map is silently protecting
    # nothing — the inert-knob shape applied to a dict.
    conftest_src = (
        pathlib.Path(mod.__file__).parent / "conftest.py"
    ).read_text()
    for fixture_name in mod._CROSS_APP_FIXTURE_APPS:
        assert f"def {fixture_name}(" in conftest_src, (
            f"_CROSS_APP_FIXTURE_APPS names {fixture_name!r}, which is not a "
            "fixture in conftest.py — a renamed fixture takes its prebuild "
            "hoist with it and the timeout comes back"
        )


def test_every_cross_app_hoist_names_an_app_its_fixture_literally_launches():
    """Each `_CROSS_APP_FIXTURE_APPS` entry is pinned to the LITERAL app name its
    fixture hands `create_driver` — the table's own stated rule, enforced.

    The hoist exists for a fixture that launches an app the item's own
    parametrization can never name, and that is only true of a fixture naming
    the app *literally*. A fixture that derives its second seat from the
    SELECTED app (`create_driver(app_name)` after branching on
    `app.driver.is_tui()` and friends) launches the app the item is already
    parametrized by, so the parametrization scan has it covered — and an entry
    for such a fixture does not protect anything, it over-builds: it hoists a
    hard-named app the run will never launch.

    That is not hypothetical. `caldav_mailbox_less_attendee_app` was linux-only
    when it entered the table and grew tui/macos/ios arms afterwards, and the
    entry stayed. Measured 2026-09-02 on the primary Linux dev VM: a `--nest
    docker --app tui` run of `test_caldav_autoschedule_mailbox_less.py`
    opened with `[prebuild]
    app[linux]` → a cold `just linux-debug` plus a machine-wide build-slot wait,
    for an app whose driver the run never creates — on the exact verification
    path this journey's own arm re-runs.

    **Deliberately one-directional.** The reverse — every literal
    `create_driver("x")` caller must be in the table — is measurably wrong here:
    `tui_member` hard-names tui and belongs to nobody's table, because it
    `pytest.skip`s unless tui is in the run's own selection, so the app it names
    is always one the parametrization scan already saw. Its availability guard
    is the protection; a table entry would be a second mechanism for one fact.

    **The literal may sit one call up.** A shared launcher hands `create_driver`
    one of its OWN parameters (`_alice_extra_seat(app_name, …)` →
    `create_driver(app_name)`), and the fixture that names the app passes the
    literal to the launcher (`alice_builder_seats` → `_alice_extra_seat("macos",
    …)`). The scan follows the literal through that parameter's position, so a
    fixture built on a shared launcher is judged by the app it literally names,
    exactly as one calling `create_driver` itself — and a launcher called with
    a variable (`alice_second_device`) still reads as client-matched.
    """
    import ast

    import conftest as mod

    src = pathlib.Path(mod.__file__).read_text()
    functions = [
        node for node in ast.walk(ast.parse(src))
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    ]

    def _create_driver_arg(node):
        for call in ast.walk(node):
            if (isinstance(call, ast.Call) and isinstance(call.func, ast.Name)
                    and call.func.id == "create_driver" and call.args):
                return call.args[0]
        return None

    # Launchers: functions handing `create_driver` one of their own parameters,
    # keyed to that parameter's position.
    launcher_param: dict[str, int] = {}
    for node in functions:
        arg = _create_driver_arg(node)
        params = [a.arg for a in node.args.args]
        if isinstance(arg, ast.Name) and arg.id in params:
            launcher_param[node.name] = params.index(arg.id)

    literal_by_fixture: dict[str, object] = {}
    for node in functions:
        for call in ast.walk(node):
            if not (isinstance(call, ast.Call)
                    and isinstance(call.func, ast.Name) and call.args):
                continue
            if call.func.id == "create_driver":
                arg = call.args[0]
            elif (call.func.id in launcher_param
                    and len(call.args) > launcher_param[call.func.id]):
                arg = call.args[launcher_param[call.func.id]]
            else:
                continue
            literal_by_fixture[node.name] = (
                arg.value if isinstance(arg, ast.Constant) else None)

    wrong = {
        name: literal_by_fixture.get(name)
        for name, declared in mod._CROSS_APP_FIXTURE_APPS.items()
        if literal_by_fixture.get(name) != declared
    }
    assert not wrong, (
        f"these _CROSS_APP_FIXTURE_APPS entries do not match the app their "
        f"fixture literally launches: {wrong} (declared "
        f"{ {k: mod._CROSS_APP_FIXTURE_APPS[k] for k in wrong} }).\n"
        f"`None` means the fixture derives its app from the SELECTED one, so "
        f"the item's own parametrization already names it and the entry only "
        f"over-builds — drop the entry. A different literal means the fixture "
        f"changed which app it launches and the hoist was left behind, which is "
        f"the `Timeout (>900.0s)` on `subprocess.wait` this layer exists to "
        f"prevent."
    )


_STUB_JUST = """\
#!/usr/bin/env bash
echo "stub-just $*" >> "$STUB_JUST_LOG"
case "$1" in
  seal-helper-build|mail-bridge-build) sleep 12 ;;
esac
exit 0
"""

_INNER_CONFTEST = """\
import importlib.util
import sys

sys.path.insert(0, {e2e_dir!r})

_spec = importlib.util.spec_from_file_location(
    "fauna_e2e_conftest_real", {conftest_path!r}
)
_real = importlib.util.module_from_spec(_spec)
sys.modules["fauna_e2e_conftest_real"] = _real
_spec.loader.exec_module(_real)

# Re-export exactly the surface under test: the collection-time prebuild hook
# and the fixture chain a mail test would request.
pytest_collection_finish = _real.pytest_collection_finish
_generated_files_fresh = _real._generated_files_fresh
seal_helper_binary = _real.seal_helper_binary
"""

_INNER_TEST = """\
import pytest

pytestmark = pytest.mark.tier_3


def test_prebuilt_fixture_does_not_burn_the_test_clock(seal_helper_binary):
    assert isinstance(seal_helper_binary, str)
"""

_INNER_CONTROL_TEST = """\
import time

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="session")
def slow_setup():
    time.sleep(12)  # sleep-ok: the sleep IS the subject — a deliberately slow fixture proving setup time is charged to the child's per-test bound


def test_control(slow_setup):
    pass
"""


def _write_stub_just(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    stub = bin_dir / "just"
    stub.write_text(_STUB_JUST)
    stub.chmod(0o755)
    return bin_dir


def _inner_env(tmp_path, bin_dir):
    env = dict(os.environ)
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    env["STUB_JUST_LOG"] = str(tmp_path / "stub-just.log")
    # Reentrancy: the child run must not queue on the real machine-wide e2e
    # pool — this proof must give the same verdict at any fleet load.
    env["FAUNA_SLOT_HELD_E2E"] = "1"
    env["FAUNA_SLOT_HELD_BUILD"] = "1"
    return env


@pytest.mark.timeout(300)
@_posix_only
def test_fixture_build_is_charged_to_collection_not_the_test(tmp_path):
    """THE bound-inversion pin. A 12 s `just seal-helper-build` (stubbed —
    standing in for slot queue + compile) runs at collection time, so the test
    that requests the fixture passes under a 6 s per-test timeout. Reverting
    the conftest wiring puts the 12 s back into fixture setup, where the 6 s
    bound kills it — red for the right reason (see the control below, which
    proves setup time IS charged in this exact child configuration)."""
    bin_dir = _write_stub_just(tmp_path)
    conftest_path = str(Path(_E2E_DIR) / "conftest.py")
    (tmp_path / "conftest.py").write_text(
        _INNER_CONFTEST.format(e2e_dir=_E2E_DIR, conftest_path=conftest_path)
    )
    test_file = tmp_path / "test_probe.py"
    test_file.write_text(_INNER_TEST)

    result = subprocess.run(
        [sys.executable, "-m", "pytest", str(test_file),
         "--timeout=6", "-p", "no:cacheprovider", "-q"],
        capture_output=True, text=True, timeout=240,
        cwd=tmp_path, env=_inner_env(tmp_path, bin_dir),
    )
    assert result.returncode == 0, (
        f"child pytest failed — the fixture's build is being charged to the "
        f"test clock again?\n--- stdout ---\n{result.stdout[-4000:]}\n"
        f"--- stderr ---\n{result.stderr[-4000:]}"
    )
    assert "1 passed" in result.stdout, result.stdout[-2000:]

    # Built exactly once, at collection — the fixture replayed the memo
    # instead of re-invoking `just` (which would re-queue the build slot).
    log = (tmp_path / "stub-just.log").read_text()
    seal_lines = [l for l in log.splitlines() if "seal-helper-build" in l]
    assert len(seal_lines) == 1, log


@pytest.mark.timeout(300)
@_posix_only
def test_setup_time_is_still_charged_to_the_test_clock(tmp_path):
    """Control for the pin above: in the SAME child configuration, 12 s spent
    in an ordinary fixture's setup still trips a 6 s per-test timeout
    (pytest-timeout covers setup — testing.md point 9's 'never fix a slow test
    by flipping timeout_func_only'). Without this, the pin above could pass
    vacuously on a child config whose timeout ignores setup."""
    test_file = tmp_path / "test_control.py"
    test_file.write_text(_INNER_CONTROL_TEST)

    result = subprocess.run(
        [sys.executable, "-m", "pytest", str(test_file),
         "--timeout=6", "-p", "no:cacheprovider", "-q"],
        capture_output=True, text=True, timeout=240,
        cwd=tmp_path, env=dict(os.environ),
    )
    assert result.returncode != 0, result.stdout[-2000:]
    assert "Timeout" in result.stdout, result.stdout[-2000:]

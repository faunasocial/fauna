r"""tier_1: a linux or tui app that died of a crash before the harness closed it
fails the phase that found it.

Convention 6's close rider (`docs/goal/architecture/e2e-self-diagnosing-failures.md`
§ The convention), the Linux drivers' leg — windows' is
`test_windows_close_reads_exit.py`. Both drivers close their local child with
`terminate_tree` (SIGTERM to the group, then SIGKILL), which ends a dead app as
quietly as a live one and drops the handle the exit is read from, so the exit is
read BEFORE it (`drivers/local_launch.py::read_exit_before_close`). A crash is a
death by `SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL` or `SIGFPE` — a negative return
code, or `128 + n` through `xvfb-run`'s shell — or a Rust panic's exit 101; the
harness's own kill, a clean quit and an ordinary exit code stay silent.

Every handle and every `/proc` here is a fake; the real-app red is
`test_app_death_at_close_forced_local.py`.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import linux as linux_mod  # noqa: E402
from drivers import local_launch  # noqa: E402
from drivers import tui as tui_mod  # noqa: E402
from drivers.linux import LinuxBridgeDriver  # noqa: E402
from drivers.tui import TuiDriver  # noqa: E402
from helpers import app_deaths  # noqa: E402

pytestmark = pytest.mark.tier_1

if sys.platform == "win32":
    # The fixtures are POSIX signal deaths (a negative return code names the
    # signal, and `signal.SIGKILL` / `SIGHUP` do not exist on windows), while on
    # windows `local_launch.crash_exit_phrase` reads an NTSTATUS instead — so the
    # module cannot even import there, and its subject is the POSIX close path.
    pytest.skip(
        "environment: POSIX signal-death fixtures; windows reads an NTSTATUS",
        allow_module_level=True,
    )

PANIC = [
    "thread 'main' panicked at apps/fauna-tui/src/automation.rs:2230:9:",
    "FAUNA_E2E_FAULT_AT_RESET=panic: ending this launch on purpose",
    "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
]


class _Handle:
    """A Popen-compatible handle (`pid`, `poll`, `wait`, `kill`, `close`) that logs
    every call into `calls`, so a test can read the order of read and close."""

    def __init__(self, calls: list[str], rc: int | None, *, pid: int = 4242, wait_rc=None):
        self.calls, self.rc, self.pid, self.wait_rc = calls, rc, pid, wait_rc

    def poll(self):
        self.calls.append("poll")
        return self.rc

    def wait(self, timeout=None):
        self.calls.append("wait")
        if self.rc is None and self.wait_rc is None:
            raise subprocess.TimeoutExpired("fake", timeout)
        self.rc = self.rc if self.rc is not None else self.wait_rc
        return self.rc

    def kill(self):
        self.calls.append("kill")
        self.rc = -signal.SIGKILL

    def close(self):
        self.calls.append("close")


@pytest.fixture(autouse=True)
def _clean_registry(monkeypatch):
    """Each test reads what it recorded, and leaves nothing for the phase wrapper
    to raise on its behalf — nor a handle or launch marked reported."""
    monkeypatch.setattr(app_deaths, "_reported", set())
    app_deaths.drain()
    yield
    app_deaths.drain()


@pytest.fixture(autouse=True)
def _no_real_signal(monkeypatch):
    """No fake pid is ever signalled: `getpgid` fails, so `kill_uncleanly` falls back
    to the handle's own `kill()`, and a `killpg` that slips through fails the test."""

    def _getpgid(pid):
        raise ProcessLookupError(pid)

    def _killpg(*a):
        raise AssertionError(f"a fake pid was signalled: killpg{a}")

    monkeypatch.setattr(os, "getpgid", _getpgid)
    monkeypatch.setattr(os, "killpg", _killpg)


def _terminate(calls):
    def terminate_tree(handle, *a, **k):
        calls.append("terminate")
        if handle.rc is None:
            handle.rc = -signal.SIGTERM

    return terminate_tree


def _linux(monkeypatch, tmp_path, rc, *, wrapper=None, wait_rc=None):
    calls: list[str] = []
    d = LinuxBridgeDriver.__new__(LinuxBridgeDriver)
    d._app_proc = _Handle(calls, rc, wait_rc=wait_rc)
    d._app_wrapper = wrapper
    d._app_launched_at = 1000.0
    d._session_bus = None
    d._app_stdout = d._app_stderr = None
    d._tmp_dir = str(tmp_path)
    d._xdg_config = str(tmp_path / "config")
    (tmp_path / "app.err").write_text("\n".join(["[fauna] started", *PANIC]) + "\n")
    monkeypatch.setattr(linux_mod, "terminate_tree", _terminate(calls))
    monkeypatch.setattr(linux_mod, "untrack_process", lambda *a: calls.append("untrack"))
    return d, calls


def _tui(monkeypatch, tmp_path, rc):
    calls: list[str] = []
    d = TuiDriver.__new__(TuiDriver)
    d._pty = _Handle(calls, rc)
    d._app_launched_at = 1000.0
    d._app_stderr = None
    d._tmp_dir = str(tmp_path)
    d._xdg_config = str(tmp_path / "config")
    (tmp_path / "app.err").write_text("\n".join(PANIC) + "\n")
    monkeypatch.setattr(tui_mod, "terminate_tree", _terminate(calls))
    monkeypatch.setattr(tui_mod, "untrack_process", lambda *a: calls.append("untrack"))
    return d, calls


def _one_death() -> str:
    found = app_deaths.drain()
    assert len(found) == 1, found
    return found[0]


# ---- the read precedes the close, on every close path ------------------------


def test_linux_teardown_reads_the_exit_before_terminating_and_records_the_crash(monkeypatch, tmp_path):
    d, calls = _linux(monkeypatch, tmp_path, -signal.SIGSEGV)

    d._stop_app()

    assert calls.index("poll") < calls.index("terminate"), calls
    death = _one_death()
    first = death.splitlines()[0]
    assert first == "the app died before teardown closed it: fauna-desktop was killed by SIGSEGV (signal 11)", first
    assert "it was found dead at this close, +" in death, death
    assert "when it died is unread" in death, death
    assert "--- the app's panic / FATAL lines ---" in death, death
    assert "FAUNA_E2E_FAULT_AT_RESET=panic" in death, death
    # The close and its cleanup ran all the same.
    assert "untrack" in calls and d._app_proc is None, calls


def test_tui_teardown_reads_the_exit_before_terminating_and_records_the_panic(monkeypatch, tmp_path):
    d, calls = _tui(monkeypatch, tmp_path, 101)

    d._stop_app()

    assert calls.index("poll") < calls.index("terminate"), calls
    first = _one_death().splitlines()[0]
    assert first == (
        "the app died before teardown closed it: fauna-tui exited with code 101 — "
        "a Rust panic (the unwinding strategy's exit)"
    ), first
    assert "close" in calls and d._pty is None, calls


@pytest.mark.parametrize("app", ["linux", "tui"])
@pytest.mark.parametrize(
    ("label", "call"),
    [
        ("quit_app()", lambda d: d.quit_app()),
        ("kill_uncleanly()", lambda d: d.kill_uncleanly()),
    ],
)
def test_the_other_closes_read_first_too(monkeypatch, tmp_path, app, label, call):
    d, calls = (_linux if app == "linux" else _tui)(monkeypatch, tmp_path, -signal.SIGABRT)

    call(d)

    first_close = next(i for i, c in enumerate(calls) if c in ("terminate", "kill"))
    assert "poll" in calls[:first_close], calls
    first = _one_death().splitlines()[0]
    assert first.startswith(f"the app died before {label} closed it: "), first
    assert first.endswith("was killed by SIGABRT (signal 6)"), first


def test_a_killed_handle_kept_for_the_next_teardown_is_not_reported_twice(monkeypatch, tmp_path):
    """`kill_uncleanly` keeps its dead handle for the next teardown: a crash it found
    is one death, and the teardown's own SIGKILL'd read is silent."""
    d, calls = _linux(monkeypatch, tmp_path, -signal.SIGBUS)

    d.kill_uncleanly()
    d._stop_app()

    assert len(app_deaths.drain()) == 1


@pytest.mark.parametrize("app", ["linux", "tui"])
def test_a_relaunch_relaunches_and_still_records_the_death(monkeypatch, tmp_path, app):
    """`recover()` is how the `app` fixture gets past a reset the dead app never
    acknowledged. The relaunch still happens, the death is recorded once (its read
    in `recover()`, not again in the `_stop_app` inside it), and named as the
    relaunch's."""
    d, calls = (_linux if app == "linux" else _tui)(monkeypatch, tmp_path, -signal.SIGILL)
    d._launch_config = {"app_path": "/nowhere"}
    launched: list[dict] = []
    monkeypatch.setattr(d, "launch", lambda config: launched.append(config))

    assert d.recover() is True

    assert launched == [{"app_path": "/nowhere"}]
    first = _one_death().splitlines()[0]
    assert first.startswith("the app died before the relaunch in recover() closed it: "), first


# ---- every ending but a crash stays silent -----------------------------------


@pytest.mark.parametrize(
    "rc",
    [
        pytest.param(None, id="alive"),
        pytest.param(0, id="clean-quit"),
        pytest.param(73, id="ordinary-exit-code"),
        pytest.param(-signal.SIGTERM, id="sigterm"),
        pytest.param(-signal.SIGKILL, id="sigkill"),
        pytest.param(-signal.SIGHUP, id="pty-hangup"),
        pytest.param(139, id="139-without-a-wrapper-is-ordinary"),
    ],
)
@pytest.mark.parametrize("app", ["linux", "tui"])
def test_every_ending_but_a_crash_stays_silent(monkeypatch, tmp_path, app, rc):
    d, calls = (_linux if app == "linux" else _tui)(monkeypatch, tmp_path, rc)

    d._stop_app()

    assert app_deaths.drain() == []
    assert "terminate" in calls, calls


def test_a_driver_that_never_launched_reads_nothing(monkeypatch, tmp_path):
    d, calls = _linux(monkeypatch, tmp_path, None)
    d._app_proc = None
    d._stop_app()
    assert app_deaths.drain() == [] and calls == []


def test_an_exit_the_launch_gate_reported_is_not_reported_again(monkeypatch, tmp_path):
    """The launch gate's FAULTED verdict already carries an early exit."""
    d, calls = _tui(monkeypatch, tmp_path, 101)
    local_launch.mark_exit_reported(d._pty)
    d._stop_app()
    assert app_deaths.drain() == []


# ---- the xvfb-run wrapper ----------------------------------------------------


def test_a_signal_through_xvfb_run_reads_as_128_plus_n(monkeypatch, tmp_path):
    d, _calls = _linux(monkeypatch, tmp_path, 128 + signal.SIGSEGV, wrapper="xvfb-run")
    d._stop_app()
    first = _one_death().splitlines()[0]
    assert first == (
        "the app died before teardown closed it: fauna-desktop was killed by SIGSEGV "
        "(signal 11), read as exit code 139 through xvfb-run"
    ), first


def test_sigterm_through_xvfb_run_is_silent(monkeypatch, tmp_path):
    d, _calls = _linux(monkeypatch, tmp_path, 128 + signal.SIGTERM, wrapper="xvfb-run")
    d._stop_app()
    assert app_deaths.drain() == []


def _stat(pid: int, comm: str, ppid: int, state: str = "S", exit_status: int = 0) -> str:
    # Fields from the state on: state, ppid, then up to field 52 (exit_code).
    rest = [state, str(ppid), *["0"] * 47, str(exit_status)]
    return f"{pid} ({comm}) {' '.join(rest)}\n"


def _proc_root(tmp_path: Path, procs: list[tuple[int, str, int, str, int]]) -> str:
    root = tmp_path / "proc"
    for pid, comm, ppid, state, status in procs:
        (root / str(pid)).mkdir(parents=True)
        (root / str(pid) / "stat").write_text(_stat(pid, comm, ppid, state, status))
    return str(root)


def test_a_live_wrapper_over_a_zombie_app_reads_the_apps_own_status(tmp_path):
    """Between the app's death and the wrapper's `wait`, `/proc` still holds the app
    as a zombie whose `exit_code` field is its `waitpid` status."""
    calls: list[str] = []
    handle = _Handle(calls, None, pid=10)
    root = _proc_root(tmp_path, [(10, "xvfb-run", 1, "S", 0), (11, "fauna-desktop", 10, "Z", signal.SIGSEGV)])

    local_launch.read_exit_before_close(
        handle, close="teardown", comm="fauna-desktop", wrapper="xvfb-run", proc_root=root
    )

    first = _one_death().splitlines()[0]
    assert first == "the app died before teardown closed it: fauna-desktop was killed by SIGSEGV (signal 11)"
    assert "wait" not in calls, calls


def test_a_live_wrapper_over_a_live_app_is_silent_and_waits_for_nothing(tmp_path):
    calls: list[str] = []
    handle = _Handle(calls, None, pid=10)
    root = _proc_root(tmp_path, [(10, "xvfb-run", 1, "S", 0), (11, "fauna-desktop", 10, "S", 0)])

    local_launch.read_exit_before_close(
        handle, close="teardown", comm="fauna-desktop", wrapper="xvfb-run", proc_root=root
    )

    assert app_deaths.drain() == []
    assert "wait" not in calls, calls


def test_a_live_wrapper_with_its_app_gone_hands_back_the_apps_status(tmp_path):
    calls: list[str] = []
    handle = _Handle(calls, None, pid=10, wait_rc=128 + signal.SIGABRT)
    root = _proc_root(tmp_path, [(10, "xvfb-run", 1, "S", 0)])

    local_launch.read_exit_before_close(
        handle, close="teardown", comm="fauna-desktop", wrapper="xvfb-run", proc_root=root
    )

    first = _one_death().splitlines()[0]
    assert first.endswith("was killed by SIGABRT (signal 6), read as exit code 134 through xvfb-run"), first


def test_a_wrapper_that_never_settles_is_silent(tmp_path):
    calls: list[str] = []
    handle = _Handle(calls, None, pid=10)  # wait() times out
    root = _proc_root(tmp_path, [(10, "xvfb-run", 1, "S", 0)])

    local_launch.read_exit_before_close(
        handle, close="teardown", comm="fauna-desktop", wrapper="xvfb-run", proc_root=root, settle_s=0.01
    )

    assert app_deaths.drain() == []


# ---- the classifier, and the windows rule it shares ----------------------------


@pytest.mark.parametrize(
    ("rc", "nt", "crash"),
    [
        (-signal.SIGSEGV, False, True),
        (-signal.SIGFPE, False, True),
        (101, False, True),
        (101, True, True),
        (0xC0000005 - 2**32, True, True),
        (-1, True, False),
        (0xC000013A - 2**32, True, False),
        (73, True, False),
        (0, False, False),
    ],
)
def test_the_classifier(rc, nt, crash):
    assert bool(local_launch.crash_exit_phrase(rc, nt=nt)) is crash


def test_windows_reads_the_same_ntstatus_rule():
    from drivers import windows

    assert windows._is_crash_exit is local_launch.is_ntstatus_crash


def test_a_diagnosis_that_cannot_be_read_still_records(tmp_path):
    def broken():
        raise OSError("gone")

    handle = _Handle([], -signal.SIGSEGV)
    local_launch.read_exit_before_close(
        handle, close="teardown", comm="fauna-tui", stderr_text=broken, log_text=broken
    )
    death = _one_death()
    assert death.startswith("the app died before teardown closed it: fauna-tui was killed by SIGSEGV")
    assert "--- app stderr: empty ---" in death, death

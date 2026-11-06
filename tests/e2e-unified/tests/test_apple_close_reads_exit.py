r"""tier_1: a macOS or iOS app that died of a crash before the harness closed it
fails the phase that found it.

Convention 6's close rider (`docs/goal/architecture/e2e-self-diagnosing-failures.md`
§ The convention), the apple drivers' leg — linux's and tui's is
`test_local_close_reads_exit.py`, windows' `test_windows_close_reads_exit.py`.

**macOS** spawns its app and holds the Popen, so its closes read the exit through
the shared `drivers/local_launch.py::read_exit_before_close`: a death by a crash
signal (`SIGTRAP` is Swift's `fatalError` on arm64) or a Rust panic's 101 is a
crash; a live app, a kill, a clean quit and an ordinary code are silent. Its
Launch Services launch, and every **iOS** launch, leave the harness only a pid
(launchd's child; `simctl`'s process): there a gone pid is a crash only on a
witness of one — the app's FATAL line since its launch, or the crash report
ReportCrash wrote naming the pid — and a gone pid with neither prints UNKNOWN and
fails nothing (`read_gone_pid_before_close`).

The stand-in processes are real `sleep`s (a live pid, and a gone one reaped by
the test), every handle that "crashed" and every crash report is a fake: nothing
here crashes a real process. The real-app red is
`test_app_death_at_close_forced_apple.py`.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import inprocess_agent as inprocess_mod  # noqa: E402
from drivers import ios as ios_mod  # noqa: E402
from drivers import local_launch  # noqa: E402
from drivers import macos as macos_mod  # noqa: E402
from drivers.ios import IosInProcessDriver  # noqa: E402
from drivers.macos import MacosInProcessDriver, _LaunchedApp  # noqa: E402
from drivers.port_util import popen_group_kwargs, reap_descendants_of  # noqa: E402
from helpers import app_deaths  # noqa: E402

pytestmark = pytest.mark.tier_1

if sys.platform == "win32":
    # The fixtures are POSIX signal deaths of the apple apps (a negative return
    # code names the signal, and `signal.SIGKILL` / `SIGTRAP` do not exist on
    # windows), so the module cannot even import there; the apps it drives are
    # the macOS and iOS ones.
    pytest.skip(
        "environment: POSIX signal-death fixtures of the apple apps",
        allow_module_level=True,
    )

FATAL ="2026-11-05T08:00:00.000000Z ERROR fauna.e2e: [fauna] FATAL FAUNA_E2E_FAULT_AT_RESET=abort: crashing"


class _Handle:
    """A Popen-compatible handle that logs `poll` into `calls`, so a test reads the
    order of the read and the close."""

    def __init__(self, calls: list[str], rc: int | None, *, pid: int = 4242):
        self.calls, self.rc, self.pid = calls, rc, pid

    def poll(self):
        self.calls.append("poll")
        return self.rc

    def wait(self, timeout=None):
        return self.rc


@pytest.fixture(autouse=True)
def _clean(monkeypatch, tmp_path):
    """Each test reads what it recorded and leaves nothing marked reported; crash
    reports are read from a fake directory, with no settle wait."""
    monkeypatch.setattr(app_deaths, "_reported", set())
    monkeypatch.setattr(local_launch, "_pids_reported", set())
    reports = tmp_path / "DiagnosticReports"
    reports.mkdir()
    monkeypatch.setattr(local_launch, "DIAGNOSTIC_REPORTS_DIR", reports)
    monkeypatch.setattr(local_launch, "CRASH_REPORT_SETTLE_S", 0.0)
    app_deaths.drain()
    yield
    app_deaths.drain()


def _write_ips(tmp_path: Path, proc_name: str, pid: int, *, signal_name: str = "SIGTRAP",
               asi: str = "Fatal error: [fauna] FATAL FAUNA_E2E_FAULT_AT_RESET=abort") -> Path:
    """A crash report in darwin's `.ips` shape: a one-line JSON header, a JSON body."""
    path = tmp_path / "DiagnosticReports" / f"{proc_name}-2026-11-05-080000.ips"
    header = json.dumps({"app_name": proc_name, "bug_type": "309"})
    body = json.dumps({
        "pid": pid,
        "procName": proc_name,
        "exception": {"type": "EXC_BREAKPOINT", "signal": signal_name},
        "termination": {"namespace": "SIGNAL", "indicator": "Trace/BPT trap: 5"},
        "asi": {"libswiftCore.dylib": [asi]},
        "faultingThread": 0,
        "threads": [{"frames": [
            {"symbol": "_assertionFailure(_:_:file:line:flags:)"},
            {"symbol": "static TestAgentBootstrap.faultAtResetIfAsked()"},
            {"imageIndex": 3, "imageOffset": 1234},
        ]}],
    }, indent=2)
    path.write_text(header + "\n" + body)
    return path


@pytest.fixture
def sleeper():
    proc = subprocess.Popen(["sleep", "60"], **popen_group_kwargs())
    reap_descendants_of(proc.pid)
    try:
        yield proc
    finally:
        proc.kill()
        proc.wait()


@pytest.fixture
def gone_pid():
    """A pid that was a real process and is gone now — reaped by this test, so
    `kill(pid, 0)` answers no such process."""
    proc = subprocess.Popen(["sleep", "0"], **popen_group_kwargs())
    reap_descendants_of(proc.pid)
    proc.wait()
    return proc.pid


# --------------------------------------------------------------------------
# macOS — the spawned app's handle
# --------------------------------------------------------------------------


def _macos(monkeypatch, tmp_path, proc, calls: list[str]) -> MacosInProcessDriver:
    drv = object.__new__(MacosInProcessDriver)
    drv._app_proc = proc
    drv._app_launched_at = 1.0
    drv._tmp_dir = str(tmp_path)
    drv._app_support = str(tmp_path / "AS")
    drv._log_baseline = {}
    drv._keychain_path = None
    drv._launch_mode = "binary"
    drv._bundle_path = None
    monkeypatch.setattr(macos_mod, "terminate_tree", lambda p: calls.append("terminate"))
    monkeypatch.setattr(macos_mod, "untrack_process", lambda p: None)
    return drv


def test_macos_teardown_reads_the_exit_before_terminating_and_records_the_trap(monkeypatch, tmp_path):
    calls: list[str] = []
    proc = _Handle(calls, -signal.SIGTRAP)
    (tmp_path / "app.err").write_text("starting\n" + FATAL + "\n")
    _write_ips(tmp_path, "FaunaMacOS", proc.pid)
    _macos(monkeypatch, tmp_path, proc, calls)._stop_app()

    assert calls[:2] == ["poll", "terminate"], calls
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert found[0].splitlines()[0] == (
        "the app died before teardown closed it: FaunaMacOS was killed by SIGTRAP (signal 5)"
    )
    assert "FAUNA_E2E_FAULT_AT_RESET=abort" in found[0], found[0]
    assert "--- crash report " in found[0] and "EXC_BREAKPOINT" in found[0], found[0]


def test_macos_a_crash_with_no_report_yet_states_the_absence(monkeypatch, tmp_path):
    calls: list[str] = []
    _macos(monkeypatch, tmp_path, _Handle(calls, -signal.SIGSEGV), calls)._stop_app()
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert "crash report: none naming pid 4242" in found[0], found[0]


def test_macos_quit_reads_first(monkeypatch, tmp_path):
    calls: list[str] = []
    drv = _macos(monkeypatch, tmp_path, _Handle(calls, -signal.SIGABRT), calls)
    monkeypatch.setattr(drv, "_post", lambda path, body: calls.append(f"post {path}"))
    drv.quit_app()
    assert calls == ["poll", "post /app/lifecycle/quit"], calls
    assert app_deaths.drain()[0].startswith("the app died before quit_app() closed it: FaunaMacOS")


def test_macos_relaunch_records_the_death_once(monkeypatch, tmp_path):
    calls: list[str] = []
    proc = _Handle(calls, -signal.SIGTRAP)
    drv = _macos(monkeypatch, tmp_path, proc, calls)
    drv._launch_config = {"app_path": "x"}
    launched: list[dict] = []

    def fake_launch(config):
        launched.append(config)
        drv._app_proc = proc  # the same dead handle met again by the next close

    monkeypatch.setattr(drv, "launch", fake_launch)
    assert inprocess_mod.InProcessAgentDriver.recover(drv) is True
    drv._stop_app()
    found = app_deaths.drain()
    assert launched == [{"app_path": "x"}]
    assert len(found) == 1, found
    assert found[0].startswith("the app died before the relaunch in recover() closed it: FaunaMacOS")


@pytest.mark.parametrize(
    "rc", [None, -signal.SIGKILL, -signal.SIGTERM, 0, 3], ids=["live", "kill", "term", "clean", "code"]
)
def test_macos_every_ending_but_a_crash_stays_silent(monkeypatch, tmp_path, rc):
    calls: list[str] = []
    _macos(monkeypatch, tmp_path, _Handle(calls, rc), calls)._stop_app()
    assert app_deaths.drain() == []


def test_macos_a_rust_panic_exit_is_a_crash(monkeypatch, tmp_path):
    calls: list[str] = []
    _macos(monkeypatch, tmp_path, _Handle(calls, 101), calls)._stop_app()
    assert "exited with code 101" in app_deaths.drain()[0]


def test_macos_no_handle_reads_nothing(monkeypatch, tmp_path):
    _macos(monkeypatch, tmp_path, None, [])._stop_app()
    assert app_deaths.drain() == []


def test_macos_an_exit_the_launch_gate_reported_is_not_reported_again(monkeypatch, tmp_path):
    calls: list[str] = []
    proc = _Handle(calls, -signal.SIGTRAP)
    local_launch.mark_exit_reported(proc)
    _macos(monkeypatch, tmp_path, proc, calls)._stop_app()
    assert app_deaths.drain() == []


def test_macos_spawn_gate_marks_an_early_exit_reported(monkeypatch, tmp_path):
    calls: list[str] = []
    drv = _macos(monkeypatch, tmp_path, _Handle(calls, -signal.SIGTRAP), calls)
    monkeypatch.setattr(drv, "_launch_timeout_report", lambda *a, **k: "FAULTED")
    with pytest.raises(RuntimeError, match="FAULTED"):
        drv._await_spawned_app()
    drv._stop_app()
    assert app_deaths.drain() == []


# --------------------------------------------------------------------------
# macOS — the Launch Services launch: a pid, no exit code
# --------------------------------------------------------------------------


def test_macos_launch_services_gone_pid_with_a_report_is_a_crash(monkeypatch, tmp_path, gone_pid):
    calls: list[str] = []
    _write_ips(tmp_path, "FaunaMacOS", gone_pid)
    _macos(monkeypatch, tmp_path, _LaunchedApp(gone_pid), calls)._stop_app()
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert found[0].splitlines()[0].startswith(
        f"the app died before teardown closed it: FaunaMacOS (pid {gone_pid}) was gone before "
        "the close, a crash by its crash report"
    ), found[0]
    assert "launchd's child" in found[0]


def test_macos_launch_services_gone_pid_alone_is_unknown(monkeypatch, tmp_path, gone_pid, capsys):
    _macos(monkeypatch, tmp_path, _LaunchedApp(gone_pid), [])._stop_app()
    assert app_deaths.drain() == []
    assert "app exit UNKNOWN before teardown" in capsys.readouterr().out


def test_macos_launch_services_live_pid_is_silent(monkeypatch, tmp_path, sleeper, capsys):
    _write_ips(tmp_path, "FaunaMacOS", sleeper.pid)  # a live pid's report is never read
    _macos(monkeypatch, tmp_path, _LaunchedApp(sleeper.pid), [])._read_exit_before_close("teardown")
    assert app_deaths.drain() == []
    assert "UNKNOWN" not in capsys.readouterr().out


# --------------------------------------------------------------------------
# iOS — the pid `simctl launch` printed
# --------------------------------------------------------------------------


def _ios(monkeypatch, tmp_path, pid, calls: list[str], log: str = "") -> IosInProcessDriver:
    drv = object.__new__(IosInProcessDriver)
    drv._app_pid = pid
    drv._app_launched_at = 1.0
    support = tmp_path / "container" / "Library" / "Application Support"
    (support / "logs").mkdir(parents=True)
    if log:
        (support / "logs" / "fauna.log.2026-11-05").write_text(log)
    drv._app_support = str(support)
    drv._log_baseline = {}
    drv._launch_config = {"udid": "FAKE-UDID"}

    def run_simctl(cmd, **kwargs):
        calls.append(" ".join(cmd[2:4]))
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(ios_mod, "_run_simctl", run_simctl)
    return drv


def test_ios_teardown_reads_the_gone_pid_before_terminating(monkeypatch, tmp_path, gone_pid):
    calls: list[str] = []
    _write_ips(tmp_path, "FaunaiOS", gone_pid)
    drv = _ios(monkeypatch, tmp_path, gone_pid, calls)
    real_read = drv._read_exit_before_close
    monkeypatch.setattr(
        drv, "_read_exit_before_close", lambda close: (calls.append("read"), real_read(close))
    )
    drv._stop_app()
    assert calls == ["read", "terminate FAKE-UDID"], calls
    found = app_deaths.drain()
    assert len(found) == 1, found
    first = found[0].splitlines()[0]
    assert first.startswith(
        f"the app died before teardown closed it: FaunaiOS (pid {gone_pid}) was gone before the "
        "close, a crash by its crash report"
    ), first
    assert "no exit code is readable (`simctl` owns" in first, first
    assert "Fatal error: [fauna] FATAL FAUNA_E2E_FAULT_AT_RESET=abort" in found[0], found[0]
    assert "  static TestAgentBootstrap.faultAtResetIfAsked()" in found[0], found[0]
    assert "  <image 3+1234>" in found[0], found[0]


def test_ios_a_fatal_line_in_this_launchs_log_is_a_crash(monkeypatch, tmp_path, gone_pid):
    _ios(monkeypatch, tmp_path, gone_pid, [], log="booting\n" + FATAL + "\n")._stop_app()
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert "a crash by a FATAL/panic line in its log" in found[0].splitlines()[0], found[0]


def test_ios_a_predecessors_fatal_below_the_floor_is_not_this_launchs(monkeypatch, tmp_path, gone_pid, capsys):
    drv = _ios(monkeypatch, tmp_path, gone_pid, [], log=FATAL + "\n")
    log = Path(drv._app_support) / "logs" / "fauna.log.2026-11-05"
    drv._log_baseline = {str(log): log.stat().st_size}
    drv._stop_app()
    assert app_deaths.drain() == []
    assert "app exit UNKNOWN" in capsys.readouterr().out


def test_ios_a_gone_pid_alone_is_unknown_once_and_fails_nothing(monkeypatch, tmp_path, gone_pid, capsys):
    drv = _ios(monkeypatch, tmp_path, gone_pid, [])
    drv._read_exit_before_close("the relaunch in recover()")
    drv._stop_app()
    assert app_deaths.drain() == []
    out = capsys.readouterr().out
    assert out.count("app exit UNKNOWN") == 1, out
    assert f"FaunaiOS (pid {gone_pid}) was gone" in out and "not failed" in out, out


def test_ios_a_live_pid_is_silent(monkeypatch, tmp_path, sleeper, capsys):
    _write_ips(tmp_path, "FaunaiOS", sleeper.pid)
    _ios(monkeypatch, tmp_path, sleeper.pid, [])._read_exit_before_close("teardown")
    assert app_deaths.drain() == []
    assert "UNKNOWN" not in capsys.readouterr().out


def test_ios_no_pid_reads_nothing(monkeypatch, tmp_path, capsys):
    _ios(monkeypatch, tmp_path, None, [])._stop_app()
    assert app_deaths.drain() == []
    assert "UNKNOWN" not in capsys.readouterr().out


def test_ios_kill_uncleanly_reads_first_and_its_own_kill_stays_silent(monkeypatch, tmp_path, gone_pid):
    signals: list[int] = []

    def fake_kill(pid, sig):
        # The probe (`kill(pid, 0)`) answers gone; the SIGKILL is recorded, never sent.
        signals.append(sig)
        raise ProcessLookupError

    _write_ips(tmp_path, "FaunaiOS", gone_pid)
    drv = _ios(monkeypatch, tmp_path, gone_pid, [])
    monkeypatch.setattr(os, "kill", fake_kill)
    drv.kill_uncleanly()
    assert signals[:2] == [0, signal.SIGKILL], signals
    found = app_deaths.drain()
    assert len(found) == 1 and found[0].startswith("the app died before kill_uncleanly() closed it"), found
    drv._stop_app()  # the pid is dropped: the teardown after the kill reads nothing
    assert app_deaths.drain() == []


def test_ios_relaunch_records_the_death_once(monkeypatch, tmp_path, gone_pid):
    _write_ips(tmp_path, "FaunaiOS", gone_pid)
    drv = _ios(monkeypatch, tmp_path, gone_pid, [])
    monkeypatch.setattr(drv, "launch", lambda config: None)
    assert inprocess_mod.InProcessAgentDriver.recover(drv) is True
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert found[0].startswith("the app died before the relaunch in recover() closed it: FaunaiOS")


def test_ios_launch_gate_timeout_marks_the_pid_reported(monkeypatch, tmp_path, gone_pid, capsys):
    drv = _ios(monkeypatch, tmp_path, gone_pid, [], log=FATAL + "\n")
    monkeypatch.setattr(drv, "_LAUNCH_READY_BUDGET_S", 0.0)
    monkeypatch.setattr(drv, "_agent_health_ok", lambda: False)
    monkeypatch.setattr(drv, "_launch_timeout_report", lambda *a, **k: "FAULTED")
    with pytest.raises(TimeoutError, match="FAULTED"):
        drv._await_launched_app("FAKE-UDID", 1.0)
    drv._stop_app()
    assert app_deaths.drain() == []
    assert "UNKNOWN" not in capsys.readouterr().out


# --------------------------------------------------------------------------
# The crash-report reader
# --------------------------------------------------------------------------


def test_a_report_naming_another_pid_is_not_read(tmp_path):
    _write_ips(tmp_path, "FaunaiOS", 111)
    text = local_launch.crash_report_naming(222, proc_name="FaunaiOS", since=None)
    assert text.startswith("--- crash report: none naming pid 222"), text


def test_a_report_older_than_the_launch_is_a_predecessors(tmp_path):
    path = _write_ips(tmp_path, "FaunaiOS", 333)
    os.utime(path, (1000.0, 1000.0))
    text = local_launch.crash_report_naming(333, proc_name="FaunaiOS", since=2000.0)
    assert text.startswith("--- crash report: none"), text


def test_a_malformed_report_is_skipped(tmp_path):
    (tmp_path / "DiagnosticReports" / "FaunaiOS-x.ips").write_text("{}\nnot json")
    text = local_launch.crash_report_naming(1, proc_name="FaunaiOS", since=None)
    assert text.startswith("--- crash report: none"), text


def test_another_process_name_is_never_opened(tmp_path):
    _write_ips(tmp_path, "FaunaMacOS", 444)
    text = local_launch.crash_report_naming(444, proc_name="FaunaiOS", since=None)
    assert text.startswith("--- crash report: none"), text


def test_sigtrap_is_a_crash_signal():
    assert local_launch.crash_exit_phrase(-signal.SIGTRAP, nt=False) == (
        "was killed by SIGTRAP (signal 5)"
    )

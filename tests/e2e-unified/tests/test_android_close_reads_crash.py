r"""tier_1: an android app that died of a crash before the harness closed it
fails the phase that found it.

Convention 6's close rider, its android leg
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention). The
android close is `am force-stop` through the instrumentation bridge
(`DELETE /session`, `AppLauncher.stopApp`), which ends the app however it was, so
an app that had already crashed — after a test's last step, or in the reset that
opens the next test — was relaunched unseen. The bridge lives in the app's own
process, so the witnesses are the device's, over `adb`: the app's pid, read at
the launch and again before the close, and logcat's crash blocks since the
launch. These tests drive each close path with only `adb` and the bridge's HTTP
calls replaced: the read comes first, a crash is recorded with its fatal line
and its block, every other ending stays silent, and the close still runs.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import android  # noqa: E402
from drivers.android import AndroidBridgeDriver, _crash_block, _device_instant_reading  # noqa: E402
from helpers import app_deaths  # noqa: E402

pytestmark = pytest.mark.tier_1

PKG = AndroidBridgeDriver.APPLICATION_ID
LAUNCHED = 1_800_000_000.0

JAVA_CRASH = f"""\
{LAUNCHED + 10:.3f}  4242  4242 I TestAgent: Executing reset
{LAUNCHED + 11:.3f}  4242  4242 E AndroidRuntime: FATAL EXCEPTION: main
{LAUNCHED + 11:.3f}  4242  4242 E AndroidRuntime: Process: {PKG}, PID: 4242
{LAUNCHED + 11:.3f}  4242  4242 E AndroidRuntime: java.lang.IllegalStateException: e2e reset fault
{LAUNCHED + 11:.3f}  4242  4242 E AndroidRuntime: \tat com.fauna.app.testing.TestAgent.dispatchCommand(TestAgent.kt:1400)
{LAUNCHED + 11:.3f}  4243  4243 I ActivityManager: Process {PKG} (pid 4242) has died
"""

NATIVE_CRASH = f"""\
{LAUNCHED + 5:.3f}  4242  4250 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4250 (tokio-rt), pid 4242 ({PKG})
{LAUNCHED + 5:.3f}  5000  5000 F DEBUG   : *** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
{LAUNCHED + 5:.3f}  5000  5000 F DEBUG   : pid: 4242, tid: 4250, name: tokio-rt  >>> {PKG} <<<
{LAUNCHED + 5:.3f}  5000  5000 F DEBUG   : signal 6 (SIGABRT), code -1 (SI_QUEUE), fault addr --------
"""

OTHER_APP_CRASH = f"""\
{LAUNCHED + 3:.3f}  7777  7777 E AndroidRuntime: FATAL EXCEPTION: main
{LAUNCHED + 3:.3f}  7777  7777 E AndroidRuntime: Process: com.example.other, PID: 7777
{LAUNCHED + 3:.3f}  7777  7777 E AndroidRuntime: java.lang.NullPointerException
"""

EARLIER_LAUNCH_CRASH = f"""\
{LAUNCHED - 60:.3f}  4100  4100 E AndroidRuntime: FATAL EXCEPTION: main
{LAUNCHED - 60:.3f}  4100  4100 E AndroidRuntime: Process: {PKG}, PID: 4100
{LAUNCHED - 60:.3f}  4100  4100 E AndroidRuntime: java.lang.RuntimeException: an earlier launch's
"""


@pytest.fixture(autouse=True)
def _clean_registry(monkeypatch):
    monkeypatch.setattr(app_deaths, "_reported", set())
    app_deaths.drain()
    yield
    app_deaths.drain()


class _Adb:
    """The device: `date; pidof` answers with the clock and the pid now,
    `logcat` with the buffer. Every script is logged, in order, beside the
    bridge's HTTP calls."""

    def __init__(self, calls, *, pid="4242", now=LAUNCHED + 30, logcat="", raises=False):
        self.calls, self.pid, self.now, self.logcat, self.raises = calls, pid, now, logcat, raises

    def __call__(self, script: str) -> str:
        self.calls.append("adb logcat" if script.startswith("logcat") else "adb pidof")
        if self.raises:
            raise android.subprocess.TimeoutExpired("adb", 10)
        if script.startswith("logcat"):
            return self.logcat
        return f"{self.now:.6f}\npid {self.pid}\n"


def _driver(monkeypatch, *, adb: _Adb, launch_pid="4242", marked=True):
    d = AndroidBridgeDriver.__new__(AndroidBridgeDriver)
    AndroidBridgeDriver.__init__(d)
    d._url = "http://127.0.0.1:1"
    d._bridge_dead = False
    d._bridge_log = ["[out] INSTRUMENTATION_RESULT: shortMsg=Process crashed."]
    d._app_launch_mark = (LAUNCHED, launch_pid, 1234.5) if marked else None
    monkeypatch.setattr(d, "_device_shell", adb)
    monkeypatch.setattr(d, "_delete", lambda path: adb.calls.append(f"DELETE {path}") or {})
    monkeypatch.setattr(d, "_post", lambda path, body=None: adb.calls.append(f"POST {path}") or {})
    monkeypatch.setattr(d, "_get", lambda path: {})
    return d


# ---- the parsers ------------------------------------------------------------


def test_the_instant_reading_takes_the_clock_and_the_pid():
    assert _device_instant_reading("1800000030.123456789\npid 4242\n") == (1800000030.123456789, "4242")
    assert _device_instant_reading("1800000030.%N\npid \n") == (1800000030.0, "")


def test_a_java_crash_leads_with_the_exception():
    fatal, block, at = _crash_block(JAVA_CRASH, package=PKG, pid="4242", since=LAUNCHED)
    assert fatal == "FATAL EXCEPTION: main — java.lang.IllegalStateException: e2e reset fault"
    assert len(block) == 4 and "TestAgent.kt:1400" in block[-1]
    assert at == pytest.approx(LAUNCHED + 11)


def test_a_native_crash_leads_with_libcs_fatal_signal():
    fatal, _block, _at = _crash_block(NATIVE_CRASH, package=PKG, pid="4242", since=LAUNCHED)
    assert fatal.startswith("Fatal signal 6 (SIGABRT)") and f"({PKG})" in fatal


def test_a_tombstone_alone_is_owned_by_the_package_name():
    tombstone = "\n".join(NATIVE_CRASH.splitlines()[1:])
    fatal, block, _at = _crash_block(tombstone, package=PKG, pid="4242", since=LAUNCHED)
    assert fatal == "tombstone — signal 6 (SIGABRT), code -1 (SI_QUEUE), fault addr --------"
    assert len(block) == 3


def test_another_apps_crash_and_an_earlier_launchs_are_not_this_one():
    assert _crash_block(OTHER_APP_CRASH, package=PKG, pid="4242", since=LAUNCHED) is None
    assert _crash_block(EARLIER_LAUNCH_CRASH, package=PKG, pid="4242", since=LAUNCHED) is None
    # The earlier launch's block is skipped, this one's still found behind it.
    found = _crash_block(EARLIER_LAUNCH_CRASH + JAVA_CRASH, package=PKG, pid="4242", since=LAUNCHED)
    assert found is not None and "e2e reset fault" in found[0]


def test_an_unread_pid_falls_back_to_the_package_name():
    found = _crash_block(JAVA_CRASH, package=PKG, pid="", since=LAUNCHED)
    assert found is not None and "e2e reset fault" in found[0]


# ---- each close path: the read first, the crash recorded, the close still run --


@pytest.mark.parametrize(
    "close, call, closing",
    [
        ("teardown", lambda d: d._stop_app(), "DELETE /session"),
        ("quit_app()", lambda d: d.quit_app(), "DELETE /session"),
    ],
)
def test_a_crash_before_the_close_is_recorded_and_the_close_runs(monkeypatch, close, call, closing):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="", logcat=JAVA_CRASH))
    call(d)
    assert calls[:3] == ["adb pidof", "adb logcat", closing]
    found = app_deaths.drain()
    assert len(found) == 1
    lines = found[0].splitlines()
    assert lines[0] == (
        f"the app died before {close} closed it: "
        "FATAL EXCEPTION: main — java.lang.IllegalStateException: e2e reset fault"
    )
    assert lines[1].startswith("it died +11 s after its launch, 19 s before this close")
    assert any("TestAgent.kt:1400" in ln for ln in lines)
    assert any("Process crashed." in ln for ln in lines)


def test_recover_reads_before_its_health_wait_and_relaunches_anyway(monkeypatch):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="", logcat=JAVA_CRASH))
    d._launch_config = {"seed_credentials": {}}
    monkeypatch.setattr(d, "_await_health", lambda *a, **k: calls.append("health") or True)
    monkeypatch.setattr(d, "_mark_log_baseline", lambda: None)
    monkeypatch.setattr(d, "_session_body", lambda config: {})
    assert d.recover() is True
    assert calls[:4] == ["adb pidof", "adb logcat", "health", "DELETE /session"]
    assert "POST /session" in calls
    (death,) = app_deaths.drain()
    assert death.startswith("the app died before the relaunch in recover() closed it: FATAL EXCEPTION")
    # The relaunch re-marks: a fresh launch for the next close to read against.
    assert d._app_launch_mark is not None and calls[-1] == "adb pidof"


def test_a_crash_that_took_the_bridge_is_recorded_though_recover_gives_up(monkeypatch):
    """The bridge runs in the app's process: the crash killed it too, the
    health wait fails — and the death is already on record for the phase."""
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="", logcat=NATIVE_CRASH))
    d._launch_config = {}
    monkeypatch.setattr(d, "_await_health", lambda *a, **k: False)
    assert d.recover() is False
    (death,) = app_deaths.drain()
    assert "Fatal signal 6 (SIGABRT)" in death.splitlines()[0]
    # The teardown after it reads nothing: the mark was spent by recover's read.
    d._stop_app()
    assert app_deaths.drain() == []


# ---- every other ending stays silent ----------------------------------------


def test_a_live_app_is_silent_and_logcat_is_not_read(monkeypatch):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="4242", logcat=JAVA_CRASH))
    d._stop_app()
    assert calls[:2] == ["adb pidof", "DELETE /session"]
    assert app_deaths.drain() == []


def test_a_gone_pid_with_no_crash_block_is_unknown_printed_never_failed(monkeypatch, capsys):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="", logcat=OTHER_APP_CRASH + EARLIER_LAUNCH_CRASH))
    d._stop_app()
    assert app_deaths.drain() == []
    assert "was gone before teardown closed it: UNKNOWN — logcat holds no crash block" in capsys.readouterr().out


def test_no_launch_mark_reads_nothing(monkeypatch):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="", logcat=JAVA_CRASH), marked=False)
    d._stop_app()
    assert "adb pidof" not in calls and app_deaths.drain() == []


def test_the_harness_own_quit_is_not_read_as_a_crash_at_the_teardown(monkeypatch):
    """`quit_app()` force-stops the app; the teardown after it finds no process,
    and must not read the harness's own close as a death."""
    calls: list[str] = []
    adb = _Adb(calls, pid="4242", logcat=JAVA_CRASH)
    d = _driver(monkeypatch, adb=adb)
    d.quit_app()
    adb.pid = ""
    d._stop_app()
    assert calls.count("adb pidof") == 1 and "adb logcat" not in calls
    assert app_deaths.drain() == []


def test_an_unanswering_device_is_silent_and_the_close_runs(monkeypatch):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, raises=True))
    d._stop_app()
    assert "DELETE /session" in calls and app_deaths.drain() == []


def test_one_death_is_recorded_once_per_launch(monkeypatch):
    calls: list[str] = []
    d = _driver(monkeypatch, adb=_Adb(calls, pid="", logcat=JAVA_CRASH))
    d.quit_app()
    d._stop_app()
    assert len(app_deaths.drain()) == 1

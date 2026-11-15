r"""tier_1: the android driver's unclean kill — a power-loss-shaped kill of the
app process on the device, and the relaunch that follows it.

Authority: `PlatformDriver.kill_uncleanly`'s contract (`drivers/base.py`) and
convention 7 mode 5's surface-gate rule (`docs/goal/architecture/e2e-conventions.md`):
`supports_unclean_kill` is a harness predicate that is real code, answered by
the driver's own handle — on android the pid the launch read on the device.
The kill is `adb shell run-as <package> kill -9 <that pid>`: SIGKILL under the
app's own uid, so no lifecycle callback runs, and never `am force-stop`. The
instrumentation bridge runs inside the app's process and dies with it, so
`recover()` starts a new `am instrument` over the same install and never
clears app data. `adb` and the bridge's HTTP calls are faked; nothing touches
a device.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import android  # noqa: E402
from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

PKG = AndroidBridgeDriver.APPLICATION_ID
ADB = ["adb", "-s", "emulator-5554"]


class _Device:
    """The device as adb sees it: the app's pid until a `kill -9` of that pid,
    a new pid once a new `am instrument` starts the app again."""

    def __init__(self, pid="4242", *, kill_takes=True):
        self.pid, self.kill_takes, self.next_pid = pid, kill_takes, "5151"
        self.calls: list[list[str]] = []

    def run(self, argv, **_kwargs):
        argv = list(argv)
        self.calls.append(argv)
        if argv[len(ADB):] == ["shell", "run-as", PKG, "kill", "-9", self.pid or ""] and self.kill_takes:
            self.pid = ""

        class _Done:
            returncode, stdout, stderr = 0, "", ""

        return _Done()

    def shell(self, script: str) -> str:
        self.calls.append(["shell", script])
        if script.startswith("pidof"):
            return self.pid
        if script.startswith("date"):
            return f"1800000000.5\npid {self.pid}\n"
        return ""


class _Bridge:
    """The `am instrument -w` client a relaunch starts."""

    def __init__(self, device: _Device):
        self.device = device
        self.pid = 999_999_999
        device.pid = device.next_pid

    def poll(self):
        return None

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0


def _launched(monkeypatch, device: _Device, *, pinned=False) -> AndroidBridgeDriver:
    driver = AndroidBridgeDriver()
    driver._device_port = 18500
    driver._local_port = 40000
    driver._url = "http://127.0.0.1:40000"
    driver._reverse_ports = [8443]
    driver._launch_config = {"environment": {}, "seed_credentials": {"fauna/index": "{}"}}
    if pinned:
        driver._launch_config[AndroidBridgeDriver._PRESERVE_STORE_KEY] = True
    driver._adb_cmd = lambda: list(ADB)
    driver._device_shell = device.shell
    driver._read_exit_before_close = lambda close: None
    driver._mark_log_baseline = lambda: None
    driver._vault_slots_before_erase = lambda: None
    driver.posts: list[tuple[str, dict]] = []
    driver.deletes: list[str] = []
    driver._post = lambda path, body: driver.posts.append((path, body)) or {}
    driver._delete = lambda path: driver.deletes.append(path) or {}
    driver._wait_for_health = lambda posted_at, timeout=30.0: None
    driver._await_health = lambda budget=None: True
    monkeypatch.setattr(android.subprocess, "run", device.run)
    monkeypatch.setattr(
        android.subprocess, "Popen",
        lambda argv, **kw: device.calls.append(list(argv)) or _Bridge(device),
    )
    monkeypatch.setattr(android, "reap_descendants_of", lambda pid: None)
    monkeypatch.setattr(android, "drain_pipes", lambda proc: [])
    driver._bridge_proc = _Bridge(device)
    device.pid = "4242"
    driver._mark_app_launch()
    device.calls.clear()
    return driver


def test_the_gate_is_the_launchs_own_pid():
    """(surface, app) alone: a launched android driver holds a pid, so it
    answers True; one never launched holds none and answers False."""
    driver = AndroidBridgeDriver()
    assert driver.supports_unclean_kill() is False
    driver._app_pid = "4242"
    assert driver.supports_unclean_kill() is True


def test_the_launch_mark_records_the_pid_the_kill_signals(monkeypatch):
    driver = _launched(monkeypatch, _Device())
    assert driver._app_pid == "4242"
    assert driver.supports_unclean_kill() is True


def test_the_kill_is_run_as_sigkill_of_the_launchs_pid(monkeypatch):
    device = _Device()
    driver = _launched(monkeypatch, device)
    driver.kill_uncleanly()
    signals = [c for c in device.calls if c[:len(ADB)] == ADB]
    assert signals == [[*ADB, "shell", "run-as", PKG, "kill", "-9", "4242"]], device.calls
    flat = " ".join(" ".join(c) for c in device.calls)
    assert "force-stop" not in flat and "am kill" not in flat and "pm clear" not in flat
    assert driver.deletes == [], "the kill never goes through the bridge's graceful stop"
    assert driver._app_pid is None
    assert driver._bridge_dead is True


def test_a_kill_that_does_not_take_is_a_failed_primitive(monkeypatch):
    device = _Device(kill_takes=False)
    driver = _launched(monkeypatch, device)
    monkeypatch.setattr(AndroidBridgeDriver, "_UNCLEAN_KILL_GONE_BUDGET_S", 0.0)
    with pytest.raises(RuntimeError, match="did not take"):
        driver.kill_uncleanly()


def test_an_unlaunched_driver_refuses_with_the_contract():
    with pytest.raises(NotImplementedError, match="unclean-kill primitive"):
        AndroidBridgeDriver().kill_uncleanly()


def test_recover_after_the_kill_starts_a_new_bridge_and_keeps_app_data(monkeypatch):
    device = _Device()
    driver = _launched(monkeypatch, device, pinned=True)
    driver.kill_uncleanly()
    device.calls.clear()
    assert driver.recover() is True
    instruments = [c for c in device.calls if "instrument" in c]
    assert len(instruments) == 1 and instruments[0][len(ADB):len(ADB) + 4] == [
        "shell", "am", "instrument", "-w"
    ]
    flat = " ".join(" ".join(c) for c in device.calls)
    assert "pm clear" not in flat and " clear" not in flat and "uninstall" not in flat
    assert "install" not in flat.replace("instrument", ""), "the same install, never a reinstall"
    assert "forward" not in flat and "reverse" not in flat, (
        "the forward and the reverses live on the adb server and outlive the app"
    )
    assert driver.deletes == []
    assert [p for p, _ in driver.posts] == ["/session"]
    assert "seed_credentials" not in driver.posts[0][1], (
        "a pinned relaunch reads back the store the killed process left"
    )
    assert driver._bridge_dead is False
    assert driver._app_pid == "5151", "the relaunch's own pid is the next kill's"
    assert driver._reverse_ports == [8443]


def test_recover_without_a_kill_keeps_the_in_place_relaunch(monkeypatch):
    device = _Device()
    driver = _launched(monkeypatch, device)
    assert driver.recover() is True
    assert driver.deletes == ["/session"]
    assert not [c for c in device.calls if "instrument" in c]

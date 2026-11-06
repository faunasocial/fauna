r"""tier_1: the Linux drivers' launch gates name their suspect, through the one builder.

Convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention). linux
and tui used to raise `in-process agent never became healthy`, android `bridge not
ready after Ns`, and each had a second string for an early exit. All three now
gather the four witnesses and raise `http_bridge.launch_timeout_report`'s grammar:
linux and tui through `drivers/local_launch.py` (the app's own pid resolved under
its handle — `xvfb-run`'s shell where that wrapper leads — and its CPU time read
from `/proc/<pid>/stat`), android through the instrumentation bridge and the
device's `/proc` over `adb`.

Every witness here is a fake except the stand-in process of the pid-resolution
test, which is a real `sh` running a real `sleep` (the `ping`/`sleep` stand-in
shape). STARVED and STALLED are pinned to the builder's current rule
(`LAUNCH_STARVED_CORE_SHARE`); how a launch that blocks is told apart is
another row's, which will move these
pins with it.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import local_launch  # noqa: E402
from drivers.android import AndroidBridgeDriver, _device_cpu_reading, _device_wait_reading  # noqa: E402
from drivers.http_bridge import LAUNCH_STARVED_CORE_SHARE  # noqa: E402
from drivers.linux import LinuxBridgeDriver  # noqa: E402
from drivers.port_util import popen_group_kwargs, reap_descendants_of  # noqa: E402
from drivers.tui import TuiDriver  # noqa: E402
from helpers import launch_load  # noqa: E402
from helpers.app_surface import skip_environment  # noqa: E402
from helpers.launch_load import MachineWindow  # noqa: E402

pytestmark = pytest.mark.tier_1

HZ = local_launch.CLK_TCK


def _machine(**over) -> MachineWindow:
    base = {
        "ticks": 6,
        "fence_cpus": 2,
        "fence_cpus_max": 2,
        "fence_of": 10,
        "lane_k": 2,
        "priv_pct": 30.0,
        "phantom": 0.4,
        "busy": 8.1,
        "outside": 1.0,
        "host_cores": 9.0,
        "steal_host": 0.3,
    }
    return MachineWindow(**{**base, **over})


class _Proc:
    """A Popen-compatible handle: `pid`, and `poll()` answering `rc`."""

    def __init__(self, pid: int, rc: int | None = None):
        self.pid, self.rc = pid, rc

    def poll(self):
        return self.rc


def _stat_line(pid: int, comm: str, ppid: int, cpu_s: float, start_ticks: int) -> str:
    # Fields after the comm: state, ppid, then 9 we ignore, utime, stime, then 6
    # more, starttime (field 22).
    ticks = int(cpu_s * HZ)
    rest = ["S", str(ppid), *["0"] * 9, str(ticks), "0", *["0"] * 6, str(start_ticks)]
    return f"{pid} ({comm}) {' '.join(rest)}\n"


def _proc_root(
    tmp_path: Path,
    procs: list[tuple[int, str, int, float]],
    age_s: float = 60.0,
    wait_s: float | None = 50.0,
) -> str:
    """A fake `/proc`: `procs` as (pid, comm, ppid, cpu seconds), every one started
    `age_s` ago by the harness's clock, each with two threads whose `schedstat`
    run-queue wait is `wait_s` and half of it (None: no schedstat at all)."""
    root = tmp_path / "proc"
    root.mkdir()
    boot = time.time() - 1000.0
    (root / "stat").write_text(f"cpu  1 2 3 4\nbtime {boot:.0f}\n")
    start_ticks = int((1000.0 - age_s) * HZ)
    for pid, comm, ppid, cpu_s in procs:
        (root / str(pid)).mkdir()
        (root / str(pid) / "stat").write_text(_stat_line(pid, comm, ppid, cpu_s, start_ticks))
        for tid, share in ((pid, 1.0), (pid + 1000, 0.5)):
            (root / str(pid) / "task" / str(tid)).mkdir(parents=True)
            if wait_s is not None:
                (root / str(pid) / "task" / str(tid) / "schedstat").write_text(
                    f"{int(cpu_s * 1e9)} {int(wait_s * share * 1e9)} 12\n"
                )
    return str(root)


def _first(report: str) -> str:
    return report.splitlines()[0]


def _local(tmp_path, proc, root, *, stderr="", log="", machine=None, comm="fauna-desktop"):
    err = tmp_path / "app.err"
    err.write_text(stderr)
    return local_launch.local_launch_timeout_report(
        proc=proc,
        comm=comm,
        budget_s=30.0,
        posted_at=time.time() - 30.0,
        stderr_paths=(str(err),),
        log_text=log,
        apps=("linux",),
        proc_root=root,
        machine=machine or _machine(),
    )


# ---- linux/tui: the shared local-process witnesses --------------------------


def test_an_early_exit_is_faulted_and_carries_the_stderr_tail(tmp_path):
    root = _proc_root(tmp_path, [])
    report = _local(
        tmp_path, _Proc(4242, rc=3), root, stderr="starting\nError: no display\n"
    )
    assert _first(report) == "launch timeout (30 s): FAULTED — fauna-desktop exited with code 3"
    assert "Error: no display" in report, report


def test_a_signal_death_is_named_as_one(tmp_path):
    report = _local(tmp_path, _Proc(4242, rc=-11), _proc_root(tmp_path, []))
    assert _first(report).endswith("FAULTED — fauna-desktop was killed by signal 11")


def test_a_panic_while_alive_is_faulted(tmp_path):
    root = _proc_root(tmp_path, [(100, "xvfb-run", 1, 0.1), (101, "fauna-desktop", 100, 40.0)])
    report = _local(
        tmp_path, _Proc(100), root, stderr="thread 'main' panicked at src/app.rs:9:5:\nboom\n"
    )
    assert "FAULTED — thread 'main' panicked at src/app.rs:9:5:" in _first(report)


def test_starved_reads_the_apps_cpu_not_the_xvfb_wrapper(tmp_path):
    # The wrapper burnt a core; the app under it got 2 s of CPU in 60 s.
    root = _proc_root(
        tmp_path,
        [(100, "xvfb-run", 1, 59.0), (102, "Xvfb", 100, 30.0), (103, "fauna-desktop", 100, 2.0)],
    )
    report = _local(tmp_path, _Proc(100), root)
    first = _first(report)
    # 50 s of 60 s, the wall rounded to the harness's clock: 83 or 84 %.
    assert first.startswith(
        "launch timeout (30 s): STARVED — the app ran 2.0 core-s in 60 s and waited for a CPU 8"
    ), first
    assert " % of them; fence" in first, first
    assert "fence 2 of 2, priv 30 %, phantom 0.4, steal_host 0.3" in first
    assert "its longest-waiting thread's run-queue wait, 50.0 s of " in report, report


def test_stalled_when_the_app_was_given_its_cpu(tmp_path):
    root = _proc_root(tmp_path, [(100, "xvfb-run", 1, 0.1), (103, "fauna-desktop", 100, 45.0)], wait_s=3.0)
    first = _first(_local(tmp_path, _Proc(100), root, log="2026-10-30T10:00:00.000Z  INFO fauna client logging initialised\n"))
    assert first.startswith(
        "launch timeout (30 s): STALLED, NOT STARVED — ran 45.0 core-s, waited for a CPU 5 % of 60 s;"
    ), first
    assert 'last line "fauna client logging initialised"' in first


def test_a_blocked_launch_is_stalled_not_starved(tmp_path):
    """Little CPU and no run-queue wait: it asked for none — the app's own, never
    STARVED (the rider's rule since 2026-10-30)."""
    root = _proc_root(tmp_path, [(100, "xvfb-run", 1, 0.1), (103, "fauna-desktop", 100, 0.2)], wait_s=0.0)
    first = _first(_local(tmp_path, _Proc(100), root))
    assert first.startswith(
        "launch timeout (30 s): STALLED, NOT STARVED — ran 0.2 core-s, waited for a CPU 0 % of 60 s;"
    ), first


def test_unknown_under_the_line_without_schedstat(tmp_path):
    """A kernel without `CONFIG_SCHED_INFO` keeps no run-queue wait; under the
    quarter-core line that witness decides, so its absence is stated."""
    root = _proc_root(tmp_path, [(7, "fauna-desktop", 1, 0.2)], wait_s=None)
    report = _local(tmp_path, _Proc(7), root)
    first = _first(report)
    assert first.startswith("launch timeout (30 s): UNKNOWN — the process's CPU wait: "), first
    assert "task/*/schedstat could not be read (no schedstat line under" in report, report


def test_the_longest_waiting_thread_is_the_reading():
    """One thread's wait, never a sum over a dozen idle threads."""
    lines = ["100 2000000000 5", "100 500000000 5", "not a schedstat line", "1 2"]
    assert local_launch.schedstat_wait_s(lines) == 2.0
    assert local_launch.schedstat_wait_s(["1 2"]) is None


def test_the_rule_is_the_builders_quarter_core(tmp_path):
    # 60 s of wall: just under and just over a quarter of a core.
    under = LAUNCH_STARVED_CORE_SHARE * 60 - 0.5
    over = LAUNCH_STARVED_CORE_SHARE * 60 + 0.5
    for cpu_s, verdict in ((under, "STARVED"), (over, "STALLED")):
        sub = tmp_path / verdict
        sub.mkdir()
        root = _proc_root(sub, [(7, "fauna-desktop", 1, cpu_s)])
        assert verdict in _first(_local(sub, _Proc(7), root))


def test_unknown_when_the_ledger_cannot_be_read(tmp_path):
    root = _proc_root(tmp_path, [(7, "fauna-desktop", 1, 2.0)])
    first = _first(
        _local(tmp_path, _Proc(7), root, machine=MachineWindow(unread="no agent ledger at /x"))
    )
    assert first == "launch timeout (30 s): UNKNOWN — the agent ledger: no agent ledger at /x"


def test_unknown_names_the_pid_it_could_not_resolve(tmp_path):
    root = _proc_root(tmp_path, [(100, "xvfb-run", 1, 0.1), (102, "Xvfb", 100, 3.0)])
    first = _first(_local(tmp_path, _Proc(100), root))
    assert first == (
        "launch timeout (30 s): UNKNOWN — the process's CPU time: "
        "no fauna-desktop process at or under the launch's pid 100"
    )


def test_no_proc_is_an_absence_stated(tmp_path):
    first = _first(_local(tmp_path, _Proc(7), "/no-proc-here"))  # short: a long tmp path pushes the phrase past the ceiling
    assert "UNKNOWN — the process's CPU time: no " in first and "on this platform" in first


def test_the_real_app_pid_is_found_under_a_real_wrapper():
    if not os.path.isdir("/proc"):
        skip_environment("no /proc on this platform: the reader is linux's")
    wrapper = subprocess.Popen(["sh", "-c", "sleep 30; :"], **popen_group_kwargs())
    reap_descendants_of(wrapper.pid)
    try:
        deadline = time.monotonic() + 10
        pid = None
        while pid is None and time.monotonic() < deadline:
            pid = local_launch.descendant_named(wrapper.pid, "sleep")
            time.sleep(0.05)
        assert pid is not None and pid != wrapper.pid
        cpu_s, started_at = local_launch.proc_cpu_reading(pid)
        assert 0.0 <= cpu_s < 5.0
        assert abs(time.time() - started_at) < 15.0
        assert 0.0 <= local_launch.proc_wait_reading(pid) < 15.0
        assert local_launch.descendant_named(wrapper.pid, "sh") == wrapper.pid
    finally:
        wrapper.kill()
        wrapper.wait()


# ---- the drivers route both ends of their gate through it ------------------


def _no_ledger_read(monkeypatch):
    monkeypatch.setattr(launch_load, "machine_window", lambda *a, **k: _machine())


def test_the_linux_gate_reports_through_the_builder(tmp_path, monkeypatch):
    _no_ledger_read(monkeypatch)
    d = LinuxBridgeDriver.__new__(LinuxBridgeDriver)
    d._app_proc = _Proc(4242, rc=1)
    d._tmp_dir = str(tmp_path)
    d._xdg_config = str(tmp_path / "config")
    (tmp_path / "app.err").write_text("Gtk-WARNING: cannot open display\n")
    report = d._launch_timeout_report(time.time() - 5)
    assert _first(report) == "launch timeout (30 s): FAULTED — fauna-desktop exited with code 1"
    assert "cannot open display" in report


def test_the_tui_gate_reads_the_pty_stream_when_stderr_is_empty(tmp_path, monkeypatch):
    _no_ledger_read(monkeypatch)
    d = TuiDriver.__new__(TuiDriver)
    d._pty = _Proc(4242, rc=101)
    d._xdg_config = str(tmp_path / "config")
    (tmp_path / "app.err").write_text("")
    out = tmp_path / "app.out"
    out.write_text("thread 'main' panicked at src/main.rs:1:1:\n")
    first = _first(d._launch_timeout_report(time.time() - 5, str(tmp_path), str(out)))
    assert first == "launch timeout (30 s): FAULTED — fauna-tui exited with code 101"


# ---- android: the bridge and the device's /proc -----------------------------


def _device_answer(cpu_s: float, age_s: float, hz: int = 100, wait_s: float | None = 50.0) -> str:
    uptime = 5000.0
    start = int((uptime - age_s) * hz)
    stat = _stat_line(9001, "social.fauna.fauna", 300, 0, start).replace(
        " 0 0 0 0 0 0 0 0 0 0 0 ", f" 0 0 0 0 0 0 0 0 0 {int(cpu_s * hz)} 0 ", 1
    )
    schedstat = "" if wait_s is None else f"{int(cpu_s * 1e9)} {int(wait_s * 1e9)} 12\n100 {int(wait_s * 5e8)} 3\n"
    return f"pid 9001\n{hz}\n{stat}{uptime:.2f} 9000.00\n{schedstat}"


def test_the_device_reading_parses_cpu_and_wall():
    pid, cpu_s, wall_s, why = _device_cpu_reading(_device_answer(2.0, 60.0))
    assert (pid, why) == ("9001", "")
    assert cpu_s == pytest.approx(2.0, abs=0.02)
    assert wall_s == pytest.approx(60.0, abs=0.02)


def test_the_device_reading_parses_the_wait_too():
    share, basis, why = _device_wait_reading(_device_answer(2.0, 60.0), "9001", 60.0)
    assert (share, why) == (pytest.approx(50.0 / 60.0), "")
    assert "50.0 s of 60.0 s" in basis and "/proc/9001/task/*/schedstat" in basis
    share, basis, why = _device_wait_reading(_device_answer(2.0, 60.0, wait_s=None), "9001", 60.0)
    assert (share, basis) == (None, "") and "schedstat" in why


def test_the_device_reading_says_why_without_a_pid():
    assert _device_cpu_reading("pid \n100\n5000.00 9000.00\n") == (
        "",
        None,
        None,
        "the app has no process on the device (pidof found none)",
    )


def _android(monkeypatch, *, rc, answer, logcat="I/fauna: started\n"):
    _no_ledger_read(monkeypatch)
    d = AndroidBridgeDriver.__new__(AndroidBridgeDriver)
    d._bridge_proc = _Proc(77, rc=rc)
    d._bridge_log = ["[err] INSTRUMENTATION_RESULT: shortMsg=Process crashed."] if rc is not None else []
    monkeypatch.setattr(
        d, "_device_shell", lambda script: logcat if script.startswith("logcat") else answer
    )
    return d


def test_android_bridge_exit_is_faulted(monkeypatch):
    d = _android(monkeypatch, rc=1, answer="pid \n100\n5000.00 9000.00\n")
    report = d._launch_timeout_report(time.time() - 3, 30.0)
    assert _first(report) == (
        "launch timeout (30 s): FAULTED — the bridge instrumentation exited with code 1"
    )
    assert "Process crashed" in report
    assert "the emulator runs on the emulator host" in report


def test_android_alive_and_silent_reads_the_devices_cpu(monkeypatch):
    starved = _android(monkeypatch, rc=None, answer=_device_answer(2.0, 60.0))
    assert "STARVED — the app ran 2.0 core-s in 60 s and waited for a CPU 83 % of them" in _first(
        starved._launch_timeout_report(time.time() - 30, 30.0)
    )
    stalled = _android(monkeypatch, rc=None, answer=_device_answer(40.0, 60.0, wait_s=3.0))
    assert "STALLED, NOT STARVED — ran 40.0 core-s, waited for a CPU 5 % of 60 s" in _first(
        stalled._launch_timeout_report(time.time() - 30, 30.0)
    )
    blocked = _android(monkeypatch, rc=None, answer=_device_answer(0.2, 60.0, wait_s=0.0))
    assert "STALLED, NOT STARVED — ran 0.2 core-s, waited for a CPU 0 % of 60 s" in _first(
        blocked._launch_timeout_report(time.time() - 30, 30.0)
    )
    no_schedstat = _android(monkeypatch, rc=None, answer=_device_answer(0.2, 60.0, wait_s=None))
    assert "UNKNOWN — the process's CPU wait: the device answered no schedstat line" in _first(
        no_schedstat._launch_timeout_report(time.time() - 30, 30.0)
    )


def test_android_unread_device_is_unknown(monkeypatch):
    d = _android(monkeypatch, rc=None, answer="")
    first = _first(d._launch_timeout_report(time.time() - 30, 30.0))
    assert first.startswith("launch timeout (30 s): UNKNOWN — the process's CPU time:"), first


# ---- the ledger every machine now writes reads as a machine window ----------


def test_a_usage_only_ledger_row_reads_as_a_window(tmp_path, monkeypatch):
    """Linux's and macOS's rows carry the usage record and no probe fields
    (`core-broker.py::cmd_agent`); the reader needs nothing else, so a launch
    timeout there reads STARVED or STALLED rather than UNKNOWN."""
    try:
        broker = launch_load._broker()
    except FileNotFoundError as exc:
        skip_environment(f"the fleet's tooling is not in this checkout: {exc}")
    monkeypatch.setenv("FAUNA_SLOT_DIR", str(tmp_path))
    monkeypatch.setattr(launch_load, "_host_reading", lambda *a, **k: (None, None, "not asked"))
    now = time.time()
    import datetime

    rows = [
        {
            "ts": datetime.datetime.fromtimestamp(now - 20 + i * 5).astimezone().isoformat(),
            "cpu_pct": 60.0,
            "user_pct": 40.0,
            "priv_pct": 20.0,
            "intr_pct": 1.0,
            "tick_s": 5.0,
            "fence": {"cpus": 2, "of": 10, "by": "broker"},
            "grants": {"e2e_tui": 1},
            "owners": {},
        }
        for i in range(4)
    ]
    (tmp_path / broker._PROBE_LEDGER).write_text("".join(json.dumps(r) + "\n" for r in rows))
    window = launch_load.machine_window(now - 30, now, apps=("linux",), vcpus=10)
    assert not window.unread, window.unread
    assert (window.ticks, window.fence_cpus, window.priv_pct) == (4, 2, 20.0)

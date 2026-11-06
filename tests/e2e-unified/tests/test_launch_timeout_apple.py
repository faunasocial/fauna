"""tier_1: the apple drivers' launch gates name their suspect, through the one builder.

Convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention). macOS
(the spawned app, and the photo-library bundle started by `open`) and iOS (the app
`simctl launch` started in the simulator) used to raise `in-process agent never
became healthy`; they now gather the four witnesses and raise
`http_bridge.launch_timeout_report`'s grammar through `drivers/local_launch.py`,
with the CPU read from `ps` where there is no `/proc`.

No simulator and no app: the stand-in process is a real `sleep`, every other
witness is a fake. Darwin keeps no run-queue wait, so the CPU wait that tells a
blocked launch from a starved one is `local_launch.ThreadStateSampler`'s: thread
states read once a second through the gate, against a fake `ps -M` carrying the
shape the real one prints, and one real `ps -M` over a sleeper and a spinner.
"""

from __future__ import annotations

import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import local_launch  # noqa: E402
from drivers.ios import IosInProcessDriver  # noqa: E402
from drivers.macos import MacosInProcessDriver, _pid_of_bundle  # noqa: E402
from drivers.port_util import popen_group_kwargs, reap_descendants_of  # noqa: E402
from helpers.launch_load import MachineWindow  # noqa: E402
from helpers.waiting import wait_until  # noqa: E402

pytestmark = pytest.mark.tier_1

#: `ps -o`, `ps -M` and the macOS `log` slice are Darwin/POSIX tools (Windows has none).
_NEEDS_PS = pytest.mark.skipif(
    sys.platform == "win32", reason="reads POSIX `ps` / the macOS log: no such tool on Windows"
)


def _machine() -> MachineWindow:
    return MachineWindow(
        ticks=6, fence_cpus=2, fence_cpus_max=2, fence_of=10, lane_k=2,
        priv_pct=30.0, phantom=0.4, busy=8.1, outside=1.0, host_cores=9.0, steal_host=0.3,
    )


class _Proc:
    def __init__(self, pid: int, rc: int | None = None):
        self.pid, self.rc = pid, rc

    def poll(self):
        return self.rc


@pytest.fixture
def sleeper():
    proc = subprocess.Popen(["sleep", "60"], **popen_group_kwargs())
    reap_descendants_of(proc.pid)
    try:
        yield proc
    finally:
        proc.kill()
        proc.wait()


def _report(**over) -> str:
    args = dict(
        comm="FaunaMacOS",
        budget_s=30.0,
        posted_at=time.time() - 30.0,
        stderr_paths=(),
        log_text="",
        apps=("macos",),
        cpu_reader=local_launch.ps_cpu_reading,
        machine=_machine(),
    )
    args.update(over)
    return local_launch.local_launch_timeout_report(**args)


def _first(report: str) -> str:
    return report.splitlines()[0]


@pytest.mark.parametrize(
    "text,secs",
    [("0:01.50", 1.5), ("12:34.00", 754.0), ("1:02:03", 3723.0), ("2-00:00:10", 172810.0)],
)
def test_ps_cputime_formats(text, secs):
    assert local_launch.parse_ps_cputime(text) == pytest.approx(secs)


@_NEEDS_PS
def test_ps_reads_a_live_process_cpu_and_start(sleeper):
    cpu_s, started = local_launch.ps_cpu_reading(sleeper.pid)
    assert 0.0 <= cpu_s < 5.0
    assert abs(time.time() - started) < 120


def test_ps_raises_process_lookup_for_a_gone_pid(sleeper):
    sleeper.kill()
    sleeper.wait()
    with pytest.raises(ProcessLookupError):
        local_launch.ps_cpu_reading(sleeper.pid)


def test_a_macos_early_exit_is_faulted_not_a_second_grammar():
    report = _report(proc=_Proc(4242, rc=3), log_text="")
    assert _first(report) == "launch timeout (30 s): FAULTED — FaunaMacOS exited with code 3"


def test_a_macos_live_process_reads_its_cpu_from_ps(sleeper):
    report = _report(proc=sleeper)
    assert _first(report).startswith("launch timeout (30 s): "), report
    assert "FAULTED" not in _first(report), report
    assert "ps has no process" not in report and "could not be read" not in report, report


def test_a_fatal_in_this_launchs_log_is_faulted(sleeper):
    report = _report(
        proc=sleeper,
        log_text="2026-10-30T10:00:00.000000Z ERROR [fauna] FATAL: store is locked\n",
    )
    assert _first(report).startswith("launch timeout (30 s): FAULTED"), report


def test_no_handle_and_no_pid_reads_unknown_naming_why():
    report = _report(proc=None, pid=None, pid_unread="`open` returns no pid and no process runs the staged bundle")
    assert _first(report).startswith("launch timeout (30 s): UNKNOWN"), report
    assert "`open` returns no pid" in report, report


def test_a_pid_ps_no_longer_knows_is_the_process_gone(sleeper):
    sleeper.kill()
    sleeper.wait()
    report = _report(proc=None, pid=sleeper.pid)
    assert _first(report).startswith("launch timeout (30 s): FAULTED"), report
    assert "no longer running" in _first(report), report


@_NEEDS_PS
def test_a_resolved_pid_without_a_handle_is_read_through_ps(sleeper):
    report = _report(proc=None, pid=sleeper.pid)
    assert "FAULTED" not in _first(report), report
    assert "ps has no process" not in report, report


def test_ios_report_states_the_missing_console_and_ends_on_the_hint(sleeper):
    drv = object.__new__(IosInProcessDriver)
    drv._app_pid = sleeper.pid
    drv._app_support = None
    report = IosInProcessDriver._launch_timeout_report(drv, time.time() - 30.0, "UDID-1")
    assert _first(report).startswith("launch timeout (30 s): "), report
    assert "never became healthy" not in report, report
    assert report.splitlines()[-1].endswith(
        f"xcrun simctl launch --console UDID-1 {IosInProcessDriver.BUNDLE_ID}"
    ), report


def test_ios_without_a_printed_pid_reads_unknown():
    drv = object.__new__(IosInProcessDriver)
    drv._app_pid = None
    drv._app_support = None
    report = IosInProcessDriver._launch_timeout_report(drv, time.time() - 30.0, "UDID-1")
    assert "UNKNOWN" in _first(report), report
    assert "`simctl launch` printed no pid" in report, report


@_NEEDS_PS
def test_macos_log_is_sliced_at_this_launchs_floor(tmp_path):
    drv = object.__new__(MacosInProcessDriver)
    drv._app_support = str(tmp_path)
    logs = tmp_path / "logs"
    logs.mkdir()
    log = logs / "fauna.log.2026-10-30"
    log.write_text("[fauna] FATAL: a predecessor's\n")
    drv._mark_log_baseline()
    assert drv.app_log_text() is None
    with open(log, "a") as fh:
        fh.write("this launch's line\n")
    assert drv.app_log_text() == "this launch's line\n"


def test_pid_of_bundle_finds_nothing_for_an_unstaged_bundle(tmp_path):
    assert _pid_of_bundle(tmp_path / "Nope.app") is None
    assert os.path.isdir(tmp_path)


# The sampled CPU wait. `ps -M -o state -p <pid>` prints darwin's fixed thread table
# and appends the asked column: the first row carries USER and COMMAND, a thread's
# continuation row has neither, and a pid `ps` does not know exits 1 with the header
# alone (all three measured on macOS, 2026-10-31).
PS_M_HEADER = "USER   PID   TT   %CPU STAT PRI     STIME     UTIME COMMAND STAT\n"


def _ps_m(*states: str) -> str:
    rows = [f"user  4242   ??    0.0 {states[0]}    31T   0:00.15   0:00.00 /Applic {states[0]}   \n"]
    rows += [f"      4242         0.0 {s}    31T   0:00.00   0:00.00         {s}   \n" for s in states[1:]]
    return PS_M_HEADER + "".join(rows)


class _Answer:
    def __init__(self, stdout: str, returncode: int = 0):
        self.stdout, self.returncode, self.stderr = stdout, returncode, ""


def _fake_ps_m(monkeypatch, *answers: _Answer):
    """`subprocess.run` answering `ps -M` from `answers`, the last one repeating."""
    queue = list(answers)

    def run(cmd, **_kw):
        assert cmd[:4] == ["ps", "-M", "-o", "state"], cmd
        return queue.pop(0) if len(queue) > 1 else queue[0]

    monkeypatch.setattr(local_launch.subprocess, "run", run)


def _sampled(*runnable: bool, pid: int | None = 4242) -> local_launch.ThreadStateSampler:
    """A sampler that has already taken one tick per entry, over a fake reader."""
    answers = iter(runnable)
    sampler = local_launch.ThreadStateSampler(
        lambda: pid, reader=lambda _pid: ["R" if next(answers) else "S", "S"]
    )
    for _ in runnable:
        sampler.tick()
    return sampler


def test_thread_states_are_the_last_column_of_every_thread_row(monkeypatch):
    _fake_ps_m(monkeypatch, _Answer(_ps_m("S", "R", "S")))
    assert local_launch.ps_thread_states(4242) == ["S", "R", "S"]


def test_a_header_only_answer_is_a_failed_read_not_a_process_with_no_runnable_thread(monkeypatch):
    _fake_ps_m(monkeypatch, _Answer(PS_M_HEADER, returncode=1))
    with pytest.raises(ProcessLookupError):
        local_launch.ps_thread_states(4242)
    sampler = local_launch.ThreadStateSampler(lambda: 4242)
    sampler.tick()
    assert sampler.samples == 0 and sampler.failures == 1


def test_a_failed_read_between_good_ones_is_no_sample(monkeypatch):
    _fake_ps_m(
        monkeypatch,
        _Answer(_ps_m("R", "S")),
        _Answer(PS_M_HEADER, returncode=1),
        _Answer(_ps_m("S", "S")),
    )
    sampler = local_launch.ThreadStateSampler(lambda: 4242)
    for _ in range(3):
        sampler.tick()
    assert (sampler.runnable, sampler.samples, sampler.failures) == (1, 2, 1)


def test_a_tick_with_no_pid_is_not_a_sample():
    pids = iter([None, 4242])
    sampler = local_launch.ThreadStateSampler(lambda: next(pids), reader=lambda _pid: ["R"])
    sampler.tick()
    assert (sampler.samples, sampler.failures) == (0, 0)
    sampler.tick()
    assert (sampler.runnable, sampler.samples) == (1, 1)


def test_a_reader_that_raises_never_reaches_the_gate():
    def reader(_pid):
        raise RuntimeError("ps is broken")

    sampler = local_launch.ThreadStateSampler(lambda: 4242, reader=reader)
    sampler.tick()
    assert sampler.failures == 1
    assert "ps is broken" in sampler.unread()


def test_a_pid_source_that_raises_never_reaches_the_gate():
    def source():
        raise RuntimeError("no scan")

    sampler = local_launch.ThreadStateSampler(source)
    sampler.tick()
    assert sampler.samples == 0 and "no scan" in sampler.unread()


def test_the_sampler_never_outlives_its_gate(monkeypatch):
    monkeypatch.setattr(local_launch, "THREAD_SAMPLE_INTERVAL_S", 0.01)
    sampled = threading.Event()

    def reader(_pid):
        sampled.set()
        return ["S"]

    with local_launch.ThreadStateSampler(lambda: 4242, reader=reader) as sampler:
        assert sampled.wait(30), "the sampler took no sample"
        thread = sampler._thread
    assert not thread.is_alive()


def test_the_sampler_stops_at_its_ceiling(monkeypatch):
    monkeypatch.setattr(local_launch, "THREAD_SAMPLE_INTERVAL_S", 0)
    monkeypatch.setattr(local_launch, "THREAD_SAMPLE_CEILING", 3)
    sampler = local_launch.ThreadStateSampler(lambda: 4242, reader=lambda _pid: ["S"])
    sampler.start()
    sampler._thread.join(30)
    assert not sampler._thread.is_alive()
    assert sampler.samples == 3


def _fixed_cpu(cpu_s: float, wall_s: float = 30.0):
    return lambda _pid: (cpu_s, time.time() - wall_s)


def test_a_launch_with_runnable_threads_and_no_cpu_reads_starved():
    report = _report(
        proc=_Proc(4242), cpu_reader=_fixed_cpu(0.3), wait_sampler=_sampled(*[True] * 28, False, False)
    )
    first = _first(report)
    assert first.startswith("launch timeout (30 s): STARVED — the app ran 0.3 core-s in 30 s"), report
    assert "and waited for a CPU 92 % of them" in first, report
    assert "a thread runnable at 28 of 30 one-a-second `ps -M` reads" in report, report


def test_a_launch_with_no_runnable_thread_reads_stalled():
    report = _report(proc=_Proc(4242), cpu_reader=_fixed_cpu(0.3), wait_sampler=_sampled(*[False] * 30))
    first = _first(report)
    assert first.startswith("launch timeout (30 s): STALLED, NOT STARVED — ran 0.3 core-s, "), report
    assert "waited for a CPU 0 % of 30 s" in first, report


def test_its_own_running_samples_are_not_a_wait():
    """`R` is running OR runnable. A launch that ran 0.2 of a core and was runnable at
    40 % of the reads spent half of those on a CPU: 20 % is what it waited, under the
    quarter, where the raw `R` share would have read a false STARVED."""
    report = _report(
        proc=_Proc(4242), cpu_reader=_fixed_cpu(6.0), wait_sampler=_sampled(*[True] * 12, *[False] * 18)
    )
    first = _first(report)
    assert first.startswith("launch timeout (30 s): STALLED, NOT STARVED — "), report
    assert "waited for a CPU 20 % of 30 s" in first, report
    assert "less the 0.20 of a core it ran" in report, report


def test_the_wait_is_floored_at_zero():
    """Several threads can spend more of a core than the reads caught runnable."""
    report = _report(proc=_Proc(4242), cpu_reader=_fixed_cpu(6.0), wait_sampler=_sampled(True, *[False] * 29))
    assert "waited for a CPU 0 % of 30 s" in _first(report), report


def test_a_launch_over_the_quarter_needs_no_sample():
    sampler = local_launch.ThreadStateSampler(lambda: None)
    report = _report(proc=_Proc(4242), cpu_reader=_fixed_cpu(20.0), wait_sampler=sampler)
    assert _first(report).startswith("launch timeout (30 s): STALLED, NOT STARVED — ran 20.0 core-s"), report


def test_a_sampler_that_read_nothing_keeps_the_unknown_wording():
    def reader(_pid):
        raise ProcessLookupError("ps -M has no thread row for pid 4242 (exit 1)")

    sampler = local_launch.ThreadStateSampler(lambda: 4242, reader=reader)
    sampler.tick()
    report = _report(proc=_Proc(4242), cpu_reader=_fixed_cpu(0.3), wait_sampler=sampler)
    first = _first(report)
    assert first.startswith("launch timeout (30 s): UNKNOWN — the process's CPU wait: "), report
    assert "darwin keeps no run-queue wait" in first, report
    assert "ps -M has no thread row" in report, report


def test_no_sampler_reads_unknown_naming_that():
    report = _report(proc=_Proc(4242), cpu_reader=_fixed_cpu(0.3))
    assert "UNKNOWN — the process's CPU wait: " in _first(report), report
    assert "no thread-state sampler ran for this launch" in report, report


def test_the_failed_reads_are_counted_in_the_basis():
    answers = iter([["R"], ProcessLookupError("gone"), ["S"]])

    def reader(_pid):
        answer = next(answers)
        if isinstance(answer, Exception):
            raise answer
        return answer

    sampler = local_launch.ThreadStateSampler(lambda: 4242, reader=reader)
    for _ in range(3):
        sampler.tick()
    report = _report(proc=_Proc(4242), cpu_reader=_fixed_cpu(0.3), wait_sampler=sampler)
    assert "1 read(s) failed" in report, report


@_NEEDS_PS
def test_a_real_ps_reads_a_sleeper_as_not_runnable_and_a_spinner_as_runnable(sleeper):
    assert not local_launch.any_runnable(local_launch.ps_thread_states(sleeper.pid))
    spinner = subprocess.Popen(
        [sys.executable, "-c", "while True: pass"], **popen_group_kwargs()
    )
    reap_descendants_of(spinner.pid)
    try:
        wait_until(
            lambda: local_launch.any_runnable(local_launch.ps_thread_states(spinner.pid)),
            30.0,
            diagnose=lambda: str(local_launch.ps_thread_states(spinner.pid)),
        )
    finally:
        spinner.kill()
        spinner.wait()


def _gate_ps(monkeypatch, *, bundle_exe: str, seen: threading.Event):
    """A fake `subprocess.run` for a whole gate: the `open`, the bundle scan, the
    `time=` / `lstart=` reads and `ps -M`, whose first answer releases `seen`."""
    started = time.strftime("%a %b %d %H:%M:%S %Y", time.localtime(time.time() - 30))

    def run(cmd, **_kw):
        if cmd[0] == "open":
            return _Answer("")
        if cmd[:2] == ["ps", "-axo"]:
            return _Answer(f"  4242 {bundle_exe}\n")
        if cmd[:2] == ["ps", "-M"]:
            seen.set()
            return _Answer(_ps_m("R", "S"))
        if cmd[:3] == ["ps", "-o", "time="]:
            return _Answer("0:00.30\n")
        if cmd[:3] == ["ps", "-o", "lstart="]:
            return _Answer(f"{started}\n")
        raise AssertionError(f"unexpected command {cmd}")

    monkeypatch.setattr(subprocess, "run", run)


def _unhealthy_until(seen: threading.Event):
    """An `_agent_health_ok` that answers no, and not before a sample was taken: the
    gate's budget is then spent on the sampler's clock, not on a guess at it."""

    def health(_self=None) -> bool:
        assert seen.wait(30), "the gate's sampler took no sample"
        return False

    return health


def _no_machine(monkeypatch):
    from helpers import launch_load

    monkeypatch.setattr(launch_load, "machine_window", lambda *_a, **_k: _machine())


def test_the_macos_spawn_gate_samples_its_handle(monkeypatch, tmp_path):
    seen = threading.Event()
    monkeypatch.setattr(local_launch, "THREAD_SAMPLE_INTERVAL_S", 0.01)
    _no_machine(monkeypatch)
    drv = object.__new__(MacosInProcessDriver)
    drv._app_proc = _Proc(4242)
    drv._tmp_dir = str(tmp_path)
    drv._app_support = None
    monkeypatch.setattr(MacosInProcessDriver, "_LAUNCH_READY_BUDGET_S", 0.05)
    monkeypatch.setattr(MacosInProcessDriver, "_agent_health_ok", _unhealthy_until(seen))
    _gate_ps(monkeypatch, bundle_exe="/nowhere", seen=seen)
    with pytest.raises(TimeoutError) as raised:
        drv._await_spawned_app()
    assert "one-a-second `ps -M` reads" in str(raised.value), raised.value


def test_the_open_gate_resolves_the_pid_per_tick_and_samples_it(monkeypatch, tmp_path):
    seen = threading.Event()
    monkeypatch.setattr(local_launch, "THREAD_SAMPLE_INTERVAL_S", 0.01)
    _no_machine(monkeypatch)
    app = tmp_path / "Fauna.app"
    exe = app / "Contents" / "MacOS" / "FaunaMacOS"
    exe.parent.mkdir(parents=True)
    drv = object.__new__(MacosInProcessDriver)
    drv._tmp_dir = str(tmp_path)
    drv._app_support = None
    monkeypatch.setattr(MacosInProcessDriver, "_LAUNCH_READY_BUDGET_S", 0.05)
    monkeypatch.setattr(MacosInProcessDriver, "_agent_health_ok", _unhealthy_until(seen))
    _gate_ps(monkeypatch, bundle_exe=str(exe), seen=seen)
    with pytest.raises(TimeoutError) as raised:
        drv._launch_through_launch_services(app, {}, {}, str(tmp_path))
    assert "one-a-second `ps -M` reads" in str(raised.value), raised.value


def test_the_ios_gate_samples_the_printed_pid(monkeypatch):
    seen = threading.Event()
    monkeypatch.setattr(local_launch, "THREAD_SAMPLE_INTERVAL_S", 0.01)
    _no_machine(monkeypatch)
    drv = object.__new__(IosInProcessDriver)
    drv._app_pid = 4242
    drv._app_support = None
    monkeypatch.setattr(IosInProcessDriver, "_LAUNCH_READY_BUDGET_S", 0.05)
    monkeypatch.setattr(IosInProcessDriver, "_agent_health_ok", _unhealthy_until(seen))
    _gate_ps(monkeypatch, bundle_exe="/nowhere", seen=seen)
    with pytest.raises(TimeoutError) as raised:
        drv._await_launched_app("UDID-1")
    assert "one-a-second `ps -M` reads" in str(raised.value), raised.value

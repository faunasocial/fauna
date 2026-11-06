r"""tier_3: the two forced launch failures, on a real fauna-desktop and fauna-tui.

The Linux half of the evidence for convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention), the
twin of `test_launch_timeout_forced_windows.py`: the app's start broken on purpose
reads `FAULTED`, the app's CPU held back on purpose reads `STARVED`, each from the
exception's first line with no log opened by hand. The mechanism is pinned
headlessly in `test_launch_timeout_local.py`; this is what its fakes stand in for —
a real `fauna-desktop` under `xvfb-run` whose own pid the driver must find, and this
machine's agent ledger read for a real launch.

Each test spends a whole launch budget, so a sweep never runs them: they skip unless
`FAUNA_E2E_FORCED_LAUNCH_FAILURES=1` is exported for that one run.

**The fault** is `fauna_e2e_agent::fault_at_launch`, the shared Rust hook both apps
call right after installing their log: `FAUNA_E2E_FAULT_AT_LAUNCH=exit` ends the
launch with code 73, windows' code. Compiled out of a release artifact.

**The burner outranks the app rather than sharing its CPU.** Without privilege
(`RLIMIT_RTPRIO` is 0 on Linux) a burner cannot take a real-time class, as windows'
takes the high priority class, so the app lowers its own instead: it runs as
`sh -c 'exec taskset -c <cpu> chrt --idle 0 <app>'` (every step execs, so the app's
command name is its own and the driver's pid resolution is exercised as in a real
launch), and a `SCHED_OTHER` burner on that CPU leaves a `SCHED_IDLE` task about
0.3 % of it (CFS weight 3 against 1024). The CPU is the second of this process's
allowed set, so it is inside whatever fence the run holds; every other CPU stays
free for the harness, Xvfb and every other session. `nice -n 19` (weight 15) was
measured first, 2026-10-30, and is not enough: the app ran 0.15 core-s at about
1.4 % and answered in 10 s.

**The STARVED case runs on linux only, by measurement.** tui's gate answers
`/health` after 0.01-0.02 core-s, so even `SCHED_IDLE` under a burner lets it up in
1-4 s, and no unprivileged scheduler hold reaches below that inside a 30 s budget.
Its CPU witness is the same `local_launch` read as linux's; what tui's run would add
is a starved launch that times out, and a tui start that cheap does not.
"""

from __future__ import annotations

import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import local_launch  # noqa: E402
from drivers.linux import LinuxBridgeDriver  # noqa: E402
from drivers.port_util import drain_pipes, popen_group_kwargs, reap_descendants_of  # noqa: E402
from drivers.tui import TuiDriver  # noqa: E402
from helpers.app_surface import skip_environment  # noqa: E402
from helpers.waiting import wait_until  # noqa: E402

pytestmark = pytest.mark.tier_3

OPT_IN = "FAUNA_E2E_FORCED_LAUNCH_FAILURES"
#: The app's exit under `FAUNA_E2E_FAULT_AT_LAUNCH=exit`
#: (`fauna_e2e_agent::FAULT_AT_LAUNCH_EXIT_CODE`).
FAULT_EXIT_CODE = 73
#: How long past the launch budget the burner keeps its CPU: the budget's window
#: and a margin for the spawn. Bounded by the burner itself, so a killed run
#: leaves nothing spinning.
BURN_MARGIN_S = 30.0
#: How long a burner gets to say it holds its CPU. Named and generous (convention
#: 14): it says so the moment it does, so a green run pays only its interpreter start.
BURNER_READY_BUDGET_S = 60.0

#: Pins itself to one CPU, says so and spins for its argv's seconds. Own process
#: and own mask, so nothing it does reaches the harness.
BURNER = r"""
import os, sys, time
os.sched_setaffinity(0, {int(sys.argv[1])})
print("burning", flush=True)
end = time.monotonic() + float(sys.argv[2])
while time.monotonic() < end:
    pass
"""

LINUX = pytest.param("linux", marks=pytest.mark.linux, id="linux")
TUI = pytest.param("tui", marks=pytest.mark.tui, id="tui")

_DRIVERS = {"linux": (LinuxBridgeDriver, "fauna-desktop"), "tui": (TuiDriver, "fauna-tui")}


@pytest.fixture(autouse=True)
def _opted_in():
    if not os.environ.get(OPT_IN):
        skip_environment(f"a forced launch failure spends a whole budget; export {OPT_IN}=1")
    if not sys.platform.startswith("linux"):
        skip_environment("the forced pair's pinning and the /proc witness are Linux's")


@pytest.fixture
def launched(request):
    app = request.param
    driver_cls, comm = _DRIVERS[app]
    app_path = request.getfixturevalue(f"{app}_app_path")
    driver = driver_cls()
    yield driver, app_path, comm
    driver.teardown()


def _held_back(cpu: int, app_path: str) -> list[str]:
    """`sh`'s argv for the held-back launch: `exec` into the app pinned to `cpu` in
    `SCHED_IDLE`, so the app's own pid carries its name."""
    return ["-c", f'exec taskset -c {cpu} chrt --idle 0 "$0"', app_path]


def _allowed(pid: int) -> str:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("Cpus_allowed_list:"):
            return line.split(":", 1)[1].strip()
    return "?"


def _held_reading(driver, comm: str, burner) -> str:
    """What the hold actually was, read rather than assumed: the app's own CPU
    time and allowed CPUs, the burner's allowed CPUs. Never raises."""
    try:
        handle = getattr(driver, "_app_proc", None) or getattr(driver, "_pty", None)
        pid = local_launch.descendant_named(handle.pid, comm)
        cpu_s, _ = local_launch.proc_cpu_reading(pid)
        return (
            f"the app (pid {pid}) ran {cpu_s:.2f} core-s on CPUs {_allowed(pid)}, "
            f"policy {os.sched_getscheduler(pid)}; the burner on CPUs "
            f"{_allowed(burner.pid)}"
        )
    except Exception as exc:  # noqa: BLE001 — a diagnosis never masks the failure
        return f"the hold could not be read ({type(exc).__name__}: {exc})"


def _pinned_cpu() -> int:
    allowed = sorted(os.sched_getaffinity(0))
    return allowed[1] if len(allowed) > 1 else allowed[0]


@pytest.mark.parametrize("launched", [LINUX, TUI], indirect=True)
def test_the_app_start_broken_on_purpose_reads_faulted(launched):
    driver, app_path, comm = launched
    config = {"app_path": app_path, "environment": {"FAUNA_E2E_FAULT_AT_LAUNCH": "exit"}}

    with pytest.raises(RuntimeError) as raised:
        driver.launch(config)

    report = str(raised.value)
    print(report)
    first = report.splitlines()[0]
    budget = f"{driver._RELAUNCH_READY_BUDGET_S:.0f}"
    assert first.startswith(
        f"launch timeout ({budget} s): FAULTED — {comm} exited with code {FAULT_EXIT_CODE}"
    ), first


@pytest.mark.parametrize("launched", [LINUX, TUI], indirect=True)
def test_the_app_blocked_on_purpose_reads_stalled_not_starved(launched):
    """`FAUNA_E2E_FAULT_AT_LAUNCH=block` parks the main thread right after the log
    is installed: as little CPU as a starved launch, and no run-queue wait, because
    it asks for none — the app's own, read from `schedstat`."""
    driver, app_path, comm = launched
    config = {"app_path": app_path, "environment": {"FAUNA_E2E_FAULT_AT_LAUNCH": "block"}}

    with pytest.raises((TimeoutError, RuntimeError)) as raised:
        driver.launch(config)

    report = str(raised.value)
    print(report)
    first = report.splitlines()[0]
    budget = f"{driver._RELAUNCH_READY_BUDGET_S:.0f}"
    assert first.startswith(f"launch timeout ({budget} s): STALLED, NOT STARVED — ran "), first
    assert "run-queue wait" in report, report


@pytest.mark.parametrize("launched", [LINUX], indirect=True)
def test_the_apps_cpu_held_back_on_purpose_reads_starved(launched):
    driver, app_path, comm = launched
    cpu = _pinned_cpu()
    seconds = driver._RELAUNCH_READY_BUDGET_S + BURN_MARGIN_S
    burner = subprocess.Popen(
        [sys.executable, "-c", BURNER, str(cpu), str(seconds)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        **popen_group_kwargs(),
    )
    reap_descendants_of(burner.pid)
    output = drain_pipes(burner)
    try:
        # The premise, proved before the launch: a burner that died on its way up
        # holds nothing back, and the launch would read as the app's own good health.
        wait_until(
            lambda: any("burning" in line for line in list(output)),
            BURNER_READY_BUDGET_S,
            diagnose=lambda: f"exit {burner.poll()}: " + "\n".join(list(output)),
        )
        config = {"app_path": "sh", "app_args": shlex.join(_held_back(cpu, app_path))}
        started = time.monotonic()
        try:
            driver.launch(config)
        except TimeoutError as exc:
            report = str(exc)
        else:
            pytest.fail(
                f"the app came up {time.monotonic() - started:.0f} s after the launch, "
                f"pinned to CPU {cpu} in SCHED_IDLE, the burner "
                f"{'running' if burner.poll() is None else 'gone'}: "
                f"this did not hold it back enough — {_held_reading(driver, comm, burner)}"
            )
        assert burner.poll() is None, (
            f"the burner ended after {time.monotonic() - started:.0f} s, before the "
            f"budget did: nothing held this app back (exit {burner.returncode})"
        )
    finally:
        burner.kill()
        burner.wait(timeout=30)

    print(report)
    first = report.splitlines()[0]
    budget = f"{driver._RELAUNCH_READY_BUDGET_S:.0f}"
    assert first.startswith(f"launch timeout ({budget} s): STARVED — the app ran "), first
    assert " and waited for a CPU " in first, first

r"""tier_3: the three forced launch failures, on a real FaunaApp, read as what they are.

The evidence half of convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention): the
app's start broken on purpose reads `FAULTED`, the app's CPU held back on purpose
reads `STARVED`, and the app blocked on purpose — asking for no CPU — reads
`STALLED, NOT STARVED`, each from the exception's first line with no log opened by
hand. The third is the one the quarter-core rule alone misread as `STARVED`.
The mechanism is pinned headlessly in `test_launch_timeout_report.py` and
`test_windows_launch_timeout.py`; this is what those fakes stand in for.

Each test spends a whole launch budget, so a sweep never runs them: they skip unless
`FAUNA_E2E_FORCED_LAUNCH_FAILURES=1` is exported for that one run.

**The burner outranks the app rather than sharing its CPU:** it runs in the high
priority class on the one CPU the app is pinned to. CPU 1 because the core fence's
floor is two CPUs, so it is always inside the fence, and because its mask is not a
lowest-n one: the fence's sweep leaves that alone as a deliberate pin. The other
CPUs stay free for the harness and for every other session.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import windows as win  # noqa: E402
from drivers.port_util import drain_pipes, popen_group_kwargs, reap_descendants_of  # noqa: E402
from helpers.app_surface import skip_environment  # noqa: E402
from helpers.waiting import wait_until  # noqa: E402

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

OPT_IN = "FAUNA_E2E_FORCED_LAUNCH_FAILURES"
#: The one CPU the starved launch and its burner share.
PINNED_CPU_MASK = 0b10
#: The app's exit under `FAUNA_E2E_FAULT_AT_LAUNCH=exit`
#: (`App.xaml.cs::E2eFaultAtLaunchExitCode`); `=block` parks its UI thread instead.
FAULT_EXIT_CODE = 73
#: How long past the launch budget the burner keeps its CPU: the budget's window, and
#: a margin for the launch post and the two seconds before the gate opens. Bounded by
#: the burner itself, so a killed run leaves nothing spinning.
BURN_MARGIN_S = 30.0

#: How long a burner gets to say it holds its CPU. Named and generous (convention
#: 14): it says so the moment it does, so a green run pays only its interpreter start.
BURNER_READY_BUDGET_S = 60.0

#: Pins itself to one CPU, takes the high priority class, says so and spins for its
#: argv's seconds. Own process and own mask, so nothing it does reaches the harness.
BURNER = r"""
import ctypes, ctypes.wintypes as w, sys, time
k32 = ctypes.WinDLL("kernel32")
k32.GetCurrentProcess.restype = w.HANDLE
k32.SetProcessAffinityMask.argtypes = [w.HANDLE, ctypes.c_size_t]
k32.SetPriorityClass.argtypes = [w.HANDLE, w.DWORD]
me = k32.GetCurrentProcess()
assert k32.SetProcessAffinityMask(me, int(sys.argv[1]))
assert k32.SetPriorityClass(me, 0x80)
print("burning", flush=True)
end = time.monotonic() + float(sys.argv[2])
while time.monotonic() < end:
    pass
"""


@pytest.fixture(autouse=True)
def _opted_in():
    if not os.environ.get(OPT_IN):
        skip_environment(f"a forced launch failure spends a whole budget; export {OPT_IN}=1")


@pytest.fixture
def driver():
    d = win.WindowsBridgeDriver()
    yield d
    d.teardown()


def _launch_config(windows_app_path: str, **extra) -> dict:
    return {"app_path": windows_app_path, "args": "", "environment": {}, **extra}


def test_the_app_start_broken_on_purpose_reads_faulted(driver, windows_app_path):
    config = _launch_config(
        windows_app_path, environment={"FAUNA_E2E_FAULT_AT_LAUNCH": "exit"}
    )

    with pytest.raises(TimeoutError) as raised:
        driver.launch(config)

    report = str(raised.value)
    print(report)
    first = report.splitlines()[0]
    budget = f"{driver._RELAUNCH_READY_BUDGET_S:.0f}"
    assert first.startswith(f"launch timeout ({budget} s): FAULTED — exit code {FAULT_EXIT_CODE} "), first


def test_the_app_blocked_on_purpose_reads_stalled_not_starved(driver, windows_app_path):
    """A launch that blocks spends as little CPU as a starved one, because it asks
    for none: the thread table shows no Ready thread, and the verdict is the app's."""
    config = _launch_config(
        windows_app_path, environment={"FAUNA_E2E_FAULT_AT_LAUNCH": "block"}
    )

    with pytest.raises(TimeoutError) as raised:
        driver.launch(config)

    report = str(raised.value)
    print(report)
    first = report.splitlines()[0]
    budget = f"{driver._RELAUNCH_READY_BUDGET_S:.0f}"
    assert first.startswith(f"launch timeout ({budget} s): STALLED, NOT STARVED — ran "), first
    assert "a thread Ready at" in report, report


def test_the_apps_cpu_held_back_on_purpose_reads_starved(driver, windows_app_path):
    seconds = driver._RELAUNCH_READY_BUDGET_S + BURN_MARGIN_S
    # The base interpreter: a venv's `python.exe` is a launcher whose child does the
    # work, and `kill` below reaches only the process it holds.
    interpreter = getattr(sys, "_base_executable", sys.executable)
    burner = subprocess.Popen(
        [interpreter, "-c", BURNER, str(PINNED_CPU_MASK), str(seconds)],
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
        config = _launch_config(windows_app_path, cpu_affinity_mask=PINNED_CPU_MASK)
        started = time.monotonic()
        try:
            driver.launch(config)
        except TimeoutError as exc:
            report = str(exc)
        else:
            status = driver._get("/session/status")
            pytest.fail(
                f"the app came up {time.monotonic() - started:.0f} s after the launch "
                f"on {status.get('cpu_ms')} ms of CPU, CPU mask "
                f"{status.get('cpu_affinity_mask')}, the burner "
                f"{'running' if burner.poll() is None else 'gone'}: "
                f"this did not hold it back enough"
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

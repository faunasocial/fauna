r"""tier_3: the forced launch failures, on a real FaunaMacOS and a real iOS simulator app.

The macOS half of the evidence for convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention), the twin
of `test_launch_timeout_forced_local.py` and `test_launch_timeout_forced_windows.py`:
the app's start broken on purpose reads `FAULTED`, the app parked on purpose reads
`STALLED, NOT STARVED`, each from the exception's first line with no log opened by
hand. The mechanism is pinned headlessly in `test_launch_timeout_apple.py`; this is
what its fake `ps -M` stands in for: a real app's thread table, read once a second
through a real gate.

Each test spends a whole launch budget, so a sweep never runs them: they skip unless
`FAUNA_E2E_FORCED_LAUNCH_FAILURES=1` is exported for that one run.

**The fault** is `TestAgentBootstrap.faultAtLaunchIfAsked`, the Swift twin of
`fauna_e2e_agent::fault_at_launch`, which both apps call right after installing their
log: `FAUNA_E2E_FAULT_AT_LAUNCH=exit` ends the launch with code 73, `block` parks the
main thread for good, before the agent starts. Compiled out of a release artifact.

**iOS has no exit code to read.** The app is the simulator's child, not the harness's,
so its exit reads as the printed pid gone from `ps`, at the deadline.

**The STARVED case is not run, by the platform.** Holding the app back needs it to
share a CPU with a burner, and darwin has no unprivileged way to pin a process to one
CPU (no `sched_setaffinity`, no `taskset`). The only hold left is burners on every CPU
of the box for the whole budget, which takes those CPUs from every other session on
it. The STARVED branch stays pinned against the fake in `test_launch_timeout_apple.py`.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import create_driver  # noqa: E402
from helpers.app_launch import app_launch_config  # noqa: E402
from helpers.app_surface import skip_environment  # noqa: E402

pytestmark = pytest.mark.tier_3

OPT_IN = "FAUNA_E2E_FORCED_LAUNCH_FAILURES"
#: The app's exit under `FAUNA_E2E_FAULT_AT_LAUNCH=exit`
#: (`TestAgentBootstrap.faultAtLaunchExitCode`).
FAULT_EXIT_CODE = 73

MACOS = pytest.param("macos", marks=pytest.mark.macos, id="macos")
IOS = pytest.param("ios", marks=pytest.mark.ios, id="ios")


@pytest.fixture(autouse=True)
def _opted_in():
    if not os.environ.get(OPT_IN):
        skip_environment(f"a forced launch failure spends a whole budget; export {OPT_IN}=1")


@pytest.fixture
def launched(request):
    app = request.param
    config = app_launch_config(app, request)
    driver = create_driver(app)
    yield app, driver, config
    driver.teardown()


def _budget(driver) -> str:
    return f"{driver._LAUNCH_READY_BUDGET_S:.0f}"


@pytest.mark.parametrize("launched", [MACOS, IOS], indirect=True)
def test_the_app_start_broken_on_purpose_reads_faulted(launched):
    app, driver, config = launched
    config["environment"] = {"FAUNA_E2E_FAULT_AT_LAUNCH": "exit"}

    with pytest.raises((TimeoutError, RuntimeError)) as raised:
        driver.launch(config)

    report = str(raised.value)
    print(report)
    first = report.splitlines()[0]
    verdict = f"launch timeout ({_budget(driver)} s): FAULTED — "
    if app == "macos":
        assert first == f"{verdict}FaunaMacOS exited with code {FAULT_EXIT_CODE}", first
    else:
        assert first.startswith(f"{verdict}{driver.BUNDLE_ID} (pid "), first
        assert first.endswith("is no longer running"), first


@pytest.mark.parametrize("launched", [MACOS, IOS], indirect=True)
def test_the_app_blocked_on_purpose_reads_stalled_not_starved(launched):
    """As little CPU as a starved launch, and no runnable thread, because it asks for
    none: the app's own, read from its thread table once a second."""
    _app, driver, config = launched
    config["environment"] = {"FAUNA_E2E_FAULT_AT_LAUNCH": "block"}

    with pytest.raises(TimeoutError) as raised:
        driver.launch(config)

    report = str(raised.value)
    print(report)
    first = report.splitlines()[0]
    assert first.startswith(f"launch timeout ({_budget(driver)} s): STALLED, NOT STARVED — ran "), first
    assert "one-a-second `ps -M` reads" in report, report

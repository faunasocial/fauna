r"""tier_3: a real fauna-desktop and fauna-tui that crash before the harness's close
fail the phase that found them, naming the signal.

The Linux half of the evidence for convention 6's close rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention), the
real-app twin of `test_local_close_reads_exit.py`, whose fakes this stands in for:
a real `fauna-desktop` under `xvfb-run` (whose shell hands back the app's
`SIGABRT` as exit code 134) and a real `fauna-tui` on its pty (its own status).

**The fault** is `fauna_e2e_agent::fault_at_reset`, the shared Rust hook both apps
call at the head of their test-agent `reset` arm: `FAUNA_E2E_FAULT_AT_RESET=abort`
aborts the process — the death that lands between a test's last step and the
harness's close, which the close used to end as if the app were alive. Compiled
out of a release artifact.

The test drains the recorded death itself and asserts its first line — the very
text the phase wrapper (`helpers/app_deaths.py`) raises as `AppDiedBeforeClose`
when nothing drains it — and that the close went on.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.linux import LinuxBridgeDriver  # noqa: E402
from drivers.tui import TuiDriver  # noqa: E402
from helpers import app_deaths  # noqa: E402
from helpers.app_surface import skip_environment  # noqa: E402

pytestmark = pytest.mark.tier_3

#: The reset's acknowledgement budget here: the abort lands as the command does, so
#: the budget only bounds how long the driver keeps asking a dead app. Measured
#: 2026-11-05: both apps' deaths were found by the in-line relaunch, +1 s after launch.
RESET_BUDGET_S = 10.0

LINUX = pytest.param("linux", marks=pytest.mark.linux, id="linux")
TUI = pytest.param("tui", marks=pytest.mark.tui, id="tui")

#: (driver class, the app's command name, how its SIGABRT reads at the close)
_APPS = {
    "linux": (
        LinuxBridgeDriver,
        "fauna-desktop",
        # Under `xvfb-run` the handle is the wrapper's shell; without it, the app.
        ("was killed by SIGABRT (signal 6), read as exit code 134 through xvfb-run",
         "was killed by SIGABRT (signal 6)"),
    ),
    "tui": (TuiDriver, "fauna-tui", ("was killed by SIGABRT (signal 6)",)),
}


@pytest.fixture(autouse=True)
def _linux_only():
    if not sys.platform.startswith("linux"):
        skip_environment("the forced abort's signal reading and the /proc witness are Linux's")


@pytest.mark.parametrize("app", [LINUX, TUI])
def test_an_app_aborted_at_its_reset_fails_the_close_that_found_it(request, app):
    driver_cls, comm, phrases = _APPS[app]
    app_path = request.getfixturevalue(f"{app}_app_path")
    driver = driver_cls()
    app_deaths.drain()
    try:
        driver.launch({"app_path": app_path, "environment": {"FAUNA_E2E_FAULT_AT_RESET": "abort"}})
        # The reset the app dies in. Its outcome is not the subject: the driver's
        # HTTP layer meets the dead agent and relaunches in-line (`recover()`, the
        # path that hid such deaths), or the acknowledgement times out and the
        # teardown closes the corpse — either close must name the death.
        try:
            driver.reset(timeout=RESET_BUDGET_S)
        except Exception as exc:  # noqa: BLE001 - see above
            print(f"reset ended: {type(exc).__name__}: {exc}".splitlines()[0])
    finally:
        driver.teardown()

    found = app_deaths.drain()
    assert len(found) == 1, found
    print(found[0])
    first = found[0].splitlines()[0]
    assert any(
        first in (
            f"the app died before the relaunch in recover() closed it: {comm} {phrase}",
            f"the app died before teardown closed it: {comm} {phrase}",
        )
        for phrase in phrases
    ), first
    assert "FAUNA_E2E_FAULT_AT_RESET=abort" in found[0], found[0]

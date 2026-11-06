r"""tier_3: a real FaunaMacOS and a real iOS simulator app that crash before the
harness's close fail the phase that found them.

The macOS half of the evidence for convention 6's close rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention), the
twin of `test_app_death_at_close_forced_local.py`, and the real-app proof of the
fakes in `test_apple_close_reads_exit.py`.

**The fault** is `TestAgentBootstrap.faultAtResetIfAsked`, the Swift twin of
`fauna_e2e_agent::fault_at_reset` (the apple apps do not link the Rust agent), which
the shared in-process automation server calls at the head of its `reset` and
`logout` arms: `FAUNA_E2E_FAULT_AT_RESET=abort` ends the app with a Swift
`fatalError` — a trap, `SIGTRAP` on arm64. Compiled out of a release artifact.

**macOS** holds its app's Popen, so the death reads as the signal. **iOS** holds only
the pid `simctl launch` printed: the death reads as that pid gone, with the witness
that makes it a crash — the crash report ReportCrash wrote for it (`EXC_BREAKPOINT`,
`SIGTRAP`), or the FATAL line in the container's log, which the shared log's
non-blocking writer may not flush before the trap (measured 2026-11-05: it did not,
and the report was the witness). A Swift trap's report carries no message, so on
iOS the fault is named by the report's faulting frame (`faultAtResetIfAsked`); on
macOS by the hook's FATAL line in the app's stderr.

The test drains the recorded death itself and asserts its first line — the very
text the phase wrapper (`helpers/app_deaths.py`) raises as `AppDiedBeforeClose`
when nothing drains it — and that the close went on.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import create_driver  # noqa: E402
from helpers import app_deaths  # noqa: E402
from helpers.app_launch import app_launch_config  # noqa: E402

pytestmark = pytest.mark.tier_3

#: The reset's acknowledgement budget here: the crash lands as the command does, so
#: the budget only bounds how long the driver keeps asking a dead app.
RESET_BUDGET_S = 10.0

MACOS = pytest.param("macos", marks=pytest.mark.macos, id="macos")
IOS = pytest.param("ios", marks=pytest.mark.ios, id="ios")

_CLOSES = ("the relaunch in recover()", "teardown")


@pytest.mark.parametrize("app", [MACOS, IOS])
def test_an_app_crashed_at_its_reset_fails_the_close_that_found_it(request, app):
    config = app_launch_config(app, request)
    config["environment"] = {"FAUNA_E2E_FAULT_AT_RESET": "abort"}
    driver = create_driver(app)
    app_deaths.drain()
    try:
        driver.launch(config)
        # The reset the app dies in. Its outcome is not the subject: the driver's
        # HTTP layer meets the dead agent and relaunches in-line (`recover()`, the
        # path that hid such deaths) — the relaunch keeps the fault, so it may die
        # again at a later reset — or the acknowledgement times out and the
        # teardown closes the corpse. Every close must name the death it finds.
        try:
            driver.reset(timeout=RESET_BUDGET_S)
        except Exception as exc:  # noqa: BLE001 - see above
            print(f"reset ended: {type(exc).__name__}: {exc}".splitlines()[0])
    finally:
        driver.teardown()

    found = app_deaths.drain()
    print("\n\n".join(found))
    assert found, "the crashed app's close recorded no death"
    first = found[0].splitlines()[0]
    if app == "macos":
        assert first in {
            f"the app died before {close} closed it: FaunaMacOS was killed by SIGTRAP (signal 5)"
            for close in _CLOSES
        }, first
    else:
        assert any(
            first.startswith(f"the app died before {close} closed it: FaunaiOS (pid ")
            for close in _CLOSES
        ), first
        assert "was gone before the close, a crash by" in first, first
    assert (
        "FAUNA_E2E_FAULT_AT_RESET=abort" in found[0] or "faultAtResetIfAsked" in found[0]
    ), found[0]


@pytest.mark.parametrize("app", [MACOS, IOS])
def test_a_clean_reset_and_close_record_nothing(request, app):
    """The control: the same launch, reset and close without the fault are silent."""
    config = app_launch_config(app, request)
    driver = create_driver(app)
    app_deaths.drain()
    try:
        driver.launch(config)
        driver.reset(timeout=RESET_BUDGET_S * 3)
    finally:
        driver.teardown()
    assert app_deaths.drain() == []

"""tier_3 e2e: an android app that crashes in a reset is named by the close after it.

Convention 6's close rider, the android leg, witnessed on the device
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention, and
§ Implementation status today). The `app` fixture's reset that opens a test is
where an app left in a bad state by the test before it dies; the fixture then
relaunches through `recover()`, and before the rider that relaunch handed the
next test a live app with no word of the death.

The fault is the debug test agent's `arm_crash_on_next_reset` (debug source set
only, so compiled out of release): the next `reset` throws on the app's main
looper, the runtime's FATAL EXCEPTION. The instrumentation bridge lives in the
app's process, so the crash takes the bridge with it. This test owns its driver
for that reason — a crash on the session-scoped driver would leave every later
test of the session without a bridge — and drains the registry itself, so the
death it provoked is asserted here rather than failing its own phase.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conftest import _seeded_environment
from drivers import create_driver
from helpers import app_deaths

pytestmark = [pytest.mark.tier_3, pytest.mark.android]

FAULT_LINE = (
    "FATAL EXCEPTION: main — java.lang.IllegalStateException: "
    "e2e crash fault armed by arm_crash_on_next_reset"
)


def _launch_config(request) -> dict:
    from helpers import android_device

    return {
        "app_path": request.getfixturevalue("android_app_path"),
        **android_device.run_launch_facts(Path(__file__).resolve().parents[3]),
    }


def test_a_crash_in_the_reset_is_named_by_the_relaunch_after_it(nest_instance, request):
    driver = create_driver("android")
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": {},
        "environment": _seeded_environment(request, nest_instance),
        **_launch_config(request),
    })
    try:
        app_deaths.drain()
        driver.call_command("arm_crash_on_next_reset")
        with pytest.raises(Exception) as reset_failure:
            driver.reset()
        # The fixture's next move, made by hand: the relaunch reads the exit
        # before it closes anything, whether or not the bridge survived.
        driver.recover()
        deaths = app_deaths.drain()
        assert deaths, (
            "the app crashed in its reset, and the relaunch after it recorded no death; "
            f"the reset failed with {type(reset_failure.value).__name__}: {reset_failure.value}"
        )
        first = deaths[0].splitlines()[0]
        assert first == f"the app died before the relaunch in recover() closed it: {FAULT_LINE}", deaths[0]
    finally:
        try:
            driver.teardown()
        finally:
            # The teardown's own read must not count the same death twice.
            assert app_deaths.drain() == [], "the teardown re-recorded a death the relaunch already found"

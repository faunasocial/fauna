"""tier_1: web paints ``onboarding.launch.launch_failed`` only on the launch surface.

Owner: ``docs/goal/behavior/onboarding.md`` § App-launch routing — "A launch that
fails is not a routing row": ``launch_failed`` is the failed-launch surface's
alone (``surfaceLaunchFailure`` and the two slot-unreadable arms). The locked
surface's stolen-identity button with no secret key held is not a startup
failure and restarting reaches the same step, so ``succeedFromLocked`` paints
its own string (``onboarding.launch.locked_secret_missing``).

Pure source analysis of the web onboarding route; no driver, no nest.
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_PAGE = os.path.normpath(
    os.path.join(
        os.path.dirname(__file__),
        "..", "..", "..", "apps", "fauna-web", "src", "routes", "onboarding", "+page.svelte",
    )
)


def _source() -> str:
    with open(_PAGE, encoding="utf-8") as f:
        return f.read()


def _function_body(src: str, name: str) -> str:
    start = src.index(f"async function {name}(")
    # The next top-level function in this file ends the body.
    end = re.search(r"\n  (?:async )?function \w+\(", src[start + 1 :])
    return src[start : start + 1 + end.start()] if end else src[start:]


def test_succeed_from_locked_does_not_paint_launch_failed():
    body = _function_body(_source(), "succeedFromLocked")
    assert "launch_failed" not in body
    assert "t.onboarding.launch.locked_secret_missing" in body


def test_launch_failed_is_painted_only_by_the_launch_failure_sites():
    src = _source()
    uses = [m.start() for m in re.finditer(r"onboarding\.launch\.launch_failed", src)]
    # surfaceLaunchFailure + the two slot-unreadable arms.
    assert len(uses) == 3, f"unexpected launch_failed painters: {len(uses)}"

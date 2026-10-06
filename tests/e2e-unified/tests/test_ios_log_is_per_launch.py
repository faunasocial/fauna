"""iOS's app log answers for THIS launch, even when the container is pinned.

`helpers/real_rail_control.witness_real_rail` — the precondition every
real-conversations test on a launch-gate app now runs through — asks the app's
own log a yes/no question: did the real FaunaMls rail start? The answer is only
as good as the reader's scope. Three families close that by construction (macOS,
linux and tui mint a fresh tmp dir and reopen `app.err` in every `launch()`), and
windows' genuinely append-shared log never reaches the control. iOS is the one
family that can carry a dead predecessor's words into a live launch: `launch()`
normally uninstalls the data container — which takes `logs/` with it — but
`preserve_state_across_relaunch()` pins the container, and that pin survives into
the module-boundary relaunch because `module_relaunch` runs BEFORE the
`driver.reset()` that clears it.

**Red-first against the real shape, not a convenient one.** The obvious version
of this test — relaunch a driver and assert the control still fails — passes on
macOS against an unchanged tree, because macOS was never the problem. So the
subject here is the iOS reader itself, with a pinned container simulated by a
directory that outlives the "launch": without the per-launch floor these
assertions fail, and they fail by returning the predecessor's line.

The floor is taken between `simctl install` and `simctl launch` — the one window
where the container exists and the app is not running — so it needs no simulator
to exercise, only a directory. What still needs a simulator, and is therefore
not claimed here, is that a real pinned relaunch lands in this same code path.
"""
from pathlib import Path

import pytest

from drivers.ios import IosInProcessDriver

pytestmark = pytest.mark.tier_1

_PREVIOUS = "INFO mls-sync: cross-device plane wired (2 channel(s) restored from replica)\n"
_CURRENT = "INFO some other line from the live launch\n"


def _driver_over(app_support: Path) -> IosInProcessDriver:
    """An iOS driver with nothing set up but the container path.

    `object.__new__` rather than the constructor on purpose: the two methods
    under test read `_app_support` and `_log_baseline` and nothing else, and a
    real construction would want a simulator this test has no business needing.
    """
    driver = object.__new__(IosInProcessDriver)
    driver._app_support = str(app_support)
    driver._log_baseline = None
    return driver


def _write(app_support: Path, name: str, text: str) -> Path:
    logs = app_support / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    path = logs / name
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(text)
    return path


def test_a_pinned_containers_previous_launch_is_not_read_as_this_ones(tmp_path):
    """The defect this floor exists for: the dead launch said `mls-sync:`, the
    live one never did, and the reader must not answer for the dead one."""
    _write(tmp_path, "fauna.log", _PREVIOUS)

    driver = _driver_over(tmp_path)
    driver._mark_log_baseline()          # between install and launch
    _write(tmp_path, "fauna.log", _CURRENT)   # the live launch's own words

    text = driver.app_log_text()
    assert _CURRENT.strip() in text, "this launch's own lines must still be read"
    assert "mls-sync:" not in text, (
        "the reader returned a line the PREVIOUS launch wrote — so a control "
        "asking 'did the real rail start THIS launch' would be satisfied by a "
        f"ghost. Got: {text!r}"
    )


def test_an_unpinned_launch_reads_everything_it_wrote(tmp_path):
    """The common case — the container was uninstalled, so there is no floor and
    the reader must not now under-report."""
    driver = _driver_over(tmp_path)
    driver._mark_log_baseline()          # nothing there yet: floor is empty
    _write(tmp_path, "fauna.log", _PREVIOUS)

    assert "mls-sync:" in driver.app_log_text()


def test_a_log_file_created_after_the_floor_is_read_whole(tmp_path):
    """A rolling log that rotates to a NEW filename during the live launch has
    no floor of its own, and must be read from its first byte rather than
    inheriting some other file's offset."""
    _write(tmp_path, "fauna-day1.log", _PREVIOUS)

    driver = _driver_over(tmp_path)
    driver._mark_log_baseline()
    _write(tmp_path, "fauna-day2.log", _CURRENT)

    text = driver.app_log_text()
    assert _CURRENT.strip() in text
    assert "mls-sync:" not in text


def test_no_container_is_still_the_deliberate_None(tmp_path):
    """`None` is a verdict ("the app never started or never logged"), distinct
    from the no-section-at-all a family with no reader gets — the floor must not
    quietly turn it into an empty string."""
    driver = _driver_over(tmp_path)
    driver._mark_log_baseline()
    assert driver.app_log_text() is None

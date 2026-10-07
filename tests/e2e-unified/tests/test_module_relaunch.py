"""Module-boundary cold relaunch is a 7-app fixture contract, not apple's workaround.

`testing.md` § point 10 (a launch is isolated from the box it runs on). The
`app` fixture cold-relaunches the session-scoped app process at every test-
MODULE boundary, so each module starts against a fresh app instance — the same
baseline it would see run in isolation. The decision is driver-type-free:
`helpers/module_relaunch.py` asks `driver.supports_cold_relaunch()` and routes
through `driver.recover()`; it never names platforms.

Two properties are load-bearing:

  * **The capability is derived, never tabulated.** `supports_cold_relaunch()`
    answers whether the driver class actually overrides `recover()` (the base
    implementation only health-probes the bridge — it relaunches nothing), so
    a driver the table "never heard of" cannot silently skip the contract —
    convention 7's `app_capabilities.py` lesson.

  * **An absence is declared and pinned, never silent.** Android's bridge
    keeps no relaunchable session today, so its driver answers False; the pin
    below fails the moment it gains a real `recover()`, at which point the
    absence note (drivers/android.py) and the capture go with it.
"""

import pytest

from helpers import module_relaunch as mr

pytestmark = pytest.mark.tier_1


class _Driver:
    def __init__(self, capable=True, recover_result=True):
        self._capable = capable
        self._recover_result = recover_result
        self.recover_calls = 0

    def supports_cold_relaunch(self):
        return self._capable

    def recover(self):
        self.recover_calls += 1
        return self._recover_result


# ── The boundary decision ───────────────────────────────────────────────────

def test_the_sessions_first_module_does_not_relaunch():
    # _driver_cache just launched the app fresh; a relaunch would pay the
    # session's most expensive launch twice for nothing.
    d = _Driver()
    assert mr.at_module_boundary(d, "tests.test_a") is None
    assert d.recover_calls == 0


def test_tests_within_one_module_share_the_instance():
    d = _Driver()
    mr.at_module_boundary(d, "tests.test_a")
    assert mr.at_module_boundary(d, "tests.test_a") is None
    assert d.recover_calls == 0


def test_a_module_change_relaunches_exactly_once():
    d = _Driver()
    mr.at_module_boundary(d, "tests.test_a")
    outcome = mr.at_module_boundary(d, "tests.test_b")
    assert d.recover_calls == 1
    assert "test_a" in outcome and "test_b" in outcome
    # The new module's remaining tests pay nothing.
    assert mr.at_module_boundary(d, "tests.test_b") is None
    assert d.recover_calls == 1


def test_returning_to_an_earlier_module_is_still_a_boundary():
    # pytest normally runs a module contiguously, but -p no:randomly is not
    # guaranteed everywhere; the contract is "boundary", not "new name".
    d = _Driver()
    mr.at_module_boundary(d, "tests.test_a")
    mr.at_module_boundary(d, "tests.test_b")
    mr.at_module_boundary(d, "tests.test_a")
    assert d.recover_calls == 2


def test_a_failed_relaunch_reports_and_still_advances():
    d = _Driver(recover_result=False)
    mr.at_module_boundary(d, "tests.test_a")
    outcome = mr.at_module_boundary(d, "tests.test_b")
    assert outcome is not None and "FAILED" in outcome
    # State advanced: the next test in the module must not retry a relaunch
    # storm — the fixture's reset() path owns surfacing the wedge.
    assert mr.at_module_boundary(d, "tests.test_b") is None
    assert d.recover_calls == 1


def test_an_unnamed_module_never_triggers_a_relaunch():
    d = _Driver()
    mr.at_module_boundary(d, None)
    assert mr.at_module_boundary(d, "tests.test_a") is None
    assert d.recover_calls == 0


def test_an_incapable_driver_is_never_asked_to_recover():
    d = _Driver(capable=False)
    mr.at_module_boundary(d, "tests.test_a")
    first = mr.at_module_boundary(d, "tests.test_b")
    assert d.recover_calls == 0
    # The absence is announced at the first boundary it would have acted on —
    # visible in the run output, never a silent skip …
    assert first is not None and "declared absence" in first
    # … and announced once, not per boundary.
    assert mr.at_module_boundary(d, "tests.test_c") is None
    assert d.recover_calls == 0


# ── The capability pin ──────────────────────────────────────────────────────

def test_every_driver_declares_its_relaunch_capability():
    """Equality-pinned per driver, instantiating each class and asking the
    real method — a hand-maintained expectation, deliberately: when a driver's
    status changes (android gaining a real recover()), this fails and forces
    the absence declaration and its NEXT capture to move in the same change.
    """
    from drivers.android import AndroidBridgeDriver
    from drivers.ios import IosInProcessDriver
    from drivers.linux import LinuxBridgeDriver
    from drivers.macos import MacosInProcessDriver
    from drivers.tui import TuiDriver
    from drivers.web import WebBridgeDriver
    from drivers.windows import WindowsBridgeDriver

    expected = {
        TuiDriver: True,
        LinuxBridgeDriver: True,
        WebBridgeDriver: True,
        WindowsBridgeDriver: True,
        MacosInProcessDriver: True,
        IosInProcessDriver: True,
        # Android's recover() is the base bridge health-probe: it relaunches
        # nothing, so the module-boundary contract is a declared absence there
        # (drivers/android.py — build recover() when
        # host e2e exists, then flip this pin).
        AndroidBridgeDriver: False,
    }
    actual = {
        cls: cls().supports_cold_relaunch() for cls in expected
    }
    assert actual == expected


def test_every_driver_declares_what_its_log_says_after_a_relaunch():
    """The relaunch contract's other half, and the one that was an ACCIDENT.

    `supports_cold_relaunch()` above says whether a driver relaunches the app;
    this says what its app-log reader answers once it has. That matters because
    `helpers/real_rail_control.witness_real_rail` is a precondition of every
    real-conversations test on a launch-gate app and asks the log a yes/no
    question — so a reader that can answer with a DEAD launch's words makes a
    control that must not be satisfiable by anything but the real rail
    satisfiable by a ghost.

    Until 2026-09-22 this was true of three families only because each happened
    to mint a fresh tmp dir in `launch()`; nothing said they had to, and a note
    in the control claimed — wrongly, and unmeasured — that none of them did
    . Equality-pinned like its sibling: a driver that changes its log
    policy fails here and has to say so, rather than silently weakening every
    control built on the reader.
    """
    from drivers.android import AndroidBridgeDriver
    from drivers.ios import IosInProcessDriver
    from drivers.linux import LinuxBridgeDriver
    from drivers.macos import MacosInProcessDriver
    from drivers.tui import TuiDriver
    from drivers.web import WebBridgeDriver
    from drivers.windows import WindowsBridgeDriver

    expected = {
        TuiDriver: "per-launch",
        LinuxBridgeDriver: "per-launch",
        MacosInProcessDriver: "per-launch",
        # iOS is per-launch by uninstall normally, and by `_mark_log_baseline()`'s
        # byte floor when `preserve_state_across_relaunch()` pins the container —
        # the one family where it had to be BUILT rather than inherited.
        IosInProcessDriver: "per-launch",
        # windows' data-dir log is append-shared; `_app_log_since(mark)` exists
        # for exactly that. It never reaches the real-rail control (it takes the
        # readiness-poll branch), so this is a declaration, not a hole.
        WindowsBridgeDriver: "cumulative",
        # A 500-entry ring: absence is never evidence, whatever the launch.
        WebBridgeDriver: "evicting",
        # filesDir (and its `fauna_log` files) survives every relaunch, so the
        # reader slices from a byte floor taken before `/session` — the iOS shape.
        AndroidBridgeDriver: "per-launch",
    }
    actual = {cls: cls().log_scope_across_relaunch() for cls in expected}
    assert actual == expected, (
        "a driver's log scope across a relaunch changed, or a new driver has "
        "not declared one (the base answers 'unknown' so that it cannot pass "
        "silently). Every control that reads the app's own log as evidence "
        "depends on this answer — see drivers/base.py::log_scope_across_relaunch."
    )

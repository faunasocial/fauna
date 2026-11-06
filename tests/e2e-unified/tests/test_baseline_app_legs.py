"""Pin the `app_legs` block of a --baseline-json payload (apple-e2e-automation.md
§ Baseline discipline): a leg whose setup read `ok` and whose app was then lost
mid-run must report dead, so the apple baseline sweep refuses the write.

Client-independent (listed in conftest ``_CLIENT_INDEPENDENT_FILES``): drives the
LIVE parent-conftest module, every mutation of its memo snapshotted and restored
so the surrounding session keeps its real state — no driver, no app.
"""

from __future__ import annotations

import pytest

from drivers.inprocess_agent import AppArtifactMissing

pytestmark = pytest.mark.tier_1


def _e2e_conftest():
    import conftest as mod

    assert hasattr(mod, "_baseline_app_legs"), (
        f"'import conftest' resolved to {mod.__file__!r}, which has no "
        "_baseline_app_legs — expected tests/e2e-unified/conftest.py"
    )
    return mod


@pytest.fixture
def legs_isolated():
    """The live conftest with its build memo and lost-leg record cleared."""
    mod = _e2e_conftest()
    memo, lost = dict(mod._BINARY_BUILD_MEMO), dict(mod._LOST_APP_LEGS)
    mod._BINARY_BUILD_MEMO.clear()
    mod._LOST_APP_LEGS.clear()
    try:
        yield mod
    finally:
        mod._BINARY_BUILD_MEMO.clear()
        mod._BINARY_BUILD_MEMO.update(memo)
        mod._LOST_APP_LEGS.clear()
        mod._LOST_APP_LEGS.update(lost)


def _excinfo(exc):
    try:
        raise exc
    except BaseException:
        return pytest.ExceptionInfo.from_current()


def _set_up_ios(mod):
    mod._BINARY_BUILD_MEMO["app:ios"] = ("ok", "/tmp/Fauna.app")
    mod._BINARY_BUILD_MEMO["ios-setup"] = ("ok", {"udid": "x"})


def test_a_leg_set_up_and_never_lost_reads_ok(legs_isolated):
    _set_up_ios(legs_isolated)
    assert legs_isolated._baseline_app_legs() == {"ios": "ok"}


def test_a_leg_set_up_then_lost_mid_run_reads_dead(legs_isolated):
    mod = legs_isolated
    _set_up_ios(mod)
    mod._record_lost_app_leg(
        "tests/test_a.py::test_one",
        _excinfo(AppArtifactMissing("ios", "iOS app bundle not found at /gone.\nhint")))
    mod._record_lost_app_leg(
        "tests/test_b.py::test_two",
        _excinfo(AppArtifactMissing("ios", "iOS app bundle not found at /gone.")))
    state = mod._baseline_app_legs()["ios"]
    assert state.startswith("lost mid-run: tests/test_a.py::test_one: iOS app bundle not found")
    assert "hint" not in state


def test_a_loss_re_raised_by_a_fixture_still_records(legs_isolated):
    mod = legs_isolated
    _set_up_ios(mod)
    try:
        try:
            raise AppArtifactMissing("macos", "macOS executable not found at /gone.")
        except AppArtifactMissing as inner:
            raise RuntimeError("fixture could not launch") from inner
    except RuntimeError:
        info = pytest.ExceptionInfo.from_current()
    mod._BINARY_BUILD_MEMO["app:macos"] = ("ok", "/tmp/fauna-macos")
    mod._record_lost_app_leg("tests/test_c.py::test_three", info)
    legs = mod._baseline_app_legs()
    assert legs["ios"] == "ok"
    assert legs["macos"].startswith("lost mid-run: tests/test_c.py::test_three")


def test_an_unrelated_failure_records_nothing(legs_isolated):
    mod = legs_isolated
    _set_up_ios(mod)
    mod._record_lost_app_leg("tests/test_d.py::test_four", _excinfo(RuntimeError("boom")))
    mod._record_lost_app_leg("tests/test_d.py::test_five", None)
    assert mod._baseline_app_legs() == {"ios": "ok"}


def test_a_setup_failure_outranks_a_later_loss(legs_isolated):
    mod = legs_isolated
    mod._BINARY_BUILD_MEMO["app:ios"] = ("ok", "/tmp/Fauna.app")
    mod._BINARY_BUILD_MEMO["ios-setup"] = ("failed", "no simulator")
    mod._record_lost_app_leg(
        "tests/test_e.py::test_six", _excinfo(AppArtifactMissing("ios", "gone")))
    assert mod._baseline_app_legs()["ios"].startswith("failed: ios-setup:")

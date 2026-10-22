"""tier_1: `drivers/android.py`'s `preserve_state_across_relaunch()` pin.

Authority: the base contract (`drivers/http_bridge.py::preserve_state_across_relaunch`)
and its consumer, `test_identity_succession_refusal.py` — a succeeded device is
only reproduced when the relaunched app reads back the store the previous
process wrote. android's store is the app's filesDir, which every relaunch
keeps; the one thing that replaces it is the launch's own `seed_credentials`
overwrite (`AppLauncher.launchApp`), so the pin withholds the seed.

The pin answers False when the driver cannot relaunch (no `recover()`
override) so a caller skips honestly instead of asserting a relaunch that never
happened; android's `recover()` relaunches in place, so it answers True.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

_SEED = {"fauna/index": "{}"}


def _launched(cold_relaunch: bool) -> AndroidBridgeDriver:
    driver = AndroidBridgeDriver()
    driver._device_port = 18500
    driver._launch_config = {"environment": {}, "seed_credentials": dict(_SEED)}
    driver.supports_cold_relaunch = lambda: cold_relaunch
    return driver


def test_an_unpinned_launch_seeds_the_credential_file():
    driver = _launched(cold_relaunch=True)
    body = driver._session_body(driver._launch_config)
    assert body["seed_credentials"] == _SEED


def test_the_pin_withholds_the_seed_so_the_relaunch_reads_back_its_own_store():
    driver = _launched(cold_relaunch=True)
    assert driver.preserve_state_across_relaunch() is True
    body = driver._session_body(driver._launch_config)
    assert "seed_credentials" not in body, (
        "a pinned relaunch must not overwrite the credential file the previous "
        "process wrote — that would hand back the fixture's seed, not the device"
    )


def test_reset_unpins_so_the_next_test_seeds_again():
    driver = _launched(cold_relaunch=True)
    driver.preserve_state_across_relaunch()
    driver._clear_relaunch_pin()
    body = driver._session_body(driver._launch_config)
    assert body["seed_credentials"] == _SEED


def test_no_pin_when_the_driver_cannot_relaunch():
    driver = _launched(cold_relaunch=False)
    assert driver.preserve_state_across_relaunch() is False
    assert "preserve_store" not in driver._launch_config


def test_no_pin_before_any_launch():
    driver = AndroidBridgeDriver()
    assert driver.preserve_state_across_relaunch() is False


def test_android_relaunches_so_the_pin_answers_true():
    driver = AndroidBridgeDriver()
    assert driver.supports_cold_relaunch() is True
    driver._launch_config = {"environment": {}}
    assert driver.preserve_state_across_relaunch() is True


def _recovering(config: dict):
    driver = _launched(cold_relaunch=True)
    driver._launch_config = config
    driver._device_port = 18500
    driver._await_health = lambda: True
    calls = []
    driver._delete = lambda path: calls.append(("DELETE", path)) or {}
    driver._post = lambda path, body: calls.append(("POST", path, body)) or {}
    driver._mark_log_baseline = lambda: calls.append(("BASELINE",))
    return driver, calls


def test_recover_cycles_the_app_in_place_and_retakes_the_log_baseline():
    driver, calls = _recovering({"environment": {}, "seed_credentials": dict(_SEED)})
    assert driver.recover() is True
    assert [c[0] for c in calls] == ["DELETE", "BASELINE", "POST"]
    assert calls[2][1] == "/session"


def test_an_unpinned_relaunch_without_a_seed_posts_an_explicit_empty_one():
    # The bridge forwards whatever credential file the previous process left, so
    # no key would bring the app back signed in, unlike every other driver.
    driver, calls = _recovering({"environment": {}})
    assert driver.recover() is True
    assert calls[-1][2]["seed_credentials"] == {}


def test_a_pinned_relaunch_withholds_the_seed():
    driver, calls = _recovering({"environment": {}, "seed_credentials": dict(_SEED)})
    driver.preserve_state_across_relaunch()
    assert driver.recover() is True
    assert "seed_credentials" not in calls[-1][2]


def test_a_drivers_first_launch_is_a_fresh_install_and_its_relaunches_are_not():
    # The bridge clears install-scoped state (the push intent file) only on a
    # fresh install, so a test's own relaunch — pinned or re-seeded, as the
    # sign-out journey re-seeds — reads back the install it left
    # (`test_push_settings.py`, `AppLauncher.launchApp`).
    driver = _launched(cold_relaunch=True)
    assert driver._session_body(driver._launch_config)["fresh_install"] is True
    assert driver._session_body(driver._launch_config)["fresh_install"] is False
    driver.preserve_state_across_relaunch()
    assert driver._session_body(driver._launch_config)["fresh_install"] is False

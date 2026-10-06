"""tier_1: `drivers/android.py`'s `preserve_state_across_relaunch()` pin.

Authority: the base contract (`drivers/http_bridge.py::preserve_state_across_relaunch`)
and its consumer, `test_identity_succession_refusal.py` — a succeeded device is
only reproduced when the relaunched app reads back the store the previous
process wrote. android's store is the app's filesDir, which every relaunch
keeps; the one thing that replaces it is the launch's own `seed_credentials`
overwrite (`AppLauncher.launchApp`), so the pin withholds the seed.

The pin answers False while the driver cannot relaunch at all — android's
`recover()` is still the base health probe (`supports_cold_relaunch()` is
False) — so a caller skips honestly instead of asserting a relaunch that never
happened.
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


def test_no_pin_while_the_driver_cannot_relaunch():
    driver = _launched(cold_relaunch=False)
    assert driver.preserve_state_across_relaunch() is False
    assert "preserve_store" not in driver._launch_config


def test_no_pin_before_any_launch():
    driver = AndroidBridgeDriver()
    assert driver.preserve_state_across_relaunch() is False


def test_android_cannot_relaunch_today():
    # The premise the False answer rests on — when android gains a real
    # recover(), this fails, and the pin above starts answering True.
    assert AndroidBridgeDriver().supports_cold_relaunch() is False

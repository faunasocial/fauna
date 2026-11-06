"""The iOS driver's `real_keychain: "keep"` launch: a kept keychain needs a kept
container, and a missing pin is named rather than read as an app that did not
come back. No simulator: every path below stops before `simctl`.
"""

import pytest

from drivers.ios import IosInProcessDriver

pytestmark = pytest.mark.tier_1


def _driver(config: dict) -> IosInProcessDriver:
    driver = IosInProcessDriver.__new__(IosInProcessDriver)
    driver._launch_config = config
    return driver


def test_a_keep_launch_without_the_pin_is_refused():
    with pytest.raises(RuntimeError, match="preserve_state_across_relaunch"):
        _driver({}).launch({"real_keychain": "keep"})


def test_recover_surfaces_the_missing_pin_instead_of_answering_false():
    """The shared `recover()` swallows every `launch()` failure into `False`."""
    with pytest.raises(RuntimeError, match="preserve_state_across_relaunch"):
        _driver({"real_keychain": "keep"}).recover()


def test_a_pinned_keep_passes_the_check():
    driver = _driver({"real_keychain": True})
    driver._preserve_container = True
    driver.keep_real_keychain_at_next_launch()
    assert driver._launch_config["real_keychain"] == "keep"
    driver._require_keep_pin(driver._launch_config)


def test_the_reset_ends_the_keep(monkeypatch):
    """The reset sweeps the seat, so the driver's next launch sweeps too."""
    monkeypatch.setattr("drivers.inprocess_agent.InProcessAgentDriver.reset",
                        lambda self, timeout=10.0: None)
    driver = _driver({"real_keychain": "keep"})
    driver._preserve_container = True
    driver.reset()
    assert driver._launch_config["real_keychain"] is True
    assert driver._preserve_container is False

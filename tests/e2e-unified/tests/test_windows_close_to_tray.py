"""tier_3 E2E: windows' close-to-tray residency (general-settings 1, windows leg).

Windows has no D-Bus tray-host concept (`docs/goal/architecture/apps/linux.md`
§ System Tray's `StatusNotifierWatcher` gate is linux-only) — the Win32 tray
icon is always present once `TrayIconService.Initialize()` runs, so there is
no host-absent/host-present axis to drive here, unlike
`test_tray_close_to_tray.py`. What windows' mechanism (`apps/windows.md`
§ App Lifecycle → *Window close*) needs witnessed is the cross-app promise
itself (`common.md` § Desktop Residency, outcome 1): closing the window is
never a silent stop — the app hides to the tray and STAYS ALIVE (the process
is the sync agent's bearer-minting service) rather than quitting, the default
is ON, and the choice survives a relaunch.

The quit-path half of this mechanism (close-to-tray OFF -> real quit ->
quit-time draft flush) is already proven end to end by
`test_windows_window_close_flush.py`; this file covers the hide-path half plus
the default/persistence claims that file does not touch.
"""

import time

import pytest

from common.launch_harness import reached_authenticated_app

#: `logged_in_app` reuses ONE app process across every test in this module
#: (`app`'s session-scoped driver cache + `reset()` between tests — see
#: `conftest.py::app`); `reset()` returns the app to factory/onboarding state
#: but does not restore window visibility, so a test that hides the window
#: (SW_HIDE) must explicitly recover a fresh, visible instance before the
#: next test runs, rather than leaving that to the shared teardown.

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

CLOSE_TO_TRAY = "close-to-tray-toggle"
_GENERAL = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}}


def _goto_general(driver):
    driver.set_state(_GENERAL)
    driver.wait_for(CLOSE_TO_TRAY, timeout=10)


@pytest.mark.feature("general-settings")
def test_close_to_tray_defaults_on_for_a_fresh_install(logged_in_app):
    """Shape A's default: a fresh install has close-to-tray already ON —
    nobody has to find the toggle for a window close to stop killing file
    sync (`apps/windows.md` § App Lifecycle → *Window close*; the app is the
    sync agent's bearer-minting service, so a silently-quit window would take
    sync down with it)."""
    driver = logged_in_app.driver
    _goto_general(driver)

    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "close-to-tray must default ON for a fresh install (no "
        "app-settings.json yet)"
    )


@pytest.mark.feature("general-settings")
def test_closing_hides_to_tray_and_keeps_the_app_alive(logged_in_app):
    """The outcome's core claim: with close-to-tray ON (the default), closing
    the window HIDES it rather than quitting — the process (and the resident
    sync engine it hosts) stays alive. Mirrors linux's
    `test_host_present_enables_toggle_and_hides_on_close`, minus the tray-host
    axis windows does not have."""
    driver = logged_in_app.driver
    _goto_general(driver)

    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "precondition: close-to-tray defaults ON"
    )

    driver.window_close()
    # Window hidden -> its elements leave the visible search roots; process alive.
    hidden = False
    for _ in range(20):
        if not driver.is_visible(CLOSE_TO_TRAY):
            hidden = True
            break
        time.sleep(0.1)  # sleep-ok: bounded poll cadence, not a settle guess
    assert driver.is_app_alive(), (
        f"close-to-tray hide must keep the app running; "
        f"error={logged_in_app.error_text()!r}"
    )
    assert hidden, "with close-to-tray ON, closing the window must hide it, not leave it open"

    # Leave a fresh, VISIBLE instance for the next test in this module — `reset()`
    # (the shared per-test teardown) returns app STATE to factory but never
    # un-hides a window this test itself hid. The instance has to come back
    # SIGNED IN (that is what `reached_authenticated_app` waits for, and nothing
    # here re-logs-in), so the store is pinned: a default relaunch drops the
    # session credentials and the app returns to onboarding.
    assert driver.preserve_state_across_relaunch(), (
        "the relaunch below must keep the session — nothing here signs back in"
    )
    assert driver.recover(), "failed to restore a visible instance after the hide"
    reached_authenticated_app(driver, timeout=90)


@pytest.mark.feature("general-settings")
def test_close_to_tray_choice_survives_a_relaunch(logged_in_app):
    """A close-to-tray choice round-trips a real process boundary, both ways
    (mirrors linux's `test_close_to_tray_choice_survives_a_relaunch`). BOTH
    halves are load-bearing: only a persisted `true` surviving the second
    assertion rules out a store that silently never loads and always reads
    the bare-atomic default."""
    driver = logged_in_app.driver
    _goto_general(driver)

    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "precondition: a fresh install defaults close-to-tray ON"
    )
    driver.click(CLOSE_TO_TRAY)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "false"

    assert driver.preserve_state_across_relaunch(), (
        "the durability assertion is vacuous unless the client-local store is pinned"
    )
    assert driver.recover(), "the app did not come back up after a relaunch"
    reached_authenticated_app(driver, timeout=90)
    _goto_general(driver)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "false", (
        "an explicitly-OFF close-to-tray must survive a relaunch — the ON "
        "default must never overwrite a user's explicit choice"
    )

    # ...and back ON must survive too (the non-vacuous half — see the docstring).
    driver.click(CLOSE_TO_TRAY)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true"

    assert driver.recover(), "the app did not come back up after a relaunch"
    reached_authenticated_app(driver, timeout=90)
    _goto_general(driver)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "an explicitly-ON close-to-tray must survive a relaunch too"
    )

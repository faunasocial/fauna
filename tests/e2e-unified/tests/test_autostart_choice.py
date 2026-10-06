"""Start at login is a choice the user keeps — `settings-autostart-toggle`.

`apps/linux.md` § Auto-start at sign-in and `apps/windows.md` § App Lifecycle
ratify it default-ON and tri-state (never chosen / explicitly on / explicitly
off), so the toggle shows the user's CHOICE, never the registration's
existence. Under the e2e bridge all three apps gate the OS registration
itself off (linux by construction; windows `AutoStartGate.cs`; macOS
`AutoStart.applyUserChoice` under `FaunaE2E.isActive`), so this test
witnesses the part a user experiences across a process boundary — the
persisted choice surviving a relaunch — and leaves the registration to each
app's unit tests. The cross-app promise itself (`settings-autostart-toggle`
is one of the two Desktop Residency halves) is owned by `common.md` §
Desktop Residency, not a per-platform lifecycle section.

linux + windows + macOS. web, tui, android and ios have no desktop sign-in to
start at (declared absences).
"""

import pytest

from common.launch_harness import reached_authenticated_app
from helpers.app_surface import declared_absence

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.windows, pytest.mark.macos]

AUTOSTART = "settings-autostart-toggle"
_GENERAL = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}}


def _require_surface(driver) -> None:
    """The platform markers deselect runs with neither desktop app selected;
    a mixed run (`--app sweep`) still parametrizes its other apps here."""
    if driver.is_linux() or driver.is_windows() or driver.is_macos():
        return
    declared_absence(
        driver,
        capability="starting the app at desktop sign-in",
        doc="ui.yaml settings.platform_elements — settings-autostart-toggle is "
        "scoped to linux, windows and macos",
    )


def _goto_general(driver) -> None:
    driver.set_state(_GENERAL)
    driver.wait_for(AUTOSTART, timeout=10)


def _relaunch_to_general(driver) -> None:
    """Force-quit + relaunch keeping the client-local store; the app signs
    itself back in (the real relaunch path — see the close-to-tray tests)."""
    assert driver.preserve_state_across_relaunch(), (
        "the durability assertion is vacuous unless the client-local store is pinned"
    )
    assert driver.recover(), "the app did not come back up after a relaunch"
    reached_authenticated_app(driver, timeout=90)
    _goto_general(driver)


@pytest.mark.feature("general-settings")
def test_start_at_login_is_a_choice_that_survives_a_relaunch(logged_in_app):
    """Default ON, an explicit OFF survives a relaunch, and turning it back ON
    survives too.

    Both halves are load-bearing: OFF surviving proves a choice is persisted
    at all (the ON default would otherwise win on relaunch); ON surviving
    after an OFF proves the second write landed too, rather than a store stuck
    on its first value.
    """
    driver = logged_in_app.driver
    _require_surface(driver)
    _goto_general(driver)

    # `"checked"` (`drivers/base.py::get_attr` — any checkbox/toggle, "true"/
    # "false" cross-app), not `"state"` (a different per-element vocabulary,
    # e.g. `recipient-resolve-status`'s "idle"/"resolving"/… — apple's toggle
    # publishes no `"state"` at all, only its registered "on"/"off" `value`,
    # which `"checked"` maps to true/false for every driver, apple included).
    assert driver.get_attr(AUTOSTART, "checked") == "true", (
        "precondition: a user who never chose sees start-at-login ON"
    )
    driver.click(AUTOSTART)
    assert driver.get_attr(AUTOSTART, "checked") == "false"

    _relaunch_to_general(driver)
    assert driver.get_attr(AUTOSTART, "checked") == "false", (
        "an explicit OFF must survive a relaunch — the ON default must never "
        "overwrite a user's choice"
    )

    driver.click(AUTOSTART)
    assert driver.get_attr(AUTOSTART, "checked") == "true"

    _relaunch_to_general(driver)
    assert driver.get_attr(AUTOSTART, "checked") == "true", (
        "turning start-at-login back ON must survive a relaunch too"
    )

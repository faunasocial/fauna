"""Removing the device in your hand is refused on the Devices page — nothing is
deleted, and the page says why.

Owner: ``docs/goal/ui/devices.md`` § Errors & edge cases (*a refused removal is
one of those, and deletes nothing*) and ``docs/goal/behavior/devices.md``
§ Removing a Device (the rule). The refusal is shared Rust
(``DevicesMachine::remove_device`` → ``fauna_core::fleet_removal``), so every app
that wires the fleet-removal port behaves alike; this journey drives it through
the app UI (convention 8).

**What this pins.** Before the fleet-removal rule, ``device-remove-button`` on
this machine's own row deleted the row on the nest and wrote nothing else — the
app kept running on a device the account no longer listed. The rule refuses it
BEFORE any nest call: the row still lists, the roster is unchanged, and
``error-message`` carries ``devices.error_remove_own_device`` (sign out on this
device instead).

**The other two refusals.** A row whose principal is no verified fleet member
(``devices.error_remove_unverified_device``) is driven in
``test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`` —
it plants a granted row whose key never joined the fleet
(``helpers.enrollment.register_granted_device``) on the seat whose generation
tip has resolved, the same row the relay-only marker is witnessed on. **Bound:**
a row its member's own record disagrees with
(``devices.error_remove_row_mismatch``) is not driven by any journey — it needs
a verified fleet member whose own device-endpoints record names a different
row, which only a second enrolled seat of the same account can publish, and
the harness has no such seat. That arm is pinned in Rust
(``fauna_core::fleet_removal``'s
``a_claimed_member_stating_a_different_row_is_a_mismatch``, and
``fauna-devices-machine``'s
``a_refused_resolution_deletes_nothing_and_says_the_device_was_not_removed``).

⚠ **A DEDICATED actor, never the shared ``test_user``** — which row is "this
device" on the shared actor depends on what earlier tests registered.
``test_user`` is still requested: its fixture lifts the device cap on the tier
every test user shares.

⚠ **Every read is a deadline poll on state, never a settle-sleep** (convention
14): the enrollment latch naming a listed row, the account runtime assembled
(``await_device_removal_ready`` — otherwise the click meets the
runtime-not-running refusal, a DIFFERENT error this test would misread), then a
poll on ``error-message`` re-entering the page on tui's nav-edge hydrate.
"""
from __future__ import annotations

import time

import pytest

from helpers import enrollment
from helpers.waiting import await_device_removal_ready, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The this-device marker landing on the enrolled row after the latch — a
#: nav-edge hydrate reading the roster plus the runtime's row read. Generous by
#: design; a green run pays only the real latency.
MARKED_VISIBLE_S = 120.0

#: The refusal is local (the runtime resolves the row against its own replica,
#: no nest call) — one gesture plus one repaint. Generous by design.
REFUSAL_VISIBLE_S = 30.0

#: How often the poll re-enters the Devices page so tui's nav-edge hydrate
#: re-reads the roster — a re-hydrate cadence, not a settle.
REHYDRATE_EVERY_S = 2.0


def _this_device_card_index(driver) -> int | None:
    """The index of the ``device-card`` carrying ``device-this-mark-badge`` —
    the enrolled row — or ``None`` while no card carries it yet
    (``test_device_online.py``'s reader)."""
    for i in range(driver.count("device-card")):
        if driver.is_visible_scrolled("device-this-mark-badge", scope=f"device-card[{i}]"):
            return i
    return None


def _error_text(driver) -> str:
    if not driver.is_visible("error-message"):
        return ""
    return driver.get_text("error-message") or ""


@pytest.mark.feature("devices")
def test_removing_the_device_in_hand_is_refused_and_deletes_nothing(
    app, request, nest_instance, test_user
):
    """Sign in on a fresh actor, wait for the enrollment to latch, press remove
    on the card marked as this device: the page names the refusal, the card is
    still there, and the nest still lists the row."""
    from conftest import _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)

    enrollment.sign_in(app, request, nest_instance, user)
    _writer, latched_row, roster = enrollment.await_enrollment(app, nest_url, user)
    assert latched_row in roster, (latched_row, roster)

    driver = app.driver
    app.backups.navigate_devices()
    last_nav = time.monotonic()
    found: dict = {}

    def this_device_marked():
        nonlocal last_nav
        if time.monotonic() - last_nav >= REHYDRATE_EVERY_S:
            app.backups.navigate_devices()
            last_nav = time.monotonic()
        found["index"] = _this_device_card_index(driver)
        found["cards"] = driver.count("device-card")
        return found["index"] is not None

    wait_until(
        this_device_marked,
        MARKED_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            f"no device-card ever carried device-this-mark-badge (last read: {found!r}; "
            f"enrollment latched on row {latched_row}, roster {sorted(roster)!r})"
        ),
    )
    index = found["index"]
    cards_before = driver.count("device-card")

    # The gesture under test (convention 8). The barrier first: a click before
    # the account runtime assembled reads the runtime-not-running refusal.
    await_device_removal_ready(driver)
    driver.click("device-remove-button", index=index)

    last: dict = {}

    def refused():
        last["error"] = _error_text(driver)
        return last["error"] == S.devices.error_remove_own_device

    wait_until(
        refused,
        REFUSAL_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            "removing this machine's own row never painted the own-device refusal "
            f"(last error-message: {last.get('error')!r}). An empty error with the row "
            "gone from the roster = the fleet-removal port is not wired on this app; "
            "another error = the runtime refused for a different reason (grep the app "
            "log for 'fauna_devices')."
        ),
    )

    # Nothing was deleted: the nest still lists the row, and the page still
    # paints it, marked as this device.
    assert latched_row in enrollment.roster(nest_url, user), (
        "a refused removal must not delete the row on the nest"
    )
    app.backups.navigate_devices()
    assert driver.count("device-card") == cards_before, driver.diagnose("device-card")
    assert _this_device_card_index(driver) is not None, (
        "the device in hand must still be listed and marked after the refusal: "
        f"{driver.diagnose('device-this-mark-badge')}"
    )

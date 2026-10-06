"""The per-device p2p participation control — wormability rule 5's off switch,
driven through the app UI (tui, the lead app).

Owner: ``docs/goal/behavior/p2p.md`` § Per-device participation (ratified
2026-09-25, user-directed): whether THIS device runs its peer listeners is the
device's own choice, ``device-p2p-participation-toggle`` on its own
``device-card`` row (the row ``device-this-mark-badge`` marks), device-local
authority, default on. Off means no listener of either kind — which the
tier_1 tests pin on the socket (``fauna-sync-engine/tests/p2p_participation_gate.rs``,
``offline_share::tests``, ``fauna-iroh/tests/participation_socket.rs``); what
THIS journey proves is the app glue above them: the toggle flips the
device-local row through the shared machine's door, the page re-reads it on
its next hydrate, and the share plane's own reading on the Folders page
(``share-serve-status``) says off — the driver unbound its seat on the pass
the toggle woke — and comes back once the switch is on again.

⚠ Every wait is a deadline poll on state (convention 14): the toggle's
``checked`` mirror after the gesture's re-snapshot, the Folders page's status
reading after the driver's pass. The share plane's cadence never gates a
green run: the toggle wakes the driver at once.
"""
from __future__ import annotations

import pytest

from helpers.app_surface import app_name, declared_absence, skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # macOS' production agent spawner is launchd, which an e2e launch must not
    # bootstrap; this marker makes its launch construct the private child-agent
    # spawner, and the share plane's provisioner is built from it — no agent,
    # no plane, no `share-serve-status` (the `test_share_pump_two_actor.py`
    # precedent). A no-op for tui/linux, which direct-spawn their agent.
    pytest.mark.real_sync_agent,
    # The share plane waits for the REAL conversations session
    # (`fauna-ffi/src/share_plane.rs` — its durable sink rides it), and a
    # native app (macOS) launches its deterministic mock backend unless this
    # marker sets the launch gate. Inert on tui, which runs the real session.
    pytest.mark.real_conversations,
]

#: The gesture's own re-snapshot paints the new state on tui; the driver's
#: pass behind it re-reads the row. Generous by design.
_TOGGLE_VISIBLE_S = 30.0
_PLANE_READING_S = 60.0


def _require_participation_toggle(driver) -> None:
    """The own-row journeys: tui leads; macOS, windows and iOS followed;
    linux and android (2026-10-06) draw the machine's published paint
    (``p2p.md`` § Implementation status today). iOS renders the shared
    FaunaKit card and hosts the account runtime, so its own row's gesture
    writes the device-local row as macOS' does. Web renders the toggle too,
    but only the request-off arm: the tab runs no listener, so it has no own
    row to flip."""
    app = app_name(driver)
    if app in ("tui", "macos", "windows", "ios", "linux", "android"):
        return
    if app == "web":
        declared_absence(
            driver,
            capability="own-row device-p2p-participation-toggle (the tab runs no "
                       "peer listener, so every row it paints is a sibling's "
                       "request-off arm)",
            doc="p2p.md § Per-device participation → Web",
        )
    skip_unbuilt(
        driver,
        surface="device-p2p-participation-toggle on the devices page",
        detail="tui, macOS, windows, iOS, linux and android render it",
        tracked="p2p.md § Implementation status today",
    )


def _require_share_plane(driver) -> None:
    """The plane-reading half needs the app to host the share plane and paint
    its ``share-serve-status``. windows and iOS render the toggle, and their
    own row flips the device-local row through the door (the toggle-only
    journey below runs there), but neither wires ``start_share_plane``, so
    there is no reading to wait for. android likewise renders the toggle but
    starts no share plane, so it paints no ``share-serve-status``; linux runs
    the plane (``share_glue.rs``) and runs the whole journey."""
    app = app_name(driver)
    if app == "android":
        skip_unbuilt(
            driver,
            surface="share-serve-status (the share plane) on the folders page",
            detail="android renders device-p2p-participation-toggle (2026-10-06) but "
                   "never starts the share plane, so no share-serve-status paints",
            tracked="p2p.md § Implementation status today",
        )
    if app == "windows":
        skip_unbuilt(
            driver,
            surface="share-serve-status (the share plane) on the folders page",
            detail="windows renders device-p2p-participation-toggle (2026-09-26) but "
                   "never wires start_share_plane, so no share-serve-status paints",
            tracked="p2p.md § Implementation status today",
        )
    if app == "ios":
        skip_unbuilt(
            driver,
            surface="share-serve-status (the share plane) on the folders page",
            detail="iOS hosts the account runtime and its own row writes the "
                   "device-local row, but its phone-peer leg (the share plane) is "
                   "unbuilt, so no share-serve-status paints",
            tracked="p2p-shared-set-build.md § Cross-user shared-set transfer → Phone peers — design",
        )


def _own_card(driver) -> str:
    """The scope of this device's own ``device-card`` — the row the
    this-device marker sits on."""
    driver.wait_for("device-card", timeout=30.0)
    for i in range(driver.count("device-card")):
        scope = f"device-card[{i}]"
        if not driver.is_absent("device-this-mark-badge", scope=scope):
            return scope
    raise AssertionError(
        "no device-card carries device-this-mark-badge: this seat has not enrolled "
        f"yet ({driver.diagnose('device-card')})"
    )


def _own_toggle_checked(driver) -> bool | None:
    scope = _own_card(driver)
    value = driver.get_attr("device-p2p-participation-toggle", "checked", scope=scope)
    if value is None:
        return None
    return str(value).lower() == "true"


def _await_own_toggle(driver, app, checked: bool, why: str) -> None:
    """Re-hydrate the page and poll the own row's toggle until it reads
    ``checked`` — the gesture's re-snapshot on tui, and on any app a fresh
    nav-edge read of the door."""
    last: dict = {}

    def probe():
        last["value"] = _own_toggle_checked(driver)
        return last["value"] is checked

    wait_until(
        probe,
        _TOGGLE_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            f"{why}: device-p2p-participation-toggle on this device's own row never "
            f"read checked={checked} (last {last.get('value')!r}); "
            f"{driver.diagnose('device-p2p-participation-toggle')}"
        ),
    )


def _serve_status(driver) -> str | None:
    if not driver.is_visible("share-serve-status"):
        return None
    return driver.get_text("share-serve-status")


def _await_plane_reading(driver, app, off: bool, why: str) -> None:
    """The Folders page's ``share-serve-status`` — the share driver's own
    reading, painted from its cell. ``off`` waits for the participation-off
    label; ``not off`` waits for any OTHER reading (no sets, the brake, or
    serving), since a fresh account has no shared set to serve."""
    off_label = S.folders.share_serve_status_participation_off
    last: dict = {}

    def probe():
        app.backups.navigate_folders()
        last["status"] = _serve_status(driver)
        status = last["status"]
        if status is None:
            return False
        return (status == off_label) if off else (status != off_label)

    wait_until(
        probe,
        _PLANE_READING_S,
        interval=1.0,
        diagnose=lambda: (
            f"{why}: share-serve-status never read {'the participation-off label' if off else 'a non-off reading'} "
            f"(last {last.get('status')!r}; expected off label {off_label!r}). A None "
            f"reading means the share plane never started this session (no agent share "
            f"access at AccountStoreReady) — {driver.diagnose('share-serve-status')}"
        ),
    )


def _open_own_card(app, request, nest_instance) -> str:
    """A DEDICATED actor, signed in through the enrollment helper and awaited
    on its own roster row (the `test_device_member_removal.py` shape): the own
    row exists only once this launch's enrollment has latched, and the device
    row the toggle sits on is that row. Returns that row's scope, with the
    toggle asserted at its default (on) and carrying the own-switch label."""
    from conftest import _make_user

    from helpers import enrollment

    driver = app.driver
    url = nest_instance["url"]
    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, url, user)

    app.backups.navigate_devices()
    scope = _own_card(driver)
    assert _own_toggle_checked(driver) is True, (
        "the default is on — every device has had a listener since the planes shipped; "
        f"{driver.diagnose('device-p2p-participation-toggle')}"
    )
    assert driver.get_text("device-p2p-participation-toggle", scope=scope) == (
        S.devices.p2p_participation_own
    ), "this device's own row carries the own-switch label"
    return scope


@pytest.mark.feature("device-to-device-file-transfer")
def test_this_devices_peer_transfer_switch_flips_and_rests(
    app, request, nest_instance, test_user
):
    """The toggle half alone — every app that renders the toggle, including
    the ones that host no share plane yet (windows, iOS): the own row's toggle
    unchecks, stays unchecked across a fresh hydrate (the device-local row
    behind the door, not a paint), and checks again. `test_user` is requested
    for its device-cap lift on the shared tier."""
    driver = app.driver
    _require_participation_toggle(driver)
    scope = _open_own_card(app, request, nest_instance)

    driver.click("device-p2p-participation-toggle", scope=scope)
    _await_own_toggle(driver, app, False, "after turning peer transfers off")
    app.backups.navigate_folders()
    app.backups.navigate_devices()
    _await_own_toggle(driver, app, False, "after leaving and re-opening the devices page")

    scope = _own_card(driver)
    driver.click("device-p2p-participation-toggle", scope=scope)
    _await_own_toggle(driver, app, True, "after turning peer transfers back on")
    app.backups.navigate_folders()
    app.backups.navigate_devices()
    _await_own_toggle(driver, app, True, "after re-opening the devices page with them on")


def test_turning_this_devices_peer_transfers_off_and_on_again(
    app, request, nest_instance, test_user
):
    """Off: the own row's toggle unchecks, stays unchecked across a fresh
    hydrate (the device-local row, not a paint), and the share plane's own
    status says off. On again: both revert. `test_user` is requested for its
    device-cap lift on the shared tier."""
    driver = app.driver
    _require_participation_toggle(driver)
    _require_share_plane(driver)
    scope = _open_own_card(app, request, nest_instance)

    # --- off
    driver.click("device-p2p-participation-toggle", scope=scope)
    _await_own_toggle(driver, app, False, "after turning peer transfers off")
    # A fresh hydrate reads the door again: the row rests, this is not a paint.
    app.backups.navigate_folders()
    app.backups.navigate_devices()
    _await_own_toggle(driver, app, False, "after leaving and re-opening the devices page")
    _await_plane_reading(driver, app, True, "with peer transfers off")

    # --- on again
    app.backups.navigate_devices()
    scope = _own_card(driver)
    driver.click("device-p2p-participation-toggle", scope=scope)
    _await_own_toggle(driver, app, True, "after turning peer transfers back on")
    _await_plane_reading(driver, app, False, "with peer transfers on again")

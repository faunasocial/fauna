"""The signed-in app's own device reads **Online** on the Devices page.

Owner: ``docs/goal/behavior/devices.md`` § Listing Devices → *The binding*
(ratified 2026-09-22) and § Implementation status today.

**What went wrong, measured.** ``fauna.sync.devices.list``'s ``online`` read the
legacy data-plane socket registry alone, and the only client that registered
there was the legacy headless ``fauna-sync`` daemon. Every app seat and every
per-user sync agent takes the WS-RPC push nudge instead and holds no data-plane
socket, so the Devices page painted **Offline** for every device a user actually
owns, however connected it was — this machine's own row included.

**The binding.** A WS-RPC connection is bound to a device when the bearer it
was upgraded with was minted over ``fauna.auth.device_handshake`` by the device
key the row carries as its granted ``principal``. The app's own primary bearer
is seed-minted and binds nothing; what makes THIS machine's row online is the
account runtime's principal client — its own connection, upgraded with a bearer
the machine's store writer key minted, on the row the enrollment latched on.
So the witness is the row carrying ``device-this-mark-badge`` (the enrolled row,
``devices.md`` § This-device marker: *enrolled wins*) reading Online.

⚠ **A DEDICATED actor, never the shared ``test_user``** — the shared actor's
roster is the whole session's, and which of its rows is "this device" depends
on what earlier tests registered. ``test_user`` is still requested: its fixture
lifts the device cap on the tier every test user shares.

⚠ **Every read sits behind a causal barrier, never a settle-sleep**
(convention 14): the enrollment latch naming a listed row
(``helpers.enrollment.await_enrollment``), then a deadline poll on the page —
re-entering it on tui's nav-edge hydrate cadence — for the state the principal
client's connect produces. That connect deliberately races the enrollment
ceremony (``fauna_client::ws_device_handshake_bearer::spawn_connect_retry``,
backoff to a 30 s ceiling), which is why the poll has a budget of its own
rather than asserting on the first paint.
"""
from __future__ import annotations

import time

import pytest

from helpers import enrollment
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The principal client's connect after the enrollment latched (its retry
#: backoff reaches 30 s at the ceiling), plus a nav-edge hydrate reading the
#: roster. Generous by design; a green run pays only the real latency.
ONLINE_VISIBLE_S = 180.0

#: How often the poll re-enters the Devices page so tui's nav-edge hydrate
#: re-reads ``fauna.sync.devices.list`` — a re-hydrate cadence, not a settle.
REHYDRATE_EVERY_S = 2.0

#: The label of the fixture row that is registered but never connected.
UNCONNECTED_LABEL = "never-connected-device"


def _this_device_card_index(driver) -> int | None:
    """The index of the ``device-card`` carrying ``device-this-mark-badge`` —
    the enrolled row — or ``None`` while no card carries it yet."""
    for i in range(driver.count("device-card")):
        if driver.is_visible_scrolled("device-this-mark-badge", scope=f"device-card[{i}]"):
            return i
    return None


@pytest.mark.feature("devices")
def test_the_signed_in_apps_own_device_reads_online(app, request, nest_instance, test_user):
    """Sign in on a fresh actor, wait for this launch's enrollment to latch on a
    listed row, open Settings → Devices: the card marked as this device reads
    Online — the assertion that was false on every app until 2026-09-22."""
    from conftest import _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    # A fixture row nothing is connected as (no grant, no socket): the control
    # that pins the verdict as PER DEVICE, not "the actor has a connection".
    enrollment.register_device(nest_url, user, UNCONNECTED_LABEL)

    enrollment.sign_in(app, request, nest_instance, user)
    _writer, latched_row, roster = enrollment.await_enrollment(app, nest_url, user)
    assert latched_row in roster, (latched_row, roster)

    driver = app.driver
    app.backups.navigate_devices()
    last_nav = time.monotonic()
    last: dict = {}

    def this_device_reads_online():
        nonlocal last_nav
        if time.monotonic() - last_nav >= REHYDRATE_EVERY_S:
            app.backups.navigate_devices()
            last_nav = time.monotonic()
        index = _this_device_card_index(driver)
        last["cards"] = driver.count("device-card")
        last["this_index"] = index
        if index is None:
            return False
        status = app.backups.device_status(index)
        last["status"] = status
        return status == S.devices.online

    wait_until(
        this_device_reads_online,
        ONLINE_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            "the enrolled row never read Online (last read: "
            f"{last!r}; enrollment latched on row {latched_row}, roster {sorted(roster)!r}). "
            "No this-device card = the marker never landed on the enrolled row; a card "
            "reading Offline = the account runtime's principal client never connected "
            "(grep the app log for 'device-principal client'), or the nest's "
            "`fauna.sync.devices.list` is not joining the connection's bound device key."
        ),
    )
    # The control: the fixture row no connection is bound to reads Offline on
    # the same paint. (Found by its label — every app this suite drives decodes
    # sealed device labels, `test_device_cards.py`'s by-label rule.)
    control = next(
        (
            i
            for i in range(driver.count("device-card"))
            if app.backups.device_name(i) == UNCONNECTED_LABEL
        ),
        None,
    )
    assert control is not None, (
        f"the unconnected fixture row never painted: {driver.diagnose('device-name')}"
    )
    assert app.backups.device_status(control) == S.devices.offline, (
        "a row no connection is bound to must read Offline while this machine's row "
        f"reads Online: {driver.diagnose('device-status', scope=f'device-card[{control}]')}"
    )

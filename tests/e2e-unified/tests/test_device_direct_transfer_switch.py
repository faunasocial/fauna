"""One device's direct-transfer switch, seen from a second device — the
cross-device half of ``p2p.md`` § Per-device participation.

``test_p2p_participation.py`` pins the own-row half on one seat (the toggle
flips, rests across a fresh hydrate). What it cannot show is the half that spans
two real devices of one account: that a sibling can only ASK a device to stop
(never switch it on), that a request made while the device was away lands when
it comes back, that the sibling's card then shows the device's own last word,
that the device's syncing is untouched meanwhile (its files still arrive through
the nest, with no byte from a sibling and no sibling leg), and that the setting
rests across a restart of the app.

Owner: ``docs/goal/behavior/p2p.md`` § Per-device participation (the owner arm
is a brake only; the device folds a pending brake at its first full pump pass
back and acknowledges it with a self-arm report of ``off``) and
``account-sync-plane.md`` § The peer leg (requirement 7).

Two tui seats on one machine, no operator (convention 16). The gestures are the
user's: the Devices page's ``device-p2p-participation-toggle`` on a sibling's
row (a request) and on the device's own row (convention 8). Every wait is a
deadline poll on state (convention 14); the brake's landing waits on the
device's first full pass back, which the pump's backstop bounds at five
minutes, so the window covers one full backstop.
"""

import contextlib
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from helpers import sync_seats
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_device_to_device_file_transfer import (
    BODY_BYTES,
    SIBLING_WINDOW,
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _await_bytes,
    _await_siblings,
    _reading,
    _seat_factory,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
]

# The tui pair only: this journey drives the Devices page through the seat's
# own action layer, and tui is the lead app.
_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(sys.platform, (("tui", "tui"),))
]

#: A card's paint settles on the gesture's own re-snapshot; the nest's roster
#: carries a sibling's report on the next hydrate.
_CARD_WINDOW = 120.0
#: The brake lands on the device's first full pass back: reconnect wake or the
#: five-minute backstop.
_BRAKE_WINDOW = 330.0


def _card_scope(driver, *, own: bool) -> str:
    """The scope of this seat's own ``device-card`` (the row the this-device
    marker sits on), or of the one other device's card."""
    driver.wait_for("device-card", timeout=30.0)
    count = driver.count("device-card")
    own_name = None
    for i in range(count):
        scope = f"device-card[{i}]"
        if not driver.is_absent("device-this-mark-badge", scope=scope):
            own_name = driver.get_text("device-name", scope=scope)
            if own:
                return scope
    # The roster also lists the harness's own enrolment (an offline row of
    # another name); the other SEAT is the non-own card named like this one.
    for i in range(count):
        scope = f"device-card[{i}]"
        if (
            driver.is_absent("device-this-mark-badge", scope=scope)
            and driver.get_text("device-name", scope=scope) == own_name
        ):
            return scope
    raise AssertionError(
        f"no {'own' if own else 'sibling'} device-card on the page "
        f"({driver.diagnose('device-card')})"
    )


def _roster(seat) -> list:
    """Every card on the seat's Devices page as ``(name, status, this?, checked,
    label)`` — the self-diagnosis a missing brake needs (convention 6)."""
    seat.app.backups.navigate_devices()
    driver = seat.app.driver
    driver.wait_for("device-card", timeout=30.0)
    rows = []
    for i in range(driver.count("device-card")):
        scope = f"device-card[{i}]"
        rows.append((
            driver.get_text("device-name", scope=scope),
            driver.get_text("device-status", scope=scope),
            not driver.is_absent("device-this-mark-badge", scope=scope),
            driver.get_attr("device-p2p-participation-toggle", "checked", scope=scope),
            driver.get_text("device-p2p-participation-toggle", scope=scope),
        ))
    return rows


def _toggle(seat, *, own: bool) -> tuple[bool | None, str]:
    """``(checked, label)`` of the toggle on the own / sibling card, after a
    fresh read of the Devices page."""
    seat.app.backups.navigate_devices()
    driver = seat.app.driver
    scope = _card_scope(driver, own=own)
    value = driver.get_attr("device-p2p-participation-toggle", "checked", scope=scope)
    label = driver.get_text("device-p2p-participation-toggle", scope=scope)
    checked = None if value is None else str(value).lower() == "true"
    return checked, label


def _await_toggle(seat, *, own: bool, checked: bool, label: str | None, window: float, why: str):
    last: dict = {}

    def probe() -> bool:
        last["now"] = _toggle(seat, own=own)
        got_checked, got_label = last["now"]
        return got_checked is checked and (label is None or got_label == label)

    wait_until(
        probe, window, interval=2.0,
        diagnose=lambda: (
            f"{why}: seat {seat.name}'s {'own' if own else 'sibling'} card toggle never "
            f"read checked={checked} label={label!r} (last {last.get('now')!r}); "
            f"roster (name, status, this?, checked, label): {_roster(seat)!r}"
        ),
    )


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + 3 * SIBLING_WINDOW + 3 * _BRAKE_WINDOW + 3 * WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched(
    nest_instance, request, seat_pair
):
    """Seat ``a`` asks seat ``b`` — away at the time — to stop taking part in
    direct transfers: the request cannot switch anything on, lands when ``b``
    comes back, shows on ``a``'s card for ``b`` as ``b``'s own last word, leaves
    ``b`` syncing through the nest with no sibling leg, and rests across another
    restart of ``b``."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"switch-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-after-off.bin"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            # The default is on, and the direct path exists: this is a switch
            # being turned off, not a path that never was.
            _await_siblings(b, a, "before any request")
            _await_toggle(
                a, own=False, checked=True, label=S.devices.p2p_participation,
                window=_CARD_WINDOW,
                why="b's own report of 'on' (outcome 14, the on reading)",
            )

            # b goes away; a asks it to stop while it is away (outcome 13).
            b.stop_keeping_state()
            a.app.backups.navigate_devices()
            driver_a = a.app.driver
            driver_a.click(
                "device-p2p-participation-toggle", scope=_card_scope(driver_a, own=False)
            )
            # The request is pending: b's last word is still 'on', so the box
            # stays checked and only the label says the request is in.
            _await_toggle(
                a, own=False, checked=True, label=S.devices.p2p_participation_off_requested,
                window=_CARD_WINDOW, why="a's request to turn b's direct transfers off",
            )

            # A request is all a sibling can make (outcome 12): the pending row
            # is not actionable any more (the app disables it, and the bridge
            # refuses to drive a disabled control, convention 11), so it can
            # never become a switch-on and the request stands.
            with pytest.raises(RuntimeError, match="disabled"):
                driver_a.click(
                    "device-p2p-participation-toggle", scope=_card_scope(driver_a, own=False)
                )
            checked_after, label_after = _toggle(a, own=False)
            assert (checked_after, label_after) == (
                True, S.devices.p2p_participation_off_requested
            ), (
                f"a second click on a pending sibling row changed it to "
                f"({checked_after!r}, {label_after!r}): a remote switch-on must be "
                f"refused and the request must stand"
            )

            b.restart(STARTUP_WINDOW)
            _await_toggle(
                b, own=True, checked=False, label=S.devices.p2p_participation_own,
                window=_BRAKE_WINDOW,
                why="the request made while b was away, on b's first pass back",
            )
            # b holds no sibling leg and opens nothing for a to reach (outcome 11).
            wait_until(
                lambda: _reading(b).get("siblings", 1) == 0,
                WINDOW, interval=2.0,
                diagnose=lambda: f"b still holds a sibling leg with direct transfers off: {b.peer_files()!r}",
            )
            # a's card shows b's own last word: off, no longer pending (outcome 14).
            _await_toggle(
                a, own=False, checked=False, label=S.devices.p2p_participation,
                window=_CARD_WINDOW, why="b's own report of 'off' on a's card",
            )

            # Syncing untouched (outcome 10): a file written on a reaches b
            # byte for byte through the nest, no chunk from a sibling.
            before = _reading(b)
            (a.path / name).write_bytes(body)
            _await_bytes(b, name, body, "the nest-mediated transfer with direct transfers off", a)
            after = _reading(b)
            assert after.get("chunks", 0) == before.get("chunks", 0), (
                f"b's agent counts chunks served by a sibling with direct transfers off: "
                f"{before!r} -> {after!r}"
            )
            assert after.get("siblings", 0) == 0, f"b holds a sibling leg again: {after!r}"

            # The setting rests across a restart of the app (outcome 15).
            b.stop_keeping_state()
            b.restart(STARTUP_WINDOW)
            _await_toggle(
                b, own=True, checked=False, label=S.devices.p2p_participation_own,
                window=_CARD_WINDOW, why="after another restart of b",
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

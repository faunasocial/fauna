"""Signing out of a device keeps its one row on a sibling's Devices page, and
signing back in brings it back to that row — never a second one.

Owner: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Credential
model — the RULED 2026-09-20 named-row block: a sign-out retires the machine's
grant nest-side and keeps its named row ("this machine, signed out, whose label
and history the returning user expects"), and the next sign-in from the same
install re-adopts that row under a fresh writer key.

``test_sign_out_device_roster.py`` pins the same cycle from the nest's roster
with one app; this is the user's view of it (convention 8): two seats of one
account, no operator (convention 16), seat `a`'s Devices page read while seat
`b` signs out through its own Account page and signs back in. Every wait is a
deadline poll on state (convention 14): the nest's grant cleared after the
sign-out, the enrollment latch after the sign-in, and `a`'s page re-read until
it agrees with the nest's roster.
"""

import contextlib
import secrets
import shutil
import tempfile
from pathlib import Path

import pytest

from helpers import enrollment, sync_seats
from helpers.waiting import wait_until
from tests.test_device_names import journey_seat_sets
from tests.test_device_to_device_file_transfer import (
    STARTUP_WINDOW,
    _account,
    _seat_factory,
)

pytestmark = [pytest.mark.tier_3]

#: Seat `a` reads its Devices page, seat `b` signs out and back in: two tui
#: seats, two of this platform's native app, and `a` played by the web app or
#: the phone (`test_device_names.journey_seat_sets`).
_PAIRS = journey_seat_sets(("tui", "tui"), reader=0)
#: An app and its sync helper on each of two computers: a desktop-only fact, so
#: the tui and native pairs alone — a browser and a phone run no sync helper
#: (`sync-agent-credentials.md` § Credential model, decision 5: an agent-less
#: enrollment is one row by construction).
_COMPUTER_PAIRS = journey_seat_sets(("tui", "tui"), reader=None)

#: The harness's own enrolment — an offline row every roster also lists.
_HARNESS_ROW = "e2e-harness"
#: A roster change reaches `a`'s page on its next visit's hydrate.
_ROSTER_WINDOW = 120.0


def _sibling_cards(driver) -> list:
    """The names of every card that is neither this device's own (the
    this-device marker) nor the harness's enrolment."""
    driver.wait_for("device-card", timeout=30.0)
    names = []
    for i in range(driver.count("device-card")):
        scope = f"device-card[{i}]"
        if not driver.is_absent("device-this-mark-badge", scope=scope):
            continue
        name = driver.get_text("device-name", scope=scope)
        if name != _HARNESS_ROW:
            names.append(name)
    return names


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + 4 * _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.ios
@pytest.mark.android
def test_a_sign_out_and_back_in_keeps_one_row_on_a_siblings_devices_page(
    nest_instance, request, seat_pair
):
    """`b` signs out from its Account page: the nest clears `b`'s grant and
    keeps its row, and `a`'s Devices page still lists `b` as one row. `b` signs
    back in: its enrollment latches on that same row, and `a`'s page lists one
    row for it — not two."""
    from tests.test_filesync_seats import harness_sign_in

    account = _account(nest_instance)
    port, url, secret_key, actor_id, handle = account
    user = {"actor_id_hex": actor_id, "signing_key": bytes.fromhex(secret_key)}
    folder = f"signout-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            da = a.app.driver

            _w, b_row, _ = enrollment.await_enrollment(b.app, url, user)
            last: dict = {}

            def a_lists(count: int):
                def check() -> bool:
                    a.app.backups.navigate_devices()
                    last["names"] = _sibling_cards(da)
                    last["roster"] = enrollment.principals(url, user)
                    return len(last["names"]) == count and b_row in last["roster"]
                return check

            def diagnose(what: str):
                return lambda: (
                    f"{what}: seat a's Devices page lists siblings {last.get('names')!r}; "
                    f"nest roster (device id → principal): {last.get('roster')!r}; "
                    f"seat b's row {b_row}\n{b.diagnostics()}"
                )

            wait_until(a_lists(1), _ROSTER_WINDOW, interval=2.0,
                       diagnose=diagnose("before the sign-out"))

            b.app.settings.sign_out()

            def b_grant_cleared() -> bool:
                last["roster"] = enrollment.principals(url, user)
                return b_row in last["roster"] and last["roster"][b_row] is None

            wait_until(
                b_grant_cleared, enrollment.RETIREMENT_VISIBLE_S, interval=1.0,
                diagnose=lambda: (
                    f"after seat b's sign-out the nest roster reads {last.get('roster')!r}: "
                    f"seat b's row {b_row} should stay, its grant cleared — a missing row "
                    f"is a sign-out that deleted the user's device"
                ),
            )
            wait_until(a_lists(1), _ROSTER_WINDOW, interval=2.0,
                       diagnose=diagnose("after the sign-out (the row stays)"))
            assert len(last["roster"]) == 3, (
                f"after seat b's sign-out the roster should be a, b and the harness "
                f"row, and reads {last['roster']!r}"
            )

            harness_sign_in(url, secret_key, actor_id, handle)(b.app, "b")
            enrollment.await_account_runtime_assembled(b.app.driver)
            _w2, b_row_again, _ = enrollment.await_enrollment(b.app, url, user)
            assert b_row_again == b_row, (
                f"seat b's second sign-in latched on {b_row_again}, not its named row "
                f"{b_row} — the same install must come back to its own row"
            )

            wait_until(a_lists(1), _ROSTER_WINDOW, interval=2.0,
                       diagnose=diagnose("after the sign-in (one row, not two)"))
            assert len(last["roster"]) == 3 and last["roster"][b_row], (
                f"after seat b signed back in the roster should be a, b (granted again) "
                f"and the harness row, and reads {last['roster']!r}"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


#: Sign-in → immediate reset cycles for the cut-short arm — the count
#: `test_sign_out_device_roster.py`'s mid-assembly case measured as a rate.
_QUICK_CYCLES = 3


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * _ROSTER_WINDOW + _QUICK_CYCLES * 60 + 300))
@pytest.mark.parametrize("seat_pair", _COMPUTER_PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
def test_an_app_and_its_helper_are_one_row_and_a_cut_short_sign_out_still_retires_it(
    nest_instance, request, seat_pair
):
    """Two computers, each running the app AND its sync helper (a bound folder,
    the agent serving it): the roster holds one row per computer — never one
    for the app and another for its helper — and seat `a`'s Devices page lists
    `b` once. Then `b` is signed out the instant each of several sign-ins
    lands, while its account runtime is still assembling and its helper is up
    (`sync-agent-credentials.md` § Credential model: a sign-out cuts the
    assembly or pass it lands in and retires the enrollment inside the stop):
    `b`'s row stays, its grant cleared, and no second row is ever left behind.

    **Why ``driver.reset()``** for the cut-short sign-outs: the window under
    test is the second after sign-in, which the Settings walk would spend — the
    same reasoning and the same per-test ``StopReason::SignOut`` teardown as
    ``test_sign_out_device_roster.py``'s mid-assembly case. Whether a given
    reset lands inside the assembly is timing; the final equality does not
    depend on it (convention 14), and a stranded second row never leaves on its
    own, so a regression cannot pass by being slow.
    """
    from tests.test_filesync_seats import harness_sign_in

    account = _account(nest_instance)
    port, url, secret_key, actor_id, handle = account
    user = {"actor_id_hex": actor_id, "signing_key": bytes.fromhex(secret_key)}
    folder = f"onerow-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    # Every row listed before either computer starts is the harness's own
    # enrolment (the folder's creation registers it, under an id of its own).
    harness = set(enrollment.principals(url, user))

    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            da = a.app.driver

            _w, a_row, _ = enrollment.await_enrollment(a.app, url, user)
            _w, b_row, _ = enrollment.await_enrollment(b.app, url, user)
            assert a_row != b_row, (
                f"two installs latched on one row {a_row} — each computer is its own row"
            )

            # Each computer runs its helper as a process of its own, observed
            # from its socket's peer, never from the app's say-so.
            helpers_running = {name: s.agent_pid() for name, s in (("a", a), ("b", b))}
            assert all(helpers_running.values()), (
                f"a seat's sync helper is not serving its endpoint: {helpers_running!r} — "
                "the one-row claim below would then be about an app alone"
            )

            last: dict = {"harness": harness}

            def computer_rows(roster: dict) -> dict:
                """The roster minus the harness's own enrolment."""
                return {k: v for k, v in roster.items() if k not in harness}

            def one_row_each() -> bool:
                a.app.backups.navigate_devices()
                last["names"] = _sibling_cards(da)
                last["roster"] = enrollment.principals(url, user)
                rows = computer_rows(last["roster"])
                return (
                    len(last["names"]) == 1
                    and set(rows) == {a_row, b_row}
                    and all(rows.values())
                )

            def diagnose(what: str):
                return lambda: (
                    f"{what}: seat a's Devices page lists siblings {last.get('names')!r}; "
                    f"nest roster (device id → principal): {last.get('roster')!r}, "
                    f"the harness's row(s) {last.get('harness')!r}; "
                    f"the computers' rows are a={a_row}, b={b_row} — any other row is the "
                    f"app and its helper counted apart, or a sign-out's stranded row\n"
                    f"{b.diagnostics()}"
                )

            wait_until(one_row_each, _ROSTER_WINDOW, interval=2.0,
                       diagnose=diagnose("app and helper running on both computers"))

            sign_in_b = harness_sign_in(url, secret_key, actor_id, handle)
            for _ in range(_QUICK_CYCLES):
                b.app.driver.reset()
                sign_in_b(b.app, "b")
            b.app.driver.reset()

            def b_retired() -> bool:
                last["roster"] = enrollment.principals(url, user)
                rows = computer_rows(last["roster"])
                return (
                    set(rows) == {a_row, b_row}
                    and rows[b_row] is None
                    and bool(rows[a_row])
                )

            wait_until(
                b_retired, enrollment.RETIREMENT_VISIBLE_S, interval=1.0,
                diagnose=lambda: (
                    f"after {_QUICK_CYCLES} sign-in → immediate sign-out cycles on seat b "
                    f"the nest roster reads {last.get('roster')!r}: b's row {b_row} should "
                    f"stay with its grant cleared, a's row {a_row} untouched, and no other "
                    f"row. A missing b row is a sign-out that deleted the user's device; a "
                    f"granted one, a cut-short sign-out that never retired it. Grep b's app "
                    f"log for \"the machine's enrollment retirement\".\n{b.diagnostics()}"
                ),
            )

            def a_still_lists_b_once() -> bool:
                a.app.backups.navigate_devices()
                last["names"] = _sibling_cards(da)
                return len(last["names"]) == 1

            wait_until(a_still_lists_b_once, _ROSTER_WINDOW, interval=2.0,
                       diagnose=diagnose("after the cut-short sign-outs (the row stays, once)"))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

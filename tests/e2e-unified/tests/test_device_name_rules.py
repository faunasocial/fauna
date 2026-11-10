"""Device names past the rename itself: a chosen name outlives its app's default
re-register, alike names are told apart by a code, and a reserved name is
refused in words with nothing sent — ``devices.md`` § Device names, rules (b)
and (c), and the page's refusal (``ui/devices.md`` § Device names).

Two seats of one account, no operator (convention 16) — two tui seats, two of
this platform's native app, and the reading seat `a` played by the web app or
the phone (``test_device_names.journey_seat_sets``); every mutation is a
UI gesture on a Devices page (convention 8) and every wait a deadline poll on
state (convention 14). The nest's own rows are read only to know that a
re-register has happened and to say, in a failure, what the page should have
painted (convention 6) — never to stand in for the user.

Owner: ``docs/goal/behavior/devices.md`` § Device names.
"""

import contextlib
import secrets
import shutil
import tempfile
from pathlib import Path

import pytest

from helpers.waiting import wait_until
from tests.test_device_names import (
    _CODE_SEPARATOR,
    _DESKTOP_DEFAULTS,
    _ROSTER_WINDOW,
    _nest_rows,
    _own_name,
    _sibling_card,
    journey_seat_sets,
)
from tests.test_device_to_device_file_transfer import (
    STARTUP_WINDOW,
    _account,
    _seat_factory,
)

pytestmark = [pytest.mark.tier_3]

#: `a` reads and renames, `b` is the folder-binding sibling it names.
_PAIRS = journey_seat_sets(("tui", "tui"), reader=0)
#: App defaults — synthetic labels a rename must refuse
#: (``fauna_core::label_custody::APP_DEVICE_LABELS``). The test types one that
#: no seat of the run already carries: a phone seat's own row rests as
#: ``fauna-ios`` (or ``fauna-android``), so typing that one could not show the
#: refused name stayed off the nest.
_RESERVED_CANDIDATES = ("fauna-ios", "fauna-android", "fauna-windows", "fauna-linux")
#: ``i18n/strings/en.yaml`` ``devices.error_rename_reserved_name``.
_RESERVED_REFUSAL = (
    "That name is the one an app gives a device you haven't named. Choose another."
)


def _painted_names(driver) -> list:
    driver.wait_for("device-card", timeout=30.0)
    return [
        driver.get_text("device-name", scope=f"device-card[{i}]")
        for i in range(driver.count("device-card"))
    ]


def _own_card(driver) -> int | None:
    driver.wait_for("device-card", timeout=30.0)
    for i in range(driver.count("device-card")):
        if not driver.is_absent("device-this-mark-badge", scope=f"device-card[{i}]"):
            return i
    return None


def _error_text(driver):
    return None if driver.is_absent("error-message") else driver.get_text("error-message")


def _rename(driver, card: int, name: str) -> None:
    driver.click("device-rename-button", scope=f"device-card[{card}]")
    driver.wait_for("device-rename-input", timeout=15.0)
    driver.fill("device-rename-input", name)
    driver.click("device-rename-confirm-button")


def _raw_rows(account) -> list:
    """The nest's rows with ``registered_at`` — the column every register leg
    restamps and a rename never touches (``register_sync_device_capped``)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    _port, url, secret_key, actor_id, _handle = account
    with WsRpcAdminClient(url, bytes.fromhex(actor_id), bytes.fromhex(secret_key)) as client:
        return client.call("fauna.sync.devices.list", {}).get("devices", [])


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + 6 * _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.ios
@pytest.mark.android
def test_a_chosen_name_survives_its_apps_default_reregister_and_alike_names_carry_a_code(
    nest_instance, request, seat_pair
):
    """Devices outcome 29. Seat `a` names seat `b` ``X``; `b` quits and comes
    back, and its engine registers again under its app default (the row's
    ``registered_at`` moves); `a`'s page and `b`'s own card still read ``X``
    — rule (b): a machine-authored label never displaces a user-chosen one.
    Then `a` names its own device ``X`` too, and both cards read ``X · <code>``
    with two different codes — rule (c), on names the user chose, with no
    user action beyond the naming.
    """
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"name-rules-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    chosen = f"workshop {secrets.token_hex(3)}"
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    from helpers import sync_seats

    seat = _seat_factory(
        nest_instance, request, account, folder, tmp,
        sync_seats.new_run_token(seats=2), seat_pair,
    )

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            da, db = a.app.driver, b.app.driver
            last: dict = {}

            def sibling_listed() -> bool:
                a.app.backups.navigate_devices()
                last["card"] = _sibling_card(da)
                return last["card"] is not None

            wait_until(
                sibling_listed, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: f"seat a never listed seat b; nest rows: {_nest_rows(account)!r}",
            )
            # The harness's own enrolment rests sealed too (its name is a
            # chosen one), so b's row is the one the rename newly sealed.
            sealed_before = {
                d.get("device_id") for d in _raw_rows(account) if d.get("label_sealed")
            }
            _rename(da, last["card"], chosen)

            def sealed_row():
                rows = [
                    d for d in _raw_rows(account)
                    if d.get("label_sealed") and d.get("device_id") not in sealed_before
                ]
                return rows[0] if len(rows) == 1 else None

            def b_named() -> bool:
                a.app.backups.navigate_devices()
                card = _sibling_card(da)
                last["a_name"] = (
                    None if card is None
                    else da.get_text("device-name", scope=f"device-card[{card}]")
                )
                return last["a_name"] == chosen and sealed_row() is not None

            wait_until(
                b_named, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's card for b reads {last.get('a_name')!r}, not {chosen!r}; "
                    f"error-message: {_error_text(da)!r}; nest rows: {_nest_rows(account)!r}"
                ),
            )
            renamed = sealed_row()
            b_device, stamped = renamed["device_id"], renamed.get("registered_at")

            # ── b quits and comes back: its engine registers under the default ──
            b.stop_keeping_state()
            b.restart(STARTUP_WINDOW)

            def reregistered() -> bool:
                row = next(
                    (d for d in _raw_rows(account) if d.get("device_id") == b_device), None
                )
                last["b_row"] = row
                return row is not None and (row.get("registered_at") or 0) > (stamped or 0)

            wait_until(
                reregistered, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b's row never re-registered after its restart (registered_at "
                    f"was {stamped!r}, row now {last.get('b_row')!r})\n{b.diagnostics()}"
                ),
            )

            def a_still_reads_chosen() -> bool:
                a.app.backups.navigate_devices()
                card = _sibling_card(da)
                last["a_name"] = (
                    None if card is None
                    else da.get_text("device-name", scope=f"device-card[{card}]")
                )
                return last["a_name"] is not None

            wait_until(
                a_still_reads_chosen, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: f"seat a lost b's card; nest rows: {_nest_rows(account)!r}",
            )
            assert last["a_name"] == chosen, (
                f"after seat b's default re-register seat a's card for it reads "
                f"{last['a_name']!r}, not the chosen {chosen!r} — the app default "
                f"displaced the user's name (rule (b)); nest rows: {_nest_rows(account)!r}"
            )

            def b_reads_chosen() -> bool:
                b.app.backups.navigate_devices()
                last["b_own"] = _own_name(db)
                return last["b_own"] == chosen

            wait_until(
                b_reads_chosen, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b's own card reads {last.get('b_own')!r} after its restart, not "
                    f"{chosen!r}; nest rows: {_nest_rows(account)!r}"
                ),
            )

            # ── a names itself the same: two alike names, two codes ──
            a.app.backups.navigate_devices()
            own = _own_card(da)
            assert own is not None, f"seat a's page has no this-device card: {_painted_names(da)!r}"
            _rename(da, own, chosen)

            def both_coded() -> bool:
                a.app.backups.navigate_devices()
                names = [n for n in _painted_names(da) if n.startswith(chosen)]
                last["alike"] = names
                return len(names) == 2 and all(_CODE_SEPARATOR in n for n in names)

            wait_until(
                both_coded, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's page never painted both {chosen!r} rows with a code "
                    f"(it reads {last.get('alike')!r}, all names {_painted_names(da)!r}); "
                    f"error-message: {_error_text(da)!r}; nest rows: {_nest_rows(account)!r}"
                ),
            )
            halves = [n.partition(_CODE_SEPARATOR) for n in last["alike"]]
            assert all(name == chosen and code for name, _sep, code in halves), (
                f"the alike rows read {last['alike']!r}, not {chosen!r}{_CODE_SEPARATOR}<code>"
            )
            assert halves[0][2] != halves[1][2], (
                f"the two alike rows carry one code {last['alike']!r} — rule (c) did "
                f"not tell them apart"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.ios
@pytest.mark.android
def test_a_rename_to_another_apps_default_name_is_refused_in_words_and_sends_nothing(
    nest_instance, request, seat_pair
):
    """Devices outcome 30, the reserved-name refusal: seat `a` opens the editor
    on seat `b`'s card, types an app default name no seat of the run carries (``fauna-ios``, say) — and
    confirms. The page says why on ``error-message``; `b`'s card keeps its
    painted name — its app default, with rule (c)'s ``· <code>`` when `a`
    shares that default — `b`'s own page too, and the nest's row stays the
    plaintext default with no seal — nothing was sent.

    The other refusal, no owner key, has no state a signed-in seat reaches:
    every app hands the Devices machine its owner key whenever its identity
    secret decodes (tui's ``apps/fauna-tui/src/settings/devices.rs``), which
    every signed-in seat's does. That branch is pinned on the shared machine itself
    (``libs/fauna-devices-machine/tests/devices_lifecycle.rs``).
    """
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"name-refusal-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    from helpers import sync_seats

    seat = _seat_factory(
        nest_instance, request, account, folder, tmp,
        sync_seats.new_run_token(seats=2), seat_pair,
    )

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            da, db = a.app.driver, b.app.driver
            last: dict = {}
            # `b`'s painted name: its app default, coded when `a`'s app shares
            # that default (rule (c)) — two tui or two native seats.
            b_default = _DESKTOP_DEFAULTS[seat_pair[1]]
            b_painted_prefix = (
                b_default + _CODE_SEPARATOR if seat_pair[0] == seat_pair[1] else b_default
            )

            def b_painted(name) -> bool:
                return bool(name) and (
                    name.startswith(b_painted_prefix)
                    if seat_pair[0] == seat_pair[1] else name == b_default
                )

            def sibling_coded() -> bool:
                a.app.backups.navigate_devices()
                card = _sibling_card(da)
                last["card"] = card
                last["before"] = (
                    None if card is None
                    else da.get_text("device-name", scope=f"device-card[{card}]")
                )
                return b_painted(last["before"])

            wait_until(
                sibling_coded, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a never painted b as {b_painted_prefix!r}… "
                    f"(it reads {last.get('before')!r}); nest rows: {_nest_rows(account)!r}"
                ),
            )
            before = last["before"]
            rows_before = sorted(
                (d.get("device_id"), d.get("label"), bool(d.get("label_sealed")))
                for d in _raw_rows(account)
            )
            labels_before = {label for _id, label, _s in rows_before}
            reserved = next(n for n in _RESERVED_CANDIDATES if n not in labels_before)

            _rename(da, last["card"], reserved)

            def refused() -> bool:
                last["error"] = _error_text(da)
                return bool(last["error"])

            wait_until(
                refused, 30.0, interval=1.0,
                diagnose=lambda: (
                    f"no refusal reached error-message after typing {reserved!r}; "
                    f"nest rows: {_nest_rows(account)!r}"
                ),
            )
            assert last["error"] == _RESERVED_REFUSAL, (
                f"the refusal reads {last['error']!r}, not the reserved-name sentence "
                f"{_RESERVED_REFUSAL!r}"
            )

            rows_after = sorted(
                (d.get("device_id"), d.get("label"), bool(d.get("label_sealed")))
                for d in _raw_rows(account)
            )
            assert rows_after == rows_before, (
                f"the refused rename reached the nest: rows {rows_before!r} became "
                f"{rows_after!r}"
            )
            assert not any(label == reserved for _id, label, _s in rows_after), rows_after

            a.app.backups.navigate_devices()
            card = _sibling_card(da)
            assert card is not None, f"seat a lost b's card: {_painted_names(da)!r}"
            after = da.get_text("device-name", scope=f"device-card[{card}]")
            assert after == before, (
                f"after the refusal seat a's card for b reads {after!r}, not {before!r}"
            )

            b.app.backups.navigate_devices()
            b_own = _own_name(db)
            assert b_painted(b_own), (
                f"seat b's own card reads {b_own!r} after the refused rename, not "
                f"{b_painted_prefix!r}…; nest rows: {_nest_rows(account)!r}"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

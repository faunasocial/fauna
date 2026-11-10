"""A device you have not named is listed under its app's name on a sibling's
Devices page — a phone that syncs no folder included: the app-default half of
``devices.md`` § Device names, class (2).

Every register leg of a machine passes the app's default name, the enrollment
ceremony's register-create included, so a phone's roster row is born
``fauna-ios`` / ``fauna-android`` and never the ceremony's old placeholder
``fauna``. A desktop seat cannot witness the ceremony's leg on its own: its
folder's engine registers again under the app default, renaming whatever the
ceremony wrote. A phone binds no folder (``sync_seats.DeviceSeatDriver``), so no
in-process engine ever registers it and the ceremony's register-create is the
only name its row ever gets — the case where the placeholder would show.

The desktop witness is the plainer case: two seats of one account, the second
a desktop seat binding a folder so its engine registers; the first's Devices
page names the second for its app. The first — the reader — is the same desktop
app, or the web app in a browser (the ``web+tui`` pair, a device-only seat whose
page draws the roster through the same shared ``render_devices``).

Two seats of one account, no operator (convention 16): the ``tui+phone`` pair,
tui reading its Devices page while the phone app signs in. The phone does
nothing but sign in; the assertion is the name tui's roster paints for it, read
off tui's own Devices page (convention 8: the sibling's view, not an API call
standing in for the user). Every wait is a deadline poll on state (convention
14).

Owner: ``docs/goal/behavior/devices.md`` § Device names.
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
from tests.test_device_to_device_file_transfer import (
    STARTUP_WINDOW,
    _account,
    _seat_factory,
)

pytestmark = [pytest.mark.tier_3]

# Seat `a` is tui (a folder-syncing desktop seat, whose page is read); seat `b`
# is the phone — the iOS simulator on macOS, the android emulator on Linux
# (`sync_seats.PHONE_APPS`).
_PHONE_PAIR = ("tui", sync_seats.PHONE_MODE)
_PHONE_PAIRS = (
    [pytest.param(_PHONE_PAIR, id=sync_seats.seat_set_param_id(_PHONE_PAIR))]
    if sync_seats.phone_app_for_platform(sys.platform)
    else []
)

# The desktop pairs: two seats of one desktop app, `a` reading its page, `b`
# the sibling it names — tui, and this platform's own native app (`native`
# resolves through `sync_seats.NATIVE_APPS`; `seat_sets_for_platform` drops a
# set this platform cannot drive).
_DESKTOP_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(
        sys.platform, (("tui", "tui"), ("native", "native"))
    )
]
# The web reader's pair: seat `a` is the web app (a device-only seat, so the
# ledger credits web alone), seat `b` the folder-binding tui seat it names. web
# runs on Linux alone (`sync_seats.WEB_APPS`).
_WEB_READER_PAIR = (sync_seats.WEB_MODE, "tui")
if sync_seats.seat_mode_app(sync_seats.WEB_MODE, sys.platform):
    _DESKTOP_PAIRS.append(
        pytest.param(_WEB_READER_PAIR, id=sync_seats.seat_set_param_id(_WEB_READER_PAIR))
    )



def journey_seat_sets(tui_set: tuple, *, reader: int | None = 0) -> list:
    """The seat sets a Devices-page journey first written over ``tui_set`` (all
    ``tui``) collects here, as ``pytest.param``s: the tui set itself, its
    ``native`` twin (this platform's own GUI app in every seat), and — when
    ``reader`` names a seat — the same set with that seat played by each
    device-only mode this platform launches (``web`` on Linux, ``phone``: the
    android emulator on Linux, the iOS simulator on macOS).

    The device-only seat is always the one whose Devices page the journey reads
    and acts on, the folder-binding seats around it the devices it names — the
    shape outcomes 18 and 19 took first — so the feature ledger credits the web
    or phone app alone for that set (``sync_seats.DEVICE_ONLY_MODES``). A
    journey whose subject is a desktop-only fact (an app beside its sync helper)
    passes ``reader=None`` and collects the tui and native sets alone. A set
    naming a mode this platform cannot launch is dropped at collection, never
    skipped (convention 7)."""
    sets = [tuple(tui_set), tuple("native" for _ in tui_set)]
    if reader is not None:
        for mode in (sync_seats.WEB_MODE, sync_seats.PHONE_MODE):
            seats = list(tui_set)
            seats[reader] = mode
            sets.append(tuple(seats))
    launchable = [
        s for s in sync_seats.seat_sets_for_platform(sys.platform, tuple(sets))
        if all(sync_seats.seat_mode_app(m, sys.platform) for m in s)
    ]
    return [pytest.param(s, id=sync_seats.seat_set_param_id(s)) for s in launchable]


#: The app default each desktop seat mode registers under; the native seat's
#: is its platform app's (``fauna-macos`` on macOS).
_DESKTOP_DEFAULTS = {"tui": "fauna-tui"}
if sync_seats.native_app_for_platform(sys.platform):
    _DESKTOP_DEFAULTS["native"] = f"fauna-{sync_seats.native_app_for_platform(sys.platform)}"
#: Rule (c)'s separator: a row whose name another row of the list renders
#: identically reads ``name · code`` — two tui seats share ``fauna-tui``.
_CODE_SEPARATOR = " · "

#: The app default each phone app registers under (``devices.md`` § Device
#: names, class (2); apple's ``FaunaClient.deviceLabel``).
_PHONE_DEFAULTS = {"ios": "fauna-ios", "android": "fauna-android"}
#: The harness's own enrolment — an offline row the roster also lists.
_HARNESS_ROW = "e2e-harness"
#: The phone's row reaches tui's roster on the next hydrate after it registers.
_ROSTER_WINDOW = 120.0


def _sibling_names(driver) -> list:
    """The names of every card on the page that is neither this device's own
    (the this-device marker) nor the harness's enrolment."""
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


def _nest_rows(account) -> list:
    """The nest's own rows for this account as ``(device_id, label, sealed?,
    online)`` — what the Devices page should be painting (convention 6). An app
    default is one of the synthetic labels, which rest plaintext; a user-chosen
    one rests only sealed."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    _port, url, secret_key, actor_id, _handle = account
    try:
        with WsRpcAdminClient(url, bytes.fromhex(actor_id), bytes.fromhex(secret_key)) as client:
            reply = client.call("fauna.sync.devices.list", {})
    except Exception as exc:  # a diagnosis never masks the failure it explains
        return [f"unreadable ({exc!r})"]
    return [
        (d.get("device_id"), d.get("label"), bool(d.get("label_sealed")), d.get("online"))
        for d in reply.get("devices", [])
    ]


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _PHONE_PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.ios
@pytest.mark.android
def test_a_phone_that_syncs_no_folder_is_named_for_its_app_on_a_siblings_devices_page(
    nest_instance, request, seat_pair
):
    """The phone signs in and binds no folder; tui's Devices page lists its row
    as exactly the phone app's default name — never the placeholder ``fauna``.

    Before the app default reached the ceremony (``AccountRuntimeParams::
    enrollment_device_label``), the register-create named an absent row
    ``SELF_REGISTER_LABEL`` (``fauna``), and with no folder no engine ever
    registered the phone again to rename it, so this page read ``fauna``.
    """
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    # tui's seat is a folder-syncing seat and binds one; the phone binds none.
    folder = f"names-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    phone_app = sync_seats.phone_app_for_platform(sys.platform)
    expected = _PHONE_DEFAULTS[phone_app]
    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)

            last: dict = {}

            def phone_listed() -> bool:
                a.app.backups.navigate_devices()
                last["names"] = _sibling_names(a.app.driver)
                return bool(last["names"])

            wait_until(
                phone_listed, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"tui's Devices page never listed the phone's row (sibling names "
                    f"{last.get('names')!r}); nest rows (device_id, label, sealed?, online): "
                    f"{_nest_rows(account)!r}\n{b.diagnostics()}"
                ),
            )
            # One phone of the app, so its name is the bare default — no
            # `· <code>` suffix, which only rows sharing a name carry.
            assert last["names"] == [expected], (
                f"tui's Devices page names the folder-less {phone_app} phone "
                f"{last['names']!r}, not [{expected!r}] — the enrollment ceremony's "
                f"register-create did not pass the app default; nest rows "
                f"(device_id, label, sealed?, online): {_nest_rows(account)!r}"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _DESKTOP_PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
def test_an_unnamed_desktop_device_is_named_for_its_app_on_a_siblings_devices_page(
    nest_instance, request, seat_pair
):
    """Two seats of one account; `b` is a desktop seat and binds the folder, so
    its engine registers it. `a`'s Devices page lists `b`'s row under `b`'s app
    default — never the placeholder ``fauna`` (``SELF_REGISTER_LABEL``).

    On a same-app pair both rows carry one name and rule (c) tells them apart
    by the code: `b`'s card reads ``fauna-tui · <code>`` (or ``fauna-macos ·
    <code>`` / ``fauna-linux · <code>`` on the native pair). The name half is
    the outcome's; the code half only has to be present. On the ``web+tui``
    pair the reader's own row is the web ceremony's — no web default is
    ratified, so it rests as the placeholder (`devices.md` § Implementation
    status today, the four placeholder legs) — and `b`'s ``fauna-tui`` is alike
    to no other row, so it reads bare: rule (c) widens only alike names.
    """
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"names-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    expected = _DESKTOP_DEFAULTS[seat_pair[1]]
    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)

            last: dict = {}

            def sibling_listed() -> bool:
                a.app.backups.navigate_devices()
                last["names"] = _sibling_names(a.app.driver)
                return bool(last["names"])

            wait_until(
                sibling_listed, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's Devices page never listed seat b's row (sibling names "
                    f"{last.get('names')!r}); nest rows (device_id, label, sealed?, online): "
                    f"{_nest_rows(account)!r}\n{b.diagnostics()}"
                ),
            )
            names = last["names"]
            assert len(names) == 1, (
                f"seat a's Devices page lists {names!r} beside itself, not one sibling; "
                f"nest rows (device_id, label, sealed?, online): {_nest_rows(account)!r}"
            )
            name, sep, code = names[0].partition(_CODE_SEPARATOR)
            if seat_pair[0] == seat_pair[1]:
                assert name == expected and sep and code, (
                    f"seat a's Devices page names its unnamed {seat_pair[1]} sibling "
                    f"{names[0]!r}, not {expected!r}{_CODE_SEPARATOR}<code> — a register "
                    f"leg did not pass the app default, or rule (c) did not tell the two "
                    f"alike rows apart; nest rows (device_id, label, sealed?, online): "
                    f"{_nest_rows(account)!r}"
                )
            else:
                assert names[0] == expected, (
                    f"the {seat_pair[0]} seat's Devices page names its unnamed "
                    f"{seat_pair[1]} sibling {names[0]!r}, not {expected!r} — a register "
                    f"leg did not pass the app default, or rule (c) widened a name no "
                    f"other row shares; nest rows (device_id, label, sealed?, online): "
                    f"{_nest_rows(account)!r}"
                )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


#: Two seats of one desktop app: tui (the lead, first) and this platform's own
#: native app (``ui/devices.md`` § Device names — the other apps by
#: trickle-down; ``seat_sets_for_platform`` drops a set this platform cannot
#: drive). The reader is seat `a`; the card it renames is seat `b`'s.
_RENAME_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(
        sys.platform, (("tui", "tui"), ("native", "native"))
    )
]
# The web reader renames the folder-binding tui seat's card (web is a
# device-only seat, Linux alone).
if sync_seats.seat_mode_app(sync_seats.WEB_MODE, sys.platform):
    _RENAME_PAIRS.append(
        pytest.param(_WEB_READER_PAIR, id=sync_seats.seat_set_param_id(_WEB_READER_PAIR))
    )
# The phone is the renamed card on the tui reader's page, then reads its own
# card back through its own driver.
if sync_seats.phone_app_for_platform(sys.platform):
    _RENAME_PAIRS.append(
        pytest.param(_PHONE_PAIR, id=sync_seats.seat_set_param_id(_PHONE_PAIR))
    )


def _default_name(mode: str) -> str:
    """The app default the renamed seat's card reads before the rename."""
    if mode == sync_seats.PHONE_MODE:
        return _PHONE_DEFAULTS[sync_seats.phone_app_for_platform(sys.platform)]
    return _DESKTOP_DEFAULTS[mode]


def _sibling_card(driver) -> int | None:
    """The index of the one card that is neither this device's own nor the
    harness's enrolment, or ``None`` while the roster does not list it yet."""
    driver.wait_for("device-card", timeout=30.0)
    for i in range(driver.count("device-card")):
        scope = f"device-card[{i}]"
        if not driver.is_absent("device-this-mark-badge", scope=scope):
            continue
        if driver.get_text("device-name", scope=scope) != _HARNESS_ROW:
            return i
    return None


def _own_name(driver) -> str | None:
    """The name this device's own card (the this-device marker) paints."""
    driver.wait_for("device-card", timeout=30.0)
    for i in range(driver.count("device-card")):
        scope = f"device-card[{i}]"
        if not driver.is_absent("device-this-mark-badge", scope=scope):
            return driver.get_text("device-name", scope=scope)
    return None


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_pair", _RENAME_PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
def test_a_device_renamed_on_one_seat_reads_its_new_name_on_both(
    nest_instance, request, seat_pair
):
    """The rename affordance (``devices.md`` § Device names, rule (1)): seat
    `a` opens the editor on seat `b`'s card — prefilled with `b`'s NAME, the
    app default without rule (c)'s ``· <code>`` — types a new name and saves.
    `a`'s card for `b` then reads exactly the new name (alike to no other row,
    so bare), and `b`'s own card reads it too: the name is sealed under the
    owner root, so the sibling opens what `a` sealed.

    Every mutation is a UI gesture (convention 8); every wait a deadline poll
    on state (convention 14).
    """
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"rename-{secrets.token_hex(4)}"
    from common import user_create_folder

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    new_name = f"kitchen {secrets.token_hex(3)}"
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

            last: dict = {}

            def sibling_listed() -> bool:
                a.app.backups.navigate_devices()
                last["card"] = _sibling_card(da)
                return last["card"] is not None

            wait_until(
                sibling_listed, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's Devices page never listed seat b's row; nest rows "
                    f"(device_id, label, sealed?, online): {_nest_rows(account)!r}\n"
                    f"{b.diagnostics()}"
                ),
            )
            scope = f"device-card[{last['card']}]"
            da.click("device-rename-button", scope=scope)
            da.wait_for("device-rename-input", timeout=15.0)
            prefill = da.get_text("device-rename-input")
            default = _default_name(seat_pair[1])
            assert prefill == default, (
                f"the rename editor opened on {prefill!r}, not seat b's name "
                f"{default!r} — it must open on the opened name, "
                f"never the painted label with rule (c)'s code"
            )
            da.fill("device-rename-input", new_name)
            da.click("device-rename-confirm-button")

            def a_reads_new_name() -> bool:
                a.app.backups.navigate_devices()
                card = _sibling_card(da)
                last["a_name"] = (
                    None if card is None
                    else da.get_text("device-name", scope=f"device-card[{card}]")
                )
                return last["a_name"] == new_name

            wait_until(
                a_reads_new_name, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's card for seat b reads {last.get('a_name')!r}, not "
                    f"{new_name!r}; error-message: "
                    f"{da.get_text('error-message') if not da.is_absent('error-message') else None!r}; "
                    f"nest rows (device_id, label, sealed?, online): {_nest_rows(account)!r}"
                ),
            )
            assert da.is_absent("device-rename-input"), "the save closes the editor"

            db = b.app.driver

            def b_reads_new_name() -> bool:
                b.app.backups.navigate_devices()
                last["b_name"] = _own_name(db)
                return last["b_name"] == new_name

            wait_until(
                b_reads_new_name, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b's own card reads {last.get('b_name')!r}, not {new_name!r} "
                    f"— the sibling could not open the owner-root seal, or its engine's "
                    f"re-register overwrote the user's name (rule (b)); nest rows "
                    f"(device_id, label, sealed?, online): {_nest_rows(account)!r}"
                ),
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


#: Three seats of one account: `a` acts, `b`'s Devices page stays open and is
#: never navigated after the rename, `c` is the device `a` removes. `b`, the
#: open page, is the seat a device-only app plays (web, phone).
_OPEN_PAGE_SETS = journey_seat_sets(("tui", "tui", "tui"), reader=1)


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + 5 * _ROSTER_WINDOW + 300))
@pytest.mark.parametrize("seat_set", _OPEN_PAGE_SETS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.ios
@pytest.mark.android
def test_an_open_devices_page_repaints_a_sibling_rename_and_removal(
    nest_instance, request, seat_set
):
    """Devices outcome 26 (``devices.md`` § Listing Devices → *An open roster
    follows a sibling's write*): seat `b`'s Devices page is open; seat `a`
    renames `b`'s card, and `b`'s own card reads the new name; `a` then
    removes seat `c`, and `c`'s card drops from `b`'s page — with no
    navigation on `b` between either write and its assertion.

    A rename is nest-only (nothing lands in any account store), so before the
    roster push (``fauna.sync.devices.changed`` → ``StaleSurfaces::devices``)
    `b`'s open page kept the old name until the user left and came back. Every
    mutation is a UI gesture (convention 8); every wait a deadline poll on
    state (convention 14).
    """
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"open-roster-{secrets.token_hex(4)}"
    from common import user_create_folder
    from helpers.waiting import await_device_removal_ready

    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    new_name = f"study {secrets.token_hex(3)}"
    run_token = sync_seats.new_run_token(seats=3)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_set)

    def painted_names(driver) -> list:
        driver.wait_for("device-card", timeout=30.0)
        return [
            driver.get_text("device-name", scope=f"device-card[{i}]")
            for i in range(driver.count("device-card"))
        ]

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            c = stack.enter_context(seat("c"))
            for s in (a, b, c):
                s.await_ready(STARTUP_WINDOW)
            da, db = a.app.driver, b.app.driver
            last: dict = {}

            # Each seat's own painted name. Rows sharing an app default
            # (three tui seats, or a and c beside a web or phone b) read
            # `<default> · <code>` by rule (c), and the code is set-relative —
            # one roster, so every page paints a row alike; the three names
            # are therefore three distinct strings on every page.
            def own_name_on(s, key) -> bool:
                s.app.backups.navigate_devices()
                last[key] = _own_name(s.app.driver)
                return bool(last[key])

            for s, key in ((a, "a_own"), (b, "b_own"), (c, "c_own")):
                wait_until(
                    lambda s=s, key=key: own_name_on(s, key), _ROSTER_WINDOW,
                    interval=2.0,
                    diagnose=lambda key=key: (
                        f"seat {key[0]}'s own card never read a name "
                        f"({last.get(key)!r}); nest rows (device_id, label, sealed?, "
                        f"online): {_nest_rows(account)!r}"
                    ),
                )

            # `b`'s page is open from here on, and lists all three seats.
            def b_lists_everyone() -> bool:
                b.app.backups.navigate_devices()
                last["b_names"] = painted_names(db)
                return all(
                    last[k] in last["b_names"] for k in ("a_own", "b_own", "c_own")
                )

            wait_until(
                b_lists_everyone, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b's Devices page never listed all three seats "
                    f"({last.get('b_names')!r}); nest rows: {_nest_rows(account)!r}"
                ),
            )

            def card_on_a(name: str) -> int | None:
                a.app.backups.navigate_devices()
                names = painted_names(da)
                last["a_names"] = names
                return names.index(name) if name in names else None

            def a_lists(name: str) -> bool:
                return card_on_a(name) is not None

            wait_until(
                lambda: a_lists(last["b_own"]) and a_lists(last["c_own"]),
                _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's Devices page never listed b {last['b_own']!r} and c "
                    f"{last['c_own']!r} (it reads {last.get('a_names')!r})"
                ),
            )

            # ── The rename, from `a`; `b` is not navigated again ──
            scope = f"device-card[{card_on_a(last['b_own'])}]"
            da.click("device-rename-button", scope=scope)
            da.wait_for("device-rename-input", timeout=15.0)
            da.fill("device-rename-input", new_name)
            da.click("device-rename-confirm-button")

            def b_reads_new_name() -> bool:
                last["b_now"] = _own_name(db)
                return last["b_now"] == new_name

            wait_until(
                b_reads_new_name, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b's OPEN Devices page still reads {last.get('b_now')!r} for "
                    f"its own card, not {new_name!r} — the roster push did not reach "
                    f"it, or tui did not re-read on it; nest rows (device_id, label, "
                    f"sealed?, online): {_nest_rows(account)!r}\n{b.diagnostics()}"
                ),
            )

            # ── The removal, from `a`; `b` is still not navigated ──
            await_device_removal_ready(da)
            index = card_on_a(last["c_own"])
            assert index is not None, f"seat a lost c's card: {last.get('a_names')!r}"
            da.click("device-remove-button", scope=f"device-card[{index}]")

            def c_dropped_from_b() -> bool:
                last["b_names"] = painted_names(db)
                return last["c_own"] not in last["b_names"]

            wait_until(
                c_dropped_from_b, _ROSTER_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b's OPEN Devices page still lists the removed seat c "
                    f"{last['c_own']!r} ({last.get('b_names')!r}); error-message on a: "
                    f"{da.get_text('error-message') if not da.is_absent('error-message') else None!r}; "
                    f"nest rows: {_nest_rows(account)!r}\n{c.diagnostics()}"
                ),
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

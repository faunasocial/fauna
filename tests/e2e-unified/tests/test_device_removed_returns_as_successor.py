"""A device you removed and then sign in on again comes back as ONE new device
in the list, and the change it made that never reached your nest is kept.

Owner: ``docs/goal/architecture/account-replica-posture.md`` § The store device
principal → *Principal succession after a device delete* (decision 3: the
un-pushed tail is RE-AUTHORED under the successor, never resurrected; decision
2: the successor is a fresh principal that re-creates the machine's named row
the delete removed — one new device on the machine's own row); status
``docs/goal/architecture/account-data-plane.md`` § Implementation status today
(delete-then-sign-in revives the machine as a NEW fleet device with its
un-pushed tail preserved). Feature outcome: ``docs/features/devices.md``
§ Coverage contract, outcome 20. The mechanism is pinned below the app by
``conformance_account_plane_bind``; this is the user's view of it (convention
8): two seats of one account, no operator (convention 16) — two tui seats, two
of this platform's native app, and the removing, reading seat ``a`` played by
the web app or the phone (``test_device_names.journey_seat_sets``); the
returning device ``b`` is always a folder-binding desktop seat, the one whose
nest link the test can cut.

**The hold is causal, never timed (convention 14).** Seat ``b`` reaches the
nest only through a :class:`helpers.nest_link.NestLink` the test holds. With the
link cut nothing ``b`` writes can reach the nest, whatever its push cadence:
``a`` removes ``b`` on its Devices page, ``b`` mutes a word (into its own store
only), and only then is the link restored. A seat left connected would push the
word within a pass, and a seed-holding seat rotates as soon as it reads its own
``Removed`` row — "a change after the removal, before the rotation" is a race
there and a plain fact here: ``b`` cannot read the row while it is cut off.

Every wait is a deadline poll on state: ``b``'s own connection status, the
nest's roster, ``b``'s enrollment latch, and ``a``'s pages re-read until they
agree.
"""

import contextlib
import secrets
import shutil
import tempfile
from pathlib import Path

import pytest

from helpers import enrollment, sync_seats
from helpers.nest_link import NestLink
from helpers.waiting import wait_until
from tests.test_device_names import journey_seat_sets
from tests.test_device_to_device_file_transfer import STARTUP_WINDOW, _account

pytestmark = [pytest.mark.tier_3]

#: Seat `a` removes and reads, seat `b` returns as a successor.
_PAIRS = journey_seat_sets(("tui", "tui"), reader=0)

#: The harness's own enrolment — an offline row every roster also lists.
_HARNESS_ROW = "e2e-harness"
#: A roster change reaches `a`'s page on its next visit's hydrate.
_ROSTER_WINDOW = 120.0
#: The cut reaching `b`'s own connection status.
_OFFLINE_WINDOW = 120.0
#: `b`'s reconnect after the restore, its successor's mint and enrollment, and
#: the re-authored tail's push — a generous POSITIVE budget (convention 14).
_SUCCESSION_WINDOW = 360.0
#: The muted word reaching `a`'s own Muted words page.
_TAIL_WINDOW = 240.0


def _sibling_cards(driver) -> list[tuple[int, str]]:
    """``(index, name)`` of every card that is neither this device's own (the
    this-device marker) nor the harness's enrolment."""
    driver.wait_for("device-card", timeout=30.0)
    cards = []
    for i in range(driver.count("device-card")):
        scope = f"device-card[{i}]"
        if not driver.is_absent("device-this-mark-badge", scope=scope):
            continue
        name = driver.get_text("device-name", scope=scope)
        if name != _HARNESS_ROW:
            cards.append((i, name))
    return cards


def _connection_status(driver) -> str:
    try:
        return driver.get_text("connection-status") or ""
    except Exception as exc:  # an app mid-reconnect reads as not connected
        return f"unreadable ({exc!r})"


def _latched_row(app, user) -> str | None:
    store = enrollment.attach_account_store(enrollment.app_name(app.driver), app.driver)
    return enrollment.latched_row_in(store.read_map(), user["actor_id_hex"])


@pytest.mark.timeout(
    int(2 * STARTUP_WINDOW + 3 * _ROSTER_WINDOW + _OFFLINE_WINDOW
        + _SUCCESSION_WINDOW + _TAIL_WINDOW + 300)
)
@pytest.mark.parametrize("seat_pair", _PAIRS)
@pytest.mark.feature("devices")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.ios
@pytest.mark.android
def test_a_removed_device_signed_in_again_is_one_new_device_and_keeps_its_offline_change(
    nest_instance, request, seat_pair
):
    """``b`` is cut off from the nest; ``a`` removes ``b``; ``b`` mutes a word,
    which lands in its own store only; the link comes back. ``b`` returns as a
    successor — its named row, which the removal deleted, re-created under a
    NEW principal, the removed one granted nowhere — so ``a``'s Devices page
    lists exactly one sibling, never two, and the word ``b`` muted offline shows
    on ``a``'s own Muted words page."""
    from common import user_create_folder
    from tests.test_filesync_seats import harness_sign_in

    account = _account(nest_instance)
    port, url, secret_key, actor_id, handle = account
    user = {"actor_id_hex": actor_id, "signing_key": bytes.fromhex(secret_key)}
    folder = f"succession-{secrets.token_hex(4)}"
    word = f"offline{secrets.token_hex(3)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    modes = dict(zip(sync_seats.seat_names(2), seat_pair))
    link = NestLink(url)
    # A web seat's browser reaches the nest only through the SPA proxy; the web
    # seat is always `a`, which talks to the nest directly (`b` alone is cut).
    spa_url = (
        request.getfixturevalue("spa_url") if sync_seats.WEB_MODE in seat_pair else None
    )

    def seat(name: str, node_url: str):
        return sync_seats.make_seat(
            modes[name],
            name=name,
            run_token=run_token,
            root=tmp / name,
            node_url=node_url,
            # The nest's identity is read on its real port; the trust seed is
            # keyed by the URL the seat launches against (`_apply_r14_trust_env`).
            node_port=port,
            folder=folder,
            sign_in=harness_sign_in(
                node_url, secret_key, actor_id, handle,
                spa_url=spa_url if modes[name] == sync_seats.WEB_MODE else None,
            ),
            request=request,
        )

    try:
        with contextlib.ExitStack() as stack:
            stack.callback(link.close)
            a = stack.enter_context(seat("a", url))
            b = stack.enter_context(seat("b", link.url))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            da, db = a.app.driver, b.app.driver

            _w, b_row, _ = enrollment.await_enrollment(b.app, url, user)
            before = enrollment.principals(url, user)
            b_principal = before.get(b_row)
            b_session_device = db.get_state("session.device_id")
            last: dict = {}

            def a_lists_one_sibling(extra=lambda: True):
                def check() -> bool:
                    a.app.backups.navigate_devices()
                    last["cards"] = _sibling_cards(da)
                    last["roster"] = enrollment.principals(url, user)
                    return len(last["cards"]) == 1 and extra()
                return check

            def diagnose(what: str):
                return lambda: (
                    f"{what}: seat a's Devices page lists siblings {last.get('cards')!r}; "
                    f"nest roster (device id → principal): {last.get('roster')!r}; "
                    f"seat b's first row {b_row} (principal {b_principal}); "
                    f"seat b's latched row {last.get('latched')!r}; {link.describe()}\n"
                    f"--- seat b ---\n{b.diagnostics()}\n--- seat a ---\n{a.diagnostics()}"
                )

            wait_until(a_lists_one_sibling(lambda: b_row in last["roster"]),
                       _ROSTER_WINDOW, interval=2.0, diagnose=diagnose("before the cut"))
            # The words page reads loaded-and-empty on a fresh account: the
            # word below is b's, not a leftover.
            a.app.muted_words.navigate()
            assert a.app.muted_words.wait_for_empty_state(), (
                f"seat a's Muted words page never read empty on a fresh account: "
                f"{da.diagnose('muted-word-item')}"
            )

            # ── b is cut off from the nest ──
            link.cut()
            wait_until(
                lambda: not _connection_status(db).startswith("Connected"),
                _OFFLINE_WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"seat b still reads {_connection_status(db)!r} after its link was "
                    f"cut — {link.describe()}\n{b.diagnostics()}"
                ),
            )

            # ── a removes b through its Devices page ──
            a.app.backups.navigate_devices()
            cards = _sibling_cards(da)
            assert len(cards) == 1, diagnose("removing b")()
            a.app.backups.remove_device(cards[0][0])

            def b_removed() -> bool:
                last["roster"] = enrollment.principals(url, user)
                return b_row not in last["roster"]

            wait_until(b_removed, _ROSTER_WINDOW, interval=1.0,
                       diagnose=diagnose("after a removed b (the nest still lists b's row)"))

            # ── b mutes a word, offline: it lands in b's store only ──
            b.app.muted_words.navigate()
            b.app.muted_words.add(word)
            wait_until(
                lambda: word in b.app.muted_words.words(),
                60.0, interval=1.0,
                diagnose=lambda: (
                    f"seat b's Muted words page never listed {word!r} it added while cut "
                    f"off: {db.diagnose('error-message')} — {link.describe()}"
                ),
            )
            assert link.is_cut and link.live_connections() == 0, link.describe()

            # ── the link comes back: b returns as a successor ──
            link.restore()

            def b_is_a_successor() -> bool:
                last["roster"] = enrollment.principals(url, user)
                try:
                    last["latched"] = _latched_row(b.app, user)
                except Exception as exc:
                    last["latched"] = f"unreadable ({exc!r})"
                principal = last["roster"].get(b_row)
                return (
                    last["latched"] == b_row
                    and bool(principal) and principal != b_principal
                )

            wait_until(b_is_a_successor, _SUCCESSION_WINDOW, interval=2.0,
                       diagnose=diagnose("after the restore (no successor enrolled)"))
            successor = last["roster"][b_row]
            assert b_principal not in last["roster"].values(), diagnose(
                "the removed principal must hold no grant on any row"
            )()

            wait_until(a_lists_one_sibling(lambda: last["roster"].get(b_row) == successor),
                       _ROSTER_WINDOW, interval=2.0,
                       diagnose=diagnose("after the succession (one row for b, not two)"))
            assert len(last["roster"]) == len(before), diagnose(
                f"the roster should hold as many rows as before the removal ({len(before)})"
            )()

            # ── the change b made offline is kept, read on a's own page ──
            def a_reads_the_word() -> bool:
                a.app.muted_words.navigate()
                last["a_words"] = a.app.muted_words.words()
                return word in last["a_words"]

            wait_until(
                a_reads_the_word, _TAIL_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat a's Muted words page lists {last.get('a_words')!r}, never "
                    f"{word!r} — the word b muted while cut off was lost, not "
                    f"re-authored under its successor\n" + diagnose("the tail")()
                ),
            )
            print(
                f"[succession] b: row {b_row} removed and re-created, principal "
                f"{b_principal} -> {successor}, session device "
                f"{b_session_device} -> {db.get_state('session.device_id')}; "
                f"{link.describe()}",
                flush=True,
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

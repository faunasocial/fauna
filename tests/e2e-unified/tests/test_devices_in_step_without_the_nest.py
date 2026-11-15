"""Your devices stay in step without your nest — the account's settings and
read state pass straight between two of your devices while the nest is down.

Owner: ``docs/goal/architecture/account-sync-plane.md`` § The peer leg —
device↔device sync over iroh (requirement 7: an outage is not a special mode;
LWW and commutative class-2 state converge peer-wise without the nest; the
replica is its own peer registry) and § The bind leg, ruling 1 (a row a sibling
wrote reaches the nest through whichever replica holds it first);
``docs/goal/behavior/conversation-read-state.md`` § The read-marker record. The
catalog page is ``docs/features/devices-in-step-without-the-nest.md``.

**What this pins that the tier below cannot.** The mechanism is proven one tier
down with two runtimes handed to each other and no nest at all
(``fauna-sync-engine/tests/peer_dial_convergence.rs``,
``fauna-client-account-runtime/tests/read_positions_convergence.rs``). What none
of those answers is whether two real seats — each a real app beside its own
sync agent, signed in to one account — find each other with nobody pairing
them, carry a setting the user changed in one app's UI to the other app's
screen, and carry a thread read in one app to the other's unread count, while
the nest they both bind is stopped.

**Nothing is set up first (outcome 3).** Between signing in and the nest
going down, neither test visits a page or flips a switch beyond what every
seat's start does (sign in, bind its folder); the sibling is admitted on its
own. Each test then reads its seat's own Devices card and asserts the
participation switch still rests at its default — the evidence that the path
the rest of the test takes is the default one, not one somebody opened.

**Which seats.** ``sync_seats.DEFAULT_PAIRS`` through the one ``make_seat``
seam, exactly as the device-to-device file journey runs them. The sibling
observable is the receiving agent's ``peer_files`` reading (its peer leg holds
an admitted sibling); the agent holds the machine's engine role beside a
running app (``account-runtime.md`` § Multi-instance concurrency), so the same
leg carries the account plane. A column whose app host passes no peer
transport reads short until it dials — the app is short, not the test. The
phone's column runs the ``tui+phone`` pair in functions of its own: the phone
runs no agent, its app's in-process runtime holds the leg (``fauna-ffi``'s
account runtime), so the sibling precondition is read from tui's agent and
every outcome from the phone's own screen.

Two seats on one machine, no operator (convention 16); the nest is stopped
and returned through the harness's own in-place restart, never a kill. Every
wait is a deadline poll on state under a named ceiling (convention 14).
"""

import contextlib
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register, user_create_folder
from common.nest import start_nest_in_place, stop_nest
from helpers import sync_seats
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.diagnostics import account_plane_log_lines
from helpers.waiting import (
    await_account_runtime_assembled,
    wait_rail_blob_settled,
    wait_until,
)
from i18n.strings import S
from tests.api import conv_api
from tests.test_conversations_read_state_across_restart import _read_marker, _thread
from tests.test_device_direct_transfer_switch import _roster, _toggle
from tests.test_device_to_device_file_transfer import (
    RETURN_WINDOW,
    SIBLING_WINDOW,
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _await_siblings,
    _seat_factory,
    seat_pairs,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (the one app host that dials the peer leg in-process,
    # `account-runtime.md` § Implementation status today); the native pairs
    # run through the same seat seam.
    pytest.mark.tui,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.macos,
]

_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in seat_pairs(sys.platform)
]
# The phone's leg: seat `b` is the phone (`sync_seats.PHONE_APPS`), seat `a`
# (and the return leg's `c`) tui. A phone runs no agent: its app hosts the
# account runtime in-process and dials the peer leg from there (`fauna-ffi`'s
# account runtime hands it the shared `fauna_iroh::peer_leg_factory`), so the
# outcomes are read on its screen exactly as on a desktop seat's, and only the
# sibling precondition reads from tui's agent alone. Functions of their own so
# the catalog maps the phone's column to the phone's run, as
# `test_device_direct_transfer_switch.py` does. android carries the mark beside
# ios: both phones reach the leg through the same `fauna-ffi` entry, and
# android's foreground seam (`AccountRuntimeForeground`) makes the same
# start/stop calls iOS makes at its own.
_PHONE_PAIR = ("tui", sync_seats.PHONE_MODE)
_PHONE_PAIRS = (
    [pytest.param(_PHONE_PAIR, id=sync_seats.seat_set_param_id(_PHONE_PAIR))]
    if sync_seats.phone_app_for_platform(sys.platform)
    else []
)

#: A row crossing the peer leg with the nest down. The leg is pull-only: the
#: receiving runtime dials and walks on a pass of its own, and with the nest
#: gone nothing wakes that pass but the five-minute backstop. That pass then
#: spends each of its nest calls' timeouts before its peer leg runs (measured
#: 2026-11-07: 15–20 s, the dial landing 20 s after the pass began), so the
#: ceiling is one sibling window plus one nest-down pass.
PEER_WINDOW = SIBLING_WINDOW + 90.0

#: The nest's class-2 feed for the account's delegable state — preferences and
#: read markers both rest here, sealed (`account-sync-plane.md`).
_STATE_FEED = "state-entry"
_STATE_SCOPE = "state"


def _seats(nest_instance, request, account, folder, tmp, seat_pair):
    """The seat factory for seats ``a``, ``b`` and — for the return leg — a
    third seat ``c`` of the same mode as ``a``."""
    run_token = sync_seats.new_run_token(seats=3)
    return _seat_factory(
        nest_instance, request, account, folder, tmp, run_token,
        tuple(seat_pair) + (seat_pair[0],),
    )


def _await_in_reach(a, b, what: str) -> None:
    """Both seats hold an admitted channel to each other, read from each seat
    whose agent gives a plane reading. A phone seat gives none
    (``carries_peer_leg`` False: no agent beside it), so a pair with a phone is
    read from its desktop seat alone — whose only sibling is the phone."""
    for seat, other in ((b, a), (a, b)):
        if seat.carries_peer_leg:
            _await_siblings(seat, other, what)


def _assert_nothing_switched_on(seat) -> None:
    """Outcome 3's evidence: the seat's own direct-transfer switch rests at its
    default — on, under its own-device label — so the path the test took is
    the one a user who set nothing up gets."""
    checked, label = _toggle(seat)
    assert checked is True and label == S.devices.p2p_participation_own, (
        f"seat {seat.name}'s own participation switch reads checked={checked!r} "
        f"label={label!r}, not its default — something switched the direct path "
        f"before the test, so it would not witness 'nothing is set up first'; "
        f"roster (name, status, this?, checked, label): {_roster(seat)!r}"
    )


def _plane_note(seat) -> str:
    """The seat's account-plane story at failure time: its app's runtime role,
    pass counters and account-runtime lines, then its agent's peer-leg and pass
    lines (the targets ``sync_seats.PEER_LEG_LOG_TARGETS`` turns up)."""
    try:
        agent = [
            ln for ln in seat.agent_log_lines()
            if "peer_leg" in ln or "account_driver::pass" in ln
        ][-30:]
    except Exception as exc:  # a diagnosis never masks the failure it explains
        agent = [f"<unreadable: {exc!r}>"]
    return (
        f"  app account plane: {account_plane_log_lines(seat.app)}\n"
        f"  agent peer-leg/pass lines:\n    "
        + ("\n    ".join(agent) or "<none>")
    )


def _state_entries(account) -> set:
    """Every sealed row the nest holds in the account's delegable state feed,
    as its entry bytes — a row's coordinates and seal are its identity, so a
    row the nest did not hold before is a new member of this set."""
    _port, url, secret_key, actor_id, _handle = account
    with WsRpcAdminClient(url, bytes.fromhex(actor_id), bytes.fromhex(secret_key)) as client:
        reply = client.call(
            "fauna.sync.changes.list",
            {"item_class": _STATE_FEED, "scope": _STATE_SCOPE, "frontier": {}},
        )
    return {bytes(r["entry"]) for r in reply.get("changes") or [] if r.get("entry")}


def _await_word(seat, word: str, what: str, *others, window: float = WINDOW) -> None:
    """Poll seat's Muted words page, re-entered each time (`revisit`: an app
    that reads the list once per visit shows another device's edit at the next
    one), until ``word`` is listed."""
    last: dict = {}

    def listed() -> bool:
        last["words"] = seat.app.muted_words.revisit()
        return word in last["words"]

    wait_until(
        listed, window, interval=2.0,
        diagnose=lambda: (
            f"{what}: seat {seat.name}'s Muted words never listed {word!r} "
            f"(last {last.get('words')!r}, error {seat.app.error_text()!r}; its "
            f"plane reading {seat.peer_files()!r})\n"
            f"--- seat {seat.name} ---\n{_plane_note(seat)}\n{seat.diagnostics()}\n"
            + "".join(
                f"--- seat {o.name} ---\n{_plane_note(o)}\n{o.diagnostics()}\n"
                for o in others
            )
        ),
    )


_SETTING_TIMEOUT = int(
    3 * STARTUP_WINDOW + SIBLING_WINDOW + PEER_WINDOW + 3 * WINDOW + RETURN_WINDOW + 180
)


@pytest.mark.timeout(_SETTING_TIMEOUT)
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("devices-in-step-without-the-nest")
def test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down(
    nest_instance, request, seat_pair
):
    """Outcomes 1, 3 and 4. Two seats of one account find each other with
    nothing set up; the nest goes down; a muted word added on seat ``a``
    through its Settings page is listed on seat ``b`` with the nest still down.
    Then ``a`` goes away and the nest returns: ``b`` — the one device that
    connects — carries ``a``'s row to the nest, and a third seat ``c`` that
    starts with ``a`` and ``b`` both gone, so the nest is its only source,
    lists the word."""
    _setting_journey(nest_instance, request, seat_pair)


@pytest.mark.timeout(_SETTING_TIMEOUT)
@pytest.mark.parametrize("seat_pair", _PHONE_PAIRS)
@pytest.mark.feature("devices-in-step-without-the-nest")
@pytest.mark.ios
@pytest.mark.android
def test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down(
    nest_instance, request, seat_pair
):
    """Outcomes 1, 3 and 4 with the phone as seat ``b``: the word added on tui
    is listed on the phone with the nest down — the phone's own runtime dialed
    tui and pulled it — and once tui is gone and the nest is back, the phone is
    the device that carries tui's row to the nest."""
    _setting_journey(nest_instance, request, seat_pair)


def _setting_journey(nest_instance, request, seat_pair):
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"instep-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    word = f"instep{secrets.token_hex(3)}"
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seats(nest_instance, request, account, folder, tmp, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            # No gesture between the seats' start and here (outcome 3).
            _await_in_reach(a, b, "before the nest went down")
            for s in (a, b):
                _assert_nothing_switched_on(s)
            # The word is not there yet — the listing below is news.
            assert word not in b.app.muted_words.revisit()
            held_before = _state_entries(account)

            stop_nest(nest_instance, graceful=True)
            nest_up = False
            try:
                # Outcome 1: the user's own gesture on a, read on b's screen.
                a.app.muted_words.navigate()
                a.app.muted_words.add(word)
                assert a.app.muted_words.wait_for_word(word, timeout=WINDOW), (
                    f"seat a's own add of {word!r} never listed with the nest down "
                    f"(words {a.app.muted_words.words()!r}, error "
                    f"{a.app.error_text()!r}) — a preference write must not need "
                    f"the nest\n{a.diagnostics()}"
                )
                _await_word(b, word, "the nest-down preference", a, window=PEER_WINDOW)

                # Outcome 4's set-up: the writer leaves while the nest is
                # still down, so only b can carry a's row back to it.
                a.stop_keeping_state()
            finally:
                # The session-scoped nest must outlive this test: same port,
                # same data dir, no factory reset.
                start_nest_in_place(nest_instance)
                nest_up = True
            assert nest_up

            # Outcome 4: b's pass after the return pushes the sibling row the
            # nest's listing lacks (`account-sync-plane.md` § The bind leg,
            # ruling 1). With a stopped, a row the nest did not hold before
            # the outage can only have come from b.
            last: dict = {}

            def pushed() -> bool:
                try:
                    last["new"] = _state_entries(account) - held_before
                except Exception as exc:  # the nest still coming up
                    last["new"] = f"unreadable ({exc!r})"
                    return False
                return bool(last["new"])

            wait_until(
                pushed, RETURN_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"the nest came back, but its delegable state feed gained no row "
                    f"({last.get('new')!r}) — seat b holds seat a's preference row "
                    f"and never pushed it on its catch-up\n"
                    f"--- seat b ---\n{b.diagnostics()}"
                ),
            )
            b.stop_keeping_state()

            # Nothing but the nest can serve c now: a and b are both gone.
            c = stack.enter_context(seat("c"))
            c.await_ready(STARTUP_WINDOW)
            _await_word(c, word, "the preference that reached the nest from seat b")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _seed_thread(a, account, nest_instance) -> str:
    """An API-tier sender seats the account in a 1:1 and posts one message,
    which seat ``a`` decrypts; ``a``'s MLS replica reaches the nest so a seat
    started later restores the channel. Returns the channel's hex id."""
    port, url, secret_key, actor_id, _handle = account
    me = {
        "actor_id_hex": actor_id,
        "actor_id_bytes": bytes.fromhex(actor_id),
        "signing_key": bytes.fromhex(secret_key),
    }
    a.app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(a.app.driver)
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, me, actor_id) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for seat a to publish a key package",
    )
    kp = conv_api.keypackage_fetch(port, sender, actor_id)
    assert kp is not None, "the account should have a fetchable key package"
    channel_hex, welcome, (opener,) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), kp, ["in step without the nest: read me"]
    )
    conv_api.accept_contact(port, me, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, actor_id, channel_hex, welcome)
    conv_api.await_welcome_applied(port, me, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, opener)
    wait_until(
        lambda: (_thread(a.app, channel_hex) is not None)
        and _thread(a.app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for seat a to decrypt the sender's message",
    )

    def client():
        return WsRpcAdminClient(
            url, actor_id=me["actor_id_bytes"], signing_key=me["signing_key"]
        )

    wait_rail_blob_settled(client, "fauna.mls.get", "provider")
    wait_rail_blob_settled(client, "fauna.mls.get", f"history/{channel_hex}")
    return channel_hex


_READ_TIMEOUT = int(
    2 * STARTUP_WINDOW + SIBLING_WINDOW + PEER_WINDOW + 4 * MLS_HANDSHAKE_S + 2 * WINDOW + 180
)


@pytest.mark.timeout(_READ_TIMEOUT)
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.real_conversations
@pytest.mark.feature("devices-in-step-without-the-nest")
def test_a_conversation_read_on_one_device_shows_read_on_the_other_while_the_nest_is_down(
    nest_instance, request, seat_pair
):
    """Outcomes 2 and 3. A conversation seeded while the nest is up shows
    unread on seat ``b``; the nest goes down; seat ``a`` opens the thread; ``b``'s
    unread count falls to zero with nothing done on ``b`` and the nest still
    down — the read crossed as the channel's read marker over the peer leg."""
    _read_journey(nest_instance, request, seat_pair)


@pytest.mark.timeout(_READ_TIMEOUT)
@pytest.mark.parametrize("seat_pair", _PHONE_PAIRS)
@pytest.mark.real_conversations
@pytest.mark.feature("devices-in-step-without-the-nest")
@pytest.mark.ios
@pytest.mark.android
def test_a_conversation_read_on_tui_shows_read_on_the_phone_while_the_nest_is_down(
    nest_instance, request, seat_pair
):
    """Outcomes 2 and 3 with the phone as seat ``b``: the thread tui opens with
    the nest down reads as read on the phone, its read marker pulled over the
    leg the phone's own runtime dialed."""
    _read_journey(nest_instance, request, seat_pair)


def _read_journey(nest_instance, request, seat_pair):
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"inread-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seats(nest_instance, request, account, folder, tmp, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            a.await_ready(STARTUP_WINDOW)
            # Seeded before b exists: the welcome is minted against a's key
            # package, and b restores the channel from a's replica on login.
            channel_hex = _seed_thread(a, account, nest_instance)

            b = stack.enter_context(seat("b"))
            b.await_ready(STARTUP_WINDOW)
            b.app.conversations.enable_real_faunamls()
            await_account_runtime_assembled(b.app.driver)
            wait_until(
                lambda: _thread(b.app, channel_hex) is not None,
                MLS_HANDSHAKE_S,
                diagnose=lambda: "seat b should restore the 1:1 from the replica on "
                f"login; its threads were "
                f"{[(t.rail, t.channel_id_hex) for t in b.app.conversations.list_threads()]}",
            )
            # The precondition that keeps the witness from being vacuous.
            wait_until(
                lambda: (_thread(b.app, channel_hex).unread_count or 0) >= 1,
                MLS_HANDSHAKE_S,
                diagnose=lambda: "seat b should show the sender's message unread "
                f"before seat a reads it; it reads "
                f"{_thread(b.app, channel_hex).unread_count!r} (marker on b: "
                f"{_read_marker(b.app, channel_hex)!r})",
            )
            # No gesture on either seat between their start and here beyond
            # opening the conversations rail (outcome 3).
            _await_in_reach(a, b, "before the nest went down")
            for s in (a, b):
                _assert_nothing_switched_on(s)

            stop_nest(nest_instance, graceful=True)
            try:
                a.app.conversations.navigate()
                a.app.conversations.open_thread_by_channel(channel_hex)
                wait_until(
                    lambda: _thread(a.app, channel_hex).unread_count == 0,
                    WINDOW,
                    diagnose=lambda: "opening the thread on seat a should read the "
                    "message, nest or no nest",
                )
                wait_until(
                    lambda: (_read_marker(a.app, channel_hex) or {}).get("found") is True,
                    WINDOW,
                    diagnose=lambda: "the read never raised the channel's marker in "
                    f"seat a's account store: {_read_marker(a.app, channel_hex)!r}",
                )
                wait_until(
                    lambda: _thread(b.app, channel_hex).unread_count == 0,
                    PEER_WINDOW, interval=1.0,
                    diagnose=lambda: (
                        f"seat a read the conversation with the nest down, but seat b "
                        f"still counts {_thread(b.app, channel_hex).unread_count!r} "
                        f"unread (marker on a: {_read_marker(a.app, channel_hex)!r}; on "
                        f"b: {_read_marker(b.app, channel_hex)!r}; b's plane reading "
                        f"{b.peer_files()!r}) — the read marker never crossed the peer "
                        f"leg, or b's read-position seam never delivered it\n"
                        f"--- seat b ---\n{_plane_note(b)}\n{b.diagnostics()}\n"
                        f"--- seat a ---\n{_plane_note(a)}\n{a.diagnostics()}"
                    ),
                )
            finally:
                start_nest_in_place(nest_instance)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

"""Tier_3 — a community room still waiting for its key says so, and never stalls your other conversations.

``community-rooms.md`` § The three classes (the ``awaiting-key`` entry of
§ Implementation status today): a newcomer who has accepted an invitation is
seated on the room's floor before any member's device has wrapped them a key,
so their room cannot open its messages yet. The room says so in its header —
``thread-room-notice`` with ``state="awaiting-key"`` (``ui/conversations.md``
§ Element IDs; the shared ``RoomSnapshot::notice`` picks it) — and the wait is
the room's alone: the rest of the account goes on.

The owner doc records why no journey reached this before: the founder's
device keys the newcomer in with no gesture, so the window is a race, not a
state. **This journey holds it**: the nest parks the founder's key-in
(``fauna.conversations.room.backfill_generations``, the one door a key-in goes
through — ``key_in_room_member``) with the test hook's hold, so the newcomer
stays unkeyed for as long as the test needs, and releasing it lets the key-in
land.

**Three seats, one nest** (``helpers.room_seats.room_seats``): the
``founder``, the ``member`` who joins, and an ``other`` account the member
talks to one-to-one. While the member's room waits, the other's message
reaches the member and the member's reply reaches the other — the unkeyed
room stalls nothing. Released, the key-in lands and the notice goes.

**From the accept, and a send says why.** The room reads as waiting at its
first sight, seeded by the accept rather than by the walk's first stop at a
record it cannot open, and a send inside the window is refused with
``error.send.room_awaiting_key`` in ``conversations.unified.error_send``,
never the generic failure.

**Latency-independent** (convention 14): the window is a held RPC the nest
counts, every wait a deadline poll on projected state.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    room_app,
    room_seats,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads; the six others join through the batched trickle-down.
    pytest.mark.tui,
    # web, linux and android: the community-room surfaces are built on all three;
    # android's device run is its venue's, the mark is the coverage contract.
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    # macos and ios: FaunaKit paints every control these journeys drive and
    # forwards each to the shared manager, so the surfaces are theirs too.
    pytest.mark.macos,
    pytest.mark.ios,
]

KEY_IN_KIND = "fauna.conversations.room.backfill_generations"


def _founded(seat):
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
            return t
    return None


def _notice(seat) -> str | None:
    d = seat.app.driver
    if d.count("thread-room-notice") == 0:
        return None
    return d.get_attr("thread-room-notice", "state")


def _thread_with(seat, needle: str):
    return next(
        (t for t in seat.app.conversations.list_threads() if needle in (t.snippet or "")),
        None,
    )


@pytest.mark.feature("group-conversations")
def test_a_community_room_waiting_for_its_key_says_so_and_stalls_nothing_else(
    room_seats, nest_instance
):
    from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold, wait_for_held_rpc

    founder, member, other = room_seats
    port = nest_instance["port"]
    conv_api.accept_contact(port, member.actor, other.actor_hex)
    word = secrets.token_hex(4)

    arm_rpc_hold(port, KEY_IN_KIND)
    try:
        # ── founding, every step a gesture ───────────────────────────────
        conv = founder.app.conversations
        conv.navigate()
        founder.app.driver.click("new-conversation-button")
        conv.add_recipient(member.actor_hex)
        conv.toggle_home_nest()
        assert conv.prospective_room_class() == "community", (
            seat_element(founder, "recipient-picker-class")
        )
        founder.app.driver.type_text("dm-text-field", f"hello {word}")
        founder.app.driver.click("dm-send-button")
        room = wait_for(
            "the founder's community room binding to its room id",
            lambda: _founded(founder),
            lambda t: t is not None,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'found', 'room')}",
        )
        room_id = room.channel_id_hex

        # ── the member accepts; the key-in is parked at the nest ─────────
        mconv = member.app.conversations
        wait_for(
            "the founder's invitation standing on the member's list",
            lambda: mconv.room_invitation_count(),
            lambda n: n >= 1,
            WELCOME_BUDGET_S,
            diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
        )
        mconv.accept_room_invitation(0)
        waiting = wait_for(
            "the accepted room opening on the member's seat",
            lambda: thread_by_channel(member.app, room_id),
            lambda t: t is not None,
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: seat_story(member),
        )
        # The very first sight of the room already says it waits: the accept
        # seeds the wait, rather than the walk's first stop at a sealed record
        # it cannot open.
        assert (waiting.room or {}).get("awaiting_key") is True, (
            f"the room must read as waiting from the accept: {waiting.room!r}\n{seat_story(member)}"
        )
        wait_for_held_rpc(port, KEY_IN_KIND)
        assert (thread_by_channel(member.app, room_id).room or {}).get("awaiting_key") is True, (
            "with the founder's key-in parked the member is still unkeyed"
        )
        mconv.open_thread_by_id(waiting.thread_id)
        wait_for(
            "the member's room header saying it waits for its key",
            lambda: _notice(member),
            lambda state: state == "awaiting-key",
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: f"{seat_element(member, 'thread-room-notice')}\n{seat_story(member)}",
        )
        assert not member.app.has_error(), (
            f"waiting for a key is a notice, never an error: {member.app.error_text()!r}"
        )

        # ── a send in the window says why, never the generic failure ─────
        member.app.driver.type_text("dm-text-field", f"too early {word}")
        member.app.driver.click("dm-send-button")
        expected = S.conversations.unified.error_send(message=S.error.send.room_awaiting_key)
        refusal = wait_for(
            "the member's early send refused by name",
            lambda: member.app.error_text() if member.app.has_error() else None,
            lambda text: text is not None,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'send', 'room')}",
        )
        assert refusal == expected, (
            f"a send before the key-in must say why ({expected!r}), got {refusal!r}"
        )

        # ── the rest of the account goes on ──────────────────────────────
        hello = f"are you there {word}"
        other.app.conversations.real_resolve_send_new(member.actor_hex, hello)
        one = wait_for(
            "the other account's message reaching the member while the room waits",
            lambda: _thread_with(member, hello),
            lambda t: t is not None,
            MESSAGE_BUDGET_S,
            diagnose=lambda: seat_story(member),
        )
        reply = f"yes I am {word}"
        mconv.real_send(one.thread_id, reply)
        wait_for(
            "the member's reply reaching the other account while the room waits",
            lambda: _thread_with(other, reply),
            lambda t: t is not None,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"{seat_story(other)}\n{seat_story(member)}",
        )
        assert (thread_by_channel(member.app, room_id).room or {}).get("awaiting_key") is True, (
            "the exchange ran inside the window: the room is still unkeyed"
        )
    finally:
        release_rpc_hold(port, KEY_IN_KIND)

    # ── released, the key-in lands and the notice goes ───────────────────
    wait_for(
        "the member's seat being keyed in once the hold is released",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(founder, 'key-in', 'key_in', 'backfill')}",
    )
    # Keyed in, the room takes the send it refused: the refusal was the early
    # send's own terminal state, and a send that goes through replaces it.
    mconv.open_thread_by_id(waiting.thread_id)
    mconv.real_send(waiting.thread_id, f"keyed in now {word}")
    wait_for(
        "the member's send going through once keyed in, clearing the early refusal",
        lambda: member.app.error_text() if member.app.has_error() else None,
        lambda text: text is None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'send', 'room')}",
    )

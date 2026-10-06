"""Tier_3 — a community room joined from ANOTHER nest: two seats, two nests.

``conversation-rooms.md`` § Done definition's community box closes on one
clause this journey witnesses and ``test_conversation_room_community.py``
(both seats on one nest) cannot: "sealed log opened by a second member **on a
second nest** through the relay". The ruling it drives is § Join rules and
invites → *A cross-nest invitation* — the room's home judges the invitation
and pushes the knock to the invitee's own nest; that nest runs the invitee's
reach policy and relays the acceptance back; the home seats and binds; the
founder's device keys the foreign seat in exactly as a same-nest one; and the
newcomer reads and writes the room through its own nest's relay.

**Topology — two real nest binaries, each with a handle domain and floor
TLS**, because each has to dial the other: the room's home (``home``,
``cross_nest_foreign``) delivers the knock to the member's nest and declares
the address the acceptance is relayed back to, and the member's nest
(``away``, ``cross_nest_foreign_ephemeral``) relays that acceptance and every
later read of the room. The founder is a fresh account on ``home``; the member
is ``away``'s pre-provisioned handled actor, so the founder can address them
as ``handle@authority`` in the recipient picker.

**What the journey proves, in order:** the composer takes a foreign handle
and the home-nest choice makes the room a community room; the first send
founds it and invites across the nest boundary; the invitation stands atop
the member's list on the other nest and accepting it (through their own
nest) opens the room; the founder's device keys the foreign seat with no
gesture; a message crosses each way, sealed under the room's generation,
through the member's relay.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state — never "sleep N then assert".
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    Seat,
    launch_seat,
    room_app,
    seat_app_path,
    seat_element,
    seat_log,
    seat_story,
    teardown_seats,
    thread_by_channel,
    wait_for,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS (`community-rooms.md` — the lead app paints every new room
    # gesture first). The six others join through the batched trickle-down,
    # each arriving with its own marker AND a `helpers.room_seats.ROOM_APPS`
    # entry.
    pytest.mark.tui,
]

CROSSING_BUDGET_S = WELCOME_BUDGET_S
"""For a knock to cross nest → nest and stand on the member's list, and for
an acceptance to cross back and the member's seat to be keyed through the
relay — the Welcome budget, the same order of cross-nest round trip."""


@pytest.fixture()
def cross_nest_room_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(founder, member)`` on two nests: the founder a fresh account on
    ``cross_nest_foreign`` (the room's home), the member the handled actor
    ``cross_nest_foreign_ephemeral`` provisioned (their own home). The member
    accepts the founder as a contact on *their* nest, exactly as the same-nest
    pair does — the invitation is judged against that edge where the member
    lives."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        founder = launch_seat(home, app_path, room_app, request)
        seats.append(founder)
        member = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(member)
        conv_api.accept_contact(away["port"], member.actor, founder.actor_hex, scheme="https")
        yield founder, member
    finally:
        teardown_seats(seats)


def _founded_room(seat):
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
            return t
    return None


@pytest.mark.feature("group-conversations")
def test_a_member_of_another_nest_joins_a_community_room_through_the_relay(
    cross_nest_room_seats, cross_nest_foreign_ephemeral
):
    """Found on one nest with a foreign handle in the picker; the knock crosses;
    accepting through the member's own nest seats them; the founder keys the
    foreign seat; a sealed message crosses each way through the relay."""
    founder, member = cross_nest_room_seats
    away = cross_nest_foreign_ephemeral
    member_address = f"{member.handle}@{away['authority']}"
    first_word = f"founding{secrets.token_hex(4)}"
    reply_word = f"answer{secrets.token_hex(4)}"
    later_word = f"welcome{secrets.token_hex(4)}"

    # ── founding: a foreign chip plus the home-nest choice ───────────────
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(member_address)
    conv.toggle_home_nest()
    assert conv.home_nest_included(), seat_element(founder, "recipient-picker-home-nest-toggle")
    assert conv.prospective_room_class() == "community", (
        f"the class statement follows the choice: {seat_element(founder, 'recipient-picker-class')}"
    )
    founder.app.driver.type_text("dm-text-field", f"hello {first_word}")
    founder.app.driver.click("dm-send-button")

    room = wait_for(
        "the founder's community room binding to its room id",
        lambda: _founded_room(founder),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'found', 'room', 'invite')}",
    )
    room_id = room.channel_id_hex
    assert not founder.app.has_error(), founder.app.error_text()

    # ── the knock crosses: it stands atop the member's list on THEIR nest ─
    wait_for(
        "the founder's invitation crossing to the member's nest",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}\n"
            f"{seat_log(founder, 'invite', 'federation')}"
        ),
    )
    member.app.conversations.accept_room_invitation(0)
    joined = wait_for(
        "the accepted room opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'accept', 'room', 'relay')}",
    )
    assert not member.app.has_error(), (
        f"accepting through the member's own nest must not refuse: "
        f"{member.app.error_text()!r}\n{seat_story(member)}"
    )
    assert member.app.conversations.room_invitation_count() == 0, (
        "an accepted invitation stops standing"
    )

    # ── the key-in crosses too: the founder's device keys the foreign seat ─
    wait_for(
        "the foreign seat being keyed in by the founder's device",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'key-in', 'key_in', 'tend', 'mint')}\n"
            f"{seat_log(member, 'generation', 'remote', 'relay')}"
        ),
    )

    # ── a sealed message each way, through the member's relay ────────────
    conv.open_thread_by_id(room.thread_id)
    conv.send_in_open_thread(f"glad you came {later_word}")
    wait_for(
        "the founder's message opening on the member's seat across the relay",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and later_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'room', 'fetch')}",
    )
    member.app.conversations.open_thread_by_id(joined.thread_id)
    member.app.conversations.send_in_open_thread(f"thanks {reply_word}")
    wait_for(
        "the member's reply opening on the founder's seat",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and reply_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_story(member)}",
    )
    assert (thread_by_channel(member.app, room_id).room or {}).get("class") == "Community", (
        "the newcomer's projection states the class it joined"
    )

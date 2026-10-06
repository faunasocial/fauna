"""Tier_3 — a community room, founded, joined and searched from the app: two seats on one nest.

``community-rooms.md`` § The three classes: a community room is the class
whose home nest holds a read, which is what lets it search and label the
room for its members. Nest-side the class has been complete since
2026-09-09, and the shared manager has founded, invited, joined, keyed and
rotated it since 2026-09-10 — but no app could reach any of it: the room had
no founding control, no invitation list and no read control
(§ Implementation status today, *Still dark*). This journey is those three
controls, driven the way a user drives them.

**Two principals, two seats** — the ``founder`` and a ``member``. No witness
is needed: everything the story asserts about the room's third party, the
home nest, is read off the nest itself through the member's own
``room.search``.

**What the journey proves, in order:** the composer's home-nest choice turns
the room about to be created into a community room, and says so before the
first message; the first send FOUNDS it (the room ceremony, never an MLS
bootstrap), and its header states the class; the invitation reaches the
member as a row atop their list; the founder's room settings list it as
pending and one press withdraws it — the member's row stops standing, with
nobody told, and inviting again is the undo; accepting it opens the room; the
founder's device keys the member in with no gesture of its own; a message
crosses both ways, sealed; the home nest's search index finds it, answered
to a member; and withdrawing the nest's read — one toggle and Save in the
founder's room settings — empties that index, which is the members'
standing choice working rather than an error.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state (the room's ``awaiting_key`` and
``nest_read``, a snippet, an invitation row) or over the nest's own search
answer — never "sleep N then assert".
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    room_app,
    room_seat_pair,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS this one (`community-rooms.md` — the lead app paints every new
    # room gesture first). The six others join through the batched
    # trickle-down, each arriving with its own marker AND a
    # `helpers.room_seats.ROOM_APPS` entry.
    pytest.mark.tui,
]

SEARCH_BUDGET_S = MESSAGE_BUDGET_S
"""For the home nest to fold a sealed message into its index (at reception)
and, after a rotation withdrew its read, to delete what it built."""


def _search(port: int, reader: dict, room: str, needle: str) -> list | None:
    """``reader``'s room search for ``needle``, or ``None`` on a refusal — a
    refusal is worth polling through: the member's floor seat and the room's
    community standing are both the nest's to settle."""
    try:
        return conv_api.room_search(port, reader, room, needle)
    except Exception:
        return None


def _founded_room(seat, label_hint: str):
    """The founder's community room: the FaunaMls thread whose room says
    ``Community`` and which has bound to a room id."""
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
            return t
    return None


@pytest.mark.feature("group-conversations")
def test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn(
    room_seat_pair, nest_instance
):
    """Found with the home-nest toggle, accept from the list, exchange, the
    nest's index finds the text, and withdrawing the nest's read empties it."""
    founder, member = room_seat_pair
    port = nest_instance["port"]
    # One unguessable word per message, so a hit can only be this run's.
    first_word = f"founding{secrets.token_hex(4)}"
    reply_word = f"answer{secrets.token_hex(4)}"
    later_word = f"welcome{secrets.token_hex(4)}"

    # ── founding: the composer's home-nest choice, one gesture each ──────
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(member.actor_hex)
    assert conv.prospective_room_class() == "end-to-end", (
        "with the home nest out, a Fauna chip makes an end-to-end room"
    )
    assert not conv.home_nest_included()
    conv.toggle_home_nest()
    assert conv.home_nest_included(), seat_element(founder, "recipient-picker-home-nest-toggle")
    assert conv.prospective_room_class() == "community", (
        "the class statement follows the choice, before the first message: "
        f"{seat_element(founder, 'recipient-picker-class')}"
    )
    founder.app.driver.type_text("dm-text-field", f"hello {first_word}")
    founder.app.driver.click("dm-send-button")

    room = wait_for(
        "the founder's community room binding to its room id",
        lambda: _founded_room(founder, first_word),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'found', 'room')}",
    )
    room_id = room.channel_id_hex
    assert not founder.app.has_error(), founder.app.error_text()
    conv.open_thread_by_id(room.thread_id)
    assert conv.room_class() == "community", (
        f"the header states the class: {seat_element(founder, 'thread-room-class')}"
    )

    # ── the invitation stands atop the member's list ─────────────────────
    wait_for(
        "the founder's invitation standing on the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )

    # ── the founder sees it pending, takes it back, and offers it again ──
    # The editor's pending section (`room-pending-invite[i]`) is served to
    # the founder because the founder may withdraw it; the withdrawal is
    # the ruled no-audience act — the invitee is told nothing, their
    # envelope simply stops standing (`conversation-rooms.md` § Join rules
    # and invites → *Pending invitations are visible to whoever may
    # withdraw them*). So the member-side observable is the row LEAVING
    # their list on the next sweep, never an accept raced against that
    # sweep; what a stale accept answers is pinned at the API tier
    # (`a_withdrawn_invitation_leaves_with_its_envelope_and_cannot_be_accepted`).
    conv.open_room_settings()
    wait_for(
        "the founder's editor listing the member's invitation as pending",
        lambda: conv.pending_invite_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'pending', 'invite')}",
    )
    pending_row = conv.pending_invite_text(0)
    assert member.handle.split("@")[0] in pending_row, (
        f"the row names the invitee as the home nest knows them: {pending_row!r}"
    )
    assert conv.pending_invite_lapsed(0) is False, (
        "an invitation its inviter may still issue has not lapsed: "
        f"{seat_element(founder, 'room-pending-invite')}"
    )
    conv.withdraw_pending_invite(0)
    wait_for(
        "the withdrawn row leaving the founder's editor",
        lambda: conv.pending_invite_count(),
        lambda n: n == 0,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={founder.app.error_text()!r}\n{seat_story(founder)}",
    )
    assert not founder.app.has_error(), founder.app.error_text()
    # Nothing is staged, so Save is the editor's plain close.
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    wait_for(
        "the withdrawn invitation leaving the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n == 0,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )
    # Inviting again is the undo: on a community room the add-participant
    # overlay issues an invitation and seats nobody.
    conv.add_participant_to_thread(member.actor_hex)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the renewed invitation standing on the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )

    # ── accepting opens the room ─────────────────────────────────────────
    member.app.conversations.accept_room_invitation(0)
    joined = wait_for(
        "the accepted room opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert not member.app.has_error(), (
        f"accepting must not refuse: {member.app.error_text()!r}\n{seat_story(member)}"
    )
    assert member.app.conversations.room_invitation_count() == 0, (
        "an accepted invitation stops standing"
    )

    # ── the key-in: the founder's device keys the member, no gesture ────
    wait_for(
        "the member's seat being keyed in by the founder's device",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'key-in', 'key_in', 'tend', 'mint')}"
        ),
    )

    # ── a message each way, sealed under the room's generation ───────────
    conv.send_in_open_thread(f"glad you came {later_word}")
    wait_for(
        "the founder's message opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and later_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'room')}",
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

    # ── the home nest's read: its index finds the text, for a member ─────
    hits = wait_for(
        "the home nest's search index finding the member's reply",
        lambda: _search(port, member.actor, room_id, reply_word),
        lambda h: bool(h),
        SEARCH_BUDGET_S,
        diagnose=lambda: f"search={_search(port, member.actor, room_id, reply_word)!r}",
    )
    assert all("text" not in h and "snippet" not in h for h in hits), (
        f"a hit is a position, never the message's text: {hits!r}"
    )

    # ── withdrawing the read: one toggle and Save, in the app ────────────
    founder_view = thread_by_channel(founder.app, room_id)
    assert (founder_view.room or {}).get("nest_read") is True, founder_view.room
    conv.open_thread_by_id(founder_view.thread_id)
    conv.open_room_settings()
    assert conv.room_nest_read_staged() is True, seat_element(founder, "room-nest-read-toggle")
    conv.toggle_room_nest_read()
    assert conv.room_nest_read_staged() is False
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()

    # The rotation wrapped past the nest; the revoke deletes every view it
    # built, so the same query now answers empty — the members' choice
    # working, never a refusal (RoomSearchReply's contract).
    wait_for(
        "the home nest's index emptying once its read is withdrawn",
        lambda: _search(port, member.actor, room_id, reply_word),
        lambda h: h == [],
        SEARCH_BUDGET_S,
        diagnose=lambda: (
            f"search={_search(port, member.actor, room_id, reply_word)!r}\n"
            f"{seat_story(founder)}\n{seat_log(founder, 'nest_read', 'rotate', 'mint')}"
        ),
    )
    assert _search(port, member.actor, room_id, first_word) == [], (
        "every view the nest built goes, not only the newest"
    )
    after = wait_for(
        "the founder's projection naming the read withdrawn",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("nest_read") is False,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    assert (after.room or {}).get("class") == "Community", (
        "withdrawing the read does not change what the room is — the class is its "
        f"seating, and the nest keeps its floor seat: {after.room!r}"
    )

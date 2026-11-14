"""Tier_3 — a room whose moderation rests on a name it cannot verify says so.

Outcome **44** of ``docs/features/group-conversations.md``
(``community-rooms.md`` § The three classes; the mechanism is
``conversation-rooms.md`` § Roles and authorization → *Delete any message —
the mechanism* → *Members verify what they paint*): a floor delete record
parked on a name this device holds no anchor for paints nothing, and a retired
name homed on ANOTHER nest never anchors — the peer-anchor harvest's reach is
same-nest only (``identity-succession.md`` § The succession statement, the
harvest's bound 3). Silence would show the target as if nobody had acted on
it, so once the harvest has settled the name the room states its moderation is
unverified (``RoomSnapshot::moderation_unverified`` → ``thread-room-notice``)
and keeps the message shown.

**Two seats, two nests** (``cross_nest_room_seats``): the founder on the
room's home, the member on ``away``. The member posts; the founder takes their
account back (the theft remedy, ``succeed_identity_from``) and, as the
successor, deletes the member's message. The successor's authority runs
through the founder's RETIRED name — the name the member's anchored policy
chain carries — and that name is homed on the other nest, so the member's
device can never verify the line. The witnesses, on the member's seat: the
room notice reads the moderation-unverified sentence, and the message is
still shown (no tombstone). The successor's own seat tombstones it: the
control that the delete was filed at all.

**Latency-independent** (convention 14): every wait is a deadline poll.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    found_cross_nest_community_room,
    room_app,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from helpers.succession_ceremony import (
    require_stolen_gate,
    succeed_identity_from,
    wait_for_successor_actor,
)
from i18n.strings import S
from tests.test_conversation_room_community_cross_nest import (  # noqa: F401 — a pytest fixture adopted by import
    cross_nest_room_seats,
)

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


def _texts(seat) -> list[str]:
    """The open thread's live bubble texts — a tombstone paints no text."""
    d = seat.app.driver
    return [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]


def _tombstones(seat) -> int:
    return seat.app.driver.count("dm-message-deleted")


def _index_of(seat, needle: str) -> int | None:
    """The live bubble's index — also its ``dm-message-actions-button``
    index, since a tombstone paints neither a text nor a ⋯."""
    for i, text in enumerate(_texts(seat)):
        if needle in text:
            return i
    return None


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("community-room-survives-succession")
def test_a_room_says_so_when_its_moderation_rests_on_a_retired_name_from_another_nest(
    cross_nest_room_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """The successor's delete of the foreign member's message parks on the
    founder's retired name; the member's room says its moderation could not be
    verified and keeps the message shown."""
    founder, member = cross_nest_room_seats
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    word = secrets.token_hex(4)
    room_id, founder_thread, member_thread = found_cross_nest_community_room(
        founder, member, home, away, f"hello {word}"
    )
    mconv = member.app.conversations
    mconv.open_thread_by_id(member_thread)
    target = f"the successor takes this away {word}"
    mconv.send_in_open_thread(target)
    assert not member.app.has_error(), member.app.error_text()
    # The founder's device must have READ the message before the ceremony: a
    # successor opens no generation minted before it (the predecessor's
    # reception keys are not re-sealed — `community-rooms.md` § Implementation
    # status today, *A seat gains or rotates its wrap target*), so what it
    # shows of the room's past is what this device folded into the history
    # slice — landed by the ceremony's flush before its submit
    # (`succession-aftermath.md` § Re-key scope → *The predecessor's replica
    # is flushed before the submit*).
    founder.app.conversations.open_thread_by_id(founder_thread)
    wait_for(
        "the member's message shown on the founder's seat before the ceremony",
        lambda: _index_of(founder, target),
        lambda i: i is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )

    # ── the founder takes their account back ─────────────────────────────
    old_actor = founder.actor_hex
    founder.app.settings.navigate()
    founder.app.settings.open_recovery_kit()
    require_stolen_gate(founder.app)
    succeed_identity_from(founder.app)
    wait_for_successor_actor(
        founder.app, old_actor, diagnose=lambda: "\n" + seat_log(founder, "succession", "launch")
    )

    # ── the successor deletes the member's message ───────────────────────
    returned = wait_for(
        "the room listed on the successor's seat, keyed and owned",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None
        and (t.room or {}).get("my_role") == "Owner"
        and (t.room or {}).get("awaiting_key") is False,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'tend', 'mint', 'reception')}",
    )
    fconv = founder.app.conversations
    fconv.open_thread_by_id(returned.thread_id)
    index = wait_for(
        "the member's message shown on the successor's seat",
        lambda: _index_of(founder, target),
        lambda i: i is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    fconv.delete_message(message_index=index)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the successor's own seat tombstoning the message (the delete was filed)",
        lambda: (_tombstones(founder), _texts(founder)),
        lambda seen: seen[0] >= 1 and not any(target in t for t in seen[1]),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{_texts(founder)!r}\n{seat_story(founder)}",
    )

    # ── the member's room says its moderation could not be verified ──────
    wait_for(
        "the member's room stating its moderation is unverified",
        lambda: mconv.room_notice(),
        lambda notice: notice == S.conversations.unified.room_notice_moderation_unverified,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"{seat_element(member, 'thread-room-notice')}\n{seat_story(member)}\n"
            f"{seat_log(member, 'park', 'succession', 'harvest', 'witness', 'floor')}"
        ),
    )
    assert any(target in t for t in _texts(member)) and _tombstones(member) == 0, (
        "the message stays shown on the member's seat, never tombstoned: "
        f"texts={_texts(member)!r} tombstones={_tombstones(member)}"
    )
    for seat in (founder, member):
        assert not seat.app.has_error(), seat.app.error_text()

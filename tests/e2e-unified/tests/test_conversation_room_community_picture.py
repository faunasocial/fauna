"""Tier_3 — a picture exchanged in a community room opens for the other member.

``community-rooms.md`` § The three classes: a community room's messages are
sealed under the room's generation, and an attachment rides with its message —
its bytes stored on the room's home nest and opened by every member holding
the generation (``conversation-rooms.md`` § The home nest). The labeler
journey already sends a picture into a community room, but asserts the home
nest's verdict on its bytes, never that the other MEMBER can open it; this is
that half.

**Two seats, one nest** (``helpers.room_seats.room_seat_pair``): the member
sends a picture with a word; the founder's thread paints the image — the
picture itself, not its placeholder — beside the word. Then the founder sends
one back and the member's thread paints it, so both directions are witnessed.

**Latency-independent** (convention 14): each wait is a deadline poll on the
painted state (``attachment_image_states`` reading ``painted``), never a delay.
"""

from __future__ import annotations

import secrets
from pathlib import Path

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    found_community_room,
    room_app,
    room_seat_pair,
    seat_element,
    seat_story,
    wait_for,
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

PICTURE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _painted(seat, needle: str) -> list[str]:
    """The open thread's image states once a bubble carrying ``needle`` has
    painted — empty until then."""
    d = seat.app.driver
    texts = [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]
    if not any(needle in t for t in texts):
        return []
    return seat.app.conversations.attachment_image_states()


@pytest.mark.feature("group-conversations")
def test_a_picture_sent_in_a_community_room_opens_for_the_other_member(room_seat_pair):
    """The member's picture paints on the founder's seat, and the founder's
    on the member's."""
    founder, member = room_seat_pair
    word = secrets.token_hex(4)
    _room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    member.app.conversations.open_thread_by_id(member_thread)
    founder.app.conversations.open_thread_by_id(founder_thread)

    # ── the member's picture, opened by the founder ──────────────────────
    from_member = f"a picture from the member {word}"
    member.app.conversations.send_attachment_in_open_thread(str(PICTURE), from_member)
    assert not member.app.has_error(), member.app.error_text()
    wait_for(
        "the member's picture painting on the founder's seat",
        lambda: _painted(founder, from_member),
        lambda states: states.count("painted") >= 1,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_element(founder, 'dm-attachment-image')}\n{seat_story(founder)}",
    )

    # ── the founder's picture, opened by the member ──────────────────────
    from_founder = f"and one back {word}"
    founder.app.conversations.send_attachment_in_open_thread(str(PICTURE), from_founder)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the founder's picture painting on the member's seat",
        lambda: _painted(member, from_founder),
        lambda states: states.count("painted") >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_element(member, 'dm-attachment-image')}\n{seat_story(member)}",
    )
    for seat in (founder, member):
        assert not seat.app.has_error(), seat.app.error_text()

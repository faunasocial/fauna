"""Tier_3 — a community room's owner takes their account back and keeps the room.

Two outcomes of ``docs/features/group-conversations.md``, one story:

- **34** — after an identity succession the successor holds the owner's seat
  (``conversation-rooms.md`` § Roles and authorization; the floor half is
  ``community-rooms.md`` § Implementation status today, *An identity
  succession hands a seat on*: the ceremony seats the successor at the
  predecessor's rank and absorbs the predecessor's row).
- **50**, its first clause — after taking your account back your app re-keys
  its own seat and reads the room again (``community-rooms.md`` § The three
  classes; the client half is the tend pass, ``FaunaMlsBackend::
  tend_community_room``: the successor's account plane holds no reception key,
  so the first pass mints a fresh one, supplies it through
  ``room.set_reception_key``, and — the room's only key authority being this
  seat — mints a fresh generation parented on the answered ``uncovered_tip``).

**Two seats, one nest** (``helpers.room_seats.room_seat_pair``): the founder
founds a community room and seats the member, then runs the theft remedy from
its own Settings (``helpers.succession_ceremony.succeed_identity_from``: a kit,
then "my identity was stolen" with it) and the app comes back up as the
successor. The witnesses, each a state the home nest or an app holds:

1. the home nest's floor names the successor as the room's **owner** and the
   retired id nowhere — the seat moved, it was not re-joined;
2. the successor's floor entry is **covered by the tip's wraps** — its own
   device re-keyed the seat (no other device of the room may: a plain member
   never mints);
3. a message the member sends **after** the ceremony paints on the successor's
   seat, and one the successor sends paints on the member's — the room reads
   both ways under the generation the successor minted;
4. the successor's own app still renders itself the room's owner.

Fixture order: unlike ``test_identity_succession_ceremony.py``'s member
re-point, nothing here waits on the MEMBER's peer-anchor harvest — every
witness is the floor's or the successor's own, and the floor resolves the
succession inside its own transaction — so the kit is made just before the
ceremony.

**Latency-independent** (convention 14): every wait is a deadline poll on the
state named, never a delay.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    found_community_room,
    room_app,
    room_seat_pair,
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


def _floor(port: int, reader: dict, room_id: str) -> dict[str, dict] | None:
    """The home nest's floor as ``reader`` reads it, keyed by principal;
    ``None`` on a refusal or while unanswered."""
    try:
        roster = conv_api.room_list_roster(port, reader, room_id)
    except Exception:
        return None
    return {m.get("principal"): m for m in roster.get("members", [])}


def _bubble_texts(seat) -> list[str]:
    d = seat.app.driver
    return [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("community-room-survives-succession")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
def test_a_community_rooms_owner_takes_the_account_back_and_keeps_the_room(room_seat_pair):
    """The successor holds the owner's seat, re-keys it from its own device,
    and the room reads both ways again."""
    founder, member = room_seat_pair
    word = secrets.token_hex(4)
    room_id, _founder_thread, member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    port = founder.port
    old_actor = founder.actor_hex

    # Skip-gate before the ceremony: an app without the succession driver
    # reports "not built" rather than failing on the room it does have.
    founder.app.settings.navigate()
    founder.app.settings.open_recovery_kit()
    require_stolen_gate(founder.app)

    # ── the theft remedy, from the founder's own Settings ────────────────
    ran_from, _held = succeed_identity_from(founder.app)
    assert ran_from == old_actor, (
        f"the ceremony ran from the founder's seat: Status read {ran_from!r}, "
        f"the seat signed in as {old_actor!r}"
    )
    successor = wait_for_successor_actor(
        founder.app,
        old_actor,
        diagnose=lambda: "\n" + seat_log(founder, "succession", "launch"),
    )

    # ── (1) the floor seats the successor as owner; the retired id is gone ─
    seated = wait_for(
        "the home nest's floor naming the successor as the room's owner",
        lambda: _floor(port, member.actor, room_id),
        lambda floor: floor is not None
        and (floor.get(successor) or {}).get("role") == "owner"
        and old_actor not in floor,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"floor={_floor(port, member.actor, room_id)!r}",
    )
    owners = [p for p, m in seated.items() if m.get("role") == "owner"]
    assert owners == [successor], f"exactly one owner, the successor: {seated!r}"

    # ── (2) the successor's own device holds the room and re-keys its seat ─
    # The room first: the tend pass that supplies the seat's key walks the
    # rooms this device holds, so a room the successor's app does not list
    # can never be re-keyed, and the failure says which step broke.
    returned = wait_for(
        "the room listed on the successor's seat",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None,
        WELCOME_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )

    wait_for(
        "the successor's floor entry covered by the tip's wraps (its own re-key)",
        lambda: (_floor(port, member.actor, room_id) or {}).get(successor, {}).get(
            "tip_wrapped"
        ),
        lambda wrapped: wrapped is True,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"floor={_floor(port, member.actor, room_id)!r}\n"
            f"{seat_log(founder, 'tend', 'reception', 'mint', 'uncovered')}"
        ),
    )

    # ── (3) the room reads both ways again ───────────────────────────────
    fconv = founder.app.conversations
    mconv = member.app.conversations
    fconv.open_thread_by_id(returned.thread_id)
    mconv.open_thread_by_id(member_thread)

    after = f"after the recovery {word}"
    mconv.send_in_open_thread(after)
    assert not member.app.has_error(), member.app.error_text()
    wait_for(
        "the member's post-ceremony message painting on the successor's seat",
        lambda: _bubble_texts(founder),
        lambda texts: any(after in t for t in texts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(founder)}\n{seat_log(founder, 'awaiting', 'generation', 'tend')}"
        ),
    )
    back = f"the successor answers {word}"
    fconv.send_in_open_thread(back)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the successor's message painting on the member's seat",
        lambda: _bubble_texts(member),
        lambda texts: any(back in t for t in texts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'generation', 'room')}",
    )

    # ── (4) the successor's app renders itself the owner ─────────────────
    mine = wait_for(
        "the successor's own room projection carrying its role",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    assert (mine.room or {}).get("my_role") == "Owner", (
        f"the successor's app renders it the owner: room={mine.room!r}"
    )
    for seat in (founder, member):
        assert not seat.app.has_error(), seat.app.error_text()

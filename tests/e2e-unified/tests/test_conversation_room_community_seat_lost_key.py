"""Tier_3 — a community-room seat that lost its key is re-keyed by its own app.

Outcome **50** of ``docs/features/group-conversations.md``, its second clause —
"on a seat that lost its key, your app re-keys its own seat and reads the room
again" (``community-rooms.md`` § The three classes; the recovery arm is
§ Implementation status today, *A reception key is FIPS-203-valid before it is
ever stored*, and the heal is *A seat gains or rotates its wrap target*: the
member's own ``FaunaMlsBackend::tend_community_room`` pass reads its seat's key
absent and re-supplies its current one through ``room.set_reception_key``). The
first clause — after taking your account back — is
``test_conversation_room_community_succession.py``'s.

**No gesture loses a seat's key.** A seat loses it only by a key the nest
stored before every writer validated one, and no door can write such a key any
more — so the state is seeded by the nest's test hook
``POST /api/v1/test/rooms/poison-seat-key`` (``room_seat_key_test_hook.rs``,
``test-hooks`` builds only), which writes the same FIPS-invalid bytes the
conformance case ``a_poisoned_seat_is_skipped_not_frozen_and_heals_on_its_own_rekey``
seeds (convention 8 carve-out: setup no user can produce). Everything after the
hook is production.

**Two seats, one nest** (``helpers.room_seats.room_seat_pair``): the founder
founds a community room and seats the member (every step a gesture), then the
hook poisons the member's seat. The witnesses, each a state the home nest or an
app holds:

1. the floor reports the member's key **absent** right after the hook — the
   roster projects an unusable key as none, which is what the member's tend
   pass reads — and the member's app **reaches for** ``room.set_reception_key``
   (refused for the member alone by the actor-scoped refusal hook,
   ``helpers.rpc_hold.refuse_rpc``, so the keyless state holds still to be read
   and the refusal count proves the attempt is the member's own);
2. the floor carries the member's key **again, the same key it held before** —
   only the member's own device can write its seat, so its app re-keyed it —
   and the seat is covered by the tip's wraps;
3. messages sent **after** the heal read both ways.

**Latency-independent** (convention 14): every wait is a deadline poll on the
state named, never a delay.
"""

from __future__ import annotations

import json
import secrets
import urllib.request

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    found_community_room,
    room_app,
    room_seat_pair,
    seat_log,
    seat_story,
    wait_for,
)
from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
from tests.api import conv_api

SET_RECEPTION_KEY = "fauna.conversations.room.set_reception_key"

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


def _member_row(port: int, reader: dict, room_id: str, member_hex: str) -> dict | None:
    """``member_hex``'s floor row as ``reader`` reads it; ``None`` on a
    refusal, while unanswered, or when the member is off the floor."""
    try:
        roster = conv_api.room_list_roster(port, reader, room_id)
    except Exception:
        return None
    for m in roster.get("members", []):
        if m.get("principal") == member_hex:
            return m
    return None


def _key(row: dict | None) -> bytes | None:
    key = (row or {}).get("reception_pubkey")
    return bytes(key) if key else None


def _poison_seat_key(port: int, room_id: str, member_hex: str) -> bool:
    body = json.dumps({"room_id": room_id, "principal_id": member_hex}).encode()
    req = urllib.request.Request(
        f"{conv_api._base_url(port)}/api/v1/test/rooms/poison-seat-key",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"the seat-key poisoning hook returned {resp.status}"
        return bool(json.loads(resp.read()).get("poisoned"))


def _bubble_texts(seat) -> list[str]:
    d = seat.app.driver
    return [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("room-seat-lost-key")
def test_a_community_room_seat_that_lost_its_key_is_rekeyed_by_its_own_app(room_seat_pair):
    """The floor loses the member's key; the member's app puts it back and the
    room reads both ways."""
    founder, member = room_seat_pair
    word = secrets.token_hex(4)
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    port = founder.port
    mhex = member.actor_hex

    held = _key(_member_row(port, founder.actor, room_id, mhex))
    assert held, (
        "the member's seat is keyed once the founding completes: "
        f"{_member_row(port, founder.actor, room_id, mhex)!r}"
    )

    # The member's re-key is turned away while the seat is poisoned, so the
    # keyless state holds still to be read, and the refusal count proves the
    # member's own app reached for the door (the founder's calls pass).
    refuse_rpc(port, SET_RECEPTION_KEY, actor=mhex)
    try:
        # ── the seat loses its key (the hook: no gesture reaches it) ────────
        assert _poison_seat_key(port, room_id, mhex), "the member holds a live user seat"

        # ── (1) the floor reads the seat keyless — what the tend pass sees ──
        first = _member_row(port, founder.actor, room_id, mhex)
        assert first is not None, "the member stays on the floor"
        assert _key(first) is None, (
            "the roster never serves the poisoned bytes — an unusable key reads "
            f"as none: {first!r}"
        )
        wait_for_refused_rpc(port, SET_RECEPTION_KEY, timeout=WELCOME_BUDGET_S)
        assert _key(_member_row(port, founder.actor, room_id, mhex)) is None, (
            "a refused re-key leaves the seat keyless"
        )
    finally:
        release_rpc_hold(port, SET_RECEPTION_KEY)

    # ── (2) the member's own app re-keys its seat ───────────────────────────
    healed = wait_for(
        "the member's floor row carrying its own key again, covered by the tip",
        lambda: _member_row(port, founder.actor, room_id, mhex),
        lambda row: row is not None and _key(row) == held and row.get("tip_wrapped") is True,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"row={_member_row(port, founder.actor, room_id, mhex)!r}\n"
            f"{seat_log(member, 'tend', 'reception', 'wrap target')}"
        ),
    )
    assert healed.get("role") != "owner", "the member stays a member"

    # ── (3) the room reads both ways after the heal ─────────────────────────
    fconv = founder.app.conversations
    mconv = member.app.conversations
    fconv.open_thread_by_id(founder_thread)
    mconv.open_thread_by_id(member_thread)

    out = f"after the re-key {word}"
    fconv.send_in_open_thread(out)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the founder's post-heal message painting on the member's seat",
        lambda: _bubble_texts(member),
        lambda texts: any(out in t for t in texts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'generation', 'room')}",
    )
    back = f"the member answers {word}"
    mconv.send_in_open_thread(back)
    assert not member.app.has_error(), member.app.error_text()
    wait_for(
        "the member's post-heal message painting on the founder's seat",
        lambda: _bubble_texts(founder),
        lambda texts: any(back in t for t in texts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'generation', 'room')}",
    )
    for seat in (founder, member):
        assert not seat.app.has_error(), seat.app.error_text()

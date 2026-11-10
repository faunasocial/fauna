"""Tier_3 — history for a community room's later joiner: the tip under ``none``, the whole retained history under ``full``.

``community-rooms.md`` § The three classes → *The add's backfill lands, and
``history_policy`` becomes enforceable*: ``none`` — every room's default —
authorizes the tip alone, so someone seated later reads only from the tip;
``full`` authorizes the whole retained bundle, which the admitting device
publishes through ``room.backfill_generations``. The end-to-end class's twin
is ``test_conversation_room_history.py``; this is the community class, whose
mechanism is entirely different (generation wraps on the home nest, never an
MLS ``HistorySlice``).

**Three seats, one nest** (``helpers.room_seats.room_seats``): the founder
founds the room with one member (``found_community_room``, every step a
gesture) and the room says something; then the founder invites the
``newcomer`` through the room's own add-person control — on a community room
that issues an invitation — and the newcomer accepts from their list. The
``full`` leg first sets the policy through the room's settings editor.

**The newcomer's view is anchored** on a message the founder sends after the
newcomer was keyed in: once it paints, everything the room will ever show the
newcomer has been fetched (the backfill is published at the key-in, ahead of
it), so what is or is not there is a statement about the policy, not timing
(convention 14).
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    found_community_room,
    room_app,
    room_seats,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
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


def _bodies(seat) -> list[str]:
    d = seat.app.driver
    return [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]


def _tip(seat, room: str) -> str | None:
    """The id of the room's tip generation as the home nest wraps it to
    ``seat``, or ``None`` on a refusal."""
    try:
        gens = conv_api.room_generations(seat.port, seat.actor, room)
    except Exception:
        return None
    return next((g["generation_id"] for g in gens if g.get("is_tip")), None)


def _join_later(founder, member, newcomer, *, policy: str) -> tuple[str, str]:
    """Found the room with ``member``, say ``before`` in it, rotate the
    room's generation, set ``policy``
    when it is not the default, then invite ``newcomer``, who accepts and is
    keyed in; the founder then says ``after``. Returns ``(before, after)``
    with the newcomer's room open."""
    word = secrets.token_hex(4)
    before = f"said before the newcomer {word}"
    after = f"said after the newcomer {word}"
    room_id, founder_thread, _member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    conv = founder.app.conversations
    conv.open_thread_by_id(founder_thread)
    conv.send_in_open_thread(before)
    wait_for(
        "the founder's pre-join message reaching the member",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and before in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )

    # ── the room moves to a new generation before the newcomer arrives ──
    # `none` authorizes the TIP generation, not "everything after the join":
    # a message sealed under the generation that is still the tip when the
    # newcomer is keyed in is readable to them by design. So the room rotates
    # first — withdrawing the home nest's read is a rotation, one toggle and
    # Save — and `before` is then sealed under a generation the newcomer is
    # wrapped only if the history rule shares it.
    tip_before = _tip(founder, room_id)
    conv.open_room_settings()
    conv.toggle_room_nest_read()
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the read's withdrawal rotating the room's generation",
        lambda: _tip(founder, room_id),
        lambda tip: tip is not None and tip != tip_before,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'nest_read', 'rotate', 'mint')}",
    )

    if policy == "full":
        conv.open_room_settings()
        conv.set_room_history_policy("full")
        conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
        assert not founder.app.has_error(), founder.app.error_text()
        wait_for(
            "the community room's history policy reading `full`",
            lambda: thread_by_channel(founder.app, room_id),
            lambda t: t is not None
            and ((t.room or {}).get("policy") or {}).get("history_policy") == "Full",
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: seat_story(founder),
        )
    else:
        view = thread_by_channel(founder.app, room_id)
        assert ((view.room or {}).get("policy") or {}).get("history_policy") == "None", (
            f"a community room is born under `none`: {view.room!r}"
        )

    # ── the invitation, through the room's own add-person control ────────
    conv.add_participant_to_thread(newcomer.actor_hex)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the founder's invitation standing on the newcomer's list",
        lambda: newcomer.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(newcomer)}\n{seat_log(newcomer, 'invitation', 'room')}",
    )
    newcomer.app.conversations.accept_room_invitation(0)
    wait_for(
        "the newcomer being keyed in by the founder's device",
        lambda: thread_by_channel(newcomer.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(newcomer)}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'key-in', 'key_in', 'backfill', 'tend', 'mint')}"
        ),
    )
    assert not newcomer.app.has_error(), newcomer.app.error_text()

    # ── the anchor: a message after the key-in ───────────────────────────
    conv.send_in_open_thread(after)
    joined = wait_for(
        "the founder's post-join message reaching the newcomer",
        lambda: thread_by_channel(newcomer.app, room_id),
        lambda t: t is not None and after in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(newcomer),
    )
    newcomer.app.conversations.open_thread_by_id(joined.thread_id)
    wait_for(
        "the newcomer's bubbles painting the post-join message",
        lambda: _bodies(newcomer),
        lambda b: any(after in t for t in b),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(newcomer),
    )
    return before, after


@pytest.mark.feature("group-conversations")
def test_a_community_rooms_later_joiner_under_none_reads_only_from_the_tip(room_seats):
    """Under the default ``none`` the newcomer reads what the room says after
    they were keyed in, and nothing it said before."""
    founder, member, newcomer = room_seats
    before, _after = _join_later(founder, member, newcomer, policy="none")
    bodies = _bodies(newcomer)
    assert not any(before in t for t in bodies), (
        f"under `none` nothing said before the join paints for the newcomer: {bodies!r}"
    )


@pytest.mark.feature("group-conversations")
def test_a_community_rooms_later_joiner_under_full_receives_the_retained_history(room_seats):
    """Under ``full`` the whole retained history arrives: the newcomer reads
    what the room said before they joined."""
    founder, member, newcomer = room_seats
    before, _after = _join_later(founder, member, newcomer, policy="full")
    bodies = wait_for(
        "the retained history painting for the newcomer",
        lambda: _bodies(newcomer),
        lambda b: any(before in t for t in b),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(newcomer)}\n{seat_log(newcomer, 'backfill', 'generation')}",
    )
    assert any(before in t for t in bodies)

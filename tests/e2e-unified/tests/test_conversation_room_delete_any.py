"""Tier_3 — an owner's and an admin's delete of another member's message, end-to-end class.

``conversation-rooms.md`` § Roles and authorization — the role table's *delete
any message* row and *Delete any message — the mechanism*: a cross-sender
delete is a second admission beside the sender match, honoured by every member
only when its MLS-authenticated author held owner or admin under the policy of
the epoch the delete was sealed in. The affordance is the snapshot's
(``MessageSnapshot::can_delete``) — the same ``dm-message-delete-button`` →
``dm-message-delete-confirm-button`` on a bubble the viewer may delete, and no
delete at all on one it may not.

**Three principals, three seats** (``helpers.room_seats``): the ``owner``; the
``stayer``, who is a plain member first (no delete on another's bubble), is then
appointed admin through the policy editor, and deletes as admin; and the
``leaver``, the plain member whose two messages are the targets. Every tombstone
is asserted on all three seats — the deleter's is optimistic, the other two are
each *that seat's own engine* admitting the delete.

**Latency-independent by construction** (convention 14): every assertion is a
deadline-poll on a state that, once reached, stays — a tombstone is never
lifted, and the absent delete button is read only after the open menu has
painted its reaction row (the menu is up, so an absent item is absent, not
late).

The community class's half is unbuilt and has no seat setup here (nothing in
this suite founds a community room); the owner doc's § Implementation status
today declares it.
"""

from __future__ import annotations

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    bootstrap_room,
    index_of,
    room_app,
    room_seats,
    seat_story,
    thread_by_channel,
    wait_for,
    wait_snippet,
    wait_thread_by_channel,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads; the other six join through the batched trickle-down, each
    # arriving with its own marker (and its `helpers.room_seats.ROOM_APPS` entry).
    pytest.mark.tui,
]

FIRST = "the member's first message"
SECOND = "the member's second message"


def _bubble_texts(seat) -> list[str]:
    d = seat.app.driver
    return [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))]


_OPENED: set[int] = set()


def _ensure_open(seat, channel: str) -> None:
    """Open ``seat``'s thread for ``channel`` — once. A seat in this journey
    only ever looks at the one room, and an app already showing a thread's
    detail has no list to pick it from again (tui's detail replaces the list
    pane), so a second open would be a harness act with no user analogue."""
    if id(seat) in _OPENED:
        return
    thread = thread_by_channel(seat.app, channel)
    assert thread is not None, seat_story(seat)
    seat.app.conversations.open_thread_by_id(thread.thread_id)
    _OPENED.add(id(seat))


def _open_with(seat, channel: str, needle: str) -> int:
    """With ``seat``'s thread for ``channel`` open, wait until a live bubble
    carries ``needle``; return that bubble's index among the live bubbles —
    which is also its ``dm-message-actions-button`` index, since a tombstone
    paints neither a text nor a ⋯."""
    _ensure_open(seat, channel)
    texts = wait_for(
        f"a bubble carrying {needle!r} painting",
        lambda: _bubble_texts(seat),
        lambda ts: any(needle in t for t in ts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )
    return next(i for i, t in enumerate(texts) if needle in t)


def _wait_tombstoned(seat, channel: str, needle: str, tombstones: int, *, what: str) -> None:
    _ensure_open(seat, channel)
    d = seat.app.driver
    wait_for(
        what,
        lambda: (d.count("dm-message-deleted"), _bubble_texts(seat)),
        lambda seen: seen[0] >= tombstones and not any(needle in t for t in seen[1]),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(seat)}\n{d.diagnose('dm-message-deleted')}",
    )


@pytest.mark.feature("group-conversations")
def test_an_admin_and_the_owner_delete_a_members_message_for_everyone(room_seats):
    """A plain member is offered no delete on another member's bubble; once
    appointed admin the same seat deletes it, and the owner deletes another —
    each leaving the same tombstone on all three seats."""
    owner, stayer, leaver = room_seats
    _OPENED.clear()
    _group_id, channel = bootstrap_room(owner, stayer, leaver)
    for member, who in ((stayer, "stayer"), (leaver, "leaver")):
        wait_thread_by_channel(member, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(member, channel, "hi room", what=f"'hi room' reaching the {who}")

    # ── the plain member posts the two target messages ───────────────────
    leaver_thread = thread_by_channel(leaver.app, channel)
    for body in (FIRST, SECOND):
        leaver.app.conversations.real_send(leaver_thread.thread_id, body)
        for seat, who in ((owner, "owner"), (stayer, "stayer")):
            wait_snippet(seat, channel, body, what=f"{body!r} reaching the {who}")

    # ── a plain member: the ⋯ is there, delete is not ────────────────────
    first_on_stayer = _open_with(stayer, channel, FIRST)
    stayer.app.conversations.open_message_actions(message_index=first_on_stayer)
    wait_for(
        "the stayer's ⋯ menu painting its reaction row",
        lambda: stayer.app.driver.count("dm-reaction-option"),
        lambda n: n > 0,
        MESSAGE_BUDGET_S,
        diagnose=lambda: stayer.app.driver.diagnose("dm-message-actions-menu"),
    )
    assert stayer.app.driver.count("dm-message-delete-button") == 0, (
        "a plain member is offered no delete on another member's message: "
        f"{stayer.app.driver.diagnose('dm-message-delete-button')}"
    )
    # Leave the menu the way a member would — by using it (a reaction closes it).
    stayer.app.driver.click("dm-reaction-option", index=0)

    # ── the owner appoints the stayer through the policy editor ──────────
    _ensure_open(owner, channel)
    owner_open = wait_for(
        "the owner's chips rendering the roster",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and owner.app.conversations.member_chip_count() >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    stayer_index = index_of(owner_open, stayer.actor_hex)
    owner.app.conversations.open_room_settings()
    owner.app.conversations.toggle_room_admin(stayer_index)
    owner.app.conversations.save_room_settings()
    assert not owner.app.has_error(), (
        f"the owner's appointment must not refuse: {owner.app.error_text()!r}"
    )
    wait_for(
        "the stayer's projection flipping to Admin",
        lambda: thread_by_channel(stayer.app, channel),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Admin",
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(stayer),
    )

    # ── the admin deletes the member's first message ─────────────────────
    first_on_stayer = _open_with(stayer, channel, FIRST)
    stayer.app.conversations.delete_message(message_index=first_on_stayer)
    assert not stayer.app.has_error(), stayer.app.error_text()
    for seat, who in ((stayer, "admin's own"), (owner, "owner's"), (leaver, "author's")):
        _wait_tombstoned(
            seat, channel, FIRST, 1, what=f"the admin's delete tombstoning on the {who} seat"
        )

    # ── the owner deletes the member's second ────────────────────────────
    second_on_owner = _open_with(owner, channel, SECOND)
    owner.app.conversations.delete_message(message_index=second_on_owner)
    assert not owner.app.has_error(), owner.app.error_text()
    for seat, who in ((owner, "owner's own"), (stayer, "admin's"), (leaver, "author's")):
        _wait_tombstoned(
            seat, channel, SECOND, 2, what=f"the owner's delete tombstoning on the {who} seat"
        )

    # The room's first message — nobody deleted it — still reads on every seat.
    for seat in (owner, stayer, leaver):
        assert any("hi room" in t for t in _bubble_texts(seat)), seat_story(seat)

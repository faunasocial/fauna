"""Tier_3 — a member walks out of a room, end-to-end class, per app: three seats on one nest.

``conversation-rooms.md`` § Roles and authorization → *Leaving — the
mechanism*: the roles table has granted *leave (remove self)* to admins and
members since the room model landed, but no app produced a departure, so one
reached the floor only when some *other* member happened to commit. The
departure is now the self-scoped ``room.leave`` door on this class too (it was
a final roster report, which a leaver behind the newest commit could use to
roll the floor back). This journey is that gesture, and the outcome it witnesses is
``group-conversations`` 9, "You can leave a group yourself; the owner hands the
room over first".

**Three principals, three seats** — the ``owner``, a ``witness`` who stays,
and the ``leaver`` (the third seat) who walks out. Three rather than two for
the reason the hand-over journey gives: the point is that the roster moves on
somebody *else's* nest-side record with no act by them, and a party's own
projection proves less than a bystander's floor read.

**What the journey proves, in order:** the owner's own walk-out is refused —
the control is painted and dead, because a room is never owner-less; the
leaver's is live, and leaving it takes **one gesture in the app**, never an
API call standing in for the user (convention 8); the room's floor on the
nest then drops the leaver **with no act by any remaining member** — nobody
commits, nobody saves, nobody even has the room open; the leaver **keeps its
own copy** of the conversation, which is the ratified posture (deleting the
history would destroy the user's own copy of a conversation they were
legitimately part of); and — the declared residue, asserted rather than
wished away — the leaver **still hears** what the room says afterwards.

That last one is the honest shape of *a departure is not a mute*
(§ Roles and authorization → *Leaving — the mechanism*, the last bullet).
The floor severs what the FLOOR gates, and `channel.send` is not one of those
doors; the leaver's MLS leaf stays in the group until a remaining owner or
admin commits the Remove, because nothing can unseat its own committer. So
this journey asserts the residue POSITIVELY and is the alarm for it: it goes
red the day a remaining device learns to reconcile a departed floor row, which
is exactly when that goal-doc paragraph should be deleted.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state or over the nest's own roster read — never
"sleep N then assert". The residue assertion is anchored to an event rather
than to the clock: the witness must receive the later message first.
"""

from __future__ import annotations

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    bootstrap_room,
    room_app,
    room_seats,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
    wait_room_role,
    wait_snippet,
    wait_thread_by_channel,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS this one (`conversation-rooms.md` — the lead app paints every
    # new room gesture first). The six others join through the batched
    # trickle-down, each arriving with its own marker AND a
    # `helpers.room_seats.ROOM_APPS` entry (that tuple already lists all
    # seven). This journey's member seat is also the only witness that the
    # editor's door is LIVE for a plain member, with the greying per control
    # inside it.
    pytest.mark.tui,
    pytest.mark.windows,
]

LEAVE_BUDGET_S = MESSAGE_BUDGET_S + MEMBERSHIP_BUDGET_S
"""For the departure report to reach the room's home nest and be absorbed,
and for a following message to cross the channel to the seats still in it."""


def _roster_actors(port: int, reader: dict, channel: str) -> set[str] | None:
    """The room's live floor roster as ``reader`` is served it — the principals.

    ``None`` on a refusal rather than a raise, because the refusal is itself an
    answer worth polling through: the nest says "not a member of this room"
    both when ``reader`` is off a stored roster and when it holds no roster for
    the room at all, and a floor that has not been reported yet is the second.
    """
    try:
        reply = conv_api.room_list_roster(port, reader, channel)
    except Exception:
        return None
    return {m["principal"] for m in reply.get("members", [])}


@pytest.mark.feature("group-conversations")
def test_a_member_leaves_the_room_and_the_floor_drops_them(room_seats, nest_instance):
    """A member walks out from the app; the room's floor drops them with no act
    by anyone else; the leaver keeps its own copy and hears nothing further."""
    owner, witness, leaver = room_seats
    port = nest_instance["port"]

    group_id, channel = bootstrap_room(owner, witness, leaver)
    for member, who in ((witness, "witness"), (leaver, "leaver")):
        wait_thread_by_channel(member, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(member, channel, "hi room", what=f"'hi room' reaching the {who}")

    # ── the roles table: the owner may not walk out, the member may ──────
    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    assert owner_view.room["my_role"] == "Owner", owner_view.room
    assert not owner_view.capabilities["can_leave_room"], (
        "a room is never owner-less: the owner hands it over before leaving"
    )
    leaver_view = wait_room_role(leaver, channel, what="the leaver's room projection")
    assert leaver_view.room["my_role"] == "Member", leaver_view.room
    assert leaver_view.capabilities["can_leave_room"], leaver_view.capabilities

    # The owner's control is PAINTED and DEAD — greyed, never hidden.
    owner.app.conversations.open_thread_by_id(group_id)
    owner.app.conversations.open_room_settings()
    assert owner.app.driver.is_visible("room-leave-button"), (
        "the walk-out is painted for the owner too — greyed, never hidden\n"
        f"{seat_element(owner, 'room-leave-button')}"
    )
    assert not owner.app.conversations.room_leave_enabled(), (
        "and it is dead for the owner, whose remedy is the hand-over\n"
        f"{seat_element(owner, 'room-leave-button')}"
    )

    # ── the floor names all three before anybody leaves ──────────────────
    # A wait, not a read: the floor is a member-REPORTED mirror, so it fills
    # from the birth report and each membership commit's report rather than at
    # the moment the room exists.
    everyone = {owner.actor_hex, witness.actor_hex, leaver.actor_hex}
    wait_for(
        "the nest's floor roster naming every member before the departure",
        lambda: _roster_actors(port, owner.actor, channel),
        lambda names: names is not None and everyone <= names,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"floor={_roster_actors(port, owner.actor, channel)}",
    )

    # ── the departure: ONE gesture, in the app (convention 8) ────────────
    leaver_thread = thread_by_channel(leaver.app, channel)
    leaver.app.conversations.open_thread_by_id(leaver_thread.thread_id)
    leaver.app.conversations.open_room_settings()
    assert leaver.app.conversations.room_leave_enabled(), (
        f"an ordinary member's walk-out is live: "
        f"{leaver.app.driver.diagnose('room-leave-button')}"
    )
    leaver.app.conversations.leave_room()
    assert not leaver.app.has_error(), (
        f"a member's departure must not refuse: {leaver.app.error_text()!r}\n"
        f"{seat_story(leaver)}\n{seat_log(leaver, 'leave', 'roster', 'room')}"
    )

    # ── the floor drops them, with NO act by any remaining member ────────
    # Nobody saved anything, nobody committed, and neither remaining seat has
    # touched the room since the departure. The report the leaver sent IS the
    # whole mechanism.
    after = wait_for(
        "the room's floor dropping the leaver",
        lambda: _roster_actors(port, owner.actor, channel),
        lambda actors: actors is not None and leaver.actor_hex not in actors,
        LEAVE_BUDGET_S,
        diagnose=lambda: (
            f"floor still names the leaver ({leaver.actor_hex}); "
            f"{seat_story(leaver)}\n{seat_log(leaver, 'leave', 'roster', 'report')}"
        ),
    )
    assert owner.actor_hex in after and witness.actor_hex in after, (
        f"and nobody else moved — a departure is not a demotion: {sorted(after)}"
    )
    assert not owner.app.has_error(), (
        f"the owner did nothing and saw no error: {owner.app.error_text()!r}"
    )

    # The witness is served the same floor: the record is the room's, not a
    # per-reader view of it.
    assert leaver.actor_hex not in (_roster_actors(port, witness.actor, channel) or set()), (
        "the witness is served the same roster the owner is"
    )

    # ── the leaver keeps its own copy (ratified posture) ─────────────────
    kept = thread_by_channel(leaver.app, channel)
    assert kept is not None, (
        "the conversation stays in the leaver's own list — deleting the history "
        "would destroy the user's own copy of a conversation they were "
        "legitimately part of, and would not un-read a byte"
    )
    assert "hi room" in (kept.snippet or ""), (
        f"and what it already read is still there: {kept.snippet!r}"
    )

    # ── DECLARED RESIDUE: a departure is not a mute ──────────────────────
    # `conversation-rooms.md` § Roles and authorization → *Leaving — the
    # mechanism*, the last bullet. The floor severs what the FLOOR gates — the
    # custody serve door, the three binding-only relayed doors, the room-post
    # verdict read — and `channel.send` is not one of them. The leaver's leaf
    # is still in the MLS group (nothing can unseat its own committer), so it
    # can still decrypt what the room says until a remaining owner or admin
    # commits the Remove and re-keys.
    #
    # Asserted rather than wished away, and asserted POSITIVELY, so this test
    # is the alarm: it goes red the day a remaining device learns to reconcile
    # a departed floor row, which is exactly when that goal-doc paragraph
    # should be deleted. Anchored to an event, never to the clock (convention
    # 14): the witness must receive it first.
    owner.app.conversations.real_send(group_id, "after you left")
    wait_snippet(witness, channel, "after you left", what="the witness receiving the later message")
    heard = wait_for(
        "the leaver still hearing the room — the declared residue",
        lambda: thread_by_channel(leaver.app, channel),
        lambda t: t is not None and "after you left" in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            "DECLARED RESIDUE CHANGED: the leaver no longer hears the room after "
            "leaving. If a remaining member now commits the Remove on seeing a "
            "departed floor row, delete the residue paragraph in "
            "conversation-rooms.md § Roles and authorization → Leaving — the "
            "mechanism, and the twin unit pin "
            "a_remaining_members_next_commit_re_seats_the_leaver_until_the_leaf_goes.\n"
            f"{seat_story(leaver)}"
        ),
    )
    assert "after you left" in (heard.snippet or ""), heard.snippet

"""Tier_3 — the ownership transfer ceremony, end-to-end class, per app: three seats on one nest.

``conversation-rooms.md`` § Roles and authorization → *Ownership transfer*:
the owner's act posts an **offer** — the policy naming the successor,
countersigned by the outgoing owner and bound to the room — and the named
member's device signs the same policy and commits it, so every member admits
the change only with both signatures. Nothing changes hands until that commit
is folded, and the roles every seat then paints come off the **agreed group
context**, never off anything the owner's app told anyone.

**Three principals, three seats** — the ``owner``, the ``heir`` the room is
handed to, and a ``witness`` whose only job is to see the roles move on its
own chips: with two seats the witness would be one of the parties, and a
party's projection proves less than a bystander's. Every seat is a real
launch with its own MLS engine.

**What the journey proves, in order:** the owner stages the hand-over in the
policy editor (at most one row staged — staging a second un-stages the
first) and saves; the offer is a real record on the channel; the heir's
projection flips to ``Owner`` and its capabilities gain the owner-only pair;
the owner of yesterday flips to ``Member``, loses every governing capability
and finds the editor's door greyed; the witness's chips carry the moved roles;
and — the reason a transfer exists at all — the room's authorization root has
moved: the new owner removes the old one through the chip like any member,
and the witness's roster follows.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state (a role, a capability, a participant
count), with ``TRANSFER_BUDGET_S`` covering the offer crossing the channel,
the heir's device completing it on its next receive cycle, and the commit
crossing back.
"""

from __future__ import annotations

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    bootstrap_room,
    index_of,
    room_app,
    room_seats,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    thread_by_id,
    wait_for,
    wait_room_role,
    wait_snippet,
    wait_thread_by_channel,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads (`conversation-rooms.md` — "rendered on tui first"); the six
    # apps join through the batched trickle-down, each arriving with its own
    # marker AND a `helpers.room_seats.ROOM_APPS` entry. linux joined first,
    # then macos and ios (the apple leg), then windows, then web.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.web,
]

TRANSFER_BUDGET_S = MESSAGE_BUDGET_S + MEMBERSHIP_BUDGET_S
"""For the offer to cross the channel, the heir's receive loop (a 2 s tick
under the harness) to complete it with a gated commit, and that commit to
cross back to the other seats."""


def _open_chips(seat, thread_id: str, channel: str, *, what: str, live: bool = False):
    """Open ``thread_id`` on ``seat`` and wait for its chips to render the
    two-member roster (``live``: with the chips enabled — the removing seat's
    precondition)."""
    seat.app.conversations.open_thread_by_id(thread_id)
    return wait_for(
        what,
        lambda: thread_by_channel(seat.app, channel),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and seat.app.conversations.member_chip_count() >= 2
        and (not live or seat.app.driver.is_enabled("thread-member-chip")),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )


@pytest.mark.feature("group-conversations")
def test_the_owner_hands_the_room_over_and_the_root_moves(room_seats, nest_instance):
    """The owner stages a hand-over in the editor and saves; the heir's device
    completes the ceremony; every seat's projection follows the agreed group
    context; and the new owner can remove the old one."""
    owner, heir, witness = room_seats
    port = nest_instance["port"]

    group_id, channel = bootstrap_room(owner, heir, witness)
    for member, who in ((heir, "heir"), (witness, "witness")):
        wait_thread_by_channel(member, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(member, channel, "hi room", what=f"'hi room' reaching the {who}")

    # ── the capability is the owner's alone ──────────────────────────────
    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    assert owner_view.room["my_role"] == "Owner", owner_view.room
    assert owner_view.capabilities["can_transfer_ownership"], owner_view.capabilities
    heir_view = wait_room_role(heir, channel, what="the heir's room projection")
    assert heir_view.room["my_role"] == "Member", heir_view.room
    assert not heir_view.capabilities["can_transfer_ownership"], heir_view.capabilities

    # ── the owner stages the hand-over in the editor, one row at a time ──
    owner_open = _open_chips(owner, group_id, channel, what="the owner's chips rendering the roster")
    heir_index = index_of(owner_open, heir.actor_hex)
    witness_index = index_of(owner_open, witness.actor_hex)
    owner.app.conversations.open_room_settings()
    assert owner.app.driver.is_enabled("room-owner-transfer-button"), (
        f"the hand-over control is live for the owner: "
        f"{owner.app.driver.diagnose('room-owner-transfer-button')}"
    )
    assert not owner.app.conversations.room_owner_transfer_staged(heir_index)
    owner.app.conversations.toggle_room_owner_transfer(witness_index)
    assert owner.app.conversations.room_owner_transfer_staged(witness_index)
    owner.app.conversations.toggle_room_owner_transfer(heir_index)
    assert owner.app.conversations.room_owner_transfer_staged(heir_index)
    assert not owner.app.conversations.room_owner_transfer_staged(witness_index), (
        "at most one row is staged: staging the heir un-stages the witness"
    )
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    owner.app.conversations.save_room_settings()
    assert not owner.app.has_error(), (
        f"the owner's hand-over must not refuse: {owner.app.error_text()!r}"
    )
    assert owner.app.driver.is_absent("room-settings-save-button"), (
        "the editor closes once the offer is on the channel\n"
        f"{seat_element(owner, 'room-settings-save-button')}\n"
        f"{seat_story(owner)}\n{seat_log(owner, 'room settings', 'room_settings', 'policy', 'ownership')}"
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) > envelopes_before, (
        "the offer is a real record on the channel"
    )

    # ── the heir's device completes the ceremony ─────────────────────────
    heir_owner = wait_for(
        "the heir's projection flipping to Owner",
        lambda: thread_by_channel(heir.app, channel),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Owner",
        TRANSFER_BUDGET_S,
        diagnose=lambda: seat_story(heir),
    )
    caps = heir_owner.capabilities
    assert caps["can_transfer_ownership"] and caps["can_appoint_admins"] and caps["can_set_policy"], (
        f"the new owner holds the owner-only capabilities: {caps}"
    )
    assert heir_owner.room["policy"]["version"] == owner_view.room["policy"]["version"] + 1, (
        f"the hand-over is one policy version: {heir_owner.room} vs {owner_view.room}"
    )

    # ── the owner of yesterday is a plain member now ─────────────────────
    owner_after = wait_for(
        "the old owner's projection flipping to Member",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Member",
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    caps = owner_after.capabilities
    assert not caps["can_transfer_ownership"] and not caps["can_set_policy"], caps
    assert not caps["can_appoint_admins"] and not caps["can_remove_members"], caps
    assert owner.app.driver.is_visible("thread-room-settings-button"), (
        "the editor's door stays painted (greyed, never hidden)\n"
        # Rule 6 — classify the failure without a re-run. The three neighbours
        # split the ways this can go wrong: the whole detail pane gone (the
        # rename button, painted on every open thread, absent too), the room
        # gone from the detail projection (the class statement absent, the
        # rename button present), or just this one control mispainted. The
        # editor's Save button says whether the overlay this seat had open
        # really closed.
        + "\n".join(
            seat_element(owner, e)
            for e in (
                "thread-room-settings-button",
                "thread-room-class",
                "thread-header",
                "thread-member-chip",
                "thread-rename-button",
                "room-settings-save-button",
            )
        )
        + f"\n{seat_story(owner)}"
    )
    # The DOOR stays live, and that is not a regression: any member of a room
    # opens this surface, because it carries the walk-out (widened 2026-09-20,
    # user-approved; `ui/conversations.md` § Element IDs). The greying moved
    # INSIDE, per control. What tracks the demotion here is therefore the
    # capability pair asserted above — `can_set_policy` is already False for
    # the owner of yesterday — and the per-control greying is witnessed by
    # `test_conversation_room_leave.py`, whose member seat opens this editor
    # and finds only the walk-out live. Opening it here would strand an
    # overlay this suite has no dismiss id for (Esc is the human-only cancel).
    assert owner.app.driver.is_enabled("thread-room-settings-button"), (
        "the door opens for any member of a room — the greying is per control "
        "inside it\n"
        f"{seat_element(owner, 'thread-room-settings-button')}"
    )

    # ── the witness paints the moved roles off its own group context ─────
    witness_after = wait_for(
        "the witness's roles following the hand-over",
        lambda: thread_by_channel(witness.app, channel),
        lambda t: t is not None
        and t.room is not None
        and heir.actor_hex in t.participant_actor_ids
        and owner.actor_hex in t.participant_actor_ids
        and t.room["member_roles"][t.participant_actor_ids.index(heir.actor_hex)] == "Owner"
        and t.room["member_roles"][t.participant_actor_ids.index(owner.actor_hex)] == "Member",
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(witness),
    )
    _open_chips(witness, witness_after.thread_id, channel, what="the witness's chips rendering the roster")
    assert (
        witness.app.driver.get_attr("thread-member-chip", "role", index=index_of(witness_after, heir.actor_hex))
        == "owner"
    ), f"the heir's chip carries the owner role: {witness.app.driver.diagnose('thread-member-chip')}"
    assert (
        witness.app.driver.get_attr("thread-member-chip", "role", index=index_of(witness_after, owner.actor_hex))
        == "member"
    ), f"the old owner's chip carries the member role: {witness.app.driver.diagnose('thread-member-chip')}"

    # ── the root moved: the new owner removes the old one like any member ─
    heir_open = _open_chips(
        heir, heir_owner.thread_id, channel, what="the new owner's chips rendering live", live=True
    )
    heir.app.conversations.remove_member(index_of(heir_open, owner.actor_hex))
    after = wait_for(
        "the new owner's roster dropping the owner of yesterday",
        lambda: thread_by_channel(heir.app, channel),
        lambda t: t is not None and t.participant_count == 1,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(heir),
    )
    assert owner.actor_hex not in after.participant_actor_ids, after.participant_actor_ids
    assert not heir.app.has_error(), heir.app.error_text()
    wait_for(
        "the witness's roster following the removal",
        lambda: thread_by_channel(witness.app, channel),
        lambda t: t is not None and t.participant_count == 1,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(witness),
    )

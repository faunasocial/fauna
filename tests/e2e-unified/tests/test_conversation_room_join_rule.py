"""Tier_3 — the room's join rule decides who may bring people in.

``docs/features/group-conversations.md`` outcome 12, whose promise
``conversation-rooms.md`` § Join rules and invites owns: under ``invite`` only
the owner and admins may invite; under ``member-invite`` **any member may**.

**What nothing drove before this file.** ``test_conversation_room_roles.py``
already asserts the *default* half — a plain member's ``can_invite`` is false
while the room carries the ``invite`` rule it is born with. The outcome is
about the CHANGE: the owner picks a different rule in the room-settings editor,
and the plain member's add-person control follows. That is the arm this journey
drives, end to end, with the member's own add landing as a real Commit.

**The member's add is the member's own engine's act.** The three seats are
three real launches with three MLS engines (``helpers.room_seats``), so "the
member may now add" is that seat's engine authoring the Add and the owner's
engine folding it — never one process holding both identities. The fourth
principal the member brings in is an API-tier actor with key packages
published, exactly as ``test_fauna_mls_real_roundtrip`` mints its peers: it has
to be reachable for a real Welcome, and it needs no app of its own to prove who
authored the Commit.

**Greyed, never hidden** (``conversations.md`` § Architectural rules 5): before
the change the member's ``thread-add-participant-button`` is painted and dead,
and the automation gate's named 409 is that deadness (convention 11). After the
change the same control is live. Asserting both sides of one control is what
makes this a witness of the *rule* rather than of a capability flag.

tui LEADS this one (``conversation-rooms.md`` — the lead app paints every new
room gesture first); the six others join through the batched trickle-down, each
arriving with its own marker AND a ``helpers.room_seats.ROOM_APPS`` entry.

Latency-independent throughout (convention 14): every wait polls for merged
state to *become* what the act should make it — a capability flipping in the
seat's own projection, a roster count growing — and no step sleeps a span.
"""

from __future__ import annotations

import pytest

from common import create_actor_and_register
from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    bootstrap_room,
    room_app,
    room_seats,
    seat_element,
    seat_story,
    thread_by_channel,
    wait_for,
    wait_room_role,
    wait_snippet,
    wait_thread_by_channel,
)
from tests.api import conv_api
from tests.api.conv_api import mint_key_packages as _mint_keypackages

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    pytest.mark.tui,
]


@pytest.mark.feature("group-conversations")
def test_the_join_rule_decides_whether_a_plain_member_may_invite(
    room_seats, nest_instance
):
    """Under the born ``invite`` rule the member's add is greyed and refused;
    the owner picks ``member-invite`` in the editor; the member's own add then
    lands as a real Commit and the newcomer is on the room's roster."""
    owner, member, witness = room_seats
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    group_id, channel = bootstrap_room(owner, member, witness)
    for seat, who in ((member, "member"), (witness, "witness")):
        wait_thread_by_channel(seat, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(seat, channel, "hi room", what=f"'hi room' reaching the {who}")

    # The newcomer the member will bring in: an API-tier actor with key
    # packages published (a real Welcome needs one) that has accepted the
    # MEMBER, since it is the member who will initiate (`direct-messages.md`
    # § Reach policy — the invite is initiation, mediated by the recipient's
    # inbox mode, whose default `allow_knock` would otherwise refuse).
    newcomer = create_actor_and_register(port, admin_signing_key=admin_sk)
    conv_api.keypackage_upload(
        port, newcomer, _mint_keypackages(bytes(newcomer["signing_key"]), 3)
    )
    conv_api.accept_contact(port, newcomer, member.actor_hex)

    # ── the born rule: `invite`, and the member may not ──────────────────
    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    assert owner_view.room["policy"]["join_rule"] == "Invite", (
        f"a room born from the fork carries the initial policy: {owner_view.room}"
    )
    assert owner_view.capabilities["can_invite"], (
        f"the owner always may: {owner_view.capabilities}"
    )
    member_view = wait_room_role(member, channel, what="the member's room projection")
    assert member_view.room["my_role"] == "Member", member_view.room
    assert not member_view.capabilities["can_invite"], (
        f"under `invite` a plain member may not: {member_view.capabilities}"
    )

    # Painted and dead — the observable of "refused client-side".
    member.app.conversations.open_thread_by_id(member_view.thread_id)
    assert member.app.driver.is_visible("thread-add-participant-button"), (
        "the add-person control is greyed, never hidden "
        f"(`conversations.md` § Architectural rules 5):\n"
        f"{seat_element(member, 'thread-add-participant-button')}"
    )
    assert not member.app.driver.is_enabled("thread-add-participant-button"), (
        f"and dead under `invite`:\n"
        f"{seat_element(member, 'thread-add-participant-button')}"
    )
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    with pytest.raises(RuntimeError, match="disabled"):
        # The automation gate's named 409 (convention 11) — the client side of
        # "a plain member's Add is never authored under `invite`".
        member.app.conversations.add_participant_to_thread(newcomer["actor_id_hex"])
    assert not member.app.has_error(), (
        f"a refused actuation never reaches the manager, so no page error: "
        f"{member.app.error_text()!r}"
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) == envelopes_before, (
        "no Commit was posted for the refused add"
    )

    # ── the owner picks `member-invite` in the editor ────────────────────
    owner.app.conversations.open_thread_by_id(group_id)
    owner.app.conversations.open_room_settings()
    owner.app.conversations.set_room_join_rule("member-invite")
    owner.app.conversations.save_room_settings()
    assert not owner.app.has_error(), (
        f"the owner's policy edit must not refuse: {owner.app.error_text()!r}"
    )
    assert owner.app.driver.is_absent("room-settings-save-button"), (
        "the editor closes once the policy commit landed\n"
        f"{seat_element(owner, 'room-settings-save-button')}\n{seat_story(owner)}"
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) > envelopes_before, (
        "the join-rule change is a real policy commit on the channel"
    )

    # The member learns the new rule from the agreed group context, not from
    # anything the owner's app told it.
    member_after = wait_for(
        "the member's projection carrying the new join rule",
        lambda: thread_by_channel(member.app, channel),
        lambda t: t is not None
        and ((t.room or {}).get("policy") or {}).get("join_rule") == "MemberInvite",
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert member_after.capabilities["can_invite"], (
        f"under `member-invite` any member may: {member_after.capabilities}"
    )
    assert not member_after.capabilities["can_remove_members"], (
        f"and the rule widens invites ALONE — the roles table is untouched: "
        f"{member_after.capabilities}"
    )

    # ── the member's own add lands ───────────────────────────────────────
    # The member's seat is still ON the thread it opened above — the refused
    # add raised at the disabled control's own click, so nothing navigated
    # away. Re-opening here would be worse than redundant: `open_thread_by_id`
    # re-navigates to the conversations LIST first, which a seat already inside
    # a thread detail does not paint on every app.
    assert member.app.driver.is_visible("thread-header"), (
        f"the member's seat stayed on the room through the refusal:\n"
        f"{seat_element(member, 'thread-header')}"
    )
    wait_for(
        "the member's add-person control going live off the new rule",
        lambda: member.app.driver.is_enabled("thread-add-participant-button"),
        lambda live: live,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_element(member, "thread-add-participant-button"),
    )
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    member.app.conversations.add_participant_to_thread(newcomer["actor_id_hex"])
    assert not member.app.has_error(), (
        f"the member's add must not refuse: {member.app.error_text()!r}"
    )

    grown = wait_for(
        "the member's own roster carrying the newcomer",
        lambda: thread_by_channel(member.app, channel),
        lambda t: t is not None and newcomer["actor_id_hex"] in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) > envelopes_before, (
        "the member's add posts a real Commit on the channel"
    )
    # The owner folds the MEMBER's commit — the roster grew for everyone, which
    # is what makes this the room's rule rather than one seat's optimism.
    wait_for(
        "the owner folding the member's Add",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None and newcomer["actor_id_hex"] in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    assert len(grown.participant_actor_ids) == 3, (
        f"three counterparts on the member's own seat: {grown.participant_actor_ids}"
    )

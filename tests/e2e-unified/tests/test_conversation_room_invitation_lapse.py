"""Tier_3 — a room invitation is judged again when it is accepted: three seats on one nest.

``room-invitations.md`` § Join rules and invites → *An invitation is a
standing offer, judged again when it is accepted*: an invitation is good only
while its inviter could still issue it, so the accept door runs the invite
door's own judgement again, and a refusal CONSUMES the invitation (*A lapse
consumes the invitation*) — the app's list drops it in the act that refused
it (``ConversationsManager::accept_room_invitation`` re-lists the standing
invitations on a refused accept). The nest's half is pinned at the API tier
(``conformance_conversation_rooms.rs``) and the app's half in Rust
(``a_lapsed_invitation_leaves_the_list_when_its_accept_is_refused``); this
journey is the two together, driven the way a user drives them.

**Three principals, three seats** (``helpers.room_seats.room_seats``): the
``owner`` founds a community room; the ``admin`` joins it, is appointed admin
by the owner and invites the ``invitee``; the owner then demotes the admin;
the invitee accepts. The accept is refused, the refusal is on the invitee's
``error-message``, the invitation leaves the invitee's list, and no room opens
on the invitee's seat.

**One arm of three.** The same ruling lapses an invitation whose inviter has
since LEFT (or been removed from) the room, and one whose inviter identity has
since been HANDED ON (succeeded — judged by its own seat, never through its
succession line). The demoted-admin arm is the witness for the app path,
which is one code path for all three: the refusal, the re-list, the discarded
thread. The other two arms are pinned at the API tier
(``…_from_a_member_who_has_left_…``,
``an_invitation_from_an_identity_since_succeeded_lapses_at_accept``).

**Latency-independent by construction** (convention 14): the demotion is
proven to have reached the room's floor before the accept — the owner's
editor marks the pending invitation ``lapsed`` (``room.list_invites`` runs the
accept door's own judgement) — so the accept cannot race the demotion; every
wait is a deadline poll over projected state, never "sleep N then assert".
"""

from __future__ import annotations

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    found_community_room,
    index_of,
    room_app,
    room_seats,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads (the lead app); the six others join through the batched
    # trickle-down, each arriving with its own marker AND a
    # `helpers.room_seats.ROOM_APPS` entry.
    pytest.mark.tui,
]


@pytest.mark.feature("group-conversations")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_an_invitation_from_an_admin_since_demoted_lapses_at_accept(room_seats, nest_instance):
    """An admin invites, the owner demotes the admin, the invitee accepts:
    refused, the invitation leaves the list, and no room opens."""
    owner, admin, invitee = room_seats
    port = nest_instance["port"]
    # Setup (convention 8 carve-out (b)): the invitee admits the admin as a
    # contact, so the admin's invitation passes the invitee's reach policy
    # under the default `allow_knock` inbox mode, as the fixture's members do
    # for the owner.
    conv_api.accept_contact(port, invitee.actor, admin.actor_hex)

    # ── the owner founds a community room and seats the admin-to-be ──────
    room_id, owner_thread_id, admin_thread_id = found_community_room(owner, admin, "hello room")
    conv = owner.app.conversations
    # `thread-member-chip[i]` paints the OTHER members only (index-parallel to
    # `participant_actor_ids`, which never lists the viewer), so with the
    # invitee not yet seated the owner's editor carries exactly one chip.
    owner_view = wait_for(
        "the owner's chips painting the admin-to-be",
        lambda: thread_by_channel(owner.app, room_id),
        lambda t: t is not None
        and admin.actor_hex in t.participant_actor_ids
        and conv.member_chip_count() >= len(t.participant_actor_ids),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(owner)}\n{seat_element(owner, 'thread-member-chip')}",
    )
    admin_index = index_of(owner_view, admin.actor_hex)

    # ── the owner appoints the admin ──────────────────────────────────────
    conv.open_room_settings()
    conv.toggle_room_admin(admin_index)
    assert conv.room_admin_staged(admin_index), seat_element(owner, "room-admin-toggle")
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not owner.app.has_error(), (
        f"the owner's appointment must not refuse: {owner.app.error_text()!r}"
    )
    wait_for(
        "the admin's projection flipping to Admin",
        lambda: thread_by_channel(admin.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Admin",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(admin)}\n{seat_log(admin, 'policy', 'role')}",
    )

    # ── the admin invites the invitee ─────────────────────────────────────
    # On a community room the add-participant overlay issues an invitation
    # and seats nobody.
    admin.app.conversations.open_thread_by_id(admin_thread_id)
    admin.app.conversations.add_participant_to_thread(invitee.actor_hex)
    assert not admin.app.has_error(), (
        f"the admin's invitation must not refuse: {admin.app.error_text()!r}"
    )
    wait_for(
        "the admin's invitation standing on the invitee's list",
        lambda: invitee.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(invitee)}\n{seat_log(invitee, 'invitation', 'room')}",
    )

    # ── the owner demotes the admin ───────────────────────────────────────
    conv.open_thread_by_id(owner_thread_id)
    conv.open_room_settings()
    assert conv.room_admin_staged(admin_index), (
        f"the editor opens with the admin staged as admin: {seat_element(owner, 'room-admin-toggle')}"
    )
    conv.toggle_room_admin(admin_index)
    assert not conv.room_admin_staged(admin_index), seat_element(owner, "room-admin-toggle")
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not owner.app.has_error(), (
        f"the owner's demotion must not refuse: {owner.app.error_text()!r}"
    )
    wait_for(
        "the demoted admin's projection falling back to a plain member",
        lambda: thread_by_channel(admin.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Member",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(admin)}\n{seat_log(admin, 'policy', 'role')}",
    )

    # The barrier: the room's floor judges the invitation lapsed BEFORE the
    # accept is pressed — the owner's pending list runs the accept door's own
    # judgement (`room.list_invites`), so this accept cannot race the demotion.
    conv.open_room_settings()
    wait_for(
        "the owner's editor marking the demoted admin's invitation as lapsed",
        lambda: conv.pending_invite_count() >= 1 and conv.pending_invite_lapsed(0),
        lambda lapsed: lapsed is True,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"{seat_element(owner, 'room-pending-invite')}\n{seat_story(owner)}\n"
            f"{seat_log(owner, 'pending', 'invite')}"
        ),
    )
    # Nothing is staged, so Save is the editor's plain close.
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)

    # ── the invitee accepts: refused, and the invitation leaves the list ─
    assert invitee.app.conversations.room_invitation_count() >= 1, (
        "a lapse is consumed at accept, never before: the invitation still stands\n"
        f"{seat_story(invitee)}"
    )
    invitee.app.conversations.accept_room_invitation(0)
    refusal_prefix = S.conversations.unified.error_room_invitation(message="")
    error = wait_for(
        "the refused accept landing on the invitee's error-message",
        lambda: invitee.app.error_text() if invitee.app.has_error() else "",
        lambda text: bool(text),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(invitee)}\n{seat_log(invitee, 'invitation', 'room')}",
    )
    assert error.startswith(refusal_prefix), (
        f"the refusal names the invitation: {error!r} (expected the prefix {refusal_prefix!r})"
    )
    wait_for(
        "the lapsed invitation leaving the invitee's list",
        lambda: invitee.app.conversations.room_invitation_count(),
        lambda n: n == 0,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(invitee)}\n{seat_log(invitee, 'invitation', 'room')}",
    )
    assert thread_by_channel(invitee.app, room_id) is None, (
        "a refused accept opens no room: the thread made for it is discarded\n"
        f"{seat_story(invitee)}"
    )
    # That the lapse consumed the row and its envelope at the room's home is
    # the nest's half, pinned at the API tier
    # (`an_invitation_from_an_admin_since_demoted_lapses_at_accept`): an empty
    # pending list read here could equally be one not yet served.

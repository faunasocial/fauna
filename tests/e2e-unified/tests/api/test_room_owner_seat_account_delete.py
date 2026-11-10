"""tier_3 API: deleting your account is refused while you hold a community
room's owner seat a local member could take over (group-conversations
outcome 35).

Goal doc: ``docs/goal/behavior/conversation-rooms.md`` § Roles and
authorization, *An owner's account deletion* — a user's own deletion is
refused while they hold a community room's owner seat and another live user
member homed on the room's home nest could take it over, at the scheduling
door and again when the delayed deletion executes; with nobody who could take
over, the deletion proceeds.

The nest half is ``account_handlers.rs`` (the room-ownership gate,
``fauna.account.room_ownership_held``) and its executor re-check
``pending_actions::finalize_self_deletion``; these tests observe both from
outside over a real nest binary, through the doors a user's app calls:
``room.create`` / ``room.invite`` / ``room.accept_invite`` (via
``helpers/community_room.py``), ``account.delete``, and the
``pending_actions/run_due`` test hook, which runs the production executor.

Latency-independent (convention 14): every step is a request with a reply and
every assertion reads returned state.
"""

from __future__ import annotations

import pytest
from clients.ws_rpc_admin_client import RpcCallError

from conftest import _make_user
from helpers.admin_wire import run_due_pending_actions
from helpers.community_room import call, found_room, seat_member
from helpers.family_wire import actor_ws
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


def _delete(nest: dict, actor: dict) -> dict:
    with actor_ws(nest, actor) as ws:
        return ws.call("fauna.account.delete", {})


def _owner_principals(room) -> set[str]:
    members = conv_api.room_list_roster(room.nest["port"], room.owner, room.room_hex)["members"]
    return {m.get("principal") for m in members if m.get("role") == "owner"}


@pytest.mark.feature("group-conversations")
def test_an_owner_with_a_local_member_who_could_take_over_cannot_delete_the_account(
    delete_account_nest,
):
    """The owner of a community room with a second member homed on the same
    nest asks to delete the account: refused at the scheduling door, naming the
    room-ownership reason. The control is an owner of a room with nobody to
    hand it to (only the home nest beside them): that owner's deletion is
    scheduled, so the refusal is the takeable seat's doing."""
    nest = delete_account_nest
    owner = nest["user"]
    room = found_room(nest, owner=owner)
    seat_member(room, _make_user(nest))

    with pytest.raises(RpcCallError) as refused:
        _delete(nest, owner)
    assert refused.value.code == "fauna.account.room_ownership_held", refused.value
    assert owner["actor_id_hex"] in _owner_principals(room), "the owner seat is untouched"

    lone_owner = _make_user(nest)
    found_room(nest, owner=lone_owner)
    scheduled = _delete(nest, lone_owner)
    assert scheduled.get("status") == "pending", (
        f"an owner nobody could take over from is let go: {scheduled!r}"
    )


@pytest.mark.feature("group-conversations")
def test_a_scheduled_deletion_is_refused_again_when_a_local_member_sat_down_meanwhile(
    delete_account_nest,
):
    """An owner with nobody to hand the room to schedules the deletion — let
    go at the door — and a local member then sits down. When the cool-off runs
    out the executor re-checks and does not delete: the owner's account still
    answers and still holds the owner seat."""
    nest = delete_account_nest
    owner = nest["user"]
    room = found_room(nest, owner=owner)

    scheduled = _delete(nest, owner)
    assert scheduled.get("status") == "pending", scheduled

    seat_member(room, _make_user(nest))
    ran = run_due_pending_actions(nest)
    assert ran.get("made_due", 0) >= 1, f"the scheduled deletion was brought due: {ran!r}"

    assert owner["actor_id_hex"] in _owner_principals(room), (
        "the executor re-checked the takeable seat and kept the account and its owner seat"
    )
    reply = call(nest, owner, "fauna.conversations.room.list_roster", {"room_id": room.room_hex})
    assert reply.get("members"), "the owner's account still answers"

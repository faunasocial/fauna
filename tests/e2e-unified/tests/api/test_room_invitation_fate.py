"""tier_3 API: an invitation shares its invitee's fate (group-conversations
outcome 28).

Goal doc: ``docs/goal/behavior/room-invitations.md`` § Join rules and invites,
*An invitation shares its invitee's fate, as one thing* — account deletion
removes the invitation row (with its inbox envelope and quota charge), and an
identity succession carries it to the successor, who accepts under its own key.

The Rust tests ``db::rooms::tests::deleting_an_account_removes_the_invitations_addressed_to_it``
and ``successions::tests::a_pending_room_invitation_follows_the_account_with_its_envelope``
pin the store half; these two witness it over a real nest binary through the
doors a user's app calls: ``room.invite``, ``account.delete`` (its 14-day
cool-off brought due by the ``pending_actions/run_due`` test hook, which runs
the production executor), the succession ceremony, ``room.list_invites`` and
``room.accept_invite``.

Latency-independent (convention 14): every step is a request with a reply, and
every assertion reads returned state.
"""

from __future__ import annotations

import secrets

import pytest
from nacl.signing import SigningKey

import fauna_ffi
from conftest import _make_user
from helpers.admin_wire import run_due_pending_actions
from helpers.community_room import call, found_room, secret_of
from helpers.family_wire import actor_ws
from helpers.succession import register_recovery_kit, succeed_identity
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


def _invite(room, invitee: dict) -> None:
    """The owner invites ``invitee`` as a member, the way `seat_member` does —
    the invitee first accepts the owner as a contact — but stops short of the
    accept, leaving the invitation pending."""
    conv_api.accept_contact(room.nest["port"], invitee, room.owner["actor_id_hex"])
    invite = fauna_ffi.sign_room_member_invite(
        secret_of(room.owner), room.room_id, bytes.fromhex(invitee["actor_id_hex"]), 1
    )
    call(room.nest, room.owner, "fauna.conversations.room.invite", {"invite": invite, "invitee_node": ""})


def _pending_invitees(room) -> set[str]:
    """The invitee ids of the room's pending invitations, as the owner is
    served them (the owner sees every pending invitation)."""
    reply = call(
        room.nest, room.owner, "fauna.conversations.room.list_invites", {"room_id": room.room_hex}
    )
    return {row["invitee"] for row in reply["invites"]}


def _roster_principals(room) -> set[str]:
    members = conv_api.room_list_roster(room.nest["port"], room.owner, room.room_hex)["members"]
    return {m.get("principal") for m in members}


@pytest.mark.feature("group-conversations")
def test_deleting_an_account_removes_the_room_invitations_addressed_to_it(delete_account_nest):
    """The invitee deletes their account; once the deletion runs, the room's
    pending invitations no longer name them. A second invitee who stays is the
    control: the list still serves their invitation, so the absence is the
    deletion, not an empty read."""
    nest = delete_account_nest
    leaving = nest["user"]
    staying = _make_user(nest)
    room = found_room(nest)

    _invite(room, leaving)
    _invite(room, staying)
    assert _pending_invitees(room) == {leaving["actor_id_hex"], staying["actor_id_hex"]}, (
        "both invitations are pending before the deletion"
    )

    with actor_ws(nest, leaving) as ws:
        scheduled = ws.call("fauna.account.delete", {})
    assert scheduled.get("status") == "pending", scheduled
    ran = run_due_pending_actions(nest)
    assert ran.get("executed", 0) >= 1, f"the deletion ran: {ran!r}"

    assert _pending_invitees(room) == {staying["actor_id_hex"]}, (
        "the deleted account's invitation is gone; the other invitee's stays"
    )


@pytest.mark.feature("group-conversations")
def test_a_pending_room_invitation_follows_an_account_its_owner_takes_back(nest_instance):
    """The invitee's owner takes the account back through the succession
    ceremony before accepting. The pending invitation now names the successor,
    not the retired key, and the successor accepts it under its own key and is
    seated on the room's floor."""
    nest = nest_instance
    # Dedicated actors: a succession revokes every session of the old identity.
    invitee = _make_user(nest)
    room = found_room(nest)

    _invite(room, invitee)
    assert invitee["actor_id_hex"] in _pending_invitees(room), "the invitation is pending"

    old_seed = bytes(invitee["signing_key"])
    new_seed = secrets.token_bytes(32)
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=invitee["actor_id_hex"], identity_seed_hex=old_seed.hex()
    )
    successor_hex = succeed_identity(
        nest["url"],
        old_actor_id_hex=invitee["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=new_seed.hex(),
        old_seed_hex=old_seed.hex(),
    )
    assert successor_hex == SigningKey(new_seed).verify_key.encode().hex()

    pending = _pending_invitees(room)
    assert successor_hex in pending, f"the invitation follows the account to its successor: {pending!r}"
    assert invitee["actor_id_hex"] not in pending, "no invitation is left on the retired key"

    successor = {
        "actor_id_hex": successor_hex,
        "actor_id_bytes": bytes.fromhex(successor_hex),
        "signing_key": new_seed,
    }
    call(
        nest,
        successor,
        "fauna.conversations.room.accept_invite",
        {"room_id": room.room_hex, "reception_pubkey": fauna_ffi.mint_room_reception_pubkey()},
    )
    assert successor_hex in _roster_principals(room), "the successor accepted and is seated"
    assert successor_hex not in _pending_invitees(room), "an accepted invitation is no longer pending"

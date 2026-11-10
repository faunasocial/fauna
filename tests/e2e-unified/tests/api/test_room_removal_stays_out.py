"""Once removed, a member stays out — seen from outside with no app driver
(``docs/features/group-conversations.md``, the ``[nest]`` outcome 30).

The outcome has three clauses, one test each:

* **No report of theirs re-seats them** — the roster ratchet
  (``docs/goal/behavior/conversation-rooms.md`` § The floor roster): once an
  end-to-end room has a floor roster, ``room.roster_report`` is admitted only
  from a live member of the stored roster it replaces, so a removed member's
  report naming itself is refused however it is shaped, while a live member's
  honest report re-seats them.
* **Nothing rewrites a community room's roster** — the same door's provenance
  gate: a room founded by the create ceremony carries a birth record, its
  floor IS the membership authority, and a member report against it is
  refused from anyone, its owner included.
* **A member on another nest who is removed loses the roster, the member list
  and the attachment door until honestly re-added**
  (``docs/goal/behavior/community-rooms.md`` § Implementation status today →
  *A removal severs*): ``room.remove`` purges the foreign member's binding with
  the seat, so the three relayed doors that read that binding alone —
  ``room.list_roster_remote``, ``channel.actors_remote`` and
  ``blob.write_token.get`` — are refused through the member's own nest, while a
  co-member from the same nest keeps all three; a fresh invitation and its
  relayed accept write the binding back and the doors open again.

The tier_1 pins are ``bins/fauna-nest/src/conversations_handlers.rs``'s
``room_roster_report_apply`` gates and
``bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs``
(``a_removed_foreign_members_relayed_roster_read_and_mint_die_with_the_binding``);
these are their outside-observed twins over real nest binaries — two of them,
each serving TLS under its own loopback handle domain, for the cross-nest
clause, so every relayed call crosses a real federation channel.

Every absence is anchored on a presence read first from the same door
(convention 14): each refusal is preceded by the same call served.
"""

from __future__ import annotations

import os
import secrets

import pytest
from clients.ws_rpc_admin_client import RpcCallError

from conftest import _make_user, provision_foreign_handled_actor
from helpers.community_room import call, found_room, remove_member, seat_foreign_member, seat_member
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


# ── helpers ──────────────────────────────────────────────────────────────────


def _report(nest: dict, actor: dict, room_hex: str, members: list[dict]) -> dict:
    """``room.roster_report`` as ``actor``: the roster ``members`` (actor dicts)
    with no roles — an end-to-end room whose floor names no owner."""
    return call(
        nest,
        actor,
        "fauna.conversations.room.roster_report",
        {"room_id": room_hex, "members": [{"actor": m["actor_id_hex"]} for m in members]},
    )


def _refused(fn) -> RpcCallError:
    with pytest.raises(RpcCallError) as caught:
        fn()
    return caught.value


def _seated(nest: dict, reader: dict, room_hex: str) -> set[str]:
    """The live floor's principals as ``reader`` reads them."""
    return {m["principal"] for m in conv_api.room_list_roster(nest["port"], reader, room_hex)["members"]}


def _join_routing_roster(nest: dict, actor: dict, channel_hex: str) -> None:
    """``actor`` sends one structurally real envelope to the channel, which
    self-registers it on the channel's routing roster — the liveness gate the
    report door stacks under the ratchet."""
    throwaway_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, envelope = conv_api.mint_group_welcome_with_message(
        bytes(actor["signing_key"]), throwaway_kp, "on the routing roster"
    )
    assert conv_api.channel_send(nest["port"], actor, channel_hex, envelope) >= 1


def _three_doors(away: dict, member: dict, room) -> dict[str, str]:
    """The three relayed doors that read the foreign-member binding alone, as
    ``member`` calls them through its own nest ``away``. Each maps to
    ``"served"`` or the refusal's wire code, so one read names whichever
    drifted."""
    home_url = room.nest["url"]
    doors = {
        "roster": ("fauna.conversations.room.list_roster_remote", {"room_id": room.room_hex, "nest_url": home_url}),
        "member list": (
            "fauna.conversations.channel.actors_remote",
            {"channel_id": room.room_hex, "nest_url": home_url},
        ),
        "attachment door": (
            "fauna.conversations.blob.write_token.get",
            {"channel_id": room.room_hex, "nest_url": home_url},
        ),
    }
    out = {}
    for name, (kind, payload) in doors.items():
        try:
            call(away, member, kind, payload)
            out[name] = "served"
        except RpcCallError as e:
            out[name] = e.code
    return out


ALL_SERVED = {"roster": "served", "member list": "served", "attachment door": "served"}


# ── clause 1: the ratchet ────────────────────────────────────────────────────


@pytest.mark.feature("group-conversations")
def test_a_removed_members_own_report_never_reseats_them(nest_instance):
    """Outcome 30, first clause: in an end-to-end room, A reports the roster
    ``{A, B}``, then ``{A}``; B — still on the routing roster, the row a
    departure leaves — reports ``{A, B}`` and then ``{B}``, and both are refused
    with the floor unchanged; A's honest ``{A, B}`` re-seats B."""
    nest = nest_instance
    alice, bob = _make_user(nest), _make_user(nest)
    room_hex = secrets.token_bytes(32).hex()
    for who in (alice, bob):
        _join_routing_roster(nest, who, room_hex)

    _report(nest, alice, room_hex, [alice, bob])
    both = {alice["actor_id_hex"], bob["actor_id_hex"]}
    assert _seated(nest, alice, room_hex) == both, "the first report bootstrapped the floor"

    _report(nest, alice, room_hex, [alice])
    assert _seated(nest, alice, room_hex) == {alice["actor_id_hex"]}, "A's report removed B"

    for shape in ([alice, bob], [bob]):
        err = _refused(lambda shape=shape: _report(nest, bob, room_hex, shape))
        assert "permission_denied" in err.code, (
            f"a removed member's report naming {len(shape)} member(s) is refused "
            f"as not from a live member: {err.code} {err.details!r}"
        )
        assert _seated(nest, alice, room_hex) == {alice["actor_id_hex"]}, (
            "and the floor is exactly what A left it"
        )

    _report(nest, alice, room_hex, [alice, bob])
    assert _seated(nest, alice, room_hex) == both, "a live member's honest report re-seats B"


# ── clause 2: no member report rewrites a community room ────────────────────


@pytest.mark.feature("group-conversations")
def test_nothing_rewrites_a_community_rooms_roster(nest_instance):
    """Outcome 30, second clause: in a community room, a seated member — on
    the routing roster after one send — reports a roster naming itself owner,
    and the owner reports one dropping the member; both are refused and the
    floor reads as the room's own ceremonies left it."""
    nest = nest_instance
    room = found_room(nest)
    member = _make_user(nest)
    seat_member(room, member)
    for who in (room.owner, member):
        _join_routing_roster(nest, who, room.room_hex)
    before = conv_api.room_list_roster(nest["port"], room.owner, room.room_hex)["members"]
    assert member["actor_id_hex"] in {m["principal"] for m in before}, "the member sits on the floor"

    attempts = {
        "the member names itself owner": (
            member,
            [{"actor": member["actor_id_hex"], "role": "owner"}],
        ),
        "the owner drops the member": (
            room.owner,
            [{"actor": room.owner["actor_id_hex"], "role": "owner"}],
        ),
    }
    for what, (reporter, members) in attempts.items():
        err = _refused(
            lambda reporter=reporter, members=members: call(
                nest,
                reporter,
                "fauna.conversations.room.roster_report",
                {"room_id": room.room_hex, "members": members},
            )
        )
        assert "permission_denied" in err.code, f"{what}: refused at the door: {err.code} {err.details!r}"
        after = conv_api.room_list_roster(nest["port"], room.owner, room.room_hex)["members"]
        assert sorted((m["principal"], m.get("role")) for m in after) == sorted(
            (m["principal"], m.get("role")) for m in before
        ), f"{what}: the floor is unchanged: {after!r}"


# ── clause 3: a removed foreign member loses the binding-only doors ─────────


@pytest.mark.feature("group-conversations")
def test_a_removed_foreign_member_loses_the_three_doors_until_reinvited(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """Outcome 30, third clause: a community room homed on one nest seats two
    members homed on another through the cross-nest invitation; both read the
    roster, the member list and the attachment door through their own nest;
    the owner removes one, whose three doors are then refused while the
    other's stay served; a fresh invitation and relayed accept open them
    again."""
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    room = found_room(home)
    leaving = away["actor"]
    staying = provision_foreign_handled_actor(away)
    for who in (leaving, staying):
        seat_foreign_member(room, away, who)
    assert {leaving["actor_id_hex"], staying["actor_id_hex"]} <= {m["principal"] for m in room.floor}, (
        f"the relayed accepts seated both foreign members on the home's floor: {room.floor!r}"
    )

    for who in (leaving, staying):
        assert _three_doors(away, who, room) == ALL_SERVED, "an admitted foreign member is served all three"

    remove_member(room, leaving)
    assert leaving["actor_id_hex"] not in {m["principal"] for m in room.floor}, "the removal unseated it"

    refused = _three_doors(away, leaving, room)
    assert all(v != "served" for v in refused.values()), (
        f"the removed member's binding died with its seat, so every door on it is shut: {refused!r}"
    )
    assert _three_doors(away, staying, room) == ALL_SERVED, (
        "the co-member from the same nest keeps all three — one admission ended, not the relay"
    )

    seat_foreign_member(room, away, leaving)
    assert _three_doors(away, leaving, room) == ALL_SERVED, (
        "an honest re-invitation and its relayed accept restore the doors — removal is not a blacklist"
    )

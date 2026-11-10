"""Tier_3 — a community room's policy edit changes only what was changed; its hand-over demotes the old owner, who can then be removed.

``community-rooms.md`` § The three classes: a community room's policy is a
signed, versioned chain kept on its home nest, and an edit is one new version
carrying exactly the change the editor staged — every other field, and every
other member's role, as it was. Handing the room over makes the successor its
owner and leaves the previous owner an ordinary member, whom the new owner can
remove through the room's own door like anyone else. The end-to-end twins are
``test_conversation_room_ownership.py`` and the policy legs of the roles and
history suites; nothing founded a community room for these two acts before.

**Two seats, one nest** (``helpers.room_seats.room_seat_pair``): the founder
founds the room with the member (every step a gesture), changes its history
rule in the settings editor and saves — the policy moves one version, the
history rule reads ``full``, and the name, join rule and both members' roles
are what they were; then stages the hand-over to the member and saves — the
member's projection reads ``Owner``, the founder's ``Member``; and the new
owner's tap on the old owner's chip takes them off the room's floor.

**Latency-independent** (convention 14): every wait is a deadline poll on a
projected role, policy field or the nest's own roster read.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    found_community_room,
    index_of,
    room_app,
    room_seat_pair,
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

TRANSFER_BUDGET_S = MESSAGE_BUDGET_S * 2
"""The offer reaching the successor, its device completing it, and the result
reaching the offering owner."""


def _floor(port: int, reader: dict, room: str) -> list[str] | None:
    try:
        return [m["principal"] for m in conv_api.room_list_roster(port, reader, room)["members"]]
    except Exception:
        return None


def _roles(thread) -> dict[str, str]:
    room = thread.room or {}
    return dict(zip(thread.participant_actor_ids, room.get("member_roles") or []))


@pytest.mark.feature("group-conversations")
def test_a_community_rooms_policy_edit_changes_only_what_changed_and_its_hand_over_demotes_the_old_owner(
    room_seat_pair, nest_instance
):
    founder, member = room_seat_pair
    port = nest_instance["port"]
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {secrets.token_hex(4)}"
    )
    conv = founder.app.conversations
    before = wait_for(
        "the founder's projection naming both members",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and member.actor_hex in t.participant_actor_ids
        and (t.room or {}).get("my_role") == "Owner",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    policy_before = dict(before.room["policy"])
    roles_before = _roles(before)
    assert policy_before["history_policy"] == "None", policy_before

    # ── the policy edit: one field staged, one field moves ───────────────
    conv.open_thread_by_id(founder_thread)
    conv.open_room_settings()
    conv.set_room_history_policy("full")
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    for seat, who in ((founder, "founder"), (member, "member")):
        edited = wait_for(
            f"the {who}'s projection reading the edited policy",
            lambda seat=seat: thread_by_channel(seat.app, room_id),
            lambda t: t is not None
            and ((t.room or {}).get("policy") or {}).get("history_policy") == "Full",
            MESSAGE_BUDGET_S,
            diagnose=lambda seat=seat: f"{seat_story(seat)}\n{seat_log(seat, 'policy')}",
        )
        policy = dict(edited.room["policy"])
        assert policy["version"] == policy_before["version"] + 1, (
            f"the edit is one policy version on the {who}'s seat: {policy} vs {policy_before}"
        )
        for field in ("name", "join_rule"):
            assert policy[field] == policy_before[field], (
                f"the edit changed only the history rule, but {field!r} moved on the {who}'s "
                f"seat: {policy} vs {policy_before}"
            )
        if seat is founder:
            assert _roles(edited) == roles_before, (
                f"the edit leaves every role as it was: {_roles(edited)} vs {roles_before}"
            )

    # ── the hand-over: the member becomes the owner ──────────────────────
    current = thread_by_channel(founder.app, room_id)
    conv.open_room_settings()
    conv.toggle_room_owner_transfer(index_of(current, member.actor_hex))
    assert conv.room_owner_transfer_staged(index_of(current, member.actor_hex))
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    heir = wait_for(
        "the member's projection reading Owner",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Owner",
        TRANSFER_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'ownership', 'policy')}",
    )
    wait_for(
        "the old owner's projection reading Member",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Member",
        TRANSFER_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'ownership', 'policy')}",
    )

    # ── the new owner removes the old one through the chip ───────────────
    mconv = member.app.conversations
    mconv.open_thread_by_id(member_thread)
    live = wait_for(
        "the new owner's chips going live",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and founder.actor_hex in t.participant_actor_ids
        and member.app.driver.is_enabled("thread-member-chip"),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    mconv.remove_member(index_of(live, founder.actor_hex))
    wait_for(
        "the old owner leaving the room's floor",
        lambda: _floor(port, member.actor, room_id),
        lambda seats: seats is not None and founder.actor_hex not in seats,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n{seat_story(member)}\n"
        f"{seat_log(member, 'remove', 'room')}",
    )
    assert not member.app.has_error(), member.app.error_text()
    assert heir.room["policy"]["history_policy"] == "Full", (
        f"the hand-over carried the edited rule forward: {heir.room['policy']}"
    )

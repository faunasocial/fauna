"""Tier_3 — what a member homed on ANOTHER nest may and may not do to a community room.

``docs/features/group-conversations.md`` outcomes 41 and 48, whose promises
``community-rooms.md`` § The three classes owns (§ Implementation status
today → *A removal severs* and its *leaving does sever* clause for 41; the
cross-nest invitation's foreign-inviter leg, ``room-invitations.md`` § Join
rules and invites → *A cross-nest invitation*, and the declared re-homing
bound, ``community-rooms.md`` § Implementation status today, the re-homing
paragraph, for 48):

* **48** — under ``member-invite`` a member homed elsewhere invites a third
  person: the inviter's device issues through its own nest's relay, the room's
  home judges and delivers it, and the invitee accepts and is seated. Handing
  the room to that foreign member is REFUSED at the home ("needs the re-homing
  ceremony, which is not built") and changes nothing — the editor stays open on
  the refusal, the policy keeps its version and both roles stand.
* **41** — the foreign member walks out (the relayed leave), is invited back
  and keeps the bubbles they already read; then the founder removes them (the
  purge, the unseat, the rotation) and they can be invited back again — the
  binding the removal purged is what used to refuse that re-admission.

**Topology** — :mod:`test_conversation_room_community_cross_nest`'s two real
nest binaries with floor TLS and handle domains, and its fixtures: the room's
home (``cross_nest_foreign``) holds the founder (and, for 48, a third home
account); the member is ``cross_nest_foreign_ephemeral``'s handled actor.

**Every mutation is a gesture** (convention 8) — the founding, each accept,
the policy editor, the walk-out, the chip removal, each re-invite through the
add-person control. The nest is read only as a witness (convention 5): the
home's own floor and tip-coverage read by the founder. **Latency-independent**
(convention 14): every wait is a deadline poll over projected or floor state.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    index_of,
    room_app,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from i18n.strings import S
from tests.api import conv_api
from tests.test_conversation_room_community_cross_nest import (  # noqa: F401 — fixtures adopted by import
    cross_nest_room_seats,
    cross_nest_room_trio,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS (`testing.md` § Default app and nest mode — a new outcome's
    # witness lands on the lead app first). The six others join through the
    # batched trickle-down, each with its own marker AND a
    # `helpers.room_seats.ROOM_APPS` entry.
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

CROSSING_BUDGET_S = WELCOME_BUDGET_S
"""A knock crossing nest → nest, an acceptance crossing back and the seat being
keyed through the relay — the cross-nest module's budget."""


def _floor(home: dict, reader: dict, room: str) -> dict[str, dict] | None:
    """The home nest's live floor, principal hex → entry, read by ``reader``
    (a seat homed there); ``None`` on a refusal worth polling."""
    try:
        roster = conv_api.room_list_roster(home["port"], reader, room, scheme="https")
    except Exception:
        return None
    return {m["principal"]: m for m in roster.get("members", [])}


def _keyed(home: dict, reader: dict, room: str, who: str) -> bool:
    """Whether ``who`` sits on the home's floor with the tip wrapped to it —
    the key-in barrier, read where it is decided (see
    ``helpers.room_seats.found_community_room``'s note on why the seat's own
    ``awaiting_key`` cannot be it)."""
    entry = (_floor(home, reader, room) or {}).get(who)
    return bool(entry and entry.get("tip_wrapped") is True)


def _founded_room(seat):
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
            return t
    return None


def _found_with_foreign_member(founder, member, home, away) -> tuple[str, str, str]:
    """Found a community room on ``home`` with ``member`` (homed on ``away``)
    and seat them — every step a gesture. Returns ``(room id, founder's thread
    id, member's thread id)`` once the home reads the member's seat keyed."""
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(f"{member.handle}@{away['authority']}")
    conv.toggle_home_nest()
    assert conv.prospective_room_class() == "community", (
        seat_element(founder, "recipient-picker-class")
    )
    founder.app.driver.type_text("dm-text-field", f"hello {secrets.token_hex(4)}")
    founder.app.driver.click("dm-send-button")
    room = wait_for(
        "the founder's community room binding to its room id",
        lambda: _founded_room(founder),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'found', 'room', 'invite')}",
    )
    room_id = room.channel_id_hex
    assert not founder.app.has_error(), founder.app.error_text()
    joined = _accept_and_key(founder, member, home, room_id, what="the founding invitation")
    return room_id, room.thread_id, joined.thread_id


def _accept_and_key(inviter, invitee, home, room_id: str, *, what: str):
    """The invitation standing on ``invitee``'s list → accept → the home reads
    the seat keyed. Returns the invitee's thread."""
    wait_for(
        f"{what} standing on the invitee's list",
        lambda: invitee.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(invitee)}\n{seat_log(invitee, 'invitation', 'room')}\n"
            f"{seat_log(inviter, 'invite', 'federation', 'relay')}"
        ),
    )
    invitee.app.conversations.accept_room_invitation(0)
    joined = wait_for(
        f"the room opening on the invitee's seat after accepting {what}",
        lambda: thread_by_channel(invitee.app, room_id),
        lambda t: t is not None,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(invitee)}\n{seat_log(invitee, 'accept', 'room', 'relay')}",
    )
    assert not invitee.app.has_error(), (
        f"accepting {what} must not refuse: {invitee.app.error_text()!r}\n{seat_story(invitee)}"
    )
    assert invitee.app.conversations.room_invitation_count() == 0, (
        "an accepted invitation stops standing"
    )
    # The founder's device keys every seat; read the home as the founder
    # would — any seat homed there reads the same floor.
    reader = inviter if inviter.port == home["port"] else None
    assert reader is not None, "the barrier is read by a seat homed on the room's home"
    wait_for(
        f"the invitee's seat being keyed in after {what}",
        lambda: _keyed(home, reader.actor, room_id, invitee.actor_hex),
        lambda keyed: keyed,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"floor={_floor(home, reader.actor, room_id)}\n{seat_story(invitee)}\n"
            f"{seat_log(inviter, 'key-in', 'key_in', 'tend', 'mint')}"
        ),
    )
    return joined


def _open_chips(seat, thread_id, room_id: str, *, members: set[str], what: str):
    seat.app.conversations.open_thread_by_id(thread_id)
    return wait_for(
        what,
        lambda: thread_by_channel(seat.app, room_id),
        lambda t: t is not None
        and members <= set(t.participant_actor_ids)
        and seat.app.conversations.member_chip_count() >= len(members),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )


@pytest.mark.feature("group-conversations")
def test_a_foreign_member_invites_into_the_room_and_handing_them_the_room_is_refused(
    cross_nest_room_trio, cross_nest_foreign, cross_nest_foreign_ephemeral, room_app
):
    """48: under ``member-invite`` the foreign member's add-person control
    invites a home account, who accepts and is seated; the founder's hand-over
    to the foreign member is refused and nothing changes hands."""
    founder, member, third = cross_nest_room_trio
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    room_id, founder_thread, member_thread = _found_with_foreign_member(
        founder, member, home, away
    )
    # The third person is reached by the MEMBER's initiation, so their reach
    # policy must admit the member (`direct-messages.md` § Reach policy).
    conv_api.accept_contact(home["port"], third.actor, member.actor_hex, scheme="https")

    # ── the founder opens invitations to every member ────────────────────
    conv = founder.app.conversations
    conv.open_thread_by_id(founder_thread)
    conv.open_room_settings()
    conv.set_room_join_rule("member-invite")
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    member_view = wait_for(
        "the foreign member's projection granting the invite",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.capabilities or {}).get("can_invite"),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'floor', 'policy')}",
    )
    assert (member_view.room or {}).get("my_role") == "Member", member_view.room

    # ── the foreign member invites a home account through its own relay ──
    mconv = member.app.conversations
    mconv.open_thread_by_id(member_thread)
    wait_for(
        "the foreign member's add-person control going live",
        lambda: member.app.driver.is_enabled("thread-add-participant-button"),
        lambda live: live,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_element(member, "thread-add-participant-button"),
    )
    # From the member's nest the third person is a FOREIGN principal, so the
    # picker takes them as `handle@authority` — a bare actor id resolves only
    # on the picker's own nest (measured 2026-11-07: `resolve_state='not-found'`).
    mconv.add_participant_to_thread(f"{third.handle}@{home['authority']}")
    assert not member.app.has_error(), (
        f"a foreign member's invitation must not refuse: {member.app.error_text()!r}\n"
        f"{seat_log(member, 'invite', 'relay', 'room')}"
    )
    _accept_and_key(founder, third, home, room_id, what="the foreign member's invitation")
    floor = _floor(home, founder.actor, room_id) or {}
    assert {founder.actor_hex, member.actor_hex, third.actor_hex} <= set(floor), (
        f"the home seats the foreign member's invitee beside them: {sorted(floor)}"
    )

    # ── handing the room to the foreign member is refused, not half done ─
    before = wait_for(
        "the founder's chips naming all three",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None
        and {member.actor_hex, third.actor_hex} <= set(t.participant_actor_ids)
        and conv.member_chip_count() >= 2,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    version_before = before.room["policy"]["version"]
    assert before.room["my_role"] == "Owner", before.room
    member_index = index_of(before, member.actor_hex)
    conv.open_room_settings()
    conv.toggle_room_owner_transfer(member_index)
    assert conv.room_owner_transfer_staged(member_index), (
        seat_element(founder, "room-owner-transfer-button")
    )
    refusal_prefix = S.conversations.unified.error_set_room_policy(message="")
    founder.app.driver.click("room-settings-save-button")
    refused = wait_for(
        "the refused hand-over reaching the founder's error line",
        lambda: founder.app.error_text(),
        lambda text: bool(text) and text.startswith(refusal_prefix),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: (
            f"{seat_element(founder, 'error-message')}\n"
            f"{seat_element(founder, 'room-settings-save-button')}\n"
            f"{seat_log(founder, 'transfer', 'ownership', 'policy')}"
        ),
    )
    # The wording is the authorization family's one sentence on purpose
    # (`RpcError::localized_family` — a refusal never says which check
    # fired), so the witness is the refusal plus the unchanged room below.
    assert founder.app.driver.is_visible("room-settings-save-button"), (
        f"the editor stays open over the refusal ({refused!r}):\n"
        f"{seat_element(founder, 'room-settings-save-button')}"
    )
    after = thread_by_channel(founder.app, room_id)
    assert after is not None and after.room["my_role"] == "Owner", (
        f"the founder still owns the room: {after.room if after else None}"
    )
    assert after.room["policy"]["version"] == version_before, (
        f"no policy version was spent on a refused hand-over: {after.room['policy']}"
    )
    still_member = thread_by_channel(member.app, room_id)
    assert still_member is not None and (still_member.room or {}).get("my_role") == "Member", (
        f"the foreign member was handed nothing: {seat_story(member)}"
    )


@pytest.mark.feature("group-conversations")
def test_a_foreign_member_leaves_and_returns_and_a_removed_one_is_invited_back(
    cross_nest_room_seats, cross_nest_foreign, cross_nest_foreign_ephemeral, room_app
):
    """41: the foreign member walks out and is invited back keeping what they
    read; removed by the founder, they are invited back again and read the
    room once more."""
    founder, member = cross_nest_room_seats
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    member_address = f"{member.handle}@{away['authority']}"
    room_id, founder_thread, member_thread = _found_with_foreign_member(
        founder, member, home, away
    )
    conv = founder.app.conversations
    mconv = member.app.conversations
    read_word = f"before{secrets.token_hex(4)}"

    # ── something the member reads before leaving ────────────────────────
    conv.open_thread_by_id(founder_thread)
    conv.send_in_open_thread(f"read this {read_word}")
    wait_for(
        "the founder's message opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and read_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'fetch')}",
    )

    # ── the foreign member walks out ─────────────────────────────────────
    mconv.open_thread_by_id(member_thread)
    mconv.open_room_settings()
    assert mconv.room_leave_enabled(), seat_element(member, "room-leave-button")
    mconv.leave_room(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not member.app.has_error(), (
        f"a foreign member's walk-out must not refuse: {member.app.error_text()!r}\n"
        f"{seat_log(member, 'leave', 'relay', 'room')}"
    )
    wait_for(
        "the home's floor dropping the foreign leaver",
        lambda: _floor(home, founder.actor, room_id),
        lambda floor: floor is not None and member.actor_hex not in floor,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"floor={_floor(home, founder.actor, room_id)}\n"
        f"{seat_log(member, 'leave', 'relay')}",
    )

    # ── invited back, they keep what they read ───────────────────────────
    conv.open_thread_by_id(founder_thread)
    wait_for(
        "the founder's add-person control live after the departure",
        lambda: founder.app.driver.is_enabled("thread-add-participant-button"),
        lambda live: live,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_element(founder, "thread-add-participant-button"),
    )
    conv.add_participant_to_thread(member_address)
    assert not founder.app.has_error(), (
        f"re-inviting a member who left must not refuse: {founder.app.error_text()!r}"
    )
    back = _accept_and_key(founder, member, home, room_id, what="the re-invitation after leaving")
    assert back.thread_id == member_thread, (
        "the room comes back as the same conversation, not a new one"
    )
    mconv.open_thread_by_id(member_thread)
    bubbles = wait_for(
        "the returned member's thread painting its bubbles",
        lambda: member.app.driver.get_texts("dm-message-text"),
        lambda texts: bool(texts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert any(read_word in (b or "") for b in bubbles), (
        f"the returned member keeps the bubble they read before leaving: {bubbles!r}"
    )
    returned_word = f"again{secrets.token_hex(4)}"
    conv.open_thread_by_id(founder_thread)
    conv.send_in_open_thread(f"welcome back {returned_word}")
    wait_for(
        "the founder's next message reaching the returned member",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and returned_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'fetch')}",
    )
    # ── removed by the founder, then invited back again ──────────────────
    listed = _open_chips(
        founder, founder_thread, room_id, members={member.actor_hex},
        what="the founder's chips naming the returned member",
    )
    conv.remove_member(index_of(listed, member.actor_hex))
    wait_for(
        "the home's floor dropping the removed foreign member",
        lambda: _floor(home, founder.actor, room_id),
        lambda floor: floor is not None and member.actor_hex not in floor,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={founder.app.error_text()!r}\n"
        f"{seat_log(founder, 'remove', 'rotate', 'mint')}",
    )
    assert not founder.app.has_error(), (
        f"the removal and its rotation both landed: {founder.app.error_text()!r}"
    )
    conv.add_participant_to_thread(member_address)
    assert not founder.app.has_error(), (
        f"re-inviting a removed member must not refuse — the removal purged the "
        f"binding that used to: {founder.app.error_text()!r}"
    )
    _accept_and_key(founder, member, home, room_id, what="the re-invitation after the removal")
    last_word = f"readmitted{secrets.token_hex(4)}"
    conv.open_thread_by_id(founder_thread)
    conv.send_in_open_thread(f"once more {last_word}")
    wait_for(
        "the founder's message reaching the re-admitted member",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and last_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'fetch', 'generation')}",
    )

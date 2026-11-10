"""Tier_3 — a community room's invitation edges: a silent decline, one standing re-invitation, a full inbox.

``docs/features/group-conversations.md`` outcome 42, whose promise
``community-rooms.md`` § The three classes owns (§ Implementation status today,
the invitations entry: *A decline tells the room nothing*, *One standing
envelope per open invitation, quota-charged (2026-09-13)*):

* **Declining tells the room nothing.** The invitee declines from their list;
  the row leaves it, while the room's own view — the founder's pending list,
  the floor, the founder's page — moves not at all: a refusal the inviter
  could read would give declining an audience.
* **Re-inviting leaves exactly one invitation standing.** The founder invites
  the same person again, twice; the invitee's nest holds ONE room envelope,
  a fresh one each time (the previous one acked and refunded), and their list
  shows one row.
* **A full inbox refuses the invitation with that reason.** The founder
  invites an account whose tier allows a one-byte inbox: the page's error line
  says the invitee's inbox is full, and nothing was delivered.

* **An invitation into a room you already sit in is settled quietly.** The
  nest refuses to invite a live member, and a same-nest accept consumes its
  own knock, so the one envelope a seated device ever meets is a FOREIGN
  member's accepted knock — resting on their own nest — whose settle did not
  land; the nest's test hook turns that settle away, and the device neither
  offers the room again nor errors, and settles the knock itself once the
  nest answers. Two nests for this clause alone (``cross_nest_room_seats``).

**Every mutation is a gesture** (convention 8); the nest is read only as a
witness — the invitee's own inbox peek and the room's floor (convention 5).
**Latency-independent** (convention 14): every wait is a deadline poll.
"""

from __future__ import annotations

import secrets

import pytest

from common import create_actor_and_register
from common.auth import ensure_tier, set_user_tier
from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    WELCOME_BUDGET_S,
    found_community_room,
    found_cross_nest_community_room,
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
from tests.api.conv_api import mint_key_packages
from tests.test_conversation_room_community_cross_nest import (  # noqa: F401 — a pytest fixture adopted by import
    cross_nest_room_seats,
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

FULL_INBOX_TIER = "room-invite-one-byte-inbox"
"""A tier of this file's own whose inbox holds one byte — no invitation fits."""


def _room_envelopes(nest_url: str, actor: dict, room_id: str) -> list[dict] | None:
    """The items standing in ``actor``'s inbox that carry ``room_id`` — the
    room invitation envelopes, read by a pure peek (``conv_api.inbox``);
    ``None`` on a refusal worth polling."""
    try:
        items = conv_api.inbox(nest_url, actor)
    except Exception:
        return None
    return [it for it in items if room_id in it["payload"]]


def _floor(port: int, reader: dict, room: str) -> set[str] | None:
    try:
        return {m["principal"] for m in conv_api.room_list_roster(port, reader, room)["members"]}
    except Exception:
        return None


@pytest.mark.feature("group-conversations")
def test_a_declined_invitation_tells_the_room_nothing_and_a_reinvitation_stands_once(
    room_seats, nest_instance
):
    """The invitee's decline changes nothing the room can see; two
    re-invitations leave one envelope standing, the second replacing the
    first."""
    founder, member, invitee = room_seats
    port = nest_instance["port"]
    nest_url = nest_instance["url"]
    room_id, founder_thread, _ = found_community_room(
        founder, member, f"hello {secrets.token_hex(4)}"
    )
    conv = founder.app.conversations

    # ── the founder invites a third person ───────────────────────────────
    conv.open_thread_by_id(founder_thread)
    conv.add_participant_to_thread(invitee.actor_hex)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the invitation standing on the invitee's list",
        lambda: invitee.app.conversations.room_invitation_count(),
        lambda n: n == 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(invitee)}\n{seat_log(invitee, 'invitation', 'room')}",
    )

    # ── the decline: the row leaves the invitee's list ───────────────────
    invitee.app.conversations.decline_room_invitation(0)
    wait_for(
        "the declined row leaving the invitee's list",
        lambda: invitee.app.conversations.room_invitation_count(),
        lambda n: n == 0,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={invitee.app.error_text()!r}\n{seat_story(invitee)}",
    )
    assert not invitee.app.has_error(), (
        f"a decline must not refuse: {invitee.app.error_text()!r}"
    )

    # ── …and the room is told nothing ────────────────────────────────────
    assert not founder.app.has_error(), (
        f"the founder's page says nothing of the decline: {founder.app.error_text()!r}"
    )
    conv.open_room_settings()
    wait_for(
        "the founder's editor serving its pending list",
        lambda: conv.pending_invite_count(),
        lambda n: n >= 1,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_element(founder, 'room-pending-invite')}\n{seat_story(founder)}",
    )
    assert invitee.handle.split("@")[0] in conv.pending_invite_text(0), (
        "the founder still sees the invitation pending — the decline gave it no "
        f"audience: {conv.pending_invite_text(0)!r}"
    )
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)  # nothing staged: Save closes
    assert invitee.actor_hex not in (_floor(port, founder.actor, room_id) or set()), (
        "nobody was seated by an invitation that was declined"
    )
    founder_view = thread_by_channel(founder.app, room_id)
    assert founder_view is not None and invitee.actor_hex not in founder_view.participant_actor_ids, (
        f"the founder's room lists nobody new: {seat_story(founder)}"
    )

    # ── re-invited twice: one envelope stands, the second replacing the first
    conv.add_participant_to_thread(invitee.actor_hex)
    assert not founder.app.has_error(), (
        f"re-inviting after a decline must not refuse: {founder.app.error_text()!r}"
    )
    first = wait_for(
        "the re-invitation's envelope standing in the invitee's inbox",
        lambda: _room_envelopes(nest_url, invitee.actor, room_id),
        lambda items: items is not None and len(items) == 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"envelopes={_room_envelopes(nest_url, invitee.actor, room_id)}",
    )
    conv.add_participant_to_thread(invitee.actor_hex)
    assert not founder.app.has_error(), (
        f"a second re-invitation must not refuse: {founder.app.error_text()!r}"
    )
    replaced = wait_for(
        "the second re-invitation replacing the standing envelope",
        lambda: _room_envelopes(nest_url, invitee.actor, room_id),
        lambda items: items is not None and items and items[0]["id"] != first[0]["id"],
        WELCOME_BUDGET_S,
        diagnose=lambda: f"first={first}\nnow={_room_envelopes(nest_url, invitee.actor, room_id)}",
    )
    assert len(replaced) == 1, (
        f"exactly one invitation stands after re-inviting: {replaced}"
    )
    wait_for(
        "the invitee's list showing the one standing invitation",
        lambda: invitee.app.conversations.room_invitation_count(),
        lambda n: n == 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(invitee)}\n{seat_log(invitee, 'invitation', 'room')}",
    )


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("room-invite-inbox-full-reason")
def test_an_invitation_to_a_full_inbox_is_refused_with_that_reason(room_seats, nest_instance):
    """The founder invites an account whose inbox cannot take the envelope: the
    page's error line names the full inbox, and nothing was delivered."""
    founder, member, _ = room_seats
    port = nest_instance["port"]
    nest_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    room_id, founder_thread, _ = found_community_room(
        founder, member, f"hello {secrets.token_hex(4)}"
    )

    full = create_actor_and_register(port, admin_signing_key=admin_sk)
    # The recipient picker promotes an address to a Fauna chip only once the
    # account's key-package reachability probe reads `keypackage_count > 0`,
    # so an API-only account publishes some before the founder types it.
    conv_api.keypackage_upload(port, full, mint_key_packages(bytes(full["signing_key"]), 1))
    ensure_tier(port, admin_signing_key=admin_sk, name=FULL_INBOX_TIER, base_url=nest_url, max_inbox_bytes=1)
    set_user_tier(port, full["actor_id_hex"], FULL_INBOX_TIER, admin_signing_key=admin_sk, base_url=nest_url)
    conv_api.accept_contact(port, full, founder.actor_hex)

    conv = founder.app.conversations
    conv.open_thread_by_id(founder_thread)
    prefix = S.conversations.unified.error_add_participant(message="")
    conv.add_participant_to_thread(full["actor_id_hex"])
    refused = wait_for(
        "the refused invitation reaching the founder's error line",
        lambda: founder.app.error_text(),
        lambda text: bool(text) and text.startswith(prefix),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_element(founder, 'error-message')}\n{seat_log(founder, 'invite', 'room')}",
    )
    reason = refused[len(prefix):].lower()
    assert "inbox" in reason and "full" in reason, (
        f"the refusal names the invitee's full inbox, not a generic reason: {refused!r}"
    )
    assert _room_envelopes(nest_url, full, room_id) == [], (
        "a refused invitation delivers nothing"
    )


@pytest.mark.feature("group-conversations")
def test_an_invitation_into_a_room_you_already_sit_in_is_settled_quietly(
    cross_nest_room_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """A member homed on another nest accepts while their own nest turns the
    settle away: the knock still stands in their inbox although they sit in
    the room, their device does not offer it again, and settles it on its own
    once the nest answers.

    **Why two nests.** The nest refuses to invite a live member, and a
    same-nest accept consumes the knock in the accept's own transaction
    (``db/rooms.rs``, the accept door's ``ack_and_refund_row``), so on one nest
    no envelope can ever stand for a seated device. A foreign invitee's knock
    rests in the inbox on THEIR nest, which the home's accept never touches:
    the device settles it (``fauna.inbox.ack`` on its own nest), and that
    settle is the one an outage can lose — the case
    ``FaunaMlsBackend::room_invitations`` settles quietly. The nest's test hook
    turns the member's ack away (``refuse_rpc(..., actor=)``); every gesture is
    still the user's."""
    from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc

    founder, member = cross_nest_room_seats
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    ack = "fauna.inbox.ack"
    refuse_rpc(away["port"], ack, actor=member.actor_hex)
    try:
        room_id, _founder_thread, _member_thread = found_cross_nest_community_room(
            founder, member, home, away, f"hello {secrets.token_hex(4)}"
        )
        wait_for_refused_rpc(away["port"], ack)
        # The control: the settle did not land — the knock still stands.
        standing = _room_envelopes(away["url"], member.actor, room_id)
        assert standing, (
            "with the accept's settle turned away the knock must still stand in the "
            f"member's inbox on their own nest, else this proves nothing: {standing!r}"
        )
        # ── not offered again, and no error for a room they sit in ───────
        wait_for(
            "the member's list offering nothing for a room they sit in",
            lambda: member.app.conversations.room_invitation_count(),
            lambda n: n == 0,
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: (
                f"{seat_element(member, 'room-invitation')}\n{seat_story(member)}\n"
                f"{seat_log(member, 'invitation', 'settl')}"
            ),
        )
    finally:
        release_rpc_hold(away["port"], ack)

    # ── once the nest answers, the device settles it on its own ──────────
    wait_for(
        "the stale knock settled by the member's device with no gesture",
        lambda: _room_envelopes(away["url"], member.actor, room_id),
        lambda envelopes: envelopes == [],
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'settl')}",
    )
    assert member.app.conversations.room_invitation_count() == 0
    assert not member.app.has_error(), (
        f"a quiet settle says nothing: {member.app.error_text()!r}"
    )

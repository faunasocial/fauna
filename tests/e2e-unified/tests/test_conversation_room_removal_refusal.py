"""Tier_3 — a removal that cannot land fails visibly, and a retry completes it.

``docs/features/group-conversations.md`` outcome 36: "A removal that cannot be
completed safely fails visibly and can be retried; it never reports success
while the removed person keeps reading". The rule is ``devices.md``
§ Cross-device MLS group-state sync → *Rule 1*: the conversations membership
path stages the removal, persists the staged pending, **sends**, and merges
only once the nest accepted the send — clearing the staged commit on a failed
send — so a merged-but-undistributed removal (a seat that believes someone is
out while every other member still keys them in) is unreachable. This journey
drives that through the app: the owner's chip removal meets a nest that turns
the commit away, and the observables are the ones a user has.

**How the removal is made to fail.** The nest refuses every
``fauna.conversations.channel.send`` while armed (``helpers.rpc_hold``'s
``refuse_rpc``, released in a ``finally``): the removal commit never reaches
the channel, exactly the failed send Rule 1 is about. What the journey
asserts, in order: the page's error line names the failed removal; the
owner's roster still lists the member (no success reported); nothing reached
the channel. After the release the same chip removes the member for real; the
witness's roster follows; and a message the owner sends next reaches the
witness and not the removed member.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state or the nest's refusal counter; the one
negative assertion (the removed member does not receive the next message) is
read only after the witness — who would receive it on the same path — has.
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
    wait_for,
    wait_room_role,
    wait_snippet,
    wait_thread_by_channel,
)
from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS (`testing.md` § Default app and nest mode — a new outcome's
    # witness lands on the lead app first). The six others join through the
    # batched trickle-down, each with its own marker AND a
    # `helpers.room_seats.ROOM_APPS` entry.
    pytest.mark.tui,
]

CHANNEL_SEND = "fauna.conversations.channel.send"
"""The kind the removal commit is posted by — refused, so the send fails."""


def _diag(seat) -> str:
    return "\n".join(
        [
            seat_element(seat, "error-message"),
            seat_element(seat, "thread-member-chip"),
            seat_story(seat),
            seat_log(seat, "remove", "commit", "pending", "channel.send"),
        ]
    )


@pytest.mark.feature("group-conversations")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_a_removal_the_nest_turns_away_fails_visibly_and_a_retry_completes_it(
    room_seats, nest_instance
):
    """The owner's removal meets a refused send: the error line says so and the
    member stays listed; retried after the release, it lands and the removed
    member stops receiving."""
    owner, member, witness = room_seats
    port = nest_instance["port"]

    group_id, channel = bootstrap_room(owner, member, witness)
    for seat, who in ((member, "member"), (witness, "witness")):
        wait_thread_by_channel(seat, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(seat, channel, "hi room", what=f"'hi room' reaching the {who}")

    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    assert owner_view.capabilities["can_remove_members"], owner_view.capabilities
    conv = owner.app.conversations
    conv.open_thread_by_id(group_id)
    owner_open = wait_for(
        "the owner's chips rendering the roster live",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and conv.member_chip_count() >= 2
        and owner.app.driver.is_enabled("thread-member-chip"),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )

    # ── the removal meets a refused send ─────────────────────────────────
    refusal_prefix = S.conversations.unified.error_remove_participant(message="")
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    refuse_rpc(port, CHANNEL_SEND)
    try:
        conv.remove_member(index_of(owner_open, member.actor_hex))
        wait_for_refused_rpc(port, CHANNEL_SEND)
        failed = wait_for(
            "the failed removal reaching the page's error line",
            lambda: owner.app.error_text(),
            lambda text: bool(text) and text.startswith(refusal_prefix),
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: _diag(owner),
        )
        still = thread_by_channel(owner.app, channel)
        assert still is not None and member.actor_hex in still.participant_actor_ids, (
            f"a removal that did not land reports no success — the member stays "
            f"listed (error {failed!r}):\n{_diag(owner)}"
        )
    finally:
        release_rpc_hold(port, CHANNEL_SEND)
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) == envelopes_before, (
        "nothing of the failed removal reached the channel"
    )

    # ── retried, the same chip removes them ──────────────────────────────
    retry_view = wait_for(
        "the owner's chips still live over the unchanged roster",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None
        and member.actor_hex in t.participant_actor_ids
        and owner.app.driver.is_enabled("thread-member-chip"),
        MESSAGE_BUDGET_S,
        diagnose=lambda: _diag(owner),
    )
    conv.remove_member(index_of(retry_view, member.actor_hex))
    wait_for(
        "the owner's roster dropping the member on the retry",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None and member.actor_hex not in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: _diag(owner),
    )
    assert not owner.app.has_error(), (
        f"the retried removal lands cleanly: {owner.app.error_text()!r}\n{_diag(owner)}"
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) > envelopes_before, (
        "the retried removal is a real commit on the channel"
    )
    wait_for(
        "the witness's roster following the removal",
        lambda: thread_by_channel(witness.app, channel),
        lambda t: t is not None and member.actor_hex not in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(witness),
    )

    # ── the removed member stops receiving ───────────────────────────────
    conv.real_send(group_id, "after the removal")
    wait_snippet(witness, channel, "after the removal", what="the next message reaching the witness")
    gone = thread_by_channel(member.app, channel)
    assert gone is None or "after the removal" not in (gone.snippet or ""), (
        f"the removed member must not read what the room says next:\n{seat_story(member)}"
    )

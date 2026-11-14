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


def _chip_texts(seat) -> str:
    """Every painted chip's text in index order — the chip a click at index
    ``i`` lands on, so a removal that hits the wrong member names itself."""
    try:
        return f"[thread-member-chip texts: {seat.app.driver.get_texts('thread-member-chip')!r}]"
    except Exception as exc:  # noqa: BLE001 — a diagnostic never masks the failure
        return f"[thread-member-chip texts: <{type(exc).__name__}: {exc}>]"


def _chips_match_roster(app, conv, t) -> bool:
    """The painted chips are exactly the roster: one chip per listed
    participant, live. ``thread-member-chip[i]`` is index-parallel with
    ``participant_actor_ids`` only once the count agrees, so a chip is driven
    only over a painted roster that has caught up with the projected one (the
    failed removal's rollback re-orders it)."""
    return (
        t is not None
        and conv.member_chip_count() == len(t.participant_actor_ids)
        and app.driver.is_enabled("thread-member-chip")
        and _chips_paint_roster(app, t)
    )


def _chips_paint_roster(app, t) -> bool:
    """Each painted chip names, in order, the participant the projection lists
    at its index — a count that agrees is not enough: a header still painting
    an earlier roster of the same size (a re-order, a swap) drives the wrong
    removal from the right index. The chip text may carry a localized role
    mark, so it must CONTAIN the display, never equal it; an app whose
    serializer ships no ``participant_displays`` yet is checked on the count
    alone."""
    displays = t.participant_displays
    if not displays:
        return True
    texts = app.driver.get_texts("thread-member-chip")
    return len(texts) == len(displays) and all(
        d in text for d, text in zip(displays, texts)
    )


def _remove_by_chip(owner, view, target, clicks: list) -> None:
    """Tap the chip ``index_of`` names for ``target``, first recording what
    the tap is aimed at — the roster order, the index and every painted chip's
    text — so a removal that lands on someone else says which chip it hit."""
    i = index_of(view, target.actor_hex)
    clicks.append(
        f"[click {len(clicks) + 1}: index {i} for {target.actor_hex[:12]}… over "
        f"roster {[a[:12] for a in view.participant_actor_ids]}; {_chip_texts(owner)}]"
    )
    owner.app.conversations.remove_member(i)


def _diag(seat) -> str:
    return "\n".join(
        [
            seat_element(seat, "error-message"),
            seat_element(seat, "thread-member-chip"),
            _chip_texts(seat),
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
        and _chips_match_roster(owner.app, conv, t),
        MESSAGE_BUDGET_S,
        diagnose=lambda: _diag(owner),
    )

    # ── the removal meets a refused send ─────────────────────────────────
    refusal_prefix = S.conversations.unified.error_remove_participant(message="")
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    clicks: list[str] = []
    refuse_rpc(port, CHANNEL_SEND)
    try:
        _remove_by_chip(owner, owner_open, member, clicks)
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
        and witness.actor_hex in t.participant_actor_ids
        and _chips_match_roster(owner.app, conv, t),
        MESSAGE_BUDGET_S,
        diagnose=lambda: _diag(owner),
    )
    _remove_by_chip(owner, retry_view, member, clicks)
    wait_for(
        "the owner's roster dropping the member on the retry",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None and member.actor_hex not in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: "\n".join([*clicks, _diag(owner)]),
    )
    wait_for(
        "the owner's chips repainting the roster without the member",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None
        and conv.member_chip_count() == len(t.participant_actor_ids)
        and _chips_paint_roster(owner.app, t),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: "\n".join([*clicks, _diag(owner)]),
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

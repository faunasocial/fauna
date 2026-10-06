"""Tier_3 — a community room names a labeler from the app, and a member's message comes back labelled.

``community-rooms.md`` § The three classes → *What the home nest does with its
read*, purpose 2: the home nest applies the transparent labelers the room's
owner or admins name, in the act that indexes each message, and serves what
they derive beside the message — which every app's existing
``content-label-badge`` paints. The nest half and the shared editor seam were
built on 2026-09-10; this journey drives the app's half: the owner inspects a
published labeler in the room's settings, names it, saves, and a member's
message is labelled on the owner's screen.

**Two seats** — the ``founder`` (owner) and a ``member`` — on one nest. The
fixture labeler (``fixtures/labeler/room_commercial_labeler.wat``) says
``commercial`` about any text containing "cat" and nothing about anything
else; publishing it is fixture setup (convention 8 carve-out (b)): no app
offers a publish gesture in v1, and the mutation under test is naming it.

**Latency-independent** (convention 14): every wait is a deadline poll over
projected state — the room's verified ``labelers`` set, a snippet, the
bubble's badge count — never "sleep N then assert".
"""

from __future__ import annotations

import secrets

import pytest

from helpers.labeler_publish import FIXTURES, publish_wasm_labeler
from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    found_community_room,
    room_app,
    room_seat_pair,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS (the lead app paints every new room gesture first); the six
    # others join through the room model's batched trickle-down, each with its
    # own marker AND a `helpers.room_seats.ROOM_APPS` entry.
    pytest.mark.tui,
]


@pytest.mark.feature("group-conversations")
def test_a_community_rooms_owner_names_a_labeler_and_a_members_message_is_labelled(
    room_seat_pair, nest_instance, run_seal_helper
):
    """Publish a fixture labeler, found a community room, inspect and name the
    labeler in the room's settings, and see a member's message labelled."""
    founder, member = room_seat_pair
    labeler = publish_wasm_labeler(
        run_seal_helper, nest_instance, FIXTURES / "room_commercial_labeler.wat"
    ).hex()
    word = f"whiskers{secrets.token_hex(4)}"

    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {secrets.token_hex(4)}"
    )
    conv = founder.app.conversations
    before = thread_by_channel(founder.app, room_id)
    assert (before.room or {}).get("labelers") == [], (
        f"a freshly founded community room names no labelers: {before.room!r}"
    )

    # ── the owner inspects the published labeler, then names it ──────────
    conv.open_thread_by_id(founder_thread)
    conv.open_room_settings()
    row = conv.room_labeler_index(labeler)
    assert row is not None, (
        "the editor lists every labeler the nest publishes that a room may name: "
        f"{seat_element(founder, 'room-labeler-toggle')}"
    )
    assert conv.room_labeler_staged(row) is False
    metadata = conv.inspect_room_labeler(row)
    assert labeler in metadata and "verified: true" in metadata, (
        f"inspect shows the labeler's own signed, re-verified metadata: {metadata!r}"
    )
    conv.close_labeler_inspect()
    assert founder.app.driver.is_absent("labeler-inspect-panel")
    assert conv.room_labeler_staged(row) is False, "inspecting stages nothing"
    conv.toggle_room_labeler(row)
    assert conv.room_labeler_staged(row) is True, seat_element(founder, "room-labeler-toggle")
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the room's verified labeler set naming the fixture on the founder's seat",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("labelers") == [labeler],
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'labeler')}",
    )

    # ── a member's message the labeler has something to say about ────────
    member.app.conversations.open_thread_by_id(member_thread)
    member.app.conversations.send_in_open_thread(f"my cat says {word}")
    wait_for(
        "the member's message opening on the founder's seat",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_story(member)}",
    )
    conv.open_thread_by_id(founder_thread)
    d = founder.app.driver
    wait_for(
        "the home nest's verdict painting a badge on the member's bubble",
        lambda: d.count("content-label-badge"),
        lambda n: n >= 1,
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"badges={d.count('content-label-badge')} "
            f"messages={d.count('dm-message-text')}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'label')}"
        ),
    )
    assert d.count("content-label-badge") == 1, (
        "only the message the labeler had something to say about is labelled — the "
        "founding message carries no 'cat' and was sent before the set existed"
    )
    badge = d.get_text("content-label-badge")
    assert S.moderation.category.commercial in badge, (
        f"the badge names the labeler's verdict ({S.moderation.category.commercial!r}); "
        f"got {badge!r}"
    )

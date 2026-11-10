"""Tier_3 — a room-settings change that does not land says so, on the page.

``docs/features/group-conversations.md`` outcome 32: "A policy change refused
on Save shows on the page's error line and the editor stays open, and an
ownership offer the room moved past tells the offering owner to offer again".
The promise is ``conversation-rooms.md`` § Roles and authorization — the
editor's Save "closes only when all of them landed, and stays open — the
page's ``error-message`` carrying the refusal — when one did not"
(``actions.conversations.save_room_settings``), and *Ownership transfer*: "An
offer the room has moved past … is refused by every member and dropped by the
device holding it; the owner offers again", the offering device painting the
ordinary page error at its own commit fold
(``FaunaMlsBackend``'s ``notice_superseded_own_offer``; its Rust-tier proof is
``fauna_mls_backend_tests.rs``'s
``an_offer_the_room_moved_past_tells_the_owner_who_made_it``). This journey is
the same two stories driven through the app on three real seats.

**One room, both arms, in the order the story needs them.** The owner's first
Save — appointing the ``admin`` seat — is refused: the nest turns away every
``fauna.conversations.channel.send`` while it is armed (``helpers.rpc_hold``'s
``refuse_rpc``, released in a ``finally``), so the policy commit never reaches
the channel. The error line carries the refusal, the editor is still open with
the appointment staged, and once the refusal is released the same Save lands —
which is what makes the refusal a visible, recoverable failure rather than a
lost gesture. That admin is then the one who moves the room past the owner's
later offer.

**The offer is made stale without a race.** The heir's seat is shut down
before the owner offers it the room, so no device of the heir's can complete
the hand-over before the admin's change lands; the admin's join-rule commit
then takes the policy version the offer needed. The owner's seat folds that
commit and its error line says to hand the room over again; the room did not
change hands on any running seat.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state or the nest's refusal counter — never
"sleep N then assert".
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
    teardown_seats,
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
"""The kind every policy commit and ownership offer is posted by — refused for
the first arm, so the owner's Save cannot reach the channel."""


def _editor_diag(seat) -> str:
    return "\n".join(
        [
            seat_element(seat, "error-message"),
            seat_element(seat, "room-settings-save-button"),
            seat_element(seat, "room-admin-toggle"),
            seat_story(seat),
            seat_log(seat, "room settings", "policy", "ownership", "channel.send"),
        ]
    )


@pytest.mark.feature("group-conversations")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_a_refused_policy_save_keeps_the_editor_open_and_a_stale_offer_says_offer_again(
    room_seats, nest_instance
):
    """A refused Save shows its error with the editor still open and lands
    once retried; a hand-over the room moved past tells the owner to offer
    again, and nothing changed hands."""
    owner, admin, heir = room_seats
    port = nest_instance["port"]

    group_id, channel = bootstrap_room(owner, admin, heir)
    for seat, who in ((admin, "admin-to-be"), (heir, "heir")):
        wait_thread_by_channel(seat, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(seat, channel, "hi room", what=f"'hi room' reaching the {who}")

    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    assert owner_view.room["my_role"] == "Owner", owner_view.room
    version_before = owner_view.room["policy"]["version"]
    conv = owner.app.conversations
    conv.open_thread_by_id(group_id)
    owner_open = wait_for(
        "the owner's chips rendering the roster",
        lambda: thread_by_channel(owner.app, channel),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and conv.member_chip_count() >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    admin_index = index_of(owner_open, admin.actor_hex)
    heir_index = index_of(owner_open, heir.actor_hex)

    # ── arm 1: a Save the nest refuses ───────────────────────────────────
    conv.open_room_settings()
    conv.toggle_room_admin(admin_index)
    assert conv.room_admin_staged(admin_index), seat_element(owner, "room-admin-toggle")
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    refusal_prefix = S.conversations.unified.error_set_room_policy(message="")
    refuse_rpc(port, CHANNEL_SEND)
    try:
        owner.app.driver.click("room-settings-save-button")
        wait_for_refused_rpc(port, CHANNEL_SEND)
        refused = wait_for(
            "the refused Save reaching the page's error line",
            lambda: owner.app.error_text(),
            lambda text: bool(text) and text.startswith(refusal_prefix),
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: _editor_diag(owner),
        )
        assert owner.app.driver.is_visible("room-settings-save-button"), (
            f"the editor stays open over a refused Save (error {refused!r}):\n"
            f"{_editor_diag(owner)}"
        )
        assert conv.room_admin_staged(admin_index), (
            "the refused change is still staged, so the same Save can be retried:\n"
            f"{_editor_diag(owner)}"
        )
    finally:
        release_rpc_hold(port, CHANNEL_SEND)
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) == envelopes_before, (
        "nothing of the refused Save reached the channel"
    )
    unchanged = thread_by_channel(admin.app, channel)
    assert unchanged is not None and (unchanged.room or {}).get("my_role") == "Member", (
        f"the refused appointment changed nobody's role: {seat_story(admin)}"
    )

    # The same Save, retried, lands — the editor closes only once it did.
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not owner.app.has_error(), (
        f"the retried Save must land: {owner.app.error_text()!r}\n{_editor_diag(owner)}"
    )
    admin_view = wait_for(
        "the admin's projection flipping to Admin off the retried Save",
        lambda: thread_by_channel(admin.app, channel),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Admin",
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(admin)}\n{seat_log(admin, 'policy', 'role')}",
    )
    assert admin_view.capabilities["can_set_policy"], admin_view.capabilities

    # ── arm 2: an offer the room moves past ──────────────────────────────
    # No device of the heir's runs from here on, so nothing can complete the
    # hand-over before the admin's change takes its version.
    teardown_seats([heir])
    owner_mid = wait_room_role(owner, channel, what="the owner's projection after the appointment")
    offered_at = owner_mid.room["policy"]["version"] + 1
    assert offered_at == version_before + 2, (
        f"one policy version for the appointment: {owner_mid.room} vs {version_before}"
    )
    conv.open_room_settings()
    conv.toggle_room_owner_transfer(heir_index)
    assert conv.room_owner_transfer_staged(heir_index), (
        seat_element(owner, "room-owner-transfer-button")
    )
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not owner.app.has_error(), (
        f"the offer itself posts cleanly: {owner.app.error_text()!r}\n{_editor_diag(owner)}"
    )

    # The admin changes the join rule — the version the offer needed.
    admin.app.conversations.open_thread_by_id(admin_view.thread_id)
    admin.app.conversations.open_room_settings()
    admin.app.conversations.set_room_join_rule("member-invite")
    admin.app.conversations.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not admin.app.has_error(), (
        f"an admin may set the join rule: {admin.app.error_text()!r}\n{_editor_diag(admin)}"
    )

    stale = S.error.send.room_transfer_superseded
    told = wait_for(
        "the owner's error line telling it to hand the room over again",
        lambda: owner.app.error_text(),
        lambda text: bool(text) and stale in text,
        MESSAGE_BUDGET_S,
        diagnose=lambda: _editor_diag(owner),
    )
    assert told.startswith(refusal_prefix), (
        f"the notice rides the room-settings error line: {told!r}"
    )
    owner_after = thread_by_channel(owner.app, channel)
    assert owner_after is not None and owner_after.room["my_role"] == "Owner", (
        f"the room did not change hands: {seat_story(owner)}"
    )
    assert owner_after.room["policy"]["version"] == offered_at, (
        f"the admin's change took the offered version: {owner_after.room}"
    )
    assert owner_after.room["policy"]["join_rule"] == "MemberInvite", owner_after.room
    admin_after = thread_by_channel(admin.app, channel)
    roles = dict(zip(admin_after.participant_actor_ids, admin_after.room["member_roles"]))
    assert roles.get(owner.actor_hex) == "Owner" and roles.get(heir.actor_hex) == "Member", (
        f"every running seat still sees the owner owning the room: {roles}\n{seat_story(admin)}"
    )

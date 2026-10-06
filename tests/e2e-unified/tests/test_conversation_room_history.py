"""Tier_3 — history for joiners in an end-to-end room, per app: two seats on one nest.

``conversation-rooms.md`` § History for joiners: under the room's ``none``
history rule a newcomer sees nothing the room said before their admission;
under ``full`` the inviting device re-seals its slice of the transcript to
the newcomer (a ``HistorySlice`` application message in the newcomer's first
epoch) and the newcomer's thread carries the pre-join messages.

**The shape.** The owner builds a real governed room with two API-tier peers
(setup, convention 8 carve-out (b)) and says two things in it. Then the
action under test: the owner adds the newcomer — a real seat with its own
engine — through the ordinary picker (``thread-add-participant-button`` →
``recipient-picker-input`` → ``add-participant-confirm``), the door the
history offer hangs off (``ConversationsManager::confirm_add_participant``).
The newcomer's view is read off its own thread: ``message_count`` and the
rendered ``dm-message-text`` bubbles, after the owner's post-join message has
reached it (the causal anchor: a decrypted post-join message proves the
Welcome applied AND every earlier channel record was fetched — the slice, if
one was offered, rides the same channel ahead of it).

**Both rules, each through the UI.** A room is born with the initial policy
(``none``); the ``full`` leg flips it through the policy editor
(``thread-room-settings-button`` → ``room-history-policy-select`` →
``room-settings-save-button``; ``ui/conversations.md`` § Element IDs,
user-approved 2026-09-09) before the add. The ``none`` leg is not vacuous: it
pins the newcomer path end to end (key-package pool, Welcome, join, first
decrypt) and the policy the room reports on the newcomer's own seat.
"""

from __future__ import annotations

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MESSAGE_BUDGET_S,
    Seat,
    launch_seat,
    room_app,
    seat_app_path,
    seat_story,
    teardown_seats,
    thread_by_id,
    wait_for,
    wait_room_role,
    wait_snippet,
    wait_thread_by_channel,
)
from tests.api import conv_api
from tests.api.conv_api import mint_key_packages as _mint_keypackages

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads; the six apps join through the batched trickle-down, each with
    # its own marker AND a `helpers.room_seats.ROOM_APPS` entry. linux joined
    # first, then macos and ios (the apple leg), then windows, then web.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.web,
]

BEFORE_ONE = "before the newcomer: one"
BEFORE_TWO = "before the newcomer: two"
AFTER = "after the newcomer joined"


@pytest.fixture()
def history_seats(nest_instance, room_app, request):
    """Two same-app seats: the ``owner`` who builds the room and the
    ``newcomer`` added later through the picker. The newcomer accepts the
    owner as a contact so the owner's Welcome is admitted (default
    ``allow_knock`` inbox mode)."""
    app_path = seat_app_path(request, room_app)
    port = nest_instance["port"]
    seats: list[Seat] = []
    try:
        owner = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(owner)
        newcomer = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(newcomer)
        conv_api.accept_contact(port, newcomer.actor, owner.actor_hex)
        yield owner, newcomer
    finally:
        teardown_seats(seats)


def _api_peer(nest_instance, owner: Seat) -> dict:
    """An engine-less member with real key packages — minted by the same
    engine code every app runs, so each advertises the policy extension and
    the room they seat in stays governed."""
    from common.auth import create_actor_and_register

    port = nest_instance["port"]
    peer = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])
    conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
    assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3
    conv_api.accept_contact(port, peer, owner.actor_hex)
    return peer


def _bootstrap_room_with_history(owner: Seat, first: dict, second: dict) -> tuple[str, str]:
    """Setup (convention 8 carve-out (b)): a real bound governed room of
    [owner, first, second] that has already said two things. Returns the
    owner's thread id and the channel hex."""
    owner.app.conversations.real_resolve_send_new(first["actor_id_hex"], "hi first")
    one = wait_for(
        "the 1:1's own echo",
        lambda: next(
            (
                t
                for t in owner.app.conversations.list_threads()
                if t.rail == "FaunaMls" and "hi first" in (t.snippet or "")
            ),
            None,
        ),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    before_ids = {t.thread_id for t in owner.app.conversations.list_threads()}
    owner.app.conversations.real_add(one.thread_id, second["actor_id_hex"], second["handle"])
    fresh = [
        t
        for t in owner.app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, f"the fork should mint exactly one group; got {len(fresh)}"
    group_id = fresh[0].thread_id
    owner.app.conversations.real_send(group_id, BEFORE_ONE)
    owner.app.conversations.real_send(group_id, BEFORE_TWO)
    bound = wait_for(
        "the group binding to a channel with both messages",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and bool(t.channel_id_hex) and t.message_count >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    return group_id, bound.channel_id_hex


def _rendered_bodies(app) -> list[str]:
    n = app.driver.count("dm-message-text")
    return [app.driver.get_text("dm-message-text", index=i) for i in range(n)]


@pytest.mark.feature("group-conversations")
def test_a_newcomer_under_none_sees_nothing_before_the_join(history_seats, nest_instance):
    """The initial policy is ``none``: the newcomer, added through the picker,
    carries only what the room said after they joined."""
    owner, newcomer = history_seats
    first = _api_peer(nest_instance, owner)
    second = _api_peer(nest_instance, owner)

    group_id, channel = _bootstrap_room_with_history(owner, first, second)
    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    assert owner_view.room["policy"]["history_policy"] == "None", owner_view.room

    # ── the action under test: the add through the picker ───────────────
    owner.app.conversations.open_thread_by_id(group_id)
    owner.app.conversations.add_participant_to_thread(newcomer.handle)
    assert not owner.app.has_error(), (
        f"the add must not refuse: {owner.app.error_text()!r}\n{seat_story(owner)}"
    )
    wait_for(
        "the owner's roster carrying the newcomer",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and newcomer.actor_hex in t.participant_actor_ids,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )

    # ── the newcomer's view, anchored on a post-join message ─────────────
    wait_thread_by_channel(newcomer, channel, what="the newcomer's Welcome applying")
    owner.app.conversations.real_send(group_id, AFTER)
    newcomer_view = wait_snippet(newcomer, channel, AFTER, what="the post-join message reaching the newcomer")
    assert newcomer_view.message_count == 1, (
        "under `none` the newcomer carries exactly the post-join message: "
        f"count={newcomer_view.message_count}\n{seat_story(newcomer)}"
    )
    newcomer_room = wait_room_role(newcomer, channel, what="the newcomer's room projection").room
    assert newcomer_room["my_role"] == "Member", newcomer_room
    assert newcomer_room["policy"]["history_policy"] == "None", (
        f"the newcomer reads the room's policy off the agreed group context: {newcomer_room}"
    )

    newcomer.app.conversations.open_thread_by_id(newcomer_view.thread_id)
    bodies = wait_for(
        "the newcomer's bubbles rendering",
        lambda: _rendered_bodies(newcomer.app),
        lambda b: any(AFTER in body for body in b),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(newcomer),
    )
    assert not any(BEFORE_ONE in b or BEFORE_TWO in b for b in bodies), (
        f"nothing said before the join renders under `none`: {bodies}"
    )


@pytest.mark.feature("group-conversations")
def test_a_newcomer_under_full_sees_the_conversation_so_far(history_seats, nest_instance):
    """The owner sets ``full`` through the editor, then adds the newcomer
    through the picker: the inviting device re-seals its slice, and the
    newcomer carries both pre-join messages beside the post-join one."""
    owner, newcomer = history_seats
    first = _api_peer(nest_instance, owner)
    second = _api_peer(nest_instance, owner)

    group_id, channel = _bootstrap_room_with_history(owner, first, second)
    wait_room_role(owner, channel, what="the owner's room projection")

    # ── the policy edit, through the editor ─────────────────────────────
    owner.app.conversations.open_thread_by_id(group_id)
    owner.app.conversations.open_room_settings()
    owner.app.conversations.set_room_history_policy("full")
    owner.app.conversations.save_room_settings()
    assert not owner.app.has_error(), (
        f"the owner may set policy: {owner.app.error_text()!r}\n{seat_story(owner)}"
    )
    wait_for(
        "the room's history policy reading `full`",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None
        and ((t.room or {}).get("policy") or {}).get("history_policy") == "Full",
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )

    # ── the add through the picker, then the newcomer's view ────────────
    owner.app.conversations.add_participant_to_thread(newcomer.handle)
    assert not owner.app.has_error(), (
        f"the add must not refuse: {owner.app.error_text()!r}\n{seat_story(owner)}"
    )
    wait_thread_by_channel(newcomer, channel, what="the newcomer's Welcome applying")
    owner.app.conversations.real_send(group_id, AFTER)
    newcomer_view = wait_snippet(newcomer, channel, AFTER, what="the post-join message reaching the newcomer")
    # The slice rides the same channel ahead of the post-join message, so
    # having the latter is the anchor for having folded the former; the
    # count is then a statement about the policy, not about timing.
    assert newcomer_view.message_count == 3, (
        "under `full` the newcomer carries the two pre-join messages and the "
        f"post-join one: count={newcomer_view.message_count}\n{seat_story(newcomer)}"
    )
    newcomer_room = wait_room_role(newcomer, channel, what="the newcomer's room projection").room
    assert newcomer_room["policy"]["history_policy"] == "Full", newcomer_room

    newcomer.app.conversations.open_thread_by_id(newcomer_view.thread_id)
    bodies = wait_for(
        "the newcomer's bubbles rendering",
        lambda: _rendered_bodies(newcomer.app),
        lambda b: any(AFTER in body for body in b),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(newcomer),
    )
    assert any(BEFORE_ONE in b for b in bodies) and any(BEFORE_TWO in b for b in bodies), (
        f"the conversation so far renders for the newcomer under `full`: {bodies}"
    )

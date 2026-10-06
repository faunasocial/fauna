"""Tier_3 — the room model's roles, end-to-end class, per app: three seats on one nest.

``conversation-rooms.md`` § Roles and authorization (the end-to-end arm: the
signed policy in the MLS group context; commit refusal), § The three classes,
§ The room. The room is born from the ordinary 1:1 → add-a-person fork
(``direct-messages.md`` § Group-Forked Threads) with every peer's key package
advertising the policy extension, so it is **governed**: the creator is the
owner, everyone else a plain member, and the policy is the initial one
(``invite`` join rule, ``none`` history).

**Three principals, three seats** (the review's correction 6): the owner,
a member who stays and witnesses what the room says after a removal, and a
member who first has their own removal attempt refused and is then removed by
the admin the owner appoints. Every seat is a real launch with its own MLS engine — the refusal
and the silence are each *that seat's own engine's* behavior, which one
process holding three identities could never show.

**What "refused client-side" looks like on tui — and why it is not the page's
``error-message``.** ``conversations.md`` § Architectural rules 5 greys a
role-gated affordance, never hides it, so a plain member's
``thread-member-chip`` is painted disabled off ``capabilities.can_remove_members``
and the automation gate refuses to drive it (convention 11's named 409,
``fauna_e2e_agent::gate_actuation``). That refusal is the observable here.
The manager's own pre-commit refusal — the one that DOES land on
``error-message`` (``error.send.room_remove_not_permitted``) — is the
belt-and-braces for a snapshot that has gone stale under the user's finger,
and it is pinned where it can be reached deterministically:
``libs/fauna-conversations/tests/fauna_mls_backend_tests.rs``.

**The silence is latency-independent by construction** (convention 14): the
remaining member's ``message_count`` growing proves the post-removal message
crossed the channel; a receive cycle of the removed seat that *began after*
that proof (``await_receive_cycle_after``) proves the removed seat has looked
for it; only then is its ``message_count`` asserted unchanged.

**The admin leg drives the policy editor** (``ui/conversations.md`` § Element
IDs, user-approved 2026-09-09): the owner opens ``thread-room-settings-button``,
flips the stayer's ``room-admin-toggle`` and saves; the stayer's own projection
flips to ``Admin`` off the agreed group context, and it is the *admin* who then
removes the leaver through the chip — the roles table's second row exercised
end to end, not the owner's.
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
    thread_by_id,
    wait_for,
    wait_room_role,
    wait_snippet,
    wait_thread_by_channel,
)
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads (`conversation-rooms.md` — "rendered on tui first"); the six
    # apps join through the batched trickle-down, each arriving with its own
    # marker AND a `helpers.room_seats.ROOM_APPS` entry. linux joined first,
    # then macos and ios (the apple leg), then windows, then web.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.web,
]

RECEIVE_CYCLE_BUDGET_S = 60.0
"""For the removed seat to complete one receive cycle begun after the proof —
its loop ticks every 2 s under the harness and is poked besides."""

_index_of = index_of


@pytest.mark.feature("group-conversations")
def test_a_room_is_born_governed_and_its_roles_govern_removal(room_seats, nest_instance):
    """The fork births an end-to-end room with the creator as owner; a plain
    member's remove is greyed and refused; the owner appoints an admin through
    the editor, and the admin's remove lands as a real
    Commit; the removed seat hears nothing the room says afterwards.

    The three seats of ``room_seats`` are the ``owner``, the ``stayer`` (the
    member who remains and witnesses) and the ``leaver`` (the member whose own
    removal attempt is refused, then removed)."""
    owner, stayer, leaver = room_seats
    port = nest_instance["port"]

    group_id, channel = bootstrap_room(owner, stayer, leaver)

    # ── every seat is in, and has the first message ──────────────────────
    for member, who in ((stayer, "stayer"), (leaver, "leaver")):
        wait_thread_by_channel(member, channel, what=f"the {who}'s Welcome applying")
        wait_snippet(member, channel, "hi room", what=f"'hi room' reaching the {who}")

    # ── the class and the roles, read off the projection on every seat ───
    owner_view = wait_room_role(owner, channel, what="the owner's room projection")
    room = owner_view.room
    assert room["class"] == "EndToEnd", f"three Fauna members are an end-to-end room: {room}"
    assert room["my_role"] == "Owner", f"the creator owns the room: {room}"
    assert room["policy"] is not None and room["policy"]["join_rule"] == "Invite", (
        f"a room born from the fork carries the initial policy: {room}"
    )
    assert room["policy"]["history_policy"] == "None", room
    assert room["member_roles"] == ["Member"] * len(owner_view.participant_actor_ids), (
        f"everyone the owner added is a plain member: {room} / "
        f"{owner_view.participant_actor_ids}"
    )
    caps = owner_view.capabilities
    assert caps["can_remove_members"] and caps["can_set_policy"] and caps["can_appoint_admins"], (
        f"the owner holds every role-gated capability: {caps}"
    )

    leaver_view = wait_room_role(leaver, channel, what="the leaver's room projection")
    assert leaver_view.room["class"] == "EndToEnd", leaver_view.room
    assert leaver_view.room["my_role"] == "Member", leaver_view.room
    leaver_caps = leaver_view.capabilities
    assert not leaver_caps["can_remove_members"], (
        f"a plain member may not remove under the roles table: {leaver_caps}"
    )
    assert not leaver_caps["can_invite"], (
        f"under the `invite` join rule a plain member may not invite: {leaver_caps}"
    )
    assert not leaver_caps["can_appoint_admins"] and not leaver_caps["can_set_policy"], (
        leaver_caps
    )

    # ── the plain member's removal attempt: greyed, refused, no Commit ───
    leaver.app.conversations.open_thread_by_id(leaver_view.thread_id)
    leaver_open = wait_for(
        "the leaver's chips rendering the roster",
        lambda: thread_by_channel(leaver.app, channel),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and leaver.app.conversations.member_chip_count() >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(leaver),
    )
    owner_chip = _index_of(leaver_open, owner.actor_hex)
    stayer_chip = _index_of(leaver_open, stayer.actor_hex)
    assert leaver.app.driver.get_attr("thread-member-chip", "role", index=owner_chip) == "owner", (
        f"the owner's chip carries the owner role: {leaver.app.driver.diagnose('thread-member-chip')}"
    )
    assert leaver.app.driver.get_attr("thread-member-chip", "role", index=stayer_chip) == "member"
    assert not leaver.app.driver.is_enabled("thread-member-chip"), (
        "a plain member's remove chip is greyed, never hidden "
        f"(`conversations.md` § Architectural rules 5): "
        f"{leaver.app.driver.diagnose('thread-member-chip')}"
    )
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    with pytest.raises(RuntimeError, match="disabled"):
        # The automation gate's named 409 — the app refuses to drive a
        # control the UI has disabled (convention 11), which is the client
        # side of "a Remove from a plain member is never authored".
        leaver.app.conversations.remove_member(stayer_chip)
    assert not leaver.app.has_error(), (
        f"a refused actuation never reaches the manager, so no page error: "
        f"{leaver.app.error_text()!r}"
    )
    still = thread_by_channel(leaver.app, channel)
    assert still is not None and still.participant_count == 2, (
        f"the roster is untouched on the refusing seat: {seat_story(leaver)}"
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) == envelopes_before, (
        "no Commit was posted for the refused removal"
    )

    # The editor's door is painted AND LIVE on a plain member's seat: the
    # surface carries the walk-out, which is a member's own verb (widened
    # 2026-09-20, user-approved; `ui/conversations.md` § Element IDs). What a
    # member may not do is greyed per control INSIDE it — witnessed by
    # `test_conversation_room_leave.py`, whose member seat opens this editor
    # and finds every policy control and Save dead and only the walk-out live.
    # Here the role's reach is the capability assertions above.
    assert leaver.app.driver.is_visible("thread-room-settings-button"), (
        f"the editor's door is painted on every seat: "
        f"{leaver.app.driver.diagnose('thread-room-settings-button')}"
    )
    assert leaver.app.driver.is_enabled("thread-room-settings-button"), (
        f"and live — any member of a room opens it: "
        f"{leaver.app.driver.diagnose('thread-room-settings-button')}"
    )
    assert leaver.app.conversations.room_class() == "end-to-end", (
        f"the header states the class: {leaver.app.driver.diagnose('thread-room-class')}"
    )

    # ── the owner appoints the stayer through the policy editor ──────────
    owner.app.conversations.open_thread_by_id(group_id)
    owner_open = wait_for(
        "the owner's chips rendering the roster",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and owner.app.conversations.member_chip_count() >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    assert owner.app.driver.is_enabled("thread-member-chip"), (
        f"the owner's chips are live: {owner.app.driver.diagnose('thread-member-chip')}"
    )
    stayer_index = _index_of(owner_open, stayer.actor_hex)
    owner.app.conversations.open_room_settings()
    assert not owner.app.conversations.room_admin_staged(stayer_index)
    owner.app.conversations.toggle_room_admin(stayer_index)
    assert owner.app.conversations.room_admin_staged(stayer_index)
    owner.app.conversations.save_room_settings()
    assert not owner.app.has_error(), (
        f"the owner's appointment must not refuse: {owner.app.error_text()!r}"
    )
    assert owner.app.driver.is_absent("room-settings-save-button"), (
        "the editor closes once the appointment commit landed\n"
        f"{seat_element(owner, 'room-settings-save-button')}\n"
        f"{seat_element(owner, 'thread-header')}\n"
        f"{seat_story(owner)}\n{seat_log(owner, 'room settings', 'room_settings', 'policy')}"
    )
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) > envelopes_before, (
        "the appointment is a real policy commit on the channel"
    )
    envelopes_before = len(conv_api.channel_fetch(port, owner.actor, channel, after=0))
    assert owner.app.driver.get_attr("thread-member-chip", "role", index=stayer_index) == "admin", (
        f"the owner's chip for the stayer carries the admin role: "
        f"{owner.app.driver.diagnose('thread-member-chip')}"
    )
    # The stayer learns its role from the agreed group context, not from
    # anything the owner's app told it.
    stayer_admin = wait_for(
        "the stayer's projection flipping to Admin",
        lambda: thread_by_channel(stayer.app, channel),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Admin",
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(stayer),
    )
    assert stayer_admin.capabilities["can_remove_members"], stayer_admin.capabilities
    assert stayer_admin.capabilities["can_set_policy"], stayer_admin.capabilities
    assert not stayer_admin.capabilities["can_appoint_admins"], (
        f"only the owner appoints: {stayer_admin.capabilities}"
    )

    # ── the admin removes the leaver through the chip ────────────────────
    stayer.app.conversations.open_thread_by_id(stayer_admin.thread_id)
    stayer_open = wait_for(
        "the admin's chips rendering the roster live",
        lambda: thread_by_channel(stayer.app, channel),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and stayer.app.conversations.member_chip_count() >= 2
        and stayer.app.driver.is_enabled("thread-member-chip"),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(stayer),
    )
    stayer_before = stayer_open
    leaver_before = thread_by_channel(leaver.app, channel)
    cycles = conv_receive_cycles(leaver.app.driver)
    assert cycles is not None, "every room app publishes conv_receive_cycles"
    # `await_receive_cycle_after` takes the STARTED count as its baseline.
    leaver_cycles = cycles[0]

    stayer.app.conversations.remove_member(_index_of(stayer_open, leaver.actor_hex))

    after = wait_for(
        "the admin's roster dropping the leaver",
        lambda: thread_by_channel(stayer.app, channel),
        lambda t: t is not None and t.participant_count == 1,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(stayer),
    )
    assert leaver.actor_hex not in after.participant_actor_ids, after.participant_actor_ids
    assert not stayer.app.has_error(), stayer.app.error_text()
    assert len(conv_api.channel_fetch(port, owner.actor, channel, after=0)) > envelopes_before, (
        "the admin's removal posts a real Commit on the channel"
    )
    # The owner folds the admin's commit: its roster drops the leaver too.
    wait_for(
        "the owner's roster following the admin's removal",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and t.participant_count == 1,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )

    # ── what the room says next reaches the stayer and not the leaver ────
    owner.app.conversations.real_send(group_id, "after the leaver left")
    wait_for(
        "the post-removal message reaching the stayer",
        lambda: thread_by_channel(stayer.app, channel),
        lambda t: t is not None and t.message_count > stayer_before.message_count,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(stayer),
    )
    # The stayer having it proves it is on the channel; a leaver cycle that
    # began after that proof proves the leaver looked. Then, and only then,
    # the leaver's count is a statement about the room and not about timing.
    poke_receive_cycle(leaver.app.driver)
    await_receive_cycle_after(
        leaver.app.driver,
        leaver_cycles,
        budget_s=RECEIVE_CYCLE_BUDGET_S,
        what="the leaver's next receive cycle",
    )
    leaver_after = thread_by_channel(leaver.app, channel)
    assert leaver_after is not None
    assert leaver_after.message_count == leaver_before.message_count, (
        "the removed seat must not decrypt what the room says after its removal: "
        f"before={leaver_before.message_count} after={leaver_after.message_count}\n"
        f"{seat_story(leaver)}"
    )
    assert "after the leaver left" not in (leaver_after.snippet or ""), leaver_after.snippet

    # ── the add arm: a seat seats a member it did not add ────────────────
    # `conversation-rooms.md` § The floor roster — the roster every member
    # renders is the one the group agrees on. The owner re-admits the leaver;
    # the STAYER authored nothing and must seat them off the agreed roster
    # alone. (The drop arm above is the same seam's other half: the owner
    # folded the admin's removal.)
    owner.app.conversations.real_add(group_id, leaver.actor_hex, leaver.handle)
    wait_for(
        "the owner's own roster carrying the re-admitted member",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and t.participant_count == 2,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    # ...and the nest's FLOOR ROSTER follows the room: every membership commit
    # above owed a report (`conversation-rooms.md` § The floor roster — the
    # committing device reports the resulting roster), and all three of them
    # named every current member. So once the owner's re-add is folded, the
    # floor must list all three — the witness included. This is what the
    # handle read below stands on, so asserting it here turns that read's
    # failure into one that names its cause: the reading seat's "not a member
    # of this room" is the SAME refusal whether the witness was dropped from a
    # stored roster or the nest never stored one at all.
    everyone = {owner.actor_hex, stayer.actor_hex, leaver.actor_hex}
    floor_attempts: list[str] = []

    def _floor_names():
        try:
            reply = conv_api.room_list_roster(port, owner.actor, channel)
        except Exception as e:  # the refusal IS the diagnostic
            floor_attempts.append(f"refused: {e}")
            return None
        names = {m["principal"] for m in reply.get("members", [])}
        floor_attempts.append(f"listed: {sorted(names)}")
        return names

    wait_for(
        "the nest's floor roster naming every current member",
        _floor_names,
        lambda names: names is not None and everyone <= names,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: "\n".join(floor_attempts[-5:]),
    )
    stayer_readmitted = wait_for(
        "the witness seating a member it did not add",
        lambda: thread_by_channel(stayer.app, channel),
        lambda t: t is not None and t.participant_count == 2,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(stayer),
    )
    assert leaver.actor_hex in stayer_readmitted.participant_actor_ids, (
        "the witness seats the re-admitted member by actor id, off the agreed "
        f"roster: {stayer_readmitted.participant_actor_ids}\n{seat_story(stayer)}"
    )
    # ...and the seat NAMES THE MEMBER BY HANDLE. The agreed roster carries
    # actor ids and nothing else, so the witness resolves a handle two ways:
    # at the seat, off what this device already renders elsewhere
    # (`ConversationsManager::seat_address_for`), and -- for a member it has
    # never met, which the witness may well be, having joined by Welcome --
    # off the home nest's floor roster, whose per-principal handle is joined
    # nest-side from `users.handle`
    # (`FaunaMlsBackend::resolve_nameless_members`, driven after the inbound
    # poll releases the channel lock). Both seats here are homed on THIS nest,
    # so that second path must resolve: the elided actor id is a transient
    # state here, never this test's resting answer. Before the read landed it
    # WAS a permitted resting answer, which is exactly what this assertion
    # used to have to allow.
    #
    # A deadline poll rather than a bare assert because that second path is
    # asynchronous by design (a nest round trip, deliberately off the ack
    # path), not because the answer is timing-dependent: the state waited for
    # is terminal, and the budget is a generous ceiling on one round trip
    # (convention 14).
    #
    # Either handle form is accepted. Which one lands depends on which path
    # got there first: the seat-time scan copies whatever another thread
    # renders (the bare handle the picker was given), while the floor-roster
    # read seats the canonical `handle@domain`
    # (`RoomRosterKnownMember::qualified_handle`, matching what
    # `resolve_address` stamps for a typed recipient). Pinning one would
    # assert which path won, which is not a property of the room.
    seat = stayer_readmitted.participant_actor_ids.index(leaver.actor_hex)

    def _seated_thread():
        t = thread_by_channel(stayer.app, channel)
        if t is None or leaver.actor_hex not in t.participant_actor_ids:
            return None
        return t

    def _names_the_leaver(t) -> bool:
        # `wait_for` hands the predicate whatever `read()` returned, `None`
        # included, so the guard belongs here — an AttributeError would
        # replace the budget's diagnostic failure with a bare traceback.
        if t is None:
            return False
        displays = t.participant_displays
        if len(displays) != len(t.participant_actor_ids):
            return False
        shown = displays[t.participant_actor_ids.index(leaver.actor_hex)]
        return shown == leaver.handle or shown.startswith(leaver.handle + "@")

    def _diag():
        # EVERY seat's roster lines, not only the witness's: the witness's read
        # can only fail because of what the REPORTERS sent, and the owner sends
        # two of this journey's three reports. A report the nest refused is
        # logged by the reporting seat (`fauna_client_conversations`,
        # "floor-roster report not delivered: <the nest's refusal>"), so a
        # witness-only diagnostic shows the symptom and hides the cause.
        sections = []
        for who, seat_ in (("owner", owner), ("stayer", stayer), ("leaver", leaver)):
            log = seat_.app.driver.app_stderr_text()
            interesting = "\n".join(
                ln for ln in log.splitlines()
                if "roster" in ln.lower() or "nameless" in ln.lower()
            )
            sections.append(f"--- {who}: roster/nameless log lines ---\n{interesting}")
        return seat_story(stayer) + "\n" + "\n".join(sections)

    named = wait_for(
        "the witness naming a member it never met, off the floor roster",
        _seated_thread,
        _names_the_leaver,
        MEMBERSHIP_BUDGET_S,
        diagnose=_diag,
    )
    displays = named.participant_displays
    # Asserted unconditionally, like `participant_actor_ids` right above: every
    # app this test runs on publishes the shared serializer's row, so an `if
    # displays:` guard here would not be defensive -- it would silently skip the
    # whole check the day the column went missing, which is the one regression
    # it exists to catch.
    assert len(displays) == len(named.participant_actor_ids), (
        "the chip-text column must be published and index-parallel with the "
        f"ids: displays={displays!r} ids={named.participant_actor_ids!r}"
        f"\n{seat_story(stayer)}"
    )
    elided = leaver.actor_hex[:12] + "…"
    assert displays[seat] != elided, (
        "the re-admitted member is homed on this very nest, so the floor "
        "roster's nest-joined handle must resolve it - an elided actor id "
        f"here is the id-keyed handle read failing: displays={displays!r}\n"
        f"{seat_story(stayer)}"
    )
    assert not stayer.app.has_error(), stayer.app.error_text()

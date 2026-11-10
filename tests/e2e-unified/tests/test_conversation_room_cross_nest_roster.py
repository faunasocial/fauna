"""Tier_3 — a room with a member from ANOTHER nest: names on both seats, and
that member's removal as an admin lands on the room's home.

``docs/features/group-conversations.md`` outcome 24: "In a room with someone
from another nest, the member list names them, they see the same names, and a
removal made by that nest's admin takes effect". The mechanisms are
``federation.md`` § Implementation status today — the id→handle announce (the
home's floor roster names a foreign member), the foreign member's OWN device
reading that roster through its own nest (``room.list_roster_remote``), and the
foreign member's own membership commits reaching the home's floor
(``room.roster_report_remote``). Their nest-side proof is
``conformance_cross_nest_conversations_client.rs``
(``a_foreign_member_reads_the_rooms_floor_through_its_own_nest``,
``a_foreign_members_membership_commit_reaches_the_rooms_home_floor``); this is
the same story driven through the app on every seat.

**Three seats, two nests — the conformance test's shape.** ``founder`` (home,
the owner), ``member`` (away, the foreign seat) and ``witness`` (home, a plain
member). Three seats make the room governed, and the witness is the one the
foreign admin removes: a removal of the owner is not an admin's to make.

**What the journey proves, in order:** the founder founds an end-to-end room
with a foreign handle and a same-nest member in the picker; both Welcomes land;
the founder's member list names the foreign member by handle, and the foreign
member's list names the founder and the witness — names it can only have read
off the home's floor through its own nest; the founder appoints the foreign
member admin through the room editor and the foreign seat learns its role
off the agreed group context; the foreign admin removes the witness through
the member chip; the founder's roster and the home nest's floor roster both
lose the witness.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state — never "sleep N then assert".
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    Seat,
    index_of,
    launch_seat,
    room_app,
    seat_app_path,
    seat_element,
    seat_log,
    seat_story,
    teardown_seats,
    thread_by_channel,
    wait_for,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS (`testing.md` § Default app and nest mode — a new outcome's
    # witness lands on the lead app first). The six others join through the
    # batched trickle-down, each with its own marker.
    pytest.mark.tui,
]

CROSSING_BUDGET_S = WELCOME_BUDGET_S
"""For a knock, a key-in or a roster read to cross nest → nest — the Welcome
budget, the same order of cross-nest round trip."""


@pytest.fixture()
def cross_nest_roster_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(founder, member, witness)``: the founder and the witness fresh
    accounts on ``cross_nest_foreign`` (the room's home), the member the handled
    actor ``cross_nest_foreign_ephemeral`` provisioned. Each invitee accepts the
    founder as a contact on its own nest, so the Welcome is admitted under
    the default reach policy."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        founder = launch_seat(home, app_path, room_app, request)
        seats.append(founder)
        member = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(member)
        witness = launch_seat(home, app_path, room_app, request)
        seats.append(witness)
        conv_api.accept_contact(away["port"], member.actor, founder.actor_hex, scheme="https")
        conv_api.accept_contact(home["port"], witness.actor, founder.actor_hex, scheme="https")
        _drain_harness_key_packages(away, member)
        yield founder, member, witness
    finally:
        teardown_seats(seats)


HARNESS_KEY_PACKAGES = 2
"""How many key packages ``provision_foreign_handled_actor`` mints harness-side
for the away actor (its default), so ``fauna.actor.by_handle`` reports it
addressable before any device exists."""


def _drain_harness_key_packages(away: dict, member: Seat) -> None:
    """Setup: consume the away actor's harness-minted key packages, so the
    founder's add claims one the member's DEVICE published.

    An end-to-end room seats a member by a Welcome sealed to a key package it
    claimed, and the nest hands out the oldest one-time package first
    (``db/channels.rs``, ``ORDER BY published_at ASC``). The fixture's packages
    were minted by no device, so a Welcome sealed to one is unopenable on the
    member's seat ("addressed to a key package this device does not hold").
    Once the device's own packages stand behind them, the two are taken off the
    front of the queue — harness state, not a user gesture."""
    port = away["port"]
    wait_for(
        "the member's device publishing key packages of its own",
        lambda: conv_api.keypackage_count(port, member.actor, member.actor_hex, scheme="https"),
        lambda n: n > HARNESS_KEY_PACKAGES,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_log(member, "key package", "keypackage"),
    )
    for _ in range(HARNESS_KEY_PACKAGES):
        assert conv_api.keypackage_fetch(port, member.actor, member.actor_hex, scheme="https"), (
            "a harness key package stands at the front of the queue"
        )


def _founded_room(seat, word):
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if (
            t.rail == "FaunaMls"
            and room.get("class") == "EndToEnd"
            and t.channel_id_hex
            and word in (t.snippet or "")
        ):
            return t
    return None


def _names(handle: str, shown: str) -> bool:
    """A seat names a member by handle in either form the shared layer seats
    (the bare handle a picker was given, or the canonical ``handle@domain``
    off the floor roster) — which one landed is which path won, not a property
    of the room (`test_conversation_room_roles.py`, the add arm)."""
    return shown == handle or shown.startswith(handle + "@")


def _named_thread(seat, room_id, expected: dict[str, str]):
    """The seat's room thread once it lists every actor in ``expected``
    (actor hex → handle) and names each by handle; ``None`` until then."""
    t = thread_by_channel(seat.app, room_id)
    if t is None:
        return None
    ids, shown = t.participant_actor_ids, t.participant_displays
    if len(shown) != len(ids) or not set(expected) <= set(ids):
        return None
    if all(_names(handle, shown[ids.index(a)]) for a, handle in expected.items()):
        return t
    return None


def _roster_diag(*seats) -> str:
    sections = []
    for seat in seats:
        sections.append(seat_story(seat))
        for t in seat.app.conversations.list_threads():
            if t.flavor != "OneOnOne":
                sections.append(
                    f"  {t.thread_id}: ids={t.participant_actor_ids} "
                    f"displays={t.participant_displays}"
                )
        sections.append(seat_log(seat, "roster", "nameless", "relay"))
    return "\n".join(sections)


@pytest.mark.feature("group-conversations")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_a_foreign_member_is_named_on_both_seats_and_their_removal_as_admin_lands_at_home(
    cross_nest_roster_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """Names from both seats, then a removal authored on the foreign nest's
    side reaching the room's home seat and floor."""
    founder, member, witness = cross_nest_roster_seats
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    member_address = f"{member.handle}@{away['authority']}"
    first_word = f"founding{secrets.token_hex(4)}"

    # ── founding: a foreign chip and a same-nest chip, home nest left out ─
    # The end-to-end class: the room's membership authority is its MLS group,
    # so a removal is a membership commit plus the committing device's roster
    # report — the path a foreign admin's removal reaches the home floor by
    # (`conversation-rooms.md` § The floor roster; `federation.md`, the
    # `conversation.roster.report` relay).
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(member_address)
    conv.add_recipient(witness.actor_hex)
    assert not conv.home_nest_included(), seat_element(founder, "recipient-picker-home-nest-toggle")
    assert conv.prospective_room_class() == "end-to-end", (
        f"with the home nest out, Fauna chips make an end-to-end room: "
        f"{seat_element(founder, 'recipient-picker-class')}"
    )
    founder.app.driver.type_text("dm-text-field", f"hello {first_word}")
    founder.app.driver.click("dm-send-button")

    room = wait_for(
        "the founder's room binding to its channel",
        lambda: _founded_room(founder, first_word),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'bootstrap', 'welcome', 'relay')}",
    )
    room_id = room.channel_id_hex
    assert not founder.app.has_error(), founder.app.error_text()
    assert (room.room or {}).get("my_role") == "Owner", f"the founder owns the room: {room.room}"

    # ── both Welcomes land, the foreign one through the relay ────────────
    for seat, who in ((member, "foreign member"), (witness, "witness")):
        wait_for(
            f"the founder's Welcome opening the room on the {who}'s seat",
            lambda seat=seat: thread_by_channel(seat.app, room_id),
            lambda t: t is not None and first_word in (t.snippet or ""),
            CROSSING_BUDGET_S,
            diagnose=lambda seat=seat: (
                f"{seat_story(seat)}\n{seat_log(seat, 'welcome', 'relay', 'inbox')}\n"
                f"{seat_log(founder, 'welcome', 'relay', 'federation')}"
            ),
        )

    # ── the names, from both seats ───────────────────────────────────────
    # The founder's list names the foreign member — the picker gave it the
    # handle, and the home's floor binds the announced one.
    founder_view = wait_for(
        "the founder's member list naming the foreign member by handle",
        lambda: _named_thread(founder, room_id, {member.actor_hex: member.handle}),
        lambda t: t is not None,
        CROSSING_BUDGET_S,
        diagnose=lambda: _roster_diag(founder),
    )
    # The foreign member joined by Welcome and met nobody: the names on its
    # seat come from the room home's floor roster, read through its own nest.
    wait_for(
        "the foreign member's list naming the founder and the witness, read through its own nest",
        lambda: _named_thread(
            member, room_id, {founder.actor_hex: founder.handle, witness.actor_hex: witness.handle}
        ),
        lambda t: t is not None,
        CROSSING_BUDGET_S,
        diagnose=lambda: _roster_diag(member, founder),
    )
    founder.app.conversations.open_thread_by_id(founder_view.thread_id)
    wait_for(
        "the founder's chips painting the roster",
        lambda: founder.app.conversations.member_chip_count(),
        lambda n: n >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_element(founder, "thread-member-chip"),
    )

    # ── the founder appoints the foreign member admin ────────────────────
    member_index = index_of(founder_view, member.actor_hex)
    conv.open_room_settings()
    conv.toggle_room_admin(member_index)
    assert conv.room_admin_staged(member_index), seat_element(founder, "room-admin-toggle")
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), (
        f"the owner's appointment must not refuse: {founder.app.error_text()!r}"
    )
    member_admin = wait_for(
        "the foreign member's projection flipping to Admin",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Admin",
        CROSSING_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'policy', 'role')}",
    )
    assert member_admin.capabilities["can_remove_members"], member_admin.capabilities

    # ── the foreign admin removes the witness through the chip ───────────
    member.app.conversations.open_thread_by_id(member_admin.thread_id)
    member_open = wait_for(
        "the foreign admin's chips rendering the roster live",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None
        and witness.actor_hex in t.participant_actor_ids
        and member.app.conversations.member_chip_count() >= 2
        and member.app.driver.is_enabled("thread-member-chip"),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_element(member, 'thread-member-chip')}",
    )
    member.app.conversations.remove_member(index_of(member_open, witness.actor_hex))
    wait_for(
        "the foreign admin's roster dropping the witness",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and witness.actor_hex not in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert not member.app.has_error(), member.app.error_text()

    # ── the removal takes effect at the room's home ──────────────────────
    after = wait_for(
        "the founder's roster following the foreign admin's removal",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and witness.actor_hex not in t.participant_actor_ids,
        CROSSING_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_story(member)}",
    )
    assert member.actor_hex in after.participant_actor_ids, after.participant_actor_ids

    floor_attempts: list[str] = []

    def _floor_names():
        try:
            reply = conv_api.room_list_roster(home["port"], founder.actor, room_id, scheme="https")
        except Exception as e:  # the refusal IS the diagnostic
            floor_attempts.append(f"refused: {e}")
            return None
        names = {m["principal"] for m in reply.get("members", [])}
        floor_attempts.append(f"listed: {sorted(names)}")
        return names

    wait_for(
        "the home nest's floor roster losing the witness, reported from the foreign nest's side",
        _floor_names,
        lambda names: names is not None
        and witness.actor_hex not in names
        and {founder.actor_hex, member.actor_hex} <= names,
        CROSSING_BUDGET_S,
        diagnose=lambda: "\n".join(floor_attempts[-5:])
        + "\n"
        + seat_log(member, "roster", "report", "relay"),
    )

"""Tier_3 — a room's deletes outlive a relaunch: the governed room's admin delete, and the community room's.

Two outcomes of the group-conversations page, one module because both are the
same question asked of the two Fauna room classes — *is a delete still there
when the app comes back?*

**The end-to-end class** (``conversation-rooms.md`` § Roles and
authorization, *Delete any message — the mechanism*): the owner deletes a plain
member's message; the member's own engine admits the cross-sender delete and
paints the tombstone; and after the member's app is quit and reopened against
its pinned store the tombstone is still there, the body nowhere — then the
same on the owner's seat. The member's seat is the meaningful one: its
tombstone is a verdict its engine reached about someone else's record, which
the ``history/<ch>`` slice must carry (``devices.md`` § Cross-device MLS
group-state sync, the ``history/<ch>`` contents bullet).

**The community class** (``community-rooms.md`` § The three classes), three
tests. The first is live: a member deletes their own message and reacts to the
founder's, both seen on the other seat; the founder deletes the member's other
message, leaving the same tombstone on both seats. The second asks the plain
relaunch question: the member, quit with that tombstone judged, reopens to the
room, its kept message and the tombstone (``surfaces.toml``
``community-room-relaunch-restore``). The third asks it of a delete still
unjudged at the quit (the harness surface ``room-roster-refusal-per-actor``):
the member's judgment of the founder's delete is made to PARK first. The home
nest refuses the MEMBER's roster reads alone (``room.list_roster``, the test
hook's refusal scoped to the member's actor → ``Unavailable`` → *not yet*) —
the founder's own delete reads the same kind for the version it signs under,
so a refusal of every caller would file no record at all — and the founder
then moves the room's policy one version (a settings-editor save), so the
delete names a version the member never anchored and cannot walk to. The
member's device holds the record unpainted while the founder's shows it
deleted. The member then quits. The parked record rides ``history/<ch>``
(§ Implementation status today, residual (d): *the parked floor delete
records*), so the reopened app — the refusal lifted — judges it and paints the
tombstone. Without that carriage the target would stay painted on the member's
account for ever while the founder saw it deleted.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll on painted state that, once reached, stays; "the member's device
has walked past the delete record" is read off a LATER message of the
founder's having painted there (the inbound walk is in order), never a delay;
the store is quit only once the replica's autosave has settled
(``wait_replica_settled``), so a red is the restore path's, not a race with the
save. Mutations go through the app UI (convention 8); the refusal is the
harness's fault injection, not a user act.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.connection import wait_until_online
from helpers.conversations_restart import wait_replica_settled
from helpers.room_seats import (  # noqa: F401 — room_app/room_seats/room_seat_pair are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    bootstrap_room,
    found_community_room,
    room_app,
    room_seat_pair,
    room_seats,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
    wait_snippet,
    wait_thread_by_channel,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads; the six others join through the batched trickle-down, each
    # arriving with its own marker (and its `helpers.room_seats.ROOM_APPS` entry).
    pytest.mark.tui,
    # web, linux and android: the community-room surfaces are built on all three;
    # android's device run is its venue's, the mark is the coverage contract.
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    # macos and ios: FaunaKit forwards every room control these journeys drive
    # to the same shared manager.
    pytest.mark.macos,
    pytest.mark.ios,
]

ROSTER_KIND = "fauna.conversations.room.list_roster"


def _bubble_texts(seat) -> list[str]:
    d = seat.app.driver
    return [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]


def _live_index(seat, needle: str, *, sender=None) -> int:
    """With ``seat``'s room open, wait for a live bubble carrying ``needle``
    and return its index among the live bubbles — also its
    ``dm-message-actions-button`` index, since a tombstone paints neither a
    text nor a ⋯."""
    texts = wait_for(
        f"a bubble carrying {needle!r} painting",
        lambda: _bubble_texts(seat),
        lambda ts: any(needle in t for t in ts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat)
        + (
            f"\n-- the sender --\nerror={sender.app.error_text()!r} "
            f"bubbles={_bubble_texts(sender)!r}\n{seat_story(sender)}"
            if sender is not None
            else ""
        ),
    )
    return next(i for i, t in enumerate(texts) if needle in t)


def _wait_tombstoned(seat, needle: str, tombstones: int, *, what: str) -> None:
    d = seat.app.driver
    wait_for(
        what,
        lambda: (d.count("dm-message-deleted"), _bubble_texts(seat)),
        lambda seen: seen[0] >= tombstones and not any(needle in t for t in seen[1]),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(seat)}\n{d.diagnose('dm-message-deleted')}",
    )


def _quit_pinned(seat, nest_instance, channel: str) -> None:
    """Close ``seat``'s app with its store pinned, once its copy of the room
    has settled on the nest — so the reopened app is the same device, and a
    red after the relaunch is the restore's."""

    def client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=seat.actor["actor_id_bytes"],
            signing_key=bytes(seat.actor["signing_key"]),
        )

    wait_replica_settled(client, f"history/{channel}")
    wait_replica_settled(client, "provider")
    seat.app.driver.quit_app()


def _reopen(seat, channel: str) -> None:
    """Relaunch ``seat`` against its pinned store and open the room again."""
    seat.app.driver.hard_reload()
    wait_until_online(seat.app.driver)
    seat.app.conversations.enable_real_faunamls()
    wait_thread_by_channel(seat, channel, what="the reopened app restoring the room")
    seat.app.conversations.navigate()
    seat.app.conversations.open_thread_by_channel(channel)


def _pin(seat) -> None:
    assert seat.app.driver.preserve_state_across_relaunch(), (
        "the seat's store must survive the relaunch, or the reopened app is a "
        "fresh device and the survival assertion is vacuous"
    )


@pytest.mark.feature("group-conversations")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
def test_an_owners_delete_of_a_members_message_survives_a_relaunch(room_seats, nest_instance):
    """The owner deletes a member's message; the tombstone is still on the
    member's seat, and on the owner's, after each app is quit and reopened."""
    owner, stayer, member = room_seats
    word = secrets.token_hex(4)
    target = f"the member's words {word}"
    kept = f"the member's kept line {word}"
    _group_id, channel = bootstrap_room(owner, stayer, member)
    wait_thread_by_channel(member, channel, what="the member's Welcome applying")
    wait_snippet(member, channel, "hi room", what="'hi room' reaching the member")
    _pin(member)
    _pin(owner)

    member_thread = thread_by_channel(member.app, channel)
    for body in (target, kept):
        member.app.conversations.real_send(member_thread.thread_id, body)
        wait_snippet(owner, channel, body, what=f"{body!r} reaching the owner")

    owner.app.conversations.open_thread_by_id(thread_by_channel(owner.app, channel).thread_id)
    member.app.conversations.open_thread_by_id(member_thread.thread_id)
    owner.app.conversations.delete_message(message_index=_live_index(owner, target))
    assert not owner.app.has_error(), owner.app.error_text()
    for seat, who in ((owner, "owner's own"), (member, "author's")):
        _wait_tombstoned(seat, target, 1, what=f"the owner's delete tombstoning on the {who} seat")

    # ── the author's app: quit, reopen, the tombstone stands ─────────────
    _quit_pinned(member, nest_instance, channel)
    _reopen(member, channel)
    _live_index(member, kept)  # the room's other words came back: the restore ran
    _wait_tombstoned(member, target, 1, what="the tombstone standing on the reopened author's seat")

    # ── the owner's app: the same ────────────────────────────────────────
    _quit_pinned(owner, nest_instance, channel)
    _reopen(owner, channel)
    _live_index(owner, kept)
    _wait_tombstoned(owner, target, 1, what="the tombstone standing on the reopened owner's seat")
    for seat in (owner, member):
        assert not seat.app.has_error(), seat.app.error_text()


@pytest.mark.feature("group-conversations")
def test_a_community_rooms_own_delete_reaction_and_admin_delete_reach_every_seat(room_seat_pair):
    """In a community room: a member's own delete and reaction cross to the
    founder; the founder's delete of the member's message leaves the same
    tombstone on both seats — the member's painted by its own engine's
    verdict on the founder's rank."""
    founder, member = room_seat_pair
    word = secrets.token_hex(4)
    _room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    fconv = founder.app.conversations
    mconv = member.app.conversations
    fconv.open_thread_by_id(founder_thread)
    mconv.open_thread_by_id(member_thread)

    own = f"the member takes this back {word}"
    target = f"the founder takes this away {word}"
    kept = f"the member keeps this {word}"
    react_to = f"react to me {word}"

    # ── the member's three messages, the founder's one ───────────────────
    for body in (own, target, kept):
        mconv.send_in_open_thread(body)
        _live_index(founder, body, sender=member)
    fconv.send_in_open_thread(react_to)

    # ── a reaction crosses ───────────────────────────────────────────────
    mconv.react_to_message(message_index=_live_index(member, react_to), reaction_index=0)
    mconv.wait_for_reaction_pill(expected_count=1, timeout_s=MESSAGE_BUDGET_S)
    fconv.wait_for_reaction_pill(expected_count=1, timeout_s=MESSAGE_BUDGET_S)
    assert not member.app.has_error(), member.app.error_text()

    # ── an own delete crosses ────────────────────────────────────────────
    mconv.delete_message(message_index=_live_index(member, own))
    assert not member.app.has_error(), member.app.error_text()
    for seat, who in ((member, "author's own"), (founder, "founder's")):
        _wait_tombstoned(seat, own, 1, what=f"the member's own delete tombstoning on the {who} seat")

    # ── the founder deletes the member's message: one tombstone for all ──
    fconv.delete_message(message_index=_live_index(founder, target))
    assert not founder.app.has_error(), founder.app.error_text()
    for seat, who in ((founder, "founder's own"), (member, "author's")):
        _wait_tombstoned(seat, target, 2, what=f"the founder's delete tombstoning on the {who} seat")
    assert any(kept in t for t in _bubble_texts(member)), (
        f"only the deleted messages went: {_bubble_texts(member)!r}"
    )
    for seat in (founder, member):
        assert not seat.app.has_error(), seat.app.error_text()


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("community-room-relaunch-restore")
def test_a_community_room_comes_back_with_its_messages_and_tombstone_after_a_relaunch(
    room_seat_pair, nest_instance
):
    """A community room, quit with a founder's delete already judged on the
    member's seat, is listed again on the reopened member's app — its kept
    message there, the tombstone still standing. A community room has no MLS
    group, so only its community marker lists it in the replica
    (``devices.md`` § Durability rules, *A community room is listed by its
    marker*); without it the reopened app listed no community room at all."""
    founder, member = room_seat_pair
    word = secrets.token_hex(4)
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    fconv = founder.app.conversations
    mconv = member.app.conversations
    fconv.open_thread_by_id(founder_thread)
    mconv.open_thread_by_id(member_thread)
    target = f"the founder takes this away {word}"
    kept = f"the member keeps this {word}"
    for body in (target, kept):
        mconv.send_in_open_thread(body)
        _live_index(founder, body, sender=member)

    _pin(member)
    fconv.delete_message(message_index=_live_index(founder, target))
    assert not founder.app.has_error(), founder.app.error_text()
    _wait_tombstoned(member, target, 1, what="the founder's delete tombstoning on the member's seat")
    _quit_pinned(member, nest_instance, room_id)

    _reopen(member, room_id)
    _live_index(member, kept)
    _wait_tombstoned(
        member, target, 1, what="the tombstone standing on the reopened member's seat"
    )
    assert not member.app.has_error(), member.app.error_text()


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("community-room-relaunch-restore")
# The fault injection needs the member-scoped refusal and the policy move
# (module doc); an app without the room settings editor lacks the surface.
@pytest.mark.requires_surface("room-roster-refusal-per-actor")
def test_a_community_rooms_delete_parked_at_quit_is_applied_after_the_relaunch(
    room_seat_pair, nest_instance
):
    """The founder's delete of the member's message, its judgment parked on the
    member's device when the member quits, tombstones there after the
    relaunch."""
    from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc

    founder, member = room_seat_pair
    port = nest_instance["port"]
    word = secrets.token_hex(4)
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {word}"
    )
    fconv = founder.app.conversations
    mconv = member.app.conversations
    fconv.open_thread_by_id(founder_thread)
    mconv.open_thread_by_id(member_thread)
    target = f"the founder takes this away {word}"
    kept = f"the member keeps this {word}"
    after = f"and this comes after {word}"
    for body in (target, kept):
        mconv.send_in_open_thread(body)
        _live_index(founder, body, sender=member)

    before = wait_for(
        "the founder's projection reading the room's policy",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and ((t.room or {}).get("policy") or {}).get("version"),
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    version_before = before.room["policy"]["version"]

    # ── the founder's delete, its judgment parked on the member's device ─
    _pin(member)
    refuse_rpc(port, ROSTER_KIND, actor=member.actor_hex)
    try:
        # The founder moves the policy one version, so the delete it signs
        # next names a version the member never anchored — and the member's
        # walk to it is refused. The founder's own reads pass.
        fconv.open_room_settings()
        fconv.set_room_history_policy("full")
        fconv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
        assert not founder.app.has_error(), founder.app.error_text()
        wait_for(
            "the founder's projection reading the moved policy",
            lambda: thread_by_channel(founder.app, room_id),
            lambda t: t is not None
            and ((t.room or {}).get("policy") or {}).get("version") == version_before + 1,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'policy')}",
        )
        fconv.delete_message(message_index=_live_index(founder, target))
        assert not founder.app.has_error(), founder.app.error_text()
        _wait_tombstoned(founder, target, 1, what="the founder's delete tombstoning on its own seat")
        # A later message painting on the member's seat means its inbound walk
        # has passed the delete record (the walk is in order) — judged, and
        # with the version read refused, parked.
        fconv.send_in_open_thread(after)
        _live_index(member, after, sender=founder)
        wait_for_refused_rpc(port, ROSTER_KIND)
        assert any(target in t for t in _bubble_texts(member)), (
            "precondition: with the policy-version read refused the member's "
            "device cannot rank the founder's delete, so the target must still "
            f"paint there: {_bubble_texts(member)!r}\n"
            f"{seat_element(member, 'dm-message-deleted')}"
        )
        _quit_pinned(member, nest_instance, room_id)
    finally:
        release_rpc_hold(port, ROSTER_KIND)

    # ── the reopened member applies the parked delete ────────────────────
    _reopen(member, room_id)
    _live_index(member, kept)
    _wait_tombstoned(
        member, target, 1, what="the parked delete judged and painted on the reopened member's seat"
    )
    assert not member.app.has_error(), member.app.error_text()

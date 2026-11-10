"""Tier_3 — a community room joined from ANOTHER nest: two seats, two nests.

``conversation-rooms.md`` § Done definition's community box closes on one
clause this journey witnesses and ``test_conversation_room_community.py``
(both seats on one nest) cannot: "sealed log opened by a second member **on a
second nest** through the relay". The ruling it drives is § Join rules and
invites → *A cross-nest invitation* — the room's home judges the invitation
and pushes the knock to the invitee's own nest; that nest runs the invitee's
reach policy and relays the acceptance back; the home seats and binds; the
founder's device keys the foreign seat in exactly as a same-nest one; and the
newcomer reads and writes the room through its own nest's relay.

**Topology — two real nest binaries, each with a handle domain and floor
TLS**, because each has to dial the other: the room's home (``home``,
``cross_nest_foreign``) delivers the knock to the member's nest and declares
the address the acceptance is relayed back to, and the member's nest
(``away``, ``cross_nest_foreign_ephemeral``) relays that acceptance and every
later read of the room. The founder is a fresh account on ``home``; the member
is ``away``'s pre-provisioned handled actor, so the founder can address them
as ``handle@authority`` in the recipient picker.

**What the journey proves, in order:** the composer takes a foreign handle
and the home-nest choice makes the room a community room; the first send
founds it and invites across the nest boundary; the invitation stands atop
the member's list on the other nest and accepting it (through their own
nest) opens the room; the founder's device keys the foreign seat with no
gesture; a message crosses each way, sealed under the room's generation,
through the member's relay. A third journey seats a second home account
beside them and has the foreign member, appointed admin, remove it through
the relayed removal and rotation.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state — never "sleep N then assert".
"""

from __future__ import annotations

import secrets

import pytest

from helpers.labeler_publish import FIXTURES, publish_wasm_labeler
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    Seat,
    admit_web_seat_origins,
    found_community_room,
    index_of,
    launch_seat,
    room_app,
    seat_app_path,
    seat_element,
    seat_log,
    seat_story,
    teardown_seats,
    thread_by_channel,
    thread_by_id,
    wait_for,
)
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS (`community-rooms.md` — the lead app paints every new room
    # gesture first). The six others join through the batched trickle-down,
    # each arriving with its own marker AND a `helpers.room_seats.ROOM_APPS`
    # entry.
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]

CROSSING_BUDGET_S = WELCOME_BUDGET_S
"""For a knock to cross nest → nest and stand on the member's list, and for
an acceptance to cross back and the member's seat to be keyed through the
relay — the Welcome budget, the same order of cross-nest round trip."""


@pytest.fixture()
def cross_nest_room_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(founder, member)`` on two nests: the founder a fresh account on
    ``cross_nest_foreign`` (the room's home), the member the handled actor
    ``cross_nest_foreign_ephemeral`` provisioned (their own home). The member
    accepts the founder as a contact on *their* nest, exactly as the same-nest
    pair does — the invitation is judged against that edge where the member
    lives."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        founder = launch_seat(home, app_path, room_app, request)
        seats.append(founder)
        member = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(member)
        # Each web seat's page is served from a proxy of its own, born after
        # these session-scoped nests were seeded; the member's attachment
        # upload goes raw to the home, so both nests admit both pages.
        admit_web_seat_origins([home, away], seats, request)
        conv_api.accept_contact(away["port"], member.actor, founder.actor_hex, scheme="https")
        yield founder, member
    finally:
        teardown_seats(seats)


def _founded_room(seat):
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
            return t
    return None


@pytest.mark.android
@pytest.mark.feature("group-conversations")
def test_a_member_of_another_nest_joins_a_community_room_through_the_relay(
    cross_nest_room_seats, cross_nest_foreign_ephemeral
):
    """Found on one nest with a foreign handle in the picker; the knock crosses;
    accepting through the member's own nest seats them; the founder keys the
    foreign seat; a sealed message crosses each way through the relay."""
    founder, member = cross_nest_room_seats
    away = cross_nest_foreign_ephemeral
    member_address = f"{member.handle}@{away['authority']}"
    first_word = f"founding{secrets.token_hex(4)}"
    reply_word = f"answer{secrets.token_hex(4)}"
    later_word = f"welcome{secrets.token_hex(4)}"

    # ── founding: a foreign chip plus the home-nest choice ───────────────
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(member_address)
    conv.toggle_home_nest()
    assert conv.home_nest_included(), seat_element(founder, "recipient-picker-home-nest-toggle")
    assert conv.prospective_room_class() == "community", (
        f"the class statement follows the choice: {seat_element(founder, 'recipient-picker-class')}"
    )
    founder.app.driver.type_text("dm-text-field", f"hello {first_word}")
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

    # ── the knock crosses: it stands atop the member's list on THEIR nest ─
    wait_for(
        "the founder's invitation crossing to the member's nest",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}\n"
            f"{seat_log(founder, 'invite', 'federation')}"
        ),
    )
    member.app.conversations.accept_room_invitation(0)
    joined = wait_for(
        "the accepted room opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'accept', 'room', 'relay')}",
    )
    assert not member.app.has_error(), (
        f"accepting through the member's own nest must not refuse: "
        f"{member.app.error_text()!r}\n{seat_story(member)}"
    )
    assert member.app.conversations.room_invitation_count() == 0, (
        "an accepted invitation stops standing"
    )

    # ── the key-in crosses too: the founder's device keys the foreign seat ─
    wait_for(
        "the foreign seat being keyed in by the founder's device",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'key-in', 'key_in', 'tend', 'mint')}\n"
            f"{seat_log(member, 'generation', 'remote', 'relay')}"
        ),
    )

    # ── a sealed message each way, through the member's relay ────────────
    conv.open_thread_by_id(room.thread_id)
    conv.send_in_open_thread(f"glad you came {later_word}")
    wait_for(
        "the founder's message opening on the member's seat across the relay",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and later_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'room', 'fetch')}",
    )
    member.app.conversations.open_thread_by_id(joined.thread_id)
    member.app.conversations.send_in_open_thread(f"thanks {reply_word}")
    wait_for(
        "the member's reply opening on the founder's seat",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and reply_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_story(member)}",
    )
    assert (thread_by_channel(member.app, room_id).room or {}).get("class") == "Community", (
        "the newcomer's projection states the class it joined"
    )


@pytest.mark.android
@pytest.mark.feature("group-conversations")
@pytest.mark.feature("room-restricted-posts")
def test_a_member_of_another_nest_searches_the_room_and_opens_a_room_post_through_the_relay(
    cross_nest_room_seats, cross_nest_foreign, cross_nest_foreign_ephemeral, run_seal_helper
):
    """The room's search from the member's seat on ANOTHER nest
    (``room-search.md`` § The surface), and a post hit opening the room post
    the member's own nest does not hold (``ui/feed.md`` § The read model →
    *Opening a post the timeline never loaded* → *A post homed on another
    nest*).

    The founder on ``home`` addresses a room post to the room through the
    feed composer — its sealed words carry this run's marker, its teaser
    does not. The member on ``away`` follows nobody, so nothing brings the
    post to their nest: the strip's submit rides ``away``'s
    ``room.search_remote`` relay to the room's home, the post hit's row is
    painted from the envelope ``away`` relays from ``home``
    (``posts.get_remote``), and opening the hit lands on the feed's
    ``post_detail`` showing the teaser, resolved through the same relay.

    Then the detail **unseals the sealed words**: the sealed body rests on
    ``home``, which serves the self-signed floor, so the member's app fetches
    it over the foreign byte plane pinned against the identity ``away``
    stamped on the relay reply (``transport-trust.md`` § Mechanism for the
    federation-granted row → *The post arm*). No harness CA and no trust
    seed: the two nests' unchanged floor IS the production self-signed-home
    branch — with the stamp withheld the fetch fails at TLS and the detail
    stays on the teaser.

    The room's labels on that post (``docs/features/room-restricted-posts.md``
    outcome 21): the founder publishes the fixture labeler on ``home`` and
    names it in the room's settings before the post, whose sealed words carry
    its trigger ("cat") and whose teaser does not. The shared manager folds the
    room's relayed verdicts, read by ``home`` and relayed by ``away``, onto the
    opened post when it unseals, so the detail's ``content-label-badge`` reads
    the room labeler's ``commercial`` verdict — a post opened through the
    deep-link door has no ``post-card``, so the detail is where the member
    sees it.

    Latency-independent (convention 14): every wait is a deadline poll over
    a painted row, the detail body or ``error-message``."""
    founder, member = cross_nest_room_seats
    away = cross_nest_foreign_ephemeral
    labeler = publish_wasm_labeler(
        run_seal_helper, cross_nest_foreign, FIXTURES / "room_commercial_labeler.wat"
    ).hex()
    marker = f"whiskers{secrets.token_hex(4)}"
    preview = f"A post for the room {secrets.token_hex(3)}"
    full_body = f"Only the room reads this: my cat says {marker}"

    room_id, founder_thread, member_thread = found_community_room(
        founder,
        member,
        f"hello {secrets.token_hex(4)}",
        address=f"{member.handle}@{away['authority']}",
    )

    # ── The founder names the labeler in the room's settings. ─────────────
    founder_conv = founder.app.conversations
    founder_conv.open_thread_by_id(founder_thread)
    founder_conv.open_room_settings()
    row = founder_conv.room_labeler_index(labeler)
    assert row is not None, seat_element(founder, "room-labeler-toggle")
    founder_conv.toggle_room_labeler(row)
    founder_conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the room's verified labeler set naming the fixture",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("labelers") == [labeler],
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'labeler')}",
    )

    # ── The founder addresses a post to the room, through the composer. ──
    option = S.feed.post.gate_room(room=thread_by_id(founder.app, founder_thread).label)
    founder.app.feed.navigate()
    assert founder.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
        f"error-message={founder.app.error_text()!r}\n{seat_story(founder)}"
    )
    founder.app.feed.create_gated_post(full_body, preview, option)

    # ── The member searches the room for words only the sealed body holds. ─
    conv = member.app.conversations
    conv.open_thread_by_id(member_thread)
    # The door is painted off the room's FLOOR (class and this seat's rank),
    # which the member's device reads through its own nest's relay on a later
    # poll than the key-in — until then the room projects from its
    # participants alone and offers no search.
    wait_for(
        "the member's device reading the room's floor and offering its search",
        conv.room_search_offered,
        lambda offered: offered,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: (
            f"room={(thread_by_id(member.app, member_thread).room or {})!r}\n"
            f"{seat_element(member, 'room-search-button')}\n{seat_story(member)}\n"
            f"{seat_log(member, 'floor', 'roster', 'relay')}"
        ),
    )

    def post_hit():
        # Re-submitted each pass: the home indexes the post on its own
        # reception pass, so an early search may truly answer nothing.
        conv.search_room(marker)
        rows = conv.room_search_results()
        return next((i for i, (kind, _) in enumerate(rows) if kind == "post"), None)

    at = wait_for(
        "the room's search on the home nest answering the room post",
        post_hit,
        lambda i: i is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"error-message={member.app.error_text()!r}\n{seat_story(member)}\n"
            f"{seat_log(member, 'search', 'relay', 'get_remote')}"
        ),
    )
    _, words = conv.room_search_results()[at]
    assert preview in words, (
        f"the post row is painted from the envelope relayed from the room's home: {words!r}"
    )
    assert not member.app.has_error(), member.app.error_text()

    # ── The hit opens the post the member's nest does not hold. ───────────
    conv.open_room_search_result(at)
    member.app.driver.wait_for("feed-post-detail-body", timeout=MESSAGE_BUDGET_S)
    wait_for(
        "the post detail showing the relayed room post",
        member.app.feed.post_detail_body,
        # The teaser — or already the sealed words, if the unseal below won
        # the race to the first read.
        lambda text: preview in text or marker in text,
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"room={room_id} error-message={member.app.error_text()!r}\n{seat_story(member)}\n"
            f"{seat_log(member, 'get_remote', 'resolve', 'blob')}"
        ),
    )

    # ── The sealed words open, fetched from the room's home over the pin. ─
    # A browser holds no pin; the harness's `ignore_https_errors` lets web's
    # direct fetch through, so on web this witnesses the home pick and the fetch
    # (`transport-trust.md` § The post arm, *The witness splits the mile*).
    wait_for(
        "the post detail unsealing the room post's words from its home nest",
        member.app.feed.post_detail_body,
        lambda text: marker in text,
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"room={room_id} error-message={member.app.error_text()!r}\n{seat_story(member)}\n"
            f"{seat_log(member, 'graduate', 'pin', 'blob', 'unlock', 'tls')}"
        ),
    )
    assert not member.app.has_error(), (
        f"a foreign home's byte leg never surfaces a page error: {member.app.error_text()!r}"
    )

    # ── The room's verdict paints on the opened post's detail. ────────────
    def detail_badge():
        driver = member.app.driver
        if driver.count("content-label-badge") == 0:
            return None
        return driver.get_text("content-label-badge")

    badge = wait_for(
        "the room's verdict painting a label on the opened post's detail",
        detail_badge,
        lambda text: text is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"room={room_id} error-message={member.app.error_text()!r}\n{seat_story(member)}\n"
            f"{seat_log(member, 'room_labels', 'label', 'relay')}"
        ),
    )
    assert S.moderation.category.commercial in badge, (
        f"the detail names the room labeler's verdict "
        f"({S.moderation.category.commercial!r}); got {badge!r}"
    )


@pytest.fixture()
def cross_nest_room_trio(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(founder, member, third)``: :func:`cross_nest_room_seats`' pair plus a
    THIRD seat, a second fresh account on the room's home (``home``), which
    accepts the founder as a contact there — the member a foreign admin's
    removal will unseat."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        founder = launch_seat(home, app_path, room_app, request)
        seats.append(founder)
        member = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(member)
        third = launch_seat(home, app_path, room_app, request)
        seats.append(third)
        admit_web_seat_origins([home, away], seats, request)
        conv_api.accept_contact(away["port"], member.actor, founder.actor_hex, scheme="https")
        conv_api.accept_contact(home["port"], third.actor, founder.actor_hex, scheme="https")
        yield founder, member, third
    finally:
        teardown_seats(seats)


def _home_floor(home: dict, reader: dict, room: str) -> list[str] | None:
    """The live floor's principal hexes, read off the room's HOME nest by
    ``reader`` (a seat homed there), or ``None`` on a refusal worth polling."""
    try:
        roster = conv_api.room_list_roster(home["port"], reader, room, scheme="https")
    except Exception:
        return None
    return [m["principal"] for m in roster["members"]]


def _home_tip(home: dict, reader: dict, room: str) -> str | None:
    """The id of the tip generation wrapped to ``reader`` on the room's home."""
    try:
        gens = conv_api.room_generations(home["port"], reader, room, scheme="https")
    except Exception:
        return None
    return next((g["generation_id"] for g in gens if g.get("is_tip")), None)


@pytest.mark.feature("group-conversations")
def test_a_foreign_admin_removes_a_home_member_through_the_relay(
    cross_nest_room_trio, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """A member homed on ANOTHER nest, appointed admin, removes a member homed
    on the room's home through their chip — the relayed twins of `room.remove`
    and `room.publish_generation` (``federation.md`` § Federation residue
    surface, the *Conversation room remove* and *… generation publish and
    backfill* rows): the unseat, then the severance mint, each through the
    admin's own nest to the room's home. The same-nest journey's closing leg
    (``test_conversation_room_community.py``), run from the foreign seat.

    Every mutation is a gesture (convention 8): the founding with two chips,
    both acceptances, the founder's appointment in the policy editor, the
    admin's chip tap. The witness is the home nest's own floor and tip, read
    by the founder (convention 5); every wait is a deadline poll (convention
    14)."""
    founder, member, third = cross_nest_room_trio
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral

    # ── founding: the foreign member's handle and the home third's id ─────
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(f"{member.handle}@{away['authority']}")
    conv.add_recipient(third.actor_hex)
    conv.toggle_home_nest()
    assert conv.prospective_room_class() == "community", (
        f"{seat_element(founder, 'recipient-picker-class')}"
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

    # ── both invitees accept, each through their own nest ────────────────
    for seat, budget in ((member, CROSSING_BUDGET_S), (third, WELCOME_BUDGET_S)):
        wait_for(
            "the invitation standing on the invitee's list",
            lambda seat=seat: seat.app.conversations.room_invitation_count(),
            lambda n: n >= 1,
            budget,
            diagnose=lambda seat=seat: f"{seat_story(seat)}\n{seat_log(seat, 'invitation', 'room')}",
        )
        seat.app.conversations.accept_room_invitation(0)
        wait_for(
            "the invitee's seat being keyed in",
            lambda seat=seat: thread_by_channel(seat.app, room_id),
            lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
            CROSSING_BUDGET_S,
            diagnose=lambda seat=seat: (
                f"{seat_story(seat)}\n{seat_story(founder)}\n"
                f"{seat_log(founder, 'key-in', 'key_in', 'tend', 'mint')}"
            ),
        )
        assert not seat.app.has_error(), seat.app.error_text()

    # ── the founder appoints the foreign member admin ────────────────────
    listed = wait_for(
        "both invitees listed on the founder's thread",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None
        and member.actor_hex in t.participant_actor_ids
        and third.actor_hex in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    conv.open_thread_by_id(listed.thread_id)
    conv.open_room_settings()
    member_index = index_of(listed, member.actor_hex)
    conv.toggle_room_admin(member_index)
    assert conv.room_admin_staged(member_index)
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()

    admin_view = wait_for(
        "the foreign member's projection granting the removal",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None
        and (t.capabilities or {}).get("can_remove_members")
        and third.actor_hex in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'floor', 'roster', 'policy')}",
    )

    # ── the foreign admin removes the home member through the chip ───────
    tip_before = _home_tip(home, founder.actor, room_id)
    assert tip_before is not None, "the founder holds the room's tip before the removal"
    mconv = member.app.conversations
    mconv.open_thread_by_id(admin_view.thread_id)
    mconv.remove_member(index_of(admin_view, third.actor_hex))
    wait_for(
        "the third member leaving the home nest's floor",
        lambda: _home_floor(home, founder.actor, room_id),
        lambda seats: seats is not None and third.actor_hex not in seats,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n{seat_story(member)}\n"
        f"{seat_log(member, 'remove', 'room', 'relay')}",
    )
    wait_for(
        "the foreign admin's removal rotating the room's generation on its home",
        lambda: _home_tip(home, founder.actor, room_id),
        lambda tip: tip is not None and tip != tip_before,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n"
        f"{seat_log(member, 'remove', 'rotate', 'mint', 'relay')}",
    )
    assert member.actor_hex in (_home_floor(home, founder.actor, room_id) or []), (
        "the removal was scoped to its target — the admin stays seated"
    )
    assert not member.app.has_error(), (
        f"the relayed removal and its relayed rotation both landed: {member.app.error_text()!r}"
    )

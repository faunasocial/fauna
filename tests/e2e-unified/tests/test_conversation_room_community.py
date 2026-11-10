"""Tier_3 — a community room, founded, joined and searched from the app: two seats on one nest.

``community-rooms.md`` § The three classes: a community room is the class
whose home nest holds a read, which is what lets it search and label the
room for its members. Nest-side the class has been complete since
2026-09-09, and the shared manager has founded, invited, joined, keyed and
rotated it since 2026-09-10 — but no app could reach any of it: the room had
no founding control, no invitation list and no read control
(§ Implementation status today, *Still dark*). This journey is those three
controls, driven the way a user drives them.

**Two principals, two seats** — the ``founder`` and a ``member``. No witness
is needed: everything the story asserts about the room's third party, the
home nest, is read off the nest itself through the member's own
``room.search``.

**What the journey proves, in order:** the composer's home-nest choice turns
the room about to be created into a community room, and says so before the
first message; the first send FOUNDS it (the room ceremony, never an MLS
bootstrap), and its header states the class; the invitation reaches the
member as a row atop their list; the founder's room settings list it as
pending and one press withdraws it — the member's row stops standing, with
nobody told, and inviting again is the undo; accepting it opens the room; the
founder's device keys the member in with no gesture of its own; a message
crosses both ways, sealed; the home nest's search index finds it, answered
to a member; and withdrawing the nest's read — one toggle and Save in the
founder's room settings — empties that index, which is the members'
standing choice working rather than an error; and the founder's tap on the
member's chip removes them through the room's own door — off the floor, the
room's generation rotated past them.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state (the room's ``awaiting_key`` and
``nest_read``, a snippet, an invitation row) or over the nest's own search
answer — never "sleep N then assert".
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    index_of,
    room_app,
    room_seat_pair,
    seat_element,
    seat_log,
    seat_story,
    thread_by_channel,
    wait_for,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS this one (`community-rooms.md` — the lead app paints every new
    # room gesture first). The six others join through the batched
    # trickle-down, each arriving with its own marker AND a
    # `helpers.room_seats.ROOM_APPS` entry.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # android: the home-nest toggle, pending-invite withdraw and nest-read
    # toggle are all built in `apps/fauna-android/`; its device run is the
    # venue row's, the mark is the coverage contract.
    pytest.mark.android,
    # web: the composer's home-nest toggle, the pending-invite withdraw, the
    # nest-read toggle and the room's search strip are all painted in
    # `apps/fauna-web/src/routes/conversations/+page.svelte`; founding and
    # accepting need the tab's account runtime up (`community-rooms.md`,
    # the founding entry of § Implementation status today), which the seat's
    # sign-in brings.
    pytest.mark.web,
]

SEARCH_BUDGET_S = MESSAGE_BUDGET_S
"""For the home nest to fold a sealed message into its index (at reception)
and, after a rotation withdrew its read, to delete what it built."""


def _search(port: int, reader: dict, room: str, needle: str) -> list | None:
    """``reader``'s room search for ``needle``, or ``None`` on a refusal — a
    refusal is worth polling through: the member's floor seat and the room's
    community standing are both the nest's to settle."""
    try:
        return conv_api.room_search(port, reader, room, needle)
    except Exception:
        return None


def _founded_room(seat, label_hint: str):
    """The founder's community room: the FaunaMls thread whose room says
    ``Community`` and which has bound to a room id."""
    for t in seat.app.conversations.list_threads():
        room = t.room or {}
        if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
            return t
    return None


@pytest.mark.feature("group-conversations")
def test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn(
    room_seat_pair, nest_instance
):
    """Found with the home-nest toggle, accept from the list, exchange, the
    nest's index finds the text, and withdrawing the nest's read empties it."""
    founder, member = room_seat_pair
    port = nest_instance["port"]
    # One unguessable word per message, so a hit can only be this run's.
    first_word = f"founding{secrets.token_hex(4)}"
    reply_word = f"answer{secrets.token_hex(4)}"
    later_word = f"welcome{secrets.token_hex(4)}"

    # ── founding: the composer's home-nest choice, one gesture each ──────
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(member.actor_hex)
    assert conv.prospective_room_class() == "end-to-end", (
        "with the home nest out, a Fauna chip makes an end-to-end room"
    )
    assert not conv.home_nest_included()
    conv.toggle_home_nest()
    assert conv.home_nest_included(), seat_element(founder, "recipient-picker-home-nest-toggle")
    assert conv.prospective_room_class() == "community", (
        "the class statement follows the choice, before the first message: "
        f"{seat_element(founder, 'recipient-picker-class')}"
    )
    founder.app.driver.type_text("dm-text-field", f"hello {first_word}")
    founder.app.driver.click("dm-send-button")

    room = wait_for(
        "the founder's community room binding to its room id",
        lambda: _founded_room(founder, first_word),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'found', 'room')}",
    )
    room_id = room.channel_id_hex
    assert not founder.app.has_error(), founder.app.error_text()
    conv.open_thread_by_id(room.thread_id)
    assert conv.room_class() == "community", (
        f"the header states the class: {seat_element(founder, 'thread-room-class')}"
    )

    # ── the invitation stands atop the member's list ─────────────────────
    wait_for(
        "the founder's invitation standing on the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )

    # ── the founder sees it pending, takes it back, and offers it again ──
    # The editor's pending section (`room-pending-invite[i]`) is served to
    # the founder because the founder may withdraw it; the withdrawal is
    # the ruled no-audience act — the invitee is told nothing, their
    # envelope simply stops standing (`conversation-rooms.md` § Join rules
    # and invites → *Pending invitations are visible to whoever may
    # withdraw them*). So the member-side observable is the row LEAVING
    # their list on the next sweep, never an accept raced against that
    # sweep; what a stale accept answers is pinned at the API tier
    # (`a_withdrawn_invitation_leaves_with_its_envelope_and_cannot_be_accepted`).
    conv.open_room_settings()
    wait_for(
        "the founder's editor listing the member's invitation as pending",
        lambda: conv.pending_invite_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'pending', 'invite')}",
    )
    pending_row = conv.pending_invite_text(0)
    assert member.handle.split("@")[0] in pending_row, (
        f"the row names the invitee as the home nest knows them: {pending_row!r}"
    )
    assert conv.pending_invite_lapsed(0) is False, (
        "an invitation its inviter may still issue has not lapsed: "
        f"{seat_element(founder, 'room-pending-invite')}"
    )
    conv.withdraw_pending_invite(0)
    wait_for(
        "the withdrawn row leaving the founder's editor",
        lambda: conv.pending_invite_count(),
        lambda n: n == 0,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={founder.app.error_text()!r}\n{seat_story(founder)}",
    )
    assert not founder.app.has_error(), founder.app.error_text()
    # Nothing is staged, so Save is the editor's plain close.
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    wait_for(
        "the withdrawn invitation leaving the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n == 0,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )
    # Inviting again is the undo: on a community room the add-participant
    # overlay issues an invitation and seats nobody.
    conv.add_participant_to_thread(member.actor_hex)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the renewed invitation standing on the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )

    # ── accepting opens the room ─────────────────────────────────────────
    member.app.conversations.accept_room_invitation(0)
    joined = wait_for(
        "the accepted room opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert not member.app.has_error(), (
        f"accepting must not refuse: {member.app.error_text()!r}\n{seat_story(member)}"
    )
    assert member.app.conversations.room_invitation_count() == 0, (
        "an accepted invitation stops standing"
    )

    # ── the key-in: the founder's device keys the member, no gesture ────
    wait_for(
        "the member's seat being keyed in by the founder's device",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'key-in', 'key_in', 'tend', 'mint')}"
        ),
    )

    # ── a message each way, sealed under the room's generation ───────────
    conv.send_in_open_thread(f"glad you came {later_word}")
    wait_for(
        "the founder's message opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and later_word in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'roomsealed', 'room')}",
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

    # ── the home nest's read: its index finds the text, for a member ─────
    hits = wait_for(
        "the home nest's search index finding the member's reply",
        lambda: _search(port, member.actor, room_id, reply_word),
        lambda h: bool(h),
        SEARCH_BUDGET_S,
        diagnose=lambda: f"search={_search(port, member.actor, room_id, reply_word)!r}",
    )
    assert all("text" not in h and "snippet" not in h for h in hits), (
        f"a hit is a position, never the message's text: {hits!r}"
    )

    # ── the same search, from the member's app (`room-search.md` § The surface)
    # A community room offers it; a message hit opens selected and in view; a
    # query nothing matches says so.
    mconv = member.app.conversations
    assert mconv.room_search_offered(), (
        f"a community room offers its search: {seat_element(member, 'room-search-button')}"
    )
    mconv.search_room(reply_word)
    found = wait_for(
        "the member's room search painting the reply as a message hit",
        lambda: mconv.room_search_results(),
        lambda rows: any(kind == "message" for kind, _ in rows),
        SEARCH_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n"
        f"{seat_element(member, 'room-search-result-item')}",
    )
    mconv.open_room_search_result(next(i for i, (k, _) in enumerate(found) if k == "message"))
    def selected_text():
        at = mconv.selected_message_index()
        texts = member.app.driver.get_texts("dm-message-text")
        return texts[at] if at is not None and at < len(texts) else None

    wait_for(
        "the opened hit selecting the reply in the stream",
        selected_text,
        lambda text: text is not None and reply_word in text,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n"
        f"{seat_element(member, 'dm-message-timestamp')}",
    )
    assert mconv.message_in_view(reply_word), (
        f"the selected hit is brought into view: {seat_element(member, 'dm-message-text')}"
    )
    mconv.search_room(f"nothing{secrets.token_hex(6)}")
    wait_for(
        "the member's room search saying nothing matched",
        lambda: mconv.room_search_found_nothing(),
        lambda shown: shown,
        SEARCH_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n"
        f"{seat_element(member, 'room-search-result-item')}",
    )
    assert not member.app.has_error(), member.app.error_text()
    mconv.close_room_search()

    # ── withdrawing the read: one toggle and Save, in the app ────────────
    founder_view = thread_by_channel(founder.app, room_id)
    assert (founder_view.room or {}).get("nest_read") is True, founder_view.room
    conv.open_thread_by_id(founder_view.thread_id)
    conv.open_room_settings()
    assert conv.room_nest_read_staged() is True, seat_element(founder, "room-nest-read-toggle")
    conv.toggle_room_nest_read()
    assert conv.room_nest_read_staged() is False
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()

    # The rotation wrapped past the nest; the revoke deletes every view it
    # built, so the same query now answers empty — the members' choice
    # working, never a refusal (RoomSearchReply's contract).
    wait_for(
        "the home nest's index emptying once its read is withdrawn",
        lambda: _search(port, member.actor, room_id, reply_word),
        lambda h: h == [],
        SEARCH_BUDGET_S,
        diagnose=lambda: (
            f"search={_search(port, member.actor, room_id, reply_word)!r}\n"
            f"{seat_story(founder)}\n{seat_log(founder, 'nest_read', 'rotate', 'mint')}"
        ),
    )
    assert _search(port, member.actor, room_id, first_word) == [], (
        "every view the nest built goes, not only the newest"
    )
    after = wait_for(
        "the founder's projection naming the read withdrawn",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("nest_read") is False,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    assert (after.room or {}).get("class") == "Community", (
        "withdrawing the read does not change what the room is — the class is its "
        f"seating, and the nest keeps its floor seat: {after.room!r}"
    )

    # ── removing the member: the chip is the room.remove door ────────────
    # A community seat is keyed by generation wraps, not an MLS add, so the
    # chip's removal is the unseat THEN the rotation (`community-rooms.md`
    # § Implementation status today → *A removal severs, because the client
    # rotates*) — never the end-to-end class's Remove commit, which finds no
    # leaf here. The rotation is read off the nest as the tip moving.
    tip_before = _tip_generation(port, founder.actor, room_id)
    assert tip_before is not None, "the founder holds the room's tip before the removal"
    removing = wait_for(
        "the member listed on the founder's thread",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and member.actor_hex in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    conv.open_thread_by_id(removing.thread_id)
    conv.remove_member(index_of(removing, member.actor_hex))
    wait_for(
        "the member leaving the room's floor",
        lambda: _floor_principals(port, founder.actor, room_id),
        lambda seats: seats is not None and member.actor_hex not in seats,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={founder.app.error_text()!r}\n{seat_story(founder)}\n"
        f"{seat_log(founder, 'remove', 'room')}",
    )
    wait_for(
        "the removal rotating the room's generation",
        lambda: _tip_generation(port, founder.actor, room_id),
        lambda tip: tip is not None and tip != tip_before,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error={founder.app.error_text()!r}\n"
        f"{seat_log(founder, 'remove', 'rotate', 'mint')}",
    )
    assert not founder.app.has_error(), (
        f"the removal and its rotation both landed: {founder.app.error_text()!r}"
    )


def _floor_principals(port: int, reader: dict, room: str) -> list[str] | None:
    """The live floor's principal hexes as ``reader`` reads them, or ``None``
    on a refusal (worth polling through)."""
    try:
        return [m["principal"] for m in conv_api.room_list_roster(port, reader, room)["members"]]
    except Exception:
        return None


def _tip_generation(port: int, reader: dict, room: str) -> str | None:
    """The id of the room's tip generation wrapped to ``reader``, or ``None``."""
    try:
        gens = conv_api.room_generations(port, reader, room)
    except Exception:
        return None
    return next((g["generation_id"] for g in gens if g.get("is_tip")), None)


SEARCH_KIND = "fauna.conversations.room.search"
FETCH_KIND = "fauna.conversations.channel.fetch"


@pytest.mark.feature("group-conversations")
def test_a_failed_room_search_shows_its_error_and_a_hit_not_yet_here_selects_once_it_lands(
    room_seat_pair, nest_instance
):
    """Two edges of the room's search (``room-search.md`` § Errors & edge cases).

    **A failed search shows the error, never the empty state.** The home nest
    turns the member's search away (the test hook's refusal, the same reply a
    refused or failing door gives the app): ``error-message`` carries it, and
    neither a row nor ``room-search-no-results`` paints.

    **A hit this device has not received yet says so, and is selected once it
    lands.** The nest parks every ``channel.fetch`` (the member's device drains
    the room only through it — a push is a wake), the founder writes, and the
    home nest indexes the message at reception; the member's search then finds
    a position its device does not hold, so the row reads *not on this device
    yet*. Opening it and releasing the fetch lands the message, and the
    selection the open set lights on it.

    Latency-independent (convention 14): every wait is a deadline poll on a
    painted row, ``error-message``, the nest's own refusal and hold counters or
    its own search answer."""
    from helpers.rpc_hold import (
        arm_rpc_hold,
        refuse_rpc,
        release_rpc_hold,
        wait_for_refused_rpc,
    )
    from helpers.room_seats import found_community_room, thread_by_id
    from i18n.strings import S

    founder, member = room_seat_pair
    port = nest_instance["port"]
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {secrets.token_hex(4)}"
    )
    mconv = member.app.conversations
    mconv.open_thread_by_id(member_thread)
    wait_for(
        "the member's device reading the room's floor and offering its search",
        mconv.room_search_offered,
        lambda offered: offered,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_element(member, 'room-search-button')}\n{seat_story(member)}",
    )
    assert not member.app.has_error(), member.app.error_text()

    # ── a refused search: the error, never the empty state ───────────────
    refuse_rpc(port, SEARCH_KIND)
    try:
        mconv.search_room(f"refused{secrets.token_hex(4)}")
        wait_for_refused_rpc(port, SEARCH_KIND)
        wait_for(
            "the refused search surfacing on error-message",
            member.app.has_error,
            lambda shown: shown,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"{seat_element(member, 'room-search-no-results')}\n"
            f"{seat_element(member, 'room-search-result-item')}",
        )
        assert member.app.driver.is_absent("room-search-no-results"), (
            "a failed search must never paint the empty state: "
            f"{seat_element(member, 'room-search-no-results')}"
        )
        assert mconv.room_search_results() == [], (
            f"a failed search paints no rows: {mconv.room_search_results()!r}"
        )
    finally:
        release_rpc_hold(port, SEARCH_KIND)
    mconv.close_room_search()

    # ── a hit whose message has not reached this device yet ──────────────
    word = f"pending{secrets.token_hex(4)}"
    arm_rpc_hold(port, FETCH_KIND)
    try:
        fconv = founder.app.conversations
        fconv.open_thread_by_id(founder_thread)
        fconv.send_in_open_thread(f"this lands later {word}")
        wait_for(
            "the home nest's search index finding the founder's new message",
            lambda: _search(port, member.actor, room_id, word),
            lambda h: bool(h),
            SEARCH_BUDGET_S,
            diagnose=lambda: f"search={_search(port, member.actor, room_id, word)!r}\n"
            f"{seat_story(founder)}",
        )
        held = thread_by_id(member.app, member_thread)
        assert word not in (held.snippet or ""), (
            "precondition: with every channel fetch parked the member's device "
            f"cannot hold the new message yet, but its thread reads {held.snippet!r}"
        )

        mconv.search_room(word)
        rows = wait_for(
            "the member's search painting the not-yet-received hit",
            mconv.room_search_results,
            lambda rs: any(
                kind == "message" and S.conversations.unified.room_search_hit_pending in text
                for kind, text in rs
            ),
            SEARCH_BUDGET_S,
            diagnose=lambda: f"error={member.app.error_text()!r}\n"
            f"{seat_element(member, 'room-search-result-item')}",
        )
        at = next(
            i for i, (kind, text) in enumerate(rows)
            if kind == "message" and S.conversations.unified.room_search_hit_pending in text
        )
        mconv.open_room_search_result(at)
    finally:
        release_rpc_hold(port, FETCH_KIND)

    def selected_text():
        index = mconv.selected_message_index()
        texts = member.app.driver.get_texts("dm-message-text")
        return texts[index] if index is not None and index < len(texts) else None

    wait_for(
        "the opened pending hit selecting the message once it lands",
        selected_text,
        lambda text: text is not None and word in text,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error={member.app.error_text()!r}\n"
        f"{seat_element(member, 'dm-message-timestamp')}\n{seat_story(member)}",
    )
    assert not member.app.has_error(), member.app.error_text()


PUBLISH_GENERATION_KIND = "fauna.conversations.room.publish_generation"


def _severance_owed(port: int, reader: dict, room: str) -> bool | None:
    """The floor's ``severance_owed`` answer as ``reader`` reads it — ``None``
    on a refusal (worth polling through) and on a reply without the field."""
    try:
        return conv_api.room_list_roster(port, reader, room).get("severance_owed")
    except Exception:
        return None


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("community-room-rekey-retry")
def test_a_removal_whose_rekey_fails_says_so_and_the_next_pass_completes_it(
    room_seat_pair, nest_instance
):
    """Outcome 37's retry arm (``docs/features/group-conversations.md``): a
    re-key that fails says so, and the next owner's or admin's app to look
    completes it without being asked (``community-rooms.md`` § Implementation
    status today → *A removal severs*, the retry paragraph).

    The home nest refuses the founder's mint (the test hook's refusal — the
    reply a failing door gives the app), so the chip's removal lands its
    unseat and not its rotation: the member's chip stays gone (they ARE off
    the floor) and ``error-message`` carries the residue's own sentence, never
    "Could not remove them". The floor answers ``severance_owed``. With the
    refusal lifted, the founder's own tend pass — no gesture — mints past the
    unseated entry: the tip moves and the floor answers ``severance_owed =
    false``, which the nest computes as "the tip no longer wraps to the
    removed entry", i.e. the removed member reads nothing new.

    Every mutation is a gesture (convention 8); every wait is a deadline poll
    on the nest's own floor and tip or on painted state (convention 14)."""
    from helpers.room_seats import found_community_room
    from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
    from i18n.strings import S

    founder, member = room_seat_pair
    port = nest_instance["port"]
    room_id, _founder_thread, _member_thread = found_community_room(
        founder, member, f"hello {secrets.token_hex(4)}"
    )
    conv = founder.app.conversations
    tip_before = _tip_generation(port, founder.actor, room_id)
    assert tip_before is not None, "the founder holds the room's tip before the removal"
    assert _severance_owed(port, founder.actor, room_id) is False, (
        "a keyed room with nobody unseated owes nothing"
    )
    removing = wait_for(
        "the member listed on the founder's thread",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and member.actor_hex in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )

    # ── the unseat lands, the re-key is refused ──────────────────────────
    marker = "⁣"
    residue_tail = S.conversations.unified.error_remove_participant_unsevered(
        name=marker
    ).split(marker, 1)[1]
    refuse_rpc(port, PUBLISH_GENERATION_KIND)
    try:
        conv.open_thread_by_id(removing.thread_id)
        conv.remove_member(index_of(removing, member.actor_hex))
        wait_for_refused_rpc(port, PUBLISH_GENERATION_KIND)
        wait_for(
            "the member leaving the room's floor",
            lambda: _floor_principals(port, founder.actor, room_id),
            lambda seats: seats is not None and member.actor_hex not in seats,
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: f"error={founder.app.error_text()!r}\n{seat_story(founder)}",
        )
        shown = wait_for(
            "the residue's own sentence on the founder's error-message",
            founder.app.error_text,
            lambda text: residue_tail in (text or ""),
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'remove', 'rotate')}",
        )
        assert "Could not remove" not in shown, (
            f"the removal happened — it must not read as a failed one: {shown!r}"
        )
        after = thread_by_channel(founder.app, room_id)
        assert after is not None and member.actor_hex not in after.participant_actor_ids, (
            "the chip stays gone — the member IS off the floor: "
            f"{after.participant_actor_ids if after else None!r}"
        )
        assert _tip_generation(port, founder.actor, room_id) == tip_before, (
            "precondition: the refused mint left the tip where it was"
        )
        assert _severance_owed(port, founder.actor, room_id) is True, (
            "the floor names the debt while the tip still wraps to the removed entry"
        )
    finally:
        release_rpc_hold(port, PUBLISH_GENERATION_KIND)

    # ── the founder's next tend pass completes it, with no gesture ───────
    wait_for(
        "the founder's tend pass re-keying the room past the removed member",
        lambda: _severance_owed(port, founder.actor, room_id),
        lambda owed: owed is False,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"tip={_tip_generation(port, founder.actor, room_id)!r}\n"
        f"{seat_story(founder)}\n{seat_log(founder, 'severance', 'tend', 'rotate', 'mint')}",
    )
    tip_after = _tip_generation(port, founder.actor, room_id)
    assert tip_after is not None and tip_after != tip_before, (
        f"the pass minted a fresh generation: {tip_before!r} → {tip_after!r}"
    )
    assert member.actor_hex not in (_floor_principals(port, founder.actor, room_id) or []), (
        "and the removal stood throughout — nothing re-seated the member"
    )

"""tier_3 e2e — **a member's reply to a room post is sealed to the room, and
an outsider's goes public only by their explicit answer**, the lead app's legs
of `ui/feed.md` § Encryption at rest → *A reply, quote or repost of a restricted
post — the ruling*, ruling 5 (its sealed arm, *Ruling 5's build — the shape*,
(a)–(d), and its confirmation arm, (e)).

Four seats on one nest, as `test_room_restricted_post.py`: the room's owner
addresses a post to the room; a **member** replies to it through the ordinary
`feed-reply-dialog`; a **second member** finds the reply in its local feed —
a card with no words on it — opens it, and reads the words; an **outsider**
finds the same card and cannot. Every mutation under test is driven through the
UI (convention 8); the room is bootstrapped through the real-wire commands
`room_seats` shares with every room journey (its carve-out (b)).

What the journey witnesses that the shared-Rust pins cannot: the app leg. The
reply dialog calls the manager's prepare verb, uploads the sealed body under
the room arm's sidecar on the app's own bulk plane, and submits — the
composer's flow, reused — and the nest stores, lists and serves a sealed post
that carries a reference and an empty public body.

The outsider's card is asserted POSITIVELY — listed, badged with the reserved
tier, the words absent — for the reason `test_room_restricted_post.py` gives:
"its device cannot open it" is a claim about a key it does not hold, pinned
where it is causal (the shared feed crate), and a negative assert on an async
unseal could only be a settle-sleep (convention 14).

The second journey is the confirmation arm: a reader off the room's floor
opens the same dialog, is told the reply would be public
(`feed-reply-audience`), is refused while `feed-reply-public-confirm` is
unchecked (ruling 6's refusal stays the unconfirmed default), and once the box
is checked the reply is sent public — a member reads the words on the list
card, in the clear. The checkbox starts unchecked at every open.

tui only: the other six apps' reply dialogs still call `reply` alone, which
refuses words under a restricted post — the safe direction — until their lift.
"""

import time
import uuid

import pytest

from helpers.room_seats import (  # noqa: F401 — `room_app`/`room_seats` are fixtures
    MESSAGE_BUDGET_S,
    bootstrap_room,
    launch_seat,
    room_app,
    room_seats,
    seat_app_path,
    seat_story,
    teardown_seats,
    thread_by_id,
    wait_for,
    wait_thread_by_channel,
)
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    pytest.mark.tui,
]

ROOM_POST_TIER = "room"


def _requery_until(seat, what: str, found):
    """Leave and re-enter the feed page — the app's own re-query, what a user
    would do — until ``found(seat)`` answers a non-negative index; return it.
    No app live-reloads the feed for another actor's post."""
    feed = seat.app.feed
    deadline = time.monotonic() + MESSAGE_BUDGET_S
    index = -1
    while index < 0 and time.monotonic() < deadline:
        seat.app.conversations.navigate()
        feed.navigate()
        try:
            # One re-query's worth of looking, on the shared deadline-poll.
            index = wait_for(what, lambda: found(seat), lambda i: i >= 0, 15.0)
        except AssertionError:
            index = -1
    assert index >= 0, f"{what}\n{seat_story(seat)}"
    return index


def _post_index(needle: str):
    return lambda seat: seat.app.feed.post_index_by_text(needle)


def _reply_index(seat) -> int:
    """The index of the listed post that is a reply — read off the app's own
    feed state (`is_reply`, the nest's projection of the plaintext reference),
    because the sealed reply's card carries no words to find it by."""
    for i, post in enumerate(seat.app.feed._feed_posts_from_state()):
        if post.get("is_reply"):
            return i
    return -1


def _card_badge(seat, index: int) -> str:
    """The `gated-post-badge` on the card at ``index``, or "" while it has not
    painted (a mid-render lookup race is expected while polling)."""
    try:
        return seat.app.driver.get_text("gated-post-badge", scope=f"post-card[{index}]")
    except Exception:  # noqa: BLE001 — mid-render races are expected while polling
        return ""


def _card_text(seat, index: int) -> str:
    return seat.app.driver.get_text("post-card", index=index)


@pytest.mark.feature("feed-interactions")
def test_a_members_reply_to_a_room_post_opens_for_the_room_and_nobody_else(
    room_seats, nest_instance, room_app, request
):
    owner, member, second = room_seats
    preview = f"A post for the room — {uuid.uuid4().hex[:6]}"
    full_body = f"Only the room reads this: {uuid.uuid4().hex[:8]}"
    words = f"said-in-the-room-{uuid.uuid4().hex[:8]}"

    group_id, channel_hex = bootstrap_room(owner, member, second)
    for seat, who in ((member, "member"), (second, "second member")):
        wait_thread_by_channel(seat, channel_hex, what=f"the room reaching the {who}")

    outsider = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        # ── The owner addresses a post to the room (the composer's own flow). ─
        option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)
        owner.app.feed.navigate()
        assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"compose-gate-tier-select should offer {option!r}; "
            f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
        )
        owner.app.feed.create_gated_post(full_body, preview, option)

        # ── A member replies, through the ordinary reply dialog. ─────────────
        at = _requery_until(
            member, "the room post should reach the member's feed", _post_index(preview)
        )
        member.app.feed.reply_post(words, index=at)
        # The send is a reference's send: the target's counter is read back
        # from the nest once the sealed reply is created. That it moved is the
        # causal proof the prepare → upload → submit leg completed.
        wait_for(
            "the target's reply count moving on the member's seat",
            lambda: next(
                (
                    p.get("reply_count", 0)
                    for p in member.app.feed._feed_posts_from_state()
                    if preview in p.get("body", "")
                ),
                0,
            ),
            lambda n: n >= 1,
            MESSAGE_BUDGET_S,
            diagnose=lambda: (
                f"error-message={member.app.error_text()!r}\n{seat_story(member)}"
            ),
        )
        assert not member.app.has_error(), (
            f"a seated member's reply is not refused: {member.app.error_text()!r}"
        )

        # ── A second member finds the reply, wordless, and opens it. ─────────
        at = _requery_until(
            second, "the sealed reply should reach the second member's feed", _reply_index
        )
        listed = second.app.feed._feed_posts_from_state()[at]
        assert words not in _card_text(second, at) and words not in listed.get("body", ""), (
            "none of the reply's words are on the list card — the public body is "
            f"empty: {listed!r}"
        )
        second.app.feed.open_post_detail(at)
        second.app.driver.wait_for("feed-post-detail-body")
        opened = wait_for(
            "the second member's detail unsealing the reply",
            second.app.feed.post_detail_body,
            lambda body: words in body,
            MESSAGE_BUDGET_S,
            diagnose=lambda: (
                f"error-message={second.app.error_text()!r}\n{seat_story(second)}"
            ),
        )
        assert words in opened

        # ── A reader off the room's floor sees the locked card. ──────────────
        at = _requery_until(
            outsider, "the sealed reply should be listed for the outsider too", _reply_index
        )
        assert words not in _card_text(outsider, at), (
            "the outsider's card carries none of the words"
        )
        listed = outsider.app.feed._feed_posts_from_state()[at]
        assert words not in listed.get("body", ""), (
            f"nor does the outsider's feed state: {listed!r}"
        )
        # Scoped to the reply's own card: the room post it answers is badged too.
        badge = wait_for(
            "the outsider's reply card painting its badge",
            lambda: _card_badge(outsider, at),
            bool,
            MESSAGE_BUDGET_S,
            diagnose=lambda: seat_story(outsider),
        )
        assert badge == ROOM_POST_TIER, (
            f"the outsider's card names the reserved tier it cannot join: {badge!r}"
        )
    finally:
        teardown_seats([outsider])


def _reply_count_of(seat, needle: str) -> int:
    return next(
        (
            p.get("reply_count", 0)
            for p in seat.app.feed._feed_posts_from_state()
            if needle in p.get("body", "")
        ),
        0,
    )


def _open_reply_dialog(seat, index: int) -> None:
    seat.app.driver.click("feed-reply-button", index=index)
    # Convention 14: the dialog's own field, never a sleep.
    seat.app.driver.wait_for("feed-reply-text-field", timeout=10.0)


@pytest.mark.feature("feed-interactions")
def test_an_outsiders_reply_to_a_room_post_is_refused_until_confirmed_public(
    room_seats, nest_instance, room_app, request
):
    owner, member, second = room_seats
    preview = f"A post for the room — {uuid.uuid4().hex[:6]}"
    full_body = f"Only the room reads this: {uuid.uuid4().hex[:8]}"
    words = f"said-out-loud-{uuid.uuid4().hex[:8]}"

    group_id, channel_hex = bootstrap_room(owner, member, second)
    wait_thread_by_channel(member, channel_hex, what="the room reaching the member")

    outsider = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        # ── The owner addresses a post to the room (the composer's own flow). ─
        option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)
        owner.app.feed.navigate()
        assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"compose-gate-tier-select should offer {option!r}; "
            f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
        )
        owner.app.feed.create_gated_post(full_body, preview, option)

        # ── The outsider opens the dialog: told the reply would be public. ────
        at = _requery_until(
            outsider, "the room post's teaser should reach the outsider's feed", _post_index(preview)
        )
        _open_reply_dialog(outsider, at)
        driver = outsider.app.driver
        assert driver.get_text("feed-reply-audience") == S.feed.post.reply_audience_public, (
            f"the dialog states the manager's answer\n{seat_story(outsider)}"
        )
        assert driver.get_attr("feed-reply-public-confirm", "checked") == "false", (
            "unchecked at every open"
        )

        # ── Unconfirmed: refused with the reason, and nothing is posted. ─────
        driver.fill("feed-reply-text-field", words)
        driver.click("feed-reply-submit-button")
        refused = wait_for(
            "the unconfirmed reply's refusal reaching error-message",
            outsider.app.error_text,
            bool,
            MESSAGE_BUDGET_S,
            diagnose=lambda: seat_story(outsider),
        )
        assert refused == S.feed.reference_restricted, (
            f"the refusal reads as the shared string: {refused!r}"
        )

        # ── Confirmed: the answer is given in the act, and the reply goes public.
        _open_reply_dialog(outsider, at)
        assert driver.get_attr("feed-reply-public-confirm", "checked") == "false", (
            "never remembered across opens"
        )
        driver.fill("feed-reply-text-field", words)
        driver.click("feed-reply-public-confirm")
        assert driver.get_attr("feed-reply-public-confirm", "checked") == "true"
        driver.click("feed-reply-submit-button")
        # The send is a reference's send: the target's counter is read back
        # from the nest once the public reply is created — the causal proof
        # the confirmed verb composed and the nest accepted it. The count is
        # exactly one: the refused attempt created nothing.
        wait_for(
            "the target's reply count moving on the outsider's seat",
            lambda: _reply_count_of(outsider, preview),
            lambda n: n >= 1,
            MESSAGE_BUDGET_S,
            diagnose=lambda: (
                f"error-message={outsider.app.error_text()!r}\n{seat_story(outsider)}"
            ),
        )
        assert _reply_count_of(outsider, preview) == 1, "the refused attempt created nothing"
        assert not outsider.app.has_error(), (
            f"the confirmed reply is not refused: {outsider.app.error_text()!r}"
        )

        # ── A member reads the words in the clear: the reply is public. ───────
        at = _requery_until(
            member, "the outsider's public reply should reach the member's feed", _post_index(words)
        )
        assert words in _card_text(member, at), "the words are on the list card, public"
        listed = member.app.feed._feed_posts_from_state()[at]
        assert listed.get("is_reply"), f"listed as a reply: {listed!r}"
    finally:
        teardown_seats([outsider])

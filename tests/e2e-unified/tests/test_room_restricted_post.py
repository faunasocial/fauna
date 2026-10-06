"""tier_3 e2e — **room-restricted posts**, the lead app's leg: a floor member
addresses a post to a room through the composer, another member opens it, and
a reader who is not in the room sees only the locked card. All four seats share
one nest, so the post reaches the others through the local feed, which lists
every local actor's posts — gated ones included — with no follow needed.

`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling* owns the
shape: a room post is an ordinary post of its author whose body only the
room's floor members open — `tier` is the reserved constant `room`, the wire
arm is `KeyAccess::Room` on the frozen tag `Mls`, and the seal is the Posts
row's own (`derive_post_key(base, seal_id)`, `base` the room's key). The shared
and nest halves were built first; what this journey witnesses is the app half:

1. **The seam** — the app installs its conversations session as the feed's
   room-post key seam, so a member's device opens a room post and offers the
   user's rooms as audiences (`RoomPostKeys`).
2. **The composer** — `compose-gate-tier-select` gains one option per room the
   author sits on the floor of, labelled `Room: <room>`; choosing it seals the
   body under the room's key and uploads it under the room arm's sidecar class.
3. **The open** — the unchanged detail-open unseal, now answered by the seam.

Every mutation under test is driven through the UI (convention 8); the room
itself is bootstrapped through the real-wire commands `room_seats` shares with
every room journey (its carve-out (b)).

The outsider's card is asserted POSITIVELY — teaser plus the reserved-tier
badge, the full body absent. That its device cannot open the post is a claim
about a key it does not hold, and the only thing that would observe the
refusal is an async page op that surfaces nothing; a negative assert on it
could only ever be a settle-sleep (convention 14). It is pinned instead where
it is causal, in the shared feed crate
(`a_room_post_this_device_holds_no_key_for_stays_locked`).
"""

import time
import uuid

import pytest

from helpers.app_surface import app_name, skip_unbuilt
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
    # `room_seats.py::launch_seat` starts the REAL conversations session on
    # macOS/iOS only under `FAUNA_E2E_REAL_CONVERSATIONS`, which
    # `_apply_real_conversations_env` sets session-wide whenever any selected
    # test carries this marker (tui/linux/web always run real regardless).
    # This test's own `bootstrap_room` call needs it exactly as every other
    # room journey's does — missing it left the apple apps on the
    # deterministic mock backend, where the owner's 1:1 resolved onto the
    # wrong rail (`Smtp` instead of `FaunaMls`) and the bootstrap's first
    # wait never observed its own echo.
    pytest.mark.real_conversations,
    # tui leads (`conversation-rooms.md` — "rendered on tui first"); the six
    # apps join through the batched trickle-down, each arriving with its own
    # marker AND a `helpers.room_seats.ROOM_APPS` entry.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.web,
    pytest.mark.android,
]

# The reserved tier a room post carries (`fauna_core::subscription::ROOM_POST_TIER`,
# ruling 3) — what `gated-post-badge` paints for a reader who is NOT in the
# room. A member's card names the room instead (`_wait_badge` below).
ROOM_POST_TIER = "room"


def _wait_badge(seat, index: int, expected: str, what: str) -> None:
    """Deadline-poll the index-th `gated-post-badge` on ``seat`` until it reads
    ``expected`` (convention 14). The badge's text is a per-read derivation on
    the shared manager (`PostSummary.room_label` against `own_rooms`), so a
    card can paint once with the reserved tier and again with the room once
    the conversations plane's tick has re-read the seat's rooms — the same
    value race `wait_for_gated_badge_text` documents, one step later."""

    def read():
        try:
            return seat.app.feed.gated_badge_text(index)
        except Exception:  # noqa: BLE001 — mid-render races are expected while polling
            return None

    wait_for(
        what,
        read,
        lambda text: text == expected,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error-message={seat.app.error_text()!r}\n{seat_story(seat)}",
    )


def _read_post(seat, needle: str):
    """Re-query this seat's local feed until the post carrying ``needle`` is
    listed, and return its index.

    No app live-reloads the feed for another actor's post, and entering the
    page is the app's own re-query (tui's `nav_enter_op` → `RefreshCurrentFeed`)
    — so leaving and returning is what a user would do. Repeated within the
    budget rather than once: the first re-query can land before the post the
    author has just created is visible to this reader."""
    feed = seat.app.feed
    deadline = time.monotonic() + MESSAGE_BUDGET_S
    found = False
    while not found and time.monotonic() < deadline:
        seat.app.conversations.navigate()
        feed.navigate()
        found = feed.wait_for_post_text(needle, timeout_s=15.0)
    assert found, (
        f"the room post should reach this seat's local feed\n{seat_story(seat)}"
    )
    index = feed._find_post_index_by_text(needle)
    assert index >= 0, f"post {needle!r} listed but not locatable"
    return index


@pytest.mark.feature("room-restricted-posts")
def test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked(
    room_seats, nest_instance, room_app, request
):
    owner, member, third = room_seats
    if app_name(owner.app.driver) not in (
        "tui", "linux", "web", "macos", "ios", "windows", "android",
    ):
        skip_unbuilt(
            owner.app.driver,
            surface="the room-restricted post compose option and room-post seam",
            detail="tui, linux, web, macos, ios, windows and android carry room "
            "posts; the room family's batched trickle-down is now complete",
            tracked="ui/feed.md § Implementation status today",
        )

    marker = f"room-body-{uuid.uuid4().hex[:8]}"
    preview = f"A post for the room — {uuid.uuid4().hex[:6]}"
    full_body = f"Only the room reads this: {marker}"

    group_id, channel_hex = bootstrap_room(owner, member, third)
    # The member must be keyed into the room before it can open a post sealed
    # under the room's current key. Its own thread's label is what ITS card
    # names the room by — the reader's conversation list, never the author's.
    member_thread = wait_thread_by_channel(
        member, channel_hex, what="the room reaching the member"
    )

    outsider = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        # ── The author: the room is offered as an audience. ──────────────────
        # The app re-reads its rooms on the conversations plane's own tick, so
        # the option appears without re-entering the feed.
        label = thread_by_id(owner.app, group_id).label
        option = S.feed.post.gate_room(room=label)
        owner.app.feed.navigate()
        assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"compose-gate-tier-select should offer {option!r} once the room is "
            f"bound; error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
        )

        # ── The author addresses the post to the room, through the composer. ─
        # The author sits on the room's floor, so their own card names the
        # room too (the card bullet of *the app half*) — by their own label.
        owner.app.feed.create_gated_post(full_body, preview, option)
        _wait_badge(owner, 0, option, "the author's card naming the room")
        assert marker not in owner.app.feed.first_post_text(), (
            "the full body must never reach the list card"
        )

        # ── A member of the room opens it. ───────────────────────────────────
        at = _read_post(member, preview)
        # The member's card names the room BEFORE the open, by the member's own
        # label, and its "open" is the card's own detail-open — no new control.
        _wait_badge(
            member,
            at,
            S.feed.post.gate_room(room=member_thread.label),
            "the member's card naming the room",
        )
        member.app.feed.open_post_detail(at)
        member.app.driver.wait_for("feed-post-detail-body")
        opened = wait_for(
            "the member's detail unsealing the room post",
            member.app.feed.post_detail_body,
            lambda body: marker in body,
            MESSAGE_BUDGET_S,
            diagnose=lambda: (
                f"error-message={member.app.error_text()!r}\n{seat_story(member)}"
            ),
        )
        assert marker in opened

        # ── A reader who is not in the room sees the locked card. ────────────
        at = _read_post(outsider, preview)
        assert outsider.app.feed.gated_badge_text(at) == ROOM_POST_TIER, (
            "the outsider's card names the reserved tier it cannot join"
        )
        assert marker not in outsider.app.feed.post_text(at), (
            "the outsider's card must show only the teaser"
        )
    finally:
        teardown_seats([outsider])

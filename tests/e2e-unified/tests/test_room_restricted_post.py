"""tier_3 e2e — **room-restricted posts**, the lead app's leg: a floor member
addresses a post to a room through the composer, another member opens it, and
a reader who is not in the room sees only the locked card. All four seats share
one nest, so the post reaches the others through the local feed, which lists
every local actor's posts — gated ones included — with no follow needed.

`behavior/restricted-posts.md` § Encryption at rest → *Room-restricted — the
ruling* owns the shape: a room post is an ordinary post of its author whose body only the
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
import urllib.request
import uuid
from pathlib import Path

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.labeler_publish import FIXTURES, publish_wasm_labeler
from helpers.room_seats import (  # noqa: F401 — `room_app`/`room_seats`/`room_seat_pair` are fixtures
    MEMBERSHIP_BUDGET_S,
    MESSAGE_BUDGET_S,
    bootstrap_room,
    found_community_room,
    index_of,
    launch_seat,
    room_app,
    room_seat_pair,
    room_seats,
    seat_app_path,
    seat_element,
    seat_log,
    seat_story,
    teardown_seats,
    thread_by_channel,
    thread_by_id,
    wait_for,
    wait_snippet,
    wait_thread_by_channel,
)
from helpers.waiting import poke_receive_cycle
from i18n.strings import S
from tests.api import conv_api

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


def _offered(seat, option: str) -> bool:
    """One read of whether the composer's audience select offers ``option``:
    an agent refuses a value its frame did not paint (convention 11), so the
    select itself is the read."""
    from drivers.http_bridge import SelectOptionNotOffered

    try:
        seat.app.driver.select("compose-gate-tier-select", option)
    except SelectOptionNotOffered:
        return False
    return True


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
        # ── Only a group room is offered, never the one-to-one it forked from.
        # The options are one read of the seat's rooms, so the read that
        # painted the room's option is the read that leaves the 1:1 out.
        one_to_one = [
            t
            for t in owner.app.conversations.list_threads()
            if t.rail == "FaunaMls" and t.flavor != "MlsGroup"
        ]
        assert one_to_one, f"the bootstrap's 1:1 is listed\n{seat_story(owner)}"
        for thread in one_to_one:
            assert not _offered(owner, S.feed.post.gate_room(room=thread.label)), (
                f"a one-to-one conversation ({thread.label!r}) is never offered "
                f"as an audience\n{seat_story(owner)}"
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


def _wait_card_badge(seat, post_id: str, expected: str, what: str) -> None:
    """Deadline-poll the `gated-post-badge` scoped to the card of ``post_id``
    until it reads ``expected``. Id-keyed, not index-keyed: a repost row and
    its original share the window, and both carry a badge, so neither a bare
    badge index nor a text-keyed card index names one card unambiguously."""

    def read():
        try:
            at = seat.app.feed.post_index_by_id(post_id)
            if at < 0:
                return None
            return seat.app.driver.get_text("gated-post-badge", scope=f"post-card[{at}]")
        except Exception:  # noqa: BLE001 — mid-render races are expected while polling
            return None

    wait_for(
        what,
        read,
        lambda text: text == expected,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error-message={seat.app.error_text()!r}\n{seat_story(seat)}",
    )


def _read_repost_row(seat, target_post_id: str) -> dict:
    """Re-query this seat's local feed until a repost row of ``target_post_id``
    is listed, and return its state row — `_read_post`'s twin, keyed on the
    repost carrier because the row's own body is empty (its card text is the
    folded embed, which the original's card shares)."""
    feed = seat.app.feed
    deadline = time.monotonic() + MESSAGE_BUDGET_S
    row = None
    while row is None and time.monotonic() < deadline:
        seat.app.conversations.navigate()
        feed.navigate()
        row = feed.wait_for_repost_row_by_target(target_post_id, timeout=15.0)
    assert row is not None, (
        f"the repost should reach this seat's local feed\n{seat_story(seat)}"
    )
    return row


@pytest.mark.feature("room-restricted-posts")
def test_a_repost_of_a_room_post_shows_an_outsider_the_teaser_and_the_lock(
    room_seats, nest_instance, room_app, request
):
    """`behavior/restricted-posts.md` § Encryption at rest, ruling 3: a repost
    is wordless, so it stays public — and renders "as the target's own gated
    card, teaser and badge". A member reposts the room post through
    `feed-repost-button`; the outsider's REPOST-ROW card then reads the
    teaser, none of the sealed words, and the reserved-tier badge, so the
    teaser never passes for a short public post. The member's own repost card
    names the room by the member's label, exactly as the original's card does.
    The badge rides the nest's projection of the TARGET's gate onto the repost
    row (`reposted_gated_*`), pinned in shared Rust
    (`a_repost_row_carries_its_targets_gate_and_names_the_room_for_a_member`)."""
    owner, member, third = room_seats
    if app_name(owner.app.driver) not in ("tui", "linux", "web", "android"):
        skip_unbuilt(
            owner.app.driver,
            surface="the repost card's target badge (`reposted_gated_*`)",
            detail="tui leads, linux, web and android follow; macos, ios and windows paint it in their trickle-down",
            tracked="behavior/restricted-posts.md § Implementation status today",
        )

    marker = f"room-body-{uuid.uuid4().hex[:8]}"
    preview = f"A room post to repost — {uuid.uuid4().hex[:6]}"
    full_body = f"Only the room reads this: {marker}"

    group_id, channel_hex = bootstrap_room(owner, member, third)
    member_thread = wait_thread_by_channel(
        member, channel_hex, what="the room reaching the member"
    )
    member_names_room = S.feed.post.gate_room(room=member_thread.label)

    outsider = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        label = thread_by_id(owner.app, group_id).label
        option = S.feed.post.gate_room(room=label)
        owner.app.feed.navigate()
        assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"compose-gate-tier-select should offer {option!r} once the room is "
            f"bound; error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
        )
        owner.app.feed.create_gated_post(full_body, preview, option)

        # ── A member reposts the room post, through the repost button. ───────
        at = _read_post(member, preview)
        _wait_badge(member, at, member_names_room, "the member's card naming the room")
        target = member.app.feed.post_state_by_text(preview)
        assert target is not None, f"the room post's state row\n{seat_story(member)}"
        target_id = target["post_id"]
        member.app.feed.repost_post(at)
        mine = _read_repost_row(member, target_id)
        _wait_card_badge(
            member,
            mine["post_id"],
            member_names_room,
            "the member's repost card naming the room by the member's label",
        )

        # ── The outsider's repost-row card: teaser + the lock, no words. ─────
        row = _read_repost_row(outsider, target_id)
        _wait_card_badge(
            outsider,
            row["post_id"],
            ROOM_POST_TIER,
            "the outsider's repost card carrying the reserved-tier badge",
        )
        card = outsider.app.feed.post_index_by_id(row["post_id"])
        # The repost row's own body is empty; its words are the folded embed,
        # which some apps paint as `feed-post-text` and web as the `quoted-post`
        # embed, resolved lazily after the card first paints.
        deadline = time.monotonic() + MESSAGE_BUDGET_S
        while True:
            text = (
                outsider.app.driver.get_text("feed-post-text", scope=f"post-card[{card}]")
                or ""
            ) + (outsider.app.feed.quoted_post_text(card) or "")
            if preview in text or time.monotonic() >= deadline:
                break
            time.sleep(0.5)
        assert preview in text, (
            f"the outsider's repost card shows the target's teaser, got {text!r}"
        )
        assert marker not in text, "the repost card must never show the sealed words"
        assert outsider.app.feed.repost_attribution_count(card) == 1, (
            "the badged card is the repost row itself, not the original's"
        )
    finally:
        teardown_seats([outsider])


TEST_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def _room_photo_journey(room_seats, nest_instance, room_app, request, *, attach_first):
    """The room-post photo journey both photo witnesses drive; ``attach_first``
    picks the composer order (`FeedActions.create_gated_post`)."""
    owner, member, third = room_seats
    if app_name(owner.app.driver) != "tui":
        skip_unbuilt(
            owner.app.driver,
            surface="the room post's photo journey",
            detail="the seal is shared Rust; tui leads the witness, the other "
            "six follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")

    marker = f"room-photo-{uuid.uuid4().hex[:8]}"
    full_body = f"A picture only the room sees: {marker}"
    preview = f"A room post with a photo — {uuid.uuid4().hex[:6]}"

    group_id, channel_hex = bootstrap_room(owner, member, third)
    member_thread = wait_thread_by_channel(
        member, channel_hex, what="the room reaching the member"
    )

    outsider = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)
        owner.app.feed.navigate()
        assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
        )
        owner.app.feed.create_gated_post(
            full_body, preview, option, image_path=str(TEST_IMAGE), attach_first=attach_first
        )

        # ── A member opens the post: words and picture together (outcome 4). ─
        at = _read_post(member, preview)
        _wait_badge(
            member, at, S.feed.post.gate_room(room=member_thread.label),
            "the member's card naming the room",
        )
        member.app.feed.open_post_detail(at)
        member.app.driver.wait_for("feed-post-detail-body")
        wait_for(
            "the member's detail unsealing the room post",
            member.app.feed.post_detail_body,
            lambda body: marker in body,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"error-message={member.app.error_text()!r}\n{seat_story(member)}",
        )
        assert member.app.feed.post_detail_image_painted(), (
            "the member's opened room post must PAINT its picture, not the "
            "placeholder a still-sealed item leaves; "
            f"paint={member.app.feed.post_image_paint_report()} "
            f"error-message={member.app.error_text()!r}"
        )

        # ── What the nest holds is ciphertext (outcome 22). ──────────────────
        # Read off the member's opened body — the item is named only inside it.
        blob_hash = member.app.feed.post_image_blob_hash_by_text(full_body, timeout=MESSAGE_BUDGET_S)
        assert len(blob_hash) == 64, (
            f"the opened room post carries its photo as a MediaItem, got {blob_hash!r}"
        )
        # Fetched with NO credentials, as any stranger holding the hash would.
        with urllib.request.urlopen(
            f"{nest_instance['url']}/api/v1/blob/{blob_hash}", timeout=RPC_ROUNDTRIP_S
        ) as resp:
            stored = resp.read()
        assert stored[: len(PNG_MAGIC)] != PNG_MAGIC, (
            "the room post's photo rests on the nest as readable PNG — it was "
            f"uploaded before it was sealed; first bytes={stored[:16]!r}"
        )
        assert len(stored) >= 28, f"a sealed blob is at least nonce + tag; got {len(stored)}B"

        # ── Outside the room: the teaser, locked, naming no picture. ─────────
        at = _read_post(outsider, preview)
        assert outsider.app.feed.gated_badge_text(at) == ROOM_POST_TIER
        listed = outsider.app.feed.post_state_by_text(preview)
        assert listed is not None and marker not in listed.get("body", ""), listed
        assert not listed.get("has_media") and not listed.get("media_hash"), (
            f"nothing outside the sealed body names the photo: {listed!r}"
        )
    finally:
        teardown_seats([outsider])


@pytest.mark.feature("room-restricted-posts")
def test_a_room_posts_photo_rests_sealed_and_opens_for_a_member(
    room_seats, nest_instance, room_app, request
):
    """A photo on a room post is sealed before it is uploaded, under the same
    per-post key as the words (`ui/media.md` § Encryption at rest): the bytes
    the nest holds are ciphertext to anyone fetching them by hash; a member
    opens the post and the picture PAINTS with its words; a reader outside
    the room is listed the teaser with no picture named at all — the item is
    named only inside the sealed body. The room is chosen first, then the file."""
    _room_photo_journey(room_seats, nest_instance, room_app, request, attach_first=False)


@pytest.mark.feature("room-restricted-posts")
def test_a_photo_attached_before_the_room_is_chosen_is_still_sealed_for_the_room(
    room_seats, nest_instance, room_app, request
):
    """Outcome 23: the photo is picked BEFORE the room is chosen as the
    audience, and the post still seals it for the room — the audience in force
    at submit decides the seal, never the one selected when the file was
    picked (`ui/media.md` § Encryption at rest). The same three facts as the
    room-first journey: ciphertext at rest, the member's picture paints, the
    outsider's row names no media. A composer that seals at pick time makes
    the submit refuse (`feed.compose_attachment_stale`); one that uploads at
    pick time leaves readable PNG on the nest — either reds here."""
    _room_photo_journey(room_seats, nest_instance, room_app, request, attach_first=True)


@pytest.mark.feature("room-restricted-posts")
def test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room(
    room_seats, nest_instance
):
    """A room post written while a member sat on an end-to-end room's floor;
    the owner removes them through the member chip; then, on the removed
    seat: the room is no longer offered as an audience, the earlier post's
    card stops naming the room and shows the reserved tier's lock — and the
    post still opens, because what a member held while seated stays theirs
    (`restricted-posts.md` § Encryption at rest, *the rooms offered* and the
    end-to-end class's history rule)."""
    owner, stayer, leaver = room_seats
    if app_name(owner.app.driver) != "tui":
        skip_unbuilt(
            owner.app.driver,
            surface="the removed member's room-post journey",
            detail="the ex-member's composer and kept-base open are shared Rust; "
            "tui leads the witness, the other six follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )

    marker = f"room-body-{uuid.uuid4().hex[:8]}"
    preview = f"Before the removal — {uuid.uuid4().hex[:6]}"

    group_id, channel_hex = bootstrap_room(owner, stayer, leaver)
    leaver_thread = wait_thread_by_channel(
        leaver, channel_hex, what="the room reaching the leaver"
    )
    leaver_option = S.feed.post.gate_room(room=leaver_thread.label)

    # ── While seated: the room is offered, and the post names it. ───────────
    leaver.app.feed.navigate()
    assert leaver.app.feed.wait_for_audience_option(leaver_option, MESSAGE_BUDGET_S), (
        f"a seated member is offered the room\n{seat_story(leaver)}"
    )
    option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)
    owner.app.feed.navigate()
    assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
        f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
    )
    owner.app.feed.create_gated_post(f"Only the room reads this: {marker}", preview, option)

    at = _read_post(leaver, preview)
    _wait_badge(leaver, at, leaver_option, "the seated member's card naming the room")

    # ── The owner removes the leaver through the member chip. ───────────────
    owner.app.conversations.open_thread_by_id(group_id)
    seated = wait_for(
        "the owner's chips rendering the roster",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and owner.app.conversations.member_chip_count() >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    owner.app.conversations.remove_member(index_of(seated, leaver.actor_hex))
    wait_for(
        "the owner's roster dropping the leaver",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and leaver.actor_hex not in t.participant_actor_ids,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}",
    )

    # ── The removed seat: the room is no longer offered (outcome 10). ───────
    # The flip is the state under test: each poll asks the seat's receive
    # loop to look for the removal commit, then reads the option list again.
    leaver.app.feed.navigate()

    def offered_after_a_look() -> bool:
        poke_receive_cycle(leaver.app.driver)
        leaver.app.feed._open_composer()
        return _offered(leaver, leaver_option)

    wait_for(
        "the removed seat's composer no longer offering the room",
        offered_after_a_look,
        lambda offered: not offered,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(leaver),
    )

    # ── Its earlier post stops naming the room and shows locked (outcome 11).
    at = _read_post(leaver, preview)
    _wait_badge(leaver, at, ROOM_POST_TIER, "the removed seat's card showing the lock")
    assert marker not in leaver.app.feed.post_text(at), "the card shows only the teaser"

    # ── …and still opens: written while they were a member (outcome 12). ─────
    leaver.app.feed.open_post_detail(at)
    leaver.app.driver.wait_for("feed-post-detail-body")
    opened = wait_for(
        "the removed seat's detail unsealing the earlier room post",
        leaver.app.feed.post_detail_body,
        lambda body: marker in body,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error-message={leaver.app.error_text()!r}\n{seat_story(leaver)}",
    )
    assert marker in opened


# tui's post detail has no dismiss element and a page nav keeps the feed's
# sub-page (`PageOp`/`feed::Mode`); the way back is the Esc key, which the
# agent presses as `FeedActions.close_post_detail`. Each outcome-6 half below
# is its own journey, and the open it asserts on is that seat's final gesture.
_DETAIL_OPEN_IS_SETTLED = (
    "the agent awaits a click's page op before it replies unless the op "
    "outlives the click (`PageOp::outlives_click` names no feed op), so when "
    "the open returns, the detail-open's unseal attempt has already run"
)


def _open_and_read_detail(seat, needle_preview: str) -> str:
    """List the post carrying ``needle_preview`` on ``seat``, open it, and
    return the detail body — a settled read (``_DETAIL_OPEN_IS_SETTLED``), and
    one that shows the post's own teaser, so a detail that never opened cannot
    pass for one that stayed locked."""
    at = _read_post(seat, needle_preview)
    seat.app.feed.open_post_detail(at)
    seat.app.driver.wait_for("feed-post-detail-body")
    body = seat.app.feed.post_detail_body()
    assert needle_preview in body, (
        f"the opened detail shows the post's teaser, got {body!r}\n{seat_story(seat)}"
    )
    return body


@pytest.mark.feature("room-restricted-posts")
def test_a_member_removed_from_the_room_opens_no_room_post_written_after(
    room_seats, nest_instance
):
    """Outcome 6, the removal half (`restricted-posts.md` § Encryption at
    rest, ruling 6): once the owner has removed a member from an end-to-end
    room, a room post written after the removal never opens on the removed
    seat — the room's key has moved on without them. The removed seat's
    composer dropping the room (outcome 10) is the causal anchor that its
    device has processed the removal before the post is written."""
    owner, _stayer, leaver = room_seats
    if app_name(owner.app.driver) != "tui":
        skip_unbuilt(
            owner.app.driver,
            surface="the removed member's room-post journey",
            detail="the seal and open are shared Rust; tui leads the witness, "
            "the other six follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )

    marker = f"room-body-{uuid.uuid4().hex[:8]}"
    preview = f"After the removal — {uuid.uuid4().hex[:6]}"

    group_id, channel_hex = bootstrap_room(owner, _stayer, leaver)
    leaver_thread = wait_thread_by_channel(
        leaver, channel_hex, what="the room reaching the leaver"
    )
    leaver_option = S.feed.post.gate_room(room=leaver_thread.label)
    option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)

    owner.app.conversations.open_thread_by_id(group_id)
    seated = wait_for(
        "the owner's chips rendering the roster",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None
        and len(t.participant_actor_ids) == 2
        and owner.app.conversations.member_chip_count() >= 2,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    owner.app.conversations.remove_member(index_of(seated, leaver.actor_hex))

    leaver.app.feed.navigate()

    def offered_after_a_look() -> bool:
        poke_receive_cycle(leaver.app.driver)
        leaver.app.feed._open_composer()
        return _offered(leaver, leaver_option)

    wait_for(
        "the removed seat's composer no longer offering the room",
        offered_after_a_look,
        lambda offered: not offered,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(leaver),
    )

    owner.app.feed.navigate()
    assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
        f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
    )
    owner.app.feed.create_gated_post(f"Only the room reads this: {marker}", preview, option)

    body = _open_and_read_detail(leaver, preview)
    assert marker not in body, (
        "a member removed from the room must not open a room post written after "
        f"the removal\n{seat_story(leaver)}"
    )


@pytest.mark.feature("room-restricted-posts")
def test_a_newcomer_to_a_room_that_shares_no_history_opens_no_earlier_room_post(
    room_seats, nest_instance, room_app, request
):
    """Outcome 6, the join half (`restricted-posts.md` § Encryption at rest,
    ruling 6): a room post written before someone joins an end-to-end room
    under its `none` history rule — the rule a room is born with — never opens
    for them, though they now hold the room's key.

    The owner adds the newcomer through the ordinary picker. A room message
    the owner sends after the join decrypting on the newcomer's seat is the
    causal anchor: the newcomer holds the room's current epoch, whose exporter
    is a room post's base. The pre-join post's open is then a settled read
    (`_DETAIL_OPEN_IS_SETTLED`) showing the post's teaser and not its body."""
    owner, member, third = room_seats
    if app_name(owner.app.driver) != "tui":
        skip_unbuilt(
            owner.app.driver,
            surface="the room newcomer's room-post journey",
            detail="the seal and open are shared Rust; tui leads the witness, "
            "the other six follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )

    before = f"room-body-{uuid.uuid4().hex[:8]}"
    before_preview = f"Before the join — {uuid.uuid4().hex[:6]}"
    after = f"after the join {uuid.uuid4().hex[:8]}"

    group_id, channel_hex = bootstrap_room(owner, member, third)
    option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)
    newcomer = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        # Setup (convention 8 carve-out (b)), as the room-history journey does:
        # the newcomer admits the owner as a contact, so the owner's Welcome
        # is accepted under the default knock inbox mode.
        conv_api.accept_contact(nest_instance["port"], newcomer.actor, owner.actor_hex)
        owner.app.feed.navigate()
        assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
        )
        owner.app.feed.create_gated_post(
            f"Only the room reads this: {before}", before_preview, option
        )

        # ── The owner adds the newcomer, through the picker. ─────────────────
        owner.app.conversations.open_thread_by_id(group_id)
        owner.app.conversations.add_participant_to_thread(newcomer.handle)
        wait_for(
            "the owner's roster carrying the newcomer",
            lambda: thread_by_id(owner.app, group_id),
            lambda t: t is not None and newcomer.actor_hex in t.participant_actor_ids,
            MEMBERSHIP_BUDGET_S,
            diagnose=lambda: f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}",
        )
        wait_thread_by_channel(newcomer, channel_hex, what="the newcomer's Welcome applying")

        # ── The newcomer holds the room's current key: a message the owner
        # sends after the join decrypts on its seat. ─────────────────────────
        owner.app.conversations.real_send(group_id, after)
        wait_snippet(newcomer, channel_hex, after, what="the post-join message reaching the newcomer")

        # ── The one written before the join does not — this seat's last read. ─
        body = _open_and_read_detail(newcomer, before_preview)
        assert before not in body, (
            "a newcomer under the `none` history rule must not open a room post "
            f"written before they joined\n{seat_story(newcomer)}"
        )
    finally:
        teardown_seats([newcomer])


@pytest.mark.feature("room-restricted-posts")
def test_a_rooms_labels_show_on_a_room_posts_card_for_its_members_only(
    room_seat_pair, nest_instance, room_app, request, run_seal_helper
):
    """Outcome 7 (`restricted-posts.md` § Encryption at rest, ruling 7 and
    its *Built* detail (v)): the labelers a community room's owner names run
    over a room post when the room's home nest indexes it, and the verdicts
    reach a MEMBER's card. The shared feed manager asks the floor-gated
    `fauna.posts.room_labels` read when the member opens the post and merges
    the answer into the card's own labels, so the card's existing
    `content-label-badge` paints them with no app change. A reader outside the
    room opens the same post and its card stays unlabelled.

    The fixture labeler says `commercial` about any text containing "cat".
    Only the sealed words carry it, never the public teaser, so a badge on
    the card can only come from the room's verdict on the sealed body. The
    outsider's no-badge read follows a settled open
    (`_DETAIL_OPEN_IS_SETTLED`): the open that would have fetched a member's
    verdicts has already run when the click returns."""
    founder, member = room_seat_pair
    if app_name(founder.app.driver) != "tui":
        skip_unbuilt(
            founder.app.driver,
            surface="the room post's label journey (Esc out of the post detail)",
            detail="the label merge is shared Rust; tui leads the witness, the "
            "other six follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )

    labeler = publish_wasm_labeler(
        run_seal_helper, nest_instance, FIXTURES / "room_commercial_labeler.wat"
    ).hex()
    marker = f"whiskers{uuid.uuid4().hex[:8]}"
    preview = f"A post for the room — {uuid.uuid4().hex[:6]}"
    full_body = f"Only the room reads this: my cat says {marker}"

    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello {uuid.uuid4().hex[:8]}"
    )

    # ── The owner names the labeler in the room's settings. ─────────────────
    conv = founder.app.conversations
    conv.open_thread_by_id(founder_thread)
    conv.open_room_settings()
    row = conv.room_labeler_index(labeler)
    assert row is not None, seat_element(founder, "room-labeler-toggle")
    conv.toggle_room_labeler(row)
    conv.save_room_settings(timeout_s=MEMBERSHIP_BUDGET_S)
    assert not founder.app.has_error(), founder.app.error_text()
    wait_for(
        "the room's verified labeler set naming the fixture",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and (t.room or {}).get("labelers") == [labeler],
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'labeler')}",
    )

    outsider = launch_seat(nest_instance, seat_app_path(request, room_app), room_app, request)
    try:
        # ── The owner addresses a post to the room, through the composer. ───
        option = S.feed.post.gate_room(room=thread_by_id(founder.app, founder_thread).label)
        founder.app.feed.navigate()
        assert founder.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
            f"error-message={founder.app.error_text()!r}\n{seat_story(founder)}"
        )
        founder.app.feed.create_gated_post(full_body, preview, option)

        # ── A member opens it, steps back, and its card carries the label. ──
        at = _read_post(member, preview)
        _wait_badge(
            member,
            at,
            S.feed.post.gate_room(room=thread_by_id(member.app, member_thread).label),
            "the member's card naming the room",
        )
        post_id = member.app.feed.post_state_by_text(preview)["post_id"]
        member.app.feed.open_post_detail(at)
        member.app.driver.wait_for("feed-post-detail-body")
        wait_for(
            "the member's detail unsealing the room post",
            member.app.feed.post_detail_body,
            lambda body: marker in body,
            MESSAGE_BUDGET_S,
            diagnose=lambda: f"error-message={member.app.error_text()!r}\n{seat_story(member)}",
        )
        member.app.feed.close_post_detail()

        def member_card_badge():
            card = member.app.feed.post_index_by_id(post_id)
            if card < 0:
                return None
            scope = f"post-card[{card}]"
            if member.app.driver.count("content-label-badge", scope=scope) == 0:
                return None
            return member.app.driver.get_text("content-label-badge", scope=scope)

        badge = wait_for(
            "the room's verdict painting a label on the member's card",
            member_card_badge,
            lambda text: text is not None,
            MESSAGE_BUDGET_S,
            diagnose=lambda: (
                f"error-message={member.app.error_text()!r}\n{seat_story(member)}\n"
                f"{seat_log(member, 'room_labels', 'label')}"
            ),
        )
        assert S.moderation.category.commercial in badge, (
            f"the card names the room labeler's verdict "
            f"({S.moderation.category.commercial!r}); got {badge!r}"
        )

        # ── A reader outside the room: the lock, and no label. ──────────────
        at = _read_post(outsider, preview)
        assert outsider.app.feed.gated_badge_text(at) == ROOM_POST_TIER
        outsider_post = outsider.app.feed.post_state_by_text(preview)["post_id"]
        body = _open_and_read_detail(outsider, preview)
        assert marker not in body, "the outsider's detail stays the teaser"
        outsider.app.feed.close_post_detail()
        card = outsider.app.feed.post_index_by_id(outsider_post)
        assert card >= 0, f"the post is still listed\n{seat_story(outsider)}"
        assert outsider.app.driver.count("content-label-badge", scope=f"post-card[{card}]") == 0, (
            "the room's verdicts are served to its members only: "
            f"{outsider.app.driver.get_text('content-label-badge', scope=f'post-card[{card}]')!r}"
        )
    finally:
        teardown_seats([outsider])


@pytest.mark.feature("room-restricted-posts")
def test_a_room_post_is_never_a_message_in_the_room_and_reaches_a_member_like_any_post(
    room_seats, nest_instance, room_app, request
):
    """Outcome 8 (`restricted-posts.md` § Encryption at rest; the feed's read
    model, `ui/feed.md` § The read model): a room post never goes through the
    room — nobody in the room is sent it as a message there — and a member who
    does not follow the author comes across it only the way they come across
    the author's other posts.

    No seat of `room_seats` follows another. The negative is anchored on a
    message the owner sends AFTER the post (convention 14): once it has
    decrypted on the member's seat, the room's newest message is that one,
    and the thread's count has grown by exactly it — nothing for the post sits
    between. The positive half is the member's feed listing the room post the
    way it lists a public post of the owner's, and opening it."""
    owner, member, third = room_seats
    if app_name(owner.app.driver) != "tui":
        skip_unbuilt(
            owner.app.driver,
            surface="the room post never-a-message journey",
            detail="the seal and the feed are shared Rust; tui leads the witness, "
            "the other six follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )

    nonce = uuid.uuid4().hex[:8]
    marker = f"room-body-{nonce}"
    preview = f"A post for the room, not the chat — {nonce}"
    public = f"An ordinary public post — {nonce}"
    after = f"after the post {nonce}"

    group_id, channel_hex = bootstrap_room(owner, member, third)
    wait_snippet(member, channel_hex, "hi room", what="the bootstrap message reaching the member")
    before_count = thread_by_channel(member.app, channel_hex).message_count
    option = S.feed.post.gate_room(room=thread_by_id(owner.app, group_id).label)

    # ── The owner writes a public post and a room post, through the composer. ─
    owner.app.feed.navigate()
    owner.app.feed.create_post(public)
    assert owner.app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
        f"error-message={owner.app.error_text()!r}\n{seat_story(owner)}"
    )
    owner.app.feed.create_gated_post(f"Only the room reads this: {marker}", preview, option)

    # ── The anchor: a room message sent after the post reaches the member. ──
    owner.app.conversations.real_send(group_id, after)
    thread = wait_snippet(
        member, channel_hex, after, what="the after-post message reaching the member"
    )
    assert thread.message_count == before_count + 1, (
        "the room's thread grew by more than the one message sent after the post "
        f"({before_count} → {thread.message_count}) — a room post must never be "
        f"sent as a message in the room\n{seat_story(member)}"
    )
    member.app.conversations.navigate()
    member.app.conversations.open_thread_by_channel(channel_hex)

    def bubbles():
        return [
            member.app.driver.get_text("dm-message-text", index=i)
            for i in range(member.app.driver.count("dm-message-text"))
        ]

    texts = wait_for(
        "the open room painting the after-post message",
        bubbles,
        lambda ts: any(after in t for t in ts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert not any(marker in t or preview in t for t in texts), (
        f"nothing of the room post appears among the room's messages: {texts!r}"
    )

    # ── The member comes across it like the owner's public post: in the feed. ─
    _read_post(member, public)
    at = _read_post(member, preview)
    member.app.feed.open_post_detail(at)
    member.app.driver.wait_for("feed-post-detail-body")
    wait_for(
        "the member's detail unsealing the room post from their feed",
        member.app.feed.post_detail_body,
        lambda body: marker in body,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error-message={member.app.error_text()!r}\n{seat_story(member)}",
    )

"""Tier_3 — following an author whose account lives on ANOTHER nest.

``docs/goal/behavior/federated-follow.md`` is the owner. A reader on one nest
follows an author on another and sees that author's posts in their own feed,
from their own nest (§ The ruling this doc builds on). Three of its decisions
meet in this journey:

* § Decision 2: the follower's own client mints the signed ``FollowIntent``
  and names the author's home; the follower's nest carries it to the home;
  the author's client grants the follow; the state leg pushes the approval
  back.
* § Decision 1: the home pushes the author's posts to the follower's nest
  through the outbox, both the backfill at approval and each post at create.
* § Decision 3 and § Decision 5: the follower's nest keeps the envelope only,
  marked, and lists it to the follower alone; the author's delete removes it
  there too.

**Topology:** two real nest binaries, each with a handle domain and floor TLS,
the shape ``test_conversation_room_community_cross_nest.py`` drives. The
author is a handled actor on ``home`` (``cross_nest_foreign``). The follower
is ``away``'s (``cross_nest_foreign_ephemeral``) pre-provisioned handled
actor. A second account on ``away`` follows nobody.

**What the journey proves, in order:**

1. The follower finds the author by ``handle@authority`` and opens their
   Profile through the Find User result's
   ``contact-actor-id-open-profile-button``. That door carries the home
   (``profile.md`` § Layout & flow → *Reaching another actor's profile*).
2. ``profile-follow-button`` follows at that home.
3. The followers tier auto-approves. The author's tui runs the author pump
   (``drain_auto_approvals``) with no gesture, so ``home`` pushes
   ``follow.state`` approved, and the follower's own nest reads the follow as
   held.
4. The author posts a public post and a followers-gated post through the
   composer. Both reach the follower's Feed on ``away``.
5. The follower opens the followers-only post, and its detail shows the
   sealed words. They rest on ``home`` and are fetched from there by content
   address, the key through ``away``'s relay (§ Decision 3, *What is not
   stored*).
6. The author posts a photo, and the follower's card paints it from ``home``.
7. The non-follower on ``away`` lists neither, in their feed or in the
   author read.
8. The author deletes the public post, and it leaves the follower's feed.

Steps 5 and 6 cross the foreign byte plane to a ``home`` serving the
self-signed floor. They are reached only because the delivered post carries
the identity ``away`` verified for its home, and the follower's plane
graduates a pin against it (``transport-trust.md`` § *The post arm*).

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state, never "sleep N then assert".
"""

from __future__ import annotations

import secrets
from pathlib import Path

import pytest

from conftest import provision_foreign_handled_actor
from helpers.app_surface import skip_unbuilt
from helpers.federated_follow import (
    CROSSING_BUDGET_S,
    FOLLOWERS_TIER,
    call,
    feed_row,
    follow_across_nests,
)
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MESSAGE_BUDGET_S,
    Seat,
    launch_seat,
    room_app,
    seat_app_path,
    seat_log,
    seat_story,
    teardown_seats,
    wait_for,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui LEADS (`testing.md` § Default app and nest mode); the others
    # join through the batched trickle-down, each with its own marker.
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.linux,
]

BUILT_APPS = frozenset({"tui", "android", "linux"})
"""The apps that build the federated follow; each joins with its marker above."""

TEST_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


@pytest.fixture()
def follow_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(author, follower)`` on two nests. The author is a fresh handled
    actor on ``cross_nest_foreign`` (their home), so ``handle@authority``
    finds them from the other nest. The follower is the handled actor that
    ``cross_nest_foreign_ephemeral`` provisioned."""
    if room_app not in BUILT_APPS:
        skip_unbuilt(
            room_app,
            surface="profile-follow-button at a remote actor's home",
            detail="tui leads the federated follow; the others follow in their trickle-down",
            tracked="behavior/federated-follow.md § Implementation status today",
        )
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        author = launch_seat(
            home, app_path, room_app, request, actor=provision_foreign_handled_actor(home)
        )
        seats.append(author)
        follower = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(follower)
        yield author, follower
    finally:
        teardown_seats(seats)


def _listed_bodies(nest: dict, reader: dict, author_hex: str) -> list[str]:
    """Every body ``reader``'s own nest lists to them from ``author_hex``, from
    their timeline read and the author read alike."""
    timeline = call(nest, reader, "fauna.feed.local.posts", {}).get("posts", [])
    by_author = call(nest, reader, "fauna.feed.author.posts", {"author": author_hex}).get(
        "posts", []
    )
    return [str(p) for p in timeline + by_author]


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_reader_follows_an_author_on_another_nest_and_their_posts_reach_the_readers_feed(
    follow_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """Find the author across nests, open their profile, follow: the follow
    is granted with no author gesture, the author's public and followers-only
    posts list in the follower's feed on their own nest, the followers-only
    post opens to its sealed words and a photo paints, both from the author's
    home, a non-follower there lists neither, and a deleted post leaves."""
    author, follower = follow_seats
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    public_word = f"openly{secrets.token_hex(4)}"
    gated_word = f"sealed{secrets.token_hex(4)}"
    preview = f"For my followers {secrets.token_hex(3)}"
    photo_word = f"pictured{secrets.token_hex(4)}"

    from common.auth import register_handled_actor

    stranger = register_handled_actor(
        away["port"],
        handle=f"carol{secrets.token_hex(4)}",
        domain=away["authority"],
        base_url=away["url"],
    )

    # ── 1–3. The door, the follow at the home, and the grant crossing back ─
    follow_across_nests(follower, author, home, away)

    # ── 4. The author posts, public and followers-only, through the composer
    feed = author.app.feed
    feed.navigate()
    feed.create_post(f"hello from home {public_word}")
    feed.create_gated_post(f"only followers read {gated_word}", preview, FOLLOWERS_TIER)
    assert not author.app.has_error(), author.app.error_text()

    for words, what in ((public_word, "the public post"), (preview, "the followers-only post")):
        row = wait_for(
            f"{what} reaching the follower's feed on their own nest",
            lambda w=words: feed_row(follower, w),
            lambda r: r is not None,
            CROSSING_BUDGET_S,
            diagnose=lambda: (
                f"error-message={follower.app.error_text()!r}\n{seat_story(follower)}\n"
                f"{seat_log(author, 'outbox', 'deliver', 'follow')}"
            ),
        )
        assert (row.get("author") or author.actor_hex) == author.actor_hex, row
    assert gated_word not in str(follower.app.feed.post_state_by_text(preview)), (
        "the delivered followers-only envelope lists its teaser; its sealed words "
        "stay on the author's home"
    )

    # ── 5. The follower opens it: the sealed words come from the home ─────
    follower_feed = follower.app.feed
    follower_feed.open_post_detail(follower_feed.post_index_by_text(preview))
    follower.app.driver.wait_for("feed-post-detail-body", timeout=MESSAGE_BUDGET_S)
    wait_for(
        "the delivered post's detail unsealing its words from the author's home",
        follower_feed.post_detail_body,
        lambda text: gated_word in text,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"error-message={follower.app.error_text()!r}\n{seat_story(follower)}\n"
            f"{seat_log(follower, 'graduate', 'pin', 'blob', 'key_blob', 'unlock', 'tls')}"
        ),
    )
    assert not follower.app.has_error(), (
        f"a foreign home's byte leg never surfaces a page error: {follower.app.error_text()!r}"
    )
    follower_feed.close_post_detail()

    # ── 6. A photo post: the follower's card paints it from the home ──────
    feed.navigate()
    feed.create_post_with_image(f"a picture {photo_word}", str(TEST_IMAGE))
    assert not author.app.has_error(), author.app.error_text()
    wait_for(
        "the photo post reaching the follower's feed on their own nest",
        lambda: feed_row(follower, photo_word),
        lambda r: r is not None,
        CROSSING_BUDGET_S,
        diagnose=lambda: f"{seat_story(follower)}\n{seat_log(author, 'outbox', 'deliver')}",
    )
    assert follower_feed.post_image_painted_by_text(photo_word), (
        f"the delivered photo paints from the author's home: "
        f"error-message={follower.app.error_text()!r}\n{seat_story(follower)}\n"
        f"{seat_log(follower, 'graduate', 'pin', 'blob', 'media', 'tls')}"
    )

    # ── 7. A neighbour on the same nest who follows nobody lists neither ──
    neighbour_sees = _listed_bodies(away, stranger, author.actor_hex)
    assert not any(public_word in b or preview in b for b in neighbour_sees), (
        f"a delivered post lists to its follower alone: {neighbour_sees!r}"
    )

    # ── 8. The author deletes the public post; it leaves the follower's feed
    feed.navigate()
    feed.delete_post(feed.post_index_by_text(public_word))
    wait_for(
        "the deleted post leaving the follower's feed",
        lambda: feed_row(follower, public_word),
        lambda r: r is None,
        CROSSING_BUDGET_S,
        diagnose=lambda: f"{seat_story(follower)}\n{seat_log(author, 'delete', 'outbox')}",
    )
    # Step 5 unsealed the followers-only post, so its card now shows the
    # sealed words rather than the teaser.
    assert feed_row(follower, gated_word) is not None, (
        "deleting one post leaves the author's other delivered post standing"
    )

"""Tapping a notification takes you to what it is about — cross-app.

The witness for ``docs/features/notifications.md`` outcome 4, and the journey
``docs/goal/behavior/notifications.md`` § Deep-link destinations ratifies: one
shared-Rust router (``fauna_client_notifications::notification_destination``)
returns the destination, app glue navigates, and a row with no destination
renders inert.

**The journey.** The driven seat composes a post *through its own composer*;
a second actor on the same nest likes it, which fires the author's real
``like`` notification through the production ``insert_notification``
(``bins/fauna-nest/src/interact_routes.rs``); the seat then opens its
Notifications page and **activates the row through the app UI**, and the post
detail that opens carries the body it wrote.

**Why the liker is headless (e2e rule 8b).** Convention 8 governs *the journey's
mutations* — here the compose and the tap, both of which go through the app UI.
The like is the precondition being arranged: a second actor must exist and must
have liked something before there is any notification to tap. It rides
``fauna.posts.interact``, the same wire kind a second app seat's like button
would issue, so nothing about how the notification is produced, stored or
rendered is stubbed — only how the *other* person is driven. That is the
standing shape for this page's tests
(``test_notifications_type_icon.py``'s docstring makes the same call) and it is
what keeps this witness inside the default ``[tui]`` app set on every machine
rather than behind a two-seat special.

**Convention 14.** Every assertion is on element state — the detail dialog is
visible, its body carries the composed text — never on elapsed time. The post
the tap opens is one the seat's *own* timeline already holds, so this does not
depend on the deep-link fetch either way.

tier_3: the full stack from locally-built binaries — a real app driver, a real
nest, and a real second actor over the production wire.
"""

from __future__ import annotations

import uuid

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S

pytestmark = [pytest.mark.tier_3]


# **macos and ios joined 2026-09-21** — one shared FaunaKit seam
# (`Core/NotificationOpen.swift`) calls the very router tui/linux/web call, over
# `FfiNotifItem`'s `source` / `contentId` / `senderId`, which the apple FFI
# mapping had been dropping; each shell then routes the typed target into the
# destination page's own gesture, the Post arm through `FeedVM.pendingPostOpen`
# — the same cross-page deep-link slot a `search-result-item` activation uses.
# A row the router cannot place stays a plain label, never a disabled control:
# `AutomationRegistry` answers 409 for a disabled one, which would reject the
# gesture where this page's contract is to ignore it.
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("notifications")
def test_tapping_a_like_notification_opens_the_liked_post(
    logged_in_app, api_actor_peer
):
    """A ``like`` notification opens the post it is about."""
    app = logged_in_app
    body = f"tap-through probe {uuid.uuid4().hex[:8]}"

    # 1. The seat writes the post, through its own composer.
    app.feed.navigate()
    app.feed.create_post(body)

    # 2. Someone else likes it — the precondition, on the production wire.
    api_actor_peer.like_post(_post_id_of(api_actor_peer, body))

    # 3. The seat opens its notifications and finds the row.
    app.notifications.navigate()
    app.driver.wait_for("notification-item", timeout=RPC_ROUNDTRIP_S)
    assert app.notifications.notification_count() >= 1, (
        "the like should have produced a notification row: "
        f"{app.driver.diagnose('notification-item')}"
    )

    # 4. The tap — the gesture under test.
    app.notifications.open_notification(index=0)

    # 5. It landed on the post, and the post is the right one. Element state,
    #    never a wall-clock wait (convention 14).
    app.driver.wait_for("feed-post-detail-dialog", timeout=RPC_ROUNDTRIP_S)
    assert app.feed.post_detail_visible(), (
        "activating a like notification must open the liked post's detail: "
        f"{app.driver.diagnose('feed-post-detail-dialog')}"
    )
    detail = app.feed.post_detail_body()
    assert body in detail, (
        "the detail that opened is not the post the notification was about; "
        f"expected {body!r} in {detail!r}"
    )


def _post_id_of(actor, body: str) -> str:
    """The id of the post whose body is ``body``, read back over the wire.

    The liker needs the *target's* id and has no UI to read it from. The nest's
    ``General`` feed query returns posts by every actor on the nest, not only
    the feed owner's (``test_feed_post_delete.py`` pins that same fact from the
    other direction), so the seat's post is visible from here.

    Matched on the body, never "the newest row": the session nest is shared and
    accumulating, so index 0 is whatever the last test to post left behind. The
    body carries a uuid suffix, which makes the match exact.
    """
    feeds = actor._ws_call("fauna.feed.list", {})
    feed_id = next(
        (f["feed_id"] for f in feeds.get("feeds", []) if f.get("name") == "General"),
        None,
    )
    assert feed_id, f"the peer actor should have its General feed: {feeds}"
    posts = actor.get_feed_posts(feed_id)
    match = next((p for p in posts if body in p.get("body", "")), None)
    assert match, (
        f"the seat's post {body!r} should have reached the nest before the "
        f"like; feed {feed_id} holds {[p.get('body') for p in posts][:5]}"
    )
    return match["post_id"]

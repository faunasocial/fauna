"""Opening a Bluesky post's thread shows the conversation around it — the post
it answers, the post itself marked, and a reply from someone the member does
not follow — and a post gone at its origin says so.

`docs/goal/behavior/bridges.md` § Bluesky-native thread view: a post detail
whose source is ``bluesky`` offers *View thread* (``feed-post-view-thread-button``);
the thread page (``feed-thread-page``) lists ancestors oldest-first, the opened
post, then its direct replies, one ``feed-thread-post`` each, the opened post
``selected`` — the post the nest names by ``BlueskyThreadReply.focal_index``,
never an app's guess; ``bluesky.not_found`` / ``bluesky.blocked`` /
``bluesky.upstream`` surface on the page's ``error-message``. The shared page
state is ``fauna_feed::FeedManager::open_thread`` (pinned headlessly in
``fauna-feed``'s manager tests); this is the journey through a running nest
and the lead app. Catalog outcome ``atproto`` 43.

The far end, the link through the real OAuth callback and the poll-now seam
are `test_bluesky_feed_ingest.py`'s, imported rather than copied; read its
module docstring for why each is a precondition and not a shortcut (e2e rule
8b). The far end's ``getPostThread`` answers from the posts the test published
there — the conversation is arranged upstream, exactly as a real one would be;
the subject — opening the detail, then the thread — is the app's.

tier_3: a locally-built nest and the real app; the atproto network is a fake.
tui first, the lead app, then linux (re-pointed onto the shared page state),
android (compile-verified until android has a run venue), web and windows
(their trickle-down lifts); macos and ios gain their markers in theirs.
"""

from __future__ import annotations

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    FOLLOWED_DID,
    FOLLOWED_HANDLE,
    _link_through_callback,
    _poll_now,
    atproto_far_end,
    bluesky_feed_nest,
    bluesky_feed_spa_url,
)

pytestmark = [pytest.mark.tier_3]

#: Someone the member does not follow: their posts reach Fauna only through a
#: thread, never the timeline.
STRANGER_DID = "did:plc:carolfaketeststranger234"
STRANGER_HANDLE = "carol.test"


def _until(probe, budget_s: float):
    """``probe()``'s first truthy answer within ``budget_s``, else its last."""
    deadline = time.monotonic() + budget_s
    seen = probe()
    while not seen and time.monotonic() < deadline:
        time.sleep(0.3)  # sleep-ok: pacing between poll iterations
        seen = probe()
    return seen


def _ingested_post_id(nest, text: str) -> str:
    user = nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        local = client.call("fauna.feed.local.posts", {"limit": 50})
    wire = [
        p for p in local.get("posts", [])
        if p.get("source") == "bluesky" and p.get("body") == text
    ]
    assert len(wire) == 1, (
        f"the ingested post {text!r} should be on the wire once; got {local.get('posts')!r}"
    )
    return wire[0]["post_id"]


def _open_detail(app, post_id: str) -> None:
    index = _until(lambda: app.feed.post_index_by_id(post_id) + 1, 30) - 1
    assert index >= 0, (
        f"the ingested Bluesky post {post_id} is on the wire but not in the "
        f"app's feed; error={app.error_text()!r}"
    )
    app.feed.open_post_detail(index)
    assert _until(app.feed.view_thread_offered, 15), (
        "a Bluesky post's detail offers feed-post-view-thread-button: "
        f"{app.driver.diagnose('feed-post-view-thread-button')}; "
        f"error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.feature("atproto")
def test_a_bluesky_posts_thread_shows_the_conversation_with_the_opened_post_marked(
    request, app, bluesky_feed_nest, atproto_far_end,
):
    from conftest import _login_app_as

    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    # The conversation, upstream: a stranger's post, the followed account's
    # answer to it (the only one on the timeline), and a stranger's reply.
    root_text = "where does everyone keep their notes"
    opened_text = "in a text file, honestly"
    reply_text = "same here, plain text forever"
    root_uri = far.publish_post(
        author_did=STRANGER_DID, author_handle=STRANGER_HANDLE, rkey="3kthroot",
        text=root_text, on_timeline=False,
    )
    opened_uri = far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3kthopen",
        text=opened_text, reply_to=root_uri,
    )
    far.publish_post(
        author_did=STRANGER_DID, author_handle=STRANGER_HANDLE, rkey="3kthreply",
        text=reply_text, reply_to=opened_uri, on_timeline=False,
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, (
        f"the poll should store the followed post: {report!r}; far end saw {far.requests!r}"
    )
    post_id = _ingested_post_id(nest, opened_text)

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture="bluesky_feed_spa_url", verify_live_actor=True,
    )
    app.feed.navigate()
    _open_detail(app, post_id)
    app.feed.open_thread()

    expected = [
        (STRANGER_HANDLE, root_text),
        (FOLLOWED_HANDLE, opened_text),
        (STRANGER_HANDLE, reply_text),
    ]
    posts = _until(lambda: (lambda t: t if len(t) == 3 else [])(app.feed.thread_posts()), 30)
    thread_calls = [r for r in far.requests if r[2].endswith("app.bsky.feed.getPostThread")]
    assert len(posts) == 3, (
        f"the thread page lists the root, the opened post and its reply; "
        f"got {app.feed.thread_posts()!r}; page visible="
        f"{app.feed.thread_page_visible()}; getPostThread calls={thread_calls!r}; "
        f"error={app.error_text()!r}"
    )
    for shown, (handle, text) in zip(posts, expected):
        assert handle in shown and shown.endswith(text), (
            f"thread order is ancestors, the opened post, then replies: "
            f"expected {expected!r}, got {posts!r}"
        )
    assert app.feed.thread_selected_flags() == ["false", "true", "false"], (
        f"exactly the opened post is marked: {app.feed.thread_selected_flags()!r}"
    )
    assert not app.error_text(), f"a loaded thread raises no error: {app.error_text()!r}"


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.feature("atproto")
def test_a_thread_whose_post_is_gone_at_its_origin_says_so(
    request, app, bluesky_feed_nest, atproto_far_end,
):
    from conftest import _login_app_as

    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    text = "a post deleted on bluesky after it arrived"
    uri = far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3kthgone", text=text,
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, report
    post_id = _ingested_post_id(nest, text)
    far.remove_post(uri)

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture="bluesky_feed_spa_url", verify_live_actor=True,
    )
    app.feed.navigate()
    _open_detail(app, post_id)
    app.feed.open_thread()

    # The nest's typed `fauna.bluesky.not_found`, rendered by the shared
    # `RpcError::localized` arm — never the generic "something went wrong".
    error = _until(app.error_text, 30)
    assert error and "no longer on bluesky" in error.lower(), (
        f"a post gone at its origin says so on the page's error-message; "
        f"got {error!r}; thread posts={app.feed.thread_posts()!r}"
    )
    assert app.feed.thread_posts() == [], "a failed thread lists no posts"

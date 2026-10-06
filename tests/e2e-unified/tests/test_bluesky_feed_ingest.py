"""A post from someone a linked Bluesky account follows arrives in the feed,
and a reply to it from Fauna threads under it on Bluesky.

`docs/goal/behavior/bridges.md` § Unified feed ingestion → *Bridge ingestion*
(the consume-side poller stores each timeline post with source ``bluesky`` and
its ``bluesky_posts`` map row) and § Interactions / § Cross-posting → *A post
that references a Bluesky record* (a reply to a Bluesky-origin post is acked
by the reply door, composed natively, and written through to the linked PDS
with ``reply.parent`` naming the record it answers). The headless halves are
pinned in Rust (``feed_ingest.rs``, ``conformance_bluesky_feed_ingest.rs``,
``write_through_reference_tests``); this is the journey through a running
nest and the lead app.

**The far end.** The nest's consume-side OAuth client is routed to
:class:`helpers.atproto_fakes.FakeAtprotoFarEnd` by the ``test-hooks``-only
``FAUNA_TEST_ATPROTO_FAR_END`` seam — the same real OAuth client, callback,
poller and write-through, with only the network canned.

**What the harness arranges, and why that is not a shortcut (e2e rule 8b).**
Linking is a precondition, not the subject: it is started over the user's own
``fauna.bridges.link`` and finished by playing the authorization server's
redirect to the nest's real callback route — the browser leg no app owns. The
poll is poked through ``POST /api/v1/test/bluesky/feed/poll-now`` because the
worker's own cadence is five minutes (convention 14). The subject — the post in
the feed with its badge, and the reply — is the app's.

**The post's picture** (`docs/goal/architecture/render-model.md` § D6c,
bridges.md ruling 4): the followed post carries an image embed, which the
ingest stores as a nest-relative ``/api/v1/bluesky/media?url=…`` path and the
shared feed fold turns into ``RenderBlock::ProxiedImage`` — so the card paints
a ``post-image`` addressed by that path, never by the zero blob hash every app
used to request. The CDN itself is unreachable from the harness (the proxy's
SSRF guard forbids loopback, rightly), so the witness is the slot and its
address; the bytes-to-art half is pinned headlessly
(``a_bridged_proxied_image_paints_in_post_image_through_the_nest``).

tier_3: a locally-built nest and the real app — tui, which built the
``ProxiedImage`` paint first, and macos/ios, linux and web, whose `post-image`
placeholder answers the same path; the atproto network is a fake.
"""

from __future__ import annotations

import time

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [pytest.mark.tier_3]

#: The domain the nest is claimed onto — Bluesky OAuth needs a public identity
#: domain to derive its client (`bluesky::oauth_public_url`).
FEED_DOMAIN = "bsky-feed.test"

FOLLOWED_DID = "did:plc:bobfaketestfollowed2345"
FOLLOWED_HANDLE = "bob.test"

#: The followed post's picture on the bsky CDN (the host the `bluesky/media`
#: route allowlists), and the nest-relative path the ingest stores for it.
PICTURE_URL = f"https://cdn.bsky.app/img/feed_fullsize/plain/{FOLLOWED_DID}/bafkfakepicture@jpeg"
PICTURE_PATH = "/api/v1/bluesky/media?url=" + requests.utils.quote(PICTURE_URL, safe="")


# Both fixtures are per-test, never module-scoped: the test links the user's
# Bluesky account and publishes a fixed post, so a second app's run against a
# shared nest (`--app linux,web`, a mac `sweep`) was refused
# `fauna.bridges.already_linked` before it reached the app.
@pytest.fixture
def atproto_far_end():
    from helpers.atproto_fakes import FakeAtprotoFarEnd

    far = FakeAtprotoFarEnd()
    yield far
    far.close()


@pytest.fixture
def bluesky_feed_nest(bluesky_nest_binary, tmp_path_factory, atproto_far_end):
    """A `bluesky` nest claimed onto a public domain whose consume-side OAuth
    client talks to :func:`atproto_far_end`, with one registered user."""
    from conftest import _make_nest, _make_user

    nest, cleanup = _make_nest(
        bluesky_nest_binary, tmp_path_factory, "bluesky-feed-ingest",
        claim_domain=FEED_DOMAIN,
        extra_env={"FAUNA_TEST_ATPROTO_FAR_END": atproto_far_end.url},
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def bluesky_feed_spa_url(spa_proxy_for, bluesky_feed_nest):
    """The SPA proxy a web seat dials this nest through (a browser cannot reach a
    raw nest — no CORS); native apps take the nest URL and never resolve it."""
    return spa_proxy_for(bluesky_feed_nest["url"])


def _link_through_callback(nest, far) -> None:
    """Start the link as the user, then land the authorization server's
    redirect on the nest's callback with the `state` it was handed at PAR."""
    user = nest["user"]
    before = len(far.par_states)
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        reply = client.call("fauna.bridges.link", {
            "bridge_id": "bluesky", "mode": "oauth",
            "params": {"handle": far.handle},
        })
    assert reply.get("redirect_url"), f"the oauth link starts a redirect: {reply!r}"
    assert len(far.par_states) > before, (
        f"authorize() never reached the fake's PAR endpoint; saw {far.requests!r}"
    )
    resp = requests.get(
        f"{nest['url']}/api/v1/bluesky/auth/callback",
        params={"code": "fake-code", "state": far.par_states[-1], "iss": far.issuer},
        allow_redirects=False, verify=False, timeout=30,
    )
    location = resp.headers.get("location", "")
    assert "result=linked" in location, (
        f"the callback must complete the link; status {resp.status_code}, "
        f"Location {location!r}; far end saw {far.requests!r}"
    )


def _poll_now(nest) -> dict:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/bluesky/feed/poll-now", timeout=60, verify=False,
    )
    assert resp.status_code == 200, f"poll-now: {resp.status_code} {resp.text}"
    return resp.json()


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("atproto")
def test_a_followed_bluesky_post_arrives_in_the_feed_and_a_reply_threads_under_it(
    request, app, bluesky_feed_nest, atproto_far_end,
):
    from conftest import _login_app_as

    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    text = "a post from someone you follow on bluesky"
    post_uri = far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3kfeed1", text=text,
        image_urls=(PICTURE_URL,),
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, (
        f"the poll should store the followed post: {report!r}; "
        f"far end saw {far.requests!r}"
    )

    # Wire leg first (convention 6): if the nest's own feed read lacks the
    # post, the defect is nest-side and no element-hunting will say so.
    user = nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        local = client.call("fauna.feed.local.posts", {"limit": 50})
    # Found by its TEXT: the list-card body is `content_meta.preview`, written
    # for every post by the one index funnel (`feed.md` § The read model →
    # *The list-card preview*), so a bridged post's card carries its text.
    wire = [
        p for p in local.get("posts", [])
        if p.get("source") == "bluesky" and p.get("body") == text
    ]
    assert len(wire) == 1, (
        f"fauna.feed.local.posts should carry exactly the one ingested post, "
        f"its text {text!r} as the card body; got {local.get('posts')!r}"
    )
    post_id = wire[0]["post_id"]

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture="bluesky_feed_spa_url", verify_live_actor=True,
    )
    app.feed.navigate()
    deadline = time.monotonic() + 30
    index = -1
    while time.monotonic() < deadline:
        index = app.feed.post_index_by_id(post_id)
        if index >= 0:
            break
        time.sleep(0.3)
    assert index >= 0, (
        f"the ingested Bluesky post {post_id} is on the wire but not in the "
        f"app's feed; app feed ids="
        f"{[p.get('post_id') for p in app.feed._feed_posts_from_state()]!r}; "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count("protocol-badge", scope=f"post-card[{index}]") == 1, (
        "a Bluesky-sourced post carries exactly one protocol-badge: "
        f"{app.driver.diagnose('protocol-badge')}"
    )
    # Stripped: web's `feed-post-text` is the rendered markdown body, whose
    # paragraph ends in a newline — layout, not content.
    card_text = app.feed.post_text(index).strip()
    assert card_text == text, (
        f"the bridged post's card shows its text {text!r}; got {card_text!r}; "
        f"error={app.error_text()!r}"
    )

    # The picture: one `post-image` in the card, addressed by the proxied path
    # (its placeholder text until bytes arrive — and they cannot, from here),
    # never `/api/v1/blob/000…`.
    scope = f"post-card[{index}]"
    deadline = time.monotonic() + 30
    seen = 0
    while time.monotonic() < deadline:
        seen = app.driver.count("post-image", scope=scope)
        if seen:
            break
        time.sleep(0.3)
    count = app.driver.count("post-image", scope=scope)
    assert count == 1, (
        "a bridged post with a picture paints exactly one post-image: "
        f"{seen} in the card when the wait ended, {count} on the re-read; "
        f"{app.driver.diagnose('post-image')}; the post's state row "
        f"{[p for p in app.feed._feed_posts_from_state() if p.get('post_id') == post_id]!r}; "
        f"error={app.error_text()!r}"
    )
    picture = app.driver.get_text("post-image", scope=scope)
    assert picture == PICTURE_PATH, (
        f"the post-image is addressed by the nest-relative proxied path "
        f"{PICTURE_PATH!r}; got {picture!r}"
    )

    reply_text = "replying from fauna"
    app.feed.reply_post(reply_text, index=index)

    deadline = time.monotonic() + 30
    replies = []
    while time.monotonic() < deadline:
        replies = [
            r for r in far.created_snapshot()
            if (r.get("record") or {}).get("reply", {}).get("parent", {}).get("uri") == post_uri
        ]
        if replies:
            break
        time.sleep(0.3)
    assert replies, (
        "the reply should be written through to the linked PDS with "
        f"reply.parent = {post_uri}; createRecord bodies seen: "
        f"{far.created_snapshot()!r}; app error={app.error_text()!r}"
    )
    record = replies[0]["record"]
    assert record.get("text") == reply_text, record
    assert record["reply"]["root"]["uri"] == post_uri, (
        "a reply to a thread-starting post names it as the root too: "
        f"{record['reply']!r}"
    )

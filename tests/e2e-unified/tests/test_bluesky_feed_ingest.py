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

**Two more journeys, tui first** (the lead app; the six-app lift of the
label is its own row):

* a post served only by a **custom feed generator** the user subscribed to in
  the app arrives — and did not before the subscription (§ Unified feed
  ingestion: one ``getFeed`` page per ``bridge_feed_subscriptions`` row);
* a **bridged author** paints by their Bluesky display name, never their DID
  or short id, and a private nickname the user gives them comes first
  (§ Unified feed ingestion → *Bridged authors*; `contacts.md` § The private
  overlay).

tier_3: a locally-built nest and the real app — tui, which built the
``ProxiedImage`` paint first, and macos/ios, linux, web and windows, whose
`post-image` placeholder answers the same path; the atproto network is a fake.
"""

from __future__ import annotations

import time

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

#: The domain the nest is claimed onto — Bluesky OAuth needs a public identity
#: domain to derive its client (`bluesky::oauth_public_url`).
FEED_DOMAIN = "bsky-feed.test"

FOLLOWED_DID = "did:plc:bobfaketestfollowed2345"
FOLLOWED_HANDLE = "bob.test"

#: A custom feed generator the user subscribes to, and an author whose post
#: only that generator serves — never the linked account's timeline.
FEED_GENERATOR_URI = "at://did:plc:feedgenfaketest23456789/app.bsky.feed.generator/fauna-e2e"
FEED_AUTHOR_DID = "did:plc:carolfaketestfeedauthor"
FEED_AUTHOR_HANDLE = "carol.test"

#: A followed author with a Bluesky display name (the bridged-author label).
NAMED_DID = "did:plc:danafaketestnamedauthor"
NAMED_HANDLE = "dana.test"
NAMED_DISPLAY = "Dana Example"

#: A painted-state re-read after a local gesture.
PAINT_S = 30.0

#: A Save of the private section in a fresh session first-need mints the
#: generation tip before its local write lands (`test_contact_overlay.py`'s
#: budget for the same first Save).
FIRST_SAVE_S = 240.0

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


def _link_through_callback(nest, far, user: dict | None = None) -> None:
    """Start the link as ``user`` (default the nest's one registered user),
    then land the authorization server's redirect on the nest's callback with
    the `state` it was handed at PAR."""
    user = user or nest["user"]
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


def _local_bluesky_posts(nest, text: str, user: dict | None = None) -> list[dict]:
    """``user``'s (default the nest's one registered user)
    ``fauna.feed.local.posts`` rows sourced ``bluesky`` whose card body is
    ``text`` (the list-card preview carries a bridged post's text — `feed.md`
    § The read model → *The list-card preview*)."""
    user = user or nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        local = client.call("fauna.feed.local.posts", {"limit": 50})
    return [
        p for p in local.get("posts", [])
        if p.get("source") == "bluesky" and p.get("body") == text
    ]


def _await_card(app, post_id: str) -> int:
    """The app's feed-card index for ``post_id``, once it paints."""
    def found():
        i = app.feed.post_index_by_id(post_id)
        return i + 1 if i >= 0 else 0  # truthy for index 0 too

    return wait_until(
        found, PAINT_S,
        diagnose=lambda: (
            f"the post {post_id} is on the wire but not in the app's feed; app "
            f"feed ids={[p.get('post_id') for p in app.feed._feed_posts_from_state()]!r}; "
            f"error={app.error_text()!r}"
        ),
    ) - 1


def _card_author(app, post_id: str) -> str:
    """The ``post-author`` painted inside the card for ``post_id`` — scoped, so
    an index into the unscoped ``post-author`` list cannot name another card."""
    i = app.feed.post_index_by_id(post_id)
    return app.driver.get_text("post-author", scope=f"post-card[{i}]") if i >= 0 else ""


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
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


@pytest.mark.tui
@pytest.mark.feature("atproto")
def test_posts_from_a_subscribed_bluesky_custom_feed_arrive_in_the_feed(
    request, app, bluesky_feed_nest, atproto_far_end,
):
    """A post only a custom feed generator serves reaches the feed once the
    user subscribes to that generator in the app — and not before: the first
    poll, with no subscription, must leave it out, so the subscription (one
    ``getFeed`` page per ``bridge_feed_subscriptions`` row) is what makes the
    witness green, never the timeline."""
    from conftest import _login_app_as

    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    text = "a post only the custom feed serves"
    far.publish_post(
        author_did=FEED_AUTHOR_DID, author_handle=FEED_AUTHOR_HANDLE, rkey="3kcustom1",
        text=text, on_timeline=False, feed_uri=FEED_GENERATOR_URI,
    )
    report = _poll_now(nest)
    assert report["errors"] == [], f"the first poll: {report!r}"
    assert _local_bluesky_posts(nest, text) == [], (
        "with no subscription yet the generator's post must not be in the feed — "
        "nothing but the subscription may make this witness green"
    )

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture="bluesky_feed_spa_url", verify_live_actor=True,
    )
    app.driver.navigate_to("feed")
    app.driver.wait_for("feed-view")
    app.driver.wait_for("bridge-feed-subscribe-toggle", timeout=20)
    app.driver.click("bridge-feed-subscribe-toggle")
    app.driver.wait_for("bridge-form-uri-input")
    app.driver.select("bridge-form-bridge-select", "bluesky")
    app.driver.clear_and_type("bridge-form-uri-input", FEED_GENERATOR_URI)
    app.driver.clear_and_type("bridge-form-name-input", "fauna e2e custom feed")
    app.driver.wait_until_enabled("bridge-form-subscribe-button")
    app.driver.click("bridge-form-subscribe-button")
    wait_until(
        lambda: app.driver.count("bridge-feed-unsubscribe-button") > 0, PAINT_S,
        diagnose=lambda: (
            f"the subscription row never appeared after Subscribe; "
            f"error={app.error_text()!r}"
        ),
    )

    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, (
        f"the poll after subscribing should store the generator's post: {report!r}; "
        f"far end saw {far.requests!r}"
    )
    wire = _local_bluesky_posts(nest, text)
    assert len(wire) == 1, (
        f"fauna.feed.local.posts should carry the custom feed's post {text!r} once; "
        f"got {wire!r}"
    )
    post_id = wire[0]["post_id"]

    app.feed.navigate()
    index = _await_card(app, post_id)
    card_text = app.feed.post_text(index).strip()
    assert card_text == text, (
        f"the custom feed's post card shows its text {text!r}; got {card_text!r}; "
        f"error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.feature("feed-read")
def test_a_bridged_author_shows_by_their_name_and_a_nickname_you_gave_them_comes_first(
    request, app, bluesky_feed_nest, atproto_far_end,
):
    """A Bluesky author's card names them by the display name Bluesky served —
    never their DID or a short id — and once the user saves a private nickname
    for them on their profile, the nickname heads the card instead (the one
    resolver: nickname → display name → handle → short id)."""
    from conftest import _login_app_as

    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    text = "a post from someone with a name on bluesky"
    far.publish_post(
        author_did=NAMED_DID, author_handle=NAMED_HANDLE, rkey="3knamed1", text=text,
        display_name=NAMED_DISPLAY,
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, (
        f"the poll should store the named author's post: {report!r}"
    )
    wire = _local_bluesky_posts(nest, text)
    assert len(wire) == 1, f"the named author's post on the wire once; got {wire!r}"
    post_id, author_hex = wire[0]["post_id"], wire[0]["author"]

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture="bluesky_feed_spa_url", verify_live_actor=True,
    )
    app.feed.navigate()
    _await_card(app, post_id)
    label = wait_until(
        lambda: (lambda t: t if t == NAMED_DISPLAY else "")(_card_author(app, post_id)),
        PAINT_S,
        diagnose=lambda: (
            f"the bridged author's card should name them {NAMED_DISPLAY!r} (their "
            f"Bluesky display name); it paints {_card_author(app, post_id)!r} — "
            f"author id {author_hex}, DID {NAMED_DID}; error={app.error_text()!r}"
        ),
    )
    assert label not in (NAMED_DID, author_hex) and author_hex[:8] not in label

    nickname = "Dana from bsky"
    app.profile.require_state_protocol_actor_nav_supported()
    app.profile.navigate_to_actor(author_hex)
    assert app.driver.is_visible("profile-nickname-field"), (
        f"the bridged author's profile ({author_hex}) should offer the private "
        f"section to name them; {app.driver.diagnose('profile-nickname-field')}; "
        f"error={app.error_text()!r}"
    )
    app.profile.set_private_nickname(nickname)
    app.profile.save_private()
    wait_until(
        lambda: app.profile.handle_text() == nickname, FIRST_SAVE_S,
        diagnose=lambda: (
            f"the profile header never became the saved nickname {nickname!r} "
            f"(header {app.profile.handle_text()!r}, error-message "
            f"{app.profile.error_text()!r})"
        ),
    )

    app.feed.navigate()
    _await_card(app, post_id)
    wait_until(
        lambda: _card_author(app, post_id) == nickname, PAINT_S,
        diagnose=lambda: (
            f"after saving the nickname {nickname!r} the card should name the "
            f"author by it; it paints {_card_author(app, post_id)!r}; "
            f"error={app.error_text()!r}"
        ),
    )

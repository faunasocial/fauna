"""A bridged post's own picture paints at once, fetched through the reader's nest.

``docs/goal/architecture/render-model.md`` § D6c (user-ruled 2026-09-30): a
bridged post's image attachment rests as a nest-relative proxied path and the
shared fold turns it into ``RenderBlock::ProxiedImage`` — the author's own
attachment, which the reader's own nest fetches, so it **paints immediately**:
never a ``doc-remote-image`` behind a reveal, and never dialed by the device.
It paints in the ``post-image`` slot, keyed by that path.

**The remote.** A fediverse note carries the picture as an AS2 ``Document`` on a
real ``https`` hostname. The nest's media proxy dials only ``https`` on global
addresses, so the file is served from loopback by
:class:`helpers.remote_media_server.RemoteMediaServer` and reached through the
proxy's two ``test-hooks`` seams — the shape
``test_feed_video_playback.py``'s bridged-video leg drives. The note itself is
delivered by a signed fake follower (``helpers.ap_nest.deliver_note``): its
arrival is a precondition, not the subject (convention 8b); the subject — the
card painting the picture with no reveal control — is the app's.

**What it proves:** the card's ``post-image`` reaches the painted state (on tui,
the half-block art is the element's text — ``FeedActions._post_image_states_in``),
no ``doc-remote-image`` registers on the card (no reveal button), and the remote
saw the picture's GET — the bytes came through the nest's proxy, the only dialer
the loopback remote is reachable from.

tier_3: a locally-built AP nest and the real app — tui led; linux, web and
android joined in the trickle-down, the paint read per app in
``FeedActions._post_image_states_in``.
Latency-independent (convention 14): every wait is a deadline poll.
"""

from __future__ import annotations

import uuid
from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier_3]

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"

MEDIA_HOST = "pictures.test"
PICTURE_PATH = "/bridged-picture.png"


@pytest.fixture(scope="module")
def remote_pictures(tmp_path_factory):
    from helpers.remote_media_server import RemoteMediaServer

    server = RemoteMediaServer(
        MEDIA_HOST,
        tmp_path_factory.mktemp("remote-pictures"),
        {PICTURE_PATH: (TEST_IMAGE.read_bytes(), "image/png")},
    )
    try:
        yield server
    finally:
        server.close()


@pytest.fixture(scope="module")
def bridged_picture_nest(remote_pictures, tmp_path_factory, ap_binary):
    """One AP-enabled nest whose media proxy reaches ``remote_pictures`` — its
    binary the ``ap_binary`` fixture's, which the collection-time prebuild warms
    outside every test's timeout (a direct ``_ensure_ap_nest_built()`` here paid
    a cold build inside the test and ran past its 900 s, 2026-11-08)."""
    from drivers.port_util import find_free_port
    from helpers.ap_nest import start_ap_nest

    port = find_free_port()
    nest = start_ap_nest(
        ap_binary,
        tmp_path_factory.mktemp("bridged-picture-nest"),
        port,
        f"127.0.0.1:{port}",
        extra_env=remote_pictures.nest_env(),
    )
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture
def bridged_picture_spa_url(static_dir, bridged_picture_nest):
    """Web-only SPA proxy onto ``bridged_picture_nest``."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, bridged_picture_nest["url"])
    yield url
    server.shutdown()


@pytest.mark.standalone_only
@pytest.mark.tui  # lead app
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("feed-images-and-video")
def test_a_bridged_posts_picture_paints_at_once_through_the_nest(
    request, app, bridged_picture_nest, remote_pictures
):
    """A fediverse note's picture paints in the card's ``post-image`` with no
    reveal control, its bytes fetched by the nest's media proxy."""
    from conftest import _login_app_as, _make_user
    from drivers.port_util import find_free_port
    from helpers.ap_nest import deliver_note, enable_ap
    from helpers.fake_follower import FakeFollower

    nest = bridged_picture_nest
    # The remote outlives this leg (module scope): judge only this leg's GETs.
    seen_before = len(remote_pictures.requests)

    def fetched() -> list[str]:
        return remote_pictures.requests[seen_before:]

    user = _make_user(nest)
    username, actor_url = enable_ap(nest, user)
    text = f"bridged-picture-{uuid.uuid4().hex[:8]}"
    with FakeFollower(find_free_port()) as follower:
        deliver_note(
            nest, follower, username, actor_url, text,
            attachment=[{
                "type": "Document",
                "mediaType": "image/png",
                "url": remote_pictures.url(PICTURE_PATH),
                "name": "a bridged picture",
            }],
        )

    # Web is single-origin: the page is served from this nest's SPA proxy for
    # the journey (`WebBridgeDriver.serve_from`), as a production nest serves it
    # — the `test_feed_video_playback.py` bridged leg's arm.
    restore = None
    if app.driver.is_web():
        restore = app.driver.serve_from(request.getfixturevalue("bridged_picture_spa_url"))
    try:
        _login_app_as(
            app, request, nest, user,
            spa_url_fixture="bridged_picture_spa_url", verify_live_actor=True,
        )
        assert app.feed.wait_for_post_text(text, timeout_s=30.0), (
            f"the bridged note {text!r} never rendered in the feed; "
            f"error={app.error_text()!r}"
        )

        assert app.feed.post_image_painted_by_text(text), (
            "a bridged post's picture paints at once through the nest, with no "
            f"reveal step; the remote saw {fetched()!r}; "
            f"{app.feed.post_image_paint_report()}"
        )
        idx = app.feed._find_post_index_by_text(text)
        assert idx >= 0, f"the card for {text!r} left the feed"
        reveal_targets = app.driver.count("doc-remote-image", scope=f"post-card[{idx}]")
        assert reveal_targets == 0, (
            "a bridged post's own picture is never a blocked remote image: the card "
            f"must carry no doc-remote-image, found {reveal_targets}"
        )
        assert PICTURE_PATH in fetched(), (
            "the picture's bytes should have been fetched through the nest's media "
            f"proxy; the remote saw {fetched()!r}"
        )
    finally:
        if restore is not None:
            restore()

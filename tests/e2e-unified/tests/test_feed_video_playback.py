"""Feed inline video playback — ``video-thumbnail`` as the player host.

``docs/goal/architecture/render-model.md`` § D6c → *Inline playback* (ruled
2026-10-02): a tap on ``video-thumbnail`` asks the shared
``FeedManager::playback_source`` projection what plays and swaps the play glyph
for the platform's NATIVE player in the same element (no new ui.yaml ID). The
element publishes ``state`` ∈ ``idle`` / ``loading`` / ``playing`` / ``error``
(documented in ``drivers/base.py::get_attr``); never autoplay — the tap is what
spends the bytes.

Web leads (inline AV is tui's declared absence — tui's leg is the OS external
handoff, its own test). The others add their marker here as their leg lands,
with a per-app fixture map: Playwright's bundled Chromium ships no H.264, so web
(and linux's GStreamer) play the VP9 ``tiny-video.webm``; AVFoundation plays no
WebM, so macos + ios (``AVPlayer`` in FaunaKit's ``VideoThumbnailView``) take the
H.264 ``tiny-video.mp4``, and so does windows (``MediaPlayerElement`` in
``VideoThumbnailView``: Media Foundation decodes H.264 on every Windows 11, while
VP9 needs an optional codec pack no test may assume).

Fixtures: ``tests/fixtures/tiny-video.webm`` — 5 s, 64x64, VP9, no audio
(``ffmpeg -f lavfi -i testsrc=s=64x64:d=5:r=10 -c:v libvpx-vp9 -b:v 50k
-pix_fmt yuv420p -an``) — and the 1 s, 64x64 H.264 ``tiny-video.mp4``
``test_feed_video_thumbnail.py`` already posts. The position witness is "rose
above zero", which holds for a clip of any length: a 1 s clip may well have
ended before a second read.

Position and source are read where each app's native player keeps them: web off
the ``<video>`` itself, the others off the ``position`` / ``source`` attributes
the element publishes beside ``state`` (``drivers/base.py::get_attr``) — on
windows packed into the element's one UIA HelpText and answered per key by the
FlaUI bridge (``flaui-bridge/PackedAttr.cs``), on linux by the automation agent
(``position`` off the ``gtk::Video``'s live media stream).

tier_3: real nest binary + real client driver, no mocks.
"""

import re
import time
import uuid
from pathlib import Path

import pytest

from helpers.app_surface import app_name, skip_unbuilt

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.linux,
]

FIXTURE_DIR = Path(__file__).parent.parent.parent / "fixtures"
FIXTURES = {
    "web": FIXTURE_DIR / "tiny-video.webm",
    "macos": FIXTURE_DIR / "tiny-video.mp4",
    "ios": FIXTURE_DIR / "tiny-video.mp4",
    "windows": FIXTURE_DIR / "tiny-video.mp4",
    "linux": FIXTURE_DIR / "tiny-video.webm",
}

_VIDEO = "document.querySelector('[data-testid=\"video-thumbnail\"] video')"


def _wait_state(driver, want: str, timeout: float) -> str | None:
    deadline = time.monotonic() + timeout
    state = None
    while time.monotonic() < deadline:
        state = driver.get_attr("video-thumbnail", "state")
        if state == want:
            return state
        time.sleep(0.25)
    return state


def _position(driver) -> float | None:
    """The native player's position in seconds; ``None`` when there is no player
    to ask."""
    if driver.is_web():
        value = driver.eval_js(f"{_VIDEO}.currentTime")
        return value if isinstance(value, (int, float)) else None
    try:
        return float(driver.get_attr("video-thumbnail", "position"))
    except (TypeError, ValueError):
        return None


def _source(driver) -> str:
    """What the native player was handed to play."""
    if driver.is_web():
        return str(driver.eval_js(f"{_VIDEO}.currentSrc"))
    return str(driver.get_attr("video-thumbnail", "source"))


@pytest.mark.feature("feed-images-and-video")
def test_tapping_a_video_plays_it_in_place(logged_in_app):
    """Tap a public post's ``video-thumbnail``: ``state`` walks ``idle`` →
    ``playing``, the native player's position advances, and it streams off the
    unauthenticated blob route the shared projection named."""
    _post_and_play_a_video(logged_in_app)


# Each fixture's length in seconds (the module docstring's ffmpeg lines).
FIXTURE_LENGTH_S = {"tiny-video.webm": 5.0, "tiny-video.mp4": 1.0}


@pytest.mark.feature("feed-images-and-video")
def test_a_video_played_to_its_end_reads_its_whole_length(logged_in_app):
    """A video played to its end reads as played to its end: its ``position``
    reaches the clip's length and stays there once the stream ends, as a web
    ``<video>``'s ``currentTime`` does (render-model.md § D6c → *Inline
    playback*; the media arm of ``watch-complete`` reads the played fraction,
    ``engagement-cues.md`` § Cue vocabulary & derivation).

    linux once read 0.0 again the moment the clip ended: GTK resets an ended
    stream's timestamp to 0, so a reader polling slower than the stream's last
    quarter-second saw a video played to its end as never started."""
    app = logged_in_app
    fixture = FIXTURES[app_name(app.driver)]
    length = FIXTURE_LENGTH_S[fixture.name]
    _post_and_play_a_video(app)

    def at_end() -> float | None:
        position = _position(app.driver)
        return position if position is not None and position >= 0.95 * length else None

    deadline = time.monotonic() + length + 20
    position = at_end()
    while position is None and time.monotonic() < deadline:
        time.sleep(0.2)
        position = at_end()
    assert position is not None, (
        f"the video never read as played to its end: position={_position(app.driver)!r}, "
        f"length={length}: {app.driver.diagnose('video-thumbnail')}"
    )
    # Past the end the stream stands still: whatever was read above, a read
    # once it has ended must still be the whole length, never back at 0.
    deadline = time.monotonic() + length + 5
    while time.monotonic() < deadline:
        time.sleep(0.25)
        position = _position(app.driver)
        assert position is not None and position >= 0.95 * length, (
            f"a video played to its end read {position!r} again (length {length}): "
            f"{app.driver.diagnose('video-thumbnail')}"
        )


@pytest.mark.feature("feed-images-and-video")
def test_a_playing_video_survives_a_feed_reload(logged_in_app):
    """A feed reload landing while a video plays keeps the player: posting a
    second note re-queries the feed and prepends a card above the video, and the
    ``video-thumbnail`` still reads ``state=playing`` once that note shows.

    The shared manager re-folds the media it already resolved onto the landed
    page (``FeedManager::reapply_resolved_media``), so the card never paints
    media-less, and the app keys its post list and media blocks by identity, so
    the shifted card is the same live element. Before, the landed page arrived
    unresolved, the card unmounted its thumbnail, and a fresh ``idle`` one
    mounted after the re-resolve — the same window that dropped a tap landing
    between the thumbnail's paint and the reload (render-model.md § D6c →
    *Inline playback*: a tap must not silently fall back to ``idle``)."""
    app = logged_in_app
    if app_name(app.driver) not in ("web", "linux", "windows"):
        skip_unbuilt(
            app.driver,
            surface="a playing video kept across a feed reload",
            detail="the shared re-fold keeps the media; this app's card keeping "
            "its live player across the landed page is not yet witnessed",
            tracked=(
                "render-model.md § D6c → Inline playback (each app keeps its "
                "player across a reload)"
            ),
        )
    _post_and_play_a_video(app)

    text = f"after-the-video-{uuid.uuid4().hex[:8]}"
    # `create_post` returns once the new note renders, i.e. once the re-queried
    # page has landed — the causal anchor for the read below (convention 14).
    app.feed.create_post(text)
    app.driver.barrier()
    state = app.driver.get_attr("video-thumbnail", "state")
    assert state == "playing", (
        f"the reload re-created the playing video's card: state={state!r}: "
        f"{app.driver.diagnose('video-thumbnail')}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_leaving_the_feed_while_a_video_plays_keeps_the_app_running(logged_in_app):
    """Leave the feed while a video plays, then come back: the app answers
    through the page change, and ``video-thumbnail`` is back on the card.

    Leaving the page takes the live native player down with it, which is where
    a faulty teardown kills the app: windows' ``MediaPlayerElement``, still
    holding the ``MediaPlayer`` the view had just disposed, died of a stowed
    exception there — after every passing playback test, unseen, until this one.
    What the element reads on return is each app's own (a page that keeps its
    player may still read ``playing``), so the read asks only for a phase from
    which the video can play on, never ``error`` or no answer."""
    app = logged_in_app
    _post_and_play_a_video(app)

    app.contacts.navigate()
    # An app that died tearing its player down never acknowledges the barrier.
    app.driver.barrier()
    app.feed.navigate()
    app.driver.wait_for("video-thumbnail", timeout=20)
    state = app.driver.get_attr("video-thumbnail", "state")
    assert state in ("idle", "playing"), (
        f"back on the feed the video should be there to play: state={state!r}: "
        f"{app.driver.diagnose('video-thumbnail')}"
    )


# ── The TLS twin: a nest on its self-signed floor ────────────────────────────
#
# `security.md` § Transport trust → *The native-player leg* (ruled 2026-10-08): a
# platform media player handed a nest URL dials through the OS media stack, which
# trusts the OS store alone — no loopback carve-out, no SPKI pin — so it refuses
# the self-signed floor every nest serves before ACME, a LAN nest serves for
# good, and the same-box install serves on `https://127.0.0.1:3000`. A native
# app therefore hands its player a byte source fed by the shared pinned
# `RangedNestReader`, never a URL. The plain-HTTP session nest above cannot see
# any of this, so the twin runs the same journey on the two TLS fixtures: the
# loopback floor (`self_signed_logged_in_app`) and a LAN-IP dial whose cert is
# trusted by the pin alone (`spki_pinned_logged_in_app`). Both fixtures declare
# web absent before they log in (`_declare_no_web_cert_trust`): the SPA is
# same-origin with its nest, so the browser's `<video>` already is the pinned
# peer and there is no app-side trust decision to witness.

@pytest.mark.feature("feed-images-and-video")
@pytest.mark.parametrize(
    "tls_app", ["self_signed_logged_in_app", "spki_pinned_logged_in_app"]
)
def test_a_video_plays_against_a_self_signed_nest(request, app, tls_app):
    """The same tap as ``test_tapping_a_video_plays_it_in_place``, against a nest
    whose cert no public CA signed: the player reaches ``state=playing`` because
    it reads through the app's pinned byte source, and the ``source`` it
    publishes is still the blob route's nest-relative path."""
    # Each native app's reader-backed adapter lands on its own, red first; apple's
    # joins the twin with its own. Skipped before the TLS fixture spends a nest.
    if app_name(app.driver) in ("macos", "ios"):
        skip_unbuilt(
            app.driver,
            surface="inline playback against a self-signed nest (the reader-backed player)",
            detail="the Url arm still hands AVPlayer a nest URL, which the OS media "
            "stack refuses on the self-signed floor",
            tracked=(
                "security.md § Transport trust → The native-player leg"
                ""
            ),
        )
    _post_and_play_a_video(request.getfixturevalue(tls_app))


def _post_and_play_a_video(app) -> None:
    """Post a video as ``app``'s user, tap its thumbnail, and assert it plays in
    place off the blob route."""
    fixture = FIXTURES[app_name(app.driver)]
    assert fixture.exists(), f"{fixture} missing"
    text = f"video-play-{uuid.uuid4().hex[:8]}"
    app.feed.create_post_with_video(text=text, video_path=str(fixture))
    app.driver.wait_for("video-thumbnail", timeout=20)

    assert app.driver.get_attr("video-thumbnail", "state") == "idle", (
        "a video must never autoplay: before the tap the thumbnail is idle"
    )
    app.driver.click("video-thumbnail")
    state = _wait_state(app.driver, "playing", timeout=20)
    assert state == "playing", (
        f"the tap should reach state=playing, got {state!r}: "
        f"{app.driver.diagnose('video-thumbnail')}"
    )

    # The position advancing is the latency-independent witness that frames are
    # decoding (convention 14): poll for the condition, never sleep-then-assert.
    position = _position(app.driver)
    assert position is not None, (
        f"no native player in the slot: {app.driver.diagnose('video-thumbnail')}"
    )
    deadline = time.monotonic() + 10
    while not position and time.monotonic() < deadline:
        time.sleep(0.1)
        position = _position(app.driver)
    assert position and position > 0, (
        f"playback position should advance past 0, got {position!r}"
    )

    current_src = _source(app.driver)
    assert re.search(r"/api/v1/blob/[0-9a-f]+$", current_src), (
        f"a public video plays off the blob route, got {current_src!r}"
    )


# ── A bridged video, played through the ticketed media proxy ────────────────
#
# The only writer of a `ProxiedVideo` is an ActivityPub attachment
# (`fauna-bridge-activitypub::translate`): an https attachment rests as the
# proxy's nest-relative path. So the seed is a fediverse note with a video
# `Document` attachment, delivered by the in-test follower to a dedicated AP
# nest (`helpers.ap_nest.start_ap_nest`, the `activitypub,test-hooks` build —
# hence `standalone_only`). The remote file is served over TLS on loopback
# under a mapped hostname (`helpers.remote_media_server`), which the nest's
# media proxy reaches through its `FAUNA_TEST_MEDIA_PROXY_*` test hooks.

MEDIA_HOST = "media.test"
MEDIA_FILES = {
    "web": ("/clip.webm", "video/webm"),
    "macos": ("/clip.mp4", "video/mp4"),
    "ios": ("/clip.mp4", "video/mp4"),
    "windows": ("/clip.mp4", "video/mp4"),
    "linux": ("/clip.webm", "video/webm"),
}


@pytest.fixture(scope="module")
def remote_media(tmp_path_factory):
    from helpers.remote_media_server import RemoteMediaServer

    files = {
        path: (FIXTURES[app].read_bytes(), content_type)
        for app, (path, content_type) in MEDIA_FILES.items()
    }
    server = RemoteMediaServer(MEDIA_HOST, tmp_path_factory.mktemp("remote-media"), files)
    try:
        yield server
    finally:
        server.close()


@pytest.fixture(scope="module")
def bridged_video_nest(remote_media, tmp_path_factory):
    """One AP-enabled nest whose media proxy reaches ``remote_media``."""
    from conftest import _ensure_ap_nest_built
    from drivers.port_util import find_free_port
    from helpers.ap_nest import start_ap_nest

    port = find_free_port()
    nest = start_ap_nest(
        _ensure_ap_nest_built(),
        tmp_path_factory.mktemp("bridged-video-nest"),
        port,
        f"127.0.0.1:{port}",
        extra_env=remote_media.nest_env(),
    )
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture
def bridged_video_spa_url(static_dir, bridged_video_nest):
    """Web-only SPA proxy onto ``bridged_video_nest``."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, bridged_video_nest["url"])
    yield url
    server.shutdown()


@pytest.mark.standalone_only
@pytest.mark.feature("feed-images-and-video")
def test_tapping_a_bridged_video_plays_it_through_the_ticketed_proxy(
    request, app, bridged_video_nest, remote_media
):
    """A fediverse post's video attachment folds to a ``ProxiedVideo``: its
    ``video-thumbnail`` loads no remote byte before the tap (§ D6c → *Proxied
    video*), and the tap plays it from the proxied path ``playback_source``
    ticketed — ``currentSrc`` carries the ticket's ``exp`` and ``sig``, and the
    remote file is fetched through the nest."""
    from conftest import _login_app_as, _make_user
    from helpers.ap_nest import deliver_note, enable_ap
    from helpers.fake_follower import FakeFollower
    from drivers.port_util import find_free_port

    name = app_name(app.driver)
    if name not in ("web", "windows", "linux"):
        skip_unbuilt(
            app.driver,
            surface="ProxiedVideo playback (video-thumbnail on a bridged post)",
            detail="the thumbnail paints the glyph + path inert; the tap is not "
            "wired to playback_source's ticketed Url yet",
            tracked=(
                "render-model.md § D6c → Inline playback (each native app wires its "
                "ProxiedVideo tap)"
            ),
        )
    nest = bridged_video_nest
    path, content_type = MEDIA_FILES[name]
    user = _make_user(nest)
    username, actor_url = enable_ap(nest, user)
    text = f"bridged-video-{uuid.uuid4().hex[:8]}"
    with FakeFollower(find_free_port()) as follower:
        deliver_note(
            nest, follower, username, actor_url, text,
            attachment=[{
                "type": "Document",
                "mediaType": content_type,
                "url": remote_media.url(path),
                "name": "a bridged clip",
            }],
        )

    # Web is single-origin: its CSP lets a `<video src>` reach only the page's
    # own origin, so the page is served from this nest's SPA proxy for the
    # journey (`WebBridgeDriver.serve_from`), as a production nest serves it.
    restore = None
    if app.driver.is_web():
        restore = app.driver.serve_from(request.getfixturevalue("bridged_video_spa_url"))
    try:
        _login_app_as(
            app, request, nest, user,
            spa_url_fixture="bridged_video_spa_url", verify_live_actor=True,
        )
        assert app.feed.wait_for_post_text(text, timeout_s=30.0), (
            f"the bridged note {text!r} never rendered in the feed; "
            f"error={app.error_text()!r}"
        )
        app.driver.wait_for("video-thumbnail", timeout=20)

        assert app.driver.get_attr("video-thumbnail", "state") == "idle", (
            "a video must never autoplay: before the tap the thumbnail is idle"
        )
        # § D6c → *Proxied video*, answer 2: the play glyph + the nest-relative
        # path, over a poster-less frame — and no byte load before the tap. The
        # glyph is text only on web; a native app draws its platform's play icon
        # (windows' Segoe MDL2 glyph, which the FlaUI bridge's text read skips),
        # so the automation text there carries the path alone.
        label = app.driver.get_text("video-thumbnail")
        assert "/api/v1/media/proxy?url=" in label, (
            f"a ProxiedVideo thumbnail paints its proxied path, got {label!r}"
        )
        if app.driver.is_web():
            assert "▶" in label, (
                f"a ProxiedVideo thumbnail paints the play glyph, got {label!r}"
            )
        assert path not in remote_media.requests, (
            "a ProxiedVideo thumbnail must load no remote byte before the tap; "
            f"the remote saw {remote_media.requests!r}"
        )

        app.driver.click("video-thumbnail")
        state = _wait_state(app.driver, "playing", timeout=20)
        assert state == "playing", (
            f"the tap should reach state=playing, got {state!r}: "
            f"{app.driver.diagnose('video-thumbnail')}"
        )
        position = _position(app.driver)
        deadline = time.monotonic() + 10
        while not position and time.monotonic() < deadline:
            time.sleep(0.1)
            position = _position(app.driver)
        assert position and position > 0, (
            f"playback position should advance past 0, got {position!r}"
        )

        current_src = _source(app.driver)
        assert "/api/v1/media/proxy?url=" in current_src, (
            f"a bridged video plays through the nest's media proxy, got {current_src!r}"
        )
        assert re.search(r"[?&]exp=\d+", current_src) and re.search(
            r"[?&]sig=[A-Za-z0-9_-]+", current_src
        ), f"the proxied path must carry the playback ticket, got {current_src!r}"
        assert path in remote_media.requests, (
            "the remote file should have been fetched through the proxy; "
            f"the remote saw {remote_media.requests!r}"
        )
    finally:
        if restore is not None:
            restore()

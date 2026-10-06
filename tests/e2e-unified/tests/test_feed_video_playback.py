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
H.264 ``tiny-video.mp4``, and windows will.

Fixtures: ``tests/fixtures/tiny-video.webm`` — 5 s, 64x64, VP9, no audio
(``ffmpeg -f lavfi -i testsrc=s=64x64:d=5:r=10 -c:v libvpx-vp9 -b:v 50k
-pix_fmt yuv420p -an``) — and the 1 s, 64x64 H.264 ``tiny-video.mp4``
``test_feed_video_thumbnail.py`` already posts. The position witness is "rose
above zero", which holds for a clip of any length: a 1 s clip may well have
ended before a second read.

Position and source are read where each app's native player keeps them: web off
the ``<video>`` itself, the others off the ``position`` / ``source`` attributes
the element publishes beside ``state`` (``drivers/base.py::get_attr``).

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
]

FIXTURE_DIR = Path(__file__).parent.parent.parent / "fixtures"
FIXTURES = {
    "web": FIXTURE_DIR / "tiny-video.webm",
    "macos": FIXTURE_DIR / "tiny-video.mp4",
    "ios": FIXTURE_DIR / "tiny-video.mp4",
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
    app = logged_in_app
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


def test_tapping_a_bridged_video_plays_it_through_the_ticketed_proxy(logged_in_app):
    """A bridged post's ``ProxiedVideo`` plays from the ticketed proxy URL
    ``playback_source`` mints. Owed once the ticket arm lands."""
    skip_unbuilt(
        logged_in_app.driver,
        surface="ProxiedVideo playback (video-thumbnail on a bridged post)",
        detail="RenderBlock::ProxiedVideo folds since 2026-10-03, but "
        "playback_source has no ticket arm yet and answers Unplayable for one",
        tracked="render-model.md § D6c → Inline playback (playback_source's "
        "ticket arm)",
    )

"""Feed post video attachment — ``video-thumbnail``.

``tests/e2e-unified/ui.yaml`` ``video-thumbnail`` (inside ``post-card``).
Since 2026-08-15 (render-model.md § D6b) the shared feed fold is TYPED and
multi-item: `RenderBlock::Video { hash, alt }` is a sibling of `Image`, folded
by `fauna_feed::media_blocks` off the attachment's real `media_type`, so every
document-painting app branches off the SAME shared document instead of a
per-app decode. `first_video_hash()` / `render_document_first_video_hash`
(UniFFI) / `renderDocumentFirstVideoHash` (wasm) are the single-element
accessor every app's paint leg reads.

Fixture: ``tests/fixtures/tiny-video.mp4`` — a 1-second, 64x64 h264 clip
generated with ffmpeg (``ffmpeg -f lavfi -i color=... -c:v libx264
-preset ultrafast``), committed so this test carries no external
dependency.

Built: tui (lead, 2026-08-15), web (adopted the same commit — its second
app-side `decodePost` was retired in favor of `documentMediaBlocks`), linux +
android (neither app has a poster frame
to paint since no writer populates `MediaItem::thumbnail`, so both mirror
tui's play-glyph-+-hash text rather than inventing one). android's leg is
compile-verified only here (no host emulator on this dev VM — the standing
android-e2e gap every app-gated row carries); the marker stays so a machine
with the emulator picks it up automatically. macos + ios: `VideoThumbnailView` (FaunaKit, shared by both targets), same
play-glyph + hash text render — four call sites (macOS card + detail, iOS
card + detail), mirroring each app's existing `documentMediaImageHash`-style
wrapper. windows: landed 2026-08-26
— `DocumentRenderer.MediaVideoHash` twinning the existing `MediaImageHash`
wrapper, painted at the feed list card + post-detail dialog, closing D6b on
all 7 apps.

Along the way (2026-08-15) this found and fixed a real, separate gap in the
shared ``fauna_media::process::sniff_mime`` (used by the NATIVE upload path —
`fauna-client::media_upload`, i.e. linux/tui — unlike web's browser-supplied
mime): it recognized image formats + PDF but had no video signature at all,
so any native video upload was misclassified as
``application/octet-stream`` forever. Fixed to detect the ISO-BMFF ``ftyp``
box (mp4/mov/m4a/3gp) and the WebM/Matroska EBML header
(`libs/fauna-media/tests/process_test.rs::sniff_mime_mp4`/`_webm`).

tier_3: real nest binary + real client driver, no mocks.
"""

import uuid
from pathlib import Path

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

FIXTURE_DIR = Path(__file__).parent.parent.parent / "fixtures"
TEST_VIDEO = FIXTURE_DIR / "tiny-video.mp4"


@pytest.mark.feature("feed-images-and-video")
def test_video_attachment_renders_video_thumbnail(logged_in_app):
    """Posting with a real ``.mp4`` attachment renders ``video-thumbnail``
    (not ``post-image``) on the post's card."""
    if not TEST_VIDEO.exists():
        pytest.skip("tiny-video.mp4 fixture not found")
    app = logged_in_app
    text = f"video-post-{uuid.uuid4().hex[:8]}"
    app.feed.create_post_with_video(text=text, video_path=str(TEST_VIDEO))
    app.driver.wait_for("video-thumbnail", timeout=20)
    assert app.driver.is_visible("video-thumbnail"), (
        "a post with a video attachment should render video-thumbnail: "
        f"{app.driver.diagnose('video-thumbnail')}"
    )

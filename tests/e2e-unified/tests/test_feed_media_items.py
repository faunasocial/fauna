"""A post's every picture and video, in the order attached, each labelled with its author's alt text.

``docs/features/feed-images-and-video.md`` outcomes 16 ("a post with several
pictures or videos shows all of them, in the order they were attached") and 18
("a picture or video carries its author's own description as its label, and a
generic label only when there is none").

The shared fold already carries both: ``fauna_feed::media_blocks`` folds **every**
``MediaItem`` into one typed block, in body order, each with the item's own
``alt`` (render-model.md § D6b, and § D6c → *Proxied video*, the ``alt``
paragraph). What this pins is the app leg — each app paints one ``post-image``
per picture block and one ``video-thumbnail`` per video block off
``RenderDocument::media_blocks()``, on the list card and in the post detail, and
publishes the block's ``alt`` (its fixed generic text when the author wrote none)
as the element's ``alt`` attribute. The element's text stays what every older
witness reads (the hash or proxied path as the placeholder, the painted art once
the bytes land), so the label rides an attribute every driver's ``get_attr``
answers — web's ``<img alt>`` is the attribute itself.

**Seeding.** The composer attaches one file, so the multi-item post is the
user's own ``fauna.posts.create`` carrying three pictures and a video — a
precondition, not the journey (e2e-conventions.md convention 8b); what the user
does — read the feed and open the post — stays in the app.

tier_3: real nest binary + real app driver, no mocks.
"""

import time
import uuid
from pathlib import Path

import pytest

from helpers.blob_upload import upload_blob
from helpers.png_chunks import png_with_chunk
from tests.api import ws_api
from tests.api.bare import media_item, sign_and_encode_post

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"
TEST_VIDEO = Path(__file__).parent.parent.parent / "fixtures" / "tiny-video.mp4"

FIRST_ALT = "A red square at dusk"
VIDEO_ALT = "One second of a grey frame"
SECOND_ALT = "The same square, at noon"


def _unique_png(tag: str) -> bytes:
    # Blob storage is content-addressed: each picture needs its own bytes, or the
    # post would name one blob three times and order could not be read.
    marker = f"{tag}-{uuid.uuid4().hex}".encode()
    return png_with_chunk(TEST_IMAGE.read_bytes(), b"tEXt", b"fauna\x00" + marker)


def _read_media(app, scope: str) -> tuple[list[str | None], list[str | None]]:
    d = app.driver
    return (
        d.get_attrs("post-image", "alt", scope=scope),
        d.get_attrs("video-thumbnail", "alt", scope=scope),
    )


def _wait_media(app, scope: str, want_images: list[str], want_video: list[str]):
    deadline = time.monotonic() + 20
    images, videos = _read_media(app, scope)
    while time.monotonic() < deadline:
        if images[: len(want_images)] == want_images and videos == want_video and len(images) == 3:
            break
        time.sleep(0.5)
        images, videos = _read_media(app, scope)
    return images, videos


def _assert_media(app, scope: str, surface: str) -> None:
    images, videos = _wait_media(app, scope, [FIRST_ALT, SECOND_ALT], [VIDEO_ALT])
    assert len(images) == 3, (
        f"the {surface} must paint all three pictures, one post-image each, got "
        f"{len(images)} (alts {images!r}): {app.driver.diagnose('post-image')}"
    )
    assert images[:2] == [FIRST_ALT, SECOND_ALT], (
        f"the {surface}'s pictures must carry their authors' alt texts in the order "
        f"attached, got {images!r}"
    )
    generic = images[2]
    assert generic and generic not in (FIRST_ALT, SECOND_ALT, VIDEO_ALT), (
        f"the {surface}'s undescribed picture must carry a generic label of its own, "
        f"never empty and never a sibling's description, got {generic!r}"
    )
    assert videos == [VIDEO_ALT], (
        f"the {surface} must paint the video once, labelled with its author's alt "
        f"text, got {videos!r}: {app.driver.diagnose('video-thumbnail')}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_every_media_item_paints_in_order_with_its_alt(
    logged_in_app, nest_instance, test_user
):
    """A post with three pictures and a video shows all four, pictures in the order
    attached, each picture and the video labelled with its own alt text and the
    undescribed picture with the generic label — on the card and in the detail."""
    if not TEST_VIDEO.exists():
        pytest.skip("tiny-video.mp4 fixture not found")
    app = logged_in_app
    port = nest_instance["port"]
    from conftest import live_user_token

    token = live_user_token(nest_instance, test_user)
    first = _unique_png("first")
    second = _unique_png("second")
    third = _unique_png("third")
    video = TEST_VIDEO.read_bytes()
    uploads = [
        (first, "image/png", FIRST_ALT),
        (video, "video/mp4", VIDEO_ALT),
        (second, "image/png", SECOND_ALT),
        (third, "image/png", None),
    ]
    items = []
    for data, mime, alt in uploads:
        blob_hash = upload_blob(port, token, data, audience_class="PublicPost", mime=mime)
        items.append(media_item(blob_hash, mime, len(data), alt=alt))

    text = f"media-items-{uuid.uuid4().hex[:8]}"
    ws_api.create_post(
        port,
        test_user,
        sign_and_encode_post(
            test_user["signing_key"],
            int(time.time() * 1_000_000),
            text,
            media_items=items,
        ),
    )

    app.feed.reenter()
    assert app.feed.wait_for_post_text(text), (
        f"post {text!r} should reach the feed: error={app.error_text()!r}"
    )
    index = app.feed.post_index_by_text(text)
    assert index >= 0, f"the post's card must be locatable: {app.driver.diagnose('post-card')}"
    _assert_media(app, f"post-card[{index}]", "list card")

    app.feed.open_post_detail(index)
    assert app.feed.post_detail_visible(), (
        f"the post detail must open: {app.driver.diagnose('feed-post-detail-dialog')}"
    )
    _assert_media(app, "feed-post-detail-dialog", "post detail")

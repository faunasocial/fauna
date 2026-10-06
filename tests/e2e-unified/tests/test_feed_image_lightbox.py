"""Feed post image — ``image-lightbox`` (full-screen viewer): web, macos, ios,
linux, tui, windows.

``tests/e2e-unified/ui.yaml`` ``image-lightbox`` (inside ``post-card``,
``feed.post_detail``). ``C2paImage.svelte`` (feed's ``post-image``) opens the
lightbox on click regardless of C2PA content, so a plain image fixture
exercises it — no need for the C2PA-signed one. macOS/iOS mirror this exactly:
the shared FaunaKit ``ImageLightbox`` opens on a ``post-image`` tap inside the
feed list's ``post-card`` (``PostCardView.swift`` / ``MacPostCardView.swift``).

✅ linux + tui joined 2026-07-30, closing the divergence ui.yaml's
`image-lightbox` notes called "a real priority-#1 divergence, not just a
coverage gap". Until then linux's `views/lightbox.rs::show_image_lightbox` had
zero callers from the feed — the `post-image` click wrote the blob to a
hard-coded `/work/tmp/fauna-blob-…` path and opened nothing (a stale doc
comment on `post_list.rs::build_post_image` claimed otherwise). It now reuses
the texture the card already decoded (`show_image_lightbox_paintable`). tui
paints its lightbox as half-block art re-rasterized at `LIGHTBOX_COLS`, so the
picture is genuinely enlarged rather than the card preview repeated.
So five apps now agree on one surface: **click the feed card's `post-image`**.

✅ windows joined 2026-09-07: the list card's `post-image` was a static
`Image` with no click handler and `FeedPage.ShowImageLightbox` had zero
callers — genuinely unwired, not merely unverified. `post-image` is now a
transparent Button wrapping the Image (the same nested-Button-inside-
post-card idiom `load-remote-content-button`/`feed-post-actions-button`
already use) whose Click opens `image-lightbox` from the already-loaded
bitmap — a bare `Image.Tapped` was tried first and reliably hit FlaUI's
physical-SendInput "Access is denied" fallback (a known off-foreground
flake). The Button gives it a real UIA InvokePattern instead, driven
directly with no physical input and no double-fire risk.

android has its own ImageLightbox/ContentDialog implementation per ui.yaml
but its wiring to a post-card click is unverified — not attempted here;
widen when that area verifies it.

✅ GREEN on macos/ios since 2026-07-18. This file previously carried an
"EXPECTED RED" warning: image attachment was unimplemented on apple end-to-end,
so no post ever got an image and the test never reached its own lightbox-click
assertion. That gap is closed — `FeedVM.attachComposeFile` wires the
`compose.file` state-injection command this test's `create_post_with_image`
relies on, on both platforms, and `test_feed_image_lightbox.py` +
`test_post_with_image`/`test_post_with_image_and_tags` were confirmed live on
both `--client macos` and `--client ios`. A red here is now a real signal —
diagnose it, don't attribute it to the old attachment gap.

The production picker behind `compose-file` is likewise implemented as of
2026-07-20 (shared FaunaKit `ComposeAttachButton`, `.fileImporter`), replacing
macOS's no-op `NSOpenPanel` and iOS's `.disabled(true)` paperclip. That path is
deliberately NOT what this test drives: an OS file panel can't be driven
in-process (`drivers/http_bridge.py::set_input_files` — "Native bridges
(AT-SPI, FlaUI, Apple) can't control file picker dialogs"), so the injection
command remains the e2e seam and calls the very same `attachComposeFile` seam
the button calls. The mechanism is therefore covered headlessly end-to-end;
only the panel's own pixels ever need a human eye.

tier_3: real nest binary + real client driver, no mocks — a real image
upload, not an injected/stubbed post.
"""

import uuid
from pathlib import Path

import pytest

from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.windows,
]

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


@pytest.mark.feature("feed-images-and-video")
def test_post_image_click_opens_lightbox(logged_in_app):
    """Clicking a rendered post-image opens ``image-lightbox``."""
    if not TEST_IMAGE.exists():
        pytest.skip("test-image.png fixture not found")
    app = logged_in_app
    text = f"lightbox-{uuid.uuid4().hex[:8]}"
    app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))
    assert app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image "
        f"(a red here on macos/ios is the known compose.file gap, not a "
        f"lightbox bug — see this file's docstring): error={app.error_text()!r}"
    )

    app.driver.click("post-image", index=0)
    # Generous, deadline-polled — a native modal (e.g. WinUI ContentDialog.ShowAsync)
    # can take a beat past the click's own round trip to actually present; the loop
    # exits the moment it settles, so a fast platform pays none of the budget
    # (testing.md § point 14).
    wait_until(
        lambda: app.driver.is_visible("image-lightbox"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "clicking post-image should open the image-lightbox: "
            f"{app.driver.diagnose('image-lightbox')}"
        ),
    )

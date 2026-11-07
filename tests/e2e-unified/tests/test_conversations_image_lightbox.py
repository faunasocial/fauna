"""Conversations received image — ``image-lightbox`` (full-screen viewer): tui,
web, linux, macos, ios.

``tests/e2e-unified/ui.yaml`` ``components.image-lightbox`` lists
``conversations.conversation_detail`` in its ``used_in``, and the conversations
page's app glue owns "Image lightbox UI" (``docs/goal/ui/conversations.md``
§ Where logic lives → *App glue*). The trigger is the feed's own shape, applied
to the conversation bubble: activating a received ``dm-attachment-image`` opens
``image-lightbox`` over the conversation, and Escape (ui.yaml declares no
dismiss element inside the lightbox) closes it.

tui leads (``docs/goal/architecture/testing.md`` § Default app and nest mode):
it paints the lightbox from the one painter its feed already uses
(``apps/fauna-tui/src/lightbox.rs``), re-rasterized at ``LIGHTBOX_COLS`` so the
picture is genuinely enlarged. web joined through the feed's own component:
the bubble's picture is a ``C2paImage``, whose click opens ``Lightbox.svelte``.
linux joined by giving the bubble's picture the feed ``post-image``'s shape (a
flat button wrapping the ``gtk::Picture``) whose click opens
``views/lightbox.rs::show_image_lightbox_paintable`` from the decoded texture,
and the lightbox window binds Escape to close. macos and ios joined through the
shared FaunaKit ``DmMessageBubble``, which opens the shared ``ImageLightbox`` (the
feed card's trigger) on the decoded bytes it painted; the lightbox takes Escape.
windows and android follow; widen the marks as each one wires the trigger.

The attachment arrives through the inject seam (a *received* message, the row's
subject) with bytes unique to this run, so the picture is this run's own and
paints from the device's store.

tier_3: real nest binary + real client driver, no mocks.
"""

import base64
import secrets
from pathlib import Path

import pytest

from helpers.budgets import UI_SETTLE_S
from helpers.png_chunks import png_with_chunk
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
]

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


@pytest.mark.feature("conversation-attachments")
def test_received_attachment_image_click_opens_lightbox(logged_in_app):
    """Clicking a received ``dm-attachment-image`` opens ``image-lightbox``; Escape
    closes it and leaves the conversation open behind it."""
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    token = secrets.token_hex(4)
    picture = png_with_chunk(FIXTURE_IMAGE.read_bytes(), b"tEXt", b"run\x00" + token.encode())
    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="petra-lightbox@self-nest.test",
        body=f"a picture to enlarge {token}",
        attachments=[
            {"filename": "pic.png", "mime_type": "image/png", "data_base64": base64.b64encode(picture).decode()},
        ],
    )
    wait_until(
        lambda: conv.attachment_image_states() == ["painted"],
        UI_SETTLE_S,
        diagnose=lambda: f"the received picture should paint: {conv.attachment_image_states()}",
    )
    assert driver.is_absent("image-lightbox"), "no lightbox until one is opened"

    driver.click("dm-attachment-image", 0)
    wait_until(
        lambda: driver.is_visible("image-lightbox"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "clicking dm-attachment-image should open the image-lightbox: "
            f"{driver.diagnose('image-lightbox')}"
        ),
    )

    driver.press_key("image-lightbox", "Escape")
    wait_until(
        lambda: driver.is_absent("image-lightbox"),
        UI_SETTLE_S,
        diagnose=lambda: f"Escape should close the lightbox: {driver.diagnose('image-lightbox')}",
    )
    assert driver.is_visible("thread-header"), "closing the lightbox must leave the conversation open"

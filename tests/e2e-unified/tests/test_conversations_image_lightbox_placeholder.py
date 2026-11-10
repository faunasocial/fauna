"""Conversations — a received picture whose bytes this device no longer holds still
opens ``image-lightbox``, as its placeholder: tui, windows, linux, web, macos, ios.

``docs/goal/ui/conversations.md`` § User actions, the ``dm-attachment-image[i]`` row:
"A picture whose bytes the device does not hold opens as the placeholder, never a
dropped click", and "Escape closes it and stays on the thread". The painted case is
``test_conversations_image_lightbox.py``; this is the other load state of the same
trigger.

tui keeps the picture a button in every load state and its one lightbox painter
paints the placeholder for a picture it cannot resolve (``apps/fauna-tui/src/
lightbox.rs``). windows wraps the placeholder in the same transparent Button as the
painted picture, and the page opens ``Controls/ImageLightboxOverlay`` through its
placeholder form. linux wraps it in the same flat button as the painted picture and
opens ``views/lightbox.rs::show_image_lightbox_placeholder``; web makes it a button
opening ``Lightbox.svelte`` in its placeholder mode. macos and ios share FaunaKit's
``DmMessageBubble``: the placeholder carries the same tap and driver-click action as
the painted picture and opens ``ImageLightbox`` with a placeholder item (the declared
text in the picture's place). android leaves the placeholder inert today (no click
handler on it); widen the marks when it opens it.

The picture is seen painted first and then evicted through the shared test seam
(``conversations_evict_attachment``, the retention witness's), so the placeholder is
what this same attachment turns into. The inject seam records no location for it, so
nothing refills it while the test clicks.

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
    pytest.mark.feature("conversation-attachments"),
    pytest.mark.tui,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
]

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def test_a_picture_without_its_bytes_opens_the_lightbox_as_its_placeholder(logged_in_app):
    """Clicking a received ``dm-attachment-image`` that shows its placeholder opens
    ``image-lightbox``; Escape closes it and leaves the conversation open."""
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    token = secrets.token_hex(4)
    picture = png_with_chunk(FIXTURE_IMAGE.read_bytes(), b"tEXt", b"run\x00" + token.encode())
    thread_id = conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="petra-lightbox-gone@self-nest.test",
        body=f"a picture you will lose {token}",
        attachments=[
            {"filename": "pic.png", "mime_type": "image/png", "data_base64": base64.b64encode(picture).decode()},
        ],
    )
    wait_until(
        lambda: conv.attachment_image_states() == ["painted"],
        UI_SETTLE_S,
        diagnose=lambda: f"the picture should paint while its bytes are held: {conv.attachment_image_states()}",
    )
    conv.evict_attachment(thread_id, "pic.png")
    wait_until(
        lambda: conv.attachment_image_states() == ["placeholder"],
        UI_SETTLE_S,
        diagnose=lambda: f"a picture without its bytes shows its placeholder: {conv.attachment_image_states()}",
    )
    assert driver.is_absent("image-lightbox"), "no lightbox until one is opened"

    driver.click("dm-attachment-image", 0)
    wait_until(
        lambda: driver.is_visible("image-lightbox"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "clicking a placeholder dm-attachment-image should open the image-lightbox "
            f"(never a dropped click): {driver.diagnose('image-lightbox')} "
            f"states={conv.attachment_image_states()}"
        ),
    )

    driver.press_key("image-lightbox", "Escape")
    wait_until(
        lambda: driver.is_absent("image-lightbox"),
        UI_SETTLE_S,
        diagnose=lambda: f"Escape should close the lightbox: {driver.diagnose('image-lightbox')}",
    )
    assert driver.is_visible("thread-header"), "closing the lightbox must leave the conversation open"

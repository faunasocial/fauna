"""Conversation attachments: an inbound message's attachments render for real.

The shared SMTP attachment rail produces ``MessageSnapshot.attachments``
(``AttachmentSnapshot { blob_hash, filename, mime_type, size_bytes, is_image,
c2pa }``); each app renders ``dm-attachment-image[i]`` / ``dm-attachment-file[i]``
off that structured field, resolving ``blob_hash`` to real bytes through the
shared loader ``ConversationsManager.attachment_bytes``
(``docs/goal/ui/conversations.md`` § Attachments — Per-app render lift).

Driven through the conversations inject seam (``inject_inbound_for_test`` with an
``attachments`` payload of ``{filename, mime_type, data_base64}`` entries): every
app routes those entries through the shared Rust seam
``ConversationsManager::make_attachment_for_test``, which decodes the bytes,
hashes them (BLAKE3 → ``blob_hash``), and caches them under that handle — exactly
as a real inbound MIME parse does — so the rendered bubble resolves the handle to
real bytes through ``attachment_bytes`` and the image-render arm fires, the same
path a live SMTP receive exercises without a backend round-trip.

Verified clients: linux (the lead render lift) + apple (macOS + iOS — the
``DmMessageBubble`` ``FaunaImage.decode`` → ``Image(platformImage:)`` arm, sharing
linux's inject path via ``ConversationsTestInject`` → ``makeAttachmentForTest``).
Each other app adds its marker when it lifts the bubble's attachment render
off the generic-icon stub (android) / the legacy ``[attachment:HASH:mime]``
body-marker (windows) onto the structured field; web via the wasm passthrough.
tui joined 2026-07-30 — its bubble reads the same ``RenderBlock::Attachment``
blocks and rasterizes an image leaf through the ``thumbnail::rasterize`` path
feed's ``post-image`` uses (the inject seam and ``make_attachment_for_test``
call were already wired; only the render was missing).

⚠ Apple e2e runs on a SINGLE serialized runner (AutomationMode is machine-wide +
wedge-prone); the apple run for this file is entrusted to that runner.
"""

import base64

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.tui,
]

# A real 1x1 PNG so `gdk::Texture::from_bytes` / `NSImage`/`UIImage` actually
# decode it. The byte correctness of the round-trip is covered by the shared-Rust
# integration test (`injected_attachment_is_renderable_via_attachment_bytes`);
# here we just need a decodable image so the real-render path runs.
_PNG_1x1_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAf"
    "FcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAA"
    "AABJRU5ErkJggg=="
)
_TXT_B64 = base64.b64encode(b"hello notes\n").decode()


@pytest.mark.feature("conversation-attachments")
def test_inbound_image_and_file_attachments_render(logged_in_app):
    """An inbound message carrying an image + a non-image attachment renders a
    real ``dm-attachment-image`` and a ``dm-attachment-file`` (with its filename)
    in the bubble — proving the bubble reads the structured ``attachments`` field
    and resolves each ``blob_hash`` to bytes via the shared loader."""
    conv = logged_in_app.conversations
    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="bob-attach@self-nest.test",
        body="see attached",
        attachments=[
            {
                "filename": "pic.png",
                "mime_type": "image/png",
                "data_base64": _PNG_1x1_B64,
            },
            {
                "filename": "notes.txt",
                "mime_type": "text/plain",
                "data_base64": _TXT_B64,
            },
        ],
    )

    assert logged_in_app.driver.is_visible("thread-header"), "detail empty on select"
    assert (
        logged_in_app.driver.count("dm-attachment-image") >= 1
    ), "image attachment did not render"
    assert (
        logged_in_app.driver.count("dm-attachment-file") >= 1
    ), "file attachment did not render"
    file_text = logged_in_app.driver.get_text("dm-attachment-file", index=0)
    assert "notes.txt" in file_text, file_text

"""Conversation attachments: a received picture's content credentials show as a
provenance badge in the bubble.

The receiver's shared Rust probes each attachment's bytes
(``fauna_media::process::detect_c2pa``) and carries the verdict on the
attachment itself (``AttachmentSnapshot.c2pa`` / ``RenderBlock::Attachment.c2pa``);
the bubble paints ``c2pa-badge`` off that per-attachment field
(``docs/goal/ui/conversations.md`` § Layout & flow; § Attachments "C2PA
on-device"). The inject seam runs the same probe
(``ConversationsManager::make_attachment_for_test``), so an injected signed
picture carries the real verdict without a live backend round-trip.

The feed twin is ``test_feed.py::test_post_image_shows_c2pa_badge_when_uploaded_with_provenance``.

Apps: tui (the lead), linux, macos, ios, windows and android. Web's bubble
detection is a genuine stub (``conversations.md`` § Attachments "C2PA
on-device").
"""

import base64
from pathlib import Path

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]

# Lives in the repo-root `tests/fixtures/`, as the feed twin's does.
_C2PA_SIGNED_PNG = Path(__file__).resolve().parents[2] / "fixtures" / "c2pa-signed.png"

# A real 1x1 PNG with no C2PA manifest — the non-vacuity arm.
_PNG_1x1_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAf"
    "FcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
)


@pytest.mark.feature("conversation-attachments")
def test_a_received_signed_picture_shows_a_provenance_badge(logged_in_app):
    """An unsigned received picture shows no ``c2pa-badge``; a C2PA-signed one
    shows exactly one, beside its ``dm-attachment-image``."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="bob-c2pa-plain@self-nest.test",
        body="an unsigned picture",
        attachments=[
            {"filename": "plain.png", "mime_type": "image/png", "data_base64": _PNG_1x1_B64},
        ],
    )
    assert d.count("dm-attachment-image") >= 1, (
        f"the unsigned picture did not render: error={logged_in_app.error_text()!r}"
    )
    assert d.count("c2pa-badge") == 0, (
        "an unsigned picture must NOT show a c2pa-badge — the badge would be "
        "vacuously true otherwise"
    )

    signed_b64 = base64.b64encode(_C2PA_SIGNED_PNG.read_bytes()).decode()
    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="bob-c2pa-signed@self-nest.test",
        body="a signed picture",
        attachments=[
            {"filename": "signed.png", "mime_type": "image/png", "data_base64": signed_b64},
        ],
    )
    assert d.count("dm-attachment-image") >= 1, (
        f"the signed picture did not render: error={logged_in_app.error_text()!r}"
    )
    assert d.count("c2pa-badge") == 1, (
        f"a C2PA-signed received picture must show exactly one c2pa-badge; "
        f"got {d.count('c2pa-badge')}, error={logged_in_app.error_text()!r}"
    )

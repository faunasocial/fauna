"""tier_3: a conversation attachment survives this device dropping its cached copy — it
is fetched again where it can be, and where it cannot it still shows its name and size.

Goal doc: ``docs/goal/ui/conversations.md`` § Attachments → *Retention*: the device holds
a bounded cache ("The store holds at most ``ATTACHMENT_STORE_BUDGET_BYTES`` (128 MiB)"),
"An evicted attachment is fetched again where it can be" (on the FaunaMls rail "the send
remembers the same for the sender's own attachments"), and "A handle whose bytes are gone
for good … renders **declared** from then on: filename and size, no bytes".

No e2e fills 128 MiB of store, so the eviction is the shared test seam
``evict_thread_attachments_for_test`` (``conversations_evict_attachment``), which drops the
named attachments' bytes exactly as the budget's eviction drops one entry — bytes gone,
remembered location kept — and refuses when nothing was resident, so neither witness can
pass over bytes that never left. Everything after the eviction is production: the render's
miss, the receive loop's refill over the real wire, the paint.

tui first (``docs/goal/architecture/testing.md`` § Default app and nest mode). The
restored-device half of the same outcome is
``test_fauna_mls_cross_device_sync.py::test_an_attachment_restores_onto_a_second_device``.
"""

import base64
import re
import secrets
from pathlib import Path

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, UI_SETTLE_S
from helpers.png_chunks import png_with_chunk
from helpers.waiting import wait_until
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]

# A real photo-sized PNG: large enough that every app paints it as a picture (a 1x1
# image rasterizes to no half-block cell on tui, so it never reads as painted).
FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"
_NOTES = b"hello notes\n"
# The shared `byte_size` shape: "<n> B", "<n.n> KB", … (value-formatting.md § Byte sizes).
_SIZE = re.compile(r"\b\d+(?:\.\d)? (?:B|KB|MB|GB)\b")


# The launch-time real-wire gate apple, windows and android need (`real_faunamls_app`);
# run this test in its own invocation there, since the gate is session-wide.
@pytest.mark.real_conversations
@pytest.mark.feature("conversation-attachments")
def test_an_attachment_this_device_dropped_is_fetched_again(real_faunamls_app, nest_instance, test_user):
    """A picture you sent, whose bytes this device then drops, paints again: the render
    misses, the receive loop fetches the sealed blob from the room's home nest with the
    location the send remembered, opens it, and the picture comes back."""
    app = real_faunamls_app
    conv = app.conversations
    driver = app.driver
    bob = conv_api.reachable_peer(
        nest_instance["port"], nest_instance["admin"]["signing_key"], test_user["actor_id_hex"]
    )
    opener = f"retention opener {secrets.token_hex(3)}"
    conv.real_resolve_send_new(bob["actor_id_hex"], opener)
    thread = wait_until(
        lambda: next(
            (t for t in conv.list_threads() if t.rail == "FaunaMls" and opener in (t.snippet or "")),
            None,
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"threads={[(t.rail, t.snippet) for t in conv.list_threads()]}",
    )
    conv.open_thread_by_id(thread.thread_id)
    driver.clear_and_type("dm-text-field", "a picture to drop")
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    assert not app.has_error(), f"staging the attachment failed: {app.error_text()}"
    driver.click("dm-send-button")
    wait_until(
        lambda: conv.attachment_image_states() == ["painted"],
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the sent picture never painted in the echo: {conv.attachment_image_states()}",
    )

    conv.evict_attachment(thread.thread_id, FIXTURE_IMAGE.name)

    wait_until(
        lambda: conv.attachment_image_states() == ["painted"],
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the dropped picture was never fetched again: "
        f"{conv.attachment_image_states()} {driver.diagnose('dm-attachment-image')} "
        f"error={app.error_text()!r}",
    )


@pytest.mark.feature("conversation-attachments")
def test_a_file_this_device_cannot_fetch_still_shows_its_name_and_size(logged_in_app):
    """A picture and a file whose bytes this device no longer holds, and has nowhere to
    fetch from, still show by name and size — never a blank row, never a vanished file.

    The attachments arrive through the inject seam, which records no location for them,
    so once their bytes are dropped they are exactly the goal's "gone for good" handle:
    nothing can refill them. The picture is first seen painted, so the placeholder is
    what the SAME attachment turns into, not what an undecodable one always looked like.

    Both files carry bytes unique to this run. The store is content-addressed and
    shared across threads, so the fixture picture as-is shares its handle with any
    earlier REAL send of it in the session — whose remembered location refills it,
    correctly, and the placeholder never shows (measured: the re-fetch witness above
    sends that very file).
    """
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    token = secrets.token_hex(4)
    picture = png_with_chunk(FIXTURE_IMAGE.read_bytes(), b"tEXt", b"run\x00" + token.encode())
    notes = _NOTES + token.encode()
    thread_id = conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="petra-retention@self-nest.test",
        body=f"two files you will lose {token}",
        attachments=[
            {"filename": "pic.png", "mime_type": "image/png", "data_base64": base64.b64encode(picture).decode()},
            {"filename": "notes.txt", "mime_type": "text/plain", "data_base64": base64.b64encode(notes).decode()},
        ],
    )
    wait_until(
        lambda: conv.attachment_image_states() == ["painted"],
        UI_SETTLE_S,
        diagnose=lambda: f"the picture should paint while its bytes are held: {conv.attachment_image_states()}",
    )

    conv.evict_attachment(thread_id, "pic.png")
    conv.evict_attachment(thread_id, "notes.txt")

    wait_until(
        lambda: conv.attachment_image_states() == ["placeholder"],
        UI_SETTLE_S,
        diagnose=lambda: f"a picture without its bytes shows its placeholder: "
        f"{conv.attachment_image_states()} image={driver.get_text('dm-attachment-image', index=0)[:80]!r} "
        f"file={driver.get_text('dm-attachment-file', index=0)!r} "
        f"messages={driver.get_state('messages')!r}",
    )
    picture = driver.get_text("dm-attachment-image", index=0)
    assert "pic.png" in picture and _SIZE.search(picture), (
        f"a picture this device cannot fetch must still show its name and size: {picture!r}"
    )
    file_text = driver.get_text("dm-attachment-file", index=0)
    assert "notes.txt" in file_text and re.search(rf"\b{len(notes)} B\b", file_text), (
        f"a file this device cannot fetch must still show its name and size "
        f"({len(notes)} B): {file_text!r}"
    )

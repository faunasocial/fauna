"""tier_3: files on the MAIL rail, driven through the app — a file sent to an outside
address arrives as an ordinary attachment, a file on mail from outside shows in the
bubble, and camera/location details are gone from a picture before it leaves the device.

Goal doc: ``docs/goal/ui/conversations.md`` § Attachments — *Built (SMTP rail)*: outbound,
``rfc5322::build_message`` "emits ``multipart/mixed`` with one base64
``Content-Disposition: attachment`` part per attachment (the standard email shape a
non-Fauna MUA renders)"; inbound, ``extract_attachments`` walks the MIME parts and the
bubble renders them; and *Privacy metadata*: ``stage_attachment`` — the one function every
staged attachment on every app funnels through — runs ``strip_metadata`` before hashing.

Every app-driving attachment journey before these rode the encrypted rail. Here the send is
the app's own compose bar (``attachment-button`` → ``dm-send-button``) relayed by the real
MTA to the in-process stub external MX, whose captured bytes are parsed as MIME; the
receive is a real multipart/mixed message delivered over the MTA's port-25 listener. Both
ride the session's ``mail_bridge_mta``, so neither needs Docker or a DNS lookup.

The strip is witnessed on the relayed bytes because they are the one place a mailed
file's content is readable by a third party: the stub MX sits exactly where the outside
recipient does. As in the feed twin
(``test_feed.py::test_post_image_strips_exif_gps_before_the_nest_stores_it``), the nest
cannot strip — it never sees the plaintext — so a clean attachment there can only have
been cleaned on the device under test.

tui first (``docs/goal/architecture/testing.md`` § Default app and nest mode); the other
six columns join through the cross-app lift row.
"""

import email
import email.policy
import secrets
import time
from email.message import EmailMessage
from pathlib import Path

import pytest

from helpers.budgets import MAIL_OUTBOUND_CYCLE_S, MLS_HANDSHAKE_S, UI_SETTLE_S
from helpers.mail_client_ui import deliver_inbound, route_inbound_mail_to_app
from helpers.png_chunks import (
    EXIF_GPS_CANARY,
    METADATA_CHUNK_TYPES,
    PNG_MAGIC,
    ihdr,
    png_chunks,
    png_with_exif_chunk,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

# The relayed-to domain; the `mail_bridge_mta` fixture routes it to the in-process
# stub MX via the `mta_mx_override` hatch.
EXTERNAL_DOMAIN = "external.test"
TEST_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _relayed(app, mail_bridge_mta, token: str) -> email.message.EmailMessage:
    """The relayed message tagged ``token``, parsed — waited for across one outbound
    drain cycle (the `outbound_ready` push makes it prompt; the budget covers the
    MTA's own MX attempt ceiling)."""

    def _find():
        return next((raw for raw in mail_bridge_mta.stub_mx.messages() if token.encode() in raw), None)

    raw = wait_until(
        _find,
        MAIL_OUTBOUND_CYCLE_S,
        diagnose=lambda: f"the stub external MX received no message tagged {token!r}; "
        f"conversations error: {app.error_text()!r}; bridge log: {mail_bridge_mta.log_file}",
    )
    return email.message_from_bytes(raw, policy=email.policy.default)


def _attachment_parts(msg) -> list:
    return [part for part in msg.walk() if part.get_content_disposition() == "attachment"]


@pytest.mark.feature("conversation-attachments")
def test_a_file_sent_to_an_outside_address_arrives_as_an_ordinary_attachment(
    logged_in_app, mail_bridge_mta, tmp_path
):
    """The outside recipient's server receives a ``multipart/mixed`` mail whose file is
    an ordinary ``Content-Disposition: attachment`` part carrying the file's own name and
    exactly its bytes — what any mail program shows as an attachment."""
    token = f"mailfile{secrets.token_hex(4)}"
    notes = tmp_path / "meeting-notes.txt"
    notes_bytes = f"agenda for {token}\nitem one\nitem two\n".encode()
    notes.write_bytes(notes_bytes)

    logged_in_app.conversations.start_new_conversation(
        f"recipient@{EXTERNAL_DOMAIN}",
        subject=f"Notes {token}",
        body="the notes are attached",
        files=[notes],
    )
    msg = _relayed(logged_in_app, mail_bridge_mta, token)

    assert msg.get_content_type() == "multipart/mixed", (
        f"a mail carrying a file must be multipart/mixed, got {msg.get_content_type()!r}"
    )
    parts = _attachment_parts(msg)
    assert [p.get_filename() for p in parts] == ["meeting-notes.txt"], (
        f"exactly the attached file, as an attachment part under its own name: "
        f"{[(p.get_content_type(), p.get_filename()) for p in msg.walk()]}"
    )
    assert parts[0].get_payload(decode=True) == notes_bytes, (
        "the attachment part must carry exactly the file's bytes"
    )
    assert "the notes are attached" in msg.get_body(preferencelist=("plain", "html")).get_content(), (
        "the message body still travels beside the attachment"
    )


@pytest.mark.feature("conversation-attachments")
def test_camera_and_location_details_are_removed_from_a_file_before_it_is_sent(
    logged_in_app, mail_bridge_mta, tmp_path
):
    """A picture carrying a GPS canary in a real ``eXIf`` chunk reaches the outside
    recipient with no metadata chunk and no trace of the canary — and is still the same
    picture (same IHDR geometry, smaller only by what was removed)."""
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    tagged = png_with_exif_chunk(TEST_IMAGE.read_bytes(), EXIF_GPS_CANARY)
    assert EXIF_GPS_CANARY in tagged and any(t == b"eXIf" for t, _ in png_chunks(tagged)), (
        "the fixture must carry a real eXIf chunk holding the canary, else the strip "
        "assertions are vacuous"
    )
    photo = tmp_path / "holiday.png"
    photo.write_bytes(tagged)
    token = f"mailgps{secrets.token_hex(4)}"

    logged_in_app.conversations.start_new_conversation(
        f"recipient@{EXTERNAL_DOMAIN}",
        subject=f"Photo {token}",
        body="a photo from the trip",
        files=[photo],
    )
    parts = _attachment_parts(_relayed(logged_in_app, mail_bridge_mta, token))
    assert [p.get_filename() for p in parts] == ["holiday.png"], (
        f"the picture must travel as one attachment part: {[p.get_filename() for p in parts]}"
    )
    sent = parts[0].get_payload(decode=True)
    assert sent[:8] == PNG_MAGIC, f"the attachment is not the PNG that was attached: {sent[:8]!r}"
    leaked = [t.decode("ascii", "replace") for t, _ in png_chunks(sent) if t in METADATA_CHUNK_TYPES]
    assert not leaked, (
        f"the mailed picture still carries metadata chunks {leaked} — the staging strip "
        f"(`ConversationsManager::stage_attachment` → `strip_metadata`) did not run"
    )
    assert EXIF_GPS_CANARY not in sent, (
        "the picture's location left the device inside a mail to an outside address"
    )
    # Last, so a real strip regression prints the strip message above: these pin that
    # the bytes read are the attached picture (not a re-encode or a thumbnail) and that
    # something was actually removed on the way.
    assert ihdr(sent) == ihdr(tagged), "the mailed attachment is a different image"
    assert len(sent) < len(tagged), "nothing was removed from the picture on the way out"


@pytest.mark.feature("conversation-attachments")
def test_a_file_on_mail_from_outside_shows_in_the_bubble(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """A real ``multipart/mixed`` mail from an outside sender, carrying a picture and a
    text file, shows both in its bubble: the picture painted from its bytes (not the
    name-and-size placeholder a picture without them shows) and the file by name."""
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    app = logged_in_app
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "e2efiles"
    )
    nonce = f"mailin{secrets.token_hex(4)}"
    msg = EmailMessage()
    msg["From"] = f"Outside Sender <sender@{EXTERNAL_DOMAIN}>"
    msg["To"] = recipient
    msg["Subject"] = f"Files {nonce}"
    msg["Message-ID"] = f"<{nonce}@{EXTERNAL_DOMAIN}>"
    msg["Date"] = "Mon, 21 Sep 2026 09:00:00 +0000"
    msg.set_content(f"The {nonce} files are attached.")
    msg.add_attachment(TEST_IMAGE.read_bytes(), maintype="image", subtype="png", filename="view.png")
    msg.add_attachment(b"hello notes\n", maintype="text", subtype="plain", filename="notes.txt")
    deliver_inbound(
        mail_bridge_mta.mx_port,
        mail_bridge_mta.domain,
        f"sender@{EXTERNAL_DOMAIN}",
        recipient,
        msg.as_bytes(policy=email.policy.SMTP),
        time.monotonic() + 40.0,
    )

    conv = app.conversations
    thread = wait_until(
        lambda: next(
            (t for t in conv.list_threads() if nonce in (t.label or "") or nonce in (t.snippet or "")),
            None,
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the inbound mail {nonce!r} never surfaced; threads: "
        f"{[(t.label, t.rail) for t in conv.list_threads()]}; error: {app.error_text()!r}; "
        f"bridge log: {mail_bridge_mta.log_file}",
    )
    assert thread.rail == "Smtp", f"mail from outside lands on the mail rail, got {thread.rail!r}"
    conv.open_thread_by_id(thread.thread_id)

    driver = app.driver
    wait_until(
        lambda: conv.attachment_image_states() == ["painted"],
        UI_SETTLE_S,
        diagnose=lambda: f"the mailed picture should paint from its bytes: "
        f"{conv.attachment_image_states()} {driver.diagnose('dm-attachment-image')}",
    )
    assert driver.count("dm-attachment-file") == 1 and "notes.txt" in driver.get_text(
        "dm-attachment-file", index=0
    ), f"the mailed text file should show by name: {driver.diagnose('dm-attachment-file')}"

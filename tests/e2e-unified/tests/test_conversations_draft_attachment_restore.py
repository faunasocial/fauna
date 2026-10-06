"""tier_3: a file attached to a half-written message comes back by name after an
app restart, and sending it refuses by name rather than going without it —
``docs/goal/ui/conversations.md`` § Persistence, *A restored draft's attachment
is a handle, not a file*.

The conversations twin of ``test_feed_draft_attachment_restore.py``. A message
draft rests its attachments by content address; their bytes live only in the
in-memory attachment store of the device that staged them (§ Attachments →
*Retention*). After a relaunch the file is therefore a name with nothing behind
it on this device, and three things must hold, each through the UI the author
looks at:

1. the composer still shows the attachment chip naming the file
   (``dm-compose-attachment-chip``);
2. sending refuses on ``error-message`` with ``error.send.attachment_missing``
   naming that file — every staged attachment resolves to bytes or the whole
   send fails closed, before anything reaches the backend — and nothing is
   sent: the message never goes without the file the author attached;
3. the draft is kept, so the author can drop the chip
   (``dm-compose-attachment-remove``) and attach the file again.

The refusal itself is shared Rust, pinned tier_1 by
``manager_integration_tests.rs::a_restored_drafts_attachment_either_reaches_the_backend_or_refuses_the_send``;
what only this test can see is the app carrying the handle through the rail and
painting the refusal the manager stamps.

**Written first, addressed after the restart.** The message is saved with no
recipient and addressed once it comes back, so the restored picker holds nothing
at ``idle`` and no restore-time probe fires (``conversations.md``
§ Persistence, *A restore that FILLS a recipient picker owes it a probe*,
ruled 2026-09-21) — the restart leg exercises the attachment and nothing else.
The recipient is a mail address at a domain no nest serves, which the picker
resolves as Email with no mail setup (the ``test_recipient_picker.py`` shape):
the refusal comes before the rail is ever asked to send, so which rail it is
does not matter, only that the send reaches the check. The address is unique per
run so the thread the send materializes is this test's alone.

**Ordering is what makes the sealed draft blob assertable.** The blob is opaque
to the test, so "a save landed" is observed as "the blob changed", and each wait
follows exactly one change to the draft (the text, then the attach). The restart
is the DEFAULT fresh-store relaunch, never ``preserve_state_across_relaunch()``:
the handle has to come back from the nest's ``__drafts`` plane, and a surviving
local store would hold the bytes and hide exactly the case under test.

**A dedicated account**, so no earlier test's resting draft is restored into
this composer and nothing this test leaves behind reaches a later one.

Latency-independent throughout (e2e convention 14): every wait polls the
observable that IS the contract — the nest's draft blob, the restored text, the
chip, the error — and its budget is a ceiling a green run never pays.
"""

import time
import uuid
from pathlib import Path

import pytest

from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; the other apps join through the drafts-survive cross-app lift.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
    # tui runs the real ConversationsSession for every e2e login regardless;
    # marked so the flag's session-wide env stays consistent when the suite
    # runs cross-app.
    pytest.mark.real_conversations,
    # The debounce itself must land this draft — see the marker's entry in
    # pytest.ini. Keeps a `drafts_autosave_window_ms` run from silently
    # recording this as a product red.
    pytest.mark.drafts_production_window,
]

#: The draft rail this test drives (``fauna_protocol::drafts::DRAFT_RAILS``).
RAIL = "conversations"

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _poll(read, done, timeout: float = 15.0):
    """Read until ``done(value)`` or the budget runs out; returns the last value
    read, for the failure message."""
    deadline = time.monotonic() + timeout
    value = read()
    while not done(value) and time.monotonic() < deadline:
        time.sleep(0.2)
        value = read()
    return value


def _chip_text(driver) -> str:
    if driver.count("dm-compose-attachment-chip") == 0:
        return ""
    return driver.get_text("dm-compose-attachment-chip")


def _draft_blob(node_url, actor_id, signing_key):
    """The rail's sealed blob as the nest holds it, read by a fresh side-channel
    device for the same actor."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        return dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")


def _wait_draft_saved(node_url, actor_id, signing_key, previous, timeout: float = 20.0):
    """Poll until the debounced ``fauna.drafts.put`` lands a blob other than
    ``previous``; ``None`` on timeout."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        blob = _draft_blob(node_url, actor_id, signing_key)
        if blob is not None and blob != previous:
            return blob
        time.sleep(0.5)
    return None


def _open_new_conversation(app) -> None:
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)


@pytest.mark.feature("drafts-survive")
def test_restored_message_draft_attachment_is_named_and_its_send_refuses(
    app, request, nest_instance
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    driver = app.driver
    conv = app.conversations
    node_url = nest_instance["url"]
    actor_id = user["actor_id_bytes"]
    signing_key = bytes(user["signing_key"])
    filename = FIXTURE_IMAGE.name
    tag = uuid.uuid4().hex[:8]
    body = f"a half-written message whose picture stays on the device it was picked on {tag}"
    recipient = f"draft-attach-{tag}@plain-email-host.test"

    baseline = _draft_blob(node_url, actor_id, signing_key)

    # 1. The text first, saved — so the next save can only be the attach's.
    _open_new_conversation(app)
    driver.type_text("dm-text-field", body)
    typed = _poll(conv.compose_body_text, lambda t: t == body)
    assert typed == body, f"precondition: the message must be in the composer, got {typed!r}"
    text_saved = _wait_draft_saved(node_url, actor_id, signing_key, baseline)
    assert text_saved is not None, (
        f"precondition: the typed draft must reach the nest __drafts plane at path={RAIL!r}"
    )

    # 2. Attach. The chip names the file at once, and the draft is saved again —
    #    carrying the file's content address, the only thing that can survive a
    #    relaunch.
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    chip = _poll(lambda: _chip_text(driver), lambda t: filename in t)
    assert filename in chip, (
        f"attaching {filename!r} must name it on dm-compose-attachment-chip, got {chip!r}; "
        f"error={app.error_text()!r}"
    )
    attach_saved = _wait_draft_saved(node_url, actor_id, signing_key, text_saved)
    assert attach_saved is not None, (
        "attaching must re-save the draft with the file's handle, or a relaunch "
        "loses the file with nothing left to refuse"
    )

    # 3. Force-quit + relaunch on a fresh store: only what reached the nest returns.
    driver.hard_reload()
    _open_new_conversation(app)
    restored = _poll(conv.compose_body_text, lambda t: t == body)
    assert restored == body, (
        f"the message draft did not survive the restart (expected {body!r}, got "
        f"{restored!r}); error={app.error_text()!r}"
    )

    # 4. The restored draft's file is named on its chip, though this device holds
    #    no bytes for it.
    chip = _poll(lambda: _chip_text(driver), lambda t: filename in t)
    assert filename in chip, (
        f"the restored draft's attachment must be named on dm-compose-attachment-chip "
        f"before any send, got {chip!r} — otherwise the refusal names a file the "
        f"composer does not show"
    )

    # 5. Address it and send: the send refuses by name, before anything is sent.
    conv.add_recipient(recipient)
    driver.click("dm-send-button")
    expected = S.conversations.unified.error_send(
        message=S.error.send.attachment_missing(filename=filename)
    )
    refusal = _poll(app.error_text, lambda t: t == expected)
    assert refusal == expected, (
        f"sending a restored draft whose file is not on this device must refuse on "
        f"error-message with {expected!r}, got {refusal!r}"
    )
    # The refusal is the send's terminal state — it returns before the backend is
    # asked — so there is no send in flight for a later bubble to come from.
    assert driver.count("dm-message-text") == 0, (
        "the message must never be sent without the file the author attached, but "
        f"the conversation shows a message: {driver.diagnose('dm-message-text')}"
    )

    # 6. The draft is kept: the text and the named file, so the author can drop
    #    the file and attach it again.
    assert conv.compose_body_text() == body, "the refused draft must keep its text"
    assert filename in _chip_text(driver), "the refused draft must keep its file's chip"
    driver.click("dm-compose-attachment-remove")
    still_there = _poll(lambda: driver.count("dm-compose-attachment-chip"), lambda n: n == 0)
    assert still_there == 0, "dm-compose-attachment-remove must drop the restored file's chip"
    assert conv.compose_body_text() == body, "removing the file must keep the text"

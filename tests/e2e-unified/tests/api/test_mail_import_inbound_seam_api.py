"""tier_3 — the nest's promises about an import that meet mail the nest delivered.

Four `[nest]` outcomes of `docs/features/mailbox-import.md` that need a
recipient whose mail can be both DELIVERED (a real Go MTA on port 25) and READ
BACK over IMAP (a real MDA), so the import is judged against mail that really
arrived the ordinary way and its own result is read the way the person reads it.
The shared ``mail_bridge_inbound_to_imap`` seam provides that recipient; the
import is driven with no app, through the production kinds, as the recipient
(its seal key is the MSEK-derived one the MDA opens, so the IMAP side can read
what the import sealed).

* **39** — a stranger reusing the Message-ID of mail you have not imported cannot make the real
  message be skipped (``mailbox-migration.md`` § The envelope key confirms a Message-ID hit).
* **44** — marks only the nest writes are removed from an imported message as it arrives
  (§ Nest-side sealing).
* **47** — imported mail is ordinary mail: it is there over IMAP (§ Architectural rules).
* **54** — mail delivered to you, or that you sent, is not copied a second time by an import
  (§ Dedup scope).
"""

from __future__ import annotations

import secrets
import time

import pytest

from tests.api.test_import_push_api import _user_client
from tests.api.test_mail_import_nest_api import DEDUP, IMPORTED, _import, _item, _session
from tests.test_mail_bridge_mua_submission import _imap_find, _mail_user, _message, _submit
from tests.test_mail_inbound_stamps import _deliver, _headers, _read_back

pytestmark = pytest.mark.tier_3

SENDER = "sender@external.test"


def _recipient_client(handle, nest_instance):
    """A User-class client for the seam's recipient — the owner of the mailbox read back over IMAP."""
    return _user_client(nest_instance["port"], handle.recipient.recipient)


def _import_once(c, item: dict) -> str:
    """Import ``item`` in a session of its own and finalize it: the seam's recipient is shared
    across tests and holds one running import per source."""
    sid = _session(c)
    try:
        return _import(c, sid, item)
    finally:
        c.call("fauna.bridges.finalize_import_session", {"session_id": sid})


def _plain(handle, nonce: str, *, message_id: str, body_extra: str = "", extra: tuple = ()) -> bytes:
    """A message to the seam's recipient with an explicit Message-ID and body."""
    lines = [
        *extra,
        f"From: External Sender <{SENDER}>",
        f"To: {handle.recipient_username}",
        f"Subject: import seam {nonce}",
        f"Message-ID: <{message_id}>",
        "Date: Thu, 08 Oct 2026 12:00:00 +0000",
        "",
        f"body {nonce}{body_extra}",
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


# ── 54: mail delivered to you is not copied a second time ────────────────────


@pytest.mark.feature("mailbox-import")
def test_mail_delivered_to_you_is_not_copied_a_second_time_by_an_import(
    mail_bridge_inbound_to_imap, nest_instance
):
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    deadline = time.monotonic() + 120.0
    nonce = f"deliveredthenimported{time.time_ns()}"
    mid = f"{nonce}@external.test"
    raw = _plain(handle, nonce, message_id=mid)
    _deliver(handle, raw, deadline)
    _read_back(handle, nonce, deadline)  # it really arrived, so the index has it

    with _recipient_client(handle, nest_instance) as c:
        answer = _import_once(c, _item(raw, mid))
    assert answer == DEDUP, (
        f"the message the nest delivered must not be imported again from the old provider: {answer!r}"
    )


@pytest.mark.feature("mailbox-import")
def test_mail_you_sent_is_not_copied_a_second_time_by_an_import(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary
):
    """The sent half of 54: a message submitted through the nest leaves a Sent copy, and an
    import of the same message answers ``skipped:dedup`` — sent in-domain, so nothing is relayed."""
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    sender = _mail_user(nest_instance, seal_helper_binary, handle.domain)
    rcpt = _mail_user(nest_instance, seal_helper_binary, handle.domain)
    token = f"sentthenimported{secrets.token_hex(4)}"
    _submit(handle, sender.username, sender.password, [rcpt.username], token)
    hits, _, _ = _imap_find(handle.mda, sender, "Sent", token)
    assert hits, "the message the person submitted must be in Sent, so the index has it"

    raw = _message(sender.username, [rcpt.username], token)
    with _user_client(nest_instance["port"], sender.recipient) as c:
        answer = _import_once(c, _item(raw, f"{token}@mua.test"))
    assert answer == DEDUP, (
        f"the message the person sent must not be imported again from the old provider: {answer!r}"
    )


# ── 39: a stranger cannot make the real message be skipped ───────────────────


@pytest.mark.feature("mailbox-import")
def test_a_stranger_reusing_a_message_id_cannot_make_the_real_message_be_skipped(
    mail_bridge_inbound_to_imap, nest_instance
):
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    deadline = time.monotonic() + 120.0
    nonce = f"planted{time.time_ns()}"
    mid = f"{nonce}@external.test"
    planted = _plain(handle, f"{nonce}-planted", message_id=mid)
    real = _plain(handle, f"{nonce}-real", message_id=mid)
    _deliver(handle, planted, deadline)
    _read_back(handle, f"{nonce}-planted", deadline)

    with _recipient_client(handle, nest_instance) as c:
        answer = _import_once(c, _item(real, mid))
    assert answer == IMPORTED, (
        "mail from a stranger reusing the Message-ID must not make the real, different "
        f"message be skipped: {answer!r}"
    )


# ── 44 + 47: an imported message arrives as ordinary mail, nest marks only ───


@pytest.mark.feature("mailbox-import")
def test_imported_mail_is_there_over_imap_carrying_only_marks_the_nest_wrote(
    mail_bridge_inbound_to_imap, nest_instance
):
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    deadline = time.monotonic() + 120.0
    nonce = f"importedmarks{time.time_ns()}"
    mid = f"{nonce}@example.org"
    forged = ("spam-forged", "forged-alias@example.invalid")
    raw = _plain(
        handle,
        nonce,
        message_id=mid,
        extra=(
            f"X-Fauna-Spam-Verdict: {forged[0]}",
            f"X-Fauna-Address-Matched: {forged[1]}",
        ),
    )

    with _recipient_client(handle, nest_instance) as c:
        assert _import_once(c, _item(raw, mid)) == IMPORTED

    copy = _read_back(handle, nonce, deadline)  # 47: the person's mail app sees it
    headers = _headers(copy)
    assert not any(v in forged for _, v in headers), (
        f"a mark the nest never made must not survive the import; got {headers!r}"
    )
    assert not any(
        n.lower() in ("x-fauna-spam-verdict", "x-fauna-address-matched") for n, _ in headers
    ), f"the forged spam / alias marks must be removed on arrival; got {headers!r}"

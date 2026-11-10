"""tier_3 API witness for `mail-abuse-refused` outcome 28: every message the
perimeter accepts is scanned whatever size limit the admin set, and mail that
never passed the scanner (an app/MUA submission, the sender's own Sent copy)
carries no verdict and is never shown as clean
(`mail-content-scanning.md` § Oversize messages, § Per-message scan-result
storage; the `NotScanned` arm, built 2026-09-28).

Observed from outside over the real SMTP doors and the real WS-RPC `User` wire,
through `fauna.bridges.get_message_scan_result`:

  * An admin sets `max_message_bytes` to a small limit; a message the
    perimeter accepts under it reads the fake clamd's `clean` verdict -- not
    `bypassed_oversize`: the scan cap is the live ceiling, never a smaller
    number.
  * An admin RAISES `max_message_bytes` above the value the bridge booted
    with; with no bridge restart, a declared `MAIL FROM ... SIZE=` between the
    two is accepted and a message between the two delivers and is scanned --
    go-smtp's own size field holds the product ceiling, so the live cap alone
    decides (`mail-message-size.md`, the perimeter + EHLO bullet).
  * A message the user's own submission door took reads back to both the
    colleague (the twin in INBOX) and the sender (the Sent copy) as
    `fauna.bridges.not_found` -- no row, so no verdict and in particular not a
    `clean` nobody computed.

Taxonomy: tier_3 -- real `fauna-nest`, real MTA/submission bridge, fake clamd +
rspamd, real WS-RPC User wire.
"""

import time

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from helpers.waiting import wait_until
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _read_final_reply,
    _smtp_auth_plain,
)

pytestmark = pytest.mark.tier_3

SENDER_DOMAIN = "external.test"
NOT_FOUND = "fauna.bridges.not_found"
ADMIN_LIMIT = 100_000
# The bridge boots on the shipped default cap (the fixture posture leaves
# `max_message_bytes` unset): 50 MB. The raised cap sits above it, the message
# between the two, and both well under the 250 MB product ceiling.
BOOT_LIMIT = 50_000_000
RAISED_LIMIT = 60_000_000
BETWEEN = 51_000_000


def _user_client(nest_instance, actor_id: bytes, signing_key: bytes) -> WsRpcAdminClient:
    return WsRpcAdminClient(nest_instance["url"], actor_id=actor_id, signing_key=signing_key)


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _last_uid(client, method: str) -> int:
    msgs = client.call(method, {"after_uid": 0, "limit": 500}).get("messages", [])
    return max((m["uid"] for m in msgs), default=0)


def _new_message_id(client, method: str, after_uid: int) -> bytes:
    """The one message that appeared in the mailbox after `after_uid`."""
    msgs = client.call(method, {"after_uid": after_uid, "limit": 50}).get("messages", [])
    assert len(msgs) == 1, (
        f"{method}: exactly the one new message expected after uid {after_uid}; "
        f"got {[m.get('uid') for m in msgs]}"
    )
    return bytes(msgs[0]["message_id"])


def _send_inbound(handle, recipient_addr: str, raw: bytes, deadline: float) -> str:
    """One inbound SMTP delivery; returns the reply to the end-of-DATA dot."""
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)
        try:
            conn.cmd("QUIT", "221", deadline)
        except Exception:
            pass
        return reply


def _mail_from_reply(handle, declared_size: int, deadline: float) -> str:
    """The reply to one `MAIL FROM ... SIZE=<declared_size>`, whatever its code."""
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.send_raw(f"MAIL FROM:<sender@{SENDER_DOMAIN}> SIZE={declared_size}\r\n".encode())
        reply = _read_final_reply(conn, deadline)
        try:
            conn.cmd("QUIT", "221", deadline)
        except Exception:
            pass
        return reply


def _message(recipient_addr: str, nonce: str, total_bytes: int) -> bytes:
    header = (
        f"From: External Sender <sender@{SENDER_DOMAIN}>\r\n"
        f"To: {recipient_addr}\r\n"
        f"Subject: sized {nonce}\r\n"
        f"Message-ID: <{nonce}@{SENDER_DOMAIN}>\r\n"
        "Date: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "\r\n"
    ).encode()
    line = b"A" * 76 + b"\r\n"
    return header + line * ((total_bytes - len(header)) // len(line))


@pytest.mark.feature("mail-abuse-refused")
def test_a_message_accepted_under_an_admin_set_size_limit_is_still_scanned(
    nest_instance, mail_bridge_mta
):
    """The perimeter's live size limit is the scan cap, so whatever limit the
    admin sets, a message it accepts reads a real clamd verdict -- never
    `bypassed_oversize`. The admin lowers the limit to 100 000 bytes; once the
    perimeter enforces it (polled: an over-limit probe is refused 552), a
    message just under it is accepted and its recipient reads `clean`."""
    from conftest import MTA_FIXTURE_SPAM_POSTURE

    handle = mail_bridge_mta
    recipient = handle.recipient_actor
    recipient_addr = f"{handle.recipient_local_part}@{handle.domain}"
    nonce = f"sizescan{int(time.time() * 1000)}"
    admin = _admin_client(nest_instance)
    recipient_client = _user_client(
        nest_instance, bytes(recipient["actor_id_bytes"]), bytes(recipient["signing_key"])
    )
    with admin, recipient_client:
        admin.call(
            "fauna.bridges.put_spam_policy",
            {**MTA_FIXTURE_SPAM_POSTURE, "max_message_bytes": ADMIN_LIMIT},
        )
        try:
            def _limit_enforced() -> bool:
                probe = _message(recipient_addr, nonce + "probe", ADMIN_LIMIT + 50_000)
                return _send_inbound(handle, recipient_addr, probe,
                                     time.monotonic() + 30.0).startswith("552")

            wait_until(_limit_enforced, 60.0, interval=1.0,
                       diagnose=lambda: "the perimeter never started refusing over-limit mail")
            before = _last_uid(recipient_client, "fauna.email.inbox.fetch")
            reply = _send_inbound(handle, recipient_addr,
                                  _message(recipient_addr, nonce, ADMIN_LIMIT - 10_000),
                                  time.monotonic() + 30.0)
            assert reply.startswith("250"), f"a message under the limit is accepted: {reply}"
        finally:
            admin.call("fauna.bridges.put_spam_policy", dict(MTA_FIXTURE_SPAM_POSTURE))

        message_id = _new_message_id(recipient_client, "fauna.email.inbox.fetch", before)
        row = recipient_client.call(
            "fauna.bridges.get_message_scan_result", {"message_id": message_id}
        )
        assert row["clamav_verdict"] == {"kind": "clean"}, (
            f"an accepted message is scanned whatever limit the admin set "
            f"(never bypassed_oversize): {row}"
        )
        assert row["action_taken"] == "delivered", row


@pytest.mark.feature("mail-abuse-refused")
def test_a_size_limit_raised_above_the_boot_value_applies_without_a_restart(
    nest_instance, mail_bridge_mta
):
    """An admin raises the size limit above the one the bridge started with;
    with no bridge restart a sender declaring a size between the two is
    accepted at MAIL FROM, and a message between the two is delivered and
    scanned. Before 2026-11-07 go-smtp kept the boot value as its own limit and
    refused both `552` until the bridge restarted."""
    from conftest import MTA_FIXTURE_SPAM_POSTURE

    handle = mail_bridge_mta
    recipient = handle.recipient_actor
    recipient_addr = f"{handle.recipient_local_part}@{handle.domain}"
    nonce = f"sizeraise{int(time.time() * 1000)}"
    admin = _admin_client(nest_instance)
    recipient_client = _user_client(
        nest_instance, bytes(recipient["actor_id_bytes"]), bytes(recipient["signing_key"])
    )
    with admin, recipient_client:
        over_raised = _mail_from_reply(handle, RAISED_LIMIT + 1, time.monotonic() + 30.0)
        assert over_raised.startswith("552") and "5.3.4" in over_raised, (
            f"a declared size over the live limit is refused before any transfer: {over_raised}"
        )
        admin.call(
            "fauna.bridges.put_spam_policy",
            {**MTA_FIXTURE_SPAM_POSTURE, "max_message_bytes": RAISED_LIMIT},
        )
        try:
            last = {"reply": ""}

            def _raise_applied() -> bool:
                last["reply"] = _mail_from_reply(handle, BETWEEN, time.monotonic() + 30.0)
                return last["reply"].startswith("250")

            wait_until(
                _raise_applied, 60.0, interval=1.0,
                diagnose=lambda: (
                    f"a declared size of {BETWEEN} (above the {BOOT_LIMIT} boot limit, under "
                    f"the raised {RAISED_LIMIT}) was never accepted without a restart; "
                    f"last MAIL FROM reply: {last['reply']!r}"
                ),
            )
            before = _last_uid(recipient_client, "fauna.email.inbox.fetch")
            reply = _send_inbound(handle, recipient_addr,
                                  _message(recipient_addr, nonce, BETWEEN),
                                  time.monotonic() + 120.0)
            assert reply.startswith("250"), (
                f"a {BETWEEN}-byte message under the raised limit is accepted without a "
                f"bridge restart: {reply}"
            )
        finally:
            admin.call("fauna.bridges.put_spam_policy", dict(MTA_FIXTURE_SPAM_POSTURE))

        def _arrived() -> bool:
            msgs = recipient_client.call(
                "fauna.email.inbox.fetch", {"after_uid": before, "limit": 50}
            ).get("messages", [])
            return len(msgs) == 1

        wait_until(_arrived, 60.0, interval=1.0,
                   diagnose=lambda: "the accepted message never reached the recipient's inbox")
        message_id = _new_message_id(recipient_client, "fauna.email.inbox.fetch", before)
        row = recipient_client.call(
            "fauna.bridges.get_message_scan_result", {"message_id": message_id}
        )
        assert row["clamav_verdict"] == {"kind": "clean"}, (
            f"the message under the raised limit is scanned like any other: {row}"
        )


@pytest.mark.feature("mail-abuse-refused")
def test_a_submitted_message_and_the_senders_sent_copy_carry_no_verdict(
    nest_instance, mail_bridge_mta
):
    handle = mail_bridge_mta
    recipient = handle.recipient_actor
    sender_actor = handle.submission_sender_actor
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    nonce = f"subscan{int(time.time() * 1000)}"
    message = (
        f"From: Sender <{sender}>\r\n"
        f"To: {recipient_addr}\r\n"
        f"Subject: submitted {nonce}\r\n"
        f"Message-ID: <{nonce}@{domain}>\r\n"
        "Date: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "\r\n"
        f"A benign submitted body {nonce}.\r\n"
    ).encode()

    recipient_client = _user_client(
        nest_instance, bytes(recipient["actor_id_bytes"]), bytes(recipient["signing_key"])
    )
    sender_client = _user_client(
        nest_instance, bytes(sender_actor["actor_id_bytes"]), bytes(sender_actor["signing_key"])
    )
    with recipient_client, sender_client:
        inbox_before = _last_uid(recipient_client, "fauna.email.inbox.fetch")
        sent_before = _last_uid(sender_client, "fauna.email.sent.fetch")

        deadline = time.monotonic() + 40.0
        with _connect_submission_tls(handle.submission_port_465, domain) as conn:
            conn.expect("220", deadline)
            conn.cmd(f"EHLO {domain}", "250", deadline)
            _smtp_auth_plain(conn, sender, handle.submission_credential.decode(), deadline)
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(message)
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        twin_id = _new_message_id(recipient_client, "fauna.email.inbox.fetch", inbox_before)
        sent_id = _new_message_id(sender_client, "fauna.email.sent.fetch", sent_before)

        for client, mid, what in (
            (recipient_client, twin_id, "the colleague's twin"),
            (sender_client, sent_id, "the sender's own Sent copy"),
        ):
            with pytest.raises(RpcCallError) as e:
                client.call("fauna.bridges.get_message_scan_result", {"message_id": mid})
            assert e.value.code == NOT_FOUND, (
                f"{what} never passed the scanner, so it has no scan row (and in "
                f"particular no 'clean' verdict): {e.value}"
            )

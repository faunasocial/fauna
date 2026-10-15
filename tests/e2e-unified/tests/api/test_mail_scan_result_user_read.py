"""tier_3 API-helper: the user's own read of a received message's scan verdict
(`mail-content-scanning.md` § User sees their own message's row) over the real
WS-RPC `User` wire — the nest-side half of `email-in-conversations` outcome 17,
so a failure there is triaged UI-side vs nest-side (E2E § rule 5).

  * `fauna.bridges.get_message_scan_result` returns the recipient's own
    message's row, keyed by the `message_id` `fauna.email.inbox.fetch` hands
    the app: the fake clamd's `clean` verdict, the fake rspamd's score, and
    `delivered`.
  * A second user asking for the same id gets `fauna.bridges.not_found` — the
    same answer as an id that names nothing, so the reply never reveals that
    someone else's message exists.

The session MTA runs the fake clamd + rspamd (conftest's `mail_bridge_mta`), so a
benign SMTP delivery reaches the scan pipeline and the nest writes its
`message_scan_results` row in the same ingest the `250` to `DATA` waits on — no
polling.

Taxonomy: tier_3 — real `fauna-nest` binary, real MTA scan + seal, real WS-RPC
User wire.
"""

import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3

SENDER_DOMAIN = "external.test"
NOT_FOUND = "fauna.bridges.not_found"


def _deliver(mx_port: int, server_name: str, recipient_addr: str,
             raw_message: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def test_recipient_reads_own_scan_result_and_other_user_is_refused(
    nest_instance, mail_bridge_mta, seal_helper_binary
):
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
    from conftest import _provision_msek_recipient

    owner = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_mta.domain,
        local_part="scan-owner",
        password="scan-owner-password-1",
    )
    other = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_mta.domain,
        local_part="scan-other",
        password="scan-other-password-1",
    )
    nonce = f"scanread{int(time.time() * 1000)}"
    message = (
        f"From: External Sender <sender@{SENDER_DOMAIN}>\r\n"
        f"To: {owner.username}\r\n"
        f"Subject: scan result {nonce}\r\n"
        f"Message-ID: <{nonce}@{SENDER_DOMAIN}>\r\n"
        "Date: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "\r\n"
        f"A benign body for {nonce}.\r\n"
    ).encode()
    _deliver(mail_bridge_mta.mx_port, mail_bridge_mta.domain, owner.username,
             message, time.monotonic() + 40.0)

    owner_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=owner.actor_id,
        signing_key=bytes(owner.recipient["signing_key"]),
    )
    with owner_client:
        page = owner_client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
        messages = page.get("messages", [])
        assert len(messages) == 1, (
            f"the one delivery must be in INBOX once the SMTP session ended; got "
            f"{[m.get('uid') for m in messages]}"
        )
        message_id = bytes(messages[0]["message_id"])
        assert len(message_id) == 32, message_id

        row = owner_client.call("fauna.bridges.get_message_scan_result",
                                {"message_id": message_id})
        assert bytes(row["message_id"]) == message_id, row
        assert row["clamav_verdict"] == {"kind": "clean"}, (
            f"the fake clamd scans a benign body clean: {row}"
        )
        assert row["rspamd_score"] is not None, (
            f"the fake rspamd ran, so its score is on the row: {row}"
        )
        assert row["action_taken"] == "delivered", row
        assert row["received_at"] > 0 and row["scanned_at"] > 0, row

    other_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=other.actor_id,
        signing_key=bytes(other.recipient["signing_key"]),
    )
    with other_client:
        with pytest.raises(RpcCallError) as e:
            other_client.call("fauna.bridges.get_message_scan_result",
                              {"message_id": message_id})
        assert e.value.code == NOT_FOUND, (
            f"another user's message must answer not-found: {e.value}"
        )
        with pytest.raises(RpcCallError) as e:
            other_client.call("fauna.bridges.get_message_scan_result",
                              {"message_id": bytes(32)})
        assert e.value.code == NOT_FOUND, (
            f"an id naming nothing answers the same not-found: {e.value}"
        )

"""tier_3 — ordinary delivery never dedups; only the import skips a seen message.

`[nest]` outcome 61 of `docs/features/mailbox-import.md` (`mailbox-migration.md`
§ Dedup scope): the dedup index is consulted only by `import_message`; every
inbound MTA delivery writes it (record-only) and never reads it. So a legitimate
resend (identical bytes) and a different message reusing a Message-ID both still
arrive. Observed from outside: three real SMTP deliveries through the Go MTA,
then the recipient's own `fauna.email.inbox.fetch` shows all three. The MTA
answers `250` to `DATA` only after the nest ingest committed, so no polling.
"""

import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3

SENDER_DOMAIN = "external.test"


def _deliver(mx_port: int, server_name: str, recipient_addr: str, raw: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _message(to: str, message_id: str, subject: str, body: str) -> bytes:
    return (
        f"From: External Sender <sender@{SENDER_DOMAIN}>\r\n"
        f"To: {to}\r\n"
        f"Subject: {subject}\r\n"
        f"Message-ID: <{message_id}>\r\n"
        "Date: Thu, 08 Oct 2026 12:00:00 +0000\r\n"
        "\r\n"
        f"{body}\r\n"
    ).encode()


@pytest.mark.feature("mailbox-import")
def test_a_resend_or_a_copy_reusing_a_message_id_still_arrives_by_delivery(
    nest_instance, mail_bridge_mta, seal_helper_binary
):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _provision_msek_recipient

    recip = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_mta.domain,
        local_part="no-dedup",
        password="no-dedup-password-1",
    )
    nonce = f"nodedup{time.time_ns()}"
    mid = f"{nonce}@{SENDER_DOMAIN}"
    original = _message(recip.username, mid, f"original {nonce}", f"body {nonce}")
    different = _message(recip.username, mid, f"different {nonce}", f"another body {nonce}")
    deadline = time.monotonic() + 60.0
    # the original, a byte-identical legitimate resend, and a different message with the same Message-ID
    for raw in (original, original, different):
        _deliver(mail_bridge_mta.mx_port, mail_bridge_mta.domain, recip.username, raw, deadline)

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=recip.actor_id,
        signing_key=bytes(recip.recipient["signing_key"]),
    )
    with client:
        page = client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
    messages = page.get("messages", [])
    assert len(messages) == 3, (
        "delivery must never dedup: the original, its byte-identical resend and the "
        f"same-Message-ID copy must all be in INBOX; got {len(messages)}: "
        f"{[m.get('uid') for m in messages]}"
    )

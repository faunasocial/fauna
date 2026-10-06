"""tier_3 API-helper: the app-facing mail read-state wire
(`mail-app-surface.md` § Read state) over the real WS-RPC `User` wire from
Python — the nest-side half of the conversations read-state journey, so a
failure there is triaged UI-side vs nest-side (E2E § rule 5).

  * `fauna.email.inbox.fetch` carries `highest_modseq`, the client's baseline.
  * `fauna.email.inbox.mark_seen` adds `\\Seen` to the caller's own INBOX UIDs
    and nothing else; a repeat is a no-op.
  * `fauna.email.inbox.flag_changes` returns exactly the rows past the cursor,
    each with its whole flag set, and nothing once the cursor catches up.

A fresh mail-enabled recipient receives two real MTA-sealed messages; the test
marks one read and checks the delta over the wire and the flags column in the
nest DB. The MTA answers `250` to `DATA` only after the nest ingest committed
(`fauna.bridges.ingest_inbound_mail` is awaited inside the DATA handler), so the
messages are in INBOX when the SMTP session ends — no polling.

Taxonomy: tier_3 — real `fauna-nest` binary, real MTA seal, real WS-RPC User wire.
"""

import sqlite3
import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3

SENDER_DOMAIN = "external.test"
SEEN = "\\Seen"


def _deliver(mx_port: int, server_name: str, recipient_addr: str,
             raw_message: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _inbox_flags(db_path: str, actor_id: bytes) -> dict:
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT uid, flags FROM bridge_imap_messages "
            "WHERE actor_id = ?1 AND mailbox = 'INBOX'",
            (actor_id,),
        ).fetchall()
    finally:
        conn.close()
    return {uid: set(flags.split()) for uid, flags in rows}


def test_mark_seen_then_flag_changes_round_trip(
    nest_instance, mail_bridge_mta, seal_helper_binary
):
    """Mark one of two delivered messages read; the delta reports exactly that
    row with its whole flag set, the other row stays unread, and a repeat
    changes nothing."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _provision_msek_recipient

    recip = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_mta.domain,
        local_part="read-state",
        password="read-state-password-1",
    )
    nonce = f"readstate{int(time.time() * 1000)}"
    for i in range(2):
        message = (
            f"From: External Sender <sender@{SENDER_DOMAIN}>\r\n"
            f"To: {recip.username}\r\n"
            f"Subject: read state {nonce} #{i}\r\n"
            f"Message-ID: <{nonce}-{i}@{SENDER_DOMAIN}>\r\n"
            "Date: Mon, 25 May 2026 12:00:00 +0000\r\n"
            "\r\n"
            f"Body {i} of {nonce}.\r\n"
        ).encode()
        _deliver(mail_bridge_mta.mx_port, mail_bridge_mta.domain, recip.username,
                 message, time.monotonic() + 40.0)

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=recip.actor_id,
        signing_key=bytes(recip.recipient["signing_key"]),
    )
    with client:
        page = client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
        messages = page.get("messages", [])
        assert len(messages) == 2, (
            "both deliveries must be in INBOX once the SMTP session ended; got "
            f"{[(m.get('uid'), m.get('flags')) for m in messages]}"
        )
        baseline = page.get("highest_modseq", 0)
        assert baseline > 0, f"inbox.fetch must carry highest_modseq; reply keys={list(page)}"
        read_uid, unread_uid = messages[0]["uid"], messages[1]["uid"]

        quiet = client.call("fauna.email.inbox.flag_changes",
                            {"since_modseq": baseline, "limit": 0})
        assert quiet["changes"] == [], f"nothing changed since the baseline: {quiet}"

        marked = client.call("fauna.email.inbox.mark_seen", {"uids": [read_uid]})
        assert marked["updated"] == 1, marked

        delta = client.call("fauna.email.inbox.flag_changes",
                            {"since_modseq": baseline, "limit": 0})
        assert [c["uid"] for c in delta["changes"]] == [read_uid], delta
        assert SEEN in delta["changes"][0]["flags"], delta
        assert delta["more"] is False, delta

        again = client.call("fauna.email.inbox.mark_seen", {"uids": [read_uid]})
        assert again["updated"] == 0, f"a repeat is a no-op: {again}"
        caught_up = client.call("fauna.email.inbox.flag_changes",
                                {"since_modseq": delta["highest_modseq"], "limit": 0})
        assert caught_up["changes"] == [], (
            f"a no-op repeat must not re-report the row: {caught_up}"
        )

    # ── Nest-DB ground truth: only the marked row is read.
    flags = _inbox_flags(nest_instance["db_path"], recip.actor_id)
    assert SEEN in flags[read_uid], flags
    assert SEEN not in flags[unread_uid], flags

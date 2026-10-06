"""Tier 3: the floors every forward is held to — the hourly cap queues rather
than drops, a forward that keeps failing bounces to its owner at most once per
message a week, and a forwarded message leaves with its sender and signature
intact — witnessed over the real MTA bridge, the real nest outbound queue and
the in-process stub MX the fixture routes `external.test` to
(`docs/goal/behavior/mail-forwarding.md` § Per-account forward rate-limit,
§ NDR rate-limit, § Architectural rules).

Each test forwards for a FRESH recipient (`_provision_recipient`), so its hourly
forward window and bounce history start empty whatever sibling tests put on the
session-shared recipient. Forward configuration goes through the user's own
doors — `fauna.bridges.set_forward_all_to` and `set_forward_per_hour`, the calls
an app's mail settings make — so the hourly-cap witness proves the limit a user
sets is the one the rate cap enforces. Waits are `wait_until` on a named state —
a queue row, a stub-MX capture, a sealed DSN — and the outbound drain is triggered by the production
`outbound_ready` nudge (`_poke_outbound_bridge`), never by elapsed time
(convention 14).
"""

import base64
import secrets
import sqlite3
import time

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import _connect_smtp_starttls, _find_tagged, _poke_outbound_bridge
from helpers.waiting import wait_until

from .test_mail_bridge_filter_delivery import _provision_recipient
from .test_mail_bridge_mta import _msgid_key, _outbound_queue

pytestmark = pytest.mark.tier_3


def _set_forwarding(nest_url, recipient, forward_all_to, per_hour=None):
    """As the recipient: forward all mail to `forward_all_to` (None stops it) and,
    when given, set the hourly forward limit — the user's own settings doors."""
    with WsRpcAdminClient(nest_url, actor_id=recipient["actor_id_bytes"],
                          signing_key=bytes(recipient["signing_key"])) as ws:
        ws.call("fauna.bridges.set_forward_all_to", {"forward_all_to": forward_all_to})
        if per_hour is not None:
            ws.call("fauna.bridges.set_forward_per_hour", {"forward_per_hour": per_hour})


def _count(db_path, sql, *args):
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(sql, args).fetchone()[0]
    finally:
        conn.close()


def _inbound(mta, sender, rcpt, raw: bytes):
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _message(sender, rcpt, token, msgid=None):
    return ("\r\n".join([
        f"From: Original Sender <{sender}>",
        f"To: {rcpt}",
        f"Subject: forward floor {token}",
        f"Message-ID: <{msgid or token}@orig.test>",
        "Date: Thu, 16 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"Forwarded body {token}.",
        "",
    ])).encode()


@pytest.mark.feature("mail-filter-rules")
def test_forward_over_the_hourly_cap_waits_and_goes_out_later(mail_bridge_mta, nest_instance):
    """With an hourly cap of one, the second forward in the hour is held in the
    forward queue, not dropped and not sent; once the cap allows it, the next
    outbound drain sends it on."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    target = f"capq-{secrets.token_hex(4)}@external.test"
    first, second = f"capq1-{secrets.token_hex(4)}", f"capq2-{secrets.token_hex(4)}"
    _set_forwarding(nest_instance["url"], recipient, target, per_hour=1)
    try:
        _inbound(mta, "sender@orig.test", addr, _message("sender@orig.test", addr, first))
        wait_until(lambda: _find_tagged(mta.stub_mx, first), MAIL_OUTBOUND_CYCLE_S,
                   diagnose=lambda: "the first forward (within the cap) never went out")

        _inbound(mta, "sender@orig.test", addr, _message("sender@orig.test", addr, second))
        wait_until(
            lambda: _count(db, "SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?",
                           actor_id) == 1,
            MAIL_OUTBOUND_CYCLE_S,
            diagnose=lambda: "the over-cap forward was not held in the forward queue")
        assert _find_tagged(mta.stub_mx, second) is None, "an over-cap forward must not go out yet"

        _set_forwarding(nest_instance["url"], recipient, target, per_hour=2)
        _poke_outbound_bridge(nest_instance["url"])
        wait_until(lambda: _find_tagged(mta.stub_mx, second), MAIL_OUTBOUND_CYCLE_S,
                   diagnose=lambda: "the held forward never went out once the cap allowed it")
        assert _count(db, "SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?",
                      actor_id) == 0, "the drained forward must leave the forward queue"
    finally:
        _set_forwarding(nest_instance["url"], recipient, None)


@pytest.mark.feature("mail-filter-rules")
def test_a_failing_forward_bounces_to_its_owner_once_per_message_a_week(
        mail_bridge_mta, nest_instance):
    """The same message forwarded twice to a destination that refuses it
    produces one bounce notice in the owner's inbox, not two; a different
    message still gets its own."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    url = nest_instance["url"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    target = f"nonexistent-ndr-{secrets.token_hex(4)}@external.test"
    repeated = f"ndr-{secrets.token_hex(4)}"
    other = f"ndr-other-{secrets.token_hex(4)}"

    def dsns():
        return _count(db, "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?"
                      " AND mailbox = 'INBOX' AND LOWER(from_norm) = LOWER(?)",
                      actor_id, mta.domain)

    def terminal_rows(msgid):
        return [r for r in _outbound_queue(url)
                if _msgid_key(r["original_msgid"]) == f"{msgid}@orig.test"
                and r["recipient"] == target and r["status"] != "pending"]

    _set_forwarding(nest_instance["url"], recipient, target)
    try:
        for n in (1, 2):
            _inbound(mta, "sender@orig.test", addr,
                     _message("sender@orig.test", addr, f"{repeated}-{n}", msgid=repeated))
            wait_until(lambda: len(terminal_rows(repeated)) == n, MAIL_OUTBOUND_CYCLE_S,
                       diagnose=lambda: f"forward {n} of the same message never reached a "
                                        f"final state; queue={_outbound_queue(url)!r}")
        statuses = sorted(r["status"] for r in terminal_rows(repeated))
        assert statuses == ["bounced", "suppressed_rate"], (
            f"the second failure of the same message must be rate-suppressed; got {statuses}")
        assert dsns() == 1, "the owner must get exactly one bounce notice for the message"

        _inbound(mta, "sender@orig.test", addr, _message("sender@orig.test", addr, other))
        wait_until(lambda: dsns() == 2, MAIL_OUTBOUND_CYCLE_S,
                   diagnose=lambda: "a different message's failed forward must bounce too")
    finally:
        _set_forwarding(nest_instance["url"], recipient, None)


@pytest.mark.feature("mail-filter-rules")
def test_forwarded_mail_keeps_its_sender_and_signature(mail_bridge_mta, nest_instance):
    """A forward leaves with the original `From:` and body byte-for-byte and the
    original sender's DKIM signature still verifying at the destination, while
    the envelope sender is rewritten under SRS — so the destination's SPF and
    DKIM checks both pass."""
    import dkim  # dkimpy — independent verifier, fleet venv dep

    mta = mail_bridge_mta
    recipient, _, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    target = f"dkimfwd-{secrets.token_hex(4)}@external.test"
    token = f"dkimfwd-{secrets.token_hex(4)}"
    sender = "author@orig.test"

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    pem = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                            serialization.NoEncryption())
    spki = key.public_key().public_bytes(serialization.Encoding.DER,
                                         serialization.PublicFormat.SubjectPublicKeyInfo)
    txt = b"v=DKIM1; k=rsa; p=" + base64.b64encode(spki)
    original = _message(sender, addr, token)
    signature = dkim.sign(original, b"fwdtest", b"orig.test", pem,
                          include_headers=[b"from", b"to", b"subject", b"message-id", b"date"])
    signed = signature + original

    _set_forwarding(nest_instance["url"], recipient, target)
    try:
        _inbound(mta, sender, addr, signed)
        received = wait_until(lambda: _find_tagged(mta.stub_mx, token), MAIL_OUTBOUND_CYCLE_S,
                              diagnose=lambda: "the forward never reached the destination")
    finally:
        _set_forwarding(nest_instance["url"], recipient, None)

    def dnsfunc(name, timeout=5):
        return txt if name == b"fwdtest._domainkey.orig.test." else None

    assert dkim.verify(received, dnsfunc=dnsfunc), (
        f"the original DKIM signature must still verify after forwarding:\n{received[:900]!r}")
    headers, _, body = received.partition(b"\r\n\r\n")
    assert body == original.partition(b"\r\n\r\n")[2], "the forwarded body must be byte-identical"
    assert f"From: Original Sender <{sender}>".encode() in headers.split(b"\r\n"), (
        "the forwarded From: must be the original sender's, unchanged")
    env_from = next(e for e, raw in mta.stub_mx.records() if token.encode() in raw)
    assert env_from.upper().startswith("SRS0="), (
        f"the envelope sender must be SRS-rewritten; got {env_from!r}")

"""Tier 3: the floors every forward is held to — the hourly cap and the daily
sending allowance queue rather than drop, a full forward queue hands a redirect
back to the owner's mailbox, a spoof of the nest's own domain is never
forwarded, a forged, expired or orphaned bounce never reaches anyone, a forward
that keeps failing bounces to its owner at most once per message a week, and a
forwarded message leaves with its sender and signature intact — witnessed over
the real MTA bridge, the real nest outbound queue and the in-process stub MX
the fixture routes `external.test` to (`docs/goal/behavior/mail-forwarding.md`
§ Per-account forward rate-limit, § Queue ceiling, § Spoof floor, § Bounce
decode on inbound, § NDR rate-limit, § Architectural rules).

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
import hashlib
import hmac
import secrets
import sqlite3
import string
import time

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import _connect_smtp_starttls, _find_tagged, _poke_outbound_bridge
from helpers.waiting import wait_until

from .test_mail_bridge_filter_delivery import (
    _create_forward_rule,
    _placed_count,
    _provision_recipient,
)
from .test_mail_bridge_mta import (
    _admin_ws,
    _create_admin_forwarder,
    _msgid_key,
    _outbound_queue,
)

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


def _message(sender, rcpt, token, msgid=None, header_from=None):
    return ("\r\n".join([
        f"From: Original Sender <{header_from or sender}>",
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


def _forwarded(db_path, destination, token):
    """Forwards of the message tagged `token` to `destination` the nest holds —
    sent or due (`outbound_mail_queue`) and parked (`forward_queue`). The MTA
    hands a forward to the nest before it ACKs the DATA dot, so the count is
    final once `_inbound` returns."""
    needle = f"%{token}%"
    return (_count(db_path, "SELECT COUNT(*) FROM outbound_mail_queue WHERE is_forwarded = 1"
                   " AND recipient = ? AND CAST(raw_message AS TEXT) LIKE ?",
                   destination, needle)
            + _count(db_path, "SELECT COUNT(*) FROM forward_queue WHERE destination_address = ?"
                     " AND CAST(raw_message AS TEXT) LIKE ?", destination, needle))


@pytest.mark.feature("mail-filter-rules")
def test_a_spoof_of_the_nests_own_domain_is_delivered_but_never_forwarded(
        mail_bridge_mta, nest_instance):
    """Mail from outside whose `From:` claims the nest's own domain, and which
    never authenticated as it, still reaches the member's mailbox — but neither
    forward-all, a forward rule nor an admin forwarder sends it on; the same
    shape of mail from a foreign domain goes out through all three
    (mail-forwarding.md § Spoof floor)."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    url = nest_instance["url"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    tag = secrets.token_hex(4)
    word = f"spoofrule{tag}"
    all_target = f"spoof-all-{tag}@external.test"
    rule_target = f"spoof-rule-{tag}@external.test"
    af_target = f"spoof-af-{tag}@external.test"
    af_local = f"spoof-af-{tag}"
    af_addr = f"{af_local}@{mta.domain}"
    _create_admin_forwarder(nest_instance, mta.domain, af_local, af_target)
    _create_forward_rule(url, recipient, actor_id, word, rule_target)
    targets = (all_target, rule_target, af_target)
    spoof, spoof_af = f"{word}-spoof", f"{word}-spoofaf"
    legit, legit_af = f"{word}-legit", f"{word}-legitaf"
    envelope = "sender@orig.test"

    _set_forwarding(url, recipient, all_target)
    try:
        forged_from = f"ceo@{mta.domain}"
        _inbound(mta, envelope, addr,
                 _message(envelope, addr, spoof, header_from=forged_from))
        _inbound(mta, envelope, af_addr,
                 _message(envelope, af_addr, spoof_af, header_from=forged_from))
        assert _placed_count(db, actor_id) == 1, (
            "the member's own copy of a spoofed message must still arrive")
        for target in targets:
            for token in (spoof, spoof_af):
                assert _forwarded(db, target, token) == 0, (
                    f"a spoof of {mta.domain} must not be forwarded to {target}")

        # The same doors forward mail whose From: is a foreign domain: the
        # floor, not the setup, is what held the spoof back.
        _inbound(mta, envelope, addr, _message(envelope, addr, legit))
        _inbound(mta, envelope, af_addr, _message(envelope, af_addr, legit_af))
        assert _forwarded(db, all_target, legit) == 1, "forward-all must forward foreign mail"
        assert _forwarded(db, rule_target, legit) == 1, "the rule must forward foreign mail"
        assert _forwarded(db, af_target, legit_af) == 1, (
            "the admin forwarder must forward foreign mail")
    finally:
        _set_forwarding(url, recipient, None)


def _used_today(db_path, actor_id):
    return _count(db_path, "SELECT COALESCE(SUM(used), 0) FROM bridge_submission_quota"
                  " WHERE actor_id = ? AND day_bucket = ?", actor_id, int(time.time()) // 86400)


@pytest.mark.feature("mail-filter-rules")
def test_forwards_draw_on_the_daily_allowance_and_wait_once_it_is_spent(
        mail_bridge_mta, nest_instance):
    """A forward spends one unit of its owner's daily sending allowance; once
    the day's allowance is spent the next forward is held in the forward queue,
    not lost and not sent, and it goes out when the allowance opens again
    (mail-forwarding.md § Architectural rules — the daily recipients quota at
    both dispatch points)."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    url = nest_instance["url"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    target = f"dayq-{secrets.token_hex(4)}@external.test"
    first, second = f"dayq1-{secrets.token_hex(4)}", f"dayq2-{secrets.token_hex(4)}"
    assert _used_today(db, actor_id) == 0, "a fresh member starts the day with nothing spent"

    def parked():
        return _count(db, "SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?", actor_id)

    _set_forwarding(url, recipient, target)
    try:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {"max_per_day": 1})
        _inbound(mta, "sender@orig.test", addr, _message("sender@orig.test", addr, first))
        wait_until(lambda: _find_tagged(mta.stub_mx, first), MAIL_OUTBOUND_CYCLE_S,
                   diagnose=lambda: "the forward within the daily allowance never went out")
        assert _used_today(db, actor_id) == 1, "a forward must spend one unit of the allowance"

        _inbound(mta, "sender@orig.test", addr, _message("sender@orig.test", addr, second))
        assert parked() == 1, "a forward past the spent allowance must be held in the queue"
        assert _used_today(db, actor_id) == 1, "a held forward must spend nothing"
        assert _find_tagged(mta.stub_mx, second) is None, "a held forward must not go out yet"

        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {"max_per_day": 2})
        _poke_outbound_bridge(url)
        wait_until(lambda: _find_tagged(mta.stub_mx, second), MAIL_OUTBOUND_CYCLE_S,
                   diagnose=lambda: "the held forward never went out once the allowance opened")
        assert parked() == 0, "the sent forward must leave the forward queue"
        assert _used_today(db, actor_id) == 2, "the promoted forward must spend its unit"
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {})
        _set_forwarding(url, recipient, None)


# RFC 4648 base32, upper-case: the letters, then the digits 2 to 7.
_B32 = string.ascii_uppercase + "".join(str(d) for d in range(2, 8))


def _b32(value, n_chars):
    return "".join(_B32[(value >> (5 * (n_chars - 1 - i))) & 0x1F] for i in range(n_chars))


def _srs_mac(secret, tt, short_id, sender_domain, sender_local):
    """`HHH` as `fauna_mail::srs` computes it (mail-forwarding.md § SRS scheme):
    the top 20 bits of HMAC-SHA-256 over `TT || short-id || domain || local`,
    NUL-separated, in 4 base32 characters. The test proves this copy right by
    reproducing the MAC of an address the nest minted before it uses it."""
    payload = (tt.to_bytes(2, "big") + b"\0" + short_id.encode() + b"\0"
               + sender_domain.encode() + b"\0" + sender_local.encode())
    out = hmac.new(secret, payload, hashlib.sha256).digest()
    return _b32((out[0] << 12) | (out[1] << 4) | (out[2] >> 4), 4)


def _bounce(mta, srs_addr, rcpt_code, deliver=False):
    """Play the downstream MX: a null-sender bounce to `srs_addr`. Returns the
    RCPT reply; with `deliver`, also sends the DSN and expects the dot's 250."""
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<>", "250", deadline)
        reply = conn.cmd(f"RCPT TO:<{srs_addr}>", rcpt_code, deadline)
        if deliver:
            conn.cmd("DATA", "354", deadline)
            conn.send_raw("\r\n".join([
                "From: Mail Delivery System <MAILER-DAEMON@external.test>",
                f"To: <{srs_addr}>",
                "Subject: Delivery Status Notification (Failure)",
                f"Message-ID: <dsn-{secrets.token_hex(6)}@external.test>",
                "Date: Thu, 16 May 2026 12:05:00 +0000",
                "Content-Type: text/plain; charset=utf-8",
                "",
                "550 5.1.1 mailbox unavailable",
                "",
            ]).encode())
            conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)
    return reply


@pytest.mark.feature("mail-filter-rules")
def test_a_forged_or_expired_bounce_is_refused_and_an_orphan_is_dropped(
        mail_bridge_mta, nest_instance):
    """A bounce to one of the nest's forwarding (SRS) addresses is refused
    `550 5.1.1` when the address was forged and `550 5.4.4` when it is older
    than the bounce window; a genuine bounce for a forward the nest no longer
    holds is accepted and reaches no mailbox (mail-forwarding.md § Bounce
    decode on inbound — steps 3 and 4, and the orphan paragraph). The routed
    case is `test_mail_bridge_mta.py::test_inbound_srs_bounce_routes_to_forwarder`."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    url = nest_instance["url"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    target = f"srsfail-{secrets.token_hex(4)}@external.test"
    token = f"srsfail-{secrets.token_hex(4)}"

    _set_forwarding(url, recipient, target)
    try:
        _inbound(mta, "sender@orig.test", addr, _message("sender@orig.test", addr, token))
        srs_addr = wait_until(
            lambda: next((e for e, raw in mta.stub_mx.records()
                          if token.encode() in raw and e.upper().startswith("SRS0=")), None),
            MAIL_OUTBOUND_CYCLE_S,
            diagnose=lambda: f"the forward never went out under SRS (log: {mta.log_file})")
    finally:
        _set_forwarding(url, recipient, None)
    local, _, domain = srs_addr.rpartition("@")
    prefix, hhh, tt_b32, short_id, sender_domain, sender_local = local.split("=")
    tt = sum(_B32.index(c) << (5 * (1 - i)) for i, c in enumerate(tt_b32.upper()))
    conn = sqlite3.connect(db, timeout=10.0)
    try:
        stored = [bytes(r[0]) for r in conn.execute("SELECT secret FROM mail_srs_secrets")]
    finally:
        conn.close()
    secret = next((s for s in stored
                   if _srs_mac(s, tt, short_id, sender_domain, sender_local) == hhh.upper()),
                  None)
    assert secret is not None, f"the test's MAC copy must reproduce the nest's: {srs_addr!r}"

    flipped = ("B" if hhh[0].upper() != "B" else "C") + hhh[1:]
    forged = "=".join([prefix, flipped, tt_b32, short_id, sender_domain, sender_local])
    reply = _bounce(mta, f"{forged}@{domain}", "550")
    assert "5.1.1" in str(reply), f"a forged SRS address must be refused 5.1.1; got {reply!r}"

    old_tt = (tt - 40) % 1024
    expired = "=".join([prefix, _srs_mac(secret, old_tt, short_id, sender_domain, sender_local),
                        _b32(old_tt, 2), short_id, sender_domain, sender_local])
    reply = _bounce(mta, f"{expired}@{domain}", "550")
    assert "5.4.4" in str(reply), (
        f"a genuine SRS address older than the bounce window must be refused 5.4.4; "
        f"got {reply!r}")

    # The forward is pruned from the nest between forward and bounce: the
    # bounce verifies, but nobody is left to give it to.
    conn = sqlite3.connect(db, timeout=10.0)
    try:
        conn.execute("DELETE FROM outbound_mail_queue WHERE id = ?", (int(short_id),))
        conn.commit()
    finally:
        conn.close()
    inbox_before = _placed_count(db, actor_id)
    admin_id = bytes(nest_instance["admin"]["signing_key"].verify_key)
    admin_before = _placed_count(db, admin_id)
    _bounce(mta, srs_addr, "250", deliver=True)
    assert _placed_count(db, actor_id) == inbox_before, (
        "an orphan bounce must not reach the forwarder's mailbox")
    assert _placed_count(db, admin_id) == admin_before, (
        "an orphan bounce must never be handed to the admin either")


@pytest.mark.feature("mail-filter-rules")
def test_a_redirect_the_full_forward_queue_cannot_take_lands_in_the_owners_mailbox(
        mail_bridge_mta, nest_instance):
    """With the hourly forward limit at one, the forward queue holds 24
    forwards; once a redirect rule has filled it, the next message the rule
    would have redirected is delivered to the owner's own mailbox instead —
    the queue keeps every held redirect, and nothing is lost (mail-forwarding.md
    § Queue ceiling)."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    url = nest_instance["url"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    tag = secrets.token_hex(4)
    word = f"qfull{tag}"
    destination = f"qfull-{tag}@external.test"
    per_hour = 1
    ceiling = per_hour * 24
    _set_forwarding(url, recipient, None, per_hour=per_hour)
    _create_forward_rule(url, recipient, actor_id, word, destination, redirect=True)

    def parked():
        return _count(db, "SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?", actor_id)

    def sent_or_due():
        return _count(db, "SELECT COUNT(*) FROM outbound_mail_queue WHERE is_forwarded = 1"
                      " AND recipient = ?", destination)

    envelope = "sender@orig.test"
    _inbound(mta, envelope, addr, _message(envelope, addr, f"{word}-0"))
    assert sent_or_due() == 1, "the first redirect, within the hourly limit, must be sent"
    for n in range(1, ceiling + 1):
        _inbound(mta, envelope, addr, _message(envelope, addr, f"{word}-{n}"))
    assert parked() == ceiling, f"the forward queue must hold {ceiling} redirects; holds {parked()}"
    assert _placed_count(db, actor_id) == 0, "a redirect keeps no local copy while it is queued"

    _inbound(mta, envelope, addr, _message(envelope, addr, f"{word}-over"))
    assert _placed_count(db, actor_id) == 1, (
        "a redirect the full queue cannot take must be delivered to the owner's mailbox")
    assert parked() == ceiling, "no held redirect may be evicted to make room"
    assert _forwarded(db, destination, f"{word}-over") == 0, (
        "the refused redirect must not be queued as well")
    assert sent_or_due() == 1, "nothing more may be sent past the hourly limit"

"""Tier 3: delivery-time email filter-rule execution (T3.3).

The "green test or it doesn't work" gate for the filter track: prove that a
recipient's stored `fauna.email.filters.*` rules, evaluated at the Go MTA
perimeter at delivery, actually place a delivered message in the rule's folder
(or drop it) — end-to-end over real binaries and the real WS-RPC wire.

Why MTA-only + a nest-DB placement assertion (not IMAP read-back): the filter
feature's observable result is *which mailbox the inbound message lands in* —
exactly the `bridge_imap_messages.mailbox` column an IMAP SELECT later reads
from. Reading it back via the MDA/IMAP bridge would require a second bridge on
the same nest, but the MTA and MDA fixtures each register their own *primary*
domain (nest-global singleton), so co-instantiating them collides (the MTA
bridge's TLS cert no longer matches the clobbered primary). So this test spins
only the MTA bridge, delivers via SMTP, and asserts placement from the nest DB —
the same row IMAP reads. (IMAP read-back of decrypted bodies is covered by
`test_mail_bridge_mda.py`.)

A *fresh, dedicated* recipient is provisioned per test so the persistent filter
rules created here can't leak into other MTA tests that share the session
recipient. Spam score note: the e2e fixture runs no live rspamd, so the combined
score is deterministically 0; the spam-score rule uses
`SpamScoreAtLeast { milli: 0 }` (the matching boundary at score 0) — the
threshold comparison itself is unit-tested in `fauna_mail::filter`.
"""

import secrets
import sqlite3
import time

import pytest
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.mail_wire import _connect_smtp_starttls
from helpers.mail_aliases import add_exact_alias
from helpers.recipient_seal_key import provision_recipient_seal_key

pytestmark = pytest.mark.tier_3


def _deliver(mx_port, sni_domain, mail_from, rcpt_to, subject,
             body_text="filter delivery test body"):
    """Deliver one inbound message through the real MTA bridge (STARTTLS on 25).

    Loopback peer ⇒ the sender-domain MX check and FCrDNS are exempt, so the
    `mail_from` domain need not resolve. `body_text` is the decoded text/plain
    body the perimeter `BodyContains` rule matches against. Returns on a 250 OK
    for the DATA dot (the round-trip signal that nest's ingest committed)."""
    deadline = time.monotonic() + 30.0
    body = "\r\n".join([
        f"From: Sender <{mail_from}>",
        f"To: {rcpt_to}",
        f"Subject: {subject}",
        f"Message-ID: <{int(time.time() * 1_000_000)}@filter.test>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        body_text,
    ]) + "\r\n"
    with _connect_smtp_starttls(mx_port, sni_domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{mail_from}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt_to}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _placements(db_path, actor_id):
    """Return {sender_domain: mailbox} for every message placed for `actor_id`.

    `from_norm` is the lowercased envelope/From sender domain the ingest stored;
    `mailbox` is where the perimeter filter (or spam disposition) placed it."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT from_norm, mailbox FROM bridge_imap_messages WHERE actor_id = ?",
            (actor_id,),
        ).fetchall()
    finally:
        conn.close()
    return {frm: mbox for (frm, mbox) in rows}


def _placement_flags(db_path, actor_id):
    """Return {sender_domain: (mailbox, flags)} for every placed message.

    `flags` is the IMAP-keyword string the ingest stored — the `AddLabel`
    filter action appends to it, so a continue-chain test can assert both the
    folder (`FileInto`) and the label (`AddLabel`) landed."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT from_norm, mailbox, flags FROM bridge_imap_messages WHERE actor_id = ?",
            (actor_id,),
        ).fetchall()
    finally:
        conn.close()
    return {frm: (mbox, flags) for (frm, mbox, flags) in rows}


@pytest.mark.feature("mail-filter-rules")
def test_delivery_time_filters_route_to_folders(mail_bridge_mta, nest_instance):
    mta = mail_bridge_mta
    nest_port = nest_instance["port"]
    nest_url = nest_instance["url"]
    admin = nest_instance["admin"]

    # ── 1. A fresh, isolated recipient on the MTA's domain (own mailboxes, so
    # the persistent filter rules below can't leak into other MTA tests).
    recipient = create_actor_and_register(nest_port, admin_signing_key=admin["signing_key"])
    recipient_actor_id = recipient["actor_id_bytes"]
    rcpt_local = f"filterbox-{int(time.time() * 1000)}"
    rcpt_addr = f"{rcpt_local}@{mta.domain}"

    # A throwaway seal key is enough: the MTA seals the body to it and nest
    # places the record; this test asserts placement, never decrypts.
    admin_sk = admin["signing_key"]
    admin_ws = WsRpcAdminClient(
        nest_url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        add_exact_alias(nest_url, recipient["signing_key"], mta.domain, rcpt_local)
        provision_recipient_seal_key(admin_ws, recipient_actor_id)

    # ── 2. Filter rules as the recipient (first-match-wins by priority):
    #   p0  SenderDomain discard.test → Discard          (drop)
    #   p10 SenderDomain reports.test → FileInto Reports (custom folder)
    #   p15 BodyContains "invoice"    → FileInto Invoices (body-axis, S4b)
    #   p20 SpamScoreAtLeast{milli:0} → FileInto Junk    (catch-all via score)
    recipient_ws = WsRpcAdminClient(
        nest_url, actor_id=recipient_actor_id, signing_key=bytes(recipient["signing_key"])
    )
    with recipient_ws:
        recipient_ws.call("fauna.email.filters.create", {
            "name": "discard senders",
            "rules": [{"SenderDomain": {"domain": "discard.test"}}],
            "combination": "all",
            "action": "Discard",
            "priority": 0,
        })
        recipient_ws.call("fauna.email.filters.create", {
            "name": "reports to folder",
            "rules": [{"SenderDomain": {"domain": "reports.test"}}],
            "combination": "all",
            "action": {"FileInto": {"mailbox": "Reports"}},
            "priority": 10,
        })
        recipient_ws.call("fauna.email.filters.create", {
            "name": "invoices by body",
            "rules": [{"BodyContains": {"text": "invoice"}}],
            "combination": "all",
            "action": {"FileInto": {"mailbox": "Invoices"}},
            "priority": 15,
        })
        # Continue chain: p12 labels (continue → falls through), p13 files into a
        # folder (terminal). Both fire for a team.test message, proving multi-action.
        recipient_ws.call("fauna.email.filters.create", {
            "name": "label team mail",
            "rules": [{"SenderDomain": {"domain": "team.test"}}],
            "combination": "all",
            "action": {"AddLabel": {"label": "important"}},
            "priority": 12,
            "continue_on_match": True,
        })
        recipient_ws.call("fauna.email.filters.create", {
            "name": "team to folder",
            "rules": [{"SenderDomain": {"domain": "team.test"}}],
            "combination": "all",
            "action": {"FileInto": {"mailbox": "Team"}},
            "priority": 13,
        })
        recipient_ws.call("fauna.email.filters.create", {
            "name": "scored to junk",
            "rules": [{"SpamScoreAtLeast": {"milli": 0}}],
            "combination": "all",
            "action": {"FileInto": {"mailbox": "Junk"}},
            "priority": 20,
        })

    # ── 3. Deliver one message matching each rule (distinct sender domains).
    # The body-match message uses a fresh sender domain (no sender rule fires) and
    # a body containing "invoice" so the p15 BodyContains rule wins before the p20
    # score catch-all; the others carry the default body (no "invoice").
    _deliver(mta.mx_port, mta.domain, "alice@reports.test", rcpt_addr, "quarterly report")
    _deliver(mta.mx_port, mta.domain, "spammer@elsewhere.test", rcpt_addr, "scored to junk")
    _deliver(mta.mx_port, mta.domain, "biller@billing.test", rcpt_addr, "your statement",
             body_text="Attached is your INVOICE for May. Total due: $42.")
    _deliver(mta.mx_port, mta.domain, "lead@team.test", rcpt_addr, "standup notes")
    _deliver(mta.mx_port, mta.domain, "drop@discard.test", rcpt_addr, "should be discarded")

    # ── 4. Assert placement from the nest DB (the row IMAP SELECT reads from).
    placed = _placements(nest_instance["db_path"], recipient_actor_id)

    assert placed.get("reports.test") == "Reports", (
        f"SenderDomain→FileInto must file the reports.test message into Reports; "
        f"placements={placed}"
    )
    assert placed.get("elsewhere.test") == "Junk", (
        f"SpamScoreAtLeast→FileInto must file the scored message into Junk; "
        f"placements={placed}"
    )
    assert placed.get("billing.test") == "Invoices", (
        f"BodyContains→FileInto must file the body-matched message into Invoices "
        f"(case-insensitive substring over the decoded body); placements={placed}"
    )
    assert "discard.test" not in placed, (
        f"Discard must drop the discard.test message (no placement row); "
        f"placements={placed}"
    )

    # ── 5. Continue chain: p12 AddLabel(continue) + p13 FileInto both applied.
    # The folder proves p13 was reached (continue fell through p12); the flag
    # proves p12's AddLabel composed in (multi-action, not first-match-wins).
    placed_flags = _placement_flags(nest_instance["db_path"], recipient_actor_id)
    team_mailbox, team_flags = placed_flags.get("team.test", (None, None))
    assert team_mailbox == "Team", (
        f"continue chain: p13 FileInto must file the team.test message into Team "
        f"(proves continue fell through p12); placements={placed_flags}"
    )
    assert team_flags is not None and "important" in team_flags, (
        f"continue chain: p12 AddLabel must add the 'important' keyword "
        f"(proves multi-action composition); flags={team_flags!r}"
    )


# ── Reject action (O2/O3) ────────────────────────────────────────────────────


def _deliver_rcpts(mx_port, sni_domain, mail_from, rcpts, subject,
                   body_text="reject filter test body", expect_data_code="250",
                   extra_headers=None, header_from=None):
    """Deliver one inbound message to one or more RCPT TOs through the real MTA.

    `expect_data_code` is the SMTP reply expected at the DATA dot — `250`
    normally, `550` when a *single-recipient* transaction is refused by a fired
    `Reject` filter. A multi-recipient transaction always 250s (the server has
    already committed to delivering the accepted recipients). `extra_headers` are
    extra `Name: value` header lines (e.g. `Auto-Submitted` for the AutoReply
    loop-guard tests). The `To:` header lists the rcpts so the AutoReply
    recipient-in-header guard (RFC 5230 §4.4) is satisfied by default.
    `header_from` overrides the `From:` address (a null envelope sender still
    needs a well-formed header author)."""
    deadline = time.monotonic() + 30.0
    headers = [
        f"From: Sender <{header_from or mail_from}>",
        f"To: {', '.join(rcpts)}",
        f"Subject: {subject}",
        f"Message-ID: <{secrets.token_hex(8)}@filter.test>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
    ]
    headers.extend(extra_headers or [])
    body = "\r\n".join(headers + ["", body_text]) + "\r\n"
    with _connect_smtp_starttls(mx_port, sni_domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{mail_from}>", "250", deadline)
        for rcpt in rcpts:
            conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", expect_data_code, deadline)
        conn.cmd("QUIT", "221", deadline)


def _outbound_count(db_path):
    """Rows in nest's `outbound_mail_queue` (DSNs, forwards, auto-replies). A
    `Reject` must never enqueue one — the bounce is the sender's MX's job, not
    ours (backscatter suppression)."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute("SELECT COUNT(*) FROM outbound_mail_queue").fetchone()[0]
    finally:
        conn.close()


def _provision_recipient(nest_port, nest_url, admin, mta_domain):
    """Provision a fresh, isolated recipient on `mta_domain`: register an actor,
    map an exact alias to it, and provision a throwaway seal key (the MTA seals
    to it; these tests assert placement / outbound, never decrypt). Returns
    `(recipient_dict, actor_id_bytes, address)`."""
    recipient = create_actor_and_register(nest_port, admin_signing_key=admin["signing_key"])
    actor_id = recipient["actor_id_bytes"]
    local = f"rejbox-{secrets.token_hex(4)}"
    addr = f"{local}@{mta_domain}"
    admin_sk = admin["signing_key"]
    admin_ws = WsRpcAdminClient(
        nest_url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        add_exact_alias(nest_url, recipient["signing_key"], mta_domain, local)
        provision_recipient_seal_key(admin_ws, actor_id)
    return recipient, actor_id, addr


@pytest.mark.feature("mail-filter-rules")
def test_reject_single_recipient_refuses_at_data(mail_bridge_mta, nest_instance):
    """A single-recipient transaction whose recipient's `Reject` rule fires is
    refused `550` at end-of-DATA (so the sender's own MX bounces it); the message
    is not placed and we synthesize no DSN — backscatter-safe (O2/O3)."""
    mta = mail_bridge_mta
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)

    recipient_ws = WsRpcAdminClient(
        nest_instance["url"], actor_id=actor_id, signing_key=bytes(recipient["signing_key"]))
    with recipient_ws:
        recipient_ws.call("fauna.email.filters.create", {
            "name": "reject senders",
            "rules": [{"SenderDomain": {"domain": "reject.test"}}],
            "combination": "all",
            "action": {"Reject": {"reason": "Not accepting your mail"}},
            "priority": 0,
        })

    outbound_before = _outbound_count(nest_instance["db_path"])
    _deliver_rcpts(mta.mx_port, mta.domain, "spammer@reject.test", [addr],
                   "please reject me", expect_data_code="550")

    placed = _placements(nest_instance["db_path"], actor_id)
    assert "reject.test" not in placed, (
        f"single-recipient Reject must place nothing (refused at DATA); "
        f"placements={placed}")
    assert _outbound_count(nest_instance["db_path"]) == outbound_before, (
        "Reject must not enqueue a DSN — the sender's MX bounces it, not us")


@pytest.mark.feature("mail-filter-rules")
def test_reject_multi_recipient_drops_without_dsn(mail_bridge_mta, nest_instance):
    """In a multi-recipient transaction the server has already committed `250`
    for the accepted recipients, so a fired `Reject` drops only that recipient
    (no whole-DATA 550, no DSN); the sibling recipient is still delivered (O2)."""
    mta = mail_bridge_mta
    nest_url = nest_instance["url"]
    admin = nest_instance["admin"]
    rejected, rej_actor, rej_addr = _provision_recipient(
        nest_instance["port"], nest_url, admin, mta.domain)
    normal, norm_actor, norm_addr = _provision_recipient(
        nest_instance["port"], nest_url, admin, mta.domain)

    rej_ws = WsRpcAdminClient(
        nest_url, actor_id=rej_actor, signing_key=bytes(rejected["signing_key"]))
    with rej_ws:
        rej_ws.call("fauna.email.filters.create", {
            "name": "reject senders",
            "rules": [{"SenderDomain": {"domain": "reject.test"}}],
            "combination": "all",
            "action": {"Reject": {"reason": "go away"}},
            "priority": 0,
        })

    outbound_before = _outbound_count(nest_instance["db_path"])
    # One transaction, two recipients → 250 at DATA (the multi-RCPT contract).
    _deliver_rcpts(mta.mx_port, mta.domain, "spammer@reject.test",
                   [rej_addr, norm_addr], "multi reject", expect_data_code="250")

    rej_placed = _placements(nest_instance["db_path"], rej_actor)
    norm_placed = _placements(nest_instance["db_path"], norm_actor)
    assert "reject.test" not in rej_placed, (
        f"Reject must drop the rejecting recipient's copy; placements={rej_placed}")
    assert "reject.test" in norm_placed, (
        f"the sibling recipient must still be delivered; placements={norm_placed}")
    assert _outbound_count(nest_instance["db_path"]) == outbound_before, (
        "multi-recipient Reject must not enqueue a DSN")


# ── AutoReply action (Sieve vacation; O4/O5/O6) ──────────────────────────────


def _outbound_rows_to(db_path, recipient):
    """Return [(original_sender, raw_message_bytes)] for outbound_mail_queue rows
    addressed to `recipient`. An AutoReply enqueues exactly one such row (to the
    original envelope sender) with a NULL `original_sender` (MAIL FROM:<>)."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT original_sender, raw_message FROM outbound_mail_queue WHERE recipient = ?",
            (recipient,),
        ).fetchall()
    finally:
        conn.close()
    return rows


@pytest.mark.feature("mail-filter-rules")
def test_autoreply_sends_once_then_rate_limited(mail_bridge_mta, nest_instance):
    """An AutoReply rule enqueues one vacation reply to the sender (null
    envelope-from, `Auto-Submitted: auto-replied`); a second message from the
    same sender within `interval_hours` is rate-limited — still exactly one
    reply (O5/O6)."""
    mta = mail_bridge_mta
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)

    recipient_ws = WsRpcAdminClient(
        nest_instance["url"], actor_id=actor_id, signing_key=bytes(recipient["signing_key"]))
    with recipient_ws:
        recipient_ws.call("fauna.email.filters.create", {
            "name": "vacation",
            "rules": [{"SenderDomain": {"domain": "vacation.test"}}],
            "combination": "all",
            "action": {"AutoReply": {
                "subject": "Out of office",
                "body": "I am away until June. — auto-reply",
                "interval_hours": 24,
            }},
            "priority": 0,
        })

    sender = "boss@vacation.test"
    _deliver_rcpts(mta.mx_port, mta.domain, sender, [addr], "are you there?")

    rows = _outbound_rows_to(nest_instance["db_path"], sender)
    assert len(rows) == 1, f"AutoReply must enqueue exactly one reply to the sender; rows={len(rows)}"
    original_sender, raw = rows[0]
    assert original_sender == "", (
        f"the auto-reply must use a null envelope-from (MAIL FROM:<>); got {original_sender!r}")
    raw_text = bytes(raw).decode("utf-8", "replace")
    assert "Auto-Submitted: auto-replied" in raw_text, (
        f"the reply must carry Auto-Submitted: auto-replied (RFC 3834); raw=\n{raw_text}")
    assert f"To: {sender}" in raw_text and "Subject: Out of office" in raw_text, raw_text

    # A second message from the same sender within the interval → rate-limited.
    _deliver_rcpts(mta.mx_port, mta.domain, sender, [addr], "still there?")
    rows = _outbound_rows_to(nest_instance["db_path"], sender)
    assert len(rows) == 1, (
        f"a repeat within interval_hours must be rate-limited (still one reply); rows={len(rows)}")


@pytest.mark.feature("mail-filter-rules")
def test_autoreply_suppressed_by_loop_guard(mail_bridge_mta, nest_instance):
    """An inbound message that is itself automated (`Auto-Submitted` ≠ no) must
    not trigger a vacation reply (RFC 3834 loop guard, O4) — no outbound row."""
    mta = mail_bridge_mta
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)

    recipient_ws = WsRpcAdminClient(
        nest_instance["url"], actor_id=actor_id, signing_key=bytes(recipient["signing_key"]))
    with recipient_ws:
        recipient_ws.call("fauna.email.filters.create", {
            "name": "vacation",
            "rules": [{"SenderDomain": {"domain": "robot.test"}}],
            "combination": "all",
            "action": {"AutoReply": {
                "subject": "Out of office",
                "body": "away",
                "interval_hours": 24,
            }},
            "priority": 0,
        })

    sender = "mailer@robot.test"
    _deliver_rcpts(mta.mx_port, mta.domain, sender, [addr], "newsletter",
                   extra_headers=["Auto-Submitted: auto-generated"])

    rows = _outbound_rows_to(nest_instance["db_path"], sender)
    assert rows == [], (
        f"an Auto-Submitted message must not trigger a vacation reply; rows={rows}")


# ── Forward action (mail-forwarding.md § Per-rule "forward to") ─────────────


def _placed_count(db_path, actor_id):
    """Messages placed for `actor_id` (every mailbox) — the local copy a
    suppressed forward must still leave behind."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?", (actor_id,),
        ).fetchone()[0]
    finally:
        conn.close()


def _forward_rows_to(db_path, destination):
    """`(forward_rule_id, forward_actor_id, original_sender, raw_message)` for
    every forwarded outbound row addressed to `destination`. The MTA enqueues a
    rule's forward before it ACKs the DATA dot, so the rows are final by the
    time `_deliver_rcpts` returns — no wait."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(
            "SELECT forward_rule_id, forward_actor_id, original_sender, raw_message"
            " FROM outbound_mail_queue WHERE recipient = ? AND is_forwarded = 1",
            (destination,),
        ).fetchall()
    finally:
        conn.close()


def _create_forward_rule(nest_url, recipient, actor_id, subject_word, destination,
                         redirect=False):
    """Store a rule forwarding mail whose subject carries `subject_word` to
    `destination`; returns the rule id (the `rule=` token of the loop stamp).
    `redirect=True` is the "don't keep a local copy" copy mode; the default
    omits the wire key entirely — the default copy mode, which current apps
    also send (mail-forwarding.md § Per-rule "forward to")."""
    action = {"address": destination}
    if redirect:
        action["redirect"] = True
    ws = WsRpcAdminClient(nest_url, actor_id=actor_id,
                          signing_key=bytes(recipient["signing_key"]))
    with ws:
        reply = ws.call("fauna.email.filters.create", {
            "name": "forward on",
            "rules": [{"SubjectContains": {"text": subject_word}}],
            "combination": "all",
            "action": {"Forward": action},
            "priority": 0,
        })
        # The copy mode survives the create → list round trip through the
        # nest's storage form (the `forward_redirect` column) — the rule the
        # perimeter fetches is the one the user wrote.
        stored = [f for f in ws.call("fauna.email.filters.list", {})["filters"]
                  if f["id"] == reply["id"]]
        assert len(stored) == 1, stored
        assert stored[0]["action"] == {
            "Forward": {"address": destination, "redirect": redirect}
        }, stored[0]["action"]
    return reply["id"]


@pytest.mark.feature("mail-filter-rules")
def test_forward_rule_loop_suppressed_keeps_local_copy(mail_bridge_mta, nest_instance):
    """A fired forward rule forwards the message on (one forwarded row to the
    rule's destination, attributed to the rule owner, `rule=<id>` stamped); the
    same message arriving back already carrying our own `X-Fauna-Forwarded-By`
    is still delivered locally, and only its forward is dropped (§ Loop
    detection; § Loop suppression vs. delivery)."""
    mta = mail_bridge_mta
    db_path = nest_instance["db_path"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    destination = f"fwd-{secrets.token_hex(4)}@downstream.test"
    rule_id = _create_forward_rule(nest_instance["url"], recipient, actor_id,
                                   "fwdloop", destination)

    _deliver_rcpts(mta.mx_port, mta.domain, "someone@origin.test", [addr],
                   "fwdloop first pass")
    rows = _forward_rows_to(db_path, destination)
    assert len(rows) == 1, f"a fired forward rule must forward once; rows={len(rows)}"
    rule_col, fwd_actor, original_sender, raw = rows[0]
    assert rule_col == str(rule_id), f"forward_rule_id: {rule_col!r}, want {rule_id}"
    assert bytes(fwd_actor) == actor_id, "the forward is attributed to the rule's owner"
    assert original_sender == "someone@origin.test", original_sender
    assert f"rule={rule_id}".encode() in bytes(raw), "the loop stamp names the rule"
    assert _placed_count(db_path, actor_id) == 1, "the forward keeps the local copy"

    # The forwarded copy comes back to us: our own stamp is on it.
    stamp = f"X-Fauna-Forwarded-By: actor={actor_id.hex()}; t=1; rule={rule_id}"
    _deliver_rcpts(mta.mx_port, mta.domain, "someone@origin.test", [addr],
                   "fwdloop second pass", extra_headers=[stamp])
    assert _placed_count(db_path, actor_id) == 2, (
        "a looped message must still be delivered locally")
    assert len(_forward_rows_to(db_path, destination)) == 1, (
        "a message carrying our own forward stamp must not be forwarded again")


@pytest.mark.feature("mail-filter-rules")
def test_forward_rule_redirect_forwards_without_local_copy(mail_bridge_mta, nest_instance):
    """A `redirect` forward rule forwards the message (one forwarded row,
    `copy_mode=redirect`, attributed to the rule owner) and places NOTHING in
    the owner's mailbox — the forward's durable enqueue is the delivery. The
    same message arriving back with our own loop stamp has its forward
    suppressed, and then lands locally instead: suppression costs the redirect,
    never the mail (mail-forwarding.md § Per-rule "forward to", the redirect
    paragraph; the `copy` half of outcome 17 is
    `test_forward_rule_loop_suppressed_keeps_local_copy`)."""
    mta = mail_bridge_mta
    db_path = nest_instance["db_path"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    destination = f"fwd-{secrets.token_hex(4)}@downstream.test"
    rule_id = _create_forward_rule(nest_instance["url"], recipient, actor_id,
                                   "fwdredirect", destination, redirect=True)

    _deliver_rcpts(mta.mx_port, mta.domain, "someone@origin.test", [addr],
                   "fwdredirect first pass")
    rows = _forward_rows_to(db_path, destination)
    assert len(rows) == 1, f"a fired redirect rule must forward once; rows={len(rows)}"
    rule_col, fwd_actor, original_sender, raw = rows[0]
    assert rule_col == str(rule_id), f"forward_rule_id: {rule_col!r}, want {rule_id}"
    assert bytes(fwd_actor) == actor_id, "the forward is attributed to the rule's owner"
    assert original_sender == "someone@origin.test", original_sender
    assert f"rule={rule_id}".encode() in bytes(raw), "the loop stamp names the rule"
    assert _placed_count(db_path, actor_id) == 0, "redirect keeps no local copy"

    # The forwarded copy comes back carrying our own stamp: the forward is
    # suppressed, so the message must land locally rather than vanish.
    stamp = f"X-Fauna-Forwarded-By: actor={actor_id.hex()}; t=1; rule={rule_id}"
    _deliver_rcpts(mta.mx_port, mta.domain, "someone@origin.test", [addr],
                   "fwdredirect second pass", extra_headers=[stamp])
    assert len(_forward_rows_to(db_path, destination)) == 1, (
        "a message carrying our own forward stamp must not be forwarded again")
    assert _placed_count(db_path, actor_id) == 1, (
        "a redirect whose forward was suppressed must fall back to local delivery")


@pytest.mark.feature("mail-filter-rules")
def test_forward_rule_never_forwards_a_bounce(mail_bridge_mta, nest_instance):
    """A null-sender message (a bounce) that matches a forward rule is delivered
    locally but never forwarded (§ Architectural rules — forward attempts don't
    run on null-sender messages; § Don't do these)."""
    mta = mail_bridge_mta
    db_path = nest_instance["db_path"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    destination = f"fwd-{secrets.token_hex(4)}@downstream.test"
    _create_forward_rule(nest_instance["url"], recipient, actor_id, "fwdbounce", destination)

    _deliver_rcpts(mta.mx_port, mta.domain, "", [addr], "fwdbounce delivery failure",
                   header_from="MAILER-DAEMON@bounce.test")
    assert _placed_count(db_path, actor_id) == 1, "the bounce is delivered locally"
    assert _forward_rows_to(db_path, destination) == [], (
        "a null-sender message must never be forwarded")


# ── FileInto guard (email-filters.md § Email filter rules) ──────────────────


@pytest.mark.feature("mail-filter-rules")
def test_file_into_sent_drafts_or_held_refused_at_create_and_placement(
        mail_bridge_mta, nest_instance):
    """No rule ever files inbound mail into Sent, Drafts or the guardian's held
    mailbox: creating such a rule is refused (`invalid_params`), and a rule that
    reached the store anyway (stored before the check, or carried across a
    succession) is re-screened at placement — the message falls back to its
    disposition mailbox (INBOX at the fixture's score 0), never the forbidden one."""
    mta = mail_bridge_mta
    db_path = nest_instance["db_path"]
    recipient, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    ws = WsRpcAdminClient(nest_instance["url"], actor_id=actor_id,
                          signing_key=bytes(recipient["signing_key"]))
    with ws:
        for forbidden in ("Sent", "Drafts", "Guardian Review"):
            with pytest.raises(RpcCallError) as exc:
                ws.call("fauna.email.filters.create", {
                    "name": f"into {forbidden}",
                    "rules": [{"SenderDomain": {"domain": "intosent.test"}}],
                    "combination": "all",
                    "action": {"FileInto": {"mailbox": forbidden}},
                    "priority": 0,
                })
            assert exc.value.code == "fauna.email.invalid_params", (
                f"FileInto {forbidden!r} must be refused at create; got {exc.value.code}")
        created = ws.call("fauna.email.filters.create", {
            "name": "into Archive",
            "rules": [{"SenderDomain": {"domain": "intosent.test"}}],
            "combination": "all",
            "action": {"FileInto": {"mailbox": "Archive"}},
            "priority": 0,
        })

    # A rule stored before the create-time check: rewrite the valid row's
    # action in place to the forbidden target, bypassing the create path.
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute("UPDATE email_filters SET action = 'fileinto:Sent' WHERE id = ?",
                     (created["id"],))
        conn.commit()
    finally:
        conn.close()

    _deliver(mta.mx_port, mta.domain, "someone@intosent.test", addr, "file me into Sent")
    placed = _placements(db_path, actor_id)
    assert placed.get("intosent.test") == "INBOX", (
        f"a stored FileInto Sent rule must fall back to disposition placement; "
        f"placements={placed}")


@pytest.mark.feature("mail-filter-rules")
def test_forward_all_to_a_hosted_domain_is_refused(mail_bridge_mta, nest_instance):
    """Forward-all pointed at a domain this nest hosts is refused and nothing is
    stored — the user should add an alias instead (mail-forwarding.md § Don't do
    these); an external target on the same call path is accepted."""
    mta = mail_bridge_mta
    recipient, actor_id, _ = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    ws = WsRpcAdminClient(nest_instance["url"], actor_id=actor_id,
                          signing_key=bytes(recipient["signing_key"]))
    with ws:
        with pytest.raises(RpcCallError) as e:
            ws.call("fauna.bridges.set_forward_all_to",
                    {"forward_all_to": f"someone@{mta.domain}"})
        # The refusal's own code is what carries the add-an-alias remedy: an app
        # renders the code's sentence, never `details`.
        assert e.value.code == "fauna.bridges.forward_target_on_local_domain", (
            f"the refusal must be the hosted-domain code; got {e.value!r}")
        assert ws.call("fauna.bridges.get_forward_all_to", {}).get("forward_all_to") is None
        ws.call("fauna.bridges.set_forward_all_to",
                {"forward_all_to": "me-elsewhere@downstream.test"})
        assert ws.call("fauna.bridges.get_forward_all_to", {})["forward_all_to"] == (
            "me-elsewhere@downstream.test")
        ws.call("fauna.bridges.set_forward_all_to", {"forward_all_to": None})


@pytest.mark.feature("spam")
def test_shared_rules_spam_goes_to_junk_untrained_never_refused(mail_bridge_mta, nest_instance):
    """Mail the deployment's shared rules score as spam is accepted and filed to
    Junk for a user who has trained nothing — never refused at the door, never
    held back — and the filing teaches the user's own filter nothing
    (`smtp-server.md` § Spam handling (user-visible)). A benign message to the
    same user is the control that lands in the inbox."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    _, actor_id, addr = _provision_recipient(
        nest_instance["port"], nest_instance["url"], nest_instance["admin"], mta.domain)
    # 20 on rspamd's native scale — past the default spam-folder tier (5 of 15).
    _deliver_rcpts(mta.mx_port, mta.domain, "promo@shared-spam.test", [addr],
                   "shared rules spam", extra_headers=["X-Test-Rspamd-Score: 20"],
                   expect_data_code="250")
    _deliver_rcpts(mta.mx_port, mta.domain, "friend@benign.test", [addr], "benign")
    placed = _placements(db, actor_id)
    assert placed.get("shared-spam.test") == "Junk", (
        f"shared-rules spam must be accepted into Junk; placements {placed}")
    assert placed.get("benign.test") == "INBOX", f"the control lands in INBOX; {placed}"
    conn = sqlite3.connect(db, timeout=10.0)
    try:
        trained = conn.execute(
            "SELECT (SELECT COUNT(*) FROM spam_training_history WHERE actor_id = ?1)"
            " + (SELECT COUNT(*) FROM spam_models WHERE actor_id = ?1)", (actor_id,),
        ).fetchone()[0]
    finally:
        conn.close()
    assert trained == 0, "a shared-rules filing must not train the user's own filter"

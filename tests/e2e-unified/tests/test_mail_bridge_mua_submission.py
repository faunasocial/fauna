"""Tier 3: what a standard mail app sees when it SENDS — over the real MTA
submission listener (`mail_bridge_mta`, port 465) and, where the outcome is
visible in a mailbox, read back over the real MDA (`mail_bridge_mda`), both
bridges on the same nest and the same primary domain
(`docs/goal/behavior/smtp-server.md` § Recipient handling on submission,
§ First-party client send + receive, § Auth on each port, § Outbound submission
flow; `docs/goal/behavior/mail-mass-mailing.md` § Pattern).

Every sender here is a fresh user holding both halves a real mail app needs: an
IMAP credential (`_provision_msek_recipient`) and a submission token sealed
under the same password (what the primary client uploads). Each assertion reads
a reply the server wrote for this transaction, a mailbox the tagged SELECT
settled, or a counter row the reply follows — convention 14.
"""

import base64
import json
import secrets
import sqlite3
import time
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from conftest import SUBMISSION_TOKEN_LIFETIME_S, _provision_msek_recipient, _run_seal_helper
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.caldav_client import CalDAVClient, build_invite_vevent
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _find_tagged,
    _imap_auth_plain,
    _imap_cmd,
    _imap_uid_fetch_body,
    _imap_uid_search,
    _imaps_connect,
    _wait_for_tagged,
    _poke_outbound_bridge,
    dkim_signature_tag,
    verify_each_dkim_signature,
    outbound_clock,
    outbound_clock_now,
    restore_outbound_clock,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3


def _mail_user(nest_instance, seal_helper_binary, domain, *, max_recipients=100):
    """A fresh user who can both read (IMAP) and send (submission) with one
    password. The local part is the actor's handle, so the app's own send path
    (`fauna.email.send`) accepts the same From: address."""
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    password = f"mua-{secrets.token_hex(6)}"
    r = _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary, domain=domain,
        local_part=actor["handle"], password=password, actor=actor)
    now = int(time.time())
    token = _run_seal_helper(seal_helper_binary, "seal-submission-token", {
        "signing_seed_b64": base64.b64encode(bytes(actor["signing_key"])).decode(),
        "credential_id": "default",
        "credential_kind": "plain",
        "credential_b64": base64.b64encode(password.encode()).decode(),
        "issued_at": now,
        "expires_at": now + SUBMISSION_TOKEN_LIFETIME_S,
        "max_recipients": max_recipients,
        "max_messages_per_day": 1000,
    })
    with _user_ws(nest_instance, r) as ws:
        ws.provision_wrapped_submission_token(token)
    return r


def _user_ws(nest_instance, r):
    return WsRpcAdminClient(nest_instance["url"], actor_id=r.actor_id,
                            signing_key=bytes(r.recipient["signing_key"]))


def _admin_ws(nest_instance):
    sk = nest_instance["admin"]["signing_key"]
    return WsRpcAdminClient(nest_instance["url"], actor_id=bytes(sk.verify_key),
                            signing_key=bytes(sk))


def _auth_line(username, password):
    return "AUTH PLAIN " + base64.b64encode(
        b"\x00" + username.encode() + b"\x00" + password.encode()).decode()


def _message(sender, rcpts, token):
    return ("\r\n".join([
        f"From: <{sender}>",
        f"To: {', '.join(rcpts)}",
        f"Subject: mua {token}",
        f"Message-ID: <{token}@mua.test>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"body {token}",
    ]) + "\r\n").encode()


def _submit(mta, sender, password, rcpts, token, *, rcpt_codes=None):
    """One authenticated submission. `rcpt_codes` (default all "250") is the
    reply each RCPT must get; DATA runs when any RCPT was accepted."""
    rcpt_codes = rcpt_codes or ["250"] * len(rcpts)
    deadline = time.monotonic() + 45.0
    replies = []
    with _connect_submission_tls(mta.submission_port_465, mta.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {mta.domain}", "250", deadline)
        conn.cmd(_auth_line(sender, password), "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        for rcpt, code in zip(rcpts, rcpt_codes):
            replies.append(conn.cmd(f"RCPT TO:<{rcpt}>", code, deadline))
        if "250" in rcpt_codes:
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(_message(sender, rcpts, token))
            conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)
    return replies


def _imap_find(mda, r, mailbox, token):
    """SELECT `mailbox` as `r` and return (uids matching `token`, the first
    match's flags + body). BODY, not SUBJECT: mail rests sealed, so the nest
    keeps no subject column to match against and a header-axis SEARCH on it is
    a no-match by design (imap-server.md § SEARCH → Search-axis degradation);
    the body axis opens the
    sealed index hint, which carries the body's `token`."""
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(mda, deadline)
    with sock:
        assert _imap_auth_plain(sock, buf, "f1", r.username, r.password, deadline) == "OK"
        status, _ = _imap_cmd(sock, buf, "f2", f"SELECT {mailbox}", deadline)
        assert status == "OK", f"SELECT {mailbox} must succeed; got {status}"
        status, hits = _imap_uid_search(sock, buf, "f3", f'BODY "{token}"', deadline)
        assert status == "OK"
        flags = body = None
        if hits:
            uid = min(hits)
            _, flags = _imap_cmd(sock, buf, "f4", f"UID FETCH {uid} (FLAGS)", deadline)
            _, body = _imap_uid_fetch_body(sock, buf, "f5", uid, deadline)
        sock.sendall(b"f6 LOGOUT\r\n")
    return hits, flags, body


@pytest.mark.feature("standard-mail-apps")
def test_mail_app_send_is_saved_to_sent(mail_bridge_mta, mail_bridge_mda, nest_instance,
                                        seal_helper_binary):
    """A message sent from a mail app lands in the sender's Sent folder, read and
    readable, where every mail app sees it."""
    u = _mail_user(nest_instance, seal_helper_binary, mail_bridge_mta.domain)
    token = f"sent{secrets.token_hex(4)}"
    _submit(mail_bridge_mta, u.username, u.password, ["friend@external.test"], token)
    hits, flags, body = _imap_find(mail_bridge_mda, u, "Sent", token)
    assert hits, "the mail-app send must be saved in Sent"
    assert "\\Seen" in flags, f"the Sent copy is already read; got {flags!r}"
    assert token.encode() in body, "the Sent copy must open to the message sent"


@pytest.mark.feature("standard-mail-apps")
def test_mail_app_send_to_a_local_user_lands_in_their_inbox(
        mail_bridge_mta, mail_bridge_mda, nest_instance, seal_helper_binary):
    """Sent from a mail app to another user on this nest, the message lands in
    their inbox straight away and never leaves for the outside."""
    sender = _mail_user(nest_instance, seal_helper_binary, mail_bridge_mta.domain)
    rcpt = _mail_user(nest_instance, seal_helper_binary, mail_bridge_mta.domain)
    token = f"local{secrets.token_hex(4)}"
    _submit(mail_bridge_mta, sender.username, sender.password, [rcpt.username], token)
    hits, _, body = _imap_find(mail_bridge_mda, rcpt, "INBOX", token)
    assert hits and token.encode() in body, "the local recipient's INBOX must hold the message"
    assert _find_tagged(mail_bridge_mta.stub_mx, token) is None, (
        "mail between two users of this nest must not be relayed to an outside MX")


@pytest.mark.feature("standard-mail-apps")
def test_an_oversize_mail_app_send_is_refused_at_once(mail_bridge_mta, nest_instance,
                                                      seal_helper_binary):
    """A message larger than the nest accepts is refused `552` while the mail app
    is still sending — at `MAIL FROM` with its declared SIZE — and nothing goes
    out; no bounce arrives later."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    deadline = time.monotonic() + 45.0
    with _connect_submission_tls(mta.submission_port_465, mta.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {mta.domain}", "250", deadline)
        conn.cmd(_auth_line(u.username, u.password), "235", deadline)
        reply = conn.cmd(f"MAIL FROM:<{u.username}> SIZE=200000000", "552", deadline)
        assert "5.3.4" in str(reply), f"the refusal must say the message is too big; got {reply!r}"
        conn.cmd("QUIT", "221", deadline)


def _used_today(db_path, actor_id):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT used FROM bridge_submission_quota WHERE actor_id = ? AND day_bucket = ?",
            (actor_id, int(time.time()) // 86400)).fetchone()
        return row[0] if row else 0
    finally:
        conn.close()


@pytest.mark.feature("standard-mail-apps")
def test_recipient_cap_and_daily_allowance_shared_with_app_sends(
        mail_bridge_mta, nest_instance, seal_helper_binary):
    """A mail app is told `452 4.7.12` when a message names more recipients than
    allowed, and when today's sending allowance is used up — and that allowance
    is one pool with the Fauna app's own sends, drawn from either side."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain, max_recipients=2)
    token = f"cap{secrets.token_hex(4)}"
    ext = [f"r{i}-{token}@external.test" for i in range(3)]
    replies = _submit(mta, u.username, u.password, ext, token,
                      rcpt_codes=["250", "250", "452"])
    assert "4.7.12" in str(replies[2]), f"the third recipient must be refused 4.7.12; got {replies[2]!r}"

    def app_send(tag):
        with _user_ws(nest_instance, u) as ws:
            return ws.call("fauna.email.send", {
                "recipients": [f"app-{tag}@external.test"],
                "raw_rfc5322": _message(u.username, [f"app-{tag}@external.test"], tag),
            })

    db = nest_instance["db_path"]
    used = _used_today(db, u.actor_id)
    try:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {"max_per_day": used + 1})
        # The mail app spends the last unit; the app's own send is then refused.
        _submit(mta, u.username, u.password, [f"one-{token}@external.test"], token + "a")
        with pytest.raises(RpcCallError) as e:
            app_send(token + "b")
        assert e.value.code == "fauna.email.rate_limited", (
            f"an app send past the shared allowance must be refused; got {e.value.code!r}")

        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {"max_per_day": used + 2})
        # The app spends the next unit; the mail app is then refused.
        app_send(token + "c")
        replies = _submit(mta, u.username, u.password, [f"two-{token}@external.test"],
                          token + "d", rcpt_codes=["452"])
        assert "4.7.12" in str(replies[0]), (
            f"a mail-app send past the allowance the app used must be refused 4.7.12; "
            f"got {replies[0]!r}")
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {})


@pytest.mark.feature("standard-mail-apps")
def test_a_multi_recipient_mail_app_send_spends_one_unit_per_outside_recipient(
        mail_bridge_mta, nest_instance, seal_helper_binary):
    """One message from a mail app to several people spends one unit of today's
    sending allowance per recipient outside this nest — no more for the later
    recipients, and nothing for a recipient on this nest — and a send from the
    Fauna app to the same kind of mix is counted the same way."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    neighbour = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    token = f"multi{secrets.token_hex(4)}"
    db = nest_instance["db_path"]
    before = _used_today(db, u.actor_id)

    outside = [f"r{i}-{token}@external.test" for i in range(3)]
    _submit(mta, u.username, u.password, outside + [neighbour.username], token)
    after_mail_app = _used_today(db, u.actor_id)
    assert after_mail_app - before == 3, (
        f"one mail-app message to 3 outside recipients and 1 neighbour must spend 3 units; "
        f"spent {after_mail_app - before}")

    app_rcpts = [f"app{i}-{token}@external.test" for i in range(2)] + [neighbour.username]
    with _user_ws(nest_instance, u) as ws:
        ws.call("fauna.email.send", {
            "recipients": app_rcpts,
            "raw_rfc5322": _message(u.username, app_rcpts, token + "app"),
        })
    after_app = _used_today(db, u.actor_id)
    assert after_app - after_mail_app == 2, (
        f"one app send to 2 outside recipients and 1 neighbour must spend 2 units; "
        f"spent {after_app - after_mail_app}")


@pytest.mark.feature("mail-server")
def test_an_app_send_to_an_outside_recipient_arrives_dkim_signed(
        mail_bridge_mta, nest_instance, seal_helper_binary):
    """A message sent from the Fauna app to someone outside this nest arrives
    signed twice for the sender's domain (RSA and Ed25519), and each signature
    verifies against the DKIM record the nest publishes for it."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    token = f"appdkim{secrets.token_hex(4)}"
    rcpt = f"app-{token}@external.test"
    with _user_ws(nest_instance, u) as ws:
        ws.call("fauna.email.send", {
            "recipients": [rcpt],
            "raw_rfc5322": _message(u.username, [rcpt], token),
        })

    received = _wait_for_tagged(mta.stub_mx, token, timeout=30.0)
    assert received is not None, (
        f"the outside mail server received no message tagged {token} within 30s "
        f"(bridge log: {mta.log_file})")

    # Answer only for the aligned `<selector>._domainkey.<domain>.` names, so a
    # signature under any other domain or selector cannot verify.
    verified, seen_queries = verify_each_dkim_signature(
        received, mta.domain, mta.dkim_records)
    assert verified == list(mta.dkim_records), (
        "a message sent from the app must arrive with two DKIM signatures that "
        f"each verify against their published record; verified {verified!r}, "
        f"dnsfunc queries: {seen_queries!r}. First 900 bytes:\n{received[:900]!r}")
    assert dkim_signature_tag(received, "d") == mta.domain, (
        f"the signing domain must be the sender's domain {mta.domain!r}; "
        f"got {dkim_signature_tag(received, 'd')!r}")


@pytest.mark.feature("standard-mail-apps")
def test_repeated_wrong_passwords_are_briefly_refused(mail_bridge_mta, nest_instance,
                                                      seal_helper_binary):
    """After the allowed number of wrong passwords within a minute, the next
    sign-in is refused `421` — even with the right password — while another user
    signs in normally."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    bystander = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    budget = 3

    def auth(username, password):
        deadline = time.monotonic() + 45.0
        with _connect_submission_tls(mta.submission_port_465, mta.domain) as conn:
            conn.expect("220", deadline)
            conn.cmd(f"EHLO {mta.domain}", "250", deadline)
            return str(conn.cmd(_auth_line(username, password), "", deadline))

    try:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_auth_policy", {"max_auth_failures_per_minute": budget})
        # The policy reaches the bridge asynchronously and rebuilds its counters
        # on arrival, so count the refusals the bridge actually makes: at most
        # the old default budget (30) plus ours before the lockout shows.
        replies = []
        for _ in range(30 + budget + 1):
            reply = auth(u.username, "wrong-password")
            replies.append(reply[:3])
            if reply.startswith("421"):
                break
        assert replies[-1] == "421", f"repeated wrong passwords must lock out; got {replies}"
        assert replies[-1 - budget:-1] == ["535"] * budget, (
            f"the lockout must follow {budget} consecutive refusals; got {replies}")
        right = auth(u.username, u.password)
        assert right.startswith("421") and "4.7.0" in right, (
            f"while locked out even the right password is refused 421 4.7.0; got {right!r}")
        assert auth(bystander.username, bystander.password).startswith("235"), (
            "another user's sign-in is unaffected")
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_auth_policy", {})


@pytest.mark.feature("mailing-lists")
def test_sending_as_the_list_from_a_mail_app_is_refused(mail_bridge_mta, nest_instance,
                                                        seal_helper_binary):
    """The list's owner, signed in to a mail app, cannot send with the list's
    address as the sender; issues go out only through the list send."""
    mta = mail_bridge_mta
    owner = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    local = f"news-{secrets.token_hex(4)}"
    with _user_ws(nest_instance, owner) as ws:
        ws.call("fauna.bridges.create_account_list", {"local_part": local,
                                                      "local_domain": mta.domain})
    list_addr = f"{local}@{mta.domain}"
    deadline = time.monotonic() + 45.0
    with _connect_submission_tls(mta.submission_port_465, mta.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {mta.domain}", "250", deadline)
        conn.cmd(_auth_line(owner.username, owner.password), "235", deadline)
        reply = str(conn.cmd(f"MAIL FROM:<{list_addr}>", "", deadline))
        assert reply.startswith("5"), (
            f"a mail-app send as the list's address must be refused permanently; got {reply!r}")
        conn.cmd("QUIT", "221", deadline)


# ── The fresh-IP warm-up (`mail-deliverability.md` § Enforcement at submission
# time) ───────────────────────────────────────────────────────────────────────
#
# The cap is the deployment IP's, so every door a user's own act sends mail out
# through spends one deployment-wide allowance, and mail the nest writes itself
# never does. Neither the day nor the ramp is real: the admin's own
# `outbound_warmup_reset` puts the nest on day 1 with nothing spent, and
# `POST /api/v1/test/outbound/clock` freezes and moves the outbound clock every
# queue time is read on. Both are state on a session-scoped nest, so each
# witness puts the clock back as it found it and resets the warm-up on the
# way out.

_DAY = 86_400


def _queue_rows(nest_url, recipients):
    """{recipient: queue row} for the outbound-queue rows addressed to any of
    `recipients` (scheduling state only — never bodies)."""
    req = urllib.request.Request(f"{nest_url}/api/v1/test/outbound/queue", method="GET")
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        rows = json.loads(resp.read().decode())["rows"]
    wanted = set(recipients)
    return {r["recipient"]: r for r in rows if r["recipient"] in wanted}


def _warmup(nest_instance, method="fauna.bridges.outbound_warmup_status"):
    with _admin_ws(nest_instance) as ws:
        return ws.call(method, {})


def _start_warmup_day_one(nest_instance, mta):
    """Freeze the outbound clock and restart the ramp: day 1, nothing spent.
    Returns (frozen now, today's cap)."""
    now = outbound_clock(mta.nest_url, advance=0)
    status = _warmup(nest_instance, "fauna.bridges.outbound_warmup_reset")
    assert status["current_day"] == 1 and status["today_used"] == 0, status
    assert status["today_max"], f"day 1 of the ramp must be capped; got {status!r}"
    return now, status["today_max"]


def _app_send(nest_instance, u, rcpts, token):
    with _user_ws(nest_instance, u) as ws:
        return ws.call("fauna.email.send", {
            "recipients": rcpts, "raw_rfc5322": _message(u.username, rcpts, token),
        })


def _inbox_count(db_path, actor_id):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        (n,) = conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ? AND mailbox = 'INBOX'",
            (actor_id,)).fetchone()
        return n
    finally:
        conn.close()


@pytest.mark.feature("mail-server")
def test_mail_over_the_warmup_allowance_is_accepted_and_delivered_the_next_day(
        mail_bridge_mta, nest_instance, seal_helper_binary):
    """On a new server, mail beyond the day's warm-up allowance is accepted —
    from a mail app and from the Fauna app alike — held until the next 00:00 UTC
    and delivered then, never bounced. Both doors spend the one allowance: a
    mail app's send fills it, and the Fauna app's next send is the one held."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    token = f"warm{secrets.token_hex(4)}"
    prior_clock = outbound_clock_now(mta.nest_url)
    try:
        now, cap = _start_warmup_day_one(nest_instance, mta)
        fill = [f"fill{i}-{token}@external.test" for i in range(cap)]
        _submit(mta, u.username, u.password, fill, token + "fill")
        spent = _warmup(nest_instance)
        assert spent["today_used"] == cap, (
            f"a mail app's {cap}-recipient send spends the whole day-1 allowance; got {spent!r}")

        # Past the allowance, through both doors: each is accepted as usual.
        app_rcpt, smtp_rcpt = f"app-{token}@external.test", f"smtp-{token}@external.test"
        reply = _app_send(nest_instance, u, [app_rcpt], token + "app")
        assert reply["remote_queued"] == 1 and not reply["remote_errors"], (
            f"the Fauna app's over-allowance send is accepted with its queued count; got {reply!r}")
        _submit(mta, u.username, u.password, [smtp_rcpt], token + "smtp")  # 250 at the dot

        midnight = (now // _DAY + 1) * _DAY
        held = _queue_rows(mta.nest_url, [app_rcpt, smtp_rcpt])
        for rcpt in (app_rcpt, smtp_rcpt):
            row = held.get(rcpt)
            assert row is not None, f"the over-allowance message to {rcpt} was not queued: {held!r}"
            assert (row["status"], row["attempt_count"], row["next_attempt_at"]) == (
                "pending", 0, midnight), (
                f"the over-allowance message to {rcpt} must wait, untried, for the next "
                f"00:00 UTC ({midnight}); its row is {row!r}")
        assert _warmup(nest_instance)["today_used"] == cap, "held mail spends nothing today"

        # The next day: both leave, and nothing bounces.
        outbound_clock(mta.nest_url, set=midnight + 60)
        assert _poke_outbound_bridge(mta.nest_url) > 0
        for tag in (token + "app", token + "smtp"):
            wait_until(lambda tag=tag: _find_tagged(mta.stub_mx, tag), MAIL_OUTBOUND_CYCLE_S,
                       interval=0.5,
                       diagnose=lambda: f"the held message {tag} never reached the outside "
                       f"server the next day; queue: {_queue_rows(mta.nest_url, [app_rcpt, smtp_rcpt])!r} "
                       f"(bridge log: {mta.log_file})")
        sent = wait_until(
            lambda: (lambda rows: rows if all(r["status"] == "sent" for r in rows.values())
                     and len(rows) == 2 else None)(_queue_rows(mta.nest_url, [app_rcpt, smtp_rcpt])),
            MAIL_OUTBOUND_CYCLE_S, interval=0.5,
            diagnose=lambda: f"the held rows did not end sent: "
            f"{_queue_rows(mta.nest_url, [app_rcpt, smtp_rcpt])!r}")
        assert all(r["status"] == "sent" for r in sent.values()), sent
    finally:
        restore_outbound_clock(mta.nest_url, prior_clock)
        _warmup(nest_instance, "fauna.bridges.outbound_warmup_reset")


@pytest.mark.feature("mail-server")
def test_mail_app_send_wider_than_the_warmup_allowance_reads_back_held_on_sent(
        mail_bridge_mta, nest_instance, seal_helper_binary):
    """A mail app's one submission wider than the day's warm-up allowance reads
    back, through the sender's own `fauna.email.sent.fetch`, as held until the
    next 00:00 UTC. The Sent copy and the queued message reach the nest in two
    separate bridge calls, so this holds only while both carry the same
    message (mail-deliverability.md § Enforcement at submission time)."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    token = f"held{secrets.token_hex(4)}"
    prior_clock = outbound_clock_now(mta.nest_url)
    try:
        now, cap = _start_warmup_day_one(nest_instance, mta)
        rcpts = [f"h{i}-{token}@external.test" for i in range(cap + 1)]
        _submit(mta, u.username, u.password, rcpts, token)  # 250 at the dot
        assert _warmup(nest_instance)["today_used"] == 0, (
            "a submission wider than the whole allowance is held entire, spending nothing")

        def sent_copy():
            with _user_ws(nest_instance, u) as ws:
                messages = ws.call("fauna.email.sent.fetch",
                                   {"after_uid": 0, "limit": 50})["messages"]
            return messages or None

        messages = wait_until(
            sent_copy, MAIL_OUTBOUND_CYCLE_S, interval=0.5,
            diagnose=lambda: f"the submission's Sent copy never reached the sender's Sent "
            f"read (bridge log: {mta.log_file})")
        assert len(messages) == 1, f"one submission files one Sent copy: {messages!r}"
        hold = messages[0].get("warmup_hold")
        assert hold, (
            f"the {cap + 1}-recipient submission is past the day-1 allowance of {cap}, so "
            f"its Sent copy reads back held; got {messages[0]!r} (queue: "
            f"{_queue_rows(mta.nest_url, rcpts[:1])!r})")
        assert hold["release_at"] == (now // _DAY + 1) * _DAY, (
            f"held until the next 00:00 UTC after {now}: {hold!r}")
    finally:
        restore_outbound_clock(mta.nest_url, prior_clock)
        _warmup(nest_instance, "fauna.bridges.outbound_warmup_reset")


@pytest.mark.feature("mail-server")
def test_bounces_auto_replies_and_invitations_leave_with_the_warmup_spent(
        mail_bridge_mta, mail_bridge_mda, nest_instance, seal_helper_binary):
    """Mail the nest writes on its own is never held by the warm-up: with the
    day's allowance spent, a bounce, an automatic reply and a calendar
    invitation each leave at once, and none of them is counted."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    token = f"exempt{secrets.token_hex(4)}"
    db = nest_instance["db_path"]
    with _user_ws(nest_instance, u) as ws:
        ws.call("fauna.email.filters.create", {
            "name": "away",
            "rules": [{"SenderDomain": {"domain": "external.test"}}],
            "combination": "all",
            "action": {"AutoReply": {
                "subject": f"Away {token}", "body": f"Back soon {token}", "interval_hours": 24,
            }},
            "priority": 0,
        })
    prior_clock = outbound_clock_now(mta.nest_url)
    try:
        now, cap = _start_warmup_day_one(nest_instance, mta)
        inbox_before = _inbox_count(db, u.actor_id)

        # Spend the whole allowance in one app send; one recipient the outside
        # server refuses for good, so its bounce is written with nothing left.
        bad = f"nonexistent-{token}@external.test"
        fill = [f"fill{i}-{token}@external.test" for i in range(cap - 1)] + [bad]
        reply = _app_send(nest_instance, u, fill, token + "fill")
        assert reply["remote_queued"] == cap, reply
        assert _warmup(nest_instance)["today_used"] == cap, "the allowance is spent"

        # The bounce: the refused recipient's row ends bounced and the notice
        # reaches the sender (in-domain, so it is sealed into their INBOX).
        assert _poke_outbound_bridge(mta.nest_url) > 0
        bounced = wait_until(
            lambda: (_queue_rows(mta.nest_url, [bad]).get(bad) or {}).get("status") == "bounced"
            and _inbox_count(db, u.actor_id) > inbox_before,
            MAIL_OUTBOUND_CYCLE_S, interval=0.5,
            diagnose=lambda: f"no bounce reached the sender: row "
            f"{_queue_rows(mta.nest_url, [bad])!r}, INBOX {inbox_before} → "
            f"{_inbox_count(db, u.actor_id)} (bridge log: {mta.log_file})")
        assert bounced

        # The automatic reply: mail from outside triggers it, and it is due now.
        boss = f"boss-{token}@external.test"
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{boss}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{u.username}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(_message(boss, [u.username], token + "in"))
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # The invitation: the user, as organizer in a calendar app, invites
        # someone outside; the nest mails the invitation.
        attendee = f"att-{token}@external.test"
        uid = f"warm-{token}"
        cal = CalDAVClient(f"https://127.0.0.1:{mail_bridge_mda.caldav_port}",
                           u.username, u.password, verify=False)
        cal.wait_until_serving()
        cal.put_event(cal.personal_calendar(), uid, build_invite_vevent(
            uid, f"Planning {token}", "20361012T150000Z", "20361012T160000Z",
            u.username, [attendee]))

        rows = wait_until(
            lambda: (lambda r: r if boss in r and attendee in r else None)(
                _queue_rows(mta.nest_url, [boss, attendee])),
            MAIL_OUTBOUND_CYCLE_S, interval=0.5,
            diagnose=lambda: f"the automatic reply or the invitation was never queued: "
            f"{_queue_rows(mta.nest_url, [boss, attendee])!r} (bridge log: {mta.log_file})")
        for rcpt, row in rows.items():
            assert row["next_attempt_at"] <= now, (
                f"mail the nest writes itself must leave now, not wait for tomorrow; "
                f"the row to {rcpt} is {row!r} (frozen now {now})")
        assert _poke_outbound_bridge(mta.nest_url) > 0
        for tag in (f"Away {token}", f"UID:{uid}"):
            wait_until(lambda tag=tag: _find_tagged(mta.stub_mx, tag), MAIL_OUTBOUND_CYCLE_S,
                       interval=0.5,
                       diagnose=lambda: f"{tag!r} never reached the outside server; queue: "
                       f"{_queue_rows(mta.nest_url, [boss, attendee])!r}")

        assert _warmup(nest_instance)["today_used"] == cap, (
            "a bounce, an automatic reply and an invitation are never counted")
    finally:
        restore_outbound_clock(mta.nest_url, prior_clock)
        _warmup(nest_instance, "fauna.bridges.outbound_warmup_reset")


@pytest.mark.feature("standard-mail-apps")
def test_a_mail_app_sending_while_the_nest_is_down_is_told_to_try_again(restartable_mta_nest):
    """standard-mail-apps 22: a mail app mid-send when the nest becomes
    unavailable is told to try again (`4xx`), never refused (`5xx`) and never
    told the message was accepted. The app signs in and names an outside
    recipient while the nest is up; the nest stops (its own process, through the
    harness — the mail bridge keeps running); the next recipient and the end of
    the message are each answered `4xx`. The nest comes back, the app sends the
    same message again, and exactly one copy leaves
    (`smtp-server.md` § Recipient handling on submission: a transport failure
    tempfails)."""
    from common.nest import start_nest_in_place, stop_nest
    from helpers.budgets import MAIL_BRIDGE_RECONNECT_S
    from helpers.mail_wire import _read_final_reply

    venue = restartable_mta_nest
    sender = f"{venue.submission_sender_local}@{venue.domain}"
    password = venue.submission_credential.decode()
    token = f"nestdown{secrets.token_hex(4)}"
    outside = f"{token}@external.test"

    deadline = time.monotonic() + 120.0
    with _connect_submission_tls(venue.submission_port_465, venue.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {venue.domain}", "250", deadline)
        conn.cmd(_auth_line(sender, password), "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{outside}>", "250", deadline)

        stop_nest(venue.nest)
        try:
            conn.send_raw(f"RCPT TO:<{sender}>\r\n".encode())
            rcpt = _read_final_reply(conn, deadline)
            assert rcpt.startswith("4"), (
                f"a recipient the nest must resolve, asked while it is down, must be "
                f"told to try again; got {rcpt!r}")
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(_message(sender, [outside], token))
            conn.send_raw(b".\r\n")
            end = _read_final_reply(conn, deadline)
            assert end.startswith("4"), (
                f"the message must not be accepted (or refused) while the nest is down; "
                f"got {end!r}")
            conn.cmd("QUIT", "221", deadline)
        finally:
            start_nest_in_place(venue.nest)

    refusals = []

    def _resent() -> bool:
        try:
            _submit(venue, sender, password, [outside], token)
        except AssertionError as exc:  # the bridge is not serving again yet
            refusals.append(str(exc))
            return False
        return True

    wait_until(
        _resent, MAIL_BRIDGE_RECONNECT_S, interval=1.0,
        diagnose=lambda: f"the resend never went through after the nest came back: "
        f"{refusals[-1:]!r} (bridge log: {venue.log_file})")
    _poke_outbound_bridge(venue.nest_url)
    assert _wait_for_tagged(venue.stub_mx, token, timeout=MAIL_OUTBOUND_CYCLE_S) is not None, (
        f"the resent message never left (bridge log: {venue.log_file})")
    copies = [m for m in venue.stub_mx.messages() if token.encode() in m]
    assert len(copies) == 1, f"exactly one copy must leave; got {len(copies)}"


def _imap_mailbox_bodies(mda, r, mailbox):
    """Every message in `mailbox` as `r`, opened (an MSEK user reads plaintext)."""
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(mda, deadline)
    bodies = []
    with sock:
        assert _imap_auth_plain(sock, buf, "b1", r.username, r.password, deadline) == "OK"
        status, _ = _imap_cmd(sock, buf, "b2", f"SELECT {mailbox}", deadline)
        assert status == "OK", f"SELECT {mailbox} must succeed; got {status}"
        status, uids = _imap_uid_search(sock, buf, "b3", "ALL", deadline)
        assert status == "OK"
        for i, uid in enumerate(sorted(uids)):
            _, body = _imap_uid_fetch_body(sock, buf, f"b4{i}", uid, deadline)
            bodies.append(body)
        sock.sendall(b"b5 LOGOUT\r\n")
    return bodies


@pytest.mark.feature("mail-server")
def test_a_bounce_names_the_recipient_and_reason_and_carries_headers_not_body(
        mail_bridge_mta, mail_bridge_mda, nest_instance, seal_helper_binary):
    """A message the receiving server refuses for good comes back to the sender
    as a bounce that names the recipient that failed and what the server said,
    and carries the message's headers without its body (`smtp-server.md`
    § Permanent-failure bounce generation: RFC 3464 multipart/report,
    `Final-Recipient`, `Diagnostic-Code`, the original headers as
    `message/rfc822-headers`).

    The stub MX refuses `nonexistent*` recipients with `550 … User unknown`. The
    sender is on this nest, so the bounce is sealed into their INBOX; a fresh
    user reads it there over IMAP."""
    mta = mail_bridge_mta
    u = _mail_user(nest_instance, seal_helper_binary, mta.domain)
    token = f"o17{secrets.token_hex(4)}"
    bad = f"nonexistent-{token}@external.test"
    _submit(mta, u.username, u.password, [bad], token)
    wait_until(
        lambda: (_queue_rows(mta.nest_url, [bad]).get(bad) or {}).get("status") == "bounced",
        MAIL_OUTBOUND_CYCLE_S, interval=0.5,
        diagnose=lambda: f"the refused message never bounced: {_queue_rows(mta.nest_url, [bad])!r} "
        f"(bridge log: {mta.log_file})")

    # The bounce is sealed before the row is marked bounced, so it is there now.
    bodies = _imap_mailbox_bodies(mail_bridge_mda, u, "INBOX")
    reports = [b for b in bodies
               if b"multipart/report" in b.lower() and token.encode() in b]
    assert len(reports) == 1, (
        f"the sender's INBOX must hold exactly one bounce for this message; got "
        f"{len(reports)} among {len(bodies)} messages")
    dsn = reports[0].decode("utf-8", errors="replace")
    unfolded = dsn.replace("\r\n ", " ").replace("\r\n\t", " ")
    low = unfolded.lower()

    assert "multipart/report" in low and "report-type=delivery-status" in low, (
        f"the bounce must be a delivery-status report:\n{dsn[:800]}")
    assert f"final-recipient: rfc822; {bad}".lower() in low, (
        f"the bounce must name the recipient that failed ({bad}):\n{dsn[:1500]}")
    diag = next((ln for ln in unfolded.splitlines()
                 if ln.lower().startswith("diagnostic-code:")), "")
    assert "User unknown" in diag, (
        f"the bounce must carry what the receiving server said; Diagnostic-Code: {diag!r}")
    assert "message/rfc822-headers" in low, f"the bounce must carry the headers:\n{dsn[:1500]}"
    assert f"Message-ID: <{token}@mua.test>" in unfolded and f"Subject: mua {token}" in unfolded, (
        f"the bounce must carry the original message's headers:\n{dsn}")
    assert f"body {token}" not in unfolded, (
        f"the bounce must not carry the original message's body:\n{dsn}")

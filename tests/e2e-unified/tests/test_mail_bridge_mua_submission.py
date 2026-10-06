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
import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from conftest import _provision_msek_recipient, _run_seal_helper
from helpers.mail_wire import (
    _connect_submission_tls,
    _find_tagged,
    _imap_auth_plain,
    _imap_cmd,
    _imap_uid_fetch_body,
    _imap_uid_search,
    _imaps_connect,
    _wait_for_tagged,
    dkim_signature_tag,
)

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
        "expires_at": now + 86400,
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
    signed for the sender's domain, and the signature verifies against the DKIM
    record the nest publishes for that domain."""
    import dkim  # dkimpy — an independent verifier, never our own signer

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

    # Answer only for the aligned `<selector>._domainkey.<domain>.` name, so a
    # signature under any other domain or selector cannot verify.
    expected_query = f"{mta.dkim_selector}._domainkey.{mta.domain}.".encode()
    seen_queries = []

    def dnsfunc(name, timeout=5):
        seen_queries.append(name)
        return mta.dkim_public_dns_value if name == expected_query else None

    assert dkim.verify(received, dnsfunc=dnsfunc), (
        "a message sent from the app must arrive with a DKIM signature that verifies "
        f"against the published record; dnsfunc queries: {seen_queries!r}. "
        f"First 600 bytes:\n{received[:600]!r}")
    assert expected_query in seen_queries, (
        f"the signature is not under the published selector; queries: {seen_queries!r}")
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

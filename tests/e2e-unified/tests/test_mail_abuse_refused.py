"""tier_3 witnesses for what the mail server refuses with nothing chosen.

The `[nest]` outcomes of `docs/features/mail-abuse-refused.md` that a peer on
the local machine can observe: each test drives raw SMTP at a real Go MTA
bridge in front of a real nest and asserts the reply a sending server would
see (`docs/goal/behavior/smtp-server.md` § Connection-time limits,
§ Architectural rules, § Error / tempfail strategy, § Auth on each port;
`mail-content-scanning.md` § Don't do these).

The peer is loopback, so the outcomes whose checks exempt a loopback peer —
the greeting-name rules, the sender-domain lookup, the blocklist, the per-IP
concurrent cap and the per-IP message rate — are witnessed by the tier_4 suite,
from a sending container on the Docker network, not here.

Most tests share the session's `mail_bridge_mta`. Two boot a bridge of their
own (`disposable_mta_bridge_factory`): the default connection rate, which must
be read from a bridge's boot snapshot with nothing chosen, and the scanner
outage, which needs a bridge whose scanners are unreachable.
"""

import socket
import time

import pytest

from conftest import MTA_FIXTURE_SPAM_POSTURE
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _read_final_reply,
    _smtp_auth_plain,
    _SmtpConn,
)

pytestmark = pytest.mark.tier_3


def _admin_ws(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _message(to_addr: str, *, headers: list[str] | None = None, body: str = "hello") -> bytes:
    """A small benign inbound message; `headers` replaces the default header set."""
    if headers is None:
        headers = [
            "From: External Sender <sender@external.test>",
            f"To: {to_addr}",
            f"Subject: abuse-refused {time.time_ns()}",
            f"Message-ID: <abuse-{time.time_ns()}@external.test>",
            "Date: Thu, 08 Oct 2026 12:00:00 +0000",
        ]
    return ("\r\n".join(headers) + "\r\n\r\n" + body + "\r\n").encode()


def _rcpt(handle) -> str:
    return f"{handle.recipient_local_part}@{handle.domain}"


def _send_data(handle, raw: bytes, deadline: float) -> str:
    """One inbound transaction to the local recipient; returns the reply to `.`."""
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{_rcpt(handle)}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)
        try:
            conn.cmd("QUIT", "221", deadline)
        except (AssertionError, OSError):
            pass  # a refusal may close the connection; the reply is the witness
        return reply


def _ehlo_lines(conn: _SmtpConn, deadline: float) -> list[str]:
    conn.send_raw(b"EHLO external.test\r\n")
    lines = []
    while True:
        line = conn.recv_line(deadline)
        lines.append(line)
        if len(line) < 4 or line[3] != "-":
            return lines


# ── Outcome 12: the default connection rate ─────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_default_connection_rate_tells_the_eleventh_connection_in_a_minute_to_retry(
    disposable_mta_bridge_factory, nest_instance,
):
    """With no connection rate chosen, a bridge admits ten connections from one
    server in a minute and answers the eleventh `421 … try again later`.

    "Nothing chosen" is the spam group put without `max_conn_per_min`, so the
    nest projects its default; the bridge is booted AFTER that, so its limiter
    starts from that snapshot with an empty window. A connection counts at its
    first command (go-smtp opens the session there), so the spawn's TCP probe
    spends nothing and the count is exact.
    """
    posture = {k: v for k, v in MTA_FIXTURE_SPAM_POSTURE.items() if k != "max_conn_per_min"}
    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_spam_policy", posture)
    try:
        handle = disposable_mta_bridge_factory()
        deadline = time.monotonic() + 45.0
        replies = []
        for _ in range(11):
            raw = socket.create_connection(("127.0.0.1", handle.mx_port), timeout=15.0)
            with _SmtpConn(raw) as conn:
                banner = _read_final_reply(conn, deadline)
                assert banner.startswith("220"), f"banner: {banner!r}"
                conn.send_raw(b"EHLO external.test\r\n")
                try:
                    replies.append(_read_final_reply(conn, deadline))
                except AssertionError as e:  # closed before a full line
                    replies.append(f"closed: {e}")
                if replies[-1].startswith("250"):
                    conn.cmd("QUIT", "221", deadline)
        admitted = [r for r in replies[:10] if r.startswith("250")]
        assert len(admitted) == 10, (
            f"the first ten connections in the minute must be admitted; replies {replies!r} "
            f"(bridge log {handle.log_file})"
        )
        assert replies[10].startswith("421") and "try again later" in replies[10], (
            f"the eleventh connection in the minute must be told to retry later (421); "
            f"got {replies[10]!r} (bridge log {handle.log_file})"
        )
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_spam_policy", dict(MTA_FIXTURE_SPAM_POSTURE))


# ── Outcome 15: a hundred recipients a message ──────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_one_message_cannot_be_addressed_to_more_than_a_hundred_recipients(mail_bridge_mta):
    """A hundred `RCPT TO` in one transaction are accepted; the hundred-and-first
    is refused `452`, so one message never fans out wider than that."""
    handle = mail_bridge_mta
    deadline = time.monotonic() + 120.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        for i in range(100):
            reply = conn.cmd(f"RCPT TO:<{_rcpt(handle)}>", "250", deadline)
            assert reply.startswith("250"), f"recipient {i + 1} of 100: {reply!r}"
        conn.send_raw(f"RCPT TO:<{_rcpt(handle)}>\r\n".encode())
        over = _read_final_reply(conn, deadline)
        assert over.startswith("452"), (
            f"the 101st recipient of one message must be refused 452; got {over!r}"
        )
        conn.cmd("RSET", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


# ── Outcome 16: refused requests are answered ever more slowly ──────────


@pytest.mark.feature("mail-abuse-refused")
def test_refused_requests_are_answered_ever_more_slowly_and_a_clean_sender_never_is(
    mail_bridge_mta,
):
    """Each refused request on a connection is answered after a longer wait than
    the one before (a quarter second more each time), while a sender whose
    requests are accepted is answered without that wait.

    The tarpit's waits are floors the server must sit through before replying,
    so the escalating side asserts lower bounds only. The well-behaved side
    times `MAIL FROM` — local work, no nest call — against the floor the fourth
    refusal had to pay.
    """
    handle = mail_bridge_mta
    deadline = time.monotonic() + 60.0
    waits = []
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        for n in range(1, 5):
            started = time.monotonic()
            conn.send_raw(f"RCPT TO:<relay-{n}@elsewhere.test>\r\n".encode())
            reply = _read_final_reply(conn, deadline)
            waits.append(time.monotonic() - started)
            assert reply.startswith("550"), f"relay attempt {n} must be refused; got {reply!r}"
        conn.cmd("QUIT", "221", deadline)
    for n, waited in enumerate(waits, start=1):
        floor = 0.25 * n
        assert waited >= floor * 0.95, (
            f"refusal {n} must wait at least {floor:.2f}s before the reply; "
            f"waited {waited:.3f}s (all: {waits!r})"
        )

    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        started = time.monotonic()
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        clean = time.monotonic() - started
        conn.cmd("QUIT", "221", deadline)
    assert clean < 1.0, (
        f"an accepted request must not pay the tarpit the fourth refusal paid (1s); "
        f"took {clean:.3f}s"
    )


# ── Outcome 17: half a minute of silence disconnects ────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_a_sender_silent_for_half_a_minute_mid_conversation_is_disconnected(mail_bridge_mta):
    """A server that greets and then says nothing is disconnected once half a
    minute passes, so a slow sender cannot hold a connection open.

    Lower bound and budget, not a measurement: the read deadline is a floor the
    server cannot close before, and the connection must be closed within a
    generous budget after it.
    """
    handle = mail_bridge_mta
    raw = socket.create_connection(("127.0.0.1", handle.mx_port), timeout=15.0)
    with _SmtpConn(raw) as conn:
        deadline = time.monotonic() + 15.0
        assert _read_final_reply(conn, deadline).startswith("220")
        _ehlo_lines(conn, deadline)
        silent_from = time.monotonic()
        budget = silent_from + 90.0
        tail = b""
        raw.settimeout(5.0)
        while True:
            try:
                chunk = raw.recv(4096)
            except socket.timeout:
                assert time.monotonic() < budget, (
                    "a silent sender must be disconnected; still open after 90s"
                )
                continue
            if not chunk:
                break
            tail += chunk
        closed_after = time.monotonic() - silent_from
    assert closed_after >= 25.0, (
        f"the connection was closed after {closed_after:.1f}s of silence — before the "
        f"half-minute a sender is allowed (tail {tail!r})"
    )
    assert not tail or tail.startswith(b"421"), f"a parting reply must be a 421; got {tail!r}"


# ── Outcome 18: header caps ─────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_a_message_with_an_absurd_number_or_size_of_header_lines_is_refused(mail_bridge_mta):
    """A header section of more than 256 lines, or one over a mebibyte, is refused
    `554 5.6.0` at the end of the message, before anything reads it."""
    handle = mail_bridge_mta
    deadline = time.monotonic() + 60.0
    base = [
        "From: External Sender <sender@external.test>",
        f"To: {_rcpt(handle)}",
        "Subject: header bomb",
    ]
    many = base + [f"X-Filler-{i}: x" for i in range(300)]
    reply = _send_data(handle, _message(_rcpt(handle), headers=many), deadline)
    assert reply.startswith("554") and "5.6.0" in reply, (
        f"300 header lines must be refused 554 5.6.0; got {reply!r}"
    )
    # Size: one header line far past the line-length limit. (The 1 MiB section
    # cap is unreachable on the wire — 256 lines of at most the line limit are
    # well under it — so a single absurd line is the size case a peer can send.)
    huge = base + ["X-Huge: " + "y" * 5000]
    reply = _send_data(handle, _message(_rcpt(handle), headers=huge), deadline)
    assert reply.startswith("5"), (
        f"a 5000-byte header line must be refused; got {reply!r}"
    )


# ── Outcome 19: exactly one sender line ─────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_a_message_without_exactly_one_from_line_is_refused_inbound_and_from_a_mail_app(
    mail_bridge_mta,
):
    """A message with no `From:` line, or with two, is refused `554 5.6.0` —
    arriving from outside on port 25, and sent from a signed-in mail app on the
    submission port."""
    handle = mail_bridge_mta
    deadline = time.monotonic() + 60.0
    rcpt = _rcpt(handle)
    common = [f"To: {rcpt}", "Subject: from-count", "Date: Thu, 08 Oct 2026 12:00:00 +0000"]
    none = _message(rcpt, headers=common)
    two = _message(rcpt, headers=[
        "From: First <sender@external.test>",
        "From: Second <other@external.test>",
        *common,
    ])
    for label, raw in (("no From line", none), ("two From lines", two)):
        reply = _send_data(handle, raw, deadline)
        assert reply.startswith("554") and "5.6.0" in reply, (
            f"inbound with {label} must be refused 554 5.6.0; got {reply!r}"
        )

    login = f"{handle.submission_sender_local}@{handle.domain}"
    own = f"Me <{login}>"
    for label, froms in (("no From line", []), ("two From lines", [f"From: {own}", f"From: {own}"])):
        raw = _message("x", headers=[*froms, "To: friend@external.test", "Subject: from-count",
                                     f"Message-ID: <fc-{time.time_ns()}@{handle.domain}>",
                                     "Date: Thu, 08 Oct 2026 12:00:00 +0000"])
        with _connect_submission_tls(handle.submission_port_465, handle.domain) as conn:
            conn.expect("220", deadline)
            conn.cmd(f"EHLO {handle.domain}", "250", deadline)
            _smtp_auth_plain(conn, login, handle.submission_credential.decode(), deadline)
            conn.cmd(f"MAIL FROM:<{login}>", "250", deadline)
            conn.cmd("RCPT TO:<friend@external.test>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(raw)
            conn.send_raw(b".\r\n")
            reply = _read_final_reply(conn, deadline)
            conn.cmd("QUIT", "221", deadline)
        assert reply.startswith("554") and "5.6.0" in reply, (
            f"a mail app's message with {label} must be refused 554 5.6.0; got {reply!r}"
        )


# ── Outcome 21: no smuggled second message ──────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_unusual_line_endings_cannot_smuggle_a_second_message(mail_bridge_mta):
    """A body that hides `<LF>.<LF>` followed by a whole second transaction is one
    message: the server answers its end once, and nothing after the hidden dot is
    read as a command — the next reply is the one to our own `QUIT`.

    A server that honoured the bare-LF dot would answer the smuggled `MAIL`,
    `RCPT` and `DATA` (and the hidden message's end) before our `QUIT`, and the
    `221` would not be the next reply.
    """
    handle = mail_bridge_mta
    deadline = time.monotonic() + 60.0
    rcpt = _rcpt(handle)
    smuggled = (
        "first message\n"
        ".\n"
        "MAIL FROM:<smuggler@external.test>\n"
        f"RCPT TO:<{rcpt}>\n"
        "DATA\n"
        "Subject: smuggled\n"
        "\n"
        "the hidden second message\n"
        ".\n"
    )
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_message(rcpt, body=smuggled))
        conn.send_raw(b".\r\nQUIT\r\n")
        end = _read_final_reply(conn, deadline)
        nxt = _read_final_reply(conn, deadline)
    assert end.startswith("250"), f"the one real message must be answered once; got {end!r}"
    assert nxt.startswith("221"), (
        f"the reply after the message must be the one to QUIT — anything else means "
        f"the hidden transaction was read as commands; got {nxt!r}"
    )


# ── Outcome 22: no sign-in on the incoming-mail port ────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_the_incoming_mail_port_never_accepts_a_sign_in(mail_bridge_mta):
    """Port 25 offers no sign-in, before or after TLS, and refuses one tried
    anyway — so it cannot be used to guess passwords."""
    import base64

    handle = mail_bridge_mta
    deadline = time.monotonic() + 30.0
    raw = socket.create_connection(("127.0.0.1", handle.mx_port), timeout=15.0)
    with _SmtpConn(raw) as plain:
        assert _read_final_reply(plain, deadline).startswith("220")
        caps = _ehlo_lines(plain, deadline)
        assert not any("AUTH" in c.upper() for c in caps), f"no AUTH before TLS: {caps!r}"
    login = f"{handle.submission_sender_local}@{handle.domain}"
    secret = base64.b64encode(
        f"\0{login}\0{handle.submission_credential.decode()}".encode()
    ).decode()
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        caps = _ehlo_lines(conn, deadline)
        assert not any("AUTH" in c.upper() for c in caps), f"no AUTH after TLS: {caps!r}"
        conn.send_raw(f"AUTH PLAIN {secret}\r\n".encode())
        reply = _read_final_reply(conn, deadline)
        assert reply.startswith("5"), (
            f"a sign-in on the incoming-mail port must be refused, even with a real "
            f"password; got {reply!r}"
        )
        conn.cmd("QUIT", "221", deadline)


# ── Outcome 23: scanner down defers ─────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_while_the_malware_scanner_is_down_mail_is_put_off_never_delivered_unscanned(
    disposable_mta_bridge_factory,
):
    """A bridge whose malware scanner cannot be reached answers a clean message
    `451` — the sender keeps it and retries — instead of delivering it unscanned.

    The scanner addresses are loopback ports with nothing listening (taken from
    the free-port finder and left unbound), the deployment shape of a scanner
    that is down.
    """
    from drivers.port_util import find_free_ports

    clamd_port, rspamd_port = find_free_ports(2)
    handle = disposable_mta_bridge_factory(
        f'clamd_addr = "127.0.0.1:{clamd_port}"\n'
        f'rspamd_url = "http://127.0.0.1:{rspamd_port}"\n'
    )
    deadline = time.monotonic() + 60.0
    reply = _send_data(handle, _message(_rcpt(handle)), deadline)
    assert reply.startswith("451"), (
        f"with the scanner down a message must be put off (451), never delivered "
        f"unscanned; got {reply!r} (bridge log {handle.log_file})"
    )

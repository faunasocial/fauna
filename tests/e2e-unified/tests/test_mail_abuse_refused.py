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
outage, which needs a bridge whose scanners are unreachable. The pre-auth
deadline tests boot an MTA and an MDA from the e2e flavor of the bridge
(`mail_bridge_e2e_binary`), whose harness seam shortens the 60 s deadline to a
few seconds — the shipped bridge compiles that seam out (convention 15).
"""

import socket
import time
import uuid

import pytest

from conftest import MTA_FIXTURE_SPAM_POSTURE, _spawn_mda_bridge, _spawn_mta_bridge
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


# ── Outcome 26: a connection that never signs in is closed at a fixed bound ──

# The e2e flavor's pre-auth deadline. Short beside every idle bound that could
# also close a quiet connection (port 25's 30 s read timeout; the submission
# listeners' 5 min), so a close inside `_PREAUTH_BUDGET_S` can only be the bound
# fixed at accept.
_PREAUTH_DEADLINE_S = 4.0
_PREAUTH_ENV = {"FAUNA_MAIL_E2E_PREAUTH_DEADLINE_MS": str(int(_PREAUTH_DEADLINE_S * 1000))}
_PREAUTH_BUDGET_S = 25.0


@pytest.fixture(scope="module")
def preauth_bridges(
    mail_bridge_mta, mail_bridge_mda, mail_bridge_e2e_binary, seal_helper_binary,
    nest_instance, tmp_path_factory,
):
    """An MTA and an MDA from the e2e flavor with the short pre-auth deadline,
    beside the session bridges (whose nest-global state they reuse read-only).
    Yields `{port_label: port}` for 465, 587, 993 and 143."""
    mta = _spawn_mta_bridge(
        mail_bridge_binary=mail_bridge_e2e_binary,
        seal_helper_binary=seal_helper_binary,
        nest_instance=nest_instance,
        tmp=tmp_path_factory.mktemp("mail-bridge-mta-preauth"),
        bridge_id=f"test-mta-preauth-{uuid.uuid4().hex[:12]}",
        domain=mail_bridge_mta.domain,
        extra_env=_PREAUTH_ENV,
    )
    try:
        mda = _spawn_mda_bridge(
            mail_bridge_binary=mail_bridge_e2e_binary,
            nest_instance=nest_instance,
            tmp=tmp_path_factory.mktemp("mail-bridge-mda-preauth"),
            bridge_id=f"test-mda-preauth-{uuid.uuid4().hex[:12]}",
            domain=mail_bridge_mda.domain,
            extra_env=_PREAUTH_ENV,
        )
        try:
            yield {
                "465": mta.submission_port_465,
                "587": mta.submission_port_587,
                "993": mda.imaps_port,
                "143": mda.imap_starttls_port,
                "logs": f"MTA log {mta.log_file_path}, MDA log {mda.log_file_path}",
            }
        finally:
            mda.cleanup()
    finally:
        mta.cleanup()


def _seconds_until_closed(port: int, *, trickle: bytes | None = None) -> tuple[float, bytes]:
    """Connect to `port` and wait for the server to close the connection,
    returning (seconds from before the connect, bytes received).

    Silent unless `trickle` is given, which is sent every half second — a
    client that keeps talking, so no idle timeout can be what closes it. The
    clock starts before the connect, so the bound (fixed at the server's accept)
    can never read as early. Fails if the connection outlives the budget.
    """
    started = time.monotonic()
    sock = socket.create_connection(("127.0.0.1", port), timeout=10.0)
    received = b""
    next_send = started
    try:
        sock.settimeout(0.25)
        while True:
            now = time.monotonic()
            assert now - started < _PREAUTH_BUDGET_S, (
                f"port {port}: an unauthenticated connection was still open after "
                f"{_PREAUTH_BUDGET_S:.0f}s with a {_PREAUTH_DEADLINE_S:.0f}s pre-auth "
                f"deadline (received {received[-300:]!r})"
            )
            if trickle is not None and now >= next_send:
                try:
                    sock.sendall(trickle)
                except OSError:
                    break  # closed under us
                next_send = now + 0.5
            try:
                chunk = sock.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                break  # reset
            if not chunk:
                break
            received += chunk
    finally:
        sock.close()
    return time.monotonic() - started, received


def _assert_closed_at_the_bound(port_label: str, closed_after: float, received: bytes, logs: str):
    assert closed_after >= _PREAUTH_DEADLINE_S * 0.95, (
        f"port {port_label}: closed after {closed_after:.2f}s — before the "
        f"{_PREAUTH_DEADLINE_S:.0f}s pre-auth deadline, so something else closed it "
        f"(received {received[-300:]!r}; {logs})"
    )


@pytest.mark.feature("mail-abuse-refused")
@pytest.mark.parametrize("port_label", ["465", "587", "993", "143"])
def test_a_silent_connection_that_never_signs_in_is_closed_at_the_pre_auth_deadline(
    preauth_bridges, port_label,
):
    """On the submission ports (465, 587) and the mail-reading ports (993, 143),
    a connection that never signs in — here one that never says anything, not
    even a TLS hello on 465 and 993 — is closed once the pre-auth deadline
    passes: not before it, and well inside a budget no idle timeout fits.

    Lower bound and budget (convention 14): the deadline is a floor the server
    cannot close before; the budget is generous, and still shorter than every
    idle timeout that could also close a quiet connection.
    """
    closed_after, received = _seconds_until_closed(preauth_bridges[port_label])
    _assert_closed_at_the_bound(port_label, closed_after, received, preauth_bridges["logs"])


@pytest.mark.feature("mail-abuse-refused")
@pytest.mark.parametrize("port_label, trickle", [
    ("587", b"NOOP\r\n"),
    ("143", b"a1 NOOP\r\n"),
])
def test_a_talkative_connection_that_never_signs_in_is_still_closed_at_the_bound_fixed_at_accept(
    preauth_bridges, port_label, trickle,
):
    """A connection that keeps sending harmless commands but never signs in is
    closed at the same bound: the deadline runs from accept and no command
    moves it, so a stranger cannot hold a connection open by trickling bytes
    (every per-command idle timeout restarts on each line and never fires here).
    """
    closed_after, received = _seconds_until_closed(preauth_bridges[port_label], trickle=trickle)
    _assert_closed_at_the_bound(port_label, closed_after, received, preauth_bridges["logs"])
    # The commands were answered while the connection lived — it was open and
    # talking, not stuck, when the bound closed it.
    assert received.count(b"\r\n") >= 2, (
        f"port {port_label}: expected a greeting and answered commands before the "
        f"close; received {received[-300:]!r} ({preauth_bridges['logs']})"
    )


# ── Outcome 27: ten unknown recipients end the session ──────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_ten_unknown_recipients_in_one_session_end_it_with_421(mail_bridge_mta):
    """A sender walking local parts that do not exist is refused each one, and
    after the tenth refusal the next command is answered `421 4.7.0` and the
    session ends — a brake on harvesting the directory.

    No timing assertion; the budget covers the tarpit, which waits a quarter
    second longer before each refusal (about 14 s over ten).
    """
    handle = mail_bridge_mta
    deadline = time.monotonic() + 120.0
    refusals = []
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        for n in range(1, 11):
            conn.send_raw(f"RCPT TO:<nobody-{uuid.uuid4().hex[:10]}@{handle.domain}>\r\n".encode())
            reply = _read_final_reply(conn, deadline)
            refusals.append(reply)
            assert reply.startswith("5"), (
                f"unknown recipient {n} must be refused; got {reply!r} (all: {refusals!r})"
            )
        conn.send_raw(f"RCPT TO:<{_rcpt(handle)}>\r\n".encode())
        try:
            capped = _read_final_reply(conn, deadline)
        except AssertionError as e:  # closed before a full line
            capped = f"closed: {e}"
    assert capped.startswith("421") and "4.7.0" in capped, (
        f"after ten unknown recipients the next command — even one naming a real "
        f"recipient — must be answered 421 4.7.0; got {capped!r} (refusals {refusals!r})"
    )


# ── The in-flight DATA byte budget (smtp-server.md § Connection-time limits) ──

# Bytes each holder streams: under any size cap the fixture runs with (the
# 50 MB default), so no holder is refused for size; twelve of them are well past
# the bridge's 256 MiB budget whatever loopback buffering leaves unread.
_HOLDER_BYTES = 40_000_000
_HOLDERS = 12


@pytest.mark.feature("mail-abuse-refused")
def test_messages_past_the_servers_memory_budget_are_put_off_not_accepted(mail_bridge_mta):
    """While other connections hold unfinished messages that fill the server's
    in-flight memory budget, the next message is put off with `451 4.3.1` — the
    sender keeps it and retries — instead of being read into memory without bound.

    No timing: twelve connections stream 40 MB each and none finishes, so the
    server holds every byte it has read. Loopback leaves at most a few megabytes
    per connection unread, so the last connection's message cannot fit; its reply
    to `.` is the witness. The others are dropped mid-message, which ends their
    transactions without delivering anything.
    """
    handle = mail_bridge_mta
    deadline = time.monotonic() + 300.0
    line = b"x" * 998 + b"\r\n"
    head = _message(_rcpt(handle), body="budget")
    chunk = line * 1000
    conns = []
    try:
        for _ in range(_HOLDERS):
            conn = _connect_smtp_starttls(handle.mx_port, handle.domain, deadline)
            conns.append(conn)
            conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
            conn.cmd(f"RCPT TO:<{_rcpt(handle)}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(head)
            sent = len(head)
            while sent < _HOLDER_BYTES:
                conn.send_raw(chunk)
                sent += len(chunk)
        last = conns[-1]
        last.send_raw(b".\r\n")
        reply = _read_final_reply(last, deadline)
        assert reply.startswith("451") and "4.3.1" in reply, (
            f"a message past the in-flight memory budget must be put off 451 4.3.1; "
            f"got {reply!r}"
        )
    finally:
        for conn in conns:
            conn.__exit__(None, None, None)


# ── Outcome 29: a sender check past its deadline is put off, not read in unchecked ──

# The e2e cdylib's verification deadline (`FAUNA_MAIL_E2E_VERIFY_DEADLINE_MS`,
# fauna-mail's `test-helpers` seam in place of the 30 s `VERIFY_DEADLINE`). Short
# beside port 25's 30 s read timeout, so a reply to `.` inside the budget can
# only be the verification's own deadline.
_VERIFY_DEADLINE_S = 3.0
_VERIFY_BUDGET_S = 25.0


@pytest.fixture(scope="module")
def silent_resolver_mta(
    mail_bridge_mta, mail_bridge_e2e_binary, seal_helper_binary, nest_instance,
    tmp_path_factory,
):
    """An MTA from the e2e flavor whose sender-authentication lookups go to a
    loopback UDP socket this fixture binds and never answers — the shape of a
    sender's DNS that is deliberately silent — under a short verification
    deadline. Reuses the session bridge's nest state (domain, recipient)
    read-only. Yields `(spawned bridge, recipient address)`."""
    silent = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    silent.bind(("127.0.0.1", 0))
    try:
        spawned = _spawn_mta_bridge(
            mail_bridge_binary=mail_bridge_e2e_binary,
            seal_helper_binary=seal_helper_binary,
            nest_instance=nest_instance,
            tmp=tmp_path_factory.mktemp("mail-bridge-mta-silent-dns"),
            bridge_id=f"test-mta-silent-dns-{uuid.uuid4().hex[:12]}",
            domain=mail_bridge_mta.domain,
            extra_env={
                "FAUNA_MAIL_E2E_RESOLVER": f"127.0.0.1:{silent.getsockname()[1]}",
                "FAUNA_MAIL_E2E_VERIFY_DEADLINE_MS": str(int(_VERIFY_DEADLINE_S * 1000)),
            },
        )
        try:
            yield spawned, _rcpt(mail_bridge_mta)
        finally:
            spawned.cleanup()
    finally:
        silent.close()


@pytest.mark.feature("mail-abuse-refused")
def test_a_message_whose_sender_cannot_be_checked_in_time_is_put_off_with_451(
    silent_resolver_mta,
):
    """When the sender checks (SPF, DKIM, DMARC, ARC) cannot finish inside the
    verification deadline — here because every lookup goes to a DNS server that
    never answers — a benign message is answered `451 4.7.0`: the sender keeps it
    and retries, and nothing is accepted unchecked.

    Lower bound and budget (convention 14): the reply cannot come before the
    deadline (an instant `451` would be some other transient failure, such as a
    resolver that could not be built), and comes well inside a budget shorter
    than port 25's read timeout. The bridge log names the deadline as the cause
    (`AuthError: Timeout`, not a resolver-init failure).
    """
    spawned, rcpt = silent_resolver_mta
    deadline = time.monotonic() + 120.0
    with _connect_smtp_starttls(spawned.mx_port, rcpt.split("@", 1)[1], deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_message(rcpt))
        sent_at = time.monotonic()
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)
        answered_after = time.monotonic() - sent_at
    with open(spawned.log_file_path, encoding="utf-8", errors="replace") as fh:
        log_tail = fh.read()[-4000:]
    assert reply.startswith("451") and "4.7.0" in reply, (
        f"a message whose sender checks outran the deadline must be put off 451 4.7.0; "
        f"got {reply!r} after {answered_after:.2f}s (bridge log {spawned.log_file_path})"
    )
    assert answered_after >= _VERIFY_DEADLINE_S * 0.95, (
        f"451 came {answered_after:.2f}s after the message — before the "
        f"{_VERIFY_DEADLINE_S:.0f}s verification deadline, so it was not the deadline "
        f"(log tail: {log_tail[-1500:]!r})"
    )
    assert answered_after < _VERIFY_BUDGET_S, (
        f"451 came {answered_after:.2f}s after the message, past the {_VERIFY_BUDGET_S:.0f}s "
        f"budget a {_VERIFY_DEADLINE_S:.0f}s deadline allows (log tail: {log_tail[-1500:]!r})"
    )
    assert "AuthError: Timeout" in log_tail, (
        f"the bridge log does not name the verification deadline as the cause of the 451 "
        f"(log tail: {log_tail[-1500:]!r})"
    )

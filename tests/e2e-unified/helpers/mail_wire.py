"""Shared socket-level SMTP + IMAP wire helpers for the tier_3 mail-bridge tests.

These are the raw-wire primitives the mail-bridge e2e tests drive the real
`fauna-mail-bridge` binaries with — hand-rolled SMTP/IMAP clients rather than
`smtplib`/`imaplib`, because the go-imap fork advertises an IMAP4rev2-only
CAPABILITY that `imaplib` rejects as "not IMAP4 compliant", and the SMTP reader
must buffer across CRLFs (go-smtp flushes the whole EHLO capability list in one
TCP segment, so a per-line reader that drops the remainder hangs forever).

Lifted here from `tests/test_mail_bridge_mta.py` (the SMTP half) and
`tests/test_mail_bridge_mda.py` (the IMAP half) so the inbound→IMAP-read seam
test (`tests/test_mail_inbound_to_imap.py`) can reuse both without importing
across test modules (priority #1/#4 — lift, don't copy). The underscore-prefixed
names are preserved verbatim so the migrating test files change by import only.

The IMAP helpers that take a `handle` (`_imaps_connect`) duck-type on
`handle.imaps_port` / `handle.domain`, so any bridge handle (MDA or the combined
inbound→imap handle) works.
"""

from __future__ import annotations

import base64
import contextlib
import json
import re
import socket
import ssl
import time
import urllib.request

from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.waiting import wait_until


# ─────────────────────────────────────────────────────────────────────────
# SMTP wire (port 25 / submission)
# ─────────────────────────────────────────────────────────────────────────


class _SmtpConn:
    """Buffered SMTP client connection over a raw (or TLS-wrapped) socket.

    Keeps a persistent receive buffer across `recv_line` calls. This
    matters because a multi-line reply is often delivered in a single TCP
    segment — go-smtp flushes the whole EHLO capability list as one write
    — so a reader that `recv`s into a fresh buffer per line and keeps only
    the bytes up to the first CRLF silently drops every following line,
    then blocks forever on the next `recv` (the server already sent
    everything). That was a TCP-segmentation-luck flake in the previous
    free-function helpers; buffering the remainder fixes it deterministically.

    Works over any object exposing `settimeout`/`recv`/`sendall`/`close`,
    so the E.3 submission test can wrap an `ssl`-upgraded socket the same way.
    """

    def __init__(self, sock: socket.socket) -> None:
        self._sock = sock
        self._buf = bytearray()

    def __enter__(self) -> "_SmtpConn":
        return self

    def __exit__(self, *_exc: object) -> None:
        self._sock.close()

    def recv_line(self, deadline: float) -> str:
        """Read one CRLF-terminated SMTP reply line; time out at deadline."""
        while b"\r\n" not in self._buf:
            self._sock.settimeout(max(0.1, deadline - time.monotonic()))
            chunk = self._sock.recv(4096)
            if not chunk:
                raise AssertionError(
                    f"SMTP server closed connection mid-line; partial buf={bytes(self._buf)!r}"
                )
            self._buf.extend(chunk)
        line, _, rest = self._buf.partition(b"\r\n")
        self._buf = bytearray(rest)
        return line.decode("utf-8", errors="replace")

    def expect(self, code: str, deadline: float) -> str:
        """Read reply lines until a non-continuation arrives; assert the code."""
        last_line = ""
        while True:
            line = self.recv_line(deadline)
            last_line = line
            # Continuation lines have a hyphen after the code (e.g. "250-...").
            if len(line) >= 4 and line[3] == "-":
                continue
            break
        if not last_line.startswith(code):
            raise AssertionError(f"SMTP expected {code} reply, got {last_line!r}")
        return last_line

    def cmd(self, command: str, expect: str, deadline: float) -> str:
        self._sock.sendall((command + "\r\n").encode())
        return self.expect(expect, deadline)

    def send_raw(self, data: bytes) -> None:
        self._sock.sendall(data)


def _read_final_reply(conn: _SmtpConn, deadline: float) -> str:
    """Read reply lines until a non-continuation arrives — WITHOUT asserting a code.

    `_SmtpConn.expect` asserts the code it was handed; a test that must
    *discover* the server's verdict (deliver, defer, refuse) — or poll it while
    an admin change hot-applies — reads the final line with this instead.
    """
    while True:
        line = conn.recv_line(deadline)
        if len(line) >= 4 and line[3] == "-":
            continue
        return line


def _connect_smtp_starttls(mx_port: int, server_name: str, deadline: float) -> _SmtpConn:
    """Connect to port 25 and upgrade to TLS via STARTTLS, then re-EHLO.

    Port 25 runs `InboundTLSMode=required` (smtp-server.md § TLS posture:
    "port 25 demands STARTTLS"): the bridge advertises STARTTLS and rejects a
    cleartext MAIL FROM with `530 5.7.10`, so every inbound transaction must
    STARTTLS first. The fixture's self-signed cert is trusted via CERT_NONE
    (this exercises the mail path, not PKI — same posture as
    `_connect_submission_tls`). Returns a fresh `_SmtpConn` over the TLS socket,
    post-EHLO, ready for MAIL FROM.
    """
    raw = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    raw.settimeout(15.0)
    raw.connect(("127.0.0.1", mx_port))
    plain = _SmtpConn(raw)
    plain.expect("220", deadline)
    plain.cmd("EHLO external.test", "250", deadline)
    plain.cmd("STARTTLS", "220", deadline)
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    tls = ctx.wrap_socket(raw, server_hostname=server_name)
    conn = _SmtpConn(tls)
    conn.cmd("EHLO external.test", "250", deadline)
    return conn


def _connect_submission_tls(port: int, server_name: str) -> _SmtpConn:
    """TCP-connect + implicit-TLS-wrap the submission listener (port 465).

    The bridge serves the fixture's self-signed cert; the test trusts it
    via CERT_NONE (this exercises submission auth + DKIM signing + outbound
    delivery, not TLS PKI). The wrapped socket plugs into the same buffered
    `_SmtpConn` reader the plaintext path uses. The caller drives EHLO + AUTH.
    """
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", port), timeout=15.0)
    tls = ctx.wrap_socket(raw, server_hostname=server_name)
    return _SmtpConn(tls)


def _smtp_auth_plain(conn: _SmtpConn, username: str, secret: str, deadline: float) -> str:
    """Drive submission `AUTH PLAIN` with an initial response (RFC 4954 §4).

    The MUA-side counterpart of IMAP's `_imap_auth_plain`: the password is the
    client-minted PLAIN credential, which the bridge AEAD-unwraps against the
    wrapped submission token with Argon2id (`mailfauna.KdfKindArgon2id`).
    Asserts the `235` success reply.
    """
    ir = base64.b64encode(
        b"\x00" + username.encode() + b"\x00" + secret.encode()
    ).decode()
    return conn.cmd(f"AUTH PLAIN {ir}", "235", deadline)


def _smtp_auth_oauthbearer(conn: _SmtpConn, username: str, token: str, deadline: float) -> str:
    """Drive submission SASL `OAUTHBEARER` with an initial response (RFC 7628).

    The client-first GS2 message is the same `n,a=<user>,\\x01auth=Bearer
    <token>\\x01\\x01` shape as IMAP's `_imap_auth_oauthbearer` (host/port advisory,
    omitted — already trusted at the TLS layer). The bearer token is the one-time
    token the client minted at enable-mail, which the bridge AEAD-unwraps against
    the wrapped submission token with HKDF (`mailfauna.KdfKindHkdf`).

    On rejection, go-sasl's OAUTHBEARER server returns a JSON error as a `334`
    continuation and only completes after a `\\x01` cancel response, so a bad
    token surfaces as the failure code only after we send that cancel byte.
    Asserts the `235` success reply.
    """
    gs2 = f"n,a={username},\x01auth=Bearer {token}\x01\x01"
    ir = base64.b64encode(gs2.encode()).decode()
    conn.send_raw(f"AUTH OAUTHBEARER {ir}\r\n".encode())
    line = conn.recv_line(deadline)
    # A failed exchange sends a JSON error challenge (334) first; send the GS2
    # cancel (a single 0x01 byte, base64-encoded) so the server completes.
    if line.startswith("334"):
        conn.send_raw((base64.b64encode(b"\x01").decode() + "\r\n").encode())
        line = conn.recv_line(deadline)
    if not line.startswith("235"):
        raise AssertionError(f"SMTP OAUTHBEARER auth expected 235 reply, got {line!r}")
    return line


def _find_tagged(stub_mx, token: str) -> bytes | None:
    """Return the first accumulated stub-MX message carrying `token`, or None."""
    needle = token.encode()
    for msg in stub_mx.messages():
        if needle in msg:
            return msg
    return None


def _wait_for_tagged(stub_mx, token: str, timeout: float) -> bytes | None:
    """Poll the stub MX's accumulated messages for one tagged `token`."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        msg = _find_tagged(stub_mx, token)
        if msg is not None:
            return msg
        time.sleep(0.25)
    return None

def _poke_outbound_bridge(nest_url: str) -> int:
    """Make the MTA's outbound worker run a drain cycle NOW.

    Hits `POST /api/v1/test/outbound/poke_bridge` (gated on `--features
    test-hooks` alone, which `build_node()` enables), which emits the
    **production** `fauna.bridges.outbound_ready` push to every approved
    MTA-role bridge. The bridge's `wsrpc.OutboundReadyHandler` fires the
    outbound worker's coalescing `Trigger()`, short-circuiting its 30s
    `PollInterval` sleep — the `run_now` poke of `e2e-conventions.md`
    § convention 14, over the real nudge path rather than a test-only
    listener bolted onto the bridge.

    Asserts at least one MTA bridge was addressed: `nudged: 0` means none is
    enrolled, so the poke did nothing and a caller treating it as a barrier
    would silently degrade to waiting out the poll interval.
    """
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/poke_bridge",
        data=b"",
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"outbound poke hook returned {resp.status}"
        nudged = json.loads(resp.read().decode())["nudged"]
    assert nudged >= 1, (
        "outbound poke reached no MTA-role bridge — nothing was nudged, so the "
        "poke is not a barrier (is the mail_bridge_mta fixture's bridge enrolled?)"
    )
    return nudged


# ─────────────────────────────────────────────────────────────────────────
# IMAP wire (IMAPS / IMAP+STARTTLS)
# ─────────────────────────────────────────────────────────────────────────


def outbound_clock(nest_url: str, **body) -> int:
    """Set (`set=<epoch>`, `set=0` back to the real clock) or move
    (`advance=<secs>`) the nest's outbound clock — the one every queue time and
    the warm-up day are read on — via `POST /api/v1/test/outbound/clock`
    (`test-hooks`); returns the clock's new value. Any value but `set=0` leaves
    the clock FROZEN there."""
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/clock", data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        return json.loads(resp.read().decode())["clock"]


def outbound_clock_now(nest_url: str) -> tuple[int, bool]:
    """Read the nest's outbound clock without leaving it changed: returns
    `(now, frozen)`. The hook has no read form, so this freezes the clock at its
    current value and, when that value is the real clock (unfrozen before),
    un-freezes it again. A test that moves the clock hands `(now, frozen)` to
    `restore_outbound_clock` on the way out, so the session-scoped nest's later
    tests see the clock they would have seen."""
    now = outbound_clock(nest_url, advance=0)
    frozen = abs(now - time.time()) > 2
    if not frozen:
        outbound_clock(nest_url, set=0)
    return now, frozen


def restore_outbound_clock(nest_url: str, prior: tuple[int, bool]) -> None:
    """Put the outbound clock back as `outbound_clock_now` found it."""
    now, frozen = prior
    outbound_clock(nest_url, set=now if frozen else 0)


def outbound_queue(nest_url: str) -> list:
    """Snapshot nest's `outbound_mail_queue` rows (any status), ordered by id.

    `GET /api/v1/test/outbound/queue`, same `test-hooks` gate. Projects
    scheduling state only — `attempt_count`, `next_attempt_at`,
    `delay_warned_at`, `status` — never message bodies.
    """
    req = urllib.request.Request(f"{nest_url}/api/v1/test/outbound/queue", method="GET")
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"outbound queue hook returned {resp.status}"
        return json.loads(resp.read().decode())["rows"]


def outbound_msgid_key(msgid: str) -> str:
    """A Message-ID normalized for comparison against `original_msgid`.

    ⚠ The queue stores the two provenances of a msgid differently, and the
    difference is invisible until an assertion silently never matches: a msgid
    **parsed from a message header** is stored bare (`t12bounce-…@fauna.test`),
    while one nest **mints itself** keeps its angle brackets (`<delay-…>`,
    `<bounce-…>` — see `_count_delay_warning_rows`). Callers hold the header
    form, so normalizing both sides is what makes a lookup work for either.
    """
    return msgid.strip().lstrip("<").rstrip(">")


@contextlib.contextmanager
def outbound_clock_on_real_time(nest_url: str):
    """Put the outbound clock on the real clock for the block; restore it after."""
    prior = outbound_clock_now(nest_url)
    outbound_clock(nest_url, set=0)
    try:
        yield
    finally:
        restore_outbound_clock(nest_url, prior)


def outbound_row(nest_url: str, token: str, rcpt: str):
    """The queue row of the message tagged `token` (its Message-ID is
    `<token@domain>`, `submit_one_message`) for `rcpt`, or None."""
    return next(
        (r for r in outbound_queue(nest_url)
         if outbound_msgid_key(r["original_msgid"]).split("@", 1)[0] == token
         and r["recipient"] == rcpt),
        None,
    )


def await_outbound_row(nest_url: str, token: str, rcpt: str, want, what: str) -> dict:
    """Wait until `outbound_row(...)` exists and satisfies `want`; return it."""
    def _matching():
        row = outbound_row(nest_url, token, rcpt)
        return row if row is not None and want(row) else None

    return wait_until(
        _matching,
        MAIL_OUTBOUND_CYCLE_S,
        diagnose=lambda: f"{what}: row is {outbound_row(nest_url, token, rcpt)!r}",
    )


def outbound_attempt_at(nest_url: str, token: str, rcpt: str, at: int) -> dict:
    """Move the clock to `at`, run a drain cycle now, and return the row once
    that attempt is recorded (its count advanced, or it went terminal)."""
    before = outbound_row(nest_url, token, rcpt)
    assert before is not None and before["status"] == "pending", before
    outbound_clock(nest_url, set=at)
    _poke_outbound_bridge(nest_url)
    return await_outbound_row(
        nest_url, token, rcpt,
        lambda r: r["attempt_count"] > before["attempt_count"] or r["status"] != "pending",
        f"an attempt at clock {at}",
    )

def await_outbound_terminal(nest_url: str, msgid: str, recipient: str) -> dict:
    """Barrier: wait until the queue row for `(msgid, recipient)` is terminal.

    The causal anchor every "the DSN must NOT be relayed out" guard rests on
    (`e2e-conventions.md` § convention 14 — negative asserts anchor to causal
    order, never to a settle window). nest's `mark_outbound_bounced` handler
    runs the **entire** NDR generation before it flips the row off `pending`,
    and `submit_outbound` — the single chokepoint where a DSN could ever be
    enqueued for MX relay — is inside that span:

      * to-sender NDR: `submit_outbound` (`outbound_bounce.rs:174-200`) then
        `mark_outbound_bounced_with_reason` (`:206`);
      * forwarder NDR: `seal_and_ingest_local` (`:321-337`) then
        `mark_outbound_bounced_with_reason` (`:343`);
      * both of the handler's own fallback arms mark the row bounced too
        (`bridge_routing_handlers.rs:6300-6330`), so the status leaves
        `pending` on every path.

    So a row observed terminal proves that message's relay decision has already
    been made, and any row it would have enqueued is **already in the queue**.
    "No DSN was relayed out" becomes a statement about state rather than about
    elapsed time — and it is strictly stronger than reading the stub MX, which
    could only see a relayed DSN after a further outbound drain cycle (the
    worker's `PollInterval` is 30 s, so the 2 s settle windows this replaced
    could not reliably have caught the regression at all).

    Returns the terminal row, so a caller can assert on the status it reached.
    """
    return wait_until(
        lambda: next(
            (
                row
                for row in outbound_queue(nest_url)
                if outbound_msgid_key(row["original_msgid"]) == outbound_msgid_key(msgid)
                and row["recipient"] == recipient
                and row["status"] != "pending"
            ),
            None,
        ),
        MAIL_OUTBOUND_CYCLE_S,
        diagnose=lambda: (
            f"no terminal outbound row for msgid={msgid!r} recipient={recipient!r}; "
            f"queue={[(r['original_msgid'], r['recipient'], r['status']) for r in outbound_queue(nest_url)]}"
        ),
    )


def _recv_line(sock: socket.socket, buf: bytearray, deadline: float) -> str:
    """Read one CRLF-terminated IMAP line, buffering any trailing bytes."""
    while b"\r\n" not in buf:
        sock.settimeout(max(0.1, deadline - time.monotonic()))
        chunk = sock.recv(4096)
        if not chunk:
            raise AssertionError(
                f"IMAPS connection closed mid-line; partial buf={bytes(buf)!r}"
            )
        buf.extend(chunk)
    line, _, rest = buf.partition(b"\r\n")
    buf[:] = rest
    return line.decode("utf-8", errors="replace")


def _recv_exact(sock: socket.socket, buf: bytearray, n: int, deadline: float) -> bytes:
    """Read exactly `n` raw bytes (an IMAP literal payload, which may itself
    contain CRLFs), consuming from + refilling the shared buffer."""
    while len(buf) < n:
        sock.settimeout(max(0.1, deadline - time.monotonic()))
        chunk = sock.recv(65536)
        if not chunk:
            raise AssertionError("IMAPS connection closed mid-literal")
        buf.extend(chunk)
    data = bytes(buf[:n])
    buf[:] = buf[n:]
    return data


_LITERAL_SUFFIX = re.compile(r"\{(\d+)\}$")


def _imaps_connect_addr(host: str, port: int, server_name: str, deadline: float):
    """Open an implicit-TLS IMAPS socket to ``(host, port)`` and read the greeting.

    Returns (sock, buf). CERT_NONE: a self-signed cert (the process fixture's or
    the deploy image's) exercises the mail path, not PKI. We drive the wire by
    hand rather than via `imaplib`, which rejects the go-imap fork's IMAP4rev2-only
    CAPABILITY as "not IMAP4 compliant". The address-taking form so callers that
    don't have a bridge handle (the docker deploy round-trip, which connects by
    mapped host port) reuse the same primitives; `_imaps_connect` wraps it.
    """
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection((host, port), timeout=15.0)
    sock = ctx.wrap_socket(raw, server_hostname=server_name)
    buf = bytearray()
    greeting = _recv_line(sock, buf, deadline)
    assert greeting.startswith("* OK"), f"unexpected IMAPS greeting: {greeting!r}"
    return sock, buf


def _imaps_connect(handle, deadline: float):
    """Open an implicit-TLS IMAPS socket to the MDA bridge and read the greeting.

    Duck-types on `handle.imaps_port` / `handle.domain`, so any bridge handle
    (MDA or the combined inbound→imap handle) works. Thin wrapper over
    `_imaps_connect_addr` (loopback host).
    """
    return _imaps_connect_addr("127.0.0.1", handle.imaps_port, handle.domain, deadline)


def _imap_read_tagged(sock, buf, tag: str, deadline: float):
    """Read IMAP response lines until the tagged completion `<tag> OK|NO|BAD`.

    Returns (status, untagged_lines).
    """
    untagged = []
    while True:
        line = _recv_line(sock, buf, deadline)
        if line.startswith(tag + " "):
            return line.split(" ", 2)[1], untagged
        untagged.append(line)


def _imap_auth_plain(sock, buf, tag: str, username: str, password: str, deadline: float):
    """Drive SASL PLAIN via the two-step AUTHENTICATE continuation. Returns the
    tagged status ("OK" / "NO" / "BAD")."""
    sock.sendall(f"{tag} AUTHENTICATE PLAIN\r\n".encode())
    cont = _recv_line(sock, buf, deadline)
    assert cont.startswith("+"), f"expected AUTHENTICATE continuation, got {cont!r}"
    ir = base64.b64encode(
        b"\x00" + username.encode() + b"\x00" + password.encode()
    ).decode()
    sock.sendall((ir + "\r\n").encode())
    status, _ = _imap_read_tagged(sock, buf, tag, deadline)
    return status


def _imap_auth_oauthbearer(sock, buf, tag: str, username: str, token: str, deadline: float):
    """Drive SASL OAUTHBEARER (RFC 7628) via the AUTHENTICATE continuation.
    Returns the tagged status ("OK" / "NO" / "BAD").

    The client-first GS2 message is `n,a=<user>,\\x01auth=Bearer <token>\\x01\\x01`
    (host/port are advisory — already trusted at the TLS layer — so they're
    omitted; the bridge only reads the user + token, auth.go:131). The MUA-side
    counterpart of PLAIN's `_imap_auth_plain`: the bearer token here is the
    one-time token the client minted at enable-mail, which the bridge AEAD-unwraps
    with the HKDF KDF (`mailfauna.KdfKindHkdf`, auth.go:156) rather than PLAIN's
    Argon2id.

    On failure, go-sasl's OAUTHBEARER server returns a JSON error as a `+`
    continuation challenge and only completes the exchange after a `\\x01` dummy
    response (RFC 7628 § 3.2.3), so a rejected token surfaces as `NO` only after
    we send that cancel byte — handled here so the helper always returns a tagged
    status instead of hanging.
    """
    sock.sendall(f"{tag} AUTHENTICATE OAUTHBEARER\r\n".encode())
    cont = _recv_line(sock, buf, deadline)
    assert cont.startswith("+"), f"expected AUTHENTICATE continuation, got {cont!r}"
    gs2 = f"n,a={username},\x01auth=Bearer {token}\x01\x01"
    sock.sendall((base64.b64encode(gs2.encode()).decode() + "\r\n").encode())
    line = _recv_line(sock, buf, deadline)
    # A failed exchange sends a JSON error challenge first; send the GS2 cancel
    # (a single 0x01 byte, base64-encoded) so the server completes with NO.
    if line.startswith("+"):
        sock.sendall((base64.b64encode(b"\x01").decode() + "\r\n").encode())
        line = _recv_line(sock, buf, deadline)
    while not line.startswith(tag + " "):
        line = _recv_line(sock, buf, deadline)
    return line.split(" ", 2)[1]


def _imap_append(sock, buf, tag: str, mailbox: str, message: bytes, deadline: float):
    """Drive `APPEND <mailbox> {<len>}` with a single literal. Returns
    (status, appenduid) where appenduid is the UID parsed from the
    UIDPLUS `[APPENDUID <validity> <uid>]` response code (None if absent)."""
    sock.sendall(f"{tag} APPEND {mailbox} {{{len(message)}}}\r\n".encode())
    cont = _recv_line(sock, buf, deadline)
    assert cont.startswith("+"), f"expected APPEND literal continuation, got {cont!r}"
    sock.sendall(message + b"\r\n")
    line = _recv_line(sock, buf, deadline)
    while not line.startswith(tag + " "):
        line = _recv_line(sock, buf, deadline)
    status = line.split(" ", 2)[1]
    m = re.search(r"\[APPENDUID\s+\d+\s+(\d+)\]", line)
    return status, (int(m.group(1)) if m else None)


def _imap_fetch_body_cmd(sock, buf, tag: str, fetch_cmd: str, deadline: float):
    """Drive a `FETCH`-family command that returns `BODY[]` and parse the
    response literal so a body containing CRLFs round-trips byte-exactly →
    (status, body_bytes)."""
    sock.sendall(f"{tag} {fetch_cmd}\r\n".encode())
    body = None
    while True:
        line = _recv_line(sock, buf, deadline)
        if line.startswith(tag + " "):
            return line.split(" ", 2)[1], body
        m = _LITERAL_SUFFIX.search(line)
        if m:
            body = _recv_exact(sock, buf, int(m.group(1)), deadline)


def _imap_uid_fetch_body(sock, buf, tag: str, uid: int, deadline: float):
    """`UID FETCH <uid> BODY[]` → (status, body_bytes)."""
    return _imap_fetch_body_cmd(sock, buf, tag, f"UID FETCH {uid} BODY[]", deadline)


def _imap_seq_fetch_body(sock, buf, tag: str, seq: int, deadline: float):
    """`FETCH <seq> BODY[]` (sequence-number addressed) → (status, body_bytes)."""
    return _imap_fetch_body_cmd(sock, buf, tag, f"FETCH {seq} BODY[]", deadline)


def _imap_uid_fetch_section(sock, buf, tag: str, uid: int, item: str, deadline: float):
    """`UID FETCH <uid> <item>` for one sectioned BODY item (e.g.
    `BODY[HEADER]`, `BODY[TEXT]`, `BODY.PEEK[HEADER.FIELDS (Subject)]`) →
    (status, section_bytes). The response literal is parsed byte-exactly so a
    section containing CRLFs round-trips. One BODY item per call (the parser
    keeps the last literal). Also serves `BINARY[N]` — its `~{n}` literal8 ends
    in `{n}`, which the literal-suffix regex matches like any literal."""
    return _imap_fetch_body_cmd(sock, buf, tag, f"UID FETCH {uid} {item}", deadline)


def _imap_uid_fetch_capture(sock, buf, tag: str, uid: int, item: str, deadline: float):
    """Like `_imap_uid_fetch_section` but ALSO returns the untagged response
    text so a test can assert the message attributes (FLAGS / MODSEQ) that
    accompanied the body — e.g. the implicit-`\\Seen` + CONDSTORE MODSEQ bump a
    non-PEEK BODY[] FETCH carries (RFC 9051 §6.4.5 + RFC 7162 §3.1.4). The
    FETCH writer emits those attributes on the same `* <seq> FETCH (...)` line
    that precedes the `BODY[] {n}` literal, so capturing every untagged line
    (literal payload returned separately) surfaces them.

    Returns (status, body_bytes, attr_text) where attr_text joins the untagged
    `* ...` lines with "\\n" (the literal bytes are NOT in attr_text)."""
    sock.sendall(f"{tag} UID FETCH {uid} {item}\r\n".encode())
    body = None
    untagged: list[str] = []
    while True:
        line = _recv_line(sock, buf, deadline)
        if line.startswith(tag + " "):
            return line.split(" ", 2)[1], body, "\n".join(untagged)
        untagged.append(line)
        m = _LITERAL_SUFFIX.search(line)
        if m:
            body = _recv_exact(sock, buf, int(m.group(1)), deadline)


_BINARY_SIZE_RE = re.compile(r"BINARY\.SIZE\[[^\]]*\]\s+(\d+)")


def _imap_uid_fetch_binary_size(sock, buf, tag: str, uid: int, item: str, deadline: float):
    """`UID FETCH <uid> BINARY.SIZE[N]` → (status, size_int). BINARY.SIZE
    returns the decoded octet count inline (no literal), so parse the number
    from the untagged `* <seq> FETCH (BINARY.SIZE[N] <n>)` response."""
    sock.sendall(f"{tag} UID FETCH {uid} {item}\r\n".encode())
    size = None
    while True:
        line = _recv_line(sock, buf, deadline)
        if line.startswith(tag + " "):
            return line.split(" ", 2)[1], size
        m = _BINARY_SIZE_RE.search(line)
        if m:
            size = int(m.group(1))


def _imap_wait_for_exists(sock, buf, deadline: float) -> int:
    """Read untagged responses until an unsolicited `* <n> EXISTS` arrives;
    return n. Used by the IDLE-push test."""
    while True:
        line = _recv_line(sock, buf, deadline)
        parts = line.split()
        if len(parts) >= 3 and parts[0] == "*" and parts[2].upper() == "EXISTS":
            return int(parts[1])


def _imap_read_until_fetch_modseq(sock, buf, deadline: float) -> list[str]:
    """Accumulate unsolicited IDLE response lines until one is a
    `* <seq> FETCH (… MODSEQ (<n>))` push, then return every line read
    (including the preceding `* <n> EXISTS`). Used by the CONDSTORE-IDLE
    test to assert the MODSEQ-bearing FETCH accompanied the EXISTS."""
    lines = []
    while True:
        line = _recv_line(sock, buf, deadline)
        lines.append(line)
        if "FETCH" in line and re.search(r"MODSEQ\s+\(\d+\)", line):
            return lines


def _imap_cmd(sock, buf, tag: str, cmd: str, deadline: float):
    """Drive a tagged command whose request and responses carry **no IMAP
    literals** (ENABLE / SELECT / STORE / EXPUNGE / GETQUOTAROOT — *not*
    APPEND or FETCH BODY[], which have dedicated literal-aware helpers).

    Returns (status, full_text) where full_text joins every untagged line and
    the tagged completion line with "\\n" — mirroring the Go fork tests'
    `command` helper so the same substring assertions transfer."""
    sock.sendall(f"{tag} {cmd}\r\n".encode())
    lines = []
    while True:
        line = _recv_line(sock, buf, deadline)
        lines.append(line)
        if line.startswith(tag + " "):
            return line.split(" ", 2)[1], "\n".join(lines)


def _parse_uid_set(s: str) -> set:
    """Expand an IMAP sequence-set (`1:3,5,7:9`) into a set of ints. `*` is
    not expanded (our membership checks never produce it)."""
    out = set()
    for part in s.split(","):
        part = part.strip()
        if not part or "*" in part:
            continue
        if ":" in part:
            lo_s, hi_s = part.split(":", 1)
            lo, hi = int(lo_s), int(hi_s)
            if lo > hi:
                lo, hi = hi, lo
            out.update(range(lo, hi + 1))
        else:
            out.add(int(part))
    return out


def _imap_uid_search(sock, buf, tag: str, criteria: str, deadline: float):
    """`UID SEARCH <criteria>` → (status, set_of_uids). Parses the IMAP4rev2
    `* ESEARCH (TAG "<tag>") UID ALL <set>` form the go-imap fork emits, and
    the legacy `* SEARCH <nums>` form as a fallback (empty set when neither
    carries results, i.e. no match)."""
    sock.sendall(f"{tag} UID SEARCH {criteria}\r\n".encode())
    uids: set = set()
    while True:
        line = _recv_line(sock, buf, deadline)
        if line.startswith(tag + " "):
            return line.split(" ", 2)[1], uids
        upper = line.upper()
        if "ESEARCH" in upper:
            m = re.search(r"\bALL\s+([0-9,:*]+)", line)
            if m:
                uids |= _parse_uid_set(m.group(1))
        elif upper.startswith("* SEARCH"):
            for tok in line.split()[2:]:
                if tok.isdigit():
                    uids.add(int(tok))


# ─────────────────────────────────────────────────────────────────────────
# DKIM-Signature parsing (DMARC-alignment assertions)
# ─────────────────────────────────────────────────────────────────────────


def dkim_signature_tag(raw: bytes, tag: str) -> str | None:
    """Return the value of ``<tag>=`` from the first DKIM-Signature header.

    Unfolds RFC-5322 continuation lines (the signature header is always
    folded across the `h=`/`b=`/`bh=` tags) before splitting on `;`, so a
    tag that lands on a continuation line is still found. Used to assert
    DMARC alignment (`d=` equals the From: domain); `d=` is a bare token
    with no internal whitespace, so the naive split is exact for it.

    Lifted here from ``tests/test_mail_bridge_mta.py::_dkim_sig_tag`` (priority
    #1/#4 — one DKIM parser, not a copy per test) so the process-level MTA test
    and the docker deploy round-trip tests share it.
    """
    tags = dkim_signature_tags(raw, tag)
    return tags[0] if tags else None


def dkim_signature_tags(raw: bytes, tag: str) -> list[str | None]:
    """The value of ``<tag>=`` from EVERY DKIM-Signature header, in header
    order (``None`` for a signature without it). The nest signs each message
    twice — RSA first, then Ed25519 (`mail-multidomain.md` § Per-domain DKIM →
    *Key shape*) — so ``s=`` reads ``["<label>-rsa", "<label>-ed25519"]``."""
    text = raw.decode("latin-1", "replace")
    hdr_end = text.find("\r\n\r\n")
    if hdr_end < 0:
        hdr_end = text.find("\n\n")
    header_block = text[: hdr_end if hdr_end >= 0 else len(text)]
    logical: list[str] = []
    for line in header_block.replace("\r\n", "\n").split("\n"):
        if line[:1] in (" ", "\t") and logical:
            logical[-1] += line
        else:
            logical.append(line)
    values: list[str | None] = []
    for h in logical:
        if h.lower().startswith("dkim-signature:"):
            value = None
            for part in h.split(":", 1)[1].split(";"):
                part = part.strip()
                if part.startswith(tag + "="):
                    value = part[len(tag) + 1 :].strip()
                    break
            values.append(value)
    return values


def dkim_selector_pair(label: str) -> list[str]:
    """The two DKIM selectors a selector label publishes under, in signing
    order — the test-side twin of shared Rust's
    ``fauna_mail::dns::per_domain::dkim_selector_pair``."""
    return [f"{label}-rsa", f"{label}-ed25519"]


def verify_each_dkim_signature(
    raw: bytes, domain: str, records: dict[str, str]
) -> tuple[list[str], list[bytes]]:
    """Verify EVERY DKIM-Signature on ``raw`` independently with dkimpy (an
    independent verifier, never our signer) and return ``(selectors whose
    signature verified, every name dkimpy queried)``.

    ``records`` maps a selector to the TXT value published at
    ``<selector>._domainkey.<domain>`` — the nest's ``list_dkim_selectors``
    values. The dkimpy ``dnsfunc`` answers ONLY those aligned names, so a
    signature with a misaligned ``d=``/``s=`` queries a name nobody answers
    and fails. A caller asserts the verified list equals the selectors it
    expects (both of a label's, RSA first) — the in-CI proxy for a receiver's
    ``dkim=pass`` on each algorithm.
    """
    import dkim  # dkimpy — independent verifier; fleet venv dep

    by_name = {
        f"{selector}._domainkey.{domain}.".encode(): value
        for selector, value in records.items()
    }
    queries: list[bytes] = []

    def dnsfunc(name, timeout=5):
        queries.append(name)
        return by_name.get(name)

    verifier = dkim.DKIM(raw)
    selectors = dkim_signature_tags(raw, "s")
    verified = [
        selector
        for idx, selector in enumerate(selectors)
        if verifier.verify(idx=idx, dnsfunc=dnsfunc)
    ]
    return verified, queries


# ─────────────────────────────────────────────────────────────────────────
# RFC 5322 header read + reply synthesis (the stub-MX auto-reply path)
# ─────────────────────────────────────────────────────────────────────────


def header_value(raw: bytes, name: str) -> str | None:
    """Return the (unfolded, whitespace-collapsed) value of the first header
    `name` in `raw`, or None if absent.

    Unfolds RFC 5322 continuation lines like `dkim_signature_tag` does, so a
    `Subject:`/`From:` that wraps across lines reads back as one logical value.
    Case-insensitive on the field name.
    """
    text = raw.decode("latin-1", "replace")
    hdr_end = text.find("\r\n\r\n")
    if hdr_end < 0:
        hdr_end = text.find("\n\n")
    header_block = text[: hdr_end if hdr_end >= 0 else len(text)]
    logical: list[str] = []
    for line in header_block.replace("\r\n", "\n").split("\n"):
        if line[:1] in (" ", "\t") and logical:
            logical[-1] += " " + line.strip()
        else:
            logical.append(line)
    prefix = name.lower() + ":"
    for h in logical:
        if h.lower().startswith(prefix):
            return h.split(":", 1)[1].strip()
    return None


def parse_addr(header: str | None) -> str:
    """Extract the bare `local@domain` address from an RFC 5322 address header
    value — `Display Name <a@b>` → `a@b`, a bare `a@b` → `a@b`. Returns "" when
    no address is parseable. Used to read the `From:` of a relayed-out message
    so a reply can be addressed back to exactly that sender (the replyability
    proof — a reply routes only if the From the client stamped is a real,
    routable mailbox)."""
    if not header:
        return ""
    if "<" in header and ">" in header:
        return header[header.index("<") + 1 : header.index(">")].strip()
    return header.strip()


def build_reply_message(
    received_raw: bytes, *, reply_nonce: str, reply_from: str
) -> tuple[str, bytes]:
    """Synthesize an external correspondent's REPLY to a relayed-out message.

    Reads the original message's `From:` (the reply recipient — the address the
    Fauna app stamped on its outbound), `Subject:`, and `Message-ID:`, and
    builds a standard RFC 5322 reply: `To:` the original From, `From:` the given
    external address, `Subject: Re: <orig> <reply_nonce>`, with `In-Reply-To`/
    `References` threading the original Message-ID. The `reply_nonce` lands in
    both the Subject and the body so a test can unambiguously match the reply as
    it renders decrypted in the recipient's client.

    Returns `(reply_to_addr, raw_reply_bytes)` — `reply_to_addr` is the parsed
    original From so the caller can both deliver the reply to it (RCPT TO) and
    assert it equals the sender's own mail address (replyability). The model is
    `helpers.stub_mx.StubMX.on_message`: an external MX that, on receiving a
    message, replies to its From.
    """
    reply_to = parse_addr(header_value(received_raw, "From"))
    orig_subject = header_value(received_raw, "Subject") or ""
    orig_subject = orig_subject[3:].strip() if orig_subject[:3].lower() == "re:" else orig_subject
    orig_msgid = header_value(received_raw, "Message-ID")
    lines = [
        f"From: External Correspondent <{reply_from}>",
        f"To: {reply_to}",
        f"Subject: Re: {orig_subject} {reply_nonce}",
        f"Message-ID: <{reply_nonce}@{reply_from.split('@', 1)[-1]}>",
        "Date: Mon, 01 Jun 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
    ]
    if orig_msgid:
        lines.append(f"In-Reply-To: {orig_msgid}")
        lines.append(f"References: {orig_msgid}")
    lines += ["", f"Reply body carrying {reply_nonce} from the external side."]
    return reply_to, ("\r\n".join(lines) + "\r\n").encode()


def _mx_accepts(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1.0):
            return True
    except OSError:
        return False


def _open_mail_gate_and_wait_for_the_mta(handle, budget_s: float = 120.0) -> None:
    """The admin opens the deployment's mail gate; return once the MTA serves.

    A venue starts with the gate shut, and the admin's act has TWO possible
    outcomes depending on a race this setup cannot see. The MTA is spawned
    without waiting to serve, and its first config fetch may land after the
    gate opens: it then serves at once and never exits. Or it has already
    idled: then it exits for rebind, and the venue's `rebind_after_enable`
    plays the supervisor. Either outcome is observable through the venue's own
    methods (the listener accepting, or `assert_mta_running` failing), so the
    wait ends on whichever state the MTA reaches. Waiting only for the exit
    timed out against an MTA that was already serving.
    """
    from helpers.waiting import wait_until

    handle.admin_opens_mail_gate()

    def _state():
        if _mx_accepts(handle.mx_port):
            return "serving"
        try:
            handle.assert_mta_running()
        except AssertionError:
            return "exited"
        return None

    state = wait_until(
        _state,
        budget_s,
        interval=0.5,
        diagnose=lambda: f"the MTA neither served nor exited for rebind; {handle.bridge_log_hint('mta')}",
    )
    if state == "exited":
        handle.rebind_after_enable(mda=False)


def submit_one_message(handle, rcpt: str, token: str, extra_rcpts=()) -> None:
    """AUTH PLAIN on 465 as the fixture's sender; one message tagged `token`,
    to `rcpt` and any `extra_rcpts` in one transaction."""
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + handle.submission_credential
    ).decode()
    body = "\r\n".join([
        f"From: Sender <{sender}>",
        f"To: {rcpt}",
        f"Subject: mail health {token}",
        f"Message-ID: <{token}@{domain}>",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"mail health e2e marker {token}.",
    ]) + "\r\n"
    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        for r in (rcpt, *extra_rcpts):
            conn.cmd(f"RCPT TO:<{r}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

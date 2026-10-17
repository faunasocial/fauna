"""tier_3 witnesses for the delivery marks on the copy a user reads.

Two `[nest]` outcomes of `docs/features/mail-abuse-refused.md`, observed the
way the user observes them — over IMAP, on the decrypted copy — through the
shared inbound→IMAP seam (`mail_bridge_inbound_to_imap`: a real nest, a real
Go MTA taking the message on port 25 and a real MDA serving it):

- a sender cannot plant the nest's own `X-Fauna-*` delivery marks
  (`smtp-server.md` § Architectural rules); the copy carries only the marks the
  nest wrote;
- clean mail arrives carrying its scan result (`mail-content-scanning.md`
  § How the result is sealed).
"""

import time

import pytest

from helpers.mail_wire import (
    _connect_smtp_starttls,
    _imap_auth_plain,
    _imap_cmd,
    _imap_uid_fetch_body,
    _imap_uid_search,
    _imaps_connect,
)

pytestmark = pytest.mark.tier_3


def _deliver(handle, raw: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _read_back(handle, nonce: str, deadline: float) -> bytes:
    """The decrypted copy carrying `nonce`, from INBOX, polled until it lands."""
    sock, buf = _imaps_connect(handle.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the recipient"
        uids = []
        n = 0
        while not uids:
            n += 1
            st, _ = _imap_cmd(sock, buf, f"s{n}", "SELECT INBOX", deadline)
            assert st == "OK", f"SELECT INBOX: {st}"
            _, uids = _imap_uid_search(sock, buf, f"q{n}", f'BODY "{nonce}"', deadline)
            if not uids:
                assert time.monotonic() < deadline, f"message {nonce} never reached INBOX"
                time.sleep(0.5)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14)
        st, body = _imap_uid_fetch_body(sock, buf, "f1", max(uids), deadline)
        assert st == "OK" and body, f"UID FETCH BODY[]: {st}"
        sock.sendall(b"z LOGOUT\r\n")
        return body


def _headers(raw: bytes) -> list[tuple[str, str]]:
    head = raw.split(b"\r\n\r\n", 1)[0].decode("utf-8", errors="replace")
    out = []
    for line in head.split("\r\n"):
        if line[:1] in (" ", "\t") and out:
            out[-1] = (out[-1][0], out[-1][1] + " " + line.strip())
        elif ":" in line:
            name, value = line.split(":", 1)
            out.append((name.strip(), value.strip()))
    return out


def _message(handle, nonce: str, extra: list[str]) -> bytes:
    lines = [
        *extra,
        "From: External Sender <sender@external.test>",
        f"To: {handle.recipient_username}",
        f"Subject: stamps {nonce}",
        f"Message-ID: <{nonce}@external.test>",
        "Date: Thu, 08 Oct 2026 12:00:00 +0000",
        "",
        f"body {nonce}",
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


@pytest.mark.feature("mail-abuse-refused")
def test_a_sender_cannot_plant_the_nests_delivery_marks(mail_bridge_inbound_to_imap):
    """Forged `X-Fauna-Scan-Clamav: infected-forged` and `X-Fauna-Address-Matched`
    marks are gone from the copy the user reads; the one scan mark it carries is
    the nest's own."""
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    deadline = time.monotonic() + 120.0
    nonce = f"plantedmarks{time.time_ns()}"
    planted = ("infected-planted", "planted-alias@example.invalid")
    _deliver(handle, _message(handle, nonce, [
        f"X-Fauna-Scan-Clamav: {planted[0]}",
        f"X-Fauna-Address-Matched: {planted[1]}",
    ]), deadline)
    headers = _headers(_read_back(handle, nonce, deadline))
    assert not any(v in planted for _, v in headers), (
        f"no sender-planted mark may survive to the copy the user reads; got {headers!r}"
    )
    clamav = [v for n, v in headers if n.lower() == "x-fauna-scan-clamav"]
    assert clamav == ["clean"], (
        f"the copy must carry exactly the nest's own scan mark; got {clamav!r}"
    )
    assert not any(n.lower() == "x-fauna-address-matched" for n, _ in headers), (
        f"a planted address mark must be stripped (the message reached an exact "
        f"address, so the nest wrote none); got {headers!r}"
    )


@pytest.mark.feature("mail-abuse-refused")
def test_clean_mail_reaches_you_carrying_the_result_of_its_scan(mail_bridge_inbound_to_imap):
    """A clean message read back over IMAP carries the malware scan's verdict,
    `X-Fauna-Scan-Clamav: clean`, sealed with it at delivery."""
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    deadline = time.monotonic() + 120.0
    nonce = f"scanresult{time.time_ns()}"
    _deliver(handle, _message(handle, nonce, []), deadline)
    headers = _headers(_read_back(handle, nonce, deadline))
    clamav = [v for n, v in headers if n.lower() == "x-fauna-scan-clamav"]
    assert clamav == ["clean"], (
        f"clean mail must reach the user carrying its scan result; got {headers!r}"
    )

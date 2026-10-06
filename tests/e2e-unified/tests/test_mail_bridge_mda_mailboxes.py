"""Tier 3: what a standard IMAP mail app sees of its mailboxes and sign-in —
witnessed from outside over the real MDA bridge (`mail_bridge_mda`) and a real
nest, per `docs/goal/behavior/imap-server.md`.

Covers the `standard-mail-apps` outcomes the protocol-level suite in
`test_mail_bridge_mda.py` leaves unwitnessed: the cleartext-auth refusal on 143,
the `LOGIN` command and the rev1 dialect over TLS, the six special-use mailboxes
(seeded, flagged, undeletable, unrenamable), and `INTERNALDATE` as the
server-assigned time a backdated `Date:` cannot move. Every assertion reads a
tagged completion or a response the server wrote before it — convention 14.

`mail_bridge_mda` is session-scoped: tests use fresh mailbox names and
UID-addressed assertions so accumulated state from sibling tests never matters.
"""

import base64
import re
import secrets
import socket
import time

import pytest

from helpers.mail_wire import (
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imap_read_tagged,
    _imap_uid_search,
    _imaps_connect,
    _recv_line,
)

pytestmark = pytest.mark.tier_3

SPECIAL_USE = {
    "INBOX": None,  # INBOX is identified by name (RFC 9051); \\Inbox is optional
    "Archive": "\\Archive",
    "Drafts": "\\Drafts",
    "Sent": "\\Sent",
    "Trash": "\\Trash",
    "Junk": "\\Junk",
}


def _signed_in(handle, deadline):
    sock, buf = _imaps_connect(handle, deadline)
    status = _imap_auth_plain(sock, buf, "a1", handle.recipient_username,
                              handle.recipient_password, deadline)
    assert status == "OK", f"AUTH PLAIN must succeed; got {status}"
    return sock, buf


@pytest.mark.feature("standard-mail-apps")
def test_cleartext_143_refuses_every_password_path(mail_bridge_mda):
    """Before STARTTLS on 143 the server advertises `LOGINDISABLED`, offers no
    `AUTH=` mechanism, and refuses both `AUTHENTICATE PLAIN` (with its initial
    response inline) and `LOGIN` — a password never crosses in cleartext
    (§ Authentication; § Don't do these)."""
    handle = mail_bridge_mda
    deadline = time.monotonic() + 40.0
    sock = socket.create_connection(("127.0.0.1", handle.imap_starttls_port), timeout=15.0)
    buf = bytearray()
    with sock:
        greeting = _recv_line(sock, buf, deadline)
        assert greeting.startswith("* OK"), greeting
        status, caps = _imap_cmd(sock, buf, "c1", "CAPABILITY", deadline)
        assert status == "OK", caps
        assert "LOGINDISABLED" in caps.upper(), f"pre-TLS must advertise LOGINDISABLED: {caps}"
        assert "AUTH=" not in caps.upper(), f"pre-TLS must offer no AUTH= mechanism: {caps}"

        ir = base64.b64encode(
            b"\x00" + handle.recipient_username.encode() + b"\x00"
            + handle.recipient_password.encode()).decode()
        sock.sendall(f"c2 AUTHENTICATE PLAIN {ir}\r\n".encode())
        status, untagged = _imap_read_tagged(sock, buf, "c2", deadline)
        assert status in ("NO", "BAD"), f"cleartext AUTHENTICATE must fail; got {status}"
        assert not any(ln.startswith("+") for ln in untagged), untagged

        status, text = _imap_cmd(
            sock, buf, "c3",
            f'LOGIN "{handle.recipient_username}" "{handle.recipient_password}"', deadline)
        assert status in ("NO", "BAD"), f"cleartext LOGIN must fail; got {status}: {text}"


@pytest.mark.feature("standard-mail-apps")
def test_login_command_and_rev1_dialect_over_tls(mail_bridge_mda):
    """Over TLS an older mail app's plain `LOGIN` command signs in, and the
    server announces the IMAP4rev1 dialect alongside rev2 (§ Capabilities;
    § Authentication)."""
    handle = mail_bridge_mda
    deadline = time.monotonic() + 40.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        status, caps = _imap_cmd(sock, buf, "l1", "CAPABILITY", deadline)
        assert status == "OK", caps
        assert "IMAP4REV1" in caps.upper(), f"rev1 must be advertised: {caps}"
        status, text = _imap_cmd(
            sock, buf, "l2",
            f'LOGIN "{handle.recipient_username}" "{handle.recipient_password}"', deadline)
        assert status == "OK", f"LOGIN over TLS must sign in; got {status}: {text}"
        status, text = _imap_cmd(sock, buf, "l3", "SELECT INBOX", deadline)
        assert status == "OK", f"a LOGIN session must be usable; got {status}: {text}"


@pytest.mark.feature("standard-mail-apps")
def test_six_special_use_mailboxes_seeded_and_protected(mail_bridge_mda):
    """After sign-in the six standard mailboxes are listed with their RFC 6154
    attributes, and none of them can be deleted or renamed (§ Standard
    mailboxes)."""
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    sock, buf = _signed_in(handle, deadline)
    with sock:
        status, listing = _imap_cmd(sock, buf, "m1", 'LIST "" "*"', deadline)
        assert status == "OK", listing
        by_name = {}
        for ln in listing.splitlines():
            m = re.match(r'\* LIST \(([^)]*)\) "?[^ ]*"? "?([^"]+)"?$', ln)
            if m:
                by_name[m.group(2)] = m.group(1)
        for name, attr in SPECIAL_USE.items():
            assert name in by_name, f"{name} must be seeded; listing=\n{listing}"
            if attr:
                assert attr.lower() in by_name[name].lower(), (
                    f"{name} must carry {attr}; got ({by_name[name]})")
        for i, name in enumerate(SPECIAL_USE):
            status, text = _imap_cmd(sock, buf, f"d{i}", f"DELETE {name}", deadline)
            assert status == "NO", f"DELETE {name} must be refused; got {status}: {text}"
            if name == "INBOX":
                # RFC 9051 §6.3.6: RENAME INBOX moves its contents out and
                # re-seeds an empty INBOX — INBOX itself survives by design.
                continue
            status, text = _imap_cmd(
                sock, buf, f"r{i}", f"RENAME {name} Renamed{secrets.token_hex(2)}", deadline)
            assert status == "NO", f"RENAME {name} must be refused; got {status}: {text}"


@pytest.mark.feature("standard-mail-apps")
def test_internaldate_is_server_time_not_the_date_header(mail_bridge_mda):
    """A message whose `Date:` header claims 1999 is dated by the server when
    stored: `INTERNALDATE` is today's, and `SEARCH BEFORE 1-Jan-2000` — which
    runs on the internal date — does not find it (§ SEARCH: SINCE / BEFORE are
    `internal_date` predicates). A sender cannot backdate mail out of view."""
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: backdater@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: backdated " + secrets.token_hex(3).encode() + b"\r\n"
        b"Date: Fri, 01 Jan 1999 00:00:00 +0000\r\n"
        b"\r\n"
        b"old news\r\n"
    )
    sock, buf = _signed_in(handle, deadline)
    with sock:
        status, uid = _imap_append(sock, buf, "a2", "INBOX", message, deadline)
        assert status == "OK" and uid is not None, f"APPEND: {status} uid={uid}"
        status, text = _imap_cmd(sock, buf, "a3", "SELECT INBOX", deadline)
        assert status == "OK", text
        status, text = _imap_cmd(sock, buf, "a4", f"UID FETCH {uid} (INTERNALDATE)", deadline)
        assert status == "OK", text
        # RFC 3501 `date-day-fixed = (SP DIGIT) / 2DIGIT`: days 1-9 come
        # space-padded (`" 6-Oct-2026 …"`), so the pad is part of the grammar.
        m = re.search(r'INTERNALDATE "[ ]?(\d{1,2})-(\w{3})-(\d{4})', text)
        assert m, f"FETCH must return INTERNALDATE: {text}"
        assert int(m.group(3)) >= 2026, (
            f"INTERNALDATE must be the server's receipt time, not the 1999 header: {text}")
        status, old = _imap_uid_search(sock, buf, "a5", "BEFORE 1-Jan-2000", deadline)
        assert status == "OK"
        assert uid not in old, "BEFORE must search the internal date, not the Date: header"
        status, recent = _imap_uid_search(sock, buf, "a6", "SINCE 1-Jan-2026", deadline)
        assert status == "OK"
        assert uid in recent, "SINCE must find the message by its receipt date"


@pytest.mark.feature("standard-mail-apps")
def test_mua_ahead_of_restored_nest_is_told_to_resync(mail_bridge_mda):
    """A mail app holding newer state than the nest (the post-restore window:
    its remembered modseq is ahead of the mailbox's) re-SELECTs with QRESYNC and
    is answered with the nest's own, lower `HIGHESTMODSEQ` and no partial delta —
    RFC 7162 §3.2.5.2's stale-modseq signal, so the app resyncs to what the nest
    holds instead of mixing its newer cache with the restored state (§ Restore
    divergence detection)."""
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    sock, buf = _signed_in(handle, deadline)
    with sock:
        status, resp = _imap_cmd(sock, buf, "q1", "ENABLE QRESYNC", deadline)
        assert status == "OK" and "ENABLED QRESYNC" in resp, resp
        status, resp = _imap_cmd(sock, buf, "q2", "SELECT INBOX", deadline)
        assert status == "OK", resp
        uidvalidity = re.search(r"UIDVALIDITY\s+(\d+)", resp).group(1)
        server_modseq = int(re.search(r"HIGHESTMODSEQ\s+(\d+)", resp).group(1))
        status, resp = _imap_cmd(sock, buf, "q3", "UNSELECT", deadline)
        assert status == "OK", resp

        ahead = server_modseq + 1_000_000
        status, resp = _imap_cmd(
            sock, buf, "q4", f"SELECT INBOX (QRESYNC ({uidvalidity} {ahead}))", deadline)
        assert status == "OK", f"a stale-ahead QRESYNC SELECT still selects; got {resp}"
        reported = int(re.search(r"HIGHESTMODSEQ\s+(\d+)", resp).group(1))
        assert reported < ahead, (
            f"the nest's own HIGHESTMODSEQ ({reported}) must come back, never the "
            f"app's newer claim ({ahead}); got {resp}")
        assert "VANISHED" not in resp and " FETCH " not in resp, (
            f"no partial delta may be sent against a modseq the nest never had; got {resp}")

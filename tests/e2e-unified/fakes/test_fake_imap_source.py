"""Self-test for `fake_imap_source`: it really speaks TLS IMAP4rev1, and it
answers the exact command shapes `libs/fauna-mail/src/imap_client/session.rs`
sends.

A fake that a journey test leans on is worth its own witness — a subtly wrong
response here surfaces later as "the mail-import wizard is broken", which is the
most expensive possible way to learn about a typo in a `* LIST` line.

Two independent halves:

  1. **Conformance.** Python's own `imaplib` — an RFC 3501 client that has never
     heard of fauna — completes a real TLS session against the fake and reads the
     mailboxes and a message body back. If the framing were wrong, a
     general-purpose client is the thing that notices.
  2. **The exact shapes our client sends.** `imaplib` does not issue
     `UID FETCH <n>:* (UID RFC822.SIZE)`, nor pipeline four FETCHes, so those go
     over a raw TLS socket spelled the way `session.rs` spells them.

tier_1: pure in-process sockets, no nest, no driver, no app.
"""

from __future__ import annotations

import imaplib
import re
import ssl

import pytest

from fake_imap_source import (
    FakeImapSource,
    SourceMailbox,
    mint_localhost_chain,
    sample_message,
    session_ca_pem,
    session_cert,
)

pytestmark = pytest.mark.tier_1


def _mailboxes() -> list[SourceMailbox]:
    return [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message("first"), sample_message("second")],
        ),
        SourceMailbox(name="Archive", messages=[sample_message("archived")]),
    ]


def _client_ctx(source: FakeImapSource) -> ssl.SSLContext:
    """A client context trusting exactly the fake's minted CA — the Python-side
    twin of what the source-IMAP trust seed does for the app."""
    ctx = ssl.create_default_context()
    ctx.load_verify_locations(cadata=source.cert_pem)
    return ctx


def test_a_stock_imap_client_completes_a_session_over_real_tls():
    """`imaplib` logs in, lists, examines and fetches — nothing fauna-specific
    involved, so this is a statement about RFC conformance, not about us."""
    with FakeImapSource(_mailboxes()) as source:
        client = imaplib.IMAP4_SSL(
            host="localhost", port=source.port, ssl_context=_client_ctx(source)
        )
        try:
            status, _ = client.login(source.username, source.password)
            assert status == "OK"

            status, boxes = client.list()
            assert status == "OK"
            names = {line.decode().rsplit(" ", 1)[-1].strip('"') for line in boxes}
            assert {"INBOX", "Archive"} <= names, names

            status, counts = client.select("INBOX", readonly=True)
            assert status == "OK"
            assert counts[0] == b"2"

            status, data = client.uid("FETCH", "1", "(UID FLAGS INTERNALDATE BODY.PEEK[])")
            assert status == "OK"
            body = b"".join(part[1] for part in data if isinstance(part, tuple))
            assert b"Subject: first" in body
        finally:
            client.logout()

        assert source.logins == [(source.username, source.password)]
        assert ("INBOX", 1) in source.fetched_uids


def test_bad_credentials_are_refused_with_the_servers_own_error():
    """§ Wizard steps 2 has the wizard surface the source's error verbatim and
    let the user retry, so the fake must actually say NO rather than accept
    anything."""
    with FakeImapSource(_mailboxes()) as source:
        client = imaplib.IMAP4_SSL(
            host="localhost", port=source.port, ssl_context=_client_ctx(source)
        )
        with pytest.raises(imaplib.IMAP4.error, match="AUTHENTICATIONFAILED"):
            client.login(source.username, "wrong-password")


def _raw_session(source: FakeImapSource) -> ssl.SSLSocket:
    import socket

    raw = socket.create_connection(("localhost", source.port), timeout=10)
    sock = _client_ctx(source).wrap_socket(raw, server_hostname="localhost")
    _read_until_tagged(sock, None)  # the greeting
    return sock


def _read_until_tagged(sock: ssl.SSLSocket, tag: str | None) -> bytes:
    """Read until `tag`'s completion line, or one line when `tag` is None."""
    buf = b""
    while True:
        chunk = sock.recv(8192)
        if not chunk:
            return buf
        buf += chunk
        if tag is None:
            if b"\r\n" in buf:
                return buf
        elif re.search(rf"(?m)^{re.escape(tag)} (OK|NO|BAD)".encode(), buf):
            return buf


def test_the_enumerate_and_fetch_shapes_our_client_sends_are_answered():
    """The two `UID FETCH` spellings from `session.rs`, verbatim.

    `imaplib` issues neither: `enumerate_uids` asks for `<n>:* (UID
    RFC822.SIZE)` — the resume-aware enumeration whose `<n>:*` semantics §
    Resume protocol depends on — and `fetch_messages` pipelines. A fake that
    only satisfies `imaplib` would pass half 1 and hang the real client.
    """
    with FakeImapSource(_mailboxes()) as source:
        sock = _raw_session(source)
        try:
            sock.sendall(f"a1 LOGIN {source.username} {source.password}\r\n".encode())
            assert b"a1 OK" in _read_until_tagged(sock, "a1")

            sock.sendall(b'a2 EXAMINE "INBOX"\r\n')
            examine = _read_until_tagged(sock, "a2")
            assert b"* 2 EXISTS" in examine
            assert b"[UIDVALIDITY 42]" in examine
            assert b"[READ-ONLY]" in examine

            # enumerate_uids from the start of the mailbox.
            sock.sendall(b"a3 UID FETCH 1:* (UID RFC822.SIZE)\r\n")
            enum_all = _read_until_tagged(sock, "a3")
            assert b"UID 1 RFC822.SIZE" in enum_all
            assert b"UID 2 RFC822.SIZE" in enum_all

            # enumerate_uids resuming past the first message: the cursor must
            # actually filter, or a resume re-imports what it already has.
            sock.sendall(b"a4 UID FETCH 2:* (UID RFC822.SIZE)\r\n")
            enum_resumed = _read_until_tagged(sock, "a4")
            assert b"UID 2 RFC822.SIZE" in enum_resumed
            assert b"UID 1 RFC822.SIZE" not in enum_resumed

            # Pipelined body fetches: both commands go out before either reply
            # is read, which is what `fetch_messages` does under its concurrency
            # cap. A fake answering one line per recv() deadlocks here.
            sock.sendall(
                b"a5 UID FETCH 1 (UID FLAGS INTERNALDATE BODY.PEEK[])\r\n"
                b"a6 UID FETCH 2 (UID FLAGS INTERNALDATE BODY.PEEK[])\r\n"
            )
            both = _read_until_tagged(sock, "a6")
            assert b"a5 OK" in both, both
            assert b"Subject: first" in both
            assert b"Subject: second" in both
            assert b"INTERNALDATE" in both
        finally:
            sock.close()

        assert source.fetched_uids == [("INBOX", 1), ("INBOX", 2)]


def test_an_unknown_command_is_refused_rather_than_dropped():
    """Convention 11's rule, applied to the fake: an unanswered tag is a client
    hang, and a hang diagnoses nothing."""
    with FakeImapSource(_mailboxes()) as source:
        sock = _raw_session(source)
        try:
            sock.sendall(b"z1 FROBNICATE everything\r\n")
            assert b"z1 BAD" in _read_until_tagged(sock, "z1")
        finally:
            sock.close()


def test_starttls_mode_upgrades_a_cleartext_connection():
    """§ The two TLS modes ratifies both source modes, so the fake serves both —
    otherwise a Generic-IMAP-over-STARTTLS journey has nothing to point at."""
    import socket

    with FakeImapSource(_mailboxes(), tls_mode="starttls") as source:
        raw = socket.create_connection(("localhost", source.port), timeout=10)
        try:
            assert b"* OK" in raw.recv(4096)
            raw.sendall(b"b1 STARTTLS\r\n")
            assert b"b1 OK" in raw.recv(4096)

            sock = _client_ctx(source).wrap_socket(raw, server_hostname="localhost")
            sock.sendall(f"b2 LOGIN {source.username} {source.password}\r\n".encode())
            assert b"b2 OK" in _read_until_tagged(sock, "b2")
            sock.close()
        finally:
            raw.close()


def test_the_seeded_anchor_is_a_ca_and_the_served_cert_is_not():
    """The anchor and the leaf must be two different certificates.

    A rustls client refuses a certificate carrying ``BasicConstraints(ca=True)``
    in the end-entity position — ``invalid peer certificate:
    Other(OtherError(CaUsedAsEndEntity))``. The fixture used to mint ONE
    self-signed ``ca=True`` cert and use it as both the seeded anchor and the
    served cert, so every mail-import walk failed its TLS handshake while
    reporting the generic ``nest unreachable: TLS handshake with localhost
    failed`` — an error that reads like the app's *nest* link, not its source
    link, and cost the walk's first run its whole budget chasing the wrong
    connection.

    ``imaplib`` in the sibling tests above cannot catch this: it verifies with
    OpenSSL, which accepts a CA cert as an end-entity. Only the property itself
    is portable, so this asserts the property.
    """
    from cryptography import x509

    ca_pem, leaf_pem, _key_pem = mint_localhost_chain()
    ca = x509.load_pem_x509_certificate(ca_pem.encode())
    leaf = x509.load_pem_x509_certificate(leaf_pem.encode())

    assert ca.extensions.get_extension_for_class(x509.BasicConstraints).value.ca, (
        "the seeded anchor must be a CA"
    )
    assert not leaf.extensions.get_extension_for_class(
        x509.BasicConstraints
    ).value.ca, (
        "the SERVED certificate must not be a CA — rustls refuses it with "
        "CaUsedAsEndEntity, and the wizard reports that as a nest-link failure"
    )
    assert leaf.issuer == ca.subject, "the leaf must chain to the seeded anchor"

    # And the two session accessors hand out the right halves of that chain.
    served_cert, _served_key = session_cert()
    assert not x509.load_pem_x509_certificate(
        served_cert.encode()
    ).extensions.get_extension_for_class(x509.BasicConstraints).value.ca
    assert x509.load_pem_x509_certificate(
        session_ca_pem().encode()
    ).extensions.get_extension_for_class(x509.BasicConstraints).value.ca

    # The SANs live on the leaf, where the client looks: the wizard's host field
    # may hold either spelling and both must verify.
    names = leaf.extensions.get_extension_for_class(
        x509.SubjectAlternativeName
    ).value
    assert "localhost" in names.get_values_for_type(x509.DNSName)
    assert str(names.get_values_for_type(x509.IPAddress)[0]) == "127.0.0.1"

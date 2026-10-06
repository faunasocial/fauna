"""A minimal TLS IMAP4rev1 server standing in for the *foreign* mailbox the
mail-import wizard migrates away from.

`docs/goal/behavior/mailbox-migration.md` § Client-driven streaming model: the
source server is the one participant in an import that is genuinely not ours —
Gmail, iCloud, a self-hosted Dovecot. Everything else in a tier_3 import run is
real (real app, real shared-Rust IMAP client, real nest), and this fake is the
external system at the far end, exactly as `fake_clamd` is for the scan gate.

**It speaks only what the client actually sends.** The command set is derived
from `libs/fauna-mail/src/imap_client/session.rs`, not from RFC 3501 at large:

    CAPABILITY                                   (after connect, and after LOGIN)
    LOGIN <user> <pass>
    LIST "" "*"                                  → list_mailboxes
    EXAMINE <mailbox>                            → examine (read-only, never SELECT)
    UID FETCH <n>:* (UID RFC822.SIZE)            → enumerate_uids
    UID FETCH <uid> (UID FLAGS INTERNALDATE BODY.PEEK[])
                                                 → fetch_messages
    LOGOUT

Anything else gets a tagged ``BAD``, which is the honest answer and surfaces as
a wizard error rather than a hang — convention 11's rule applied to a fake:
never silently drop a command.

**TLS is not optional here** and that is the whole reason this file exists.
§ The two TLS modes ratifies that a source session has no plaintext variant —
the user's password crosses it — so the fake terminates real TLS with a cert
minted per instance, and the app trusts it through the source-IMAP trust seed
(`e2e-automation-surface-gating.md` § The source-IMAP trust seed). Both ratified
modes are served: ``implicit`` (TLS from the first byte, the 993 default) and
``starttls`` (negotiated over a cleartext connection).

**Pipelining is load-bearing.** `fetch_messages` keeps up to 4 UID FETCHes
outstanding at once (§ Throttling), so several commands arrive in one read. The
reader below drains every complete line it holds before reading again; replying
to only the first would deadlock the client against its own concurrency cap.
"""

from __future__ import annotations

import datetime
import ipaddress
import re
import socket
import ssl
import tempfile
import threading
from dataclasses import dataclass, field
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519
from cryptography.x509.oid import NameOID

# The client filters `\Recent` out itself (nest's `validate_item` rejects a
# message carrying it), so serving it here exercises that filter rather than
# tripping it.
DEFAULT_FLAGS = ("\\Seen",)


def mint_localhost_chain() -> tuple[str, str, str]:
    """A real two-cert chain, as ``(ca_pem, leaf_cert_pem, leaf_key_pem)``.

    **Why a chain and not one self-signed cert.** The obvious shape — mint one
    self-signed cert, seed it as the trust anchor, and serve it — does not work
    against a rustls client, and fails in a way that reads like a trust-seed
    bug rather than a fixture bug: webpki refuses a certificate carrying
    ``BasicConstraints(ca=True)`` in the *end-entity* position and returns
    ``invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))``. That
    error surfaces to the wizard as the generic ``nest unreachable: TLS
    handshake with localhost failed``, which sent the mail-import walk's first
    run chasing the app's nest link — the source connection is the one that was
    failing. So the anchor and the served leaf must be two different
    certificates, exactly as they are in production.

    The CA is the value to seed (``FAUNA_E2E_IMAP_EXTRA_CA_PEM``); the leaf is
    what the server presents. Only the leaf carries the SANs — ``localhost``
    *and* ``127.0.0.1``, because the client dials by whichever the wizard's host
    field holds, and a cert valid for only one of them fails the other dial with
    an error that also reads like a trust-seed bug.

    Ed25519 rather than RSA: minting is per-session and an RSA-2048 keygen is
    hundreds of milliseconds of pure latency. rustls verifies Ed25519 anchors
    and end-entity certs fine.
    """
    now = datetime.datetime.now(datetime.timezone.utc)

    ca_key = ed25519.Ed25519PrivateKey.generate()
    ca_name = x509.Name(
        [x509.NameAttribute(NameOID.COMMON_NAME, "fauna-e2e-source-ca")]
    )
    ca = (
        x509.CertificateBuilder()
        .subject_name(ca_name)
        .issuer_name(ca_name)
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .sign(ca_key, None)
    )

    leaf_key = ed25519.Ed25519PrivateKey.generate()
    leaf = (
        x509.CertificateBuilder()
        .subject_name(
            x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "localhost")])
        )
        .issuer_name(ca_name)
        .public_key(leaf_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(
            x509.SubjectAlternativeName(
                [
                    x509.DNSName("localhost"),
                    x509.IPAddress(ipaddress.ip_address("127.0.0.1")),
                ]
            ),
            critical=False,
        )
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .sign(ca_key, None)
    )

    pem = serialization.Encoding.PEM
    return (
        ca.public_bytes(pem).decode(),
        leaf.public_bytes(pem).decode(),
        leaf_key.private_bytes(
            encoding=pem,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=serialization.NoEncryption(),
        ).decode(),
    )


def mint_localhost_cert() -> tuple[str, str]:
    """The served half of a fresh chain, as ``(leaf_cert_pem, leaf_key_pem)``.

    The anchor that would verify it is discarded, which is right for a caller
    that drives the fake directly without verifying — and wrong for anything an
    already-running app must verify, which is what :func:`session_cert` and
    :func:`session_ca_pem` are for.
    """
    _ca_pem, leaf_pem, key_pem = mint_localhost_chain()
    return leaf_pem, key_pem


_SESSION_CHAIN: tuple[str, str, str] | None = None


def _session_chain() -> tuple[str, str, str]:
    """The pytest session's one source-server chain, minted on first use.

    The app reads its trust anchor from the launch environment, and app drivers
    are session-scoped (`conftest._driver_cache`) — so the anchor is fixed
    before the first app starts and cannot be re-seeded later. One CA per
    session, shared by every `FakeImapSource` in it, is what makes that work;
    a per-instance mint would leave every server after the first untrusted.
    """
    global _SESSION_CHAIN
    if _SESSION_CHAIN is None:
        _SESSION_CHAIN = mint_localhost_chain()
    return _SESSION_CHAIN


def session_ca_pem() -> str:
    """The session CA to seed as the app's extra trust anchor.

    This is the CA, never the leaf: seeding the served certificate itself is
    what produced ``CaUsedAsEndEntity`` (see :func:`mint_localhost_chain`).
    """
    return _session_chain()[0]


def session_cert() -> tuple[str, str]:
    """The session's server certificate, as ``(leaf_cert_pem, leaf_key_pem)`` —
    what a `FakeImapSource` presents, verifiable against
    :func:`session_ca_pem`."""
    return _session_chain()[1], _session_chain()[2]


@dataclass
class SourceMailbox:
    """One mailbox the fake serves.

    `messages` are raw RFC 5322 bytes; their UIDs are assigned in order from
    `first_uid`, which is what makes the resume cursor meaningful.
    """

    name: str
    messages: list[bytes] = field(default_factory=list)
    selectable: bool = True
    uid_validity: int = 42
    first_uid: int = 1

    def uids(self) -> list[int]:
        return [self.first_uid + i for i in range(len(self.messages))]

    def body_for(self, uid: int) -> bytes | None:
        idx = uid - self.first_uid
        if 0 <= idx < len(self.messages):
            return self.messages[idx]
        return None


def sample_message(subject: str, body: str = "hello from the old mailbox") -> bytes:
    """A small, well-formed RFC 5322 message.

    A real `Message-ID` matters: the client derives the per-actor dedup key from
    the parsed message (§ Dedup), so a malformed header would make an import
    *look* like it skipped everything.
    """
    slug = re.sub(r"[^a-z0-9]+", "-", subject.lower()).strip("-") or "m"
    return (
        f"From: someone@example.com\r\n"
        f"To: user@example.com\r\n"
        f"Subject: {subject}\r\n"
        f"Message-ID: <{slug}@example.com>\r\n"
        f"Date: Wed, 01 Jan 2020 00:00:00 +0000\r\n"
        f"\r\n"
        f"{body}\r\n"
    ).encode()


class FakeImapSource:
    """A loopback TLS IMAP server serving a fixed set of mailboxes.

    Not a fake of a fauna binary — a stand-in for the third-party server on the
    far side of an import, which is why a test using it stays tier_3.
    """

    def __init__(
        self,
        mailboxes: list[SourceMailbox],
        *,
        username: str = "olduser",
        password: str = "oldpass",
        tls_mode: str = "implicit",
        host: str = "127.0.0.1",
        cert: tuple[str, str] | None = None,
    ) -> None:
        if tls_mode not in ("implicit", "starttls"):
            raise ValueError(f"tls_mode must be implicit|starttls, got {tls_mode!r}")
        self.mailboxes = {m.name: m for m in mailboxes}
        self.username = username
        self.password = password
        self.tls_mode = tls_mode

        # `cert` lets a caller pin the session CA the app was launched trusting
        # (see `session_cert`). Left None, each instance mints its own — right
        # for a self-test that drives the fake directly, wrong for anything an
        # already-running app has to verify.
        self.cert_pem, self._key_pem = cert if cert is not None else mint_localhost_cert()
        # `ssl` wants files, and the PEMs must outlive every handshake, so the
        # temp dir is owned by the instance and cleaned in stop().
        self._tmp = tempfile.TemporaryDirectory(prefix="fake-imap-source-")
        cert_path = Path(self._tmp.name) / "cert.pem"
        key_path = Path(self._tmp.name) / "key.pem"
        cert_path.write_text(self.cert_pem)
        key_path.write_text(self._key_pem)
        self._ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self._ctx.load_cert_chain(str(cert_path), str(key_path))

        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind((host, 0))
        self._sock.listen(8)
        self.host, self.port = self._sock.getsockname()[:2]

        # Observability for the tests: what the client actually asked for. A
        # journey test asserting "the import really read the source" wants this
        # rather than a screenshot.
        self.logins: list[tuple[str, str]] = []
        self.fetched_uids: list[tuple[str, int]] = []

        self._stopped = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)

    def start(self) -> "FakeImapSource":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stopped.set()
        try:
            self._sock.close()
        except OSError:
            pass
        self._tmp.cleanup()

    def __enter__(self) -> "FakeImapSource":
        return self.start()

    def __exit__(self, *_exc: object) -> None:
        self.stop()

    # ── internals ─────────────────────────────────────────────────────

    def _serve(self) -> None:
        while not self._stopped.is_set():
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return  # socket closed by stop()
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn: socket.socket) -> None:
        try:
            conn.settimeout(30.0)
            if self.tls_mode == "implicit":
                stream = self._ctx.wrap_socket(conn, server_side=True)
                self._greet_and_serve(stream)
            else:
                stream = self._starttls(conn)
                if stream is not None:
                    self._serve_commands(stream)
        except (OSError, ssl.SSLError):
            return
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def _starttls(self, conn: socket.socket) -> ssl.SSLSocket | None:
        """The cleartext phase: greet, answer exactly one STARTTLS, upgrade.

        Pre-TLS `CAPABILITY` is deliberately NOT advertised as a decision point:
        § The two TLS modes has the client issue STARTTLS unconditionally and
        read capabilities *inside* TLS, because trusting a pre-TLS CAPABILITY is
        the classic downgrade. The fake answers whatever it is asked.
        """
        conn.sendall(b"* OK IMAP4rev1 fake source ready\r\n")
        buf = bytearray()
        while b"\r\n" not in buf:
            chunk = conn.recv(4096)
            if not chunk:
                return None
            buf.extend(chunk)
        line = bytes(buf).split(b"\r\n", 1)[0].decode("utf-8", "replace")
        tag = line.split(" ", 1)[0]
        if "STARTTLS" not in line.upper():
            conn.sendall(f"{tag} BAD expected STARTTLS\r\n".encode())
            return None
        conn.sendall(f"{tag} OK begin TLS\r\n".encode())
        return self._ctx.wrap_socket(conn, server_side=True)

    def _greet_and_serve(self, stream: ssl.SSLSocket) -> None:
        stream.sendall(b"* OK IMAP4rev1 fake source ready\r\n")
        self._serve_commands(stream)

    def _serve_commands(self, stream: ssl.SSLSocket) -> None:
        selected: SourceMailbox | None = None
        buf = bytearray()

        while not self._stopped.is_set():
            try:
                chunk = stream.recv(8192)
            except (OSError, ssl.SSLError):
                return
            if not chunk:
                return
            buf.extend(chunk)

            # Drain EVERY complete line before reading again: `fetch_messages`
            # pipelines up to 4 commands, so several routinely land in one recv,
            # and answering only the first deadlocks the client — it will not
            # issue more until some outstanding tag completes.
            while b"\r\n" in buf:
                idx = buf.index(b"\r\n")
                line = bytes(buf[:idx]).decode("utf-8", "replace")
                del buf[: idx + 2]
                reply, selected, done = self._respond(line, selected)
                if reply:
                    try:
                        stream.sendall(reply)
                    except (OSError, ssl.SSLError):
                        return
                if done:
                    return

    def _respond(
        self, line: str, selected: SourceMailbox | None
    ) -> tuple[bytes, SourceMailbox | None, bool]:
        parts = line.split(" ", 2)
        tag = parts[0] if parts else "*"
        verb = parts[1].upper() if len(parts) > 1 else ""
        rest = parts[2] if len(parts) > 2 else ""

        if verb == "CAPABILITY":
            return (
                f"* CAPABILITY IMAP4rev1 UIDPLUS\r\n{tag} OK CAPABILITY done\r\n".encode(),
                selected,
                False,
            )

        if verb == "LOGIN":
            user, _, pwd = rest.partition(" ")
            user, pwd = _unquote(user), _unquote(pwd)
            self.logins.append((user, pwd))
            if user != self.username or pwd != self.password:
                # § Wizard steps 2: the wizard surfaces the source's own error
                # verbatim and lets the user retry without re-entering the host.
                return (
                    f"{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n".encode(),
                    selected,
                    False,
                )
            return (f"{tag} OK LOGIN done\r\n".encode(), selected, False)

        if verb == "LIST":
            # The attribute list is built outside the f-string: a backslash in an
            # f-string expression is a syntax error before Python 3.12, and the
            # e2e venv's version is a per-machine fact this file should not
            # depend on.
            noselect = "\\Noselect"
            out = "".join(
                '* LIST ({}) "/" "{}"\r\n'.format(
                    "" if m.selectable else noselect, m.name
                )
                for m in self.mailboxes.values()
            )
            return (f"{out}{tag} OK LIST done\r\n".encode(), selected, False)

        if verb == "EXAMINE":
            name = _unquote(rest.strip())
            mailbox = self.mailboxes.get(name)
            if mailbox is None:
                return (f"{tag} NO no such mailbox\r\n".encode(), selected, False)
            out = (
                f"* {len(mailbox.messages)} EXISTS\r\n"
                f"* OK [UIDVALIDITY {mailbox.uid_validity}] uids valid\r\n"
                f"{tag} OK [READ-ONLY] EXAMINE done\r\n"
            )
            return (out.encode(), mailbox, False)

        if verb == "UID":
            if selected is None:
                return (f"{tag} BAD no mailbox selected\r\n".encode(), selected, False)
            return (self._uid_fetch(tag, rest, selected), selected, False)

        if verb == "LOGOUT":
            return (f"* BYE bye\r\n{tag} OK LOGOUT done\r\n".encode(), selected, True)

        # Never silently drop a command (convention 11, applied to the fake): an
        # unanswered tag is a client hang, and a hang diagnoses nothing.
        return (f"{tag} BAD unknown command\r\n".encode(), selected, False)

    def _uid_fetch(self, tag: str, rest: str, mailbox: SourceMailbox) -> bytes:
        upper = rest.upper()

        # enumerate_uids: `UID FETCH <n>:* (UID RFC822.SIZE)`.
        if "RFC822.SIZE" in upper:
            m = re.search(r"FETCH\s+(\d+):\*", rest, re.IGNORECASE)
            from_uid = int(m.group(1)) if m else mailbox.first_uid
            lines = []
            for seq, uid in enumerate(mailbox.uids(), start=1):
                if uid < from_uid:
                    continue
                body = mailbox.body_for(uid) or b""
                lines.append(f"* {seq} FETCH (UID {uid} RFC822.SIZE {len(body)})\r\n")
            return ("".join(lines) + f"{tag} OK UID FETCH done\r\n").encode()

        # fetch_messages: `UID FETCH <uid> (UID FLAGS INTERNALDATE BODY.PEEK[])`.
        m = re.search(r"FETCH\s+(\d+)\s", rest, re.IGNORECASE)
        if m is None:
            return f"{tag} BAD malformed UID FETCH\r\n".encode()
        uid = int(m.group(1))
        body = mailbox.body_for(uid)
        if body is None:
            # A UID that vanished between enumerate and fetch is legal (the user
            # deleted it on the source mid-import); RFC 3501 says answer with no
            # untagged data, not an error.
            return f"{tag} OK UID FETCH done\r\n".encode()
        self.fetched_uids.append((mailbox.name, uid))
        seq = mailbox.uids().index(uid) + 1
        flags = " ".join(DEFAULT_FLAGS)
        head = (
            f"* {seq} FETCH (UID {uid} FLAGS ({flags}) "
            f'INTERNALDATE "01-Jan-2020 00:00:00 +0000" '
            f"BODY[] {{{len(body)}}}\r\n"
        ).encode()
        return head + body + f")\r\n{tag} OK UID FETCH done\r\n".encode()


def _unquote(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == '"' and value[-1] == '"':
        return value[1:-1].replace('\\"', '"').replace("\\\\", "\\")
    return value

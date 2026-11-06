"""The harness's per-authority SPKI pin for a WS-RPC dial (`common/auth.py`
§ SPKI pins per authority; `clients/_ws_rpc_core.py::_create_ws`).

A same-box app trusts the installed nest's self-signed floor by pinning its
leaf's SubjectPublicKeyInfo per authority (`nest/common.md` § Same-box reach),
never by turning verification off. The harness's own WS-RPC clients take the
same posture for an authority a caller pinned: the leaf is compared before
one application byte is written, so the bearer the authenticated upgrade
carries in its subprotocol header never reaches a listener the pin does not
name.

Each case stands up a throwaway TLS listener on loopback, serving a fresh
self-signed ECDSA P-256 leaf (the floor's key type), and records the bytes it
receives after the handshake. That record is the observable: a refused pin
leaves it empty, an accepted one shows the plaintext upgrade request arriving
inside the TLS session — which also proves the pre-checked socket is handed
to ``websocket-client`` as-is rather than wrapped a second time.
"""

from __future__ import annotations

import datetime
import socket
import ssl
import threading

import pytest

pytestmark = pytest.mark.tier_1


def _self_signed(tmp_path, name: str):
    """A fresh self-signed P-256 cert + key on disk; returns (cert_path,
    key_path, cert)."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, name)])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .sign(key, hashes.SHA256())
    )
    cert_path = tmp_path / f"{name}.pem"
    key_path = tmp_path / f"{name}.key"
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    key_path.write_bytes(key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ))
    return cert_path, key_path, cert


class _RecordingTlsListener:
    """One-connection TLS listener on 127.0.0.1 that records what it receives
    after the handshake, then closes (so a client waiting on the upgrade reply
    sees the connection end)."""

    def __init__(self, cert_path, key_path):
        self._ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self._ctx.load_cert_chain(str(cert_path), str(key_path))
        self._sock = socket.create_server(("127.0.0.1", 0))
        self._sock.settimeout(10.0)
        self.port = self._sock.getsockname()[1]
        self.received = b""
        self.handshook = False
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        try:
            conn, _ = self._sock.accept()
        except OSError:
            return
        try:
            with self._ctx.wrap_socket(conn, server_side=True) as tls:
                self.handshook = True
                tls.settimeout(5.0)
                chunks = []
                while True:
                    try:
                        data = tls.recv(4096)
                    except (OSError, ssl.SSLError):
                        break
                    if not data:
                        break
                    chunks.append(data)
                    if b"\r\n\r\n" in b"".join(chunks):
                        break
                self.received = b"".join(chunks)
        except (OSError, ssl.SSLError):
            pass
        finally:
            self._sock.close()

    def join(self) -> None:
        self._thread.join(timeout=15.0)


@pytest.fixture
def listener(tmp_path):
    cert_path, key_path, cert = _self_signed(tmp_path, "floor")
    srv = _RecordingTlsListener(cert_path, key_path)
    yield srv, cert
    srv.join()


@pytest.fixture
def pins():
    """Every pin a case records is dropped after it, so a later case (or a
    later nest on a reused port) never inherits one."""
    from common import auth

    recorded: list[str] = []

    def pin(authority: str, spki: str) -> None:
        recorded.append(authority)
        auth.pin_nest_spki(authority, spki)

    yield pin
    for authority in recorded:
        auth.unpin_nest_spki(authority)


def test_an_unpinned_authority_is_not_dialled_by_the_pin_path():
    from common.auth import open_pinned_tls

    assert open_pinned_tls("wss://127.0.0.1:9/api/v1/ws", timeout=1.0) is None
    # A plain scheme is never the pin path, pinned or not.
    assert open_pinned_tls("ws://127.0.0.1:9/api/v1/ws", timeout=1.0) is None


def test_a_matching_pin_carries_the_upgrade_inside_the_tls_session(listener, pins):
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    from helpers.tls_spki import spki_sha256_hex

    srv, cert = listener
    pins(f"127.0.0.1:{srv.port}", spki_sha256_hex(cert))

    client = WsRpcAnonClient(f"https://127.0.0.1:{srv.port}", http_timeout=5.0)
    with pytest.raises(Exception) as excinfo:
        with client:
            pass
    srv.join()

    from common.auth import SpkiPinMismatch

    assert not isinstance(excinfo.value, SpkiPinMismatch), (
        f"a matching pin was refused: {excinfo.value!r}"
    )
    assert srv.handshook, "the client never completed a TLS handshake"
    assert srv.received.startswith(b"GET /api/v1/ws HTTP/1.1\r\n"), (
        "the pinned socket did not carry the plaintext upgrade inside the TLS "
        f"session (received {srv.received[:80]!r}) — a second TLS wrap would "
        "show a ClientHello here instead"
    )


def test_a_disagreeing_pin_is_refused_before_one_application_byte(listener, pins):
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    from common.auth import SpkiPinMismatch

    srv, _cert = listener
    pins(f"127.0.0.1:{srv.port}", "00" * 32)

    client = WsRpcAnonClient(f"https://127.0.0.1:{srv.port}", http_timeout=5.0)
    with pytest.raises(SpkiPinMismatch) as excinfo:
        with client:
            pass
    srv.join()

    assert srv.received == b"", (
        f"bytes reached a listener whose leaf disagreed with the pin: {srv.received[:80]!r}"
    )
    assert f"127.0.0.1:{srv.port}" in str(excinfo.value), (
        f"the refusal does not name the authority: {excinfo.value}"
    )


def test_a_pin_is_per_authority(listener, pins):
    """A pin for another port of the same host does not reach this one: the
    authority is the key, as it is for the product's pin store."""
    from common.auth import open_pinned_tls, pinned_spki

    srv, _cert = listener
    pins(f"127.0.0.1:{srv.port + 1}", "11" * 32)

    assert pinned_spki(f"127.0.0.1:{srv.port}") is None
    assert open_pinned_tls(f"wss://127.0.0.1:{srv.port}/api/v1/ws", timeout=1.0) is None
    # Nothing dialled it: let the listener's accept time out cleanly.
    srv.join()

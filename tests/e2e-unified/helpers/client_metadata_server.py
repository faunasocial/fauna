"""An ``https`` client-metadata document on a real hostname, for a tier_3
third-party journey that needs a **kind manifest**.

A manifest verifies only against the host its document is served from
(``third-party-kinds.md`` § The manifest), so the loopback development client —
whose document is synthesized, never fetched — cannot carry one. This serves
``https://<host>/client-metadata.json`` on a loopback port under a throwaway
CA, and hands the nest the two ``test-hooks`` seams that let its guarded
fetcher reach it: ``FAUNA_TEST_CLIENT_METADATA_RESOLVE_JSON`` (``{host:
"127.0.0.1:<port>"}``) and ``FAUNA_TEST_CLIENT_METADATA_EXTRA_CA_PEM`` (the
CA's path). Both are loopback-only and mapped-host-only on the nest side
(``ssrf::test_resolve_client``); an unmapped host still meets the full guard.

The manifest is signed here, independently of Rust's ``sign_manifest``: a
compact JWS whose header is exactly ``{"alg": "EdDSA", "kid": <did:key>,
"typ": "fauna-manifest+json"}`` over the UTF-8 JSON of the extension object —
an independent signer is what makes the nest's verify a real check rather
than Rust agreeing with itself.
"""

from __future__ import annotations

import base64
import datetime
import http.server
import json
import ssl
import threading
import time
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519
from cryptography.x509.oid import NameOID

from drivers.port_util import find_free_port
from helpers.atproto_fakes import _B58_ALPHABET

DOCUMENT_PATH = "/client-metadata.json"


def _b64u(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def _b58_encode(raw: bytes) -> str:
    n = int.from_bytes(raw, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = _B58_ALPHABET[r] + out
    pad = len(raw) - len(raw.lstrip(b"\x00"))
    return "1" * pad + out


def ed25519_did_key(public: bytes) -> str:
    """``did:key:z6Mk…`` — multicodec ``0xed`` (prefix ``ed 01``), base58btc."""
    return "did:key:z" + _b58_encode(b"\xed\x01" + public)


def sign_manifest(key: ed25519.Ed25519PrivateKey, payload: dict) -> str:
    """The compact JWS a document's ``fauna`` member carries."""
    public = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    header = {"alg": "EdDSA", "kid": ed25519_did_key(public), "typ": "fauna-manifest+json"}
    signing_input = (
        f"{_b64u(json.dumps(header, separators=(',', ':')).encode())}."
        f"{_b64u(json.dumps(payload, separators=(',', ':')).encode())}"
    )
    return f"{signing_input}.{_b64u(key.sign(signing_input.encode()))}"


def manifest_payload(host: str, key: ed25519.Ed25519PrivateKey, kind_names: list[str]) -> dict:
    """The extension object: host's publisher key and one ``latest-wins``
    ``state`` kind per name (the one admitted vocabulary)."""
    public = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return {
        "version": 1,
        "publisher": {"domain": host, "key": ed25519_did_key(public)},
        "kinds": [
            {"kind": f"ext.{host}.{name}", "class": "state", "merge": "latest-wins", "floor": "none"}
            for name in kind_names
        ],
    }


def _ca_and_leaf(hosts: list[str], tmp: Path) -> tuple[Path, Path, Path]:
    """A throwaway CA and one leaf for every name in ``hosts`` it signs →
    (ca_pem, cert_pem, key_pem)."""
    now = datetime.datetime.now(datetime.timezone.utc)
    ca_key = ec.generate_private_key(ec.SECP256R1())
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "fauna e2e client-metadata CA")])
    ca = (
        x509.CertificateBuilder()
        .subject_name(ca_name)
        .issuer_name(ca_name)
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True, key_cert_sign=True, crl_sign=True,
                content_commitment=False, key_encipherment=False, data_encipherment=False,
                key_agreement=False, encipher_only=False, decipher_only=False,
            ),
            critical=True,
        )
        .sign(ca_key, hashes.SHA256())
    )
    leaf_key = ec.generate_private_key(ec.SECP256R1())
    leaf = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, hosts[0])]))
        .issuer_name(ca_name)
        .public_key(leaf_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(
            x509.SubjectAlternativeName([x509.DNSName(h) for h in hosts]), critical=False
        )
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .add_extension(
            x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]), critical=False
        )
        .sign(ca_key, hashes.SHA256())
    )
    ca_pem, cert_pem, key_pem = tmp / "ca.pem", tmp / "leaf.pem", tmp / "leaf.key"
    ca_pem.write_bytes(ca.public_bytes(serialization.Encoding.PEM))
    cert_pem.write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
    key_pem.write_bytes(
        leaf_key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    return ca_pem, cert_pem, key_pem


class ClientMetadataServer:
    """Serves client-metadata documents for ``host`` over HTTPS on loopback,
    one per path — each path its own ``client_id``, so two clients of one
    publisher (one public, one confidential) share the host a manifest names.

    The nest caches a resolved client for 15 minutes, so a document is set
    once, before its first ceremony, and never changed under a client.

    ``extra_hosts`` serves more publishers from the same loopback port under
    the same CA — a journey needing two admitted publishers on one nest. Paths
    are shared across hosts, so give each host's document its own path.

    It is also the publisher's **webhook receiver**: every ``POST`` to any
    path is accepted (``202``, RFC 8935) and recorded in ``posts`` as
    ``(path, lower-cased headers, body)``; ``wait_posts`` blocks for them."""

    def __init__(self, host: str, tmp: Path, extra_hosts: tuple[str, ...] = ()):
        self.host = host
        self.hosts = [host, *extra_hosts]
        self.documents: dict[str, dict] = {}
        self.posts: list[tuple[str, dict[str, str], bytes]] = []
        self._posted = threading.Condition()
        self.port = find_free_port()
        self.ca_pem, cert_pem, key_pem = _ca_and_leaf(self.hosts, tmp)

        server = self

        class _Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 — the stdlib's name
                document = server.documents.get(self.path)
                if document is None:
                    self.send_error(404)
                    return
                body = json.dumps(document).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):  # noqa: N802 — the stdlib's name
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length)
                headers = {k.lower(): v for k, v in self.headers.items()}
                with server._posted:
                    server.posts.append((self.path, headers, body))
                    server._posted.notify_all()
                self.send_response(202)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *_args):
                pass

        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert_pem, key_pem)
        self._httpd = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), _Handler)
        self._httpd.socket = ctx.wrap_socket(self._httpd.socket, server_side=True)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        self._thread.start()

    def client_id(self, path: str = DOCUMENT_PATH, host: str | None = None) -> str:
        """The ``client_id`` a document at ``path`` answers to, on the emulator host
        (default: the first)."""
        return f"https://{host or self.host}{path}"

    def wait_posts(
        self, path: str, count: int, timeout: float
    ) -> list[tuple[str, dict[str, str], bytes]]:
        """The POSTs to ``path`` so far, once at least ``count`` have arrived
        or ``timeout`` seconds have passed — the caller asserts on what came,
        never on how long it took."""
        deadline = time.monotonic() + timeout
        with self._posted:
            while True:
                hits = [p for p in self.posts if p[0] == path]
                remaining = deadline - time.monotonic()
                if len(hits) >= count or remaining <= 0:
                    return hits
                self._posted.wait(remaining)

    def nest_env(self) -> dict[str, str]:
        """The two ``test-hooks`` seams the nest's metadata fetcher reads."""
        return {
            "FAUNA_TEST_CLIENT_METADATA_RESOLVE_JSON": json.dumps(
                {h: f"127.0.0.1:{self.port}" for h in self.hosts}
            ),
            "FAUNA_TEST_CLIENT_METADATA_EXTRA_CA_PEM": str(self.ca_pem),
        }

    def close(self) -> None:
        self._httpd.shutdown()
        self._httpd.server_close()


def client_document(
    client_id: str,
    redirect_uri: str,
    scope: str,
    manifest_jws: str | None = None,
    jwk: dict | None = None,
) -> dict:
    """A DPoP-bound client's document. Public (``none``) by default — a
    device-form principal once consented; with ``jwk`` it is confidential
    (``private_key_jwt`` over that one key) — a remote-form principal.
    ``manifest_jws`` rides as the ``fauna`` member."""
    doc = {
        "client_id": client_id,
        "client_name": "Example Notes",
        "application_type": "native" if jwk is None else "web",
        "redirect_uris": [redirect_uri],
        "scope": scope,
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_method": "none" if jwk is None else "private_key_jwt",
        "dpop_bound_access_tokens": True,
    }
    if jwk is not None:
        doc["jwks"] = {"keys": [jwk]}
    if manifest_jws is not None:
        doc["fauna"] = manifest_jws
    return doc

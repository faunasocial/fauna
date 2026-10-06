"""SHA-256 of a certificate's ``SubjectPublicKeyInfo`` — the harness's one home
for the TLS channel-binding fingerprint, and the Python mirror of
``libs/fauna-protocol/src/tls_spki.rs``.

The fingerprint is the value both ends of every Fauna TLS binding compute and
compare: the nest signs the SPKI of the cert *it* serves
(``bins/fauna-nest/src/acme.rs::ServedCertSpki``), and the peer recomputes it
from the leaf *it* received during the handshake. Because the two are
independent implementations of one number, a green comparison anywhere in this
suite is a cross-implementation pin — which is exactly why the computation must
not be re-spelled per test file.

It had been re-spelled three times before this module existed
(``test_dane_tlsa_cert_coupling.py``, ``test_acme_http01_pebble_issuance.py``,
and — as a hard-coded empty string standing in for the loopback posture —
``clients/ws_rpc_federation_client.py``); a fourth was about to be written for
the federation channel over TLS. Priority #4: one shape, lifted, not replicated
on a fourth surface.

**Why re-encoding the public key agrees with the Rust side.** Rust hashes
``tbs_certificate.subject_pki.raw`` — the SPKI bytes exactly as they appear in
the certificate — while ``cryptography`` re-encodes the parsed key as DER. For
every key type Fauna serves (the self-signed floor's ECDSA P-256, ACME-issued
ECDSA/RSA) the SPKI is already canonical DER, so the two are byte-identical.
That is not an assumption resting on this comment: the DANE-TLSA coupling test
asserts a published rdata (computed by the nest from the raw slice) equals a
value computed here from a re-encode, and
``tests/api/test_federation_channel_tls_binding.py`` makes a real handshake
succeed or fail on the same equality.

Verify-off throughout: a Fauna listener serves a self-signed floor by default
and an ACME leaf chains to a throwaway CA in test venues, so a chain-validating
read would fail on postures that are working correctly. We want the leaf bytes
off the handshake, never a trust decision — the trust decision is the product's
(``security.md`` § Transport trust).
"""

from __future__ import annotations

import hashlib
import socket
import ssl
from typing import Optional

from cryptography import x509
from cryptography.hazmat.primitives import serialization


def spki_sha256_hex(cert: x509.Certificate) -> str:
    """``sha256(SubjectPublicKeyInfo)`` of ``cert``, lowercase hex."""
    spki = cert.public_key().public_bytes(
        serialization.Encoding.DER,
        serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    return hashlib.sha256(spki).hexdigest()


def spki_sha256_hex_of_der(cert_der: bytes) -> str:
    """:func:`spki_sha256_hex` of a DER-encoded certificate."""
    return spki_sha256_hex(x509.load_der_x509_certificate(cert_der))


def served_leaf(
    host: str, port: int, sni: str, *, timeout: float = 15
) -> x509.Certificate:
    """The leaf a TLS listener serves for ``sni``, off a throwaway handshake.

    Mirrors the nest's own ``cert_for_sni`` selection: pass the SNI the caller
    cares about (an IP literal is fine — Python omits SNI for one, which is
    what a real IP-dialled peer does too).
    """
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with socket.create_connection((host, port), timeout=timeout) as sock:
        with ctx.wrap_socket(sock, server_hostname=sni) as tls:
            der = tls.getpeercert(binary_form=True)
    if der is None:
        raise AssertionError(
            f"{host}:{port} completed a TLS handshake for SNI {sni!r} but "
            "presented no certificate"
        )
    return x509.load_der_x509_certificate(der)


def served_leaf_spki_sha256_hex(
    host: str, port: int, sni: str, *, timeout: float = 15
) -> str:
    """``sha256(SubjectPublicKeyInfo)`` hex of the leaf served for ``sni``."""
    return spki_sha256_hex(served_leaf(host, port, sni, timeout=timeout))


def observed_spki_sha256_hex(sock: Optional[socket.socket]) -> str:
    """The channel binding for a connection **already open**, or ``""``.

    The distinction from :func:`served_leaf_spki_sha256_hex` is the whole point
    and not a convenience: a channel binding read from a *second* connection
    binds nothing — a MITM is free to answer two connections with two certs.
    The product reads it off the very socket it then speaks over
    (``federation_channel.rs::connect_federation_ws``, whose capturing rustls
    verifier records the leaf during that connection's own handshake), and a
    harness peer that wants to be a witness for that mechanism must do the same.

    ``""`` for a plain (non-TLS) socket — the value a loopback ``ws://`` nest's
    listener computes for itself (``serve_listener``'s
    ``current_spki_sha256()…unwrap_or_default()``), so the empty string is a
    real posture rather than a fallback.
    """
    if not isinstance(sock, ssl.SSLSocket):
        return ""
    der = sock.getpeercert(binary_form=True)
    if der is None:
        return ""
    return spki_sha256_hex_of_der(der)

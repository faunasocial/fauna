"""Per-run ephemeral CA + leaf certs for the real-Mastodon interop harness.

The harness terminates TLS on both sides of the fediverse boundary with an
in-memory CA minted fresh per run:

  * our nest dials `https://mastodon.test` and must trust the CA — passed to the
    test-hooks binary via `FAUNA_TEST_AP_EXTRA_CA_PEM` (the CA cert PEM), because
    reqwest's `rustls-tls` bakes the webpki roots and ignores `SSL_CERT_FILE`;
  * Mastodon dials `https://nest.test` and must trust the same CA — the CA cert
    is mounted into the Mastodon container's trust store.

Nothing here touches `/etc/hosts` or any machine-global trust store: the CA lives
only in the temp dir handed back, so a launch stays isolated from the box
(e2e conventions point 10). Uses `cryptography`, already an e2e dependency.
"""

from __future__ import annotations

import datetime
import ipaddress
import os
from dataclasses import dataclass

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID

# Leaf certs are dated from a fixed point in the past so a slow clock or a
# just-booted container never sees a not-yet-valid cert; the long window keeps
# the harness working without a per-run clock read.
_NOT_BEFORE = datetime.datetime(2020, 1, 1, tzinfo=datetime.timezone.utc)
_NOT_AFTER = datetime.datetime(2100, 1, 1, tzinfo=datetime.timezone.utc)


@dataclass
class LeafCert:
    """A hostname's cert + private key, PEM-encoded."""

    hostname: str
    cert_pem: bytes
    key_pem: bytes

    def write(self, dir_path: str, basename: str | None = None) -> tuple[str, str]:
        """Write `<basename>.crt` + `<basename>.key`; return their paths."""
        base = basename or self.hostname
        cert_path = os.path.join(dir_path, f"{base}.crt")
        key_path = os.path.join(dir_path, f"{base}.key")
        with open(cert_path, "wb") as f:
            f.write(self.cert_pem)
        with open(key_path, "wb") as f:
            f.write(self.key_pem)
        return cert_path, key_path


class EphemeralCA:
    """A throwaway CA that signs leaf certs for the harness's two hostnames.

    RSA-2048 throughout: it is the one key type every TLS terminator in the
    stack (nginx, rustls, Rails' HTTP client) accepts without ceremony, and cert
    generation is one-off per run.
    """

    def __init__(self) -> None:
        self._key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "fauna-interop-test-ca")])
        self._cert = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(self._key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(_NOT_BEFORE)
            .not_valid_after(_NOT_AFTER)
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
            .add_extension(
                x509.KeyUsage(
                    digital_signature=True,
                    key_cert_sign=True,
                    crl_sign=True,
                    content_commitment=False,
                    key_encipherment=False,
                    data_encipherment=False,
                    key_agreement=False,
                    encipher_only=False,
                    decipher_only=False,
                ),
                critical=True,
            )
            .sign(self._key, hashes.SHA256())
        )

    @property
    def cert_pem(self) -> bytes:
        """The CA certificate, PEM-encoded — the trust anchor both sides need."""
        return self._cert.public_bytes(serialization.Encoding.PEM)

    def write_cert(self, dir_path: str, basename: str = "ca") -> str:
        """Write the CA cert to `<basename>.crt`; return its path."""
        path = os.path.join(dir_path, f"{basename}.crt")
        with open(path, "wb") as f:
            f.write(self.cert_pem)
        return path

    def issue(self, hostname: str) -> LeafCert:
        """Mint a leaf cert for `hostname`, signed by this CA.

        The SAN carries `hostname` (a DNS name, or an IP if it parses as one) so
        strict TLS verifiers (rustls, modern OpenSSL) accept it — a bare CN is no
        longer honored.
        """
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        try:
            san: x509.GeneralName = x509.IPAddress(ipaddress.ip_address(hostname))
        except ValueError:
            san = x509.DNSName(hostname)
        cert = (
            x509.CertificateBuilder()
            .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, hostname)]))
            .issuer_name(self._cert.subject)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(_NOT_BEFORE)
            .not_valid_after(_NOT_AFTER)
            .add_extension(x509.SubjectAlternativeName([san]), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .sign(self._key, hashes.SHA256())
        )
        return LeafCert(
            hostname=hostname,
            cert_pem=cert.public_bytes(serialization.Encoding.PEM),
            key_pem=key.private_bytes(
                encoding=serialization.Encoding.PEM,
                format=serialization.PrivateFormat.TraditionalOpenSSL,
                encryption_algorithm=serialization.NoEncryption(),
            ),
        )
